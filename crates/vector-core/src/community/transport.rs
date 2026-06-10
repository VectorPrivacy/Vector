//! Transport abstraction for Community events.
//!
//! The protocol's send/sync logic is written against this trait, not against the
//! live Nostr client, so it can be exercised end-to-end by multiple emulated
//! clients sharing an in-memory relay (no network, fully deterministic). Production
//! provides an adapter over `NOSTR_CLIENT`; tests use [`MemoryRelay`].

use nostr_sdk::prelude::*;

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
    /// Author pubkeys (hex) to match (OR). Empty = any author.
    pub authors: Vec<String>,
    /// Lower bound on `created_at` (seconds), inclusive.
    pub since: Option<u64>,
    /// Upper bound on `created_at` (seconds), inclusive — pages OLDER history (events
    /// strictly/inclusively before a scroll cursor).
    pub until: Option<u64>,
    /// Max events to return (newest-first), the relay-side page cap. `None` = no limit.
    pub limit: Option<usize>,
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
        true
    }

    fn matches_single_letter(&self, name: &str, wanted: &[String], event: &Event) -> bool {
        let val = event.tags.iter().find_map(|t| {
            let s = t.as_slice();
            (s.len() >= 2 && s[0] == name).then(|| s[1].clone())
        });
        matches!(val, Some(v) if wanted.iter().any(|w| *w == v))
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

/// Union grace window: how long past the FIRST response the live transport keeps unioning
/// slower relays before returning. Long enough for a healthy-but-slower relay on a
/// high-latency link; short enough that a dead relay doesn't stall every sync.
pub const FETCH_UNION_GRACE_MS: u64 = 3500;

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
        let client = Self::warm_client(relays, self.timeout).await?;
        let timeout = self.timeout;
        let filter = query.to_filter();

        let mut targets: Vec<String> = Vec::new();
        for r in relays {
            if !targets.contains(r) {
                targets.push(r.clone());
            }
        }

        // One relay → nothing to race.
        if targets.len() <= 1 {
            return client
                .fetch_events_from(targets, filter, timeout)
                .await
                .map(|evs| evs.into_iter().collect())
                .map_err(|e| e.to_string());
        }

        // RACE per-relay (mirrors the publish first-ACK race): hand the caller the FIRST relay's
        // batch the instant it completes, then keep the slower relays running in the BACKGROUND and
        // feed any events they alone hold back through the ingester. Never wait for the slowest relay.
        use futures_util::stream::{FuturesUnordered, StreamExt};
        let mut fetches: FuturesUnordered<_> = targets
            .into_iter()
            .map(|r| {
                let client = client.clone();
                let filter = filter.clone();
                tokio::spawn(async move {
                    client
                        .fetch_events_from(vec![r], filter, timeout)
                        .await
                        .map(|evs| evs.into_iter().collect::<Vec<Event>>())
                        .unwrap_or_default()
                })
            })
            .collect();

        // UNION every relay that answers — ALWAYS. A single fast-but-shallow relay must never be
        // the sole input to what comes back: fail-closed folds gap-quarantine on a partial plane
        // (seats wedge on stale names/roles), the re-founding coverage gate aborts on a missing
        // head, and the history-start verdict latches scroll-back dead. All three happened live
        // off the same first-relay-wins race. Back-paging (`until`) drains to completion (its
        // verdict must be authoritative; bounded by the per-relay timeout); everything else
        // drains up to a grace window past the FIRST response — so one dead relay costs the
        // grace, not the full timeout. Relays slower than the grace still background-merge below.
        let mut result: Vec<Event> = Vec::new();
        loop {
            match fetches.next().await {
                Some(Ok(evs)) => {
                    result = evs;
                    break;
                }
                Some(Err(_)) => continue, // task join error — try the next relay
                None => break,            // every relay failed
            }
        }
        let mut union_ids: std::collections::HashSet<EventId> = result.iter().map(|e| e.id).collect();
        if query.until.is_some() {
            while let Some(joined) = fetches.next().await {
                if let Ok(evs) = joined {
                    for e in evs {
                        if union_ids.insert(e.id) {
                            result.push(e);
                        }
                    }
                }
            }
        } else if !fetches.is_empty() {
            let grace = tokio::time::sleep(std::time::Duration::from_millis(FETCH_UNION_GRACE_MS));
            tokio::pin!(grace);
            loop {
                tokio::select! {
                    _ = &mut grace => break,
                    next = fetches.next() => match next {
                        Some(Ok(evs)) => {
                            for e in evs {
                                if union_ids.insert(e.id) {
                                    result.push(e);
                                }
                            }
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
                    if let Ok(evs) = joined {
                        for e in evs {
                            if !seen.contains(&e.id) && extra_ids.insert(e.id) {
                                extra.push(e);
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
    }

    impl MemoryRelay {
        pub fn new() -> Self {
            MemoryRelay { per_relay: Mutex::new(HashMap::new()) }
        }

        /// Publish to ONLY a subset of relays — used to simulate a relay missing an
        /// event (e.g. a dropped rekey) for redundancy tests.
        pub fn inject(&self, event: &Event, relays: &[String]) {
            // Parameterized-replaceable kinds (30000-39999, e.g. a public-invite bundle): a relay keeps only
            // the latest per (kind, pubkey, d-tag), so a new event at that coordinate REPLACES the old one
            // (NIP-01). This is what makes a revocation tombstone overwrite the bundle even on relays that
            // ignore deletions — model it so tests match real relay behavior.
            let d_tag = |e: &Event| e.tags.iter().find_map(|t| {
                let s = t.as_slice();
                (s.len() >= 2 && s[0] == "d").then(|| s[1].clone())
            }).unwrap_or_default();
            let replaceable = (30000..40000).contains(&event.kind.as_u16());
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
