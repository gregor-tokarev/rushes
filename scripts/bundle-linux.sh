#!/usr/bin/env bash
# Bundle Rushes for Linux: a tarball for ~/.local (or /usr/local) and an AppImage.
#
#   scripts/bundle-linux.sh <version> <binary>
#
# Needs ImageMagick (imagemagick) and, for the AppImage, appimagetool: $APPIMAGETOOL,
# or `appimagetool` on PATH. Writes to $OUT_DIR (default target/bundle):
#   rushes-<version>-linux-<arch>.tar.gz    bin/, share/applications/, share/icons/
#   Rushes-<version>-<arch>.AppImage
# and the same under VideoCRM's names, which copies from before the rename
# update themselves with:
#   videocrm-<version>-linux-<arch>.tar.gz  bin/videocrm, its desktop entry and icons
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

# The install tree, shared by the tarball and the AppImage, with the executable,
# desktop entry and icon named <name> (default rushes). app_id "rushes" is the
# window's Wayland app id / X11 class, which StartupWMClass matches.
install_tree() {
  local prefix=$1 name=${2:-rushes}
  install -Dm755 "$binary" "$prefix/bin/$name"
  mkdir -p "$prefix/share/applications"
  cat > "$prefix/share/applications/$name.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Rushes
GenericName=Video production planner
Comment=Plan videos, write scripts, edit with AI and upload to YouTube
Exec=$name
Icon=$name
Terminal=false
Categories=AudioVideo;Video;Office;
Keywords=YouTube;video;script;footage;
StartupWMClass=rushes
EOF
  for size in 16 24 32 48 64 128 256 512; do
    mkdir -p "$prefix/share/icons/hicolor/${size}x${size}/apps"
    "$imagemagick" "$root/assets/icon.png" -filter Lanczos -resize ${size}x${size} \
      "PNG32:$prefix/share/icons/hicolor/${size}x${size}/apps/$name.png"
  done
  if command -v desktop-file-validate >/dev/null; then
    desktop-file-validate "$prefix/share/applications/$name.desktop"
  fi
}

# Tarball: one top-level folder, so `tar -xzf … -C ~/.local --strip-components=1`
# installs it.
name=rushes-$version-linux-$arch
install_tree "$work/$name"
tarball=$out/$name.tar.gz
tar -C "$work" -czf "$tarball" "$name"

# VideoCRM's tarball: its updater puts bin/videocrm in place and starts it. The
# next update, by Rushes, replaces VideoCRM's names with Rushes's.
legacy_name=videocrm-$version-linux-$arch
install_tree "$work/$legacy_name" videocrm
legacy_tarball=$out/$legacy_name.tar.gz
tar -C "$work" -czf "$legacy_tarball" "$legacy_name"

# AppImage: the same tree under usr/, plus the entry, icon and AppRun at the top.
appdir=$work/Rushes.AppDir
install_tree "$appdir/usr"
cp "$appdir/usr/share/applications/rushes.desktop" "$appdir/rushes.desktop"
cp "$appdir/usr/share/icons/hicolor/256x256/apps/rushes.png" "$appdir/rushes.png"
ln -s rushes.png "$appdir/.DirIcon"
ln -s usr/bin/rushes "$appdir/AppRun"
appimagetool=${APPIMAGETOOL:-appimagetool}
appimage=$out/Rushes-$version-$arch.AppImage
ARCH=$arch "$appimagetool" --no-appstream "$appdir" "$appimage"
legacy_appimage=$out/VideoCRM-$version-$arch.AppImage
cp "$appimage" "$legacy_appimage"

echo "$tarball"
echo "$appimage"
echo "$legacy_tarball"
echo "$legacy_appimage"
