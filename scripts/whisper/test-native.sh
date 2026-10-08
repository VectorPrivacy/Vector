#!/usr/bin/env bash
# Builds whisper.cpp (the commit build.sh pins) with patches/ for this machine's CPU and checks
# that a reused whisper_state transcribes exactly as a fresh one: retries, other clips before it,
# shrinking audio_ctx, a clip over 30 s, a one-ULP change.
#   scripts/whisper/test-native.sh model.bin [-acft] a.wav b.wav [...]   (16 kHz mono PCM16)
# TEST_GPU=1 runs it on the GPU backend instead (Metal on macOS).
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CACHE="${VW_CACHE:-$HOME/.cache/vector-web-whisper}"
WHISPER_COMMIT=$(sed -n 's/^WHISPER_COMMIT=\([0-9a-f]*\).*/\1/p' "$HERE/../whisper-web/build.sh")
SRC="$CACHE/native-src"
BUILD="$CACHE/native-build"

if [ ! -d "$CACHE/whisper.cpp" ]; then
    git clone -q https://github.com/ggml-org/whisper.cpp "$CACHE/whisper.cpp"
fi
git -C "$CACHE/whisper.cpp" cat-file -e "$WHISPER_COMMIT^{commit}" 2>/dev/null || git -C "$CACHE/whisper.cpp" fetch -q origin
rm -rf "$SRC" && mkdir -p "$SRC"
git -C "$CACHE/whisper.cpp" archive "$WHISPER_COMMIT" | tar -x -C "$SRC"
git -C "$SRC" init -q
for p in "$HERE"/patches/*.patch; do
    git -C "$SRC" apply --whitespace=nowarn "$p"
done

cmake -S "$SRC" -B "$BUILD" -G Ninja -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
    -DWHISPER_BUILD_TESTS=OFF -DWHISPER_BUILD_EXAMPLES=OFF -DGGML_OPENMP=OFF -DGGML_CCACHE=OFF >/dev/null
cmake --build "$BUILD" --target whisper -j "$(sysctl -n hw.ncpu 2>/dev/null || nproc)" >/dev/null
libs=$(find "$BUILD" -name '*.a' | tr '\n' ' ')
case "$(uname)" in
    Darwin) link=(-Wl,-all_load $libs -framework Accelerate -framework Foundation -framework Metal -framework MetalKit) ;;
    *)      link=(-Wl,--whole-archive $libs -Wl,--no-whole-archive -lpthread) ;;
esac
c++ -O2 -std=c++17 -I"$SRC/include" -I"$SRC/ggml/include" "$HERE/test-native.cpp" -o "$BUILD/test-native" "${link[@]}"
"$BUILD/test-native" "$@"
