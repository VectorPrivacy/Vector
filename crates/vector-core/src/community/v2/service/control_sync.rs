//! One pass of a plane's wraps as held here ([`Held`]: a Control Plane view, a Guestbook)
//! against its relays. Each is asked what it holds that this
//! device doesn't (NIP-77), every wrap named is fetched once from one relay that named it, and a
//! relay that can't reconcile is read by pages from its own cursor.
//!
//! A relay's "in sync" is its word. A read to the end of its plane backs it when the relay's
//! answer shows its index stopping short of the plane, and whenever a caller asks for one
//! ([`read_whole`]): at most once a day per relay.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures_util::stream::{FuturesUnordered, StreamExt};
use nostr_sdk::prelude::EventId;
use serde::{Deserialize, Serialize};

use super::guestbook_reads::{Plan, RelayRead, OVERLAP_SECS, REREAD_SECS};
use super::{plane_page, stream, CommunityV2, Query, Transport, FOLLOW_MAX_PAGES, FOLLOW_PAGE};
use crate::community::transport::{Diff, Evidence, ReconcileFail, QUORUM_GRACE_MS, RESIDUAL_GRACE_MS};

/// The wraps one pass may download for one plane.
const BUDGET: usize = FOLLOW_MAX_PAGES * FOLLOW_PAGE;

#[cfg(test)]
std::thread_local! {
    static TEST_BUDGET: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn set_budget_for_test(budget: Option<usize>) {
    TEST_BUDGET.with(|b| b.set(budget));
}

fn budget_of_a_pass() -> usize {
    #[cfg(test)]
    if let Some(budget) = TEST_BUDGET.with(|b| b.get()) {
        return budget;
    }
    BUDGET
}

/// Wraps asked of a relay by id in one request.
const FETCH_BATCH: usize = 200;

/// A relay no pass has waited out for this long is waited out again, so one always slower than
/// the grace is still heard.
const HEARD_SECS: u64 = 6 * 3600;

/// How long a flooded view is left to the pager before it is reconciled again, by how many times
/// running it has been.
const FLOOD_BACKOFF_SECS: [u64; 5] = [3_600, 7_200, 14_400, 28_800, 86_400];

/// Above any real wrap: a read from here starts at the plane's newest.
pub(super) const FROM_THE_TOP: u64 = 4_102_444_800;

/// How one relay's pass ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Relayed {
    /// Its set was compared with ours, or it was read to the end, and all it has is held.
    Whole,
    /// Read by pages from its cursor on.
    Since,
    /// It answered, and something it has is still unread.
    Partial,
    /// No answer, or not waited for.
    Unreached,
}

#[derive(Debug, Default)]
pub(super) struct PlaneSync {
    pub relays: BTreeMap<String, Relayed>,
    /// Wraps that opened and are held as of this pass.
    pub opened: usize,
    /// Something a relay named or holds is known to be unread.
    pub unread: bool,
    /// The view names more than a pass reads, or is at a cap: read it by pages, as before.
    pub flooded: bool,
    /// What each relay lacks of what is held here.
    pub lacking: BTreeMap<String, usize>,
    /// The wraps held that no relay heard holds. Known only when every one of them compared its
    /// whole set with ours: a relay read by pages, or by an index that stops short, says nothing
    /// of what it lacks.
    pub unserved: Option<Vec<EventId>>,
    /// What each relay that compared its whole set lacks of what is held here.
    pub lacks: BTreeMap<String, Vec<EventId>>,
}

/// What became of the wraps handed to [`Held::absorb`].
#[derive(Debug, Default)]
pub(super) struct Absorbed {
    /// Opened here and held now.
    pub opened: usize,
    /// Refused, and held as that.
    pub refused: usize,
    /// Not the events their ids name: never held.
    pub unfit: Vec<EventId>,
    /// A cap was reached and something was left out.
    pub full: bool,
}

/// One plane's wraps as held here: what a pass reads into. An id is held only with what came of
/// its wrap, and only once the id and signature were checked against the event itself.
pub(super) trait Held: Sync {
    /// Names the plane as read with these keys: the pass's lock and its state go by it.
    fn id(&self) -> [u8; 32];
    /// The plane's address.
    fn pk(&self) -> nostr_sdk::prelude::PublicKey;
    /// What a failure to read it calls it.
    fn name(&self) -> &'static str;
    /// The settings key its relays' reads are kept under.
    fn state_key(&self, cid_hex: &str) -> String;
    /// Every wrap held, refused ones too: what a relay is told this device has.
    fn held(&self) -> Result<Vec<(EventId, u64)>, String>;
    /// Judge `wraps` and hold each with what came of it. One already held, or not of this plane,
    /// is passed over.
    fn absorb(&self, wraps: &[nostr_sdk::prelude::Event]) -> Result<Absorbed, String>;
    /// Forget the refusals due to be judged again, the longest standing few once a day (`due`).
    fn rejudge(&self, due: bool);
    /// Whether it takes no more.
    fn at_cap(&self) -> bool {
        false
    }
    /// The plane's own keys, for a relay that serves its wraps only to a connection authed as
    /// the plane.
    fn plane_keys(&self) -> Option<&nostr_sdk::prelude::Keys> {
        None
    }
    /// Drop all of it: the community was left while a pass ran.
    fn purge(&self);
}

/// A relay read by pages holds more wraps in one second than a page carries, so what else it
/// holds in that second cannot be asked for. The newest such second.
type Wall = u64;

impl PlaneSync {
    pub fn answered(&self) -> usize {
        self.relays.values().filter(|r| **r != Relayed::Unreached).count()
    }

    /// A relay answered, and none that did left something unread.
    #[cfg(test)]
    pub fn whole(&self) -> bool {
        self.answered() > 0 && !self.unread && !self.flooded && self.relays.values().all(|r| *r != Relayed::Partial)
    }
}

/// Why a pass ended with nothing.
#[derive(Debug)]
pub(super) enum Unsynced {
    /// No relay answered.
    NoRelay(String),
    /// The wraps held could not be read or written.
    Store(String),
}

/// How long a pass waits for relays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Wait {
    /// For a majority, or a short grace after the first answer.
    Quorum,
    /// For every relay to answer or fail.
    All,
}

#[derive(Serialize, Deserialize, Default)]
struct State {
    view: String,
    #[serde(default)]
    relays: BTreeMap<String, RelayRead>,
    /// When refusals were last put up to be judged again.
    #[serde(default)]
    rejudged_at: u64,
    /// A flooded view: when it may be reconciled again, and how many times running it was put off.
    #[serde(default)]
    flood: Option<(u64, u8)>,
    /// Reconciliations in a row that broke off, per relay.
    #[serde(default)]
    broke: BTreeMap<String, u8>,
    /// Relays whose last answer compared their whole set with ours.
    #[serde(default)]
    compared: BTreeSet<String>,
    #[serde(default)]
    walls: BTreeMap<String, Wall>,
    /// Ids a relay names and serves neither by id nor in its pages, per relay.
    #[serde(default)]
    ghosts: BTreeMap<String, BTreeSet<String>>,
    /// Per relay: when it may be healed again, how many heals running it kept none of, and how
    /// many wraps it lacked at the last.
    #[serde(default)]
    heal: BTreeMap<String, (u64, u8, usize)>,
    /// How many wraps were held when this was saved.
    #[serde(default)]
    held: usize,
}

/// A relay naming more ids than this that it never serves is left unread instead.
const GHOSTS_MAX: usize = 512;

impl State {
    /// `relay` was read by pages and met `wall`. A read of its whole plane that met none clears it.
    fn paged(&mut self, relay: &str, wall: Option<Wall>, whole: bool) {
        match (wall, self.walls.get(relay).copied()) {
            (Some(wall), was) => drop(self.walls.insert(relay.to_string(), was.map_or(wall, |w| w.max(wall)))),
            (None, _) if whole => drop(self.walls.remove(relay)),
            (None, _) => {}
        }
    }

    /// The second at and below which the plane may hold what no read can reach: every relay
    /// `heard` has a wall, and none compared sets. One relay past a second covers it for all, as
    /// a union of their pages would, and only while it is there to be read.
    fn wall<'a>(&self, heard: impl Iterator<Item = &'a String> + Clone) -> Option<Wall> {
        if heard.clone().any(|r| self.compared.contains(r)) {
            return None;
        }
        heard.map(|r| self.walls.get(r).copied()).collect::<Option<Vec<Wall>>>()?.into_iter().min()
    }

    fn ghost(&self, relay: &str, id: &EventId) -> bool {
        self.ghosts.get(relay).is_some_and(|g| g.contains(&id.to_hex()))
    }
}

/// A relay whose reconciliation broke off this many times running is read by pages.
pub(super) const BROKE_MAX: u8 = 3;

/// What is held of `view`'s relays. Another view's is nothing to this one.
fn load<H: Held + ?Sized>(cid_hex: &str, view: &H) -> State {
    let hex = crate::simd::hex::bytes_to_hex_32(&view.id());
    let mut state = crate::db::settings::get_sql_setting(view.state_key(cid_hex))
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str::<State>(&v).ok())
        .filter(|s| s.view == hex)
        .unwrap_or(State { view: hex, ..Default::default() });
    // Fewer wraps held than when the cursors were earned: they vouch for wraps no longer here.
    if view.held().is_ok_and(|held| held.len() < state.held) {
        crate::log_debug!("[v2:sync {}] {}: fewer wraps held than at the last pass, so its relays are read again", &cid_hex[..8], view.name());
        state.relays.clear();
    }
    state
}

/// Keep `state`, unless the community was left while the pass ran: then nothing of it stays.
fn save<H: Held + ?Sized>(community: &CommunityV2, view: &H, relays: &[String], mut state: State) {
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    if !crate::db::community::community_held(community.id()).unwrap_or(true) {
        view.purge();
        return;
    }
    state.held = view.held().map_or(0, |held| held.len());
    state.relays.retain(|r, _| relays.contains(r));
    state.broke.retain(|r, _| relays.contains(r));
    state.compared.retain(|r| relays.contains(r));
    state.walls.retain(|r, _| relays.contains(r));
    state.ghosts.retain(|r, _| relays.contains(r));
    state.heal.retain(|r, _| relays.contains(r));
    if let Ok(value) = serde_json::to_string(&state) {
        let _ = crate::db::settings::set_sql_setting(view.state_key(&cid_hex), value);
    }
}

struct Passes;
type PassLocks = std::sync::Mutex<HashMap<[u8; 32], Arc<tokio::sync::Mutex<()>>>>;

/// One pass per view at a time: a second waits, and finds little left to ask for.
fn pass_lock<H: Held + ?Sized>(view: &H) -> Arc<tokio::sync::Mutex<()>> {
    let locks = crate::db::current_session().scoped::<Passes, PassLocks>();
    let mut locks = locks.lock().unwrap_or_else(|e| e.into_inner());
    locks.entry(view.id()).or_default().clone()
}

struct Waits;
type WaitMap = std::sync::Mutex<HashMap<([u8; 32], String), u64>>;

fn waits() -> Arc<WaitMap> {
    crate::db::current_session().scoped::<Waits, WaitMap>()
}

/// Where the relays `sync` heard leave the plane unreachable by any read, if they do, as the
/// state stands now. Without a pass, every relay ever read counts.
pub(super) fn wall<H: Held + ?Sized>(community: &CommunityV2, view: &H, sync: Option<&PlaneSync>) -> Option<u64> {
    let state = load(&crate::simd::hex::bytes_to_hex_32(&community.id().0), view);
    match sync {
        Some(sync) => state.wall(sync.relays.iter().filter(|(_, ended)| **ended != Relayed::Unreached).map(|(relay, _)| relay)),
        None => state.wall(state.relays.keys()),
    }
}

/// Whether `view` is left to the pager for now.
pub(super) fn flooded<H: Held + ?Sized>(community: &CommunityV2, view: &H) -> bool {
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    load(&cid_hex, view).flood.is_some_and(|(until, _)| crate::clock::now_secs() < until) || view.at_cap()
}

/// What no relay heard in `sync` holds of the wraps held of `view`, for a read that must be exactly
/// what the relays hold now (an admin's whole read, a rekey or access plane). A relay that
/// compared its whole set says what it lacks; any other that answered is read to the end by
/// pages, and holds what it served. `None` when a relay's set cannot be told: a read cut short,
/// or a second fuller than a page.
pub(super) async fn unserved_by_all<T: Transport + ?Sized, H: Held + ?Sized>(transport: &T, view: &H, sync: &PlaneSync) -> Option<HashSet<EventId>> {
    let mut unserved: Option<HashSet<EventId>> = None;
    for lack in sync.lacks.values() {
        let lack: HashSet<EventId> = lack.iter().copied().collect();
        unserved = Some(match unserved {
            Some(all) => all.intersection(&lack).copied().collect(),
            None => lack,
        });
    }
    let held: HashSet<EventId> = view.held().ok()?.into_iter().map(|(id, _)| id).collect();
    let mut unserved = unserved.unwrap_or_else(|| held.clone());
    let unpaged: Vec<&String> = sync.relays.iter().filter(|(relay, ended)| **ended != Relayed::Unreached && !sync.lacks.contains_key(*relay)).map(|(relay, _)| relay).collect();
    if unpaged.is_empty() {
        return Some(unserved);
    }
    let budget = AtomicUsize::new(budget_of_a_pass());
    let plan = Plan { since: 0, until: None, began: crate::clock::now_secs() };
    let reads = futures_util::future::join_all(unpaged.iter().map(|relay| page_relay(transport, view, relay, &plan, &budget))).await;
    for read in reads {
        let paged = read.ok()?;
        if paged.stopped.is_some() || paged.wall.is_some() {
            return None;
        }
        unserved.retain(|id| !paged.served.contains(id));
    }
    // What the reads brought in was served by the relay that brought it.
    let now: HashSet<EventId> = view.held().ok()?.into_iter().map(|(id, _)| id).collect();
    unserved.retain(|id| held.contains(id) && now.contains(id));
    Some(unserved)
}

/// The longest a heal waits on a relay that keeps none of what it is given, step by step.
const HEAL_BACKOFF_SECS: [u64; 5] = [3_600, 6 * 3_600, 24 * 3_600, 72 * 3_600, 7 * 24 * 3_600];

/// Wraps re-published to one relay in one heal, at most.
const HEAL_MAX: usize = 200;

/// The wraps to give each relay that lacks what another relay that compared sets still holds,
/// as `sync` found it: what no relay vouches for stays with this device. A relay that kept none
/// of its last heal waits longer each time.
pub(super) async fn plan_heal<H: Held + ?Sized>(community: &CommunityV2, view: &H, sync: &PlaneSync) -> Vec<(String, Vec<EventId>)> {
    let lacks: BTreeMap<&String, HashSet<&EventId>> = sync.lacks.iter().map(|(relay, lack)| (relay, lack.iter().collect())).collect();
    let lock = pass_lock(view);
    let _pass = lock.lock().await;
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let mut state = load(&cid_hex, view);
    let now = crate::clock::now_secs();
    let mut plan = Vec::new();
    for (relay, lack) in &sync.lacks {
        let wanted: Vec<EventId> = lack.iter().filter(|id| lacks.iter().any(|(other, theirs)| *other != relay && !theirs.contains(id))).copied().collect();
        if wanted.is_empty() {
            state.heal.remove(relay);
            continue;
        }
        let (next_at, step, lacked) = state.heal.get(relay).copied().unwrap_or_default();
        if now < next_at {
            continue;
        }
        let step = if lacked > 0 && wanted.len() >= lacked { (step as usize + 1).min(HEAL_BACKOFF_SECS.len() - 1) as u8 } else { 0 };
        state.heal.insert(relay.clone(), (now + HEAL_BACKOFF_SECS[step as usize], step, wanted.len()));
        plan.push((relay.clone(), wanted.into_iter().take(HEAL_MAX).collect()));
    }
    let relays = relays_of(community);
    save(community, view, &relays, state);
    plan
}

/// How reading `view` stands with each relay, for a diagnostic.
pub(super) fn report<H: Held + ?Sized>(community: &CommunityV2, view: &H) -> serde_json::Value {
    let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let state = load(&cid_hex, view);
    let relays: BTreeMap<&String, serde_json::Value> = state
        .relays
        .iter()
        .map(|(relay, read)| (relay, serde_json::json!({ "read_to_the_end_at": read.whole_at, "paged_from": read.cursor, "long_read_pending": read.pass.is_some(), "reconciliations_broken_off": state.broke.get(relay).copied().unwrap_or(0), "compares_sets": state.compared.contains(relay), "wall": state.walls.get(relay), "named_and_never_served": state.ghosts.get(relay).map_or(0, |g| g.len()), "heal": state.heal.get(relay) })))
        .collect();
    serde_json::json!({
        "wraps": view.held().map_or(0, |held| held.len()),
        "flooded_until": state.flood.map(|(until, _)| until),
        "unreachable_from": state.wall(state.relays.keys()),
        "relays": relays,
    })
}

/// Whether a relay's read of `view` stopped short and has more to read.
pub(super) fn pending<H: Held + ?Sized>(community: &CommunityV2, view: &H) -> bool {
    load(&crate::simd::hex::bytes_to_hex_32(&community.id().0), view).relays.values().any(|read| read.pass.is_some())
}

/// Each relay's read of `view`, as kept.
#[cfg(test)]
pub(super) fn relay_reads<H: Held + ?Sized>(community: &CommunityV2, view: &H) -> BTreeMap<String, RelayRead> {
    load(&crate::simd::hex::bytes_to_hex_32(&community.id().0), view).relays
}

fn relays_of(community: &CommunityV2) -> Vec<String> {
    let mut relays: Vec<String> = Vec::new();
    for r in &community.relays {
        if !relays.contains(r) {
            relays.push(r.clone());
        }
    }
    relays
}

/// What a relay said when asked.
enum Asked {
    Named(Diff),
    Paged(Plan, Paged),
    /// Its reconciliation broke off: asked again next pass.
    Broke,
    Unreached,
}

impl Asked {
    fn answered(&self) -> bool {
        !matches!(self, Asked::Unreached | Asked::Broke)
    }
}

struct Paged {
    /// Where it stopped, when the pages or the budget ran out before the plane did.
    stopped: Option<u64>,
    opened: usize,
    /// It served a wrap that is not the event its id names.
    unfit: bool,
    wall: Option<Wall>,
    /// Every id it served.
    served: HashSet<EventId>,
}

/// `query` asked of `relay` alone, as `view` is read: authed as the plane where it must be.
async fn ask_one<T: Transport + ?Sized, H: Held + ?Sized>(transport: &T, view: &H, query: &Query, relay: &str) -> Result<Vec<nostr_sdk::prelude::Event>, String> {
    let relays = [relay.to_string()];
    match view.plane_keys() {
        Some(keys) => match transport.fetch_plane(keys, query, &relays).await? {
            fetched if fetched.answered == 0 => Err("relay did not answer".into()),
            fetched => Ok(fetched.events),
        },
        None => transport.fetch(query, &relays).await,
    }
}

/// `relay` read by pages along `plan`, each page held as it arrives. `Err` when it never answered.
async fn page_relay<T: Transport + ?Sized, H: Held + ?Sized>(transport: &T, view: &H, relay: &str, plan: &Plan, budget: &AtomicUsize) -> Result<Paged, ()> {
    let mut out = Paged { stopped: None, opened: 0, unfit: false, wall: None, served: HashSet::new() };
    let mut until = plan.until;
    let mut oldest: Option<u64> = None;
    let mut answered = false;
    for _ in 0..FOLLOW_MAX_PAGES {
        if budget.load(Ordering::Relaxed) == 0 {
            out.stopped = Some(until.unwrap_or(FROM_THE_TOP));
            return Ok(out);
        }
        let query = Query { since: (plan.since > 0).then_some(plan.since), evidence: Evidence::Full, ..plane_page(view.pk().to_hex(), FOLLOW_PAGE, until) };
        let wraps = match ask_one(transport, view, &query, relay).await {
            Ok(wraps) => wraps,
            Err(_) if !answered => return Err(()),
            Err(_) => {
                out.stopped = Some(until.unwrap_or(FROM_THE_TOP));
                return Ok(out);
            }
        };
        answered = true;
        let fresh: Vec<nostr_sdk::prelude::Event> = wraps.iter().filter(|w| out.served.insert(w.id)).cloned().collect();
        if fresh.is_empty() {
            // A full page all seen before is one second holding more than a page: step past it.
            match until {
                Some(u) if wraps.len() >= FOLLOW_PAGE && u > plan.since => {
                    out.wall = out.wall.max(Some(u));
                    until = Some(u - 1);
                    continue;
                }
                _ => return Ok(out),
            }
        }
        // Not held, so not read: the walk keeps its place rather than its cursor passing them.
        let Ok(absorbed) = view.absorb(&fresh) else {
            out.stopped = Some(until.unwrap_or(FROM_THE_TOP));
            return Ok(out);
        };
        let new = absorbed.opened + absorbed.refused;
        let _ = budget.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |b| Some(b.saturating_sub(new)));
        out.opened += absorbed.opened;
        out.unfit |= !absorbed.unfit.is_empty();
        oldest = oldest.into_iter().chain(fresh.iter().map(|w| w.created_at.as_secs())).min();
        if oldest.is_some_and(|o| o < plan.since) {
            return Ok(out);
        }
        until = oldest;
    }
    out.stopped = Some(until.unwrap_or(FROM_THE_TOP));
    Ok(out)
}

/// The wraps `ids` asked of `relay` by id. Returns the ones it served as the events they name.
async fn fetch_named<T: Transport + ?Sized, H: Held + ?Sized>(transport: &T, view: &H, relay: &str, ids: &[EventId]) -> (HashSet<EventId>, usize) {
    let mut served: HashSet<EventId> = HashSet::new();
    let mut opened = 0usize;
    for chunk in ids.chunks(FETCH_BATCH) {
        let query = Query {
            kinds: vec![stream::KIND_WRAP],
            authors: vec![view.pk().to_hex()],
            ids: chunk.iter().map(|id| id.to_hex()).collect(),
            evidence: Evidence::Full,
            ..Default::default()
        };
        let Ok(wraps) = ask_one(transport, view, &query, relay).await else { continue };
        let asked: Vec<nostr_sdk::prelude::Event> = wraps.into_iter().filter(|w| chunk.contains(&w.id)).collect();
        let Ok(absorbed) = view.absorb(&asked) else { continue };
        opened += absorbed.opened;
        let ours = |w: &&nostr_sdk::prelude::Event| w.kind.as_u16() == stream::KIND_WRAP && w.pubkey == view.pk();
        served.extend(asked.iter().filter(ours).map(|w| w.id).filter(|id| !absorbed.unfit.contains(id)));
    }
    (served, opened)
}

/// Where `relay`'s index stops short, if its answer shows that: it holds none of what is held
/// here from before some second, and something from it. That second.
pub(super) fn index_stops_at(held: &[(EventId, u64)], lack: &[EventId]) -> Option<u64> {
    if lack.is_empty() {
        return None;
    }
    let lack: HashSet<&EventId> = lack.iter().collect();
    match held.iter().filter(|(id, _)| !lack.contains(id)).map(|(_, at)| *at).min() {
        Some(first) => held.iter().any(|(_, at)| *at < first).then_some(first),
        // Lacking everything held: nothing shows its index reaching the plane at all.
        None => Some(FROM_THE_TOP),
    }
}

/// One pass of `view` against the community's relays. `Err` when none answered.
pub(super) async fn sync_plane<T: Transport + ?Sized, H: Held + ?Sized>(transport: &T, community: &CommunityV2, view: &H, wait: Wait) -> Result<PlaneSync, Unsynced> {
    crate::db::scoped(async move {
        let lock = pass_lock(view);
        let _pass = lock.lock().await;
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let relays = relays_of(community);
        if relays.is_empty() {
            return Err(Unsynced::NoRelay("community has no relays configured".to_string()));
        }
        let now = crate::clock::now_secs();
        let mut state = load(&cid_hex, view);
        let rejudge = now.saturating_sub(state.rejudged_at) >= REREAD_SECS;
        view.rejudge(rejudge);
        if rejudge {
            state.rejudged_at = now;
        }
        let held = view.held().map_err(Unsynced::Store)?;
        let pass_budget = budget_of_a_pass();
        let budget = AtomicUsize::new(pass_budget);
        let query = Query { kinds: vec![stream::KIND_WRAP], authors: vec![view.pk().to_hex()], ..Default::default() };
        let reconciler = transport.reconciler();
        let prior = state.relays.clone();
        let read_of = |relay: &str| prior.get(relay).copied().unwrap_or_default();
        let broke = state.broke.clone();

        // A. Ask every relay.
        let ask = |relay: &String| {
            let (held, query, budget) = (&held, &query, &budget);
            let read = read_of(relay);
            let give_up = broke.get(relay).copied().unwrap_or(0) + 1 >= BROKE_MAX;
            let relay = relay.clone();
            async move {
                let relay = &relay;
                let asked = match reconciler {
                    Some(reconciler) => match match view.plane_keys() {
                        Some(keys) => reconciler.reconcile_plane(keys, query, held, relay, pass_budget).await,
                        None => reconciler.reconcile(query, held, relay, pass_budget).await,
                    } {
                        Ok(diff) => Some(Asked::Named(diff)),
                        Err(ReconcileFail::Unreached) => Some(Asked::Unreached),
                        Err(ReconcileFail::Failed(_)) if !give_up => Some(Asked::Broke),
                        // No reconciling here, now or ever: its pages say what it holds.
                        Err(ReconcileFail::Unsupported | ReconcileFail::Gated | ReconcileFail::Failed(_)) => None,
                    },
                    None => None,
                };
                let asked = match asked {
                    Some(asked) => asked,
                    None => {
                        let plan = read.plan(now);
                        match page_relay(transport, view, relay, &plan, budget).await {
                            Ok(paged) => Asked::Paged(plan, paged),
                            Err(()) => Asked::Unreached,
                        }
                    }
                };
                (relay.clone(), asked)
            }
        };
        // Waited out whatever the grace: a relay not heard this session or for hours, and one
        // whose read by pages is a long one that a cut would only start over.
        let waited = waits();
        let must: HashSet<String> = {
            let waited = waited.lock().unwrap_or_else(|e| e.into_inner());
            relays
                .iter()
                .filter(|r| {
                    let read = read_of(r);
                    let plan = read.plan(now);
                    let long = plan.until.is_some() || (read.cursor.is_some() && plan.since == 0);
                    long || waited.get(&(view.id(), (*r).clone())).is_none_or(|at| now.saturating_sub(*at) >= HEARD_SECS)
                })
                .cloned()
                .collect()
        };
        let mut asking: FuturesUnordered<_> = relays.iter().map(ask).collect();
        let mut answers: Vec<(String, Asked)> = Vec::new();
        let mut deadline: Option<crate::rt::time::Instant> = None;
        while !asking.is_empty() {
            let owed = must.iter().any(|r| !answers.iter().any(|(a, _)| a == r));
            let next = match deadline.filter(|_| wait == Wait::Quorum && !owed) {
                Some(deadline) => match crate::rt::time::timeout_at(deadline, asking.next()).await {
                    Ok(next) => next,
                    Err(_) => break,
                },
                None => asking.next().await,
            };
            let Some((relay, asked)) = next else { break };
            if asked.answered() {
                let answered = 1 + answers.iter().filter(|(_, a)| a.answered()).count();
                let grace = if answered * 2 > relays.len() { RESIDUAL_GRACE_MS } else { QUORUM_GRACE_MS };
                let at = crate::rt::time::Instant::now() + std::time::Duration::from_millis(grace);
                deadline = Some(deadline.map_or(at, |d| d.min(at)));
            }
            answers.push((relay, asked));
        }
        drop(asking);
        {
            let mut waited = waited.lock().unwrap_or_else(|e| e.into_inner());
            for (relay, _) in &answers {
                waited.insert((view.id(), relay.clone()), now);
            }
        }
        for (relay, asked) in &answers {
            match asked {
                Asked::Broke => *state.broke.entry(relay.clone()).or_default() += 1,
                Asked::Named(_) | Asked::Paged(..) => drop(state.broke.remove(relay)),
                Asked::Unreached => {}
            }
        }
        if !answers.iter().any(|(_, a)| a.answered()) {
            // Reconciliations that broke off are all there was: those relays' pages are still
            // to be had, and a pass with nothing else to go on reads them.
            let broke: Vec<(String, Plan)> = answers.iter().filter(|(_, a)| matches!(a, Asked::Broke)).map(|(relay, _)| (relay.clone(), read_of(relay).plan(now))).collect();
            let paged = futures_util::future::join_all(broke.iter().map(|(relay, plan)| {
                let budget = &budget;
                async move { page_relay(transport, view, relay, plan, budget).await }
            }))
            .await;
            for ((relay, plan), paged) in broke.into_iter().zip(paged) {
                if let (Ok(paged), Some(answer)) = (paged, answers.iter_mut().find(|(r, _)| *r == relay)) {
                    answer.1 = Asked::Paged(plan, paged);
                }
            }
        }
        if !answers.iter().any(|(_, a)| a.answered()) {
            save(community, view, &relays, state);
            return Err(Unsynced::NoRelay(format!("no relay answered the {} read", view.name())));
        }
        // A relay due its daily read has what it named and never served asked of it again.
        for (relay, asked) in &answers {
            if let Asked::Named(diff) = asked {
                if now.saturating_sub(read_of(relay).whole_at) >= REREAD_SECS {
                    state.ghosts.remove(relay);
                } else if let Some(ghosts) = state.ghosts.get_mut(relay) {
                    ghosts.retain(|id| diff.need.iter().any(|n| n.to_hex() == *id));
                }
            }
        }

        let mut sync = PlaneSync::default();
        for relay in &relays {
            sync.relays.insert(relay.clone(), Relayed::Unreached);
        }

        // B. Every wrap named, fetched once from a relay that named it, the next taking over a miss.
        let have: HashSet<EventId> = view.held().map_err(Unsynced::Store)?.into_iter().map(|(id, _)| id).collect();
        let mut namers: BTreeMap<EventId, VecDeque<String>> = BTreeMap::new();
        for (relay, asked) in &answers {
            if let Asked::Named(diff) = asked {
                for id in diff.need.iter().filter(|id| !have.contains(id) && !state.ghost(relay, id)) {
                    namers.entry(*id).or_default().push_back(relay.clone());
                }
            }
        }
        let mut over_budget = answers.iter().any(|(_, a)| matches!(a, Asked::Named(diff) if !diff.complete));
        let room = budget.load(Ordering::Relaxed);
        if namers.len() > room {
            over_budget = true;
            let keep: Vec<EventId> = namers.keys().take(room).copied().collect();
            namers.retain(|id, _| keep.contains(id));
        }
        let mut failed_to_serve: HashSet<String> = HashSet::new();
        while !namers.is_empty() {
            let mut shares: BTreeMap<String, Vec<EventId>> = BTreeMap::new();
            namers.retain(|id, queue| match queue.pop_front() {
                Some(relay) => {
                    shares.entry(relay).or_default().push(*id);
                    true
                }
                None => false,
            });
            if shares.is_empty() {
                break;
            }
            let fetched = futures_util::future::join_all(shares.iter().map(|(relay, ids)| async move { (relay, ids, fetch_named(transport, view, relay, ids).await) })).await;
            for (relay, ids, (served, opened)) in fetched {
                sync.opened += opened;
                for id in ids {
                    if served.contains(id) {
                        namers.remove(id);
                    } else {
                        failed_to_serve.insert(relay.clone());
                    }
                }
            }
        }

        // A relay that named what nobody served, or whose answer shows its index stopping short
        // of the plane, is read by pages: to the end once a day, and between those from where
        // that read began, which is where a wrap it has only just named would sit.
        let have: HashSet<EventId> = view.held().map_err(Unsynced::Store)?.into_iter().map(|(id, _)| id).collect();
        let unserved_by = |state: &State, relay: &String, diff: &Diff, have: &HashSet<EventId>| -> Vec<EventId> { diff.need.iter().filter(|id| !have.contains(id) && !state.ghost(relay, id)).copied().collect() };
        let mut owed_reads: Vec<(String, Plan, bool)> = Vec::new();
        for (relay, asked) in &answers {
            let Asked::Named(diff) = asked else { continue };
            let read = read_of(relay);
            let unserved = failed_to_serve.contains(relay) && !unserved_by(&state, relay, diff, &have).is_empty();
            let whole = match read.pass {
                Some(_) => Some(read.plan(now)),
                None if now.saturating_sub(read.whole_at) >= REREAD_SECS => {
                    let stops_at = index_stops_at(&held, &diff.lack);
                    (unserved || stops_at.is_some()).then(|| Plan { since: 0, until: stops_at.filter(|at| !unserved && *at < FROM_THE_TOP), began: now })
                }
                None => None,
            };
            if whole.is_some() || unserved {
                crate::log_debug!(
                    "[v2:sync {}] {}: {relay} read by pages (named and not served: {unserved}, index stops at: {:?}, a read going on: {})",
                    &cid_hex[..8], view.name(), index_stops_at(&held, &diff.lack), read.pass.is_some()
                );
            }
            match whole {
                Some(plan) => owed_reads.push((relay.clone(), plan, true)),
                None if unserved => owed_reads.push((relay.clone(), Plan { since: read.whole_at.saturating_sub(OVERLAP_SECS), until: None, began: now }, false)),
                None => {}
            }
        }
        let reads = futures_util::future::join_all(owed_reads.iter().map(|(relay, plan, _)| {
            let budget = &budget;
            async move { page_relay(transport, view, relay, plan, budget).await }
        }))
        .await;
        let have: HashSet<EventId> = view.held().map_err(Unsynced::Store)?.into_iter().map(|(id, _)| id).collect();
        let mut owed: HashMap<String, RelayRead> = HashMap::new();
        let mut cut_short: HashSet<String> = HashSet::new();
        for ((relay, plan, whole), paged) in owed_reads.iter().zip(reads) {
            let Ok(paged) = paged else { continue };
            sync.opened += paged.opened;
            if *whole {
                owed.insert(relay.clone(), read_of(relay).after(plan, paged.stopped));
                state.paged(relay, paged.wall, paged.stopped.is_none() && plan.until.is_none());
            }
            if paged.stopped.is_some() {
                cut_short.insert(relay.clone());
                continue;
            }
            // Its pages ran to their end without them: what it names it does not hold to give.
            let Some((_, Asked::Named(diff))) = answers.iter().find(|(r, _)| r == relay) else { continue };
            let gone = unserved_by(&state, relay, diff, &have);
            let ghosts = state.ghosts.entry(relay.clone()).or_default();
            if ghosts.len() + gone.len() <= GHOSTS_MAX {
                ghosts.extend(gone.iter().map(|id| id.to_hex()));
            }
        }

        // C. How each relay's pass ended.
        let have: HashSet<EventId> = view.held().map_err(Unsynced::Store)?.into_iter().map(|(id, _)| id).collect();
        for (relay, asked) in &answers {
            let mut read = read_of(relay);
            let ended = match asked {
                Asked::Unreached | Asked::Broke => Relayed::Unreached,
                Asked::Named(diff) => {
                    sync.lacking.insert(relay.clone(), diff.lack.len());
                    if let Some(after) = owed.get(relay) {
                        read = *after;
                    }
                    let unread = !diff.complete || cut_short.contains(relay) || diff.need.iter().any(|id| !have.contains(id) && !state.ghost(relay, id));
                    sync.unread |= unread;
                    if !unread && index_stops_at(&held, &diff.lack).is_none() {
                        state.compared.insert(relay.clone());
                        state.walls.remove(relay);
                        sync.lacks.insert(relay.clone(), diff.lack.clone());
                    } else {
                        state.compared.remove(relay);
                    }
                    if unread { Relayed::Partial } else { Relayed::Whole }
                }
                Asked::Paged(plan, paged) => {
                    sync.opened += paged.opened;
                    read = read.after(plan, paged.stopped);
                    sync.unread |= paged.stopped.is_some();
                    state.compared.remove(relay);
                    state.paged(relay, paged.wall, paged.stopped.is_none() && plan.since == 0 && plan.until.is_none());
                    match (paged.stopped, paged.unfit, plan.since) {
                        (Some(_), ..) | (_, true, _) => Relayed::Partial,
                        (None, false, 0) => Relayed::Whole,
                        (None, false, _) => Relayed::Since,
                    }
                }
            };
            if asked.answered() {
                state.relays.insert(relay.clone(), read);
            }
            sync.relays.insert(relay.clone(), ended);
        }
        sync.unread |= over_budget;
        let mut lacks = answers.iter().filter(|(_, asked)| asked.answered()).map(|(relay, asked)| match asked {
            Asked::Named(diff) if state.compared.contains(relay) => Some(diff.lack.iter().copied().collect::<HashSet<EventId>>()),
            _ => None,
        });
        sync.unserved = lacks.next().flatten().and_then(|first| lacks.try_fold(first, |all, lack| lack.map(|lack| all.intersection(&lack).copied().collect()))).map(|all: HashSet<EventId>| all.into_iter().collect());

        sync.flooded = over_budget || view.at_cap();
        state.flood = match (sync.flooded, state.flood) {
            (false, _) => None,
            (true, was) => {
                let step = was.map_or(0, |(_, step)| step as usize).min(FLOOD_BACKOFF_SECS.len() - 1);
                Some((now + FLOOD_BACKOFF_SECS[step], (step + 1) as u8))
            }
        };
        if sync.flooded {
            crate::log_warn!(
                "[v2:community {}] the {} holds more than a pass reads",
                &cid_hex[..8.min(cid_hex.len())],
                view.name()
            );
        }
        save(community, view, &relays, state);
        Ok(sync)
    })
    .await
}

/// Each relay read by pages to the end of the plane, unless it was read so within the day:
/// events, where [`sync_plane`] takes a relay's word. `Err` when a relay due a read was not
/// the only one and none answered.
pub(super) async fn read_whole<T: Transport + ?Sized, H: Held + ?Sized>(transport: &T, community: &CommunityV2, view: &H) -> Result<PlaneSync, Unsynced> {
    crate::db::scoped(async move {
        let lock = pass_lock(view);
        let _pass = lock.lock().await;
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&community.id().0);
        let relays = relays_of(community);
        let now = crate::clock::now_secs();
        let mut state = load(&cid_hex, view);
        let budget = AtomicUsize::new(budget_of_a_pass());
        let mut sync = PlaneSync::default();
        let due: Vec<(String, Plan)> = relays
            .iter()
            .filter_map(|relay| {
                let read = state.relays.get(relay).copied().unwrap_or_default();
                if now.saturating_sub(read.whole_at) < REREAD_SECS {
                    sync.relays.insert(relay.clone(), Relayed::Whole);
                    return None;
                }
                // A long read still going carries on; anything else starts from the top.
                let plan = match read.pass {
                    Some(pass) if pass.since == 0 => read.plan(now),
                    _ => Plan { since: 0, until: None, began: now },
                };
                Some((relay.clone(), plan))
            })
            .collect();
        let reads = futures_util::future::join_all(due.iter().map(|(relay, plan)| {
            let budget = &budget;
            async move { (relay, plan, page_relay(transport, view, relay, plan, budget).await) }
        }))
        .await;
        for (relay, plan, paged) in reads {
            let ended = match paged {
                Err(()) => Relayed::Unreached,
                Ok(paged) => {
                    sync.opened += paged.opened;
                    sync.unread |= paged.stopped.is_some();
                    let read = state.relays.get(relay).copied().unwrap_or_default();
                    state.relays.insert(relay.clone(), read.after(plan, paged.stopped));
                    state.paged(relay, paged.wall, paged.stopped.is_none() && plan.until.is_none());
                    if paged.stopped.is_some() || paged.unfit { Relayed::Partial } else { Relayed::Whole }
                }
            };
            sync.relays.insert(relay.clone(), ended);
        }
        save(community, view, &relays, state);
        if sync.answered() == 0 {
            return Err(Unsynced::NoRelay(format!("no relay answered the {} read", view.name())));
        }
        Ok(sync)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(walls: &[(&str, Option<Wall>)], compared: &[&str]) -> State {
        State {
            relays: walls.iter().map(|(r, _)| (r.to_string(), RelayRead::default())).collect(),
            walls: walls.iter().filter_map(|(r, w)| w.map(|w| (r.to_string(), w))).collect(),
            compared: compared.iter().map(|r| r.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn the_plane_is_walled_only_where_every_relay_heard_is() {
        let wall = |s: State, heard: &[&str]| {
            let heard: Vec<String> = heard.iter().map(|r| r.to_string()).collect();
            s.wall(heard.iter())
        };
        assert_eq!(wall(state(&[], &[]), &[]), None);
        assert_eq!(wall(state(&[("a", Some(900))], &[]), &["a"]), Some(900));
        assert_eq!(wall(state(&[("a", Some(900)), ("b", None)], &[]), &["a", "b"]), None, "b reads past it");
        assert_eq!(wall(state(&[("a", Some(900)), ("b", None)], &[]), &["a"]), Some(900), "b is not there to read past it");
        // Above 500 b has no wall; at 500 a may have one below its newest.
        assert_eq!(wall(state(&[("a", Some(900)), ("b", Some(500))], &[]), &["a", "b"]), Some(500));
        assert_eq!(wall(state(&[("a", Some(900)), ("b", Some(500))], &["b"]), &["a", "b"]), None, "b compared its whole set");
        assert_eq!(wall(state(&[("a", Some(900)), ("b", Some(500))], &["b"]), &["a"]), Some(900), "which counts while b answers");
    }

    #[test]
    fn a_wall_is_kept_at_its_newest_and_cleared_by_a_whole_read_that_meets_none() {
        let mut s = State::default();
        s.paged("a", Some(500), false);
        s.paged("a", Some(300), true);
        assert_eq!(s.walls.get("a"), Some(&500));
        s.paged("a", None, false);
        assert_eq!(s.walls.get("a"), Some(&500), "a read from a cursor never went back there");
        s.paged("a", None, true);
        assert_eq!(s.walls.get("a"), None);
    }
}
