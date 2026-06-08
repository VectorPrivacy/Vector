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

/// Build the shareable invite URL. The fragment = base64url(JSON{v, relays, token}); the
/// relays are needed because a fresh joiner has nowhere else to look for the bundle.
pub fn encode_invite_url(relays: &[String], token: &[u8; 32]) -> String {
    let frag = UrlFragment {
        v: 1,
        relays: relays.to_vec(),
        t: crate::simd::hex::bytes_to_hex_32(token),
    };
    let json = serde_json::to_string(&frag).expect("UrlFragment serializes");
    let b64 = base64_simd::URL_SAFE_NO_PAD.encode_to_string(json.as_bytes());
    format!("{INVITE_URL_BASE}#{b64}")
}

/// Parse a shareable invite URL (or a bare fragment) back to `(relays, token)`. Accepts
/// the full URL or just the fragment after `#`.
pub fn parse_invite_url(url: &str) -> Result<(Vec<String>, [u8; 32]), PublicInviteError> {
    let fragment = url.rsplit_once('#').map(|(_, f)| f).unwrap_or(url);
    if fragment.is_empty() {
        return Err(PublicInviteError::BadUrl("no fragment".into()));
    }
    let json = base64_simd::URL_SAFE_NO_PAD
        .decode_to_vec(fragment.as_bytes())
        .map_err(|e| PublicInviteError::BadUrl(format!("base64: {e}")))?;
    let frag: UrlFragment =
        serde_json::from_slice(&json).map_err(|e| PublicInviteError::BadUrl(format!("json: {e}")))?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let many: Vec<String> = (0..MAX_URL_RELAYS + 1).map(|i| format!("wss://r{i}")).collect();
        let url = encode_invite_url(&many, &token);
        assert!(parse_invite_url(&url).is_err());
        // Exactly at the cap still parses.
        let ok: Vec<String> = (0..MAX_URL_RELAYS).map(|i| format!("wss://r{i}")).collect();
        assert!(parse_invite_url(&encode_invite_url(&ok, &token)).is_ok());
    }
}
