#!/bin/bash
# Renders vector-gpui under Xvfb and captures its window.
#   reference/shot.sh <out.png> [WxH] [app args...]
set -e
OUT=$1; SIZE=${2:-1200x760}; shift 2 || true
W0=${SIZE%x*}; H0=${SIZE#*x}
cd "$(dirname "$0")/.."
export DISPLAY=${DISPLAY:-:99} XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/tmp/xdg}; mkdir -p -m 700 "$XDG_RUNTIME_DIR"
pgrep -x Xvfb >/dev/null || { Xvfb "$DISPLAY" -screen 0 2400x1800x24 >/tmp/xvfb.log 2>&1 & sleep 1; }
cargo build -q ${PROFILE:+--profile $PROFILE} 2>&1 | grep -E "^error" -A20 && exit 1 || true
BIN=target/${PROFILE:-debug}/vector-gpui; [ "$PROFILE" = dev ] && BIN=target/debug/vector-gpui
TZ=UTC "$BIN" --size "$SIZE" "$@" >/tmp/vector-gpui.log 2>&1 & PID=$!
until WID=$(xdotool search --pid $PID 2>/dev/null | tail -1) && [ -n "$WID" ]; do
    kill -0 $PID 2>/dev/null || { grep -m1 -A2 panicked /tmp/vector-gpui.log; exit 1; }; sleep 0.1
done
sleep 0.5
# Xvfb drops the frame presented before the window maps; a resize forces another.
xdotool windowsize "$WID" $((W0 + 1)) "$H0"; sleep 0.3; xdotool windowsize "$WID" "$W0" "$H0"; sleep 0.8
# ACT: optional xdotool steps to drive the app before the capture, with $WID set.
[ -n "$ACT" ] && { eval "$ACT"; sleep 0.6; }
eval "$(xdotool getwindowgeometry --shell "$WID")"
import -window root -crop "${WIDTH}x${HEIGHT}+${X}+${Y}" +repage "$OUT"
kill $PID
echo "$OUT ${WIDTH}x${HEIGHT}"
