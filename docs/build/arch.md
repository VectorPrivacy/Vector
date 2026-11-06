# Vector Arch Linux Compiling Guide

Compiling Vector on Arch Linux and Arch-based distributions (Manjaro, EndeavourOS, Omarchy).

Two builds are covered: **bare** (`build:bare`, no Whisper voice AI, Tor or video) and **full**
(`build`, everything). The full build's only extra requirement is the Vulkan toolchain Whisper
uses for GPU transcription.

---

## Prerequisites

### 1. System Dependencies

```bash
sudo pacman -Syu --needed \
  base-devel \
  curl \
  wget \
  file \
  git \
  openssl \
  webkit2gtk-4.1 \
  libappindicator-gtk3 \
  librsvg \
  xdotool \
  alsa-lib
```

| Package | Purpose |
|---------|---------|
| `base-devel` | C/C++ compiler and build tools (gcc, make, pkgconf) |
| `webkit2gtk-4.1` | WebKit rendering engine for Tauri UI |
| `libappindicator-gtk3` | System tray integration |
| `librsvg` | SVG rendering library |
| `xdotool` | X11 input simulation library |
| `alsa-lib` | ALSA audio library for sound and calls |
| `openssl` | OpenSSL libraries and headers |

### 2. Full Build Only: Vulkan

Whisper compiles its GPU shaders at build time, so the full build needs the Vulkan headers,
loader, the SPIR-V headers and `glslc` (from `shaderc`):

```bash
sudo pacman -S --needed vulkan-headers vulkan-icd-loader spirv-headers shaderc
```

A missing `spirv-headers` is the usual cause of a full build failing while the bare build works.

### 3. Rust and Node.js

```bash
sudo pacman -S --needed rustup nodejs npm
rustup default stable
```

---

## Building Vector

```bash
git clone https://github.com/VectorPrivacy/Vector.git
cd Vector
npm install
```

Then one of:

```bash
npm run build:bare   # bare: no Vulkan needed
npm run build        # full: needs the Vulkan packages above
```

The executable lands in `src-tauri/target/release/`, and the packaged bundles in
`src-tauri/target/release/bundle/`.

| Command | Description |
|---------|-------------|
| `npm run build:bare` | Production build without Whisper, Tor or video |
| `npm run dev:bare` | Development mode without Whisper, Tor or video |
| `npm run build` | Full production build |
| `npm run dev` | Full development mode |

---

## Troubleshooting

### `spirv/unified1/spirv.hpp` not found, or a Vulkan shader step fails

The full build is missing part of the Vulkan toolchain:

```bash
sudo pacman -S --needed vulkan-headers vulkan-icd-loader spirv-headers shaderc
```

### WebKit not found

```
error: could not find system library 'webkit2gtk-4.1'
```

```bash
sudo pacman -S --needed webkit2gtk-4.1
```
