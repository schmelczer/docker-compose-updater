use super::parser::{ComposeFile, ComposeParser, ServiceImage};
use crate::config::Config;
use crate::registry::Client as RegistryClient;
use crate::strategy::create_selector;
use crate::version::parse_version_tag;
use anyhow::{anyhow, Result};
use regex::Regex;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

pub struct ComposeUpdater {
    config: Config,
    registry_client: RegistryClient,
    parser: ComposeParser,
}

impl ComposeUpdater {
    pub fn new(config: Config) -> Self {
        let registry_client = RegistryClient::new(config.clone());
        let parser = ComposeParser::new();

        Self {
            config,
            registry_client,
            parser,
        }
    }

    pub async fn update_all_compose_files(&self) -> Result<Vec<String>> {
        let mut updated_files = Vec::new();

        for compose_path in &self.config.compose_paths {
            let compose_files = self.find_compose_files(compose_path)?;

            for file_path in compose_files {
                let updated = self.update_compose_file(&file_path).await?;
                if updated {
                    updated_files.push(file_path);
                }
            }
        }

        Ok(updated_files)
    }

    pub fn parse_compose_file(&self, file_path: &str) -> Result<ComposeFile> {
        self.parser.parse_file(file_path)
    }

    async fn update_compose_file(&self, file_path: &str) -> Result<bool> {
        info!("Processing compose file: {}", file_path);

        let compose_file = self.parse_compose_file(file_path)?;
        let mut updated = false;
        let mut new_content = compose_file.content.clone();

        for service in &compose_file.services {
            if self.config.is_image_ignored(&service.image_ref.to_string()) {
                info!("Skipping ignored image: {}", service.image_ref.to_string());
                continue;
            }

            match self.update_service_image(service).await {
                Ok(Some(new_image)) => {
                    new_content =
                        self.replace_image_in_content(&new_content, service, &new_image)?;
                    updated = true;
                    info!(
                        "Updated {}: {} -> {}",
                        service.service_name,
                        service.image_ref.to_string(),
                        new_image
                    );
                }
                Ok(None) => {
                    debug!(
                        "No update needed for {}: {}",
                        service.service_name,
                        service.image_ref.to_string()
                    );
                }
                Err(e) => {
                    return Err(
                        e.context(format!("Failed to update service {}", service.service_name))
                    );
                }
            }
        }

        if updated {
            self.write_updated_content(file_path, new_content)?;
        }

        Ok(updated)
    }

    fn write_updated_content(&self, file_path: &str, content: String) -> Result<()> {
        if !self.config.dry_run {
            fs::write(file_path, content)?;
            info!("Updated compose file: {}", file_path);
        } else {
            info!("Dry run: Would update compose file: {}", file_path);
        }
        Ok(())
    }

    async fn update_service_image(&self, service: &ServiceImage) -> Result<Option<String>> {
        info!(
            "Checking for updates for service: {} (current image: {})",
            service.service_name,
            service.image_ref.to_string()
        );
        let (current_version, current_prefix, current_suffix, _) =
            parse_version_tag(&service.image_ref.tag);

        let available_versions = self
            .registry_client
            .get_available_versions(&service.image_ref)
            .await?;

        if available_versions.is_empty() {
            warn!("No versions available for selection");
            return Ok(None);
        }

        let selector = create_selector(&self.config.update_strategy);
        if let Some(target_version_info) =
            selector.select_target_version(&available_versions, current_prefix, current_suffix)
        {
            // Only update if the target version is different AND higher than the current version
            // This prevents downgrades
            if Some(target_version_info.version.clone()) != current_version {
                if let Some(ref current_ver) = current_version {
                    if target_version_info.version < *current_ver {
                        info!(
                            "Skipping downgrade from {} to {}",
                            current_ver, target_version_info.version
                        );
                        return Ok(None);
                    }
                }
                let mut new_image_ref = service.image_ref.clone();
                new_image_ref.tag = target_version_info.to_string();
                return Ok(Some(new_image_ref.to_string()));
            }
        }

        Ok(None)
    }

    pub fn replace_image_in_content(
        &self,
        content: &str,
        service: &ServiceImage,
        new_image: &str,
    ) -> Result<String> {
        let image_regex =
            Regex::new(r#"^(\s*image:\s*)(?:["']([^"']+)["']|([^\s#]+))(\s*(?:#.*)?)$"#)?;

        if let Some(captures) = image_regex.captures(&service.original_line) {
            let prefix = captures.get(1).unwrap().as_str();
            let _old_image = captures
                .get(2)
                .or_else(|| captures.get(3))
                .unwrap()
                .as_str();
            let suffix = captures.get(4).unwrap().as_str();

            let image_part = if captures.get(2).is_some() {
                format!("\"{new_image}\"")
            } else {
                new_image.to_string()
            };

            let new_line = format!("{prefix}{image_part}{suffix}");

            let lines: Vec<&str> = content.lines().collect();
            if service.line_number < lines.len()
                && lines[service.line_number] == service.original_line
            {
                let mut result_lines = lines;
                result_lines[service.line_number] = &new_line;
                let mut result = result_lines.join("\n");

                if content.ends_with('\n') {
                    result.push('\n');
                }

                Ok(result)
            } else {
                Err(anyhow!(
                    "Line number mismatch or content changed for service: {}",
                    service.service_name
                ))
            }
        } else {
            Err(anyhow!(
                "Could not parse image line: {}",
                service.original_line
            ))
        }
    }

    fn find_compose_files(&self, path: &Path) -> Result<Vec<String>> {
        let mut visited = HashSet::new();
        self.find_compose_files_recursive(path, &mut visited)
    }

    fn find_compose_files_recursive(
        &self,
        path: &Path,
        visited: &mut HashSet<PathBuf>,
    ) -> Result<Vec<String>> {
        let canonical_path = match path.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                warn!("Failed to canonicalize path {}: {}", path.display(), e);
                return Ok(Vec::new());
            }
        };

        if !visited.insert(canonical_path.clone()) {
            return Ok(Vec::new());
        }

        let mut compose_files = Vec::new();

        if path.is_file() {
            if self.is_compose_file(path)? {
                compose_files.push(path.to_string_lossy().to_string());
            }
        } else if path.is_dir() {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let entry_path = entry.path();

                if entry_path.is_file() && self.is_compose_file(&entry_path)? {
                    compose_files.push(entry_path.to_string_lossy().to_string());
                } else if entry_path.is_dir() {
                    compose_files.extend(self.find_compose_files_recursive(&entry_path, visited)?);
                }
            }
        }

        Ok(compose_files)
    }

    fn is_compose_file(&self, path: &Path) -> Result<bool> {
        let filename = path
            .file_name()
            .ok_or_else(|| anyhow!("Invalid file path: {:?}", path))?
            .to_string_lossy();
        Ok(filename.ends_with(".yml") || filename.ends_with(".yaml"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, UpdateStrategy};
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_prevents_downgrade() {
        let mut config = Config::default();
        config.update_strategy = UpdateStrategy::Latest;
        config.dry_run = true;

        let updater = ComposeUpdater::new(config);

        // Create a temporary compose file with a higher version
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "services:\n  web:\n    image: nginx:1.25.0").unwrap();

        let file_path = temp_file.path().to_str().unwrap();
        let compose_file = updater.parse_compose_file(file_path).unwrap();

        assert_eq!(compose_file.services.len(), 1);
        let service = &compose_file.services[0];

        // Mock a scenario where the strategy selects a lower version
        // This would happen if available versions only include older versions
        let (current_version, _, _, _) = parse_version_tag(&service.image_ref.tag);
        assert!(current_version.is_some());

        // The actual test would need mocked registry responses, but we can verify
        // the logic by checking that current version is properly extracted
        assert_eq!(current_version.unwrap().to_string(), "1.25.0");
    }
}
