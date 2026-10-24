//! DM catch-up: what our read relays hold for us that we don't, fetched once each.
//!
//! Each relay reconciles (NIP-77) only the window it hasn't vouched for: from its
//! cursor, less NIP-59's backdating slack. On an established account that is a few
//! kilobytes of fingerprints. What a relay names missing is fetched from that relay
//! alone, so a wrap five relays hold comes down once, not five times. A relay that
//! can't reconcile walks its window in small pages instead. Everything commits as it
//! lands, so a link that dies part-way keeps what came. Fetches wait for silence, not
//! a total: a slow relay that keeps sending keeps its turn. A relay's cursor advances
//! only once its whole window is held.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt};
use nostr_sdk::prelude::*;

use crate::community::transport::{fetch_relay_eose_filters, EoseFail};
use crate::event_handler::{self, BatchingPersist, PreparedEvent};
use crate::negentropy;

/// NIP-59 backdates a wrap's created_at by up to two days, so a window that reaches
/// back to a cursor has to start this much before it.
pub const NIP59_BACKDATE_SLACK: u64 = 2 * 24 * 3600;
/// A relay with no cursor catches up over this much of the recent past; deeper
/// history is the bootstrap's.
const UNCURSORED_WINDOW: u64 = 7 * 24 * 3600;
/// Ids asked for per REQ.
const FETCH_BATCH: usize = 200;
/// Events per page when a relay's window is walked by REQ.
const PAGE: usize = 100;
/// A walk stops here whatever it finds: no window is this deep.
const MAX_PAGES: usize = 200;
/// Messages held before a batched persist.
const PERSIST_BATCH: usize = 100;
/// After an incomplete catch-up, the waits before each retry: soon, then less often.
pub const RETRY_BACKOFF_SECS: [u64; 6] = [15, 45, 120, 300, 900, 1800];
/// A catch-up this often while the app runs: a live subscription can die without a
/// word, and the window since the last one is a few fingerprints.
pub const SAFETY_NET_SECS: u64 = 600;

pub struct Opts {
    /// What arrives is news (a reconnect, a return to the app) rather than history (a launch).
    pub is_new: bool,
    /// Only these relays; `None` for every read relay.
    pub only: Option<Vec<String>>,
    /// A relay with no cursor reconciles its whole history and earns one, rather than
    /// catching up its recent week (a fresh device's first sync).
    pub bootstrap: bool,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Report {
    pub relays: usize,
    pub reconciled: usize,
    pub walked: usize,
    /// Wraps found missing, each counted once however many relays held it.
    pub missing: usize,
    pub fetched: usize,
    /// Wraps a relay named but then didn't send, reaching its end without them.
    pub unservable: usize,
    pub new_messages: u32,
    pub cursors_advanced: usize,
    /// Relays whose window isn't fully held yet, with why: worth another try.
    pub incomplete: Vec<String>,
    pub elapsed_ms: u64,
}

impl Report {
    pub fn complete(&self) -> bool {
        self.incomplete.is_empty()
    }

    /// The relays to try again, without their reasons.
    pub fn incomplete_relays(&self) -> Vec<String> {
        self.incomplete.iter().map(|r| r.split(' ').next().unwrap_or(r).to_string()).collect()
    }

    /// No relay answered at all: the network may be down, not empty.
    pub fn reached_none(&self) -> bool {
        self.relays > 0 && self.reconciled == 0 && self.walked == 0 && self.fetched == 0
    }
}

struct Plan {
    url: RelayUrl,
    relay: Relay,
    /// The window's start; `None` reconciles the whole history (a bootstrap).
    since: Option<u64>,
    cursored: bool,
}

enum Outcome {
    Missing(HashSet<EventId>),
    NoNeg,
    Unreached,
    Failed(String),
}

#[derive(Default)]
struct Got {
    received: HashSet<EventId>,
    events: usize,
    new_messages: u32,
    /// The link went quiet before everything asked for came.
    stalled: bool,
    /// The relay refused the REQ, so what it didn't send says nothing.
    closed: bool,
}

enum Job {
    Reconcile(usize),
    Fetch(usize, Vec<EventId>),
    Walk(usize),
}

enum Step {
    Reconciled(usize, Outcome),
    Fetched(usize, Vec<EventId>, Got),
    Walked(usize, Got, bool),
}

#[derive(Clone)]
enum Standing {
    Pending,
    Reconciled,
    Walked(bool),
    Incomplete(String),
}

struct Ctx<'b, 'p, F> {
    client: Client,
    me: PublicKey,
    now: u64,
    plans: Vec<Plan>,
    items: Vec<(EventId, Timestamp)>,
    batcher: &'b BatchingPersist<'p>,
    commit: &'b F,
    is_new: bool,
    flush_ok: AtomicBool,
}

/// Catch up on DMs over the read relays (or `opts.only`). Each event goes through
/// `commit` (a shell's own commit, which may intercept, around the core one) with the
/// shared `batcher` holding the persists.
pub async fn catch_up<F, Fut>(opts: &Opts, batcher: &BatchingPersist<'_>, commit: F) -> Report
where
    F: Fn(PreparedEvent, bool) -> Fut,
    Fut: Future<Output = bool>,
{
    let started = crate::rt::time::Instant::now();
    let mut report = Report::default();
    let (Some(client), Some(me)) = (crate::state::nostr_client(), crate::state::my_public_key()) else {
        return report;
    };
    let now = Timestamp::now().as_secs();
    let wanted = |url: &RelayUrl| {
        opts.only.as_ref().is_none_or(|only| {
            only.iter().any(|u| u.trim_end_matches('/') == url.as_str().trim_end_matches('/'))
        })
    };
    // Read relays only: a gossip-flagged community relay must never see a REQ naming our npub.
    let plans: Vec<Plan> = client
        .relays()
        .await
        .into_iter()
        .filter(|(url, relay)| relay.capabilities().has_read() && wanted(url))
        .map(|(url, relay)| {
            let cursor = negentropy::reconcile_cursor(url.as_str());
            let since = match cursor {
                Some(c) => Some(c.saturating_sub(NIP59_BACKDATE_SLACK)),
                None if opts.bootstrap => None,
                None => Some(now.saturating_sub(UNCURSORED_WINDOW)),
            };
            Plan { url, relay, since, cursored: cursor.is_some() }
        })
        .collect();
    report.relays = plans.len();
    if plans.is_empty() {
        return report;
    }

    // Our fingerprints over the widest window any relay needs, read once.
    let items = if plans.iter().any(|p| p.since.is_none()) {
        crate::db::wrappers::load_negentropy_items()
    } else {
        let widest = plans.iter().filter_map(|p| p.since).min().unwrap_or(now);
        crate::db::wrappers::load_negentropy_items_since(widest)
    }
    .unwrap_or_default();

    let n = plans.len();
    let ctx = Ctx { client, me, now, plans, items, batcher, commit: &commit, is_new: opts.is_new, flush_ok: AtomicBool::new(true) };

    let mut jobs = FuturesUnordered::new();
    for i in 0..n {
        jobs.push(run_job(Job::Reconcile(i), &ctx));
    }
    let mut standing = vec![Standing::Pending; n];
    let mut wants: Vec<HashSet<EventId>> = vec![HashSet::new(); n];
    // A relay that went quiet mid-fetch is handed nothing more this run.
    let mut quiet = vec![false; n];
    // What each relay named, was asked for, and ended its answer without.
    let mut refused: Vec<HashSet<EventId>> = vec![HashSet::new(); n];
    // Each missing wrap's fetcher while its fetch runs, and who has already failed it.
    let mut owner: HashMap<EventId, usize> = HashMap::new();
    let mut tried: HashMap<EventId, Vec<usize>> = HashMap::new();
    let mut received: HashSet<EventId> = HashSet::new();

    while let Some(step) = jobs.next().await {
        match step {
            Step::Reconciled(i, Outcome::Missing(ids)) => {
                report.reconciled += 1;
                standing[i] = Standing::Reconciled;
                report.missing += ids.iter().filter(|id| !wants.iter().any(|w| w.contains(*id))).count();
                let mine: Vec<EventId> = ids
                    .iter()
                    .filter(|id| !received.contains(*id) && !owner.contains_key(*id) && !tried.get(*id).is_some_and(|t| t.contains(&i)))
                    .copied()
                    .collect();
                wants[i] = ids;
                for id in &mine {
                    owner.insert(*id, i);
                }
                if !mine.is_empty() {
                    jobs.push(run_job(Job::Fetch(i, mine), &ctx));
                }
            }
            Step::Reconciled(i, Outcome::NoNeg) => jobs.push(run_job(Job::Walk(i), &ctx)),
            Step::Reconciled(i, Outcome::Unreached) => standing[i] = Standing::Incomplete("unreachable".into()),
            Step::Reconciled(i, Outcome::Failed(e)) => standing[i] = Standing::Incomplete(e),
            Step::Fetched(i, asked, got) => {
                report.fetched += got.events;
                report.new_messages += got.new_messages;
                received.extend(got.received);
                quiet[i] |= got.stalled;
                let answered = !got.stalled && !got.closed;
                // What it didn't bring goes to another relay that named it.
                let mut handoff: HashMap<usize, Vec<EventId>> = HashMap::new();
                for id in asked {
                    owner.remove(&id);
                    if received.contains(&id) {
                        continue;
                    }
                    if answered {
                        refused[i].insert(id);
                    }
                    let failed = tried.entry(id).or_default();
                    failed.push(i);
                    let next = (0..n).find(|j| {
                        !quiet[*j] && !failed.contains(j) && matches!(standing[*j], Standing::Reconciled) && wants[*j].contains(&id)
                    });
                    if let Some(j) = next {
                        owner.insert(id, j);
                        handoff.entry(j).or_default().push(id);
                    }
                }
                for (j, ids) in handoff {
                    jobs.push(run_job(Job::Fetch(j, ids), &ctx));
                }
            }
            Step::Walked(i, got, complete) => {
                report.walked += 1;
                report.fetched += got.events;
                report.new_messages += got.new_messages;
                received.extend(got.received);
                standing[i] = if complete { Standing::Walked(true) } else { Standing::Incomplete("walk cut short".into()) };
            }
        }
    }
    drop(jobs);

    let flushed = ctx.batcher.try_flush().await.is_ok() && ctx.flush_ok.load(Ordering::SeqCst);
    let live = !crate::db::session_stopped();
    for (i, plan) in ctx.plans.iter().enumerate() {
        let url = plan.url.as_str();
        let held = match &standing[i] {
            // A windowed reconcile can't vouch for history before its window: only a
            // cursored relay, or a whole-history reconcile, earns an advance.
            Standing::Reconciled => {
                // A wrap the relay itself won't serve can't hold its cursor back forever.
                if !wants[i].iter().all(|id| received.contains(id) || refused[i].contains(id)) {
                    report.incomplete.push(format!("{url} (not every missing wrap arrived)"));
                    continue;
                }
                plan.cursored || plan.since.is_none()
            }
            // A walk is all a relay without NIP-77 ever gets, so its window is all there is to vouch for.
            Standing::Walked(complete) => *complete,
            Standing::Pending => false,
            Standing::Incomplete(why) => {
                report.incomplete.push(format!("{url} ({why})"));
                continue;
            }
        };
        if held && flushed && live {
            negentropy::advance_reconcile_cursor(url, ctx.now);
            report.cursors_advanced += 1;
        }
    }
    if !flushed {
        report.incomplete.push("(a batch failed to persist)".into());
    }
    report.unservable = refused.iter().flatten().filter(|id| !received.contains(*id)).collect::<HashSet<_>>().len();
    report.elapsed_ms = started.elapsed().as_millis() as u64;

    crate::log_net_info!(
        "[DM catch-up] {} relay(s): {} reconciled, {} walked, {} missing, {} fetched ({} new, {} unservable), {} cursor(s) advanced in {}ms",
        report.relays, report.reconciled, report.walked, report.missing, report.fetched,
        report.new_messages, report.unservable, report.cursors_advanced, report.elapsed_ms
    );
    if !report.incomplete.is_empty() {
        crate::log_net_fail!("[DM catch-up] incomplete: {}", report.incomplete.join(", "));
    }
    report
}

/// Retry what a catch-up left incomplete, backing off, until every relay is held or
/// the account is swapped out. `run` catches up the relays it's given.
pub async fn retry_incomplete<R, Fut>(report: &Report, run: R)
where
    R: Fn(Vec<String>) -> Fut,
    Fut: Future<Output = Report>,
{
    let mut relays = report.incomplete_relays();
    relays.retain(|u| u.contains("://"));
    // A failed persist names no relay, but leaves every relay's window to fetch again.
    let all = report.incomplete.iter().any(|r| !r.contains("://"));
    for wait in RETRY_BACKOFF_SECS {
        if (relays.is_empty() && !all) || crate::db::session_stopped() {
            return;
        }
        crate::rt::time::sleep(Duration::from_secs(wait)).await;
        if crate::db::session_stopped() {
            return;
        }
        let next = run(if all { Vec::new() } else { relays.clone() }).await;
        if next.complete() {
            return;
        }
        relays = next.incomplete_relays();
        relays.retain(|u| u.contains("://"));
    }
}

struct SafetyNet;

/// Catch up every [`SAFETY_NET_SECS`] for as long as this account is the one
/// logged in. Once per account however often it's called.
pub fn start_safety_net<R, Fut>(run: R)
where
    R: Fn() -> Fut + crate::rt::MaybeSend + 'static,
    Fut: Future<Output = Report> + crate::rt::MaybeSend + 'static,
{
    let started = crate::db::current_session().scoped::<SafetyNet, AtomicBool>();
    if started.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::db::spawn_bound(async move {
        loop {
            crate::rt::time::sleep(Duration::from_secs(SAFETY_NET_SECS)).await;
            if crate::db::session_stopped() {
                return;
            }
            run().await;
        }
    });
}

async fn run_job<F, Fut>(job: Job, ctx: &Ctx<'_, '_, F>) -> Step
where
    F: Fn(PreparedEvent, bool) -> Fut,
    Fut: Future<Output = bool>,
{
    match job {
        Job::Reconcile(i) => Step::Reconciled(i, reconcile(ctx, &ctx.plans[i]).await),
        Job::Fetch(i, ids) => {
            let got = fetch_ids(ctx, &ctx.plans[i].url, &ids).await;
            Step::Fetched(i, ids, got)
        }
        Job::Walk(i) => {
            let (got, complete) = walk(ctx, &ctx.plans[i]).await;
            Step::Walked(i, got, complete)
        }
    }
}

async fn reconcile<F>(ctx: &Ctx<'_, '_, F>, plan: &Plan) -> Outcome {
    let url = plan.url.as_str();
    if !negentropy::wait_connected(&plan.relay, crate::relay_connect_timeout(Duration::from_secs(15))).await {
        return Outcome::Unreached;
    }
    if negentropy::neg_supported_cached(url) == Some(false) {
        return Outcome::NoNeg;
    }
    let mut filter = Filter::new().pubkey(ctx.me).kind(Kind::GiftWrap);
    if let Some(since) = plan.since {
        filter = filter.since(Timestamp::from_secs(since));
    }
    // A bootstrap's first frame is the relay walking its whole set, which a slow box
    // can take minutes over; a window's is small, so its wait is about the link.
    let (first, whole) = match plan.since {
        Some(_) => (crate::relay_request_timeout(Duration::from_secs(20)), crate::relay_request_timeout(Duration::from_secs(90))),
        None => (Duration::from_secs(300), Duration::from_secs(480)),
    };
    let sync_opts = SyncOptions::new().direction(SyncDirection::Down).initial_timeout(first).dry_run();
    let in_window = |at: &Timestamp| plan.since.is_none_or(|s| at.as_secs() >= s);
    let attempt = || {
        crate::rt::time::timeout(
            whole,
            plan.relay
                .sync(filter.clone())
                .items(ctx.items.iter().filter(|(_, at)| in_window(at)).copied())
                .opts(sync_opts.clone()),
        )
    };
    let mut result = attempt().await;
    // An AUTH-gating relay refuses the first gated request while its challenge is
    // answered; a lagged broadcast says nothing about the relay at all.
    if matches!(&result, Ok(Err(e)) if { let e = e.to_string(); e.contains("auth-required") || e.contains("lagged") }) {
        crate::rt::time::sleep(Duration::from_millis(1200)).await;
        result = attempt().await;
    }
    match result {
        Ok(Ok(recon)) => {
            negentropy::record_neg_support(url, true);
            Outcome::Missing(recon.remote)
        }
        Ok(Err(e)) => {
            let e = e.to_string();
            if negentropy::classify_neg_sync_error(&e, false) == Some(false) {
                crate::log_net_info!("[DM catch-up] {url} can't reconcile ({e}): walking its window");
                negentropy::record_neg_support(url, false);
                return Outcome::NoNeg;
            }
            // Silence is never recorded: a wrong verdict costs a walk of the whole window on
            // every run for a day. One that has reconciled before is just slow; one that never
            // has, yet answers a REQ promptly, walks this once.
            if e == "timeout" && !negentropy::neg_ever_confirmed(url) && answers_req(ctx, url).await {
                crate::log_net_info!("[DM catch-up] {url} silent on NIP-77: walking its window this time");
                return Outcome::NoNeg;
            }
            crate::log_warn!("[DM catch-up] {url} reconcile failed: {e}");
            Outcome::Failed(e)
        }
        Err(_) => {
            crate::log_warn!("[DM catch-up] {url} reconcile ran past {whole:?}");
            Outcome::Failed("timed out".into())
        }
    }
}

/// Whether a relay answers a REQ promptly: on a weak link a healthy relay is slow at
/// both, so silence on a reconcile alone says nothing.
async fn answers_req<F>(ctx: &Ctx<'_, '_, F>, url: &str) -> bool {
    let probe = Filter::new().pubkey(ctx.me).kind(Kind::GiftWrap).limit(1);
    fetch_relay_eose_filters(&ctx.client, url, vec![probe], crate::relay_request_timeout(Duration::from_secs(5)))
        .await
        .is_ok()
}

/// Fetch these wraps from one relay, committing each as it comes.
async fn fetch_ids<F, Fut>(ctx: &Ctx<'_, '_, F>, url: &RelayUrl, ids: &[EventId]) -> Got
where
    F: Fn(PreparedEvent, bool) -> Fut,
    Fut: Future<Output = bool>,
{
    // The wait for the next event, not for the whole batch.
    let idle = crate::relay_request_timeout(Duration::from_secs(20));
    let mut got = Got::default();
    for batch in ids.chunks(FETCH_BATCH) {
        if crate::db::session_stopped() {
            break;
        }
        // The #p is not redundant: Ditto refuses gift-wrap REQs carrying neither authors nor #p.
        let filter = Filter::new().ids(batch.iter().copied()).kind(Kind::GiftWrap).pubkey(ctx.me);
        let Ok(stream) = ctx
            .client
            .stream_events(ReqTarget::single(url.as_str(), [filter]))
            .timeout(Duration::from_secs(600))
            .await
        else {
            got.stalled = true;
            break;
        };
        let mut stream = std::pin::pin!(stream);
        let mut pending: Vec<Event> = Vec::new();
        loop {
            match crate::rt::time::timeout(idle, stream.next()).await {
                Ok(Some((_, Ok(event)))) => {
                    if got.received.insert(event.id) {
                        pending.push(event);
                        if pending.len() >= event_handler::UNWRAP_BATCH {
                            absorb(ctx, std::mem::take(&mut pending), &mut got).await;
                        }
                    }
                }
                Ok(Some((_, Err(_)))) => got.closed = true,
                Ok(None) => break,
                Err(_) => {
                    got.stalled = true;
                    break;
                }
            }
        }
        absorb(ctx, pending, &mut got).await;
        if got.stalled {
            break;
        }
    }
    got
}

/// Walk a relay's window newest-first in small pages, for a relay that can't
/// reconcile. Complete once a page brings nothing new.
async fn walk<F, Fut>(ctx: &Ctx<'_, '_, F>, plan: &Plan) -> (Got, bool)
where
    F: Fn(PreparedEvent, bool) -> Fut,
    Fut: Future<Output = bool>,
{
    // Never a whole history by REQ.
    let since = plan.since.unwrap_or(ctx.now.saturating_sub(UNCURSORED_WINDOW));
    let budget = crate::relay_request_timeout(Duration::from_secs(30));
    let mut got = Got::default();
    let mut until: Option<u64> = None;
    for page in 0..MAX_PAGES {
        if crate::db::session_stopped() {
            return (got, false);
        }
        let mut filter = Filter::new().pubkey(ctx.me).kind(Kind::GiftWrap).since(Timestamp::from_secs(since)).limit(PAGE);
        if let Some(u) = until {
            // Inclusive: wraps sharing the boundary second are deduped, not skipped.
            filter = filter.until(Timestamp::from_secs(u));
        }
        let events = match fetch_relay_eose_filters(&ctx.client, plan.url.as_str(), vec![filter.clone()], budget).await {
            Ok(events) => events,
            Err(EoseFail::Closed) if page == 0 => {
                crate::rt::time::sleep(Duration::from_millis(1200)).await;
                match fetch_relay_eose_filters(&ctx.client, plan.url.as_str(), vec![filter], budget).await {
                    Ok(events) => events,
                    Err(_) => return (got, false),
                }
            }
            Err(_) => return (got, false),
        };
        let oldest = events.iter().map(|e| e.created_at.as_secs()).min();
        let fresh: Vec<Event> = events.into_iter().filter(|e| got.received.insert(e.id)).collect();
        if fresh.is_empty() {
            return (got, true);
        }
        absorb(ctx, fresh, &mut got).await;
        until = oldest;
    }
    (got, false)
}

async fn absorb<F, Fut>(ctx: &Ctx<'_, '_, F>, events: Vec<Event>, got: &mut Got)
where
    F: Fn(PreparedEvent, bool) -> Fut,
    Fut: Future<Output = bool>,
{
    got.events += events.len();
    for chunk in events.chunks(event_handler::UNWRAP_BATCH) {
        for prepared in event_handler::prepare_events(chunk, ctx.me).await {
            if (ctx.commit)(prepared, ctx.is_new).await {
                got.new_messages += 1;
            }
            if ctx.batcher.buffered() >= PERSIST_BATCH && ctx.batcher.try_flush().await.is_err() {
                ctx.flush_ok.store(false, Ordering::SeqCst);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_relays_drop_their_reasons() {
        let report = Report {
            incomplete: vec!["wss://a.example (timed out)".into(), "wss://b.example/nostr (unreachable)".into()],
            ..Default::default()
        };
        assert!(!report.complete());
        assert_eq!(report.incomplete_relays(), vec!["wss://a.example", "wss://b.example/nostr"]);
    }

    #[test]
    fn reached_none_only_when_relays_were_asked() {
        assert!(!Report::default().reached_none());
        assert!(Report { relays: 3, ..Default::default() }.reached_none());
        assert!(!Report { relays: 3, reconciled: 1, ..Default::default() }.reached_none());
    }
}
