#!/bin/bash
# Build + install for the ATTACHED device only.
#
# `tauri android build` compiles all four Android targets. Distribution only
# ships two (build.gradle.kts splits: armeabi-v7a, arm64-v8a) and any one device
# runs exactly one, so three quarters of that work is discarded — at
# opt-level=3, codegen-units=1, the most expensive settings in the profile.
# This asks adb which ABI is actually attached and builds just that.
#
#   ./scripts/android-emu.sh            release without LTO, attached ABI only
#   ./scripts/android-emu.sh --debug    also skip the optimiser
#
# For FRONTEND-ONLY changes prefer `npm run android:dev`: it serves the assets
# over the network, so a reload replaces the whole rebuild. This script is for
# when Rust changed, or you need a real installed APK.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_ROOT"

# A plain string, not an array: macOS ships bash 3.2, where expanding an EMPTY
# array under `set -u` is an unbound-variable error.
PROFILE_FLAG=""
PROFILE_DIR="release"
for arg in "$@"; do
  if [ "$arg" = "--debug" ]; then PROFILE_FLAG="--debug"; PROFILE_DIR="debug"; fi
done

# A test build, not a shipped one: the same opt-level without the whole-program LTO and single
# codegen unit that cost minutes a build, in its own target dir so neither build invalidates the
# other's dependencies. Exported, so Gradle's re-entry into cargo finds the same build.
if [ -z "$PROFILE_FLAG" ]; then
  export CARGO_PROFILE_RELEASE_LTO=false
  export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
  export CARGO_PROFILE_RELEASE_INCREMENTAL=true
  export CARGO_TARGET_DIR="$PROJECT_ROOT/src-tauri/target/emu"
fi

DEVICE_ABI="$(adb shell getprop ro.product.cpu.abi 2>/dev/null | tr -d '\r')"
if [ -z "$DEVICE_ABI" ]; then
  echo "No device via adb. Start the emulator or plug in a phone." >&2
  exit 1
fi

# Tauri's target names differ from Android's ABI names.
case "$DEVICE_ABI" in
  arm64-v8a)   TARGET=aarch64 ;;
  armeabi-v7a) TARGET=armv7 ;;
  x86_64)      TARGET=x86_64 ;;
  x86)         TARGET=i686 ;;
  *) echo "Unrecognised ABI: $DEVICE_ABI" >&2; exit 1 ;;
esac
echo "Device ABI $DEVICE_ABI -> tauri target $TARGET (${PROFILE_DIR})"

node scripts/build-frontend.mjs
npx tauri android build --apk true --target "$TARGET" $PROFILE_FLAG

OUT="src-tauri/gen/android/app/build/outputs/apk"
# Prefer the split matching this ABI; the universal is the fallback.
APK="$(ls -t "$OUT/universal/$PROFILE_DIR"/*"$DEVICE_ABI"*.apk 2>/dev/null | head -1 || true)"
[ -n "$APK" ] || APK="$(ls -t "$OUT/universal/$PROFILE_DIR"/*.apk 2>/dev/null | head -1 || true)"
if [ -z "$APK" ]; then
  echo "No APK under $OUT/universal/$PROFILE_DIR" >&2
  exit 1
fi
echo "APK: $APK ($(( $(stat -f%z "$APK" 2>/dev/null || stat -c%s "$APK") / 1024 / 1024 )) MB)"

# An update signed by another key is refused, and reinstalling would wipe the account.
# Re-sign to match a debug-signed install rather than uninstall.
BT="$(ls -d "$HOME/Library/Android/sdk/build-tools"/* 2>/dev/null | sort -V | tail -1)"
CURRENT_SIGNER="$("$BT/apksigner" verify --print-certs "$APK" 2>/dev/null | grep -m1 'certificate DN' || true)"
INSTALLED_SIGNER=""
INSTALLED_APK="$(adb shell pm path io.vectorapp 2>/dev/null | sed -n 's/^package://p' | head -1 | tr -d '\r')"
if [ -n "$INSTALLED_APK" ]; then
  PULLED="${TMPDIR:-/tmp}/vector-emu-installed.apk"
  adb pull "$INSTALLED_APK" "$PULLED" >/dev/null
  INSTALLED_SIGNER="$("$BT/apksigner" verify --print-certs "$PULLED" 2>/dev/null | grep -m1 'certificate DN' || true)"
fi
if echo "$INSTALLED_SIGNER" | grep -q 'Android Debug' && ! echo "$CURRENT_SIGNER" | grep -q 'Android Debug'; then
  echo "Re-signing with the debug keystore to match the installed app…"
  SIGNED="${TMPDIR:-/tmp}/vector-emu-signed.apk"
  cp "$APK" "$SIGNED"
  "$BT/apksigner" sign --ks "$HOME/.android/debug.keystore" --ks-pass pass:android \
      --key-pass pass:android --ks-key-alias androiddebugkey "$SIGNED"
  APK="$SIGNED"
fi

adb install -r "$APK"
echo "Installed. Launching…"
adb shell monkey -p io.vectorapp -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1
