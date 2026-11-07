use crate::registry::ImageRef;
use anyhow::{ensure, Context, Result};
use serde_yaml::Value;
use std::{fs, path::Path};
use tracing::warn;

#[derive(Debug)]
pub struct ServiceImage {
    pub service_name: String,
    pub image_ref: ImageRef,
}

pub struct ComposeFile {
    pub document: Value,
    pub(crate) original_content: String,
}

impl ComposeFile {
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        Self::parse(&fs::read_to_string(path)?)
    }

    pub fn parse(content: &str) -> Result<Self> {
        let mut document: Value = serde_yaml::from_str(content).context("Invalid Compose YAML")?;
        ensure!(
            document.is_mapping() || document.is_null(),
            "Compose file must be a mapping"
        );
        resolve_merges(&mut document).context("Invalid Compose YAML merge")?;
        Ok(Self {
            document,
            original_content: content.to_owned(),
        })
    }

    pub fn images(&self) -> Result<Vec<ServiceImage>> {
        let mut images = Vec::new();
        let Some(services) = self
            .document
            .get("services")
            .filter(|value| !value.is_null())
        else {
            return Ok(images);
        };
        for (name, service) in services
            .as_mapping()
            .context("Compose services must be a mapping")?
        {
            let name = name
                .as_str()
                .context("Compose service names must be strings")?;
            let Some(image) = service.get("image").and_then(Value::as_str) else {
                continue;
            };
            if image.trim().is_empty() || image.contains('@') {
                continue;
            }
            match ImageRef::parse(image) {
                Ok(image_ref) => images.push(ServiceImage {
                    service_name: name.to_owned(),
                    image_ref,
                }),
                Err(error) => warn!(
                    "Skipping invalid image '{}' for service '{}': {}",
                    image, name, error
                ),
            }
        }
        Ok(images)
    }

    pub fn set_image(&mut self, service_name: &str, image: &str) -> Result<()> {
        let value = self
            .document
            .get_mut("services")
            .and_then(|services| services.get_mut(service_name))
            .and_then(|service| service.get_mut("image"))
            .context("Service has no image property")?;
        *value = Value::String(image.to_owned());
        Ok(())
    }
}

fn resolve_merges(value: &mut Value) -> Result<(), serde_yaml::Error> {
    match value {
        Value::Mapping(mapping) => {
            // Inherited keys must be resolved before combining merge-sequence
            // entries, where earlier entries take precedence over later ones.
            for child in mapping.values_mut() {
                resolve_merges(child)?;
            }
            if mapping.contains_key("<<") {
                value.apply_merge()?;
            }
        }
        Value::Sequence(sequence) => {
            for child in sequence {
                resolve_merges(child)?;
            }
        }
        Value::Tagged(tagged) => resolve_merges(&mut tagged.value)?,
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_empty_build_only_nested_and_digest_pinned_images() {
        let file = ComposeFile::parse(
            r#"
services:
  empty_double: {image: "", build: .}
  empty_single: {image: '', build: .}
  whitespace: {image: '   ', build: .}
  build_only: {build: {context: ., image: nested:9.9.9}}
  pinned: {image: 'nginx@sha256:abc123'}
  web:
    build: {image: nested:9.9.9}
    image: nginx:1.21.0
"#,
        )
        .unwrap();
        let images = file.images().unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].service_name, "web");
        assert_eq!(images[0].image_ref.to_string(), "nginx:1.21.0");
    }

    #[test]
    fn updates_flow_mappings_aliases_and_merges() {
        for content in [
            "services: {web: {image: 'nginx:1.21.0', ports: ['80:80']}}",
            "x-base: &base {image: 'nginx:1.21.0', ports: ['80:80']}\nservices: {web: *base}",
            "x-base: &base {image: 'nginx:1.21.0', ports: ['80:80']}\nservices:\n  'web':\n    <<: *base",
        ] {
            let mut file = ComposeFile::parse(content).unwrap();
            assert_eq!(file.images().unwrap()[0].image_ref.to_string(), "nginx:1.21.0");
            let before = file.document.clone();
            file.set_image("web", "nginx:1.22.0").unwrap();
            let yaml = serde_yaml::to_string(&file.document).unwrap();
            let updated: Value = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(updated["services"]["web"]["image"], "nginx:1.22.0");
            assert_eq!(updated["services"]["web"]["ports"], before["services"]["web"]["ports"]);
            assert_eq!(updated.get("x-base"), before.get("x-base"));
        }
    }

    #[test]
    fn nested_merge_precedence_survives_image_updates() {
        let mut file = ComposeFile::parse(
            r#"
x-base: &base {KEY: correct, BASE_ONLY: retained}
x-derived: &derived {<<: *base}
x-deeper: &deeper {<<: [*derived, {KEY: intermediate}]}
services:
  web:
    image: nginx:1.21.0
    environment:
      <<: [*derived, {KEY: wrong, FALLBACK_ONLY: retained}]
  deeper:
    image: nginx:1.21.0
    environment:
      <<: [*deeper, {KEY: wrong, FALLBACK_ONLY: retained}]
  explicit:
    image: nginx:1.21.0
    environment:
      <<: [*deeper, {KEY: wrong, FALLBACK_ONLY: retained}]
      KEY: explicit
"#,
        )
        .unwrap();

        for service in ["web", "deeper", "explicit"] {
            file.set_image(service, "nginx:1.22.0").unwrap();
        }
        let yaml = serde_yaml::to_string(&file.document).unwrap();
        let updated: Value = serde_yaml::from_str(&yaml).unwrap();
        for (service, expected) in [
            ("web", "correct"),
            ("deeper", "correct"),
            ("explicit", "explicit"),
        ] {
            let declaration = &updated["services"][service];
            assert_eq!(declaration["image"], "nginx:1.22.0");
            let environment = &declaration["environment"];
            assert_eq!(environment["KEY"], expected, "service: {service}");
            assert_eq!(environment["BASE_ONLY"], "retained");
            assert_eq!(environment["FALLBACK_ONLY"], "retained");
            assert!(environment.get("<<").is_none());
        }
    }

    #[test]
    fn parses_calibre_compose_with_empty_image() {
        let file = ComposeFile::parse(include_str!("../../compose/calibre.yml")).unwrap();
        let images = file.images().unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].service_name, "audiobookshelf");
    }

    #[test]
    fn reports_invalid_yaml_and_services() {
        assert!(ComposeFile::parse("services: [invalid yaml").is_err());
        for content in [
            "services: {web: {environment: {<<: invalid}}}",
            "services: {web: {environment: {<<: [{KEY: value}, invalid]}}}",
        ] {
            let error = ComposeFile::parse(content).err().unwrap();
            assert!(error.to_string().contains("Invalid Compose YAML merge"));
        }
        assert!(ComposeFile::parse("services: [web]")
            .unwrap()
            .images()
            .is_err());
        assert!(ComposeFile::parse("services: {1: {image: nginx}}")
            .unwrap()
            .images()
            .is_err());
        for content in ["", "services:", "version: '3.8'"] {
            assert!(ComposeFile::parse(content)
                .unwrap()
                .images()
                .unwrap()
                .is_empty());
        }
    }
}
