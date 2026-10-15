# Agent Desk

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
AgentDesk.app (GPUI)
 ├─ acp_host ── ACP over stdio ──> claude-agent-acp / codex-acp (on this Mac)
 │                                   └─ MCP stdio ──> agent-desk-mcp (vm_* tools)
 ├─ bridge.sock  <───── Unix socket ───────────────────┘
 └─ vm (Virtualization.framework)
      └─ vsock port 7000 ──> agent-desk-guest (inside the agent Mac)
```

| Crate | Binary | Purpose |
|---|---|---|
| `crates/app` | `agent-desk` | GPUI window, chats, permission prompts, Agent Mac controls, bridge socket |
| `crates/acp_host` | — | Starts an ACP agent and turns its updates into UI events |
| `crates/vm` | — | Installs, starts, stops, and shows the macOS VM; vsock requests |
| `crates/mcp_bridge` | `agent-desk-mcp` | MCP server with `vm_shell`, `vm_screenshot`, `vm_click`, `vm_type`, `vm_key`, ... |
| `crates/guest_agent` | `agent-desk-guest` | Runs in the VM: shell, files, screenshots, mouse and keyboard input |
| `crates/protocol` | — | JSON-lines messages between the host and the guest |

### Shared folders in the agent Mac

| In the VM | On this Mac | Access |
|---|---|---|
| `/Volumes/My Shared Files/host-home` | your home folder | read-only |
| `/Volumes/My Shared Files/workspace` | `~/AgentDesk/workspace` | read-write |
| `/Volumes/My Shared Files/tools` | `~/Library/Application Support/AgentDesk/tools` | read-only |

## Requirements

- An Apple silicon Mac with macOS 14 or later.
- Node.js (`npx` must be on the login shell `PATH`).
- A Claude Code login (`claude`, then log in) and a Codex login (`codex login`).

## Build and run

```sh
scripts/bundle.sh            # builds target/debug/AgentDesk.app (signed)
open target/debug/AgentDesk.app
```

`cargo run -p app` also works. The Cargo runner (`scripts/sign-and-run.sh`)
signs the binary with the `com.apple.security.virtualization` entitlement,
because Virtualization.framework does not create a VM without it.

## Set up the agent Mac (one time)

1. In the app, click **Install macOS**. The app downloads the latest supported
   restore image (about 15 GB) and installs it. The disk is a 96 GB sparse file
   in `~/Library/Application Support/AgentDesk/vm`.
2. Click **Start**, then **Show screen**. Complete the macOS Setup Assistant.
3. In the VM, open Terminal and run:
   ```sh
   zsh "/Volumes/My Shared Files/tools/install.sh"
   ```
4. In the VM, open System Settings > Privacy & Security. Allow
   `agent-desk-guest` in **Accessibility** and in **Screen & System Audio
   Recording**. Then run:
   ```sh
   launchctl kickstart -k gui/$(id -u)/com.agentdesk.guest
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

## Not done yet

- Always-on features: a background daemon, a menu bar icon, a global hotkey, voice, and schedules.
- Saved chat history across app restarts.
- Image (screenshot) display in the chat.
- Model selection (ACP config options).
