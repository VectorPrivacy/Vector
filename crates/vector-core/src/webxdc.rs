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
/// nanos + a per-process counter, encoded base32 (RFC 4648, no padding) — the
/// same codec the miniapp realtime layer's `decode_topic_id` expects, so the tag
/// value round-trips into an iroh `TopicId`.
///
/// The counter is what actually guarantees re-sends are distinct sessions. The
/// clock cannot: `SystemTime::now()` reports nanos but does not RESOLVE them, so
/// two sends of the same file inside one tick hashed identical input and minted
/// the same topic — the "fresh" session silently rejoined the previous one.
pub fn mint_topic_id(file_hash: &str, sender_hex: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};
    static MINTED: AtomicU64 = AtomicU64::new(0);
    let nanos = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = MINTED.fetch_add(1, Ordering::Relaxed);
    let mut hasher = Sha256::new();
    hasher.update(b"webxdc-realtime-v1:");
    hasher.update(file_hash.as_bytes());
    hasher.update(b":");
    hasher.update(sender_hex.as_bytes());
    hasher.update(b":");
    hasher.update(nanos.to_le_bytes());
    hasher.update(b":");
    hasher.update(seq.to_le_bytes());
    base32_nopad_encode(&hasher.finalize())
}

/// Derive the realtime topic for a URL-shared Mini App.
///
/// A pasted `.xdc` URL has no file event to carry a minted topic, so every
/// recipient derives the same one from what the message already gives them:
/// the URL string and the message id (which keeps re-shares distinct
/// sessions, the role nanos+counter play in `mint_topic_id`). Deliberately
/// NOT the content hash: servers rebuild identical apps into new bytes, and
/// players who tapped the same card at different times must still share a
/// session — the message is the session anchor, the URL only the source.
pub fn derive_url_topic_id(url: &str, msg_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"webxdc-url-realtime-v1:");
    hasher.update(url.as_bytes());
    hasher.update(b":");
    hasher.update(msg_id.as_bytes());
    base32_nopad_encode(&hasher.finalize())
}

/// BASE32 no-pad encoding (RFC 4648). Mirrors the miniapp realtime layer's
/// codec exactly — the two must agree for topic tags to decode.
pub fn base32_nopad_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
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
mod url_topic_tests {
    use super::*;

    #[test]
    fn a_url_topic_is_deterministic_per_message_and_survives_server_rebuilds() {
        let a = derive_url_topic_id("https://x.org/app.xdc", "msg1");
        assert_eq!(a, derive_url_topic_id("https://x.org/app.xdc", "msg1"));
        assert_eq!(a.len(), 52, "must be a valid 52-char base32 TopicId");
        assert!(parse_peer_signal(&peer_signal_content(&a, Some("iroh:x"))).is_some());
        assert_ne!(a, derive_url_topic_id("https://x.org/other.xdc", "msg1"), "different URL = disjoint topics");
        assert_ne!(a, derive_url_topic_id("https://x.org/app.xdc", "msg2"), "re-share = fresh session");
    }
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

/// Represents a Mini App listing in the marketplace
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MarketplaceApp {
    /// Unique identifier (from d-tag)
    pub id: String,
    /// Display name
    pub name: String,
    /// Description
    pub description: String,
    /// Version string
    pub version: String,
    /// Blossom SHA-256 hash of the .xdc file
    pub blossom_hash: String,
    /// Full Blossom download URL
    pub download_url: String,
    /// File size in bytes
    pub size: u64,
    /// Optional icon URL (Blossom)
    pub icon_url: Option<String>,
    /// Optional icon MIME type (e.g., "image/png", "image/svg+xml")
    pub icon_mime: Option<String>,
    /// Locally cached icon path (for offline support)
    pub icon_cached: Option<String>,
    /// Categories/tags
    pub categories: Vec<String>,
    /// Extended description or changelog (from content)
    pub changelog: Option<String>,
    /// Developer name (the actual creator of the game/app)
    pub developer: Option<String>,
    /// Source code or website URL
    pub source_url: Option<String>,
    /// Publisher's public key (npub)
    pub publisher: String,
    /// Event creation timestamp
    pub published_at: u64,
    /// Whether this app is installed locally
    pub installed: bool,
    /// Local file path if installed
    pub local_path: Option<String>,
    /// Installed version (if different from marketplace version, update is available)
    pub installed_version: Option<String>,
    /// Whether an update is available (marketplace version != installed version)
    pub update_available: bool,
    /// Requested permissions (comma-separated string for easy serialization)
    /// Format: "microphone,camera,fullscreen"
    pub requested_permissions: String,
}

// ─── Marketplace listings (kind 30078, `t=miniapp`) ─────────────────────────

use nostr_sdk::prelude::*;

/// The event kind for Mini App marketplace listings
/// Kind 30078 = Parameterized Replaceable Application-Specific Data
pub const MINIAPP_MARKETPLACE_KIND: u16 = 30078;

/// The application identifier for Vector Mini Apps marketplace
pub const MINIAPP_APP_ID: &str = "vector/miniapp";

/// Trusted publisher npub (only apps from this pubkey are shown initially)
/// This is the Vector project's official npub for publishing verified apps
pub const TRUSTED_PUBLISHER: &str = "npub16ye7evyevwnl0fc9hujsxf9zym72e063awn0pvde0huvpyec5nyq4dg4wn";

/// Parse a Nostr event into a MarketplaceApp
/// A marketplace `d`-tag id is attacker-controlled and becomes a filesystem path (`<id>.xdc`).
/// Allow only a bounded, separator-free, traversal-free token so an id can never escape the
/// miniapps dir. Permits reverse-DNS / slug / hash ids; bans `/`, `\`, `..`, absolute/UNC paths.
pub fn is_safe_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.contains("..")
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

pub fn parse_marketplace_event(event: &Event) -> Option<MarketplaceApp> {
    // Verify it's the correct kind
    if event.kind.as_u16() != MINIAPP_MARKETPLACE_KIND {
        return None;
    }

    // Extract required tags
    let mut id = None;
    let mut name = None;
    let mut description = None;
    let mut version = None;
    let mut blossom_hash = None;
    let mut download_url = None;
    let mut size: Option<u64> = None;
    let mut icon_url = None;
    let mut icon_mime = None;
    let mut categories = Vec::new();
    let mut developer = None;
    let mut source_url = None;
    let mut requested_permissions = String::new();

    for tag in event.tags.iter() {
        let tag_vec: Vec<String> = tag.clone().to_vec();
        if tag_vec.len() < 2 {
            continue;
        }

        match tag_vec[0].as_str() {
            "d" => id = Some(tag_vec[1].clone()),
            "name" => name = Some(tag_vec[1].clone()),
            "description" => description = Some(tag_vec[1].clone()),
            "version" => version = Some(tag_vec[1].clone()),
            "x" => blossom_hash = Some(tag_vec[1].clone()),
            "url" => download_url = Some(tag_vec[1].clone()),
            "size" => size = tag_vec[1].parse().ok(),
            "icon" => {
                // Icon tag format: ["icon", "<url>", "<mime-type>"]
                icon_url = Some(tag_vec[1].clone());
                if tag_vec.len() >= 3 {
                    icon_mime = Some(tag_vec[2].clone());
                }
            }
            "dev" => developer = Some(tag_vec[1].clone()),
            "src" => source_url = Some(tag_vec[1].clone()),
            "permissions" => {
                // Permissions tag format: ["permissions", "microphone,camera,fullscreen"]
                requested_permissions = tag_vec[1].clone();
            }
            "t" => {
                // Skip the generic "miniapp" and "webxdc" tags for categories
                let tag_value = &tag_vec[1];
                if tag_value != "miniapp" && tag_value != "webxdc" {
                    categories.push(tag_value.clone());
                }
            }
            _ => {}
        }
    }

    // Validate required fields
    let id = id?;
    if !is_safe_app_id(&id) {
        log_warn!("[Marketplace] Rejected app with unsafe d-tag id: {:?}", id);
        return None;
    }
    let name = name?;
    let blossom_hash = blossom_hash?;
    let download_url = download_url?;

    let publisher = event.pubkey.to_bech32().unwrap_or_else(|_| event.pubkey.to_hex());

    Some(MarketplaceApp {
        id,
        name,
        description: description.unwrap_or_default(),
        version: version.unwrap_or_else(|| "1.0.0".to_string()),
        blossom_hash,
        download_url,
        size: size.unwrap_or(0),
        icon_url,
        icon_mime,
        icon_cached: None,
        categories,
        changelog: if event.content.is_empty() { None } else { Some(event.content.clone()) },
        developer,
        source_url,
        publisher,
        published_at: event.created_at.as_secs(),
        installed: false,
        local_path: None,
        installed_version: None,
        update_available: false,
        requested_permissions,
    })
}

/// The publisher-supplied metadata of a marketplace listing.
pub struct MarketplaceListing<'a> {
    pub app_id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub version: &'a str,
    pub categories: Vec<&'a str>,
    pub changelog: Option<&'a str>,
    pub developer: Option<&'a str>,
    pub source_url: Option<&'a str>,
    /// Comma-separated permissions string
    pub permissions: Option<&'a str>,
}

/// Build a Nostr event for publishing a Mini App to the marketplace
pub async fn build_marketplace_event<T: crate::signer::VectorSigner>(
    signer: &T,
    listing: &MarketplaceListing<'_>,
    blossom_hash: &str,
    download_url: &str,
    size: u64,
    icon_info: Option<(&str, &str)>, // (url, mime_type)
) -> Result<Event, String> {
    let MarketplaceListing {
        app_id,
        name,
        description,
        version,
        ref categories,
        changelog,
        developer,
        source_url,
        permissions,
    } = *listing;
    let mut tags = vec![
        Tag::custom("d", vec![app_id.to_string()]),
        Tag::custom("name", vec![name.to_string()]),
        Tag::custom("description", vec![description.to_string()]),
        Tag::custom("version", vec![version.to_string()]),
        Tag::custom("x", vec![blossom_hash.to_string()]),
        Tag::custom("url", vec![download_url.to_string()]),
        Tag::custom("size", vec![size.to_string()]),
        Tag::custom("t", vec!["miniapp".to_string()]),
        Tag::custom("t", vec!["webxdc".to_string()]),
    ];

    // Add optional icon with MIME type: ["icon", "<url>", "<mime-type>"]
    if let Some((icon_url, mime_type)) = icon_info {
        tags.push(Tag::custom(
            "icon",
            vec![icon_url.to_string(), mime_type.to_string()]
        ));
    }

    // Add optional developer name
    if let Some(dev) = developer {
        if !dev.is_empty() {
            tags.push(Tag::custom(
                "dev",
                vec![dev.to_string()]
            ));
        }
    }

    // Add optional source URL
    if let Some(src) = source_url {
        if !src.is_empty() {
            tags.push(Tag::custom(
                "src",
                vec![src.to_string()]
            ));
        }
    }

    // Add category tags
    for category in categories {
        tags.push(Tag::custom("t", vec![category.to_string()]));
    }

    // Add optional permissions tag
    if let Some(perms) = permissions {
        if !perms.is_empty() {
            tags.push(Tag::custom(
                "permissions",
                vec![perms.to_string()]
            ));
        }
    }

    let content = changelog.unwrap_or("");

    let event_builder = EventBuilder::new(Kind::from(MINIAPP_MARKETPLACE_KIND), content)
        .tags(tags);

    event_builder
        .finalize_async(signer)
        .await
        .map_err(|e| format!("Failed to sign marketplace event: {}", e))
}

#[cfg(test)]
mod app_id_tests {
    use super::is_safe_app_id;

    #[test]
    fn accepts_normal_ids() {
        for id in ["com.example.app", "my-cool-app", "abc123", "app.v2", "a"] {
            assert!(is_safe_app_id(id), "should accept {:?}", id);
        }
        assert!(is_safe_app_id(&"x".repeat(128)), "128 chars should be accepted");
    }

    #[test]
    fn rejects_traversal_and_unsafe() {
        for id in ["", "..", "....", "a..b", "../../etc/passwd", "..\\win",
                   "foo/bar", "foo\\bar", "/abs", "C:\\x", "app id", "café"] {
            assert!(!is_safe_app_id(id), "should reject {:?}", id);
        }
        assert!(!is_safe_app_id(&"x".repeat(129)), "129 chars should be rejected");
    }
}
