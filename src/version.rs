use regex::Regex;
use semver::Version;
use std::fmt;
use std::sync::LazyLock;

static VERSION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<prefix>.*?)(?P<version>(?:\d+\.)+\d+)(?P<suffix>.*?)$").unwrap()
});

#[derive(Debug, Clone, PartialEq)]
pub struct VersionInfo {
    pub version: Version,
    pub prefix: Option<String>,
    pub suffix: Option<String>,
    pub original: String,
}

impl VersionInfo {
    /// Parses a container image tag into a VersionInfo struct
    ///
    /// # Examples
    ///
    /// ```
    /// use docker_compose_updater::version::VersionInfo;
    ///
    /// let version_info = VersionInfo::from_tag("v1.29.0-alpine-slim").unwrap();
    /// assert_eq!(version_info.version.to_string(), "1.29.0");
    /// assert_eq!(version_info.prefix, Some("v".to_string()));
    /// assert_eq!(version_info.suffix, Some("-alpine-slim".to_string()));
    /// ```
    pub fn from_tag(tag: &str) -> Option<Self> {
        let captures = VERSION_REGEX.captures(tag)?;
        let version = captures["version"]
            .split('.')
            .chain(std::iter::repeat("0"))
            .take(3)
            .collect::<Vec<_>>()
            .join(".");
        Some(Self {
            version: Version::parse(&version).ok()?,
            prefix: (!captures["prefix"].is_empty()).then(|| captures["prefix"].to_owned()),
            suffix: (!captures["suffix"].is_empty()).then(|| captures["suffix"].to_owned()),
            original: tag.to_owned(),
        })
    }
}

impl fmt::Display for VersionInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.original)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version_tag() {
        let test_cases = vec![
            ("1.29.0", "1.29.0", None, None),
            ("v1.29.0", "1.29.0", Some("v"), None),
            ("v1.29.0.10.2", "1.29.0", Some("v"), None),
            ("v1.29", "1.29.0", Some("v"), None),
            ("release-1.2.3", "1.2.3", Some("release-"), None),
            ("app_v1.5.0", "1.5.0", Some("app_v"), None),
            ("1.29.0-alpine", "1.29.0", None, Some("-alpine")),
            ("15.3-alpine3.18", "15.3.0", None, Some("-alpine3.18")),
            (
                "v1.29.0-alpine-slim",
                "1.29.0",
                Some("v"),
                Some("-alpine-slim"),
            ),
            (
                "release-1.2.3-ubuntu",
                "1.2.3",
                Some("release-"),
                Some("-ubuntu"),
            ),
            ("v4.1.1", "4.1.1", Some("v"), None),
            (
                "v2.1.3-bookworm-perl",
                "2.1.3",
                Some("v"),
                Some("-bookworm-perl"),
            ),
            (
                "build123-2.0.1-final",
                "2.0.1",
                Some("build123-"),
                Some("-final"),
            ),
        ];

        for (input, expected_version, expected_prefix, expected_suffix) in test_cases {
            let parsed = VersionInfo::from_tag(input).unwrap();
            assert_eq!(
                parsed.version.to_string(),
                expected_version,
                "Version mismatch for {input}"
            );
            assert_eq!(
                parsed.prefix,
                expected_prefix.map(String::from),
                "Prefix mismatch for {input}"
            );
            assert_eq!(
                parsed.suffix,
                expected_suffix.map(String::from),
                "Suffix mismatch for {input}"
            );
        }
    }

    #[test]
    fn test_non_semver_tags() {
        let non_semver_tags = vec!["latest", "stable", "main", "alpine"];

        for tag in non_semver_tags {
            let version = VersionInfo::from_tag(tag);
            assert!(
                version.is_none(),
                "Expected no version for non-semver tag: {tag}"
            );
        }
    }
}
