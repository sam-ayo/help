//! Build the `VZVirtualMachineConfiguration` for the agent Mac.

use anyhow::{Context, Result, anyhow};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString, NSURL};
use objc2_virtualization::*;

use crate::{Paths, ns_error};

/// Display size in pixels. 80 pixels per inch gives a non-Retina display, so
/// screenshot pixels are equal to screen points.
pub const DISPLAY_WIDTH: isize = 1600;
pub const DISPLAY_HEIGHT: isize = 1000;

pub struct Hardware {
    pub model: Retained<VZMacHardwareModel>,
    pub machine_id: Retained<VZMacMachineIdentifier>,
    pub aux: Retained<VZMacAuxiliaryStorage>,
}

fn url(path: &std::path::Path) -> Result<Retained<NSURL>> {
    NSURL::from_file_path(path).with_context(|| format!("bad path {}", path.display()))
}

pub fn load_hardware(paths: &Paths) -> Result<Hardware> {
    let model_bytes = std::fs::read(paths.hardware_model()).context("read HardwareModel")?;
    let id_bytes = std::fs::read(paths.machine_id()).context("read MachineIdentifier")?;
    unsafe {
        let model = VZMacHardwareModel::initWithDataRepresentation(
            VZMacHardwareModel::alloc(),
            &NSData::with_bytes(&model_bytes),
        )
        .context("the saved hardware model is not valid")?;
        anyhow::ensure!(model.isSupported(), "this Mac does not support the VM hardware model");
        let machine_id = VZMacMachineIdentifier::initWithDataRepresentation(
            VZMacMachineIdentifier::alloc(),
            &NSData::with_bytes(&id_bytes),
        )
        .context("the saved machine identifier is not valid")?;
        let aux = VZMacAuxiliaryStorage::initWithURL(VZMacAuxiliaryStorage::alloc(), &*url(&paths.aux())?);
        Ok(Hardware { model, machine_id, aux })
    }
}

/// Build a configuration. `hardware` is `Some` during install (new bundle) and
/// `None` on a normal start (load from the bundle).
pub fn build(paths: &Paths, hardware: Option<Hardware>) -> Result<Retained<VZVirtualMachineConfiguration>> {
    let hw = match hardware {
        Some(hw) => hw,
        None => load_hardware(paths)?,
    };
    std::fs::create_dir_all(&paths.workspace)?;
    std::fs::create_dir_all(&paths.tools)?;
    let home = std::env::var("HOME").map(std::path::PathBuf::from).unwrap_or_default();

    let build = || -> Result<Retained<VZVirtualMachineConfiguration>> {
        unsafe {
            let platform = VZMacPlatformConfiguration::new();
            platform.setHardwareModel(&hw.model);
            platform.setMachineIdentifier(&hw.machine_id);
            platform.setAuxiliaryStorage(Some(&hw.aux));

            let c = VZVirtualMachineConfiguration::new();
            c.setPlatform(&platform);
            c.setBootLoader(Some(&VZMacOSBootLoader::new()));

            let host_cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
            let cpus = (host_cpus / 2)
                .clamp(4, 8)
                .clamp(
                    VZVirtualMachineConfiguration::minimumAllowedCPUCount(),
                    VZVirtualMachineConfiguration::maximumAllowedCPUCount(),
                );
            c.setCPUCount(cpus);
            let memory: u64 = 8 << 30;
            c.setMemorySize(memory.clamp(
                VZVirtualMachineConfiguration::minimumAllowedMemorySize(),
                VZVirtualMachineConfiguration::maximumAllowedMemorySize(),
            ));

            let disk = VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_error(
                VZDiskImageStorageDeviceAttachment::alloc(),
                &*url(&paths.disk())?,
                false,
            )
            .map_err(|e| ns_error(&e))?;
            let block = VZVirtioBlockDeviceConfiguration::initWithAttachment(
                VZVirtioBlockDeviceConfiguration::alloc(),
                &disk,
            );
            c.setStorageDevices(&NSArray::from_retained_slice(&[block.into_super()]));

            let net = VZVirtioNetworkDeviceConfiguration::new();
            net.setAttachment(Some(&VZNATNetworkDeviceAttachment::new()));
            c.setNetworkDevices(&NSArray::from_retained_slice(&[net.into_super()]));

            let gfx = VZMacGraphicsDeviceConfiguration::new();
            let display = VZMacGraphicsDisplayConfiguration::initWithWidthInPixels_heightInPixels_pixelsPerInch(
                VZMacGraphicsDisplayConfiguration::alloc(),
                DISPLAY_WIDTH,
                DISPLAY_HEIGHT,
                80,
            );
            gfx.setDisplays(&NSArray::from_retained_slice(&[display]));
            c.setGraphicsDevices(&NSArray::from_retained_slice(&[gfx.into_super()]));

            c.setKeyboards(&NSArray::from_retained_slice(&[VZMacKeyboardConfiguration::new().into_super()]));
            c.setPointingDevices(&NSArray::from_retained_slice(&[
                VZMacTrackpadConfiguration::new().into_super(),
                VZUSBScreenCoordinatePointingDeviceConfiguration::new().into_super(),
            ]));
            c.setSocketDevices(&NSArray::from_retained_slice(&[VZVirtioSocketDeviceConfiguration::new().into_super()]));
            c.setEntropyDevices(&NSArray::from_retained_slice(&[VZVirtioEntropyDeviceConfiguration::new().into_super()]));

            // Shares mount in the guest at "/Volumes/My Shared Files/<name>".
            let share = |path: &std::path::Path, read_only: bool| -> Result<Retained<VZSharedDirectory>> {
                Ok(VZSharedDirectory::initWithURL_readOnly(VZSharedDirectory::alloc(), &*url(path)?, read_only))
            };
            let names = [
                NSString::from_str("host-home"),
                NSString::from_str("workspace"),
                NSString::from_str("tools"),
            ];
            let dirs = [
                share(&home, true)?,
                share(&paths.workspace, false)?,
                share(&paths.tools, true)?,
            ];
            let name_refs: Vec<&NSString> = names.iter().map(|n| &**n).collect();
            let dir_refs: Vec<&VZSharedDirectory> = dirs.iter().map(|d| &**d).collect();
            let multi = VZMultipleDirectoryShare::initWithDirectories(
                VZMultipleDirectoryShare::alloc(),
                &NSDictionary::from_slices(&name_refs, &dir_refs),
            );
            let fs = VZVirtioFileSystemDeviceConfiguration::initWithTag(
                VZVirtioFileSystemDeviceConfiguration::alloc(),
                &VZVirtioFileSystemDeviceConfiguration::macOSGuestAutomountTag(),
            );
            fs.setShare(Some(&multi));
            c.setDirectorySharingDevices(&NSArray::from_retained_slice(&[fs.into_super()]));

            c.validateWithError().map_err(|e| ns_error(&e))?;
            Ok(c)
        }
    };
    objc2::exception::catch(std::panic::AssertUnwindSafe(build))
        .map_err(|e| anyhow!("Virtualization.framework exception: {e:?}"))?
}
