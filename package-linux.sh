#!/bin/bash
# Builds a Debian/Ubuntu package of CodexBar (#167): the app in /usr/bin, a desktop entry and its icon.
#
#   ./package-linux.sh            builds target/package/linux/codexbar_<version>_<arch>.deb
#   OUT=dist ./package-linux.sh   puts it in dist/
#
# Install it with `sudo apt install ./codexbar_<version>_<arch>.deb`, which also installs what it depends on.
set -euo pipefail
cd "$(dirname "$0")"

OUT=${OUT:-target/package/linux}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(dpkg --print-architecture)
# The app id GPUI gives the window (main.rs); desktops match it to this entry's name for the icon and title.
APP_ID=HemSoft.CodexBar

cargo build --release --locked -p codexbar-app
cargo build --release --locked -p codexbar-app --example icon

ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT"' EXIT
chmod 755 "$ROOT"
install -Dm755 target/release/codexbar "$ROOT/usr/bin/codexbar"
for size in 48 128 256 512; do
    mkdir -p "$ROOT/usr/share/icons/hicolor/${size}x${size}/apps"
    target/release/examples/icon "$ROOT/usr/share/icons/hicolor/${size}x${size}/apps/$APP_ID.png" "$size"
done
install -Dm644 /dev/stdin "$ROOT/usr/share/applications/$APP_ID.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=CodexBar
Comment=AI provider usage limits in the tray
Exec=codexbar
Icon=$APP_ID
StartupWMClass=$APP_ID
Categories=Utility;
Keywords=AI;usage;limits;Codex;Claude;Copilot;
DESKTOP
install -Dm644 crates/codexbar-app/assets/brand/LICENSE.md "$ROOT/usr/share/doc/codexbar/provider-logos.md"

# GPUI links xcb and xkbcommon, and loads Vulkan and Wayland at run time; the tray and the keyring speak D-Bus in
# pure Rust. A Vulkan driver (Mesa's, or the GPU vendor's) draws the window.
mkdir -p "$ROOT/DEBIAN"
cat > "$ROOT/DEBIAN/control" <<CONTROL
Package: codexbar
Version: $VERSION
Architecture: $ARCH
Maintainer: HemSoft <codexbar@users.noreply.github.com>
Depends: libc6, libgcc-s1, libxcb1, libxcb-xkb1, libxkbcommon0, libxkbcommon-x11-0, libvulkan1, libwayland-client0
Recommends: mesa-vulkan-drivers, libegl1, gnome-keyring | kwalletmanager
Section: utils
Priority: optional
Homepage: https://github.com/hemsoft-dev/codexbar
Description: AI provider usage limits in the tray
 CodexBar keeps the usage limits of ChatGPT/Codex, Claude, Copilot, Cursor and
 other AI providers in view from the system tray.
CONTROL

mkdir -p "$OUT"
DEB="$OUT/codexbar_${VERSION}_${ARCH}.deb"
dpkg-deb --build --root-owner-group "$ROOT" "$DEB" >/dev/null
echo "Built $DEB"
