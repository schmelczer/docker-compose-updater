FROM rust:1.85-alpine AS builder

RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

# Build the application
RUN cargo build --release

# Runtime stage
FROM alpine:latest

# Install runtime dependencies
RUN apk add --no-cache \
    ca-certificates \
    tzdata \
    curl

# Create non-root user
RUN addgroup -g 1000 updater && \
    adduser -u 1000 -G updater -s /bin/sh -D updater

# Create necessary directories
RUN mkdir -p /app/config && \
    chown -R updater:updater /app

# Copy binary from builder stage
COPY --from=builder /app/target/release/docker-compose-updater /usr/local/bin/docker-compose-updater

# Switch to non-root user
USER updater

# Set working directory
WORKDIR /app

# Expose health check port
EXPOSE 8080

# Health check
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

CMD ["docker-compose-updater", "--config", "/app/config/config.yaml", "start"]
