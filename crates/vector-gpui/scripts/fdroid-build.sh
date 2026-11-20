#!/usr/bin/env bash
# F-Droid build recipe for the GPUI client.
#
# One script for F-Droid's builder and for CI, so the published flavour and F-Droid's rebuild of
# it cannot drift (as scripts/fdroid-build.sh is for the Tauri client). Output is an UNSIGNED
# arm64 release APK of the `fdroid` feature: no in-app updater, and Gradle leaves out the
# embedded FCM distributor, so push is UnifiedPush through a distributor app.
#
#   crates/vector-gpui/scripts/fdroid-build.sh prepare   # network: toolchain, cargo fetch
#   crates/vector-gpui/scripts/fdroid-build.sh build     # cargo offline; Gradle resolves its own
set -euo pipefail
cd "$(dirname "$0")/../../.."

export PATH="$HOME/.cargo/bin:$PATH"

# Pinned here and in gpui-android.yaml: F-Droid's rebuild has to use the released flavour's rustc.
RUST_TOOLCHAIN=1.98.0
export RUSTUP_TOOLCHAIN="$RUST_TOOLCHAIN"
TARGET=aarch64-linux-android

prepare() {
    rustup toolchain install "$RUST_TOOLCHAIN" --profile minimal --target "$TARGET"
    (cd crates/vector-gpui && cargo fetch --locked --target "$TARGET")
}

build() {
    : "${NDK_HOME:?NDK_HOME must point at the Android NDK}"
    # A path dependency outside the workspace root is hashed by its absolute path into every
    # mangled symbol, so the build always runs from one fixed directory.
    local build_root="$HOME/vector-gpui-fdroid-build" src_tree=""
    if [ "$(pwd -P)" != "$build_root" ]; then
        src_tree=$(pwd -P)
        rm -rf "$build_root"
        mkdir -p "$build_root"
        cp -a . "$build_root/"
        cd "$build_root"
    fi
    export VECTOR_FDROID=1
    export CARGO_NET_OFFLINE=true
    export ANDROID_NDK_HOME="$NDK_HOME"
    # The NDK ships only llvm-* binutils; vendored C builds ask for a ranlib by its GNU name.
    local ndk_bin
    ndk_bin=$(ls -d "$NDK_HOME"/toolchains/llvm/prebuilt/*/bin | head -1)
    export PATH="$ndk_bin:$PATH"
    export RANLIB_aarch64_linux_android="$ndk_bin/llvm-ranlib"
    export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --pretty=%ct)}"
    # Reproducibility: no builder-specific paths in the library. Cargo canonicalises paths, so a
    # checkout reached through a symlink bakes in the physical one: remap both spellings, and
    # both spellings of HOME for the cargo caches. The std sources map to /rustc/<commit>, as a
    # toolchain without rust-src embeds them.
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
    CARGO_ENCODED_RUSTFLAGS=$(printf '%s\x1f' "${remaps[@]}")
    CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS%$'\x1f'}"
    # C built through the cc crate embeds __FILE__ in its asserts.
    export CFLAGS="-ffile-prefix-map=$repo_physical=/vector -ffile-prefix-map=$repo_logical=/vector"
    export CXXFLAGS="$CFLAGS"
    (cd crates/vector-gpui/android &&
        ./gradlew --no-daemon :app:assembleRelease -PvectorNdk="$(basename "$NDK_HOME")" -PvectorAbis=arm64-v8a)
    local out=crates/vector-gpui/android/app/build/outputs/apk/release
    ls -l "$out/"
    if [ -n "$src_tree" ]; then
        mkdir -p "$src_tree/$out"
        cp -a "$out"/. "$src_tree/$out/"
    fi
}

case "${1:-all}" in
    prepare) prepare ;;
    build) build ;;
    all) prepare; build ;;
    *) echo "usage: $0 [prepare|build|all]" >&2; exit 2 ;;
esac
