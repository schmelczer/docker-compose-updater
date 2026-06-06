use crate::config::{Config, RegistryConfig};
use crate::version::VersionInfo;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use reqwest::{Client as HttpClient, Response, StatusCode};
use serde::Deserialize;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, warn};

const PAGE_SIZE: usize = 500;
const MAX_PAGES: usize = 100;
const MAX_RETRY_ATTEMPTS: u32 = 5;
const MAX_AUTH_ATTEMPTS: u32 = 2;
const INITIAL_RETRY_DELAY_SECS: u64 = 1;
const HTTP_TIMEOUT_SECS: u64 = 30;
const HTTP_CONNECT_TIMEOUT_SECS: u64 = 10;

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

#[derive(Deserialize)]
struct DockerHubTagsResponse {
    results: Vec<DockerHubTag>,
    next: Option<String>,
}

#[derive(Deserialize)]
struct DockerHubTag {
    name: String,
}

pub struct Client {
    http_client: HttpClient,
    config: Config,
}

impl Client {
    pub fn new(config: Config) -> Self {
        let http_client = HttpClient::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(HTTP_CONNECT_TIMEOUT_SECS))
            .build()
            .expect("failed to build HTTP client");

        Self {
            http_client,
            config,
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

    async fn request_with_retry<F, Fut>(
        &self,
        request_fn: F,
        operation_name: &str,
    ) -> Result<Response>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<Response, reqwest::Error>>,
    {
        let mut attempt = 0;
        let mut delay = Duration::from_secs(INITIAL_RETRY_DELAY_SECS);

        loop {
            let response = request_fn().await?;

            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                attempt += 1;

                if attempt > MAX_RETRY_ATTEMPTS {
                    return Err(anyhow!(
                        "{} failed: Rate limited (429) after {} attempts",
                        operation_name,
                        MAX_RETRY_ATTEMPTS
                    ));
                }

                let wait_duration = if let Some(retry_after) = response
                    .headers()
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
                return Ok(response);
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

    fn parse_dockerhub_response(
        &self,
        response_text: &str,
    ) -> Result<(Vec<VersionInfo>, Option<String>)> {
        let dockerhub_response: DockerHubTagsResponse = serde_json::from_str(response_text)
            .map_err(|e| anyhow!("Failed to parse Docker Hub response: {}", e))?;

        let mut versions = Vec::new();

        for tag in dockerhub_response.results {
            if let Some(version_info) = VersionInfo::from_tag(&tag.name) {
                versions.push(version_info);
            }
        }

        Ok((versions, dockerhub_response.next))
    }

    fn parse_v2_response(&self, response_text: &str) -> Result<Vec<VersionInfo>> {
        debug!("Parsing registry v2 response: {}", response_text);
        let tags_response: serde_json::Value = serde_json::from_str(response_text)
            .map_err(|e| anyhow!("Failed to parse registry response: {}", e))?;

        let mut versions = Vec::new();

        if let Some(tags) = tags_response.get("tags").and_then(|t| t.as_array()) {
            for tag in tags {
                if let Some(tag_str) = tag.as_str() {
                    if let Some(version_info) = VersionInfo::from_tag(tag_str) {
                        versions.push(version_info);
                    }
                }
            }
        }

        Ok(versions)
    }

    pub async fn get_available_versions(&self, image_ref: &ImageRef) -> Result<Vec<VersionInfo>> {
        if image_ref.registry == "docker.io" {
            self.get_dockerhub_versions(image_ref).await
        } else {
            self.get_registry_v2_versions(image_ref).await
        }
    }

    async fn get_dockerhub_versions(&self, image_ref: &ImageRef) -> Result<Vec<VersionInfo>> {
        let repo_path = self.build_repository_path(image_ref);
        let mut results = Vec::new();
        let mut page_count: usize = 0;
        let mut url = format!(
            "https://hub.docker.com/v2/repositories/{repo_path}/tags/?page_size={PAGE_SIZE}"
        );

        loop {
            debug!("Docker Hub API URL: {}", url);

            let url_clone = url.clone();
            let response = self
                .request_with_retry(
                    || async { self.http_client.get(&url_clone).send().await },
                    "Docker Hub API request",
                )
                .await?;
            debug!("Docker Hub response status: {}", response.status());

            if !response.status().is_success() {
                return Err(anyhow!(
                    "Docker Hub request failed with status {}: {}",
                    response.status(),
                    response.text().await.unwrap_or_default()
                ));
            }

            let response_text = response.text().await?;

            let (new_tags, next_page) = self.parse_dockerhub_response(&response_text)?;

            results.extend(new_tags);

            page_count += 1;
            if page_count >= MAX_PAGES {
                warn!(
                    "Reached maximum page count ({}) for {}, stopping pagination",
                    MAX_PAGES, image_ref
                );
                break;
            }

            if let Some(next) = next_page {
                url = next;
            } else {
                break;
            }
        }

        Ok(results)
    }

    async fn get_registry_v2_versions(&self, image_ref: &ImageRef) -> Result<Vec<VersionInfo>> {
        let registry_config = self.get_registry_config(&image_ref.registry)?;
        let repo_path = self.build_repository_path(image_ref);

        let mut results = Vec::new();
        let mut last_tag: Option<String> = None;
        let mut next_url: Option<String> = None;
        let mut bearer_token: Option<String> = None;
        let mut auth_attempts: u32 = 0;
        let mut page_count: usize = 0;

        loop {
            let url = if let Some(next) = next_url.take() {
                if next.starts_with("http://") || next.starts_with("https://") {
                    next
                } else {
                    format!("{}{next}", registry_config.url)
                }
            } else {
                format!(
                    "{}/v2/{repo_path}/tags/list?n={PAGE_SIZE}{}",
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
            let response = self
                .request_with_retry(
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

            let (new_tags, link_next) = if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                if auth_attempts >= MAX_AUTH_ATTEMPTS {
                    return Err(anyhow!(
                        "Authentication failed after {} attempts for registry {}",
                        auth_attempts,
                        image_ref.registry
                    ));
                }
                if let Some(token) = &registry_config.auth_token {
                    if let Some(auth_header) = response.headers().get("www-authenticate") {
                        let auth_str = auth_header.to_str().map_err(|e| {
                            anyhow!("Invalid WWW-Authenticate header encoding: {}", e)
                        })?;
                        bearer_token = self.try_registry_v2_auth(auth_str, token).await?;
                        auth_attempts += 1;
                        continue;
                    } else {
                        return Err(anyhow!(
                            "Unauthorized request but no WWW-Authenticate header found"
                        ));
                    }
                } else {
                    return Err(anyhow!("Unauthorized request but no auth token configured"));
                }
            } else if response.status().is_success() {
                let link_next = response
                    .headers()
                    .get("link")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|h| self.parse_link_header(h));

                let response_text = response.text().await?;
                let tags = self.parse_v2_response(&response_text)?;
                (tags, link_next)
            } else {
                let status = response.status();
                let error_text = response.text().await.unwrap_or_default();
                return Err(anyhow!(
                    "Registry request failed with status {}: {}",
                    status,
                    error_text
                ));
            };

            page_count += 1;
            if page_count >= MAX_PAGES {
                warn!(
                    "Reached maximum page count ({}) for {}, stopping pagination",
                    MAX_PAGES, image_ref
                );
                results.extend(new_tags);
                break;
            }

            let maybe_last_tag = new_tags.last().map(|v| v.original.clone());
            results.extend(new_tags);

            if let Some(next) = link_next {
                next_url = Some(next);
            } else if let Some(ref last) = maybe_last_tag {
                if last_tag.as_ref() == Some(last) {
                    break;
                }
                last_tag = maybe_last_tag;
            } else {
                break;
            }
        }

        Ok(results)
    }

    async fn try_registry_v2_auth(&self, auth_str: &str, token: &str) -> Result<Option<String>> {
        let realm = extract_auth_param(auth_str, "realm")?;
        let service = extract_auth_param(auth_str, "service")?;
        let scope = extract_auth_param(auth_str, "scope")?;

        let auth_url = format!(
            "{realm}?service={}&scope={}",
            urlencoding::encode(&service),
            urlencoding::encode(&scope),
        );
        debug!("Getting registry token from: {}", auth_url);

        let auth_url_clone = auth_url.clone();
        let token_clone = token.to_string();
        let token_response = self
            .request_with_retry(
                || async {
                    self.http_client
                        .get(&auth_url_clone)
                        .basic_auth("token", Some(&token_clone))
                        .send()
                        .await
                },
                "Registry auth token request",
            )
            .await?;

        if !token_response.status().is_success() {
            return Err(anyhow!(
                "Failed to get token: {} - {}",
                token_response.status(),
                token_response.text().await.unwrap_or_default()
            ));
        }

        let token_json: serde_json::Value = token_response.json().await?;

        if let Some(registry_token) = token_json.get("token").and_then(|t| t.as_str()) {
            return Ok(Some(registry_token.to_string()));
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
