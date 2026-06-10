//! Orchestration that ties the Community send/delete primitives to persistence,
//! with multi-account safety. This is the layer Tauri commands are thin wrappers
//! over: it publishes a message AND retains its ephemeral key (so the sender can
//! later delete it), and deletes by loading that retained key back.
//!
//! Every method is `SessionGuard`-gated: a `swap_session` can happen at any await
//! point, and persisting an ephemeral secret (or reading one) must never cross into
//! the wrong account's DB.

use nostr_sdk::prelude::{Event, EventId, JsonUtil, Keys, Tag, ToBech32};

use super::invite::CommunityInvite;
use super::public_invite::{
    self, build_public_invite_event, locator_hex, parse_public_invite_event, PublicInviteBundle,
};
use super::send::{delete_own_message, publish_signed_message};
use super::transport::{Query, Transport};
use super::{Channel, Community};
use crate::state::SessionGuard;
use crate::stored_event::event_kind;

/// The active signer for authority actions (bunker support): the live client's signer — which covers a
/// NIP-46 bunker — falling back to the local vault keys when there is no client OR the client has no
/// signer attached (local accounts, headless/CLI paths, and tests). Every keyless control edition +
/// moderation hide signs through this, so a bunker account can create AND administer a community. (The
/// REKEY path is the one exception — its blob locator needs a raw ECDH the signer can't expose, so it
/// still requires a local key; the ban/privatize flows fail-fast for bunker accounts.)
async fn active_signer() -> Result<std::sync::Arc<dyn nostr_sdk::prelude::NostrSigner>, String> {
    if let Some(client) = crate::state::nostr_client() {
        if let Ok(s) = client.signer().await {
            return Ok(s);
        }
    }
    let keys = crate::state::MY_SECRET_KEY.to_keys().ok_or("no signer available (no client and no local key)")?;
    Ok(std::sync::Arc::new(keys))
}

/// Create a brand-new Community end-to-end: mint keys + the default channel, persist
/// it locally, and publish its GroupRoot + ChannelMetadata to the Community's relays.
/// Returns the created Community. (The caller then runs the subscription refresh so it
/// starts receiving.)
pub async fn create_community<T: Transport + ?Sized>(
    transport: &T,
    name: &str,
    default_channel_name: &str,
    relays: Vec<String>,
) -> Result<Community, String> {
    let session = SessionGuard::capture();
    let mut community = Community::create(name, default_channel_name, relays);
    // Owner attestation — MANDATORY: a community cannot exist without the root that anchors its
    // authority graph. It binds the community id to the creator's identity, signed by the owner's identity
    // signer. The proven owner is later DERIVED by verifying this, never an unverified claim. Sign via the
    // local vault when present (local accounts + tests), else the
    // active client's signer (bunker / NIP-46). No signer at all → creation fails, by design.
    let owner_pk = crate::state::my_public_key().ok_or("cannot create a community without an identity")?;
    let unsigned = super::owner::build_owner_attestation_unsigned(owner_pk, &community.id.to_hex());
    // Use the local vault ONLY if it actually holds the active identity's key — else a stale/mismatched
    // local secret would sign the attestation as the WRONG owner (or break verification). On mismatch,
    // fall through to the client signer, which is the authority that produced `my_public_key()`.
    let attestation = if let Some(keys) = crate::state::MY_SECRET_KEY.to_keys().filter(|k| k.public_key() == owner_pk) {
        unsigned.sign_with_keys(&keys).map_err(|e| format!("sign owner attestation: {e}"))?
    } else if let Some(client) = crate::state::nostr_client() {
        let signer = client.signer().await.map_err(|e| format!("no signer for owner attestation: {e}"))?;
        unsigned.sign(&signer).await.map_err(|e| format!("sign owner attestation: {e}"))?
    } else {
        return Err("cannot create a community without an identity signer (the owner attestation is mandatory)".to_string());
    };
    community.owner_attestation = Some(attestation.as_json());
    // Minting + the DB write straddle the (above) signer round-trip, so re-check before persist.
    if !session.is_valid() {
        return Err("account changed during community creation".to_string());
    }
    // CREATION is the deliberate exception to publish-first: we save locally BEFORE publishing because
    // (a) no peers exist yet, so there is no shared view to diverge from, and (b) the keys are
    // fresh-random — losing them (e.g. by rolling back on a publish hiccup) would orphan the community
    // irrecoverably. A failed publish leaves a local community the owner can re-publish
    // (`republish_community_metadata`), not a cross-member divergence.
    crate::db::community::save_community(&community)?;

    // The owner signs every genesis edition with their REAL identity (keyless control plane) via the
    // active signer — local vault OR a NIP-46 bunker.
    let signer = active_signer().await?;
    let cid = community.id.to_hex();
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // The genesis control plane: GroupRoot (vsk=0) + each channel's display metadata (vsk=2) + the
    // auto Admin role (vsk=1), all real-npub 3308 editions signed by the owner. The Admin role is
    // DATA, not a hardcoded flag (Mod/custom roles are additive later); the owner takes no grant (owner
    // = implicit position 0, never a Role). Build + collect each (entity_hex, self_hash) head, publish
    // each, and only AFTER every publish succeeds record the heads — so a mid-create publish failure
    // never leaves heads for a partially-published genesis (which would make a later base rotation's
    // re-anchor coverage gate trip forever on an entity the relay never received).
    let admin = super::roles::Role::admin(crate::simd::hex::bytes_to_hex_32(&super::random_32()));
    let root_meta = super::metadata::CommunityMetadata::of(&community);
    let root_inner = super::roster::build_community_root_edition_unsigned(owner_pk, &community.id, &root_meta, 1, None, created, None)?
        .sign(&signer).await.map_err(|e| format!("sign genesis group-root: {e}"))?;
    let role_inner = super::roster::build_role_edition_unsigned(owner_pk, &admin, 1, None, created, None)?
        .sign(&signer).await.map_err(|e| format!("sign genesis admin-role: {e}"))?;
    // (entity_hex, self_hash, inner_id-for-display-entities). The GroupRoot + channels record their
    // inner_id so a same-version genesis fork resolves by the deterministic tiebreak; the role doesn't
    // converge (authority record), so it carries None.
    let mut heads: Vec<(String, [u8; 32], Option<[u8; 32]>)> = vec![
        (cid.clone(), super::version::edition_hash(&community.id.0, 1, None, root_inner.content.as_bytes()), Some(root_inner.id.to_bytes())),
        (admin.role_id.clone(), super::version::edition_hash(&crate::simd::hex::hex_to_bytes_32(&admin.role_id), 1, None, role_inner.content.as_bytes()), None),
    ];
    let mut to_publish: Vec<Event> = vec![
        super::roster::seal_control_edition(&Keys::generate(), &root_inner, &community.server_root_key, &community.id, community.server_root_epoch)?,
        super::roster::seal_control_edition(&Keys::generate(), &role_inner, &community.server_root_key, &community.id, community.server_root_epoch)?,
    ];
    for channel in &community.channels {
        let meta = super::metadata::ChannelMetadata { name: channel.name.clone() };
        let inner = super::roster::build_channel_metadata_edition_unsigned(owner_pk, &channel.id, &meta, 1, None, created, None)?
            .sign(&signer).await.map_err(|e| format!("sign genesis channel-metadata: {e}"))?;
        heads.push((channel.id.to_hex(), super::version::edition_hash(&channel.id.0, 1, None, inner.content.as_bytes()), Some(inner.id.to_bytes())));
        to_publish.push(super::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, community.server_root_epoch)?);
    }
    // Publish the genesis editions durably: each returns once a relay ACKs (the laggards thread in the
    // background) and throws if NO relay accepts within the confirm window — so a dead relay set fails the
    // create loudly instead of recording heads for editions that never reached the network.
    for outer in &to_publish {
        transport.publish_durable(outer, &community.relays).await?;
    }
    // Every edition reached at least one relay — now record each head + cache the Admin role (gated on the
    // session still being ours, so a mid-publish account swap doesn't write into the wrong account).
    if session.is_valid() {
        for (entity_hex, hash, inner_id) in &heads {
            let _ = match inner_id {
                Some(id) => crate::db::community::set_edition_head_with_id(&cid, entity_hex, 1, hash, id),
                None => crate::db::community::set_edition_head(&cid, entity_hex, 1, hash),
            };
        }
        let roster = super::roles::CommunityRoles { roles: vec![admin], grants: Vec::new() };
        let _ = crate::db::community::set_community_roles(&cid, &roster, created as i64);
    }
    Ok(community)
}

/// Publish a Community message and retain its ephemeral key in the account DB so the
/// sender can delete it later. Returns the published outer event.
pub async fn send_message<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    channel: &Channel,
    author: &Keys,
    content: &str,
    ms: u64,
) -> Result<Event, String> {
    let session = SessionGuard::capture();
    // Build + sign the inner explicitly so we know the message_id (the deletion key) up
    // front, then publish via the signed path. Identical wire output to the old
    // publish_message route.
    let inner = super::envelope::build_inner_event(author.public_key(), &channel.id, channel.epoch, content, ms, None)
        .sign_with_keys(author)
        .map_err(|e| e.to_string())?;
    let (outer, ephemeral) = publish_signed_message(transport, community, channel, &inner, false).await?;
    // The publish straddled network I/O; bail before writing to the (possibly
    // swapped) account DB.
    if !session.is_valid() {
        return Err("account changed during send; not persisting message key".to_string());
    }
    crate::db::community::store_message_key(&inner.id.to_hex(), &outer.id.to_hex(), &ephemeral, &community.relays)?;
    Ok(outer)
}

/// Publish a message whose inner authorship event was signed externally (via the active
/// signer — local OR bunker) and retain its ephemeral key. Use this from the command
/// layer where `client.signer()` is available; it gives bunker accounts send parity with
/// DMs. (Local-only callers/tests can use [`send_message`].)
pub async fn send_signed_message<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    channel: &Channel,
    inner: &Event,
) -> Result<Event, String> {
    let session = SessionGuard::capture();
    let (outer, ephemeral) = publish_signed_message(transport, community, channel, inner, false).await?;
    if !session.is_valid() {
        return Err("account changed during send; not persisting message key".to_string());
    }
    crate::db::community::store_message_key(&inner.id.to_hex(), &outer.id.to_hex(), &ephemeral, &community.relays)?;
    Ok(outer)
}

/// Announce presence (join/leave) into a channel: a kind-3306 inner signed by the active identity,
/// published under a fresh ephemeral outer. Content is `"leave"`, plain `"join"`, or — for a join via a
/// public invite — a small JSON `{"by":"<inviter npub>","l":"<label>"}` carrying attribution (which
/// link/source brought this member; members-only). Client best-practice (not enforced); no deletion key
/// retained. Callers treat failure as non-fatal. `attribution` = `Some((inviter_npub, label))` on an
/// invite-join, else `None`.
/// Build + sign a presence (3306) inner event WITHOUT publishing. Lets the caller record the local
/// system event first (memory→DB, like an outgoing message) and publish in the background — the relay
/// echo then dedups by this inner's id. `inner.id` is the system-event dedup key.
pub async fn build_presence(
    channel: &Channel,
    joined: bool,
    attribution: Option<(String, Option<String>)>,
) -> Result<nostr_sdk::Event, String> {
    let author_pk = crate::state::my_public_key().ok_or("not logged in")?;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let content = match (joined, attribution) {
        (false, _) => "leave".to_string(),
        (true, Some((by, label))) => serde_json::json!({ "by": by, "l": label }).to_string(),
        (true, None) => "join".to_string(),
    };
    let unsigned = super::envelope::build_inner_typed(
        author_pk, &channel.id, channel.epoch, event_kind::COMMUNITY_PRESENCE, &content, ms, None, &[],
    );
    let signer = active_signer().await?;
    unsigned.sign(&signer).await.map_err(|e| format!("Failed to sign presence: {e}"))
}

/// Publish a pre-built presence inner (from [`build_presence`]) to the channel's recipient set.
pub async fn publish_presence_event<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    channel: &Channel,
    inner: &nostr_sdk::Event,
) -> Result<(), String> {
    let _ = publish_signed_message(transport, community, channel, inner, true).await?;
    Ok(())
}

pub async fn publish_presence<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    channel: &Channel,
    joined: bool,
    attribution: Option<(String, Option<String>)>,
) -> Result<(), String> {
    let inner = build_presence(channel, joined, attribution).await?;
    publish_presence_event(transport, community, channel, &inner).await
}

/// Publish a WebXDC realtime peer signal (3310) into a channel: an advertisement of the local
/// Iroh node for a Mini App session (`node_addr` = Some) or a peer-left (`node_addr` = None).
/// The Community-transport twin of the NIP-17 peer-advertisement/peer-left DM rumors — signed
/// by the member's real identity (a member can't forge another player's presence), sealed under
/// the channel epoch key like presence. Callers treat failure as non-fatal (a missed ad only
/// delays discovery; the next re-advertise covers it).
pub async fn publish_webxdc_signal<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    channel: &Channel,
    topic_id: &str,
    node_addr: Option<&str>,
) -> Result<(), String> {
    let author_pk = crate::state::my_public_key().ok_or("not logged in")?;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let content = match node_addr {
        Some(addr) => serde_json::json!({ "op": "ad", "topic": topic_id, "addr": addr }).to_string(),
        None => serde_json::json!({ "op": "left", "topic": topic_id }).to_string(),
    };
    let unsigned = super::envelope::build_inner_typed(
        author_pk, &channel.id, channel.epoch, event_kind::COMMUNITY_WEBXDC, &content, ms, None, &[],
    );
    let signer = active_signer().await?;
    let inner = unsigned.sign(&signer).await.map_err(|e| format!("Failed to sign webxdc signal: {e}"))?;
    let _ = publish_signed_message(transport, community, channel, &inner, true).await?;
    Ok(())
}

/// Persist an inbound WebXDC peer signal as a kind-30078 event row — the SAME shape the DM
/// peer-advertisement handler writes (content `peer-advertisement`/`peer-left`, `reference_id`
/// = topic, `webxdc-topic`/`webxdc-node-addr` tags) — so the miniapp layer's
/// `get_active_peer_advertisements` (latest-per-npub, left-tombstone-aware) reads both
/// transports identically. This is what lets a member who closed Vector mid-session rediscover
/// the active players on reopen. Idempotent via `event_exists`.
pub async fn persist_webxdc_signal(
    channel_hex: &str,
    npub: &str,
    topic_id: &str,
    node_addr: Option<&str>,
    event_id: &str,
    created_at: u64,
) {
    if crate::db::events::event_exists(event_id).unwrap_or(true) {
        return;
    }
    // Sender-claimed timestamp: clamp into the near future so a forged far-future ad
    // can't outrank every later genuine peer-left in the latest-per-npub read.
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let created_at = created_at.min(now_secs + 300);
    let Ok(chat_id) = crate::db::id_cache::get_or_create_chat_id(channel_hex) else { return };
    let mut tags = vec![
        vec!["webxdc-topic".to_string(), topic_id.to_string()],
        vec!["d".to_string(), "vector-webxdc-peer".to_string()],
    ];
    if let Some(addr) = node_addr {
        tags.push(vec!["webxdc-node-addr".to_string(), addr.to_string()]);
    }
    let event = crate::stored_event::StoredEvent {
        id: event_id.to_string(),
        kind: crate::stored_event::event_kind::APPLICATION_SPECIFIC,
        chat_id,
        user_id: None,
        content: if node_addr.is_some() { "peer-advertisement" } else { "peer-left" }.to_string(),
        tags,
        reference_id: Some(topic_id.to_string()),
        created_at,
        received_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        mine: false,
        pending: false,
        failed: false,
        wrapper_event_id: None,
        npub: Some(npub.to_string()),
        preview_metadata: None,
    };
    if let Err(e) = crate::db::events::save_event(&event).await {
        crate::log_warn!("[community] failed to persist webxdc peer signal: {e}");
    }
}

/// Publish a cooperative kick (3309) of `target_hex` into `channel`: a real-npub-signed inner directive
/// (content = the target's hex pubkey) carrying the actor's `vac` authority citation. NOT a rekey and NOT
/// folded — the kicked client self-removes on receipt (drops the community keys + wipes local chat data);
/// peers drop the target from their observed member list. The actor must hold `KICK` and strictly outrank
/// the target (the owner is never a valid target); this is the sender-side half of the rule peers
/// re-verify on receipt. For a malicious target that ignores the kick, escalate to a BAN.
/// Signs via the active client signer, so a bunker (NIP-46) identity works without exposing the secret.
/// On removal (kick/ban), strip the target's roles so their authority doesn't dangle — a removed admin
/// would otherwise silently regain @admin on re-add, and the roster would keep listing a non-member as an
/// admin. Best-effort: a no-op if the target holds no role; a SKIP (logged) if the remover lacks
/// `MANAGE_ROLES`/outrank for any held role (a future mid-tier remover) — the kick/ban still neutralizes
/// them, and leaving the grant beats a partial strip. Publishes the full revoke (empty grant) when
/// authorized for EVERY held role.
async fn strip_member_roles_on_removal<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    member_hex: &str,
) {
    let cid = community.id.to_hex();
    let roster = match crate::db::community::get_community_roles(&cid) {
        Ok(r) => r,
        Err(_) => return,
    };
    let held: Vec<String> = roster
        .grants
        .iter()
        .find(|g| g.member == member_hex)
        .map(|g| g.role_ids.clone())
        .unwrap_or_default();
    if held.is_empty() {
        return; // plain member — no authority to strip
    }
    for role_id in &held {
        if caller_can_manage_role(community, &roster, role_id, member_hex).is_err() {
            crate::log_warn!(
                "removal: not authorized to revoke role {role_id} of {member_hex}; leaving the grant (kick/ban still neutralizes)"
            );
            return;
        }
    }
    if let Err(e) = set_member_grant(transport, community, member_hex, Vec::new()).await {
        crate::log_warn!("removal: role-strip publish failed for {member_hex}: {e}");
    }
}

pub async fn publish_kick<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    channel: &Channel,
    target_hex: &str,
) -> Result<String, String> {
    let author_pk = crate::state::my_public_key().ok_or("not logged in")?;
    let me = author_pk.to_hex();
    let cid = community.id.to_hex();
    // hierarchy gate: hold KICK + strictly outrank the target (owner is never a valid target). Mirror
    // of publish_banlist's gate; peers re-verify the same rule against their floor-protected roster.
    {
        let owner = proven_owner_hex(community);
        let roster = crate::db::community::get_community_roles(&cid).unwrap_or_default();
        if !roster.can_act_on_member(&me, owner.as_deref(), target_hex, super::roles::Permissions::KICK) {
            return Err("you can't kick a member who outranks you (or the owner)".to_string());
        }
    }
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    // pinned authority: a non-owner kicker cites the grant that authorizes them (owner cites nothing).
    let citation = authority_citation(community, &me);
    let extra: Vec<nostr_sdk::prelude::Tag> = citation.iter().map(|c| c.to_tag()).collect();
    let unsigned = super::envelope::build_inner_full(
        author_pk, &channel.id, channel.epoch, event_kind::COMMUNITY_KICK, target_hex, ms, None, &[], &extra,
    );
    let signer = active_signer().await?;
    let inner = unsigned.sign(&signer).await.map_err(|e| format!("Failed to sign kick: {e}"))?;
    publish_signed_message(transport, community, channel, &inner, true).await?;
    // Removal strips authority: revoke the kicked member's roles too (best-effort) so a kicked admin
    // doesn't rejoin (fresh invite) silently still admin, and no non-member lingers in the roster.
    strip_member_roles_on_removal(transport, community, target_hex).await;
    // Return the inner id so the caller can record a local "Member Left" that dedups with the relay echo.
    Ok(inner.id.to_hex())
}


/// Replace the Community banlist and publish it as a real-npub-signed 3308 EDITION (vsk=4) at the
/// community-scoped banlist locator (keyless; foldable + re-anchorable). `banned_hex`
/// is the full new list (latest-wins). The actor's inner signature IS the authority proof; every member
/// re-verifies it held `BAN` against the authorized roster on receipt. Publish FIRST, then
/// persist locally on success — a failed publish must not leave us enforcing a ban no one else sees.
pub async fn publish_banlist<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    banned_hex: &[String],
) -> Result<(), String> {
    let session = SessionGuard::capture();
    let cid = community.id.to_hex();
    // Keyless model: sign with the actor's own identity via the active signer (local vault OR a NIP-46
    // bunker). `author` is the active pubkey; `signer` signs the unsigned edition below.
    let signer = active_signer().await?;
    let actor_pk = crate::state::my_public_key().ok_or("no local identity to sign the banlist edition")?;
    // hierarchy gate: the actor must hold BAN and strictly outrank every member in the DELTA
    // both those being ADDED (ban) and those being REMOVED (unban). Gating only additions would let a
    // low-ranked admin undo a superior's ban or wholesale-clear the list. The owner is never a valid
    // target. This is the sender-side half of the rule peers re-verify on receipt.
    {
        let me = actor_pk.to_hex();
        let owner = proven_owner_hex(community);
        let roster = crate::db::community::get_community_roles(&cid).unwrap_or_default();
        let current: std::collections::HashSet<String> =
            crate::db::community::get_community_banlist(&cid).unwrap_or_default().into_iter().collect();
        let next: std::collections::HashSet<&str> = banned_hex.iter().map(|s| s.as_str()).collect();
        let added = banned_hex.iter().filter(|n| !current.contains(n.as_str()));
        let removed = current.iter().filter(|n| !next.contains(n.as_str()));
        for target in added.chain(removed) {
            if !roster.can_act_on_member(&me, owner.as_deref(), target, super::roles::Permissions::BAN) {
                return Err("you can't ban or unban a member who outranks you (or the owner)".to_string());
            }
        }
    }
    // Fail-fast (bunker boundary): a newly-banned member in a PRIVATE community must be READ-CUT (a
    // base rekey), and a rekey needs a RAW local key — its blob locator is an ECDH a NIP-46 bunker can't
    // expose. Refuse BEFORE publishing anything, so we never half-apply (publish a ban we then can't
    // enforce, leaving a "banned but still readable" member). Covers a pending prior cut too. A community
    // admin who holds a local key can carry out the ban. (Public bans + unbans don't rekey → allowed.)
    {
        let prev: std::collections::HashSet<String> =
            crate::db::community::get_community_banlist(&cid).unwrap_or_default().into_iter().collect();
        let adds = banned_hex.iter().any(|n| !prev.contains(n.as_str()));
        let cut_needed = (adds || crate::db::community::get_read_cut_pending(&cid)?) && !is_public(community)?;
        if cut_needed && crate::state::MY_SECRET_KEY.to_keys().is_none() {
            return Err("Banning someone from a private community cuts their read access, which needs a key rotation your account can't perform: it signs remotely (a NIP-46 bunker), and a rotation requires a local key. Ask a community admin who holds a local key to carry out the ban.".to_string());
        }
    }
    // Next version in the banlist's own chain (single community-wide entity at the banlist locator).
    let entity_id = super::derive::banlist_locator(&community.id);
    let entity_hex = crate::simd::hex::bytes_to_hex_32(&entity_id);
    let (version, prev_hash) = match crate::db::community::get_edition_head(&cid, &entity_hex)? {
        Some((v, h)) => (v + 1, Some(h)),
        None => (1, None),
    };
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // pinned authority: a non-owner banner cites the grant edition that authorizes them, so peers
    // resolve the ban against that exact grant version (not their live roster). The owner cites nothing.
    let citation = authority_citation(community, &actor_pk.to_hex());
    let unsigned = super::roster::build_banlist_edition_unsigned(actor_pk, &community.id, banned_hex, version, prev_hash.as_ref(), created_at, citation.as_ref())?;
    let inner = unsigned.sign(&signer).await.map_err(|e| format!("sign banlist edition: {e}"))?;
    let outer = super::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, community.server_root_epoch)?;
    let self_hash = super::version::edition_hash(&entity_id, version, prev_hash.as_ref(), inner.content.as_bytes());

    // Did this ban ADD anyone (vs the list we held)? Captured BEFORE the persist below so we can decide
    // whether to cut read access. Unbans (removals) never rekey.
    let newly_added: Vec<String> = {
        let prev: std::collections::HashSet<String> =
            crate::db::community::get_community_banlist(&cid).unwrap_or_default().into_iter().collect();
        banned_hex.iter().filter(|n| !prev.contains(n.as_str())).cloned().collect()
    };
    let newly_banned = !newly_added.is_empty();

    // Publish FIRST — advancing the head before a fallible publish would leave a phantom head (the next
    // edition cites an unpublished predecessor → fold quarantines it forever). Re-check the session after
    // the await: it may have straddled an account swap, and persisting then would write the wrong account.
    transport.publish_durable(&outer, &community.relays).await?;
    if session.is_valid() {
        crate::db::community::set_community_banlist(&cid, banned_hex, created_at as i64)?;
        crate::db::community::set_edition_head(&cid, &entity_hex, version, &self_hash)?;
    }

    // Removal strips authority: revoke the roles of every NEWLY-banned member so their grant doesn't dangle
    // — a banned admin would otherwise silently regain @admin on unban, and the roster would keep listing a
    // removed member as admin. Best-effort, and BEFORE the read-cut so its re-anchor carries the revoked
    // (empty) grant forward.
    if session.is_valid() {
        for member_hex in &newly_added {
            strip_member_roles_on_removal(transport, community, member_hex).await;
        }
    }

    // rekey-on-removal: in a PRIVATE community, a newly-banned member must also lose READ access, so
    // re-seal the base to the surviving observed participants (`community_member_activity` excludes the
    // banlist, so the just-banned member is dropped). A PUBLIC community does NOT rotate the base
    // (anti-memberlist: no recipient set to wrap to, and a banned member could re-enter via a link
    // anyway) — there the banlist alone suppresses them, and the UI must say "blocked," not "removed."
    // Runs after the banlist is persisted (so the observed set already excludes the banned).
    //
    // rekey-on-removal read-cut. Re-seal if this ban ADDED someone, OR a prior re-seal is still
    // pending (`read_cut_pending`) — the latter decouples recovery from the add-delta (which the durable
    // banlist persist consumes), so a re-seal that failed on a previous ban is RETRIED here even when this
    // call adds no one. Mark pending BEFORE the attempt (durable intent) and clear ONLY on success: a
    // failure (total relay outage / re-anchor-withhold / mid-ban swap) leaves the flag set, so the next
    // ban OR a community sync ([`retry_pending_read_cut`]) re-attempts it — no "blocked but not read-cut"
    // member survives a transient failure. The re-seal publish is itself durable (×30 per relay).
    let need_cut = (newly_banned || crate::db::community::get_read_cut_pending(&cid)?)
        && session.is_valid()
        && !is_public(community)?;
    if need_cut {
        // `newly_banned` is a fresh exclusion delta → force a base epoch past the removal; otherwise this is
        // a resume of an interrupted prior cut → keep its in-flight target.
        run_read_cut(transport, community, newly_banned).await?;
    }
    Ok(())
}

/// Is the local user in this community's (folded, cached) banlist? Drives BAN self-removal: a
/// banned member tears down locally (drop the community keys + wipe local chat data) exactly like a kick,
/// but CANNOT rejoin — re-detecting the ban on any later sync re-removes them, and admins can't invite a
/// banned npub. Reads the cached banlist, so refresh it via [`fetch_and_apply_banlist`] first for an
/// authoritative (realtime or boot) check.
pub fn am_i_banned(community: &Community) -> bool {
    let me = match crate::state::my_public_key() {
        Some(p) => p.to_hex(),
        None => return false,
    };
    crate::db::community::get_community_banlist(&community.id.to_hex())
        .unwrap_or_default()
        .iter()
        .any(|b| b == &me)
}

/// Retry an outstanding PRIVATE-community read-cut re-seal, if one is pending. Called from the sync
/// path so a re-seal that failed during a ban (e.g. a relay outage) AUTO-RECOVERS on the owner's next
/// community sync — no manual re-ban needed. No-op if nothing is pending. If the community has since gone
/// PUBLIC the read-cut is moot (anti-memberlist: a Public ban doesn't rotate the base), so the stale flag
/// is cleared. Best-effort + idempotent; the re-seal authority (BAN) is enforced by `rotate_server_root`.
pub async fn retry_pending_read_cut<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
) -> Result<(), String> {
    let cid = community.id.to_hex();
    if !crate::db::community::get_read_cut_pending(&cid)? {
        return Ok(());
    }
    if is_public(community)? {
        crate::db::community::set_read_cut_pending(&cid, false)?; // moot in Public mode
        return Ok(());
    }
    // Reload so the re-seal rotates from the FRESHEST root/epoch — the caller's `community` struct may
    // predate a recent rotation, and rotating from a stale root would address the rekey under the wrong
    // prior-root pseudonym. Pure resume (`fresh = false`): keep the in-flight target so an interrupted cut
    // finishes without forcing an extra base rotation.
    let fresh = crate::db::community::load_community(&community.id)?.ok_or("community no longer present")?;
    run_read_cut(transport, &fresh, false).await
}

/// Fetch the Community's control plane and apply the folded banlist locally. The banlist is a 3308
/// edition at the community-scoped banlist locator; the folded head is applied only if its signer held
/// `BAN` in the authorized roster (the keyless authority gate) and it is strictly newer than the
/// banlist edition we hold (refuse-downgrade by version). No authorized edition → local unchanged.
/// ONE REQ for the entire control plane: fetch every kind-3308 edition at the control pseudonym(s) and
/// fold the full roster (banlist + roles + invite-links + metadata) in a single pass. The per-slice
/// `fetch_and_apply_*` functions and `fetch_and_apply_control` share this, so a sync/join/boot folds ONCE
/// instead of issuing four identical REQs. Fetches at the CURRENT server-root epoch (re-anchoring keeps
/// the complete plane reachable there); `z_tags` is a Vec so the addressing can extend if ever needed.
async fn fetch_control_folded<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
) -> Result<super::roster::FoldedRoster, String> {
    // The control plane lives at the CURRENT server-root epoch — a rotation re-anchors it there, and all
    // live publishes (grants/banlist/metadata/invite-links) seal at the same epoch. Fetch exactly that one
    // (NOT a 0..=epoch range — a post-rotation joiner can't derive prior-epoch pseudonyms; the re-anchor
    // guarantees the complete current plane is reachable here).
    let z_tags = vec![super::roster::control_pseudonym(&community.server_root_key, &community.id, community.server_root_epoch)];
    let query = Query { kinds: vec![event_kind::COMMUNITY_CONTROL], z_tags, ..Default::default() };
    let raw = transport.fetch(&query, &community.relays).await?;
    // Bound the AEAD work too (fold_roster re-caps the verify/fold): a relay
    // flooding the coordinate must not buy unbounded decrypt attempts.
    let inner_editions: Vec<Event> = raw
        .iter()
        .take(super::roster::MAX_CONTROL_EDITIONS)
        .filter_map(|ev| super::roster::open_control_edition(ev, &community.server_root_key).ok())
        .collect();
    // VALID (opened) editions, NOT raw.len() — the admin-write isolation signal must mean "a relay served
    // our actual control plane," so a relay returning only junk/unopenable events at the coordinate doesn't
    // count as a response. (Floors still guard against stale/rollback; this just stops content-withholding
    // from masquerading as connectivity.)
    let fetched = inner_editions.len();
    // Fold from the persisted per-entity floors (refuse-downgrade) so a withholding relay can't roll
    // an entity's chain back to a since-revoked version. EPOCH-PRIMARY: seed only the floors recorded
    // at the CURRENT epoch — a head from a PRIOR epoch belongs to a superseded founding, so that entity
    // folds fresh from the new epoch's v1 genesis (which anchors cleanly at floor 0; not Policy-B, since a
    // compacted genesis carries no prev_hash). Within the current epoch, refuse-downgrade + floor anchoring hold.
    let current_epoch = community.server_root_epoch.0;
    let floors: std::collections::HashMap<String, (u64, [u8; 32])> =
        crate::db::community::get_all_edition_heads_epoched(&community.id.to_hex())?
            .into_iter()
            .filter(|(_, (epoch, _, _))| *epoch == current_epoch)
            .map(|(entity, (_epoch, version, hash))| (entity, (version, hash)))
            .collect();
    let mut folded = super::roster::fold_roster(&inner_editions, &community.id, &floors);
    folded.fetched = fetched; // openable editions the relays served (isolation signal for admin-write guards)
    Ok(folded)
}

/// Fetch the control plane ONCE and apply every slice — banlist, roles, invite links, metadata — from a
/// single REQ + single fold. Sync/join/boot call THIS instead of the four `fetch_and_apply_*` in sequence
/// (which was four identical REQs). Banlist is applied first so a caller's subsequent `am_i_banned` sees the
/// freshest list. Each slice is best-effort; one failing doesn't abort the rest. (Solo callers that need a
/// single slice — e.g. revoke refreshing invite links — still use the individual `fetch_and_apply_*`.)
pub async fn fetch_and_apply_control<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
) -> Result<usize, String> {
    let session = SessionGuard::capture();
    let cid = community.id.to_hex();
    // binary seal: once dissolved, the control fold STOPS advancing — no further editions apply (the
    // inbound message path likewise drops everything). Cheap flag check before any fetch.
    if crate::db::community::get_community_dissolved(&cid)? {
        return Ok(0);
    }
    let folded = fetch_control_folded(transport, community).await?;
    if !session.is_valid() {
        return Err("account changed during control fetch".to_string());
    }
    // tombstone: if a GroupDissolved edition at the locator was signed by the PROVEN owner (derived
    // via the deed at fold time, never a cached field), SEAL the community and stop. Fail-closed: an
    // unreadable deed (no proven owner) or a non-owner signer is REJECTED — we stay in the prior state,
    // never death-by-default. THIS fold pass IS the "one bounded final drain": the banlist/roles/
    // metadata applied below are the last accepted control; subsequent syncs see the flag and drop.
    // Detect an owner tombstone via EITHER the rotation-stable coordinate probe (the cross-epoch path: a
    // post-rotation joiner only derives a later root + never fetches the publish-epoch control_pseudonym,
    // but always derives `dissolved_pseudonym`) OR the control-plane fold (the current-epoch fast path).
    // Owner derived from the deed at fold time; fail-closed (no proven owner / non-owner signer ⇒ rejected).
    if let Some(owner) = proven_owner_hex(community) {
        let by_fold = folded.dissolved_by.iter().any(|s| s.to_hex() == owner);
        let by_probe = !by_fold && dissolved_tombstone_present(transport, community, &owner).await;
        if by_fold || by_probe {
            // This fold pass IS the "one bounded final drain": apply the last accepted control, then
            // seal. Subsequent syncs short-circuit on the flag above and drop everything.
            let _ = fetch_and_apply_banlist_inner(transport, community, Some(folded.clone())).await;
            let _ = fetch_and_apply_roles_inner(transport, community, Some(folded.clone())).await;
            let _ = fetch_and_apply_invite_links_inner(transport, community, Some(folded.clone())).await;
            let _ = fetch_and_apply_metadata_inner(transport, community, Some(folded.clone())).await;
            if session.is_valid() {
                crate::db::community::set_community_dissolved(&cid)?;
                // Notify the UI to re-render the dead community live (lock composer + end divider). Emitting
                // from the single seal point covers EVERY caller — sync, boot, realtime refresh — not just the
                // realtime path. Fires once: the short-circuit above skips it on every subsequent fetch.
                crate::emit_event("community_refreshed", &serde_json::json!({ "community_id": cid }));
            }
            return Ok(folded.fetched);
        }
    }
    // Openable control editions this single fetch served — the caller's "≥1 relay returned our actual plane"
    // isolation signal (no separate probe fetch needed).
    let fetched = folded.fetched;
    let _ = fetch_and_apply_banlist_inner(transport, community, Some(folded.clone())).await;
    let _ = fetch_and_apply_roles_inner(transport, community, Some(folded.clone())).await;
    let _ = fetch_and_apply_invite_links_inner(transport, community, Some(folded.clone())).await;
    let _ = fetch_and_apply_metadata_inner(transport, community, Some(folded)).await;
    Ok(fetched)
}

pub async fn fetch_and_apply_banlist<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
) -> Result<Vec<String>, String> {
    fetch_and_apply_banlist_inner(transport, community, None).await
}

async fn fetch_and_apply_banlist_inner<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    prefolded: Option<super::roster::FoldedRoster>,
) -> Result<Vec<String>, String> {
    let session = SessionGuard::capture();
    let cid = community.id.to_hex();
    let folded = match prefolded {
        Some(f) => f,
        None => fetch_control_folded(transport, community).await?,
    };
    // Authority: the banlist signer must hold BAN in the AUTHORIZED roster (delegation-chain filtered),
    // not merely be validly-signed. A demoted/never-authorized signer's banlist is dropped.
    let owner = proven_owner_hex(community);
    let authorized = super::roster::authorize_delegation(&folded, owner.as_deref());
    if !session.is_valid() {
        return Err("account changed during banlist fetch".to_string());
    }
    if let (Some(author), Some(head)) = (folded.banlist_author, &folded.banlist_head) {
        // Authority is per-target, not just the BAN bit: the signer must strictly OUTRANK every member
        // in the delta between the list we hold and the folded list (both newly-banned and newly-unbanned)
        // — the same check the sender ran. A bit-only check would let a low-ranked BAN-holder ban or
        // unban a peer/superior (or the owner). Owner is never a valid target (folds out of can_act_on_member).
        let author_hex = author.to_hex();
        let held: std::collections::HashSet<String> =
            crate::db::community::get_community_banlist(&cid)?.into_iter().collect();
        let next: std::collections::HashSet<&str> = folded.banned.iter().map(|s| s.as_str()).collect();
        let added = folded.banned.iter().filter(|n| !held.contains(n.as_str()));
        let removed = held.iter().filter(|n| !next.contains(n.as_str()));
        // version-pinned authority: the banner's edition cites the grant that authorizes them; we
        // apply only if we have folded that grant to AT LEAST the cited version (a complete, un-forked
        // view — else fail closed, never act on a partial authority view). The per-target outrank below
        // is then resolved against the CURRENT authorized roster, so a since-demoted banner is dropped
        // there (refuse-superseded). Owner cites nothing and is supreme.
        let citation = folded.banlist_head.as_ref().and_then(|h| h.citation.as_ref());
        let banner_grant_hex = crate::simd::hex::bytes_to_hex_32(&super::derive::grant_locator(&community.id, &author.to_bytes()));
        let pinned = super::roster::authority_citation_satisfied(&folded.heads, owner.as_deref(), &author_hex, &banner_grant_hex, citation);
        let authed = pinned
            && added.chain(removed).all(|target| {
                authorized.can_act_on_member(&author_hex, owner.as_deref(), target, super::roles::Permissions::BAN)
            });
        let held_version = crate::db::community::get_edition_head(&cid, &head.entity_hex)?.map(|(v, _)| v).unwrap_or(0);
        if authed && head.version > held_version {
            crate::db::community::set_community_banlist(&cid, &folded.banned, head.version as i64)?;
            crate::db::community::set_edition_head(&cid, &head.entity_hex, head.version, &head.self_hash)?;
            return Ok(folded.banned);
        }
    }
    // Nothing newer/authorized applied — report the banlist we still hold, not an empty list.
    crate::db::community::get_community_banlist(&cid)
}

/// Set a member's complete role set (owner/admin authority) and publish their per-member
/// Grant event (vsk=3). Empty `role_ids` revokes all of that member's roles. Persists the updated
/// local graph BEFORE the publish await (so our own client reflects it immediately and the write
/// lands in the captured account); the relay echo dedups.
pub async fn set_member_grant<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    member_hex: &str,
    role_ids: Vec<String>,
) -> Result<(), String> {
    let session = SessionGuard::capture();
    // Keyless model: the grant is a real-npub-signed edition. Sign
    // with the actor's own identity via the active signer (local vault OR a NIP-46 bunker).
    let signer = active_signer().await?;
    let actor_pk = crate::state::my_public_key().ok_or("no local identity to sign the grant edition")?;
    let cid = community.id.to_hex();
    let grant = super::roles::MemberGrant { member: member_hex.to_string(), role_ids };

    // Next version in this member's grant chain. The entity coordinate is the member's grant locator,
    // so the head tracks per-member; v+1 cites the held head's self_hash (genesis v1 if none).
    let member_bytes = crate::simd::hex::hex_to_bytes_32(member_hex);
    let entity_id = super::derive::grant_locator(&community.id, &member_bytes);
    let entity_hex = crate::simd::hex::bytes_to_hex_32(&entity_id);
    let (version, prev_hash) = match crate::db::community::get_edition_head(&cid, &entity_hex)? {
        Some((v, h)) => (v + 1, Some(h)),
        None => (1, None),
    };
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Build (real-npub signed inner) + seal under the server-root for the wire. The grant authoring
    // gate (`caller_can_manage_role`) runs in the grant_role/revoke_role callers; this is the encoder.
    // pinned authority: a delegated admin granting a lower member cites the grant that authorizes
    // them, so the delegation chain is verifiable at that version. The owner cites nothing (supreme).
    // (Owner-only granting is the MVP norm, so this is usually `None` — but emitting it now keeps the
    // immutable wire data complete for the delegation-chain verifier, rather than baking in a gap.)
    let citation = authority_citation(community, &actor_pk.to_hex());
    let unsigned = super::roster::build_grant_edition_unsigned(actor_pk, &community.id, &grant, version, prev_hash.as_ref(), created_at, citation.as_ref())?;
    let inner = unsigned.sign(&signer).await.map_err(|e| format!("sign grant edition: {e}"))?;
    let outer = super::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, community.server_root_epoch)?;
    // The new head's self_hash = a hash over the EXACT content bytes the inner committed to (not a
    // re-serialization), so the stored head matches the published edition and the next edition's
    // prev_hash cites it correctly.
    let self_hash = super::version::edition_hash(&entity_id, version, prev_hash.as_ref(), inner.content.as_bytes());

    let is_full_revoke = grant.role_ids.is_empty();
    // Compute the advanced local state in memory (cheap; no DB write yet).
    let mut roster = crate::db::community::get_community_roles(&cid)?;
    roster.grants.retain(|g| g.member != member_hex);
    if !grant.role_ids.is_empty() {
        roster.grants.push(grant);
    }

    // Publish FIRST, then persist the advanced head + roster only on success. Advancing the head
    // before a fallible publish would leave a phantom head: a failed publish means the next edition
    // cites an unpublished predecessor, which every fold quarantines as a gap forever. Re-check the
    // session after the await — it may have straddled an account swap, and persisting then would
    // write into the wrong account (the edition published under the captured one).
    transport.publish_durable(&outer, &community.relays).await?;
    if session.is_valid() {
        crate::db::community::set_community_roles(&cid, &roster, created_at as i64)?;
        crate::db::community::set_edition_head(&cid, &entity_hex, version, &self_hash)?;
    }

    // Revoke-time re-assert (publish-time authority — "Concord Convergence"): a demotion drops the
    // member's authority, so the author-aware fold would orphan any authority-gated entity the member
    // currently HEADS. Re-publish those heads as the actor (the `republish_*` helpers gate on the actor's
    // own permission), so the member's validly-published content survives for EVERY client — fresh joiners
    // included — and a post-demotion forgery can't win. Skip-if-not-head: only entities the member actually
    // heads are re-asserted (the common case publishes nothing). Best-effort + per-entity publish-then-
    // persist inside the helpers (W2). MVP: full revoke only (`role_ids` empty); partial demote is a follow-on.
    if is_full_revoke && session.is_valid() {
        if let Ok(folded) = fetch_control_folded(transport, community).await {
            if session.is_valid() {
                let current = crate::db::community::load_community(&community.id)?.unwrap_or_else(|| community.clone());
                if folded.root_author.map(|a| a.to_hex()).as_deref() == Some(member_hex) {
                    if let Some(meta) = &folded.root_meta {
                        let mut c = current.clone();
                        c.name = meta.name.clone();
                        c.description = meta.description.clone();
                        c.icon = meta.icon.clone();
                        c.banner = meta.banner.clone();
                        let _ = republish_community_metadata(transport, &c).await;
                    }
                }
                for cm in &folded.channel_meta {
                    if cm.author.to_hex() == member_hex
                        && current.channels.iter().any(|ch| ch.id.0 == cm.channel_id)
                    {
                        let _ = republish_channel_metadata(
                            transport, &current, &crate::community::ChannelId(cm.channel_id), &cm.meta.name,
                        ).await;
                    }
                }
            }
        }
    }
    Ok(())
}

/// True iff the local user is the PROVEN owner of this community — derived by verifying the owner
/// attestation against `my_public_key()` (keyless: the owner is the npub that signed the attestation
/// binding this community_id). The check honest clients use to gate
/// owner-only actions (mint invites, set images) and to render the owner crown.
pub fn is_proven_owner(community: &Community) -> bool {
    match crate::state::my_public_key() {
        Some(me) => proven_owner_hex(community).as_deref() == Some(me.to_hex().as_str()),
        None => false,
    }
}

/// True iff the local user may manage roles — i.e. holds the `MANAGE_ROLES` permission.
/// Permission-based, NOT a hardcoded owner check: the owner is simply the uppermost role and holds
/// every permission; any member granted a role carrying `MANAGE_ROLES` qualifies just the same.
pub fn caller_can_manage_roles(community: &Community) -> bool {
    let me = match crate::state::my_public_key() {
        Some(p) => p,
        None => return false,
    };
    let cid = community.id.to_hex();
    let is_owner = community
        .owner_attestation
        .as_ref()
        .and_then(|a| super::owner::verify_owner_attestation(a, &cid))
        .map(|pk| pk == me)
        .unwrap_or(false);
    if is_owner {
        return true; // the uppermost role holds all permissions
    }
    crate::db::community::get_community_roles(&cid)
        .unwrap_or_default()
        .has_permission(&me.to_hex(), super::roles::Permissions::MANAGE_ROLES)
}

/// Does the local user hold `permission` in this community? The generalized [`caller_can_manage_roles`]:
/// owner = supreme (every bit), otherwise the union of their granted roles' bits (the role engine).
/// Drives both the capability report and the producer-side authority gates — no hardcoded owner check.
pub fn caller_has_permission(community: &Community, permission: u64) -> bool {
    let me = match crate::state::my_public_key() {
        Some(p) => p,
        None => return false,
    };
    crate::db::community::get_community_roles(&community.id.to_hex())
        .unwrap_or_default()
        .is_authorized(&me.to_hex(), proven_owner_hex(community).as_deref(), permission)
}

/// Can the local caller grant/revoke `role_id` — i.e. do they hold `MANAGE_ROLES` AND outrank that role's
/// position? The crown's gate, expressed as the POSITION rule (NOT an owner check): the owner is just
/// position 0, so in the single-@admin-role MVP this resolves to "owner only" because the @admin role sits
/// directly below position 0 — but it generalizes to any role hierarchy. `false` if the role is unknown.
pub fn caller_can_manage_role_id(community: &Community, role_id: &str) -> bool {
    let me = match crate::state::my_public_key() {
        Some(p) => p.to_hex(),
        None => return false,
    };
    let roster = crate::db::community::get_community_roles(&community.id.to_hex()).unwrap_or_default();
    let position = match roster.role(role_id) {
        Some(r) => r.position,
        None => return false,
    };
    roster.can_manage_position(&me, proven_owner_hex(community).as_deref(), position)
}

/// The local user's effective management capabilities in a community, resolved purely by the role engine
/// (positions + permission bits; the owner is just the role at position 0 — NOTHING is owner-hardcoded).
/// The frontend gates each management affordance on the matching bit, so an admin whose role carries a
/// permission gets the exact same affordance as the owner.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CommunityCapabilities {
    pub manage_metadata: bool,
    pub manage_channels: bool,
    pub create_invite: bool,
    pub kick: bool,
    pub ban: bool,
    pub manage_messages: bool,
    pub manage_roles: bool,
}

pub fn caller_capabilities(community: &Community) -> CommunityCapabilities {
    use super::roles::Permissions as P;
    let me_hex = match crate::state::my_public_key() {
        Some(p) => p.to_hex(),
        None => return CommunityCapabilities::default(),
    };
    let owner = proven_owner_hex(community);
    let roster = crate::db::community::get_community_roles(&community.id.to_hex()).unwrap_or_default();
    let has = |bit: u64| roster.is_authorized(&me_hex, owner.as_deref(), bit);
    CommunityCapabilities {
        manage_metadata: has(P::MANAGE_METADATA),
        manage_channels: has(P::MANAGE_CHANNELS),
        create_invite: has(P::CREATE_INVITE),
        kick: has(P::KICK),
        ban: has(P::BAN),
        manage_messages: has(P::MANAGE_MESSAGES),
        manage_roles: has(P::MANAGE_ROLES),
    }
}

/// The pinned authority citation the local user attaches to a control action — points at their
/// OWN authorizing Grant edition (stable community-scoped coordinate + its current head version/hash),
/// so every verifier resolves the action's authority against that exact point instead of their own
/// possibly-lagging-or-ahead live roster. `None` when the local user is the proven owner (supreme —
/// owner actions cite nothing) or has no grant head to cite (an unauthorized actor — the send-side
/// authority gate refuses them before a citation would matter). See
/// [`super::roster::authority_citation_satisfied`] for the verifier side.
fn authority_citation(community: &Community, actor_hex: &str) -> Option<super::edition::AuthorityCitation> {
    if proven_owner_hex(community).as_deref() == Some(actor_hex) {
        return None;
    }
    let cid = community.id.to_hex();
    let actor_bytes = crate::simd::hex::hex_to_bytes_32(actor_hex);
    let entity_id = super::derive::grant_locator(&community.id, &actor_bytes);
    let entity_hex = crate::simd::hex::bytes_to_hex_32(&entity_id);
    crate::db::community::get_edition_head(&cid, &entity_hex)
        .ok()
        .flatten()
        .map(|(version, edition_hash)| super::edition::AuthorityCitation { entity_id, version, edition_hash })
}

/// The proven owner's pubkey (hex), or `None` on an unproven community (no attestation / fails to
/// verify). The owner is DERIVED by verifying the attestation, never a bare claim.
fn proven_owner_hex(community: &Community) -> Option<String> {
    let cid = community.id.to_hex();
    community
        .owner_attestation
        .as_ref()
        .and_then(|a| super::owner::verify_owner_attestation(a, &cid))
        .map(|pk| pk.to_hex())
}

/// Rekey-plane authority with §6 banlist precedence: a positive authority
/// lookup can never honor a banned identity. The banlist and the grant-revoke
/// are SEPARATE editions a withholding relay can split — without this, a
/// since-banned admin whose revoke is withheld still ranks for rotations,
/// letting them race their own removal with a re-founding. Read failure
/// degrades to "not banned" (the roster gate still fails closed on its own
/// read failure); the owner is exempt (supreme, never a valid ban target).
fn rotator_is_authorized(
    cid: &str,
    roster: &super::roles::CommunityRoles,
    owner_hex: Option<&str>,
    rotator_hex: &str,
    permission: u64,
) -> bool {
    if owner_hex != Some(rotator_hex)
        && crate::db::community::get_community_banlist(cid)
            .unwrap_or_default()
            .iter()
            .any(|b| b == rotator_hex)
    {
        return false;
    }
    roster.is_authorized(rotator_hex, owner_hex, permission)
}

/// escalation defense for an authoring action — may the local caller grant/revoke `role_id` on
/// `member_hex`? The caller must strictly outrank BOTH the role being changed AND the target member
/// (so they can't grant a role at/above their own rank, nor touch a superior member). The owner is
/// supreme. Returns a frontend-displayable error if refused. Peers re-run the same predicate on
/// receipt (Phase 2) — this is the local half of the same rule.
fn caller_can_manage_role(
    community: &Community,
    roster: &super::roles::CommunityRoles,
    role_id: &str,
    member_hex: &str,
) -> Result<(), String> {
    let me = crate::state::my_public_key().ok_or("no active identity")?.to_hex();
    let owner = proven_owner_hex(community);
    let owner_ref = owner.as_deref();
    let role = roster.role(role_id).ok_or("no such role")?;
    if !roster.can_manage_position(&me, owner_ref, role.position) {
        return Err("you can only manage roles below your own".to_string());
    }
    if !roster.can_manage_member(&me, owner_ref, member_hex) {
        return Err("you can't manage a member who outranks you".to_string());
    }
    Ok(())
}

/// Grant `member` a role (requires the `MANAGE_ROLES` permission). Publishes the per-member Grant
/// event. The member already holds read keys from membership; the roster entry adds write authority,
/// exercised by signing their own control actions, which peers verify against the roster.
pub async fn grant_role<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    member: nostr_sdk::prelude::PublicKey,
    role_id: &str,
) -> Result<(), String> {
    let cid = community.id.to_hex();
    let member_hex = member.to_hex();
    let roster = crate::db::community::get_community_roles(&cid)?;
    caller_can_manage_role(community, &roster, role_id, &member_hex)?;
    // The member's new full role set = existing + this role (deduped).
    let mut role_ids: Vec<String> = roster
        .grants
        .iter()
        .find(|g| g.member == member_hex)
        .map(|g| g.role_ids.clone())
        .unwrap_or_default();
    if !role_ids.iter().any(|r| r == role_id) {
        role_ids.push(role_id.to_string());
    }

    // Keyless model: granting a role delivers NO secret. Authority is the grantee's npub being in
    // the roster at that rank — they exercise it by signing their own actions, which peers verify
    // against the roster.
    set_member_grant(transport, community, &member_hex, role_ids).await
}

/// Revoke a role from `member` (owner/admin authority) — instant *logical* (the role record is
/// dropped, so the grant-set check stops honoring their actions). The *physical* lockout
/// (channel rekey per) is a later step; this only edits the grant. In the MVP a role is permission
/// bits, NOT a channel read key (channels aren't role-gated), so a revoke needs NO rekey and a bunker
/// account can do it freely. WHEN role-gated channels ship, the rekey-on-revoke path must adopt the same
/// bunker fail-fast guard as `publish_banlist`/`revoke_public_invite` (a rekey needs a raw local key).
pub async fn revoke_role<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    member: nostr_sdk::prelude::PublicKey,
    role_id: &str,
) -> Result<(), String> {
    let cid = community.id.to_hex();
    let member_hex = member.to_hex();
    let roster = crate::db::community::get_community_roles(&cid)?;
    caller_can_manage_role(community, &roster, role_id, &member_hex)?;
    let role_ids: Vec<String> = roster
        .grants
        .iter()
        .find(|g| g.member == member_hex)
        .map(|g| g.role_ids.iter().filter(|r| r.as_str() != role_id).cloned().collect())
        .unwrap_or_default();
    set_member_grant(transport, community, &member_hex, role_ids).await
}

/// Fetch the Community's role graph (real-npub control editions, kind 3308) and fold it into the
/// local roster. Fetches by the **server-root pseudonym** (not by author — the outer is
/// ephemeral), opens each edition under the server-root key, and folds: verify authorship, bind
/// entity↔content, version-fold, quarantine gaps. Advances each entity's monotonic head (the
/// per-entity refuse-downgrade floor) and refreshes the roster cache. Returns the folded roster.
pub async fn fetch_and_apply_roles<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
) -> Result<super::roles::CommunityRoles, String> {
    fetch_and_apply_roles_inner(transport, community, None).await
}

async fn fetch_and_apply_roles_inner<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    prefolded: Option<super::roster::FoldedRoster>,
) -> Result<super::roles::CommunityRoles, String> {
    let session = SessionGuard::capture();
    let cid = community.id.to_hex();
    let folded = match prefolded {
        Some(f) => f,
        None => fetch_control_folded(transport, community).await?,
    };

    if !session.is_valid() {
        return Err("account changed during roles fetch".to_string());
    }
    // NOTE: `folded.gapped_entities` is not consumed yet — the fold is fail-closed by construction
    // (gapped heads are never folded into `folded.roles`), so it's safe in the single-writer MVP. Once
    // multi-writer + rotation ship, this must suspend any cached entry whose entity is now gapped.
    // Advance each entity's head MONOTONICALLY — the per-entity rollback defense (a withholding relay
    // serving only old editions can't lower a head; our own publish's echo is a no-op). The roster
    // CACHE is a derived view refreshed from the fold; a withholding relay can transiently shrink it,
    // but it self-heals on the next quorum fetch and the send side reads the (monotonic) heads, not
    // the cache. (`roles_at` is vestigial under the per-entity model — the heads are the floor now.)
    for head in &folded.heads {
        crate::db::community::set_edition_head(&cid, &head.entity_hex, head.version, &head.self_hash)?;
    }
    // Don't let an empty/withheld fetch wipe a populated roster cache: only refresh it when the fold
    // actually produced editions. The heads above already advanced monotonically (the real floor);
    // the cache is a derived view, so on an empty fold we return what we still hold. (Full per-entity
    // merge so a PARTIAL fetch can't shrink the cache either is the quorum/completeness work, G1.)
    if folded.heads.is_empty() {
        return crate::db::community::get_community_roles(&cid);
    }
    // Authorize: keep only entries whose SIGNER was allowed (delegation chain to the owner).
    // A validly-signed+bound-but-unauthorized edition (e.g. a self-signed Admin grant) is dropped here,
    // never cached as authority. Owner resolved from the (verified) attestation; unproven → empty.
    let authorized = super::roster::authorize_delegation(&folded, proven_owner_hex(community).as_deref());
    crate::db::community::set_community_roles(&cid, &authorized, 0)?;
    Ok(authorized)
}

/// Moderation-hide: publish a 3305 delete for another member's message, signed by the actor's
/// REAL npub (keyless). Authority is the inner signature, re-verified
/// by every member against the owner-rooted roster (MANAGE_MESSAGES + a strict outrank of the
/// target's author). Permanent (the tombstone can't be un-published).
pub async fn publish_owner_hide<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    channel: &Channel,
    target_message_id: &str,
) -> Result<(), String> {
    // hierarchy gate (keyless): I must hold MANAGE_MESSAGES and strictly outrank the target
    // message's author — the owner, outranked by no one, can never be hidden. Resolve the author from
    // local state (you can only moderate a message you can see). A granted
    // MANAGE_MESSAGES member can moderate. Peers RE-verify this against my real-npub inner sig + roster.
    let signer = active_signer().await?;
    let me_pk = crate::state::my_public_key().ok_or("no local identity to sign the hide")?;
    let me = me_pk.to_hex();
    {
        let target_author = {
            let st = crate::state::STATE.lock().await;
            st.find_message(target_message_id).and_then(|(_, m)| m.npub)
        };
        let author = target_author
            .ok_or("can't resolve the target message's author to authorize the hide")?;
        let owner = proven_owner_hex(community);
        let roster = crate::db::community::get_community_roles(&community.id.to_hex()).unwrap_or_default();
        if !roster.can_act_on_member(&me, owner.as_deref(), &author, super::roles::Permissions::MANAGE_MESSAGES) {
            return Err("you can't hide a message from a member who outranks you (or the owner)".to_string());
        }
    }
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    // Keyless moderation-hide: a 3305 delete signed by MY REAL npub. The inner signature IS the
    // authority proof — every member re-verifies it against
    // the roster, so authority is member-visible + non-repudiable, not anonymized.
    // pinned authority: a non-owner hider cites the grant that authorizes them, carried as a `vac`
    // tag on the inner so peers resolve the hide against that grant version (the owner cites nothing).
    let citation = authority_citation(community, &me);
    let extra: Vec<Tag> = citation.iter().map(|c| c.to_tag()).collect();
    let inner = super::envelope::build_inner_full(
        me_pk, &channel.id, channel.epoch,
        event_kind::COMMUNITY_DELETE, "", ms, Some(target_message_id), &[], &extra,
    )
    .sign(&signer)
    .await
    .map_err(|e| format!("sign hide: {e}"))?;
    let _ = publish_signed_message(transport, community, channel, &inner, true).await?;
    Ok(())
}

/// Delete a message the local user previously sent, by its INNER message id (what the UI
/// holds). Loads the retained ephemeral key + the outer event id it points at, then
/// NIP-09-deletes that outer event. Errors if no key is retained (not ours, or already
/// deleted).
pub async fn delete_message<T: Transport + ?Sized>(
    transport: &T,
    message_id: &str,
) -> Result<(), String> {
    let session = SessionGuard::capture();
    if !session.is_valid() {
        return Err("account changed; aborting delete".to_string());
    }
    // PEEK the key (don't consume it yet): the NIP-09 publish below is fallible, and the
    // key is single-use — consuming it before a failed publish would leave the message
    // permanently undeletable. Remove it only after the deletion actually goes out.
    let (ephemeral, outer_event_id_hex, relays) = match crate::db::community::get_message_key(message_id)? {
        Some(v) => v,
        None => {
            return Err("no retained key for this message (not yours, or already deleted)".to_string())
        }
    };
    let id = EventId::from_hex(&outer_event_id_hex).map_err(|e| e.to_string())?;
    delete_own_message(transport, &relays, &ephemeral, id).await?;
    // Published — now it's safe to consume the key.
    crate::db::community::delete_message_key(message_id)?;
    Ok(())
}

/// Accept a parked invite and persist the member-view Community (the user-consented
/// half of the carrier — the inbound handler only *parks* invites; this is reached
/// from an explicit accept command). Guards against id-collision overwrites:
///
/// - if we already OWN a Community with this id, refuse (a member-view save would clobber
///   our owner state);
/// - if we already hold it as a member under a DIFFERENT server root, refuse —
/// `community_id` is unauthenticated random bytes, so a hostile bundle reusing
///   a known id must not be able to swap out our channel keys / authority / relays.
///
/// `SessionGuard`-gated: the accept may straddle a relay-fetch in the caller, and the
/// save must land in the account that consented.
pub fn accept_invite(invite: &CommunityInvite) -> Result<Community, String> {
    let session = SessionGuard::capture();
    let community = super::invite::accept_invite(invite)?; // validates caps + decodes keys

    if let Some(existing) = crate::db::community::load_community(&community.id)? {
        if is_proven_owner(&existing) {
            return Err("you already own this Community".to_string());
        }
        // A known community id arriving with a DIFFERENT base key is a different community wearing the
        // same id (collision / hijack) — reject rather than overwrite. The server-root key is the
        // community's core secret, so it's the keyless authority anchor.
        if existing.server_root_key.as_bytes() != community.server_root_key.as_bytes() {
            return Err(
                "invite reuses a known Community id under a different authority — rejected"
                    .to_string(),
            );
        }
    }

    if !session.is_valid() {
        return Err("account changed during invite accept".to_string());
    }
    crate::db::community::save_community(&community)?;
    Ok(community)
}

/// Warm a community's primary-channel first page into the RAM preload cache BEFORE the user joins,
/// so accepting opens a populated chat instead of paying the join sync. RAM-only and side-effect-
/// free: builds the member view from the bundle WITHOUT persisting (nothing is stored for a
/// community the user may decline), fetches one page, and stashes it keyed by community id (the
/// fetch also warms the relay connection). Best-effort — any failure just leaves Join to sync
/// normally. Spawn this behind a `SessionGuard`; promotion on Join re-validates freshness.
pub async fn preload_community(invite: &super::invite::CommunityInvite) {
    let Ok(community) = super::invite::accept_invite(invite) else { return };
    let Some(channel) = community.channels.first() else { return };
    let cid = community.id.to_hex();
    // Mark in-flight FIRST so a Join that races the fetch adopts it instead of double-fetching.
    crate::community::cache::begin_preload(&cid);
    let transport = super::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(12));
    // Newest page, no `since` (first warm). 50 mirrors the GUI page limit.
    match super::send::fetch_channel_page(&transport, &community, channel, None, None, 50).await {
        Ok(page) if !page.is_empty() => crate::community::cache::finish_preload(&cid, page),
        // Empty page or fetch error → drop the in-flight marker so an adopter falls back at once.
        _ => crate::community::cache::abort_preload(&cid),
    }
}

/// Persist edited Community display metadata and republish the GroupRoot as a real-npub 3308 edition
/// (vsk=0) so other members + re-anchoring pick it up. Keyless authority: the actor must hold
/// `MANAGE_METADATA` (the owner holds every permission). The caller mutates `community` (name /
/// description / icon / banner) first; this gates, saves it, then publishes the next edition version.
pub async fn republish_community_metadata<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
) -> Result<(), String> {
    let session = SessionGuard::capture();
    let cid = community.id.to_hex();
    let signer = active_signer().await?;
    let actor_pk = crate::state::my_public_key().ok_or("no local identity to sign the metadata edition")?;
    let owner = proven_owner_hex(community);
    let roster = crate::db::community::get_community_roles(&cid).unwrap_or_default();
    if !roster.is_authorized(&actor_pk.to_hex(), owner.as_deref(), super::roles::Permissions::MANAGE_METADATA) {
        return Err("only a member with manage-metadata authority can edit the community".to_string());
    }
    // Publish-FIRST, then persist content + head on success (now that `fetch_and_apply_metadata` is a
    // live consumer, metadata is relay-authoritative: a failed publish must not leave us showing an edit
    // no member can see, and advancing the head before a fallible publish would phantom-head it — the
    // successor cites an unpublished predecessor → the fold quarantines the chain forever).
    let (version, prev_hash) = match crate::db::community::get_edition_head(&cid, &cid)? {
        Some((v, h)) => (v + 1, Some(h)),
        None => (1, None),
    };
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let meta = super::metadata::CommunityMetadata::of(community);
    // authority citation — the actor's "role badge" (the grant they act under), emitted by EVERY other
    // control producer. Owner cites nothing (supreme). The metadata consumer doesn't version-pin on it (a
    // metadata edit is cosmetic + self-healing, unlike an access-cutting ban), but emitting it keeps the
    // immutable wire data complete rather than baking in a gap.
    let citation = authority_citation(community, &actor_pk.to_hex());
    let unsigned = super::roster::build_community_root_edition_unsigned(actor_pk, &community.id, &meta, version, prev_hash.as_ref(), created, citation.as_ref())?;
    let inner = unsigned.sign(&signer).await.map_err(|e| format!("sign community-root edition: {e}"))?;
    let outer = super::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, community.server_root_epoch)?;
    transport.publish_durable(&outer, &community.relays).await?;
    if session.is_valid() {
        crate::db::community::save_community(community)?;
        let h = super::version::edition_hash(&community.id.0, version, prev_hash.as_ref(), inner.content.as_bytes());
        // Record OUR own edition's inner_id so a peer's same-version fork can't displace it unless that
        // peer genuinely wins the deterministic tiebreak (lower inner id), per converge_edition_head.
        crate::db::community::set_edition_head_with_id(&cid, &cid, version, &h, &inner.id.to_bytes())?;
    }
    Ok(())
}

/// Rename a channel and republish its ChannelMetadata as a real-npub 3308 edition (vsk=2) so
/// members fold it via [`fetch_and_apply_metadata`]. Keyless authority: the actor must hold
/// `MANAGE_CHANNELS` (channel edits are a channel-management action; the owner holds every permission).
/// `channel_id` must be one of `community`'s channels. Publish-FIRST then persist on success (relay-
/// authoritative, phantom-head-safe — same contract as the community GroupRoot).
pub async fn republish_channel_metadata<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    channel_id: &crate::community::ChannelId,
    new_name: &str,
) -> Result<(), String> {
    let session = SessionGuard::capture();
    let cid = community.id.to_hex();
    let ch_hex = channel_id.to_hex();
    if !community.channels.iter().any(|c| &c.id == channel_id) {
        return Err("no such channel in this community".to_string());
    }
    let signer = active_signer().await?;
    let actor_pk = crate::state::my_public_key().ok_or("no local identity to sign the channel metadata edition")?;
    let owner = proven_owner_hex(community);
    let roster = crate::db::community::get_community_roles(&cid).unwrap_or_default();
    if !roster.is_authorized(&actor_pk.to_hex(), owner.as_deref(), super::roles::Permissions::MANAGE_CHANNELS) {
        return Err("only a member with manage-channels authority can rename a channel".to_string());
    }
    let (version, prev_hash) = match crate::db::community::get_edition_head(&cid, &ch_hex)? {
        Some((v, h)) => (v + 1, Some(h)),
        None => (1, None),
    };
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let meta = super::metadata::ChannelMetadata { name: new_name.to_string() };
    // authority citation — same "role badge" the community-root + grant/ban producers emit (owner cites
    // nothing). Consumer doesn't version-pin metadata, but the wire data stays complete.
    let citation = authority_citation(community, &actor_pk.to_hex());
    let unsigned = super::roster::build_channel_metadata_edition_unsigned(actor_pk, channel_id, &meta, version, prev_hash.as_ref(), created, citation.as_ref())?;
    let inner = unsigned.sign(&signer).await.map_err(|e| format!("sign channel-metadata edition: {e}"))?;
    let outer = super::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, community.server_root_epoch)?;
    transport.publish_durable(&outer, &community.relays).await?;
    if session.is_valid() {
        let mut current = crate::db::community::load_community(&community.id)?.ok_or("community no longer present")?;
        if let Some(ch) = current.channels.iter_mut().find(|c| &c.id == channel_id) {
            ch.name = new_name.to_string();
        }
        crate::db::community::save_community(&current)?;
        let h = super::version::edition_hash(&channel_id.0, version, prev_hash.as_ref(), inner.content.as_bytes());
        crate::db::community::set_edition_head_with_id(&cid, &ch_hex, version, &h, &inner.id.to_bytes())?;
    }
    Ok(())
}

// ============================================================================
// Public (link) invites
// ============================================================================

/// Mint a public invite link for a Community the local user owns: snapshot its preview,
/// build + publish the token-encrypted bundle to the Community relays, retain the token
/// locally (for list/revoke), and return `(hex token, shareable URL)`.
///
/// Owner-only: the bundle grants the @everyone base (server-root) key, and minting the
/// canonical link is an owner action. `SessionGuard`-gated around the token persist.
/// A short, human-typable label for an unlabeled invite link. Crockford-ish base32 (no 0/1/I/O)
/// so it's unambiguous to read and share aloud; 6 chars ≈ 1B combinations (collision-improbable).
fn generate_invite_label() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    (0..6).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect()
}

pub async fn create_public_invite<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    expires_at: Option<u64>,
    label: Option<String>,
) -> Result<(String, String), String> {
    if !caller_has_permission(community, super::roles::Permissions::CREATE_INVITE) {
        return Err("you need the create-invite permission to mint a public invite".to_string());
    }
    let session = SessionGuard::capture();

    // Every link gets a label: use the one provided, else mint a random 6-char handle. A stable label
    // makes the link identifiable in the UI and keys per-link join attribution off (creator, label),
    // so it must be unique among THIS creator's links (else two links share a join bucket).
    let existing = crate::db::community::list_public_invites(&community.id.to_hex()).unwrap_or_default();
    let label_taken = |cand: &str| {
        existing.iter().any(|r| r.label.as_deref().map(|e| e.eq_ignore_ascii_case(cand)).unwrap_or(false))
    };
    let label = match label {
        Some(l) if !l.trim().is_empty() => {
            let l = l.trim().to_string();
            if label_taken(&l) {
                return Err(format!("You already have an invite link labeled \u{201c}{l}\u{201d}. Pick a different label."));
            }
            Some(l)
        }
        // Random handle — regenerate on the (astronomically unlikely) collision.
        _ => {
            let mut l = generate_invite_label();
            while label_taken(&l) {
                l = generate_invite_label();
            }
            Some(l)
        }
    };

    // Attribution (metrics): stamp the bundle with who minted it (my npub) + the creator's label, so
    // a joiner's Presence can announce "invited by me via <label>".
    let creator_npub = crate::state::my_public_key().and_then(|pk| pk.to_bech32().ok());
    let token = public_invite::new_token();
    let event = build_public_invite_event(community, &token, expires_at, creator_npub, label.clone()).map_err(|e| e.to_string())?;
    transport.publish_durable(&event, &community.relays).await?;

    // Published — retain the token so the owner can list + revoke. Bail if the account
    // swapped across the publish await.
    if !session.is_valid() {
        return Err("account changed during public invite creation".to_string());
    }
    let token_hex = crate::simd::hex::bytes_to_hex_32(&token);
    let url = public_invite::encode_invite_url(&community.relays, &token);
    crate::db::community::save_public_invite(
        &token_hex,
        &community.id.to_hex(),
        &url,
        expires_at.map(|e| e as i64),
        label.as_deref(),
    )?;
    // Publish MY updated invite-link set so every member's computed mode flips to Public — the link
    // now exists in the signed, foldable per-creator source of truth, not just my local token store.
    republish_my_invite_links(transport, community).await?;
    Ok((token_hex, url))
}

/// Read-only freshen for an invite preview: build the bundle's ephemeral community, fold the live
/// control plane, and return the LATEST authorized display metadata — never the bundle's mint-time
/// snapshot (which goes stale the moment metadata is edited; mirrors the website preview). No DB
/// floors and no persistence: the previewer isn't a member, so there is no local state to anchor.
/// Any failure falls back to the snapshot so a flaky relay can't blank the preview.
pub async fn latest_invite_preview<T: Transport + ?Sized>(
    transport: &T,
    bundle: &public_invite::PublicInviteBundle,
) -> public_invite::PublicInvitePreview {
    let snapshot = bundle.preview.clone();
    let Ok(community) = super::invite::accept_invite(&bundle.join) else {
        return snapshot;
    };
    let Ok(folded) = fetch_control_folded(transport, &community).await else {
        return snapshot;
    };
    let owner = proven_owner_hex(&community);
    let authorized = super::roster::authorize_delegation(&folded, owner.as_deref());
    match folded.root_candidates.iter().find(|c| {
        authorized.is_authorized(&c.author.to_hex(), owner.as_deref(), super::roles::Permissions::MANAGE_METADATA)
    }) {
        Some(c) => public_invite::PublicInvitePreview {
            name: c.meta.name.clone(),
            description: c.meta.description.clone(),
            icon: c.meta.icon.clone(),
        },
        None => snapshot,
    }
}

/// Fetch + decrypt the bundle for a public-invite token from the given bootstrap relays.
/// Queries the addressable coordinate (`d` = token locator, author = token signer) and
/// verifies the signer, so an impostor squatting the locator is rejected.
pub async fn fetch_public_invite<T: Transport + ?Sized>(
    transport: &T,
    relays: &[String],
    token: &[u8; 32],
) -> Result<PublicInviteBundle, String> {
    // Query by coordinate (kind + locator d-tag) only — do NOT rely on the relay to
    // honor an authors filter. A hostile relay can pile junk events at the same locator
    // (signed by other keys, possibly with a newer created_at to shadow the real one).
    let query = Query {
        kinds: vec![event_kind::APPLICATION_SPECIFIC],
        d_tags: vec![locator_hex(token)],
        ..Default::default()
    };
    let events = transport.fetch(&query, relays).await?;
    // Resolve by the NEWEST token-signed event at the coordinate (replaceable-event semantics), skipping any
    // impostor/junk (parse enforces author == token signer). A revocation tombstone is unforgeable, so a
    // `Revoked` verdict on ANY relay is authoritative — and it WINS ties with a bundle (fail-safe: a
    // deliberate revoke beats a same-second bundle), defeating the mixed-relay race where one relay kept the
    // stale live bundle. A genuinely re-created link (a bundle STRICTLY newer than the tombstone) still wins.
    let (mut bundle_at, mut bundle, mut revoked_at) = (0u64, None, None::<u64>);
    for ev in &events {
        match parse_public_invite_event(ev, token) {
            Ok(b) => if bundle.is_none() || ev.created_at.as_secs() > bundle_at {
                bundle_at = ev.created_at.as_secs();
                bundle = Some(b);
            },
            Err(super::public_invite::PublicInviteError::Revoked) => {
                let at = ev.created_at.as_secs();
                if revoked_at.map_or(true, |r| at > r) { revoked_at = Some(at); }
            }
            Err(_) => {} // impostor / junk / undecryptable — ignore
        }
    }
    match (bundle, revoked_at) {
        (Some(b), Some(r)) if bundle_at > r => Ok(b), // a re-created bundle strictly newer than the tombstone
        (_, Some(_)) => Err("this invite was revoked".to_string()),
        (Some(b), None) => Ok(b),
        (None, None) => Err("no public invite found at that link (revoked, never posted, or shadowed)".to_string()),
    }
}

/// Accept a fetched public-invite bundle: reject if expired, join via the guarded
/// member-save (caps + id-collision checks), then patch in the preview's display
/// metadata (description/icon) so the new member sees them immediately.
pub fn accept_public_invite(bundle: &PublicInviteBundle, now_secs: u64) -> Result<Community, String> {
    if bundle.is_expired(now_secs) {
        return Err("this invite link has expired".to_string());
    }
    let mut community = accept_invite(&bundle.join)?;
    // accept_invite leaves display metadata None; the public bundle carries a preview,
    // so populate it (and re-save) for an immediately-rich member view.
    if bundle.preview.description.is_some() || bundle.preview.icon.is_some() {
        community.description = bundle.preview.description.clone();
        community.icon = bundle.preview.icon.clone();
        crate::db::community::save_community(&community)?;
    }
    Ok(community)
}

/// Revoke a public invite: NIP-09-delete the bundle event (by its addressable coordinate, signed by the
/// token-derived key we re-derive from the retained token), forget the token locally, and republish the
/// invite-link registry so the mode tracks reality. **If this was the LAST link, the community goes
/// Private → it is re-founded (privatize): the base key is rotated to the observed-participants set,
/// sealing out link-joined lurkers who never spoke.** Creator-only: you can only retire YOUR OWN
/// links (the token is held only by its creator); the privatize rekey is `BAN`-gated + needs a local key.
pub async fn revoke_public_invite<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    token: &[u8; 32],
) -> Result<(), String> {
    let session = SessionGuard::capture();
    let cid = community.id.to_hex();
    let token_hex = crate::simd::hex::bytes_to_hex_32(token);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // Idempotent no-op if we don't hold the token: either it's already retired (re-revoke) or it's not
    // ours — creator-only, the token is held only by its creator. Nothing to do, never a double-rotate.
    if !crate::db::community::list_public_invites(&cid)?.iter().any(|r| r.token == token_hex) {
        return Ok(());
    }
    let my_locators_before: Vec<String> = crate::db::community::list_public_invites(&cid)?
        .iter()
        .filter(|r| r.expires_at.map_or(true, |e| (e as u64) > now))
        .map(|r| public_invite::locator_hex(&crate::simd::hex::hex_to_bytes_32(&r.token)))
        .collect();
    // B1 fix: refresh the aggregate from relays FIRST, so the privatize decision sees OTHER creators'
    // live links (a stale/scroll-back-only cache would wrongly read empty and rekey a still-Public
    // community out from under another creator). Best-effort; on failure we fall back to the cache.
    let _ = fetch_and_apply_invite_links(transport, community).await;
    if !session.is_valid() {
        return Err("account changed during invite revoke".to_string());
    }
    // Will retiring this link empty the AGGREGATE (this creator's remaining ∪ every other creator's)?
    // Others' locators = the freshly-folded aggregate minus mine (locators are per-token-unique). Only
    // then does it privatize → re-found rekey. Fail-fast (bunker): the rekey needs a RAW local key
    // (the blob locator is an ECDH a NIP-46 bunker can't expose) — refuse BEFORE publishing so we never
    // half-apply (flip to Private over a live base key). A community admin with a local key privatizes.
    let this_locator = public_invite::locator_hex(token);
    let cached_aggregate: std::collections::BTreeSet<String> =
        crate::db::community::get_community_invite_registry(&cid)?.into_iter().collect();
    let my_before: std::collections::BTreeSet<String> = my_locators_before.iter().cloned().collect();
    let others: std::collections::BTreeSet<String> = cached_aggregate.difference(&my_before).cloned().collect();
    let my_after: std::collections::BTreeSet<String> =
        my_before.iter().filter(|l| **l != this_locator).cloned().collect();
    let would_empty_aggregate = others.is_empty() && my_after.is_empty();
    if would_empty_aggregate && crate::state::MY_SECRET_KEY.to_keys().is_none() {
        return Err("Revoking this last invite link makes the community private, which re-keys it so link-joined lurkers lose access. Your account signs remotely (a NIP-46 bunker) and can't perform that rotation. Ask a community admin who holds a local key to privatize the community.".to_string());
    }
    // Revoke the bundle by OVERWRITING it with an empty, token-signed revocation tombstone (vsk=9) at its
    // coordinate. The bundle is a replaceable event (kind 30078), and relays honor replaceable-event
    // REPLACEMENT near-universally — far more reliably than NIP-09 `a`-tag (coordinate) deletions, which
    // many relays silently ignore (live-confirmed: 2 of 3 relays kept the bundle after a coordinate delete,
    // but all 3 replaced it with the tombstone). So the tombstone alone reliably kills the live bundle on
    // every relay AND leaves an explicit marker the preview page reads as "revoked". A NIP-09 delete is not
    // just redundant but counterproductive: on a relay that honors it, a same-second delete can drop the
    // tombstone too, leaving the coordinate empty and losing the revoked marker. (Not the access cut — the
    // rekey below is.) Best-effort so a publish hiccup can't block the rekey; publish_durable retries.
    if let Ok(tombstone) = public_invite::build_public_invite_tombstone(token) {
        let _ = transport.publish_durable(&tombstone, &community.relays).await;
    }
    // Re-check the session straddling the publish await before any per-account DB write (B2).
    if !session.is_valid() {
        return Err("account changed during invite revoke".to_string());
    }
    crate::db::community::delete_public_invite(&token_hex)?;
    // Republish MY (reduced) link set so the mode reflects the removal, then set the recomputed aggregate.
    republish_my_invite_links(transport, community).await?;
    if session.is_valid() {
        let aggregate_after: Vec<String> = others.union(&my_after).cloned().collect();
        crate::db::community::set_community_invite_registry(&cid, &aggregate_after)?;
    }
    if would_empty_aggregate {
        // Aggregate empty → a genuine Public→Private transition → re-found (re-seal base to observed).
        // Durable (read_cut_pending): a failed privatize re-seal is resumed on the next ban or sync, like a
        // ban read-cut — not silently dropped, which would leave it half-private.
        run_read_cut(transport, community, true).await?;
    }
    Ok(())
}

/// owner dissolution ("Delete Community") — publish the terminal GroupDissolved tombstone, then seal
/// locally. The owner's ONLY honest exit (a bare leave would orphan the chain root). Order (defense in
/// depth): (a) authority — the caller MUST be the proven owner (a BAN admin is NOT enough — ending the
/// community for everyone is the owner's call alone); (b) publish the tombstone at `dissolved_locator`
/// FIRST and require it to LAND (must-succeed durable publish — a failed tombstone after a link-retire is a
/// stuck half-state); (c) THEN best-effort retire all of the owner's OWN public invite-link editions on a
/// path that emits NO 3303 rekey and NO epoch bump (dissolution rotates nothing — there is no future
/// content to protect); (d) set the local seal. Irreversible.
/// Probe the ROTATION-STABLE dissolved coordinate for a tombstone signed by `owner_hex`. The
/// cross-epoch discovery path: it fetches `dissolved_pseudonym` (community-id-derived, epoch-free) and
/// opens under the community-id envelope key, so a client holding ANY epoch root finds it. Best-effort
/// (a relay miss ⇒ false; the next sync re-probes). The caller has already derived + verified the owner.
async fn dissolved_tombstone_present<T: Transport + ?Sized>(transport: &T, community: &Community, owner_hex: &str) -> bool {
    let z = super::derive::dissolved_pseudonym(&community.id);
    let q = Query { kinds: vec![event_kind::COMMUNITY_CONTROL], z_tags: vec![z], ..Default::default() };
    for ev in transport.fetch(&q, &community.relays).await.unwrap_or_default() {
        if super::roster::dissolved_tombstone_signer(&ev, &community.id).map(|s| s.to_hex()) == Some(owner_hex.to_string()) {
            return true;
        }
    }
    false
}

pub async fn dissolve_community<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
) -> Result<(), String> {
    let session = SessionGuard::capture();
    let cid = community.id.to_hex();

    // (a) Authority: owner-only, derived from the deed (never a cached claim). Stricter than re-founding.
    if !is_proven_owner(community) {
        return Err("only the community owner can dissolve (delete) the community".to_string());
    }
    let signer = active_signer().await?;
    let actor_pk = crate::state::my_public_key().ok_or("no local identity to sign the dissolution")?;

    // (b) Tombstone FIRST, must-succeed. The marker is the whole mechanism; build it chain-free (vsk=10,
    // fixed v1, no prev-hash) and seal under the CURRENT server root for the wire (re-anchoring keeps the
    // plane reachable there). A durable publish that fails returns Err so we never half-apply.
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let unsigned = super::roster::build_group_dissolved_edition_unsigned(actor_pk, &community.id, created_at);
    let inner = unsigned.sign(&signer).await.map_err(|e| format!("sign dissolution tombstone: {e}"))?;
    // Publish at the ROTATION-STABLE coordinate — the load-bearing path: a community-id-keyed
    // envelope at `dissolved_pseudonym`, found + openable by any client at any epoch, so a concurrent
    // re-founding can't strand the tombstone at an old epoch and let post-rotation joiners see a live group.
    let stable = super::roster::seal_dissolved_edition(&Keys::generate(), &inner, &community.id)?;
    transport.publish_durable(&stable, &community.relays).await?;
    // Also publish at the current `control_pseudonym` (a current-epoch fast path so members fold it in their
    // normal control fetch without the extra probe). Best-effort — the stable publish above is the guarantee.
    if let Ok(outer) = super::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, community.server_root_epoch) {
        let _ = transport.publish_durable(&outer, &community.relays).await;
    }
    if !session.is_valid() {
        return Err("account changed during dissolution".to_string());
    }

    // (c) Best-effort retire the owner's OWN public invite-link editions WITHOUT the privatize re-founding
    // path: publish an empty per-creator link set (NO 3303 rekey, NO epoch bump — that rekey lives only in
    // `revoke_public_invite`) and tombstone+delete each owned token. A failure here is harmless (the
    // tombstone above already ends the community + an honest joiner refuses the stable-locator-dissolved
    // group). Skipped if we lack CREATE_INVITE (no links to retire).
    if caller_has_permission(community, super::roles::Permissions::CREATE_INVITE) {
        let _ = publish_my_invite_links(transport, community, &[]).await;
        if let Ok(records) = crate::db::community::list_public_invites(&cid) {
            for r in records {
                let token = crate::simd::hex::hex_to_bytes_32(&r.token);
                if let Ok(tombstone) = public_invite::build_public_invite_tombstone(&token) {
                    let _ = transport.publish_durable(&tombstone, &community.relays).await;
                }
                let _ = crate::db::community::delete_public_invite(&r.token);
            }
        }
    }

    // (d) Seal locally — permanent. Re-check the session straddling the awaits before the per-account write.
    if !session.is_valid() {
        return Err("account changed during dissolution".to_string());
    }
    crate::db::community::set_community_dissolved(&cid)?;
    Ok(())
}

/// Publish the LOCAL user's OWN invite-link set as a `CREATE_INVITE`-gated vsk=8 control edition at
/// their per-creator coordinate — one of the per-creator lists members fold into the aggregate active-set.
/// `my_locators` is the FULL new set of THIS creator's active link locators (hex; the token in the URL is
/// the secret, never listed). Publish FIRST, then advance the head + merge into the cached aggregate on
/// success (relay-authoritative + phantom-head rule). A creator manages only their own list — no
/// `MANAGE_INVITES`. Carries the actor's `vac` citation so a non-owner creator's authority is verifiable.
pub async fn publish_my_invite_links<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    my_locators: &[String],
) -> Result<(), String> {
    let session = SessionGuard::capture();
    if !caller_has_permission(community, super::roles::Permissions::CREATE_INVITE) {
        return Err("you need the create-invite permission to publish invite links".to_string());
    }
    let cid = community.id.to_hex();
    let signer = active_signer().await?;
    let actor_pk = crate::state::my_public_key().ok_or("no local identity to sign the invite links")?;
    let entity_id = super::derive::invite_links_locator(&community.id, &actor_pk.to_bytes());
    let entity_hex = crate::simd::hex::bytes_to_hex_32(&entity_id);
    let (version, prev_hash) = match crate::db::community::get_edition_head(&cid, &entity_hex)? {
        Some((v, h)) => (v + 1, Some(h)),
        None => (1, None),
    };
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // pinned authority: a non-owner creator cites the grant that authorizes them (owner cites nothing).
    let citation = authority_citation(community, &actor_pk.to_hex());
    let unsigned = super::roster::build_invite_links_edition_unsigned(actor_pk, &community.id, my_locators, version, prev_hash.as_ref(), created_at, citation.as_ref())?;
    let inner = unsigned.sign(&signer).await.map_err(|e| format!("sign invite-links edition: {e}"))?;
    let outer = super::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, community.server_root_epoch)?;
    let self_hash = super::version::edition_hash(&entity_id, version, prev_hash.as_ref(), inner.content.as_bytes());
    transport.publish_durable(&outer, &community.relays).await?;
    if session.is_valid() {
        crate::db::community::set_edition_head(&cid, &entity_hex, version, &self_hash)?;
        // Optimistically merge MY locators into the cached aggregate so `is_public` is right immediately;
        // the next `fetch_and_apply_invite_links` recomputes the authoritative union across all creators.
        let mut agg: std::collections::BTreeSet<String> =
            crate::db::community::get_community_invite_registry(&cid)?.into_iter().collect();
        agg.extend(my_locators.iter().cloned());
        crate::db::community::set_community_invite_registry(&cid, &agg.into_iter().collect::<Vec<_>>())?;
        crate::db::community::upsert_invite_link_set(&cid, &actor_pk.to_hex(), my_locators)?;
    }
    Ok(())
}

/// Fetch the control plane and apply the folded invite-link AGGREGATE locally: UNION the locators of
/// every per-creator vsk=8 edition whose `creator` held `CREATE_INVITE` in the AUTHORIZED roster (the
/// keyless gate, same shape as the banlist's BAN check), advancing each authorized creator's head
/// (refuse-downgrade). The union is the source of truth for the Public/Private mode (`is_public`) + the
/// metrics — NOT join-gating (joining is envelope-only). Returns the aggregate set (empty = Private).
pub async fn fetch_and_apply_invite_links<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
) -> Result<Vec<String>, String> {
    fetch_and_apply_invite_links_inner(transport, community, None).await
}

async fn fetch_and_apply_invite_links_inner<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    prefolded: Option<super::roster::FoldedRoster>,
) -> Result<Vec<String>, String> {
    let session = SessionGuard::capture();
    let cid = community.id.to_hex();
    let folded = match prefolded {
        Some(f) => f,
        None => fetch_control_folded(transport, community).await?,
    };
    if !session.is_valid() {
        return Err("account changed during invite-links fetch".to_string());
    }
    let owner = proven_owner_hex(community);
    let authorized = super::roster::authorize_delegation(&folded, owner.as_deref());
    let mut aggregate: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // Per-creator sets (attribution) for the "X has N active invite links" UI.
    let mut per_creator: Vec<crate::db::community::InviteLinkSetRow> = Vec::new();
    for set in &folded.invite_link_sets {
        // authority: only a creator who held CREATE_INVITE counts. A self-minted list from an
        // unpermissioned member is dropped (the inner sig proves authorship, not authority).
        if !authorized.is_authorized(&set.creator.to_hex(), owner.as_deref(), super::roles::Permissions::CREATE_INVITE) {
            continue;
        }
        let held = crate::db::community::get_edition_head(&cid, &set.head.entity_hex)?.map(|(v, _)| v).unwrap_or(0);
        if set.head.version > held {
            crate::db::community::set_edition_head(&cid, &set.head.entity_hex, set.head.version, &set.head.self_hash)?;
        }
        aggregate.extend(set.locators.iter().cloned());
        per_creator.push(crate::db::community::InviteLinkSetRow {
            creator_hex: set.creator.to_hex(),
            locators: set.locators.clone(),
        });
    }
    let aggregate: Vec<String> = aggregate.into_iter().collect();
    crate::db::community::set_community_invite_registry(&cid, &aggregate)?;
    crate::db::community::replace_invite_link_sets(&cid, &per_creator)?;
    Ok(aggregate)
}

/// Fetch the Community's control plane and apply folded METADATA edits locally: the GroupRoot
/// (vsk=0 — community name/description/icon/banner) and each ChannelMetadata (vsk=2 — channel name). An
/// edition applies only if its signer held `MANAGE_METADATA` in the AUTHORIZED roster (the keyless 
/// gate, same as the producer) AND is strictly newer than the head we hold (refuse-downgrade by version).
/// Identity/transport fields (`server_root_key`, `relays`, `owner_attestation`) are NEVER taken from a
/// metadata edit — a manage-metadata admin edits DISPLAY, not the community's identity. Best-effort:
/// returns `Ok` even when nothing applied. This is what makes an owner/admin's edit sync to every member.
pub async fn fetch_and_apply_metadata<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
) -> Result<(), String> {
    fetch_and_apply_metadata_inner(transport, community, None).await
}

async fn fetch_and_apply_metadata_inner<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    prefolded: Option<super::roster::FoldedRoster>,
) -> Result<(), String> {
    let session = SessionGuard::capture();
    let cid = community.id.to_hex();
    let folded = match prefolded {
        Some(f) => f,
        None => fetch_control_folded(transport, community).await?,
    };
    if !session.is_valid() {
        return Err("account changed during metadata fetch".to_string());
    }
    let owner = proven_owner_hex(community);
    let authorized = super::roster::authorize_delegation(&folded, owner.as_deref());
    // Community display = MANAGE_METADATA; channel display = MANAGE_CHANNELS (— channel edits are a
    // channel-management action, matching `build_channel_metadata_edition`'s contract).
    let manage = super::roles::Permissions::MANAGE_METADATA;
    let manage_channels = super::roles::Permissions::MANAGE_CHANNELS;

    // Apply onto the freshest local state (the caller's struct may predate other syncs). `save_community`
    // UPSERTs the community row, so `created_at` (the kick join-anchor) and the banlist are preserved.
    let mut current = match crate::db::community::load_community(&community.id)? {
        Some(c) => c,
        None => return Ok(()),
    };
    let mut dirty = false;
    // (entity_hex, version, self_hash, inner_id, is_converge) of each edition applied — written AFTER a
    // successful save. `is_converge` routes a same-version fork-resolution to converge_edition_head; a
    // strictly-higher version is a plain advance.
    let mut head_updates: Vec<(String, u64, [u8; 32], [u8; 32], bool)> = Vec::new();

    // Decide whether a folded display head should apply, and how. A strictly-higher version ADVANCES the
    // refuse-downgrade floor. An equal version with a DIFFERENT, lower-inner-id edition CONVERGES a
    // concurrent fork: two authorized editors editing from the same base both produce v+1, and every
    // client must adopt the same deterministic winner (lowest inner edition id). Mirrors
    // converge_edition_head's SQL (a NULL/None held id is "always replaceable") so we never apply a
    // display edit the head write would then refuse. `Some(is_converge)` → apply; `None` → keep the floor.
    let decide = |entity_hex: &str, head: &super::roster::EntityHead| -> Result<Option<bool>, String> {
        let held = crate::db::community::get_edition_head(&cid, entity_hex)?;
        let held_v = held.map(|(v, _)| v).unwrap_or(0);
        if head.version > held_v {
            return Ok(Some(false)); // advance
        }
        if head.version == held_v && held.map(|(_, h)| h) != Some(head.self_hash) {
            let held_id = crate::db::community::get_edition_head_inner_id(&cid, entity_hex)?;
            if held_id.is_none() || Some(head.inner_id) < held_id {
                return Ok(Some(true)); // converge to the lower-inner-id authorized winner
            }
        }
        Ok(None)
    };

    // Author-aware descending scan (/ B1b): the candidates are sorted (version desc, inner-id asc), so
    // the first whose author CURRENTLY holds MANAGE_METADATA is both the highest-version AND (within a
    // version) the deterministic tiebreak winner. Skips a demoted author's editions, incl. a same-version
    // forgery. No authorized candidate → keep the floor.
    if let Some(c) = folded.root_candidates.iter()
        .find(|c| authorized.is_authorized(&c.author.to_hex(), owner.as_deref(), manage))
    {
        let head = &c.head;
        if let Some(is_converge) = decide(&head.entity_hex, head)? {
            let meta = &c.meta;
            // Apply only the editable display fields.
            // `meta.owner_attestation` is DELIBERATELY NOT applied: the owner is the deed, anchored from
            // the invite/founding. Letting an editable field redefine it = a one-edit takeover, so
            // ownership is NON-TRANSFERABLE for the MVP. (Transfer — and eventually owner quorums — will
            // be a deliberate owner-signed action, never a metadata side-effect.)
            // `meta.relays` is also dropped for now (silently following an embedded relay list is a
            // herding/partition vector). Relay migration is likewise deferred to a first-class,
            // permissioned, ADDITIVE (union-not-replace) action.
            current.name = meta.name.clone();
            current.description = meta.description.clone();
            current.icon = meta.icon.clone();
            current.banner = meta.banner.clone();
            dirty = true;
            head_updates.push((head.entity_hex.clone(), head.version, head.self_hash, head.inner_id, is_converge));
        }
    }
    // Channels mirror GroupRoot: per channel, an author-aware descending scan over its candidates (sorted
    // version desc, inner-id asc) → the highest whose author CURRENTLY holds MANAGE_CHANNELS, then decide()
    // advance/converge. A concurrent same-version rename converges to the same deterministic winner on every
    // client; a demoted author's edition (incl. a same-version forgery) is skipped. Candidates arrive grouped
    // + sorted per channel, so the first authorized per channel is the winner.
    let mut resolved_channels: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
    for cm in &folded.channel_candidates {
        if resolved_channels.contains(&cm.channel_id) {
            continue; // this channel already resolved (its candidates are contiguous + sorted)
        }
        if !authorized.is_authorized(&cm.author.to_hex(), owner.as_deref(), manage_channels) {
            continue; // skip a demoted author; keep scanning lower candidates for this channel
        }
        resolved_channels.insert(cm.channel_id);
        let Some(is_converge) = decide(&cm.head.entity_hex, &cm.head)? else { continue };
        if let Some(ch) = current.channels.iter_mut().find(|c| c.id.0 == cm.channel_id) {
            ch.name = cm.meta.name.clone();
            dirty = true;
            head_updates.push((cm.head.entity_hex.clone(), cm.head.version, cm.head.self_hash, cm.head.inner_id, is_converge));
        }
    }

    if dirty && session.is_valid() {
        crate::db::community::save_community(&current)?;
        // Persist heads in the SAME save block so a subsequent re-assert/edit chains prev_hash from the
        // converged head, not a stale one (else the fork regenerates at the next version).
        for (entity_hex, version, self_hash, inner_id, is_converge) in &head_updates {
            if *is_converge {
                crate::db::community::converge_edition_head(&cid, entity_hex, *version, self_hash, inner_id)?;
            } else {
                crate::db::community::set_edition_head_with_id(&cid, entity_hex, *version, self_hash, inner_id)?;
            }
        }
    }
    Ok(())
}

/// The computed Public/Private mode: a community is PUBLIC iff the folded per-creator invite-link
/// aggregate has ≥1 active locator, else PRIVATE. Every member computes the same value from the folded
/// editions, which is what lets it drive rekey-on-removal consistently (Private removals rekey the base
/// to the roster; Public ones don't — anti-memberlist). Reads the cached aggregate, which is only as
/// fresh as the last successful latest-page sync ([`fetch_and_apply_invite_links`], wired best-effort
/// into the sync path) — a member who only scrolled back, or whose sync failed, can hold a stale mode
/// (which is why `revoke_public_invite` refreshes the aggregate before deciding to privatize).
pub fn is_public(community: &Community) -> Result<bool, String> {
    Ok(!crate::db::community::get_community_invite_registry(&community.id.to_hex())?.is_empty())
}

/// Recompute the LOCAL user's OWN invite-link set from their currently-retained public-invite tokens and
/// publish it (per-creator), so every member's computed Public/Private mode tracks reality. Returns
/// this creator's new active link-locator set (empty = they hold no links). Expired links are dropped —
/// they can't be joined, so they don't keep a community Public.
async fn republish_my_invite_links<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
) -> Result<Vec<String>, String> {
    let cid = community.id.to_hex();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let locators: Vec<String> = crate::db::community::list_public_invites(&cid)?
        .iter()
        .filter(|r| r.expires_at.map_or(true, |e| (e as u64) > now))
        .map(|r| public_invite::locator_hex(&crate::simd::hex::hex_to_bytes_32(&r.token)))
        .collect();
    publish_my_invite_links(transport, community, &locators).await?;
    Ok(locators)
}

/// Fetch + INGEST the channel append-plane across ALL held epochs (messages + presence) into the local
/// store. The retain set for a rekey is computed from this store (`community_member_activity`), and the
/// no-role chatters live ONLY here — not in the control plane — so a privatize/ban must observe it first or
/// it would shed anyone the re-founder hasn't already synced. Best-effort per channel; uses the multi-epoch
/// fetch so activity under any retained epoch counts. `SessionGuard`-gated across the fetches.
async fn observe_channel_activity<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
) -> Result<(), String> {
    let session = SessionGuard::capture();
    let my_pk = crate::state::my_public_key().ok_or("no local identity to observe channel activity")?;
    for channel in &community.channels {
        let events = super::send::fetch_channel_events(transport, community, channel)
            .await
            .unwrap_or_default();
        if !session.is_valid() {
            return Err("account changed during activity observation".to_string());
        }
        let outcomes = {
            let mut st = crate::state::STATE.lock().await;
            super::inbound::process_channel_batch(&mut st, &events, channel, &my_pk)
        };
        let ch_hex = channel.id.to_hex();
        for o in &outcomes {
            match o {
                super::inbound::IncomingEvent::NewMessage(m)
                | super::inbound::IncomingEvent::Updated { message: m, .. } => {
                    let _ = crate::db::events::save_message(&ch_hex, m).await;
                }
                super::inbound::IncomingEvent::Presence { npub, joined, event_id, created_at, invited_by, invited_label } => {
                    let et = if *joined {
                        crate::stored_event::SystemEventType::MemberJoined
                    } else {
                        crate::stored_event::SystemEventType::MemberLeft
                    };
                    let note = invited_by.as_ref().map(|by| match invited_label {
                        Some(l) if !l.is_empty() => format!("{by}|{l}"),
                        _ => by.clone(),
                    });
                    let _ = crate::db::events::save_system_event_at(event_id, &ch_hex, et, npub, note.as_deref(), *created_at, invited_by.as_deref(), invited_label.as_deref()).await;
                }
                super::inbound::IncomingEvent::WebxdcPeer { npub, topic_id, node_addr, event_id, created_at } => {
                    persist_webxdc_signal(&ch_hex, npub, topic_id, node_addr.as_deref(), event_id, *created_at).await;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// FRESHEN-BEFORE-WRITE guard for an administrative write (rekey / ban / kick / grant / revoke / metadata):
/// hop any base rotation + fold the LATEST control plane from ALL relays + (for a rekey) ingest channel
/// activity, so the write acts on the freshest reachable truth — not just a stale local view. The
/// demonstrated bug this fixes: privatizing before observing a member's activity wrongly cut them.
///
/// BEST-EFFORT, not hard-fail: the refuse-downgrade FLOORS already prevent the write from acting on
/// rolled-back state (the fold can't apply below what we hold), so blocking when relays are unreachable
/// would only forbid legitimate admin actions during an outage (e.g. you couldn't ban anyone). The one
/// hard stop is REMOVAL — if an authorized base rotation has cut us, we must not be writing at all.
/// Returns the refreshed community.
pub async fn sync_before_admin_write<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    observe_activity: bool,
) -> Result<Community, String> {
    // Hop any base rotation we missed; abort only if it REMOVED us (we shouldn't be writing then).
    if catch_up_server_root(transport, community).await?.removed {
        return Err("you have been removed from this community".to_string());
    }
    let community = crate::db::community::load_community(&community.id)?
        .ok_or("community gone during admin sync")?;
    let cid = community.id.to_hex();
    // ONE fresh control fetch+fold from all relays, applied (banlist/roles/metadata/invites) so the roster +
    // floors the write reads are as current as the relays can make them; its raw event count doubles as the
    // isolation signal (no separate probe). Floors guard against stale/rolled-back data, so we DON'T block on
    // "can't confirm latest" — only on true ISOLATION: if we KNOW a control plane exists (we hold edition
    // heads) but NO relay returned ANY control event, an admin decision made blind (and unpublishable) must
    // not happen. A community with no published plane (no local heads) has nothing to confirm → proceed.
    let responded = fetch_and_apply_control(transport, &community).await.map(|n| n > 0).unwrap_or(false);
    let hold_local_heads = !crate::db::community::get_all_edition_heads_epoched(&cid)?.is_empty();
    if hold_local_heads && !responded {
        return Err("can't reach any relay to confirm this community's current state — administrative actions are blocked while offline (try again when connected)".to_string());
    }
    let community = crate::db::community::load_community(&community.id)?
        .ok_or("community gone during admin sync")?;
    // For a rekey, ingest channel activity so the retain set sees no-role chatters too (they live only in
    // the message/presence history, not the control plane).
    if observe_activity {
        let _ = observe_channel_activity(transport, &community).await;
    }
    crate::db::community::load_community(&community.id)?.ok_or("community gone during admin sync".to_string())
}

/// Drive a read-cut (re-founding) to completion, DURABLY. Sets `read_cut_pending` as the intent BEFORE
/// the work and clears it only on full success — so a transient failure (relay outage, power cut, mid-cut
/// account swap) leaves it pending, and the next ban OR a community sync ([`retry_pending_read_cut`])
/// resumes EXACTLY where it stopped (no double base rotation, channels picked up where they left off).
///
/// `fresh` distinguishes a NEW exclusion delta (a ban add / a privatize transition) from a pure RESUME: a
/// fresh delta bumps `read_cut_target_epoch` to `base + 1` so the base MUST rotate past it (excluding the
/// newly-removed member) and every channel is re-cut; a resume keeps the in-flight target so an interrupted
/// cut finishes without forcing an extra base rotation.
async fn run_read_cut<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    fresh: bool,
) -> Result<(), String> {
    let cid = community.id.to_hex();
    let session = SessionGuard::capture();
    if fresh {
        // Compute the target from the FRESHEST base epoch in the DB (the passed struct may predate a recent
        // rotation), so a fresh exclusion always lands at an epoch strictly past the current root.
        let base = crate::db::community::load_community(&community.id)?
            .map(|c| c.server_root_epoch.0)
            .unwrap_or(community.server_root_epoch.0);
        crate::db::community::set_read_cut_target_epoch(&cid, base.saturating_add(1))?;
    }
    crate::db::community::set_read_cut_pending(&cid, true)?;
    reseal_base_to_observed(transport, community).await?;
    if session.is_valid() {
        crate::db::community::set_read_cut_pending(&cid, false)?;
    }
    Ok(())
}

/// Re-seal the base / server-root key to the current OBSERVED-PARTICIPANTS set
/// (`community_member_activity` — everyone who posted, reacted, or announced a join, minus those who
/// left or were banned). The shared read-cut behind two actions: PRIVATIZE (revoking the last link →
/// re-found, sealing link-joined lurkers) and REKEY-ON-REMOVAL (a ban in a Private community →
/// forward-exclude the banned member, who is absent from the observed set because the banlist filters
/// them out). The re-keyer (here, the owner) is always included (`rotate_server_root` adds its own self).
/// Honest joiners are observable because they emit a `join` Presence on accept, so a removed member
/// is the only one shed. `rotate_server_root` re-anchors the control plane (incl. the current banlist +
/// the registry head) under the new epoch, so post-rotation peers read complete authority state.
async fn reseal_base_to_observed<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
) -> Result<(), String> {
    let session = SessionGuard::capture();
    let cid = community.id.to_hex();
    // BLOCK-UNTIL-SYNCED: fold the latest control plane + ingest channel activity from ALL relays BEFORE
    // computing the retain set, so it reflects current truth (roster ∪ presence ∪ activity), not a stale
    // local view. The demonstrated bug: privatizing before observing a member's posts cut them. Fails closed
    // if no relay confirms our head — better to abort the rekey than shed real members on a partial view.
    let community = &sync_before_admin_write(transport, community, true).await?;
    // `community_member_activity` returns npubs in the events table's BECH32 form (`npub1...`), so parse
    // with `PublicKey::parse` (bech32 OR hex) — `from_hex` would reject every one, emptying the set and
    // sealing the community down to the owner alone (the re-founding inverted).
    let participants: Vec<nostr_sdk::PublicKey> = crate::db::community::community_member_activity(&cid)?
        .into_iter()
        .filter_map(|(npub, _)| nostr_sdk::PublicKey::parse(&npub).ok())
        .collect();
    // RESUMABLE re-founding (durable across interruption — outage, power cut, mass relay failure mid-cut).
    // A re-founding rotates the base THEN each channel key; a naive retry would re-run BOTH from scratch
    // (a second base epoch + full control-plane re-anchor, and re-rotation of channels already done).
    //
    // `target` = the base epoch THIS pending cut must reach (set durably when the cut was triggered). The
    // base is rotated ONLY while the OBSERVABLE base epoch is below it — so a crash AFTER the base advanced
    // but BEFORE any flag write never double-rotates (the decision reads the real epoch, not a separate
    // flag that could be out of step). `rotate_server_root` reuses its archived root + recomputes the epoch
    // from the DB head, so even a retry of the base itself is idempotent (no same-epoch fork).
    let target = crate::db::community::get_read_cut_target_epoch(&cid)?;
    if community.server_root_epoch.0 < target {
        rotate_server_root(transport, community, &participants).await?;
        if !session.is_valid() {
            return Err("account changed during re-founding".to_string());
        }
    }
    // O2: the base rotation cuts the control plane + @everyone, but channel MESSAGES are sealed under
    // per-channel keys — so a removed member who held a channel key would keep reading NEW messages.
    // Rotate every channel key to the retained set. Reload first so we see the freshest per-channel rekey
    // progress + the new base epoch. (SessionGuard: a mid-rotation account swap must not reload/rotate
    // against the wrong account's pool.)
    let community = crate::db::community::load_community(&community.id)?
        .ok_or("community gone after base rotation")?;
    let cut_epoch = community.server_root_epoch.0;
    // / A-B2 fix: envelope + address each channel rekey under the PRIOR (pre-rotation) root, NOT the new
    // one — mirroring the base rekey. Concurrent re-founders each mint their OWN new root; base convergence
    // adopts ONE and the losers DROP theirs, so a channel rekey sealed under the new root becomes unreadable
    // to any loser (the live-proven channel fork). The prior root is the shared key EVERY retained member
    // still holds through the convergence, so all can open + apply the channel rekey and converge.
    let prior_root = crate::db::community::held_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, cut_epoch.saturating_sub(1))?
        .unwrap_or(*community.server_root_key.as_bytes()); // epoch 0 (no prior) → current root (no fork risk)
    for channel in &community.channels {
        let ch_hex = channel.id.to_hex();
        // Skip channels already rotated for this read-cut — a retry resumes exactly where it stopped, so
        // each pass makes monotonic forward progress (no re-publishing rekeys for finished channels).
        if crate::db::community::channel_rekeyed_at_server_epoch(&cid, &ch_hex)? >= cut_epoch {
            continue;
        }
        rotate_channel(transport, &community, &channel.id, &participants, &prior_root).await?;
        if !session.is_valid() {
            return Err("account changed during re-founding".to_string());
        }
        crate::db::community::mark_channel_rekeyed_at_server_epoch(&cid, &ch_hex, cut_epoch)?;
    }
    Ok(())
}

/// The result of applying a received channel Rekey (3303).
#[derive(Debug, PartialEq, Eq)]
pub enum RekeyOutcome {
    /// The new key was recovered + committed. `head_advanced` is true if it became the channel's
    /// current epoch (a catch-up of an OLDER epoch archives the key but leaves the head, so `false`).
    Applied { head_advanced: bool },
    /// No blob at my recipient locator — I'm not in this rotation's recipient set (a non-member of the
    /// channel, or the member this removal deliberately excluded). Expected, NOT an error.
    NotARecipient,
}

/// Apply a received, already-opened channel Rekey ([`super::rekey::open_rekey_event`]) for `community`.
///
/// Verifies the rotator's authority (`MANAGE_CHANNELS`) against the current roster (owner supreme,
///), checks chain continuity against the held prior-epoch key (fork detection — when held),
/// finds + opens MY per-recipient blob, and commits the new key via `advance_channel_epoch` (the
/// atomic archive+head write). `SessionGuard`-gated: the caller's fetch can straddle an account swap,
/// so the DB write is re-validated immediately before it. Does NOT fetch — the catch-up fetch loop is
/// a later layer. (Scope-pinned + version-pinned authority — evaluating the rotator's rank at the
/// roster version the rekey cites, under block-until-synced — is deferred; server-root rotation has
/// its own apply, deferred.)
pub fn apply_channel_rekey(
    community: &Community,
    parsed: &super::rekey::ParsedRekey,
) -> Result<RekeyOutcome, String> {
    // Fully synchronous (no `.await`), so a session swap can't preempt between the MY_SECRET_KEY read
    // and the DB write — one captured guard + one re-check before the write suffices. If a remote
    // signer (bunker) open path ever adds an await here, MY_SECRET_KEY must be re-read after it.
    let session = SessionGuard::capture();

    // Scope must be a channel of THIS community (server-root rotation is a separate, deferred path).
    let channel_id = match parsed.scope {
        super::derive::RekeyScope::Channel(c) => c,
        super::derive::RekeyScope::ServerRoot => {
            return Err("server-root rotation uses apply_server_root_rekey, not the channel path".to_string())
        }
    };
    if !community.channels.iter().any(|c| c.id == channel_id) {
        return Err("rekey targets a channel not in this community".to_string());
    }
    let cid = community.id.to_hex();
    let channel_hex = channel_id.to_hex();

    // Authority: the rotator must hold MANAGE_CHANNELS per the current roster; the owner is
    // supreme. Reject an unauthorized rotation rather than fail open.
    // TODO(scope): is_authorized unions MANAGE_CHANNELS across ALL the rotator's roles regardless of
    // RoleScope — once channel-scoped roles become grantable, gate on the scope covering THIS channel,
    // else a Channel(other)-scoped grant would wrongly authorize rotating this one. MVP roles are all
    // Server-scoped, so the hole is currently unreachable.
    let owner = proven_owner_hex(community);
    let roster = crate::db::community::get_community_roles(&cid).unwrap_or_else(|e| {
        // A DB hiccup degrades (fail-closed) to owner-only authorization; surface it so the resulting
        // "lacks MANAGE_CHANNELS" rejection isn't mistaken for a real authority problem.
        crate::log_warn!("rekey apply: roster read failed ({e}); authorizing owner only");
        Default::default()
    });
    if !roster.is_authorized(
        &parsed.rotator.to_hex(),
        owner.as_deref(),
        super::roles::Permissions::MANAGE_CHANNELS,
    ) {
        return Err("rekey rotator lacks MANAGE_CHANNELS authority".to_string());
    }

    // Chain continuity — relaxed for FORK-CONVERGENCE: if I hold the prior-epoch key this rekey cites
    // and its commitment matches, great (the normal contiguous case). If it MISMATCHES, I'm on a LOSING
    // FORK of the prior epoch (e.g. a concurrent re-founding I lost) while this rekey extends the WINNING
    // fork. It is NOT a foreign chain: the rotator is already authority-verified above (holds MANAGE_CHANNELS)
    // and the ECDH blob below proves it's addressed to ME. So ADOPT it — converge forward onto the authorized
    // chain — rather than reject and strand myself on the dead fork forever. (Authority + recipient are the
    // real gates; the commitment is continuity, which must yield to convergence. Replays of OLD epochs can't
    // reach here: the forward walk only fetches epochs past my head.) My divergent prior-epoch key stays as
    // local history; going forward I'm on the converged key.
    if let Some(prev_key) = crate::db::community::held_epoch_key(&cid, &channel_hex, parsed.prev_epoch.0)? {
        if super::rekey::epoch_key_commitment(parsed.prev_epoch, &prev_key) != parsed.prev_key_commitment {
            crate::log_warn!(
                "channel rekey to epoch {} cites a prior-epoch key I don't hold (I'm on a losing fork of epoch {}) — converging forward onto the authorized chain",
                parsed.new_epoch.0, parsed.prev_epoch.0
            );
        }
    }

    // Find + open MY blob (compute my own recipient locator, no trial-decryption).
    let my_keys = crate::state::MY_SECRET_KEY
        .to_keys()
        .ok_or("no local identity to open the rekey blob")?;
    let secret = super::rekey::rekey_pairwise_secret(my_keys.secret_key(), &parsed.rotator)?;
    let my_locator = super::derive::recipient_pseudonym(&secret, parsed.scope, parsed.new_epoch).to_hex();
    let mine = match parsed.blobs.iter().find(|b| b.locator == my_locator) {
        Some(b) => b,
        None => return Ok(RekeyOutcome::NotARecipient),
    };
    let new_key =
        super::rekey::open_rekey_blob(my_keys.secret_key(), &parsed.rotator, parsed.scope, parsed.new_epoch, mine)?;

    // Commit (N2 dual-write). Re-validate the session straddling the caller's fetch before writing.
    if !session.is_valid() {
        return Err("session changed during rekey apply".to_string());
    }
    let head_advanced =
        crate::db::community::advance_channel_epoch(&cid, &channel_hex, parsed.new_epoch.0, &new_key)?;
    Ok(RekeyOutcome::Applied { head_advanced })
}

/// Mint the new key for a rotation, OR reuse the one a prior (failed-mid-publish) attempt already minted
/// and archived for this `(scope, epoch)`. Reuse is the FORK-SAFETY crux of splitting: a rotation's key
/// is minted ONCE and persisted to the epoch-key archive BEFORE publishing, so a retry re-publishes the
/// SAME key across all chunks — never a second random root for the same epoch (which would split
/// recipients onto incompatible keys). Returns the (zeroized) key.
fn mint_or_reuse_rotation_key(cid: &str, scope_id: &str, epoch: u64) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
    if let Some(k) = crate::db::community::held_epoch_key(cid, scope_id, epoch)? {
        return Ok(zeroize::Zeroizing::new(k));
    }
    let k = zeroize::Zeroizing::new(super::random_32());
    crate::db::community::store_epoch_key(cid, scope_id, epoch, &k)?;
    Ok(k)
}

/// Publish a rotation's per-recipient blobs as one OR MORE 3303 events, SPLIT into chunks of
/// `MAX_REKEY_BLOBS` so each stays under the relay size limit (e.g. 200 recipients → a 120-blob event
/// + an 80-blob event). All chunks share the SAME address (the builder derives it from scope/epoch, not
/// the blobs) and carry the SAME new key, so a recipient finds + recovers their key from whichever chunk
/// holds their blob. Each chunk is published durably; FAIL-FAST if a chunk reaches no relay (the caller
/// leaves its head unadvanced; because the key is persisted + reused on retry, re-publishing carries the
/// SAME key → no same-epoch fork).
async fn publish_rekey_chunked<T, F>(
    transport: &T,
    relays: &[String],
    blobs: &[super::rekey::RekeyBlob],
    build: F,
) -> Result<(), String>
where
    T: Transport + ?Sized,
    F: Fn(&[super::rekey::RekeyBlob]) -> Result<Event, String>,
{
    if blobs.is_empty() {
        return Err("rekey has no recipients".to_string());
    }
    for chunk in blobs.chunks(super::rekey::MAX_REKEY_BLOBS) {
        let event = build(chunk)?;
        transport.publish_durable(&event, relays).await?;
    }
    Ok(())
}

/// Rotate a channel's key (a channel rekey): mint a fresh-random key for `current_epoch + 1`,
/// deliver it to `recipients` as one self-proving 3303 event (epoch + every recipient blob + the
/// prior-epoch commitment + my real-npub authority sig, all in one — the design tenet), publish it,
/// then advance MY local epoch. Returns the new epoch.
///
/// The caller supplies the recipient set (the recipient-set policy — "everyone who stays" — is a
/// separate layer); I am always added (so my other devices recover the key). I must hold
/// `MANAGE_CHANNELS`. **Publish FIRST, advance my head only after a successful publish** — moving my
/// head to an epoch no peer received would strand me. (A post-publish session swap leaves peers ahead
/// of my local head, which self-heals: the rekey is server-root-addressed, so I re-derive my own key
/// on the next fetch.) `SessionGuard`-gated across the publish await.
pub async fn rotate_channel<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    channel_id: &super::ChannelId,
    recipients: &[nostr_sdk::PublicKey],
    // the server root this rekey is ENVELOPED + ADDRESSED under. A standalone channel removal passes the
    // CURRENT root. A re-founding (base rotation) passes the PRIOR (pre-rotation) root — exactly like the
    // base rekey (`base_rekey_pseudonym(prior_root, …)`) — so every RETAINED member can still open it after
    // the base converges to ONE winning root (the losers dropped their own new root). Sealing under the new
    // root instead would strand any base-fork loser on an unreadable channel rekey.
    envelope_root: &[u8; 32],
) -> Result<u64, String> {
    let session = SessionGuard::capture();
    let cid = community.id.to_hex();

    // Authority: I must hold MANAGE_CHANNELS (owner supreme). A rekey needs the RAW local key
    // (the blob locator is a ConversationKey ECDH, which NIP-46 can't expose) — so a bunker account can
    // administer via editions but not rekey. Fails clearly here rather than silently.
    let my_keys = crate::state::MY_SECRET_KEY.to_keys().ok_or("a key rotation requires a local key (bunker/NIP-46 accounts can't rekey)")?;
    let owner = proven_owner_hex(community);
    let roster = crate::db::community::get_community_roles(&cid).unwrap_or_default();
    if !roster.is_authorized(&my_keys.public_key().to_hex(), owner.as_deref(), super::roles::Permissions::MANAGE_CHANNELS) {
        return Err("not authorized to rotate this channel (no MANAGE_CHANNELS)".to_string());
    }

    // Current epoch + key (the chain link we extend). `channel.key` is the head key, kept in lockstep
    // with the archived prev_epoch key by `advance_channel_epoch`, so the commitment computed here
    // matches what the apply side verifies against `held_epoch_key(prev_epoch)`.
    let channel = community
        .channels
        .iter()
        .find(|c| &c.id == channel_id)
        .ok_or("channel not found in community")?;
    let prev_epoch = channel.epoch;
    let new_epoch = super::Epoch(prev_epoch.0.checked_add(1).ok_or("channel epoch overflow")?);
    let prev_commit = super::rekey::epoch_key_commitment(prev_epoch, channel.key.as_bytes());
    // The fresh channel key — minted ONCE + archived, reused on a retry (fork-safety, see
    // `mint_or_reuse_rotation_key`). Zeroized on drop.
    let new_key = mint_or_reuse_rotation_key(&cid, &channel_id.to_hex(), new_epoch.0)?;

    // Recipient set = the supplied stayers ∪ me (deduped), each wrapped a per-recipient blob. Published
    // SPLIT across ≤MAX_REKEY_BLOBS-blob events so a large channel rotates in multiple 64KB-safe
    // events at one address; a recipient recovers from whichever chunk holds their blob.
    let mut seen = std::collections::HashSet::new();
    let mut blobs = Vec::new();
    for pk in recipients.iter().chain(std::iter::once(&my_keys.public_key())) {
        if !seen.insert(pk.to_hex()) {
            continue;
        }
        blobs.push(super::rekey::build_rekey_blob(
            my_keys.secret_key(), pk, super::derive::RekeyScope::Channel(*channel_id), new_epoch, &new_key,
        )?);
    }

    // Publish FIRST (all chunks) — only advance my own head once peers can actually receive the new key.
    publish_rekey_chunked(transport, &community.relays, &blobs, |chunk| {
        super::rekey::build_channel_rekey_event(
            &Keys::generate(), &my_keys, envelope_root, channel_id,
            new_epoch, prev_epoch, &prev_commit, chunk,
        )
    })
    .await?;
    if !session.is_valid() {
        return Err("session changed during channel rotation".to_string());
    }
    crate::db::community::advance_channel_epoch(&cid, &channel_id.to_hex(), new_epoch.0, &new_key)?;
    Ok(new_epoch.0)
}

/// Emit a privatize/rekey progress step to the UI (no-op on headless clients via the unregistered emitter).
/// `pct` is OVERALL progress 0-100 across the whole rotation; `label` is layman-facing. The frontend renders
/// a determinate ring + this label in an unclosable modal so the user is guided through the multi-second op.
fn emit_rekey_progress(label: &str, pct: u8) {
    crate::emit_event("community_rekey_progress", &serde_json::json!({ "label": label, "pct": pct }));
}

/// Rotate the SERVER ROOT (a base rotation — the Private-removal / re-founding read-cut), the
/// complete orchestration: mint a fresh-random new root for `current_base_epoch + 1`, deliver it to
/// `recipients` as one self-proving server-root rekey (enveloped under the PRIOR root, addressed by
/// `base_rekey_pseudonym`), **re-anchor the control plane under the new epoch**, and only then
/// advance MY base head. Returns the new base epoch.
///
/// I am always added to the recipient set (multi-device). I must hold `BAN` (server-wide rotation
/// authority; owner supreme). The ordering is the safety contract: publish the base rekey → re-anchor →
/// advance head, with the **head-advance gated on a successful, count-complete re-anchor** — so a
/// post-rotation joiner who holds only the new root always reaches current authority, and a withholding
/// relay can't advance us over a thinned control plane. The recipient-set policy ("who stays") is still
/// the caller's (privatize/removal flow, #7/#8). `pub(crate)` — exposed only inside the crate until that
/// flow wraps it. Re-anchor carries the whole 3308 control plane (roles, grants, banlist, GroupRoot,
/// channel metadata) — every authority + display entity is preserved across a base rotation.
// Called by the privatize re-founding flow (`privatize_reseal`); also exercised directly by tests.
pub(crate) async fn rotate_server_root<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    recipients: &[nostr_sdk::PublicKey],
) -> Result<u64, String> {
    let session = SessionGuard::capture();
    let cid = community.id.to_hex();

    // a re-founding cannot cross a tombstone. A dissolved community never rotates the base again.
    if crate::db::community::get_community_dissolved(&cid)? {
        return Err("community is dissolved; it cannot be re-founded".to_string());
    }

    // Authority: I must hold BAN (server-wide rotation; owner supreme). Re-founding re-WRAPS each entity
    // head verbatim (never re-authors), so an HONEST re-founder of any rank preserves everything: grants keep
    // their original granter and the owner deed rides along untouched (ownership is unstealable — the deed is
    // owner-signed and verified from the invite bundle, never from the snapshot).
    // KNOWN MVP LIMITATION (audited, accepted): a MALICIOUS non-owner admin (modified client) can OMIT a peer
    // admin's grant from the snapshot to demote them — a privilege escalation, since epoch-primary floors drop
    // the prior-epoch floors so followers can't detect the omission. Accepted because admins are owner-
    // appointed/trusted and the owner recovers (re-grant the peer + remove the bad admin); it can't steal
    // ownership or leak data. The bulletproof fix (verifiable removal: followers reject a snapshot that drops
    // a member the re-founder doesn't outrank) is deferred. Needs the RAW local key (ECDH), so no bunker.
    let my_keys = crate::state::MY_SECRET_KEY.to_keys().ok_or("a base rotation (privatize / private-ban read-cut) requires a local key (bunker/NIP-46 accounts can't rekey)")?;
    let owner = proven_owner_hex(community);
    let roster = crate::db::community::get_community_roles(&cid).unwrap_or_default();
    if !roster.is_authorized(&my_keys.public_key().to_hex(), owner.as_deref(), super::roles::Permissions::BAN) {
        return Err("not authorized to rotate the server root (no BAN)".to_string());
    }

    // Derive prev_epoch / prev_commit / the rekey ENVELOPE root from the FRESHEST base state, never a
    // possibly-stale caller struct: addressing a rotation under a root that's already been superseded (e.g.
    // a re-founder re-rotating from a pre-convergence in-memory struct) lands it at a pseudonym converged
    // members never query → a base re-fork with no past-epoch heal to recover it. Reload first (mirrors
    // run_read_cut's freshest-epoch read); all downstream uses (envelope, re-anchor fetch) then agree.
    let fresh = crate::db::community::load_community(&community.id)?
        .ok_or("community gone before base rotation")?;
    let community = &fresh;
    let prev_epoch = community.server_root_epoch;
    let new_epoch = super::Epoch(prev_epoch.0.checked_add(1).ok_or("server-root epoch overflow")?);
    // Commit to the PRIOR root (the chain link the apply side verifies against `held_epoch_key(prev)`).
    let prev_commit = super::rekey::epoch_key_commitment(prev_epoch, community.server_root_key.as_bytes());
    // The fresh server root — minted ONCE + archived, reused on a retry (fork-safety). Zeroized on drop.
    let new_root = mint_or_reuse_rotation_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, new_epoch.0)?;
    emit_rekey_progress("Rerolling community keys...", 5);

    // ACQUIRE-BEFORE-COMMIT: do EVERY fetch the re-founding needs BEFORE publishing anything. The only
    // mid-rekey fetch is the re-anchor (the current control plane → re-wrapped under the new epoch); its
    // coverage gate (a head not fetchable) is exactly what stranded a published base rekey when it ran AFTER
    // the publish. Fetch + seal it now, so a transient miss aborts the whole re-founding with ZERO published
    // state. Only the publishes below need retry logic. The sealed editions are sent in the commit phase.
    let sealed = prepare_reanchor_control_plane(transport, community, &new_root, new_epoch).await?;
    if !session.is_valid() {
        return Err("session changed during re-founding acquire".to_string());
    }

    let total_recipients = (recipients.len() + 1).max(1); // recipients + me (multi-device)
    let mut seen = std::collections::HashSet::new();
    let mut blobs = Vec::new();
    for pk in recipients.iter().chain(std::iter::once(&my_keys.public_key())) {
        if !seen.insert(pk.to_hex()) {
            continue;
        }
        blobs.push(super::rekey::build_rekey_blob(
            my_keys.secret_key(), pk, super::derive::RekeyScope::ServerRoot, new_epoch, &new_root,
        )?);
        emit_rekey_progress(
            &format!("Preparing keys for members ({}/{})...", blobs.len(), total_recipients),
            (5 + 35 * blobs.len() / total_recipients) as u8,
        );
    }

    // COMMIT phase (publishes only — all fetching is done above). Publish the base rekey (delivers the new
    // root to recipients), SPLIT across ≤MAX_REKEY_BLOBS-blob events so a large recipient set rotates
    // in multiple 64KB-safe events at one address.
    emit_rekey_progress("Sending keys to members...", 42);
    publish_rekey_chunked(transport, &community.relays, &blobs, |chunk| {
        super::rekey::build_server_root_rekey_event(
            &Keys::generate(), &my_keys, community.server_root_key.as_bytes(), &community.id,
            new_epoch, prev_epoch, &prev_commit, chunk,
        )
    })
    .await?;

    // RE-FOUND BY COMPACTION: publish the pre-sealed snapshot (the current folded state re-wrapped as
    // editions under the new epoch) so a post-rotation joiner reaches the new root with reachable authority.
    // Gate the head-advance on EVERY edition landing (O(entities), tiny): a single un-ACKed edition aborts,
    // head-not-advanced is the safe side. A failed publish leaves the base rekey on relays while our head
    // stays put; a retry REUSES the archived root via `mint_or_reuse_rotation_key`, recomputing `new_epoch`
    // from the DB head — no same-epoch fork, idempotent re-publish. (The fetch can no longer fail here: the
    // snapshot was acquired up front, so this commit phase is publish-retry territory only.)
    let snapshot = publish_reanchor_snapshot(transport, &community.relays, sealed).await?;
    if snapshot.iter().any(|e| !e.published) {
        return Err(
            "re-founding aborted: a snapshot edition did not land (rate-limited / unreachable relay?); base head NOT advanced".to_string()
        );
    }
    if !session.is_valid() {
        return Err("session changed during server-root rotation".to_string());
    }
    emit_rekey_progress("Finalizing...", 98);
    // Only now commit: the new root is on relays AND the compacted plane is reachable at the new epoch.
    crate::db::community::advance_server_root_epoch(&cid, new_epoch.0, &new_root)?;
    // Record our carried heads at the (now-committed) new epoch so a subsequent edit chains from them, not
    // the abandoned old-epoch chain. The head is re-wrapped VERBATIM, so its version is preserved; epoch is
    // primary, so it supersedes the prior epoch's head regardless of version.
    for e in &snapshot {
        crate::db::community::set_edition_head_with_id(&cid, &e.entity_hex, e.version, &e.self_hash, &e.inner_id)?;
    }
    Ok(new_epoch.0)
}

/// Re-anchor the control plane after a base rotation: re-post the current control HEADS under the NEW
/// epoch's server-root pseudonym, so a post-rotation joiner (who holds only the new root) reaches current
/// authority with the one control-plane query they can make. Returns the per-entity snapshot it published.
///
/// **Re-WRAP, not re-sign.** Each edition's inner is the original real-npub-signed event — its signature,
/// version, and (community-scoped, rotation-stable) `entity_id` are all preserved; only the outer envelope
/// is fresh (new-root encryption + new-epoch `control_pseudonym` + ephemeral signer). Anyone can re-wrap
/// because the inner signature is what verifies — so the owner deed (carried inside the GroupRoot head) and
/// every grant's original granter survive untouched, which is what lets any BAN-holder re-found without
/// re-authoring or demoting anyone.
///
/// **COMPACTION: re-posts only the per-entity HEAD, not the whole `v1..vN` chain.** Cost is O(entities),
/// not O(history) — the fix for the original full-chain re-anchor, which failed once relays dropped old
/// editions or rate-limited the burst. The head is carried VERBATIM (keeps its real version number), so at
/// the new epoch its `prev_hash` dangles; that's fine because epoch-primary floors put a following member
/// in BOOTSTRAP mode for the new epoch (floor 0), where `fold_roster` surfaces the head via `bootstrap_head`
/// (Policy B) + the authority gate — no contiguous `v1..vN` is needed. (The old chain stays orphaned at the
/// prior epoch.) Only the freshest editions (the heads) need to be fetchable, sidestepping the dropped-old-
/// version wall.
///
/// **SCOPE: every tracked control entity** (GroupRoot, ChannelMetadata, roles, grants, the banlist) — built
/// from `get_all_edition_heads_epoched` and matched to its fetched raw edition; a head we can't fetch ABORTS
/// the rotation (better than stranding members on a thinned plane).
///
/// PRECONDITION: call this while `community` still holds the CURRENT (pre-rotation) root/epoch — it
/// fetches the current plane and re-posts under the new one. Running it after the head advanced would
/// fetch the (empty) new-epoch plane and re-anchor nothing.
///
/// `pub(crate)` + part of the base-rotation orchestration (#4e-2 sequences rekey → re-anchor → advance,
/// gating the head-advance on a successful re-anchor); `SessionGuard`-gated across the fetch + each
/// publish (publish-only — no local DB write, so a mid-loop swap is not a cross-account hazard).
// Reached in production via `rotate_server_root` (the privatize re-founding path); also tested directly.
/// One re-wrapped entity head in a re-founding snapshot: its coordinate + (version, self_hash, inner_id)
/// of the head carried forward (for recording at the new epoch), and whether its publish landed.
pub(crate) struct SnapshotEntry {
    pub entity_hex: String,
    pub version: u64,
    pub self_hash: [u8; 32],
    pub inner_id: [u8; 32],
    pub published: bool,
}

/// ACQUIRE half of the re-anchor (acquire-before-commit): fetch the current control plane and re-wrap every
/// entity head under the new root/epoch — but publish NOTHING. Returns the sealed editions ready to send.
/// The coverage gate (a head not fetchable ABORTS) lives here, so it trips BEFORE any rekey is published —
/// a transient fetch miss then aborts the whole re-founding with ZERO published state (clean retry), instead
/// of stranding a published base rekey with a half-anchored plane. See `rotate_server_root` for the ordering.
pub(crate) async fn prepare_reanchor_control_plane<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    new_root: &[u8; 32],
    new_epoch: super::Epoch,
) -> Result<Vec<(Event, SnapshotEntry)>, String> {
    let session = SessionGuard::capture();
    let cid = community.id.to_hex();

    // RE-FOUND BY COMPACTION: re-wrap each entity's CURRENT HEAD verbatim under the new epoch — ONE
    // edition per entity, not the O(history) chain. "Re-wrap, not re-sign": the inner real-npub signature
    // (and the owner deed riding inside the GroupRoot content) are carried UNCHANGED, so every grant keeps
    // its ORIGINAL granter and authority re-derives identically at the new epoch. That's what lets ANY
    // BAN-holder re-found without demoting peer admins or touching ownership — the re-founder only re-keys
    // + re-addresses, never re-authors. Only the HEADS are needed (the freshest, most-retained editions),
    // so this sidesteps the unfetchable-old-version wall that broke the full-history re-anchor.
    let z = super::roster::control_pseudonym(&community.server_root_key, &community.id, community.server_root_epoch);
    let query = Query { kinds: vec![event_kind::COMMUNITY_CONTROL], z_tags: vec![z], ..Default::default() };
    let outers = transport.fetch(&query, &community.relays).await?;
    if !session.is_valid() {
        return Err("session changed during re-founding fetch".to_string());
    }
    // self_hash → (raw inner edition opened under the CURRENT root, its inner_id).
    let mut by_hash: std::collections::HashMap<[u8; 32], (Event, [u8; 32])> = std::collections::HashMap::new();
    for outer in &outers {
        if let Ok(inner) = super::roster::open_control_edition(outer, &community.server_root_key) {
            if let Ok(parsed) = super::edition::parse_edition_inner(&inner) {
                by_hash.insert(parsed.self_hash, (inner, parsed.inner_id));
            }
        }
    }

    // Each entity's CURRENT head (the floors recorded at the current epoch) → re-wrap that exact edition
    // verbatim under the new root/epoch. A head we can't fetch ABORTS (better than stranding members on a
    // plane missing an entity); heads are the freshest editions, so the relay union almost always has them.
    let new_root_key = super::ServerRootKey(*new_root);
    let mut sealed: Vec<(Event, SnapshotEntry)> = Vec::new();
    for (entity_hex, (epoch, version, self_hash)) in crate::db::community::get_all_edition_heads_epoched(&cid)? {
        if epoch != community.server_root_epoch.0 {
            continue; // only the current founding's heads (a stale prior-epoch head is already superseded)
        }
        let (inner, inner_id) = by_hash.get(&self_hash).ok_or_else(|| {
            format!("re-founding aborted: head edition for entity {entity_hex} (v{version}) not fetchable — aborting so no member is stranded")
        })?;
        let outer = super::roster::seal_control_edition(&Keys::generate(), inner, &new_root_key, &community.id, new_epoch)?;
        sealed.push((outer, SnapshotEntry { entity_hex, version, self_hash, inner_id: *inner_id, published: false }));
    }
    Ok(sealed)
}

/// COMMIT half of the re-anchor: publish the (already-fetched + sealed) snapshot editions. Publishing only —
/// no fetch — so the caller's acquire phase guarantees there's nothing left that could fail-to-fetch here.
/// Each `published` flag reports whether that edition landed; the caller gates the head-advance on all true.
pub(crate) async fn publish_reanchor_snapshot<T: Transport + ?Sized>(
    transport: &T,
    relays: &[String],
    sealed: Vec<(Event, SnapshotEntry)>,
) -> Result<Vec<SnapshotEntry>, String> {
    // Publish THROTTLED (a bounded window, not an all-at-once burst) so the snapshot survives rate-limited
    // relays — the 0/N stall that the old concurrent re-anchor hit. Volume is O(entities), so this is small.
    use futures_util::stream::StreamExt;
    let total = sealed.len().max(1);
    let done = std::sync::atomic::AtomicUsize::new(0);
    let done_ref = &done;
    emit_rekey_progress(&format!("Re-founding community (0/{total})..."), 50);
    let out: Vec<SnapshotEntry> = futures_util::stream::iter(sealed.into_iter().map(|(ev, mut entry)| async move {
        entry.published = transport.publish_durable(&ev, relays).await.is_ok();
        let n = done_ref.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        emit_rekey_progress(&format!("Re-founding community ({n}/{total})..."), (50 + 45 * n / total) as u8);
        entry
    }))
    .buffer_unordered(4)
    .collect()
    .await;
    Ok(out)
}

/// Re-anchor in one shot (fetch + seal + publish). Test-only convenience; production splits the two halves
/// (`prepare_*` then `publish_*`) so the fetch precedes any rekey publish (acquire-before-commit).
#[cfg(test)]
pub(crate) async fn reanchor_control_plane<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    new_root: &[u8; 32],
    new_epoch: super::Epoch,
) -> Result<Vec<SnapshotEntry>, String> {
    let sealed = prepare_reanchor_control_plane(transport, community, new_root, new_epoch).await?;
    publish_reanchor_snapshot(transport, &community.relays, sealed).await
}

/// Apply a received, already-opened SERVER-ROOT (base) Rekey for `community` — the base counterpart to
/// [`apply_channel_rekey`]. Verifies the rotator's server-wide rotation authority (`BAN`, "role-based,
/// not owner-only"), checks continuity against the held prior ROOT (when held), finds + opens MY
/// ServerRoot-scope blob, and commits the new root via the atomic base head+archive write. The new root
/// reaches me ONLY through my ECDH blob — if I was removed in this rotation I find no blob
/// (`NotARecipient`) and recover nothing. `SessionGuard`-gated; synchronous (one guard + the write
/// re-check suffice, same as `apply_channel_rekey`).
pub fn apply_server_root_rekey(
    community: &Community,
    parsed: &super::rekey::ParsedRekey,
) -> Result<RekeyOutcome, String> {
    let session = SessionGuard::capture();

    // Scope must be the server root (a Channel rekey is the other path).
    if !matches!(parsed.scope, super::derive::RekeyScope::ServerRoot) {
        return Err("not a server-root rekey (channel rekeys use apply_channel_rekey)".to_string());
    }
    let cid = community.id.to_hex();

    // a re-founding cannot cross a tombstone. Once dissolved, a base rekey is a "subsequent control
    // event" → refuse to advance the epoch (a rekey after a tombstone is invalid).
    if crate::db::community::get_community_dissolved(&cid)? {
        return Err("community is dissolved; base epoch cannot advance".to_string());
    }

    // Authority: a server-wide rotation is gated on BAN (owner supreme). The deed-derived `owner` is
    // the chain root — a community whose deed is missing/stripped yields `owner = None`, so NO rotator
    // authorizes (a deedless re-founding is followed by no one). A roster-read failure degrades to
    // owner-only (fail-closed): a stale/unreadable roster only UNDER-authorizes a non-owner, never over-.
    // Version-pinned rotator authority (spec §6 rule 1) is deferred, as on the channel path; the
    // banlist-precedence gate + the heal's deauthorized-root abandonment are the implemented mitigations.
    let owner = proven_owner_hex(community);
    let roster = crate::db::community::get_community_roles(&cid).unwrap_or_else(|e| {
        crate::log_warn!("base rekey apply: roster read failed ({e}); authorizing owner only");
        Default::default()
    });
    if !rotator_is_authorized(&cid, &roster, owner.as_deref(), &parsed.rotator.to_hex(), super::roles::Permissions::BAN) {
        return Err("base rekey rotator lacks server-wide rotation authority (BAN)".to_string());
    }

    // Chain continuity: if I hold the prior ROOT and its commitment mismatches, I'm on a LOSING fork of
    // that epoch (a concurrent re-founding I lost) while this rekey extends the WINNING fork. As with channel
    // rekeys, that is NOT a foreign chain — the rotator is authority-verified (BAN) above and the ECDH blob
    // below proves it's addressed to ME — so ADOPT it (converge forward / reorg onto the authorized chain)
    // rather than reject and strand myself on the dead fork, which would stall every later base rotation too.
    // Replays of OLD epochs can't reach here: the forward walk only fetches epochs past my head.
    if let Some(prev_root) =
        crate::db::community::held_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, parsed.prev_epoch.0)?
    {
        if super::rekey::epoch_key_commitment(parsed.prev_epoch, &prev_root) != parsed.prev_key_commitment {
            crate::log_warn!(
                "base rekey to epoch {} cites a prior-root I don't hold (I'm on a losing fork of epoch {}) — converging forward onto the authorized chain",
                parsed.new_epoch.0, parsed.prev_epoch.0
            );
        }
    }

    // Find + open MY blob (ServerRoot scope).
    let my_keys = crate::state::MY_SECRET_KEY.to_keys().ok_or("no local identity to open the base rekey blob")?;
    let secret = super::rekey::rekey_pairwise_secret(my_keys.secret_key(), &parsed.rotator)?;
    let my_locator = super::derive::recipient_pseudonym(&secret, parsed.scope, parsed.new_epoch).to_hex();
    let mine = match parsed.blobs.iter().find(|b| b.locator == my_locator) {
        Some(b) => b,
        None => return Ok(RekeyOutcome::NotARecipient),
    };
    let new_root =
        super::rekey::open_rekey_blob(my_keys.secret_key(), &parsed.rotator, parsed.scope, parsed.new_epoch, mine)?;

    if !session.is_valid() {
        return Err("session changed during base rekey apply".to_string());
    }
    let head_advanced = crate::db::community::advance_server_root_epoch(&cid, parsed.new_epoch.0, &new_root)?;
    Ok(RekeyOutcome::Applied { head_advanced })
}

/// How many candidate epochs the catch-up scan derives + fetches per round. All rekey pseudonyms are
/// server-root-derived, so a member computes the whole window up front and fetches it in ONE batched
/// `#z` REQ (not a sequential walk). One round covers up to this many missed rotations.
const REKEY_CATCHUP_WINDOW: u64 = 64;
/// Backstop on catch-up rounds — bounds an endless slide (e.g. a relay fabricating contiguous rekeys).
/// At `REKEY_CATCHUP_WINDOW` epochs/round this still covers thousands of real rotations before bailing.
const MAX_REKEY_CATCHUP_ROUNDS: usize = 64;

/// Converge a SET of held channel epochs to the deterministic LOWEST authorized key on the wire (the
/// concurrent-rekey tiebreak), in ONE batched fetch per held server root. Two MANAGE_CHANNELS holders can rotate an epoch
/// with different keys (a concurrent-rekey fork); both forked rekeys collide under the PRIOR (shared) server
/// root, so search every held root, peek the key each delivers to ME, and adopt the lowest. Heals the head,
/// any epoch reorged THIS sync, AND the recent window of held epochs — the last covers a member that reorged
/// its head under an EARLIER build (so the in-sync forked-epoch set was never populated) yet still sits on a
/// losing sibling at a past epoch whose messages would otherwise stay unreadable.
///
/// Converge DOWN only: a held epoch is re-keyed only to a sibling STRICTLY lower than the key it already
/// holds, so a flaky round that returns just the higher sibling can't re-fork a converged epoch. Epochs I do
/// NOT hold are left to the gap-fill / forward walk (recovery via `apply`, not a same-epoch swap).
async fn heal_channel_fork_epochs<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    channel_id: &super::ChannelId,
    cid: &str,
    channel_hex: &str,
    epochs: &std::collections::BTreeSet<u64>,
    server_roots: &[[u8; 32]],
    session: &SessionGuard,
) -> Result<(), String> {
    if epochs.is_empty() {
        return Ok(());
    }
    let owner_hex = proven_owner_hex(community);
    let roster = crate::db::community::get_community_roles(cid).unwrap_or_default();
    // Batched fetch: every target epoch's rekey under each held root, ONE query per root (mirrors the
    // forward walk). Track the lowest key delivered to ME by an authorized rotator, per epoch.
    let mut winner: std::collections::BTreeMap<u64, [u8; 32]> = std::collections::BTreeMap::new();
    for sr in server_roots {
        let z_tags: Vec<String> = epochs
            .iter()
            .map(|e| super::derive::rekey_pseudonym(&super::ServerRootKey(*sr), channel_id, super::Epoch(*e)).to_hex())
            .collect();
        let q = Query { kinds: vec![event_kind::COMMUNITY_REKEY], z_tags, ..Default::default() };
        for ev in transport.fetch(&q, &community.relays).await.unwrap_or_default() {
            let Ok(p) = super::rekey::open_rekey_event(&ev, sr) else { continue };
            if !matches!(p.scope, super::derive::RekeyScope::Channel(c) if &c == channel_id) || !epochs.contains(&p.new_epoch.0) {
                continue;
            }
            if !rotator_is_authorized(cid, &roster, owner_hex.as_deref(), &p.rotator.to_hex(), super::roles::Permissions::MANAGE_CHANNELS) {
                continue;
            }
            let Some(key) = peek_my_channel_key(&p) else { continue }; // not a recipient of this candidate
            winner.entry(p.new_epoch.0).and_modify(|best| { if key < *best { *best = key; } }).or_insert(key);
        }
    }
    for (epoch, win_key) in winner {
        if !session.is_valid() {
            return Err("session changed during channel convergence".to_string());
        }
        // Only re-converge an epoch I ALREADY hold, and only DOWNWARD.
        // ACCEPTED MVP LIMITATION (GROUP_PROTOCOL.md): adoption checks blob-opens + authority, NOT that
        // the key decrypts extant messages — so a malicious MANAGE_CHANNELS holder can darken a settled past
        // epoch with a fresh lower key. Data-availability only, trusted-admin only; content-bind hardening deferred.
        if let Ok(Some(cur)) = crate::db::community::held_epoch_key(cid, channel_hex, epoch) {
            if win_key < cur {
                // `false` = the channel head moved off `epoch` between read and write (benign race); trace it
                // so a fork that keeps failing to converge is diagnosable in the field without changing flow.
                match crate::db::community::converge_channel_epoch(cid, channel_hex, epoch, &win_key) {
                    Ok(false) => crate::log_trace!("channel heal: converge of epoch {epoch} did not apply (head moved)"),
                    Err(e) => crate::log_trace!("channel heal: converge of epoch {epoch} errored: {e}"),
                    Ok(true) => {}
                }
            }
        }
    }
    Ok(())
}

/// Catch a channel up to the latest epoch it is still a recipient of (windowed scan): fetch every
/// rekey published since our held epoch and apply the chain. Returns the channel's new current epoch.
/// Idempotent + cheap on the steady state (no new rotations → one empty-window fetch → returns the
/// held epoch). 3303s are addressed by the server-root-derived `rekey_pseudonym`, so this is a SEPARATE
/// fetch from the channel message plane (the exception).
///
/// **Removal is terminal.** Within a channel, the recipient set is forward-monotonic — once a member
/// is removed they are excluded from every later rotation, and re-addition is an out-of-band INVITE
/// that resets them to a fresh starter epoch (NOT something this scan discovers). So the walk stops at
/// the first `NotARecipient`: there is nothing legitimate past it for us. A *missing* intermediate
/// epoch (a relay-incomplete gap, where we ARE still a recipient on both sides) is logged and stepped
/// over (the hole stays unreadable until re-fetched from another relay), not treated as removal.
/// `SessionGuard`-gated; applies in ascending epoch order so each rekey's prior-key continuity check
/// sees the key its predecessor just archived.
pub async fn catch_up_channel_rekeys<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
    channel_id: &super::ChannelId,
) -> Result<u64, String> {
    let session = SessionGuard::capture();
    let server_root = community.server_root_key.as_bytes();
    let cid = community.id.to_hex();
    let channel_hex = channel_id.to_hex();
    // A channel rekey is addressed AND encrypted under whatever server root was current when it was
    // published — and the root itself ratchets on every base rotation. So derive + open the rekey window
    // under EVERY held server-root key, not just the current head: a channel rekey published under a prior
    // root is otherwise both unfindable (wrong pseudonym) and undecryptable, leaving permanent channel-key
    // gaps (and every message under those epochs stranded). We hold all prior roots in the epoch archive.
    let mut server_roots: Vec<[u8; 32]> = crate::db::community::held_epoch_keys(&cid, crate::community::SERVER_ROOT_SCOPE_HEX)
        .unwrap_or_default()
        .into_iter()
        .map(|(_, k)| k)
        .collect();
    if !server_roots.iter().any(|r| r == server_root) {
        server_roots.push(*server_root); // ensure the current root is covered even if the archive lags
    }
    let mut head = community
        .channels
        .iter()
        .find(|c| &c.id == channel_id)
        .ok_or("channel not found in community")?
        .epoch
        .0;

    // Past epochs I reorged through (applied a rekey whose cited prior key I don't hold — I'm on a losing
    // fork there). The forward walk converges my HEAD, but a forked PAST epoch keeps the wrong sibling's key
    // and its messages stay unreadable. Collect them here and re-converge each to the lowest sibling below.
    let mut forked_epochs: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();

    for _round in 0..MAX_REKEY_CATCHUP_ROUNDS {
        let window_top = head.saturating_add(REKEY_CATCHUP_WINDOW);
        // Derive + fetch the window under EACH held server root (a channel rekey lives under the root that
        // was current at its publish). Window × |held roots| is small and catch-up is rare; opening with
        // the SAME root that addressed each batch is unambiguous (a wrong root just fails the MAC).
        let mut parsed: Vec<super::rekey::ParsedRekey> = Vec::new();
        for sr in &server_roots {
            let z_tags: Vec<String> = (head.saturating_add(1)..=window_top)
                .map(|e| super::derive::rekey_pseudonym(&super::ServerRootKey(*sr), channel_id, super::Epoch(e)).to_hex())
                .collect();
            let query = Query { kinds: vec![event_kind::COMMUNITY_REKEY], z_tags, ..Default::default() };
            // Best-effort: a transient relay error on the forward-walk fetch must NOT abort the whole
            // catch-up (the caller ignores the Result), which would silently SKIP the current-head
            // convergence heal below — leaving a concurrent-rekey fork unhealed. Treat a failed fetch as
            // "no events here this round"; the next sync re-walks.
            for ev in transport.fetch(&query, &community.relays).await.unwrap_or_default() {
                if let Ok(p) = super::rekey::open_rekey_event(&ev, sr) {
                    if matches!(p.scope, super::derive::RekeyScope::Channel(c) if &c == channel_id) {
                        parsed.push(p);
                    }
                }
            }
        }
        if parsed.is_empty() {
            break; // no rekey exists past `head` under any held root
        }
        // Apply in ascending epoch order (so each rekey's prior-key continuity sees its predecessor's key).
        parsed.sort_by_key(|p| p.new_epoch.0);
        let max_found = parsed.last().map(|p| p.new_epoch.0).unwrap_or(head);

        let head_before = head;
        let mut removed = false;
        // GROUP BY EPOCH: a rotation may be SPLIT across multiple chunk events at the same address.
        // For each epoch try every chunk — Applied if ANY chunk holds my blob; "removed" only if a VALID
        // chunk said NotARecipient and none Applied (a NotARecipient on one chunk just means my blob is
        // in another). All-errored at an epoch = a gap (skip), not a removal.
        let mut by_epoch: std::collections::BTreeMap<u64, Vec<&super::rekey::ParsedRekey>> = std::collections::BTreeMap::new();
        for p in &parsed {
            by_epoch.entry(p.new_epoch.0).or_default().push(p);
        }
        for (e, chunks) in by_epoch {
            if !session.is_valid() {
                return Err("session changed during rekey catch-up".to_string());
            }
            let mut applied = false;
            let mut saw_not_recipient = false;
            for p in &chunks {
                match apply_channel_rekey(community, p) {
                    Ok(RekeyOutcome::Applied { .. }) => {
                        applied = true;
                        break;
                    }
                    Ok(RekeyOutcome::NotARecipient) => saw_not_recipient = true,
                    Err(err) => crate::log_warn!("rekey catch-up: skipping epoch {e} chunk: {err}"),
                }
            }
            if applied {
                // Reorg detection: if this rekey continues from a prior epoch whose key I hold but whose
                // commitment mismatches, I just converged forward off a losing fork — that prior epoch is forked
                // and needs its own lowest-key heal (else its messages stay unreadable under the wrong sibling).
                // All chunks of one rotation carry IDENTICAL continuity fields (same prev_epoch + prev_commit —
                // they're the same rotation split across size-bounded events), so `first()` is representative.
                if let Some(p) = chunks.first() {
                    let pe = p.prev_epoch.0;
                    if let Ok(Some(prev_key)) = crate::db::community::held_epoch_key(&cid, &channel_hex, pe) {
                        if super::rekey::epoch_key_commitment(p.prev_epoch, &prev_key) != p.prev_key_commitment {
                            forked_epochs.insert(pe);
                        }
                    }
                }
                // A non-contiguous jump means intermediate epochs weren't recovered (a relay gap) —
                // surface the hole (that history stays unreadable until re-fetched).
                if e > head + 1 {
                    crate::log_warn!(
                        "rekey catch-up: channel epochs {}..={} not recovered (key gap; history unreadable until re-fetched)",
                        head + 1, e - 1
                    );
                }
                head = head.max(e);
            } else if saw_not_recipient {
                // A valid rotation at this epoch held no blob for me across ALL its chunks ⇒ I was removed
                // here. Forward-terminal (re-add is a fresh invite), so stop — nothing past it is ours.
                removed = true;
                break;
            }
            // else: all chunks at this epoch errored (gap/forged) — don't advance, don't remove.
        }

        // Stop on removal (terminal), when a full round advanced nothing (only gaps/forged events — no
        // legit rekey for us here), or when the window wasn't saturated (we've reached the latest).
        if removed || head == head_before || max_found < window_top {
            break;
        }
    }

    // BACKWARD gap-fill (heal): the forward walk above advances the HEAD and can leapfrog an epoch
    // whose rekey wasn't found (a prior catch-up that lacked the addressing root, or a relay miss). Those
    // holes are below `head`, so the forward window never revisits them — yet we're entitled to those
    // keys. Re-fetch each MISSING epoch's rekey under every held server root and apply it (archive-only:
    // `advance_channel_epoch` never regresses the head), so stranded history (messages under a skipped
    // epoch) becomes readable. Non-ratcheted keys make this pure random-access — no replay needed.
    let held: std::collections::HashSet<u64> = crate::db::community::held_epoch_keys(&cid, &channel_hex)
        .unwrap_or_default()
        .into_iter()
        .map(|(e, _)| e.0)
        .collect();
    let missing: Vec<u64> = (0..head).filter(|e| !held.contains(e)).collect();
    if !missing.is_empty() {
        for sr in &server_roots {
            if !session.is_valid() {
                return Err("session changed during rekey gap-fill".to_string());
            }
            let z_tags: Vec<String> = missing
                .iter()
                .map(|e| super::derive::rekey_pseudonym(&super::ServerRootKey(*sr), channel_id, super::Epoch(*e)).to_hex())
                .collect();
            let query = Query { kinds: vec![event_kind::COMMUNITY_REKEY], z_tags, ..Default::default() };
            // Best-effort (same rationale as the forward walk): a relay error on a gap-fill fetch must not
            // abort before the convergence heal.
            for ev in transport.fetch(&query, &community.relays).await.unwrap_or_default() {
                if let Ok(p) = super::rekey::open_rekey_event(&ev, sr) {
                    if matches!(p.scope, super::derive::RekeyScope::Channel(c) if &c == channel_id) {
                        let _ = apply_channel_rekey(community, &p); // archive-only for sub-head epochs
                    }
                }
            }
        }
    }

    // CONCURRENT RE-FOUNDING HEAL: converge to the deterministic LOWEST authorized sibling at every epoch that
    // can be forked — the current HEAD (two MANAGE_CHANNELS holders rotated it concurrently with different
    // keys), every epoch I reorged through THIS sync, AND the recent window of held epochs. The window
    // pass heals a member that reorged its head under an EARLIER build (so `forked_epochs` was never populated
    // for it) yet still sits on a losing sibling at a past epoch — otherwise that epoch's messages stay
    // unreadable forever (the gap-fill skips it because a key IS held). One batched fetch per held root.
    if head > 0 && session.is_valid() {
        let lo = head.saturating_sub(REKEY_CATCHUP_WINDOW).max(1);
        let mut epochs: std::collections::BTreeSet<u64> = (lo..=head).collect();
        epochs.append(&mut forked_epochs);
        let _ = heal_channel_fork_epochs(transport, community, channel_id, &cid, &channel_hex, &epochs, &server_roots, &session).await;
    }
    Ok(head)
}

/// Backstop on base-rotation walk steps (base rotations are rare, so this far exceeds any real chain;
/// it bounds a hostile/fabricated chain — which already fails at `apply_server_root_rekey` anyway).
const MAX_BASE_CATCHUP_STEPS: usize = 256;

/// Catch the SERVER ROOT up to its latest epoch — a FORWARD WALK (the base has no stable key above
/// it, so `base_rekey_pseudonym` is keyed by the PRIOR root). Each step: derive the next base rekey's
/// address from the root I currently hold, fetch it, open it under that root, apply it (recovering the
/// NEXT root), and repeat. Returns the new base epoch. One step per base rotation — bounded, and base
/// rotations are rare. Stops on a removal (`NotARecipient` — re-add is a fresh invite, not this walk),
/// when no further base rekey exists, or when a rekey can't be applied (can't get the next root).
///
/// After this advances the base epoch, the caller MUST resync the control plane at the NEW epoch
/// (`control_pseudonym(new_root, …)`) before trusting authority — the re-anchoring guarantees the
/// current heads are reachable there (#4e). This fn only recovers the base keys + advances the head.
/// B2 helper: open MY ServerRoot blob in `parsed` WITHOUT committing, to learn which new root this rotation
/// would deliver me. Lets [`catch_up_server_root`] pick the canonical rotation among concurrent re-foundings
/// before applying any. `Ok(None)` = I'm not a recipient of this rotation (or it's not a base rekey).
fn peek_my_server_root(parsed: &super::rekey::ParsedRekey) -> Result<Option<[u8; 32]>, String> {
    if !matches!(parsed.scope, super::derive::RekeyScope::ServerRoot) {
        return Ok(None);
    }
    let my_keys = crate::state::MY_SECRET_KEY.to_keys().ok_or("no local key to open a base rekey blob")?;
    let secret = super::rekey::rekey_pairwise_secret(my_keys.secret_key(), &parsed.rotator)?;
    let my_locator = super::derive::recipient_pseudonym(&secret, parsed.scope, parsed.new_epoch).to_hex();
    let mine = match parsed.blobs.iter().find(|b| b.locator == my_locator) {
        Some(b) => b,
        None => return Ok(None),
    };
    super::rekey::open_rekey_blob(my_keys.secret_key(), &parsed.rotator, parsed.scope, parsed.new_epoch, mine).map(Some)
}

/// Convergence helper: open MY Channel blob in `parsed` WITHOUT committing, to learn which new channel key this
/// rotation would deliver me. Lets the channel current-head heal pick a deterministic winner (lowest
/// delivered key) among concurrent same-epoch channel rotations. `None` = not a recipient / can't open.
fn peek_my_channel_key(parsed: &super::rekey::ParsedRekey) -> Option<[u8; 32]> {
    let my_keys = crate::state::MY_SECRET_KEY.to_keys()?;
    let secret = super::rekey::rekey_pairwise_secret(my_keys.secret_key(), &parsed.rotator).ok()?;
    let my_locator = super::derive::recipient_pseudonym(&secret, parsed.scope, parsed.new_epoch).to_hex();
    let mine = parsed.blobs.iter().find(|b| b.locator == my_locator)?;
    super::rekey::open_rekey_blob(my_keys.secret_key(), &parsed.rotator, parsed.scope, parsed.new_epoch, mine).ok()
}

pub async fn catch_up_server_root<T: Transport + ?Sized>(
    transport: &T,
    community: &Community,
) -> Result<BaseCatchup, String> {
    let session = SessionGuard::capture();
    let cid = community.id.to_hex();
    let mut head = community.server_root_epoch.0;
    // Set true if the walk stops because an AUTHORIZED base rotation EXCLUDED us (read-cut / private ban):
    // we hold the prior root, opened the rotation, its rotator held BAN per the roster we hold, but no chunk
    // carried our blob. The caller treats this as removal and erases local community data (the cut member
    // can't read the new banlist to learn it the normal way, so this is the catch-all removal signal).
    let mut removed = false;
    // The root I currently hold at `head` — drives the next step's address (prior-root-keyed) + opens it.
    let mut current_root: [u8; 32] = *community.server_root_key.as_bytes();

    for _step in 0..MAX_BASE_CATCHUP_STEPS {
        let next = match head.checked_add(1) {
            Some(n) => n,
            None => break,
        };
        let addr = super::derive::base_rekey_pseudonym(&super::ServerRootKey(current_root), &community.id, super::Epoch(next)).to_hex();
        let query = Query { kinds: vec![event_kind::COMMUNITY_REKEY], z_tags: vec![addr], ..Default::default() };
        let events = transport.fetch(&query, &community.relays).await?;
        if events.is_empty() {
            break; // no base rotation past `head`
        }

        // Open under the root I hold; a base rotation at `next` may be SPLIT across chunk events at this
        // address, so collect ALL chunks for `next`.
        let chunks: Vec<super::rekey::ParsedRekey> = events
            .iter()
            .filter_map(|ev| super::rekey::open_rekey_event(ev, &current_root).ok())
            .filter(|p| matches!(p.scope, super::derive::RekeyScope::ServerRoot) && p.new_epoch.0 == next)
            .collect();
        if chunks.is_empty() {
            break; // nothing valid for `next` under the root we hold
        }

        if !session.is_valid() {
            return Err("session changed during base rekey catch-up".to_string());
        }

        // B2 — CONCURRENT RE-FOUNDING CONVERGENCE. There may be MORE than one rotation at `next` (two
        // BAN-holders re-founding at once, each delivering a DIFFERENT new root to the same observed set).
        // Every member must pick the SAME one or the community forks irrecoverably. Peek the root each
        // rotation would give me, then deterministically choose the LOWEST new-root bytes — convergent for
        // everyone who received both. (The root is the only member-computable rotation identity: the inner
        // event id of "my" chunk differs per member, since each member's blob sits in a different chunk, so
        // it can't be the tiebreak.) A member who only received the losing root heals on the next re-founding.
        //
        // AUTHORITY BEFORE THE TIEBREAK: only a BAN-holder's rotation is a candidate. A plain member
        // holds the prior root + can sign as rotator + build valid ECDH blobs, so without this gate they
        // could forge a byte-LOWER root that honest members would PICK as the winner and then fail to apply
        // (authority is also checked in apply) — stalling them at the prior epoch while others advance: a
        // permanent fork the heal can't recover. Gate here, before `min_by`, exactly like the current-head heal.
        let owner_hex = proven_owner_hex(community);
        let roster = crate::db::community::get_community_roles(&cid).unwrap_or_default();
        let mut candidates: Vec<(&super::rekey::ParsedRekey, [u8; 32])> = Vec::new();
        for parsed in &chunks {
            if !rotator_is_authorized(&cid, &roster, owner_hex.as_deref(), &parsed.rotator.to_hex(), super::roles::Permissions::BAN) {
                continue;
            }
            match peek_my_server_root(parsed) {
                Ok(Some(root)) => candidates.push((parsed, root)),
                Ok(None) => {}
                Err(err) => crate::log_warn!("base rekey catch-up: epoch {next} peek: {err}"),
            }
        }
        let applied = match candidates.into_iter().min_by(|a, b| a.1.cmp(&b.1)) {
            Some((parsed, _)) => match apply_server_root_rekey(community, parsed) {
                Ok(RekeyOutcome::Applied { .. }) => true,
                Ok(RekeyOutcome::NotARecipient) => false, // unreachable: peek already confirmed recipiency
                Err(err) => { crate::log_warn!("base rekey catch-up: epoch {next} apply: {err}"); false }
            },
            None => {
                // No chunk held my blob — I was excluded from this base rotation. If an AUTHORIZED rotator
                // (held BAN per the roster I STILL hold) performed it, this is a read-cut removing me →
                // signal removal so the caller erases. Verify authority so a non-BAN member who merely holds
                // the prior root can't forge an eviction event that tricks me into self-deleting.
                let owner = proven_owner_hex(community);
                let roster = crate::db::community::get_community_roles(&cid).unwrap_or_default();
                if chunks.iter().any(|p| rotator_is_authorized(&cid, &roster, owner.as_deref(), &p.rotator.to_hex(), super::roles::Permissions::BAN)) {
                    removed = true;
                }
                false // removed from the base (terminal) → stop the walk
            }
        };
        if !applied {
            break;
        }
        // Recover the just-archived new root to address the next step.
        match crate::db::community::held_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, next)? {
            Some(root) => {
                current_root = root;
                head = next;
            }
            None => {
                // Shouldn't happen — apply archives the root before returning Applied. If it ever does,
                // a DB-archive invariant broke; stop rather than loop on a stale root.
                crate::log_warn!("base rekey catch-up: epoch {next} applied but its root is not archived; halting walk");
                break;
            }
        }
    }

    // CONCURRENT RE-FOUNDING HEAL (current-head convergence): the forward walk only tiebreaks at head+1, so
    // two BAN-holders who re-founded at the SAME epoch each end on their OWN root and never reconcile each
    // other (only bystanders advancing INTO the epoch do). Re-fetch THIS epoch's base rekeys — they're all
    // at the one address keyed by the PRIOR root we still hold — and if an AUTHORIZED sibling delivers a
    // LOWER root than the one we hold, switch to it (the same lowest-root rule), then re-fold the control
    // plane under the adopted root. Convergent for everyone: the lowest root is the deterministic winner.
    if head > 0 && !removed {
        if let Ok(Some(prior_root)) = crate::db::community::held_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, head - 1) {
            let addr = super::derive::base_rekey_pseudonym(&super::ServerRootKey(prior_root), &community.id, super::Epoch(head)).to_hex();
            let query = Query { kinds: vec![event_kind::COMMUNITY_REKEY], z_tags: vec![addr], ..Default::default() };
            let events = transport.fetch(&query, &community.relays).await.unwrap_or_default();
            let chunks: Vec<super::rekey::ParsedRekey> = events
                .iter()
                .filter_map(|ev| super::rekey::open_rekey_event(ev, &prior_root).ok())
                .filter(|p| matches!(p.scope, super::derive::RekeyScope::ServerRoot) && p.new_epoch.0 == head)
                .collect();
            let owner_hex = proven_owner_hex(community);
            let roster = crate::db::community::get_community_roles(&cid).unwrap_or_default();
            let mut best: Option<(&super::rekey::ParsedRekey, [u8; 32])> = None;
            for p in &chunks {
                // Only an AUTHORIZED re-founding (rotator held BAN, not banned) is a convergence
                // candidate — a non-BAN member who merely holds the prior root can't forge a lower
                // root to hijack the chain.
                if !rotator_is_authorized(&cid, &roster, owner_hex.as_deref(), &p.rotator.to_hex(), super::roles::Permissions::BAN) {
                    continue;
                }
                if let Ok(Some(root)) = peek_my_server_root(p) {
                    if best.as_ref().map_or(true, |(_, br)| root < *br) {
                        best = Some((p, root));
                    }
                }
            }
            // Authority dominates the down-only rule: if the root I currently hold is POSITIVELY
            // identified as a since-deauthorized rotation (its chunk is on the wire, delivers my
            // current root, and its rotator now fails the authority/banlist gate), abandon it for
            // the lowest AUTHORIZED sibling even when that sibling is byte-higher. Without this, a
            // banned admin who raced their own removal with a ground-low re-founding root keeps
            // every member who adopted it partitioned forever — the heal would refuse to climb back
            // to the owner's legitimate (higher) root. Positive identification only: when the
            // current root's chunk is absent (withheld), keep the strict down-only rule so a flaky
            // round can't re-fork a converged epoch.
            let current_deauthorized = chunks.iter().any(|p| {
                matches!(peek_my_server_root(p), Ok(Some(r)) if r == current_root)
                    && !rotator_is_authorized(&cid, &roster, owner_hex.as_deref(), &p.rotator.to_hex(), super::roles::Permissions::BAN)
            });
            if let Some((winner, win_root)) = best {
                let adopt = if current_deauthorized {
                    win_root != current_root
                } else {
                    win_root < current_root
                };
                if adopt {
                    if !session.is_valid() {
                        return Err("session changed during base convergence".to_string());
                    }
                    // Adopt the winner: apply archives its root (no head advance at the same epoch), then
                    // `converge_server_root_epoch` swaps the head root, then re-fold control under it.
                    if apply_server_root_rekey(community, winner).is_ok() {
                        match crate::db::community::converge_server_root_epoch(&cid, head, &win_root) {
                            Ok(false) => crate::log_trace!("base heal: converge of epoch {head} did not apply (head moved)"),
                            Err(e) => crate::log_trace!("base heal: converge of epoch {head} errored: {e}"),
                            Ok(true) => {}
                        }
                        current_root = win_root;
                        if let Ok(Some(fresh)) = crate::db::community::load_community(&community.id) {
                            let _ = fetch_and_apply_control(transport, &fresh).await;
                        }
                    }
                }
            }
        }
    }
    let _ = current_root; // may be unused if no further steps read it
    Ok(BaseCatchup { epoch: head, removed })
}

/// Outcome of [`catch_up_server_root`]: the base epoch reached, and whether an AUTHORIZED base rotation
/// EXCLUDED us (a read-cut / private ban). `removed` is the catch-all "you've been removed" signal for a
/// cryptographically cut member who can no longer read the banlist to learn it the normal way.
#[derive(Debug, Clone, Copy)]
pub struct BaseCatchup {
    pub epoch: u64,
    pub removed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::community::send::fetch_channel_messages;
    use crate::community::transport::{memory::MemoryRelay, Query, Transport};
    use nostr_sdk::prelude::{EventBuilder, Kind};

    /// A transport whose publish always fails (fetch returns nothing) — for testing that
    /// a failed deletion publish doesn't strand the single-use key.
    struct FailingRelay;
    #[async_trait::async_trait]
    impl Transport for FailingRelay {
        async fn publish(&self, _event: &Event, _relays: &[String]) -> Result<(), String> {
            Err("relay unreachable".to_string())
        }
        async fn publish_durable(&self, _event: &Event, _relays: &[String]) -> Result<(), String> {
            Err("relay unreachable".to_string())
        }
        async fn fetch(&self, _query: &Query, _relays: &[String]) -> Result<Vec<Event>, String> {
            Ok(Vec::new())
        }
    }

    /// A relay that selectively fails REKEY (3303) publishes (toggleable), delegating everything else to
    /// an inner [`MemoryRelay`]. Lets a test make a re-seal's base rekey fail while the banlist edition
    /// still lands, then "fix" the relay and verify the read-cut retry recovers.
    struct RekeyFailingRelay {
        inner: MemoryRelay,
        fail_rekey: std::sync::atomic::AtomicBool,
    }
    impl RekeyFailingRelay {
        fn new() -> Self {
            Self { inner: MemoryRelay::new(), fail_rekey: std::sync::atomic::AtomicBool::new(true) }
        }
        fn allow_rekey(&self) {
            self.fail_rekey.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        fn blocks(&self, event: &Event) -> bool {
            self.fail_rekey.load(std::sync::atomic::Ordering::Relaxed)
                && event.kind.as_u16() == crate::stored_event::event_kind::COMMUNITY_REKEY
        }
    }
    #[async_trait::async_trait]
    impl Transport for RekeyFailingRelay {
        async fn publish(&self, event: &Event, relays: &[String]) -> Result<(), String> {
            if self.blocks(event) { return Err("rekey relay down".to_string()); }
            self.inner.publish(event, relays).await
        }
        async fn publish_durable(&self, event: &Event, relays: &[String]) -> Result<(), String> {
            if self.blocks(event) { return Err("rekey relay down".to_string()); }
            self.inner.publish_durable(event, relays).await
        }
        async fn fetch(&self, query: &Query, relays: &[String]) -> Result<Vec<Event>, String> {
            self.inner.fetch(query, relays).await
        }
    }

    static TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(5000);

    fn make_test_npub(n: u32) -> String {
        const BECH32: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let mut payload = vec![b'q'; 58];
        let mut x = n as u64;
        let mut i = 58;
        while x > 0 && i > 0 {
            i -= 1;
            payload[i] = BECH32[(x as usize) % 32];
            x /= 32;
        }
        format!("npub1{}", std::str::from_utf8(&payload).unwrap())
    }

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        let tmp = tempfile::tempdir().unwrap();
        let n = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let account = make_test_npub(n);
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        // Clear any client a prior test installed — else `active_signer()` would prefer that stale
        // client's signer over this test's fresh local identity (cross-test contamination).
        let _ = crate::state::take_nostr_client();
        // A local owner identity so create_community can sign the (now mandatory) owner attestation.
        let owner = Keys::generate();
        crate::state::MY_SECRET_KEY.store_from_keys(&owner, &[]);
        crate::state::set_my_public_key(owner.public_key());
        (tmp, guard)
    }

    // --- apply_channel_rekey (#3c) ---

    /// Build + persist a member-view community whose proven owner is `owner` (attestation signed by
    /// them), archiving the genesis epoch-0 channel key + server root via save_community.
    fn saved_community_owned_by(owner: &Keys) -> Community {
        use nostr_sdk::JsonUtil;
        let mut community = Community::create("HQ", "general", vec!["r".into()]);
        let cid = community.id.to_hex();
        community.owner_attestation = Some(
            crate::community::owner::build_owner_attestation_unsigned(owner.public_key(), &cid)
                .sign_with_keys(owner)
                .unwrap()
                .as_json(),
        );
        crate::db::community::save_community(&community).unwrap();
        community
    }

    /// An in-memory owner-attested Community signed by the SEEDED local identity (so `is_proven_owner`
    /// is true and owner-gated actions like `create_public_invite` pass). NOT saved to the DB — for
    /// tests where the same single DB later plays the joiner.
    fn attested_community(name: &str, channel: &str, relays: Vec<String>) -> Community {
        use nostr_sdk::JsonUtil;
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let mut community = Community::create(name, channel, relays);
        community.owner_attestation = Some(
            crate::community::owner::build_owner_attestation_unsigned(owner.public_key(), &community.id.to_hex())
                .sign_with_keys(&owner).unwrap().as_json(),
        );
        community
    }

    /// Set the local identity (the rekey recipient in these tests).
    fn become_local(me: &Keys) {
        crate::state::MY_SECRET_KEY.store_from_keys(me, &[]);
        crate::state::set_my_public_key(me.public_key());
    }

    /// An owner-authored channel rekey to `new_epoch` carrying one blob for `recipient_pk`, citing the
    /// genesis epoch-0 key as `prev`. Returns the opened ParsedRekey ready for apply.
    fn owner_channel_rekey(
        owner: &Keys,
        community: &Community,
        recipient_pk: &nostr_sdk::PublicKey,
        new_epoch: u64,
        new_key: &[u8; 32],
    ) -> super::super::rekey::ParsedRekey {
        let chan = &community.channels[0];
        let scope = super::super::derive::RekeyScope::Channel(chan.id);
        let blob = super::super::rekey::build_rekey_blob(
            owner.secret_key(), recipient_pk, scope, crate::community::Epoch(new_epoch), new_key,
        )
        .unwrap();
        let commit = super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), chan.key.as_bytes());
        let outer = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), owner, community.server_root_key.as_bytes(), &chan.id,
            crate::community::Epoch(new_epoch), crate::community::Epoch(0), &commit, &[blob],
        )
        .unwrap();
        super::super::rekey::open_rekey_event(&outer, community.server_root_key.as_bytes()).unwrap()
    }

    /// Transport-unified outer dedup: a wire event we've already persisted (its outer id recorded as
    /// the inner's `wrapper_event_id`) is dropped BEFORE decryption on a re-fetch — the same contract
    /// DM gift-wraps get from the wrapper-id layer. This is what keeps a boot/catch-up sweep's re-fetch
    /// of the whole channel page from re-ingesting or re-emitting events we already hold.
    #[tokio::test]
    async fn outer_event_dedup_skips_an_already_persisted_wire_event() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let channel = community.channels[0].clone();
        let chan_hex = channel.id.to_hex();

        // A real wire event (stable outer id) authored by a keyholding member.
        let author = Keys::generate();
        let outer = crate::community::envelope::seal_message(
            &author, &channel.key, &channel.id, channel.epoch, "gm", 1000,
        ).unwrap();
        let outer_hex = outer.id.to_hex();

        // First sight: ingests, and the inner records its OUTER wire id as the wrapper link.
        let mut state = crate::state::ChatState::new();
        let msg = match crate::community::inbound::process_incoming(&mut state, &outer, &channel, &me.public_key()) {
            Some(crate::community::inbound::IncomingEvent::NewMessage(m)) => m,
            _ => panic!("expected NewMessage from a fresh wire event"),
        };
        assert_eq!(msg.wrapper_event_id.as_deref(), Some(outer_hex.as_str()),
            "the inner must carry its outer wire id as wrapper_event_id");

        // Persist exactly as the sweep does (writes wrapper_event_id into the events table).
        crate::db::events::save_message(&chan_hex, &msg).await.unwrap();

        // Re-fetch / relay redelivery of the SAME wire event → dropped before decryption.
        let mut state2 = crate::state::ChatState::new();
        let second = crate::community::inbound::process_incoming(&mut state2, &outer, &channel, &me.public_key());
        assert!(second.is_none(), "an already-processed wire event must dedup before decryption");
    }

    /// The dedup ledger is shared across transports, but NIP-77 negentropy must fingerprint ONLY the
    /// gift-wrap ('nip17') subset — a Concord wrapper in the DM reconciliation set would bloat and skew it.
    #[tokio::test]
    async fn ledger_is_shared_but_negentropy_stays_nip17_only() {
        let (_tmp, _guard) = init_test_db();
        let dm = [0xA1u8; 32];
        let concord = [0xC0u8; 32];
        crate::db::wrappers::save_processed_wrapper(&dm, 100, crate::db::wrappers::TRANSPORT_NIP17).unwrap();
        crate::db::wrappers::save_processed_wrapper(&concord, 200, crate::db::wrappers::TRANSPORT_CONCORD).unwrap();

        // The dedup ledger sees BOTH transports.
        assert!(crate::db::wrappers::processed_wrapper_exists(&dm));
        assert!(crate::db::wrappers::processed_wrapper_exists(&concord));

        // NIP-77 fingerprints only the gift-wrap subset — Concord never leaks into DM sync.
        let items = crate::db::wrappers::load_negentropy_items().unwrap();
        assert_eq!(items.len(), 1, "negentropy must exclude concord wrappers");
        assert_eq!(items[0].0.to_bytes(), dm);
    }

    /// A non-message sub-kind (presence) has no inner row to carry a wrapper_event_id, so it records the
    /// outer id in the shared ledger at process time. A re-fetch then dedups it before decryption, just
    /// like a message — every sub-kind gets the same transport-level skip.
    #[tokio::test]
    async fn non_message_subkind_dedups_via_the_shared_ledger() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let channel = community.channels[0].clone();

        // A presence (3306) wire event from a member — a non-row sub-kind.
        let author = Keys::generate();
        let inner = super::super::envelope::build_inner_typed(
            author.public_key(), &channel.id, channel.epoch,
            crate::stored_event::event_kind::COMMUNITY_PRESENCE, "join", 5, None, &[],
        ).sign_with_keys(&author).unwrap();
        let outer = super::super::envelope::seal_with_signed_inner(
            &Keys::generate(), &inner, &channel.key, &channel.id, channel.epoch,
        ).unwrap();

        // First sight: a Presence outcome, and the outer id is recorded in the ledger.
        let mut state = crate::state::ChatState::new();
        let first = crate::community::inbound::process_incoming(&mut state, &outer, &channel, &me.public_key());
        assert!(matches!(first, Some(crate::community::inbound::IncomingEvent::Presence { .. })),
            "expected a Presence outcome");
        assert!(crate::db::wrappers::processed_wrapper_exists(&outer.id.to_bytes()),
            "a non-message sub-kind must record its outer id in the shared ledger");

        // Re-fetch of the same wire event → dropped before decryption.
        let second = crate::community::inbound::process_incoming(&mut crate::state::ChatState::new(), &outer, &channel, &me.public_key());
        assert!(second.is_none(), "a re-fetched presence must dedup via the shared ledger");
    }

    #[test]
    fn apply_channel_rekey_recovers_and_advances_head() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate(); // owner = rotator (supreme authority)
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();
        let chan_hex = community.channels[0].id.to_hex();
        let new_key = [0xCDu8; 32];

        let parsed = owner_channel_rekey(&owner, &community, &me.public_key(), 1, &new_key);
        let outcome = apply_channel_rekey(&community, &parsed).unwrap();
        assert_eq!(outcome, RekeyOutcome::Applied { head_advanced: true });

        // Archive holds the new epoch-1 key, and the channel head advanced to it (epoch + key).
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 1).unwrap(), Some(new_key));
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.channels[0].epoch, crate::community::Epoch(1));
        assert_eq!(reloaded.channels[0].key.as_bytes(), &new_key);
        // The genesis epoch-0 key is RETAINED (cross-epoch history stays decryptable).
        assert!(crate::db::community::held_epoch_key(&cid, &chan_hex, 0).unwrap().is_some());
    }

    #[test]
    fn apply_channel_rekey_accepts_matching_continuity() {
        // The happy continuity path: I HOLD the prior (genesis epoch-0) key and the rekey cites a
        // commitment over it → the fork-detection check passes and the rekey applies.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        // owner_channel_rekey commits over the genesis epoch-0 key, which I hold (archived on save).
        let parsed = owner_channel_rekey(&owner, &community, &me.public_key(), 1, &[0x44u8; 32]);
        assert_eq!(
            apply_channel_rekey(&community, &parsed).unwrap(),
            RekeyOutcome::Applied { head_advanced: true },
            "a rekey whose prior-key commitment matches the held genesis key applies"
        );
    }

    #[test]
    fn advance_channel_epoch_archives_when_no_head_row() {
        // A rekey for a channel with no community_channels head row: archive the key, don't fabricate
        // a head. (Exercises advance_channel_epoch's channel-row-absent branch directly.)
        let (_tmp, _guard) = init_test_db();
        let cid = "f".repeat(64);
        let orphan_channel = "a".repeat(64);
        let advanced = crate::db::community::advance_channel_epoch(&cid, &orphan_channel, 2, &[0x77u8; 32]).unwrap();
        assert!(!advanced, "no head row → head not advanced");
        assert_eq!(crate::db::community::held_epoch_key(&cid, &orphan_channel, 2).unwrap(), Some([0x77u8; 32]), "key still archived");
    }

    #[tokio::test]
    async fn rotate_channel_publishes_recoverable_rekey_and_advances_own_head() {
        use crate::community::derive::{recipient_pseudonym, rekey_pseudonym};
        use crate::community::rekey::{open_rekey_blob, open_rekey_event, rekey_pairwise_secret};
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        become_local(&owner); // I am the owner (supreme authority to rotate)
        let community = saved_community_owned_by(&owner);
        let channel_id = community.channels[0].id;
        let member = Keys::generate(); // a stayer who must recover the new key
        let relay = MemoryRelay::new();

        let new_epoch = rotate_channel(&relay, &community, &channel_id, &[member.public_key()], community.server_root_key.as_bytes())
            .await
            .expect("rotate");
        assert_eq!(new_epoch, 1);

        // My own head advanced to the new epoch + a fresh key.
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.channels[0].epoch, crate::community::Epoch(1));

        // The published rekey is found at the SERVER-ROOT-derived address (no channel key needed) and
        // opens under the server root.
        let addr = rekey_pseudonym(
            &crate::community::ServerRootKey(*community.server_root_key.as_bytes()),
            &channel_id, crate::community::Epoch(1),
        )
        .to_hex();
        let found = relay
            .fetch(&Query { kinds: vec![event_kind::COMMUNITY_REKEY], z_tags: vec![addr], ..Default::default() }, &community.relays)
            .await
            .unwrap();
        assert_eq!(found.len(), 1, "rekey addressable by its server-root pseudonym");
        let parsed = open_rekey_event(&found[0], community.server_root_key.as_bytes()).unwrap();
        assert_eq!(parsed.rotator, owner.public_key());
        assert_eq!(parsed.new_epoch, crate::community::Epoch(1));
        assert_eq!(parsed.prev_epoch, crate::community::Epoch(0));
        assert_eq!(parsed.blobs.len(), 2, "the member + me (multi-device) each get a blob");

        // The member recovers a key, and it is EXACTLY the key my head advanced to (one source of truth).
        let secret = rekey_pairwise_secret(member.secret_key(), &parsed.rotator).unwrap();
        let loc = recipient_pseudonym(&secret, parsed.scope, parsed.new_epoch).to_hex();
        let mine = parsed.blobs.iter().find(|b| b.locator == loc).expect("member's blob present");
        let recovered = open_rekey_blob(member.secret_key(), &parsed.rotator, parsed.scope, parsed.new_epoch, mine).unwrap();
        assert_eq!(reloaded.channels[0].key.as_bytes(), &recovered, "member's recovered key == my advanced head key");
    }

    #[tokio::test]
    async fn rotate_channel_failed_publish_leaves_head_unadvanced() {
        // The publish-before-advance invariant: if the publish fails, my local head must NOT move to an
        // epoch no peer received (else I'd be stranded talking to no one).
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        become_local(&owner);
        let community = saved_community_owned_by(&owner);
        let member = Keys::generate();
        let err = rotate_channel(&FailingRelay, &community, &community.channels[0].id, &[member.public_key()], community.server_root_key.as_bytes()).await;
        assert!(err.is_err(), "a failed publish must propagate, not silently advance");
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.channels[0].epoch, crate::community::Epoch(0), "head stays put on publish failure");
    }

    /// Build a properly-chained run of channel rekeys (epoch 1..=n), each citing the prior epoch's key
    /// commitment (epoch 1 cites the genesis key), each carrying a blob for `recipient_pk`. Returns the
    /// events + the per-epoch keys. Does NOT touch the DB (so the recipient stays "behind" at epoch 0).
    fn build_rekey_chain(
        owner: &Keys, community: &Community, recipient_pk: &nostr_sdk::PublicKey, n: u64,
    ) -> (Vec<Event>, Vec<[u8; 32]>) {
        let chan = &community.channels[0];
        let scope = super::super::derive::RekeyScope::Channel(chan.id);
        let mut prev_key = *chan.key.as_bytes();
        let mut events = Vec::new();
        let mut keys = Vec::new();
        for e in 1..=n {
            let new_key = [e as u8; 32];
            let blob = super::super::rekey::build_rekey_blob(owner.secret_key(), recipient_pk, scope, crate::community::Epoch(e), &new_key).unwrap();
            let commit = super::super::rekey::epoch_key_commitment(crate::community::Epoch(e - 1), &prev_key);
            let ev = super::super::rekey::build_channel_rekey_event(
                &Keys::generate(), owner, community.server_root_key.as_bytes(), &chan.id,
                crate::community::Epoch(e), crate::community::Epoch(e - 1), &commit, &[blob],
            ).unwrap();
            events.push(ev);
            keys.push(new_key);
            prev_key = new_key;
        }
        (events, keys)
    }

    #[tokio::test]
    async fn catch_up_steps_over_a_missing_epoch() {
        // W1: a relay-incomplete gap (epoch 2 absent). Catch-up applies 1, steps over the missing 2
        // (logged), applies 3 → head reaches the latest present epoch; epoch-2's key stays a hole.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let channel_id = community.channels[0].id;
        let cid = community.id.to_hex();
        let chan_hex = channel_id.to_hex();

        let (events, keys) = build_rekey_chain(&owner, &community, &me.public_key(), 3);
        let relay = MemoryRelay::new();
        relay.inject(&events[0], &community.relays); // epoch 1
        relay.inject(&events[2], &community.relays); // epoch 3 — epoch 2 deliberately omitted
        let reached = catch_up_channel_rekeys(&relay, &community, &channel_id).await.unwrap();

        assert_eq!(reached, 3, "head reaches the latest present epoch, stepping over the gap");
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 1).unwrap(), Some(keys[0]));
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 2).unwrap(), None, "missing epoch is a hole");
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 3).unwrap(), Some(keys[2]));
    }

    #[tokio::test]
    async fn catch_up_recovers_a_rekey_under_a_prior_server_root() {
        // A channel rekey is addressed + encrypted under whatever server root was current at publish, and
        // the root ratchets on every base rotation. After the base rotates 0→1, an epoch-1 channel rekey
        // published under root-0 must STILL be found + opened (we hold root-0 in the archive) — else its
        // key is lost. Cross-root catch-up.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let root0_community = saved_community_owned_by(&owner);
        let cid = root0_community.id.to_hex();
        let channel_id = root0_community.channels[0].id;
        let chan_hex = channel_id.to_hex();
        let scope = super::super::derive::RekeyScope::Channel(channel_id);
        let genesis_key = *root0_community.channels[0].key.as_bytes();

        // Base rotation 0→1; the member now holds BOTH roots (epoch 0 from save, epoch 1 from advance).
        let root1 = [0x99u8; 32];
        crate::db::community::advance_server_root_epoch(&cid, 1, &root1).unwrap();
        let community = crate::db::community::load_community(&root0_community.id).unwrap().unwrap();
        assert_eq!(community.server_root_epoch, crate::community::Epoch(1));

        // Epoch-1 channel rekey under the PRIOR root (root-0); epoch-2 under the CURRENT root (root-1).
        let (k1, k2) = ([0x11u8; 32], [0x22u8; 32]);
        let blob1 = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &k1).unwrap();
        let commit0 = super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), &genesis_key);
        let ev1 = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, root0_community.server_root_key.as_bytes(), &channel_id,
            crate::community::Epoch(1), crate::community::Epoch(0), &commit0, &[blob1]).unwrap();
        let blob2 = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(2), &k2).unwrap();
        let commit1 = super::super::rekey::epoch_key_commitment(crate::community::Epoch(1), &k1);
        let ev2 = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, &root1, &channel_id,
            crate::community::Epoch(2), crate::community::Epoch(1), &commit1, &[blob2]).unwrap();

        let relay = MemoryRelay::new();
        relay.inject(&ev1, &community.relays);
        relay.inject(&ev2, &community.relays);

        let reached = catch_up_channel_rekeys(&relay, &community, &channel_id).await.unwrap();
        assert_eq!(reached, 2, "reached the latest channel epoch across the server-root rotation");
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 1).unwrap(), Some(k1),
            "epoch-1 key recovered from a rekey under the PRIOR server root");
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 2).unwrap(), Some(k2));
    }

    #[tokio::test]
    async fn catch_up_backfills_a_sub_head_gap() {
        // An EXISTING hole below the head (an earlier catch-up leapfrogged epoch 1). The forward window
        // never revisits sub-head epochs, so the backward gap-fill must re-fetch + apply it.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();
        let channel_id = community.channels[0].id;
        let chan_hex = channel_id.to_hex();
        let scope = super::super::derive::RekeyScope::Channel(channel_id);
        let genesis_key = *community.channels[0].key.as_bytes();

        // Pre-existing state: head already at epoch 2 (with its key), but epoch 1 is a HOLE.
        let k2 = [0x22u8; 32];
        crate::db::community::advance_channel_epoch(&cid, &chan_hex, 2, &k2).unwrap();
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 1).unwrap(), None, "epoch 1 starts as a hole");

        // Epoch-1's rekey is on relays (under the current root). The backward gap-fill should recover it.
        let k1 = [0x11u8; 32];
        let blob1 = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &k1).unwrap();
        let commit0 = super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), &genesis_key);
        let ev1 = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, community.server_root_key.as_bytes(), &channel_id,
            crate::community::Epoch(1), crate::community::Epoch(0), &commit0, &[blob1]).unwrap();
        let relay = MemoryRelay::new();
        relay.inject(&ev1, &community.relays);

        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();
        let reached = catch_up_channel_rekeys(&relay, &community, &channel_id).await.unwrap();
        assert_eq!(reached, 2, "head unchanged (gap-fill never regresses it)");
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 1).unwrap(), Some(k1),
            "the sub-head hole was backfilled");
    }

    #[tokio::test]
    async fn catch_up_walks_a_chain_of_rotations_to_the_latest() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me); // I'm a member, behind at epoch 0
        let community = saved_community_owned_by(&owner);
        let channel_id = community.channels[0].id;
        let cid = community.id.to_hex();
        let chan_hex = channel_id.to_hex();

        // 3 rotations happened while I was away; inject them onto the relay (unordered).
        let (events, keys) = build_rekey_chain(&owner, &community, &me.public_key(), 3);
        let relay = MemoryRelay::new();
        for ev in events.iter().rev() {
            relay.inject(ev, &community.relays);
        }

        let reached = catch_up_channel_rekeys(&relay, &community, &channel_id).await.unwrap();
        assert_eq!(reached, 3, "caught up to the latest epoch");
        // Head advanced to 3 with epoch-3's key; ALL intervening epoch keys retained.
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.channels[0].epoch, crate::community::Epoch(3));
        assert_eq!(reloaded.channels[0].key.as_bytes(), &keys[2]);
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, (i + 1) as u64).unwrap(), Some(*k));
        }
    }

    #[tokio::test]
    async fn catch_up_slides_across_the_window_boundary() {
        // Exercises the multi-round slide arithmetic: 70 contiguous rotations (all for me) exceed the
        // 64-wide window, so catch-up must fetch window 1 (1..64), advance, then slide to window 2 and
        // reach 70 — proving the window math, not just a single-window apply.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let channel_id = community.channels[0].id;
        let cid = community.id.to_hex();
        let chan_hex = channel_id.to_hex();

        let (events, keys) = build_rekey_chain(&owner, &community, &me.public_key(), 70);
        let relay = MemoryRelay::new();
        for ev in &events {
            relay.inject(ev, &community.relays);
        }
        let reached = catch_up_channel_rekeys(&relay, &community, &channel_id).await.unwrap();
        assert_eq!(reached, 70, "slid past the 64-epoch window boundary to the latest");
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 70).unwrap(), Some(keys[69]));
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 64).unwrap(), Some(keys[63]), "window-1 keys retained too");
    }

    // --- catch_up_server_root (#4d) ---

    /// A properly-chained run of base rekeys (epoch 1..=n), each enveloped under the PRIOR root and
    /// citing it, each carrying a ServerRoot blob for `recipient_pk`. Returns the events + per-epoch
    /// roots. Does NOT touch the DB (the recipient stays "behind" at base epoch 0).
    fn build_base_rekey_chain(
        owner: &Keys, community: &Community, recipient_pk: &nostr_sdk::PublicKey, n: u64,
    ) -> (Vec<Event>, Vec<[u8; 32]>) {
        let mut prior_root = *community.server_root_key.as_bytes();
        let mut events = Vec::new();
        let mut roots = Vec::new();
        for e in 1..=n {
            let new_root = [(e % 256) as u8; 32];
            let blob = super::super::rekey::build_rekey_blob(
                owner.secret_key(), recipient_pk, super::super::derive::RekeyScope::ServerRoot, crate::community::Epoch(e), &new_root,
            )
            .unwrap();
            let commit = super::super::rekey::epoch_key_commitment(crate::community::Epoch(e - 1), &prior_root);
            events.push(super::super::rekey::build_server_root_rekey_event(
                &Keys::generate(), owner, &prior_root, &community.id,
                crate::community::Epoch(e), crate::community::Epoch(e - 1), &commit, &[blob],
            ).unwrap());
            roots.push(new_root);
            prior_root = new_root;
        }
        (events, roots)
    }

    #[tokio::test]
    async fn catch_up_server_root_walks_a_chain_of_base_rotations() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();

        let (events, roots) = build_base_rekey_chain(&owner, &community, &me.public_key(), 3);
        let relay = MemoryRelay::new();
        for ev in events.iter().rev() {
            relay.inject(ev, &community.relays);
        }
        let reached = catch_up_server_root(&relay, &community).await.unwrap();
        assert_eq!(reached.epoch, 3, "walked the base chain to the latest epoch");
        assert!(!reached.removed, "a normal catch-up is not a removal");
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_epoch, crate::community::Epoch(3));
        assert_eq!(reloaded.server_root_key.as_bytes(), &roots[2], "base head is the latest root");
        // All intervening roots retained (read old control/base history).
        for (i, r) in roots.iter().enumerate() {
            assert_eq!(crate::db::community::held_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, (i + 1) as u64).unwrap(), Some(*r));
        }
    }

    #[tokio::test]
    async fn catch_up_recovers_from_a_split_base_rotation_second_chunk() {
        // SPLIT: a base rotation at epoch 1 is published as TWO chunk events at the SAME address; MY
        // blob is in the SECOND chunk. The walk must try both and recover from chunk 2 — the old
        // first-match logic would have hit chunk 1 (no blob for me), read it as removal, and stranded me.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let genesis = *community.server_root_key.as_bytes();
        let new_root = [0x5Au8; 32];
        let scope = super::super::derive::RekeyScope::ServerRoot;
        let commit = super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), &genesis);
        let mk = |recipient: &nostr_sdk::PublicKey| {
            let blob = super::super::rekey::build_rekey_blob(owner.secret_key(), recipient, scope, crate::community::Epoch(1), &new_root).unwrap();
            super::super::rekey::build_server_root_rekey_event(
                &Keys::generate(), &owner, &genesis, &community.id,
                crate::community::Epoch(1), crate::community::Epoch(0), &commit, &[blob],
            ).unwrap()
        };
        let relay = MemoryRelay::new();
        relay.inject(&mk(&Keys::generate().public_key()), &community.relays); // chunk 1: NOT for me
        relay.inject(&mk(&me.public_key()), &community.relays); // chunk 2: my blob

        let reached = catch_up_server_root(&relay, &community).await.unwrap();
        assert_eq!(reached.epoch, 1, "recovered the split rotation via the second chunk");
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_key.as_bytes(), &new_root, "recovered the new root from chunk 2");
    }

    #[tokio::test]
    async fn catch_up_converges_concurrent_refoundings_on_the_lowest_root() {
        // B2: two BAN-holders re-found at the SAME epoch, each delivering a DIFFERENT new root to me. Every
        // member must pick the SAME canonical root or the community forks irrecoverably. The walk converges
        // on the LOWEST new-root bytes — deterministic for everyone — regardless of which arrived first.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let genesis = *community.server_root_key.as_bytes();
        let scope = super::super::derive::RekeyScope::ServerRoot;
        let commit = super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), &genesis);
        let root_lo = [0x10u8; 32];
        let root_hi = [0xF0u8; 32]; // root_lo < root_hi bytewise
        let mk = |root: &[u8; 32]| {
            let blob = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), root).unwrap();
            super::super::rekey::build_server_root_rekey_event(
                &Keys::generate(), &owner, &genesis, &community.id,
                crate::community::Epoch(1), crate::community::Epoch(0), &commit, &[blob],
            ).unwrap()
        };
        let relay = MemoryRelay::new();
        // Inject the HIGHER root FIRST — "first-arrived" logic would pick the wrong one without the tiebreak.
        relay.inject(&mk(&root_hi), &community.relays);
        relay.inject(&mk(&root_lo), &community.relays);

        let reached = catch_up_server_root(&relay, &community).await.unwrap();
        assert_eq!(reached.epoch, 1, "advanced one epoch");
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_key.as_bytes(), &root_lo, "converged on the LOWEST root, not the first-arrived");
    }

    #[tokio::test]
    async fn rotate_retry_reuses_the_archived_root_no_same_epoch_fork() {
        // FORK-SAFETY crux: a rotation whose publish fails archives the new root, and a RETRY reuses that
        // SAME root (never mints a fresh one for the same epoch — which would split recipients onto
        // incompatible keys). Fail the base rekey publish, capture the archived root, recover the relay,
        // retry, and assert the root is identical.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        become_local(&owner);
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();
        let relay = RekeyFailingRelay::new(); // base rekey (3303) publish fails
        let member = Keys::generate();

        assert!(rotate_server_root(&relay, &community, &[member.public_key()]).await.is_err(), "the rekey publish fails");
        let k1 = crate::db::community::held_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, 1).unwrap()
            .expect("the new root is archived before publishing (fork-safety)");
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_epoch, crate::community::Epoch(0), "head not advanced on a failed publish");

        relay.allow_rekey();
        rotate_server_root(&relay, &reloaded, &[member.public_key()]).await.unwrap();
        let k2 = crate::db::community::held_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, 1).unwrap().unwrap();
        assert_eq!(k1, k2, "the retry REUSES the archived root — no second root for epoch 1, no fork");
        let after = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(after.server_root_epoch, crate::community::Epoch(1), "the retry completed the rotation");
        assert_eq!(after.server_root_key.as_bytes(), &k1, "the committed root is the one minted on attempt 1");
    }

    #[tokio::test]
    async fn rotate_server_root_splits_a_large_recipient_set_into_multiple_events() {
        // A recipient set past MAX_REKEY_BLOBS publishes as MULTIPLE chunk events at one address.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        become_local(&owner);
        let community = saved_community_owned_by(&owner);
        let genesis = *community.server_root_key.as_bytes();
        let relay = MemoryRelay::new();
        // MAX_REKEY_BLOBS recipients + the owner self-blob = MAX+1 blobs → exactly 2 chunks.
        let recipients: Vec<_> = (0..super::super::rekey::MAX_REKEY_BLOBS).map(|_| Keys::generate().public_key()).collect();
        rotate_server_root(&relay, &community, &recipients).await.unwrap();
        let addr = super::super::derive::base_rekey_pseudonym(&super::super::ServerRootKey(genesis), &community.id, crate::community::Epoch(1)).to_hex();
        let evs = relay
            .fetch(&Query { kinds: vec![event_kind::COMMUNITY_REKEY], z_tags: vec![addr], ..Default::default() }, &community.relays)
            .await
            .unwrap();
        assert_eq!(evs.len(), 2, "a >MAX_REKEY_BLOBS rotation splits into 2 events at one address");
    }

    #[tokio::test]
    async fn catch_up_server_root_is_a_noop_with_no_rotations() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let relay = MemoryRelay::new();
        assert_eq!(catch_up_server_root(&relay, &community).await.unwrap().epoch, 0, "no base rotations → stays at 0");
    }

    #[tokio::test]
    async fn concurrent_refounders_converge_to_the_lowest_root() {
        // Two BAN-holders re-found at the SAME epoch with DIFFERENT roots → each ORIGINATOR ends on its own
        // root (the forward walk only tiebreaks at head+1). The current-head convergence reconciles them:
        // whoever holds the HIGHER root adopts the LOWER (deterministic winner). This is the exact case the
        // live dual-admin race broke — the bystander-only B2 test never covered the originators self-healing.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me); // a member sitting on the LOSING (higher) root after my own concurrent re-founding
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();
        let genesis_root = *community.server_root_key.as_bytes();
        let scope = super::super::derive::RekeyScope::ServerRoot;

        // The OTHER originator's epoch-1 base rekey (root_lo, the winner) — carries a blob for ME, addressed
        // under the genesis (prior) root. Owner-authored, so it's authorized (supreme) regardless of roster.
        let root_lo = [0x10u8; 32];
        let root_hi = [0x99u8; 32]; // my own losing fork's root
        let commit0 = super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), &genesis_root);
        let blob_lo = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &root_lo).unwrap();
        let ev_lo = super::super::rekey::build_server_root_rekey_event(
            &Keys::generate(), &owner, &genesis_root, &community.id, crate::community::Epoch(1), crate::community::Epoch(0), &commit0, &[blob_lo]).unwrap();

        let relay = MemoryRelay::new();
        relay.inject(&ev_lo, &community.relays);

        // I'm currently on the HIGHER root at epoch 1 (my own losing fork).
        crate::db::community::advance_server_root_epoch(&cid, 1, &root_hi).unwrap();
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(community.server_root_key.as_bytes(), &root_hi, "start on the higher root");

        let out = catch_up_server_root(&relay, &community).await.unwrap();
        assert_eq!(out.epoch, 1, "converged in place at the same epoch (not advanced)");
        assert!(!out.removed);
        let after = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(after.server_root_key.as_bytes(), &root_lo, "originator converged to the lowest authorized root");

        // Idempotent: a second pass holding the winner stays put (no flip back to the higher root).
        let _ = catch_up_server_root(&relay, &after).await.unwrap();
        assert_eq!(crate::db::community::load_community(&community.id).unwrap().unwrap().server_root_key.as_bytes(), &root_lo, "no flip-flop");
    }

    #[tokio::test]
    async fn banned_rotators_rekey_is_not_a_convergence_candidate() {
        // §6 banlist precedence on the rekey plane: an admin who holds a (withheld-revoke) BAN grant
        // but sits on the SYNCED banlist must not be honored as a rotator — not by apply, not by the
        // forward walk, not by the heal. Here the banned admin's re-founding delivers a byte-LOWER
        // root than the one I hold; without the banlist gate the heal would adopt it.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        let banned_admin = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();
        let genesis_root = *community.server_root_key.as_bytes();
        let scope = super::super::derive::RekeyScope::ServerRoot;

        // The attacker still ranks in the roster (their grant-revoke is "withheld")...
        let role_id = "e".repeat(64);
        let roster = crate::community::roles::CommunityRoles {
            roles: vec![crate::community::roles::Role::admin(role_id.clone())],
            grants: vec![crate::community::roles::MemberGrant { member: banned_admin.public_key().to_hex(), role_ids: vec![role_id] }],
        };
        crate::db::community::set_community_roles(&cid, &roster, 1).unwrap();
        // ...but the banlist naming them DID sync. Banlist must dominate.
        crate::db::community::set_community_banlist(&cid, &[banned_admin.public_key().to_hex()], 2).unwrap();

        // Banned admin's epoch-1 re-founding with a ground-low root, blob addressed to me.
        let root_evil = [0x01u8; 32];
        let commit0 = super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), &genesis_root);
        let blob = super::super::rekey::build_rekey_blob(banned_admin.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &root_evil).unwrap();
        let ev = super::super::rekey::build_server_root_rekey_event(
            &Keys::generate(), &banned_admin, &genesis_root, &community.id, crate::community::Epoch(1), crate::community::Epoch(0), &commit0, &[blob]).unwrap();
        let relay = MemoryRelay::new();
        relay.inject(&ev, &community.relays);

        // Forward walk: the banned rotation is the ONLY epoch-1 candidate → not adopted, not a
        // removal signal (a banned admin can't trick members into self-erasing either).
        let out = catch_up_server_root(&relay, &community).await.unwrap();
        assert_eq!(out.epoch, 0, "banned rotator's re-founding must not advance the base");
        assert!(!out.removed, "banned rotator's exclusion must not read as an authorized removal");
        let after = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(after.server_root_key.as_bytes(), &genesis_root, "root unchanged");

        // Direct apply refuses too.
        let parsed = super::super::rekey::open_rekey_event(&ev, &genesis_root).unwrap();
        assert!(apply_server_root_rekey(&community, &parsed).is_err(), "apply must refuse a banned rotator");
    }

    #[tokio::test]
    async fn heal_abandons_a_deauthorized_root_for_the_authorized_higher_sibling() {
        // B1 (rekey-race fork): I adopted a since-BANNED admin's ground-low epoch-1 root before the
        // banlist reached me. Once the banlist syncs, the heal must abandon their root and climb UP
        // to the owner's legitimate (byte-higher) sibling — authority dominates the down-only rule.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        let banned_admin = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();
        let genesis_root = *community.server_root_key.as_bytes();
        let scope = super::super::derive::RekeyScope::ServerRoot;
        let commit0 = super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), &genesis_root);

        // Both epoch-1 siblings on the wire, addressed under the shared genesis root:
        // the attacker's (ground-low) and the owner's (higher).
        let root_evil = [0x01u8; 32];
        let root_owner = [0x77u8; 32];
        let blob_evil = super::super::rekey::build_rekey_blob(banned_admin.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &root_evil).unwrap();
        let ev_evil = super::super::rekey::build_server_root_rekey_event(
            &Keys::generate(), &banned_admin, &genesis_root, &community.id, crate::community::Epoch(1), crate::community::Epoch(0), &commit0, &[blob_evil]).unwrap();
        let blob_owner = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &root_owner).unwrap();
        let ev_owner = super::super::rekey::build_server_root_rekey_event(
            &Keys::generate(), &owner, &genesis_root, &community.id, crate::community::Epoch(1), crate::community::Epoch(0), &commit0, &[blob_owner]).unwrap();
        let relay = MemoryRelay::new();
        relay.inject(&ev_evil, &community.relays);
        relay.inject(&ev_owner, &community.relays);

        // I already adopted the attacker's root at epoch 1 (the race), and the ban has now synced.
        crate::db::community::advance_server_root_epoch(&cid, 1, &root_evil).unwrap();
        crate::db::community::set_community_banlist(&cid, &[banned_admin.public_key().to_hex()], 2).unwrap();
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(community.server_root_key.as_bytes(), &root_evil, "start partitioned on the attacker's root");

        let out = catch_up_server_root(&relay, &community).await.unwrap();
        assert_eq!(out.epoch, 1);
        assert!(!out.removed);
        let after = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(after.server_root_key.as_bytes(), &root_owner,
            "heal must abandon the deauthorized root and adopt the owner's higher sibling");

        // Stable: re-running keeps the owner's root (the attacker's lower root never wins again).
        let _ = catch_up_server_root(&relay, &after).await.unwrap();
        assert_eq!(crate::db::community::load_community(&community.id).unwrap().unwrap().server_root_key.as_bytes(), &root_owner, "no flap back to the banned root");
    }

    #[tokio::test]
    async fn concurrent_channel_rekeyers_converge_to_the_lowest_key() {
        // Two MANAGE_CHANNELS holders rotate the SAME channel at the SAME epoch with DIFFERENT keys —
        // a true fork inside the propagation window. Both rekeys land at the same address under the (already
        // converged) server root, so relay order would otherwise decide last-write-wins. The current-head
        // heal must pick the LOWEST delivered key deterministically — every member computes the same winner.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me); // a member sitting on the LOSING (higher) channel key after my own fork
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();
        let channel_id = community.channels[0].id;
        let chan_hex = channel_id.to_hex();
        let scope = super::super::derive::RekeyScope::Channel(channel_id);
        let genesis_key = *community.channels[0].key.as_bytes();
        let root = *community.server_root_key.as_bytes();
        let commit0 = super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), &genesis_key);

        // Two owner-authorized epoch-1 channel rekeys, each carrying a blob for ME, both citing genesis.
        let key_lo = [0x10u8; 32];
        let key_hi = [0x99u8; 32];
        let blob_lo = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &key_lo).unwrap();
        let blob_hi = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &key_hi).unwrap();
        let ev_lo = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, &root, &channel_id, crate::community::Epoch(1), crate::community::Epoch(0), &commit0, &[blob_lo]).unwrap();
        let ev_hi = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, &root, &channel_id, crate::community::Epoch(1), crate::community::Epoch(0), &commit0, &[blob_hi]).unwrap();

        let relay = MemoryRelay::new();
        relay.inject(&ev_hi, &community.relays); // inject the HIGHER first: naive relay-order would pick it
        relay.inject(&ev_lo, &community.relays);

        // The two forked channel rekeys are addressed under the PRIOR
        // (shared) root they cite, not the current one. Advance the SERVER root so genesis becomes a prior
        // root — the heal must search EVERY held root to find them. A current-root-only fetch
        // missed both and never converged (the channel forked live while the base healed).
        crate::db::community::advance_server_root_epoch(&cid, 1, &[0x42u8; 32]).unwrap();
        // I'm currently on the HIGHER key at epoch 1 (my own losing fork).
        crate::db::community::advance_channel_epoch(&cid, &chan_hex, 1, &key_hi).unwrap();
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();

        let reached = catch_up_channel_rekeys(&relay, &community, &channel_id).await.unwrap();
        assert_eq!(reached, 1, "converged in place at the same channel epoch");
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 1).unwrap(), Some(key_lo),
            "adopted the lowest delivered key regardless of relay order");

        // Idempotent: re-running holding the winner stays put (no flip back to the higher key).
        let after = crate::db::community::load_community(&community.id).unwrap().unwrap();
        let _ = catch_up_channel_rekeys(&relay, &after, &channel_id).await.unwrap();
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 1).unwrap(), Some(key_lo), "no flip-flop");
    }

    #[tokio::test]
    async fn concurrent_channel_rekeyers_converge_when_i_authored_the_losing_fork() {
        // FAITHFUL LIVE REPLICA of the dual-admin ban (the case the simpler test missed): TWO DISTINCT
        // authorized rotators (owner + a granted admin), and the LOCAL user IS one of them — I authored the
        // HIGHER (losing) channel rekey myself, the owner authored the lower. Both sit under the PRIOR shared
        // root, both deliver a blob to me. The heal must still converge ME down to the owner's lower key.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate(); // I am the ADMIN rotator (not a bystander) — mirrors the agent in the live test
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();
        let channel_id = community.channels[0].id;
        let chan_hex = channel_id.to_hex();
        let scope = super::super::derive::RekeyScope::Channel(channel_id);
        let genesis_key = *community.channels[0].key.as_bytes();
        let root = *community.server_root_key.as_bytes();
        let commit0 = super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), &genesis_key);

        // Grant ME (the admin) a role carrying MANAGE_CHANNELS, so MY OWN rekey is an authorized candidate
        // (owner is supreme regardless). Without this the heal would trivially pick the owner's; with it,
        // BOTH siblings are authorized — exactly the live ambiguity that must resolve to the lowest key.
        let role_id = "d".repeat(64);
        let roster = crate::community::roles::CommunityRoles {
            roles: vec![crate::community::roles::Role::admin(role_id.clone())],
            grants: vec![crate::community::roles::MemberGrant { member: me.public_key().to_hex(), role_ids: vec![role_id] }],
        };
        crate::db::community::set_community_roles(&cid, &roster, 1).unwrap();

        let key_lo = [0x10u8; 32]; // owner's (the winner)
        let key_hi = [0x99u8; 32]; // MINE (the losing fork I authored + currently hold)
        // Owner's rekey: rotator = owner, blob for ME.
        let blob_lo = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &key_lo).unwrap();
        let ev_lo = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, &root, &channel_id, crate::community::Epoch(1), crate::community::Epoch(0), &commit0, &[blob_lo]).unwrap();
        // MY rekey: rotator = me (the admin), blob for ME (self-delivered, as rotate_channel always adds self).
        let blob_hi = super::super::rekey::build_rekey_blob(me.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &key_hi).unwrap();
        let ev_hi = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &me, &root, &channel_id, crate::community::Epoch(1), crate::community::Epoch(0), &commit0, &[blob_hi]).unwrap();

        let relay = MemoryRelay::new();
        relay.inject(&ev_hi, &community.relays);
        relay.inject(&ev_lo, &community.relays);

        // The rekeys are under genesis (prior) root; advance the SERVER root so genesis is no longer current.
        crate::db::community::advance_server_root_epoch(&cid, 1, &[0x42u8; 32]).unwrap();
        // I currently hold MY OWN (higher) key at channel epoch 1.
        crate::db::community::advance_channel_epoch(&cid, &chan_hex, 1, &key_hi).unwrap();
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();

        let _ = catch_up_channel_rekeys(&relay, &community, &channel_id).await.unwrap();
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 1).unwrap(), Some(key_lo),
            "I authored the losing fork but must converge DOWN to the owner's lower key");
    }

    #[tokio::test]
    async fn reorg_through_a_fork_heals_the_forked_past_epoch() {
        // I sit on the LOSING sibling at a PAST channel epoch (epoch 1) and then reorg forward when an
        // authorized epoch-2 rekey continues from the WINNING epoch-1 key. Advancing the head alone leaves
        // epoch 1 on the wrong key (its messages unreadable). catch_up must re-converge the forked PAST epoch
        // to the lowest sibling — not just the head.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();
        let channel_id = community.channels[0].id;
        let chan_hex = channel_id.to_hex();
        let scope = super::super::derive::RekeyScope::Channel(channel_id);
        let genesis_key = *community.channels[0].key.as_bytes();
        let root = *community.server_root_key.as_bytes();
        let commit0 = super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), &genesis_key);

        // Two owner-authorized epoch-1 siblings (the fork), both delivering a blob to me.
        let key_lo1 = [0x10u8; 32]; // winner at epoch 1
        let key_hi1 = [0x99u8; 32]; // loser at epoch 1 (what I currently hold)
        let key_e2 = [0x20u8; 32]; // epoch 2, continuing from the WINNER's key_lo1
        let blob_lo1 = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &key_lo1).unwrap();
        let blob_hi1 = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &key_hi1).unwrap();
        let ev_lo1 = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, &root, &channel_id, crate::community::Epoch(1), crate::community::Epoch(0), &commit0, &[blob_lo1]).unwrap();
        let ev_hi1 = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, &root, &channel_id, crate::community::Epoch(1), crate::community::Epoch(0), &commit0, &[blob_hi1]).unwrap();
        // Epoch 2 cites the WINNER's epoch-1 key — applying it while I hold key_hi1 is the reorg.
        let commit1_win = super::super::rekey::epoch_key_commitment(crate::community::Epoch(1), &key_lo1);
        let blob_e2 = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(2), &key_e2).unwrap();
        let ev_e2 = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, &root, &channel_id, crate::community::Epoch(2), crate::community::Epoch(1), &commit1_win, &[blob_e2]).unwrap();

        let relay = MemoryRelay::new();
        relay.inject(&ev_lo1, &community.relays);
        relay.inject(&ev_hi1, &community.relays);
        relay.inject(&ev_e2, &community.relays);

        // All three rekeys are under the genesis (now-prior) server root.
        crate::db::community::advance_server_root_epoch(&cid, 1, &[0x42u8; 32]).unwrap();
        // I'm sitting on the LOSING epoch-1 key.
        crate::db::community::advance_channel_epoch(&cid, &chan_hex, 1, &key_hi1).unwrap();
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();

        let reached = catch_up_channel_rekeys(&relay, &community, &channel_id).await.unwrap();
        assert_eq!(reached, 2, "reorged forward to the head epoch");
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 2).unwrap(), Some(key_e2), "head epoch adopted");
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 1).unwrap(), Some(key_lo1),
            "the FORKED past epoch re-converged to the lowest sibling (its messages become readable)");
    }

    #[tokio::test]
    async fn window_heal_converges_an_already_reorged_past_fork() {
        // A member sitting at head epoch 2 holding the LOSING sibling at epoch 1, with NO new rekey to apply
        // this sync (so the in-sync forked-epoch set stays empty). The recent-window heal must STILL
        // re-converge epoch 1 to the lowest sibling — otherwise its messages are stranded forever. Distinct
        // from `reorg_through_a_fork_*` (which reorgs in-sync).
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();
        let channel_id = community.channels[0].id;
        let chan_hex = channel_id.to_hex();
        let scope = super::super::derive::RekeyScope::Channel(channel_id);
        let genesis_key = *community.channels[0].key.as_bytes();
        let root = *community.server_root_key.as_bytes();
        let commit0 = super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), &genesis_key);

        let key_lo1 = [0x10u8; 32]; // winner at epoch 1 (on the wire, authorized, blob for me)
        let key_hi1 = [0x99u8; 32]; // loser at epoch 1 (what I currently hold)
        let key_e2 = [0x20u8; 32]; // my head at epoch 2 (already reorged here under the old build)
        let blob_lo1 = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &key_lo1).unwrap();
        let blob_hi1 = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &key_hi1).unwrap();
        let ev_lo1 = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, &root, &channel_id, crate::community::Epoch(1), crate::community::Epoch(0), &commit0, &[blob_lo1]).unwrap();
        let ev_hi1 = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, &root, &channel_id, crate::community::Epoch(1), crate::community::Epoch(0), &commit0, &[blob_hi1]).unwrap();

        let relay = MemoryRelay::new();
        relay.inject(&ev_lo1, &community.relays);
        relay.inject(&ev_hi1, &community.relays);
        // NOTE: no epoch-2 rekey on the relay — nothing for the forward walk to apply, so the heal is the
        // ONLY thing that can fix epoch 1.

        crate::db::community::advance_server_root_epoch(&cid, 1, &[0x42u8; 32]).unwrap();
        // Simulate the prior-build reorg: I hold the LOSING epoch-1 key and have already advanced to epoch 2.
        crate::db::community::advance_channel_epoch(&cid, &chan_hex, 1, &key_hi1).unwrap();
        crate::db::community::advance_channel_epoch(&cid, &chan_hex, 2, &key_e2).unwrap();
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();

        let reached = catch_up_channel_rekeys(&relay, &community, &channel_id).await.unwrap();
        assert_eq!(reached, 2, "head unchanged (no new rekey to apply)");
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 2).unwrap(), Some(key_e2), "head epoch untouched");
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 1).unwrap(), Some(key_lo1),
            "the already-forked past epoch re-converged to the lowest sibling via the window heal (no in-sync reorg)");
    }

    #[tokio::test]
    async fn channel_heal_cannot_converge_to_a_key_i_was_not_given() {
        // The winning (lower) fork's channel rekey carries NO blob for me
        // (the other re-founder's retain set excluded me — e.g. it kept the just-banned victim and dropped
        // me in the concurrent-ban window). I literally cannot DECRYPT that key, so the heal can't adopt it
        // and I stay stranded on my own higher key. This proves the live bug is RETAIN-SET incompleteness in
        // concurrent re-founding, NOT the heal logic (which the two tests above prove correct). The fix must
        // guarantee each re-founder's rekey reaches the OTHER re-founder.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();
        let channel_id = community.channels[0].id;
        let chan_hex = channel_id.to_hex();
        let scope = super::super::derive::RekeyScope::Channel(channel_id);
        let genesis_key = *community.channels[0].key.as_bytes();
        let root = *community.server_root_key.as_bytes();
        let commit0 = super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), &genesis_key);

        let key_lo = [0x10u8; 32]; // owner's (lower) — but its rekey DOES NOT include me
        let key_hi = [0x99u8; 32]; // mine (higher) — the one I currently hold
        // Owner's lower rekey delivers ONLY to a third party (the banned victim's seat), NOT to me.
        let other = Keys::generate();
        let blob_lo = super::super::rekey::build_rekey_blob(owner.secret_key(), &other.public_key(), scope, crate::community::Epoch(1), &key_lo).unwrap();
        let ev_lo = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, &root, &channel_id, crate::community::Epoch(1), crate::community::Epoch(0), &commit0, &[blob_lo]).unwrap();
        // My higher rekey delivers to me.
        let blob_hi = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &key_hi).unwrap();
        let ev_hi = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, &root, &channel_id, crate::community::Epoch(1), crate::community::Epoch(0), &commit0, &[blob_hi]).unwrap();

        let relay = MemoryRelay::new();
        relay.inject(&ev_lo, &community.relays);
        relay.inject(&ev_hi, &community.relays);
        crate::db::community::advance_server_root_epoch(&cid, 1, &[0x42u8; 32]).unwrap();
        crate::db::community::advance_channel_epoch(&cid, &chan_hex, 1, &key_hi).unwrap();
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();

        let _ = catch_up_channel_rekeys(&relay, &community, &channel_id).await.unwrap();
        // Excluded from the winning rekey: I can't decrypt the lower key, so I keep my own and cannot converge.
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 1).unwrap(), Some(key_hi),
            "excluded from the winning rekey ⇒ cannot converge");
    }

    #[tokio::test]
    async fn refounding_channel_rekey_is_sealed_under_the_prior_root() {
        // #262 fix: a channel rekey accompanying a re-founding must be ENVELOPED + ADDRESSED under the PRIOR
        // (shared) root, NOT the re-founder's new one — so a base-fork loser (who dropped its own new root)
        // can still open it. This pins the write side: rotate_channel seals under the passed envelope_root,
        // and the event opens under that root and NOT under the community's current/new root.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        become_local(&owner); // owner is supreme → authorized to rotate
        let community = saved_community_owned_by(&owner);
        let channel_id = community.channels[0].id;
        let prior_root = [0x11u8; 32]; // the shared pre-rotation root (≠ the community's current root)

        let relay = MemoryRelay::new();
        rotate_channel(&relay, &community, &channel_id, &[owner.public_key()], &prior_root).await.unwrap();

        // Addressed at the PRIOR-root pseudonym...
        let z = super::super::derive::rekey_pseudonym(&crate::community::ServerRootKey(prior_root), &channel_id, crate::community::Epoch(1)).to_hex();
        let q = Query { kinds: vec![event_kind::COMMUNITY_REKEY], z_tags: vec![z], ..Default::default() };
        let evs = relay.fetch(&q, &community.relays).await.unwrap();
        assert_eq!(evs.len(), 1, "channel rekey is addressed at the PRIOR-root pseudonym");
        // ...and opens ONLY under the prior root, NOT the community's current (new) root.
        assert!(super::super::rekey::open_rekey_event(&evs[0], &prior_root).is_ok(),
            "opens under the prior (shared) root every retained member still holds");
        assert!(super::super::rekey::open_rekey_event(&evs[0], community.server_root_key.as_bytes()).is_err(),
            "does NOT open under the current/new root (which a base-fork loser would have dropped)");
    }

    #[tokio::test]
    async fn apply_channel_rekey_converges_past_a_divergent_prior_epoch() {
        // FORK-CONVERGENCE: I hold epoch-1 = my LOSING fork key. An AUTHORIZED rekey
        // to epoch 2 cites a DIFFERENT epoch-1 key (the winner's, which I never held) and delivers epoch-2 to
        // ME. The relaxed continuity check must ADOPT it (converge forward onto the authorized chain), not
        // reject it as a "foreign chain" and strand me on the dead fork forever.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();
        let channel_id = community.channels[0].id;
        let chan_hex = channel_id.to_hex();
        let scope = super::super::derive::RekeyScope::Channel(channel_id);
        let root = *community.server_root_key.as_bytes();

        // I'm on my LOSING fork at epoch 1.
        let my_fork_key = [0xAAu8; 32];
        crate::db::community::advance_channel_epoch(&cid, &chan_hex, 1, &my_fork_key).unwrap();

        // Owner's epoch-2 rekey continues from the WINNER's epoch-1 (a key I never held) + delivers to me.
        let winner_epoch1 = [0xBBu8; 32];
        let new_key = [0x22u8; 32];
        let commit = super::super::rekey::epoch_key_commitment(crate::community::Epoch(1), &winner_epoch1);
        let blob = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(2), &new_key).unwrap();
        let ev = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, &root, &channel_id, crate::community::Epoch(2), crate::community::Epoch(1), &commit, &[blob]).unwrap();
        let parsed = super::super::rekey::open_rekey_event(&ev, &root).unwrap();

        let outcome = apply_channel_rekey(&community, &parsed).unwrap();
        assert!(matches!(outcome, RekeyOutcome::Applied { head_advanced: true }),
            "must converge forward past the divergent prior epoch, got {outcome:?}");
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 2).unwrap(), Some(new_key),
            "adopted the winner's epoch-2 key");
    }

    #[tokio::test]
    async fn catch_up_server_root_stops_when_removed_from_base() {
        // Recipient of base epoch 1 but NOT epoch 2 (removed from the base). The walk applies 1, opens
        // the epoch-2 envelope (I hold root_1) but finds no blob → NotARecipient → stops at 1.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let scope = super::super::derive::RekeyScope::ServerRoot;
        let relay = MemoryRelay::new();

        // Epoch 1 → me (cites genesis).
        let root1 = [0x11u8; 32];
        let b1 = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &root1).unwrap();
        let e1 = super::super::rekey::build_server_root_rekey_event(
            &Keys::generate(), &owner, community.server_root_key.as_bytes(), &community.id,
            crate::community::Epoch(1), crate::community::Epoch(0),
            &super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), community.server_root_key.as_bytes()), &[b1],
        ).unwrap();
        // Epoch 2 → someone else (I'm removed), enveloped under root_1, cites root_1.
        let other = Keys::generate();
        let b2 = super::super::rekey::build_rekey_blob(owner.secret_key(), &other.public_key(), scope, crate::community::Epoch(2), &[0x22u8; 32]).unwrap();
        let e2 = super::super::rekey::build_server_root_rekey_event(
            &Keys::generate(), &owner, &root1, &community.id,
            crate::community::Epoch(2), crate::community::Epoch(1),
            &super::super::rekey::epoch_key_commitment(crate::community::Epoch(1), &root1), &[b2],
        ).unwrap();
        relay.inject(&e1, &community.relays);
        relay.inject(&e2, &community.relays);

        let reached = catch_up_server_root(&relay, &community).await.unwrap();
        assert_eq!(reached.epoch, 1, "stops at the last base epoch I was a recipient of");
        assert!(reached.removed, "excluded by an AUTHORIZED (owner) base rotation → flagged removed so the caller erases");
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_epoch, crate::community::Epoch(1));
    }

    #[tokio::test]
    async fn catch_up_is_a_noop_with_no_rotations() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let relay = MemoryRelay::new(); // empty: no rekeys published
        let reached = catch_up_channel_rekeys(&relay, &community, &community.channels[0].id).await.unwrap();
        assert_eq!(reached, 0, "no rotations → stays at the held epoch");
    }

    #[tokio::test]
    async fn catch_up_stops_when_removed_midway() {
        // I'm a recipient of epoch 1 but NOT epoch 2 (removed). Catch-up applies epoch 1, finds no blob
        // for epoch 2 (NotARecipient), and stops — head at 1, not dragged forward to a key I lack.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let channel_id = community.channels[0].id;
        let chan = &community.channels[0];
        let scope = super::super::derive::RekeyScope::Channel(channel_id);
        let relay = MemoryRelay::new();

        // Epoch 1: blob for me (cites genesis).
        let k1 = [0x11u8; 32];
        let b1 = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &k1).unwrap();
        let e1 = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, community.server_root_key.as_bytes(), &channel_id,
            crate::community::Epoch(1), crate::community::Epoch(0),
            &super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), chan.key.as_bytes()), &[b1],
        ).unwrap();
        // Epoch 2: blob for SOMEONE ELSE (I was removed) — cites k1.
        let other = Keys::generate();
        let b2 = super::super::rekey::build_rekey_blob(owner.secret_key(), &other.public_key(), scope, crate::community::Epoch(2), &[0x22u8; 32]).unwrap();
        let e2 = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, community.server_root_key.as_bytes(), &channel_id,
            crate::community::Epoch(2), crate::community::Epoch(1),
            &super::super::rekey::epoch_key_commitment(crate::community::Epoch(1), &k1), &[b2],
        ).unwrap();
        relay.inject(&e1, &community.relays);
        relay.inject(&e2, &community.relays);

        let reached = catch_up_channel_rekeys(&relay, &community, &channel_id).await.unwrap();
        assert_eq!(reached, 1, "stops at the last epoch I was a recipient of");
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.channels[0].epoch, crate::community::Epoch(1));
    }

    #[tokio::test]
    async fn rotate_channel_rejects_unauthorized() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let rogue = Keys::generate();
        become_local(&rogue); // not the owner, holds no role
        let community = saved_community_owned_by(&owner);
        let relay = MemoryRelay::new();
        assert!(
            rotate_channel(&relay, &community, &community.channels[0].id, &[], community.server_root_key.as_bytes()).await.is_err(),
            "a non-authorized member cannot rotate"
        );
        // My head did not move.
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.channels[0].epoch, crate::community::Epoch(0));
    }

    // --- rotate_server_root (#4c) ---

    #[tokio::test]
    async fn rotate_server_root_publishes_recoverable_rekey_and_advances_base() {
        use crate::community::derive::{base_rekey_pseudonym, recipient_pseudonym};
        use crate::community::rekey::{open_rekey_blob, open_rekey_event, rekey_pairwise_secret};
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        become_local(&owner); // owner is supreme (holds BAN)
        let community = saved_community_owned_by(&owner);
        let genesis_root = *community.server_root_key.as_bytes();
        let member = Keys::generate();
        let relay = MemoryRelay::new();

        let new_epoch = rotate_server_root(&relay, &community, &[member.public_key()]).await.expect("rotate base");
        assert_eq!(new_epoch, 1);

        // Owner's base head advanced to a fresh root.
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_epoch, crate::community::Epoch(1));
        assert_ne!(reloaded.server_root_key.as_bytes(), &genesis_root, "base root is fresh-random, not the genesis");

        // The base rekey is found at the PRIOR-root-derived address and opens under the PRIOR (genesis) root.
        let addr = base_rekey_pseudonym(&crate::community::ServerRootKey(genesis_root), &community.id, crate::community::Epoch(1)).to_hex();
        let found = relay
            .fetch(&Query { kinds: vec![event_kind::COMMUNITY_REKEY], z_tags: vec![addr], ..Default::default() }, &community.relays)
            .await
            .unwrap();
        assert_eq!(found.len(), 1, "base rekey addressable by its prior-root pseudonym");
        let parsed = open_rekey_event(&found[0], &genesis_root).unwrap();
        assert!(matches!(parsed.scope, crate::community::derive::RekeyScope::ServerRoot));
        assert_eq!(parsed.rotator, owner.public_key());
        assert_eq!(parsed.blobs.len(), 2, "member + me (multi-device)");

        // The member recovers a root, and it equals the owner's advanced base head (one source of truth).
        let secret = rekey_pairwise_secret(member.secret_key(), &parsed.rotator).unwrap();
        let loc = recipient_pseudonym(&secret, parsed.scope, parsed.new_epoch).to_hex();
        let mine = parsed.blobs.iter().find(|b| b.locator == loc).expect("member's blob present");
        let recovered = open_rekey_blob(member.secret_key(), &parsed.rotator, parsed.scope, parsed.new_epoch, mine).unwrap();
        assert_eq!(reloaded.server_root_key.as_bytes(), &recovered, "member's recovered root == owner's advanced base head");
    }

    #[tokio::test]
    async fn rotate_server_root_failed_publish_leaves_base_unadvanced() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        become_local(&owner);
        let community = saved_community_owned_by(&owner);
        let member = Keys::generate();
        assert!(rotate_server_root(&FailingRelay, &community, &[member.public_key()]).await.is_err());
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_epoch, crate::community::Epoch(0), "base head stays put on publish failure");
    }

    #[tokio::test]
    async fn rotate_server_root_dedups_self_in_recipients() {
        // Passing my own pubkey in `recipients` must not produce a duplicate blob (I'm always added).
        use crate::community::rekey::open_rekey_event;
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        become_local(&owner);
        let community = saved_community_owned_by(&owner);
        let relay = MemoryRelay::new();
        rotate_server_root(&relay, &community, &[owner.public_key()]).await.unwrap();
        let addr = crate::community::derive::base_rekey_pseudonym(
            &crate::community::ServerRootKey(*community.server_root_key.as_bytes()), &community.id, crate::community::Epoch(1),
        )
        .to_hex();
        let found = relay
            .fetch(&Query { kinds: vec![event_kind::COMMUNITY_REKEY], z_tags: vec![addr], ..Default::default() }, &community.relays)
            .await
            .unwrap();
        let parsed = open_rekey_event(&found[0], community.server_root_key.as_bytes()).unwrap();
        assert_eq!(parsed.blobs.len(), 1, "self listed in recipients yields exactly one blob, not two");
    }

    #[tokio::test]
    async fn rotate_server_root_rejects_unauthorized() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let rogue = Keys::generate();
        become_local(&rogue); // no BAN, not owner
        let community = saved_community_owned_by(&owner);
        let relay = MemoryRelay::new();
        assert!(rotate_server_root(&relay, &community, &[]).await.is_err(), "a non-BAN member cannot rotate the base");
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_epoch, crate::community::Epoch(0));
    }

    #[tokio::test]
    async fn rotate_server_root_reanchors_the_control_plane_to_the_new_epoch() {
        // #4e-2 orchestration: a base rotation carries the control plane to the new epoch as part of the
        // SAME operation — a member reading the new root reaches the roster without a separate step.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        // create publishes 3 genesis editions (GroupRoot + #general ChannelMetadata + Admin role) and
        // records all three heads.
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        assert_eq!(crate::db::community::edition_head_entity_ids(&cid).unwrap().len(), 3);

        let member = Keys::generate();
        assert_eq!(rotate_server_root(&relay, &community, &[member.public_key()]).await.unwrap(), 1);
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_epoch, crate::community::Epoch(1), "base head advanced");

        // The Admin role is reachable at the NEW epoch under the NEW root — re-anchored by the rotation.
        let z = crate::community::roster::control_pseudonym(&reloaded.server_root_key, &community.id, crate::community::Epoch(1));
        let evs = relay
            .fetch(&Query { kinds: vec![event_kind::COMMUNITY_CONTROL], z_tags: vec![z], ..Default::default() }, &community.relays)
            .await
            .unwrap();
        let inners: Vec<_> = evs
            .iter()
            .filter_map(|o| crate::community::roster::open_control_edition(o, &reloaded.server_root_key).ok())
            .collect();
        let folded = crate::community::roster::fold_roster(&inners, &community.id, &Default::default());
        assert!(!folded.roles.roles.is_empty(), "control plane re-anchored at the new epoch as part of the rotation");
    }

    #[tokio::test]
    async fn admin_refounding_carries_heads_verbatim_preserving_owner_and_peer_roles() {
        // The verbatim-heads payoff: a NON-OWNER admin re-founds, and because each head is re-wrapped (never
        // re-authored), the owner deed AND every peer admin's owner-signed grant ride along untouched — so
        // ownership and all roles survive, while the count compacts to one edition per entity.
        use crate::community::roles::Permissions;
        let (_tmp, _guard) = init_test_db();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let owner_hex = owner.public_key().to_hex();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let admin_role = crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();

        // Owner grants TWO admins (both grants OWNER-signed).
        let alice = Keys::generate();
        let bob = Keys::generate();
        set_member_grant(&relay, &community, &alice.public_key().to_hex(), vec![admin_role.clone()]).await.unwrap();
        set_member_grant(&relay, &community, &bob.public_key().to_hex(), vec![admin_role.clone()]).await.unwrap();
        let _ = fetch_and_apply_control(&relay, &community).await;
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();

        // Drive the GroupRoot ABOVE v1 with a real published edit, so this exercises verbatim-carry of a
        // >v1 head (it must keep its real version, NOT reset to v1) — not just a v1 genesis.
        let mut edited = community.clone();
        edited.name = "HQ renamed".into();
        republish_community_metadata(&relay, &edited).await.unwrap();
        let _ = fetch_and_apply_control(&relay, &community).await;
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert!(crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap().0 >= 2, "GroupRoot now above v1");

        // ALICE (a non-owner admin) re-founds. She holds BAN, so it's authorized; she re-WRAPS heads.
        become_local(&alice);
        let new_epoch = rotate_server_root(&relay, &community, &[owner.public_key(), bob.public_key()]).await.unwrap();
        assert_eq!(new_epoch, 1);
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(community.server_root_epoch, crate::community::Epoch(1));

        // Fold the new epoch fresh (floor 0): owner unchanged + BOTH alice and bob still admins.
        let z = crate::community::roster::control_pseudonym(&community.server_root_key, &community.id, crate::community::Epoch(1));
        let evs = relay.fetch(&Query { kinds: vec![event_kind::COMMUNITY_CONTROL], z_tags: vec![z], ..Default::default() }, &community.relays).await.unwrap();
        let inners: Vec<_> = evs.iter().filter_map(|o| crate::community::roster::open_control_edition(o, &community.server_root_key).ok()).collect();
        let folded = crate::community::roster::fold_roster(&inners, &community.id, &Default::default());
        let authed = crate::community::roster::authorize_delegation(&folded, Some(&owner_hex));
        assert!(authed.is_authorized(&alice.public_key().to_hex(), Some(&owner_hex), Permissions::BAN), "alice (re-founder) still admin");
        assert!(authed.is_authorized(&bob.public_key().to_hex(), Some(&owner_hex), Permissions::BAN), "bob (peer admin) NOT demoted by alice's re-founding");
        let new_owner = folded.root_meta.as_ref().and_then(|m| m.owner_attestation.as_ref())
            .and_then(|j| Event::from_json(j).ok()).map(|e| e.pubkey.to_hex());
        assert_eq!(new_owner.as_deref(), Some(owner_hex.as_str()), "owner deed carried verbatim — ownership intact after an admin re-founding");
        assert_eq!(folded.root_meta.as_ref().map(|m| m.name.as_str()), Some("HQ renamed"),
            "the >v1 GroupRoot head carried verbatim (content preserved across the re-founding)");
        // Compacted: each entity appears at most once at the new epoch.
        let mut per_entity: std::collections::HashMap<[u8; 32], usize> = std::collections::HashMap::new();
        for i in &inners {
            if let Ok(p) = crate::community::edition::parse_edition_inner(i) { *per_entity.entry(p.entity_id).or_default() += 1; }
        }
        assert!(per_entity.values().all(|&c| c == 1), "one edition per entity at the new epoch (compacted)");
    }

    /// Block-until-synced: an admin write (rekey) is REFUSED when we're network-isolated — no relay returns
    /// the control plane we KNOW exists (we hold edition heads). Acting blind on a stale view, or advancing
    /// local state we can't publish, must not happen offline.
    #[tokio::test]
    async fn admin_write_blocked_when_isolated() {
        let (_tmp, _guard) = init_test_db();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&me);
        let cid = community.id.to_hex();
        // We hold a local edition head → we KNOW a control plane exists (so an empty fetch = isolation).
        crate::db::community::set_edition_head_with_id(&cid, &cid, 1, &[1u8; 32], &[1u8; 32]).unwrap();
        crate::db::community::set_read_cut_target_epoch(&cid, 1).unwrap();
        // FailingRelay.fetch returns Ok(empty) — the isolated case (no relay responds with anything).
        let err = reseal_base_to_observed(&FailingRelay, &community).await.unwrap_err();
        assert!(err.contains("offline") || err.contains("can't reach any relay"),
            "isolated admin write must fail closed, got: {err}");
        // Untouched: no base rotation happened.
        assert_eq!(crate::db::community::load_community(&community.id).unwrap().unwrap().server_root_epoch,
            crate::community::Epoch(0), "no rotation while isolated");
    }

    /// O2 — a re-founding rotates per-channel message keys too, not just the base. Without this a removed
    /// member holding a channel key keeps reading new messages (the base cut only covers control + @everyone).
    #[tokio::test]
    async fn refounding_rotates_channel_keys_too() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let channel_id = community.channels[0].id;
        assert_eq!(community.channels[0].epoch, crate::community::Epoch(0));
        assert_eq!(community.server_root_epoch, crate::community::Epoch(0));

        run_read_cut(&relay, &community, true).await.unwrap();

        let after = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(after.server_root_epoch, crate::community::Epoch(1), "base rotated");
        let ch = after.channels.iter().find(|c| c.id == channel_id).unwrap();
        assert_eq!(ch.epoch, crate::community::Epoch(1), "channel key rotated too (O2)");
        assert_eq!(crate::db::community::channel_rekeyed_at_server_epoch(&community.id.to_hex(), &channel_id.to_hex()).unwrap(),
            1, "channel marked rekeyed for the new base epoch");
        assert!(!crate::db::community::get_read_cut_pending(&community.id.to_hex()).unwrap(),
            "a complete read-cut clears the pending flag");
    }

    /// W2 durability — a re-founding interrupted AFTER the base rotated but BEFORE a channel rekey landed
    /// (outage / power cut / mass relay failure mid-cut) must RESUME, not restart: the retry skips the
    /// already-done base (no second epoch, no second control-plane re-anchor) and finishes only the
    /// un-rotated channel. Without resumability the retry double-rotated the base every time.
    #[tokio::test]
    async fn read_cut_resumes_without_double_base_rotation_after_channel_failure() {
        // Base + channel rekeys are both COMMUNITY_REKEY (3303); the base rekey is published BEFORE any
        // channel rekey, so the 1st 3303 is the base (allowed) and every later one is a channel (failed
        // while armed). Control re-anchor (3308) is always allowed.
        struct ChannelRekeyFails {
            inner: MemoryRelay,
            rekeys: std::sync::atomic::AtomicUsize,
            fail_channel: std::sync::atomic::AtomicBool,
        }
        #[async_trait::async_trait]
        impl Transport for ChannelRekeyFails {
            async fn publish(&self, e: &Event, r: &[String]) -> Result<(), String> { self.inner.publish(e, r).await }
            async fn publish_durable(&self, e: &Event, r: &[String]) -> Result<(), String> {
                if e.kind.as_u16() == event_kind::COMMUNITY_REKEY {
                    let n = self.rekeys.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if n >= 1 && self.fail_channel.load(std::sync::atomic::Ordering::Relaxed) {
                        return Err("channel rekey relay down".into());
                    }
                }
                self.inner.publish_durable(e, r).await
            }
            async fn fetch(&self, q: &Query, r: &[String]) -> Result<Vec<Event>, String> { self.inner.fetch(q, r).await }
        }
        let (_tmp, _guard) = init_test_db();
        let relay = ChannelRekeyFails {
            inner: MemoryRelay::new(),
            rekeys: std::sync::atomic::AtomicUsize::new(0),
            fail_channel: std::sync::atomic::AtomicBool::new(true),
        };
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let channel_id = community.channels[0].id;
        let cid = community.id.to_hex();
        let ch_hex = channel_id.to_hex();

        // Phase 1: base rotates, the channel rekey fails → the cut is left PENDING, base at epoch 1.
        assert!(run_read_cut(&relay, &community, true).await.is_err(), "the channel failure surfaces an error");
        let mid = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(mid.server_root_epoch, crate::community::Epoch(1), "base advanced exactly once");
        assert_eq!(mid.channels.iter().find(|c| c.id == channel_id).unwrap().epoch, crate::community::Epoch(0),
            "channel NOT rotated (its rekey failed)");
        assert!(crate::db::community::get_read_cut_pending(&cid).unwrap(), "cut left pending after the failure");
        assert_eq!(crate::db::community::get_read_cut_target_epoch(&cid).unwrap(), 1, "target recorded durably");
        assert_eq!(crate::db::community::channel_rekeyed_at_server_epoch(&cid, &ch_hex).unwrap(), 0,
            "channel not yet marked for this cut");

        // Phase 2: relay heals; the retry RESUMES — no second base rotation, just the leftover channel.
        relay.fail_channel.store(false, std::sync::atomic::Ordering::Relaxed);
        retry_pending_read_cut(&relay, &mid).await.unwrap();
        let done = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(done.server_root_epoch, crate::community::Epoch(1),
            "base NOT rotated again — resumed at the same epoch (no double base rotation)");
        assert_eq!(done.channels.iter().find(|c| c.id == channel_id).unwrap().epoch, crate::community::Epoch(1),
            "the un-rotated channel finished on resume");
        assert_eq!(crate::db::community::channel_rekeyed_at_server_epoch(&cid, &ch_hex).unwrap(), 1,
            "channel marked rekeyed for the cut epoch");
        assert!(!crate::db::community::get_read_cut_pending(&cid).unwrap(), "pending cleared after the resume completes");
    }

    #[tokio::test]
    async fn rotate_server_root_aborts_when_the_snapshot_does_not_land() {
        // Re-founding re-wraps the current heads, but a relay that won't ACK the re-wrapped control editions
        // leaves the snapshot incomplete → the rotation must abort with the base head NOT advanced (never
        // advance onto a plane no member folds).
        // Relay that ACKs everything UNTIL `fail` is set, then rejects control-edition (3308) publishes.
        struct ControlPublishFails { inner: MemoryRelay, fail: std::sync::atomic::AtomicBool }
        #[async_trait::async_trait]
        impl Transport for ControlPublishFails {
            async fn publish(&self, e: &Event, r: &[String]) -> Result<(), String> { self.inner.publish(e, r).await }
            async fn publish_durable(&self, e: &Event, r: &[String]) -> Result<(), String> {
                if self.fail.load(std::sync::atomic::Ordering::Relaxed) && e.kind.as_u16() == event_kind::COMMUNITY_CONTROL {
                    return Err("control relay down".into());
                }
                self.inner.publish_durable(e, r).await
            }
            async fn fetch(&self, q: &Query, r: &[String]) -> Result<Vec<Event>, String> { self.inner.fetch(q, r).await }
        }
        let (_tmp, _guard) = init_test_db();
        let relay = ControlPublishFails { inner: MemoryRelay::new(), fail: std::sync::atomic::AtomicBool::new(false) };
        // Create normally (genesis editions publish + heads recorded), THEN start failing control publishes.
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        relay.fail.store(true, std::sync::atomic::Ordering::Relaxed);

        assert!(
            rotate_server_root(&relay, &community, &[]).await.is_err(),
            "a snapshot whose editions can't be re-published must abort the rotation"
        );
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_epoch, crate::community::Epoch(0), "base head NOT advanced when the snapshot doesn't land");
    }

    #[tokio::test]
    async fn acquire_before_commit_a_reanchor_fetch_miss_publishes_no_base_rekey() {
        // #264 ACQUIRE-BEFORE-COMMIT: the re-anchor snapshot (the only mid-rekey fetch) is now fetched + sealed
        // BEFORE the base rekey is published. So a control-plane fetch miss (a head not propagated) aborts the
        // rotation with the base rekey NEVER on the wire — no half-published state to strand a member. Under the
        // old publish-first ordering the base rekey was already on relays when the fetch gate tripped.
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        struct ReanchorFetchEmpty { inner: MemoryRelay, drop_control: AtomicBool, base_rekeys: AtomicUsize }
        #[async_trait::async_trait]
        impl Transport for ReanchorFetchEmpty {
            async fn publish(&self, e: &Event, r: &[String]) -> Result<(), String> { self.inner.publish(e, r).await }
            async fn publish_durable(&self, e: &Event, r: &[String]) -> Result<(), String> {
                if e.kind.as_u16() == event_kind::COMMUNITY_REKEY {
                    self.base_rekeys.fetch_add(1, Ordering::Relaxed);
                }
                self.inner.publish_durable(e, r).await
            }
            async fn fetch(&self, q: &Query, r: &[String]) -> Result<Vec<Event>, String> {
                if self.drop_control.load(Ordering::Relaxed) && q.kinds.iter().any(|k| *k == event_kind::COMMUNITY_CONTROL) {
                    return Ok(vec![]); // the re-anchor's heads are unreachable this instant
                }
                self.inner.fetch(q, r).await
            }
        }
        let (_tmp, _guard) = init_test_db();
        let relay = ReanchorFetchEmpty { inner: MemoryRelay::new(), drop_control: AtomicBool::new(false), base_rekeys: AtomicUsize::new(0) };
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        relay.drop_control.store(true, Ordering::Relaxed);

        assert!(rotate_server_root(&relay, &community, &[]).await.is_err(),
            "a re-anchor fetch miss must abort the rotation");
        assert_eq!(relay.base_rekeys.load(Ordering::Relaxed), 0,
            "the base rekey must NOT be published when the pre-publish fetch gate trips (acquire-before-commit)");
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_epoch, crate::community::Epoch(0), "base head NOT advanced");
    }

    // --- reanchor_control_plane (#4e-1) ---

    #[tokio::test]
    async fn reanchor_carries_role_and_grant_to_the_new_epoch_under_the_new_root() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        // create_community publishes the auto Admin ROLE edition (3308) at the epoch-0 control pseudonym.
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let admin_role_id = crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        let member = Keys::generate();
        // Compaction snapshots the LOCAL folded state, so seed the grant into it (publish + apply).
        set_member_grant(&relay, &community, &member.public_key().to_hex(), vec![admin_role_id]).await.unwrap();
        let _ = fetch_and_apply_control(&relay, &community).await;
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();

        // Re-anchor by COMPACTION to a fresh root + epoch 1: each entity re-genesised to v1.
        let new_root = [0x99u8; 32];
        let snap = reanchor_control_plane(&relay, &community, &new_root, crate::community::Epoch(1)).await.unwrap();
        assert!(snap.iter().all(|e| e.published), "every snapshot edition published");
        assert_eq!(snap.len(), 4, "GroupRoot + channel + Admin role + grant compacted to v1");

        // At the NEW epoch under the NEW root, the role + grant fold back (as fresh v1 geneses, community-scoped).
        let new_z = crate::community::roster::control_pseudonym(
            &crate::community::ServerRootKey(new_root), &community.id, crate::community::Epoch(1),
        );
        let after = relay
            .fetch(&Query { kinds: vec![event_kind::COMMUNITY_CONTROL], z_tags: vec![new_z], ..Default::default() }, &community.relays)
            .await
            .unwrap();
        let inners: Vec<_> = after
            .iter()
            .filter_map(|o| crate::community::roster::open_control_edition(o, &crate::community::ServerRootKey(new_root)).ok())
            .collect();
        let folded = crate::community::roster::fold_roster(&inners, &community.id, &Default::default());
        assert!(!folded.roles.roles.is_empty(), "Admin role reachable at the new epoch");
        assert!(
            folded.roles.grants.iter().any(|g| g.member == member.public_key().to_hex()),
            "grant carried to the new epoch under the new root"
        );
    }

    #[tokio::test]
    async fn grant_after_a_rekey_survives_the_fold_at_the_new_epoch() {
        // REGRESSION (epoch consistency): a grant published AFTER a server-root rotation must seal at the
        // CURRENT epoch — where the re-anchored role definition now lives — and the fetch must look there
        // too. The bug: live publishes + the fetch hardcoded epoch 0 while the re-anchor moved the control
        // plane to the new epoch, so a post-rekey grant referenced a role the fetch never saw → the member
        // silently lost admin (exactly what we hit live).
        use crate::community::roles::Permissions;
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let admin_role_id = crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();

        // Rotate the base → epoch 1 (re-anchors the Admin role + GroupRoot under the new epoch).
        rotate_server_root(&relay, &community, &[owner.public_key()]).await.expect("rotate base");
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(community.server_root_epoch, crate::community::Epoch(1), "advanced to the new epoch");

        // Grant Alice the Admin role NOW (post-rekey): the live publish seals at server_root_epoch (1).
        let alice = "aa".repeat(32);
        set_member_grant(&relay, &community, &alice, vec![admin_role_id]).await.unwrap();

        // A fresh fetch+apply at the new epoch folds the re-anchored role AND the post-rekey grant TOGETHER.
        let roster = fetch_and_apply_roles(&relay, &community).await.unwrap();
        assert!(
            roster.has_permission(&alice, Permissions::BAN),
            "post-rekey grant survives — Alice is Admin at the new epoch (pre-fix: dropped, role unreachable)"
        );
        assert_eq!(roster.highest_position(&alice), Some(1));
    }

    /// Increment 2 — the demote AUTO-re-asserts: when the demoted member HEADS the GroupRoot, revoking
    /// them publishes an owner-authored re-assert of their content as the new head, so Concord Convergence
    /// keeps it for every client (incl. fresh joiners). End-to-end of the demote path.
    #[tokio::test]
    async fn demote_re_asserts_the_demoted_members_metadata_head() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let admin_role = crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        let alice = Keys::generate();
        let alice_hex = alice.public_key().to_hex();

        set_member_grant(&relay, &community, &alice_hex, vec![admin_role]).await.unwrap();
        // Alice (admin) renames → she heads the GroupRoot.
        become_local(&alice);
        let mut as_alice = crate::db::community::load_community(&community.id).unwrap().unwrap();
        as_alice.name = "Alice's HQ".into();
        republish_community_metadata(&relay, &as_alice).await.unwrap();
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(
            fetch_control_folded(&relay, &community).await.unwrap().root_author.map(|a| a.to_hex()),
            Some(alice_hex.clone()), "alice heads the GroupRoot after her edit",
        );

        // Owner demotes alice → auto re-assert.
        become_local(&owner);
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();
        set_member_grant(&relay, &community, &alice_hex, vec![]).await.unwrap();

        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();
        let folded = fetch_control_folded(&relay, &community).await.unwrap();
        assert_eq!(folded.root_author.map(|a| a.to_hex()), Some(owner.public_key().to_hex()),
            "the demote re-asserted the GroupRoot under the owner");
        assert_eq!(folded.root_meta.as_ref().unwrap().name, "Alice's HQ",
            "the re-assert preserves the demoted member's content");
    }

    /// Increment 2 — skip-if-not-head: demoting a member who does NOT head the GroupRoot publishes no
    /// re-assert (zero unnecessary editions — the common case). The owner made the last edit here.
    #[tokio::test]
    async fn demote_skips_reassert_when_member_does_not_head() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let admin_role = crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        let alice = Keys::generate();
        let alice_hex = alice.public_key().to_hex();

        set_member_grant(&relay, &community, &alice_hex, vec![admin_role]).await.unwrap();
        // OWNER makes the last metadata edit → the owner heads it, not alice.
        let mut c = crate::db::community::load_community(&community.id).unwrap().unwrap();
        c.name = "Owner's HQ".into();
        republish_community_metadata(&relay, &c).await.unwrap();
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();
        let before = fetch_control_folded(&relay, &community).await.unwrap().root_head.unwrap().version;

        set_member_grant(&relay, &community, &alice_hex, vec![]).await.unwrap();
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();
        let after = fetch_control_folded(&relay, &community).await.unwrap().root_head.unwrap().version;
        assert_eq!(after, before, "no re-assert published — the demoted member didn't head the GroupRoot");
    }

    #[tokio::test]
    async fn reanchor_carries_the_banlist_edition_to_the_new_epoch() {
        // The banlist is now a 3308 edition at the community-scoped banlist locator, so re-anchoring
        // (kind-agnostic within 3308) carries it forward — a post-rotation joiner gets the current bans.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let carol = "cc".repeat(32);
        // Seed the banlist into LOCAL state (publish + apply), since compaction snapshots the local set.
        publish_banlist(&relay, &community, &[carol.clone()]).await.unwrap();
        let _ = fetch_and_apply_control(&relay, &community).await;
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();

        // Re-anchor by COMPACTION to a fresh root + epoch 1: the banlist is re-genesised forward.
        let new_root = [0x99u8; 32];
        let n = reanchor_control_plane(&relay, &community, &new_root, crate::community::Epoch(1)).await.unwrap();
        assert!(n.iter().all(|e| e.published), "every snapshot edition published");
        assert_eq!(n.len(), 4, "GroupRoot + channel + Admin role + banlist compacted to v1");

        // Fetch at the new epoch under the new root → the banlist folds back with Carol still banned.
        let new_z = crate::community::roster::control_pseudonym(
            &crate::community::ServerRootKey(new_root), &community.id, crate::community::Epoch(1),
        );
        let after = relay
            .fetch(&Query { kinds: vec![event_kind::COMMUNITY_CONTROL], z_tags: vec![new_z], ..Default::default() }, &community.relays)
            .await
            .unwrap();
        let inners: Vec<_> = after
            .iter()
            .filter_map(|o| crate::community::roster::open_control_edition(o, &crate::community::ServerRootKey(new_root)).ok())
            .collect();
        let folded = crate::community::roster::fold_roster(&inners, &community.id, &Default::default());
        assert_eq!(folded.banned, vec![carol], "banlist reachable at the new epoch under the new root");
    }

    // --- apply_server_root_rekey (#4b) ---

    /// An owner-authored base rekey to `new_epoch` carrying one ServerRoot blob for `recipient_pk`,
    /// citing the community's current (genesis epoch-0) root. Returns the opened ParsedRekey.
    fn owner_base_rekey(
        owner: &Keys, community: &Community, recipient_pk: &nostr_sdk::PublicKey, new_epoch: u64, new_root: &[u8; 32],
    ) -> super::super::rekey::ParsedRekey {
        let prev = community.server_root_epoch.0;
        let blob = super::super::rekey::build_rekey_blob(
            owner.secret_key(), recipient_pk, super::super::derive::RekeyScope::ServerRoot, crate::community::Epoch(new_epoch), new_root,
        )
        .unwrap();
        let commit = super::super::rekey::epoch_key_commitment(crate::community::Epoch(prev), community.server_root_key.as_bytes());
        let outer = super::super::rekey::build_server_root_rekey_event(
            &Keys::generate(), owner, community.server_root_key.as_bytes(), &community.id,
            crate::community::Epoch(new_epoch), crate::community::Epoch(prev), &commit, &[blob],
        )
        .unwrap();
        super::super::rekey::open_rekey_event(&outer, community.server_root_key.as_bytes()).unwrap()
    }

    #[test]
    fn apply_server_root_rekey_recovers_new_root_and_advances_base() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();
        let new_root = [0xCDu8; 32];

        let parsed = owner_base_rekey(&owner, &community, &me.public_key(), 1, &new_root);
        assert_eq!(apply_server_root_rekey(&community, &parsed).unwrap(), RekeyOutcome::Applied { head_advanced: true });

        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_epoch, crate::community::Epoch(1));
        assert_eq!(reloaded.server_root_key.as_bytes(), &new_root, "base head advanced to the new root");
        // Genesis root retained (cross-epoch control/base history stays decryptable).
        assert!(crate::db::community::held_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, 0).unwrap().is_some());
        assert_eq!(crate::db::community::held_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, 1).unwrap(), Some(new_root));
    }

    #[test]
    fn apply_server_root_rekey_not_a_recipient_leaves_base_unchanged() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let other = Keys::generate(); // blob wrapped to someone else → I was removed in this rotation
        let parsed = owner_base_rekey(&owner, &community, &other.public_key(), 1, &[0x11u8; 32]);
        assert_eq!(apply_server_root_rekey(&community, &parsed).unwrap(), RekeyOutcome::NotARecipient);
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_epoch, crate::community::Epoch(0), "removed-from-base member's head unchanged");
    }

    #[test]
    fn apply_server_root_rekey_rejects_rotator_without_ban() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        // A rotator who is neither owner nor BAN-ranked cannot rotate the base.
        let rogue = Keys::generate();
        let parsed = owner_base_rekey(&rogue, &community, &me.public_key(), 1, &[0x22u8; 32]);
        assert!(apply_server_root_rekey(&community, &parsed).is_err(), "unauthorized base rotation rejected");
    }

    #[test]
    fn apply_server_root_rekey_reorgs_onto_authorized_chain_despite_prior_mismatch() {
        // BASE FORK-CONVERGENCE (mirrors the channel reorg): I hold the genesis root, but an AUTHORIZED
        // (owner, BAN) epoch-1 base rekey continues from a DIFFERENT epoch-0 root (I lost a concurrent
        // re-founding). It must be ADOPTED — converge forward onto the authorized chain — not rejected and
        // left to stall every later base rotation. Authority + ECDH recipiency are the gates, not continuity.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let blob = super::super::rekey::build_rekey_blob(
            owner.secret_key(), &me.public_key(), super::super::derive::RekeyScope::ServerRoot, crate::community::Epoch(1), &[0x33u8; 32],
        )
        .unwrap();
        // Commit over a WRONG prior root (not the genesis I hold) → continuity mismatch (the losing fork).
        let bad = super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), &[0xFFu8; 32]);
        let outer = super::super::rekey::build_server_root_rekey_event(
            &Keys::generate(), &owner, community.server_root_key.as_bytes(), &community.id,
            crate::community::Epoch(1), crate::community::Epoch(0), &bad, &[blob],
        )
        .unwrap();
        let parsed = super::super::rekey::open_rekey_event(&outer, community.server_root_key.as_bytes()).unwrap();
        let outcome = apply_server_root_rekey(&community, &parsed);
        assert!(
            matches!(outcome, Ok(RekeyOutcome::Applied { .. })),
            "an authorized base chain must be adopted (reorg), not rejected as foreign; got {outcome:?}"
        );
    }

    #[test]
    fn apply_server_root_rekey_catchup_archives_without_regressing_base_head() {
        // Parity with the channel no-regress test: applying an OLDER base epoch archives its root but
        // must not regress the base head (the forward-walk can deliver out of order).
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();

        let r5 = [0x55u8; 32];
        let p5 = owner_base_rekey(&owner, &community, &me.public_key(), 5, &r5);
        assert_eq!(apply_server_root_rekey(&community, &p5).unwrap(), RekeyOutcome::Applied { head_advanced: true });
        let r3 = [0x33u8; 32];
        let p3 = owner_base_rekey(&owner, &community, &me.public_key(), 3, &r3);
        assert_eq!(apply_server_root_rekey(&community, &p3).unwrap(), RekeyOutcome::Applied { head_advanced: false });

        assert_eq!(crate::db::community::held_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, 3).unwrap(), Some(r3));
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_epoch, crate::community::Epoch(5), "base head stayed at newest");
        assert_eq!(reloaded.server_root_key.as_bytes(), &r5);
    }

    #[test]
    fn apply_server_root_rekey_authorizes_a_granted_ban_admin() {
        // role-based: a non-owner who holds a role carrying BAN may rotate the base. Re-founding re-wraps
        // each head verbatim (never re-authors), so an admin re-founder can't demote peers or steal ownership
        // — which is exactly why this stays BAN-gated rather than owner-only.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();

        let admin = Keys::generate();
        let role_id = "d".repeat(64);
        let roster = crate::community::roles::CommunityRoles {
            roles: vec![crate::community::roles::Role::admin(role_id.clone())],
            grants: vec![crate::community::roles::MemberGrant { member: admin.public_key().to_hex(), role_ids: vec![role_id] }],
        };
        crate::db::community::set_community_roles(&cid, &roster, 1).unwrap();

        let parsed = owner_base_rekey(&admin, &community, &me.public_key(), 1, &[0x77u8; 32]);
        assert_eq!(
            apply_server_root_rekey(&community, &parsed).unwrap(),
            RekeyOutcome::Applied { head_advanced: true },
            "a BAN-granted admin (not the owner) can rotate the base"
        );
    }

    #[test]
    fn apply_server_root_rekey_accepts_when_prior_root_not_held() {
        // Catch-up from further back: a base rekey citing a prior epoch whose root I don't hold skips
        // the continuity check (ECDH blob + authority still authenticate) and applies.
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);

        let new_root = [0x99u8; 32];
        let blob = super::super::rekey::build_rekey_blob(
            owner.secret_key(), &me.public_key(), super::super::derive::RekeyScope::ServerRoot, crate::community::Epoch(5), &new_root,
        )
        .unwrap();
        // Cites epoch 4 (whose root I never held); commitment is over a root I don't have.
        let commit = super::super::rekey::epoch_key_commitment(crate::community::Epoch(4), &[0xEEu8; 32]);
        let outer = super::super::rekey::build_server_root_rekey_event(
            &Keys::generate(), &owner, community.server_root_key.as_bytes(), &community.id,
            crate::community::Epoch(5), crate::community::Epoch(4), &commit, &[blob],
        )
        .unwrap();
        let parsed = super::super::rekey::open_rekey_event(&outer, community.server_root_key.as_bytes()).unwrap();
        assert_eq!(apply_server_root_rekey(&community, &parsed).unwrap(), RekeyOutcome::Applied { head_advanced: true });
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_epoch, crate::community::Epoch(5));
    }

    #[test]
    fn apply_server_root_rekey_rejects_channel_scope() {
        // A channel-scoped rekey must NOT be applied as a base rotation (fail closed).
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let channel_parsed = owner_channel_rekey(&owner, &community, &me.public_key(), 1, &[0x44u8; 32]);
        assert!(apply_server_root_rekey(&community, &channel_parsed).is_err(), "channel scope rejected by base apply");
    }

    #[test]
    fn apply_channel_rekey_not_a_recipient() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        // The blob is wrapped to SOMEONE ELSE, so my locator finds nothing.
        let other = Keys::generate();
        let parsed = owner_channel_rekey(&owner, &community, &other.public_key(), 1, &[0x11u8; 32]);
        assert_eq!(apply_channel_rekey(&community, &parsed).unwrap(), RekeyOutcome::NotARecipient);
        // Nothing committed: head stays at epoch 0.
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.channels[0].epoch, crate::community::Epoch(0));
    }

    #[test]
    fn apply_channel_rekey_rejects_unauthorized_rotator() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        // A rotator who is NEITHER the owner NOR holds MANAGE_CHANNELS in the (empty) roster.
        let rogue = Keys::generate();
        let parsed = owner_channel_rekey(&rogue, &community, &me.public_key(), 1, &[0x22u8; 32]);
        assert!(apply_channel_rekey(&community, &parsed).is_err(), "unauthorized rotation must be rejected");
    }

    #[test]
    fn apply_channel_rekey_reorgs_onto_authorized_chain_despite_prior_mismatch() {
        // FORK-CONVERGENCE ("reorg"): I hold genesis epoch-0, but an AUTHORIZED (owner) epoch-1 rekey
        // cites a DIFFERENT epoch-0 key (a chain I'm not on) and delivers epoch-1 to ME. Authority (checked
        // first) + recipient (the blob opens) are the real gates, so I REORG forward onto the authorized
        // chain instead of rejecting + stranding myself. (The commitment is continuity, not security — it
        // yields to convergence. An UNAUTHORIZED rotator with the same mismatch is still rejected by the
        // authority gate; see apply_channel_rekey_rejects_unauthorized_rotation.)
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let chan = &community.channels[0];
        let scope = super::super::derive::RekeyScope::Channel(chan.id);
        let new_key = [0x33u8; 32];
        let blob = super::super::rekey::build_rekey_blob(owner.secret_key(), &me.public_key(), scope, crate::community::Epoch(1), &new_key).unwrap();
        // Commit over a DIFFERENT prior key than the genesis I hold → a divergent prior epoch (a fork).
        let other_commit = super::super::rekey::epoch_key_commitment(crate::community::Epoch(0), &[0xFFu8; 32]);
        let outer = super::super::rekey::build_channel_rekey_event(
            &Keys::generate(), &owner, community.server_root_key.as_bytes(), &chan.id,
            crate::community::Epoch(1), crate::community::Epoch(0), &other_commit, &[blob],
        )
        .unwrap();
        let parsed = super::super::rekey::open_rekey_event(&outer, community.server_root_key.as_bytes()).unwrap();
        let outcome = apply_channel_rekey(&community, &parsed).unwrap();
        assert!(matches!(outcome, RekeyOutcome::Applied { .. }),
            "an authorized chain must be adopted (reorg), not rejected as foreign; got {outcome:?}");
        assert_eq!(crate::db::community::held_epoch_key(&community.id.to_hex(), &chan.id.to_hex(), 1).unwrap(), Some(new_key));
    }

    #[test]
    fn apply_channel_rekey_catchup_archives_without_regressing_head() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();
        let chan_hex = community.channels[0].id.to_hex();

        // Apply epoch 5 first → head advances to 5.
        let k5 = [0x55u8; 32];
        let p5 = owner_channel_rekey(&owner, &community, &me.public_key(), 5, &k5);
        assert_eq!(apply_channel_rekey(&community, &p5).unwrap(), RekeyOutcome::Applied { head_advanced: true });
        // Now apply an OLDER epoch 3 (catch-up) → archived, but head must NOT regress.
        let k3 = [0x33u8; 32];
        let p3 = owner_channel_rekey(&owner, &community, &me.public_key(), 3, &k3);
        assert_eq!(apply_channel_rekey(&community, &p3).unwrap(), RekeyOutcome::Applied { head_advanced: false });

        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 3).unwrap(), Some(k3), "old epoch archived");
        assert_eq!(crate::db::community::held_epoch_key(&cid, &chan_hex, 5).unwrap(), Some(k5));
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.channels[0].epoch, crate::community::Epoch(5), "head stayed at the newest epoch");
        assert_eq!(reloaded.channels[0].key.as_bytes(), &k5);
    }

    #[tokio::test]
    async fn create_community_persists_and_publishes_metadata() {
        use crate::community::transport::Query;
        use crate::stored_event::event_kind;

        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "Vector HQ", "general", vec!["r1".into()])
            .await
            .expect("create");

        // Returned shape.
        assert_eq!(community.name, "Vector HQ");
        assert_eq!(community.channels.len(), 1);
        assert_eq!(community.channels[0].name, "general");

        // Persisted locally (reloadable with matching keys).
        let loaded = crate::db::community::load_community(&community.id).unwrap().expect("persisted");
        assert_eq!(loaded.channels[0].name, "general");
        assert_eq!(loaded.server_root_key.as_bytes(), community.server_root_key.as_bytes());

        // GroupRoot + ChannelMetadata are 3308 editions on the control plane, keyless
        // (the actor's inner real-npub signature is the authority proof).
        let meta_events = relay
            .fetch(
                &Query { kinds: vec![event_kind::APPLICATION_SPECIFIC], ..Default::default() },
                &community.relays,
            )
            .await
            .unwrap();
        assert!(meta_events.is_empty(), "no legacy 30078 metadata events");

        // The control plane carries THREE genesis editions, all real-npub signed by the OWNER: the
        // GroupRoot (vsk=0), the #general ChannelMetadata (vsk=2), and the auto Admin role (vsk=1).
        let z = crate::community::roster::control_pseudonym(&community.server_root_key, &community.id, crate::community::Epoch(0));
        let control = relay
            .fetch(
                &Query { kinds: vec![event_kind::COMMUNITY_CONTROL], z_tags: vec![z], ..Default::default() },
                &community.relays,
            )
            .await
            .unwrap();
        assert_eq!(control.len(), 3, "GroupRoot + ChannelMetadata + Admin role editions");
        let owner_pk = crate::state::my_public_key().unwrap();
        let parsed: Vec<_> = control
            .iter()
            .filter_map(|o| crate::community::roster::open_control_edition(o, &community.server_root_key).ok())
            .filter_map(|i| crate::community::edition::parse_edition_inner(&i).ok())
            .collect();
        assert!(parsed.iter().all(|p| p.author == owner_pk), "every genesis edition authored by the owner");
        // The GroupRoot edition (vsk=0) carries the community name + owner attestation.
        let root = parsed.iter().find(|p| p.entity_id == community.id.0).expect("GroupRoot edition");
        let root_meta: crate::community::metadata::CommunityMetadata = serde_json::from_str(&root.content).unwrap();
        assert_eq!(root_meta.name, "Vector HQ");
        assert!(root_meta.owner_attestation.is_some());
        // The Admin role edition (vsk=1) is the genesis of the Admin chain.
        let role: crate::community::roles::Role = parsed
            .iter()
            .find_map(|p| serde_json::from_str::<crate::community::roles::Role>(&p.content).ok().filter(|r| r.name == "Admin"))
            .expect("Admin role edition");
        assert_eq!(role.position, 1);
        assert!(role.permissions.contains(crate::community::roles::Permissions::ADMIN_ALL));

        // Cached locally too (the owner's client immediately knows the Admin role exists).
        let cached = crate::db::community::get_community_roles(&community.id.to_hex()).unwrap();
        assert_eq!(cached.roles.len(), 1);
        assert!(cached.grants.is_empty(), "owner is implicit position 0, takes no grant");
    }

    #[tokio::test]
    async fn role_grant_round_trips_through_relays_and_revokes() {
        use crate::community::roles::Permissions;
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()])
            .await
            .expect("create");
        let cid = community.id.to_hex();
        let alice = "aa".repeat(32);
        let admin_role_id = crate::db::community::get_community_roles(&cid).unwrap().roles[0]
            .role_id
            .clone();

        // Owner grants Alice the Admin role.
        set_member_grant(&relay, &community, &alice, vec![admin_role_id.clone()])
            .await
            .unwrap();
        assert!(
            crate::db::community::get_community_roles(&cid).unwrap().is_privileged(&alice),
            "local cache reflects the grant immediately"
        );

        // A fresh fetch+apply reconstructs the whole graph from the relays: Alice is a BAN-capable
        // Admin, and the role definition came back too.
        let roster = fetch_and_apply_roles(&relay, &community).await.unwrap();
        assert!(roster.has_permission(&alice, Permissions::BAN));
        assert!(roster.has_permission(&alice, Permissions::MANAGE_ROLES));
        assert_eq!(roster.roles.len(), 1);
        assert_eq!(roster.highest_position(&alice), Some(1));

        // Revoke (empty grant) → Alice loses the role and the empty grant is pruned from the cache.
        set_member_grant(&relay, &community, &alice, vec![]).await.unwrap();
        let after = crate::db::community::get_community_roles(&cid).unwrap();
        assert!(!after.is_privileged(&alice), "revoked member holds no role");
        assert!(after.grants.is_empty(), "empty grant pruned");
    }

    #[tokio::test]
    async fn admin_cannot_grant_a_peer_rank_role() {
        // escalation defense at the authoring gate: an Admin (position 1) may NOT grant the Admin
        // role (also position 1) — equal can't escalate equal. Only the owner (position 0, strictly
        // above) can. Closes the raw-command path even though the MVP UI gates the toggle on owner.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()])
            .await
            .expect("create");
        let cid = community.id.to_hex();
        let admin_role_id =
            crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        let alice = Keys::generate();
        // Owner seeds Alice as an Admin (set_member_grant is the low-level write, not the gated action).
        set_member_grant(&relay, &community, &alice.public_key().to_hex(), vec![admin_role_id.clone()])
            .await
            .unwrap();

        // Now ACT as Alice and try to grant the Admin role to Bob — refused.
        crate::state::set_my_public_key(alice.public_key());
        let bob = Keys::generate().public_key();
        let err = grant_role(&relay, &community, bob, &admin_role_id).await.unwrap_err();
        assert!(err.contains("below your own"), "peer-rank grant refused, got: {err}");
    }

    #[tokio::test]
    async fn create_community_mints_a_verifiable_owner_attestation() {
        // The owner attestation is mandatory at creation (no root → no community) and must prove the
        // creator as owner, bound to this community.
        let (_tmp, _guard) = init_test_db();
        let me = crate::state::my_public_key().unwrap();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()])
            .await
            .expect("create");
        let att = community.owner_attestation.as_ref().expect("attestation is mandatory");
        let proven = super::super::owner::verify_owner_attestation(att, &community.id.to_hex());
        assert_eq!(proven, Some(me), "the creator is the proven owner");
        // It can't be transplanted to a different community id.
        assert_eq!(
            super::super::owner::verify_owner_attestation(att, &"f".repeat(64)),
            None,
        );
    }

    #[tokio::test]
    async fn admin_cannot_ban_a_peer_admin() {
        // hierarchy at the banlist gate: an Admin (pos 1, holds BAN) cannot ban a *peer* Admin
        // (also pos 1) — equal can't act on equal; only someone strictly above (the owner) can. Closes
        // the B1 sibling hole (the outrank gate had been wired on grant/revoke but not on the banlist).
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()])
            .await
            .expect("create");
        let cid = community.id.to_hex();
        let admin_role_id =
            crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        let alice = Keys::generate();
        let bob = Keys::generate();
        // Owner seeds both as Admins (set_member_grant is the low-level write, not the gated action).
        set_member_grant(&relay, &community, &alice.public_key().to_hex(), vec![admin_role_id.clone()])
            .await
            .unwrap();
        set_member_grant(&relay, &community, &bob.public_key().to_hex(), vec![admin_role_id.clone()])
            .await
            .unwrap();

        // Act as Alice (the edition is signed by the vault identity → become her, not just set the
        // pubkey): she may NOT ban peer-admin Bob (rejected at the gate, before any signing).
        become_local(&alice);
        let err = publish_banlist(&relay, &community, &[bob.public_key().to_hex()])
            .await
            .unwrap_err();
        assert!(err.contains("outranks you"), "peer-admin ban refused, got: {err}");
    }

    #[tokio::test]
    async fn roster_reconstructs_purely_from_relay() {
        // Prove the fetch path reconstructs from the relay editions, NOT the optimistic local cache:
        // publish the role + a grant, WIPE the local roster cache, then fetch — a populated result
        // can then only have come from the relay.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let admin_role_id =
            crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        let alice = "aa".repeat(32);
        set_member_grant(&relay, &community, &alice, vec![admin_role_id.clone()]).await.unwrap();

        // Wipe the local cache so a populated result can ONLY come from the relay.
        crate::db::community::set_community_roles(&cid, &crate::community::roles::CommunityRoles::default(), 0).unwrap();
        assert!(crate::db::community::get_community_roles(&cid).unwrap().roles.is_empty(), "cache wiped");

        let roster = fetch_and_apply_roles(&relay, &community).await.unwrap();
        assert!(roster.is_admin(&alice), "roster reconstructed from relay editions, not the cache");
        assert_eq!(roster.roles.len(), 1, "the Admin role edition folded back");
    }

    #[tokio::test]
    async fn admin_cannot_unban_a_peer_admin() {
        // hierarchy on the REMOVAL side: an Admin can't unban (drop from the banlist) a peer Admin
        // the owner banned — gating only additions would let a low admin undo a superior's ban.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let admin_role_id =
            crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        let alice = Keys::generate();
        let bob = Keys::generate();
        set_member_grant(&relay, &community, &alice.public_key().to_hex(), vec![admin_role_id.clone()])
            .await
            .unwrap();
        set_member_grant(&relay, &community, &bob.public_key().to_hex(), vec![admin_role_id.clone()])
            .await
            .unwrap();
        // Owner banned peer-admin Bob (seed the banlist directly).
        crate::db::community::set_community_banlist(&cid, &[bob.public_key().to_hex()], 1000).unwrap();

        // Alice (admin) tries to clear the banlist → unbanning peer-admin Bob is refused.
        become_local(&alice);
        let err = publish_banlist(&relay, &community, &[]).await.unwrap_err();
        assert!(err.contains("unban"), "unbanning a peer admin refused, got: {err}");
    }

    #[tokio::test]
    async fn create_community_rejects_signer_identity_mismatch() {
        // The vault must hold the ACTIVE identity's key to sign the attestation locally. If the active
        // pubkey differs from the vault key (a stale/half-swapped session) and there's no bunker
        // client, creation fails rather than minting an attestation owned by the wrong identity.
        let (_tmp, _guard) = init_test_db(); // seeds matching vault key + my_public_key
        let other = Keys::generate();
        crate::state::set_my_public_key(other.public_key()); // force a mismatch
        let relay = MemoryRelay::new();
        let err = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap_err();
        assert!(err.contains("identity signer"), "signer mismatch refused, got: {err}");
    }

    #[tokio::test]
    async fn banlist_newer_edition_applies_older_is_refused() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()])
            .await
            .expect("create");
        let id_hex = community.id.to_hex();
        let banlist_entity = crate::simd::hex::bytes_to_hex_32(&crate::community::derive::banlist_locator(&community.id));
        let mallory = "aa".repeat(32);
        let bob = "bb".repeat(32);

        // An owner-signed v1 banlist edition (banning Mallory) is injected on the relay WITHOUT touching
        // local state, so the local head stays at 0 and the first fetch must fold it from the relay.
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let inner = crate::community::roster::build_banlist_edition(&owner, &community.id, &[mallory.clone()], 1, None, 1000, None).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        relay.inject(&outer, &community.relays);

        // Fetch folds the v1 edition, verifies the owner held BAN, applies it + advances the head.
        let applied = fetch_and_apply_banlist(&relay, &community).await.unwrap();
        assert_eq!(applied, vec![mallory.clone()]);
        let (head_v, _) = crate::db::community::get_edition_head(&id_hex, &banlist_entity).unwrap().unwrap();
        assert_eq!(head_v, 1, "banlist edition head advanced to v1");

        // We now hold a NEWER local edition (v2, banning Mallory + Bob); the relay still carries only
        // v1 — a re-fetch must NOT roll us back to it (refuse-downgrade by edition version).
        crate::db::community::set_community_banlist(&id_hex, &[mallory.clone(), bob.clone()], 2).unwrap();
        crate::db::community::set_edition_head(&id_hex, &banlist_entity, 2, &[0x22u8; 32]).unwrap();
        let after = fetch_and_apply_banlist(&relay, &community).await.unwrap();
        assert_eq!(after, vec![mallory, bob], "older relay edition refused, local banlist preserved");
    }

    #[tokio::test]
    async fn unauthorized_banlist_edition_is_rejected() {
        // The keyless BAN-authority gate: a validly-signed banlist edition from a signer who holds no
        // BAN role (not the owner, never granted) is DROPPED on fetch — the inner signature proves
        // authorship, not authority. Authority is re-verified against the authorized roster.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let bob = "bb".repeat(32);

        // A random identity (no role) signs + injects a v1 banlist edition banning Bob.
        let mallory = Keys::generate();
        let inner = crate::community::roster::build_banlist_edition(&mallory, &community.id, &[bob], 1, None, 1000, None).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        relay.inject(&outer, &community.relays);

        // Fetch must reject it (signer not authorized) — the banlist stays empty.
        let applied = fetch_and_apply_banlist(&relay, &community).await.unwrap();
        assert!(applied.is_empty(), "an unauthorized signer's banlist edition is rejected");
    }

    #[tokio::test]
    async fn banlist_receiver_enforces_per_target_outrank() {
        // The receive-side gate, not just the BAN bit: an Admin (holds BAN) who bans a PEER Admin
        // is rejected on fetch — equal can't act on equal. A bit-only check would fail open here.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let admin_role_id = crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        let alice = Keys::generate();
        let bob = Keys::generate();
        // Owner grants both Admin (so both sit at position 1, peers).
        set_member_grant(&relay, &community, &alice.public_key().to_hex(), vec![admin_role_id.clone()]).await.unwrap();
        set_member_grant(&relay, &community, &bob.public_key().to_hex(), vec![admin_role_id]).await.unwrap();

        // Alice (Admin) authors a banlist banning peer-admin Bob, citing her own (owner-granted) Admin
        // grant, injected on the relay.
        let cite = authority_citation(&community, &alice.public_key().to_hex());
        let inner = crate::community::roster::build_banlist_edition(&alice, &community.id, &[bob.public_key().to_hex()], 1, None, 1000, cite.as_ref()).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        relay.inject(&outer, &community.relays);

        // Fetch must reject it — Alice doesn't strictly outrank her peer Bob.
        let applied = fetch_and_apply_banlist(&relay, &community).await.unwrap();
        assert!(applied.is_empty(), "an admin can't ban a peer admin (receiver-side outrank)");
    }

    #[tokio::test]
    async fn banlist_admin_bans_regular_member_applies() {
        // The positive companion to the peer-rejection: an Admin (holds BAN) banning a REGULAR member
        // (no role, sits below) IS authorized on the receiver — Alice strictly outranks them.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let admin_role_id = crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        let alice = Keys::generate();
        set_member_grant(&relay, &community, &alice.public_key().to_hex(), vec![admin_role_id]).await.unwrap();

        let carol = "cc".repeat(32);
        // Alice cites her owner-granted Admin grant — the pinned authority a non-owner must carry.
        let cite = authority_citation(&community, &alice.public_key().to_hex());
        let inner = crate::community::roster::build_banlist_edition(&alice, &community.id, &[carol.clone()], 1, None, 1000, cite.as_ref()).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        relay.inject(&outer, &community.relays);

        let applied = fetch_and_apply_banlist(&relay, &community).await.unwrap();
        assert_eq!(applied, vec![carol], "an admin's ban of a regular member applies");
    }

    #[tokio::test]
    async fn owner_banlist_needs_no_citation() {
        // The owner is supreme and cites nothing — an owner-signed banlist edition with NO citation
        // applies. This is the `owner_hex == actor` bypass in `authority_citation_satisfied`.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let victim = "cc".repeat(32);

        // Owner hand-signs an uncited v1 banlist, injected on the relay (local head stays 0 → folds fresh).
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let inner = crate::community::roster::build_banlist_edition(&owner, &community.id, &[victim.clone()], 1, None, 1000, None).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        relay.inject(&outer, &community.relays);

        let applied = fetch_and_apply_banlist(&relay, &community).await.unwrap();
        assert_eq!(applied, vec![victim], "an owner's uncited ban applies");
    }

    #[tokio::test]
    async fn banlist_with_forged_citation_hash_is_rejected() {
        // fork guard: an authorized admin who cites her real grant entity + version but the WRONG
        // hash (a non-canonical fork at the tip) is rejected — the cited proof must be the one we folded.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let admin_role_id = crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        let alice = Keys::generate();
        set_member_grant(&relay, &community, &alice.public_key().to_hex(), vec![admin_role_id]).await.unwrap();

        let carol = "cc".repeat(32);
        // Real entity + version, but a fabricated hash → the cited edition isn't the one that won the fold.
        let mut cite = authority_citation(&community, &alice.public_key().to_hex()).unwrap();
        cite.edition_hash = [0xEE; 32];
        let inner = crate::community::roster::build_banlist_edition(&alice, &community.id, &[carol], 1, None, 1000, Some(&cite)).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        relay.inject(&outer, &community.relays);

        let applied = fetch_and_apply_banlist(&relay, &community).await.unwrap();
        assert!(applied.is_empty(), "a forged-hash citation is rejected");
    }

    #[tokio::test]
    async fn banlist_citing_unsynced_future_version_is_rejected() {
        // The completeness gate (fail closed): a genuinely-authorized admin who cites a FUTURE version of
        // her grant that nobody has (≥ what we folded) is rejected — we can't confirm authority at a
        // version we haven't synced. Isolates the sync-floor from the permission check (she IS an admin).
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let admin_role_id = crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        let alice = Keys::generate();
        set_member_grant(&relay, &community, &alice.public_key().to_hex(), vec![admin_role_id]).await.unwrap();

        let carol = "cc".repeat(32);
        let mut cite = authority_citation(&community, &alice.public_key().to_hex()).unwrap();
        cite.version += 5; // cite a grant version that doesn't exist on any relay
        let inner = crate::community::roster::build_banlist_edition(&alice, &community.id, &[carol], 1, None, 1000, Some(&cite)).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        relay.inject(&outer, &community.relays);

        let applied = fetch_and_apply_banlist(&relay, &community).await.unwrap();
        assert!(applied.is_empty(), "citing an unsynced future grant version fails closed");
    }

    #[tokio::test]
    async fn demoted_banner_superseded_ban_is_rejected() {
        // Refuse-superseded: an admin bans (citing her v1 grant), then the owner revokes her admin role.
        // Her citation is still SATISFIED (we hold a later v2 head of her grant), but the current
        // authorized roster no longer ranks her → the per-target outrank fails → the stale ban is dropped.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let admin_role_id = crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        let alice = Keys::generate();
        set_member_grant(&relay, &community, &alice.public_key().to_hex(), vec![admin_role_id]).await.unwrap();

        let carol = "cc".repeat(32);
        // Alice bans Carol while she IS an admin, citing her v1 grant.
        let cite = authority_citation(&community, &alice.public_key().to_hex());
        let inner = crate::community::roster::build_banlist_edition(&alice, &community.id, &[carol], 1, None, 1000, cite.as_ref()).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        relay.inject(&outer, &community.relays);

        // Owner revokes Alice's admin (publishes her v2 empty grant).
        set_member_grant(&relay, &community, &alice.public_key().to_hex(), vec![]).await.unwrap();

        let applied = fetch_and_apply_banlist(&relay, &community).await.unwrap();
        assert!(applied.is_empty(), "a since-demoted banner's stale ban is rejected (refuse-superseded)");
    }

    #[tokio::test]
    async fn withheld_revocation_cannot_resurrect_a_demoted_banners_grant() {
        // The refuse-downgrade FLOOR: we have already synced Alice's revocation (her grant head is
        // at v2 locally), but a hostile relay serves only her OLD v1 admin grant + her stale ban,
        // withholding v2. The fold seeds Alice's grant from the held v2 floor, so the below-floor v1 is
        // refused — her grant never re-materializes, and the stale ban is dropped. (Without the floor,
        // the fold would roll back to v1 and re-authorize her: the H1 fail-open this closes.)
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let admin_role_id = crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        let alice = Keys::generate();
        set_member_grant(&relay, &community, &alice.public_key().to_hex(), vec![admin_role_id]).await.unwrap();

        // Alice (admin at v1) bans Carol, citing her v1 grant — only this + her v1 grant reach the relay.
        let carol = "cc".repeat(32);
        let cite = authority_citation(&community, &alice.public_key().to_hex());
        let inner = crate::community::roster::build_banlist_edition(&alice, &community.id, &[carol], 1, None, 1000, cite.as_ref()).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        relay.inject(&outer, &community.relays);

        // We've SEEN the revocation (head floor for Alice's grant advanced to v2 locally) but the relay
        // withholds the v2 edition itself.
        let alice_bytes = alice.public_key().to_bytes();
        let grant_entity = crate::simd::hex::bytes_to_hex_32(&crate::community::derive::grant_locator(&community.id, &alice_bytes));
        crate::db::community::set_edition_head(&cid, &grant_entity, 2, &[0xAB; 32]).unwrap();

        let applied = fetch_and_apply_banlist(&relay, &community).await.unwrap();
        assert!(applied.is_empty(), "a withheld revocation can't roll the banner's grant back to re-authorize them");
    }

    #[tokio::test]
    async fn invite_registry_round_trips_and_drives_is_public() {
        // computed mode: a fresh community is Private (empty registry); a peer folds the owner's
        // registry edition purely from the relay and computes Public; clearing the registry (revoke the
        // last link) flips it back to Private — the privatize precondition.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        assert!(!is_public(&community).unwrap(), "a fresh community is Private");

        // Owner's per-creator link edition v1 injected on the relay (no local head yet → folds fresh,
        // like a peer). The owner holds CREATE_INVITE (ADMIN_ALL), so the fold authorizes + unions it.
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let loc = "1a".repeat(32);
        let inner = crate::community::roster::build_invite_links_edition(&owner, &community.id, &[loc.clone()], 1, None, 1000, None).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        relay.inject(&outer, &community.relays);

        let applied = fetch_and_apply_invite_links(&relay, &community).await.unwrap();
        assert_eq!(applied, vec![loc], "the owner's link edition folds + unions from the relay");
        assert!(is_public(&community).unwrap(), "mode recomputed Public from the folded aggregate");

        // The owner retires their links (newer v2, empty) → aggregate empties → Private.
        publish_my_invite_links(&relay, &community, &[]).await.unwrap();
        let applied = fetch_and_apply_invite_links(&relay, &community).await.unwrap();
        assert!(applied.is_empty() && !is_public(&community).unwrap(), "an empty aggregate is Private");
    }

    #[tokio::test]
    async fn metadata_edit_round_trips_to_a_lagging_member() {
        // metadata fold: a member holding only the genesis v1 folds the owner's GroupRoot v2 from the
        // relay and applies the display edit. (Edition built + injected directly so the local head stays
        // at v1 — `set_edition_head` is monotonic, so a republish would advance it and defeat the test.)
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let (genesis_v, genesis_hash) = crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap();
        assert_eq!(genesis_v, 1);

        let mut edited = crate::community::metadata::CommunityMetadata::of(&community);
        edited.name = "Renamed HQ".into();
        edited.description = Some("now with a topic".into());
        let inner = crate::community::roster::build_community_root_edition(&owner, &community.id, &edited, 2, Some(&genesis_hash), 4000, None).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        relay.inject(&outer, &community.relays);

        fetch_and_apply_metadata(&relay, &community).await.unwrap();
        let after = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(after.name, "Renamed HQ", "the owner's GroupRoot edit folded from the relay");
        assert_eq!(after.description.as_deref(), Some("now with a topic"));
        assert_eq!(crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap().0, 2, "head advanced to v2");
    }

    #[tokio::test]
    async fn unauthorized_metadata_edit_is_ignored() {
        // A signer WITHOUT manage-metadata authority can't move the community's display, even with a
        // perfectly-chained, validly-signed GroupRoot edition (the author gate, not just the chain).
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let (_, genesis_hash) = crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap();

        let mallory = Keys::generate();
        let mut hacked = crate::community::metadata::CommunityMetadata::of(&community);
        hacked.name = "Pwned".into();
        let inner = crate::community::roster::build_community_root_edition(&mallory, &community.id, &hacked, 2, Some(&genesis_hash), 5000, None).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        relay.inject(&outer, &community.relays);

        fetch_and_apply_metadata(&relay, &community).await.unwrap();
        let after = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(after.name, "HQ", "a non-manage-metadata signer's GroupRoot edit is rejected");
        assert_eq!(crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap().0, 1, "an unauthorized edit never advances the head");
    }

    #[tokio::test]
    async fn channel_rename_round_trips_from_owner_edition() {
        // The vsk=2 ChannelMetadata fold: an owner-signed channel rename (v2, chained off genesis) folds
        // and applies to the matching channel.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let channel = community.channels[0].clone();
        let ch_hex = channel.id.to_hex();
        let (_, genesis_ch_hash) = crate::db::community::get_edition_head(&cid, &ch_hex).unwrap().unwrap();

        let meta = crate::community::metadata::ChannelMetadata { name: "announcements".into() };
        let inner = crate::community::roster::build_channel_metadata_edition(&owner, &channel.id, &meta, 2, Some(&genesis_ch_hash), 6000, None).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        relay.inject(&outer, &community.relays);

        fetch_and_apply_metadata(&relay, &community).await.unwrap();
        let after = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(after.channels[0].name, "announcements", "the owner's channel rename folded + applied");
        assert_eq!(crate::db::community::get_edition_head(&cid, &ch_hex).unwrap().unwrap().0, 2, "channel head advanced to v2");
    }

    /// Build a v2 GroupRoot edition by `author` (name/created over genesis) → (sealed outer, self_hash,
    /// inner_id). Two of these with different (name, created) form a same-version concurrent fork.
    fn root_fork_v2(author: &Keys, community: &Community, name: &str, created: u64, genesis_hash: &[u8; 32]) -> (Event, [u8; 32], [u8; 32]) {
        let mut meta = crate::community::metadata::CommunityMetadata::of(community);
        meta.name = name.into();
        let inner = crate::community::roster::build_community_root_edition(author, &community.id, &meta, 2, Some(genesis_hash), created, None).unwrap();
        let self_hash = crate::community::version::edition_hash(&community.id.0, 2, Some(genesis_hash), inner.content.as_bytes());
        let inner_id = inner.id.to_bytes();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        (outer, self_hash, inner_id)
    }

    /// A concurrent v2 ChannelMetadata fork (the channel analogue of [`root_fork_v2`]): a v2 rename chained
    /// off the channel's genesis, returned as (sealed outer, self_hash, inner_id).
    fn channel_fork_v2(author: &Keys, community: &Community, channel_id: &crate::community::ChannelId, name: &str, created: u64, genesis_hash: &[u8; 32]) -> (Event, [u8; 32], [u8; 32]) {
        let meta = crate::community::metadata::ChannelMetadata { name: name.into() };
        let inner = crate::community::roster::build_channel_metadata_edition(author, channel_id, &meta, 2, Some(genesis_hash), created, None).unwrap();
        let self_hash = crate::community::version::edition_hash(&channel_id.0, 2, Some(genesis_hash), inner.content.as_bytes());
        let inner_id = inner.id.to_bytes();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        (outer, self_hash, inner_id)
    }

    /// W1 — channel metadata converges on a same-version fork exactly like GroupRoot: two authorized editors
    /// rename a channel concurrently (both v2, different content); a client holding the LOSER (higher inner
    /// id) converges onto the deterministic winner (lower inner id) instead of clinging to its own.
    #[tokio::test]
    async fn channel_same_version_fork_converges_to_the_lower_inner_id() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let channel_id = community.channels[0].id;
        let ch_hex = channel_id.to_hex();
        let (_, genesis_hash) = crate::db::community::get_edition_head(&cid, &ch_hex).unwrap().unwrap();

        let (out_a, ha, ida) = channel_fork_v2(&owner, &community, &channel_id, "alpha", 1000, &genesis_hash);
        let (out_b, hb, idb) = channel_fork_v2(&owner, &community, &channel_id, "bravo", 2000, &genesis_hash);
        let (win_name, win_h, win_id, lose_name, lose_h, lose_id) = if ida < idb {
            ("alpha", ha, ida, "bravo", hb, idb)
        } else {
            ("bravo", hb, idb, "alpha", ha, ida)
        };
        // Hold the LOSER locally (head + channel name), then see both forks.
        crate::db::community::set_edition_head_with_id(&cid, &ch_hex, 2, &lose_h, &lose_id).unwrap();
        {
            let mut c = crate::db::community::load_community(&community.id).unwrap().unwrap();
            c.channels.iter_mut().find(|ch| ch.id == channel_id).unwrap().name = lose_name.into();
            crate::db::community::save_community(&c).unwrap();
        }
        relay.inject(&out_a, &community.relays);
        relay.inject(&out_b, &community.relays);

        fetch_and_apply_metadata(&relay, &community).await.unwrap();
        let after = crate::db::community::load_community(&community.id).unwrap().unwrap();
        let ch_name = &after.channels.iter().find(|c| c.id == channel_id).unwrap().name;
        assert_eq!(ch_name, win_name, "channel converged on the lower-inner-id winner, not our held fork");
        assert_eq!(crate::db::community::get_edition_head(&cid, &ch_hex).unwrap().unwrap(), (2, win_h), "channel head self_hash converged at the SAME version");
        assert_eq!(crate::db::community::get_edition_head_inner_id(&cid, &ch_hex).unwrap(), Some(win_id), "channel head inner_id moved to the winner");

        // Flip-flop-proof: a second pass holding the winner keeps it.
        fetch_and_apply_metadata(&relay, &community).await.unwrap();
        let after2 = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(&after2.channels.iter().find(|c| c.id == channel_id).unwrap().name, win_name, "no flip back to the higher-id fork");
    }

    /// W1 — channel authority gate runs BEFORE the tiebreak: a demoted/unauthorized author's same-version
    /// channel rename loses even with the lowest inner id; the authorized rename is applied.
    #[tokio::test]
    async fn channel_same_version_fork_excludes_an_unauthorized_lower_id_edition() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let channel_id = community.channels[0].id;
        let ch_hex = channel_id.to_hex();
        let (_, genesis_hash) = crate::db::community::get_edition_head(&cid, &ch_hex).unwrap().unwrap();

        let (owner_out, owner_h, owner_id) = channel_fork_v2(&owner, &community, &channel_id, "legit", 1000, &genesis_hash);
        // Grind mallory's created_at until her forgery sorts FIRST author-blind (lower inner id).
        let mallory = Keys::generate();
        let mal_out = {
            let mut chosen = None;
            for t in 1..=10_000u64 {
                let cand = channel_fork_v2(&mallory, &community, &channel_id, "forged", t, &genesis_hash);
                if cand.2 < owner_id { chosen = Some(cand.0); break; }
            }
            chosen.expect("a mallory channel edition with a lower inner id")
        };
        relay.inject(&owner_out, &community.relays);
        relay.inject(&mal_out, &community.relays);

        fetch_and_apply_metadata(&relay, &community).await.unwrap();
        let after = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(&after.channels.iter().find(|c| c.id == channel_id).unwrap().name, &"legit".to_string(), "the channel forgery never wins despite a lower inner id");
        assert_eq!(crate::db::community::get_edition_head(&cid, &ch_hex).unwrap().unwrap(), (2, owner_h), "the authorized channel edition is the head");
    }

    /// epoch-primary floor: a re-founding re-genesises every entity to v1 under the NEW epoch, and that
    /// v1 must supersede the held high version (else compaction is impossible) — WITHOUT weakening in-epoch
    /// refuse-downgrade. Exercises the `set_edition_head` write guard directly.
    #[tokio::test]
    async fn epoch_primary_floor_lets_a_refounding_v1_supersede_a_held_high_version() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        // Drive the GroupRoot head to v5 within epoch 0.
        for v in 2..=5u64 {
            crate::db::community::set_edition_head_with_id(&cid, &cid, v, &[v as u8; 32], &[v as u8; 32]).unwrap();
        }
        assert_eq!(crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap().0, 5);
        // In-epoch refuse-downgrade still holds: a lower version is a no-op.
        crate::db::community::set_edition_head_with_id(&cid, &cid, 3, &[0x33; 32], &[0x33; 32]).unwrap();
        assert_eq!(crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap().0, 5, "in-epoch downgrade refused");

        // Re-found: bump the community to epoch 1, then write the compacted GroupRoot genesis (v1 @ epoch 1).
        crate::db::community::advance_server_root_epoch(&cid, 1, &[0xEE; 32]).unwrap();
        crate::db::community::set_edition_head_with_id(&cid, &cid, 1, &[0x01; 32], &[0x01; 32]).unwrap();
        let (v, h) = crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap();
        assert_eq!((v, h), (1, [0x01; 32]), "epoch-1 v1 supersedes epoch-0 v5 (epoch-primary)");
        assert_eq!(
            crate::db::community::get_all_edition_heads_epoched(&cid).unwrap().get(&cid).map(|(e, v, _)| (*e, *v)),
            Some((1, 1)),
            "head now recorded at epoch 1",
        );
        // And within the NEW epoch, refuse-downgrade resumes: v1 holds, a re-presented v1 stays.
        crate::db::community::set_edition_head_with_id(&cid, &cid, 1, &[0xAA; 32], &[0x02; 32]).unwrap();
        assert_eq!(crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap().1, [0x01; 32], "same-epoch same-version is not an advance");
    }

    /// T1 — Concord Convergence: two authorized editors edit from the same base, both produce v2 with
    /// different content. A client holding the LOSER (higher inner id) converges IN PLACE onto the
    /// deterministic winner (lower inner id) instead of clinging to its own — the live divergence bug.
    #[tokio::test]
    async fn same_version_fork_converges_to_the_lower_inner_id() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let (_, genesis_hash) = crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap();

        let (out_a, ha, ida) = root_fork_v2(&owner, &community, "Alpha", 1000, &genesis_hash);
        let (out_b, hb, idb) = root_fork_v2(&owner, &community, "Bravo", 2000, &genesis_hash);
        // Winner = lower inner id; we hold the loser.
        let (win_name, win_h, win_id, lose_name, lose_h, lose_id) = if ida < idb {
            ("Alpha", ha, ida, "Bravo", hb, idb)
        } else {
            ("Bravo", hb, idb, "Alpha", ha, ida)
        };
        crate::db::community::set_edition_head_with_id(&cid, &cid, 2, &lose_h, &lose_id).unwrap();
        {
            let mut c = crate::db::community::load_community(&community.id).unwrap().unwrap();
            c.name = lose_name.into();
            crate::db::community::save_community(&c).unwrap();
        }
        relay.inject(&out_a, &community.relays);
        relay.inject(&out_b, &community.relays);

        fetch_and_apply_metadata(&relay, &community).await.unwrap();
        let after = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(after.name, win_name, "converged on the lower-inner-id winner, not our own held fork");
        assert_eq!(crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap(), (2, win_h), "head self_hash converged at the SAME version");
        assert_eq!(crate::db::community::get_edition_head_inner_id(&cid, &cid).unwrap(), Some(win_id), "head inner_id moved to the winner");

        // Flip-flop-proof: holding the winner, a second pass seeing both forks keeps the winner.
        fetch_and_apply_metadata(&relay, &community).await.unwrap();
        assert_eq!(crate::db::community::load_community(&community.id).unwrap().unwrap().name, win_name, "no flip back to the higher-id fork");
    }

    /// T2 — the converged head feeds the next edit: v3 chains prev_hash from the CONVERGED winner, so a
    /// fresh fold reaches v3 contiguously (no re-fork). Guards the silent same-version no-op trap (B2/B5).
    #[tokio::test]
    async fn converged_head_chains_the_next_edit_without_reforking() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let (_, genesis_hash) = crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap();

        let (out_a, ha, ida) = root_fork_v2(&owner, &community, "Alpha", 1000, &genesis_hash);
        let (out_b, hb, idb) = root_fork_v2(&owner, &community, "Bravo", 2000, &genesis_hash);
        let (lose_h, lose_id) = if ida < idb { (hb, idb) } else { (ha, ida) };
        crate::db::community::set_edition_head_with_id(&cid, &cid, 2, &lose_h, &lose_id).unwrap();
        relay.inject(&out_a, &community.relays);
        relay.inject(&out_b, &community.relays);
        fetch_and_apply_metadata(&relay, &community).await.unwrap();
        assert_eq!(crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap().0, 2, "converged at v2");

        let mut c = crate::db::community::load_community(&community.id).unwrap().unwrap();
        c.name = "Third".into();
        republish_community_metadata(&relay, &c).await.unwrap();
        assert_eq!(crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap().0, 3, "advanced to v3 off the converged head");

        let empty: std::collections::HashMap<String, (u64, [u8; 32])> = std::collections::HashMap::new();
        let folded = crate::community::roster::fold_roster(&fetch_control_inners(&relay, &community).await, &community.id, &empty);
        assert_eq!(folded.root_head.as_ref().map(|h| h.version), Some(3), "a fresh fold reaches v3");
        assert!(!folded.gapped_entities.contains(&community.id.0), "the chain is contiguous genesis -> winner -> v3");
    }

    /// T3 — authority gate runs BEFORE the tiebreak: an UNAUTHORIZED same-version edition loses even with
    /// the lowest inner id. We apply the authorized edition, never the forgery that sorts first.
    #[tokio::test]
    async fn same_version_fork_excludes_an_unauthorized_lower_id_edition() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let (_, genesis_hash) = crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap();

        let (owner_out, owner_h, owner_id) = root_fork_v2(&owner, &community, "Legit", 1000, &genesis_hash);
        // Grind mallory's created_at until her forgery sorts FIRST author-blind (lower inner id).
        let mallory = Keys::generate();
        let (mal_out, mal_id) = {
            let mut chosen = None;
            for t in 1..=10_000u64 {
                let cand = root_fork_v2(&mallory, &community, "Forged", t, &genesis_hash);
                if cand.2 < owner_id { chosen = Some((cand.0, cand.2)); break; }
            }
            chosen.expect("a mallory edition with a lower inner id")
        };
        assert!(mal_id < owner_id, "premise: the forgery sorts first author-blind");
        relay.inject(&owner_out, &community.relays);
        relay.inject(&mal_out, &community.relays);

        // Floor stays at genesis v1, so the consumer must CHOOSE among the v2 candidates.
        fetch_and_apply_metadata(&relay, &community).await.unwrap();
        let after = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(after.name, "Legit", "the forgery never wins despite a lower inner id");
        assert_eq!(crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap(), (2, owner_h), "the authorized edition is the head");
    }

    /// T4 — the convergence exemption is DISPLAY-ONLY: a same-version fork on an authority record
    /// (banlist) where we hold the higher-id edition stays QUARANTINED (gapped, not folded). Converging
    /// authority off a withheld view would be a relay-choosable censorship lever, so it fails closed.
    #[tokio::test]
    async fn same_version_fork_on_an_authority_record_fails_closed() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let bl_eid = crate::community::derive::banlist_locator(&community.id);
        let bl_hex = crate::simd::hex::bytes_to_hex_32(&bl_eid);

        let prev = [0x99u8; 32]; // both v2 forks cite the same (held) v1; the ==floor anchor checks self_hash
        let build_ban = |list: &[String], created: u64| {
            let inner = crate::community::roster::build_banlist_edition(&owner, &community.id, list, 2, Some(&prev), created, None).unwrap();
            let self_hash = crate::community::version::edition_hash(&bl_eid, 2, Some(&prev), inner.content.as_bytes());
            let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
            (outer, self_hash, inner.id.to_bytes())
        };
        let (out_a, ha, ida) = build_ban(&["aa".repeat(32)], 1000);
        let (out_b, hb, idb) = build_ban(&["bb".repeat(32)], 2000);
        // Hold the higher-id edition → the fold's winner (lower id) differs → adopting it would require a
        // same-version swap, which an authority record must REFUSE.
        let (lose_h, lose_id) = if ida < idb { (hb, idb) } else { (ha, ida) };
        crate::db::community::set_edition_head_with_id(&cid, &bl_hex, 2, &lose_h, &lose_id).unwrap();
        relay.inject(&out_a, &community.relays);
        relay.inject(&out_b, &community.relays);

        let floors = crate::db::community::get_all_edition_heads(&cid).unwrap();
        let folded = crate::community::roster::fold_roster(&fetch_control_inners(&relay, &community).await, &community.id, &floors);
        assert!(folded.gapped_entities.contains(&bl_eid), "the authority-record fork is quarantined");
        assert!(folded.banlist_head.is_none() && folded.banlist_author.is_none(), "no banlist folded off the withheld view");
    }

    #[tokio::test]
    async fn editions_sign_through_the_active_client_signer() {
        // The bunker code path: with a NOSTR_CLIENT signer installed, authority editions sign through
        // `client.signer()` (the same route a NIP-46 bunker takes) rather than the local-vault fallback.
        // Proven end-to-end: create a community while the client signer is active, then fold + authorize
        // its genesis (the owner attestation must verify → the editions were signed by the right identity).
        let (_tmp, _guard) = init_test_db();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        crate::state::set_nostr_client(nostr_sdk::Client::builder().signer(owner.clone()).build());

        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();

        // A banlist edition (a non-genesis authority action) also signs via the client path + folds.
        publish_banlist(&relay, &community, &["dd".repeat(32)]).await.unwrap();
        let floors = crate::db::community::get_all_edition_heads(&cid).unwrap();
        let folded = crate::community::roster::fold_roster(
            &fetch_control_inners(&relay, &community).await, &community.id, &floors);
        assert_eq!(folded.banlist_author, Some(owner.public_key()), "banlist signed by the client signer");
        assert!(folded.root_author.is_some(), "genesis GroupRoot folded");
        let _ = crate::state::take_nostr_client();
    }

    /// Fetch + open the control-plane inner editions for a community (epoch 0) — test helper.
    async fn fetch_control_inners(relay: &MemoryRelay, community: &Community) -> Vec<Event> {
        let z = crate::community::roster::control_pseudonym(&community.server_root_key, &community.id, crate::community::Epoch(0));
        let query = Query { kinds: vec![event_kind::COMMUNITY_CONTROL], z_tags: vec![z], ..Default::default() };
        let mut out = Vec::new();
        for ev in relay.fetch(&query, &community.relays).await.unwrap() {
            if let Ok(inner) = crate::community::roster::open_control_edition(&ev, &community.server_root_key) {
                out.push(inner);
            }
        }
        out
    }

    /// Drop the local secret key while keeping a client signer + the public key — the test shape of a
    /// NIP-46 bunker account (signs remotely, no raw local key for ECDH rekeys).
    fn simulate_bunker(owner: &Keys) {
        crate::state::set_nostr_client(nostr_sdk::Client::builder().signer(owner.clone()).build());
        crate::state::MY_SECRET_KEY.clear(&[]);
        assert!(crate::state::MY_SECRET_KEY.to_keys().is_none(), "bunker sim: no local key");
    }

    #[tokio::test]
    async fn am_i_banned_detects_own_npub_in_banlist() {
        // The ban self-remove signal: `am_i_banned` is true iff the local npub is in the folded banlist.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let me = crate::state::MY_SECRET_KEY.to_keys().unwrap().public_key().to_hex();
        let cid = community.id.to_hex();
        assert!(!am_i_banned(&community), "not banned on a fresh community");
        // Inject ourselves into the cached banlist (the fold would do this from a real edition).
        crate::db::community::set_community_banlist(&cid, &[me], 1).unwrap();
        assert!(am_i_banned(&community), "own npub in the banlist → banned → self-remove");
        crate::db::community::set_community_banlist(&cid, &[], 2).unwrap();
        assert!(!am_i_banned(&community), "cleared banlist → not banned");
    }

    #[tokio::test]
    async fn bunker_owner_cannot_ban_in_private_community() {
        // Fail-fast: a private-community ban needs a read-cut rekey, which a bunker account can't do. It
        // must refuse BEFORE publishing — no "banned but still readable" half-state.
        let (_tmp, _guard) = init_test_db();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        simulate_bunker(&owner);

        let victim = "cc".repeat(32);
        let err = publish_banlist(&relay, &community, &[victim]).await.unwrap_err();
        assert!(err.contains("private community") && err.contains("bunker"), "clear bunker explanation: {err}");
        assert!(
            crate::db::community::get_community_banlist(&community.id.to_hex()).unwrap().is_empty(),
            "the ban must NOT half-apply (nothing published or persisted)"
        );
        let _ = crate::state::take_nostr_client();
    }

    #[tokio::test]
    async fn bunker_owner_can_ban_in_public_community() {
        // A PUBLIC ban doesn't rekey (anti-memberlist), so a bunker account CAN ban — the guard must not
        // over-block. (Mint a link → Public, then ban as a bunker.)
        let (_tmp, _guard) = init_test_db();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        create_public_invite(&relay, &community, None, None).await.unwrap();
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert!(is_public(&community).unwrap(), "minting a link made it Public");
        simulate_bunker(&owner);

        let victim = "cc".repeat(32);
        publish_banlist(&relay, &community, &[victim.clone()]).await.unwrap();
        assert_eq!(
            crate::db::community::get_community_banlist(&community.id.to_hex()).unwrap(),
            vec![victim],
            "a public ban from a bunker account succeeds (no rekey needed)"
        );
        let _ = crate::state::take_nostr_client();
    }

    #[tokio::test]
    async fn bunker_owner_cannot_privatize() {
        // Fail-fast: revoking the LAST link privatizes → re-founding rekey, which a bunker can't do. Must
        // refuse before publishing, leaving the community Public (no half-apply).
        let (_tmp, _guard) = init_test_db();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let (token, _) = create_public_invite(&relay, &community, None, None).await.unwrap();
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();
        simulate_bunker(&owner);

        let err = revoke_public_invite(&relay, &community, &crate::simd::hex::hex_to_bytes_32(&token)).await.unwrap_err();
        assert!(err.contains("private") && err.contains("bunker"), "clear bunker explanation: {err}");
        let after = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert!(is_public(&after).unwrap(), "the revoke must NOT half-apply — community stays Public");
        let _ = crate::state::take_nostr_client();
    }

    #[tokio::test]
    async fn non_owner_admin_can_edit_community_metadata() {
        // The "no hardcoding" crux: a NON-OWNER member granted the Admin role (which carries
        // MANAGE_METADATA) can move the community's display, verified purely by the folded roster — not a
        // hardcoded owner check.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let (_, genesis_hash) = crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap();

        // Owner grants `admin` the Admin role (publishes the grant edition to the relay).
        let admin = Keys::generate();
        let admin_role_id = crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        set_member_grant(&relay, &community, &admin.public_key().to_hex(), vec![admin_role_id]).await.unwrap();

        // `admin` (NOT the owner) publishes a GroupRoot v2 renaming the community.
        let mut edited = crate::community::metadata::CommunityMetadata::of(&community);
        edited.name = "Admin Renamed".into();
        let inner = crate::community::roster::build_community_root_edition(&admin, &community.id, &edited, 2, Some(&genesis_hash), 7000, None).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        relay.inject(&outer, &community.relays);

        fetch_and_apply_metadata(&relay, &community).await.unwrap();
        let after = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(after.name, "Admin Renamed", "a MANAGE_METADATA admin (not the owner) can edit metadata");
    }

    #[tokio::test]
    async fn banning_an_admin_revokes_their_role() {
        // Removal strips authority: a banned admin's grant must NOT dangle — else unban silently restores
        // admin and the roster keeps listing a non-member as admin. Public community isolates the role-strip
        // from the read-cut.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        create_public_invite(&relay, &community, None, None).await.unwrap();
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();

        let alice = Keys::generate();
        let alice_hex = alice.public_key().to_hex();
        let admin_role_id = crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        set_member_grant(&relay, &community, &alice_hex, vec![admin_role_id]).await.unwrap();
        let holds_role = |hex: &str| crate::db::community::get_community_roles(&cid).unwrap()
            .grants.iter().any(|g| g.member == hex && !g.role_ids.is_empty());
        assert!(holds_role(&alice_hex), "alice is admin pre-ban");

        publish_banlist(&relay, &community, &[alice_hex.clone()]).await.unwrap();
        assert!(!holds_role(&alice_hex), "banning an admin revokes their role — no dangling grant");
    }

    #[tokio::test]
    async fn kicking_an_admin_revokes_their_role() {
        // Same removal-strips-authority rule for the soft tier: a kicked admin who rejoins (fresh invite)
        // must NOT be silently still-admin.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let alice = Keys::generate();
        let alice_hex = alice.public_key().to_hex();
        let admin_role_id = crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        set_member_grant(&relay, &community, &alice_hex, vec![admin_role_id]).await.unwrap();
        let holds_role = |hex: &str| crate::db::community::get_community_roles(&cid).unwrap()
            .grants.iter().any(|g| g.member == hex && !g.role_ids.is_empty());
        assert!(holds_role(&alice_hex), "alice is admin pre-kick");

        publish_kick(&relay, &community, &community.channels[0], &alice_hex).await.unwrap();
        assert!(!holds_role(&alice_hex), "kicking an admin revokes their role");
    }

    #[tokio::test]
    async fn republish_channel_metadata_renames_and_publishes() {
        // The producer (the write side the consumer test was missing): renaming via
        // `republish_channel_metadata` updates the local channel AND advances the channel head.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let channel = community.channels[0].clone();
        let ch_hex = channel.id.to_hex();

        republish_channel_metadata(&relay, &community, &channel.id, "lobby").await.unwrap();
        let after = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(after.channels[0].name, "lobby", "the producer renamed the channel locally");
        assert_eq!(crate::db::community::get_edition_head(&cid, &ch_hex).unwrap().unwrap().0, 2, "channel head advanced");
    }

    #[tokio::test]
    async fn revoking_the_last_link_privatizes_and_rotates_the_base() {
        // The privatize trigger: minting links flips the computed mode to Public WITHOUT rotating;
        // revoking a non-last link stays Public, no rotation; revoking the LAST link flips to Private AND
        // re-founds the community (rotate the base/server-root to the observed participants → epoch bump),
        // sealing out link-joined lurkers.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        assert!(!is_public(&community).unwrap(), "a fresh community is Private");
        assert_eq!(community.server_root_epoch, crate::community::Epoch(0));

        // Mint two links → Public, base NOT rotated.
        let (t1, _) = create_public_invite(&relay, &community, None, None).await.unwrap();
        let (t2, _) = create_public_invite(&relay, &community, None, None).await.unwrap();
        let c = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert!(is_public(&c).unwrap(), "minting a link flips the mode to Public");
        assert_eq!(c.server_root_epoch, crate::community::Epoch(0), "minting links does NOT rotate the base");

        // Revoke the first of two → one link remains → still Public, still no rotation.
        revoke_public_invite(&relay, &c, &crate::simd::hex::hex_to_bytes_32(&t1)).await.unwrap();
        let c = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert!(is_public(&c).unwrap(), "one link remains → still Public");
        assert_eq!(c.server_root_epoch, crate::community::Epoch(0), "revoking a non-last link does NOT rotate");

        // Revoke the LAST link → Private + base rotated (re-founding).
        revoke_public_invite(&relay, &c, &crate::simd::hex::hex_to_bytes_32(&t2)).await.unwrap();
        let c = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert!(!is_public(&c).unwrap(), "revoking the last link flips to Private");
        assert_eq!(c.server_root_epoch, crate::community::Epoch(1), "privatize re-founded: the base key rotated");

        // Idempotency: re-revoking the already-gone token must NOT re-found again (no second epoch
        // bump) — privatize fires only on a genuine Public→Private transition (`had_links`).
        revoke_public_invite(&relay, &c, &crate::simd::hex::hex_to_bytes_32(&t2)).await.unwrap();
        let c = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(c.server_root_epoch, crate::community::Epoch(1), "a no-op re-revoke does not double-rotate");
    }

    #[tokio::test]
    async fn private_ban_reseals_base_public_ban_does_not() {
        // rekey-on-removal: banning in a PRIVATE community re-seals the base (epoch bump → the banned
        // member's read access is cut); in a PUBLIC community the base is NOT rotated (anti-memberlist).
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let victim = "cc".repeat(32);

        // PRIVATE (no links) → banning rotates the base.
        assert!(!is_public(&community).unwrap(), "fresh community is Private");
        publish_banlist(&relay, &community, &[victim.clone()]).await.unwrap();
        let c = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(c.server_root_epoch, crate::community::Epoch(1), "a private-community ban re-seals the base");

        // Go PUBLIC (mint a link), then ban another member → the base must NOT rotate again.
        create_public_invite(&relay, &c, None, None).await.unwrap();
        let c = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert!(is_public(&c).unwrap(), "minted a link → Public");
        publish_banlist(&relay, &c, &[victim.clone(), "dd".repeat(32)]).await.unwrap();
        let c2 = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(c2.server_root_epoch, crate::community::Epoch(1), "a public-community ban does NOT rotate the base");
    }

    #[tokio::test]
    async fn private_ban_seals_the_banned_member_out_of_the_new_root() {
        // rekey-on-removal SECURITY crux: a banned member must be EXCLUDED from the re-seal
        // recipient set so they CANNOT recover the new root — read access actually cut, not just epoch
        // bumped. Exercises the banlist(hex)→activity(bech32) reconciliation AND the persist-before-reseal
        // ordering end-to-end. The existing ban tests assert the epoch bump but never that the victim is
        // sealed out — this is the assertion that matters.
        use crate::community::derive::{base_rekey_pseudonym, recipient_pseudonym};
        use crate::community::rekey::{open_rekey_event, rekey_pairwise_secret};
        use crate::types::Message;
        use nostr_sdk::ToBech32;
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let genesis_root = *community.server_root_key.as_bytes();
        let channel_hex = community.channels[0].id.to_hex();

        // The victim posts → observed participant (absent the ban, they'd BE a re-seal recipient).
        let victim = Keys::generate();
        let victim_b32 = victim.public_key().to_bech32().unwrap();
        let mut m = Message::default();
        m.id = "aa".repeat(32);
        m.npub = Some(victim_b32.clone());
        m.at = 1000;
        crate::db::events::save_message(&channel_hex, &m).await.unwrap();
        assert!(
            crate::db::community::community_member_activity(&cid).unwrap().iter().any(|(np, _)| np == &victim_b32),
            "victim is observed before the ban"
        );

        // Ban the victim (private community) → re-seal at epoch 1.
        publish_banlist(&relay, &community, &[victim.public_key().to_hex()]).await.unwrap();
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_epoch, crate::community::Epoch(1), "private ban re-seals the base");
        assert!(
            !crate::db::community::community_member_activity(&cid).unwrap().iter().any(|(np, _)| np == &victim_b32),
            "the banned victim is no longer observed (banlist hex → bech32 reconciliation worked)"
        );

        // The base rekey at epoch 1 must carry NO blob for the victim → they can't recover the new root.
        let addr = base_rekey_pseudonym(&crate::community::ServerRootKey(genesis_root), &community.id, crate::community::Epoch(1)).to_hex();
        let found = relay
            .fetch(&Query { kinds: vec![event_kind::COMMUNITY_REKEY], z_tags: vec![addr], ..Default::default() }, &community.relays)
            .await
            .unwrap();
        assert_eq!(found.len(), 1, "the base rekey is published");
        let parsed = open_rekey_event(&found[0], &genesis_root).unwrap();
        let secret = rekey_pairwise_secret(victim.secret_key(), &parsed.rotator).unwrap();
        let loc = recipient_pseudonym(&secret, parsed.scope, parsed.new_epoch).to_hex();
        assert!(
            parsed.blobs.iter().all(|b| b.locator != loc),
            "the BANNED victim has NO blob — sealed OUT of the new root (read access is actually cut)"
        );
    }

    /// A relay that simulates an account swap MID-PUBLISH: it bumps the session generation inside
    /// publish/publish_durable, so a `SessionGuard` captured before the call is invalid by the time the
    /// caller re-checks after the await. The actual store delegates to an inner MemoryRelay.
    struct SwapDuringPublishRelay {
        inner: MemoryRelay,
    }
    #[async_trait::async_trait]
    impl Transport for SwapDuringPublishRelay {
        async fn publish(&self, event: &Event, relays: &[String]) -> Result<(), String> {
            crate::state::bump_session_generation();
            self.inner.publish(event, relays).await
        }
        async fn publish_durable(&self, event: &Event, relays: &[String]) -> Result<(), String> {
            crate::state::bump_session_generation();
            self.inner.publish_durable(event, relays).await
        }
        async fn fetch(&self, query: &Query, relays: &[String]) -> Result<Vec<Event>, String> {
            self.inner.fetch(query, relays).await
        }
    }

    /// A write straddling I/O re-checks the session: a swap bumps the
    /// generation DURING `set_member_grant`'s publish. The edition is published under account A, but the
    /// post-await `is_valid()` gate must SKIP the local persist, so account B's DB is never written.
    #[tokio::test]
    async fn account_swap_during_grant_publish_skips_the_local_persist() {
        let (_tmp, _guard) = init_test_db();
        let setup = MemoryRelay::new();
        let community = create_community(&setup, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let member = "cc".repeat(32);
        let entity_hex = crate::simd::hex::bytes_to_hex_32(
            &crate::community::derive::grant_locator(&community.id, &crate::simd::hex::hex_to_bytes_32(&member)));
        assert!(crate::db::community::get_edition_head(&cid, &entity_hex).unwrap().is_none(), "no grant head yet");

        let swap = SwapDuringPublishRelay { inner: MemoryRelay::new() };
        set_member_grant(&swap, &community, &member, vec!["a".repeat(64)]).await.unwrap();

        assert!(
            crate::db::community::get_edition_head(&cid, &entity_hex).unwrap().is_none(),
            "session straddled a swap → persist skipped → no local grant head (account B uncorrupted)"
        );
    }

    /// A swap during `publish_banlist`'s publish must leave NO half-applied state — the banlist isn't
    /// persisted, the private-community base isn't rotated, and `read_cut_pending` isn't flipped (every step
    /// gates on `is_valid()`). No ban half-lands in the wrong account.
    #[tokio::test]
    async fn account_swap_during_ban_publish_applies_nothing_locally() {
        let (_tmp, _guard) = init_test_db();
        let setup = MemoryRelay::new();
        let community = create_community(&setup, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        assert!(!is_public(&community).unwrap(), "fresh community is Private (a ban would normally re-seal)");

        let swap = SwapDuringPublishRelay { inner: MemoryRelay::new() };
        publish_banlist(&swap, &community, &["cc".repeat(32)]).await.unwrap();

        assert!(crate::db::community::get_community_banlist(&cid).unwrap().is_empty(),
            "banlist persist skipped on the stale session");
        assert_eq!(crate::db::community::load_community(&community.id).unwrap().unwrap().server_root_epoch,
            crate::community::Epoch(0), "no read-cut re-seal → base NOT rotated into the wrong account");
        assert!(!crate::db::community::get_read_cut_pending(&cid).unwrap(),
            "read_cut_pending untouched (need_cut requires is_valid())");
    }

    /// `swap_session` leaves no cross-account residue — STATE and the key vaults are
    /// cleared, so account B can't inherit account A's chats/keys.
    #[tokio::test]
    async fn swap_session_clears_per_account_state_and_keys() {
        let (_tmp, _guard) = init_test_db();
        {
            let mut st = crate::state::STATE.lock().await;
            st.db_loaded = true;
            st.is_syncing = true;
        }
        assert!(crate::state::MY_SECRET_KEY.has_key(), "account A holds a live key");

        crate::VectorCore.swap_session().await;

        let st = crate::state::STATE.lock().await;
        assert!(st.chats.is_empty() && st.profiles.is_empty(), "STATE chats/profiles cleared on swap");
        assert!(!st.db_loaded && !st.is_syncing, "db_loaded / is_syncing reset");
        assert!(!crate::state::MY_SECRET_KEY.has_key(), "key vault cleared — no leak into account B");
    }

    /// A clean join PERSISTS the community up front (so the catch-up/fold can read it
    /// back) and registers the channel as a chat. Without the up-front save, the fold's load returns None
    /// and nothing persists.
    #[tokio::test]
    async fn join_finalization_persists_and_registers_the_channel() {
        let (_tmp, _guard) = init_test_db();
        crate::state::STATE.lock().await.chats.clear(); // drop any residue from a prior serialized test
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        // A different identity joins.
        become_local(&Keys::generate());

        crate::VectorCore.finalize_member_join(community.clone(), &relay, None).await.unwrap();

        assert!(crate::db::community::load_community(&community.id).unwrap().is_some(), "community persisted on join");
        assert!(!crate::state::STATE.lock().await.chats.is_empty(), "the channel is registered as a chat");
    }

    /// If the folded banlist names the joiner, `am_i_banned` fires and the
    /// just-saved community is torn back DOWN, the join returns Err, and — since the presence beacon publish
    /// is AFTER the ban check — no phantom join is announced. No orphaned community row is left behind.
    #[tokio::test]
    async fn join_finalization_tears_down_a_banned_joiner() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        // Go Public (mint a link) so the ban is anti-memberlist and does NOT rotate the base.
        create_public_invite(&relay, &community, None, None).await.unwrap();
        let community = crate::db::community::load_community(&community.id).unwrap().unwrap();

        // Ban a would-be joiner, then become them.
        let joiner = Keys::generate();
        publish_banlist(&relay, &community, &[joiner.public_key().to_hex()]).await.unwrap();
        become_local(&joiner);
        assert!(crate::db::community::load_community(&community.id).unwrap().is_some(), "community present pre-join");

        let result = crate::VectorCore.finalize_member_join(community.clone(), &relay, None).await;
        assert!(result.is_err(), "a banned joiner's finalize must fail");
        assert!(result.unwrap_err().to_string().contains("banned"), "the error names the ban");
        assert!(
            crate::db::community::load_community(&community.id).unwrap().is_none(),
            "the just-saved community is torn back down — no orphaned row for a banned joiner"
        );
    }

    /// `delete_community` must wipe EVERY community-scoped table — a missed one leaves authority/key
    /// residue a leave/re-join would fold. Populates all six scoped tables (+ the denormalized banlist),
    /// deletes, asserts each is empty. `community_message_keys` is DELIBERATELY retained — those are our
    /// OWN send-side ephemeral signing keys, and the right to NIP-09-delete our own content from relays
    /// outlives membership (even after a ban/leave), so they must survive a community delete.
    #[tokio::test]
    async fn delete_community_wipes_every_community_scoped_table() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();

        // Populate every community-scoped table.
        crate::db::community::store_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, 1, &[0x11u8; 32]).unwrap();
        crate::db::community::save_public_invite("tok", &cid, "https://x/invite#y", None, None).unwrap();
        crate::db::community::save_pending_invite(&cid, "{}", "npub1inviter").unwrap();
        crate::db::community::set_edition_head(&cid, &cid, 1, &[0x22u8; 32]).unwrap();
        crate::db::community::set_community_banlist(&cid, &["cc".repeat(32)], 100).unwrap();

        // Sanity — all populated before the delete.
        assert!(crate::db::community::community_exists(&community.id).unwrap());
        assert!(!crate::db::community::held_epoch_keys(&cid, crate::community::SERVER_ROOT_SCOPE_HEX).unwrap().is_empty());
        assert!(!crate::db::community::list_public_invites(&cid).unwrap().is_empty());
        assert!(crate::db::community::list_pending_invites().unwrap().iter().any(|p| p.community_id == cid));
        assert!(!crate::db::community::get_all_edition_heads(&cid).unwrap().is_empty());
        assert!(!crate::db::community::get_community_banlist(&cid).unwrap().is_empty());

        crate::db::community::delete_community(&cid).unwrap();

        // Every scoped table is empty for this community — no residue.
        assert!(!crate::db::community::community_exists(&community.id).unwrap(), "communities row gone");
        assert!(crate::db::community::load_community(&community.id).unwrap().is_none(), "community not loadable");
        assert!(crate::db::community::held_epoch_keys(&cid, crate::community::SERVER_ROOT_SCOPE_HEX).unwrap().is_empty(), "epoch keys wiped");
        assert!(crate::db::community::list_public_invites(&cid).unwrap().is_empty(), "public invites wiped");
        assert!(!crate::db::community::list_pending_invites().unwrap().iter().any(|p| p.community_id == cid), "pending invites wiped");
        assert!(crate::db::community::get_all_edition_heads(&cid).unwrap().is_empty(), "edition heads wiped");
        assert!(crate::db::community::get_community_banlist(&cid).unwrap().is_empty(), "banlist wiped with the channels");
    }

    /// A hostile relay piles JUNK at the control coordinate — a kind-3308 event at the right `#z` but
    /// with garbage content (not sealed under the server root). `open_control_edition` fails to decrypt it,
    /// so it's dropped before the fold; the genuine genesis plane still folds. No panic, no corruption.
    #[tokio::test]
    async fn fetch_control_folded_skips_junk_injected_at_the_coordinate() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let owner_hex = crate::state::MY_SECRET_KEY.to_keys().unwrap().public_key().to_hex();

        // Garbage 3308 at the real control pseudonym, ephemeral-signed (outers always are).
        let z = crate::community::roster::control_pseudonym(&community.server_root_key, &community.id, community.server_root_epoch);
        let junk = nostr_sdk::EventBuilder::new(nostr_sdk::Kind::Custom(event_kind::COMMUNITY_CONTROL), "not a sealed edition")
            .tags([nostr_sdk::Tag::custom(nostr_sdk::TagKind::Custom("z".into()), [z])])
            .sign_with_keys(&Keys::generate())
            .unwrap();
        relay.publish(&junk, &community.relays).await.unwrap();

        let folded = fetch_control_folded(&relay, &community).await.unwrap();
        assert!(
            !crate::community::roster::authorize_delegation(&folded, Some(&owner_hex)).roles.is_empty(),
            "the genuine Admin role still folds; the un-openable junk is silently dropped"
        );
    }

    /// Every relay is dead/empty. The fold returns an empty roster, never a panic — a member with no
    /// reachable relay degrades to "no view," not a crash.
    #[tokio::test]
    async fn fetch_control_folded_on_dead_relays_is_empty_not_a_panic() {
        let (_tmp, _guard) = init_test_db();
        let community = saved_community_owned_by(&Keys::generate());
        let folded = fetch_control_folded(&FailingRelay, &community).await.unwrap();
        assert!(folded.roles.roles.is_empty() && folded.root_meta.is_none(), "dead relays → empty fold, no panic");
    }

    #[tokio::test]
    async fn successful_private_ban_leaves_no_read_cut_pending() {
        // The happy path leaves no outstanding read-cut: the re-seal succeeds, so the flag is cleared.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        publish_banlist(&relay, &community, &["cc".repeat(32)]).await.unwrap();
        assert!(!crate::db::community::get_read_cut_pending(&cid).unwrap(), "a successful re-seal leaves no pending read-cut");
        let c = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(c.server_root_epoch, crate::community::Epoch(1));
    }

    #[tokio::test]
    async fn failed_reseal_sets_pending_then_sync_retry_recovers() {
        // The recoverability fix (closes the #5c-1 HIGH for the total-outage case): a private ban whose
        // read-cut re-seal FAILS (the base rekey can't reach relays) still applies the ban, marks
        // `read_cut_pending`, and propagates the error — then a later community sync retries the re-seal
        // and recovers (the banned member's read access is finally cut), with no manual re-ban.
        let (_tmp, _guard) = init_test_db();
        let relay = RekeyFailingRelay::new(); // the base rekey (3303) will fail; the banlist edition lands
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let victim = "cc".repeat(32);

        // The ban applies (banlist persisted) but the read-cut re-seal fails → Err + pending set, base not rotated.
        assert!(publish_banlist(&relay, &community, &[victim.clone()]).await.is_err(), "the re-seal's base rekey fails");
        assert!(crate::db::community::get_read_cut_pending(&cid).unwrap(), "a failed re-seal leaves read_cut_pending set");
        assert_eq!(crate::db::community::get_community_banlist(&cid).unwrap(), vec![victim.clone()], "the ban itself still applied");
        let c = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(c.server_root_epoch, crate::community::Epoch(0), "base NOT rotated while the re-seal is pending");

        // The relay recovers; the sync-path retry re-attempts the read-cut and succeeds.
        relay.allow_rekey();
        retry_pending_read_cut(&relay, &c).await.unwrap();
        assert!(!crate::db::community::get_read_cut_pending(&cid).unwrap(), "pending cleared after the retry succeeds");
        let c2 = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(c2.server_root_epoch, crate::community::Epoch(1), "the read-cut finally rotated the base");
    }

    #[tokio::test]
    async fn privatize_reseals_to_observed_participants_not_just_owner() {
        // Regression for the bech32-vs-hex recipient bug (B1): privatize must re-seal to the OBSERVED
        // participants (parsed from the events table's BECH32 npubs), not collapse to owner-only. Alice
        // posts → she's observed → after privatize she is a base-rekey recipient and recovers the new root.
        use crate::community::derive::{base_rekey_pseudonym, recipient_pseudonym};
        use crate::community::rekey::{open_rekey_blob, open_rekey_event, rekey_pairwise_secret};
        use crate::types::Message;
        use nostr_sdk::ToBech32;
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let genesis_root = *community.server_root_key.as_bytes();
        let channel_hex = community.channels[0].id.to_hex();

        // Alice posts in the channel → community_member_activity observes her (bech32 npub in events).
        let alice = Keys::generate();
        let alice_b32 = alice.public_key().to_bech32().unwrap();
        let mut m = Message::default();
        m.id = "aa".repeat(32);
        m.npub = Some(alice_b32.clone());
        m.at = 1000;
        crate::db::events::save_message(&channel_hex, &m).await.unwrap();
        assert!(
            crate::db::community::community_member_activity(&cid).unwrap().iter().any(|(np, _)| np == &alice_b32),
            "alice is an observed participant"
        );

        // Mint a link → Public, then revoke it (last link) → privatize re-seals to {owner, alice}.
        let (token_hex, _) = create_public_invite(&relay, &community, None, None).await.unwrap();
        revoke_public_invite(&relay, &community, &crate::simd::hex::hex_to_bytes_32(&token_hex)).await.unwrap();
        let reloaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_epoch, crate::community::Epoch(1), "privatize rotated the base");

        // Alice MUST be a recipient of the base rekey → recovers the new root (with the B1 bug she'd be
        // sealed out, leaving only the owner).
        let addr = base_rekey_pseudonym(&crate::community::ServerRootKey(genesis_root), &community.id, crate::community::Epoch(1)).to_hex();
        let found = relay
            .fetch(&Query { kinds: vec![event_kind::COMMUNITY_REKEY], z_tags: vec![addr], ..Default::default() }, &community.relays)
            .await
            .unwrap();
        assert_eq!(found.len(), 1, "the base rekey is published");
        let parsed = open_rekey_event(&found[0], &genesis_root).unwrap();
        let secret = rekey_pairwise_secret(alice.secret_key(), &parsed.rotator).unwrap();
        let loc = recipient_pseudonym(&secret, parsed.scope, parsed.new_epoch).to_hex();
        let alice_blob = parsed.blobs.iter().find(|b| b.locator == loc).expect("alice's blob present (NOT sealed out)");
        let recovered = open_rekey_blob(alice.secret_key(), &parsed.rotator, parsed.scope, parsed.new_epoch, alice_blob).unwrap();
        assert_eq!(reloaded.server_root_key.as_bytes(), &recovered, "alice recovers the new root = owner's advanced base");
    }

    #[tokio::test]
    async fn unpermissioned_invite_links_edition_is_rejected() {
        // authority: a creator's link edition counts only if they held CREATE_INVITE. A member without
        // it forging a link edition at their own coordinate (validly signed + version-shaped) is dropped
        // on fold — so an unpermissioned member can't flip the community Public.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();

        let mallory = Keys::generate();
        let loc = "2b".repeat(32);
        let inner = crate::community::roster::build_invite_links_edition(&mallory, &community.id, &[loc], 1, None, 1000, None).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        relay.inject(&outer, &community.relays);

        let applied = fetch_and_apply_invite_links(&relay, &community).await.unwrap();
        assert!(applied.is_empty(), "an unpermissioned member's link edition is rejected");
        assert!(!is_public(&community).unwrap(), "mode stays Private despite the forged edition");
    }

    #[tokio::test]
    async fn invite_links_union_across_authorized_creators() {
        // per-creator: the owner AND a granted admin (both hold CREATE_INVITE) each publish their OWN
        // link edition; the fold UNIONS both authorized creators' locators into the aggregate. Proves
        // multiple creators + non-owner authorization (no shared registry, no MANAGE_INVITES).
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();

        // Owner mints a link → their own per-creator edition.
        create_public_invite(&relay, &community, None, None).await.unwrap();
        let owner_loc = public_invite::locator_hex(&crate::simd::hex::hex_to_bytes_32(
            &crate::db::community::list_public_invites(&cid).unwrap()[0].token));

        // Grant `admin` the Admin role (carries CREATE_INVITE), then inject THEIR own link edition.
        let admin = Keys::generate();
        let admin_role_id = crate::db::community::get_community_roles(&cid).unwrap().roles[0].role_id.clone();
        set_member_grant(&relay, &community, &admin.public_key().to_hex(), vec![admin_role_id]).await.unwrap();
        let admin_loc = "ab".repeat(32);
        let inner = crate::community::roster::build_invite_links_edition(&admin, &community.id, &[admin_loc.clone()], 1, None, 2000, None).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, crate::community::Epoch(0)).unwrap();
        relay.inject(&outer, &community.relays);

        let agg = fetch_and_apply_invite_links(&relay, &community).await.unwrap();
        assert!(agg.contains(&owner_loc), "owner's link in the aggregate");
        assert!(agg.contains(&admin_loc), "the granted admin's link unions in too");
        assert!(is_public(&community).unwrap());

        // B1: the owner revoking THEIR link must NOT privatize — the admin's link keeps it Public. The
        // revoke refreshes the aggregate from the relay first, so it sees the admin's still-live link even
        // if the local cache were stale. Base epoch stays 0 (no re-founding rekey).
        let c = crate::db::community::load_community(&community.id).unwrap().unwrap();
        let owner_token = crate::db::community::list_public_invites(&cid).unwrap()[0].token.clone();
        revoke_public_invite(&relay, &c, &crate::simd::hex::hex_to_bytes_32(&owner_token)).await.unwrap();
        let c = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(c.server_root_epoch, crate::community::Epoch(0), "another creator's link remains → no privatize rekey");
        assert!(is_public(&c).unwrap(), "still Public (admin's link is live)");
    }

    #[tokio::test]
    async fn failed_banlist_publish_does_not_persist_locally() {
        // Rollback honesty: if the ban edition never reaches relays, our local banlist must stay
        // untouched — else we'd one-sidedly drop a member's messages the rest of the community sees.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let id_hex = community.id.to_hex();
        assert!(crate::db::community::get_community_banlist(&id_hex).unwrap().is_empty());

        let victim = "cc".repeat(32);
        let err = publish_banlist(&FailingRelay, &community, &[victim]).await;
        assert!(err.is_err(), "a failed publish must propagate");
        assert!(
            crate::db::community::get_community_banlist(&id_hex).unwrap().is_empty(),
            "local banlist must be untouched when the publish failed"
        );
    }

    #[tokio::test]
    async fn metadata_failed_publish_does_not_persist_locally() {
        // Metadata is RELAY-AUTHORITATIVE now (`fetch_and_apply_metadata` is the consumer fold): a failed
        // publish must NOT save locally, else we'd show an edit no member can see (and the phantom-head
        // rule keeps the edition head from advancing too). Convergence is publish-then-fold, not re-publish.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let mut community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        community.name = "Renamed HQ".to_string();
        assert!(republish_community_metadata(&FailingRelay, &community).await.is_err());
        let loaded = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(loaded.name, "HQ", "a failed metadata publish leaves the local name unchanged");
    }

    #[tokio::test]
    async fn send_persists_key_then_delete_round_trip() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = Community::create("HQ", "general", vec!["r1".into()]);
        let channel = community.channels[0].clone();
        let alice = Keys::generate();

        // send_message persists the ephemeral key keyed by the INNER message id...
        let _outer = send_message(&relay, &community, &channel, &alice, "deletable", 1).await.unwrap();
        let before = fetch_channel_messages(&relay, &community, &channel).await.unwrap();
        assert_eq!(before.len(), 1);
        let message_id = before[0].message_id.to_hex();

        // ...so delete_message (by inner message id, what the UI holds) removes it.
        delete_message(&relay, &message_id).await.unwrap();
        let after = fetch_channel_messages(&relay, &community, &channel).await.unwrap();
        assert!(after.is_empty(), "message should be deleted after delete_message");

        // The key is single-use: a second delete finds nothing retained.
        assert!(delete_message(&relay, &message_id).await.is_err());
    }

    #[tokio::test]
    async fn failed_delete_publish_preserves_key() {
        // B2: the deletion key is single-use, so a FAILED NIP-09 publish must NOT consume
        // it — otherwise the message is permanently undeletable.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = Community::create("HQ", "general", vec!["r1".into()]);
        let channel = community.channels[0].clone();
        let alice = Keys::generate();
        send_message(&relay, &community, &channel, &alice, "delete me", 1).await.unwrap();
        let message_id = fetch_channel_messages(&relay, &community, &channel).await.unwrap()[0]
            .message_id
            .to_hex();

        // Delete via a transport whose publish fails → error, key retained.
        assert!(delete_message(&FailingRelay, &message_id).await.is_err());

        // The key survived, so a retry over a working relay succeeds.
        delete_message(&relay, &message_id).await.unwrap();
        assert!(fetch_channel_messages(&relay, &community, &channel).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_unknown_message_errors() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        // A message id we never sent → no retained key → error, no panic.
        let fake = Keys::generate();
        let bogus = EventBuilder::new(Kind::Custom(1), "x").sign_with_keys(&fake).unwrap().id;
        assert!(delete_message(&relay, &bogus.to_hex()).await.is_err());
    }

    #[tokio::test]
    async fn accept_invite_persists_member_view() {
        let (_tmp, _guard) = init_test_db();
        let owner = Community::create("HQ", "general", vec!["r1".into()]);
        let invite = crate::community::invite::build_invite(&owner);

        let joined = accept_invite(&invite).expect("accept");
        assert!(!is_proven_owner(&joined), "joined as member, not owner");
        // Persisted + reloadable with the same read keys.
        let loaded = crate::db::community::load_community(&owner.id).unwrap().expect("saved");
        assert_eq!(loaded.channels[0].key.as_bytes(), owner.channels[0].key.as_bytes());
    }

    #[tokio::test]
    async fn accept_invite_does_not_downgrade_owned_community() {
        // We OWN a Community (proven via the owner attestation); an invite reusing its id must be
        // refused so it can't overwrite our row.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let owner = create_community(&relay, "HQ", "general", vec![]).await.unwrap();
        assert!(is_proven_owner(&owner), "we are the proven owner");

        let invite = crate::community::invite::build_invite(&owner);
        let err = accept_invite(&invite).unwrap_err();
        assert!(err.contains("already own"), "must refuse to downgrade an owned community, got: {err}");

        // The owner row is intact (same server-root key).
        let reloaded = crate::db::community::load_community(&owner.id).unwrap().unwrap();
        assert_eq!(reloaded.server_root_key.as_bytes(), owner.server_root_key.as_bytes());
    }

    #[tokio::test]
    async fn accept_invite_rejects_id_collision_under_different_authority() {
        // We hold Community X as a MEMBER (authority pubkey A). A hostile bundle reuses
        // X's id but names a DIFFERENT authority + channel keys. It must be rejected so
        // our keys/authority/relays can't be silently swapped (community_id is
        // unauthenticated random bytes).
        let (_tmp, _guard) = init_test_db();
        let legit = Community::create("X", "general", vec!["wss://legit".into()]);
        let member_x = accept_invite(&crate::community::invite::build_invite(&legit)).unwrap();
        let original_key = member_x.channels[0].key.as_bytes().to_vec();

        // Attacker's own Community, then forge its id to collide with X.
        let attacker = Community::create("evil", "general", vec!["wss://evil".into()]);
        let mut hostile = crate::community::invite::build_invite(&attacker);
        hostile.community_id = legit.id.to_hex();
        // The attacker's bundle carries its OWN server-root key, which differs from X's — the
        // keyless authority anchor the dedup compares.
        assert_ne!(hostile.server_root_key, crate::simd::hex::bytes_to_hex_32(member_x.server_root_key.as_bytes()));

        assert!(accept_invite(&hostile).is_err(), "id-collision under new authority must be rejected");

        // X's stored channel key is unchanged.
        let reloaded = crate::db::community::load_community(&legit.id).unwrap().unwrap();
        assert_eq!(reloaded.channels[0].key.as_bytes().to_vec(), original_key);
        assert_eq!(reloaded.relays, vec!["wss://legit".to_string()]);
    }

    #[tokio::test]
    async fn rejected_accept_leaves_pending_invite_intact() {
        // Mirrors the accept command's peek→accept→(delete only on success) order: a
        // rejected accept must NOT destroy the parked invite (no silent data loss).
        let (_tmp, _guard) = init_test_db();

        // We own this community (proven via the attestation), so an invite reusing its id is rejected.
        let owner = attested_community("HQ", "general", vec![]);
        crate::db::community::save_community(&owner).unwrap();
        let bundle = crate::community::invite::build_invite(&owner).to_json().unwrap();
        let cid = owner.id.to_hex();
        crate::db::community::save_pending_invite(&cid, &bundle, "npub1inviter").unwrap();

        // Command sequence: peek (no delete) → accept (errs) → row survives.
        let peeked = crate::db::community::get_pending_invite(&cid).unwrap().expect("parked");
        let invite = crate::community::invite::CommunityInvite::from_json(&peeked).unwrap();
        assert!(accept_invite(&invite).is_err(), "owning the id → reject");
        assert!(
            crate::db::community::pending_invite_exists(&cid).unwrap(),
            "rejected accept must leave the invite parked"
        );

        // A successful accept (community we don't already hold) clears the row.
        let other = Community::create("Other", "general", vec![]);
        let ob = crate::community::invite::build_invite(&other).to_json().unwrap();
        let ocid = other.id.to_hex();
        crate::db::community::save_pending_invite(&ocid, &ob, "npub1inviter").unwrap();
        let op = crate::db::community::get_pending_invite(&ocid).unwrap().unwrap();
        let oinvite = crate::community::invite::CommunityInvite::from_json(&op).unwrap();
        accept_invite(&oinvite).expect("accept ok");
        crate::db::community::delete_pending_invite(&ocid).unwrap();
        assert!(!crate::db::community::pending_invite_exists(&ocid).unwrap(), "cleared on success");
    }

    #[tokio::test]
    async fn public_invite_create_fetch_accept_revoke_round_trip() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let mut owner = Community::create("Public HQ", "general", vec!["r1".into(), "r2".into()]);
        owner.description = Some("everyone welcome".into());
        // Sign the owner attestation with the seeded identity so `create_public_invite`'s proven-owner
        // gate passes. The owner community is in-memory only here (create_public_invite persists the
        // token, not the community), so this single DB cleanly plays the joiner on accept.
        let owner_keys = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        owner.owner_attestation = Some(
            crate::community::owner::build_owner_attestation_unsigned(owner_keys.public_key(), &owner.id.to_hex())
                .sign_with_keys(&owner_keys).unwrap().as_json(),
        );
        // Owner mints a link.
        let (token_hex, url) = create_public_invite(&relay, &owner, None, None).await.expect("mint");
        assert!(url.contains('#'));
        assert_eq!(crate::db::community::list_public_invites(&owner.id.to_hex()).unwrap().len(), 1);

        // A joiner parses the URL → fetches → previews → accepts.
        let (relays, token) = public_invite::parse_invite_url(&url).unwrap();
        assert_eq!(crate::simd::hex::bytes_to_hex_32(&token), token_hex);
        let bundle = fetch_public_invite(&relay, &relays, &token).await.expect("fetch");
        assert_eq!(bundle.preview.name, "Public HQ");
        assert_eq!(bundle.preview.description.as_deref(), Some("everyone welcome"));

        let joined = accept_public_invite(&bundle, 0).expect("accept");
        assert_eq!(joined.id, owner.id);
        assert_eq!(joined.description.as_deref(), Some("everyone welcome"), "preview patched in");

        // Owner revokes the last link → the link no longer resolves AND the community re-founds (Private).
        revoke_public_invite(&relay, &owner, &token).await.expect("revoke");
        assert!(fetch_public_invite(&relay, &relays, &token).await.is_err(), "revoked link is dead");
        assert!(crate::db::community::list_public_invites(&owner.id.to_hex()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn revoked_invite_dies_even_if_one_relay_kept_the_bundle() {
        // Mixed-relay race (the exact case the tombstone defends): the tombstone replaces the bundle on r1,
        // but r2 was down during revoke and still serves the live bundle. fetch must STILL report the link
        // dead — a token-signed Revoked tombstone on ANY relay is authoritative and wins ties with a bundle.
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let owner = attested_community("HQ", "general", vec!["r1".into(), "r2".into()]);
        let (_token_hex, url) = create_public_invite(&relay, &owner, None, None).await.unwrap();
        let (relays, token) = public_invite::parse_invite_url(&url).unwrap();
        assert!(fetch_public_invite(&relay, &relays, &token).await.is_ok(), "live on both relays");

        // The tombstone reaches ONLY r1 (replaces the bundle there); r2 still has the live bundle.
        let tombstone = public_invite::build_public_invite_tombstone(&token).unwrap();
        relay.inject(&tombstone, &["r1".to_string()]);

        assert!(
            fetch_public_invite(&relay, &relays, &token).await.is_err(),
            "a tombstone on any one relay kills the link, even with a stale live bundle elsewhere",
        );
    }

    #[tokio::test]
    async fn fetch_skips_relay_shadow_junk_to_genuine_bundle() {
        // A hostile relay piles a NEWER event at the same locator d-tag, signed by a
        // different key (relay-shadow attack). fetch must skip it (fails token verify)
        // and still surface the genuine bundle, not report "no invite".
        use nostr_sdk::prelude::{EventBuilder, Keys, Kind, Tag, TagKind, Timestamp};

        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let owner = attested_community("HQ", "general", vec!["r1".into()]);
        let (_t, url) = create_public_invite(&relay, &owner, None, None).await.unwrap();
        let (relays, token) = public_invite::parse_invite_url(&url).unwrap();

        // Attacker posts junk at the same locator with a far-future created_at so it
        // sorts newest.
        let attacker = Keys::generate();
        let junk = EventBuilder::new(Kind::Custom(event_kind::APPLICATION_SPECIFIC), "garbage")
            .tags([
                Tag::identifier(public_invite::locator_hex(&token)),
                Tag::custom(TagKind::Custom("vsk".into()), ["6".to_string()]),
                Tag::custom(TagKind::Custom("v".into()), ["1".to_string()]),
            ])
            .custom_created_at(Timestamp::from_secs(9_000_000_000))
            .sign_with_keys(&attacker)
            .unwrap();
        relay.publish(&junk, &relays).await.unwrap();

        // Genuine bundle is still found despite the newer shadow.
        let bundle = fetch_public_invite(&relay, &relays, &token).await.expect("genuine survives shadow");
        assert_eq!(bundle.preview.name, "HQ");
    }

    #[tokio::test]
    async fn expired_public_invite_is_refused() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let owner = attested_community("HQ", "general", vec!["r1".into()]);
        let (_t, url) = create_public_invite(&relay, &owner, Some(1000), None).await.unwrap();
        let (relays, token) = public_invite::parse_invite_url(&url).unwrap();
        let bundle = fetch_public_invite(&relay, &relays, &token).await.unwrap();
        // Past expiry → accept refuses, nothing joined.
        assert!(accept_public_invite(&bundle, 2000).is_err());
        assert!(crate::db::community::load_community(&owner.id).unwrap().is_none());
    }

    #[tokio::test]
    async fn republish_metadata_saves_and_publishes() {
        use crate::community::CommunityImage;
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        // create_community mints the owner attestation (the seeded vault identity is the owner) and the
        // genesis GroupRoot edition (v1) — so the owner is proven + holds MANAGE_METADATA implicitly.
        let mut owner = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = owner.id.to_hex();

        // Edit name + description + icon, republish (publishes the GroupRoot edition at v2).
        owner.name = "HQ Renamed".into();
        owner.description = Some("now with topic".into());
        owner.icon = Some(CommunityImage {
            url: "https://b/x".into(), key: "aa".repeat(32), nonce: "bb".repeat(12),
            hash: "cc".repeat(32), ext: "png".into(),
        });
        republish_community_metadata(&relay, &owner).await.expect("republish");

        // Persisted locally.
        let loaded = crate::db::community::load_community(&owner.id).unwrap().unwrap();
        assert_eq!(loaded.name, "HQ Renamed");
        assert_eq!(loaded.description.as_deref(), Some("now with topic"));
        assert_eq!(loaded.icon.unwrap().url, "https://b/x");

        // The GroupRoot edition advanced to v2 and carries the new metadata. Fetch the control plane,
        // fold the GroupRoot entity (entity_id == community_id), confirm the head + content.
        let (head_v, _) = crate::db::community::get_edition_head(&cid, &cid).unwrap().unwrap();
        assert_eq!(head_v, 2, "GroupRoot edition advanced v1 (create) → v2 (republish)");
        let z = crate::community::roster::control_pseudonym(&owner.server_root_key, &owner.id, crate::community::Epoch(0));
        let control = relay
            .fetch(&Query { kinds: vec![event_kind::COMMUNITY_CONTROL], z_tags: vec![z], ..Default::default() }, &owner.relays)
            .await
            .unwrap();
        let newest = control
            .iter()
            .filter_map(|o| crate::community::roster::open_control_edition(o, &owner.server_root_key).ok())
            .filter_map(|i| crate::community::edition::parse_edition_inner(&i).ok())
            .filter(|p| p.entity_id == owner.id.0)
            .max_by_key(|p| p.version)
            .expect("GroupRoot edition on the relay");
        let meta: crate::community::metadata::CommunityMetadata = serde_json::from_str(&newest.content).unwrap();
        assert_eq!(meta.name, "HQ Renamed");
        assert_eq!(meta.icon.unwrap().ext, "png");
    }

    #[tokio::test]
    async fn member_cannot_republish_metadata() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let owner = Community::create("HQ", "general", vec!["r1".into()]);
        let member = crate::community::invite::accept_invite(&crate::community::invite::build_invite(&owner)).unwrap();
        assert!(republish_community_metadata(&relay, &member).await.is_err());
    }

    #[tokio::test]
    async fn member_cannot_mint_public_invite() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let owner = Community::create("HQ", "general", vec!["r1".into()]);
        let member = crate::community::invite::accept_invite(&crate::community::invite::build_invite(&owner)).unwrap();
        assert!(create_public_invite(&relay, &member, None, None).await.is_err(), "members can't mint links");
    }

    #[tokio::test]
    async fn accept_oversized_bundle_rejected() {
        let (_tmp, _guard) = init_test_db();
        let owner = Community::create("HQ", "general", vec![]);
        let mut invite = crate::community::invite::build_invite(&owner);
        // Blow past the channel cap.
        let template = invite.channels[0].clone();
        for _ in 0..300 {
            invite.channels.push(template.clone());
        }
        assert!(accept_invite(&invite).is_err(), "oversized bundle must be rejected");
        assert!(crate::db::community::load_community(&owner.id).unwrap().is_none(), "nothing persisted");
    }

    // --- owner dissolution (GroupDissolved tombstone) ---

    /// Seal + publish a GroupDissolved tombstone (vsk=10) authored by `author` to the community's control
    /// plane at the CURRENT epoch, so a subsequent `fetch_and_apply_control` folds it. `created_at` is
    /// caller-chosen so a test can prove backdating doesn't gate the binary seal.
    async fn publish_tombstone<T: Transport + ?Sized>(transport: &T, community: &Community, author: &Keys, created_at: u64) {
        let inner = crate::community::roster::build_group_dissolved_edition_unsigned(author.public_key(), &community.id, created_at)
            .sign_with_keys(author).unwrap();
        let outer = crate::community::roster::seal_control_edition(&Keys::generate(), &inner, &community.server_root_key, &community.id, community.server_root_epoch).unwrap();
        transport.publish_durable(&outer, &community.relays).await.unwrap();
    }

    /// A relay wrapping MemoryRelay that COUNTS rekey (3303) publishes — for asserting dissolution emits
    /// none. Everything else delegates to the inner relay.
    struct RekeyCountingRelay {
        inner: MemoryRelay,
        rekeys: std::sync::atomic::AtomicUsize,
    }
    impl RekeyCountingRelay {
        fn new() -> Self { Self { inner: MemoryRelay::new(), rekeys: std::sync::atomic::AtomicUsize::new(0) } }
        fn count(&self, e: &Event) {
            if e.kind.as_u16() == event_kind::COMMUNITY_REKEY {
                self.rekeys.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    #[async_trait::async_trait]
    impl Transport for RekeyCountingRelay {
        async fn publish(&self, e: &Event, r: &[String]) -> Result<(), String> { self.count(e); self.inner.publish(e, r).await }
        async fn publish_durable(&self, e: &Event, r: &[String]) -> Result<(), String> { self.count(e); self.inner.publish_durable(e, r).await }
        async fn fetch(&self, q: &Query, r: &[String]) -> Result<Vec<Event>, String> { self.inner.fetch(q, r).await }
    }

    #[tokio::test]
    async fn owner_tombstone_folds_to_dissolved() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        // The seeded local identity is the proven owner of a created community.
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        publish_tombstone(&relay, &community, &owner, 1000).await;

        assert!(!crate::db::community::get_community_dissolved(&cid).unwrap(), "alive before the fold");
        fetch_and_apply_control(&relay, &community).await.unwrap();
        assert!(crate::db::community::get_community_dissolved(&cid).unwrap(), "owner tombstone seals the community");
    }

    #[tokio::test]
    async fn non_owner_tombstone_is_ignored() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        // A BAN-capable admin is NOT enough: dissolution is the owner's call alone. A random
        // non-owner author publishing the tombstone must be rejected.
        let mallory = Keys::generate();
        publish_tombstone(&relay, &community, &mallory, 1000).await;

        fetch_and_apply_control(&relay, &community).await.unwrap();
        assert!(!crate::db::community::get_community_dissolved(&cid).unwrap(), "a non-owner tombstone is ignored");
    }

    #[tokio::test]
    async fn unreadable_deed_rejects_the_tombstone() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let mut community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        publish_tombstone(&relay, &community, &owner, 1000).await;
        // Strip the deed: the owner can no longer be derived → fail-closed, the tombstone is unverifiable.
        community.owner_attestation = None;
        crate::db::community::save_community(&community).unwrap();
        let stripped = crate::db::community::load_community(&community.id).unwrap().unwrap();

        fetch_and_apply_control(&relay, &stripped).await.unwrap();
        assert!(!crate::db::community::get_community_dissolved(&cid).unwrap(), "unverifiable tombstone is rejected, not death-by-default");
    }

    #[tokio::test]
    async fn binary_seal_drops_every_subsequent_event_with_no_timestamp_test() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        publish_tombstone(&relay, &community, &owner, 1000).await;
        fetch_and_apply_control(&relay, &community).await.unwrap();
        assert!(crate::db::community::get_community_dissolved(&cid).unwrap());

        // The channel reloaded after the seal carries the denormalized dissolved flag → inbound drops all.
        let sealed = crate::db::community::load_community(&community.id).unwrap().unwrap();
        let channel = sealed.channels[0].clone();
        let me = owner.public_key();

        // A subsequent message — even BACKDATED before the tombstone — is dropped (no created_at gate).
        let backdated = super::super::envelope::seal_message(
            &Keys::generate(), &channel.key, &channel.id, channel.epoch, "ghost", 1,
        ).unwrap();
        let mut state = crate::state::ChatState::new();
        assert!(super::super::inbound::process_incoming(&mut state, &backdated, &channel, &me).is_none(),
            "a backdated message after the seal is dropped (binary seal, no timestamp test)");

        // A subsequent control edition does not advance the fold either (it short-circuits on the flag).
        publish_tombstone(&relay, &sealed, &owner, 2000).await;
        assert_eq!(fetch_and_apply_control(&relay, &sealed).await.unwrap(), 0,
            "control fold stops advancing once sealed");
    }

    #[tokio::test]
    async fn dissolve_community_emits_no_rekey_and_no_epoch_bump() {
        let (_tmp, _guard) = init_test_db();
        let relay = RekeyCountingRelay::new();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        // Mint a public link so the link-retire path actually runs (and must NOT privatize-rekey).
        create_public_invite(&relay, &community, None, None).await.unwrap();
        let before_epoch = crate::db::community::load_community(&community.id).unwrap().unwrap().server_root_epoch;

        dissolve_community(&relay, &community).await.unwrap();

        assert!(crate::db::community::get_community_dissolved(&cid).unwrap(), "sealed locally");
        assert_eq!(relay.rekeys.load(std::sync::atomic::Ordering::Relaxed), 0,
            "dissolution publishes NO 3303 rekey (no last-link privatize re-founding)");
        assert_eq!(crate::db::community::load_community(&community.id).unwrap().unwrap().server_root_epoch, before_epoch,
            "base epoch unchanged — dissolution rotates nothing");
    }

    #[tokio::test]
    async fn duplicate_owner_tombstones_are_idempotent() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        // Two owner tombstones (distinct created_at → distinct inner ids) at the locator.
        publish_tombstone(&relay, &community, &owner, 1000).await;
        publish_tombstone(&relay, &community, &owner, 2000).await;

        fetch_and_apply_control(&relay, &community).await.unwrap();
        assert!(crate::db::community::get_community_dissolved(&cid).unwrap(), "duplicates still just dissolve, no error");
        // A second fold over the same plane is a harmless no-op (already sealed).
        assert_eq!(fetch_and_apply_control(&relay, &community).await.unwrap(), 0);
    }

    #[test]
    fn apply_server_root_rekey_refuses_once_dissolved() {
        let (_tmp, _guard) = init_test_db();
        let owner = Keys::generate();
        let me = Keys::generate();
        become_local(&me);
        let community = saved_community_owned_by(&owner);
        let cid = community.id.to_hex();
        crate::db::community::set_community_dissolved(&cid).unwrap();

        let parsed = owner_base_rekey(&owner, &community, &me.public_key(), 1, &[0xCDu8; 32]);
        assert!(apply_server_root_rekey(&community, &parsed).is_err(),
            "a base rekey cannot cross a tombstone");
        assert_eq!(crate::db::community::load_community(&community.id).unwrap().unwrap().server_root_epoch,
            crate::community::Epoch(0), "base epoch did not advance");
    }

    #[tokio::test]
    async fn tombstone_detected_after_a_base_rotation() {
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        // Re-found the base (epoch 0 → 1); the dissolved locator is rotation-STABLE, so a tombstone
        // published AFTER the rotation (sealed under the new root) is still found by a post-rotation client.
        rotate_server_root(&relay, &community, &[owner.public_key()]).await.unwrap();
        let rotated = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(rotated.server_root_epoch, crate::community::Epoch(1));
        publish_tombstone(&relay, &rotated, &owner, 1000).await;

        fetch_and_apply_control(&relay, &rotated).await.unwrap();
        assert!(crate::db::community::get_community_dissolved(&cid).unwrap(),
            "tombstone at the rotation-stable locator is detected post-rotation");
    }

    #[tokio::test]
    async fn stable_coordinate_tombstone_survives_a_concurrent_rotation() {
        // Cross-epoch: a tombstone published ONLY at the rotation-stable coordinate is
        // discovered by a client that has since advanced to a LATER epoch — whose control_pseudonym differs,
        // so the tombstone is NOT in that epoch's control fold. Only the stable-coordinate probe can find it.
        // This is the case a concurrent re-founding creates (tombstone at epoch N, joiner on epoch N+1).
        let (_tmp, _guard) = init_test_db();
        let relay = MemoryRelay::new();
        let owner = crate::state::MY_SECRET_KEY.to_keys().unwrap();
        let community = create_community(&relay, "HQ", "general", vec!["r1".into()]).await.unwrap();
        let cid = community.id.to_hex();
        // Owner publishes the tombstone ONLY at the stable coordinate (no control_pseudonym copy).
        let inner = crate::community::roster::build_group_dissolved_edition_unsigned(owner.public_key(), &community.id, 1000)
            .sign_with_keys(&owner).unwrap();
        let stable = crate::community::roster::seal_dissolved_edition(&Keys::generate(), &inner, &community.id).unwrap();
        relay.inject(&stable, &community.relays);
        // Advance the base epoch (the local client hasn't folded the tombstone yet, so rotation is allowed —
        // exactly the concurrent-re-founder's state). The control_pseudonym now differs from epoch 0's.
        rotate_server_root(&relay, &community, &[owner.public_key()]).await.unwrap();
        let rotated = crate::db::community::load_community(&community.id).unwrap().unwrap();
        assert_eq!(rotated.server_root_epoch, crate::community::Epoch(1));
        assert!(!crate::db::community::get_community_dissolved(&cid).unwrap(), "not folded yet");
        // Fetch control at the NEW epoch: the tombstone is absent from this control_pseudonym; only the
        // stable-coordinate probe can surface it.
        fetch_and_apply_control(&relay, &rotated).await.unwrap();
        assert!(crate::db::community::get_community_dissolved(&cid).unwrap(),
            "stable-coordinate probe discovers the tombstone cross-epoch (C3 closed)");
    }
}
