# Vector Ubuntu Compiling Guide

A comprehensive guide to compile Vector (bare build) on Ubuntu.

**Build Target:** `build:bare` - Minimal build without AI features (no Whisper voice AI or Vulkan GPU dependencies)

---

## Prerequisites

### 1. System Dependencies

Install the required system packages for Tauri development:

```bash
sudo apt update
sudo apt install -y \
  build-essential \
  curl \
  wget \
  file \
  git \
  pkg-config \
  libwebkit2gtk-4.1-dev \
  libxdo-dev \
  libssl-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  libasound2-dev
```

| Package | Purpose |
|---------|---------|
| `build-essential` | C/C++ compiler and build tools (gcc, g++, make) |
| `curl` / `wget` | Network utilities for downloading |
| `git` | Version control for cloning the repository |
| `pkg-config` | Helper tool for compiling libraries |
| `libwebkit2gtk-4.1-dev` | WebKit rendering engine for Tauri UI |
| `libxdo-dev` | X11 input simulation library |
| `libssl-dev` | OpenSSL development headers |
| `libayatana-appindicator3-dev` | System tray integration |
| `librsvg2-dev` | SVG rendering library |
| `libasound2-dev` | ALSA audio library for sound support |

### 2. Install Rust

Install Rust using rustup (the official Rust toolchain installer):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

When prompted, select the default installation (option 1).

After installation, load Rust into your current shell:

```bash
source "$HOME/.cargo/env"
```

Verify the installation:

```bash
rustc --version
cargo --version
```

### 3. Install Node.js

Install Node.js (v18 or later recommended). Using NodeSource:

```bash
curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
sudo apt install -y nodejs
```

Or using nvm (Node Version Manager) - recommended for development:

```bash
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.1/install.sh | bash
source ~/.bashrc
nvm install 20
nvm use 20
```

Verify the installation:

```bash
node --version
npm --version
```

---

## Building Vector (Bare)

### 1. Clone the Repository

```bash
cd ~
git clone https://github.com/VectorPrivacy/Vector.git
cd Vector
```

### 2. Install Node Dependencies

Install the Tauri CLI and project dependencies:

```bash
npm install
```

This installs:
- `@tauri-apps/cli` - Tauri build tooling
- Various Tauri plugins (clipboard, dialog, filesystem, etc.)

### 3. Compile the Bare Build

Run the bare build command (excludes Whisper AI and Vulkan dependencies):

```bash
npm run build:bare
```

This executes `tauri build --no-default-features` which:
- Compiles the Rust backend without the `whisper` feature
- Bundles the frontend assets
- Creates the final application package

**Note:** The first build will take longer as Cargo downloads and compiles all Rust dependencies.

### 4. Locate the Built Application

After successful compilation, find your executables at:

```bash
ls -la src-tauri/target/release/
```

The main executable is `vector` (or `Vector` depending on configuration).

For packaged installers (`.deb`, `.AppImage`):

```bash
ls -la src-tauri/target/release/bundle/
```

---

## Build Commands Reference

| Command | Description |
|---------|-------------|
| `npm run build:bare` | Production build without AI features |
| `npm run dev:bare` | Development mode without AI features |
| `npm run build` | Full production build (includes Whisper AI) |
| `npm run dev` | Full development mode |

---

## What's Excluded in Bare Build

The `build:bare` variant excludes:

- **Whisper-rs** - OpenAI Whisper speech recognition
- **Vulkan dependencies** - GPU acceleration for ML models
- **Voice AI processing** - All speech-to-text functionality

This results in:
- Smaller binary size
- Fewer system dependencies
- Reduced attack surface
- No GPU/ML library requirements

---

## Troubleshooting

### WebKit not found

```
error: could not find system library 'webkit2gtk-4.1'
```

**Solution:** Install the WebKit development package:
```bash
sudo apt install libwebkit2gtk-4.1-dev
```

### OpenSSL errors

```
error: failed to run custom build command for `openssl-sys`
```

**Solution:** Install OpenSSL development headers:
```bash
sudo apt install libssl-dev pkg-config
```

### Rust not found after installation

**Solution:** Source the cargo environment:
```bash
source "$HOME/.cargo/env"
```

Or add to your `~/.bashrc`:
```bash
echo 'source "$HOME/.cargo/env"' >> ~/.bashrc
source ~/.bashrc
```

### Permission denied during npm install

**Solution:** Fix npm permissions or use nvm:
```bash
mkdir -p ~/.npm-global
npm config set prefix '~/.npm-global'
echo 'export PATH=~/.npm-global/bin:$PATH' >> ~/.bashrc
source ~/.bashrc
```

### Build fails with memory errors

Large Rust projects can be memory-intensive. If you have limited RAM:

```bash
# Limit parallel compilation
export CARGO_BUILD_JOBS=2

# Then build
npm run build:bare
```

---

## Updating Vector

To update to the latest version:

```bash
cd ~/Vector
git pull origin master
npm install
npm run build:bare
```

---

## System Requirements

- **OS:** Ubuntu 24.04+ (or Debian-based distribution)
- **RAM:** 4GB minimum, 8GB recommended
- **Disk:** 60GB free space for build cache and artifacts
- **CPU:** Multi-core processor recommended for faster compilation

---

## Additional Resources

- [Vector GitHub Repository](https://github.com/VectorPrivacy/Vector)
- [Vector Discord Community](https://discord.gg/ar2pnE9Huy)
- [Tauri Prerequisites Guide](https://v2.tauri.app/start/prerequisites/)
- [Rust Installation Guide](https://www.rust-lang.org/tools/install)