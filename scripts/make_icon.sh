#!/bin/zsh
# Make assets/AppIcon.icns from assets/AppIcon.svg. Usage: scripts/make_icon.sh
# Quick Look renders the SVG. sips makes each size. iconutil makes the icns file.
set -e
cd "${0:A:h}/.."
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
qlmanage -t -s 1024 -o "$WORK" assets/AppIcon.svg >/dev/null
SET="$WORK/AppIcon.iconset"
mkdir "$SET"
for size in 16 32 128 256 512; do
  sips -z $size $size "$WORK/AppIcon.svg.png" --out "$SET/icon_${size}x${size}.png" >/dev/null
  sips -z $((size * 2)) $((size * 2)) "$WORK/AppIcon.svg.png" --out "$SET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$SET" -o assets/AppIcon.icns
echo assets/AppIcon.icns
