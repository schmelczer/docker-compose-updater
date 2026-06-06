FROM rust:1.85-alpine AS builder

RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

FROM alpine:3.21

RUN apk add --no-cache ca-certificates tzdata curl

RUN addgroup -g 1000 updater && \
    adduser -u 1000 -G updater -s /bin/sh -D updater

RUN mkdir -p /app/config && \
    chown -R updater:updater /app

COPY --from=builder /app/target/release/docker-compose-updater /usr/local/bin/docker-compose-updater

USER updater
WORKDIR /app
EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

CMD ["docker-compose-updater", "--config", "/app/config/config.yaml", "start"]
