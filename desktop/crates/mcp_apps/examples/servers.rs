//! Show the connected MCP servers that have views, with their logos.
//!
//! `cargo run -p mcp_apps --example servers -- <claude|codex> <cwd>`

use std::path::PathBuf;

use acp_host::AgentKind;
use mcp_apps::Apps;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [kind, cwd] = args.as_slice() else {
        anyhow::bail!("usage: servers <claude|codex> <cwd>");
    };
    let kind = match kind.as_str() {
        "claude" => AgentKind::Claude,
        "codex" => AgentKind::Codex,
        other => anyhow::bail!("unknown agent {other}"),
    };
    let cwd = std::fs::canonicalize(PathBuf::from(cwd))?;
    let apps = Apps::new(kind, cwd);
    for app in apps.app_servers().await {
        let logo = |l: &Option<mcp_apps::Logo>| l.as_ref().map(|l| format!("{} {} bytes", l.mime, l.bytes.len()));
        println!("{} ({}): logo {:?}, dark logo {:?}", app.title, app.server, logo(&app.logo), logo(&app.logo_dark));
    }
    Ok(())
}
