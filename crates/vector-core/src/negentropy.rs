//! Shared NIP-77 negentropy set reconciliation.
//!
//! One acquisition primitive for the whole app. DM and community sync differ only
//! in their fingerprint source and processing; the reconcile-against-relays
//! step is identical, so it lives here.

use std::collections::HashSet;
use std::time::Duration;

use nostr_sdk::prelude::*;

// ============================================================================
// NIP-77 capability cache
// ============================================================================
//
// Some relay software never implemented negentropy (nostr-rs-relay answers a
// NEG-OPEN with an unrecognized NOTICE at best), and nostr-sdk's support check
// only recognizes strfry-style refusals — so every sync against such a relay
// silently burns the full initial_timeout before failing. Verdicts persist in
// the account settings KV with a TTL: boots skip doomed reconciles, and a
// relay that upgrades its software is re-probed within a day.

const NEG_CAP_TTL_SECS: u64 = 24 * 3600;

fn cap_key(relay_url: &str) -> String {
    format!("neg_cap:{}", relay_url.trim_end_matches('/'))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Cached NIP-77 verdict for a relay. `None` = unknown or stale — attempt
/// negentropy and let the outcome refresh the cache.
pub fn neg_supported_cached(relay_url: &str) -> Option<bool> {
    let raw = crate::db::get_sql_setting(cap_key(relay_url)).ok()??;
    let (supported, checked_at) = parse_cap_entry(&raw)?;
    (now_secs().saturating_sub(checked_at) < NEG_CAP_TTL_SECS).then_some(supported)
}

/// Persist a fresh verdict (value format: `0|1:<unix seconds>`). The verdict is
/// relay-global truth, so a raced write is wasted work, never wrong data.
pub fn record_neg_support(relay_url: &str, supported: bool) {
    let _ = crate::db::set_sql_setting(
        cap_key(relay_url),
        format!("{}:{}", u8::from(supported), now_secs()),
    );
}

fn parse_cap_entry(raw: &str) -> Option<(bool, u64)> {
    let (flag, ts) = raw.split_once(':')?;
    let supported = match flag {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    Some((supported, ts.parse().ok()?))
}

/// Interpret a `relay.sync` error. `Some(false)` = the relay cannot
/// reconcile: either the SDK recognized the refusal outright, or a relay that
/// was CONNECTED stayed silent past the initial timeout — healthy negentropy
/// implementations answer the first frame in well under a second, so silence
/// on a live connection is the no-implementation signature. A timeout on a
/// relay that wasn't connected is an outage and classifies as nothing.
/// Wait briefly for a relay to reach Connected before a sync attempt.
/// `false` = never connected inside the allowance — callers treat that as a
/// TRANSIENT skip (no verdict, no skip-list, no cursor touch): an unreachable
/// relay must cost the allowance, not a full negentropy initial_timeout. The
/// Monitor-driven reconnect catch-up covers it the moment it truly connects.
pub async fn wait_connected(relay: &Relay, allowance: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + allowance;
    loop {
        match relay.status() {
            RelayStatus::Connected => return true,
            RelayStatus::Terminated | RelayStatus::Banned => return false,
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

// ============================================================================
// Per-relay reconcile cursor
// ============================================================================
//
// "Reconcile-verified through T": the boot quick sync reconciles a cursored
// relay only from (cursor − NIP-59 slack), and the full-history pass runs
// solely as a one-time bootstrap for relays with no cursor. A cursor advances
// exclusively on PROOF — a zero-missing reconcile (the wrapper ledger only
// gains rows after commit, so zero-missing means relay and ledger agree
// through the window), or a bootstrap whose every requested event was
// actually received. Never on a failed or partial pass: a wrong cursor
// silently loses history, a stalled one merely re-reconciles a small window.

fn cursor_key(relay_url: &str) -> String {
    format!("neg_cursor:{}", relay_url.trim_end_matches('/'))
}

/// The relay's reconcile cursor (unix seconds), if it has ever earned one.
pub fn reconcile_cursor(relay_url: &str) -> Option<u64> {
    crate::db::get_sql_setting(cursor_key(relay_url)).ok()??.parse().ok()
}

/// Monotonic advance — one SQL upsert (the stored value only grows), so a
/// late or concurrent writer can never regress the cursor. Session-gated
/// IMMEDIATELY before the write: a swap landing after the caller's own check
/// must not stamp this account's cursor into the next account's KV — that
/// would both skew its quick window and silently skip its bootstrap.
pub fn advance_reconcile_cursor(relay_url: &str, anchor_secs: u64) {
    let _ = crate::db::advance_u64_setting(cursor_key(relay_url), anchor_secs);
}

/// True when a `relay.sync` failure says nothing durable about the relay —
/// connection-state errors and timeouts. Only deterministic refusals (protocol
/// errors, query caps) repeat identically on a later attempt, so only those
/// belong on a same-boot skip-list; a connected relay that timed out is
/// handled through the capability cache instead.
pub fn is_transient_sync_error(err: &str) -> bool {
    err == "timeout"
        || err.contains("not connected")
        || err.contains("transport dispatcher")
        // SDK notification broadcast overflow on a busy connection — says
        // nothing about the relay at all.
        || err.contains("lagged")
}

pub fn classify_neg_sync_error(err: &str, relay_was_connected: bool) -> Option<bool> {
    if err.contains("negentropy not supported")
        || err.contains("unknown negentropy error")
        || (err.contains("negentropy") && err.contains("protocol version"))
    {
        return Some(false);
    }
    if err == "timeout" && relay_was_connected {
        return Some(false);
    }
    None
}

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
    crate::db::scoped(async move {
        use futures_util::stream::{FuturesUnordered, StreamExt};

        let client = crate::state::nostr_client().ok_or("Nostr client not initialized")?;

        let opts = SyncOptions::new()
            .direction(SyncDirection::Down)
            .initial_timeout(timeout)
            .dry_run();

        // Resolve trusted relay URLs to live Relay handles, skipping relays with a
        // fresh no-NIP-77 verdict — each would burn the full timeout for nothing.
        let relay_map = client.relays().await;
        let trusted = crate::state::active_trusted_relays().await;
        let relays: Vec<(String, Relay)> = trusted.iter().filter_map(|url| {
            if neg_supported_cached(url) == Some(false) {
                crate::log_debug!("[Negentropy] {} skipped (cached: no NIP-77)", url);
                return None;
            }
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

        let connect_allowance = crate::relay_request_timeout(Duration::from_secs(3)).min(timeout);
        let mut futs = FuturesUnordered::new();
        for (url, relay) in &relays {
            let url = url.clone();
            let relay = relay.clone();
            let f = filter.clone();
            let items = local_items.clone();
            let o = opts.clone();
            futs.push(async move {
                if !wait_connected(&relay, connect_allowance).await {
                    return (url, None, false);
                }
                let r = tokio::time::timeout(timeout, relay.sync(f).items(items).opts(o)).await;
                let connected = relay.status() == RelayStatus::Connected;
                (url, Some(r), connected)
            });
        }

        let session = crate::state::SessionGuard::capture();
        let mut missing: HashSet<EventId> = HashSet::new();
        while let Some((url, result, connected)) = futs.next().await {
            let Some(result) = result else {
                crate::log_debug!("[Negentropy] {} skipped: not connected", url);
                continue;
            };
            match result {
                Ok(Ok(recon)) => {
                    let n = recon.remote.len();
                    missing.extend(recon.remote);
                    crate::log_debug!("[Negentropy] {} reconciled: {} missing", url, n);
                    record_neg_support(&url, true);
                }
                Ok(Err(e)) => {
                    crate::log_warn!("[Negentropy] {} failed: {}", url, e);
                    if session.is_valid()
                        && classify_neg_sync_error(&e.to_string(), connected) == Some(false)
                    {
                        crate::log_info!("[Negentropy] {} marked no-NIP-77 for 24h", url);
                        record_neg_support(&url, false);
                    }
                }
                Err(_) => crate::log_warn!("[Negentropy] {} timed out", url),
            }
        }

        Ok(missing)
    })
    .await
}

#[cfg(test)]
mod cap_tests {
    use super::*;

    #[test]
    fn classify_detects_deterministic_refusals_regardless_of_connection() {
        for err in [
            "negentropy not supported",
            "unknown negentropy error",
            "negentropy: unsupported protocol version",
        ] {
            assert_eq!(classify_neg_sync_error(err, true), Some(false), "{err}");
            assert_eq!(classify_neg_sync_error(err, false), Some(false), "{err}");
        }
    }

    #[test]
    fn classify_timeout_only_counts_when_connected() {
        assert_eq!(classify_neg_sync_error("timeout", true), Some(false));
        assert_eq!(classify_neg_sync_error("timeout", false), None);
    }

    #[test]
    fn classify_ignores_unrelated_errors() {
        for err in [
            "auth-required: we can't serve DMs to unauthenticated users",
            "relay message too large: size=200000, max_size=131072",
            "not connected",
            "timeout exceeded", // only the SDK's exact Display counts
        ] {
            assert_eq!(classify_neg_sync_error(err, true), None, "{err}");
        }
    }

    #[test]
    fn transient_errors_never_skip_the_archive() {
        assert!(is_transient_sync_error("timeout"));
        assert!(is_transient_sync_error("relay not connected"));
        assert!(is_transient_sync_error("not connected"));
        assert!(is_transient_sync_error("can't send message to the transport dispatcher"));
        assert!(is_transient_sync_error("channel lagged by 558"));
        assert!(!is_transient_sync_error("negentropy not supported"));
        assert!(!is_transient_sync_error("unknown negentropy error"));
        assert!(!is_transient_sync_error("blocked: sync too big"));
    }

    #[test]
    fn cap_entry_parses_and_rejects() {
        assert_eq!(parse_cap_entry("1:1753900000"), Some((true, 1753900000)));
        assert_eq!(parse_cap_entry("0:42"), Some((false, 42)));
        assert_eq!(parse_cap_entry("2:42"), None);
        assert_eq!(parse_cap_entry("1:"), None);
        assert_eq!(parse_cap_entry("1"), None);
        assert_eq!(parse_cap_entry("nonsense"), None);
        assert_eq!(parse_cap_entry(""), None);
    }

    #[test]
    fn cap_key_normalizes_trailing_slash() {
        assert_eq!(cap_key("wss://r.example/"), cap_key("wss://r.example"));
        assert_eq!(cursor_key("wss://r.example/"), cursor_key("wss://r.example"));
    }

}
