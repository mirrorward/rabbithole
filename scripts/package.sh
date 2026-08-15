#!/usr/bin/env bash
#
# package.sh — build Debian, Alpine and RPM packages from already-built
# release binaries, via nfpm.
#
# One config (packaging/nfpm.yaml) produces all three, so the file list,
# maintainer scripts and systemd units can't drift between formats — which is
# the failure mode of keeping a debian/ tree, an APKBUILD and a .spec in
# parallel.
#
#   scripts/package.sh                     # host arch
#   PKG_TARGET=x86_64-unknown-linux-musl scripts/package.sh
#
# Requires nfpm: https://nfpm.goreleaser.com/install
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if ! command -v nfpm >/dev/null 2>&1; then
  echo "nfpm not found — install it: https://nfpm.goreleaser.com/install" >&2
  exit 1
fi

PKG_VERSION="${PKG_VERSION:-$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')}"
PKG_TARGET="${PKG_TARGET:-$(rustc -vV | sed -n 's/^host: //p')}"

# nfpm names architectures the Go way; map from the Rust target triple.
case "$PKG_TARGET" in
  x86_64-*)  PKG_ARCH="amd64" ;;
  aarch64-*) PKG_ARCH="arm64" ;;
  armv7-*)   PKG_ARCH="arm7"  ;;
  *) echo "unmapped target $PKG_TARGET — add it to scripts/package.sh" >&2; exit 1 ;;
esac

# A cross build lands in target/<triple>/release; a plain host
# `cargo build --release` lands in target/release. Accept either rather than
# forcing --target on a host build.
if [ -x "target/$PKG_TARGET/release/burrow" ]; then
  PKG_BIN_DIR="target/$PKG_TARGET/release"
else
  PKG_BIN_DIR="target/release"
fi

for bin in burrow looking-glass rabbit rabbit-tui; do
  if [ ! -x "$PKG_BIN_DIR/$bin" ]; then
    echo "missing $PKG_BIN_DIR/$bin — run:" >&2
    echo "  cargo build --release -p burrow -p rabbit -p rabbit-tui -p looking-glass" >&2
    exit 1
  fi
done

mkdir -p dist

# Render the config rather than relying on nfpm's own ${VAR} expansion, which
# does not reach `contents[].src`. One substitution pass, one temp file, no
# guessing about which fields interpolate.
rendered="$(mktemp -t nfpm-rabbithole)"
trap 'rm -f "$rendered"' EXIT
sed -e "s|\${PKG_BIN_DIR}|$PKG_BIN_DIR|g" \
    -e "s|\${PKG_VERSION}|$PKG_VERSION|g" \
    -e "s|\${PKG_ARCH}|$PKG_ARCH|g" \
    packaging/nfpm.yaml > "$rendered"

echo "packaging rabbithole $PKG_VERSION ($PKG_ARCH) from $PKG_BIN_DIR"
for fmt in deb apk rpm; do
  nfpm pkg --config "$rendered" --packager "$fmt" --target dist/
done
ls -la dist/*.deb dist/*.apk dist/*.rpm 2>/dev/null || true
