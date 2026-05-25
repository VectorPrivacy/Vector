//! Shared NIP-77 negentropy set reconciliation.
//!
//! One acquisition primitive for the whole app. DM and MLS sync differ only
//! in their fingerprint source and processing; the reconcile-against-relays
//! step is identical, so it lives here.

use std::collections::HashSet;
use std::time::Duration;

use nostr_sdk::prelude::*;

/// Race every trusted relay exchanging negentropy fingerprints for `filter`,
/// and return the union of event IDs that relays hold but we don't.
///
/// `local_items` is our fingerprint set: `(event_id, created_at)` for
/// everything we already possess. Each relay reports only the IDs absent from
/// that set, so the union across relays is the complete missing set reachable
/// from our trusted relays.
///
/// Every relay is drained (not just the first to respond): completeness beats
/// latency here, and one relay may lack events another holds. Each relay is
/// bounded by `timeout`.
pub async fn reconcile_missing(
    filter: Filter,
    local_items: Vec<(EventId, Timestamp)>,
    timeout: Duration,
) -> Result<HashSet<EventId>, String> {
    use futures_util::stream::{FuturesUnordered, StreamExt};

    let client = crate::state::nostr_client().ok_or("Nostr client not initialized")?;

    let opts = SyncOptions::new()
        .direction(SyncDirection::Down)
        .initial_timeout(timeout)
        .dry_run();

    // Resolve trusted relay URLs to live Relay handles.
    let relay_map = client.relays().await;
    let trusted = crate::state::active_trusted_relays().await;
    let relays: Vec<(String, Relay)> = trusted.iter().filter_map(|url| {
        let normalized = url.trim_end_matches('/');
        relay_map.iter()
            .find(|(u, _)| u.as_str().trim_end_matches('/') == normalized)
            .map(|(_, r)| (url.to_string(), r.clone()))
    }).collect();
    drop(relay_map);

    if relays.is_empty() {
        crate::log_warn!("[Negentropy] No trusted relays available for reconciliation");
        return Ok(HashSet::new());
    }

    let mut futs = FuturesUnordered::new();
    for (url, relay) in &relays {
        let url = url.clone();
        let relay = relay.clone();
        let f = filter.clone();
        let items = local_items.clone();
        let o = opts.clone();
        futs.push(async move {
            let r = tokio::time::timeout(timeout, relay.sync_with_items(f, items, &o)).await;
            (url, r)
        });
    }

    let mut missing: HashSet<EventId> = HashSet::new();
    while let Some((url, result)) = futs.next().await {
        match result {
            Ok(Ok(recon)) => {
                let n = recon.remote.len();
                missing.extend(recon.remote);
                crate::log_debug!("[Negentropy] {} reconciled: {} missing", url, n);
            }
            Ok(Err(e)) => crate::log_warn!("[Negentropy] {} failed: {}", url, e),
            Err(_) => crate::log_warn!("[Negentropy] {} timed out", url),
        }
    }

    Ok(missing)
}
