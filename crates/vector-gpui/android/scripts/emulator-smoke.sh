#!/bin/sh
# Installs a debug APK on the running emulator, opens it, and keeps what it showed and logged.
# Usage: scripts/emulator-smoke.sh <apk> <out-dir>
set -eu
apk="$1"
out="$2"
app=io.vectorapp.gpui
mkdir -p "$out"
adb install -r "$apk"
adb logcat -c
adb shell am start -W -n "$app/io.vectorapp.MainActivity"
sleep 45
adb exec-out screencap -p > "$out/screen.png"
adb shell pidof "$app" > "$out/pid.txt" || echo "not running" > "$out/pid.txt"
adb shell dumpsys meminfo "$app" > "$out/meminfo.txt" || true
adb logcat -d > "$out/logcat.txt"
grep -q "not running" "$out/pid.txt" && exit 1
exit 0
