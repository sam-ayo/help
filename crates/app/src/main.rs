//! Agent Desk: a desktop app to talk to Claude Code and Codex through ACP.
//! The agents run on this Mac. They can also use their own Mac, a macOS VM.

mod bridge;
mod shadcn_themes;
mod theme;
mod ui;

use std::path::{Path, PathBuf};

use gpui::{
    App, AppContext as _, Bounds, KeyBinding, TitlebarOptions, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, point, px, size,
};
use gpui_component::Root;

/// Things that every part of the app needs.
#[derive(Clone)]
pub struct Env {
    pub runtime: tokio::runtime::Handle,
    pub mac: vm::AgentMac,
    pub bridge: acp_host::BridgeConfig,
    pub workspace: PathBuf,
    /// The agent that the user picked last. New chats use it.
    pub agent_file: PathBuf,
    /// The saved chats.
    pub chats_file: PathBuf,
}

fn app_support() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Library/Application Support/AgentDesk")
}

/// An app that starts from Finder gets a short PATH. Use the login shell PATH,
/// so that `npx`, `claude`, and `codex` resolve.
fn load_login_path() {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    if let Ok(out) = std::process::Command::new(shell).args(["-lic", "printf %s \"$PATH\""]).output() {
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !path.is_empty() {
            // SAFETY: this runs at startup, before any other thread exists.
            unsafe { std::env::set_var("PATH", path) };
        }
    }
}

/// Put the guest agent and its install script in the folder that the VM sees.
fn stage_guest_tools(exe_dir: &Path, tools: &Path) {
    let _ = std::fs::create_dir_all(tools);
    let guest = exe_dir.join("agent-desk-guest");
    if guest.exists() {
        if let Err(e) = std::fs::copy(&guest, tools.join("agent-desk-guest")) {
            tracing::warn!("cannot copy the guest agent: {e}");
        }
    } else {
        tracing::warn!("{} is missing. Build the workspace with `cargo build`.", guest.display());
    }
    let _ = std::fs::write(tools.join("install.sh"), include_str!("../../../guest/install.sh"));
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .init();
    load_login_path();

    let support = app_support();
    let _ = std::fs::create_dir_all(&support);
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_default();

    let paths = vm::Paths::under(&support);
    let _ = std::fs::create_dir_all(&paths.workspace);
    stage_guest_tools(&exe_dir, &paths.tools);
    let mac = vm::AgentMac::new(paths.clone());

    // Tokio runs on its own threads. GPUI has its own executor.
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio");
    let handle = runtime.handle().clone();
    std::thread::spawn(move || runtime.block_on(std::future::pending::<()>()));

    let socket = support.join("bridge.sock");
    bridge::serve(&handle, socket.clone(), mac.clone());

    let env = Env {
        runtime: handle,
        mac,
        bridge: acp_host::BridgeConfig {
            command: exe_dir.join("agent-desk-mcp"),
            socket,
        },
        workspace: PathBuf::from(std::env::var("HOME").unwrap_or_default()),
        agent_file: support.join("agent"),
        chats_file: support.join("chats.json"),
    };

    gpui_platform::application()
        .with_assets(gpui_kit_assets::AllAssets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            cx.bind_keys([
                KeyBinding::new("cmd-n", ui::NewChat, None),
                // Cmd-Backspace deletes, as in Mail. Cmd-W stays free, so that it does not delete a chat.
                KeyBinding::new("cmd-backspace", ui::CloseChat, None),
                KeyBinding::new("cmd-]", ui::NextChat, None),
                KeyBinding::new("cmd-[", ui::PreviousChat, None),
                KeyBinding::new("cmd-.", ui::StopAgent, None),
                KeyBinding::new("cmd-f", ui::FocusSearch, None),
                KeyBinding::new("cmd-ctrl-s", ui::ToggleSidebar, None),
                KeyBinding::new("cmd-1", ui::ShowAllChats, None),
                KeyBinding::new("cmd-2", ui::ShowClaudeChats, None),
                KeyBinding::new("cmd-3", ui::ShowCodexChats, None),
            ]);
            cx.set_global(theme::ThemeChoice::load(support.join("theme")));
            theme::apply(None, cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit()
                }
            })
            .detach();

            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Agent Desk".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(18.), px(18.))),
                }),
                // The toolbars move the window. See `AppView::toolbar`.
                app_owns_titlebar_drag: true,
                window_background: WindowBackgroundAppearance::Blurred,
                window_min_size: Some(size(px(960.), px(500.))),
                ..Default::default()
            };
            cx.open_window(options, |window, cx| {
                window
                    .observe_window_appearance(|window, cx| theme::apply(Some(window), cx))
                    .detach();
                let view = cx.new(|cx| ui::AppView::new(env, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("open window");
            cx.activate(true);
        });
}
