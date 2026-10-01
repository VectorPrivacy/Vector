//! The realtime wire, byte for byte as every Vector client speaks it: base32
//! topic ids, relay-only node addresses, and the gossip frame trailer.

use iroh::{EndpointAddr, TransportAddr};

/// The largest payload an app may send in one frame (the WebXDC limit).
pub const MAX_PAYLOAD: usize = 128_000;

/// What the gossip layer accepts per message: payload plus trailer, with headroom.
pub const MAX_GOSSIP_MESSAGE: usize = 128 * 1024;

pub const NODE_KEY_LEN: usize = 32;

/// Every frame ends in a 4-byte little-endian sequence number and the sending
/// node's key. Gossip dedups by content hash, so the trailer is what keeps two
/// peers' identical payloads (two "hello"s) from collapsing into one.
pub const TRAILER_LEN: usize = 4 + NODE_KEY_LEN;

/// Append the trailer to an outbound payload.
pub fn seal_frame(mut payload: Vec<u8>, seq: i32, node_key: &[u8; NODE_KEY_LEN]) -> Vec<u8> {
    payload.reserve(TRAILER_LEN);
    payload.extend_from_slice(&seq.to_le_bytes());
    payload.extend_from_slice(node_key);
    payload
}

/// Split an inbound frame into `(payload, sender node key)`. `None` for a frame
/// too short to carry a trailer.
pub fn open_frame(frame: &[u8]) -> Option<(&[u8], [u8; NODE_KEY_LEN])> {
    let body = frame.len().checked_sub(TRAILER_LEN)?;
    let mut key = [0u8; NODE_KEY_LEN];
    key.copy_from_slice(&frame[body + 4..]);
    Some((&frame[..body], key))
}

pub use crate::webxdc::{base32_nopad_decode, base32_nopad_encode};

/// A topic id as it rides the `webxdc-topic` tag: 52 base32 characters.
pub fn decode_topic(s: &str) -> Result<[u8; 32], String> {
    let bytes = base32_nopad_decode(s.as_bytes())?;
    bytes.try_into().map_err(|_| "a topic id is 32 bytes".to_string())
}

pub fn encode_topic(topic: &[u8; 32]) -> String {
    base32_nopad_encode(topic)
}

/// The topic for an app whose message carries none: derived from the app's
/// name (as its manifest gives it) and the message, so every copy agrees.
pub fn fallback_topic(app_name: &str, chat_id: &str, message_id: &str) -> [u8; 32] {
    let mut input = Vec::with_capacity(20 + app_name.len() + chat_id.len() + message_id.len());
    input.extend_from_slice(b"webxdc-realtime-v1:");
    input.extend_from_slice(app_name.as_bytes());
    input.push(b':');
    input.extend_from_slice(chat_id.as_bytes());
    input.push(b':');
    input.extend_from_slice(message_id.as_bytes());
    crate::crypto::sha256::digest(&input)
}

pub fn encode_node_addr(addr: &EndpointAddr) -> Result<String, String> {
    let json = serde_json::to_string(addr).map_err(|e| e.to_string())?;
    Ok(base32_nopad_encode(json.as_bytes()))
}

/// A peer-supplied address, reduced to its relay paths: dialling a direct
/// address someone else nominated would hand them our IP. Which relays may be
/// dialled is the mesh's call (`is_default_relay`).
pub fn decode_node_addr(s: &str) -> Result<EndpointAddr, String> {
    if s.len() > 2048 {
        return Err("node address too long".into());
    }
    let bytes = base32_nopad_decode(s.as_bytes())?;
    let addr: EndpointAddr = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    Ok(relay_only(addr))
}

/// A peer's address as this platform may dial it: only relays every client
/// uses (a peer-named relay would be a server of their choosing that we connect
/// to), spelled as the platform can reach them. Empty when none is left.
pub fn dialable(peer: EndpointAddr) -> EndpointAddr {
    let mut addr = relay_only(peer);
    addr.addrs.retain(|a| matches!(a, TransportAddr::Relay(url) if is_default_relay(url)));
    #[cfg(target_arch = "wasm32")]
    {
        addr.addrs = addr
            .addrs
            .into_iter()
            .map(|a| match a {
                TransportAddr::Relay(url) => TransportAddr::Relay(browser_relay(&url)),
                other => other,
            })
            .collect();
    }
    addr
}

pub fn relay_only(addr: EndpointAddr) -> EndpointAddr {
    EndpointAddr {
        id: addr.id,
        addrs: addr.addrs.into_iter().filter(|a| matches!(a, TransportAddr::Relay(_))).collect(),
    }
}

/// Whether a relay is one of iroh's defaults, the set every Vector client uses.
/// Hosts compare without iroh's trailing dot.
pub fn is_default_relay(url: &iroh::RelayUrl) -> bool {
    let host = |u: &iroh::RelayUrl| u.host_str().map(|h| h.trim_end_matches('.').to_ascii_lowercase());
    let theirs = host(url);
    theirs.is_some()
        && iroh::defaults::prod::default_relay_map()
            .urls::<Vec<iroh::RelayUrl>>()
            .iter()
            .any(|d| host(d) == theirs)
}

/// A relay as a browser can dial it. WebKit refuses a WebSocket to a fully
/// qualified host, and iroh spells its relays with the trailing dot; without it
/// the URL names the same server.
pub fn browser_relay(url: &iroh::RelayUrl) -> iroh::RelayUrl {
    let mut u = (**url).clone();
    if let Some(host) = u.host_str().and_then(|h| h.strip_suffix('.')).map(str::to_string) {
        let _ = u.set_host(Some(&host));
    }
    iroh::RelayUrl::from(u)
}

/// The relays a node uses: iroh's defaults, as the platform can dial them.
pub fn relay_mode() -> iroh::RelayMode {
    #[cfg(not(target_arch = "wasm32"))]
    {
        iroh::RelayMode::Default
    }
    #[cfg(target_arch = "wasm32")]
    {
        iroh::RelayMode::Custom(iroh::defaults::prod::default_relay_map().urls::<Vec<iroh::RelayUrl>>().iter().map(browser_relay).collect())
    }
}

pub fn relay_urls(addr: &EndpointAddr) -> String {
    let urls: Vec<String> = addr
        .addrs
        .iter()
        .filter_map(|a| match a {
            TransportAddr::Relay(u) => Some(u.to_string()),
            _ => None,
        })
        .collect();
    if urls.is_empty() { "none".into() } else { urls.join(", ") }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_browser_dials_the_relay_without_its_trailing_dot() {
        let dotted: iroh::RelayUrl = "https://euc1-1.relay.n0.iroh.link./".parse().unwrap();
        let plain = browser_relay(&dotted);
        assert_eq!(plain.host_str(), Some("euc1-1.relay.n0.iroh.link"));
        assert!(is_default_relay(&plain), "still one of the default relays");
    }


    #[test]
    fn the_fallback_topic_is_the_one_every_client_derives() {
        assert_eq!(encode_topic(&fallback_topic("Chess", "npub1chat", "msg1")), "YFLKK3KDFMLO5GJXVLY6CIMNTMMN53BKOYAJ7LXBSW4DOMFWTCXA");
    }

    use super::*;

    #[test]
    fn a_frame_round_trips_its_payload_and_sender() {
        let key = [7u8; 32];
        let frame = seal_frame(b"{\"t\":\"hello\"}".to_vec(), 3, &key);
        assert_eq!(frame.len(), 13 + TRAILER_LEN);
        // The trailer is little-endian seq then key, as the app's clients write it.
        assert_eq!(&frame[13..17], &3i32.to_le_bytes());
        let (payload, sender) = open_frame(&frame).unwrap();
        assert_eq!(payload, b"{\"t\":\"hello\"}");
        assert_eq!(sender, key);
    }

    #[test]
    fn an_empty_payload_is_a_valid_frame_and_a_short_one_is_not() {
        let frame = seal_frame(Vec::new(), 1, &[1u8; 32]);
        assert_eq!(open_frame(&frame).unwrap().0, b"");
        assert!(open_frame(&frame[1..]).is_none());
    }

    #[test]
    fn topics_round_trip_and_reject_the_wrong_length() {
        let t = [0xABu8; 32];
        let s = encode_topic(&t);
        assert_eq!(s.len(), 52);
        assert_eq!(decode_topic(&s).unwrap(), t);
        assert!(decode_topic("MZXW6").is_err());
        assert!(decode_topic(&"1".repeat(52)).is_err());
    }

    #[test]
    fn only_the_default_relays_are_dialled() {
        for url in iroh::defaults::prod::default_relay_map().urls::<Vec<iroh::RelayUrl>>() {
            assert!(is_default_relay(&url));
            let undotted: iroh::RelayUrl = url.as_str().replacen(".iroh.link./", ".iroh.link/", 1).parse().unwrap();
            assert!(is_default_relay(&undotted), "{undotted} is the same server");
        }
        for evil in ["https://attacker.example./", "https://euc1-1.relay.n0.iroh.link.evil.example/", "http://127.0.0.1:3340/"] {
            assert!(!is_default_relay(&evil.parse().unwrap()), "{evil}");
        }
    }

    #[test]
    fn a_dialable_address_keeps_only_the_default_relays() {
        let default = iroh::defaults::prod::default_relay_map().urls::<Vec<iroh::RelayUrl>>()[0].clone();
        let evil: iroh::RelayUrl = "https://attacker.example./".parse().unwrap();
        let addr = EndpointAddr {
            id: iroh::SecretKey::from([1u8; 32]).public(),
            addrs: [
                TransportAddr::Relay(default.clone()),
                TransportAddr::Relay(evil),
                TransportAddr::Ip("203.0.113.7:4433".parse().unwrap()),
            ]
            .into_iter()
            .collect(),
        };
        let kept: Vec<_> = dialable(addr).addrs.into_iter().collect();
        assert_eq!(kept, vec![TransportAddr::Relay(default)]);
    }

    #[test]
    fn a_minted_topic_decodes() {
        let minted = crate::webxdc::mint_topic_id("hash", "sender");
        assert!(decode_topic(&minted).is_ok());
    }
}
