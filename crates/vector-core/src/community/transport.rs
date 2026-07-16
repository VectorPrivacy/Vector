//! Transport abstraction for Community events.
//!
//! The protocol's send/sync logic is written against this trait, not against the
//! live Nostr client, so it can be exercised end-to-end by multiple emulated
//! clients sharing an in-memory relay (no network, fully deterministic). Production
//! provides an adapter over `NOSTR_CLIENT`; tests use [`MemoryRelay`].

use nostr_sdk::prelude::*;

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
                .custom_tags(SingleLetterTag::lowercase(Alphabet::Z), self.z_tags.clone());
        }
        if !self.d_tags.is_empty() {
            filter = filter.identifiers(self.d_tags.clone());
        }
        if !self.p_tags.is_empty() {
            filter = filter
                .custom_tags(SingleLetterTag::lowercase(Alphabet::P), self.p_tags.clone());
        }
        if !self.k_tags.is_empty() {
            filter = filter
                .custom_tags(SingleLetterTag::lowercase(Alphabet::K), self.k_tags.clone());
        }
        if !self.authors.is_empty() {
            let authors: Vec<PublicKey> =
                self.authors.iter().filter_map(|a| PublicKey::from_hex(a).ok()).collect();
            if !authors.is_empty() {
                filter = filter.authors(authors);
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
    async fn fetch_plane(&self, plane: &Keys, query: &Query, relays: &[String]) -> Result<Vec<Event>, String>;

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
            tokio::time::sleep(backoff).await;
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

/// Per-relay failure tracker behind the fetch circuit breaker. Generation-keyed
/// like [`WARMED_RELAYS`] (an account swap invalidates every entry). A trip is
/// driven ONLY by consecutive failures at the relay's FULL timeout budget:
/// failures at a demoted budget never count (a slow-but-honest relay must be
/// able to recover via the post-cooldown full-budget probe), and a late EOSE
/// surfacing in the background drain resets the entry (slow ≠ dead).
#[derive(Default)]
struct BreakerEntry {
    consecutive_failures: u8,
    tripped_until: Option<std::time::Instant>,
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
        guard.1.clear();
    }
    f(&mut guard.1)
}

/// Is `url` inside a trip cooldown right now?
fn breaker_tripped(url: &str) -> bool {
    breaker_tripped_at(crate::state::current_session_generation(), url)
}

fn breaker_tripped_at(generation: u64, url: &str) -> bool {
    with_breaker_at(generation, |map| {
        map.get(url)
            .and_then(|e| e.tripped_until)
            .map_or(false, |t| std::time::Instant::now() < t)
    })
}

/// Record a per-relay fetch outcome. Success resets the entry; a failure counts
/// toward a trip only when the relay had its full timeout budget.
fn breaker_record(url: &str, success: bool, full_budget: bool) {
    breaker_record_at(crate::state::current_session_generation(), url, success, full_budget)
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
            e.tripped_until = Some(std::time::Instant::now() + BREAKER_COOLDOWN);
        }
    })
}

/// `until` forces Full — a back-page verdict (the history-start latch) trusts
/// "nothing older than the cursor" only against the completest union the
/// reachable relay set allows. A floor in the transport, not trust in callers.
fn effective_evidence(query: &Query) -> Evidence {
    if query.until.is_some() {
        Evidence::Full
    } else {
        query.evidence
    }
}

// ── Plane connection pool (fetch_plane) ─────────────────────────────────────
// A plane fetch on an AUTH-gating relay needs a connection authed AS the plane
// key. Re-connecting + re-NIP-42-authing on EVERY page/epoch dominates TTFB on
// slow relays, so keep the authed connection warm and reuse it. Keyed by (plane
// pubkey, relay set). Generation-scoped: an account swap holds account A's plane
// SECRET keys, so the pool MUST clear (also freed by `clear_plane_pool`).

struct PooledPlane {
    client: Client,
    last_used: std::time::Instant,
}

static PLANE_POOL: std::sync::LazyLock<std::sync::Mutex<(u64, std::collections::HashMap<String, PooledPlane>)>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new((0, std::collections::HashMap::new())));

/// A pooled connection unused for this long is closed on the next sweep — long
/// enough to span a community's whole backfill, short enough not to hoard sockets.
const PLANE_POOL_IDLE_TTL: std::time::Duration = std::time::Duration::from_secs(90);
/// Hard cap on simultaneously-pooled plane connections (LRU-evicted).
const PLANE_POOL_MAX: usize = 24;

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
fn disconnect_clients(clients: Vec<Client>) {
    for c in clients {
        tokio::spawn(async move {
            let _ = c.disconnect();
        });
    }
}

/// Close + drop every pooled plane connection. Call on account swap — the pooled
/// clients are authenticated as the swapped-out account's community plane keys.
pub fn clear_plane_pool() {
    let drained: Vec<Client> = {
        let mut g = PLANE_POOL.lock().unwrap_or_else(|e| e.into_inner());
        g.1.drain().map(|(_, p)| p.client).collect()
    };
    disconnect_clients(drained);
}

/// Take a warm pooled client for `key` if one is fresh; also drops entries whose
/// idle TTL expired and resets the whole pool if the session generation advanced
/// (a swap). Returns `(Some(client_if_hit), clients_to_disconnect)`.
fn plane_pool_take(generation: u64, key: &str) -> (Option<Client>, Vec<Client>) {
    let mut g = PLANE_POOL.lock().unwrap_or_else(|e| e.into_inner());
    let mut evicted: Vec<Client> = Vec::new();
    if g.0 != generation {
        evicted.extend(g.1.drain().map(|(_, p)| p.client));
        g.0 = generation;
    }
    // Sweep idle-expired entries.
    let now = std::time::Instant::now();
    let expired: Vec<String> = g.1.iter()
        .filter(|(_, p)| now.duration_since(p.last_used) >= PLANE_POOL_IDLE_TTL)
        .map(|(k, _)| k.clone())
        .collect();
    for k in expired {
        if let Some(p) = g.1.remove(&k) {
            evicted.push(p.client);
        }
    }
    let hit = g.1.get_mut(key).map(|p| {
        p.last_used = now;
        p.client.clone()
    });
    (hit, evicted)
}

/// Insert a freshly-built client for `key`, LRU-evicting if over the cap. Returns
/// clients to disconnect (a raced sibling insert, or the LRU victim).
fn plane_pool_insert(generation: u64, key: String, client: Client) -> Vec<Client> {
    let mut g = PLANE_POOL.lock().unwrap_or_else(|e| e.into_inner());
    if g.0 != generation {
        // Swapped mid-build — don't pool into the new generation; caller still uses it once.
        return vec![client];
    }
    let mut evicted: Vec<Client> = Vec::new();
    // A concurrent miss for the same key already inserted — keep theirs, drop ours.
    if g.1.contains_key(&key) {
        return vec![client];
    }
    if g.1.len() >= PLANE_POOL_MAX {
        if let Some(lru_key) = g.1.iter().min_by_key(|(_, p)| p.last_used).map(|(k, _)| k.clone()) {
            if let Some(p) = g.1.remove(&lru_key) {
                evicted.push(p.client);
            }
        }
    }
    g.1.insert(key, PooledPlane { client, last_used: std::time::Instant::now() });
    evicted
}

/// Whether Full-drain timeout demotion may apply. Under Tor EVERY relay is
/// legitimately slow — a first congested pass must not cascade into pool-wide
/// starvation, so demotion is disabled entirely.
fn demotion_allowed() -> bool {
    #[cfg(feature = "tor")]
    {
        matches!(crate::tor::transport_state(), crate::tor::TorTransportState::Disabled)
    }
    #[cfg(not(feature = "tor"))]
    {
        true
    }
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
    let relay = client.pool().relay(url).await.map_err(|_| ())?;
    // Subscribe to notifications BEFORE the REQ so the EOSE can't slip past.
    let mut notifications = relay.notifications();
    let sub_id = SubscriptionId::generate();
    let auto_close = SubscribeAutoCloseOptions::default()
        .exit_policy(ReqExitPolicy::ExitOnEOSE)
        .timeout(Some(timeout));
    relay
        .subscribe_with_id(sub_id.clone(), filter, SubscribeOptions::default().close_on(Some(auto_close)))
        .await
        .map_err(|_| ())?;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut events: Vec<Event> = Vec::new();
    let mut seen: std::collections::HashSet<EventId> = std::collections::HashSet::new();
    loop {
        let notification = match tokio::time::timeout_at(deadline, notifications.recv()).await {
            Ok(Ok(n)) => n,
            // Lagged: the broadcast skipped messages under a flood — keep
            // draining; a missed EOSE degrades to the deadline (a failure,
            // never a false success).
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => return Err(()),
            Err(_) => return Err(()), // deadline: timeout is NOT EOSE
        };
        match notification {
            RelayNotification::Event { subscription_id, event } if subscription_id == sub_id => {
                if seen.insert(event.id) {
                    events.push(*event);
                }
            }
            RelayNotification::Message { message } => match message {
                RelayMessage::Event { subscription_id, event } if *subscription_id == sub_id => {
                    if seen.insert(event.id) {
                        events.push(event.into_owned());
                    }
                }
                RelayMessage::EndOfStoredEvents(id) if *id == sub_id => return Ok(events),
                RelayMessage::Closed { subscription_id, .. } if *subscription_id == sub_id => {
                    return Err(()); // relay refused the REQ (incl. auth-required)
                }
                _ => {}
            },
            RelayNotification::Shutdown => return Err(()),
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
            Evidence::Quorum => self.successes >= (self.attempted / 2) + 1,
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

/// Max time a community network op holds while Tor is enabled-but-not-yet-
/// bootstrapped. Generous enough for a circuit to land on a normal connection,
/// bounded so a Tor that never comes up can't hang the op forever.
#[cfg(feature = "tor")]
const TOR_READY_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Poll `is_blocked` until it clears or `max_wait` elapses.
///
/// When Tor is enabled but its SOCKS proxy isn't up yet, `transport_state()` is
/// `RequiredButInactive` and every relay is routed to the blackhole proxy, so a
/// send fails at the TCP layer INSTANTLY — surfacing as a misleading "no relay
/// accepted the event" with zero network wait. Holding here turns that into
/// either success (once the circuit lands) or an honest "Tor is still
/// connecting" error. Generic over the predicate so it is testable without a
/// live Tor.
#[allow(dead_code)]
async fn wait_until_tor_ready<F: Fn() -> bool>(
    is_blocked: F,
    max_wait: std::time::Duration,
) -> Result<(), String> {
    if !is_blocked() {
        return Ok(());
    }
    let deadline = std::time::Instant::now() + max_wait;
    while is_blocked() {
        if std::time::Instant::now() >= deadline {
            return Err("Tor is still connecting. Wait a moment and try again.".to_string());
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    Ok(())
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

    let mut still_needed: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(ids) = crate::db::community::list_community_ids() {
        for id in ids {
            if let Ok(Some(c)) = crate::db::community::load_community(&id) {
                for r in &c.relays {
                    still_needed.insert(r.clone());
                }
            }
        }
    }

    let pool = client.pool();
    // all_relays(): community relays carry GOSSIP, so they're absent from `relays()` (READ/WRITE only).
    let pooled = pool.all_relays().await;
    for url in candidates {
        if still_needed.contains(url) {
            continue;
        }
        if let Ok(parsed) = nostr_sdk::RelayUrl::parse(url) {
            if let Some(relay) = pooled.get(&parsed) {
                if relay.flags().has_read() || relay.flags().has_write() {
                    continue; // a real chat relay (or an overlap) — never sever
                }
            }
            let _ = pool.force_remove_relay(parsed).await; // plain remove_relay refuses GOSSIP
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
        Self { timeout: std::time::Duration::from_secs(10) }
    }
}

impl LiveTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_timeout(timeout: std::time::Duration) -> Self {
        Self { timeout }
    }

    /// Grab the app's persistent client and make sure it's connected to `relays` — the Community's relays
    /// are the user's own relays, so this is almost always a pure no-op (already in the warm pool). A relay
    /// the pool doesn't hold yet is added idempotently (mirrors what the realtime subscription does), then
    /// `connect()` kicks it without disturbing the already-connected majority. Never shut this client down:
    /// it is shared. Errors only if there is no client yet or every relay url was invalid.
    async fn warm_client(relays: &[String], connect_timeout: std::time::Duration) -> Result<Client, String> {
        if relays.is_empty() {
            return Err("community has no relays configured".to_string());
        }
        // Tor gate — runs BEFORE the warmed-cache fast path so a relay warmed
        // before Tor was toggled on still waits. While Tor is enabled but not yet
        // bootstrapped, every relay points at the blackhole proxy and a send
        // fails instantly; hold for the circuit, then fail honestly if it never
        // comes up (see `wait_until_tor_ready`).
        #[cfg(feature = "tor")]
        wait_until_tor_ready(
            || matches!(crate::tor::transport_state(), crate::tor::TorTransportState::RequiredButInactive),
            TOR_READY_WAIT,
        ).await?;
        let client = crate::state::nostr_client().ok_or_else(|| "nostr client not initialized".to_string())?;

        // Fast path: every one of these relays was already warmed this session → the pool holds and
        // (auto-)maintains them, so skip the redundant add_relay + connect churn that otherwise runs on
        // EVERY fetch/publish. Account swaps bump the generation, dropping the cache.
        let generation = crate::state::current_session_generation();
        {
            let warmed = WARMED_RELAYS.lock().unwrap_or_else(|e| e.into_inner());
            if warmed.0 == generation && relays.iter().all(|r| warmed.1.contains(r)) {
                return Ok(client);
            }
        }

        // `add_relay` returns Ok(true) if NEWLY added, Ok(false) if the pool already held it.
        // Community relays join GOSSIP|PING (see `community_relay_options`) so they stay 24/7 warm
        // without pulling the user's DM/profile traffic onto relays they don't own. An overlap
        // relay already in the pool as a user relay keeps its READ+WRITE flags (add_relay no-ops).
        let mut added_new = false;
        let mut succeeded: Vec<&String> = Vec::new();
        for url in relays {
            let opts = crate::community_relay_options();
            match client.pool().add_relay(url.as_str(), opts).await {
                Ok(true) => { added_new = true; succeeded.push(url); }
                Ok(false) => { succeeded.push(url); }
                Err(_) => {}
            }
        }
        if succeeded.is_empty() {
            return Err("no valid community relays could be added".to_string());
        }
        if added_new {
            // A relay we weren't already connected to (a Community on non-default relays). `connect()`
            // returns before sockets are up, so WAIT for it — otherwise the immediate fetch/send reaches
            // zero relays. Already-connected relays return instantly in `success`, so the warm majority
            // adds no latency; only the genuinely-new relay's handshake is awaited (bounded).
            let _ = client.try_connect(connect_timeout).await;
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
                warmed.1.clear();
            }
            for url in succeeded {
                warmed.1.insert(url.clone());
            }
        }
        Ok(client)
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
                tokio::spawn(async move {
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
        let mut quorum_deadline: Option<tokio::time::Instant> = None;
        let mut quorum_window_closed = false;
        while !plan.satisfied() && !plan.exhausted() {
            let next = match quorum_deadline {
                Some(deadline) => match tokio::time::timeout_at(deadline, fetches.next()).await {
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
                            tokio::time::Instant::now()
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
            let grace = tokio::time::sleep(std::time::Duration::from_millis(RESIDUAL_GRACE_MS));
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
            let session = crate::state::SessionGuard::capture();
            tokio::spawn(async move {
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
                if !session.is_valid() {
                    return;
                }
                submit_stragglers(extra);
            });
        }

        Ok((result, plan.successes(), plan.attempted()))
    }
}

#[async_trait::async_trait]
impl Transport for LiveTransport {
    async fn publish(&self, event: &Event, relays: &[String]) -> Result<(), String> {
        let client = Self::warm_client(relays, self.timeout).await?;
        let timeout = self.timeout;
        let mut targets: Vec<String> = Vec::new();
        for r in relays { if !targets.contains(r) { targets.push(r.clone()); } }
        // Fan out one send per relay and RETURN on the first ACK — never wait for the slowest relay (a
        // distant/ratelimited one must not gate a reaction/edit/message). Each send is SPAWNED, so the rest
        // keep delivering to every relay after we return (dropping a JoinHandle detaches, it doesn't abort).
        // Single attempt — durable retry is publish_durable's job. The sends only touch relays, no per-account
        // state, so no SessionGuard is needed.
        use futures_util::stream::{FuturesUnordered, StreamExt};
        let mut sends: FuturesUnordered<_> = targets
            .into_iter()
            .map(|r| {
                let client = client.clone();
                let event = event.clone();
                tokio::spawn(async move {
                    matches!(
                        tokio::time::timeout(timeout, client.send_event_to(vec![r.clone()], &event)).await,
                        Ok(Ok(out)) if RelayUrl::parse(&r).map(|u| out.success.contains(&u)).unwrap_or(false)
                    )
                })
            })
            .collect();
        while let Some(joined) = sends.next().await {
            if matches!(joined, Ok(true)) {
                return Ok(()); // first relay ACKed; the others keep delivering in the background
            }
        }
        Err("no relay accepted the event".to_string())
    }

    async fn fetch(&self, query: &Query, relays: &[String]) -> Result<Vec<Event>, String> {
        self.fetch_counted(query, relays).await.map(|(events, _successes, _attempted)| events)
    }

    async fn fetch_plane(&self, plane: &Keys, query: &Query, relays: &[String]) -> Result<Vec<Event>, String> {
        if relays.is_empty() {
            return Ok(Vec::new());
        }
        #[cfg(feature = "tor")]
        wait_until_tor_ready(
            || matches!(crate::tor::transport_state(), crate::tor::TorTransportState::RequiredButInactive),
            TOR_READY_WAIT,
        ).await?;
        // Skip relays the shared breaker already knows are dead — don't even pay
        // their connect handshake + warmup + timeout (the v1 sweep / DM negentropy
        // trip them early, so by the time a v2 backfill runs they're usually
        // marked). Keep all if that would leave none — a slow fetch beats no fetch.
        let mut targets: Vec<String> = relays.iter().filter(|r| !breaker_tripped(r)).cloned().collect();
        if targets.is_empty() {
            targets = relays.to_vec();
        }
        let filter = query.to_filter();
        let generation = crate::state::current_session_generation();
        let key = plane_pool_key(&plane.public_key().to_hex(), &targets);

        // Reuse a warm, already-authed pooled connection if one exists — this is
        // the win: the NIP-42 handshake happens ONCE, not per page/epoch.
        let (hit, evicted) = plane_pool_take(generation, &key);
        disconnect_clients(evicted);
        let client = if let Some(c) = hit {
            c
        } else {
            // Cold: a dedicated connection authed AS the plane key (a NIP-42
            // connection holds ONE identity; the shared client's is the user's).
            let opts = crate::nostr_client_options().automatic_authentication(true);
            let client = nostr_sdk::Client::builder().signer(plane.clone()).opts(opts).build();
            for r in &targets {
                let _ = client.add_relay(r.clone()).await;
            }
            client.connect().await;
            // Warmup with the gated filter shape triggers each relay's NIP-42
            // challenge so auto-auth completes ONCE here; pooled reuses skip it.
            for r in &targets {
                let _ = client
                    .fetch_events_from(vec![r.clone()], filter.clone(), std::time::Duration::from_secs(5))
                    .await;
            }
            let ev = plane_pool_insert(generation, key, client.clone());
            disconnect_clients(ev);
            client
        };

        let mut result: Vec<Event> = Vec::new();
        let mut seen: std::collections::HashSet<EventId> = std::collections::HashSet::new();
        for r in &targets {
            let res = fetch_relay_eose(&client, r, filter.clone(), self.timeout).await;
            // Feed the shared breaker so this auth path both benefits from AND
            // contributes to the pool-wide dead-relay knowledge.
            breaker_record(r, res.is_ok(), true);
            if let Ok(events) = res {
                for e in events {
                    if seen.insert(e.id) {
                        result.push(e);
                    }
                }
            }
        }
        // The client stays POOLED (not disconnected) for the next page/epoch/community.
        Ok(result)
    }

    async fn publish_durable(&self, event: &Event, relays: &[String]) -> Result<(), String> {
        // "Durable" = confirm the network has it (≥1 relay ACKs within CONFIRM_WINDOW), then keep bugging the
        // SLOW/ratelimited stragglers (Damus-style 1-event/min) in the BACKGROUND. If NOTHING ACKs in the
        // window we throw — a dead relay set is a failure, not an endless wait. Uses the shared warm client
        // (NEVER shut down here) and targets the community relay set across rounds.
        let client = Self::warm_client(relays, self.timeout).await?;
        let timeout = self.timeout;
        let event = event.clone();
        let backoff = std::time::Duration::from_millis(750);
        let mut pending: Vec<String> = Vec::new();
        for r in relays { if !pending.contains(r) { pending.push(r.clone()); } }
        if pending.is_empty() {
            return Err("no relays to broadcast to".to_string());
        }

        // Phase 1 — CONFIRM: RACE the relays and return the instant ANY one ACKs — never wait for the
        // slowest. `send_event_to(all)` blocks on the slowest relay (a distant/ratelimited one dominates the
        // latency), so we fan out one send per relay and take the first winner; the losers are cancelled and
        // re-sent in the background. Retry rounds within CONFIRM_WINDOW; zero ACKs in the window = failure.
        let mut acked_any = false;
        let _ = tokio::time::timeout(CONFIRM_WINDOW, async {
            loop {
                let sends = pending.iter().cloned().map(|r| {
                    let client = &client;
                    let event = &event;
                    Box::pin(async move {
                        match tokio::time::timeout(timeout, client.send_event_to(vec![r.clone()], event)).await {
                            Ok(Ok(out)) if RelayUrl::parse(&r).map(|u| out.success.contains(&u)).unwrap_or(false) => Ok(r),
                            _ => Err(()),
                        }
                    })
                });
                if let Ok((winner, _losers)) = futures_util::future::select_ok(sends).await {
                    acked_any = true;
                    pending.retain(|r| r != &winner);
                    break;
                }
                tokio::time::sleep(backoff).await;
            }
        })
        .await;

        if !acked_any {
            return Err(format!("no relay accepted the event within {}s", CONFIRM_WINDOW.as_secs()));
        }
        if pending.is_empty() {
            return Ok(()); // every relay ACKed during the confirm phase
        }

        // Phase 2 — BACKGROUND: thread the laggards through a durable publisher (retries each up to
        // MAX_PUBLISH_ATTEMPTS at a 750ms backoff, so it can't run forever). The caller returns NOW with its
        // confirmed ACK; 's fetch-union heals anything that never lands. The client is shared — not torn
        // down — so the spawned task just drops its handle when finished.
        tokio::spawn(async move {
            let client_ref = &client;
            let event_ref = &event;
            let _ = durable_broadcast(&pending, MAX_PUBLISH_ATTEMPTS, backoff, move |round| {
                Box::pin(async move {
                    match tokio::time::timeout(timeout, client_ref.send_event_to(round.clone(), event_ref)).await {
                        Ok(Ok(output)) => round.into_iter().filter(|p| RelayUrl::parse(p).map(|u| output.success.contains(&u)).unwrap_or(false)).collect(),
                        _ => Vec::new(),
                    }
                })
            })
            .await;
        });
        Ok(())
    }
}

#[cfg(test)]
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
            }
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
            // makes a revocation tombstone overwrite a bundle, and a fresh 13302 supersede the last one,
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
        /// events — same rule strfry enforces).
        fn apply_deletion(&self, deletion: &Event, relays: &[String]) {
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
                if let Some(events) = map.get_mut(r) {
                    events.retain(|e| {
                        if e.pubkey != deletion.pubkey {
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

        async fn fetch_plane(&self, _plane: &Keys, query: &Query, relays: &[String]) -> Result<Vec<Event>, String> {
            // No auth in the in-memory relay — a plane fetch is just a fetch.
            self.fetch(query, relays).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Tor gate (community publish over a not-yet-bootstrapped Tor) ──────────
    // Hermetic: drives `wait_until_tor_ready` with an injected predicate, so no
    // live Tor is needed and the result is deterministic.

    #[tokio::test]
    async fn tor_gate_passes_immediately_when_not_blocked() {
        let start = std::time::Instant::now();
        let res = wait_until_tor_ready(|| false, std::time::Duration::from_secs(30)).await;
        assert!(res.is_ok());
        assert!(start.elapsed() < std::time::Duration::from_secs(1), "must not wait when Tor is ready");
    }

    #[tokio::test]
    async fn tor_gate_errors_honestly_after_timeout_when_perpetually_blocked() {
        // The bug: without this gate the send failed INSTANTLY with a misleading
        // "no relay accepted". Now it waits the window, then names the real cause.
        let res = wait_until_tor_ready(|| true, std::time::Duration::from_millis(300)).await;
        let err = res.expect_err("should error when Tor never activates");
        assert!(err.to_lowercase().contains("tor"), "error must name Tor, got: {err}");
    }

    #[tokio::test]
    async fn tor_gate_passes_once_circuit_comes_up_mid_wait() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        // Blocked for the first 3 polls, then Tor becomes ready.
        let res = wait_until_tor_ready(
            || calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 3,
            std::time::Duration::from_secs(5),
        ).await;
        assert!(res.is_ok(), "should succeed once Tor activates within the window");
    }

    fn evt(kind: u16, z: &str) -> Event {
        EventBuilder::new(Kind::Custom(kind), "x")
            .tags([Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Z)),
                [z.to_string()],
            )])
            .sign_with_keys(&Keys::generate())
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
            .sign_with_keys(&Keys::generate())
            .unwrap()
    }

    /// Build a z-tagged event at a controlled created_at (for deterministic paging tests —
    /// the real outer events carry wall-clock send time).
    fn evt_z_at(kind: u16, z: &str, secs: u64) -> Event {
        EventBuilder::new(Kind::Custom(kind), "x")
            .custom_created_at(Timestamp::from(secs))
            .tags([Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Z)),
                [z.to_string()],
            )])
            .sign_with_keys(&Keys::generate())
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
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Z)),
                ["abc".to_string()],
            )])
            .sign_with_keys(&Keys::generate())
            .unwrap();
        assert!(filter.match_event(&matching, MatchEventOptions::new()), "to_filter must accept what matches() accepts");
        assert!(q.matches(&matching));

        // Wrong kind, wrong z, and too-early all rejected by the same Filter.
        let wrong_kind = EventBuilder::new(Kind::Custom(3301), "x")
            .custom_created_at(Timestamp::from(150))
            .tags([Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Z)),
                ["abc".to_string()],
            )])
            .sign_with_keys(&Keys::generate())
            .unwrap();
        assert!(!filter.match_event(&wrong_kind, MatchEventOptions::new()));
    }

    /// Build an event carrying one arbitrary single-letter tag (recipient `p`, wrapped-kind `k`).
    fn evt_sl(kind: u16, letter: Alphabet, value: &str) -> Event {
        EventBuilder::new(Kind::Custom(kind), "x")
            .tags([Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(letter)),
                [value.to_string()],
            )])
            .sign_with_keys(&Keys::generate())
            .unwrap()
    }

    #[test]
    fn to_filter_and_matches_agree_on_authors() {
        let keys = Keys::generate();
        let e = EventBuilder::new(Kind::Custom(1059), "x").sign_with_keys(&keys).unwrap();
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
        let e = evt_sl(1059, Alphabet::P, &recipient);
        let q = Query { kinds: vec![1059], p_tags: vec![recipient], ..Default::default() };
        assert!(q.matches(&e));
        assert!(q.to_filter().match_event(&e, MatchEventOptions::new()));
        let miss = Query { kinds: vec![1059], p_tags: vec![Keys::generate().public_key().to_hex()], ..Default::default() };
        assert!(!miss.matches(&e));
        assert!(!miss.to_filter().match_event(&e, MatchEventOptions::new()));
    }

    #[test]
    fn to_filter_and_matches_agree_on_k_tags() {
        let e = evt_sl(1059, Alphabet::K, "3311");
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
        let wrap = evt_sl(1059, Alphabet::P, &alice);
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
    fn until_forces_full_evidence_and_default_is_quorum() {
        assert_eq!(Query::default().evidence, Evidence::Quorum, "unclassified sites get Quorum");
        assert_eq!(
            effective_evidence(&Query { until: Some(1), evidence: Evidence::Fast, ..Default::default() }),
            Evidence::Full,
            "a back-page can never ride Fast — the history-start latch needs the full union"
        );
        assert_eq!(
            effective_evidence(&Query { evidence: Evidence::Fast, ..Default::default() }),
            Evidence::Fast,
            "without `until` the declared tier stands"
        );
    }
}
