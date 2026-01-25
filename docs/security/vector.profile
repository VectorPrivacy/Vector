# Firejail profile for Vector Privacy Messenger
# https://github.com/VectorPrivacy/Vector
#
# Usage:
#   cd /path/to/vector && firejail --whitelist=`pwd` --read-only=`pwd` --profile=~/.config/firejail/vector.profile ./vector
#
# With proxychains (Tor):
#   cd /path/to/vector && firejail --whitelist=`pwd` --read-only=`pwd` --profile=~/.config/firejail/vector.profile proxychains4 ./vector
#
# NOTE: This is a maximum-security profile. OS notifications are disabled
# due to complex GNOME/GTK D-Bus requirements. In-app notifications still work.

# =====================
# WHITELIST APPROACH
# =====================
# Whitelisting directories automatically blacklists everything else in HOME.

# XDG user-dirs config (required for Tauri to resolve Downloads path correctly)
noblacklist ${HOME}/.config/user-dirs.dirs
whitelist ${HOME}/.config/user-dirs.dirs
read-only ${HOME}/.config/user-dirs.dirs

# Proxychains config (read-only - app should not modify proxy settings)
mkdir ${HOME}/.proxychains
noblacklist ${HOME}/.proxychains
whitelist ${HOME}/.proxychains
read-only ${HOME}/.proxychains

# Vector app data
mkdir ${HOME}/.local/share/io.vectorapp
noblacklist ${HOME}/.local/share/io.vectorapp
whitelist ${HOME}/.local/share/io.vectorapp

# WebKit HSTS storage
mkdir ${HOME}/.local/share/vector
noblacklist ${HOME}/.local/share/vector
whitelist ${HOME}/.local/share/vector

# Downloaded media files
mkdir ${HOME}/Downloads/vector
noblacklist ${HOME}/Downloads/vector
whitelist ${HOME}/Downloads/vector

# =====================
# Standard Firejail includes
# =====================
include disable-common.inc
include disable-programs.inc

# System sensitive files
blacklist /etc/shadow
blacklist /etc/gshadow

# =====================
# Security Hardening
# =====================
caps.drop all
netfilter
nodvd
nogroups
nonewprivs
noroot
notv
nou2f
novideo
protocol unix,inet,inet6
seccomp
seccomp.block-secondary

# Private directories (isolation)
private-tmp
# private-dev - DO NOT ENABLE: breaks /dev/urandom needed for crypto

# Prevent code execution from writable directories
noexec ${HOME}/Downloads/vector
noexec ${HOME}/.local/share/io.vectorapp
noexec ${HOME}/.local/share/vector

# Read-only system paths (defense in depth)
read-only /usr
read-only /opt
read-only /boot
read-only /var

# Resource limits (prevent fork bombs, file descriptor exhaustion)
rlimit-nproc 200
rlimit-nofile 2048
rlimit-fsize 1g

# Restrict available executables (minimal set)
# Note: WebKit processes (WebKitWebProcess, WebKitNetworkProcess, WebKitGPUProcess)
# live in /usr/lib/*/webkit*/ and are launched by the library, not via PATH.
# private-bin only affects /usr/bin, /bin, etc. so WebKit is unaffected.
private-bin Vector

# =====================
# D-Bus (minimal access)
# =====================
# NOTE: OS notifications do not work with dbus-user filter due to
# complex GNOME/GTK D-Bus requirements. Trade-off: maximum security
# over notifications. In-app notifications still work.
dbus-user filter
dbus-user.talk org.freedesktop.portal.*
dbus-user.talk org.freedesktop.FileManager1
dbus-user.talk org.gnome.Nautilus
dbus-user.talk org.kde.dolphin
dbus-system none

# =====================
# Audio (voice messages)
# =====================
ignore nosound

# Extra hardening
machine-id
hostname vector-sandbox

# Disable access to mount points (USB drives, external media)
disable-mnt

# Restrict /etc access (minimal set for WebKitGTK + networking)
private-etc alternatives,ca-certificates,crypto-policies,fonts,hosts,ld.so.cache,ld.so.conf,ld.so.conf.d,localtime,mime.types,nsswitch.conf,pki,resolv.conf,ssl,X11

# Clear potentially sensitive environment variables
rmenv SSH_AUTH_SOCK
rmenv GPG_AGENT_INFO
rmenv GNOME_KEYRING_CONTROL
rmenv SSH_AGENT_PID