//! Command-line control of the agent Mac, for tests without the GUI.
//! Usage: cargo run -p vm --example vmctl -- info | install | ping
//!
//! `install` and `ping` need a main run loop for the main dispatch queue, so this
//! tool runs the work on a thread and serves the main queue with CFRunLoop.

use std::path::PathBuf;

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "info".into());
    let support = PathBuf::from(std::env::var("HOME").unwrap()).join("Library/Application Support/Help");
    let mac = vm::AgentMac::new(vm::Paths::under(&support));
    std::thread::spawn(move || {
        let code = match run(&cmd, &mac) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e:#}");
                1
            }
        };
        std::process::exit(code);
    });
    loop {
        objc2_foundation::NSRunLoop::currentRunLoop().run();
    }
}

fn run(cmd: &str, mac: &vm::AgentMac) -> anyhow::Result<()> {
    match cmd {
        "info" => {
            println!("supported: {}", vm::AgentMac::is_supported());
            println!("phase: {}", mac.phase().label());
            println!("latest restore image: {}", vm::latest_restore_image_url()?);
        }
        "install" => {
            mac.install();
            let mut last = String::new();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let phase = mac.phase();
                if phase.label() != last {
                    last = phase.label();
                    println!("{last}");
                }
                match phase {
                    vm::VmPhase::Stopped => return Ok(()),
                    vm::VmPhase::Error(e) => anyhow::bail!(e),
                    _ => {}
                }
            }
        }
        "ping" => {
            mac.start()?;
            for _ in 0..60 {
                match mac.request(protocol::Op::Ping) {
                    Ok(r) => {
                        println!("{r:?}");
                        break;
                    }
                    Err(e) => {
                        println!("waiting: {e}");
                        std::thread::sleep(std::time::Duration::from_secs(5));
                    }
                }
            }
            mac.stop()?;
        }
        other => anyhow::bail!("unknown command {other}"),
    }
    Ok(())
}
