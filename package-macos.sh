#!/bin/bash
# Builds CodexBar.app and a DMG for macOS (#167). The app is a menu-bar app (LSUIElement) signed ad hoc, or with the
# identity in CODESIGN_IDENTITY (a Developer ID, once there is one). macOS shows notifications only from an app bundle,
# so this is the build that can alert.
#
#   ./package-macos.sh            builds target/package/macos/CodexBar.app and CodexBar-<version>.dmg
#   OUT=dist ./package-macos.sh   puts both in dist/
set -euo pipefail
cd "$(dirname "$0")"

OUT=${OUT:-target/package/macos}
IDENTITY=${CODESIGN_IDENTITY:--}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUNDLE_ID=HemSoft.CodexBar

cargo build --release --locked -p codexbar-app
cargo build --release --locked -p codexbar-app --example icon

APP="$OUT/CodexBar.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/codexbar "$APP/Contents/MacOS/codexbar"
# CodexBar's license and the embedded provider logos' notice travel with the app.
cp LICENSE "$APP/Contents/Resources/LICENSE"
cp crates/codexbar-app/assets/brand/LICENSE.md "$APP/Contents/Resources/provider-logos.md"

# The icon, at every size the Finder and the Dock ask for.
ICONSET=$(mktemp -d)/CodexBar.iconset
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    target/release/examples/icon "$ICONSET/icon_${size}x${size}.png" "$size"
    target/release/examples/icon "$ICONSET/icon_${size}x${size}@2x.png" "$((size * 2))"
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/CodexBar.icns"
rm -rf "$(dirname "$ICONSET")"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
    <key>CFBundleName</key><string>CodexBar</string>
    <key>CFBundleDisplayName</key><string>CodexBar</string>
    <key>CFBundleExecutable</key><string>codexbar</string>
    <key>CFBundleIconFile</key><string>CodexBar</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>LSUIElement</key><true/>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

codesign --force --deep --sign "$IDENTITY" "$APP"
codesign --verify --strict "$APP"

# A DMG with the app and a link to Applications, to drag it across.
STAGE=$(mktemp -d)
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
DMG="$OUT/CodexBar-$VERSION.dmg"
rm -f "$DMG"
hdiutil create -quiet -volname CodexBar -srcfolder "$STAGE" -ov -format UDZO "$DMG"
rm -rf "$STAGE"

echo "Built $APP and $DMG"
