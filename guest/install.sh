#!/bin/zsh
# Install the Agent Desk guest agent in the agent Mac VM.
# Run this in Terminal inside the VM:
#   zsh "/Volumes/My Shared Files/tools/install.sh"
set -e
DEST="$HOME/Library/AgentDesk"
PLIST="$HOME/Library/LaunchAgents/com.agentdesk.guest.plist"
mkdir -p "$DEST" "$HOME/Library/LaunchAgents"
cp "/Volumes/My Shared Files/tools/agent-desk-guest" "$DEST/agent-desk-guest"
chmod +x "$DEST/agent-desk-guest"
cat > "$PLIST" <<PLISTEOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.agentdesk.guest</string>
  <key>ProgramArguments</key><array><string>$DEST/agent-desk-guest</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardErrorPath</key><string>$DEST/guest.log</string>
</dict>
</plist>
PLISTEOF
launchctl bootout "gui/$(id -u)/com.agentdesk.guest" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"
echo "The guest agent runs."
echo "Next: in System Settings > Privacy & Security, allow agent-desk-guest in"
echo "  1. Accessibility (for mouse and keyboard control)"
echo "  2. Screen & System Audio Recording (for screenshots)"
echo "Then run: launchctl kickstart -k gui/\$(id -u)/com.agentdesk.guest"
open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
