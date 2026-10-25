//! The command line tools that the agent can use. The agent runs on this Mac with the PATH of
//! the login shell (see `load_login_path`), so a tool on that PATH is a tool of the agent. The
//! sidebar shows the known tools that the app finds there.

use std::path::{Path, PathBuf};

use gpui::Context;

use super::{AppView, demo};

/// The known tools: the command, and the name that the sidebar shows.
const KNOWN: &[(&str, &str)] = &[
    ("gh", "GitHub"),
    ("glab", "GitLab"),
    ("git", "Git"),
    ("aws", "AWS"),
    ("gcloud", "Google Cloud"),
    ("az", "Azure"),
    ("vercel", "Vercel"),
    ("netlify", "Netlify"),
    ("wrangler", "Cloudflare"),
    ("flyctl", "Fly.io"),
    ("railway", "Railway"),
    ("heroku", "Heroku"),
    ("supabase", "Supabase"),
    ("neonctl", "Neon"),
    ("firebase", "Firebase"),
    ("stripe", "Stripe"),
    ("docker", "Docker"),
    ("kubectl", "Kubernetes"),
    ("terraform", "Terraform"),
    ("doctl", "DigitalOcean"),
    ("op", "1Password"),
    ("linear", "Linear"),
    ("jira", "Jira"),
    ("sentry-cli", "Sentry"),
    ("psql", "PostgreSQL"),
    ("mysql", "MySQL"),
    ("sqlite3", "SQLite"),
    ("redis-cli", "Redis"),
    ("ffmpeg", "FFmpeg"),
    ("brew", "Homebrew"),
    ("xcodebuild", "Xcode"),
];

/// A command line tool on the PATH.
#[derive(Clone)]
pub(super) struct Cli {
    pub command: &'static str,
    pub name: &'static str,
    /// The file that runs. The tooltip shows it.
    pub path: PathBuf,
}

/// The known tools on the PATH, in the order of `KNOWN`.
fn find() -> Vec<Cli> {
    let dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    KNOWN
        .iter()
        .filter_map(|&(command, name)| {
            let path = dirs.iter().map(|d| d.join(command)).find(|p| executable(p))?;
            Some(Cli { command, name, path })
        })
        .collect()
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

impl AppView {
    /// Look for the tools again, because the user can install a tool while the app runs.
    pub(super) fn load_clis(&mut self, cx: &mut Context<Self>) {
        if demo::enabled() {
            return;
        }
        let task = cx.background_spawn(async move { find() });
        cx.spawn(async move |this, cx| {
            let clis = task.await;
            let _ = this.update(cx, |this, cx| {
                this.clis = clis;
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_unique() {
        let mut commands: Vec<_> = KNOWN.iter().map(|(c, _)| c).collect();
        commands.sort();
        commands.dedup();
        assert_eq!(commands.len(), KNOWN.len());
    }
}
