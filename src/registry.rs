use crate::config::{Config, RegistryConfig, DEFAULT_AUTH_USERNAME};
use crate::version::VersionInfo;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use reqwest::{header::HeaderMap, Client as HttpClient, Response, StatusCode};
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, warn};

// Registries implementing the distribution spec (Docker Hub, ghcr.io, lscr.io)
// cap `n` at 1000; a larger value is clamped, so 1000 minimises the number of
// requests (and thus 429s).
const OCI_PAGE_SIZE: usize = 1000;
// Upper bound on tags fetched per image, as a runaway guard only. Tag listings are
// NOT ordered by version (Docker Hub's registry API is lexical, ghcr.io/lscr.io are
// chronological), so we must scan the whole list to reliably find the highest
// version rather than truncate and risk missing it. Real repos (e.g.
// jellyfin/jellyfin at ~13.6k tags) finish far below this; hitting it means the
// result may be incomplete and is logged loudly.
const MAX_TAGS_SCANNED: usize = 50_000;
const MAX_RETRY_ATTEMPTS: u32 = 5;
const MAX_AUTH_ATTEMPTS: u32 = 2;
const INITIAL_RETRY_DELAY_SECS: u64 = 1;
const HTTP_TIMEOUT_SECS: u64 = 30;
const HTTP_CONNECT_TIMEOUT_SECS: u64 = 10;
// Evict idle keep-alive connections well before registries (Docker Hub, GHCR,
// lscr.io) close them server-side. Reusing a connection the server has already
// dropped is what surfaces as "connection closed before message completed";
// recycling early makes that race rare, and `fetch_with_retry` covers the rest.
const POOL_IDLE_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone)]
pub struct ImageRef {
    pub registry: String,
    pub namespace: Option<String>,
    pub name: String,
    pub tag: String,
}

impl ImageRef {
    pub fn parse(image: &str) -> Result<Self> {
        let (image_part, tag) = if let Some(last_delim) = image.rfind(':') {
            (&image[..last_delim], &image[last_delim + 1..])
        } else {
            (image, "latest")
        };

        let registry_parts: Vec<&str> = image_part.split('/').collect();

        let (registry, namespace, name) = match registry_parts.len() {
            0 => return Err(anyhow!("Invalid image format: {}", image)),
            1 => ("docker.io".to_string(), None, registry_parts[0].to_string()),
            2 => {
                if registry_parts[0].contains('.') || registry_parts[0].contains(':') {
                    (
                        registry_parts[0].to_string(),
                        None,
                        registry_parts[1].to_string(),
                    )
                } else {
                    (
                        "docker.io".to_string(),
                        Some(registry_parts[0].to_string()),
                        registry_parts[1].to_string(),
                    )
                }
            }
            _ => {
                let registry = registry_parts[0].to_string();
                let name = registry_parts[registry_parts.len() - 1].to_string();
                let namespace = Some(registry_parts[1..registry_parts.len() - 1].join("/"));
                (registry, namespace, name)
            }
        };

        Ok(ImageRef {
            registry,
            namespace,
            name,
            tag: tag.to_string(),
        })
    }
}

impl std::fmt::Display for ImageRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.registry == "docker.io" {
            match &self.namespace {
                Some(ns) => write!(f, "{}/{}:{}", ns, self.name, self.tag),
                None => write!(f, "{}:{}", self.name, self.tag),
            }
        } else {
            match &self.namespace {
                Some(ns) => write!(f, "{}/{}/{}:{}", self.registry, ns, self.name, self.tag),
                None => write!(f, "{}/{}:{}", self.registry, self.name, self.tag),
            }
        }
    }
}

/// A fully-read HTTP response. The body is read inside the retry scope so that a
/// connection dropped mid-body is retried like any other transport failure;
/// callers therefore receive the body as an owned string rather than a streaming
/// `Response`, while retaining the status and headers they need.
struct FetchResult {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

pub struct Client {
    http_client: HttpClient,
    config: Config,
    /// Registries whose configured credentials have been rejected, which are from
    /// then on asked for anonymous tokens only. Re-offering credentials already
    /// known to be bad costs a full retry budget per image and, on Docker Hub,
    /// trips the failed-login throttle — whose 429s then delay the anonymous
    /// request that would have worked. Config is only read at startup, so a
    /// corrected token needs a restart anyway and this may live as long as the
    /// client.
    credentials_rejected: Mutex<HashSet<String>>,
}

impl Client {
    pub fn new(config: Config) -> Self {
        let http_client = HttpClient::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(HTTP_CONNECT_TIMEOUT_SECS))
            .pool_idle_timeout(Duration::from_secs(POOL_IDLE_TIMEOUT_SECS))
            .build()
            .expect("failed to build HTTP client");

        Self {
            http_client,
            config,
            credentials_rejected: Mutex::new(HashSet::new()),
        }
    }

    fn parse_retry_after(&self, retry_after: &str) -> Option<Duration> {
        if let Ok(seconds) = retry_after.parse::<u64>() {
            return Some(Duration::from_secs(seconds));
        }

        if let Ok(date) = DateTime::parse_from_rfc2822(retry_after) {
            let now = Utc::now();
            let wait_time = date.signed_duration_since(now);
            if wait_time.num_seconds() > 0 {
                return Some(Duration::from_secs(wait_time.num_seconds() as u64));
            }
        }

        None
    }

    async fn fetch_with_retry<F, Fut>(
        &self,
        request_fn: F,
        operation_name: &str,
    ) -> Result<FetchResult>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<Response, reqwest::Error>>,
    {
        let mut attempt = 0;
        let mut delay = Duration::from_secs(INITIAL_RETRY_DELAY_SECS);

        loop {
            // Send the request AND read the body as one retryable unit: a
            // connection dropped while streaming the body (`is_body`) is just as
            // transient as one dropped while sending the request (`is_request`),
            // and reading it here lets both be retried.
            let fetched = async {
                let response = request_fn().await?;
                let status = response.status();
                let headers = response.headers().clone();
                let body = response.text().await?;
                Ok::<_, reqwest::Error>(FetchResult {
                    status,
                    headers,
                    body,
                })
            }
            .await;

            let fetched = match fetched {
                Ok(fetched) => fetched,
                Err(err) => {
                    attempt += 1;

                    // Transient transport failures (connect timeouts, dropped
                    // keep-alive connections) never reached this retry path
                    // before and aborted the whole update run. Retry them with
                    // the same bounded, exponential backoff used for 429s.
                    if !is_transient_transport_error(&err) || attempt > MAX_RETRY_ATTEMPTS {
                        return Err(
                            anyhow::Error::new(err).context(format!("{operation_name} failed"))
                        );
                    }

                    warn!(
                        "{} transient transport error ({}), attempt {}/{}, waiting {:?} before retry",
                        operation_name, err, attempt, MAX_RETRY_ATTEMPTS, delay
                    );

                    sleep(delay).await;
                    delay = delay.saturating_mul(2);
                    continue;
                }
            };

            if fetched.status == StatusCode::TOO_MANY_REQUESTS {
                attempt += 1;

                if attempt > MAX_RETRY_ATTEMPTS {
                    return Err(anyhow!(
                        "{} failed: Rate limited (429) after {} attempts",
                        operation_name,
                        MAX_RETRY_ATTEMPTS
                    ));
                }

                let wait_duration = if let Some(retry_after) = fetched
                    .headers
                    .get("retry-after")
                    .and_then(|h| h.to_str().ok())
                {
                    self.parse_retry_after(retry_after).unwrap_or(delay)
                } else {
                    delay
                };

                warn!(
                    "{} rate limited (429), attempt {}/{}, waiting {:?} before retry",
                    operation_name, attempt, MAX_RETRY_ATTEMPTS, wait_duration
                );

                sleep(wait_duration).await;

                delay = delay.saturating_mul(2);
            } else {
                return Ok(fetched);
            }
        }
    }

    fn get_registry_config(&self, registry: &str) -> Result<&RegistryConfig> {
        self.config
            .registries
            .get(registry)
            .ok_or_else(|| anyhow!("Unknown registry: {}", registry))
    }

    fn build_repository_path(&self, image_ref: &ImageRef) -> String {
        match &image_ref.namespace {
            Some(namespace) => format!("{}/{}", namespace, image_ref.name),
            None if image_ref.registry == "docker.io" => {
                format!("library/{}", image_ref.name)
            }
            None => image_ref.name.clone(),
        }
    }

    /// Returns the semver-parseable versions on the page, the raw last tag (the
    /// `last=` cursor for the next page, regardless of whether it parsed as
    /// semver), and the raw number of tags on the page.
    fn parse_v2_response(
        &self,
        response_text: &str,
    ) -> Result<(Vec<VersionInfo>, Option<String>, usize)> {
        debug!("Parsing registry v2 response: {}", response_text);
        let tags_response: serde_json::Value = serde_json::from_str(response_text)
            .map_err(|e| anyhow!("Failed to parse registry response: {}", e))?;

        let mut versions = Vec::new();
        let mut raw_last_tag = None;
        let mut raw_count = 0;

        if let Some(tags) = tags_response.get("tags").and_then(|t| t.as_array()) {
            for tag in tags {
                if let Some(tag_str) = tag.as_str() {
                    raw_count += 1;
                    raw_last_tag = Some(tag_str.to_string());
                    if let Some(version_info) = VersionInfo::from_tag(tag_str) {
                        versions.push(version_info);
                    }
                }
            }
        }

        Ok((versions, raw_last_tag, raw_count))
    }

    /// Lists every tag of an image via the registry v2 API, including Docker Hub:
    /// its hub.docker.com JSON API refuses anonymous requests past a 10,000-tag
    /// offset ("pagination offset too large for anonymous requests"), which large
    /// repos such as jellyfin/jellyfin (~13.6k tags) exceed. The v2 endpoint
    /// paginates by cursor instead of offset, so it has no such ceiling, and its
    /// 1000-tag pages need ~10x fewer requests.
    pub async fn get_available_versions(&self, image_ref: &ImageRef) -> Result<Vec<VersionInfo>> {
        self.get_registry_v2_versions(image_ref).await
    }

    async fn get_registry_v2_versions(&self, image_ref: &ImageRef) -> Result<Vec<VersionInfo>> {
        let registry_config = self.get_registry_config(&image_ref.registry)?;
        let repo_path = self.build_repository_path(image_ref);

        let mut results = Vec::new();
        let mut last_tag: Option<String> = None;
        let mut next_url: Option<String> = None;
        let mut bearer_token: Option<String> = None;
        let mut auth_attempts: u32 = 0;
        let mut tags_scanned: usize = 0;

        loop {
            let url = if let Some(next) = next_url.take() {
                if next.starts_with("http://") || next.starts_with("https://") {
                    next
                } else {
                    format!("{}{next}", registry_config.url)
                }
            } else {
                format!(
                    "{}/v2/{repo_path}/tags/list?n={OCI_PAGE_SIZE}{}",
                    registry_config.url,
                    if let Some(ref last) = last_tag {
                        format!("&last={last}")
                    } else {
                        String::new()
                    }
                )
            };

            debug!("Registry API URL: {}", url);

            let url_clone = url.clone();
            let bearer_token_clone = bearer_token.clone();
            let fetched = self
                .fetch_with_retry(
                    || async {
                        let mut request_builder = self.http_client.get(&url_clone);
                        if let Some(token) = &bearer_token_clone {
                            request_builder = request_builder.bearer_auth(token);
                        }
                        request_builder.send().await
                    },
                    "Registry v2 API request",
                )
                .await?;

            let (new_tags, raw_last_tag, raw_count, link_next) = if fetched.status
                == reqwest::StatusCode::UNAUTHORIZED
            {
                if auth_attempts >= MAX_AUTH_ATTEMPTS {
                    return Err(anyhow!(
                        "Authentication failed after {} attempts for registry {}",
                        auth_attempts,
                        image_ref.registry
                    ));
                }
                let Some(auth_header) = fetched.headers.get("www-authenticate") else {
                    return Err(anyhow!(
                        "Unauthorized request but no WWW-Authenticate header found"
                    ));
                };
                let auth_str = auth_header
                    .to_str()
                    .map_err(|e| anyhow!("Invalid WWW-Authenticate header encoding: {}", e))?;
                // Answer the challenge even with no token configured: public
                // repositories on Docker Hub and ghcr.io hand out a pull-scoped
                // token to unauthenticated callers, and the tag listing is
                // inaccessible without one.
                bearer_token = Some(
                    self.fetch_registry_v2_token(auth_str, &image_ref.registry, registry_config)
                        .await?,
                );
                auth_attempts += 1;
                continue;
            } else if fetched.status.is_success() {
                let link_next = fetched
                    .headers
                    .get("link")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|h| self.parse_link_header(h));

                // A page came back, so the current token works: give the next
                // 401 a fresh re-auth budget. Tokens expire (Docker Hub's last
                // 5 minutes) and a long scan can outlive one, which must not
                // exhaust the attempt cap and abandon the listing half-read.
                auth_attempts = 0;

                let (tags, raw_last_tag, raw_count) = self.parse_v2_response(&fetched.body)?;
                (tags, raw_last_tag, raw_count, link_next)
            } else {
                return Err(anyhow!(
                    "Registry request failed with status {}: {}",
                    fetched.status,
                    fetched.body
                ));
            };

            tags_scanned += raw_count;
            results.extend(new_tags);

            if tags_scanned >= MAX_TAGS_SCANNED {
                warn!(
                    "Reached the {}-tag scan limit for {} before exhausting the registry; \
                     the newest version may have been missed",
                    MAX_TAGS_SCANNED, image_ref
                );
                break;
            }

            // Advance via the registry's Link header when present, else the raw
            // last-tag cursor. Stop on an empty page, a Link header that points
            // back at the current page, or a cursor that did not move.
            if let Some(next) = link_next {
                let resolved = if next.starts_with("http://") || next.starts_with("https://") {
                    next
                } else {
                    format!("{}{next}", registry_config.url)
                };
                if resolved == url {
                    break;
                }
                next_url = Some(resolved);
            } else if let Some(last) = raw_last_tag {
                if raw_count == 0 || last_tag.as_ref() == Some(&last) {
                    break;
                }
                last_tag = Some(last);
            } else {
                break;
            }
        }

        Ok(results)
    }

    /// Exchanges a `WWW-Authenticate` challenge for a bearer token, using the
    /// registry's configured credentials when it has any.
    ///
    /// A rejected credential falls back to an anonymous token rather than failing
    /// the image: public repositories hand out pull-scoped tokens to anyone, which
    /// is all a tag listing needs. Docker Hub is why this matters — it validates
    /// the basic-auth username, so a `dckr_pat_…` configured without a matching
    /// `username` is refused outright, and without the fallback every Docker Hub
    /// image in the run would fail.
    async fn fetch_registry_v2_token(
        &self,
        auth_str: &str,
        registry: &str,
        registry_config: &RegistryConfig,
    ) -> Result<String> {
        let realm = extract_auth_param(auth_str, "realm")?;
        let service = extract_auth_param(auth_str, "service")?;
        let scope = extract_auth_param(auth_str, "scope")?;

        let auth_url = format!(
            "{realm}?service={}&scope={}",
            urlencoding::encode(&service),
            urlencoding::encode(&scope),
        );

        let credentials = registry_config
            .auth_token
            .as_ref()
            .filter(|_| !self.has_rejected_credentials(registry))
            .map(|token| {
                (
                    registry_config
                        .username
                        .as_deref()
                        .unwrap_or(DEFAULT_AUTH_USERNAME)
                        .to_string(),
                    token.clone(),
                )
            });

        if credentials.is_none() {
            return self.request_registry_v2_token(&auth_url, None).await;
        }

        match self.request_registry_v2_token(&auth_url, credentials).await {
            Ok(token) => Ok(token),
            Err(e) => {
                warn!(
                    "Credentialed auth for {} failed ({:#}); using anonymous tokens for it \
                     from now on. Private repositories there will be unreadable until the \
                     registry's `auth_token` (and, for Docker Hub, `username`) is corrected",
                    registry, e
                );
                self.credentials_rejected
                    .lock()
                    .expect("credentials_rejected mutex poisoned")
                    .insert(registry.to_string());
                self.request_registry_v2_token(&auth_url, None).await
            }
        }
    }

    fn has_rejected_credentials(&self, registry: &str) -> bool {
        self.credentials_rejected
            .lock()
            .expect("credentials_rejected mutex poisoned")
            .contains(registry)
    }

    /// Requests a bearer token from a token endpoint, anonymously when
    /// `credentials` is `None`.
    async fn request_registry_v2_token(
        &self,
        auth_url: &str,
        credentials: Option<(String, String)>,
    ) -> Result<String> {
        debug!(
            "Getting {} registry token from: {}",
            if credentials.is_some() {
                "authenticated"
            } else {
                "anonymous"
            },
            auth_url
        );

        let token_response = self
            .fetch_with_retry(
                || async {
                    let mut request_builder = self.http_client.get(auth_url);
                    if let Some((username, token)) = &credentials {
                        request_builder = request_builder.basic_auth(username, Some(token));
                    }
                    request_builder.send().await
                },
                "Registry auth token request",
            )
            .await?;

        if !token_response.status.is_success() {
            return Err(anyhow!(
                "Failed to get token: {} - {}",
                token_response.status,
                token_response.body
            ));
        }

        let token_json: serde_json::Value = serde_json::from_str(&token_response.body)
            .map_err(|e| anyhow!("Failed to parse auth token response: {}", e))?;

        if let Some(registry_token) = token_json.get("token").and_then(|t| t.as_str()) {
            return Ok(registry_token.to_string());
        }

        Err(anyhow!("Auth response missing 'token' field"))
    }

    fn parse_link_header(&self, link_header: &str) -> Option<String> {
        for link in link_header.split(',') {
            let parts: Vec<&str> = link.trim().split(';').collect();
            if parts.len() >= 2 {
                let url = parts[0].trim();
                if url.starts_with('<') && url.ends_with('>') {
                    let url = &url[1..url.len() - 1];
                    for param in &parts[1..] {
                        let param = param.trim();
                        if param == "rel=\"next\"" || param == "rel=next" {
                            return Some(url.to_string());
                        }
                    }
                }
            }
        }
        None
    }
}

static AUTH_PARAM_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"(\w+)="([^"]+)""#).unwrap());

/// Returns true for transient transport-level failures worth retrying:
/// connection-establishment failures (`tcp connect error: deadline has elapsed`),
/// read/overall timeouts, request-send failures such as hyper's `IncompleteMessage`
/// (`connection closed before message completed`), and body-read failures from a
/// connection dropped mid-stream. Because `fetch_with_retry` reads the body inside
/// the retry scope, `is_body` is meaningful here. Non-transient kinds (`is_decode`,
/// redirect, builder, and HTTP status errors, which arrive as a successful
/// `Response` rather than an `Err`) are not retried.
fn is_transient_transport_error(err: &reqwest::Error) -> bool {
    err.is_connect() || err.is_timeout() || err.is_request() || err.is_body()
}

fn extract_auth_param(auth_str: &str, param: &str) -> Result<String> {
    for caps in AUTH_PARAM_REGEX.captures_iter(auth_str) {
        if caps.get(1).map(|m| m.as_str()) == Some(param) {
            return Ok(caps[2].to_string());
        }
    }
    Err(anyhow!(
        "Missing '{}' in auth challenge: {}",
        param,
        auth_str
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_ref_parsing() {
        let test_cases = vec![
            ("nginx:1.21.0", "docker.io", None, "nginx", "1.21.0"),
            (
                "ubuntu/nginx:1.21.0",
                "docker.io",
                Some("ubuntu"),
                "nginx",
                "1.21.0",
            ),
            (
                "ghcr.io/user/app:v1.0.0",
                "ghcr.io",
                Some("user"),
                "app",
                "v1.0.0",
            ),
            (
                "localhost:5000/myapp:latest",
                "localhost:5000",
                None,
                "myapp",
                "latest",
            ),
            (
                "ghcr.io/schmelczer/fizika/fizika-admin:latest",
                "ghcr.io",
                Some("schmelczer/fizika"),
                "fizika-admin",
                "latest",
            ),
            (
                "registry.example.com/org/team/project/image:v2.0.0",
                "registry.example.com",
                Some("org/team/project"),
                "image",
                "v2.0.0",
            ),
        ];

        for (input, expected_registry, expected_namespace, expected_name, expected_tag) in
            test_cases
        {
            let image_ref = ImageRef::parse(input).unwrap();
            assert_eq!(
                image_ref.registry, expected_registry,
                "Registry mismatch for {}",
                input
            );
            assert_eq!(
                image_ref.namespace,
                expected_namespace.map(String::from),
                "Namespace mismatch for {}",
                input
            );
            assert_eq!(image_ref.name, expected_name, "Name mismatch for {}", input);
            assert_eq!(image_ref.tag, expected_tag, "Tag mismatch for {}", input);
        }
    }

    #[test]
    fn test_repository_path_building() {
        let config = Config::default();
        let client = Client::new(config);

        // Official Docker Hub image (no namespace)
        let image_ref = ImageRef {
            registry: "docker.io".to_string(),
            namespace: None,
            name: "nginx".to_string(),
            tag: "latest".to_string(),
        };
        assert_eq!(client.build_repository_path(&image_ref), "library/nginx");

        // Docker Hub with namespace
        let image_ref = ImageRef {
            registry: "docker.io".to_string(),
            namespace: Some("bitnami".to_string()),
            name: "nginx".to_string(),
            tag: "latest".to_string(),
        };
        assert_eq!(client.build_repository_path(&image_ref), "bitnami/nginx");

        // Custom registry
        let image_ref = ImageRef {
            registry: "ghcr.io".to_string(),
            namespace: Some("user".to_string()),
            name: "app".to_string(),
            tag: "v1.0.0".to_string(),
        };
        assert_eq!(client.build_repository_path(&image_ref), "user/app");
    }

    #[tokio::test]
    async fn test_connect_failure_is_transient() {
        // 192.0.2.1 is TEST-NET-1 (RFC 5737): guaranteed non-routable, so the
        // connect attempt fails (timeout or unreachable). Either way it must be
        // classified as a transient transport error so it gets retried.
        let client = HttpClient::builder()
            .connect_timeout(Duration::from_millis(50))
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap();

        let err = client
            .get("http://192.0.2.1:9/")
            .send()
            .await
            .expect_err("connecting to a non-routable address must fail");

        assert!(
            is_transient_transport_error(&err),
            "connect failure should be retryable, got: {err:?}"
        );
    }

    #[test]
    fn test_parse_v2_response_reports_raw_cursor_and_count() {
        let client = Client::new(Config::default());

        // Mixed semver and non-semver tags. The raw cursor must be the literal
        // last tag (even though it is not semver) so pagination advances past it,
        // and the raw count must include every tag for the scan budget.
        let body =
            r#"{"name":"linuxserver/radarr","tags":["6.1.0","6.1.1","latest","amd64-nightly"]}"#;
        let (versions, raw_last_tag, raw_count) = client.parse_v2_response(body).unwrap();

        assert_eq!(raw_count, 4);
        assert_eq!(raw_last_tag.as_deref(), Some("amd64-nightly"));
        let parsed: Vec<_> = versions.iter().map(|v| v.original.as_str()).collect();
        assert_eq!(parsed, vec!["6.1.0", "6.1.1"]);
    }

    #[test]
    fn test_parse_v2_response_handles_empty_and_null_tags() {
        let client = Client::new(Config::default());

        for body in [r#"{"name":"x","tags":[]}"#, r#"{"name":"x","tags":null}"#] {
            let (versions, raw_last_tag, raw_count) = client.parse_v2_response(body).unwrap();
            assert!(versions.is_empty());
            assert_eq!(raw_last_tag, None);
            assert_eq!(raw_count, 0, "body {body} should yield zero raw tags");
        }
    }

    #[test]
    fn test_parse_link_header() {
        let config = Config::default();
        let client = Client::new(config);

        // Test standard Link header with next rel
        let link_header = r#"<https://registry.example.com/v2/repo/tags/list?n=100&last=tag99>; rel="next", <https://registry.example.com/v2/repo/tags/list?n=100&last=tag999>; rel="last""#;
        let next_url = client.parse_link_header(link_header);
        assert_eq!(
            next_url,
            Some("https://registry.example.com/v2/repo/tags/list?n=100&last=tag99".to_string())
        );

        // Test Link header without quotes around rel value
        let link_header =
            r#"<https://registry.example.com/v2/repo/tags/list?n=100&last=tag99>; rel=next"#;
        let next_url = client.parse_link_header(link_header);
        assert_eq!(
            next_url,
            Some("https://registry.example.com/v2/repo/tags/list?n=100&last=tag99".to_string())
        );

        // Test Link header with no next relation
        let link_header = r#"<https://registry.example.com/v2/repo/tags/list?n=100&last=tag1>; rel="prev", <https://registry.example.com/v2/repo/tags/list?n=100&last=tag999>; rel="last""#;
        let next_url = client.parse_link_header(link_header);
        assert_eq!(next_url, None);

        // Test empty Link header
        let next_url = client.parse_link_header("");
        assert_eq!(next_url, None);

        // Test malformed Link header
        let link_header = "not-a-valid-link-header";
        let next_url = client.parse_link_header(link_header);
        assert_eq!(next_url, None);
    }
}
