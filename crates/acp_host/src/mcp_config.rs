//! The files where an agent reads its MCP servers. An agent reads them only when its session
//! starts, so the app watches them, and starts the session again when they change.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::AgentKind;

/// The keys of `~/.claude.json` that tell Claude Code which MCP servers to use. The other keys
/// change after each session, so the watch does not read them.
const CLAUDE_KEYS: [&str; 4] = ["mcpServers", "enabledMcpjsonServers", "disabledMcpjsonServers", "disabledMcpServers"];

/// The MCP configuration of one agent in one folder.
pub struct McpConfigWatch {
    kind: AgentKind,
    cwd: PathBuf,
    files: Vec<PathBuf>,
    /// The time of the last change of each file, or `None` if the file is not there.
    stamps: Vec<Option<SystemTime>>,
    /// The parts of the files that set the MCP servers.
    fingerprint: String,
    /// The names of the MCP servers, sorted.
    servers: Vec<String>,
}

impl McpConfigWatch {
    pub fn new(kind: AgentKind, cwd: PathBuf) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        Self::with_home(kind, cwd, &home)
    }

    fn with_home(kind: AgentKind, cwd: PathBuf, home: &Path) -> Self {
        let files = config_files(kind, &cwd, home);
        let stamps = files.iter().map(|f| modified(f)).collect();
        let (fingerprint, servers) = read(kind, &cwd, &files);
        Self { kind, cwd, files, stamps, fingerprint, servers }
    }

    /// The watch is for this agent and this folder.
    pub fn is_for(&self, kind: AgentKind, cwd: &Path) -> bool {
        self.kind == kind && self.cwd == cwd
    }

    /// Read the files again if they changed. If the MCP servers changed, give the names of the
    /// new servers. The list is empty when the change added no server.
    pub fn poll(&mut self) -> Option<Vec<String>> {
        let stamps: Vec<_> = self.files.iter().map(|f| modified(f)).collect();
        if stamps == self.stamps {
            return None;
        }
        self.stamps = stamps;
        let (fingerprint, servers) = read(self.kind, &self.cwd, &self.files);
        if fingerprint == self.fingerprint {
            return None;
        }
        let added = servers.iter().filter(|s| !self.servers.contains(s)).cloned().collect();
        self.fingerprint = fingerprint;
        self.servers = servers;
        Some(added)
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The files of the agent, as `read` expects them: for Claude Code, `.claude.json` comes first.
fn config_files(kind: AgentKind, cwd: &Path, home: &Path) -> Vec<PathBuf> {
    match kind {
        AgentKind::Claude => {
            let (state, settings) = match std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from) {
                Some(dir) => (dir.join(".claude.json"), dir.join("settings.json")),
                None => (home.join(".claude.json"), home.join(".claude/settings.json")),
            };
            vec![
                state,
                settings,
                cwd.join(".mcp.json"),
                cwd.join(".claude/settings.json"),
                cwd.join(".claude/settings.local.json"),
            ]
        }
        AgentKind::Codex => {
            let codex_home = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex"));
            vec![codex_home.join("config.toml"), cwd.join(".codex/config.toml")]
        }
    }
}

/// The fingerprint and the server names of the MCP configuration in `files`.
fn read(kind: AgentKind, cwd: &Path, files: &[PathBuf]) -> (String, Vec<String>) {
    let mut fingerprint = String::new();
    let mut servers = Vec::new();
    for (ix, file) in files.iter().enumerate() {
        let Ok(text) = std::fs::read_to_string(file) else {
            fingerprint.push('\0');
            continue;
        };
        match kind {
            AgentKind::Claude if ix == 0 => {
                let state: Value = serde_json::from_str(&text).unwrap_or_default();
                let project = state.get("projects").and_then(|p| p.get(cwd.to_string_lossy().as_ref()));
                for scope in [Some(&state), project].into_iter().flatten() {
                    for key in CLAUDE_KEYS {
                        fingerprint.push_str(&scope.get(key).map(Value::to_string).unwrap_or_default());
                        fingerprint.push('\0');
                    }
                    servers.extend(json_servers(scope));
                }
            }
            AgentKind::Claude => {
                fingerprint.push_str(&text);
                fingerprint.push('\0');
                servers.extend(json_servers(&serde_json::from_str(&text).unwrap_or_default()));
            }
            AgentKind::Codex => {
                fingerprint.push_str(&text);
                fingerprint.push('\0');
                servers.extend(toml_servers(&text));
            }
        }
    }
    servers.sort();
    servers.dedup();
    (fingerprint, servers)
}

/// The names in the `mcpServers` object of a Claude Code file.
fn json_servers(value: &Value) -> Vec<String> {
    value
        .get("mcpServers")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// The names of the `[mcp_servers.NAME]` tables of a Codex `config.toml`.
fn toml_servers(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("[mcp_servers.")?;
            let name = match rest.strip_prefix('"') {
                Some(quoted) => quoted.split('"').next()?,
                None => rest.split(['.', ']']).next()?,
            };
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("help-mcp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("project")).unwrap();
        dir
    }

    /// Write a file with a new change time, so that the watch reads it again also when two
    /// writes come in the same clock tick.
    fn write(path: &Path, text: &str) {
        static TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        let tick = TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + tick);
        std::fs::File::options().write(true).open(path).unwrap().set_modified(time).unwrap();
    }

    #[test]
    fn a_new_claude_server_shows_and_other_changes_do_not() {
        let dir = temp_dir("claude");
        let cwd = dir.join("project");
        let state = dir.join(".claude.json");
        write(&state, r#"{"numStartups":1,"mcpServers":{"posthog":{}}}"#);
        let mut watch = McpConfigWatch::with_home(AgentKind::Claude, cwd.clone(), &dir);
        assert_eq!(watch.poll(), None);

        // Claude Code writes other keys after each session. They do not change the tools.
        write(&state, r#"{"numStartups":2,"mcpServers":{"posthog":{}}}"#);
        assert_eq!(watch.poll(), None);

        write(&state, r#"{"numStartups":2,"mcpServers":{"posthog":{},"mail":{}}}"#);
        assert_eq!(watch.poll(), Some(vec!["mail".to_string()]));

        // A server of the project counts too, and a removal is a change with no new server.
        write(&cwd.join(".mcp.json"), r#"{"mcpServers":{"linear":{}}}"#);
        assert_eq!(watch.poll(), Some(vec!["linear".to_string()]));
        std::fs::remove_file(cwd.join(".mcp.json")).unwrap();
        assert_eq!(watch.poll(), Some(vec![]));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn codex_servers_come_from_the_toml_tables() {
        let text = "model = \"x\"\n[mcp_servers.mail]\ncommand = \"m\"\n[mcp_servers.mail.tools.send]\n\
                    [mcp_servers.\"a.b\"]\n[profiles.mcp_servers.no]\n";
        assert_eq!(toml_servers(text), vec!["mail", "mail", "a.b"]);
    }
}
