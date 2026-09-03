#!/usr/bin/env bash
# F-Droid build recipe.
#
# One script for F-Droid's builder (docs/fdroid/io.vectorapp.yml calls the two
# stages) and for CI, so the published flavour and F-Droid's rebuild of it
# cannot drift. Source only: the Tauri CLI comes from cargo, Svelte is bundled
# with rollup, and every prebuilt npm binary is removed before the build.
# Output is an UNSIGNED universal APK (arm64) of the `fdroid` Cargo feature.
#
#   scripts/fdroid-build.sh prepare   # network: npm ci, cargo fetch
#   scripts/fdroid-build.sh build     # offline
set -euo pipefail
cd "$(dirname "$0")/.."

: "${NDK_HOME:?NDK_HOME must point at the Android NDK}"
export ANDROID_NDK_HOME="$NDK_HOME"
export ANDROID_NDK="$NDK_HOME"   # cmake's Android toolchain (whisper.cpp)
export PATH="$HOME/.cargo/bin:$PATH"

NATIVE_NPM_PACKAGES=(
    node_modules/@tauri-apps/cli node_modules/@tauri-apps/cli-*
    node_modules/esbuild node_modules/@esbuild
    node_modules/lightningcss node_modules/lightningcss-*
    node_modules/fsevents
)

prepare() {
    npm ci --ignore-scripts
    rm -rf "${NATIVE_NPM_PACKAGES[@]}"
    if find node_modules -name '*.node' -o -name '*.wasm' | grep -q .; then
        echo "prebuilt binaries remain in node_modules:" >&2
        find node_modules -name '*.node' -o -name '*.wasm' >&2
        exit 1
    fi
    (cd src-tauri && cargo fetch --locked)
}

build() {
    export TAURI_CLI="cargo tauri"
    export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --pretty=%ct)}"
    # Reproducibility: no builder-specific paths in the binary. Setting
    # rustflags this way replaces the target flags the Tauri CLI would set, so
    # its three Android link libraries are carried along. The std sources map
    # to the same /rustc/<commit> form a toolchain without rust-src embeds.
    local sysroot commit
    sysroot=$(rustc --print sysroot)
    commit=$(rustc -vV | sed -n 's/^commit-hash: //p')
    export CARGO_ENCODED_RUSTFLAGS
    CARGO_ENCODED_RUSTFLAGS=$(printf '%s\x1f' \
        "-Clink-arg=-landroid" "-Clink-arg=-llog" "-Clink-arg=-lOpenSLES" \
        "--remap-path-prefix=$HOME/.cargo/registry/src=/cargo/registry/src" \
        "--remap-path-prefix=$HOME/.cargo/git/checkouts=/cargo/git/checkouts" \
        "--remap-path-prefix=$sysroot/lib/rustlib/src/rust=/rustc/$commit" \
        "--remap-path-prefix=$PWD=/vector")
    CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS%$'\x1f'}"
    # whisper.cpp is C/C++ built by cmake; its asserts embed __FILE__.
    export CFLAGS="-ffile-prefix-map=$PWD=/vector"
    export CXXFLAGS="$CFLAGS"
    cargo tauri android build --apk true --target aarch64 \
        --config src-tauri/tauri.fdroid.conf.json
    ls -l src-tauri/gen/android/app/build/outputs/apk/universal/release/
}

case "${1:-all}" in
    prepare) prepare ;;
    build) build ;;
    all) prepare; build ;;
    *) echo "usage: $0 [prepare|build|all]" >&2; exit 2 ;;
esac
