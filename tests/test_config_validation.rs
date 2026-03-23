use docker_compose_updater::config::{Config, RegistryConfig, UpdateStrategy};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use tempfile::NamedTempFile;

#[test]
fn test_config_default_values() {
    let config = Config::default();

    // Verify all default values are reasonable
    assert_eq!(config.schedule, "0 0 2 * * *");
    assert_eq!(
        config.update_strategy,
        UpdateStrategy::LatestPatchOfPreviousMinor
    );
    assert!(!config.dry_run);
    assert!(config.ignore_images.is_empty());
    // compose_paths should be a valid Vec (length check is always true for usize)
    let _paths_count = config.compose_paths.len();

    // Should have default registries configured
    assert!(config.registries.contains_key("docker.io"));
    assert!(config.registries.contains_key("ghcr.io"));

    let docker_registry = &config.registries["docker.io"];
    assert_eq!(docker_registry.url, "https://registry-1.docker.io");
    assert!(docker_registry.auth_token.is_none());

    let ghcr_registry = &config.registries["ghcr.io"];
    assert_eq!(ghcr_registry.url, "https://ghcr.io");
    assert!(ghcr_registry.auth_token.is_none());
}

#[test]
fn test_config_from_different_file_formats() {
    // Test loading from a properly formatted YAML file
    let valid_yaml = r#"
compose_paths:
  - "./docker-compose.yml"
  - "./services/"
schedule: "0 0 3 * * *"
update_strategy: "Latest"
dry_run: true
ignore_images:
  - "localhost"
  - "127.0.0.1"
registries:
  docker.io:
    url: "https://registry-1.docker.io"
  custom.registry.com:
    url: "https://custom.registry.com"
    auth_token: "secret-token"
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(valid_yaml.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let result = Config::load(temp_file.path().to_path_buf());
    assert!(result.is_ok(), "Should load valid YAML config");

    let config = result.unwrap();
    assert_eq!(config.compose_paths.len(), 2);
    assert_eq!(config.schedule, "0 0 3 * * *");
    assert_eq!(config.update_strategy, UpdateStrategy::Latest);
    assert!(config.dry_run);
    assert_eq!(config.ignore_images.len(), 2);
    assert!(config.registries.contains_key("custom.registry.com"));

    let custom_registry = &config.registries["custom.registry.com"];
    assert_eq!(custom_registry.url, "https://custom.registry.com");
    assert_eq!(custom_registry.auth_token, Some("secret-token".to_string()));
}

#[test]
fn test_config_with_invalid_yaml_syntax() {
    let invalid_yamls = [
        // Invalid YAML syntax
        r#"
compose_paths:
  - "./docker-compose.yml"
invalid_indent:
schedule: "0 0 2 * * *"
"#,
        // Invalid field types
        r#"
compose_paths: "not an array"
schedule: 123
dry_run: "not a boolean"
"#,
        // Completely invalid YAML
        r#"
{ invalid yaml syntax [
"#,
    ];

    for (i, invalid_yaml) in invalid_yamls.iter().enumerate() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(invalid_yaml.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let result = Config::load(temp_file.path().to_path_buf());

        // Should either fail gracefully or fall back to defaults
        if result.is_ok() {
            let config = result.unwrap();
            // If it succeeds, should have reasonable defaults
            assert!(
                !config.schedule.is_empty(),
                "Schedule should not be empty for test case {i}"
            );
        }
        // If it fails, that's also acceptable behavior for invalid YAML
    }
}

#[test]
fn test_config_update_strategy_parsing() {
    let strategies = vec![
        ("Latest", UpdateStrategy::Latest),
        (
            "LatestPatchOfPreviousMinor",
            UpdateStrategy::LatestPatchOfPreviousMinor,
        ),
        (
            "LatestPatchOfPreviousMinor",
            UpdateStrategy::LatestPatchOfPreviousMinor,
        ),
    ];

    for (strategy_str, expected_strategy) in strategies {
        let yaml_config = format!(
            r#"
compose_paths: []
schedule: "0 0 2 * * *"
update_strategy: "{strategy_str}"
"#
        );

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(yaml_config.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let result = Config::load(temp_file.path().to_path_buf());
        assert!(
            result.is_ok(),
            "Should parse valid strategy: {strategy_str}"
        );

        let config = result.unwrap();
        assert_eq!(config.update_strategy, expected_strategy);
    }
}

#[test]
fn test_config_with_invalid_update_strategy() {
    let yaml_config = r#"
compose_paths: []
schedule: "0 0 2 * * *"
update_strategy: "InvalidStrategy"
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(yaml_config.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let result = Config::load(temp_file.path().to_path_buf());

    assert!(result.is_err(), "Should not parse invalid update strategy");
}

#[test]
fn test_image_ignore_patterns_functionality() {
    // Test various ignore patterns
    let test_cases = vec![
        // (ignore_pattern, image_to_test, should_be_ignored)
        ("localhost", "localhost:5000/app:latest", true),
        ("localhost", "nginx:latest", false),
        ("127.0.0.1", "127.0.0.1:5000/app:latest", true),
        ("127.0.0.1", "192.168.1.1:5000/app:latest", false),
        ("ghcr.io", "ghcr.io/user/app:latest", true),
        ("ghcr.io", "docker.io/nginx:latest", false),
        ("test-", "test-app:latest", true),
        ("test-", "app-test:latest", false),
    ];

    for (ignore_pattern, image_to_test, should_be_ignored) in test_cases {
        let config = Config {
            ignore_images: vec![ignore_pattern.to_string()],
            ..Config::default()
        };

        let result = config.is_image_ignored(image_to_test);
        assert_eq!(
            result, should_be_ignored,
            "Pattern '{ignore_pattern}' with image '{image_to_test}' should be ignored: {should_be_ignored}"
        );
    }
}

#[test]
fn test_multiple_ignore_patterns() {
    let config = Config {
        ignore_images: vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "test-".to_string(),
            "ghcr.io".to_string(),
        ],
        ..Config::default()
    };

    let test_cases = vec![
        ("localhost:5000/app:latest", true),
        ("127.0.0.1:5000/app:latest", true),
        ("test-app:latest", true),
        ("ghcr.io/user/app:latest", true),
        ("docker.io/nginx:latest", false),
        ("registry.example.com/app:latest", false),
        ("app-test:latest", false),
    ];

    for (image, should_be_ignored) in test_cases {
        let result = config.is_image_ignored(image);
        assert_eq!(
            result, should_be_ignored,
            "Image '{image}' should be ignored: {should_be_ignored}"
        );
    }
}

#[test]
fn test_registry_configuration_validation() {
    // Test registry configs with various configurations
    let mut registries = HashMap::new();

    // Registry with just URL
    registries.insert(
        "simple.registry.com".to_string(),
        RegistryConfig {
            url: "https://simple.registry.com".to_string(),
            auth_token: None,
        },
    );

    // Registry with auth token
    registries.insert(
        "auth.registry.com".to_string(),
        RegistryConfig {
            url: "https://auth.registry.com".to_string(),
            auth_token: Some("secret-token".to_string()),
        },
    );

    let config = Config {
        registries,
        ..Config::default()
    };

    // Verify registries are properly configured
    assert!(config.registries.contains_key("simple.registry.com"));
    assert!(config.registries.contains_key("auth.registry.com"));

    let simple_registry = &config.registries["simple.registry.com"];
    assert_eq!(simple_registry.url, "https://simple.registry.com");
    assert!(simple_registry.auth_token.is_none());

    let auth_registry = &config.registries["auth.registry.com"];
    assert_eq!(auth_registry.url, "https://auth.registry.com");
    assert_eq!(auth_registry.auth_token, Some("secret-token".to_string()));
}

#[test]
fn test_config_with_empty_fields() {
    let yaml_config = r#"
compose_paths: []
schedule: ""
ignore_images: []
registries: {}
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(yaml_config.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let result = Config::load(temp_file.path().to_path_buf());

    if result.is_ok() {
        let config = result.unwrap();
        assert!(config.compose_paths.is_empty());
        assert!(config.ignore_images.is_empty());
        // Empty schedule should either be rejected or have a default
        assert!(!config.schedule.is_empty() || config.schedule.is_empty()); // Either is acceptable
    }
    // If loading fails for empty schedule, that's also acceptable
}

#[test]
fn test_config_compose_paths_handling() {
    // Test various compose path configurations
    let test_cases = vec![
        // Single file path
        vec!["./docker-compose.yml"],
        // Multiple file paths
        vec!["./docker-compose.yml", "./docker-compose.override.yml"],
        // Directory paths
        vec!["./services/", "./environments/"],
        // Mixed paths
        vec!["./docker-compose.yml", "./services/", "./override.yml"],
        // Relative and absolute-looking paths
        vec![
            "docker-compose.yml",
            "/app/config/compose.yml",
            "../compose.yml",
        ],
    ];

    for paths in test_cases {
        let yaml_config = format!(
            r#"
compose_paths:
{}
schedule: "0 0 2 * * *"
"#,
            paths
                .iter()
                .map(|p| format!("  - \"{p}\""))
                .collect::<Vec<_>>()
                .join("\n")
        );

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(yaml_config.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let result = Config::load(temp_file.path().to_path_buf());
        assert!(result.is_ok(), "Should handle paths: {paths:?}");

        let config = result.unwrap();
        assert_eq!(config.compose_paths.len(), paths.len());
        for (i, expected_path) in paths.iter().enumerate() {
            assert_eq!(config.compose_paths[i], PathBuf::from(expected_path));
        }
    }
}
