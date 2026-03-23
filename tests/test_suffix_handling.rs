use docker_compose_updater::compose::updater::ComposeUpdater;
use docker_compose_updater::config::{Config, UpdateStrategy};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use tempfile::NamedTempFile;

#[tokio::test]
async fn test_compose_update_with_suffix_preservation() {
    let compose_content = r#"
version: '3.8'
services:
  web:
    image: nginx:1.21.0-alpine
    ports:
      - "80:80"
"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(compose_content.as_bytes()).unwrap();

    let config = Config {
        compose_paths: vec![PathBuf::from(temp_file.path().to_str().unwrap())],
        schedule: "0 0 2 * * *".to_string(),
        registries: HashMap::new(),
        update_strategy: UpdateStrategy::Latest,
        ignore_images: vec![],
        dry_run: true,
    };

    let updater = ComposeUpdater::new(config);
    let result = updater
        .parse_compose_file(temp_file.path().to_str().unwrap())
        .unwrap();

    assert_eq!(result.services[0].image_ref.tag, "1.21.0-alpine");

    let new_content = updater
        .replace_image_in_content(&result.content, &result.services[0], "nginx:1.22.0-alpine")
        .unwrap();

    assert!(new_content.contains("nginx:1.22.0-alpine"));
    assert!(!new_content.contains("nginx:1.21.0-alpine"));
}
