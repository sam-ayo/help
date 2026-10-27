#!/bin/zsh
# Build the release files on this Mac: the disk image, the zip archive, and latest.json.
# The release workflow (.github/workflows/release.yml) does the same steps on GitHub.
#
# Usage: scripts/release.sh [notes file]
# Output: target/release-files/
# Needs: the needs and the variables of scripts/package.sh and scripts/sign-release.sh.
set -euo pipefail
root="${0:A:h}/.."
out="$root/target/release-files"
rm -rf "$out"
mkdir -p "$out"
"$root/scripts/package.sh"
cp "$root"/target/package/help-*.dmg "$root"/target/package/help-*.zip "$out/"
"$root/scripts/sign-release.sh" "$out" "${1:-}"
echo
echo "release files: $out"
ls -l "$out"
