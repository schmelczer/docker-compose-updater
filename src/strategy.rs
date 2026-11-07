use crate::{config::UpdateStrategy, version::VersionInfo};

pub fn select_target_version<'a>(
    strategy: &UpdateStrategy,
    available: &'a [VersionInfo],
    prefix: Option<&str>,
    suffix: Option<&str>,
) -> Option<&'a VersionInfo> {
    let matching = available
        .iter()
        .filter(|v| v.prefix.as_deref() == prefix && v.suffix.as_deref() == suffix);
    let latest = matching.clone().max_by(|a, b| a.version.cmp(&b.version))?;
    match strategy {
        UpdateStrategy::Latest => Some(latest),
        UpdateStrategy::LatestPatchOfPreviousMinor => {
            let latest_line = (latest.version.major, latest.version.minor);
            matching
                .filter(|v| (v.version.major, v.version.minor) < latest_line)
                .max_by(|a, b| a.version.cmp(&b.version))
        }
    }
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::*;

    #[test]
    fn test_version_strategy_latest_patch_of_previous_minor() {
        let strategy = UpdateStrategy::LatestPatchOfPreviousMinor;

        let available = vec![
            VersionInfo::from_tag("2.0.0").unwrap(),
            VersionInfo::from_tag("1.3.0").unwrap(),
            VersionInfo::from_tag("1.2.5").unwrap(),
            VersionInfo::from_tag("1.1.5").unwrap(),
            VersionInfo::from_tag("1.1.4").unwrap(),
        ];

        let target = select_target_version(&strategy, &available, None, None);
        assert_eq!(
            target.map(|v| v.version.clone()),
            Some(Version::parse("1.3.0").unwrap())
        );
    }

    #[test]
    fn test_version_strategy_latest_patch() {
        let strategy = UpdateStrategy::LatestPatchOfPreviousMinor;

        let available = vec![
            VersionInfo::from_tag("1.3.0").unwrap(),
            VersionInfo::from_tag("1.2.5").unwrap(),
            VersionInfo::from_tag("1.1.5").unwrap(),
        ];

        let target = select_target_version(&strategy, &available, None, None);
        assert_eq!(
            target.map(|v| v.version.clone()),
            Some(Version::parse("1.2.5").unwrap())
        );

        let available_far_ahead = vec![
            VersionInfo::from_tag("2.0.0").unwrap(),
            VersionInfo::from_tag("1.4.0").unwrap(),
            VersionInfo::from_tag("1.2.5").unwrap(),
            VersionInfo::from_tag("1.1.5").unwrap(),
        ];

        let target = select_target_version(&strategy, &available_far_ahead, None, None);
        assert_eq!(
            target.map(|v| v.version.clone()),
            Some(Version::parse("1.4.0").unwrap())
        );
    }

    #[test]
    fn test_version_strategy_latest() {
        let strategy = UpdateStrategy::Latest;

        let available = vec![
            VersionInfo::from_tag("2.0.0").unwrap(),
            VersionInfo::from_tag("1.2.5").unwrap(),
            VersionInfo::from_tag("1.1.5").unwrap(),
        ];

        let target = select_target_version(&strategy, &available, None, None);
        assert_eq!(
            target.map(|v| v.version.clone()),
            Some(Version::parse("2.0.0").unwrap())
        );
    }

    #[test]
    fn test_prefix_and_suffix_matching_in_strategy() {
        let strategy = UpdateStrategy::Latest;

        let current_prefix = Some("v".to_string());
        let current_suffix = Some("-alpine".to_string());

        let available = vec![
            VersionInfo::from_tag("1.3.0-alpine").unwrap(),
            VersionInfo::from_tag("v1.4.0").unwrap(),
            VersionInfo::from_tag("v1.5.0-alpine").unwrap(),
            VersionInfo::from_tag("2.0.0").unwrap(),
        ];

        let target = select_target_version(
            &strategy,
            &available,
            current_prefix.as_deref(),
            current_suffix.as_deref(),
        );
        assert_eq!(
            target.map(|v| v.version.clone()),
            Some(Version::parse("1.5.0").unwrap())
        );
    }

    /// linuxserver/sonarr: the newest line is 5.14 with no 5.x below it, so the
    /// previous line is the 4.0 series. Decrementing the minor used to look for a
    /// nonexistent 5.13-or-lower and leave the image stuck on 4.0.15.
    #[test]
    fn test_previous_minor_steps_across_a_major_gap() {
        let strategy = UpdateStrategy::LatestPatchOfPreviousMinor;

        let available = vec![
            VersionInfo::from_tag("5.14").unwrap(),
            VersionInfo::from_tag("4.0.19").unwrap(),
            VersionInfo::from_tag("4.0.18").unwrap(),
            VersionInfo::from_tag("4.0.15").unwrap(),
        ];

        let target = select_target_version(&strategy, &available, None, None);
        assert_eq!(
            target.map(|v| v.version.clone()),
            Some(Version::parse("4.0.19").unwrap())
        );
    }

    /// linuxserver/qbittorrent: a stray legacy 14.3.9 tag sorts above every real
    /// release, and there is no 14.x line below it. The previous line is 5.2.
    #[test]
    fn test_previous_minor_skips_an_isolated_outlier_line() {
        let strategy = UpdateStrategy::LatestPatchOfPreviousMinor;

        let available = vec![
            VersionInfo::from_tag("14.3.9").unwrap(),
            VersionInfo::from_tag("5.2.3").unwrap(),
            VersionInfo::from_tag("5.2.0").unwrap(),
            VersionInfo::from_tag("5.1.4").unwrap(),
        ];

        let target = select_target_version(&strategy, &available, None, None);
        assert_eq!(
            target.map(|v| v.version.clone()),
            Some(Version::parse("5.2.3").unwrap())
        );
    }

    /// Skipped minors (2.5 -> 2.3, no 2.4) must not strand the image either.
    #[test]
    fn test_previous_minor_handles_skipped_minors() {
        let strategy = UpdateStrategy::LatestPatchOfPreviousMinor;

        let available = vec![
            VersionInfo::from_tag("2.5.2").unwrap(),
            VersionInfo::from_tag("2.3.5").unwrap(),
            VersionInfo::from_tag("2.3.0").unwrap(),
        ];

        let target = select_target_version(&strategy, &available, None, None);
        assert_eq!(
            target.map(|v| v.version.clone()),
            Some(Version::parse("2.3.5").unwrap())
        );
    }

    /// A single release line has nothing below it: staying put is correct, since
    /// the strategy exists to keep one line behind the newest.
    #[test]
    fn test_previous_minor_without_an_older_line() {
        let strategy = UpdateStrategy::LatestPatchOfPreviousMinor;

        let available = vec![
            VersionInfo::from_tag("1.0.2").unwrap(),
            VersionInfo::from_tag("1.0.1").unwrap(),
        ];

        assert!(select_target_version(&strategy, &available, None, None).is_none());
    }

    #[test]
    fn test_cross_major_version_handling() {
        let strategy = UpdateStrategy::LatestPatchOfPreviousMinor;

        let current_suffix = Some("-fat".to_string());

        let available = vec![
            VersionInfo::from_tag("1.0.2-fat").unwrap(),
            VersionInfo::from_tag("0.46.2-fat").unwrap(),
            VersionInfo::from_tag("0.46.1-fat").unwrap(),
            VersionInfo::from_tag("0.45.6-fat").unwrap(),
        ];

        let target = select_target_version(&strategy, &available, None, current_suffix.as_deref());
        assert!(target.is_some());
        assert_eq!(target.unwrap().version, Version::parse("0.46.2").unwrap());
    }
}
