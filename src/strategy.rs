use crate::config::UpdateStrategy;
use crate::version::VersionInfo;
use tracing::{info, warn};

pub fn create_selector(strategy: &UpdateStrategy) -> Box<dyn VersionSelector> {
    match strategy {
        UpdateStrategy::Latest => Box::new(LatestVersionSelector),
        UpdateStrategy::LatestPatchOfPreviousMinor => Box::new(SmartPreviousMinorSelector),
    }
}

pub trait VersionSelector {
    fn select_target_version(
        &self,
        available: &[VersionInfo],
        current_prefix: Option<String>,
        current_suffix: Option<String>,
    ) -> Option<VersionInfo>;
}

pub struct LatestVersionSelector;

impl VersionSelector for LatestVersionSelector {
    fn select_target_version(
        &self,
        available: &[VersionInfo],
        current_prefix: Option<String>,
        current_suffix: Option<String>,
    ) -> Option<VersionInfo> {
        info!("Using update strategy: LatestVersionSelector");

        let versions =
            get_filtered_and_soreted_matching_versions(available, current_prefix, current_suffix);

        let latest = versions.first().cloned();

        if let Some(ref selected) = latest {
            info!("Selected {} as latest version", selected.version);
        } else {
            warn!("No versions available for selection");
        }

        latest
    }
}

pub struct SmartPreviousMinorSelector;

impl VersionSelector for SmartPreviousMinorSelector {
    fn select_target_version(
        &self,
        available: &[VersionInfo],
        current_prefix: Option<String>,
        current_suffix: Option<String>,
    ) -> Option<VersionInfo> {
        info!("Using update strategy: SmartPreviousMinorSelector");

        let versions =
            get_filtered_and_soreted_matching_versions(available, current_prefix, current_suffix);

        let latest = &versions.first()?.version;
        info!("Latest version available: {}", latest);

        let (target_major, max_minor) = if latest.minor == 0 {
            // If current minor is 0, look for the latest minor of the previous major
            if latest.major == 0 {
                info!("Cannot go to previous version of 0.0.x, skipping update");
                return None;
            }

            info!("The latest version has a minor version of 0, looking for the latest minor of the previous major");
            (latest.major - 1, None)
        } else {
            info!("The latest version has a minor version of {}, looking for the latest patch of the previous minor", latest.minor);
            (latest.major, Some(latest.minor - 1))
        };

        let selected = versions
            .iter()
            .find(|v| {
                v.version.major == target_major && max_minor.is_none_or(|m| v.version.minor <= m)
            })
            .cloned();

        if let Some(ref selected) = selected {
            info!("Selected {} as latest version", selected.version);
        } else {
            warn!("No versions available for selection");
        }

        selected
    }
}

fn get_filtered_and_soreted_matching_versions(
    available: &[VersionInfo],
    current_prefix: Option<String>,
    current_suffix: Option<String>,
) -> Vec<VersionInfo> {
    let mut sorted_versions = available.to_vec();
    sorted_versions.sort_by(|a, b| b.version.cmp(&a.version));

    info!(
        "Available versions for selection: {}",
        sorted_versions
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let filtered_versions: Vec<VersionInfo> = sorted_versions
        .into_iter()
        .filter(|v| v.prefix == current_prefix && v.suffix == current_suffix)
        .collect();

    info!(
        "Filtered versions with matching prefix and suffix: {}",
        filtered_versions
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );

    filtered_versions
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::*;

    #[test]
    fn test_version_strategy_latest_patch_of_previous_minor() {
        let selector = create_selector(&UpdateStrategy::LatestPatchOfPreviousMinor);

        let available = vec![
            VersionInfo::from_tag("2.0.0").unwrap(),
            VersionInfo::from_tag("1.3.0").unwrap(),
            VersionInfo::from_tag("1.2.5").unwrap(),
            VersionInfo::from_tag("1.1.5").unwrap(),
            VersionInfo::from_tag("1.1.4").unwrap(),
        ];

        let target = selector.select_target_version(&available, None, None);
        assert_eq!(
            target.map(|v| v.version),
            Some(Version::parse("1.3.0").unwrap())
        );
    }

    #[test]
    fn test_version_strategy_latest_patch() {
        let selector = create_selector(&UpdateStrategy::LatestPatchOfPreviousMinor);

        let available = vec![
            VersionInfo::from_tag("1.3.0").unwrap(),
            VersionInfo::from_tag("1.2.5").unwrap(),
            VersionInfo::from_tag("1.1.5").unwrap(),
        ];

        let target = selector.select_target_version(&available, None, None);
        assert_eq!(
            target.map(|v| v.version),
            Some(Version::parse("1.2.5").unwrap())
        );

        let available_far_ahead = vec![
            VersionInfo::from_tag("2.0.0").unwrap(),
            VersionInfo::from_tag("1.4.0").unwrap(),
            VersionInfo::from_tag("1.2.5").unwrap(),
            VersionInfo::from_tag("1.1.5").unwrap(),
        ];

        let target = selector.select_target_version(&available_far_ahead, None, None);
        assert_eq!(
            target.map(|v| v.version),
            Some(Version::parse("1.4.0").unwrap())
        );
    }

    #[test]
    fn test_version_strategy_latest() {
        let selector = create_selector(&UpdateStrategy::Latest);

        let available = vec![
            VersionInfo::from_tag("2.0.0").unwrap(),
            VersionInfo::from_tag("1.2.5").unwrap(),
            VersionInfo::from_tag("1.1.5").unwrap(),
        ];

        let target = selector.select_target_version(&available, None, None);
        assert_eq!(
            target.map(|v| v.version),
            Some(Version::parse("2.0.0").unwrap())
        );
    }

    #[test]
    fn test_prefix_and_suffix_matching_in_strategy() {
        let selector = create_selector(&UpdateStrategy::Latest);

        let current_prefix = Some("v".to_string());
        let current_suffix = Some("-alpine".to_string());

        let available = vec![
            VersionInfo::from_tag("1.3.0-alpine").unwrap(),
            VersionInfo::from_tag("v1.4.0").unwrap(),
            VersionInfo::from_tag("v1.5.0-alpine").unwrap(),
            VersionInfo::from_tag("2.0.0").unwrap(),
        ];

        let target = selector.select_target_version(&available, current_prefix, current_suffix);
        assert_eq!(
            target.map(|v| v.version),
            Some(Version::parse("1.5.0").unwrap())
        );
    }

    #[test]
    fn test_cross_major_version_handling() {
        let selector = create_selector(&UpdateStrategy::LatestPatchOfPreviousMinor);

        let current_suffix = Some("-fat".to_string());

        let available = vec![
            VersionInfo::from_tag("1.0.2-fat").unwrap(),
            VersionInfo::from_tag("0.46.2-fat").unwrap(),
            VersionInfo::from_tag("0.46.1-fat").unwrap(),
            VersionInfo::from_tag("0.45.6-fat").unwrap(),
        ];

        let target = selector.select_target_version(&available, None, current_suffix);
        assert!(target.is_some());
        assert_eq!(target.unwrap().version, Version::parse("0.46.2").unwrap());
    }
}
