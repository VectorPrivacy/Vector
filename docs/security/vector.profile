# Firejail profile for Vector Privacy Messenger
# https://github.com/VectorPrivacy/Vector
#
# Usage:
#   1. Copy to ~/.config/firejail/vector.profile
#   2. Run: firejail --profile=vector.profile /path/to/vector
#
# With proxychains (Tor):
#   firejail --profile=vector.profile proxychains4 /path/to/vector

# =====================
# Vector Data Directories
# =====================
noblacklist ${HOME}/.local/share/io.vectorapp
noblacklist ${HOME}/.local/share/vector
noblacklist ${HOME}/Downloads/vector

# =====================
# BLACKLIST SENSITIVE LOCATIONS
# =====================
include disable-common.inc
include disable-programs.inc

# Extra blacklists for sensitive data
blacklist ${HOME}/.ssh
blacklist ${HOME}/.gnupg
blacklist ${HOME}/.pki
blacklist ${HOME}/.password-store
blacklist ${HOME}/.mozilla
blacklist ${HOME}/.config/chromium
blacklist ${HOME}/.config/google-chrome
blacklist ${HOME}/.config/BraveSoftware
blacklist ${HOME}/.thunderbird
blacklist ${HOME}/.config/Signal
blacklist ${HOME}/.config/Slack
blacklist ${HOME}/.config/discord
blacklist ${HOME}/.aws
blacklist ${HOME}/.kube
blacklist ${HOME}/.docker
blacklist ${HOME}/.vault-token
blacklist ${HOME}/.netrc
blacklist ${HOME}/.git-credentials
blacklist ${HOME}/.config/gh
blacklist ${HOME}/.cargo/credentials*
blacklist ${HOME}/.npmrc
blacklist ${HOME}/.pypirc
blacklist /etc/shadow
blacklist /etc/gshadow

# =====================
# Security Hardening (aggressive)
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

# =====================
# D-Bus (minimal access)
# =====================
dbus-user filter
dbus-user.talk org.freedesktop.Notifications
dbus-user.talk org.freedesktop.portal.Desktop
dbus-user.talk org.freedesktop.portal.FileChooser
dbus-user.talk org.freedesktop.portal.OpenURI
dbus-user.talk org.freedesktop.FileManager1
dbus-user.talk org.gnome.Nautilus
dbus-user.own org.gnome.Nautilus
dbus-user.own org.freedesktop.FileManager1
dbus-user.talk org.kde.dolphin
dbus-user.talk io.vectorapp.*
dbus-system none

# =====================
# Audio (voice messages)
# =====================
ignore nosound

# Extra hardening
machine-id
hostname vector-sandbox