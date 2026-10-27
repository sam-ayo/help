//! The guest agent runs inside the agent Mac VM. It listens on vsock and runs
//! the host's requests: shell commands, files, screenshots, input, and terminals.
//!
//! Install it as a LaunchAgent in the guest user session, so that it can see the
//! screen and post input events. See `guest/install.sh`.

mod input;
mod terminal;
mod vsock;

use std::io::BufReader;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use base64::Engine;
use protocol::{Op, Reply, Request, Response, ResponseBody};

fn main() -> Result<()> {
    let port = std::env::var("HELP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(protocol::GUEST_AGENT_PORT);
    let listener = vsock::Listener::bind(port)?;
    eprintln!("help-guest: listening on vsock port {port}");
    let terminals = terminal::Terminals::default();
    loop {
        let stream = match listener.accept() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("accept failed: {e}");
                continue;
            }
        };
        let terminals = terminals.clone();
        std::thread::spawn(move || {
            if let Err(e) = serve(stream, terminals) {
                eprintln!("connection ended: {e:#}");
            }
        });
    }
}

fn serve(stream: std::fs::File, terminals: terminal::Terminals) -> Result<()> {
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    while let Some(req) = protocol::read_msg::<_, Request>(&mut reader)? {
        let body = match handle(req.op, &terminals) {
            Ok(reply) => ResponseBody::Ok(reply),
            Err(e) => ResponseBody::Error(format!("{e:#}")),
        };
        protocol::write_msg(&mut writer, &Response { id: req.id, body })?;
    }
    Ok(())
}

fn handle(op: Op, terminals: &terminal::Terminals) -> Result<Reply> {
    Ok(match op {
        Op::Ping => Reply::Pong {
            hostname: run_capture("hostname", &[])?,
            os_version: run_capture("sw_vers", &["-productVersion"])?,
        },
        Op::Shell {
            command,
            cwd,
            timeout_secs,
        } => shell(&command, cwd.as_deref(), timeout_secs.unwrap_or(120))?,
        Op::ReadFile { path } => Reply::File {
            content: std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?,
        },
        Op::WriteFile { path, content } => {
            if let Some(parent) = std::path::Path::new(&path).parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, content).with_context(|| format!("write {path}"))?;
            Reply::Done
        }
        Op::Screenshot => screenshot()?,
        Op::MouseMove { x, y } => {
            input::mouse_move(x, y);
            Reply::Done
        }
        Op::Click {
            x,
            y,
            button,
            count,
        } => {
            input::click(x, y, button, count);
            Reply::Done
        }
        Op::Drag {
            from_x,
            from_y,
            to_x,
            to_y,
        } => {
            input::drag(from_x, from_y, to_x, to_y);
            Reply::Done
        }
        Op::Scroll { x, y, dx, dy } => {
            input::scroll(x, y, dx, dy);
            Reply::Done
        }
        Op::TypeText { text } => {
            input::type_text(&text);
            Reply::Done
        }
        Op::Key { combo } => {
            input::key_combo(&combo)?;
            Reply::Done
        }
        Op::OpenApp { name } => {
            shell(&format!("open -a {}", shell_quote(&name)), None, 30)?;
            Reply::Done
        }
        Op::TerminalCreate {
            command,
            args,
            cwd,
            env,
            output_byte_limit,
        } => Reply::TerminalCreated {
            terminal_id: terminals.create(&command, &args, cwd.as_deref(), &env, output_byte_limit)?,
        },
        Op::TerminalOutput { terminal_id } => terminals.output(&terminal_id)?,
        Op::TerminalWait { terminal_id } => Reply::TerminalExit(terminals.wait(&terminal_id)?),
        Op::TerminalKill { terminal_id } => {
            terminals.kill(&terminal_id)?;
            Reply::Done
        }
        Op::TerminalRelease { terminal_id } => {
            terminals.release(&terminal_id);
            Reply::Done
        }
    })
}

fn shell(command: &str, cwd: Option<&str>, timeout_secs: u64) -> Result<Reply> {
    let mut cmd = Command::new("/bin/zsh");
    cmd.args(["-lc", command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || read_all(stdout));
    let err = std::thread::spawn(move || read_all(stderr));
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            timed_out = true;
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok(Reply::Shell {
        exit_code: status.code(),
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        timed_out,
    })
}

fn read_all(mut r: impl std::io::Read) -> String {
    let mut buf = Vec::new();
    let _ = r.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn run_capture(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program).args(args).output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Capture the main display and scale the image to display points, so that
/// screenshot pixel coordinates are the same as click coordinates.
fn screenshot() -> Result<Reply> {
    let (width, height) = input::display_size_points();
    let path = std::env::temp_dir().join(format!("help-shot-{}.png", std::process::id()));
    let path_str = path.to_string_lossy().to_string();
    let status = Command::new("/usr/sbin/screencapture")
        .args(["-x", "-C", "-t", "png", &path_str])
        .status()?;
    anyhow::ensure!(
        status.success(),
        "screencapture failed. Give Screen Recording permission to help-guest in the VM."
    );
    Command::new("/usr/bin/sips")
        .args(["-z", &height.to_string(), &width.to_string(), &path_str])
        .stdout(Stdio::null())
        .status()?;
    let png = std::fs::read(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(Reply::Screenshot {
        png_base64: base64::engine::general_purpose::STANDARD.encode(png),
        width,
        height,
    })
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
