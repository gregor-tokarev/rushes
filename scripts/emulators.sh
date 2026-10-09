#!/usr/bin/env bash
# Run the local Google OAuth + YouTube Data API emulators in the foreground.
#
#   scripts/emulators.sh
#
# Ports: GOOGLE_EMULATOR_PORT (default 4002), YOUTUBE_EMULATOR_PORT (default 4020).
# Point VideoCRM at them with VIDEOCRM_GOOGLE_URL / VIDEOCRM_YOUTUBE_URL or the
# "Local emulator" backend in settings. Ctrl-C stops both.
set -euo pipefail

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../emulators" && pwd)"
command -v node >/dev/null || { echo "node (>= 24) is required" >&2; exit 1; }
[[ -d "$dir/node_modules" ]] || npm ci --prefix "$dir" --no-audit --no-fund

google_port="${GOOGLE_EMULATOR_PORT:-4002}"
youtube_port="${YOUTUBE_EMULATOR_PORT:-4020}"
export GOOGLE_EMULATOR_PORT="$google_port" YOUTUBE_EMULATOR_PORT="$youtube_port"

cat <<INFO
Google (OAuth, userinfo):  http://127.0.0.1:$google_port
YouTube Data API v3:       http://127.0.0.1:$youtube_port
Uploads (inspection):      http://127.0.0.1:$youtube_port/_emulator/uploads

  VIDEOCRM_GOOGLE_URL=http://127.0.0.1:$google_port VIDEOCRM_YOUTUBE_URL=http://127.0.0.1:$youtube_port
INFO

cd "$dir"
exec node node_modules/emulate/dist/index.js start --config emulate.config.ts
