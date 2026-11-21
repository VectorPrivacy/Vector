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
echo "::group::test log file"
adb shell 'ls -la /sdcard/Download/Vector/ /sdcard/Android/media/io.vectorapp.gpui/ 2>/dev/null; for f in /sdcard/Download/Vector/vector-gpui-debug-*.log; do echo "== $f"; head -40 "$f"; done 2>/dev/null' || true
echo "::endgroup::"
# A tap where the login screen's first button sits, to see touches arrive.
size=$(adb shell wm size | sed 's/.*: //' | tr -d '\r')
adb shell input tap $((${size%x*} / 2)) $((${size#*x} * 36 / 100))
sleep 8
adb exec-out screencap -p > "$out/after-tap.png"
# Back to the start, then Login and some typing, to see the keyboard's text reach the field.
adb shell input keyevent 4
sleep 3
adb shell input tap $((${size%x*} / 2)) $((${size#*x} * 47 / 100))
sleep 5
adb shell input text "nsec1typed"
sleep 4
adb exec-out screencap -p > "$out/typed.png"
for shot in screen after-tap typed; do
  echo "::group::$shot.png"
  echo "SCREEN-BEGIN $shot"
  base64 -w0 "$out/$shot.png"
  echo
  echo "SCREEN-END"
  echo "::endgroup::"
done
# A restart with the background on: the service comes up with no window and the process stays.
echo "::group::service after a restart"
adb shell am force-stop "$app"
adb shell run-as "$app" sh -c "'mkdir -p shared_prefs && echo \"<map><boolean name=\\\"background_service_enabled\\\" value=\\\"true\\\" /></map>\" > shared_prefs/vector_prefs.xml'" || true
adb logcat -c
if adb root >/dev/null 2>&1; then
  adb wait-for-device
  adb shell am broadcast -a android.intent.action.BOOT_COMPLETED -p "$app" || true
  sleep 15
  echo "pid after boot: $(adb shell pidof "$app" || echo none)"
  adb shell dumpsys activity services "$app" | grep -E "ServiceRecord|isForeground" || echo "no service"
  adb logcat -d -b crash || true
  adb logcat -d | grep -E "standby|UnsatisfiedLink|FATAL" || true
  adb unroot >/dev/null 2>&1 || true
else
  echo "no root on this image: skipped"
fi
echo "::endgroup::"
[ -n "$pid" ]
