//! Live subscriptions that outlast a relay closing them, and catch-ups for the gaps they leave.
//!
//! nostr-sdk forgets a subscription a relay CLOSEs for any reason but `auth-required` or
//! `rate-limited`, so neither a reconnect's re-send nor an unchanged refresh would ask that relay
//! again: the connection stays healthy while the feed is dead. Every live subscription is kept here
//! with the filters each relay was sent, and is restored wherever a relay dropped it; the time it
//! was down is then caught up from that relay.
//!
//! [`revive`] is the return from being away (sleep, App Nap, a backgrounded app, a hidden window,
//! a network change): every relay proves it is alive or is redialled, every live subscription is
//! re-armed, and the platform's [`CatchUp`] handler fetches what was missed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use futures_util::StreamExt;
use nostr_sdk::prelude::*;

/// Relay → the filters it was sent, per live subscription id.
type Targets = HashMap<RelayUrl, Vec<Filter>>;

/// What a catch-up must cover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatchUp {
    /// One relay's live feed was down: what it holds since.
    Relay(String),
    /// Back from being away this long: everything, from every relay.
    All { away_ms: u64 },
}

static HANDLER: OnceLock<Box<dyn Fn(CatchUp) + Send + Sync>> = OnceLock::new();

/// The platform's catch-up: DMs and Communities for the live account. Set once at startup.
pub fn set_catch_up(handler: impl Fn(CatchUp) + Send + Sync + 'static) {
    let _ = HANDLER.set(Box::new(handler));
}

fn catch_up(what: CatchUp) {
    match HANDLER.get() {
        Some(handler) => handler(what),
        None => crate::log_debug!("[live] no catch-up handler for {what:?}"),
    }
}

/// How often every kept subscription is checked against what each relay still holds.
const HEAL_EVERY: Duration = Duration::from_secs(120);
/// First wait before re-sending a closed subscription; doubles per close, up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// A subscription quiet this long since its last close starts its backoff over.
const BACKOFF_RESET_MS: u64 = 10 * 60 * 1000;
/// How long a close waits for its subscription to be kept, before judging it one of ours.
const KEEP_GRACE: Duration = Duration::from_millis(500);
/// Relay catch-ups requested within this window run as one.
const GAP_SETTLE: Duration = Duration::from_secs(3);

/// Every kept subscription, with the session id it belongs to. Keyed by id rather than held on
/// the session object: a boot task and a later command can resolve the same account to two
/// session objects, and a forget must reach what the keep stored.
static KEPT: std::sync::LazyLock<Mutex<HashMap<SubscriptionId, (u64, Targets)>>> = std::sync::LazyLock::new(Default::default);

/// Sessions already watched, by id.
static WATCHING: std::sync::LazyLock<Mutex<std::collections::HashSet<u64>>> = std::sync::LazyLock::new(Default::default);

/// Per relay and subscription: closes in a row, and when the last one came (ms). Ids are random,
/// so one map serves every session.
static CLOSES: std::sync::LazyLock<Mutex<HashMap<(RelayUrl, SubscriptionId), (u32, u64)>>> =
    std::sync::LazyLock::new(Default::default);

#[derive(Default)]
struct Gaps(Mutex<Vec<String>>, AtomicBool);

fn backoff(closes: u32) -> Duration {
    FIRST_BACKOFF.saturating_mul(1u32 << closes.min(9)).min(MAX_BACKOFF)
}

/// Whether a restore after a recent close is still waiting out its backoff.
fn backing_off(relay: &RelayUrl, id: &SubscriptionId) -> bool {
    let closes = CLOSES.lock().unwrap_or_else(|e| e.into_inner());
    closes
        .get(&(relay.clone(), id.clone()))
        .is_some_and(|&(n, last)| now_ms().saturating_sub(last) < backoff(n.saturating_sub(1)).as_millis() as u64)
}

/// Keep `id` live on each relay with the filters it was sent there.
pub fn keep(id: &SubscriptionId, targets: Targets) {
    if targets.is_empty() {
        return;
    }
    let session = crate::db::current_session_id();
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    // A previous account's subscriptions went with its client.
    kept.retain(|_, (s, _)| *s == session);
    kept.insert(id.clone(), (session, targets));
}

/// [`keep`] for a pool-wide subscribe: every relay it reached or failed on, with `filters`.
pub fn keep_output<S, F>(out: &Output<SubscriptionId, S, F>, filters: Vec<Filter>) {
    let targets = out.success.keys().chain(out.failed.keys()).map(|url| (url.clone(), filters.clone())).collect();
    keep(&out.value, targets);
}

/// Stop keeping `id`: it was unsubscribed or replaced.
pub fn forget(id: &SubscriptionId) {
    KEPT.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
}

fn kept_on(id: &SubscriptionId, relay: &RelayUrl) -> Option<Vec<Filter>> {
    let session = crate::db::current_session_id();
    let kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    kept.get(id).filter(|(s, _)| *s == session).and_then(|(_, t)| t.get(relay).cloned())
}

fn now_ms() -> u64 {
    // device-clock: intervals between local events only.
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Watch `client` for relays closing a kept subscription, and check every kept one on a timer.
/// Once per session.
pub fn watch(client: &Client) {
    if !WATCHING.lock().unwrap_or_else(|e| e.into_inner()).insert(crate::db::current_session_id()) {
        return;
    }
    let mut notifications = client.notifications();
    let c = client.clone();
    crate::db::spawn_bound(async move {
        while let Some(n) = notifications.next().await {
            if crate::db::session_stopped() {
                return;
            }
            let ClientNotification::Message { relay_url, message } = n else { continue };
            let RelayMessage::Closed { subscription_id, message } = *message else { continue };
            // The NIP-42 responder re-sends once the stream is authenticated.
            if message.starts_with("auth-required:") {
                continue;
            }
            let (c, id, reason) = (c.clone(), subscription_id.into_owned(), message.into_owned());
            crate::db::spawn_bound(async move {
                // A close can beat the subscriber's own `keep` back from the REQ it answers.
                crate::rt::time::sleep(KEEP_GRACE).await;
                if kept_on(&id, &relay_url).is_some() {
                    crate::log_warn!("[live] {relay_url} closed {id}: {reason}");
                    schedule_restore(c, relay_url, id);
                }
            });
        }
    });
    let c = client.clone();
    crate::db::spawn_bound(async move {
        loop {
            crate::rt::time::sleep(HEAL_EVERY).await;
            if crate::db::session_stopped() {
                return;
            }
            heal(&c).await;
        }
    });
}

fn schedule_restore(client: Client, relay: RelayUrl, id: SubscriptionId) {
    let wait = {
        let mut map = CLOSES.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_ms();
        let entry = map.entry((relay.clone(), id.clone())).or_insert((0, now));
        if now.saturating_sub(entry.1) > BACKOFF_RESET_MS {
            entry.0 = 0;
        }
        let n = entry.0;
        *entry = (n.saturating_add(1), now);
        backoff(n)
    };
    crate::db::spawn_bound(async move {
        crate::rt::time::sleep(wait).await;
        restore(&client, &relay, &id).await;
    });
}

/// Send `id` to `relay` again under the same id, then catch up the time it was down.
async fn restore(client: &Client, relay: &RelayUrl, id: &SubscriptionId) -> bool {
    let Some(filters) = kept_on(id, relay) else { return false };
    let connected = matches!(client.relay(relay).await, Ok(Some(r)) if r.status() == RelayStatus::Connected);
    if !connected {
        // The reconnect re-sends what the pool holds, and this heals the rest after it.
        return false;
    }
    match client.subscribe(ReqTarget::single(relay.clone(), filters)).with_id(id.clone()).await {
        Ok(_) if kept_on(id, relay).is_none() => {
            // Unsubscribed while this was on its way: close what was just re-opened.
            if let Ok(Some(r)) = client.relay(relay).await {
                let _ = r.unsubscribe(id).await;
            }
            false
        }
        Ok(_) => {
            println!("[live] {relay}: restored {id}");
            gap(relay.to_string());
            true
        }
        Err(e) => {
            crate::log_warn!("[live] {relay}: restoring {id} failed: {e}");
            false
        }
    }
}

/// Restore every kept subscription a connected relay no longer holds. Cheap when nothing is
/// missing: one local lookup per relay and subscription.
pub async fn heal(client: &Client) -> usize {
    let session = crate::db::current_session_id();
    let wanted: Vec<(SubscriptionId, RelayUrl)> = KEPT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(_, (s, _))| *s == session)
        .flat_map(|(id, (_, targets))| targets.keys().map(move |url| (id.clone(), url.clone())))
        .collect();
    let mut restored = 0;
    for (id, url) in wanted {
        let Ok(Some(relay)) = client.relay(&url).await else { continue };
        if relay.status() != RelayStatus::Connected || backing_off(&url, &id) || relay.subscription(&id).await.is_some() {
            continue;
        }
        if restore(client, &url, &id).await {
            restored += 1;
        }
    }
    if restored > 0 {
        crate::log_warn!("[live] healed {restored} dropped live subscription(s)");
    }
    restored
}

/// A relay's catch-up, after a short settle so a burst of restores runs one per relay.
fn gap(relay: String) {
    let gaps = crate::db::current_session().scoped::<Gaps, Gaps>();
    {
        let mut pending = gaps.0.lock().unwrap_or_else(|e| e.into_inner());
        if !pending.contains(&relay) {
            pending.push(relay);
        }
    }
    if gaps.1.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::db::spawn_bound(async move {
        crate::rt::time::sleep(GAP_SETTLE).await;
        let relays = std::mem::take(&mut *gaps.0.lock().unwrap_or_else(|e| e.into_inner()));
        gaps.1.store(false, Ordering::SeqCst);
        if crate::db::session_is_live() {
            for relay in relays {
                catch_up(CatchUp::Relay(relay));
            }
        }
    });
}

#[derive(Default)]
struct Reviving(AtomicBool, AtomicU64);

/// A revival this soon after the last one finished is the same return, reported twice.
const REVIVE_COOLDOWN_MS: u64 = 20_000;

/// Away this long, the gap is fetched whatever the probes found.
const FULL_CATCH_UP_AFTER_MS: u64 = 15 * 60 * 1000;

/// Back from being away for `away_ms`: every relay answers a probe or is redialled, and every live
/// subscription a relay dropped is restored. The full catch-up then runs when `suspended` (the
/// process or its sockets were frozen: sleep, App Nap, a backgrounded app, a network change),
/// after a long absence, or when anything was found broken; otherwise the live feed carried it
/// all. A call while one runs, or just after, is the same return and does nothing.
pub async fn revive(client: &Client, away_ms: u64, suspended: bool) {
    let state = crate::db::current_session().scoped::<Reviving, Reviving>();
    if now_ms().saturating_sub(state.1.load(Ordering::SeqCst)) < REVIVE_COOLDOWN_MS || state.0.swap(true, Ordering::SeqCst) {
        return;
    }
    // A relay that stalls the probe must not hold back the catch-up: its own reconnect catches it up.
    let budget = crate::relay_request_timeout(Duration::from_secs(4)) + crate::relay_connect_timeout(Duration::from_secs(8));
    let redialled = crate::rt::time::timeout(budget, probe_or_redial(client)).await.unwrap_or(0);
    if !crate::db::session_stopped() {
        let healed = heal(client).await;
        let full = suspended || away_ms >= FULL_CATCH_UP_AFTER_MS || redialled + healed > 0;
        println!(
            "[live] back after {}: {redialled} relay(s) redialled, {healed} subscription(s) healed{}",
            crate::clock::describe_span(away_ms / 1000),
            if full { ", catching up" } else { "" }
        );
        if full {
            crate::community::v2::realtime::force_refresh_subscription(client).await;
            catch_up(CatchUp::All { away_ms });
        }
    }
    state.1.store(now_ms(), Ordering::SeqCst);
    state.0.store(false, Ordering::SeqCst);
}

/// Every relay answers a cheap probe or has its socket redialled: a suspended process keeps
/// sockets that died without closing.
/// Returns how many connected relays had gone silent; one already down is only retried.
pub async fn probe_or_redial(client: &Client) -> usize {
    let probe = crate::relay_request_timeout(Duration::from_secs(4));
    let connect = crate::relay_connect_timeout(Duration::from_secs(8));
    futures_util::stream::iter(client.relays().all().await)
        .map(|(url, relay)| {
            let client = client.clone();
            async move {
                if crate::transport::refuses(url.as_str()).is_some() {
                    return false;
                }
                if relay.status() == RelayStatus::Connected {
                    // An id no event can have: the relay answers with a bare EOSE.
                    let filter = Filter::new().id(EventId::from_byte_array([0; 32]));
                    let answered = crate::rt::time::timeout(
                        probe + Duration::from_secs(1),
                        client.fetch_events(ReqTarget::single(url.to_string(), [filter])).timeout(probe),
                    )
                    .await;
                    if matches!(answered, Ok(Ok(_))) {
                        return false;
                    }
                    crate::log_warn!("[live] {url}: no answer after being away, redialling");
                    relay.disconnect();
                    let _ = relay.try_connect().timeout(connect).await;
                    return true;
                }
                if crate::transport::cycle::revivable(&relay) {
                    let _ = relay.try_connect().timeout(connect).await;
                }
                false
            }
        })
        .buffer_unordered(8)
        .filter(|redialled| std::future::ready(*redialled))
        .count()
        .await
}

#[cfg(not(target_arch = "wasm32"))]
pub use wake::start_wake_watch;

#[cfg(not(target_arch = "wasm32"))]
mod wake {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// How often the process checks it is still running on time.
    const TICK: Duration = Duration::from_secs(10);
    /// A tick this late means the process was suspended (sleep, App Nap, doze), not just busy.
    const SUSPENDED_MS: u64 = 45_000;

    static STARTED: AtomicBool = AtomicBool::new(false);

    /// Notice the process waking from a suspension, whichever account is live, and revive it.
    /// Once per process.
    pub fn start_wake_watch() {
        if STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        // spawn-detached: process-lifetime timer; each wake resolves the account live at that moment.
        crate::rt::spawn(async {
            let mut last = super::now_ms();
            loop {
                crate::rt::time::sleep(TICK).await;
                let now = super::now_ms();
                let late = now.saturating_sub(last).saturating_sub(TICK.as_millis() as u64);
                last = now;
                if late < SUSPENDED_MS || !crate::db::session_is_live() {
                    continue;
                }
                let Some(client) = crate::state::nostr_client() else { continue };
                let away = late + TICK.as_millis() as u64;
                crate::db::spawn_bound(async move { super::revive(&client, away, true).await });
            }
        });
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::ClientRelayExt;
    use std::sync::Arc;
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    /// A relay that answers each subscription id's first REQ with CLOSED and every later one with
    /// EOSE, recording the ids it was asked for.
    async fn closing_relay(reason: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let reqs = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = reqs.clone();
        // spawn-detached: the test's own relay.
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let seen = seen.clone();
                // spawn-detached: one test connection.
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else { return };
                    while let Some(Ok(msg)) = ws.next().await {
                        let Message::Text(text) = msg else { continue };
                        let v: serde_json::Value = serde_json::from_str(text.as_str()).unwrap();
                        if v[0] != "REQ" {
                            continue;
                        }
                        let id = v[1].as_str().unwrap().to_string();
                        let first = {
                            let mut seen = seen.lock().unwrap();
                            let first = !seen.contains(&id);
                            seen.push(id.clone());
                            first
                        };
                        let reply = if first {
                            serde_json::json!(["CLOSED", id, reason])
                        } else {
                            serde_json::json!(["EOSE", id])
                        };
                        let _ = ws.send(Message::text(reply.to_string())).await;
                    }
                });
            }
        });
        (url, reqs)
    }

    async fn asked(reqs: &Mutex<Vec<String>>, id: &SubscriptionId, times: usize) -> bool {
        for _ in 0..60 {
            if reqs.lock().unwrap().iter().filter(|r| *r == &id.to_string()).count() >= times {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    #[tokio::test]
    async fn a_live_subscription_a_relay_closes_is_sent_again() {
        let (url, reqs) = closing_relay("error: at most 200 open subscriptions").await;
        let client = Client::builder().build();
        client.add_managed_relay(&url).await.unwrap();
        client.connect().and_wait(Duration::from_secs(5)).await;
        watch(&client);

        let filter = Filter::new().kind(Kind::GiftWrap).limit(0);
        let out = client.subscribe(filter.clone()).await.unwrap();
        keep_output(&out, vec![filter]);

        assert!(asked(&reqs, &out.value, 2).await, "the closed subscription is asked for again");
        let relay = client.relay(&url).await.unwrap().unwrap();
        assert!(relay.subscription(&out.value).await.is_some(), "and the pool holds it, so a reconnect re-sends it");
    }

    #[tokio::test]
    async fn a_closed_subscription_nobody_keeps_is_left_closed() {
        let (url, reqs) = closing_relay("error: shutting down").await;
        let client = Client::builder().build();
        client.add_managed_relay(&url).await.unwrap();
        client.connect().and_wait(Duration::from_secs(5)).await;
        watch(&client);

        let out = client.subscribe(Filter::new().kind(Kind::TextNote).limit(0)).await.unwrap();
        tokio::time::sleep(KEEP_GRACE + FIRST_BACKOFF + Duration::from_millis(800)).await;
        assert!(!asked(&reqs, &out.value, 2).await, "an unkept subscription is not revived");
    }

    #[tokio::test]
    async fn a_subscription_forgotten_before_its_close_is_judged_stays_closed() {
        let (url, reqs) = closing_relay("error: at most 200 open subscriptions").await;
        let client = Client::builder().build();
        client.add_managed_relay(&url).await.unwrap();
        client.connect().and_wait(Duration::from_secs(5)).await;
        watch(&client);

        let filter = Filter::new().kind(Kind::GiftWrap).limit(0);
        let out = client.subscribe(filter.clone()).await.unwrap();
        keep_output(&out, vec![filter]);
        forget(&out.value);
        tokio::time::sleep(KEEP_GRACE + FIRST_BACKOFF + Duration::from_millis(800)).await;
        assert!(!asked(&reqs, &out.value, 2).await, "a replaced subscription is not brought back");
    }

    #[tokio::test]
    async fn heal_waits_out_the_backoff_of_a_recent_close() {
        let (url, reqs) = closing_relay("error: at most 200 open subscriptions").await;
        let client = Client::builder().build();
        client.add_managed_relay(&url).await.unwrap();
        client.connect().and_wait(Duration::from_secs(5)).await;
        watch(&client);

        let filter = Filter::new().kind(Kind::GiftWrap).limit(0);
        let out = client.subscribe(filter.clone()).await.unwrap();
        keep_output(&out, vec![filter]);
        let relay_url = RelayUrl::parse(&url).unwrap();
        for _ in 0..30 {
            if backing_off(&relay_url, &out.value) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(backing_off(&relay_url, &out.value), "the close was seen");
        assert_eq!(heal(&client).await, 0, "the scheduled restore owns the retry");
        assert!(asked(&reqs, &out.value, 2).await, "and it still runs");
    }

    #[tokio::test]
    async fn heal_restores_what_a_relay_dropped_and_leaves_held_ones_alone() {
        let (url, reqs) = closing_relay("error: at most 200 open subscriptions").await;
        let client = Client::builder().build();
        client.add_managed_relay(&url).await.unwrap();
        client.connect().and_wait(Duration::from_secs(5)).await;

        let filter = Filter::new().kind(Kind::GiftWrap).limit(0);
        let out = client.subscribe(filter.clone()).await.unwrap();
        keep_output(&out, vec![filter]);
        let relay = client.relay(&url).await.unwrap().unwrap();
        for _ in 0..50 {
            if relay.subscription(&out.value).await.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(relay.subscription(&out.value).await.is_none(), "nostr-sdk forgot the closed subscription");

        assert_eq!(heal(&client).await, 1);
        assert!(relay.subscription(&out.value).await.is_some());
        assert_eq!(heal(&client).await, 0, "a held subscription is not sent again");
        assert!(asked(&reqs, &out.value, 2).await);
        assert_eq!(reqs.lock().unwrap().iter().filter(|r| *r == &out.value.to_string()).count(), 2);
    }
}
