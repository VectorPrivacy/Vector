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
# Granted up front, so the permission prompt doesn't cover the screen being checked.
adb shell pm grant "$app" android.permission.POST_NOTIFICATIONS || true
adb logcat -c
adb shell am start -W -n "$app/io.vectorapp.MainActivity"
sleep 45
adb exec-out screencap -p > "$out/screen.png"
pid=$(adb shell pidof "$app" || true)
echo "pid: ${pid:-not running}"
adb shell dumpsys meminfo "$app" > "$out/meminfo.txt" || true
grep -E "TOTAL PSS|TOTAL RSS|Native Heap|Graphics" "$out/meminfo.txt" || true
echo "notification channels:"
adb shell dumpsys notification | grep -oE "vector_(service|messages_v2)" | sort -u || true
adb logcat -d > "$out/logcat.txt"
echo "::group::app log"
if [ -n "$pid" ]; then adb logcat -d --pid="$pid"; else tail -400 "$out/logcat.txt"; fi
adb logcat -d -b crash || true
echo "::endgroup::"
# A tap where the login screen's first button sits, to see touches arrive.
size=$(adb shell wm size | sed 's/.*: //' | tr -d '\r')
adb shell input tap $((${size%x*} / 2)) $((${size#*x} * 36 / 100))
sleep 8
adb exec-out screencap -p > "$out/after-tap.png"
for shot in screen after-tap; do
  echo "::group::$shot.png"
  echo "SCREEN-BEGIN $shot"
  base64 -w0 "$out/$shot.png"
  echo
  echo "SCREEN-END"
  echo "::endgroup::"
done
[ -n "$pid" ]
