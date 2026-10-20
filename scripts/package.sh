#!/bin/zsh
# Build Help.app for release, and put it in a disk image (.dmg) and a zip archive.
#
# The disk image is for people: they drag the app to Applications. The updater in the app
# reads the zip archive (see crates/app/src/updater.rs). Help needs Apple silicon, because
# the agent Mac is a macOS VM, so the script builds only for arm64.
#
# Usage: scripts/package.sh
# Output: target/package/Help.app, target/package/help-<version>.dmg, and
#         target/package/help-<version>.zip
# Needs: cargo and the Xcode command line tools.
#
# Signature and notarization. Without these variables, the script makes an app with an
# ad hoc signature. That app works on this Mac, but Gatekeeper blocks it on other Macs.
#   CODESIGN_IDENTITY: the "Developer ID Application" certificate, for example
#     "Developer ID Application: Name (TEAMID)".
#   Notarization needs CODESIGN_IDENTITY and one of these:
#     NOTARY_PROFILE: a keychain profile from `xcrun notarytool store-credentials`.
#     NOTARY_KEY_FILE, NOTARY_KEY_ID, and NOTARY_ISSUER: an App Store Connect API key
#       (the .p8 file), its key ID, and its issuer ID.
set -euo pipefail
root="${0:A:h}/.."
cd "$root"
out=target/package
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

scripts/bundle.sh --release
rm -rf "$out"
mkdir -p "$out"
app="$out/Help.app"
ditto target/release/Help.app "$app"

notary_args=()
if [[ -n "${NOTARY_PROFILE:-}" ]]; then
  notary_args=(--keychain-profile "$NOTARY_PROFILE")
elif [[ -n "${NOTARY_KEY_FILE:-}" ]]; then
  notary_args=(--key "$NOTARY_KEY_FILE" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER")
fi

# Sends a file to Apple, and waits for the result. Shows the log when Apple rejects it.
notarize() {
  local file="$1" result="$out/notary.json"
  echo "notarize $file"
  xcrun notarytool submit "$file" "${notary_args[@]}" --wait --timeout 1h \
    --output-format json > "$result" || true
  if [[ "$(plutil -extract status raw "$result" 2>/dev/null || true)" != Accepted ]]; then
    cat "$result" >&2
    local id="$(plutil -extract id raw "$result" 2>/dev/null || true)"
    [[ -n "$id" ]] && xcrun notarytool log "$id" "${notary_args[@]}" >&2 || true
    echo "notarization failed" >&2
    exit 1
  fi
  rm -f "$result"
}

# Notarization needs a Developer ID signature, the hardened runtime, and a secure
# timestamp on all of the code. Sign the helpers first, because the seal of the bundle
# includes them. The guest agent runs in the agent Mac, and the VM user gives it
# permissions there. A Developer ID signature keeps these permissions after an update.
if [[ -n "${CODESIGN_IDENTITY:-}" ]]; then
  echo "sign the app with $CODESIGN_IDENTITY"
  sign=(codesign --force --options runtime --timestamp --sign "$CODESIGN_IDENTITY")
  "${sign[@]}" "$app/Contents/MacOS/help-mcp"
  "${sign[@]}" "$app/Contents/MacOS/help-guest"
  "${sign[@]}" --entitlements scripts/help.entitlements "$app"
  codesign --verify --deep --strict "$app"
fi

# Notarize the app before it goes in the disk image. The staple step then attaches the
# ticket to the app, so that a copy of the app from the image or from the zip archive
# opens without a network connection.
if [[ -n "${CODESIGN_IDENTITY:-}" && ${#notary_args[@]} -gt 0 ]]; then
  ditto -c -k --keepParent "$app" "$out/notarize.zip"
  notarize "$out/notarize.zip"
  rm "$out/notarize.zip"
  xcrun stapler staple "$app"
fi

# The updater replaces the installed app with the app in this archive.
ditto -c -k --keepParent "$app" "$out/help-$version.zip"

# The disk image has the app and a link to Applications.
dmg="$out/help-$version.dmg"
stage="$out/dmg"
mkdir -p "$stage"
ditto "$app" "$stage/Help.app"
ln -s /Applications "$stage/Applications"
hdiutil create -volname Help -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$dmg"
rm -rf "$stage"

if [[ -n "${CODESIGN_IDENTITY:-}" ]]; then
  codesign --force --timestamp --sign "$CODESIGN_IDENTITY" "$dmg"
  if [[ ${#notary_args[@]} -gt 0 ]]; then
    notarize "$dmg"
    xcrun stapler staple "$dmg"
    spctl --assess --type open --context context:primary-signature --verbose "$dmg"
    spctl --assess --type execute --verbose "$app"
  fi
fi

echo
echo "app: $app"
echo "disk image: $dmg"
echo "archive: $out/help-$version.zip"
