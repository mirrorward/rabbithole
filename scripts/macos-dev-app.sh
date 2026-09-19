#!/usr/bin/env bash
#
# Cargo's runner for the desktop shell on a Mac (apps/desktop/.cargo/config.toml):
# run the app from inside a real .app, so a dev run is RabbitHole to the system
# and not a loose executable.
#
# macOS takes an app's icon, name and identity from its bundle. A bare binary
# has none, so the Dock, Command-Tab, Force Quit, Activity Monitor and every
# other process list draw the generic "exec" tile for it, whatever the window
# looks like. (Tauri repaints the Dock tile once the app is up. Nothing else
# sees that.) A bundle around the dev binary fixes all of them at once and
# costs nothing: the binary is hard-linked in, not copied.
#
# Usage, as cargo calls it: macos-dev-app.sh <binary> [args...]
# Set RH_NO_DEV_BUNDLE=1 to run the bare binary instead.
set -euo pipefail

BIN="$1"
shift

# Only the app itself gets a bundle. Test and bench harnesses (their names
# carry a hash) and anything that is not the app run exactly as cargo built them.
if [[ "$(uname -s)" != Darwin || "$(basename "$BIN")" != RabbitHole || "${RH_NO_DEV_BUNDLE:-}" == 1 ]]; then
  exec "$BIN" "$@"
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DESKTOP="$ROOT/apps/desktop"
CONF="$DESKTOP/tauri.conf.json"
BIN="$(cd "$(dirname "$BIN")" && pwd)/RabbitHole"
APP="$(dirname "$BIN")/RabbitHole.app"

# The same identity the shipped app has: it is the same app.
IDENT="$(plutil -extract identifier raw -o - "$CONF")"
VERSION="$(plutil -extract version raw -o - "$CONF")"

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
ln -f "$BIN" "$APP/Contents/MacOS/RabbitHole" 2>/dev/null || cp -f "$BIN" "$APP/Contents/MacOS/RabbitHole"
cp -f "$DESKTOP/icons/icon.icns" "$APP/Contents/Resources/icon.icns"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>RabbitHole</string>
  <key>CFBundleDisplayName</key><string>RabbitHole</string>
  <key>CFBundleExecutable</key><string>RabbitHole</string>
  <key>CFBundleIdentifier</key><string>$IDENT</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>10.15</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
# A changed bundle is only re-read when its folder looks newer.
touch "$APP"

# WebKit files a bare process's storage under its name, and a bundle's under its
# identifier. Carry the old store over once, so the first bundled run still has
# the person's recent burrows, bookmarks, settings and identity. Copy, never
# move, and never over something that is already there.
OLD="$HOME/Library/WebKit/RabbitHole"
NEW="$HOME/Library/WebKit/$IDENT"
if [[ -d "$OLD" && ! -e "$NEW" ]]; then
  cp -R "$OLD" "$NEW"
  echo "[rh-dev-app] carried the dev run's web storage over to $NEW" >&2
fi

exec "$APP/Contents/MacOS/RabbitHole" "$@"
