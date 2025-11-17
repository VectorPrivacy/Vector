# Android Build Setup for Tauri on Apple Silicon (M-series chip)

## Overview
This guide documents how to set up a complete Android development environment for Vector on Apple Silicon (M1/M2/etc) Macs.

## Prerequisites

It is ideal that you first attempt a native compile (`npm run tauri dev`), ensuring that Vector builds and runs successfully natively, first and foremost; prior to moving on to cross-compiling.

### 1. Install Android Studio
Download and install from [developer.android.com](https://developer.android.com/studio)

### 2. Install SDK and NDK Components
Inside Android Studio, at the "Welcome to Android Studio" screen, hit "More Actions" -> "SDK Manager" -> "Languages & Frameworks" -> "Android SDK".

At this point, you'll want to install an Android Platform, we use Android 16.0 ("Baklava") for this example and for official APK Builds at this time.

Then, swap to the "SDK Tools" tab at the top, and ensure all of these tools are installed:
- Android SDK Build-Tools.
- NDK (Side by side).
- Android Emulator.
- Android SDK Platform-Tools.

Once all of these have installed, move to the next steps.

### 3. Install Homebrew packages
```bash
# Install Java
brew install openjdk@17
sudo ln -sfn /opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk /Library/Java/JavaVirtualMachines/openjdk-17.jdk
```

### 4. Install Rust Android targets
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
export NDK_HOME="$HOME/Library/Android/sdk/ndk/29.0.14206865"
export JAVA_HOME=$(/usr/libexec/java_home)
export PATH="$NDK_HOME/toolchains/llvm/prebuilt/darwin-x86_64/bin:$PATH"
export PATH="$PATH:$ANDROID_HOME/platform-tools"
export PATH="$PATH:$ANDROID_HOME/cmdline-tools/latest/bin"
export PATH="$PATH:$ANDROID_HOME/emulator"
```

**Note**: In case of a differing `NDK_HOME` version, you can use tab completion to select your installed version:
- Write, but don't hit enter: `export NDK_HOME="$HOME/Library/Android/sdk/ndk/`
- Hit "Tab", the detected NDK version will be appended.

Reload your environment:
```bash
source ~/.zshrc
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
```

### Clean build if issues persist
```bash
cd src-tauri && cargo clean
```

## Success Indicators
- ✅ `android dev` runs in Android Studio emulator
- ✅ `android build` produces APKs easily installable on physical Android phones

---

*This setup was last tested on a MacBook Pro M4 Max with Tauri v2.9.1, targeting Android API 26*