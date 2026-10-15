//! Long-running commands for the ACP terminal methods. Output collects in a
//! buffer that the host reads at any time.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};

use anyhow::{Context, Result};
use protocol::{ExitStatus, Reply};

#[derive(Clone, Default)]
pub struct Terminals {
    inner: Arc<Mutex<HashMap<String, Arc<Term>>>>,
}

struct Term {
    state: Mutex<TermState>,
    exited: Condvar,
    child: Mutex<Child>,
}

struct TermState {
    output: Vec<u8>,
    limit: Option<u64>,
    truncated: bool,
    exit: Option<ExitStatus>,
}

impl Terminals {
    pub fn create(
        &self,
        command: &str,
        args: &[String],
        cwd: Option<&str>,
        env: &[(String, String)],
        limit: Option<u64>,
    ) -> Result<String> {
        // Run through a login shell, so that PATH matches an interactive shell.
        let mut line = shell_words(command);
        for a in args {
            line.push(' ');
            line.push_str(&shell_words(a));
        }
        let mut cmd = Command::new("/bin/zsh");
        cmd.args(["-lc", &format!("exec {line} 2>&1")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().with_context(|| format!("spawn {command}"))?;
        let mut stdout = child.stdout.take().unwrap();
        let term = Arc::new(Term {
            state: Mutex::new(TermState {
                output: Vec::new(),
                limit,
                truncated: false,
                exit: None,
            }),
            exited: Condvar::new(),
            child: Mutex::new(child),
        });
        let id = format!("term-{}", next_id());
        self.inner.lock().unwrap().insert(id.clone(), term.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => term.state.lock().unwrap().push(&buf[..n]),
                }
            }
            let status = term.child.lock().unwrap().wait();
            let exit = match status {
                Ok(s) => {
                    use std::os::unix::process::ExitStatusExt;
                    ExitStatus {
                        exit_code: s.code(),
                        signal: s.signal().map(|n| format!("signal {n}")),
                    }
                }
                Err(e) => ExitStatus {
                    exit_code: None,
                    signal: Some(e.to_string()),
                },
            };
            term.state.lock().unwrap().exit = Some(exit);
            term.exited.notify_all();
        });
        Ok(id)
    }

    fn get(&self, id: &str) -> Result<Arc<Term>> {
        self.inner
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .with_context(|| format!("no terminal {id}"))
    }

    pub fn output(&self, id: &str) -> Result<Reply> {
        let term = self.get(id)?;
        let state = term.state.lock().unwrap();
        Ok(Reply::TerminalOutput {
            output: String::from_utf8_lossy(&state.output).into_owned(),
            truncated: state.truncated,
            exit: state.exit.clone(),
        })
    }

    pub fn wait(&self, id: &str) -> Result<ExitStatus> {
        let term = self.get(id)?;
        let mut state = term.state.lock().unwrap();
        while state.exit.is_none() {
            state = term.exited.wait(state).unwrap();
        }
        Ok(state.exit.clone().unwrap())
    }

    pub fn kill(&self, id: &str) -> Result<()> {
        let term = self.get(id)?;
        let _ = term.child.lock().unwrap().kill();
        Ok(())
    }

    pub fn release(&self, id: &str) {
        if let Some(term) = self.inner.lock().unwrap().remove(id) {
            let _ = term.child.lock().unwrap().kill();
        }
    }
}

impl TermState {
    fn push(&mut self, bytes: &[u8]) {
        self.output.extend_from_slice(bytes);
        if let Some(limit) = self.limit {
            let limit = limit as usize;
            if self.output.len() > limit {
                let mut cut = self.output.len() - limit;
                // Keep the buffer on a UTF-8 character boundary.
                while cut < self.output.len() && (self.output[cut] & 0xC0) == 0x80 {
                    cut += 1;
                }
                self.output.drain(..cut);
                self.truncated = true;
            }
        }
    }
}

fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

/// Quote an argument for zsh unless it is a plain word. The ACP agents often
/// send a full command line in `command` with no args, so a command that has
/// spaces and no args is passed unchanged.
fn shell_words(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}
