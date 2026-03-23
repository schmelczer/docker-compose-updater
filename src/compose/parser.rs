use crate::registry::ImageRef;
use anyhow::Result;
use regex::Regex;
use std::fs;
use tracing::{info, warn};

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
        let image_regex = Regex::new(r#"^\s*image:\s*(?:["']([^"']+)["']|([^\s#]+))\s*(#.*)?$"#)?;

        for (line_number, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with("services:") {
                in_services = true;
                continue;
            }

            if in_services
                && line.chars().next().is_some_and(|c| c.is_alphabetic())
                && !line.starts_with("  ")
            {
                in_services = false;
                current_service = None;
                continue;
            }

            if in_services {
                if let Some(service_name) = self.extract_service_name(line) {
                    current_service = Some(service_name);
                    continue;
                }

                if let Some(ref service_name) = current_service {
                    if let Some(image_ref) = self.extract_image_from_line(line, &image_regex)? {
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

        Ok(services)
    }

    fn extract_service_name(&self, line: &str) -> Option<String> {
        let trimmed = line.trim_start();
        let indent_level = line.len() - trimmed.len();

        if indent_level > 0 && indent_level <= 8 && trimmed.ends_with(':') && !trimmed.contains(' ')
        {
            let potential_service = trimmed.trim_end_matches(':');
            if !potential_service.is_empty()
                && potential_service
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            {
                return Some(potential_service.to_string());
            }
        }

        None
    }

    fn extract_image_from_line(&self, line: &str, image_regex: &Regex) -> Result<Option<ImageRef>> {
        let trimmed = line.trim_start();
        let indent_level = line.len() - trimmed.len();

        if indent_level > 2 && trimmed.starts_with("image:") {
            if let Some(captures) = image_regex.captures(line) {
                let image_str = captures
                    .get(1)
                    .or_else(|| captures.get(2))
                    .unwrap()
                    .as_str();

                if image_str.find('@').is_some() {
                    info!("Found digest in image string, skipping: {}", image_str);
                    return Ok(None);
                }

                if image_str.trim().is_empty()
                    || image_str.trim() == "\"\""
                    || image_str.trim() == "''"
                {
                    info!("Empty image string found in line: {}, skipping", line);
                    return Ok(None);
                }

                match ImageRef::parse(image_str) {
                    Ok(image_ref) => Ok(Some(image_ref)),
                    Err(e) => {
                        warn!("Failed to parse image '{}': {}", image_str, e);
                        Ok(None)
                    }
                }
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
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
}
