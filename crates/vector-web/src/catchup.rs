//! Catching up on what the live subscriptions never saw.
//!
//! A live subscription carries only what is published after it lands, so a
//! launch, a relay reconnect and a return to the foreground each fetch the gap,
//! behind subscriptions that are already streaming: the community list and
//! epochs, every held channel's latest page, then the DMs.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use futures_util::stream::{self, StreamExt};
use nostr_sdk::prelude::*;
use vector_core::community::v2::realtime;
use vector_core::community::v2::volley::{self, PaintTarget};
use vector_core::community::CommunityId;
use vector_core::dm_catchup::{self, Opts, Report};
use vector_core::event_handler::{commit_prepared_event, BatchingPersist};
use vector_core::simd::hex::bytes_to_hex_32;
use vector_core::{db, state};

/// Channels paged at once: bounds the REQ pressure on a phone's few sockets.
const WINDOW: usize = 4;
/// The paint asks from a little before each channel's newest held message.
const SINCE_SLACK_SECS: u64 = 3600;
/// Quiet time after a sweep before a reconnect buys another: a flapping relay
/// must not sweep every channel on each blip.
const COOLDOWN_MS: u64 = 20_000;

static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static RERUN: AtomicBool = AtomicBool::new(false);
static RECONNECT_ARMED: AtomicBool = AtomicBool::new(false);
static LAST_END_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Run the sweep, or queue one more behind a run already going: a relay that
/// came up mid-run may hold what that run never asked it for.
pub fn request() {
    if IN_FLIGHT.swap(true, Ordering::SeqCst) {
        RERUN.store(true, Ordering::SeqCst);
        return;
    }
    db::spawn_bound(run());
}

fn rerun() -> Pin<Box<dyn Future<Output = ()>>> {
    Box::pin(run())
}

/// A burst of relay reconnects becomes one sweep once it settles, deferred (never
/// dropped) past the cooldown. Before the live subscriptions land, the launch's
/// own sweep is still to come.
pub fn after_reconnect() {
    if !live() || RECONNECT_ARMED.swap(true, Ordering::SeqCst) {
        return;
    }
    db::spawn_bound(async {
        let cooling = (LAST_END_MS.load(Ordering::SeqCst) + COOLDOWN_MS).saturating_sub(now_ms());
        vector_core::rt::time::sleep(Duration::from_millis(cooling.max(1500))).await;
        RECONNECT_ARMED.store(false, Ordering::SeqCst);
        request();
        dms(true).await;
    });
}

/// Back in the foreground. The OS suspends a backgrounded page's sockets without
/// always closing them, so each relay proves it is alive or is redialled, and
/// anything published while away is fetched.
pub async fn resume(hidden_ms: u64) {
    let Some(client) = state::nostr_client() else { return };
    if hidden_ms >= 30_000 {
        // Pooled plane logins went quiet with the page; later reads dial fresh.
        vector_core::community::transport::clear_plane_pool();
    }
    stream::iter(client.relays().await)
        .map(|(url, relay)| {
            let client = client.clone();
            async move {
                if relay.status() == RelayStatus::Connected {
                    let probe = client
                        .fetch_events(ReqTarget::single(url.to_string(), [Filter::new().kind(Kind::Metadata).limit(1)]))
                        .timeout(Duration::from_secs(4))
                        .await;
                    if probe.is_ok() {
                        return;
                    }
                    relay.disconnect();
                }
                let _ = relay.try_connect().timeout(Duration::from_secs(8)).await;
            }
        })
        .buffer_unordered(8)
        .collect::<Vec<()>>()
        .await;

    if !live() {
        return;
    }
    // Subscriptions a dead socket took down come back on the redialled one.
    realtime::force_refresh_subscription(&client).await;
    request();
    dms(true).await;
}

fn live() -> bool {
    realtime::subscription_ready()
}

struct Bootstrapping;

/// Catch up on DMs: each read relay's window since its cursor, retried behind it
/// until held. A relay with no cursor yet then reconciles its whole history once,
/// in the background.
pub async fn dms(is_new: bool) -> Report {
    let report = dm_pass(Opts { is_new, only: None, bootstrap: false }).await;
    if !report.complete() {
        let first = report.clone();
        db::spawn_bound(async move {
            dm_catchup::retry_incomplete(&first, |relays| {
                dm_pass(Opts { is_new, only: (!relays.is_empty()).then_some(relays), bootstrap: false })
            })
            .await;
        });
    }
    bootstrap_cursorless().await;
    dm_catchup::start_safety_net(|| dm_pass(Opts { is_new: true, only: None, bootstrap: false }));
    report
}

async fn dm_pass(opts: Opts) -> Report {
    let batcher = BatchingPersist::new(&crate::events::WebEventHandler);
    dm_catchup::catch_up(&opts, &batcher, |prepared, is_new| commit_prepared_event(prepared, is_new, &batcher)).await
}

async fn bootstrap_cursorless() {
    let Some(client) = state::nostr_client() else { return };
    let fresh: Vec<String> = client
        .relays()
        .await
        .into_iter()
        .filter(|(url, relay)| relay.capabilities().has_read() && vector_core::negentropy::reconcile_cursor(url.as_str()).is_none())
        .map(|(url, _)| url.to_string())
        .collect();
    let busy = db::current_session().scoped::<Bootstrapping, AtomicBool>();
    if fresh.is_empty() || busy.swap(true, Ordering::SeqCst) {
        return;
    }
    db::spawn_bound(async move {
        dm_pass(Opts { is_new: false, only: Some(fresh), bootstrap: true }).await;
        busy.store(false, Ordering::SeqCst);
    });
}

async fn run() {
    RERUN.store(false, Ordering::SeqCst);
    let started = web_time::Instant::now();
    let channels = sweep().await;
    vector_core::log_info!("[Web] catch-up: {channels} channel(s) in {:?}", started.elapsed());
    LAST_END_MS.store(now_ms(), Ordering::SeqCst);
    IN_FLIGHT.store(false, Ordering::SeqCst);
    if RERUN.swap(false, Ordering::SeqCst) && !db::session_stopped() {
        IN_FLIGHT.store(true, Ordering::SeqCst);
        db::spawn_bound(rerun());
    }
}

async fn sweep() -> usize {
    // Memberships from other devices, alongside the sweep rather than ahead of it.
    db::spawn_bound(crate::selfsync::ingest_v2_community_list());

    let Ok(ids) = db::community::list_community_ids() else { return 0 };
    let last = db::events::get_all_chats_last_messages().await.unwrap_or_default();
    let activity = |cid: &str| last.get(cid).and_then(|v| v.first().map(|m| m.at)).unwrap_or(0);

    let mut held: Vec<CommunityId> = Vec::new();
    let mut targets: Vec<(PaintTarget, u64)> = Vec::new();
    for id in ids {
        let Ok(Some(c)) = db::community::load_community_v2(&id) else { continue };
        if c.dissolved {
            continue;
        }
        held.push(CommunityId(id.0));
        for ch in &c.channels {
            let hex = bytes_to_hex_32(&ch.id.0);
            let at = activity(&hex);
            let since = (at > 0).then(|| (at / 1000).saturating_sub(SINCE_SLACK_SECS));
            targets.push((PaintTarget { community_id: CommunityId(id.0), channel_hex: hex, since }, at));
        }
    }
    targets.sort_by_key(|t| std::cmp::Reverse(t.1));
    let channels: Vec<String> = targets.iter().map(|(t, _)| t.channel_hex.clone()).collect();

    // One batched read paints what the shared connections can see.
    volley::paint_all(targets.into_iter().map(|(t, _)| t).collect()).await;
    // Rotations and moderation fold behind the paint; a moved epoch resubscribes.
    for id in &held {
        realtime::enqueue_follow(id);
    }
    // Per-channel pages reach the relays that answer only a plane's own login.
    let count = channels.len();
    stream::iter(channels)
        .map(|cid| async move {
            if db::session_stopped() {
                return;
            }
            let _ = crate::community::sync_community_channel(&cid, None).await;
        })
        .buffer_unordered(WINDOW)
        .collect::<Vec<()>>()
        .await;

    // Relays under the sweep's load can drop the live subscriptions, and an
    // unchanged author set makes the ordinary refresh a no-op.
    if let Some(client) = state::nostr_client() {
        realtime::force_refresh_subscription(&client).await;
    }
    count
}
