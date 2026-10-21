# Help

A desktop app, written in Rust with GPUI, to talk to coding agents through the
Agent Client Protocol (ACP):

- **Claude Code** through `@agentclientprotocol/claude-agent-acp`
- **Codex (ChatGPT)** through `@agentclientprotocol/codex-acp`

The agents run on your Mac. They also have their own Mac: a macOS virtual
machine (the "agent Mac") that the app runs with Apple Virtualization.framework.
The agents use the agent Mac when you ask them to, or for computer use that would
otherwise take over your screen.

## Architecture

```
Help.app (GPUI)
 ├─ acp_host ── ACP over stdio ──> claude-agent-acp / codex-acp (on this Mac)
 │                                   └─ MCP stdio ──> help-mcp (vm_* tools)
 ├─ bridge.sock  <───── Unix socket ───────────────────┘
 └─ vm (Virtualization.framework)
      └─ vsock port 7000 ──> help-guest (inside the agent Mac)
```

| Crate | Binary | Purpose |
|---|---|---|
| `crates/app` | `help` | GPUI window, the conversation, menu bar item and hotkey, memory, permission prompts, Agent Mac controls, bridge socket |
| `crates/acp_host` | — | Starts an ACP agent and turns its updates into UI events |
| `crates/vm` | — | Installs, starts, stops, and shows the macOS VM; vsock requests |
| `crates/mcp_bridge` | `help-mcp` | MCP server with `vm_shell`, `vm_screenshot`, `vm_click`, `vm_type`, `vm_key`, ... |
| `crates/guest_agent` | `help-guest` | Runs in the VM: shell, files, screenshots, mouse and keyboard input |
| `crates/protocol` | — | JSON-lines messages between the host and the guest |

### Shared folders in the agent Mac

| In the VM | On this Mac | Access |
|---|---|---|
| `/Volumes/My Shared Files/host-home` | your home folder | read-only |
| `/Volumes/My Shared Files/workspace` | `~/Help/workspace` | read-write |
| `/Volumes/My Shared Files/tools` | `~/Library/Application Support/Help/tools` | read-only |

## Requirements

- An Apple silicon Mac with macOS 14 or later.
- Node.js (`npx` must be on the login shell `PATH`).
- A Claude Code login (`claude`, then log in) and a Codex login (`codex login`).

## Build and run

```sh
scripts/bundle.sh            # builds target/debug/Help.app (signed)
open target/debug/Help.app
```

`cargo run -p app` also works. The Cargo runner (`scripts/sign-and-run.sh`)
signs the binary with the `com.apple.security.virtualization` entitlement,
because Virtualization.framework does not create a VM without it.

## Your assistant, always there

- **Option-Space** shows the window from any app. Push it again to hide the window.
- The app has an item in the menu bar: open the window or quit.
- A close of the window (or Cmd-W) hides the app. The agent keeps its session.
  To stop the app, use Cmd-Q or **Quit Help** in the menu bar item.
- **Memory** in the sidebar opens `~/Library/Application Support/Help/memory.md`.
  Each new session reads this file. The agent adds facts about you to it, and you
  can change it.
- **History** in the sidebar opens `~/Library/Application Support/Help/help-memory.md`.
  This file has a summary of the earlier conversation. When the messages after the
  last compaction have about 30,000 characters, the app compacts the conversation:
  the agent writes a new summary, the app saves it to this file, and a new session
  starts with the summary and the newest messages. Send `/compact` to compact the
  conversation now.
- **Refresh tools** in the sidebar starts the agent again in the same session, so
  that it gets the tools of an MCP server that you connected. The conversation
  stays. The app also does this when the MCP configuration of the agent changes
  (`~/.claude.json`, `.mcp.json`, or `~/.codex/config.toml`). If a turn is in
  progress, the refresh waits until the turn ends. Send `/refresh` to refresh now.

## Set up the agent Mac (one time)

1. In the app, click **Install macOS**. The app downloads the latest supported
   restore image (about 15 GB) and installs it. The disk is a 96 GB sparse file
   in `~/Library/Application Support/Help/vm`.
2. Click **Start**, then **Show screen**. Complete the macOS Setup Assistant.
3. In the VM, open Terminal and run:
   ```sh
   zsh "/Volumes/My Shared Files/tools/install.sh"
   ```
4. In the VM, open System Settings > Privacy & Security. Allow
   `help-guest` in **Accessibility** and in **Screen & System Audio
   Recording**. Then run:
   ```sh
   launchctl kickstart -k gui/$(id -u)/com.help.guest
   ```
5. Ask the agent: "Use your Mac to check its macOS version." It calls `vm_status`.

You can also test the VM without the GUI:

```sh
cargo run -p vm --example vmctl -- info      # support check and latest restore image
cargo run -p vm --example vmctl -- install   # download and install
cargo run -p vm --example vmctl -- ping      # start, ping the guest agent, stop
```

And test the agents without the GUI:

```sh
cargo build && cargo run -p acp_host --example smoke -- claude
cargo run -p acp_host --example smoke -- codex
```

## Release

Push a tag such as `v0.2.0`. The workflow in `.github/workflows/release.yml` then:

1. Sets the version from the tag (`scripts/set-version.sh`).
2. Builds `Help.app`, signs it with the Developer ID certificate, and notarizes it
   (`scripts/package.sh`).
3. Makes `help-<version>.dmg` and `help-<version>.zip`.
4. Signs the zip archive with the update key, and writes `latest.json`
   (`scripts/sign-release.sh`).
5. Publishes the files to `sam-ayo/help-releases`.

The message of an annotated tag becomes the release notes:

```sh
git tag -a v0.2.0 -m "Add the updater."
git push origin v0.2.0
```

To install, open the disk image and drag Help to Applications. A copy of Help in
an Applications folder looks for updates at start and each 6 hours. **Check for
Updates…** in the menu bar item starts a check now.

`scripts/release.sh` makes the same files on this Mac, in `target/release-files`.
The update key is in `~/.config/help/update-signing-key.pem`. Keep a backup copy
of it. Without this key, the installed apps cannot accept new updates.

## Not done yet

- Start at login, notifications, voice, and schedules.
- A setting to change the hotkey.
- Image (screenshot) display in the chat.
- Model selection (ACP config options).
