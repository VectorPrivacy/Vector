//! CORD-05 Invites — how members are handed the keys that make them members.
//!
//! Two delivery lanes share one payload, the [`CommunityInvite`] **bundle**:
//!   - **Public link** — the bundle rides relays as an addressable event
//!     ([`kind::INVITE_BUNDLE`], empty `d`) authored by a throwaway per-link
//!     keypair, encrypted under a key nobody on the network holds (derived off
//!     the link's 16-byte unlock token). The link is `(naddr, #fragment)`: the
//!     naddr is a bare public locator, the fragment carries the token + bootstrap
//!     relays and never reaches a server. Only the *creator* (holder of the
//!     link_signer secret, synced in their [`InviteList`]) can refresh or
//!     tombstone the coordinate, so a link-holder can join but never squat or
//!     kill the link (§2).
//!   - **Direct invite** — when the invitee is a known npub the machinery drops
//!     away: the bundle giftwraps straight to them as a STANDARD NIP-59 wrap
//!     ([`build_direct_invite`], §6), not the reversed stream wrap of CORD-01.
//!
//! The inviter's identity is irrelevant to trust: the `community_id`
//! self-certifies the owner (CORD-02 A.4), so no bundle can smuggle a false
//! owner or a fake key for a real Community. Every bundle passes [`CommunityInvite::validate`]
//! before it is trusted, whichever lane carried it.

use nostr_sdk::nips::nip44::{
    self,
    v2::{decrypt_to_bytes, encrypt_to_bytes, ConversationKey},
};
use nostr_sdk::nips::nip59::RANGE_RANDOM_TIMESTAMP_TWEAK;
use nostr_sdk::prelude::{
    Event, EventBuilder, FromBech32, JsonUtil, Keys, Kind, PublicKey, Tag, TagKind, Timestamp, ToBech32, UnsignedEvent,
};
use serde::{Deserialize, Serialize};

use super::super::{cap_relays, CommunityId};
use super::control::ImageRef;
use super::derive::{verify_community_id, TOKEN_LEN};
use super::{kind, vsk};

/// Hostile-bundle bound: a bundle is attacker-crafted input reached by following
/// a link, so reject one carrying more Channels than a Community could sanely
/// hold before allocating on its claims (CORD-05 §1).
pub const MAX_BUNDLE_CHANNELS: usize = 256;

/// Sanity ceiling on a bundle's `root_epoch` / channel epochs. Not a commitment
/// (epochs aren't owner-signed), just a bound so attacker-set values can't push a
/// later `epoch + 1` toward overflow. `2^40` is astronomically above any real
/// rotation count.
pub const MAX_BUNDLE_EPOCH: u64 = 1 << 40;

/// The fragment format byte, which also selects the relay-dictionary generation
/// (CORD-05 §3). Bumping it re-labels the dictionary universe.
pub const FRAGMENT_VERSION: u8 = 4;

/// The fragment carries at most this many bootstrap relays — it only needs to
/// *find* the bundle, which then carries the authoritative set (CORD-05 §3).
pub const MAX_BOOTSTRAP_RELAYS: usize = 3;

/// `flags` bit 0: the stock set is in use, so zero relay bytes follow.
const FLAG_STOCK_SET: u8 = 0x01;

/// The stock relay dictionary, generation 4 — four primaries every client knows,
/// referenced by a single byte (id = index + 1). Both Vector and Soapbox ship it
/// identically, so an invite minted by either opens in the other. Append-only:
/// growing it is a new generation, editing an entry re-labels existing links.
const RELAY_DICT: [&str; 4] = [
    "wss://jskitty.com/nostr",       // id 1 (Vector)
    "wss://asia.vectorapp.io/nostr", // id 2 (Vector)
    "wss://relay.ditto.pub",         // id 3 (Soapbox)
    "wss://relay.dreamith.to",       // id 4 (Soapbox)
];

// ── Errors ───────────────────────────────────────────────────────────────────

/// Errors from the invite layer.
#[derive(Debug)]
pub enum InviteError {
    Json(String),
    /// A hex field wasn't 32 valid bytes.
    BadHex(&'static str),
    /// More Channels than [`MAX_BUNDLE_CHANNELS`].
    TooManyChannels(usize),
    /// `(owner, owner_salt)` fail to reproduce `community_id` (CORD-02 A.4).
    OwnerMismatch,
    /// A malformed invite fragment (base64, truncation, trailing bytes, caps).
    BadFragment(&'static str),
    /// A fragment version this client won't decode (legacy or future).
    BadVersion(u8),
    /// A link/naddr that isn't a recognizable invite coordinate.
    BadLink(&'static str),
    /// A bundle event failed a wire gate: wrong kind, wrong author, bad
    /// signature, or an unknown/missing `vsk` marker.
    BadEvent(&'static str),
    /// NIP-44 / NIP-19 / signing failure.
    Crypto(String),
}

impl std::fmt::Display for InviteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InviteError::Json(e) => write!(f, "json: {e}"),
            InviteError::BadHex(field) => write!(f, "field {field} is not 32-byte hex"),
            InviteError::TooManyChannels(n) => write!(f, "bundle carries {n} channels (cap {MAX_BUNDLE_CHANNELS})"),
            InviteError::OwnerMismatch => write!(f, "bundle owner does not reproduce its community_id"),
            InviteError::BadFragment(why) => write!(f, "bad invite fragment: {why}"),
            InviteError::BadVersion(v) => write!(f, "unsupported invite fragment version {v}"),
            InviteError::BadLink(why) => write!(f, "bad invite link: {why}"),
            InviteError::BadEvent(why) => write!(f, "bad invite bundle event: {why}"),
            InviteError::Crypto(e) => write!(f, "crypto: {e}"),
        }
    }
}

impl std::error::Error for InviteError {}

// ── 1. The Bundle ─────────────────────────────────────────────────────────────

/// One granted Channel inside a bundle (CORD-05 §1). Public Channels derive from
/// the `community_root`; a private Channel's independent `key` travels here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelGrant {
    /// Channel id (32-byte hex).
    pub id: String,
    /// Channel key (32-byte hex).
    pub key: String,
    pub epoch: u64,
    pub name: String,
}

/// The `CommunityInvite` bundle (CORD-05 §1). Field names are wire-frozen and
/// shared with Soapbox/Armada; a rename is a silent cross-client join failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityInvite {
    /// `sha256("concord/community" || owner || owner_salt)` — self-certifies the owner.
    pub community_id: String,
    /// Owner x-only pubkey (32-byte hex).
    pub owner: String,
    /// Owner salt (32-byte hex).
    pub owner_salt: String,
    /// The base access key (32-byte hex) at `root_epoch`.
    pub community_root: String,
    pub root_epoch: u64,
    pub channels: Vec<ChannelGrant>,
    pub relays: Vec<String>,
    /// Preview name so a parked invite renders; the Control fold is the authority.
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<ImageRef>,
    /// Optional, unix **ms**: past it the preview still renders, joining refuses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// Optional attribution, echoed in the joiner's Guestbook Join (CORD-05 §1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creator_npub: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Unknown fields round-trip verbatim (CORD-02 §6) — carries other clients'
    /// bundle extensions (`held_roots`, `refounder`, …) through untouched.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl CommunityInvite {
    /// Parse + bound + validate a decrypted bundle, whichever lane carried it:
    /// truncate `relays` to the Community cap, reject an over-count of Channels
    /// before trusting it, and verify the owner commitment.
    pub fn from_bundle_json(json: &str) -> Result<Self, InviteError> {
        let mut bundle: CommunityInvite = serde_json::from_str(json).map_err(|e| InviteError::Json(e.to_string()))?;
        if bundle.channels.len() > MAX_BUNDLE_CHANNELS {
            return Err(InviteError::TooManyChannels(bundle.channels.len()));
        }
        bundle.relays = cap_relays(std::mem::take(&mut bundle.relays));
        bundle.validate()?;
        Ok(bundle)
    }

    /// The self-certifying owner check (CORD-02 A.4) plus the Channel bound — a
    /// mismatching bundle is refused, so even a compromised creator can't smuggle
    /// a false owner or a fake key for a real Community.
    pub fn validate(&self) -> Result<(), InviteError> {
        if self.channels.len() > MAX_BUNDLE_CHANNELS {
            return Err(InviteError::TooManyChannels(self.channels.len()));
        }
        // Epochs aren't covered by the community_id commitment, so an attacker can
        // set them freely. Bound them well below `u64::MAX` (no real community
        // rotates a trillion times) so a downstream `epoch + 1` can't be pushed near
        // overflow and a crafted bundle can't derive nonsense addresses.
        if self.root_epoch > MAX_BUNDLE_EPOCH || self.channels.iter().any(|c| c.epoch > MAX_BUNDLE_EPOCH) {
            return Err(InviteError::BadFragment("epoch out of range"));
        }
        let owner = hex32(&self.owner, "owner")?;
        let salt = hex32(&self.owner_salt, "owner_salt")?;
        let cid = hex32(&self.community_id, "community_id")?;
        if !verify_community_id(&CommunityId(cid), &owner, &salt) {
            return Err(InviteError::OwnerMismatch);
        }
        Ok(())
    }

    /// Whether the invite's shelf life has run out (`expires_at` is unix ms).
    /// Deliberately NOT checked at parse: a parked invite still renders past
    /// expiry, only joining refuses (CORD-05 §1).
    pub fn expired(&self, now_ms: u64) -> bool {
        self.expires_at.is_some_and(|e| now_ms > e)
    }
}

// ── 2. The bundle event (kind 33301) ──────────────────────────────────────────

/// A fetched bundle coordinate resolves to one of these (CORD-05 §2).
#[derive(Debug)]
pub enum BundleState {
    /// Boxed: the bundle dwarfs the empty `Revoked` variant.
    Live(Box<CommunityInvite>),
    /// The link was retired: a fetcher finds the grave instead of keys.
    Revoked,
}

/// Build the addressable bundle event `(33301, link_signer, d="")`, marked live.
/// The content is the §1 bundle NIP-44-encrypted under `bundle_key` (derived off
/// the link's token — [`super::derive::invite_bundle_key`]), so relays store it
/// but can never open it.
pub fn build_bundle_event(
    link_signer: &Keys,
    bundle: &CommunityInvite,
    bundle_key: &[u8; 32],
) -> Result<Event, InviteError> {
    let json = serde_json::to_string(bundle).map_err(|e| InviteError::Json(e.to_string()))?;
    let content = seal_bundle(bundle_key, &json)?;
    EventBuilder::new(Kind::Custom(kind::INVITE_BUNDLE), content)
        .tags([d_empty(), vsk_tag(vsk::INVITE_LIVE)])
        .sign_with_keys(link_signer)
        .map_err(|e| InviteError::Crypto(e.to_string()))
}

/// Re-post the coordinate as a revocation tombstone (CORD-05 §2) — signer-signed,
/// so only the creator, and exactly as durable as the bundle it replaces (unlike
/// a best-effort relay deletion).
pub fn build_revocation(link_signer: &Keys) -> Result<Event, InviteError> {
    EventBuilder::new(Kind::Custom(kind::INVITE_BUNDLE), "")
        .tags([d_empty(), vsk_tag(vsk::INVITE_REVOKED)])
        .sign_with_keys(link_signer)
        .map_err(|e| InviteError::Crypto(e.to_string()))
}

/// Verify + open a fetched bundle event. The primary anti-squat guard is the
/// FETCH itself — the coordinate `(33301, link_signer, "")` means a different
/// author is a different coordinate, so a squatter's spam never matches the
/// filter — and this re-checks `event.pubkey == expected_signer` as a
/// belt-and-suspenders against a relay handing back a foreign event. Gates on
/// the signature, the `vsk` marker, decrypt, and [`CommunityInvite::validate`].
pub fn parse_bundle_event(
    event: &Event,
    expected_signer: &PublicKey,
    bundle_key: &[u8; 32],
) -> Result<BundleState, InviteError> {
    if event.kind.as_u16() != kind::INVITE_BUNDLE {
        return Err(InviteError::BadEvent("wrong kind"));
    }
    if event.pubkey != *expected_signer {
        return Err(InviteError::BadEvent("author is not the link signer"));
    }
    // The coordinate is `(33301, link_signer, "")`. The fetch filters on the author alone (relays
    // handle an empty `#d` filter inconsistently), so pin the empty `d` here instead.
    if !first_tag(event, "d").unwrap_or_default().is_empty() {
        return Err(InviteError::BadEvent("bundle is not at the link's coordinate"));
    }
    event.verify().map_err(|_| InviteError::BadEvent("signature invalid"))?;

    match first_tag(event, "vsk").as_deref() {
        Some(v) if v == vsk::INVITE_REVOKED => return Ok(BundleState::Revoked),
        Some(v) if v == vsk::INVITE_LIVE => {}
        _ => return Err(InviteError::BadEvent("unknown or missing bundle marker")),
    }

    let json = open_bundle(bundle_key, &event.content)?;
    Ok(BundleState::Live(Box::new(CommunityInvite::from_bundle_json(&json)?)))
}

// ── 3. The link (naddr + fragment codec) ──────────────────────────────────────

const INVITE_PATH: &str = "/invite/";

/// A parsed invite link: the bundle coordinate's author plus the fragment secrets.
#[derive(Debug, Clone)]
pub struct ParsedInviteLink {
    /// The link signer's pubkey — the bundle coordinate's author.
    pub link_signer: PublicKey,
    pub token: [u8; TOKEN_LEN],
    pub bootstrap_relays: Vec<String>,
    /// The bare naddr as it appeared in the link.
    pub naddr: String,
}

/// The stock relay set (dictionary ids 1..=4, in order) — selected by one flag
/// so the common invite carries zero relay bytes.
pub fn stock_relays() -> Vec<String> {
    RELAY_DICT.iter().map(|s| s.to_string()).collect()
}

/// Encode the fragment `[version=4][flags][relays?][token:16]` as base64url with
/// no padding. The stock set costs zero relay bytes (and is exempt from the
/// 3-relay cap, which applies to explicit entries only); otherwise each relay is
/// a dictionary-id byte, a `wss://`-implied literal (`0,len,host`), or a verbatim
/// literal (`255,len,url`) for `ws://` and exotic schemes.
pub fn encode_fragment(token: &[u8; TOKEN_LEN], relays: &[String]) -> Result<String, InviteError> {
    let is_stock = relays.len() == RELAY_DICT.len() && relays.iter().zip(RELAY_DICT.iter()).all(|(r, d)| r == d);

    let mut bytes = Vec::with_capacity(2 + TOKEN_LEN + relays.len() * 8);
    bytes.push(FRAGMENT_VERSION);
    if is_stock {
        bytes.push(FLAG_STOCK_SET);
    } else {
        bytes.push(0x00);
        let bounded = &relays[..relays.len().min(MAX_BOOTSTRAP_RELAYS)];
        bytes.push(bounded.len() as u8);
        for relay in bounded {
            if let Some(id) = dict_id(relay) {
                bytes.push(id);
            } else if let Some(host) = relay.strip_prefix("wss://") {
                if host.len() > u8::MAX as usize {
                    return Err(InviteError::BadFragment("relay host too long"));
                }
                bytes.extend_from_slice(&[0x00, host.len() as u8]);
                bytes.extend_from_slice(host.as_bytes());
            } else {
                if relay.len() > u8::MAX as usize {
                    return Err(InviteError::BadFragment("relay url too long"));
                }
                bytes.extend_from_slice(&[0xff, relay.len() as u8]);
                bytes.extend_from_slice(relay.as_bytes());
            }
        }
    }
    bytes.extend_from_slice(token);
    Ok(base64_simd::URL_SAFE_NO_PAD.encode_to_string(&bytes))
}

/// Decode a fragment into its token + bootstrap relays. Strict on framing: a
/// wrong version (legacy OR future), a bad count, or any trailing byte after the
/// token is fatal; an unknown dictionary id is skipped, not fatal, so the
/// dictionary can grow without breaking older readers.
pub fn decode_fragment(fragment: &str) -> Result<([u8; TOKEN_LEN], Vec<String>), InviteError> {
    let bytes = base64_simd::URL_SAFE_NO_PAD
        .decode_to_vec(fragment.trim().as_bytes())
        .map_err(|_| InviteError::BadFragment("not base64url"))?;

    if bytes.len() < 2 {
        return Err(InviteError::BadFragment("truncated"));
    }
    let version = bytes[0];
    // Reject BOTH lower (legacy, wrong dictionary) and higher (unknown format)
    // versions rather than decode against a dictionary we can't trust.
    if version != FRAGMENT_VERSION {
        return Err(InviteError::BadVersion(version));
    }
    let flags = bytes[1];
    let mut o = 2usize;

    let mut relays = Vec::new();
    if flags & FLAG_STOCK_SET != 0 {
        relays = stock_relays();
    } else {
        let count = *bytes.get(o).ok_or(InviteError::BadFragment("truncated"))? as usize;
        o += 1;
        if count > MAX_BOOTSTRAP_RELAYS {
            return Err(InviteError::BadFragment("too many bootstrap relays"));
        }
        for _ in 0..count {
            let lead = *bytes.get(o).ok_or(InviteError::BadFragment("truncated"))?;
            o += 1;
            if (1..=254).contains(&lead) {
                if let Some(url) = dict_url(lead) {
                    relays.push(url.to_string());
                }
                // Unknown dictionary id: skip, non-fatal (the dictionary grows).
            } else {
                let len = *bytes.get(o).ok_or(InviteError::BadFragment("truncated"))? as usize;
                o += 1;
                let end = o.checked_add(len).ok_or(InviteError::BadFragment("truncated"))?;
                let raw = bytes.get(o..end).ok_or(InviteError::BadFragment("truncated"))?;
                let text = std::str::from_utf8(raw).map_err(|_| InviteError::BadFragment("relay not utf8"))?;
                relays.push(if lead == 255 { text.to_string() } else { format!("wss://{text}") });
                o = end;
            }
        }
    }

    let end = o.checked_add(TOKEN_LEN).ok_or(InviteError::BadFragment("truncated"))?;
    let raw = bytes.get(o..end).ok_or(InviteError::BadFragment("truncated"))?;
    let mut token = [0u8; TOKEN_LEN];
    token.copy_from_slice(raw);
    if end != bytes.len() {
        return Err(InviteError::BadFragment("trailing bytes"));
    }
    Ok((token, relays))
}

/// Build the bare naddr for a link signer's bundle coordinate `(33301, pk, "")` —
/// no identifier bytes, no relay entries (relays travel in the fragment), keeping
/// it as short as an naddr gets.
pub fn bundle_naddr(link_signer: &PublicKey) -> Result<String, InviteError> {
    let coord = nostr_sdk::nips::nip01::Coordinate {
        kind: Kind::Custom(kind::INVITE_BUNDLE),
        public_key: *link_signer,
        identifier: String::new(),
    };
    let n19 = nostr_sdk::nips::nip19::Nip19Coordinate { coordinate: coord, relays: Vec::new() };
    nostr_sdk::nips::nip19::Nip19::Coordinate(n19)
        .to_bech32()
        .map_err(|e| InviteError::Crypto(e.to_string()))
}

/// Build a shareable invite URL on `base` — the base is interchangeable (any
/// deeplink domain works), only the naddr and fragment are protocol (CORD-05 §2).
pub fn build_invite_url(
    base: &str,
    link_signer: &PublicKey,
    token: &[u8; TOKEN_LEN],
    relays: &[String],
) -> Result<String, InviteError> {
    let naddr = bundle_naddr(link_signer)?;
    let fragment = encode_fragment(token, relays)?;
    Ok(format!("{}{INVITE_PATH}{naddr}#{fragment}", base.trim_end_matches('/')))
}

/// Parse a full URL (`…/invite/<naddr>#<fragment>`) or the domain-agnostic bare
/// form (`<naddr>#<fragment>`) into its coordinate author + fragment secrets.
pub fn parse_invite_link(input: &str) -> Result<ParsedInviteLink, InviteError> {
    let (locator, fragment) = input.trim().split_once('#').ok_or(InviteError::BadLink("no fragment"))?;
    if fragment.is_empty() {
        return Err(InviteError::BadLink("empty fragment"));
    }
    // A full URL carries the naddr after `/invite/`; the bare form IS the naddr.
    let naddr = match locator.find(INVITE_PATH) {
        Some(i) => locator[i + INVITE_PATH.len()..].trim_end_matches('/'),
        None => locator.trim_start_matches("nostr:"),
    };
    let link_signer = signer_from_naddr(naddr)?;
    let (token, bootstrap_relays) = decode_fragment(fragment)?;
    Ok(ParsedInviteLink { link_signer, token, bootstrap_relays, naddr: naddr.to_string() })
}

// ── 4. The Invite List (kind 13303) ───────────────────────────────────────────

/// One minted link in a creator's private [`InviteList`] (CORD-05 §4). The
/// `token` is the unlock secret AND the merge key; `signer_sk` is what refreshing
/// or retiring the bundle needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InviteEntry {
    pub token: String,
    pub signer_sk: String,
    pub community_id: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// A retired link: a tombstone always beats an entry, terminally (CORD-05 §4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InviteTombstone {
    pub token: String,
    pub community_id: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// A creator's Invite List — the kind-13303 replaceable, NIP-44-encrypted to
/// self. Two clients can serve one npub, so the round-trip discipline applies.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct InviteList {
    #[serde(default)]
    pub entries: Vec<InviteEntry>,
    #[serde(default)]
    pub tombstones: Vec<InviteTombstone>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Merge two Invite Lists without coordination (CORD-05 §4): the token is the
/// merge key, an entry is immutable once minted (first-seen wins), tombstones
/// union, and a tombstone always beats an entry — terminally, so a stale device
/// can never resurrect a revoked link.
pub fn merge_invite_lists(a: InviteList, b: InviteList) -> InviteList {
    use std::collections::BTreeMap;

    let mut entries: BTreeMap<String, InviteEntry> = BTreeMap::new();
    for e in a.entries.into_iter().chain(b.entries) {
        entries.entry(e.token.clone()).or_insert(e);
    }
    let mut tombstones: BTreeMap<String, InviteTombstone> = BTreeMap::new();
    for t in a.tombstones.into_iter().chain(b.tombstones) {
        tombstones.entry(t.token.clone()).or_insert(t);
    }
    for token in tombstones.keys() {
        entries.remove(token);
    }
    let mut extra = a.extra;
    extra.extend(b.extra);
    InviteList {
        entries: entries.into_values().collect(),
        tombstones: tombstones.into_values().collect(),
        extra,
    }
}

/// Build the creator's kind-13303 Invite List event (CORD-05 §4): the document
/// NIP-44-encrypted to SELF and signed by the creator's real key. Replaceable, one
/// per creator. On READ the caller MERGES into the local mirror, never replaces.
pub fn build_invite_list_event(my_keys: &Keys, list: &InviteList) -> Result<Event, InviteError> {
    use nostr_sdk::nips::nip44::{encrypt, Version};
    let json = serde_json::to_string(list).map_err(|e| InviteError::Json(e.to_string()))?;
    let content = encrypt(my_keys.secret_key(), &my_keys.public_key(), json.as_bytes(), Version::V2).map_err(|e| InviteError::Crypto(e.to_string()))?;
    EventBuilder::new(Kind::Custom(kind::INVITE_LIST), content)
        .sign_with_keys(my_keys)
        .map_err(|e| InviteError::Crypto(e.to_string()))
}

/// Decrypt + parse a kind-13303 event with the creator's own keys. A decrypt/parse
/// failure MUST be treated as "no news" by the caller — never a clobber of a
/// populated local list.
pub fn parse_invite_list_event(event: &Event, my_keys: &Keys) -> Result<InviteList, InviteError> {
    use nostr_sdk::nips::nip44::decrypt;
    if event.kind.as_u16() != kind::INVITE_LIST {
        return Err(InviteError::BadEvent("not a kind-13303 invite list"));
    }
    let json = decrypt(my_keys.secret_key(), &my_keys.public_key(), &event.content).map_err(|e| InviteError::Crypto(e.to_string()))?;
    serde_json::from_str(&json).map_err(|e| InviteError::Json(e.to_string()))
}

// ── 5. The Registry (vsk 8) ───────────────────────────────────────────────────

/// Build the vsk-8 Registry entity content (CORD-05 §5): a JSON array of the
/// creator's live link COORDINATES (link_signer pubkey hex) — never tokens,
/// URLs, or signing secrets, so members see that links exist without gaining the
/// ability to use one. The edition wrapping reuses the Control plane
/// ([`super::control`]); this is only the content shape.
pub fn build_registry_content(signers: &[PublicKey]) -> String {
    let hexes: Vec<String> = signers.iter().map(|p| p.to_hex()).collect();
    serde_json::to_string(&hexes).expect("Vec<String> always serializes")
}

/// Parse a Registry entity's content back into the link-signer coordinates.
pub fn parse_registry_content(content: &str) -> Result<Vec<PublicKey>, InviteError> {
    let hexes: Vec<String> = serde_json::from_str(content).map_err(|e| InviteError::Json(e.to_string()))?;
    hexes
        .iter()
        .map(|h| PublicKey::from_hex(h).map_err(|_| InviteError::BadHex("registry signer")))
        .collect()
}

// ── 6. Direct Invites (kind 3313) ─────────────────────────────────────────────

/// Build a Direct Invite (CORD-05 §6): the §1 bundle handed to a known npub as a
/// STANDARD NIP-59 giftwrap — ephemeral wrap author, the recipient in the `p`
/// tag, a kind-13 seal signed by the inviter's REAL key (whose verified npub is
/// what proves who invited), NOT the reversed stream wrap of CORD-01. The wrap
/// carries the outer `["k","3313"]` index hint (the deliberate exception to the
/// no-outer-tags rule) plus an optional NIP-40 `expiration` matching the bundle's
/// `expires_at` (ms → seconds). NIP-59 tweaks the wrap and seal timestamps into
/// the past.
pub fn build_direct_invite(
    inviter_keys: &Keys,
    recipient: &PublicKey,
    bundle: &CommunityInvite,
) -> Result<Event, InviteError> {
    let json = serde_json::to_string(bundle).map_err(|e| InviteError::Json(e.to_string()))?;

    // The unsigned kind-3313 rumor, authored (claimed) by the inviter.
    let mut rumor = EventBuilder::new(Kind::Custom(kind::DIRECT_INVITE), json)
        .custom_created_at(Timestamp::now())
        .build(inviter_keys.public_key());
    rumor.ensure_id();

    let seal_content = nip44::encrypt(inviter_keys.secret_key(), recipient, rumor.as_json(), nip44::Version::default())
        .map_err(|e| InviteError::Crypto(e.to_string()))?;
    let seal = EventBuilder::new(Kind::Seal, seal_content)
        .custom_created_at(Timestamp::tweaked(RANGE_RANDOM_TIMESTAMP_TWEAK))
        .sign_with_keys(inviter_keys)
        .map_err(|e| InviteError::Crypto(e.to_string()))?;

    let ephemeral = Keys::generate();
    let wrap_content = nip44::encrypt(ephemeral.secret_key(), recipient, seal.as_json(), nip44::Version::default())
        .map_err(|e| InviteError::Crypto(e.to_string()))?;
    let mut tags = vec![
        Tag::public_key(*recipient),
        Tag::custom(TagKind::Custom("k".into()), [kind::DIRECT_INVITE.to_string()]),
    ];
    if let Some(ms) = bundle.expires_at {
        tags.push(Tag::custom(TagKind::Custom("expiration".into()), [(ms / 1000).to_string()]));
    }
    EventBuilder::new(Kind::GiftWrap, wrap_content)
        .tags(tags)
        .custom_created_at(Timestamp::tweaked(RANGE_RANDOM_TIMESTAMP_TWEAK))
        .sign_with_keys(&ephemeral)
        .map_err(|e| InviteError::Crypto(e.to_string()))
}

/// Unwrap a Direct Invite giftwrap addressed to the recipient, returning the
/// verified inviter + the validated bundle. Peels wrap → seal → rumor and MUST
/// Schnorr-verify the kind-13 seal (its npub is the only proof of who invited),
/// then binds `rumor.pubkey == seal.pubkey` (anti-spoof) and gates on the rumor
/// kind (the outer `k` tag was only ever a hint). Nothing joins on unwrap —
/// consent is the caller's concern (CORD-05 §6).
pub fn unwrap_direct_invite(wrap: &Event, recipient_keys: &Keys) -> Result<(PublicKey, CommunityInvite), InviteError> {
    if wrap.kind != Kind::GiftWrap {
        return Err(InviteError::BadEvent("not a gift wrap"));
    }
    let seal_json = nip44::decrypt(recipient_keys.secret_key(), &wrap.pubkey, &wrap.content)
        .map_err(|e| InviteError::Crypto(e.to_string()))?;
    let seal = Event::from_json(&seal_json).map_err(|e| InviteError::Json(e.to_string()))?;
    if seal.kind != Kind::Seal {
        return Err(InviteError::BadEvent("inner is not a seal"));
    }
    seal.verify().map_err(|_| InviteError::BadEvent("seal signature invalid"))?;

    let rumor_json = nip44::decrypt(recipient_keys.secret_key(), &seal.pubkey, &seal.content)
        .map_err(|e| InviteError::Crypto(e.to_string()))?;
    let rumor = UnsignedEvent::from_json(rumor_json.as_bytes()).map_err(|e| InviteError::Json(e.to_string()))?;
    if rumor.kind.as_u16() != kind::DIRECT_INVITE {
        return Err(InviteError::BadEvent("rumor is not a direct invite"));
    }
    if rumor.pubkey != seal.pubkey {
        return Err(InviteError::BadEvent("rumor author does not match the seal signer"));
    }
    let bundle = CommunityInvite::from_bundle_json(&rumor.content)?;
    Ok((seal.pubkey, bundle))
}

// ── Signer-driven twins (bunker / NIP-55): identical wire, identity ops via NostrSigner ──

/// [`build_invite_list_event`] via a [`NostrSigner`]. Self-encrypts to `my_pk` and
/// signs the 13303 through the signer. `my_pk` must equal `my_public_key()`.
pub async fn build_invite_list_event_signed<S: nostr_sdk::prelude::NostrSigner + ?Sized>(
    signer: &S,
    my_pk: PublicKey,
    list: &InviteList,
) -> Result<Event, InviteError> {
    let json = serde_json::to_string(list).map_err(|e| InviteError::Json(e.to_string()))?;
    let content = signer.nip44_encrypt(&my_pk, &json).await.map_err(|e| InviteError::Crypto(e.to_string()))?;
    let unsigned = EventBuilder::new(Kind::Custom(kind::INVITE_LIST), content).build(my_pk);
    signer.sign_event(unsigned).await.map_err(|e| InviteError::Crypto(e.to_string()))
}

/// [`parse_invite_list_event`] via a [`NostrSigner`] (self-decrypt to `my_pk`).
pub async fn parse_invite_list_event_signed<S: nostr_sdk::prelude::NostrSigner + ?Sized>(
    signer: &S,
    my_pk: PublicKey,
    event: &Event,
) -> Result<InviteList, InviteError> {
    if event.kind.as_u16() != kind::INVITE_LIST {
        return Err(InviteError::BadEvent("not a kind-13303 invite list"));
    }
    let json = signer.nip44_decrypt(&my_pk, &event.content).await.map_err(|e| InviteError::Crypto(e.to_string()))?;
    serde_json::from_str(&json).map_err(|e| InviteError::Json(e.to_string()))
}

/// [`build_direct_invite`] via a [`NostrSigner`]: the kind-13 seal's NIP-44 (inviter
/// → recipient) and signature go through the signer; the ephemeral wrap stays local.
pub async fn build_direct_invite_signed<S: nostr_sdk::prelude::NostrSigner + ?Sized>(
    signer: &S,
    inviter_pk: PublicKey,
    recipient: &PublicKey,
    bundle: &CommunityInvite,
) -> Result<Event, InviteError> {
    let json = serde_json::to_string(bundle).map_err(|e| InviteError::Json(e.to_string()))?;
    let mut rumor = EventBuilder::new(Kind::Custom(kind::DIRECT_INVITE), json)
        .custom_created_at(Timestamp::now())
        .build(inviter_pk);
    rumor.ensure_id();
    let rumor_json = rumor.as_json();
    let seal_content = signer.nip44_encrypt(recipient, &rumor_json).await.map_err(|e| InviteError::Crypto(e.to_string()))?;
    let seal_unsigned = EventBuilder::new(Kind::Seal, seal_content)
        .custom_created_at(Timestamp::tweaked(RANGE_RANDOM_TIMESTAMP_TWEAK))
        .build(inviter_pk);
    let seal = signer.sign_event(seal_unsigned).await.map_err(|e| InviteError::Crypto(e.to_string()))?;
    let ephemeral = Keys::generate();
    let wrap_content = nip44::encrypt(ephemeral.secret_key(), recipient, seal.as_json(), nip44::Version::default())
        .map_err(|e| InviteError::Crypto(e.to_string()))?;
    let mut tags = vec![
        Tag::public_key(*recipient),
        Tag::custom(TagKind::Custom("k".into()), [kind::DIRECT_INVITE.to_string()]),
    ];
    if let Some(ms) = bundle.expires_at {
        tags.push(Tag::custom(TagKind::Custom("expiration".into()), [(ms / 1000).to_string()]));
    }
    EventBuilder::new(Kind::GiftWrap, wrap_content)
        .tags(tags)
        .custom_created_at(Timestamp::tweaked(RANGE_RANDOM_TIMESTAMP_TWEAK))
        .sign_with_keys(&ephemeral)
        .map_err(|e| InviteError::Crypto(e.to_string()))
}

/// [`unwrap_direct_invite`] via a [`NostrSigner`]: both giftwrap peels decrypt
/// through the signer. Same seal-verify + author-bind gates as the local path.
pub async fn unwrap_direct_invite_signed<S: nostr_sdk::prelude::NostrSigner + ?Sized>(
    signer: &S,
    wrap: &Event,
) -> Result<(PublicKey, CommunityInvite), InviteError> {
    if wrap.kind != Kind::GiftWrap {
        return Err(InviteError::BadEvent("not a gift wrap"));
    }
    let seal_json = signer.nip44_decrypt(&wrap.pubkey, &wrap.content).await.map_err(|e| InviteError::Crypto(e.to_string()))?;
    let seal = Event::from_json(&seal_json).map_err(|e| InviteError::Json(e.to_string()))?;
    if seal.kind != Kind::Seal {
        return Err(InviteError::BadEvent("inner is not a seal"));
    }
    seal.verify().map_err(|_| InviteError::BadEvent("seal signature invalid"))?;
    let rumor_json = signer.nip44_decrypt(&seal.pubkey, &seal.content).await.map_err(|e| InviteError::Crypto(e.to_string()))?;
    let rumor = UnsignedEvent::from_json(rumor_json.as_bytes()).map_err(|e| InviteError::Json(e.to_string()))?;
    if rumor.kind.as_u16() != kind::DIRECT_INVITE {
        return Err(InviteError::BadEvent("rumor is not a direct invite"));
    }
    if rumor.pubkey != seal.pubkey {
        return Err(InviteError::BadEvent("rumor author does not match the seal signer"));
    }
    let bundle = CommunityInvite::from_bundle_json(&rumor.content)?;
    Ok((seal.pubkey, bundle))
}

// ── helpers ────────────────────────────────────────────────────────────────────

fn hex32(s: &str, field: &'static str) -> Result<[u8; 32], InviteError> {
    crate::simd::hex::hex_to_bytes_32_checked(s).ok_or(InviteError::BadHex(field))
}

fn dict_id(url: &str) -> Option<u8> {
    RELAY_DICT.iter().position(|u| *u == url).map(|i| (i + 1) as u8)
}

fn dict_url(id: u8) -> Option<&'static str> {
    RELAY_DICT.get((id as usize).checked_sub(1)?).copied()
}

fn d_empty() -> Tag {
    Tag::custom(TagKind::Custom("d".into()), [""])
}

fn vsk_tag(value: &str) -> Tag {
    Tag::custom(TagKind::Custom("vsk".into()), [value])
}

fn first_tag(event: &Event, name: &str) -> Option<String> {
    event.tags.iter().find_map(|t| {
        let s = t.as_slice();
        (s.len() >= 2 && s[0] == name).then(|| s[1].clone())
    })
}

/// The NIP-44 bundle ciphertext under `bundle_key` used directly as the
/// conversation key (CORD-05 §2 — not an ECDH pair). Standard-base64 payload so
/// it interoperates with nostr-tools' `nip44.encrypt(json, bundle_key)`.
pub(crate) fn seal_bundle(bundle_key: &[u8; 32], json: &str) -> Result<String, InviteError> {
    let ck = ConversationKey::new(*bundle_key);
    let ct = encrypt_to_bytes(&ck, json.as_bytes()).map_err(|e| InviteError::Crypto(e.to_string()))?;
    Ok(base64_simd::STANDARD.encode_to_string(&ct))
}

fn open_bundle(bundle_key: &[u8; 32], content: &str) -> Result<String, InviteError> {
    let ck = ConversationKey::new(*bundle_key);
    let ct = base64_simd::STANDARD
        .decode_to_vec(content.as_bytes())
        .map_err(|e| InviteError::Crypto(e.to_string()))?;
    let pt = decrypt_to_bytes(&ck, &ct).map_err(|e| InviteError::Crypto(e.to_string()))?;
    String::from_utf8(pt).map_err(|e| InviteError::Crypto(e.to_string()))
}

fn signer_from_naddr(naddr: &str) -> Result<PublicKey, InviteError> {
    let trimmed = naddr.trim().trim_start_matches("nostr:");
    let n19 = nostr_sdk::nips::nip19::Nip19::from_bech32(trimmed).map_err(|_| InviteError::BadLink("invalid naddr"))?;
    match n19 {
        nostr_sdk::nips::nip19::Nip19::Coordinate(c)
            if c.coordinate.kind.as_u16() == kind::INVITE_BUNDLE && c.coordinate.identifier.is_empty() =>
        {
            Ok(c.coordinate.public_key)
        }
        _ => Err(InviteError::BadLink("naddr is not an invite-bundle coordinate")),
    }
}

#[cfg(test)]
mod tests {
    use super::super::control::CommunityIdentity;
    use super::super::derive::invite_bundle_key;
    use super::*;
    use crate::community::{random_32, MAX_COMMUNITY_RELAYS};

    fn hex(bytes: &[u8; 32]) -> String {
        crate::simd::hex::bytes_to_hex_32(bytes)
    }

    /// A valid owner + bundle whose (owner, salt) reproduce its community_id.
    fn valid_bundle() -> (Keys, CommunityInvite) {
        let owner = Keys::generate();
        let id = CommunityIdentity::mint(&owner.public_key());
        let bundle = CommunityInvite {
            community_id: hex(&id.community_id.0),
            owner: hex(&id.owner_xonly),
            owner_salt: hex(&id.owner_salt),
            community_root: hex(&random_32()),
            root_epoch: 0,
            channels: vec![],
            relays: vec!["wss://a.example".into(), "wss://b.example".into()],
            name: "Test community".into(),
            icon: None,
            expires_at: None,
            creator_npub: None,
            label: None,
            extra: Default::default(),
        };
        (owner, bundle)
    }

    fn token16() -> [u8; TOKEN_LEN] {
        std::array::from_fn(|i| i as u8) // 00,01,..,0f
    }

    // ── Bundle validate() ──────────────────────────────────────────────────────

    #[test]
    fn validate_accepts_a_correct_owner_salt_id_triple() {
        let (_owner, bundle) = valid_bundle();
        assert!(bundle.validate().is_ok());
    }

    #[test]
    fn validate_rejects_a_forged_owner() {
        let (_owner, mut bundle) = valid_bundle();
        // Attacker key over the REAL community id → second-preimage, must fail.
        let attacker = Keys::generate();
        bundle.owner = hex(&attacker.public_key().to_bytes());
        assert!(matches!(bundle.validate(), Err(InviteError::OwnerMismatch)));
    }

    #[test]
    fn from_json_rejects_over_the_channel_cap() {
        let (_owner, mut bundle) = valid_bundle();
        bundle.channels = (0..MAX_BUNDLE_CHANNELS + 1)
            .map(|_| ChannelGrant { id: hex(&random_32()), key: hex(&random_32()), epoch: 0, name: "x".into() })
            .collect();
        let json = serde_json::to_string(&bundle).unwrap();
        assert!(matches!(
            CommunityInvite::from_bundle_json(&json),
            Err(InviteError::TooManyChannels(n)) if n == MAX_BUNDLE_CHANNELS + 1
        ));
    }

    #[test]
    fn from_json_truncates_relays_to_the_community_cap() {
        let (_owner, mut bundle) = valid_bundle();
        bundle.relays = (0..MAX_COMMUNITY_RELAYS + 3).map(|i| format!("wss://r{i}.example")).collect();
        let json = serde_json::to_string(&bundle).unwrap();
        let parsed = CommunityInvite::from_bundle_json(&json).unwrap();
        assert_eq!(parsed.relays.len(), MAX_COMMUNITY_RELAYS);
    }

    #[test]
    fn unknown_bundle_fields_round_trip() {
        // Other clients' extensions (held_roots, refounder, future) survive a
        // parse → serialize cycle untouched (CORD-02 §6).
        let (_owner, bundle) = valid_bundle();
        let mut value = serde_json::to_value(&bundle).unwrap();
        let obj = value.as_object_mut().unwrap();
        obj.insert("held_roots".into(), serde_json::json!([{ "epoch": 2, "key": "ab" }]));
        obj.insert("refounder".into(), serde_json::json!("deadbeef"));
        obj.insert("future_field".into(), serde_json::json!({ "deep": [1, 2] }));
        let json = serde_json::to_string(&value).unwrap();

        let parsed = CommunityInvite::from_bundle_json(&json).unwrap();
        let out: serde_json::Value = serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(out["held_roots"][0]["epoch"], 2);
        assert_eq!(out["refounder"], "deadbeef");
        assert_eq!(out["future_field"]["deep"][1], 2);
    }

    #[test]
    fn expiry_is_a_join_gate_not_a_render_gate() {
        let (_owner, mut bundle) = valid_bundle();
        bundle.expires_at = Some(1_000_000);
        assert!(!bundle.expired(999_999));
        assert!(bundle.expired(1_000_001));
    }

    // ── Fragment codec GOLDEN VECTORS (hand-computed base64url) ──────────────────

    #[test]
    fn fragment_golden_stock_set() {
        // [0x04 version][0x01 stock flag][token 00..0f] = 18 bytes → base64url:
        //   04 01 00 | 01 02 03 | 04 05 06 | 07 08 09 | 0a 0b 0c | 0d 0e 0f
        //   B A E A    A Q I D    B A U G    B w g J    C g s M    D Q 4 P
        let token = token16();
        let frag = encode_fragment(&token, &stock_relays()).unwrap();
        assert_eq!(frag, "BAEAAQIDBAUGBwgJCgsMDQ4P");
        let (t, relays) = decode_fragment(&frag).unwrap();
        assert_eq!(t, token);
        assert_eq!(relays, stock_relays());
    }

    #[test]
    fn fragment_golden_dictionary_id_mix() {
        // [04][00 flags][02 count][02 dict-id][04 dict-id][token] = 21 bytes:
        //   04 00 02 | 02 04 00 | 01 02 03 | 04 05 06 | 07 08 09 | 0a 0b 0c | 0d 0e 0f
        //   B A A C    A g Q A    A Q I D    B A U G    B w g J    C g s M    D Q 4 P
        let token = token16();
        let relays = vec![RELAY_DICT[1].to_string(), RELAY_DICT[3].to_string()];
        let frag = encode_fragment(&token, &relays).unwrap();
        assert_eq!(frag, "BAACAgQAAQIDBAUGBwgJCgsMDQ4P");
        let (t, out) = decode_fragment(&frag).unwrap();
        assert_eq!(t, token);
        assert_eq!(out, relays);
    }

    #[test]
    fn fragment_golden_wss_implied_literal() {
        // [04][00][01 count][00 lead=wss-implied][03 len]["x.y"=78 2e 79][token] = 24 bytes:
        //   04 00 01 | 00 03 78 | 2e 79 00 | 01 02 03 | 04 05 06 | 07 08 09 | 0a 0b 0c | 0d 0e 0f
        //   B A A B    A A N 4    L n k A    A Q I D    B A U G    B w g J    C g s M    D Q 4 P
        let token = token16();
        let relays = vec!["wss://x.y".to_string()];
        let frag = encode_fragment(&token, &relays).unwrap();
        assert_eq!(frag, "BAABAAN4LnkAAQIDBAUGBwgJCgsMDQ4P");
        let (t, out) = decode_fragment(&frag).unwrap();
        assert_eq!(t, token);
        assert_eq!(out, relays);
    }

    #[test]
    fn fragment_golden_verbatim_literal() {
        // [04][00][01][ff lead=verbatim][06 len]["ws://h"=77 73 3a 2f 2f 68][token] = 27 bytes:
        //   04 00 01 | ff 06 77 | 73 3a 2f | 2f 68 00 | 01 02 03 | 04 05 06 | 07 08 09 | 0a 0b 0c | 0d 0e 0f
        //   B A A B    _ w Z 3    c z o v    L 2 g A    A Q I D    B A U G    B w g J    C g s M    D Q 4 P
        let token = token16();
        let relays = vec!["ws://h".to_string()];
        let frag = encode_fragment(&token, &relays).unwrap();
        assert_eq!(frag, "BAAB_wZ3czovL2gAAQIDBAUGBwgJCgsMDQ4P");
        let (t, out) = decode_fragment(&frag).unwrap();
        assert_eq!(t, token);
        assert_eq!(out, relays);
    }

    #[test]
    fn fragment_rejects_wrong_version_both_directions() {
        let token = token16();
        for bad in [3u8, 5u8] {
            let mut bytes = vec![bad, FLAG_STOCK_SET];
            bytes.extend_from_slice(&token);
            let frag = base64_simd::URL_SAFE_NO_PAD.encode_to_string(&bytes);
            assert!(matches!(decode_fragment(&frag), Err(InviteError::BadVersion(v)) if v == bad));
        }
    }

    #[test]
    fn fragment_rejects_trailing_bytes() {
        let token = token16();
        let mut bytes = vec![FRAGMENT_VERSION, FLAG_STOCK_SET];
        bytes.extend_from_slice(&token);
        bytes.push(0xff); // one byte past the token
        let frag = base64_simd::URL_SAFE_NO_PAD.encode_to_string(&bytes);
        assert!(matches!(decode_fragment(&frag), Err(InviteError::BadFragment("trailing bytes"))));
    }

    #[test]
    fn fragment_rejects_count_over_three() {
        let token = token16();
        let mut bytes = vec![FRAGMENT_VERSION, 0x00, 0x04]; // count 4 > cap
        bytes.extend_from_slice(&[1, 2, 3, 4]);
        bytes.extend_from_slice(&token);
        let frag = base64_simd::URL_SAFE_NO_PAD.encode_to_string(&bytes);
        assert!(matches!(decode_fragment(&frag), Err(InviteError::BadFragment("too many bootstrap relays"))));
    }

    #[test]
    fn fragment_skips_an_unknown_dictionary_id() {
        let token = token16();
        // count 1, dict id 200 (unknown gen-4 id) → skipped, not fatal.
        let mut bytes = vec![FRAGMENT_VERSION, 0x00, 0x01, 200];
        bytes.extend_from_slice(&token);
        let frag = base64_simd::URL_SAFE_NO_PAD.encode_to_string(&bytes);
        let (t, relays) = decode_fragment(&frag).unwrap();
        assert_eq!(t, token);
        assert!(relays.is_empty());
    }

    #[test]
    fn fragment_caps_bootstrap_relays_at_three() {
        let token = token16();
        let relays: Vec<String> = (0..4).map(|i| format!("wss://r{i}.example")).collect();
        let (_t, out) = decode_fragment(&encode_fragment(&token, &relays).unwrap()).unwrap();
        assert_eq!(out.len(), MAX_BOOTSTRAP_RELAYS);
    }

    // ── 33301 bundle event ──────────────────────────────────────────────────────

    #[test]
    fn bundle_event_round_trips_live() {
        let (_owner, bundle) = valid_bundle();
        let link = Keys::generate();
        let token = random_32();
        let key = invite_bundle_key(&token[..TOKEN_LEN].try_into().unwrap());
        let event = build_bundle_event(&link, &bundle, &key).unwrap();
        assert_eq!(event.pubkey, link.public_key());

        match parse_bundle_event(&event, &link.public_key(), &key).unwrap() {
            BundleState::Live(b) => {
                assert_eq!(b.community_id, bundle.community_id);
                assert_eq!(b.name, "Test community");
            }
            BundleState::Revoked => panic!("expected Live"),
        }
    }

    #[test]
    fn revocation_reads_as_revoked() {
        let link = Keys::generate();
        let tomb = build_revocation(&link).unwrap();
        let key = invite_bundle_key(&[0u8; TOKEN_LEN]);
        assert!(matches!(parse_bundle_event(&tomb, &link.public_key(), &key), Ok(BundleState::Revoked)));
    }

    #[test]
    fn bundle_event_wrong_key_fails() {
        let (_owner, bundle) = valid_bundle();
        let link = Keys::generate();
        let key = invite_bundle_key(&[7u8; TOKEN_LEN]);
        let event = build_bundle_event(&link, &bundle, &key).unwrap();
        let wrong = invite_bundle_key(&[8u8; TOKEN_LEN]);
        assert!(parse_bundle_event(&event, &link.public_key(), &wrong).is_err());
    }

    #[test]
    fn bundle_event_from_a_different_author_is_rejected() {
        // The coordinate is the anti-squat guard; the author re-check catches a
        // relay handing back a squatter's event at the same d.
        let (_owner, bundle) = valid_bundle();
        let real = Keys::generate();
        let squatter = Keys::generate();
        let key = invite_bundle_key(&[3u8; TOKEN_LEN]);
        let event = build_bundle_event(&squatter, &bundle, &key).unwrap();
        assert!(matches!(
            parse_bundle_event(&event, &real.public_key(), &key),
            Err(InviteError::BadEvent("author is not the link signer"))
        ));
    }

    /// The fetch filters on the author alone (an empty `#d` filter is answered inconsistently by
    /// relays), so the empty `d` MUST be pinned here — otherwise the same signer's event at any
    /// other `d` would be accepted as the bundle.
    #[test]
    fn bundle_event_at_a_non_empty_d_is_rejected() {
        let (_owner, bundle) = valid_bundle();
        let link = Keys::generate();
        let key = invite_bundle_key(&[9u8; TOKEN_LEN]);
        let json = serde_json::to_string(&bundle).unwrap();
        let content = seal_bundle(&key, &json).unwrap();
        let event = EventBuilder::new(Kind::Custom(kind::INVITE_BUNDLE), content)
            .tags([Tag::identifier("elsewhere"), vsk_tag(vsk::INVITE_LIVE)])
            .sign_with_keys(&link)
            .unwrap();
        assert!(matches!(
            parse_bundle_event(&event, &link.public_key(), &key),
            Err(InviteError::BadEvent("bundle is not at the link's coordinate"))
        ));
    }

    #[test]
    fn bundle_event_with_a_tampered_signature_is_rejected() {
        let (_owner, bundle) = valid_bundle();
        let link = Keys::generate();
        let other = Keys::generate();
        let key = invite_bundle_key(&[4u8; TOKEN_LEN]);
        let event = build_bundle_event(&link, &bundle, &key).unwrap();
        // Swap the author to `other` (its signature no longer matches the id).
        let mut json: serde_json::Value = serde_json::from_str(&event.as_json()).unwrap();
        json["pubkey"] = serde_json::Value::String(other.public_key().to_hex());
        let Ok(forged) = Event::from_json(json.to_string()) else { return };
        assert!(matches!(
            parse_bundle_event(&forged, &other.public_key(), &key),
            Err(InviteError::BadEvent("signature invalid"))
        ));
    }

    // ── Invite List merge ───────────────────────────────────────────────────────

    fn entry(token: &str, community: &str) -> InviteEntry {
        InviteEntry {
            token: token.into(),
            signer_sk: "bb".repeat(32),
            community_id: community.into(),
            url: "https://x/invite/naddr1xyz#frag".into(),
            label: None,
            created_at: 1000,
            expires_at: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn invite_list_merge_entries_immutable_tombstone_wins_terminally() {
        let e = entry(&"aa".repeat(16), &"cc".repeat(32));
        let a = merge_invite_lists(InviteList::default(), InviteList { entries: vec![e.clone()], ..Default::default() });
        assert_eq!(a.entries.len(), 1);

        // Re-merging a mutated entry under the same token can't change it.
        let mut mutated = e.clone();
        mutated.label = Some("changed".into());
        let still = merge_invite_lists(a.clone(), InviteList { entries: vec![mutated], ..Default::default() });
        assert_eq!(still.entries[0].label, None);

        // Tombstone beats the entry.
        let tomb = InviteTombstone { token: e.token.clone(), community_id: e.community_id.clone(), extra: Default::default() };
        let b = merge_invite_lists(a, InviteList { tombstones: vec![tomb], ..Default::default() });
        assert!(b.entries.is_empty());
        assert_eq!(b.tombstones.len(), 1);

        // A stale device re-merging the entry can't resurrect the revoked link.
        let c = merge_invite_lists(b, InviteList { entries: vec![e], ..Default::default() });
        assert!(c.entries.is_empty());
    }

    #[test]
    fn invite_list_tombstones_union() {
        let t1 = InviteTombstone { token: "11".repeat(16), community_id: "aa".repeat(32), extra: Default::default() };
        let t2 = InviteTombstone { token: "22".repeat(16), community_id: "bb".repeat(32), extra: Default::default() };
        let merged = merge_invite_lists(
            InviteList { tombstones: vec![t1], ..Default::default() },
            InviteList { tombstones: vec![t2], ..Default::default() },
        );
        assert_eq!(merged.tombstones.len(), 2);
    }

    #[test]
    fn invite_list_unknown_fields_round_trip() {
        let wire = r#"{"entries":[],"tombstones":[],"schema":"future","note":{"x":1}}"#;
        let list: InviteList = serde_json::from_str(wire).unwrap();
        let out: serde_json::Value = serde_json::from_str(&serde_json::to_string(&list).unwrap()).unwrap();
        assert_eq!(out["schema"], "future");
        assert_eq!(out["note"]["x"], 1);
    }

    // ── Direct invite ───────────────────────────────────────────────────────────

    #[test]
    fn direct_invite_round_trips_to_inviter_and_bundle() {
        let inviter = Keys::generate();
        let recipient = Keys::generate();
        let (_owner, bundle) = valid_bundle();

        let wrap = build_direct_invite(&inviter, &recipient.public_key(), &bundle).unwrap();
        assert_eq!(wrap.kind, Kind::GiftWrap);
        assert_ne!(wrap.pubkey, inviter.public_key(), "wrap author is ephemeral");
        assert!(wrap.tags.iter().any(|t| {
            let s = t.as_slice();
            s.len() >= 2 && s[0] == "k" && s[1] == kind::DIRECT_INVITE.to_string()
        }));

        let (sender, out) = unwrap_direct_invite(&wrap, &recipient).unwrap();
        assert_eq!(sender, inviter.public_key());
        assert_eq!(out.community_id, bundle.community_id);
        assert_eq!(out.name, "Test community");
    }

    #[test]
    fn direct_invite_rejects_a_seal_claiming_a_false_inviter() {
        // The verify Armada skips: a seal whose pubkey was swapped to Y but signed
        // by X has an invalid signature — the Schnorr check must catch it.
        let inviter = Keys::generate();
        let claimed = Keys::generate();
        let recipient = Keys::generate();
        let (_owner, bundle) = valid_bundle();

        // Build a normal invite, then rebuild the seal with a swapped pubkey.
        let wrap = build_direct_invite(&inviter, &recipient.public_key(), &bundle).unwrap();
        let seal_json = nip44::decrypt(recipient.secret_key(), &wrap.pubkey, &wrap.content).unwrap();
        let mut seal_val: serde_json::Value = serde_json::from_str(&seal_json).unwrap();
        seal_val["pubkey"] = serde_json::Value::String(claimed.public_key().to_hex());
        // Re-wrap the forged seal to the recipient under a fresh ephemeral key.
        let ephemeral = Keys::generate();
        let wrap_ct =
            nip44::encrypt(ephemeral.secret_key(), &recipient.public_key(), seal_val.to_string(), nip44::Version::default())
                .unwrap();
        let forged = EventBuilder::new(Kind::GiftWrap, wrap_ct)
            .tags([Tag::public_key(recipient.public_key())])
            .sign_with_keys(&ephemeral)
            .unwrap();
        assert!(unwrap_direct_invite(&forged, &recipient).is_err());
    }

    #[test]
    fn direct_invite_rejects_a_non_invite_rumor_kind() {
        let inviter = Keys::generate();
        let recipient = Keys::generate();
        let (_owner, bundle) = valid_bundle();
        let json = serde_json::to_string(&bundle).unwrap();

        // Hand-build the shape with a kind-9 rumor instead of 3313.
        let mut rumor = EventBuilder::new(Kind::Custom(9), json)
            .custom_created_at(Timestamp::now())
            .build(inviter.public_key());
        rumor.ensure_id();
        let seal_ct =
            nip44::encrypt(inviter.secret_key(), &recipient.public_key(), rumor.as_json(), nip44::Version::default()).unwrap();
        let seal = EventBuilder::new(Kind::Seal, seal_ct).sign_with_keys(&inviter).unwrap();
        let ephemeral = Keys::generate();
        let wrap_ct =
            nip44::encrypt(ephemeral.secret_key(), &recipient.public_key(), seal.as_json(), nip44::Version::default()).unwrap();
        let wrap = EventBuilder::new(Kind::GiftWrap, wrap_ct)
            .tags([Tag::public_key(recipient.public_key())])
            .sign_with_keys(&ephemeral)
            .unwrap();
        assert!(matches!(
            unwrap_direct_invite(&wrap, &recipient),
            Err(InviteError::BadEvent("rumor is not a direct invite"))
        ));
    }

    #[test]
    fn direct_invite_stamps_nip40_expiration_in_seconds() {
        let inviter = Keys::generate();
        let recipient = Keys::generate();
        let (_owner, mut bundle) = valid_bundle();
        let expires_ms = 1_735_689_600_000u64;
        bundle.expires_at = Some(expires_ms);

        let wrap = build_direct_invite(&inviter, &recipient.public_key(), &bundle).unwrap();
        let exp = wrap.tags.iter().find_map(|t| {
            let s = t.as_slice();
            (s.len() >= 2 && s[0] == "expiration").then(|| s[1].clone())
        });
        assert_eq!(exp, Some((expires_ms / 1000).to_string()));
    }

    #[test]
    fn direct_invite_rejects_a_forged_owner_bundle() {
        let inviter = Keys::generate();
        let recipient = Keys::generate();
        let (_owner, mut bundle) = valid_bundle();
        bundle.owner = hex(&Keys::generate().public_key().to_bytes()); // forged
        let wrap = build_direct_invite(&inviter, &recipient.public_key(), &bundle).unwrap();
        assert!(matches!(unwrap_direct_invite(&wrap, &recipient), Err(InviteError::OwnerMismatch)));
    }

    // ── Links ───────────────────────────────────────────────────────────────────

    #[test]
    fn invite_link_round_trips_full_url_and_bare_form() {
        let link = Keys::generate();
        let token = token16();
        let relays = vec!["wss://a.example".to_string()];
        let url = build_invite_url("https://vectorapp.io", &link.public_key(), &token, &relays).unwrap();
        assert!(url.contains("/invite/naddr1"));

        let parsed = parse_invite_link(&url).unwrap();
        assert_eq!(parsed.link_signer, link.public_key());
        assert_eq!(parsed.token, token);
        assert_eq!(parsed.bootstrap_relays, relays);

        let bare = format!("{}#{}", parsed.naddr, url.split('#').nth(1).unwrap());
        assert_eq!(parse_invite_link(&bare).unwrap().link_signer, link.public_key());
    }

    #[test]
    fn invite_link_rejects_non_invites() {
        assert!(parse_invite_link("hello world").is_err());
        assert!(parse_invite_link("wss://relay.example.com").is_err());
        assert!(parse_invite_link("https://x/invite/#frag").is_err());
    }

    #[test]
    fn v2_parser_rejects_a_v1_style_url_no_cross_protocol_confusion() {
        // Dual-stack dispatch (VectorCore::join_community) tries the v2 parser
        // first and falls through to v1 on failure, so the load-bearing invariant
        // is that v2 NEVER accepts a v1 link. A v1 URL is `…/invite#<base64url>`
        // (no naddr segment in the path), so the v2 parser fails at the naddr
        // step. If this ever passes, a v1 invite would be mis-routed to v2.
        let v1_url = "https://vectorapp.io/invite#AmR1bW15djFmcmFnbWVudA";
        assert!(parse_invite_link(v1_url).is_err(), "v2 must reject a v1-format invite URL");
        // A v1 URL with a trailing slash is likewise not a valid v2 naddr.
        assert!(parse_invite_link("https://vectorapp.io/invite/#AmR1bW15").is_err());
        // Sanity: a real v2 link still parses (bare naddr#fragment form).
        let token = [0x07u8; TOKEN_LEN];
        let signer = Keys::generate();
        let naddr = bundle_naddr(&signer.public_key()).unwrap();
        let frag = encode_fragment(&token, &[]).unwrap();
        assert!(parse_invite_link(&format!("{naddr}#{frag}")).is_ok());
    }

    // ── Registry ────────────────────────────────────────────────────────────────

    #[test]
    fn registry_content_round_trips_and_leaks_no_secrets() {
        let a = Keys::generate();
        let b = Keys::generate();
        let content = build_registry_content(&[a.public_key(), b.public_key()]);

        // Coordinates only: exactly the two pubkey hexes, nothing else.
        let arr: Vec<String> = serde_json::from_str(&content).unwrap();
        assert_eq!(arr, vec![a.public_key().to_hex(), b.public_key().to_hex()]);
        // No token / url / secret ever rides the member-facing Registry.
        assert!(!content.contains("invite"));
        assert!(!content.contains("token"));
        assert!(!content.contains("://"));

        let back = parse_registry_content(&content).unwrap();
        assert_eq!(back, vec![a.public_key(), b.public_key()]);
    }
}
