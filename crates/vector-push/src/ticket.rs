//! Tickets: what a device hands each contact so they can notify it.
//!
//! A ticket holds the device's Web Push encryption keys in the clear (the contact encrypts
//! to them) and its push address sealed to the pusher (only the pusher delivers). Each
//! contact gets its own handle, MAC key, capability id and sealing randomness, so two
//! contacts cannot tell they hold tickets for the same device.

use nostr::nips::nip44::{self, Version};
use nostr::prelude::{Keys, PublicKey, SecretKey};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    /// The device subscription: a newer ticket with the same id replaces the old one.
    pub id: String,
    /// Hex pubkey of the pusher the sealed part is for, and where it listens.
    pub pusher: String,
    pub relays: Vec<String>,
    pub sealed: String,
    /// The subscription's RFC 8291 keys, base64url.
    pub p256dh: String,
    pub auth: String,
    /// Where the notification opens on the device.
    pub origin: String,
    /// This contact's handle on the device, and the key proving a notice came from them.
    pub h: String,
    pub mk: String,
}

impl Ticket {
    /// Refuse a ticket that could not work, before it is stored.
    pub fn validate(&self) -> Result<()> {
        PublicKey::from_hex(&self.pusher).map_err(|_| Error::Key("bad pusher key"))?;
        if self.relays.is_empty() || self.relays.iter().any(|r| !r.starts_with("wss://") && !r.starts_with("ws://")) {
            return Err(Error::Format("bad pusher relays".into()));
        }
        if crate::unb64url(&self.p256dh)?.len() != 65 || crate::unb64url(&self.auth)?.len() != 16 {
            return Err(Error::Key("bad subscription keys"));
        }
        if crate::unhex(&self.mk)?.len() != 32 || crate::unhex(&self.h)?.len() != 8 {
            return Err(Error::Key("bad contact keys"));
        }
        if !self.origin.starts_with("https://") && !self.origin.starts_with("http://localhost") && !self.origin.starts_with("http://127.0.0.1") {
            return Err(Error::Format("bad origin".into()));
        }
        if self.id.is_empty() || self.id.len() > 64 || self.sealed.len() > 4096 {
            return Err(Error::Format("bad ticket".into()));
        }
        Ok(())
    }

    /// The push request that carries `notice` to this ticket's device, ready to publish to
    /// [`Ticket::relays`].
    pub fn request(&self, notice: &crate::Notice, ttl: u32) -> Result<nostr::prelude::Event> {
        let notice = crate::Notice { h: self.h.clone(), ..notice.clone() };
        let plain = notice.message(&self.origin, &self.mk)?;
        let body = crate::webpush::encrypt(&crate::unb64url(&self.p256dh)?, &crate::unb64url(&self.auth)?, &plain)?;
        let pusher = PublicKey::from_hex(&self.pusher).map_err(|_| Error::Key("bad pusher key"))?;
        crate::Request::new(&self.sealed, &body, ttl).event(&pusher)
    }
}

/// The rumor content carrying a device's tickets to one contact.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TicketList {
    pub v: u8,
    #[serde(default)]
    pub tickets: Vec<Ticket>,
    /// Ticket ids the contact must forget: the device turned notifications off.
    #[serde(default)]
    pub revoked: Vec<String>,
}

/// What only the pusher reads: enough to deliver, nothing about who it is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    #[serde(rename = "e")]
    pub endpoint: String,
    /// The device's VAPID private key, base64url.
    #[serde(rename = "k")]
    pub vapid: String,
    /// Per contact, so the device can pause or revoke one contact at the pusher.
    #[serde(rename = "c")]
    pub cap: String,
}

/// Every sealed capability pads to this before encryption, so endpoint lengths (which
/// differ per push service) don't show through.
const SEALED_PLAINTEXT: usize = 1000;

pub fn seal(pusher: &PublicKey, cap: &Capability) -> Result<String> {
    let mut json = serde_json::to_value(cap).map_err(|e| Error::Format(e.to_string()))?;
    let len = json.to_string().len();
    // `,"p":""` costs 7 bytes before the filler.
    if len + 7 < SEALED_PLAINTEXT {
        json["p"] = serde_json::Value::String("0".repeat(SEALED_PLAINTEXT - len - 7));
    }
    let eph = Keys::generate();
    let ct = nip44::encrypt(eph.secret_key(), pusher, json.to_string(), Version::V2)
        .map_err(|_| Error::Crypto("seal failed"))?;
    Ok(format!("{}:{ct}", eph.public_key().to_hex()))
}

pub fn unseal(pusher: &SecretKey, sealed: &str) -> Result<Capability> {
    let (eph, ct) = sealed.split_once(':').ok_or(Error::Format("bad sealed capability".into()))?;
    let eph = PublicKey::from_hex(eph).map_err(|_| Error::Key("bad sealing key"))?;
    let plain = nip44::decrypt(pusher, &eph, ct).map_err(|_| Error::Crypto("unseal failed"))?;
    serde_json::from_str(&plain).map_err(|e| Error::Format(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_pusher_opens_a_seal_and_every_seal_is_the_same_size() {
        let pusher = Keys::generate();
        let short = Capability { endpoint: "https://web.push.apple.com/abc".into(), vapid: "k".into(), cap: "c".into() };
        let long = Capability { endpoint: format!("https://fcm.googleapis.com/fcm/send/{}", "x".repeat(400)), ..short.clone() };
        let a = seal(&pusher.public_key(), &short).unwrap();
        let b = seal(&pusher.public_key(), &long).unwrap();
        assert_eq!(a.len(), b.len());
        assert_eq!(unseal(pusher.secret_key(), &a).unwrap(), short);
        assert_eq!(unseal(pusher.secret_key(), &b).unwrap(), long);
        assert!(unseal(Keys::generate().secret_key(), &a).is_err());
        // Fresh randomness per seal: the same capability never seals the same way twice.
        assert_ne!(a, seal(&pusher.public_key(), &short).unwrap());
    }

    #[test]
    fn a_ticket_carries_a_notice_only_the_device_reads() {
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        let pusher = Keys::generate();
        let device = p256::SecretKey::random(&mut rand::rngs::OsRng);
        let auth = crate::random::<16>();
        let (vapid, _) = crate::vapid::generate();
        let cap = Capability { endpoint: "https://web.push.apple.com/abc".into(), vapid, cap: "c1".into() };
        let t = Ticket {
            id: "dev".into(),
            pusher: pusher.public_key().to_hex(),
            relays: vec!["wss://relay.example".into()],
            sealed: seal(&pusher.public_key(), &cap).unwrap(),
            p256dh: crate::b64url(device.public_key().to_encoded_point(false).as_bytes()),
            auth: crate::b64url(&auth),
            origin: "https://web.vectorapp.io".into(),
            h: crate::hex(&[1; 8]),
            mk: crate::hex(&[2; 32]),
        };
        t.validate().unwrap();
        let notice = crate::Notice { h: String::new(), kind: "dm".into(), msg: "m".into(), ts: 5, text: "hi".into() };
        let ev = t.request(&notice, 60).unwrap();
        let req = crate::Request::open(&pusher, &ev).unwrap();
        assert_eq!(unseal(pusher.secret_key(), &req.s).unwrap(), cap);
        let plain = crate::webpush::decrypt(&device, &auth, &req.body().unwrap()).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&plain).unwrap();
        let data = &v["notification"]["data"];
        assert_eq!(data["x"], "hi");
        assert_eq!(data["h"], t.h);
        let signed = crate::Notice { h: t.h.clone(), ..notice };
        assert_eq!(data["a"], signed.mac(&[2; 32]).unwrap());
    }
}

