#!/bin/bash
# Builds a selective, static, LGPL FFmpeg for video compression: the formats Vector users
# actually send in, H.264 + AAC MP4 out through the platform's hardware encoder. Frames are
# scaled in Rust (fast_image_resize), so swscale is not built.
# Outputs to src-tauri/native-deps/ffmpeg/<rust-target>/{include,lib}
#
# Usage: scripts/build-ffmpeg.sh [rust-target ...]   (default: the host)
#   aarch64-apple-darwin | x86_64-apple-darwin          (macOS, VideoToolbox)
#   aarch64-linux-android | armv7-linux-androideabi | x86_64-linux-android  (NDK, MediaCodec)
#   x86_64-pc-windows-msvc                              (Media Foundation; from MSYS2, below)
#   x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu (no hardware encoder yet)
# Env: ANDROID_NDK_HOME/NDK_HOME for Android; FFMPEG_TEST_ENCODER=1 adds FFmpeg's own MPEG-4
# Part 2 encoder (and its decoder, to read the output back) so the pipeline can be tested where
# no hardware encoder exists.
#
# Prerequisites: a C toolchain, make, pkg-config; nasm on x86 hosts (else x86 SIMD is off).
# Windows: an MSYS2 shell (make, nasm, diffutils) started with MSYS2_PATH_TYPE=inherit from a
# Visual Studio x64 developer prompt, so link.exe and its INCLUDE/LIB are in reach, plus clang-cl.

set -e

FFMPEG_VERSION="8.0.1"
FFMPEG_SHA256="05ee0b03119b45c0bdb4df654b96802e909e0a752f72e4fe3794f487229e5a41"
ANDROID_API=26

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
OUTPUT_ROOT="$PROJECT_ROOT/src-tauri/native-deps/ffmpeg"
BUILD_DIR="$PROJECT_ROOT/src-tauri/native-deps/.build"
FFMPEG_TAR="$BUILD_DIR/ffmpeg-$FFMPEG_VERSION.tar.xz"

mkdir -p "$BUILD_DIR"
if [ ! -f "$FFMPEG_TAR" ]; then
    echo "Downloading FFmpeg $FFMPEG_VERSION..."
    curl -fL -o "$FFMPEG_TAR.part" "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz"
    mv "$FFMPEG_TAR.part" "$FFMPEG_TAR"
fi
if command -v sha256sum >/dev/null; then
    echo "$FFMPEG_SHA256  $FFMPEG_TAR" | sha256sum -c -
else
    echo "$FFMPEG_SHA256  $FFMPEG_TAR" | shasum -a 256 -c -
fi

# Everything off, then only what a survey of 12k received attachments showed in use: H.264
# (~92% of videos), HEVC and VP9 video; AAC and Opus audio; MP4/MOV and WebM containers.
# Anything rarer is left out on purpose: each decoder is attack surface and binary weight.
COMMON_FLAGS=(
    --disable-everything --disable-autodetect --disable-programs --disable-doc
    --disable-network --disable-avdevice --disable-avfilter --disable-debug
    --enable-static --disable-shared --enable-pic
    --enable-protocol=file
    --enable-demuxer=mov,matroska
    --enable-muxer=mp4
    --enable-parser=h264,hevc,vp9,aac,opus
    --enable-decoder=h264,hevc,vp9,aac,opus
    --enable-encoder=aac
    --enable-bsf=aac_adtstoasc
    --disable-swscale --enable-swresample
)
if [ "$FFMPEG_TEST_ENCODER" = "1" ]; then
    COMMON_FLAGS+=(--enable-encoder=mpeg4 --enable-decoder=mpeg4 --enable-parser=mpeg4video)
fi

host_target() {
    case "$(uname -s)-$(uname -m)" in
        Darwin-arm64) echo aarch64-apple-darwin ;;
        Darwin-x86_64) echo x86_64-apple-darwin ;;
        Linux-x86_64) echo x86_64-unknown-linux-gnu ;;
        Linux-aarch64) echo aarch64-unknown-linux-gnu ;;
        MINGW64*-x86_64 | MSYS*-x86_64) echo x86_64-pc-windows-msvc ;;
        *) echo "unsupported host $(uname -s)-$(uname -m)" >&2; exit 1 ;;
    esac
}

ndk_home() {
    local ndk="${ANDROID_NDK_HOME:-$NDK_HOME}"
    if [ -z "$ndk" ] && [ -d "$HOME/Library/Android/sdk/ndk" ]; then
        ndk=$(ls -d "$HOME/Library/Android/sdk/ndk"/*/ 2>/dev/null | sort -V | tail -1 | sed 's:/$::')
    fi
    if [ -z "$ndk" ] || [ ! -d "$ndk" ]; then
        echo "Error: Android NDK not found. Set ANDROID_NDK_HOME or NDK_HOME." >&2
        exit 1
    fi
    echo "$ndk"
}

build_ffmpeg() {
    local target="$1"
    local flags=("${COMMON_FLAGS[@]}")
    local out="$OUTPUT_ROOT/$target"
    local jobs
    jobs=$(getconf _NPROCESSORS_ONLN 2>/dev/null || nproc 2>/dev/null || sysctl -n hw.ncpu)

    case "$target" in
        *-apple-darwin)
            local arch="${target%%-*}"; [ "$arch" = aarch64 ] && arch=arm64
            # Xcode's clang and SDK by path: an NDK or Homebrew clang earlier on PATH can't link for macOS.
            local cc
            cc=$(xcrun -f clang)
            SDKROOT=$(xcrun --sdk macosx --show-sdk-path)
            export SDKROOT
            flags+=(--enable-videotoolbox --enable-encoder=h264_videotoolbox
                --arch="$arch" --cc="$cc -arch $arch" --extra-cflags="-mmacosx-version-min=11.0"
                --extra-ldflags="-mmacosx-version-min=11.0")
            [ "$arch" != "$(uname -m)" ] && flags+=(--enable-cross-compile --target-os=darwin)
            ;;
        *-linux-android*)
            local ndk bin triple arch
            ndk=$(ndk_home)
            bin=$(ls -d "$ndk"/toolchains/llvm/prebuilt/*/bin | head -1)
            case "$target" in
                aarch64-*) triple=aarch64-linux-android; arch=aarch64 ;;
                armv7-*) triple=armv7a-linux-androideabi; arch=arm ;;
                x86_64-*) triple=x86_64-linux-android; arch=x86_64 ;;
            esac
            flags+=(--enable-cross-compile --target-os=android --arch="$arch"
                --cc="$bin/$triple$ANDROID_API-clang" --cxx="$bin/$triple$ANDROID_API-clang++"
                --ar="$bin/llvm-ar" --nm="$bin/llvm-nm" --ranlib="$bin/llvm-ranlib" --strip="$bin/llvm-strip"
                --enable-jni --enable-mediacodec --enable-encoder=h264_mediacodec
                --extra-ldflags="-Wl,-z,max-page-size=16384")
            # x86_64 assembly needs nasm; Android x86_64 is only the emulator.
            [ "$arch" = x86_64 ] && flags+=(--disable-x86asm)
            [ "$arch" = arm ] && flags+=(--enable-neon)
            ;;
        x86_64-pc-windows-msvc)
            if ! command -v cl.exe >/dev/null; then
                echo "Error: cl.exe not found. Run from MSYS2 inheriting a Visual Studio x64 environment." >&2
                exit 1
            fi
            # clang-cl compiles, MSVC links: cl.exe 14.44 hits an internal compiler error on FFmpeg 8.
            if ! command -v clang-cl >/dev/null; then
                echo "Error: clang-cl not found. Install LLVM or Visual Studio's C++ Clang tools." >&2
                exit 1
            fi
            # FFmpeg's Media Foundation encoder is written against D3D11. -MD: Rust's CRT.
            flags+=(--toolchain=msvc --cc=clang-cl --target-os=win64 --arch=x86_64
                --enable-mediafoundation --enable-d3d11va --enable-encoder=h264_mf
                --extra-cflags=-MD)
            ;;
        x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu)
            ;;
        *)
            echo "Error: unsupported target $target" >&2
            exit 1
            ;;
    esac
    if [[ "$target" == x86_64-* && "$target" != *android* ]] && ! command -v nasm >/dev/null; then
        echo "nasm not found: building without x86 SIMD (slower decoding)."
        flags+=(--disable-x86asm)
    fi

    echo ""
    echo "========================================="
    echo "Building FFmpeg $FFMPEG_VERSION for $target"
    echo "========================================="
    local src="$BUILD_DIR/ffmpeg-$FFMPEG_VERSION-$target"
    rm -rf "$src" "$out"
    mkdir -p "$src"
    tar xJf "$FFMPEG_TAR" -C "$src" --strip-components=1
    (
        cd "$src"
        ./configure --prefix="$out" "${flags[@]}"
        make -j"$jobs"
        make install
    )
    rm -rf "$src" "$out/share"
    # MSVC's linker looks for avcodec.lib where FFmpeg installs libavcodec.a.
    if [[ "$target" == *-windows-msvc ]]; then
        for a in "$out"/lib/lib*.a; do
            local name
            name=$(basename "$a" .a)
            mv "$a" "$out/lib/${name#lib}.lib"
        done
    fi
    echo "Done: $out"
}

targets=("$@")
[ ${#targets[@]} -eq 0 ] && targets=("$(host_target)")
for t in "${targets[@]}"; do
    build_ffmpeg "$t"
done
