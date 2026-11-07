use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::{info, Level};

use docker_compose_updater::{config::Config, health::Health, scheduler::Scheduler};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Path to the configuration file
    #[arg(short, long)]
    config: PathBuf,

    /// Verbose output (-v, -vv, -vvv for increasing verbosity)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the updater service
    Start,
    /// Run a one-time update
    Update,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let log_level = match cli.verbose {
        0 => Level::INFO,
        1 => Level::DEBUG,
        _ => Level::TRACE,
    };

    tracing_subscriber::fmt()
        .with_max_level(log_level)
        .with_timer(tracing_subscriber::fmt::time::SystemTime)
        .with_target(false)
        .init();
    let config = Config::load(cli.config)?;

    info!(
        "Starting Docker Compose Updater v{}",
        env!("CARGO_PKG_VERSION")
    );

    match cli.command {
        Commands::Start => {
            let health = Health::default();
            let scheduler = Scheduler::new(config, Some(health.clone()))?;
            tokio::try_join!(health.start(), scheduler.start())?;
        }
        Commands::Update => {
            let scheduler = Scheduler::new(config, None)?;
            scheduler.run_once().await?;
        }
    }

    Ok(())
}
