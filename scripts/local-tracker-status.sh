#!/usr/bin/env bash
#
# Durable local Looking Glass bind: repo copy (git toplevel) plus a
# user-level copy so a TUI started outside the repo still finds it.
# Usage:
#   scripts/local-tracker-status.sh write 0.0.0.0:5497
#   scripts/local-tracker-status.sh clear
#   scripts/local-tracker-status.sh print   # first existing file path
#   scripts/local-tracker-status.sh bind    # first line of that file
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if git_root="$(git -C "$ROOT" rev-parse --show-toplevel 2>/dev/null)"; then
  ROOT="$git_root"
fi

repo_file="$ROOT/.rabbithole/looking-glass-status"
if [ -n "${XDG_STATE_HOME:-}" ]; then
  user_file="$XDG_STATE_HOME/rabbithole/looking-glass-status"
else
  user_file="${HOME}/.rabbithole/looking-glass-status"
fi

write() {
  local bind="$1"
  mkdir -p "$(dirname "$repo_file")" "$(dirname "$user_file")"
  printf '%s\n' "$bind" > "$repo_file"
  printf '%s\n' "$bind" > "$user_file"
}

clear() {
  rm -f "$repo_file" "$user_file"
}

print_path() {
  if [ -n "${RABBITHOLE_DATA_DIR:-}" ] && [ -f "$RABBITHOLE_DATA_DIR/.looking-glass-status" ]; then
    echo "$RABBITHOLE_DATA_DIR/.looking-glass-status"
    return
  fi
  if [ -f "$repo_file" ]; then
    echo "$repo_file"
    return
  fi
  if [ -f "$user_file" ]; then
    echo "$user_file"
  fi
}

cmd="${1:-}"
case "$cmd" in
  write)
    write "${2:?bind address}"
    ;;
  clear)
    clear
    ;;
  print)
    print_path
    ;;
  bind)
    path="$(print_path)"
    if [ -n "$path" ]; then
      tr -d '\r\n' < "$path"
      echo
    fi
    ;;
  *)
    echo "usage: $0 write <bind> | clear | print | bind" >&2
    exit 2
    ;;
esac
