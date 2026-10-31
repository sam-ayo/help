//! The laptop side of the remote control. The service keeps one WebSocket to the relay, pairs
//! phones, does the Noise handshake with each phone, and sends the events of the conversation
//! to the phones. The messages from the phones go to the app. See `relay/src/index.ts` for the
//! frames and the control messages of the relay.
//!
//! The app gives the events to the service. The service keeps the newest events in a log, so a
//! phone that connects again gets the events that it did not get.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use futures_util::{SinkExt as _, StreamExt as _};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

use crate::message::{Event, LaptopHello, PhoneHello, ToLaptop, ToPhone, VERSION};
use crate::noise::{self, Keypair, Transport};
use crate::pairing::{CODE_LIFETIME, PairingLink, random_token};

/// The most events in the log. A phone that missed more events gets a `Reset`.
const LOG: usize = 5000;
/// The prompt IDs that the service remembers, to ignore a prompt that a phone sends again.
const PROMPT_IDS: usize = 200;
const PING: Duration = Duration::from_secs(15);
/// The relay must send a message (a pong at least) in this time, or the service connects again.
/// A connection can stop without a close, for example when the Wi-Fi network changes.
const SILENCE: Duration = Duration::from_secs(45);
/// The interval of the check for a sleep of the Mac. See `Clock`.
const SLEEP_CHECK: Duration = Duration::from_secs(2);
const MAX_DELAY: Duration = Duration::from_secs(60);

/// The relay close codes. See `Close` in `relay/src/index.ts`.
const REPLACED: u16 = 4000;
const BAD_SECRET: u16 = 4401;

#[derive(Debug, Clone)]
pub struct Config {
    /// The WebSocket address of the relay, for example `wss://help-relay.example.com`.
    pub relay: String,
    /// The address of the PWA, for the pairing link.
    pub pwa: String,
    /// The file with the identity of the laptop and the paired phones.
    pub state_file: PathBuf,
    /// The name of the laptop that the phones show.
    pub name: String,
}

/// A paired phone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// The static public key of the phone, in base64url.
    pub key: String,
    /// The time of the pairing, in seconds since 1970.
    pub paired_at: i64,
}

/// What the app gives to the service.
#[derive(Debug)]
pub enum ToService {
    Event(Event),
    Status { agent: String, busy: bool, queued: u32 },
    /// The conversation changed completely, for example to a different thread. The phones clear
    /// their view and get these events.
    Reset(Vec<Event>),
    /// Make a new pairing link, and let a new phone connect for `CODE_LIFETIME`.
    StartPairing,
    StopPairing,
    /// Remove a paired phone. Its connection closes.
    Remove(String),
}

/// What the service gives to the app.
#[derive(Debug)]
pub enum FromService {
    Prompt { text: String },
    Cancel,
    Permission { request: u64, option: Option<String> },
    State(State),
}

/// The state of the service, for the app to show.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    /// The service has a connection to the relay.
    pub online: bool,
    pub devices: Vec<Device>,
    /// The IDs of the phones that are connected now, after the handshake.
    pub connected: Vec<String>,
    /// The pairing link, while a new phone can pair.
    pub pairing: Option<String>,
    pub error: Option<String>,
}

/// The connection of the app to the service. Drop it to stop the service.
pub struct Service {
    tx: mpsc::UnboundedSender<ToService>,
}

impl Service {
    pub fn send(&self, message: ToService) {
        let _ = self.tx.send(message);
    }
}

/// Start the service on `runtime`.
pub fn start(runtime: &tokio::runtime::Handle, config: Config) -> (Service, mpsc::UnboundedReceiver<FromService>) {
    let (tx, commands) = mpsc::unbounded_channel();
    let (out, rx) = mpsc::unbounded_channel();
    runtime.spawn(async move {
        match Laptop::load(config, out.clone()) {
            Ok(laptop) => laptop.run(commands).await,
            Err(e) => {
                tracing::warn!("the remote control cannot start: {e:#}");
                let state = State { error: Some(format!("{e:#}")), ..State::default() };
                let _ = out.send(FromService::State(state));
            }
        }
    });
    (Service { tx }, rx)
}

/// The file of the service.
#[derive(Serialize, Deserialize)]
struct Saved {
    laptop: String,
    secret: String,
    /// The key pair of the laptop, in base64url.
    public: String,
    private: String,
    devices: Vec<Device>,
}

struct Phone {
    transport: Option<Transport>,
}

struct Pairing {
    code: String,
    link: String,
    until: Instant,
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Laptop {
    config: Config,
    saved: Saved,
    key: Keypair,
    out: mpsc::UnboundedSender<FromService>,
    phones: HashMap<String, Phone>,
    pairing: Option<Pairing>,
    /// The events, with the `seq` of the first one. The first `seq` of a run is the time of the
    /// start, so a phone with the events of an earlier run gets a `Reset`.
    log: VecDeque<Event>,
    first: u64,
    status: ToPhone,
    prompt_ids: VecDeque<String>,
    online: bool,
    error: Option<String>,
    /// The frames for the relay, while the service is connected.
    frames: Vec<Message>,
}

impl Laptop {
    fn load(config: Config, out: mpsc::UnboundedSender<FromService>) -> Result<Self> {
        let saved = match std::fs::read(&config.state_file) {
            Ok(bytes) => serde_json::from_slice::<Saved>(&bytes).context("cannot read the remote control file")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = Keypair::generate()?;
                let saved = Saved {
                    laptop: random_token()?,
                    secret: random_token()? + &random_token()?,
                    public: B64.encode(key.public),
                    private: B64.encode(key.private),
                    devices: Vec::new(),
                };
                write_private(&config.state_file, &serde_json::to_vec_pretty(&saved)?)?;
                saved
            }
            Err(e) => return Err(e).context("cannot read the remote control file"),
        };
        let bytes = |text: &str| -> Result<[u8; 32]> {
            B64.decode(text)?.try_into().map_err(|_| anyhow::anyhow!("the laptop key must have 32 bytes"))
        };
        let key = Keypair { public: bytes(&saved.public)?, private: bytes(&saved.private)? };
        let start = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        Ok(Self {
            config,
            saved,
            key,
            out,
            phones: HashMap::new(),
            pairing: None,
            log: VecDeque::new(),
            first: start.as_millis() as u64 * 1000 + 1,
            status: ToPhone::Status { agent: String::new(), busy: false, queued: 0 },
            prompt_ids: VecDeque::new(),
            online: false,
            error: None,
            frames: Vec::new(),
        })
    }

    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<ToService>) {
        self.publish();
        let mut delay = Duration::from_secs(1);
        let mut clock = Clock::new();
        loop {
            match self.connect().await {
                Ok(ws) => {
                    delay = Duration::from_secs(1);
                    self.online = true;
                    self.error = None;
                    self.publish();
                    let result = self.serve(ws, &mut commands, &mut clock).await;
                    self.online = false;
                    self.phones.clear();
                    match result {
                        Ok(None) => return,
                        Ok(Some(code)) => self.closed(code, &mut delay),
                        Err(e) => {
                            tracing::info!("the relay connection stopped: {e:#}");
                            self.error = Some(format!("The connection to the relay stopped: {e}"));
                        }
                    }
                }
                Err(e) => {
                    tracing::info!("cannot connect to the relay: {e:#}");
                    self.error = Some(format!("Cannot connect to the relay: {e}"));
                }
            }
            self.publish();
            // Do the commands of the app while the service waits to connect again.
            let sleep = tokio::time::sleep(delay);
            tokio::pin!(sleep);
            let mut check = tokio::time::interval(SLEEP_CHECK);
            let mut woke = false;
            loop {
                tokio::select! {
                    _ = &mut sleep => break,
                    command = commands.recv() => match command {
                        Some(command) => self.command(command),
                        None => return,
                    },
                    // After a sleep, the network is back soon. Do not wait for the rest of the delay.
                    _ = check.tick() => if clock.slept() {
                        woke = true;
                        break;
                    },
                }
            }
            self.frames.clear();
            delay = if woke { Duration::from_secs(1) } else { (delay * 2).min(MAX_DELAY) };
        }
    }

    fn closed(&mut self, code: u16, delay: &mut Duration) {
        self.error = Some(match code {
            REPLACED => "A different copy of the app connected to the relay with this laptop ID.".into(),
            BAD_SECRET => "The relay refused the secret of this laptop.".into(),
            _ => format!("The relay closed the connection ({code})."),
        });
        if code == REPLACED || code == BAD_SECRET {
            *delay = MAX_DELAY;
        }
    }

    async fn connect(&self) -> Result<Socket> {
        let url = format!("{}/v1/laptop/{}", self.config.relay.trim_end_matches('/'), self.saved.laptop);
        let mut request = url.into_client_request()?;
        request.headers_mut().insert("Authorization", format!("Bearer {}", self.saved.secret).parse()?);
        let (ws, _) = tokio::time::timeout(Duration::from_secs(20), tokio_tungstenite::connect_async(request))
            .await
            .context("timed out")??;
        Ok(ws)
    }

    /// Serve one relay connection. Returns `None` when the app stops the service, and the close
    /// code when the relay closes the connection.
    async fn serve(
        &mut self,
        mut ws: Socket,
        commands: &mut mpsc::UnboundedReceiver<ToService>,
        clock: &mut Clock,
    ) -> Result<Option<u16>> {
        self.control(serde_json::json!({ "type": "devices", "ids": self.device_ids() }));
        if let Some(p) = &self.pairing {
            let seconds = p.until.saturating_duration_since(Instant::now()).as_secs();
            self.control(serde_json::json!({ "type": "pairing", "seconds": seconds }));
        }
        let mut ping = tokio::time::interval(PING);
        let mut expiry = tokio::time::interval(Duration::from_secs(5));
        let mut check = tokio::time::interval(SLEEP_CHECK);
        clock.slept();
        let mut heard = Instant::now();
        loop {
            for frame in self.frames.drain(..) {
                ws.feed(frame).await?;
            }
            ws.flush().await?;
            tokio::select! {
                message = ws.next() => match message {
                    None => return Ok(Some(1006)),
                    Some(message) => match message.inspect(|_| heard = Instant::now())? {
                        Message::Binary(bytes) => self.frame(&bytes),
                        Message::Text(text) => self.relay_control(&text),
                        Message::Close(frame) => {
                            return Ok(Some(frame.map_or(1005, |f| u16::from(f.code))));
                        }
                        _ => {}
                    },
                },
                command = commands.recv() => match command {
                    Some(command) => self.command(command),
                    None => {
                        let _ = ws.close(None).await;
                        return Ok(None);
                    }
                },
                _ = ping.tick() => {
                    if heard.elapsed() > SILENCE {
                        bail!("the relay did not answer for {} seconds", SILENCE.as_secs());
                    }
                    self.frames.push(Message::Ping(Vec::new().into()));
                }
                // The relay closed the connection while the Mac slept, but the socket does not
                // know it yet. Connect again at once.
                _ = check.tick() => if clock.slept() {
                    bail!("the Mac slept");
                },
                _ = expiry.tick() => {
                    if self.pairing.as_ref().is_some_and(|p| p.until <= Instant::now()) {
                        self.pairing = None;
                        self.publish();
                    }
                }
            }
        }
    }

    fn command(&mut self, command: ToService) {
        match command {
            ToService::Event(event) => {
                self.log.push_back(event.clone());
                if self.log.len() > LOG {
                    self.log.pop_front();
                    self.first += 1;
                }
                let seq = self.first + self.log.len() as u64 - 1;
                self.broadcast(&ToPhone::Event { seq, event });
            }
            ToService::Status { agent, busy, queued } => {
                let status = ToPhone::Status { agent, busy, queued };
                if status != self.status {
                    self.status = status.clone();
                    self.broadcast(&status);
                }
            }
            ToService::Reset(events) => {
                self.first += self.log.len() as u64;
                self.log = events.into_iter().rev().take(LOG).rev().collect();
                let ids: Vec<String> = self.ready_phones();
                for id in ids {
                    self.sync(&id, 0);
                }
            }
            ToService::StartPairing => {
                match random_token() {
                    Ok(code) => {
                        let link = PairingLink::new(&self.config.relay, &self.saved.laptop, &self.key.public, &code)
                            .to_url(&self.config.pwa);
                        self.pairing = Some(Pairing { code, link, until: Instant::now() + CODE_LIFETIME });
                        self.control(serde_json::json!({ "type": "pairing", "seconds": CODE_LIFETIME.as_secs() }));
                    }
                    Err(e) => self.error = Some(format!("{e:#}")),
                }
                self.publish();
            }
            ToService::StopPairing => {
                self.end_pairing();
                self.publish();
            }
            ToService::Remove(id) => {
                self.saved.devices.retain(|d| d.id != id);
                self.save();
                self.phones.remove(&id);
                self.control(serde_json::json!({ "type": "devices", "ids": self.device_ids() }));
                self.publish();
            }
        }
    }

    fn relay_control(&mut self, text: &str) {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum FromRelay {
            Open { device: String },
            Closed { device: String },
        }
        match serde_json::from_str::<FromRelay>(text) {
            // A new connection of a phone starts with a handshake.
            Ok(FromRelay::Open { device }) => {
                self.phones.insert(device, Phone { transport: None });
                self.publish();
            }
            Ok(FromRelay::Closed { device }) => {
                self.phones.remove(&device);
                self.publish();
            }
            Err(e) => tracing::warn!("a bad control message from the relay: {e}"),
        }
    }

    fn frame(&mut self, bytes: &[u8]) {
        let Some((&len, rest)) = bytes.split_first() else { return };
        let len = len as usize;
        if rest.len() < len {
            return;
        }
        let Ok(device) = std::str::from_utf8(&rest[..len]).map(str::to_string) else { return };
        let data = &rest[len..];
        let result = match self.phones.get_mut(&device) {
            None => return,
            Some(Phone { transport: None }) => self.handshake(&device, data),
            Some(Phone { transport: Some(transport) }) => match transport.open(data) {
                Ok(Some(message)) => match serde_json::from_slice::<ToLaptop>(&message) {
                    Ok(message) => {
                        self.message(&device, message);
                        Ok(())
                    }
                    Err(e) => Err(e.into()),
                },
                Ok(None) => Ok(()),
                Err(e) => Err(e),
            },
        };
        if let Err(e) = result {
            tracing::info!("closed the connection of a phone: {e:#}");
            self.phones.remove(&device);
            self.control(serde_json::json!({ "type": "close", "device": device }));
            self.publish();
        }
    }

    fn handshake(&mut self, device: &str, data: &[u8]) -> Result<()> {
        let hello = noise::accept(&self.key, data)?;
        let phone: PhoneHello = serde_json::from_slice(&hello.payload)?;
        let key = B64.encode(hello.phone);
        let known = self.saved.devices.iter().find(|d| d.id == device).map(|d| d.key.clone());
        let refuse = |reason: &str| LaptopHello {
            v: VERSION,
            accepted: false,
            name: self.config.name.clone(),
            reason: Some(reason.into()),
        };
        let reply = if phone.v != VERSION {
            refuse("This version of the app is not the same as the version on the laptop.")
        } else if let Some(known) = &known {
            if *known == key {
                LaptopHello { v: VERSION, accepted: true, name: self.config.name.clone(), reason: None }
            } else {
                refuse("The key of this phone is not the key that the laptop knows. Pair it again.")
            }
        } else {
            let good = self
                .pairing
                .as_ref()
                .is_some_and(|p| p.until > Instant::now() && phone.pair.as_deref() == Some(p.code.as_str()));
            if good {
                LaptopHello { v: VERSION, accepted: true, name: self.config.name.clone(), reason: None }
            } else {
                refuse("The pairing code is not correct, or it expired. Show a new QR code on the laptop.")
            }
        };
        let (message, transport) = hello.reply(&serde_json::to_vec(&reply)?)?;
        self.send_frame(device, &message);
        if !reply.accepted {
            bail!("refused the phone: {}", reply.reason.unwrap_or_default());
        }
        if known.is_none() {
            let paired_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64);
            self.saved.devices.push(Device { id: device.into(), name: phone.device, key, paired_at });
            self.save();
            self.control(serde_json::json!({ "type": "devices", "ids": self.device_ids() }));
            self.end_pairing();
        }
        if let Some(phone) = self.phones.get_mut(device) {
            phone.transport = Some(transport);
        }
        let status = self.status.clone();
        self.send(device, &status);
        self.publish();
        Ok(())
    }

    fn message(&mut self, device: &str, message: ToLaptop) {
        match message {
            ToLaptop::Prompt { id, text } => {
                self.send(device, &ToPhone::Accepted { id: id.clone() });
                if self.prompt_ids.contains(&id) {
                    return;
                }
                self.prompt_ids.push_back(id);
                if self.prompt_ids.len() > PROMPT_IDS {
                    self.prompt_ids.pop_front();
                }
                let _ = self.out.send(FromService::Prompt { text });
            }
            ToLaptop::Cancel => {
                let _ = self.out.send(FromService::Cancel);
            }
            ToLaptop::Permission { request, option } => {
                let _ = self.out.send(FromService::Permission { request, option });
            }
            ToLaptop::Sync { after } => self.sync(device, after),
        }
    }

    /// Send the events after `after` to a phone.
    fn sync(&mut self, device: &str, after: u64) {
        let last = self.first + self.log.len() as u64 - 1;
        let from = if after + 1 < self.first || after > last {
            self.send(device, &ToPhone::Reset { first: self.first });
            self.first
        } else {
            after + 1
        };
        let events: Vec<ToPhone> = (from..=last)
            .map(|seq| ToPhone::Event { seq, event: self.log[(seq - self.first) as usize].clone() })
            .collect();
        for event in events {
            self.send(device, &event);
        }
    }

    fn ready_phones(&self) -> Vec<String> {
        self.phones.iter().filter(|(_, p)| p.transport.is_some()).map(|(id, _)| id.clone()).collect()
    }

    fn broadcast(&mut self, message: &ToPhone) {
        for id in self.ready_phones() {
            self.send(&id, message);
        }
    }

    fn send(&mut self, device: &str, message: &ToPhone) {
        let Some(transport) = self.phones.get_mut(device).and_then(|p| p.transport.as_mut()) else { return };
        let chunks = serde_json::to_vec(message).map_err(anyhow::Error::from).and_then(|m| transport.seal(&m));
        match chunks {
            Ok(chunks) => {
                for chunk in chunks {
                    self.send_frame(device, &chunk);
                }
            }
            Err(e) => tracing::warn!("cannot send a message to a phone: {e:#}"),
        }
    }

    fn send_frame(&mut self, device: &str, data: &[u8]) {
        if !self.online {
            return;
        }
        let mut frame = Vec::with_capacity(1 + device.len() + data.len());
        frame.push(device.len() as u8);
        frame.extend_from_slice(device.as_bytes());
        frame.extend_from_slice(data);
        self.frames.push(Message::Binary(frame.into()));
    }

    fn control(&mut self, value: serde_json::Value) {
        if self.online {
            self.frames.push(Message::Text(value.to_string().into()));
        }
    }

    fn end_pairing(&mut self) {
        if self.pairing.take().is_some() {
            self.control(serde_json::json!({ "type": "pairing", "seconds": 0 }));
        }
    }

    fn device_ids(&self) -> Vec<String> {
        self.saved.devices.iter().map(|d| d.id.clone()).collect()
    }

    fn save(&mut self) {
        let result = serde_json::to_vec_pretty(&self.saved)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| write_private(&self.config.state_file, &bytes));
        if let Err(e) = result {
            tracing::warn!("cannot save the paired phones: {e:#}");
            self.error = Some(format!("Cannot save the paired phones: {e:#}"));
        }
    }

    fn publish(&self) {
        let mut connected = self.ready_phones();
        connected.sort();
        let _ = self.out.send(FromService::State(State {
            online: self.online,
            devices: self.saved.devices.clone(),
            connected,
            pairing: self.pairing.as_ref().map(|p| p.link.clone()),
            error: self.error.clone(),
        }));
    }
}

/// Finds a sleep of the Mac. The monotonic clock of macOS (`Instant`) stops while the Mac
/// sleeps, and the wall clock does not. A large difference between the two shows a sleep.
struct Clock {
    wall: SystemTime,
    mono: Instant,
}

impl Clock {
    /// The smallest difference that is a sleep. A small change of the wall clock is not a sleep.
    const SLEEP: Duration = Duration::from_secs(5);

    fn new() -> Self {
        Self { wall: SystemTime::now(), mono: Instant::now() }
    }

    /// True if the Mac slept since the last call.
    fn slept(&mut self) -> bool {
        let wall = self.wall.elapsed().unwrap_or_default();
        let mono = self.mono.elapsed();
        *self = Self::new();
        wall > mono + Self::SLEEP
    }
}

/// Write a file that only this user can read. It has the private key of the laptop.
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_finds_a_sleep() {
        let mut clock = Clock::new();
        assert!(!clock.slept());
        // The wall clock moved one minute, and the monotonic clock did not: the Mac slept.
        clock.wall -= Duration::from_secs(60);
        assert!(clock.slept());
        assert!(!clock.slept());
    }
}
