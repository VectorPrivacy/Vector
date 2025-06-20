# Android Build Setup for Tauri on Apple Silicon (M-series chip)

## Overview
This guide documents how to set up a complete Android development environment for Vector on Apple Silicon (M1/M2/etc) Macs, focusing on resolving common cross-compilation issues with potential Clang linker issues.

## Prerequisites

It is ideal that you first attempt a native compile (`npm run tauri dev`), ensuring that Vector builds and runs successfully natively, first and foremost; prior to moving on to cross-compiling.

### 1. Install Android Studio
Download and install from [developer.android.com](https://developer.android.com/studio)

### 2. Install Homebrew packages
```bash
# Install Java
brew install openjdk@17
sudo ln -sfn /opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk /Library/Java/JavaVirtualMachines/openjdk-17.jdk

# Install Android NDK
brew install --cask android-ndk
```

### 3. Install Rust Android targets
```bash
rustup target add aarch64-linux-android
rustup target add armv7-linux-androideabi
rustup target add x86_64-linux-android
rustup target add i686-linux-android
```

## Environment Configuration

### 1. Set up environment variables
Add to `~/.zshrc`:

```bash
# Android Development Environment for Tauri
export ANDROID_HOME="$HOME/Library/Android/sdk"
export NDK_HOME="/opt/homebrew/Caskroom/android-ndk/28b/AndroidNDK13356709.app/Contents/NDK"
export JAVA_HOME=$(/usr/libexec/java_home)
export PATH="$NDK_HOME/toolchains/llvm/prebuilt/darwin-x86_64/bin:$PATH"
export PATH="$PATH:$ANDROID_HOME/platform-tools"
export PATH="$PATH:$ANDROID_HOME/cmdline-tools/latest/bin"
export PATH="$PATH:$ANDROID_HOME/emulator"
```

Reload your environment:
```bash
source ~/.zshrc
```

## Fixing Cross-Compilation Issues

### 1. Create missing NDK tool symlinks
```bash
cd $NDK_HOME/toolchains/llvm/prebuilt/darwin-x86_64/bin/
sudo ln -sf llvm-ranlib aarch64-linux-android-ranlib
sudo ln -sf llvm-ar aarch64-linux-android-ar
```

### 2. Fix AAudio linking issue
The linker may fail to find AAudio (used by Vector Voice) when using API level 24. Create wrapper scripts to force API level 26+ in the Tauri Build System:

```bash
cd $NDK_HOME/toolchains/llvm/prebuilt/darwin-x86_64/bin/

# Create wrapper for clang
sudo tee aarch64-linux-android24-clang << 'EOF'
#!/bin/bash
exec "$(dirname "$0")/aarch64-linux-android26-clang" "$@"
EOF
sudo chmod +x aarch64-linux-android24-clang

# Create wrapper for clang++
sudo tee aarch64-linux-android24-clang++ << 'EOF'
#!/bin/bash
exec "$(dirname "$0")/aarch64-linux-android26-clang++" "$@"
EOF
sudo chmod +x aarch64-linux-android24-clang++
```

## Building Your App

### Development build with Android Studio emulator

You'll need to be running an emulation device before performing the build, as such, please ensure **Android Studio** is open, click **More Actions** in the **Projects** tab, open **Virtual Device Manager**, and launch a device, once it's fully booted, you can continue building.

```bash
npm run tauri android dev --open
```

### Release build

To be able to install the APK, you'll need to Sign the APK, please follow the [Tauri v2 Android Signing documentation](https://tauri.app/distribute/sign/android/) to configure APK signing, it is possible to self-sign, making the process fairly straightforward.

All signing code and configuration edits have already been applied to the Vector Android files, as such, **you only need to add your Keystore file to complete the signing process.**

```bash
# Build an APK for a specific architecture (aarch64 is the standard at >95% adoption, thus, the recommended APK arch)
npm run tauri android build -- --apk --target aarch64

# Build an APK for all architectures (bulky binary, not recommended)
npm run tauri android build -- --apk
```

## Troubleshooting

### Verify environment setup
```bash
echo "ANDROID_HOME: $ANDROID_HOME"
echo "NDK_HOME: $NDK_HOME"
echo "JAVA_HOME: $JAVA_HOME"
which aarch64-linux-android-ranlib
```

### Clean build if issues persist
```bash
cd src-tauri && cargo clean
```

## Key Takeaways

1. **OpenSSL vendoring**: Essential for cross-compilation - the `vendored` feature compiles OpenSSL from source for the target architecture
2. **NDK toolchain path**: Must be in PATH for build tools to find `ranlib`, `ar`, etc.
3. **API level consistency**: AAudio requires API 26+, so force this even when build system requests API 24 (due to Tauri bug?)
4. **M1 compatibility**: Use `darwin-x86_64` NDK binaries - they work fine via Rosetta 2

## Success Indicators
- ✅ No "archive member is neither ET_REL nor LLVM bitcode" warnings
- ✅ No "unable to find library -laaudio" errors
- ✅ `android dev` runs in Android Studio emulator
- ✅ `android build` produces APKs easily installable on physical Android phones

---

*This setup was tested on MacBook Air M1 with Tauri v2.5.1, targeting Android API 26*