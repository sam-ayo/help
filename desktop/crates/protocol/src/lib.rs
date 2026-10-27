//! Wire protocol between the host app and the guest agent that runs inside the VM.
//!
//! Transport: one JSON object per line over a vsock stream. The host sends a
//! [`Request`]; the guest sends back exactly one [`Response`] with the same id.

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};

/// The vsock port the guest agent listens on.
pub const GUEST_AGENT_PORT: u32 = 7000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub op: Op,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Ping,
    /// Run a shell command with `/bin/zsh -lc` and wait for it to finish.
    Shell {
        command: String,
        cwd: Option<String>,
        timeout_secs: Option<u64>,
    },
    ReadFile {
        path: String,
    },
    WriteFile {
        path: String,
        content: String,
    },
    Screenshot,
    MouseMove {
        x: f64,
        y: f64,
    },
    Click {
        x: f64,
        y: f64,
        #[serde(default)]
        button: MouseButton,
        #[serde(default = "one")]
        count: u32,
    },
    Drag {
        from_x: f64,
        from_y: f64,
        to_x: f64,
        to_y: f64,
    },
    Scroll {
        x: f64,
        y: f64,
        dx: i32,
        dy: i32,
    },
    TypeText {
        text: String,
    },
    /// A key combination such as `cmd+s`, `return`, or `ctrl+shift+tab`.
    Key {
        combo: String,
    },
    OpenApp {
        name: String,
    },
    TerminalCreate {
        command: String,
        args: Vec<String>,
        cwd: Option<String>,
        env: Vec<(String, String)>,
        output_byte_limit: Option<u64>,
    },
    TerminalOutput {
        terminal_id: String,
    },
    TerminalWait {
        terminal_id: String,
    },
    TerminalKill {
        terminal_id: String,
    },
    TerminalRelease {
        terminal_id: String,
    },
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    #[default]
    Left,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(flatten)]
    pub body: ResponseBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseBody {
    Ok(Reply),
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reply {
    Done,
    Pong {
        hostname: String,
        os_version: String,
    },
    Shell {
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
        timed_out: bool,
    },
    File {
        content: String,
    },
    Screenshot {
        /// Base64-encoded PNG.
        png_base64: String,
        width: u32,
        height: u32,
    },
    TerminalCreated {
        terminal_id: String,
    },
    TerminalOutput {
        output: String,
        truncated: bool,
        exit: Option<ExitStatus>,
    },
    TerminalExit(ExitStatus),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitStatus {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
}

/// Write one message as a JSON line.
pub fn write_msg<W: Write, T: Serialize>(w: &mut W, msg: &T) -> anyhow::Result<()> {
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()?;
    Ok(())
}

/// Read one JSON line. Returns `None` at end of stream.
pub fn read_msg<R: BufRead, T: for<'de> Deserialize<'de>>(r: &mut R) -> anyhow::Result<Option<T>> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&line)?))
}
