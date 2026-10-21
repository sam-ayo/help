//! MCP Apps (the MCP extension `io.modelcontextprotocol/ui`). A tool of an MCP server can have
//! a view: an HTML resource that the app shows with the result of the tool. The agent does not
//! send the views, so the app connects to the same servers to get them.
//!
//! Call the methods of [`Apps`] in a Tokio runtime.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use acp_host::{AgentKind, McpServerSpec};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use rmcp::Peer;
use rmcp::RoleClient;
use rmcp::model::{CallToolRequestParams, Implementation, ReadResourceRequestParams};
use serde_json::{Map, Value};
use tokio::sync::Mutex;

mod connect;
mod csp;
mod list;
mod logo;

pub use csp::csp_header;
pub use logo::Logo;

/// The version of the MCP Apps specification.
pub const PROTOCOL_VERSION: &str = "2026-01-26";
/// The MIME type of a view.
pub const MIME_TYPE: &str = "text/html;profile=mcp-app";
/// The name of the extension in the MCP capabilities.
pub const EXTENSION: &str = "io.modelcontextprotocol/ui";

/// After a server fails to connect, the app tries again only after this time.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// The UI of one tool, ready to show.
#[derive(Clone, Debug)]
pub struct AppUi {
    /// The name of the server in the configuration.
    pub server: String,
    /// The full Tool object as JSON (name, description, inputSchema, _meta, ...).
    pub tool: Value,
    /// The `ui://` URI of the resource.
    pub uri: String,
    /// The HTML of the view.
    pub html: String,
    /// The `_meta.ui.csp` of the resource.
    pub csp: Option<Value>,
    /// The `_meta.ui.permissions` of the resource.
    pub permissions: Option<Value>,
    pub prefers_border: Option<bool>,
}

/// An MCP server that the agent has connected.
#[derive(Clone, Debug)]
pub struct AppServer {
    /// The name of the server, as the agent gives it.
    pub server: String,
    /// The title that the server gives, or its name.
    pub title: String,
    /// The logo for a light background.
    pub logo: Option<Logo>,
    /// The logo for a dark background.
    pub logo_dark: Option<Logo>,
}

/// The MCP Apps of the servers of one agent in one folder. A clone shares the connections.
#[derive(Clone)]
pub struct Apps {
    inner: Arc<Inner>,
}

struct Inner {
    kind: AgentKind,
    cwd: PathBuf,
    /// The servers of the configuration. The app reads the files on the first use.
    specs: OnceLock<Vec<McpServerSpec>>,
    servers: StdMutex<HashMap<String, Arc<Server>>>,
    /// The views, by server name and URI.
    views: StdMutex<HashMap<(String, String), Arc<View>>>,
    /// The logos of each server for a light and a dark background. The app gets them one time.
    logos: StdMutex<HashMap<String, Arc<tokio::sync::OnceCell<(Option<Logo>, Option<Logo>)>>>>,
}

struct Server {
    spec: McpServerSpec,
    /// The lock also makes sure that the app starts one connection at a time.
    state: Mutex<State>,
}

enum State {
    New,
    /// The server has no tool with a view, so the app closed the connection.
    NoUi { info: Implementation },
    Failed(Instant),
    Ready { client: connect::Client, tools: Arc<Vec<Value>>, info: Implementation },
}

/// The parts of a resource that a view needs.
struct View {
    html: String,
    csp: Option<Value>,
    permissions: Option<Value>,
    prefers_border: Option<bool>,
}

impl Apps {
    pub fn new(kind: AgentKind, cwd: PathBuf) -> Self {
        let inner = Inner {
            kind,
            cwd,
            specs: OnceLock::new(),
            servers: StdMutex::default(),
            views: StdMutex::default(),
            logos: StdMutex::default(),
        };
        Self { inner: Arc::new(inner) }
    }

    /// The view of a tool, or `None` if the tool has no view or the server cannot connect.
    /// `server` and `tool` are the names that the agent gives. Claude Code changes each
    /// character that is not `[A-Za-z0-9_-]` to `_`, so the names can also match after that change.
    pub async fn find(&self, server: &str, tool: &str) -> Option<AppUi> {
        let server = self.server(server)?;
        let (peer, tools) = match self.session(&server).await {
            Ok(session) => session,
            Err(err) => {
                tracing::debug!("no MCP Apps for {}: {err:#}", server.spec.name);
                return None;
            }
        };
        let tool = match find_tool(&tools, tool) {
            Some(tool) => tool.clone(),
            // The server can add tools after it starts.
            None => find_tool(&self.refresh_tools(&server).await?, tool)?.clone(),
        };
        let uri = resource_uri(&tool)?.to_string();
        let view = match self.view(&server, &peer, &uri).await {
            Ok(view) => view,
            Err(err) => {
                tracing::warn!("cannot read {uri} of {}: {err:#}", server.spec.name);
                return None;
            }
        };
        Some(AppUi {
            server: server.spec.name.clone(),
            tool,
            uri,
            html: view.html.clone(),
            csp: view.csp.clone(),
            permissions: view.permissions.clone(),
            prefers_border: view.prefers_border,
        })
    }

    /// The MCP servers that the agent has connected, with their logos, sorted by title. For
    /// Claude Code, `claude mcp list` tells which servers connect. For Codex, the list has the
    /// servers of the configuration.
    pub async fn app_servers(&self) -> Vec<AppServer> {
        let listed = match self.inner.kind {
            AgentKind::Claude => match list::claude(&self.inner.cwd).await {
                Ok(listed) => listed.into_iter().filter(|l| l.connected).collect(),
                Err(err) => {
                    tracing::warn!("cannot list the MCP servers of Claude Code: {err:#}");
                    self.configured()
                }
            },
            AgentKind::Codex => self.configured(),
        };
        let mut tasks = tokio::task::JoinSet::new();
        for listed in listed {
            let this = self.clone();
            tasks.spawn(async move { this.app_server(listed).await });
        }
        let mut found = Vec::new();
        while let Some(Ok(server)) = tasks.join_next().await {
            found.push(server);
        }
        found.sort_by_key(|s| s.title.to_lowercase());
        found
    }

    /// The servers of the configuration files.
    fn configured(&self) -> Vec<list::Listed> {
        self.specs()
            .iter()
            .map(|spec| list::Listed {
                name: spec.name.clone(),
                target: match &spec.transport {
                    acp_host::McpTransport::Http { url, .. } => url.clone(),
                    acp_host::McpTransport::Stdio { command, .. } => command.clone(),
                },
                connected: true,
            })
            .collect()
    }

    /// The title and the logos of one server. If the server is in the configuration files, the
    /// app connects to it to get its icons. Else the logo is the icon of its website.
    async fn app_server(&self, listed: list::Listed) -> AppServer {
        let mut info = None;
        if let Some(server) = self.server(&listed.name) {
            let _ = self.session(&server).await;
            info = match &*server.state.lock().await {
                State::Ready { info, .. } | State::NoUi { info } => Some(info.clone()),
                _ => None,
            };
        }
        let key = listed.name.clone();
        let cell = self.inner.logos.lock().unwrap().entry(key).or_default().clone();
        let (logo, logo_dark) = cell.get_or_init(|| logo::logos(info.as_ref(), Some(&listed.target))).await.clone();
        let title = info
            .and_then(|i| i.title)
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| display_name(&listed.name));
        AppServer { server: listed.name, title, logo, logo_dark }
    }

    /// Call a tool for a view. The tool must be visible to apps. `server` is the name in [`AppUi`].
    /// The result is the full `CallToolResult`.
    pub async fn call_tool(&self, server: &str, name: &str, arguments: Option<Map<String, Value>>) -> Result<Value> {
        let (peer, tools) = self.connected(server).await?;
        let tool = find_tool(&tools, name).with_context(|| format!("{server} has no tool {name}"))?;
        if !visible_to_app(tool) {
            bail!("the tool {name} is not visible to apps");
        }
        let name = tool["name"].as_str().unwrap_or(name).to_string();
        let mut params = CallToolRequestParams::new(name);
        if let Some(arguments) = arguments {
            params = params.with_arguments(arguments);
        }
        Ok(serde_json::to_value(peer.call_tool(params).await?)?)
    }

    /// Read a resource for a view. The result is the full `ReadResourceResult`.
    pub async fn read_resource(&self, server: &str, uri: &str) -> Result<Value> {
        let (peer, _) = self.connected(server).await?;
        Ok(serde_json::to_value(peer.read_resource(ReadResourceRequestParams::new(uri)).await?)?)
    }

    /// All the resources of the server, as a `ListResourcesResult`.
    pub async fn list_resources(&self, server: &str) -> Result<Value> {
        let (peer, _) = self.connected(server).await?;
        Ok(serde_json::json!({ "resources": peer.list_all_resources().await? }))
    }

    /// All the resource templates of the server, as a `ListResourceTemplatesResult`.
    pub async fn list_resource_templates(&self, server: &str) -> Result<Value> {
        let (peer, _) = self.connected(server).await?;
        Ok(serde_json::json!({ "resourceTemplates": peer.list_all_resource_templates().await? }))
    }

    /// The server with this name. The exact name comes first, then the changed name.
    fn server(&self, name: &str) -> Option<Arc<Server>> {
        let specs = self.specs();
        let spec = specs
            .iter()
            .find(|s| s.name == name)
            .or_else(|| specs.iter().find(|s| normalize(&s.name) == name))?;
        let mut servers = self.inner.servers.lock().unwrap();
        let server = servers
            .entry(spec.name.clone())
            .or_insert_with(|| Arc::new(Server { spec: spec.clone(), state: Mutex::new(State::New) }));
        Some(server.clone())
    }

    /// The servers of the configuration. The app reads the files on the first use.
    fn specs(&self) -> &Vec<McpServerSpec> {
        self.inner.specs.get_or_init(|| acp_host::mcp_servers(self.inner.kind, &self.inner.cwd))
    }

    async fn connected(&self, server: &str) -> Result<(Peer<RoleClient>, Arc<Vec<Value>>)> {
        let server = self.server(server).ok_or_else(|| anyhow!("no MCP server {server}"))?;
        self.session(&server).await
    }

    /// The connection to the server and its tools. The first call connects. If the server has no
    /// tool with a view, the app closes the connection and remembers that.
    async fn session(&self, server: &Server) -> Result<(Peer<RoleClient>, Arc<Vec<Value>>)> {
        let mut state = server.state.lock().await;
        match &*state {
            State::Ready { client, tools, .. } => return Ok((client.peer().clone(), tools.clone())),
            State::NoUi { .. } => bail!("the server has no tool with a view"),
            State::Failed(at) if at.elapsed() < RETRY_AFTER => bail!("the server failed to connect"),
            State::New | State::Failed(_) => {}
        }
        let client = match connect::connect(&server.spec, &self.inner.cwd).await {
            Ok(client) => client,
            Err(err) => {
                tracing::warn!("cannot connect to the MCP server {}: {err:#}", server.spec.name);
                *state = State::Failed(Instant::now());
                return Err(err);
            }
        };
        let tools = match list_tools(&client).await {
            Ok(tools) => tools,
            Err(err) => {
                *state = State::Failed(Instant::now());
                return Err(err);
            }
        };
        let info = client
            .peer_info()
            .and_then(|i| i.server_info.clone())
            .unwrap_or_else(|| Implementation::new(&server.spec.name, ""));
        if !tools.iter().any(|t| resource_uri(t).is_some()) {
            *state = State::NoUi { info };
            drop(state);
            let _ = client.cancel().await;
            bail!("the server has no tool with a view");
        }
        let peer = client.peer().clone();
        let tools = Arc::new(tools);
        *state = State::Ready { client, tools: tools.clone(), info };
        Ok((peer, tools))
    }

    /// Get the tools of a connected server again.
    async fn refresh_tools(&self, server: &Server) -> Option<Arc<Vec<Value>>> {
        let mut state = server.state.lock().await;
        let State::Ready { client, tools, .. } = &mut *state else { return None };
        *tools = Arc::new(list_tools(client).await.ok()?);
        Some(tools.clone())
    }

    /// The view at `uri`. The app reads each view one time.
    async fn view(&self, server: &Server, peer: &Peer<RoleClient>, uri: &str) -> Result<Arc<View>> {
        let key = (server.spec.name.clone(), uri.to_string());
        if let Some(view) = self.inner.views.lock().unwrap().get(&key) {
            return Ok(view.clone());
        }
        let result = serde_json::to_value(peer.read_resource(ReadResourceRequestParams::new(uri)).await?)?;
        let contents = result["contents"].as_array().cloned().unwrap_or_default();
        let item = contents
            .iter()
            .find(|c| c["uri"] == uri)
            .or_else(|| contents.first())
            .context("the resource has no contents")?;
        let html = match (item["text"].as_str(), item["blob"].as_str()) {
            (Some(text), _) => text.to_string(),
            (None, Some(blob)) => {
                let bytes = base64::engine::general_purpose::STANDARD.decode(blob.trim())?;
                String::from_utf8_lossy(&bytes).into_owned()
            }
            (None, None) => bail!("the resource has no text and no blob"),
        };
        if let Some(mime) = item["mimeType"].as_str().filter(|m| !m.starts_with("text/html")) {
            tracing::warn!("the resource {uri} has the MIME type {mime}, not {MIME_TYPE}");
        }

        // The `_meta.ui` of the content comes first, then the one of the resource list.
        let mut ui = item["_meta"]["ui"].as_object().cloned().unwrap_or_default();
        if ["csp", "permissions", "prefersBorder"].iter().any(|k| !ui.contains_key(*k))
            && let Ok(resources) = peer.list_all_resources().await
            && let Ok(resources) = serde_json::to_value(resources)
            && let Some(listed) = resources
                .as_array()
                .and_then(|list| list.iter().find(|r| r["uri"] == uri))
                .and_then(|r| r["_meta"]["ui"].as_object())
        {
            for (key, value) in listed {
                ui.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        let view = Arc::new(View {
            html,
            csp: ui.get("csp").cloned(),
            permissions: ui.get("permissions").cloned(),
            prefers_border: ui.get("prefersBorder").and_then(Value::as_bool),
        });
        self.inner.views.lock().unwrap().insert(key, view.clone());
        Ok(view)
    }
}

async fn list_tools(client: &connect::Client) -> Result<Vec<Value>> {
    let tools = client.list_all_tools().await?;
    tools.iter().map(|t| Ok(serde_json::to_value(t)?)).collect()
}

/// The tool with this name. The exact name comes first, then the changed name.
fn find_tool<'a>(tools: &'a [Value], name: &str) -> Option<&'a Value> {
    tools
        .iter()
        .find(|t| t["name"] == name)
        .or_else(|| tools.iter().find(|t| t["name"].as_str().is_some_and(|n| normalize(n) == name)))
}

/// The `ui://` URI of the view of a tool. The flat key `ui/resourceUri` is the old form.
fn resource_uri(tool: &Value) -> Option<&str> {
    let meta = &tool["_meta"];
    meta["ui"]["resourceUri"]
        .as_str()
        .or_else(|| meta["ui/resourceUri"].as_str())
        .filter(|uri| uri.starts_with("ui://"))
}

/// A view can call a tool only when its `_meta.ui.visibility` contains `app`. With no
/// visibility, the tool is visible to the model and to apps.
fn visible_to_app(tool: &Value) -> bool {
    match tool["_meta"]["ui"]["visibility"].as_array() {
        Some(list) => list.iter().any(|v| v == "app"),
        None => true,
    }
}

/// A short name for a server: `claude.ai Linear` is `Linear`, and `plugin:figma:figma` is `figma`.
fn display_name(name: &str) -> String {
    let name = name.strip_prefix("claude.ai ").unwrap_or(name);
    match name.strip_prefix("plugin:") {
        Some(rest) => rest.rsplit(':').next().unwrap_or(rest).to_string(),
        None => name.to_string(),
    }
}

/// The name as Claude Code writes it in `mcp__<server>__<tool>`.
fn normalize(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_uri_comes_from_the_new_or_the_old_key() {
        assert_eq!(resource_uri(&json!({"_meta": {"ui": {"resourceUri": "ui://a"}}})), Some("ui://a"));
        assert_eq!(resource_uri(&json!({"_meta": {"ui/resourceUri": "ui://b"}})), Some("ui://b"));
        assert_eq!(resource_uri(&json!({"_meta": {"ui": {"resourceUri": "https://c"}}})), None);
        assert_eq!(resource_uri(&json!({"name": "x"})), None);
    }

    #[test]
    fn visibility_defaults_to_the_model_and_apps() {
        assert!(visible_to_app(&json!({"name": "x"})));
        assert!(visible_to_app(&json!({"_meta": {"ui": {"visibility": ["app"]}}})));
        assert!(!visible_to_app(&json!({"_meta": {"ui": {"visibility": ["model"]}}})));
    }

    #[test]
    fn display_names_are_short() {
        assert_eq!(display_name("claude.ai Linear"), "Linear");
        assert_eq!(display_name("plugin:revenuecat:RevenueCat"), "RevenueCat");
        assert_eq!(display_name("neon"), "neon");
    }

    #[test]
    fn names_match_exactly_first_then_after_the_change() {
        assert_eq!(normalize("my server.v2"), "my_server_v2");
        let tools = vec![json!({"name": "get.time"}), json!({"name": "get_time"})];
        assert_eq!(find_tool(&tools, "get_time"), Some(&tools[1]));
        assert_eq!(find_tool(&tools[..1], "get_time"), Some(&tools[0]));
        assert_eq!(find_tool(&tools, "other"), None);
    }
}
