#!/usr/bin/env bash
#
# stack.sh — run the whole server side of RabbitHole with one command.
#
# Starts the burrow (QUIC + WebSocket + the web client over HTTP) and the
# Looking Glass tracker together, and stops both on Ctrl-C. Running "the
# server side" previously meant three hand-typed commands from three different
# documents; this is the one command, and `just up` calls it.
#
# Deliberately not a supervisor: two background jobs and a `wait` is the whole
# requirement. A process manager here would be a subsystem to maintain.
#
#   scripts/stack.sh              build what's missing, then run
#   scripts/stack.sh --no-build   run what's already built
#   scripts/stack.sh --burrow     burrow only
#   scripts/stack.sh --tracker    tracker only
#
# Environment:
#   RABBITHOLE_DATA_DIR    burrow data dir           (default ./burrow-data)
#   RABBITHOLE_HTTP_ADDR   web client listener       (default 0.0.0.0:8080)
#   RABBIT_TRACKER_STATUS  tracker status listener   (default 0.0.0.0:4655)
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

DATA_DIR="${RABBITHOLE_DATA_DIR:-./burrow-data}"
HTTP_ADDR="${RABBITHOLE_HTTP_ADDR:-0.0.0.0:8080}"
TRACKER_STATUS="${RABBIT_TRACKER_STATUS:-0.0.0.0:4655}"
# Absolute on purpose: burrow resolves a *relative* --web-root under
# --data-dir (see apps/server/src/main.rs), so "crates/ui-web/dist" with a
# data dir elsewhere silently points at nothing and every page 404s.
WEB_ROOT="$ROOT/crates/ui-web/dist"

build=1
run_burrow=1
run_tracker=1
for arg in "$@"; do
  case "$arg" in
    --no-build) build=0 ;;
    --burrow)   run_tracker=0 ;;
    --tracker)  run_burrow=0 ;;
    -h|--help)  sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

if [ "$build" = 1 ]; then
  echo "==> building"
  cargo build --release -p burrow -p looking-glass || exit 1
  # The SPA is optional: the burrow serves whatever is in --web-root, and with
  # nothing there it still answers on QUIC and WebSocket. So a frontend build
  # that fails (missing trunk, a broken wasm-opt) must NOT take the server
  # down with it — warn, keep any previous dist, and carry on.
  if command -v trunk >/dev/null 2>&1; then
    if ! (cd crates/ui-web && trunk build --release); then
      echo "    warning: the web client failed to build — continuing without it"
      if [ -d "$WEB_ROOT" ]; then
        echo "             (serving the previously built $WEB_ROOT)"
      fi
    fi
  elif [ ! -d "$WEB_ROOT" ]; then
    echo "    note: trunk not installed and $WEB_ROOT is empty —"
    echo "          the burrow will run without the web client (\`just deps\` installs trunk)"
  fi
fi

pids=()
cleanup() {
  # One Ctrl-C stops the stack, not just whichever job holds the terminal.
  [ ${#pids[@]} -gt 0 ] && kill "${pids[@]}" 2>/dev/null
  wait 2>/dev/null
}
trap cleanup INT TERM EXIT

if [ "$run_burrow" = 1 ]; then
  args=(--data-dir "$DATA_DIR")
  if [ -d "$WEB_ROOT" ]; then
    args+=(--http --http-addr "$HTTP_ADDR" --web-root "$WEB_ROOT")
    echo "==> burrow    quic :4653 · ws :4654 · web http://$HTTP_ADDR"
  else
    echo "==> burrow    quic :4653 · ws :4654 (no web client built)"
  fi
  ./target/release/burrow "${args[@]}" run &
  pids+=($!)
fi

if [ "$run_tracker" = 1 ]; then
  echo "==> tracker   status $TRACKER_STATUS"
  ./target/release/looking-glass --status "$TRACKER_STATUS" &
  pids+=($!)
fi

if [ ${#pids[@]} -eq 0 ]; then
  echo "nothing to run" >&2
  exit 2
fi

echo "==> data      $DATA_DIR"
echo "    Ctrl-C stops everything."
echo
# Exit as soon as ANY component dies: a half-running stack that looks healthy
# is worse than one that stops and tells you.
wait -n "${pids[@]}"
status=$?
echo
echo "==> a component exited (status $status) — stopping the rest"
exit "$status"
