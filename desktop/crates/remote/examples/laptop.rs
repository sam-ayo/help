//! A laptop that uses the real service, without the app. It connects to the relay, prints a
//! pairing link, and answers each prompt with an echo. Use it to test the PWA:
//!
//!     cargo run -p remote --example laptop -- ws://localhost:8787 http://localhost:5173 /tmp/laptop.json

use remote::message::Event;
use remote::service::{self, Config, FromService, ToService};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [relay, pwa, file] = args.as_slice() else {
        anyhow::bail!("usage: laptop <relay> <pwa> <state file>");
    };
    let config = Config { relay: relay.clone(), pwa: pwa.clone(), state_file: file.into(), name: "Example laptop".into() };
    let (service, mut events) = service::start(&tokio::runtime::Handle::current(), config);
    service.send(ToService::Status { agent: "Echo".into(), busy: false, queued: 0 });
    service.send(ToService::StartPairing);
    let mut shown = None;
    while let Some(event) = events.recv().await {
        match event {
            FromService::State(state) => {
                if state.pairing.is_some() && state.pairing != shown {
                    println!("link {}", state.pairing.clone().unwrap_or_default());
                    shown = state.pairing.clone();
                }
                println!("state online={} devices={} connected={} error={:?}", state.online, state.devices.len(), state.connected.len(), state.error);
            }
            FromService::Prompt { text } => {
                println!("prompt {text}");
                service.send(ToService::Event(Event::UserText { text: text.clone() }));
                service.send(ToService::Event(Event::AgentText { text: format!("echo: {text}") }));
                service.send(ToService::Event(Event::TurnEnded { reason: "end_turn".into() }));
            }
            other => println!("{other:?}"),
        }
    }
    Ok(())
}
