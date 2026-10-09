#!/usr/bin/env bash
# Run a VideoCRM binary/example under a virtual display and capture screenshots.
#
#   scripts/screenshot.sh [-d :99] [-w 8] [-o /tmp/shot.png] -- <command...>
#
# Starts Xvfb on the display if needed, launches the command, waits, captures the
# whole screen, and leaves the app running when KEEP=1 (so xdotool can drive it).
set -euo pipefail

display=":99"
wait_secs=8
out="/tmp/videocrm-shot.png"
while [[ $# -gt 0 ]]; do
  case "$1" in
    -d) display="$2"; shift 2 ;;
    -w) wait_secs="$2"; shift 2 ;;
    -o) out="$2"; shift 2 ;;
    --) shift; break ;;
    *) break ;;
  esac
done
[[ $# -gt 0 ]] || { echo "usage: $0 [-d :N] [-w secs] [-o file.png] -- command..." >&2; exit 2; }

if ! xdpyinfo -display "$display" >/dev/null 2>&1; then
  Xvfb "$display" -screen 0 1440x900x24 >/dev/null 2>&1 &
  for _ in $(seq 1 50); do xdpyinfo -display "$display" >/dev/null 2>&1 && break; sleep 0.1; done
fi

export DISPLAY="$display"
"$@" >"/tmp/videocrm-app-${display#:}.log" 2>&1 &
app_pid=$!
sleep "$wait_secs"
import -window root "$out"
echo "screenshot: $out (app pid $app_pid, log /tmp/videocrm-app-${display#:}.log)"
if [[ "${KEEP:-0}" != "1" ]]; then
  kill "$app_pid" 2>/dev/null || true
fi
