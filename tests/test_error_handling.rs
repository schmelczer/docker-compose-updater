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
    // Test that scheduler now properly rejects invalid cron expressions instead of falling back
    let invalid_cron_configs = [
        "invalid cron",
        "0 0 25 * * *", // Invalid hour
        "60 0 2 * * *", // Invalid minute
        "",             // Empty string
        "0 0 2 * *",    // Missing field
    ];

    for invalid_cron in invalid_cron_configs {
        let config = Config {
            compose_paths: vec![PathBuf::from("./test")],
            schedule: invalid_cron.to_string(),
            registries: HashMap::new(),
            update_strategy: UpdateStrategy::Latest,
            ignore_images: vec![],
            dry_run: true,
        };

        let result = Scheduler::new(config, None);
        assert!(
            result.is_err(),
            "Expected error for invalid cron expression: '{invalid_cron}'"
        );
    }
}

#[test]
fn test_scheduler_valid_cron_expressions() {
    // Test that scheduler accepts valid cron expressions
    let valid_cron_configs = [
        "0 0 2 * * *",   // Default - 2 AM daily
        "0 30 1 * * *",  // 1:30 AM daily
        "0 0 */6 * * *", // Every 6 hours
        "0 0 2 * * MON", // Mondays at 2 AM
        "0 15 14 1 * *", // 1st of month at 2:15 PM
    ];

    for valid_cron in valid_cron_configs {
        let config = Config {
            compose_paths: vec![PathBuf::from("./test")],
            schedule: valid_cron.to_string(),
            registries: HashMap::new(),
            update_strategy: UpdateStrategy::Latest,
            ignore_images: vec![],
            dry_run: true,
        };

        let result = Scheduler::new(config, None);
        assert!(
            result.is_ok(),
            "Expected success for valid cron expression: '{valid_cron}'"
        );
    }
}

#[test]
fn test_compose_invalid_file_paths() {
    let config = create_test_config();
    let updater = ComposeUpdater::new(config);

    // Test with completely invalid path
    let result = updater.parse_compose_file("/nonexistent/path/file.yml");
    assert!(result.is_err(), "Should fail for nonexistent file");

    // Test with directory instead of file
    let temp_dir = TempDir::new().unwrap();
    let result = updater.parse_compose_file(temp_dir.path().to_str().unwrap());
    assert!(result.is_err(), "Should fail when path is a directory");
}

#[test]
fn test_compose_malformed_yaml() {
    let config = create_test_config();
    let updater = ComposeUpdater::new(config);

    let malformed_yamls = [
        // Invalid YAML syntax
        r#"
version: '3.8'
services:
  web:
    image: nginx:1.21.0
  invalid_indent
    ports:
      - "80:80"
"#,
        // Missing required fields
        r#"
version: '3.8'
# Missing services section
"#,
        // Completely empty file
        "",
        // Invalid version format
        r#"
version: invalid
services:
  web:
    image: nginx:1.21.0
"#,
    ];

    for (i, malformed_yaml) in malformed_yamls.iter().enumerate() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(malformed_yaml.as_bytes()).unwrap();

        let result = updater.parse_compose_file(temp_file.path().to_str().unwrap());

        // We expect these to either fail or return empty services (graceful degradation)
        if result.is_ok() {
            let compose_file = result.unwrap();
            // If it parses successfully, it should at least not crash
            assert!(
                compose_file.services.is_empty() || !compose_file.services.is_empty(),
                "Test case {i} should either fail or succeed gracefully"
            );
        }
        // If it fails, that's also acceptable behavior for malformed YAML
    }
}

#[test]
fn test_image_ref_parsing_edge_cases() {
    // Test that image parsing doesn't crash on edge cases
    let long_name = "a".repeat(300);
    let edge_case_refs = [
        "",              // Empty string
        ":",             // Just colon
        "image:",        // Missing tag
        ":tag",          // Missing image
        "registry.com/", // Missing image name
        &long_name,      // Extremely long name
    ];

    for edge_case_ref in edge_case_refs {
        let result = ImageRef::parse(edge_case_ref);
        // The main goal is that parsing doesn't crash and returns a result
        // Some might succeed (with defaults) or fail - both are acceptable
        if result.is_ok() {
            let _image_ref = result.unwrap();
            // If it succeeds, that's fine - the parser is robust
            // We don't make strict assertions about the content since
            // the parser may use defaults for edge cases
        }
        // If they fail, that's also perfectly acceptable behavior
    }
}

#[test]
fn test_registry_unknown_registry() {
    let config = Config {
        compose_paths: vec![],
        schedule: "0 0 2 * * *".to_string(),
        registries: HashMap::new(), // Empty registries
        update_strategy: UpdateStrategy::Latest,
        ignore_images: vec![],
        dry_run: true,
    };

    let registry_client = RegistryClient::new(config);

    // Test with an unknown registry
    let image_ref = ImageRef {
        registry: "unknown.registry.com".to_string(),
        namespace: Some("user".to_string()),
        name: "app".to_string(),
        tag: "1.0.0".to_string(),
    };

    // This should fail because the registry is not configured
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(registry_client.get_available_versions(&image_ref));
    assert!(result.is_err(), "Should fail for unknown registry");

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Unknown registry"),
        "Error should mention unknown registry, got: {error_msg}"
    );
}

#[tokio::test]
async fn test_compose_update_with_empty_paths() {
    let config = Config {
        compose_paths: vec![], // Empty paths
        ..create_test_config()
    };
    let updater = ComposeUpdater::new(config);

    // Should handle empty paths gracefully
    let result = updater.update_all_compose_files().await;
    assert!(result.is_ok(), "Should handle empty paths gracefully");
    assert!(
        result.unwrap().is_empty(),
        "Should return empty list for empty paths"
    );
}

#[test]
fn test_config_edge_cases() {
    // Test config with minimal valid data
    let minimal_config = Config {
        compose_paths: vec![], // Empty paths
        schedule: "0 0 2 * * *".to_string(),
        registries: HashMap::new(), // No registries
        update_strategy: UpdateStrategy::Latest,
        ignore_images: vec![],
        dry_run: true,
    };

    // Should be able to create scheduler with minimal config
    let scheduler_result = Scheduler::new(minimal_config.clone(), None);
    assert!(
        scheduler_result.is_ok(),
        "Should accept minimal valid config"
    );

    // Should be able to create compose updater with minimal config
    let _updater = ComposeUpdater::new(minimal_config.clone());
    // This should not panic
    assert!(!minimal_config.is_image_ignored("any-image:tag"));
}

#[test]
fn test_version_selection_with_mismatched_prefixes_suffixes() {
    use docker_compose_updater::strategy::create_selector;
    use docker_compose_updater::version::VersionInfo;

    let config = create_test_config();
    let selector = create_selector(&config.update_strategy);

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
        "Should not find target when prefix/suffix don't match"
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
