//! Runs an ACP agent (Claude Code or Codex) as a subprocess on the host Mac and
//! turns its session updates into simple [`Event`]s for the UI.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_client_protocol::schema::{ProtocolVersion, v1::*};
use agent_client_protocol::{AcpAgent, Agent, Client, ConnectionTo, Responder};
use tokio::sync::{mpsc, oneshot};

use attachment::Received;

mod attachment;
pub use attachment::{Attachment, file_uri, mime_for};
mod mcp_config;
pub use mcp_config::McpConfigWatch;
mod mcp_servers;
pub use mcp_servers::{McpServerSpec, McpTransport, mcp_servers};

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

/// A tool call to an MCP server, with the names that the agent uses. Claude Code changes the
/// characters of a server name that are not letters, digits, `_`, or `-` to `_`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCall {
    pub server: String,
    pub tool: String,
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
    /// A file in a message of the agent, for example an image.
    AgentFile(Attachment),
    Thought(String),
    /// A tool call. `kind` is `"subagent"` when the agent delegates work to a subagent.
    /// `parent` is the ID of the subagent tool call that made this tool call, if a subagent made it.
    /// `mcp` tells the MCP server and tool, if the tool is an MCP tool. `input` has the arguments
    /// of the tool.
    ToolCall {
        id: String,
        title: String,
        kind: String,
        status: ToolStatus,
        parent: Option<String>,
        mcp: Option<McpCall>,
        input: Option<serde_json::Value>,
    },
    /// `output` is the result of the tool in the form of an MCP `CallToolResult`. `files` has
    /// the images and other files in the content of the tool call, for example a screenshot.
    ToolUpdate {
        id: String,
        title: Option<String>,
        status: Option<ToolStatus>,
        detail: Option<String>,
        files: Vec<Attachment>,
        input: Option<serde_json::Value>,
        output: Option<serde_json::Value>,
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
    /// A message with the files that the user attached.
    Prompt { text: String, attachments: Vec<Attachment> },
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
/// when its process starts, so this gives an earlier session the new tools. The images and
/// other files that the agent sends go to the folder `files`.
pub fn start(
    runtime: &tokio::runtime::Handle,
    kind: AgentKind,
    cwd: PathBuf,
    bridge: BridgeConfig,
    files: PathBuf,
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
        if let Err(e) = run(kind, cwd, bridge, files, resume, events, rx).await {
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

/// The text and the files of the content of a tool call. The files go to the folder `dir`.
fn tool_content(content: Vec<ToolCallContent>, dir: &Path) -> (Option<String>, Vec<Attachment>) {
    let mut out = String::new();
    let mut files = Vec::new();
    for c in content {
        match c {
            ToolCallContent::Content(c) => match attachment::receive(c.content, dir) {
                Some(Received::Text(t)) => {
                    out.push_str(&t);
                    out.push('\n');
                }
                Some(Received::File(f)) => files.push(f),
                None => {}
            },
            ToolCallContent::Diff(d) => out.push_str(&format!("edited {}\n", d.path.display())),
            _ => {}
        }
    }
    ((!out.is_empty()).then_some(out), files)
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

/// The MCP server and tool of a tool call. Claude Code names an MCP tool
/// `mcp__<server>__<tool>`. Codex gives the title `Tool: <server>/<tool>`.
fn mcp_call(tc: &ToolCall) -> Option<McpCall> {
    let name = claude_meta(tc.meta.as_ref(), "toolName").unwrap_or(&tc.title);
    if let Some((server, tool)) = name.strip_prefix("mcp__").and_then(|rest| rest.split_once("__")) {
        return Some(McpCall { server: server.into(), tool: tool.into() });
    }
    let (server, tool) = tc.title.strip_prefix("Tool: ")?.split_once('/')?;
    Some(McpCall { server: server.into(), tool: tool.into() })
}

/// The arguments of a tool. Codex puts the arguments of an MCP tool in `arguments`, next to
/// `server` and `tool`.
fn tool_input(raw: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let raw = raw?;
    match raw.get("arguments") {
        Some(args) if raw.get("server").is_some() && raw.get("tool").is_some() => Some(args.clone()),
        _ => Some(raw.clone()),
    }
}

/// The result of a tool as an MCP `CallToolResult`. Codex sends the full result, sometimes in
/// `Ok` or `Err`. Claude Code sends only the content blocks, or a string.
fn tool_output(raw: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    use serde_json::{Value, json};
    let raw = raw?;
    Some(match raw {
        Value::Object(o) if o.contains_key("content") || o.contains_key("structuredContent") => raw.clone(),
        Value::Object(o) if o.len() == 1 && o.contains_key("Ok") => return tool_output(o.get("Ok")),
        Value::Object(o) if o.len() == 1 && o.contains_key("Err") => {
            json!({ "content": [{ "type": "text", "text": o["Err"].as_str().map(String::from).unwrap_or_else(|| o["Err"].to_string()) }], "isError": true })
        }
        Value::Array(blocks) => json!({ "content": blocks }),
        Value::String(text) => json!({ "content": [{ "type": "text", "text": text }] }),
        Value::Null => return None,
        other => json!({ "content": [{ "type": "text", "text": other.to_string() }] }),
    })
}

/// The activity and the name of a Codex subagent tool call: "Start subagent NAME",
/// "Complete subagent NAME", or "Interrupt subagent NAME".
fn codex_subagent(title: &str) -> Option<(&str, &str)> {
    let (activity, name) = title.split_once(" subagent ")?;
    matches!(activity, "Start" | "Complete" | "Interrupt").then_some((activity, name))
}

fn on_update(update: SessionUpdate, files: &Path, events: &mpsc::UnboundedSender<Event>) {
    let event = match update {
        SessionUpdate::AgentMessageChunk(c) => match attachment::receive(c.content, files) {
            Some(Received::Text(t)) => Event::AgentText(t),
            Some(Received::File(f)) => Event::AgentFile(f),
            None => return,
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
                    mcp: None,
                    input: None,
                },
                activity => Event::ToolUpdate {
                    id,
                    title: None,
                    status: Some(if activity == "Complete" { ToolStatus::Done } else { ToolStatus::Failed }),
                    detail: None,
                    files: Vec::new(),
                    input: None,
                    output: None,
                },
            }
        }
        SessionUpdate::ToolCall(tc) => Event::ToolCall {
            id: id_string(&tc.tool_call_id),
            title: if tc.title == "spawnAgent" { "Subagent".into() } else { tc.title.clone() },
            kind: if is_subagent(&tc) { SUBAGENT_KIND.into() } else { format!("{:?}", tc.kind) },
            status: status(tc.status),
            parent: claude_meta(tc.meta.as_ref(), "parentToolUseId").map(String::from),
            mcp: mcp_call(&tc),
            input: tool_input(tc.raw_input.as_ref()),
        },
        SessionUpdate::ToolCallUpdate(u) => {
            let (detail, files) = u.fields.content.map(|c| tool_content(c, files)).unwrap_or_default();
            Event::ToolUpdate {
                id: id_string(&u.tool_call_id),
                title: u.fields.title,
                status: u.fields.status.map(status),
                detail,
                files,
                input: tool_input(u.fields.raw_input.as_ref()),
                output: tool_output(u.fields.raw_output.as_ref()),
            }
        }
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
    files: PathBuf,
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
                    on_update(n.update, &files, &notif_events);
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
            // The agent tells which content blocks it takes in a prompt. See `attachment`.
            let prompt_caps = init.agent_capabilities.prompt_capabilities.clone();
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
                    Command::Prompt { text, attachments } => {
                        let blocks = attachment::prompt_blocks(text, &attachments, &prompt_caps);
                        let mut turn = std::pin::pin!(
                            cx.send_request(PromptRequest::new(session_id.clone(), blocks)).block_task()
                        );
                        // Keep reading commands during the turn, so that Cancel works.
                        let result = loop {
                            tokio::select! {
                                r = &mut turn => break r,
                                c = commands.recv() => match c {
                                    Some(Command::Cancel) => {
                                        let _ = cx.send_notification(CancelNotification::new(session_id.clone()));
                                    }
                                    Some(Command::Prompt { .. }) => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mcp_tools_get_their_server_and_tool() {
        let claude = ToolCall::new("1", "mcp__basic__get-time");
        assert_eq!(mcp_call(&claude), Some(McpCall { server: "basic".into(), tool: "get-time".into() }));
        let codex = ToolCall::new("2", "Tool: basic/get-time");
        assert_eq!(mcp_call(&codex), Some(McpCall { server: "basic".into(), tool: "get-time".into() }));
        assert_eq!(mcp_call(&ToolCall::new("3", "Read file")), None);
    }

    #[test]
    fn codex_input_is_the_arguments() {
        let raw = json!({ "server": "basic", "tool": "get-time", "arguments": { "zone": "UTC" } });
        assert_eq!(tool_input(Some(&raw)), Some(json!({ "zone": "UTC" })));
        assert_eq!(tool_input(Some(&json!({ "path": "a" }))), Some(json!({ "path": "a" })));
    }

    #[test]
    fn tool_output_becomes_a_call_tool_result() {
        let blocks = json!([{ "type": "text", "text": "hi" }]);
        assert_eq!(tool_output(Some(&blocks)), Some(json!({ "content": blocks })));
        let full = json!({ "content": [], "structuredContent": { "a": 1 } });
        assert_eq!(tool_output(Some(&json!({ "Ok": full }))), Some(full.clone()));
        assert_eq!(tool_output(Some(&json!("x"))), Some(json!({ "content": [{ "type": "text", "text": "x" }] })));
        assert_eq!(tool_output(Some(&json!({ "Err": "no" })))
            .and_then(|v| v.get("isError").cloned()), Some(json!(true)));
        assert_eq!(tool_output(Some(&serde_json::Value::Null)), None);
    }
}
