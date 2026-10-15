//! Runs an ACP agent (Claude Code or Codex) as a subprocess on the host Mac and
//! turns its session updates into simple [`Event`]s for the UI.

use std::path::PathBuf;

use agent_client_protocol::schema::{ProtocolVersion, v1::*};
use agent_client_protocol::{AcpAgent, Agent, Client, ConnectionTo, Responder};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Claude,
    Codex,
}

impl AgentKind {
    pub const ALL: [AgentKind; 2] = [AgentKind::Claude, AgentKind::Codex];

    pub fn label(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude Code",
            AgentKind::Codex => "Codex (ChatGPT)",
        }
    }

    fn transport(self) -> AcpAgent {
        match self {
            AgentKind::Claude => AcpAgent::claude_agent(),
            AgentKind::Codex => AcpAgent::codex(),
        }
    }

    /// The mode that a new session starts in, if the agent offers it.
    /// Both agents skip the permission prompts by default.
    fn default_mode(self) -> &'static str {
        match self {
            AgentKind::Claude => "bypassPermissions",
            AgentKind::Codex => "agent-full-access",
        }
    }

    fn login_hint(self) -> &'static str {
        match self {
            AgentKind::Claude => "Run `claude` in a terminal and log in, then send your message again.",
            AgentKind::Codex => "Run `codex login` in a terminal, then send your message again.",
        }
    }
}

/// Where the MCP bridge is, and how it reaches the app.
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub command: PathBuf,
    pub socket: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone)]
pub struct PermissionOptionView {
    pub id: String,
    pub name: String,
    pub allow: bool,
}

#[derive(Debug)]
pub enum Event {
    /// The session is ready. `current_mode` is the agent mode that the session runs in.
    Ready {
        current_mode: Option<String>,
    },
    AgentText(String),
    Thought(String),
    ToolCall {
        id: String,
        title: String,
        kind: String,
        status: ToolStatus,
    },
    ToolUpdate {
        id: String,
        title: Option<String>,
        status: Option<ToolStatus>,
        detail: Option<String>,
    },
    Plan(Vec<(String, ToolStatus)>),
    /// The agent asks for permission. Send the chosen option ID, or `None` to reject.
    Permission {
        title: String,
        options: Vec<PermissionOptionView>,
        reply: oneshot::Sender<Option<String>>,
    },
    TurnEnded(String),
    Error(String),
    Closed,
}

#[derive(Debug)]
pub enum Command {
    Prompt(String),
    Cancel,
}

/// A running agent session. Drop it to stop the agent.
pub struct Session {
    commands: mpsc::UnboundedSender<Command>,
}

impl Session {
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

/// Start an agent session on `runtime`. Events go to `events`.
pub fn start(
    runtime: &tokio::runtime::Handle,
    kind: AgentKind,
    cwd: PathBuf,
    bridge: BridgeConfig,
    events: mpsc::UnboundedSender<Event>,
) -> Session {
    let (tx, rx) = mpsc::unbounded_channel();
    let ev = events.clone();
    runtime.spawn(async move {
        if !on_path("npx") {
            let _ = ev.send(Event::Error(
                "Cannot find `npx`. Install Node.js (for example `brew install node`), then send your message again."
                    .into(),
            ));
            let _ = ev.send(Event::Closed);
            return;
        }
        if !bridge.command.exists() {
            let _ = ev.send(Event::Error(format!(
                "Cannot find the MCP bridge at {}. The agent starts without the agent Mac tools.",
                bridge.command.display()
            )));
        }
        if let Err(e) = run(kind, cwd, bridge, events, rx).await {
            let mut msg = e.to_string();
            if msg.to_lowercase().contains("auth") {
                msg = format!("{msg}\n\n{}", kind.login_hint());
            }
            let _ = ev.send(Event::Error(msg));
        }
        let _ = ev.send(Event::Closed);
    });
    Session { commands: tx }
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

fn status(s: ToolCallStatus) -> ToolStatus {
    match s {
        ToolCallStatus::Pending => ToolStatus::Pending,
        ToolCallStatus::InProgress => ToolStatus::Running,
        ToolCallStatus::Completed => ToolStatus::Done,
        ToolCallStatus::Failed => ToolStatus::Failed,
        _ => ToolStatus::Running,
    }
}

fn content_text(content: &[ToolCallContent]) -> Option<String> {
    let mut out = String::new();
    for c in content {
        match c {
            ToolCallContent::Content(c) => {
                if let ContentBlock::Text(t) = &c.content {
                    out.push_str(&t.text);
                    out.push('\n');
                }
            }
            ToolCallContent::Diff(d) => out.push_str(&format!("edited {}\n", d.path.display())),
            _ => {}
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Turn ID newtypes into plain strings through their JSON form.
fn id_string<T: serde::Serialize>(id: &T) -> String {
    match serde_json::to_value(id) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(v) => v.to_string(),
        Err(_) => String::new(),
    }
}

fn on_update(update: SessionUpdate, events: &mpsc::UnboundedSender<Event>) {
    let event = match update {
        SessionUpdate::AgentMessageChunk(c) => match c.content {
            ContentBlock::Text(t) => Event::AgentText(t.text),
            _ => return,
        },
        SessionUpdate::AgentThoughtChunk(c) => match c.content {
            ContentBlock::Text(t) => Event::Thought(t.text),
            _ => return,
        },
        SessionUpdate::ToolCall(tc) => Event::ToolCall {
            id: id_string(&tc.tool_call_id),
            title: tc.title.clone(),
            kind: format!("{:?}", tc.kind),
            status: status(tc.status),
        },
        SessionUpdate::ToolCallUpdate(u) => Event::ToolUpdate {
            id: id_string(&u.tool_call_id),
            title: u.fields.title.clone(),
            status: u.fields.status.map(status),
            detail: u.fields.content.as_deref().and_then(content_text),
        },
        SessionUpdate::Plan(p) => Event::Plan(
            p.entries
                .into_iter()
                .map(|e| {
                    let s = match e.status {
                        PlanEntryStatus::Completed => ToolStatus::Done,
                        PlanEntryStatus::InProgress => ToolStatus::Running,
                        _ => ToolStatus::Pending,
                    };
                    (e.content, s)
                })
                .collect(),
        ),
        _ => return,
    };
    let _ = events.send(event);
}

async fn run(
    kind: AgentKind,
    cwd: PathBuf,
    bridge: BridgeConfig,
    events: mpsc::UnboundedSender<Event>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) -> agent_client_protocol::Result<()> {
    let notif_events = events.clone();
    let perm_events = events.clone();
    Client
        .builder()
        .name("agent-desk")
        .on_receive_notification(
            async move |n: SessionNotification, _cx: ConnectionTo<Agent>| {
                on_update(n.update, &notif_events);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |r: RequestPermissionRequest,
                        responder: Responder<RequestPermissionResponse>,
                        cx: ConnectionTo<Agent>| {
                let (reply_tx, reply_rx) = oneshot::channel();
                let options: Vec<PermissionOptionView> = r
                    .options
                    .iter()
                    .map(|o| PermissionOptionView {
                        id: id_string(&o.option_id),
                        name: o.name.clone(),
                        allow: matches!(
                            o.kind,
                            PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
                        ),
                    })
                    .collect();
                let title = r.tool_call.fields.title.clone().unwrap_or_else(|| "Run a tool".into());
                let _ = perm_events.send(Event::Permission {
                    title,
                    options,
                    reply: reply_tx,
                });
                // Wait for the user off the dispatch loop.
                cx.spawn(async move {
                    let outcome = match reply_rx.await {
                        Ok(Some(id)) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
                        _ => RequestPermissionOutcome::Cancelled,
                    };
                    responder.respond(RequestPermissionResponse::new(outcome))
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(kind.transport(), async move |cx: ConnectionTo<Agent>| {
            cx.send_request(
                InitializeRequest::new(ProtocolVersion::V1)
                    .client_capabilities(ClientCapabilities::new())
                    .client_info(Implementation::new("agent-desk", env!("CARGO_PKG_VERSION"))),
            )
            .block_task()
            .await?;

            let mcp = McpServer::Stdio(
                McpServerStdio::new("agent-desk", bridge.command.clone()).env(vec![EnvVariable::new(
                    "AGENT_DESK_SOCKET",
                    bridge.socket.to_string_lossy().to_string(),
                )]),
            );
            let session = cx
                .send_request(NewSessionRequest::new(cwd).mcp_servers(vec![mcp]))
                .block_task()
                .await?;
            let session_id = session.session_id.clone();
            // The app does not let the user change the mode. Every session runs in the default mode.
            let mut current_mode = session.modes.as_ref().map(|m| id_string(&m.current_mode_id));
            let default_mode = kind.default_mode();
            let offered = session
                .modes
                .as_ref()
                .is_some_and(|m| m.available_modes.iter().any(|m| id_string(&m.id) == default_mode));
            if current_mode.as_deref() != Some(default_mode) && offered {
                match cx
                    .send_request(SetSessionModeRequest::new(session_id.clone(), default_mode.to_string()))
                    .block_task()
                    .await
                {
                    Ok(_) => current_mode = Some(default_mode.to_string()),
                    Err(e) => {
                        let _ = events.send(Event::Error(format!("cannot set the default mode: {e}")));
                    }
                }
            }
            let _ = events.send(Event::Ready { current_mode });

            while let Some(command) = commands.recv().await {
                match command {
                    Command::Prompt(text) => {
                        let mut turn = std::pin::pin!(
                            cx.send_request(PromptRequest::new(session_id.clone(), vec![text.into()]))
                                .block_task()
                        );
                        // Keep reading commands during the turn, so that Cancel works.
                        let result = loop {
                            tokio::select! {
                                r = &mut turn => break r,
                                c = commands.recv() => match c {
                                    Some(Command::Cancel) => {
                                        let _ = cx.send_notification(CancelNotification::new(session_id.clone()));
                                    }
                                    Some(Command::Prompt(_)) => {}
                                    None => return Ok(()),
                                },
                            }
                        };
                        match result {
                            Ok(r) => {
                                let _ = events.send(Event::TurnEnded(format!("{:?}", r.stop_reason)));
                            }
                            Err(e) => {
                                let _ = events.send(Event::Error(e.to_string()));
                                let _ = events.send(Event::TurnEnded("Error".into()));
                            }
                        }
                    }
                    Command::Cancel => {}
                }
            }
            Ok(())
        })
        .await
}
