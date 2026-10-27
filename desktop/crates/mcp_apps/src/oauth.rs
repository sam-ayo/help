//! The OAuth tokens of Claude Code. Claude Code signs in to an HTTP server with OAuth and keeps
//! the token in the keychain. The app uses the same token, because the server does not accept
//! a connection without it.

use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// The keychain item where Claude Code keeps its tokens.
const SERVICE: &str = "Claude Code-credentials";

/// The app does not use a token that expires in less than this time.
const MARGIN: Duration = Duration::from_secs(60);

/// The access token of Claude Code for the server `name` at `url`, or `None` if there is no
/// token that is still valid. The app does not refresh a token: a refresh can make the token of
/// Claude Code stop working. Claude Code refreshes it, and the app reads it again on the next try.
pub(crate) async fn claude_token(name: &str, url: &str) -> Option<String> {
    let output = tokio::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", SERVICE, "-w"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(5), output).await.ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    let credentials: Value = serde_json::from_slice(&output.stdout).ok()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
    find_token(&credentials, name, url, now)
}

/// The token of the entry in `mcpOAuth` with this server name and URL. When there are two or
/// more, the one that expires last comes first. `expiresAt` is in milliseconds, and 0 or no
/// value means that the expiry is not known.
fn find_token(credentials: &Value, name: &str, url: &str, now_ms: u64) -> Option<String> {
    let entries = credentials["mcpOAuth"].as_object()?;
    entries
        .values()
        .filter(|e| e["serverName"] == name && e["serverUrl"].as_str().is_some_and(|u| same_url(u, url)))
        .filter_map(|e| {
            let token = e["accessToken"].as_str().filter(|t| !t.is_empty())?;
            let expires = e["expiresAt"].as_u64().unwrap_or(0);
            let valid = expires == 0 || expires > now_ms + MARGIN.as_millis() as u64;
            valid.then_some((if expires == 0 { u64::MAX } else { expires }, token))
        })
        .max_by_key(|(expires, _)| *expires)
        .map(|(_, token)| token.to_string())
}

fn same_url(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_valid_token_of_the_same_server_comes_first() {
        let credentials = json!({"mcpOAuth": {
            "mail|1": {"serverName": "mail", "serverUrl": "https://m/mcp", "accessToken": "old", "expiresAt": 1_000},
            "mail|2": {"serverName": "mail", "serverUrl": "https://m/mcp/", "accessToken": "new", "expiresAt": 900_000},
            "mail|3": {"serverName": "mail", "serverUrl": "https://other/mcp", "accessToken": "other"},
            "neon|1": {"serverName": "neon", "serverUrl": "https://n/mcp", "accessToken": "n", "expiresAt": 0},
        }});
        assert_eq!(find_token(&credentials, "mail", "https://m/mcp", 500_000).as_deref(), Some("new"));
        assert_eq!(find_token(&credentials, "mail", "https://m/mcp", 890_000), None);
        assert_eq!(find_token(&credentials, "neon", "https://n/mcp", 500_000).as_deref(), Some("n"));
        assert_eq!(find_token(&credentials, "posthog", "https://p/mcp", 0), None);
    }
}
