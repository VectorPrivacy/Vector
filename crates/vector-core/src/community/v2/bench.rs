//! Concord timings for `cargo bench` (benches/core): the live dispatch path a wrap takes from the
//! relay pool to the database, and what it costs at a realistic number of held communities.

use std::hint::black_box;
use std::sync::Arc;

use nostr_sdk::prelude::{Event, Keys, PublicKey, ToBech32};

use super::community::CommunityV2;
use super::{derive, realtime, service, stream};
use crate::community::transport::memory::MemoryRelay;
use crate::community::{ChannelId, Epoch};
use crate::rt::time_std::Instant;

struct Quiet;
impl crate::event_handler::InboundEventHandler for Quiet {}

fn as_identity(keys: &Keys) {
    crate::state::MY_SECRET_KEY.store_from_keys(keys, &[]);
    crate::state::set_my_public_key(keys.public_key());
}

fn report(name: &str, per_op: std::time::Duration) {
    let us = per_op.as_secs_f64() * 1e6;
    if us >= 1000.0 {
        println!("{name:<58} {:>10.2} ms", us / 1000.0);
    } else {
        println!("{name:<58} {us:>10.2} µs");
    }
}

/// Best of five rounds of `n` calls.
fn time(name: &str, n: u32, mut f: impl FnMut()) {
    f();
    let mut best = std::time::Duration::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        for _ in 0..n {
            f();
        }
        best = best.min(t.elapsed() / n);
    }
    report(name, best);
}

/// Every relay asked on its own, as the live transport does: what the wire carries.
struct Metered<'a> {
    relay: &'a MemoryRelay,
    reqs: std::sync::atomic::AtomicU64,
    events: std::sync::atomic::AtomicU64,
    bytes: std::sync::atomic::AtomicU64,
    /// Reconciliations, and the bytes their messages took both ways.
    reconciles: std::sync::atomic::AtomicU64,
}

impl<'a> Metered<'a> {
    fn new(relay: &'a MemoryRelay) -> Self {
        Metered { relay, reqs: Default::default(), events: Default::default(), bytes: Default::default(), reconciles: Default::default() }
    }

    fn header() {
        println!("{:<52} {:>5} {:>5} {:>7} {:>11} {:>9}", "", "REQs", "NEGs", "events", "bytes", "CPU ms");
    }

    /// One row for what was asked since the last, `ours` being the time spent this side of the
    /// wire and `receive` what the relay pool spends on each event it is sent.
    fn row(&self, name: &str, ours: std::time::Duration, receive: std::time::Duration) {
        use std::sync::atomic::Ordering::Relaxed;
        let (reqs, events, bytes, negs) = (self.reqs.swap(0, Relaxed), self.events.swap(0, Relaxed), self.bytes.swap(0, Relaxed), self.reconciles.swap(0, Relaxed));
        let cpu = (ours + receive * events as u32).as_secs_f64() * 1e3;
        println!("{name:<52} {reqs:>5} {negs:>5} {events:>7} {bytes:>11} {cpu:>9.1}");
    }
}

/// The NIP-77 exchange itself, both ends run here, so what it puts on the wire is counted.
#[async_trait::async_trait]
impl crate::community::transport::Reconcile for Metered<'_> {
    async fn reconcile(&self, query: &crate::community::transport::Query, held: &[(nostr_sdk::prelude::EventId, u64)], relay: &str, budget: usize) -> Result<crate::community::transport::Diff, crate::community::transport::ReconcileFail> {
        use crate::community::transport::Transport;
        use negentropy::{Id, Negentropy, NegentropyStorageVector};
        use std::sync::atomic::Ordering::Relaxed;
        let theirs = self.relay.fetch(query, std::slice::from_ref(&relay.to_string())).await.unwrap_or_default();
        let side = |items: &mut dyn Iterator<Item = ([u8; 32], u64)>| {
            let mut storage = NegentropyStorageVector::new();
            for (id, at) in items {
                storage.insert(at, Id::from_byte_array(id)).unwrap();
            }
            storage.seal().unwrap();
            Negentropy::owned(storage, 60_000).unwrap()
        };
        let (mut ours, opening) = crate::community::plane_neg::open(held).unwrap();
        let mut relay_side = side(&mut theirs.iter().map(|e| (e.id.to_bytes(), e.created_at.as_secs())));
        let mut diff = crate::community::transport::Diff::default();
        // ["NEG-OPEN", id, filter, hex] out, then ["NEG-MSG", id, hex] each way.
        let mut wire = 160u64;
        let mut message = Some(opening);
        while let Some(out) = message.take() {
            let answer = relay_side.reconcile(&out).unwrap();
            wire += (out.len() + answer.len()) as u64 * 2 + 2 * 80;
            let (mut have, mut need) = (Vec::new(), Vec::new());
            message = ours.reconcile_with_ids(&answer, &mut have, &mut need).unwrap();
            diff.lack.extend(have.into_iter().map(|id| nostr_sdk::prelude::EventId::from_byte_array(id.to_bytes())));
            diff.need.extend(need.into_iter().map(|id| nostr_sdk::prelude::EventId::from_byte_array(id.to_bytes())));
            if diff.need.len() >= budget {
                break;
            }
        }
        diff.complete = message.is_none() && diff.need.len() <= budget;
        diff.need.truncate(budget);
        self.reconciles.fetch_add(1, Relaxed);
        self.bytes.fetch_add(wire, Relaxed);
        Ok(diff)
    }
}

#[async_trait::async_trait]
impl crate::community::transport::Transport for Metered<'_> {
    async fn publish(&self, e: &Event, r: &[String]) -> Result<(), String> {
        self.relay.publish(e, r).await
    }
    async fn publish_durable(&self, e: &Event, r: &[String]) -> Result<(), String> {
        self.relay.publish_durable(e, r).await
    }
    async fn fetch(&self, q: &crate::community::transport::Query, relays: &[String]) -> Result<Vec<Event>, String> {
        use std::sync::atomic::Ordering::Relaxed;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        let mut floor = crate::community::transport::PageFloor::of(q);
        for r in relays {
            let evs = self.relay.fetch(q, std::slice::from_ref(r)).await?;
            self.reqs.fetch_add(1, Relaxed);
            self.events.fetch_add(evs.len() as u64, Relaxed);
            // Each event arrives in a ["EVENT","<sub id>",…] frame.
            self.bytes.fetch_add(evs.iter().map(|e| e.as_json().len() as u64 + 30).sum(), Relaxed);
            if let Some(floor) = floor.as_mut() {
                floor.note(&evs);
            }
            out.extend(evs.into_iter().filter(|e| seen.insert(e.id)));
        }
        if let Some(floor) = &floor {
            floor.cut(&mut out);
        }
        Ok(out)
    }
    async fn fetch_plane(&self, _p: &Keys, q: &crate::community::transport::Query, r: &[String]) -> Result<crate::community::transport::PlaneFetch, String> {
        Ok(crate::community::transport::PlaneFetch::whole(self.fetch(q, r).await?))
    }
    fn reconciler(&self) -> Option<&dyn crate::community::transport::Reconcile> {
        Some(self)
    }
}

/// `communities` held communities of `channels` public channels each, on one relay.
async fn hold(relay: &MemoryRelay, communities: usize, channels: usize) -> Vec<CommunityV2> {
    let mut out = Vec::new();
    for i in 0..communities {
        let c = service::create_community(relay, &format!("Bench {i}"), vec!["wss://r".into()], None).await.unwrap();
        for j in 1..channels {
            let held = crate::db::community::load_community_v2(c.id()).unwrap().unwrap();
            service::create_public_channel(relay, &held, &format!("ch{j}")).await.unwrap();
        }
        out.push(crate::db::community::load_community_v2(c.id()).unwrap().unwrap());
    }
    out
}

/// `n` wraps posted to `channel` by `author` (a peer: nothing of theirs is stored here), each
/// with its own outer id.
fn wraps(c: &CommunityV2, channel: &ChannelId, author: &Keys, n: usize, typing: bool) -> Vec<Event> {
    let (secret, epoch) = c.channel_secret(c.channel(channel).unwrap());
    let group = derive::channel_group_key(&secret, channel, epoch);
    let base = crate::clock::now_secs() * 1000;
    (0..n)
        .map(|i| {
            let at = base + i as u64;
            let rumor = if typing {
                super::chat::build_typing_rumor(author.public_key(), channel, epoch, at)
            } else {
                super::chat::build_message_rumor(author.public_key(), channel, epoch, &format!("message {i}, a few words long"), None, &[], vec![], at)
            };
            super::chat::seal_chat_rumor(&rumor, &group, author, nostr_sdk::prelude::Timestamp::from_secs(at / 1000), typing).unwrap().0
        })
        .collect()
}

pub fn live_dispatch() {
    let (communities, channels) = std::env::var("CONCORD_HELD").ok().and_then(|v| {
        let (a, b) = v.split_once('x')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    }).unwrap_or((30usize, 15usize));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let me = Keys::generate();
        as_identity(&me);
        crate::state::ENCRYPTION_KEY.set([0x55u8; 32], &[]);
        crate::state::set_encryption_enabled(true);
        let relay = MemoryRelay::new();
        let t = Instant::now();
        let held = hold(&relay, communities, channels).await;
        println!("setup: {communities} communities x {channels} channels in {:.1}s", t.elapsed().as_secs_f64());

        time("load_held_v2 (every row, as the 2s TTL expires)", 20, || {
            black_box(realtime::load_held_v2());
        });
        let one = held[0].id();
        time("load_community_v2, one community", 200, || {
            black_box(crate::db::community::load_community_v2(one).unwrap());
        });
        time("list_community_ids", 200, || {
            black_box(crate::db::community::list_community_ids().unwrap());
        });
        let snapshot = realtime::load_held_v2();
        let author = snapshot[17].channels[9].id;
        time("route table build over the held set (memo warm)", 20, || {
            let fresh = realtime::HeldV2::for_bench(snapshot.clone());
            black_box(fresh.route(&me.public_key()));
        });
        let addresses: usize = snapshot.iter().map(realtime::bench_addresses).sum();
        println!("plane addresses routed: {addresses}");
        let secret = [7u8; 32];
        let mut n = 0u64;
        time("group key derivation, cold (HKDF + 2 EC multiplies)", 200, || {
            n += 1;
            black_box(derive::channel_group_key(&secret, &author, Epoch(n)));
        });
        time("group key derivation, memo hit", 2000, || {
            black_box(derive::channel_group_key(&secret, &author, Epoch(1)));
        });

        let target = &held[17];
        let channel = target.channels[9].id;
        let other = Keys::generate();
        let handler: Arc<dyn crate::event_handler::InboundEventHandler> = Arc::new(Quiet);

        // A sampling profiler's window: warm message dispatch, nothing else, for that many seconds.
        if let Some(secs) = std::env::var("CONCORD_LOOP").ok().and_then(|v| v.parse::<u64>().ok()) {
            let batch = wraps(target, &channel, &other, 30_000, false);
            println!("LOOP START");
            let t = Instant::now();
            for e in batch {
                if t.elapsed().as_secs() >= secs {
                    break;
                }
                if realtime::bench_cache_age_secs() > 1 {
                    let _ = realtime::load_held_v2_cached();
                }
                realtime::dispatch_event(e, Arc::clone(&handler)).await;
            }
            return;
        }
        for (label, typing) in [("typing (21059)", true), ("message (1059)", false)] {
            let batch = wraps(target, &channel, &other, 400, typing);
            let (warm, cold) = batch.split_at(200);
            let mut it = warm.iter();
            let t = Instant::now();
            let _ = realtime::load_held_v2_cached();
            for e in it.by_ref() {
                realtime::dispatch_event(e.clone(), Arc::clone(&handler)).await;
            }
            report(&format!("dispatch_event {label}, held cache warm"), t.elapsed() / warm.len() as u32);
            let t = Instant::now();
            for e in cold {
                realtime::invalidate_held_v2_cache();
                realtime::dispatch_event(e.clone(), Arc::clone(&handler)).await;
            }
            report(&format!("dispatch_event {label}, held cache expired"), t.elapsed() / cold.len() as u32);
            let again = wraps(target, &channel, &other, 200, typing);
            let target_hex = target.id().to_hex();
            let t = Instant::now();
            for e in &again {
                realtime::invalidate_held_v2_community(&target_hex);
                realtime::dispatch_event(e.clone(), Arc::clone(&handler)).await;
            }
            report(&format!("dispatch_event {label}, after one community's write"), t.elapsed() / again.len() as u32);
        }

        // The open alone, for scale.
        let batch = wraps(target, &channel, &other, 50, false);
        let (secret, epoch) = target.channel_secret(target.channel(&channel).unwrap());
        let group = derive::channel_group_key(&secret, &channel, epoch);
        let mut i = 0;
        time("stream::open_wrap of one chat wrap", 200, || {
            i = (i + 1) % batch.len();
            black_box(stream::open_wrap(&batch[i], &group).unwrap());
        });
    });
}

/// `n` peer messages on `channel`, `authors` of them taking turns, a second apart and done by now.
fn history(c: &CommunityV2, channel: &ChannelId, authors: &[Keys], n: usize) -> Vec<Event> {
    let (secret, epoch) = c.channel_secret(c.channel(channel).unwrap());
    let group = derive::channel_group_key(&secret, channel, epoch);
    let start = (crate::clock::now_secs() - n as u64 - 60) * 1000;
    (0..n)
        .map(|i| {
            let author = &authors[i % authors.len()];
            let at = start + i as u64 * 1000 + (i as u64 * 7) % 1000;
            let rumor = super::chat::build_message_rumor(author.public_key(), channel, epoch, &format!("history message {i} with a little more text in it"), None, &[], vec![], at);
            super::chat::seal_chat_rumor(&rumor, &group, author, nostr_sdk::prelude::Timestamp::from_secs(at / 1000), false).unwrap().0
        })
        .collect()
}

/// A catch-up of `CONCORD_HISTORY` (default 2000) peer messages: the relay walk that opens them,
/// then the page ingest that stores them, then the same pages again as duplicates.
pub fn history_catchup() {
    let n: usize = std::env::var("CONCORD_HISTORY").ok().and_then(|v| v.parse().ok()).unwrap_or(2000);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let me = Keys::generate();
        as_identity(&me);
        crate::state::ENCRYPTION_KEY.set([0x55u8; 32], &[]);
        crate::state::set_encryption_enabled(true);
        let relay = MemoryRelay::new();
        let c = hold(&relay, 1, 1).await.remove(0);
        let channel = c.channels[0].id;
        let channel_hex = channel.to_hex();
        let authors: Vec<Keys> = (0..20).map(|_| Keys::generate()).collect();
        for e in history(&c, &channel, &authors, n) {
            relay.inject(&e, &c.relays);
        }
        let walk = || async {
            service::fetch_channel_history_paged(&relay, &c, &channel, 200, n / 200 + 2, None, None, crate::community::transport::Evidence::Quorum, |_, _, _| true, |_| Default::default())
                .await
                .unwrap()
                .0
        };
        let t = Instant::now();
        let opened = walk().await;
        assert_eq!(opened.len(), n);
        report(&format!("fetch_channel_history_paged, {n} wraps, per wrap"), t.elapsed() / n as u32);
        let loops: u64 = std::env::var("CONCORD_LOOP").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        let t = Instant::now();
        while t.elapsed().as_secs() < loops {
            black_box(walk().await);
        }
        let session = crate::db::current_session();
        if std::env::var("CONCORD_INGEST_LOOP").is_ok() {
            // A sampling profiler's window: new messages ingested, page after page.
            println!("LOOP START");
            let t = Instant::now();
            let mut round = 0u64;
            while t.elapsed().as_secs() < 10 {
                round += 1;
                let fresh: Vec<_> = history(&c, &channel, &authors, 400)
                    .into_iter()
                    .filter_map(|w| super::chat::open_chat_event(&w, &derive::channel_group_key(&c.channel_secret(c.channel(&channel).unwrap()).0, &channel, c.channel_secret(c.channel(&channel).unwrap()).1), &channel, c.channel_secret(c.channel(&channel).unwrap()).1).ok().map(|event| service::FetchedEvent { event, epoch: c.channel_secret(c.channel(&channel).unwrap()).1, wrap: Some(w) }))
                    .collect();
                for page in fresh.chunks(200) {
                    let page: Vec<_> = page.iter().map(|f| service::FetchedEvent { event: f.event.clone(), epoch: f.epoch, wrap: f.wrap.clone() }).collect();
                    crate::VectorCore::v2_ingest_chat_page(&channel_hex, me.public_key(), session.clone(), page).await;
                }
                let _ = round;
            }
            return;
        }
        let t = Instant::now();
        let mut new = 0;
        for page in opened.chunks(200) {
            let page: Vec<_> = page.iter().map(|f| service::FetchedEvent { event: f.event.clone(), epoch: f.epoch, wrap: f.wrap.clone() }).collect();
            new += crate::VectorCore::v2_ingest_chat_page(&channel_hex, me.public_key(), session.clone(), page).await;
        }
        assert_eq!(new, n);
        report("v2_ingest_chat_page, new messages, per message", t.elapsed() / n as u32);
        let t = Instant::now();
        for page in opened.chunks(200) {
            let page: Vec<_> = page.iter().map(|f| service::FetchedEvent { event: f.event.clone(), epoch: f.epoch, wrap: f.wrap.clone() }).collect();
            crate::VectorCore::v2_ingest_chat_page(&channel_hex, me.public_key(), session.clone(), page).await;
        }
        report("v2_ingest_chat_page, all held already, per message", t.elapsed() / n as u32);
    });
}

/// One control follow of a Roles v2 community holding `CONCORD_ROSTER` (default `20x300`) roles and
/// granted members, a banlist and a few channels: the plane read with its folds, then each fold.
pub fn control_fold() {
    let (roles, members) = std::env::var("CONCORD_ROSTER").ok().and_then(|v| {
        let (a, b) = v.split_once('x')?;
        Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
    }).unwrap_or((20, 300));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let me = Keys::generate();
        as_identity(&me);
        crate::state::ENCRYPTION_KEY.set([0x55u8; 32], &[]);
        crate::state::set_encryption_enabled(true);
        let relay = MemoryRelay::new();
        let t = Instant::now();
        let c = hold(&relay, 1, 8).await.remove(0);
        let held = || crate::db::community::load_community_v2(c.id()).unwrap().unwrap();
        service::activate_roles_v2(&relay, &held(), 0).await.unwrap();
        // With CONCORD_ADMIN, an admin (not the owner) writes every grant, citing their own.
        let admin = std::env::var("CONCORD_ADMIN").is_ok().then(Keys::generate);
        if let Some(admin) = &admin {
            let role = service::create_role_v2(&relay, &held(), "Admin", 0xff0000, crate::community::roles::Permissions::MANAGE_ROLES | crate::community::roles::Permissions::BAN).await.unwrap();
            service::set_member_roles(&relay, &held(), &admin.public_key(), vec![role]).await.unwrap();
            let _ = service::follow_control(&relay, &held()).await;
        }
        let mut ids = Vec::new();
        for r in 0..roles {
            ids.push(service::create_role_v2(&relay, &held(), &format!("role {r}"), 0x55aa00, 1 << (r % 8)).await.unwrap());
        }
        let banned: Vec<String> = (0..40).map(|_| Keys::generate().public_key().to_hex()).collect();
        if let Some(admin) = &admin {
            as_identity(admin);
        }
        for m in 0..members {
            let member = Keys::generate().public_key();
            service::set_member_roles(&relay, &held(), &member, vec![ids[m % ids.len()].clone()]).await.unwrap();
        }
        as_identity(&me);
        service::set_banlist(&relay, &held(), &banned).await.unwrap();
        println!("setup: {roles} roles, {members} grants in {:.1}s", t.elapsed().as_secs_f64());
        let mut best = (std::time::Duration::MAX, std::time::Duration::MAX, std::time::Duration::MAX, 0);
        for _ in 0..5 {
            let r = service::bench_control_stages(&relay, &held()).await;
            best = (best.0.min(r.0), best.1.min(r.1), best.2.min(r.2), r.3);
        }
        println!("editions read: {}", best.3);
        report("read_control (open every edition + fold per page)", best.0);
        report("fold_authority", best.1);
        report("apply_control_fold", best.2);
        let mut follow = std::time::Duration::MAX;
        for _ in 0..5 {
            let t = Instant::now();
            black_box(service::follow_control(&relay, &held()).await.unwrap());
            follow = follow.min(t.elapsed());
        }
        report("follow_control (read, fold, persist floors and roster)", follow);
        let cid = c.id().to_hex();
        time("get_community_roles (the roster as stored)", 50, || {
            black_box(crate::db::community::get_community_roles(&cid).unwrap());
        });
        let loops: u64 = std::env::var("CONCORD_LOOP").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        let t = Instant::now();
        while t.elapsed().as_secs() < loops {
            black_box(service::bench_control_stages(&relay, &held()).await);
        }
    });
}

/// `ledger_chain_ok` over Writers ledgers of growing length, read in either order.
pub fn ledger_chain() {
    for n in [64, 256, 1024] {
        for newest_first in [false, true] {
            let best = (0..5).map(|_| service::bench_ledger_chain(n, newest_first)).min().unwrap();
            report(&format!("ledger_chain_ok, {n} versions, {}", if newest_first { "newest first" } else { "oldest first" }), best);
        }
    }
    let loops: u64 = std::env::var("CONCORD_LOOP").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let t = Instant::now();
    while t.elapsed().as_secs() < loops {
        black_box(service::bench_ledger_chain(1024, true));
    }
}

/// A Guestbook sync's wire and CPU cost on a plane of `CONCORD_GUESTBOOK_SYNC` (default 1050)
/// entries held on three relays: the first, whole read, then the reads every later sync makes.
pub fn guestbook_sync() {
    use nostr_sdk::prelude::*;

    let n: u64 = std::env::var("CONCORD_GUESTBOOK_SYNC").ok().and_then(|v| v.parse().ok()).unwrap_or(1050);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let me = Keys::generate();
        as_identity(&me);
        let relay = MemoryRelay::new();
        let relays: Vec<String> = ["wss://a", "wss://b", "wss://c"].map(String::from).to_vec();
        let c = service::create_community(&relay, "Guestbook", relays.clone(), None).await.unwrap();
        let gb = derive::guestbook_group_key(&c.community_root, c.id(), c.root_epoch);
        let join = |at: u64| {
            let k = Keys::generate();
            let rumor = super::guestbook::build_join_rumor(k.public_key(), None, at * 1000);
            super::guestbook::seal_guestbook_rumor(&rumor, &gb, &k, Timestamp::from_secs(at)).unwrap().0
        };
        // Spread over three months, and older than the hour each read looks back over.
        let now = crate::clock::now_secs();
        let held: Vec<Event> = (0..n).map(|i| join(now - 90 * 86_400 + i * (89 * 86_400 / n.max(1)))).collect();
        for w in &held {
            relay.inject(w, &relays);
        }
        // What the relay pool spends on each event it receives: parse and verify.
        let json: Vec<String> = held.iter().take(300).map(|w| w.as_json()).collect();
        let t = Instant::now();
        for j in &json {
            Event::from_json(j).unwrap().verify().unwrap();
        }
        let receive = t.elapsed() / json.len() as u32;
        report("relay pool receive, per event", receive);

        let wire = Metered::new(&relay);
        Metered::header();
        let step = async |name: &str| {
            let t = Instant::now();
            service::sync_guestbook(&wire, &c).await.unwrap();
            wire.row(name, t.elapsed(), receive);
        };
        step(&format!("first sync, {n} entries on 3 relays (whole)")).await;
        step("next sync, nothing new").await;
        for i in 0..3 {
            relay.inject(&join(crate::clock::now_secs() - i), &relays);
        }
        step("next sync, 3 new joins").await;
        step("next sync, nothing new").await;
    });
}

/// A control follow's wire and CPU cost on a plane of `CONCORD_CONTROL_SYNC` (default `20x300`)
/// roles and granted members held on four relays: the first follow of a session, the follows
/// every later one makes, and an admin's whole read.
pub fn control_sync() {
    use crate::community::transport::Transport;
    use nostr_sdk::prelude::*;
    let (roles, members) = std::env::var("CONCORD_CONTROL_SYNC").ok().and_then(|v| {
        let (a, b) = v.split_once('x')?;
        Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
    }).unwrap_or((20, 300));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let me = Keys::generate();
        as_identity(&me);
        crate::state::ENCRYPTION_KEY.set([0x55u8; 32], &[]);
        crate::state::set_encryption_enabled(true);
        let relay = MemoryRelay::new();
        let relays: Vec<String> = ["wss://a", "wss://b", "wss://c", "wss://d"].map(String::from).to_vec();
        let t = Instant::now();
        let c = service::create_community(&relay, "Control", relays.clone(), None).await.unwrap();
        let held = || crate::db::community::load_community_v2(c.id()).unwrap().unwrap();
        service::activate_roles_v2(&relay, &held(), 0).await.unwrap();
        let mut ids = Vec::new();
        for r in 0..roles {
            ids.push(service::create_role_v2(&relay, &held(), &format!("role {r}"), 0x55aa00, 1 << (r % 8)).await.unwrap());
        }
        for m in 0..members {
            let member = Keys::generate().public_key();
            service::set_member_roles(&relay, &held(), &member, vec![ids[m % ids.len()].clone()]).await.unwrap();
        }
        let banned: Vec<String> = (0..40).map(|_| Keys::generate().public_key().to_hex()).collect();
        service::set_banlist(&relay, &held(), &banned).await.unwrap();
        let control = super::control::ControlPlane::of(&held());
        let plane = relay
            .fetch(&crate::community::transport::Query { kinds: vec![stream::KIND_WRAP], authors: vec![control.pk_hex()], ..Default::default() }, &relays[..1])
            .await
            .unwrap();
        println!("setup: {roles} roles, {members} grants, {} wraps on each of {} relays in {:.1}s", plane.len(), relays.len(), t.elapsed().as_secs_f64());
        let json: Vec<String> = plane.iter().take(300).map(|w| w.as_json()).collect();
        let t = Instant::now();
        for j in &json {
            Event::from_json(j).unwrap().verify().unwrap();
        }
        let receive = t.elapsed() / json.len() as u32;
        report("relay pool receive, per event", receive);

        let wire = Metered::new(&relay);
        Metered::header();
        let follow = async |name: &str| {
            let t = Instant::now();
            black_box(service::follow_control(&wire, &held()).await.unwrap());
            wire.row(name, t.elapsed(), receive);
        };
        if service::bench_control_store_on() {
            service::bench_forget_control_wraps(&held());
            super::control::forget_opened();
            follow("first follow ever: nothing held of the plane").await;
        }
        super::control::forget_opened();
        follow("first follow of a session").await;
        follow("next follow, nothing new").await;
        // One wrap from elsewhere: an edition this device has never held.
        let stranger = Keys::generate();
        let rumor = super::control::build_edition_rumor(stranger.public_key(), super::vsk::CHANNEL_METADATA, &[0xAB; 32], 1, None, "{\"name\":\"elsewhere\",\"private\":false}", crate::clock::now_secs(), None);
        let (wrap, _) = super::control::seal_control_edition(&rumor, &control.write_group().unwrap(), &stranger, crate::clock::timestamp()).unwrap();
        relay.inject(&wrap, &relays);
        follow("next follow, one new wrap on the plane").await;
        follow("next follow, nothing new").await;
        let t = Instant::now();
        let read = service::bench_control_whole(&wire, &held()).await;
        wire.row(&format!("an admin's whole read ({read} editions)"), t.elapsed(), receive);
        let t = Instant::now();
        black_box(service::bench_control_whole(&wire, &held()).await);
        wire.row("the same again", t.elapsed(), receive);
    });
}

/// A Guestbook of `CONCORD_GUESTBOOK` (default 5000) joins and leaves: one live event's ingest,
/// the store read alone, and the memberlist a member panel reads.
pub fn guestbook_ingest() {
    use super::guestbook::{GuestbookEntry, GuestbookEvent};
    let n: usize = std::env::var("CONCORD_GUESTBOOK").ok().and_then(|v| v.parse().ok()).unwrap_or(5000);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let me = Keys::generate();
        as_identity(&me);
        crate::state::ENCRYPTION_KEY.set([0x55u8; 32], &[]);
        crate::state::set_encryption_enabled(true);
        let relay = MemoryRelay::new();
        let c = hold(&relay, 1, 1).await.remove(0);
        let cid = c.id().to_hex();
        let base = (crate::clock::now_secs() - 86_400 * 30) * 1000;
        let event = |i: usize| {
            // Distinct members without a key generation per event.
            let mut member = [0u8; 32];
            member[..8].copy_from_slice(&(i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15).to_be_bytes());
            member[8..16].copy_from_slice(&(i as u64).to_le_bytes());
            let member = PublicKey::from_byte_array(member);
            let at_ms = base + i as u64 * 60_000;
            let entry = if i % 4 == 3 { GuestbookEntry::Leave { member, at_ms } } else { GuestbookEntry::Join { member, invited_by: None, at_ms } };
            let mut rumor_id = [0u8; 32];
            rumor_id[..8].copy_from_slice(&(i as u64 + 1).to_le_bytes());
            GuestbookEvent { rumor_id, entry, epoch: Some(c.root_epoch.0), observed_at: Some(at_ms / 1000) }
        };
        let events: Vec<GuestbookEvent> = (0..n).map(event).collect();
        crate::db::community::set_guestbook(&cid, &events, 0).unwrap();
        let mut i = n;
        if std::env::var("CONCORD_LOOP").is_ok() {
            println!("LOOP START");
            let t = Instant::now();
            let only = std::env::var("CONCORD_LOOP").unwrap_or_default();
            if only == "memberlist" {
                while t.elapsed().as_secs() < 10 {
                    black_box(service::stored_memberlist(&c).unwrap());
                }
                return;
            }
            while t.elapsed().as_secs() < 10 {
                i += match only.as_str() {
                    "leave" => 4 - i % 4 + 3,
                    "join" => 4 - i % 4,
                    _ => 1,
                };
                service::ingest_guestbook_event(&c, event(i), crate::clock::now_secs()).unwrap();
            }
            return;
        }
        time(&format!("ingest_guestbook_event into {n} held"), 20, || {
            i += 1;
            service::ingest_guestbook_event(&c, event(i), crate::clock::now_secs()).unwrap();
        });
        time("  a Join (moves no departure)", 20, || {
            i += 4 - i % 4;
            service::ingest_guestbook_event(&c, event(i), crate::clock::now_secs()).unwrap();
        });
        time("  a Leave (one member departs)", 20, || {
            i += 4 - i % 4 + 3;
            service::ingest_guestbook_event(&c, event(i), crate::clock::now_secs()).unwrap();
        });
        time("get_guestbook alone", 20, || {
            black_box(crate::db::community::get_guestbook(&cid).unwrap());
        });
        let (held, cursor) = crate::db::community::get_guestbook(&cid).unwrap();
        time("set_guestbook alone (serialize, seal, write)", 20, || {
            crate::db::community::set_guestbook(&cid, &held, cursor).unwrap();
        });
        time("owe_new_departures", 20, || {
            service::owe_new_departures(&c);
        });
        {
            let _seal = crate::crypto::gate::sealing();
            let (held, _) = crate::db::community::get_guestbook(&cid).unwrap();
            let blob = crate::crypto::maybe_encrypt_text(&serde_json::to_string(&held).unwrap()).unwrap().len();
            let conn = crate::db::get_db_connection_guard_static().unwrap();
            let bodies: i64 = conn.query_row("SELECT SUM(length(body)) FROM community_guestbook_events", [], |r| r.get(0)).unwrap();
            // Each row also carries its rumor id and two small integers, and the dedup index repeats the id.
            let rows = bodies as usize + held.len() * (32 + 6) + held.len() * (32 + 6);
            println!("{n} events at rest: one blob {:.0} KB (rewritten whole per event), rows ~{:.0} KB (one row per event)", blob as f64 / 1024.0, rows as f64 / 1024.0);
        }
        time("stored_memberlist", 20, || {
            black_box(service::stored_memberlist(&c).unwrap());
        });
        // A Channel's history at a large community's scale: posts across a few hundred authors, and
        // the presence events the activity fold parses.
        let posts: usize = std::env::var("CONCORD_POSTS").ok().and_then(|v| v.parse().ok()).unwrap_or(30_000);
        let channel_hex = crate::simd::hex::bytes_to_hex_32(&c.channels[0].id.0);
        let first = crate::types::Message { id: "f".repeat(64), content: "seed".into(), at: base, npub: Some(Keys::generate().public_key().to_bech32().unwrap()), ..Default::default() };
        crate::db::events::save_message(&channel_hex, &first).await.unwrap();
        let chat = crate::db::id_cache::get_chat_id_by_identifier(&channel_hex).unwrap();
        let authors: Vec<String> = (0..300).map(|_| Keys::generate().public_key().to_bech32().unwrap()).collect();
        {
            let conn = crate::db::get_write_connection_guard_static().unwrap();
            let tx = conn.unchecked_transaction().unwrap();
            for i in 0..posts {
                let (kind, tags) = if i % 20 == 0 { (crate::stored_event::event_kind::APPLICATION_SPECIFIC, r#"[["event-type","1"],["d","presence"]]"#) } else { (14, "[]") };
                tx.execute(
                    "INSERT INTO events (id, kind, chat_id, content, tags, created_at, received_at, npub) VALUES (?1, ?2, ?3, '', ?4, ?5, ?5, ?6)",
                    rusqlite::params![format!("{i:064x}"), kind, chat, tags, (base / 1000) as i64 + i as i64, &authors[i % authors.len()]],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        time(&format!("stored_memberlist with {posts} posts by {} authors", authors.len()), 20, || {
            black_box(service::stored_memberlist(&c).unwrap());
        });
        time("  community_member_activity alone", 20, || {
            black_box(crate::db::community::community_member_activity(&cid).unwrap());
        });
    });
}
