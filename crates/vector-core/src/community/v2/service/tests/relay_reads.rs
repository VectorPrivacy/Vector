//! A Guestbook sync: relays that reconcile asked only for what is not held, the rest read by
//! pages, each from its own cursor.

use super::*;
use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Each relay answered on its own, as the live transport does; the ones in `down` don't answer.
struct Relays<'a> {
    relay: &'a MemoryRelay,
    down: Mutex<HashSet<String>>,
    served: AtomicU64,
    /// Whether the relays compare sets; without it each is read by pages.
    reconciles: bool,
}

impl<'a> Relays<'a> {
    fn new(relay: &'a MemoryRelay) -> Self {
        Relays { relay, down: Mutex::new(HashSet::new()), served: AtomicU64::new(0), reconciles: false }
    }
    fn reconciling(relay: &'a MemoryRelay) -> Self {
        Relays { reconciles: true, ..Relays::new(relay) }
    }
    fn set_down(&self, relays: &[&str]) {
        *self.down.lock().unwrap() = relays.iter().map(|r| r.to_string()).collect();
    }
    /// Events served since the last ask.
    fn served(&self) -> u64 {
        self.served.swap(0, Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl Transport for Relays<'_> {
    async fn publish(&self, e: &Event, r: &[String]) -> Result<(), String> {
        self.relay.publish(e, r).await
    }
    async fn publish_durable(&self, e: &Event, r: &[String]) -> Result<(), String> {
        self.relay.publish_durable(e, r).await
    }
    async fn fetch(&self, q: &Query, relays: &[String]) -> Result<Vec<Event>, String> {
        let up: Vec<String> = relays.iter().filter(|r| !self.down.lock().unwrap().contains(*r)).cloned().collect();
        if up.is_empty() {
            return Err("relay did not answer the fetch".into());
        }
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for r in &up {
            let evs = self.relay.fetch(q, std::slice::from_ref(r)).await?;
            self.served.fetch_add(evs.len() as u64, Ordering::Relaxed);
            out.extend(evs.into_iter().filter(|e| seen.insert(e.id)));
        }
        Ok(out)
    }
    async fn fetch_plane(&self, _p: &Keys, q: &Query, r: &[String]) -> Result<crate::community::transport::PlaneFetch, String> {
        Ok(crate::community::transport::PlaneFetch::whole(self.fetch(q, r).await?))
    }
    fn reconciler(&self) -> Option<&dyn crate::community::transport::Reconcile> {
        self.reconciles.then(|| self.relay.reconciler()).flatten()
    }
}

struct Bed {
    _tmp: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
    relay: MemoryRelay,
    community: CommunityV2,
    gb: GroupKey,
}

const SET: [&str; 3] = ["wss://a", "wss://b", "wss://c"];

async fn bed() -> Bed {
    let (tmp, guard, _owner) = init_test_db();
    let relay = MemoryRelay::new();
    let community = create_community(&relay, "Reads", SET.map(String::from).to_vec(), None).await.unwrap();
    let gb = crate::community::v2::derive::guestbook_group_key(&community.community_root, community.id(), community.root_epoch);
    Bed { _tmp: tmp, _guard: guard, relay, community, gb }
}

/// A Join dated `at` (Unix seconds), on `relays`.
fn join_on(b: &Bed, at: u64, relays: &[&str]) -> Keys {
    let k = Keys::generate();
    let join = guestbook::build_join_rumor(k.public_key(), None, at * 1000);
    let (wrap, _) = guestbook::seal_guestbook_rumor(&join, &b.gb, &k, Timestamp::from_secs(at)).unwrap();
    b.relay.inject(&wrap, &relays.iter().map(|r| r.to_string()).collect::<Vec<_>>());
    k
}

fn holds(b: &Bed, k: &Keys) -> bool {
    let (events, _) = crate::db::community::get_guestbook(&crate::simd::hex::bytes_to_hex_32(&b.community.id().0)).unwrap();
    events.iter().any(|e| matches!(e.entry, guestbook::GuestbookEntry::Join { member, .. } if member == k.public_key()))
}

fn reads(b: &Bed) -> std::collections::BTreeMap<String, super::super::guestbook_reads::RelayRead> {
    super::super::control_sync::relay_reads(&b.community, &super::super::guestbook_store::GuestbookView::of(&b.community))
}

#[tokio::test]
async fn a_quiet_sync_asks_each_relay_only_past_its_cursor() {
    let b = bed().await;
    let now = now_ms() / 1000;
    for i in 0..40 {
        join_on(&b, now - 86_400 + i * 60, &SET);
    }
    let relays = Relays::new(&b.relay);
    sync_guestbook(&relays, &b.community).await.unwrap();
    assert!(relays.served() >= 120, "the first read is whole, on every relay");
    // Two hours on, past the hour each read looks back over.
    crate::clock::shift_for_test(2 * 3_600_000);
    sync_guestbook(&relays, &b.community).await.unwrap();
    relays.served();
    sync_guestbook(&relays, &b.community).await.unwrap();
    assert_eq!(relays.served(), 0, "nothing new: nothing served");
    let fresh = join_on(&b, now_ms() / 1000, &SET);
    sync_guestbook(&relays, &b.community).await.unwrap();
    crate::clock::shift_for_test(0);
    // Each relay hands it over, and once more as its walk finds nothing below it.
    assert_eq!(relays.served(), 6, "one new entry from each relay, and nothing else");
    assert!(holds(&b, &fresh));
}

#[tokio::test]
async fn a_relay_that_was_down_is_read_from_its_own_place_when_back() {
    let b = bed().await;
    let now = now_ms() / 1000;
    let everywhere = join_on(&b, now - 7_200, &SET);
    let only_b = join_on(&b, now - 3 * 86_400, &["wss://b"]);
    let relays = Relays::new(&b.relay);
    relays.set_down(&["wss://b"]);
    sync_guestbook(&relays, &b.community).await.unwrap();
    assert!(holds(&b, &everywhere) && !holds(&b, &only_b));
    assert!(!reads(&b).contains_key("wss://b"), "a relay that didn't answer earns no place");
    assert!(reads(&b).get("wss://a").is_some_and(|r| r.cursor.is_some()));
    relays.set_down(&[]);
    sync_guestbook(&relays, &b.community).await.unwrap();
    assert!(holds(&b, &only_b), "back, it is read whole, and what only it held lands");
}

#[tokio::test]
async fn a_relay_down_after_a_read_keeps_its_cursor() {
    let b = bed().await;
    let relays = Relays::new(&b.relay);
    join_on(&b, now_ms() / 1000 - 600, &SET);
    sync_guestbook(&relays, &b.community).await.unwrap();
    let before = reads(&b)["wss://c"];
    crate::clock::shift_for_test(3_600_000);
    relays.set_down(&["wss://c"]);
    sync_guestbook(&relays, &b.community).await.unwrap();
    crate::clock::shift_for_test(0);
    assert_eq!(reads(&b)["wss://c"], before, "not moved while it didn't answer");
    assert!(reads(&b)["wss://a"].cursor > before.cursor, "the others go on");
}

#[tokio::test]
async fn every_relay_down_is_an_error_not_a_quiet_answer() {
    let b = bed().await;
    let relays = Relays::new(&b.relay);
    relays.set_down(&SET);
    assert!(sync_guestbook(&relays, &b.community).await.is_err());
    assert!(reads(&b).is_empty());
}

#[tokio::test]
async fn an_entry_dated_back_is_read_inside_the_overlap_and_older_ones_by_the_daily_whole_read() {
    let b = bed().await;
    let relays = Relays::new(&b.relay);
    sync_guestbook(&relays, &b.community).await.unwrap();
    let now = now_ms() / 1000;
    let late = join_on(&b, now - 20 * 60, &SET);
    let restored = join_on(&b, now - 30 * 86_400, &SET);
    sync_guestbook(&relays, &b.community).await.unwrap();
    assert!(holds(&b, &late), "published late inside the hour behind the cursor");
    assert!(!holds(&b, &restored), "dated a month back, outside any cursor's window");
    crate::clock::shift_for_test(25 * 3_600_000);
    sync_guestbook(&relays, &b.community).await.unwrap();
    crate::clock::shift_for_test(0);
    assert!(holds(&b, &restored), "the daily whole read finds it");
}

#[tokio::test]
async fn a_read_longer_than_one_pass_goes_on_after_a_restart() {
    let b = bed().await;
    let base = now_ms() / 1000 - 100_000;
    let mut first = None;
    for i in 0..6_100u64 {
        let k = join_on(&b, base + i, &["wss://a"]);
        first.get_or_insert(k);
    }
    let community = CommunityV2 { relays: vec!["wss://a".into()], ..b.community.clone() };
    let relays = Relays::new(&b.relay);
    super::super::control_sync::set_budget_for_test(Some(6_000));
    sync_guestbook(&relays, &community).await.unwrap();
    assert!(guestbook_walk_pending(&community), "one pass's pages fall short");
    let served = relays.served();
    assert!((6_000..6_500).contains(&served), "a pass's worth and no more: {served}");
    forget_session_walks_for_test();
    sync_guestbook(&relays, &community).await.unwrap();
    let again = relays.served();
    assert!(again < 500, "a new session goes on from where the read stopped, not from the top: {again}");
    assert!(!guestbook_walk_pending(&community) && guestbook_walked_this_session(&community));
    assert!(holds(&b, first.as_ref().unwrap()), "down to the oldest");
    super::super::control_sync::set_budget_for_test(None);
}

#[tokio::test]
async fn a_relay_leaving_the_set_takes_its_place_with_it_and_a_new_one_is_read_whole() {
    let b = bed().await;
    let relays = Relays::new(&b.relay);
    let on_d = join_on(&b, now_ms() / 1000 - 5 * 86_400, &["wss://d"]);
    sync_guestbook(&relays, &b.community).await.unwrap();
    let moved = CommunityV2 { relays: vec!["wss://a".into(), "wss://d".into()], ..b.community.clone() };
    sync_guestbook(&relays, &moved).await.unwrap();
    let held = reads(&b);
    assert_eq!(held.keys().cloned().collect::<Vec<_>>(), vec!["wss://a".to_string(), "wss://d".to_string()]);
    assert!(holds(&b, &on_d), "the new relay is read whole");
}

#[tokio::test]
async fn a_refounding_reads_its_new_plane_whole() {
    let b = bed().await;
    let relays = Relays::new(&b.relay);
    sync_guestbook(&relays, &b.community).await.unwrap();
    let mut moved = b.community.clone();
    moved.root_epoch = Epoch(b.community.root_epoch.0 + 1);
    let view = super::super::guestbook_store::GuestbookView::of(&moved);
    assert!(super::super::control_sync::relay_reads(&moved, &view).is_empty(), "no place held on a plane never read");
}

#[tokio::test]
async fn relays_that_reconcile_serve_each_entry_once_and_nothing_on_a_quiet_sync_ever_after() {
    let b = bed().await;
    let now = now_ms() / 1000;
    for i in 0..40 {
        join_on(&b, now - 86_400 + i * 60, &SET);
    }
    let relays = Relays::reconciling(&b.relay);
    let added = sync_guestbook(&relays, &b.community).await.unwrap();
    assert!(added.len() >= 40);
    let first = relays.served();
    assert!(first >= 40 && first < 60, "each wrap once, from one relay of the three: {first}");
    sync_guestbook(&relays, &b.community).await.unwrap();
    assert_eq!(relays.served(), 0);
    // Days on there is still no whole read: the sets are compared, whatever the dates.
    crate::clock::shift_for_test(3 * 86_400_000);
    assert!(sync_guestbook(&relays, &b.community).await.unwrap().is_empty());
    assert_eq!(relays.served(), 0);
    crate::clock::shift_for_test(0);
}

#[tokio::test]
async fn an_entry_dated_far_back_is_found_at_once_by_relays_that_reconcile() {
    let b = bed().await;
    let relays = Relays::reconciling(&b.relay);
    sync_guestbook(&relays, &b.community).await.unwrap();
    relays.served();
    let restored = join_on(&b, now_ms() / 1000 - 30 * 86_400, &["wss://b"]);
    let added = sync_guestbook(&relays, &b.community).await.unwrap();
    assert!(holds(&b, &restored) && added.len() == 1, "no cursor to be behind");
    assert_eq!(relays.served(), 1);
}

#[tokio::test]
async fn an_entry_that_came_in_live_is_never_asked_of_a_relay() {
    let b = bed().await;
    let relays = Relays::reconciling(&b.relay);
    sync_guestbook(&relays, &b.community).await.unwrap();
    let k = Keys::generate();
    let join = guestbook::build_join_rumor(k.public_key(), None, now_ms());
    let (wrap, _) = guestbook::seal_guestbook_rumor(&join, &b.gb, &k, Timestamp::from_secs(now_ms() / 1000)).unwrap();
    b.relay.inject(&wrap, &SET.map(String::from));
    let entry = guestbook::parse_guestbook_event(&stream::open_wrap(&wrap, &b.gb).unwrap()).unwrap();
    assert!(ingest_guestbook_event(&b.community, entry, wrap.created_at.as_secs()).unwrap());
    hold_guestbook_wrap(&b.community, &wrap);
    relays.served();
    assert!(sync_guestbook(&relays, &b.community).await.unwrap().is_empty());
    assert_eq!(relays.served(), 0);
}

#[tokio::test]
async fn a_wrap_that_opens_as_no_entry_is_fetched_once_and_a_forged_copy_never_counts_as_read() {
    let b = bed().await;
    let relays = Relays::reconciling(&b.relay);
    sync_guestbook(&relays, &b.community).await.unwrap();
    // A wrap of this plane, sealed around something the Guestbook has no entry for.
    let k = Keys::generate();
    let stray = stream::build_rumor_ms(9, k.public_key(), "not an entry", Vec::new(), now_ms());
    let (junk, _) = guestbook::seal_guestbook_rumor(&stray, &b.gb, &k, Timestamp::from_secs(now_ms() / 1000)).unwrap();
    b.relay.inject(&junk, &SET.map(String::from));
    relays.served();
    sync_guestbook(&relays, &b.community).await.unwrap();
    assert_eq!(relays.served(), 1);
    sync_guestbook(&relays, &b.community).await.unwrap();
    assert_eq!(relays.served(), 0, "held as refused: not asked for again");
    let view = super::super::guestbook_store::GuestbookView::of(&b.community);
    let held = super::super::control_sync::Held::held(&view).unwrap().len();
    let mut json: serde_json::Value = serde_json::from_str(&join_wrap(&b).as_json()).unwrap();
    json["sig"] = serde_json::Value::String(junk.sig.to_string());
    let forged: Event = serde_json::from_value(json).unwrap();
    let absorbed = super::super::control_sync::Held::absorb(&view, &[forged.clone()]).unwrap();
    assert_eq!((absorbed.unfit, absorbed.opened), (vec![forged.id], 0));
    // Nor is a wrap of another plane this one's to judge.
    let other = crate::community::v2::derive::guestbook_group_key(&b.community.community_root, b.community.id(), Epoch(b.community.root_epoch.0 + 1));
    let (elsewhere, _) = guestbook::seal_guestbook_rumor(&guestbook::build_join_rumor(k.public_key(), None, now_ms()), &other, &k, Timestamp::from_secs(now_ms() / 1000)).unwrap();
    let absorbed = super::super::control_sync::Held::absorb(&view, &[elsewhere]).unwrap();
    assert_eq!((absorbed.opened, absorbed.refused), (0, 0));
    assert_eq!(super::super::control_sync::Held::held(&view).unwrap().len(), held);
}

/// A Join's wrap, held by no relay.
fn join_wrap(b: &Bed) -> Event {
    let k = Keys::generate();
    let join = guestbook::build_join_rumor(k.public_key(), None, now_ms());
    guestbook::seal_guestbook_rumor(&join, &b.gb, &k, Timestamp::from_secs(now_ms() / 1000)).unwrap().0
}
