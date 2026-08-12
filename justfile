# RabbitHole — task runner.
#
# The workspace builds four server-side binaries plus a wasm SPA, and running
# "the server side" has meant three hand-typed commands from three different
# documents. `just up` is the one command.
#
#   just            list every recipe
#   just up         build everything and run the whole stack (burrow + SPA + tracker)
#   just burrow     just the burrow, serving the web client
#   just tracker    just the Looking Glass tracker
#   just check      what CI checks, before you push
#
# Requires: cargo. `just web` additionally needs trunk + the wasm target;
# `just deps` installs both.

set shell := ["bash", "-uc"]

# Where the burrow keeps its db, blobs, identity and ctl socket.
data_dir := env_var_or_default("RABBITHOLE_DATA_DIR", "./burrow-data")
# The tracker's status port (INDEX/HEALTH), matching apps/tui's default.
tracker_status := env_var_or_default("RABBIT_TRACKER_STATUS", "0.0.0.0:4655")
# Where `just up` serves the web client.
http_addr := env_var_or_default("RABBITHOLE_HTTP_ADDR", "0.0.0.0:8080")

# List the recipes (default).
default:
    @just --list --unsorted

# Install the toolchain bits the SPA needs. Idempotent.
deps:
    rustup target add wasm32-unknown-unknown
    cargo install trunk --locked

# Build the wasm SPA into crates/ui-web/dist.
web:
    cd crates/ui-web && trunk build --release

# Build every server-side binary (release).
build:
    cargo build --release -p burrow -p rabbit -p rabbit-tui -p looking-glass

# Run the whole server side: burrow (QUIC + WebSocket + web client) and the
# Looking Glass tracker, together, with Ctrl-C stopping both.
#
# Deliberately not a supervisor: two background jobs and a `wait` is the
# entire requirement, and a process manager would be a subsystem to maintain.
up: build web
    #!/usr/bin/env bash
    set -uo pipefail
    echo "burrow   → quic 4653 · ws 4654 · web {{http_addr}}"
    echo "tracker  → status {{tracker_status}}"
    echo "data     → {{data_dir}}"
    echo
    ./target/release/burrow --data-dir "{{data_dir}}" \
        --http --http-addr "{{http_addr}}" --web-root crates/ui-web/dist run &
    burrow_pid=$!
    ./target/release/looking-glass --status "{{tracker_status}}" &
    tracker_pid=$!
    # One Ctrl-C stops the stack, not just whichever job had the terminal.
    trap 'kill $burrow_pid $tracker_pid 2>/dev/null' INT TERM
    wait -n $burrow_pid $tracker_pid
    kill $burrow_pid $tracker_pid 2>/dev/null
    wait

# Run only the burrow, serving the web client.
burrow: build web
    ./target/release/burrow --data-dir "{{data_dir}}" \
        --http --http-addr "{{http_addr}}" --web-root crates/ui-web/dist run

# Run only the tracker.
tracker: build
    ./target/release/looking-glass --status "{{tracker_status}}"

# The dev loop for the web client: the SPA with seeded demo burrows, on 1420.
dev-web:
    cd crates/ui-web && trunk serve --address 127.0.0.1 --port 1420 --features demo

# Create an account on a running burrow: `just account alice hunter2`
account login password:
    ./target/release/burrow ctl --data-dir "{{data_dir}}" account-create "{{login}}" "{{password}}"

# Everything CI checks.
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    cargo check -p rabbithole-ui-web --target wasm32-unknown-unknown

test:
    cargo test --workspace

fmt:
    cargo fmt --all

# Stage release archives for the host target into dist/.
release:
    scripts/release.sh

# Build the distro packages (deb + apk + rpm) for the host arch into dist/.
# Needs nfpm: https://nfpm.goreleaser.com
package: build
    scripts/package.sh

# Build and run the full stack in containers.
docker-up:
    docker compose up --build -d

docker-down:
    docker compose down
