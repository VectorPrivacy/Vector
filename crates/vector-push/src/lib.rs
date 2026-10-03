//! Sealed push: background notifications nobody in the middle can read or attribute.
//!
//! A device that wants notifications hands each contact a [`Ticket`]. When that contact
//! sends it a message, the contact's client writes the notification itself, encrypts it to
//! the device's Web Push keys ([`webpush`]), and asks a pusher to deliver it ([`request`]).
//! The pusher opens only the part of the ticket sealed to it, the device's push address,
//! and never sees who wrote, who receives, or what was said.
//!
//! - [`webpush`]: RFC 8291 message encryption, done by the sender.
//! - [`vapid`]: RFC 8292 keys; the device owns its own, the pusher signs with it.
//! - [`ticket`]: what a contact holds, and the capability only the pusher can open.
//! - [`request`]: the Nostr event a sender publishes for the pusher.
//! - [`payload`]: the notification itself, authenticated per contact.

pub mod payload;
pub mod request;
pub mod ticket;
pub mod vapid;
pub mod webpush;

pub use payload::Notice;
pub use request::Request;
pub use ticket::{Capability, Ticket};

/// The pusher Vector runs. A ticket names its own, so a device may pick another.
pub const DEFAULT_PUSHER: &str = "cdfdb82459cb5f6a328fa8df5150e0f8482efe12f6a571f801b59ecd53ecbb9c";
pub const DEFAULT_PUSHER_RELAYS: &[&str] = &["wss://jskitty.com/nostr"];

/// `d` tag of the rumor that carries a device's tickets to a contact.
pub const TICKET_RUMOR_D: &str = "vector-push";

#[derive(Debug)]
pub enum Error {
    Key(&'static str),
    Crypto(&'static str),
    Format(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Key(m) | Error::Crypto(m) => f.write_str(m),
            Error::Format(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn b64url(bytes: &[u8]) -> String {
    base64_simd::URL_SAFE_NO_PAD.encode_to_string(bytes)
}

/// Lenient on padding: browsers hand out keys with and without it.
pub(crate) fn unb64url(s: &str) -> Result<Vec<u8>> {
    base64_simd::URL_SAFE_NO_PAD
        .decode_to_vec(s.trim_end_matches('='))
        .map_err(|_| Error::Format("bad base64url".into()))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn unhex(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        return Err(Error::Format("odd hex".into()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| Error::Format("bad hex".into())))
        .collect()
}

pub(crate) fn random<const N: usize>() -> [u8; N] {
    use rand::RngCore;
    let mut out = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut out);
    out
}
