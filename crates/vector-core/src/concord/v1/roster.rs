//! Folding fetched authority editions into the current roster.
//!
//! The control plane is a set of per-entity append editions (§edition), each real-npub-signed and
//! version-chained (§version). This module is the consumer: given the (already-decrypted) inner
//! edition events, it verifies each authorship signature, groups editions per entity, folds each
//! entity's version chain to its current head, and deserializes the heads into the in-memory
//! [`roles::CommunityRoles`] the authority gates query.
//!
//! Two layers: [`fold_roster`] produces the "validly signed, anchored, bound, current" roster (the
//! inner signature proves WHO authored each edition), and [`authorize_delegation`] then filters it by
//! the delegation chain — deciding WHETHER each signer was allowed (rank + the chain to the
//! owner), so a self-signed or forged-delegation entry never becomes trusted authority. Entities that
//! come back with a chain gap (unanchored / withheld prereqs, §version) are reported so the caller can
//! fail closed (tracking) or accept-via-authority (bootstrapping) per.

use super::derive::channel_pseudonym;
use super::{cipher, edition, roles, version, ChannelId, ChannelKey, CommunityId, Epoch, ServerRootKey};
use crate::stored_event::event_kind;
use nostr_sdk::prelude::*;
use std::collections::HashMap;

/// Sub-kinds. The fold here interprets ROLE/GRANT/BANLIST (authority); COMMUNITY_ROOT/CHANNEL
/// are display metadata, built as editions here but applied by the metadata consumer.
const VSK_COMMUNITY_ROOT: &str = "0";
const VSK_ROLE: &str = "1";
const VSK_CHANNEL: &str = "2";
const VSK_GRANT: &str = "3";
const VSK_BANLIST: &str = "4";
// vsk allocations 0-7 are all spoken for (5=RoleOrder reserved-unbuilt, 6=PublicInvite is the
// token-signed bundle, 7=OwnerAttestation). The invite-link REGISTRY (the member-readable Public/Private
// source of truth) is a NEW control entity at the next free number, 8. Never reuse 0-7.
const VSK_INVITE_LINKS: &str = "8";
/// vsk=10: the owner-dissolution tombstone. 9 = public-invite-revoked. The tombstone lives at
/// `dissolved_locator(community_id)` and has NO version chain / prev-hash — presence of ≥1 valid
/// owner-signed edition at the locator IS the state (it is exempt from `check_chain_shape` + the fold's
/// version discipline). Never reuse.
const VSK_DISSOLVED: &str = "10";

/// Hard cap on editions processed per fold — bounds the Schnorr-verify + fold work a hostile relay
/// can force by piling junk at the control coordinate. Legit control history is far smaller.
/// pub(crate): the fetch layer also bounds its AEAD open loop with this.
pub(crate) const MAX_CONTROL_EDITIONS: usize = 50_000;

/// Validate a 64-char lowercase/uppercase hex string and decode it, returning `None` on bad
/// length/charset (so [`crate::simd::hex::hex_to_bytes_32`]'s silent zero-on-invalid never bites).
fn hex32(s: &str) -> Option<[u8; 32]> {
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| crate::simd::hex::hex_to_bytes_32(s))
}

/// Reject a malformed edition **chain shape** at mint time, so a mis-wired caller fails loud here
/// rather than minting an edition the consumer fold silently quarantines (a baffling "my change never
/// took effect" outage). NOTE: this validates only the *shape* — that the *value* of `prev_hash`
/// actually equals the prior edition's `edition_hash` is the caller's responsibility (only the caller
/// holds the prior head), and a wrong value likewise fails silently at fold, not here.
fn check_chain_shape(version: u64, prev_hash: Option<&[u8; 32]>) -> Result<(), String> {
    match (version, prev_hash) {
        (0, _) => Err("edition version starts at 1".to_string()),
        (1, Some(_)) => Err("genesis edition (v1) must have no prev_hash".to_string()),
        (v, None) if v > 1 => Err("continuation edition (v>1) requires a prev_hash".to_string()),
        _ => Ok(()),
    }
}

/// Build a signed **RoleMetadata** edition (vsk=1) at its bound coordinate (`entity_id == role_id`),
/// the next version in the role's chain. Signed by the ACTOR's real keys (the authorship proof); the
/// caller supplies the next `version` + the prior edition's `prev_hash` (the held head's `self_hash`,
/// or `None` with `version == 1` for a brand-new role — see [`check_chain_shape`]). The resulting
/// inner event is what the envelope then seals under the server-root key for publication.
pub fn build_role_edition(
    actor: &Keys,
    role: &roles::Role,
    version: u64,
    prev_hash: Option<&[u8; 32]>,
    created_at: u64,
    authority: Option<&edition::AuthorityCitation>,
) -> Result<Event, String> {
    build_role_edition_unsigned(actor.public_key(), role, version, prev_hash, created_at, authority)?
        .sign_with_keys(actor)
        .map_err(|e| format!("sign role edition: {e}"))
}

/// The UNSIGNED RoleMetadata edition (the bunker path): build the inner, then sign it with the active
/// `NostrSigner` (`unsigned.sign(&signer).await`) so a NIP-46 remote signer works. The sync
/// [`build_role_edition`] is the local-keys convenience over this.
pub fn build_role_edition_unsigned(
    author: PublicKey,
    role: &roles::Role,
    version: u64,
    prev_hash: Option<&[u8; 32]>,
    created_at: u64,
    authority: Option<&edition::AuthorityCitation>,
) -> Result<UnsignedEvent, String> {
    check_chain_shape(version, prev_hash)?;
    let entity_id = hex32(&role.role_id).ok_or("role_id must be 64-char hex")?;
    let content = serde_json::to_string(role).map_err(|e| e.to_string())?;
    Ok(edition::build_edition_inner(author, VSK_ROLE, &entity_id, version, prev_hash, &content, created_at, authority))
}

/// Build a signed **Grant** edition (vsk=3) at its bound coordinate
/// (`entity_id == grant_locator(community_id, member)` — community-scoped so it survives a base
/// rotation, the keystone for re-anchoring), the next version in that member's grant chain.
/// Signed by the actor's real keys. An empty `role_ids` is a revoke (folds to no entry).
pub fn build_grant_edition(
    actor: &Keys,
    community_id: &CommunityId,
    grant: &roles::MemberGrant,
    version: u64,
    prev_hash: Option<&[u8; 32]>,
    created_at: u64,
    authority: Option<&edition::AuthorityCitation>,
) -> Result<Event, String> {
    build_grant_edition_unsigned(actor.public_key(), community_id, grant, version, prev_hash, created_at, authority)?
        .sign_with_keys(actor)
        .map_err(|e| format!("sign grant edition: {e}"))
}

/// The UNSIGNED Grant edition (the bunker path); sign with the active `NostrSigner`.
pub fn build_grant_edition_unsigned(
    author: PublicKey,
    community_id: &CommunityId,
    grant: &roles::MemberGrant,
    version: u64,
    prev_hash: Option<&[u8; 32]>,
    created_at: u64,
    authority: Option<&edition::AuthorityCitation>,
) -> Result<UnsignedEvent, String> {
    check_chain_shape(version, prev_hash)?;
    let member_bytes = hex32(&grant.member).ok_or("member must be 64-char hex")?;
    let entity_id = super::derive::grant_locator(community_id, &member_bytes);
    let content = serde_json::to_string(grant).map_err(|e| e.to_string())?;
    Ok(edition::build_edition_inner(author, VSK_GRANT, &entity_id, version, prev_hash, &content, created_at, authority))
}

/// Build a signed **Banlist** edition (vsk=4) at the single community-wide coordinate
/// (`entity_id == banlist_locator(community_id)` — community-scoped so it survives a base rotation and
/// re-anchors). Content is the JSON array of
/// banned pubkeys (lowercase hex). Signed by the actor's real keys; the consumer ([`fold_roster`] +
/// the BAN-authority check) decides whether that signer was allowed to ban.
pub fn build_banlist_edition(
    actor: &Keys,
    community_id: &CommunityId,
    banned: &[String],
    version: u64,
    prev_hash: Option<&[u8; 32]>,
    created_at: u64,
    authority: Option<&edition::AuthorityCitation>,
) -> Result<Event, String> {
    build_banlist_edition_unsigned(actor.public_key(), community_id, banned, version, prev_hash, created_at, authority)?
        .sign_with_keys(actor)
        .map_err(|e| format!("sign banlist edition: {e}"))
}

/// The UNSIGNED Banlist edition (the bunker path); sign with the active `NostrSigner`.
pub fn build_banlist_edition_unsigned(
    author: PublicKey,
    community_id: &CommunityId,
    banned: &[String],
    version: u64,
    prev_hash: Option<&[u8; 32]>,
    created_at: u64,
    authority: Option<&edition::AuthorityCitation>,
) -> Result<UnsignedEvent, String> {
    check_chain_shape(version, prev_hash)?;
    let entity_id = super::derive::banlist_locator(community_id);
    let content = serde_json::to_string(banned).map_err(|e| e.to_string())?;
    Ok(edition::build_edition_inner(author, VSK_BANLIST, &entity_id, version, prev_hash, &content, created_at, authority))
}

/// Build a signed **GroupDissolved** tombstone (vsk=10) at the community-scoped dissolved locator
/// (`entity_id == dissolved_locator(community_id)` — rotation-stable so a post-rotation joiner still
/// finds it). UNLIKE every other edition this has NO version chain and NO prev-hash: it is minted at
/// a fixed `version == 1` with no `prev_hash` and is exempt from `check_chain_shape` — presence of ≥1 valid
/// OWNER-signed edition here IS dissolution (duplicates are idempotent). Content is minimal (`{}`); the
/// signer (the owner) is the whole payload. Authority (the signer == proven owner) is the CALLER's check.
pub fn build_group_dissolved_edition(
    actor: &Keys,
    community_id: &CommunityId,
    created_at: u64,
) -> Result<Event, String> {
    build_group_dissolved_edition_unsigned(actor.public_key(), community_id, created_at)
        .sign_with_keys(actor)
        .map_err(|e| format!("sign dissolved edition: {e}"))
}

/// The UNSIGNED GroupDissolved tombstone (the bunker path — the owner may sign remotely); sign with the
/// active `NostrSigner`. No chain discipline (see [`build_group_dissolved_edition`]).
pub fn build_group_dissolved_edition_unsigned(
    author: PublicKey,
    community_id: &CommunityId,
    created_at: u64,
) -> UnsignedEvent {
    let entity_id = super::derive::dissolved_locator(community_id);
    // Chain-free terminal marker: fixed v1, no prev_hash, empty content.
    edition::build_edition_inner(author, VSK_DISSOLVED, &entity_id, 1, None, "{}", created_at, None)
}

/// Build a signed **InviteLinks** edition (vsk=8) at the CREATOR's own coordinate
/// (`entity_id == invite_links_locator(community_id, actor)`). Content is the JSON array of THAT
/// creator's active public-invite-link LOCATORS (lowercase hex; the locator is public — the token in the
/// URL is the secret, never listed). Per-creator: a creator publishes only their own list; members
/// fold every creator's list (gated on the author holding `CREATE_INVITE`) into the aggregate active-set,
/// the source of truth for the Public/Private mode + registry-authoritative joins. No shared registry.
pub fn build_invite_links_edition(
    actor: &Keys,
    community_id: &CommunityId,
    link_locators: &[String],
    version: u64,
    prev_hash: Option<&[u8; 32]>,
    created_at: u64,
    authority: Option<&edition::AuthorityCitation>,
) -> Result<Event, String> {
    build_invite_links_edition_unsigned(actor.public_key(), community_id, link_locators, version, prev_hash, created_at, authority)?
        .sign_with_keys(actor)
        .map_err(|e| format!("sign invite-links edition: {e}"))
}

/// The UNSIGNED InviteLinks edition (the bunker path); sign with the active `NostrSigner`. The entity
/// coordinate binds to `author`, so a creator can only publish links at their own coordinate.
pub fn build_invite_links_edition_unsigned(
    author: PublicKey,
    community_id: &CommunityId,
    link_locators: &[String],
    version: u64,
    prev_hash: Option<&[u8; 32]>,
    created_at: u64,
    authority: Option<&edition::AuthorityCitation>,
) -> Result<UnsignedEvent, String> {
    check_chain_shape(version, prev_hash)?;
    let entity_id = super::derive::invite_links_locator(community_id, &author.to_bytes());
    let content = serde_json::to_string(link_locators).map_err(|e| e.to_string())?;
    Ok(edition::build_edition_inner(author, VSK_INVITE_LINKS, &entity_id, version, prev_hash, &content, created_at, authority))
}

/// Build a signed **GroupRoot** edition (vsk=0) at the community-wide coordinate (`entity_id ==
/// community_id`) — the Community's display descriptor (name/description/icon/banner + owner
/// attestation). Real-npub signed; the consumer applies it only if the signer held `MANAGE_METADATA`.
pub fn build_community_root_edition(
    actor: &Keys,
    community_id: &CommunityId,
    meta: &super::metadata::CommunityMetadata,
    version: u64,
    prev_hash: Option<&[u8; 32]>,
    created_at: u64,
    authority: Option<&edition::AuthorityCitation>,
) -> Result<Event, String> {
    build_community_root_edition_unsigned(actor.public_key(), community_id, meta, version, prev_hash, created_at, authority)?
        .sign_with_keys(actor)
        .map_err(|e| format!("sign community-root edition: {e}"))
}

/// The UNSIGNED GroupRoot edition (the bunker path); sign with the active `NostrSigner`.
pub fn build_community_root_edition_unsigned(
    author: PublicKey,
    community_id: &CommunityId,
    meta: &super::metadata::CommunityMetadata,
    version: u64,
    prev_hash: Option<&[u8; 32]>,
    created_at: u64,
    authority: Option<&edition::AuthorityCitation>,
) -> Result<UnsignedEvent, String> {
    check_chain_shape(version, prev_hash)?;
    let content = serde_json::to_string(meta).map_err(|e| e.to_string())?;
    Ok(edition::build_edition_inner(author, VSK_COMMUNITY_ROOT, &community_id.0, version, prev_hash, &content, created_at, authority))
}

/// Build a signed **ChannelMetadata** edition (vsk=2) at the channel's coordinate (`entity_id ==
/// channel_id`) — the channel's display descriptor (name). Real-npub signed; the consumer applies it
/// only if the signer held `MANAGE_CHANNELS`.
pub fn build_channel_metadata_edition(
    actor: &Keys,
    channel_id: &ChannelId,
    meta: &super::metadata::ChannelMetadata,
    version: u64,
    prev_hash: Option<&[u8; 32]>,
    created_at: u64,
    authority: Option<&edition::AuthorityCitation>,
) -> Result<Event, String> {
    build_channel_metadata_edition_unsigned(actor.public_key(), channel_id, meta, version, prev_hash, created_at, authority)?
        .sign_with_keys(actor)
        .map_err(|e| format!("sign channel-metadata edition: {e}"))
}

/// The UNSIGNED ChannelMetadata edition (the bunker path); sign with the active `NostrSigner`.
pub fn build_channel_metadata_edition_unsigned(
    author: PublicKey,
    channel_id: &ChannelId,
    meta: &super::metadata::ChannelMetadata,
    version: u64,
    prev_hash: Option<&[u8; 32]>,
    created_at: u64,
    authority: Option<&edition::AuthorityCitation>,
) -> Result<UnsignedEvent, String> {
    check_chain_shape(version, prev_hash)?;
    let content = serde_json::to_string(meta).map_err(|e| e.to_string())?;
    Ok(edition::build_edition_inner(author, VSK_CHANNEL, &channel_id.0, version, prev_hash, &content, created_at, authority))
}

/// The relay-filterable pseudonym members use to fetch the control plane for `(community, epoch)`.
/// Derived from the server-root key + community id, so members compute it but outsiders (no
/// server-root) can't — the control plane has no stable on-wire identifier. This reuses the
/// `channel-pseudonym` derivation; what keeps a control pseudonym from ever aliasing a channel's is
/// the invariant that the server-root key is always distinct from every channel key (the HKDF
/// label is shared, so domain separation rests on the distinct IKM + id32).
pub fn control_pseudonym(server_root: &ServerRootKey, community_id: &CommunityId, epoch: Epoch) -> String {
    channel_pseudonym(&ChannelKey(*server_root.as_bytes()), &ChannelId(community_id.0), epoch).to_hex()
}

/// Seal a signed control edition (kind 3308) for the wire: encrypt the inner under the **server-root
/// key** (only members decrypt), with an **ephemeral outer signer** (the real author is the inner
/// signature, hidden from relays), addressed by the control-plane pseudonym so members fetch by
/// `#z` without exposing a stable group identifier.
pub fn seal_control_edition(
    ephemeral: &Keys,
    inner: &Event,
    server_root: &ServerRootKey,
    community_id: &CommunityId,
    epoch: Epoch,
) -> Result<Event, String> {
    if inner.kind.as_u16() != event_kind::COMMUNITY_CONTROL {
        return Err("a control edition must be kind 3308".to_string());
    }
    let content = cipher::seal(server_root.as_bytes(), inner.as_json().as_bytes())
        .map_err(|e| format!("seal control edition: {e}"))?;
    let pseudonym = control_pseudonym(server_root, community_id, epoch);
    EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_CONTROL), content)
        .tags([
            Tag::custom(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Z)), [pseudonym]),
            Tag::custom(TagKind::Custom("v".into()), ["1".to_string()]),
        ])
        .sign_with_keys(ephemeral)
        .map_err(|e| format!("sign control outer: {e}"))
}

/// Open a control-edition outer → its inner edition event (decrypt under the server-root key). Does
/// NOT verify the inner signature or parse fields — pass the result to [`edition::parse_edition_inner`]
/// (which verifies authorship) and then [`fold_roster`] (which binds + folds). A wrong server-root
/// key fails to decrypt → `Err`, which is also how **cross-community** replay is rejected.
///
/// CROSS-EPOCH replay (same community) is NOT an envelope concern and is deliberately not blocked
/// here: the control plane is encrypted under the (epoch-agnostic) server-root key, so any edition
/// can be re-wrapped under any epoch's pseudonym — the envelope cannot bind an epoch, and binding the
/// inner to an epoch would break re-anchoring (a re-WRAP, not a re-sign). It is a COMPLETENESS-layer
/// defense: a *tracking* client is protected by the version chain's refuse-downgrade; a
/// *bootstrapping* joiner relies on quorum reconciliation + the re-anchoring guarantee (the current
/// head is re-posted under the current epoch). Those MUST be wired into the fetch path (the rekey
/// increment) before a fresh joiner is safe against a withheld-demotion replay. The single-epoch MVP
/// (never rotates) is unaffected.
pub fn open_control_edition(outer: &Event, server_root: &ServerRootKey) -> Result<Event, String> {
    if outer.kind.as_u16() != event_kind::COMMUNITY_CONTROL {
        return Err("not a control-plane outer (kind != 3308)".to_string());
    }
    match outer.tags.iter().find_map(|t| {
        let s = t.as_slice();
        (s.len() >= 2 && s[0] == "v").then(|| s[1].clone())
    }) {
        Some(v) if v == "1" => {}
        other => return Err(format!("unsupported control edition version: {other:?}")),
    }
    let plaintext = cipher::open(server_root.as_bytes(), &outer.content)
        .map_err(|e| format!("open control edition: {e}"))?;
    let json = String::from_utf8(plaintext).map_err(|e| format!("control inner utf8: {e}"))?;
    let inner = Event::from_json(&json).map_err(|e| format!("control inner parse: {e}"))?;
    if inner.kind.as_u16() != event_kind::COMMUNITY_CONTROL {
        return Err("control inner is not kind 3308".to_string());
    }
    Ok(inner)
}

/// Seal a GroupDissolved tombstone for the wire at the ROTATION-STABLE coordinate: encrypt the
/// inner under the community-id-derived `dissolved_envelope_key` (NOT the per-epoch server root) and
/// address it by `dissolved_pseudonym` (NOT `control_pseudonym`), so it is discoverable + openable by any
/// member or joiner at any epoch. Ephemeral outer signer (the owner is the inner signature). The tombstone
/// is also published at the current `control_pseudonym` (a current-epoch fast path); this is the cross-epoch
/// path that survives a concurrent re-founding.
pub fn seal_dissolved_edition(ephemeral: &Keys, inner: &Event, community_id: &CommunityId) -> Result<Event, String> {
    if inner.kind.as_u16() != event_kind::COMMUNITY_CONTROL {
        return Err("a dissolved tombstone must be kind 3308".to_string());
    }
    let key = super::derive::dissolved_envelope_key(community_id);
    let content = cipher::seal(&key, inner.as_json().as_bytes())
        .map_err(|e| format!("seal dissolved edition: {e}"))?;
    let pseudonym = super::derive::dissolved_pseudonym(community_id);
    EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_CONTROL), content)
        .tags([
            Tag::custom(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Z)), [pseudonym]),
            Tag::custom(TagKind::Custom("v".into()), ["1".to_string()]),
        ])
        .sign_with_keys(ephemeral)
        .map_err(|e| format!("sign dissolved outer: {e}"))
}

/// Open a wire event at the dissolved coordinate and, IF it is a well-formed GroupDissolved tombstone
/// (`vsk=10` at `dissolved_locator`), return its inner real-npub signer. The CALLER decides validity by
/// checking that signer equals the proven owner (fail-closed authority). `None` for anything else
/// (wrong key, malformed, wrong vsk, relabelled entity_id).
pub fn dissolved_tombstone_signer(outer: &Event, community_id: &CommunityId) -> Option<PublicKey> {
    if outer.kind.as_u16() != event_kind::COMMUNITY_CONTROL {
        return None;
    }
    let key = super::derive::dissolved_envelope_key(community_id);
    let plaintext = cipher::open(&key, &outer.content).ok()?;
    let inner = Event::from_json(&String::from_utf8(plaintext).ok()?).ok()?;
    let p = edition::parse_edition_inner(&inner).ok()?;
    (p.vsk == VSK_DISSOLVED && p.entity_id == super::derive::dissolved_locator(community_id)).then_some(p.author)
}

/// The folded current head of one control entity — what the caller persists via `set_edition_head`
/// (the monotonic refuse-downgrade floor) and the send side reads to emit the next version.
#[derive(Clone)]
pub struct EntityHead {
    /// The entity coordinate (role_id for roles, grant_locator for grants), lowercase hex.
    pub entity_hex: String,
    pub version: u64,
    pub self_hash: [u8; 32],
    /// The head edition's deterministic tiebreak key (the inner edition id). Used to resolve a
    /// same-version concurrent fork: every client converges on the lower `inner_id` among authorized
    /// editions, and the persisted head ranks by it so a same-version adopt only moves toward the min.
    pub inner_id: [u8; 32],
    /// The authority citation the head edition carried, if any — the actor's pinned proof of
    /// the authority they claimed for this edit. `None` for an owner-signed edition (supreme, cites
    /// nothing) or an uncited one. Verifiers resolve the actor's standing at the cited grant version
    /// via [`authority_citation_satisfied`].
    pub citation: Option<edition::AuthorityCitation>,
}

/// The outcome of folding the control plane.
#[derive(Clone)]
pub struct FoldedRoster {
    /// The anchored, bound, validly-signed heads. This is "validly signed, anchored, and current" — NOT
    /// yet "authorized." [`authorize_delegation`] is the next layer: it filters these by the 
    /// delegation chain (each signer must outrank what it defines, chaining to the owner). Use the
    /// authorized result for any authority decision; this raw roster is the binding-layer output.
    pub roles: roles::CommunityRoles,
    /// The signer (inner real-npub author) of each entry in `roles.roles`, SAME ORDER — so
    /// [`authorize_delegation`] can check whether that signer was allowed to define the role.
    pub role_authors: Vec<PublicKey>,
    /// The signer of each entry in `roles.grants`, SAME ORDER (the delegation chain needs the granter).
    pub grant_authors: Vec<PublicKey>,
    /// Entity ids whose head is NOT chain-anchored (a gap, §version). These are **quarantined** — NOT
    /// folded into `roles` (fail closed). A bootstrapping joiner re-verifies them via authority
    /// before trusting; a tracking client refetches the missing prereqs.
    pub gapped_entities: Vec<[u8; 32]>,
    /// Count of editions dropped: bad signature, missing/duplicate fields, an `entity_id`↔content
    /// mismatch, unparseable content, or beyond the cap. **Nonzero ⇒ the roster may be degraded**
    /// (a role/grant could be silently missing), so the caller should refetch rather than trust it.
    pub skipped: usize,
    /// Count of RAW control editions the source fetch returned (before opening/folding). Set by
    /// `fetch_control_folded`; `0` for a roster folded from a hand-supplied edition set (tests, prefolds).
    /// `fetch_and_apply_control` surfaces it so an admin-write guard can tell "≥1 relay responded" (any
    /// raw event) from total network isolation — without a second, throwaway probe fetch.
    pub fetched: usize,
    /// The current head `(entity_hex, version, self_hash)` of every successfully-bound ROLE/GRANT
    /// entity, for the caller to advance `set_edition_head` (monotonic). Excludes the banlist (its head
    /// is `banlist_head`, advanced by the banlist path) and gapped/quarantined entities.
    pub heads: Vec<EntityHead>,
    /// The folded banlist content (banned pubkeys, lowercase hex) — only meaningful once the BAN
    /// authority of `banlist_author` is checked against the authorized roster. Empty if no banlist
    /// edition folded (distinct from "an authored empty banlist," which the caller learns via
    /// `banlist_head`/`banlist_author` being `Some`).
    pub banned: Vec<String>,
    /// The signer (inner real-npub author) of the folded banlist head, so the caller can verify they
    /// held `BAN`. `None` if no banlist edition folded.
    pub banlist_author: Option<PublicKey>,
    /// The DISTINCT signers of every well-formed GroupDissolved tombstone (vsk=10) at `dissolved_locator`
    /// Detection is owner-SIGNATURE-filtered, NOT position/version dependent: the fold scans the
    /// locator directly (NOT via the version-chain `version::fold` — the tombstone has no chain) and lists
    /// each authoring npub, so a flood of forged NON-owner editions can never bury the real owner's signer
    /// out of the `MAX_CONTROL_EDITIONS` cap and truncate the true tombstone away. The CALLER treats the
    /// community as dissolved ONLY if the proven owner is in this set (mirrors `banlist_author`'s BAN
    /// check). A malformed edition at the locator is dropped (`skipped`), never honored.
    pub dissolved_by: Vec<PublicKey>,
    /// The banlist entity's current head, for the banlist path to advance `set_edition_head`.
    pub banlist_head: Option<EntityHead>,
    /// Folded per-creator invite-link sets (vsk=8, one per creator at `invite_links_locator(cid,
    /// creator)`). Each holds that creator's active link locators + head. The caller authorizes each
    /// `creator` (held `CREATE_INVITE`) and UNIONS the locators into the aggregate active-set — the 
    /// source of truth for the Public/Private mode + registry-authoritative joins. No shared registry.
    pub invite_link_sets: Vec<InviteLinkSet>,
    /// The folded GroupRoot (community metadata, vsk=0 at `entity_id == community_id`). `None` if no
    /// GroupRoot edition folded. Applied only once the caller checks `root_author` held `MANAGE_METADATA`.
    pub root_meta: Option<super::metadata::CommunityMetadata>,
    /// The signer of the folded GroupRoot head, so the caller can verify they held `MANAGE_METADATA`.
    pub root_author: Option<PublicKey>,
    /// The GroupRoot entity's current head, for the metadata path to advance `set_edition_head`.
    pub root_head: Option<EntityHead>,
    /// Every gap-vetted GroupRoot candidate at-or-above the floor (fork members included), highest
    /// version first (deterministic inner-id tiebreak within a version). The consumer applies the
    /// highest whose author is CURRENTLY authorized (`MANAGE_METADATA`) — an author-aware descending
    /// scan (B1b), so a demoted author's edition (incl. a same-version forgery) can't be the head,
    /// and a fresh fold converges to the highest authorized edition (the owner's re-assert). `root_head`
    /// is `root_candidates[0]` (the author-blind head) for back-compat.
    pub root_candidates: Vec<RootCandidate>,
    /// Folded per-channel metadata (vsk=2, one per channel `entity_id == channel_id`) — the author-blind
    /// top head per channel (back-compat: used by the demoted-author re-assert path). For convergence the
    /// consumer scans [`Self::channel_candidates`] instead.
    pub channel_meta: Vec<ChannelMetaHead>,
    /// Every gap-vetted channel-metadata candidate at-or-above each channel's floor (fork members
    /// included), highest version first with the deterministic inner-id tiebreak — the per-channel mirror
    /// of [`Self::root_candidates`]. Grouped contiguously per channel (sorted within each channel). The
    /// consumer applies, per channel, the highest whose author CURRENTLY holds `MANAGE_CHANNELS` — an
    /// author-aware scan, so a demoted author's same-version forgery can't orphan an authorized re-assert
    /// and a concurrent same-version rename converges to one deterministic winner on every client.
    pub channel_candidates: Vec<ChannelMetaHead>,
}

/// A folded per-creator InviteLinks edition (vsk=8): the creator who signed it, their active link
/// locators, and the version head. Applied only if `creator` held `CREATE_INVITE`.
#[derive(Clone)]
pub struct InviteLinkSet {
    pub creator: PublicKey,
    pub locators: Vec<String>,
    pub head: EntityHead,
}

/// One gap-vetted GroupRoot candidate (vsk=0): its content, signer (for the authority gate), and head.
/// The consumer scans `FoldedRoster::root_candidates` (highest version first) for the highest whose
/// author currently holds `MANAGE_METADATA`.
#[derive(Clone)]
pub struct RootCandidate {
    pub meta: super::metadata::CommunityMetadata,
    pub author: PublicKey,
    pub head: EntityHead,
}

/// A folded ChannelMetadata edition (vsk=2): the channel it addresses, its content, its signer (for the
/// `MANAGE_METADATA` authority gate), and its version head (to advance `set_edition_head`).
#[derive(Clone)]
pub struct ChannelMetaHead {
    pub channel_id: [u8; 32],
    pub meta: super::metadata::ChannelMetadata,
    pub author: PublicKey,
    pub head: EntityHead,
}

/// Fold a set of (already-decrypted) inner edition events into the current roster.
///
/// Each edition's inner Schnorr signature is verified (the authorship proof); editions are grouped
/// per entity and version-folded to a head (from scratch, floor 0). A head is folded into the trusted
/// roster ONLY if it is **chain-anchored** (not gapped — fail closed on withheld history) AND
/// its `entity_id` **binds** to its content (a Role lives at `entity_id == role_id`, a Grant at
/// `entity_id == grant_locator(community_id, member)`) — so a signed edition can't relabel
/// itself to a different role/member. Anything else is dropped and counted in `skipped`. The grant
/// binding is keyed by the **community id** (stable across a base rotation), so a re-anchored grant
/// folds under any epoch's root — the keystone for re-anchoring.
///
/// `floors` is each entity's persisted head (`entity_hex → (version, self_hash)`, from
/// [`crate::db::community::get_all_edition_heads`]) — the refuse-downgrade FLOOR. Each entity's
/// chain is folded from ITS held floor, not from scratch, so a withholding relay serving editions below
/// what we already hold can't roll an authority chain back (the attack: resurrecting a since-revoked
/// admin's old grant by withholding the revocation). An entity absent from `floors` (a bootstrapping
/// joiner) folds from genesis (floor 0); an empty map = a fresh joiner. A relay that serves only
/// below-floor editions for an entity yields no head for it → that entity is simply absent from this
/// fold (fail closed: it is not re-authorized off a rolled-back view; it self-heals when a relay serves
/// ≥ floor).
///
/// This does NOT apply delegation-chain authorization — the signature proves WHO; deciding WHETHER
/// they were allowed (rank + chain to the owner) is a separate, later layer.
pub fn fold_roster(
    inner_editions: &[Event],
    community_id: &CommunityId,
    floors: &HashMap<String, (u64, [u8; 32])>,
) -> FoldedRoster {
    let mut skipped = inner_editions.len().saturating_sub(MAX_CONTROL_EDITIONS);

    // Verify + parse; drop (and count) anything that doesn't. The cap bounds verify work.
    let parsed: Vec<edition::ParsedEdition> = inner_editions
        .iter()
        .take(MAX_CONTROL_EDITIONS)
        .filter_map(|e| match edition::parse_edition_inner(e) {
            Ok(p) => Some(p),
            Err(_) => {
                skipped += 1;
                None
            }
        })
        .collect();

    // tombstone detection — owner-signature-filtered, NOT position/version dependent. The dissolved
    // tombstone has no version chain, so it does NOT route through the per-entity `version::fold` below;
    // instead scan the locator directly and collect EVERY well-formed signer (the caller keeps only the
    // proven owner). Listing all signers means a flood of forged non-owner editions can't bury the owner's.
    // A vsk=10 edition at the WRONG entity_id (relabel attempt) is ignored here and dropped/skipped below.
    let dissolved_eid = super::derive::dissolved_locator(community_id);
    let mut dissolved_by: Vec<PublicKey> = Vec::new();
    for p in &parsed {
        if p.vsk == VSK_DISSOLVED && p.entity_id == dissolved_eid && !dissolved_by.contains(&p.author) {
            dissolved_by.push(p.author);
        }
    }

    let mut by_entity: HashMap<[u8; 32], Vec<&edition::ParsedEdition>> = HashMap::new();
    for p in &parsed {
        // vsk=10 is chain-free and handled by the scan above; keep it out of the version-chain fold (a
        // no-prev v1 would otherwise just no-op, but excluding it makes the exemption explicit).
        if p.vsk == VSK_DISSOLVED {
            continue;
        }
        by_entity.entry(p.entity_id).or_default().push(p);
    }

    let mut out = roles::CommunityRoles::default();
    let mut role_authors: Vec<PublicKey> = Vec::new();
    let mut grant_authors: Vec<PublicKey> = Vec::new();
    let mut gapped_entities = Vec::new();
    let mut heads: Vec<EntityHead> = Vec::new();
    let mut banned: Vec<String> = Vec::new();
    let mut banlist_author: Option<PublicKey> = None;
    let mut banlist_head: Option<EntityHead> = None;
    let banlist_eid = super::derive::banlist_locator(community_id);
    let mut invite_link_sets: Vec<InviteLinkSet> = Vec::new();
    let mut root_meta: Option<super::metadata::CommunityMetadata> = None;
    let mut root_author: Option<PublicKey> = None;
    let mut root_head: Option<EntityHead> = None;
    let mut root_candidates: Vec<RootCandidate> = Vec::new();
    let mut channel_meta: Vec<ChannelMetaHead> = Vec::new();
    let mut channel_candidates: Vec<ChannelMetaHead> = Vec::new();

    for (entity_id, editions) in by_entity {
        let fold_eds: Vec<version::Edition> = editions.iter().map(|p| p.to_fold_edition()).collect();
        // Seed the fold from this entity's PERSISTED head (refuse-downgrade floor), not from
        // scratch — so a relay serving editions below what we hold can't roll the chain back.
        let entity_hex = crate::simd::hex::bytes_to_hex_32(&entity_id);
        let floor = floors.get(&entity_hex);
        let floor_v = floor.map(|(v, _)| *v).unwrap_or(0);
        let result = version::fold(&fold_eds, floor_v, floor.map(|(_, h)| h));
        // head selection by client mode. No gap → the chain-anchored head. Gap → the chain isn't
        // anchored to genesis/floor; two modes:
        //   - TRACKING (floor > 0): fail CLOSED — an unanchored tail can be a rollback/fork. Keep the
        //     held floor; the entity is flagged pending for a re-fetch from the union.
        // - BOOTSTRAPPING (floor == 0): a fresh joiner whose genesis was re-anchored away cannot
        //     verify lineage, so accept the HIGHEST signed head (Policy B) and let AUTHORITY be the gate —
        //     each VSK arm below re-checks the head's coordinate, and the caller runs is_authorized /
        //     authorize_delegation. The head's signature is already verified (parse_edition_inner), so a
        //     relay can't forge a higher version, only withhold/replay older valid ones — covered by the
        //     relay union (Vector ships multiple trusted relays) + the refuse-downgrade floor.
        let head_idx = if result.gap {
            gapped_entities.push(entity_id);
            if floor_v == 0 {
                match version::bootstrap_head(&fold_eds, 0) { Some(i) => i, None => continue }
            } else {
                // Tracking + gap. Authority records (roles/grants/banlist) FAIL CLOSED — an unanchored
                // tail can be a rollback/fork, and converging authority off a withheld view is a
                // relay-choosable censorship lever. DISPLAY metadata (GroupRoot, channel) is exempt:
                // it carries no authority, the consumer's `is_authorized` filter is the real gate, and
                // the consumer's refuse-downgrade floor still blocks any sub-floor rollback. So surface
                // the ≥floor candidates (the per-version winner at the highest version) and let the
                // consumer author-gate + converge a same-version fork. A withheld-history gap here at
                // worst forward-jumps the displayed name to a validly-signed authorized edit.
                match version::bootstrap_head(&fold_eds, floor_v) {
                    Some(i) if matches!(editions[i].vsk.as_str(), VSK_COMMUNITY_ROOT | VSK_CHANNEL) => i,
                    _ => continue, // not display metadata → fail closed
                }
            }
        } else {
            match result.head { Some(i) => i, None => continue }
        };
        let head = editions[head_idx];
        let mut record_head = || {
            heads.push(EntityHead {
                entity_hex: crate::simd::hex::bytes_to_hex_32(&entity_id),
                version: head.version,
                self_hash: head.self_hash,
                inner_id: head.inner_id,
                citation: head.authority.clone(),
            });
        };
        match head.vsk.as_str() {
            VSK_ROLE => match serde_json::from_str::<roles::Role>(&head.content) {
                // The edition coordinate IS the role id (d-tag = role_id), so the content can't
                // claim to be a different (e.g. higher-powered) role than the entity it lives at.
                Ok(role) if hex32(&role.role_id) == Some(entity_id) => {
                    record_head();
                    role_authors.push(head.author);
                    out.roles.push(role);
                }
                _ => skipped += 1,
            },
            VSK_GRANT => match serde_json::from_str::<roles::MemberGrant>(&head.content) {
                // The grant's coordinate IS its member's opaque locator, so the content
                // can't relabel the grant to a different member, and two entities can't claim one
                // member (they'd share the locator → group + fold, not duplicate).
                Ok(grant)
                    if hex32(&grant.member)
                        .is_some_and(|m| super::derive::grant_locator(community_id, &m) == entity_id) =>
                {
                    // Record the head even for an empty grant (a revoke is a real chain advance); the
                    // empty grant just folds to "no roster entry" — don't carry a husk.
                    record_head();
                    if !grant.role_ids.is_empty() {
                        grant_authors.push(head.author);
                        out.grants.push(grant);
                    }
                }
                _ => skipped += 1,
            },
            VSK_BANLIST if entity_id == banlist_eid => match serde_json::from_str::<Vec<String>>(&head.content) {
                // The banlist lives at the single community-wide locator; its content is the banned set.
                // Only meaningful once the caller checks `banlist_author` held BAN (the authority gate).
                Ok(list) => {
                    banned = list;
                    banlist_author = Some(head.author);
                    banlist_head = Some(EntityHead {
                        entity_hex: crate::simd::hex::bytes_to_hex_32(&entity_id),
                        version: head.version,
                        self_hash: head.self_hash,
                        inner_id: head.inner_id,
                        citation: head.authority.clone(),
                    });
                }
                _ => skipped += 1,
            },
            VSK_INVITE_LINKS if super::derive::invite_links_locator(community_id, &head.author.to_bytes()) == entity_id => {
                // A creator's OWN link list, bound to its author's coordinate (so one creator can't
                // publish links under another's identity). Content is that creator's active locators.
                // Meaningful once the caller checks `creator` held CREATE_INVITE (the authority gate),
                // then UNIONS authorized creators' locators into the aggregate active-set.
                match serde_json::from_str::<Vec<String>>(&head.content) {
                    Ok(locators) => invite_link_sets.push(InviteLinkSet {
                        creator: head.author,
                        locators,
                        head: EntityHead {
                            entity_hex: crate::simd::hex::bytes_to_hex_32(&entity_id),
                            version: head.version,
                            self_hash: head.self_hash,
                            inner_id: head.inner_id,
                            citation: head.authority.clone(),
                        },
                    }),
                    _ => skipped += 1,
                }
            }
            VSK_COMMUNITY_ROOT if entity_id == community_id.0 => {
                // GroupRoot lives at the community's own coordinate; its content is the community
                // descriptor. Expose ALL gap-vetted candidates at-or-above the floor (fork members
                // included — B1b), highest version first with the deterministic inner-id tiebreak,
                // so the consumer can author-aware-scan: a same-version forgery can't orphan an
                // authorized re-assert via the author-blind tiebreak. Contiguity stays author-blind
                // here; the caller checks `MANAGE_METADATA` per candidate.
                let entity_hex = crate::simd::hex::bytes_to_hex_32(&entity_id);
                let mut cands: Vec<(u64, [u8; 32], RootCandidate)> = editions
                    .iter()
                    .filter(|e| e.version >= floor_v)
                    .filter_map(|e| {
                        serde_json::from_str::<super::metadata::CommunityMetadata>(&e.content)
                            .ok()
                            .map(|meta| (e.version, e.inner_id, RootCandidate {
                                meta,
                                author: e.author,
                                head: EntityHead {
                                    entity_hex: entity_hex.clone(),
                                    version: e.version,
                                    self_hash: e.self_hash,
                                    inner_id: e.inner_id,
                                    citation: e.authority.clone(),
                                },
                            }))
                    })
                    .collect();
                if cands.is_empty() {
                    skipped += 1;
                } else {
                    cands.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
                    root_candidates = cands.into_iter().map(|(_, _, c)| c).collect();
                    let top = &root_candidates[0];
                    root_meta = Some(top.meta.clone());
                    root_author = Some(top.author);
                    root_head = Some(top.head.clone());
                }
            }
            VSK_CHANNEL => {
                // ChannelMetadata lives at the channel's own coordinate (`entity_id == channel_id`), so its
                // content can't relabel itself to a different channel. Mirror GroupRoot: expose ALL gap-
                // vetted candidates at-or-above the floor (fork members included), highest version first with
                // the inner-id tiebreak, so the consumer author-aware-scans + converges a same-version fork.
                // Contiguity stays author-blind here; the caller checks MANAGE_CHANNELS per candidate.
                let entity_hex = crate::simd::hex::bytes_to_hex_32(&entity_id);
                let mut cands: Vec<(u64, [u8; 32], ChannelMetaHead)> = editions
                    .iter()
                    .filter(|e| e.version >= floor_v)
                    .filter_map(|e| {
                        serde_json::from_str::<super::metadata::ChannelMetadata>(&e.content)
                            .ok()
                            .map(|meta| (e.version, e.inner_id, ChannelMetaHead {
                                channel_id: entity_id,
                                meta,
                                author: e.author,
                                head: EntityHead {
                                    entity_hex: entity_hex.clone(),
                                    version: e.version,
                                    self_hash: e.self_hash,
                                    inner_id: e.inner_id,
                                    citation: e.authority.clone(),
                                },
                            }))
                    })
                    .collect();
                if cands.is_empty() {
                    skipped += 1;
                } else {
                    cands.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
                    // Author-blind top stays in channel_meta (back-compat); the full list drives convergence.
                    channel_meta.push(cands[0].2.clone());
                    channel_candidates.extend(cands.into_iter().map(|(_, _, c)| c));
                }
            }
            _ => {} // other entity types (role-order/...) handled elsewhere
        }
    }

    FoldedRoster {
        roles: out, role_authors, grant_authors, gapped_entities, skipped, fetched: 0, heads,
        banned, banlist_author, banlist_head, dissolved_by,
        invite_link_sets,
        root_meta, root_author, root_head, root_candidates, channel_meta, channel_candidates,
    }
}

/// Filter a [`fold_roster`] result by the **delegation chain** → the AUTHORIZED roster. Binding
/// + a valid signature prove an edition was *well-formed and authentic*, NOT that its signer was
/// *allowed* to make it — without this layer a member could self-sign an Admin grant and have every
/// peer fold it into their roster. Here an entry is trusted only if its signer was authorized:
///
/// - a **role** at position `P` is kept only if its signer can act on `P` with `MANAGE_ROLES` (strictly
///   outranks `P`; the owner — position 0 via the attestation — is supreme);
/// - a **grant** of roles `[R..]` to member `M` is kept only if its signer outranks every granted role's
///   position AND outranks `M`, with `MANAGE_ROLES`.
///
/// Authority is resolved against the roster built SO FAR, so this is a fixpoint seeded by the owner:
/// owner-signed entries are accepted first (they add admins), then admin-signed entries those admins are
/// allowed to make, until stable. An entry whose signer never becomes authorized is dropped — that is
/// the self-promotion / forged-delegation defense. `owner_hex == None` (unproven community, no root)
/// yields an EMPTY roster (fail closed: no anchor, no authority).
///
/// Authority is evaluated against the CURRENT folded roster. With [`fold_roster`] now seeding each
/// entity from its persisted refuse-downgrade FLOOR, that "current" roster is rollback-protected:
/// a relay can't resurrect a since-revoked admin's grant to re-validate the chain. So the 
/// version-pinned guarantee for the DELEGATION plane falls out here without a separate citation check —
/// a demoted signer drops out of the accepted roster, and every edition they signed drops with them
/// (refuse-superseded). The `vac` citation is the mechanism for the ACTION plane (ban/hide), where the
/// actor is NOT the entity being folded; on the delegation plane the signer's authorizing grant IS a
/// folded entity, so the floor-fold + this fixpoint already pin it. Grant editions still CARRY a `vac`
/// (emitted by `set_member_grant`) for the audit log + forward-compat, but this consumer deliberately
/// does not read it — the rank-chain over the floor-protected roster is the authority decision. The one behavior NOT provided is
/// point-in-time PRESERVATION (keeping grants a since-demoted admin made while authorized) — that is the
/// less-safe direction, needs a roster-wide snapshot version, and is deferred; the current fail-safe
/// drop is correct and safer.
pub fn authorize_delegation(folded: &FoldedRoster, owner_hex: Option<&str>) -> roles::CommunityRoles {
    use roles::Permissions;
    let mut accepted = roles::CommunityRoles::default();
    let mut role_done = vec![false; folded.roles.roles.len()];
    let mut grant_done = vec![false; folded.roles.grants.len()];

    loop {
        let mut changed = false;

        // A role is authorized if its signer can define a role at that position (outrank + MANAGE_ROLES).
        for (i, role) in folded.roles.roles.iter().enumerate() {
            if role_done[i] {
                continue;
            }
            // Position 0 is reserved to the owner-attestation chain — no RoleMetadata may occupy
            // it, even owner-signed (the owner's authority is the attestation, not a pos-0 role). Reject.
            if role.position == 0 {
                role_done[i] = true;
                continue;
            }
            let author = folded.role_authors[i].to_hex();
            if accepted.can_act_on_position(&author, owner_hex, role.position, Permissions::MANAGE_ROLES) {
                accepted.roles.push(role.clone());
                role_done[i] = true;
                changed = true;
            }
        }

        // A grant is authorized if its signer outranks every granted role's position AND the member,
        // with MANAGE_ROLES. Granted-role positions are resolved from the ALREADY-accepted roles, so a
        // grant referencing a not-yet-accepted role defers to a later round (or is dropped if its role
        // never authorizes). This is the escalation defense: you can't grant a role you don't outrank.
        for (i, grant) in folded.roles.grants.iter().enumerate() {
            if grant_done[i] {
                continue;
            }
            let positions: Option<Vec<u32>> =
                grant.role_ids.iter().map(|rid| accepted.role(rid).map(|r| r.position)).collect();
            let Some(positions) = positions else { continue }; // a granted role not yet accepted → defer
            let author = folded.grant_authors[i].to_hex();
            let outranks_all_roles = positions
                .iter()
                .all(|p| accepted.can_act_on_position(&author, owner_hex, *p, Permissions::MANAGE_ROLES));
            if outranks_all_roles
                && accepted.can_act_on_member(&author, owner_hex, &grant.member, Permissions::MANAGE_ROLES)
            {
                accepted.grants.push(grant.clone());
                grant_done[i] = true;
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }
    accepted
}

/// version-pinned authority **completeness** check for an action that carries an
/// [`edition::AuthorityCitation`] (a ban, a hide, a delegated grant). The action names the authorizing
/// grant edition it claims authority under; this confirms we have folded a COMPLETE, un-forked view of
/// that grant — synced to AT LEAST the cited version, with the cited hash matching ours at equality.
///
/// It does NOT decide the permission/outrank — that stays with the caller's `can_act_on_member` against
/// the CURRENT authorized roster, so a since-demoted actor is refused there (refuse-superseded falls out:
/// we hold a later head of their grant that dropped the role). This check is the orthogonal half: it
/// guarantees the verdict is computed over the same authority view the actor cited, not a stale or
/// partial one.
///
/// `heads` is the fold's per-entity head set ([`FoldedRoster::heads`]) — the haystack the cited grant
/// must appear in. `actor_grant_hex` is the entity coordinate of the ACTOR's OWN authorizing grant
/// (`grant_locator(community_id, actor)`, lowercase hex): the citation MUST name it, so an actor can't
/// borrow completeness by citing some other synced edition. Returns true iff:
///   - the actor is the proven owner (supreme — cites nothing), OR
///   - the citation names the actor's own grant AND we surfaced it at version ≥ the cited version, and —
///     when our head is EXACTLY the cited version — the cited hash equals ours (a cited fork is rejected).
///
/// A non-owner who cited nothing, cited a foreign entity, or whose cited grant we have NOT folded up to
/// the cited version (a withholding relay, or we are simply behind), returns false: FAIL CLOSED — never
/// act on an incomplete authority view (the §"never act on a partial view" tenet). The block-until-synced
/// re-fetch escalation across a relay quorum is deferred (one signature suffices, per the MVP directive);
/// MVP fails closed here and self-heals on the next sync once the cited grant arrives.
pub fn authority_citation_satisfied(
    heads: &[EntityHead],
    owner_hex: Option<&str>,
    actor_hex: &str,
    actor_grant_hex: &str,
    citation: Option<&edition::AuthorityCitation>,
) -> bool {
    if owner_hex == Some(actor_hex) {
        return true;
    }
    let Some(c) = citation else { return false };
    let entity_hex = crate::simd::hex::bytes_to_hex_32(&c.entity_id);
    // The citation must name the ACTOR's OWN authorizing grant — citing a foreign synced edition can't
    // borrow completeness (the permission check keys on the actor, but pinning to their grant keeps the
    // sync-floor honest and is the coordinate the delegation verifier will resolve rank at).
    if entity_hex != actor_grant_hex {
        return false;
    }
    match heads.iter().find(|h| h.entity_hex == entity_hex) {
        // We hold a LATER edition of the actor's grant than they cited — synced past it. Whether the
        // actor is STILL authorized is the caller's roster check (which reflects this later head).
        Some(h) if h.version > c.version => true,
        // Synced to exactly the cited version: the cited hash must be the one that won our fold (else
        // the actor cited a non-canonical fork of their own grant).
        Some(h) if h.version == c.version => h.self_hash == c.edition_hash,
        // Not surfaced, or our head is BEHIND the cited version → we cannot confirm the authority.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::community::roles::{Permissions, Role, RoleScope};

    fn sr() -> ServerRootKey {
        ServerRootKey([0x07; 32])
    }

    #[test]
    fn authority_citation_satisfied_pins_to_a_synced_complete_grant() {
        let owner = "ow".repeat(32);
        let actor = "ac".repeat(32);
        let entity = [0x55u8; 32];
        let hash = [0x66u8; 32];
        let eh = crate::simd::hex::bytes_to_hex_32(&entity); // the actor's own grant locator
        let head = |v: u64, h: [u8; 32]| {
            vec![EntityHead { entity_hex: eh.clone(), version: v, self_hash: h, inner_id: [0u8; 32], citation: None }]
        };
        let cite = edition::AuthorityCitation { entity_id: entity, version: 3, edition_hash: hash };

        // Owner is supreme and cites nothing.
        assert!(authority_citation_satisfied(&[], Some(&owner), &owner, &eh, None));
        // A non-owner with no citation fails closed.
        assert!(!authority_citation_satisfied(&head(3, hash), Some(&owner), &actor, &eh, None));
        // Synced to exactly the cited version with the matching hash → satisfied.
        assert!(authority_citation_satisfied(&head(3, hash), Some(&owner), &actor, &eh, Some(&cite)));
        // Same version, wrong hash (a cited fork at the tip) → rejected.
        assert!(!authority_citation_satisfied(&head(3, [0xEE; 32]), Some(&owner), &actor, &eh, Some(&cite)));
        // We hold a LATER head of the actor's grant → synced past it (the roster check handles supersession).
        assert!(authority_citation_satisfied(&head(4, [0x77; 32]), Some(&owner), &actor, &eh, Some(&cite)));
        // We are BEHIND the cited version → fail closed.
        assert!(!authority_citation_satisfied(&head(2, hash), Some(&owner), &actor, &eh, Some(&cite)));
        // The cited grant isn't surfaced at all → fail closed.
        assert!(!authority_citation_satisfied(&[], Some(&owner), &actor, &eh, Some(&cite)));
        // The citation names a FOREIGN entity (not the actor's grant) → rejected even if surfaced.
        let foreign = "ff".repeat(32);
        assert!(!authority_citation_satisfied(&head(3, hash), Some(&owner), &actor, &foreign, Some(&cite)));
    }
    // The community id binds grant coordinates (stable across rotation); fold + grant builders take it.
    fn cid() -> CommunityId {
        CommunityId([0x09; 32])
    }

    /// A role edition at its bound coordinate (entity_id == role_id).
    fn role_event(owner: &Keys, role_id: &str, position: u32, version: u64, prev: Option<&[u8; 32]>, created_at: u64) -> Event {
        let role = Role {
            role_id: role_id.to_string(),
            name: "Admin".into(),
            position,
            permissions: Permissions::admin(),
            scope: RoleScope::Server,
            color: 0,
        };
        let eid = crate::simd::hex::hex_to_bytes_32(role_id);
        edition::build_edition_inner(owner.public_key(), VSK_ROLE, &eid, version, prev, &serde_json::to_string(&role).unwrap(), created_at, None)
            .sign_with_keys(owner)
            .unwrap()
    }

    /// A grant edition at its bound coordinate (entity_id == grant_locator(community_id, member)).
    fn grant_event(owner: &Keys, member_hex: &str, role_ids: Vec<String>, version: u64, prev: Option<&[u8; 32]>, created_at: u64) -> (Event, [u8; 32], String) {
        let member_bytes = crate::simd::hex::hex_to_bytes_32(member_hex);
        let eid = crate::community::derive::grant_locator(&cid(), &member_bytes);
        let g = roles::MemberGrant { member: member_hex.to_string(), role_ids };
        let content = serde_json::to_string(&g).unwrap();
        let ev = edition::build_edition_inner(owner.public_key(), VSK_GRANT, &eid, version, prev, &content, created_at, None)
            .sign_with_keys(owner)
            .unwrap();
        (ev, eid, content)
    }

    /// A GroupRoot (community metadata) edition at the community's own coordinate (entity_id == community_id).
    fn groot_event(signer: &Keys, name: &str, desc: &str, version: u64, prev: Option<&[u8; 32]>, created_at: u64) -> Event {
        let meta = crate::community::metadata::CommunityMetadata {
            name: name.to_string(),
            relays: vec![],
            description: Some(desc.to_string()),
            icon: None,
            banner: None,
            owner_attestation: None,
        };
        let content = serde_json::to_string(&meta).unwrap();
        edition::build_edition_inner(signer.public_key(), VSK_COMMUNITY_ROOT, &cid().0, version, prev, &content, created_at, None)
            .sign_with_keys(signer)
            .unwrap()
    }

    #[test]
    fn folds_a_role_and_a_grant() {
        let owner = Keys::generate();
        let role_id = "a".repeat(64);
        let member = "bb".repeat(32);
        let role_ev = role_event(&owner, &role_id, 1, 1, None, 100);
        let (grant_ev, _, _) = grant_event(&owner, &member, vec![role_id.clone()], 1, None, 101);
        let folded = fold_roster(&[role_ev, grant_ev], &cid(), &Default::default());
        assert_eq!(folded.roles.roles.len(), 1);
        assert_eq!(folded.roles.roles[0].role_id, role_id);
        assert!(folded.roles.is_admin(&member), "the member holds the granted Admin role");
        assert_eq!(folded.skipped, 0);
        assert!(folded.gapped_entities.is_empty());
    }

    #[test]
    fn latest_grant_edition_wins() {
        let owner = Keys::generate();
        let role_a = "a".repeat(64);
        let role_b = "b".repeat(64);
        let member = "cc".repeat(32);
        let (e1, eid, c1) = grant_event(&owner, &member, vec![role_a.clone()], 1, None, 100);
        let v1_hash = version::edition_hash(&eid, 1, None, c1.as_bytes());
        let (e2, _, _) = grant_event(&owner, &member, vec![role_b.clone()], 2, Some(&v1_hash), 101);
        let folded = fold_roster(&[e1, e2], &cid(), &Default::default());
        let held: Vec<&String> = folded.roles.grants.iter().flat_map(|g| &g.role_ids).collect();
        assert_eq!(held, vec![&role_b], "v2 supersedes v1 — the member now holds role_b, not role_a");
        assert_eq!(folded.skipped, 0);
    }

    /// TRACKING (floor > 0): a lone forged high-version edition with withheld history is QUARANTINED —
    /// flagged gapped AND kept out of the trusted roster. This is the fail-closed defense, and it
    /// stays fully intact for any client that already holds a floor (rollback/fork protection).
    #[test]
    fn tracking_quarantines_a_gapped_head() {
        let owner = Keys::generate();
        let role_id = "e".repeat(64);
        let eid = crate::simd::hex::hex_to_bytes_32(&role_id);
        let ev = role_event(&owner, &role_id, 1, 5, Some(&[0x99u8; 32]), 100);
        // We already hold this entity at v3, so a lone v5 with a non-linking prev is an unanchored tail.
        let mut floors = std::collections::HashMap::new();
        floors.insert(role_id.clone(), (3u64, [0x11u8; 32]));
        let folded = fold_roster(&[ev], &cid(), &floors);
        assert!(folded.roles.roles.is_empty(), "tracking: a gapped tail is quarantined, not folded");
        assert_eq!(folded.gapped_entities, vec![eid]);
    }

    /// BOOTSTRAPPING (floor == 0, Policy B): a fresh joiner whose genesis was re-anchored away SURFACES
    /// the highest signed head despite the gap — but AUTHORITY still gates it. An owner-authored head is
    /// authorized; an unauthorized author's (equally surfaced) head is dropped by `authorize_delegation`.
    /// This is the narrow, deliberate relaxation of the fail-closed rule at first contact only.
    #[test]
    fn bootstrapping_surfaces_a_gapped_head_but_authority_gates() {
        let owner = Keys::generate();
        let owner_hex = owner.public_key().to_hex();
        let role_id = "e".repeat(64);
        let eid = crate::simd::hex::hex_to_bytes_32(&role_id);
        // Owner-authored position-1 role at v5 with no anchor (genesis re-anchored away) → gapped @ floor 0.
        let ev = role_event(&owner, &role_id, 1, 5, Some(&[0x99u8; 32]), 100);
        let folded = fold_roster(&[ev], &cid(), &Default::default());
        assert_eq!(folded.roles.roles.len(), 1, "bootstrapping surfaces the highest signed head despite the gap");
        assert_eq!(folded.gapped_entities, vec![eid], "still flagged pending so the union can fill the gap");
        assert_eq!(authorize_delegation(&folded, Some(&owner_hex)).roles.len(), 1, "owner-authored head is authorized");

        // Same shape, authored by a STRANGER → still surfaced into the raw fold, but authority drops it.
        let stranger = Keys::generate();
        let ev2 = role_event(&stranger, &role_id, 1, 5, Some(&[0x99u8; 32]), 100);
        let folded2 = fold_roster(&[ev2], &cid(), &Default::default());
        assert_eq!(folded2.roles.roles.len(), 1, "surfaced into the raw fold (signature is valid)");
        assert!(authorize_delegation(&folded2, Some(&owner_hex)).roles.is_empty(),
            "Policy B does NOT bypass authority — an unauthorized author's surfaced head is rejected");
    }

    /// Fresh-joiner-after-rotation: a bootstrapping joiner (floor 0) recovers the FULL plane across version
    /// holes. It surfaces the latest admin-authored GroupRoot AND the owner's gapped (no-v1) grant, and
    /// authority folds the admin in so that admin's metadata is authorized.
    #[test]
    fn fresh_joiner_recovers_full_plane_across_gaps() {
        let owner = Keys::generate();
        let owner_hex = owner.public_key().to_hex();
        let admin = Keys::generate();
        let admin_hex = admin.public_key().to_hex();
        let role_id = "a".repeat(64);

        // Admin role def — owner-signed, position 1, admin perms (incl MANAGE_METADATA), contiguous v1.
        let role = role_event(&owner, &role_id, 1, 1, None, 100);
        // Owner grants the admin that role — but the grant chain is GAPPED (starts at v2, no v1 re-anchored).
        let (grant, _, _) = grant_event(&owner, &admin_hex, vec![role_id.clone()], 2, Some(&[0x99u8; 32]), 110);
        // GroupRoot: owner genesis v1, then the admin's edit at v11 with a hole below it (no v2..v10).
        let groot_v1 = groot_event(&owner, "Genesis", "", 1, None, 90);
        let groot_v11 = groot_event(&admin, "AggroTown v2", "King Claude was here", 11, Some(&[0xABu8; 32]), 200);

        let folded = fold_roster(&[role, grant, groot_v1, groot_v11], &cid(), &Default::default());

        // Bootstrapping surfaced the LATEST GroupRoot (v11, admin-authored) across the gap, not the genesis.
        assert_eq!(folded.root_author.map(|a| a.to_hex()), Some(admin_hex.clone()), "latest GroupRoot author surfaced");
        assert_eq!(folded.root_meta.as_ref().unwrap().name, "AggroTown v2");
        assert_eq!(folded.root_meta.as_ref().unwrap().description.as_deref(), Some("King Claude was here"));

        // Authority folds the gapped grant → the admin holds MANAGE_METADATA, so the v11 edit IS authorized.
        let authed = authorize_delegation(&folded, Some(&owner_hex));
        assert!(
            authed.is_authorized(&admin_hex, Some(&owner_hex), roles::Permissions::MANAGE_METADATA),
            "the bootstrapped grant authorizes the admin → the fresh joiner trusts the admin's metadata"
        );
    }

    /// Forgery-resistance under Policy B: a bootstrapping joiner SURFACES even a stranger's lone high-version
    /// GroupRoot (its signature is valid), but AUTHORITY refuses it — `is_authorized` is false for the
    /// unauthorized author, so the caller never applies it. Policy B trusts signature + authority, never a
    /// bare version number. (A relay can't forge a higher version; the worst it does is surface a non-author.)
    #[test]
    fn unauthorized_high_version_metadata_is_surfaced_but_not_authorized() {
        let owner = Keys::generate();
        let owner_hex = owner.public_key().to_hex();
        let stranger = Keys::generate();
        let stranger_hex = stranger.public_key().to_hex();
        let groot_v1 = groot_event(&owner, "Genesis", "", 1, None, 90);
        let forged = groot_event(&stranger, "PWNED", "owned by a hostile relay", 99, Some(&[0xCDu8; 32]), 300);
        let folded = fold_roster(&[groot_v1, forged], &cid(), &Default::default());
        // Surfaced (highest signed head) — but authored by the stranger, who holds no authority.
        assert_eq!(folded.root_author.map(|a| a.to_hex()), Some(stranger_hex.clone()));
        let authed = authorize_delegation(&folded, Some(&owner_hex));
        assert!(
            !authed.is_authorized(&stranger_hex, Some(&owner_hex), roles::Permissions::MANAGE_METADATA),
            "an unauthorized author's surfaced metadata is NOT authorized — the caller drops it"
        );
    }

    /// Publish-time authority (B1b): on a FRESH fold (no floor), a demoted admin's editions — incl. a
    /// same-version FORGERY sharing the owner's re-assert version — are skipped by the author-aware scan,
    /// and the owner's re-assert wins. The candidate set exposes BOTH v3 fork members so the forgery can't
    /// orphan the re-assert via the author-blind tiebreak. (This is the gauntlet's convergence demand.)
    #[test]
    fn fresh_fold_picks_owner_reassert_over_a_demoted_admins_forgery_and_edit() {
        let owner = Keys::generate();
        let owner_hex = owner.public_key().to_hex();
        let alice = Keys::generate();
        let alice_hex = alice.public_key().to_hex();
        let role_id = "a".repeat(64);

        // Owner: admin role (MANAGE_METADATA); grant Alice; then REVOKE Alice (chained grant→revoke).
        let role = role_event(&owner, &role_id, 1, 1, None, 100);
        let (grant, geid, gcontent) = grant_event(&owner, &alice_hex, vec![role_id.clone()], 1, None, 110);
        let grant_hash = version::edition_hash(&geid, 1, None, gcontent.as_bytes());
        let (revoke, _, _) = grant_event(&owner, &alice_hex, vec![], 2, Some(&grant_hash), 120);

        // GroupRoot: owner genesis; Alice's round-1 (while admin); then a v3 FORK — Alice's forgery vs the
        // owner's re-assert of Alice's content (arbitrary prevs → bootstrap path = the fresh-joiner case).
        let groot_v1 = groot_event(&owner, "Genesis", "", 1, None, 90);
        let round1_v2 = groot_event(&alice, "Alice's HQ", "by admin alice", 2, Some(&[0x22u8; 32]), 200);
        let forgery_v3 = groot_event(&alice, "FORGERY", "demoted alice", 3, Some(&[0x33u8; 32]), 300);
        let reassert_v3 = groot_event(&owner, "Alice's HQ", "re-asserted by owner", 3, Some(&[0x33u8; 32]), 310);

        let folded = fold_roster(
            &[role, grant, revoke, groot_v1, round1_v2, forgery_v3, reassert_v3],
            &cid(), &Default::default(),
        );
        let authed = authorize_delegation(&folded, Some(&owner_hex));
        assert!(!authed.is_authorized(&alice_hex, Some(&owner_hex), Permissions::MANAGE_METADATA),
            "Alice is revoked (grant→revoke folds to no roles)");

        // Both v3 fork members are exposed (so the forgery can't orphan the re-assert).
        assert_eq!(folded.root_candidates.iter().filter(|c| c.head.version == 3).count(), 2,
            "the candidate set includes both v3 fork members");

        // The author-aware descending scan (what the consumer runs) picks the owner's re-assert; Alice's
        // forgery (v3) and round-1 (v2) are skipped because she's revoked.
        let chosen = folded.root_candidates.iter()
            .find(|c| authed.is_authorized(&c.author.to_hex(), Some(&owner_hex), Permissions::MANAGE_METADATA))
            .expect("an authorized candidate exists");
        assert_eq!(chosen.author, owner.public_key(), "the owner's re-assert wins the fork, not Alice's forgery");
        assert_eq!(chosen.meta.name, "Alice's HQ", "the demoted admin's content is preserved, not 'FORGERY'");
    }

    /// A stranger publishes a high-version GRANT making themselves admin. Bootstrapping surfaces it
    /// into the raw fold (its signature is valid), but `authorize_delegation` drops it — it never chains to
    /// the owner. The grant analogue of `bootstrapping_surfaces_a_gapped_head_but_authority_gates`.
    #[test]
    fn bootstrapping_surfaces_a_stranger_grant_but_it_never_authorizes() {
        let owner = Keys::generate();
        let owner_hex = owner.public_key().to_hex();
        let stranger = Keys::generate();
        let stranger_hex = stranger.public_key().to_hex();
        let role_id = "a".repeat(64);
        // Owner-defined Admin role (so the role itself is legit) + a stranger-signed grant of it to themselves.
        let role = role_event(&owner, &role_id, 1, 1, None, 100);
        let (grant, _, _) = grant_event(&stranger, &stranger_hex, vec![role_id.clone()], 99, Some(&[0x99u8; 32]), 200);
        let folded = fold_roster(&[role, grant], &cid(), &Default::default());
        assert_eq!(folded.roles.grants.len(), 1, "the stranger's grant is surfaced (valid signature)");
        let authed = authorize_delegation(&folded, Some(&owner_hex));
        assert!(authed.grants.is_empty(), "but it never chains to the owner → dropped");
        assert!(!authed.is_authorized(&stranger_hex, Some(&owner_hex), roles::Permissions::BAN),
            "the self-granting stranger holds no authority");
    }

    /// ONE fold, the full per-entity floor x per-VSK matrix. A tracking GroupRoot on a gapped tail
    /// is now SURFACED (display metadata carries no authority — the refuse-downgrade floor still blocks
    /// any sub-floor rollback, so a gap at worst forward-jumps the name to a validly-signed authorized
    /// edit); a tracking AUTHORITY record (grant) on a gapped tail stays QUARANTINED (converging authority
    /// off a withheld view is a censorship lever); a bootstrapping (floor-0) grant is surfaced. The
    /// per-entity decision is the crux of mixed-mode correctness.
    #[test]
    fn mixed_tracking_and_bootstrapping_floors_in_one_fold() {
        let owner = Keys::generate();
        let role_id = "a".repeat(64);
        let fresh_member = "cc".repeat(32);
        let tracked_member = "dd".repeat(32);
        // GroupRoot present only at v11 with a non-linking prev — we HOLD it at v5 (tracking, gapped).
        let groot_v11 = groot_event(&owner, "Tracked", "held-at-v5", 11, Some(&[0xABu8; 32]), 200);
        // A gapped grant for a member we've NEVER seen (floor 0 → bootstrapping → surfaced).
        let (fresh_grant, _, _) = grant_event(&owner, &fresh_member, vec![role_id.clone()], 2, Some(&[0x99u8; 32]), 110);
        // A gapped grant for a member we DO track at v5 (tracking authority → quarantined).
        let (tracked_grant, _, _) = grant_event(&owner, &tracked_member, vec![role_id.clone()], 11, Some(&[0x88u8; 32]), 120);
        let role = role_event(&owner, &role_id, 1, 1, None, 100); // contiguous, folds normally

        let cid_hex = crate::simd::hex::bytes_to_hex_32(&cid().0);
        let tracked_grant_hex = crate::simd::hex::bytes_to_hex_32(
            &crate::community::derive::grant_locator(&cid(), &crate::simd::hex::hex_to_bytes_32(&tracked_member)));
        let mut floors = std::collections::HashMap::new();
        floors.insert(cid_hex, (5u64, [0x77u8; 32]));           // GroupRoot held at v5 → tracking
        floors.insert(tracked_grant_hex, (5u64, [0x66u8; 32])); // tracked member's grant held at v5 → tracking
        let folded = fold_roster(&[groot_v11, fresh_grant, tracked_grant, role], &cid(), &floors);

        assert!(folded.root_meta.is_some(), "tracking GroupRoot with a gapped tail is SURFACED (display exemption)");
        assert!(folded.gapped_entities.contains(&cid().0), "and still flagged pending for a refetch");
        assert_eq!(folded.roles.grants.len(), 1, "only the bootstrapping grant surfaces; the tracked-gapped grant stays quarantined");
    }

    /// An empty grant (a revoke) advances the entity's head (a real chain step) but carries NO roster
    /// entry — the husk must not linger as a phantom grant.
    #[test]
    fn empty_grant_revoke_advances_head_but_carries_no_entry() {
        let owner = Keys::generate();
        let role_id = "a".repeat(64);
        let member = "cc".repeat(32);
        let (g1, eid, c1) = grant_event(&owner, &member, vec![role_id], 1, None, 100);
        let v1_hash = version::edition_hash(&eid, 1, None, c1.as_bytes());
        let (g2, _, _) = grant_event(&owner, &member, vec![], 2, Some(&v1_hash), 101); // revoke = empty grant
        let folded = fold_roster(&[g1, g2], &cid(), &Default::default());
        assert!(folded.roles.grants.is_empty(), "a revoke leaves no roster entry");
        assert!(
            folded.heads.iter().any(|h| h.entity_hex == crate::simd::hex::bytes_to_hex_32(&eid) && h.version == 2),
            "but the revoke still advances the head (so a replayed v1 can't re-add the role)"
        );
    }

    /// Junk-resilience: a hostile relay interleaves malformed editions with a valid one. `fold_roster`
    /// must SKIP the junk (counting it), still fold the valid role, and never panic.
    #[test]
    fn fold_roster_skips_junk_and_still_folds_the_valid() {
        let owner = Keys::generate();
        let owner_hex = owner.public_key().to_hex();
        let role_id = "a".repeat(64);
        let good = role_event(&owner, &role_id, 1, 1, None, 100);
        // Junk A: a role edition whose CONTENT is garbage JSON → parses, but the VSK_ROLE decode fails → skipped.
        let garbage = edition::build_edition_inner(
            owner.public_key(), VSK_ROLE, &crate::simd::hex::hex_to_bytes_32(&"b".repeat(64)),
            1, None, "not json at all", 100, None,
        ).sign_with_keys(&owner).unwrap();
        // Junk B: a role whose content claims a DIFFERENT role_id than its entity coordinate → skipped (binding).
        let role_b = Role { role_id: "cc".repeat(64), name: "X".into(), position: 1, permissions: Permissions::admin(), scope: RoleScope::Server, color: 0 };
        let mismatched = edition::build_edition_inner(
            owner.public_key(), VSK_ROLE, &crate::simd::hex::hex_to_bytes_32(&"dd".repeat(64)),
            1, None, &serde_json::to_string(&role_b).unwrap(), 100, None,
        ).sign_with_keys(&owner).unwrap();

        let folded = fold_roster(&[good, garbage, mismatched], &cid(), &Default::default());
        assert!(folded.roles.role(&role_id).is_some(), "the valid role still folds through the junk");
        assert!(folded.skipped >= 2, "both junk editions are skipped, not folded (skipped={})", folded.skipped);
        assert_eq!(authorize_delegation(&folded, Some(&owner_hex)).roles.len(), 1, "only the valid role authorizes");
    }

    /// A role edition whose content claims a role_id different from its entity coordinate is rejected
    /// — a signed edition can't relabel itself to a more powerful role.
    #[test]
    fn role_content_must_bind_to_its_entity_id() {
        let owner = Keys::generate();
        let role = Role {
            role_id: "a".repeat(64), // content claims a..a
            name: "X".into(),
            position: 0,
            permissions: Permissions::admin(),
            scope: RoleScope::Server,
            color: 0,
        };
        let wrong_eid = [0x12u8; 32]; // but the edition lives at a different coordinate
        let ev = edition::build_edition_inner(owner.public_key(), VSK_ROLE, &wrong_eid, 1, None, &serde_json::to_string(&role).unwrap(), 100, None)
            .sign_with_keys(&owner)
            .unwrap();
        let folded = fold_roster(&[ev], &cid(), &Default::default());
        assert!(folded.roles.roles.is_empty(), "entity_id != role_id → rejected");
        assert_eq!(folded.skipped, 1);
    }

    /// A grant for member M placed at an entity_id that isn't M's locator is rejected — closes the
    /// "forged second grant for M re-adds revoked roles" union vector (H3).
    #[test]
    fn grant_at_wrong_locator_is_rejected() {
        let owner = Keys::generate();
        let member = "dd".repeat(32);
        let g = roles::MemberGrant { member: member.clone(), role_ids: vec!["a".repeat(64)] };
        let wrong_eid = [0x34u8; 32]; // not grant_locator(cid, member)
        let ev = edition::build_edition_inner(owner.public_key(), VSK_GRANT, &wrong_eid, 1, None, &serde_json::to_string(&g).unwrap(), 100, None)
            .sign_with_keys(&owner)
            .unwrap();
        let folded = fold_roster(&[ev], &cid(), &Default::default());
        assert!(!folded.roles.is_admin(&member), "a grant at the wrong locator does not take effect");
        assert_eq!(folded.skipped, 1);
    }

    /// The fold is independent of input order (deterministic convergence across clients).
    #[test]
    fn fold_is_order_independent() {
        let owner = Keys::generate();
        let role_id = "a".repeat(64);
        let member = "bb".repeat(32);
        let role_ev = role_event(&owner, &role_id, 1, 1, None, 100);
        let (grant_ev, _, _) = grant_event(&owner, &member, vec![role_id.clone()], 1, None, 101);
        let a = fold_roster(&[role_ev.clone(), grant_ev.clone()], &cid(), &Default::default());
        let b = fold_roster(&[grant_ev, role_ev], &cid(), &Default::default());
        assert_eq!(a.roles.roles.len(), b.roles.roles.len());
        assert!(a.roles.is_admin(&member) && b.roles.is_admin(&member));
    }

    /// The PUBLIC send-side builders produce editions the consumer fold accepts cleanly — the
    /// producer↔consumer loop closes (bound coordinates, anchored genesis, valid signatures).
    #[test]
    fn public_builders_round_trip_through_fold() {
        let owner = Keys::generate();
        let role = Role {
            role_id: "a".repeat(64),
            name: "Admin".into(),
            position: 1,
            permissions: Permissions::admin(),
            scope: RoleScope::Server,
            color: 0,
        };
        let member = "bb".repeat(32);
        let grant = roles::MemberGrant { member: member.clone(), role_ids: vec![role.role_id.clone()] };

        let role_ev = build_role_edition(&owner, &role, 1, None, 100, None).unwrap();
        let grant_ev = build_grant_edition(&owner, &cid(), &grant, 1, None, 101, None).unwrap();

        let folded = fold_roster(&[role_ev, grant_ev], &cid(), &Default::default());
        assert_eq!(folded.skipped, 0, "builders emit bound, anchored editions the fold accepts");
        assert!(folded.gapped_entities.is_empty());
        assert!(folded.roles.is_admin(&member));
        assert_eq!(folded.roles.role(&role.role_id).unwrap().position, 1);
    }

    /// Mis-shaped chains fail LOUD at mint (W1) — not silently quarantined at fold.
    #[test]
    fn builders_reject_malformed_chain_shape() {
        let owner = Keys::generate();
        let role = Role {
            role_id: "a".repeat(64), name: "Admin".into(), position: 1,
            permissions: Permissions::admin(), scope: RoleScope::Server, color: 0,
        };
        // v1 with a prev_hash, and v>1 without one, are both rejected at build time.
        assert!(build_role_edition(&owner, &role, 1, Some(&[0u8; 32]), 100, None).is_err());
        assert!(build_role_edition(&owner, &role, 5, None, 100, None).is_err());
        let grant = roles::MemberGrant { member: "bb".repeat(32), role_ids: vec![role.role_id.clone()] };
        assert!(build_grant_edition(&owner, &cid(), &grant, 1, Some(&[0u8; 32]), 100, None).is_err());
        assert!(build_grant_edition(&owner, &cid(), &grant, 2, None, 100, None).is_err());
    }

    /// Bad role_id / member hex is an Err, never a panic.
    #[test]
    fn builders_reject_bad_hex() {
        let owner = Keys::generate();
        let bad_role = Role {
            role_id: "not-hex".into(), name: "X".into(), position: 1,
            permissions: Permissions::admin(), scope: RoleScope::Server, color: 0,
        };
        assert!(build_role_edition(&owner, &bad_role, 1, None, 100, None).is_err());
        let bad_grant = roles::MemberGrant { member: "zz".repeat(32), role_ids: vec!["a".repeat(64)] };
        assert!(build_grant_edition(&owner, &cid(), &bad_grant, 1, None, 100, None).is_err());
    }

    /// A genuine producer-built v1→v2 grant chain folds to v2 (mirrors the consumer test, but the
    /// editions come from the public builders — catches any drift between what they sign and what
    /// `edition_hash` expects as the next `prev_hash`).
    #[test]
    fn producer_built_chain_folds_to_latest() {
        let owner = Keys::generate();
        let member = "cc".repeat(32);
        let role_a = "a".repeat(64);
        let role_b = "b".repeat(64);

        let g1 = roles::MemberGrant { member: member.clone(), role_ids: vec![role_a] };
        let e1 = build_grant_edition(&owner, &cid(), &g1, 1, None, 100, None).unwrap();
        // The next edition cites v1's edition_hash over the SAME bytes the builder signed.
        let member_bytes = crate::simd::hex::hex_to_bytes_32(&member);
        let eid = crate::community::derive::grant_locator(&cid(), &member_bytes);
        let v1_hash = version::edition_hash(&eid, 1, None, serde_json::to_string(&g1).unwrap().as_bytes());

        let g2 = roles::MemberGrant { member: member.clone(), role_ids: vec![role_b.clone()] };
        let e2 = build_grant_edition(&owner, &cid(), &g2, 2, Some(&v1_hash), 101, None).unwrap();

        let folded = fold_roster(&[e1, e2], &cid(), &Default::default());
        assert_eq!(folded.skipped, 0);
        assert!(folded.gapped_entities.is_empty(), "v2 links to v1 — no gap");
        let held: Vec<&String> = folded.roles.grants.iter().flat_map(|g| &g.role_ids).collect();
        assert_eq!(held, vec![&role_b]);
    }

    /// The FULL control pipeline: build → seal under server-root → (wire) → open → parse → fold.
    #[test]
    fn control_edition_seals_opens_and_folds_end_to_end() {
        let owner = Keys::generate();
        let community_id = CommunityId([0x09; 32]);
        let epoch = Epoch(0);
        let role = Role {
            role_id: "a".repeat(64), name: "Admin".into(), position: 1,
            permissions: Permissions::admin(), scope: RoleScope::Server, color: 0,
        };

        let inner = build_role_edition(&owner, &role, 1, None, 100, None).unwrap();
        let outer = seal_control_edition(&Keys::generate(), &inner, &sr(), &community_id, epoch).unwrap();

        // The wire event hides the real author (ephemeral outer) and the content (encrypted).
        assert_ne!(outer.pubkey, owner.public_key(), "outer is ephemeral-signed");
        assert!(!outer.content.contains(&role.role_id), "inner content is encrypted on the wire");

        // A wrong server-root can't open it — also how cross-community replay is rejected.
        assert!(open_control_edition(&outer, &ServerRootKey([0xAA; 32])).is_err());

        // Members open → parse → fold: the role lands, real authorship preserved.
        let reopened = open_control_edition(&outer, &sr()).unwrap();
        assert_eq!(edition::parse_edition_inner(&reopened).unwrap().author, owner.public_key());
        let folded = fold_roster(&[reopened], &cid(), &Default::default());
        assert_eq!(folded.skipped, 0);
        assert!(folded.roles.role(&role.role_id).is_some(), "build→seal→open→parse→fold round-trips");
    }

    /// Round-trip at a NON-ZERO epoch. The edition must be sealed at the epoch's pseudonym, NOT Epoch(0):
    /// sealing at the wrong epoch lands it at the wrong `#z` and a fresh joiner at epoch 4 would never find it.
    #[test]
    fn control_edition_round_trips_at_a_nonzero_epoch() {
        let owner = Keys::generate();
        let community_id = CommunityId([0x09; 32]);
        let role = Role {
            role_id: "a".repeat(64), name: "Admin".into(), position: 1,
            permissions: Permissions::admin(), scope: RoleScope::Server, color: 0,
        };
        let inner = build_role_edition(&owner, &role, 1, None, 100, None).unwrap();
        let outer = seal_control_edition(&Keys::generate(), &inner, &sr(), &community_id, Epoch(4)).unwrap();

        // Distinct epochs address distinct pseudonyms, and the edition sits at epoch 4's — not epoch 0's.
        let z4 = control_pseudonym(&sr(), &community_id, Epoch(4));
        assert_ne!(z4, control_pseudonym(&sr(), &community_id, Epoch(0)), "epochs address distinct pseudonyms");
        let z_tag = outer.tags.iter().find_map(|t| {
            let s = t.as_slice();
            (s.len() >= 2 && s[0] == "z").then(|| s[1].clone())
        }).expect("control outer carries a z tag");
        assert_eq!(z_tag, z4, "sealed at the epoch-4 pseudonym, NOT epoch 0 (regression #1 guard)");

        // A fresh joiner (empty floors) at epoch 4 still opens + folds it.
        let reopened = open_control_edition(&outer, &sr()).unwrap();
        let folded = fold_roster(&[reopened], &cid(), &Default::default());
        assert!(folded.roles.role(&role.role_id).is_some(), "seal→open→fold round-trips at a non-zero epoch");
    }

    /// Golden vector — the control-plane pseudonym is a wire coordinate other clients must reproduce
    /// byte-for-byte, so pin it against fixed inputs.
    #[test]
    fn control_pseudonym_golden_vector() {
        let server_root = ServerRootKey([0x07; 32]);
        let community_id = CommunityId([0x09; 32]);
        assert_eq!(
            control_pseudonym(&server_root, &community_id, Epoch(0)),
            "e719f2d29ca005dfe805b1f85f696948394661c748e1f98b2df7d396260f6378"
        );
    }

    /// `open_control_edition` rejects a non-control outer (defensive kind check).
    #[test]
    fn open_rejects_non_control_outer() {
        let bogus = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), "x")
            .sign_with_keys(&Keys::generate())
            .unwrap();
        assert!(open_control_edition(&bogus, &sr()).is_err());
    }

    // --- delegation-chain authorization (#2) ---

    #[test]
    fn delegation_rejects_a_self_signed_admin_grant() {
        // THE fail-open being closed: a member self-signs a grant giving herself Admin. It is validly
        // signed + bound (so it FOLDS), but authorization must DROP it — she never chained to the owner.
        let owner = Keys::generate();
        let mallory = Keys::generate();
        let admin = "a".repeat(64);
        let role_ev = role_event(&owner, &admin, 1, 1, None, 100); // owner creates Admin (pos 1)
        let (self_grant, _, _) = grant_event(&mallory, &mallory.public_key().to_hex(), vec![admin.clone()], 1, None, 101);
        let folded = fold_roster(&[role_ev, self_grant], &cid(), &Default::default());
        // Raw fold (binding layer) trusts it — proving authorization is the thing doing the work here.
        assert!(folded.roles.grants.iter().any(|g| g.member == mallory.public_key().to_hex()));

        let authorized = authorize_delegation(&folded, Some(&owner.public_key().to_hex()));
        assert!(authorized.roles.iter().any(|r| r.role_id == admin), "owner-signed role stays");
        assert!(
            !authorized.grants.iter().any(|g| g.member == mallory.public_key().to_hex()),
            "self-signed Admin grant is REJECTED — no self-promotion"
        );
    }

    #[test]
    fn delegation_accepts_owner_signed_role_and_grant() {
        let owner = Keys::generate();
        let member = Keys::generate();
        let admin = "a".repeat(64);
        let role_ev = role_event(&owner, &admin, 1, 1, None, 100);
        let (grant_ev, _, _) = grant_event(&owner, &member.public_key().to_hex(), vec![admin.clone()], 1, None, 101);
        let authorized = authorize_delegation(&fold_roster(&[role_ev, grant_ev], &cid(), &Default::default()), Some(&owner.public_key().to_hex()));
        assert!(authorized.roles.iter().any(|r| r.role_id == admin));
        assert!(authorized.grants.iter().any(|g| g.member == member.public_key().to_hex()));
    }

    #[test]
    fn delegation_chains_owner_to_admin_to_mod() {
        // owner → Alice (Admin, pos 1); Alice creates Mod (pos 3) and grants Bob — a legit 3-deep chain.
        let owner = Keys::generate();
        let alice = Keys::generate();
        let bob = Keys::generate();
        let (admin, moderator) = ("a".repeat(64), "b".repeat(64));
        let r_admin = role_event(&owner, &admin, 1, 1, None, 100);
        let (g_alice, _, _) = grant_event(&owner, &alice.public_key().to_hex(), vec![admin.clone()], 1, None, 101);
        let r_mod = role_event(&alice, &moderator, 3, 1, None, 102); // Alice (admin) creates a lower role
        let (g_bob, _, _) = grant_event(&alice, &bob.public_key().to_hex(), vec![moderator.clone()], 1, None, 103);
        let authorized = authorize_delegation(
            &fold_roster(&[r_admin, g_alice, r_mod, g_bob], &cid(), &Default::default()),
            Some(&owner.public_key().to_hex()),
        );
        assert!(authorized.roles.iter().any(|r| r.role_id == moderator), "admin Alice could create Mod");
        assert!(authorized.grants.iter().any(|g| g.member == alice.public_key().to_hex()), "owner→Alice Admin");
        assert!(authorized.grants.iter().any(|g| g.member == bob.public_key().to_hex()), "Alice→Bob Mod (delegated)");
    }

    #[test]
    fn delegation_demoted_admins_delegated_grant_is_dropped() {
        // version-pinned delegation, proven SUBSUMED by the floor-fold + current-roster fixpoint (no
        // `vac` citation on the delegation plane): owner→Alice (admin v1), Alice→Bob (mod). The owner then
        // REVOKES Alice (her grant v2 = empty). After the fold her grant head is the empty revoke, so she
        // is not an authorized admin → her delegated grant of Bob is DROPPED (refuse-superseded).
        let owner = Keys::generate();
        let alice = Keys::generate();
        let bob = Keys::generate();
        let (admin, moderator) = ("a".repeat(64), "b".repeat(64));
        let r_admin = role_event(&owner, &admin, 1, 1, None, 100);
        let (g_alice1, a_eid, a_c1) = grant_event(&owner, &alice.public_key().to_hex(), vec![admin.clone()], 1, None, 101);
        let a_v1 = version::edition_hash(&a_eid, 1, None, a_c1.as_bytes());
        let (g_alice2, _, _) = grant_event(&owner, &alice.public_key().to_hex(), vec![], 2, Some(&a_v1), 102); // revoke
        let r_mod = role_event(&alice, &moderator, 3, 1, None, 103);
        let (g_bob, _, _) = grant_event(&alice, &bob.public_key().to_hex(), vec![moderator.clone()], 1, None, 104);

        let folded = fold_roster(&[r_admin, g_alice1, g_alice2, r_mod, g_bob], &cid(), &Default::default());
        let authorized = authorize_delegation(&folded, Some(&owner.public_key().to_hex()));
        assert!(!authorized.grants.iter().any(|g| g.member == alice.public_key().to_hex()), "Alice's admin grant is revoked");
        assert!(
            !authorized.grants.iter().any(|g| g.member == bob.public_key().to_hex()),
            "a since-demoted admin's delegated grant is dropped — version-pinning falls out, no citation needed"
        );
    }

    #[test]
    fn delegation_floor_blocks_a_rolled_back_admin_grant() {
        // The refuse-downgrade FLOOR protects the DELEGATION plane too: we already hold Alice's grant at
        // v2 (her revoke), but a withholding relay re-serves only her v1 admin grant. v1 is below the
        // floor → refused → Alice is absent from the fold → her delegated grant of Bob is dropped. This
        // is the delegation-plane analogue of the banlist `withheld_revocation` test.
        let owner = Keys::generate();
        let alice = Keys::generate();
        let bob = Keys::generate();
        let (admin, moderator) = ("a".repeat(64), "b".repeat(64));
        let r_admin = role_event(&owner, &admin, 1, 1, None, 100);
        let (g_alice1, a_eid, _) = grant_event(&owner, &alice.public_key().to_hex(), vec![admin.clone()], 1, None, 101);
        let r_mod = role_event(&alice, &moderator, 3, 1, None, 102);
        let (g_bob, _, _) = grant_event(&alice, &bob.public_key().to_hex(), vec![moderator.clone()], 1, None, 103);

        let mut floors = std::collections::HashMap::new();
        floors.insert(crate::simd::hex::bytes_to_hex_32(&a_eid), (2u64, [0x9Au8; 32])); // held floor = v2
        let folded = fold_roster(&[r_admin, g_alice1, r_mod, g_bob], &cid(), &floors);
        let authorized = authorize_delegation(&folded, Some(&owner.public_key().to_hex()));
        assert!(!authorized.grants.iter().any(|g| g.member == alice.public_key().to_hex()), "rolled-back Alice grant refused");
        assert!(
            !authorized.grants.iter().any(|g| g.member == bob.public_key().to_hex()),
            "Bob's delegated grant dropped — the floor blocks the rollback that would re-authorize Alice"
        );
    }

    #[test]
    fn delegation_admin_cannot_grant_a_peer_admin() {
        // escalation defense: a position-1 admin cannot grant another position-1 Admin (1 !< 1).
        let owner = Keys::generate();
        let alice = Keys::generate();
        let bob = Keys::generate();
        let admin = "a".repeat(64);
        let r_admin = role_event(&owner, &admin, 1, 1, None, 100);
        let (g_alice, _, _) = grant_event(&owner, &alice.public_key().to_hex(), vec![admin.clone()], 1, None, 101);
        let (g_bob, _, _) = grant_event(&alice, &bob.public_key().to_hex(), vec![admin.clone()], 1, None, 102);
        let authorized = authorize_delegation(
            &fold_roster(&[r_admin, g_alice, g_bob], &cid(), &Default::default()),
            Some(&owner.public_key().to_hex()),
        );
        assert!(authorized.grants.iter().any(|g| g.member == alice.public_key().to_hex()), "owner→Alice ok");
        assert!(
            !authorized.grants.iter().any(|g| g.member == bob.public_key().to_hex()),
            "an admin cannot grant a PEER-rank Admin"
        );
    }

    #[test]
    fn delegation_rejects_a_circular_grant_with_no_owner_root() {
        // The headline adversarial case: A grants B Admin, B grants A Admin — neither chains to the
        // owner. The fixpoint cannot bootstrap a cycle (no owner-rooted seed), so NEITHER is accepted.
        let owner = Keys::generate(); // a real owner exists but is NOT party to these grants
        let a = Keys::generate();
        let b = Keys::generate();
        let admin = "a".repeat(64);
        let r_admin = role_event(&owner, &admin, 1, 1, None, 100);
        let (g_ab, _, _) = grant_event(&a, &b.public_key().to_hex(), vec![admin.clone()], 1, None, 101);
        let (g_ba, _, _) = grant_event(&b, &a.public_key().to_hex(), vec![admin.clone()], 1, None, 102);
        let authorized = authorize_delegation(
            &fold_roster(&[r_admin, g_ab, g_ba], &cid(), &Default::default()),
            Some(&owner.public_key().to_hex()),
        );
        assert!(authorized.roles.iter().any(|r| r.role_id == admin), "owner's role stays");
        assert!(authorized.grants.is_empty(), "a circular mutual-admin grant cannot bootstrap without the owner");
    }

    #[test]
    fn delegation_rejects_a_position_0_role_even_owner_signed() {
        // Position 0 is reserved to the owner attestation. A role at pos 0 is rejected regardless
        // of signer — a non-owner can't outrank pos 0, and even the owner may not mint a pos-0 ROLE.
        let owner = Keys::generate();
        let mallory = Keys::generate();
        let by_mallory = authorize_delegation(
            &fold_roster(&[role_event(&mallory, &"e".repeat(64), 0, 1, None, 100)], &cid(), &Default::default()),
            Some(&owner.public_key().to_hex()),
        );
        assert!(by_mallory.roles.is_empty(), "a non-owner cannot mint a position-0 role");
        let by_owner = authorize_delegation(
            &fold_roster(&[role_event(&owner, &"f".repeat(64), 0, 1, None, 100)], &cid(), &Default::default()),
            Some(&owner.public_key().to_hex()),
        );
        assert!(by_owner.roles.is_empty(), "position 0 is the attestation's — even the owner can't mint a pos-0 role");
    }

    #[test]
    fn delegation_unproven_community_yields_empty_roster() {
        // No owner attestation → no root of the chain → nothing is authorized (fail closed).
        let owner = Keys::generate();
        let role_ev = role_event(&owner, &"a".repeat(64), 1, 1, None, 100);
        let authorized = authorize_delegation(&fold_roster(&[role_ev], &cid(), &Default::default()), None);
        assert!(authorized.roles.is_empty() && authorized.grants.is_empty(), "no root ⇒ empty authorized roster");
    }
}
