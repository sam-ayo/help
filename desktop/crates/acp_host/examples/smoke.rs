//! Headless test: start an agent, send one prompt, print the events.
//! Usage: cargo run -p acp_host --example smoke -- claude|codex [prompt] [session-id]
//! With a session ID, a new agent process continues that session.

use acp_host::{AgentKind, BridgeConfig, Command, Event};

fn main() {
    let kind = match std::env::args().nth(1).as_deref() {
        Some("codex") => AgentKind::Codex,
        _ => AgentKind::Claude,
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let exe_dir = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().to_path_buf();
    let bridge = BridgeConfig {
        command: exe_dir.join("help-mcp"),
        socket: "/tmp/help-smoke.sock".into(),
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let session = acp_host::start(
        rt.handle(),
        kind,
        std::env::temp_dir(),
        bridge,
        std::env::temp_dir().join("help-smoke-files"),
        std::env::args().nth(3),
        tx,
    );
    rt.block_on(async {
        while let Some(ev) = rx.recv().await {
            match ev {
                Event::Ready { session_id, current_mode, resumed } => {
                    println!("READY session={session_id} resumed={resumed} mode={current_mode:?}");
                    let prompt = std::env::args().nth(2).unwrap_or_else(|| {
                        "Call the help vm_status tool once, then reply with the word PONG and the tool's result in one line."
                            .into()
                    });
                    // HELP_SMOKE_ATTACH has the paths of files to attach, separated by `:`.
                    let attachments = std::env::var("HELP_SMOKE_ATTACH")
                        .unwrap_or_default()
                        .split(':')
                        .filter(|p| !p.is_empty())
                        .filter_map(|p| acp_host::Attachment::from_path(std::path::Path::new(p)).ok())
                        .collect();
                    session.send(Command::Prompt { text: prompt, attachments });
                }
                Event::Permission { title, options, reply } => {
                    println!("PERMISSION {title} -> {}", options[0].name);
                    let _ = reply.send(Some(options[0].id.clone()));
                }
                Event::TurnEnded(r) => {
                    println!("\nTURN ENDED {r}");
                    break;
                }
                Event::AgentText(t) => print!("{t}"),
                Event::AgentFile(f) => println!("FILE {}", f.path.display()),
                Event::Thought(_) => {}
                other => println!("{other:?}"),
            }
        }
    });
}
