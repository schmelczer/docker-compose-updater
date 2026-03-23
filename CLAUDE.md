# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Development Commands

### Build and Test
```bash
# Build the project
cargo build --release

# Run all tests
cargo test

# Run integration tests specifically
cargo test --test integration

# Run with coverage
cargo test --coverage
```

### Running the Application
```bash
# Run a one-time update
./target/release/docker-compose-updater --config ./config.yaml update

# Start the scheduler service
./target/release/docker-compose-updater --config ./config.yaml start

# Run from source in development
cargo run -- --config ./config.yaml update
cargo run -- --config ./config.yaml start
```

### Development Testing
```bash
# Test against example configuration
cargo run -- --config ./config.example.yaml update

# Run in dry-run mode to see what would be updated
cargo run -- --config ./demo-config.yaml update  # (demo-config.yaml has dry_run: true)
```

## Architecture Overview

This is a Rust-based Docker Compose updater service that automatically updates image versions in Docker Compose files while preserving formatting and comments.

### Core Components

- **`main.rs`**: Entry point with CLI argument parsing using clap. Supports `start` (scheduler) and `update` (one-time) commands.
- **`config.rs`**: Configuration management with support for multiple registries, update strategies, and image ignore patterns. Loads from `config.yaml` or `/app/config/config.yaml`.
- **`compose.rs`**: Docker Compose file parsing and updating. Uses regex-based YAML parsing to preserve formatting and comments while updating image versions.
- **`registry.rs`**: Container registry interaction for fetching available image versions. Supports Docker Hub, GitHub Container Registry, GitLab, and custom registries.
- **`scheduler.rs`**: Cron-based scheduling system for automatic updates. Runs health checks and reports status.
- **`health.rs`**: Health monitoring server that exposes HTTP endpoints for service health checks.

### Key Design Patterns

- **Configuration-driven**: All behavior controlled through `config.yaml` including paths, schedules, registries, and update strategies.
- **Preserves formatting**: Uses line-by-line regex parsing rather than full YAML parsing to maintain comments and formatting.
- **Flexible update strategies**: Supports different version update approaches (latest patch of previous minor, latest patch, latest minor, latest).
- **Registry abstraction**: Generic registry client that works with multiple container registries.
- **Health monitoring**: Built-in health server for monitoring service status.

### Update Strategies

The system supports different update strategies defined in `config.rs`:
- `LatestPatchOfPreviousMinor` (default)
- `Latest`

### Configuration

- Configuration is loaded from `config.yaml` or `/app/config/config.yaml`
- Default configuration includes Docker Hub and GitHub Container Registry
- Environment variables like `GITHUB_TOKEN` are used for registry authentication
- Images can be ignored using patterns in the `ignore_images` configuration

### Testing

- Unit tests are embedded in each module using `#[cfg(test)]`
- Integration tests are in the `tests/` directory
- Uses `tempfile` for testing file operations
- Tests cover compose file parsing, image reference parsing, and scheduler creation