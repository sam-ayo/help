//! A test laptop for the interop test of the PWA (`pwa/test/interop.test.ts`). The test starts
//! this program, connects to it as a phone with the TypeScript code, and checks the replies.
//!
//! Each line is base64url text. A line with more than one chunk has a `,` between the chunks.
//!
//! 1. This program writes the public key of the laptop.
//! 2. It reads the phone hello, and writes the laptop hello.
//! 3. For each message from the phone, it writes the replies, one line for each reply:
//!    - `prompt`: `accepted`, then an `agent_text` event with "echo: " and the text.
//!    - `sync`: an `agent_text` event with 200000 "x" characters, in more than one chunk.
//!    - `cancel`: `status` with `busy: false`.

use std::io::{BufRead, Write};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use remote::message::{Event, LaptopHello, PhoneHello, ToLaptop, ToPhone, VERSION};
use remote::noise::{self, Keypair};

fn main() -> anyhow::Result<()> {
    let laptop = Keypair::generate()?;
    let mut out = std::io::stdout().lock();
    let mut lines = std::io::stdin().lock().lines();
    writeln!(out, "{}", B64.encode(laptop.public))?;
    out.flush()?;

    let first = B64.decode(lines.next().unwrap()?)?;
    let hello = noise::accept(&laptop, &first)?;
    let phone: PhoneHello = serde_json::from_slice(&hello.payload)?;
    let reply = LaptopHello {
        v: VERSION,
        accepted: phone.v == VERSION,
        name: format!("Test laptop for {}", phone.device),
        reason: phone.pair.map(|code| format!("code {code}")),
    };
    let (second, mut transport) = hello.reply(&serde_json::to_vec(&reply)?)?;
    writeln!(out, "{}", B64.encode(second))?;
    out.flush()?;

    for line in lines {
        let line = line?;
        let mut message = None;
        for chunk in line.split(',') {
            message = transport.open(&B64.decode(chunk)?)?;
        }
        let Some(message) = message else { anyhow::bail!("the line has no last chunk") };
        let replies = match serde_json::from_slice::<ToLaptop>(&message)? {
            ToLaptop::Prompt { id, text } => vec![
                ToPhone::Accepted { id },
                ToPhone::Event { seq: 1, event: Event::AgentText { text: format!("echo: {text}") } },
            ],
            ToLaptop::Sync { after } => {
                vec![ToPhone::Event { seq: after + 1, event: Event::AgentText { text: "x".repeat(200_000) } }]
            }
            ToLaptop::Cancel => vec![ToPhone::Status { agent: "Claude Code".into(), busy: false, queued: 0 }],
            ToLaptop::Permission { .. } => vec![],
        };
        for reply in replies {
            let chunks = transport.seal(&serde_json::to_vec(&reply)?)?;
            let encoded: Vec<String> = chunks.iter().map(|c| B64.encode(c)).collect();
            writeln!(out, "{}", encoded.join(","))?;
        }
        out.flush()?;
    }
    Ok(())
}
