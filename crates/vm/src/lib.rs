//! The agent Mac: a macOS guest VM that runs on Apple Virtualization.framework.
//!
//! All `VZVirtualMachine` calls run on the main dispatch queue. This is necessary
//! for `VZVirtualMachineView`, and the GPUI run loop serves the main queue. The
//! VM object itself lives in a main-thread `thread_local`.

mod config;
mod install;
mod window;

use std::cell::RefCell;
use std::io::{BufReader, Write};
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_foundation::NSError;
use objc2_virtualization::{
    VZVirtioSocketConnection, VZVirtioSocketDevice, VZVirtualMachine, VZVirtualMachineState,
};
use protocol::{Op, Reply, Request, Response, ResponseBody};

pub use window::show_window;

/// The URL of the latest macOS restore image that this Mac supports.
pub fn latest_restore_image_url() -> Result<String> {
    install::fetch_latest_url()
}

#[derive(Debug, Clone, PartialEq)]
pub enum VmPhase {
    NotInstalled,
    Downloading { fraction: f64 },
    Installing { fraction: f64 },
    Stopped,
    Starting,
    Running,
    Stopping,
    Error(String),
}

impl VmPhase {
    pub fn label(&self) -> String {
        match self {
            VmPhase::NotInstalled => "Not installed".into(),
            VmPhase::Downloading { fraction } => format!("Downloading macOS {:.0}%", fraction * 100.0),
            VmPhase::Installing { fraction } => format!("Installing macOS {:.0}%", fraction * 100.0),
            VmPhase::Stopped => "Stopped".into(),
            VmPhase::Starting => "Starting".into(),
            VmPhase::Running => "Running".into(),
            VmPhase::Stopping => "Stopping".into(),
            VmPhase::Error(e) => format!("Error: {e}"),
        }
    }
}

/// A handle to the agent Mac. Clone it freely; all clones share one VM.
#[derive(Clone)]
pub struct AgentMac {
    inner: Arc<Inner>,
}

struct Inner {
    paths: Paths,
    phase: Mutex<VmPhase>,
}

#[derive(Clone, Debug)]
pub struct Paths {
    /// The VM bundle: disk, auxiliary storage, hardware model, machine ID.
    pub bundle: PathBuf,
    /// Read-write folder that the host and the VM share.
    pub workspace: PathBuf,
    /// Read-only folder with the guest agent and its install script.
    pub tools: PathBuf,
}

impl Paths {
    pub fn under(app_support: &Path) -> Self {
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        Self {
            bundle: app_support.join("vm"),
            workspace: home.join("AgentDesk/workspace"),
            tools: app_support.join("tools"),
        }
    }
    fn disk(&self) -> PathBuf {
        self.bundle.join("Disk.img")
    }
    fn aux(&self) -> PathBuf {
        self.bundle.join("AuxiliaryStorage")
    }
    fn hardware_model(&self) -> PathBuf {
        self.bundle.join("HardwareModel")
    }
    fn machine_id(&self) -> PathBuf {
        self.bundle.join("MachineIdentifier")
    }
    fn restore_image(&self) -> PathBuf {
        self.bundle.join("RestoreImage.ipsw")
    }
    fn installed_marker(&self) -> PathBuf {
        self.bundle.join(".installed")
    }
}

thread_local! {
    /// The running VM. Only the main thread touches it.
    static VM: RefCell<Option<Retained<VZVirtualMachine>>> = const { RefCell::new(None) };
}

/// Run `f` on the main thread and wait for its result.
pub(crate) fn on_main<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    if objc2::MainThreadMarker::new().is_some() {
        return f();
    }
    let (tx, rx) = mpsc::channel();
    DispatchQueue::main().exec_async(move || {
        let _ = tx.send(f());
    });
    rx.recv().expect("main queue dropped the task")
}

pub(crate) fn ns_error(e: &NSError) -> anyhow::Error {
    anyhow!("{}", e.localizedDescription())
}

/// A one-shot completion block that reports `Result<(), String>` on a channel.
pub(crate) fn completion_block() -> (RcBlock<dyn Fn(*mut NSError)>, mpsc::Receiver<Result<(), String>>) {
    let (tx, rx) = mpsc::channel();
    let tx = std::cell::Cell::new(Some(tx));
    let block = RcBlock::new(move |err: *mut NSError| {
        let result = if err.is_null() {
            Ok(())
        } else {
            Err(unsafe { &*err }.localizedDescription().to_string())
        };
        if let Some(tx) = tx.take() {
            let _ = tx.send(result);
        }
    });
    (block, rx)
}

impl AgentMac {
    pub fn new(paths: Paths) -> Self {
        let phase = if paths.installed_marker().exists() {
            VmPhase::Stopped
        } else {
            VmPhase::NotInstalled
        };
        Self {
            inner: Arc::new(Inner {
                paths,
                phase: Mutex::new(phase),
            }),
        }
    }

    pub fn paths(&self) -> &Paths {
        &self.inner.paths
    }

    pub fn phase(&self) -> VmPhase {
        self.inner.phase.lock().unwrap().clone()
    }

    pub(crate) fn set_phase(&self, phase: VmPhase) {
        *self.inner.phase.lock().unwrap() = phase;
    }

    pub fn is_supported() -> bool {
        unsafe { VZVirtualMachine::isSupported() }
    }

    /// Download macOS and install it into a new VM bundle. This runs on a
    /// background thread and reports progress through [`AgentMac::phase`].
    pub fn install(&self) {
        let this = self.clone();
        std::thread::spawn(move || {
            if let Err(e) = install::run(&this) {
                tracing::error!("install failed: {e:#}");
                this.set_phase(VmPhase::Error(format!("{e:#}")));
            }
        });
    }

    /// Start the VM. Returns when the VM runs.
    pub fn start(&self) -> Result<()> {
        if !self.paths().installed_marker().exists() {
            bail!("the agent Mac is not installed");
        }
        self.set_phase(VmPhase::Starting);
        let paths = self.paths().clone();
        let rx = on_main(move || -> Result<mpsc::Receiver<Result<(), String>>, String> {
            let cfg = config::build(&paths, None).map_err(|e| format!("{e:#}"))?;
            let vm = unsafe { VZVirtualMachine::initWithConfiguration(VZVirtualMachine::alloc(), &cfg) };
            let (block, rx) = completion_block();
            unsafe { vm.startWithCompletionHandler(&block) };
            VM.with(|slot| *slot.borrow_mut() = Some(vm));
            Ok(rx)
        });
        let result = match rx {
            Ok(rx) => rx
                .recv_timeout(Duration::from_secs(60))
                .map_err(|_| "start timed out".to_string())
                .and_then(|r| r),
            Err(e) => Err(e),
        };
        match result {
            Ok(()) => {
                self.set_phase(VmPhase::Running);
                self.watch_state();
                Ok(())
            }
            Err(e) => {
                self.set_phase(VmPhase::Error(e.clone()));
                Err(anyhow!(e))
            }
        }
    }

    /// Ask the guest to shut down, and force a stop after 30 seconds.
    pub fn stop(&self) -> Result<()> {
        self.set_phase(VmPhase::Stopping);
        let asked = on_main(|| {
            VM.with(|slot| match slot.borrow().as_ref() {
                Some(vm) => unsafe { vm.requestStopWithError() }.is_ok(),
                None => false,
            })
        });
        if asked {
            for _ in 0..60 {
                if self.vm_state() == Some(VZVirtualMachineState::Stopped) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        if self.vm_state().is_some_and(|s| s != VZVirtualMachineState::Stopped) {
            let rx = on_main(|| {
                VM.with(|slot| {
                    slot.borrow().as_ref().map(|vm| {
                        let (block, rx) = completion_block();
                        unsafe { vm.stopWithCompletionHandler(&block) };
                        rx
                    })
                })
            });
            if let Some(rx) = rx {
                let _ = rx.recv_timeout(Duration::from_secs(20));
            }
        }
        on_main(|| VM.with(|slot| slot.borrow_mut().take()).map(|_| ()));
        self.set_phase(VmPhase::Stopped);
        Ok(())
    }

    fn vm_state(&self) -> Option<VZVirtualMachineState> {
        on_main(|| VM.with(|slot| slot.borrow().as_ref().map(|vm| unsafe { vm.state() })))
    }

    /// Follow the VM state, so that a shutdown from inside the guest shows in the UI.
    fn watch_state(&self) {
        let this = self.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                match this.vm_state() {
                    Some(VZVirtualMachineState::Running) => continue,
                    Some(VZVirtualMachineState::Error) => {
                        this.set_phase(VmPhase::Error("the VM stopped with an error".into()));
                    }
                    _ => {
                        if this.phase() == VmPhase::Running {
                            this.set_phase(VmPhase::Stopped);
                        }
                    }
                }
                on_main(|| VM.with(|slot| slot.borrow_mut().take()).map(|_| ()));
                return;
            }
        });
    }

    /// Open a vsock stream to a port in the guest.
    pub fn connect(&self, port: u32) -> Result<UnixStream> {
        let (tx, rx) = mpsc::channel::<Result<UnixStream, String>>();
        let started = on_main(move || {
            VM.with(|slot| {
                let slot = slot.borrow();
                let Some(vm) = slot.as_ref() else { return false };
                let devices = unsafe { vm.socketDevices() };
                if devices.count() == 0 {
                    return false;
                }
                let Ok(device) = devices.objectAtIndex(0).downcast::<VZVirtioSocketDevice>() else {
                    return false;
                };
                let tx = std::cell::Cell::new(Some(tx));
                let block = RcBlock::new(move |conn: *mut VZVirtioSocketConnection, err: *mut NSError| {
                    let result = if !err.is_null() {
                        Err(unsafe { &*err }.localizedDescription().to_string())
                    } else {
                        let conn = unsafe { &*conn };
                        let fd = unsafe { libc::dup(conn.fileDescriptor()) };
                        unsafe { conn.close() };
                        if fd < 0 {
                            Err("dup failed".into())
                        } else {
                            Ok(unsafe { UnixStream::from_raw_fd(fd) })
                        }
                    };
                    if let Some(tx) = tx.take() {
                        let _ = tx.send(result);
                    }
                });
                unsafe { device.connectToPort_completionHandler(port, &block) };
                true
            })
        });
        if !started {
            bail!("the agent Mac is not running. Start it in Agent Desk first.");
        }
        rx.recv_timeout(Duration::from_secs(5))
            .map_err(|_| {
                anyhow!(
                    "the guest agent did not answer on vsock port {port}. \
                     Install agent-desk-guest in the VM (see the setup steps)."
                )
            })?
            .map_err(|e| anyhow!(e))
    }

    /// Send one request to the guest agent and wait for its response.
    pub fn request(&self, op: Op) -> Result<Reply> {
        let timeout = match &op {
            Op::Shell { timeout_secs, .. } => timeout_secs.unwrap_or(120) + 10,
            _ => 60,
        };
        let mut stream = self.connect(protocol::GUEST_AGENT_PORT)?;
        stream.set_read_timeout(Some(Duration::from_secs(timeout)))?;
        protocol::write_msg(&mut stream, &Request { id: 1, op })?;
        stream.flush()?;
        let mut reader = BufReader::new(stream);
        let resp: Response = protocol::read_msg(&mut reader)?.context("the guest agent closed the connection")?;
        match resp.body {
            ResponseBody::Ok(reply) => Ok(reply),
            ResponseBody::Error(e) => Err(anyhow!(e)),
        }
    }
}
