#!/bin/bash
# Android APK build script for Vector
# Produces a release APK for arm64 and arm32

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

# OpenSSL for Android (SQLCipher dependency)
OPENSSL_ANDROID_DIR="$TAURI_DIR/android-deps/openssl"

if [ ! -d "$OPENSSL_ANDROID_DIR/include/openssl" ]; then
    echo "Error: OpenSSL headers not found at $OPENSSL_ANDROID_DIR/include/"
    exit 1
fi

if [ ! -f "$OPENSSL_ANDROID_DIR/aarch64/lib/libcrypto.a" ]; then
    echo "Error: OpenSSL aarch64 libs not found at $OPENSSL_ANDROID_DIR/aarch64/lib/"
    exit 1
fi

if [ ! -f "$OPENSSL_ANDROID_DIR/armv7/lib/libcrypto.a" ]; then
    echo "Error: OpenSSL armv7 libs not found at $OPENSSL_ANDROID_DIR/armv7/lib/"
    echo "Please build or obtain OpenSSL for armv7-linux-androideabi and place libs there."
    exit 1
fi

echo "Using OpenSSL: $OPENSSL_ANDROID_DIR"

# Export environment variables for the build
export ANDROID_NDK_HOME
export ANDROID_NDK="$ANDROID_NDK_HOME"  # cmake's Platform/Android-Determine.cmake needs this
export OPENSSL_STATIC=1
export OPENSSL_INCLUDE_DIR="$OPENSSL_ANDROID_DIR/include"

# Per-target OpenSSL lib directories
export AARCH64_LINUX_ANDROID_OPENSSL_LIB_DIR="$OPENSSL_ANDROID_DIR/aarch64/lib"
export ARMV7_LINUX_ANDROIDEABI_OPENSSL_LIB_DIR="$OPENSSL_ANDROID_DIR/armv7/lib"

# Build the APK
cd "$PROJECT_ROOT"

# Support --armv7-only for Android Go / 32-bit budget devices (~42MB vs ~95MB universal)
if [ "$1" = "--armv7-only" ]; then
    shift
    echo "Building armv7-only APK (Android Go / 32-bit)"
    npx tauri android build --apk true --target armv7 "$@"
else
    npx tauri android build --apk true --target aarch64 --target armv7 "$@"
fi

# Collect the per-ABI + universal split APKs under clean, store-friendly names.
# The abi-splits config emits app-universal-<abi>-release.apk; stores distribute
# the per-ABI slims (~46/61MB) and the universal (~104MB) is the sideload fallback.
APK_DIR="$TAURI_DIR/gen/android/app/build/outputs/apk/universal/release"
DIST_DIR="$PROJECT_ROOT/dist-android"
mkdir -p "$DIST_DIR"

# bash 3.2 (stock macOS) has no associative arrays — keep it to a plain helper.
collect() {  # $1 = built basename, $2 = distribution name
    if [ -f "$APK_DIR/$1" ]; then
        cp "$APK_DIR/$1" "$DIST_DIR/$2"
        echo "  $(du -h "$DIST_DIR/$2" | cut -f1)	$2"
    fi
}

echo ""
echo "Collecting APKs into $DIST_DIR:"
collect "app-universal-arm64-v8a-release.apk"   "Vector-arm64-v8a.apk"
collect "app-universal-armeabi-v7a-release.apk" "Vector-armeabi-v7a.apk"
collect "app-universal-universal-release.apk"   "Vector.apk"

if ! ls "$DIST_DIR"/*.apk >/dev/null 2>&1; then
    echo "Error: no release APKs found in $APK_DIR"
    exit 1
fi
