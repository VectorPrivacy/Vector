#!/bin/bash
# Android development script for Vector
# Sets up the required environment variables for cross-compiling to Android

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
TAURI_DIR="$PROJECT_ROOT/src-tauri"

# Android NDK - detect if not set
if [ -z "$ANDROID_NDK_HOME" ]; then
    # Try common locations
    if [ -d "$HOME/Library/Android/sdk/ndk" ]; then
        # Find the latest NDK version
        ANDROID_NDK_HOME=$(ls -d "$HOME/Library/Android/sdk/ndk"/*/ 2>/dev/null | sort -V | tail -1 | sed 's:/$::')
    fi
fi

if [ -z "$ANDROID_NDK_HOME" ]; then
    echo "Error: ANDROID_NDK_HOME not set and couldn't find Android NDK"
    echo "Please install Android NDK via Android Studio or set ANDROID_NDK_HOME"
    exit 1
fi

echo "Using Android NDK: $ANDROID_NDK_HOME"

# Export environment variables for the build
export ANDROID_NDK_HOME

# Strip debuginfo on Android dev builds ONLY (scoped to this script's env).
# Default debug `libvector_lib.so` is ~800MB of DWARF, producing a ~1.5GB
# APK that Android refuses to install on devices with only a few GB free
# (INSTALL_FAILED_INSUFFICIENT_STORAGE). `strip=debuginfo` brings the .so
# down ~10x; `debug=line-tables-only` keeps panics + backtrace line numbers
# usable. Desktop `npm run dev` runs from a different shell and is
# unaffected — these env vars exist only for the duration of this script.
export CARGO_PROFILE_DEV_STRIP=debuginfo
export CARGO_PROFILE_DEV_DEBUG=line-tables-only

# Run tauri android dev
cd "$PROJECT_ROOT"
npx tauri android dev "$@"
