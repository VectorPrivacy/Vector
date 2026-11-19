#!/usr/bin/env bash
# The deb and rpm, laid out to replace the Tauri client's `vector` package in place: same package
# name, /usr/bin/Vector, Vector.desktop. The Tauri updater installs them with `dpkg -i` / `rpm -U`.
#   scripts/bundle-linux-pkg.sh <dir holding vector-gpui and vector-gpui-miniapp> <out dir>
# VECTOR_RELEASE=1 names the files as a release's (Vector_…); otherwise Vector-GPUI_….
# rpmbuild is optional: without it only the deb is made.
set -euo pipefail

bins=$(realpath "$1")
out=$(realpath -m "$2")
here=$(cd "$(dirname "$0")/.." && pwd)
repo=$(cd "$here/../.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)
# A preview (0.6.0-1) sorts before its release in both: dpkg and rpm read `~` that way.
pkgversion=${version//-/\~}
prefix=$([ "${VECTOR_RELEASE:-}" = 1 ] && echo Vector || echo Vector-GPUI)

case "$(uname -m)" in
  x86_64) debarch=amd64 rpmarch=x86_64 ;;
  aarch64) debarch=arm64 rpmarch=aarch64 ;;
  *) echo "unsupported arch $(uname -m)" >&2; exit 1 ;;
esac

# Runtime packages by soname, as Ubuntu 22.04 (the build host, so the floor) names them; later
# releases' t64 packages provide these names.
depends_of() {
  case "$1" in
    libc.so.6|libm.so.6|libdl.so.2|libpthread.so.0|librt.so.1|ld-linux-*.so.*) echo libc6 ;;
    libgcc_s.so.1) echo libgcc-s1 ;;
    libstdc++.so.6) echo libstdc++6 ;;
    libxkbcommon.so.0) echo libxkbcommon0 ;;
    libxkbcommon-x11.so.0) echo libxkbcommon-x11-0 ;;
    libxcb.so.1) echo libxcb1 ;;
    libX11.so.6) echo libx11-6 ;;
    libX11-xcb.so.1) echo libx11-xcb1 ;;
    libvulkan.so.1) echo libvulkan1 ;;
    libasound.so.2) echo libasound2 ;;
    libfontconfig.so.1) echo libfontconfig1 ;;
    libfreetype.so.6) echo libfreetype6 ;;
    libwayland-client.so.0) echo libwayland-client0 ;;
    libwayland-cursor.so.0) echo libwayland-cursor0 ;;
    libdbus-1.so.3) echo libdbus-1-3 ;;
    libz.so.1) echo zlib1g ;;
    libzstd.so.1) echo libzstd1 ;;
    libssl.so.3|libcrypto.so.3) echo libssl3 ;;
    libglib-2.0.so.0|libgobject-2.0.so.0|libgio-2.0.so.0|libgmodule-2.0.so.0) echo libglib2.0-0 ;;
    libgtk-3.so.0|libgdk-3.so.0) echo libgtk-3-0 ;;
    libpango-1.0.so.0|libpangocairo-1.0.so.0) echo libpango-1.0-0 ;;
    libcairo.so.2) echo libcairo2 ;;
    libgdk_pixbuf-2.0.so.0) echo libgdk-pixbuf-2.0-0 ;;
    libwebkit2gtk-4.1.so.0) echo libwebkit2gtk-4.1-0 ;;
    libjavascriptcoregtk-4.1.so.0) echo libjavascriptcoregtk-4.1-0 ;;
    libsoup-3.0.so.0) echo libsoup-3.0-0 ;;
    *) echo "no package known for $1: add it to depends_of in $0" >&2; return 1 ;;
  esac
}

depends=$(for b in vector-gpui vector-gpui-miniapp; do
  readelf -d "$bins/$b" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'
done | sort -u | while read -r so; do depends_of "$so" || exit 1; done | sort -u | paste -sd, - | sed 's/,/, /g')

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
root="$stage/root"
install -Dm755 "$bins/vector-gpui" "$root/usr/lib/Vector/Vector"
install -Dm755 "$bins/vector-gpui-miniapp" "$root/usr/lib/Vector/vector-gpui-miniapp"
mkdir -p "$root/usr/bin"
ln -s ../lib/Vector/Vector "$root/usr/bin/Vector"
icons="$repo/src-tauri/icons"
install -Dm644 "$icons/32x32.png" "$root/usr/share/icons/hicolor/32x32/apps/Vector.png"
install -Dm644 "$icons/128x128.png" "$root/usr/share/icons/hicolor/128x128/apps/Vector.png"
install -Dm644 "$icons/128x128@2x.png" "$root/usr/share/icons/hicolor/256x256/apps/Vector.png"
install -Dm644 /dev/stdin "$root/usr/share/applications/Vector.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Vector
Comment=Private messaging
Exec=Vector %u
Icon=Vector
Terminal=false
Categories=Network;InstantMessaging;
MimeType=x-scheme-handler/vector;
StartupWMClass=io.vectorapp.gpui
DESKTOP

summary="A humble attempt at a modern, private, complete Nostr chat client."
maintainer="Formless Labs, JSKitty, YuurinBee"
mkdir -p "$out"

mkdir -p "$root/DEBIAN"
size=$(du -sk --exclude=DEBIAN "$root" | cut -f1)
cat > "$root/DEBIAN/control" <<CONTROL
Package: vector
Version: $pkgversion
Architecture: $debarch
Installed-Size: $size
Maintainer: $maintainer
Priority: optional
Section: net
Depends: $depends
Homepage: https://vectorapp.io
Description: $summary
CONTROL
deb="$out/${prefix}_${version}_$debarch.deb"
dpkg-deb --root-owner-group -Zxz --build "$root" "$deb" >/dev/null
rm -rf "$root/DEBIAN"
ls -l "$deb"

if ! command -v rpmbuild >/dev/null; then
  echo "rpmbuild not found: no rpm" >&2
  exit 0
fi
mkdir -p "$stage/rpm"/{BUILD,RPMS,SPECS,SRPMS,SOURCES}
cat > "$stage/rpm/SPECS/vector.spec" <<SPEC
Name: vector
Version: $pkgversion
Release: 1
Summary: $summary
License: MIT
URL: https://vectorapp.io
Packager: $maintainer
AutoReqProv: yes

%description
$summary

%install
cp -a "$root/." %{buildroot}/

%files
/usr/bin/Vector
/usr/lib/Vector
/usr/share/applications/Vector.desktop
/usr/share/icons/hicolor/32x32/apps/Vector.png
/usr/share/icons/hicolor/128x128/apps/Vector.png
/usr/share/icons/hicolor/256x256/apps/Vector.png
SPEC
rpmbuild --quiet -bb --define "_topdir $stage/rpm" --define "_build_id_links none" \
  --define "__strip /bin/true" "$stage/rpm/SPECS/vector.spec"
rpm="$out/${prefix}-${version}-1.$rpmarch.rpm"
mv "$stage/rpm/RPMS/$rpmarch/"vector-*.rpm "$rpm"
ls -l "$rpm"
