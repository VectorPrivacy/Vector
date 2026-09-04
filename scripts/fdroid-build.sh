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

export PATH="$HOME/.cargo/bin:$PATH"

# The one place the toolchain is pinned: F-Droid's rebuild has to compile
# with the same rustc and Tauri CLI as the released flavour it is compared to.
RUST_TOOLCHAIN=1.98.0
TAURI_CLI_VERSION=2.9.6
export RUSTUP_TOOLCHAIN="$RUST_TOOLCHAIN"

prepare() {
    rustup toolchain install "$RUST_TOOLCHAIN" --profile minimal --target aarch64-linux-android
    cargo install tauri-cli --version "$TAURI_CLI_VERSION" --locked
    npm ci --ignore-scripts
    # Globs expand here, after npm ci: a fresh clone has no node_modules yet.
    rm -rf node_modules/@tauri-apps/cli node_modules/@tauri-apps/cli-* \
           node_modules/esbuild node_modules/@esbuild \
           node_modules/lightningcss node_modules/lightningcss-* \
           node_modules/fsevents
    if find node_modules -name '*.node' -o -name '*.wasm' | grep -q .; then
        echo "prebuilt binaries remain in node_modules:" >&2
        find node_modules -name '*.node' -o -name '*.wasm' >&2
        exit 1
    fi
    (cd src-tauri && cargo fetch --locked)
}

build() {
    : "${NDK_HOME:?NDK_HOME must point at the Android NDK}"
    export ANDROID_NDK_HOME="$NDK_HOME"
    export ANDROID_NDK="$NDK_HOME"   # cmake's Android toolchain (whisper.cpp)
    export TAURI_CLI="cargo tauri"
    # The NDK ships only the llvm-* binutils. Vendored OpenSSL asks `cc` for an
    # archiver and ranlib; without these it guesses the GNU-prefixed names.
    local ndk_bin
    ndk_bin=$(ls -d "$NDK_HOME"/toolchains/llvm/prebuilt/*/bin | head -1)
    export PATH="$ndk_bin:$PATH"
    export AR_aarch64_linux_android="$ndk_bin/llvm-ar"
    export RANLIB_aarch64_linux_android="$ndk_bin/llvm-ranlib"
    export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --pretty=%ct)}"
    # Reproducibility: no builder-specific paths in the binary. Setting
    # rustflags this way replaces the target flags the Tauri CLI would set, so
    # its three Android link libraries are carried along. The std sources map
    # to the same /rustc/<commit> form a toolchain without rust-src embeds.
    # Cargo canonicalises paths, so a checkout reached through a symlink
    # (F-Droid's CI) bakes in the physical path: remap both spellings, and
    # both spellings of HOME for the cargo caches.
    local sysroot commit repo_logical repo_physical home_physical
    sysroot=$(rustc --print sysroot)
    commit=$(rustc -vV | sed -n 's/^commit-hash: //p')
    repo_logical=$PWD
    repo_physical=$(pwd -P)
    home_physical=$(cd "$HOME" && pwd -P)
    local -a remaps=(
        "--remap-path-prefix=$repo_physical=/vector"
        "--remap-path-prefix=$home_physical/.cargo/registry/src=/cargo/registry/src"
        "--remap-path-prefix=$home_physical/.cargo/git/checkouts=/cargo/git/checkouts"
        "--remap-path-prefix=$sysroot/lib/rustlib/src/rust=/rustc/$commit"
    )
    if [ "$repo_logical" != "$repo_physical" ]; then
        remaps+=("--remap-path-prefix=$repo_logical=/vector")
    fi
    if [ "$HOME" != "$home_physical" ]; then
        remaps+=("--remap-path-prefix=$HOME/.cargo/registry/src=/cargo/registry/src"
                 "--remap-path-prefix=$HOME/.cargo/git/checkouts=/cargo/git/checkouts")
    fi
    export CARGO_ENCODED_RUSTFLAGS
    CARGO_ENCODED_RUSTFLAGS=$(printf '%s\x1f' \
        "-Clink-arg=-landroid" "-Clink-arg=-llog" "-Clink-arg=-lOpenSLES" "${remaps[@]}")
    CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS%$'\x1f'}"
    # whisper.cpp is C/C++ built by cmake; its asserts embed __FILE__.
    export CFLAGS="-ffile-prefix-map=$repo_physical=/vector -ffile-prefix-map=$repo_logical=/vector"
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
