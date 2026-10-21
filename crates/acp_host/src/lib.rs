//! Runs an ACP agent (Claude Code or Codex) as a subprocess on the host Mac and
//! turns its session updates into simple [`Event`]s for the UI.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_client_protocol::schema::{ProtocolVersion, v1::*};
use agent_client_protocol::{AcpAgent, Agent, Client, ConnectionTo, Responder};
use tokio::sync::{mpsc, oneshot};

mod mcp_config;
pub use mcp_config::McpConfigWatch;

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
    /// `resumed` is true when the agent continues the session that `start` got in `resume`,
    /// so it has the earlier messages.
    Ready {
        session_id: String,
        current_mode: Option<String>,
        resumed: bool,
    },
    AgentText(String),
    Thought(String),
    /// A tool call. `kind` is `"subagent"` when the agent delegates work to a subagent.
    /// `parent` is the ID of the subagent tool call that made this tool call, if a subagent made it.
    ToolCall {
        id: String,
        title: String,
        kind: String,
        status: ToolStatus,
        parent: Option<String>,
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

/// Start an agent session on `runtime`. Events go to `events`. With `resume`, a new agent
/// process continues that session, if the agent can. The agent reads its MCP servers again
/// when its process starts, so this gives an earlier session the new tools.
pub fn start(
    runtime: &tokio::runtime::Handle,
    kind: AgentKind,
    cwd: PathBuf,
    bridge: BridgeConfig,
    resume: Option<String>,
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
        if let Err(e) = run(kind, cwd, bridge, resume, events, rx).await {
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

/// The tool kind of a tool call that starts a subagent.
pub const SUBAGENT_KIND: &str = "subagent";

/// Claude Code reports the name of its tool in `_meta.claudeCode`.
fn claude_meta<'a>(meta: Option<&'a Meta>, key: &str) -> Option<&'a str> {
    meta?.get("claudeCode")?.get(key)?.as_str()
}

/// The tool call starts a subagent: the `Agent` or `Task` tool of Claude Code, or the
/// `spawnAgent` tool of Codex.
fn is_subagent(tc: &ToolCall) -> bool {
    matches!(claude_meta(tc.meta.as_ref(), "toolName"), Some("Agent" | "Task")) || tc.title == "spawnAgent"
}

/// The activity and the name of a Codex subagent tool call: "Start subagent NAME",
/// "Complete subagent NAME", or "Interrupt subagent NAME".
fn codex_subagent(title: &str) -> Option<(&str, &str)> {
    let (activity, name) = title.split_once(" subagent ")?;
    matches!(activity, "Start" | "Complete" | "Interrupt").then_some((activity, name))
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
        // Codex reports the start and the end of a subagent as two tool calls that each end at
        // once. They become one subagent card, which works until the subagent completes.
        SessionUpdate::ToolCall(tc) if codex_subagent(&tc.title).is_some() => {
            let Some((activity, name)) = codex_subagent(&tc.title) else { return };
            let id = format!("codex-subagent-{name}");
            match activity {
                "Start" => Event::ToolCall {
                    id,
                    title: name.to_string(),
                    kind: SUBAGENT_KIND.into(),
                    status: ToolStatus::Running,
                    parent: None,
                },
                "Complete" => Event::ToolUpdate { id, title: None, status: Some(ToolStatus::Done), detail: None },
                _ => Event::ToolUpdate { id, title: None, status: Some(ToolStatus::Failed), detail: None },
            }
        }
        SessionUpdate::ToolCall(tc) => Event::ToolCall {
            id: id_string(&tc.tool_call_id),
            title: if tc.title == "spawnAgent" { "Subagent".into() } else { tc.title.clone() },
            kind: if is_subagent(&tc) { SUBAGENT_KIND.into() } else { format!("{:?}", tc.kind) },
            status: status(tc.status),
            parent: claude_meta(tc.meta.as_ref(), "parentToolUseId").map(String::from),
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

/// Open the session: continue `resume` if the agent can, or else start a new session.
/// `session/resume` comes first, because `session/load` sends all the earlier messages
/// again. The UI has them already, so `replaying` drops them.
async fn open_session(
    cx: &ConnectionTo<Agent>,
    caps: &AgentCapabilities,
    cwd: &Path,
    mcp: Vec<McpServer>,
    resume: Option<String>,
    replaying: &AtomicBool,
) -> agent_client_protocol::Result<(String, Option<SessionModeState>, bool)> {
    if let Some(id) = resume {
        let result = if caps.session_capabilities.resume.is_some() {
            cx.send_request(ResumeSessionRequest::new(id.clone(), cwd).mcp_servers(mcp.clone()))
                .block_task()
                .await
                .map(|r| r.modes)
        } else if caps.load_session {
            replaying.store(true, Ordering::SeqCst);
            let r = cx
                .send_request(LoadSessionRequest::new(id.clone(), cwd).mcp_servers(mcp.clone()))
                .block_task()
                .await
                .map(|r| r.modes);
            replaying.store(false, Ordering::SeqCst);
            r
        } else {
            Err(agent_client_protocol::Error::method_not_found())
        };
        match result {
            Ok(modes) => return Ok((id, modes, true)),
            Err(e) => tracing::warn!("cannot continue the session {id}, so a new session starts: {e}"),
        }
    }
    let session = cx
        .send_request(NewSessionRequest::new(cwd).mcp_servers(mcp))
        .block_task()
        .await?;
    Ok((id_string(&session.session_id), session.modes, false))
}

async fn run(
    kind: AgentKind,
    cwd: PathBuf,
    bridge: BridgeConfig,
    resume: Option<String>,
    events: mpsc::UnboundedSender<Event>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) -> agent_client_protocol::Result<()> {
    let notif_events = events.clone();
    let perm_events = events.clone();
    let replaying = Arc::new(AtomicBool::new(false));
    let notif_replaying = replaying.clone();
    Client
        .builder()
        .name("help")
        .on_receive_notification(
            async move |n: SessionNotification, _cx: ConnectionTo<Agent>| {
                if !notif_replaying.load(Ordering::SeqCst) {
                    on_update(n.update, &notif_events);
                }
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
            let init = cx
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1)
                        .client_capabilities(ClientCapabilities::new())
                        .client_info(Implementation::new("help", env!("CARGO_PKG_VERSION"))),
                )
                .block_task()
                .await?;

            let mcp = McpServer::Stdio(
                McpServerStdio::new("help", bridge.command.clone()).env(vec![EnvVariable::new(
                    "HELP_SOCKET",
                    bridge.socket.to_string_lossy().to_string(),
                )]),
            );
            let (session_id, modes, resumed) =
                open_session(&cx, &init.agent_capabilities, &cwd, vec![mcp], resume, &replaying).await?;
            // The app does not let the user change the mode. Every session runs in the default mode.
            let mut current_mode = modes.as_ref().map(|m| id_string(&m.current_mode_id));
            let default_mode = kind.default_mode();
            let offered = modes
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
            let _ = events.send(Event::Ready { session_id: session_id.clone(), current_mode, resumed });

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
