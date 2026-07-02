# syntax=docker/dockerfile:1

# --- Build stage: compile the burrow server (release) -------------------------
FROM rust:slim AS builder
WORKDIR /src

# Build dependencies. rustls-based transports need no OpenSSL, but pkg-config
# and ca-certificates keep the build portable across the workspace.
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY . .

# Compile only the server binary and its dependency tree, then strip it.
RUN cargo build --release -p burrow \
    && strip target/release/burrow

# --- Runtime stage: minimal image with just burrow ---------------------------
FROM debian:stable-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system burrow \
    && useradd --system --gid burrow --home-dir /data --shell /usr/sbin/nologin burrow \
    && mkdir -p /data \
    && chown burrow:burrow /data

COPY --from=builder /src/target/release/burrow /usr/local/bin/burrow

USER burrow
WORKDIR /data
VOLUME ["/data"]

ENV RABBITHOLE_DATA_DIR=/data

# QUIC (primary, UDP), WebSocket (fallback, TCP). 4655 reserved for a
# co-located looking-glass tracker status listener.
EXPOSE 4653/udp
EXPOSE 4654/tcp
EXPOSE 4655/tcp

ENTRYPOINT ["burrow"]
CMD ["run"]
