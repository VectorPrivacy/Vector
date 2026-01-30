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

# OpenSSL for Android (SQLCipher dependency)
OPENSSL_ANDROID_DIR="$TAURI_DIR/android-deps/openssl"

if [ ! -d "$OPENSSL_ANDROID_DIR/include/openssl" ]; then
    echo "Error: OpenSSL for Android not found at $OPENSSL_ANDROID_DIR"
    echo "Please run a release build first to generate OpenSSL artifacts,"
    echo "or manually place pre-built OpenSSL for Android in that directory."
    exit 1
fi

echo "Using OpenSSL: $OPENSSL_ANDROID_DIR"

# Export environment variables for the build
export ANDROID_NDK_HOME
export OPENSSL_DIR="$OPENSSL_ANDROID_DIR"
export OPENSSL_INCLUDE_DIR="$OPENSSL_ANDROID_DIR/include"
export OPENSSL_LIB_DIR="$OPENSSL_ANDROID_DIR/lib"
export OPENSSL_STATIC=1

# Run tauri android dev
cd "$PROJECT_ROOT"
npx tauri android dev "$@"
