# Running Vector with Firejail & Proxychains

This guide explains how to run Vector in a **maximum-security sandbox** using [Firejail](https://github.com/netblue30/firejail) and optionally route traffic through proxies using [Proxychains](https://github.com/haad/proxychains).

## Overview

- **Firejail**: A SUID sandbox program that restricts the running environment using Linux namespaces and seccomp-bpf
- **Proxychains**: Forces TCP connections through SOCKS4/5 or HTTP proxies (e.g., Tor)

## Security Model

Our profile uses a **whitelist approach** with maximum hardening:

| Protection | Description |
|------------|-------------|
| **Whitelist filesystem** | Only 5 HOME paths accessible, everything else blocked |
| **Single binary** | `private-bin Vector` - no shells, no utilities |
| **noexec** | All writable directories marked non-executable |
| **Read-only system** | `/usr`, `/opt`, `/boot`, `/var` all read-only |
| **Private /etc** | Only 15 essential entries (SSL, fonts, DNS) |
| **Private /tmp** | Isolated temporary directory |
| **No mount access** | USB drives and external media blocked |
| **All caps dropped** | No Linux capabilities |
| **No privileges** | noroot, nonewprivs, nogroups |
| **Seccomp** | Syscall filtering enabled |
| **Resource limits** | Max 200 procs, 2048 fds, 1GB file size |
| **Identity spoofing** | Fake machine-id and hostname |
| **Env sanitization** | SSH/GPG agent sockets stripped |
| **D-Bus filtering** | Minimal portal + file manager access |

### What an attacker CANNOT do (even with code execution):
- Access any files outside whitelisted paths
- Run shell commands (no bash/sh available)
- Execute downloaded malware (noexec on all writable dirs)
- Escalate privileges
- Access SSH keys, GPG keys, browser data, crypto wallets
- Access USB drives or external media
- Read system passwords or sensitive /etc files
- Fork bomb or exhaust resources
- Identify the real machine

### Trade-off
OS notifications are disabled due to complex GNOME/GTK D-Bus requirements. In-app notifications still work.

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
npm run build:bare
```

The resulting binary will be at:
```
src-tauri/target/release/Vector
```

## Setup

### Step 1: Install the Profile

```bash
mkdir -p ~/.config/firejail
cp docs/security/vector.profile ~/.config/firejail/vector.profile
```

### Step 2: Create Proxychains Config (Optional)

If using proxychains for Tor routing:

```bash
mkdir -p ~/.proxychains
cat > ~/.proxychains/proxychains.conf << 'EOF'
dynamic_chain
quiet_mode
proxy_dns

tcp_read_time_out 15000
tcp_connect_time_out 8000

[ProxyList]
socks5 127.0.0.1 9050
EOF
```

## Running Vector

### Firejail Only (Maximum Security)

```bash
cd /path/to/vector
firejail --whitelist=`pwd` --read-only=`pwd` \
  --profile=~/.config/firejail/vector.profile ./Vector
```

### Firejail + Proxychains (Maximum Security + Tor)

```bash
# Start Tor first
sudo systemctl start tor

# Run Vector through Tor
cd /path/to/vector
firejail --whitelist=`pwd` --read-only=`pwd` \
  --profile=~/.config/firejail/vector.profile proxychains4 ./Vector
```

### Shell Alias (Recommended)

Add to `~/.bashrc` or `~/.zshrc`:

```bash
alias vector-secure='cd /path/to/vector && firejail --whitelist=`pwd` --read-only=`pwd` --profile=~/.config/firejail/vector.profile ./Vector'
alias vector-tor='cd /path/to/vector && firejail --whitelist=`pwd` --read-only=`pwd` --profile=~/.config/firejail/vector.profile proxychains4 ./Vector'
```

### Desktop Launcher

Create `~/.local/share/applications/vector-secure.desktop`:

```ini
[Desktop Entry]
Name=Vector (Secure)
Comment=Vector Privacy Messenger - Maximum Security Sandbox
Exec=sh -c 'cd /path/to/vector && firejail --whitelist=`pwd` --read-only=`pwd` --profile=%h/.config/firejail/vector.profile ./Vector'
Icon=vector
Type=Application
Categories=Network;InstantMessaging;
Terminal=false
```

## Whitelisted Paths

Vector can **only** access these directories:

| Path | Access | Purpose |
|------|--------|---------|
| `~/.config/user-dirs.dirs` | Read-only | XDG config for path resolution |
| `~/.proxychains/` | Read-only | Proxy configuration |
| `~/.local/share/io.vectorapp/` | Read/Write | App data, databases, cache |
| `~/.local/share/vector/` | Read/Write | WebKit HSTS storage |
| `~/Downloads/vector/` | Read/Write | Downloaded attachments |
| Binary directory (via CLI) | Read-only | The Vector executable |

Everything else in HOME is **automatically blocked**.

## Troubleshooting

### "Illegal instruction" Crash

**Cause:** CPU instruction incompatibility from whisper-rs.

**Solution:** Use the bare build: `npm run build:bare`

### Stuck at "Decrypting your keys"

**Cause:** `private-dev` blocks /dev/urandom.

**Solution:** Do NOT enable `private-dev` in the profile.

### Downloads Not Completing

**Cause:** Missing XDG user-dirs config in whitelist.

**Solution:** Ensure profile has:
```ini
whitelist ${HOME}/.config/user-dirs.dirs
read-only ${HOME}/.config/user-dirs.dirs
```

### "Reveal in Explorer" Not Working

**Cause:** File manager D-Bus permissions missing.

**Solution:** Ensure profile has:
```ini
dbus-user.talk org.freedesktop.FileManager1
dbus-user.talk org.gnome.Nautilus
```

### Audio Not Working

**Cause:** Sound blocked by default.

**Solution:** Ensure profile has:
```ini
ignore nosound
```

## Debugging

### Check Sandbox Status

```bash
firejail --list
```

### See Whitelisted Directories

```bash
firejail --debug-whitelists --profile=~/.config/firejail/vector.profile -- /bin/true
```

### Full Debug Output

```bash
firejail --debug --profile=~/.config/firejail/vector.profile ./Vector 2>&1 | tee debug.log
```

### Test Proxychains

```bash
proxychains4 curl https://check.torproject.org/api/ip
```

## Security Considerations

1. **Whitelist approach**: Only explicitly listed directories are accessible - everything else is blocked automatically

2. **No shell access**: `private-bin Vector` means no bash, sh, or any utilities - even with code execution, an attacker has no tools

3. **noexec everywhere**: Downloaded files and cached data cannot be executed

4. **Read-only binary**: The `--read-only=\`pwd\`` flag prevents the app from modifying itself

5. **Private /etc**: Only essential system config files are visible

6. **Spoofed identity**: machine-id and hostname are randomized per session

7. **Environment sanitized**: SSH/GPG agent sockets are stripped

8. **Proxy DNS**: Always enable `proxy_dns` in proxychains to prevent DNS leaks

## References

- [Firejail Documentation](https://firejail.wordpress.com/documentation-2/)
- [Firejail GitHub](https://github.com/netblue30/firejail)
- [Proxychains-NG GitHub](https://github.com/rofl0r/proxychains-ng)
- [Arch Wiki - Firejail](https://wiki.archlinux.org/title/Firejail)

---

> **Security Note:** This profile provides near-maximum isolation while maintaining full Vector functionality (except OS notifications). The only remaining attack surface is the network connection (required for messaging) and the 5 whitelisted directories.

*Last tested: January 2026 on Ubuntu 25.10 with Vector bare build*