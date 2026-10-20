//! Finds new versions of the app, and installs them.
//!
//! Each release publishes `latest.json` and a zip archive of the app (see
//! `scripts/sign-release.sh`). The manifest gives an Ed25519 signature of the archive. The
//! signed message is the version, a newline, and the archive, so that an old archive cannot
//! show as a new version. The app installs an archive only when the signature agrees with
//! `PUBLIC_KEY`.
//!
//! The app looks for a new version at start and then at an interval. "Check for Updates…"
//! in the menu bar item also starts a check. Only an app in an Applications folder looks
//! for updates, so `cargo run`, `scripts/hot.sh`, and a copy in the disk image do not.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, VerifyingKey};
use gpui::{App, AppContext as _, AsyncApp, Global};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication};
use objc2_foundation::{NSString, NSUserDefaults};
use serde::Deserialize;

const MANIFEST_URL: &str =
    "https://github.com/sam-ayo/help-releases/releases/latest/download/latest.json";

/// An environment variable that replaces `MANIFEST_URL`, to test a release from a local
/// server. The archive must still have a correct signature.
const MANIFEST_URL_VAR: &str = "HELP_UPDATE_URL";

/// The public half of the release signing key, as base64 of the 32 raw bytes.
/// `scripts/sign-release.sh` tells how to get it from the private key.
const PUBLIC_KEY: &str = "LbZrwg0I2Ka6WnLIGCaXDYeVL3ps+gfepBC1VN8a3xk=";

/// The first check waits a short time, so that it does not slow the start.
const FIRST_CHECK: Duration = Duration::from_secs(10);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// The key in the user defaults of the version that the user skipped.
const SKIPPED_KEY: &str = "SkippedUpdateVersion";

#[derive(Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes: String,
    /// The zip archives of the app, by `std::env::consts::ARCH`.
    apps: HashMap<String, Archive>,
}

#[derive(Deserialize)]
struct Archive {
    url: String,
    signature: String,
}

/// True while a check or an install runs. A second check does not start then.
struct Busy(bool);

impl Global for Busy {}

/// Start the automatic checks.
pub fn init(cx: &mut App) {
    cx.set_global(Busy(false));
    if installed_app().is_none() {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(FIRST_CHECK).await;
        loop {
            cx.update(|cx| check(false, cx));
            cx.background_executor().timer(CHECK_INTERVAL).await;
        }
    })
    .detach();
}

/// Look for a new version. When `interactive` is true, the user asked for the check, so
/// the app also tells when there is no update or when the check fails.
pub fn check(interactive: bool, cx: &mut App) {
    let Some(app) = installed_app() else {
        if interactive {
            cx.spawn(async move |_| {
                show_alert(
                    "Updates are not available",
                    "Only a copy of Help in the Applications folder can install updates.",
                    &["OK"],
                )
            })
            .detach();
        }
        return;
    };
    if cx.global::<Busy>().0 {
        return;
    }
    cx.global_mut::<Busy>().0 = true;
    cx.spawn(async move |cx| {
        if let Err(err) = run(interactive, app, cx).await {
            tracing::warn!("update failed: {err:#}");
            if interactive {
                show_alert("The update failed", &format!("{err:#}"), &["OK"]);
            }
        }
        cx.update(|cx| cx.global_mut::<Busy>().0 = false);
    })
    .detach();
}

async fn run(interactive: bool, app: PathBuf, cx: &mut AsyncApp) -> Result<()> {
    let manifest = cx.background_spawn(async { fetch_manifest() }).await?;
    let current = env!("CARGO_PKG_VERSION");
    if !is_newer(&manifest.version, current) {
        if interactive {
            show_alert(
                "Help is up to date",
                &format!("You have the latest version, {current}."),
                &["OK"],
            );
        }
        return Ok(());
    }
    if !interactive && saved_string(SKIPPED_KEY).as_deref() == Some(manifest.version.as_str()) {
        return Ok(());
    }
    let Manifest { version, notes, mut apps } = manifest;
    let archive = apps
        .remove(std::env::consts::ARCH)
        .ok_or_else(|| anyhow!("version {version} has no app for this Mac"))?;

    let mut info = format!(
        "Help {version} is available. You have {current}.\n\n\
         The agent and the agent Mac stop while Help restarts."
    );
    if !notes.trim().is_empty() {
        info.push_str("\n\n");
        info.push_str(notes.trim());
    }
    // An automatic check waits until the user uses the app. The alert then does not take
    // the focus from another app.
    while !is_active() {
        cx.background_executor().timer(Duration::from_secs(1)).await;
    }
    match show_alert(
        "A new version is available",
        &info,
        &["Install and Relaunch", "Later", "Skip This Version"],
    ) {
        Some(0) => {}
        Some(2) => {
            save_string(SKIPPED_KEY, &version);
            return Ok(());
        }
        _ => return Ok(()),
    }

    if !writable(&app) || !writable(&app.join("Contents")) {
        bail!(
            "Help cannot replace {}. Download the new version from \
             https://github.com/sam-ayo/help-releases/releases/latest",
            app.display()
        );
    }
    let target = app.clone();
    cx.background_spawn(async move {
        let path = download(archive, &version)?;
        replace(&path, &target)
    })
    .await?;
    relaunch(&app)?;
    cx.update(|cx| cx.quit());
    Ok(())
}

/// The bundle of this app, when it runs from an Applications folder.
fn installed_app() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // The executable is Help.app/Contents/MacOS/help.
    let app = exe.parent()?.parent()?.parent()?.to_path_buf();
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let in_applications = app.parent().is_some_and(|dir| {
        dir == Path::new("/Applications") || dir == home.join("Applications")
    });
    (app.extension()? == "app" && in_applications).then_some(app)
}

/// Run curl, and return what it wrote. macOS always has curl, so the app needs no HTTP
/// library.
fn curl(url: &str, max_time: &str) -> Result<Vec<u8>> {
    let output = Command::new("/usr/bin/curl")
        .args(["--fail", "--silent", "--show-error", "--location"])
        .args(["--connect-timeout", "15", "--max-time", max_time])
        .args(["--user-agent", concat!("help/", env!("CARGO_PKG_VERSION"))])
        .arg(url)
        .output()
        .context("could not start curl")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("could not download {url}: {}", stderr.trim());
    }
    Ok(output.stdout)
}

fn fetch_manifest() -> Result<Manifest> {
    let url = std::env::var(MANIFEST_URL_VAR).unwrap_or_else(|_| MANIFEST_URL.into());
    serde_json::from_slice(&curl(&url, "30")?).context("could not read the update manifest")
}

/// Download the archive and check its signature. Returns the path of the archive.
fn download(archive: Archive, version: &str) -> Result<PathBuf> {
    let bytes = curl(&archive.url, "1800")?;
    verify(version, &bytes, &archive.signature)?;
    // The name does not use the version, because the manifest has no signature and the
    // version can contain any text.
    let dir = std::env::temp_dir().join("help-update");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("help.zip");
    std::fs::write(&path, &bytes)?;
    Ok(path)
}

fn verify(version: &str, archive: &[u8], signature: &str) -> Result<()> {
    let key: [u8; 32] = BASE64
        .decode(PUBLIC_KEY)?
        .try_into()
        .map_err(|_| anyhow!("the public key is not 32 bytes"))?;
    let key = VerifyingKey::from_bytes(&key)?;
    let signature: [u8; 64] = BASE64
        .decode(signature.trim())
        .ok()
        .and_then(|s| s.try_into().ok())
        .context("the update has a bad signature")?;
    let mut message = Vec::with_capacity(version.len() + 1 + archive.len());
    message.extend_from_slice(version.as_bytes());
    message.push(b'\n');
    message.extend_from_slice(archive);
    key.verify_strict(&message, &Signature::from_bytes(&signature))
        .context("the signature of the update does not agree with the release key")
}

fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a valid C string.
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

/// Replace the contents of the app with the app in a zip archive. The running process keeps
/// its open files, so it can continue until the relaunch.
fn replace(archive: &Path, app: &Path) -> Result<()> {
    let dir = archive.with_extension("");
    let _ = std::fs::remove_dir_all(&dir);
    let result = (|| {
        let output = Command::new("/usr/bin/ditto")
            .args(["-x", "-k"])
            .arg(archive)
            .arg(&dir)
            .output()
            .context("could not start ditto")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("could not open the archive: {}", stderr.trim());
        }
        let new = dir.join("Help.app");
        // Make sure that the archive has a complete app, before it replaces the app that
        // works.
        let output = Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&new)
            .output()
            .context("could not start codesign")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("the new app has a bad code signature: {}", stderr.trim());
        }
        // One atomic step exchanges the two directories. After it, the old contents are in
        // the temporary directory.
        swap(&new.join("Contents"), &app.join("Contents"))
    })();
    let _ = std::fs::remove_file(archive);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn swap(a: &Path, b: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;
    let a = CString::new(a.as_os_str().as_bytes())?;
    let b = CString::new(b.as_os_str().as_bytes())?;
    // SAFETY: Both paths are valid C strings.
    if unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) } != 0 {
        return Err(std::io::Error::last_os_error()).context("could not replace the app");
    }
    Ok(())
}

/// Open the new app after this process stops.
fn relaunch(app: &Path) -> Result<()> {
    let pid = std::process::id();
    Command::new("/bin/sh")
        .arg("-c")
        .arg(format!(
            "while /bin/kill -0 {pid} 2>/dev/null; do sleep 0.2; done; /usr/bin/open \"$0\""
        ))
        .arg(app)
        .spawn()
        .context("could not relaunch the app")?;
    Ok(())
}

/// True when `new` is a later version than `current`. Both have the form `1.2.3`. A version
/// with another form is never newer.
fn is_newer(new: &str, current: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u64>> {
        v.trim_start_matches('v').split('.').map(|n| n.parse().ok()).collect()
    };
    match (parse(new), parse(current)) {
        (Some(new), Some(current)) => new > current,
        _ => false,
    }
}

fn is_active() -> bool {
    MainThreadMarker::new().is_some_and(|mtm| NSApplication::sharedApplication(mtm).isActive())
}

/// Show a modal alert with the buttons, the first button as the default. Returns the index
/// of the button that the user chose.
///
/// The alert runs its own event loop until it closes. Call it from a task, not while GPUI
/// updates the app.
fn show_alert(message: &str, info: &str, buttons: &[&str]) -> Option<usize> {
    let mtm = MainThreadMarker::new()?;
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(message));
    alert.setInformativeText(&NSString::from_str(info));
    for title in buttons {
        alert.addButtonWithTitle(&NSString::from_str(title));
    }
    NSApplication::sharedApplication(mtm).activate();
    usize::try_from(alert.runModal() - NSAlertFirstButtonReturn).ok()
}

fn saved_string(key: &str) -> Option<String> {
    let key = NSString::from_str(key);
    Some(NSUserDefaults::standardUserDefaults().stringForKey(&key)?.to_string())
}

fn save_string(key: &str, value: &str) {
    let key = NSString::from_str(key);
    let value = NSString::from_str(value);
    // SAFETY: a string is a property list value, so the defaults can keep it.
    unsafe { NSUserDefaults::standardUserDefaults().setObject_forKey(Some(&value), &key) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("0.2.0-beta", "0.1.0"));
    }

    /// OpenSSL made this signature of "0.2.0\npackage" with the release key, as
    /// `scripts/sign-release.sh` does.
    const SIGNATURE: &str =
        "UWVMYjQh+w6k1J5k+xOrqCDV7hYzjGot/vuV7Y7J+rAG+Dq3R7aQGQyA406nHJHPpYXeEHrzSRW49VkVDZE2Bw==";

    #[test]
    fn accepts_a_release_signature() {
        verify("0.2.0", b"package", SIGNATURE).unwrap();
    }

    #[test]
    fn rejects_a_signature_for_another_version_or_archive() {
        assert!(verify("0.3.0", b"package", SIGNATURE).is_err());
        assert!(verify("0.2.0", b"packagf", SIGNATURE).is_err());
    }

    #[test]
    fn rejects_a_bad_signature() {
        assert!(verify("0.2.0", b"package", &BASE64.encode([0u8; 64])).is_err());
        assert!(verify("0.2.0", b"package", "not base64").is_err());
    }
}
