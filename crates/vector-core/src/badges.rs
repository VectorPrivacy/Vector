//! Profile badges — fetch, validate, and cache.
//!
//! Badges are not loaded during the critical boot path. The cache is filled
//! once after initial sync (see `refresh_own_badges`) so badge-gated perks
//! (e.g. raised emoji-pack limits) resolve without an on-demand network
//! round-trip. `has_vector_badge` is the cheap synchronous reader used by
//! those gates.

use nostr_sdk::prelude::*;

// Guy Fawkes Day 2025 — V for Vector badge claim window.
const FAWKES_DAY_START: u64 = 1762300800; // 2025-11-05 00:00:00 UTC
const FAWKES_DAY_END: u64 = 1762387200; // 2025-11-06 00:00:00 UTC

/// Per-account settings key: "true" once we've confirmed the Vector badge.
const BADGE_VECTOR_KEY: &str = "badge_vector";
/// Per-account settings key: unix-secs of the last unsuccessful resolve pass.
/// Throttles re-checking for accounts that don't (yet) hold the badge.
const BADGE_CHECK_TS_KEY: &str = "badge_check_ts";
/// Don't re-run the full retry loop more than this often for an account we've
/// already checked without success. The claim window is permanently closed, so
/// a non-holder can never become a holder — frequent restarts shouldn't each
/// trigger a fresh relay sweep.
const RECHECK_COOLDOWN_SECS: u64 = 6 * 3600;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether a kind-30078 event is a valid Fawkes badge claim: right content and
/// a timestamp inside the (half-open) event window. Pure so it's unit-testable.
fn is_valid_fawkes_claim(content: &str, created_at: u64) -> bool {
    content == "fawkes_badge_claimed"
        && created_at >= FAWKES_DAY_START
        && created_at < FAWKES_DAY_END
}

/// Fetch + validate whether `pubkey` holds the V for Vector (Guy Fawkes 2025)
/// badge: a kind-30078 `d=fawkes_2025` claim published within the event window.
///
/// Queries the full relay pool rather than only the trusted relays: the claim
/// was published during the event to whatever relays the holder used, and any
/// single relay (including a trusted one) can be transiently down. Broadest
/// net gives the best chance of locating the permanent claim.
pub async fn has_fawkes_badge(pubkey: &PublicKey) -> Result<bool, String> {
    let client = crate::state::nostr_client().ok_or("Nostr client not initialized")?;
    let filter = Filter::new()
        .author(*pubkey)
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::lowercase(Alphabet::D), "fawkes_2025")
        // > 1 to tolerate relays serving superseded copies of the replaceable
        // event alongside the current one.
        .limit(10);
    let mut events = client
        .stream_events(filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    while let Some(event) = events.next().await {
        if is_valid_fawkes_claim(&event.content, event.created_at.as_secs()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Cached flag for whether we hold the Vector badge. Cheap + synchronous, so
/// safe to call from limit checks. Defaults to false when unset — badge perks
/// stay off until the cache is filled post-sync.
pub fn has_vector_badge() -> bool {
    crate::db::get_sql_setting(BADGE_VECTOR_KEY.to_string())
        .ok()
        .flatten()
        .map(|v| v == "true")
        .unwrap_or(false)
}

/// Fetch our own badges and persist to the per-account cache. Called once
/// after initial sync. The SessionGuard straddles the network fetch so a
/// mid-fetch account swap can't write account A's badge into account B's DB.
pub async fn refresh_own_badges() {
    let session = crate::state::SessionGuard::capture();
    let Some(pk) = crate::state::my_public_key() else {
        crate::log_warn!("[Badges] refresh skipped — no public key");
        return;
    };

    // Sticky: the badge is a permanent achievement, so once confirmed we never
    // re-query (avoids a flaky relay later flipping it off) and never downgrade.
    if has_vector_badge() {
        crate::log_info!("[Badges] vector badge already cached — skipping refresh");
        return;
    }

    // Throttle: skip the relay sweep if we already checked recently without
    // success. The window is closed, so a miss now will still be a miss in an
    // hour — no need to re-sweep on every restart.
    let now = unix_now();
    if let Some(last) = crate::db::get_sql_setting(BADGE_CHECK_TS_KEY.to_string())
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u64>().ok())
    {
        if now.saturating_sub(last) < RECHECK_COOLDOWN_SECS {
            return;
        }
    }

    crate::log_info!(
        "[Badges] resolving own badges for {}…",
        pk.to_bech32().unwrap_or_default()
    );

    // The holding relay (often the user's own) is flaky/overloaded during the
    // heavy sync window, so retry a few times to catch it during a quiet
    // moment. A miss leaves the badge cache untouched (records only the check
    // time for the cooldown); the next boot past the cooldown tries again until
    // it lands once (then sticky-cached forever).
    const ATTEMPTS: u8 = 3;
    for attempt in 1..=ATTEMPTS {
        match has_fawkes_badge(&pk).await {
            Ok(true) => {
                if !session.is_valid() {
                    return;
                }
                crate::log_info!("[Badges] vector badge confirmed (attempt {})", attempt);
                let _ = crate::db::set_sql_setting(BADGE_VECTOR_KEY.to_string(), "true".to_string());
                return;
            }
            Ok(false) => {
                crate::log_info!("[Badges] vector badge not found (attempt {}/{})", attempt, ATTEMPTS);
            }
            Err(e) => {
                crate::log_warn!("[Badges] refresh attempt {}/{} failed: {}", attempt, ATTEMPTS, e);
            }
        }
        if !session.is_valid() {
            return;
        }
        if attempt < ATTEMPTS {
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        }
    }
    // Record the unsuccessful pass so the cooldown applies before re-sweeping.
    if session.is_valid() {
        let _ = crate::db::set_sql_setting(BADGE_CHECK_TS_KEY.to_string(), now.to_string());
    }
    crate::log_info!("[Badges] vector badge not resolved this boot — will retry after cooldown");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fawkes_claim_window_boundaries() {
        // Correct content, inside the window.
        assert!(is_valid_fawkes_claim("fawkes_badge_claimed", FAWKES_DAY_START));
        assert!(is_valid_fawkes_claim("fawkes_badge_claimed", FAWKES_DAY_END - 1));
        // End is exclusive.
        assert!(!is_valid_fawkes_claim("fawkes_badge_claimed", FAWKES_DAY_END));
        // Before the window.
        assert!(!is_valid_fawkes_claim("fawkes_badge_claimed", FAWKES_DAY_START - 1));
        // Wrong / empty content, even inside the window.
        assert!(!is_valid_fawkes_claim("", FAWKES_DAY_START));
        assert!(!is_valid_fawkes_claim("something_else", FAWKES_DAY_START));
    }
}
