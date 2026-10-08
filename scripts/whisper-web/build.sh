#!/usr/bin/env bash
# Builds Vector Web's Whisper from whisper.cpp at a pinned commit plus ../whisper/patches (shared
# with the native builds, which whisper-rs-sys patches through WHISPER_PATCHES):
#   web/whisper/whisper-gpu.{js,wasm}  WebGPU, the model on the GPU (Asyncify, no threads)
#   web/whisper/whisper-cpu.{js,wasm}  CPU on pthreads, for a cross-origin isolated page without WebGPU
# The toolchain and sources land in $VW_CACHE (default ~/.cache/vector-web-whisper).
set -euo pipefail

WHISPER_COMMIT=d1be6fde11ac6e0407606b4e42fe72d34add8037 # v1.9.5
EMSDK_VERSION=6.0.11
DAWN_TAG=v20260317.182325
DAWN_SHA256=8dcae86c630d76b6794b271c6572becba36f4237d132bee121a56638c3d76575

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
PATCHES="$ROOT/scripts/whisper/patches"
CACHE="${VW_CACHE:-$HOME/.cache/vector-web-whisper}"
OUT="$ROOT/web/whisper"
mkdir -p "$CACHE" "$OUT"

# emsdk needs Python 3.10+, which macOS's system python3 is not.
if [ -z "${EMSDK_PYTHON:-}" ]; then
    for py in python3.14 python3.13 python3.12 python3.11 python3.10 python3; do
        if command -v "$py" >/dev/null && "$py" -c 'import sys; sys.exit(sys.version_info < (3, 10))'; then
            export EMSDK_PYTHON="$(command -v "$py")"
            break
        fi
    done
fi

if [ ! -d "$CACHE/emsdk" ]; then
    git clone -q https://github.com/emscripten-core/emsdk.git "$CACHE/emsdk"
fi
(cd "$CACHE/emsdk" && ./emsdk install "$EMSDK_VERSION" >/dev/null && ./emsdk activate "$EMSDK_VERSION" >/dev/null)
# shellcheck disable=SC1091
source "$CACHE/emsdk/emsdk_env.sh" >/dev/null 2>&1

DAWN="$CACHE/emdawnwebgpu-$DAWN_TAG"
if [ ! -d "$DAWN" ]; then
    curl -sfL -o "$CACHE/emdawn.zip" "https://github.com/google/dawn/releases/download/$DAWN_TAG/emdawnwebgpu_pkg-$DAWN_TAG.zip"
    echo "$DAWN_SHA256  $CACHE/emdawn.zip" | shasum -a 256 -c - >/dev/null || { echo "[whisper-web] Dawn package checksum mismatch" >&2; exit 1; }
    rm -rf "$CACHE/emdawn-unzip" && mkdir "$CACHE/emdawn-unzip"
    unzip -q "$CACHE/emdawn.zip" -d "$CACHE/emdawn-unzip"
    mv "$CACHE/emdawn-unzip/emdawnwebgpu_pkg" "$DAWN"
    rm -rf "$CACHE/emdawn-unzip" "$CACHE/emdawn.zip"
fi

if [ ! -d "$CACHE/whisper.cpp" ]; then
    git clone -q https://github.com/ggml-org/whisper.cpp "$CACHE/whisper.cpp"
fi
git -C "$CACHE/whisper.cpp" cat-file -e "$WHISPER_COMMIT^{commit}" 2>/dev/null || git -C "$CACHE/whisper.cpp" fetch -q origin
SRC="$CACHE/src"
rm -rf "$SRC" && mkdir -p "$SRC"
git -C "$CACHE/whisper.cpp" archive "$WHISPER_COMMIT" | tar -x -C "$SRC"
# A repository of its own, so `git apply` never resolves paths against an enclosing one.
git -C "$SRC" init -q
for p in "$PATCHES"/*.patch; do
    git -C "$SRC" apply --whitespace=nowarn "$p"
done
grep -q mul_mat_tile8 "$SRC/ggml/src/ggml-webgpu/ggml-webgpu.cpp" || { echo "[whisper-web] patches did not apply" >&2; exit 1; }

# EMSCRIPTEN_SYSTEM_PROCESSOR: Emscripten reports x86 by default, which ggml-cpu takes for an
# unknown CPU and builds its scalar fallback instead of the wasm SIMD kernels.
build() { # <variant> <cmake args...>
    local name="$1"; shift
    local dir="$CACHE/build-$name"
    # A cache configured from another checkout of these scripts refuses this one.
    if [ -f "$dir/CMakeCache.txt" ] && ! grep -qxF "CMAKE_HOME_DIRECTORY:INTERNAL=$HERE" "$dir/CMakeCache.txt"; then
        rm -rf "$dir"
    fi
    emcmake cmake -DEMSCRIPTEN_SYSTEM_PROCESSOR=wasm32 -S "$HERE" -B "$dir" -G Ninja -DCMAKE_BUILD_TYPE=Release -DWHISPER_DIR="$SRC" "$@" >/dev/null
    cmake --build "$dir" --target vector_whisper -j "$(sysctl -n hw.ncpu 2>/dev/null || nproc)" >/dev/null
    sed -e "s/vector_whisper\.wasm/whisper-$name.wasm/g" -e "s/vector_whisper\.js/whisper-$name.js/g" "$dir/vector_whisper.js" > "$OUT/whisper-$name.js"
    cp "$dir/vector_whisper.wasm" "$OUT/whisper-$name.wasm"
    echo "[whisper-web] $name: $(wc -c < "$OUT/whisper-$name.wasm" | tr -d ' ') bytes"
}

build gpu -DVW_THREADS=OFF -DEMDAWNWEBGPU_DIR="$DAWN"
build cpu -DVW_THREADS=ON

# What the committed modules were built from.
{
    echo "whisper.cpp $WHISPER_COMMIT"
    echo "emsdk $EMSDK_VERSION"
    echo "emdawnwebgpu $DAWN_TAG"
    (cd "$ROOT/scripts" && shasum -a 256 whisper/patches/*.patch whisper-web/vector_whisper.cpp whisper-web/CMakeLists.txt)
    (cd "$OUT" && shasum -a 256 whisper-gpu.js whisper-gpu.wasm whisper-cpu.js whisper-cpu.wasm)
} > "$OUT/BUILD.txt"
