use crate::registry::ImageRef;
use anyhow::{ensure, Context, Result};
use regex::Regex;
use serde_yaml::Value;
use std::collections::HashMap;
use std::fs;
use std::sync::LazyLock;
use tracing::{info, warn};

static IMAGE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*image:\s*(?:["']([^"']+)["']|([^\s#]+))\s*(#.*)?$"#).unwrap()
});

// Recognize a block mapping header without discarding quoted keys, anchors,
// trailing whitespace or comments. Decode the key with YAML rather than treating
// its quotes or escapes as part of the service name.
static MAPPING_HEADER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^("(?:[^"\\]|\\.)*"|'(?:[^']|'')*'|[^:\s][^:]*?)[ \t]*:[ \t]*(?:&[^\s]+[ \t]*)?(?:#.*)?$"#,
    )
    .unwrap()
});

fn mapping_header_key(line: &str) -> Option<String> {
    let captures = MAPPING_HEADER_REGEX.captures(line)?;
    serde_yaml::from_str(captures.get(1)?.as_str().trim_end()).ok()
}

#[derive(Debug, Clone)]
pub struct ServiceImage {
    pub service_name: String,
    pub image_ref: ImageRef,
    pub original_line: String,
    pub line_number: usize,
}

pub struct ComposeFile {
    pub content: String,
    pub services: Vec<ServiceImage>,
}

pub struct ComposeParser;

impl ComposeParser {
    pub fn new() -> Self {
        Self
    }

    pub fn parse_file(&self, file_path: &str) -> Result<ComposeFile> {
        let content = fs::read_to_string(file_path)?;
        let services = self.extract_services(&content)?;
        Ok(ComposeFile { content, services })
    }

    fn extract_services(&self, content: &str) -> Result<Vec<ServiceImage>> {
        // YAML supplies the expected services, while the line scan supplies the
        // source locations needed for edits that preserve formatting. Reconcile
        // the two so unsupported layouts cannot silently lose services.
        let mut document: Value = serde_yaml::from_str(content).context("Invalid Compose YAML")?;
        document
            .apply_merge()
            .context("Invalid Compose YAML merge")?;
        let mut expected_images = HashMap::new();
        if let Some(declarations) = document.get("services").filter(|value| !value.is_null()) {
            let declarations = declarations
                .as_mapping()
                .context("Compose services must be a mapping")?;
            for (name, declaration) in declarations {
                let name = name
                    .as_str()
                    .context("Compose service names must be strings")?;
                if let Some(image) = declaration.get("image").and_then(Value::as_str) {
                    if !image.trim().is_empty() && !image.contains('@') {
                        if let Ok(image_ref) = ImageRef::parse(image) {
                            expected_images.insert(name.to_string(), image_ref.to_string());
                        }
                    }
                }
            }
        }

        let mut services = Vec::new();
        let lines: Vec<&str> = content.lines().collect();
        let mut in_services = false;
        let mut current_service: Option<String> = None;
        let mut service_indent: Option<usize> = None;
        let mut service_child_indent: Option<usize> = None;

        for (line_number, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();

            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let indent = line.len() - trimmed.len();

            if indent == 0 && mapping_header_key(trimmed).as_deref() == Some("services") {
                in_services = true;
                current_service = None;
                service_indent = None;
                service_child_indent = None;
                continue;
            }

            if in_services && indent == 0 {
                in_services = false;
                current_service = None;
                service_indent = None;
                service_child_indent = None;
                continue;
            }

            if !in_services {
                continue;
            }

            if service_indent.is_none() || service_indent == Some(indent) {
                service_indent = Some(indent);
                service_child_indent = None;
                // Always reset at a service boundary, even if this header is an
                // unsupported layout, so its image cannot belong to its sibling.
                current_service = mapping_header_key(trimmed);
                continue;
            }

            if let Some(ref service_name) = current_service {
                if let Some(si) = service_indent {
                    if indent > si {
                        // The first key under a service fixes the direct-child
                        // indent; only keys at that exact level are the service's
                        // own properties. This avoids picking up an `image:` nested
                        // under `build:` or other deeper mappings.
                        let child_indent = *service_child_indent.get_or_insert(indent);
                        if indent == child_indent {
                            if let Some(image_ref) = self.extract_image_from_line(line)? {
                                info!(
                                    "Found service '{}' with image '{}'",
                                    service_name, image_ref
                                );
                                services.push(ServiceImage {
                                    service_name: service_name.clone(),
                                    image_ref,
                                    original_line: line.to_string(),
                                    line_number,
                                });
                            }
                        }
                    }
                }
            }
        }

        for (name, image) in &expected_images {
            ensure!(
                services
                    .iter()
                    .filter(|service| service.service_name == *name
                        && service.image_ref.to_string() == *image)
                    .count()
                    == 1,
                "Cannot safely locate image for service '{}'; use a block service mapping with a direct, single-line image property (flow mappings and inherited images are unsupported)",
                name
            );
        }
        ensure!(
            services.len() == expected_images.len(),
            "Image source locations do not match the Compose YAML services"
        );

        Ok(services)
    }

    fn extract_image_from_line(&self, line: &str) -> Result<Option<ImageRef>> {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("image:") {
            return Ok(None);
        }

        let Some(captures) = IMAGE_REGEX.captures(line) else {
            return Ok(None);
        };

        let image_str = captures
            .get(1)
            .or_else(|| captures.get(2))
            .unwrap()
            .as_str();

        if image_str.contains('@') {
            info!("Skipping digest-pinned image: {}", image_str);
            return Ok(None);
        }

        if image_str.trim().is_empty() {
            return Ok(None);
        }

        match ImageRef::parse(image_str) {
            Ok(image_ref) => Ok(Some(image_ref)),
            Err(e) => {
                warn!("Failed to parse image '{}': {}", image_str, e);
                Ok(None)
            }
        }
    }
}

impl Default for ComposeParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_yaml_service_declarations() {
        for header in [
            "web: # Web server",
            "web:   ",
            "\"web\":",
            "'web': # Quoted name",
            "web: &web_config",
            "\"web\": &web_config # Anchored service",
        ] {
            let content = format!(
                "services:\n  first:\n    image: redis:7.0.0\n  {header}\n    build:\n      image: nested:9.9.9\n    image: nginx:1.21.0\n"
            );
            let services = ComposeParser::new().extract_services(&content).unwrap();

            assert_eq!(services.len(), 2, "header: {header}");
            assert_eq!(services[0].service_name, "first");
            assert_eq!(services[1].service_name, "web", "header: {header}");
            assert_eq!(services[1].image_ref.to_string(), "nginx:1.21.0");
            assert_eq!(services[1].line_number, 6);
            assert_eq!(services[1].original_line, "    image: nginx:1.21.0");
        }
    }

    #[test]
    fn test_quoted_and_anchored_services_mapping() {
        let content = "'services': &services # Services\n  'web':\n    image: nginx:1.21.0\n";
        let services = ComposeParser::new().extract_services(content).unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].service_name, "web");
    }

    #[test]
    fn test_unsupported_image_locations_are_reported() {
        for content in [
            "services:\n  web: {image: 'nginx:1.21.0'}\n",
            "services: {web: {image: 'nginx:1.21.0'}}\n",
            "x-base: &base\n  image: nginx:1.21.0\nservices:\n  web: *base\n",
            "x-base: &base\n  image: nginx:1.21.0\nservices:\n  web:\n    <<: *base\n",
        ] {
            let error = ComposeParser::new()
                .extract_services(content)
                .expect_err("an image that cannot be safely located must produce an error");
            assert!(error.to_string().contains("web"), "{error}");
        }
    }

    #[test]
    fn test_parse_compose_file() {
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
    image: redis:6.2-alpine
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(compose_content.as_bytes()).unwrap();

        let parser = ComposeParser::new();
        let result = parser
            .parse_file(temp_file.path().to_str().unwrap())
            .unwrap();

        assert_eq!(result.services.len(), 3);
        assert_eq!(result.services[0].service_name, "web");
        assert_eq!(result.services[0].image_ref.name, "nginx");
        assert_eq!(result.services[0].image_ref.tag, "1.21.0");
    }

    #[test]
    fn test_service_names_with_dots() {
        let compose_content = r#"
services:
  my.service:
    image: nginx:1.21.0
  another-service:
    image: redis:7.0
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(compose_content.as_bytes()).unwrap();

        let parser = ComposeParser::new();
        let result = parser
            .parse_file(temp_file.path().to_str().unwrap())
            .unwrap();

        assert_eq!(result.services.len(), 2);
        assert_eq!(result.services[0].service_name, "my.service");
        assert_eq!(result.services[1].service_name, "another-service");
    }

    #[test]
    fn test_ignores_image_nested_under_build() {
        let compose_content = r#"
services:
  web:
    build:
      context: .
      image: should-not-match:9.9.9
    image: nginx:1.21.0
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(compose_content.as_bytes()).unwrap();

        let parser = ComposeParser::new();
        let result = parser
            .parse_file(temp_file.path().to_str().unwrap())
            .unwrap();

        assert_eq!(result.services.len(), 1);
        assert_eq!(result.services[0].service_name, "web");
        assert_eq!(result.services[0].image_ref.name, "nginx");
        assert_eq!(result.services[0].image_ref.tag, "1.21.0");
    }
}
