//! The notification a sender writes for one device.
//!
//! It travels as a Declarative Web Push message: if the device's worker cannot run, iOS shows
//! the fallback, which is always the same generic text; when it can, the worker checks the
//! MAC and shows the real thing, naming the sender from its own contact list.

use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;

use crate::webpush::MAX_PLAINTEXT;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// The sender's handle on the device, from the ticket.
    pub h: String,
    /// `dm` or `file`.
    pub kind: String,
    /// The message (rumor) id, hex.
    pub msg: String,
    /// Send time, unix milliseconds.
    pub ts: u64,
    pub text: String,
}

/// Bytes of MAC carried: forging needs 2^128 tries against one device.
const MAC_LEN: usize = 16;

impl Notice {
    pub fn mac(&self, mk: &[u8]) -> Result<String> {
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(mk).map_err(|_| Error::Key("bad MAC key"))?;
        m.update(self.canonical().as_bytes());
        Ok(crate::hex(&m.finalize().into_bytes()[..MAC_LEN]))
    }

    fn canonical(&self) -> String {
        format!("vector-push/1\n{}\n{}\n{}\n{}\n{}", self.h, self.kind, self.msg, self.ts, self.text)
    }

    /// The push message, as JSON bytes that fit one body. Long text is cut to fit.
    pub fn message(&self, origin: &str, mk_hex: &str) -> Result<Vec<u8>> {
        let mk = crate::unhex(mk_hex)?;
        let mut notice = self.clone();
        loop {
            let out = notice.render(origin, &mk)?;
            if out.len() <= MAX_PLAINTEXT {
                return Ok(out);
            }
            let chars = notice.text.chars().count();
            if chars == 0 {
                return Err(Error::Format("notification does not fit".into()));
            }
            let over = out.len() - MAX_PLAINTEXT;
            notice.text = notice.text.chars().take(chars.saturating_sub(over.max(chars / 8)).saturating_sub(1)).collect();
            notice.text.push('…');
        }
    }

    fn render(&self, origin: &str, mk: &[u8]) -> Result<Vec<u8>> {
        // `mutable` sits both at the top and inside `notification`: WebKit moved it.
        let msg = json!({
            "web_push": 8030,
            "mutable": true,
            "notification": {
                "title": "Vector",
                "body": "New message",
                "navigate": format!("{}/", origin.trim_end_matches('/')),
                "mutable": true,
                "data": {
                    "vp": 1,
                    "h": self.h,
                    "k": self.kind,
                    "m": self.msg,
                    "t": self.ts,
                    "x": self.text,
                    "a": self.mac(mk)?,
                },
            },
        });
        Ok(msg.to_string().into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(text: &str) -> Notice {
        Notice { h: "a1b2c3d4e5f60718".into(), kind: "dm".into(), msg: "ab".repeat(32), ts: 1_790_000_000_000, text: text.into() }
    }

    #[test]
    fn the_mac_covers_every_field() {
        let mk = [9u8; 32];
        let base = notice("hi").mac(&mk).unwrap();
        assert_ne!(notice("hj").mac(&mk).unwrap(), base);
        assert_ne!(Notice { ts: 1, ..notice("hi") }.mac(&mk).unwrap(), base);
        assert_ne!(Notice { kind: "file".into(), ..notice("hi") }.mac(&mk).unwrap(), base);
        assert_ne!(notice("hi").mac(&[8u8; 32]).unwrap(), base);
    }

    #[test]
    fn long_text_is_cut_to_fit_and_still_verifies() {
        let mk = crate::hex(&[3u8; 32]);
        let out = notice(&"é\"".repeat(4000)).message("https://web.vectorapp.io", &mk).unwrap();
        assert!(out.len() <= MAX_PLAINTEXT);
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let data = &v["notification"]["data"];
        let cut = Notice { text: data["x"].as_str().unwrap().into(), ..notice("") };
        assert!(cut.text.ends_with('…'));
        assert_eq!(data["a"], cut.mac(&[3u8; 32]).unwrap());
        assert_eq!(v["notification"]["navigate"], "https://web.vectorapp.io/");
    }
}
