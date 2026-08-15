#!/usr/bin/env bash
#
# apps/desktop is its own cargo workspace (wry/webkit must not join
# `cargo build --workspace`), so it cannot inherit version.workspace.
# Fail if its crate / Tauri / iOS stamps drifted from the product version.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

quoted() {
  sed -nE "s/.*$1\"([^\"]+)\".*/\1/p" "$2" | head -1
}

workspace="$(quoted 'version = ' "$ROOT/Cargo.toml")"
desktop="$(quoted 'version = ' "$ROOT/apps/desktop/Cargo.toml")"
tauri="$(quoted '"version": ' "$ROOT/apps/desktop/tauri.conf.json")"
plist="$(sed -n '/CFBundleShortVersionString/{n;s/.*<string>\([^<]*\)<\/string>.*/\1/p;}' \
  "$ROOT/apps/desktop/gen/apple/rabbithole-desktop_iOS/Info.plist" | head -1)"
# project.yml uses unquoted and quoted forms.
yml="$(sed -nE 's/.*CFBundleShortVersionString: "?([^"]+)"?.*/\1/p' \
  "$ROOT/apps/desktop/gen/apple/project.yml" | head -1)"

if [ -z "$workspace" ]; then
  echo "error: could not read workspace version from Cargo.toml" >&2
  exit 1
fi

fail=0
check() {
  local name="$1" got="$2"
  if [ "$got" != "$workspace" ]; then
    echo "error: $name is $got, workspace is $workspace" >&2
    fail=1
  fi
}

check "apps/desktop/Cargo.toml" "$desktop"
check "apps/desktop/tauri.conf.json" "$tauri"
check "iOS Info.plist" "$plist"
check "apple project.yml" "$yml"

if [ "$fail" -ne 0 ]; then
  echo "bump the desktop stamps with the product version (isolated workspace)." >&2
  exit 1
fi
