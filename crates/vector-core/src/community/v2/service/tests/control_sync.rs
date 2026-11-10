//! The Control wraps held per view, the pass that brings them up to what the relays hold, and
//! the read that folds them.

use super::super::control_store::{self, Mode, View};
use super::super::control_sync::{self, read_whole, sync_plane, Held, Relayed, Unsynced, Wait};
use super::*;
use crate::community::transport::{Diff, Reconcile, ReconcileFail};
use nostr_sdk::prelude::{EventBuilder, EventId, FinalizeEvent, Kind};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

const SET: [&str; 3] = ["wss://a", "wss://b", "wss://c"];

fn set() -> Vec<String> {
    SET.map(String::from).to_vec()
}

fn only(relay: &str) -> Vec<String> {
    vec![relay.to_string()]
}

/// The store on for this test's thread, and every test override gone again when it ends.
struct On;

impl On {
    fn new() -> On {
        control_store::set_mode_for_test(Some(Mode::On));
        On
    }
}

impl Drop for On {
    fn drop(&mut self) {
        control_store::set_mode_for_test(None);
        control_store::set_caps_for_test(None);
        control_sync::set_budget_for_test(None);
        crate::clock::shift_for_test(0);
    }
}

/// Each relay answered on its own, as the live transport answers, counting what it serves, with
/// the ways a relay can let a reader down.
#[derive(Default)]
struct Faults {
    /// Don't answer at all.
    down: HashSet<String>,
    /// Say "in sync" whatever they hold.
    agree: HashSet<String>,
    /// Name wraps, and serve none asked for by id.
    withhold: HashSet<String>,
    /// Serve every wrap with a signature that is not its own.
    forge: HashSet<String>,
    /// Reconcile over only what they hold from this second on.
    index_from: HashMap<String, u64>,
    /// Break off every reconciliation.
    break_off: HashSet<String>,
    /// Answer after this long.
    slow: HashMap<String, std::time::Duration>,
    /// Wraps these relays no longer hold.
    lost: HashMap<String, HashSet<EventId>>,
    /// Ids they name and hold no wrap for.
    phantom: HashMap<String, Vec<EventId>>,
    /// Answer nothing asked by pages.
    pageless: HashSet<String>,
    /// Serve, for an id asked, an event of that id from another author.
    impostor: HashSet<String>,
    /// Have no NIP-77.
    pages_only: HashSet<String>,
}

struct Wire<'a> {
    relay: &'a MemoryRelay,
    faults: Mutex<Faults>,
    /// Without it nothing reconciles, and every relay is read by pages.
    reconciles: bool,
    served: AtomicU64,
    by_relay: Mutex<HashMap<String, u64>>,
    /// Reads asked of a relay that answered.
    asked: AtomicU64,
    /// Wraps published to each relay.
    published: Mutex<HashMap<String, u64>>,
}

impl<'a> Wire<'a> {
    fn new(relay: &'a MemoryRelay) -> Self {
        Wire { relay, faults: Default::default(), reconciles: true, served: AtomicU64::new(0), by_relay: Default::default(), asked: AtomicU64::new(0), published: Default::default() }
    }

    fn paged(relay: &'a MemoryRelay) -> Self {
        Wire { reconciles: false, ..Wire::new(relay) }
    }

    fn fault(&self, set: impl FnOnce(&mut Faults)) {
        set(&mut self.faults.lock().unwrap());
    }

    /// Reads relays answered since the last ask.
    fn asked(&self) -> u64 {
        self.asked.swap(0, Ordering::Relaxed)
    }

    /// Wraps served since the last ask.
    fn served(&self) -> u64 {
        self.by_relay.lock().unwrap().clear();
        self.served.swap(0, Ordering::Relaxed)
    }

    /// Wraps `relay` served since the last ask of [`Self::served`].
    fn served_by(&self, relay: &str) -> u64 {
        self.by_relay.lock().unwrap().get(relay).copied().unwrap_or(0)
    }

    async fn wait(&self, relay: &str) {
        let slow = self.faults.lock().unwrap().slow.get(relay).copied();
        if let Some(slow) = slow {
            tokio::time::sleep(slow).await;
        }
    }
}

fn forged(wrap: &Event) -> Event {
    let other = EventBuilder::new(wrap.kind, "another").finalize(&Keys::generate()).unwrap();
    let mut json: serde_json::Value = serde_json::from_str(&wrap.as_json()).unwrap();
    json["sig"] = serde_json::Value::String(other.sig.to_string());
    serde_json::from_value(json).unwrap()
}

fn by_another(wrap: &Event) -> Event {
    let mut json: serde_json::Value = serde_json::from_str(&wrap.as_json()).unwrap();
    json["pubkey"] = serde_json::Value::String(Keys::generate().public_key().to_hex());
    serde_json::from_value(json).unwrap()
}

#[async_trait::async_trait]
impl Transport for Wire<'_> {
    async fn publish(&self, e: &Event, r: &[String]) -> Result<(), String> {
        for relay in r {
            *self.published.lock().unwrap().entry(relay.clone()).or_default() += 1;
        }
        self.relay.publish(e, r).await
    }
    async fn publish_durable(&self, e: &Event, r: &[String]) -> Result<(), String> {
        self.relay.publish_durable(e, r).await
    }
    async fn fetch(&self, q: &Query, relays: &[String]) -> Result<Vec<Event>, String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut floor = crate::community::transport::PageFloor::of(q);
        let mut answered = false;
        for r in relays {
            {
                let faults = self.faults.lock().unwrap();
                if faults.down.contains(r) || (q.ids.is_empty() && faults.pageless.contains(r)) {
                    continue;
                }
            }
            self.wait(r).await;
            answered = true;
            self.asked.fetch_add(1, Ordering::Relaxed);
            let mut evs = self.relay.fetch(q, std::slice::from_ref(r)).await?;
            {
                let faults = self.faults.lock().unwrap();
                if let Some(lost) = faults.lost.get(r) {
                    evs.retain(|e| !lost.contains(&e.id));
                }
                if !q.ids.is_empty() && faults.withhold.contains(r) {
                    evs.clear();
                }
                if faults.forge.contains(r) {
                    evs = evs.iter().map(forged).collect();
                }
                if !q.ids.is_empty() && faults.impostor.contains(r) {
                    evs = evs.iter().map(by_another).collect();
                }
            }
            self.served.fetch_add(evs.len() as u64, Ordering::Relaxed);
            *self.by_relay.lock().unwrap().entry(r.clone()).or_default() += evs.len() as u64;
            if let Some(floor) = floor.as_mut() {
                floor.note(&evs);
            }
            out.extend(evs.into_iter().filter(|e| seen.insert(e.id)));
        }
        if !answered {
            return Err("relay did not answer the fetch".into());
        }
        if let Some(floor) = &floor {
            floor.cut(&mut out);
        }
        Ok(out)
    }
    async fn fetch_plane(&self, _p: &Keys, q: &Query, r: &[String]) -> Result<crate::community::transport::PlaneFetch, String> {
        Ok(crate::community::transport::PlaneFetch::whole(self.fetch(q, r).await?))
    }
    fn reconciler(&self) -> Option<&dyn Reconcile> {
        self.reconciles.then_some(self as &dyn Reconcile)
    }
}

#[async_trait::async_trait]
impl Reconcile for Wire<'_> {
    async fn reconcile(&self, query: &Query, held: &[(EventId, u64)], relay: &str, budget: usize) -> Result<Diff, ReconcileFail> {
        let (down, agree, break_off, index_from) = {
            let f = self.faults.lock().unwrap();
            (f.down.contains(relay), f.agree.contains(relay), f.break_off.contains(relay), f.index_from.get(relay).copied())
        };
        if down {
            return Err(ReconcileFail::Unreached);
        }
        if self.faults.lock().unwrap().pages_only.contains(relay) {
            return Err(ReconcileFail::Unsupported);
        }
        self.wait(relay).await;
        if break_off {
            return Err(ReconcileFail::Failed("broke off".into()));
        }
        if agree {
            return Ok(Diff { need: Vec::new(), lack: Vec::new(), complete: true });
        }
        let query = Query { since: index_from.or(query.since), ..query.clone() };
        let mut diff = self.relay.reconciler().expect("the memory relay reconciles").reconcile(&query, held, relay, budget).await?;
        let faults = self.faults.lock().unwrap();
        if let Some(lost) = faults.lost.get(relay) {
            diff.need.retain(|id| !lost.contains(id));
            diff.lack.extend(held.iter().map(|(id, _)| *id).filter(|id| lost.contains(id)));
        }
        diff.need.extend(faults.phantom.get(relay).into_iter().flatten().copied());
        Ok(diff)
    }
}

struct Bed {
    _tmp: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
    _on: On,
    owner: Keys,
    relay: MemoryRelay,
    community: CommunityV2,
}

/// A community on three relays, with the store on.
async fn bed() -> Bed {
    let (tmp, guard, owner) = init_test_db();
    let on = On::new();
    let relay = MemoryRelay::new();
    let community = create_community(&relay, "Held", set(), None).await.unwrap();
    Bed { _tmp: tmp, _guard: guard, _on: on, owner, relay, community }
}

/// An edition no fold admits, dated `secs`.
fn filler(community: &CommunityV2, i: u64, secs: u64) -> Event {
    let signer = Keys::generate();
    let group = control::ControlPlane::of(community).write_group().unwrap();
    let content = format!("{{\"name\":\"filler{i}\",\"private\":false}}");
    let rumor = control::build_edition_rumor(signer.public_key(), vsk::CHANNEL_METADATA, &[0xAB; 32], 1, None, &content, secs, None);
    control::seal_control_edition(&rumor, &group, &signer, Timestamp::from_secs(secs)).unwrap().0
}

/// A wrap at the plane's address that opens as nothing.
fn junk(community: &CommunityV2, secs: u64) -> Event {
    let signer = control::ControlPlane::of(community).signer_keys().unwrap().clone();
    EventBuilder::new(Kind::Custom(stream::KIND_WRAP), "not an edition").custom_created_at(Timestamp::from_secs(secs)).finalize(&signer).unwrap()
}

fn plane_size(b: &Bed, relay: &str) -> usize {
    let pk = control::ControlPlane::of(&b.community).pk();
    futures_util::FutureExt::now_or_never(b.relay.fetch(&Query { kinds: vec![stream::KIND_WRAP], authors: vec![pk.to_hex()], ..Default::default() }, &only(relay)))
        .unwrap()
        .unwrap()
        .len()
}

fn holds(view: &View, wrap: &Event) -> bool {
    view.held().unwrap().iter().any(|(id, _)| *id == wrap.id)
}

fn now() -> u64 {
    crate::clock::now_secs()
}

// ── The wraps held ───────────────────────────────────────────────────────────

#[test]
fn a_follow_reads_the_wraps_held_unless_switched_off() {
    let set = match std::env::var("VECTOR_CONTROL_STORE").as_deref() {
        Ok("off") => Mode::Off,
        Ok("shadow") => Mode::Shadow,
        _ => Mode::On,
    };
    assert_eq!(control_store::mode(), set);
}

#[tokio::test]
async fn a_wrap_is_held_with_its_verdict() {
    let b = bed().await;
    let view = View::of(&b.community);
    let before = view.counts().unwrap();
    let edition = filler(&b.community, 0, now());
    let refused = junk(&b.community, now());
    let elsewhere = EventBuilder::new(Kind::Custom(stream::KIND_WRAP), "another plane").finalize(&Keys::generate()).unwrap();
    let taken = view.absorb(&[edition.clone(), refused.clone(), elsewhere.clone()]).unwrap();
    assert_eq!((taken.opened, taken.refused, taken.unfit.len()), (1, 1, 0));
    let after = view.counts().unwrap();
    assert_eq!((after.0 - before.0, after.1 - before.1), (1, 1), "a body for what opened, an id alone for what did not");
    assert!(holds(&view, &edition) && holds(&view, &refused) && !holds(&view, &elsewhere));
    assert!(view.page(None, 500).unwrap().iter().any(|w| w.id == edition.id), "what opened is read back as the event it was");
    assert!(!view.page(None, 500).unwrap().iter().any(|w| w.id == refused.id), "a refusal has no body to read back");
    let again = view.absorb(&[edition, refused]).unwrap();
    assert_eq!((again.opened, again.refused), (0, 0), "held already");
}

#[tokio::test]
async fn a_copy_that_is_not_the_event_its_id_names_is_never_held() {
    let b = bed().await;
    let view = View::of(&b.community);
    let edition = filler(&b.community, 0, now());
    let taken = view.absorb(&[forged(&edition)]).unwrap();
    assert_eq!((taken.opened, taken.refused, taken.unfit), (0, 0, vec![edition.id]));
    assert!(!holds(&view, &edition), "held, the real edition would never be asked for again");
    assert_eq!(view.absorb(std::slice::from_ref(&edition)).unwrap().opened, 1, "and the edition itself is still taken");
}

#[tokio::test]
async fn refusals_are_judged_again_under_other_rules_and_a_few_each_day() {
    let b = bed().await;
    let view = View::of(&b.community);
    let cid = b.community.id().to_hex();
    let (kept, other) = (junk(&b.community, now()), junk(&b.community, now() - 5));
    view.absorb(std::slice::from_ref(&kept)).unwrap();
    for rules in [control_store::OPEN_RULES + 1, control_store::OPEN_RULES - 1] {
        let id = Keys::generate().public_key().to_bytes();
        crate::db::community::store_control_wraps(&cid, &view.id(), rules, now(), &[(id, now(), None)]).unwrap();
        crate::db::community::store_control_wraps(&cid, &view.id(), rules, now(), &[(other.id.to_bytes(), now(), None)]).unwrap();
    }
    assert_eq!(view.counts().unwrap().1, 4);
    view.rejudge(false);
    assert_eq!(view.counts().unwrap().1, 1, "a refusal under newer or older rules is asked for and judged again");
    assert!(holds(&view, &kept), "one under these rules stands");
    let bodies = view.counts().unwrap().0;
    view.rejudge(true);
    assert_eq!(view.counts().unwrap(), (bodies, 0, view.counts().unwrap().2), "once a day the longest standing go too, and no body with them");
}

#[tokio::test]
async fn a_view_at_its_cap_takes_no_more() {
    let b = bed().await;
    let view = View::of(&b.community);
    let bodies = view.counts().unwrap().0;
    control_store::set_caps_for_test(Some((bodies + 2, usize::MAX, 1)));
    let wraps: Vec<Event> = (0..4).map(|i| filler(&b.community, i, now())).chain([junk(&b.community, now()), junk(&b.community, now() - 1)]).collect();
    let taken = view.absorb(&wraps).unwrap();
    assert_eq!((taken.opened, taken.refused, taken.full), (2, 1, true));
    assert_eq!(view.counts().unwrap().0, bodies + 2);
    assert!(view.at_cap());
    assert!(!holds(&view, &wraps[3]), "what a full view turned away is not held, so it is asked for once there is room");
}

#[tokio::test]
async fn another_epochs_plane_is_another_view_and_leaving_forgets_them_all() {
    let b = bed().await;
    let view = View::of(&b.community);
    let edition = filler(&b.community, 0, now());
    view.absorb(std::slice::from_ref(&edition)).unwrap();
    let mut moved = b.community.clone();
    moved.root_epoch = Epoch(b.community.root_epoch.0 + 1);
    let next = View::of(&moved);
    assert_ne!(view.id(), next.id());
    assert!(next.held().unwrap().is_empty(), "nothing is held of a plane never read");
    assert!(holds(&view, &edition), "and the plane before keeps what was read of it");
    crate::db::community::delete_community(&b.community.id().to_hex()).unwrap();
    assert!(view.held().unwrap().is_empty());
}

// ── A pass against the relays ────────────────────────────────────────────────

#[tokio::test]
async fn a_first_pass_downloads_each_wrap_once_and_the_next_downloads_nothing() {
    let b = bed().await;
    for i in 0..40 {
        b.relay.inject(&filler(&b.community, i, now() - 100 + i), &set());
    }
    let view = View::of(&b.community);
    cold(&b.community);
    let wire = Wire::new(&b.relay);
    let plane = plane_size(&b, SET[0]) as u64;
    let sync = sync_plane(&wire, &b.community, &view, Wait::Quorum).await.unwrap();
    assert_eq!(wire.served(), plane, "once, from one relay, though three hold it");
    assert_eq!(sync.opened as u64, plane);
    assert!(sync.whole() && sync.relays.values().all(|r| *r == Relayed::Whole));
    b.relay.reconciled();
    let sync = sync_plane(&wire, &b.community, &view, Wait::Quorum).await.unwrap();
    assert_eq!((wire.served(), sync.opened), (0, 0));
    assert_eq!(b.relay.reconciled(), 3, "each relay was still asked");
}

#[tokio::test]
async fn a_new_wrap_is_downloaded_once_and_one_only_a_single_relay_holds_is_found() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::new(&b.relay);
    sync_plane(&wire, &b.community, &view, Wait::Quorum).await.unwrap();
    wire.served();
    let everywhere = filler(&b.community, 1, now());
    b.relay.inject(&everywhere, &set());
    sync_plane(&wire, &b.community, &view, Wait::Quorum).await.unwrap();
    assert_eq!(wire.served(), 1);
    assert!(holds(&view, &everywhere));
    // Dated a year back: a cursor would never look there.
    let lone = filler(&b.community, 2, now() - 365 * 86_400);
    b.relay.inject(&lone, &only(SET[2]));
    let sync = sync_plane(&wire, &b.community, &view, Wait::Quorum).await.unwrap();
    assert_eq!((wire.served(), wire.served_by(SET[2])), (1, 0));
    assert!(holds(&view, &lone));
    assert!(sync.lacking[SET[0]] == 0 || sync.lacking[SET[0]] == 1, "a later pass knows the others lack it");
}

#[tokio::test]
async fn a_relay_that_was_down_is_asked_the_same_question_when_back() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::new(&b.relay);
    let lone = filler(&b.community, 1, now() - 30 * 86_400);
    b.relay.inject(&lone, &only(SET[1]));
    wire.fault(|f| { let _ = f.down.insert(SET[1].into()); });
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert_eq!(sync.relays[SET[1]], Relayed::Unreached);
    assert!(!holds(&view, &lone) && !sync.unread, "a relay that didn't answer leaves nothing known unread");
    wire.fault(|f| f.down.clear());
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(holds(&view, &lone), "back, it names what it alone holds");
}

#[tokio::test]
async fn every_relay_down_is_an_error_and_nothing_is_held() {
    let b = bed().await;
    let view = View::of(&b.community);
    cold(&b.community);
    let wire = Wire::new(&b.relay);
    wire.fault(|f| f.down.extend(set()));
    assert!(matches!(sync_plane(&wire, &b.community, &view, Wait::Quorum).await, Err(Unsynced::NoRelay(_))));
    assert!(view.held().unwrap().is_empty());
}

#[tokio::test]
async fn a_relay_that_cannot_reconcile_is_read_by_pages_from_its_cursor() {
    let b = bed().await;
    for i in 0..30 {
        b.relay.inject(&filler(&b.community, i, now() - 30 * 86_400 + i), &set());
    }
    let view = View::of(&b.community);
    cold(&b.community);
    b.relay.without_reconcile(&only(SET[1]));
    let wire = Wire::new(&b.relay);
    let plane = plane_size(&b, SET[1]) as u64;
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert_eq!(sync.relays[SET[1]], Relayed::Whole, "its first read is the whole plane");
    // Two hours on its cursor moves past the hour each read looks back over.
    crate::clock::shift_for_test(2 * 3_600_000);
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert_eq!((sync.relays[SET[1]], sync.relays[SET[0]]), (Relayed::Since, Relayed::Whole));
    wire.served();
    crate::clock::shift_for_test(4 * 3_600_000);
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert_eq!(wire.served(), 0, "past its cursor there is nothing");
    let fresh = filler(&b.community, 99, now());
    b.relay.inject(&fresh, &only(SET[1]));
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(holds(&view, &fresh));
    assert!((1..=2).contains(&wire.served()), "the new wrap, and at most itself again as the read finds its end");
    crate::clock::shift_for_test(27 * 3_600_000);
    wire.served();
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(wire.served_by(SET[1]) > plane, "a day on it is read whole again, for what no cursor sees");
}

#[tokio::test]
async fn nothing_reconciling_reads_every_relay_by_pages_and_misses_nothing() {
    let b = bed().await;
    let view = View::of(&b.community);
    cold(&b.community);
    let lone = filler(&b.community, 1, now() - 40 * 86_400);
    b.relay.inject(&lone, &only(SET[2]));
    let wire = Wire::paged(&b.relay);
    let sync = sync_plane(&wire, &b.community, &view, Wait::Quorum).await.unwrap();
    assert!(sync.relays.values().all(|r| *r == Relayed::Whole) && holds(&view, &lone));
    assert_eq!(view.counts().unwrap().0, plane_size(&b, SET[2]));
}

#[tokio::test]
async fn a_relay_that_names_what_it_never_serves_ends_partial_and_another_serves_it() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::new(&b.relay);
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    let shared = filler(&b.community, 1, now());
    b.relay.inject(&shared, &set());
    wire.fault(|f| { let _ = f.withhold.insert(SET[0].into()); });
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(holds(&view, &shared), "the next relay that named it served it");
    assert!(sync.whole(), "and with it held, nothing is unread");

    let lone = filler(&b.community, 2, now());
    b.relay.inject(&lone, &only(SET[0]));
    wire.served();
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    // Named and not served by id, the relay is read by pages, which hand over the wrap itself.
    assert!(holds(&view, &lone) && sync.whole());
    assert!(wire.served_by(SET[0]) as usize >= plane_size(&b, SET[0]));
    // Named again within the day: read from where that read began, which is where it sits.
    wire.served();
    let lone = filler(&b.community, 3, now());
    b.relay.inject(&lone, &only(SET[0]));
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(holds(&view, &lone) && sync.whole());
    wire.served();
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert_eq!(wire.served(), 0);
}

#[tokio::test]
async fn an_id_a_relay_names_and_holds_no_wrap_for_costs_one_read_and_leaves_nothing_unread() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::new(&b.relay);
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    wire.fault(|f| drop(f.phantom.insert(SET[0].into(), vec![EventId::from_byte_array([9; 32])])));
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(sync.whole(), "its pages ran to their end without it: there is no wrap to be unread");
    wire.asked();
    b.relay.reconciled();
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(sync.whole());
    assert_eq!((wire.asked(), b.relay.reconciled()), (0, 3), "and it is not asked for again, by id or by pages");
    wire.served();
    assert!(!read_control(&wire, &b.community).await.unwrap().truncated);
    // A day on it is asked for once more, with the relay's daily read.
    crate::clock::shift_for_test(2 * 1000 * super::super::guestbook_reads::REREAD_SECS as i64);
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(wire.served() > 0);
    wire.served();
    assert!(sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap().whole());
    assert_eq!(wire.served(), 0);
}

#[tokio::test]
async fn more_ids_named_and_never_served_than_a_relay_is_allowed_stay_unread() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::new(&b.relay);
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    let many: Vec<EventId> = (0..600u16).map(|i| EventId::from_byte_array(std::array::from_fn(|b| if b < 2 { i.to_be_bytes()[b] } else { 9 }))).collect();
    wire.fault(|f| { let _ = f.phantom.insert(SET[0].into(), many); });
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(sync.unread && sync.relays[SET[0]] == Relayed::Partial);
}

#[tokio::test]
async fn an_event_of_the_id_from_another_author_is_not_the_wrap_and_the_next_relay_is_asked() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::new(&b.relay);
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    let edition = filler(&b.community, 1, now());
    b.relay.inject(&edition, &set());
    wire.fault(|f| f.impostor.extend(SET[..2].iter().map(|r| r.to_string())));
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(holds(&view, &edition) && sync.whole());
}

#[tokio::test]
async fn a_body_that_did_not_decrypt_is_kept_and_the_read_goes_by_pages() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    let before = read_control(&wire, &community).await.unwrap();
    let view = View::of(&community);
    // As a body reads when the key at hand is not the one that sealed it.
    let sealed = filler(&community, 1, now());
    crate::db::community::store_control_wraps(&community.id().to_hex(), &view.id(), control_store::OPEN_RULES, now(), &[(sealed.id.to_bytes(), sealed.created_at.as_secs(), Some(vec![0x9c; 300]))]).unwrap();
    let held = view.held().unwrap().len();
    assert!(view.page(None, 500).is_err());
    let read = read_control(&wire, &community).await.unwrap();
    assert_eq!(outcome(&read), outcome(&before));
    assert_eq!(view.held().unwrap().len(), held, "nothing held was dropped for it");
}

#[tokio::test]
async fn every_relay_breaking_its_reconciliation_off_is_a_read_by_pages_not_a_failure() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    let before = read_control(&wire, &community).await.unwrap();
    wire.fault(|f| f.break_off.extend(set()));
    let extra = filler(&community, 1, now());
    b.relay.inject(&extra, &set());
    let sync = sync_plane(&wire, &community, &View::of(&community), Wait::All).await.unwrap();
    assert!(holds(&View::of(&community), &extra) && sync.relays.values().all(|r| *r != Relayed::Unreached));
    let read = read_control(&wire, &community).await.unwrap();
    assert!(!read.truncated && read.editions.len() == before.editions.len() + 1);
}

#[tokio::test]
async fn fewer_wraps_held_than_the_cursors_were_earned_on_reads_each_relay_whole_again() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::paged(&b.relay);
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    wire.served();
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    let from_the_cursor = wire.served_by(SET[0]) as usize;
    let old = filler(&b.community, 1, now() - 30 * 86_400);
    b.relay.inject(&old, &set());
    view.forget(&view.held().unwrap()[0].0);
    wire.served();
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(holds(&view, &old), "a cursor would have read past it");
    assert!(wire.served_by(SET[0]) as usize >= plane_size(&b, SET[0]) && plane_size(&b, SET[0]) > from_the_cursor.min(0));
    assert_eq!(view.held().unwrap().len(), plane_size(&b, SET[0]));
}

#[tokio::test]
async fn a_community_not_held_is_read_by_pages() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    let before = read_control_paged(&wire, &community).await.unwrap();
    crate::db::community::delete_community(&community.id().to_hex()).unwrap();
    let read = read_control(&wire, &community).await.unwrap();
    assert!(!read.editions.is_empty());
    assert_eq!(edition_ids(&read), edition_ids(&before));
    assert!(View::of(&community).held().unwrap().is_empty(), "and nothing is kept of it");
}

#[tokio::test]
async fn a_pass_that_ends_after_the_community_was_left_keeps_nothing() {
    let b = bed().await;
    let view = View::of(&b.community);
    crate::db::community::delete_community(&b.community.id().to_hex()).unwrap();
    sync_plane(&Wire::new(&b.relay), &b.community, &view, Wait::All).await.unwrap();
    assert!(view.held().unwrap().is_empty());
    assert_eq!(control_sync::report(&b.community, &view)["relays"], serde_json::json!({}));
}

#[tokio::test]
async fn a_read_below_a_capped_index_that_stops_short_is_unread_and_goes_on_next_pass() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::new(&b.relay);
    for i in 0..5 {
        b.relay.inject(&filler(&b.community, i, now() - 90 * 86_400 + i), &only(SET[0]));
    }
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    // Its index holds only the last hour now: everything older is to be had by pages alone.
    wire.fault(|f| { let _ = f.index_from.insert(SET[0].into(), now() - 3_600); });
    let buried: Vec<Event> = (0..1_200).map(|i| filler(&b.community, 100 + i, now() - 200 * 86_400 + i)).collect();
    for w in &buried {
        b.relay.inject(w, &only(SET[0]));
    }
    control_sync::set_budget_for_test(Some(600));
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(sync.unread && sync.relays[SET[0]] == Relayed::Partial, "short of the end, and saying so");
    let held = view.held().unwrap().len();
    wire.served();
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(view.held().unwrap().len() > held && (wire.served_by(SET[0]) as usize) < 700, "the next pass goes on from where this one stopped");
    assert!(buried.iter().all(|w| holds(&view, w)) || sync.unread);
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(buried.iter().all(|w| holds(&view, w)));
}

#[tokio::test]
async fn a_broken_copy_from_one_relay_is_fetched_from_another() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::new(&b.relay);
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    let edition = filler(&b.community, 1, now());
    b.relay.inject(&edition, &set());
    wire.fault(|f| { let _ = f.forge.insert(SET[0].into()); });
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(holds(&view, &edition) && sync.whole());
    assert!(view.page(None, 500).unwrap().iter().any(|w| w.id == edition.id), "held as the edition, not as a refusal of the broken copy");

    let lone = filler(&b.community, 2, now());
    b.relay.inject(&lone, &only(SET[0]));
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(!holds(&view, &lone), "a broken copy is never held");
    assert!(sync.whole(), "nor is it a wrap left unread: its pages ran to their end with no event of that id");
    // The real one is asked for again with the relay's daily read.
    wire.fault(|f| f.forge.clear());
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(!holds(&view, &lone));
    crate::clock::shift_for_test(2 * 1000 * super::super::guestbook_reads::REREAD_SECS as i64);
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(holds(&view, &lone));
}

#[test]
fn an_index_stops_short_only_below_a_second_it_holds_nothing_before() {
    let id = |b: u8| EventId::from_byte_array([b; 32]);
    let held = [(id(1), 10), (id(2), 10), (id(3), 20)];
    let stops = |lack: &[u8]| control_sync::index_stops_at(&held, &lack.iter().map(|b| id(*b)).collect::<Vec<_>>());
    assert_eq!(stops(&[]), None);
    assert_eq!(stops(&[1]), None, "it holds another wrap of that second: a wrap it lacks, not an index cut short");
    assert_eq!(stops(&[2]), None);
    assert_eq!(stops(&[3]), None);
    assert_eq!(stops(&[1, 2]), Some(20));
    assert_eq!(stops(&[1, 2, 3]), Some(control_sync::FROM_THE_TOP));
}

#[tokio::test]
async fn a_relay_whose_index_stops_short_is_read_by_pages_below_it() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::new(&b.relay);
    let old: Vec<Event> = (0..5).map(|i| filler(&b.community, i, now() - 90 * 86_400 + i)).collect();
    for w in &old {
        b.relay.inject(w, &only(SET[0]));
    }
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    // One more that only this relay holds, far below where its index now starts.
    let buried = filler(&b.community, 9, now() - 200 * 86_400);
    b.relay.inject(&buried, &only(SET[0]));
    wire.fault(|f| { let _ = f.index_from.insert(SET[0].into(), now() - 3_600); });
    wire.served();
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(holds(&view, &buried), "its answer showed it lacking everything old, so its pages were read");
    assert!(wire.served_by(SET[0]) > 0);
    wire.served();
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert_eq!(wire.served(), 0, "once a day");
}

#[tokio::test]
async fn more_named_than_a_pass_reads_marks_the_view_flooded() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::new(&b.relay);
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    control_sync::set_budget_for_test(Some(10));
    for i in 0..25 {
        b.relay.inject(&filler(&b.community, i, now()), &set());
    }
    wire.served();
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(sync.flooded && sync.unread && !sync.whole());
    assert!(wire.served() <= 10, "a pass reads no more than its budget");
    assert!(control_sync::flooded(&b.community, &view));
    // Left to the pager for an hour, then reconciled again: under budget, it clears.
    control_sync::set_budget_for_test(None);
    crate::clock::shift_for_test(2 * 3_600_000);
    assert!(!control_sync::flooded(&b.community, &view));
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(sync.whole() && !control_sync::flooded(&b.community, &view));
}

#[tokio::test]
async fn a_view_at_its_cap_is_flooded() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::new(&b.relay);
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    control_store::set_caps_for_test(Some((view.counts().unwrap().0 + 1, usize::MAX, usize::MAX)));
    for i in 0..3 {
        b.relay.inject(&filler(&b.community, i, now()), &set());
    }
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(sync.flooded && sync.unread, "what it could not take is still unread");
}

#[tokio::test]
async fn a_reconciliation_that_keeps_breaking_off_ends_in_a_read_by_pages() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::new(&b.relay);
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    let lone = filler(&b.community, 1, now());
    b.relay.inject(&lone, &only(SET[2]));
    wire.fault(|f| { let _ = f.break_off.insert(SET[2].into()); });
    for _ in 0..2 {
        let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
        assert_eq!(sync.relays[SET[2]], Relayed::Unreached, "broken off: asked again next time");
        assert!(!holds(&view, &lone));
    }
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(holds(&view, &lone), "the third time its pages are read instead");
}

#[tokio::test(start_paused = true)]
async fn a_slow_relay_is_waited_out_once_then_left_to_the_grace() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::new(&b.relay);
    wire.fault(|f| { let _ = f.slow.insert(SET[2].into(), std::time::Duration::from_secs(30)); });
    let t = tokio::time::Instant::now();
    let sync = sync_plane(&wire, &b.community, &view, Wait::Quorum).await.unwrap();
    assert_eq!(sync.relays[SET[2]], Relayed::Whole, "never heard this session: waited out");
    assert!(t.elapsed() >= std::time::Duration::from_secs(30));
    let lone = filler(&b.community, 1, now());
    b.relay.inject(&lone, &only(SET[2]));
    let t = tokio::time::Instant::now();
    let sync = sync_plane(&wire, &b.community, &view, Wait::Quorum).await.unwrap();
    assert!(t.elapsed() < std::time::Duration::from_secs(3), "heard already: the pass doesn't wait on it");
    assert_eq!(sync.relays[SET[2]], Relayed::Unreached);
    assert!(!sync.unread, "a relay not waited for leaves nothing known unread");
    assert!(sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap().whole() && holds(&view, &lone), "a pass that waits for all hears it");
    // Hours on, a pass waits it out again, so a relay always slower than the grace is still heard.
    let later = filler(&b.community, 2, now());
    b.relay.inject(&later, &only(SET[2]));
    crate::clock::shift_for_test(7 * 3_600_000);
    sync_plane(&wire, &b.community, &view, Wait::Quorum).await.unwrap();
    assert!(holds(&view, &later));
}

#[tokio::test]
async fn a_whole_read_reads_each_relay_to_the_end_once_a_day() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::new(&b.relay);
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    // A relay that says "in sync" while holding one more.
    let hidden = filler(&b.community, 1, now() - 50 * 86_400);
    b.relay.inject(&hidden, &only(SET[1]));
    wire.fault(|f| { let _ = f.agree.insert(SET[1].into()); });
    assert!(sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap().whole(), "its word is taken");
    assert!(!holds(&view, &hidden));
    wire.served();
    let read = read_whole(&wire, &b.community, &view).await.unwrap();
    assert!(holds(&view, &hidden), "its pages are not its word");
    assert_eq!(read.opened, 1);
    assert!(wire.served() as usize >= plane_size(&b, SET[0]) + plane_size(&b, SET[1]) + plane_size(&b, SET[2]), "every relay, to the end");
    let read = read_whole(&wire, &b.community, &view).await.unwrap();
    assert_eq!((wire.served(), read.opened), (0, 0), "not again within the day");
    assert!(read.relays.values().all(|r| *r == Relayed::Whole));
    crate::clock::shift_for_test(25 * 3_600_000);
    read_whole(&wire, &b.community, &view).await.unwrap();
    assert!(wire.served() > 0);
}

#[tokio::test]
async fn a_pass_reads_no_more_than_its_budget_across_relays() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::new(&b.relay);
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    // Each relay names eight of its own: under the budget one by one, over it together.
    for (r, relay) in SET.iter().enumerate() {
        for i in 0..8 {
            b.relay.inject(&filler(&b.community, (r * 10 + i) as u64, now()), &only(relay));
        }
    }
    control_sync::set_budget_for_test(Some(10));
    wire.served();
    let sync = sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert_eq!(wire.served(), 10);
    assert!(sync.unread && sync.flooded);
}

#[tokio::test]
async fn a_refusal_under_other_rules_is_fetched_and_judged_again_by_the_next_pass() {
    let b = bed().await;
    let view = View::of(&b.community);
    let wire = Wire::new(&b.relay);
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    // An edition an older build refused, as a build with stricter or looser rules would have.
    let edition = filler(&b.community, 1, now());
    b.relay.inject(&edition, &set());
    crate::db::community::store_control_wraps(&b.community.id().to_hex(), &view.id(), control_store::OPEN_RULES + 1, now(), &[(edition.id.to_bytes(), edition.created_at.as_secs(), None)]).unwrap();
    assert!(!view.page(None, 500).unwrap().iter().any(|w| w.id == edition.id));
    sync_plane(&wire, &b.community, &view, Wait::All).await.unwrap();
    assert!(view.page(None, 500).unwrap().iter().any(|w| w.id == edition.id), "held now as the edition it is");
}

// ── The read through what is held ────────────────────────────────────────────

/// Roles, grants, a banlist, channels and a rename, as a community a while into its life has.
async fn lived_in(b: &Bed) -> CommunityV2 {
    let held = || crate::db::community::load_community_v2(b.community.id()).unwrap().unwrap();
    activate_roles_v2(&b.relay, &held(), 0).await.unwrap();
    let mut roles = Vec::new();
    for r in 0..4 {
        roles.push(create_role_v2(&b.relay, &held(), &format!("role {r}"), 0x55aa00, 1 << r).await.unwrap());
    }
    for m in 0..12 {
        set_member_roles(&b.relay, &held(), &Keys::generate().public_key(), vec![roles[m % roles.len()].clone()]).await.unwrap();
    }
    set_banlist(&b.relay, &held(), &[Keys::generate().public_key().to_hex()]).await.unwrap();
    create_public_channel(&b.relay, &held(), "second").await.unwrap();
    held()
}

fn outcome(read: &ControlRead) -> Vec<(&'static str, String)> {
    read_outcome(read)
}

fn edition_ids(read: &ControlRead) -> std::collections::BTreeSet<[u8; 32]> {
    read.editions.iter().map(|e| e.inner_id).collect()
}

#[tokio::test]
async fn the_read_through_the_store_decides_what_the_read_by_pages_decides() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    let paged = read_control_paged(&wire, &community).await.unwrap();
    let held = read_control_held(&wire, &community).await.ok().expect("read through the store");
    assert!(!paged.truncated && !held.truncated);
    assert_eq!(edition_ids(&held), edition_ids(&paged));
    assert_eq!(outcome(&held), outcome(&paged));
    assert!(held.authority.roles.grants.len() == 12 && held.authority.roles.roles.len() >= 4, "and it is the roster built: {:?}", held.authority.roles);
}

#[tokio::test]
async fn a_cold_device_reads_a_plane_longer_than_a_page_the_same_either_way() {
    let b = bed().await;
    let community = lived_in(&b).await;
    for i in 0..700u64 {
        let wrap = filler(&community, i, now() - 5_000 + i);
        // Spread unevenly: no relay holds every one.
        let on: Vec<String> = SET.iter().enumerate().filter(|(r, _)| (i as usize + r) % 3 != 0).map(|(_, r)| r.to_string()).collect();
        b.relay.inject(&wrap, &on);
    }
    // Nothing held of it yet, as on the device a member just joined from: no floors, no roster, no wraps.
    let cid = community.id().to_hex();
    crate::db::community::delete_community(&cid).unwrap();
    crate::db::community::save_community_v2(&community).unwrap();
    let wire = Wire::new(&b.relay);
    let paged = read_control_paged(&wire, &community).await.unwrap();
    wire.served();
    let held = read_control_held(&wire, &community).await.ok().expect("read through the store");
    assert!(!paged.truncated && !held.truncated);
    assert_eq!(edition_ids(&held), edition_ids(&paged), "every edition, whichever relays hold it");
    assert_eq!(outcome(&held), outcome(&paged));
    assert_eq!(wire.served() as usize, View::of(&community).held().unwrap().len(), "each wrap downloaded once");
}

#[tokio::test]
async fn a_quiet_follow_downloads_nothing_and_an_edition_written_here_is_never_fetched_back() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    wire.served();
    follow_control(&wire, &community).await.unwrap();
    assert_eq!(wire.served(), 0);
    let held = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
    create_public_channel(&wire, &held, "third").await.unwrap();
    wire.served();
    let held = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
    follow_control(&wire, &held).await.unwrap();
    assert_eq!(wire.served(), 0, "held from the moment it was published");
    assert!(held.channels.iter().any(|c| c.name == "third"));
}

#[tokio::test]
async fn a_community_made_here_and_one_refounded_here_are_never_fetched_back() {
    let (_tmp, _guard, owner) = init_test_db();
    let _on = On::new();
    let relay = MemoryRelay::new();
    let wire = Wire::new(&relay);
    let community = create_community(&wire, "Made", set(), None).await.unwrap();
    wire.served();
    follow_control(&wire, &community).await.unwrap();
    sync_guestbook(&wire, &community).await.unwrap();
    assert_eq!(wire.served(), 0, "the genesis editions and the owner's Join are held from the start");
    let held = || crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
    refound_community(&wire, &held(), &[]).await.unwrap();
    assert_ne!(held().root_epoch, community.root_epoch);
    wire.served();
    follow_control(&wire, &held()).await.unwrap();
    assert_eq!(wire.served(), 0, "nor the heads it carried over");
    let _ = owner;
}

#[tokio::test]
async fn a_live_wrap_is_held_and_never_fetched() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    let live = filler(&community, 1, now());
    b.relay.inject(&live, &set());
    let _ = follow_control_live(&community, &live).await;
    assert!(holds(&View::of(&community), &live));
    wire.served();
    follow_control(&wire, &community).await.unwrap();
    assert_eq!(wire.served(), 0);
}

#[tokio::test]
async fn a_gap_under_relays_that_say_in_sync_is_read_for_once_a_day() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    // An edition resting on one this device has never held: its entity folds with a gap.
    let channel = community.channels[0].id;
    let group = control::ControlPlane::of(&community).write_group().unwrap();
    let rumor = control::build_edition_rumor(b.owner.public_key(), vsk::CHANNEL_METADATA, &channel.0, 9, Some(&[0x77; 32]), "{\"name\":\"ahead\",\"private\":false}", now(), None);
    let (ahead, _) = control::seal_control_edition(&rumor, &group, &b.owner, Timestamp::from_secs(now())).unwrap();
    b.relay.inject(&ahead, &set());
    wire.served();
    let read = read_control(&wire, &community).await.unwrap();
    assert!(read.fold.gapped, "the fixture: a gap no relay's answer explains");
    let plane = plane_size(&b, SET[0]) as u64;
    assert!(wire.served() >= 1 + 3 * plane, "the new wrap, then each relay's pages: their word is not taken for a gap");
    read_control(&wire, &community).await.unwrap();
    assert_eq!(wire.served(), 0, "and not again that day, gapped as it stays");
}

#[tokio::test]
async fn relays_that_drop_what_this_device_read_change_nothing_it_decides() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    let before = read_control(&wire, &community).await.unwrap();
    // Every relay loses a third of the plane, never its oldest wrap: a relay lacking the oldest
    // reads as an index that stops short, and is read by pages for that.
    let mut held = View::of(&community).held().unwrap();
    held.sort_by_key(|(id, at)| (*at, *id));
    let gone: HashSet<EventId> = held.iter().skip(1).map(|(id, _)| *id).step_by(3).collect();
    wire.fault(|f| f.lost = SET.iter().map(|r| (r.to_string(), gone.clone())).collect());
    wire.served();
    let read = read_control(&wire, &community).await.unwrap();
    assert!(!read.truncated);
    assert_eq!(edition_ids(&read), edition_ids(&before));
    assert_eq!(outcome(&read), outcome(&before), "what was read once is not unread by a relay forgetting it");
    assert_eq!(wire.served(), 0);
}

#[tokio::test]
async fn a_device_that_kept_no_wraps_fills_once_and_decides_what_it_had() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    let cid = community.id().to_hex();
    let (floors, roster) = (crate::db::community::get_all_edition_heads_full(&cid).unwrap(), crate::db::community::get_community_roles(&cid).unwrap());
    let before = read_control(&wire, &community).await.unwrap();
    // Floors and roster from a build that kept none of the wraps it read.
    let view = View::of(&community);
    for (id, _) in view.held().unwrap() {
        view.forget(&id);
    }
    wire.served();
    let held = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
    follow_control(&wire, &held).await.unwrap();
    assert_eq!(wire.served() as usize, view.held().unwrap().len(), "each wrap once");
    assert_eq!(crate::db::community::get_all_edition_heads_full(&cid).unwrap(), floors, "no floor moved");
    assert_eq!(crate::db::community::get_community_roles(&cid).unwrap(), roster);
    assert_eq!(outcome(&read_control(&wire, &held).await.unwrap()), outcome(&before));
    assert_eq!(wire.served(), 0);
}

// ── Healing ──────────────────────────────────────────────────────────────────

fn published(wire: &Wire, relay: &str) -> u64 {
    wire.published.lock().unwrap().remove(relay).unwrap_or(0)
}

#[tokio::test]
async fn staff_give_a_relay_the_wraps_it_lacks_that_another_holds() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    let missing = filler(&community, 1, now());
    b.relay.inject(&missing, &[SET[1].to_string(), SET[2].to_string()]);
    follow_control(&wire, &community).await.unwrap();
    assert!(holds(&View::of(&community), &missing));
    let before = plane_size(&b, SET[0]);
    wire.published.lock().unwrap().clear();
    follow_control(&wire, &community).await.unwrap();
    assert_eq!(plane_size(&b, SET[0]), before + 1, "the owner gave it the wrap");
    assert_eq!((published(&wire, SET[0]), published(&wire, SET[1])), (1, 0));
    follow_control(&wire, &community).await.unwrap();
    assert_eq!(published(&wire, SET[0]), 0, "and has nothing more to give");
}

#[tokio::test]
async fn a_member_who_is_not_staff_gives_nothing() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    b.relay.inject(&filler(&community, 1, now()), &[SET[1].to_string(), SET[2].to_string()]);
    act_as(&Keys::generate());
    wire.published.lock().unwrap().clear();
    follow_control(&wire, &community).await.unwrap();
    follow_control(&wire, &community).await.unwrap();
    assert_eq!(published(&wire, SET[0]), 0);
}

#[tokio::test]
async fn a_wrap_no_relay_holds_is_not_given_to_any() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    let only_here = filler(&community, 1, now());
    View::of(&community).absorb(std::slice::from_ref(&only_here)).unwrap();
    wire.published.lock().unwrap().clear();
    follow_control(&wire, &community).await.unwrap();
    follow_control(&wire, &community).await.unwrap();
    assert!(SET.iter().all(|r| published(&wire, r) == 0));
}

#[tokio::test]
async fn a_relay_that_keeps_none_of_what_it_is_given_is_given_it_less_and_less_often() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    let missing = filler(&community, 1, now());
    b.relay.inject(&missing, &set());
    // SET[0] never keeps it.
    wire.fault(|f| { f.lost.insert(SET[0].to_string(), HashSet::from([missing.id])); });
    follow_control(&wire, &community).await.unwrap();
    wire.published.lock().unwrap().clear();
    let mut gave = Vec::new();
    for hours in [0, 2, 4, 8, 20, 40] {
        crate::clock::shift_for_test(hours * 3_600_000);
        follow_control(&wire, &community).await.unwrap();
        gave.push(published(&wire, SET[0]));
    }
    assert_eq!(gave, vec![1, 1, 0, 1, 0, 1], "at once, an hour on, then six, then a day");
}

#[tokio::test]
async fn a_flooded_view_is_read_by_pages_as_before() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    control_sync::set_budget_for_test(Some(5));
    for i in 0..20 {
        b.relay.inject(&filler(&community, i, now()), &set());
    }
    let paged = read_control_paged(&wire, &community).await.unwrap();
    wire.served();
    let read = read_control(&wire, &community).await.unwrap();
    assert_eq!(outcome(&read), outcome(&paged));
    assert_eq!(edition_ids(&read), edition_ids(&paged), "read by pages, off the relays");
    assert!(control_sync::flooded(&community, &View::of(&community)));
    wire.served();
    read_control(&wire, &community).await.unwrap();
    assert!(b.relay.reconciled() == 0 || wire.served() > 0, "left to the pager until its hour is up");
}

#[tokio::test]
async fn relays_answering_late_never_mark_a_read_truncated() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    wire.fault(|f| f.down.extend([SET[1].to_string(), SET[2].to_string()]));
    let read = read_control(&wire, &community).await.unwrap();
    assert!(!read.truncated, "one relay of three is an answer, as it is for a read by pages");
    let stored = crate::db::community::get_community_roles(&community.id().to_hex()).unwrap();
    follow_control(&wire, &community).await.unwrap();
    assert_eq!(crate::db::community::get_community_roles(&community.id().to_hex()).unwrap(), stored);
}

#[tokio::test]
async fn a_wrap_held_that_no_longer_opens_is_forgotten_and_fetched_again() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    let view = View::of(&community);
    let cid = community.id().to_hex();
    // Stored as a body, though it opens as nothing: as a wrap would be that a stricter build refuses.
    let stale = junk(&community, now());
    b.relay.inject(&stale, &set());
    crate::db::community::store_control_wraps(&cid, &view.id(), control_store::OPEN_RULES, now(), &[(stale.id.to_bytes(), stale.created_at.as_secs(), Some(stale.as_json().into_bytes()))]).unwrap();
    let bodies = view.counts().unwrap().0;
    read_control(&wire, &community).await.unwrap();
    assert_eq!(view.counts().unwrap().0, bodies - 1, "no longer held as an edition");
    read_control(&wire, &community).await.unwrap();
    assert_eq!(view.counts().unwrap().1, 1, "asked for again, and judged for what it is");
}

#[tokio::test]
async fn a_read_with_something_known_unread_is_truncated() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    // Every relay read to the end today, so none is read by pages for what it will not serve.
    read_whole(&wire, &community, &View::of(&community)).await.unwrap();
    assert!(!read_control(&wire, &community).await.unwrap().truncated);
    let lone = filler(&community, 1, now());
    b.relay.inject(&lone, &only(SET[0]));
    // Named, and neither served by id nor to be had by pages just now.
    wire.fault(|f| {
        f.withhold.insert(SET[0].into());
        f.pageless.insert(SET[0].into());
    });
    let read = read_control(&wire, &community).await.unwrap();
    assert!(read.truncated, "a wrap a relay named and nobody could be asked for is unread, and the read says so");
    wire.fault(|f| f.pageless.clear());
    assert!(!read_control(&wire, &community).await.unwrap().truncated, "its pages hand it over");
}

fn inner_ids(editions: &[ParsedEdition]) -> std::collections::BTreeSet<[u8; 32]> {
    editions.iter().map(|e| e.inner_id).collect()
}

#[tokio::test]
async fn a_whole_read_is_every_edition_and_downloads_none_already_held() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    let paged = fetch_control_plane_whole_paged(&wire, &community).await.unwrap();
    follow_control(&wire, &community).await.unwrap();
    wire.served();
    b.relay.reconciled();
    let whole = fetch_control_plane_whole(&wire, &community).await.unwrap();
    assert_eq!(inner_ids(&whole), inner_ids(&paged));
    assert_eq!((wire.served(), b.relay.reconciled()), (0, 3), "the wraps held, and a question to each relay");
    // An edition written here since shows without a download, one written elsewhere costs itself.
    let held = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
    create_public_channel(&wire, &held, "after").await.unwrap();
    b.relay.inject(&filler(&community, 1, now()), &set());
    wire.served();
    let held = crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
    assert_eq!(fetch_control_plane_whole(&wire, &held).await.unwrap().len(), paged.len() + 2);
    assert_eq!(wire.served(), 1);
}

#[tokio::test]
async fn a_whole_read_with_anything_unread_or_unreachable_is_none() {
    let b = bed().await;
    let mut community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    assert!(fetch_control_plane_whole(&wire, &community).await.is_some());
    let lone = filler(&community, 1, now());
    b.relay.inject(&lone, &only(SET[0]));
    wire.fault(|f| {
        f.withhold.insert(SET[0].into());
        f.pageless.insert(SET[0].into());
    });
    assert!(fetch_control_plane_whole(&wire, &community).await.is_none(), "a wrap named and not to be had");
    wire.fault(|f| {
        f.withhold.clear();
        f.pageless.clear();
        f.down.extend(set());
    });
    assert!(fetch_control_plane_whole(&wire, &community).await.is_none(), "no relay to ask");
    // A second no relay's pages get past, with none comparing sets.
    wire.fault(|f| f.down.clear());
    community.relays = only(SET[0]);
    crate::db::community::save_community_v2(&community).unwrap();
    wall_at(&b, &community, now() - 9_000, &only(SET[0]));
    crate::clock::shift_for_test(2 * 1000 * super::super::guestbook_reads::REREAD_SECS as i64);
    assert!(fetch_control_plane_whole(&Wire::paged(&b.relay), &community).await.is_none());
    // Once a pass finds the relay comparing sets, the second is whole.
    follow_control(&Wire::new(&b.relay), &community).await.unwrap();
    assert!(fetch_control_plane_whole(&Wire::new(&b.relay), &community).await.is_some(), "compared as a set, the second is whole");
}

#[tokio::test]
async fn a_whole_read_is_what_the_relays_hold_now_and_a_follow_is_what_this_device_knows() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    let whole = fetch_control_plane_whole(&wire, &community).await.unwrap();
    // Every relay loses one wrap; two of the three lose another.
    let view = View::of(&community);
    let mut ids: Vec<EventId> = view.page(None, 500).unwrap().iter().map(|w| w.id).collect();
    ids.sort();
    let (gone, kept_by_one) = (ids[0], ids[1]);
    wire.fault(|f| {
        f.lost = SET.iter().map(|r| (r.to_string(), HashSet::from([gone]))).collect();
        f.lost.get_mut(SET[0]).unwrap().insert(kept_by_one);
        f.lost.get_mut(SET[1]).unwrap().insert(kept_by_one);
    });
    let now_whole = fetch_control_plane_whole(&wire, &community).await.unwrap();
    assert_eq!(now_whole.len(), whole.len() - 1, "what no relay holds is not on the plane, and what one holds is");
    assert_eq!(inner_ids(&now_whole), inner_ids(&fetch_control_plane_whole_paged(&wire, &community).await.unwrap()), "as their pages say");
    assert_eq!(read_control(&wire, &community).await.unwrap().editions.len(), whole.len(), "a follow still folds it");
}

#[tokio::test]
async fn a_whole_read_goes_by_pages_while_a_relay_cannot_say_what_it_lacks() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    wire.fault(|f| { let _ = f.pages_only.insert(SET[2].into()); });
    follow_control(&wire, &community).await.unwrap();
    fetch_control_plane_whole(&wire, &community).await.unwrap();
    wire.served();
    b.relay.reconciled();
    let whole = fetch_control_plane_whole(&wire, &community).await.unwrap();
    assert_eq!(inner_ids(&whole), inner_ids(&fetch_control_plane_whole_paged(&wire, &community).await.unwrap()));
    assert_eq!(b.relay.reconciled(), 2, "the relays that compare sets are asked");
    wire.served();
    fetch_control_plane_whole(&wire, &community).await.unwrap();
    assert!(wire.served_by(SET[2]) as usize >= whole.len() && wire.served_by(SET[0]) == 0 && wire.served_by(SET[1]) == 0, "and only the one that can't is read by pages");
}

#[tokio::test]
async fn a_relay_whose_index_stops_short_says_nothing_of_what_it_lacks() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    let old: Vec<Event> = (0..5).map(|i| filler(&community, i, now() - 90 * 86_400 + i)).collect();
    for w in &old {
        b.relay.inject(w, &only(SET[0]));
    }
    follow_control(&wire, &community).await.unwrap();
    // It holds them still, outside what its index covers.
    wire.fault(|f| { let _ = f.index_from.insert(SET[0].into(), now() - 3_600); });
    let whole = fetch_control_plane_whole(&wire, &community).await.unwrap();
    assert_eq!(inner_ids(&whole), inner_ids(&fetch_control_plane_whole_paged(&wire, &community).await.unwrap()));
    assert_eq!(whole.len(), View::of(&community).counts().unwrap().0, "the old ones among them");
}

#[tokio::test]
async fn the_audit_log_reads_past_epochs_from_what_is_held() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    let held = || crate::db::community::load_community_v2(community.id()).unwrap().unwrap();
    refound_community(&wire, &held(), &[]).await.unwrap();
    follow_control(&wire, &held()).await.unwrap();
    assert_ne!(held().root_epoch, community.root_epoch);
    let first = audit_log(&wire, &held()).await.unwrap();
    wire.served();
    let again = audit_log(&wire, &held()).await.unwrap();
    assert_eq!(again.entries.len(), first.entries.len());
    assert_eq!(wire.served(), 0, "both epochs' planes are held, and each keeps its own relays' reads");
}

#[tokio::test]
async fn a_whole_read_of_a_community_not_held_goes_by_pages() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    let paged = fetch_control_plane_whole_paged(&wire, &community).await.unwrap();
    crate::db::community::delete_community(&community.id().to_hex()).unwrap();
    assert_eq!(inner_ids(&fetch_control_plane_whole(&wire, &community).await.unwrap()), inner_ids(&paged));
    assert!(View::of(&community).held().unwrap().is_empty());
}

// ── Rekey and access planes ──────────────────────────────────────────────────

/// A wrap at `group`'s plane that opens as nothing.
fn plane_junk(group: &GroupKey, secs: u64) -> Event {
    EventBuilder::new(Kind::Custom(stream::KIND_WRAP), "not a wrap").custom_created_at(Timestamp::from_secs(secs)).finalize(group.keys()).unwrap()
}

#[tokio::test]
async fn a_rekey_plane_is_read_as_its_pages_read_it_and_downloaded_once() {
    let b = bed().await;
    let community = b.community.clone();
    let plane = base_rekey_group_key(&community.community_root, community.id(), Epoch(1));
    publish_base_rotation(&b.relay, &community, &b.owner, &[b.owner.public_key()], &[0xA1; 32], &community.community_root).await;
    b.relay.inject(&plane_junk(&plane, now()), &set());
    let wire = Wire::new(&b.relay);
    let paged = plane_wraps_paged(&wire, &community.relays, &plane).await.unwrap();
    wire.served();
    let held = super::super::plane_wraps(&wire, &community, &plane).await.unwrap();
    let opens: HashSet<EventId> = paged.iter().filter(|w| stream::open_wrap(w, &plane).is_ok()).map(|w| w.id).collect();
    assert_eq!(held.iter().map(|w| w.id).collect::<HashSet<_>>(), opens, "every wrap that opens; the junk is held as refused");
    assert_eq!(wire.served() as usize, paged.len(), "each wrap once, from one relay, the junk too");
    assert!(fetch_rekey_chunks(&wire, &community, &plane).await.unwrap().iter().any(|c| c.rotator == b.owner.public_key()));
    assert_eq!(wire.served(), 0, "and never again");
}

#[tokio::test]
async fn a_plane_read_is_what_the_relays_hold_now() {
    let b = bed().await;
    let community = b.community.clone();
    let plane = base_rekey_group_key(&community.community_root, community.id(), Epoch(1));
    publish_base_rotation(&b.relay, &community, &b.owner, &[b.owner.public_key()], &[0xA1; 32], &community.community_root).await;
    let wire = Wire::new(&b.relay);
    let held = super::super::plane_wraps(&wire, &community, &plane).await.unwrap();
    assert!(!held.is_empty());
    // Every relay drops them: a delivery nobody can fetch is not on the plane.
    let gone: HashSet<EventId> = held.iter().map(|w| w.id).collect();
    wire.fault(|f| f.lost = SET.iter().map(|r| (r.to_string(), gone.clone())).collect());
    assert!(super::super::plane_wraps(&wire, &community, &plane).await.unwrap().is_empty());
    // One keeps them: they are.
    wire.fault(|f| { f.lost.remove(SET[2]); });
    assert_eq!(super::super::plane_wraps(&wire, &community, &plane).await.unwrap().len(), held.len());
}

#[tokio::test]
async fn a_plane_with_a_relay_that_cannot_compare_sets_reads_only_that_one_by_pages() {
    let b = bed().await;
    let community = b.community.clone();
    let plane = base_rekey_group_key(&community.community_root, community.id(), Epoch(1));
    publish_base_rotation(&b.relay, &community, &b.owner, &[b.owner.public_key()], &[0xA1; 32], &community.community_root).await;
    let wire = Wire::new(&b.relay);
    wire.fault(|f| { let _ = f.pages_only.insert(SET[1].into()); });
    let paged = plane_wraps_paged(&wire, &community.relays, &plane).await.unwrap();
    let held = super::super::plane_wraps(&wire, &community, &plane).await.unwrap();
    assert_eq!(held.len(), paged.len());
    wire.served();
    super::super::plane_wraps(&wire, &community, &plane).await.unwrap();
    assert!(wire.served_by(SET[1]) as usize >= paged.len() && wire.served_by(SET[0]) == 0 && wire.served_by(SET[2]) == 0, "that relay's pages, and nothing from the others");
}

#[tokio::test]
async fn a_plane_pass_cut_short_by_its_budget_is_read_by_pages() {
    let b = bed().await;
    let community = b.community.clone();
    let plane = base_rekey_group_key(&community.community_root, community.id(), Epoch(1));
    for i in 0..6 {
        let rumor = stream::build_rumor_secs(1, b.owner.public_key(), &format!("w{i}"), vec![], now() - 100 + i);
        let seal = stream::build_seal(&rumor, stream::SealForm::Encrypted, &plane, &b.owner).unwrap();
        b.relay.inject(&stream::wrap_seal(&seal, &plane, stream::KIND_WRAP, Timestamp::from_secs(now() - 100 + i)).unwrap().0, &set());
    }
    control_sync::set_budget_for_test(Some(2));
    let wire = Wire::new(&b.relay);
    assert_eq!(super::super::plane_wraps(&wire, &community, &plane).await.unwrap().len(), 6, "every wrap, the pass's budget notwithstanding");
}

#[tokio::test]
async fn a_whole_read_with_a_relay_down_still_reads_through_the_wraps_held() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    let whole = fetch_control_plane_whole(&wire, &community).await.unwrap();
    wire.fault(|f| { let _ = f.down.insert(SET[2].into()); });
    wire.served();
    assert_eq!(inner_ids(&fetch_control_plane_whole(&wire, &community).await.unwrap()), inner_ids(&whole));
    assert_eq!(wire.served(), 0, "a relay that is down is not one to read by pages");
}

/// More wraps dated `secs` than a page carries, on `relays`.
fn wall_at(b: &Bed, community: &CommunityV2, secs: u64, relays: &[String]) {
    for i in 0..FOLLOW_PAGE as u64 + 40 {
        b.relay.inject(&filler(community, i, secs), relays);
    }
}

/// Nothing held of `community` yet, as on the device a member just joined from.
fn cold(community: &CommunityV2) {
    crate::db::community::delete_community(&community.id().to_hex()).unwrap();
    crate::db::community::save_community_v2(community).unwrap();
}

#[tokio::test]
async fn a_second_holding_more_than_a_page_leaves_a_read_by_pages_truncated_and_is_never_read_twice() {
    let b = bed().await;
    let mut community = lived_in(&b).await;
    community.relays = only(SET[0]);
    wall_at(&b, &community, now() - 9_000, &only(SET[0]));
    cold(&community);
    let wire = Wire::paged(&b.relay);
    let read = read_control_held(&wire, &community).await.ok().expect("read through the store");
    assert!(read.truncated, "what else that second holds cannot be asked for");
    assert!(read.authority.roles.grants.len() == 12, "though the read went on below it");
    follow_control(&wire, &community).await.unwrap();
    assert_eq!(crate::db::community::get_community_roles_at(&community.id().to_hex()).unwrap(), 0, "and no roster is banked on it");
    wire.served();
    let read = read_control_held(&wire, &community).await.ok().expect("read through the store");
    assert!(read.truncated, "it stays so");
    assert!((wire.served() as usize) < FOLLOW_PAGE, "without the second being read again");
}

#[tokio::test]
async fn a_relay_that_compares_sets_has_no_wall_and_covers_one_that_does() {
    let b = bed().await;
    let mut community = lived_in(&b).await;
    community.relays = vec![SET[0].to_string(), SET[1].to_string()];
    wall_at(&b, &community, now() - 9_000, &community.relays);
    cold(&community);
    // Both read by pages, both walled at the one second: nothing reaches past it.
    let paged = Wire::paged(&b.relay);
    assert!(read_control_held(&paged, &community).await.ok().unwrap().truncated);
    // One of them compares sets: every wrap of that second is held, by id.
    let wire = Wire::new(&b.relay);
    wire.fault(|f| { let _ = f.break_off.insert(SET[1].into()); });
    let read = read_control_held(&wire, &community).await.ok().unwrap();
    assert!(!read.truncated, "the other's wall is covered");
    assert_eq!(View::of(&community).counts().unwrap().0, plane_size(&b, SET[0]));
    // Its pages meet the same second, and say nothing its set did not.
    crate::clock::shift_for_test(2 * 1000 * super::super::guestbook_reads::REREAD_SECS as i64);
    read_whole(&wire, &community, &View::of(&community)).await.unwrap();
    assert_eq!(control_sync::wall(&community, &View::of(&community), None), None);
}

#[tokio::test]
async fn a_wall_is_forgotten_once_the_relay_compares_sets() {
    let b = bed().await;
    let mut community = lived_in(&b).await;
    community.relays = only(SET[0]);
    wall_at(&b, &community, now() - 9_000, &only(SET[0]));
    cold(&community);
    let view = View::of(&community);
    assert!(read_control_held(&Wire::paged(&b.relay), &community).await.ok().unwrap().truncated);
    assert!(!read_control_held(&Wire::new(&b.relay), &community).await.ok().unwrap().truncated);
    assert_eq!(view.counts().unwrap().0, plane_size(&b, SET[0]), "every wrap of that second is held now");
    // Read by pages again, from where it was: the second is not walled anew.
    assert!(!read_control_held(&Wire::paged(&b.relay), &community).await.ok().unwrap().truncated);
    assert_eq!(control_sync::wall(&community, &view, None), None);
}

#[tokio::test]
async fn a_wall_one_relay_has_and_another_does_not_is_covered() {
    let b = bed().await;
    let mut community = lived_in(&b).await;
    community.relays = vec![SET[0].to_string(), SET[1].to_string()];
    let at = now() - 9_000;
    wall_at(&b, &community, at, &only(SET[0]));
    cold(&community);
    let wire = Wire::paged(&b.relay);
    let read = read_control_held(&wire, &community).await.ok().expect("read through the store");
    assert!(!read.truncated, "as a union of their pages reads on past it");
    wire.fault(|f| { let _ = f.down.insert(SET[1].into()); });
    assert!(read_control_held(&wire, &community).await.ok().unwrap().truncated, "which it does only while it is there to be read");
    wire.fault(|f| f.down.clear());
    // The same second fills a page on the other too: now neither reaches past it.
    wall_at(&b, &community, at, &only(SET[1]));
    crate::clock::shift_for_test(2 * 1000 * super::super::guestbook_reads::REREAD_SECS as i64);
    read_whole(&wire, &community, &View::of(&community)).await.unwrap();
    assert!(read_control_held(&wire, &community).await.ok().unwrap().truncated);
}

#[tokio::test]
async fn a_wall_above_what_a_device_tracks_truncates_its_reads() {
    let b = bed().await;
    let mut community = lived_in(&b).await;
    community.relays = only(SET[0]);
    crate::db::community::save_community_v2(&community).unwrap();
    follow_control(&Wire::new(&b.relay), &community).await.unwrap();
    // Later the relay reconciles no more, and holds a second fuller than a page, newer than
    // every head tracked.
    let wire = Wire::paged(&b.relay);
    assert!(!read_control_held(&wire, &community).await.ok().unwrap().truncated);
    crate::clock::shift_for_test(600_000);
    wall_at(&b, &community, now() - 60, &only(SET[0]));
    let read = read_control_held(&wire, &community).await.ok().unwrap();
    assert!(read.truncated, "the fold read on below a second it cannot get whole");
}

#[tokio::test]
async fn a_wall_below_everything_a_device_tracks_does_not_truncate_its_reads() {
    let b = bed().await;
    let mut community = lived_in(&b).await;
    community.relays = only(SET[0]);
    crate::db::community::save_community_v2(&community).unwrap();
    let wire = Wire::paged(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    wall_at(&b, &community, now() - 9_000, &only(SET[0]));
    crate::clock::shift_for_test(2 * 1000 * super::super::guestbook_reads::REREAD_SECS as i64);
    read_whole(&wire, &community, &View::of(&community)).await.unwrap();
    assert!(control_sync::wall(&community, &View::of(&community), None).is_some());
    assert!(!read_control_held(&wire, &community).await.ok().unwrap().truncated, "every head it tracks sits above the wall");
}

#[tokio::test(start_paused = true)]
async fn a_tracked_entity_missing_from_the_fold_waits_for_every_relay_before_any_is_read_by_pages() {
    let b = bed().await;
    let community = lived_in(&b).await;
    let wire = Wire::new(&b.relay);
    follow_control(&wire, &community).await.unwrap();
    let view = View::of(&community);
    read_whole(&wire, &community, &view).await.unwrap();
    // A grant this device tracks, whose edition it no longer holds and only the slow relay still has.
    let control = control::ControlPlane::of(&community);
    let grant = view.page(None, 500).unwrap().into_iter().find(|w| control.open_edition(w).is_ok_and(|e| e.vsk == vsk::GRANT)).expect("a grant edition");
    view.forget(&grant.id);
    wire.fault(|f| {
        for relay in &SET[..2] {
            f.lost.entry(relay.to_string()).or_default().insert(grant.id);
        }
        f.slow.insert(SET[2].into(), std::time::Duration::from_secs(30));
    });
    wire.served();
    let read = read_control(&wire, &community).await.unwrap();
    assert!(read.editions.iter().any(|e| e.vsk == vsk::GRANT && control.open_edition(&grant).is_ok_and(|g| g.inner_id == e.inner_id)), "the relay the grace would have left out was waited for");
    assert!(!read.truncated && !read.authority.gapped);
    assert_eq!(wire.served(), 1, "its answer was enough: no relay was read by pages for it");
}

// ── Over real sockets ────────────────────────────────────────────────────────

use crate::community::transport::wire::WireRelay;
use crate::community::transport::LiveTransport;

struct Live {
    _tmp: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
    _on: On,
    transport: LiveTransport,
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = crate::state::take_nostr_client();
        crate::community::transport::clear_plane_pool();
    }
}

fn live(timeout_secs: u64) -> Live {
    let (tmp, guard, _owner) = init_test_db();
    let on = On::new();
    crate::transport::set_preference(Some(crate::transport::Kind::Clearnet));
    crate::state::set_nostr_client(crate::nostr_client_builder().build());
    Live { _tmp: tmp, _guard: guard, _on: on, transport: LiveTransport::with_timeout(std::time::Duration::from_secs(timeout_secs)) }
}

fn plane_wraps(plane: &Keys, secs: impl IntoIterator<Item = u64>) -> Vec<Event> {
    secs.into_iter().map(|s| EventBuilder::new(Kind::Custom(stream::KIND_WRAP), "sealed").custom_created_at(Timestamp::from_secs(s)).finalize(plane).unwrap()).collect()
}

fn plane_query(plane: &Keys) -> Query {
    Query { kinds: vec![stream::KIND_WRAP], authors: vec![plane.public_key().to_hex()], ..Default::default() }
}

fn ids(wraps: &[Event]) -> Vec<(EventId, u64)> {
    wraps.iter().map(|w| (w.id, w.created_at.as_secs())).collect()
}

fn sorted(mut ids: Vec<EventId>) -> Vec<EventId> {
    ids.sort_unstable();
    ids
}

async fn reconcile(l: &Live, relay: &WireRelay, plane: &Keys, held: &[Event], budget: usize) -> Result<Diff, ReconcileFail> {
    l.transport.reconciler().unwrap().reconcile(&plane_query(plane), &ids(held), &relay.url(), budget).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reconcile_over_a_socket_names_what_each_side_lacks() {
    let l = live(5);
    let relay = WireRelay::start().await;
    let plane = Keys::generate();
    let wraps = plane_wraps(&plane, 1..=40);
    relay.hold(wraps[..30].iter().cloned());
    relay.hold(plane_wraps(&Keys::generate(), 1..=5));
    let ours: Vec<Event> = wraps[20..].to_vec();
    let diff = reconcile(&l, &relay, &plane, &ours, 1_000).await.unwrap();
    assert_eq!(sorted(diff.need), sorted(wraps[..20].iter().map(|w| w.id).collect()), "what the relay holds of this plane and we don't");
    assert_eq!(sorted(diff.lack), sorted(wraps[30..].iter().map(|w| w.id).collect()), "what we hold and the relay doesn't");
    assert!(diff.complete);
    let same = reconcile(&l, &relay, &plane, &wraps[..30], 1_000).await.unwrap();
    assert_eq!(same, Diff { need: Vec::new(), lack: Vec::new(), complete: true });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reconcile_stops_at_its_budget_and_keeps_what_it_named() {
    let l = live(5);
    let relay = WireRelay::start().await;
    let plane = Keys::generate();
    let wraps = plane_wraps(&plane, 1..=300);
    relay.hold(wraps.iter().cloned());
    let diff = reconcile(&l, &relay, &plane, &[], 50).await.unwrap();
    assert_eq!(diff.need.len(), 50);
    assert!(!diff.complete, "more to name than a pass reads");
    assert!(diff.need.iter().all(|id| wraps.iter().any(|w| w.id == *id)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relay_without_nip77_is_remembered_for_a_day() {
    let l = live(5);
    let relay = WireRelay::start().await;
    let plane = Keys::generate();
    relay.hold(plane_wraps(&plane, 1..=3));
    relay.no_nip77(false);
    assert_eq!(reconcile(&l, &relay, &plane, &[], 100).await, Err(ReconcileFail::Unsupported));
    assert_eq!(relay.neg_opens(), 1);
    assert_eq!(crate::negentropy::neg_supported_cached(&relay.url()), None, "what it does for our own DM reads is another matter");
    assert_eq!(reconcile(&l, &relay, &plane, &[], 100).await, Err(ReconcileFail::Unsupported));
    assert_eq!(relay.neg_opens(), 1, "not asked again that day");
    crate::clock::shift_for_test(25 * 3_600_000);
    let _ = reconcile(&l, &relay, &plane, &[], 100).await;
    assert_eq!(relay.neg_opens(), 2, "a day on it is asked again: relays gain NIP-77 with an upgrade");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn silence_is_no_nip77_only_from_a_relay_that_serves_reads() {
    let l = live(1);
    let plane = Keys::generate();
    let quiet = WireRelay::start().await;
    quiet.hold(plane_wraps(&plane, 1..=3));
    quiet.no_nip77(true);
    assert_eq!(reconcile(&l, &quiet, &plane, &[], 100).await, Err(ReconcileFail::Unsupported), "it answered a plain read, and not this");
    assert_eq!(reconcile(&l, &quiet, &plane, &[], 100).await, Err(ReconcileFail::Unsupported));
    assert_eq!(quiet.neg_opens(), 1);

    // One that has reconciled before and falls silent is slow, not unable.
    let slow = WireRelay::start().await;
    slow.hold(plane_wraps(&plane, 1..=3));
    assert!(reconcile(&l, &slow, &plane, &[], 100).await.is_ok());
    slow.no_nip77(true);
    assert!(matches!(reconcile(&l, &slow, &plane, &[], 100).await, Err(ReconcileFail::Failed(_))));
    slow.nip77();
    assert_eq!(slow.neg_opens(), 2);
    let _ = reconcile(&l, &slow, &plane, &[], 100).await;
    assert_eq!(slow.neg_opens(), 3, "and is asked again the next time");

    // One that answers nothing at all was not reached: silence alone is no verdict.
    let dead = WireRelay::start().await;
    dead.hold(plane_wraps(&plane, 1..=3));
    dead.no_nip77(true);
    dead.hang_up(true);
    assert_eq!(reconcile(&l, &dead, &plane, &[], 100).await, Err(ReconcileFail::Unreached));
    use crate::community::transport::plane_reconciles;
    assert_eq!((plane_reconciles(&quiet.url()), plane_reconciles(&slow.url()), plane_reconciles(&dead.url())), (Some(false), Some(true), None), "and nothing is held against it");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gate_on_reconciling_is_not_a_verdict() {
    let l = live(2);
    let relay = WireRelay::start().await;
    let plane = Keys::generate();
    relay.hold(plane_wraps(&plane, 1..=3));
    relay.gate(true);
    assert_eq!(reconcile(&l, &relay, &plane, &[], 100).await, Err(ReconcileFail::Gated));
    relay.gate(false);
    assert!(reconcile(&l, &relay, &plane, &[], 100).await.is_ok(), "nothing was held against it");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relay_that_caps_open_sessions_is_never_written_off_and_every_plane_still_compares() {
    let l = live(6);
    let relay = WireRelay::start().await;
    relay.cap_neg_sessions(8);
    let planes: Vec<Keys> = (0..24).map(|_| Keys::generate()).collect();
    for plane in &planes {
        relay.hold(plane_wraps(plane, 1..=3));
    }
    let reconciler = l.transport.reconciler().unwrap();
    let url = relay.url();
    let url = &url;
    let asks = planes.iter().map(|plane| async move { reconciler.reconcile(&plane_query(plane), &[], url, 100).await });
    let answers = futures_util::future::join_all(asks).await;
    assert!(answers.iter().all(|a| a.as_ref().is_ok_and(|d| d.need.len() == 3)), "kept under the cap, every one is answered: {answers:?}");
    // Past it, the relay's refusal is a retry, never a verdict about NIP-77.
    relay.cap_neg_sessions(1);
    let answers = futures_util::future::join_all(planes.iter().map(|plane| async move { reconciler.reconcile(&plane_query(plane), &[], url, 100).await })).await;
    assert!(answers.iter().all(|a| !matches!(a, Err(ReconcileFail::Unsupported))));
    assert_eq!(crate::community::transport::plane_reconciles(&relay.url()), Some(true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plane_on_a_relay_that_gates_it_is_read_as_the_plane() {
    let l = live(4);
    let (owner, relay) = (Keys::generate(), WireRelay::start().await);
    let memory = MemoryRelay::new();
    crate::state::MY_SECRET_KEY.store_from_keys(&owner, &[]);
    crate::state::set_my_public_key(owner.public_key());
    let community = create_community(&memory, "Gated", vec![relay.url()], None).await.unwrap();
    let plane = base_rekey_group_key(&community.community_root, community.id(), Epoch(1));
    publish_base_rotation(&memory, &community, &owner, &[owner.public_key()], &[0xA1; 32], &community.community_root).await;
    let wraps = memory.fetch(&plane_query(plane.keys()), &community.relays).await.unwrap();
    assert!(!wraps.is_empty());
    relay.hold(wraps.clone());
    relay.gate(true);
    let held = super::super::plane_wraps(&l.transport, &community, &plane).await.unwrap();
    assert_eq!(held.len(), wraps.len());
    let view = super::super::plane_store::PlaneView::of(&community, &plane);
    assert_eq!(Held::held(&view).unwrap().len(), wraps.len(), "through the wraps held, compared as sets as the plane");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relay_gating_a_plane_reconciles_it_on_a_connection_authed_as_the_plane() {
    let l = live(4);
    let relay = WireRelay::start().await;
    let plane = Keys::generate();
    let wraps = plane_wraps(&plane, 1..=3);
    relay.hold(wraps.clone());
    relay.gate(true);
    let reconciler = l.transport.reconciler().unwrap();
    assert_eq!(reconciler.reconcile(&plane_query(&plane), &[], &relay.url(), 100).await, Err(ReconcileFail::Gated), "as the user it is gated");
    let diff = reconciler.reconcile_plane(&plane, &plane_query(&plane), &ids(&wraps[..1]), &relay.url(), 100).await.unwrap();
    assert_eq!(sorted(diff.need), sorted(wraps[1..].iter().map(|w| w.id).collect()), "as the plane it compares");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pass_over_sockets_fills_the_store_and_the_next_asks_for_nothing() {
    let l = live(5);
    let memory = MemoryRelay::new();
    let community = create_community(&memory, "Wired", vec!["wss://r".into()], None).await.unwrap();
    for i in 0..30 {
        memory.inject(&filler(&community, i, now() - 500 + i), &community.relays);
    }
    let plane = memory.fetch(&Query { kinds: vec![stream::KIND_WRAP], authors: vec![control::ControlPlane::of(&community).pk_hex()], ..Default::default() }, &community.relays).await.unwrap();
    let (reconciles, pages) = (WireRelay::start().await, WireRelay::start().await);
    reconciles.hold(plane.iter().cloned());
    pages.hold(plane.iter().cloned());
    pages.no_nip77(false);
    // A wrap only the relay without NIP-77 holds: its pages are what brings it.
    let lone = filler(&community, 99, now() - 900);
    pages.hold([lone.clone()]);
    let wired = CommunityV2 { relays: vec![reconciles.url(), pages.url()], ..community.clone() };
    crate::db::community::delete_community(&community.id().to_hex()).unwrap();
    crate::db::community::save_community_v2(&wired).unwrap();
    let view = View::of(&wired);
    let sync = sync_plane(&l.transport, &wired, &view, Wait::All).await.unwrap();
    assert_eq!(sync.opened, plane.len() + 1);
    assert_eq!((sync.relays[&reconciles.url()], sync.relays[&pages.url()]), (Relayed::Whole, Relayed::Whole));
    assert!(holds(&view, &lone));
    assert_eq!(sync.lacking[&reconciles.url()], 0, "asked before the lone wrap was held");
    let sync = sync_plane(&l.transport, &wired, &view, Wait::All).await.unwrap();
    assert_eq!(sync.opened, 0);
    assert_eq!(sync.lacking[&reconciles.url()], 1, "and now it shows as lacking it");
    let read = read_control(&l.transport, &wired).await.unwrap();
    assert!(!read.truncated && read.editions.len() == plane.len() + 1);
}

#[tokio::test]
async fn a_swap_during_a_pass_stores_nothing_under_either_account() {
    let b = bed().await;
    let view = View::of(&b.community);
    for i in 0..20 {
        b.relay.inject(&filler(&b.community, i, now() - 100 + i), &set());
    }
    let starting = crate::db::current_session();
    let before = view.held().unwrap().len();
    // Its first read answered, the account changes under the pass.
    let swap = SwapMidFetch::arm(&b.relay);
    let sync = sync_plane(&swap, &b.community, &view, Wait::All).await;
    assert!(sync.is_ok_and(|sync| sync.opened == 0 && sync.unread), "the pass ends short, with what it could not keep unread");
    assert!(view.held().unwrap().is_empty(), "the account on screen now was handed none of it");
    // Nor the one the pass began under: the key that seals its rows left with the swap.
    let held = crate::db::with_session(starting, async { view.held().unwrap().len() }).await;
    assert_eq!(held, before);
}
