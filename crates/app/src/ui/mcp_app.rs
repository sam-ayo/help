//! MCP Apps: a tool of an MCP server can have a view, a web page that shows the result of the
//! tool. The agent owns its MCP connections, and it does not tell the app about views. So the
//! app connects to the same servers (see the `mcp_apps` crate), and shows the view of a tool in
//! a panel at the right side of the window.
//!
//! The view runs in a web view of its own. It sends JSON-RPC messages to `window.parent`, as it
//! does in an iframe. In a web view, `window.parent` is the page, so a script sends these
//! messages to the app, and the app sends its messages as `message` events of the page.

use std::{
    borrow::Cow,
    cell::Cell,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use acp_host::{McpCall, ToolStatus};
use gpui::{Bounds, Pixels, Window};
use gpui_component::{ActiveTheme as _, Theme};
use mcp_apps::{AppServer, AppUi, Apps, Logo};
use serde_json::{Value, json};
use wry::{
    NewWindowResponse, Rect, WebView, WebViewBuilder,
    dpi::{LogicalPosition, LogicalSize},
    http::{Response, header::CONTENT_TYPE},
};

use super::{AppView, Context, demo};

/// The width of the panel.
pub(super) const PANEL: f32 = 420.;
/// The scheme of the address of a view. Each view has its own web view and data store.
const SCHEME: &str = "helpapp";
/// The time that a view gets to clean up before the app removes it.
const TEARDOWN: Duration = Duration::from_millis(500);

/// The script that connects the view to the app. Messages that are not JSON-RPC go to the page
/// as usual, because some libraries send messages to their own page.
const BRIDGE: &str = r#"(() => {
  const own = window.postMessage.bind(window);
  window.postMessage = function (message, ...rest) {
    if (message && typeof message === "object" && message.jsonrpc === "2.0") {
      window.ipc.postMessage(JSON.stringify(message));
    } else {
      own(message, ...rest);
    }
  };
  window.__helpDeliver = (data) =>
    window.dispatchEvent(new MessageEvent("message", { data, source: window, origin: location.origin }));
})();"#;

/// What the app knows about the view of an MCP tool.
#[derive(Clone)]
pub(super) enum Lookup {
    /// The app did not look for a view yet.
    NotStarted,
    Waiting,
    NoView,
    View(Arc<AppUi>),
}

/// A call of an MCP tool, with the data that its view gets.
pub(super) struct McpCallState {
    pub call: McpCall,
    pub input: Option<Value>,
    pub output: Option<Value>,
    pub status: ToolStatus,
    pub lookup: Lookup,
}

impl McpCallState {
    pub fn new(call: McpCall, input: Option<Value>, status: ToolStatus) -> Self {
        Self { call, input, output: None, status, lookup: Lookup::NotStarted }
    }

    pub fn view(&self) -> Option<&Arc<AppUi>> {
        match &self.lookup {
            Lookup::View(ui) => Some(ui),
            _ => None,
        }
    }

    /// The tool has no view. Only a view uses the input and the result, and a result can be
    /// large, for example a screenshot. So the app does not keep them.
    pub fn set_no_view(&mut self) {
        self.lookup = Lookup::NoView;
        self.input = None;
        self.output = None;
    }

    /// Keep the new input and result of the tool, if a view can use them.
    pub fn update(&mut self, input: Option<Value>, output: Option<Value>) {
        if matches!(self.lookup, Lookup::NoView) {
            return;
        }
        if input.is_some() {
            self.input = input;
        }
        if output.is_some() {
            self.output = output;
        }
    }
}

/// A connected MCP server that has tools with views, as the sidebar shows it.
pub(super) struct AppEntry {
    pub server: String,
    pub title: String,
    logo: Option<Arc<gpui::Image>>,
    logo_dark: Option<Arc<gpui::Image>>,
}

impl AppEntry {
    fn new(server: AppServer) -> Self {
        let image = |logo: Option<Logo>| {
            let logo = logo?;
            // gpui knows an icon file as `image/ico`.
            let format = gpui::ImageFormat::from_mime_type(logo.mime)
                .or_else(|| (logo.mime == "image/x-icon").then_some(gpui::ImageFormat::Ico))?;
            Some(Arc::new(gpui::Image::from_bytes(format, logo.bytes)))
        };
        Self { logo: image(server.logo), logo_dark: image(server.logo_dark), server: server.server, title: server.title }
    }

    /// A server with no logo, for the sample data of the demo.
    pub fn named(server: &str, title: &str) -> Self {
        Self { server: server.into(), title: title.into(), logo: None, logo_dark: None }
    }

    /// The logo for the theme, if the server has one.
    pub fn logo(&self, dark: bool) -> Option<Arc<gpui::Image>> {
        if dark { self.logo_dark.clone() } else { self.logo.clone() }
    }
}

/// The place of the view in the window, after the app renders the panel.
pub(super) type PanelBounds = Rc<Cell<Option<Bounds<Pixels>>>>;

/// The view that the panel shows.
pub(super) struct Panel {
    /// The ID of the tool call that the view shows.
    pub tool_id: String,
    pub ui: Arc<AppUi>,
    /// The app makes the web view when it renders the panel the first time, because the web view
    /// needs the window.
    webview: Option<Rc<WebView>>,
    /// Each panel gets a new number. The messages of an older view do not apply.
    generation: u64,
    /// The view sent `ui/notifications/initialized`. The app sends nothing before that.
    initialized: bool,
    sent_input: bool,
    /// The view got the result of the tool, or the news that the tool did not complete.
    sent_result: bool,
    /// The view got this theme.
    dark: bool,
    /// The place of the view in the window.
    bounds: PanelBounds,
    next_request: u64,
}

impl Panel {
    fn send(&self, message: &Value) {
        if let Some(webview) = &self.webview
            && let Err(e) = webview.evaluate_script(&format!("window.__helpDeliver({message})"))
        {
            tracing::warn!("cannot send a message to the MCP App view: {e}");
        }
    }

    fn notify(&self, method: &str, params: Value) {
        tracing::debug!("the app sent {method} to the MCP App view");
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn reply(&self, id: Value, result: Result<Value, String>) {
        if let Err(e) = &result {
            tracing::debug!("the app sent an error to the MCP App view: {e}");
        }
        self.send(&match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32603, "message": message } }),
        });
    }
}

/// The text of the content blocks of a message from a view.
fn content_text(content: &Value) -> String {
    let blocks = match content {
        Value::Array(blocks) => blocks.iter().collect(),
        block => vec![block],
    };
    blocks
        .into_iter()
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn hex(color: gpui::Hsla) -> String {
    let c = color.to_rgb();
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}", byte(c.r), byte(c.g), byte(c.b))
}

/// The theme and the size of the panel, for the view.
fn host_context(panel: &Panel, theme: &Theme) -> Value {
    let size = panel.bounds.get().map(|b| b.size);
    json!({
        "toolInfo": { "id": panel.tool_id, "tool": panel.ui.tool },
        "theme": if theme.is_dark() { "dark" } else { "light" },
        "styles": { "variables": {
            "--color-background-primary": hex(theme.background),
            "--color-background-secondary": hex(theme.secondary),
            "--color-text-primary": hex(theme.foreground),
            "--color-text-secondary": hex(theme.muted_foreground),
            "--color-border-primary": hex(theme.border),
            "--font-sans": theme.font_family.to_string(),
            "--font-mono": theme.mono_font_family.to_string(),
        }},
        "displayMode": "inline",
        "availableDisplayModes": ["inline"],
        "containerDimensions": {
            "width": size.map_or(PANEL, |s| f32::from(s.width)),
            "maxHeight": size.map_or(600., |s| f32::from(s.height)),
        },
        "locale": std::env::var("LANG").ok().and_then(|l| l.split('.').next().map(|l| l.replace('_', "-"))).unwrap_or_else(|| "en-US".into()),
        "platform": "desktop",
        "userAgent": format!("help/{}", env!("CARGO_PKG_VERSION")),
        "deviceCapabilities": { "touch": false, "hover": true },
    })
}

fn build_webview(
    ui: &AppUi,
    generation: u64,
    messages: async_channel::Sender<(u64, String)>,
    window: &Window,
) -> wry::Result<WebView> {
    let html: Arc<[u8]> = ui.html.as_bytes().into();
    let csp = mcp_apps::csp_header(ui.csp.as_ref());
    WebViewBuilder::new()
        .with_incognito(true)
        .with_devtools(cfg!(debug_assertions))
        .with_bounds(Rect { position: LogicalPosition::new(0., 0.).into(), size: LogicalSize::new(0., 0.).into() })
        .with_initialization_script(BRIDGE)
        .with_custom_protocol(SCHEME.into(), move |_, request| {
            let found = matches!(request.uri().path(), "" | "/" | "/index.html");
            Response::builder()
                .status(if found { 200 } else { 404 })
                .header(CONTENT_TYPE, "text/html; charset=utf-8")
                .header("Content-Security-Policy", csp.as_str())
                .body(if found { Cow::Owned(html.to_vec()) } else { Cow::Borrowed(&[][..]) })
                .unwrap_or_default()
        })
        .with_ipc_handler(move |request| {
            let _ = messages.try_send((generation, request.into_body()));
        })
        // The view stays on its page. It opens links with `ui/open-link`.
        .with_navigation_handler(|url| url.starts_with(&format!("{SCHEME}://")) || url == "about:blank")
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_url(format!("{SCHEME}://localhost/"))
        .build_as_child(window)
}

/// Open a link of a view in the browser. Only web and mail links open.
pub(super) fn open_link(url: &str) -> Result<Value, String> {
    if !["https://", "http://", "mailto:"].iter().any(|p| url.starts_with(p)) {
        return Ok(json!({ "isError": true }));
    }
    std::process::Command::new("/usr/bin/open").arg(url).spawn().map_err(|e| e.to_string())?;
    Ok(json!({}))
}

impl AppView {
    /// The MCP connections of the app, for the agent and the folder of the conversation.
    fn apps(&mut self) -> Apps {
        let (kind, cwd) = (self.chat.kind, &self.chat.cwd);
        match &self.apps {
            Some((k, c, apps)) if *k == kind && c == cwd => apps.clone(),
            _ => {
                let apps = Apps::new(kind, cwd.clone());
                self.apps = Some((kind, cwd.clone(), apps.clone()));
                apps
            }
        }
    }

    /// The MCP servers changed. The next lookups connect again and read the new servers.
    pub(super) fn reset_apps(&mut self) {
        self.apps = None;
    }

    /// Connect to all the MCP servers of the agent, and show the servers that have views in the
    /// sidebar. The app calls this when the agent, the folder, or the servers change.
    pub(super) fn load_app_servers(&mut self, cx: &mut Context<Self>) {
        if demo::enabled() && self.demo_data != demo::Data::Demo {
            self.app_servers = self.demo_data.app_servers().into_iter().map(|(s, t)| AppEntry::named(s, t)).collect();
            cx.notify();
            return;
        }
        let apps = self.apps();
        let (kind, cwd) = (self.chat.kind, self.chat.cwd.clone());
        let task = self.env.runtime.spawn(async move { apps.app_servers().await });
        cx.spawn(async move |this, cx| {
            let Ok(servers) = task.await else { return };
            let entries: Vec<AppEntry> = servers.into_iter().map(AppEntry::new).collect();
            let _ = this.update(cx, |this, cx| {
                // The agent or the folder changed while the servers connected.
                if this.chat.kind != kind || this.chat.cwd != cwd {
                    return;
                }
                this.app_servers = entries;
                cx.notify();
            });
        })
        .detach();
    }

    /// Look for the views of new MCP tool calls, and send the new data of the tool to the view
    /// in the panel. The app calls this after each event of the agent.
    pub(super) fn sync_mcp(&mut self, cx: &mut Context<Self>) {
        let new: Vec<(String, McpCall)> = self
            .chat
            .mcp_calls
            .iter_mut()
            .filter(|(_, s)| matches!(s.lookup, Lookup::NotStarted))
            .map(|(id, s)| {
                s.lookup = Lookup::Waiting;
                (id.clone(), s.call.clone())
            })
            .collect();
        for (id, call) in new {
            let apps = self.apps();
            let task = self.env.runtime.spawn(async move { apps.find(&call.server, &call.tool).await });
            cx.spawn(async move |this, cx| {
                let ui = task.await.ok().flatten();
                let _ = this.update(cx, |this, cx| {
                    let Some(state) = this.chat.mcp_calls.get_mut(&id) else { return };
                    match ui {
                        Some(ui) => {
                            state.lookup = Lookup::View(Arc::new(ui));
                            this.open_app(id, cx);
                        }
                        None => state.set_no_view(),
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        self.sync_panel();
    }

    /// Send the input and the result of the tool to the view, when the view is ready for them.
    fn sync_panel(&mut self) {
        let busy = self.chat.busy;
        let Some(panel) = &mut self.panel else { return };
        let Some(state) = self.chat.mcp_calls.get(&panel.tool_id) else { return };
        if !panel.initialized || panel.sent_result {
            return;
        }
        let started = state.status != ToolStatus::Pending || state.output.is_some();
        if !panel.sent_input && started {
            panel.notify("ui/notifications/tool-input", json!({ "arguments": state.input.clone().unwrap_or(json!({})) }));
            panel.sent_input = true;
        }
        match state.status {
            ToolStatus::Done | ToolStatus::Failed if panel.sent_input => {
                let mut result = state.output.clone().unwrap_or_else(|| json!({ "content": [] }));
                if state.status == ToolStatus::Failed {
                    result["isError"] = json!(true);
                }
                panel.notify("ui/notifications/tool-result", result);
                panel.sent_result = true;
            }
            // The turn ended, and the tool did not complete.
            _ if !busy => {
                panel.notify("ui/notifications/tool-cancelled", json!({ "reason": "The turn ended." }));
                panel.sent_result = true;
            }
            _ => {}
        }
    }

    /// Show the view of the tool call `tool_id` in the panel. The old view closes.
    pub(super) fn open_app(&mut self, tool_id: String, cx: &mut Context<Self>) {
        let Some(ui) = self.chat.mcp_calls.get(&tool_id).and_then(|s| s.view()).cloned() else { return };
        if self.panel.as_ref().is_some_and(|p| p.tool_id == tool_id) {
            return;
        }
        self.close_app(cx);
        self.close_doc(cx);
        self.close_steps(cx);
        self.panel_generation += 1;
        self.panel = Some(Panel {
            tool_id,
            ui,
            webview: None,
            generation: self.panel_generation,
            initialized: false,
            sent_input: false,
            sent_result: false,
            dark: false,
            bounds: Rc::default(),
            next_request: 1,
        });
        cx.notify();
    }

    /// Close the panel. The view gets `ui/resource-teardown`, and some time to clean up.
    pub(super) fn close_app(&mut self, cx: &mut Context<Self>) {
        let Some(mut panel) = self.panel.take() else { return };
        let Some(webview) = panel.webview.take() else { return };
        if panel.initialized {
            let _ = webview.evaluate_script(&format!(
                "window.__helpDeliver({})",
                json!({ "jsonrpc": "2.0", "id": format!("teardown-{}", panel.next_request), "method": "ui/resource-teardown", "params": { "reason": "The user closed the view." } })
            ));
        }
        let _ = webview.set_visible(false);
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(TEARDOWN).await;
            drop(webview);
        })
        .detach();
        cx.notify();
    }

    /// Make the web view of the panel if it is not there, and keep it on the place of the panel.
    /// The app calls this when it renders.
    pub(super) fn prepare_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<(Rc<WebView>, PanelBounds)> {
        let messages = self.app_messages.clone();
        let panel = self.panel.as_mut()?;
        if panel.webview.is_none() {
            match build_webview(&panel.ui, panel.generation, messages, window) {
                Ok(webview) => panel.webview = Some(Rc::new(webview)),
                Err(e) => {
                    tracing::warn!("cannot show the MCP App view: {e}");
                    self.panel = None;
                    return None;
                }
            }
        }
        // The view gets a change of the theme.
        let dark = cx.theme().is_dark();
        if panel.initialized && panel.dark != dark {
            panel.dark = dark;
            let theme = cx.theme().clone();
            let context = host_context(panel, &theme);
            panel.notify("ui/notifications/host-context-changed", json!({ "theme": context["theme"], "styles": context["styles"] }));
        }
        Some((panel.webview.clone()?, panel.bounds.clone()))
    }

    /// A message from a view.
    pub(super) fn on_app_message(&mut self, generation: u64, text: String, cx: &mut Context<Self>) {
        let Some(panel) = self.panel.as_ref().filter(|p| p.generation == generation) else { return };
        let Ok(message) = serde_json::from_str::<Value>(&text) else { return };
        let Some(method) = message.get("method").and_then(Value::as_str) else { return };
        tracing::debug!("the MCP App view sent {method}");
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(json!({}));
        let result: Result<Value, String> = match method {
            "ui/initialize" => {
                let theme = cx.theme().clone();
                Ok(json!({
                    "protocolVersion": mcp_apps::PROTOCOL_VERSION,
                    "hostInfo": { "name": "help", "version": env!("CARGO_PKG_VERSION") },
                    "hostCapabilities": {
                        "openLinks": {},
                        "serverTools": {},
                        "serverResources": {},
                        "logging": {},
                    },
                    "hostContext": host_context(panel, &theme),
                }))
            }
            "ui/notifications/initialized" => {
                let dark = cx.theme().is_dark();
                if let Some(panel) = &mut self.panel {
                    panel.initialized = true;
                    panel.dark = dark;
                }
                self.sync_panel();
                return;
            }
            "ui/notifications/request-teardown" => {
                self.close_app(cx);
                return;
            }
            "notifications/message" => {
                tracing::info!("MCP App view {}: {}", panel.ui.uri, params);
                return;
            }
            "ping" => Ok(json!({})),
            "ui/open-link" => open_link(params.get("url").and_then(Value::as_str).unwrap_or_default()),
            "ui/request-display-mode" => Ok(json!({ "mode": "inline" })),
            "ui/update-model-context" => {
                let mut context = content_text(params.get("content").unwrap_or(&Value::Null));
                if let Some(data) = params.get("structuredContent") {
                    context.push_str(&format!("\n{data}"));
                }
                let name = panel.ui.tool.get("name").and_then(Value::as_str).unwrap_or("tool").to_string();
                self.app_context = (!context.trim().is_empty()).then_some((name, context));
                Ok(json!({}))
            }
            "ui/message" => {
                let text = content_text(params.get("content").unwrap_or(&Value::Null));
                if text.trim().is_empty() {
                    Err("The message has no text.".into())
                } else if self.chat.busy {
                    Err("The agent is busy. Send the message again when the turn ends.".into())
                } else if self.submit(text, Vec::new(), cx).is_ok() {
                    Ok(json!({}))
                } else {
                    Err("The agent does not run. Send the message again later.".into())
                }
            }
            "tools/call" | "resources/read" | "resources/list" | "resources/templates/list" => {
                let Some(id) = id else { return };
                self.proxy(generation, id, method.to_string(), params, cx);
                return;
            }
            // A response or a notification that the app does not use.
            _ if id.is_none() => return,
            _ => Err(format!("The app does not know the method {method}.")),
        };
        if let (Some(id), Some(panel)) = (id, &self.panel) {
            panel.reply(id, result);
        }
    }

    /// Send a request of the view to its MCP server, and the result back to the view.
    fn proxy(&mut self, generation: u64, id: Value, method: String, params: Value, cx: &mut Context<Self>) {
        let Some(panel) = &self.panel else { return };
        let server = panel.ui.server.clone();
        let apps = self.apps();
        let task = self.env.runtime.spawn(async move {
            match method.as_str() {
                "tools/call" => {
                    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
                    let arguments = params.get("arguments").and_then(Value::as_object).cloned();
                    apps.call_tool(&server, name, arguments).await
                }
                "resources/read" => {
                    apps.read_resource(&server, params.get("uri").and_then(Value::as_str).unwrap_or_default()).await
                }
                "resources/list" => apps.list_resources(&server).await,
                _ => apps.list_resource_templates(&server).await,
            }
        });
        cx.spawn(async move |this, cx| {
            let result = match task.await {
                Ok(r) => r.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            let _ = this.update(cx, |this, _| {
                if let Some(panel) = this.panel.as_ref().filter(|p| p.generation == generation) {
                    panel.reply(id, result);
                }
            });
        })
        .detach();
    }
}

/// Keep the web view on the place of the panel.
pub(super) fn place(webview: &WebView, bounds: Bounds<Pixels>) {
    let rect = Rect {
        position: LogicalPosition::new(f32::from(bounds.origin.x), f32::from(bounds.origin.y)).into(),
        size: LogicalSize::new(f32::from(bounds.size.width), f32::from(bounds.size.height)).into(),
    };
    if let Err(e) = webview.set_bounds(rect) {
        tracing::warn!("cannot move the MCP App view: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_of_a_message_joins_its_text_blocks() {
        let content = json!([{ "type": "text", "text": "a" }, { "type": "image" }, { "type": "text", "text": "b" }]);
        assert_eq!(content_text(&content), "a\nb");
        assert_eq!(content_text(&json!({ "type": "text", "text": "one" })), "one");
    }

    #[test]
    fn a_tool_with_no_view_keeps_no_data() {
        let call = McpCall { server: "help".into(), tool: "vm_screenshot".into() };
        let mut state = McpCallState::new(call, Some(json!({})), ToolStatus::Running);
        state.update(None, Some(json!({ "content": [] })));
        assert!(state.output.is_some());
        state.set_no_view();
        assert!(state.input.is_none() && state.output.is_none());
        state.update(Some(json!({})), Some(json!({ "content": [] })));
        assert!(state.input.is_none() && state.output.is_none());
    }

    #[test]
    fn only_web_and_mail_links_open() {
        assert_eq!(open_link("file:///etc/passwd").unwrap(), json!({ "isError": true }));
        assert_eq!(open_link("javascript:alert(1)").unwrap(), json!({ "isError": true }));
    }
}
