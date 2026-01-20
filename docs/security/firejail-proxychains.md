# Running Vector with Firejail & Proxychains

This guide explains how to run Vector in a sandboxed environment using [Firejail](https://github.com/netblue30/firejail) and route traffic through proxies using [Proxychains](https://github.com/haad/proxychains).

## Overview

- **Firejail**: A SUID sandbox program that restricts the running environment using Linux namespaces and seccomp-bpf
- **Proxychains**: Forces TCP connections through SOCKS4/5 or HTTP proxies (e.g., Tor)

## Prerequisites

### Install Dependencies

**Debian/Ubuntu:**
```bash
sudo apt-get install firejail proxychains4
```

**Arch Linux:**
```bash
sudo pacman -S firejail proxychains-ng
```

**Fedora:**
```bash
sudo dnf install firejail proxychains-ng
```

## Important: Use the Bare Build

Vector's default build includes **whisper-rs** (AI transcription) which uses SIMD/AVX CPU instructions that may cause "Illegal instruction" crashes under certain sandbox configurations.

**Always use the bare build for sandboxed environments:**

```bash
# Build without whisper (AI features)
npm run build:bare
```

The resulting binary will be at:
```
src-tauri/target/release/vector
```

## Vector's Required Directories

Vector needs access to these directories:

| Directory | Purpose |
|-----------|---------|
| `~/.local/share/io.vectorapp/` | Main app data (databases, cache, miniapps) |
| `~/.local/share/vector/` | WebKit HSTS storage |
| `~/Downloads/vector/` | Downloaded media files |

## Firejail Configuration

### Step 1: Create the Profile

Create the Vector firejail profile:

```bash
mkdir -p ~/.config/firejail
```

Copy the profile from [`vector.profile`](vector.profile) in this directory, or use the content below:

```ini
# Firejail profile for Vector Privacy Messenger
# https://github.com/VectorPrivacy/Vector

# Persistent local customizations (create vector.local for overrides)
include vector.local

# Persistent global definitions
include globals.local

# Vector data directories
noblacklist ${HOME}/.local/share/io.vectorapp
noblacklist ${HOME}/.local/share/vector
noblacklist ${HOME}/Downloads/vector

# Restrictions
# Note: disable-devel.inc and disable-exec.inc are NOT included
# as they may block WebKitGTK components
include disable-common.inc
include disable-programs.inc

# Security hardening
caps.drop all
netfilter
nodvd
nonewprivs
noroot
notv
nou2f
novideo
protocol unix,inet,inet6
seccomp

# D-Bus (notifications)
dbus-user filter
dbus-user.talk org.freedesktop.Notifications
dbus-user.talk org.freedesktop.portal.*
dbus-system none

# Audio (voice messages and notifications)
ignore nosound
```

> **Note:** This profile was tested on Ubuntu 25.10 with firejail 0.9.72 and proxychains-ng 4.17. More restrictive profiles (with `disable-devel.inc`, `disable-exec.inc`, `private-dev`, `private-etc`) may prevent Vector from starting due to WebKitGTK requirements.

### Step 2: Create Local Overrides (Optional)

For custom tweaks, create `~/.config/firejail/vector.local`:

```ini
# Local overrides for Vector firejail profile

# Uncomment to allow microphone (for voice messages)
# ignore noinput

# Uncomment to allow webcam (if ever needed)
# ignore novideo

# Uncomment for additional debugging
# ignore seccomp
```

### Step 3: Test the Profile

```bash
# Test with debug output
firejail --debug --profile=~/.config/firejail/vector.profile /path/to/vector

# Quick test
firejail --profile=~/.config/firejail/vector.profile /path/to/vector
```

## Proxychains Configuration

### Step 1: Configure Proxychains

Edit `/etc/proxychains4.conf` (or `~/.proxychains/proxychains.conf`):

```ini
# Proxychains configuration for Vector

# Dynamic chain - skip dead proxies
dynamic_chain

# Quiet mode - less output
quiet_mode

# Proxy DNS requests through the proxy
proxy_dns

# Timeouts
tcp_read_time_out 15000
tcp_connect_time_out 8000

# Proxy list
[ProxyList]
# Tor (default)
socks5 127.0.0.1 9050

# Or use a custom SOCKS5 proxy:
# socks5 127.0.0.1 1080
```

### Step 2: Start Tor (if using Tor)

```bash
# Install Tor
sudo apt-get install tor

# Start Tor service
sudo systemctl start tor

# Verify Tor is running
curl --socks5 127.0.0.1:9050 https://check.torproject.org/api/ip
```

## Running Vector with Both

### Method 1: Firejail Only

```bash
firejail --profile=~/.config/firejail/vector.profile /path/to/vector
```

### Method 2: Proxychains Only

```bash
proxychains4 /path/to/vector
```

### Method 3: Firejail + Proxychains (Maximum Isolation)

```bash
firejail --profile=~/.config/firejail/vector.profile proxychains4 /path/to/vector
```

### Creating a Desktop Launcher

Create `~/.local/share/applications/vector-sandboxed.desktop`:

```ini
[Desktop Entry]
Name=Vector (Sandboxed)
Comment=Private Messenger - Sandboxed with Firejail
Exec=firejail --profile=%h/.config/firejail/vector.profile /path/to/vector
Icon=vector
Type=Application
Categories=Network;InstantMessaging;
Terminal=false
```

## Troubleshooting

### "Illegal instruction" Crash

**Cause:** CPU instruction incompatibility, usually from whisper-rs or crypto libraries.

**Solution:**
1. Use the bare build: `npm run build:bare`
2. If still failing, try without seccomp:
   ```bash
   firejail --profile=~/.config/firejail/vector.profile --ignore=seccomp /path/to/vector
   ```

### Network Connection Issues

**Cause:** Firejail blocking network or DNS.

**Solution:** Verify network protocol settings:
```ini
# In vector.profile, ensure:
protocol unix,inet,inet6
```

### D-Bus / Notifications Not Working

**Cause:** D-Bus filtering too strict.

**Solution:** Add to `vector.local`:
```ini
ignore dbus-user filter
dbus-user none
```

Or allow specific services:
```ini
dbus-user.talk org.freedesktop.Notifications
```

### Audio Not Working

**Cause:** PulseAudio/PipeWire access blocked.

**Solution:** Add to `vector.local`:
```ini
ignore nosound
```

### Cannot Download Files

**Cause:** Download directory not whitelisted.

**Solution:** Ensure this is in profile:
```ini
mkdir ${HOME}/Downloads/vector
whitelist ${HOME}/Downloads/vector
```

### WebKit Cache Issues

**Cause:** Private cache too restrictive.

**Solution:** Remove or comment out:
```ini
# private-cache
```

## Debugging

### Check What's Blocked

```bash
# See blacklisted directories
firejail --debug-blacklists --profile=~/.config/firejail/vector.profile /path/to/vector

# See whitelisted directories
firejail --debug-whitelists --profile=~/.config/firejail/vector.profile /path/to/vector

# Full debug output
firejail --debug --profile=~/.config/firejail/vector.profile /path/to/vector 2>&1 | tee firejail-debug.log
```

### Monitor Running Sandbox

```bash
# List sandboxed processes
firejail --list

# Monitor specific sandbox
firemon --seccomp PID
```

### Test Proxychains Connection

```bash
# Test proxy is working
proxychains4 curl https://check.torproject.org/api/ip

# Test with verbose output
proxychains4 -f /etc/proxychains4.conf curl -v https://example.com
```

## Security Considerations

1. **Bare build recommended**: The whisper feature uses Vulkan/SIMD which may conflict with seccomp filters

2. **Proxy DNS**: Always enable `proxy_dns` in proxychains to prevent DNS leaks

3. **Tor limitations**: Nostr relays using WebSocket (WSS) work over Tor, but performance may be slower

4. **Profile auditing**: Firejail profiles use blacklists by default - audit your profile to ensure nothing sensitive is exposed

## References

- [Firejail Documentation](https://firejail.wordpress.com/documentation-2/)
- [Firejail GitHub](https://github.com/netblue30/firejail)
- [Proxychains-NG GitHub](https://github.com/rofl0r/proxychains-ng)
- [Arch Wiki - Firejail](https://wiki.archlinux.org/title/Firejail)

---

> **Note:** Our pre-built profile is a best-effort approach to isolate Vector to the furthest extent, while still maintaining 100% feature support. There may be areas that could be hardened further, or potentially unforeseen bugs. In either case, please feel free to [contribute suggestions](https://github.com/VectorPrivacy/Vector/issues)!

*Last tested: January 2026 on Ubuntu 25.10 with Vector bare build*