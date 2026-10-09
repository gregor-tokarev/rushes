#!/usr/bin/env bash
# Generate short talking-head style test footage with real synthetic speech, for
# exercising AI edit pipelines end to end.
#
#   scripts/make-test-footage.sh <output-dir>
set -euo pipefail

out="${1:?usage: $0 <output-dir>}"
mkdir -p "$out"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

take() {
  local name="$1" color="$2" text="$3"
  espeak-ng -s 150 -w "$tmp/$name.wav" "$text"
  ffmpeg -loglevel error -y \
    -f lavfi -i "color=c=${color}:s=1280x720:r=30" \
    -i "$tmp/$name.wav" \
    -vf "drawtext=text='${name}':fontcolor=white:fontsize=48:x=(w-text_w)/2:y=(h-text_h)/2" \
    -shortest -c:v libx264 -pix_fmt yuv420p -c:a aac "$out/$name.mp4"
}

take "take1" "0x1f2937" "Hi everyone, um, welcome back to the channel. Today, uh, today we are going to set up a home studio on a budget."
take "take2" "0x312e81" "So, the first thing you need is a decent microphone. Um, actually, let me start again. The first thing you need is good light."
take "take3" "0x064e3b" "That's it for today. If this helped, subscribe, and I will see you in the next video."
ls -la "$out"
