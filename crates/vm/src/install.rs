//! Download a macOS restore image and install it into a new VM bundle.

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use block2::RcBlock;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_foundation::{NSError, NSURL};
use objc2_virtualization::*;

use crate::config::{self, Hardware};
use crate::{AgentMac, VmPhase, VM, completion_block, ns_error, on_main};

const DISK_SIZE: u64 = 96 << 30;

struct SendPtr<T>(T);
unsafe impl<T> Send for SendPtr<T> {}

pub fn run(mac: &AgentMac) -> Result<()> {
    let paths = mac.paths().clone();
    std::fs::create_dir_all(&paths.bundle)?;

    let ipsw = paths.restore_image();
    if !ipsw.exists() {
        mac.set_phase(VmPhase::Downloading { fraction: 0.0 });
        let remote = fetch_latest_url()?;
        download(mac, &remote, &ipsw)?;
    }

    mac.set_phase(VmPhase::Installing { fraction: 0.0 });
    let image = load_restore_image(&ipsw)?;
    let requirements = unsafe { image.0.mostFeaturefulSupportedConfiguration() }
        .context("this Mac cannot run the macOS version in the restore image")?;
    let model = unsafe { requirements.hardwareModel() };
    anyhow::ensure!(unsafe { model.isSupported() }, "the hardware model is not supported on this Mac");

    // Create the bundle files.
    let disk = std::fs::File::create(paths.disk())?;
    disk.set_len(DISK_SIZE)?;
    drop(disk);
    let aux_url = NSURL::from_file_path(paths.aux()).context("aux path")?;
    let aux = unsafe {
        VZMacAuxiliaryStorage::initCreatingStorageAtURL_hardwareModel_options_error(
            VZMacAuxiliaryStorage::alloc(),
            &aux_url,
            &model,
            VZMacAuxiliaryStorageInitializationOptions::AllowOverwrite,
        )
    }
    .map_err(|e| ns_error(&e))?;
    let machine_id = unsafe { VZMacMachineIdentifier::new() };
    std::fs::write(paths.hardware_model(), unsafe { model.dataRepresentation() }.to_vec())?;
    std::fs::write(paths.machine_id(), unsafe { machine_id.dataRepresentation() }.to_vec())?;

    let hardware = SendPtr(Hardware { model, machine_id, aux });
    let paths2 = paths.clone();
    let ipsw2 = ipsw.clone();
    // Create the VM and the installer on the main queue, and keep both there.
    let started = on_main(move || -> Result<(SendPtr<Retained<VZMacOSInstaller>>, mpsc::Receiver<Result<(), String>>), String> {
        let hardware = hardware;
        let cfg = config::build(&paths2, Some(hardware.0)).map_err(|e| format!("{e:#}"))?;
        let vm = unsafe { VZVirtualMachine::initWithConfiguration(VZVirtualMachine::alloc(), &cfg) };
        let ipsw_url = NSURL::from_file_path(&ipsw2).ok_or("ipsw path")?;
        let installer = unsafe {
            VZMacOSInstaller::initWithVirtualMachine_restoreImageURL(VZMacOSInstaller::alloc(), &vm, &ipsw_url)
        };
        let (block, rx) = completion_block();
        unsafe { installer.installWithCompletionHandler(&block) };
        VM.with(|slot| *slot.borrow_mut() = Some(vm));
        Ok((SendPtr(installer), rx))
    });
    let (installer, rx) = started.map_err(|e| anyhow!(e))?;

    let result = loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(result) => break result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let installer = SendPtr(installer.0.clone());
                let fraction = on_main(move || {
                    let installer = installer;
                    unsafe { installer.0.progress().fractionCompleted() }
                });
                mac.set_phase(VmPhase::Installing { fraction });
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break Err("the installer stopped".into()),
        }
    };
    on_main(move || {
        drop(installer);
        VM.with(|slot| slot.borrow_mut().take()).map(|_| ())
    });
    result.map_err(|e| anyhow!("install failed: {e}"))?;

    std::fs::write(paths.installed_marker(), b"")?;
    // The restore image is large. It is not necessary after the install.
    let _ = std::fs::remove_file(&ipsw);
    mac.set_phase(VmPhase::Stopped);
    Ok(())
}

pub(crate) fn fetch_latest_url() -> Result<String> {
    let (tx, rx) = mpsc::channel();
    let tx = std::cell::Cell::new(Some(tx));
    let block = RcBlock::new(move |image: *mut VZMacOSRestoreImage, err: *mut NSError| {
        let result = if !err.is_null() {
            Err(unsafe { &*err }.localizedDescription().to_string())
        } else {
            let image = unsafe { &*image };
            unsafe { image.URL() }
                .absoluteString()
                .map(|s| s.to_string())
                .ok_or_else(|| "the restore image has no URL".to_string())
        };
        if let Some(tx) = tx.take() {
            let _ = tx.send(result);
        }
    });
    unsafe { VZMacOSRestoreImage::fetchLatestSupportedWithCompletionHandler(&block) };
    rx.recv_timeout(Duration::from_secs(60))
        .map_err(|_| anyhow!("timed out while looking for the latest macOS restore image"))?
        .map_err(|e| anyhow!(e))
}

fn load_restore_image(path: &std::path::Path) -> Result<SendPtr<Retained<VZMacOSRestoreImage>>> {
    let url = NSURL::from_file_path(path).context("ipsw path")?;
    let (tx, rx) = mpsc::channel();
    let tx = std::cell::Cell::new(Some(tx));
    let block = RcBlock::new(move |image: *mut VZMacOSRestoreImage, err: *mut NSError| {
        let result = if !err.is_null() {
            Err(unsafe { &*err }.localizedDescription().to_string())
        } else {
            Ok(SendPtr(unsafe { Retained::retain(image) }.unwrap()))
        };
        if let Some(tx) = tx.take() {
            let _ = tx.send(result);
        }
    });
    unsafe { VZMacOSRestoreImage::loadFileURL_completionHandler(&url, &block) };
    rx.recv_timeout(Duration::from_secs(120))
        .map_err(|_| anyhow!("timed out while reading the restore image"))?
        .map_err(|e| anyhow!("the restore image is not valid: {e}"))
}

/// Download with curl. A `.partial` file lets a stopped download continue.
fn download(mac: &AgentMac, url: &str, dest: &std::path::Path) -> Result<()> {
    let partial = dest.with_extension("ipsw.partial");
    let total = content_length(url).unwrap_or(0);
    let mut child = Command::new("/usr/bin/curl")
        .args(["-fL", "--retry", "5", "-C", "-", "-o"])
        .arg(&partial)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("start curl")?;
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("the download failed (curl exit code {:?})", status.code());
            }
            break;
        }
        if total > 0 {
            let have = std::fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);
            mac.set_phase(VmPhase::Downloading {
                fraction: have as f64 / total as f64,
            });
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    std::fs::rename(&partial, dest)?;
    Ok(())
}

fn content_length(url: &str) -> Option<u64> {
    let out = Command::new("/usr/bin/curl").args(["-sIL", url]).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse().ok())?
        })
        .last()
}
