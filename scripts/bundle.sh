#!/bin/zsh
# Build AgentDesk.app in target/<profile>/. Usage: scripts/bundle.sh [--release]
set -e
cd "${0:A:h}/.."
PROFILE=debug
if [[ "$1" == "--release" ]]; then PROFILE=release; cargo build --release; else cargo build; fi
APP="target/$PROFILE/AgentDesk.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp target/$PROFILE/agent-desk target/$PROFILE/agent-desk-mcp target/$PROFILE/agent-desk-guest "$APP/Contents/MacOS/"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Agent Desk</string>
  <key>CFBundleDisplayName</key><string>Agent Desk</string>
  <key>CFBundleIdentifier</key><string>com.agentdesk.app</string>
  <key>CFBundleExecutable</key><string>agent-desk</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
codesign --force --sign - "$APP/Contents/MacOS/agent-desk-mcp"
codesign --force --sign - "$APP/Contents/MacOS/agent-desk-guest"
codesign --force --sign - --entitlements scripts/agent-desk.entitlements "$APP"
echo "$APP"
