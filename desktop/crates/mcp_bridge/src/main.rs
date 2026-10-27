//! MCP server that the app gives to each ACP agent session.
//!
//! The agent runs on the user's Mac. These `vm_*` tools let it use its own Mac,
//! a macOS VM. Each call goes to the app over a Unix socket (`HELP_SOCKET`),
//! and the app sends it on to the guest agent in the VM.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use protocol::{MouseButton, Op, Reply, Request, Response, ResponseBody};
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const INSTRUCTIONS: &str = "\
You run on the user's Mac. You also have your own Mac: a macOS virtual machine (the \
'agent Mac'). Use the agent Mac when the user asks you to, or for computer use (clicking, \
typing, app control) that would otherwise take over the user's screen. Use vm_shell for \
commands, vm_screenshot to see the screen, and vm_click / vm_type / vm_key / vm_scroll / \
vm_drag to use apps. Screenshot pixel coordinates are the same as click coordinates. Take \
a new screenshot after each action that changes the screen. In the VM, the user's home \
folder is mounted read-only at '/Volumes/My Shared Files/host-home', and the shared \
workspace is at '/Volumes/My Shared Files/workspace' (on the user's Mac: ~/Help/workspace).";

#[derive(Clone)]
struct Bridge {
    socket: PathBuf,
    next_id: std::sync::Arc<AtomicU64>,
    tool_router: ToolRouter<Self>,
}

#[derive(Deserialize, JsonSchema)]
struct ShellArgs {
    /// The command line. It runs with `/bin/zsh -lc` in the VM.
    command: String,
    /// Working directory in the VM.
    cwd: Option<String>,
    /// Timeout in seconds. The default is 120.
    timeout_secs: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct PathArgs {
    /// An absolute path.
    path: String,
}

#[derive(Deserialize, JsonSchema)]
struct WriteArgs {
    /// An absolute path in the VM.
    path: String,
    /// The full file content.
    content: String,
}

#[derive(Deserialize, JsonSchema)]
struct PointArgs {
    x: f64,
    y: f64,
}

#[derive(Deserialize, JsonSchema)]
struct ClickArgs {
    x: f64,
    y: f64,
    /// "left" (default) or "right".
    button: Option<String>,
    /// 2 for a double click.
    count: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
struct DragArgs {
    from_x: f64,
    from_y: f64,
    to_x: f64,
    to_y: f64,
}

#[derive(Deserialize, JsonSchema)]
struct ScrollArgs {
    x: f64,
    y: f64,
    /// Lines to scroll right (negative for left).
    dx: Option<i32>,
    /// Lines to scroll down (negative for up).
    dy: Option<i32>,
}

#[derive(Deserialize, JsonSchema)]
struct TextArgs {
    text: String,
}

#[derive(Deserialize, JsonSchema)]
struct KeyArgs {
    /// For example "cmd+s", "return", "cmd+shift+4", "escape", "down".
    combo: String,
}

#[derive(Deserialize, JsonSchema)]
struct AppArgs {
    /// The application name, for example "Safari" or "Xcode".
    name: String,
}

fn err(e: impl std::fmt::Display) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

fn text(s: impl Into<String>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(s)]))
}

impl Bridge {
    async fn call(&self, op: Op) -> Result<Reply, McpError> {
        let mut stream = UnixStream::connect(&self.socket).await.map_err(|e| {
            err(format!("cannot reach the Help app at {}: {e}", self.socket.display()))
        })?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut line = serde_json::to_vec(&Request { id, op }).map_err(err)?;
        line.push(b'\n');
        stream.write_all(&line).await.map_err(err)?;
        let mut reader = BufReader::new(stream);
        let mut resp = String::new();
        reader.read_line(&mut resp).await.map_err(err)?;
        let resp: Response = serde_json::from_str(&resp).map_err(err)?;
        match resp.body {
            ResponseBody::Ok(reply) => Ok(reply),
            ResponseBody::Error(e) => Err(err(e)),
        }
    }

    async fn done(&self, op: Op, msg: &str) -> Result<CallToolResult, McpError> {
        self.call(op).await?;
        text(msg)
    }
}

#[tool_router]
impl Bridge {
    #[tool(description = "Check that the agent Mac VM runs, and get its macOS version.")]
    async fn vm_status(&self) -> Result<CallToolResult, McpError> {
        match self.call(Op::Ping).await? {
            Reply::Pong { hostname, os_version } => text(format!("VM is running: {hostname}, macOS {os_version}")),
            other => text(format!("{other:?}")),
        }
    }

    #[tool(description = "Run a shell command in the agent Mac VM and get stdout, stderr, and the exit code.")]
    async fn vm_shell(&self, Parameters(a): Parameters<ShellArgs>) -> Result<CallToolResult, McpError> {
        let reply = self
            .call(Op::Shell { command: a.command, cwd: a.cwd, timeout_secs: a.timeout_secs })
            .await?;
        let Reply::Shell { exit_code, stdout, stderr, timed_out } = reply else {
            return text(format!("{reply:?}"));
        };
        let mut s = format!("exit code: {exit_code:?}{}\n", if timed_out { " (timed out)" } else { "" });
        if !stdout.is_empty() {
            s.push_str(&format!("--- stdout ---\n{stdout}\n"));
        }
        if !stderr.is_empty() {
            s.push_str(&format!("--- stderr ---\n{stderr}\n"));
        }
        text(s)
    }

    #[tool(description = "Read a text file in the agent Mac VM.")]
    async fn vm_read_file(&self, Parameters(a): Parameters<PathArgs>) -> Result<CallToolResult, McpError> {
        match self.call(Op::ReadFile { path: a.path }).await? {
            Reply::File { content } => text(content),
            other => text(format!("{other:?}")),
        }
    }

    #[tool(description = "Write a text file in the agent Mac VM. This replaces the file.")]
    async fn vm_write_file(&self, Parameters(a): Parameters<WriteArgs>) -> Result<CallToolResult, McpError> {
        self.done(Op::WriteFile { path: a.path, content: a.content }, "written").await
    }

    #[tool(description = "Take a screenshot of the agent Mac VM screen. Pixel coordinates are click coordinates.")]
    async fn vm_screenshot(&self) -> Result<CallToolResult, McpError> {
        match self.call(Op::Screenshot).await? {
            Reply::Screenshot { png_base64, width, height } => Ok(CallToolResult::success(vec![
                ContentBlock::image(png_base64, "image/png"),
                ContentBlock::text(format!("screen size: {width}x{height}")),
            ])),
            other => text(format!("{other:?}")),
        }
    }

    #[tool(description = "Click at a point on the agent Mac VM screen.")]
    async fn vm_click(&self, Parameters(a): Parameters<ClickArgs>) -> Result<CallToolResult, McpError> {
        let button = match a.button.as_deref() {
            Some("right") => MouseButton::Right,
            _ => MouseButton::Left,
        };
        self.done(Op::Click { x: a.x, y: a.y, button, count: a.count.unwrap_or(1) }, "clicked").await
    }

    #[tool(description = "Move the mouse pointer on the agent Mac VM screen.")]
    async fn vm_move(&self, Parameters(a): Parameters<PointArgs>) -> Result<CallToolResult, McpError> {
        self.done(Op::MouseMove { x: a.x, y: a.y }, "moved").await
    }

    #[tool(description = "Drag with the left mouse button on the agent Mac VM screen.")]
    async fn vm_drag(&self, Parameters(a): Parameters<DragArgs>) -> Result<CallToolResult, McpError> {
        self.done(Op::Drag { from_x: a.from_x, from_y: a.from_y, to_x: a.to_x, to_y: a.to_y }, "dragged")
            .await
    }

    #[tool(description = "Scroll at a point on the agent Mac VM screen.")]
    async fn vm_scroll(&self, Parameters(a): Parameters<ScrollArgs>) -> Result<CallToolResult, McpError> {
        self.done(Op::Scroll { x: a.x, y: a.y, dx: a.dx.unwrap_or(0), dy: a.dy.unwrap_or(0) }, "scrolled")
            .await
    }

    #[tool(description = "Type text into the focused app in the agent Mac VM.")]
    async fn vm_type(&self, Parameters(a): Parameters<TextArgs>) -> Result<CallToolResult, McpError> {
        self.done(Op::TypeText { text: a.text }, "typed").await
    }

    #[tool(description = "Press a key combination in the agent Mac VM, for example cmd+s or return.")]
    async fn vm_key(&self, Parameters(a): Parameters<KeyArgs>) -> Result<CallToolResult, McpError> {
        self.done(Op::Key { combo: a.combo }, "pressed").await
    }

    #[tool(description = "Open an application in the agent Mac VM.")]
    async fn vm_open_app(&self, Parameters(a): Parameters<AppArgs>) -> Result<CallToolResult, McpError> {
        self.done(Op::OpenApp { name: a.name }, "opened").await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Bridge {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("help", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let socket = std::env::var("HELP_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| protocol_socket_default());
    let bridge = Bridge {
        socket,
        next_id: Default::default(),
        tool_router: Bridge::tool_router(),
    };
    bridge.serve(stdio()).await?.waiting().await?;
    Ok(())
}

fn protocol_socket_default() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join("Library/Application Support/Help/bridge.sock")
}
