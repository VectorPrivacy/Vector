#!/usr/bin/env bash
# The Linux updater bundle, built the way the Tauri bundler builds Vector's AppImage
# (linuxdeploy + its GTK plugin, WebKitGTK's helper processes copied in for Mini Apps).
#   scripts/bundle-appimage.sh <dir holding vector-gpui and vector-gpui-miniapp> <out dir>
set -euo pipefail

bins=$(realpath "$1")
out=$(realpath -m "$2")
here=$(cd "$(dirname "$0")/.." && pwd)
repo=$(cd "$here/../.." && pwd)

case "$(uname -m)" in
  x86_64) arch=x86_64; name=amd64; multiarch=x86_64-linux-gnu ;;
  aarch64) arch=aarch64; name=aarch64; multiarch=aarch64-linux-gnu ;;
  *) echo "unsupported arch $(uname -m)"; exit 1 ;;
esac

tools="${TOOLS:-$out/.tools}"
mkdir -p "$tools" "$out"
fetch() { [ -s "$2" ] || curl -fsSL --retry 3 -o "$2" "$1"; chmod +x "$2"; }
fetch "https://github.com/tauri-apps/binary-releases/releases/download/apprun-old/AppRun-$arch" "$tools/AppRun-$arch"
fetch "https://github.com/tauri-apps/binary-releases/releases/download/linuxdeploy/linuxdeploy-$arch.AppImage" "$tools/linuxdeploy-$arch.AppImage"
fetch "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh" "$tools/linuxdeploy-plugin-gtk.sh"
# Keeps AppImage desktop integration tools from taking linuxdeploy for an app.
dd if=/dev/zero bs=1 count=3 seek=8 conv=notrunc of="$tools/linuxdeploy-$arch.AppImage" 2>/dev/null

app="$out/Vector.AppDir"
rm -rf "$app"
mkdir -p "$app/usr/bin" "$app/usr/share/applications" "$app/usr/share/icons/hicolor/256x256/apps"
install -m755 "$bins/vector-gpui" "$bins/vector-gpui-miniapp" "$app/usr/bin/"
install -m644 "$repo/src-tauri/icons/128x128@2x.png" "$app/usr/share/icons/hicolor/256x256/apps/vector-gpui.png"
cat > "$app/usr/share/applications/vector-gpui.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Vector
Comment=Private messaging
Exec=vector-gpui %u
Icon=vector-gpui
Categories=Network;InstantMessaging;
MimeType=x-scheme-handler/vector;
StartupWMClass=io.vectorapp.gpui
DESKTOP
cp /usr/bin/xdg-open /usr/bin/xdg-mime "$app/usr/bin/"

for f in WebKitNetworkProcess WebKitWebProcess injected-bundle/libwebkit2gtkinjectedbundle.so; do
  for d in "/usr/lib/$multiarch" /usr/lib64 /usr/lib /usr/libexec; do
    src="$d/webkit2gtk-4.1/$f"
    if [ -e "$src" ]; then
      mkdir -p "$app$(dirname "$src")"
      cp "$src" "$app$src"
    fi
  done
done

cp "$tools/AppRun-$arch" "$app/AppRun"
cp "$app/usr/share/icons/hicolor/256x256/apps/vector-gpui.png" "$app/vector-gpui.png"
ln -sf vector-gpui.png "$app/.DirIcon"
ln -sf usr/share/applications/vector-gpui.desktop "$app/vector-gpui.desktop"

export PATH="$tools:$PATH" ARCH="$arch" APPIMAGE_EXTRACT_AND_RUN=1 OUTPUT="$out/Vector-GPUI_$name.AppImage"
"$tools/linuxdeploy-$arch.AppImage" --appimage-extract-and-run --appdir "$app" \
  --executable "$app/usr/bin/vector-gpui-miniapp" --plugin gtk --output appimage
rm -rf "$app"
ls -l "$OUTPUT"
