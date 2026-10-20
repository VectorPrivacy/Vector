//! Community Tauri commands
//!
//! Thin wrappers over vector-core's Community logic. The invite carrier rides a NIP-17
//! gift-wrapped DM to the invitee's npub (the transport): the bundle conveys read keys
//! (server-root + channel keys), never write authority — that is the recipient's roster rank
//! Inbound invites are PARKED for explicit consent (no auto-join, no relay connect);
//! the user accepts or declines via the commands below.

use std::time::Duration;

use nostr_sdk::prelude::ToBech32;
use vector_core::community::v2::invite::PublicInvitePreview;
use vector_core::community::transport::LiveTransport;
use vector_core::community::CommunityId;
use vector_core::sending::{FileSource, Sealed};
#[cfg(debug_assertions)]
use vector_core::ClientRelayExt;

/// UI summary of a Community + its channels (no secrets). `is_owner` gates the
/// edit/invite affordances; `has_icon` tells the frontend whether to call
/// `cache_community_image` to resolve the logo.
#[derive(serde::Serialize)]
pub struct CommunitySummary {
    pub community_id: String,
    pub name: String,
    pub description: Option<String>,
    pub is_owner: bool,
    pub has_icon: bool,
    pub channels: Vec<ChannelSummary>,
    /// The primary channel's id, stamped onto every grafted chat row so the list renders
    /// one row per community. Without it each channel claims primary until the next boot.
    pub primary_channel: Option<String>,
    /// The PROVEN owner's npub (bech32), derived by verifying the owner attestation — `None` if
    /// absent/unverifiable. The frontend crowns + hoists this npub; it's never an unchecked claim.
    pub owner_npub: Option<String>,
    /// True once a valid owner GroupDissolved tombstone has sealed the community. The frontend
    /// renders the end-of-community marker, disables the composer, and offers a local Remove.
    pub dissolved: bool,
    /// The relays hosting the community, for the settings view.
    pub relays: Vec<String>,
    /// The iroh relays its voice channels use; empty is iroh's defaults.
    pub voice_relays: Vec<String>,
    /// The categories in sidebar order; `channels` is in sidebar order too.
    pub categories: serde_json::Value,
}

#[derive(serde::Serialize)]
pub struct ChannelSummary {
    pub channel_id: String,
    pub name: String,
    /// CORD-03 private channel.
    pub private: bool,
    /// Whether we hold this channel's key. A private channel we are not (yet)
    /// granted reads false — the UI must not default to one, since it renders
    /// empty and refuses every send.
    pub readable: bool,
    /// The category it shows under, if that category stands.
    pub category_id: Option<String>,
    pub topic: Option<String>,
    /// `chat`, `forum` or `voice`.
    pub view: &'static str,
    /// What the local user may do here, as a decimal string; absent on the legacy roster.
    pub my_permissions: Option<String>,
    /// Posts land at a writer address (CORD-03 §1).
    pub restricted: bool,
    /// This device holds what posting here takes now.
    pub can_post: bool,
    /// When a Channel made private stopped being public (unix ms): posts before it were public.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_until: Option<u64>,
}

/// The [`CommunitySummary`] for a **v2** community — the owner is the
/// self-certifying `community_id` commitment, and a public
/// v2 channel has no independent key.
fn summarize_v2(c: &vector_core::community::v2::community::CommunityV2) -> CommunitySummary {
    use nostr_sdk::prelude::ToBech32;
    let me = vector_core::my_public_key();
    let owner = c.owner().ok();
    let mine = vector_core::VectorCore::my_channel_permissions(c);
    CommunitySummary {
        community_id: vector_core::simd::hex::bytes_to_hex_32(&c.identity.community_id.0),
        name: c.name.clone(),
        description: c.description.clone(),
        is_owner: matches!((me, owner), (Some(m), Some(o)) if m == o),
        has_icon: c.icon.is_some(),
        channels: c
            .layout()
            .1
            .into_iter()
            .map(|(ch, cat)| ChannelSummary {
                channel_id: vector_core::simd::hex::bytes_to_hex_32(&ch.id.0),
                name: ch.name.clone(),
                private: ch.private,
                readable: !ch.private || ch.key.is_some(),
                category_id: cat.map(|k| vector_core::simd::hex::bytes_to_hex_32(&k.id)),
                topic: ch.topic().map(String::from),
                view: ch.view().as_str(),
                my_permissions: mine.get(&ch.id.0).map(|p| p.to_string()),
                restricted: ch.restricted(),
                can_post: vector_core::community::v2::service::can_post(c, &ch.id),
                public_until: ch.public_until(),
            })
            .collect(),
        categories: vector_core::VectorCore::categories_json(c),
        primary_channel: c.primary_channel().map(|ch| vector_core::simd::hex::bytes_to_hex_32(&ch.id.0)),
        owner_npub: owner.and_then(|pk| pk.to_bech32().ok()),
        dissolved: c.dissolved,
        relays: c.relays.clone(),
        voice_relays: c.voice_relays(),
    }
}

/// Protocol-aware summary by community id — the shared read path for
/// list/get/join surfaces.
fn summarize_any(id: &CommunityId) -> Option<CommunitySummary> {
    vector_core::db::community::load_community_v2(id).ok().flatten().map(|c| summarize_v2(&c))
}

/// List every Community the local user holds (owned or joined), for the chat list.
#[tauri::command]
pub async fn list_communities() -> Result<Vec<CommunitySummary>, String> {
    let ids = vector_core::db::community::list_community_ids()?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(summary) = summarize_any(&id) {
            out.push(summary);
        }
    }
    Ok(out)
}

/// A best-effort member entry: an observed participant (someone who has posted) + their
/// most-recent activity. Membership is not authoritative (§ no roster); see community.rs.
#[derive(serde::Serialize, Clone)]
pub struct CommunityMember {
    pub npub: String,
    pub last_active: u64,
}

/// Observed participants of a Community: distinct authors seen across its channels, newest
/// activity first. The frontend resolves each npub's profile (name/avatar) for display.
#[tauri::command]
pub async fn get_community_members(community_id: String) -> Result<Vec<CommunityMember>, String> {
    // v2 memberlist = the persisted guestbook fold ∪ observed authors ∪ granted
    // roster − banlist, all LOCAL (seeded post-join, cursor-caught-up at boot,
    // live-ingested) — the facade read is instant.
    Ok(vector_core::VectorCore
        .get_community_members(&community_id)
        .await
        .into_iter()
        .filter_map(|m| m.get("npub").and_then(|n| n.as_str()).map(String::from))
        .map(|npub| CommunityMember { npub, last_active: 0 })
        .collect())
}

/// The `/` picker snapshot for a chat: its bot members' last-known command
/// manifests, served instantly from local state. When stale, the backend
/// re-fetches in the background and emits `chat_commands_updated`.
#[tauri::command]
pub async fn get_chat_commands(chat_id: String) -> Result<serde_json::Value, String> {
    let snapshot = vector_core::VectorCore.get_chat_commands(&chat_id).await;
    serde_json::to_value(snapshot).map_err(|e| e.to_string())
}

/// Ban a member: add their npub to the Community banlist and republish it. Owner-only (enforced
/// by `publish_banlist`). Honest clients then drop ALL of that npub's events, presence included.
#[tauri::command]
pub async fn ban_community_member(community_id: String, npub: String) -> Result<(), String> {
    set_member_banned(&community_id, &npub, true).await
}

/// Unban a member: remove their npub from the banlist and republish.
#[tauri::command]
pub async fn unban_community_member(community_id: String, npub: String) -> Result<(), String> {
    set_member_banned(&community_id, &npub, false).await
}

/// Owner dissolution / "Delete Community": publish the terminal GroupDissolved tombstone at the
/// rotation-stable coordinate (and best-effort retire the owner's own invite links, no rekey), then
/// tear the community down locally — the DISSOLVER forgets it entirely (members fold the tombstone
/// and see the sealed husk instead). OWNER-ONLY (enforced in vector-core). The frontend gates the
/// button on ownership + a type-to-confirm; this re-verifies authority cryptographically.
#[tauri::command]
pub async fn delete_community(community_id: String) -> Result<(), String> {
    vector_core::db::scoped(async move {
        hex_to_id32(&community_id)?;
        // Dissolve at the community_id-derived plane (CORD-02 §9) via the facade, then the same
        // local teardown so no sealed husk lingers.
        let channel_ids = community_channel_ids(&community_id);
        if is_held(&community_id) && !vector_core::db::community::get_community_dissolved(&community_id).unwrap_or(false) {
            vector_core::VectorCore.dissolve_community(&community_id).await.map_err(|e| e.to_string())?;
        }
        teardown_community_local(&community_id, &channel_ids).await;
        Ok(())
    })
    .await
}

/// Kick a member: publish a cooperative kick (3309) into the primary channel. The kicked client
/// self-removes on receipt; peers drop them from the member list. NOT a rekey — a malicious member who
/// ignores the kick is BANNED instead. Requires the caller hold `KICK` and outrank the target (enforced
/// in `publish_kick`, re-verified by peers).
#[tauri::command]
pub async fn kick_community_member(community_id: String, npub: String) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.kick_member(&community_id, &npub).await.map_err(|e| e.to_string())
    })
    .await
}

/// The Community's current banlist as npubs (bech32), for the owner's manage-bans UI.
#[tauri::command]
pub async fn get_community_banlist(community_id: String) -> Result<Vec<String>, String> {
    let hexes = vector_core::db::community::get_community_banlist(&community_id)?;
    Ok(hexes
        .iter()
        .filter_map(|h| nostr_sdk::prelude::PublicKey::from_hex(h).ok().and_then(|pk| pk.to_bech32().ok()))
        .collect())
}

/// Check each relay a move to `relays` would add: whether it holds what a Community needs, and
/// whether it throttles. Changes nothing.
#[tauri::command]
pub async fn check_community_relays(community_id: String, relays: Vec<String>) -> Result<Vec<vector_core::community::v2::service::RelayCheck>, String> {
    vector_core::db::scoped(async move {
        let checks = vector_core::VectorCore.check_community_relays(&community_id, &relays).await.map_err(|e| e.to_string())?;
        if !vector_core::db::session_is_live() {
            return Err("the account changed while the relays were checked".to_string());
        }
        Ok(checks)
    })
    .await
}

/// Move the Community to `relays`: its history is copied onto every added relay before the new
/// list is announced, on the old relays and the new.
#[tauri::command]
pub async fn set_community_relays(community_id: String, relays: Vec<String>) -> Result<vector_core::community::v2::service::RelayMove, String> {
    vector_core::db::scoped(async move {
        let moved = vector_core::VectorCore.move_community_relays(&community_id, &relays).await.map_err(|e| e.to_string())?;
        if !vector_core::db::session_is_live() {
            return Err("the account changed during the move".to_string());
        }
        Ok(moved)
    })
    .await
}

/// The Community's Audit Log, newest first: who changed what, and when.
#[tauri::command]
pub async fn get_community_audit_log(community_id: String) -> Result<vector_core::community::v2::service::AuditLog, String> {
    vector_core::db::scoped(async move {
        let log = vector_core::VectorCore.community_audit_log(&community_id).await.map_err(|e| e.to_string())?;
        // Bound to the account that asked: a log finished after a swap belongs to no one on screen.
        if !vector_core::db::session_is_live() {
            return Err("the account changed while the audit log loaded".to_string());
        }
        Ok(log)
    })
    .await
}

/// Grant a member the Community's Admin role: publishes the per-member Grant event, adding them
/// to the roster at admin rank so they can sign management actions. Owner/admin-only.
#[tauri::command]
pub async fn grant_community_admin(community_id: String, npub: String) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.grant_admin(&community_id, &npub).await.map_err(|e| e.to_string())
    })
    .await
}

/// Revoke a member's Admin role (instant-logical revocation). Owner/admin-only.
#[tauri::command]
pub async fn revoke_community_admin(community_id: String, npub: String) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.revoke_admin(&community_id, &npub).await.map_err(|e| e.to_string())
    })
    .await
}

/// Every role a community defines, with its position and colour, plus who holds
/// which — what the member list groups on.
#[tauri::command]
pub fn get_community_role_graph(community_id: String) -> Result<serde_json::Value, String> {
    vector_core::VectorCore.community_role_graph(&community_id).map_err(|e| e.to_string())
}

/// The npubs (bech32) of members holding a MANAGEMENT role — the admin set, for the member-list
/// crown. (A member holding only a non-management/social role is not an admin.)
#[tauri::command]
pub fn get_community_admins(community_id: String) -> Result<Vec<String>, String> {
    let roles = vector_core::VectorCore.community_roles(&community_id).map_err(|e| e.to_string())?;
    Ok(roles
        .get("admins")
        .and_then(|a| a.as_array())
        .map(|arr| arr.iter().filter_map(|n| n.as_str().map(String::from)).collect())
        .unwrap_or_default())
}

/// Whether the local user may grant/revoke roles — holds the `MANAGE_ROLES` permission. Drives the
/// member-list crown toggle. Permission-based (the owner is just the uppermost role with every
/// permission), NOT a hardcoded owner check.
#[tauri::command]
pub fn can_manage_community_roles(community_id: String) -> Result<bool, String> {
    Ok(vector_core::VectorCore
        .community_capabilities(&community_id)
        .ok()
        .and_then(|c| c.get("manage_roles").and_then(|v| v.as_bool()))
        .unwrap_or(false))
}

/// The local user's effective management capabilities in a community (role engine — owner is just
/// position 0, NOTHING is owner-hardcoded). Drives which management affordances the UI shows: an admin
/// whose role carries a permission gets the same buttons as the owner. `manage_admin_role` is the crown's
/// gate (can grant/revoke the @admin role = outrank its position; owner-only in the single-role MVP, but
/// computed by the role engine, not hardcoded).
#[tauri::command]
pub fn get_community_capabilities(community_id: String) -> Result<serde_json::Value, String> {
    vector_core::VectorCore.community_capabilities(&community_id).map_err(|e| e.to_string())
}

/// The Community's invite-link state for the management UI: the computed mode (`is_public` = any live
/// link across ALL creators) plus a per-creator breakdown so the panel can show "X has N active
/// invite links". The mode here is the AUTHORITATIVE folded registry, not the local user's own links —
/// so a member sees "Public" whenever another admin's link is live, not just their own.
#[tauri::command]
pub fn get_community_invite_summary(community_id: String) -> Result<serde_json::Value, String> {
    let is_public = !vector_core::db::community::get_community_invite_registry(&community_id)?.is_empty();
    let sets = vector_core::db::community::get_invite_link_sets(&community_id)?;
    let creators: Vec<serde_json::Value> = sets
        .into_iter()
        .filter(|s| !s.locators.is_empty())
        .filter_map(|s| {
            let npub = nostr_sdk::prelude::PublicKey::from_hex(&s.creator_hex).ok()?.to_bech32().ok()?;
            Some(serde_json::json!({ "npub": npub, "count": s.locators.len() }))
        })
        .collect();
    Ok(serde_json::json!({ "is_public": is_public, "creators": creators }))
}

/// Whether `community_id` (hex) names a locally-held community.
fn is_held(community_id: &str) -> bool {
    hex_to_id32(community_id)
        .ok()
        .is_some_and(|b| vector_core::db::community::community_held(&CommunityId(b)).unwrap_or(false))
}

/// Re-run the v2 Community List sync: adopt anything newly listed, tear down anything a sibling
/// device left. Shared by the boot reconcile and the LIVE kind-33302 self-sync subscription, so a
/// join or leave on one device lands on the others without a reboot.
pub(crate) async fn ingest_v2_community_list_update() {
    use vector_core::community::{transport::LiveTransport, v2::service as v2};
    let bootstrap: Vec<String> = match vector_core::state::nostr_client() {
        Some(client) => client.relays().await.keys().map(|r| r.to_string()).collect(),
        None => return,
    };
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    let Ok(outcome) = v2::sync_community_list(&transport, &bootstrap).await else { return };
    // The fetches straddled awaits — never park account A's ids into account B's queue.
    // A sibling device LEFT: core dropped the community + channel rows, but the chat row, its
    // STATE entry and the live subscription are the shell's to clear. Skipping this leaves a
    // ghost room in the list — present, unopenable, "0 Members" — pointing at nothing.
    for (community_id, channel_ids) in &outcome.removed {
        teardown_community_records(community_id, channel_ids).await;
        vector_core::emit_event("community_kicked", &serde_json::json!({ "community_id": community_id }));
    }
    let joined = outcome.joined;
    // Joining retires any parked invite for that community — otherwise the list shows an
    // "accept/decline" row above the community you are already in, including one accepted on
    // another device.
    if matches!(vector_core::db::community::purge_pending_invites_for_held_communities(), Ok(n) if n > 0) {
        vector_core::emit_event("community_invites_purged", &serde_json::json!({}));
    }
    for c in &joined {
        vector_core::community::v2::realtime::enqueue_follow(c.id());
        // Tell the FRONTEND. Adoption registers the chat rows in
        // STATE + the DB, but the live UI only learns of a new community through this event — so
        // without it a community joined on another device is held, synced and invisible until the
        // next restart, which reads as the cross-device sync being broken when it isn't.
        vector_core::emit_event(
            "community_surfaced",
            &serde_json::to_value(summarize_v2(c)).unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(client) = vector_core::state::nostr_client() {
        vector_core::community::v2::realtime::refresh_subscription(&client).await;
    }
}

async fn set_member_banned(community_id: &str, npub: &str, banned: bool) -> Result<(), String> {
    vector_core::db::scoped(async move {
        // v2: banlist edition + grant-strip + refound (CORD-04 §6), all in the facade.
        vector_core::VectorCore.set_member_banned(community_id, npub, banned).await.map_err(|e| e.to_string())?;
        // Their messages stop rendering the moment they are banned, so a badge counting
        // them sends the reader hunting for something the client will never show. The
        // unread cache is maintained incrementally and cannot know a ban happened —
        // recompute every channel they could have posted in, straight from the DB.
        reconcile_community_unread(community_id).await;
        Ok(())
    })
    .await
}

/// Fetch one Community's summary (for the overview/settings panel).
#[tauri::command]
pub async fn get_community(community_id: String) -> Result<CommunitySummary, String> {
    let id_bytes = hex_to_id32(&community_id)?;
    summarize_any(&CommunityId(id_bytes)).ok_or_else(|| "Community not found".to_string())
}

/// Leave a Community: drop all local state (keys, channels, invites) and stop
/// subscribing. There is no protocol "leave" (membership is key possession), so this is
/// purely local. Also clears the channels' chat rows from STATE.
#[tauri::command]
pub async fn leave_community(community_id: String) -> Result<(), String> {
    vector_core::db::scoped(async move {
        hex_to_id32(&community_id)?;
        // Guestbook Leave announce (facade) + shared local teardown.
        let channel_ids = community_channel_ids(&community_id);
        // Never silent: no announce means no list tombstone, and the community
        // comes back on the next boot.
        if let Err(e) = vector_core::VectorCore.leave_community(&community_id).await {
            vector_core::log_net_fail!("[Leave] {} announced nothing ({})", &community_id[..8], e);
        }
        teardown_community_local(&community_id, &channel_ids).await;
        crate::commands::rail::forget_community(&community_id).await;
        Ok(())
    })
    .await
}

/// A community's channel-id hexes. Every teardown path needs these to purge the chat rows + STATE.
pub(crate) fn community_channel_ids(community_id: &str) -> Vec<String> {
    let Ok(id_bytes) = hex_to_id32(community_id) else { return Vec::new() };
    vector_core::db::community::load_community_v2(&CommunityId(id_bytes))
        .ok()
        .flatten()
        .map(|c| c.channels.iter().map(|ch| vector_core::simd::hex::bytes_to_hex_32(&ch.id.0)).collect())
        .unwrap_or_default()
}

/// A community's relay set.
fn community_relays_of(community_id: &str) -> Vec<String> {
    let Ok(id_bytes) = hex_to_id32(community_id) else { return Vec::new() };
    vector_core::db::community::load_community_v2(&CommunityId(id_bytes))
        .ok()
        .flatten()
        .map(|c| c.relays)
        .unwrap_or_default()
}

/// A removal this device decided (leave, delete): torn down and recorded
/// in the local tombstone mirror at now.
pub(crate) async fn teardown_community_local(community_id: &str, channel_ids: &[String]) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    vector_core::community::v2::service::note_local_removal(community_id, now);
    teardown_community_records(community_id, channel_ids).await
}

/// Tear down local state for a community: drop the DB rows + rebuild the subscription routes WITHOUT it,
/// then clear the channels' chat rows. RETAINS the held epoch keys so a later self-scrub of own past
/// messages stays possible. Order matters — drop + refresh routes BEFORE the chat-row teardown, else an
/// in-flight inbound message could route in and recreate a ghost chat after we'd deleted it.
///
/// Records nothing: a removal a sibling published is already mirrored, at its own time, by the List sync.
pub(crate) async fn teardown_community_records(community_id: &str, channel_ids: &[String]) {
    // Captured BEFORE deletion so the prune below can drop relays no remaining community needs.
    let left_relays: Vec<String> = community_relays_of(community_id);
    // A row deleted before teardown takes its channel rows with it; the rooms still name it.
    let mut channel_ids = channel_ids.to_vec();
    for chat in vector_core::state::STATE.lock().await.chats.iter() {
        if chat.metadata.community_id() == Some(community_id)
            && !channel_ids.contains(&chat.id)
        {
            channel_ids.push(chat.id.clone());
        }
    }

    let _ = vector_core::db::community::delete_community_retain_keys(community_id);
    crate::services::subscription_handler::refresh_community_subscription().await;
    {
        let mut state = vector_core::state::STATE.lock().await;
        state.chats.retain(|c| !channel_ids.contains(&c.id));
    }
    for cid in &channel_ids {
        let _ = vector_core::db::chats::delete_chat(cid);
    }

    // Drop the left community's now-orphaned relays from the pool (bounds growth).
    prune_orphaned_community_relays(&left_relays).await;
}

/// Disconnect + remove relays that belonged to a just-left community, but ONLY the ones nothing
/// else needs. Three protections, any of which keeps a relay:
/// 1. another community still lists it (`still_needed`);
/// 2. the user reads/writes it — their own primary/imported relay, or a relay that's BOTH theirs
///    and a community's (READ/WRITE flag set; community relays are GOSSIP|PING — see
///    `community_relay_options`). DM recipient inbox relays are added READ+WRITE too, so this
///    also shields any transient chat relay;
/// 3. it's a NIP-65 GOSSIP relay (the pool itself refuses to remove those).
///
/// So leaving a community can never sever the user's own connectivity or another chat's relays.
/// Delegates to the shared vector-core prune (same keep-set logic also used by the invite-preload
/// TTL cleanup, #297) so the two paths can't drift.
async fn prune_orphaned_community_relays(left_relays: &[String]) {
    vector_core::community::transport::prune_unneeded_community_relays(left_relays).await;
}

/// Involuntary self-removal from a community — a cooperative KICK (3309) targeting us, OR detecting our
/// own npub in the BANLIST. Resolve the channels, tear down all local state (no voluntary "left"
/// announce — it's involuntary), then tell the frontend so it silently closes the view. A ban differs
/// from a kick only in that the banlist persists (re-detected → re-removed) and admins can't re-invite us.
pub(crate) async fn self_remove_from_community(community_id: &str) {
    println!("[teardown] self_remove_from_community {}", &community_id[..8.min(community_id.len())]);
    let channel_ids = community_channel_ids(community_id);
    // Core recorded the removal at the evicting act's time.
    teardown_community_records(community_id, &channel_ids).await;
    vector_core::emit_event(
        "community_kicked",
        &serde_json::json!({ "community_id": community_id }),
    );
}


// ============================================================================
// Create + send (the core lifecycle)
// ============================================================================

/// Create a new single-channel Community owned by the local user. Defaults the channel
/// to "general" and the relay set to the active trusted relays. Persists + publishes
/// metadata, surfaces the channel locally, and starts the subscription. Returns the
/// `(community_id, channel_id)` hex pair.
#[tauri::command]
pub async fn create_community(
    name: String,
    channel_name: Option<String>,
    relays: Option<Vec<String>>,
) -> Result<CreatedCommunity, String> {
    let relays = match relays {
        Some(r) if !r.is_empty() => r,
        _ => vector_core::state::active_trusted_relays()
            .await
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };
    if relays.is_empty() {
        return Err("No relays available to host the Community".to_string());
    }
    let channel_name = channel_name.unwrap_or_else(|| "general".to_string());

    let session = vector_core::db::current_session();
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    let mut community =
        vector_core::community::v2::service::create_community_on_roles_v2(&transport, &name, relays, None).await?;

    // Genesis mints the first channel as "general"; honor a custom name.
    if channel_name != "general" {
        if let Some(ch) = community.channels.first().cloned() {
            // Rebuild from the held document — a rename must never strip vsk-2
            // fields it didn't touch (CORD-02 §6).
            let mut meta = ch.metadata();
            meta.name = channel_name.clone();
            vector_core::community::v2::service::edit_channel_metadata(&transport, &community, &ch.id, &meta).await?;
            if let Ok(Some(fresh)) = vector_core::db::community::load_community_v2(community.id()) {
                community = fresh;
            }
        }
    }

    let community_id = vector_core::simd::hex::bytes_to_hex_32(&community.id().0);
    let channel_id = community
        .channels
        .first()
        .map(|ch| vector_core::simd::hex::bytes_to_hex_32(&ch.id.0))
        .ok_or("created community has no channel")?;
    // Surface the channel chat row(s) like any DM. (The v2 service already
    // published the cross-device Community List during create.)
    vector_core::VectorCore.register_v2_chats(&community, &session).await;
    // Start receiving on the new planes.
    if let Some(client) = crate::nostr_client() {
        vector_core::community::v2::realtime::refresh_subscription(&client).await;
    }

    let owner_npub = community.owner().ok().and_then(|pk| pk.to_bech32().ok());
    Ok(CreatedCommunity {
        community_id,
        channel_id,
        owner_npub,
        channel_name,
    })
}

#[derive(serde::Serialize)]
pub struct CreatedCommunity {
    pub community_id: String,
    pub channel_id: String,
    pub owner_npub: Option<String>,
    /// The first channel's name, so the chat the UI grafts before any reload carries the
    /// same stamp `upsert_community_chat` persists.
    pub channel_name: String,
}

/// Publish an ephemeral typing indicator into a Community channel. Best-effort fire-and-forget — a
/// dropped keystroke ping is harmless. `channel_id` is the channel hex id (the open-chat id the
/// frontend already hands `start_typing`). Returns false if it isn't a known Community channel.
pub(crate) async fn send_community_typing(channel_id: &str) -> bool {
    if !matches!(vector_core::db::community::community_id_for_channel(channel_id), Ok(Some(_))) {
        return false;
    }
    vector_core::VectorCore.send_community_typing(channel_id).await.is_ok()
}

/// Post a text message to a Community channel (addressed by its `channel_id`). Drives the
/// same pending → sent/failed lifecycle as DMs (via `TauriSendCallback`): an optimistic
/// message renders instantly, then flips to sent on relay ACK or failed (with retry) on
/// error. Authorship is signed through the active signer (local OR bunker).
#[tauri::command]
pub async fn send_community_message(
    channel_id: String,
    content: String,
    replied_to: Option<String>,
    bot: Option<String>,
) -> Result<(), String> {
    vector_core::db::scoped(async move {
        use vector_core::sending::SendCallback;
        use vector_core::Message;
        let reply = replied_to.filter(|r| !r.is_empty());
        let community_id = vector_core::db::community::community_id_for_channel(&channel_id)?
            .ok_or("Unknown Community channel")?;
        let (content, role_mentions) = vector_core::community::mentions::resolve_in(&community_id, &content);
        let (content, text_spans) = vector_core::text_spans::extract(&content);
        let span_tags: Vec<nostr_sdk::prelude::Tag> = vector_core::text_spans::to_nostr_tags(&text_spans)
            .chain(vector_core::community::mentions::to_tags(&role_mentions))
            .collect();
        // A `/` picker send names its chosen bot so only that bot executes when two
        // bots share a command name (untagged = broadcast). The tag rides the
        // signed rumor, and the sender's own optimistic row carries the
        // npub too — the passive "ran /cmd with Bot" render can't wait for an echo.
        let bot_pk = bot
            .as_deref()
            .filter(|b| !b.is_empty())
            .and_then(|b| nostr_sdk::prelude::PublicKey::parse(b).ok());
        let bot_tags: Vec<nostr_sdk::prelude::Tag> =
            bot_pk.map(|pk| vec![vector_core::bot_interface::bot_tag(&pk)]).unwrap_or_default();
        let addressed_bots: Vec<String> = bot_pk
            .and_then(|pk| nostr_sdk::prelude::ToBech32::to_bech32(&pk).ok())
            .into_iter()
            .collect();

        let author_pk = vector_core::my_public_key().ok_or("Public key not set")?;
        let my_npub = author_pk.to_bech32().ok();

        let id_bytes = hex_to_id32(&community_id)?;

        // The same pending → sent/failed lifecycle as DMs. The rumor id is a pure function
        // of its inputs (at_ms included), so the optimistic row is keyed by its REAL id up
        // front and the in-process echo dedups onto it — the id never changes.
        let community = vector_core::db::community::load_community_v2(&CommunityId(id_bytes))?
            .ok_or("Community not found")?;
        let ch = vector_core::community::ChannelId(hex_to_id32(&channel_id)?);
        let channel = community.channel(&ch).ok_or("Channel not found in Community")?;
        let (_, epoch) = community.channel_secret(channel);
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let callback = crate::message::sending::TauriSendCallback;
        // NIP-30 pairs + the NIP-C7 reply author (best-effort from the held parent).
        let emoji_tags = vector_core::emoji_packs::resolve_outbound_emoji_tags(&content);
        let emoji_pairs: Vec<(&str, &str)> = emoji_tags.iter().map(|t| (t.shortcode.as_str(), t.url.as_str())).collect();
        let reply_owned = match reply.as_deref() {
            Some(parent_id) => {
                let author_hex = {
                    let st = vector_core::state::STATE.lock().await;
                    st.find_message(parent_id)
                        .and_then(|(_, m)| m.npub.as_deref().and_then(|n| nostr_sdk::prelude::PublicKey::parse(n).ok()))
                        .map(|pk| pk.to_hex())
                        .unwrap_or_default()
                };
                Some((parent_id.to_string(), author_hex))
            }
            None => None,
        };
        let reply_ref = reply_owned.as_ref().map(|(id, a)| (id.as_str(), a.as_str()));
        // Self-Destruct Timer: stamp the sender's per-channel NIP-40 expiry (if
        // set) so relays drop the wrap and every member's client purges on
        // schedule. Resolved ONCE and passed to both the precompute and the send
        // so the pure-function rumor id can't fork.
        let mut extra_tags = bot_tags;
        extra_tags.extend(span_tags.iter().cloned());
        // Resolve the Self-Destruct expiry ONCE: it stamps both the outgoing
        // rumor (so recipients + relays honour NIP-40) AND the optimistic message
        // below, so the sender's own echo self-destructs like everyone else's.
        let expiry = vector_core::self_destruct::resolve_send_expiry(&channel_id);
        if let Some(exp) = expiry {
            extra_tags.push(nostr_sdk::prelude::Tag::expiration(nostr_sdk::prelude::Timestamp::from_secs(exp)));
        }
        let rumor = vector_core::community::v2::chat::build_message_rumor(
            author_pk, &ch, epoch, &content, reply_ref, &emoji_pairs, extra_tags.clone(), ms,
        );
        let rumor = vector_core::community::v2::service::outbound_rumor(&community, &ch, rumor);
        let message_id = rumor.id.ok_or("inner rumor has no id")?.to_hex();

        // 1. Optimistic message — renders instantly, keyed by its real id.
        let mut pending_msg = Message {
            id: message_id.clone(),
            content: content.clone(),
            at: ms,
            pending: true,
            mine: true,
            npub: my_npub.clone(),
            replied_to: reply.clone().unwrap_or_default(),
            emoji_tags: emoji_tags.clone(),
            addressed_bots: addressed_bots.clone(),
            text_spans: text_spans.clone(),
            role_mentions,
            expiration: expiry,
            ..Default::default()
        };
        // The quote rides the message itself: the parent may be nowhere in the UI's memory.
        let _ = vector_core::db::events::populate_reply_context(&mut pending_msg).await;
        {
            let mut state = vector_core::state::STATE.lock().await;
            state.add_message_to_chat(&channel_id, &pending_msg);
        }
        callback.on_pending(&channel_id, &pending_msg);

        // 2. Seal + publish — the service re-derives the identical rumor from `ms`.
        let transport = LiveTransport::with_timeout(Duration::from_secs(12));
        // Same tags as the precomputed rumor above — the rumor id is a pure
        // function of its inputs, so any divergence would fork the optimistic id.
        let sent = vector_core::community::v2::service::send_chat_message_at(
            &transport, &community, &ch, &content, reply_ref, &emoji_pairs, extra_tags, ms,
        )
        .await;
        match sent {
            Ok(sent_id) => {
                if sent_id != message_id {
                    // An epoch rolled between the precompute and the seal — the send
                    // landed under a NEW id. Drop the orphaned pending row and adopt
                    // the echo (persisted under sent_id).
                    let adopted = {
                        let mut state = vector_core::state::STATE.lock().await;
                        state.remove_message(&message_id);
                        state.find_message(&sent_id).map(|(_, m)| m.clone())
                    };
                    if let Some(ref msg) = adopted {
                        callback.on_sent(&channel_id, &message_id, msg);
                        callback.on_persist(&channel_id, msg);
                    }
                    return Ok(());
                }
                // 3a. Sent — clear the pending flag (the echo deduped onto this row).
                let sent_row = {
                    let mut state = vector_core::state::STATE.lock().await;
                    state.update_message(&message_id, |m| m.set_pending(false))
                };
                if let Some((_cid, ref msg)) = sent_row {
                    callback.on_sent(&channel_id, &message_id, msg);
                    callback.on_persist(&channel_id, msg);
                }
                Ok(())
            }
            Err(e) => {
                // 3b. Failed — mark the optimistic message failed (offers retry in the UI).
                let failed = {
                    let mut state = vector_core::state::STATE.lock().await;
                    state.update_message(&message_id, |m| {
                        m.set_failed(true);
                        m.set_pending(false);
                    })
                };
                if let Some((_cid, ref msg)) = failed {
                    callback.on_failed(&channel_id, &message_id, msg);
                }
                Err(e)
            }
        }
    })
    .await
}

/// A Community attachment that's been encrypted and previewed locally but NOT yet uploaded.
/// The upload is deferred into [`dispatch_community_attachment_message`] so the optimistic
/// bubble (with the progress ring + cancel button) shows BEFORE the bytes hit the network —
/// parity with DM file sends.
struct PreparedCommunityAttachment {
    /// Optimistic attachment — `url` is empty until the upload completes; the plaintext is
    /// already on disk so the sender previews it instantly. Its key and nonce are the
    /// ones the upload will seal with, unless a live blob of these bytes is reused.
    attachment: vector_core::types::Attachment,
    /// What gets sealed once the bubble is up: bytes in hand, or the local copy on disk.
    plain: FileSource,
    /// Original MIME (servers reject `application/octet-stream` but accept the same bytes
    /// under their real type) — used for capability-aware server routing.
    mime: String,
}

/// Encrypt a single outbound file (read from disk) for a Community message.
/// Thin wrapper over [`process_outbound_community_attachment_bytes`]. `name_override` (a
/// full filename, e.g. `SPOILER_photo.png` or an edited name) wins when non-empty — this is
/// how spoiler + rename reach the attachment's `name` (parity with DM `file_message`);
/// otherwise the on-disk filename is used.
async fn process_outbound_community_attachment(
    file_path: &str,
    name_override: &str,
    use_compression: bool,
    keep_metadata: bool,
) -> Result<PreparedCommunityAttachment, String> {
    // Android's file dialog hands back a content:// URI, which std::fs can't open and which
    // has no usable filename in its last path segment — the content resolver holds both the
    // bytes and the real display name. Same routing the community image picker already uses.
    #[cfg(target_os = "android")]
    if file_path.starts_with("content://") {
        // Neither the preview's byte copy nor its pre-compression is used from here.
        crate::message::types::ANDROID_FILE_CACHE.lock().unwrap().remove(file_path);
        crate::message::files::take_precompressed(file_path, false).await;
        // A pick copied to disk at selection streams from that copy, which goes once sealed.
        let picked = crate::message::types::ANDROID_PICKED_FILES.lock().unwrap().get(file_path).cloned();
        if let Some((copy, _, cached_name, _)) = picked {
            let name = if !name_override.is_empty() { name_override.to_string() } else { cached_name };
            let prepared = process_outbound_community_attachment_path(copy, name).await;
            crate::message::types::discard_picked_file(file_path);
            return prepared;
        }
        let af = crate::android::filesystem::read_android_uri(file_path.to_string())
            .map_err(|e| format!("read attachment: {e}"))?;
        let mut name = if !name_override.is_empty() {
            name_override.to_string()
        } else {
            af.name.clone()
        };
        // The bytes path derives the extension from the filename, but the resolver's
        // display-name query can come back empty or bare (its own notes call out
        // provider URIs like Google Photos), which would leave the attachment
        // extensionless. `af.extension` is MIME-derived and survives that.
        if std::path::Path::new(&name).extension().is_none() && !af.extension.is_empty() {
            let stem = if name.is_empty() { "file" } else { name.as_str() };
            name = format!("{stem}.{}", af.extension);
        }
        return process_outbound_community_attachment_bytes(
            (*af.bytes).clone(),
            &name,
            use_compression,
            keep_metadata,
            None,
        )
        .await;
    }

    let name = if !name_override.is_empty() {
        name_override.to_string()
    } else {
        std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string()
    };
    let extension = std::path::Path::new(&name).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    // Anything that isn't processed first streams from disk, whatever its size.
    if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico") {
        use crate::message::video_compression;
        if use_compression && video_compression::is_video_extension(&extension) {
            // The preview's encode, or one now; only a smaller result replaces the streamed original.
            let compressed = match crate::message::files::take_precompressed(file_path, true).await {
                Some(pc) => Some(pc),
                None => {
                    let path = file_path.to_string();
                    tokio::task::spawn_blocking(move || video_compression::compress_file(&path)).await.ok().and_then(Result::ok)
                }
            };
            if let Some(pc) = compressed.filter(|pc| pc.compressed_size < pc.original_size) {
                let name = video_compression::renamed(&name, &pc.extension);
                let bytes = std::sync::Arc::try_unwrap(pc.bytes).unwrap_or_else(|a| (*a).clone());
                return process_outbound_community_attachment_bytes(bytes, &name, false, keep_metadata, None).await;
            }
        } else {
            crate::message::files::take_precompressed(file_path, false).await;
        }
        return process_outbound_community_attachment_path(std::path::PathBuf::from(file_path), name).await;
    }

    // The preview already compressed this file (or is still at it): a compressed send
    // uses that result, a full-resolution one only its thumbhash.
    let precompressed = crate::message::files::take_precompressed(file_path, use_compression && !keep_metadata).await;
    let path = file_path.to_string();
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(path))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("read attachment: {e}"))?;
    process_outbound_community_attachment_bytes(bytes, &name, use_compression, keep_metadata, precompressed).await
}

/// Build the optimistic attachment for bytes ready to seal. Nothing touches the network
/// or seals here: both wait until the bubble is up, where they can say what they're doing.
fn stage_community_attachment(
    plain: FileSource,
    plain_len: u64,
    plaintext_hash: String,
    local_path: std::path::PathBuf,
    extension: String,
    name: String,
    img_meta: Option<vector_core::types::ImageMetadata>,
) -> PreparedCommunityAttachment {
    let params = vector_core::crypto::generate_encryption_params();
    let mime = vector_core::crypto::mime_from_extension(&extension).to_string();

    // Mini Apps: mint the realtime topic at send time so every member joins the
    // same gossip topic — it rides the imeta (see vector_core::webxdc).
    let webxdc_topic = extension.eq_ignore_ascii_case("xdc").then(|| {
        let sender = vector_core::state::my_public_key()
            .map(|pk| pk.to_hex())
            .unwrap_or_default();
        vector_core::webxdc::mint_topic_id(&plaintext_hash, &sender)
    });

    let attachment = vector_core::types::Attachment {
        id: plaintext_hash.clone(),
        key: params.key,
        nonce: params.nonce,
        extension,
        name,
        url: String::new(), // filled in once the upload completes, or by a reused blob
        path: local_path.to_string_lossy().to_string(),
        // AES-GCM appends a 16-byte tag, so the sealed size is known before sealing.
        size: plain_len + 16,
        img_meta,
        downloading: false,
        downloaded: true, // plaintext already on disk for the sender
        webxdc_topic,
        group_id: None,
        original_hash: Some(plaintext_hash),
        fallback_urls: Vec::new(), // filled by the post-upload mirror fan-out
    };
    PreparedCommunityAttachment { attachment, plain, mime }
}

/// Prepare a file that needs no processing straight from disk: hashed, copied into
/// the download folder and sealed file to file, so its size costs no memory.
async fn process_outbound_community_attachment_path(
    path: std::path::PathBuf,
    name: String,
) -> Result<PreparedCommunityAttachment, String> {
    let extension = std::path::Path::new(&name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
        .to_lowercase();
    let dir = vector_core::db::get_download_dir();
    let ext = extension.clone();
    let (plaintext_hash, local_path) = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let (hash, len) = vector_core::crypto::stream::hash_file(&path)?;
        if len == 0 {
            return Err("Empty attachment".to_string());
        }
        // The local copy, keyed by hash like the inbound path's: a file copy, a clone
        // where the filesystem has them.
        let _ = std::fs::create_dir_all(&dir);
        let local = dir.join(format!("{}.{}", hash, ext));
        // Reused only when it really is these bytes: an edited or truncated copy under the
        // hash's name would be sealed and sent as this file.
        let intact = std::fs::metadata(&local).is_ok_and(|m| m.len() == len)
            && vector_core::crypto::stream::hash_file(&local).is_ok_and(|(h, _)| h == hash);
        if !intact {
            // Named for this send: two sends of the same file must not share one.
            let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let tmp = dir.join(format!(".{}.{}.{}.tmp", hash, nonce, ext));
            std::fs::copy(&path, &tmp).map_err(|e| format!("read attachment: {e}"))?;
            if let Err(e) = std::fs::rename(&tmp, &local) {
                let _ = std::fs::remove_file(&tmp);
                return Err(format!("save attachment: {e}"));
            }
        }
        Ok((hash, local))
    })
    .await
    .map_err(|e| e.to_string())??;
    let plain_len = std::fs::metadata(&local_path).map(|m| m.len()).map_err(|e| format!("read attachment: {e}"))?;
    Ok(stage_community_attachment(FileSource::Path(local_path.clone()), plain_len, plaintext_hash, local_path, extension, name, None))
}

/// Encrypt a single outbound file (raw bytes + filename) for a Community message.
/// Returns the optimistic [`Attachment`] (with the plaintext saved locally so the sender
/// previews it instantly, `url` empty until upload) plus the ciphertext to upload. Uses the
/// NIP-17 attachment technique: a fresh per-file AES-GCM key+nonce, so the Blossom ciphertext
/// is only decryptable by members who open the event. Drives the bytes path (clipboard paste
/// / Android File object) — no on-disk source required.
async fn process_outbound_community_attachment_bytes(
    bytes: Vec<u8>,
    file_name: &str,
    use_compression: bool,
    keep_metadata: bool,
    precompressed: Option<crate::message::types::CachedCompressedImage>,
) -> Result<PreparedCommunityAttachment, String> {
    if bytes.is_empty() {
        return Err("Empty attachment".to_string());
    }
    let mut extension = std::path::Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
        .to_lowercase();
    let name = file_name.to_string();

    let is_image = matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico"
    );

    // Process image bytes per the compress + keep-metadata choice: strips EXIF
    // by default, keeps orientation, and re-attaches metadata when kept. GIFs and
    // non-images pass through untouched. Parity with DM sends. Hashing and the
    // local save ride the same blocking task: each is a full pass over the bytes.
    let dir = vector_core::db::get_download_dir();
    let (bytes, img_meta, extension, plaintext_hash, local_path) = tokio::task::spawn_blocking(move || {
        let (bytes, img_meta) = if is_image {
            let processed = crate::message::compression::process_image_for_send(
                std::sync::Arc::new(bytes), &extension, use_compression, keep_metadata, precompressed,
            )?;
            extension = processed.extension;
            (std::sync::Arc::try_unwrap(processed.bytes).unwrap_or_else(|a| (*a).clone()), processed.img_meta)
        } else {
            (bytes, None)
        };
        let plaintext_hash = vector_core::crypto::sha256_hex(&bytes);
        // Save the plaintext locally (keyed by hash, matching the inbound path convention)
        // so the sender's optimistic bubble renders immediately as a downloaded file.
        let _ = std::fs::create_dir_all(&dir);
        let local_path = dir.join(format!("{}.{}", plaintext_hash, extension));
        if !local_path.exists() {
            let _ = std::fs::write(&local_path, &bytes);
        }
        Ok::<_, String>((bytes, img_meta, extension, plaintext_hash, local_path))
    })
    .await
    .map_err(|e| e.to_string())??;

    let plain_len = bytes.len() as u64;
    Ok(stage_community_attachment(FileSource::Bytes(std::sync::Arc::new(bytes)), plain_len, plaintext_hash, local_path, extension, name, img_meta))
}

/// Post a Community message carrying a caption (`content`, may be empty) plus one or more
/// file attachments (read from disk) in a SINGLE event (the protocol's multi-attachment
/// capability — each file rides its own NIP-92 `imeta` tag). `name_overrides[i]` (a full
/// filename) overrides `file_paths[i]`'s display name when non-empty — carries spoiler
/// (`SPOILER_` prefix) and rename, matching DM file sends. A short/empty list = no override.
#[tauri::command]
pub async fn send_community_files(
    channel_id: String,
    content: String,
    file_paths: Vec<String>,
    name_overrides: Vec<String>,
    use_compression: bool,
    keep_metadata: bool,
    replied_to: Option<String>,
) -> Result<CommunityAttachmentSendResult, String> {
    if file_paths.is_empty() {
        return Err("No files to send".to_string());
    }
    // Capture session BEFORE the uploads so a mid-upload account swap is caught.
    let mut prepared = Vec::with_capacity(file_paths.len());
    for (i, fp) in file_paths.iter().enumerate() {
        let name_override = name_overrides.get(i).map(String::as_str).unwrap_or("");
        prepared.push(process_outbound_community_attachment(fp, name_override, use_compression, keep_metadata).await?);
    }
    dispatch_community_attachment_message(channel_id, content, replied_to, prepared).await
}

/// Send a voice note to a Community channel. Same upload path as a file, but the attachment's
/// `name` is blanked so the renderer treats it as a voice message (waveform + transcription) rather
/// than a named audio file — mirroring DM voice notes, which also carry an empty name. The WAV
/// extension survives via the imeta `m audio/wav` field, so the recipient reconstructs name=""/ext=wav.
pub(crate) async fn send_community_voice_bytes(
    channel_id: String,
    bytes: Vec<u8>,
    replied_to: Option<String>,
) -> Result<(), String> {
    let mut prepared = process_outbound_community_attachment_bytes(bytes, "voice-message.wav", false, false, None).await?;
    prepared.attachment.name = String::new();
    dispatch_community_attachment_message(channel_id, String::new(), replied_to, vec![prepared]).await.map(|_| ())
}

/// Send the JS-cached paste bytes (populated by `cache_file_bytes` on clipboard paste,
/// where the actual bytes live Rust-side and JS only holds a flag) as a Community
/// attachment. Mirrors the DM `send_cached_file` source, routed through the Community path.
#[tauri::command]
pub async fn send_community_cached_file(
    channel_id: String,
    content: String,
    name_override: Option<String>,
    use_compression: bool,
    keep_metadata: bool,
    replied_to: Option<String>,
) -> Result<(), String> {
    // The paste's background compression, if it finished.
    let precompressed = crate::message::files::JS_COMPRESSION_CACHE.lock().await.take();
    // Take ownership of the cached bytes + name + extension, clearing in one lock.
    let (bytes, cache_name, cache_ext) = {
        let mut cache = crate::message::files::JS_FILE_CACHE.lock().unwrap();
        match cache.take() {
            Some((b, n, e)) => ((*b).clone(), n, e),
            None => return Err("No cached file to send".to_string()),
        }
    };
    // A non-empty override (spoiler / rename) wins over the cached source name.
    let mut name = name_override.filter(|s| !s.is_empty()).unwrap_or(cache_name);
    // Ensure the name carries the cached extension: format detection (is_image,
    // EXIF strip) keys off the name here, and a clipboard name can lack one —
    // parity with the DM cached path, which uses the cached extension directly.
    if std::path::Path::new(&name).extension().is_none() && !cache_ext.is_empty() {
        name = format!("{}.{}", name, cache_ext);
    }
    let prepared = vec![process_outbound_community_attachment_bytes(bytes, &name, use_compression, keep_metadata, precompressed).await?];
    dispatch_community_attachment_message(channel_id, content, replied_to, prepared).await.map(|_| ())
}

/// Shared tail for the Community file-send commands: resolve the channel, show an optimistic
/// bubble FIRST (temp id), upload each attachment with progress + cancel, then build ONE inner
/// event carrying the caption + every attachment's `imeta`, sign, publish, and finalize the
/// temp id → real id. Mirrors the DM `send_file_dm` lifecycle so Communities get the same
/// progress ring / cancel button / instant preview. `prepared` is the encrypted-but-not-yet-
/// uploaded attachment set.
/// Returned by the community file-send commands. `webxdc_topic` is the realtime topic minted
/// for a `.xdc` attachment (None otherwise) — lets "Play & Invite" open the Mini App on the
/// exact message+topic it just sent without racing the optimistic-state events.
#[derive(serde::Serialize)]
pub struct CommunityAttachmentSendResult {
    pub message_id: String,
    pub webxdc_topic: Option<String>,
}

async fn dispatch_community_attachment_message(
    channel_id: String,
    content: String,
    replied_to: Option<String>,
    prepared: Vec<PreparedCommunityAttachment>,
) -> Result<CommunityAttachmentSendResult, String> {
    vector_core::db::scoped(async move {
        use vector_core::sending::SendCallback;
        use vector_core::Message;

        let reply = replied_to.filter(|r| !r.is_empty());
        let author_pk = vector_core::my_public_key().ok_or("Public key not set")?;
        let my_npub = author_pk.to_bech32().ok();

        // Resolve channel → owning Community (same as send_community_message).
        let community_id = vector_core::db::community::community_id_for_channel(&channel_id)?
            .ok_or("Unknown Community channel")?;
        // A caption names a Role in text only.
        let (content, _) = vector_core::community::mentions::resolve_in(&community_id, &content);
        let (content, text_spans) = vector_core::text_spans::extract(&content);
        let id_bytes = hex_to_id32(&community_id)?;
        // Resolved before any upload work.
        let community = vector_core::db::community::load_community_v2(&CommunityId(id_bytes))?
            .ok_or("Community not found")?;
        let ch = vector_core::community::ChannelId(hex_to_id32(&channel_id)?);
        if community.channel(&ch).is_none() {
            return Err("Channel not found in Community".to_string());
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let ms = now.as_millis() as u64;
        // Temp id keyed during upload — the real inner id depends on the imeta (uploaded URLs),
        // so it isn't known until every attachment lands. Finalized → real id on publish ack.
        let pending_id = format!("pending-{}", now.as_nanos());
        let callback = crate::message::sending::TauriSendCallback;
        let emoji_tags = vector_core::emoji_packs::resolve_outbound_emoji_tags(&content);

        // Self-Destruct Timer: the lifespan is resolved to a stamp after the upload, so
        // the clock starts at publish; the bubble shows no clock until the echo lands.
        let self_destruct_secs = vector_core::self_destruct::chat_duration_secs(&channel_id);
        // Optimistic bubble — attachments carry empty URLs (plaintext is already on disk for the
        // sender's preview); the upload fills them in.
        let optimistic_attachments: Vec<_> = prepared.iter().map(|p| p.attachment.clone()).collect();
        let mut pending_msg = Message {
            id: pending_id.clone(),
            content: content.clone(),
            at: ms,
            pending: true,
            mine: true,
            npub: my_npub.clone(),
            replied_to: reply.clone().unwrap_or_default(),
            emoji_tags: emoji_tags.clone(),
            text_spans: text_spans.clone(),
            attachments: optimistic_attachments,
            expiration: None,
            ..Default::default()
        };
        // The quote rides the message itself: the parent may be nowhere in the UI's memory.
        let _ = vector_core::db::events::populate_reply_context(&mut pending_msg).await;
        {
            let mut state = vector_core::state::STATE.lock().await;
            state.add_message_to_chat(&channel_id, &pending_msg);
        }
        // Registers the cancel flag (keyed by pending_id) + emits the pending bubble.
        callback.on_pending(&channel_id, &pending_msg);

        // Cancel flag the cancel_upload command flips — bridge it into Blossom so a cancel
        // between progress ticks still aborts the transfer.
        let cancel_flag = crate::message::upload_cancel_flags().lock().unwrap().get(&pending_id).cloned();

        let _client = vector_core::state::nostr_client().ok_or("Not logged in")?;
        let signer = vector_core::signer::active_signer().map_err(|e| format!("Signer unavailable: {e}"))?;
        let servers = vector_core::state::get_blossom_servers();
        if servers.is_empty() {
            let _ = mark_attachment_send_failed(&callback, &channel_id, &pending_id).await;
            return Err("No Blossom servers configured.".to_string());
        }

        // Upload each attachment, driving the progress ring (keyed by pending_id) + filling URLs.
        let mut uploaded: Vec<vector_core::types::Attachment> = Vec::with_capacity(prepared.len());
        for prep in prepared {
            let PreparedCommunityAttachment { mut attachment, plain, mime } = prep;
            let sealed = match vector_core::sending::seal_or_reuse(
                plain, &attachment.id, &attachment.key, &attachment.nonce, &pending_id,
                std::sync::Arc::new(callback), cancel_flag.clone(),
            ).await {
                Ok(sealed) => sealed,
                Err(e) => {
                    let _ = mark_attachment_send_failed(&callback, &channel_id, &pending_id).await;
                    return Err(e);
                }
            };
            // `_sealed_file` outlives the upload below, then takes its ciphertext file with it.
            let (encrypted, _sealed_file) = match sealed {
                // Smart-forward: the attachment now references a live blob — no
                // upload, no fan-out; the send is just the message. A foreign blob
                // gets a background mirror as availability insurance.
                Sealed::Reused(r) => {
                    let foreign = !r.mine;
                    (attachment.key, attachment.nonce, attachment.url, attachment.size) = (r.key, r.nonce, r.url, r.size);
                    {
                        let adopted = vector_core::compact::CompactAttachment::from_attachment(&attachment);
                        let mut state = vector_core::state::STATE.lock().await;
                        state.update_attachment(&channel_id, &pending_id, &attachment.id, |a| *a = adopted.clone());
                    }
                    callback.on_upload_complete(&channel_id, &pending_id, &attachment.id, &attachment.url);
                    if foreign {
                        let signer_bg = signer.clone();
                        let url_bg = attachment.url.clone();
                        vector_core::db::spawn_bound(async move {
                            let _ = vector_core::blossom::mirror_blob_to_servers(
                                signer_bg, &url_bg,
                                vector_core::state::get_blossom_servers(),
                                1, Duration::from_secs(8), &[],
                            ).await;
                        });
                    }
                    uploaded.push(attachment);
                    continue;
                }
                Sealed::Body(body, guard) => (body, guard),
            };
            let cb_for_progress = callback;
            let pid_for_progress = pending_id.clone();
            let progress_cb: vector_core::blossom::ProgressCallback =
                std::sync::Arc::new(move |percentage, bytes| {
                    cb_for_progress.on_upload_progress(
                        &pid_for_progress,
                        percentage.unwrap_or(0),
                        bytes.unwrap_or(0),
                    )
                });

            let accepted = match vector_core::blossom::upload_body_with_progress_and_failover(
                signer.clone(),
                servers.clone(),
                encrypted,
                Some(mime.as_str()),
                /* is_encrypted */ true,
                progress_cb,
                Some(3),
                Some(Duration::from_secs(2)),
                cancel_flag.clone(),
            )
            .await
            {
                Ok(url) => url,
                Err(e) => {
                    vector_core::log_net_fail!("[Community Upload] attachment upload failed (channel {}…): {}", &channel_id[..8.min(channel_id.len())], e);
                    let _ = mark_attachment_send_failed(&callback, &channel_id, &pending_id).await;
                    return Err(format!("Upload failed: {e}"));
                }
            };
            let upload_url = accepted.url.clone();

            attachment.url = upload_url.clone();
            // Reflect the uploaded URL on the optimistic bubble's attachment.
            {
                let mut state = vector_core::state::STATE.lock().await;
                state.update_attachment(&channel_id, &pending_id, &attachment.id, |a| {
                    a.url = upload_url.clone().into_boxed_str();
                });
            }
            callback.on_upload_complete(&channel_id, &pending_id, &attachment.id, &upload_url);

            // BUD-04 mirror fan-out: bounded, best-effort. Verified mirrors ride
            // the imeta as `fallback` sources; zero mirrors never fails the send.
            let mirror_urls = vector_core::blossom::mirror_blob_to_servers(
                signer.clone(),
                &upload_url,
                servers.clone(),
                2,
                Duration::from_secs(5),
                std::slice::from_ref(&accepted.server),
            )
            .await;
            if !mirror_urls.is_empty() {
                attachment.fallback_urls = mirror_urls.clone();
                let mut state = vector_core::state::STATE.lock().await;
                state.update_attachment(&channel_id, &pending_id, &attachment.id, |a| {
                    a.set_fallback_urls(mirror_urls.iter().map(|s| s.as_str().into()));
                });
            }
            uploaded.push(attachment);
        }

        // Build the real inner now that every imeta carries its uploaded URL.
        let mut imeta_tags: Vec<_> = uploaded
            .iter()
            .map(vector_core::community::attachments::attachment_to_imeta)
            .collect();
        imeta_tags.extend(vector_core::text_spans::to_nostr_tags(&text_spans));

        // v2: seal the caption + imeta rumor through the v2 service. The send echoes
        // the persisted message into STATE/DB (bot-silent), so adopt the echo over
        // the optimistic bubble — finalizing the temp id would duplicate the row.
        let reply_owned = match reply.as_deref() {
            Some(parent_id) => {
                // The NIP-C7 q author slot is a SHOULD — best-effort from the held parent.
                let author_hex = {
                    let st = vector_core::state::STATE.lock().await;
                    st.find_message(parent_id)
                        .and_then(|(_, m)| m.npub.as_deref().and_then(|n| nostr_sdk::prelude::PublicKey::parse(n).ok()))
                        .map(|pk| pk.to_hex())
                        .unwrap_or_default()
                };
                Some((parent_id.to_string(), author_hex))
            }
            None => None,
        };
        let reply_ref = reply_owned.as_ref().map(|(id, a)| (id.as_str(), a.as_str()));
        let emoji_pairs: Vec<(&str, &str)> = emoji_tags.iter().map(|t| (t.shortcode.as_str(), t.url.as_str())).collect();
        // Self-Destruct Timer: stamped now that the upload is done, and the echo carries
        // it to the bubble.
        let expiry = self_destruct_secs.and_then(vector_core::self_destruct::expiry_after);
        if let Some(exp) = expiry {
            imeta_tags.push(nostr_sdk::prelude::Tag::expiration(nostr_sdk::prelude::Timestamp::from_secs(exp)));
        }
        let transport = LiveTransport::with_timeout(Duration::from_secs(12));
        let sent = vector_core::community::v2::service::send_chat_message(
            &transport, &community, &ch, &content, reply_ref, &emoji_pairs, imeta_tags,
        )
        .await;
        match sent {
            Ok(real_id) => {
                let echoed = {
                    let mut state = vector_core::state::STATE.lock().await;
                    state.remove_message(&pending_id);
                    // Re-adopt our local download state onto the echo. The echo is parsed by
                    // the inbound path, which never claims `downloaded` (an arriving imeta
                    // proves nothing about what's on disk) — but we wrote these plaintexts
                    // ourselves moments ago. Without this the sender downloads its own upload
                    // back from Blossom, which on a slow transport shows as a file that sits
                    // in a downloading state for minutes after sending. The DB upsert already
                    // applies this rule via `downloaded=MAX(...)`; STATE needs it too.
                    for local in uploaded.iter().filter(|u| u.downloaded) {
                        let path = local.path.clone();
                        state.update_attachment(&channel_id, &real_id, &local.id, |a| {
                            a.set_downloaded(true);
                            a.set_downloading(false);
                            a.set_path(path.clone());
                        });
                    }
                    state.find_message(&real_id).map(|(_, m)| m.clone())
                };
                if let Some(ref msg) = echoed {
                    callback.on_sent(&channel_id, &pending_id, msg);
                    callback.on_persist(&channel_id, msg);
                } else {
                    vector_core::log_warn!("[community] v2 sent message {} not in STATE to finalize", real_id);
                }
                let webxdc_topic = uploaded.iter().find_map(|a| a.webxdc_topic.clone());
                Ok(CommunityAttachmentSendResult { message_id: real_id, webxdc_topic })
            }
            Err(e) => {
                let _ = mark_attachment_send_failed(&callback, &channel_id, &pending_id).await;
                Err(e)
            }
        }
    })
    .await
}

/// Mark an optimistic Community attachment message failed (keeps the temp id; offers retry).
async fn mark_attachment_send_failed(
    callback: &crate::message::sending::TauriSendCallback,
    channel_id: &str,
    pending_id: &str,
) -> Option<()> {
    use vector_core::sending::SendCallback;
    let failed = {
        let mut state = vector_core::state::STATE.lock().await;
        state.update_message(pending_id, |m| {
            m.set_failed(true);
            m.set_pending(false);
        })
    };
    if let Some((_cid, ref msg)) = failed {
        callback.on_failed(channel_id, pending_id, msg);
    }
    Some(())
}

/// Result of a Community page sync. `reached_start` is the AUTHORITATIVE "no more older
/// history" signal (the relay returned nothing strictly older than the cursor) — the frontend
/// keys its scroll-up termination off this, never off a DB row-count delta (a page can return
/// only already-known events while older history still exists).
#[derive(serde::Serialize, Default)]
pub struct CommunitySyncResult {
    pub new_messages: u32,
    pub reached_start: bool,
    /// Where the relay walk stopped (ms) with history still unread below: the cursor for the
    /// next older page, which skips nothing. `None` when the page reached what it was after.
    pub oldest_ms: Option<u64>,
}

/// Sync one PAGE of a Community channel from the network (Discord-style).
///
/// `before_ms = None` → the LATEST page: emits `message_new` (recent → append + preview),
/// used on open / join / boot.
/// `before_ms = Some(ms)` → an OLDER page (scroll-up past local history): the oldest displayed
/// message's `at`. New messages ingest SILENTLY (frontend prepends from its DB re-query —
/// avoids the append-vs-prepend mismatch); reaction/edit `Updated`s still emit (they apply
/// surgically by target id, position-independent).
///
/// Anti-stampede: one in-flight fetch per channel+page. Ingest dedups on inner id; re-persist is
/// idempotent (INSERT OR REPLACE).
#[tauri::command]
pub async fn sync_community_channel(channel_id: String, before_ms: Option<u64>) -> Result<CommunitySyncResult, String> {
    let is_older = before_ms.is_some();

    // Anti-stampede: one in-flight fetch per channel per direction. Claimed FIRST so the cursor reset
    // below runs UNDER the claim — it can't clear the floor out from under a racing scroll-up fetch.
    let Some(_claim) = vector_core::community::cache::claim_page_fetch(&channel_id, is_older) else {
        return Ok(CommunitySyncResult::default());
    };

    sync_community_channel_inner(&channel_id, before_ms, is_older).await
}

async fn sync_community_channel_inner(
    channel_id: &str,
    before_ms: Option<u64>,
    is_older: bool,
) -> Result<CommunitySyncResult, String> {
    vector_core::db::scoped(async move {
        // Consensus refold + chat backfill into the shared events tables, through the facade;
        // the frontend re-queries get_messages from those tables.
        let limit: usize = 50;
        // Scroll-up carries its cursor to the network: each rotated epoch is its own address,
        // and an uncursored back-page would re-fetch the newest window forever.
        let before_secs = is_older.then(|| before_ms.map(|m| m / 1000)).flatten();
        let count = vector_core::VectorCore
            .sync_community_channel_page(channel_id, limit, before_secs, None)
            .await
            .map(|(c, _warnings)| c)
            .unwrap_or_default();
        // An older page is finished only when the RELAY had nothing at or before the cursor:
        // judged by new-count, the scroll would stop at the first page of already-held history,
        // which is exactly where a rotated channel's older epochs sit.
        let reached_start = if is_older { count.fetched == 0 && count.next_before.is_none() } else { count.new_messages < limit };
        Ok(CommunitySyncResult { new_messages: count.new_messages as u32, reached_start, oldest_ms: count.next_before.map(|s| s * 1000) })
    })
    .await
}

/// Read-only v2 diagnostic snapshot: epochs, the channel-rekey addressing fan,
/// per-channel key state, derived plane authors, edition floors, and the
/// guestbook cursor. NO secret material crosses this boundary — roots and keys
/// are reported as epoch numbers, counts, and PUBLIC keys only. Pairs with the
/// dev debug bridge for live epoch-wedge forensics.
/// Every Control edition of a held v2 community, newest first, and whether the fold kept it.
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn debug_v2_control_editions(community_id: String) -> Result<serde_json::Value, String> {
    let id = CommunityId(hex_to_id32(&community_id)?);
    let c = vector_core::db::community::load_community_v2(&id)?.ok_or("not a held v2 community")?;
    vector_core::community::v2::service::debug_control_editions(&c).await
}

#[cfg(debug_assertions)]
#[tauri::command]
pub async fn debug_v2_community_state(community_id: String) -> Result<serde_json::Value, String> {
    let id = CommunityId(hex_to_id32(&community_id)?);
    let c = vector_core::db::community::load_community_v2(&id)?.ok_or("not a held v2 community")?;
    let cid_hex = c.id().to_hex();

    let archived_root_epochs: Vec<u64> =
        vector_core::db::community::held_epoch_keys(&cid_hex, vector_core::community::SERVER_ROOT_SCOPE_HEX)
            .unwrap_or_default()
            .into_iter()
            .map(|(e, _)| e.0)
            .collect();
    let channels: Vec<serde_json::Value> = c
        .channels
        .iter()
        .map(|ch| {
            let ch_hex = vector_core::simd::hex::bytes_to_hex_32(&ch.id.0);
            let held_epochs: Vec<u64> = vector_core::db::community::held_epoch_keys(&cid_hex, &ch_hex)
                .unwrap_or_default()
                .into_iter()
                .map(|(e, _)| e.0)
                .collect();
            serde_json::json!({
                "channel_id": ch_hex,
                "private": ch.private,
                "epoch": ch.epoch.0,
                "key_held": ch.key.is_some(),
                "held_epoch_keys": held_epochs,
                // The Writers ledger names public addresses only; the sealed form only its epoch.
                "writers": ch.writers.as_ref().and_then(|l| serde_json::from_str::<serde_json::Value>(&l.to_json()).ok()),
                "sealed": ch.sealed.as_ref().map(|s| serde_json::json!({ "epoch": s.epoch, "open": s.open, "junk": s.junk })),
            })
        })
        .collect();
    let plane_authors: Vec<String> = vector_core::community::v2::realtime::plane_authors(std::slice::from_ref(&c))
        .into_iter()
        .map(|p| p.to_hex())
        .collect();
    // The live sub's ACTUAL author set vs this community's derived planes: a
    // rotation that outran the re-subscribe shows here as sub_covers=false —
    // otherwise it only shows as silently missing live events.
    let subscribed = vector_core::community::v2::realtime::subscribed_author_set().await;
    let sub_covers = plane_authors.iter().all(|a| subscribed.contains(a));
    let floors: Vec<serde_json::Value> = vector_core::db::community::get_all_edition_heads_epoched(&cid_hex)
        .unwrap_or_default()
        .into_iter()
        .map(|(entity, (epoch, version, _hash))| {
            serde_json::json!({ "entity": entity, "epoch": epoch, "version": version })
        })
        .collect();
    let (guestbook_events, guestbook_cursor) =
        vector_core::db::community::get_guestbook(&cid_hex).unwrap_or_default();

    Ok(serde_json::json!({
        "community_id": cid_hex,
        "root_epoch": c.root_epoch.0,
        "archived_root_epochs": archived_root_epochs,
        "dissolved": vector_core::db::community::get_community_dissolved(&cid_hex).unwrap_or(false),
        "relays": c.relays,
        "channels": channels,
        "plane_authors": plane_authors,
        "sub_covers": sub_covers,
        "subscribed_authors": subscribed,
        "edition_floors": floors,
        "guestbook": { "events": guestbook_events.len(), "cursor": guestbook_cursor },
    }))
}

/// Run one v2 follow inline and report EVERY stage as JSON — the headless twin
/// of the follow worker for wedge forensics: is the community even visible to
/// the v2 realtime layer (protocol row, held-v2 set, worker state), are its
/// relays connected, and what does each follow stage actually return.
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn debug_v2_follow_trace(community_id: String) -> Result<serde_json::Value, String> {
    let id = CommunityId(hex_to_id32(&community_id)?);
    let session = vector_core::db::current_session();

    let held = vector_core::db::community::community_held(&id);
    let in_held_v2 = vector_core::community::v2::realtime::load_held_v2()
        .iter()
        .any(|c| c.id().to_hex() == community_id);
    let worker_running = vector_core::community::v2::realtime::follow_worker_running();

    let c = vector_core::db::community::load_community_v2(&id)?.ok_or("not a held v2 community")?;

    // Relay pool status for the community's relays.
    let mut relay_status = Vec::new();
    if let Some(client) = vector_core::state::nostr_client() {
        let pool = client.relays().all().await;
        for r in &c.relays {
            let status = nostr_sdk::prelude::RelayUrl::parse(r)
                .ok()
                .and_then(|u| pool.get(&u).map(|rel| format!("{:?}", rel.status())))
                .unwrap_or_else(|| "NOT IN POOL".into());
            relay_status.push(serde_json::json!({ "relay": r, "status": status }));
        }
    }

    // Run the three stages UNDER the follow lock so this can't race the live
    // worker into a whole-row clobber (the stages persist — this is a mutating
    // diagnostic, serialized against the worker by the shared lock).
    let (rekeys, control, guestbook) =
        vector_core::community::v2::realtime::debug_run_follow_stages(&id, &session).await;
    let c = vector_core::db::community::load_community_v2(&id)?.ok_or("community gone mid-trace")?;

    Ok(serde_json::json!({
        "community_id": community_id,
        "held_row": format!("{held:?}"),
        "in_held_v2": in_held_v2,
        "follow_worker_running": worker_running,
        "relay_status": relay_status,
        "root_epoch_before_after": c.root_epoch.0,
        "follow_rekeys": rekeys,
        "follow_control": control,
        "sync_guestbook": guestbook,
    }))
}

/// Re-vend the epoch chain to members a rotation forgot.
///
/// A member missed at one hop is stranded there forever — a client can only
/// derive `held + 1`, so no later rotation reaches them, and re-inviting does
/// nothing because they are already a member. Only the rotator holds the
/// archived roots this needs, which is why it lives on the client that rotated.
#[tauri::command]
pub async fn rescue_stranded_members(community_id: String, npubs: Vec<String>) -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move {
        let refs: Vec<&str> = npubs.iter().map(|s| s.as_str()).collect();
        vector_core::VectorCore.rescue_stranded_members(&community_id, &refs).await.map_err(|e| e.to_string())
    })
    .await
}

/// Repair a v2 community's roster cache when it is pinned holding WRONG data:
/// resets the cache's provenance so the next control fold may replace it, then
/// folds. Publishes nothing.
#[tauri::command]
pub async fn repair_v2_roster(community_id: String) -> Result<serde_json::Value, String> {
    vector_core::VectorCore
        .repair_community_roster(&community_id)
        .await
        .map_err(|e| e.to_string())
}

/// Explain this client's epoch state and WHY it is or isn't adopting the next base
/// rotation: runs the real fetch+parse+authority+continuity pipeline and reports the
/// gate each rotation trips, alongside the local state two clients diverge on.
/// Read-only (public keys only). Ships in release — a strand is silent otherwise.
#[tauri::command]
pub async fn debug_v2_explain_base_rekey(community_id: String) -> Result<serde_json::Value, String> {
    use vector_core::community::transport::LiveTransport;
    let id = CommunityId(hex_to_id32(&community_id)?);
    let c = vector_core::db::community::load_community_v2(&id)?.ok_or("not a held v2 community")?;
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    vector_core::community::v2::service::explain_epoch_state(&transport, &c).await
}

/// Wire-level probe for a v2 community's rotation planes. For each candidate
/// derivation (the spec next-epoch base plane, archived-root variants, and the
/// guestbook/control planes as positive controls), open a FRESH connection to
/// the community's relays authenticated AS that plane key — a gating relay
/// serves an author's plane only to a connection authed as it, and challenges
/// once per connection, so the app's live pool can't re-auth for ad-hoc keys.
/// Every verdict is EOSE-verified (empty is proven, never assumed from a
/// timeout or CLOSED). Read-only; nothing persists.
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn debug_v2_probe_rekey_planes(community_id: String) -> Result<serde_json::Value, String> {
    use vector_core::community::v2::{derive, stream};
    use vector_core::community::Epoch;
    let id = CommunityId(hex_to_id32(&community_id)?);
    let c = vector_core::db::community::load_community_v2(&id)?.ok_or("not a held v2 community")?;
    let cid_hex = c.id().to_hex();

    let next = Epoch(c.root_epoch.0.saturating_add(1));
    let mut candidates: Vec<(String, vector_core::community::v2::derive::GroupKey)> = vec![
        (format!("base-rekey e{} @ current root [SPEC address]", next.0),
         derive::base_rekey_group_key(&c.community_root, c.id(), next)),
        (format!("base-rekey e{} @ current root [skipped-epoch check]", next.0 + 1),
         derive::base_rekey_group_key(&c.community_root, c.id(), Epoch(next.0 + 1))),
    ];
    let archived = vector_core::db::community::held_epoch_keys(&cid_hex, vector_core::community::SERVER_ROOT_SCOPE_HEX)
        .unwrap_or_default();
    for (epoch, root) in &archived {
        if *root != c.community_root {
            candidates.push((
                format!("base-rekey e{} @ archived root e{} [divergent-derivation check]", next.0, epoch.0),
                derive::base_rekey_group_key(root, c.id(), next),
            ));
        }
    }
    candidates.push(("guestbook @ current epoch [positive control]".into(),
        derive::guestbook_group_key(&c.community_root, c.id(), c.root_epoch)));
    candidates.push(("control @ current epoch [positive control]".into(),
        derive::control_group_key(&c.community_root, c.id(), c.root_epoch)));

    let mut report = Vec::new();
    for (label, group) in candidates {
        let pk_hex = group.pk_hex();
        // Probe traffic must obey the user's transport, and this connection auths
        // as the PLANE key rather than the session identity — hence its own
        // authenticator instead of `nostr_client_builder()`.
        let client = vector_core::apply_transport(
            nostr_sdk::prelude::Client::builder().authenticator(
                nostr_sdk::prelude::SignerAuthenticator::new(group.keys().clone()),
            ),
            vector_core::transport::Lane::Shared,
        )
        .build();
        for r in &c.relays {
            let _ = client.add_managed_relay(r.clone()).await;
        }
        client.connect().await;
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let filter = nostr_sdk::prelude::Filter::new()
            .kinds([
                nostr_sdk::prelude::Kind::Custom(stream::KIND_WRAP),
                nostr_sdk::prelude::Kind::Custom(stream::KIND_WRAP_EPHEMERAL),
            ])
            .author(group.pk());
        let mut per_relay = Vec::new();
        for r in &c.relays {
            // Warmup REQ with the GATED filter shape: Ditto's policy ("all
            // authors must be authenticated") only challenges author-filtered
            // REQs, and nostr-sdk's auto-auth completes + resubscribes inside
            // this fetch — so the EOSE-verified probe after it rides an
            // already-authenticated connection instead of dying on the first
            // CLOSED. (An ungated warmup never triggers auth at all.)
            let _ = client
                .fetch_events(nostr_sdk::prelude::ReqTarget::manual(vec![r.clone()].into_iter().map(|u| (u, vec![filter.clone().clone()]))))
            .timeout(Duration::from_secs(6))
                .await;
            tokio::time::sleep(Duration::from_millis(800)).await;
            let entry = match vector_core::community::transport::fetch_relay_eose(&client, r, filter.clone(), Duration::from_secs(8)).await {
                Ok(events) => {
                    let detail: Vec<serde_json::Value> = events
                        .iter()
                        .map(|e| serde_json::json!({
                            "id": e.id.to_hex(),
                            "kind": e.kind.as_u16(),
                            "created_at": e.created_at.as_secs(),
                            "tags": e.tags.iter().map(|t| t.as_slice().to_vec()).collect::<Vec<_>>(),
                        }))
                        .collect();
                    serde_json::json!({ "relay": r, "eose": true, "events": detail.len(), "detail": detail })
                }
                Err(_) => serde_json::json!({ "relay": r, "eose": false, "outcome": "CLOSED / timeout / no answer" }),
            };
            per_relay.push(entry);
        }
        client.disconnect().await;
        report.push(serde_json::json!({ "plane": label, "pk": pk_hex, "relays": per_relay }));
    }
    Ok(serde_json::json!({ "community_id": cid_hex, "root_epoch": c.root_epoch.0, "probe": report }))
}

/// Coalesce a burst of relay reconnections into one Community catch-up, scoped to the
/// Communities that use those relays. `ours` marks a relay we read from: one that also carries
/// the cross-device Community List. A reconnect that lands while a catch-up runs is queued for
/// the next pass, not dropped.
pub fn trigger_community_reconnect_resync(relay: String, ours: bool) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
    static PENDING: std::sync::Mutex<(Vec<String>, bool)> = std::sync::Mutex::new((Vec::new(), false));
    {
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        if !pending.0.contains(&relay) {
            pending.0.push(relay);
        }
        pending.1 |= ours;
    }
    if IN_FLIGHT.swap(true, Ordering::AcqRel) {
        return;
    }
    // Bound to the account that reconnected: a swap mid-debounce ends the loop, and the new
    // account's own reconnects start their own.
    vector_core::db::spawn_bound(async move {
        loop {
            // Lets a reconnect burst settle, so this re-subscribes against live connections.
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let (relays, ours) = {
                let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
                (std::mem::take(&mut pending.0), std::mem::take(&mut pending.1))
            };
            if !relays.is_empty() && vector_core::db::session_is_live() {
                // RE-ARM the live Community subscription FIRST: the pool re-applies the pool-wide
                // DM sub on reconnect but not our targeted Community sub, so realtime delivery
                // would stay dead after any reconnect (notably Android's bg-sync).
                crate::services::subscription_handler::refresh_community_subscription().await;
                let _ = sync_communities_on_reconnect(relays, ours).await;
            }
            IN_FLIGHT.store(false, Ordering::Release);
            let queued = !PENDING.lock().unwrap_or_else(|e| e.into_inner()).0.is_empty();
            if !queued || !vector_core::db::session_is_live() || IN_FLIGHT.swap(true, Ordering::AcqRel) {
                break;
            }
        }
    });
}

/// Catch up after `relays` came back: only the Communities that use them, and only their gap
/// (each channel's latest page is already floored at its newest cursor). The boot-only work
/// (paint volley, probes) stays at boot.
async fn sync_communities_on_reconnect(relays: Vec<String>, ours: bool) -> Result<(), String> {
    vector_core::db::scoped(async move {
        let t = std::time::Instant::now();
        let back: std::collections::HashSet<String> =
            relays.iter().map(|r| vector_core::inbox_relays::normalize_relay_url(r)).collect();
        let uses = |list: &[String]| list.iter().any(|r| back.contains(&vector_core::inbox_relays::normalize_relay_url(r)));

        let mut channels: Vec<String> = Vec::new();
        let mut communities = 0usize;
        for id in vector_core::db::community::list_community_ids()? {
            let Ok(Some(c)) = vector_core::db::community::load_community_v2(&id) else { continue };
            if !uses(&c.relays) {
                continue;
            }
            communities += 1;
            // Its control, guestbook and rekeys may have moved in the gap too.
            vector_core::community::v2::realtime::enqueue_follow(c.id());
            channels.extend(c.channels.iter().map(|ch| vector_core::simd::hex::bytes_to_hex_32(&ch.id.0)));
        }

        // One of our own relays also holds the Community List: other devices' joins and
        // leaves in the gap land there.
        if ours {
            vector_core::db::spawn_bound(async move {
                ingest_v2_community_list_update().await;
            });
        }

        const RECONNECT_SYNC_WINDOW: usize = 6;
        use futures_util::stream::StreamExt;
        let channel_count = channels.len();
        futures_util::stream::iter(channels)
            .map(|cid| async move {
                if vector_core::db::session_stopped() {
                    return;
                }
                let _ = sync_community_channel(cid, None).await;
            })
            .buffer_unordered(RECONNECT_SYNC_WINDOW)
            .collect::<Vec<()>>()
            .await;
        log_info!(
            "[Reconnect] {} relay(s) back: {} community(ies), {} channel(s) caught up in {:?}",
            back.len(),
            communities,
            channel_count,
            t.elapsed()
        );
        Ok(())
    })
    .await
}

/// Boot sweep: sync the LATEST page of every joined Community channel, most-recent-activity
/// first (so the top of the chat list refreshes first), through a sliding window of 3 to avoid
/// overwhelming the relays/bandwidth. ONE IPC call drives the whole sweep — no per-channel
/// frontend round-trips. Each page emits `message_new` as it lands, so the chat list fills in
/// progressively. Per-channel anti-stampede makes this safe to overlap with reconnect re-syncs.
#[tauri::command]
pub async fn sync_communities_boot() -> Result<(), String> {
    vector_core::db::scoped(async move {
        // One pipeline at a time: a relay reconnect mid-boot re-triggers this whole
        // run, doubling the volley and probes (the sweep's page claims absorb the
        // twins, but the repeated work is real). A run that arrives while one is
        // active is redundant — the active run reads the same relays.
        static SWEEP_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        static RERUN_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        // Wall-clock floor between sweeps. The in-flight latch only collapses runs
        // that OVERLAP; a relay flapping every few seconds lands each trigger after
        // the last run finished, so every blip bought a fresh sweep of every held
        // channel — the load that causes the next blip. A trigger inside the window
        // is deferred to its end, not dropped: the relay that just came up may hold
        // events the finished run never saw.
        const SWEEP_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(45);
        static LAST_SWEEP_END: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
        static DEFERRED_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let cooling = LAST_SWEEP_END
            .lock()
            .ok()
            .and_then(|g| *g)
            .and_then(|end| SWEEP_COOLDOWN.checked_sub(end.elapsed()));
        if let Some(wait) = cooling {
            if !SWEEP_IN_FLIGHT.load(std::sync::atomic::Ordering::SeqCst)
                && !DEFERRED_ARMED.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                println!("[Boot] community sync within cooldown — deferring {:?}", wait);
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        tokio::time::sleep(wait).await;
                        DEFERRED_ARMED.store(false, std::sync::atomic::Ordering::SeqCst);
                        respawn_community_sync();
                    });
                } else {
                    DEFERRED_ARMED.store(false, std::sync::atomic::Ordering::SeqCst);
                }
            }
            return Ok(());
        }
        if SWEEP_IN_FLIGHT.swap(true, std::sync::atomic::Ordering::SeqCst) {
            // Don't DROP the trigger: channels the active run fetched before this
            // caller's relay came up never saw its gap events — queue one rerun.
            RERUN_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
            println!("[Boot] community sync already in flight — queuing a follow-up run");
            return Ok(());
        }
        struct SweepClaim;
        impl Drop for SweepClaim {
            fn drop(&mut self) {
                SWEEP_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);
                // The cooldown runs from the END of a sweep: a long run must not
                // have its own duration counted against the next one's quiet window.
                if let Ok(mut g) = LAST_SWEEP_END.lock() {
                    *g = Some(std::time::Instant::now());
                }
                // Honor a queued rerun on EVERY exit path: an early return (account
                // swap, propagated error) must not strand the trigger until the
                // next reconnect. The respawned run captures its own fresh session.
                if RERUN_REQUESTED.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    respawn_community_sync();
                }
            }
        }
        let _claim = SweepClaim;
        // A fresh full run satisfies any rerun queued before it started (and an
        // early-return run must not leave a stale latch for the next account).
        RERUN_REQUESTED.store(false, std::sync::atomic::Ordering::SeqCst);

        let session = vector_core::db::current_session();
        let boot_start = std::time::Instant::now();

        // Parked-invite hygiene FIRST, unconditionally: a dormant machine may boot
        // with months of fossils that only the lifetime rule can cull (no declared
        // expiry, never held, never tombstoned) — and the list-sync purges only
        // run when a 33302 actually arrives.
        if let Ok(n) = vector_core::db::community::purge_expired_pending_invites() {
            if n > 0 {
                println!("[Boot] purged {} stale pending invite(s)", n);
            }
        }
        if let Ok(n) = vector_core::db::community::purge_pending_invites_for_held_communities() {
            if n > 0 {
                println!("[Boot] purged {} pending invite(s) for held communities", n);
            }
        }

        // Newest-message time per chat: the volley's since-bounds AND the sweep's
        // activity ordering (one DB read shared by both).
        let last_msgs = crate::db::get_all_chats_last_messages().await.unwrap_or_default();
        let activity = |cid: &str| -> u64 {
            last_msgs.get(cid).and_then(|v| v.first().map(|m| m.at)).unwrap_or(0)
        };

        // Held v2 communities: re-register chat rows (cheap, DB-only) BEFORE the
        // volley so painted messages land on restored community chats. Follows
        // queue AFTER the volley — the rapid pass gets the network to itself.
        for c in vector_core::community::v2::realtime::load_held_v2() {
            vector_core::VectorCore.register_v2_chats(&c, &session).await;
        }
        // Dissolved ones too: they are no longer followed, but their rows must stay
        // sealed, and this also repairs rows a dissolution never reached.
        for c in vector_core::community::v2::realtime::load_dissolved_v2() {
            let cid = vector_core::simd::hex::bytes_to_hex_32(&c.id().0);
            let unsealed = crate::STATE.lock().await.chats.iter().any(|ch| {
                ch.metadata.community_id() == Some(cid.as_str()) && !ch.metadata.is_dissolved()
            });
            vector_core::VectorCore.register_v2_chats(&c, &session).await;
            // The UI already holds the rows it booted with; tell it only about a repair.
            if unsealed {
                vector_core::emit_event("community_refreshed", &serde_json::json!({ "community_id": cid }));
            }
        }

        // ── Volley Sync (chat-plane paint) ───────────────────────────────────────────────
        // Paints every channel's latest page from stored epoch state
        // (CONCORD_CHAT_PLANE_VOLLEY_DESIGN.md).
        {
            let volley_start = std::time::Instant::now();
            const VOLLEY_SINCE_SLACK_SECS: u64 = 3600;
            let mut with_activity: Vec<(vector_core::community::v2::volley::PaintTarget, u64)> = Vec::new();
            for id in vector_core::db::community::list_community_ids()? {
                let Ok(Some(c)) = vector_core::db::community::load_community_v2(&id) else { continue };
                for ch in &c.channels {
                    let cid = vector_core::simd::hex::bytes_to_hex_32(&ch.id.0);
                    let at = activity(&cid);
                    let since = (at > 0).then(|| (at / 1000).saturating_sub(VOLLEY_SINCE_SLACK_SECS));
                    with_activity.push((
                        vector_core::community::v2::volley::PaintTarget {
                            community_id: vector_core::community::CommunityId(id.0),
                            channel_hex: cid,
                            since,
                        },
                        at,
                    ));
                }
            }
            with_activity.sort_by_key(|t| std::cmp::Reverse(t.1));
            let volley_count = with_activity.len();
            let targets: Vec<_> = with_activity.into_iter().map(|(t, _)| t).collect();
            let (painted, stats) = vector_core::community::v2::volley::paint_all(targets).await;
            let total: usize = painted.iter().map(|(_, n)| n).sum();
            for (cid, n) in &painted {
                println!("[Boot]   volley {}…: {} new", &cid[..8.min(cid.len())], n);
            }
            println!(
                "[Boot] Volley Sync: {} channel(s), {} new in {:?} (batch {}ev/{}ms, fallback {}ev/{}ms)",
                volley_count,
                total,
                volley_start.elapsed(),
                stats.batch_events,
                stats.batch_ms,
                stats.fallback_events,
                stats.fallback_ms
            );
        }

        // Verification follows queue AFTER the whole chat paint BY OWNER RULING: boot exists to deliver message data; ban data
        // rides seconds behind and retro-hide revokes anything it invalidates.
        for c in vector_core::community::v2::realtime::load_held_v2() {
            vector_core::community::v2::realtime::enqueue_follow(c.id());
        }

        // Cross-device discovery runs CONCURRENTLY with the channel sweep: the List fetch takes
        // seconds and may not gate already-held communities' pages. A community it adopts pages
        // itself on arrival, and the per-channel anti-stampede coalesces any overlap.
        {
            vector_core::db::spawn_bound(async move {
                let t = std::time::Instant::now();
                // ONE implementation shared with the live kind-33302 self-sync: adopt, follow,
                // resubscribe, and SURFACE to the UI. This reconcile lands AFTER `init_finished`,
                // so a community adopted here needs the same frontend notification a live update does.
                ingest_v2_community_list_update().await;
                vector_core::log_info!("[Boot] community list reconcile (background) in {:?}", t.elapsed());
            });
        }

        // Flatten every joined Community's channels.
        let mut channels: Vec<String> = Vec::new();
        // v2 root epoch per community, sampled BEFORE the sweep. A refounding adopted
        // while the sweep runs is picked up by every channel it has not reached yet
        // (each slot re-resolves from the DB), but channels already swept read the OLD
        // plane and the sweep never revisits them. Comparing after tells us exactly
        // which few to redo, instead of waiting for a later pass.
        let mut epoch_before: std::collections::HashMap<Vec<u8>, u64> = std::collections::HashMap::new();
        let mut v2_channels: std::collections::HashMap<Vec<u8>, Vec<String>> = std::collections::HashMap::new();
        for id in vector_core::db::community::list_community_ids()? {
            let Ok(Some(c)) = vector_core::db::community::load_community_v2(&id) else { continue };
            let key = id.0.to_vec();
            epoch_before.insert(key.clone(), c.root_epoch.0);
            let mut mine = Vec::new();
            for ch in &c.channels {
                let hex = vector_core::simd::hex::bytes_to_hex_32(&ch.id.0);
                mine.push(hex.clone());
                channels.push(hex);
            }
            v2_channels.insert(key, mine);
        }
        // Most-recent-activity first.
        channels.sort_by_key(|c| std::cmp::Reverse(activity(c)));

        // No coverage cap — every joined Community syncs at boot (NIP-17 parity: bulk catch-up here,
        // realtime after, re-sync on reconnect; nothing on-demand). A sliding window (not fixed
        // batches) bounds peak relay load: a finished sync yields its slot to the next immediately
        // instead of waiting on the slowest of a chunk. Backfills reuse warm pooled connections, so more channels in flight hides slow-relay
        // latency across fewer sequential slots. The plane pool (cap 24) and per-relay breaker
        // bound the concurrent load.
        const BOOT_SYNC_WINDOW: usize = 6;
        use futures_util::stream::StreamExt;

        let channel_count = channels.len();
        futures_util::stream::iter(channels)
            .map(|cid| async move {
                // The sweep is 31 channels of relay round-trips on a real
                // account. Each slot re-checks, so a swap drains the window
                // instead of syncing the rest of a chat list nobody is on.
                if vector_core::db::session_stopped() {
                    return;
                }
                let t = std::time::Instant::now();
                let new = sync_community_channel(cid.clone(), None)
                    .await
                    .map(|r| r.new_messages)
                    .unwrap_or(0);
                // Truncated id only — full channel ids correlate planes.
                println!(
                    "[Boot]   channel {}…: {} new in {:?}",
                    &cid[..8.min(cid.len())],
                    new,
                    t.elapsed()
                );
            })
            .buffer_unordered(BOOT_SYNC_WINDOW)
            .collect::<Vec<()>>()
            .await;
        println!(
            "[Boot] community channel sweep: {} channel(s) in {:?}",
            channel_count,
            boot_start.elapsed()
        );

        // Redo only the channels of communities whose root moved mid-sweep.
        let mut restale: Vec<String> = Vec::new();
        for (key, before) in &epoch_before {
            let Ok(bytes) = <[u8; 32]>::try_from(key.as_slice()) else { continue };
            let id = vector_core::community::CommunityId(bytes);
            if let Ok(Some(c)) = vector_core::db::community::load_community_v2(&id) {
                if c.root_epoch.0 != *before {
                    if let Some(chs) = v2_channels.get(key) {
                        restale.extend(chs.iter().cloned());
                    }
                }
            }
        }
        if !restale.is_empty() {
            let n = restale.len();
            let t = std::time::Instant::now();
            futures_util::stream::iter(restale)
                .map(|cid| async move {
                    if vector_core::db::session_stopped() {
                        return;
                    }
                    let _ = sync_community_channel(cid, None).await;
                })
                .buffer_unordered(BOOT_SYNC_WINDOW)
                .collect::<Vec<()>>()
                .await;
            println!("[Boot] re-swept {} channel(s) whose epoch moved mid-sweep in {:?}", n, t.elapsed());
        }
        // Re-assert the live subscriptions now that the flood is over. A relay
        // under the sweep's load can drop or refuse the boot-time subs, and
        // nothing else re-asks until a reconnect happens to fire — which is
        // exactly the "dead until it all arrives at once" window.
        if let Some(client) = vector_core::state::nostr_client() {
            // FORCE: an unchanged author set makes the ordinary refresh a no-op, which
            // is precisely the boot case — so the sub the sweep just knocked out would
            // never be rebuilt.
            vector_core::community::v2::realtime::force_refresh_subscription(&client).await;
            println!("[Boot] live subs re-asserted after sweep");
            crate::services::subscription_handler::reassert_dm_sub().await;
        }
        drop(_claim);
        Ok(())
    })
    .await
}

/// One queued re-run of the boot sweep (a reconnect landed mid-run). Boxed to
/// break the recursive-future Send inference; the guard inside the new run
/// still serializes against any racing caller.
fn respawn_community_sync() {
    // Callable from SweepClaim::Drop — a drop during runtime teardown must
    // not panic in tokio::spawn, so bail when no runtime context exists.
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async move {
        let fut: std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), String>> + Send>,
        > = Box::pin(sync_communities_boot());
        let _ = fut.await;
    });
}

/// Delete one of the local user's own Community messages. NIP-09-deletes the outer
/// event via its retained ephemeral key, then drops it locally (STATE + DB) and tells the
/// UI to remove the row. Errors if no key is retained for `message_id` (not ours, or
/// already deleted) — only the original sender can delete their own message.
#[tauri::command]
pub async fn delete_community_message(message_id: String) -> Result<(), String> {
    vector_core::db::scoped(async move {
        let transport = LiveTransport::with_timeout(Duration::from_secs(12));

        // Snapshot the owning channel + attachment URLs BEFORE the cooperative echo removes the
        // row (the publish below locally applies its own delete, dropping the message from STATE).
        let (channel_id, attachment_urls) = {
            let state = vector_core::state::STATE.lock().await;
            match state.find_message(&message_id) {
                Some((chat, msg)) => (
                    chat.id.clone(),
                    msg.attachments
                        .iter()
                        .flat_map(|a| a.all_urls().map(str::to_string))
                        .collect::<Vec<_>>(),
                ),
                None => return Err("message not found (already deleted?)".to_string()),
            }
        };

        // Layer 1 — relay nuke: a real NIP-09 against the retained per-message ephemeral key,
        // erasing the wrapper from relays. Only possible when we hold that key (own send, this
        // device, post-retention). Best-effort: a relay failure still falls through to the
        // cooperative tombstone, and the key stays retained for a later retry.
        let has_key = vector_core::db::community::get_message_key(&message_id)
            .map(|k| k.is_some())
            .unwrap_or(false);
        if has_key {
            if let Err(e) = vector_core::community::message_delete::delete_message(&transport, &message_id).await {
                log_error!("Community relay delete failed, using cooperative tombstone only: {e}");
            }
        }

        // Layer 2 — the in-plane delete so peers hide it in-app even when we lack the original
        // signing key (the "Limited Delete" path). Author-gated on the peer side.
        community_target_op(&channel_id, &message_id, TargetOp::Delete).await?;

        // Layer 3 — best-effort Blossom blob delete for attachments (signed by the active
        // identity so bunker accounts authorize correctly). Smart-forward gate:
        // only urls no OTHER message still references may be scrubbed.
        let attachment_urls =
            vector_core::db::attachments::urls_unreferenced_elsewhere(&attachment_urls, &message_id)
                .unwrap_or_default();
        if !attachment_urls.is_empty() {
            if let Some(_client) = vector_core::state::nostr_client() {
                if let Ok(signer) = vector_core::signer::active_signer() {
                    vector_core::blossom::delete_blobs_best_effort(signer, attachment_urls);
                }
            }
        }

        // Drop locally + tell the UI to remove the row (idempotent — the cooperative echo may
        let removed_chat = {
            let mut state = vector_core::state::STATE.lock().await;
            state.remove_message(&message_id).map(|(cid, _)| cid)
        };
        let _ = crate::db::delete_event(&message_id).await;
        vector_core::emit_event(
            "message_removed",
            &serde_json::json!({
                "id": &message_id,
                "chat_id": removed_chat.as_deref().unwrap_or(&channel_id),
                "reason": "deleted"
            }),
        );
        Ok(())
    })
    .await
}

/// Revoke ONE of your own reactions (DM or Community). Author-gated: only the reactor can revoke,
/// re-verified on the peer side. Mirrors message deletion: a relay nuke via the retained per-reaction
/// ephemeral key, then a cooperative tombstone so live peers drop the chip. Pre-retention reactions
/// hold no key (can't be relay-nuked), but the cooperative notice still reaches live peers.
#[tauri::command]
pub async fn revoke_reaction(reaction_id: String) -> Result<(), String> {
    vector_core::db::scoped(async move {

        // Locate the reaction, confirm it's ours, grab the parent message + chat type.
        let (chat_id, message_id, author_npub, is_community) = {
            let state = vector_core::state::STATE.lock().await;
            state
                .find_reaction(&reaction_id)
                .ok_or_else(|| format!("Reaction not found (id: {reaction_id})"))?
        };
        let my_npub = vector_core::db::get_current_account()?;
        if author_npub != my_npub {
            return Err("Cannot revoke a reaction that isn't yours".to_string());
        }

        // Optimistic local removal + live chip refresh. Drop the kind-7 row (save is additive) and
        // emit before the network round-trip — snappy, and it makes the cooperative self-echo a no-op.
        let updated = {
            let mut state = vector_core::state::STATE.lock().await;
            state.remove_reaction_from_message(&message_id, &reaction_id)
        };
        let _ = crate::db::delete_event(&reaction_id).await;
        if let Some((_cid, message)) = updated {
            vector_core::emit_event(
                "message_update",
                &serde_json::json!({ "old_id": &message_id, "message": message, "chat_id": &chat_id }),
            );
        }

        // Network propagation.
        if is_community {
            let transport = LiveTransport::with_timeout(Duration::from_secs(12));
            // Layer 1 — relay nuke via the retained per-reaction ephemeral key (own send, post-retention).
            let has_key = vector_core::db::community::get_message_key(&reaction_id)
                .map(|k| k.is_some())
                .unwrap_or(false);
            if has_key {
                if let Err(e) = vector_core::community::message_delete::delete_message(&transport, &reaction_id).await {
                    log_error!("Community reaction relay delete failed, tombstone only: {e}");
                }
            }
            // Layer 2 — the in-plane delete targeting the reaction id (author-gated peer-side).
            community_target_op(&chat_id, &reaction_id, TargetOp::Delete).await?;
        } else {
            use nostr_sdk::prelude::{EventId, PublicKey};
            let rid = EventId::from_hex(&reaction_id).map_err(|e| format!("Invalid reaction id: {e}"))?;
            let recipient = PublicKey::parse(&chat_id)
                .map_err(|e| format!("Invalid DM counterpart: {e}"))?;
            vector_core::deletion::delete_own_reaction(&rid, recipient).await?;
        }

        Ok(())
    })
    .await
}

/// Moderation-hide: permanently remove ANY member's message you hold authority over
/// (cooperative — honest clients drop it because the delete carries the actor's real-npub
/// signature, re-verified against the roster). No undo. Gated on MANAGE_MESSAGES + a strict
/// outrank in vector-core.
#[tauri::command]
pub async fn hide_community_message(channel_id: String, message_id: String) -> Result<(), String> {
    vector_core::VectorCore
        .hide_community_message(&channel_id, &message_id)
        .await
        .map_err(|e| e.to_string())
}

/// Persist + surface a Community presence (join/leave) as a `MemberJoined`/`MemberLeft`
/// system event. Dedups by inner event id (the save returns false on a known id), so relay
/// replays and the sender's own echo are silent. Shared by the live subscription + sync paths.
pub(crate) async fn apply_community_presence(
    channel_id: &str,
    npub: &str,
    joined: bool,
    event_id: &str,
    created_at: u64,
    invited_by: Option<&str>,
    invited_label: Option<&str>,
) {
    vector_core::db::scoped(async move {
        let et = if joined {
            vector_core::stored_event::SystemEventType::MemberJoined
        } else {
            vector_core::stored_event::SystemEventType::MemberLeft
        };
        // attribution: persist "invited_by[|label]" in the system event's note so "joined via X's link"
        // survives a reload, and surface it on the live event for the member list.
        let note = invited_by.map(|by| match invited_label {
            Some(l) if !l.is_empty() => format!("{by}|{l}"),
            _ => by.to_string(),
        });
        // A swap can land during the save await; without this re-check the queue push
        // below would seed account A's npub into account B's freshly-cleared profile
        // queue, and the emit would surface A's join in B's open chat.
        let inserted = vector_core::db::events::save_system_event_at(event_id, channel_id, et, npub, note.as_deref(), created_at, invited_by, invited_label)
            .await
            .unwrap_or(false);
        if inserted {
            // A member we can't NAME renders as an npub stub everywhere (join/leave
            // line, member list, @mention pool) — queue their profile so the name
            // lands moments after the event. Gated on nameless-ness: a member we
            // already show a name for refreshes on the profile system's own cadence.
            let nameless = {
                let state = vector_core::state::STATE.lock().await;
                state.get_profile(npub).is_none_or(|p| {
                    p.nickname().is_empty() && p.display_name().is_empty() && p.name().is_empty()
                })
            };
            if nameless {
                vector_core::profile::sync::queue_profile_sync(
                    npub.to_string(),
                    vector_core::profile::sync::SyncPriority::High,
                    false,
                );
            }
            vector_core::emit_event(
                "system_event",
                &serde_json::json!({
                    "conversation_id": channel_id,
                    "event_id": event_id,
                    "event_type": et.as_u8(),
                    "member_pubkey": npub,
                    "member_name": serde_json::Value::Null,
                    "invited_by": invited_by,
                    "invited_label": invited_label,
                    // The event's REAL time (ms) so the UI sorts it chronologically. Without it the frontend
                    // stamps `now`, and a join replayed during history paging/rehydration sinks to the bottom.
                    "created_at_ms": created_at.saturating_mul(1000),
                }),
            );
        }
    })
    .await
}

/// A target-op on a Community message.
enum TargetOp<'a> {
    React { emoji: &'a str, url: Option<&'a str> },
    Edit(&'a str),
    Delete,
}

async fn community_target_op(channel_id: &str, target: &str, op: TargetOp<'_>) -> Result<(), String> {
    vector_core::db::scoped(async move {
        // The target-op (reaction / edit / delete) goes through the facade, which seals +
        // publishes + echoes into STATE/DB. The echo is emit-silent (bot parity), so surface the
        // update to the frontend via the same events the live dispatch fires.
        let core = vector_core::VectorCore;
        match op {
            TargetOp::React { emoji, url } => {
                core.send_community_reaction(channel_id, target, emoji, url).await.map_err(|e| e.to_string())?;
            }
            TargetOp::Edit(content) => {
                core.edit_community_message(channel_id, target, content).await.map_err(|e| e.to_string())?;
            }
            TargetOp::Delete => {
                core.delete_community_message_in(channel_id, target).await.map_err(|e| e.to_string())?;
                vector_core::emit_event("message_removed", &serde_json::json!({
                    "id": target, "chat_id": channel_id, "reason": "deleted",
                }));
                return Ok(());
            }
        }
        // React/edit updated the target in STATE — emit the fresh view.
        let updated = {
            let st = vector_core::state::STATE.lock().await;
            st.find_message(target).map(|(_, m)| m.clone())
        };
        if let Some(msg) = updated {
            vector_core::emit_event("message_update", &serde_json::json!({
                "old_id": target, "message": msg, "chat_id": channel_id,
            }));
        }
        Ok(())
    })
    .await
}

/// React to a Community message with an emoji. `emoji_url` carries the NIP-30 image when
/// the reaction is a custom `:shortcode:` (so the chip renders the image — parity w/ DMs).
#[tauri::command]
pub async fn react_to_community_message(
    channel_id: String,
    message_id: String,
    emoji: String,
    emoji_url: Option<String>,
) -> Result<(), String> {
    // Group ceiling + per-tier fresh-reaction allowance (joins always pass)
    vector_core::badges::check_new_reaction_allowance(&message_id, &emoji).await?;

    // A custom-emoji reaction (`:shortcode:` + url) carries its image.
    let custom = emoji.starts_with(':') && emoji.ends_with(':') && emoji.len() >= 3;
    let url = emoji_url.as_deref().filter(|_| custom);
    community_target_op(&channel_id, &message_id, TargetOp::React { emoji: &emoji, url }).await
}

/// Edit one of your own Community messages (only the original author's edit is honored).
#[tauri::command]
pub async fn edit_community_message(
    channel_id: String,
    message_id: String,
    new_content: String,
) -> Result<(), String> {
    // Core takes the colour markup and the custom emoji tags out of the raw text itself.
    community_target_op(&channel_id, &message_id, TargetOp::Edit(&new_content)).await
}

/// Invite an npub to a Community by gift-wrapping its invite bundle to them.
///
/// `community_id` is the 64-char hex Community id; the caller must be the proven owner.
/// The bundle travels on the USER's DM relays (not the
/// Community relays), since a fresh invitee has no Community pseudonym yet.
#[tauri::command]
pub async fn invite_to_community(community_id: String, invitee_npub: String) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.invite_to_community(&community_id, &invitee_npub).await.map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

/// List invites awaiting the user's accept/decline decision.
#[tauri::command]
pub async fn list_community_invites() -> Result<Vec<vector_core::db::community::PendingCommunityInvite>, String> {
    vector_core::db::community::list_pending_invites()
}

/// Accept a parked invite: verify, join, persist, and start receiving.
#[tauri::command]
pub async fn accept_community_invite(community_id: String) -> Result<CommunitySummary, String> {
    vector_core::db::scoped(async move {
        // A failed join leaves the invite parked so the user can retry or decline; a bundle
        // this build can't join is dropped.
        let summary = vector_core::VectorCore.accept_pending_invite(&community_id).await.map_err(|e| e.to_string())?;
        let cid = summary
            .get("community_id")
            .or_else(|| summary.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or(&community_id);
        let id_bytes = hex_to_id32(cid)?;
        summarize_any(&CommunityId(id_bytes)).ok_or_else(|| "accepted community not found".to_string())
    })
    .await
}

/// Decline a parked invite. Drops it locally, writes a decline tombstone to the synced Community List
/// (so a sibling device drops its copy too, and a re-delivered/older invite stays suppressed — a
/// strictly-newer one resurfaces), and sheds the relays the invite's bundle named.
#[tauri::command]
pub async fn decline_community_invite(community_id: String) -> Result<(), String> {
    // A bundle this build can't parse names no community the List could ever hold: only drop it.
    let bundle = vector_core::db::community::get_pending_invite(&community_id)
        .ok()
        .flatten()
        .and_then(|j| vector_core::community::v2::invite::CommunityInvite::from_bundle_json(&j).ok());
    let relays: Vec<String> = bundle.as_ref().map(|b| b.relays.clone()).unwrap_or_default();

    vector_core::db::community::delete_pending_invite(&community_id)?;
    // Cross-device + durable suppression: tombstone the kind-33302 List so the un-deletable
    // invite wrap can't re-nag and other devices drop their parked copy on their next list sync.
    if let Some(cid) = bundle.and_then(|_| vector_core::simd::hex::hex_to_bytes_32_checked(&community_id)) {
        let removed_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        vector_core::community::v2::service::tombstone_community_list_durable(
            vector_core::community::CommunityId(cid),
            relays.clone(),
            removed_at,
        );
    }
    // Prune the relays no joined community needs.
    if !relays.is_empty() {
        vector_core::community::transport::prune_unneeded_community_relays(&relays).await;
    }
    Ok(())
}

// ============================================================================
// Community display metadata (name / description / logo / banner)
// ============================================================================

/// Edit a Community's text metadata (MANAGE_METADATA) as a control edition so members
/// pick it up. `None` leaves a field unchanged. Previews + the app reflect the change.
#[tauri::command]
pub async fn update_community_metadata(
    community_id: String,
    name: Option<String>,
    description: Option<String>,
) -> Result<(), String> {
    vector_core::db::scoped(async move {
        let session = vector_core::db::current_session();
        let id_bytes = hex_to_id32(&community_id)?;
        vector_core::VectorCore
            .edit_community_metadata(&community_id, name.as_deref(), description.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        if let Ok(Some(c)) = vector_core::db::community::load_community_v2(&CommunityId(id_bytes)) {
            vector_core::VectorCore.register_v2_chats(&c, &session).await;
        }
        Ok(())
    })
    .await
}

/// The iroh relays a community's voice channels use; empty means iroh's defaults.
#[tauri::command]
pub async fn set_community_voice_relays(community_id: String, relays: Vec<String>) -> Result<Vec<String>, String> {
    vector_core::db::scoped(async move { vector_core::VectorCore.set_voice_relays(&community_id, &relays).await.map_err(|e| e.to_string()) }).await
}

/// Rename a channel (requires manage-channels authority) and republish its ChannelMetadata so members
/// pick it up. `channel_id` is the channel's hex id.
#[tauri::command]
pub async fn rename_community_channel(
    community_id: String,
    channel_id: String,
    name: String,
) -> Result<(), String> {
    vector_core::db::scoped(async move {
        let session = vector_core::db::current_session();
        let id_bytes = hex_to_id32(&community_id)?;
        let ch_bytes = hex_to_id32(&channel_id)?;
        let ch_id = vector_core::community::ChannelId(ch_bytes);
        let community = vector_core::db::community::load_community_v2(&CommunityId(id_bytes))?
            .ok_or("Community not found")?;
        let held = community.channel(&ch_id).ok_or("Channel not found in Community")?;
        // Rebuild from the held document — a rename must never strip vsk-2
        // fields it didn't touch (CORD-02 §6).
        let mut meta = held.metadata();
        meta.name = name.clone();
        let transport = LiveTransport::with_timeout(Duration::from_secs(12));
        vector_core::community::v2::service::edit_channel_metadata(&transport, &community, &ch_id, &meta).await?;
        if let Ok(Some(c)) = vector_core::db::community::load_community_v2(&CommunityId(id_bytes)) {
            vector_core::VectorCore.register_v2_chats(&c, &session).await;
        }
        Ok(())
    })
    .await
}

/// Add a channel to a **v2** Community (CORD-03 §1-2). A public channel derives its
/// Chat-Plane key from the community root; a private one mints its own and rekeys the
/// current members, which is why it takes the longer transport budget. A non-v2 community
/// is refused rather than silently no-op'd.
/// Authorization (`MANAGE_CHANNELS`, or owner) is enforced in vector-core.
#[tauri::command]
pub async fn create_community_channel(
    community_id: String,
    name: String,
    private: Option<bool>,
    category_id: Option<String>,
    voice: Option<bool>,
) -> Result<String, String> {
    vector_core::db::scoped_result(create_community_channel_inner(community_id, name, private, category_id, voice.unwrap_or(false))).await
}

async fn create_community_channel_inner(
    community_id: String,
    name: String,
    private: Option<bool>,
    category_id: Option<String>,
    voice: bool,
) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("A channel needs a name".to_string());
    }
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community_v2(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    let transport = LiveTransport::with_timeout(Duration::from_secs(20));
    let category = category_id.as_deref().map(hex_to_id32).transpose()?;
    let channel_id = vector_core::community::v2::service::create_channel_in(&transport, &community, trimmed, private.unwrap_or(false), category).await?;
    let channel_hex = vector_core::simd::hex::bytes_to_hex_32(&channel_id.0);
    if voice {
        vector_core::VectorCore.edit_channel_details(&community_id, &channel_hex, trimmed, None, "voice").await.map_err(|e| e.to_string())?;
    }
    // Re-load: the create wrote the new channel onto the held document, and registering
    // from the stale in-memory copy would drop it again.
    if let Ok(Some(c)) = vector_core::db::community::load_community_v2(&CommunityId(id_bytes)) {
        vector_core::VectorCore.register_v2_chats(&c, &vector_core::db::current_session()).await;
    }
    Ok(vector_core::simd::hex::bytes_to_hex_32(&channel_id.0))
}

/// Tombstone a channel in a **v2** Community (CORD-03 §2). The primary channel is refused:
/// it anchors the community's chat-list row and its history. Message history for a deleted
/// channel stays in the local DB; the community document is the authority on which channels
/// exist, so a tombstoned one simply stops being listed.
#[tauri::command]
pub async fn delete_community_channel(community_id: String, channel_id: String) -> Result<(), String> {
    vector_core::db::scoped_result(delete_community_channel_inner(community_id, channel_id)).await
}

async fn delete_community_channel_inner(community_id: String, channel_id: String) -> Result<(), String> {
    let id_bytes = hex_to_id32(&community_id)?;
    let ch_id = vector_core::community::ChannelId(hex_to_id32(&channel_id)?);
    let community = vector_core::db::community::load_community_v2(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    if community.primary_channel().is_some_and(|p| p.id.0 == ch_id.0) {
        return Err("The community's first channel can't be deleted".to_string());
    }
    let name = community
        .channel(&ch_id)
        .map(|c| c.name.clone())
        .ok_or("Channel not found in Community")?;
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    vector_core::community::v2::service::delete_channel(&transport, &community, &ch_id, &name).await?;
    if let Ok(Some(c)) = vector_core::db::community::load_community_v2(&CommunityId(id_bytes)) {
        vector_core::VectorCore.register_v2_chats(&c, &vector_core::db::current_session()).await;
    }
    Ok(())
}

/// Resolve a Community's logo (or banner) to a local cached file path: download the
/// encrypted blob, decrypt it with the per-image key from the (already-loaded) metadata,
/// verify the plaintext hash, and cache it. Returns `None` if the Community has no such
/// image. Mirrors `cache_group_avatar`, but uses Vector's attachment crypto.
#[tauri::command]
pub async fn cache_community_image(
    community_id: String,
    is_banner: bool,
) -> Result<Option<String>, String> {
    let handle = crate::TAURI_APP.get().ok_or("App handle not initialized")?.clone();
    let id = vector_core::simd::hex::bytes_to_hex_32(&hex_to_id32(&community_id)?);
    let Some(image) = vector_core::db::community::community_image(&id, is_banner)? else {
        return Ok(None);
    };
    let kind = if is_banner { crate::image_cache::ImageType::Banner } else { crate::image_cache::ImageType::Avatar };
    download_decrypt_cache_image(&handle, &image, kind).await.map(Some)
}

/// Resolve an invite-preview logo to a local cached file path. Unlike `cache_community_image`
/// this works BEFORE the community is joined/persisted: the `CommunityImage` (url/key/nonce/hash)
/// arrives straight from `preview_public_invite`, so we decrypt it without a DB lookup.
#[tauri::command]
pub async fn cache_invite_logo(
    image: vector_core::community::CommunityImage,
) -> Result<String, String> {
    let handle = crate::TAURI_APP.get().ok_or("App handle not initialized")?.clone();
    download_decrypt_cache_image(&handle, &image, crate::image_cache::ImageType::Avatar).await
}

/// Download an encrypted community image blob, decrypt + verify it against the committed hash,
/// and cache the plaintext. Returns the local file path. Shared by `cache_community_image`
/// (joined communities) and `cache_invite_logo` (invite previews).
async fn download_decrypt_cache_image<R: tauri::Runtime>(
    handle: &tauri::AppHandle<R>,
    image: &vector_core::community::CommunityImage,
    kind: crate::image_cache::ImageType,
) -> Result<String, String> {
    // Fast path: already cached (keyed by the encrypted blob URL).
    if let Some(path) = crate::image_cache::get_cached_path(handle, &image.url, kind) {
        return Ok(path);
    }

    // Download the ciphertext (Tor failsafe applies via build_http_client), bounded. Banners get
    // more room: other clients upload their crops uncompressed.
    let max_img: usize = if kind == crate::image_cache::ImageType::Banner { 16 } else { 10 } * 1024 * 1024;
    let client = vector_core::net::build_http_client(std::time::Duration::from_secs(30))?;
    // An encrypted community logo is still a download from somebody's Blossom
    // server: proxied and signed like every other picture when the setting is on.
    let mut resp = vector_core::net::proxied_request(&client, reqwest::Method::GET, &image.url)
        .await
        .send()
        .await
        .map_err(|e| format!("download: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download failed: HTTP {}", resp.status()));
    }
    // Stream with a hard cap: `content_length` is absent under chunked transfer-encoding, so
    // a single buffered read could OOM on a hostile/oversized blob. Abort as soon as the
    // running total exceeds the cap (memory bounded to the cap + one chunk).
    let mut encrypted: Vec<u8> = Vec::with_capacity(
        resp.content_length().map(|l| (l as usize).min(max_img)).unwrap_or(64 * 1024),
    );
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("read body: {e}"))? {
        if encrypted.len() + chunk.len() > max_img {
            return Err("community image too large".to_string());
        }
        encrypted.extend_from_slice(&chunk);
    }

    let decrypted = vector_core::crypto::decrypt_data_owned(encrypted, &image.key, &image.nonce)?;
    // Integrity: the plaintext must match the hash committed in the sealed metadata.
    if vector_core::crypto::sha256_hex(&decrypted) != image.hash {
        return Err("community image failed integrity check".to_string());
    }
    if vector_core::svg::looks_like_svg(&decrypted) {
        return Err(vector_core::community::SVG_REFUSED.to_string());
    }

    match crate::image_cache::precache_image_bytes(
        handle,
        &image.url,
        &decrypted,
        kind,
    ) {
        crate::image_cache::CacheResult::Cached(p)
        | crate::image_cache::CacheResult::AlreadyCached(p) => Ok(p),
        crate::image_cache::CacheResult::Failed(e) => Err(format!("cache image: {e}")),
    }
}

pub use vector_core::image_crop::CropRect;

/// Set a Community's logo or banner: encrypt the image at `filepath` with a
/// fresh per-file key (NIP-17 attachment technique), upload the ciphertext to Blossom,
/// store the ref in the community metadata edition, and publish it. `is_banner` targets the
/// banner instead of the icon. Authority (MANAGE_METADATA, the same as name/description
/// edits) is enforced by the control fold.
#[tauri::command]
pub async fn set_community_image(
    community_id: String,
    filepath: String,
    is_banner: bool,
    crop: Option<CropRect>,
) -> Result<(), String> {
    vector_core::db::scoped(async move {
        let session = vector_core::db::current_session();
        let id_bytes = hex_to_id32(&community_id)?;
        // Existence-check BEFORE the expensive read + encrypt + upload.
        let v2 = vector_core::db::community::load_community_v2(&CommunityId(id_bytes))?
            .ok_or("Community not found")?;

        // Read the picked file. On Android the file dialog returns a content:// URI that std::fs can't
        // open, so route it through the JNI content-resolver (the same path upload_avatar uses); desktop
        // reads the real path directly. Without this, picking an image on Android failed instantly at the
        // read ("read image: ...") and surfaced as the generic "Failed to update the image" toast — for
        // the owner too, not just admins.
        let (raw_bytes, _raw_ext): (Vec<u8>, String) = {
            #[cfg(not(target_os = "android"))]
            {
                let bytes = std::fs::read(&filepath).map_err(|e| format!("read image: {e}"))?;
                let ext = std::path::Path::new(&filepath)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("png")
                    .to_lowercase();
                (bytes, ext)
            }
            #[cfg(target_os = "android")]
            {
                let af = crate::android::filesystem::read_android_uri(filepath.clone())?;
                ((*af.bytes).clone(), af.extension)
            }
        };

        if vector_core::svg::looks_like_svg(&raw_bytes) {
            return Err(vector_core::community::SVG_REFUSED.to_string());
        }
        let raw_bytes = match crop {
            Some(c) => tokio::task::spawn_blocking(move || crate::commands::emoji_packs::crop_drawn(raw_bytes, c))
            .await
            .map_err(|e| format!("crop join: {e}"))??,
            None => raw_bytes,
        };
        // Strip metadata + resize + cap before encrypting (parity with profile images).
        // Members, and anyone the community key reaches, must not receive the owner's
        // camera EXIF; the re-encode also shrinks the blob every member downloads.
        let prepared = crate::shared::image::prepare_upload_image(
            &raw_bytes,
            if is_banner {
                crate::shared::image::UploadImageKind::Banner
            } else {
                crate::shared::image::UploadImageKind::Avatar
            },
        )?;
        let bytes = prepared.bytes;
        let ext = prepared.extension.to_string();

        let plaintext_hash = vector_core::crypto::sha256_hex(&bytes);

        // Encrypt with a fresh random key+nonce (same as file attachments); the key rides in
        // the members-only metadata edition, so only members can decrypt the blob.
        let params = vector_core::crypto::generate_encryption_params();
        let encrypted = vector_core::crypto::encrypt_data(&bytes, &params)?;

        let _client = vector_core::state::nostr_client().ok_or("Nostr client not initialised")?;
        let signer = vector_core::signer::active_signer().map_err(|e| format!("signer: {e}"))?;
        let servers = vector_core::state::get_blossom_servers();
        if servers.is_empty() {
            return Err("No Blossom servers configured.".to_string());
        }
        // Emit upload progress so the avatar shows a ring (parity with profile avatars) instead of freezing
        // until the new icon appears. `is_encrypted = true` routes the encrypted blob to servers that accept
        // encrypted types (the same ranking file attachments use).
        let cid_for_progress = community_id.clone();
        let progress_cb: vector_core::blossom::ProgressCallback = std::sync::Arc::new(move |pct, _bytes| {
            vector_core::emit_event(
                "community_image_upload_progress",
                &serde_json::json!({ "community_id": cid_for_progress, "progress": pct.unwrap_or(0), "is_banner": is_banner }),
            );
            Ok(())
        });
        let url = vector_core::blossom::upload_blob_with_progress_and_failover(
            signer,
            servers,
            std::sync::Arc::new(encrypted),
            Some("application/octet-stream"),
            true,
            progress_cb,
            None,
            None,
            None,
        )
        .await?
        // Community images are never mirrored, so only the descriptor matters.
        .url;


        // The image is a field of the vsk-0 metadata document. Overlay onto the
        // FULL held document (`metadata()`) so setting one image can't wipe the
        // other fields, then persist locally for instant display — the control-fold
        // echo re-applies the same head idempotently.
        let mut extra = serde_json::Map::new();
        extra.insert("ext".into(), serde_json::Value::String(ext.clone()));
        let img_ref = vector_core::community::v2::control::ImageRef {
            url: url.clone(),
            key: params.key.clone(),
            nonce: params.nonce.clone(),
            hash: plaintext_hash.clone(),
            extra,
        };
        let mut meta = v2.metadata();
        if is_banner {
            meta.banner = Some(img_ref.clone());
        } else {
            meta.icon = Some(img_ref.clone());
        }
        let transport = LiveTransport::with_timeout(Duration::from_secs(12));
        vector_core::community::v2::service::edit_community_metadata(&transport, &v2, &meta).await?;
        if let Some(updated) = vector_core::community::v2::service::persist_community_image(
            v2.id(),
            img_ref,
            is_banner,
        )
        .await
        {
            vector_core::VectorCore.register_v2_chats(&updated, &session).await;
        }
        Ok(())
    })
    .await
}

// ============================================================================
// Public (link) invites
// ============================================================================

/// Mint a shareable public-invite URL for a Community the user owns. `expires_in_secs`
/// (optional) sets a client-enforced expiry. Returns the URL.
#[tauri::command]
pub async fn create_public_invite(
    community_id: String,
    expires_in_secs: Option<u64>,
    label: Option<String>,
) -> Result<String, String> {
    vector_core::db::scoped(async move {
        // v2 mints a naddr#fragment link. The facade takes an ABSOLUTE ms deadline;
        // `expires_in_secs` is a duration, so resolve it against now before converting.
        let expires_at_ms = expires_in_secs.map(|secs| now_secs().saturating_add(secs).saturating_mul(1000));
        vector_core::VectorCore
            .create_public_invite(&community_id, expires_at_ms, label)
            .await
            .map_err(|e| e.to_string())
    })
    .await
}

/// Preview payload for the GUI: the bundle's public preview plus the community id, so a
/// rendered invite (chat card, join dialog) can tell "already joined" from "new".
#[derive(serde::Serialize)]
pub struct PublicInvitePreviewInfo {
    #[serde(flatten)]
    pub preview: PublicInvitePreview,
    pub community_id: String,
}

/// Fetch + decrypt the preview for a public-invite URL (shown before joining). Read-only.
#[tauri::command]
pub async fn preview_public_invite(url: String) -> Result<PublicInvitePreviewInfo, String> {
    // Fetch the (encrypted) bundle and fold the live Control Plane for the real
    // name/description/icon — a bundle deliberately carries no icon (the fold is the
    // authority). Read-only; owner-root verification rides the fold, so a forged link
    // can't render a convincing preview.
    vector_core::community::v2::invite::parse_invite_link(&url).map_err(|_| "Not a Vector invite link".to_string())?;
    {
        let transport = LiveTransport::with_timeout(Duration::from_secs(12));
        let bundle = vector_core::community::v2::service::fetch_public_bundle(&transport, &url).await?;
        // The bundle's `community_id` is self-certifying (sha256 over owner+salt, verified as the
        // bundle is decrypted), so it names the community without folding anything.
        let community_id = bundle.community_id.clone();
        // Already a member → local state IS the preview (the community's own sync keeps it fresh):
        // one source of truth, zero divergence. Short-circuit BEFORE the fold — that walk is the
        // join gate (`accept_public_link` runs it again anyway) and no join is coming.
        if let Ok(id_bytes) = hex_to_id32(&community_id) {
            if let Ok(Some(local)) = vector_core::db::community::load_community_v2(&CommunityId(id_bytes)) {
                return Ok(PublicInvitePreviewInfo {
                    preview: PublicInvitePreview {
                        name: local.name.clone(),
                        description: local.description.clone(),
                        icon: local.icon.as_ref().map(|i| i.to_community_image()),
                    },
                    community_id,
                });
            }
        }
        let folded = vector_core::community::v2::service::preview_bundle(&transport, &bundle).await?;
        Ok(PublicInvitePreviewInfo {
            preview: PublicInvitePreview {
                name: folded.name.clone(),
                description: folded.description.clone(),
                icon: folded.icon.as_ref().map(|i| i.to_community_image()),
            },
            community_id,
        })
    }
}

/// Accept a public-invite URL: fetch the bundle, join as a member (expiry + id-collision
/// guarded), and start receiving.
#[tauri::command]
pub async fn accept_public_invite(url: String) -> Result<CommunitySummary, String> {
    vector_core::db::scoped(async move {
        // The facade join verifies the owner root, persists, registers chats, publishes the
        // cross-device join, and starts the planes.
        let summary = vector_core::VectorCore.join_community(&url).await.map_err(|e| e.to_string())?;
        let cid = summary
            .get("community_id")
            .or_else(|| summary.get("id"))
            .and_then(|v| v.as_str())
            .ok_or("v2 join returned no community id")?;
        let id_bytes = hex_to_id32(cid)?;
        summarize_any(&CommunityId(id_bytes)).ok_or_else(|| "joined community not found".to_string())
    })
    .await
}

/// List the active public-invite links the user has minted for a Community.
#[tauri::command]
pub async fn list_public_invites(
    community_id: String,
) -> Result<Vec<vector_core::db::community::PublicInviteRecord>, String> {
    vector_core::db::community::list_public_invites(&community_id)
}

/// Revoke a public-invite link: delete the bundle on the Community relays and forget the
/// token locally. `token` is the 64-char hex token (from `list_public_invites`).
#[tauri::command]
pub async fn revoke_public_invite(community_id: String, token: String) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.revoke_public_invite(&community_id, &token).await.map_err(|e| e.to_string())
    })
    .await
}

/// Current Unix time in seconds.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Decode a deterministic 64-char hex id (DB-stored community/channel id, our own encrypted
/// self-list, or a frontend-supplied command param — never a raw inbound-event field, which is
/// validated at the network boundary in event_handler's invite parse) into 32 bytes via the SIMD
/// hex path. The length guard keeps the fallible contract the call sites rely on; the decode is the
/// benchmarked `hex_to_bytes_32` (which assumes well-formed input — guaranteed by the provenance above).
fn hex_to_id32(hex: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 {
        return Err(format!("expected 64 hex chars, got {}", hex.len()));
    }
    Ok(vector_core::simd::hex::hex_to_bytes_32(hex))
}

// Handlers: list_communities, get_community, leave_community,
// create_community, send_community_message,
// invite_to_community, list_community_invites, accept_community_invite,
// decline_community_invite, create_public_invite, preview_public_invite,
// accept_public_invite, list_public_invites, revoke_public_invite,
// update_community_metadata, set_community_image, cache_community_image

// ── Pins (CORD-04 §7) ────────────────────────────────────────────────────────

/// Pin a message in a v2 community channel. Authority is enforced by every
/// reader's fold (PIN_MESSAGES); the frontend gates the affordance on
/// `get_community_capabilities().pin_messages`.
#[tauri::command]
pub async fn pin_community_message(community_id: String, channel_id: String, message_id: String) -> Result<(), String> {
    vector_core::VectorCore
        .pin_community_message(&community_id, &channel_id, &message_id)
        .await
        .map_err(|e| e.to_string())
}

/// Unpin a message: the next Pin List edition without the entry.
#[tauri::command]
pub async fn unpin_community_message(community_id: String, channel_id: String, message_id: String) -> Result<(), String> {
    vector_core::VectorCore
        .unpin_community_message(&community_id, &channel_id, &message_id)
        .await
        .map_err(|e| e.to_string())
}

/// A channel's verified pins from the locally folded head. `sealed: true`
/// means the list exists but is unreadable here — the UI must render that as
/// unavailable, never as empty. Live updates arrive as `community_pins_updated`.
#[tauri::command]
pub fn get_channel_pins(community_id: String, channel_id: String) -> Result<serde_json::Value, String> {
    vector_core::VectorCore
        .get_channel_pins(&community_id, &channel_id)
        .map_err(|e| e.to_string())
}

/// Fetch a pinned attachment from the pin proof alone (CORD-04 §7): the
/// disclosed keys open the rumor, whose imeta carries the blob URL + file
/// decryption material — so media pins render for members with no chat
/// history and no old epoch key. Downloads/decrypts/caches on first touch,
/// serves the verified local copy after.
#[tauri::command]
pub async fn fetch_pinned_attachment(community_id: String, channel_id: String, message_id: String) -> Result<serde_json::Value, String> {
    vector_core::VectorCore
        .fetch_pinned_attachment(&community_id, &channel_id, &message_id)
        .await
        .map_err(|e| e.to_string())
}

// ── Moderation panel (Concord v2) ────────────────────────────────────────────

/// The moderation panel's whole snapshot in one read: every member with the raid
/// evidence behind their verdict, the duplicate-text cohorts, the live invite links,
/// banlist headroom and the caller's capabilities. Read-only.
#[tauri::command]
pub async fn get_moderation_intel(community_id: String) -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.community_moderation_intel(&community_id).map_err(|e| e.to_string())
    })
    .await
}

/// The preset library a designer offers instead of a blank document.
#[tauri::command]
pub async fn policy_presets() -> Result<serde_json::Value, String> {
    vector_core::VectorCore.policy_presets().map_err(|e| e.to_string())
}

/// What a candidate policy WOULD do against real history. Read-only: stores
/// nothing, publishes nothing, removes nobody.
#[tauri::command]
pub async fn preview_community_policy(community_id: String, bytes: String) -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.preview_community_policy(&community_id, &bytes).map_err(|e| e.to_string())
    })
    .await
}

/// A community's stored policies, with the validator's verdict on each.
#[tauri::command]
pub async fn list_community_policies(community_id: String) -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.list_community_policies(&community_id).map_err(|e| e.to_string())
    })
    .await
}

/// Store a policy. Validated before it is written: an invalid policy is a
/// rejected edit, never a stored one that quietly evaluates to nothing.
#[tauri::command]
pub async fn set_community_policy(
    community_id: String,
    policy_id: String,
    bytes: String,
    enabled: bool,
) -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore
            .set_community_policy(&community_id, &policy_id, &bytes, enabled)
            .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn delete_community_policy(community_id: String, policy_id: String) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.delete_community_policy(&community_id, &policy_id).map_err(|e| e.to_string())
    })
    .await
}

/// The built-in policy as editable JSON — what an editor opens instead of a
/// blank document.
#[tauri::command]
pub async fn builtin_policy_json() -> Result<String, String> {
    vector_core::VectorCore.builtin_policy_json().map_err(|e| e.to_string())
}

/// Run the policy engine beside the shipped assessor and report where they
/// disagree. Read-only: nothing is published, nobody is removed.
#[tauri::command]
pub async fn policy_side_by_side(community_id: String) -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.policy_side_by_side(&community_id).map_err(|e| e.to_string())
    })
    .await
}

/// Ban a whole wave as ONE moderation unit: one banlist edition, one strip, one
/// re-founding. Banning them one by one would mint a rotation per member and let
/// racing editions erase each other.
#[tauri::command]
pub async fn ban_community_members(community_id: String, npubs: Vec<String>) -> Result<(), String> {
    vector_core::db::scoped(async move {
        let refs: Vec<&str> = npubs.iter().map(|s| s.as_str()).collect();
        vector_core::VectorCore
            .set_members_banned(&community_id, &refs, true)
            .await
            .map_err(|e| e.to_string())?;
        vector_core::VectorCore::invalidate_raid_report(&community_id);
        // Their messages stop rendering the moment they are banned, so a badge counting
        // them sends the reader hunting for something the client will never show. The
        // unread cache is maintained incrementally and cannot know a ban happened —
        // recompute every channel they could have posted in, straight from the DB.
        reconcile_community_unread(&community_id).await;
        // The rotation moved every plane pseudonym; without this our live subscription
        // stays pinned to the buried epoch until the next sync.
        if vector_core::db::session_is_live() {
            crate::services::subscription_handler::refresh_community_subscription().await;
        }
        Ok(())
    })
    .await
}

/// Re-register a community's chat rows after its channel document changed here.
async fn reregister_channels(community_id: &str) {
    if let Ok(id) = hex_to_id32(community_id) {
        if let Ok(Some(c)) = vector_core::db::community::load_community_v2(&CommunityId(id)) {
            vector_core::VectorCore.register_v2_chats(&c, &vector_core::db::current_session()).await;
        }
    }
}

/// Edit a channel's name, topic and view (`chat`, `forum` or `voice`), keeping every
/// field another client wrote.
#[tauri::command]
pub async fn edit_community_channel(community_id: String, channel_id: String, name: String, topic: Option<String>, view: String) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore
            .edit_channel_details(&community_id, &channel_id, &name, topic.as_deref(), &view)
            .await
            .map_err(|e| e.to_string())?;
        reregister_channels(&community_id).await;
        Ok(())
    })
    .await
}

/// Make a channel private (it takes a key of its own) or public again.
#[tauri::command]
pub async fn set_community_channel_private(community_id: String, channel_id: String, private: bool) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.set_channel_private(&community_id, &channel_id, private).await.map_err(|e| e.to_string())?;
        reregister_channels(&community_id).await;
        Ok(())
    })
    .await
}

/// Move a channel into a category (none = uncategorized) at `index` among its channels.
#[tauri::command]
pub async fn move_community_channel(community_id: String, channel_id: String, category_id: Option<String>, index: u32) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore
            .move_channel(&community_id, &channel_id, category_id.as_deref(), index as usize)
            .await
            .map_err(|e| e.to_string())
    })
    .await
}

/// Create a category at the bottom of the sidebar; returns its id.
#[tauri::command]
pub async fn create_community_category(community_id: String, name: String) -> Result<String, String> {
    vector_core::db::scoped(async move { vector_core::VectorCore.create_category(&community_id, &name).await.map_err(|e| e.to_string()) }).await
}

#[tauri::command]
pub async fn rename_community_category(community_id: String, category_id: String, name: String) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.rename_category(&community_id, &category_id, &name).await.map_err(|e| e.to_string())
    })
    .await
}

/// Delete a category; its channels render uncategorized.
#[tauri::command]
pub async fn delete_community_category(community_id: String, category_id: String) -> Result<(), String> {
    vector_core::db::scoped(async move { vector_core::VectorCore.delete_category(&community_id, &category_id).await.map_err(|e| e.to_string()) }).await
}

/// Move a category to `index` among the categories (0 is the top).
#[tauri::command]
pub async fn move_community_category(community_id: String, category_id: String, index: u32) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.move_category(&community_id, &category_id, index as usize).await.map_err(|e| e.to_string())
    })
    .await
}

/// Delete a role (new roles only): it leaves every holder.
#[tauri::command]
pub async fn delete_community_role(community_id: String, role_id: String) -> Result<(), String> {
    vector_core::db::scoped(async move { vector_core::VectorCore.delete_role(&community_id, &role_id).await.map_err(|e| e.to_string()) }).await
}

/// Set what `@everyone` holds; `permissions` is a decimal string.
#[tauri::command]
pub async fn set_community_everyone(community_id: String, permissions: String) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.set_everyone_permissions(&community_id, &permissions).await.map_err(|e| e.to_string())
    })
    .await
}

/// Move a community to the new roles (owner only); returns how many roles carried over.
#[tauri::command]
pub async fn activate_community_roles_v2(community_id: String, everyone: String) -> Result<usize, String> {
    vector_core::db::scoped(async move { vector_core::VectorCore.activate_roles_v2(&community_id, &everyone).await.map_err(|e| e.to_string()) }).await
}

/// A channel's permission overwrites, read locally.
#[tauri::command]
pub fn get_channel_permissions(community_id: String, channel_id: String) -> Result<serde_json::Value, String> {
    vector_core::VectorCore.channel_permissions_view(&community_id, &channel_id).map_err(|e| e.to_string())
}

/// Set, or with no bits clear, one overwrite on a channel or category. Bits are decimal strings.
#[tauri::command]
pub async fn set_channel_overwrite(
    community_id: String,
    scope_id: String,
    kind: String,
    target_id: Option<String>,
    allow: String,
    deny: String,
) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore
            .set_channel_overwrite(&community_id, &scope_id, &kind, target_id.as_deref(), &allow, &deny)
            .await
            .map_err(|e| e.to_string())?;
        reregister_channels(&community_id).await;
        Ok(())
    })
    .await
}

/// Sync a channel to a category's permissions, or (no category) back to its own.
#[tauri::command]
pub async fn set_channel_sync(community_id: String, channel_id: String, category_id: Option<String>) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore
            .set_channel_sync(&community_id, &channel_id, category_id.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        reregister_channels(&community_id).await;
        Ok(())
    })
    .await
}

/// Who may mention everyone, and the community's Roles for mentioning, read locally.
#[tauri::command]
pub fn get_community_mentions(community_id: String) -> Result<serde_json::Value, String> {
    vector_core::VectorCore.community_mentions(&community_id).map_err(|e| e.to_string())
}

/// Of `npubs`, those who can't send in a channel: their posts there collapse.
#[tauri::command]
pub fn get_channel_send_denied(community_id: String, channel_id: String, npubs: Vec<String>) -> Result<Vec<String>, String> {
    vector_core::VectorCore.channel_send_denied(&community_id, &channel_id, &npubs).map_err(|e| e.to_string())
}

/// The Timeouts running in a community: npub → { from, until }.
#[tauri::command]
pub fn get_community_timeouts(community_id: String) -> Result<serde_json::Value, String> {
    vector_core::VectorCore.community_timeouts(&community_id).map_err(|e| e.to_string())
}

/// Time a member out for `seconds` (at most 28 days); 0 lifts it.
#[tauri::command]
pub async fn timeout_community_member(community_id: String, npub: String, seconds: u64) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.timeout_member(&community_id, &npub, seconds).await.map_err(|e| e.to_string())
    })
    .await
}

/// Everything the role editor needs, read locally.
#[tauri::command]
pub fn get_community_roles_view(community_id: String) -> Result<serde_json::Value, String> {
    vector_core::VectorCore.community_roles_view(&community_id).map_err(|e| e.to_string())
}

/// Create a Role at the bottom of the list; returns its id. `permissions` is a decimal string.
#[tauri::command]
pub async fn create_community_role(
    community_id: String,
    name: String,
    color: u32,
    permissions: String,
    channel_id: Option<String>,
) -> Result<String, String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore
            .create_role(&community_id, &name, color, &permissions, channel_id.as_deref())
            .await
            .map_err(|e| e.to_string())
    })
    .await
}

/// Edit a Role's name, colour, permissions and channel scope.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn edit_community_role(
    community_id: String,
    role_id: String,
    name: String,
    color: u32,
    permissions: String,
    channel_id: Option<String>,
    display: Option<bool>,
    mentionable: Option<bool>,
) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore
            .edit_role_shown(&community_id, &role_id, &name, color, &permissions, channel_id.as_deref(), display, mentionable)
            .await
            .map_err(|e| e.to_string())
    })
    .await
}

/// Reorder the roles beneath the caller, top to bottom; returns how many moved.
#[tauri::command]
pub async fn reorder_community_roles(community_id: String, ordered: Vec<String>) -> Result<usize, String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.reorder_roles(&community_id, &ordered).await.map_err(|e| e.to_string())
    })
    .await
}

/// Set exactly which Roles a member holds.
#[tauri::command]
pub async fn set_community_member_roles(community_id: String, npub: String, role_ids: Vec<String>) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore
            .set_member_roles(&community_id, &npub, role_ids)
            .await
            .map_err(|e| e.to_string())
    })
    .await
}

/// Lift several bans as one banlist edition. An unban never rotates (only a fresh
/// ban severs), so this is one publish however many are selected, where lifting them
/// one by one would publish, and race, an edition each.
#[tauri::command]
pub async fn unban_community_members(community_id: String, npubs: Vec<String>) -> Result<(), String> {
    vector_core::db::scoped(async move {
        let refs: Vec<&str> = npubs.iter().map(|s| s.as_str()).collect();
        vector_core::VectorCore
            .set_members_banned(&community_id, &refs, false)
            .await
            .map_err(|e| e.to_string())?;
        // Their messages render again, and the unread cache cannot know that.
        reconcile_community_unread(&community_id).await;
        // The live routes carry the banned set they were built with.
        if vector_core::db::session_is_live() {
            crate::services::subscription_handler::refresh_community_subscription().await;
        }
        Ok(())
    })
    .await
}

/// Recompute the unread badge for every channel of a Community.
///
/// Used after a membership change that retroactively hides messages (a ban), where an
/// incremental delta cannot be derived — the count has to come back from the query that
/// now excludes them.
async fn reconcile_community_unread(community_id: &str) {
    let Ok(id_bytes) = vector_core::simd::hex::hex_to_bytes_32_checked(community_id).ok_or(()) else {
        return;
    };
    let cid = vector_core::community::CommunityId(id_bytes);
    let channels: Vec<String> = vector_core::db::community::load_community_v2(&cid)
        .ok()
        .flatten()
        .map(|c| c.channels.iter().map(|ch| vector_core::simd::hex::bytes_to_hex_32(&ch.id.0)).collect())
        .unwrap_or_default();
    for ch in channels {
        crate::commands::messaging::reconcile_chat_unread(&ch).await;
    }
}

/// Rotate the Community's keys, keeping only `retain`. The containment that scales
/// past the 500-npub banlist ceiling: a wave of thousands cannot be banned, but it can
/// be locked out of the next epoch in a single publish. An empty `retain` rotates
/// without removing anyone — the right move when a link leaked but the members are real.
#[tauri::command]
pub async fn refound_community(community_id: String, retain: Vec<String>) -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move {
        let refs: Vec<&str> = retain.iter().map(|s| s.as_str()).collect();
        let cid = community_id.clone();
        // The sweep runs for as long as there are targets, so this counter is the only
        // sign of life the panel has.
        let progress = move |done: usize, total: usize| {
            vector_core::emit_event(
                "community_purge_progress",
                &serde_json::json!({ "community_id": cid, "done": done, "total": total }),
            );
        };
        let summary = vector_core::VectorCore
            .purge_and_refound(&community_id, &refs, &progress)
            .await
            .map_err(|e| e.to_string())?;
        vector_core::VectorCore::invalidate_raid_report(&community_id);
        if vector_core::db::session_is_live() {
            crate::services::subscription_handler::refresh_community_subscription().await;
        }
        Ok(summary)
    })
    .await
}

/// Kick a set of members without touching the keys — roster hygiene on its own.
#[tauri::command]
pub async fn kick_community_members(community_id: String, npubs: Vec<String>) -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move {
        let refs: Vec<&str> = npubs.iter().map(|s| s.as_str()).collect();
        let cid = community_id.clone();
        let progress = move |done: usize, total: usize| {
            vector_core::emit_event(
                "community_purge_progress",
                &serde_json::json!({ "community_id": cid, "done": done, "total": total }),
            );
        };
        let summary = vector_core::VectorCore
            .kick_community_members(&community_id, &refs, &progress)
            .await
            .map_err(|e| e.to_string())?;
        vector_core::VectorCore::invalidate_raid_report(&community_id);
        Ok(summary)
    })
    .await
}

/// Retire every public invite link at once. Revoking them one at a time loses the race
/// against a raid: the next wave walks in through whichever link is still live.
/// Returns `{ revoked, failed }`.
#[tauri::command]
pub async fn revoke_all_public_invites(community_id: String) -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move {
        let (revoked, failed) = vector_core::VectorCore
            .revoke_all_public_invites(&community_id)
            .await
            .map_err(|e| e.to_string())?;
        if vector_core::db::session_is_live() {
            crate::services::subscription_handler::refresh_community_subscription().await;
        }
        Ok(serde_json::json!({ "revoked": revoked, "failed": failed }))
    })
    .await
}

/// Whether this community currently reads as under a raid, for the alert badge on its
/// header. Cheap after the first call: the verdict is memoised alongside the panel's.
#[tauri::command]
pub async fn check_community_raid(community_id: String) -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move {
        vector_core::VectorCore.check_community_raid(&community_id).map_err(|e| e.to_string())
    })
    .await
}

/// Diagnostic: local memberlist vs the wire guestbook fold, with the diff named.
/// `pruned` = counted locally but absent from the current epoch's plane — the set a
/// rotation stranded without keys. `wire_only` = the inverse (joined since our last
/// local fold caught up).
#[tauri::command]
pub async fn debug_v2_memberlist_diff(community_id: String) -> Result<serde_json::Value, String> {
    use nostr_sdk::prelude::ToBech32;
    vector_core::db::scoped(async move {
        let id = CommunityId(hex_to_id32(&community_id)?);
        let c = vector_core::db::community::load_community_v2(&id)?.ok_or("not a held v2 community")?;
        let transport = LiveTransport::with_timeout(Duration::from_secs(20));
        let wire = vector_core::community::v2::service::wire_guestbook_members(&transport, &c).await?;
        let local = vector_core::community::v2::service::stored_memberlist(&c)?;
        let wire_set: std::collections::HashSet<_> = wire.iter().copied().collect();
        let local_set: std::collections::HashSet<_> = local.iter().copied().collect();
        let b32 = |pks: Vec<nostr_sdk::prelude::PublicKey>| -> Vec<String> {
            pks.into_iter().filter_map(|p| p.to_bech32().ok()).collect()
        };
        Ok(serde_json::json!({
            "epoch": c.root_epoch.0,
            "wire": wire.len(),
            "local": local.len(),
            "pruned": b32(local.iter().filter(|p| !wire_set.contains(p)).copied().collect()),
            "wire_only": b32(wire.iter().filter(|p| !local_set.contains(p)).copied().collect()),
        }))
    })
    .await
}
