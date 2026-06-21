use docker_compose_updater::compose::updater::ComposeUpdater;
use docker_compose_updater::config::{Config, RegistryConfig, UpdateStrategy};
use docker_compose_updater::registry::{Client as RegistryClient, ImageRef};
use docker_compose_updater::scheduler::Scheduler;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use tempfile::{NamedTempFile, TempDir};

#[test]
fn test_scheduler_invalid_cron_expressions() {
    let invalid_crons = [
        "invalid cron",
        "0 0 25 * * *",
        "60 0 2 * * *",
        "",
        "0 0 2 * *",
    ];

    for cron in invalid_crons {
        let config = Config {
            compose_paths: vec![PathBuf::from("./test")],
            schedule: cron.to_string(),
            registries: HashMap::new(),
            update_strategy: UpdateStrategy::Latest,
            ignore_images: vec![],
            dry_run: true,
        };

        assert!(
            Scheduler::new(config, None).is_err(),
            "Expected error for invalid cron expression: '{cron}'"
        );
    }
}

#[test]
fn test_scheduler_valid_cron_expressions() {
    let valid_crons = [
        "0 0 2 * * *",
        "0 30 1 * * *",
        "0 0 */6 * * *",
        "0 0 2 * * MON",
        "0 15 14 1 * *",
    ];

    for cron in valid_crons {
        let config = Config {
            compose_paths: vec![PathBuf::from("./test")],
            schedule: cron.to_string(),
            registries: HashMap::new(),
            update_strategy: UpdateStrategy::Latest,
            ignore_images: vec![],
            dry_run: true,
        };

        assert!(
            Scheduler::new(config, None).is_ok(),
            "Expected success for valid cron expression: '{cron}'"
        );
    }
}

#[test]
fn test_compose_invalid_file_paths() {
    let updater = ComposeUpdater::new(create_test_config());

    assert!(updater
        .parse_compose_file("/nonexistent/path/file.yml")
        .is_err());

    let temp_dir = TempDir::new().unwrap();
    assert!(updater
        .parse_compose_file(temp_dir.path().to_str().unwrap())
        .is_err());
}

#[test]
fn test_compose_malformed_yaml() {
    let updater = ComposeUpdater::new(create_test_config());

    // Missing services section -> no services found
    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(b"version: '3.8'\n").unwrap();
    let result = updater
        .parse_compose_file(temp_file.path().to_str().unwrap())
        .unwrap();
    assert!(result.services.is_empty());

    // Empty file -> no services found
    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(b"").unwrap();
    let result = updater
        .parse_compose_file(temp_file.path().to_str().unwrap())
        .unwrap();
    assert!(result.services.is_empty());

    // Valid services section with invalid version field -> still finds services
    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file
        .write_all(b"version: invalid\nservices:\n  web:\n    image: nginx:1.21.0\n")
        .unwrap();
    let result = updater
        .parse_compose_file(temp_file.path().to_str().unwrap())
        .unwrap();
    assert_eq!(result.services.len(), 1);
}

#[test]
fn test_image_ref_parsing_edge_cases() {
    // Empty string parses but produces empty name (no panic)
    let result = ImageRef::parse("").unwrap();
    assert_eq!(result.name, "");
    assert_eq!(result.tag, "latest");

    // Missing tag defaults to "latest"
    let result = ImageRef::parse("nginx").unwrap();
    assert_eq!(result.tag, "latest");
    assert_eq!(result.name, "nginx");

    // Registry with namespace
    let result = ImageRef::parse("ghcr.io/user/app:v1").unwrap();
    assert_eq!(result.registry, "ghcr.io");
    assert_eq!(result.namespace, Some("user".to_string()));
    assert_eq!(result.name, "app");
    assert_eq!(result.tag, "v1");

    // Very long name shouldn't panic
    let long_name = "a".repeat(300);
    let _ = ImageRef::parse(&long_name);
}

#[test]
fn test_registry_unknown_registry() {
    let config = Config {
        compose_paths: vec![],
        schedule: "0 0 2 * * *".to_string(),
        registries: HashMap::new(),
        update_strategy: UpdateStrategy::Latest,
        ignore_images: vec![],
        dry_run: true,
    };

    let registry_client = RegistryClient::new(config);

    let image_ref = ImageRef {
        registry: "unknown.registry.com".to_string(),
        namespace: Some("user".to_string()),
        name: "app".to_string(),
        tag: "1.0.0".to_string(),
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(registry_client.get_available_versions(&image_ref));
    assert!(result.is_err());

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Unknown registry"),
        "Expected 'Unknown registry' in error, got: {error_msg}"
    );
}

#[tokio::test]
async fn test_compose_update_with_empty_paths() {
    let config = Config {
        compose_paths: vec![],
        ..create_test_config()
    };
    let updater = ComposeUpdater::new(config);

    let result = updater.update_all_compose_files().await.unwrap();
    assert_eq!(result.files_found, 0);
    assert!(result.updated_files.is_empty());
    assert!(result.errors.is_empty());
}

#[tokio::test]
async fn test_update_continues_after_service_failure() {
    // Two services point at registries absent from the config, so each fails
    // fast with "Unknown registry" (no network). With an empty registry map and
    // a non-semver tag, the third service is skipped without error. This proves
    // a failure on the first service no longer aborts the run before the second.
    let temp_dir = TempDir::new().unwrap();
    let compose_path = temp_dir.path().join("stack.yml");
    std::fs::write(
        &compose_path,
        "services:\n  \
           skipped:\n    image: nginx:latest\n  \
           bad_one:\n    image: unknown.registry.example/foo/app:1.0.0\n  \
           bad_two:\n    image: another.unknown.example/bar/app:2.0.0\n",
    )
    .unwrap();

    let config = Config {
        compose_paths: vec![temp_dir.path().to_path_buf()],
        schedule: "0 0 2 * * *".to_string(),
        registries: HashMap::new(),
        update_strategy: UpdateStrategy::Latest,
        ignore_images: vec![],
        dry_run: true,
    };

    let updater = ComposeUpdater::new(config);
    let report = updater.update_all_compose_files().await.unwrap();

    assert_eq!(report.files_found, 1);
    assert!(report.updated_files.is_empty());
    assert_eq!(
        report.errors.len(),
        2,
        "both failing services should be attempted and recorded, got: {:?}",
        report.errors
    );
}

#[tokio::test]
async fn test_scheduler_reports_failure_on_service_error() {
    // A degraded cycle (a service that cannot be resolved) must surface as an
    // error from run_once even though the run itself did not abort.
    let temp_dir = TempDir::new().unwrap();
    let compose_path = temp_dir.path().join("stack.yml");
    std::fs::write(
        &compose_path,
        "services:\n  bad:\n    image: unknown.registry.example/foo/app:1.0.0\n",
    )
    .unwrap();

    let config = Config {
        compose_paths: vec![temp_dir.path().to_path_buf()],
        schedule: "0 0 2 * * *".to_string(),
        registries: HashMap::new(),
        update_strategy: UpdateStrategy::Latest,
        ignore_images: vec![],
        dry_run: true,
    };

    let scheduler = Scheduler::new(config, None).unwrap();
    assert!(
        scheduler.run_once().await.is_err(),
        "a cycle where a service fails to update should report an error"
    );
}

#[test]
fn test_config_edge_cases() {
    let minimal_config = Config {
        compose_paths: vec![],
        schedule: "0 0 2 * * *".to_string(),
        registries: HashMap::new(),
        update_strategy: UpdateStrategy::Latest,
        ignore_images: vec![],
        dry_run: true,
    };

    assert!(Scheduler::new(minimal_config.clone(), None).is_ok());
    let _updater = ComposeUpdater::new(minimal_config.clone());
    assert!(!minimal_config.is_image_ignored("any-image:tag"));
}

#[test]
fn test_version_selection_with_mismatched_prefixes_suffixes() {
    use docker_compose_updater::strategy::create_selector;
    use docker_compose_updater::version::VersionInfo;

    let selector = create_selector(&create_test_config().update_strategy);

    let current_prefix = Some("v".to_string());
    let current_suffix = Some("-alpine".to_string());

    let available = [
        VersionInfo::from_tag("1.3.0").unwrap(),
        VersionInfo::from_tag("v1.3.0-ubuntu").unwrap(),
        VersionInfo::from_tag("release-1.1.5-alpine").unwrap(),
    ];

    let target = selector.select_target_version(&available, current_prefix, current_suffix);
    assert!(
        target.is_none(),
        "No versions match both prefix 'v' and suffix '-alpine'"
    );
}

fn create_test_config() -> Config {
    let mut registries = HashMap::new();
    registries.insert(
        "docker.io".to_string(),
        RegistryConfig {
            url: "https://registry-1.docker.io".to_string(),
            auth_token: None,
        },
    );

    Config {
        compose_paths: vec![PathBuf::from("./test")],
        schedule: "0 0 2 * * *".to_string(),
        registries,
        update_strategy: UpdateStrategy::Latest,
        ignore_images: vec![],
        dry_run: true,
    }
}
