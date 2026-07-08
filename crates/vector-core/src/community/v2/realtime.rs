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
}

/// Every plane pubkey a set of v2 communities publishes under — the subscription
/// author-set. Per community: the control plane, the guestbook, the NEXT
/// base-rekey address (so a live base rotation streams), each channel's current
/// Chat-Plane address, and each PRIVATE channel's NEXT rekey address. Pure +
/// deterministic (deduped, sorted) — the testable core.
pub fn plane_authors(communities: &[CommunityV2]) -> Vec<PublicKey> {
    let mut out = Vec::new();
    for c in communities {
        out.push(derive::control_group_key(&c.community_root, c.id(), c.root_epoch).pk());
        out.push(derive::guestbook_group_key(&c.community_root, c.id(), c.root_epoch).pk());
        out.push(derive::base_rekey_group_key(&c.community_root, c.id(), Epoch(c.root_epoch.0.saturating_add(1))).pk());
        for ch in &c.channels {
            let (secret, epoch) = c.channel_secret(ch);
            out.push(derive::channel_group_key(&secret, &ch.id, epoch).pk());
            if ch.private {
                out.push(derive::channel_rekey_group_key(&c.community_root, &ch.id, Epoch(ch.epoch.0.saturating_add(1))).pk());
            }
        }
    }
    out.sort_by_key(|p| p.to_hex());
    out.dedup();
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
    let communities = load_held_v2();
    for c in &communities {
        match inbound::dispatch_wrap(&event, c, &my_pk, &*handler) {
            inbound::DispatchedV2::NotOurs => continue,
            _ => return, // handled by this community's plane.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::control::{genesis, CommunityMetadata};
    use nostr_sdk::prelude::Keys;

    fn a_community(name: &str) -> CommunityV2 {
        let owner = Keys::generate();
        let g = genesis(&owner, CommunityMetadata { name: name.into(), ..Default::default() }, 1_000).unwrap();
        CommunityV2::from_genesis(&g, name, None, vec!["wss://r".into()], 0)
    }

    #[test]
    fn plane_authors_covers_control_guestbook_next_base_rekey_and_channels() {
        let c = a_community("A");
        let authors = plane_authors(std::slice::from_ref(&c));

        // Control, guestbook, next-base-rekey, and the one public channel — 4 planes.
        let control = derive::control_group_key(&c.community_root, c.id(), c.root_epoch).pk();
        let gb = derive::guestbook_group_key(&c.community_root, c.id(), c.root_epoch).pk();
        let next_base = derive::base_rekey_group_key(&c.community_root, c.id(), Epoch(1)).pk();
        let general = {
            let (s, e) = c.channel_secret(&c.channels[0]);
            derive::channel_group_key(&s, &c.channels[0].id, e).pk()
        };
        for pk in [control, gb, next_base, general] {
            assert!(authors.contains(&pk), "author-set must include every plane");
        }
        // A public channel adds no channel-rekey address (public rotates with the base).
        assert_eq!(authors.len(), 4);
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
        let rec = Arc::new(Recorder::default());
        let session = SessionGuard::capture();
        dispatch_event(&session, wrap, rec.clone()).await;

        let got = rec.got.lock().unwrap();
        assert_eq!(got.len(), 1, "the live message reached the handler");
        assert_eq!(got[0].1, "live ping");
        assert_eq!(got[0].0, crate::simd::hex::bytes_to_hex_32(&general.0));
    }

    #[test]
    fn a_private_channel_adds_its_next_rekey_address() {
        let mut c = a_community("Priv");
        c.channels.push(super::super::community::ChannelV2 {
            id: crate::community::ChannelId([0x33; 32]),
            name: "mods".into(),
            private: true,
            key: Some([0x44; 32]),
            epoch: Epoch(1),
        });
        let authors = plane_authors(std::slice::from_ref(&c));
        let next_rekey = derive::channel_rekey_group_key(&c.community_root, &c.channels[1].id, Epoch(2)).pk();
        assert!(authors.contains(&next_rekey), "a private channel subscribes to its next rekey address");
    }
}
