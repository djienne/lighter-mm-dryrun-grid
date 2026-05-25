# syntax=docker/dockerfile:1.7

FROM rust:1-slim-bookworm AS builder

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential ca-certificates libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release \
    && mkdir -p /out \
    && cp /app/target/release/lighter-mm-dryrun /out/lighter-mm-dryrun

FROM debian:bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /out/lighter-mm-dryrun /usr/local/bin/lighter-mm-dryrun
COPY config.json grid_config.json /app/config/
RUN mkdir -p /app/logs

ENV LOG_DIR=/app/logs
ENV RUST_LOG=info

ENTRYPOINT ["lighter-mm-dryrun"]
CMD ["--symbol", "BTC", "--config", "/app/config/config.json", "--grid", "/app/config/grid_config.json"]
