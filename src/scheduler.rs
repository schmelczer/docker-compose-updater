use crate::{compose::updater::ComposeUpdater, config::Config, health::Health};
use anyhow::{ensure, Context, Result};
use cron::Schedule;
use std::{str::FromStr, time::Duration};
use tokio::time::{sleep, Instant};
use tracing::{error, info};

pub struct Scheduler {
    config: Config,
    updater: ComposeUpdater,
    schedule: Schedule,
    health: Option<Health>,
}

impl Scheduler {
    pub fn new(config: Config, health: Option<Health>) -> Result<Self> {
        let schedule = Schedule::from_str(&config.schedule)
            .with_context(|| format!("Invalid cron expression '{}'", config.schedule))?;
        let updater = ComposeUpdater::new(config.clone());
        Ok(Self {
            config,
            updater,
            schedule,
            health,
        })
    }

    pub async fn start(&self) -> Result<()> {
        info!(
            "Starting scheduler with cron expression: {}",
            self.config.schedule
        );
        loop {
            if let Err(error) = self.run_once().await {
                error!("{:#}", error);
            }
            let delay = if let Some(next) = self.schedule.upcoming(chrono::Utc).next() {
                info!("Next update scheduled for: {}", next);
                (next - chrono::Utc::now())
                    .to_std()
                    .unwrap_or_default()
                    .max(Duration::from_secs(1))
            } else {
                Duration::from_secs(60)
            };
            sleep(delay).await;
        }
    }

    pub async fn run_once(&self) -> Result<()> {
        if let Some(health) = &self.health {
            health.update_started();
        }
        let result = self.run_update().await;
        if let Some(health) = &self.health {
            health.update_finished(result.is_ok());
        }
        result
    }

    async fn run_update(&self) -> Result<()> {
        let start = Instant::now();
        info!("Starting update cycle");
        let report = self.updater.update_all_compose_files().await;
        ensure!(
            report.files_found > 0,
            "No compose files found under configured compose_paths: {:?}",
            self.config.compose_paths
        );
        for error in &report.errors {
            error!("Update error: {}", error);
        }
        ensure!(
            report.errors.is_empty(),
            "Update cycle completed with {} error(s); {} file(s) updated",
            report.errors.len(),
            report.updated_files.len()
        );
        let action = if self.config.dry_run {
            "would update"
        } else {
            "updated"
        };
        info!(
            "Update cycle completed in {:?}: scanned {} files, {} {}",
            start.elapsed(),
            report.files_found,
            action,
            report.updated_files.len()
        );
        for file in &report.updated_files {
            info!("  - {}", file);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, UpdateStrategy};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn test_config(schedule: &str) -> Config {
        Config {
            compose_paths: vec![PathBuf::from("./test")],
            schedule: schedule.to_string(),
            registries: HashMap::new(),
            update_strategy: UpdateStrategy::LatestPatchOfPreviousMinor,
            ignore_images: vec![],
            dry_run: true,
            ..Config::default()
        }
    }

    #[test]
    fn test_scheduler_creation() {
        let scheduler = Scheduler::new(test_config("0 0 2 * * *"), None).unwrap();
        assert!(scheduler.schedule.upcoming(chrono::Utc).next().is_some());
    }

    #[test]
    fn test_scheduler_with_different_cron() {
        let scheduler = Scheduler::new(test_config("0 30 1 * * *"), None).unwrap();
        assert!(scheduler.schedule.upcoming(chrono::Utc).next().is_some());
    }

    #[tokio::test]
    async fn test_run_once_errors_when_no_compose_files_found() {
        let mut config = test_config("0 0 2 * * *");
        config.compose_paths = vec![];
        let scheduler = Scheduler::new(config, None).unwrap();

        let result = scheduler.run_once().await;
        assert!(
            result.is_err(),
            "an update cycle that finds no compose files should report an error"
        );
    }
}
