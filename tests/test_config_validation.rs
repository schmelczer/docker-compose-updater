use docker_compose_updater::config::{Config, RegistryConfig, UpdateStrategy};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use tempfile::NamedTempFile;

#[test]
fn test_config_default_values() {
    let config = Config::default();

    assert_eq!(config.schedule, "0 0 2 * * *");
    assert_eq!(
        config.update_strategy,
        UpdateStrategy::LatestPatchOfPreviousMinor
    );
    assert!(!config.dry_run);
    assert!(config.ignore_images.is_empty());
    assert!(!config.compose_paths.is_empty());

    assert!(config.registries.contains_key("docker.io"));
    assert!(config.registries.contains_key("ghcr.io"));

    let docker_registry = &config.registries["docker.io"];
    assert_eq!(docker_registry.url, "https://registry-1.docker.io");
    assert!(docker_registry.auth_token.is_none());

    let ghcr_registry = &config.registries["ghcr.io"];
    assert_eq!(ghcr_registry.url, "https://ghcr.io");
}

#[test]
fn test_config_from_yaml_file() {
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

    let config = Config::load(temp_file.path().to_path_buf()).unwrap();
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
fn test_config_rejects_invalid_yaml() {
    let invalid_yaml = r#"{ invalid yaml syntax ["#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(invalid_yaml.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let result = Config::load(temp_file.path().to_path_buf());
    assert!(
        result.is_err(),
        "Completely invalid YAML should fail to load"
    );
}

#[test]
fn test_config_update_strategy_parsing() {
    let strategies = vec![
        ("Latest", UpdateStrategy::Latest),
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

        let config = Config::load(temp_file.path().to_path_buf()).unwrap();
        assert_eq!(config.update_strategy, expected_strategy);
    }
}

#[test]
fn test_config_rejects_invalid_update_strategy() {
    let yaml_config = r#"
compose_paths: []
schedule: "0 0 2 * * *"
update_strategy: "InvalidStrategy"
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(yaml_config.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let result = Config::load(temp_file.path().to_path_buf());
    assert!(result.is_err());
}

#[test]
fn test_image_ignore_patterns() {
    let test_cases = vec![
        ("localhost", "localhost:5000/app:latest", true),
        ("localhost", "nginx:latest", false),
        ("127.0.0.1", "127.0.0.1:5000/app:latest", true),
        ("127.0.0.1", "192.168.1.1:5000/app:latest", false),
        ("ghcr.io", "ghcr.io/user/app:latest", true),
        ("ghcr.io", "docker.io/nginx:latest", false),
        ("test-", "test-app:latest", true),
        // "app-test:latest" does NOT contain "test-" (it has "test:" after the dash)
        ("test-", "app-test:latest", false),
    ];

    for (ignore_pattern, image_to_test, should_be_ignored) in test_cases {
        let config = Config {
            ignore_images: vec![ignore_pattern.to_string()],
            ..Config::default()
        };

        assert_eq!(
            config.is_image_ignored(image_to_test),
            should_be_ignored,
            "Pattern '{}' with image '{}': expected ignored={}",
            ignore_pattern,
            image_to_test,
            should_be_ignored
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

    assert!(config.is_image_ignored("localhost:5000/app:latest"));
    assert!(config.is_image_ignored("127.0.0.1:5000/app:latest"));
    assert!(config.is_image_ignored("test-app:latest"));
    assert!(config.is_image_ignored("ghcr.io/user/app:latest"));
    assert!(!config.is_image_ignored("docker.io/nginx:latest"));
    assert!(!config.is_image_ignored("registry.example.com/app:latest"));
}

#[test]
fn test_registry_configuration() {
    let mut registries = HashMap::new();
    registries.insert(
        "simple.registry.com".to_string(),
        RegistryConfig {
            url: "https://simple.registry.com".to_string(),
            auth_token: None,
            username: None,
        },
    );
    registries.insert(
        "auth.registry.com".to_string(),
        RegistryConfig {
            url: "https://auth.registry.com".to_string(),
            auth_token: Some("secret-token".to_string()),
            username: None,
        },
    );

    let config = Config {
        registries,
        ..Config::default()
    };

    assert!(config.registries["simple.registry.com"]
        .auth_token
        .is_none());
    assert_eq!(
        config.registries["auth.registry.com"].auth_token,
        Some("secret-token".to_string())
    );
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

    let config = Config::load(temp_file.path().to_path_buf()).unwrap();
    assert!(config.compose_paths.is_empty());
    assert!(config.ignore_images.is_empty());
    assert!(config.schedule.is_empty());
    // The other fields stay as written, but the default registries are restored:
    // resolving an image now goes through its registry entry, so an empty map
    // would make every service fail with "Unknown registry" rather than express
    // any useful intent.
    assert!(config.registries.contains_key("docker.io"));
}

#[test]
fn test_config_compose_paths_handling() {
    let test_cases = vec![
        vec!["./docker-compose.yml"],
        vec!["./docker-compose.yml", "./docker-compose.override.yml"],
        vec!["./services/", "./environments/"],
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

        let config = Config::load(temp_file.path().to_path_buf()).unwrap();
        assert_eq!(config.compose_paths.len(), paths.len());
        for (i, expected_path) in paths.iter().enumerate() {
            assert_eq!(config.compose_paths[i], PathBuf::from(expected_path));
        }
    }
}

#[test]
fn test_load_normalizes_empty_auth_token_to_none() {
    // An auth_token referencing an unset variable expands to "" and should be
    // treated as "no token" rather than an empty credential.
    let yaml_config = r#"
compose_paths: []
schedule: "0 0 2 * * *"
registries:
  private.registry.com:
    url: "https://private.registry.com"
    auth_token: "${DCU_DEFINITELY_UNSET_TOKEN}"
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(yaml_config.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let config = Config::load(temp_file.path().to_path_buf()).unwrap();
    assert!(config.registries["private.registry.com"]
        .auth_token
        .is_none());
}

#[test]
fn test_load_restores_default_registries_omitted_by_the_config_file() {
    // A `registries` block replaces the defaults instead of merging, so a file
    // listing only a private registry must still be able to resolve Docker Hub:
    // every `nginx:1.2.3`-style image in the compose files depends on it.
    let yaml_config = r#"
compose_paths: []
schedule: "0 0 2 * * *"
registries:
  private.registry.com:
    url: "https://private.registry.com"
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(yaml_config.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let config = Config::load(temp_file.path().to_path_buf()).unwrap();
    assert_eq!(
        config.registries["docker.io"].url,
        "https://registry-1.docker.io"
    );
    assert!(config.registries.contains_key("private.registry.com"));
}

#[test]
fn test_load_keeps_explicit_registry_overrides() {
    let yaml_config = r#"
compose_paths: []
schedule: "0 0 2 * * *"
registries:
  docker.io:
    url: "https://mirror.internal"
    username: "andras"
    auth_token: "secret"
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(yaml_config.as_bytes()).unwrap();
    temp_file.flush().unwrap();

    let config = Config::load(temp_file.path().to_path_buf()).unwrap();
    let docker_io = &config.registries["docker.io"];
    assert_eq!(docker_io.url, "https://mirror.internal");
    assert_eq!(docker_io.username.as_deref(), Some("andras"));
}
