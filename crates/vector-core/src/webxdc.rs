//! WebXDC Mini App helpers shared across transports (DM + Community).
//!
//! The realtime-channel topic for a Mini App is minted ONCE at send time and
//! carried on the file event as a `webxdc-topic` tag, so every participant
//! joins the SAME gossip topic. Locally-derived topics are asymmetric in DMs
//! (each side's chat_id is the other party's npub), which silently splits the
//! players onto disjoint topics — the tag is the single source of truth.

/// Mint a fresh realtime-channel topic id for an outbound `.xdc` attachment.
///
/// 32 bytes of SHA-256 over a domain separator + file hash + sender + send-time
/// nanos, encoded base32 (RFC 4648, no padding) — the same codec the miniapp
/// realtime layer's `decode_topic_id` expects, so the tag value round-trips
/// into an iroh `TopicId`. The nanos input makes re-sends of the same file
/// distinct sessions.
pub fn mint_topic_id(file_hash: &str, sender_hex: &str) -> String {
    use sha2::{Digest, Sha256};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(b"webxdc-realtime-v1:");
    hasher.update(file_hash.as_bytes());
    hasher.update(b":");
    hasher.update(sender_hex.as_bytes());
    hasher.update(b":");
    hasher.update(nanos.to_le_bytes());
    base32_nopad_encode(&hasher.finalize())
}

/// BASE32 no-pad encoding (RFC 4648). Mirrors the miniapp realtime layer's
/// codec exactly — the two must agree for topic tags to decode.
pub fn base32_nopad_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity((bytes.len() * 8 + 4) / 5);
    let mut buf: u64 = 0;
    let mut bits: u32 = 0;
    for &b in bytes {
        buf = (buf << 8) | b as u64;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buf >> bits) & 0x1F) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buf << (5 - bits)) & 0x1F) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_topic_is_32_bytes_base32() {
        let t = mint_topic_id("abc123", "deadbeef");
        // 32 bytes → ceil(256/5) = 52 base32 chars
        assert_eq!(t.len(), 52);
        assert!(t.chars().all(|c| c.is_ascii_uppercase() || ('2'..='7').contains(&c)));
    }

    #[test]
    fn resends_mint_distinct_topics() {
        let a = mint_topic_id("abc123", "deadbeef");
        let b = mint_topic_id("abc123", "deadbeef");
        assert_ne!(a, b, "same file re-sent must start a fresh session topic");
    }

    #[test]
    fn base32_matches_rfc4648_vectors() {
        // RFC 4648 §10 test vectors (padding stripped)
        assert_eq!(base32_nopad_encode(b""), "");
        assert_eq!(base32_nopad_encode(b"f"), "MY");
        assert_eq!(base32_nopad_encode(b"fo"), "MZXQ");
        assert_eq!(base32_nopad_encode(b"foo"), "MZXW6");
        assert_eq!(base32_nopad_encode(b"foob"), "MZXW6YQ");
        assert_eq!(base32_nopad_encode(b"fooba"), "MZXW6YTB");
        assert_eq!(base32_nopad_encode(b"foobar"), "MZXW6YTBOI");
    }
}

/// The kind-3310 peer-signal content, shared by every community transport (the
/// v1 channel plane and the v2 chat plane must stay byte-compatible):
/// `{"op":"ad","topic":..,"addr":..}` advertises an Iroh node, `{"op":"left",..}`
/// departs.
pub fn peer_signal_content(topic_id: &str, node_addr: Option<&str>) -> String {
    match node_addr {
        Some(addr) => serde_json::json!({ "op": "ad", "topic": topic_id, "addr": addr }).to_string(),
        None => serde_json::json!({ "op": "left", "topic": topic_id }).to_string(),
    }
}

/// Parse + bound a kind-3310 peer signal: `Some((topic, Some(addr)))` for an
/// advertisement, `Some((topic, None))` for a departure. Both fields are
/// author-controlled: the topic must be a 52-char base32 TopicId and the addr is
/// size-bounded — the realtime layer's decode is the final word.
pub fn parse_peer_signal(content: &str) -> Option<(String, Option<String>)> {
    let v: serde_json::Value = serde_json::from_str(content).ok()?;
    let topic_id = v
        .get("topic")
        .and_then(|t| t.as_str())
        .filter(|t| t.len() == 52 && t.bytes().all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b)))?
        .to_string();
    let node_addr = match v.get("op").and_then(|o| o.as_str())? {
        "ad" => Some(v.get("addr").and_then(|a| a.as_str()).filter(|a| !a.is_empty() && a.len() <= 2048)?.to_string()),
        "left" => None,
        _ => return None,
    };
    Some((topic_id, node_addr))
}

#[cfg(test)]
mod peer_signal_tests {
    use super::*;

    #[test]
    fn peer_signal_round_trips_and_bounds() {
        let topic = "A".repeat(52);
        let ad = peer_signal_content(&topic, Some("iroh:node/abc"));
        assert_eq!(parse_peer_signal(&ad), Some((topic.clone(), Some("iroh:node/abc".into()))));
        let left = peer_signal_content(&topic, None);
        assert_eq!(parse_peer_signal(&left), Some((topic.clone(), None)));

        // Author-controlled fields are bounded: bad topic, oversized addr, junk op.
        assert_eq!(parse_peer_signal(&peer_signal_content("short", Some("a"))), None);
        let oversized = "a".repeat(2049);
        assert_eq!(parse_peer_signal(&peer_signal_content(&topic, Some(&oversized))), None);
        assert_eq!(parse_peer_signal(&format!("{{\"op\":\"warp\",\"topic\":\"{topic}\"}}")), None);
        assert_eq!(parse_peer_signal("not json"), None);
    }
}
