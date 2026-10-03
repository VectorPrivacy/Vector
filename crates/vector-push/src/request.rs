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
        let keys = Keys::generate();
        let json = serde_json::to_string(self).map_err(|e| Error::Format(e.to_string()))?;
        let content = nip44::encrypt(keys.secret_key(), pusher, json, Version::V2)
            .map_err(|_| Error::Crypto("request encryption failed"))?;
        EventBuilder::new(Kind::Custom(KIND), content)
            .tag(Tag::public_key(*pusher))
            .finalize(&keys)
            .map_err(|e| Error::Format(e.to_string()))
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
}
