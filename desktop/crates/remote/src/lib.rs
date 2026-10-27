//! The remote control of the app from a phone. The phone (the PWA in `pwa/`) and the laptop
//! both connect to the relay (in `relay/`). The relay passes the messages between them. The
//! messages have end-to-end encryption, so the relay cannot read them.

pub mod message;
pub mod noise;
pub mod pairing;
