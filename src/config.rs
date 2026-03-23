use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub compose_paths: Vec<PathBuf>,
    #[serde(default)]
    pub schedule: String,
    #[serde(default)]
    pub registries: HashMap<String, RegistryConfig>,
    #[serde(default)]
    pub update_strategy: UpdateStrategy,
    #[serde(default)]
    pub ignore_images: Vec<String>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryConfig {
    pub url: String,
    pub auth_token: Option<String>,
}

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
            },
        );
        registries.insert(
            "ghcr.io".to_string(),
            RegistryConfig {
                url: "https://ghcr.io".to_string(),
                auth_token: std::env::var("GITHUB_TOKEN").ok(),
            },
        );

        Self {
            compose_paths: vec![PathBuf::from(".")],
            schedule: "0 0 2 * * *".to_string(), // Daily at 2 AM
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
        config.resolve_env_tokens();
        Ok(config)
    }

    /// Expand environment variable placeholders like ${VAR} in the config content
    pub fn expand_env_vars(content: &str) -> String {
        let env_var_pattern = regex::Regex::new(r"\$\{([^}]+)\}").unwrap();

        env_var_pattern
            .replace_all(content, |caps: &regex::Captures| {
                let var_name = &caps[1];
                std::env::var(var_name).unwrap_or_else(|_| format!("${{{var_name}}}"))
            })
            .to_string()
    }

    /// Resolve environment variable tokens after deserialization
    fn resolve_env_tokens(&mut self) {
        for registry_config in self.registries.values_mut() {
            if let Some(token) = &registry_config.auth_token {
                if token.starts_with("$") {
                    // Handle direct env var references like "$GITHUB_TOKEN"
                    let env_var_name = token.trim_start_matches('$');
                    registry_config.auth_token = std::env::var(env_var_name).ok();
                }
            }
        }
    }

    pub fn is_image_ignored(&self, image: &str) -> bool {
        self.ignore_images
            .iter()
            .any(|ignored| image.contains(ignored))
    }
}
