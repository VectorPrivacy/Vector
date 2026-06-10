//! Public (link) invites (GROUP_PROTOCOL.md).
//!
//! A public invite is a shareable URL — `https://vectorapp.io/invite#<fragment>` — whose
//! `#fragment` carries a **fetch-token**, never the keys directly. The token decrypts a
//! bundle posted on the community's relays. This indirection (token → relay bundle) is
//! what buys Discord-style rotate/revoke/expire without changing the URL.
//!
//! From the token, three sub-keys derive ([`super::derive`]):
//! - a NIP-44 **decryption key** for the bundle content;
//! - a **locator** (the addressable `d`-tag) so the bundle is findable on relays;
//! - a stable **signer key**, so re-posting under one coordinate rotates the link and a
//!   joiner can reject an impostor bundle squatting the locator.
//!
//! The bundle carries the same join material a private invite does ([`CommunityInvite`])
//! plus a **preview** (name, description, logo) so a recipient sees what they're joining
//! before committing. The preview lives inside the token-gated ciphertext, so a relay
//! scraper without the link sees only an opaque blob — metadata privacy holds.

use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use super::derive::{public_invite_key, public_invite_locator, public_invite_signer};
use super::invite::{build_invite, CommunityInvite};
use super::{cipher, Community, CommunityImage};
use crate::stored_event::event_kind;

/// Default invite URL base. The path is irrelevant to the protocol (the fragment is read
/// client-side and never sent to the server); only the fragment matters.
pub const INVITE_URL_BASE: &str = "https://vectorapp.io/invite";

/// Cap on relays embedded in an invite URL. The URL is attacker-controlled (anyone can
/// craft one and share it), and its relays feed straight into the connect loop on
/// preview/accept — bound it so a hostile link can't fan out connections.
const MAX_URL_RELAYS: usize = 32;

const TAG_VERSION: &str = "v";
const TAG_SUBKIND: &str = "vsk";
const PROTOCOL_VERSION: &str = "1";
/// Sub-kind for a public-invite bundle event (append-only enum; distinct author
/// coordinate, but tagged for explicit parse-time disambiguation).
const VSK_PUBLIC_INVITE: &str = "6";
/// Sub-kind for a REVOCATION tombstone — the empty replaceable event that overwrites a revoked bundle at
/// its coordinate (append-only). A self-describing marker so a fetcher (the invite preview page, a
/// joining client) reads "this invite was revoked" instantly, instead of choking on an empty decrypt.
const VSK_PUBLIC_INVITE_REVOKED: &str = "9";

/// The non-secret community details shown before joining (no member count — the protocol
/// hides membership, so any count would be a fiction).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicInvitePreview {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Logo (encrypted blob ref). The per-image key rides here; the joiner also receives
    /// the server-root key via `join`, so this exposes nothing they aren't already given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<CommunityImage>,
}

/// The full decrypted bundle: a preview + the join material + an optional expiry + attribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicInviteBundle {
    pub preview: PublicInvitePreview,
    pub join: CommunityInvite,
    /// Unix seconds after which clients refuse the link (`None` = no expiry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// Attribution (metrics): the link creator's npub (bech32) so a joiner can announce "invited by
    /// X" in their join Presence. `serde(default)` so older bundles (no attribution) still parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_npub: Option<String>,
    /// Creator-set label for the link ("Reddit", "Conf 2026") — the metric's human-readable bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl PublicInviteBundle {
    pub fn is_expired(&self, now_secs: u64) -> bool {
        self.expires_at.map_or(false, |e| now_secs >= e)
    }
}

/// Errors building or parsing a public invite.
#[derive(Debug)]
pub enum PublicInviteError {
    Json(String),
    Cipher(String),
    Sign(String),
    /// Bundle event not signed by the token-derived signer (impostor at the locator).
    UnexpectedSigner,
    BadSignature,
    WrongVersion(Option<String>),
    WrongSubKind(Option<String>),
    BadUrl(String),
    Expired,
    /// The coordinate holds a token-signed revocation tombstone — the link was explicitly revoked.
    Revoked,
}

impl std::fmt::Display for PublicInviteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PublicInviteError::Json(e) => write!(f, "json: {e}"),
            PublicInviteError::Cipher(e) => write!(f, "cipher: {e}"),
            PublicInviteError::Sign(e) => write!(f, "sign: {e}"),
            PublicInviteError::UnexpectedSigner => write!(f, "bundle not signed by the invite token"),
            PublicInviteError::BadSignature => write!(f, "bundle signature invalid"),
            PublicInviteError::WrongVersion(v) => write!(f, "unsupported invite version: {v:?}"),
            PublicInviteError::WrongSubKind(v) => write!(f, "not a public-invite bundle: {v:?}"),
            PublicInviteError::BadUrl(e) => write!(f, "bad invite url: {e}"),
            PublicInviteError::Expired => write!(f, "invite has expired"),
            PublicInviteError::Revoked => write!(f, "this invite was revoked"),
        }
    }
}

impl std::error::Error for PublicInviteError {}

/// Mint a fresh 32-byte token (OsRng). The whole secret of a public invite.
pub fn new_token() -> [u8; 32] {
    super::random_32()
}

/// Snapshot a Community's current display metadata into a preview.
fn preview_of(community: &Community) -> PublicInvitePreview {
    PublicInvitePreview {
        name: community.name.clone(),
        description: community.description.clone(),
        icon: community.icon.clone(),
    }
}

/// Build the signed, token-encrypted bundle event to post on the community's relays.
/// Addressable (kind 30078) at coordinate `(30078, signer(token), d=locator(token))` so
/// re-posting under the same token replaces it (rotate); deleting it revokes the link.
pub fn build_public_invite_event(
    community: &Community,
    token: &[u8; 32],
    expires_at: Option<u64>,
    creator_npub: Option<String>,
    label: Option<String>,
) -> Result<Event, PublicInviteError> {
    let bundle = PublicInviteBundle {
        preview: preview_of(community),
        join: build_invite(community),
        expires_at,
        creator_npub,
        label,
    };
    let json = serde_json::to_string(&bundle).map_err(|e| PublicInviteError::Json(e.to_string()))?;
    let content = cipher::seal(&public_invite_key(token), json.as_bytes())
        .map_err(PublicInviteError::Cipher)?;
    let signer = Keys::new(public_invite_signer(token));
    let locator = crate::simd::hex::bytes_to_hex_32(&public_invite_locator(token));
    EventBuilder::new(Kind::Custom(event_kind::APPLICATION_SPECIFIC), content)
        .tags([
            Tag::identifier(locator),
            Tag::custom(TagKind::Custom(TAG_SUBKIND.into()), [VSK_PUBLIC_INVITE.to_string()]),
            Tag::custom(TagKind::Custom(TAG_VERSION.into()), [PROTOCOL_VERSION.to_string()]),
        ])
        .sign_with_keys(&signer)
        .map_err(|e| PublicInviteError::Sign(e.to_string()))
}

/// Build a token-signed TOMBSTONE at the bundle's coordinate: an empty-content replaceable event with a
/// fresh `created_at` that REPLACES the live bundle. Relays honor replaceable-event replacement far more
/// reliably than NIP-09 `a`-tag (coordinate) deletions of addressable events — so publishing this on
/// revoke, alongside the deletion, guarantees the bundle is overwritten (the browser preview dies) even on
/// relays that silently ignore coordinate deletions. The empty content fails to decrypt to a valid bundle,
/// so a fetcher gets nothing.
pub fn build_public_invite_tombstone(token: &[u8; 32]) -> Result<Event, PublicInviteError> {
    let signer = Keys::new(public_invite_signer(token));
    let locator = crate::simd::hex::bytes_to_hex_32(&public_invite_locator(token));
    EventBuilder::new(Kind::Custom(event_kind::APPLICATION_SPECIFIC), "")
        .tags([
            Tag::identifier(locator),
            Tag::custom(TagKind::Custom(TAG_SUBKIND.into()), [VSK_PUBLIC_INVITE_REVOKED.to_string()]),
            Tag::custom(TagKind::Custom(TAG_VERSION.into()), [PROTOCOL_VERSION.to_string()]),
        ])
        .sign_with_keys(&signer)
        .map_err(|e| PublicInviteError::Sign(e.to_string()))
}

/// The addressable `d`-tag (hex locator) a public invite for `token` is posted under —
/// the value to query relays by.
pub fn locator_hex(token: &[u8; 32]) -> String {
    crate::simd::hex::bytes_to_hex_32(&public_invite_locator(token))
}

/// The public key a valid bundle for `token` must be signed by.
pub fn signer_pubkey(token: &[u8; 32]) -> PublicKey {
    Keys::new(public_invite_signer(token)).public_key()
}

/// Verify + decrypt a bundle event with the URL token. Checks: protocol version (before
/// decrypt), sub-kind, that the author is the token-derived signer (rejects an impostor
/// squatting the locator), the signature, then decrypts under the token key. Does NOT
/// enforce expiry (so a preview can still render an expired link); callers gate joins on
/// [`PublicInviteBundle::is_expired`].
pub fn parse_public_invite_event(
    event: &Event,
    token: &[u8; 32],
) -> Result<PublicInviteBundle, PublicInviteError> {
    let version = find_tag(event, TAG_VERSION);
    if version.as_deref() != Some(PROTOCOL_VERSION) {
        return Err(PublicInviteError::WrongVersion(version));
    }
    // Author + signature FIRST, so only a genuine token-signed event at the coordinate gets a verdict —
    // an impostor squatting the locator can spoof neither a bundle NOR a revocation.
    if event.pubkey != signer_pubkey(token) {
        return Err(PublicInviteError::UnexpectedSigner);
    }
    event.verify().map_err(|_| PublicInviteError::BadSignature)?;
    let subkind = find_tag(event, TAG_SUBKIND);
    match subkind.as_deref() {
        Some(VSK_PUBLIC_INVITE) => {}
        // A token-signed revocation tombstone (overwrites the bundle on revoke) → explicit "revoked".
        Some(VSK_PUBLIC_INVITE_REVOKED) => return Err(PublicInviteError::Revoked),
        other => return Err(PublicInviteError::WrongSubKind(other.map(|s| s.to_string()))),
    }
    let plaintext =
        cipher::open(&public_invite_key(token), &event.content).map_err(PublicInviteError::Cipher)?;
    serde_json::from_slice(&plaintext).map_err(|e| PublicInviteError::Json(e.to_string()))
}

fn find_tag(event: &Event, name: &str) -> Option<String> {
    event.tags.iter().find_map(|t| {
        let s = t.as_slice();
        (s.len() >= 2 && s[0] == name).then(|| s[1].clone())
    })
}

// --- URL encoding (the #fragment carries everything; the path/host are cosmetic) ---

#[derive(Serialize, Deserialize)]
struct UrlFragment {
    v: u8,
    /// Bootstrap relays — a fresh joiner has no community context to find the bundle.
    relays: Vec<String>,
    /// The fetch-token, hex.
    t: String,
}

/// v2 fragment version byte. A v1 fragment is base64url(JSON) whose first decoded byte is
/// `{` (0x7B), so the first payload byte discriminates the two formats for free.
const URL_V2: u8 = 2;
/// v2 flags bit: the community runs on the stock [`crate::state::TRUSTED_RELAYS`] set,
/// encoded as ZERO bytes — the parser (and the website's copy) reconstitutes it.
const V2_FLAG_DEFAULT_RELAYS: u8 = 0b0000_0001;
/// Mint-side cap on explicit v2 bootstrap relays: the URL only has to FIND the bundle (the
/// bundle carries the authoritative relay set), and 3 rides out one sick relay.
const MAX_V2_BOOTSTRAP_RELAYS: usize = 3;

/// Append-only dictionary of well-known relays for v2 links — a listed relay costs ONE byte
/// instead of a literal string. Ids live in 1..=254 (0 = wss-implied literal, 255 = verbatim
/// literal). NEVER renumber or remove entries (ids are baked into minted links forever);
/// append only, in lockstep with the website's /invite parser copy.
const RELAY_DICTIONARY: &[&str] = &[
    "wss://jskitty.com/nostr",        // id 1
    "wss://asia.vectorapp.io/nostr",  // id 2
    "wss://nostr.computingcache.com", // id 3
    "wss://relay.damus.io",           // id 4
];

/// Order/case/trailing-slash-insensitive relay comparison key.
fn norm_relay(r: &str) -> String {
    r.trim().trim_end_matches('/').to_ascii_lowercase()
}

fn is_default_relay_set(relays: &[String]) -> bool {
    let defaults: std::collections::HashSet<String> =
        crate::state::TRUSTED_RELAYS.iter().map(|r| norm_relay(r)).collect();
    let theirs: std::collections::HashSet<String> = relays.iter().map(|r| norm_relay(r)).collect();
    theirs == defaults
}

fn dictionary_id(relay: &str) -> Option<u8> {
    let n = norm_relay(relay);
    RELAY_DICTIONARY.iter().position(|d| norm_relay(d) == n).map(|i| (i + 1) as u8)
}

/// Build the shareable invite URL (v2 binary fragment): `[ver][flags][relays?][token:32]`,
/// base64url. The stock relay set costs zero bytes (flag bit); known relays cost one byte
/// each (dictionary); customs are length-prefixed literals with `wss://` implied. ~74 chars
/// total in the common case, vs ~269 for the v1 JSON fragment it replaces.
pub fn encode_invite_url(relays: &[String], token: &[u8; 32]) -> String {
    let mut payload: Vec<u8> = Vec::with_capacity(34);
    payload.push(URL_V2);
    if is_default_relay_set(relays) {
        payload.push(V2_FLAG_DEFAULT_RELAYS);
    } else {
        payload.push(0);
        // Bootstrap only — the bundle carries the authoritative set. Relays over 255 bytes
        // can't length-prefix; skip them (absurd in practice).
        let boot: Vec<&String> = relays
            .iter()
            .filter(|r| r.strip_prefix("wss://").unwrap_or(r).len() <= 255)
            .take(MAX_V2_BOOTSTRAP_RELAYS)
            .collect();
        payload.push(boot.len() as u8);
        for r in boot {
            match dictionary_id(r) {
                Some(id) => payload.push(id),
                None => {
                    // Literal: wss:// (the overwhelmingly common scheme) rides implied as
                    // entry 0; anything else is stored VERBATIM as entry 255 so the string
                    // round-trips exactly (ws://, exotic schemes, test relays).
                    let (kind, s) = match r.strip_prefix("wss://") {
                        Some(host) => (0u8, host),
                        None => (255u8, r.as_str()),
                    };
                    payload.push(kind);
                    payload.push(s.len() as u8);
                    payload.extend_from_slice(s.as_bytes());
                }
            }
        }
    }
    payload.extend_from_slice(token);
    let b64 = base64_simd::URL_SAFE_NO_PAD.encode_to_string(&payload);
    format!("{INVITE_URL_BASE}#{b64}")
}

/// Parse a shareable invite URL (or a bare fragment) back to `(relays, token)`. Accepts
/// the full URL or just the fragment after `#`, in either the v2 binary format or the
/// legacy v1 JSON format (v1 links in the wild stay valid forever).
pub fn parse_invite_url(url: &str) -> Result<(Vec<String>, [u8; 32]), PublicInviteError> {
    let fragment = url.rsplit_once('#').map(|(_, f)| f).unwrap_or(url);
    if fragment.is_empty() {
        return Err(PublicInviteError::BadUrl("no fragment".into()));
    }
    let raw = base64_simd::URL_SAFE_NO_PAD
        .decode_to_vec(fragment.as_bytes())
        .map_err(|e| PublicInviteError::BadUrl(format!("base64: {e}")))?;
    match raw.first() {
        Some(&URL_V2) => parse_v2_fragment(&raw),
        Some(&b'{') => parse_v1_fragment(&raw),
        _ => Err(PublicInviteError::BadUrl("unrecognized fragment format".into())),
    }
}

fn parse_v1_fragment(json: &[u8]) -> Result<(Vec<String>, [u8; 32]), PublicInviteError> {
    let frag: UrlFragment =
        serde_json::from_slice(json).map_err(|e| PublicInviteError::BadUrl(format!("json: {e}")))?;
    if frag.v != 1 {
        return Err(PublicInviteError::BadUrl(format!("unsupported url version {}", frag.v)));
    }
    if frag.relays.len() > MAX_URL_RELAYS {
        return Err(PublicInviteError::BadUrl(format!(
            "invite url declares too many relays ({})",
            frag.relays.len()
        )));
    }
    if frag.t.len() != 64 {
        return Err(PublicInviteError::BadUrl("token must be 64 hex chars".into()));
    }
    let mut token = [0u8; 32];
    for (i, byte) in token.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&frag.t[i * 2..i * 2 + 2], 16)
            .map_err(|_| PublicInviteError::BadUrl("invalid hex in token".into()))?;
    }
    Ok((frag.relays, token))
}

fn parse_v2_fragment(raw: &[u8]) -> Result<(Vec<String>, [u8; 32]), PublicInviteError> {
    let bad = |m: &str| PublicInviteError::BadUrl(m.into());
    let flags = *raw.get(1).ok_or_else(|| bad("truncated v2 fragment"))?;
    let mut pos = 2usize;
    let relays: Vec<String> = if flags & V2_FLAG_DEFAULT_RELAYS != 0 {
        crate::state::TRUSTED_RELAYS.iter().map(|s| s.to_string()).collect()
    } else {
        // Parse cap is the lax v1 cap (not the mint cap) so a future build minting more
        // bootstrap relays stays readable here.
        let count = *raw.get(pos).ok_or_else(|| bad("truncated v2 relay count"))? as usize;
        pos += 1;
        if count == 0 || count > MAX_URL_RELAYS {
            return Err(bad("bad v2 relay count"));
        }
        let mut out = Vec::with_capacity(count.min(MAX_V2_BOOTSTRAP_RELAYS));
        for _ in 0..count {
            let id = *raw.get(pos).ok_or_else(|| bad("truncated v2 relay entry"))?;
            pos += 1;
            if id == 0 || id == 255 {
                let len = *raw.get(pos).ok_or_else(|| bad("truncated v2 relay literal"))? as usize;
                pos += 1;
                let end = pos.checked_add(len).ok_or_else(|| bad("bad v2 relay length"))?;
                let host = raw.get(pos..end).ok_or_else(|| bad("truncated v2 relay literal"))?;
                let host =
                    std::str::from_utf8(host).map_err(|_| bad("v2 relay literal not utf-8"))?;
                out.push(if id == 0 { format!("wss://{host}") } else { host.to_string() });
                pos = end;
            } else if let Some(relay) = RELAY_DICTIONARY.get(id as usize - 1) {
                out.push(relay.to_string());
            }
            // Unknown dictionary id = an entry appended by a NEWER build — skip it
            // (forward-compat); the remaining entries still bootstrap.
        }
        if out.is_empty() {
            return Err(bad("no resolvable bootstrap relays"));
        }
        out
    };
    let token_bytes = raw.get(pos..).ok_or_else(|| bad("truncated v2 token"))?;
    if token_bytes.len() != 32 {
        return Err(bad("v2 token must be exactly 32 bytes"));
    }
    let mut token = [0u8; 32];
    token.copy_from_slice(token_bytes);
    Ok((relays, token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_url_default_relay_set_roundtrips_and_is_tiny() {
        let relays: Vec<String> =
            crate::state::TRUSTED_RELAYS.iter().map(|s| s.to_string()).collect();
        let token = new_token();
        let url = encode_invite_url(&relays, &token);
        assert!(url.len() <= 80, "default-set v2 url should be ~74 chars, got {}", url.len());
        let (parsed_relays, parsed_token) = parse_invite_url(&url).unwrap();
        assert_eq!(parsed_token, token);
        assert_eq!(parsed_relays, relays);
    }

    #[test]
    fn v2_url_dictionary_and_literal_relays_roundtrip() {
        // One of each entry kind: dictionary, wss-implied literal, verbatim (schemeless) literal.
        let relays = vec![
            "wss://relay.damus.io".to_string(),
            "wss://my.custom.relay/nostr".to_string(),
            "r1".to_string(),
        ];
        let token = new_token();
        let url = encode_invite_url(&relays, &token);
        let (parsed_relays, parsed_token) = parse_invite_url(&url).unwrap();
        assert_eq!(parsed_token, token);
        assert_eq!(parsed_relays, relays);
    }

    #[test]
    fn v1_json_fragment_still_parses() {
        // Links minted before v2 live in chats forever — the old JSON format must keep parsing.
        let token = new_token();
        let json = format!(
            "{{\"v\":1,\"relays\":[\"wss://r1\",\"wss://r2\"],\"t\":\"{}\"}}",
            crate::simd::hex::bytes_to_hex_32(&token)
        );
        let frag = base64_simd::URL_SAFE_NO_PAD.encode_to_string(json.as_bytes());
        let (relays, parsed) = parse_invite_url(&format!("{INVITE_URL_BASE}#{frag}")).unwrap();
        assert_eq!(parsed, token);
        assert_eq!(relays, vec!["wss://r1".to_string(), "wss://r2".to_string()]);
    }

    #[test]
    fn v2_unknown_dictionary_id_is_skipped_not_fatal() {
        // Forward-compat: an id appended by a NEWER build must not brick the link on an old
        // client — the remaining entries still bootstrap.
        let token = new_token();
        let mut payload = vec![2u8, 0, 2, 250, 1]; // count=2: unknown id 250, then id 1
        payload.extend_from_slice(&token);
        let frag = base64_simd::URL_SAFE_NO_PAD.encode_to_string(&payload);
        let (relays, parsed) = parse_invite_url(&frag).unwrap();
        assert_eq!(parsed, token);
        assert_eq!(relays, vec!["wss://jskitty.com/nostr".to_string()]);
    }

    #[test]
    fn v2_garbage_and_truncations_error_without_panic() {
        for frag_bytes in [
            vec![2u8],               // just the version byte
            vec![2u8, 1],            // default flag, no token
            vec![2u8, 0, 1, 0, 200], // literal claims 200 bytes, has none
            vec![2u8, 1, 0xAA],      // token wrong length
            vec![9u8, 1, 2, 3],      // unknown version byte
        ] {
            let frag = base64_simd::URL_SAFE_NO_PAD.encode_to_string(&frag_bytes);
            assert!(parse_invite_url(&frag).is_err());
        }
    }

    #[test]
    fn bundle_round_trips_with_token() {
        let mut c = Community::create("Cool Place", "general", vec!["wss://r1".into()]);
        c.description = Some("the coolest".into());
        let token = new_token();
        let event = build_public_invite_event(&c, &token, None, None, None).unwrap();

        // Posted under the token-derived coordinate.
        assert_eq!(event.pubkey, signer_pubkey(&token));
        assert_eq!(find_tag(&event, "d").as_deref(), Some(locator_hex(&token).as_str()));

        let bundle = parse_public_invite_event(&event, &token).unwrap();
        assert_eq!(bundle.preview.name, "Cool Place");
        assert_eq!(bundle.preview.description.as_deref(), Some("the coolest"));
        // The join material actually works: reconstruct a member and read the keys.
        let member = super::super::invite::accept_invite(&bundle.join).unwrap();
        assert_eq!(member.id, c.id);
        assert_eq!(member.channels[0].key.as_bytes(), c.channels[0].key.as_bytes());
    }

    #[test]
    fn tombstone_replaces_the_bundle_at_the_same_coordinate() {
        // Revoke overwrites the live bundle with a tombstone at the SAME coordinate so relays that ignore
        // `a`-tag deletions still replace it (replaceable-event semantics) → the browser preview dies.
        let c = Community::create("HQ", "general", vec!["wss://r1".into()]);
        let token = new_token();
        let bundle = build_public_invite_event(&c, &token, None, None, None).unwrap();
        let tomb = build_public_invite_tombstone(&token).unwrap();
        // Same (kind, pubkey, d-tag) → a relay replaces the bundle with the tombstone.
        assert_eq!(tomb.kind, bundle.kind);
        assert_eq!(tomb.pubkey, bundle.pubkey);
        assert_eq!(find_tag(&tomb, "d"), find_tag(&bundle, "d"));
        // Empty content + the revoked subkind → a fetcher gets an explicit "revoked" verdict, not a vague
        // decrypt failure (so the preview page can show "this invite was revoked" instantly).
        assert!(tomb.content.is_empty());
        assert!(
            matches!(parse_public_invite_event(&tomb, &token), Err(PublicInviteError::Revoked)),
            "tombstone parses as an explicit Revoked, not a bundle",
        );
    }

    #[test]
    fn wrong_token_cannot_read_bundle() {
        let c = Community::create("HQ", "general", vec![]);
        let token = new_token();
        let event = build_public_invite_event(&c, &token, None, None, None).unwrap();

        // A different token derives a different signer pubkey → rejected before decrypt.
        let other = new_token();
        let err = parse_public_invite_event(&event, &other);
        assert!(matches!(err, Err(PublicInviteError::UnexpectedSigner)), "got {err:?}");
    }

    #[test]
    fn impostor_at_locator_is_rejected() {
        // An attacker posts their OWN event at the right locator d-tag, signed by their
        // own key. A joiner with the token must reject it (author != token signer).
        let c = Community::create("HQ", "general", vec![]);
        let token = new_token();
        let attacker = Keys::generate();
        let impostor = EventBuilder::new(Kind::Custom(event_kind::APPLICATION_SPECIFIC), "x")
            .tags([
                Tag::identifier(locator_hex(&token)),
                Tag::custom(TagKind::Custom(TAG_SUBKIND.into()), [VSK_PUBLIC_INVITE.to_string()]),
                Tag::custom(TagKind::Custom(TAG_VERSION.into()), [PROTOCOL_VERSION.to_string()]),
            ])
            .sign_with_keys(&attacker)
            .unwrap();
        let _ = &c;
        assert!(matches!(
            parse_public_invite_event(&impostor, &token),
            Err(PublicInviteError::UnexpectedSigner)
        ));
    }

    #[test]
    fn expiry_is_reported() {
        let c = Community::create("HQ", "general", vec![]);
        let token = new_token();
        let event = build_public_invite_event(&c, &token, Some(1000), None, None).unwrap();
        let bundle = parse_public_invite_event(&event, &token).unwrap();
        assert!(!bundle.is_expired(999));
        assert!(bundle.is_expired(1000), "expiry is inclusive");
        assert!(bundle.is_expired(2000));
    }

    #[test]
    fn url_round_trips() {
        let relays = vec!["wss://a".to_string(), "wss://b".to_string()];
        let token = new_token();
        let url = encode_invite_url(&relays, &token);
        assert!(url.starts_with(INVITE_URL_BASE));
        assert!(url.contains('#'));
        let (r, t) = parse_invite_url(&url).unwrap();
        assert_eq!(r, relays);
        assert_eq!(t, token);
        // A bare fragment (no scheme/host) also parses.
        let frag = url.rsplit_once('#').unwrap().1;
        assert_eq!(parse_invite_url(frag).unwrap().1, token);
    }

    #[test]
    fn malformed_url_errors() {
        assert!(parse_invite_url("https://vectorapp.io/invite#").is_err());
        assert!(parse_invite_url("https://vectorapp.io/invite#not!!base64").is_err());
    }

    #[test]
    fn url_with_too_many_relays_is_rejected() {
        // A hostile link can't fan out unbounded relay connections on preview/accept.
        let token = new_token();
        // v2 minting CAPS the bootstrap set (the bundle carries the authoritative relays)…
        let many: Vec<String> = (0..MAX_URL_RELAYS + 1).map(|i| format!("wss://r{i}")).collect();
        let (relays, _) = parse_invite_url(&encode_invite_url(&many, &token)).unwrap();
        assert!(relays.len() <= MAX_V2_BOOTSTRAP_RELAYS);
        // …a hand-crafted v2 fragment claiming an absurd count is refused outright…
        let mut payload = vec![URL_V2, 0, (MAX_URL_RELAYS + 1) as u8];
        payload.extend_from_slice(&token);
        let frag = base64_simd::URL_SAFE_NO_PAD.encode_to_string(&payload);
        assert!(parse_invite_url(&frag).is_err());
        // …and v1 JSON fragments keep their original cap.
        let json = format!(
            "{{\"v\":1,\"relays\":[{}],\"t\":\"{}\"}}",
            (0..MAX_URL_RELAYS + 1).map(|i| format!("\"wss://r{i}\"")).collect::<Vec<_>>().join(","),
            crate::simd::hex::bytes_to_hex_32(&token)
        );
        let frag = base64_simd::URL_SAFE_NO_PAD.encode_to_string(json.as_bytes());
        assert!(parse_invite_url(&frag).is_err());
    }
}
