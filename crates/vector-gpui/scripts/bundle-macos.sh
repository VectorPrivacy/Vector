#!/bin/bash
# Wraps a release binary in an ad-hoc signed .app, zipped with ditto so the bundle keeps its
# permissions and signature through an artifact upload, plus the .app.tar.gz the in-app updater
# unpacks (backend/install.rs).
# Usage: scripts/bundle-macos.sh <binary> <out-dir>
set -euo pipefail

bin="$1"
out="$2"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tauri="$here/../../src-tauri"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)
app="$out/Vector GPUI.app"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/vector-gpui"
cp "$tauri/icons/icon.icns" "$app/Contents/Resources/icon.icns"

# Its own identifier, so it never takes the desktop client's notifications or vector:// links.
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>io.vectorapp</string>
  <key>CFBundleName</key><string>Vector GPUI</string>
  <key>CFBundleDisplayName</key><string>Vector GPUI</string>
  <key>CFBundleExecutable</key><string>vector-gpui</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>This is required for sending Voice Messages as well as other voice activities, such as calls.</string>
  <key>NSCameraUsageDescription</key><string>This is required for video calls.</string>
  <key>NSDocumentsFolderUsageDescription</key><string>This is required to receive and save files.</string>
</dict>
</plist>
PLIST

codesign --force --sign - --entitlements "$tauri/Entitlements.plist" "$app"
codesign --verify --strict "$app"
ditto -c -k --keepParent "$app" "$out/Vector-GPUI-macOS.zip"
case "$(lipo -archs "$bin")" in
  arm64) arch=aarch64 ;;
  x86_64) arch=x64 ;;
  *) echo "one architecture per bundle" >&2; exit 1 ;;
esac
# No AppleDouble ._ entries: the updater takes the first .app it unpacks.
COPYFILE_DISABLE=1 tar -C "$out" -czf "$out/Vector-GPUI_$arch.app.tar.gz" "Vector GPUI.app"
rm -rf "$app"
