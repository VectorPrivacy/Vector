#!/bin/sh
# Installs a debug APK on the running emulator, opens it, and keeps what it showed and logged,
# in the job's log as well as the artifact.
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
pid=$(adb shell pidof "$app" || true)
echo "pid: ${pid:-not running}"
adb shell dumpsys meminfo "$app" > "$out/meminfo.txt" || true
grep -E "TOTAL PSS|TOTAL RSS|Native Heap|Graphics" "$out/meminfo.txt" || true
adb logcat -d > "$out/logcat.txt"
echo "::group::app log"
if [ -n "$pid" ]; then adb logcat -d --pid="$pid"; else tail -400 "$out/logcat.txt"; fi
adb logcat -d -b crash || true
echo "::endgroup::"
echo "::group::screen.png"
echo "SCREEN-BEGIN"
base64 -w0 "$out/screen.png"
echo
echo "SCREEN-END"
echo "::endgroup::"
[ -n "$pid" ]
