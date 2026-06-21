use crate::health::HealthHandle;
use crate::{compose::updater::ComposeUpdater, config::Config};
use anyhow::{anyhow, Context, Result};
use cron::Schedule;
use std::str::FromStr;
use std::time::Duration;
use tokio::time::{sleep, Instant};
use tracing::{error, info};

pub struct Scheduler {
    config: Config,
    updater: ComposeUpdater,
    schedule: Schedule,
    health_handle: Option<HealthHandle>,
}

impl Scheduler {
    pub fn new(config: Config, health_handle: Option<HealthHandle>) -> Result<Self> {
        let schedule = Schedule::from_str(&config.schedule)
            .map_err(|e| anyhow!("Invalid cron expression '{}': {}", config.schedule, e))?;

        let updater = ComposeUpdater::new(config.clone());

        Ok(Self {
            config,
            updater,
            schedule,
            health_handle,
        })
    }

    pub async fn start(&self) -> Result<()> {
        info!(
            "Starting scheduler with cron expression: {}",
            self.config.schedule
        );

        loop {
            if let Err(err) = self.run_update().await.context("Failed to run update") {
                error!("{:#}", err);
            }

            let sleep_duration =
                if let Some(next_run) = self.schedule.upcoming(chrono::Utc).take(1).next() {
                    let now = chrono::Utc::now();
                    let duration_until_next = next_run.signed_duration_since(now);
                    let millis = duration_until_next.num_milliseconds().max(0) as u64;
                    let duration = Duration::from_millis(millis).max(Duration::from_secs(1));

                    info!(
                        "Next update scheduled for: {}",
                        next_run.format("%Y-%m-%d %H:%M:%S UTC")
                    );
                    duration
                } else {
                    Duration::from_secs(60)
                };

            sleep(sleep_duration).await;
        }
    }

    pub async fn run_once(&self) -> Result<()> {
        info!("Running one-time update");
        self.run_update().await
    }

    async fn run_update(&self) -> Result<()> {
        let start_time = Instant::now();
        info!("Starting update cycle");

        match self.updater.update_all_compose_files().await {
            Ok(report) => {
                let duration = start_time.elapsed();

                if report.files_found == 0 {
                    if let Some(health) = &self.health_handle {
                        health.report_update_failure();
                    }
                    return Err(anyhow!(
                        "No compose files found under configured compose_paths: {:?}",
                        self.config.compose_paths
                    ));
                }

                // A degraded run (some services/files failed) still applied the
                // updates that succeeded, but the operator must see the failure:
                // health stays binary and fails closed.
                if !report.errors.is_empty() {
                    for err in &report.errors {
                        error!("Update error: {}", err);
                    }
                    if let Some(health) = &self.health_handle {
                        health.report_update_failure();
                    }
                    return Err(anyhow!(
                        "Update cycle completed with {} error(s); {} file(s) updated",
                        report.errors.len(),
                        report.updated_files.len()
                    ));
                }

                if report.updated_files.is_empty() {
                    info!(
                        "Update cycle completed in {:?} - scanned {} files, none updated",
                        duration, report.files_found
                    );
                } else {
                    let verb = if self.config.dry_run {
                        "would update"
                    } else {
                        "updated"
                    };
                    info!(
                        "Update cycle completed in {:?} - scanned {} files, {} {} files:",
                        duration,
                        report.files_found,
                        verb,
                        report.updated_files.len()
                    );
                    for file in &report.updated_files {
                        info!("  - {}", file);
                    }
                }

                if let Some(health) = &self.health_handle {
                    health.report_update_success();
                }
                Ok(())
            }
            Err(e) => {
                if let Some(health) = &self.health_handle {
                    health.report_update_failure();
                }
                Err(e.context("Failed to update Docker Compose files"))
            }
        }
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
        }
    }

    #[test]
    fn test_scheduler_creation() {
        let scheduler = Scheduler::new(test_config("0 0 2 * * *"), None).unwrap();
        assert!(scheduler
            .schedule
            .upcoming(chrono::Utc)
            .take(1)
            .next()
            .is_some());
    }

    #[test]
    fn test_scheduler_with_different_cron() {
        let scheduler = Scheduler::new(test_config("0 30 1 * * *"), None).unwrap();
        assert!(scheduler
            .schedule
            .upcoming(chrono::Utc)
            .take(1)
            .next()
            .is_some());
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
