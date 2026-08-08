use docker_compose_updater::compose::updater::ComposeUpdater;
use docker_compose_updater::config::{Config, RegistryConfig, UpdateStrategy};
use docker_compose_updater::registry::ImageRef;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use tempfile::NamedTempFile;

#[test]
fn test_image_ref_comprehensive_parsing() {
    // Test comprehensive image reference parsing scenarios
    let test_cases = vec![
        // (input, expected_registry, expected_namespace, expected_name, expected_tag)

        // Standard Docker Hub images
        ("nginx", "docker.io", None, "nginx", "latest"),
        ("nginx:1.21", "docker.io", None, "nginx", "1.21"),
        ("nginx:latest", "docker.io", None, "nginx", "latest"),
        // Docker Hub with namespace
        (
            "library/nginx:1.21",
            "docker.io",
            Some("library"),
            "nginx",
            "1.21",
        ),
        (
            "bitnami/nginx:1.21",
            "docker.io",
            Some("bitnami"),
            "nginx",
            "1.21",
        ),
        // Custom registries
        ("quay.io/nginx:1.21", "quay.io", None, "nginx", "1.21"),
        (
            "gcr.io/project/nginx:1.21",
            "gcr.io",
            Some("project"),
            "nginx",
            "1.21",
        ),
        (
            "registry.k8s.io/pause:3.9",
            "registry.k8s.io",
            None,
            "pause",
            "3.9",
        ),
        // GitHub Container Registry
        (
            "ghcr.io/user/app:v1.0.0",
            "ghcr.io",
            Some("user"),
            "app",
            "v1.0.0",
        ),
        (
            "ghcr.io/org/project:latest",
            "ghcr.io",
            Some("org"),
            "project",
            "latest",
        ),
        // Local registries with ports
        (
            "localhost:5000/app:dev",
            "localhost:5000",
            None,
            "app",
            "dev",
        ),
        (
            "127.0.0.1:8080/ns/app:test",
            "127.0.0.1:8080",
            Some("ns"),
            "app",
            "test",
        ),
        // Complex tags with versions and suffixes
        (
            "nginx:1.21.6-alpine",
            "docker.io",
            None,
            "nginx",
            "1.21.6-alpine",
        ),
        (
            "postgres:13.7-bullseye",
            "docker.io",
            None,
            "postgres",
            "13.7-bullseye",
        ),
        (
            "redis:7.0.0-alpine3.16",
            "docker.io",
            None,
            "redis",
            "7.0.0-alpine3.16",
        ),
        // Enterprise/custom registries
        (
            "my-registry.company.com/team/app:v2.1.3",
            "my-registry.company.com",
            Some("team"),
            "app",
            "v2.1.3",
        ),
        (
            "harbor.example.org/project/nginx:stable",
            "harbor.example.org",
            Some("project"),
            "nginx",
            "stable",
        ),
    ];

    for (input, expected_registry, expected_namespace, expected_name, expected_tag) in test_cases {
        let result = ImageRef::parse(input);
        assert!(result.is_ok(), "Failed to parse: {input}");

        let image_ref = result.unwrap();
        assert_eq!(
            image_ref.registry, expected_registry,
            "Registry mismatch for: {input}"
        );
        assert_eq!(
            image_ref.namespace,
            expected_namespace.map(String::from),
            "Namespace mismatch for: {input}"
        );
        assert_eq!(image_ref.name, expected_name, "Name mismatch for: {input}");
        assert_eq!(image_ref.tag, expected_tag, "Tag mismatch for: {input}");

        // Test that the image ref can be converted back to string representation
        let display_string = image_ref.to_string();
        assert!(
            !display_string.is_empty(),
            "Display string should not be empty for: {input}"
        );

        // For Docker Hub images, the display should omit the registry
        if expected_registry == "docker.io" {
            assert!(
                !display_string.starts_with("docker.io"),
                "Docker Hub images should not show registry in display: {input}"
            );
        }
    }
}

#[tokio::test]
async fn test_compose_file_parsing_edge_cases() {
    let config = create_test_config();
    let updater = ComposeUpdater::new(config);

    // Test various compose file formats and edge cases
    let test_cases = vec![
        // Standard compose file
        (
            "standard_compose",
            r#"
version: '3.8'
services:
  web:
    image: nginx:1.21.0
    ports:
      - "80:80"
  db:
    image: postgres:13.7
"#,
            2, // Expected number of services
        ),
        // Compose file with comments and whitespace
        (
            "with_comments",
            r#"
# This is a comment
version: '3.8'

services:
  # Web service comment
  web:
    image: nginx:1.21.0  # Inline comment
    ports:
      - "80:80"
  
  # Database service
  db:
    image: postgres:13.7
    # More comments
    environment:
      POSTGRES_DB: test
"#,
            2,
        ),
        // Compose file with complex image references
        (
            "complex_images",
            r#"
version: '3.8'
services:
  frontend:
    image: ghcr.io/company/frontend:v2.1.0
  backend:
    image: my-registry.com:5000/team/backend:latest
  cache:
    image: redis:7.0.0-alpine3.16
  database:
    image: postgres:14.5-bullseye
"#,
            4,
        ),
        // Minimal compose file
        (
            "minimal",
            r#"
version: '3'
services:
  app:
    image: hello-world
"#,
            1,
        ),
        // Compose file with extends and other features
        (
            "with_extends",
            r#"
version: '3.8'
services:
  base:
    image: ubuntu:20.04
    environment:
      - ENV=production
      
  app:
    extends:
      service: base
    image: myapp:latest
    ports:
      - "3000:3000"
"#,
            2,
        ),
    ];

    for (test_name, compose_content, expected_services) in test_cases {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(compose_content.as_bytes()).unwrap();

        let result = updater.parse_compose_file(temp_file.path().to_str().unwrap());
        assert!(result.is_ok(), "Failed to parse compose file: {test_name}");

        let compose_file = result.unwrap();
        assert_eq!(
            compose_file.services.len(),
            expected_services,
            "Service count mismatch for: {test_name}"
        );

        // Verify that all services have valid image references
        for service in &compose_file.services {
            assert!(
                !service.service_name.is_empty(),
                "Service name should not be empty in: {test_name}"
            );
            assert!(
                !service.image_ref.name.is_empty(),
                "Image name should not be empty in: {test_name}"
            );
            assert!(
                !service.image_ref.tag.is_empty(),
                "Image tag should not be empty in: {test_name}"
            );
            assert!(
                !service.image_ref.registry.is_empty(),
                "Image registry should not be empty in: {test_name}"
            );
        }
    }
}

#[tokio::test]
async fn test_compose_content_replacement() {
    let config = create_test_config();
    let updater = ComposeUpdater::new(config);

    let original_content = r#"
version: '3.8'
services:
  web:
    image: nginx:1.20.0  # Current version
    ports:
      - "80:80"
  db:
    image: postgres:13.6
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(original_content.as_bytes()).unwrap();

    let compose_file = updater
        .parse_compose_file(temp_file.path().to_str().unwrap())
        .unwrap();

    // Test replacing nginx image
    let nginx_service = compose_file
        .services
        .iter()
        .find(|s| s.service_name == "web")
        .unwrap();

    let result =
        updater.replace_image_in_content(&compose_file.content, nginx_service, "nginx:1.21.0");
    assert!(result.is_ok(), "Should successfully replace image");

    let new_content = result.unwrap();
    assert!(
        new_content.contains("nginx:1.21.0"),
        "Should contain new image version"
    );
    assert!(
        !new_content.contains("nginx:1.20.0"),
        "Should not contain old image version"
    );
    assert!(
        new_content.contains("# Current version"),
        "Should preserve comments"
    );
    assert!(
        new_content.contains("postgres:13.6"),
        "Should preserve other services unchanged"
    );
}

#[test]
fn test_compose_service_image_replacement_edge_cases() {
    let config = create_test_config();
    let updater = ComposeUpdater::new(config);

    // Test cases with tricky image replacement scenarios
    let test_cases = vec![
        // Multiple occurrences of similar image names
        (
            r#"
version: '3.8'
services:
  nginx-proxy:
    image: nginx:1.20.0
  nginx-cache:
    image: nginx:1.21.0
  web:
    image: nginx:1.20.0
"#,
            "nginx-proxy",
            "nginx:1.20.0",
            "nginx:1.22.0",
        ),
        // Images with similar names but different registries
        (
            r#"
version: '3.8'
services:
  public-nginx:
    image: nginx:1.20.0
  private-nginx:
    image: my-registry.com/nginx:1.20.0
"#,
            "public-nginx",
            "nginx:1.20.0",
            "nginx:1.21.0",
        ),
        // Complex image with registry and namespace
        (
            r#"
version: '3.8'
services:
  app:
    image: ghcr.io/company/app:v1.0.0
    environment:
      - IMAGE_VERSION=v1.0.0
"#,
            "app",
            "ghcr.io/company/app:v1.0.0",
            "ghcr.io/company/app:v1.1.0",
        ),
    ];

    for (compose_content, service_name, old_image, new_image) in test_cases {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(compose_content.as_bytes()).unwrap();

        let compose_file = updater
            .parse_compose_file(temp_file.path().to_str().unwrap())
            .unwrap();

        let target_service = compose_file
            .services
            .iter()
            .find(|s| s.service_name == service_name)
            .unwrap();

        let result =
            updater.replace_image_in_content(&compose_file.content, target_service, new_image);
        assert!(
            result.is_ok(),
            "Should replace image for service: {service_name}"
        );

        let new_content = result.unwrap();
        assert!(
            new_content.contains(new_image),
            "Should contain new image: {new_image}"
        );

        // Count occurrences to ensure only the right image was replaced
        let old_count = compose_content.matches(old_image).count();
        let new_old_count = new_content.matches(old_image).count();
        assert_eq!(
            new_old_count,
            old_count - 1,
            "Should replace exactly one occurrence for: {service_name}"
        );
    }
}

#[test]
fn test_image_ignore_patterns_comprehensive() {
    // Test comprehensive ignore pattern scenarios
    let test_cases = vec![
        // Basic pattern matching
        (vec!["localhost"], "localhost:5000/app:latest", true),
        (vec!["localhost"], "nginx:latest", false),
        // Multiple patterns
        (
            vec!["localhost", "127.0.0.1"],
            "127.0.0.1:5000/app:latest",
            true,
        ),
        (
            vec!["localhost", "127.0.0.1"],
            "192.168.1.1:5000/app:latest",
            false,
        ),
        // Registry-based patterns
        (vec!["ghcr.io"], "ghcr.io/user/app:latest", true),
        (vec!["ghcr.io"], "docker.io/user/app:latest", false),
        // Prefix patterns
        (vec!["test-"], "test-app:latest", true),
        (vec!["test-"], "app-test:latest", false),
        (vec!["test-"], "testing:latest", false),
        // Complex registry patterns
        (
            vec!["internal.company.com"],
            "internal.company.com/team/app:v1.0",
            true,
        ),
        (
            vec!["internal.company.com"],
            "external.company.com/team/app:v1.0",
            false,
        ),
        // Partial matches
        (vec!["redis"], "redis:latest", true),
        (vec!["redis"], "valkey:latest", false),
        (vec!["redis"], "my-redis:latest", true), // This should match as substring
    ];

    for (ignore_patterns, image, should_be_ignored) in test_cases {
        let config = Config {
            ignore_images: ignore_patterns.iter().map(|s| s.to_string()).collect(),
            ..create_test_config()
        };

        let result = config.is_image_ignored(image);
        assert_eq!(
            result, should_be_ignored,
            "Pattern {ignore_patterns:?} with image '{image}' should be ignored: {should_be_ignored}"
        );
    }
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
    registries.insert(
        "ghcr.io".to_string(),
        RegistryConfig {
            url: "https://ghcr.io".to_string(),
            auth_token: None,
            username: None,
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
