//! Transfer messages as they travel in an event's content: JSON with a cleartext version, so a
//! device on an older protocol says "update Vector" rather than "wrong code".

use serde::{Deserialize, Serialize};

pub const VERSION: u8 = 1;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Msg {
    /// Joiner: the hash of the SPAKE2 message it will reveal.
    Commit([u8; 32]),
    /// Shower: its SPAKE2 message, sent before it can see the joiner's.
    Pake(Vec<u8>),
    /// Joiner: its SPAKE2 message and its sealed hello.
    Reveal { pake: Vec<u8>, hello: Vec<u8> },
    /// Shower: its sealed hello.
    Hello(Vec<u8>),
    /// Sender: the ML-KEM ciphertext, and the identity sealed under SPAKE2's and ML-KEM's secrets.
    Bundle { kem: Vec<u8>, sealed: Vec<u8> },
    /// Sender: the user said no.
    Deny(Vec<u8>),
    /// Receiver: the identity arrived and checked out.
    Ack(Vec<u8>),
    /// Either side gave up. Unauthenticated, so it can only end a session, never steer one.
    Abort(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// A message from another protocol version.
    Version(u8),
    Malformed,
}

#[derive(Serialize, Deserialize, Default)]
struct Wire {
    v: u8,
    t: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    h: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    m: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    e: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    r: Option<String>,
}

fn b64(bytes: &[u8]) -> Option<String> {
    Some(base64_simd::STANDARD.encode_to_string(bytes))
}

fn unb64(field: &Option<String>) -> Result<Vec<u8>, DecodeError> {
    let text = field.as_deref().ok_or(DecodeError::Malformed)?;
    base64_simd::STANDARD.decode_to_vec(text).map_err(|_| DecodeError::Malformed)
}

impl Msg {
    pub fn kind(&self) -> &'static str {
        match self {
            Msg::Commit(_) => "commit",
            Msg::Pake(_) => "pake",
            Msg::Reveal { .. } => "reveal",
            Msg::Hello(_) => "hello",
            Msg::Bundle { .. } => "bundle",
            Msg::Deny(_) => "deny",
            Msg::Ack(_) => "ack",
            Msg::Abort(_) => "abort",
        }
    }

    pub fn encode(&self) -> String {
        let mut w = Wire { v: VERSION, t: self.kind().to_string(), ..Default::default() };
        match self {
            Msg::Commit(h) => w.h = b64(h),
            Msg::Pake(m) => w.m = b64(m),
            Msg::Reveal { pake, hello } => {
                w.m = b64(pake);
                w.e = b64(hello);
            }
            Msg::Bundle { kem, sealed } => {
                w.m = b64(kem);
                w.e = b64(sealed);
            }
            Msg::Hello(e) | Msg::Deny(e) | Msg::Ack(e) => w.e = b64(e),
            Msg::Abort(reason) => w.r = Some(reason.clone()),
        }
        serde_json::to_string(&w).expect("plain struct")
    }

    pub fn decode(content: &str) -> Result<Self, DecodeError> {
        let w: Wire = serde_json::from_str(content).map_err(|_| DecodeError::Malformed)?;
        if w.v != VERSION {
            return Err(DecodeError::Version(w.v));
        }
        Ok(match w.t.as_str() {
            "commit" => Msg::Commit(unb64(&w.h)?.try_into().map_err(|_| DecodeError::Malformed)?),
            "pake" => Msg::Pake(unb64(&w.m)?),
            "reveal" => Msg::Reveal { pake: unb64(&w.m)?, hello: unb64(&w.e)? },
            "hello" => Msg::Hello(unb64(&w.e)?),
            "bundle" => Msg::Bundle { kem: unb64(&w.m)?, sealed: unb64(&w.e)? },
            "deny" => Msg::Deny(unb64(&w.e)?),
            "ack" => Msg::Ack(unb64(&w.e)?),
            "abort" => Msg::Abort(w.r.unwrap_or_default().chars().take(64).collect()),
            _ => return Err(DecodeError::Malformed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_round_trips() {
        for msg in [
            Msg::Commit([7; 32]),
            Msg::Pake(vec![0x53; 33]),
            Msg::Reveal { pake: vec![1; 33], hello: vec![2; 40] },
            Msg::Hello(vec![3; 20]),
            Msg::Bundle { kem: vec![4; 1568], sealed: vec![5; 200] },
            Msg::Deny(vec![5; 16]),
            Msg::Ack(vec![6; 16]),
            Msg::Abort("mismatch".into()),
        ] {
            assert_eq!(Msg::decode(&msg.encode()).unwrap(), msg);
        }
    }

    #[test]
    fn another_version_or_junk_is_told_apart() {
        assert_eq!(Msg::decode(r#"{"v":2,"t":"pake","m":"AA=="}"#), Err(DecodeError::Version(2)));
        for junk in ["", "{}", r#"{"v":1,"t":"pake"}"#, r#"{"v":1,"t":"commit","h":"AAAA"}"#, r#"{"v":1,"t":"nope"}"#] {
            assert_eq!(Msg::decode(junk), Err(DecodeError::Malformed), "{junk}");
        }
    }
}
