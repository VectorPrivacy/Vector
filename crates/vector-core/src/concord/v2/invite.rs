//! CORD-05: Invites — the bundle, the link, the Registry, Direct Invites.
//!
//! An invite delivers the keys that make a member: as a shareable URL whose
//! keys live in a token-encrypted bundle on relays (revocable before use), or
//! the same bundle giftwrapped straight to an npub as a Direct Invite (a key
//! handoff, unrevocable once landed, never flips the Community Public).

use nostr_sdk::nips::nip19::{FromBech32, Nip19Coordinate, ToBech32};
use nostr_sdk::nips::nip44::v2::{decrypt_to_bytes, encrypt_to_bytes, ConversationKey};
use nostr_sdk::nips::nip59::UnwrappedGift;
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use super::control::ImageRef;
use super::derive::{invite_bundle_key, verify_owner};
use super::{
    kind, vsk, ChannelId, ChannelKey, CommunityId, CommunityRoot, Epoch, OwnerSalt,
    FRAGMENT_MAX_RELAYS, INVITE_MAX_CHANNELS, NIP44_MAX_PLAINTEXT, RELAYS_RECOMMENDED_MAX,
};

// ============================================================================
// The bundle (§1)
// ============================================================================

/// One granted Channel inside a bundle: Public Channels carry no key (they
/// derive from the root); Private ones deliver theirs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelGrant {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub epoch: u64,
    pub name: String,
}

impl ChannelGrant {
    pub fn channel_id(&self) -> Option<ChannelId> {
        ChannelId::from_hex(&self.id)
    }

    pub fn channel_key(&self) -> Option<ChannelKey> {
        crate::simd::hex::hex_to_bytes_32_checked(self.key.as_deref()?).map(ChannelKey)
    }
}

/// The CommunityInvite bundle (CORD-05 §1). The inviter's identity is
/// irrelevant to trust: the `community_id` self-certifies the owner, so a
/// bundle can't smuggle a false owner or a fake key for a real Community.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityInvite {
    pub community_id: String,
    pub owner: String,
    pub owner_salt: String,
    pub community_root: String,
    pub root_epoch: u64,
    #[serde(default)]
    pub channels: Vec<ChannelGrant>,
    #[serde(default)]
    pub relays: Vec<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<ImageRef>,
    /// Unix ms; past it the preview still renders, joining refuses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_npub: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug)]
pub enum InviteError {
    /// `owner`/`owner_salt` fail to reproduce the `community_id`.
    OwnerMismatch,
    /// Bundle bounds exceeded (hostile-allocation guard).
    TooManyChannels(usize),
    Expired,
    Malformed(String),
    Crypto(String),
}

impl std::fmt::Display for InviteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InviteError::OwnerMismatch => write!(f, "bundle owner/salt do not reproduce the community_id"),
            InviteError::TooManyChannels(n) => write!(f, "bundle carries {n} channels (cap {INVITE_MAX_CHANNELS})"),
            InviteError::Expired => write!(f, "invite expired"),
            InviteError::Malformed(e) => write!(f, "malformed invite: {e}"),
            InviteError::Crypto(e) => write!(f, "invite crypto: {e}"),
        }
    }
}

impl std::error::Error for InviteError {}

impl CommunityInvite {
    /// Validate an attacker-crafted bundle *before allocating on it*: the
    /// self-certifying owner check, the channel-count bound, the relay trim.
    /// Expiry is separate (`is_expired`) — a parked invite still previews.
    pub fn validate(&mut self) -> Result<(), InviteError> {
        if self.channels.len() > INVITE_MAX_CHANNELS {
            return Err(InviteError::TooManyChannels(self.channels.len()));
        }
        let id = CommunityId::from_hex(&self.community_id)
            .ok_or_else(|| InviteError::Malformed("community_id not 32-byte hex".into()))?;
        let owner = crate::simd::hex::hex_to_bytes_32_checked(&self.owner)
            .ok_or_else(|| InviteError::Malformed("owner not 32-byte hex".into()))?;
        let salt = crate::simd::hex::hex_to_bytes_32_checked(&self.owner_salt)
            .map(OwnerSalt)
            .ok_or_else(|| InviteError::Malformed("owner_salt not 32-byte hex".into()))?;
        if !verify_owner(&id, &owner, &salt) {
            return Err(InviteError::OwnerMismatch);
        }
        self.relays.truncate(RELAYS_RECOMMENDED_MAX);
        Ok(())
    }

    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.expires_at.map(|e| now_ms > e).unwrap_or(false)
    }

    pub fn community_id_typed(&self) -> Option<CommunityId> {
        CommunityId::from_hex(&self.community_id)
    }

    pub fn community_root_typed(&self) -> Option<CommunityRoot> {
        crate::simd::hex::hex_to_bytes_32_checked(&self.community_root).map(CommunityRoot)
    }

    pub fn root_epoch_typed(&self) -> Epoch {
        Epoch(self.root_epoch)
    }
}

// ============================================================================
// The relay dictionary + fragment codec (§3)
// ============================================================================

/// The format/dictionary generation byte. A client MAY reject any lower value
/// as a legacy link rather than decode it against the wrong dictionary.
pub const FRAGMENT_VERSION: u8 = 4;

/// The stock set: four primaries selected by one flag, so the common invite
/// carries zero relay bytes.
pub const STOCK_RELAYS: [&str; 4] = [
    "wss://jskitty.com/nostr",
    "wss://asia.vectorapp.io/nostr",
    "wss://relay.ditto.pub",
    "wss://relay.dreamith.to",
];

const FLAG_STOCK_SET: u8 = 0x01;

/// Dictionary id → relay URL (generation 4).
pub fn dictionary_relay(id: u8) -> Option<&'static str> {
    STOCK_RELAYS.get(id.checked_sub(1)? as usize).copied()
}

fn relay_dictionary_id(url: &str) -> Option<u8> {
    STOCK_RELAYS.iter().position(|r| *r == url).map(|i| (i + 1) as u8)
}

/// The decoded `#fragment`: the unlock token plus bootstrap relays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    pub token: [u8; 16],
    pub relays: Vec<String>,
}

/// Encode the fragment: `[version][flags][relays?][token:16]`, base64url, no
/// padding. Passing exactly the stock set (or nothing) sets the stock flag
/// and zero relay bytes.
pub fn encode_fragment(token: &[u8; 16], relays: &[String]) -> Result<String, InviteError> {
    let mut bytes = vec![FRAGMENT_VERSION];
    let is_stock = relays.is_empty() || relays.iter().map(String::as_str).eq(STOCK_RELAYS);
    if is_stock {
        bytes.push(FLAG_STOCK_SET);
    } else {
        if relays.len() > FRAGMENT_MAX_RELAYS {
            return Err(InviteError::Malformed(format!(
                "fragment carries at most {FRAGMENT_MAX_RELAYS} bootstrap relays"
            )));
        }
        bytes.push(0x00);
        bytes.push(relays.len() as u8);
        for relay in relays {
            if let Some(id) = relay_dictionary_id(relay) {
                bytes.push(id);
            } else if let Some(host) = relay.strip_prefix("wss://") {
                if host.len() > 255 {
                    return Err(InviteError::Malformed("relay host too long".into()));
                }
                bytes.push(0x00);
                bytes.push(host.len() as u8);
                bytes.extend_from_slice(host.as_bytes());
            } else {
                if relay.len() > 255 {
                    return Err(InviteError::Malformed("relay URL too long".into()));
                }
                bytes.push(0xFF);
                bytes.push(relay.len() as u8);
                bytes.extend_from_slice(relay.as_bytes());
            }
        }
    }
    bytes.extend_from_slice(token);
    Ok(base64_simd::URL_SAFE_NO_PAD.encode_to_string(&bytes))
}

/// Decode a fragment, bounding every read (a hostile link is parsed input).
pub fn decode_fragment(fragment: &str) -> Result<Fragment, InviteError> {
    let bytes = base64_simd::URL_SAFE_NO_PAD
        .decode_to_vec(fragment.as_bytes())
        .map_err(|e| InviteError::Malformed(format!("fragment base64url: {e}")))?;
    let mut at = 0usize;
    let mut next = |n: usize| -> Result<&[u8], InviteError> {
        let slice = bytes
            .get(at..at + n)
            .ok_or_else(|| InviteError::Malformed("fragment truncated".into()))?;
        at += n;
        Ok(slice)
    };

    let version = next(1)?[0];
    if version != FRAGMENT_VERSION {
        return Err(InviteError::Malformed(format!(
            "fragment version {version} (this dictionary generation is {FRAGMENT_VERSION})"
        )));
    }
    let flags = next(1)?[0];
    let relays = if flags & FLAG_STOCK_SET != 0 {
        STOCK_RELAYS.iter().map(|s| s.to_string()).collect()
    } else {
        let count = next(1)?[0] as usize;
        if count > FRAGMENT_MAX_RELAYS {
            return Err(InviteError::Malformed("too many bootstrap relays".into()));
        }
        let mut relays = Vec::with_capacity(count);
        for _ in 0..count {
            let lead = next(1)?[0];
            match lead {
                0x00 => {
                    let len = next(1)?[0] as usize;
                    let host = std::str::from_utf8(next(len)?)
                        .map_err(|_| InviteError::Malformed("relay host not UTF-8".into()))?;
                    relays.push(format!("wss://{host}"));
                }
                0xFF => {
                    let len = next(1)?[0] as usize;
                    let url = std::str::from_utf8(next(len)?)
                        .map_err(|_| InviteError::Malformed("relay URL not UTF-8".into()))?;
                    relays.push(url.to_string());
                }
                id => {
                    relays.push(
                        dictionary_relay(id)
                            .ok_or_else(|| InviteError::Malformed(format!("unknown dictionary relay {id}")))?
                            .to_string(),
                    );
                }
            }
        }
        relays
    };
    let token: [u8; 16] = next(16)?
        .try_into()
        .expect("sliced exactly 16");
    if at != bytes.len() {
        return Err(InviteError::Malformed("trailing fragment bytes".into()));
    }
    Ok(Fragment { token, relays })
}

// ============================================================================
// The link (§2)
// ============================================================================

/// The parsed protocol content of an invite URL: the bundle's coordinate and
/// the decoded fragment. The base is interchangeable; only these are protocol.
#[derive(Debug, Clone)]
pub struct InviteLink {
    /// The per-link signer's pubkey — the bundle's addressable author.
    pub link_signer: PublicKey,
    pub fragment: Fragment,
}

/// The bundle's addressable coordinate: `(33301, link_signer, "")` — the
/// per-link pubkey alone makes it unique, no identifier bytes, no relay
/// entries (relays travel compactly in the fragment).
pub fn bundle_coordinate(link_signer: PublicKey) -> Nip19Coordinate {
    Nip19Coordinate::new(
        Coordinate::new(Kind::Custom(kind::PUBLIC_INVITE), link_signer),
        Vec::<RelayUrl>::new(),
    )
}

/// Mint the shareable URL: `$BASE/invite/<naddr>#<fragment>`.
pub fn mint_link(
    base: &str,
    link_signer: PublicKey,
    token: &[u8; 16],
    relays: &[String],
) -> Result<String, InviteError> {
    let naddr = bundle_coordinate(link_signer)
        .to_bech32()
        .map_err(|e| InviteError::Malformed(e.to_string()))?;
    let fragment = encode_fragment(token, relays)?;
    Ok(format!("{}/invite/{naddr}#{fragment}", base.trim_end_matches('/')))
}

/// Parse an invite URL (any base). Returns the coordinate author and the
/// decoded fragment.
pub fn parse_link(url: &str) -> Result<InviteLink, InviteError> {
    let (path, fragment) = url
        .split_once('#')
        .ok_or_else(|| InviteError::Malformed("no #fragment".into()))?;
    let naddr = path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| InviteError::Malformed("no naddr in path".into()))?;
    let coord = Nip19Coordinate::from_bech32(naddr)
        .map_err(|e| InviteError::Malformed(format!("naddr: {e}")))?;
    if coord.kind != Kind::Custom(kind::PUBLIC_INVITE) || !coord.identifier.is_empty() {
        return Err(InviteError::Malformed("naddr is not an invite coordinate".into()));
    }
    Ok(InviteLink {
        link_signer: coord.public_key,
        fragment: decode_fragment(fragment)?,
    })
}

// ============================================================================
// The bundle event (§2) — kind 33301, bare on relays
// ============================================================================

fn bundle_conversation_key(token: &[u8; 16]) -> ConversationKey {
    ConversationKey::new(invite_bundle_key(token))
}

/// Publish form of a live bundle: addressable, authored by the link signer,
/// empty `d`, `vsk` 6.
pub fn build_bundle_event(
    link_signer: &Keys,
    bundle: &CommunityInvite,
    token: &[u8; 16],
    created_at_secs: u64,
) -> Result<Event, InviteError> {
    let json = serde_json::to_string(bundle).map_err(|e| InviteError::Malformed(e.to_string()))?;
    if json.len() > NIP44_MAX_PLAINTEXT {
        return Err(InviteError::Malformed("bundle exceeds the NIP-44 plaintext cap".into()));
    }
    let ct = encrypt_to_bytes(&bundle_conversation_key(token), json.as_bytes())
        .map_err(|e| InviteError::Crypto(e.to_string()))?;
    EventBuilder::new(Kind::Custom(kind::PUBLIC_INVITE), base64_simd::STANDARD.encode_to_string(&ct))
        .tags([
            Tag::identifier(""),
            Tag::custom(TagKind::Custom("vsk".into()), [vsk::INVITE_LIVE.to_string()]),
        ])
        .custom_created_at(Timestamp::from_secs(created_at_secs))
        .sign_with_keys(link_signer)
        .map_err(|e| InviteError::Crypto(e.to_string()))
}

/// The revocation tombstone: same coordinate, `vsk` 9, exactly as durable as
/// the bundle it replaces. Retiring the last live link flips Private.
pub fn build_revocation_event(link_signer: &Keys, created_at_secs: u64) -> Result<Event, InviteError> {
    EventBuilder::new(Kind::Custom(kind::PUBLIC_INVITE), "")
        .tags([
            Tag::identifier(""),
            Tag::custom(TagKind::Custom("vsk".into()), [vsk::INVITE_REVOKED.to_string()]),
        ])
        .custom_created_at(Timestamp::from_secs(created_at_secs))
        .sign_with_keys(link_signer)
        .map_err(|e| InviteError::Crypto(e.to_string()))
}

/// What a fetcher finds at a link's coordinate.
#[derive(Debug, Clone)]
pub enum FetchedBundle {
    Live(CommunityInvite),
    Revoked,
}

/// Open a fetched coordinate event: verify the signer matches the link (a
/// squatter is a different coordinate, but verify anyway), read the `vsk`
/// marker, decrypt and validate a live bundle.
pub fn open_bundle_event(
    event: &Event,
    expected_signer: &PublicKey,
    token: &[u8; 16],
) -> Result<FetchedBundle, InviteError> {
    if event.kind != Kind::Custom(kind::PUBLIC_INVITE) || event.pubkey != *expected_signer {
        return Err(InviteError::Malformed("not this link's bundle event".into()));
    }
    if event.verify().is_err() {
        return Err(InviteError::Malformed("bundle signature invalid".into()));
    }
    let marker = event
        .tags
        .iter()
        .find(|t| t.kind() == TagKind::Custom("vsk".into()))
        .and_then(|t| t.content())
        .ok_or_else(|| InviteError::Malformed("no vsk marker".into()))?;
    if marker == vsk::INVITE_REVOKED.to_string() {
        return Ok(FetchedBundle::Revoked);
    }
    if marker != vsk::INVITE_LIVE.to_string() {
        return Err(InviteError::Malformed(format!("unknown invite marker vsk {marker}")));
    }
    let ct = base64_simd::STANDARD
        .decode_to_vec(event.content.as_bytes())
        .map_err(|e| InviteError::Malformed(format!("bundle base64: {e}")))?;
    let json = decrypt_to_bytes(&bundle_conversation_key(token), &ct)
        .map_err(|e| InviteError::Crypto(e.to_string()))?;
    let mut bundle: CommunityInvite =
        serde_json::from_slice(&json).map_err(|e| InviteError::Malformed(e.to_string()))?;
    bundle.validate()?;
    Ok(FetchedBundle::Live(bundle))
}

// ============================================================================
// Direct invites (§6) — kind 3313 in a standard NIP-59 giftwrap
// ============================================================================

/// Build a Direct Invite: the bundle giftwrapped straight to an npub — the
/// classic NIP-59 shape (ephemeral wrap author, recipient `p`, kind 13 seal),
/// plus the outer `["k","3313"]` that makes invites indexable, and a NIP-40
/// expiration matching the bundle's.
pub async fn build_direct_invite<T: NostrSigner>(
    inviter: &T,
    recipient: &PublicKey,
    bundle: &CommunityInvite,
) -> Result<Event, InviteError> {
    let json = serde_json::to_string(bundle).map_err(|e| InviteError::Malformed(e.to_string()))?;
    let inviter_pk = inviter
        .get_public_key()
        .await
        .map_err(|e| InviteError::Crypto(e.to_string()))?;
    let rumor = EventBuilder::new(Kind::Custom(kind::DIRECT_INVITE), json).build(inviter_pk);
    let mut extra = vec![Tag::custom(TagKind::k(), [kind::DIRECT_INVITE.to_string()])];
    if let Some(expires_ms) = bundle.expires_at {
        extra.push(Tag::expiration(Timestamp::from_secs(expires_ms / 1000)));
    }
    EventBuilder::gift_wrap(inviter, recipient, rumor, extra)
        .await
        .map_err(|e| InviteError::Crypto(e.to_string()))
}

/// A received Direct Invite: the seal-verified inviter plus the validated
/// bundle. Nothing is fetched, joined, or announced on receipt — acceptance
/// is the user's, later.
#[derive(Debug, Clone)]
pub struct DirectInvite {
    pub inviter: PublicKey,
    pub bundle: CommunityInvite,
}

/// Unwrap a giftwrap into a Direct Invite. The outer `k` tag is an unsigned
/// hint and never authority: an invite is whatever unwraps to a kind 3313
/// rumor, so this accepts untagged wraps all the same.
pub async fn open_direct_invite<T: NostrSigner>(
    recipient: &T,
    wrap: &Event,
) -> Result<DirectInvite, InviteError> {
    let unwrapped = UnwrappedGift::from_gift_wrap(recipient, wrap)
        .await
        .map_err(|e| InviteError::Crypto(e.to_string()))?;
    if unwrapped.rumor.kind != Kind::Custom(kind::DIRECT_INVITE) {
        return Err(InviteError::Malformed(format!(
            "rumor kind {} is not a direct invite",
            unwrapped.rumor.kind
        )));
    }
    // The rumor must claim the seal's verified author.
    if unwrapped.rumor.pubkey != unwrapped.sender {
        return Err(InviteError::Malformed("rumor author differs from seal signer".into()));
    }
    let mut bundle: CommunityInvite = serde_json::from_str(&unwrapped.rumor.content)
        .map_err(|e| InviteError::Malformed(e.to_string()))?;
    bundle.validate()?;
    Ok(DirectInvite { inviter: unwrapped.sender, bundle })
}

/// The indexed lookup for exactly one's own invites:
/// `{"kinds":[1059], "#p":[me], "#k":["3313"]}`.
pub fn direct_invite_filter(me: &PublicKey) -> Filter {
    Filter::new()
        .kind(Kind::GiftWrap)
        .pubkey(*me)
        .custom_tags(SingleLetterTag::lowercase(Alphabet::K), [kind::DIRECT_INVITE.to_string()])
}

// ============================================================================
// The Invite List (§4) — kind 13303, self-encrypted bookkeeping
// ============================================================================

/// One minted link: immutable once minted; the token is the merge key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InviteListEntry {
    pub token: String,
    /// The link_signer secret — refreshing or retiring the bundle needs it.
    pub signer_sk: String,
    pub community_id: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InviteTombstone {
    pub token: String,
    pub community_id: String,
}

/// The creator's private bookkeeping, NIP-44-encrypted to self.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InviteList {
    #[serde(default)]
    pub entries: Vec<InviteListEntry>,
    #[serde(default)]
    pub tombstones: Vec<InviteTombstone>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl InviteList {
    /// Merge two device copies without coordination: entries union by token
    /// (immutable once minted), tombstones union, a tombstone beats its entry
    /// terminally — a stale device can never resurrect a revoked link.
    pub fn merge(&mut self, other: &InviteList) {
        for entry in &other.entries {
            if !self.entries.iter().any(|e| e.token == entry.token) {
                self.entries.push(entry.clone());
            }
        }
        for tomb in &other.tombstones {
            if !self.tombstones.iter().any(|t| t.token == tomb.token) {
                self.tombstones.push(tomb.clone());
            }
        }
        self.entries.sort_by(|a, b| a.token.cmp(&b.token));
        self.tombstones.sort_by(|a, b| a.token.cmp(&b.token));
    }

    pub fn is_revoked(&self, token: &str) -> bool {
        self.tombstones.iter().any(|t| t.token == token)
    }

    /// Entries not tombstoned (the links still live somewhere).
    pub fn live_entries(&self) -> impl Iterator<Item = &InviteListEntry> {
        self.entries.iter().filter(|e| !self.is_revoked(&e.token))
    }
}

/// Publish form: kind 13303, replaceable, signed by the real key, encrypted
/// to self.
pub fn build_invite_list_event(keys: &Keys, list: &InviteList, created_at_secs: u64) -> Result<Event, InviteError> {
    let json = serde_json::to_string(list).map_err(|e| InviteError::Malformed(e.to_string()))?;
    if json.len() > NIP44_MAX_PLAINTEXT {
        return Err(InviteError::Malformed("invite list exceeds the NIP-44 plaintext cap".into()));
    }
    let ct = nostr_sdk::nips::nip44::encrypt(keys.secret_key(), &keys.public_key(), &json, nostr_sdk::nips::nip44::Version::V2)
        .map_err(|e| InviteError::Crypto(e.to_string()))?;
    EventBuilder::new(Kind::Custom(kind::INVITE_LIST), ct)
        .custom_created_at(Timestamp::from_secs(created_at_secs))
        .sign_with_keys(keys)
        .map_err(|e| InviteError::Crypto(e.to_string()))
}

pub fn open_invite_list_event(keys: &Keys, event: &Event) -> Result<InviteList, InviteError> {
    let json = nostr_sdk::nips::nip44::decrypt(keys.secret_key(), &keys.public_key(), &event.content)
        .map_err(|e| InviteError::Crypto(e.to_string()))?;
    serde_json::from_str(&json).map_err(|e| InviteError::Malformed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concord::v2::derive;

    fn owner_bundle() -> (Keys, CommunityInvite) {
        let owner = Keys::generate();
        let salt = OwnerSalt([0x33; 32]);
        let cid = derive::community_id(&owner.public_key().to_bytes(), &salt);
        let bundle = CommunityInvite {
            community_id: cid.to_hex(),
            owner: owner.public_key().to_hex(),
            owner_salt: crate::simd::hex::bytes_to_hex_32(&salt.0),
            community_root: crate::simd::hex::bytes_to_hex_32(&[0x44; 32]),
            root_epoch: 0,
            channels: vec![ChannelGrant {
                id: crate::simd::hex::bytes_to_hex_32(&[0x77; 32]),
                key: None,
                epoch: 0,
                name: "general".into(),
            }],
            relays: vec!["wss://jskitty.com/nostr".into()],
            name: "Vector".into(),
            icon: None,
            expires_at: None,
            creator_npub: None,
            label: None,
            extra: Default::default(),
        };
        (owner, bundle)
    }

    #[test]
    fn bundle_self_certifies_the_owner() {
        let (_, mut bundle) = owner_bundle();
        assert!(bundle.validate().is_ok());
        // A smuggled owner fails the commitment.
        let mut forged = bundle.clone();
        forged.owner = Keys::generate().public_key().to_hex();
        assert!(matches!(forged.validate(), Err(InviteError::OwnerMismatch)));
    }

    #[test]
    fn bundle_bounds_are_enforced_before_allocation() {
        let (_, mut bundle) = owner_bundle();
        bundle.channels = (0..=INVITE_MAX_CHANNELS)
            .map(|_| bundle.channels[0].clone())
            .collect();
        assert!(matches!(bundle.validate(), Err(InviteError::TooManyChannels(_))));

        let (_, mut bundle) = owner_bundle();
        bundle.relays = (0..10).map(|i| format!("wss://r{i}")).collect();
        bundle.validate().unwrap();
        assert_eq!(bundle.relays.len(), RELAYS_RECOMMENDED_MAX, "relay list trimmed to the cap");
    }

    #[test]
    fn expiry_refuses_joining_not_previewing() {
        let (_, mut bundle) = owner_bundle();
        bundle.expires_at = Some(1_000);
        bundle.validate().unwrap(); // still previews
        assert!(bundle.is_expired(1_001));
        assert!(!bundle.is_expired(999));
    }

    #[test]
    fn fragment_stock_set_is_two_bytes_plus_token() {
        let token = [5u8; 16];
        let stock: Vec<String> = STOCK_RELAYS.iter().map(|s| s.to_string()).collect();
        let frag = encode_fragment(&token, &stock).unwrap();
        // [version][flags][token:16] = 18 bytes → 24 base64url chars.
        assert_eq!(frag.len(), 24);
        let decoded = decode_fragment(&frag).unwrap();
        assert_eq!(decoded.token, token);
        assert_eq!(decoded.relays, stock);
        // Empty relay list also selects the stock set.
        assert_eq!(encode_fragment(&token, &[]).unwrap(), frag);
    }

    #[test]
    fn fragment_dictionary_and_literals_roundtrip() {
        let token = [9u8; 16];
        let relays = vec![
            "wss://relay.ditto.pub".to_string(),   // dictionary id 3
            "wss://my.own.relay".to_string(),      // wss-implied literal
            "ws://localhost:7777".to_string(),     // verbatim literal
        ];
        let frag = encode_fragment(&token, &relays).unwrap();
        let decoded = decode_fragment(&frag).unwrap();
        assert_eq!(decoded.token, token);
        assert_eq!(decoded.relays, relays);
    }

    #[test]
    fn fragment_rejects_hostile_input() {
        let token = [1u8; 16];
        // More than 3 bootstrap relays refused at encode.
        let many: Vec<String> = (0..4).map(|i| format!("wss://r{i}")).collect();
        assert!(encode_fragment(&token, &many).is_err());
        // Truncated fragment.
        let frag = encode_fragment(&token, &[]).unwrap();
        assert!(decode_fragment(&frag[..frag.len() - 4]).is_err());
        // Legacy/wrong version byte.
        let mut bytes = base64_simd::URL_SAFE_NO_PAD.decode_to_vec(frag.as_bytes()).unwrap();
        bytes[0] = 3;
        let legacy = base64_simd::URL_SAFE_NO_PAD.encode_to_string(&bytes);
        assert!(decode_fragment(&legacy).is_err());
        // Unknown dictionary id.
        let unknown = {
            let mut b = vec![FRAGMENT_VERSION, 0x00, 1, 200];
            b.extend_from_slice(&token);
            base64_simd::URL_SAFE_NO_PAD.encode_to_string(&b)
        };
        assert!(decode_fragment(&unknown).is_err());
    }

    #[test]
    fn link_mints_and_parses_on_any_base() {
        let signer = Keys::generate();
        let token = [7u8; 16];
        let url = mint_link("https://vectorapp.io", signer.public_key(), &token, &[]).unwrap();
        assert!(url.starts_with("https://vectorapp.io/invite/naddr1"));
        let parsed = parse_link(&url).unwrap();
        assert_eq!(parsed.link_signer, signer.public_key());
        assert_eq!(parsed.fragment.token, token);
        // The same naddr+fragment opens on any redirect base.
        let other_base = url.replace("https://vectorapp.io", "https://armada.soapbox.pub");
        assert_eq!(parse_link(&other_base).unwrap().link_signer, signer.public_key());
    }

    #[test]
    fn bundle_event_roundtrip_and_revocation() {
        let (_, bundle) = owner_bundle();
        let signer = Keys::generate();
        let token = [7u8; 16];
        let event = build_bundle_event(&signer, &bundle, &token, 1_719_800_000).unwrap();
        assert_eq!(event.kind.as_u16(), kind::PUBLIC_INVITE);

        match open_bundle_event(&event, &signer.public_key(), &token).unwrap() {
            FetchedBundle::Live(got) => assert_eq!(got.name, "Vector"),
            FetchedBundle::Revoked => panic!("expected live"),
        }

        // The wrong token cannot open it.
        assert!(open_bundle_event(&event, &signer.public_key(), &[8u8; 16]).is_err());
        // A squatter's event is a different author: refused.
        assert!(open_bundle_event(&event, &Keys::generate().public_key(), &token).is_err());

        // Revocation replaces the bundle with a grave.
        let tomb = build_revocation_event(&signer, 1_722_400_000).unwrap();
        assert!(matches!(
            open_bundle_event(&tomb, &signer.public_key(), &token).unwrap(),
            FetchedBundle::Revoked
        ));
    }

    #[tokio::test]
    async fn direct_invite_roundtrip() {
        let (_, mut bundle) = owner_bundle();
        bundle.expires_at = Some(1_735_689_600_000);
        let inviter = Keys::generate();
        let recipient = Keys::generate();
        let wrap = build_direct_invite(&inviter, &recipient.public_key(), &bundle).await.unwrap();

        // The classic NIP-59 shape: ephemeral author, recipient p, k hint,
        // NIP-40 expiration matching the bundle.
        assert_eq!(wrap.kind, Kind::GiftWrap);
        assert_ne!(wrap.pubkey, inviter.public_key(), "wrap author is ephemeral");
        let k = wrap.tags.iter().find(|t| t.kind() == TagKind::k()).and_then(|t| t.content());
        assert_eq!(k, Some("3313"));
        let exp = wrap.tags.iter().find(|t| t.kind() == TagKind::Expiration).and_then(|t| t.content());
        assert_eq!(exp, Some("1735689600"));

        let opened = open_direct_invite(&recipient, &wrap).await.unwrap();
        assert_eq!(opened.inviter, inviter.public_key(), "seal proves who invited");
        assert_eq!(opened.bundle.name, "Vector");

        // The wrong recipient cannot open it.
        assert!(open_direct_invite(&Keys::generate(), &wrap).await.is_err());
    }

    #[tokio::test]
    async fn direct_invite_refuses_a_non_invite_rumor() {
        let inviter = Keys::generate();
        let recipient = Keys::generate();
        let rumor = EventBuilder::new(Kind::Custom(9), "not an invite").build(inviter.public_key());
        let wrap = EventBuilder::gift_wrap(&inviter, &recipient.public_key(), rumor, [])
            .await
            .unwrap();
        assert!(open_direct_invite(&recipient, &wrap).await.is_err());
    }

    #[test]
    fn invite_list_merges_and_tombstones_terminally() {
        let entry = |token: &str| InviteListEntry {
            token: token.into(),
            signer_sk: "aa".into(),
            community_id: "bb".into(),
            url: "https://x/invite/n#f".into(),
            label: Some("Reddit".into()),
            created_at: 1,
            expires_at: None,
            extra: Default::default(),
        };
        let mut device_a = InviteList {
            entries: vec![entry("t1"), entry("t2")],
            tombstones: vec![],
            extra: Default::default(),
        };
        let device_b = InviteList {
            entries: vec![entry("t2"), entry("t3")],
            tombstones: vec![InviteTombstone { token: "t1".into(), community_id: "bb".into() }],
            extra: Default::default(),
        };
        device_a.merge(&device_b);
        assert_eq!(device_a.entries.len(), 3, "entries union by token");
        assert!(device_a.is_revoked("t1"), "a stale device can never resurrect a revoked link");
        let live: Vec<&str> = device_a.live_entries().map(|e| e.token.as_str()).collect();
        assert_eq!(live, vec!["t2", "t3"]);
        // Merge is idempotent.
        let snapshot = device_a.clone();
        device_a.merge(&snapshot.clone());
        assert_eq!(device_a, snapshot);
    }

    #[test]
    fn invite_list_event_roundtrip() {
        let keys = Keys::generate();
        let list = InviteList {
            entries: vec![InviteListEntry {
                token: "t1".into(),
                signer_sk: "sk".into(),
                community_id: "cid".into(),
                url: "https://x/invite/n#f".into(),
                label: None,
                created_at: 1,
                expires_at: Some(2),
                extra: Default::default(),
            }],
            tombstones: vec![],
            extra: Default::default(),
        };
        let event = build_invite_list_event(&keys, &list, 1_722_400_000).unwrap();
        assert_eq!(event.kind.as_u16(), kind::INVITE_LIST);
        let back = open_invite_list_event(&keys, &event).unwrap();
        assert_eq!(back, list);
        // Another key cannot read it.
        assert!(open_invite_list_event(&Keys::generate(), &event).is_err());
    }
}
