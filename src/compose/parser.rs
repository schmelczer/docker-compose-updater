use crate::registry::ImageRef;
use anyhow::Result;
use regex::Regex;
use std::fs;
use std::sync::LazyLock;
use tracing::{info, warn};

static IMAGE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*image:\s*(?:["']([^"']+)["']|([^\s#]+))\s*(#.*)?$"#).unwrap()
});

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

            if indent == 0 && trimmed.starts_with("services:") {
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

            if (service_indent.is_none() || indent == service_indent.unwrap())
                && trimmed.ends_with(':')
                && !trimmed.contains(' ')
                && trimmed
                    .trim_end_matches(':')
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
            {
                let name = trimmed.trim_end_matches(':');
                if !name.is_empty() {
                    service_indent = Some(indent);
                    service_child_indent = None;
                    current_service = Some(name.to_string());
                }
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
