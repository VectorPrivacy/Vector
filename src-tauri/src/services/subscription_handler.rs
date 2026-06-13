//! Live subscription handler for real-time Nostr events.
//!
//! This module handles:
//! - GiftWrap subscription (DMs, files)
//! - Community (kind-3300) message subscription

use nostr_sdk::prelude::*;

use std::collections::HashMap;
use std::sync::LazyLock;
use tokio::sync::Mutex;

use crate::nostr_client;

/// Current Community (kind-3300) subscription id — single subscription scoped to the
/// epoch pseudonyms of every channel we hold, refreshed on join/leave/rekey.
pub(crate) static COMMUNITY_SUB_ID: LazyLock<Mutex<Option<SubscriptionId>>> =
    LazyLock::new(|| Mutex::new(None));

/// Self-sync subscription ids: our OWN replaceable "settings" lists (the cross-device Community List 30078,
/// and the emoji-pack List 10030). One OPEN sub per filter (no `limit(0)` — these are replaceable, so the
/// relay replays the latest stored at connect = boot/reconnect sync, AND streams every later edit = instant
/// cross-device). A join/leave/pack-change on one device lands on the others with no reboot.
pub(crate) static SELFSYNC_SUB_IDS: LazyLock<Mutex<Vec<SubscriptionId>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Last self-sync event id processed per kind. A replaceable event stored on N relays is delivered N times
/// with the SAME id; without this every copy would kick a full ingest/rehydrate sweep (N× the work). A
/// genuine update has a new id and passes through.
static SELFSYNC_LAST_EVENT: LazyLock<Mutex<HashMap<u16, EventId>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Routing table: `z` pseudonym (hex) → the Channel it belongs to. Lets the
/// notification loop open an arriving event against the right channel key. Rebuilt
/// alongside the subscription.
pub(crate) static COMMUNITY_ROUTES: LazyLock<Mutex<HashMap<String, vector_core::community::Channel>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Routing table: control-plane `z` pseudonym (hex) → the community id it belongs to. Lets the
/// notification loop catch a CONTROL edition (kind 3308 — banlist, roles, metadata, invite-links) in
/// REALTIME and refresh that community's control state (so e.g. a ban self-removes the instant it lands,
/// not only on the next sync). Rebuilt alongside the channel routes.
pub(crate) static CONTROL_ROUTES: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Rebuild ONLY the in-memory routing tables (`COMMUNITY_ROUTES` pseudonym→channel, `CONTROL_ROUTES`
/// control-pseudonym→community) from the freshly persisted communities, WITHOUT touching the live relay
/// subscription. The cached `Channel` (incl. its `banned` set, which the inbound drop-filter reads) is
/// refreshed here — so a remotely-received ban/unban takes live effect without a full resubscribe (which
/// would re-enter the notification loop). Returns the pseudonyms + relays for a caller that also resubscribes.
pub(crate) async fn rebuild_community_routes() -> (Vec<String>, std::collections::HashSet<String>) {
    let mut routes: HashMap<String, vector_core::community::Channel> = HashMap::new();
    let mut control_routes: HashMap<String, String> = HashMap::new();
    let mut pseudonyms: Vec<String> = Vec::new();
    let mut relays: std::collections::HashSet<String> = std::collections::HashSet::new();

    if let Ok(ids) = vector_core::db::community::list_community_ids() {
        for id in ids {
            if let Ok(Some(community)) = vector_core::db::community::load_community(&id) {
                for r in &community.relays {
                    relays.insert(r.clone());
                }
                for ch in &community.channels {
                    // Subscribe to EVERY held epoch pseudonym (not just the head), so a straggler posting
                    // under a retained older epoch still arrives in realtime. The inbound router opens each
                    // event against the channel's full keyset (read_epoch_keys), so all map to one channel.
                    for (epoch, key) in ch.read_epoch_keys() {
                        let pseudonym =
                            vector_core::community::derive::channel_pseudonym(&key, &ch.id, epoch).to_hex();
                        pseudonyms.push(pseudonym.clone());
                        routes.insert(pseudonym, ch.clone());
                    }
                    // The NEXT channel-rekey coordinate (kind 3303), under the CURRENT server root — so a
                    // channel rotation is FOLLOWED event-driven, the instant it hits the relay, even if it's
                    // minutes-late (relay propagation). A re-founding rekeys the channel under the NEW root,
                    // so we only watch the right coordinate AFTER following the base + resubscribing; until
                    // then the post-base-follow fetch-retry covers the already-published case. Routed like a
                    // control edition → refresh_community_control's catch_up applies it.
                    let next_chan = vector_core::community::derive::rekey_pseudonym(
                        &community.server_root_key, &ch.id,
                        vector_core::community::Epoch(ch.epoch.0 + 1)).to_hex();
                    pseudonyms.push(next_chan.clone());
                    control_routes.insert(next_chan, community.id.to_hex());
                }
                // Control-plane pseudonym at the current server-root epoch — so a banlist/role/metadata/
                // invite-links edition (kind 3308) arrives in REALTIME and we refresh that community.
                let ctrl = vector_core::community::roster::control_pseudonym(
                    &community.server_root_key, &community.id, community.server_root_epoch);
                pseudonyms.push(ctrl.clone());
                control_routes.insert(ctrl, community.id.to_hex());
                // The NEXT base re-founding coordinate (kind 3303 lands here) — so a privatize / private-ban
                // rekey is FOLLOWED in realtime, not only on next sync. Routed like a control edition; the
                // handler runs refresh_community_control's full catch_up. Without this, the re-founding's
                // trigger (the banlist/invite 3308) arrives BEFORE the 3303 exists, so the catch-up no-ops
                // and the 3303 itself never reaches us (we're not otherwise subscribed to rekey coordinates).
                let next_base = vector_core::community::derive::base_rekey_pseudonym(
                    &community.server_root_key, &community.id,
                    vector_core::community::Epoch(community.server_root_epoch.0 + 1)).to_hex();
                pseudonyms.push(next_base.clone());
                control_routes.insert(next_base, community.id.to_hex());
            }
        }
    }

    *COMMUNITY_ROUTES.lock().await = routes;
    *CONTROL_ROUTES.lock().await = control_routes;
    (pseudonyms, relays)
}

/// Rebuild the Community subscription: scope it to the epoch pseudonyms of every
/// channel in every Community we hold, and rebuild the pseudonym→channel routing
/// table. Called at boot and whenever Communities/channels change.
pub(crate) async fn refresh_community_subscription() {
    let Some(client) = nostr_client() else { return; };

    let (pseudonyms, relays) = rebuild_community_routes().await;

    let mut sub_guard = COMMUNITY_SUB_ID.lock().await;
    if let Some(old_id) = sub_guard.take() {
        client.unsubscribe(&old_id).await;
    }

    if !pseudonyms.is_empty() {
        // Community events live on the Community's relays, which may differ from the user's own DM
        // relays. Add them GOSSIP|PING (24/7 warm, but excluded from pool-wide DM/profile ops — the
        // user's traffic never touches relays they don't own) and subscribe to them by TARGET, since
        // a non-READ relay is skipped by the pool-wide `subscribe(None)`. An overlap relay that's
        // also a user relay keeps its READ+WRITE flags and its single existing connection.
        for r in &relays {
            let _ = client.pool().add_relay(r.as_str(), vector_core::community_relay_options()).await;
        }
        client.connect().await;

        // Messages (3300), reactions (3301), and edits (3302) all flow on the channel's
        // epoch pseudonym; the inbound router dispatches by inner kind.
        use vector_core::stored_event::event_kind as ek;
        let filter = Filter::new()
            .kinds([
                Kind::Custom(ek::COMMUNITY_MESSAGE),
                Kind::Custom(ek::COMMUNITY_REACTION),
                Kind::Custom(ek::COMMUNITY_EDIT),
                Kind::Custom(ek::COMMUNITY_DELETE),
                Kind::Custom(ek::COMMUNITY_PRESENCE),
                Kind::Custom(ek::COMMUNITY_KICK),
                Kind::Custom(ek::COMMUNITY_WEBXDC),
                Kind::Custom(ek::COMMUNITY_CONTROL),
                Kind::Custom(ek::COMMUNITY_REKEY),
            ])
            .custom_tags(SingleLetterTag::lowercase(Alphabet::Z), pseudonyms)
            .limit(0);
        // Targeted at the Community relays (flag-independent) — NOT pool-wide, which would skip the
        // GOSSIP|PING Community relays and uselessly REQ the user's own relays that lack these events.
        match client.subscribe_to(relays.iter().cloned(), filter, None).await {
            Ok(output) => { *sub_guard = Some(output.val); }
            Err(e) => eprintln!("[community] Failed to subscribe: {:?}", e),
        }
    }
}

/// (Re)subscribe to our own replaceable self-sync lists (Community List + emoji list). Open subscriptions
/// (no `limit(0)`): the relay replays the current stored event on connect AND on every reconnect, then
/// streams edits live — so this one mechanism covers boot sync, reconnect re-sync, AND instant cross-device.
/// Idempotent: drops any prior ids first (account swap / re-entry).
pub(crate) async fn subscribe_self_sync() {
    let Some(client) = nostr_client() else { return };
    let Some(my_pk) = vector_core::my_public_key() else { return };

    // Subscribe FIRST (no lock held across relay I/O), then atomically swap the id set under one lock and
    // unsubscribe whatever it displaced — so two concurrent calls (start racing a swap re-entry) can't leak
    // an orphaned subscription or leave the routing set momentarily empty.
    let mut new_ids = Vec::new();
    // Community List (parameterized-replaceable, d-tag scoped so it never aliases a wallpaper/badge 30078).
    let community_filter = Filter::new()
        .author(my_pk)
        .kind(Kind::Custom(vector_core::stored_event::event_kind::APPLICATION_SPECIFIC))
        .identifier(vector_core::community::list::COMMUNITY_LIST_D_TAG);
    match client.subscribe(community_filter, None).await {
        Ok(out) => new_ids.push(out.val),
        Err(e) => eprintln!("[self-sync] community-list subscribe failed: {:?}", e),
    }
    // Emoji-pack List (replaceable kind 10030).
    let emoji_filter = Filter::new().author(my_pk).kind(Kind::Custom(10030));
    match client.subscribe(emoji_filter, None).await {
        Ok(out) => new_ids.push(out.val),
        Err(e) => eprintln!("[self-sync] emoji-list subscribe failed: {:?}", e),
    }

    let displaced = {
        let mut ids = SELFSYNC_SUB_IDS.lock().await;
        std::mem::replace(&mut *ids, new_ids)
    };
    for id in displaced {
        client.unsubscribe(&id).await;
    }
}

/// Route an arriving self-sync list event (our own replaceable settings): a Community List update folds +
/// rehydrates (so a join on another device appears live); an emoji-list update refreshes the pack set.
/// Spawned off the notification loop — both run several relay fetches and must not head-of-line-block it.
async fn handle_self_sync_event(session: &vector_core::state::SessionGuard, event: Event) {
    if !session.is_valid() {
        return;
    }
    // Coalesce multi-relay re-delivery of the SAME replaceable event (same id) so one update = one sweep.
    {
        let mut last = SELFSYNC_LAST_EVENT.lock().await;
        if last.get(&event.kind.as_u16()) == Some(&event.id) {
            return;
        }
        last.insert(event.kind.as_u16(), event.id);
    }
    match event.kind.as_u16() {
        k if k == vector_core::stored_event::event_kind::APPLICATION_SPECIFIC => {
            tokio::spawn(async move {
                crate::commands::community::ingest_community_list_update(event).await;
            });
        }
        10030 => {
            tokio::spawn(async move {
                let _ = vector_core::emoji_packs::refresh_subscribed_packs().await;
            });
        }
        _ => {}
    }
}

/// Route an arriving Community (kind-3300) event: find the channel its `z` pseudonym
/// maps to, open + verify + ingest it into STATE, then persist + emit if it is new.
/// Events that fail to open (wrong key, splice, forged sig) are dropped inside
/// `process_incoming`. (The notification loop's `session.is_valid()` gate above guards
/// against account-swap before dispatch.)
async fn handle_community_event(
    session: &vector_core::state::SessionGuard,
    event: Event,
) {
    let Some(my_pk) = crate::my_public_key() else { return; };
    let pseudonym = event.tags.iter().find_map(|t| {
        let s = t.as_slice();
        (s.len() >= 2 && s[0] == "z").then(|| s[1].clone())
    });
    let Some(pseudonym) = pseudonym else { return; };

    // A CONTROL edition (3308 — banlist/roles/metadata/invite-links) OR a base re-founding REKEY (3303)
    // arrived in REALTIME. Both route by pseudonym → refresh that community: re-fold control AND follow any
    // rekey (refresh_community_control does the full catch_up). So a ban self-removes the instant it lands, a
    // role/metadata/mode change applies live, and a privatize/private-ban re-founding is followed in realtime.
    let kind = event.kind.as_u16();
    if kind == vector_core::stored_event::event_kind::COMMUNITY_CONTROL
        || kind == vector_core::stored_event::event_kind::COMMUNITY_REKEY
    {
        let community_id = CONTROL_ROUTES.lock().await.get(&pseudonym).cloned();
        if let Some(community_id) = community_id {
            // SPAWN off the notification loop — the refresh runs several relay fetches (seconds) and must
            // not head-of-line-block DM/other-community event consumption. It self-captures a SessionGuard
            // + re-checks before every write, and loads by community id (account-scoped), so a mid-flight
            // swap is safe.
            if session.is_valid() {
                tokio::spawn(async move {
                    crate::commands::community::refresh_community_control(&community_id).await;
                });
            }
        }
        return;
    }

    let Some(channel) = COMMUNITY_ROUTES.lock().await.get(&pseudonym).cloned() else { return; };

    // Re-check the session straddling the awaits above: a mid-flight account swap
    // must not write this event into the new account's STATE/DB.
    if !session.is_valid() {
        return;
    }
    let outcome = {
        let mut state = crate::STATE.lock().await;
        vector_core::community::inbound::process_incoming(&mut state, &event, &channel, &my_pk)
    };
    let chat_id = channel.id.to_hex();
    match outcome {
        Some(vector_core::community::inbound::IncomingEvent::NewMessage(msg)) => {
            let _ = crate::db::save_message(&chat_id, &msg).await;
            vector_core::emit_event(
                "message_new",
                &serde_json::json!({ "message": &msg, "chat_id": &chat_id }),
            );
            // OS notification + badge for the realtime arrival (boot/catch-up sweeps don't notify).
            show_community_notification(&chat_id, &msg).await;
            if let Some(handle) = crate::TAURI_APP.get() {
                let _ = crate::commands::messaging::update_unread_counter(handle.clone()).await;
            }
        }
        Some(vector_core::community::inbound::IncomingEvent::Updated { target_id, message }) => {
            // Reaction or edit applied to an existing message → surgical UI update.
            let _ = crate::db::save_message(&chat_id, &message).await;
            vector_core::emit_event(
                "message_update",
                &serde_json::json!({ "old_id": &target_id, "message": &message, "chat_id": &chat_id }),
            );
        }
        Some(vector_core::community::inbound::IncomingEvent::Removed { target_id }) => {
            // Cooperative tombstone (3305) removed its target → drop locally + fade the row.
            let _ = crate::db::delete_event(&target_id).await;
            vector_core::emit_event(
                "message_removed",
                &serde_json::json!({ "id": &target_id, "chat_id": &chat_id, "reason": "deleted" }),
            );
        }
        Some(vector_core::community::inbound::IncomingEvent::Presence { npub, joined, event_id, created_at, invited_by, invited_label }) => {
            // Join/leave (3306) → a MemberJoined/MemberLeft system event (feeds the member list), with 
            // invite attribution when present.
            crate::commands::community::apply_community_presence(&chat_id, &npub, joined, &event_id, created_at, invited_by.as_deref(), invited_label.as_deref()).await;
        }
        Some(vector_core::community::inbound::IncomingEvent::WebxdcPeer { npub, topic_id, node_addr, event_id, created_at }) => {
            // WebXDC peer signal (3310) → same handling as the NIP-17 DM twin: persist for
            // rejoin discovery, feed the live gossip channel / lobby UI.
            match node_addr {
                Some(addr) => {
                    crate::services::event_handler::handle_webxdc_peer_advertisement(
                        &event_id, &topic_id, &addr, &npub, created_at, &chat_id,
                    ).await;
                }
                None => {
                    crate::services::event_handler::handle_webxdc_peer_left(
                        &event_id, &topic_id, &npub, created_at, &chat_id,
                    ).await;
                }
            }
        }
        Some(vector_core::community::inbound::IncomingEvent::Typing { npub, until }) => {
            // Typing indicator (3311) → feed the live typing tracker + emit the same `typing-update`
            // shape the DM path uses, so the (already group-aware) frontend renders it identically.
            let typers = {
                let mut state = crate::STATE.lock().await;
                state.update_typing_and_get_active(&chat_id, &npub, until)
            };
            vector_core::emit_event(
                "typing-update",
                &serde_json::json!({ "conversation_id": &chat_id, "typers": typers }),
            );
        }
        Some(vector_core::community::inbound::IncomingEvent::Kicked { community_id })
        | Some(vector_core::community::inbound::IncomingEvent::SelfLeft { community_id }) => {
            // self-removal (cooperative kick of us, or a leave another device authored) → wipe local
            // data but RETAIN the held epoch keys, then tell the UI. Received, not locally originated, so
            // tombstone local-only (boot's publish propagates). Idempotent on our own echoed leave.
            crate::commands::community::self_remove_from_community(&community_id, false).await;
        }
        None => {}
    }
}

/// Routes "straggler" community events — ones a slower relay returned after a racing
/// `LiveTransport::fetch` already handed the caller the fast relay's batch — back through the SAME
/// realtime ingest path. So a historical message, control edition, or rekey that only a slow relay
/// held is never lost; it's folded a beat late by the deterministic convergence engine (`process_incoming`
/// for content, `refresh_community_control` for authority — both via `handle_community_event`).
pub struct CommunityStragglerSink;

impl vector_core::community::transport::CommunityIngestSink for CommunityStragglerSink {
    fn ingest_stragglers(&self, events: Vec<Event>) {
        // Called from inside the transport's background drain task (always within the tokio runtime).
        // SessionGuard captured BEFORE the spawn boundary (a capture inside the task would validate
        // against whatever generation is current by then) — re-checked per event across the fold loop.
        let session = vector_core::state::SessionGuard::capture();
        tokio::spawn(async move {
            for event in events {
                if !session.is_valid() {
                    return;
                }
                handle_community_event(&session, event).await;
            }
        });
    }
}

/// OS notification for a realtime Community message, mirroring the DM/group rules: a normal message
/// notifies only when the channel isn't muted; a direct @mention, a reply to one of our own messages,
/// or an authorized @everyone (owner or admin) breaks through a muted channel — unless the SENDER's DM
/// is muted, they're blocked, or @everyone pings are globally disabled. `chat_id` is the channel id.
async fn show_community_notification(chat_id: &str, msg: &vector_core::Message) {
    if msg.mine { return; }
    let sender_npub = msg.npub.as_deref().unwrap_or_default();
    if sender_npub.is_empty() { return; }

    // Resolve @everyone authority only when the text actually contains it (zero-cost on normal sends).
    let everyone_ping = if msg.mentions_everyone() {
        let muted_everyone = vector_core::db::settings::get_sql_setting("notif_mute_everyone".to_string())
            .ok().flatten().map_or(false, |v| v == "true");
        !muted_everyone && community_sender_is_admin(chat_id, sender_npub)
    } else {
        false
    };

    // A reply to our own message is an implicit ping (same as a direct @mention). The inbound parse
    // doesn't resolve the reply's author, so check the target event's `mine` flag directly.
    let reply_ping = !msg.replied_to.is_empty()
        && vector_core::db::events::is_own_event(&msg.replied_to);

    let should_notify = {
        let state = crate::STATE.lock().await;
        let mentions_me = msg.mentions_me();
        let sender_blocked = state.get_profile(sender_npub).map_or(false, |p| p.flags.is_blocked());
        let sender_dm_muted = state.get_chat(sender_npub).map_or(false, |c| c.muted);
        if sender_blocked {
            false
        } else if mentions_me || reply_ping || everyone_ping {
            // Pings bypass a muted CHANNEL, but never a muted/blocked sender.
            !sender_dm_muted
        } else {
            state.get_chat(chat_id).map_or(false, |c| !c.muted)
        }
    };
    if !should_notify { return; }

    let is_file = !msg.attachments.is_empty();
    let (sender_name, community_name, avatar, content) = {
        let state = crate::STATE.lock().await;
        let (sender, av) = state.get_profile(sender_npub).map(|p| {
            let name = if !p.nickname.is_empty() { p.nickname.to_string() }
                else if !p.name.is_empty() { p.name.to_string() }
                else { "Someone".to_string() };
            let cached = if !p.avatar_cached.is_empty() { Some(p.avatar_cached.to_string()) } else { None };
            (name, cached)
        }).unwrap_or_else(|| ("Someone".to_string(), None));
        let community_name = state.get_chat(chat_id)
            .and_then(|c| c.metadata.get_name().map(|n| n.to_string()))
            .unwrap_or_else(|| "Community".to_string());
        let content = if is_file {
            let ext = msg.attachments.first().map(|a| a.extension.clone()).unwrap_or_else(|| "file".into());
            "Sent a ".to_string() + &crate::util::get_file_type_description(&ext)
        } else {
            crate::services::strip_content_for_preview(
                &crate::services::resolve_mention_display_names(&msg.content, &state)
            )
        };
        (sender, community_name, av, content)
    };

    // Community icon for the Android embedded design (sender + community + both avatars). Fast
    // cached-path lookup only (no network) — resolves once the channel's been opened + icon cached.
    let community_avatar = crate::TAURI_APP.get().and_then(|handle| {
        vector_core::db::community::community_id_for_channel(chat_id)
            .ok()
            .flatten()
            .and_then(|cid| {
                let id = vector_core::community::CommunityId(vector_core::simd::hex::hex_to_bytes_32(&cid));
                vector_core::db::community::load_community(&id).ok().flatten()
            })
            .and_then(|c| c.icon)
            .and_then(|icon| crate::image_cache::get_cached_path(handle, &icon.url, crate::image_cache::ImageType::Avatar))
    });

    let notification = crate::services::NotificationData::community_message(
        sender_name, community_name, content, avatar, community_avatar, chat_id.to_string(),
    );
    crate::services::show_notification_generic(notification);
}

/// Whether `sender_npub` (bech32) is the owner or an admin of the community owning `channel_id`.
/// Used only for @everyone authority; a lookup failure denies the bypass (fail-closed).
fn community_sender_is_admin(channel_id: &str, sender_npub: &str) -> bool {
    let Ok(sender_hex) = nostr_sdk::PublicKey::from_bech32(sender_npub).map(|pk| pk.to_hex()) else {
        return false;
    };
    let Ok(Some(community_id)) = vector_core::db::community::community_id_for_channel(channel_id) else {
        return false;
    };
    // Owner (verified attestation) outranks all.
    let owner_is_sender = vector_core::db::community::load_community(
        &vector_core::community::CommunityId(vector_core::simd::hex::hex_to_bytes_32(&community_id)),
    )
    .ok()
    .flatten()
    .and_then(|c| {
        c.owner_attestation
            .as_ref()
            .and_then(|att| vector_core::community::owner::verify_owner_attestation(att, &community_id))
    })
    .map_or(false, |pk| pk.to_hex() == sender_hex);
    if owner_is_sender {
        return true;
    }
    // Otherwise a non-owner admin grant-holder.
    vector_core::db::community::get_community_roles(&community_id)
        .map(|roles| roles.is_admin(&sender_hex))
        .unwrap_or(false)
}

/// Called once after login to begin receiving real-time events.
///
/// Uses vector-core's `subscribe_dms()` for the GiftWrap subscription,
/// then layers on the Community (kind-3300) subscription.
pub(crate) async fn start_subscriptions() -> Result<bool, String> {
    let client = nostr_client().ok_or("Nostr client not initialized")?;
    // Session captured at subscription start; every notification short-
    // circuits on swap so account A's inbound events don't persist into
    // account B's DB.
    let session = vector_core::state::SessionGuard::capture();

    // GiftWrap subscription via vector-core (DMs, files)
    let core = vector_core::VectorCore;
    let gift_sub_id = core.subscribe_dms().await.map_err(|e| e.to_string())?;

    // Community (kind-3300) subscription — scoped to our channels' epoch pseudonyms.
    refresh_community_subscription().await;

    // Self-sync subscription — our own replaceable settings lists (Community List + emoji list). Covers
    // boot, reconnect, AND instant cross-device in one open subscription.
    subscribe_self_sync().await;

    // Notification loop: dispatch GiftWraps through Tauri's event handler,
    // Community messages through the Community handler.
    match client
        .handle_notifications(|notification| async {
            // If the session has been swapped out from under us, exit the
            // notification loop. Returning Ok(true) tells nostr-sdk to break.
            if !session.is_valid() { return Ok(true); }
            if let RelayPoolNotification::Event { event, subscription_id, .. } = notification {
                if subscription_id == gift_sub_id {
                    // DMs/files/reactions/edits (via tauri_commit_prepared_event)
                    super::handle_event(*event, true).await;
                } else if COMMUNITY_SUB_ID.lock().await.as_ref() == Some(&subscription_id) {
                    handle_community_event(&session, *event).await;
                } else if SELFSYNC_SUB_IDS.lock().await.contains(&subscription_id) {
                    handle_self_sync_event(&session, *event).await;
                }
            }
            Ok(false)
        })
        .await
    {
        Ok(_) => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}