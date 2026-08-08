use crate::config::UpdateStrategy;
use crate::version::VersionInfo;
use tracing::debug;

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
        let versions =
            get_filtered_and_sorted_matching_versions(available, current_prefix, current_suffix);

        let latest = versions.first().cloned();

        if let Some(ref selected) = latest {
            debug!("Latest version selected: {}", selected.version);
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
        let versions =
            get_filtered_and_sorted_matching_versions(available, current_prefix, current_suffix);

        let latest = &versions.first()?.version;
        let latest_line = (latest.major, latest.minor);
        debug!("Latest version available: {}", latest);

        // The target is the highest patch of the newest release line strictly
        // below the latest one. Deriving that line by decrementing (`minor - 1`,
        // or `major - 1` when the latest is a `.0`) assumes releases never skip a
        // number, which registries routinely do: linuxserver/sonarr publishes 5.14
        // and 4.0.x with no 5.13, so a decrement searched for a 5.x line that does
        // not exist and returned nothing, stranding the image on its current tag.
        // Since `versions` is sorted descending, the first entry below the latest
        // line is exactly that.
        let selected = versions
            .iter()
            .find(|v| (v.version.major, v.version.minor) < latest_line)
            .cloned();

        if let Some(ref selected) = selected {
            debug!("Selected version: {}", selected.version);
        } else {
            debug!("No release line below {} to fall back to", latest);
        }

        selected
    }
}

fn get_filtered_and_sorted_matching_versions(
    available: &[VersionInfo],
    current_prefix: Option<String>,
    current_suffix: Option<String>,
) -> Vec<VersionInfo> {
    let mut versions: Vec<VersionInfo> = available
        .iter()
        .filter(|v| v.prefix == current_prefix && v.suffix == current_suffix)
        .cloned()
        .collect();
    versions.sort_by(|a, b| b.version.cmp(&a.version));

    debug!(
        "Filtered versions (prefix={:?}, suffix={:?}): {}",
        current_prefix,
        current_suffix,
        versions.len()
    );

    versions
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

    /// linuxserver/sonarr: the newest line is 5.14 with no 5.x below it, so the
    /// previous line is the 4.0 series. Decrementing the minor used to look for a
    /// nonexistent 5.13-or-lower and leave the image stuck on 4.0.15.
    #[test]
    fn test_previous_minor_steps_across_a_major_gap() {
        let selector = create_selector(&UpdateStrategy::LatestPatchOfPreviousMinor);

        let available = vec![
            VersionInfo::from_tag("5.14").unwrap(),
            VersionInfo::from_tag("4.0.19").unwrap(),
            VersionInfo::from_tag("4.0.18").unwrap(),
            VersionInfo::from_tag("4.0.15").unwrap(),
        ];

        let target = selector.select_target_version(&available, None, None);
        assert_eq!(
            target.map(|v| v.version),
            Some(Version::parse("4.0.19").unwrap())
        );
    }

    /// linuxserver/qbittorrent: a stray legacy 14.3.9 tag sorts above every real
    /// release, and there is no 14.x line below it. The previous line is 5.2.
    #[test]
    fn test_previous_minor_skips_an_isolated_outlier_line() {
        let selector = create_selector(&UpdateStrategy::LatestPatchOfPreviousMinor);

        let available = vec![
            VersionInfo::from_tag("14.3.9").unwrap(),
            VersionInfo::from_tag("5.2.3").unwrap(),
            VersionInfo::from_tag("5.2.0").unwrap(),
            VersionInfo::from_tag("5.1.4").unwrap(),
        ];

        let target = selector.select_target_version(&available, None, None);
        assert_eq!(
            target.map(|v| v.version),
            Some(Version::parse("5.2.3").unwrap())
        );
    }

    /// Skipped minors (2.5 -> 2.3, no 2.4) must not strand the image either.
    #[test]
    fn test_previous_minor_handles_skipped_minors() {
        let selector = create_selector(&UpdateStrategy::LatestPatchOfPreviousMinor);

        let available = vec![
            VersionInfo::from_tag("2.5.2").unwrap(),
            VersionInfo::from_tag("2.3.5").unwrap(),
            VersionInfo::from_tag("2.3.0").unwrap(),
        ];

        let target = selector.select_target_version(&available, None, None);
        assert_eq!(
            target.map(|v| v.version),
            Some(Version::parse("2.3.5").unwrap())
        );
    }

    /// A single release line has nothing below it: staying put is correct, since
    /// the strategy exists to keep one line behind the newest.
    #[test]
    fn test_previous_minor_without_an_older_line() {
        let selector = create_selector(&UpdateStrategy::LatestPatchOfPreviousMinor);

        let available = vec![
            VersionInfo::from_tag("1.0.2").unwrap(),
            VersionInfo::from_tag("1.0.1").unwrap(),
        ];

        assert!(selector
            .select_target_version(&available, None, None)
            .is_none());
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
