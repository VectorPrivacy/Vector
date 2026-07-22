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

// The Community subscription id + route maps now live in `vector_core::community::realtime`.

/// Self-sync subscription ids: our OWN replaceable "settings" lists (the cross-device Community List 30078,
/// and the emoji-pack List 10030). One OPEN sub per filter (no `limit(0)` — these are replaceable, so the
/// relay replays the latest stored at connect = boot/reconnect sync, AND streams every later edit = instant
/// cross-device). A join/leave/pack-change on one device lands on the others with no reboot.
pub(crate) static SELFSYNC_SUB_IDS: LazyLock<Mutex<Vec<SubscriptionId>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Last self-sync event id processed per kind. A replaceable event stored on N relays is delivered N times
/// with the SAME id; without this every copy would kick a full ingest/rehydrate sweep (N× the work). A
/// genuine update has a new id and passes through.
// Keyed by a per-list string (the `d`-tag for kind-30078 lists, else the kind) so the Community List and
// Invite List — both kind 30078 — don't share a dedup slot and clobber each other's last-id.
static SELFSYNC_LAST_EVENT: LazyLock<Mutex<HashMap<String, EventId>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// `rebuild_community_routes` + `refresh_community_subscription` route state now lives in
// `vector_core::community::realtime`; `refresh_community_subscription` below stays as a thin wrapper
// for the call sites that trigger a resubscribe (join/leave/ban/etc.).

/// Rebuild the Community subscription: scope it to the epoch pseudonyms of every
/// channel in every Community we hold, and rebuild the pseudonym→channel routing
/// table. Called at boot and whenever Communities/channels change.
pub(crate) async fn refresh_community_subscription() {
    if let Some(client) = nostr_client() {
        vector_core::community::realtime::refresh_subscription(&client).await;
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
    // Community List + Invite List — both parameterized-replaceable kind-30078, d-tag scoped so they never
    // alias a wallpaper/badge 30078. One filter (both d-tags) keeps the live sub as wire-efficient as boot.
    let self_lists_filter = Filter::new()
        .author(my_pk)
        .kind(Kind::Custom(vector_core::stored_event::event_kind::APPLICATION_SPECIFIC))
        .identifiers([
            vector_core::community::list::COMMUNITY_LIST_D_TAG.to_string(),
            vector_core::community::invite_list::INVITE_LIST_D_TAG.to_string(),
        ]);
    match client.subscribe(self_lists_filter, None).await {
        Ok(out) => new_ids.push(out.val),
        Err(e) => eprintln!("[self-sync] self-lists subscribe failed: {:?}", e),
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
    // Per-list dedup key: the `d`-tag for kind-30078 lists (Community vs Invite share the kind), else the
    // kind. Coalesces multi-relay re-delivery of the SAME replaceable event so one update = one sweep.
    let dedup_key = if event.kind.as_u16() == vector_core::stored_event::event_kind::APPLICATION_SPECIFIC {
        event.tags.identifier().unwrap_or_default().to_string()
    } else {
        event.kind.as_u16().to_string()
    };
    {
        let mut last = SELFSYNC_LAST_EVENT.lock().await;
        if last.get(&dedup_key) == Some(&event.id) {
            return;
        }
        last.insert(dedup_key, event.id);
    }
    match event.kind.as_u16() {
        k if k == vector_core::stored_event::event_kind::APPLICATION_SPECIFIC => {
            // Both lists are kind 30078 — route by `d`-tag.
            let is_invite = event.tags.identifier()
                == Some(vector_core::community::invite_list::INVITE_LIST_D_TAG);
            tokio::spawn(async move {
                if is_invite {
                    crate::commands::community::ingest_invite_list_update(event).await;
                } else {
                    crate::commands::community::ingest_community_list_update(event).await;
                }
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
/// Route an arriving Community event through `vector_core::community::realtime`, which opens +
/// verifies + ingests + persists it and dispatches the typed outcome to the Tauri handler (UI +
/// notifications + presence/teardown). Thin wrapper — the realtime pipeline now lives in core.
async fn handle_community_event(
    session: &vector_core::state::SessionGuard,
    event: Event,
) {
    let handler: std::sync::Arc<dyn vector_core::InboundEventHandler> =
        std::sync::Arc::new(super::event_handler::TauriEventHandler);
    vector_core::community::realtime::dispatch_event(session, event, handler).await;
}

/// v2 twin of [`handle_community_event`]: the same Tauri handler surface fed by
/// the v2 dispatcher (authors-addressed 1059/21059 wraps → open → route →
/// persist-gated callbacks), so a v2 message emits to the frontend identically
/// to a v1 one.
async fn handle_community_v2_event(
    session: &vector_core::state::SessionGuard,
    event: Event,
) {
    let handler: std::sync::Arc<dyn vector_core::InboundEventHandler> =
        std::sync::Arc::new(super::event_handler::TauriEventHandler);
    vector_core::community::v2::realtime::dispatch_event(session, event, handler).await;
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
pub(crate) async fn show_community_notification(chat_id: &str, msg: &vector_core::Message) {
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
        // Only a community's surfaced (primary) row notifies — sibling-channel rows
        // are bare persistence anchors carrying no community metadata.
        let registered = state
            .get_chat(chat_id)
            .is_some_and(|c| c.metadata.custom_fields.contains_key("community_id"));
        let mentions_me = msg.mentions_me();
        let sender_blocked = state.get_profile(sender_npub).map_or(false, |p| p.flags.is_blocked());
        let sender_dm_muted = state.get_chat(sender_npub).map_or(false, |c| c.muted);
        if !registered || sender_blocked {
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
            let name = if !p.nickname().is_empty() { p.nickname().to_string() }
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

    // Backstop: reap retained resend bodies for messages left red and untouched
    // past a week, so a pile of never-retried failures can't grow unbounded (the
    // NIP-09 key row survives; only the ~1-2 KB republish blob is nulled).
    const RESEND_BODY_TTL_SECS: i64 = 7 * 24 * 60 * 60;
    let _ = vector_core::db::nip17_keys::prune_stale_resend_payloads(RESEND_BODY_TTL_SECS);

    // v2 stream-AUTH responder BEFORE any subscription: a gating relay issues ONE
    // NIP-42 challenge per connection and the DM subscribe below consumes it via
    // the user auto-auth — the responder must witness (and remember) it, or
    // stream keys registered later can never authenticate and the v2 sub dies
    // silently on gated relays.
    vector_core::community::v2::streamauth::ensure_responder(&client);
    // The single v2 follow worker (control/rekey refolds) — same Tauri handler
    // surface as live dispatch, so a refold emits to the frontend identically.
    vector_core::community::v2::realtime::spawn_follow_worker(std::sync::Arc::new(
        super::event_handler::TauriEventHandler,
    ));

    // GiftWrap subscription via vector-core (DMs, files)
    let core = vector_core::VectorCore;
    let gift_sub_id = core.subscribe_dms().await.map_err(|e| e.to_string())?;

    // Community (kind-3300) subscription — scoped to our channels' epoch pseudonyms.
    refresh_community_subscription().await;

    // v2 plane subscription (authors-addressed wraps) + boot catch-up: enqueue a
    // refold per held v2 community so anything missed offline (rotations, control
    // edits, messages) folds in — coalesced, drained by the worker off this path.
    vector_core::community::v2::realtime::refresh_subscription(&client).await;
    for c in vector_core::community::v2::realtime::load_held_v2() {
        vector_core::community::v2::realtime::enqueue_follow(c.id());
    }

    // Self-sync subscription — our own replaceable settings lists (Community List + emoji list). Covers
    // boot, reconnect, AND instant cross-device in one open subscription.
    subscribe_self_sync().await;

    // v2 reconnect catch-up: a `limit(0)` sub never replays what a relay missed
    // while down, so each Connected transition enqueues a refold + re-tracks the
    // subs at the current epochs (debounced across a reconnect burst). v1 leans
    // on open-sub replay; v2's consensus planes need the explicit fold.
    if let Some(monitor) = client.monitor() {
        let mut rx = monitor.subscribe();
        let monitor_session = vector_core::state::SessionGuard::capture();
        tokio::spawn(async move {
            let mut last: Option<std::time::Instant> = None;
            while let Ok(n) = rx.recv().await {
                if !monitor_session.is_valid() {
                    return;
                }
                let MonitorNotification::StatusChanged { status, .. } = n;
                if status == RelayStatus::Connected {
                    if last.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(3)) {
                        continue;
                    }
                    for c in vector_core::community::v2::realtime::load_held_v2() {
                        vector_core::community::v2::realtime::enqueue_follow(c.id());
                    }
                    if let Some(c) = crate::nostr_client() {
                        vector_core::community::v2::realtime::refresh_subscription(&c).await;
                    }
                    last = Some(std::time::Instant::now());
                }
            }
        });
    }

    // Notification loop: dispatch GiftWraps through Tauri's event handler,
    // Community messages through the Community handler.
    match client
        .handle_notifications(|notification| async {
            // If the session has been swapped out from under us, exit the
            // notification loop. Returning Ok(true) tells nostr-sdk to break.
            if !session.is_valid() { return Ok(true); }
            match notification {
                RelayPoolNotification::Event { event, subscription_id, .. } => {
                    let k = event.kind.as_u16();
                    if subscription_id == gift_sub_id {
                        // DMs/files/reactions/edits (via tauri_commit_prepared_event)
                        super::handle_event(*event, true).await;
                    } else if (3300..=3311).contains(&k) {
                        // Route Community events by KIND, not by subscription id: an event can arrive on the
                        // live community sub OR on a fetch/sync/reconcile sub, so matching only the live sub
                        // id would drop the rest. dispatch_event resolves the channel by the event's
                        // z-pseudonym, and process_incoming dedups by outer-event id, so handling every
                        // community event the pool surfaces is correct and idempotent.
                        handle_community_event(&session, *event).await;
                    } else if k == 1059 || k == 21059 {
                        // v2 wraps (plane-key authors). DM gift wraps matched the gift sub above;
                        // any other wrap-kind event tries the v2 route — the dispatcher dedups by
                        // wrap id and drops NotOurs (e.g. a stray DM copy on another sub) for free.
                        handle_community_v2_event(&session, *event).await;
                    } else if SELFSYNC_SUB_IDS.lock().await.contains(&subscription_id) {
                        handle_self_sync_event(&session, *event).await;
                    }
                }
                RelayPoolNotification::Message { message, .. } => {
                    // Relay OKs feed the send pipeline: an OK that outlives
                    // the per-attempt wait still confirms delivery, and can
                    // rescue a message already marked Failed.
                    if let nostr_sdk::RelayMessage::Ok { event_id, status, .. } = message {
                        vector_core::sending::note_relay_ok(&event_id, status);
                    }
                }
                _ => {}
            }
            Ok(false)
        })
        .await
    {
        Ok(_) => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}