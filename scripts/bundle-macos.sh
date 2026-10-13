#!/usr/bin/env bash
# Bundle Rushes for macOS: Rushes.app inside a drag-to-Applications .dmg.
#
#   scripts/bundle-macos.sh <version> <binary> [<binary>...]
#
# Several binaries (one per architecture, from `cargo build --target ...`) are
# joined into one universal binary. The app is signed ad hoc (no Developer ID),
# so Gatekeeper asks before the first start of a downloaded copy.
#
# Needs macOS (lipo, iconutil, codesign, hdiutil) and ImageMagick
# (`brew install imagemagick`). Writes to $OUT_DIR (default target/bundle):
#   Rushes.app, Rushes-<version>-macos-universal.dmg (or -<arch> for one binary)
#   VideoCRM-<version>-macos-universal.dmg: the same app as VideoCRM.app, which
#   copies from before the rename update themselves with
set -euo pipefail

[[ $# -ge 2 ]] || { echo "usage: $0 <version> <binary> [<binary>...]" >&2; exit 2; }
version=$1
shift
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT_DIR:-$root/target/bundle}
identifier=work.tokarev.rushes
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

app=$out/Rushes.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

# Executable: one binary as is, several joined into a universal one.
if [[ $# -gt 1 ]]; then
  lipo -create -output "$app/Contents/MacOS/rushes" "$@"
  arch=universal
else
  cp "$1" "$app/Contents/MacOS/rushes"
  arch=$(lipo -archs "$1" | tr ' ' '-')
fi
chmod 755 "$app/Contents/MacOS/rushes"

# Icon: macOS icons put the artwork on an 824 px square centred in 1024 px.
magick "$root/assets/icon.png" -filter Lanczos -resize 824x824 \
  -background none -gravity center -extent 1024x1024 "PNG32:$work/icon.png"
iconset=$work/Rushes.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  magick "$work/icon.png" -filter Lanczos -resize ${size}x${size} "PNG32:$iconset/icon_${size}x${size}.png"
  magick "$work/icon.png" -filter Lanczos -resize $((size * 2))x$((size * 2)) \
    "PNG32:$iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/Rushes.icns"

# CFBundleShortVersionString takes three numbers: drop a pre-release suffix.
short_version=${version%%[-+]*}
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleDisplayName</key>
  <string>Rushes</string>
  <key>CFBundleExecutable</key>
  <string>rushes</string>
  <key>CFBundleIconFile</key>
  <string>Rushes</string>
  <key>CFBundleIdentifier</key>
  <string>$identifier</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>Rushes</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$short_version</string>
  <key>CFBundleVersion</key>
  <string>$version</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.productivity</string>
  <key>LSMinimumSystemVersion</key>
  <string>10.15.7</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key>
  <true/>
</dict>
</plist>
EOF
plutil -lint "$app/Contents/Info.plist" >/dev/null

# Ad-hoc signature: Apple silicon refuses to run unsigned code, and a bundle signed
# as a whole keeps its resources sealed.
codesign --force --sign - --identifier "$identifier" --timestamp=none "$app"
codesign --verify --strict --verbose=1 "$app"

# Disk image <dmg>: the app, as <bundle>, next to a link to /Applications.
disk_image() {
  local bundle=$1 dmg=$2 stage
  stage=$(mktemp -d "$work/dmg.XXXXXX")
  cp -R "$app" "$stage/$bundle"
  ln -s /Applications "$stage/Applications"
  rm -f "$dmg"
  # hdiutil fails now and then with "Resource busy" on CI machines; try again.
  for attempt in 1 2 3 4 5; do
    if hdiutil create -volname Rushes -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$dmg" >/dev/null; then
      break
    fi
    [[ $attempt -lt 5 ]] || { echo "hdiutil create failed" >&2; exit 1; }
    sleep $((attempt * 5))
  done
  hdiutil verify "$dmg" >/dev/null
}
dmg=$out/Rushes-$version-macos-$arch.dmg
disk_image Rushes.app "$dmg"
# VideoCRM's updater copies VideoCRM.app out of VideoCRM's disk image; the next
# update, by Rushes, renames the bundle to Rushes.app.
legacy_dmg=$out/VideoCRM-$version-macos-$arch.dmg
disk_image VideoCRM.app "$legacy_dmg"

echo "$app"
echo "$dmg"
echo "$legacy_dmg"
