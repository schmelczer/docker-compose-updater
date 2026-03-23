use crate::health::HealthHandle;
use crate::{compose::updater::ComposeUpdater, config::Config};
use anyhow::{anyhow, Context, Result};
use cron::Schedule;
use std::str::FromStr;
use std::time::Duration;
use tokio::time::{sleep, Instant};
use tracing::error;
use tracing::info;

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
                error!("{:?}", err)
            }

            if let Some(next_run) = self.schedule.upcoming(chrono::Utc).take(1).next() {
                let now = chrono::Utc::now();
                let duration_until_next = next_run.signed_duration_since(now);

                if duration_until_next.num_seconds() > 0 {
                    info!(
                        "Next update scheduled for: {}",
                        next_run.format("%Y-%m-%d %H:%M:%S UTC")
                    );

                    let sleep_duration =
                        Duration::from_secs(duration_until_next.num_seconds() as u64);
                    sleep(sleep_duration).await;
                }
            }
        }
    }

    pub async fn run_once(&self) -> Result<()> {
        info!("Running one-time update");
        self.run_update().await
    }

    async fn run_update(&self) -> Result<()> {
        let start_time = Instant::now();
        info!("Starting Docker Compose update cycle");

        match self.updater.update_all_compose_files().await {
            Ok(updated_files) => {
                let duration = start_time.elapsed();
                if updated_files.is_empty() {
                    info!(
                        "Update cycle completed in {:?} - no files updated",
                        duration
                    );
                } else {
                    info!(
                        "Update cycle completed in {:?} - {} {} files:",
                        duration,
                        if self.config.dry_run {
                            "would update"
                        } else {
                            "updated"
                        },
                        updated_files.len()
                    );
                    for file in &updated_files {
                        info!("  - {}", file);
                    }
                }

                // Report success to health monitor
                if let Some(ref health) = self.health_handle {
                    health.report_update_success();
                }

                Ok(())
            }
            Err(e) => {
                let error = e.context("Failed to update Docker Compose files");

                // Report failure to health monitor
                if let Some(ref health) = self.health_handle {
                    health.report_update_failure();
                }

                Err(error)
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

    #[test]
    fn test_scheduler_creation() {
        let config = Config {
            compose_paths: vec![PathBuf::from("./test")],
            schedule: "0 0 2 * * *".to_string(),
            registries: HashMap::new(),
            update_strategy: UpdateStrategy::LatestPatchOfPreviousMinor,
            ignore_images: vec![],
            dry_run: true,
        };

        let scheduler = Scheduler::new(config, None).unwrap();
        assert!(scheduler
            .schedule
            .upcoming(chrono::Utc)
            .take(1)
            .next()
            .is_some());
    }

    #[test]
    fn test_cron_parsing() {
        let config = Config {
            compose_paths: vec![PathBuf::from("./test")],
            schedule: "0 30 1 * * *".to_string(), // 1:30 AM daily
            registries: HashMap::new(),
            update_strategy: UpdateStrategy::LatestPatchOfPreviousMinor,
            ignore_images: vec![],
            dry_run: true,
        };

        let scheduler = Scheduler::new(config, None).unwrap();

        // Just verify the scheduler can be created
        assert!(scheduler
            .schedule
            .upcoming(chrono::Utc)
            .take(1)
            .next()
            .is_some());
    }
}
