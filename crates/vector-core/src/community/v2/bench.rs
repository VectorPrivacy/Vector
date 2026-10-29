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
            service::fetch_channel_history_paged(&relay, &c, &channel, 200, n / 200 + 2, None, None, crate::community::transport::Evidence::Quorum, |_, _| true, |_| Default::default())
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
