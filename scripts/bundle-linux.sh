#!/usr/bin/env bash
# Bundle VideoCRM for Linux: a tarball for ~/.local (or /usr/local) and an AppImage.
#
#   scripts/bundle-linux.sh <version> <binary>
#
# Needs ImageMagick (imagemagick) and, for the AppImage, appimagetool: $APPIMAGETOOL,
# or `appimagetool` on PATH. Writes to $OUT_DIR (default target/bundle):
#   videocrm-<version>-linux-<arch>.tar.gz  bin/, share/applications/, share/icons/
#   VideoCRM-<version>-<arch>.AppImage
# The binary is used as built: it loads Wayland, X11 and Vulkan from the system at run
# time, so build it on the oldest distribution you support (glibc is the floor).
set -euo pipefail

[[ $# -eq 2 ]] || { echo "usage: $0 <version> <binary>" >&2; exit 2; }
version=$1
binary=$2
root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT_DIR:-$root/target/bundle}
arch=$(uname -m)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"
# ImageMagick 7 is `magick`; 6 (Ubuntu 22.04) only has `convert`.
imagemagick=$(command -v magick || command -v convert) || { echo "ImageMagick is missing" >&2; exit 1; }

# The install tree, shared by the tarball and the AppImage. app_id "videocrm" is the
# window's Wayland app id / X11 class, which StartupWMClass matches.
install_tree() {
  local prefix=$1
  install -Dm755 "$binary" "$prefix/bin/videocrm"
  mkdir -p "$prefix/share/applications"
  cat > "$prefix/share/applications/videocrm.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=VideoCRM
GenericName=Video production planner
Comment=Plan videos, write scripts, edit with AI and upload to YouTube
Exec=videocrm
Icon=videocrm
Terminal=false
Categories=AudioVideo;Video;Office;
Keywords=YouTube;video;script;CRM;
StartupWMClass=videocrm
EOF
  for size in 16 24 32 48 64 128 256 512; do
    mkdir -p "$prefix/share/icons/hicolor/${size}x${size}/apps"
    "$imagemagick" "$root/assets/icon.png" -filter Lanczos -resize ${size}x${size} \
      "PNG32:$prefix/share/icons/hicolor/${size}x${size}/apps/videocrm.png"
  done
}

# Tarball: one top-level folder, so `tar -xzf … -C ~/.local --strip-components=1`
# installs it.
name=videocrm-$version-linux-$arch
install_tree "$work/$name"
if command -v desktop-file-validate >/dev/null; then
  desktop-file-validate "$work/$name/share/applications/videocrm.desktop"
fi
tarball=$out/$name.tar.gz
tar -C "$work" -czf "$tarball" "$name"

# AppImage: the same tree under usr/, plus the entry, icon and AppRun at the top.
appdir=$work/VideoCRM.AppDir
install_tree "$appdir/usr"
cp "$appdir/usr/share/applications/videocrm.desktop" "$appdir/videocrm.desktop"
cp "$appdir/usr/share/icons/hicolor/256x256/apps/videocrm.png" "$appdir/videocrm.png"
ln -s videocrm.png "$appdir/.DirIcon"
ln -s usr/bin/videocrm "$appdir/AppRun"
appimagetool=${APPIMAGETOOL:-appimagetool}
appimage=$out/VideoCRM-$version-$arch.AppImage
ARCH=$arch "$appimagetool" --no-appstream "$appdir" "$appimage"

echo "$tarball"
echo "$appimage"
