use docker_compose_updater::compose::updater::ComposeUpdater;
use docker_compose_updater::config::{Config, RegistryConfig, UpdateStrategy};
use docker_compose_updater::registry::{Client, ImageRef};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use tempfile::NamedTempFile;

#[tokio::test]
async fn test_compose_file_parsing() {
    let compose_content = r#"
version: '3.8'
services:
  web:
    image: nginx:1.21.0  # Web server
    ports:
      - "80:80"

  db:
    image: postgres:13.7
    environment:
      POSTGRES_DB: myapp

  redis:
    image: redis:6.2.1-alpine
    ports:
      - "6379:6379"
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(compose_content.as_bytes()).unwrap();

    let updater = ComposeUpdater::new(create_test_config());
    let result = updater
        .parse_compose_file(temp_file.path().to_str().unwrap())
        .unwrap();

    assert_eq!(result.services.len(), 3);

    assert_eq!(result.services[0].service_name, "web");
    assert_eq!(result.services[0].image_ref.name, "nginx");
    assert_eq!(result.services[0].image_ref.tag, "1.21.0");
    assert_eq!(result.services[0].image_ref.registry, "docker.io");

    assert_eq!(result.services[1].service_name, "db");
    assert_eq!(result.services[1].image_ref.name, "postgres");
    assert_eq!(result.services[1].image_ref.tag, "13.7");

    assert_eq!(result.services[2].service_name, "redis");
    assert_eq!(result.services[2].image_ref.name, "redis");
    assert_eq!(result.services[2].image_ref.tag, "6.2.1-alpine");
}

#[tokio::test]
async fn test_comment_and_formatting_preservation() {
    let compose_content = r#"
version: '3.8'
services:
  web:
    image: nginx:1.21.0  # This is a comment
    ports:
      - "80:80"

  # This is another comment
  db:
    image: "postgres:13.7"  # Database comment with quotes
    environment:
      POSTGRES_DB: myapp
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(compose_content.as_bytes()).unwrap();

    let updater = ComposeUpdater::new(create_test_config());
    let result = updater
        .parse_compose_file(temp_file.path().to_str().unwrap())
        .unwrap();

    assert!(result.services[0]
        .original_line
        .contains("# This is a comment"));
    assert!(result.services[1]
        .original_line
        .contains("# Database comment"));

    // Replace unquoted image -> stays unquoted, preserves comment
    let new_content = updater
        .replace_image_in_content(&result.content, &result.services[0], "nginx:1.20.0")
        .unwrap();

    assert!(new_content.contains("nginx:1.20.0  # This is a comment"));
    assert!(!new_content.contains("nginx:1.21.0"));

    // Replace quoted image -> stays quoted, preserves comment
    let new_content = updater
        .replace_image_in_content(&new_content, &result.services[1], "postgres:14.0")
        .unwrap();

    assert!(new_content.contains("\"postgres:14.0\"  # Database comment"));
    assert!(!new_content.contains("postgres:13.7"));
}

#[tokio::test]
async fn test_empty_compose_paths() {
    let config = Config {
        compose_paths: vec![],
        ..create_test_config()
    };
    let updater = ComposeUpdater::new(config);

    let result = updater.update_all_compose_files().await.unwrap();
    assert_eq!(result.files_found, 0);
    assert!(result.updated_files.is_empty());
}

#[test]
fn test_config_defaults_and_image_filtering() {
    let config = Config::default();

    assert_eq!(config.schedule, "0 0 2 * * *");
    assert_eq!(
        config.update_strategy,
        UpdateStrategy::LatestPatchOfPreviousMinor
    );
    assert!(!config.dry_run);
    assert!(config.registries.contains_key("docker.io"));
    assert!(config.registries.contains_key("ghcr.io"));

    let config_with_ignores = Config {
        ignore_images: vec!["localhost".to_string(), "127.0.0.1".to_string()],
        ..Config::default()
    };

    assert!(config_with_ignores.is_image_ignored("localhost:5000/myapp:latest"));
    assert!(config_with_ignores.is_image_ignored("127.0.0.1:5000/myapp:latest"));
    assert!(!config_with_ignores.is_image_ignored("nginx:1.21.0"));
}

#[test]
fn test_env_var_substitution() {
    std::env::set_var("TEST_TOKEN", "test_value_123");

    let test_yaml = r#"
registries:
  "test.registry":
    url: "https://test.registry"
    auth_token: "${TEST_TOKEN}"
"#;

    let expanded = Config::expand_env_vars(test_yaml);
    assert!(expanded.contains("test_value_123"));
    assert!(!expanded.contains("${TEST_TOKEN}"));

    std::env::remove_var("TEST_TOKEN");
}

#[test]
fn test_dollar_var_and_unset_var_expansion() {
    std::env::set_var("TEST_DOLLAR_TOKEN", "dollar_value_456");
    let expanded = Config::expand_env_vars("token: $TEST_DOLLAR_TOKEN");
    assert!(expanded.contains("dollar_value_456"));
    std::env::remove_var("TEST_DOLLAR_TOKEN");

    let expanded = Config::expand_env_vars("token: ${DEFINITELY_UNSET_VAR}");
    assert!(!expanded.contains("DEFINITELY_UNSET_VAR"));
    assert!(expanded.contains("token: "));
}

#[tokio::test]
#[ignore = "Only run in CI with GITHUB_TOKEN set"]
async fn test_ghcr_authentication_e2e() {
    let github_token = std::env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN must be set");

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
            auth_token: Some(github_token),
            username: None,
        },
    );

    let config = Config {
        compose_paths: vec![PathBuf::from("./test")],
        schedule: "0 0 2 * * *".to_string(),
        registries,
        update_strategy: UpdateStrategy::LatestPatchOfPreviousMinor,
        ignore_images: vec!["localhost".to_string()],
        dry_run: true,
    };

    let client = Client::new(config);
    let image_ref = ImageRef::parse("ghcr.io/schmelczer/vault-link:0.5.1").unwrap();

    let versions = client.get_available_versions(&image_ref).await.unwrap();
    assert!(!versions.is_empty());
    assert!(
        versions.iter().any(|v| v.version.to_string() == "0.5.1"),
        "Should find version 0.5.1"
    );
}

fn create_test_config() -> Config {
    let mut registries = HashMap::new();
    registries.insert(
        "docker.io".to_string(),
        RegistryConfig {
            url: "https://registry-1.docker.io".to_string(),
            auth_token: None,
            username: None,
        },
    );

    Config {
        compose_paths: vec![PathBuf::from("./test")],
        schedule: "0 0 2 * * *".to_string(),
        registries,
        update_strategy: UpdateStrategy::LatestPatchOfPreviousMinor,
        ignore_images: vec!["localhost".to_string()],
        dry_run: true,
    }
}
