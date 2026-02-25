#!/bin/bash
# Builds OpenSSL static libraries for Android (aarch64 + armv7)
# Outputs to src-tauri/android-deps/openssl/{aarch64,armv7}/lib/
#
# Prerequisites: Android NDK installed

set -e

OPENSSL_VERSION="3.5.0"
ANDROID_API=26

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
OUTPUT_DIR="$PROJECT_ROOT/src-tauri/android-deps/openssl"
BUILD_DIR="$PROJECT_ROOT/src-tauri/android-deps/.build"

# Detect Android NDK
if [ -z "$ANDROID_NDK_HOME" ] && [ -z "$NDK_HOME" ]; then
    if [ -d "$HOME/Library/Android/sdk/ndk" ]; then
        ANDROID_NDK_HOME=$(ls -d "$HOME/Library/Android/sdk/ndk"/*/ 2>/dev/null | sort -V | tail -1 | sed 's:/$::')
    fi
fi
ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$NDK_HOME}"

if [ -z "$ANDROID_NDK_HOME" ] || [ ! -d "$ANDROID_NDK_HOME" ]; then
    echo "Error: Android NDK not found. Set ANDROID_NDK_HOME or NDK_HOME."
    exit 1
fi

echo "NDK: $ANDROID_NDK_HOME"
echo "OpenSSL: $OPENSSL_VERSION"
echo "Android API: $ANDROID_API"
echo ""

# Download OpenSSL source if needed
mkdir -p "$BUILD_DIR"
OPENSSL_TAR="$BUILD_DIR/openssl-$OPENSSL_VERSION.tar.gz"
OPENSSL_SRC="$BUILD_DIR/openssl-$OPENSSL_VERSION"

if [ ! -f "$OPENSSL_TAR" ]; then
    echo "Downloading OpenSSL $OPENSSL_VERSION..."
    curl -L -o "$OPENSSL_TAR" \
        "https://github.com/openssl/openssl/releases/download/openssl-$OPENSSL_VERSION/openssl-$OPENSSL_VERSION.tar.gz"
fi

# Build for each target
build_openssl() {
    local target_name="$1"   # aarch64 or armv7
    local openssl_target="$2" # android-arm64 or android-arm

    echo ""
    echo "========================================="
    echo "Building OpenSSL for $target_name ($openssl_target)"
    echo "========================================="

    # Clean and extract fresh source
    rm -rf "$OPENSSL_SRC"
    tar xzf "$OPENSSL_TAR" -C "$BUILD_DIR"

    cd "$OPENSSL_SRC"

    export ANDROID_NDK_ROOT="$ANDROID_NDK_HOME"

    # Auto-detect host platform for NDK toolchain path
    case "$(uname -s)" in
        Darwin) HOST_TAG="darwin-x86_64" ;;
        Linux)  HOST_TAG="linux-x86_64" ;;
        *)      echo "Unsupported host OS: $(uname -s)"; exit 1 ;;
    esac
    export PATH="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$HOST_TAG/bin:$PATH"

    ./Configure "$openssl_target" \
        -D__ANDROID_API__=$ANDROID_API \
        no-shared \
        no-tests \
        no-ui-console \
        --prefix="$BUILD_DIR/install-$target_name"

    # Use nproc on Linux, sysctl on macOS
    NPROC=$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)
    make -j$NPROC > /dev/null 2>&1
    make install_sw > /dev/null 2>&1

    # Copy outputs
    mkdir -p "$OUTPUT_DIR/$target_name/lib"
    cp "$BUILD_DIR/install-$target_name/lib/libcrypto.a" "$OUTPUT_DIR/$target_name/lib/"
    cp "$BUILD_DIR/install-$target_name/lib/libssl.a" "$OUTPUT_DIR/$target_name/lib/"

    # Copy headers (same for both arches, but do it once)
    if [ ! -d "$OUTPUT_DIR/include/openssl" ] || [ "$target_name" = "aarch64" ]; then
        rm -rf "$OUTPUT_DIR/include"
        cp -r "$BUILD_DIR/install-$target_name/include" "$OUTPUT_DIR/include"
    fi

    echo "Done: $OUTPUT_DIR/$target_name/lib/"
}

build_openssl "aarch64" "android-arm64"
build_openssl "armv7"   "android-arm"

# Cleanup build directory
rm -rf "$BUILD_DIR"

echo ""
echo "========================================="
echo "OpenSSL $OPENSSL_VERSION built successfully"
echo "  aarch64: $OUTPUT_DIR/aarch64/lib/"
echo "  armv7:   $OUTPUT_DIR/armv7/lib/"
echo "  headers: $OUTPUT_DIR/include/"
echo "========================================="
