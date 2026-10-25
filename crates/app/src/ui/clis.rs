//! The command line tools that the agent can use. The agent runs on this Mac with the PATH of
//! the login shell (see `load_login_path`), so a tool on that PATH is a tool of the agent.
//!
//! The sidebar shows the tools that the user installed, not the tools of macOS. A package can
//! have many commands, so the sidebar shows one row for each package: a Homebrew formula or
//! cask, an npm package, a Python package, or an app. Homebrew formulae that other formulae need do not show.

use std::{
    collections::{BTreeMap, HashSet},
    path::{Component, Path, PathBuf},
};

use gpui::{AppContext as _, Context};

use super::{AppView, demo};

/// The folders of macOS. Their tools are on each Mac, so the sidebar does not show them.
const SYSTEM: &[&str] = &["/usr/bin", "/bin", "/usr/sbin", "/sbin", "/System", "/Library/Apple", "/var/run", "/private/var/run"];

/// A package with commands on the PATH.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Cli {
    pub name: String,
    /// The commands of the package, in alphabetical order. The tooltip shows them.
    pub commands: Vec<String>,
}

/// The packages on the PATH, in alphabetical order.
fn find() -> Vec<Cli> {
    let dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    let mut seen = HashSet::new();
    let mut packages: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for dir in dirs {
        if dir.is_relative() || SYSTEM.iter().any(|s| dir.starts_with(s)) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let real_dir = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        for entry in entries.flatten() {
            let Ok(command) = entry.file_name().into_string() else { continue };
            if command.starts_with('.') || [".bat", ".cmd", ".exe"].iter().any(|e| command.ends_with(e)) {
                continue;
            }
            let Ok(real) = entry.path().canonicalize() else { continue };
            if !executable(&real) {
                continue;
            }
            // As in the shell, the first folder on the PATH has the command.
            if !seen.insert(command.clone()) {
                continue;
            }
            let Some(name) = package(&command, &real, &real_dir) else { continue };
            let (_, commands) = packages.entry(name.to_lowercase()).or_insert_with(|| (name, Vec::new()));
            commands.push(command);
        }
    }
    packages
        .into_values()
        .map(|(name, mut commands)| {
            commands.sort();
            Cli { name, commands }
        })
        .collect()
}

/// The package of a command. `real` is the file that runs, and `real_dir` is the folder of the
/// command on the PATH, with no links. `None` if the package does not show.
fn package(command: &str, real: &Path, real_dir: &Path) -> Option<String> {
    let parts: Vec<&str> = real
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    for (ix, part) in parts.iter().enumerate() {
        let next = parts.get(ix + 1).copied();
        match *part {
            "Cellar" => {
                let name = next?;
                parts.get(ix + 2)?;
                let keg: PathBuf = std::iter::once("/").chain(parts[..ix + 3].iter().copied()).collect();
                return installed_on_request(&keg).then(|| name.to_string());
            }
            "Caskroom" => return next.map(str::to_string),
            "node_modules" => {
                let name = next?;
                return match name.starts_with('@') {
                    true => parts.get(ix + 2).map(|rest| format!("{name}/{rest}")),
                    false => Some(name.to_string()),
                };
            }
            _ => {}
        }
        for suffix in [".app", ".framework"] {
            if let Some(name) = part.strip_suffix(suffix) {
                return Some(name.to_string());
            }
        }
    }
    if let Some(module) = python_module(real) {
        return Some(module);
    }
    // A link to a file in the same folder, as `cargo` to `rustup`, is a command of that file.
    if real.parent() == Some(real_dir) {
        return real.file_name()?.to_str().map(str::to_string);
    }
    Some(command.to_string())
}

/// The top module of a script that pip made for a command, as `rsa` for `pyrsa-sign`. In
/// each script of this type, the line before `if __name__ == '__main__':` imports the
/// function of the command from the package, as in `from rsa.cli import sign`.
fn python_module(script: &Path) -> Option<String> {
    use std::io::Read as _;
    let mut head = [0; 512];
    let n = std::fs::File::open(script).and_then(|mut f| f.read(&mut head)).ok()?;
    let head = String::from_utf8_lossy(&head[..n]);
    let lines: Vec<&str> = head.lines().collect();
    if !lines.first()?.starts_with("#!") || !lines[0].contains("python") {
        return None;
    }
    let main = lines.iter().position(|l| l.starts_with("if __name__ == "))?;
    let from = lines.get(main.checked_sub(1)?)?.strip_prefix("from ")?;
    let module = from.split(['.', ' ']).next()?;
    (!module.is_empty() && !module.starts_with("__")).then(|| module.to_string())
}

/// A Homebrew formula that the user installed, not a formula that other formulae need.
fn installed_on_request(keg: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(keg.join("INSTALL_RECEIPT.json")) else { return true };
    let receipt: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    receipt["installed_on_request"].as_bool().unwrap_or(true)
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

    fn name(real: &str, real_dir: &str) -> Option<String> {
        package("cmd", Path::new(real), Path::new(real_dir))
    }

    #[test]
    fn packages() {
        assert_eq!(name("/opt/homebrew/Caskroom/ngrok/3.1/ngrok", "/opt/homebrew/bin").as_deref(), Some("ngrok"));
        assert_eq!(name("/n/lib/node_modules/@openai/codex/bin/codex.js", "/n/bin").as_deref(), Some("@openai/codex"));
        assert_eq!(name("/n/lib/node_modules/neon/dist/cli.js", "/n/bin").as_deref(), Some("neon"));
        assert_eq!(name("/Applications/Visual Studio Code.app/Contents/bin/code", "/usr/local/bin").as_deref(), Some("Visual Studio Code"));
        assert_eq!(name("/Library/Frameworks/Python.framework/Versions/3.11/bin/python3", "/usr/local/bin").as_deref(), Some("Python"));
        assert_eq!(name("/h/.cargo/bin/rustup", "/h/.cargo/bin").as_deref(), Some("rustup"));
        assert_eq!(name("/h/.local/share/claude/versions/2.1.0", "/h/.local/bin").as_deref(), Some("cmd"));
    }

    #[test]
    fn python_scripts_show_their_package() {
        let dir = std::env::temp_dir().join(format!("clis-py-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("pyrsa-sign");
        std::fs::write(&script, "#!/usr/bin/python3\nfrom __future__ import x\nfrom rsa.cli import sign\nif __name__ == '__main__':\n").unwrap();
        let other = dir.join("tool");
        std::fs::write(&other, "#!/usr/bin/python3\nfrom io import StringIO\nimport sys\nif __name__ == '__main__':\n").unwrap();
        assert_eq!(python_module(&script).as_deref(), Some("rsa"));
        assert_eq!(python_module(&other), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn homebrew_dependencies_do_not_show() {
        let root = std::env::temp_dir().join(format!("clis-test-{}", std::process::id()));
        let keg = |name: &str, on_request: bool| {
            let keg = root.join("Cellar").join(name).join("1.0");
            std::fs::create_dir_all(&keg).unwrap();
            std::fs::write(keg.join("INSTALL_RECEIPT.json"), format!(r#"{{"installed_on_request":{on_request}}}"#)).unwrap();
            keg.join("bin/x").display().to_string()
        };
        let bin = root.join("bin").display().to_string();
        assert_eq!(name(&keg("gh", true), &bin).as_deref(), Some("gh"));
        assert_eq!(name(&keg("openssl", false), &bin), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
