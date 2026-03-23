# Docker Compose Updater

A robust, production-ready Rust service that automatically updates Docker Compose image versions whilst preserving comments, formatting, and maintaining operational stability. The service operates on a configurable schedule and uses intelligent update strategies to ensure safe, conservative updates.

## Key Features

- 🔄 **Intelligent Updates**: Automatically updates Docker Compose files using configurable update strategies
- 📝 **Format Preservation**: Maintains all comments, whitespace, and YAML formatting perfectly
- 🎯 **Conservative Strategies**: Defaults to stable, tested versions rather than bleeding edge
- 🏥 **Health Monitoring**: Built-in health endpoints for monitoring and observability
- 🔐 **Multi-Registry Support**: Works with Docker Hub, GitHub Container Registry, GitLab, and custom registries
- 🔍 **Flexible Filtering**: Configurable patterns to ignore specific images or registries
- 🛡️ **Robust Error Handling**: Comprehensive error management with no silent failures
- 📋 **Prefix/Suffix Support**: Handles complex versioning schemes like `v1.2.3-alpine`, `release-1.0.0-slim`
- 🚀 **Container-Ready**: Designed for containerised environments with proper security
- 📊 **Comprehensive Testing**: Extensively tested with 44+ test cases covering edge cases

## Quick Start

### Using Docker Compose (Recommended)

1. **Create a configuration file** (`config.yaml`):
```yaml
# Paths to scan for Docker Compose files
compose_paths:
  - "./compose-files"
  - "./docker-compose.yml"
  - "./services"

# Cron expression for update schedule
schedule: "0 0 2 * * *"  # Daily at 2 AM UTC

# Update strategy (conservative by default)
update_strategy: "LatestPatchOfPreviousMinor"

# Runtime behaviour
dry_run: false  # Set to true for testing

# Images to ignore during updates (substring matching)
ignore_images:
  - "localhost"
  - "127.0.0.1"
  - "internal.company.com"

# Registry configurations
registries:
  "docker.io":
    url: "https://registry-1.docker.io"
  
  "ghcr.io":
    url: "https://ghcr.io"
    auth_token: "${GITHUB_TOKEN}"
```

2. **Deploy with Docker Compose**:
```yaml
version: '3.8'
services:
  docker-compose-updater:
    image: ghcr.io/your-username/docker-compose-updater:latest
    container_name: docker-compose-updater
    restart: unless-stopped
    environment:
      - GITHUB_TOKEN=${GITHUB_TOKEN}
      - RUST_LOG=info
    volumes:
      # Mount Docker socket (read-only for security)
      - /var/run/docker.sock:/var/run/docker.sock:ro
      # Mount compose files directory
      - ./compose-files:/app/compose-files:rw
      # Mount configuration
      - ./config.yaml:/app/config/config.yaml:ro
    ports:
      - "8080:8080"  # Health monitoring port
    user: "1000:1000"  # Run as non-root user
```

3. **Start the service**:
```bash
docker-compose up -d
```

### Command Line Usage

```bash
# Install or build the binary
cargo build --release

# Run a one-time update
./target/release/docker-compose-updater --config ./config.yaml update

# Start the scheduled service with health monitoring
./target/release/docker-compose-updater --config ./config.yaml start
```

## Update Strategies

The service supports three intelligent update strategies, defaulting to the most conservative:


### `LatestPatchOfPreviousMinor`
- **Predictable**: Always updates to the latest patch of the previous minor version
- **Consistent Behaviour**: Never updates to the current latest minor
- **Example**: `1.5.3` → `1.4.8`

### `Latest`
- **Aggressive**: Updates to the absolute latest available version
- **Cutting Edge**: Useful for development environments
- **Example**: `1.5.3` → `2.1.0`

## Advanced Version Handling

The service handles complex versioning schemes intelligently:

### Supported Version Formats
- **Standard Semantic Versions**: `1.2.3`, `2.0.1`
- **Prefixed Versions**: `v1.2.3`, `release-1.0.0`, `build123-2.1.0`
- **Suffixed Versions**: `1.2.3-alpine`, `2.0.1-slim`, `1.5.0-ubuntu20.04`
- **Combined**: `v1.2.3-alpine-slim`, `release-2.0.1-final`

### Version Matching Logic
The service ensures that prefix and suffix combinations remain consistent:
- `v1.2.3-alpine` will only be updated to other `v*.*.*-alpine` versions
- `release-1.0.0` will only be updated to other `release-*.*.*` versions
- This prevents incompatible image variants from being selected

## Comprehensive Registry Support

### Supported Registries
- **Docker Hub**: `docker.io` (default registry)
- **GitHub Container Registry**: `ghcr.io`
- **GitLab Container Registry**: `registry.gitlab.com`
- **Google Container Registry**: `gcr.io`
- **Amazon ECR**: `*.dkr.ecr.*.amazonaws.com`
- **Azure Container Registry**: `*.azurecr.io`
- **Custom/Private Registries**: Any registry following Docker Registry API v2

### Registry Configuration
```yaml
registries:
  "docker.io":
    url: "https://registry-1.docker.io"
    # Docker Hub typically doesn't require auth for public images

  "ghcr.io":
    url: "https://ghcr.io"
    auth_token: "${GITHUB_TOKEN}"

  "registry.gitlab.com":
    url: "https://registry.gitlab.com"
    auth_token: "${GITLAB_TOKEN}"

  "your-registry.company.com":
    url: "https://your-registry.company.com"
    auth_token: "${COMPANY_REGISTRY_TOKEN}"
```

## Supported Image Formats

The updater handles various image naming conventions:

```yaml
# Standard Docker Hub images
nginx:1.21.0 → nginx:1.20.6
ubuntu:20.04 → ubuntu:18.04

# Namespaced images
bitnami/nginx:1.21.0 → bitnami/nginx:1.20.6
library/postgres:13.7 → library/postgres:12.11

# Custom registries
ghcr.io/user/app:v1.0.0 → ghcr.io/user/app:v0.9.5
registry.example.com/team/service:2.1.3 → registry.example.com/team/service:2.0.9

# Complex versioning
nginx:1.21.0-alpine → nginx:1.20.6-alpine
postgres:13.7-bullseye → postgres:12.11-bullseye
redis:v7.0.0-alpine3.16 → redis:v6.2.7-alpine3.16

# Local development (typically ignored)
localhost:5000/myapp:latest → [ignored by default]
127.0.0.1:8080/service:dev → [ignored by default]
```

## Health Monitoring & Observability

The service provides comprehensive monitoring capabilities:

### Health Endpoints
- **`GET /`**: Basic health check
  - Returns `{"status":"healthy"}` when operational
  - Returns `{"status":"unhealthy"}` during failures
  - Includes basic service information

### Logging
```bash
# Configure logging level via environment variable
RUST_LOG=debug  # For detailed debugging
RUST_LOG=info   # For operational information (default)
RUST_LOG=warn   # For warnings and errors only
RUST_LOG=error  # For errors only
```

### Service Monitoring
The service reports:
- Successful update cycles with file counts
- Failed updates with detailed error information
- Registry connectivity status
- Configuration validation results

## Security Features

- **Non-Root Execution**: Designed to run as non-root user in containers
- **Minimal Attack Surface**: Alpine Linux base image with minimal packages
- **Read-Only Docker Socket**: Only requires read access to Docker socket
- **Secret Management**: All tokens provided via environment variables
- **Input Validation**: Comprehensive validation of all configuration inputs
- **Error Sanitisation**: No sensitive information leaked in error messages

## Development

### Building from Source

```bash
# Clone the repository
git clone https://github.com/your-username/docker-compose-updater.git
cd docker-compose-updater

# Build in release mode
cargo build --release

# Run comprehensive test suite
cargo test

# Run with custom configuration
./target/release/docker-compose-updater --config ./config.example.yaml start
```

### Testing

The project includes extensive testing:

```bash
# Run all tests (44+ test cases)
cargo test

# Run specific test suites
cargo test --test integration_tests       # Integration tests
cargo test --test test_error_handling     # Error handling tests
cargo test --test test_config_validation  # Configuration tests
cargo test --test test_compose_operations # Compose file operations

# Run with output for debugging
cargo test -- --nocapture

# Generate coverage report (requires cargo-llvm-cov)
cargo llvm-cov --html
```

### Test Coverage
- **Error Handling**: Invalid configurations, malformed files, network failures
- **Config Validation**: All configuration formats, edge cases, defaults
- **Compose Operations**: Complex file formats, image parsing, version handling
- **Registry Integration**: Multiple registry types, authentication, failures
- **Version Logic**: All update strategies, complex versioning schemes

## Configuration Reference

### Complete Configuration Example
```yaml
# File/directory paths to scan for Docker Compose files
compose_paths:
  - "/app/compose-files"      # Directory to scan recursively
  - "./docker-compose.yml"    # Specific file
  - "./services/"             # Another directory

# Cron expression for update schedule (UTC timezone)
schedule: "0 0 2 * * *"  # Daily at 2 AM

# Update strategy selection
update_strategy: "LatestPatchOfPreviousMinor"

# Prevent actual file modifications (for testing)
dry_run: false

# Images to ignore during updates (substring matching)
ignore_images:
  - "localhost"               # Local development images
  - "127.0.0.1"              # Local registry
  - "internal.company.com"    # Private internal registry
  - "snapshot"               # Snapshot versions

# Registry configurations
registries:
  "docker.io":
    url: "https://registry-1.docker.io"
    # auth_token not typically needed for public images

  "ghcr.io":
    url: "https://ghcr.io"
    auth_token: "${GITHUB_TOKEN}"

  "registry.gitlab.com":
    url: "https://registry.gitlab.com"
    auth_token: "${GITLAB_TOKEN}"
```

### Environment Variables
- **`GITHUB_TOKEN`**: Personal access token for GitHub Container Registry
- **`GITLAB_TOKEN`**: Access token for GitLab Container Registry
- **`RUST_LOG`**: Logging level (`debug`, `info`, `warn`, `error`)
- **Registry-specific tokens**: For private registries

## Operational Considerations

### Backup Strategy
- Always backup your Docker Compose files before running updates
- Use version control (Git) to track changes
- Test with `dry_run: true` before enabling actual updates

### Monitoring
- Monitor the health endpoint for service availability
- Set up alerts for failed update cycles
- Review logs regularly for registry connectivity issues

### Resource Requirements
- **Memory**: ~10-50MB during operation
- **CPU**: Minimal during dormant periods, brief spikes during updates
- **Network**: Requires outbound HTTPS access to configured registries
- **Storage**: Minimal, only for configuration and temporary files

## Troubleshooting

### Common Issues

**Service fails to start**
- Check configuration file syntax with `yamllint`
- Verify file paths exist and are accessible
- Ensure cron expression is valid

**Registry authentication failures**
- Verify environment variables are set correctly
- Check token permissions for the target registry
- Test registry connectivity manually

**Updates not applied**
- Check if `dry_run` is enabled
- Verify images are not in the ignore list
- Ensure semantic versioning is used in image tags

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Write tests for your changes
4. Ensure all tests pass (`cargo test`)
5. Commit your changes (`git commit -m 'Add amazing feature'`)
6. Push to the branch (`git push origin feature/amazing-feature`)
7. Open a Pull Request

## Licence

This project is licensed under the MIT Licence - see the [LICENSE](LICENSE) file for details.

## Changelog

### v0.1.0
- Initial release with comprehensive functionality
- Multi-registry support (Docker Hub, GHCR, GitLab, custom)
- Intelligent update strategies with conservative defaults
- Comment and formatting preservation
- Comprehensive error handling (no silent failures)
- Health monitoring and observability
- Extensive test coverage (44+ test cases)
- Prefix/suffix version handling
- Robust configuration validation
- Production-ready security features