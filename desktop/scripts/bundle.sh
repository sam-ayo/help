#!/bin/zsh
# Build Help.app in target/<profile>/. Usage: scripts/bundle.sh [--release]
set -e
cd "${0:A:h}/.."
PROFILE=debug
if [[ "$1" == "--release" ]]; then PROFILE=release; cargo build --release; else cargo build; fi
APP="target/$PROFILE/Help.app"
# The version of the workspace. scripts/set-version.sh changes it.
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/$PROFILE/help target/$PROFILE/help-mcp target/$PROFILE/help-guest "$APP/Contents/MacOS/"
cp assets/AppIcon.icns "$APP/Contents/Resources/"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Help</string>
  <key>CFBundleDisplayName</key><string>Help</string>
  <key>CFBundleIdentifier</key><string>com.help.app</string>
  <key>CFBundleExecutable</key><string>help</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
codesign --force --sign - "$APP/Contents/MacOS/help-mcp"
codesign --force --sign - "$APP/Contents/MacOS/help-guest"
codesign --force --sign - --entitlements scripts/help.entitlements "$APP"
echo "$APP"
