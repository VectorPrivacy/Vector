//! Realtime Community (Concord) subscription, routing, dispatch, and control-follow.
//!
//! The per-event cryptographic engine ([`inbound::process_incoming`]) already lives in core;
//! this module is the realtime *plumbing* around it — the relay subscription, the pseudonym→channel
//! route maps, the dispatch of typed [`inbound::IncomingEvent`]s to an [`InboundEventHandler`], and
//! the control/rekey realtime-follow. It is consumed by [`crate::VectorCore::listen`] so headless
//! clients (SDK, CLI, bots) get realtime Community delivery through the same handler as DMs.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex as StdMutex};
use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::Mutex;

use crate::community::{derive, inbound, roster, service, Channel, CommunityId, Epoch};
use crate::community::transport::LiveTransport;
use crate::event_handler::InboundEventHandler;
use crate::state::SessionGuard;
use crate::stored_event::event_kind;

/// Realtime channel-follow retry budget: a re-founding publishes the channel rekey under the NEW
/// root right after the base rekey, so the base-3303-triggered follow can race its propagation.
const CHANNEL_FOLLOW_MAX_ATTEMPTS: usize = 5;
const CHANNEL_FOLLOW_BACKOFF_MS: u64 = 700;

/// Current Community subscription id — single subscription scoped to the epoch pseudonyms of
/// every channel we hold; refreshed on join/leave/rekey.
static COMMUNITY_SUB_ID: LazyLock<Mutex<Option<SubscriptionId>>> = LazyLock::new(|| Mutex::new(None));

/// Sorted pseudonym set of the CURRENTLY live Community subscription. Lets `refresh_subscription` skip
/// a redundant unsubscribe+resubscribe when nothing changed (e.g. a metadata/avatar edit, which fires
/// a control re-fold but doesn't alter the pseudonym set). The relay pool auto-re-applies the existing
/// subscription on every reconnect, so leaving it in place is strictly more reliable than rebuilding it
/// (a rebuild that lands mid-reconnect silently fails to register and kills realtime delivery).
static COMMUNITY_SUB_SET: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Pool-wide `subscribe` of the community filter, opened alongside the targeted `subscribe_to`. On
/// Android the targeted sub registers but never streams; the pool-wide one does (it rides the same
/// auto-managed path the DM sub uses). On desktop the targeted sub streams. We keep BOTH so every
/// platform gets live delivery; `process_incoming` dedups by outer-event id, so an event seen on both
/// folds exactly once.
static COMMUNITY_POOLWIDE_SUB_ID: LazyLock<Mutex<Option<SubscriptionId>>> = LazyLock::new(|| Mutex::new(None));

/// `z` pseudonym (hex) → the Channel it belongs to. Lets the notification loop open an arriving
/// event against the right channel key (and read its `banned` set for the inbound drop-filter).
static COMMUNITY_ROUTES: LazyLock<Mutex<HashMap<String, Channel>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Control-plane `z` pseudonym (hex) → community id (hex). Routes a CONTROL edition (3308) or a
/// base/channel REKEY coordinate (3303) to a realtime control refresh of that community.
static CONTROL_ROUTES: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Per-community in-flight guard for [`refresh_control`] — one concurrent fold per community.
static REFRESH_CONTROL_INFLIGHT: LazyLock<StdMutex<HashSet<String>>> =
    LazyLock::new(|| StdMutex::new(HashSet::new()));

/// The subscription id of the live Community subscription, if any. Lets a notification loop test
/// whether an arriving event belongs to the Community sub.
pub async fn subscription_id() -> Option<SubscriptionId> {
    COMMUNITY_SUB_ID.lock().await.clone()
}

/// Clear all realtime route/subscription state. Call from `swap_session` so a swapped-in account
/// can't read the prior account's channel keys / banned sets.
pub async fn clear() {
    *COMMUNITY_SUB_ID.lock().await = None;
    *COMMUNITY_POOLWIDE_SUB_ID.lock().await = None;
    COMMUNITY_SUB_SET.lock().await.clear();
    COMMUNITY_ROUTES.lock().await.clear();
    CONTROL_ROUTES.lock().await.clear();
    REFRESH_CONTROL_INFLIGHT.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Rebuild ONLY the in-memory route maps from the persisted communities, WITHOUT touching the live
/// relay subscription. Refreshes each cached `Channel` (incl. its `banned` set) so a received
/// ban/unban takes live effect without a full resubscribe. Returns the pseudonyms + relays a caller
/// also resubscribing needs.
pub async fn rebuild_routes() -> (Vec<String>, HashSet<String>) {
    let mut routes: HashMap<String, Channel> = HashMap::new();
    let mut control_routes: HashMap<String, String> = HashMap::new();
    let mut pseudonyms: Vec<String> = Vec::new();
    let mut relays: HashSet<String> = HashSet::new();

    if let Ok(ids) = crate::db::community::list_community_ids() {
        for id in ids {
            if let Ok(Some(community)) = crate::db::community::load_community(&id) {
                for r in &community.relays {
                    relays.insert(r.clone());
                }
                for ch in &community.channels {
                    // Subscribe to EVERY held epoch pseudonym (not just the head) so a straggler posting
                    // under a retained older epoch still arrives in realtime; the inbound router opens each
                    // against the channel's full keyset.
                    for (epoch, key) in ch.read_epoch_keys() {
                        let pseudonym = derive::channel_pseudonym(&key, &ch.id, epoch).to_hex();
                        pseudonyms.push(pseudonym.clone());
                        routes.insert(pseudonym, ch.clone());
                    }
                    // The NEXT channel-rekey coordinate (3303) under the CURRENT server root — follow a
                    // channel rotation event-driven the instant it lands.
                    let next_chan = derive::rekey_pseudonym(
                        &community.server_root_key, &ch.id, Epoch(ch.epoch.0 + 1),
                    ).to_hex();
                    pseudonyms.push(next_chan.clone());
                    control_routes.insert(next_chan, community.id.to_hex());
                }
                // Control-plane pseudonym at the current server-root epoch (banlist/roles/metadata/invites).
                let ctrl = roster::control_pseudonym(
                    &community.server_root_key, &community.id, community.server_root_epoch,
                );
                pseudonyms.push(ctrl.clone());
                control_routes.insert(ctrl, community.id.to_hex());
                // The NEXT base re-founding coordinate (3303) — follow a privatize / private-ban in realtime.
                let next_base = derive::base_rekey_pseudonym(
                    &community.server_root_key, &community.id,
                    Epoch(community.server_root_epoch.0 + 1),
                ).to_hex();
                pseudonyms.push(next_base.clone());
                control_routes.insert(next_base, community.id.to_hex());
            }
        }
    }

    *COMMUNITY_ROUTES.lock().await = routes;
    *CONTROL_ROUTES.lock().await = control_routes;
    (pseudonyms, relays)
}

/// (Re)build the Community subscription: rebuild the route maps, then open TWO subscriptions over the
/// same filter — a targeted `subscribe_to` (streams on desktop) and a pool-wide `subscribe` (streams on
/// Android). `process_incoming` dedups by outer-event id, so overlap folds exactly once.
pub async fn refresh_subscription(client: &Client) {
    let (pseudonyms, relays) = rebuild_routes().await;

    // Hold COMMUNITY_SUB_ID across the unsubscribe+subscribe ON PURPOSE: it serializes concurrent
    // refreshers (Monitor, health-probe reconnect, refresh_control, accept_invite) so they can't
    // race into a duplicate subscription. Narrowing this lock would reintroduce that double-sub race.
    let mut new_set = pseudonyms.clone();
    new_set.sort();

    let mut sub_guard = COMMUNITY_SUB_ID.lock().await;
    let mut set_guard = COMMUNITY_SUB_SET.lock().await;

    // Idempotent: unchanged pseudonym set + a live sub → keep it (the pool auto-re-applies it across
    // reconnects, verified). Rebuilding would be pure churn and a rebuild landing mid-reconnect silently
    // fails to register. rebuild_routes() above already refreshed routes/banlist — all a no-change needs.
    if sub_guard.is_some() && *set_guard == new_set {
        return;
    }

    if let Some(old_id) = sub_guard.take() {
        client.unsubscribe(&old_id).await;
    }
    *set_guard = new_set;

    if pseudonyms.is_empty() {
        if let Some(old_pw) = COMMUNITY_POOLWIDE_SUB_ID.lock().await.take() {
            client.unsubscribe(&old_pw).await;
        }
        return;
    }

    // Community events live on the community's relays, which may differ from the user's DM relays.
    // Add them GOSSIP|PING (warm, but excluded from pool-wide DM/profile ops) and subscribe by TARGET.
    for r in &relays {
        let _ = client.pool().add_relay(r.as_str(), crate::community_relay_options()).await;
    }
    client.connect().await;

    // Wait (briefly) for at least one community relay to actually CONNECT before subscribing. A
    // subscribe_to/subscribe issued against a still-connecting relay silently fails to register the
    // live sub (seen right after the Android bg-sync churn / on create-with-avatar, which fires extra
    // control re-folds → re-subscribes mid-reconnect). Polling until a socket is live makes the sub
    // land reliably regardless of churn timing. Dead/slow relays (e.g. a timing-out one) are ignored —
    // one connected relay is enough to stream.
    {
        let wanted: Vec<RelayUrl> = relays.iter().filter_map(|r| RelayUrl::parse(r).ok()).collect();
        for _ in 0..24 {
            let pool = client.pool().all_relays().await;
            let any_live = wanted.iter().any(|u| {
                pool.get(u).map(|r| r.status() == RelayStatus::Connected).unwrap_or(false)
            });
            if any_live {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    let filter = Filter::new()
        .kinds([
            Kind::Custom(event_kind::COMMUNITY_MESSAGE),
            Kind::Custom(event_kind::COMMUNITY_REACTION),
            Kind::Custom(event_kind::COMMUNITY_EDIT),
            Kind::Custom(event_kind::COMMUNITY_DELETE),
            Kind::Custom(event_kind::COMMUNITY_PRESENCE),
            Kind::Custom(event_kind::COMMUNITY_KICK),
            Kind::Custom(event_kind::COMMUNITY_WEBXDC),
            Kind::Custom(event_kind::COMMUNITY_CONTROL),
            Kind::Custom(event_kind::COMMUNITY_REKEY),
        ])
        .custom_tags(SingleLetterTag::lowercase(Alphabet::Z), pseudonyms)
        .limit(0);

    // Pool-wide subscribe — the path that streams on Android (replaces any prior one).
    {
        let mut pw = COMMUNITY_POOLWIDE_SUB_ID.lock().await;
        if let Some(old) = pw.take() {
            client.unsubscribe(&old).await;
        }
        if let Ok(out) = client.subscribe(filter.clone(), None).await {
            *pw = Some(out.val);
        }
    }
    // Targeted subscribe — the path that streams on desktop.
    if let Ok(output) = client.subscribe_to(relays.iter().cloned(), filter, None).await {
        *sub_guard = Some(output.val);
    }
}

/// Route an arriving Community event: a CONTROL/REKEY edition triggers a realtime control refresh;
/// any other (message/reaction/edit/delete/presence/typing/webxdc/kick) is opened against its
/// channel via [`inbound::process_incoming`], persisted, and dispatched to `handler`. `session`
/// straddles the relay I/O so a mid-flight account swap can't write into the swapped-in account.
pub async fn dispatch_event(
    session: &SessionGuard,
    event: Event,
    handler: Arc<dyn InboundEventHandler>,
) {
    let Some(my_pk) = crate::my_public_key() else { return; };
    let Some(pseudonym) = event.tags.iter().find_map(|t| {
        let s = t.as_slice();
        (s.len() >= 2 && s[0] == "z").then(|| s[1].clone())
    }) else { return; };

    // A CONTROL edition (3308) or base/channel REKEY (3303) → follow the control plane in realtime.
    let kind = event.kind.as_u16();
    if kind == event_kind::COMMUNITY_CONTROL || kind == event_kind::COMMUNITY_REKEY {
        let community_id = CONTROL_ROUTES.lock().await.get(&pseudonym).cloned();
        if let Some(community_id) = community_id {
            if session.is_valid() {
                // Spawn off the loop — the refresh runs several relay fetches (seconds) and must not
                // head-of-line-block other event consumption. It self-captures a guard + re-checks.
                tokio::spawn(refresh_control(community_id, handler.clone()));
            }
        }
        return;
    }

    let Some(channel) = COMMUNITY_ROUTES.lock().await.get(&pseudonym).cloned() else {
        return;
    };
    if !session.is_valid() {
        return;
    }

    let outcome = {
        let mut state = crate::state::STATE.lock().await;
        inbound::process_incoming(&mut state, &event, &channel, &my_pk)
    };
    let chat_id = channel.id.to_hex();
    match outcome {
        Some(inbound::IncomingEvent::NewMessage(mut msg)) => {
            // Resolve the reply preview (content/npub) from the DB before emitting,
            // mirroring the DM realtime path. The replied-to message is often an
            // older one that's persisted but outside the in-memory window; without
            // this the recipient's live render finds no in-memory target and the
            // reply shows as a plain message with no context.
            if !msg.replied_to.is_empty() {
                let _ = crate::db::events::populate_reply_context(&mut msg).await;
            }
            let _ = crate::db::events::save_message(&chat_id, &msg).await;
            handler.on_community_message(&chat_id, &msg, true);
        }
        Some(inbound::IncomingEvent::Updated { target_id, message, edit_event }) => {
            // Edits are event-sourced (folded on reload); reactions re-save the message row.
            if let Some(ev) = edit_event {
                let mut ev = (*ev).clone();
                if let Ok(cid) = crate::db::id_cache::get_chat_id_by_identifier(&chat_id) {
                    ev.chat_id = cid;
                }
                let _ = crate::db::events::save_event(&ev).await;
            } else {
                let _ = crate::db::events::save_message(&chat_id, &message).await;
            }
            handler.on_community_update(&chat_id, &target_id, &message);
        }
        Some(inbound::IncomingEvent::Removed { target_id }) => {
            let _ = crate::db::events::delete_event(&target_id).await;
            handler.on_community_removed(&chat_id, &target_id);
        }
        Some(inbound::IncomingEvent::ReactionRemoved { message_id, reaction_id, message }) => {
            // Drop the reaction's kind-7 row (save is additive) and refresh the parent's chips.
            let _ = crate::db::events::delete_event(&reaction_id).await;
            handler.on_community_update(&chat_id, &message_id, &message);
        }
        Some(inbound::IncomingEvent::Presence { npub, joined, event_id, created_at, invited_by, invited_label }) => {
            handler.on_community_presence(
                &chat_id, &npub, joined, &event_id, created_at,
                invited_by.as_deref(), invited_label.as_deref(),
            );
        }
        Some(inbound::IncomingEvent::WebxdcPeer { npub, topic_id, node_addr, event_id, created_at }) => {
            handler.on_community_webxdc(
                &chat_id, &npub, &topic_id, node_addr.as_deref(), &event_id, created_at,
            );
        }
        Some(inbound::IncomingEvent::Typing { npub, until }) => {
            handler.on_community_typing(&chat_id, &npub, until);
        }
        Some(inbound::IncomingEvent::Kicked { community_id })
        | Some(inbound::IncomingEvent::SelfLeft { community_id }) => {
            // The handler owns teardown (the GUI prunes chats/relays + republishes the list; a
            // headless consumer can call `teardown_local`). Core only routes + notifies here.
            handler.on_community_self_removed(&community_id);
        }
        None => {}
    }
}

/// Realtime control-plane follow: walk a re-founding, fold the control plane (banlist/roles/
/// metadata/invites), follow channel rekeys, then resubscribe at the new pseudonyms if any epoch
/// advanced (else just refresh the route maps so the new banlist takes live effect). Mirrors the
/// Tauri `refresh_community_control` orchestration over the already-core `catch_up_*` primitives.
pub async fn refresh_control(community_id: String, handler: Arc<dyn InboundEventHandler>) {
    // Claim the in-flight slot or bail (a concurrent refresh is already folding this community).
    {
        let mut inflight = REFRESH_CONTROL_INFLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if !inflight.insert(community_id.clone()) {
            return;
        }
    }
    struct RefreshClaim(String);
    impl Drop for RefreshClaim {
        fn drop(&mut self) {
            REFRESH_CONTROL_INFLIGHT.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
        }
    }
    let _claim = RefreshClaim(community_id.clone());

    let session = SessionGuard::capture();
    let Some(id_bytes) = hex_to_id32(&community_id) else { return; };
    let Some(community) = crate::db::community::load_community(&CommunityId(id_bytes)).ok().flatten() else { return; };
    let bt = LiveTransport::with_timeout(Duration::from_secs(20));
    let pre_server_epoch = community.server_root_epoch.0;
    let pre_channel_epochs: Vec<(String, u64)> =
        community.channels.iter().map(|c| (c.id.to_hex(), c.epoch.0)).collect();

    // FOLLOW FIRST: a privatize / private-ban re-founds the base under a NEW epoch + re-anchors the
    // control plane there, so walk the rotation BEFORE folding control. An AUTHORIZED rotation that
    // excluded us is a removal → tear down locally.
    if let Ok(c) = service::catch_up_server_root(&bt, &community).await {
        if !session.is_valid() { return; }
        if c.removed { handler.on_community_self_removed(&community_id); return; }
    }
    if !session.is_valid() { return; }
    let community = crate::db::community::load_community(&CommunityId(id_bytes)).ok().flatten().unwrap_or(community);
    let _ = service::fetch_and_apply_control(&bt, &community).await;
    if !session.is_valid() { return; }
    // Banned by the just-folded banlist → torn down, nothing more to do.
    if let Some(c) = crate::db::community::load_community(&CommunityId(id_bytes)).ok().flatten() {
        if service::am_i_banned(&c) {
            handler.on_community_self_removed(&community_id);
            return;
        }
    }
    if !session.is_valid() { return; }

    // Walk each channel's rekey chain. A re-founding rotates base AND every channel once; the channel
    // rekey publishes right after the base rekey, so a single fetch can race propagation — retry with a
    // short backoff until every channel reaches the expected epoch (next sync is the backstop).
    let base_delta = crate::db::community::load_community(&CommunityId(id_bytes)).ok().flatten()
        .map(|c| c.server_root_epoch.0).unwrap_or(pre_server_epoch).saturating_sub(pre_server_epoch);
    for attempt in 0..CHANNEL_FOLLOW_MAX_ATTEMPTS {
        if !session.is_valid() { return; }
        let Some(cur) = crate::db::community::load_community(&CommunityId(id_bytes)).ok().flatten() else { break; };
        for ch in &cur.channels {
            let _ = service::catch_up_channel_rekeys(&bt, &cur, &ch.id).await;
        }
        let caught = base_delta == 0 || crate::db::community::load_community(&CommunityId(id_bytes)).ok().flatten()
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
    let community = crate::db::community::load_community(&CommunityId(id_bytes)).ok().flatten().unwrap_or(community);
    let _ = service::retry_pending_read_cut(&bt, &community).await;
    if !session.is_valid() { return; }
    let community = crate::db::community::load_community(&CommunityId(id_bytes)).ok().flatten().unwrap_or(community);

    // If an epoch advanced, rebuild the FULL subscription so realtime delivery resumes at the new
    // pseudonyms; else just refresh the route maps so the inbound drop-filter sees the new banlist.
    let advanced = community.server_root_epoch.0 != pre_server_epoch
        || community.channels.iter().any(|c| {
            pre_channel_epochs.iter().find(|(id, _)| id == &c.id.to_hex()).map(|(_, e)| *e != c.epoch.0).unwrap_or(true)
        });
    if advanced && session.is_valid() {
        if let Some(client) = crate::state::nostr_client() {
            refresh_subscription(&client).await;
        }
    } else {
        let _ = rebuild_routes().await;
    }
    if !session.is_valid() { return; }
    crate::community::list::refresh_membership_current(&community);
    handler.on_community_refreshed(&community_id);
}

/// Tear down a community locally on a received removal (kick/leave/ban), RETAINING the held epoch
/// keys (so a self-scrub republish/erase still works), then refresh the subscription so we stop
/// listening on its pseudonyms. Headless consumers can call this from `on_community_self_removed`;
/// the GUI does a richer teardown (prune chats/relays + republish the list) in its handler instead.
pub async fn teardown_local(community_id: &str) {
    let _ = crate::db::community::delete_community_retain_keys(community_id);
    if let Some(client) = crate::state::nostr_client() {
        refresh_subscription(&client).await;
    }
}

/// hex (64 chars) → 32-byte id.
fn hex_to_id32(hex: &str) -> Option<[u8; 32]> {
    (hex.len() == 64).then(|| crate::simd::hex::hex_to_bytes_32(hex))
}
