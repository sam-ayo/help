//! The pairing link. The laptop shows it as a QR code, and the phone opens it in the PWA.
//!
//! The link is `<pwa>/#pair=<data>`. `data` is the base64url text of a JSON `PairingLink`.
//! The data is in the fragment of the URL, so the browser does not send it to the server of
//! the PWA. The PWA reads the link in `pwa/src/pairing.ts`.

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde::{Deserialize, Serialize};

/// The code of a pairing link is valid for this time after the laptop makes it.
pub const CODE_LIFETIME: std::time::Duration = std::time::Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingLink {
    pub v: u32,
    /// The WebSocket address of the relay, for example `wss://help-relay.example.workers.dev`.
    pub relay: String,
    /// The ID of the laptop at the relay.
    pub laptop: String,
    /// The static public key of the laptop, in base64url.
    pub key: String,
    /// The one-time code. The phone sends it in `PhoneHello::pair`.
    pub code: String,
}

impl PairingLink {
    pub fn new(relay: &str, laptop: &str, key: &[u8; 32], code: &str) -> Self {
        Self { v: 1, relay: relay.into(), laptop: laptop.into(), key: B64.encode(key), code: code.into() }
    }

    pub fn to_url(&self, pwa: &str) -> String {
        let data = B64.encode(serde_json::to_vec(self).expect("serializable"));
        format!("{}/#pair={data}", pwa.trim_end_matches('/'))
    }

    pub fn from_url(url: &str) -> Result<Self> {
        let data = url.split_once("#pair=").context("the link has no #pair=")?.1;
        let link: Self = serde_json::from_slice(&B64.decode(data)?)?;
        if link.v != 1 {
            bail!("pairing link version {} is not supported", link.v);
        }
        link.key()?;
        Ok(link)
    }

    pub fn key(&self) -> Result<[u8; 32]> {
        B64.decode(&self.key)?.try_into().map_err(|_| anyhow::anyhow!("the laptop key must have 32 bytes"))
    }
}

/// A random ID or code: 16 bytes in base64url.
pub fn random_token() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no random bytes: {e}"))?;
    Ok(B64.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let link = PairingLink::new("wss://relay.test", "lap", &[7; 32], &random_token().unwrap());
        let url = link.to_url("https://pwa.test/");
        assert!(url.starts_with("https://pwa.test/#pair="));
        assert_eq!(PairingLink::from_url(&url).unwrap(), link);
        assert_eq!(link.key().unwrap(), [7; 32]);
    }

    #[test]
    fn bad_key_fails() {
        let mut link = PairingLink::new("wss://relay.test", "lap", &[7; 32], "c");
        link.key = B64.encode([1; 31]);
        assert!(PairingLink::from_url(&link.to_url("https://pwa.test")).is_err());
    }
}
