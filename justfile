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
#   just tui        terminal client, with the local glass port just up wrote
#   just desktop    Tauri shell, same local glass port
#   just check      what CI checks, before you push
#
# Requires: cargo. `just web` additionally needs trunk + the wasm target;
# `just deps` installs both.

set shell := ["bash", "-uc"]

# Where the burrow keeps its db, blobs, identity and ctl socket.
data_dir := env_var_or_default("RABBITHOLE_DATA_DIR", "./burrow-data")
# The tracker's status port (INDEX/HEALTH) for the local stack.
# Not 4655: that is the burrow's default `federation_addr`, and `just up`
# would lose the fight if burrow.toml enables Tunnels. The public
# looking-glass still defaults to 4655; this is only the side-by-side recipe.
# Override with RABBIT_TRACKER_STATUS.
tracker_status := env_var_or_default("RABBIT_TRACKER_STATUS", "0.0.0.0:5497")
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

# Persist the local INDEX bind (git toplevel + ~/.rabbithole) so a TUI
# started outside the repo still finds it. Cleared when the stack stops.
[private]
write-tracker-status:
    scripts/local-tracker-status.sh write "{{tracker_status}}"

[private]
clear-tracker-status:
    scripts/local-tracker-status.sh clear

up: build web write-tracker-status
    #!/usr/bin/env bash
    set -uo pipefail
    echo "burrow   → quic 4653 · ws 4654 · web {{http_addr}}"
    echo "tracker  → status {{tracker_status}}"
    bind='{{tracker_status}}'
    echo "clients  → 127.0.0.1:${bind##*:}  (public glass stays :4655)"
    echo "status   → $(scripts/local-tracker-status.sh print)"
    echo "data     → {{data_dir}}"
    echo
    cleanup() {
      kill $burrow_pid $tracker_pid 2>/dev/null
      scripts/local-tracker-status.sh clear
    }
    ./target/release/burrow --data-dir "{{data_dir}}" \
        --http --http-addr "{{http_addr}}" --web-root crates/ui-web/dist run &
    burrow_pid=$!
    ./target/release/looking-glass --status "{{tracker_status}}" &
    tracker_pid=$!
    # One Ctrl-C stops the stack and drops the status files so a stale
    # bind is not what typed localhost follows.
    trap cleanup INT TERM
    wait -n $burrow_pid $tracker_pid
    cleanup
    wait

# Run only the burrow, serving the web client.
burrow: build web
    ./target/release/burrow --data-dir "{{data_dir}}" \
        --http --http-addr "{{http_addr}}" --web-root crates/ui-web/dist run

# Run only the tracker.
tracker: build write-tracker-status
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'scripts/local-tracker-status.sh clear' INT TERM EXIT
    ./target/release/looking-glass --status "{{tracker_status}}"

# Terminal client. The binary reads the status file (and probes it);
# we do not export a stale bind into the env.
tui: build
    ./target/release/rabbit-tui

# Desktop shell. Same status-file discovery as the TUI.
desktop:
    cd apps/desktop && cargo tauri dev

# The dev loop for the web client: the SPA with seeded demo burrows, on 1420.
dev-web:
    cd crates/ui-web && trunk serve --address 127.0.0.1 --port 1420 --features demo

# Create an account on a running burrow: `just account alice hunter2`
account login password:
    ./target/release/burrow ctl --data-dir "{{data_dir}}" account-create "{{login}}" "{{password}}"

# Everything CI checks.
check:
    scripts/check-desktop-version.sh
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
