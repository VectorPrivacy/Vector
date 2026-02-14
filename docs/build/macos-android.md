# Vector macOS → Android Compiling Guide

A comprehensive guide to cross-compile Vector for Android from macOS (Apple Silicon).

**Build Target:** Android APK - Cross-compiled from macOS for Android devices

---

## Prerequisites

> **Note:** First ensure Vector builds natively (`npm run tauri dev`) before attempting cross-compilation.

### 1. Install Android Studio

Download and install Android Studio from [developer.android.com](https://developer.android.com/studio)

### 2. Install SDK and NDK Components

Inside Android Studio, navigate to: **Welcome Screen** → **More Actions** → **SDK Manager** → **Languages & Frameworks** → **Android SDK**

Install an Android Platform (we use Android 16.0 "Baklava" for official builds).

Then, switch to the **SDK Tools** tab and install the following:

| Component | Purpose |
|-----------|---------|
| Android SDK Build-Tools | Compiles and packages Android apps |
| NDK (Side by side) | Native Development Kit for Rust compilation |
| Android Emulator | Virtual device for testing |
| Android SDK Platform-Tools | ADB and other platform utilities |

Once all components are installed, proceed to the next steps.

### 3. Install Java via Homebrew

Install OpenJDK 17 (required for Android builds):

```bash
brew install openjdk@17
sudo ln -sfn /opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk /Library/Java/JavaVirtualMachines/openjdk-17.jdk
```

Verify the installation:

```bash
java --version
```

### 4. Install Rust Android Targets

Add all Android target architectures to your Rust installation:

```bash
rustup target add aarch64-linux-android
rustup target add armv7-linux-androideabi
rustup target add x86_64-linux-android
rustup target add i686-linux-android
```

| Target | Architecture | Usage |
|--------|--------------|-------|
| `aarch64-linux-android` | ARM64 | Modern phones (>95% of devices) |
| `armv7-linux-androideabi` | ARM32 | Older devices |
| `x86_64-linux-android` | x86_64 | Emulators, Chromebooks |
| `i686-linux-android` | x86 | Older emulators |

Verify the targets are installed:

```bash
rustup target list | grep android
```

---

## Environment Configuration

Add the following to your `~/.zshrc`:

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

> **Tip:** Your NDK version may differ. Use tab completion to find yours:
> - Type: `export NDK_HOME="$HOME/Library/Android/sdk/ndk/`
> - Press **Tab** to auto-complete with your installed version.

Reload your environment:

```bash
source ~/.zshrc
```

Verify the configuration:

```bash
echo "ANDROID_HOME: $ANDROID_HOME"
echo "NDK_HOME: $NDK_HOME"
echo "JAVA_HOME: $JAVA_HOME"
```

---

## Building Vector for Android

### 1. Clone the Repository

```bash
cd ~
git clone https://github.com/VectorPrivacy/Vector.git
cd Vector
```

### 2. Install Node Dependencies

```bash
npm install
```

### 3. Development Build (Emulator)

Start an Android emulator before building:

1. Open **Android Studio**
2. Click **More Actions** in the Projects tab
3. Open **Virtual Device Manager**
4. Launch a virtual device and wait for it to fully boot

Once the emulator is running:

```bash
npm run tauri android dev --open
```

This will compile and install the debug build on your emulator.

### 4. Release Build (APK)

To create a distributable APK, you'll need to sign it first.

**Configure APK Signing:**

Follow the [Tauri v2 Android Signing documentation](https://tauri.app/distribute/sign/android/) to set up APK signing. Vector's signing configuration is pre-applied — you only need to add your Keystore file.

**Build the APK:**

```bash
# Recommended: Build for aarch64 (>95% of devices)
npm run tauri android build -- --apk true --target aarch64

# Build for all architectures (larger binary, not recommended)
npm run tauri android build -- --apk true
```

### 5. Locate the Built APK

After successful compilation, find your APK at:

```bash
ls -la src-tauri/gen/android/app/build/outputs/apk/
```

---

## Build Commands Reference

| Command | Description |
|---------|-------------|
| `npm run tauri android dev` | Development build on emulator |
| `npm run tauri android dev --open` | Dev build, opens Android Studio |
| `npm run tauri android build -- --apk true` | Release APK (all architectures) |
| `npm run tauri android build -- --apk true --target aarch64` | Release APK (ARM64 only, recommended) |

---

## Troubleshooting

### NDK not found

```
error: could not find NDK
```

**Solution:** Verify `NDK_HOME` points to a valid NDK installation:
```bash
ls $NDK_HOME
```

If empty or error, reinstall NDK via Android Studio SDK Manager.

### Java version mismatch

```
error: Unsupported class file major version
```

**Solution:** Ensure you're using Java 17:
```bash
java --version
```

If not Java 17, check your `JAVA_HOME` configuration.

### Emulator not detected

```
error: no devices/emulators found
```

**Solution:** Ensure the emulator is fully booted before building. Check connected devices:
```bash
adb devices
```

### Clean build if issues persist

```bash
cd src-tauri && cargo clean
```

---

## Success Indicators

- `android dev` launches the app in the Android Studio emulator
- `android build` produces APKs installable on physical Android devices
- No errors during the Rust compilation phase

---

## System Requirements

- **OS:** macOS 12.0+ (Apple Silicon recommended)
- **RAM:** 8GB minimum, 16GB recommended
- **Disk:** 20GB free space for Android SDK, NDK, and build cache
- **Software:** Android Studio, Xcode Command Line Tools

---

## Additional Resources

- [Vector GitHub Repository](https://github.com/VectorPrivacy/Vector)
- [Vector Discord Community](https://discord.gg/ar2pnE9Huy)
- [Tauri Android Prerequisites](https://v2.tauri.app/start/prerequisites/#android)
- [Tauri Android Signing Guide](https://tauri.app/distribute/sign/android/)
- [Android Studio Download](https://developer.android.com/studio)

---

*Last tested on MacBook Pro M4 Max with Tauri v2.9.1, targeting Android API 26*