//! Chat-plane volley — the boot paint pass (CONCORD_CHAT_PLANE_VOLLEY_DESIGN.md).
//!
//! Paints every channel's CURRENT-epoch chat plane from stored state in a few
//! batched multi-filter REQs on the shared warm community client. No per-plane
//! clients, no NIP-42 warmups, no dial storms: a cold `fetch_plane` per plane
//! pays a connect + per-relay warmup gauntlet, which is how a 47-channel boot
//! became a 36s crawl. Auth-gating relays contribute nothing to a batch (they
//! CLOSE REQs whose authors aren't the connection's authed key); the planes
//! also live on the non-gating relays, and anything held ONLY by a gating
//! relay still arrives via the verification sweep behind this.
//!
//! Non-authoritative by design: chat planes carry nothing that needs verifying
//! before display. The control plane folds AFTER paint and enforces
//! retroactively (retro-hide revokes a banned member's painted rows). Epochs
//! rotated while offline make these filters return silence — never lies — and
//! the rekey walk behind this repaints the channel under its new epoch.

use std::collections::{HashMap, HashSet};

use futures_util::stream::{FuturesUnordered, StreamExt};
use nostr_sdk::prelude::*;

use crate::community::transport::{Evidence, LiveTransport, Query, Transport};
use crate::community::v2::derive::{channel_group_key, GroupKey};
use crate::community::v2::service::FetchedEvent;
use crate::community::v2::{chat, stream};
use crate::community::{ChannelId, CommunityId, Epoch};
use crate::state::{self, SessionGuard};

/// One channel to paint. `since` = the chat's last held message (seconds,
/// minus the caller's slack) so the page carries only genuinely-new wraps.
pub struct PaintTarget {
    pub community_id: CommunityId,
    pub channel_hex: String,
    pub since: Option<u64>,
}

/// Filters per REQ — comfortably under every relay's max_filters cap.
const BATCH_FILTERS: usize = 20;

struct Job {
    channel_hex: String,
    channel_id: ChannelId,
    group: GroupKey,
    epoch: Epoch,
    since: Option<u64>,
    relay_set: usize,
}

/// Learned from live NIP-42 challenges (streamauth records the KV): a gating
/// relay answers batch REQs with silence no matter what it holds.
fn relay_auth_gating(url: &str) -> bool {
    crate::db::get_sql_setting(format!("auth_gate:{}", url.trim_end_matches('/')))
        .ok()
        .flatten()
        .is_some()
}

/// The subset of `relays` currently CONNECTED on the shared client, waiting up
/// to `allowance` for the first one — cold boots dial these sockets moments
/// before the volley needs them.
async fn connected_targets(
    client: &Client,
    relays: &[String],
    allowance: std::time::Duration,
) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + allowance;
    loop {
        let pool = client.relays().await;
        let up: Vec<String> = relays
            .iter()
            .filter(|r| {
                RelayUrl::parse(r)
                    .ok()
                    .and_then(|u| pool.get(&u).map(|rl| rl.status() == RelayStatus::Connected))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        if !up.is_empty() || tokio::time::Instant::now() >= deadline {
            return up;
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
}

/// Stage timing + hit counts for the boot log — the volley is a performance
/// feature, and every regression so far was found by reading these.
#[derive(Default)]
pub struct VolleyStats {
    pub batch_ms: u128,
    pub fallback_ms: u128,
    pub batch_events: usize,
    pub fallback_events: usize,
}

/// Paint every target's latest page. Returns `(channel_hex, new)` per channel
/// that gained messages (callers own the logging) plus stage stats.
pub async fn paint_all(targets: Vec<PaintTarget>) -> (Vec<(String, usize)>, VolleyStats) {
    let session = SessionGuard::capture();
    let mut stats = VolleyStats::default();
    let Some(my_pk) = state::my_public_key() else {
        return (Vec::new(), stats);
    };

    // Group targets per community: one DB load each, current-epoch planes only.
    let mut by_community: Vec<(CommunityId, Vec<PaintTarget>)> = Vec::new();
    for t in targets {
        match by_community.iter_mut().find(|(id, _)| *id == t.community_id) {
            Some((_, v)) => v.push(t),
            None => by_community.push((t.community_id, vec![t])),
        }
    }

    let mut relay_sets: Vec<Vec<String>> = Vec::new();
    let mut jobs: Vec<Job> = Vec::new();
    let mut plane_index: HashMap<PublicKey, usize> = HashMap::new();
    for (cid, ts) in by_community {
        let cid_hex = crate::simd::hex::bytes_to_hex_32(&cid.0);
        if crate::db::community::get_community_dissolved(&cid_hex).unwrap_or(false) {
            continue;
        }
        let Ok(Some(community)) = crate::db::community::load_community_v2(&cid) else {
            continue;
        };
        let mut sorted = community.relays.clone();
        sorted.sort();
        let relay_set = match relay_sets.iter().position(|r| {
            let mut s = r.clone();
            s.sort();
            s == sorted
        }) {
            Some(i) => i,
            None => {
                relay_sets.push(community.relays.clone());
                relay_sets.len() - 1
            }
        };
        for t in ts {
            let ch_id = ChannelId(crate::simd::hex::hex_to_bytes_32(&t.channel_hex));
            let Some(ch) = community.channel(&ch_id) else { continue };
            // ONE plane per channel, at the MAX HELD epoch: the community
            // row's epoch fields can lag a rotation the rekey walk already
            // archived, so "current" means the freshest key the DB holds.
            // Older epochs stay the history-pagination system's job.
            let ch_hex = crate::simd::hex::bytes_to_hex_32(&ch.id.0);
            let (group, epoch) = if ch.private {
                let mut best: Option<(crate::community::Epoch, [u8; 32])> =
                    ch.key.map(|k| (ch.epoch, k));
                for (ep, k) in
                    crate::db::community::held_epoch_keys(&cid_hex, &ch_hex).unwrap_or_default()
                {
                    // A private plane is never derived from the root value.
                    if k == community.community_root {
                        continue;
                    }
                    if best.map_or(true, |(be, _)| ep.0 > be.0) {
                        best = Some((ep, k));
                    }
                }
                let Some((ep, key)) = best else { continue };
                (channel_group_key(&key, &ch_id, ep), ep)
            } else {
                let mut best = (community.root_epoch, community.community_root);
                for (ep, k) in crate::db::community::held_epoch_keys(
                    &cid_hex,
                    crate::community::SERVER_ROOT_SCOPE_HEX,
                )
                .unwrap_or_default()
                {
                    if ep.0 > best.0 .0 {
                        best = (ep, k);
                    }
                }
                (channel_group_key(&best.1, &ch_id, best.0), best.0)
            };
            // A duplicated target would orphan the first job (unroutable) and
            // burn a fallback dial — first derivation wins.
            if plane_index.contains_key(&group.pk()) {
                continue;
            }
            plane_index.insert(group.pk(), jobs.len());
            jobs.push(Job {
                channel_hex: t.channel_hex,
                channel_id: ch_id,
                group,
                epoch,
                since: t.since,
                relay_set,
            });
        }
    }
    if jobs.is_empty() {
        return (Vec::new(), stats);
    }

    // One multi-filter REQ per ≤BATCH_FILTERS jobs per relay, all concurrent,
    // all on the shared warm client. Callers pass targets recency-first and
    // job order preserves it, so the hottest channels ride the first batches.
    let mut by_set: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, j) in jobs.iter().enumerate() {
        by_set.entry(j.relay_set).or_default().push(i);
    }
    let batch_start = std::time::Instant::now();
    let shared = LiveTransport::warm_client(
        relay_sets.iter().flat_map(|r| r.iter().cloned()).collect::<Vec<_>>().as_slice(),
        std::time::Duration::from_secs(4),
    )
    .await
    .ok();

    // Per-set pipelines: each set gates on its own first live socket, then
    // fires its filter chunks — independent, so a dead-only set's allowance
    // never holds another set's filters hostage.
    let fetch_budget = crate::relay_request_timeout(std::time::Duration::from_secs(4));
    let mut fetches = FuturesUnordered::new();
    for (set, idxs) in by_set {
        let chunks: Vec<Vec<Filter>> = idxs
            .chunks(BATCH_FILTERS)
            .map(|chunk| {
                chunk
                    .iter()
                    .map(|&i| {
                        let j = &jobs[i];
                        Query {
                            kinds: vec![stream::KIND_WRAP],
                            authors: vec![j.group.pk_hex()],
                            since: j.since,
                            limit: Some(50),
                            ..Default::default()
                        }
                        .to_filter()
                    })
                    .collect()
            })
            .collect();
        let relays = relay_sets[set].clone();
        let client = shared.clone();
        fetches.push(async move {
            let Some(client) = client else {
                return (set, Vec::new(), Vec::new());
            };
            let live =
                connected_targets(&client, &relays, std::time::Duration::from_millis(2500)).await;
            if live.is_empty() {
                return (set, live, Vec::new());
            }
            let mut evs: Vec<Event> = Vec::new();
            let mut per = FuturesUnordered::new();
            for chunk in &chunks {
                for r in &live {
                    let c = client.clone();
                    let f = chunk.clone();
                    let r = r.clone();
                    per.push(async move {
                        c.fetch_events(ReqTarget::single(r, f)).timeout(fetch_budget).await
                    });
                }
            }
            while let Some(res) = per.next().await {
                if let Ok(batch) = res {
                    evs.extend(batch);
                }
            }
            (set, live, evs)
        });
    }

    // Route each wrap to its job by plane author as sets complete.
    let mut live_by_set: HashMap<usize, Vec<String>> = HashMap::new();
    let mut seen_wraps: HashSet<EventId> = HashSet::new();
    let mut pages: HashMap<usize, Vec<FetchedEvent>> = HashMap::new();
    while let Some((set, live, evs)) = fetches.next().await {
        live_by_set.insert(set, live);
        for wrap in evs {
            if !seen_wraps.insert(wrap.id) {
                continue;
            }
            let Some(&job_idx) = plane_index.get(&wrap.pubkey) else { continue };
            let j = &jobs[job_idx];
            if let Ok(event) = chat::open_chat_event(&wrap, &j.group, &j.channel_id, j.epoch) {
                stats.batch_events += 1;
                pages.entry(job_idx).or_default().push(FetchedEvent { event, epoch: j.epoch });
            }
        }
    }
    stats.batch_ms = batch_start.elapsed().as_millis();
    let fallback_start = std::time::Instant::now();

    // Second barrel: jobs the batch couldn't see. When the only LIVE relay in
    // a set is auth-gating (Ditto serves plane reads solely to a connection
    // authed AS the plane), batches get protocol-correct silence — fetch_plane
    // pays the per-plane authed connection through its pool instead. Quiet
    // channels cost one pooled round trip; the breaker keeps dead relays from
    // taxing the warmups.
    let transport = LiveTransport::with_timeout(std::time::Duration::from_secs(4));
    // One KV read per relay, not three per missed job.
    let gating: HashSet<String> = live_by_set
        .values()
        .flat_map(|v| v.iter())
        .filter(|r| relay_auth_gating(r))
        .cloned()
        .collect();
    let missed: Vec<(usize, nostr_sdk::prelude::Keys, Query, Vec<String>)> = jobs
        .iter()
        .enumerate()
        .filter(|(idx, j)| {
            if pages.contains_key(idx) {
                return false;
            }
            // NO live relay: nothing can answer at any price — the reconnect
            // catch-up owns the channel when its relays return.
            let live = live_by_set.get(&j.relay_set).map(Vec::as_slice).unwrap_or(&[]);
            if live.is_empty() {
                return false;
            }
            // Every live relay gates: batch silence proved nothing.
            if live.iter().all(|r| gating.contains(r.as_str())) {
                return true;
            }
            // A live OPEN relay answered with silence — usually a quiet
            // channel, but a flaky relay can hold HOLES (missed publishes),
            // so recently-active channels still confirm against a live gating
            // relay. Dormant ones trust the batch.
            const RECENT_SECS: u64 = 7 * 24 * 3600;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            j.since.is_some_and(|s| now.saturating_sub(s) < RECENT_SECS)
                && live.iter().any(|r| gating.contains(r.as_str()))
        })
        .map(|(idx, j)| {
            let q = Query {
                kinds: vec![stream::KIND_WRAP],
                authors: vec![j.group.pk_hex()],
                since: j.since,
                limit: Some(50),
                // Declared intent — fetch_plane does not consult evidence
                // yet (#370); its 4s transport bound is the effective limit.
                evidence: Evidence::Fast,
                ..Default::default()
            };
            // Confirmations go to the gating relays only — the open ones
            // already answered this job in the batch.
            let live = live_by_set.get(&j.relay_set).map(Vec::as_slice).unwrap_or(&[]);
            let gate_targets: Vec<String> =
                live.iter().filter(|r| gating.contains(r.as_str())).cloned().collect();
            let targets = if gate_targets.is_empty() { live.to_vec() } else { gate_targets };
            (idx, j.group.keys().clone(), q, targets)
        })
        .collect();
    let fallback_pages: Vec<(usize, Vec<Event>)> = futures_util::stream::iter(missed)
        .map(|(idx, keys, q, relays)| {
            let t = &transport;
            async move { (idx, t.fetch_plane(&keys, &q, &relays).await.unwrap_or_default()) }
        })
        .buffer_unordered(24)
        .collect()
        .await;
    for (job_idx, evs) in fallback_pages {
        let j = &jobs[job_idx];
        for wrap in evs {
            if !seen_wraps.insert(wrap.id) {
                continue;
            }
            if wrap.pubkey != j.group.pk() {
                continue;
            }
            if let Ok(event) = chat::open_chat_event(&wrap, &j.group, &j.channel_id, j.epoch) {
                stats.fallback_events += 1;
                pages.entry(job_idx).or_default().push(FetchedEvent { event, epoch: j.epoch });
            }
        }
    }
    stats.fallback_ms = fallback_start.elapsed().as_millis();

    let mut painted: Vec<(String, usize)> = Vec::new();
    for (job_idx, mut page) in pages {
        if !session.is_valid() {
            break;
        }
        page.sort_by_key(|f| f.event.opened().at_ms);
        let new = crate::VectorCore::v2_ingest_chat_page(
            &jobs[job_idx].channel_hex,
            my_pk,
            session,
            page,
        )
        .await;
        if new > 0 {
            painted.push((jobs[job_idx].channel_hex.clone(), new));
        }
    }
    (painted, stats)
}
