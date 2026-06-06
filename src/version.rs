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
        if let Some(captures) = VERSION_REGEX.captures(tag) {
            let prefix_part = captures.name("prefix").map_or("", |m| m.as_str());
            let version_part = captures.name("version").map_or("", |m| m.as_str());
            let suffix_part = captures.name("suffix").map_or("", |m| m.as_str());

            let version_part = version_part
                .split('.')
                .chain(std::iter::repeat("0"))
                .take(3)
                .collect::<Vec<_>>()
                .join(".");

            if let Ok(version) = Version::parse(&version_part) {
                let prefix = if prefix_part.is_empty() {
                    None
                } else {
                    Some(prefix_part.to_string())
                };

                let suffix = if suffix_part.is_empty() {
                    None
                } else {
                    Some(suffix_part.to_string())
                };

                return Some(Self {
                    version,
                    prefix,
                    suffix,
                    original: tag.to_string(),
                });
            }
        }

        None
    }
}

impl PartialEq<Version> for VersionInfo {
    fn eq(&self, other: &Version) -> bool {
        self.version == *other
    }
}

impl fmt::Display for VersionInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.original)
    }
}

/// Extracts semantic version, prefix and suffix from a container image tag
///
/// # Examples
///
/// ```
/// use docker_compose_updater::version::parse_version_tag;
///
/// let (version, prefix, suffix, original) = parse_version_tag("v1.29.0-alpine-slim");
/// assert_eq!(version.unwrap().to_string(), "1.29.0");
/// assert_eq!(prefix, Some("v".to_string()));
/// assert_eq!(suffix, Some("-alpine-slim".to_string()));
/// assert_eq!(original, "v1.29.0-alpine-slim".to_string());
/// ```
pub fn parse_version_tag(tag: &str) -> (Option<Version>, Option<String>, Option<String>, String) {
    if let Some(version_info) = VersionInfo::from_tag(tag) {
        (
            Some(version_info.version),
            version_info.prefix,
            version_info.suffix,
            version_info.original,
        )
    } else {
        (None, None, None, tag.to_string())
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
            let (version_opt, prefix, suffix, _) = parse_version_tag(input);

            match version_opt {
                Some(version) => {
                    assert_eq!(
                        version.to_string(),
                        expected_version,
                        "Version mismatch for {input}"
                    );
                    assert_eq!(
                        prefix,
                        expected_prefix.map(String::from),
                        "Prefix mismatch for {input}"
                    );
                    assert_eq!(
                        suffix,
                        expected_suffix.map(String::from),
                        "Suffix mismatch for {input}"
                    );
                }
                None => {
                    panic!("Expected version for {input}");
                }
            }
        }
    }

    #[test]
    fn test_non_semver_tags() {
        let non_semver_tags = vec!["latest", "stable", "main", "alpine"];

        for tag in non_semver_tags {
            let (version_opt, _prefix, _suffix, _) = parse_version_tag(tag);
            assert!(
                version_opt.is_none(),
                "Expected no version for non-semver tag: {tag}"
            );
        }
    }
}
