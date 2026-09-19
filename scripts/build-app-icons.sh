#!/usr/bin/env bash
#
# Regenerate every app icon under apps/desktop/icons that is NOT the Mac one,
# from the brand masters in brand/. These are what a process wears outside
# macOS: icon.ico is embedded in the Windows executable (taskbar, Alt-Tab,
# Task Manager), icon.png and the sized PNGs are the Linux window and launcher
# icon, and ios/ and android/ are the home-screen icons.
#
# The Mac icon is a different drawing (squircle plus the system margin, see
# make-macos-icon.swift) and has its own script: build-macos-icns.sh. This one
# never touches icon.icns or about.png.
#
# Idempotent. Needs the tauri CLI (`cargo tauri`) and python3.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SVG="$ROOT/brand/rabbithole-logo.svg"
PNG="$ROOT/brand/rabbithole-logo-1024.png"
OUT="$ROOT/apps/desktop/icons"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/desktop" "$WORK/mobile"
cp "$PNG" "$WORK/desktop/master.png"

# A phone masks an icon to its own shape, so the finished tile (rounded, on
# transparent) would show a sliver of backing colour at every corner. Phones
# get the drawing full-bleed instead. Android goes further and draws two
# layers, masked to whatever the launcher likes: the gradient behind, the hole
# on its own in front. All three come out of the one SVG, so they cannot drift.
python3 - "$SVG" "$WORK/mobile" <<'PY'
import re, sys
src, work = open(sys.argv[1]).read(), sys.argv[2]
head = src[: src.index("</defs>") + len("</defs>")]
tile = re.search(r'<rect [^>]*fill="url\(#tile\)"/>', src).group(0)
hole = re.search(r'<g filter="url\(#shadow\)">.*?</g>', src, re.S).group(0)
square = re.sub(r'\srx="[^"]*"', "", tile)
assert square != tile, "the tile lost its rx: check the brand SVG"
open(f"{work}/master.svg", "w").write(f"{head}\n{square}\n{hole}\n</svg>\n")
open(f"{work}/android-bg.svg", "w").write(f"{head}\n{square}\n</svg>\n")
open(f"{work}/android-fg.svg", "w").write(f"{head}\n{hole}\n</svg>\n")
PY

# 85% keeps the hole inside Android's safe zone. bg_color is what shows through
# transparency on iOS; there is none left, the brand blue is a belt to the braces.
cat > "$WORK/mobile/manifest.json" <<'JSON'
{
  "default": "master.svg",
  "bg_color": "#6c9cff",
  "android_bg": "android-bg.svg",
  "android_fg": "android-fg.svg",
  "android_fg_scale": 85
}
JSON

cd "$ROOT/apps/desktop"
cargo tauri icon "$WORK/desktop/master.png" --output "$WORK/desktop/out" >/dev/null
cargo tauri icon "$WORK/mobile/manifest.json" --output "$WORK/mobile/out" >/dev/null

# Desktop: the finished tile. The generic .icns has no squircle and no margin,
# and the Mac icon is not ours to write.
rm -rf "$WORK/desktop/out/icon.icns" "$WORK/desktop/out/ios" "$WORK/desktop/out/android"
cp -R "$WORK/desktop/out/." "$OUT/"
# Phones: full-bleed.
rm -rf "$OUT/ios" "$OUT/android"
cp -R "$WORK/mobile/out/ios" "$WORK/mobile/out/android" "$OUT/"
# The generated Xcode project keeps its own copy of the iOS set, and that copy
# is the one an iOS build reads.
XCSET="$ROOT/apps/desktop/gen/apple/Assets.xcassets/AppIcon.appiconset"
if [[ -d "$XCSET" ]]; then
  cp "$OUT"/ios/*.png "$XCSET/"
fi
echo "wrote the Windows, Linux, iOS and Android icons under $OUT (icon.icns and about.png untouched)"
