//! Show the MCP App of one tool, and call the tool.
//!
//! `cargo run -p mcp_apps --example probe -- <claude|codex> <cwd> <server> <tool> [arguments JSON]`

use std::path::PathBuf;

use acp_host::AgentKind;
use mcp_apps::{Apps, csp_header};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [kind, cwd, server, tool, rest @ ..] = args.as_slice() else {
        anyhow::bail!("usage: probe <claude|codex> <cwd> <server> <tool> [arguments JSON]");
    };
    let kind = match kind.as_str() {
        "claude" => AgentKind::Claude,
        "codex" => AgentKind::Codex,
        other => anyhow::bail!("unknown agent {other}"),
    };
    let cwd = std::fs::canonicalize(PathBuf::from(cwd))?;
    for spec in acp_host::mcp_servers(kind, &cwd) {
        println!("config: {} {:?}", spec.name, spec.transport);
    }

    let apps = Apps::new(kind, cwd);
    // Two calls at the same time must share one connection.
    let (ui, again) = tokio::join!(apps.find(server, tool), apps.find(server, tool));
    let Some(mut ui) = ui else {
        println!("no UI for {server} / {tool}");
        return Ok(());
    };
    assert!(again.is_some());
    let html_len = ui.html.len();
    ui.html = ui.html.chars().take(300).collect::<String>() + "...";
    println!("AppUi: {ui:#?}");
    println!("HTML length: {html_len}");
    println!("CSP: {}", csp_header(ui.csp.as_ref()));

    let arguments = match rest.first() {
        Some(json) => Some(serde_json::from_str(json)?),
        None => Some(serde_json::Map::new()),
    };
    let name = ui.tool["name"].as_str().unwrap_or(tool);
    let result = apps.call_tool(&ui.server, name, arguments).await?;
    println!("call_tool: {}", serde_json::to_string_pretty(&result)?);
    let resources = apps.list_resources(&ui.server).await?;
    println!("list_resources: {resources}");
    Ok(())
}
