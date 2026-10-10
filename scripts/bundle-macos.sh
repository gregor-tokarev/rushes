#!/usr/bin/env bash
# Bundle VideoCRM for macOS: VideoCRM.app inside a drag-to-Applications .dmg.
#
#   scripts/bundle-macos.sh <version> <binary> [<binary>...]
#
# Several binaries (one per architecture, from `cargo build --target ...`) are
# joined into one universal binary. The app is signed ad hoc (no Developer ID),
# so Gatekeeper asks before the first start of a downloaded copy.
#
# Needs macOS (lipo, iconutil, codesign, hdiutil) and rsvg-convert
# (`brew install librsvg`). Writes to $OUT_DIR (default target/bundle):
#   VideoCRM.app, VideoCRM-<version>-macos-universal.dmg (or -<arch> for one binary)
set -euo pipefail

[[ $# -ge 2 ]] || { echo "usage: $0 <version> <binary> [<binary>...]" >&2; exit 2; }
version=$1
shift
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT_DIR:-$root/target/bundle}
identifier=work.tokarev.videocrm
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

app=$out/VideoCRM.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

# Executable: one binary as is, several joined into a universal one.
if [[ $# -gt 1 ]]; then
  lipo -create -output "$app/Contents/MacOS/videocrm" "$@"
  arch=universal
else
  cp "$1" "$app/Contents/MacOS/videocrm"
  arch=$(lipo -archs "$1" | tr ' ' '-')
fi
chmod 755 "$app/Contents/MacOS/videocrm"

# Icon: macOS icons put the artwork on an 824 px square centred in 1024 px.
artwork=$(sed -e 's#<svg[^>]*>##' -e 's#</svg>##' "$root/assets/icon.svg")
cat > "$work/icon.svg" <<EOF
<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <svg x="100" y="100" width="824" height="824" viewBox="0 0 64 64">$artwork</svg>
</svg>
EOF
iconset=$work/VideoCRM.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  rsvg-convert -w $size -h $size "$work/icon.svg" -o "$iconset/icon_${size}x${size}.png"
  rsvg-convert -w $((size * 2)) -h $((size * 2)) "$work/icon.svg" -o "$iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/VideoCRM.icns"

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
  <string>VideoCRM</string>
  <key>CFBundleExecutable</key>
  <string>videocrm</string>
  <key>CFBundleIconFile</key>
  <string>VideoCRM</string>
  <key>CFBundleIdentifier</key>
  <string>$identifier</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>VideoCRM</string>
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

# Disk image: the app next to a link to /Applications.
stage=$work/dmg
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
dmg=$out/VideoCRM-$version-macos-$arch.dmg
rm -f "$dmg"
# hdiutil fails now and then with "Resource busy" on CI machines; try again.
for attempt in 1 2 3 4 5; do
  if hdiutil create -volname VideoCRM -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$dmg" >/dev/null; then
    break
  fi
  [[ $attempt -lt 5 ]] || { echo "hdiutil create failed" >&2; exit 1; }
  sleep $((attempt * 5))
done
hdiutil verify "$dmg" >/dev/null

echo "$app"
echo "$dmg"
