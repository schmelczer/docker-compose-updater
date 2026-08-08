use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;
use tracing::{info, warn};

static ENV_VAR_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{([^}]+)\}|\$([A-Za-z_][A-Za-z0-9_]*)").unwrap());

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub compose_paths: Vec<PathBuf>,
    pub schedule: String,
    pub registries: HashMap<String, RegistryConfig>,
    pub update_strategy: UpdateStrategy,
    pub ignore_images: Vec<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryConfig {
    pub url: String,
    pub auth_token: Option<String>,
    /// Basic-auth username paired with `auth_token` when exchanging a registry
    /// challenge for a bearer token. ghcr.io and GitLab ignore it (the token
    /// carries the identity), hence the "token" default; Docker Hub validates it
    /// and rejects anything but the real account name, so a `dckr_pat_…` token
    /// needs the owning username set here.
    pub username: Option<String>,
}

/// Username used with `auth_token` when a registry config does not set one.
pub const DEFAULT_AUTH_USERNAME: &str = "token";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum UpdateStrategy {
    #[default]
    LatestPatchOfPreviousMinor,
    Latest,
}

impl Default for Config {
    fn default() -> Self {
        let mut registries = HashMap::new();
        registries.insert(
            "docker.io".to_string(),
            RegistryConfig {
                url: "https://registry-1.docker.io".to_string(),
                auth_token: None,
                username: None,
            },
        );
        registries.insert(
            "ghcr.io".to_string(),
            RegistryConfig {
                url: "https://ghcr.io".to_string(),
                auth_token: std::env::var("GITHUB_TOKEN").ok(),
                username: None,
            },
        );

        Self {
            compose_paths: vec![PathBuf::from(".")],
            schedule: "0 0 2 * * *".to_string(),
            registries,
            update_strategy: UpdateStrategy::LatestPatchOfPreviousMinor,
            ignore_images: vec![],
            dry_run: false,
        }
    }
}

impl Config {
    pub fn load(config_path: PathBuf) -> Result<Self> {
        info!("Loading configuration from {}", config_path.display());
        let content = std::fs::read_to_string(config_path)?;
        let expanded_content = Self::expand_env_vars(&content);
        let mut config: Self = serde_yaml::from_str(&expanded_content)?;
        config.normalize_auth_tokens();
        config.restore_missing_default_registries();
        Ok(config)
    }

    /// A `registries` block in the config file replaces the defaults wholesale
    /// rather than merging, so one that lists only (say) ghcr.io would leave
    /// Docker Hub unresolvable. Put the defaults back for any key the file did
    /// not set; an explicit entry always wins.
    fn restore_missing_default_registries(&mut self) {
        for (name, registry) in Self::default().registries {
            self.registries.entry(name).or_insert(registry);
        }
    }

    pub fn expand_env_vars(content: &str) -> String {
        ENV_VAR_REGEX
            .replace_all(content, |caps: &regex::Captures| {
                let var_name = caps.get(1).or_else(|| caps.get(2)).unwrap().as_str();
                std::env::var(var_name).unwrap_or_else(|_| {
                    warn!(
                        "Environment variable `{}` referenced in config is not set; substituting an empty string",
                        var_name
                    );
                    String::new()
                })
            })
            .into_owned()
    }

    /// Treat a registry `auth_token` that expanded to an empty string (e.g. an
    /// unset `${TOKEN}`) as no token at all, so we fall back to unauthenticated
    /// access with a clear error instead of attempting auth with an empty token.
    fn normalize_auth_tokens(&mut self) {
        for registry in self.registries.values_mut() {
            if registry.auth_token.as_deref() == Some("") {
                registry.auth_token = None;
            }
        }
    }

    pub fn is_image_ignored(&self, image: &str) -> bool {
        self.ignore_images
            .iter()
            .any(|pattern| image.contains(pattern))
    }
}
