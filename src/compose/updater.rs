use super::parser::{ComposeFile, ServiceImage};
use crate::config::{Config, UpdateStrategy};
use crate::registry::{Client as RegistryClient, ImageRef};
use crate::strategy::select_target_version;
use crate::version::VersionInfo;
use anyhow::{ensure, Context, Result};
use std::fs;
use tracing::{debug, info, warn};

pub struct ComposeUpdater {
    config: Config,
    registry_client: RegistryClient,
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
        }
    }

    pub async fn update_all_compose_files(&self) -> UpdateReport {
        let mut updated_files = Vec::new();
        let mut files_found = 0;
        let mut errors = Vec::new();

        for compose_path in &self.config.compose_paths {
            for entry in walkdir::WalkDir::new(compose_path).follow_links(true) {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        errors.push(format!("path {}: {}", compose_path.display(), error));
                        continue;
                    }
                };
                if !entry.file_type().is_file()
                    || !matches!(
                        entry.path().extension().and_then(|ext| ext.to_str()),
                        Some("yml" | "yaml")
                    )
                {
                    continue;
                }
                files_found += 1;
                let file_path = entry.path().to_string_lossy().into_owned();
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

        UpdateReport {
            files_found,
            updated_files,
            errors,
        }
    }

    /// Updates every service in a single compose file. Returns whether the file
    /// was modified along with a list of per-service failures. A transient
    /// registry error on one service is recorded and skipped rather than
    /// aborting the file, so unrelated services still get updated and written.
    async fn update_compose_file(&self, file_path: &str) -> Result<(bool, Vec<String>)> {
        info!("Processing compose file: {}", file_path);

        let mut compose_file = ComposeFile::read(file_path)?;
        let mut updated = false;
        let mut errors = Vec::new();

        for service in compose_file.images()? {
            if self.config.is_image_ignored(&service.image_ref.to_string()) {
                debug!("Skipping ignored image: {}", service.image_ref);
                continue;
            }

            match self.update_service_image(&service).await {
                Ok(Some(new_image)) => {
                    compose_file.set_image(&service.service_name, &new_image)?;
                    updated = true;
                    info!(
                        "Updated {}: {} -> {}",
                        service.service_name, service.image_ref, new_image
                    );
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
            let content = serde_yaml::to_string(&compose_file.document)?;
            self.write_updated_content(file_path, &compose_file.original_content, content)?;
        }

        Ok((updated, errors))
    }

    fn write_updated_content(
        &self,
        file_path: &str,
        original_content: &str,
        content: String,
    ) -> Result<()> {
        if self.config.dry_run {
            info!("Dry run: would update {}", file_path);
        } else {
            // Registry lookups can take minutes. Check the entire snapshot, not
            // just the image lines, immediately before writing so edits to other
            // properties and comments are protected as well.
            let current_content = fs::read_to_string(file_path)
                .context("Could not re-read Compose file before writing; update skipped")?;
            ensure!(
                current_content == original_content,
                "Compose file changed during registry lookups; update skipped, retry on the next run"
            );
            fs::write(file_path, content)?;
            info!("Updated compose file: {}", file_path);
        }
        Ok(())
    }

    async fn update_service_image(&self, service: &ServiceImage) -> Result<Option<String>> {
        let Some(current) = VersionInfo::from_tag(&service.image_ref.tag) else {
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
            &current,
            &available_versions,
            &self.config.update_strategy,
        ))
    }
}

/// Chooses the new image string for `image_ref`, or `None` when no suitable
/// upgrade exists. The selected version must share the current tag's prefix and
/// suffix and be strictly higher than the current version, which prevents both
/// downgrades and no-op rewrites to the same version.
fn choose_new_tag(
    image_ref: &ImageRef,
    current: &VersionInfo,
    available_versions: &[VersionInfo],
    strategy: &UpdateStrategy,
) -> Option<String> {
    // Warn here rather than inside the selector: this is the innermost scope that
    // knows *which* image was skipped, and a bare "no matching versions" line is
    // undiagnosable in a run covering dozens of services.
    let Some(target) = select_target_version(
        strategy,
        available_versions,
        current.prefix.as_deref(),
        current.suffix.as_deref(),
    ) else {
        warn!(
            "No {:?} candidate for {} (prefix {:?}, suffix {:?}) among {} parseable registry tags",
            strategy,
            image_ref,
            current.prefix,
            current.suffix,
            available_versions.len()
        );
        return None;
    };

    if target.version <= current.version {
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
    use crate::config::RegistryConfig;
    use axum::{routing::get, Json, Router};
    use tempfile::TempDir;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn test_edits_during_registry_lookup_are_preserved() {
        for edit in [
            Some("services:\n  web:\n    image: test.registry/app:1.0.0\n# User comment\n"),
            Some("services:\n  web:\n    image: test.registry/app:3.0.0\n"),
            None,
        ] {
            check_update_during_lookup(
                "services:\n  web:\n    image: test.registry/app:1.0.0\n",
                edit,
                true,
            )
            .await;
        }
    }

    #[tokio::test]
    async fn test_unchanged_file_is_updated_after_registry_lookup() {
        for original in [
            "services: {web: {image: 'test.registry/app:1.0.0', ports: ['80:80']}}",
            "x-base: &base {image: 'test.registry/app:1.0.0', ports: ['80:80']}\nservices: {web: *base}",
            "x-base: &base {image: 'test.registry/app:1.0.0', ports: ['80:80']}\nservices:\n  web:\n    <<: *base\n  build_only: {image: '', build: .}",
            r#"
x-base: &base {KEY: correct}
x-derived: &derived {<<: *base}
services:
  web:
    image: test.registry/app:1.0.0
    environment:
      <<: [*derived, {KEY: wrong}]
"#,
        ] {
            check_update_during_lookup(original, None, false).await;
        }
    }

    async fn check_update_during_lookup(original: &str, edit: Option<&str>, change_file: bool) {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("compose.yaml");
        fs::write(&path, original).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server_path = path.clone();
        let edit = edit.map(str::to_owned);
        let server_edit = edit.clone();
        let app = Router::new().route(
            "/v2/app/tags/list",
            get(move || {
                let path = server_path.clone();
                let edit = server_edit.clone();
                async move {
                    // Apply an edit while the updater is awaiting the registry.
                    if change_file {
                        match edit {
                            Some(content) => fs::write(path, content).unwrap(),
                            None => fs::remove_file(path).unwrap(),
                        }
                    }
                    Json(serde_json::json!({"name": "app", "tags": ["1.0.0", "2.0.0"]}))
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut config = Config {
            compose_paths: vec![path.clone()],
            update_strategy: UpdateStrategy::Latest,
            dry_run: false,
            registry_request_interval_ms: 0,
            ..Config::default()
        };
        config.registries.insert(
            "test.registry".to_string(),
            RegistryConfig {
                url,
                auth_token: None,
                username: None,
            },
        );
        let report = ComposeUpdater::new(config).update_all_compose_files().await;
        server.abort();

        if change_file {
            assert!(report.updated_files.is_empty());
            assert_eq!(report.errors.len(), 1);
            if let Some(edit) = edit {
                assert_eq!(fs::read_to_string(path).unwrap(), edit);
                assert!(report.errors[0].contains("changed"), "{:?}", report.errors);
            } else {
                assert!(!path.exists(), "a deleted file must not be recreated");
            }
        } else {
            assert!(report.errors.is_empty(), "{:?}", report.errors);
            assert_eq!(report.updated_files, vec![path.to_string_lossy()]);
            let mut expected = ComposeFile::parse(original).unwrap().document;
            expected["services"]["web"]["image"] = "test.registry/app:2.0.0".into();
            let updated: serde_yaml::Value =
                serde_yaml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(updated, expected);
        }
    }

    fn version_infos(tags: &[&str]) -> Vec<VersionInfo> {
        tags.iter()
            .map(|t| VersionInfo::from_tag(t).unwrap())
            .collect()
    }

    #[test]
    fn test_choose_new_tag_upgrades_to_higher_version() {
        let image = ImageRef::parse("nginx:1.25.0").unwrap();
        let current = VersionInfo::from_tag(&image.tag).unwrap();
        let available = version_infos(&["1.25.0", "1.26.0", "1.24.0"]);

        let result = choose_new_tag(&image, &current, &available, &UpdateStrategy::Latest);
        assert_eq!(result, Some("nginx:1.26.0".to_string()));
    }

    #[test]
    fn test_choose_new_tag_prevents_downgrade() {
        let image = ImageRef::parse("nginx:1.25.0").unwrap();
        let current = VersionInfo::from_tag(&image.tag).unwrap();
        // Registry only offers older versions.
        let available = version_infos(&["1.24.0", "1.23.5"]);

        let result = choose_new_tag(&image, &current, &available, &UpdateStrategy::Latest);
        assert!(result.is_none(), "should never downgrade");
    }

    #[test]
    fn test_choose_new_tag_skips_equal_version() {
        let image = ImageRef::parse("nginx:1.25.0").unwrap();
        let current = VersionInfo::from_tag(&image.tag).unwrap();
        let available = version_infos(&["1.25.0"]);

        let result = choose_new_tag(&image, &current, &available, &UpdateStrategy::Latest);
        assert!(result.is_none(), "no rewrite when already on the latest");
    }

    #[test]
    fn test_choose_new_tag_respects_prefix_and_suffix() {
        let image = ImageRef::parse("nginx:v1.25.0-alpine").unwrap();
        let current = VersionInfo::from_tag(&image.tag).unwrap();
        let available = vec![
            VersionInfo::from_tag("v1.26.0-alpine").unwrap(),
            VersionInfo::from_tag("1.27.0").unwrap(), // no matching prefix/suffix
            VersionInfo::from_tag("v1.26.0").unwrap(), // missing suffix
        ];

        let result = choose_new_tag(&image, &current, &available, &UpdateStrategy::Latest);
        assert_eq!(result, Some("nginx:v1.26.0-alpine".to_string()));
    }
}
