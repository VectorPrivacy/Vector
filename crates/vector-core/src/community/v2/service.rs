//! Concord v2 service — the stateful orchestration binding the pure v2 modules
//! to storage + transport. Free functions, `SessionGuard`-gated at every write
//! (a `swap_session` can land at any await — see CLAUDE.md), mirroring the v1
//! service's discipline.
//!
//! First-cut scope (bots): create a community, send + fetch channel messages,
//! accept invites, publish a Guestbook Join. Rotation/refounding/moderation and
//! full roster folding layer on next. Signing is local-keys for now (bots hold
//! their nsec); a NIP-46 bunker create/send path is a documented follow-up (v2's
//! genesis + chat seals are sign-only ops, so it composes — just needs the async
//! signer threaded through `build_seal`).

use nostr_sdk::prelude::{Event, Keys, PublicKey, SecretKey, Timestamp};

use super::super::transport::{Query, Transport};
use super::super::{version, ChannelId, Epoch};
use super::chat::{self, ChatEvent};
use super::community::{ChannelV2, CommunityV2};
use super::control;
use super::derive::{base_rekey_group_key, channel_group_key, channel_rekey_group_key, control_group_key, GroupKey};
use super::invite::{self, CommunityInvite};
use super::rekey::{self, Continuity, RekeyScope};
use super::{guestbook, stream, vsk};
use crate::community::edition::ParsedEdition;
use crate::state::SessionGuard;

/// The local identity keys, or an error if none is installed. First-cut signing
/// path (a bunker account routes through the async signer — a follow-up).
fn local_keys() -> Result<Keys, String> {
    crate::state::MY_SECRET_KEY
        .to_keys()
        .ok_or_else(|| "Concord v2 needs a local identity key (bunker create/send is not wired yet)".to_string())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Create a fresh v2 community owned by the local identity: mint the genesis
/// (self-certifying id + the two owner editions), persist, publish the genesis
/// control editions, and announce the owner's Guestbook Join. Returns the saved
/// community.
pub async fn create_community<T: Transport + ?Sized>(
    transport: &T,
    name: &str,
    relays: Vec<String>,
    description: Option<String>,
) -> Result<CommunityV2, String> {
    let session = SessionGuard::capture();
    let owner = local_keys()?;
    let at_ms = now_ms();

    let meta = control::CommunityMetadata {
        name: name.to_string(),
        description: description.clone(),
        relays: relays.clone(),
        ..Default::default()
    };
    let genesis = control::genesis(&owner, meta, at_ms / 1000).map_err(|e| e.to_string())?;
    let community = CommunityV2::from_genesis(&genesis, name, description, relays.clone(), at_ms);

    // Save-before-publish (like v1 create): no peers exist yet so there's no
    // shared view to diverge from, and the fresh-random keys are irrecoverable
    // if a publish hiccup rolled them back. Re-check the session first — genesis
    // signing straddled no await here, but the DB write is the side effect.
    if !session.is_valid() {
        return Err("account changed during community creation".to_string());
    }
    // Seed the genesis edition heads (v1) as the owner's refuse-downgrade floor, so a
    // later edit can't be rolled back by a relay serving only the genesis prefix. The
    // live control sub is replay-free (limit 0), so the owner won't re-fold its own
    // genesis to seed the floor otherwise. Floors land BEFORE the community row
    // (floors-then-state ordering).
    let control = control_group_key(&community.community_root, community.id(), community.root_epoch);
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    for wrap in &genesis.wraps {
        if let Ok((ed, _)) = control::open_control_edition(wrap, &control) {
            let entity_hex = crate::simd::hex::bytes_to_hex_32(&ed.entity_id);
            crate::db::community::set_edition_head_at_epoch(&cid_hex, &entity_hex, ed.version, &ed.self_hash, &ed.inner_id, community.root_epoch.0)?;
        }
    }
    crate::db::community::save_community_v2(&community)?;
    // Archive the genesis root at epoch 0, so a later Refounding leaves this epoch's
    // Public-channel history readable (CORD-03 §3 multi-epoch read).
    let _ = crate::db::community::store_epoch_key(&cid_hex, crate::community::SERVER_ROOT_SCOPE_HEX, community.root_epoch.0, &community.community_root);

    // Publish the two genesis control editions at the epoch-0 control plane.
    for wrap in &genesis.wraps {
        transport.publish(wrap, &community.relays).await?;
    }

    // Announce the owner's Guestbook Join so they appear in the memberlist.
    let gb_group = super::derive::guestbook_group_key(&community.community_root, community.id(), community.root_epoch);
    let join_rumor = guestbook::build_join_rumor(owner.public_key(), None, at_ms);
    if let Ok((join_wrap, _)) = guestbook::seal_guestbook_rumor(&join_rumor, &gb_group, &owner, Timestamp::from_secs(at_ms / 1000)) {
        let _ = transport.publish(&join_wrap, &community.relays).await;
    }

    // Sync the new membership across devices (CORD-02 §8) — best-effort.
    let _ = republish_community_list(transport).await;
    Ok(community)
}

/// Send a text message to a channel. Derives the channel's Chat-Plane group key
/// (community_root for a Public channel, the channel key for a Private one),
/// seals it encrypted, and publishes. Returns the message's rumor id (hex).
pub async fn send_message<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    channel_id: &ChannelId,
    content: &str,
) -> Result<String, String> {
    let session = SessionGuard::capture();
    let author = local_keys()?;
    let ch = community.channel(channel_id).ok_or("no such channel in this community")?;
    // A keyless private channel can't be addressed (deriving from the root would post
    // to the public plane); wait for the rekey to deliver its key.
    if ch.private && ch.key.is_none() {
        return Err("this private channel has no key yet (awaiting rekey delivery)".to_string());
    }
    let (secret, epoch) = community.channel_secret(ch);
    let group = channel_group_key(&secret, channel_id, epoch);

    let at_ms = now_ms();
    let rumor = chat::build_message_rumor(author.public_key(), channel_id, epoch, content, None, &[], vec![], at_ms);
    let rumor_id = rumor.id.ok_or("rumor has no id")?.to_hex();
    let (wrap, _ephemeral) = chat::seal_chat_rumor(&rumor, &group, &author, Timestamp::from_secs(at_ms / 1000), false)
        .map_err(|e| e.to_string())?;

    if !session.is_valid() {
        return Err("account changed before send".to_string());
    }
    transport.publish(&wrap, &community.relays).await?;
    Ok(rumor_id)
}

/// A chat event opened from a channel fetch, tagged with the epoch its key
/// decrypted under.
pub struct FetchedEvent {
    pub event: ChatEvent,
    pub epoch: Epoch,
}

/// Fetch a channel's recent messages: query every held epoch's Chat-Plane
/// address, open + bind each, drop foreign/malformed, dedup by rumor id, and
/// order oldest→newest by the millisecond timestamp. `limit` bounds the newest
/// slice fetched from each address.
pub async fn fetch_channel<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    channel_id: &ChannelId,
    limit: usize,
) -> Result<Vec<FetchedEvent>, String> {
    let ch = community.channel(channel_id).ok_or("no such channel in this community")?;
    // A Public channel reads across EVERY held base-root epoch, so history spanning a
    // Refounding stays continuous (CORD-03 §3); a Private channel reads its current key
    // (private multi-epoch history is deferred with the per-channel key archive).
    let coords: Vec<([u8; 32], Epoch)> = if ch.private {
        community.channel_read_coords(ch)
    } else {
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let mut roots = crate::db::community::held_epoch_keys(&cid_hex, crate::community::SERVER_ROOT_SCOPE_HEX).unwrap_or_default();
        if !roots.iter().any(|(ep, _)| *ep == community.root_epoch) {
            roots.push((community.root_epoch, community.community_root));
        }
        roots.into_iter().map(|(ep, root)| (root, ep)).collect()
    };

    // Address every held epoch by its Chat-Plane pubkey.
    let authors: Vec<String> = coords
        .iter()
        .map(|(secret, epoch)| channel_group_key(secret, channel_id, *epoch).pk_hex())
        .collect();
    let query = Query {
        kinds: vec![stream::KIND_WRAP],
        authors: authors.clone(),
        limit: Some(limit),
        ..Default::default()
    };
    let wraps = transport.fetch(&query, &community.relays).await?;

    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<(u64, FetchedEvent)> = Vec::new();
    for wrap in &wraps {
        // Select the epoch whose group key authored this wrap (no trial decrypt).
        for (secret, epoch) in &coords {
            let group = channel_group_key(secret, channel_id, *epoch);
            if wrap.pubkey != group.pk() {
                continue;
            }
            if let Ok(event) = chat::open_chat_event(wrap, &group, channel_id, *epoch) {
                let id = event.opened().rumor_id;
                if seen.insert(id) {
                    out.push((event.opened().at_ms, FetchedEvent { event, epoch: *epoch }));
                }
            }
            break;
        }
    }
    out.sort_by_key(|(ms, _)| *ms);
    Ok(out.into_iter().map(|(_, e)| e).collect())
}

// ── Invites (CORD-05) ────────────────────────────────────────────────────────

/// Build the §1 invite bundle for this community. Every channel is granted: a
/// Public channel carries the `community_root` as its "key" (the joiner derives
/// the real secret from the root), a Private one its own key. The bundle
/// self-certifies the owner, so the inviter's identity is irrelevant to trust.
pub fn bundle_of(
    community: &CommunityV2,
    creator: Option<PublicKey>,
    expires_at_ms: Option<u64>,
    label: Option<String>,
) -> CommunityInvite {
    let hex = crate::simd::hex::bytes_to_hex_32;
    let channels = community
        .channels
        .iter()
        .map(|c| invite::ChannelGrant {
            id: hex(&c.id.0),
            key: hex(&c.key.unwrap_or(community.community_root)),
            epoch: c.epoch.0,
            name: c.name.clone(),
        })
        .collect();
    CommunityInvite {
        community_id: hex(&community.identity.community_id.0),
        owner: hex(&community.identity.owner_xonly),
        owner_salt: hex(&community.identity.owner_salt),
        community_root: hex(&community.community_root),
        root_epoch: community.root_epoch.0,
        channels,
        relays: community.relays.clone(),
        name: community.name.clone(),
        icon: None,
        expires_at: expires_at_ms,
        creator_npub: creator.map(|p| p.to_hex()),
        label,
        extra: Default::default(),
    }
}

/// Gift-wrap a Direct Invite (kind 3313) of this community straight to `recipient`
/// and publish it to the community relays. `expires_at_ms` (unix ms) optionally
/// bounds its shelf life; `label` is echoed in the joiner's Guestbook Join. The
/// bundle hands over the keys; the recipient consents by accepting (nothing joins
/// on receipt). Returns the wrap.
pub async fn send_direct_invite<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    recipient: &PublicKey,
    expires_at_ms: Option<u64>,
    label: Option<String>,
) -> Result<Event, String> {
    let session = SessionGuard::capture();
    let inviter = local_keys()?;
    let bundle = bundle_of(community, Some(inviter.public_key()), expires_at_ms, label);
    let wrap = invite::build_direct_invite(&inviter, recipient, &bundle).map_err(|e| e.to_string())?;
    if !session.is_valid() {
        return Err("account changed before sending invite".to_string());
    }
    transport.publish(&wrap, &community.relays).await?;
    Ok(wrap)
}

/// A minted public link: the shareable URL plus the addressable bundle event to
/// publish and the link keypair to retain (in the Invite List) for later refresh
/// or revocation.
pub struct MintedLink {
    pub url: String,
    pub bundle_event: Event,
    pub link_signer: Keys,
    pub token: [u8; super::derive::TOKEN_LEN],
}

/// Mint a public invite link for this community: a fresh token + link keypair, the
/// bundle encrypted under the token key and published at `(33301, link_signer,
/// "")`, and the `base/invite/<naddr>#<fragment>` URL. `base` is the deep-link
/// domain (e.g. `https://vectorapp.io`); the fragment carries the token + bootstrap
/// relays and never reaches a server.
pub async fn mint_public_link<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    base: &str,
    expires_at_ms: Option<u64>,
    label: Option<String>,
) -> Result<MintedLink, String> {
    let session = SessionGuard::capture();
    let mut token = [0u8; super::derive::TOKEN_LEN];
    token.copy_from_slice(&super::super::random_32()[..super::derive::TOKEN_LEN]);
    let link_signer = Keys::generate();
    let bundle = bundle_of(community, Some(local_keys()?.public_key()), expires_at_ms, label);
    let bundle_key = super::derive::invite_bundle_key(&token);
    let bundle_event = invite::build_bundle_event(&link_signer, &bundle, &bundle_key).map_err(|e| e.to_string())?;
    let url = invite::build_invite_url(base, &link_signer.public_key(), &token, &community.relays).map_err(|e| e.to_string())?;

    if !session.is_valid() {
        return Err("account changed before minting link".to_string());
    }
    transport.publish_durable(&bundle_event, &community.relays).await?;
    let minted = MintedLink { url, bundle_event, link_signer, token };
    // Sync the link across the creator's devices (13303) + publish the Registry
    // (vsk-8) so members see the community is Public. Best-effort — the link works
    // without the sync.
    let _ = record_minted_link(transport, community, &minted).await;
    Ok(minted)
}

// ── The Invite Registry (vsk 8) + Invite List (13303), CORD-05 §4/§5 ──────────

/// Fetch the creator's own 13303 Invite List from `relays` (newest wins; a
/// decrypt/parse failure is "no news", never a clobber of the local mirror).
async fn fetch_invite_list<T: Transport + ?Sized>(transport: &T, relays: &[String]) -> Option<invite::InviteList> {
    let me = local_keys().ok()?;
    let query = Query {
        kinds: vec![super::kind::INVITE_LIST],
        authors: vec![me.public_key().to_hex()],
        limit: Some(4),
        ..Default::default()
    };
    let events = transport.fetch(&query, relays).await.ok()?;
    events
        .into_iter()
        .filter_map(|e| invite::parse_invite_list_event(&e, &me).ok().map(|l| (e.created_at.as_secs(), l)))
        .max_by_key(|(at, _)| *at)
        .map(|(_, l)| l)
}

/// The creator's LIVE (non-tombstoned) link-signer pubkeys for one community — the
/// Registry's content (CORD-05 §5), derived from the stored link secrets.
fn live_signers_for(list: &invite::InviteList, community_id_hex: &str) -> Vec<PublicKey> {
    let dead: std::collections::HashSet<&str> = list.tombstones.iter().map(|t| t.token.as_str()).collect();
    list.entries
        .iter()
        .filter(|e| e.community_id == community_id_hex && !dead.contains(e.token.as_str()))
        .filter_map(|e| Keys::parse(&e.signer_sk).ok().map(|k| k.public_key()))
        .collect()
}

/// Publish the creator's Registry (vsk-8) edition — their live link signers for this
/// community — so members fold it into the Public/Private source of truth (a
/// non-empty aggregate = Public).
async fn publish_invite_registry<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, session: &SessionGuard, live_signers: &[PublicKey]) -> Result<(), String> {
    let me = local_keys()?;
    let eid = super::derive::invite_links_locator(community.id(), &me.public_key().to_bytes());
    let content = invite::build_registry_content(live_signers);
    publish_control_edition(transport, community, session, vsk::INVITE_LINKS, &eid, &content, None).await
}

/// Record a freshly-minted public link across the creator's devices: append it to the
/// 13303 Invite List and refresh the Registry (CORD-05 §4/§5).
async fn record_minted_link<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, minted: &MintedLink) -> Result<(), String> {
    let session = SessionGuard::capture();
    let me = local_keys()?;
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let token_hex = crate::simd::hex::bytes_to_hex_16(&minted.token);
    let mut list = fetch_invite_list(transport, &community.relays).await.unwrap_or_default();
    if !list.entries.iter().any(|e| e.token == token_hex) {
        list.entries.push(invite::InviteEntry {
            token: token_hex,
            signer_sk: minted.link_signer.secret_key().to_secret_hex(),
            community_id: cid_hex.clone(),
            url: minted.url.clone(),
            label: None,
            created_at: now_ms() / 1000,
            expires_at: None,
            extra: Default::default(),
        });
    }
    if !session.is_valid() {
        return Err("account changed during link record".to_string());
    }
    let event = invite::build_invite_list_event(&me, &list).map_err(|e| e.to_string())?;
    transport.publish(&event, &community.relays).await?;
    let signers = live_signers_for(&list, &cid_hex);
    publish_invite_registry(transport, community, &session, &signers).await
}

/// Revoke a public link by its token hex (CORD-05 §2/§5): re-post its coordinate as a
/// revocation tombstone (retiring the bundle behind the URL, so a fetcher finds the
/// grave), tombstone the Invite List entry, and refresh the Registry. Retiring the
/// LAST live link empties the Registry → the community reads Private (a Refounding is
/// the owner's separate read-cut).
pub async fn revoke_public_link<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, token_hex: &str) -> Result<(), String> {
    let session = SessionGuard::capture();
    let me = local_keys()?;
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let mut list = fetch_invite_list(transport, &community.relays).await.ok_or("no invite list found to revoke from")?;
    let entry = list
        .entries
        .iter()
        .find(|e| e.token == token_hex && e.community_id == cid_hex)
        .cloned()
        .ok_or("no such link in the invite list")?;
    // Re-post the bundle coordinate as a revocation tombstone (creator-signed).
    let link_signer = Keys::parse(&entry.signer_sk).map_err(|_| "malformed link signer")?;
    let revocation = invite::build_revocation(&link_signer).map_err(|e| e.to_string())?;
    if !session.is_valid() {
        return Err("account changed during revoke".to_string());
    }
    transport.publish_durable(&revocation, &community.relays).await?;
    // Tombstone the Invite List entry (permanent — a stale device can't resurrect it).
    list.tombstones.push(invite::InviteTombstone { token: token_hex.to_string(), community_id: cid_hex.clone(), extra: Default::default() });
    list.entries.retain(|e| e.token != token_hex);
    let event = invite::build_invite_list_event(&me, &list).map_err(|e| e.to_string())?;
    transport.publish(&event, &community.relays).await?;
    let signers = live_signers_for(&list, &cid_hex);
    publish_invite_registry(transport, community, &session, &signers).await
}

/// Refresh every live public link's bundle behind its stable URL (CORD-05 §2) — e.g.
/// after a Rekey/Refounding rolled the keys — by re-posting the bundle at the same
/// coordinate with the CURRENT community state, so a link shared once keeps working
/// across rotations. Best-effort.
pub async fn refresh_public_links<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> Result<(), String> {
    let session = SessionGuard::capture();
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let Some(list) = fetch_invite_list(transport, &community.relays).await else {
        return Ok(());
    };
    let creator = local_keys()?.public_key();
    let dead: std::collections::HashSet<&str> = list.tombstones.iter().map(|t| t.token.as_str()).collect();
    for entry in &list.entries {
        if entry.community_id != cid_hex || dead.contains(entry.token.as_str()) || entry.token.len() != 2 * super::derive::TOKEN_LEN {
            continue;
        }
        let Ok(link_signer) = Keys::parse(&entry.signer_sk) else { continue };
        let token = crate::simd::hex::hex_to_bytes_16(&entry.token);
        let bundle = bundle_of(community, Some(creator), entry.expires_at, entry.label.clone());
        let bundle_key = super::derive::invite_bundle_key(&token);
        if let Ok(event) = invite::build_bundle_event(&link_signer, &bundle, &bundle_key) {
            if !session.is_valid() {
                return Err("account changed during link refresh".to_string());
            }
            let _ = transport.publish_durable(&event, &community.relays).await;
        }
    }
    Ok(())
}

/// Whether this community is PUBLIC (CORD-05 §5): fold every creator's Registry
/// (vsk-8) that its author is authorized for (`CREATE_INVITE`, bound to their
/// coordinate) into an aggregate live-link set — non-empty ⇒ a live link exists ⇒
/// Public; empty ⇒ Private. Retiring the last link is what flips it back.
pub async fn community_is_public<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> bool {
    use crate::community::roles::Permissions;
    use std::collections::BTreeMap;
    let Ok(owner) = community.owner() else { return false };
    let owner_hex = owner.to_hex();
    let cid = community.id();
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&cid.0);
    let floors: Floors = crate::db::community::get_all_edition_heads_full(&cid_hex)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, f)| f.0 == community.root_epoch.0)
        .map(|(entity, f)| (entity, (f.1, f.2, f.3)))
        .collect();
    let control = control_group_key(&community.community_root, cid, community.root_epoch);
    let query = Query { kinds: vec![stream::KIND_WRAP], authors: vec![control.pk_hex()], limit: Some(FOLLOW_PAGE), ..Default::default() };
    let editions: Vec<ParsedEdition> = transport
        .fetch(&query, &community.relays)
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|w| control::open_control_edition(w, &control).ok().map(|(ed, _)| ed))
        .collect();
    let authority = fold_authority(community, &editions, &floors);

    let mut by_eid: BTreeMap<[u8; 32], Vec<&ParsedEdition>> = BTreeMap::new();
    for e in &editions {
        if e.vsk == vsk::INVITE_LINKS {
            by_eid.entry(e.entity_id).or_default().push(e);
        }
    }
    for (eid, group) in &by_eid {
        let fold_eds: Vec<version::Edition> = group.iter().map(|p| p.to_fold_edition()).collect();
        let (Some(hi), _) = fold_head(&fold_eds, floors.get(&crate::simd::hex::bytes_to_hex_32(eid))) else { continue };
        let head = group[hi];
        // The creator must hold CREATE_INVITE AND own this coordinate.
        if !authority.roles.is_authorized(&head.author.to_hex(), Some(&owner_hex), Permissions::CREATE_INVITE) {
            continue;
        }
        if super::derive::invite_links_locator(cid, &head.author.to_bytes()) != *eid {
            continue;
        }
        if invite::parse_registry_content(&head.content).map(|s| !s.is_empty()).unwrap_or(false) {
            return true;
        }
    }
    false
}

/// Accept an already-unwrapped bundle: verify the owner commitment AND that the
/// delivered community_root is genuinely the owner's, persist the community, and
/// announce a Guestbook Join (with invite attribution). Shared tail of both accept
/// paths. Takes the caller's `SessionGuard` (captured BEFORE any network fetch the
/// caller did) so the `is_valid()` gate straddles that I/O.
async fn accept_bundle<T: Transport + ?Sized>(
    transport: &T,
    session: &SessionGuard,
    bundle: &CommunityInvite,
    invited_by: Option<PublicKey>,
) -> Result<CommunityV2, String> {
    let me = local_keys()?;
    let at_ms = now_ms();
    // Expiry gate: a past invite still previews but must not join (CORD-05 §1).
    if bundle.expired(at_ms) {
        return Err("this invite has expired".to_string());
    }
    // `from_bundle` re-validates bounds + the owner commitment fail-closed.
    let community = CommunityV2::from_bundle(bundle, at_ms)?;

    // Authenticate the delivered community_root before trusting it. The owner
    // commitment proves WHO the owner is, but community_root (and channel keys) are
    // NOT in that commitment, so a forged invite can pair a real (id, owner, salt)
    // with an attacker-chosen root and silently partition the joiner onto planes
    // only the attacker controls. Requiring the owner's genesis to open under the
    // delivered root closes that eclipse; also reconciles channel classification.
    let (community, join_heads) = verify_owner_root_and_reconcile(transport, community).await?;

    // A dissolved community is a grave (CORD-02 §9): refuse to join it.
    if is_dissolved(transport, &community).await {
        return Err("this community has been dissolved".to_string());
    }

    // The account must not have swapped since the guard was captured (which was
    // before any fetch the caller / the verify above performed) — else we'd write
    // A's join into B.
    if !session.is_valid() {
        return Err("account changed during join".to_string());
    }
    // Seed the verified heads as the initial refuse-downgrade floor BEFORE the
    // community row lands (floors-then-state, so a mid-seed error can't leave saved
    // state outrunning its floor); the first post-join follow then can't persist a
    // state below what this join already showed.
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    for h in &join_heads {
        crate::db::community::set_edition_head_at_epoch(&cid_hex, &h.entity_hex, h.version, &h.self_hash, &h.inner_id, community.root_epoch.0)?;
    }
    crate::db::community::save_community_v2(&community)?;
    // Archive the joined root at its epoch, so this member reads Public-channel
    // history from their join epoch onward across later Refoundings (CORD-03 §3).
    let _ = crate::db::community::store_epoch_key(&cid_hex, crate::community::SERVER_ROOT_SCOPE_HEX, community.root_epoch.0, &community.community_root);

    // Announce our Guestbook Join, echoing the invite attribution when present.
    let attribution = invited_by
        .map(|p| p.to_hex())
        .or_else(|| bundle.creator_npub.clone())
        .zip(Some(bundle.label.clone().unwrap_or_default()));
    let attr_ref = attribution.as_ref().map(|(c, l)| (c.as_str(), l.as_str()));
    let gb_group = super::derive::guestbook_group_key(&community.community_root, community.id(), community.root_epoch);
    let join_rumor = guestbook::build_join_rumor(me.public_key(), attr_ref, at_ms);
    if let Ok((join_wrap, _)) = guestbook::seal_guestbook_rumor(&join_rumor, &gb_group, &me, Timestamp::from_secs(at_ms / 1000)) {
        let _ = transport.publish(&join_wrap, &community.relays).await;
    }

    // Record the membership across devices (CORD-02 §8) — best-effort.
    let _ = republish_community_list(transport).await;
    Ok(community)
}

/// Prove the delivered `community_root` is genuinely the owner's, and reconcile
/// channel classification from the owner's editions. `community_id` commits only
/// to `(owner_xonly, owner_salt)` — both semi-public (they ride every bundle and
/// every synced Community List) — so a forged invite can present a real community's
/// id/owner/salt with an attacker-chosen root; every plane then derives from that
/// root, silently eclipsing the joiner onto attacker-controlled addresses while the
/// owner commitment still "verifies". The defense: the owner's genesis metadata
/// edition (vsk-0, `eid == community_id`) only opens under the AUTHENTIC root — an
/// attacker can't forge the owner's seal — so its presence on the control plane
/// derived from the delivered root proves that root. Fail-closed: no owner genesis
/// (forged invite, or relays unreachable) → refuse to join. On success, folds the
/// owner's authoritative editions to heal a bundle that misclassified a channel.
async fn verify_owner_root_and_reconcile<T: Transport + ?Sized>(
    transport: &T,
    community: CommunityV2,
) -> Result<(CommunityV2, Vec<FoldedHead>), String> {
    let owner = community.owner()?;
    let control = control_group_key(&community.community_root, community.id(), community.root_epoch);
    let control_pk = control.pk_hex();

    // Authenticity = the owner's GENESIS metadata edition (vsk-0, `eid ==
    // community_id`) at the root-derived control plane. The genesis eid pins it to
    // THIS community, and it lives ONLY under the real root — so a forged root can't
    // produce one: an edition's seal carries no community binding, but another
    // community's genesis has a different eid, and this community's own genesis is
    // unreadable without its real root (which the forger lacks). ("Any owner edition"
    // is NOT sound: an owner sig from any co-owned community, rewrapped onto the fake
    // plane, would pass — reopening the eclipse.) The residual — a T-member replaying
    // T's genesis onto a fake root to MITM another T-joiner — is closed only by
    // binding the root into community_id (protocol, deferred).
    //
    // Seed `until` with a FAR-FUTURE constant (NOT now-based): `until.is_some()` takes
    // the transport's AUTHORITATIVE drain-ALL-relays path (an open `until` returns only
    // a fast relay's partial window and misses a genesis on a lagging relay — routine
    // over Tor), while a constant beyond any real created_at clips NOTHING — so neither
    // a clock-skewed future-dated genesis nor a >1h-slow-clock joiner is excluded (a
    // now-based bound could clip either). Break on an EMPTY page (a short page is a
    // relay cap). A forged root walks to exhaustion and rejects; a flood/deep plane
    // that buries the genesis past the walk is the deferred protocol residual.
    const PAGE: usize = 500;
    const MAX_PAGES: usize = 4;
    const FAR_FUTURE_SECS: u64 = 4_102_444_800; // ~year 2100 — above any real edition, safe as a relay `until`.
    let mut editions: Vec<ParsedEdition> = Vec::new();
    let mut found_genesis = false;
    let mut until: Option<u64> = Some(FAR_FUTURE_SECS);
    let mut seen_wraps: std::collections::HashSet<nostr_sdk::EventId> = std::collections::HashSet::new();
    for _ in 0..MAX_PAGES {
        let query = Query {
            kinds: vec![stream::KIND_WRAP],
            authors: vec![control_pk.clone()],
            until,
            limit: Some(PAGE),
            ..Default::default()
        };
        let wraps = transport.fetch(&query, &community.relays).await?;
        // INCLUSIVE `until` + wrap-id dedup: a `-1` step can skip same-second
        // siblings at a page boundary (and the genesis with them); re-served
        // boundary events are free, and no-new-events means exhausted.
        let mut oldest = u64::MAX;
        let mut fresh = 0usize;
        for w in &wraps {
            if !seen_wraps.insert(w.id) {
                continue;
            }
            fresh += 1;
            oldest = oldest.min(w.created_at.as_secs());
            if let Ok((ed, _)) = control::open_control_edition(w, &control) {
                if ed.author == owner {
                    if ed.vsk == vsk::COMMUNITY_METADATA && ed.entity_id == community.id().0 {
                        found_genesis = true;
                    }
                    editions.push(ed);
                }
            }
        }
        if found_genesis || fresh == 0 {
            break; // authenticated (the owner genesis), or the relay is exhausted.
        }
        until = Some(oldest);
    }
    if !found_genesis {
        return Err(
            "could not verify this community from its relays (the invite may be forged, the relays are unreachable, or the control plane is being flooded); not joining"
                .to_string(),
        );
    }
    // Join-time reconcile: the joiner holds no floors yet (empty map → bootstrap per
    // entity). The heads this fold verified are returned for the caller to SEED as
    // the initial floor once the community row is saved — without that, the first
    // post-join follow would bootstrap floor-less and could persist a state BELOW
    // what this join already verified and showed.
    // Join-time reconcile folds only the owner's editions (genesis-authenticated
    // above), and the owner is supreme — so owner-only authority suffices. The full
    // roster (admins) folds on the first post-join follow_control.
    let empty_floors = Floors::new();
    let authority = AuthoritySet::owner_only();
    let fold = apply_control_fold(&community, &editions, &empty_floors, &authority);
    Ok((fold.updated.unwrap_or(community), fold.heads))
}

/// Accept a Direct Invite: unwrap the 3313 giftwrap (Schnorr-verifying the seal),
/// then run the shared accept path. The recipient's consent IS this call. No
/// network await precedes the accept, so the guard captured here suffices.
pub async fn accept_direct_invite<T: Transport + ?Sized>(transport: &T, wrap: &Event) -> Result<CommunityV2, String> {
    let session = SessionGuard::capture();
    let me = local_keys()?;
    let (inviter, bundle) = invite::unwrap_direct_invite(wrap, &me).map_err(|e| e.to_string())?;
    accept_bundle(transport, &session, &bundle, Some(inviter)).await
}

/// Accept a PARKED Direct Invite from its stored bundle JSON (the wrap was already
/// unwrapped + owner-verified at park time). Re-parses through the same fail-closed
/// bundle validation, then runs the shared accept path (which re-verifies the owner
/// root over the network). `inviter_hex` is the parked seal signer, for Guestbook
/// Join attribution.
pub async fn accept_parked_invite<T: Transport + ?Sized>(
    transport: &T,
    bundle_json: &str,
    inviter_hex: Option<&str>,
) -> Result<CommunityV2, String> {
    let session = SessionGuard::capture();
    let bundle = CommunityInvite::from_bundle_json(bundle_json).map_err(|e| e.to_string())?;
    let invited_by = inviter_hex.and_then(|h| PublicKey::parse(h).ok());
    accept_bundle(transport, &session, &bundle, invited_by).await
}

/// Accept a public invite link: parse it, fetch every event at `(33301,
/// link_signer, "")`, and join. **Revocation is authoritative-if-present**: if
/// ANY signer-valid tombstone is among the fetched events, refuse — never trust
/// fetch ordering (a cross-relay union has no global newest-first sort, so a
/// stale Live could otherwise win a partial-propagation race). Otherwise pick the
/// newest valid Live by `created_at`.
pub async fn accept_public_link<T: Transport + ?Sized>(transport: &T, url: &str) -> Result<CommunityV2, String> {
    // Capture BEFORE the network fetch so the join's is_valid() gate straddles it.
    let session = SessionGuard::capture();
    let parsed = invite::parse_invite_link(url).map_err(|e| e.to_string())?;
    let query = Query {
        kinds: vec![super::kind::INVITE_BUNDLE],
        authors: vec![parsed.link_signer.to_hex()],
        d_tags: vec![String::new()],
        ..Default::default()
    };
    let relays = if parsed.bootstrap_relays.is_empty() {
        invite::stock_relays()
    } else {
        parsed.bootstrap_relays.clone()
    };
    let events = transport.fetch(&query, &relays).await?;
    if !session.is_valid() {
        return Err("account changed during join".to_string());
    }
    let bundle_key = super::derive::invite_bundle_key(&parsed.token);

    // Scan EVERY event: a tombstone beats a Live unconditionally (order-independent).
    let mut newest_live: Option<(u64, CommunityInvite)> = None;
    for event in &events {
        match invite::parse_bundle_event(event, &parsed.link_signer, &bundle_key) {
            Ok(invite::BundleState::Revoked) => return Err("this invite link has been revoked".to_string()),
            Ok(invite::BundleState::Live(bundle)) => {
                let at = event.created_at.as_secs();
                if newest_live.as_ref().is_none_or(|(t, _)| at > *t) {
                    newest_live = Some((at, *bundle));
                }
            }
            Err(_) => {} // a foreign/garbage event at the coordinate — ignore.
        }
    }
    match newest_live {
        Some((_, bundle)) => accept_bundle(transport, &session, &bundle, None).await,
        None => Err("invite bundle not found on relays".to_string()),
    }
}

/// Leave a community: publish a Guestbook Leave and tear down the local hold.
pub async fn leave_community<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> Result<(), String> {
    let session = SessionGuard::capture();
    let me = local_keys()?;
    let at_ms = now_ms();
    let gb_group = super::derive::guestbook_group_key(&community.community_root, community.id(), community.root_epoch);
    let leave_rumor = guestbook::build_leave_rumor(me.public_key(), at_ms);
    if let Ok((wrap, _)) = guestbook::seal_guestbook_rumor(&leave_rumor, &gb_group, &me, Timestamp::from_secs(at_ms / 1000)) {
        let _ = transport.publish(&wrap, &community.relays).await;
    }
    if !session.is_valid() {
        return Err("account changed during leave".to_string());
    }
    // Tombstone the membership across devices (CORD-02 §8) BEFORE the local delete,
    // to the leaving community's own relays (it's about to be gone locally) —
    // best-effort.
    let _ = tombstone_community_list(transport, community.id(), &community.relays).await;
    crate::db::community::delete_community(&crate::simd::hex::bytes_to_hex_32(&community.id().0))?;
    Ok(())
}

/// Fold the Complete Memberlist from the Guestbook plane. The proven owner is
/// ALWAYS a member (derived from the self-certifying community_id — no network,
/// so a lost/evicted genesis Join can't drop them). Observed authors — anyone
/// seen publishing on a channel — are folded in FORWARD-only per CORD-02 §5, so a
/// member whose Join was lost still counts.
pub async fn memberlist<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> Result<Vec<PublicKey>, String> {
    let gb_group = super::derive::guestbook_group_key(&community.community_root, community.id(), community.root_epoch);
    let query = Query {
        kinds: vec![stream::KIND_WRAP],
        authors: vec![gb_group.pk_hex()],
        limit: Some(500),
        ..Default::default()
    };
    let wraps = transport.fetch(&query, &community.relays).await?;
    let owner = community.owner()?;
    let mut events = Vec::new();
    for wrap in &wraps {
        if let Ok(opened) = stream::open_wrap(wrap, &gb_group) {
            if let Ok(ev) = guestbook::parse_guestbook_event(&opened) {
                events.push(ev);
            }
        }
    }
    // Observed authors: fold each held channel's recent authorship (real author +
    // newest ms), so a member who posted but whose Join was lost is still counted.
    let mut observed: std::collections::BTreeMap<PublicKey, u64> = std::collections::BTreeMap::new();
    for ch in &community.channels {
        if let Ok(page) = fetch_channel(transport, community, &ch.id, 200).await {
            for f in &page {
                let e = observed.entry(f.event.opened().author).or_insert(0);
                *e = (*e).max(f.event.opened().at_ms);
            }
        }
    }

    // Fold the Control Plane roster + banlist (CORD-04) for Kick authority and the
    // ban subtraction. A control fetch failure degrades to owner-only authority + no
    // bans (fail-open on availability is safe here: a Kick still needs a real signer,
    // and a missed ban only fails to HIDE, never to wrongly admit authority).
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let floors: Floors = crate::db::community::get_all_edition_heads_full(&cid_hex)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, f)| f.0 == community.root_epoch.0)
        .map(|(entity, f)| (entity, (f.1, f.2, f.3)))
        .collect();
    let control = control_group_key(&community.community_root, community.id(), community.root_epoch);
    let control_query = Query {
        kinds: vec![stream::KIND_WRAP],
        authors: vec![control.pk_hex()],
        limit: Some(FOLLOW_PAGE),
        ..Default::default()
    };
    let control_eds: Vec<ParsedEdition> = transport
        .fetch(&control_query, &community.relays)
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|w| control::open_control_edition(w, &control).ok().map(|(ed, _)| ed))
        .collect();
    let authority = fold_authority(community, &control_eds, &floors);
    let owner_hex = owner.to_hex();

    // Genesis / never-refounded community: NO snapshot authority (there is no
    // refounder — an owner who didn't mint the epoch has no snapshot power). The
    // refounder of a rotated `root_epoch` is threaded once Refounding SEND lands.
    let no_snapshots: Option<&PublicKey> = None;
    // Kick authority (CORD-04 §6): the signer must hold KICK AND strictly outrank the
    // target (the owner is supreme; equal cannot kick equal).
    let can_kick = |actor: &PublicKey, target: &PublicKey| {
        authority
            .roles
            .can_act_on_member(&actor.to_hex(), Some(&owner_hex), &target.to_hex(), crate::community::roles::Permissions::KICK)
    };
    let coalesced = guestbook::coalesce(&events, now_ms(), no_snapshots, &can_kick);
    // The authorized banlist, as pubkeys (a malformed hex entry is simply dropped).
    let banlist: std::collections::BTreeSet<PublicKey> =
        authority.banned.iter().filter_map(|h| PublicKey::from_hex(h).ok()).collect();
    let mut members = guestbook::complete_memberlist(&coalesced, &observed, &banlist);
    // The owner is a member by definition, independent of any fetched Join.
    if !banlist.contains(&owner) {
        members.insert(owner);
    }
    Ok(members.into_iter().collect())
}

// ── Dissolution (CORD-02 §9) ─────────────────────────────────────────────────

/// Owner dissolution / "Delete Community" (CORD-02 §9): publish the terminal
/// tombstone at the dissolved plane (`community_id`-derived, epoch-free, so every
/// past or present member resolves the same grave and a Refounding can never strand
/// it). The tombstone's presence IS the state; only the owner's seal counts.
/// Irreversible — on success the local hold is sealed read-only.
pub async fn dissolve_community<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> Result<(), String> {
    let session = SessionGuard::capture();
    let me = local_keys()?;
    if community.owner()? != me.public_key() {
        return Err("only the owner can dissolve a community".to_string());
    }
    let at = now_ms() / 1000;
    let rumor = super::dissolution::dissolved_tombstone_rumor(me.public_key(), community.id(), at);
    let wrap = super::dissolution::seal_dissolved(&rumor, community.id(), &me, Timestamp::from_secs(at)).map_err(|e| e.to_string())?;
    if !session.is_valid() {
        return Err("account changed during dissolve".to_string());
    }
    // Durable broadcast: death must propagate (a rekey racing a dissolution loses).
    transport.publish_durable(&wrap, &community.relays).await?;
    crate::db::community::set_community_dissolved(&crate::simd::hex::bytes_to_hex_32(&community.id().0))?;
    Ok(())
}

/// Whether a valid owner-signed dissolution tombstone exists for this community on
/// its relays (CORD-02 §9). A join refuses a dead community, and a live follow seals
/// on sight. Fail-OPEN on a fetch error (absence of proof is not death), but any
/// owner-verified tombstone found is authoritative.
pub async fn is_dissolved<T: Transport + ?Sized>(transport: &T, community: &CommunityV2) -> bool {
    let group = super::derive::dissolved_group_key(community.id());
    let query = Query {
        kinds: vec![stream::KIND_WRAP],
        authors: vec![group.pk_hex()],
        limit: Some(20),
        ..Default::default()
    };
    let Ok(wraps) = transport.fetch(&query, &community.relays).await else {
        return false;
    };
    wraps.iter().any(|w| super::dissolution::verify_dissolved(w, &community.identity))
}

// ── Refounding (CORD-06 §3) ──────────────────────────────────────────────────

/// Owner/admin Refounding (CORD-06 §3): roll the `community_root` to
/// cryptographically remove `removed` from a Private community (a Ban's read-cut).
/// Compacts the Control Plane under the new root (re-wraps each head VERBATIM — the
/// inner owner/actor signatures survive, so no re-authoring), rekeys the base plus
/// every Private channel (each sealed under the PRIOR root, D2, so a base-fork loser
/// can still open them), and seeds the new epoch's Guestbook snapshot. Requires BAN.
///
/// **Acquire-before-commit:** the compaction is fetched + re-sealed BEFORE any
/// publish, and a head we can't fetch ABORTS with ZERO published state — so a
/// transient miss never strands a published rekey with a half-anchored plane.
pub async fn refound_community<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, removed: &[PublicKey]) -> Result<CommunityV2, String> {
    let session = SessionGuard::capture();
    let cid = community.id();
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&cid.0);
    // Death wins every race: a dissolved community never re-founds (CORD-02 §9).
    if crate::db::community::get_community_dissolved(&cid_hex).unwrap_or(false) {
        return Err("this community has been dissolved; it cannot be re-founded".to_string());
    }
    let me = local_keys()?;
    // Reload the FRESHEST base state: a stale caller struct would address the rotation
    // under a superseded root (a base fork with no heal). The community_id is
    // self-certifying + stable, so re-loading by it is safe.
    let fresh = crate::db::community::load_community_v2(cid)?.ok_or("community gone before re-founding")?;
    let community = &fresh;
    let owner = community.owner()?;

    // OWNER-ONLY send: the receive counterpart (`advance_scope`) honors ONLY the
    // owner's rotation, so a non-owner BAN-holder's Refounding would fork onto a root
    // nobody follows and fail to sever the target. A non-owner's ban still silences
    // (Banlist) + strips authority (Grant); the read-cut is the owner's action alone
    // (CORD-06 §3 partial-removal degradation). Owner ⊃ BAN (supreme), so this is the
    // stricter gate.
    if me.public_key() != owner {
        return Err("only the owner can re-found (the cryptographic read-cut)".to_string());
    }

    // Fold the current roster: the opened editions are reused for the compaction (their
    // seals re-wrap under the new epoch), and the roster gates which admin-authored
    // heads carry forward.
    let floors: Floors = crate::db::community::get_all_edition_heads_full(&cid_hex)?
        .into_iter()
        .filter(|(_, f)| f.0 == community.root_epoch.0)
        .map(|(entity, f)| (entity, (f.1, f.2, f.3)))
        .collect();
    let current_control = control_group_key(&community.community_root, cid, community.root_epoch);
    let control_query = Query {
        kinds: vec![stream::KIND_WRAP],
        authors: vec![current_control.pk_hex()],
        limit: Some(FOLLOW_PAGE),
        ..Default::default()
    };
    let control_wraps = transport.fetch(&control_query, &community.relays).await?;
    let opened: Vec<(ParsedEdition, super::stream::OpenedStream)> =
        control_wraps.iter().filter_map(|w| control::open_control_edition(w, &current_control).ok()).collect();
    let editions: Vec<ParsedEdition> = opened.iter().map(|(e, _)| e.clone()).collect();
    let authority = fold_authority(community, &editions, &floors);

    let prev_epoch = community.root_epoch;
    let new_epoch = Epoch(prev_epoch.0.checked_add(1).ok_or("root epoch overflow")?);
    let prev_commit = super::derive::epoch_key_commitment(prev_epoch, &community.community_root);
    // Mint-or-REUSE the new root, keyed by (scope, new_epoch) and archived BEFORE any
    // publish: a retried Refounding re-delivers the SAME root at this epoch/address, so
    // it can't double-mint two roots a receiver's correlation dedup would collapse into
    // a permanent fork (CORD-06 §3 idempotency).
    let new_root = mint_or_reuse_rotation_key(&cid_hex, crate::community::SERVER_ROOT_SCOPE_HEX, new_epoch.0)?;
    let new_control = control_group_key(&new_root, cid, new_epoch);
    let at = now_ms();
    let at_secs = at / 1000;

    // ACQUIRE: re-wrap every current control HEAD under the new epoch (compaction,
    // O(entities)). A head not fetchable ABORTS — before anything is published.
    let control_fold = apply_control_fold(community, &editions, &floors, &authority);
    let mut carried: Vec<(FoldedHead, Event)> = Vec::new();
    for h in control_fold.heads.iter().chain(authority.heads.iter()) {
        let Some((_, os)) = opened.iter().find(|(e, _)| e.self_hash == h.self_hash) else {
            return Err("re-founding aborted: a control head is not fetchable (relay drop / flood); no state published".to_string());
        };
        let (rewrapped, _) = super::stream::rewrap_seal(&os.seal, &new_control, Timestamp::from_secs(at_secs)).map_err(|e| e.to_string())?;
        carried.push((h.clone(), rewrapped));
    }
    if !session.is_valid() {
        return Err("account changed during re-founding acquire".to_string());
    }

    // Recipients: the current members minus `removed`, plus me (multi-device).
    let members = memberlist(transport, community).await?;
    let removed_set: std::collections::HashSet<[u8; 32]> = removed.iter().map(|p| p.to_bytes()).collect();
    let mut recipients: Vec<PublicKey> = members.into_iter().filter(|m| !removed_set.contains(&m.to_bytes())).collect();
    if !recipients.iter().any(|p| *p == me.public_key()) {
        recipients.push(me.public_key());
    }

    // Base rekey blobs (the new root to each recipient), sealed under the PRIOR root.
    let mut base_blobs = Vec::new();
    for r in &recipients {
        base_blobs.push(
            super::rekey::build_blob_local(me.secret_key(), &me.public_key().to_bytes(), r, super::rekey::RekeyScope::Root, new_epoch, &new_root)
                .map_err(|e| e.to_string())?,
        );
    }
    let base_group = super::derive::base_rekey_group_key(&community.community_root, cid, new_epoch);
    let base_chunks =
        super::rekey::build_rekey_chunks_local(&me, &base_group, super::rekey::RekeyScope::Root, new_epoch, prev_epoch, &prev_commit, &base_blobs, at_secs)
            .map_err(|e| e.to_string())?;

    // Private-channel rekeys: each mints a fresh key at its next channel-epoch, sealed
    // under the PRIOR root (D2). Public channels ride the base — no per-channel rekey.
    let mut channel_updates: Vec<(ChannelId, [u8; 32], Epoch)> = Vec::new();
    let mut channel_chunk_sets: Vec<Vec<Event>> = Vec::new();
    for ch in &community.channels {
        let (Some(old_key), true) = (ch.key, ch.private) else { continue };
        let ch_new_epoch = Epoch(ch.epoch.0.checked_add(1).ok_or("channel epoch overflow")?);
        // Mint-or-reuse per channel too, keyed by (channel_id, next epoch) — same
        // retry-idempotency as the base root.
        let ch_new_key = mint_or_reuse_rotation_key(&cid_hex, &crate::simd::hex::bytes_to_hex_32(&ch.id.0), ch_new_epoch.0)?;
        let ch_prev_commit = super::derive::epoch_key_commitment(ch.epoch, &old_key);
        let mut ch_blobs = Vec::new();
        for r in &recipients {
            ch_blobs.push(
                super::rekey::build_blob_local(me.secret_key(), &me.public_key().to_bytes(), r, super::rekey::RekeyScope::Channel(ch.id), ch_new_epoch, &ch_new_key)
                    .map_err(|e| e.to_string())?,
            );
        }
        let ch_group = super::derive::channel_rekey_group_key(&community.community_root, &ch.id, ch_new_epoch);
        let ch_chunks = super::rekey::build_rekey_chunks_local(&me, &ch_group, super::rekey::RekeyScope::Channel(ch.id), ch_new_epoch, ch.epoch, &ch_prev_commit, &ch_blobs, at_secs)
            .map_err(|e| e.to_string())?;
        channel_updates.push((ch.id, ch_new_key, ch_new_epoch));
        channel_chunk_sets.push(ch_chunks);
    }
    if !session.is_valid() {
        return Err("account changed during re-founding prepare".to_string());
    }

    // COMMIT (durable publishes only — all fetching is done). Base rekey first
    // (delivers the new root), then channel rekeys, then the compacted control.
    for c in &base_chunks {
        transport.publish_durable(c, &community.relays).await?;
    }
    for set in &channel_chunk_sets {
        for c in set {
            transport.publish_durable(c, &community.relays).await?;
        }
    }
    for (_, wrap) in &carried {
        transport.publish_durable(wrap, &community.relays).await?;
    }
    // Guestbook snapshot at the new epoch — best-effort (a Refounding succeeds without
    // it; an omitted member heals by publishing their own Join).
    let gb_group = super::derive::guestbook_group_key(&new_root, cid, new_epoch);
    let snap_id = crate::community::random_32();
    for rumor in guestbook::build_snapshot_rumors(me.public_key(), &recipients, snap_id, at) {
        if let Ok((wrap, _)) = guestbook::seal_guestbook_rumor(&rumor, &gb_group, &me, Timestamp::from_secs(at_secs)) {
            let _ = transport.publish(&wrap, &community.relays).await;
        }
    }

    // COMMIT locally, only now that the new root + compacted plane are on relays.
    if !session.is_valid() {
        return Err("account changed during re-founding commit".to_string());
    }
    if crate::db::community::community_protocol(cid)?.is_none() {
        return Ok(community.clone()); // left/deleted mid-rotation — don't resurrect.
    }
    // Save the new root/epoch + rekeyed channel keys in ONE tx FIRST, so a crash can
    // never leave the base root advanced while the channel keys lag (which would
    // re-derive the channel rekey address under the wrong root and orphan them).
    let mut updated = community.clone();
    updated.community_root = new_root;
    updated.root_epoch = new_epoch;
    for (id, key, ep) in &channel_updates {
        if let Some(c) = updated.channels.iter_mut().find(|c| c.id.0 == id.0) {
            c.key = Some(*key);
            c.epoch = *ep;
        }
    }
    crate::db::community::save_community_v2(&updated)?;
    // Archive the new epoch key + confirm the monotonic base head (the root was already
    // archived by mint_or_reuse, so this is idempotent). Record the carried heads at
    // the NEW epoch; if a crash skips this, the epoch-filtered floors bootstrap the
    // compacted control on the next follow, so they self-heal.
    crate::db::community::advance_server_root_epoch(&cid_hex, new_epoch.0, &new_root)?;
    for (h, _) in &carried {
        crate::db::community::set_edition_head_at_epoch(&cid_hex, &h.entity_hex, h.version, &h.self_hash, &h.inner_id, new_epoch.0)?;
    }
    // Refresh any live public links so their bundles carry the NEW root behind the
    // same URL (a link shared once survives the rotation, CORD-05 §2). Best-effort.
    let _ = refresh_public_links(transport, &updated).await;
    Ok(updated)
}

/// Mint a fresh 32-byte rotation key for `(scope, new_epoch)`, or REUSE the one
/// already archived from a prior (aborted) attempt — so a retried Refounding re-
/// delivers the SAME key at the same epoch/address instead of double-minting two roots
/// a receiver's correlation dedup would collapse into a permanent fork (CORD-06 §3
/// idempotency). Archived BEFORE the first publish; `scope` is the all-zero server-root
/// sentinel for a base rotation, else the channel_id hex.
fn mint_or_reuse_rotation_key(community_id_hex: &str, scope_hex: &str, new_epoch: u64) -> Result<[u8; 32], String> {
    if let Some(existing) = crate::db::community::held_epoch_key(community_id_hex, scope_hex, new_epoch)? {
        return Ok(existing);
    }
    let fresh = crate::community::random_32();
    crate::db::community::store_epoch_key(community_id_hex, scope_hex, new_epoch, &fresh)?;
    Ok(fresh)
}

// ── The Community List (kind 13302, CORD-02 §8) ──────────────────────────────

/// This community's MEMBERSHIP subset for the 13302 list (CORD-02 §8): never the
/// icon (a rehydrating device folds it from the Control Plane), never the link
/// fields. Only PRIVATE channel keys ride — public channels derive from the root.
fn join_material(community: &CommunityV2) -> super::list::JoinMaterial {
    let hex = crate::simd::hex::bytes_to_hex_32;
    let channels = community
        .channels
        .iter()
        .filter(|c| c.private)
        .filter_map(|c| {
            c.key.map(|k| super::list::ChannelKeyRef { id: hex(&c.id.0), key: hex(&k), epoch: c.epoch.0, name: c.name.clone() })
        })
        .collect();
    super::list::JoinMaterial {
        community_id: hex(&community.identity.community_id.0),
        owner: hex(&community.identity.owner_xonly),
        owner_salt: hex(&community.identity.owner_salt),
        community_root: hex(&community.community_root),
        root_epoch: community.root_epoch.0,
        channels,
        relays: community.relays.clone(),
        name: community.name.clone(),
        extra: Default::default(),
    }
}

/// Rebuild an invite bundle from list join material, for a cross-device rehydrate
/// (the material IS the membership subset of a bundle). The owner root is still
/// verified over the network before the community is trusted (accept_bundle).
fn material_to_invite(jm: &super::list::JoinMaterial) -> CommunityInvite {
    let channels = jm
        .channels
        .iter()
        .map(|c| invite::ChannelGrant { id: c.id.clone(), key: c.key.clone(), epoch: c.epoch, name: c.name.clone() })
        .collect();
    CommunityInvite {
        community_id: jm.community_id.clone(),
        owner: jm.owner.clone(),
        owner_salt: jm.owner_salt.clone(),
        community_root: jm.community_root.clone(),
        root_epoch: jm.root_epoch,
        channels,
        relays: jm.relays.clone(),
        name: jm.name.clone(),
        icon: None,
        expires_at: None,
        creator_npub: None,
        label: None,
        extra: Default::default(),
    }
}

/// The union of every held v2 community's relays — where this account's 13302 list
/// lives (a fresh device that opens any held community reaches the same set).
fn held_v2_relays() -> Vec<String> {
    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Ok(ids) = crate::db::community::list_community_ids() {
        for id in ids {
            if matches!(crate::db::community::community_protocol(&id), Ok(Some(crate::community::ConcordProtocol::V2))) {
                if let Ok(Some(c)) = crate::db::community::load_community_v2(&id) {
                    set.extend(c.relays);
                }
            }
        }
    }
    set.into_iter().collect()
}

/// Fetch this account's own 13302 Community List from `relays` (the newest wins;
/// a decrypt/parse failure is "no news", never a clobber of the local mirror).
async fn fetch_community_list<T: Transport + ?Sized>(transport: &T, relays: &[String]) -> Option<super::list::CommunityList> {
    let me = local_keys().ok()?;
    let query = Query {
        kinds: vec![super::kind::COMMUNITY_LIST],
        authors: vec![me.public_key().to_hex()],
        limit: Some(4),
        ..Default::default()
    };
    let events = transport.fetch(&query, relays).await.ok()?;
    events
        .into_iter()
        .filter_map(|e| super::list::parse_list_event(&e, &me).ok().map(|l| (e.created_at.as_secs(), l)))
        .max_by_key(|(at, _)| *at)
        .map(|(_, l)| l)
}

/// Rebuild this account's 13302 from its held v2 communities, MERGE with the remote
/// copy (preserving tombstones, other-device entries, unknown fields), and publish.
/// Idempotent; called after any membership change. Best-effort — a list-publish
/// failure never fails the membership change itself.
pub async fn republish_community_list<T: Transport + ?Sized>(transport: &T) -> Result<(), String> {
    let session = SessionGuard::capture();
    let me = local_keys()?;
    let relays = held_v2_relays();
    if relays.is_empty() {
        return Ok(()); // nothing held → nothing to sync
    }
    let remote = fetch_community_list(transport, &relays).await.unwrap_or_default();
    let now = now_ms();
    let mut local = super::list::CommunityList::default();
    for id in crate::db::community::list_community_ids()? {
        if !matches!(crate::db::community::community_protocol(&id), Ok(Some(crate::community::ConcordProtocol::V2))) {
            continue;
        }
        let Some(c) = crate::db::community::load_community_v2(&id)? else { continue };
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&c.id().0);
        // Keep an already-live entry's add time (no churn); a new or previously-left
        // (now re-held) community adds/resurrects at `now`.
        let added_at = if remote.is_live(&cid_hex) {
            remote.entries.iter().find(|e| e.community_id == cid_hex).map(|e| e.added_at).unwrap_or(now)
        } else {
            now
        };
        let jm = join_material(&c);
        local.entries.push(super::list::CommunityListEntry { community_id: cid_hex, seed: jm.clone(), current: jm, added_at, extra: Default::default() });
    }
    let merged = remote.merge(&local);
    merged.assert_fits().map_err(|e| e.to_string())?;
    let event = super::list::build_list_event(&me, &merged).map_err(|e| e.to_string())?;
    if !session.is_valid() {
        return Err("account changed during community-list publish".to_string());
    }
    transport.publish(&event, &relays).await
}

/// Record a permanent leave tombstone for `community_id` in the 13302, published to
/// `relays` (the leaving community's own, since it's about to be deleted locally).
async fn tombstone_community_list<T: Transport + ?Sized>(transport: &T, community_id: &crate::community::CommunityId, relays: &[String]) -> Result<(), String> {
    let me = local_keys()?;
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community_id.0);
    let mut doc = fetch_community_list(transport, relays).await.unwrap_or_default();
    let now = now_ms();
    doc.tombstones.retain(|t| t.community_id != cid_hex);
    doc.tombstones.push(super::list::Tombstone { community_id: cid_hex, removed_at: now, extra: Default::default() });
    doc.assert_fits().map_err(|e| e.to_string())?;
    let event = super::list::build_list_event(&me, &doc).map_err(|e| e.to_string())?;
    transport.publish(&event, relays).await
}

/// Sync memberships from the 13302 across devices: fetch this account's list from
/// `bootstrap_relays` (its held communities' relays plus any caller-supplied set for
/// a fresh device), and JOIN every live entry not already held — reconstructing the
/// community from its join material and re-verifying the owner root. Returns the
/// newly-rehydrated communities (so the caller can subscribe + notify).
pub async fn sync_community_list<T: Transport + ?Sized>(transport: &T, bootstrap_relays: &[String]) -> Result<Vec<CommunityV2>, String> {
    let session = SessionGuard::capture();
    let mut relays = held_v2_relays();
    relays.extend(bootstrap_relays.iter().cloned());
    relays.sort();
    relays.dedup();
    if relays.is_empty() {
        return Ok(vec![]);
    }
    let Some(list) = fetch_community_list(transport, &relays).await else {
        return Ok(vec![]);
    };
    let mut joined = Vec::new();
    for entry in list.live_entries() {
        let Some(cid) = crate::simd::hex::hex_to_bytes_32_checked(&entry.community_id) else { continue };
        if crate::db::community::load_community_v2(&crate::community::CommunityId(cid)).ok().flatten().is_some() {
            continue; // already held
        }
        if !session.is_valid() {
            return Err("account changed during community-list sync".to_string());
        }
        // The material IS a bundle; accept_bundle re-verifies the owner root, saves,
        // seeds floors, and announces our Join (idempotent for an existing member).
        let bundle = material_to_invite(&entry.current);
        if let Ok(community) = accept_bundle(transport, &session, &bundle, None).await {
            joined.push(community);
        }
    }
    Ok(joined)
}

// ── Control edition authoring (CORD-04 roles / CORD-02 §6 / CORD-03 §2) ──────

/// Publish one control edition (a role, grant, banlist, community-metadata, or
/// channel-metadata edit) at the next version for its entity, chaining `prev` from
/// our held head, and advance our local floor. Authority is enforced by every
/// reader's roster fold (CORD-04 §5: authority is rejection, not prevention), so this
/// requires only a valid local signer; a well-behaved client checks its own rank
/// first, but a reader drops an unauthorized edition regardless.
async fn publish_control_edition<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    session: &SessionGuard,
    vsk: &str,
    entity_id: &[u8; 32],
    content: &str,
    citation: Option<&crate::community::edition::AuthorityCitation>,
) -> Result<(), String> {
    let me = local_keys()?;
    let control = control_group_key(&community.community_root, community.id(), community.root_epoch);
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let entity_hex = crate::simd::hex::bytes_to_hex_32(entity_id);
    let (version, prev) = match crate::db::community::get_edition_head(&cid_hex, &entity_hex)? {
        Some((v, h)) => (v + 1, Some(h)),
        None => (1, None),
    };
    let at = now_ms() / 1000;
    let rumor = control::build_edition_rumor(me.public_key(), vsk, entity_id, version, prev.as_ref(), content, at, citation);
    let (wrap, _) = control::seal_control_edition(&rumor, &control, &me, Timestamp::from_secs(at)).map_err(|e| e.to_string())?;
    if !session.is_valid() {
        return Err("account changed before control publish".to_string());
    }
    transport.publish(&wrap, &community.relays).await?;
    // Advance our own floor so a follow-up edit chains from this head and refuse-
    // downgrade holds; open our own wrap to recover the self_hash + inner_id.
    if let Ok((ed, _)) = control::open_control_edition(&wrap, &control) {
        crate::db::community::set_edition_head_at_epoch(&cid_hex, &entity_hex, ed.version, &ed.self_hash, &ed.inner_id, community.root_epoch.0)?;
    }
    Ok(())
}

/// Create or edit a Role (vsk 1, CORD-04 §2). `role.role_id` is the coordinate; a
/// rename or permission change is a versioned edit of the same id. Gated on the
/// reader side by `MANAGE_ROLES` + outrank.
pub async fn set_role<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, role: &crate::community::roles::Role) -> Result<(), String> {
    let session = SessionGuard::capture();
    super::roles::validate_role(role)?;
    let content = super::roles::role_content_json(role)?;
    let role_id = crate::simd::hex::hex_to_bytes_32_checked(&role.role_id).ok_or("role_id must be 32-byte hex")?;
    publish_control_edition(transport, community, &session, vsk::ROLE, &role_id, &content, None).await
}

/// Grant or revoke a member's Roles (vsk 3, CORD-04 §2). Empty `role_ids` is a
/// revoke. Gated on the reader side by `MANAGE_ROLES` + outrank of every role + the
/// member.
pub async fn grant_roles<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, member: &PublicKey, role_ids: Vec<String>) -> Result<(), String> {
    let session = SessionGuard::capture();
    let grant = crate::community::roles::MemberGrant { member: member.to_hex(), role_ids };
    let content = super::roles::grant_content_json(&grant)?;
    let eid = super::derive::grant_locator(community.id(), &member.to_bytes());
    publish_control_edition(transport, community, &session, vsk::GRANT, &eid, &content, None).await
}

/// Replace the Banlist (vsk 4, CORD-04 §4) with `banned` (lowercase-hex npubs), the
/// whole list on every edit. Gated on the reader side by `BAN`.
pub async fn set_banlist<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, banned: &[String]) -> Result<(), String> {
    let session = SessionGuard::capture();
    super::roles::validate_banlist(banned)?;
    let content = super::roles::banlist_content_json(banned)?;
    let eid = super::derive::banlist_locator(community.id());
    publish_control_edition(transport, community, &session, vsk::BANLIST, &eid, &content, None).await
}

/// Edit the community metadata (vsk 0, CORD-02 §6). Gated on the reader side by
/// `MANAGE_METADATA`.
pub async fn edit_community_metadata<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, meta: &control::CommunityMetadata) -> Result<(), String> {
    let session = SessionGuard::capture();
    control::validate_community_metadata(meta).map_err(|e| e.to_string())?;
    let content = serde_json::to_string(meta).map_err(|e| e.to_string())?;
    publish_control_edition(transport, community, &session, vsk::COMMUNITY_METADATA, &community.id().0, &content, None).await
}

/// Add or edit a channel's metadata (vsk 2, CORD-03 §2). `channel_id` is the
/// coordinate. Gated on the reader side by `MANAGE_CHANNELS`.
pub async fn edit_channel_metadata<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, channel_id: &ChannelId, meta: &control::ChannelMetadata) -> Result<(), String> {
    let session = SessionGuard::capture();
    control::validate_channel_metadata(meta).map_err(|e| e.to_string())?;
    let content = serde_json::to_string(meta).map_err(|e| e.to_string())?;
    publish_control_edition(transport, community, &session, vsk::CHANNEL_METADATA, &channel_id.0, &content, None).await
}

// ── Live control-follow (CORD-02 §6 / CORD-03 §2) ────────────────────────────

/// Re-fold this community's Control Plane and apply the current metadata +
/// **public** channel set to the held community, persisting any change. Called
/// when a control-plane wrap arrives in realtime (a rename, a new channel, an
/// edited description) so a long-running bot tracks the community mid-session
/// instead of freezing at its join-time view.
///
/// **Authority (CORD-04 §5):** the roster (roles/grants/banlist) folds first into
/// the owner-seeded authorized set ([`fold_authority`]), then each metadata/channel
/// edition is eligible only if its signer CURRENTLY holds the entity's management
/// bit (`MANAGE_METADATA`/`MANAGE_CHANNELS`) — so an authorized admin's edits fold,
/// a demoted one's drop. The owner is supreme, proven by the self-certifying
/// community_id (no network trust).
///
/// **Private channels are skipped here:** a Private channel's Chat-Plane key is
/// delivered over the rekey plane (or an invite bundle), never derivable from a
/// control edition alone. A new Private channel therefore surfaces only once
/// [`follow_rekeys`] delivers its key. Public channels derive from the
/// community_root, so they fold in directly.
///
/// Returns the updated community iff something changed (so the caller can skip a
/// redundant re-subscribe + refresh notification).
pub async fn follow_control<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    session: &SessionGuard,
) -> Result<Option<CommunityV2>, String> {
    community.owner()?; // fail fast if the community is somehow unproven.
    let control = control_group_key(&community.community_root, community.id(), community.root_epoch);
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);

    // Per-entity refuse-downgrade floors for the CURRENT epoch only. A head recorded
    // under a prior epoch is excluded, so that entity auto-bootstraps after a
    // Refounding (Armada accepts a compacted head across a dangling prev — matched).
    // A read error FAILS CLOSED: an empty map would silently re-open the rollback
    // window the floor exists to shut.
    let floors: Floors = crate::db::community::get_all_edition_heads_full(&cid_hex)?
        .into_iter()
        .filter(|(_, f)| f.0 == community.root_epoch.0)
        .map(|(entity, f)| (entity, (f.1, f.2, f.3)))
        .collect();

    // Newest window first; page OLDER only while a tracking entity is gapped (its
    // floor link evicted from the window — H1/M8 refetch), bounded like the join
    // verifier. A withholding relay still converges to fail-closed after the cap.
    let mut editions: Vec<ParsedEdition> = Vec::new();
    let mut seen: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
    let mut seen_wraps: std::collections::HashSet<nostr_sdk::EventId> = std::collections::HashSet::new();
    let mut oldest: Option<u64> = None;
    let mut until: Option<u64> = None;
    let mut fold = ControlFold { updated: None, heads: Vec::new(), gapped: false };
    let mut authority = AuthoritySet::owner_only();
    for _ in 0..FOLLOW_MAX_PAGES {
        let query = Query {
            kinds: vec![stream::KIND_WRAP],
            authors: vec![control.pk_hex()],
            until,
            limit: Some(FOLLOW_PAGE),
            ..Default::default()
        };
        let wraps = transport.fetch(&query, &community.relays).await?;
        // The `until` cursor is INCLUSIVE (a `-1` step can skip same-second siblings
        // at a page boundary); the wrap-id dedup makes re-served boundary events
        // free, and a page with nothing new means the relay is exhausted.
        let mut fresh = 0usize;
        for w in &wraps {
            if !seen_wraps.insert(w.id) {
                continue;
            }
            fresh += 1;
            let at = w.created_at.as_secs();
            if oldest.is_none_or(|o| at < o) {
                oldest = Some(at);
            }
            // Open + seal-verify every edition; authority is resolved by the roster
            // fold (CORD-04 §5), not by a signer filter here — an admin's edits fold.
            if let Ok((ed, _)) = control::open_control_edition(w, &control) {
                if seen.insert(ed.inner_id) {
                    editions.push(ed);
                }
            }
        }
        // Roster first (roles/grants/banlist → authorized set), then the authority-
        // gated metadata/channel fold over the same edition set.
        authority = fold_authority(community, &editions, &floors);
        fold = apply_control_fold(community, &editions, &floors, &authority);
        if !(fold.gapped || authority.gapped) || fresh == 0 {
            break;
        }
        until = oldest;
    }

    // The fetches straddled awaits; a swap since the guard was captured must not
    // write account A's control state into B.
    if !session.is_valid() {
        return Err("account changed during control follow".to_string());
    }
    // A leave/delete raced this follow: writing now would resurrect the community
    // row and orphan floor rows past delete_community's wipe.
    if crate::db::community::community_protocol(community.id())?.is_none() {
        return Ok(None);
    }
    // Persist advanced floors BEFORE the state save (a failed floor write must not
    // let saved state outrun its floor), stamping the epoch this fold ran under —
    // not the row's write-time value, which a concurrent re-founding can bump. Both
    // the metadata/channel heads and the roster/banlist heads advance their floors;
    // run the advance (v+1) and same-version convergence (fork tiebreak) paths.
    for h in fold.heads.iter().chain(authority.heads.iter()) {
        crate::db::community::set_edition_head_at_epoch(&cid_hex, &h.entity_hex, h.version, &h.self_hash, &h.inner_id, community.root_epoch.0)?;
        crate::db::community::converge_edition_head_at_epoch(&cid_hex, &h.entity_hex, h.version, &h.self_hash, &h.inner_id, community.root_epoch.0)?;
    }
    // Persist the authorized banlist content (retained/withholding folds carry None,
    // so the stored banlist is left intact — an anti-roster never silently un-bans).
    if let Some((banned, version)) = &authority.banlist_persist {
        crate::db::community::set_community_banlist(&cid_hex, banned, *version as i64)?;
    }
    match fold.updated {
        Some(u) => {
            crate::db::community::save_community_v2(&u)?;
            Ok(Some(u))
        }
        None => Ok(None),
    }
}

/// Control-follow paging bounds: enough depth to re-anchor a long-offline floor
/// (H1/M8 refetch) without letting a flooding relay stall the follow queue.
const FOLLOW_MAX_PAGES: usize = 4;
const FOLLOW_PAGE: usize = 500;

/// A folded control head to persist as the per-entity refuse-downgrade floor.
#[derive(Clone)]
struct FoldedHead {
    entity_hex: String,
    version: u64,
    self_hash: [u8; 32],
    inner_id: [u8; 32],
}

/// The outcome of a floor-aware control fold: the updated community (if content
/// changed), the heads to persist as the new floor (returned even when content is
/// unchanged, so the floor still seeds/advances), and whether any TRACKING entity
/// hit an unresolvable gap — the caller's signal to page older history and re-fold
/// (CORD-04 H1/M8's refetch).
struct ControlFold {
    updated: Option<CommunityV2>,
    heads: Vec<FoldedHead>,
    gapped: bool,
}

/// Per-entity floor: `(version, self_hash, inner_id)` of the committed head.
type Floors = std::collections::HashMap<String, (u64, [u8; 32], Option<[u8; 32]>)>;

/// Fold owner-authored control editions into an updated community using the
/// PERSISTED per-entity version floor (refuse-downgrade). Per entity, fold with
/// [`version::fold`]`(floor, floor_hash)`:
///   - ANCHORED: adopt the chain-verified head. A `gap` ABOVE it (withheld middles)
///     doesn't block the verified prefix — refuse-downgrade holds for everything
///     applied — but flags `gapped` so the caller pages for the rest.
///   - UNANCHORED under a held floor: one legitimate cause is a same-version owner
///     fork AT the floor whose deterministic winner (lower inner id; a NULL held id
///     is always replaceable, mirroring v1's `decide()`) isn't our held edition —
///     the floor CONVERGES to the winner and the chain re-anchors on it, so every
///     client lands on the same head where a hash-strict floor would wedge forever.
///     Anything else is withholding → fail closed + `gapped`.
///   - BOOTSTRAPPING (`floor == 0` — a fresh joiner, or a fresh epoch after a
///     Refounding, since the caller epoch-filters the floor) takes the highest
///     signed head (author already owner-filtered).
/// This matches CORD-04 §1 and mirrors v1's `fold_roster`. Epoch-filtering makes a
/// compaction at a new epoch auto-bootstrap, converging with Armada's acceptance of
/// a compacted head across a dangling `prev` (Armada doesn't persist a floor, so a
/// Vector floor only makes Vector STRICTER locally — no wire change, honest-case
/// convergence preserved).
fn apply_control_fold(community: &CommunityV2, editions: &[ParsedEdition], floors: &Floors, authority: &AuthoritySet) -> ControlFold {
    use crate::community::roles::Permissions;
    use std::collections::BTreeMap;

    let owner_hex = community.owner().ok().map(|o| o.to_hex());

    let mut groups: BTreeMap<(String, [u8; 32]), Vec<&ParsedEdition>> = BTreeMap::new();
    for e in editions {
        groups.entry((e.vsk.clone(), e.entity_id)).or_default().push(e);
    }

    let mut out = community.clone();
    let mut changed = false;
    let mut heads = Vec::new();
    let mut gapped = false;
    for ((vsk_code, eid), group) in &groups {
        // This fold applies exactly two entities: community metadata (eid ==
        // community_id) and channel metadata. A vsk-2 whose eid equals the community
        // id is excluded — the floor row keys on the entity alone, so it would share
        // (and corrupt) the metadata chain's floor.
        let is_meta = vsk_code == vsk::COMMUNITY_METADATA && *eid == community.id().0;
        let is_channel = vsk_code == vsk::CHANNEL_METADATA && *eid != community.id().0;
        if !is_meta && !is_channel {
            continue;
        }
        // Authority gate (CORD-04 §5): only editions whose author CURRENTLY holds the
        // entity's management bit are eligible. Pre-filtering before the fold means a
        // demoted admin's (possibly higher-version) edition can't be the head; the
        // highest AUTHORIZED head wins. The owner is supreme.
        let required = if is_meta { Permissions::MANAGE_METADATA } else { Permissions::MANAGE_CHANNELS };
        let authed: Vec<&ParsedEdition> = group
            .iter()
            .copied()
            .filter(|e| {
                let author = e.author.to_hex();
                // A banned npub's edits are dropped (CORD-04 §4), even if they still
                // held a bit via a not-yet-stripped grant.
                !authority.banned.contains(&author) && authority.roles.is_authorized(&author, owner_hex.as_deref(), required)
            })
            .collect();
        if authed.is_empty() {
            continue;
        }
        let entity_hex = crate::simd::hex::bytes_to_hex_32(eid);
        let fold_eds: Vec<version::Edition> = authed.iter().map(|p| p.to_fold_edition()).collect();
        let (hi, entity_gapped) = fold_head(&fold_eds, floors.get(&entity_hex));
        gapped |= entity_gapped;
        let Some(hi) = hi else { continue };

        let head = authed[hi];
        heads.push(FoldedHead { entity_hex, version: head.version, self_hash: head.self_hash, inner_id: head.inner_id });
        if is_meta {
            if let Ok(meta) = serde_json::from_str::<control::CommunityMetadata>(&head.content) {
                changed |= apply_community_metadata(&mut out, meta);
            }
        } else if let Ok(meta) = serde_json::from_str::<control::ChannelMetadata>(&head.content) {
            // vsk-2 carries no community binding (shared v1 grammar); a same-owner
            // cross-community replay can inject a phantom PUBLIC channel (bounded:
            // root-scoped key, eids don't collide). Binding is a deferred wire change.
            changed |= apply_channel_metadata(&mut out, ChannelId(*eid), meta);
        }
    }
    ControlFold { updated: changed.then_some(out), heads, gapped }
}

/// Fold one entity's editions against its persisted floor into a head index (into the
/// input slice) plus whether a TRACKING gap was hit (the caller pages older history).
/// Encapsulates the W2 refuse-downgrade policy: bootstrap at floor 0 (highest signed
/// head, what Armada shows across a compaction's dangling prev); adopt the chain-
/// anchored head, paging on an upper gap; converge a same-version fork at the floor to
/// the lower-inner-id winner; and fail closed otherwise.
fn fold_head(fold_eds: &[version::Edition], floor: Option<&(u64, [u8; 32], Option<[u8; 32]>)>) -> (Option<usize>, bool) {
    let floor_v = floor.map(|f| f.0).unwrap_or(0);
    if floor_v == 0 {
        return (version::bootstrap_head(fold_eds, 0), false);
    }
    let floor_hash = floor.map(|f| &f.1);
    let held_inner = floor.and_then(|f| f.2);
    let result = version::fold(fold_eds, floor_v, floor_hash);
    if result.anchored {
        return (result.head, result.gap); // verified prefix; page any upper gap.
    }
    if result.head.is_none() && !result.gap {
        return (None, false); // everything below floor — a stale relay, no paging.
    }
    // Unanchored under a held floor: converge a same-version fork at the floor to its
    // deterministic winner (lower inner id; a NULL held id is always replaceable),
    // else fail closed.
    let fork = fold_eds.iter().enumerate().filter(|(_, e)| e.version == floor_v).min_by_key(|(_, e)| e.tiebreak_id);
    let win_hash = match fork {
        Some((_, w)) if floor_hash != Some(&w.self_hash) && held_inner.is_none_or(|h| w.tiebreak_id < h) => w.self_hash,
        _ => return (None, true), // detached from our committed head → withholding.
    };
    let re = version::fold(fold_eds, floor_v, Some(&win_hash));
    if !re.anchored {
        return (None, true);
    }
    (re.head, re.gap)
}

/// The folded, delegation-AUTHORIZED control-plane authority (CORD-04): the roster
/// (roles + grants, owner-seeded fixpoint), the enforced banlist, and the
/// role/grant/banlist heads to persist as refuse-downgrade floors. The owner is
/// recomputed from the self-certifying community_id at each use.
struct AuthoritySet {
    roles: crate::community::roles::CommunityRoles,
    banned: std::collections::BTreeSet<String>,
    heads: Vec<FoldedHead>,
    gapped: bool,
    /// The authorized banlist `(content, version)` to persist when an authorized head
    /// advanced the floor. `None` when the banlist was retained (no new authorized
    /// head) or is empty — the caller then leaves the stored banlist untouched.
    banlist_persist: Option<(Vec<String>, u64)>,
}

impl AuthoritySet {
    /// Bootstrap authority for a community with no roster editions folded yet: only
    /// the owner is authorized (supreme), nobody banned.
    fn owner_only() -> Self {
        AuthoritySet { roles: Default::default(), banned: Default::default(), heads: vec![], gapped: false, banlist_persist: None }
    }
}

/// Fold the roster/banlist entities (vsk 1/3/4) from the control editions into the
/// delegation-AUTHORIZED roster + enforced banlist (CORD-04 §2-§5). Each entity binds
/// to its coordinate (role at role_id, grant at grant_locator(cid, member), banlist at
/// banlist_locator(cid)); a content whose coordinate doesn't match is dropped. Roles
/// cap at the 100 lowest role_ids, a member at 64 roles, the banlist at 500. The
/// banlist is enforced only if its head's signer held BAN in the authorized roster.
fn fold_authority(community: &CommunityV2, editions: &[ParsedEdition], floors: &Floors) -> AuthoritySet {
    use crate::community::roles::{CommunityRoles, MemberGrant, Permissions, Role};
    use crate::community::roster::{authorize_delegation, FoldedRoster};
    use std::collections::BTreeMap;

    let cid = community.id();
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&cid.0);
    let owner = community.owner().ok();
    let owner_hex = owner.map(|o| o.to_hex());
    let banlist_eid = super::derive::banlist_locator(cid);
    let banlist_hex = crate::simd::hex::bytes_to_hex_32(&banlist_eid);

    let mut groups: BTreeMap<[u8; 32], Vec<&ParsedEdition>> = BTreeMap::new();
    for e in editions {
        if e.vsk == vsk::ROLE || e.vsk == vsk::GRANT || e.vsk == vsk::BANLIST {
            groups.entry(e.entity_id).or_default().push(e);
        }
    }

    let mut roles: Vec<Role> = Vec::new();
    let mut role_authors: Vec<PublicKey> = Vec::new();
    let mut grants: Vec<MemberGrant> = Vec::new();
    let mut grant_authors: Vec<PublicKey> = Vec::new();
    let mut heads: Vec<FoldedHead> = Vec::new();
    let mut gapped = false;

    for (eid, group) in &groups {
        // The banlist is folded author-aware AFTER the roster is known (below) — not
        // here, where head selection is author-blind.
        if *eid == banlist_eid {
            continue;
        }
        let entity_hex = crate::simd::hex::bytes_to_hex_32(eid);
        let fold_eds: Vec<version::Edition> = group.iter().map(|p| p.to_fold_edition()).collect();
        let (hi, entity_gapped) = fold_head(&fold_eds, floors.get(&entity_hex));
        gapped |= entity_gapped;
        let Some(hi) = hi else { continue };
        let head = group[hi];
        let folded_head = FoldedHead { entity_hex: entity_hex.clone(), version: head.version, self_hash: head.self_hash, inner_id: head.inner_id };

        match head.vsk.as_str() {
            vsk::ROLE => {
                // Bind: the content's role_id IS the coordinate; position 0 is the owner's.
                if let Some(role) = super::roles::parse_role_content(&head.content) {
                    if role.role_id == entity_hex && role.position != 0 {
                        roles.push(role);
                        role_authors.push(head.author);
                        heads.push(folded_head);
                    }
                }
            }
            vsk::GRANT => {
                if let Some(mut grant) = super::roles::parse_grant_content(&head.content) {
                    if let Some(member) = crate::simd::hex::hex_to_bytes_32_checked(&grant.member) {
                        if super::derive::grant_locator(cid, &member) == *eid {
                            grant.role_ids.truncate(super::roles::MAX_ROLES_PER_MEMBER);
                            grants.push(grant);
                            grant_authors.push(head.author);
                            heads.push(folded_head);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Cap the community at the 100 lowest role_ids (deterministic), keeping the
    // author list aligned.
    if roles.len() > super::roles::MAX_ROLES_PER_COMMUNITY {
        let mut order: Vec<usize> = (0..roles.len()).collect();
        order.sort_by(|&a, &b| roles[a].role_id.cmp(&roles[b].role_id));
        let keep: std::collections::HashSet<usize> = order.into_iter().take(super::roles::MAX_ROLES_PER_COMMUNITY).collect();
        let (mut kr, mut ka) = (Vec::new(), Vec::new());
        for (i, (r, a)) in roles.into_iter().zip(role_authors).enumerate() {
            if keep.contains(&i) {
                kr.push(r);
                ka.push(a);
            }
        }
        roles = kr;
        role_authors = ka;
    }

    // Delegation-authorize a role/grant set (owner-seeded fixpoint). The unused
    // FoldedRoster fields (metadata/channel/registry) are empty — authorize_delegation
    // reads only roles + role_authors + grant_authors.
    let authorize = |roles: Vec<Role>, role_authors: Vec<PublicKey>, grants: Vec<MemberGrant>, grant_authors: Vec<PublicKey>| {
        let folded = FoldedRoster {
            roles: CommunityRoles { roles, grants },
            role_authors,
            grant_authors,
            gapped_entities: vec![],
            skipped: 0,
            fetched: 0,
            heads: vec![],
            banned: vec![],
            banlist_author: None,
            dissolved_by: vec![],
            banlist_head: None,
            invite_link_sets: vec![],
            root_meta: None,
            root_author: None,
            root_head: None,
            root_candidates: vec![],
            channel_meta: vec![],
            channel_candidates: vec![],
        };
        authorize_delegation(&folded, owner_hex.as_deref())
    };

    // Preliminary roster (bans not yet applied) — the authority view the banlist head
    // is judged against.
    let prelim = authorize(roles.clone(), role_authors.clone(), grants.clone(), grant_authors.clone());

    // Banlist (CORD-04 §4), folded AUTHORITY-aware so its two anti-roster hazards are
    // both closed:
    //   - head selection: the head is the highest version whose author CURRENTLY holds
    //     BAN — an unauthorized higher-version edition can't erase existing bans
    //     (fail-open), and the floor never advances to one;
    //   - per-target: each entry is kept only if the author STRICTLY OUTRANKS that
    //     target (`can_act_on_member` — an admin can't ban a peer/superior, and the
    //     owner is unbannable);
    //   - withholding: when no authorized head is served, the persisted banlist is
    //     RETAINED (an anti-roster must not un-ban on a relay withholding the ban).
    let persisted_banned: Vec<String> = crate::db::community::get_community_banlist(&cid_hex).unwrap_or_default();
    let banlist_authored: Vec<&ParsedEdition> = groups
        .get(&banlist_eid)
        .map(|g| {
            g.iter()
                .copied()
                .filter(|e| prelim.is_authorized(&e.author.to_hex(), owner_hex.as_deref(), Permissions::BAN))
                .collect()
        })
        .unwrap_or_default();
    let mut banlist_persist: Option<(Vec<String>, u64)> = None;
    let banned: std::collections::BTreeSet<String> = if banlist_authored.is_empty() {
        persisted_banned.into_iter().collect()
    } else {
        let fold_eds: Vec<version::Edition> = banlist_authored.iter().map(|p| p.to_fold_edition()).collect();
        let (hi, g) = fold_head(&fold_eds, floors.get(&banlist_hex));
        gapped |= g;
        match hi {
            Some(hi) => {
                let head = banlist_authored[hi];
                let ah = head.author.to_hex();
                let list: Vec<String> = super::roles::parse_banlist_content(&head.content)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|t| prelim.can_act_on_member(&ah, owner_hex.as_deref(), t, Permissions::BAN))
                    .take(super::roles::MAX_BANLIST)
                    .collect();
                heads.push(FoldedHead { entity_hex: banlist_hex.clone(), version: head.version, self_hash: head.self_hash, inner_id: head.inner_id });
                banlist_persist = Some((list.clone(), head.version));
                list.into_iter().collect()
            }
            None => persisted_banned.into_iter().collect(),
        }
    };

    // CORD-04 §4: a banned npub vanishes — every event from them is dropped. Re-
    // authorize with editions AUTHORED by a banned npub removed, and grants TO a
    // banned member removed (they hold no rank), so a banned admin loses authority.
    let authorized = if banned.is_empty() {
        prelim
    } else {
        let (mut fr, mut fra) = (Vec::new(), Vec::new());
        for (r, a) in roles.into_iter().zip(role_authors) {
            if !banned.contains(&a.to_hex()) {
                fr.push(r);
                fra.push(a);
            }
        }
        let (mut fg, mut fga) = (Vec::new(), Vec::new());
        for (g, a) in grants.into_iter().zip(grant_authors) {
            if !banned.contains(&a.to_hex()) && !banned.contains(&g.member) {
                fg.push(g);
                fga.push(a);
            }
        }
        authorize(fr, fra, fg, fga)
    };

    AuthoritySet { roles: authorized, banned, heads, gapped, banlist_persist }
}

/// Apply a folded community-metadata head. Relays only overwrite when the edition
/// carries a non-empty list (a metadata edition that omits relays must not blank
/// the working set). Returns whether anything changed.
fn apply_community_metadata(out: &mut CommunityV2, meta: control::CommunityMetadata) -> bool {
    let mut changed = false;
    if out.name != meta.name {
        out.name = meta.name;
        changed = true;
    }
    if out.description != meta.description {
        out.description = meta.description;
        changed = true;
    }
    if !meta.relays.is_empty() && out.relays != meta.relays {
        out.relays = meta.relays;
        changed = true;
    }
    changed
}

/// Apply a folded channel-metadata head: delete removes the channel, a rename
/// updates an existing one, and a brand-new PUBLIC channel is added (a new Private
/// channel is skipped — its key arrives over the rekey plane). Returns whether
/// anything changed.
fn apply_channel_metadata(out: &mut CommunityV2, id: ChannelId, meta: control::ChannelMetadata) -> bool {
    let deleted = meta.deleted.unwrap_or(false);
    if deleted {
        let before = out.channels.len();
        out.channels.retain(|c| c.id.0 != id.0);
        return out.channels.len() != before;
    }
    match out.channels.iter_mut().find(|c| c.id.0 == id.0) {
        Some(existing) => {
            let mut changed = false;
            if existing.name != meta.name {
                existing.name = meta.name;
                changed = true;
            }
            // The owner's edition authoritatively declares visibility. A channel the
            // owner marks PUBLIC must derive from the root (key = None) — this heals a
            // bundle-time misclassification where an attacker set a public channel's
            // grant key to their own, silently addressing it at a plane only they read.
            // (public -> private needs a rekey-delivered key, so it's left to rekey.)
            if !meta.private && (existing.private || existing.key.is_some()) {
                existing.private = false;
                existing.key = None;
                changed = true;
            }
            changed
        }
        None if !meta.private => {
            // A public channel derives its Chat Plane from the community_root at the
            // current root epoch (key = None); its stored epoch mirrors the root.
            out.channels.push(ChannelV2 {
                id,
                name: meta.name,
                private: false,
                key: None,
                epoch: out.root_epoch,
            });
            true
        }
        None => false, // a new Private channel — deferred to rekey key delivery.
    }
}

// ── Live rekey-follow (CORD-06 §2/§3) ────────────────────────────────────────

/// The outcome of a rekey-follow pass.
pub struct RekeyFollow {
    /// The community after adopting every rotation it could catch up on, or `None`
    /// if nothing advanced.
    pub updated: Option<CommunityV2>,
    /// A base rotation removed us — the caller tears the local hold down (the
    /// updated community is not persisted in that case).
    pub self_removed: bool,
}

/// Follow rekeys for a held community: advance the base (root) epoch and each
/// Private channel's epoch as far as owner-authored rotations allow, adopting the
/// fresh key we're still a recipient of at each step and dropping a scope we've
/// been removed from. Persists the result. Called when a rekey wrap arrives in
/// realtime so a long-running bot keeps decrypting after a rotation instead of
/// going silent.
///
/// **Authority (first cut): owner-authored rotations only** (`rotator == owner`).
/// The roster fold that would honour an admin's `BAN`/`MANAGE_CHANNELS` rotation
/// is deferred — a safe under-approximation (never adopts a non-owner's key, which
/// a malicious member could otherwise use to fork the follower onto a key only
/// they know). A legitimate admin-driven channel rekey is simply not followed yet.
///
/// **Continuity + fork resolution are spec-strict:** only a rotation whose
/// `prevcommit` extends the exact `(epoch, key)` I hold advances me, one epoch at
/// a time; a same-epoch owner fork resolves by the lexicographically lowest new
/// key ([`rekey::lowest_key_winner`]), so every follower converges. An incomplete
/// rotation (a missing chunk) never concludes removal — it just waits.
///
/// **First-cut limitation (documented):** no prior-epoch read archive. Adopting a
/// rotation moves the scope's read coordinate forward; history published under the
/// old epoch is not re-fetched afterwards (a live follower already received it).
/// The multi-epoch read archive layers on with the GUI history work.
pub async fn follow_rekeys<T: Transport + ?Sized>(
    transport: &T,
    community: &CommunityV2,
    session: &SessionGuard,
) -> Result<RekeyFollow, String> {
    let me = local_keys()?;
    let my_xonly = me.public_key().to_bytes();
    let owner = community.owner()?;
    let mut cur = community.clone();
    let mut changed = false;

    // Bound the catch-up: each real step consumes a valid owner rotation, so a
    // finite chain terminates naturally; the cap defends against a relay feeding a
    // pathological set.
    const MAX_STEPS: usize = 128;
    for _ in 0..MAX_STEPS {
        let mut advanced = false;

        // Private channels first: a removal-forced channel rekey can ride the PRIOR
        // root (CORD-06 D2), so read channels under the current root before a base
        // adopt moves it.
        let channel_ids: Vec<ChannelId> = cur.channels.iter().filter(|c| c.private).map(|c| c.id).collect();
        for cid in channel_ids {
            let (held_key, held_epoch) = match cur.channel(&cid) {
                Some(ch) => match ch.key {
                    Some(k) => (k, ch.epoch),
                    None => continue, // a keyless private channel can't be advanced.
                },
                None => continue,
            };
            let next = Epoch(held_epoch.0.saturating_add(1));
            let group = channel_rekey_group_key(&cur.community_root, &cid, next);
            let chunks = fetch_rekey_chunks(transport, &cur.relays, &group).await?;
            match advance_scope(&chunks, RekeyScope::Channel(cid), &owner, me.secret_key(), &my_xonly, held_epoch, &held_key, next) {
                Advance::Adopt { new_key } => {
                    if let Some(ch) = cur.channels.iter_mut().find(|c| c.id.0 == cid.0) {
                        ch.key = Some(new_key);
                        ch.epoch = next;
                    }
                    advanced = true;
                    changed = true;
                }
                Advance::Removed => {
                    cur.channels.retain(|c| c.id.0 != cid.0);
                    advanced = true;
                    changed = true;
                }
                Advance::Stay => {}
            }
        }

        // Base rotation (Refounding): advances the root + root_epoch, re-addressing
        // every public channel, the guestbook, and the control plane by derivation
        // (refresh_subscription recomputes the author-set from the new root).
        {
            let held_epoch = cur.root_epoch;
            let held_key = cur.community_root;
            let next = Epoch(held_epoch.0.saturating_add(1));
            let group = base_rekey_group_key(&cur.community_root, cur.id(), next);
            let chunks = fetch_rekey_chunks(transport, &cur.relays, &group).await?;
            match advance_scope(&chunks, RekeyScope::Root, &owner, me.secret_key(), &my_xonly, held_epoch, &held_key, next) {
                Advance::Adopt { new_key } => {
                    cur.community_root = new_key;
                    cur.root_epoch = next;
                    advanced = true;
                    changed = true;
                }
                Advance::Removed => {
                    if !session.is_valid() {
                        return Err("account changed during rekey follow".to_string());
                    }
                    return Ok(RekeyFollow { updated: None, self_removed: true });
                }
                Advance::Stay => {}
            }
        }

        if !advanced {
            break;
        }
    }

    if !changed {
        return Ok(RekeyFollow { updated: None, self_removed: false });
    }
    if !session.is_valid() {
        return Err("account changed during rekey follow".to_string());
    }
    // A leave/delete raced this follow: saving would resurrect the community row
    // (the save is an upsert) with no floor rows behind it.
    if crate::db::community::community_protocol(community.id())?.is_none() {
        return Ok(RekeyFollow { updated: None, self_removed: false });
    }
    crate::db::community::save_community_v2(&cur)?;
    Ok(RekeyFollow { updated: Some(cur), self_removed: false })
}

/// One scope's catch-up decision from the rekey chunks fetched at its next-epoch
/// address.
enum Advance {
    /// Adopt this fresh key for `next_epoch`.
    Adopt { new_key: [u8; 32] },
    /// A complete owner rotation at `next_epoch` dropped my blob — I'm removed.
    Removed,
    /// No owner rotation extends my held epoch (yet) — keep the current key.
    Stay,
}

/// Fetch + parse every seal-verified 3303 chunk at a rekey plane address.
async fn fetch_rekey_chunks<T: Transport + ?Sized>(
    transport: &T,
    relays: &[String],
    group: &GroupKey,
) -> Result<Vec<rekey::RekeyChunk>, String> {
    let query = Query {
        kinds: vec![stream::KIND_WRAP],
        authors: vec![group.pk_hex()],
        limit: Some(200),
        ..Default::default()
    };
    let wraps = transport.fetch(&query, relays).await?;
    let mut out = Vec::new();
    for w in &wraps {
        if let Ok(opened) = stream::open_wrap(w, group) {
            if let Ok(chunk) = rekey::parse_rekey_chunk(&opened) {
                out.push(chunk);
            }
        }
    }
    Ok(out)
}

/// Decide how a scope advances from a batch of rekey chunks (pure). Considers only
/// owner-authored, complete rotations of this scope that extend the held
/// `(epoch, key)` and target the immediate `next_epoch` (so a same-epoch fork
/// resolves among candidates at one continuity point). Among those, a rotation
/// carrying my blob yields a candidate key; the lexicographically lowest wins
/// (convergent). If every complete candidate dropped my blob, I'm removed; if none
/// qualifies, stay.
#[allow(clippy::too_many_arguments)]
fn advance_scope(
    chunks: &[rekey::RekeyChunk],
    scope: RekeyScope,
    owner: &PublicKey,
    my_sk: &SecretKey,
    my_xonly: &[u8; 32],
    held_epoch: Epoch,
    held_key: &[u8; 32],
    next_epoch: Epoch,
) -> Advance {
    let rotations = rekey::collect_rotations(chunks);
    let mut winners: Vec<[u8; 32]> = Vec::new();
    let mut saw_complete_candidate = false;
    for r in &rotations {
        if r.rotator != *owner
            || r.scope.id32() != scope.id32()
            || r.new_epoch.0 != next_epoch.0
            || r.continuity(held_epoch, held_key) != Continuity::Extends
            || !r.is_complete()
        {
            continue;
        }
        saw_complete_candidate = true;
        if let Some(blob) = rekey::find_my_blob(&r.blobs, &r.rotator.to_bytes(), my_xonly, r.scope, r.new_epoch) {
            if let Ok(k) = rekey::open_blob_local(my_sk, &r.rotator, r.scope, r.new_epoch, blob) {
                winners.push(k);
            }
        }
    }
    if !winners.is_empty() {
        // `collect_rotations` correlates on `(rotator, scope, new_epoch, prev_commit)`,
        // so a single rotator's blobs merge into ONE rotation (and a retried Refounding
        // MINT-OR-REUSES its root, so it never emits two distinct roots to fork on).
        // The lowest-key tiebreak engages only for CONCURRENT DISTINCT refounders (two
        // rotators racing the same epoch — different `rotator`, so separate rotations):
        // every follower converges on the same lowest new key.
        let idx = rekey::lowest_key_winner(&winners).expect("winners is non-empty");
        return Advance::Adopt { new_key: winners[idx] };
    }
    if saw_complete_candidate {
        Advance::Removed
    } else {
        Advance::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::transport::memory::MemoryRelay;
    use super::*;
    use crate::community::roles::{MemberGrant, Permissions, Role, RoleScope};

    /// A distinct npub-shaped account-dir name (bech32 charset) per counter.
    fn account_name(n: u32) -> String {
        const B: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let mut acct = String::from("npub1");
        let mut v = n as usize;
        for _ in 0..58 {
            acct.push(B[v % 32] as char);
            v = v / 32 + 7;
        }
        acct
    }

    /// One test participant: its identity keys and its isolated account DB dir.
    struct Actor {
        keys: Keys,
        account: String,
    }

    /// Two participants sharing one relay but isolated per-account DBs — the
    /// cross-account harness a real invite/join loop needs. `swap_to` mirrors a
    /// live `swap_session`: re-point the DB pool + rebind the identity + clear
    /// the per-account id caches, so account A's community is invisible to B
    /// until B legitimately joins.
    struct TestBed {
        _tmp: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
        relay: MemoryRelay,
        relays: Vec<String>,
    }

    impl TestBed {
        fn new() -> (TestBed, Actor, Actor) {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(70_000);
            let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
            crate::db::close_database();
            crate::db::clear_id_caches();
            let tmp = tempfile::tempdir().unwrap();
            crate::db::set_app_data_dir(tmp.path().to_path_buf());

            let mut mk = || {
                let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let account = account_name(n);
                std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
                crate::db::set_current_account(account.clone()).unwrap();
                crate::db::init_database(&account).unwrap();
                Actor { keys: Keys::generate(), account }
            };
            let owner = mk();
            let member = mk();
            let _ = crate::state::take_nostr_client();
            let bed = TestBed {
                _tmp: tmp,
                _guard: guard,
                relay: MemoryRelay::new(),
                relays: vec!["wss://r".to_string()],
            };
            (bed, owner, member)
        }

        /// Become `actor`: swap the account DB + identity, as a real session swap.
        fn swap_to(&self, actor: &Actor) {
            crate::db::set_current_account(actor.account.clone()).unwrap();
            crate::db::init_database(&actor.account).unwrap();
            crate::db::clear_id_caches();
            crate::state::MY_SECRET_KEY.store_from_keys(&actor.keys, &[]);
            crate::state::set_my_public_key(actor.keys.public_key());
        }
    }

    /// Legacy single-actor helper (the create/send tests below).
    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>, Keys) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(50_000);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let acct = account_name(n);
        std::fs::create_dir_all(tmp.path().join(&acct)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(acct.clone()).unwrap();
        crate::db::init_database(&acct).unwrap();
        let _ = crate::state::take_nostr_client();
        let owner = Keys::generate();
        crate::state::MY_SECRET_KEY.store_from_keys(&owner, &[]);
        crate::state::set_my_public_key(owner.public_key());
        (tmp, guard, owner)
    }

    /// A transport that simulates a session swap landing DURING a fetch await —
    /// so a join straddling the fetch sees an invalid session and aborts.
    struct SwapMidFetch {
        inner: MemoryRelay,
    }
    #[async_trait::async_trait]
    impl Transport for SwapMidFetch {
        async fn publish(&self, e: &Event, r: &[String]) -> Result<(), String> {
            self.inner.publish(e, r).await
        }
        async fn publish_durable(&self, e: &Event, r: &[String]) -> Result<(), String> {
            self.inner.publish_durable(e, r).await
        }
        async fn fetch(&self, q: &Query, r: &[String]) -> Result<Vec<Event>, String> {
            let out = self.inner.fetch(q, r).await;
            crate::state::bump_session_generation();
            out
        }
    }

    /// A transport whose `fetch` returns a FIXED, UNSORTED event list — modelling
    /// the production `LiveTransport` union (first-responding relay's batch, no
    /// global newest-first sort), which `MemoryRelay` hides by sorting. This is
    /// the only harness that can exercise the revocation-race ordering.
    struct FixedFetch {
        events: Vec<Event>,
    }
    #[async_trait::async_trait]
    impl Transport for FixedFetch {
        async fn publish(&self, _e: &Event, _r: &[String]) -> Result<(), String> {
            Ok(())
        }
        async fn publish_durable(&self, _e: &Event, _r: &[String]) -> Result<(), String> {
            Ok(())
        }
        async fn fetch(&self, _q: &Query, _r: &[String]) -> Result<Vec<Event>, String> {
            Ok(self.events.clone())
        }
    }

    /// Fetch a pending Direct Invite (kind 3313 giftwrap) addressed to `me` — the
    /// indexed inbox query CORD-05 §6 defines: `{1059, #p:[me], #k:["3313"]}`.
    async fn fetch_direct_invite(relay: &MemoryRelay, relays: &[String], me: &PublicKey) -> Event {
        let q = Query {
            kinds: vec![stream::KIND_WRAP],
            p_tags: vec![me.to_hex()],
            k_tags: vec!["3313".to_string()],
            ..Default::default()
        };
        relay.fetch(&q, relays).await.unwrap().into_iter().next().expect("a direct invite is waiting")
    }

    #[tokio::test]
    async fn create_persists_and_reloads_a_v2_community() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let relays = vec!["wss://r".to_string()];

        let created = create_community(&relay, "Vectorville", relays.clone(), Some("hi".into())).await.unwrap();
        assert!(created.identity.verify());
        assert_eq!(created.owner().unwrap(), owner.public_key());
        assert_eq!(created.channels.len(), 1);

        // Protocol dispatch sees it as v2, and it reloads byte-faithfully.
        assert_eq!(
            crate::db::community::community_protocol(created.id()).unwrap(),
            Some(crate::community::ConcordProtocol::V2)
        );
        let loaded = crate::db::community::load_community_v2(created.id()).unwrap().expect("reloads");
        assert_eq!(loaded.name, "Vectorville");
        assert_eq!(loaded.community_root, created.community_root);
        assert_eq!(loaded.identity, created.identity);
        assert_eq!(loaded.channels[0].id.0, created.channels[0].id.0);
        assert!(!loaded.channels[0].private);

        // The genesis control editions + the owner Join landed on the relay.
        assert!(relay.count_on("wss://r") >= 3, "2 genesis editions + 1 guestbook join");
    }

    #[tokio::test]
    async fn owner_sends_and_reads_back_a_message() {
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Chat", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;

        let id1 = send_message(&relay, &community, &general, "hello world").await.unwrap();
        let id2 = send_message(&relay, &community, &general, "second message").await.unwrap();
        assert_ne!(id1, id2);

        let page = fetch_channel(&relay, &community, &general, 100).await.unwrap();
        let texts: Vec<String> = page
            .iter()
            .filter_map(|f| match &f.event {
                ChatEvent::Message { .. } => Some(f.event.opened().rumor.content.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["hello world", "second message"], "messages round-trip in ms order");
    }

    #[tokio::test]
    async fn a_second_member_reads_the_public_channel_from_the_root() {
        // A member who holds the community_root (via an invite bundle, modeled
        // here by cloning the community) reads the owner's public-channel message
        // — public channels need no key delivery, they derive from the root.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Public", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        send_message(&relay, &community, &general, "everyone can read this").await.unwrap();

        // The "member" reconstructs the same read coordinates from the root.
        let member_view = community.clone();
        let page = fetch_channel(&relay, &member_view, &general, 100).await.unwrap();
        assert_eq!(page.len(), 1);
        assert!(matches!(&page[0].event, ChatEvent::Message { .. }));
        assert_eq!(page[0].event.opened().rumor.content, "everyone can read this");
    }

    // ── Two-actor end-to-end (the create → invite → join → message loop) ──────

    async fn texts_in(relay: &MemoryRelay, community: &CommunityV2, channel: &ChannelId) -> Vec<String> {
        fetch_channel(relay, community, channel, 100)
            .await
            .unwrap()
            .iter()
            .filter_map(|f| match &f.event {
                ChatEvent::Message { .. } => Some(f.event.opened().rumor.content.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn direct_invite_full_loop_owner_and_member_converse() {
        let (bed, owner, member) = TestBed::new();

        // Owner creates a community, posts, and Direct-Invites the member's npub.
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Guild", bed.relays.clone(), None).await.unwrap();
        let general = community.channels[0].id;
        send_message(&bed.relay, &community, &general, "owner: welcome!").await.unwrap();
        send_direct_invite(&bed.relay, &community, &member.keys.public_key(), None, None).await.unwrap();

        // Member (a DIFFERENT account, no prior knowledge) finds + accepts the invite.
        bed.swap_to(&member);
        assert!(
            crate::db::community::load_community_v2(community.id()).unwrap().is_none(),
            "the member does not hold the community before joining"
        );
        let invite_wrap = fetch_direct_invite(&bed.relay, &bed.relays, &member.keys.public_key()).await;
        let joined = accept_direct_invite(&bed.relay, &invite_wrap).await.unwrap();
        assert_eq!(joined.id().0, community.id().0, "joined the same community");
        assert!(joined.identity.verify(), "the joiner independently verifies the owner commitment");
        assert_eq!(joined.owner().unwrap(), owner.keys.public_key());

        // The member reads the owner's public-channel history and replies.
        assert_eq!(texts_in(&bed.relay, &joined, &general).await, vec!["owner: welcome!"]);
        send_message(&bed.relay, &joined, &general, "member: thanks for the invite").await.unwrap();

        // The owner reads the member's reply.
        bed.swap_to(&owner);
        assert_eq!(
            texts_in(&bed.relay, &community, &general).await,
            vec!["owner: welcome!", "member: thanks for the invite"],
            "both actors' messages interleave in ms order on the shared channel"
        );

        // The Guestbook memberlist now folds both participants.
        let members = memberlist(&bed.relay, &community).await.unwrap();
        assert!(members.contains(&owner.keys.public_key()), "owner is a member (genesis Join)");
        assert!(members.contains(&member.keys.public_key()), "member is a member (invite Join)");
        assert_eq!(members.len(), 2);
    }

    #[tokio::test]
    async fn public_link_full_loop() {
        let (bed, owner, member) = TestBed::new();

        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Public Guild", bed.relays.clone(), None).await.unwrap();
        let general = community.channels[0].id;
        send_message(&bed.relay, &community, &general, "come on in").await.unwrap();
        // Mint a shareable link (a non-stock relay so the fragment carries it).
        let link = mint_public_link(&bed.relay, &community, "https://vectorapp.io", None, None).await.unwrap();
        assert!(link.url.starts_with("https://vectorapp.io/invite/"));
        assert!(link.url.contains('#'), "the fragment carries the token");

        // Member joins purely from the URL string.
        bed.swap_to(&member);
        let joined = accept_public_link(&bed.relay, &link.url).await.unwrap();
        assert_eq!(joined.id().0, community.id().0);
        assert_eq!(texts_in(&bed.relay, &joined, &general).await, vec!["come on in"]);
    }

    #[tokio::test]
    async fn a_revoked_link_refuses_to_join() {
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Revoked", bed.relays.clone(), None).await.unwrap();
        let link = mint_public_link(&bed.relay, &community, "https://vectorapp.io", None, None).await.unwrap();
        // Owner retires the link (re-posts the coordinate as a tombstone).
        let tombstone = invite::build_revocation(&link.link_signer).unwrap();
        bed.relay.publish_durable(&tombstone, &bed.relays).await.unwrap();

        bed.swap_to(&member);
        let err = accept_public_link(&bed.relay, &link.url).await.unwrap_err();
        assert!(err.contains("revoked"), "a retired link finds the grave, not keys: {err}");
    }

    #[tokio::test]
    async fn an_expired_direct_invite_refuses_to_join() {
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Expired", bed.relays.clone(), None).await.unwrap();
        // Hand-mint an invite that expired in the past.
        let inviter = owner.keys.clone();
        let mut bundle = bundle_of(&community, Some(inviter.public_key()), Some(1_000), None);
        bundle.expires_at = Some(1_000); // unix ms, long past
        let wrap = invite::build_direct_invite(&inviter, &member.keys.public_key(), &bundle).unwrap();
        bed.relay.publish(&wrap, &bed.relays).await.unwrap();

        bed.swap_to(&member);
        let invite_wrap = fetch_direct_invite(&bed.relay, &bed.relays, &member.keys.public_key()).await;
        let err = accept_direct_invite(&bed.relay, &invite_wrap).await.unwrap_err();
        assert!(err.contains("expired"), "a past-expiry invite refuses to join: {err}");
    }

    #[tokio::test]
    async fn a_tombstone_beats_a_live_bundle_regardless_of_fetch_order() {
        // The revocation-durability fix: if ANY signer-valid tombstone is among the
        // fetched events, refuse — even when a Live bundle is returned FIRST (the
        // production union has no newest-first sort, so a stale relay's Live can lead).
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Rev", bed.relays.clone(), None).await.unwrap();
        let link = mint_public_link(&bed.relay, &community, "https://vectorapp.io", None, None).await.unwrap();
        let tombstone = invite::build_revocation(&link.link_signer).unwrap();

        // A relay union that hands back [Live, tombstone] — Live FIRST. Old
        // `events.first()` would join the Live; the scan-all fix must refuse.
        let union = FixedFetch { events: vec![link.bundle_event.clone(), tombstone] };

        bed.swap_to(&member);
        let err = accept_public_link(&union, &link.url).await.unwrap_err();
        assert!(err.contains("revoked"), "a tombstone must beat a Live returned first: {err}");
    }

    #[test]
    fn from_bundle_refuses_an_over_cap_bundle_before_allocating() {
        // The accept-side DoS bound: from_bundle (which accept_bundle calls)
        // rejects a >256-channel bundle via validate() BEFORE the Vec allocation.
        // (The Direct-Invite wire path is additionally bounded by NIP-44's 64KB
        // cap, which trips even earlier — but the count guard is the real defense
        // for the single-layer public-link bundle.)
        let owner = Keys::generate();
        let identity = super::super::control::CommunityIdentity::mint(&owner.public_key());
        let hex = crate::simd::hex::bytes_to_hex_32;
        let root = [0x11u8; 32];
        let mut bundle = CommunityInvite {
            community_id: hex(&identity.community_id.0),
            owner: hex(&identity.owner_xonly),
            owner_salt: hex(&identity.owner_salt),
            community_root: hex(&root),
            root_epoch: 0,
            channels: vec![],
            relays: vec!["wss://r".into()],
            name: "X".into(),
            icon: None,
            expires_at: None,
            creator_npub: None,
            label: None,
            extra: Default::default(),
        };
        bundle.channels = (0..=invite::MAX_BUNDLE_CHANNELS)
            .map(|i| {
                let mut id = [0u8; 32];
                id[..8].copy_from_slice(&(i as u64).to_be_bytes());
                invite::ChannelGrant { id: hex(&id), key: hex(&root), epoch: 0, name: "x".into() }
            })
            .collect();
        assert!(CommunityV2::from_bundle(&bundle, 0).is_err(), "an over-cap bundle is refused before allocating");
    }

    #[tokio::test]
    async fn a_join_swap_between_fetch_and_save_aborts_and_leaves_the_other_account_clean() {
        // The SessionGuard straddle: a public-link accept fetches then saves. If the
        // account swaps in that window, the join must abort — never write A's
        // community into B's DB. SwapMidFetch bumps the session generation during
        // the fetch await, exactly as a real swap_session would.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Straddle", bed.relays.clone(), None).await.unwrap();
        let link = mint_public_link(&bed.relay, &community, "https://vectorapp.io", None, None).await.unwrap();
        // A fresh swap-injecting transport holding the same bundle event.
        let swap_relay = SwapMidFetch { inner: MemoryRelay::new() };
        swap_relay.inner.publish_durable(&link.bundle_event, &bed.relays).await.unwrap();

        bed.swap_to(&member);
        let err = accept_public_link(&swap_relay, &link.url).await.unwrap_err();
        assert!(err.contains("account changed"), "a swap mid-join must abort: {err}");
        assert!(
            crate::db::community::load_community_v2(community.id()).unwrap().is_none(),
            "the aborted join wrote nothing to the (member) account DB"
        );
    }

    #[tokio::test]
    async fn the_owner_is_a_member_even_without_a_fetched_genesis_join() {
        // The owner is derived from the self-certifying community_id, so the
        // memberlist includes them independent of any Guestbook fetch.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Owned", vec!["wss://r".into()], None).await.unwrap();
        // A memberlist over an EMPTY guestbook (fetch a community-relay-less view)
        // still contains the owner.
        let empty = MemoryRelay::new();
        let members = memberlist(&empty, &community).await.unwrap();
        assert_eq!(members, vec![owner.public_key()], "owner present with no fetched Join");
    }

    #[tokio::test]
    async fn an_expiring_minted_invite_refuses_after_the_deadline() {
        // The mint path can now produce an expiring invite, and the accept gate
        // trips on it (end-to-end through the real service, not a hand-built bundle).
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Timed", bed.relays.clone(), None).await.unwrap();
        send_direct_invite(&bed.relay, &community, &member.keys.public_key(), Some(1_000), Some("beta".into()))
            .await
            .unwrap();

        bed.swap_to(&member);
        let invite_wrap = fetch_direct_invite(&bed.relay, &bed.relays, &member.keys.public_key()).await;
        assert!(
            accept_direct_invite(&bed.relay, &invite_wrap).await.unwrap_err().contains("expired"),
            "a minted expiring invite refuses past its deadline"
        );
    }

    #[tokio::test]
    async fn a_member_who_leaves_drops_from_the_memberlist() {
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Leaving", bed.relays.clone(), None).await.unwrap();
        send_direct_invite(&bed.relay, &community, &member.keys.public_key(), None, None).await.unwrap();

        bed.swap_to(&member);
        let invite_wrap = fetch_direct_invite(&bed.relay, &bed.relays, &member.keys.public_key()).await;
        let joined = accept_direct_invite(&bed.relay, &invite_wrap).await.unwrap();
        // Let the leave land strictly after the join.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        leave_community(&bed.relay, &joined).await.unwrap();

        bed.swap_to(&owner);
        let members = memberlist(&bed.relay, &community).await.unwrap();
        assert!(members.contains(&owner.keys.public_key()));
        assert!(!members.contains(&member.keys.public_key()), "a member who left drops from the list");
    }

    #[tokio::test]
    async fn a_swapped_member_cannot_see_the_owners_community_until_joining() {
        // Multi-account isolation: after the swap, the member's DB holds nothing
        // of the owner's community — the dual-stack storage is per-account.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Private-so-far", bed.relays.clone(), None).await.unwrap();
        assert!(crate::db::community::load_community_v2(community.id()).unwrap().is_some());

        bed.swap_to(&member);
        assert!(
            crate::db::community::load_community_v2(community.id()).unwrap().is_none(),
            "the owner's community must be invisible in the member's account DB"
        );
        assert_eq!(crate::db::community::list_community_ids().unwrap().len(), 0);
    }

    // ── Live control-follow ──────────────────────────────────────────────────

    /// Publish an owner-grammar channel edition straight to the control plane,
    /// signed by `signer` (the owner for a legit edit, a stranger for the
    /// authority test). `version`/`deleted` drive add-vs-rename-vs-delete.
    /// The entity's current head `self_hash` on the relay (highest version wins),
    /// so a helper can chain a new edition the way a real owner client does.
    async fn head_hash_on_relay(relay: &MemoryRelay, community: &CommunityV2, entity_id: &[u8; 32]) -> Option<[u8; 32]> {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let query = Query { kinds: vec![stream::KIND_WRAP], authors: vec![group.pk_hex()], limit: Some(500), ..Default::default() };
        let wraps = relay.fetch(&query, &community.relays).await.ok()?;
        let mut head: Option<(u64, [u8; 32])> = None;
        for w in &wraps {
            if let Ok((ed, _)) = control::open_control_edition(w, &group) {
                if ed.entity_id == *entity_id && head.is_none_or(|(v, _)| ed.version > v) {
                    head = Some((ed.version, ed.self_hash));
                }
            }
        }
        head.map(|(_, h)| h)
    }

    async fn publish_channel_edition(
        relay: &MemoryRelay,
        community: &CommunityV2,
        signer: &Keys,
        channel_id: &ChannelId,
        name: &str,
        private: bool,
        version: u64,
        deleted: bool,
    ) {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let prev = head_hash_on_relay(relay, community, &channel_id.0).await;
        let meta = control::ChannelMetadata { name: name.into(), private, deleted: deleted.then_some(true), ..Default::default() };
        let content = serde_json::to_string(&meta).unwrap();
        let rumor = control::build_edition_rumor(signer.public_key(), vsk::CHANNEL_METADATA, &channel_id.0, version, prev.as_ref(), &content, 1_000, None);
        let (wrap, _) = control::seal_control_edition(&rumor, &group, signer, Timestamp::from_secs(1_000)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();
    }

    /// Publish an owner-grammar community-metadata edition (rename etc.), chained
    /// to the current relay head like a real owner client.
    async fn publish_community_meta(relay: &MemoryRelay, community: &CommunityV2, signer: &Keys, name: &str, version: u64) {
        publish_community_meta_at(relay, community, signer, name, version, 1_000).await;
    }

    /// As [`publish_community_meta`] with an explicit timestamp, for tests that need
    /// relay-side newest-first ordering (paging/eviction scenarios).
    async fn publish_community_meta_at(relay: &MemoryRelay, community: &CommunityV2, signer: &Keys, name: &str, version: u64, at_secs: u64) {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let prev = head_hash_on_relay(relay, community, &community.id().0).await;
        let meta = control::CommunityMetadata { name: name.into(), ..Default::default() };
        let content = serde_json::to_string(&meta).unwrap();
        let rumor = control::build_edition_rumor(signer.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, version, prev.as_ref(), &content, at_secs, None);
        let (wrap, _) = control::seal_control_edition(&rumor, &group, signer, Timestamp::from_secs(at_secs)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();
    }

    /// Publish a Role edition (vsk 1) signed by `signer`, chained to the current head.
    async fn publish_role(relay: &MemoryRelay, community: &CommunityV2, signer: &Keys, role: &Role, version: u64) {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let role_id = crate::simd::hex::hex_to_bytes_32_checked(&role.role_id).unwrap();
        let prev = head_hash_on_relay(relay, community, &role_id).await;
        let content = crate::community::v2::roles::role_content_json(role).unwrap();
        let rumor = control::build_edition_rumor(signer.public_key(), vsk::ROLE, &role_id, version, prev.as_ref(), &content, 1_000, None);
        let (wrap, _) = control::seal_control_edition(&rumor, &group, signer, Timestamp::from_secs(1_000)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();
    }

    /// Publish a Grant edition (vsk 3) signed by `signer`, at grant_locator(cid, member).
    async fn publish_grant(relay: &MemoryRelay, community: &CommunityV2, signer: &Keys, member: &PublicKey, role_ids: Vec<String>, version: u64) {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let eid = crate::community::v2::derive::grant_locator(community.id(), &member.to_bytes());
        let prev = head_hash_on_relay(relay, community, &eid).await;
        let grant = MemberGrant { member: member.to_hex(), role_ids };
        let content = crate::community::v2::roles::grant_content_json(&grant).unwrap();
        let rumor = control::build_edition_rumor(signer.public_key(), vsk::GRANT, &eid, version, prev.as_ref(), &content, 1_000, None);
        let (wrap, _) = control::seal_control_edition(&rumor, &group, signer, Timestamp::from_secs(1_000)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();
    }

    /// Publish a Banlist edition (vsk 4) signed by `signer`, at banlist_locator(cid).
    async fn publish_banlist(relay: &MemoryRelay, community: &CommunityV2, signer: &Keys, banned: &[String], version: u64) {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let eid = crate::community::v2::derive::banlist_locator(community.id());
        let prev = head_hash_on_relay(relay, community, &eid).await;
        let content = crate::community::v2::roles::banlist_content_json(banned).unwrap();
        let rumor = control::build_edition_rumor(signer.public_key(), vsk::BANLIST, &eid, version, prev.as_ref(), &content, 1_000, None);
        let (wrap, _) = control::seal_control_edition(&rumor, &group, signer, Timestamp::from_secs(1_000)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();
    }

    fn admin_role(role_id: &str, perms: u64) -> Role {
        Role { role_id: role_id.into(), name: "Admin".into(), position: 1, permissions: Permissions(perms), scope: RoleScope::Server, color: 0 }
    }

    #[tokio::test]
    async fn an_authorized_admin_edits_metadata_but_a_demoted_one_cannot() {
        // CORD-04 §5: an admin holding MANAGE_METADATA renames the community; once the
        // owner revokes the grant, the (now unauthorized) admin's further edit drops
        // and the name holds at the last authorized state.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Base", vec!["wss://r".into()], None).await.unwrap();
        let admin = Keys::generate();
        let rid = "a1".repeat(32);
        publish_role(&relay, &community, &owner, &admin_role(&rid, Permissions::MANAGE_METADATA), 1).await;
        publish_grant(&relay, &community, &owner, &admin.public_key(), vec![rid.clone()], 1).await;
        publish_community_meta(&relay, &community, &admin, "Admin Rename", 2).await;

        let session = SessionGuard::capture();
        let updated = follow_control(&relay, &community, &session).await.unwrap().expect("admin edit authorized");
        assert_eq!(updated.name, "Admin Rename", "an admin with MANAGE_METADATA renames");

        publish_grant(&relay, &community, &owner, &admin.public_key(), vec![], 2).await; // revoke
        publish_community_meta(&relay, &community, &admin, "Demoted Rename", 3).await;
        let _ = follow_control(&relay, &community, &session).await.unwrap();
        let held = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        assert_eq!(held.name, "Admin Rename", "a demoted admin's edit is dropped; the name holds");
    }

    #[tokio::test]
    async fn a_roleless_member_cannot_edit_metadata() {
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Guarded", vec!["wss://r".into()], None).await.unwrap();
        let stranger = Keys::generate();
        publish_community_meta(&relay, &community, &stranger, "Hijacked", 2).await;
        let session = SessionGuard::capture();
        assert!(
            follow_control(&relay, &community, &session).await.unwrap().is_none(),
            "a roleless member's metadata edit never folds"
        );
    }

    #[tokio::test]
    async fn a_self_signed_grant_is_not_authority() {
        // The self-promotion defense: a member self-signs both a role and a grant of
        // it to themselves. authorize_delegation drops both (their signer never traces
        // to the owner), so their metadata edit stays unauthorized.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "NoSelfPromo", vec!["wss://r".into()], None).await.unwrap();
        let rogue = Keys::generate();
        let rid = "b2".repeat(32);
        publish_role(&relay, &community, &rogue, &admin_role(&rid, Permissions::ADMIN_ALL), 1).await;
        publish_grant(&relay, &community, &rogue, &rogue.public_key(), vec![rid.clone()], 1).await;
        publish_community_meta(&relay, &community, &rogue, "Seized", 2).await;
        let session = SessionGuard::capture();
        assert!(
            follow_control(&relay, &community, &session).await.unwrap().is_none(),
            "a self-signed grant confers no authority"
        );
    }

    #[tokio::test]
    async fn the_banlist_is_enforced_only_from_a_ban_holder() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Bans", vec!["wss://r".into()], None).await.unwrap();
        let target = "cc".repeat(32);

        // A non-BAN-holder's banlist edition is folded but NOT enforced.
        let rogue = Keys::generate();
        publish_banlist(&relay, &community, &rogue, &[target.clone()], 1).await;
        let floors = load_floors(&community);
        let editions = fetch_control(&relay, &community).await;
        let authority = fold_authority(&community, &editions, &floors);
        assert!(authority.banned.is_empty(), "a non-owner (no BAN) banlist is not enforced");

        // The owner (supreme, holds BAN) bans the target: now enforced.
        publish_banlist(&relay, &community, &owner, &[target.clone()], 2).await;
        let editions = fetch_control(&relay, &community).await;
        let authority = fold_authority(&community, &editions, &floors);
        assert!(authority.banned.contains(&target), "the owner's banlist is enforced");
    }

    #[tokio::test]
    async fn a_banned_admin_loses_all_authority() {
        // CORD-04 §4: a banned npub vanishes — even holding an un-stripped grant, a
        // banned admin's authority is dropped and their edits refused.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "BanAuth", vec!["wss://r".into()], None).await.unwrap();
        let admin = Keys::generate();
        let rid = "e5".repeat(32);
        publish_role(&relay, &community, &owner, &admin_role(&rid, Permissions::MANAGE_METADATA), 1).await;
        publish_grant(&relay, &community, &owner, &admin.public_key(), vec![rid.clone()], 1).await;
        publish_banlist(&relay, &community, &owner, &[admin.public_key().to_hex()], 1).await; // ban, grant left intact
        publish_community_meta(&relay, &community, &admin, "Banned Rename", 2).await;

        let session = SessionGuard::capture();
        assert!(
            follow_control(&relay, &community, &session).await.unwrap().is_none(),
            "a banned admin's edit is dropped even with an unstripped grant"
        );
        let authority = fold_authority(&community, &fetch_control(&relay, &community).await, &load_floors(&community));
        assert!(authority.banned.contains(&admin.public_key().to_hex()));
        assert!(
            !authority.roles.is_authorized(&admin.public_key().to_hex(), Some(&owner.public_key().to_hex()), Permissions::MANAGE_METADATA),
            "a banned admin holds no bit"
        );
    }

    #[tokio::test]
    async fn a_ban_holder_cannot_ban_a_superior_or_the_owner() {
        // CORD-04 §3/§5: BAN needs the bit AND a strict outrank of the target. A mod
        // (pos 2, holds BAN) can ban a lower member but NOT a superior admin (pos 1)
        // and NOT the owner (supreme, unbannable).
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Ranks", vec!["wss://r".into()], None).await.unwrap();
        let admin = Keys::generate();
        let moder = Keys::generate();
        let stranger = Keys::generate();
        let (admin_rid, mod_rid) = ("a1".repeat(32), "b2".repeat(32));
        publish_role(&relay, &community, &owner, &Role { role_id: admin_rid.clone(), name: "Admin".into(), position: 1, permissions: Permissions(Permissions::ADMIN_ALL), scope: RoleScope::Server, color: 0 }, 1).await;
        publish_role(&relay, &community, &owner, &Role { role_id: mod_rid.clone(), name: "Mod".into(), position: 2, permissions: Permissions(Permissions::BAN), scope: RoleScope::Server, color: 0 }, 1).await;
        publish_grant(&relay, &community, &owner, &admin.public_key(), vec![admin_rid], 1).await;
        publish_grant(&relay, &community, &owner, &moder.public_key(), vec![mod_rid], 1).await;
        publish_banlist(&relay, &community, &moder, &[admin.public_key().to_hex(), owner.public_key().to_hex(), stranger.public_key().to_hex()], 1).await;

        let authority = fold_authority(&community, &fetch_control(&relay, &community).await, &load_floors(&community));
        assert!(!authority.banned.contains(&admin.public_key().to_hex()), "a mod cannot ban a superior admin");
        assert!(!authority.banned.contains(&owner.public_key().to_hex()), "nobody can ban the owner");
        assert!(authority.banned.contains(&stranger.public_key().to_hex()), "the mod CAN ban a lower-ranked member");
    }

    #[tokio::test]
    async fn an_unauthorized_higher_banlist_cannot_unban() {
        // CORD-04 §4 anti-roster fail-CLOSED: a rogue's higher-version empty banlist
        // must not erase the owner's ban (author-aware head selection + persisted
        // banlist retention).
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "NoUnban", vec!["wss://r".into()], None).await.unwrap();
        let target = "cc".repeat(32);
        publish_banlist(&relay, &community, &owner, &[target.clone()], 1).await;
        let session = SessionGuard::capture();
        follow_control(&relay, &community, &session).await.unwrap(); // persists the ban

        let rogue = Keys::generate();
        publish_banlist(&relay, &community, &rogue, &[], 2).await; // unauthorized higher, empty
        let authority = fold_authority(&community, &fetch_control(&relay, &community).await, &load_floors(&community));
        assert!(authority.banned.contains(&target), "an unauthorized higher banlist cannot un-ban");
    }

    #[tokio::test]
    async fn the_community_list_syncs_a_membership_to_a_fresh_device() {
        // CORD-02 §8: create publishes the 13302; a fresh device (community dropped
        // locally, the 13302 + genesis still on the relay) rehydrates it on sync.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let relays = vec!["wss://r".to_string()];
        let community = create_community(&relay, "Synced", relays.clone(), None).await.unwrap();
        crate::db::community::delete_community(&crate::simd::hex::bytes_to_hex_32(&community.id().0)).unwrap();
        assert!(crate::db::community::load_community_v2(community.id()).unwrap().is_none());

        let rehydrated = sync_community_list(&relay, &relays).await.unwrap();
        assert_eq!(rehydrated.len(), 1, "the left-behind membership rehydrates");
        assert_eq!(rehydrated[0].id().0, community.id().0);
        assert!(crate::db::community::load_community_v2(community.id()).unwrap().is_some(), "and is now held locally");
    }

    #[tokio::test]
    async fn a_leave_tombstones_the_membership_so_sync_does_not_rejoin() {
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let relays = vec!["wss://r".to_string()];
        let community = create_community(&relay, "Left", relays.clone(), None).await.unwrap();
        leave_community(&relay, &community).await.unwrap(); // tombstones the 13302 + deletes

        let rehydrated = sync_community_list(&relay, &relays).await.unwrap();
        assert!(rehydrated.is_empty(), "a tombstoned membership is not rejoined on sync");
    }

    #[tokio::test]
    async fn dissolution_blocks_a_join() {
        // CORD-02 §9: the owner dissolves; a would-be joiner resolves the grave and
        // refuses to join.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Doomed", bed.relays.clone(), None).await.unwrap();
        let bundle = bundle_of(&community, Some(owner.keys.public_key()), None, None);
        let bundle_json = serde_json::to_string(&bundle).unwrap();
        dissolve_community(&bed.relay, &community).await.unwrap();
        assert!(crate::db::community::load_community_v2(community.id()).unwrap().unwrap().dissolved, "the owner's local hold is sealed");

        bed.swap_to(&member);
        let err = accept_parked_invite(&bed.relay, &bundle_json, None).await.unwrap_err();
        assert!(err.contains("dissolved"), "a join refuses a dissolved community: {err}");
    }

    #[tokio::test]
    async fn only_the_owner_can_dissolve() {
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Mine", bed.relays.clone(), None).await.unwrap();
        bed.swap_to(&member);
        assert!(dissolve_community(&bed.relay, &community).await.is_err(), "only the owner can dissolve");
        assert!(!is_dissolved(&bed.relay, &community).await, "and no tombstone was published");
    }

    #[tokio::test]
    async fn a_foreign_tombstone_is_not_death() {
        // A non-owner sealing the dissolved plane is noise (verify_dissolved is
        // owner-gated), so the community is not treated as dead.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Safe", vec!["wss://r".into()], None).await.unwrap();
        let rogue = Keys::generate();
        let rumor = crate::community::v2::dissolution::dissolved_tombstone_rumor(rogue.public_key(), community.id(), 1_000);
        let wrap = crate::community::v2::dissolution::seal_dissolved(&rumor, community.id(), &rogue, Timestamp::from_secs(1_000)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();
        assert!(!is_dissolved(&relay, &community).await, "a foreign-signed tombstone is not death");
    }

    #[tokio::test]
    async fn a_public_channel_reads_history_across_a_refounding() {
        // CORD-03 §3: after a Refounding rolls the base root, a Public channel's
        // pre-rotation messages stay readable (the prior epoch's root is archived and
        // the read fans out across held epochs).
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "History", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        send_message(&relay, &community, &general, "before the refounding").await.unwrap();

        let refounded = refound_community(&relay, &community, &[]).await.unwrap();
        assert_eq!(refounded.root_epoch, Epoch(1), "the epoch advanced");
        send_message(&relay, &refounded, &general, "after the refounding").await.unwrap();

        let texts = texts_in(&relay, &refounded, &general).await;
        assert!(texts.contains(&"before the refounding".to_string()), "the epoch-0 message is still readable");
        assert!(texts.contains(&"after the refounding".to_string()), "the epoch-1 message reads too");
    }

    #[tokio::test]
    async fn refounding_rolls_the_root_and_severs_a_removed_member() {
        // CORD-06 §3: the owner re-founds, removing a member. The base root rolls, the
        // epoch advances, and the removed member's rekey-follow concludes they're cut.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Refound", bed.relays.clone(), None).await.unwrap();
        let bundle = bundle_of(&community, Some(owner.keys.public_key()), None, None);
        let bundle_json = serde_json::to_string(&bundle).unwrap();
        bed.swap_to(&member);
        let joined = accept_parked_invite(&bed.relay, &bundle_json, None).await.unwrap();

        bed.swap_to(&owner);
        let refounded = refound_community(&bed.relay, &community, &[member.keys.public_key()]).await.unwrap();
        assert_eq!(refounded.root_epoch, Epoch(1), "the epoch advanced");
        assert_ne!(refounded.community_root, community.community_root, "the base root rolled");
        // The owner still reads the compacted control plane at the new epoch.
        let session = SessionGuard::capture();
        assert_eq!(
            crate::db::community::load_community_v2(community.id()).unwrap().unwrap().root_epoch,
            Epoch(1),
            "the owner committed the new epoch"
        );

        // The removed member, following rekeys, is severed (no blob in the rotation).
        bed.swap_to(&member);
        let follow = follow_rekeys(&bed.relay, &joined, &session).await.unwrap();
        assert!(follow.self_removed, "the removed member is cut by the re-founding");
    }

    #[tokio::test]
    async fn only_the_owner_re_founds_even_a_ban_holding_admin_cannot() {
        // The receive side (advance_scope) honors ONLY the owner's rotation, so the
        // SEND is owner-only — a non-owner BAN-holder's Refounding would fork onto a
        // root nobody follows. Owner grants a member BAN; they still can't re-found.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Guarded", bed.relays.clone(), None).await.unwrap();
        let rid = "b0".repeat(32);
        publish_role(&bed.relay, &community, &owner.keys, &admin_role(&rid, Permissions::BAN), 1).await;
        publish_grant(&bed.relay, &community, &owner.keys, &member.keys.public_key(), vec![rid], 1).await;
        let bundle = bundle_of(&community, Some(owner.keys.public_key()), None, None);
        let bundle_json = serde_json::to_string(&bundle).unwrap();
        bed.swap_to(&member);
        let joined = accept_parked_invite(&bed.relay, &bundle_json, None).await.unwrap();
        assert!(refound_community(&bed.relay, &joined, &[owner.keys.public_key()]).await.is_err(), "a non-owner BAN-holder can't re-found");
    }

    #[tokio::test]
    async fn a_retried_refounding_reuses_the_same_root() {
        // B1 idempotency: minting for the same (scope, epoch) twice yields the SAME
        // root, so a retried Refounding re-delivers one root — never a double-mint fork.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Retry", vec!["wss://r".into()], None).await.unwrap();
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let first = mint_or_reuse_rotation_key(&cid_hex, crate::community::SERVER_ROOT_SCOPE_HEX, 1).unwrap();
        let second = mint_or_reuse_rotation_key(&cid_hex, crate::community::SERVER_ROOT_SCOPE_HEX, 1).unwrap();
        assert_eq!(first, second, "a retry reuses the archived root, never double-mints");
    }

    #[tokio::test]
    async fn minting_a_link_makes_the_community_public_and_revoke_makes_it_private() {
        // CORD-05 §5: the Registry is the Public/Private source of truth. Minting a
        // link publishes it (Public); retiring the last link empties it (Private).
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Invitable", vec!["wss://r".into()], None).await.unwrap();
        assert!(!community_is_public(&relay, &community).await, "a fresh community is Private");

        let minted = mint_public_link(&relay, &community, "https://x", None, None).await.unwrap();
        assert!(community_is_public(&relay, &community).await, "a live link makes it Public");
        let list = fetch_invite_list(&relay, &community.relays).await.expect("the 13303 list was published");
        assert_eq!(list.entries.len(), 1, "the minted link is recorded across devices");

        let token_hex = crate::simd::hex::bytes_to_hex_16(&minted.token);
        revoke_public_link(&relay, &community, &token_hex).await.unwrap();
        assert!(!community_is_public(&relay, &community).await, "retiring the last link makes it Private again");
        let after = fetch_invite_list(&relay, &community.relays).await.unwrap();
        assert!(after.entries.is_empty() && after.tombstones.len() == 1, "the link is tombstoned in the invite list");
    }

    #[tokio::test]
    async fn a_registry_from_a_non_create_invite_holder_does_not_make_it_public() {
        // The CREATE_INVITE gate: a rogue publishing a registry can't fake Public.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Gated", vec!["wss://r".into()], None).await.unwrap();
        let rogue = Keys::generate();
        // Rogue publishes a registry edition at THEIR coordinate with a fake signer.
        let eid = crate::community::v2::derive::invite_links_locator(community.id(), &rogue.public_key().to_bytes());
        let content = crate::community::v2::invite::build_registry_content(&[Keys::generate().public_key()]);
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let rumor = control::build_edition_rumor(rogue.public_key(), vsk::INVITE_LINKS, &eid, 1, None, &content, 1_000, None);
        let (wrap, _) = control::seal_control_edition(&rumor, &group, &rogue, Timestamp::from_secs(1_000)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();
        let _ = owner;
        assert!(!community_is_public(&relay, &community).await, "a non-CREATE_INVITE registry is ignored");
    }

    #[tokio::test]
    async fn full_lifecycle_e2e() {
        // The whole stack end to end across two accounts: create -> Public link ->
        // owner grants an admin -> member joins + reads history -> admin edits metadata
        // (authorized fold) -> owner bans the member (CORD-04 §6: banlist + strip +
        // Refounding) -> the banned member is severed AND stays banned across the new
        // epoch -> pre-ban history still reads -> owner dissolves -> sealed.
        let (bed, owner, member) = TestBed::new();

        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Lifecycle", bed.relays.clone(), None).await.unwrap();
        let general = community.channels[0].id;
        send_message(&bed.relay, &community, &general, "owner: welcome").await.unwrap();

        // Public link → the community reads Public.
        let _minted = mint_public_link(&bed.relay, &community, "https://x", None, None).await.unwrap();
        assert!(community_is_public(&bed.relay, &community).await, "a live link makes it Public");

        // Owner defines + grants an Admin role (MANAGE_METADATA among the bits).
        let rid = "aa".repeat(32);
        publish_role(&bed.relay, &community, &owner.keys, &admin_role(&rid, Permissions::ADMIN_ALL), 1).await;
        publish_grant(&bed.relay, &community, &owner.keys, &member.keys.public_key(), vec![rid], 1).await;

        // Member joins from the bundle + reads the owner's message.
        let bundle = bundle_of(&community, Some(owner.keys.public_key()), None, None);
        let bundle_json = serde_json::to_string(&bundle).unwrap();
        bed.swap_to(&member);
        let joined = accept_parked_invite(&bed.relay, &bundle_json, None).await.unwrap();
        assert_eq!(texts_in(&bed.relay, &joined, &general).await, vec!["owner: welcome"]);
        // The admin renames the community.
        publish_community_meta(&bed.relay, &joined, &member.keys, "Lifecycle Renamed", 2).await;

        // Owner follows: the admin's rename folds (authorized).
        bed.swap_to(&owner);
        let session = SessionGuard::capture();
        let updated = follow_control(&bed.relay, &community, &session).await.unwrap().expect("the admin edit folds");
        assert_eq!(updated.name, "Lifecycle Renamed", "an authorized admin's metadata edit is honored");

        // Ban the member (the three-removal composition, in order).
        set_banlist(&bed.relay, &updated, &[member.keys.public_key().to_hex()]).await.unwrap();
        grant_roles(&bed.relay, &updated, &member.keys.public_key(), vec![]).await.unwrap();
        let refounded = refound_community(&bed.relay, &updated, &[member.keys.public_key()]).await.unwrap();
        assert_eq!(refounded.root_epoch, Epoch(1), "the ban rolled the root");
        // The ban survives the Refounding (the banlist head compacted forward).
        let post = fold_authority(&refounded, &fetch_control(&bed.relay, &refounded).await, &load_floors(&refounded));
        assert!(post.banned.contains(&member.keys.public_key().to_hex()), "the ban survives the re-founding");
        // Pre-ban history still reads across the new epoch.
        assert!(
            texts_in(&bed.relay, &refounded, &general).await.contains(&"owner: welcome".to_string()),
            "pre-refounding history stays readable"
        );

        // The banned member's rekey-follow concludes they're severed.
        bed.swap_to(&member);
        let follow = follow_rekeys(&bed.relay, &joined, &session).await.unwrap();
        assert!(follow.self_removed, "the banned member is cryptographically cut");

        // Owner dissolves → sealed.
        bed.swap_to(&owner);
        dissolve_community(&bed.relay, &refounded).await.unwrap();
        assert!(crate::db::community::load_community_v2(community.id()).unwrap().unwrap().dissolved, "the community is sealed");
    }

    #[tokio::test]
    async fn a_grant_revoke_survives_a_withholding_relay() {
        // Floor persistence on the delegation plane: after the owner revokes an admin,
        // a relay serving only the OLD (still owner-signed) grant can't resurrect it.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Revoke", vec!["wss://good".into()], None).await.unwrap();
        let admin = Keys::generate();
        let rid = "d4".repeat(32);
        publish_role(&relay, &community, &owner, &admin_role(&rid, Permissions::MANAGE_METADATA), 1).await;
        publish_grant(&relay, &community, &owner, &admin.public_key(), vec![rid.clone()], 1).await;
        let session = SessionGuard::capture();
        follow_control(&relay, &community, &session).await.unwrap(); // seed floors incl. the grant at v1
        publish_grant(&relay, &community, &owner, &admin.public_key(), vec![], 2).await; // revoke → grant floor v2
        follow_control(&relay, &community, &session).await.unwrap();

        // A stale relay serves only the grant prefix (v1, the live grant).
        inject_stale_prefix(&relay, &community, 1, "wss://stale").await;
        let mut stale = community.clone();
        stale.relays = vec!["wss://stale".into()];
        let floors = load_floors(&community);
        let editions = fetch_control(&relay, &stale).await;
        let authority = fold_authority(&stale, &editions, &floors);
        assert!(
            !authority.roles.is_authorized(&admin.public_key().to_hex(), Some(&owner.public_key().to_hex()), Permissions::MANAGE_METADATA),
            "the persisted grant floor refuses the rolled-back (re-granted) view"
        );
    }

    /// Load the current-epoch floors for a community (test mirror of follow_control).
    fn load_floors(community: &CommunityV2) -> Floors {
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        crate::db::community::get_all_edition_heads_full(&cid_hex)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, f)| f.0 == community.root_epoch.0)
            .map(|(e, f)| (e, (f.1, f.2, f.3)))
            .collect()
    }

    /// Fetch + open every control edition at a community's control plane (test helper).
    async fn fetch_control(relay: &MemoryRelay, community: &CommunityV2) -> Vec<ParsedEdition> {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let q = Query { kinds: vec![stream::KIND_WRAP], authors: vec![group.pk_hex()], limit: Some(500), ..Default::default() };
        relay
            .fetch(&q, &community.relays)
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|w| control::open_control_edition(w, &group).ok().map(|(ed, _)| ed))
            .collect()
    }

    #[tokio::test]
    async fn follow_control_is_a_noop_on_a_freshly_created_community() {
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Fresh", vec!["wss://r".into()], None).await.unwrap();
        let session = SessionGuard::capture();
        // Only the genesis editions exist; folding them reproduces the held view.
        assert!(follow_control(&relay, &community, &session).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn follow_control_adds_a_new_public_channel_and_re_subscribes_it() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Grow", vec!["wss://r".into()], None).await.unwrap();
        let new_id = ChannelId([0x5a; 32]);
        publish_channel_edition(&relay, &community, &owner, &new_id, "announcements", false, 1, false).await;

        let session = SessionGuard::capture();
        let updated = follow_control(&relay, &community, &session).await.unwrap().expect("a new channel changed the view");
        assert_eq!(updated.channels.len(), 2);
        let added = updated.channel(&new_id).expect("the new channel folded in");
        assert_eq!(added.name, "announcements");
        assert!(!added.private);
        assert_eq!(added.key, None, "a public channel derives from the root (no stored key)");

        // The new channel is now in the realtime author-set (it would be subscribed).
        let authors = super::super::realtime::plane_authors(std::slice::from_ref(&updated));
        let addr = channel_group_key(&updated.community_root, &new_id, updated.root_epoch).pk();
        assert!(authors.contains(&addr), "the added channel joins the live subscription");

        // Persisted: a reload sees it too.
        let reloaded = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        assert!(reloaded.channel(&new_id).is_some());
    }

    #[tokio::test]
    async fn follow_control_renames_the_community_and_an_existing_channel() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Old Name", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        // A v2 metadata edition renames the community; a v2 channel edition renames #general.
        publish_community_meta(&relay, &community, &owner, "New Name", 2).await;
        publish_channel_edition(&relay, &community, &owner, &general, "lobby", false, 2, false).await;

        let session = SessionGuard::capture();
        let updated = follow_control(&relay, &community, &session).await.unwrap().unwrap();
        assert_eq!(updated.name, "New Name");
        assert_eq!(updated.channel(&general).unwrap().name, "lobby");
        assert_eq!(updated.channels.len(), 1, "a rename doesn't add a channel");
    }

    #[tokio::test]
    async fn follow_control_deletes_a_channel() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Prune", vec!["wss://r".into()], None).await.unwrap();
        let extra = ChannelId([0x77; 32]);
        let session = SessionGuard::capture();

        // The channel is first added and folded into the held view.
        publish_channel_edition(&relay, &community, &owner, &extra, "temp", false, 1, false).await;
        let with_extra = follow_control(&relay, &community, &session).await.unwrap().expect("added");
        assert!(with_extra.channel(&extra).is_some());

        // Then it's tombstoned — the delete (higher version) folds the held one back out.
        publish_channel_edition(&relay, &community, &owner, &extra, "temp", false, 2, true).await;
        let updated = follow_control(&relay, &with_extra, &session).await.unwrap().expect("removed");
        assert!(updated.channel(&extra).is_none(), "a deleted channel folds out");
        assert_eq!(updated.channels.len(), 1, "only #general remains");
    }

    /// Re-inject only the OLD prefix (every edition at/below `max_version`) of a
    /// community's control plane onto a second relay URL — the withholding-relay
    /// simulation: everything it serves is genuinely owner-signed, just stale.
    async fn inject_stale_prefix(relay: &MemoryRelay, community: &CommunityV2, max_version: u64, stale_relay: &str) {
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let query = Query { kinds: vec![stream::KIND_WRAP], authors: vec![group.pk_hex()], limit: Some(500), ..Default::default() };
        let wraps = relay.fetch(&query, &community.relays).await.unwrap();
        for w in &wraps {
            if let Ok((ed, _)) = control::open_control_edition(w, &group) {
                if ed.version <= max_version {
                    relay.inject(w, &[stale_relay.to_string()]);
                }
            }
        }
    }

    #[tokio::test]
    async fn a_withholding_relay_cannot_roll_back_a_rename() {
        // W2 persisted floor: after adopting the owner's v2 rename, a relay serving
        // only the (owner-signed) v1 genesis must not revert the held name.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Original", vec!["wss://good".into()], None).await.unwrap();
        publish_community_meta(&relay, &community, &owner, "Renamed", 2).await;

        let session = SessionGuard::capture();
        let updated = follow_control(&relay, &community, &session).await.unwrap().expect("rename adopted");
        assert_eq!(updated.name, "Renamed");

        // The stale relay holds only the genesis prefix; point the follow at it.
        inject_stale_prefix(&relay, &community, 1, "wss://stale").await;
        let mut stale_view = updated.clone();
        stale_view.relays = vec!["wss://stale".into()];
        assert!(
            follow_control(&relay, &stale_view, &session).await.unwrap().is_none(),
            "a stale-only relay must not change the held view"
        );
        let held = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        assert_eq!(held.name, "Renamed", "the persisted floor refuses the rollback");
    }

    #[tokio::test]
    async fn a_withholding_relay_cannot_resurrect_a_deleted_channel() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Prune2", vec!["wss://good".into()], None).await.unwrap();
        let extra = ChannelId([0x44; 32]);
        let session = SessionGuard::capture();

        // A same-content metadata edit: no visible change (None), but the floor must
        // still advance to v2 (so the genesis metadata can't re-present below).
        publish_community_meta(&relay, &community, &owner, "Prune2", 2).await;
        assert!(follow_control(&relay, &community, &session).await.unwrap().is_none());

        publish_channel_edition(&relay, &community, &owner, &extra, "temp", false, 1, false).await;
        let with_extra = follow_control(&relay, &community, &session).await.unwrap().expect("added");
        publish_channel_edition(&relay, &community, &owner, &extra, "temp", false, 2, true).await;
        let pruned = follow_control(&relay, &with_extra, &session).await.unwrap().expect("removed");
        assert!(pruned.channel(&extra).is_none());

        // The stale relay serves the add (v1) but withholds the delete (v2).
        inject_stale_prefix(&relay, &community, 1, "wss://stale").await;
        let mut stale_view = pruned.clone();
        stale_view.relays = vec!["wss://stale".into()];
        assert!(
            follow_control(&relay, &stale_view, &session).await.unwrap().is_none(),
            "the withheld delete must not resurrect the channel"
        );
        let held = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        assert!(held.channel(&extra).is_none(), "the deleted channel stays deleted");
    }

    #[tokio::test]
    async fn a_new_epoch_bootstraps_past_an_old_epoch_floor() {
        // The Armada-convergence carve-out: a Refounding compacts the chain and
        // re-wraps a detached head at the NEW epoch's control plane. The old epoch's
        // floor must not block it — epoch-filtering makes the entity bootstrap.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Before", vec!["wss://good".into()], None).await.unwrap();
        let session = SessionGuard::capture();
        publish_community_meta(&relay, &community, &owner, "Edited", 2).await;
        let updated = follow_control(&relay, &community, &session).await.unwrap().expect("edit adopted");
        assert_eq!(updated.name, "Edited");

        // Refounding lands (epoch bump saved by the rekey path); the compacted head
        // arrives DETACHED (high version, no prev) on the new epoch's plane.
        let mut refounded = updated.clone();
        refounded.root_epoch = crate::community::Epoch(1);
        crate::db::community::save_community_v2(&refounded).unwrap();
        publish_community_meta(&relay, &refounded, &owner, "Compacted", 5).await;

        let adopted = follow_control(&relay, &refounded, &session).await.unwrap().expect("compacted head adopted");
        assert_eq!(adopted.name, "Compacted", "a fresh epoch bootstraps despite the dangling prev");
        // The persisted floor is stamped with the epoch the FOLD ran under.
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let heads = crate::db::community::get_all_edition_heads_epoched(&cid_hex).unwrap();
        assert!(
            heads.get(&cid_hex).is_some_and(|(e, v, _)| *e == 1 && *v == 5),
            "the adopted head carries the fold's epoch + version"
        );
    }

    #[tokio::test]
    async fn a_same_version_owner_fork_at_the_floor_converges_to_the_deterministic_winner() {
        // Two owner-signed editions at the SAME version (publish retry / two owner
        // devices): every client must land on the lower-inner-id winner. A hash-strict
        // floor would wedge here forever while Armada converges — the floor must
        // CONVERGE instead (the v1 decide() rule).
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Fork", vec!["wss://r".into()], None).await.unwrap();
        let session = SessionGuard::capture();
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let genesis_hash = head_hash_on_relay(&relay, &community, &community.id().0).await.unwrap();

        publish_community_meta(&relay, &community, &owner, "Ours", 2).await;
        let ours = follow_control(&relay, &community, &session).await.unwrap().expect("ours adopted");
        assert_eq!(ours.name, "Ours");

        // Our committed v2 edition's tiebreak id.
        let our_inner = {
            let q = Query { kinds: vec![stream::KIND_WRAP], authors: vec![group.pk_hex()], limit: Some(500), ..Default::default() };
            let wraps = relay.fetch(&q, &community.relays).await.unwrap();
            wraps
                .iter()
                .find_map(|w| {
                    control::open_control_edition(w, &group)
                        .ok()
                        .filter(|(ed, _)| ed.version == 2 && ed.vsk == vsk::COMMUNITY_METADATA)
                        .map(|(ed, _)| ed.inner_id)
                })
                .unwrap()
        };

        // Craft the concurrent fork so it WINS the deterministic tiebreak (vary the
        // authored timestamp until its inner id is lower).
        let meta = control::CommunityMetadata { name: "Theirs".into(), ..Default::default() };
        let content = serde_json::to_string(&meta).unwrap();
        let mut ts = 2_000u64;
        let fork_wrap = loop {
            let rumor = control::build_edition_rumor(owner.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, 2, Some(&genesis_hash), &content, ts, None);
            let inner = rumor.id.unwrap().to_bytes();
            if inner < our_inner {
                break control::seal_control_edition(&rumor, &group, &owner, Timestamp::from_secs(ts)).unwrap().0;
            }
            ts += 1;
        };
        relay.publish(&fork_wrap, &community.relays).await.unwrap();

        let converged = follow_control(&relay, &ours, &session).await.unwrap().expect("fork winner adopted");
        assert_eq!(converged.name, "Theirs", "the floor converges to the lower-inner-id winner");
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let held = crate::db::community::get_edition_head_inner_id(&cid_hex, &cid_hex).unwrap();
        assert!(held.is_some_and(|h| h < our_inner), "the persisted floor's tiebreak key moved to the winner");
    }

    #[tokio::test]
    async fn an_anchored_prefix_applies_while_a_gap_above_awaits_the_missing_link() {
        // v2 chains to the floor; v4 arrives but its v3 link is withheld. The
        // chain-verified prefix (v2) applies NOW — refuse-downgrade holds for it —
        // while the detached v4 waits. When v3 lands, the chain heals to v4.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Prefix", vec!["wss://r".into()], None).await.unwrap();
        let session = SessionGuard::capture();
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);

        publish_community_meta(&relay, &community, &owner, "Two", 2).await;
        let v2_hash = head_hash_on_relay(&relay, &community, &community.id().0).await.unwrap();

        // Craft v3 (held back) and v4 (published, chained to the withheld v3).
        let c3 = serde_json::to_string(&control::CommunityMetadata { name: "Three".into(), ..Default::default() }).unwrap();
        let r3 = control::build_edition_rumor(owner.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, 3, Some(&v2_hash), &c3, 3_000, None);
        let (w3, _) = control::seal_control_edition(&r3, &group, &owner, Timestamp::from_secs(3_000)).unwrap();
        let (ed3, _) = control::open_control_edition(&w3, &group).unwrap();
        let c4 = serde_json::to_string(&control::CommunityMetadata { name: "Four".into(), ..Default::default() }).unwrap();
        let r4 = control::build_edition_rumor(owner.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, 4, Some(&ed3.self_hash), &c4, 4_000, None);
        let (w4, _) = control::seal_control_edition(&r4, &group, &owner, Timestamp::from_secs(4_000)).unwrap();
        relay.publish(&w4, &community.relays).await.unwrap();

        let updated = follow_control(&relay, &community, &session).await.unwrap().expect("the verified prefix applies");
        assert_eq!(updated.name, "Two", "the anchored prefix lands; the detached v4 does not");

        relay.publish(&w3, &community.relays).await.unwrap();
        let healed = follow_control(&relay, &updated, &session).await.unwrap().expect("the chain heals");
        assert_eq!(healed.name, "Four", "once the link arrives, the head advances past the prefix");
    }

    #[tokio::test]
    async fn paging_rescues_a_floor_link_evicted_from_the_newest_window() {
        // The held floor is v2; the owner publishes v3, then a flood of foreign junk
        // wraps fills the newest window, then v4. Page 1 sees only v4 (detached →
        // gapped); paging older must recover v3 (and the floor link) and heal to v4.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Paged", vec!["wss://r".into()], None).await.unwrap();
        let session = SessionGuard::capture();
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);

        publish_community_meta(&relay, &community, &owner, "Two", 2).await;
        let base = follow_control(&relay, &community, &session).await.unwrap().expect("floor at v2");
        publish_community_meta(&relay, &base, &owner, "Three", 3).await; // ts 1_000 (old)
        let v3_hash = head_hash_on_relay(&relay, &community, &community.id().0).await.unwrap();

        // Rogue flood occupying the newest window (sealed to the control plane, but
        // non-owner — the authority gate drops them; they only crowd the page).
        let rogue = Keys::generate();
        for i in 0..(FOLLOW_PAGE as u64 - 1) {
            let rumor = control::build_edition_rumor(rogue.public_key(), vsk::CHANNEL_METADATA, &[0xCC; 32], 1, None, "{\"name\":\"junk\",\"private\":false}", 4_000 + i, None);
            let (w, _) = control::seal_control_edition(&rumor, &group, &rogue, Timestamp::from_secs(4_000 + i)).unwrap();
            relay.publish(&w, &community.relays).await.unwrap();
        }
        // v4 chained to the real v3 (crafted directly: the flood also blinds the
        // helper's own newest-window head lookup), timestamped newest of all.
        let c4 = serde_json::to_string(&control::CommunityMetadata { name: "Four".into(), ..Default::default() }).unwrap();
        let r4 = control::build_edition_rumor(owner.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, 4, Some(&v3_hash), &c4, 10_000, None);
        let (w4, _) = control::seal_control_edition(&r4, &group, &owner, Timestamp::from_secs(10_000)).unwrap();
        relay.publish(&w4, &community.relays).await.unwrap();

        let healed = follow_control(&relay, &base, &session).await.unwrap().expect("paging recovered the chain");
        assert_eq!(healed.name, "Four", "the gap paged past the flood to the floor link");
    }

    #[tokio::test]
    async fn a_follow_after_delete_does_not_resurrect_the_community() {
        // A leave/delete racing an in-flight follow: the follow must not re-insert
        // the community row or floor rows past delete_community's wipe.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Gone", vec!["wss://r".into()], None).await.unwrap();
        publish_community_meta(&relay, &community, &owner, "Edited", 2).await;
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        crate::db::community::delete_community(&cid_hex).unwrap();

        let session = SessionGuard::capture();
        assert!(
            follow_control(&relay, &community, &session).await.unwrap().is_none(),
            "a follow racing a delete is a no-op"
        );
        assert!(crate::db::community::load_community_v2(community.id()).unwrap().is_none(), "the community stays deleted");
        assert!(crate::db::community::edition_head_entity_ids(&cid_hex).unwrap().is_empty(), "no orphan floor rows");
    }

    #[tokio::test]
    async fn a_rekey_follow_after_delete_does_not_resurrect_the_community() {
        // The rekey sibling of the follow_control guard: an owner rotation adopted
        // mid-race must not upsert the community row back after a leave/delete.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "GoneKeys", vec!["wss://r".into()], None).await.unwrap();
        let new_root = [0xB2; 32];
        publish_base_rotation(&relay, &community, &owner, &[owner.public_key()], &new_root, &community.community_root).await;
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        crate::db::community::delete_community(&cid_hex).unwrap();

        let session = SessionGuard::capture();
        let follow = follow_rekeys(&relay, &community, &session).await.unwrap();
        assert!(follow.updated.is_none() && !follow.self_removed, "a rekey follow racing a delete adopts nothing");
        assert!(crate::db::community::load_community_v2(community.id()).unwrap().is_none(), "the community stays deleted");
    }

    #[tokio::test]
    async fn a_joiner_bootstraps_the_highest_head_across_a_lost_middle_edition() {
        // {v1, v3} on the relays with v2 lost at publish time (a rate-limiting relay
        // that still ACKed): the genesis anchors, so an anchored-prefix-first fold
        // would take v1 and SEED the joiner's floor there — pinning them below the
        // head Armada shows, forever. A joiner (floor 0) must bootstrap v3.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Skip", bed.relays.clone(), None).await.unwrap();
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let genesis_hash = head_hash_on_relay(&bed.relay, &community, &community.id().0).await.unwrap();

        // v2 is crafted but NEVER published; v3 chains to it and is published.
        let c2 = serde_json::to_string(&control::CommunityMetadata { name: "Two".into(), ..Default::default() }).unwrap();
        let r2 = control::build_edition_rumor(owner.keys.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, 2, Some(&genesis_hash), &c2, 2_000, None);
        let (w2, _) = control::seal_control_edition(&r2, &group, &owner.keys, Timestamp::from_secs(2_000)).unwrap();
        let (ed2, _) = control::open_control_edition(&w2, &group).unwrap();
        let c3 = serde_json::to_string(&control::CommunityMetadata { name: "Three".into(), ..Default::default() }).unwrap();
        let r3 = control::build_edition_rumor(owner.keys.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, 3, Some(&ed2.self_hash), &c3, 3_000, None);
        let (w3, _) = control::seal_control_edition(&r3, &group, &owner.keys, Timestamp::from_secs(3_000)).unwrap();
        bed.relay.publish(&w3, &community.relays).await.unwrap();

        let bundle = bundle_of(&community, Some(owner.keys.public_key()), None, None);
        let bundle_json = serde_json::to_string(&bundle).unwrap();
        bed.swap_to(&member);
        let joined = accept_parked_invite(&bed.relay, &bundle_json, None).await.unwrap();
        assert_eq!(joined.name, "Three", "the joiner bootstraps the highest signed head, not the anchored stale prefix");
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&joined.id().0);
        let head = crate::db::community::get_edition_head(&cid_hex, &cid_hex).unwrap();
        assert!(head.is_some_and(|(v, _)| v == 3), "the seeded floor is the bootstrap head");
    }

    #[tokio::test]
    async fn a_losing_same_version_fork_cannot_replace_the_held_floor() {
        // The refusal half of fork convergence: a relay withholding OUR committed
        // floor edition while serving only a same-version fork with a HIGHER inner
        // id must be treated as withholding — held state and floor unchanged.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Fork2", vec!["wss://good".into()], None).await.unwrap();
        let session = SessionGuard::capture();
        let group = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let genesis_hash = head_hash_on_relay(&relay, &community, &community.id().0).await.unwrap();

        publish_community_meta(&relay, &community, &owner, "Ours", 2).await;
        let ours = follow_control(&relay, &community, &session).await.unwrap().expect("ours adopted");
        assert_eq!(ours.name, "Ours");
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let held_before = crate::db::community::get_edition_head(&cid_hex, &cid_hex).unwrap().unwrap();
        let our_inner = crate::db::community::get_edition_head_inner_id(&cid_hex, &cid_hex).unwrap().unwrap();

        // Grind the fork to LOSE the tiebreak (higher inner id), then serve it —
        // with the genesis but WITHOUT our v2 — from a withholding relay.
        let meta = control::CommunityMetadata { name: "Theirs".into(), ..Default::default() };
        let content = serde_json::to_string(&meta).unwrap();
        let mut ts = 5_000u64;
        let fork_wrap = loop {
            let rumor = control::build_edition_rumor(owner.public_key(), vsk::COMMUNITY_METADATA, &community.id().0, 2, Some(&genesis_hash), &content, ts, None);
            if rumor.id.unwrap().to_bytes() > our_inner {
                break control::seal_control_edition(&rumor, &group, &owner, Timestamp::from_secs(ts)).unwrap().0;
            }
            ts += 1;
        };
        inject_stale_prefix(&relay, &community, 1, "wss://stale").await; // genesis only
        relay.inject(&fork_wrap, &["wss://stale".to_string()]);
        let mut stale_view = ours.clone();
        stale_view.relays = vec!["wss://stale".into()];

        assert!(
            follow_control(&relay, &stale_view, &session).await.unwrap().is_none(),
            "a losing fork served without our floor edition changes nothing"
        );
        let held_after = crate::db::community::get_edition_head(&cid_hex, &cid_hex).unwrap().unwrap();
        assert_eq!(held_after, held_before, "the floor row is untouched");
        let held = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        assert_eq!(held.name, "Ours", "the held state is untouched");
    }

    #[tokio::test]
    async fn follow_control_ignores_a_non_owner_edition() {
        // A member holds the community_root, so they CAN seal a control edition —
        // but they aren't the owner, so the authority gate drops it (first cut:
        // owner-only). The rogue channel must never appear.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Guarded", vec!["wss://r".into()], None).await.unwrap();
        let rogue = Keys::generate();
        let rogue_id = ChannelId([0x99; 32]);
        publish_channel_edition(&relay, &community, &rogue, &rogue_id, "backdoor", false, 1, false).await;

        let session = SessionGuard::capture();
        assert!(
            follow_control(&relay, &community, &session).await.unwrap().is_none(),
            "a non-owner control edition is not folded"
        );
    }

    #[tokio::test]
    async fn follow_control_skips_a_new_private_channel_without_a_key() {
        // A Private channel's key rides the rekey plane, not the control edition —
        // control-follow can't key it, so it's deferred (never added keyless, which
        // would wrongly read the public address).
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Priv", vec!["wss://r".into()], None).await.unwrap();
        let priv_id = ChannelId([0x33; 32]);
        publish_channel_edition(&relay, &community, &owner, &priv_id, "mods", true, 1, false).await;

        let session = SessionGuard::capture();
        assert!(
            follow_control(&relay, &community, &session).await.unwrap().is_none(),
            "a new private channel is skipped until its key arrives via rekey"
        );
    }

    // ── Live rekey-follow ────────────────────────────────────────────────────

    /// Publish an owner-grammar base rotation (Refounding) delivering `new_root`
    /// to each recipient. `rotator` is the seal signer (owner for a legit rotation,
    /// a stranger for the authority test); `prev_key` is the root it claims to
    /// extend (mismatch → a fork).
    async fn publish_base_rotation(
        relay: &MemoryRelay,
        community: &CommunityV2,
        rotator: &Keys,
        recipients: &[PublicKey],
        new_root: &[u8; 32],
        prev_key: &[u8; 32],
    ) {
        let new_epoch = Epoch(community.root_epoch.0 + 1);
        let prev_epoch = community.root_epoch;
        let prev_commit = super::super::derive::epoch_key_commitment(prev_epoch, prev_key);
        let group = base_rekey_group_key(&community.community_root, community.id(), new_epoch);
        let blobs: Vec<_> = recipients
            .iter()
            .map(|r| rekey::build_blob_local(rotator.secret_key(), &rotator.public_key().to_bytes(), r, RekeyScope::Root, new_epoch, new_root).unwrap())
            .collect();
        let events = rekey::build_rekey_chunks_local(rotator, &group, RekeyScope::Root, new_epoch, prev_epoch, &prev_commit, &blobs, 2_000).unwrap();
        for e in &events {
            relay.publish(e, &community.relays).await.unwrap();
        }
    }

    /// Attach a Private channel (key + epoch) to a held community and persist it.
    fn add_private_channel(community: &mut CommunityV2, id: ChannelId, key: [u8; 32], epoch: Epoch) {
        community.channels.push(ChannelV2 { id, name: "mods".into(), private: true, key: Some(key), epoch });
        crate::db::community::save_community_v2(community).unwrap();
    }

    #[tokio::test]
    async fn follow_rekeys_is_a_noop_without_rotations() {
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Still", vec!["wss://r".into()], None).await.unwrap();
        let session = SessionGuard::capture();
        let follow = follow_rekeys(&relay, &community, &session).await.unwrap();
        assert!(follow.updated.is_none() && !follow.self_removed, "no rotation → nothing to adopt");
    }

    #[tokio::test]
    async fn follow_rekeys_adopts_an_owner_base_rotation() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Refound", vec!["wss://r".into()], None).await.unwrap();
        let new_root = [0xB1; 32];
        // Owner rotates the base to epoch 1, delivering the new root to me.
        publish_base_rotation(&relay, &community, &owner, &[owner.public_key()], &new_root, &community.community_root).await;

        let session = SessionGuard::capture();
        let updated = follow_rekeys(&relay, &community, &session).await.unwrap().updated.expect("adopted");
        assert_eq!(updated.root_epoch, Epoch(1), "advanced one epoch");
        assert_eq!(updated.community_root, new_root, "adopted the fresh root");
        // The public channel now reads under the NEW root/epoch (its address moved).
        let addr = super::super::realtime::plane_authors(std::slice::from_ref(&updated));
        let general = updated.channels[0].id;
        let new_chat = channel_group_key(&new_root, &general, Epoch(1)).pk();
        assert!(addr.contains(&new_chat), "the public channel re-addresses under the new root");
    }

    #[tokio::test]
    async fn follow_rekeys_adopts_an_owner_private_channel_rotation() {
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let mut community = create_community(&relay, "PrivRot", vec!["wss://r".into()], None).await.unwrap();
        let priv_id = ChannelId([0x33; 32]);
        add_private_channel(&mut community, priv_id, [0x44; 32], Epoch(0));

        // Owner rotates the private channel to epoch 1 with a fresh key, delivered to me.
        let new_key = [0x55; 32];
        let prev_commit = super::super::derive::epoch_key_commitment(Epoch(0), &[0x44; 32]);
        let group = channel_rekey_group_key(&community.community_root, &priv_id, Epoch(1));
        let blob = rekey::build_blob_local(owner.secret_key(), &owner.public_key().to_bytes(), &owner.public_key(), RekeyScope::Channel(priv_id), Epoch(1), &new_key).unwrap();
        let events = rekey::build_rekey_chunks_local(&owner, &group, RekeyScope::Channel(priv_id), Epoch(1), Epoch(0), &prev_commit, &[blob], 2_000).unwrap();
        for e in &events {
            relay.publish(e, &community.relays).await.unwrap();
        }

        let session = SessionGuard::capture();
        let updated = follow_rekeys(&relay, &community, &session).await.unwrap().updated.expect("adopted");
        let ch = updated.channel(&priv_id).unwrap();
        assert_eq!(ch.epoch, Epoch(1), "the private channel advanced an epoch");
        assert_eq!(ch.key, Some(new_key), "adopted the fresh channel key");
        assert_eq!(updated.root_epoch, Epoch(0), "the base is untouched by a channel rotation");
    }

    #[tokio::test]
    async fn follow_rekeys_ignores_a_non_owner_rotation() {
        // A member holds the community_root, so they can derive the rekey group key
        // and mint a rotation — but they aren't the owner, so it's not adopted.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Guarded", vec!["wss://r".into()], None).await.unwrap();
        let rogue = Keys::generate();
        publish_base_rotation(&relay, &community, &rogue, &[rogue.public_key()], &[0xEE; 32], &community.community_root).await;

        let session = SessionGuard::capture();
        let follow = follow_rekeys(&relay, &community, &session).await.unwrap();
        assert!(follow.updated.is_none() && !follow.self_removed, "a non-owner rotation is not adopted");
    }

    #[tokio::test]
    async fn follow_rekeys_ignores_a_rotation_off_the_wrong_prev() {
        // A rotation whose prevcommit doesn't match the key I hold is a fork, not an
        // extension — never adopted (would splice me onto an unrelated chain).
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Forked", vec!["wss://r".into()], None).await.unwrap();
        // prev_key ≠ the real community_root → the continuity check reads Fork.
        publish_base_rotation(&relay, &community, &owner, &[owner.public_key()], &[0xB2; 32], &[0x00; 32]).await;

        let session = SessionGuard::capture();
        let follow = follow_rekeys(&relay, &community, &session).await.unwrap();
        assert!(follow.updated.is_none(), "a fork off the wrong prev is not adopted");
    }

    #[tokio::test]
    async fn follow_rekeys_holds_on_an_incomplete_rotation() {
        // A 2-chunk rotation with only chunk 1 present can never conclude — not an
        // adoption, and crucially NOT a removal (a missing chunk might carry my blob).
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Partial", vec!["wss://r".into()], None).await.unwrap();
        let new_epoch = Epoch(1);
        let prev_commit = super::super::derive::epoch_key_commitment(Epoch(0), &community.community_root);
        let group = base_rekey_group_key(&community.community_root, community.id(), new_epoch);
        // Chunk 1 of a declared 2, carrying someone else's blob (not mine).
        let other = Keys::generate();
        let blob = rekey::build_blob_local(owner.secret_key(), &owner.public_key().to_bytes(), &other.public_key(), RekeyScope::Root, new_epoch, &[0xB3; 32]).unwrap();
        let rumor = rekey::build_rekey_rumor(owner.public_key(), RekeyScope::Root, new_epoch, Epoch(0), &prev_commit, &[blob], 1, 2, 2_000).unwrap();
        let (wrap, _) = rekey::seal_rekey_chunk(&rumor, &group, &owner, Timestamp::from_secs(2_000)).unwrap();
        relay.publish(&wrap, &community.relays).await.unwrap();

        let session = SessionGuard::capture();
        let follow = follow_rekeys(&relay, &community, &session).await.unwrap();
        assert!(follow.updated.is_none() && !follow.self_removed, "an incomplete rotation neither adopts nor removes");
    }

    #[tokio::test]
    async fn follow_rekeys_removes_a_member_dropped_by_a_base_rotation() {
        // Realistic two-actor removal: the owner Refounds the base and delivers the
        // new root to a THIRD party, not the member — a complete rotation with no
        // blob for the member is a removal.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Evict", bed.relays.clone(), None).await.unwrap();
        send_direct_invite(&bed.relay, &community, &member.keys.public_key(), None, None).await.unwrap();

        bed.swap_to(&member);
        let invite_wrap = fetch_direct_invite(&bed.relay, &bed.relays, &member.keys.public_key()).await;
        let joined = accept_direct_invite(&bed.relay, &invite_wrap).await.unwrap();

        // Owner rotates, delivering only to a stranger (the member is dropped).
        bed.swap_to(&owner);
        let stranger = Keys::generate();
        publish_base_rotation(&bed.relay, &community, &owner.keys, &[stranger.public_key()], &[0xC4; 32], &community.community_root).await;

        // The member's follow concludes removal (a complete rotation without their blob).
        bed.swap_to(&member);
        let session = SessionGuard::capture();
        let follow = follow_rekeys(&bed.relay, &joined, &session).await.unwrap();
        assert!(follow.self_removed, "a complete base rotation dropping the member removes them");
        assert!(follow.updated.is_none(), "a removed member adopts nothing");
    }

    // ── Audit regressions ────────────────────────────────────────────────────

    #[tokio::test]
    async fn accept_rejects_a_bundle_with_a_forged_community_root() {
        // The eclipse: community_id commits only to (owner, salt) — both semi-public
        // — so a forged invite pairs the REAL triple with an attacker root, and every
        // plane derives from it. The join-time owner-genesis check must refuse.
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Real", bed.relays.clone(), None).await.unwrap();

        let fake = crate::simd::hex::bytes_to_hex_32(&[0xEE; 32]);
        let mut forged = bundle_of(&community, None, None, None);
        forged.community_root = fake.clone();
        for ch in &mut forged.channels {
            ch.key = fake.clone();
        }
        let attacker = Keys::generate();
        let wrap = invite::build_direct_invite(&attacker, &member.keys.public_key(), &forged).unwrap();
        bed.relay.publish(&wrap, &bed.relays).await.unwrap();

        bed.swap_to(&member);
        let invite_wrap = fetch_direct_invite(&bed.relay, &bed.relays, &member.keys.public_key()).await;
        let err = accept_direct_invite(&bed.relay, &invite_wrap).await.unwrap_err();
        assert!(err.contains("could not verify"), "a forged root fails the owner-genesis check: {err}");
        assert!(
            crate::db::community::load_community_v2(community.id()).unwrap().is_none(),
            "a rejected join persists nothing"
        );
    }

    #[tokio::test]
    async fn follow_control_heals_a_bundle_misclassified_public_channel() {
        // A bundle can set a PUBLIC channel's grant key to the attacker's, so the
        // joiner addresses it at a plane only the attacker reads. The owner's genuine
        // public:false edition must override it on follow.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Heal", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        let mut poisoned = community.clone();
        poisoned.channels[0].private = true;
        poisoned.channels[0].key = Some([0x66; 32]);
        crate::db::community::save_community_v2(&poisoned).unwrap();

        let session = SessionGuard::capture();
        let healed = follow_control(&relay, &poisoned, &session).await.unwrap().expect("healed");
        let ch = healed.channel(&general).unwrap();
        assert!(!ch.private, "the owner's public declaration overrides the bundle");
        assert_eq!(ch.key, None, "a healed public channel derives from the root");
    }

    #[tokio::test]
    async fn a_deleted_channel_does_not_resurrect_on_reload() {
        // save_community_v2 must prune orphan channel rows, or a control-follow delete
        // reappears (with a stale key) on the next reload.
        let (_tmp, _guard, owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Prune", vec!["wss://r".into()], None).await.unwrap();
        let extra = ChannelId([0x77; 32]);
        let session = SessionGuard::capture();
        publish_channel_edition(&relay, &community, &owner, &extra, "temp", false, 1, false).await;
        let with_extra = follow_control(&relay, &community, &session).await.unwrap().unwrap();
        assert!(with_extra.channel(&extra).is_some());
        publish_channel_edition(&relay, &community, &owner, &extra, "temp", false, 2, true).await;
        let after = follow_control(&relay, &with_extra, &session).await.unwrap().unwrap();
        assert!(after.channel(&extra).is_none());

        let reloaded = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
        assert!(reloaded.channel(&extra).is_none(), "a deleted channel must not resurrect on reload");
        assert_eq!(reloaded.channels.len(), 1);
    }

    #[tokio::test]
    async fn a_channel_owned_by_another_community_is_skipped_not_clobbered() {
        // channel_id is the sole DB primary key, so a bundle/replay reusing another
        // community's channel_id must NOT overwrite that row. It's skipped (not an
        // error — erroring would wedge all of this community's control persistence).
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let a = create_community(&relay, "A", vec!["wss://r".into()], None).await.unwrap();
        let a_channel = a.channels[0].id;
        let mut b = create_community(&relay, "B", vec!["wss://r".into()], None).await.unwrap();
        let b_channel = b.channels[0].id;
        // B's set includes a phantom whose id collides with A's channel.
        b.channels.push(ChannelV2 { id: a_channel, name: "phantom".into(), private: false, key: None, epoch: b.root_epoch });

        crate::db::community::save_community_v2(&b).expect("save succeeds, the phantom is skipped");
        // A's channel row is untouched.
        let a_reloaded = crate::db::community::load_community_v2(a.id()).unwrap().unwrap();
        assert!(!a_reloaded.channels.iter().any(|c| c.private), "A's channel is untouched");
        assert_eq!(a_reloaded.channels[0].id.0, a_channel.0);
        // B keeps its own channel but never acquired a row for the foreign id.
        let b_reloaded = crate::db::community::load_community_v2(b.id()).unwrap().unwrap();
        assert!(b_reloaded.channel(&b_channel).is_some(), "B's own channel persists");
        assert!(b_reloaded.channel(&a_channel).is_none(), "the foreign-owned channel is skipped, not stolen");
    }

    /// A single relay that CAPS every query below the page size (modelling a real
    /// relay's maxFilterLimit) and honors `until` — so the join-verify walk MUST
    /// paginate to reach an old genesis. MemoryRelay can't model this (it unions then
    /// truncates the whole set), which is why a MemoryRelay flood test gives false
    /// confidence about the production `LiveTransport` behaviour.
    struct CappedRelay {
        events: Vec<Event>,
        cap: usize,
    }
    #[async_trait::async_trait]
    impl Transport for CappedRelay {
        async fn publish(&self, _e: &Event, _r: &[String]) -> Result<(), String> {
            Ok(())
        }
        async fn publish_durable(&self, _e: &Event, _r: &[String]) -> Result<(), String> {
            Ok(())
        }
        async fn fetch(&self, q: &Query, _r: &[String]) -> Result<Vec<Event>, String> {
            let mut m: Vec<Event> = self
                .events
                .iter()
                .filter(|e| q.authors.is_empty() || q.authors.contains(&e.pubkey.to_hex()))
                .filter(|e| q.until.is_none_or(|u| e.created_at.as_secs() <= u))
                .cloned()
                .collect();
            m.sort_by(|a, b| b.created_at.cmp(&a.created_at)); // newest first
            m.truncate(self.cap.min(q.limit.unwrap_or(usize::MAX)));
            Ok(m)
        }
    }

    #[tokio::test]
    async fn verify_pages_a_capped_relay_past_a_flood_to_the_genesis() {
        // The join-verify DoS mitigation, tested against a relay that caps below PAGE
        // (production behaviour MemoryRelay hides): a rogue root-holder buries the
        // genesis under junk, and the `until`-walk must page past it. Uses fixed OLD
        // timestamps so `until = now` includes everything and the walk is deterministic.
        let (_tmp, _guard, owner) = init_test_db();
        let meta = control::CommunityMetadata { name: "Capped".into(), relays: vec!["wss://r".into()], ..Default::default() };
        let g = control::genesis(&owner, meta, 1_000).unwrap();
        let community = CommunityV2::from_genesis(&g, "Capped", None, vec!["wss://r".into()], 1_000);

        let control = control_group_key(&community.community_root, community.id(), community.root_epoch);
        let rogue = Keys::generate();
        let mut events: Vec<Event> = g.wraps.to_vec();
        for i in 0..250u64 {
            let rumor = control::build_edition_rumor(rogue.public_key(), vsk::CHANNEL_METADATA, &[0xAB; 32], 1, None, "{\"name\":\"junk\",\"private\":false}", 1_001 + i, None);
            let (wrap, _) = control::seal_control_edition(&rumor, &control, &rogue, Timestamp::from_secs(1_001 + i)).unwrap();
            events.push(wrap);
        }
        // Cap 100/query forces the walk across ~3 pages down to the genesis at ts 1000.
        let relay = CappedRelay { events, cap: 100 };
        let verified = verify_owner_root_and_reconcile(&relay, community.clone()).await;
        assert!(verified.is_ok(), "the until-walk pages a capped relay past the flood to the genesis: {:?}", verified.err());
    }

    #[tokio::test]
    async fn accept_parked_invite_joins_from_the_stored_bundle() {
        // The 3313 receive path: an invite is parked as its bundle JSON, then accepted
        // from the stored bundle (re-verifying the owner root over the network).
        let (bed, owner, member) = TestBed::new();
        bed.swap_to(&owner);
        let community = create_community(&bed.relay, "Parked", bed.relays.clone(), None).await.unwrap();
        let general = community.channels[0].id;
        send_message(&bed.relay, &community, &general, "owner: hi").await.unwrap();
        let bundle = bundle_of(&community, Some(owner.keys.public_key()), None, None);
        let bundle_json = serde_json::to_string(&bundle).unwrap();
        let inviter_hex = owner.keys.public_key().to_hex();

        bed.swap_to(&member);
        let joined = accept_parked_invite(&bed.relay, &bundle_json, Some(&inviter_hex)).await.unwrap();
        assert_eq!(joined.id().0, community.id().0, "joined the community from the parked bundle");
        assert!(joined.identity.verify());
        assert_eq!(texts_in(&bed.relay, &joined, &general).await, vec!["owner: hi"]);
        // The join seeded the verified fold as the member's initial floor, so their
        // first follow can't roll below the state the join just showed.
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&joined.id().0);
        assert!(
            crate::db::community::get_edition_head(&cid_hex, &cid_hex).unwrap().is_some(),
            "the joiner's control floor is seeded from the join-time fold"
        );

        // The Guestbook memberlist now folds both participants.
        bed.swap_to(&owner);
        let members = memberlist(&bed.relay, &community).await.unwrap();
        assert!(members.contains(&member.keys.public_key()), "the parked-invite joiner is a member");
    }

    #[tokio::test]
    async fn accept_parked_invite_rejects_a_forged_root() {
        // A forged-root parked bundle (real identity triple, attacker-chosen root) fails
        // accept — the shared accept path re-verifies the owner root, so a parked invite
        // gets the same eclipse protection as a live one.
        let (_tmp, _guard, _owner) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Real", vec!["wss://r".into()], None).await.unwrap();
        let mut forged = bundle_of(&community, None, None, None);
        let fake = crate::simd::hex::bytes_to_hex_32(&[0xEE; 32]);
        forged.community_root = fake.clone();
        for ch in &mut forged.channels {
            ch.key = fake.clone();
        }
        let bundle_json = serde_json::to_string(&forged).unwrap();

        let err = accept_parked_invite(&relay, &bundle_json, None).await.unwrap_err();
        assert!(err.contains("could not verify"), "a forged-root parked bundle fails definitively: {err}");
    }

    #[test]
    fn v2_and_v1_bundles_are_distinguishable_by_parse() {
        // The protocol discriminator the facade list/accept relies on: a v2 bundle
        // (self-certifying: owner + owner_salt + community_root) parses; a v1-shaped
        // one does not, so a parked invite routes to the right accept path.
        let owner = Keys::generate();
        let identity = super::super::control::CommunityIdentity::mint(&owner.public_key());
        let hex = crate::simd::hex::bytes_to_hex_32;
        let v2 = invite::CommunityInvite {
            community_id: hex(&identity.community_id.0),
            owner: hex(&identity.owner_xonly),
            owner_salt: hex(&identity.owner_salt),
            community_root: hex(&[0x11; 32]),
            root_epoch: 0,
            channels: vec![],
            relays: vec!["wss://r".into()],
            name: "V2".into(),
            icon: None,
            expires_at: None,
            creator_npub: None,
            label: None,
            extra: Default::default(),
        };
        let v2_json = serde_json::to_string(&v2).unwrap();
        assert!(invite::CommunityInvite::from_bundle_json(&v2_json).is_ok(), "a real v2 bundle parses");
        let v1_like = r#"{"community_id":"aa","name":"X","relays":[]}"#;
        assert!(invite::CommunityInvite::from_bundle_json(v1_like).is_err(), "a v1 bundle is not a v2 bundle");
    }

    #[tokio::test]
    async fn verify_rejects_a_cross_community_owner_edition_replay() {
        // The eclipse-via-replay: an owner-signed edition from community X (eid == X.id)
        // rewrapped onto a FORGED community T's fake control plane must NOT authenticate
        // T. T's genesis has eid == T.id, so X's edition — a genuine owner signature but
        // a different eid — is not a valid proof of T's root. This is why "any owner
        // edition" is unsound and the eid==community_id genesis pin is required.
        let (_tmp, _guard, owner) = init_test_db();

        // Community X (real), owned by `owner`.
        let gx = control::genesis(&owner, control::CommunityMetadata { name: "X".into(), ..Default::default() }, 1_000).unwrap();
        let x_control = control_group_key(&gx.community_root, &gx.identity.community_id, Epoch(0));
        let (_ed, opened) = control::open_control_edition(&gx.wraps[0], &x_control).unwrap();

        // Forged community T: the real owner triple but an ATTACKER-chosen root.
        let t_identity = control::CommunityIdentity::mint(&owner.public_key());
        let fake_root = [0xEE; 32];
        let t = CommunityV2 {
            identity: t_identity,
            community_root: fake_root,
            root_epoch: Epoch(0),
            name: "T".into(),
            description: None,
            relays: vec!["wss://r".into()],
            channels: vec![],
            dissolved: false,
            created_at_ms: 0,
        };
        // Rewrap X's owner-signed genesis onto T's fake control plane (the attacker
        // controls the fake root, so they can derive its control group key).
        let t_control = control_group_key(&fake_root, t.id(), t.root_epoch);
        let (replayed, _) = stream::rewrap_seal(&opened.seal, &t_control, Timestamp::from_secs(1_000)).unwrap();
        let relay = MemoryRelay::new();
        relay.publish(&replayed, &t.relays).await.unwrap();

        let verified = verify_owner_root_and_reconcile(&relay, t.clone()).await;
        assert!(verified.is_err(), "a cross-community owner-edition replay must not authenticate a forged root");
    }

    /// LIVE smoke test (network) — ignored by default. Creates a v2 community on a
    /// REAL relay via `LiveTransport`, sends a message, fetches it back, and mints
    /// a public link. A fresh throwaway identity in an isolated temp data dir, so
    /// it never touches real accounts. Run explicitly:
    /// ```sh
    /// cargo test -p vector-core -- --ignored --nocapture live_smoke
    /// ```
    #[tokio::test]
    #[ignore = "hits a real relay over the network"]
    async fn live_smoke_create_send_fetch_on_a_real_relay() {
        use crate::community::transport::LiveTransport;
        use nostr_sdk::prelude::ToBech32;

        let relay = std::env::var("VECTOR_SMOKE_RELAY").unwrap_or_else(|_| "wss://jskitty.com/nostr".to_string());
        let relays = vec![relay.clone()];

        // Isolated account + data dir (a fresh throwaway key — never a real account).
        let _g = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        // Bring your own key (VECTOR_SMOKE_NSEC) to create a community you can log
        // into elsewhere; otherwise a fresh throwaway.
        let keys = match std::env::var("VECTOR_SMOKE_NSEC") {
            Ok(n) => Keys::parse(&n).expect("VECTOR_SMOKE_NSEC is not a valid nsec"),
            Err(_) => Keys::generate(),
        };
        let npub = keys.public_key().to_bech32().unwrap();
        // Off by default (never leak secrets from a committed test); set
        // VECTOR_SMOKE_PRINT_NSEC=1 to print the owner nsec for cross-client login.
        if std::env::var("VECTOR_SMOKE_PRINT_NSEC").is_ok() {
            println!("[smoke] OWNER nsec (throwaway — do NOT reuse): {}", keys.secret_key().to_bech32().unwrap());
        }
        std::fs::create_dir_all(tmp.path().join(&npub)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(npub.clone()).unwrap();
        crate::db::init_database(&npub).unwrap();
        crate::state::MY_SECRET_KEY.store_from_keys(&keys, &[]);
        crate::state::set_my_public_key(keys.public_key());
        println!("[smoke] throwaway identity {npub}");

        // A live client (LiveTransport rides the global NOSTR_CLIENT + warms relays).
        let client = nostr_sdk::ClientBuilder::new().signer(keys.clone()).build();
        client.pool().add_relay(relay.as_str(), nostr_sdk::RelayOptions::default()).await.ok();
        client.connect().await;
        crate::state::set_nostr_client(client);
        let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(15));

        // Create → send → fetch-back → verify.
        let community = create_community(&transport, "V2 Live Smoke", relays.clone(), None).await.expect("create");
        let general = community.channels[0].id;
        println!("[smoke] created community {} on {relay}", crate::simd::hex::bytes_to_hex_32(&community.id().0));

        let text = "hello from a Vector Concord v2 live smoke test";
        let sent_id = send_message(&transport, &community, &general, text).await.expect("send");
        println!("[smoke] sent message {sent_id}");

        // Give the relay a moment to store + be ready to serve it.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        let page = fetch_channel(&transport, &community, &general, 50).await.expect("fetch");
        let texts: Vec<String> = page
            .iter()
            .filter_map(|f| match &f.event {
                ChatEvent::Message { .. } => Some(f.event.opened().rumor.content.clone()),
                _ => None,
            })
            .collect();
        println!("[smoke] fetched {} message(s) back: {texts:?}", texts.len());
        assert!(texts.contains(&text.to_string()), "the message did not round-trip through the real relay");

        // Mint a shareable v2 link (the thing a bot hands out).
        let link = mint_public_link(&transport, &community, "https://vectorapp.io", None, None).await.expect("mint link");
        println!("[smoke] invite link: {}", link.url);
        println!("[smoke] PASS — v2 create+send+fetch+invite round-tripped on {relay}");
    }
}
