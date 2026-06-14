//! Community Tauri commands
//!
//! Thin wrappers over vector-core's Community logic. The invite carrier rides a NIP-17
//! gift-wrapped DM to the invitee's npub (the transport): the bundle conveys read keys
//! (server-root + channel keys), never write authority — that is the recipient's roster rank
//! Inbound invites are PARKED for explicit consent (no auto-join, no relay connect);
//! the user accepts or declines via the commands below.

use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::ToBech32;
use vector_core::community::invite::{build_invite_rumor, CommunityInvite};
use vector_core::community::public_invite::{parse_invite_url, PublicInvitePreview};
use vector_core::community::transport::LiveTransport;
use vector_core::community::{service, CommunityId};
use vector_core::sending::{send_rumor_dm, NoOpSendCallback, SendCallback, SendConfig};

/// Write a Community's channel chats into STATE + the chats table with display metadata
/// (name/description/owner/icon-flag), so they load uniformly with DMs at startup (no
/// separate hydrate). Called whenever the Community is created, joined, or its metadata
/// changes.
async fn sync_community_chats(community: &vector_core::community::Community) {
    use nostr_sdk::ToBech32;
    let session = vector_core::state::SessionGuard::capture();
    let is_owner = vector_core::community::service::is_proven_owner(community);
    let has_icon = community.icon.is_some();
    let name = community.name.clone();
    let description = community.description.clone().unwrap_or_default();
    let community_id = community.id.to_hex();
    // Proven owner npub (verified attestation) → stored in custom_fields for the crown + tag.
    let owner_npub = community
        .owner_attestation
        .as_ref()
        .and_then(|att| vector_core::community::owner::verify_owner_attestation(att, &community_id))
        .and_then(|pk| pk.to_bech32().ok());

    // Chatlist sort anchor for a not-yet-active community: prefer JOIN time (when we joined — from the synced
    // list), falling back to the community's founding time. An empty community then sorts by when we joined,
    // never to the bottom (no messages → newest, not oldest).
    let created_at_ms = vector_core::community::list::membership_added_at(&community_id)
        .or_else(|| vector_core::db::community::community_created_at_ms(&community.id));
    let slims = {
        let mut state = vector_core::state::STATE.lock().await;
        let mut slims = Vec::new();
        for ch in &community.channels {
            let channel_id = ch.id.to_hex();
            state.upsert_community_chat(&channel_id, &name, &description, &community_id, is_owner, has_icon, owner_npub.as_deref(), created_at_ms, community.dissolved);
            if let Some(chat) = state.chats.iter().find(|c| c.id == channel_id) {
                slims.push(vector_core::db::chats::SlimChatDB::from_chat(chat, &state.interner));
            }
        }
        slims
    };
    // Don't persist account A's community chat rows into a swapped-in account B's DB.
    if !session.is_valid() {
        return;
    }
    for slim in &slims {
        let _ = vector_core::db::chats::save_slim_chat(slim);
    }
}

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
    /// The PROVEN owner's npub (bech32), derived by verifying the owner attestation — `None` if
    /// absent/unverifiable. The frontend crowns + hoists this npub; it's never an unchecked claim.
    pub owner_npub: Option<String>,
    /// True once a valid owner GroupDissolved tombstone has sealed the community. The frontend
    /// renders the end-of-community marker, disables the composer, and offers a local Remove.
    pub dissolved: bool,
    /// True when a warmed invite preload was promoted on this accept — the chat is already populated
    /// (and its messages emitted), so the frontend can open it IMMEDIATELY instead of awaiting the
    /// first sync (which would otherwise gate the open on the control fold). Always false outside
    /// the accept path.
    #[serde(default)]
    pub preloaded: bool,
}

#[derive(serde::Serialize)]
pub struct ChannelSummary {
    pub channel_id: String,
    pub name: String,
}

fn summarize(community: &vector_core::community::Community) -> CommunitySummary {
    // Derive the proven owner from the attestation (verified against THIS community's id, keyless)
    // — never a stored/asserted npub. None when absent or it doesn't verify.
    let owner_npub = community
        .owner_attestation
        .as_ref()
        .and_then(|att| {
            vector_core::community::owner::verify_owner_attestation(att, &community.id.to_hex())
        })
        .and_then(|pk| pk.to_bech32().ok());
    CommunitySummary {
        community_id: community.id.to_hex(),
        name: community.name.clone(),
        description: community.description.clone(),
        is_owner: vector_core::community::service::is_proven_owner(community),
        has_icon: community.icon.is_some(),
        channels: community
            .channels
            .iter()
            .map(|c| ChannelSummary { channel_id: c.id.to_hex(), name: c.name.clone() })
            .collect(),
        owner_npub,
        dissolved: community.dissolved,
        preloaded: false,
    }
}

/// List every Community the local user holds (owned or joined), for the chat list.
#[tauri::command]
pub async fn list_communities() -> Result<Vec<CommunitySummary>, String> {
    let ids = vector_core::db::community::list_community_ids()?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(c) = vector_core::db::community::load_community(&id)? {
            out.push(summarize(&c));
        }
    }
    Ok(out)
}

/// A best-effort member entry: an observed participant (someone who has posted) + their
/// most-recent activity. Membership is not authoritative (§ no roster); see community.rs.
#[derive(serde::Serialize)]
pub struct CommunityMember {
    pub npub: String,
    pub last_active: u64,
}

/// Observed participants of a Community: distinct authors seen across its channels, newest
/// activity first. The frontend resolves each npub's profile (name/avatar) for display.
#[tauri::command]
pub async fn get_community_members(community_id: String) -> Result<Vec<CommunityMember>, String> {
    let activity = vector_core::db::community::community_member_activity(&community_id)?;
    Ok(activity
        .into_iter()
        .map(|(npub, last_active)| CommunityMember { npub, last_active })
        .collect())
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
    let session = vector_core::state::SessionGuard::capture();
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    let channel_ids: Vec<String> = community.channels.iter().map(|ch| ch.id.to_hex()).collect();
    if !session.is_valid() {
        return Err("account changed during dissolution".to_string());
    }
    // An already-sealed husk (dissolved before teardown existed) skips the publish — the
    // tombstone is out there; this call is just the local cleanup.
    if !vector_core::db::community::get_community_dissolved(&community_id).unwrap_or(false) {
        let transport = LiveTransport::with_timeout(Duration::from_secs(12));
        vector_core::community::service::dissolve_community(&transport, &community).await?;
    }
    // Full local teardown + cross-device list tombstone — a sealed husk lingering in the
    // owner's own DB just re-registers its chat row at every boot ("dissolved group came back").
    if !session.is_valid() {
        return Ok(());
    }
    teardown_community_local(&community_id, &channel_ids, true).await;
    Ok(())
}

/// Kick a member: publish a cooperative kick (3309) into the primary channel. The kicked client
/// self-removes on receipt; peers drop them from the member list. NOT a rekey — a malicious member who
/// ignores the kick is BANNED instead. Requires the caller hold `KICK` and outrank the target (enforced
/// in `publish_kick`, re-verified by peers).
#[tauri::command]
pub async fn kick_community_member(community_id: String, npub: String) -> Result<(), String> {
    let session = vector_core::state::SessionGuard::capture();
    let hex = nostr_sdk::PublicKey::parse(&npub).map_err(|_| "invalid npub".to_string())?.to_hex();
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    let channel = community.channels.first().ok_or("Community has no channel to kick in")?;
    if !session.is_valid() {
        return Err("account changed during kick".to_string());
    }
    let channel_id = channel.id.to_hex();
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    let kick_id = vector_core::community::service::publish_kick(&transport, &community, channel, &hex).await?;
    // The publish above is network-bound (12s timeout) — re-validate before the local write.
    if !session.is_valid() {
        return Ok(());
    }
    // We don't process our OWN kick, so record it locally as a "Member Left" — folds the target out of our
    // member list durably (kick already stripped their grant, so the roster re-assert won't resurrect them)
    // and renders "X left" in chat, matching what peers see on receipt. The inner id dedups with the echo.
    apply_community_presence(&channel_id, &npub, false, &kick_id, now_secs(), None, None).await;
    Ok(())
}

/// The Community's current banlist as npubs (bech32), for the owner's manage-bans UI.
#[tauri::command]
pub async fn get_community_banlist(community_id: String) -> Result<Vec<String>, String> {
    let hexes = vector_core::db::community::get_community_banlist(&community_id)?;
    Ok(hexes
        .iter()
        .filter_map(|h| nostr_sdk::PublicKey::from_hex(h).ok().and_then(|pk| pk.to_bech32().ok()))
        .collect())
}

/// Grant a member the Community's Admin role: publishes the per-member Grant event, adding them
/// to the roster at admin rank so they can sign management actions. Owner/admin-only.
#[tauri::command]
pub async fn grant_community_admin(community_id: String, npub: String) -> Result<(), String> {
    let session = vector_core::state::SessionGuard::capture();
    let member = nostr_sdk::PublicKey::parse(&npub).map_err(|_| "invalid npub".to_string())?;
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    let admin_role_id = admin_role_id(&community_id)?;
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    vector_core::community::service::grant_role(&transport, &community, member, &admin_role_id).await?;
    if !session.is_valid() {
        return Err("account changed during grant".to_string());
    }
    crate::services::subscription_handler::refresh_community_subscription().await;
    Ok(())
}

/// Revoke a member's Admin role (instant-logical revocation). Owner/admin-only.
#[tauri::command]
pub async fn revoke_community_admin(community_id: String, npub: String) -> Result<(), String> {
    let session = vector_core::state::SessionGuard::capture();
    let member = nostr_sdk::PublicKey::parse(&npub).map_err(|_| "invalid npub".to_string())?;
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    let admin_role_id = admin_role_id(&community_id)?;
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    vector_core::community::service::revoke_role(&transport, &community, member, &admin_role_id).await?;
    if !session.is_valid() {
        return Err("account changed during revoke".to_string());
    }
    crate::services::subscription_handler::refresh_community_subscription().await;
    Ok(())
}

/// The npubs (bech32) of members holding a MANAGEMENT role — the admin set, for the member-list
/// crown. (A member holding only a non-management/social role is not an admin.)
#[tauri::command]
pub fn get_community_admins(community_id: String) -> Result<Vec<String>, String> {
    let roles = vector_core::db::community::get_community_roles(&community_id)?;
    Ok(roles
        .grants
        .iter()
        .filter(|g| roles.is_admin(&g.member))
        .filter_map(|g| nostr_sdk::PublicKey::from_hex(&g.member).ok().and_then(|pk| pk.to_bech32().ok()))
        .collect())
}

/// Whether the local user may grant/revoke roles — holds the `MANAGE_ROLES` permission. Drives the
/// member-list crown toggle. Permission-based (the owner is just the uppermost role with every
/// permission), NOT a hardcoded owner check.
#[tauri::command]
pub fn can_manage_community_roles(community_id: String) -> Result<bool, String> {
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    Ok(vector_core::community::service::caller_can_manage_roles(&community))
}

/// The local user's effective management capabilities in a community (role engine — owner is just
/// position 0, NOTHING is owner-hardcoded). Drives which management affordances the UI shows: an admin
/// whose role carries a permission gets the same buttons as the owner. `manage_admin_role` is the crown's
/// gate (can grant/revoke the @admin role = outrank its position; owner-only in the single-role MVP, but
/// computed by the role engine, not hardcoded).
#[tauri::command]
pub fn get_community_capabilities(community_id: String) -> Result<serde_json::Value, String> {
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    let caps = vector_core::community::service::caller_capabilities(&community);
    let manage_admin_role = admin_role_id(&community_id)
        .ok()
        .map(|rid| vector_core::community::service::caller_can_manage_role_id(&community, &rid))
        .unwrap_or(false);
    Ok(serde_json::json!({
        "manage_metadata": caps.manage_metadata,
        "manage_channels": caps.manage_channels,
        "create_invite": caps.create_invite,
        "kick": caps.kick,
        "ban": caps.ban,
        "manage_messages": caps.manage_messages,
        "manage_roles": caps.manage_roles,
        "manage_admin_role": manage_admin_role,
    }))
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
            let npub = nostr_sdk::PublicKey::from_hex(&s.creator_hex).ok()?.to_bech32().ok()?;
            Some(serde_json::json!({ "npub": npub, "count": s.locators.len() }))
        })
        .collect();
    Ok(serde_json::json!({ "is_public": is_public, "creators": creators }))
}

/// The Community's auto-created Admin role id (the server-scope role carrying all management bits).
fn admin_role_id(community_id: &str) -> Result<String, String> {
    let roles = vector_core::db::community::get_community_roles(community_id)?;
    roles
        .roles
        .iter()
        .find(|r| {
            matches!(r.scope, vector_core::community::roles::RoleScope::Server)
                && r.permissions.contains(vector_core::community::roles::Permissions::ADMIN_ALL)
        })
        .map(|r| r.role_id.clone())
        .ok_or_else(|| "Admin role not found (role graph not yet synced?)".to_string())
}

async fn set_member_banned(community_id: &str, npub: &str, banned: bool) -> Result<(), String> {
    let session = vector_core::state::SessionGuard::capture();
    let hex = nostr_sdk::PublicKey::parse(npub).map_err(|_| "invalid npub".to_string())?.to_hex();
    let id_bytes = hex_to_id32(community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    // Recompute the full list (latest-wins): drop any existing entry, then add if banning.
    let mut list = vector_core::db::community::get_community_banlist(community_id)?;
    list.retain(|h| h != &hex);
    if banned {
        list.push(hex);
    }
    if !session.is_valid() {
        return Err("account changed during ban update".to_string());
    }
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    vector_core::community::service::publish_banlist(&transport, &community, &list).await?;
    // Rebuild the live subscription so its cached channel routes carry the fresh `banned` set
    // (COMMUNITY_ROUTES froze each Channel at the last refresh — without this, a banned author's
    // LIVE messages keep flowing until reopen/restart).
    crate::services::subscription_handler::refresh_community_subscription().await;
    Ok(())
}

/// Fetch one Community's summary (for the overview/settings panel).
#[tauri::command]
pub async fn get_community(community_id: String) -> Result<CommunitySummary, String> {
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    Ok(summarize(&community))
}

/// Leave a Community: drop all local state (keys, channels, invites) and stop
/// subscribing. There is no protocol "leave" (membership is key possession), so this is
/// purely local. Also clears the channels' chat rows from STATE.
#[tauri::command]
pub async fn leave_community(community_id: String) -> Result<(), String> {
    let session = vector_core::state::SessionGuard::capture();
    let id_bytes = hex_to_id32(&community_id)?;
    // Capture the full community first (channel ids for chat-row teardown + a leave announce).
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?;
    let channel_ids: Vec<String> = community
        .as_ref()
        .map(|c| c.channels.iter().map(|ch| ch.id.to_hex()).collect())
        .unwrap_or_default();
    if !session.is_valid() {
        return Err("account changed during leave".to_string());
    }
    // Best-effort "left" announcement (kind 3306) BEFORE dropping keys — afterward we can no
    // longer sign/seal into the channel. Honest clients then show "X has left".
    if let Some(ref c) = community {
        if let Some(primary) = c.channels.first() {
            let transport = LiveTransport::with_timeout(Duration::from_secs(12));
            let _ = vector_core::community::service::publish_presence(&transport, c, primary, false, None).await;
        }
    }
    // Voluntary leave on THIS device — propagate the removal to our other devices.
    teardown_community_local(&community_id, &channel_ids, true).await;
    Ok(())
}

/// Tear down local state for a community: drop the DB rows + rebuild the subscription routes WITHOUT it,
/// then clear the channels' chat rows. self-removal teardown: RETAINS the held epoch keys so a later
/// self-scrub of own past messages stays possible. Order matters — drop + refresh routes BEFORE the
/// chat-row teardown, else an in-flight inbound message could route in and recreate a ghost chat after
/// we'd deleted it. Shared by every self-removal trigger (voluntary leave, kick of us, ban-rekey exclusion).
pub(crate) async fn teardown_community_local(community_id: &str, channel_ids: &[String], republish_list: bool) {
    // Capture this community's relays BEFORE deletion so we can drop any that no remaining community
    // needs — and that aren't the user's own relays — from the pool after the routes refresh.
    let left_relays: Vec<String> = hex_to_id32(community_id)
        .ok()
        .and_then(|b| vector_core::db::community::load_community(&CommunityId(b)).ok().flatten())
        .map(|c| c.relays.clone())
        .unwrap_or_default();

    let _ = vector_core::db::community::delete_community_retain_keys(community_id);
    // tombstone it out of the cross-device list. A LOCAL trigger (leave / observed-ban / kick)
    // republishes so our other devices tear it down too; the RECEIVE path (a sibling already published the
    // removal) tombstones locally only — republishing there would re-echo our own event over the live sub.
    if republish_list {
        vector_core::community::list::remove_membership(community_id);
    } else {
        vector_core::community::list::tombstone_local_only(community_id);
    }
    crate::services::subscription_handler::refresh_community_subscription().await;
    {
        let mut state = vector_core::state::STATE.lock().await;
        state.chats.retain(|c| !channel_ids.contains(&c.id));
    }
    for cid in channel_ids {
        let _ = vector_core::db::chats::delete_chat(cid);
        // Reset the RAM sync state to cold — a surviving `since` cursor would make a
        // same-session rejoin fetch only "new since I left" (empty chat despite history).
        vector_core::community::cache::clear_channel_sync_state(cid);
    }
    vector_core::community::cache::abort_preload(community_id);

    // Drop the left community's now-orphaned relays from the pool (bounds growth).
    prune_orphaned_community_relays(&left_relays).await;
}

/// Disconnect + remove relays that belonged to a just-left community, but ONLY the ones nothing
/// else needs. Three protections, any of which keeps a relay:
///   1. another community still lists it (`still_needed`);
///   2. the user reads/writes it — their own primary/imported relay, or a relay that's BOTH theirs
///      and a community's (READ/WRITE flag set; community relays are GOSSIP|PING — see
///      `community_relay_options`). DM recipient inbox relays are added READ+WRITE too, so this
///      also shields any transient chat relay;
///   3. it's a NIP-65 GOSSIP relay (the pool itself refuses to remove those).
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
pub(crate) async fn self_remove_from_community(community_id: &str, republish_list: bool) {
    let channel_ids: Vec<String> = hex_to_id32(community_id)
        .ok()
        .and_then(|b| vector_core::db::community::load_community(&CommunityId(b)).ok().flatten())
        .map(|c| c.channels.iter().map(|ch| ch.id.to_hex()).collect())
        .unwrap_or_default();
    teardown_community_local(community_id, &channel_ids, republish_list).await;
    vector_core::emit_event(
        "community_kicked",
        &serde_json::json!({ "community_id": community_id }),
    );
}

/// If the local user is in `community`'s (already-folded) banlist, self-remove. Call AFTER a
/// `fetch_and_apply_banlist` so the check is authoritative. Returns true if we removed ourselves.
pub(crate) async fn check_self_banned(community_id: &str) -> bool {
    let Some(community) = hex_to_id32(community_id).ok()
        .and_then(|b| vector_core::db::community::load_community(&CommunityId(b)).ok().flatten()) else { return false; };
    if vector_core::community::service::am_i_banned(&community) {
        // Involuntary, detected locally — tombstone local-only; boot's explicit publish propagates it.
        self_remove_from_community(community_id, false).await;
        true
    } else {
        false
    }
}

/// Realtime channel-follow retry budget: a re-founding publishes the channel rekey under the NEW root
/// right after the base rekey, so the base-3303-triggered follow can fetch the channel rekey before it has
/// propagated to our relays. Retry a few times with a short backoff (each retry is sub-second on a hit) so
/// the channel converges in ~realtime instead of stranding until the next sync.
const CHANNEL_FOLLOW_MAX_ATTEMPTS: u32 = 5;
const CHANNEL_FOLLOW_BACKOFF_MS: u64 = 700;

/// Realtime control-plane refresh: a kind-3308 edition (banlist / roles / metadata / invite-links) just
/// arrived for this community, so FOLLOW any re-founding it implies (base + channel rekeys) and re-fold the
/// whole plane, self-removing if WE were just banned. Mirrors `sync_community_channel_inner` so an online
/// member experiences a control change — including a privatize/private-ban re-founding — LIVE rather than
/// only on the next sync; a `community_refreshed` event tells the UI to re-render. Best-effort, SessionGuard
/// across the fetches. Runs SPAWNED off the notification loop, so it may rebuild the subscription when the
/// follow advances an epoch (resubscribe at the new pseudonyms) without re-entering that loop.
/// Per-community in-flight guard for `refresh_community_control` — one 3308 can be delivered by the
/// realtime route, the straggler sink, AND a reconnect resync at once; without this they'd run
/// duplicate concurrent full control catch-ups racing on the same community row.
static REFRESH_CONTROL_INFLIGHT: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

pub(crate) async fn refresh_community_control(community_id: &str) {
    // Claim the in-flight slot or bail (a concurrent refresh is already folding this community).
    {
        let mut inflight = REFRESH_CONTROL_INFLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if !inflight.insert(community_id.to_string()) {
            return;
        }
    }
    // RAII release — panic-safe, so a fold panic can't permanently wedge this community's refreshes.
    struct RefreshClaim(String);
    impl Drop for RefreshClaim {
        fn drop(&mut self) {
            REFRESH_CONTROL_INFLIGHT
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.0);
        }
    }
    let _claim = RefreshClaim(community_id.to_string());

    let session = vector_core::state::SessionGuard::capture();
    let Ok(id_bytes) = hex_to_id32(community_id) else { return; };
    let Some(community) = vector_core::db::community::load_community(&CommunityId(id_bytes)).ok().flatten() else { return; };
    // Longer than the 12s op-norm: this single REQ must fold the WHOLE control plane, and boot/refresh is
    // REQ-heavy (relays contended + ratelimited), so a short cap returns it partial → grants/metadata drop
    // and authority/convergence silently fail closed.
    let bt = LiveTransport::with_timeout(Duration::from_secs(20));
    // Epochs the live subscription is currently pinned to — if the follow below advances either, the sub
    // must be rebuilt (resubscribe at the new pseudonyms) or realtime delivery stays dead at the old epoch.
    let pre_server_epoch = community.server_root_epoch.0;
    let pre_channel_epochs: Vec<(String, u64)> =
        community.channels.iter().map(|c| (c.id.to_hex(), c.epoch.0)).collect();
    // FOLLOW FIRST (mirrors sync_community_channel_inner): a privatize / private-ban re-founds the base
    // under a NEW epoch + re-anchors the control plane there, so walk the rotation BEFORE folding control —
    // else we'd read the stale-epoch coordinate and never advance. This is what makes a re-founding follow
    // in REALTIME (on receipt of the triggering 3308), not only on the next sync. An AUTHORIZED rotation
    // that excluded us is a removal → tear down locally (the catch-all for a cut member who can no longer
    // decrypt the new control plane to see the banlist).
    if let Ok(c) = vector_core::community::service::catch_up_server_root(&bt, &community).await {
        // Re-check BEFORE the destructive teardown: catch_up straddled network I/O, and a mid-flight
        // account swap must not let this tear down the new account's state.
        if !session.is_valid() { return; }
        if c.removed { self_remove_from_community(community_id, false).await; return; }
    }
    if !session.is_valid() { return; }
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes)).ok().flatten().unwrap_or(community);
    // ONE REQ for the whole control plane (banlist + roles + invite-links + metadata), applied banlist-first.
    let _ = vector_core::community::service::fetch_and_apply_control(&bt, &community).await;
    if !session.is_valid() { return; }
    if check_self_banned(community_id).await { return; } // we were banned → torn down, nothing more to do
    if !session.is_valid() { return; }
    // Walk each channel's rekey chain so we hold the current channel keys. INVARIANT: a re-founding rotates
    // the base AND every channel ONCE per base rotation, so each channel must advance by the same delta the
    // base just did. The channel rekey is published under the NEW root right after the base rekey, so a
    // single fetch can RACE its propagation (and there's no channel-rekey-coordinate subscription to re-fire
    // it). Retry with a short backoff until every channel reaches the expected epoch, or the budget is spent
    // (the next sync is the backstop). No-op fast path when the base didn't advance (base_delta == 0).
    let base_delta = vector_core::db::community::load_community(&CommunityId(id_bytes)).ok().flatten()
        .map(|c| c.server_root_epoch.0).unwrap_or(pre_server_epoch).saturating_sub(pre_server_epoch);
    for attempt in 0..CHANNEL_FOLLOW_MAX_ATTEMPTS {
        if !session.is_valid() { return; }
        let Some(cur) = vector_core::db::community::load_community(&CommunityId(id_bytes)).ok().flatten() else { break; };
        for ch in &cur.channels {
            let _ = vector_core::community::service::catch_up_channel_rekeys(&bt, &cur, &ch.id).await;
        }
        let caught = base_delta == 0 || vector_core::db::community::load_community(&CommunityId(id_bytes)).ok().flatten()
            .map(|c| c.channels.iter().all(|ch| {
                let pre = pre_channel_epochs.iter().find(|(id, _)| id == &ch.id.to_hex()).map(|(_, e)| *e).unwrap_or(ch.epoch.0);
                ch.epoch.0 >= pre.saturating_add(base_delta)
            }))
            .unwrap_or(true);
        if caught { break; }
        if attempt + 1 < CHANNEL_FOLLOW_MAX_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(CHANNEL_FOLLOW_BACKOFF_MS)).await;
        }
    }
    if !session.is_valid() { return; }
    // Resume any outstanding read-cut re-seal (a private ban whose rotation failed transiently). No-op if none.
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes)).ok().flatten().unwrap_or(community);
    let _ = vector_core::community::service::retry_pending_read_cut(&bt, &community).await;
    if !session.is_valid() { return; }
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes)).ok().flatten().unwrap_or(community);
    // Push the freshened display metadata (name/desc/icon/owner) into STATE + the chat rows, then tell
    // the frontend to re-read so the change shows live (member list, roster crown, name, mode).
    sync_community_chats(&community).await;
    // If the follow advanced the base or any channel epoch, rebuild the FULL subscription so realtime
    // delivery resumes at the new pseudonyms (safe here — we're a spawned task, not in the notification
    // loop). Otherwise just refresh the route maps so the inbound drop-filter sees the new banlist (a
    // received ban starts dropping that author live, a received unban stops dropping them).
    let advanced = community.server_root_epoch.0 != pre_server_epoch
        || community.channels.iter().any(|c| {
            pre_channel_epochs.iter().find(|(id, _)| id == &c.id.to_hex()).map(|(_, e)| *e != c.epoch.0).unwrap_or(true)
        });
    if advanced && session.is_valid() {
        crate::services::subscription_handler::refresh_community_subscription().await;
    } else {
        crate::services::subscription_handler::rebuild_community_routes().await;
    }
    // Re-check after the subscription/route await before writing the cross-device list mirror — a
    // mid-flight account swap must not persist this account's snapshot into the new account's settings.
    if !session.is_valid() { return; }
    // a base advance is a re-founding we followed in REALTIME — point the cross-device list's
    // current snapshot (root + channel keys + name) so another device jumps straight to the latest epoch
    // (debounced; no-op if unchanged). Mirrors `sync_community_channel_inner` (the explicit-sync follow path).
    vector_core::community::list::refresh_membership_current(&community);
    vector_core::emit_event("community_refreshed", &serde_json::json!({ "community_id": community_id }));
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

    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    let community =
        service::create_community(&transport, &name, &channel_name, relays).await?;

    let channel_id = community.channels[0].id.to_hex();
    // Persist the channel chat(s) with display metadata so they load like any DM.
    sync_community_chats(&community).await;
    // record the join in the cross-device list so our other devices auto-join silently.
    vector_core::community::list::add_membership(&community);
    // Start receiving on the new channel.
    crate::services::subscription_handler::refresh_community_subscription().await;

    // Proven owner npub (verified attestation) so the frontend can stamp the crown
    // immediately, rather than waiting for the next reload to re-derive it.
    let community_id = community.id.to_hex();
    let owner_npub = community
        .owner_attestation
        .as_ref()
        .and_then(|att| vector_core::community::owner::verify_owner_attestation(att, &community_id))
        .and_then(|pk| pk.to_bech32().ok());

    Ok(CreatedCommunity { community_id, channel_id, owner_npub })
}

#[derive(serde::Serialize)]
pub struct CreatedCommunity {
    pub community_id: String,
    pub channel_id: String,
    pub owner_npub: Option<String>,
}

/// Publish an ephemeral typing indicator into a Community channel. Best-effort fire-and-forget — a
/// dropped keystroke ping is harmless. `channel_id` is the channel hex id (the open-chat id the
/// frontend already hands `start_typing`). Returns false if it isn't a known Community channel.
pub(crate) async fn send_community_typing(channel_id: &str) -> bool {
    let session = vector_core::state::SessionGuard::capture();
    let Ok(Some(community_id)) = vector_core::db::community::community_id_for_channel(channel_id) else {
        return false;
    };
    let Ok(id_bytes) = hex_to_id32(&community_id) else { return false; };
    let Ok(Some(community)) = vector_core::db::community::load_community(&CommunityId(id_bytes)) else {
        return false;
    };
    let Some(channel) = community.channels.iter().find(|c| c.id.to_hex() == channel_id).cloned() else {
        return false;
    };
    if !session.is_valid() { return false; }
    let transport = LiveTransport::with_timeout(Duration::from_secs(8));
    service::publish_typing_signal(&transport, &community, &channel).await.is_ok()
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
) -> Result<(), String> {
    use vector_core::sending::SendCallback;
    use vector_core::Message;
    let reply = replied_to.filter(|r| !r.is_empty());

    let session = vector_core::state::SessionGuard::capture();
    let author_pk = vector_core::my_public_key().ok_or("Public key not set")?;
    let my_npub = author_pk.to_bech32().ok();

    // Resolve channel → owning Community.
    let community_id = vector_core::db::community::community_id_for_channel(&channel_id)?
        .ok_or("Unknown Community channel")?;
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    let channel = community
        .channels
        .iter()
        .find(|c| c.id.to_hex() == channel_id)
        .ok_or("Channel not found in Community")?
        .clone();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let ms = now.as_millis() as u64;
    let callback = crate::message::sending::TauriSendCallback;

    // Build the inner event up front so its id (the message_id) is known BEFORE the
    // optimistic insert. Using the final id from the start — not a swapped "pending-"
    // id — means the message id never changes: the inbound STATE dedup recognizes the
    // relay echo (same inner id) and drops it, so the sender can't see a duplicate even
    // if the echo races the post-publish finalize. The id is derivable from the unsigned
    // event (it doesn't depend on the signature).
    // NIP-30: resolve `:shortcode:` in the content against subscribed packs so the inner
    // event carries `["emoji", ...]` tags (custom emoji render for everyone + our echo).
    let emoji_tags = vector_core::emoji_packs::resolve_outbound_emoji_tags(&content);
    let unsigned = vector_core::community::envelope::build_inner_typed(
        author_pk,
        &channel.id,
        channel.epoch,
        vector_core::stored_event::event_kind::COMMUNITY_MESSAGE,
        &content,
        ms,
        reply.as_deref(),
        &emoji_tags,
    );
    let message_id = unsigned.id.ok_or("inner event has no id")?.to_hex();

    // 1. Optimistic message — renders instantly (parity with DMs), keyed by its real id.
    let pending_msg = Message {
        id: message_id.clone(),
        content: content.clone(),
        at: ms,
        pending: true,
        mine: true,
        npub: my_npub.clone(),
        replied_to: reply.clone().unwrap_or_default(),
        emoji_tags: emoji_tags.clone(),
        ..Default::default()
    };
    {
        let mut state = vector_core::state::STATE.lock().await;
        state.add_message_to_chat(&channel_id, pending_msg.clone());
    }
    callback.on_pending(&channel_id, &pending_msg);

    // 2. Sign the inner event (local or bunker — may round-trip), then publish.
    let signed = async {
        let client = vector_core::state::nostr_client().ok_or("Not logged in")?;
        let signer = client.signer().await.map_err(|e| format!("Signer unavailable: {e}"))?;
        unsigned.sign(&signer).await.map_err(|e| format!("Failed to sign message: {e}"))
    }
    .await;

    let publish_result = match signed {
        Ok(inner) if session.is_valid() => {
            let transport = LiveTransport::with_timeout(Duration::from_secs(12));
            service::send_signed_message(&transport, &community, &channel, &inner).await
        }
        Ok(_) => Err("account changed during send".to_string()),
        Err(e) => Err(e),
    };

    match publish_result {
        Ok(_outer) => {
            // 3a. Sent — clear the pending flag (id is unchanged) + persist.
            let sent = {
                let mut state = vector_core::state::STATE.lock().await;
                state.update_message(&message_id, |m| m.set_pending(false))
            };
            if let Some((_cid, ref msg)) = sent {
                callback.on_sent(&channel_id, &message_id, msg);
                callback.on_persist(&channel_id, msg);
            } else {
                // The optimistic message is gone (e.g. account swap cleared STATE). It
                // did publish; just don't strand a phantom pending bubble.
                vector_core::log_warn!("[community] sent message {} not in STATE to finalize", message_id);
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
}

/// A Community attachment that's been encrypted and previewed locally but NOT yet uploaded.
/// The upload is deferred into [`dispatch_community_attachment_message`] so the optimistic
/// bubble (with the progress ring + cancel button) shows BEFORE the bytes hit the network —
/// parity with DM file sends.
struct PreparedCommunityAttachment {
    /// Optimistic attachment — `url` is empty until the upload completes; the plaintext is
    /// already on disk so the sender previews it instantly.
    attachment: vector_core::types::Attachment,
    /// Ciphertext to upload to Blossom.
    encrypted: Vec<u8>,
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
) -> Result<PreparedCommunityAttachment, String> {
    let bytes = std::fs::read(file_path).map_err(|e| format!("read attachment: {e}"))?;
    let name = if !name_override.is_empty() {
        name_override.to_string()
    } else {
        std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string()
    };
    process_outbound_community_attachment_bytes(bytes, &name, use_compression).await
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
) -> Result<PreparedCommunityAttachment, String> {
    use vector_core::types::{Attachment, ImageMetadata};

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

    // Compress non-GIF images before upload — parity with DM sends (same JPEG quality).
    // GIFs are never compressed (animation). Falls back to the original bytes/extension on
    // any decode/encode failure. Done BEFORE hashing/preview so they describe what's uploaded.
    let mut bytes = bytes;
    if use_compression && is_image && extension != "gif" {
        if let Ok(img) = vector_core::crypto::decode_image_bounded(&bytes) {
            let rgba = img.to_rgba8();
            if let Ok(enc) = crate::shared::image::encode_rgba_auto(
                rgba.as_raw(), img.width(), img.height(), crate::shared::image::JPEG_QUALITY_STANDARD,
            ) {
                bytes = enc.bytes;
                extension = enc.extension.to_string();
            }
        }
    }

    let plaintext_hash = vector_core::crypto::sha256_hex(&bytes);

    // Image metadata (thumbhash + dimensions) for inline rendering, when decodable.
    let img_meta = if is_image {
        vector_core::crypto::decode_image_bounded(&bytes).ok().and_then(|img| {
            crate::util::generate_thumbhash_from_image(&img).map(|thumbhash| ImageMetadata {
                thumbhash,
                width: img.width(),
                height: img.height(),
            })
        })
    } else {
        None
    };

    // Save the plaintext locally (keyed by hash, matching the inbound path convention) so
    // the sender's optimistic bubble renders immediately as a downloaded file.
    let dir = vector_core::db::get_download_dir();
    let _ = std::fs::create_dir_all(&dir);
    let local_path = dir.join(format!("{}.{}", plaintext_hash, extension));
    if !local_path.exists() {
        let _ = std::fs::write(&local_path, &bytes);
    }

    // Encrypt with a fresh key+nonce; the ciphertext is uploaded later (after the optimistic
    // bubble is shown) so progress + cancel drive the sender's UI.
    let params = vector_core::crypto::generate_encryption_params();
    let encrypted = vector_core::crypto::encrypt_data(&bytes, &params)?;
    let encrypted_size = encrypted.len() as u64;
    let mime = vector_core::crypto::mime_from_extension(&extension).to_string();

    // Mini Apps: mint the realtime topic at send time so every member joins the
    // same gossip topic — it rides the imeta (see vector_core::webxdc).
    let webxdc_topic = extension.eq_ignore_ascii_case("xdc").then(|| {
        let sender = vector_core::state::my_public_key()
            .map(|pk| pk.to_hex())
            .unwrap_or_default();
        vector_core::webxdc::mint_topic_id(&plaintext_hash, &sender)
    });

    let attachment = Attachment {
        id: plaintext_hash.clone(),
        key: params.key,
        nonce: params.nonce,
        extension,
        name,
        url: String::new(), // filled in once the upload completes
        path: local_path.to_string_lossy().to_string(),
        size: encrypted_size,
        img_meta,
        downloading: false,
        downloaded: true, // plaintext already on disk for the sender
        webxdc_topic,
        group_id: None,
        original_hash: Some(plaintext_hash),
        scheme_version: None,
        mls_filename: None,
    };
    Ok(PreparedCommunityAttachment { attachment, encrypted, mime })
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
    replied_to: Option<String>,
) -> Result<CommunityAttachmentSendResult, String> {
    if file_paths.is_empty() {
        return Err("No files to send".to_string());
    }
    // Capture session BEFORE the uploads so a mid-upload account swap is caught.
    let session = vector_core::state::SessionGuard::capture();
    let mut prepared = Vec::with_capacity(file_paths.len());
    for (i, fp) in file_paths.iter().enumerate() {
        let name_override = name_overrides.get(i).map(String::as_str).unwrap_or("");
        prepared.push(process_outbound_community_attachment(fp, name_override, use_compression).await?);
    }
    dispatch_community_attachment_message(channel_id, content, replied_to, session, prepared).await
}

/// Like [`send_community_files`] but for a single file delivered as raw bytes + filename
/// (clipboard paste / Android File object — no on-disk source). Same multi-attachment
/// envelope + optimistic lifecycle; one attachment per call.
#[tauri::command]
pub async fn send_community_file_bytes(
    channel_id: String,
    content: String,
    file_bytes: Vec<u8>,
    file_name: String,
    use_compression: bool,
    replied_to: Option<String>,
) -> Result<CommunityAttachmentSendResult, String> {
    let session = vector_core::state::SessionGuard::capture();
    let prepared = vec![process_outbound_community_attachment_bytes(file_bytes, &file_name, use_compression).await?];
    dispatch_community_attachment_message(channel_id, content, replied_to, session, prepared).await
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
    let session = vector_core::state::SessionGuard::capture();
    let mut prepared = process_outbound_community_attachment_bytes(bytes, "voice-message.wav", false).await?;
    prepared.attachment.name = String::new();
    dispatch_community_attachment_message(channel_id, String::new(), replied_to, session, vec![prepared]).await.map(|_| ())
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
    replied_to: Option<String>,
) -> Result<(), String> {
    let session = vector_core::state::SessionGuard::capture();
    // Take ownership of the cached bytes + name, clearing the cache in one lock.
    let (bytes, cache_name) = {
        let mut cache = crate::message::files::JS_FILE_CACHE.lock().unwrap();
        match cache.take() {
            Some((b, n, _ext)) => ((*b).clone(), n),
            None => return Err("No cached file to send".to_string()),
        }
    };
    // A non-empty override (spoiler / rename) wins over the cached source name.
    let name = name_override.filter(|s| !s.is_empty()).unwrap_or(cache_name);
    let prepared = vec![process_outbound_community_attachment_bytes(bytes, &name, use_compression).await?];
    dispatch_community_attachment_message(channel_id, content, replied_to, session, prepared).await.map(|_| ())
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
    session: vector_core::state::SessionGuard,
    prepared: Vec<PreparedCommunityAttachment>,
) -> Result<CommunityAttachmentSendResult, String> {
    use vector_core::sending::SendCallback;
    use vector_core::Message;

    if !session.is_valid() {
        return Err("account changed during upload".to_string());
    }
    let reply = replied_to.filter(|r| !r.is_empty());
    let author_pk = vector_core::my_public_key().ok_or("Public key not set")?;
    let my_npub = author_pk.to_bech32().ok();

    // Resolve channel → owning Community (same as send_community_message).
    let community_id = vector_core::db::community::community_id_for_channel(&channel_id)?
        .ok_or("Unknown Community channel")?;
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    let channel = community
        .channels
        .iter()
        .find(|c| c.id.to_hex() == channel_id)
        .ok_or("Channel not found in Community")?
        .clone();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let ms = now.as_millis() as u64;
    // Temp id keyed during upload — the real inner id depends on the imeta (uploaded URLs),
    // so it isn't known until every attachment lands. Finalized → real id on publish ack.
    let pending_id = format!("pending-{}", now.as_nanos());
    let callback = crate::message::sending::TauriSendCallback;
    let emoji_tags = vector_core::emoji_packs::resolve_outbound_emoji_tags(&content);

    // Optimistic bubble — attachments carry empty URLs (plaintext is already on disk for the
    // sender's preview); the upload fills them in.
    let optimistic_attachments: Vec<_> = prepared.iter().map(|p| p.attachment.clone()).collect();
    let pending_msg = Message {
        id: pending_id.clone(),
        content: content.clone(),
        at: ms,
        pending: true,
        mine: true,
        npub: my_npub.clone(),
        replied_to: reply.clone().unwrap_or_default(),
        emoji_tags: emoji_tags.clone(),
        attachments: optimistic_attachments,
        ..Default::default()
    };
    {
        let mut state = vector_core::state::STATE.lock().await;
        state.add_message_to_chat(&channel_id, pending_msg.clone());
    }
    // Registers the cancel flag (keyed by pending_id) + emits the pending bubble.
    callback.on_pending(&channel_id, &pending_msg);

    // Cancel flag the cancel_upload command flips — bridge it into Blossom so a cancel
    // between progress ticks still aborts the transfer.
    let cancel_flag = crate::message::upload_cancel_flags().lock().unwrap().get(&pending_id).cloned();

    let client = vector_core::state::nostr_client().ok_or("Not logged in")?;
    let signer = client.signer().await.map_err(|e| format!("Signer unavailable: {e}"))?;
    let servers = vector_core::blossom_servers::compute_enabled_servers();
    if servers.is_empty() {
        let _ = mark_attachment_send_failed(&callback, &channel_id, &pending_id).await;
        return Err("No Blossom servers configured.".to_string());
    }

    // Upload each attachment, driving the progress ring (keyed by pending_id) + filling URLs.
    let mut uploaded: Vec<vector_core::types::Attachment> = Vec::with_capacity(prepared.len());
    for prep in prepared {
        let PreparedCommunityAttachment { mut attachment, encrypted, mime } = prep;
        let cb_for_progress = callback.clone();
        let pid_for_progress = pending_id.clone();
        let progress_cb: vector_core::blossom::ProgressCallback =
            std::sync::Arc::new(move |percentage, bytes| {
                cb_for_progress.on_upload_progress(
                    &pid_for_progress,
                    percentage.unwrap_or(0),
                    bytes.unwrap_or(0),
                )
            });

        let upload_url = match vector_core::blossom::upload_blob_with_progress_and_failover(
            signer.clone(),
            servers.clone(),
            std::sync::Arc::new(encrypted),
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
                let _ = mark_attachment_send_failed(&callback, &channel_id, &pending_id).await;
                return Err(format!("Upload failed: {e}"));
            }
        };

        attachment.url = upload_url.clone();
        // Reflect the uploaded URL on the optimistic bubble's attachment.
        {
            let mut state = vector_core::state::STATE.lock().await;
            state.update_attachment(&channel_id, &pending_id, &attachment.id, |a| {
                a.url = upload_url.clone().into_boxed_str();
            });
        }
        callback.on_upload_complete(&channel_id, &pending_id, &attachment.id, &upload_url);
        uploaded.push(attachment);
    }

    // Build the real inner now that every imeta carries its uploaded URL.
    let imeta_tags: Vec<_> = uploaded
        .iter()
        .map(vector_core::community::attachments::attachment_to_imeta)
        .collect();
    let unsigned = vector_core::community::envelope::build_inner_full(
        author_pk,
        &channel.id,
        channel.epoch,
        vector_core::stored_event::event_kind::COMMUNITY_MESSAGE,
        &content,
        ms,
        reply.as_deref(),
        &emoji_tags,
        &imeta_tags,
    );
    let real_id = match unsigned.id {
        Some(id) => id.to_hex(),
        None => {
            let _ = mark_attachment_send_failed(&callback, &channel_id, &pending_id).await;
            return Err("inner event has no id".to_string());
        }
    };

    let signed = unsigned
        .sign(&signer)
        .await
        .map_err(|e| format!("Failed to sign message: {e}"));

    let publish_result = match signed {
        Ok(inner) if session.is_valid() => {
            let transport = LiveTransport::with_timeout(Duration::from_secs(12));
            service::send_signed_message(&transport, &community, &channel, &inner).await
        }
        Ok(_) => Err("account changed during send".to_string()),
        Err(e) => Err(e),
    };

    match publish_result {
        Ok(_outer) => {
            // Swap temp id → real id and clear pending.
            let finalized = {
                let mut state = vector_core::state::STATE.lock().await;
                state.finalize_pending_message(&channel_id, &pending_id, &real_id)
            };
            if let Some((_old, ref msg)) = finalized {
                callback.on_sent(&channel_id, &pending_id, msg);
                callback.on_persist(&channel_id, msg);
            }
            let webxdc_topic = uploaded.iter().find_map(|a| a.webxdc_topic.clone());
            Ok(CommunityAttachmentSendResult { message_id: real_id, webxdc_topic })
        }
        Err(e) => {
            let _ = mark_attachment_send_failed(&callback, &channel_id, &pending_id).await;
            Err(e)
        }
    }
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

/// How many append-plane events a single network page fetches (newest-first). Larger than
/// the frontend's display batch so the first few scroll-ups are served entirely from the DB
/// before another network page is needed.
const COMMUNITY_PAGE_FETCH_LIMIT: usize = 50;

// Community sync RAM state (page cursors, history-start floors, in-flight de-dup) lives in
// `vector_core::community::cache`, consolidated under one session-generation invalidation key.

/// Result of a Community page sync. `reached_start` is the AUTHORITATIVE "no more older
/// history" signal (the relay returned nothing strictly older than the cursor) — the frontend
/// keys its scroll-up termination off this, never off a DB row-count delta (a page can return
/// only already-known events while older history still exists).
#[derive(serde::Serialize, Default)]
pub struct CommunitySyncResult {
    pub new_messages: u32,
    pub reached_start: bool,
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
/// Anti-stampede (one in-flight fetch per channel+page) + history-start short-circuit keep it
/// orderly and waste-free. Ingest dedups on inner id; re-persist is idempotent (INSERT OR REPLACE).
#[tauri::command]
pub async fn sync_community_channel(channel_id: String, before_ms: Option<u64>) -> Result<CommunitySyncResult, String> {
    let is_older = before_ms.is_some();

    // History-start: stop paging into the void once we've found a channel's beginning.
    if is_older && vector_core::community::cache::is_at_history_start(&channel_id) {
        return Ok(CommunitySyncResult { new_messages: 0, reached_start: true });
    }

    // Anti-stampede: one in-flight fetch per channel per direction. The older cursor is
    // backend-tracked (not the varying `before_ms`), so all concurrent older requests target
    // the same page → one key dedups them; latest is its own key.
    let key = format!("{channel_id}:{}", if is_older { "older" } else { "latest" });
    if !vector_core::community::cache::try_begin_page_fetch(&key) {
        return Ok(CommunitySyncResult::default());
    }
    // RAII release: the claim is dropped on return OR panic, so a panic deep in the fetch can never
    // leave a permanent in-flight claim that wedges all future syncs of this channel.
    struct PageClaim(String);
    impl Drop for PageClaim {
        fn drop(&mut self) {
            vector_core::community::cache::end_page_fetch(&self.0);
        }
    }
    let _claim = PageClaim(key);
    sync_community_channel_inner(&channel_id, before_ms, is_older).await
}

async fn sync_community_channel_inner(
    channel_id: &str,
    before_ms: Option<u64>,
    is_older: bool,
) -> Result<CommunitySyncResult, String> {
    use vector_core::community::inbound::IncomingEvent;

    let session = vector_core::state::SessionGuard::capture();
    let my_pk = vector_core::my_public_key().ok_or("Public key not set")?;

    let community_id = vector_core::db::community::community_id_for_channel(channel_id)?
        .ok_or("Unknown Community channel")?;
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    // Epochs the realtime subscription is currently pinned to (it was built from the DB's last-synced
    // epochs). If the follow below advances either, the sub must be rebuilt or live delivery stays dead at
    // the OLD epoch's pseudonyms until the next sync (the rekey realtime gap).
    let pre_server_epoch = community.server_root_epoch.0;
    let pre_channel_epoch = community.channels.iter().find(|c| c.id.to_hex() == channel_id).map(|c| c.epoch.0);
    // On the latest-page sync, run the control catch-up UNCONDITIONALLY — no liveness/freshness gate
    // ever decides whether to detect authority changes. `catch_up_server_root` /
    // `catch_up_channel_rekeys` are themselves cheap probes (one racing-fast fetch that breaks when
    // nothing rotated), and the channel walk MUST run every sync to converge: a re-founding's channel
    // rekey can lag the base rekey's propagation and is addressed under the PRIOR root, so a one-shot
    // "did it rotate?" probe would false-negative and strand the new channel key. Skipped only on
    // older-page scroll-back (the plane is already current).
    let community = if !is_older {
        // Longer than the 12s op-norm: catch-up + the whole-control-plane fold ride this one transport, and
        // boot is REQ-heavy (relays contended + ratelimited), so a short cap returns the plane partial →
        // convergence/authority silently fail closed. (See refresh_community_control for the rationale.)
        let bt = LiveTransport::with_timeout(Duration::from_secs(20));
        // Follow a base (server-root) re-founding BEFORE reading control/messages (else we'd read
        // stale-epoch pseudonyms and fall off). An AUTHORIZED base rotation that excluded us (private
        // ban / read-cut) is a removal — tear down locally, the catch-all for a cut member who can no
        // longer decrypt the new control plane to read the banlist the normal way.
        if let Ok(c) = vector_core::community::service::catch_up_server_root(&bt, &community).await {
            if c.removed {
                // Destructive teardown after a multi-second fetch: re-validate the
                // session or a mid-sync account swap tears down account B's membership.
                if !session.is_valid() {
                    return Err("account changed during sync".to_string());
                }
                self_remove_from_community(&community_id, false).await;
                return Ok(CommunitySyncResult { new_messages: 0, reached_start: true });
            }
        }
        if !session.is_valid() {
            return Err("account changed during sync".to_string());
        }
        let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?.unwrap_or(community);
        // ONE REQ for the entire control plane, applied banlist-first: roles (authority graph), invite links
        // (Public/Private mode), and metadata (name/description/icon/channel) all fold from a single fetch.
        let _ = vector_core::community::service::fetch_and_apply_control(&bt, &community).await;
        // Did a ban land on us? A banned member self-removes (drop keys + wipe data, like a kick but no
        // rejoin) — caught here on sync/boot, and in realtime via the control-plane subscription.
        // Guarded: check_self_banned tears down on a hit, and the control fold above awaited.
        if !session.is_valid() {
            return Err("account changed during sync".to_string());
        }
        if check_self_banned(&community_id).await {
            return Ok(CommunitySyncResult { new_messages: 0, reached_start: true });
        }
        // Walk THIS channel's rekey chain (all held roots + gap-fill) so we hold the current channel
        // key before paging — unconditional, the self-healing convergence for a lagging/prior-root
        // channel rekey (mirrors the realtime path).
        if let Some(ch) = community.channels.iter().find(|c| c.id.to_hex() == channel_id) {
            let _ = vector_core::community::service::catch_up_channel_rekeys(&bt, &community, &ch.id).await;
        }
        // Retry an outstanding read-cut re-seal (a private ban whose rotation failed transiently) so it
        // auto-recovers on sync instead of leaving a banned member with read access. No-op if none.
        let _ = vector_core::community::service::retry_pending_read_cut(&bt, &community).await;
        vector_core::db::community::load_community(&CommunityId(id_bytes))?.unwrap_or(community)
    } else {
        community
    };
    // Re-persist the chat's display metadata (incl. the `dissolved` seal) after the control fold, so a
    // community that just sealed via this sync reflects it in the persisted chat row — otherwise the stale
    // pre-seal row loads at boot and the UI looks alive. Mirrors refresh_community_control's re-persist.
    if session.is_valid() {
        sync_community_chats(&community).await;
        // Tell the live UI to re-read this community's metadata (name / description / members / mode /
        // dissolved) after the control fold, so a change folded during a SYNC — e.g. a rename picked up at
        // boot, or an already-sealed community — shows without a reload. The realtime control path emits the
        // same event; this covers the sync/boot path. Only on a fresh (latest-page) sync; an older-page
        // scroll-back folds nothing. The listener is a cheap read-only re-render (no publish → no loop).
        if !is_older {
            vector_core::emit_event("community_refreshed", &serde_json::json!({ "community_id": community.id.to_hex() }));
        }
    }
    // Close the rekey realtime gap: if the follow advanced the server-root OR this channel's epoch, the
    // live subscription is still pinned to the OLD pseudonyms — rebuild it so realtime delivery resumes at
    // the NEW epoch immediately, instead of only on the next sync (the documented post-MVP closure).
    if !is_older && session.is_valid() {
        let post_channel_epoch = community.channels.iter().find(|c| c.id.to_hex() == channel_id).map(|c| c.epoch.0);
        if community.server_root_epoch.0 != pre_server_epoch || post_channel_epoch != pre_channel_epoch {
            crate::services::subscription_handler::refresh_community_subscription().await;
        }
        // refresh the cross-device list's `current` snapshot (root + channel keys + name) so another
        // device jumps straight to the latest epoch (debounced; no-op if unchanged — covers rekey + rename).
        vector_core::community::list::refresh_membership_current(&community);
    }
    let channel = community
        .channels
        .iter()
        .find(|c| c.id.to_hex() == channel_id)
        .ok_or("Channel not found in Community")?
        .clone();

    // Older-page cursor on the OUTER (send-time) clock — the clock the relay actually filters.
    // Prefer the real oldest wire-time we've fetched for this channel (immune to inner-ms
    // manipulation); fall back to the frontend's inner-`at` hint only before we've fetched any
    // page this session. `until` is inclusive (re-admits the boundary; dedup drops it). Latest
    // page (None) fetches the newest events.
    let until_secs = if is_older {
        let tracked = vector_core::community::cache::oldest_cursor(channel_id);
        tracked.or_else(|| before_ms.map(|m| m / 1000))
    } else {
        None
    };

    // Latest-page `since`: skip re-pulling events we already hold by floor-ing the fetch at the
    // newest wire time seen this session. ONLY the latest page (an older page must page strictly
    // back with no lower bound). `None` before the first latest fetch this session → full newest
    // page. Epoch spanning is untouched (it's in the pseudonym OR-set, not the cursor).
    //
    // The floor is pulled back by SINCE_LOOKBACK_SECS so an event whose OUTER time lands slightly
    // BELOW the cursor — author clock-skew or late relay propagation — is still swept in (dedup
    // drops the overlap). Reconnect-gap events aren't at risk: they're NEWER than the cursor, so
    // they're above the floor regardless.
    const SINCE_LOOKBACK_SECS: u64 = 120;
    let since_secs = if is_older {
        None
    } else {
        vector_core::community::cache::newest_cursor(channel_id).map(|s| s.saturating_sub(SINCE_LOOKBACK_SECS))
    };

    // Adopt an in-flight invite preload for the PRIMARY channel's latest page: rather than fire a
    // second fetch, wait on the warm-up fetch already running (it IS the page) — so the join speedup
    // holds even if the user tapped Join before the warm-up landed. Falls through to a normal fetch
    // on miss / timeout / non-primary channel / older page (only the primary channel was warmed, and
    // only the latest page is preload-shaped: newest, no `until`).
    let is_primary = community.channels.first().map(|c| c.id) == Some(channel.id);
    let adopted = if !is_older && is_primary {
        vector_core::community::cache::take_or_await_preload(&community_id).await
    } else {
        None
    };
    // Fetch one page over the network — NO STATE lock held across the await.
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    let events = match adopted {
        Some(page) => page,
        None => vector_core::community::send::fetch_channel_page(
            &transport, &community, &channel, until_secs, since_secs, COMMUNITY_PAGE_FETCH_LIMIT,
        )
        .await?,
    };
    if !session.is_valid() {
        return Err("account changed during sync".to_string());
    }

    // Process the batch into STATE (sync), collecting outcomes to persist (+ emit).
    let outcomes = {
        let mut state = vector_core::state::STATE.lock().await;
        vector_core::community::inbound::process_channel_batch(&mut state, &events, &channel, &my_pk)
    };

    let mut new_messages = 0u32;
    for outcome in &outcomes {
        if !session.is_valid() {
            break;
        }
        match outcome {
            IncomingEvent::NewMessage(msg) => {
                let _ = crate::db::save_message(channel_id, msg).await;
                // Latest page → emit (append + preview). Older page → silent: the frontend
                // prepends these from its DB re-query (emitting message_new would append them
                // at the BOTTOM, which is wrong for back-paged history).
                if !is_older {
                    vector_core::emit_event(
                        "message_new",
                        &serde_json::json!({ "message": msg, "chat_id": channel_id }),
                    );
                }
                new_messages += 1;
            }
            IncomingEvent::Updated { target_id, message, edit_event } => {
                persist_community_update(channel_id, message, edit_event.as_deref()).await;
                // Reactions/edits apply surgically by target id (position-independent), so
                // emit on BOTH latest and older pages — an older page can carry a reaction to
                // a still-visible message, which must update live.
                vector_core::emit_event(
                    "message_update",
                    &serde_json::json!({ "old_id": target_id, "message": message, "chat_id": channel_id }),
                );
            }
            IncomingEvent::Removed { target_id } => {
                // Cooperative tombstone applies surgically by target id (position-independent),
                // so honor it on both latest and older pages — drop locally + fade the row.
                let _ = crate::db::delete_event(target_id).await;
                vector_core::emit_event(
                    "message_removed",
                    &serde_json::json!({ "id": target_id, "chat_id": channel_id, "reason": "deleted" }),
                );
            }
            IncomingEvent::Presence { npub, joined, event_id, created_at, invited_by, invited_label } => {
                apply_community_presence(channel_id, npub, *joined, event_id, *created_at, invited_by.as_deref(), invited_label.as_deref()).await;
            }
            IncomingEvent::WebxdcPeer { npub, topic_id, node_addr, event_id, created_at } => {
                // Full DM-parity handling: persist (rejoin discovery), feed the live gossip
                // channel if this Mini App is open, else cache + surface the lobby status.
                match node_addr {
                    Some(addr) => {
                        crate::services::event_handler::handle_webxdc_peer_advertisement(
                            event_id, topic_id, addr, npub, *created_at, channel_id,
                        ).await;
                    }
                    None => {
                        crate::services::event_handler::handle_webxdc_peer_left(
                            event_id, topic_id, npub, *created_at, channel_id,
                        ).await;
                    }
                }
            }
            IncomingEvent::Kicked { community_id } | IncomingEvent::SelfLeft { community_id } => {
                // self-removal (kick of us, or a leave another device authored) — received, not
                // locally originated, so tombstone local-only (boot's explicit publish propagates). Stop the
                // batch — the community is being torn down, so later same-batch writes (message saves,
                // presence) would orphan rows under a now-deleted chat. Teardown retains the held epoch keys.
                self_remove_from_community(community_id, false).await;
                return Ok(CommunitySyncResult { new_messages, reached_start: false });
            }
            IncomingEvent::Typing { .. } => {
                // Realtime-only ephemeral signal; never fetched in a sync/straggler batch. No-op.
            }
        }
    }

    // Cursors and the history-start verdict consider ONLY events that authenticate against the
    // channel's keys: the outer created_at is unauthenticated, and the cleartext pseudonym is in
    // our own REQ — a relay (or any member) could otherwise stamp one junk event far-future/past
    // and silently wedge this channel's fetch floor/ceiling for the whole session. Computed AFTER
    // ingest so just-processed events are dedup-ledger hits (no second decryption).
    if !session.is_valid() {
        return Err("account changed during sync".to_string());
    }
    let verified_times: Vec<u64> = events
        .iter()
        .filter(|e| vector_core::community::inbound::event_authenticates(e, &channel))
        .map(|e| e.created_at.as_secs())
        .collect();

    // Advance the outer-time cursor to the oldest wire created_at this page returned (so the
    // NEXT older page steps strictly further back, on the relay's own clock).
    if let Some(oldest) = verified_times.iter().copied().min() {
        vector_core::community::cache::advance_oldest_cursor(channel_id, oldest);
    }
    // Advance the latest-page `since` floor to the newest wire time this page returned, so the next
    // latest sync only pulls what's genuinely new. Latest page only — an older page must not raise
    // the floor (it returns OLD events, which would wrongly cap future top-fetches).
    if !is_older {
        if let Some(newest) = verified_times.iter().copied().max() {
            vector_core::community::cache::advance_newest_cursor(channel_id, newest);
        }
    }

    // History-start (older pages): the page came back NON-EMPTY but with nothing strictly
    // older than the cursor → we've hit the channel's beginning; mark it so future older pages
    // stay DB-only, and report it (the frontend trusts THIS, not a row-count delta). An EMPTY
    // page is treated as a transient relay miss (rate-limit / unreachable), NOT history-start —
    // so a flaky relay can't permanently wedge scroll-back. A FULL page is never history-start
    // either: ≥limit events sharing the boundary second (a burst "wall") just means the next
    // page must step past them, not that history ended.
    let reached_start = if is_older && !verified_times.is_empty() && events.len() < COMMUNITY_PAGE_FETCH_LIMIT {
        let older_than_cursor = until_secs.map_or(0, |u| {
            verified_times.iter().filter(|t| **t < u).count()
        });
        if older_than_cursor == 0 {
            vector_core::community::cache::mark_history_start(channel_id);
            true
        } else {
            false
        }
    } else {
        false
    };

    Ok(CommunitySyncResult { new_messages, reached_start })
}

/// Coalesce a burst of relay reconnections into a single Community re-sync. `sync_communities_boot`
/// already fans every fetch to each Community's full relay set, so one sweep re-syncs everything a
/// just-reconnected relay might hold — N concurrent reconnects need exactly ONE sweep, not N. An
/// in-flight guard drops overlapping triggers; a short coalescing delay lets a reconnect burst settle
/// before the sweep fires. Mirrors the DM reconnect re-sync, debounced for the all-relays fan-out.
pub fn trigger_community_reconnect_resync() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
    if IN_FLIGHT.swap(true, Ordering::AcqRel) {
        return; // a sweep is already pending/running — it covers this reconnect too
    }
    // Capture the session BEFORE the spawn (SessionGuard convention): if the account swaps during the
    // debounce, bail — the swapped-in account already gets a full boot sweep at selection, so a stale
    // reconnect trigger must not drive its sync. `sync_communities_boot` also re-guards each per-account
    // write, but capturing here keeps the whole detached task scoped to the account that reconnected.
    let session = vector_core::state::SessionGuard::capture();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        if session.is_valid() {
            let _ = sync_communities_boot().await;
        }
        IN_FLIGHT.store(false, Ordering::Release);
    });
}

/// boot reconcile: fetch the cross-device Community List, fold the relay copy into ours and
/// republish the union (so this device's local-only memberships propagate AND we learn the others'), then
/// rehydrate + surface any listed community we don't hold yet (auto-join silently). A listed community
/// we've since been banned from self-heals: rehydrate tears it down + we tombstone it. Rehydrated
/// communities page their latest in `rehydrate_listed_communities`; the boot sweep that runs after re-pages
/// them too, but per-channel anti-stampede coalesces the overlap.
pub(crate) async fn reconcile_community_list_boot() {
    let session = vector_core::state::SessionGuard::capture();
    let client = match vector_core::state::nostr_client() {
        Some(c) => c,
        None => return,
    };

    // Boot is a READ/SYNC, not a write: fetch the relay copy and fold it into the local mirror so we learn
    // other devices' joins. Publishing here every boot would be backwards — a fetch-then-publish race could
    // clobber another device's just-published join by timestamp.
    let my_pk = match vector_core::my_public_key() {
        Some(pk) => pk,
        None => return,
    };
    let relay = vector_core::community::list::fetch_community_list(&client, my_pk, session.clone())
        .await
        .unwrap_or_default();
    if !session.is_valid() {
        return;
    }
    let merged = vector_core::community::list::load_local_list().merge(&relay);
    let _ = vector_core::community::list::save_local_list(&merged);
    // A tombstone we folded from another device (a leave/kick/ban there) may name a community whose DB row
    // still lingers here — tear it down so the leave converges AND `backfill_from_db` below doesn't re-add it
    // as a live row. Received removal → local-only tombstone (no republish; boot's own publish carries it).
    for t in &merged.tombstones {
        if !session.is_valid() {
            return;
        }
        if let Ok(b) = hex_to_id32(&t.community_id) {
            if let Ok(Some(_)) = vector_core::db::community::load_community(&CommunityId(b)) {
                self_remove_from_community(&t.community_id, false).await;
            }
        }
    }
    // Seed any community we hold in the DB but that predates the list feature (or was joined on a device that
    // did). Combined with the merge above, the local mirror now reflects everything we actually belong to.
    vector_core::community::list::backfill_from_db();
    let list = vector_core::community::list::load_local_list();
    // Write ONLY if we're genuinely ahead of the relay (backfilled memberships, or an edit that never
    // propagated) — otherwise boot stays read-only. The debounced republish is the single write path; the
    // ADD/REMOVE/refresh hooks drive it on real edits.
    if list.is_ahead_of(&relay) {
        vector_core::community::list::republish_community_list_debounced();
    }
    // page_messages = false: the boot channel sweep (sync_communities_boot) pages every community right after
    // this, so paging here too would double-fetch the latest page (anti-stampede only dedups CONCURRENT syncs).
    rehydrate_listed_communities(&list, &session, false).await;
    if session.is_valid() {
        purge_stale_pending_invites(&list.tombstones);
    }
}

/// Drop parked invites the synced membership list proves STALE: any invite whose community we
/// HOLD (joined on some device) or that is TOMBSTONED (left / kicked / banned / dissolved on
/// some device). Cross-device truth: an accepted-or-departed community's invite must never
/// resurface anywhere. Re-downloaded gift wraps are stamped received_at = now, so membership —
/// not time — is the only reliable signal. Emits `community_invites_purged` when anything
/// dropped so the open UI re-pulls its invite rows live.
fn purge_stale_pending_invites(tombstones: &[vector_core::community::list::CommunityRemoval]) {
    let mut purged_any = false;
    for t in tombstones {
        if vector_core::db::community::pending_invite_exists(&t.community_id).unwrap_or(false) {
            let _ = vector_core::db::community::delete_pending_invite(&t.community_id);
            purged_any = true;
        }
    }
    if let Ok(n) = vector_core::db::community::purge_pending_invites_for_held_communities() {
        if n > 0 {
            purged_any = true;
        }
    }
    if purged_any {
        vector_core::emit_event("community_invites_purged", &serde_json::json!({}));
    }
}

/// A remote device edited our Community List (the live self-sync path): fold the received event into the
/// local mirror (NO republish — avoids an echo loop) and rehydrate any newly-present community, so a join
/// on another device appears here WITHOUT a reboot. A removal arriving this way tears the community down.
pub(crate) async fn ingest_community_list_update(event: nostr_sdk::Event) {
    let session = vector_core::state::SessionGuard::capture();
    let client = match vector_core::state::nostr_client() {
        Some(c) => c,
        None => return,
    };
    let Some(my_pk) = vector_core::my_public_key() else { return };
    let merged = match vector_core::community::list::ingest_remote_list_event(&client, &my_pk, &event, session.clone()).await {
        Ok(m) => m,
        Err(e) => {
            vector_core::log_warn!("[CommunityList] ingest remote update failed: {}", e);
            return;
        }
    };
    if !session.is_valid() {
        return;
    }
    // A removal that arrived in this update buries a community we still hold locally — tear it down so all
    // devices converge (the merged list already dropped its entry; honor any fresh tombstone here).
    for t in &merged.tombstones {
        if let Ok(b) = hex_to_id32(&t.community_id) {
            if let Ok(Some(_)) = vector_core::db::community::load_community(&CommunityId(b)) {
                // Receive path — tombstone local-only (we got here BY receiving the removal).
                self_remove_from_community(&t.community_id, false).await;
            }
        }
    }
    // page_messages = true: the live ingest path has NO boot sweep after it, so we must page the latest here
    // for a community that just appeared, or it would render empty until the user opens it.
    rehydrate_listed_communities(&merged, &session, true).await;
    if session.is_valid() {
        purge_stale_pending_invites(&merged.tombstones);
    }
}

/// Rehydrate + surface every listed community this device doesn't hold yet (auto-join silently); tombstone
/// any we've since been banned from. Shared by the boot reconcile and the live ingest path. `page_messages`
/// pages each rehydrated channel's latest — true on the live path (no sweep follows), false at boot (the boot
/// sweep pages right after, so paging here would double-fetch). Returns true if anything was rehydrated.
async fn rehydrate_listed_communities(
    list: &vector_core::community::list::CommunityList,
    session: &vector_core::state::SessionGuard,
    page_messages: bool,
) -> bool {
    let mut rehydrated_any = false;
    let transport = LiveTransport::with_timeout(Duration::from_secs(20));
    for entry in &list.entries {
        if !session.is_valid() {
            return rehydrated_any;
        }
        match vector_core::community::list::rehydrate_community_from_seed(&transport, entry, session.clone()).await {
            Ok(vector_core::community::list::RehydrateOutcome::Rehydrated(community)) => {
                // Surface its channel chat(s) so they load like any DM, then page the latest so it isn't empty.
                sync_community_chats(&community).await;
                // Push the full metadata to the live UI: a seamlessly-rehydrated community otherwise reaches
                // the frontend only via `message_new` (a bare "Group <id>" chat with no name/owner/members).
                // The frontend runs this summary through its join-path render so name/crown/members appear
                // without a restart.
                if session.is_valid() {
                    vector_core::emit_event(
                        "community_surfaced",
                        &serde_json::to_value(summarize(&community)).unwrap_or(serde_json::Value::Null),
                    );
                }
                if page_messages {
                    for ch in &community.channels {
                        let _ = sync_community_channel(ch.id.to_hex(), None).await;
                    }
                }
                // Quietly archive PRIOR epochs' keys in the background so older history loads on scroll-back —
                // the instant latest view above never waits for it. No-op for a never-re-founded community.
                let entry_for_backfill = entry.clone();
                let session_for_backfill = *session;
                let backfill_channels: Vec<String> = community.channels.iter().map(|c| c.id.to_hex()).collect();
                let backfill_cid = community.id.to_hex();
                tokio::spawn(async move {
                    let bt = LiveTransport::with_timeout(Duration::from_secs(20));
                    match vector_core::community::list::backfill_history_from_seed(
                        &bt, &entry_for_backfill, session_for_backfill,
                    ).await {
                        Ok(true) if session_for_backfill.is_valid() => {
                            // Prior-epoch keys are now archived. Clear the per-channel scroll floors (a
                            // scroll-back that raced the backfill may have falsely hit "history start"), then
                            // re-page each channel's latest with the now-complete keyset: the multi-epoch fetch
                            // spans all epochs, so a small community shows its whole history immediately and a
                            // busy one still loads older on scroll. Nudge the UI last.
                            for cid in &backfill_channels {
                                vector_core::community::cache::clear_channel_floors(cid);
                            }
                            for cid in &backfill_channels {
                                if !session_for_backfill.is_valid() {
                                    break;
                                }
                                let _ = sync_community_channel(cid.clone(), None).await;
                            }
                            vector_core::emit_event(
                                "community_refreshed",
                                &serde_json::json!({ "community_id": backfill_cid }),
                            );
                        }
                        Ok(_) => {}
                        Err(e) => vector_core::log_warn!(
                            "[CommunityList] history backfill {} failed: {}", backfill_cid, e,
                        ),
                    }
                });
                rehydrated_any = true;
            }
            Ok(vector_core::community::list::RehydrateOutcome::AlreadyHeld(_)) => {}
            Ok(vector_core::community::list::RehydrateOutcome::Removed) => {
                // Banned since the entry was written. Full teardown (DB + STATE chats + routes) + tombstone
                // local-only — boot's explicit publish propagates it; republishing here would re-echo.
                // Destructive after a long rehydrate await: re-validate the session first.
                if !session.is_valid() {
                    break;
                }
                let channel_ids: Vec<String> = hex_to_id32(&entry.community_id)
                    .ok()
                    .and_then(|b| vector_core::db::community::load_community(&CommunityId(b)).ok().flatten())
                    .map(|c| c.channels.iter().map(|ch| ch.id.to_hex()).collect())
                    .unwrap_or_default();
                teardown_community_local(&entry.community_id, &channel_ids, false).await;
            }
            Err(e) => {
                vector_core::log_warn!("[CommunityList] rehydrate {} failed: {}", entry.community_id, e);
            }
        }
    }
    if rehydrated_any && session.is_valid() {
        crate::services::subscription_handler::refresh_community_subscription().await;
    }
    rehydrated_any
}

/// Boot sweep: sync the LATEST page of every joined Community channel, most-recent-activity
/// first (so the top of the chat list refreshes first), through a sliding window of 3 to avoid
/// overwhelming the relays/bandwidth. ONE IPC call drives the whole sweep — no per-channel
/// frontend round-trips. Each page emits `message_new` as it lands, so the chat list fills in
/// progressively. Per-channel anti-stampede makes this safe to overlap with reconnect re-syncs.
#[tauri::command]
pub async fn sync_communities_boot() -> Result<(), String> {
    let session = vector_core::state::SessionGuard::capture();

    // pull the cross-device Community List + rehydrate any community this device hasn't joined yet,
    // BEFORE flattening channels — so a freshly-rehydrated community gets its messages paged in this same
    // boot pass instead of waiting for the next sync.
    reconcile_community_list_boot().await;
    if !session.is_valid() {
        return Ok(());
    }

    // Newest-message time per chat, for activity ordering.
    let last_msgs = crate::db::get_all_chats_last_messages().await.unwrap_or_default();
    let activity = |cid: &str| -> u64 {
        last_msgs.get(cid).and_then(|v| v.first().map(|m| m.at)).unwrap_or(0)
    };

    // Flatten every joined Community's channels.
    let mut channels: Vec<String> = Vec::new();
    for id in vector_core::db::community::list_community_ids()? {
        if let Ok(Some(community)) = vector_core::db::community::load_community(&id) {
            for ch in &community.channels {
                channels.push(ch.id.to_hex());
            }
        }
    }
    // Most-recent-activity first.
    channels.sort_by(|a, b| activity(b).cmp(&activity(a)));

    // No coverage cap — every joined Community syncs at boot (NIP-17 parity: bulk catch-up here,
    // realtime after, re-sync on reconnect; nothing on-demand). A sliding window (not fixed
    // batches) bounds peak relay load: a finished sync yields its slot to the next immediately
    // instead of waiting on the slowest of a chunk. Each sync is ~4 REQs (server-root probe +
    // control fold + rekey probe + page), so the window caps concurrent REQ pressure at window×4.
    // MVP is single-channel Communities, so a channel here is 1:1 with a Community.
    const BOOT_SYNC_WINDOW: usize = 3;
    use futures_util::stream::StreamExt;
    futures_util::stream::iter(channels)
        .map(|cid| async move {
            if session.is_valid() {
                let _ = sync_community_channel(cid, None).await;
            }
        })
        .buffer_unordered(BOOT_SYNC_WINDOW)
        .collect::<Vec<()>>()
        .await;
    Ok(())
}

/// Delete one of the local user's own Community messages. NIP-09-deletes the outer
/// event via its retained ephemeral key, then drops it locally (STATE + DB) and tells the
/// UI to remove the row. Errors if no key is retained for `message_id` (not ours, or
/// already deleted) — only the original sender can delete their own message.
#[tauri::command]
pub async fn delete_community_message(message_id: String) -> Result<(), String> {
    let session = vector_core::state::SessionGuard::capture();
    if !session.is_valid() {
        return Err("account changed during delete".to_string());
    }
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));

    // Snapshot the owning channel + attachment URLs BEFORE the cooperative echo removes the
    // row (the publish below locally applies its own 3305, dropping the message from STATE).
    let (channel_id, attachment_urls) = {
        let state = vector_core::state::STATE.lock().await;
        match state.find_message(&message_id) {
            Some((chat, msg)) => (
                chat.id.clone(),
                msg.attachments
                    .iter()
                    .filter(|a| !a.url.is_empty())
                    .map(|a| a.url.clone())
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
        if let Err(e) = vector_core::community::service::delete_message(&transport, &message_id).await {
            log_error!("Community relay delete failed, using cooperative tombstone only: {e}");
        }
    }

    // Layer 2 — cooperative tombstone (3305) so Vector peers hide it in-app even when we lack
    // the original signing key (the "Limited Delete" path). Author-gated on the peer side.
    publish_community_control(
        &channel_id,
        vector_core::stored_event::event_kind::COMMUNITY_DELETE,
        "",
        &message_id,
        &[],
    )
    .await?;

    // Layer 3 — best-effort Blossom blob delete for attachments (signed by the active
    // identity so bunker accounts authorize correctly).
    if !attachment_urls.is_empty() {
        if let Some(client) = vector_core::state::nostr_client() {
            if let Ok(signer) = client.signer().await {
                vector_core::blossom::delete_blobs_best_effort(signer, attachment_urls);
            }
        }
    }

    // Drop locally + tell the UI to remove the row (idempotent — the cooperative echo may
    // already have removed it). Re-check the session: the publishes above can take seconds,
    // and an account swap mid-publish must not delete from the swapped-in account's STATE/DB.
    if !session.is_valid() {
        return Err("account changed during delete".to_string());
    }
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
}

/// Owner moderation-hide: permanently hide ANY member's message (cooperative — honest clients
/// drop it because the 3305 carries the owner's real-npub signature, re-verified against the roster).
/// No undo. Owner-only (enforced by `publish_owner_hide` via the roster MANAGE_MESSAGES check).
#[tauri::command]
pub async fn hide_community_message(channel_id: String, message_id: String) -> Result<(), String> {
    let session = vector_core::state::SessionGuard::capture();
    if !session.is_valid() {
        return Err("account changed during hide".to_string());
    }
    let community_id = vector_core::db::community::community_id_for_channel(&channel_id)?
        .ok_or("Unknown Community channel")?;
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    let channel = community
        .channels
        .iter()
        .find(|c| c.id.to_hex() == channel_id)
        .ok_or("Channel not found in Community")?
        .clone();
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    vector_core::community::service::publish_owner_hide(&transport, &community, &channel, &message_id).await?;
    // Drop locally + tell the UI (idempotent — the cooperative echo may already have removed it).
    // Re-check after the multi-second publish so a mid-flight account swap can't delete from the
    // swapped-in account's STATE/DB.
    if !session.is_valid() {
        return Err("account changed during hide".to_string());
    }
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
            "reason": "hidden"
        }),
    );
    Ok(())
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
    let session = vector_core::state::SessionGuard::capture();
    let inserted = vector_core::db::events::save_system_event_at(event_id, channel_id, et, npub, note.as_deref(), created_at, invited_by, invited_label)
        .await
        .unwrap_or(false);
    if inserted && session.is_valid() {
        // A member we can't NAME renders as an npub stub everywhere (join/leave
        // line, member list, @mention pool) — queue their profile so the name
        // lands moments after the event. Gated on nameless-ness: a member we
        // already show a name for refreshes on the profile system's own cadence.
        let nameless = {
            let state = vector_core::state::STATE.lock().await;
            state.get_profile(npub).is_none_or(|p| {
                p.nickname.is_empty() && p.display_name.is_empty() && p.name.is_empty()
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
}

/// Shared publish path for a control event that targets an existing message — a reaction
/// (3301) or edit (3302). Signs via the active signer (local/bunker), publishes, retains
/// the key (so it's deletable), then locally echoes by feeding the published event back
/// through `process_incoming` (which applies it to STATE + yields the updated target
/// message); the relay echo dedups. `target` is the inner id of the message being
/// reacted-to / edited.
/// Persist a Community `Updated` outcome. Edits are event-sourced — saved as a foldable
/// MESSAGE_EDIT event so reload reconstructs the revision history like DMs, rather than
/// overwriting the row (which would strand the `(edited)` history). Reactions carry no edit
/// event and re-save the message row (which holds the new reaction).
pub(crate) async fn persist_community_update(
    channel_id: &str,
    message: &vector_core::types::Message,
    edit_event: Option<&vector_core::stored_event::StoredEvent>,
) {
    if let Some(ev) = edit_event {
        let mut ev = ev.clone();
        if let Ok(cid) = crate::db::get_chat_id_by_identifier(channel_id) {
            ev.chat_id = cid;
        }
        let _ = crate::db::save_event(&ev).await;
    } else {
        let _ = crate::db::save_message(channel_id, message).await;
    }
}

async fn publish_community_control(
    channel_id: &str,
    kind: u16,
    content: &str,
    target: &str,
    emoji_tags: &[vector_core::types::EmojiTag],
) -> Result<(), String> {
    let session = vector_core::state::SessionGuard::capture();
    let author_pk = vector_core::my_public_key().ok_or("Public key not set")?;

    let community_id = vector_core::db::community::community_id_for_channel(channel_id)?
        .ok_or("Unknown Community channel")?;
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    let channel = community
        .channels
        .iter()
        .find(|c| c.id.to_hex() == channel_id)
        .ok_or("Channel not found in Community")?
        .clone();

    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let unsigned = vector_core::community::envelope::build_inner_typed(
        author_pk, &channel.id, channel.epoch, kind, content, ms, Some(target), emoji_tags,
    );
    let client = vector_core::state::nostr_client().ok_or("Not logged in")?;
    let signer = client.signer().await.map_err(|e| format!("Signer unavailable: {e}"))?;
    let inner = unsigned.sign(&signer).await.map_err(|e| format!("Failed to sign: {e}"))?;
    if !session.is_valid() {
        return Err("account changed during send".to_string());
    }
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    let outer = service::send_signed_message(&transport, &community, &channel, &inner).await?;

    // Local echo: apply our own event immediately (relay echo dedups). Re-check after the
    // publish — an account swap during send must not echo into the swapped-in account.
    if !session.is_valid() {
        return Err("account changed during send".to_string());
    }
    let outcome = {
        let mut state = vector_core::state::STATE.lock().await;
        vector_core::community::inbound::process_incoming(&mut state, &outer, &channel, &author_pk)
    };
    if let Some(vector_core::community::inbound::IncomingEvent::Updated { target_id, message, edit_event }) = outcome {
        persist_community_update(channel_id, &message, edit_event.as_deref()).await;
        vector_core::emit_event(
            "message_update",
            &serde_json::json!({ "old_id": &target_id, "message": &message, "chat_id": channel_id }),
        );
    }
    Ok(())
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
    // For a custom-emoji reaction (`:shortcode:` + url), attach the `["emoji", sc, url]`.
    let emoji_tags: Vec<vector_core::types::EmojiTag> = match emoji_url {
        Some(url) if emoji.starts_with(':') && emoji.ends_with(':') && emoji.len() >= 3 => {
            vec![vector_core::types::EmojiTag {
                shortcode: emoji[1..emoji.len() - 1].to_string(),
                url,
            }]
        }
        _ => Vec::new(),
    };
    publish_community_control(
        &channel_id,
        vector_core::stored_event::event_kind::COMMUNITY_REACTION,
        &emoji,
        &message_id,
        &emoji_tags,
    )
    .await
}

/// Edit one of your own Community messages (only the original author's edit is honored).
#[tauri::command]
pub async fn edit_community_message(
    channel_id: String,
    message_id: String,
    new_content: String,
) -> Result<(), String> {
    // The edited content may introduce/keep custom emoji → carry their tags too.
    let emoji_tags = vector_core::emoji_packs::resolve_outbound_emoji_tags(&new_content);
    publish_community_control(
        &channel_id,
        vector_core::stored_event::event_kind::COMMUNITY_EDIT,
        &new_content,
        &message_id,
        &emoji_tags,
    )
    .await
}

/// Invite an npub to a Community by gift-wrapping its invite bundle to them.
///
/// `community_id` is the 64-char hex Community id; the caller must be the proven owner.
/// The bundle travels on the USER's DM relays (not the
/// Community relays), since a fresh invitee has no Community pseudonym yet.
#[tauri::command]
pub async fn invite_to_community(community_id: String, invitee_npub: String) -> Result<(), String> {
    let session = vector_core::state::SessionGuard::capture();

    let my_pk = vector_core::my_public_key().ok_or("Public key not set")?;

    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;

    // Role engine: anyone with CREATE_INVITE may invite (owner is just the top role).
    if !vector_core::community::service::caller_has_permission(&community, vector_core::community::roles::Permissions::CREATE_INVITE) {
        return Err("You need the create-invite permission to invite someone".to_string());
    }
    // A BANNED npub can't be re-invited: they self-removed and stay out — admins shouldn't be able to
    // pull them back in. Match the banlist (stored as lowercase hex) against the invitee.
    let invitee_hex = nostr_sdk::PublicKey::parse(&invitee_npub).map_err(|_| "invalid npub".to_string())?.to_hex();
    if vector_core::db::community::get_community_banlist(&community_id)?.iter().any(|b| b == &invitee_hex) {
        return Err("That member is banned from this community and can't be invited".to_string());
    }

    // The bundle is built from purely local state; bail if the account swapped before
    // we hand it to the gift-wrap path.
    if !session.is_valid() {
        return Err("account changed during invite".to_string());
    }

    let rumor = build_invite_rumor(&community, my_pk)?;
    let pending_id = format!("community-invite-{}", community_id);

    // self_send=false: the owner already holds the Community; echoing the bundle back
    // would be a no-op the inbound guard drops anyway, so don't even emit it.
    let config = SendConfig { self_send: false, ..SendConfig::gui() };
    let callback: Arc<dyn SendCallback> = Arc::new(NoOpSendCallback);

    send_rumor_dm(&invitee_npub, &pending_id, rumor, &config, callback)
        .await
        .map(|_| ())
}

/// List invites awaiting the user's accept/decline decision.
#[tauri::command]
pub async fn list_community_invites() -> Result<Vec<vector_core::db::community::PendingCommunityInvite>, String> {
    vector_core::db::community::list_pending_invites()
}

/// Ingest a warmed preload page into STATE + DB so a just-accepted community opens populated,
/// emitting message_new/update so the (optimistic, locked) chat row paints + unlocks NOW.
/// Presence/membership outcomes are deliberately left to the background true-up sync — it
/// re-fetches the page and applies them deduped, which is why the newest cursor is NOT
/// seeded here (a seeded cursor would skip that re-fetch and silently drop them).
/// Returns `true` if it painted ≥1 message (so the caller can flag the summary `preloaded` and the
/// frontend opens immediately instead of awaiting the first sync).
async fn promote_preloaded_page(community: &vector_core::community::Community, page: Vec<nostr_sdk::Event>) -> bool {
    use vector_core::community::inbound::IncomingEvent;
    let Some(channel) = community.channels.first().cloned() else { return false };
    let channel_id = channel.id.to_hex();
    let Some(my_pk) = vector_core::my_public_key() else { return false };
    let session = vector_core::state::SessionGuard::capture();
    if !session.is_valid() {
        return false;
    }
    let outcomes = {
        let mut state = vector_core::state::STATE.lock().await;
        vector_core::community::inbound::process_channel_batch(&mut state, &page, &channel, &my_pk)
    };
    let mut painted = 0u32;
    for outcome in &outcomes {
        if !session.is_valid() {
            break;
        }
        match outcome {
            IncomingEvent::NewMessage(msg) => {
                let _ = crate::db::save_message(&channel_id, msg).await;
                // Emit so the (optimistic, locked) chat row populates + unlocks NOW — the frontend
                // learns messages via message_new, so without this the promote is invisible to the UI.
                vector_core::emit_event(
                    "message_new",
                    &serde_json::json!({ "message": msg, "chat_id": &channel_id }),
                );
                painted += 1;
            }
            IncomingEvent::Updated { target_id, message, edit_event } => {
                persist_community_update(&channel_id, message, edit_event.as_deref()).await;
                vector_core::emit_event(
                    "message_update",
                    &serde_json::json!({ "old_id": target_id, "message": message, "chat_id": &channel_id }),
                );
            }
            IncomingEvent::Removed { target_id } => {
                let _ = crate::db::delete_event(target_id).await;
            }
            // Presence / membership outcomes are left to the background true-up sync (it re-fetches
            // the same page and applies them, deduped) — promotion only paints the message content.
            _ => {}
        }
    }
    painted > 0
}

/// Accept a parked invite: join as a member, persist, and start receiving. Guards
/// against id-collision overwrites (vector-core `service::accept_invite`), then dials
/// the Community relays + subscribes.
///
/// The peek→accept→delete-on-success ordering is contract-tested in vector-core
/// (`community::service::tests::rejected_accept_leaves_pending_invite_intact`); keep
/// them in sync if you reorder here.
#[tauri::command]
pub async fn accept_community_invite(community_id: String) -> Result<CommunitySummary, String> {
    let session = vector_core::state::SessionGuard::capture();

    // Peek WITHOUT deleting: accept is fallible (caps, owner/authority collision), and a
    // rejected accept must leave the invite parked so the user can retry or decline.
    let bundle_json = vector_core::db::community::get_pending_invite(&community_id)?
        .ok_or("No pending invite for that Community")?;
    let invite = CommunityInvite::from_json(&bundle_json)?;

    // Guarded save (caps + owner/authority-collision checks + its own SessionGuard). On
    // error the pending row is untouched.
    let community = vector_core::community::service::accept_invite(&invite)?;

    // Don't clear the parked row into a swapped-in account's DB.
    if !session.is_valid() {
        return Err("account changed during invite accept".to_string());
    }
    // Joined — clear the parked row + register the channel chat(s) locally. accept_invite already
    // installed the bundle's keys, so the community is read/writeable now; everything below is relay-bound
    // PROPAGATION the user shouldn't wait on (where the join latency lived — esp. the 12s presence timeout).
    // Return the summary the instant local state is ready; fan the rest out in the background.
    vector_core::db::community::delete_pending_invite(&community_id)?;
    sync_community_chats(&community).await;

    // Promote a warmed preload: if we fetched this community's first page ahead of Join (invite
    // receive / public preview), ingest it NOW so the chat opens populated instead of waiting on the
    // first sync. The background sync still runs and trues it up; dedup makes the re-fetch harmless.
    let preloaded = match vector_core::community::cache::take_ready_preload(&community_id) {
        Some(page) => promote_preloaded_page(&community, page).await,
        None => false,
    };

    // Local-first self-join: build our join presence, record the "X joined" system event immediately
    // (memory→DB + UI), then publish it in the background. The relay echo dedups by this inner's id, so
    // our own join shows instantly without waiting on (or depending on) the echo — same as an outgoing
    // message. Without this a fresh join opens to an empty timeline until the relay round-trips the echo.
    if let Some(primary) = community.channels.first() {
        if let Ok(inner) = vector_core::community::service::build_presence(primary, true, None).await {
            if let Some(my_npub) = vector_core::my_public_key().and_then(|pk| pk.to_bech32().ok()) {
                // Presence signing can be slow (bunker) — re-validate before the local write.
                if session.is_valid() {
                    apply_community_presence(
                        &primary.id.to_hex(), &my_npub, true,
                        &inner.id.to_hex(), inner.created_at.as_secs(), None, None,
                    ).await;
                }
            }
            let bg_pub = vector_core::state::SessionGuard::capture();
            let community_pub = community.clone();
            let primary_pub = primary.clone();
            tokio::spawn(async move {
                if !bg_pub.is_valid() { return; }
                let transport = LiveTransport::with_timeout(Duration::from_secs(12));
                let _ = vector_core::community::service::publish_presence_event(&transport, &community_pub, &primary_pub, &inner).await;
            });
        }
    }

    // Background: record the cross-device membership (so our other devices auto-join) + (re)subscribe for
    // realtime. SessionGuard re-checked so a mid-flight account swap can't write account A's join into B.
    let bg = vector_core::state::SessionGuard::capture();
    let community_bg = community.clone();
    tokio::spawn(async move {
        if !bg.is_valid() { return; }
        vector_core::community::list::add_membership(&community_bg);
        crate::services::subscription_handler::refresh_community_subscription().await;
    });

    Ok(CommunitySummary { preloaded, ..summarize(&community) })
}

/// Decline a parked invite. Drops it locally, writes a decline tombstone to the synced Community List
/// (so a sibling device drops its copy too, and a re-delivered/older invite stays suppressed — a
/// strictly-newer one resurfaces), and immediately sheds the relays this invite's preload warmed.
#[tauri::command]
pub async fn decline_community_invite(community_id: String) -> Result<(), String> {
    // Grab the bundle's relays before dropping it, so we can shed what its preload warmed (the
    // immediate counterpart to the TTL prune; an accepted invite would have kept them).
    let relays: Vec<String> = vector_core::db::community::get_pending_invite(&community_id)
        .ok()
        .flatten()
        .and_then(|j| vector_core::community::invite::CommunityInvite::from_json(&j).ok())
        .map(|inv| inv.relays)
        .unwrap_or_default();

    vector_core::db::community::delete_pending_invite(&community_id)?;
    // Cross-device + durable suppression: tombstone (reuses the leave path's publish/converge) so the
    // un-deletable 3304 can't re-nag and other devices drop their parked copy.
    vector_core::community::list::remove_membership(&community_id);
    // Drop any lingering warm entry, then prune the relays no joined community needs.
    vector_core::community::cache::abort_preload(&community_id);
    if !relays.is_empty() {
        vector_core::community::transport::prune_unneeded_community_relays(&relays).await;
    }
    Ok(())
}

// ============================================================================
// Community display metadata (name / description / logo / banner)
// ============================================================================

/// Edit a Community's text metadata (owner only) and republish the GroupRoot so members
/// pick it up. `None` leaves a field unchanged. Previews + the app reflect the change.
#[tauri::command]
pub async fn update_community_metadata(
    community_id: String,
    name: Option<String>,
    description: Option<String>,
) -> Result<(), String> {
    let session = vector_core::state::SessionGuard::capture();
    let id_bytes = hex_to_id32(&community_id)?;
    let mut community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    if let Some(n) = name {
        community.name = n;
    }
    if let Some(d) = description {
        // Empty string clears the description.
        community.description = if d.is_empty() { None } else { Some(d) };
    }
    if !session.is_valid() {
        return Err("account changed during metadata update".to_string());
    }
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    service::republish_community_metadata(&transport, &community).await?;
    sync_community_chats(&community).await;
    // the name lives in the synced list's `current` snapshot — refresh it so other devices show the
    // new name on rehydrate without waiting to fold the GroupRoot (no-op if unchanged).
    vector_core::community::list::refresh_membership_current(&community);
    Ok(())
}

/// Rename a channel (requires manage-channels authority) and republish its ChannelMetadata so members
/// pick it up. `channel_id` is the channel's hex id.
#[tauri::command]
pub async fn rename_community_channel(
    community_id: String,
    channel_id: String,
    name: String,
) -> Result<(), String> {
    let session = vector_core::state::SessionGuard::capture();
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    let ch_bytes = hex_to_id32(&channel_id)?;
    let ch_id = vector_core::community::ChannelId(ch_bytes);
    if !session.is_valid() {
        return Err("account changed during channel rename".to_string());
    }
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    service::republish_channel_metadata(&transport, &community, &ch_id, &name).await?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?.unwrap_or(community);
    sync_community_chats(&community).await;
    // channel names ride the list's `current` snapshot too — refresh so a rehydrating device shows
    // the renamed channel (no-op if unchanged).
    vector_core::community::list::refresh_membership_current(&community);
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
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    let image = match if is_banner { community.banner } else { community.icon } {
        Some(img) => img,
        None => return Ok(None),
    };
    download_decrypt_cache_image(&handle, &image).await.map(Some)
}

/// Resolve an invite-preview logo to a local cached file path. Unlike `cache_community_image`
/// this works BEFORE the community is joined/persisted: the `CommunityImage` (url/key/nonce/hash)
/// arrives straight from `preview_public_invite`, so we decrypt it without a DB lookup.
#[tauri::command]
pub async fn cache_invite_logo(
    image: vector_core::community::CommunityImage,
) -> Result<String, String> {
    let handle = crate::TAURI_APP.get().ok_or("App handle not initialized")?.clone();
    download_decrypt_cache_image(&handle, &image).await
}

/// Download an encrypted community image blob, decrypt + verify it against the committed hash,
/// and cache the plaintext. Returns the local file path. Shared by `cache_community_image`
/// (joined communities) and `cache_invite_logo` (invite previews).
async fn download_decrypt_cache_image<R: tauri::Runtime>(
    handle: &tauri::AppHandle<R>,
    image: &vector_core::community::CommunityImage,
) -> Result<String, String> {
    // Fast path: already cached (keyed by the encrypted blob URL).
    if let Some(path) =
        crate::image_cache::get_cached_path(handle, &image.url, crate::image_cache::ImageType::Avatar)
    {
        return Ok(path);
    }

    // Download the ciphertext (Tor failsafe applies via build_http_client), bounded.
    const MAX_IMG: usize = 10 * 1024 * 1024;
    let client = vector_core::net::build_http_client(std::time::Duration::from_secs(30))?;
    let mut resp = client.get(&image.url).send().await.map_err(|e| format!("download: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download failed: HTTP {}", resp.status()));
    }
    // Stream with a hard cap: `content_length` is absent under chunked transfer-encoding, so
    // a single buffered read could OOM on a hostile/oversized blob. Abort as soon as the
    // running total exceeds MAX_IMG (memory bounded to MAX_IMG + one chunk).
    let mut encrypted: Vec<u8> = Vec::with_capacity(
        resp.content_length().map(|l| (l as usize).min(MAX_IMG)).unwrap_or(64 * 1024),
    );
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("read body: {e}"))? {
        if encrypted.len() + chunk.len() > MAX_IMG {
            return Err("community image too large".to_string());
        }
        encrypted.extend_from_slice(&chunk);
    }

    let decrypted = vector_core::crypto::decrypt_data(&encrypted, &image.key, &image.nonce)?;
    // Integrity: the plaintext must match the hash committed in the sealed metadata.
    if vector_core::crypto::sha256_hex(&decrypted) != image.hash {
        return Err("community image failed integrity check".to_string());
    }

    match crate::image_cache::precache_image_bytes(
        handle,
        &image.url,
        &decrypted,
        crate::image_cache::ImageType::Avatar,
    ) {
        crate::image_cache::CacheResult::Cached(p)
        | crate::image_cache::CacheResult::AlreadyCached(p) => Ok(p),
        crate::image_cache::CacheResult::Failed(e) => Err(format!("cache image: {e}")),
    }
}

/// Set a Community's logo or banner: encrypt the image at `filepath` with a
/// fresh per-file key (NIP-17 attachment technique), upload the ciphertext to Blossom,
/// store the ref (key gated by the server-root inside the GroupRoot), and republish.
/// `is_banner` targets the banner instead of the icon. Authority (MANAGE_METADATA,
/// the same as name/description edits) is enforced downstream by republish_community_metadata.
#[tauri::command]
pub async fn set_community_image(
    community_id: String,
    filepath: String,
    is_banner: bool,
) -> Result<(), String> {
    use vector_core::community::CommunityImage;

    let session = vector_core::state::SessionGuard::capture();
    let id_bytes = hex_to_id32(&community_id)?;
    let mut community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;

    let bytes = std::fs::read(&filepath).map_err(|e| format!("read image: {e}"))?;
    let ext = std::path::Path::new(&filepath)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_lowercase();
    let plaintext_hash = vector_core::crypto::sha256_hex(&bytes);

    // Encrypt with a fresh random key+nonce (same as file attachments); the key rides in
    // the ServerRoot-sealed GroupRoot, so only members can decrypt the blob.
    let params = vector_core::crypto::generate_encryption_params();
    let encrypted = vector_core::crypto::encrypt_data(&bytes, &params)?;

    let client = vector_core::state::nostr_client().ok_or("Nostr client not initialised")?;
    let signer = client.signer().await.map_err(|e| format!("signer: {e}"))?;
    let servers = vector_core::blossom_servers::compute_enabled_servers();
    if servers.is_empty() {
        return Err("No Blossom servers configured.".to_string());
    }
    let url = vector_core::blossom::upload_blob_with_failover(
        signer,
        servers,
        std::sync::Arc::new(encrypted),
        Some("application/octet-stream"),
    )
    .await?;

    if !session.is_valid() {
        return Err("account changed during image upload".to_string());
    }
    let image = CommunityImage { url, key: params.key, nonce: params.nonce, hash: plaintext_hash, ext };
    if is_banner {
        community.banner = Some(image);
    } else {
        community.icon = Some(image);
    }
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    service::republish_community_metadata(&transport, &community).await?;
    sync_community_chats(&community).await;
    Ok(())
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
    let session = vector_core::state::SessionGuard::capture();
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    if !session.is_valid() {
        return Err("account changed during invite creation".to_string());
    }
    let expires_at = expires_in_secs.map(|secs| now_secs().saturating_add(secs));
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    let (_token, url) = service::create_public_invite(&transport, &community, expires_at, label).await?;
    Ok(url)
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
    let (relays, token) = parse_invite_url(&url).map_err(|e| e.to_string())?;
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    let bundle = service::fetch_public_invite(&transport, &relays, &token).await?;
    let community_id = bundle.join.community_id.clone();

    // Already a member → local state IS the preview (the community's own sync keeps it fresh).
    // No fold, no preload, no snapshot: one source of truth, zero divergence.
    if let Ok(id_bytes) = hex_to_id32(&community_id) {
        if let Ok(Some(local)) = vector_core::db::community::load_community(&CommunityId(id_bytes)) {
            return Ok(PublicInvitePreviewInfo {
                preview: PublicInvitePreview {
                    name: local.name.clone(),
                    description: local.description.clone(),
                    icon: local.icon.clone(),
                },
                community_id,
            });
        }
    }

    // Warm the community's first page in the background while the user reads the preview, so an
    // Accept opens populated. RAM-only + best-effort; promotion on Join re-validates freshness.
    let invite_warm = bundle.join.clone();
    let bg = vector_core::state::SessionGuard::capture();
    tokio::spawn(async move {
        if !bg.is_valid() {
            return;
        }
        vector_core::community::service::preload_community(&invite_warm).await;
    });
    // Not a member: fold the live plane for the LATEST display metadata — the bundle's
    // mint-time snapshot is only the fallback.
    let preview = service::latest_invite_preview(&transport, &bundle).await;
    Ok(PublicInvitePreviewInfo { preview, community_id })
}

/// Accept a public-invite URL: fetch the bundle, join as a member (expiry + id-collision
/// guarded), and start receiving.
#[tauri::command]
pub async fn accept_public_invite(url: String) -> Result<CommunitySummary, String> {
    let session = vector_core::state::SessionGuard::capture();
    let (relays, token) = parse_invite_url(&url).map_err(|e| e.to_string())?;
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    let bundle = service::fetch_public_invite(&transport, &relays, &token).await?;
    if !session.is_valid() {
        return Err("account changed during invite accept".to_string());
    }
    let community = service::accept_public_invite(&bundle, now_secs())?;
    if !session.is_valid() {
        return Err("account changed during invite accept".to_string());
    }
    // Open NOW, gate in the BACKGROUND (mirrors the private accept). Register the chat + promote the
    // warmed page so the chat opens populated instantly; the control gate (rotation follow + banlist /
    // read-cut) runs in the background first-sync (fired by the frontend) AND in the membership task
    // below, tearing the chat down if this link's holder turns out to be banned/removed. The page
    // shown is content the link's own keys already decrypt, so the brief pre-teardown window discloses
    // nothing new — the rekey + teardown cut all future access.
    sync_community_chats(&community).await;
    let preloaded = match vector_core::community::cache::take_ready_preload(&community.id.to_hex()) {
        Some(page) => promote_preloaded_page(&community, page).await,
        None => false,
    };

    // Local-first join presence (carry the link's attribution), published in the background — so the
    // "X joined" line shows instantly without waiting on the 12s presence publish.
    let attribution = bundle.creator_npub.clone().map(|by| (by, bundle.label.clone()));
    if let Some(primary) = community.channels.first() {
        if let Ok(inner) = service::build_presence(primary, true, attribution.clone()).await {
            if let Some(my_npub) = vector_core::my_public_key().and_then(|pk| pk.to_bech32().ok()) {
                let (by, label) = match &attribution {
                    Some((b, l)) => (Some(b.as_str()), l.as_deref()),
                    None => (None, None),
                };
                // Presence signing can be slow (bunker) — re-validate before the local write.
                if session.is_valid() {
                    apply_community_presence(
                        &primary.id.to_hex(), &my_npub, true,
                        &inner.id.to_hex(), inner.created_at.as_secs(), by, label,
                    ).await;
                }
            }
            let bg_pub = vector_core::state::SessionGuard::capture();
            let community_pub = community.clone();
            let primary_pub = primary.clone();
            tokio::spawn(async move {
                if !bg_pub.is_valid() { return; }
                let transport = LiveTransport::with_timeout(Duration::from_secs(12));
                let _ = service::publish_presence_event(&transport, &community_pub, &primary_pub, &inner).await;
            });
        }
    }

    // Background: follow any rotation the link predates (so membership records CURRENT keys; an
    // excluding rotation = removal → teardown), then record cross-device membership + (re)subscribe.
    let bg = vector_core::state::SessionGuard::capture();
    let community_bg = community.clone();
    tokio::spawn(async move {
        if !bg.is_valid() { return; }
        let bt = LiveTransport::with_timeout(Duration::from_secs(20));
        if let Ok(c) = service::catch_up_server_root(&bt, &community_bg).await {
            if c.removed {
                self_remove_from_community(&community_bg.id.to_hex(), false).await;
                return;
            }
        }
        let community_bg = vector_core::db::community::load_community(&community_bg.id)
            .ok()
            .flatten()
            .unwrap_or(community_bg);
        // Public bans mint no rekey — fold control + check the banlist HERE too (not only the
        // frontend-fired first sync), so a banned link-holder tears down even if that sync never runs.
        let _ = service::fetch_and_apply_control(&bt, &community_bg).await;
        let community_bg = vector_core::db::community::load_community(&community_bg.id)
            .ok()
            .flatten()
            .unwrap_or(community_bg);
        if service::am_i_banned(&community_bg) {
            self_remove_from_community(&community_bg.id.to_hex(), false).await;
            return;
        }
        if !bg.is_valid() { return; }
        vector_core::community::list::add_membership(&community_bg);
        crate::services::subscription_handler::refresh_community_subscription().await;
    });
    Ok(CommunitySummary { preloaded, ..summarize(&community) })
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
    let session = vector_core::state::SessionGuard::capture();
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    let token_bytes = hex_to_id32(&token)?;
    if !session.is_valid() {
        return Err("account changed during invite revoke".to_string());
    }
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    service::revoke_public_invite(&transport, &community, &token_bytes).await?;
    // Privatize re-founds (advances the server-root + every channel epoch), so OUR realtime sub is now
    // pinned to the OLD pseudonyms — rebuild it (if still our account) so live delivery resumes at the new
    // epoch immediately, instead of only on the next sync.
    if session.is_valid() {
        crate::services::subscription_handler::refresh_community_subscription().await;
    }
    Ok(())
}

/// Current Unix time in seconds.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Decode a 64-char hex Community id to 32 bytes (rejects malformed input).
/// Resolve a channel id (hex) to its owning Community + Channel — the shared front half of
/// every channel-addressed send.
pub(crate) fn resolve_community_channel(
    channel_id: &str,
) -> Result<(vector_core::community::Community, vector_core::community::Channel), String> {
    let community_id = vector_core::db::community::community_id_for_channel(channel_id)?
        .ok_or("Unknown Community channel")?;
    let id_bytes = hex_to_id32(&community_id)?;
    let community = vector_core::db::community::load_community(&CommunityId(id_bytes))?
        .ok_or("Community not found")?;
    let channel = community
        .channels
        .iter()
        .find(|c| c.id.to_hex() == channel_id)
        .ok_or("Channel not found in Community")?
        .clone();
    Ok((community, channel))
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
