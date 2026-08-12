#!/usr/bin/env bash
#
# Regenerate apps/desktop/icons/icon.icns from the brand master, with the
# macOS squircle and margin baked in (see make-macos-icon.swift for why that
# is necessary rather than cosmetic). Idempotent; stock macOS tools only.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/brand/rabbithole-logo-1024.png"
OUT="$ROOT/apps/desktop/icons/icon.icns"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

swift "$ROOT/scripts/make-macos-icon.swift" "$SRC" "$WORK/master.png"

mkdir -p "$WORK/icon.iconset"
while IFS=: read -r px name; do
  sips -s format png -z "$px" "$px" "$WORK/master.png" --out "$WORK/icon.iconset/$name.png" >/dev/null
done <<'EOF'
16:icon_16x16
32:icon_16x16@2x
32:icon_32x32
64:icon_32x32@2x
128:icon_128x128
256:icon_128x128@2x
256:icon_256x256
512:icon_256x256@2x
512:icon_512x512
1024:icon_512x512@2x
EOF

iconutil -c icns "$WORK/icon.iconset" -o "$OUT"
echo "wrote $OUT ($(stat -f%z "$OUT") bytes)"
