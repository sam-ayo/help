#!/bin/zsh
# Make assets/AppIcon.icns and assets/DmgBackground.gif from the avatar in
# crates/app/src/ui/avatar.rs, in the default colors. Usage: scripts/make_icon.sh
# A test renders the images. sips makes each size of the icon. iconutil makes the icns file.
set -e
cd "${0:A:h}/.."
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
HELP_IMAGES_DIR="$WORK" cargo test -p app write_release_images -- --ignored --quiet
SET="$WORK/AppIcon.iconset"
mkdir "$SET"
for size in 16 32 128 256 512; do
  sips -z $size $size "$WORK/AppIcon.png" --out "$SET/icon_${size}x${size}.png" >/dev/null
  sips -z $((size * 2)) $((size * 2)) "$WORK/AppIcon.png" --out "$SET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$SET" -o assets/AppIcon.icns
cp "$WORK/DmgBackground.gif" assets/DmgBackground.gif
echo assets/AppIcon.icns assets/DmgBackground.gif
