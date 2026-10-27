//! The end-to-end encryption between a phone and the laptop. The relay passes the messages,
//! but it cannot read them.
//!
//! The pattern is `Noise_IK_25519_ChaChaPoly_SHA256`. The phone is the initiator. It knows the
//! static key of the laptop from the pairing link, and it sends its own static key in the
//! first message:
//!
//! ```text
//! <- s
//! ...
//! -> e, es, s, ss     phone hello (see `message::PhoneHello`)
//! <- e, ee, se        laptop hello (see `message::LaptopHello`)
//! ```
//!
//! After the handshake, each message goes in one or more chunks, because a Noise message can
//! hold at most 65535 bytes. The first byte of the plaintext of a chunk is `MORE` or `LAST`.
//! The PWA has the same code in `pwa/src/noise.ts`.

use anyhow::{Context, Result, bail};

pub const PATTERN: &str = "Noise_IK_25519_ChaChaPoly_SHA256";
/// Both sides mix the prologue into the handshake. A peer with a different prologue fails the
/// handshake.
pub const PROLOGUE: &[u8] = b"help remote 1";

const MAX_MESSAGE: usize = 65535;
const TAG: usize = 16;
const MAX_CHUNK: usize = MAX_MESSAGE - TAG - 1;
/// The largest message after the chunks join. A peer cannot fill the memory of the other peer.
pub const MAX_PLAINTEXT: usize = 16 * 1024 * 1024;
const MORE: u8 = 0;
const LAST: u8 = 1;

/// A static X25519 key pair: the identity of a laptop or of a phone.
#[derive(Clone)]
pub struct Keypair {
    pub public: [u8; 32],
    pub private: [u8; 32],
}

impl std::fmt::Debug for Keypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keypair").field("public", &self.public).finish_non_exhaustive()
    }
}

impl Keypair {
    pub fn generate() -> Result<Self> {
        let pair = builder().generate_keypair()?;
        Ok(Self {
            public: pair.public.try_into().map_err(|_| anyhow::anyhow!("bad public key"))?,
            private: pair.private.try_into().map_err(|_| anyhow::anyhow!("bad private key"))?,
        })
    }
}

fn builder() -> snow::Builder<'static> {
    snow::Builder::new(PATTERN.parse().expect("valid Noise pattern"))
}

/// The first message of a phone, after the laptop reads it. Examine `phone` and `payload`, then
/// send the `reply`.
pub struct Hello {
    /// The static public key of the phone.
    pub phone: [u8; 32],
    pub payload: Vec<u8>,
    state: snow::HandshakeState,
}

/// Read the first message of a phone on the laptop.
pub fn accept(laptop: &Keypair, message: &[u8]) -> Result<Hello> {
    let mut state = builder().local_private_key(&laptop.private)?.prologue(PROLOGUE)?.build_responder()?;
    let mut payload = vec![0; MAX_MESSAGE];
    let n = state.read_message(message, &mut payload).context("bad phone hello")?;
    payload.truncate(n);
    let phone = state
        .get_remote_static()
        .context("the phone hello has no static key")?
        .try_into()
        .context("bad phone key")?;
    Ok(Hello { phone, payload, state })
}

impl Hello {
    /// Make the reply of the laptop. After the reply, both sides can send messages.
    pub fn reply(mut self, payload: &[u8]) -> Result<(Vec<u8>, Transport)> {
        let mut out = vec![0; MAX_MESSAGE];
        let n = self.state.write_message(payload, &mut out)?;
        out.truncate(n);
        Ok((out, Transport::new(self.state.into_transport_mode()?)))
    }
}

/// A phone that sent its first message and waits for the reply of the laptop.
pub struct Dial {
    state: snow::HandshakeState,
}

/// Make the first message of a phone. The PWA does this in TypeScript. The Rust version is for
/// the tests, and for a native app later.
pub fn dial(phone: &Keypair, laptop: &[u8; 32], payload: &[u8]) -> Result<(Vec<u8>, Dial)> {
    let mut state = builder()
        .local_private_key(&phone.private)?
        .remote_public_key(laptop)?
        .prologue(PROLOGUE)?
        .build_initiator()?;
    let mut out = vec![0; MAX_MESSAGE];
    let n = state.write_message(payload, &mut out)?;
    out.truncate(n);
    Ok((out, Dial { state }))
}

impl Dial {
    /// Read the reply of the laptop. Returns its payload.
    pub fn finish(mut self, message: &[u8]) -> Result<(Vec<u8>, Transport)> {
        let mut payload = vec![0; MAX_MESSAGE];
        let n = self.state.read_message(message, &mut payload).context("bad laptop hello")?;
        payload.truncate(n);
        Ok((payload, Transport::new(self.state.into_transport_mode()?)))
    }
}

/// An encrypted channel after the handshake. The messages must arrive in the order that the
/// peer sent them. A lost, changed, or repeated message makes `open` fail. Then close the
/// connection.
pub struct Transport {
    state: snow::TransportState,
    partial: Vec<u8>,
}

impl Transport {
    fn new(state: snow::TransportState) -> Self {
        Self { state, partial: Vec::new() }
    }

    /// Encrypt a message. Send each chunk as one WebSocket message, in order.
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<Vec<u8>>> {
        if plaintext.len() > MAX_PLAINTEXT {
            bail!("the message has {} bytes, more than {MAX_PLAINTEXT}", plaintext.len());
        }
        let pieces: Vec<&[u8]> =
            if plaintext.is_empty() { vec![&[]] } else { plaintext.chunks(MAX_CHUNK).collect() };
        let last = pieces.len() - 1;
        let mut chunk = Vec::with_capacity(MAX_CHUNK + 1);
        pieces
            .into_iter()
            .enumerate()
            .map(|(i, piece)| {
                chunk.clear();
                chunk.push(if i == last { LAST } else { MORE });
                chunk.extend_from_slice(piece);
                let mut out = vec![0; chunk.len() + TAG];
                let n = self.state.write_message(&chunk, &mut out)?;
                out.truncate(n);
                Ok(out)
            })
            .collect()
    }

    /// Decrypt a chunk. Returns the full message after its last chunk, and `None` before.
    pub fn open(&mut self, chunk: &[u8]) -> Result<Option<Vec<u8>>> {
        let mut plain = vec![0; chunk.len()];
        let n = self.state.read_message(chunk, &mut plain).context("cannot decrypt the message")?;
        let (&flag, data) = plain[..n].split_first().context("empty chunk")?;
        if self.partial.len() + data.len() > MAX_PLAINTEXT {
            bail!("the message has more than {MAX_PLAINTEXT} bytes");
        }
        self.partial.extend_from_slice(data);
        match flag {
            MORE => Ok(None),
            LAST => Ok(Some(std::mem::take(&mut self.partial))),
            _ => bail!("bad chunk flag {flag}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (Transport, Transport) {
        let laptop = Keypair::generate().unwrap();
        let phone = Keypair::generate().unwrap();
        let (first, dial) = dial(&phone, &laptop.public, b"hi laptop").unwrap();
        let hello = accept(&laptop, &first).unwrap();
        assert_eq!(hello.phone, phone.public);
        assert_eq!(hello.payload, b"hi laptop");
        let (second, laptop_side) = hello.reply(b"hi phone").unwrap();
        let (payload, phone_side) = dial.finish(&second).unwrap();
        assert_eq!(payload, b"hi phone");
        (phone_side, laptop_side)
    }

    fn send(from: &mut Transport, to: &mut Transport, message: &[u8]) -> Vec<u8> {
        receive(to, &from.seal(message).unwrap())
    }

    fn receive(to: &mut Transport, chunks: &[Vec<u8>]) -> Vec<u8> {
        let (last, rest) = chunks.split_last().unwrap();
        for chunk in rest {
            assert_eq!(to.open(chunk).unwrap(), None);
        }
        to.open(last).unwrap().unwrap()
    }

    #[test]
    fn both_directions() {
        let (mut phone, mut laptop) = pair();
        assert_eq!(send(&mut phone, &mut laptop, b"prompt"), b"prompt");
        assert_eq!(send(&mut laptop, &mut phone, b"answer"), b"answer");
        assert_eq!(send(&mut laptop, &mut phone, b""), b"");
    }

    #[test]
    fn large_message_in_chunks() {
        let (mut phone, mut laptop) = pair();
        let big: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        let chunks = laptop.seal(&big).unwrap();
        assert_eq!(chunks.len(), 4);
        assert_eq!(receive(&mut phone, &chunks), big);
    }

    #[test]
    fn wrong_laptop_key_fails() {
        let laptop = Keypair::generate().unwrap();
        let other = Keypair::generate().unwrap();
        let phone = Keypair::generate().unwrap();
        let (first, _) = dial(&phone, &other.public, b"").unwrap();
        assert!(accept(&laptop, &first).is_err());
    }

    #[test]
    fn changed_message_fails() {
        let (mut phone, mut laptop) = pair();
        let mut chunk = phone.seal(b"prompt").unwrap().remove(0);
        chunk[3] ^= 1;
        assert!(laptop.open(&chunk).is_err());
    }

    #[test]
    fn repeated_message_fails() {
        let (mut phone, mut laptop) = pair();
        let chunk = phone.seal(b"prompt").unwrap().remove(0);
        laptop.open(&chunk).unwrap();
        assert!(laptop.open(&chunk).is_err());
    }
}
