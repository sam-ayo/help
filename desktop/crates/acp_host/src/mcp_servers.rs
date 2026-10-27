//! The MCP servers that an agent uses, with the data to connect to them. The app connects to
//! the same servers to get the views of MCP Apps, because the agent does not send them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::AgentKind;
use crate::mcp_config::config_files;

/// One MCP server of the agent configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerSpec {
    /// The name of the server in the configuration.
    pub name: String,
    pub transport: McpTransport,
}

/// How to connect to an MCP server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpTransport {
    /// Start a process, and send the messages on its standard input and output.
    Stdio { command: String, args: Vec<String>, env: Vec<(String, String)>, cwd: Option<PathBuf> },
    /// Send the messages to a URL with streamable HTTP.
    Http { url: String, headers: Vec<(String, String)> },
}

/// The MCP servers that the agent uses in `cwd`, sorted by name. When two files set the same
/// name, the server of the file with the higher priority replaces the other.
pub fn mcp_servers(kind: AgentKind, cwd: &Path) -> Vec<McpServerSpec> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    with_home(kind, cwd, &home)
}

fn with_home(kind: AgentKind, cwd: &Path, home: &Path) -> Vec<McpServerSpec> {
    let files = config_files(kind, cwd, home);
    let mut servers = BTreeMap::new();
    match kind {
        AgentKind::Claude => {
            // The priority goes up: user, then project, then local.
            let state = read_json(&files[0]);
            let project = files.iter().find(|f| f.ends_with(".mcp.json")).map(|f| read_json(f));
            let local = state.get("projects").and_then(|p| p.get(cwd.to_string_lossy().as_ref()));
            for scope in [Some(&state), project.as_ref(), local].into_iter().flatten() {
                let Some(entries) = scope.get("mcpServers").and_then(Value::as_object) else { continue };
                for (name, entry) in entries {
                    if let Some(transport) = claude_transport(entry) {
                        servers.insert(name.clone(), transport);
                    }
                }
            }
        }
        AgentKind::Codex => {
            // The file of the project comes last, so it has the higher priority.
            for file in &files {
                let Ok(text) = std::fs::read_to_string(file) else { continue };
                let table: toml::Table = match toml::from_str(&text) {
                    Ok(table) => table,
                    Err(err) => {
                        tracing::warn!("cannot read {}: {err}", file.display());
                        continue;
                    }
                };
                let Some(entries) = table.get("mcp_servers").and_then(toml::Value::as_table) else { continue };
                for (name, entry) in entries {
                    if let Some(transport) = entry.as_table().and_then(codex_transport) {
                        servers.insert(name.clone(), transport);
                    }
                }
            }
        }
    }
    servers.into_iter().map(|(name, transport)| McpServerSpec { name, transport }).collect()
}

fn read_json(path: &Path) -> Value {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// The transport of one Claude Code entry. The types `sse` and the unknown types give `None`.
fn claude_transport(entry: &Value) -> Option<McpTransport> {
    let string = |key: &str| entry.get(key).and_then(Value::as_str).map(expand);
    let pairs = |key: &str| -> Vec<(String, String)> {
        let Some(map) = entry.get(key).and_then(Value::as_object) else { return Vec::new() };
        map.iter().filter_map(|(k, v)| Some((k.clone(), expand(v.as_str()?)))).collect()
    };
    match entry.get("type").and_then(Value::as_str) {
        None | Some("stdio") => Some(McpTransport::Stdio {
            command: string("command")?,
            args: entry
                .get("args")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(expand).collect())
                .unwrap_or_default(),
            env: pairs("env"),
            cwd: None,
        }),
        Some("http") => Some(McpTransport::Http { url: string("url")?, headers: pairs("headers") }),
        Some(_) => None,
    }
}

/// The transport of one Codex `[mcp_servers.NAME]` table. A server with `enabled = false`
/// gives `None`.
fn codex_transport(entry: &toml::Table) -> Option<McpTransport> {
    if entry.get("enabled").and_then(toml::Value::as_bool) == Some(false) {
        return None;
    }
    let string = |key: &str| entry.get(key).and_then(toml::Value::as_str).map(str::to_string);
    let pairs = |key: &str| -> Vec<(String, String)> {
        let Some(table) = entry.get(key).and_then(toml::Value::as_table) else { return Vec::new() };
        table.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect()
    };
    if let Some(command) = string("command") {
        let args = entry
            .get("args")
            .and_then(toml::Value::as_array)
            .map(|a| a.iter().filter_map(toml::Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        return Some(McpTransport::Stdio { command, args, env: pairs("env"), cwd: string("cwd").map(PathBuf::from) });
    }
    let url = string("url")?;
    let mut headers = pairs("http_headers");
    // These headers get their values from environment variables.
    for (header, var) in pairs("env_http_headers") {
        if let Ok(value) = std::env::var(&var) {
            headers.push((header, value));
        }
    }
    if let Some(token) = string("bearer_token_env_var").and_then(|var| std::env::var(var).ok()) {
        headers.push(("Authorization".to_string(), format!("Bearer {token}")));
    }
    Some(McpTransport::Http { url, headers })
}

/// Replace `${VAR}` and `${VAR:-default}` with the value of the environment variable. If the
/// variable is not set and there is no default, the text stays the same, as in Claude Code.
fn expand(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            rest = &rest[start..];
            break;
        };
        let inner = &after[..end];
        let (var, default) = match inner.split_once(":-") {
            Some((var, default)) => (var, Some(default)),
            None => (inner, None),
        };
        match (std::env::var(var), default) {
            (Ok(value), _) => out.push_str(&value),
            (Err(_), Some(default)) => out.push_str(default),
            (Err(_), None) => out.push_str(&rest[start..start + 3 + end]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("help-mcp-servers-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("project")).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn stdio(command: &str, args: &[&str], env: &[(&str, &str)]) -> McpTransport {
        McpTransport::Stdio {
            command: command.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
            env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            cwd: None,
        }
    }

    #[test]
    fn claude_scopes_follow_the_priority() {
        let dir = temp_dir("claude");
        let cwd = dir.join("project");
        let state = serde_json::json!({
            "mcpServers": {
                "user": {"command": "u"},
                "both": {"command": "from-user"},
                "old": {"type": "sse", "url": "http://x"},
                "odd": {"type": "ws", "url": "ws://x"},
            },
            "projects": {
                cwd.to_string_lossy(): {"mcpServers": {"both": {"type": "stdio", "command": "from-local"}}},
            },
        });
        write(&dir.join(".claude.json"), &state.to_string());
        write(
            &cwd.join(".mcp.json"),
            r#"{"mcpServers":{
                "both":{"command":"from-project"},
                "web":{"type":"http","url":"https://a/${HELP_TEST_NO_VAR:-mcp}","headers":{"X":"${HELP_TEST_NO_VAR}"}},
                "tool":{"command":"t","args":["-y","${HELP_TEST_NO_VAR:-}x"],"env":{"K":"v"}}
            }}"#,
        );
        let servers = with_home(AgentKind::Claude, &cwd, &dir);
        let names: Vec<_> = servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["both", "tool", "user", "web"]);
        assert_eq!(servers[0].transport, stdio("from-local", &[], &[]));
        assert_eq!(servers[1].transport, stdio("t", &["-y", "x"], &[("K", "v")]));
        assert_eq!(
            servers[3].transport,
            McpTransport::Http {
                url: "https://a/mcp".to_string(),
                headers: vec![("X".to_string(), "${HELP_TEST_NO_VAR}".to_string())],
            }
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn codex_project_file_wins() {
        let dir = temp_dir("codex");
        let cwd = dir.join("project");
        write(
            &dir.join(".codex/config.toml"),
            "model = \"x\"\n\
             [mcp_servers.mail]\ncommand = \"m\"\nargs = [\"a\"]\ncwd = \"/tmp\"\n[mcp_servers.mail.env]\nK = \"v\"\n\
             [mcp_servers.web]\nurl = \"https://w\"\nbearer_token_env_var = \"HELP_TEST_NO_VAR\"\n\
             [mcp_servers.web.http_headers]\nX = \"y\"\n\
             [mcp_servers.off]\ncommand = \"o\"\nenabled = false\n",
        );
        write(&cwd.join(".codex/config.toml"), "[mcp_servers.mail]\ncommand = \"project\"\n");
        let servers = with_home(AgentKind::Codex, &cwd, &dir);
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].name, "mail");
        assert_eq!(servers[0].transport, stdio("project", &[], &[]));
        assert_eq!(
            servers[1].transport,
            McpTransport::Http { url: "https://w".to_string(), headers: vec![("X".to_string(), "y".to_string())] }
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn codex_reads_all_stdio_keys() {
        let table: toml::Table =
            toml::from_str("command = \"m\"\nargs = [\"a\"]\ncwd = \"/tmp\"\n[env]\nK = \"v\"\n").unwrap();
        assert_eq!(
            codex_transport(&table),
            Some(McpTransport::Stdio {
                command: "m".to_string(),
                args: vec!["a".to_string()],
                env: vec![("K".to_string(), "v".to_string())],
                cwd: Some(PathBuf::from("/tmp")),
            })
        );
    }

    #[test]
    fn expand_replaces_variables() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand("${HOME}/a"), format!("{home}/a"));
        assert_eq!(expand("a${HELP_TEST_NO_VAR:-b}c"), "abc");
        assert_eq!(expand("${HELP_TEST_NO_VAR}"), "${HELP_TEST_NO_VAR}");
        assert_eq!(expand("x${open"), "x${open");
        assert_eq!(expand("$HOME"), "$HOME");
    }
}
