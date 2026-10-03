//! The event a sender publishes for the pusher: signed by a throwaway key, encrypted to the
//! pusher, so neither the relay nor the pusher learns who asked.

use nostr::nips::nip44::{self, Version};
use nostr::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Ephemeral range: relays hand it to the listening pusher and keep nothing.
pub const KIND: u16 = 24590;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub v: u8,
    /// The ticket's sealed capability.
    pub s: String,
    /// The RFC 8291 body, base64url.
    pub b: String,
    /// Seconds the push service may hold it for a device that is offline.
    pub t: u32,
}

impl Request {
    pub fn new(sealed: &str, body: &[u8], ttl: u32) -> Self {
        Request { v: 1, s: sealed.to_string(), b: crate::b64url(body), t: ttl }
    }

    pub fn body(&self) -> Result<Vec<u8>> {
        crate::unb64url(&self.b)
    }

    pub fn event(&self, pusher: &PublicKey) -> Result<Event> {
        let json = serde_json::to_string(self).map_err(|e| Error::Format(e.to_string()))?;
        seal_event(pusher, &json)
    }

    pub fn open(pusher: &Keys, event: &Event) -> Result<Self> {
        if event.kind != Kind::Custom(KIND) {
            return Err(Error::Format("not a push request".into()));
        }
        let json = nip44::decrypt(pusher.secret_key(), &event.pubkey, &event.content)
            .map_err(|_| Error::Crypto("request decryption failed"))?;
        let req: Request = serde_json::from_str(&json).map_err(|e| Error::Format(e.to_string()))?;
        if req.v != 1 {
            return Err(Error::Format(format!("request version {}", req.v)));
        }
        Ok(req)
    }
}

/// A device telling its pusher to hold or release pushes for some of its contacts. Ticket ids
/// are sealed to the pusher, so knowing one is the proof of owning it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Control {
    pub v: u8,
    pub ctl: Vec<Hold>,
    /// Filler to a push request's size, so the relay can't tell the two apart.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub p: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    /// The ticket's capability id.
    pub c: String,
    /// Unix seconds pushes are held until: 0 releases, [`HOLD_FOREVER`] never expires.
    pub until: u64,
}

pub const HOLD_FOREVER: u64 = u64::MAX;

/// Plaintext length every message to a pusher pads to: one NIP-44 size bucket for both kinds.
const MESSAGE_PLAINTEXT: usize = 5600;

impl Control {
    pub fn new(ctl: Vec<Hold>) -> Self {
        Control { v: 1, ctl, p: String::new() }
    }

    pub fn event(&self, pusher: &PublicKey) -> Result<Event> {
        let mut padded = self.clone();
        let len = serde_json::to_string(&padded).map_err(|e| Error::Format(e.to_string()))?.len();
        if len + 8 < MESSAGE_PLAINTEXT {
            padded.p = "0".repeat(MESSAGE_PLAINTEXT - len - 8);
        }
        seal_event(pusher, &serde_json::to_string(&padded).map_err(|e| Error::Format(e.to_string()))?)
    }
}

/// What a pusher can receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    Push(Request),
    Control(Control),
}

impl Incoming {
    pub fn open(pusher: &Keys, event: &Event) -> Result<Self> {
        if event.kind != Kind::Custom(KIND) {
            return Err(Error::Format("not a push request".into()));
        }
        let json = nip44::decrypt(pusher.secret_key(), &event.pubkey, &event.content)
            .map_err(|_| Error::Crypto("request decryption failed"))?;
        let value: serde_json::Value = serde_json::from_str(&json).map_err(|e| Error::Format(e.to_string()))?;
        if value.get("v").and_then(|v| v.as_u64()) != Some(1) {
            return Err(Error::Format("unknown message version".into()));
        }
        if value.get("ctl").is_some() {
            return serde_json::from_value(value).map(Incoming::Control).map_err(|e| Error::Format(e.to_string()));
        }
        serde_json::from_value(value).map(Incoming::Push).map_err(|e| Error::Format(e.to_string()))
    }
}

fn seal_event(pusher: &PublicKey, json: &str) -> Result<Event> {
    let keys = Keys::generate();
    let content = nip44::encrypt(keys.secret_key(), pusher, json, Version::V2)
        .map_err(|_| Error::Crypto("request encryption failed"))?;
    EventBuilder::new(Kind::Custom(KIND), content)
        .tag(Tag::public_key(*pusher))
        .finalize(&keys)
        .map_err(|e| Error::Format(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_round_trips_and_names_nobody() {
        let pusher = Keys::generate();
        let req = Request::new("sealed", &[7; 3000], 86_400);
        let ev = req.event(&pusher.public_key()).unwrap();
        ev.verify().unwrap();
        assert_eq!(ev.tags.len(), 1);
        assert_eq!(Request::open(&pusher, &ev).unwrap(), req);
        assert_ne!(ev.pubkey, req.event(&pusher.public_key()).unwrap().pubkey);
        assert!(Request::open(&Keys::generate(), &ev).is_err());
    }

    #[test]
    fn a_control_looks_like_a_push_to_the_relay() {
        let pusher = Keys::generate();
        let sealed = crate::ticket::seal(&pusher.public_key(), &crate::Capability { endpoint: "https://web.push.apple.com/x".into(), vapid: "k".repeat(43), cap: "c".repeat(32) }).unwrap();
        let push = Request::new(&sealed, &[7; crate::webpush::BODY_LEN], 172_800).event(&pusher.public_key()).unwrap();
        let ctl = Control::new(vec![Hold { c: "ab".repeat(16), until: HOLD_FOREVER }, Hold { c: "cd".repeat(16), until: 0 }]);
        let ev = ctl.event(&pusher.public_key()).unwrap();
        assert_eq!(ev.content.len(), push.content.len(), "same NIP-44 bucket");
        match Incoming::open(&pusher, &ev).unwrap() {
            Incoming::Control(c) => assert_eq!(c.ctl, ctl.ctl),
            other => panic!("{other:?}"),
        }
        assert!(matches!(Incoming::open(&pusher, &push).unwrap(), Incoming::Push(_)));
    }
}
