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
# Both server-side binaries: the compose stack runs the tracker from this
# same image (different entrypoint), so building only `burrow` would mean a
# second image for one extra binary.
RUN cargo build --release -p burrow -p looking-glass \
    && strip target/release/burrow target/release/looking-glass

# --- Web stage: build the SPA the burrow serves ------------------------------
# Separate stage so it caches independently of the server build, and so a
# change to the Rust server doesn't rebuild wasm (or vice versa). Without this
# the image would ship a server that serves nothing at `/`, and
# `docker compose up` would not actually give you the whole thing.
FROM rust:slim AS web
WORKDIR /src
RUN rustup target add wasm32-unknown-unknown \
    && cargo install trunk --locked
COPY . .
RUN cd crates/ui-web && trunk build --release

# --- Runtime stage: minimal image with the server binaries -------------------
FROM debian:stable-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system burrow \
    && useradd --system --gid burrow --home-dir /data --shell /usr/sbin/nologin burrow \
    && mkdir -p /data \
    && chown burrow:burrow /data

COPY --from=builder /src/target/release/burrow /usr/local/bin/burrow
COPY --from=builder /src/target/release/looking-glass /usr/local/bin/looking-glass
# The built web client. `--web-root /srv/web` (see docker-compose.yml) serves it.
COPY --from=web /src/crates/ui-web/dist /srv/web

USER burrow
WORKDIR /data
VOLUME ["/data"]

ENV RABBITHOLE_DATA_DIR=/data

# QUIC (primary, UDP) and optional S2S federation. The plaintext WebSocket
# backend is loopback-only and intentionally not exposed by this image.
EXPOSE 4653/udp
EXPOSE 4655/tcp
EXPOSE 8080/tcp

ENTRYPOINT ["burrow"]
CMD ["run"]
