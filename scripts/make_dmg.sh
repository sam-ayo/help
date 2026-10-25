#!/bin/zsh
# Put an app in a disk image (.dmg) with a link to Applications. The window of the disk
# image shows the moving avatar above an arrow from the app to Applications.
# Usage: scripts/make_dmg.sh <app> <dmg>
#
# Finder can only lay out the window of a mounted, read-write image. So the script lays out
# a read-write image, then hdiutil compresses it. Finder plays the GIF background
# (assets/DmgBackground.gif, from scripts/make_icon.sh). The window size and the positions
# of the icons match the background. See `write_release_images` in crates/app/src/ui/avatar.rs.
set -euo pipefail
cd "${0:A:h}/.."
app="$1"
dmg="$2"
name="${app:t}"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

stage="$work/stage"
mkdir -p "$stage/.background"
ditto "$app" "$stage/$name"
ln -s /Applications "$stage/Applications"
cp assets/DmgBackground.gif "$stage/.background/background.gif"
rw="$work/rw.dmg"
hdiutil create -volname Help -srcfolder "$stage" -fs HFS+ -format UDRW -ov "$rw" >/dev/null

# The volume can get another name, for example "Help 1", if a volume "Help" is mounted.
attach=$(hdiutil attach "$rw" -readwrite -noverify -noautoopen | grep Apple_HFS)
device=$(echo "$attach" | awk '{print $1}')
volume=$(echo "$attach" | sed 's|.*/Volumes/||; s/[[:space:]]*$//')
# The window is 640 x 420 points, without the title bar.
osascript <<OSA
tell application "Finder"
  tell disk "$volume"
    open
    set current view of container window to icon view
    set toolbar visible of container window to false
    set statusbar visible of container window to false
    set bounds of container window to {200, 120, 840, 540}
    set options to icon view options of container window
    set arrangement of options to not arranged
    set icon size of options to 96
    set text size of options to 13
    set background picture of options to file ".background:background.gif"
    set position of item "$name" of container window to {180, 290}
    set position of item "Applications" of container window to {460, 290}
    update without registering applications
    delay 1
    close
  end tell
end tell
OSA
sync
# Finder can keep the volume busy for a short time after the window closes.
for i in {1..5}; do
  hdiutil detach "$device" >/dev/null && break
  sleep 2
done
hdiutil convert "$rw" -format UDZO -ov -o "$dmg" >/dev/null
echo "$dmg"
