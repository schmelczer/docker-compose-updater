use super::parser::{ComposeFile, ComposeParser, ServiceImage};
use crate::config::{Config, UpdateStrategy};
use crate::registry::{Client as RegistryClient, ImageRef};
use crate::strategy::create_selector;
use crate::version::{parse_version_tag, VersionInfo};
use anyhow::{anyhow, Result};
use regex::Regex;
use semver::Version;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use tracing::{debug, info, warn};

static IMAGE_LINE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(\s*image:\s*)(?:["']([^"']+)["']|([^\s#]+))(\s*(?:#.*)?)$"#).unwrap()
});

pub struct ComposeUpdater {
    config: Config,
    registry_client: RegistryClient,
    parser: ComposeParser,
}

pub struct UpdateReport {
    pub files_found: usize,
    pub updated_files: Vec<String>,
    /// Human-readable failures collected during the run (one per failed service,
    /// file, or path). Non-empty means the cycle was degraded but did not abort:
    /// every other service and file was still processed and successful updates
    /// were still written.
    pub errors: Vec<String>,
}

impl ComposeUpdater {
    pub fn new(config: Config) -> Self {
        let registry_client = RegistryClient::new(config.clone());
        Self {
            config,
            registry_client,
            parser: ComposeParser::new(),
        }
    }

    pub async fn update_all_compose_files(&self) -> Result<UpdateReport> {
        let mut updated_files = Vec::new();
        let mut files_found = 0;
        let mut errors = Vec::new();

        for compose_path in &self.config.compose_paths {
            let compose_files = match self.find_compose_files(compose_path) {
                Ok(files) => files,
                Err(e) => {
                    errors.push(format!("path {}: {:#}", compose_path.display(), e));
                    continue;
                }
            };
            files_found += compose_files.len();

            for file_path in compose_files {
                // A failure on one file (or one service within it) must not abort
                // the rest of the run: collect it and keep going.
                match self.update_compose_file(&file_path).await {
                    Ok((updated, file_errors)) => {
                        if updated {
                            updated_files.push(file_path);
                        }
                        errors.extend(file_errors);
                    }
                    Err(e) => errors.push(format!("file {}: {:#}", file_path, e)),
                }
            }
        }

        Ok(UpdateReport {
            files_found,
            updated_files,
            errors,
        })
    }

    pub fn parse_compose_file(&self, file_path: &str) -> Result<ComposeFile> {
        self.parser.parse_file(file_path)
    }

    /// Updates every service in a single compose file. Returns whether the file
    /// was modified along with a list of per-service failures. A transient
    /// registry error on one service is recorded and skipped rather than
    /// aborting the file, so unrelated services still get updated and written.
    async fn update_compose_file(&self, file_path: &str) -> Result<(bool, Vec<String>)> {
        info!("Processing compose file: {}", file_path);

        let compose_file = self.parse_compose_file(file_path)?;
        let mut updated = false;
        let mut errors = Vec::new();
        let mut new_content = compose_file.content.clone();

        for service in &compose_file.services {
            if self.config.is_image_ignored(&service.image_ref.to_string()) {
                debug!("Skipping ignored image: {}", service.image_ref);
                continue;
            }

            match self.update_service_image(service).await {
                Ok(Some(new_image)) => {
                    match self.replace_image_in_content(&new_content, service, &new_image) {
                        Ok(replaced) => {
                            new_content = replaced;
                            updated = true;
                            info!(
                                "Updated {}: {} -> {}",
                                service.service_name, service.image_ref, new_image
                            );
                        }
                        Err(e) => {
                            let msg = format!("service {}: {:#}", service.service_name, e);
                            warn!("Failed to update {} in {}", msg, file_path);
                            errors.push(msg);
                        }
                    }
                }
                Ok(None) => {
                    debug!(
                        "No update needed for {}: {}",
                        service.service_name, service.image_ref
                    );
                }
                Err(e) => {
                    let msg = format!("service {}: {:#}", service.service_name, e);
                    warn!("Failed to update {} in {}", msg, file_path);
                    errors.push(msg);
                }
            }
        }

        if updated {
            self.write_updated_content(file_path, new_content)?;
        }

        Ok((updated, errors))
    }

    fn write_updated_content(&self, file_path: &str, content: String) -> Result<()> {
        if self.config.dry_run {
            info!("Dry run: would update {}", file_path);
        } else {
            fs::write(file_path, content)?;
            info!("Updated compose file: {}", file_path);
        }
        Ok(())
    }

    async fn update_service_image(&self, service: &ServiceImage) -> Result<Option<String>> {
        let (current_version, current_prefix, current_suffix, _) =
            parse_version_tag(&service.image_ref.tag);

        let Some(current_version) = current_version else {
            debug!(
                "Skipping non-semver tag '{}' for service {}",
                service.image_ref.tag, service.service_name
            );
            return Ok(None);
        };

        let available_versions = self
            .registry_client
            .get_available_versions(&service.image_ref)
            .await?;

        if available_versions.is_empty() {
            warn!("No versions available for {}", service.image_ref);
            return Ok(None);
        }

        Ok(choose_new_tag(
            &service.image_ref,
            &current_version,
            &available_versions,
            current_prefix,
            current_suffix,
            &self.config.update_strategy,
        ))
    }

    pub fn replace_image_in_content(
        &self,
        content: &str,
        service: &ServiceImage,
        new_image: &str,
    ) -> Result<String> {
        let Some(captures) = IMAGE_LINE_REGEX.captures(&service.original_line) else {
            return Err(anyhow!(
                "Could not parse image line: {}",
                service.original_line
            ));
        };

        let prefix = captures.get(1).unwrap().as_str();
        let suffix = captures.get(4).unwrap().as_str();
        let was_quoted = captures.get(2).is_some();

        let image_part = if was_quoted {
            format!("\"{new_image}\"")
        } else {
            new_image.to_string()
        };

        let new_line = format!("{prefix}{image_part}{suffix}");

        let lines: Vec<&str> = content.lines().collect();
        if service.line_number < lines.len() && lines[service.line_number] == service.original_line
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
                "Line mismatch for service '{}': line {} expected '{}', got '{}'",
                service.service_name,
                service.line_number,
                service.original_line,
                lines.get(service.line_number).unwrap_or(&"<out of bounds>")
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

/// Chooses the new image string for `image_ref`, or `None` when no suitable
/// upgrade exists. The selected version must share the current tag's prefix and
/// suffix and be strictly higher than `current_version`, which prevents both
/// downgrades and no-op rewrites to the same version.
fn choose_new_tag(
    image_ref: &ImageRef,
    current_version: &Version,
    available_versions: &[VersionInfo],
    current_prefix: Option<String>,
    current_suffix: Option<String>,
    strategy: &UpdateStrategy,
) -> Option<String> {
    let selector = create_selector(strategy);
    // Warn here rather than inside the selector: this is the innermost scope that
    // knows *which* image was skipped, and a bare "no matching versions" line is
    // undiagnosable in a run covering dozens of services.
    let Some(target) = selector.select_target_version(
        available_versions,
        current_prefix.clone(),
        current_suffix.clone(),
    ) else {
        warn!(
            "No {:?} candidate for {} (prefix {:?}, suffix {:?}) among {} parseable registry tags",
            strategy,
            image_ref,
            current_prefix,
            current_suffix,
            available_versions.len()
        );
        return None;
    };

    if target.version <= *current_version {
        debug!("{} is already at or above the target {}", image_ref, target);
        return None;
    }

    let mut new_image_ref = image_ref.clone();
    new_image_ref.tag = target.to_string();
    Some(new_image_ref.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version_infos(tags: &[&str]) -> Vec<VersionInfo> {
        tags.iter()
            .map(|t| VersionInfo::from_tag(t).unwrap())
            .collect()
    }

    #[test]
    fn test_choose_new_tag_upgrades_to_higher_version() {
        let image = ImageRef::parse("nginx:1.25.0").unwrap();
        let current = Version::parse("1.25.0").unwrap();
        let available = version_infos(&["1.25.0", "1.26.0", "1.24.0"]);

        let result = choose_new_tag(
            &image,
            &current,
            &available,
            None,
            None,
            &UpdateStrategy::Latest,
        );
        assert_eq!(result, Some("nginx:1.26.0".to_string()));
    }

    #[test]
    fn test_choose_new_tag_prevents_downgrade() {
        let image = ImageRef::parse("nginx:1.25.0").unwrap();
        let current = Version::parse("1.25.0").unwrap();
        // Registry only offers older versions.
        let available = version_infos(&["1.24.0", "1.23.5"]);

        let result = choose_new_tag(
            &image,
            &current,
            &available,
            None,
            None,
            &UpdateStrategy::Latest,
        );
        assert!(result.is_none(), "should never downgrade");
    }

    #[test]
    fn test_choose_new_tag_skips_equal_version() {
        let image = ImageRef::parse("nginx:1.25.0").unwrap();
        let current = Version::parse("1.25.0").unwrap();
        let available = version_infos(&["1.25.0"]);

        let result = choose_new_tag(
            &image,
            &current,
            &available,
            None,
            None,
            &UpdateStrategy::Latest,
        );
        assert!(result.is_none(), "no rewrite when already on the latest");
    }

    #[test]
    fn test_choose_new_tag_respects_prefix_and_suffix() {
        let image = ImageRef::parse("nginx:v1.25.0-alpine").unwrap();
        let current = Version::parse("1.25.0").unwrap();
        let available = vec![
            VersionInfo::from_tag("v1.26.0-alpine").unwrap(),
            VersionInfo::from_tag("1.27.0").unwrap(), // no matching prefix/suffix
            VersionInfo::from_tag("v1.26.0").unwrap(), // missing suffix
        ];

        let result = choose_new_tag(
            &image,
            &current,
            &available,
            Some("v".to_string()),
            Some("-alpine".to_string()),
            &UpdateStrategy::Latest,
        );
        assert_eq!(result, Some("nginx:v1.26.0-alpine".to_string()));
    }
}
