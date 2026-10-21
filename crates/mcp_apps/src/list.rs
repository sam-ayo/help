//! The MCP servers that the agent has connected. Claude Code also uses the connectors of
//! claude.ai, which are not in the configuration files, so the app asks `claude mcp list`.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::connect;

/// `claude mcp list` checks each server, so it can take some time.
const LIST_TIMEOUT: Duration = Duration::from_secs(90);

/// One server in the list of the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listed {
    /// The name of the server, as the agent gives it.
    pub name: String,
    /// The URL of an HTTP server, or the command of a stdio server.
    pub target: String,
    pub connected: bool,
}

/// The servers of Claude Code in `cwd`, with their status.
pub(crate) async fn claude(cwd: &Path) -> Result<Vec<Listed>> {
    let (program, path) = connect::program("claude").await;
    let mut command = tokio::process::Command::new(program);
    command.args(["mcp", "list"]).stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    // The stdio servers that the check starts need the `PATH` of the user too.
    if let Some(path) = path {
        command.env("PATH", path);
    }
    if cwd.is_dir() {
        command.current_dir(cwd);
    }
    let output = tokio::time::timeout(LIST_TIMEOUT, command.output())
        .await
        .context("`claude mcp list` did not complete")??;
    if !output.status.success() {
        bail!("`claude mcp list` failed with {}", output.status);
    }
    Ok(parse(&String::from_utf8_lossy(&output.stdout)))
}

/// Read the lines `NAME: TARGET - STATUS`. A name can have `:` in it, but not `: `.
fn parse(text: &str) -> Vec<Listed> {
    text.lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once(": ")?;
            // An error in the status can have ` - ` in it.
            let (target, status) = rest.split_once(" - ")?;
            let target = target.trim_end_matches(" (HTTP)").trim_end_matches(" (SSE)");
            Some(Listed {
                name: name.trim().to_string(),
                target: target.trim().to_string(),
                connected: status.contains("Connected") && !status.contains("Failed"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lines_give_names_targets_and_status() {
        let text = "Checking MCP server health…\n\n\
            claude.ai Linear: https://mcp.linear.app/mcp - ✔ Connected\n\
            plugin:figma:figma: https://mcp.figma.com/mcp (HTTP) - ! Needs authentication\n\
            paper: /x/paper mcp - ✔ Connected\n\
            local: http://localhost:8080/mcp (HTTP) - ✘ Failed to connect — ECONNREFUSED: a - b\n";
        let listed = parse(text);
        assert_eq!(listed.len(), 4);
        assert_eq!(
            listed[0],
            Listed { name: "claude.ai Linear".into(), target: "https://mcp.linear.app/mcp".into(), connected: true }
        );
        assert_eq!(listed[1].name, "plugin:figma:figma");
        assert_eq!(listed[1].target, "https://mcp.figma.com/mcp");
        assert!(!listed[1].connected);
        assert_eq!(listed[2].target, "/x/paper mcp");
        assert_eq!(listed[3].target, "http://localhost:8080/mcp");
        assert!(!listed[3].connected);
    }
}
