#!/usr/bin/env bash
#
# release.sh — build all RabbitHole release binaries for the host target,
# checksum them, and stage a distributable archive into dist/.
#
# Usage: scripts/release.sh
set -euo pipefail

BINS=(burrow rabbit rabbit-tui looking-glass)

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
TARGET="$(rustc -vV | sed -n 's/^host: //p')"
DIST="$ROOT/dist"
PKG="rabbithole-${VERSION}-${TARGET}"
STAGE="$DIST/$PKG"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$@"
  else
    shasum -a 256 "$@"
  fi
}

echo "Building release binaries for ${TARGET} (v${VERSION})..."
build_args=()
for bin in "${BINS[@]}"; do
  build_args+=(-p "$bin")
done
cargo build --release "${build_args[@]}"

echo "Staging into ${STAGE}..."
rm -rf "$STAGE"
mkdir -p "$STAGE"
for bin in "${BINS[@]}"; do
  src="target/release/${bin}"
  if [ ! -f "$src" ]; then
    echo "error: expected binary not found: $src" >&2
    exit 1
  fi
  cp "$src" "$STAGE/"
  strip "$STAGE/${bin}" 2>/dev/null || true
done
cp README.md "$STAGE/" 2>/dev/null || true

archive="${PKG}.tar.gz"
(
  cd "$DIST"
  tar czf "$archive" "$PKG"
  sha256 "$archive" > "${archive}.sha256"
)

echo "Done:"
echo "  $DIST/$archive"
echo "  $DIST/${archive}.sha256"
sha256 "$DIST/$archive"
