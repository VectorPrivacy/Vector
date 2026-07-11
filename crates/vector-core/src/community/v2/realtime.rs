//! v2 realtime — the `authors`-based live subscription + dispatch.
//!
//! v2 addresses planes by their group pubkey (CORD-01), not v1's `#z` tag, so
//! the subscription is `{kinds:[1059,21059], authors:[…plane pubkeys…]}`. A
//! received event is routed to the owning community and handed to the shared
//! [`inbound::dispatch_wrap`], which fires the protocol-agnostic
//! `InboundEventHandler` the SDK's `on_message` consumes.
//!
//! The kind-1059 dispatch rule (CLAUDE.md A4): the listen loop tries this v2
//! path for events on the v2 subscription; DM gift wraps and 3313 Direct Invites
//! (both `#p=me`) stay on the DM subscription — the author-set here never
//! includes an identity key, so the two never collide.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex as StdMutex};

use nostr_sdk::prelude::{Client, Event, Filter, Kind, PublicKey, RelayStatus, RelayUrl, SubscriptionId};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Mutex;

use super::community::CommunityV2;
use super::stream;
use super::{derive, inbound};
use crate::community::{CommunityId, ConcordProtocol, Epoch};
use crate::event_handler::InboundEventHandler;
use crate::state::SessionGuard;

/// The targeted subscription id (streams on desktop).
static V2_SUB_ID: LazyLock<Mutex<Option<SubscriptionId>>> = LazyLock::new(|| Mutex::new(None));
/// The pool-wide subscription id (the path that streams on Android).
static V2_POOLWIDE_SUB_ID: LazyLock<Mutex<Option<SubscriptionId>>> = LazyLock::new(|| Mutex::new(None));
/// The current author-set (sorted hex) — an unchanged set skips a churny
/// unsubscribe+resubscribe, exactly like v1's `COMMUNITY_SUB_SET`.
static V2_SUB_SET: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));
/// Outer-wrap ids already dispatched, so the handler fires EXACTLY ONCE per
/// message. The relay pool delivers the same wrap under both the targeted and
/// pool-wide subs and from every relay independently, so without this a bot's
/// `on_message` (and its reply) would run several times per message — the v1
/// community path and the DM path dedup by outer id for the same reason. Cleared
/// on session swap; coarsely bounded (a message's duplicates all arrive within a
/// short window, so a recent-set suffices).
static V2_SEEN_WRAPS: LazyLock<Mutex<HashSet<[u8; 32]>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
/// Bound on [`V2_SEEN_WRAPS`] before a coarse flush.
const SEEN_WRAPS_CAP: usize = 8192;
/// The per-community follow QUEUE. dispatch, boot catch-up, reconnect, and manual
/// sync all just [`enqueue_follow`] — non-blocking + coalesced. A single spawned
/// worker ([`spawn_follow_worker`]) drains it and runs one combined rekey+control
/// follow per community at a time, so two triggers can never concurrently
/// whole-row-save and clobber each other. This replaces the old gate/rerun/
/// spawn-vs-await machinery: an enqueue never blocks its caller, and a junk-wrap
/// flood coalesces to at most one queued + one running follow per community.
/// Reset on session swap (the worker exits when its `SessionGuard` invalidates or
/// its channel closes).
static V2_FOLLOW_TX: LazyLock<StdMutex<Option<UnboundedSender<CommunityId>>>> = LazyLock::new(|| StdMutex::new(None));
/// Community ids currently queued or processing — coalesces a burst to one follow.
static V2_FOLLOW_PENDING: LazyLock<StdMutex<HashSet<[u8; 32]>>> = LazyLock::new(|| StdMutex::new(HashSet::new()));
/// Per-community follow serialization, shared by the queue worker AND the inline
/// (headless) follow path. The worker-vs-inline CHOICE is a benign race
/// (`follow_worker_running` is check-then-act; a worker can spawn right after a
/// `false`), so correctness can't ride on it: whichever path runs, the follow body
/// executes under this lock and two follows of one community can never interleave
/// their whole-row saves. Bounded by the held-community count; reset on swap.
static V2_FOLLOW_LOCKS: LazyLock<StdMutex<std::collections::HashMap<[u8; 32], Arc<Mutex<()>>>>> =
    LazyLock::new(|| StdMutex::new(std::collections::HashMap::new()));

/// The follow lock for one community (created on first use).
pub(crate) fn follow_lock(id: &CommunityId) -> Arc<Mutex<()>> {
    V2_FOLLOW_LOCKS.lock().unwrap().entry(id.0).or_default().clone()
}

pub async fn subscription_id() -> Option<SubscriptionId> {
    V2_SUB_ID.lock().await.clone()
}

pub async fn poolwide_subscription_id() -> Option<SubscriptionId> {
    V2_POOLWIDE_SUB_ID.lock().await.clone()
}

/// Clear the v2 realtime state on a session reset (called from `swap_session`
/// alongside v1's clear), so a stale sub id / author-set can't leak across accounts.
pub async fn clear() {
    *V2_SUB_ID.lock().await = None;
    *V2_POOLWIDE_SUB_ID.lock().await = None;
    V2_SUB_SET.lock().await.clear();
    V2_SEEN_WRAPS.lock().await.clear();
    // Drop the queue sender so the worker's channel closes and it exits (its
    // SessionGuard also invalidates); the next login spawns a fresh worker.
    *V2_FOLLOW_TX.lock().unwrap() = None;
    V2_FOLLOW_PENDING.lock().unwrap().clear();
    V2_FOLLOW_LOCKS.lock().unwrap().clear();
    // Account A's stream keys must not keep authenticating (or answering relay
    // challenges) once account B is live.
    super::streamauth::clear();
}

/// Every plane pubkey a set of v2 communities publishes under that
/// [`inbound::dispatch_wrap`] handles — the subscription author-set. Per
/// community: the guestbook, the control plane, and each channel's current
/// Chat-Plane address. Pure + deterministic (deduped, sorted) — the testable core.
///
/// The **control plane** rides here so a long-running bot follows metadata +
/// public-channel edits live ([`super::service::follow_control`] re-folds on a
/// recognized wrap), and the next-epoch rekey planes ride via [`rekey_authors`]
/// (each subscribed author has its `dispatch_wrap` arm — never one without the
/// other).
pub fn plane_authors(communities: &[CommunityV2]) -> Vec<PublicKey> {
    let mut out = Vec::new();
    for c in communities {
        out.push(derive::guestbook_group_key(&c.community_root, c.id(), c.root_epoch).pk());
        out.push(control_author(c));
        // The dissolved plane (CORD-02 §9) — so a mid-session dissolution seals live.
        out.push(super::derive::dissolved_group_key(c.id()).pk());
        for ch in &c.channels {
            // A KEYLESS private channel has no readable chat plane — channel_secret's
            // root fallback would subscribe the PUBLIC plane for it. Its rekey plane
            // (below) is still watched, which is how its key arrives.
            if ch.private && ch.key.is_none() {
                continue;
            }
            let (secret, epoch) = c.channel_secret(ch);
            out.push(derive::channel_group_key(&secret, &ch.id, epoch).pk());
        }
        out.extend(rekey_authors(c));
    }
    out.sort_by_key(|p| p.to_hex());
    out.dedup();
    out
}

/// This community's Control Plane address at the current root epoch — the single
/// source of truth shared by [`plane_authors`] (subscribe) and
/// [`inbound::dispatch_wrap`] (recognize) so the two can't drift.
pub(crate) fn control_author(c: &CommunityV2) -> PublicKey {
    derive::control_group_key(&c.community_root, c.id(), c.root_epoch).pk()
}

/// The next-epoch rekey plane addresses for a community: the base rotation
/// (`root_epoch + 1`) and each Private channel's rotation (`channel epoch + 1`),
/// both under the CURRENT community_root. This is the single source of truth for
/// which rekey wraps we subscribe AND recognize (`inbound::dispatch_wrap` calls
/// it), so the two can never drift. A Public channel has no independent rotation
/// (it rides the base), so only Private channels contribute a channel address.
pub(crate) fn rekey_authors(c: &CommunityV2) -> Vec<PublicKey> {
    // saturating: a bundle's epoch isn't covered by the community_id commitment, so
    // it's attacker-influenced — never let `epoch + 1` overflow (a u64::MAX epoch
    // just yields a dead address, never a panic).
    let mut out = vec![derive::base_rekey_group_key(&c.community_root, c.id(), Epoch(c.root_epoch.0.saturating_add(1))).pk()];
    for ch in &c.channels {
        if ch.private {
            out.push(derive::channel_rekey_group_key(&c.community_root, &ch.id, Epoch(ch.epoch.0.saturating_add(1))).pk());
        }
    }
    out
}

/// Load every locally-held, LIVE **v2** community (dispatching each id by its
/// stored protocol). The realtime layer folds these into the subscription +
/// routing, so excluding a DISSOLVED community here is what enforces CORD-02 §9
/// on the receive side: its chat/control/rekey planes stop being subscribed and
/// an arriving wrap for it is `NotOurs` (dropped, never honored). Held keys still
/// open old history through the explicit read paths — this only stops NEW events.
pub fn load_held_v2() -> Vec<CommunityV2> {
    let ids = crate::db::community::list_community_ids().unwrap_or_default();
    ids.iter()
        .filter(|id| matches!(crate::db::community::community_protocol(id).ok().flatten(), Some(ConcordProtocol::V2)))
        .filter(|id| !crate::db::community::get_community_dissolved(&crate::simd::hex::bytes_to_hex_32(&id.0)).unwrap_or(false))
        .filter_map(|id| crate::db::community::load_community_v2(id).ok().flatten())
        .collect()
}

/// Refresh the v2 subscription for the held communities: register
/// `{kinds:[1059,21059], authors:[…]}` on their relays (targeted + pool-wide,
/// mirroring v1). Idempotent on an unchanged author-set.
pub async fn refresh_subscription(client: &Client) {
    // Phase 1, LOCK-FREE: make sure the community relays are added + connected —
    // the slow part (a connect wait of up to ~6s). Holding the sub locks across
    // this stalled every concurrent dispatch/refresh behind one caller's connect.
    {
        let communities = load_held_v2();
        let mut relays: Vec<String> = communities.iter().flat_map(|c| c.relays.iter().cloned()).collect();
        relays.sort();
        relays.dedup();
        if !relays.is_empty() {
            // Community relays ride GOSSIP|PING (warm but excluded from pool-wide DM ops).
            for r in &relays {
                let _ = client.pool().add_relay(r.as_str(), crate::community_relay_options()).await;
            }
            client.connect().await;
            // Wait briefly for at least one relay to actually connect (a subscribe
            // against a still-connecting relay silently fails to register — same
            // trap as v1).
            let wanted: Vec<RelayUrl> = relays.iter().filter_map(|r| RelayUrl::parse(r).ok()).collect();
            for _ in 0..24 {
                let pool = client.pool().all_relays().await;
                if wanted.iter().any(|u| pool.get(u).map(|r| r.status() == RelayStatus::Connected).unwrap_or(false)) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            // AUTH-gating relays serve the planes only to a stream-authenticated
            // connection, and a live subscription isn't auto-retried after the gate.
            // Register every held plane's key + prime the connection auth (a cheap
            // gated fetch the responder answers) so the subscription below streams.
            for c in &communities {
                super::streamauth::register_community(c);
            }
            super::streamauth::prime_auth(client, &relays).await;
        }
    }

    // Phase 2, LOCKED + bounded (no connect waits): RE-snapshot the held state
    // INSIDE the sub locks — the follow worker runs concurrently and may have just
    // adopted a rotation, so the LAST locker must read the freshest persisted
    // authors. An out-of-lock read lets a stale caller commit an old author-set
    // over a fresh one and silently mute a rotated community. (A community whose
    // relays appeared between the phases subscribes now and connects on the next
    // refresh — the follow that discovers it always triggers one.)
    let mut sub_guard = V2_SUB_ID.lock().await;
    let mut set_guard = V2_SUB_SET.lock().await;

    let communities = load_held_v2();
    let authors = plane_authors(&communities);
    let mut relays: Vec<String> = communities.iter().flat_map(|c| c.relays.iter().cloned()).collect();
    relays.sort();
    relays.dedup();

    let mut new_set: Vec<String> = authors.iter().map(|p| p.to_hex()).collect();
    new_set.sort();

    // Unchanged-set fast path — but only when BOTH subs actually registered (a
    // failed pool-wide subscribe would otherwise stay absent until the author-set
    // changes, and Android streams via the pool-wide path).
    if sub_guard.is_some() && *set_guard == new_set && (authors.is_empty() || V2_POOLWIDE_SUB_ID.lock().await.is_some()) {
        return; // the pool re-applies the live subs across reconnects.
    }
    if let Some(old) = sub_guard.take() {
        client.unsubscribe(&old).await;
    }
    *set_guard = new_set;

    if authors.is_empty() {
        if let Some(old_pw) = V2_POOLWIDE_SUB_ID.lock().await.take() {
            client.unsubscribe(&old_pw).await;
        }
        return;
    }

    let filter = Filter::new()
        .kinds([Kind::Custom(stream::KIND_WRAP), Kind::Custom(stream::KIND_WRAP_EPHEMERAL)])
        .authors(authors)
        .limit(0);

    {
        let mut pw = V2_POOLWIDE_SUB_ID.lock().await;
        if let Some(old) = pw.take() {
            client.unsubscribe(&old).await;
        }
        if let Ok(out) = client.subscribe(filter.clone(), None).await {
            *pw = Some(out.val);
        }
    }
    if let Ok(out) = client.subscribe_to(relays.iter().cloned(), filter, None).await {
        *sub_guard = Some(out.val);
    }
}

/// Re-send the CURRENT v2 subscriptions (same ids) to ONE relay. An AUTH-gating
/// relay CLOSEs a sub REQ that raced ahead of the stream AUTHs on a fresh
/// connection, and it never re-challenges once the connection is authenticated —
/// so the moment the streams finish authenticating is exactly when the subs must
/// be re-sent. nostr-sdk re-sends them only when ITS OWN auth completes (it
/// can't see ours), which usually wins by socket ordering; this makes the heal
/// deterministic and independent of that internal. Same-id REQs are idempotent.
pub(crate) async fn resubscribe_relay(client: &Client, relay: &RelayUrl) {
    let targeted = V2_SUB_ID.lock().await.clone();
    let poolwide = V2_POOLWIDE_SUB_ID.lock().await.clone();
    if targeted.is_none() && poolwide.is_none() {
        return; // nothing subscribed yet — the first refresh registers on an authed socket.
    }
    let communities = load_held_v2();
    let authors = plane_authors(&communities);
    if authors.is_empty() {
        return;
    }
    let filter = Filter::new()
        .kinds([Kind::Custom(stream::KIND_WRAP), Kind::Custom(stream::KIND_WRAP_EPHEMERAL)])
        .authors(authors)
        .limit(0);
    for id in [targeted, poolwide].into_iter().flatten() {
        let _ = client.subscribe_with_id_to([relay.clone()], id, filter.clone(), None).await;
    }
}

/// Route an arriving v2 wrap: find the held community whose plane it opens under
/// and fire the matching handler callback (via the shared bridge). Persistence to
/// the local DB is deferred (bots deliver via the callback; GUI history is v1 for
/// now). `session` gates against a mid-flight account swap.
pub async fn dispatch_event(session: &SessionGuard, event: Event, handler: Arc<dyn InboundEventHandler>) {
    let Some(my_pk) = crate::my_public_key() else {
        return;
    };
    if !session.is_valid() {
        return;
    }
    // Fire EXACTLY ONCE per wrap: the pool re-delivers the same event under both
    // subs and from every relay. `insert` returns false if already dispatched.
    {
        let mut seen = V2_SEEN_WRAPS.lock().await;
        if !seen.insert(event.id.to_bytes()) {
            return;
        }
        if seen.len() > SEEN_WRAPS_CAP {
            let keep = event.id.to_bytes();
            seen.clear();
            seen.insert(keep);
        }
    }
    let communities = load_held_v2();
    for c in &communities {
        match inbound::dispatch_wrap(&event, c, &my_pk, &*handler) {
            inbound::DispatchedV2::NotOurs => continue,
            // A control OR a rekey wrap: just enqueue a follow for this community.
            // Non-blocking + coalesced — the single follow worker serializes control
            // and rekey per community (no concurrent whole-row clobber) off this hot
            // path, so a junk-wrap flood can't head-of-line-block the notification loop.
            inbound::DispatchedV2::Control { .. } | inbound::DispatchedV2::Rekey { .. } => {
                enqueue_follow(c.id());
                return;
            }
            inbound::DispatchedV2::Dissolved { community_id } => {
                // Death wins (CORD-02 §9): seal read-only + surface the grave, ONCE
                // (a re-wrapped tombstone with a fresh outer id must not re-fire the
                // handler). The next load_held_v2 excludes it, so its planes also
                // stop being subscribed + routed.
                if crate::db::community::set_community_dissolved(&community_id).unwrap_or(false) {
                    handler.on_community_dissolved(&community_id);
                    if let Some(client) = crate::state::nostr_client() {
                        refresh_subscription(&client).await;
                    }
                }
                return;
            }
            // A chat event, opened but NOT yet applied: persist first (dedup by inner
            // id + the author-scoped edit/delete checks), then fire the callback from
            // the outcome — v1's exact model. A re-wrapped duplicate (any keyholder
            // can re-seal a signed rumor into a fresh 1059), the relay echo of our
            // own send, or a forged edit/delete yields no outcome and re-fires
            // nothing.
            inbound::DispatchedV2::Chat { channel_id, event } => {
                if !session.is_valid() {
                    return;
                }
                match inbound::persist_chat_event(&event, &channel_id, &my_pk, session).await {
                    Some(inbound::ChatPersist::New(message)) => handler.on_community_message(&channel_id, &message, true),
                    // A reaction or an edit: the folded TARGET row (its id is the
                    // target's) — the same payload v1 hands this callback.
                    Some(inbound::ChatPersist::Updated { message, .. }) => handler.on_community_update(&channel_id, &message.id, &message),
                    Some(inbound::ChatPersist::Removed(target_id)) => handler.on_community_removed(&channel_id, &target_id),
                    None => {}
                }
                return;
            }
            inbound::DispatchedV2::Presence { .. } => {
                // Live membership motion: fold it into the persisted Guestbook so
                // the memberlist stays a local read (the presence callback already
                // fired inline). Reopen here — the dispatcher stays pure — and
                // refresh the overview when it lands.
                if !session.is_valid() {
                    return;
                }
                let gb = derive::guestbook_group_key(&c.community_root, c.id(), c.root_epoch);
                if let Ok(opened) = super::stream::open_wrap(&event, &gb) {
                    if let Ok(ev) = super::guestbook::parse_guestbook_event(&opened) {
                        let changed = super::service::ingest_guestbook_event(c, ev, event.created_at.as_secs()).unwrap_or(false);
                        if changed && session.is_valid() {
                            handler.on_community_refreshed(&crate::simd::hex::bytes_to_hex_32(&c.id().0));
                        }
                    }
                }
                return;
            }
            _ => return, // typing (and non-surfaced guestbook kinds) handled inline by the dispatcher.
        }
    }
}

/// Whether a live follow worker is draining the queue (a `listen()` is running).
/// Headless callers use this to run a follow inline instead of enqueueing into
/// the void.
pub fn follow_worker_running() -> bool {
    V2_FOLLOW_TX.lock().unwrap().as_ref().map(|tx| !tx.is_closed()).unwrap_or(false)
}

/// Queue a follow for `id` — NON-BLOCKING + coalesced. A burst (or a junk-wrap
/// flood) collapses to at most one queued + one running follow per community. A
/// no-op if no worker is running (no live `listen()`). Callers: dispatch,
/// boot/reconnect catch-up, manual sync — none of them block or touch a lock for
/// longer than the enqueue.
pub fn enqueue_follow(id: &CommunityId) {
    let mut pending = V2_FOLLOW_PENDING.lock().unwrap();
    if !pending.insert(id.0) {
        return; // already queued or processing — coalesce.
    }
    match V2_FOLLOW_TX.lock().unwrap().as_ref() {
        Some(tx) if tx.send(*id).is_ok() => {}
        _ => {
            pending.remove(&id.0); // no worker / channel closed — nothing queued.
        }
    }
}

/// Spawn the single follow worker for this session. Installs the queue sender and
/// drains it, running one combined follow per community at a time. Replacing the
/// sender (a re-`listen()`) or [`clear`] (a swap) closes the old channel so the old
/// worker exits; the captured `SessionGuard` also stops it. Idempotent per session.
pub fn spawn_follow_worker(handler: Arc<dyn InboundEventHandler>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CommunityId>();
    *V2_FOLLOW_TX.lock().unwrap() = Some(tx);
    V2_FOLLOW_PENDING.lock().unwrap().clear();
    let session = SessionGuard::capture();
    tokio::spawn(async move {
        while let Some(id) = rx.recv().await {
            if !session.is_valid() {
                break;
            }
            // Remove from pending BEFORE running, so a trigger arriving DURING the
            // follow re-enqueues (and is processed after) rather than being lost.
            V2_FOLLOW_PENDING.lock().unwrap().remove(&id.0);
            follow_community(&session, &id, &*handler).await;
        }
    });
}

/// One combined rekey-then-control follow for a community, each pass against the
/// FRESHLY-RELOADED persisted state (never a stale clone, so the two planes can't
/// lose each other's writes). Rekey runs first: a base adopt moves the control
/// address, and a self-removal tears the community down (skipping control). No-op
/// without a live client — unit tests drive `service::follow_control` /
/// `follow_rekeys` directly.
async fn follow_community(session: &SessionGuard, id: &CommunityId, handler: &dyn InboundEventHandler) {
    let Some(client) = crate::state::nostr_client() else {
        return;
    };
    // Serialize against an inline (headless) follow of the same community — the
    // queue only serializes triggers routed THROUGH it.
    let lock = follow_lock(id);
    let _guard = lock.lock().await;
    let community_id = crate::simd::hex::bytes_to_hex_32(&id.0);
    let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(12));

    // Rekey first (fresh DB state).
    let Ok(Some(current)) = crate::db::community::load_community_v2(id) else {
        return; // community gone (left / removed).
    };
    match super::service::follow_rekeys(&transport, &current, session).await {
        // A tombstone surfaced during catch-up (an offline member learning of a
        // death) — the flag is set; seal + surface, and stop following.
        Ok(follow) if follow.dissolved => {
            if !session.is_valid() {
                return;
            }
            handler.on_community_dissolved(&community_id);
            return;
        }
        Ok(follow) if follow.self_removed => {
            if !session.is_valid() {
                return;
            }
            let _ = crate::db::community::delete_community(&community_id);
            refresh_subscription(&client).await;
            handler.on_community_self_removed(&community_id);
            return;
        }
        Ok(follow) if follow.updated.is_some() => {
            if !session.is_valid() {
                return;
            }
            refresh_subscription(&client).await;
            handler.on_community_refreshed(&community_id);
        }
        Ok(_) => {}
        Err(_) => return,
    }

    // Control second, on the (possibly new-root) freshly-reloaded state.
    let Ok(Some(current)) = crate::db::community::load_community_v2(id) else {
        return;
    };
    if let Ok(Some(_)) = super::service::follow_control(&transport, &current, session).await {
        if !session.is_valid() {
            return;
        }
        refresh_subscription(&client).await;
        handler.on_community_refreshed(&community_id);
        // A control change can reveal rekey work that predates it — a just-announced
        // private channel's key crate is already sitting on its rekey plane (the key
        // ships BEFORE the vsk-2), and this pass's rekey walk ran before the channel
        // existed. Queue one more pass; it coalesces and converges (an unchanged
        // control fold doesn't re-queue).
        enqueue_follow(id);
    }

    // Guestbook third: catch the membership store up from its cursor. Boot and
    // reconnect land here through this same queue, so the memberlist is a local
    // read by the time any panel asks for it.
    let Ok(Some(current)) = crate::db::community::load_community_v2(id) else {
        return;
    };
    if matches!(super::service::sync_guestbook(&transport, &current, session).await, Ok(true)) {
        if !session.is_valid() {
            return;
        }
        handler.on_community_refreshed(&community_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::control::{genesis, CommunityMetadata};
    use crate::community::Epoch;
    use nostr_sdk::prelude::Keys;

    fn a_community(name: &str) -> CommunityV2 {
        let owner = Keys::generate();
        let g = genesis(&owner, CommunityMetadata { name: name.into(), ..Default::default() }, 1_000).unwrap();
        CommunityV2::from_genesis(&g, name, None, vec!["wss://r".into()], 0)
    }

    #[test]
    fn plane_authors_covers_the_dispatched_planes_only() {
        let c = a_community("A");
        let authors = plane_authors(std::slice::from_ref(&c));

        // Subscribed: the guestbook, the control plane, and the one public channel
        // (the planes dispatch_wrap handles). Exactly those three.
        let gb = derive::guestbook_group_key(&c.community_root, c.id(), c.root_epoch).pk();
        let control = derive::control_group_key(&c.community_root, c.id(), c.root_epoch).pk();
        let general = {
            let (s, e) = c.channel_secret(&c.channels[0]);
            derive::channel_group_key(&s, &c.channels[0].id, e).pk()
        };
        // Plus the next base-rekey address (rekey-follow). A public channel has no
        // independent rotation, so #general contributes no channel-rekey address.
        let next_base = derive::base_rekey_group_key(&c.community_root, c.id(), Epoch(1)).pk();
        // Plus the dissolved plane (CORD-02 §9), so a live dissolution is detected.
        let dissolved = derive::dissolved_group_key(c.id()).pk();
        assert!(
            authors.contains(&gb)
                && authors.contains(&control)
                && authors.contains(&general)
                && authors.contains(&next_base)
                && authors.contains(&dissolved)
        );
        assert_eq!(authors.len(), 5, "guestbook + control + dissolved + chat + base-rekey planes are subscribed");
    }

    #[test]
    fn plane_authors_is_deterministic_deduped_and_multi_community() {
        let a = a_community("A");
        let b = a_community("B");
        let one = plane_authors(std::slice::from_ref(&a));
        // Re-running over the same community is byte-identical (deterministic).
        assert_eq!(plane_authors(std::slice::from_ref(&a)), one);
        // Two distinct communities' planes are all present, none dropped.
        let two = plane_authors(&[a.clone(), b.clone()]);
        assert_eq!(two.len(), one.len() * 2);
        // Order-independent: reversing the input yields the identical sorted set.
        assert_eq!(plane_authors(&[b, a]), two);
    }

    #[tokio::test]
    async fn dispatch_event_routes_a_v2_message_to_the_handler() {
        use crate::community::transport::memory::MemoryRelay;
        use crate::community::transport::{Query, Transport};
        use crate::types::Message;
        use std::sync::Mutex as StdMutex;

        #[derive(Default)]
        struct Recorder {
            got: StdMutex<Vec<(String, String)>>,
        }
        impl InboundEventHandler for Recorder {
            fn on_community_message(&self, chat_id: &str, msg: &Message, _new: bool) {
                self.got.lock().unwrap().push((chat_id.to_string(), msg.content.clone()));
            }
        }

        // Offline DB + identity.
        let _g = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        let acct = {
            const B: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
            let mut s = String::from("npub1");
            for i in 0..58 {
                s.push(B[(i * 5 + 1) % 32] as char);
            }
            s
        };
        std::fs::create_dir_all(tmp.path().join(&acct)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(acct.clone()).unwrap();
        crate::db::init_database(&acct).unwrap();
        let _ = crate::state::take_nostr_client();
        let me = Keys::generate();
        crate::state::MY_SECRET_KEY.store_from_keys(&me, &[]);
        crate::state::set_my_public_key(me.public_key());

        // Create a v2 community (persisted), then ANOTHER member (holds the root)
        // posts — the incoming case a live sub delivers (an OWN send is echoed at
        // send time, so its relay copy correctly dedups instead of firing).
        let relay = MemoryRelay::new();
        let community = super::super::service::create_community(&relay, "Live", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        let member = Keys::generate();
        let group = derive::channel_group_key(&community.community_root, &general, community.root_epoch);
        let rumor = super::super::chat::build_message_rumor(member.public_key(), &general, community.root_epoch, "live ping", None, &[], vec![], 5_000);
        let (wrap, _) = super::super::chat::seal_chat_rumor(&rumor, &group, &member, nostr_sdk::prelude::Timestamp::from_secs(5), false).unwrap();
        let _ = relay.publish(&wrap, &community.relays).await;
        let q = Query { kinds: vec![stream::KIND_WRAP], authors: vec![group.pk_hex()], ..Default::default() };
        let wrap = relay.fetch(&q, &community.relays).await.unwrap().into_iter().find(|w| w.pubkey == group.pk()).unwrap();

        // The realtime dispatch (loading held v2 communities from the DB) routes it.
        // Dispatch the SAME wrap TWICE — modelling the pool re-delivering it under
        // the targeted + pool-wide subs (and from multiple relays). The handler
        // must fire EXACTLY ONCE (no duplicate bot replies) — and only AFTER the
        // persist outcome (the callbacks-from-persist model).
        let rec = Arc::new(Recorder::default());
        let session = SessionGuard::capture();
        crate::community::v2::realtime::clear().await; // fresh seen-set for the test
        dispatch_event(&session, wrap.clone(), rec.clone()).await;
        dispatch_event(&session, wrap, rec.clone()).await;

        let got = rec.got.lock().unwrap();
        assert_eq!(got.len(), 1, "a re-delivered wrap fires the handler exactly once");
        assert_eq!(got[0].1, "live ping");
        assert_eq!(got[0].0, crate::simd::hex::bytes_to_hex_32(&general.0));
    }

    #[tokio::test]
    async fn follow_queue_coalesces_a_burst_and_re_enqueues_after_processing() {
        // Install a test channel in place of the worker's, so we can observe what the
        // queue delivers without a live client.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CommunityId>();
        *V2_FOLLOW_TX.lock().unwrap() = Some(tx);
        V2_FOLLOW_PENDING.lock().unwrap().clear();

        let id = CommunityId([0x11; 32]);
        // A burst for one community collapses to a SINGLE queued follow (coalesced).
        enqueue_follow(&id);
        enqueue_follow(&id);
        enqueue_follow(&id);
        assert_eq!(rx.recv().await, Some(id), "first trigger queues a follow");
        assert!(rx.try_recv().is_err(), "the burst coalesced to exactly one");

        // The worker removes it from pending before running; a trigger AFTER that
        // re-queues (so a change during a follow isn't lost).
        V2_FOLLOW_PENDING.lock().unwrap().remove(&id.0);
        enqueue_follow(&id);
        assert_eq!(rx.recv().await, Some(id), "a trigger after processing re-queues");

        // A different community is independent (not coalesced against the first).
        let id2 = CommunityId([0x22; 32]);
        enqueue_follow(&id2);
        assert_eq!(rx.recv().await, Some(id2));

        *V2_FOLLOW_TX.lock().unwrap() = None;
        V2_FOLLOW_PENDING.lock().unwrap().clear();
    }

    #[tokio::test]
    async fn a_dissolved_community_honors_no_new_events_and_fires_death_once() {
        use super::super::service;
        use crate::community::transport::memory::MemoryRelay;
        use crate::community::transport::Transport;
        use crate::types::Message;
        use std::sync::Mutex as StdMutex;

        #[derive(Default)]
        struct Recorder {
            messages: StdMutex<Vec<String>>,
            deaths: StdMutex<Vec<String>>,
        }
        impl InboundEventHandler for Recorder {
            fn on_community_message(&self, _chat: &str, msg: &Message, _new: bool) {
                self.messages.lock().unwrap().push(msg.content.clone());
            }
            fn on_community_dissolved(&self, community_id: &str) {
                self.deaths.lock().unwrap().push(community_id.to_string());
            }
        }

        let _g = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        let acct = {
            const B: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
            let mut s = String::from("npub1");
            for i in 0..58 {
                s.push(B[(i * 3 + 2) % 32] as char);
            }
            s
        };
        std::fs::create_dir_all(tmp.path().join(&acct)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(acct.clone()).unwrap();
        crate::db::init_database(&acct).unwrap();
        let _ = crate::state::take_nostr_client();
        let me = Keys::generate();
        crate::state::MY_SECRET_KEY.store_from_keys(&me, &[]);
        crate::state::set_my_public_key(me.public_key());

        let relay = MemoryRelay::new();
        let community = service::create_community(&relay, "Doomed", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;

        // The owner's tombstone arrives as a MEMBER sees it (local flag still 0 —
        // build + publish it directly rather than via dissolve_community, which
        // would seal our own DB first and make it the owner-published case). `me`
        // is the owner, so the seal verifies.
        let rumor = super::super::dissolution::dissolved_tombstone_rumor(me.public_key(), community.id(), 8_000);
        let tombstone = super::super::dissolution::seal_dissolved(&rumor, community.id(), &me, nostr_sdk::prelude::Timestamp::from_secs(8_000)).unwrap();
        let _ = relay.publish(&tombstone, &community.relays).await;
        assert!(!crate::db::community::get_community_dissolved(&crate::simd::hex::bytes_to_hex_32(&community.id().0)).unwrap(), "not yet locally sealed");

        let rec = Arc::new(Recorder::default());
        let session = SessionGuard::capture();
        clear().await;
        // Fire the SAME tombstone twice AND a fresh re-wrap of its verified seal
        // (distinct outer id) — death must be announced exactly once.
        dispatch_event(&session, tombstone.clone(), rec.clone()).await;
        dispatch_event(&session, tombstone, rec.clone()).await;
        assert!(crate::db::community::get_community_dissolved(&crate::simd::hex::bytes_to_hex_32(&community.id().0)).unwrap());

        // A member posts a fresh message to #general AFTER the tombstone. It must
        // not be honored (the community is excluded from load_held_v2, so its
        // plane is NotOurs).
        let member = Keys::generate();
        let cgroup = derive::channel_group_key(&community.community_root, &general, community.root_epoch);
        let rumor = super::super::chat::build_message_rumor(member.public_key(), &general, community.root_epoch, "into the grave", None, &[], vec![], 9_000);
        let (mw, _) = super::super::chat::seal_chat_rumor(&rumor, &cgroup, &member, nostr_sdk::prelude::Timestamp::from_secs(9), false).unwrap();
        dispatch_event(&session, mw, rec.clone()).await;

        assert_eq!(rec.deaths.lock().unwrap().len(), 1, "death is announced exactly once");
        assert!(rec.messages.lock().unwrap().is_empty(), "a post-tombstone message is never honored (CORD-02 §9)");
    }

    #[test]
    fn a_private_channel_subscribes_to_its_own_chat_plane() {
        let mut c = a_community("Priv");
        c.channels.push(super::super::community::ChannelV2 {
            id: crate::community::ChannelId([0x33; 32]),
            name: "mods".into(),
            private: true,
            key: Some([0x44; 32]),
            epoch: Epoch(1),
            voice: None,
            meta_custom: None,
            meta_extra: Default::default(),
        });
        let authors = plane_authors(std::slice::from_ref(&c));
        // A private channel is read under its OWN key/epoch (not the root).
        let priv_chat = derive::channel_group_key(&[0x44; 32], &c.channels[1].id, Epoch(1)).pk();
        assert!(authors.contains(&priv_chat), "a private channel subscribes to its own chat plane");
        // Its next-rekey address IS subscribed (rekey-follow), keyed by the current
        // root at the channel's next epoch — so a rotation is delivered.
        let next_rekey = derive::channel_rekey_group_key(&c.community_root, &c.channels[1].id, Epoch(2)).pk();
        assert!(authors.contains(&next_rekey), "a private channel's next rekey plane is subscribed");
    }
}
