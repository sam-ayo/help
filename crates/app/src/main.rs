//! Help: a desktop app to talk to Claude Code and Codex through ACP.
//! The agents run on this Mac. They can also use their own Mac, a macOS VM.

mod bridge;
mod hotpatch;
mod presence;
mod shadcn_themes;
mod theme;
mod ui;
mod updater;

use std::path::{Path, PathBuf};

use gpui::{
    App, AppContext as _, Bounds, KeyBinding, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowOptions, point, px, size,
};
use gpui_component::Root;

/// Things that every part of the app needs.
#[derive(Clone)]
pub struct Env {
    pub runtime: tokio::runtime::Handle,
    pub mac: vm::AgentMac,
    pub bridge: acp_host::BridgeConfig,
    /// The folder that the agent works in when the user picked none: the home folder.
    pub workspace: PathBuf,
    /// The agent that the user picked last.
    pub agent_file: PathBuf,
    /// The saved conversation.
    pub conversation_file: PathBuf,
    /// The chats of an older version of the app. The conversation starts with them once.
    pub chats_file: PathBuf,
    /// The folder that the user picked last.
    pub folder_file: PathBuf,
    /// What the assistant knows about the user. Each new session reads it.
    pub memory_file: PathBuf,
    /// The summary of the earlier conversation, from the last compaction. Each new session reads it.
    pub history_file: PathBuf,
    /// The colors of the avatar that the user picked.
    pub avatar_file: PathBuf,
    /// The images that the user pastes, and the files that the agent sends. The conversation
    /// keeps the paths of these files.
    pub files_dir: PathBuf,
}

fn app_support() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join("Library/Application Support/Help")
}

/// The app had the name Agent Desk before. Move its folders to the new names once,
/// so that the VM, the conversation, and the memory stay.
fn move_old_folders() {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    for (old, new) in [
        ("Library/Application Support/AgentDesk", "Library/Application Support/Help"),
        ("AgentDesk", "Help"),
    ] {
        let (old, new) = (home.join(old), home.join(new));
        if old.exists() && !new.exists() {
            if let Err(e) = std::fs::rename(&old, &new) {
                tracing::warn!("cannot move {} to {}: {e}", old.display(), new.display());
            }
        }
    }
}

/// An app that starts from Finder gets a short PATH. Use the login shell PATH,
/// so that `npx`, `claude`, and `codex` resolve.
fn load_login_path() {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    if let Ok(out) = std::process::Command::new(shell)
        .args(["-lic", "printf %s \"$PATH\""])
        .output()
    {
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
    let guest = exe_dir.join("help-guest");
    if guest.exists() {
        if let Err(e) = std::fs::copy(&guest, tools.join("help-guest")) {
            tracing::warn!("cannot copy the guest agent: {e}");
        }
    } else {
        tracing::warn!(
            "{} is missing. Build the workspace with `cargo build`.",
            guest.display()
        );
    }
    let _ = std::fs::write(
        tools.join("install.sh"),
        include_str!("../../../guest/install.sh"),
    );
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("info".parse().unwrap()),
        )
        .init();
    load_login_path();

    move_old_folders();
    let support = app_support();
    let _ = std::fs::create_dir_all(&support);
    // The guest agent and the MCP bridge are next to the app. `scripts/hot.sh` runs the app
    // from a bundle that does not have them, so it sets HELP_HELPERS.
    let exe_dir = std::env::var_os("HELP_HELPERS")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf))
        })
        .unwrap_or_default();

    let paths = vm::Paths::under(&support);
    let _ = std::fs::create_dir_all(&paths.workspace);
    stage_guest_tools(&exe_dir, &paths.tools);
    let mac = vm::AgentMac::new(paths.clone());

    // Tokio runs on its own threads. GPUI has its own executor.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio");
    let handle = runtime.handle().clone();
    std::thread::spawn(move || runtime.block_on(std::future::pending::<()>()));

    let socket = support.join("bridge.sock");
    bridge::serve(&handle, socket.clone(), mac.clone());

    let env = Env {
        runtime: handle,
        mac,
        bridge: acp_host::BridgeConfig {
            command: exe_dir.join("help-mcp"),
            socket,
        },
        workspace: PathBuf::from(std::env::var("HOME").unwrap_or_default()),
        agent_file: support.join("agent"),
        conversation_file: support.join("conversation.json"),
        chats_file: support.join("chats.json"),
        folder_file: support.join("folder"),
        memory_file: support.join("memory.md"),
        history_file: support.join("help-memory.md"),
        avatar_file: support.join("avatar"),
        files_dir: support.join("files"),
    };

    let app = gpui_platform::application().with_assets(gpui_kit_assets::AllAssets);
    // A click on the Dock icon shows the hidden window.
    app.on_reopen(presence::show);
    app.run(move |cx: &mut App| {
        gpui_component::init(cx);
        hotpatch::connect(cx);
        cx.bind_keys([
            KeyBinding::new("cmd-.", ui::StopAgent, None),
            KeyBinding::new("cmd-ctrl-s", ui::ToggleSidebar, None),
            KeyBinding::new("cmd-w", presence::HideWindow, None),
            KeyBinding::new("cmd-q", presence::Quit, None),
        ]);
        cx.set_global(theme::ThemeChoice::load(support.join("theme")));
        theme::apply(None, cx);

        let bounds = Bounds::centered(None, size(px(1100.), px(800.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Help".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(18.), px(18.))),
            }),
            // The toolbars move the window. See `AppView::toolbar`.
            app_owns_titlebar_drag: true,
            window_background: WindowBackgroundAppearance::Blurred,
            window_min_size: Some(size(px(640.), px(480.))),
            ..Default::default()
        };
        let mut app_view = None;
        let window = cx
            .open_window(options, |window, cx| {
                window
                    .observe_window_appearance(|window, cx| theme::apply(Some(window), cx))
                    .detach();
                let view = cx.new(|cx| ui::AppView::new(env, window, cx));
                app_view = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("open window");
        presence::install(window, app_view.expect("the window has a view"), cx);
        updater::init(cx);
        cx.activate(true);
    });
}
