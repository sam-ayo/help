//! The connection of the app to one MCP server.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use acp_host::{McpServerSpec, McpTransport};
use anyhow::{Context, Result};
use rmcp::model::{ClientCapabilities, ClientConfig, Implementation, JsonObject};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use tokio::io::AsyncBufReadExt;
use tokio::sync::OnceCell;

use crate::{EXTENSION, MIME_TYPE};

/// The maximum time to start a server and complete the MCP initialize.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) type Client = RunningService<RoleClient, ClientConfig>;

/// Connect to the server, and complete the MCP initialize.
pub(crate) async fn connect(spec: &McpServerSpec, cwd: &Path) -> Result<Client> {
    let config = client_config();
    let connect = async {
        match &spec.transport {
            McpTransport::Stdio { command, args, env, cwd: dir } => {
                let dir = dir.as_deref().unwrap_or(cwd);
                let command = stdio_command(command, args, env, dir).await;
                let (process, stderr) = TokioChildProcess::builder(command).stderr(Stdio::piped()).spawn()?;
                if let Some(stderr) = stderr {
                    let name = spec.name.clone();
                    tokio::spawn(async move {
                        let mut lines = tokio::io::BufReader::new(stderr).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            tracing::debug!(server = %name, "{line}");
                        }
                    });
                }
                Ok(config.serve(process).await?)
            }
            McpTransport::Http { url, headers } => {
                let mut map = HashMap::new();
                for (name, value) in headers {
                    match (http::HeaderName::try_from(name.as_str()), http::HeaderValue::try_from(value.as_str())) {
                        (Ok(name), Ok(value)) => _ = map.insert(name, value),
                        _ => tracing::warn!(server = %spec.name, "skip the bad header {name}"),
                    }
                }
                let transport = StreamableHttpClientTransport::from_config(
                    StreamableHttpClientTransportConfig::with_uri(url.as_str()).custom_headers(map),
                );
                Ok(config.serve(transport).await?)
            }
        }
    };
    tokio::time::timeout(CONNECT_TIMEOUT, connect)
        .await
        .with_context(|| format!("{} did not start in {} s", spec.name, CONNECT_TIMEOUT.as_secs()))?
}

/// The initialize data of the app. It tells the server that the app shows MCP Apps views.
fn client_config() -> ClientConfig {
    let mut ui = JsonObject::new();
    ui.insert("mimeTypes".to_string(), serde_json::json!([MIME_TYPE]));
    let mut capabilities = ClientCapabilities::default();
    capabilities.extensions = Some(BTreeMap::from([(EXTENSION.to_string(), ui)]));
    ClientConfig::new(capabilities, Implementation::new("help", env!("CARGO_PKG_VERSION")))
}

/// The command for a stdio server. The process gets the environment of the app with `env` on
/// top. An app that starts from the Finder has a short `PATH`, so when the command is not on
/// that `PATH`, the command uses the `PATH` of the login shell.
async fn stdio_command(command: &str, args: &[String], env: &[(String, String)], cwd: &Path) -> tokio::process::Command {
    let env_path = env.iter().rev().find(|(k, _)| k == "PATH").map(|(_, v)| v.clone());
    let mut path_for_child = None;
    let program = if command.contains('/') {
        cwd.join(command)
    } else {
        let own_path = env_path.clone().or_else(|| std::env::var("PATH").ok()).unwrap_or_default();
        match find_in(&own_path, command) {
            Some(found) => found,
            None => match login_path().await.and_then(|path| Some((find_in(path, command)?, path))) {
                Some((found, path)) => {
                    if env_path.is_none() {
                        path_for_child = Some(path.to_string());
                    }
                    found
                }
                None => PathBuf::from(command),
            },
        }
    };
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).kill_on_drop(true);
    if let Some(path) = path_for_child {
        cmd.env("PATH", path);
    }
    cmd.envs(env.iter().map(|(k, v)| (k, v)));
    if cwd.is_dir() {
        cmd.current_dir(cwd);
    }
    cmd
}

/// The path of a program, and the `PATH` of the login shell when the program is only on that
/// `PATH`. An app that starts from the Finder has a short `PATH`.
pub(crate) async fn program(command: &str) -> (PathBuf, Option<&'static str>) {
    if let Some(found) = find_in(&std::env::var("PATH").unwrap_or_default(), command) {
        return (found, None);
    }
    match login_path().await.and_then(|path| Some((find_in(path, command)?, path))) {
        Some((found, path)) => (found, Some(path)),
        None => (PathBuf::from(command), None),
    }
}

/// The first executable file with the name `command` in the folders of `path`.
fn find_in(path: &str, command: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path)
        .map(|dir| dir.join(command))
        .find(|file| std::fs::metadata(file).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// The `PATH` of the login shell of the user. The app reads it one time.
async fn login_path() -> Option<&'static str> {
    static PATH: OnceCell<Option<String>> = OnceCell::const_new();
    PATH.get_or_init(|| async {
        const MARK: &str = "__HELP_PATH__";
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let output = tokio::process::Command::new(&shell)
            .args(["-ilc", &format!("printf '{MARK}%s{MARK}' \"$PATH\"")])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(Duration::from_secs(5), output).await.ok()?.ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let path = text.split(MARK).nth(1)?.to_string();
        tracing::debug!("the login shell PATH is {path}");
        (!path.is_empty()).then_some(path)
    })
    .await
    .as_deref()
}
