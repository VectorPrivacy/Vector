//! Transport abstraction for Community events.
//!
//! The protocol's send/sync logic is written against this trait, not against the
//! live Nostr client, so it can be exercised end-to-end by multiple emulated
//! clients sharing an in-memory relay (no network, fully deterministic). Production
//! provides an adapter over `NOSTR_CLIENT`; tests use [`MemoryRelay`].

use nostr_sdk::prelude::*;
use crate::ClientRelayExt;

/// How much relay coverage a fetch waits to witness before returning.
///
/// The community planes distinguish POSITIVE DATA (signed events, hash-chained
/// editions — safe to act on from any relay; refuse-downgrade floors make stale
/// or replayed data harmless) from NEGATIVE VERDICTS (conclusions from absence:
/// "no rotation happened", "history ends here", "coverage complete"). A partial
/// relay view can only ever STALL consensus — and every stall heals via the
/// straggler sink, the live subscription, or the next sync — but a written
/// negative verdict has no healer. Pick the tier by what the caller CONCLUDES
/// from the result, not by how fast it wants to be.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Evidence {
    /// Return at the FIRST successful relay EOSE (+ the residual merge window).
    /// For positive-data consumers only: never conclude absence from a Fast
    /// result.
    Fast,
    /// Wait for a MAJORITY of attempted relays — time-bounded to
    /// [`QUORUM_GRACE_MS`] past the first success, so one dead relay can't
    /// gate a degraded set. A single fast-lying relay can't force an early
    /// return while its honest peers answer within the window. Note: for a
    /// 2-relay set the majority is BOTH, so a degraded pair always rides the
    /// grace bound (first success + 2s), never a dead relay's full timeout.
    #[default]
    Quorum,
    /// Wait for EVERY relay to resolve (EOSE or its per-relay timeout). For
    /// presence-latches and coverage gates whose correctness depends on the
    /// union being as complete as the reachable set allows. Forced whenever
    /// `Query::until` is set (back-page verdicts).
    Full,
}

/// A plane fetch plus how much of the relay set actually answered it.
///
/// One honest relay is enough to RUN Concord: a valid, sealed, signed event is
/// proof on its own, and no amount of corroboration adds to it. So coverage never
/// gates accepting data — a single relay's answer is taken whole.
///
/// It gates exactly one thing: concluding that something is **absent**. An empty
/// result from four relays where three timed out says nothing; the same empty
/// result where all four answered is real evidence. Both used to be a bare
/// `Vec<Event>`, indistinguishable, so every "it's gone" decision in the protocol
/// was really "nobody who happened to reply mentioned it".
///
/// Coverage is always relative to the REACHABLE set, never an absolute quorum: a
/// one-relay community reports 1/1 and its absences are as confirmed as they can
/// ever be. Nothing halts for want of relays.
#[derive(Debug, Clone)]
pub struct PlaneFetch {
    pub events: Vec<Event>,
    /// Relays that resolved successfully (an empty EOSE counts — that is a real answer).
    pub answered: usize,
    /// Relays attempted.
    pub attempted: usize,
}

impl PlaneFetch {
    /// Every relay we could reach answered, so an empty `events` IS absence.
    pub fn is_complete(&self) -> bool {
        self.answered > 0 && self.answered == self.attempted
    }

    /// For transports with no relay set of their own (the in-memory test relays and
    /// stubs): one authoritative source, fully answered.
    pub fn whole(events: Vec<Event>) -> Self {
        PlaneFetch { events, answered: 1, attempted: 1 }
    }
}

/// The slice of a relay query the Community protocol needs: event kinds, the `z`
/// pseudonym tag values, and an optional `since` floor. Production translates
/// this into a Nostr `Filter`; the in-memory relay matches it directly.
#[derive(Clone, Debug, Default)]
pub struct Query {
    pub kinds: Vec<u16>,
    /// `z` tag values to match (OR). Empty = match any (no `z` constraint).
    pub z_tags: Vec<String>,
    /// `d` tag (identifier) values to match (OR). Empty = no `d` constraint. Used to
    /// locate addressable events (e.g. a public-invite bundle by its token locator).
    pub d_tags: Vec<String>,
    /// `p` tag (recipient pubkey hex) values to match (OR). Empty = no `p` constraint.
    /// Used to fetch person-addressed giftwraps (direct invites) by recipient.
    pub p_tags: Vec<String>,
    /// `k` tag (wrapped-kind) values to match (OR). Empty = no `k` constraint. Narrows
    /// giftwrap fetches to the inner kind advertised on the wrap.
    pub k_tags: Vec<String>,
    /// Author pubkeys (hex) to match (OR). Empty = any author.
    pub authors: Vec<String>,
    /// Event ids (hex) to match (OR). Empty = any id. Used to recover a known
    /// wrap verbatim (e.g. re-opening a message's seal to build a pin proof).
    pub ids: Vec<String>,
    /// Lower bound on `created_at` (seconds), inclusive.
    pub since: Option<u64>,
    /// Upper bound on `created_at` (seconds), inclusive — pages OLDER history (events
    /// strictly/inclusively before a scroll cursor).
    pub until: Option<u64>,
    /// Max events to return (newest-first), the relay-side page cap. `None` = no limit.
    pub limit: Option<usize>,
    /// Relay-coverage requirement before the fetch may return. Defaults to
    /// [`Evidence::Quorum`]; opt into [`Evidence::Fast`] only for positive-data
    /// reads. Ignored by the in-memory test relay (inherently full-coverage).
    pub evidence: Evidence,
}

impl Query {
    /// Does `event` satisfy this query?
    pub fn matches(&self, event: &Event) -> bool {
        // Compare via `Kind` (not raw u16) so this matches `to_filter`'s
        // `Kind::Custom` normalization exactly — the live and in-memory paths must
        // agree even at kind values that nostr maps to named variants.
        if !self.kinds.is_empty() && !self.kinds.iter().any(|k| Kind::Custom(*k) == event.kind) {
            return false;
        }
        if let Some(since) = self.since {
            if event.created_at.as_secs() < since {
                return false;
            }
        }
        if let Some(until) = self.until {
            if event.created_at.as_secs() > until {
                return false;
            }
        }
        if !self.authors.is_empty() && !self.authors.iter().any(|a| *a == event.pubkey.to_hex()) {
            return false;
        }
        if !self.ids.is_empty() && !self.ids.iter().any(|i| *i == event.id.to_hex()) {
            return false;
        }
        if !self.z_tags.is_empty() && !self.matches_single_letter("z", &self.z_tags, event) {
            return false;
        }
        if !self.d_tags.is_empty() && !self.matches_single_letter("d", &self.d_tags, event) {
            return false;
        }
        if !self.p_tags.is_empty() && !self.matches_single_letter("p", &self.p_tags, event) {
            return false;
        }
        if !self.k_tags.is_empty() && !self.matches_single_letter("k", &self.k_tags, event) {
            return false;
        }
        true
    }

    fn matches_single_letter(&self, name: &str, wanted: &[String], event: &Event) -> bool {
        // ANY occurrence may satisfy the OR-set (an event can carry several `p` tags);
        // relays match every tag instance, and matches() must agree with to_filter.
        event.tags.iter().any(|t| {
            let s = t.as_slice();
            s.len() >= 2 && s[0] == name && wanted.iter().any(|w| *w == s[1])
        })
    }

    /// Translate to a Nostr relay `Filter` for the live client.
    pub fn to_filter(&self) -> Filter {
        let mut filter = Filter::new();
        if !self.kinds.is_empty() {
            filter = filter.kinds(self.kinds.iter().map(|k| Kind::Custom(*k)));
        }
        if !self.z_tags.is_empty() {
            filter = filter
                .custom_tags(SingleLetterTag::LOWERCASE_Z, self.z_tags.clone());
        }
        if !self.d_tags.is_empty() {
            filter = filter.identifiers(self.d_tags.clone());
        }
        if !self.p_tags.is_empty() {
            filter = filter
                .custom_tags(SingleLetterTag::LOWERCASE_P, self.p_tags.clone());
        }
        if !self.k_tags.is_empty() {
            filter = filter
                .custom_tags(SingleLetterTag::LOWERCASE_K, self.k_tags.clone());
        }
        if !self.authors.is_empty() {
            let authors: Vec<PublicKey> =
                self.authors.iter().filter_map(|a| PublicKey::from_hex(a).ok()).collect();
            if !authors.is_empty() {
                filter = filter.authors(authors);
            }
        }
        if !self.ids.is_empty() {
            let ids: Vec<EventId> =
                self.ids.iter().filter_map(|i| EventId::from_hex(i).ok()).collect();
            if !ids.is_empty() {
                filter = filter.ids(ids);
            }
        }
        if let Some(since) = self.since {
            filter = filter.since(Timestamp::from_secs(since));
        }
        if let Some(until) = self.until {
            filter = filter.until(Timestamp::from_secs(until));
        }
        if let Some(limit) = self.limit {
            filter = filter.limit(limit);
        }
        filter
    }
}

/// Publish + fetch over a set of relays. Async to match the live Nostr client (the
/// whole app is tokio-based and network I/O is async); `async-trait` boxes the
/// futures as `Send` so impls work inside spawned tasks.
#[async_trait::async_trait]
pub trait Transport {
    /// Publish `event` to every relay in `relays`. Single-attempt: Ok if ≥1 relay ACKs.
    async fn publish(&self, event: &Event, relays: &[String]) -> Result<(), String>;
    /// Fetch events matching `query` across `relays`, unioned and deduped by id.
    async fn fetch(&self, query: &Query, relays: &[String]) -> Result<Vec<Event>, String>;

    /// Fetch a group PLANE (events authored by `plane`'s pubkey), authenticating
    /// to AUTH-gating relays AS that plane key. On a relay that requires "the
    /// author you query must be authenticated" (Ditto), the shared client — authed
    /// as the USER — can't fetch a plane, so its catch-up REQ is CLOSED and the
    /// rotation/control is never folded (an offline member wedges at the old
    /// epoch). This fetches over a connection authed as the plane itself. Required
    /// (not defaulted — a default async-trait method forces a Sync bound on every
    /// generic caller); the in-memory test relay just fetches (no auth).
    async fn fetch_plane(&self, plane: &Keys, query: &Query, relays: &[String]) -> Result<PlaneFetch, String>;

    /// DURABLE publish for security-critical control events (rekeys, bans, the invite registry, deletes):
    /// retry **each relay independently** until it ACKs, up to [`MAX_PUBLISH_ATTEMPTS`] times, re-sending
    /// only the relays that have NOT yet accepted. The already-signed `event` is broadcast as-is — the
    /// crypto (e.g. a rekey's fresh root) is minted ONCE by the caller; this only hardens the broadcast,
    /// so a relay blip or a brief local connectivity drop can't leave a rekey/ban under-propagated.
    /// (Required, not defaulted — a default async-trait method would force a `Sync` bound on every
    /// generic `T: Transport` caller. The in-memory test relay implements it as a single publish.)
    async fn publish_durable(&self, event: &Event, relays: &[String]) -> Result<(), String>;
}

/// Per-relay broadcast cap: retry each relay up to this many times before giving up on it (matching the
/// NIP-17 deletable-DM durability). High enough to ride out a transient relay/local-network blip.
pub const MAX_PUBLISH_ATTEMPTS: usize = 30;

/// Residual union window past the moment a fetch's evidence requirement is met:
/// relays finishing within it still merge synchronously; slower ones background-
/// merge via the straggler sink.
pub const RESIDUAL_GRACE_MS: u64 = 400;

/// Time-bound on the Quorum majority wait, measured from the FIRST successful
/// EOSE. Without it a 2-relay set with one dead relay would ride the dead
/// relay's full timeout on every fetch; with it a degraded set costs
/// first-success + this bound, and a healthy set returns at majority
/// (typically far sooner).
pub const QUORUM_GRACE_MS: u64 = 2000;

/// Consecutive FULL-BUDGET failures before a relay trips.
const BREAKER_TRIP_THRESHOLD: u8 = 2;

/// How long a tripped relay stays demoted/skipped before its next fetch becomes
/// the full-budget half-open probe.
const BREAKER_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

/// Demoted per-relay timeout for tripped relays on Full drains (N ≥ 2, non-Tor
/// only) — halves a Full drain's dead-relay tail without shrinking the evidence
/// denominator.
const TRIPPED_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6);

/// Hard ceiling on the initial confirmation: at least one relay must ACK within this window or the publish
/// is a failure (we throw rather than spin on a dead/unreachable relay set forever). Once ONE relay accepts,
/// the slow/ratelimited stragglers are threaded in the background (capped at MAX_PUBLISH_ATTEMPTS).
pub const CONFIRM_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);

/// The retry engine behind a durable broadcast, factored out so it is unit-testable without a live
/// client. `send_round(pending)` performs ONE broadcast attempt to the given still-pending relays and
/// returns the subset that ACKed; this retries the rest (with `backoff` between rounds) until every relay
/// has ACKed or `max_attempts` is reached. Returns `Ok` if at least one relay ever accepted (the event is
/// durably out there; the fetch-union self-heals the stragglers), `Err` only if ZERO relays accepted
/// after exhausting the retries. Dedups `relays` first so a duplicated url isn't double-counted.
pub async fn durable_broadcast<'a, F>(
    relays: &[String],
    max_attempts: usize,
    backoff: std::time::Duration,
    mut send_round: F,
) -> Result<(), String>
where
    F: FnMut(Vec<String>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send + 'a>>,
{
    let mut pending: Vec<String> = Vec::new();
    for r in relays {
        if !pending.contains(r) {
            pending.push(r.clone());
        }
    }
    let total = pending.len();
    if total == 0 {
        return Err("no relays to broadcast to".to_string());
    }
    for attempt in 0..max_attempts {
        if pending.is_empty() {
            break;
        }
        let acked = send_round(pending.clone()).await;
        pending.retain(|r| !acked.contains(r));
        if pending.is_empty() || attempt + 1 == max_attempts {
            break;
        }
        if !backoff.is_zero() {
            crate::rt::time::sleep(backoff).await;
        }
    }
    if pending.len() < total {
        Ok(()) // at least one relay accepted; the rest were retried to the cap
    } else {
        Err(format!("no relay accepted the event after {max_attempts} attempts each"))
    }
}

/// Sink for "straggler" events — ones a SLOWER relay returns after a racing [`LiveTransport::fetch`]
/// has already handed the caller the first relay's batch. The integrator (src-tauri) registers a
/// handler that feeds them back through the normal Concord ingest path (`process_incoming` for
/// content, control/rekey re-fold for authority). The transport stays DUMB: it dedups only by event
/// id (identical bytes) and forwards everything else. Two relays disagreeing on the latest control
/// commit are two editions with DIFFERENT ids, so BOTH reach the ingester, where the deterministic
/// convergence engine (version floors + same-version tiebreakers) decides the winner. The transport
/// never resolves conflicts itself.
pub trait CommunityIngestSink: Send + Sync + 'static {
    fn ingest_stragglers(&self, events: Vec<Event>);
}

static INGEST_SINK: std::sync::OnceLock<Box<dyn CommunityIngestSink>> = std::sync::OnceLock::new();

/// Register the straggler ingest sink. Call once during app startup (mirrors `set_event_emitter`).
pub fn set_community_ingest_sink(sink: Box<dyn CommunityIngestSink>) {
    let _ = INGEST_SINK.set(sink);
}

fn submit_stragglers(events: Vec<Event>) {
    if events.is_empty() {
        return;
    }
    if let Some(sink) = INGEST_SINK.get() {
        sink.ingest_stragglers(events);
    }
}

/// Relay urls already added + connected into the shared pool this session, so `warm_client` can
/// skip the per-call `add_relay` bookkeeping + whole-pool `connect()` sweep once a community's
/// relays are established (relays auto-reconnect on drop, so the re-kick is redundant once warm).
/// Keyed by session generation: an account swap bumps the generation, invalidating every entry
/// (the pool is rebuilt for the new account).
static WARMED_RELAYS: std::sync::LazyLock<std::sync::Mutex<(u64, std::collections::HashSet<String>)>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new((0, std::collections::HashSet::new())));

/// Forget a relay from the warm set — call when a relay is REMOVED from the pool (e.g. pruned after
/// leaving a community), so a later `warm_client` for another community that shares it doesn't
/// fast-path-skip the re-add and target a relay the pool no longer holds. Poison-tolerant: the set
/// is pure optimization state, so a poisoned lock is recovered rather than propagated.
pub fn forget_warmed_relay(url: &str) {
    WARMED_RELAYS.lock().unwrap_or_else(|e| e.into_inner()).1.remove(url);
}

/// A network switch: every warm socket and breaker verdict belonged to the old route.
pub fn on_transport_switch() {
    // Replaced, not cleared: a cleared table keeps its peak allocation.
    WARMED_RELAYS.lock().unwrap_or_else(|e| e.into_inner()).1 = Default::default();
    RELAY_BREAKER.lock().unwrap_or_else(|e| e.into_inner()).1 = Default::default();
}

/// Per-relay failure tracker behind the fetch circuit breaker. Generation-keyed
/// like [`WARMED_RELAYS`] (an account swap invalidates every entry). A trip is
/// driven ONLY by consecutive failures at the relay's FULL timeout budget:
/// failures at a demoted budget never count (a slow-but-honest relay must be
/// able to recover via the post-cooldown full-budget probe), and a late EOSE
/// surfacing in the background drain resets the entry (slow ≠ dead).
#[derive(Default)]
struct BreakerEntry {
    consecutive_failures: u8,
    tripped_until: Option<web_time::Instant>,
}

static RELAY_BREAKER: std::sync::LazyLock<
    std::sync::Mutex<(u64, std::collections::HashMap<String, BreakerEntry>)>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new((0, std::collections::HashMap::new())));

/// Run `f` over the breaker map for `generation`, resetting the map if the
/// generation advanced (pure optimization state — poison-tolerant). Generation
/// is injected so tests pin a fixed one — other tests bump the REAL session
/// generation concurrently, and a mid-test bump would wipe the map under us.
fn with_breaker_at<R>(
    generation: u64,
    f: impl FnOnce(&mut std::collections::HashMap<String, BreakerEntry>) -> R,
) -> R {
    let mut guard = RELAY_BREAKER.lock().unwrap_or_else(|e| e.into_inner());
    if guard.0 != generation {
        guard.0 = generation;
        guard.1 = Default::default();
    }
    f(&mut guard.1)
}

/// Is `url` inside a trip cooldown right now?
/// Drop targets whose socket cannot progress without external intervention.
///
/// With pool auto-reconnect OFF (Vector owns reconnects), a relay sitting in
/// `Terminated`/`Shutdown`/`Banned` will not move until the reconcile loop
/// revives it — it is physically incapable of answering THIS call, so waiting
/// out its per-relay budget yields the same nothing as skipping it. This is not
/// the breaker's job: the breaker judges relays that ANSWER badly and must not
/// shrink a Quorum/Full evidence denominator; a dead socket was never part of
/// the reachable denominator to begin with. `Connecting`/`Pending`/unknown stay
/// eligible (a just-added relay may finish its handshake mid-fetch), and an
/// all-dead set falls back to the full list so the honest-timeout error paths
/// still run.
fn drop_unrevivable(targets: Vec<String>, is_unrevivable: impl Fn(&str) -> bool) -> Vec<String> {
    let alive: Vec<String> = targets.iter().filter(|r| !is_unrevivable(r)).cloned().collect();
    if alive.is_empty() {
        targets
    } else {
        alive
    }
}

/// [`drop_unrevivable`] against a live pool's current statuses.
async fn drop_unrevivable_targets(client: &Client, targets: Vec<String>) -> Vec<String> {
    let pool = client.relays().all().await;
    drop_unrevivable(targets, |r| socket_dead(&pool, r))
}

/// Every target whose socket can still answer, with no fallback.
async fn reachable_targets(client: &Client, targets: Vec<String>) -> Vec<String> {
    let pool = client.relays().all().await;
    targets.into_iter().filter(|r| !socket_dead(&pool, r)).collect()
}

fn socket_dead(pool: &std::collections::HashMap<RelayUrl, Relay>, url: &str) -> bool {
    use nostr_sdk::prelude::RelayStatus;
    RelayUrl::parse(url)
        .ok()
        .and_then(|u| pool.get(&u).map(|rl| matches!(rl.status(), RelayStatus::Terminated | RelayStatus::Shutdown | RelayStatus::Banned)))
        .unwrap_or(false)
}

fn breaker_tripped(url: &str) -> bool {
    breaker_tripped_at(crate::db::current_session_id(), url)
}

fn breaker_tripped_at(generation: u64, url: &str) -> bool {
    with_breaker_at(generation, |map| {
        map.get(url)
            .and_then(|e| e.tripped_until)
            .is_some_and(|t| web_time::Instant::now() < t)
    })
}

/// Record a per-relay fetch outcome. Success resets the entry; a failure counts
/// toward a trip only when the relay had its full timeout budget.
fn breaker_record(url: &str, success: bool, full_budget: bool) {
    breaker_record_at(crate::db::current_session_id(), url, success, full_budget)
}

fn breaker_record_at(generation: u64, url: &str, success: bool, full_budget: bool) {
    with_breaker_at(generation, |map| {
        if success {
            map.remove(url);
            return;
        }
        if !full_budget {
            return;
        }
        let e = map.entry(url.to_string()).or_default();
        e.consecutive_failures = e.consecutive_failures.saturating_add(1);
        if e.consecutive_failures >= BREAKER_TRIP_THRESHOLD {
            e.tripped_until = Some(web_time::Instant::now() + BREAKER_COOLDOWN);
        }
    })
}

/// The relays a plane read asks. A tripped relay is skipped to spare its dead
/// timeout, unless that would leave nobody to ask — but it stays in the reachable
/// set, unanswered, so a read that skipped one never claims full coverage.
struct PlaneTargets {
    ask: Vec<String>,
    skipped: usize,
}

fn plane_targets(relays: Vec<String>, tripped: impl Fn(&str) -> bool) -> PlaneTargets {
    let ask: Vec<String> = relays.iter().filter(|r| !tripped(r)).cloned().collect();
    if ask.is_empty() {
        return PlaneTargets { ask: relays, skipped: 0 };
    }
    let skipped = relays.len() - ask.len();
    PlaneTargets { ask, skipped }
}

impl PlaneTargets {
    /// The read's coverage, over the `asked` relays still standing after dead sockets left.
    fn fetched(&self, events: Vec<Event>, answered: usize, asked: usize) -> PlaneFetch {
        PlaneFetch { events, answered, attempted: asked + self.skipped }
    }
}

/// Callers own their evidence tier. The chat plane is flat, linear data — an
/// event exists or it doesn't — so no transport floor promotes its reads.
/// Every site that draws a completeness-sensitive conclusion from an `until`
/// walk (join-verify's genesis anchor, refound compaction, guestbook folds)
/// REQUESTS Full explicitly at its own Query.
fn effective_evidence(query: &Query) -> Evidence {
    query.evidence
}

// ── Plane connection pool (fetch_plane) ─────────────────────────────────────
// A plane fetch on an AUTH-gating relay needs a connection authed AS the plane
// key. Re-connecting + re-NIP-42-authing on EVERY page/epoch dominates TTFB on
// slow relays, so keep the authed connection warm and reuse it. Keyed by (plane
// pubkey, relay set). Generation-scoped: an account swap holds account A's plane
// SECRET keys, so the pool MUST clear (also freed by `clear_plane_pool`).
//
// A relay that never challenges needs no identity on the socket, so its plane
// reads share one unauthenticated connection (`OPEN_PLANES`, keyed by relay).
// Never the account client: the user's own subscriptions stay off that socket.

struct PooledPlane {
    client: Client,
    last_used: web_time::Instant,
    /// Tells a replacement apart from the entry a caller found dead.
    id: u64,
    /// Reads dialling or reading on it: neither the idle sweep nor the LRU closes it under them.
    in_use: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl PooledPlane {
    fn new(client: Client) -> Self {
        PooledPlane { client, last_used: web_time::Instant::now(), id: next_pooled_id(), in_use: Default::default() }
    }

    fn idle(&self) -> bool {
        self.in_use.load(std::sync::atomic::Ordering::Acquire) == 0
    }
}

/// A read's hold on an open pool entry, released (and the idle clock restarted) on drop.
struct InUse {
    pool: &'static PlanePool,
    generation: u64,
    key: String,
    id: u64,
    count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Drop for InUse {
    fn drop(&mut self) {
        self.pool.touch(self.generation, &self.key, self.id);
        self.count.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

fn next_pooled_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

type PlaneSlots = (u64, std::collections::HashMap<String, PooledPlane>);

#[derive(Default)]
struct PlanePool {
    /// The session generation the entries belong to, and the entries by key.
    slots: std::sync::Mutex<PlaneSlots>,
    /// Whether a sweep is waiting on the pool's next expiry: one at a time, none for an empty pool.
    sweep_armed: std::sync::atomic::AtomicBool,
}

static PLANE_POOL: std::sync::LazyLock<PlanePool> = std::sync::LazyLock::new(PlanePool::default);
static OPEN_PLANES: std::sync::LazyLock<PlanePool> = std::sync::LazyLock::new(PlanePool::default);

/// A pooled connection unused for this long is closed — long enough to span a
/// community's whole backfill, short enough not to hoard sockets.
const PLANE_POOL_IDLE_TTL: std::time::Duration = if cfg!(test) {
    std::time::Duration::from_millis(200)
} else {
    std::time::Duration::from_secs(90)
};
/// Hard cap on simultaneously-pooled plane connections (LRU-evicted).
const PLANE_POOL_MAX: usize = 24;

/// Remove the entries idle past the TTL, returning their clients to disconnect.
fn take_expired(pool: &mut std::collections::HashMap<String, PooledPlane>, now: web_time::Instant) -> Vec<Client> {
    let expired: Vec<String> = pool.iter()
        .filter(|(_, p)| p.idle() && now.duration_since(p.last_used) >= PLANE_POOL_IDLE_TTL)
        .map(|(k, _)| k.clone())
        .collect();
    expired.into_iter().filter_map(|k| pool.remove(&k)).map(|p| p.client).collect()
}

impl PlanePool {
    fn lock(&self) -> std::sync::MutexGuard<'_, PlaneSlots> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Close pooled connections as they pass the idle TTL, without waiting for another fetch to
    /// notice: after a catch-up nothing fetches, and every connection would stay open all session.
    fn arm_sweep(&'static self) {
        if self.sweep_armed.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        // spawn-detached: drains the process-wide pool; touches no account state.
        crate::rt::spawn(async move {
            loop {
                let (held, next) = {
                    let g = self.lock();
                    (!g.1.is_empty(), g.1.values().filter(|p| p.idle()).map(|p| p.last_used).min())
                };
                if !held {
                    break;
                }
                // Only entries in use: look again a TTL on, as releasing one restarts its clock.
                let due = next.map_or(PLANE_POOL_IDLE_TTL, |oldest| {
                    PLANE_POOL_IDLE_TTL.saturating_sub(web_time::Instant::now().duration_since(oldest))
                });
                crate::rt::time::sleep(due).await;
                let expired = take_expired(&mut self.lock().1, web_time::Instant::now());
                disconnect_clients(expired);
            }
            self.sweep_armed.store(false, std::sync::atomic::Ordering::Release);
            // An insert that saw the sweep still armed, just before it stood down.
            if !self.lock().1.is_empty() {
                self.arm_sweep();
            }
        });
    }

    /// Take a warm pooled client for `key` if one is fresh; also drops entries whose
    /// idle TTL expired and resets the whole pool if the session generation advanced
    /// (a swap). Returns `(Some(client_if_hit), clients_to_disconnect)`.
    fn take(&self, generation: u64, key: &str) -> (Option<Client>, Vec<Client>) {
        let (hit, evicted) = take_locked(&mut self.lock(), generation, key);
        (hit.map(|(client, _)| client), evicted)
    }

    /// [`take`](Self::take) held for a read, pooling `make()` in the same step on a miss so
    /// callers racing for one key share one client. `None` for a previous session's caller.
    fn take_or_insert(&'static self, generation: u64, key: &str, make: impl FnOnce() -> Client) -> Option<(Client, InUse, Vec<Client>)> {
        let mut g = self.lock();
        if generation < g.0 {
            return None;
        }
        let (_, mut evicted) = take_locked(&mut g, generation, key);
        if !g.1.contains_key(key) {
            evicted.extend(make_room(&mut g.1));
            g.1.insert(key.to_string(), PooledPlane::new(make()));
        }
        let entry = &g.1[key];
        entry.in_use.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let hold = InUse { pool: self, generation, key: key.to_string(), id: entry.id, count: entry.in_use.clone() };
        let client = entry.client.clone();
        drop(g);
        self.arm_sweep();
        Some((client, hold, evicted))
    }

    /// Insert a freshly-built client for `key`, LRU-evicting if over the cap. Returns
    /// ONLY the displaced LRU victim(s) to disconnect — NEVER the just-built `client`.
    /// If we don't pool it (swapped mid-build, or a concurrent miss already pooled
    /// this key), we return nothing: the caller still uses the client for this one
    /// fetch and it closes on drop, so we must not disconnect the connection it's
    /// about to run on.
    fn insert(&'static self, generation: u64, key: String, client: Client) -> Vec<Client> {
        let mut g = self.lock();
        // Swapped mid-build, or a concurrent miss already pooled this key — don't pool
        // ours (the caller uses it once, then it drops).
        if g.0 != generation || g.1.contains_key(&key) {
            return Vec::new();
        }
        let evicted = make_room(&mut g.1);
        g.1.insert(key, PooledPlane::new(client));
        drop(g);
        self.arm_sweep();
        evicted.into_iter().collect()
    }

    /// Restart `key`'s idle clock, if it is still entry `id`.
    fn touch(&self, generation: u64, key: &str, id: u64) {
        let mut g = self.lock();
        if g.0 == generation {
            if let Some(p) = g.1.get_mut(key).filter(|p| p.id == id) {
                p.last_used = web_time::Instant::now();
            }
        }
    }

    /// Remove `key` if it is still entry `id`.
    fn remove(&self, generation: u64, key: &str, id: u64) -> Option<Client> {
        let mut g = self.lock();
        if g.0 != generation || g.1.get(key).is_none_or(|p| p.id != id) {
            return None;
        }
        g.1.remove(key).map(|p| p.client)
    }

    /// Empty the pool, stamped with the live generation so an insert still in flight from the
    /// prior one is refused rather than pooled.
    fn clear(&self) -> Vec<Client> {
        let mut g = self.lock();
        g.0 = crate::db::current_session_id();
        g.1.drain().map(|(_, p)| p.client).collect()
    }
}

/// Drops entries idle past the TTL, and every entry if the session generation advanced (a
/// swap); a previous session's caller finds nothing and resets nothing. Returns the hit and
/// the clients to disconnect.
fn take_locked(g: &mut PlaneSlots, generation: u64, key: &str) -> (Option<(Client, u64)>, Vec<Client>) {
    if generation < g.0 {
        return (None, Vec::new());
    }
    let mut evicted: Vec<Client> = Vec::new();
    if g.0 != generation {
        evicted.extend(g.1.drain().map(|(_, p)| p.client));
        g.0 = generation;
    }
    let now = web_time::Instant::now();
    evicted.extend(take_expired(&mut g.1, now));
    let hit = g.1.get_mut(key).map(|p| {
        p.last_used = now;
        (p.client.clone(), p.id)
    });
    (hit, evicted)
}

/// The least recently used entry no read holds, removed when the pool is full.
fn make_room(pool: &mut std::collections::HashMap<String, PooledPlane>) -> Option<Client> {
    if pool.len() < PLANE_POOL_MAX {
        return None;
    }
    let lru_key = pool.iter().filter(|(_, p)| p.idle()).min_by_key(|(_, p)| p.last_used).map(|(k, _)| k.clone())?;
    pool.remove(&lru_key).map(|p| p.client)
}

fn plane_pool_key(plane_pk: &str, relays: &[String]) -> String {
    let mut rs: Vec<&str> = relays.iter().map(|s| s.as_str()).collect();
    rs.sort_unstable();
    let mut k = String::with_capacity(plane_pk.len() + 1 + rs.iter().map(|r| r.len() + 1).sum::<usize>());
    k.push_str(plane_pk);
    k.push('|');
    k.push_str(&rs.join(","));
    k
}

/// Disconnect the given clients off the hot path (never awaited under the lock).
/// Runtime-guarded: `clear_plane_pool` is public and a bot author may call it from
/// a non-tokio thread — `disconnect` off a live handle if there is one, else drop
/// the client (its background task ends on drop).
fn disconnect_clients(clients: Vec<Client>) {
    if clients.is_empty() {
        return;
    }
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            for c in clients {
                handle.spawn(async move {
                    c.disconnect().await;
                });
            }
        }
        Err(_) => drop(clients),
    }
}

/// Close + drop every pooled plane connection. Call on account swap — the pooled
/// clients are authenticated as the swapped-out account's community plane keys, and
/// the open ones carry its plane reads. A network switch calls it too: a dial that
/// failed on the old route is no reason to wait before the new one.
pub fn clear_plane_pool() {
    let mut drained = PLANE_POOL.clear();
    drained.extend(OPEN_PLANES.clear());
    disconnect_clients(drained);
    for relay in PLANE_RELAYS.lock().unwrap_or_else(|e| e.into_inner()).1.values_mut() {
        relay.dial_failed = None;
    }
}

/// What a relay has shown of NIP-42 to the plane reads: `Auth` once it challenged or answered
/// "auth-required:", `Open` once it served a read without either. `Auth` holds for the session.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RelayGate {
    Open,
    Auth,
}

#[derive(Clone, Copy, Default)]
struct PlaneRelay {
    gate: Option<RelayGate>,
    /// When its open connection last failed to dial: not redialled until `PLANE_REDIAL_GAP` passes,
    /// or a dead relay would charge its connect wait to every page of a backfill.
    dial_failed: Option<web_time::Instant>,
}

const PLANE_REDIAL_GAP: std::time::Duration = std::time::Duration::from_secs(30);

/// Plane reads in flight on one open connection: relays cap the subscriptions a connection holds.
const OPEN_READS_PER_RELAY: usize = 8;

/// Keyed by [`normalize_relay_url`](crate::inbox_relays::normalize_relay_url), as `auth_gate:` is.
static PLANE_RELAYS: std::sync::LazyLock<std::sync::Mutex<(u64, std::collections::HashMap<String, PlaneRelay>)>> =
    std::sync::LazyLock::new(Default::default);

fn with_plane_relay<R>(generation: u64, key: &str, f: impl FnOnce(&mut PlaneRelay) -> R) -> R {
    let mut g = PLANE_RELAYS.lock().unwrap_or_else(|e| e.into_inner());
    if generation < g.0 {
        // A previous session's read neither reads nor resets this one's.
        return f(&mut PlaneRelay::default());
    }
    if g.0 != generation {
        *g = (generation, Default::default());
    }
    f(g.1.entry(key.to_string()).or_default())
}

/// The relay's gate, seeded from the challenges this account has recorded (`auth_gate:`).
fn plane_gate(generation: u64, url: &str) -> Option<RelayGate> {
    let key = crate::inbox_relays::normalize_relay_url(url);
    if let Some(gate) = with_plane_relay(generation, &key, |r| r.gate) {
        return Some(gate);
    }
    let recorded = crate::db::get_sql_setting(format!("auth_gate:{key}")).ok().flatten().is_some();
    recorded.then(|| {
        mark_auth(generation, &key);
        RelayGate::Auth
    })
}

fn mark_auth(generation: u64, key: &str) {
    with_plane_relay(generation, key, |r| r.gate = Some(RelayGate::Auth));
}

/// [`mark_auth`], recorded for the account too (the volley routes by it), unless the account
/// changed since the read began.
fn learn_auth(generation: u64, key: &str) {
    mark_auth(generation, key);
    if crate::db::current_session_id() != generation {
        return;
    }
    let setting = format!("auth_gate:{key}");
    if crate::db::get_sql_setting(setting.clone()).ok().flatten().is_none() {
        let _ = crate::db::set_sql_setting(setting, "1".to_string());
    }
}

fn redial_waits(generation: u64, key: &str) -> bool {
    with_plane_relay(generation, key, |r| r.dial_failed).is_some_and(|at| at.elapsed() < PLANE_REDIAL_GAP)
}

fn open_read_permits(key: &str) -> std::sync::Arc<tokio::sync::Semaphore> {
    type Permits = std::collections::HashMap<String, std::sync::Arc<tokio::sync::Semaphore>>;
    static PERMITS: std::sync::LazyLock<std::sync::Mutex<Permits>> = std::sync::LazyLock::new(Default::default);
    let mut permits = PERMITS.lock().unwrap_or_else(|e| e.into_inner());
    permits.entry(key.to_string()).or_insert_with(|| std::sync::Arc::new(tokio::sync::Semaphore::new(OPEN_READS_PER_RELAY))).clone()
}

/// The pooled open client for `key`, built on a miss with the per-plane connections' lane and
/// relay options, so it routes as they do.
fn open_entry(generation: u64, key: &str) -> Option<(Client, InUse)> {
    let (client, hold, evicted) = OPEN_PLANES.take_or_insert(generation, key, || {
        crate::apply_transport(
            nostr_sdk::prelude::Client::builder().notification_channel_size(std::num::NonZeroUsize::MIN),
            crate::transport::Lane::Shared,
        )
        .build()
    })?;
    disconnect_clients(evicted);
    Some((client, hold))
}

async fn open_relay(client: &Client, url: &str) -> Option<Relay> {
    if let Ok(Some(relay)) = client.relay(url).await {
        return Some(relay);
    }
    client.add_managed_relay(url).capabilities(crate::community_relay_capabilities()).await.ok()?;
    client.relay(url).await.ok().flatten()
}

/// The relay's open plane connection, held for the read, dialled when it has none. `None` when
/// the dial failed: the relay was never reached.
async fn open_plane_client(generation: u64, url: &str, key: &str, timeout: std::time::Duration) -> Option<(Client, InUse)> {
    use RelayStatus::{Banned, Connected, Connecting, Initialized, Pending, Shutdown, Sleeping, Terminated};
    let (mut client, mut hold) = open_entry(generation, key)?;
    let mut relay = open_relay(&client, url).await?;
    if !matches!(relay.status(), Initialized | Pending | Connecting | Connected | Sleeping) {
        if redial_waits(generation, key) {
            return None;
        }
        // A dropped socket is replaced, never redialled: reconnecting the relay it ran on can
        // leave it Pending with no connection task behind it.
        disconnect_clients(OPEN_PLANES.remove(generation, key, hold.id).into_iter().collect());
        (client, hold) = open_entry(generation, key)?;
        relay = open_relay(&client, url).await?;
    }
    if matches!(relay.status(), Initialized | Sleeping) {
        if redial_waits(generation, key) {
            return None;
        }
        watch_for_challenge(generation, key, &relay);
        relay.connect();
    }
    relay.wait_for_connection(crate::relay_connect_timeout(timeout)).await;
    match relay.status() {
        Connected => {
            with_plane_relay(generation, key, |r| r.dial_failed = None);
            Some((client, hold))
        }
        Terminated | Banned | Shutdown => {
            with_plane_relay(generation, key, |r| r.dial_failed = Some(web_time::Instant::now()));
            None
        }
        // Still connecting: the REQ queues on the socket and the read counts against the relay.
        _ => Some((client, hold)),
    }
}

/// Marks the relay `Auth` when it challenges on this socket outside any read: on connect, before
/// the first REQ.
fn watch_for_challenge(generation: u64, key: &str, relay: &Relay) {
    let mut notifications = relay.notifications();
    let key = key.to_string();
    // spawn-detached: one socket's frames into the generation-keyed gate map; no account state.
    crate::rt::spawn(async move {
        while let Some(n) = notifications.next().await {
            match n {
                RelayNotification::Message { message } if matches!(*message, RelayMessage::Auth { .. }) => {
                    return mark_auth(generation, &key);
                }
                RelayNotification::RelayStatus { status: RelayStatus::Terminated | RelayStatus::Shutdown | RelayStatus::Banned } => return,
                _ => {}
            }
        }
    });
}

enum OpenRead {
    Read(Vec<Event>),
    /// It asks for AUTH: read it over a connection authed as the plane instead.
    Gated,
    Failed,
    /// Never reached, so out of the denominator like a dead socket.
    Unreachable,
}

async fn read_open_plane(generation: u64, url: &str, filter: Filter, timeout: std::time::Duration) -> OpenRead {
    let key = crate::inbox_relays::normalize_relay_url(url);
    let Some((client, hold)) = open_plane_client(generation, url, &key, timeout).await else { return OpenRead::Unreachable };
    let permits = open_read_permits(&key);
    let _permit = permits.acquire().await;
    let (gate, events) = match read_relay_eose(&client, url, vec![filter], timeout, true).await {
        // Under one lock, so a challenge the watcher saw first is never overwritten.
        Ok(events) => (with_plane_relay(generation, &key, |r| *r.gate.get_or_insert(RelayGate::Open)), events),
        Err(ReadFail::Gated) => (RelayGate::Auth, Vec::new()),
        Err(ReadFail::Eose(_)) => return OpenRead::Failed,
    };
    match gate {
        RelayGate::Open => OpenRead::Read(events),
        RelayGate::Auth => {
            learn_auth(generation, &key);
            // Out of the pool but left open: other reads may still be on it, and it closes
            // when the last of them drops it.
            drop(OPEN_PLANES.remove(generation, &key, hold.id));
            OpenRead::Gated
        }
    }
}

/// Whether Full-drain timeout demotion may apply. Over an anonymity network EVERY relay is
/// legitimately slow — a first congested pass must not cascade into pool-wide starvation, so
/// demotion applies on Clearnet only.
fn demotion_allowed() -> bool {
    crate::transport::preference() == Some(crate::transport::Kind::Clearnet)
}

/// Dial every held community's relay set on the shared warm client, ahead of
/// first use — the volley otherwise pays the TLS/WS dial (and the gating
/// relay's NIP-42 challenge round) inside its own wall time. Fire-and-forget.
/// Session-gated between the per-account DB reads and the shared-client
/// mutation: a swap mid-read must not warm account A's relays under B.
pub async fn prewarm_held_communities() {
    crate::db::scoped(async move {
        let mut relays: Vec<String> =
            crate::db::community::held_community_relays().unwrap_or_default().into_iter().collect();
        relays.sort();
        if relays.is_empty() {
            return;
        }
        let Ok(client) = LiveTransport::warm_client(&relays, std::time::Duration::from_secs(4)).await
        else {
            return;
        };
        // Elicit each relay's NIP-42 challenge NOW: a gating relay challenges on
        // the first gated REQ (never on connect), and challenges are
        // per-connection — without this the volley's priming pays the full
        // challenge round inside its own wall time every boot. The responder
        // remembers the challenge; the volley's prime then replays it instantly.
        crate::community::v2::streamauth::ensure_responder(&client);
        let probe = Keys::generate();
        let filter = Query {
            kinds: vec![crate::community::v2::stream::KIND_WRAP],
            authors: vec![probe.public_key().to_hex()],
            limit: Some(1),
            ..Default::default()
        }
        .to_filter();
        let mut elicits = futures_util::stream::FuturesUnordered::new();
        for r in &relays {
            let c = client.clone();
            let f = filter.clone();
            let r = r.clone();
            elicits.push(async move {
                let _ = fetch_relay_eose_filters(&c, &r, vec![f], std::time::Duration::from_secs(3)).await;
            });
        }
        use futures_util::StreamExt;
        while elicits.next().await.is_some() {}
    })
    .await
}

/// Why a genuine-EOSE read failed — callers that retry must not treat a
/// burned deadline like an AUTH-gate CLOSED (the retry exists for the
/// latter; repeating the former doubles a dead relay's cost).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EoseFail {
    /// The relay CLOSED the REQ (includes auth-required).
    Closed,
    /// The deadline elapsed without an EOSE.
    Deadline,
    /// No usable connection (lookup/send failure, shutdown, stream end).
    Gone,
}

/// Fetch one relay to GENUINE EOSE, or fail. `Client::fetch_events_from` (and
/// the whole nostr-sdk 0.44 fetch stack) returns `Ok(collected)` on timeout,
/// disconnect, and relay-CLOSED alike — success does NOT mean EOSE, which would
/// let a dead relay count as quorum evidence and return confident empties. So
/// the verdict is read from the relay's own notification stream instead: EOSE =
/// success (empty included — a quiet coordinate is a legitimate answer);
/// CLOSED / shutdown / deadline = failure.
///
/// Public for diagnostics (the v2 plane probe); production fetches go through
/// [`Transport::fetch`], which layers the evidence tiers on top.
pub async fn fetch_relay_eose(
    client: &Client,
    url: &str,
    filter: Filter,
    timeout: std::time::Duration,
) -> Result<Vec<Event>, ()> {
    fetch_relay_eose_filters(client, url, vec![filter], timeout).await.map_err(|_| ())
}

/// [`fetch_relay_eose`] for a multi-filter REQ (one frame, many filters — the
/// batched-volley shape). Raw REQ/CLOSE frames: the subscribe builder takes a
/// single filter, and the auto-close bookkeeping is unnecessary for a one-shot
/// read we close ourselves on every exit.
pub async fn fetch_relay_eose_filters(
    client: &Client,
    url: &str,
    filters: Vec<Filter>,
    timeout: std::time::Duration,
) -> Result<Vec<Event>, EoseFail> {
    read_relay_eose(client, url, filters, timeout, false).await.map_err(|fail| match fail {
        ReadFail::Gated => EoseFail::Closed,
        ReadFail::Eose(fail) => fail,
    })
}

enum ReadFail {
    /// CLOSED "auth-required:", or on an open plane socket any challenge.
    Gated,
    Eose(EoseFail),
}

/// On an open plane socket (`open_socket`) a challenge is a gate, since a relay may challenge and
/// then serve an unauthenticated REQ less than it holds, and a socket that closed ends the read.
async fn read_relay_eose(
    client: &Client,
    url: &str,
    filters: Vec<Filter>,
    timeout: std::time::Duration,
    open_socket: bool,
) -> Result<Vec<Event>, ReadFail> {
    use EoseFail::{Closed, Deadline, Gone};
    let relay = client.relay(url).await.map_err(|_| ReadFail::Eose(Gone))?.ok_or(ReadFail::Eose(Gone))?;
    // Subscribe to notifications BEFORE the REQ so the EOSE can't slip past.
    let mut notifications = relay.notifications();
    let sub_id = SubscriptionId::generate();
    // Close on every exit — this REQ bypasses the pool's subscription map.
    // Constructed BEFORE the send: cancellation during the send await must
    // not leave a queued REQ unguarded (a CLOSE for a never-sent REQ is
    // harmless). Drop can't await, so the CLOSE rides a detached task; all
    // callers drop on runtime threads (a JNI-thread drop would silently skip
    // the CLOSE, not panic).
    struct CloseGuard(Relay, SubscriptionId);
    impl Drop for CloseGuard {
        fn drop(&mut self) {
            let relay = self.0.clone();
            let id = self.1.clone();
            // spawn-detached: sends a relay CLOSE, touches no account state.
            crate::rt::spawn(async move {
                let _ = relay
                    .send_msg(nostr_sdk::prelude::ClientMessage::Close(std::borrow::Cow::Owned(id)))
                    .await;
            });
        }
    }
    let _close = CloseGuard(relay.clone(), sub_id.clone());
    relay
        .send_msg(nostr_sdk::prelude::ClientMessage::Req {
            subscription_id: std::borrow::Cow::Borrowed(&sub_id),
            filters: filters.into_iter().map(std::borrow::Cow::Owned).collect(),
        })
        .await
        .map_err(|_| ReadFail::Eose(Gone))?;
    let deadline = crate::rt::time::Instant::now() + timeout;
    let mut events: Vec<Event> = Vec::new();
    let mut seen: std::collections::HashSet<EventId> = std::collections::HashSet::new();
    loop {
        // 0.45 hands back a Stream, so there's no broadcast-lag case to drain:
        // the stream ending is the closed case, and the deadline is still a
        // failure rather than a false EOSE.
        let notification = match crate::rt::time::timeout_at(deadline, notifications.next()).await {
            Ok(Some(n)) => n,
            Ok(None) => return Err(ReadFail::Eose(Gone)),
            Err(_) => return Err(ReadFail::Eose(Deadline)), // timeout is NOT EOSE
        };
        match notification {
            RelayNotification::Event { subscription_id, event }
                if subscription_id == sub_id && seen.insert(event.id) =>
            {
                events.push(*event);
            }
            RelayNotification::Message { message } => match *message {
                RelayMessage::Event { subscription_id, event }
                    if *subscription_id == sub_id && seen.insert(event.id) =>
                {
                    events.push(event.into_owned());
                }
                RelayMessage::EndOfStoredEvents(id) if *id == sub_id => return Ok(events),
                RelayMessage::Closed { subscription_id, message } if *subscription_id == sub_id => {
                    return Err(match MachineReadablePrefix::parse(&message) {
                        Some(MachineReadablePrefix::AuthRequired) => ReadFail::Gated,
                        _ => ReadFail::Eose(Closed),
                    });
                }
                RelayMessage::Auth { .. } if open_socket => return Err(ReadFail::Gated),
                _ => {}
            },
            RelayNotification::RelayStatus { status: nostr_sdk::prelude::RelayStatus::Shutdown } => {
                return Err(ReadFail::Eose(Gone));
            }
            RelayNotification::RelayStatus { status: RelayStatus::Terminated | RelayStatus::Banned } if open_socket => {
                return Err(ReadFail::Eose(Gone));
            }
            _ => {}
        }
    }
}

/// Return-timing state machine for a multi-relay fetch: feed it per-relay
/// outcomes and ask whether the query's [`Evidence`] requirement is met. Pure
/// sync logic so the quorum math is unit-testable without a client.
pub(crate) struct UnionPlan {
    attempted: usize,
    successes: usize,
    resolved: usize,
    evidence: Evidence,
}

impl UnionPlan {
    pub(crate) fn new(evidence: Evidence, attempted: usize) -> Self {
        Self { attempted, successes: 0, resolved: 0, evidence }
    }

    pub(crate) fn record(&mut self, success: bool) {
        self.resolved += 1;
        if success {
            self.successes += 1;
        }
    }

    /// The tier's coverage requirement is met — the fetch may return after the
    /// residual merge window. Note Full's requirement is all-RESOLVED (a dead
    /// relay's timeout is a resolution); the zero-success case errors at the
    /// call site regardless of tier.
    pub(crate) fn satisfied(&self) -> bool {
        match self.evidence {
            Evidence::Fast => self.successes >= 1,
            Evidence::Quorum => self.successes > (self.attempted / 2),
            Evidence::Full => self.resolved >= self.attempted,
        }
    }

    /// Every relay resolved — nothing left to wait for.
    pub(crate) fn exhausted(&self) -> bool {
        self.resolved >= self.attempted
    }

    pub(crate) fn successes(&self) -> usize {
        self.successes
    }

    pub(crate) fn attempted(&self) -> usize {
        self.attempted
    }
}

/// Hold a community network op while the chosen network is still connecting, bounded by its
/// ready-wait budget: a refused send would otherwise read as "no relay accepted the event".
async fn wait_transport_ready() -> Result<(), String> {
    let budget = crate::transport::budget(crate::transport::Op::ReadyWait, std::time::Duration::ZERO);
    crate::transport::wait_ready(budget).await.map_err(|why| {
        // A block only the user can lift says so; waiting longer would not help.
        if crate::transport::status::waits_for_user() {
            return why;
        }
        let label = crate::transport::preference().map_or("Vector", crate::transport::Kind::label);
        format!("{label} is still connecting. Wait a moment and try again.")
    })
}

/// Targets the account's network can never reach (another network's names, I2P-Only) leave the
/// set, like a dead socket: they were never part of the reachable denominator. An all-refused
/// set fails with the reason.
fn drop_refused(targets: Vec<String>) -> Result<Vec<String>, String> {
    if targets.is_empty() {
        return Ok(targets);
    }
    let reachable: Vec<String> = targets.iter().filter(|r| crate::transport::refuses(r).is_none()).cloned().collect();
    if reachable.is_empty() {
        let label = crate::transport::preference().map_or("this network", crate::transport::Kind::label);
        return Err(format!("None of this community's relays are reachable on {label}."));
    }
    Ok(reachable)
}

/// Shed pooled Community relays from `candidates` that no JOINED community still needs. Used by both
/// the leave path (relays of a community we left) and the invite-preload TTL cleanup (relays an
/// unsolicited/declined invite warmed but never became a join, #297). Keep rules: a relay is kept if
/// a remaining joined community lists it, OR it carries READ/WRITE (the user's own chat relays —
/// Community relays are GOSSIP-only, so never READ/WRITE). A pruned relay re-warms automatically if
/// its invite is later accepted (the join's subscription re-adds it), so pruning a still-pending
/// invite's relay is safe.
pub async fn prune_unneeded_community_relays(candidates: &[String]) {
    if candidates.is_empty() {
        return;
    }
    let Some(client) = crate::state::nostr_client() else { return };

    let still_needed = crate::db::community::held_community_relays().unwrap_or_default();

    let pool = client;
    // all_relays(): community relays carry GOSSIP, so they're absent from `relays()` (READ/WRITE only).
    let pooled = pool.relays().all().await;
    for url in candidates {
        if still_needed.contains(url) {
            continue;
        }
        if let Ok(parsed) = nostr_sdk::prelude::RelayUrl::parse(url) {
            if let Some(relay) = pooled.get(&parsed) {
                if relay.capabilities().load().can_read() || relay.capabilities().load().can_write() {
                    continue; // a real chat relay (or an overlap) — never sever
                }
            }
            let _ = pool.remove_relay(parsed).force().await; // plain remove refuses GOSSIP
            forget_warmed_relay(url);
        }
    }
}

/// Production [`Transport`] over the live Nostr network.
///
/// Reuses the app's persistent client (`state::nostr_client`) — already connected to the user's relays,
/// which ARE the Community's relays — and targets sends/fetches at the Community relay set explicitly. No
/// per-call cold handshake (a throwaway client paid ~4s of TLS + relay handshake on every op). Any Community
/// relay the pool doesn't already hold is added idempotently, mirroring the realtime subscription.
pub struct LiveTransport {
    timeout: std::time::Duration,
}

impl Default for LiveTransport {
    fn default() -> Self {
        Self::with_timeout(std::time::Duration::from_secs(10))
    }
}

impl LiveTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Tor-aware: every caller's budget is sized for clearnet, and a community fetch
    /// that expires returns the same "no relay answered" as a genuinely dead relay —
    /// so under Tor the control plane reads as unreachable and rekey/catch-up silently
    /// stop. Raised to a floor here rather than at ~65 call sites; `max()` keeps any
    /// caller that already asked for longer.
    pub fn with_timeout(timeout: std::time::Duration) -> Self {
        Self { timeout: crate::relay_request_timeout(timeout) }
    }

    /// Grab the app's persistent client and make sure it's connected to `relays` — the Community's relays
    /// are the user's own relays, so this is almost always a pure no-op (already in the warm pool). A relay
    /// the pool doesn't hold yet is added idempotently (mirrors what the realtime subscription does), then
    /// `connect()` kicks it without disturbing the already-connected majority. Never shut this client down:
    /// it is shared. Errors only if there is no client yet or every relay url was invalid.
    pub(crate) async fn warm_client(relays: &[String], connect_timeout: std::time::Duration) -> Result<Client, String> {
        if relays.is_empty() {
            return Err("community has no relays configured".to_string());
        }
        // Before the warmed-cache fast path, so a relay warmed before a switch still waits:
        // while the chosen network is connecting every relay connect is refused at once.
        wait_transport_ready().await?;
        let client = crate::state::nostr_client().ok_or_else(|| "nostr client not initialized".to_string())?;

        // Fast path: every one of these relays was already warmed this session → the pool holds and
        // (auto-)maintains them, so skip the redundant add_relay + connect churn that otherwise runs on
        // EVERY fetch/publish. Account swaps bump the generation, dropping the cache.
        let generation = crate::db::current_session_id();
        {
            let warmed = WARMED_RELAYS.lock().unwrap_or_else(|e| e.into_inner());
            if warmed.0 == generation && relays.iter().all(|r| warmed.1.contains(r)) {
                return Ok(client);
            }
        }

        // `add_relay` returns Ok(true) if NEWLY added, Ok(false) if the pool already held it.
        // Community relays join GOSSIP-only (see `community_relay_capabilities`) so they stay warm
        // without pulling the user's DM/profile traffic onto relays they don't own — omitting the
        // capabilities would default them to READ|WRITE and leak the user's own traffic there. An
        // overlap relay already in the pool as a user relay keeps its READ+WRITE (add_relay no-ops).
        let mut added: Vec<&String> = Vec::new();
        let mut succeeded: Vec<&String> = Vec::new();
        for url in relays {
            match client
                .add_managed_relay(url.as_str())
                .capabilities(crate::community_relay_capabilities())
                .await
            {
                Ok(true) => { added.push(url); succeeded.push(url); }
                Ok(false) => { succeeded.push(url); }
                Err(_) => {}
            }
        }
        if succeeded.is_empty() {
            return Err("no valid community relays could be added".to_string());
        }
        if !added.is_empty() {
            // A relay we weren't already connected to (a Community on non-default relays): wait up to
            // the caller's cap for its socket, or the immediate fetch/send reaches zero relays. Only the
            // new relays connect, each on the transport's own budget: a pool-wide `try_connect` that
            // runs out terminates every relay still mid-handshake, and over Tor that is most of them.
            let budget = crate::relay_connect_timeout(connect_timeout);
            let mut fresh = Vec::new();
            for url in &added {
                if let Ok(Some(relay)) = client.relay(url.as_str()).await {
                    let connecting = relay.clone();
                    // spawn-detached: one relay's handshake, socket work only; it outlives the wait.
                    crate::rt::spawn(async move { let _ = connecting.try_connect().timeout(budget).await; });
                    fresh.push(relay);
                }
            }
            futures_util::future::join_all(
                fresh.iter().map(|r| crate::negentropy::wait_connected(r, connect_timeout)),
            ).await;
        } else {
            // Every relay already warm in the pool — cheap re-kick of any dropped connection, no wait.
            client.connect().await;
        }

        // Record the now-connected relays as warmed for this generation so subsequent calls fast-path
        // (reset the set if the generation advanced under us — a swap mid-warm).
        {
            let mut warmed = WARMED_RELAYS.lock().unwrap_or_else(|e| e.into_inner());
            if warmed.0 != generation {
                warmed.0 = generation;
                warmed.1 = Default::default();
            }
            for url in succeeded {
                warmed.1.insert(url.clone());
            }
        }
        Ok(client)
    }

    /// A plane read over a connection authed as the plane, for the relays that ask for AUTH:
    /// the events, how many relays answered, and how many could be asked.
    async fn read_authed_plane(&self, plane: &Keys, filter: &Filter, targets: Vec<String>, generation: u64) -> (Vec<Event>, usize, usize) {
        let key = plane_pool_key(&plane.public_key().to_hex(), &targets);

        // Reuse a warm, already-authed pooled connection if one exists — this is
        // the win: the NIP-42 handshake happens ONCE, not per page/epoch.
        let (hit, evicted) = PLANE_POOL.take(generation, &key);
        disconnect_clients(evicted);
        let client = if let Some(c) = hit {
            c
        } else {
            // Cold: a dedicated connection authed AS the plane key (a NIP-42
            // connection holds ONE identity; the shared client's is the user's).
            // Authenticates as the PLANE key, not the user — hence its own
            // authenticator rather than `nostr_client_builder()`, which resolves the
            // session identity.
            let client = crate::apply_transport(
                nostr_sdk::prelude::Client::builder()
                    .authenticator(nostr_sdk::prelude::SignerAuthenticator::new(plane.clone()))
                    // Nothing reads a plane client's pool notifications; the default
                    // channel allocates 4096 of them up front.
                    .notification_channel_size(std::num::NonZeroUsize::MIN),
                crate::transport::Lane::Shared,
            )
            .build();
            // Community relay options (GOSSIP|PING + Tor-aware ConnectionMode): a
            // bare add_relay leaves ConnectionMode::Direct, so under active Tor the
            // plane fetch — and the NIP-42 auth AS the plane key — would connect
            // direct and tie the user's IP to community membership.
            for r in &targets {
                let _ = client.add_managed_relay(r.clone()).capabilities(crate::community_relay_capabilities()).await;
            }
            client.connect().await;
            // Warmup with the gated filter shape triggers each relay's NIP-42
            // challenge so auto-auth completes ONCE here; pooled reuses skip it.
            for r in &targets {
                let _ = client
                    .fetch_events(nostr_sdk::prelude::ReqTarget::single(r.clone(), [filter.clone()]))
                    // Tor-aware: this warmup carries a NIP-42 challenge round trip, so a
                    // clearnet 5s budget expires mid-auth and every relay ends up
                    // unauthenticated — which surfaces as `0/N attempted` below.
                    .timeout(crate::relay_request_timeout(std::time::Duration::from_secs(5)))
                    .await;
            }
            let ev = PLANE_POOL.insert(generation, key, client.clone());
            disconnect_clients(ev);
            client
        };

        // This walk is SERIAL, so a dead socket taxes every page its full budget;
        // judged against the PLANE pool's statuses (a separate client — nothing
        // revives its members, so an unrevivable one here is dead for good). No
        // all-dead fallback: the open relays may have answered, and with none
        // reached at all the fetch errs anyway.
        let targets = reachable_targets(&client, targets).await;

        let mut result: Vec<Event> = Vec::new();
        let mut seen: std::collections::HashSet<EventId> = std::collections::HashSet::new();
        let mut successes = 0usize;
        // CONCURRENTLY. Serially, one dead relay cost its entire timeout before the
        // next was even tried, and the whole plane read paid that on every page of
        // every epoch of a catch-up. The union is order-independent, so there is
        // nothing to serialize for.
        {
            use futures_util::stream::{FuturesUnordered, StreamExt};
            let mut inflight: FuturesUnordered<_> = targets
                .iter()
                .map(|r| {
                    let client = client.clone();
                    let filter = filter.clone();
                    let timeout = self.timeout;
                    async move { (r.clone(), fetch_relay_eose(&client, r, filter, timeout).await) }
                })
                .collect();
            while let Some((r, res)) = inflight.next().await {
                // Feed the shared breaker so this auth path both benefits from AND
                // contributes to the pool-wide dead-relay knowledge.
                breaker_record(&r, res.is_ok(), true);
                if let Ok(events) = res {
                    successes += 1;
                    for e in events {
                        if seen.insert(e.id) {
                            result.push(e);
                        }
                    }
                }
            }
        }
        // The client stays POOLED (not disconnected) for the next page/epoch/community.
        let asked = targets.len();
        (result, successes, asked)
    }

    /// Coverage-reporting fetch — same engine as [`Transport::fetch`], returning
    /// `(events, relays_that_EOSEd, relays_attempted)`. The boot control probe
    /// reads the counts to decide whether its cursor may advance (full coverage
    /// only — a majority return must not skip a down relay's pending editions).
    pub async fn fetch_counted(&self, query: &Query, relays: &[String]) -> Result<(Vec<Event>, usize, usize), String> {
        let client = Self::warm_client(relays, self.timeout).await?;
        let base_timeout = self.timeout;
        let filter = query.to_filter();

        let evidence = effective_evidence(query);

        let mut targets: Vec<String> = Vec::new();
        for r in relays {
            if !targets.contains(r) {
                targets.push(r.clone());
            }
        }
        let mut targets = drop_refused(targets)?;
        // Even a Full drain skips a DEAD socket: with pool auto-reconnect off, a
        // Terminated relay cannot answer this call no matter how long we wait, so
        // its timeout buys byte-identical evidence to skipping it — and paid per
        // PAGE, it is what stretched one dead relay into a minute-long rotation.
        targets = drop_unrevivable_targets(&client, targets).await;

        // Fast tier: skip tripped relays outright (pure bandwidth save — the
        // evidence bar is ≥1 success either way, and the union self-heals).
        // Never skip down to an empty set. Quorum/Full always attempt every
        // relay so a trip can't shrink their evidence denominator.
        if evidence == Evidence::Fast && targets.len() >= 2 {
            let alive: Vec<String> =
                targets.iter().filter(|r| !breaker_tripped(r)).cloned().collect();
            if !alive.is_empty() {
                targets = alive;
            }
        }

        // One relay → nothing to race; the sole relay always gets the full
        // budget (there is nothing to union around).
        if targets.len() <= 1 {
            let Some(url) = targets.first() else {
                return Err("no valid relay to fetch from".to_string());
            };
            let res = fetch_relay_eose(&client, url, filter, base_timeout)
                .await
                .map_err(|_| format!("relay did not answer the fetch: {url}"));
            breaker_record(url, res.is_ok(), true);
            return res.map(|evs| (evs, 1, 1));
        }

        fn merge_events(
            evs: Vec<Event>,
            result: &mut Vec<Event>,
            seen: &mut std::collections::HashSet<EventId>,
        ) {
            for e in evs {
                if seen.insert(e.id) {
                    result.push(e);
                }
            }
        }

        // RACE per-relay (mirrors the publish first-ACK race) and union per the
        // evidence tier. Every relay's outcome is tracked — genuine EOSE vs
        // error/timeout — so an all-dead pool surfaces as Err, never as a
        // confident empty answer. Tripped relays keep their place in the
        // denominator; on Full drains (non-Tor) they run on a demoted budget so
        // a dead relay's tail shrinks without weakening the union.
        let demote = evidence == Evidence::Full && demotion_allowed();
        use futures_util::stream::{FuturesUnordered, StreamExt};
        let mut fetches: FuturesUnordered<_> = targets
            .iter()
            .map(|r| {
                let client = client.clone();
                let filter = filter.clone();
                let r = r.clone();
                let timeout = if demote && breaker_tripped(&r) {
                    TRIPPED_TIMEOUT.min(base_timeout)
                } else {
                    base_timeout
                };
                let full_budget = timeout >= base_timeout;
                // spawn-detached: relay I/O only — no account storage is touched.
                crate::rt::spawn(async move {
                    let out = fetch_relay_eose(&client, &r, filter, timeout).await;
                    (r, full_budget, out)
                })
            })
            .collect();

        let mut plan = UnionPlan::new(evidence, targets.len());
        let mut result: Vec<Event> = Vec::new();
        let mut union_ids: std::collections::HashSet<EventId> = std::collections::HashSet::new();

        // Phase 1 — wait for the tier's coverage requirement. Quorum's majority
        // wait is TIME-BOUNDED from the first success so a dead relay can't
        // gate a degraded set (a 2-relay community with one relay down must not
        // ride that relay's timeout on every fetch).
        let mut quorum_deadline: Option<crate::rt::time::Instant> = None;
        let mut quorum_window_closed = false;
        while !plan.satisfied() && !plan.exhausted() {
            let next = match quorum_deadline {
                Some(deadline) => match crate::rt::time::timeout_at(deadline, fetches.next()).await {
                    Ok(n) => n,
                    Err(_) => {
                        quorum_window_closed = true;
                        break; // window closed — return with what we hold (≥1 success)
                    }
                },
                None => fetches.next().await,
            };
            let Some(joined) = next else { break };
            match joined {
                Ok((url, full_budget, Ok(evs))) => {
                    breaker_record(&url, true, full_budget);
                    merge_events(evs, &mut result, &mut union_ids);
                    plan.record(true);
                    if evidence == Evidence::Quorum && quorum_deadline.is_none() {
                        quorum_deadline = Some(
                            crate::rt::time::Instant::now()
                                + std::time::Duration::from_millis(QUORUM_GRACE_MS),
                        );
                    }
                }
                Ok((url, full_budget, Err(()))) => {
                    breaker_record(&url, false, full_budget);
                    plan.record(false);
                }
                Err(_) => plan.record(false), // task join error — a resolved failure
            }
        }

        if plan.successes() == 0 {
            return Err(format!(
                "no relay answered the fetch (0/{} attempted)",
                plan.attempted()
            ));
        }

        // Phase 2 — residual union window: relays finishing just behind the
        // requirement still merge synchronously. Skipped when the quorum window
        // already expired (that wait subsumes this one).
        if !fetches.is_empty() && !quorum_window_closed {
            let grace = crate::rt::time::sleep(std::time::Duration::from_millis(RESIDUAL_GRACE_MS));
            tokio::pin!(grace);
            loop {
                tokio::select! {
                    _ = &mut grace => break,
                    next = fetches.next() => match next {
                        Some(Ok((url, full_budget, Ok(evs)))) => {
                            breaker_record(&url, true, full_budget);
                            merge_events(evs, &mut result, &mut union_ids);
                        }
                        Some(Ok((url, full_budget, Err(())))) => {
                            breaker_record(&url, false, full_budget);
                        }
                        Some(Err(_)) => continue,
                        None => break,
                    }
                }
            }
        }

        // Background-merge the relays that haven't finished: dedup by event id ONLY (identical bytes)
        // against what we returned, then hand the rest to the ingester. Conflicting editions carry
        // distinct ids, so the protocol's convergence engine resolves them — not the transport.
        if !fetches.is_empty() {
            let seen: std::collections::HashSet<EventId> = result.iter().map(|e| e.id).collect();
            // Captured BEFORE the drain spawn: the drain can outlive an account swap, and
            // stragglers fetched under the prior session must not feed the new one's ingest.
            crate::db::spawn_bound(async move {
                let mut extra: Vec<Event> = Vec::new();
                let mut extra_ids: std::collections::HashSet<EventId> = std::collections::HashSet::new();
                while let Some(joined) = fetches.next().await {
                    if let Ok((url, full_budget, out)) = joined {
                        // A late EOSE is still a SUCCESS — slow ≠ dead; without
                        // this a relay slower than the residual window could
                        // never un-trip.
                        breaker_record(&url, out.is_ok(), full_budget);
                        if let Ok(evs) = out {
                            for e in evs {
                                if !seen.contains(&e.id) && extra_ids.insert(e.id) {
                                    extra.push(e);
                                }
                            }
                        }
                    }
                }
                submit_stragglers(extra);
            });
        }

        Ok((result, plan.successes(), plan.attempted()))
    }
}

/// Send to one pooled relay, true on its ACK. Callers verify the event and mark it as ours
/// once; `Client::send_event` would redo both for every relay, every round.
async fn send_to_relay(client: &Client, event: &Event, url: &str) -> bool {
    send_to_relay_why(client, event, url).await.is_ok()
}

/// [`send_to_relay`], with the relay's own reason when it refuses.
async fn send_to_relay_why(client: &Client, event: &Event, url: &str) -> Result<(), String> {
    let Ok(Some(relay)) = client.relay(url).await else { return Err("not connected".to_string()) };
    let wait = crate::relay_request_timeout(std::time::Duration::from_secs(10));
    relay.send_event(event).ok_timeout(wait).authentication_timeout(wait).await.map(|_| ()).map_err(|e| e.to_string())
}

/// Mark our own event as seen before it goes out, so the relays' echoes of it are dropped
/// before any work. The mark only silences echoes; it never stands for delivery, which is
/// each relay's OK. Returns whether the mark is ours: an id seen before stays seen.
pub(crate) async fn mark_own(client: &Client, event: &Event) -> bool {
    let fresh = matches!(
        client.database().check_id(&event.id).await,
        Ok(nostr_sdk::prelude::DatabaseEventStatus::NotExistent)
    );
    let _ = client.database().save_event(event).await;
    fresh
}

/// Undo a mark [`mark_own`] made once no relay took the event, so a copy that turns up later
/// is handled like anyone else's.
pub(crate) async fn unmark_own(client: &Client, event: &Event) {
    let _ = client.database().delete(nostr_sdk::prelude::Filter::new().id(event.id)).await;
}

#[async_trait::async_trait]
impl Transport for LiveTransport {
    async fn publish(&self, event: &Event, relays: &[String]) -> Result<(), String> {
        event.verify().map_err(|e| e.to_string())?;
        crate::event_size::check(event)?;
        let client = Self::warm_client(relays, self.timeout).await?;
        let timeout = self.timeout;
        let mut targets: Vec<String> = Vec::new();
        for r in relays { if !targets.contains(r) { targets.push(r.clone()); } }
        targets = drop_refused(targets)?;
        // A dead socket can't ACK and won't be revived mid-send — don't spawn at it.
        targets = drop_unrevivable_targets(&client, targets).await;
        // Fan out one send per relay and RETURN on the first ACK — never wait for the slowest relay (a
        // distant/ratelimited one must not gate a reaction/edit/message). Each send is SPAWNED, so the rest
        // keep delivering to every relay after we return (dropping a JoinHandle detaches, it doesn't abort).
        // Single attempt — durable retry is publish_durable's job.
        use futures_util::stream::{FuturesUnordered, StreamExt};
        let marked = mark_own(&client, event).await;
        let mut sends: FuturesUnordered<_> = targets
            .into_iter()
            .map(|r| {
                let client = client.clone();
                let event = event.clone();
                // spawn-detached: relay I/O only — no account storage is touched.
                crate::rt::spawn(async move {
                    match crate::rt::time::timeout(timeout, send_to_relay_why(&client, &event, &r)).await {
                        Ok(sent) => sent,
                        Err(_) => Err("timed out".to_string()),
                    }
                })
            })
            .collect();
        let mut why = None;
        while let Some(joined) = sends.next().await {
            match joined {
                Ok(Ok(())) => return Ok(()), // first relay ACKed; the others keep delivering in the background
                Ok(Err(e)) => why = Some(e),
                Err(_) => {}
            }
        }
        // Every send has finished without an ACK.
        if marked {
            unmark_own(&client, event).await;
        }
        Err(match why {
            Some(why) => format!("no relay accepted the event: {why}"),
            None => "no relay accepted the event".to_string(),
        })
    }

    async fn fetch(&self, query: &Query, relays: &[String]) -> Result<Vec<Event>, String> {
        self.fetch_counted(query, relays).await.map(|(events, _successes, _attempted)| events)
    }

    async fn fetch_plane(&self, plane: &Keys, query: &Query, relays: &[String]) -> Result<PlaneFetch, String> {
        if relays.is_empty() {
            return Ok(PlaneFetch { events: Vec::new(), answered: 0, attempted: 0 });
        }
        wait_transport_ready().await?;
        // Skip relays the shared breaker already knows are dead — don't even pay
        // their connect handshake + warmup + timeout (DM negentropy and other sweeps
        // trip them early, so by the time a v2 backfill runs they're usually
        // marked). Keep all if that would leave none — a slow fetch beats no fetch.
        let relays = drop_refused(relays.to_vec())?;
        let selected = plane_targets(relays, breaker_tripped);
        let filter = query.to_filter();
        let generation = crate::db::current_session_id();
        let (gated, open): (Vec<String>, Vec<String>) =
            selected.ask.iter().cloned().partition(|r| plane_gate(generation, r) == Some(RelayGate::Auth));

        let mut result: Vec<Event> = Vec::new();
        let mut seen: std::collections::HashSet<EventId> = std::collections::HashSet::new();
        let mut keep = |events: Vec<Event>| result.extend(events.into_iter().filter(|e| seen.insert(e.id)));
        let (mut successes, mut asked) = (0usize, 0usize);
        let open_reads = futures_util::future::join_all(open.iter().map(|r| {
            let filter = filter.clone();
            async move { (r, read_open_plane(generation, r, filter, self.timeout).await) }
        }));
        // Relays already known to gate read alongside, not after a slow open relay's dial.
        let known_gated = async {
            if gated.is_empty() {
                (Vec::new(), 0, 0)
            } else {
                self.read_authed_plane(plane, &filter, gated.clone(), generation).await
            }
        };
        let (reads, (events, answered, reached)) = futures_util::future::join(open_reads, known_gated).await;
        successes += answered;
        asked += reached;
        keep(events);
        let mut newly_gated: Vec<String> = Vec::new();
        for (r, read) in reads {
            match read {
                OpenRead::Read(events) => {
                    breaker_record(r, true, true);
                    successes += 1;
                    asked += 1;
                    keep(events);
                }
                OpenRead::Failed => {
                    breaker_record(r, false, true);
                    asked += 1;
                }
                OpenRead::Unreachable => {}
                OpenRead::Gated => newly_gated.push(r.clone()),
            }
        }
        if !newly_gated.is_empty() {
            let (events, answered, reached) = self.read_authed_plane(plane, &filter, newly_gated, generation).await;
            successes += answered;
            asked += reached;
            keep(events);
        }
        // Zero EOSE = every relay refused/timed out — an honest transient failure,
        // NOT a "the plane is empty" verdict (a genuine empty plane EOSEs with no
        // events, which counts as a success). A confident-empty here could mask a
        // rotation from a caller that concludes absence.
        if successes == 0 {
            return Err(format!("no relay answered the plane fetch (0/{} attempted)", selected.ask.len()));
        }
        Ok(selected.fetched(result, successes, asked))
    }

    async fn publish_durable(&self, event: &Event, relays: &[String]) -> Result<(), String> {
        // "Durable" = confirm the network has it (≥1 relay ACKs within CONFIRM_WINDOW), then keep bugging the
        // SLOW/ratelimited stragglers (Damus-style 1-event/min) in the BACKGROUND. If NOTHING ACKs in the
        // window we throw — a dead relay set is a failure, not an endless wait. Uses the shared warm client
        // (NEVER shut down here) and targets the community relay set across rounds.
        event.verify().map_err(|e| e.to_string())?;
        crate::event_size::check(event)?;
        let client = Self::warm_client(relays, self.timeout).await?;
        let timeout = self.timeout;
        let event = event.clone();
        let backoff = std::time::Duration::from_millis(750);
        let mut pending: Vec<String> = Vec::new();
        for r in relays { if !pending.contains(r) { pending.push(r.clone()); } }
        if pending.is_empty() {
            return Err("no relays to broadcast to".to_string());
        }
        let mut pending = drop_refused(pending)?;

        // Phase 1 — CONFIRM: RACE the relays and return the instant ANY one ACKs — never wait for the
        // slowest. `send_event_to(all)` blocks on the slowest relay (a distant/ratelimited one dominates the
        // latency), so we fan out one send per relay and take the first winner; the losers are cancelled and
        // re-sent in the background. Retry rounds within CONFIRM_WINDOW; zero ACKs in the window = failure.
        let mut acked_any = false;
        let marked = mark_own(&client, &event).await;
        // Over Tor one round alone can take the 30 s request floor: the 60 s connect floor leaves
        // room for a retry. Clearnet keeps 30 s.
        let confirm_window = crate::relay_connect_timeout(CONFIRM_WINDOW);
        let _ = crate::rt::time::timeout(confirm_window, async {
            loop {
                // Per ROUND, not from `pending`: a dead socket stays pending (the
                // reconcile loop may revive it before the stragglers give up) but a
                // confirm round must not spend its budget on a relay that cannot ACK.
                let round = drop_unrevivable_targets(&client, pending.clone()).await;
                let sends = round.iter().cloned().map(|r| {
                    let client = &client;
                    let event = &event;
                    Box::pin(async move {
                        match crate::rt::time::timeout(timeout, send_to_relay(client, event, &r)).await {
                            Ok(true) => Ok(r),
                            _ => Err(()),
                        }
                    })
                });
                if let Ok((winner, _losers)) = futures_util::future::select_ok(sends).await {
                    acked_any = true;
                    pending.retain(|r| r != &winner);
                    break;
                }
                crate::rt::time::sleep(backoff).await;
            }
        })
        .await;

        if !acked_any {
            if marked {
                unmark_own(&client, &event).await;
            }
            return Err(format!("no relay accepted the event within {}s", confirm_window.as_secs()));
        }
        if pending.is_empty() {
            return Ok(()); // every relay ACKed during the confirm phase
        }

        // Phase 2 — BACKGROUND: thread the laggards through a durable publisher (retries each up to
        // MAX_PUBLISH_ATTEMPTS at a 750ms backoff, so it can't run forever). The caller returns NOW with its
        // confirmed ACK; 's fetch-union heals anything that never lands. The client is shared — not torn
        // down — so the spawned task just drops its handle when finished.
        // spawn-detached: relay I/O only — no account storage is touched.
        crate::rt::spawn(async move {
            let client_ref = &client;
            let event_ref = &event;
            let _ = durable_broadcast(&pending, MAX_PUBLISH_ATTEMPTS, backoff, move |round| {
                Box::pin(async move {
                    match crate::rt::time::timeout(timeout, crate::transport_aware(client_ref.send_event(event_ref)).to(round.clone())).await {
                        Ok(Ok(output)) => round.into_iter().filter(|p| RelayUrl::parse(p).map(|u| output.success.contains_key(&u)).unwrap_or(false)).collect(),
                        _ => Vec::new(),
                    }
                })
            })
            .await;
        });
        Ok(())
    }
}

#[cfg(any(test, feature = "bench"))]
pub(crate) mod memory {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    /// An in-memory stand-in for the Community's relay set. Stores events per relay
    /// url so tests can model partial propagation (a relay that missed an event) and
    /// verify the redundancy/self-heal property: a fetch across the set unions +
    /// dedups, so a gap on one relay is covered by its siblings.
    pub struct MemoryRelay {
        per_relay: Mutex<HashMap<String, Vec<Event>>>,
        subscribers: Mutex<Vec<(Query, tokio::sync::mpsc::UnboundedSender<Event>)>>,
        /// Relays that judge a wrap's deletion by its `p`-tagged owner, as CORD-01 asks.
        owner_rule: Mutex<HashSet<String>>,
    }

    /// NIP-01 ephemeral range: relays stream these to live subscriptions but never store
    /// them, so a fetch on a real relay can never return one.
    fn is_ephemeral(kind: u16) -> bool {
        (20000..30000).contains(&kind)
    }

    impl MemoryRelay {
        pub fn new() -> Self {
            MemoryRelay {
                per_relay: Mutex::new(HashMap::new()),
                subscribers: Mutex::new(Vec::new()),
                owner_rule: Mutex::new(HashSet::new()),
            }
        }

        /// Have `relays` follow CORD-01 §Deletions: a wrap goes only by its `p`-tagged owner's
        /// deletion, never its author's.
        pub fn follow_cord01(&self, relays: &[String]) {
            self.owner_rule.lock().unwrap().extend(relays.iter().cloned());
        }

        /// Total stored events across every relay url — lets a test assert that a
        /// code path published NOTHING, which an absence-of-effect check can't
        /// express by fetching (an empty result also means "never published").
        pub fn stored_count(&self) -> usize {
            self.per_relay.lock().unwrap().values().map(|v| v.len()).sum()
        }

        /// Open a live subscription: every subsequent publish/inject matching `query` is
        /// delivered — ephemerals included, which stream but are never stored.
        pub fn subscribe(&self, query: Query) -> tokio::sync::mpsc::UnboundedReceiver<Event> {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            self.subscribers.lock().unwrap().push((query, tx));
            rx
        }

        /// Push `event` to every live matching subscriber, pruning closed ones.
        fn deliver(&self, event: &Event) {
            self.subscribers.lock().unwrap().retain(|(q, tx)| {
                if q.matches(event) {
                    tx.send(event.clone()).is_ok()
                } else {
                    !tx.is_closed()
                }
            });
        }

        /// Publish to ONLY a subset of relays — used to simulate a relay missing an
        /// event (e.g. a dropped rekey) for redundancy tests.
        pub fn inject(&self, event: &Event, relays: &[String]) {
            self.deliver(event);
            if is_ephemeral(event.kind.as_u16()) {
                return; // live-delivered above, never stored
            }
            // Replaceable kinds: parameterized (30000-39999, keyed by (kind, pubkey, d-tag)) AND
            // standard (10000-19999, plus 0/3, keyed by (kind, pubkey) — the d-tag is "") — a relay keeps
            // only the latest at that coordinate, so a new event REPLACES the old (NIP-01). This is what
            // makes a revocation tombstone overwrite a bundle, and a fresh Community List supersede the last one,
            // even on relays that ignore deletions — model it so tests match real relay behavior.
            let d_tag = |e: &Event| e.tags.iter().find_map(|t| {
                let s = t.as_slice();
                (s.len() >= 2 && s[0] == "d").then(|| s[1].clone())
            }).unwrap_or_default();
            let k = event.kind.as_u16();
            let replaceable = (30000..40000).contains(&k) || (10000..20000).contains(&k) || k == 0 || k == 3;
            let coord = (event.kind.as_u16(), event.pubkey, d_tag(event));
            let mut map = self.per_relay.lock().unwrap();
            for r in relays {
                let v = map.entry(r.clone()).or_default();
                if replaceable {
                    v.retain(|e| (e.kind.as_u16(), e.pubkey, d_tag(e)) != coord);
                }
                v.push(event.clone());
            }
        }

        /// How many events a given relay holds (test introspection).
        pub fn count_on(&self, relay: &str) -> usize {
            self.per_relay.lock().unwrap().get(relay).map_or(0, |v| v.len())
        }

        /// Apply a NIP-09 deletion: drop any stored event matched by the deletion's `e`
        /// tags (by id) OR `a` tags (addressable coordinate `kind:pubkey:d`), AND whose
        /// author matches the deletion's author (a deleter can only delete their own
        /// events — same rule strfry enforces). A relay following CORD-01 takes a wrap's
        /// `p`-tagged owner in place of its author.
        fn apply_deletion(&self, deletion: &Event, relays: &[String]) {
            let owner_rule = self.owner_rule.lock().unwrap().clone();
            let mut id_targets: HashSet<String> = HashSet::new();
            let mut coord_targets: HashSet<String> = HashSet::new();
            for t in deletion.tags.iter() {
                let s = t.as_slice();
                if s.len() >= 2 && s[0] == "e" {
                    id_targets.insert(s[1].clone());
                } else if s.len() >= 2 && s[0] == "a" {
                    coord_targets.insert(s[1].clone());
                }
            }
            let mut map = self.per_relay.lock().unwrap();
            for r in relays {
                let by_owner = owner_rule.contains(r);
                if let Some(events) = map.get_mut(r) {
                    events.retain(|e| {
                        let deleter = if by_owner && e.kind.as_u16() == 1059 {
                            crate::community::v2::stream::wrap_owner(e)
                        } else {
                            Some(e.pubkey)
                        };
                        if deleter != Some(deletion.pubkey) {
                            return true;
                        }
                        if id_targets.contains(&e.id.to_hex()) {
                            return false;
                        }
                        // Addressable coordinate "kind:pubkey:d-identifier".
                        let d = e.tags.iter().find_map(|t| {
                            let s = t.as_slice();
                            (s.len() >= 2 && s[0] == "d").then(|| s[1].clone())
                        }).unwrap_or_default();
                        let coord = format!("{}:{}:{}", e.kind.as_u16(), e.pubkey.to_hex(), d);
                        !coord_targets.contains(&coord)
                    });
                }
            }
        }
    }

    #[async_trait::async_trait]
    impl Transport for MemoryRelay {
        async fn publish(&self, event: &Event, relays: &[String]) -> Result<(), String> {
            // Refuse what a real relay would, so every test publish is a size test too.
            crate::event_size::check(event)?;
            // Honor NIP-09 so the delete→gone cycle is testable offline.
            if event.kind == Kind::EventDeletion {
                self.apply_deletion(event, relays);
                self.deliver(event);
            } else {
                self.inject(event, relays);
            }
            Ok(())
        }

        // The in-memory relay always accepts, so a "durable" publish is just a publish (the retry
        // engine itself is unit-tested separately via `durable_broadcast`).
        async fn publish_durable(&self, event: &Event, relays: &[String]) -> Result<(), String> {
            self.publish(event, relays).await
        }

        async fn fetch(&self, query: &Query, relays: &[String]) -> Result<Vec<Event>, String> {
            let map = self.per_relay.lock().unwrap();
            let mut seen = HashSet::new();
            let mut out = Vec::new();
            for r in relays {
                if let Some(events) = map.get(r) {
                    for ev in events {
                        // Never stored, but guard the read path too: a real relay never
                        // serves an ephemeral from a fetch, whatever got in.
                        if is_ephemeral(ev.kind.as_u16()) {
                            continue;
                        }
                        if query.matches(ev) && seen.insert(ev.id) {
                            out.push(ev.clone());
                        }
                    }
                }
            }
            // Apply the relay-side page cap newest-first (matches how relays honor `limit`):
            // sort by created_at desc, keep the newest `limit`. Mirrors production paging.
            if let Some(limit) = query.limit {
                out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));
                out.truncate(limit);
            }
            Ok(out)
        }

        async fn fetch_plane(&self, _plane: &Keys, query: &Query, relays: &[String]) -> Result<PlaneFetch, String> {
            // No auth in the in-memory relay — a plane fetch is just a fetch.
            Ok(PlaneFetch::whole(self.fetch(query, relays).await?))
        }
    }
}

/// A relay on a real loopback socket, for what only a socket shows: how many connections a
/// client opens, and how it answers a NIP-42 gate.
#[cfg(test)]
pub(crate) mod wire {
    use futures_util::{SinkExt, StreamExt};
    use nostr_sdk::prelude::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio_tungstenite::tungstenite::Message as Ws;

    /// Answers REQs from what it holds. Gated, it serves a REQ only to a connection authed as
    /// every author it names, as Ditto does: otherwise one AUTH challenge per connection and a
    /// CLOSED "auth-required:", or with `quiet` an EOSE with nothing. Greeting, it challenges on
    /// connect and serves reads anyway. Refusing, it CLOSEs every REQ with an error.
    pub struct WireRelay {
        pub port: u16,
        opened: Arc<AtomicUsize>,
        modes: Arc<Modes>,
        held: Arc<Mutex<Vec<Event>>>,
    }

    #[derive(Default)]
    struct Modes {
        gated: AtomicBool,
        quiet: AtomicBool,
        greets: AtomicBool,
        refuses: AtomicBool,
        /// Milliseconds a new connection waits before its WebSocket handshake.
        stall_ms: std::sync::atomic::AtomicU64,
        /// Subscriptions one connection may hold open; 0 for no limit.
        sub_cap: AtomicUsize,
        /// Close the connection on the first REQ, unanswered.
        hangs_up: AtomicBool,
        /// Gate without ever sending a challenge.
        mute: AtomicBool,
        /// Challenge once a connection has been served a read.
        challenges_after: AtomicBool,
    }

    impl WireRelay {
        pub async fn start() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let relay = WireRelay { port, opened: Arc::default(), modes: Arc::default(), held: Arc::default() };
            let (opened, modes, held) = (relay.opened.clone(), relay.modes.clone(), relay.held.clone());
            let url = RelayUrl::parse(&relay.url()).unwrap();
            tokio::spawn(async move {
                while let Ok((tcp, _)) = listener.accept().await {
                    opened.fetch_add(1, Ordering::SeqCst);
                    tokio::spawn(serve(tcp, url.clone(), modes.clone(), held.clone()));
                }
            });
            relay
        }

        pub fn url(&self) -> String {
            format!("ws://127.0.0.1:{}", self.port)
        }

        /// Connections accepted so far.
        pub fn opened(&self) -> usize {
            self.opened.load(Ordering::SeqCst)
        }

        pub fn gate(&self, on: bool) {
            self.modes.gated.store(on, Ordering::SeqCst);
        }

        pub fn quiet(&self, on: bool) {
            self.modes.quiet.store(on, Ordering::SeqCst);
        }

        pub fn greet(&self, on: bool) {
            self.modes.greets.store(on, Ordering::SeqCst);
        }

        pub fn refuse(&self, on: bool) {
            self.modes.refuses.store(on, Ordering::SeqCst);
        }

        pub fn stall(&self, by: std::time::Duration) {
            self.modes.stall_ms.store(by.as_millis() as u64, Ordering::SeqCst);
        }

        pub fn cap_subs(&self, n: usize) {
            self.modes.sub_cap.store(n, Ordering::SeqCst);
        }

        pub fn hang_up(&self, on: bool) {
            self.modes.hangs_up.store(on, Ordering::SeqCst);
        }

        pub fn mute(&self, on: bool) {
            self.modes.mute.store(on, Ordering::SeqCst);
        }

        pub fn challenge_after_a_read(&self, on: bool) {
            self.modes.challenges_after.store(on, Ordering::SeqCst);
        }

        pub fn hold(&self, events: impl IntoIterator<Item = Event>) {
            self.held.lock().unwrap().extend(events);
        }
    }

    async fn serve(tcp: tokio::net::TcpStream, url: RelayUrl, modes: Arc<Modes>, held: Arc<Mutex<Vec<Event>>>) {
        tokio::time::sleep(std::time::Duration::from_millis(modes.stall_ms.load(Ordering::SeqCst))).await;
        let greet = modes.greets.load(Ordering::SeqCst);
        let Ok(ws) = tokio_tungstenite::accept_async(tcp).await else { return };
        let (mut sink, mut stream) = ws.split();
        static CONNECTION: AtomicUsize = AtomicUsize::new(0);
        let challenge = format!("challenge-{}", CONNECTION.fetch_add(1, Ordering::SeqCst));
        let (mut challenged, mut authed) = (greet, HashSet::<PublicKey>::new());
        let mut open = HashSet::<SubscriptionId>::new();
        if greet && sink.send(Ws::Text(RelayMessage::auth(challenge.clone()).as_json().into())).await.is_err() {
            return;
        }
        while let Some(Ok(frame)) = stream.next().await {
            let Ws::Text(text) = frame else { continue };
            let mut out: Vec<RelayMessage> = Vec::new();
            match ClientMessage::from_json(text.as_str()) {
                Ok(ClientMessage::Req { .. }) if modes.hangs_up.load(Ordering::SeqCst) => return,
                Ok(ClientMessage::Req { subscription_id, filters }) => {
                    let id = subscription_id.into_owned();
                    let named = filters.iter().flat_map(|f| f.authors.iter().flatten().copied());
                    let cap = modes.sub_cap.load(Ordering::SeqCst);
                    if modes.refuses.load(Ordering::SeqCst) {
                        out.push(RelayMessage::closed(id, "error: refused"));
                    } else if cap > 0 && open.len() >= cap {
                        out.push(RelayMessage::closed(id, "error: too many subscriptions"));
                    } else if modes.gated.load(Ordering::SeqCst) && !named.into_iter().all(|a| authed.contains(&a)) {
                        if !challenged && !modes.mute.load(Ordering::SeqCst) {
                            challenged = true;
                            out.push(RelayMessage::auth(challenge.clone()));
                        }
                        out.push(match modes.quiet.load(Ordering::SeqCst) {
                            true => RelayMessage::eose(id),
                            false => RelayMessage::closed(id, "auth-required: all authors must be authenticated"),
                        });
                    } else {
                        open.insert(id.clone());
                        let held = held.lock().unwrap();
                        for e in held.iter().filter(|e| filters.iter().any(|f| f.match_event(e, MatchEventOptions::new()))) {
                            out.push(RelayMessage::event(id.clone(), e.clone()));
                        }
                        out.push(RelayMessage::eose(id));
                        if !challenged && modes.challenges_after.load(Ordering::SeqCst) {
                            challenged = true;
                            out.push(RelayMessage::auth(challenge.clone()));
                        }
                    }
                }
                Ok(ClientMessage::Close(id)) => {
                    open.remove(id.as_ref());
                }
                Ok(ClientMessage::Auth(event)) => {
                    let ok = nostr::nips::nip42::is_valid_auth_event(&event, &url, &challenge);
                    if ok {
                        authed.insert(event.pubkey);
                    }
                    out.push(RelayMessage::ok(event.id, ok, if ok { "" } else { "invalid: auth" }));
                }
                Ok(ClientMessage::Event(event)) => {
                    out.push(RelayMessage::ok(event.id, true, ""));
                    held.lock().unwrap().push(event.into_owned());
                }
                _ => {}
            }
            for m in out {
                if sink.send(Ws::Text(m.as_json().into())).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// A [`Transport`] that fails on purpose.
///
/// `MemoryRelay` always succeeds: no failed fetch, no short page, no reordering,
/// no relay that serves a different subset than its siblings. Every strand and
/// fork this codebase has shipped lives in exactly the space it cannot express,
/// which is a large part of why none of them were caught by a test. This wraps it
/// and injects the failures a real relay set produces.
///
/// Deterministic by construction — the only randomness is a seeded counter, so a
/// failing scenario reproduces exactly from its seed.
#[cfg(test)]
pub(crate) mod adversarial {
    // A fault-injection toolkit: each scenario uses the rules it needs, and the rest
    // of the surface waits for the next scenario rather than being trimmed to fit.
    #![allow(dead_code)]

    use super::memory::MemoryRelay;
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    /// What a fetch should do instead of succeeding.
    #[derive(Clone, Debug)]
    pub enum Fault {
        /// Return `Err` — a dead relay, an AUTH gate, a timeout.
        FailFetch(String),
        /// Succeed, but serve at most `n` events — the "short page reads as the end
        /// of the plane" hazard.
        ShortPage(usize),
        /// Succeed, but serve nothing at all — a relay silently withholding.
        Empty,
        /// Serve at most `n` events from the relays that answered, while another
        /// stayed silent: a short page that is NOT the end of the plane.
        ShortUnconfirmed(usize),
    }

    /// Which fetches a rule applies to. Matched against the query, so a plan can
    /// target one plane (an author) without knowing call order.
    #[derive(Clone, Debug, Default)]
    pub struct Match {
        /// Any of these authors appears in the query.
        pub author: Option<String>,
        /// Only the Nth matching call (1-based); `None` = every one.
        pub nth: Option<u64>,
        /// Only from this call onward (1-based).
        pub from_nth: Option<u64>,
    }

    impl Match {
        pub fn author(hex: &str) -> Self {
            Match { author: Some(hex.to_string()), ..Default::default() }
        }
        pub fn any() -> Self {
            Match::default()
        }
        pub fn nth(mut self, n: u64) -> Self {
            self.nth = Some(n);
            self
        }
        pub fn from_nth(mut self, n: u64) -> Self {
            self.from_nth = Some(n);
            self
        }
    }

    struct Rule {
        m: Match,
        fault: Fault,
        hits: AtomicU64,
    }

    pub struct AdversarialRelay {
        inner: MemoryRelay,
        rules: Mutex<Vec<Rule>>,
        /// Every fetch, whether or not a rule matched — the round-trip counter a
        /// scale test asserts on ("~2 per hop, not ~97").
        fetches: AtomicU64,
    }

    impl AdversarialRelay {
        pub fn new() -> Self {
            AdversarialRelay { inner: MemoryRelay::new(), rules: Mutex::new(Vec::new()), fetches: AtomicU64::new(0) }
        }

        /// The underlying honest relay — publish fixtures through this so a fault
        /// plan governs only what the client can READ back.
        pub fn relay(&self) -> &MemoryRelay {
            &self.inner
        }

        pub fn on(&self, m: Match, fault: Fault) -> &Self {
            self.rules.lock().unwrap_or_else(|e| e.into_inner()).push(Rule { m, fault, hits: AtomicU64::new(0) });
            self
        }

        /// Drop every rule — "the faults cease", the precondition of the
        /// convergence property every scenario asserts.
        pub fn heal(&self) {
            self.rules.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }

        pub fn fetch_count(&self) -> u64 {
            self.fetches.load(Ordering::Relaxed)
        }

        pub fn reset_fetch_count(&self) {
            self.fetches.store(0, Ordering::Relaxed);
        }

        fn verdict(&self, query: &Query) -> Option<Fault> {
            let rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
            for r in rules.iter() {
                if let Some(a) = &r.m.author {
                    if !query.authors.iter().any(|q| q == a) {
                        continue;
                    }
                }
                let n = r.hits.fetch_add(1, Ordering::Relaxed) + 1;
                if r.m.nth.is_some_and(|want| want != n) {
                    continue;
                }
                if r.m.from_nth.is_some_and(|first| n < first) {
                    continue;
                }
                return Some(r.fault.clone());
            }
            None
        }

        /// The served events, and whether every relay answered.
        async fn governed(&self, query: &Query, relays: &[String], plane: Option<&Keys>) -> Result<(Vec<Event>, bool), String> {
            self.fetches.fetch_add(1, Ordering::Relaxed);
            let short = match self.verdict(query) {
                Some(Fault::FailFetch(why)) => return Err(why),
                Some(Fault::Empty) => return Ok((Vec::new(), true)),
                Some(Fault::ShortPage(n)) => Some((n, true)),
                Some(Fault::ShortUnconfirmed(n)) => Some((n, false)),
                None => None,
            };
            let mut out = match plane {
                Some(p) => self.inner.fetch_plane(p, query, relays).await?.events,
                None => self.inner.fetch(query, relays).await?,
            };
            let Some((n, complete)) = short else { return Ok((out, true)) };
            out.truncate(n);
            Ok((out, complete))
        }
    }

    #[async_trait::async_trait]
    impl Transport for AdversarialRelay {
        async fn publish(&self, event: &Event, relays: &[String]) -> Result<(), String> {
            self.inner.publish(event, relays).await
        }
        async fn publish_durable(&self, event: &Event, relays: &[String]) -> Result<(), String> {
            self.inner.publish_durable(event, relays).await
        }
        async fn fetch(&self, query: &Query, relays: &[String]) -> Result<Vec<Event>, String> {
            self.governed(query, relays, None).await.map(|(events, _)| events)
        }
        async fn fetch_plane(&self, plane: &Keys, query: &Query, relays: &[String]) -> Result<PlaneFetch, String> {
            // `Empty` answers with nothing (coverage intact, so absence is real)
            // while `FailFetch` propagates an Err (nobody answered, so absence is
            // unproven) — the exact distinction the coverage field carries.
            let (events, complete) = self.governed(query, relays, Some(plane)).await?;
            Ok(if complete { PlaneFetch::whole(events) } else { PlaneFetch { events, answered: 1, attempted: 2 } })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Plane read coverage ───────────────────────────────────────────────────

    fn urls(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("wss://r{i}")).collect()
    }

    #[test]
    fn a_plane_read_that_skipped_a_tripped_relay_does_not_claim_full_coverage() {
        // A skipped relay was never asked, so whatever it holds is unseen: the read
        // may RETURN early, but it must not report every relay as having answered.
        let sel = plane_targets(urls(3), |r| r == "wss://r1");
        assert_eq!(sel.ask, vec!["wss://r0".to_string(), "wss://r2".to_string()], "the tripped relay is spared");
        let read = sel.fetched(Vec::new(), 2, sel.ask.len());
        assert_eq!((read.answered, read.attempted), (2, 3), "the skipped relay stays in the denominator");
        assert!(!read.is_complete(), "an empty answer from the asked relays is not proof of absence");
    }

    #[test]
    fn a_plane_read_with_nothing_tripped_or_everything_tripped_asks_everyone() {
        let none = plane_targets(urls(3), |_| false);
        assert_eq!((none.ask.len(), none.skipped), (3, 0));
        assert!(none.fetched(Vec::new(), 3, 3).is_complete(), "every relay asked and answered IS full coverage");
        // Skipping every relay would ask nobody: ask them all rather than return nothing.
        let all = plane_targets(urls(3), |_| true);
        assert_eq!((all.ask.len(), all.skipped), (3, 0));
        assert!(all.fetched(Vec::new(), 3, 3).is_complete());
    }

    #[test]
    fn a_dead_socket_leaves_the_denominator_but_a_tripped_relay_does_not() {
        // A dead socket can't answer this call at all, so it was never reachable;
        // a tripped relay merely answered badly lately and may hold the event.
        let sel = plane_targets(urls(4), |r| r == "wss://r3");
        let after_dead_sockets = sel.ask.len() - 1;
        let read = sel.fetched(Vec::new(), after_dead_sockets, after_dead_sockets);
        assert_eq!(read.attempted, 3, "asked minus the dead socket, plus the tripped relay");
        assert!(!read.is_complete());
    }

    // ── Plane pool ────────────────────────────────────────────────────────────

    /// Polls until `cond` holds: the sweep runs on real time, so wait for it rather than guess.
    async fn eventually(cond: impl Fn() -> bool) -> bool {
        for _ in 0..100 {
            if cond() {
                return true;
            }
            tokio::time::sleep(PLANE_POOL_IDLE_TTL / 4).await;
        }
        cond()
    }

    #[tokio::test]
    async fn an_idle_plane_connection_closes_without_another_fetch() {
        // Its own pool: the global one is drained by any swap elsewhere in the suite.
        let pool: &'static PlanePool = Box::leak(Box::default());
        let in_pool = |key: &str| pool.lock().1.contains_key(key);
        let (kept, idle) = ("plane-kept|wss://a", "plane-idle|wss://a");
        pool.insert(0, idle.into(), crate::nostr_client_builder().build());
        pool.insert(0, kept.into(), crate::nostr_client_builder().build());
        assert!(in_pool(idle) && in_pool(kept));

        // One stays in use (stamped ahead, so no sleep can let it lapse): only the idle one
        // goes, and nothing fetched to notice it.
        pool.lock().1.get_mut(kept).unwrap().last_used = web_time::Instant::now() + std::time::Duration::from_secs(60);
        assert!(eventually(|| !in_pool(idle)).await, "the idle connection was swept");
        assert!(in_pool(kept), "the one in use stays warm");

        pool.lock().1.get_mut(kept).unwrap().last_used = web_time::Instant::now();
        assert!(eventually(|| !in_pool(kept)).await, "and goes once it idles too");
        let disarmed = || !pool.sweep_armed.load(std::sync::atomic::Ordering::Acquire);
        assert!(eventually(disarmed).await, "an empty pool runs no sweep");
    }

    // ── Plane connections by relay gate ───────────────────────────────────────

    /// A plane key and the one wrap it authored, held by `relay`.
    fn plane_on(relay: &wire::WireRelay) -> (Keys, Event) {
        use nostr_sdk::prelude::FinalizeEvent;
        let plane = Keys::generate();
        let wrap = EventBuilder::new(Kind::Custom(crate::community::v2::stream::KIND_WRAP), "sealed").finalize(&plane).unwrap();
        relay.hold([wrap.clone()]);
        (plane, wrap)
    }

    async fn read_plane(relay: &wire::WireRelay, plane: &Keys) -> PlaneFetch {
        read_plane_at(relay.url(), plane.clone()).await
    }

    async fn read_plane_at(url: String, plane: Keys) -> PlaneFetch {
        let query = Query {
            kinds: vec![crate::community::v2::stream::KIND_WRAP],
            authors: vec![plane.public_key().to_hex()],
            ..Default::default()
        };
        LiveTransport::with_timeout(std::time::Duration::from_secs(5)).fetch_plane(&plane, &query, &[url]).await.unwrap()
    }

    fn gate_of(relay: &wire::WireRelay) -> Option<RelayGate> {
        with_plane_relay(crate::db::current_session_id(), &relay.url(), |r| r.gate)
    }

    fn authed_pooled(plane: &Keys) -> bool {
        let pk = plane.public_key().to_hex();
        PLANE_POOL.lock().1.keys().any(|k| k.starts_with(&pk))
    }

    fn open_pooled(relay: &wire::WireRelay) -> bool {
        OPEN_PLANES.lock().1.contains_key(&relay.url())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn a_relay_that_never_challenges_serves_every_plane_over_one_connection() {
        let _s = GateScene::new();
        let relay = wire::WireRelay::start().await;
        let planes: Vec<(Keys, Event)> = (0..5).map(|_| plane_on(&relay)).collect();
        for (plane, wrap) in &planes {
            let read = read_plane(&relay, plane).await;
            assert_eq!(read.events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![wrap.id]);
            assert!(read.is_complete());
            assert!(!authed_pooled(plane), "no connection of its own");
        }
        assert_eq!(relay.opened(), 1, "five planes, one connection");
        assert_eq!(gate_of(&relay), Some(RelayGate::Open));
        assert!(open_pooled(&relay));
        clear_plane_pool();
        assert!(!open_pooled(&relay), "a swap closes it with the authed ones");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn reads_that_reach_a_new_relay_together_dial_it_once() {
        let _s = GateScene::new();
        let relay = wire::WireRelay::start().await;
        let planes: Vec<(Keys, Event)> = (0..8).map(|_| plane_on(&relay)).collect();
        let reads = planes.iter().map(|(plane, _)| tokio::spawn(read_plane_at(relay.url(), plane.clone())));
        for read in futures_util::future::join_all(reads).await {
            assert_eq!(read.unwrap().events.len(), 1);
        }
        assert_eq!(relay.opened(), 1);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn on_one_thread_a_challenge_inside_a_read_is_seen_by_that_read() {
        let _s = GateScene::new();
        let relay = wire::WireRelay::start().await;
        relay.gate(true);
        relay.quiet(true);
        let (plane, wrap) = plane_on(&relay);
        let read = read_plane(&relay, &plane).await;
        assert_eq!(read.events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![wrap.id]);
        assert!(authed_pooled(&plane) && !open_pooled(&relay));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn auth_required_without_a_challenge_is_still_a_gate() {
        let _s = GateScene::new();
        let relay = wire::WireRelay::start().await;
        relay.gate(true);
        relay.mute(true);
        let (plane, _) = plane_on(&relay);
        let query = Query { kinds: vec![crate::community::v2::stream::KIND_WRAP], authors: vec![plane.public_key().to_hex()], ..Default::default() };
        let read = LiveTransport::with_timeout(std::time::Duration::from_secs(2)).fetch_plane(&plane, &query, &[relay.url()]).await;
        assert!(read.is_err(), "nothing to authenticate against, so nothing served");
        assert_eq!(gate_of(&relay), Some(RelayGate::Auth));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn a_challenge_between_reads_moves_the_next_read_to_an_authed_connection() {
        let _s = GateScene::new();
        let relay = wire::WireRelay::start().await;
        relay.challenge_after_a_read(true);
        let (first, _) = plane_on(&relay);
        read_plane(&relay, &first).await;
        assert!(eventually(|| gate_of(&relay) == Some(RelayGate::Auth)).await, "the socket's watcher saw it");
        let (second, _) = plane_on(&relay);
        read_plane(&relay, &second).await;
        assert!(authed_pooled(&second));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn a_socket_that_closes_under_a_read_ends_it_at_once() {
        let _s = GateScene::new();
        let relay = wire::WireRelay::start().await;
        relay.hang_up(true);
        let plane = Keys::generate();
        let query = Query { kinds: vec![crate::community::v2::stream::KIND_WRAP], authors: vec![plane.public_key().to_hex()], ..Default::default() };
        let started = web_time::Instant::now();
        let read = LiveTransport::with_timeout(std::time::Duration::from_secs(5)).fetch_plane(&plane, &query, &[relay.url()]).await;
        assert!(read.is_err(), "nothing answered");
        assert!(started.elapsed() < std::time::Duration::from_secs(2), "not left to its deadline: {:?}", started.elapsed());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn reads_sharing_one_connection_stay_under_its_subscription_cap() {
        let _s = GateScene::new();
        let relay = wire::WireRelay::start().await;
        relay.cap_subs(16);
        let planes: Vec<(Keys, Event)> = (0..40).map(|_| plane_on(&relay)).collect();
        let reads = planes.iter().map(|(plane, _)| tokio::spawn(read_plane_at(relay.url(), plane.clone())));
        for read in futures_util::future::join_all(reads).await {
            assert_eq!(read.unwrap().events.len(), 1);
        }
        assert_eq!(relay.opened(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn a_relay_that_cannot_be_reached_is_not_redialled_for_every_read() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let _s = GateScene::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let dials = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = dials.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                counted.fetch_add(1, SeqCst);
                drop(tcp);
            }
        });
        let plane = Keys::generate();
        let query = Query { kinds: vec![crate::community::v2::stream::KIND_WRAP], authors: vec![plane.public_key().to_hex()], ..Default::default() };
        let live = LiveTransport::with_timeout(std::time::Duration::from_secs(2));
        for _ in 0..3 {
            assert!(live.fetch_plane(&plane, &query, std::slice::from_ref(&url)).await.is_err(), "nothing answered");
        }
        assert_eq!(dials.load(SeqCst), 1, "one dial inside the redial gap");
        clear_plane_pool();
        assert!(live.fetch_plane(&plane, &query, std::slice::from_ref(&url)).await.is_err());
        assert_eq!(dials.load(SeqCst), 2, "a new network dials at once");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn a_relay_that_asks_for_auth_reads_each_plane_on_a_connection_authed_as_it() {
        let _s = GateScene::new();
        let relay = wire::WireRelay::start().await;
        relay.gate(true);
        let planes: Vec<(Keys, Event)> = (0..3).map(|_| plane_on(&relay)).collect();
        for (plane, wrap) in &planes {
            let read = read_plane(&relay, plane).await;
            assert_eq!(read.events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![wrap.id], "served once authed as the plane");
            assert!(authed_pooled(plane));
        }
        assert_eq!(gate_of(&relay), Some(RelayGate::Auth));
        assert!(!open_pooled(&relay), "the open connection closed at the first challenge");
        assert_eq!(relay.opened(), 1 + planes.len(), "the read that met the gate, then one per plane");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn a_relay_that_starts_challenging_moves_its_planes_to_authed_connections() {
        let _s = GateScene::new();
        let relay = wire::WireRelay::start().await;
        let (first, _) = plane_on(&relay);
        read_plane(&relay, &first).await;
        assert_eq!((gate_of(&relay), relay.opened()), (Some(RelayGate::Open), 1));

        relay.gate(true);
        let (second, wrap) = plane_on(&relay);
        let read = read_plane(&relay, &second).await;
        assert_eq!(read.events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![wrap.id], "the same read, from an authed connection");
        assert_eq!(gate_of(&relay), Some(RelayGate::Auth));
        assert!(authed_pooled(&second) && !open_pooled(&relay));

        let (third, _) = plane_on(&relay);
        read_plane(&relay, &third).await;
        assert_eq!(relay.opened(), 3, "no read goes back to the open connection");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn a_relay_that_challenges_then_answers_with_silence_still_serves_the_plane_once_authed() {
        let _s = GateScene::new();
        let relay = wire::WireRelay::start().await;
        relay.gate(true);
        relay.quiet(true);
        let (plane, wrap) = plane_on(&relay);
        let read = read_plane(&relay, &plane).await;
        assert_eq!(read.events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![wrap.id], "the silence was no answer");
        assert_eq!(gate_of(&relay), Some(RelayGate::Auth));
        assert!(authed_pooled(&plane));
    }

    /// A port that refuses every connection.
    async fn refusing_url() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        format!("ws://127.0.0.1:{port}")
    }

    async fn read_across(urls: &[String], plane: &Keys, timeout: std::time::Duration) -> PlaneFetch {
        let query = Query { kinds: vec![crate::community::v2::stream::KIND_WRAP], authors: vec![plane.public_key().to_hex()], ..Default::default() };
        LiveTransport::with_timeout(timeout).fetch_plane(plane, &query, urls).await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn a_relay_never_reached_leaves_the_read_of_the_others_complete() {
        let _s = GateScene::new();
        let relay = wire::WireRelay::start().await;
        let (plane, _) = plane_on(&relay);
        let read = read_across(&[relay.url(), refusing_url().await], &plane, std::time::Duration::from_secs(2)).await;
        assert_eq!((read.answered, read.attempted), (1, 1));
        assert!(read.is_complete());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn a_relay_that_refuses_the_read_keeps_it_incomplete() {
        let _s = GateScene::new();
        let (answers, refuses) = (wire::WireRelay::start().await, wire::WireRelay::start().await);
        refuses.refuse(true);
        let (plane, _) = plane_on(&answers);
        let read = read_across(&[answers.url(), refuses.url()], &plane, std::time::Duration::from_secs(2)).await;
        assert_eq!((read.answered, read.attempted), (1, 2));
        assert!(!read.is_complete());
        assert_eq!(gate_of(&refuses), None, "a refusal is no gate");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn a_relay_still_connecting_is_read_not_dropped() {
        let _s = GateScene::new();
        let (quick, slow) = (wire::WireRelay::start().await, wire::WireRelay::start().await);
        slow.stall(std::time::Duration::from_millis(1500));
        let (plane, wrap) = plane_on(&slow);
        let read = read_across(&[quick.url(), slow.url()], &plane, std::time::Duration::from_secs(1)).await;
        assert_eq!(read.events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![wrap.id], "the slow relay held it");
        assert_eq!((read.answered, read.attempted), (2, 2));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn a_gating_relay_that_is_down_leaves_the_denominator_when_an_open_one_answered() {
        let _s = GateScene::new();
        let relay = wire::WireRelay::start().await;
        let down = refusing_url().await;
        mark_auth(crate::db::current_session_id(), &crate::inbox_relays::normalize_relay_url(&down));
        let (plane, wrap) = plane_on(&relay);
        let read = read_across(&[relay.url(), down], &plane, std::time::Duration::from_secs(2)).await;
        assert_eq!(read.events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![wrap.id]);
        assert_eq!((read.answered, read.attempted), (1, 1));
    }

    #[tokio::test]
    async fn a_previous_sessions_caller_leaves_the_pool_alone() {
        let pool: &'static PlanePool = Box::leak(Box::default());
        let build = || crate::nostr_client_builder().build();
        let (_, hold, _) = pool.take_or_insert(7, "wss://a", build).unwrap();
        let id = hold.id;
        assert!(pool.take_or_insert(6, "wss://a", build).is_none(), "an earlier session builds nothing");
        assert_eq!(pool.take(6, "wss://a").0.map(|_| ()), None);
        assert_eq!(pool.lock().0, 7);
        assert_eq!(pool.lock().1.get("wss://a").map(|p| p.id), Some(id), "and wipes nothing");
        assert!(pool.remove(7, "wss://a", id + 1).is_none(), "a replacement is not removed for its predecessor");
    }

    #[tokio::test]
    async fn a_held_open_connection_outlives_the_idle_clock_and_goes_once_released() {
        let pool: &'static PlanePool = Box::leak(Box::default());
        let (_, hold, _) = pool.take_or_insert(1, "wss://a", || crate::nostr_client_builder().build()).unwrap();
        tokio::time::sleep(PLANE_POOL_IDLE_TTL * 3).await;
        assert!(pool.lock().1.contains_key("wss://a"), "a dial or read in flight keeps it");
        drop(hold);
        assert!(eventually(|| !pool.lock().1.contains_key("wss://a")).await, "and it idles out after");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn a_challenge_on_connect_is_a_gate_even_when_reads_are_served() {
        let _s = GateScene::new();
        let relay = wire::WireRelay::start().await;
        relay.greet(true);
        let (plane, wrap) = plane_on(&relay);
        let read = read_plane(&relay, &plane).await;
        assert_eq!(read.events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![wrap.id]);
        assert_eq!(gate_of(&relay), Some(RelayGate::Auth));
        assert!(authed_pooled(&plane) && !open_pooled(&relay));
    }

    // ── Dead-socket target filtering ──────────────────────────────────────────

    #[test]
    fn a_dead_socket_is_skipped_but_a_connecting_or_unknown_one_is_not() {
        let targets: Vec<String> = ["wss://dead", "wss://connecting", "wss://unknown"].map(String::from).into();
        let out = drop_unrevivable(targets, |r| r == "wss://dead");
        assert_eq!(out, ["wss://connecting", "wss://unknown"].map(String::from).to_vec());
    }

    #[test]
    fn an_all_dead_set_falls_back_to_the_full_list() {
        // Offline (or a status race) must keep the honest-timeout error paths —
        // an instant empty-target "success" would read as a confident empty.
        let targets: Vec<String> = ["wss://a", "wss://b"].map(String::from).into();
        let out = drop_unrevivable(targets.clone(), |_| true);
        assert_eq!(out, targets);
    }

    // ── Ready gate (community publish while the chosen network still connects) ──
    // The live gate, `wait_transport_ready`, over the real transport state: a network installed
    // by hand stands in for Tor, so no live Tor is needed.

    /// A network that is up or not as the test says, and dials nothing.
    struct Gate(std::sync::atomic::AtomicBool);

    #[async_trait::async_trait]
    impl crate::transport::Transport for Gate {
        fn kind(&self) -> crate::transport::Kind {
            crate::transport::Kind::Tor
        }
        fn route(&self, dest: &crate::transport::Dest, port: u16, ctx: &crate::transport::RouteCtx) -> crate::transport::Route {
            crate::transport::route::route_tor(dest, port, ctx)
        }
        async fn dial(
            &self,
            _: &crate::transport::Route,
            _: crate::transport::Lane,
        ) -> Result<(crate::transport::BoxedStream, crate::transport::Dialed), crate::transport::ConnectError> {
            Err(crate::transport::ConnectError::Unreachable("gate".into()))
        }
        fn ready(&self) -> bool {
            self.0.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn kind_status(&self) -> crate::transport::status::KindStatus {
            crate::transport::status::KindStatus::ready(serde_json::Value::Null)
        }
        async fn new_identity(&self) {}
        async fn shutdown(&self) {}
        fn into_any(self: std::sync::Arc<Self>) -> std::sync::Arc<dyn std::any::Any + Send + Sync> {
            self
        }
    }

    /// Holds the process-wide network for one test and puts Clearnet back however it ends.
    struct GateScene(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    impl GateScene {
        fn new() -> Self {
            let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
            drop(crate::transport::host::uninstall());
            GateScene(guard)
        }
    }

    impl Drop for GateScene {
        fn drop(&mut self) {
            drop(crate::transport::host::uninstall());
            crate::transport::budget::clear_overrides_for_test();
            crate::transport::set_preference(Some(crate::transport::Kind::Clearnet));
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn ready_gate_passes_at_once_when_nothing_holds_traffic() {
        let _s = GateScene::new();
        crate::transport::set_preference(Some(crate::transport::Kind::Clearnet));
        let start = web_time::Instant::now();
        assert!(wait_transport_ready().await.is_ok());
        crate::transport::set_preference(Some(crate::transport::Kind::Tor));
        let gate = std::sync::Arc::new(Gate(std::sync::atomic::AtomicBool::new(true)));
        crate::transport::host::activate(gate, crate::db::live_session_id(), false).unwrap();
        assert!(wait_transport_ready().await.is_ok());
        assert!(start.elapsed() < std::time::Duration::from_millis(500), "must not wait while ready");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn ready_gate_errors_honestly_after_its_window() {
        // Without the gate a send failed at once with a misleading "no relay accepted"; with
        // it, it waits the window and then names the cause.
        let _s = GateScene::new();
        crate::transport::budget::override_for_test(crate::transport::Op::ReadyWait, Some(std::time::Duration::from_millis(300)));
        crate::transport::set_preference(Some(crate::transport::Kind::Tor));
        let gate = std::sync::Arc::new(Gate(std::sync::atomic::AtomicBool::new(false)));
        crate::transport::host::activate(gate, crate::db::live_session_id(), false).unwrap();
        let start = web_time::Instant::now();
        let err = wait_transport_ready().await.expect_err("Tor never comes up");
        assert_eq!(err, "Tor is still connecting. Wait a moment and try again.");
        let took = start.elapsed();
        assert!(took >= std::time::Duration::from_millis(300) && took < std::time::Duration::from_millis(1500), "{took:?}");

        // A network this build lacks never comes: no wait, and its own words.
        if !crate::transport::Kind::Tor.compiled() {
            drop(crate::transport::host::uninstall());
            let start = web_time::Instant::now();
            assert_eq!(wait_transport_ready().await.unwrap_err(), "This build doesn't include Tor.");
            assert!(start.elapsed() < std::time::Duration::from_millis(100));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::await_holding_lock)]
    async fn ready_gate_passes_once_the_network_comes_up_mid_wait() {
        let _s = GateScene::new();
        crate::transport::budget::override_for_test(crate::transport::Op::ReadyWait, Some(std::time::Duration::from_secs(10)));
        crate::transport::set_preference(Some(crate::transport::Kind::Tor));
        let gate = std::sync::Arc::new(Gate(std::sync::atomic::AtomicBool::new(false)));
        let inst = crate::transport::host::activate(gate.clone(), crate::db::live_session_id(), false).unwrap();
        let id = inst.id();
        let up = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            gate.0.store(true, std::sync::atomic::Ordering::SeqCst);
            crate::transport::host::notify(crate::transport::host::TransportEvent::Ready {
                kind: crate::transport::Kind::Tor,
                instance: id,
                owner: crate::db::live_session_id(),
                recovered: false,
            });
        });
        let start = web_time::Instant::now();
        assert!(wait_transport_ready().await.is_ok(), "passes once the network is up");
        assert!(start.elapsed() < std::time::Duration::from_secs(2), "woken by the change, not the window");
        up.await.unwrap();
    }

    fn evt(kind: u16, z: &str) -> Event {
        EventBuilder::new(Kind::Custom(kind), "x")
            .tags([Tag::custom(
                "z",
                [z.to_string()],
            )])
            .finalize(&Keys::generate())
            .unwrap()
    }

    #[test]
    fn query_matches_kind_and_z() {
        let e = evt(3300, "abc");
        assert!(Query { kinds: vec![3300], z_tags: vec!["abc".into()], since: None, ..Default::default() }.matches(&e));
        assert!(!Query { kinds: vec![3301], ..Default::default() }.matches(&e));
        assert!(!Query { kinds: vec![], z_tags: vec!["xyz".into()], since: None, ..Default::default() }.matches(&e));
        assert!(Query::default().matches(&e), "empty query matches anything");
    }

    fn evt_at(kind: u16, secs: u64) -> Event {
        EventBuilder::new(Kind::Custom(kind), "x")
            .custom_created_at(Timestamp::from(secs))
            .finalize(&Keys::generate())
            .unwrap()
    }

    /// Build a z-tagged event at a controlled created_at (for deterministic paging tests —
    /// the real outer events carry wall-clock send time).
    fn evt_z_at(kind: u16, z: &str, secs: u64) -> Event {
        EventBuilder::new(Kind::Custom(kind), "x")
            .custom_created_at(Timestamp::from(secs))
            .tags([Tag::custom(
                "z",
                [z.to_string()],
            )])
            .finalize(&Keys::generate())
            .unwrap()
    }

    #[tokio::test]
    async fn fetch_pages_with_until_and_limit_newest_first() {
        // The Discord-style paging mechanism: `limit` caps newest-first; `until` walks older.
        let relay = super::memory::MemoryRelay::new();
        let relays = vec!["r1".to_string()];
        for s in 1..=5u64 {
            relay.inject(&evt_z_at(3300, "pg", s), &relays);
        }
        let secs = |evs: &[Event]| evs.iter().map(|e| e.created_at.as_secs()).collect::<Vec<_>>();

        // Latest page: the two newest (secs 5, 4), newest-first.
        let latest = relay
            .fetch(&Query { kinds: vec![3300], z_tags: vec!["pg".into()], limit: Some(2), ..Default::default() }, &relays)
            .await
            .unwrap();
        assert_eq!(secs(&latest), vec![5, 4]);

        // Older page before the cursor (until=3, inclusive): secs 3, 2.
        let older = relay
            .fetch(&Query { kinds: vec![3300], z_tags: vec!["pg".into()], until: Some(3), limit: Some(2), ..Default::default() }, &relays)
            .await
            .unwrap();
        assert_eq!(secs(&older), vec![3, 2]);

        // Start of history: until=1 returns only the single oldest event — the signal the
        // caller uses to mark "no more older" and stop hitting the network.
        let start = relay
            .fetch(&Query { kinds: vec![3300], z_tags: vec!["pg".into()], until: Some(1), limit: Some(2), ..Default::default() }, &relays)
            .await
            .unwrap();
        assert_eq!(secs(&start), vec![1]);
    }

    #[test]
    fn to_filter_translates_kinds_z_and_since() {
        // The live/test-parity claim rests on to_filter matching matches(); assert
        // the Filter directly. An event the Query matches must also pass the Filter.
        let q = Query { kinds: vec![3300], z_tags: vec!["abc".into()], since: Some(100), ..Default::default() };
        let filter = q.to_filter();
        let matching = EventBuilder::new(Kind::Custom(3300), "x")
            .custom_created_at(Timestamp::from(150))
            .tags([Tag::custom(
                "z",
                ["abc".to_string()],
            )])
            .finalize(&Keys::generate())
            .unwrap();
        assert!(filter.match_event(&matching, MatchEventOptions::new()), "to_filter must accept what matches() accepts");
        assert!(q.matches(&matching));

        // Wrong kind, wrong z, and too-early all rejected by the same Filter.
        let wrong_kind = EventBuilder::new(Kind::Custom(3301), "x")
            .custom_created_at(Timestamp::from(150))
            .tags([Tag::custom(
                "z",
                ["abc".to_string()],
            )])
            .finalize(&Keys::generate())
            .unwrap();
        assert!(!filter.match_event(&wrong_kind, MatchEventOptions::new()));
    }

    /// Build an event carrying one arbitrary single-letter tag (recipient `p`, wrapped-kind `k`).
    fn evt_sl(kind: u16, letter: SingleLetterTag, value: &str) -> Event {
        EventBuilder::new(Kind::Custom(kind), "x")
            .tags([Tag::custom(
                letter.as_char().to_string(),
                [value.to_string()],
            )])
            .finalize(&Keys::generate())
            .unwrap()
    }

    #[test]
    fn to_filter_and_matches_agree_on_authors() {
        let keys = Keys::generate();
        let e = EventBuilder::new(Kind::Custom(1059), "x").finalize(&keys).unwrap();
        let q = Query { kinds: vec![1059], authors: vec![keys.public_key().to_hex()], ..Default::default() };
        assert!(q.matches(&e));
        assert!(q.to_filter().match_event(&e, MatchEventOptions::new()));
        let miss = Query { kinds: vec![1059], authors: vec![Keys::generate().public_key().to_hex()], ..Default::default() };
        assert!(!miss.matches(&e));
        assert!(!miss.to_filter().match_event(&e, MatchEventOptions::new()));
    }

    #[test]
    fn to_filter_and_matches_agree_on_p_tags() {
        let recipient = Keys::generate().public_key().to_hex();
        let e = evt_sl(1059, SingleLetterTag::LOWERCASE_P, &recipient);
        let q = Query { kinds: vec![1059], p_tags: vec![recipient], ..Default::default() };
        assert!(q.matches(&e));
        assert!(q.to_filter().match_event(&e, MatchEventOptions::new()));
        let miss = Query { kinds: vec![1059], p_tags: vec![Keys::generate().public_key().to_hex()], ..Default::default() };
        assert!(!miss.matches(&e));
        assert!(!miss.to_filter().match_event(&e, MatchEventOptions::new()));
    }

    #[test]
    fn to_filter_and_matches_agree_on_k_tags() {
        let e = evt_sl(1059, SingleLetterTag::LOWERCASE_K, "3311");
        let q = Query { kinds: vec![1059], k_tags: vec!["3311".into()], ..Default::default() };
        assert!(q.matches(&e));
        assert!(q.to_filter().match_event(&e, MatchEventOptions::new()));
        let miss = Query { kinds: vec![1059], k_tags: vec!["3300".into()], ..Default::default() };
        assert!(!miss.matches(&e));
        assert!(!miss.to_filter().match_event(&e, MatchEventOptions::new()));
    }

    #[test]
    fn to_filter_empty_kinds_only_constrains_z_and_since() {
        // Empty kinds = no kind constraint, matching matches()' behavior.
        let q = Query { kinds: vec![], z_tags: vec!["p".into()], since: None, ..Default::default() };
        let filter = q.to_filter();
        let e = evt(3300, "p");
        assert!(filter.match_event(&e, MatchEventOptions::new()));
        assert!(q.matches(&e));
    }

    #[test]
    fn since_is_an_inclusive_lower_bound() {
        let below = evt_at(3300, 99);
        let exact = evt_at(3300, 100);
        let above = evt_at(3300, 101);
        let q = Query { kinds: vec![3300], z_tags: vec![], since: Some(100), ..Default::default() };
        assert!(!q.matches(&below), "below the floor is excluded");
        assert!(q.matches(&exact), "exactly the floor is included");
        assert!(q.matches(&above), "above the floor is included");
    }

    #[test]
    fn z_tags_match_as_or_set() {
        // The whole-channel fetch lists multiple epoch pseudonyms; an event
        // tagged with any one of them must match.
        let e = evt(3300, "p2");
        let q = Query { kinds: vec![3300], z_tags: vec!["p1".into(), "p2".into()], since: None, ..Default::default() };
        assert!(q.matches(&e));
        let miss = Query { kinds: vec![3300], z_tags: vec!["p1".into(), "p3".into()], since: None, ..Default::default() };
        assert!(!miss.matches(&e));
    }

    #[tokio::test]
    async fn fetch_unions_and_dedups_across_relays() {
        use super::memory::MemoryRelay;
        let relay = MemoryRelay::new();
        let relays = vec!["r1".to_string(), "r2".to_string(), "r3".to_string()];
        let e = evt(3300, "p");
        relay.publish(&e, &relays).await.unwrap();
        let got = relay
            .fetch(&Query { kinds: vec![3300], z_tags: vec!["p".into()], since: None, ..Default::default() }, &relays)
            .await
            .unwrap();
        assert_eq!(got.len(), 1, "same event on 3 relays dedups to 1");
    }

    #[tokio::test]
    async fn durable_broadcast_retries_only_the_failing_relays_until_they_ack() {
        // r1/r3 ACK on round 1; r2 fails the first 4 rounds then ACKs. The engine must keep re-sending
        // ONLY r2 (not the already-ACKed r1/r3) until it lands, and succeed.
        use std::cell::Cell;
        let relays = vec!["r1".to_string(), "r2".to_string(), "r3".to_string()];
        let round = Cell::new(0usize);
        let r2_round_seen = Cell::new(0usize);
        let res = durable_broadcast(&relays, 30, std::time::Duration::ZERO, |pending| {
            let n = round.get();
            round.set(n + 1);
            // r2 is only ever retried alone after round 1 — assert we never re-send a relay that ACKed.
            if n >= 1 {
                assert_eq!(pending, vec!["r2".to_string()], "only the failing relay is retried");
                r2_round_seen.set(r2_round_seen.get() + 1);
            }
            Box::pin(async move {
                pending.into_iter().filter(|r| r != "r2" || n >= 4).collect()
            })
        })
        .await;
        assert!(res.is_ok(), "all relays eventually ACK → Ok");
        assert!(round.get() >= 5, "kept retrying r2 across rounds");
    }

    #[tokio::test]
    async fn durable_broadcast_is_ok_if_some_ack_even_when_one_never_does() {
        // r1 ACKs; r2 never does. After exhausting r2's retries, the event is still durably out (r1 has
        // it, fetch-union covers r2), so the result is Ok — durability is best-effort per relay.
        let relays = vec!["r1".to_string(), "r2".to_string()];
        let res = durable_broadcast(&relays, 5, std::time::Duration::ZERO, |pending| {
            Box::pin(async move { pending.into_iter().filter(|r| r == "r1").collect() })
        })
        .await;
        assert!(res.is_ok(), "≥1 relay accepted → Ok despite a permanently-failing relay");
    }

    #[tokio::test]
    async fn durable_broadcast_errs_only_if_zero_relays_ever_accept() {
        let relays = vec!["r1".to_string(), "r2".to_string()];
        let res = durable_broadcast(&relays, 5, std::time::Duration::ZERO, |_pending| {
            Box::pin(async move { Vec::new() }) // nobody ever ACKs
        })
        .await;
        assert!(res.is_err(), "zero acceptances after the retry cap → Err");
    }

    #[tokio::test]
    async fn redundancy_self_heals_a_missing_relay() {
        use super::memory::MemoryRelay;
        let relay = MemoryRelay::new();
        let all = vec!["r1".to_string(), "r2".to_string(), "r3".to_string()];
        let e = evt(3300, "p");
        // Event lands on ONLY r2 (the others "missed" it).
        relay.inject(&e, &["r2".to_string()]);
        assert_eq!(relay.count_on("r1"), 0);
        assert_eq!(relay.count_on("r2"), 1);
        // A fetch across the full set still finds it (redundancy).
        let got = relay.fetch(&Query { kinds: vec![3300], ..Default::default() }, &all).await.unwrap();
        assert_eq!(got.len(), 1);
    }

    #[tokio::test]
    async fn ephemeral_kind_streams_live_but_is_never_stored_or_fetched() {
        use super::memory::MemoryRelay;
        let relay = MemoryRelay::new();
        let relays = vec!["r1".to_string()];
        let mut sub = relay.subscribe(Query { kinds: vec![21059], ..Default::default() });
        let e = evt(21059, "p");
        relay.publish(&e, &relays).await.unwrap();
        assert_eq!(relay.count_on("r1"), 0, "ephemeral is never stored");
        let got = relay
            .fetch(&Query { kinds: vec![21059], ..Default::default() }, &relays)
            .await
            .unwrap();
        assert!(got.is_empty(), "a real relay never serves an ephemeral from a fetch");
        assert_eq!(sub.try_recv().unwrap().id, e.id, "but a live subscriber receives it");
    }

    #[tokio::test]
    async fn stored_kind_is_fetchable_and_delivered_live() {
        use super::memory::MemoryRelay;
        let relay = MemoryRelay::new();
        let relays = vec!["r1".to_string()];
        let mut sub = relay.subscribe(Query { kinds: vec![1059], ..Default::default() });
        let e = evt(1059, "p");
        relay.publish(&e, &relays).await.unwrap();
        let got = relay
            .fetch(&Query { kinds: vec![1059], ..Default::default() }, &relays)
            .await
            .unwrap();
        assert_eq!(got.len(), 1, "stored kind is fetchable");
        assert_eq!(sub.try_recv().unwrap().id, e.id, "and delivered to the live subscriber");
    }

    #[tokio::test]
    async fn p_tags_route_a_giftwrap_to_the_matching_subscriber() {
        use super::memory::MemoryRelay;
        let relay = MemoryRelay::new();
        let relays = vec!["r1".to_string()];
        let alice = Keys::generate().public_key().to_hex();
        let bob = Keys::generate().public_key().to_hex();
        let mut sub_alice =
            relay.subscribe(Query { kinds: vec![1059], p_tags: vec![alice.clone()], ..Default::default() });
        let mut sub_bob =
            relay.subscribe(Query { kinds: vec![1059], p_tags: vec![bob.clone()], ..Default::default() });
        let wrap = evt_sl(1059, SingleLetterTag::LOWERCASE_P, &alice);
        relay.publish(&wrap, &relays).await.unwrap();
        assert_eq!(sub_alice.try_recv().unwrap().id, wrap.id, "addressed recipient gets it live");
        assert!(sub_bob.try_recv().is_err(), "a differently-addressed subscriber does not");
        // Fetch agrees with the live routing.
        let for_alice = relay
            .fetch(&Query { kinds: vec![1059], p_tags: vec![alice], ..Default::default() }, &relays)
            .await
            .unwrap();
        assert_eq!(for_alice.len(), 1);
        let for_bob = relay
            .fetch(&Query { kinds: vec![1059], p_tags: vec![bob], ..Default::default() }, &relays)
            .await
            .unwrap();
        assert!(for_bob.is_empty());
    }

    // ── UnionPlan: the evidence tiers' return-timing math ────────────────────

    #[test]
    fn union_plan_fast_satisfied_on_first_success() {
        let mut p = UnionPlan::new(Evidence::Fast, 4);
        p.record(false);
        assert!(!p.satisfied(), "a failure is not evidence");
        p.record(true);
        assert!(p.satisfied(), "one genuine EOSE satisfies Fast");
        assert!(!p.exhausted());
    }

    #[test]
    fn union_plan_quorum_majority_math() {
        // (attempted, successes needed): majority = attempted/2 + 1
        for (n, need) in [(2usize, 2usize), (3, 2), (4, 3), (5, 3)] {
            let mut p = UnionPlan::new(Evidence::Quorum, n);
            for _ in 0..need - 1 {
                p.record(true);
            }
            assert!(!p.satisfied(), "{}/{} must not satisfy quorum", need - 1, n);
            p.record(true);
            assert!(p.satisfied(), "{}/{} satisfies quorum", need, n);
        }
    }

    #[test]
    fn union_plan_quorum_failures_never_substitute_for_successes() {
        let mut p = UnionPlan::new(Evidence::Quorum, 3);
        p.record(true);
        p.record(false);
        p.record(false);
        assert!(!p.satisfied(), "1 success + 2 failures is not a majority");
        assert!(p.exhausted(), "all resolved — the degraded path returns best-effort");
        assert_eq!(p.successes(), 1);
    }

    #[test]
    fn union_plan_full_requires_every_relay_resolved() {
        let mut p = UnionPlan::new(Evidence::Full, 3);
        p.record(true);
        p.record(true);
        assert!(!p.satisfied(), "Full waits for the last relay even after 2 EOSEs");
        p.record(false);
        assert!(p.satisfied(), "a timeout is a resolution — Full is done");
        assert!(p.exhausted());
    }

    #[test]
    fn union_plan_all_dead_is_reportable_not_a_confident_empty() {
        let mut p = UnionPlan::new(Evidence::Quorum, 2);
        p.record(false);
        p.record(false);
        assert!(p.exhausted());
        assert_eq!(p.successes(), 0, "the caller must map this to Err, never Ok(vec![])");
    }

    // ── Circuit breaker: trip/reset rules ────────────────────────────────────
    // Pinned generation + unique urls per test: the breaker is one global map,
    // and other tests bump the REAL session generation concurrently (which
    // would wipe it mid-assertion via the production accessors).

    const BREAKER_TEST_GEN: u64 = u64::MAX;

    #[test]
    fn breaker_trips_only_after_consecutive_full_budget_failures() {
        let url = "wss://breaker-test-full-budget.example";
        breaker_record_at(BREAKER_TEST_GEN, url, false, true);
        assert!(!breaker_tripped_at(BREAKER_TEST_GEN, url), "one failure is below the threshold");
        breaker_record_at(BREAKER_TEST_GEN, url, false, true);
        assert!(breaker_tripped_at(BREAKER_TEST_GEN, url), "two consecutive full-budget failures trip");
    }

    #[test]
    fn breaker_demoted_budget_failures_never_count() {
        let url = "wss://breaker-test-demoted.example";
        breaker_record_at(BREAKER_TEST_GEN, url, false, false);
        breaker_record_at(BREAKER_TEST_GEN, url, false, false);
        breaker_record_at(BREAKER_TEST_GEN, url, false, false);
        assert!(
            !breaker_tripped_at(BREAKER_TEST_GEN, url),
            "demoted-budget failures must not trip (anti-starvation: the post-cooldown probe must stay reachable)"
        );
    }

    #[test]
    fn breaker_success_resets_the_entry() {
        let url = "wss://breaker-test-reset.example";
        breaker_record_at(BREAKER_TEST_GEN, url, false, true);
        breaker_record_at(BREAKER_TEST_GEN, url, false, true);
        assert!(breaker_tripped_at(BREAKER_TEST_GEN, url));
        // A late EOSE (e.g. via the background drain) proves slow ≠ dead.
        breaker_record_at(BREAKER_TEST_GEN, url, true, false);
        assert!(!breaker_tripped_at(BREAKER_TEST_GEN, url), "any success unconditionally resets");
        breaker_record_at(BREAKER_TEST_GEN, url, false, true);
        assert!(!breaker_tripped_at(BREAKER_TEST_GEN, url), "and the failure count restarted from zero");
    }

    // ── The evidence floor ───────────────────────────────────────────────────

    #[test]
    fn declared_evidence_stands_and_default_is_quorum() {
        assert_eq!(Query::default().evidence, Evidence::Quorum, "unclassified sites get Quorum");
        assert_eq!(
            effective_evidence(&Query { until: Some(1), evidence: Evidence::Fast, ..Default::default() }),
            Evidence::Fast,
            "chat pagination rides its declared tier — absence verdicts request Full themselves"
        );
        assert_eq!(
            effective_evidence(&Query { evidence: Evidence::Fast, ..Default::default() }),
            Evidence::Fast,
            "without `until` the declared tier stands"
        );
    }
}
