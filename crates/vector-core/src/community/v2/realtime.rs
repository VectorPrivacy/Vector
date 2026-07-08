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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use nostr_sdk::prelude::{Client, Event, Filter, Kind, PublicKey, RelayStatus, RelayUrl, SubscriptionId};
use tokio::sync::Mutex;

use super::community::CommunityV2;
use super::stream;
use super::{derive, inbound};
use crate::community::{ConcordProtocol, Epoch};
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
/// Serializes + coalesces the combined control+rekey follow, keyed by
/// `community_id_hex`. ONE follow runs per community at a time (control and rekey
/// share this key, so they can't concurrently whole-row-save and clobber each
/// other), and a trigger arriving while one is in flight sets the rerun flag so
/// the burst's final state is still captured. This also bounds the amplification
/// DoS: any community_root holder can sign a junk wrap at a plane address
/// (recognition is address-only), but a flood collapses to one in-flight follow.
/// Cleared on session swap.
static V2_FOLLOW_GATE: LazyLock<Mutex<HashMap<String, bool>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

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
    V2_FOLLOW_GATE.lock().await.clear();
}

/// Every plane pubkey a set of v2 communities publishes under that
/// [`inbound::dispatch_wrap`] handles — the subscription author-set. Per
/// community: the guestbook, the control plane, and each channel's current
/// Chat-Plane address. Pure + deterministic (deduped, sorted) — the testable core.
///
/// The **control plane** rides here so a long-running bot follows metadata +
/// public-channel edits live ([`super::service::follow_control`] re-folds on a
/// recognized wrap).
///
/// **Deliberately NOT subscribed yet:** the next base-rekey and per-channel
/// next-rekey addresses. Those wraps need the rekey catch-up (adopt-forward /
/// removal), so subscribing them without that arm would only deliver events the
/// dispatcher drops. When rekey-follow lands, add its authors HERE together with
/// its `dispatch_wrap` arm — never one without the other.
pub fn plane_authors(communities: &[CommunityV2]) -> Vec<PublicKey> {
    let mut out = Vec::new();
    for c in communities {
        out.push(derive::guestbook_group_key(&c.community_root, c.id(), c.root_epoch).pk());
        out.push(control_author(c));
        for ch in &c.channels {
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

/// Load every locally-held **v2** community (dispatching each id by its stored
/// protocol). The realtime layer folds these into the subscription + routing.
pub fn load_held_v2() -> Vec<CommunityV2> {
    let ids = crate::db::community::list_community_ids().unwrap_or_default();
    ids.iter()
        .filter(|id| matches!(crate::db::community::community_protocol(id).ok().flatten(), Some(ConcordProtocol::V2)))
        .filter_map(|id| crate::db::community::load_community_v2(id).ok().flatten())
        .collect()
}

/// Refresh the v2 subscription for the held communities: register
/// `{kinds:[1059,21059], authors:[…]}` on their relays (targeted + pool-wide,
/// mirroring v1). Idempotent on an unchanged author-set.
pub async fn refresh_subscription(client: &Client) {
    let communities = load_held_v2();
    let authors = plane_authors(&communities);
    let mut relays: Vec<String> = communities.iter().flat_map(|c| c.relays.iter().cloned()).collect();
    relays.sort();
    relays.dedup();

    let mut new_set: Vec<String> = authors.iter().map(|p| p.to_hex()).collect();
    new_set.sort();

    let mut sub_guard = V2_SUB_ID.lock().await;
    let mut set_guard = V2_SUB_SET.lock().await;
    if sub_guard.is_some() && *set_guard == new_set {
        return; // unchanged — the pool re-applies the live sub across reconnects.
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

    // Community relays ride GOSSIP|PING (warm but excluded from pool-wide DM ops).
    for r in &relays {
        let _ = client.pool().add_relay(r.as_str(), crate::community_relay_options()).await;
    }
    client.connect().await;
    // Wait briefly for at least one relay to actually connect (a subscribe against
    // a still-connecting relay silently fails to register — same trap as v1).
    {
        let wanted: Vec<RelayUrl> = relays.iter().filter_map(|r| RelayUrl::parse(r).ok()).collect();
        for _ in 0..24 {
            let pool = client.pool().all_relays().await;
            if wanted.iter().any(|u| pool.get(u).map(|r| r.status() == RelayStatus::Connected).unwrap_or(false)) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
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
            // A control OR a rekey wrap both drive the SAME combined per-community
            // follow — one gate key serializes them so they can't concurrently
            // whole-row-save and clobber each other (a rekey-adopted root reverted by
            // a stale control save, or a self-removed community resurrected by one).
            inbound::DispatchedV2::Control { community_id } | inbound::DispatchedV2::Rekey { community_id } => {
                // Spawn off the dispatch hot path: the follow does network fetches and
                // this dispatch is awaited inline in the single notification loop, so
                // awaiting here would let a flood of (address-recognized, unopened) junk
                // wraps head-of-line-block every DM + community.
                let handler = handler.clone();
                let bg = SessionGuard::capture();
                tokio::spawn(async move {
                    if !bg.is_valid() {
                        return;
                    }
                    follow_and_refresh(&bg, &community_id, &*handler).await;
                });
                return;
            }
            _ => return, // chat/guestbook handled inline by the dispatcher.
        }
    }
}

/// Claim the follow slot for `community_id`. True → run the follow now; false →
/// one is already in flight (a trailing rerun is requested so the burst's final
/// state is still captured).
async fn acquire_follow(community_id: &str) -> bool {
    let mut gate = V2_FOLLOW_GATE.lock().await;
    if gate.contains_key(community_id) {
        gate.insert(community_id.to_string(), true);
        false
    } else {
        gate.insert(community_id.to_string(), false);
        true
    }
}

/// End a follow iteration. `force` releases and stops unconditionally. Otherwise:
/// if a rerun was requested while this iteration ran, reset the flag and return
/// true (caller loops on fresh state); else release and return false. Every exit
/// path of a follow MUST reach this (with `force` on early breaks) or the slot leaks.
async fn finish_follow(community_id: &str, force: bool) -> bool {
    let mut gate = V2_FOLLOW_GATE.lock().await;
    if force {
        gate.remove(community_id);
        return false;
    }
    match gate.get(community_id) {
        Some(true) => {
            gate.insert(community_id.to_string(), false);
            true
        }
        _ => {
            gate.remove(community_id);
            false
        }
    }
}

/// The combined per-community follow: on any control/rekey wrap, run the rekey
/// catch-up THEN the control re-fold over a live transport, each against the
/// FRESHLY-RELOADED persisted community (never a stale dispatch-time clone, so two
/// planes can't lose each other's writes). Rekey runs first: a base adopt moves the
/// control address, and a self-removal tears the community down (skipping control).
/// Serialized + coalesced via [`V2_FOLLOW_GATE`]. No-op without a live client —
/// unit tests drive `service::follow_control` / `follow_rekeys` directly.
async fn follow_and_refresh(session: &SessionGuard, community_id: &str, handler: &dyn InboundEventHandler) {
    let Some(id) = crate::simd::hex::hex_to_bytes_32_checked(community_id).map(crate::community::CommunityId) else {
        return;
    };
    if crate::state::nostr_client().is_none() {
        return;
    }
    if !acquire_follow(community_id).await {
        return; // already in flight — a rerun was requested for this burst.
    }
    loop {
        if follow_once(session, &id, community_id, handler).await == FollowOnce::SessionGone {
            finish_follow(community_id, true).await;
            return;
        }
        if !finish_follow(community_id, false).await {
            break;
        }
        // A rerun was requested mid-flight — the next iteration reloads fresh state.
    }
}

#[derive(PartialEq)]
enum FollowOnce {
    Done,
    SessionGone,
}

/// One rekey-then-control pass against the freshly-reloaded community.
async fn follow_once(
    session: &SessionGuard,
    id: &crate::community::CommunityId,
    community_id: &str,
    handler: &dyn InboundEventHandler,
) -> FollowOnce {
    let Some(client) = crate::state::nostr_client() else {
        return FollowOnce::Done;
    };
    let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(12));

    // Rekey first (fresh DB state): a base adopt moves the control address, and a
    // self-removal tears the community down before control runs.
    let Ok(Some(current)) = crate::db::community::load_community_v2(id) else {
        return FollowOnce::Done; // community gone (left / removed).
    };
    match super::service::follow_rekeys(&transport, &current, session).await {
        Ok(follow) if follow.self_removed => {
            if !session.is_valid() {
                return FollowOnce::SessionGone;
            }
            let _ = crate::db::community::delete_community(community_id);
            refresh_subscription(&client).await;
            handler.on_community_self_removed(community_id);
            return FollowOnce::Done;
        }
        Ok(follow) if follow.updated.is_some() => {
            if !session.is_valid() {
                return FollowOnce::SessionGone;
            }
            refresh_subscription(&client).await;
            handler.on_community_refreshed(community_id);
        }
        Ok(_) => {}
        Err(_) => return FollowOnce::Done,
    }

    // Control second, on the (possibly new-root) freshly-reloaded state.
    let Ok(Some(current)) = crate::db::community::load_community_v2(id) else {
        return FollowOnce::Done;
    };
    if let Ok(Some(_)) = super::service::follow_control(&transport, &current, session).await {
        if !session.is_valid() {
            return FollowOnce::SessionGone;
        }
        refresh_subscription(&client).await;
        handler.on_community_refreshed(community_id);
    }
    FollowOnce::Done
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
        assert!(authors.contains(&gb) && authors.contains(&control) && authors.contains(&general) && authors.contains(&next_base));
        assert_eq!(authors.len(), 4, "guestbook + control + chat + base-rekey planes are subscribed");
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

        // Create a v2 community (persisted), post a message, and grab the raw wrap.
        let relay = MemoryRelay::new();
        let community = super::super::service::create_community(&relay, "Live", vec!["wss://r".into()], None).await.unwrap();
        let general = community.channels[0].id;
        super::super::service::send_message(&relay, &community, &general, "live ping").await.unwrap();
        let author = derive::channel_group_key(&community.community_root, &general, community.root_epoch).pk_hex();
        let q = Query { kinds: vec![stream::KIND_WRAP], authors: vec![author], ..Default::default() };
        let wrap = relay.fetch(&q, &community.relays).await.unwrap().into_iter().next().unwrap();

        // The realtime dispatch (loading held v2 communities from the DB) routes it.
        // Dispatch the SAME wrap TWICE — modelling the pool re-delivering it under
        // the targeted + pool-wide subs (and from multiple relays). The handler
        // must fire EXACTLY ONCE (no duplicate bot replies).
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
    async fn follow_gate_coalesces_a_burst_and_runs_one_trailing_rerun() {
        clear().await;
        let cid = "cid";
        // First trigger claims the slot; concurrent triggers during the run (control
        // OR rekey — one key per community) are coalesced into a single trailing
        // rerun, bounding the fan-out AND serializing the two planes.
        assert!(acquire_follow(cid).await, "first trigger runs the follow");
        assert!(!acquire_follow(cid).await, "a trigger while in-flight is coalesced");
        assert!(!acquire_follow(cid).await, "a whole burst collapses to one rerun");
        // The in-flight follow ends: a rerun was requested → loop once more.
        assert!(finish_follow(cid, false).await, "a requested rerun makes the follow loop");
        // No new trigger during the rerun → the next finish releases the slot.
        assert!(!finish_follow(cid, false).await, "no further trigger → release");
        // Released: a fresh trigger can claim it again (no leak).
        assert!(acquire_follow(cid).await, "the released slot is re-acquirable");
        finish_follow(cid, true).await; // clean up
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
