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
            vector_core::pinned_chats::PINNED_D_TAG.to_string(),
            vector_core::synced_prefs::BLOCKS_D_TAG.to_string(),
            vector_core::synced_prefs::MUTES_D_TAG.to_string(),
            vector_core::synced_prefs::NICKNAMES_D_TAG.to_string(),
        ]);
    match client.subscribe(self_lists_filter).await {
        Ok(out) => new_ids.push(out.value),
        Err(e) => eprintln!("[self-sync] self-lists subscribe failed: {:?}", e),
    }
    // v2 Community List (addressable kind 33302, CORD-02 §8 — one event per fragment).
    // Its own kind, so it needs its own filter — v1's list is a d-tagged 30078 above.
    // Without this a v2 join/leave only reached the other devices on their next BOOT,
    // while v1 had been instant since it shipped.
    let v2_list_filter = Filter::new()
        .author(my_pk)
        .kind(Kind::Custom(vector_core::community::v2::kind::COMMUNITY_LIST_FRAG));
    match client.subscribe(v2_list_filter).await {
        Ok(out) => new_ids.push(out.value),
        Err(e) => eprintln!("[self-sync] v2 community-list subscribe failed: {:?}", e),
    }
    // Emoji-pack List (replaceable kind 10030).
    let emoji_filter = Filter::new().author(my_pk).kind(Kind::Custom(10030));
    match client.subscribe(emoji_filter).await {
        Ok(out) => new_ids.push(out.value),
        Err(e) => eprintln!("[self-sync] emoji-list subscribe failed: {:?}", e),
    }

    let displaced = {
        let mut ids = SELFSYNC_SUB_IDS.lock().await;
        std::mem::replace(&mut *ids, new_ids)
    };
    for id in displaced {
        let _ = client.unsubscribe(&id).await;
    }
}

/// Route an arriving self-sync list event (our own replaceable settings): a Community List update folds +
/// rehydrates (so a join on another device appears live); an emoji-list update refreshes the pack set.
/// Spawned off the notification loop — both run several relay fetches and must not head-of-line-block it.
async fn handle_self_sync_event(event: Event) {
    // Per-list dedup key: kind plus `d` where there is one, since several distinct lists ride a
    // single kind (30078) and a fragmented list rides several `d`s of its own. Coalesces
    // multi-relay re-delivery of the SAME event so one update = one sweep.
    let dedup_key = match event.tags.identifier() {
        Some(d) => format!("{}:{}", event.kind.as_u16(), d),
        None => event.kind.as_u16().to_string(),
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
            // Several lists share kind 30078 — route by `d`-tag, and match each
            // explicitly: an unrecognised tag must fall through, not land on
            // whichever ingest happens to be the else-branch.
            let d = event.tags.identifier().unwrap_or_default().to_string();
            vector_core::db::spawn_bound(async move {
                if d == vector_core::community::invite_list::INVITE_LIST_D_TAG {
                    crate::commands::community::ingest_invite_list_update(event).await;
                } else if d == vector_core::pinned_chats::PINNED_D_TAG {
                    crate::commands::pinned::ingest_pinned_chats_update(event).await;
                } else if vector_core::synced_prefs::Pref::from_d_tag(&d).is_some() {
                    crate::commands::prefs::ingest_prefs_update(event).await;
                } else if d == vector_core::community::list::COMMUNITY_LIST_D_TAG {
                    crate::commands::community::ingest_community_list_update(event).await;
                }
            });
        }
        k if k == vector_core::community::v2::kind::COMMUNITY_LIST_FRAG => {
            // Re-run the same sync the boot path uses: it adopts newly-listed communities and
            // tears down ones a sibling device left. Spawned — it runs several relay fetches.
            vector_core::db::spawn_bound(async move {
                crate::commands::community::ingest_v2_community_list_update().await;
            });
        }
        10030 => {
            vector_core::db::spawn_bound(async move {
                let _ = vector_core::emoji_packs::refresh_subscribed_packs().await;
            });
        }
        _ => {}
    }
}

/// Route an arriving Community (kind-3300) event: find the channel its `z` pseudonym
/// maps to, open + verify + ingest it into STATE, then persist + emit if it is new.
/// Events that fail to open (wrong key, splice, forged sig) are dropped inside
/// `process_incoming`. (The notification loop's `session.is_live()` gate above guards
/// against account-swap before dispatch.)
/// Route an arriving Community event through `vector_core::community::realtime`, which opens +
/// verifies + ingests + persists it and dispatches the typed outcome to the Tauri handler (UI +
/// notifications + presence/teardown). Thin wrapper — the realtime pipeline now lives in core.
async fn handle_community_event(
    event: Event,
) {
    let handler: std::sync::Arc<dyn vector_core::InboundEventHandler> =
        std::sync::Arc::new(super::event_handler::TauriEventHandler);
    vector_core::community::realtime::dispatch_event(event, handler).await;
}

/// v2 twin of [`handle_community_event`]: the same Tauri handler surface fed by
/// the v2 dispatcher (authors-addressed 1059/21059 wraps → open → route →
/// persist-gated callbacks), so a v2 message emits to the frontend identically
/// to a v1 one.
async fn handle_community_v2_event(
    event: Event,
) {
    let handler: std::sync::Arc<dyn vector_core::InboundEventHandler> =
        std::sync::Arc::new(super::event_handler::TauriEventHandler);
    vector_core::community::v2::realtime::dispatch_event(event, handler).await;
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
        // std::sync::Arc<crate::db::Session> captured BEFORE the spawn boundary (a capture inside the task would validate
        // against whatever generation is current by then) — re-checked per event across the fold loop.
        vector_core::db::spawn_bound(async move {
            for event in events {
                handle_community_event(event).await;
            }
        });
    }
}

/// OS notification for a realtime Community message, mirroring the DM/group rules: a normal message
/// notifies only when neither the channel nor the sender is muted; a direct @mention, a reply to one of our own messages,
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
        // Only a community's surfaced (primary) row notifies. Every channel is registered
        // and synced, but there is no row to open for a sibling channel, so ringing for
        // one would be a notification the user can't act on or clear.
        let registered = state
            .get_chat(chat_id)
            .is_some_and(|c| c.is_surfaced_community_channel());
        let mentions_me = msg.mentions_me();
        let sender_blocked = state.get_profile(sender_npub).map_or(false, |p| p.flags.is_blocked());
        let sender_dm_muted = state.get_chat(sender_npub).map_or(false, |c| c.muted);
        if !registered || sender_blocked {
            false
        } else if mentions_me || reply_ping || everyone_ping {
            // Pings bypass a muted CHANNEL, but never a muted/blocked sender.
            !sender_dm_muted
        } else {
            // A muted SENDER is silent in every channel, muted or not.
            state.get_chat(chat_id).map_or(false, |c| !c.muted) && !sender_dm_muted
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
    let Ok(sender_hex) = nostr_sdk::prelude::PublicKey::from_bech32(sender_npub).map(|pk| pk.to_hex()) else {
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
/// The boot sweep waits on this gate so the LIVE pipe wins the relay race.
///
/// At boot three things once hit the relays at the same moment: the community
/// sweep (~107 channels x ~4 REQs), DM negentropy, and the live subscriptions —
/// and the subscriptions routinely lost. Events published while they were losing
/// belonged to nobody: too new for the sweep's snapshot, too old for a sub that
/// wasn't up yet. The observable result was minutes of a booted, synced app
/// receiving nothing, then everything at once when the reconnect path fired.
static SUBS_COMMITTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SUBS_NOTIFY: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();

fn subs_notify() -> &'static tokio::sync::Notify {
    SUBS_NOTIFY.get_or_init(tokio::sync::Notify::new)
}

/// A fresh login's sweep must wait on THIS login's subscriptions, not the last
/// account's. Called at boot-init before the sweep is spawned.
pub(crate) fn reset_subs_gate() {
    SUBS_COMMITTED.store(false, std::sync::atomic::Ordering::Release);
}

/// Wait until the live subscriptions have committed, or the cap elapses. The cap
/// exists so a wedged subscribe can never hold the whole boot sync hostage —
/// late history beats no history.
pub(crate) async fn await_subs_committed(cap: std::time::Duration) {
    if SUBS_COMMITTED.load(std::sync::atomic::Ordering::Acquire) {
        return;
    }
    let _ = tokio::time::timeout(cap, async {
        loop {
            let notified = subs_notify().notified();
            if SUBS_COMMITTED.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    })
    .await;
}

/// The live DM (gift-wrap) subscription's current id, so it can be REPLACED.
///
/// Routing no longer keys on this id (DM wraps route by kind + p-tag below), so
/// re-subscribing under a fresh id after the boot flood is safe — this exists
/// only to unsubscribe the old one instead of stacking duplicates.
static DM_SUB_ID: std::sync::Mutex<Option<nostr_sdk::prelude::SubscriptionId>> = std::sync::Mutex::new(None);

/// Drop and re-create the gift-wrap subscription. A relay under the boot
/// sweep's load can drop the boot-time sub, and DMs then stay dead until a
/// lucky reconnect: community subs had a post-flood re-assert, DMs had none.
/// Subscribe to gift wraps, SAYING which relays accepted. `client.subscribe`
/// reports per-relay success/failure and `subscribe_dms` discards it — which is
/// how a DM sub that every gated relay CLOSEd (the REQ racing the NIP-42
/// handshake) could look identical to one that worked.
async fn subscribe_dms_verbose(label: &str) -> Result<nostr_sdk::prelude::SubscriptionId, String> {
    use nostr_sdk::prelude::*;
    let client = nostr_client().ok_or("no client")?;
    let me = vector_core::state::my_public_key().ok_or("not logged in")?;
    let filter = Filter::new().pubkey(me).kind(Kind::GiftWrap).limit(0);
    let output = client.subscribe(filter).await.map_err(|e| e.to_string())?;
    println!(
        "[dm-sub] {label}: id {} — ok on {:?}, failed on {:?}",
        *output,
        output.success.iter().map(|(r, _)| r.to_string()).collect::<Vec<_>>(),
        output.failed.iter().map(|(r, e)| format!("{r}: {e:?}")).collect::<Vec<_>>()
    );
    Ok(output.value)
}

pub(crate) async fn reassert_dm_sub() {
    let Some(client) = nostr_client() else { return };
    let old = DM_SUB_ID.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(id) = old {
        let _ = client.unsubscribe(&id).await;
    }
    match subscribe_dms_verbose("post-sweep").await {
        Ok(id) => {
            *DM_SUB_ID.lock().unwrap_or_else(|e| e.into_inner()) = Some(id);
            println!("[Boot] DM sub re-asserted after sweep");
        }
        Err(e) => eprintln!("[Boot] DM sub re-assert failed: {e}"),
    }
}

pub(crate) async fn start_subscriptions() -> Result<bool, String> {
    // Stage timing, INFO on purpose: the live pipe going up is the moment the
    // app stops being deaf, and a stage stalling here reads from the outside as
    // "everything synced but nothing arrives" with no line saying why.
    let t0 = std::time::Instant::now();
    macro_rules! stage {
        ($name:expr) => {
            // println!, not log_info!: the default runtime level is WARN, so an
            // info line here is invisible in exactly the situation it exists for.
            println!("[subs] {} at +{}ms", $name, t0.elapsed().as_millis())
        };
    }
    let client = nostr_client().ok_or("Nostr client not initialized")?;
    // Session captured at subscription start; every notification short-
    // circuits on swap so account A's inbound events don't persist into
    // account B's DB.

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
    stage!("dm subscribe: begin");
    let gift_sub_id = subscribe_dms_verbose("boot").await?;
    let _ = &core;
    *DM_SUB_ID.lock().unwrap_or_else(|e| e.into_inner()) = Some(gift_sub_id.clone());
    stage!("dm subscribe: LIVE");

    // Community (kind-3300) subscription — scoped to our channels' epoch pseudonyms.
    refresh_community_subscription().await;
    stage!("v1 community sub: LIVE");

    // v2 plane subscription (authors-addressed wraps) + boot catch-up: enqueue a
    // refold per held v2 community so anything missed offline (rotations, control
    // edits, messages) folds in — coalesced, drained by the worker off this path.
    vector_core::community::v2::realtime::refresh_subscription(&client).await;
    stage!("v2 plane sub: LIVE");
    for c in vector_core::community::v2::realtime::load_held_v2() {
        vector_core::community::v2::realtime::enqueue_follow(c.id());
    }

    // The three delivery subscriptions are up: open the gate HERE, before the
    // self-sync sub and prefs hydration below — those are their own slow network
    // and the sweep must not wait on them.
    SUBS_COMMITTED.store(true, std::sync::atomic::Ordering::Release);
    subs_notify().notify_waiters();
    stage!("live pipe COMMITTED");

    // Self-sync subscription — our own replaceable settings lists (Community List + emoji list). Covers
    // boot, reconnect, AND instant cross-device in one open subscription.
    subscribe_self_sync().await;

    // Reconcile blocks/mutes/nicknames BEFORE the user can change them. The
    // subscription above delivers them too, but it races the user: acting in
    // that window would publish this device's emptier view over another
    // device's prefs. Until this lands, those lists are unpublishable.
    crate::commands::prefs::hydrate_prefs().await;

    // v2 reconnect catch-up: a `limit(0)` sub never replays what a relay missed
    // while down, so each Connected transition enqueues a refold + re-tracks the
    // subs at the current epochs (debounced across a reconnect burst). v1 leans
    // on open-sub replay; v2's consensus planes need the explicit fold.
    if let Some(monitor) = client.monitor() {
        let mut rx = monitor.subscribe();
        vector_core::db::spawn_bound(async move {
            let mut last: Option<std::time::Instant> = None;
            while let Ok(n) = rx.recv().await {
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
    // 0.45 removed `handle_notifications`; drive the stream directly. It ends when
    // the client shuts down, and the session check still breaks out on a swap.
    // DMs get their OWN consumer. `notifications()` is a broadcast, so each
    // receiver sees every event — and the main loop below awaits community
    // handling INLINE, which at boot means hundreds of fold/DB awaits queued in
    // front of whatever arrives next. Gift wraps sat in that queue for minutes
    // (measured: published-to-relay vs reaching the loop, 20s-2min, in pulses;
    // handled in ~3ms once dequeued). A DM's handler is fast, so a dedicated
    // lane makes DM latency independent of community-event handling entirely.
    {
        let mut dm_notifications = client.notifications();
        vector_core::db::spawn_bound(async move {
            while let Some(n) = dm_notifications.next().await {
                if let ClientNotification::Event { event, .. } = n {
                    let is_dm = event.kind.as_u16() == 1059
                        && vector_core::state::my_public_key()
                            .is_some_and(|me| event.tags.public_keys().any(|pk| pk == me));
                    if is_dm {
                        super::handle_event(*event, true).await;
                    }
                }
            }
        });
    }

    let mut notifications = client.notifications();
    while let Some(notification) = notifications.next().await {
        {
            match notification {
                ClientNotification::Event { event, subscription_id, .. } => {
                    let _ = &subscription_id;
                    let k = event.kind.as_u16();
                    // DM gift wraps route by KIND + the p-tag addressing us — the
                    // same lesson the community branch below already carries.
                    // Keyed on the boot-time sub id, the DM path was welded to
                    // one subscription: if a relay dropped it under the boot
                    // flood, a replacement sub's events fell through to the v2
                    // route, which discards DM wraps as NotOurs — DMs stayed
                    // dead until a restart. v2 plane wraps are group-addressed
                    // (no p-tag to us), so the tag is the discriminator, and
                    // the wrapper ledger dedups a wrap that arrives on several
                    // subscriptions at once.
                    let dm_wrap = (k == 1059)
                        && vector_core::state::my_public_key()
                            .is_some_and(|me| event.tags.public_keys().any(|pk| pk == me));
                    if dm_wrap {
                        // The dedicated DM lane above handles these; handling
                        // here too would race it to the dedup ledger for every
                        // wrap. This loop's job is everything else.
                        continue;
                    } else if (3300..=3311).contains(&k) {
                        // Route Community events by KIND, not by subscription id: an event can arrive on the
                        // live community sub OR on a fetch/sync/reconcile sub, so matching only the live sub
                        // id would drop the rest. dispatch_event resolves the channel by the event's
                        // z-pseudonym, and process_incoming dedups by outer-event id, so handling every
                        // community event the pool surfaces is correct and idempotent.
                        handle_community_event(*event).await;
                    } else if k == 1059 || k == 21059 {
                        // v2 wraps (plane-key authors). DM gift wraps matched the gift sub above;
                        // any other wrap-kind event tries the v2 route — the dispatcher dedups by
                        // wrap id and drops NotOurs (e.g. a stray DM copy on another sub) for free.
                        handle_community_v2_event(*event).await;
                    } else if SELFSYNC_SUB_IDS.lock().await.contains(&subscription_id) {
                        handle_self_sync_event(*event).await;
                    }
                }
                ClientNotification::Message { message, .. } => {
                    // Relay OKs feed the send pipeline: an OK that outlives
                    // the per-attempt wait still confirms delivery, and can
                    // rescue a message already marked Failed.
                    if let nostr_sdk::prelude::RelayMessage::Ok { event_id, status, .. } = *message {
                        vector_core::sending::note_relay_ok(&event_id, status);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(true)
}