#!/bin/zsh
# Sign the zip archive of the app in a directory, and write latest.json there. The updater
# in the app reads latest.json (see crates/app/src/updater.rs).
#
# Usage: scripts/sign-release.sh <directory> [notes file]
# The directory must have help-<version>.zip. The notes file has the text that the update
# alert shows.
# Needs: jq and OpenSSL 3 (`brew install openssl@3`). The OpenSSL of macOS is LibreSSL,
# which cannot sign with Ed25519.
#
# The signing key comes from UPDATE_SIGNING_KEY (the PEM text) or from
# UPDATE_SIGNING_KEY_FILE. The default file is ~/.config/help/update-signing-key.pem.
#
# To make a new key, run:
#   openssl genpkey -algorithm ed25519 -out update-signing-key.pem
# Then put the public key in PUBLIC_KEY in crates/app/src/updater.rs. To get it, run:
#   openssl pkey -in update-signing-key.pem -pubout -outform DER | tail -c 32 | base64
# An app with the old public key cannot install updates from the new key.
set -euo pipefail
REPO=sam-ayo/help-releases
root="${0:A:h}/.."
dir="$1"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
notes="$(if [[ -n "${2:-}" ]]; then cat "$2"; fi)"

openssl=openssl
if [[ -x "$(brew --prefix 2>/dev/null)/opt/openssl@3/bin/openssl" ]]; then
  openssl="$(brew --prefix)/opt/openssl@3/bin/openssl"
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
key="$work/key.pem"
(
  umask 077
  if [[ -n "${UPDATE_SIGNING_KEY:-}" ]]; then
    printf '%s\n' "$UPDATE_SIGNING_KEY" > "$key"
  else
    cp "${UPDATE_SIGNING_KEY_FILE:-$HOME/.config/help/update-signing-key.pem}" "$key"
  fi
)

file="help-$version.zip"
if [[ ! -f "$dir/$file" ]]; then
  echo "$dir/$file is missing" >&2
  exit 1
fi
# The signed message is the version, a newline, and the file, so that an old file cannot
# show as a new version.
{ printf '%s\n' "$version"; cat "$dir/$file"; } > "$work/message"
signature=$("$openssl" pkeyutl -sign -rawin -inkey "$key" -in "$work/message" | base64 | tr -d '\n')
url="https://github.com/$REPO/releases/download/v$version/$file"

# The updater finds the file by std::env::consts::ARCH.
jq -n --arg v "$version" --arg n "$notes" --arg u "$url" --arg s "$signature" \
  '{version: $v, notes: $n, apps: {aarch64: {url: $u, signature: $s}}}' > "$dir/latest.json"
