#!/bin/bash
# Build "Claude Profiles.app": the Rust command plus its window.
#
#   scripts/build-app.sh            build into build/
#   scripts/build-app.sh --install  also copy it to ~/Applications and open it
#
# The app is the cross-platform GPUI window (gui/), as a normal Dock app,
# carrying the SwiftUI menu bar panel (macos/) as a helper app that the
# window starts too, so opening it gives both. MENUBAR=1 builds the menu bar
# panel on its own instead, as before.
#
# The command goes inside the app (Contents/Resources/claude-profiles), so the
# app needs nothing on PATH. The bundle is signed ad hoc, for this Mac only.
set -euo pipefail
cd "$(dirname "$0")/.."

APP="build/Claude Profiles.app"
MIN_MACOS=13.0

MENUBAR="${MENUBAR:-0}"

cargo build --release --workspace

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

if [[ "$MENUBAR" == 1 ]]; then
  swiftc -O -swift-version 5 -parse-as-library \
    -target "$(uname -m)-apple-macos$MIN_MACOS" \
    -o "$APP/Contents/MacOS/ClaudeProfiles" \
    macos/ClaudeProfilesApp.swift macos/Icon.swift
  BUNDLE_ID=dev.claude-profiles.menubar
  # Menu bar only: no Dock icon, no app menu.
  UI_ELEMENT="<key>LSUIElement</key><true/>"
else
  cp target/release/claude-profiles-gui "$APP/Contents/MacOS/ClaudeProfiles"
  BUNDLE_ID=dev.claude-profiles.app
  UI_ELEMENT=""
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

# The app icon, drawn by the same code as the menu bar icon.
swiftc -O -swift-version 5 -target "$(uname -m)-apple-macos$MIN_MACOS" \
  -o build/render-icon macos/Icon.swift macos/render-icon/main.swift
rm -rf build/AppIcon.iconset
build/render-icon build/AppIcon.iconset
iconutil -c icns -o "$APP/Contents/Resources/AppIcon.icns" build/AppIcon.iconset

cp target/release/claude-profiles "$APP/Contents/Resources/claude-profiles"

# The menu bar helper: its own small bundle, menu bar only, with its own copy
# of the command (it looks in its own Resources).
HELPER="$APP/Contents/Helpers/Claude Profiles Menu.app"
if [[ "$MENUBAR" != 1 ]]; then
  mkdir -p "$HELPER/Contents/MacOS" "$HELPER/Contents/Resources"
  swiftc -O -swift-version 5 -parse-as-library \
    -target "$(uname -m)-apple-macos$MIN_MACOS" \
    -o "$HELPER/Contents/MacOS/ClaudeProfilesMenu" \
    macos/ClaudeProfilesApp.swift macos/Icon.swift
  cp target/release/claude-profiles "$HELPER/Contents/Resources/claude-profiles"
  cp "$APP/Contents/Resources/AppIcon.icns" "$HELPER/Contents/Resources/AppIcon.icns"
  cat > "$HELPER/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Claude Profiles Menu</string>
  <key>CFBundleIdentifier</key><string>dev.claude-profiles.menubar</string>
  <key>CFBundleExecutable</key><string>ClaudeProfilesMenu</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>$MIN_MACOS</string>
  <key>LSUIElement</key><true/>
  <key>NSAppleEventsUsageDescription</key>
  <string>Claude Profiles opens your terminal to start Claude Code as the account you pick.</string>
</dict>
</plist>
PLIST
  # Inside out: the helper's signature is sealed into the app's.
  codesign --force --sign - "$HELPER"
fi
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Claude Profiles</string>
  <key>CFBundleDisplayName</key><string>Claude Profiles</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleExecutable</key><string>ClaudeProfiles</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>$MIN_MACOS</string>
  <key>NSHighResolutionCapable</key><true/>
  $UI_ELEMENT
  <key>NSAppleEventsUsageDescription</key>
  <string>Claude Profiles opens your terminal to start Claude Code as the account you pick.</string>
</dict>
</plist>
PLIST

# The command is left as cargo signed it, so it stays byte-identical to the
# one `cargo install` puts on PATH. Re-signing it would make the two look like
# different builds, and each would keep replacing the launch helper with
# itself. The bundle's signature seals it as a resource.
codesign --force --sign - "$APP"
echo "Built $APP"

if [[ "${1:-}" == "--install" ]]; then
  mkdir -p "$HOME/Applications"
  pkill -x ClaudeProfiles 2>/dev/null || true
  pkill -x ClaudeProfilesMenu 2>/dev/null || true
  # Opening it again before the old one has quit fails with error -600.
  for _ in $(seq 20); do pgrep -x ClaudeProfiles >/dev/null || pgrep -x ClaudeProfilesMenu >/dev/null || break; sleep 0.25; done
  rm -rf "$HOME/Applications/Claude Profiles.app"
  cp -R "$APP" "$HOME/Applications/"
  # Only the installed copy should show in Launchpad and Spotlight.
  LSREG=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
  "$LSREG" -u "$PWD/$APP" 2>/dev/null || true
  # Touch it so Launchpad and the Dock pick up a changed icon, not a cached one.
  touch "$HOME/Applications/Claude Profiles.app"
  "$LSREG" -f "$HOME/Applications/Claude Profiles.app" 2>/dev/null || true
  open "$HOME/Applications/Claude Profiles.app"
  echo "Installed to ~/Applications/Claude Profiles.app"
fi
