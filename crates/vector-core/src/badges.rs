//! Profile badges — fetch, validate, and cache.
//!
//! Badges are not loaded during the critical boot path. The cache is filled
//! once after initial sync (see `refresh_own_badges`) so badge-gated perks
//! (e.g. raised emoji-pack limits) resolve without an on-demand network
//! round-trip. `has_vector_badge` is the cheap synchronous reader used by
//! those gates.

use nostr_sdk::prelude::*;
use std::collections::HashSet;
use std::sync::LazyLock;

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

/// Per-account settings key: cached Bug Hunter NIP-58 tier (0-3). Filled by the
/// award fetch; downgraded only on a seen issuer revocation, never on absence.
const BADGE_BUG_HUNTER_TIER_KEY: &str = "badge_bug_hunter_tier";

/// Per-account settings key: comma-separated hex ids of the award events backing
/// the cached tier, so a revocation still resolves after the relays purge the award.
const BADGE_BUG_HUNTER_AWARD_IDS_KEY: &str = "badge_bug_hunter_award_ids";

/// Cached Bug Hunter tier (0-3) for the current account. Cheap + synchronous;
/// 0 until the award fetch fills it.
pub fn bug_hunter_tier() -> u8 {
    crate::db::get_sql_setting(BADGE_BUG_HUNTER_TIER_KEY.to_string())
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u8>().ok())
        .map(|t| t.min(3))
        .unwrap_or(0)
}

/// The account's effective premium tier (0-3): the higher of the Bug Hunter tier
/// and the V for Vector badge (full premium = tier 3). Per-account perks (emoji
/// limits) read this.
pub fn effective_tier() -> u8 {
    bug_hunter_tier().max(if has_vector_badge() { 3 } else { 0 })
}

/// The highest effective tier across ALL accounts on this install. The
/// multi-account cap is device-level (adding a profile spans accounts), so it
/// must not drop when you switch to an un-badged account — unlike the per-account
/// perks. Reads each account's badge state straight from its vector.db.
pub fn max_account_tier() -> u8 {
    let mut max = effective_tier();
    if let Ok(accounts) = crate::db::get_accounts() {
        for npub in accounts {
            max = max.max(read_account_tier(&npub).unwrap_or(0));
        }
    }
    max
}

/// A (possibly non-active) account's effective tier, read read-only from its
/// vector.db settings (plaintext KV). None if the DB/keys are absent or locked.
fn read_account_tier(npub: &str) -> Option<u8> {
    let path = crate::db::account_dir(npub).ok()?.join("vector.db");
    let conn = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let get = |key: &str| -> Option<String> {
        conn.query_row("SELECT value FROM settings WHERE key = ?1", rusqlite::params![key], |r| r.get(0)).ok()
    };
    let bug = get(BADGE_BUG_HUNTER_TIER_KEY).and_then(|v| v.parse::<u8>().ok()).map(|t| t.min(3)).unwrap_or(0);
    let vector = get(BADGE_VECTOR_KEY).map(|v| v == "true").unwrap_or(false);
    Some(bug.max(if vector { 3 } else { 0 }))
}

/// Record the result of an on-demand badge check. When the checked key is our
/// own and the badge is present, persist it (sticky) and emit `badges_updated`
/// so badge-gated perks (raised emoji-pack limits) turn on immediately — this is
/// the safety net for a post-sync `refresh_own_badges` that missed the claim
/// (the holding relay is often flaky during the saturated sync window) and is now
/// sitting in its multi-hour re-check cooldown. An on-demand check runs at a
/// quiet moment, so it lands where the sync-time sweep didn't.
///
/// No-op for other users, a negative result, or an account swap mid-check (the
/// own-key comparison re-reads the *current* account, so a stale key never
/// writes the wrong DB). No awaits, so the read + write stay on one account.
pub fn note_own_badge_confirmed(pubkey: &PublicKey, has_badge: bool) {
    if !has_badge || has_vector_badge() {
        return;
    }
    if crate::state::my_public_key().as_ref() != Some(pubkey) {
        return;
    }
    let _ = crate::db::set_sql_setting(BADGE_VECTOR_KEY.to_string(), "true".to_string());
    crate::log_info!("[Badges] vector badge confirmed via on-demand check");
    crate::traits::emit_event_json(
        "badges_updated",
        serde_json::json!({ "vector": true, "tier": effective_tier(), "bug_hunter": bug_hunter_tier() }),
    );
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

// ── Bug Hunter (NIP-58 tiered, team-awarded) ───────────────────────────────

/// The Vector Team issuer key — the NIP-58 trust root for Bug Hunter badges.
/// Only awards (kind 8) and revocations (kind 5) signed by this key are honored,
/// so a forged award from any other key grants nothing.
const BUG_HUNTER_ISSUER_NPUB: &str =
    "npub1hrujuc08r4zcdtn0u6ts7u7apldcjqgftz0z7stmaaz9hwaf9jxs66f3yh";

static BUG_HUNTER_ISSUER: LazyLock<PublicKey> = LazyLock::new(|| {
    PublicKey::from_bech32(BUG_HUNTER_ISSUER_NPUB)
        .expect("hardcoded Bug Hunter issuer npub must be valid")
});

/// Map a badge-definition `d` identifier to its tier. The display `name` can
/// change freely; the `d` slug is the permanent identity.
fn tier_from_slug(d: &str) -> Option<u8> {
    match d {
        "bug-hunter-tier-1" => Some(1),
        "bug-hunter-tier-2" => Some(2),
        "bug-hunter-tier-3" => Some(3),
        _ => None,
    }
}

/// Parse a NIP-58 `a`-tag coordinate (`30009:<issuer-hex>:<d>`) into a tier, but
/// only when the kind is 30009 AND the author is our trusted issuer — an award
/// pointing at a definition minted under any other key is ignored.
fn tier_from_coord(coord: &str, issuer_hex: &str) -> Option<u8> {
    let mut parts = coord.splitn(3, ':');
    let kind = parts.next()?;
    let author = parts.next()?;
    let d = parts.next()?;
    if kind != "30009" || author != issuer_hex {
        return None;
    }
    tier_from_slug(d)
}

/// Fold awards (event id + tier) and the set of revoked award ids into the tier
/// seen this fetch, plus whether a revocation actually applied to one of our
/// awards. Pure so it's unit-testable. `saw_revocation` is the positive signal
/// that justifies a downgrade (vs. a flaky-relay absence, which must not).
fn fold_bug_hunter(awards: &[(EventId, u8)], revoked: &HashSet<EventId>) -> (u8, bool) {
    let mut seen_tier = 0u8;
    let mut saw_revocation = false;
    for (id, tier) in awards {
        if revoked.contains(id) {
            saw_revocation = true;
        } else {
            seen_tier = seen_tier.max(*tier);
        }
    }
    (seen_tier, saw_revocation)
}

/// Fetch `pubkey`'s raw Bug Hunter standing from the issuer: the seen kind-8
/// awards (id + tier) whose `a` points at one of our tier definitions, plus the
/// set of award ids the issuer has revoked (kind-5). Queries the full pool.
async fn fetch_bug_hunter_raw(pubkey: &PublicKey) -> Result<(Vec<(EventId, u8)>, HashSet<EventId>), String> {
    let client = crate::state::nostr_client().ok_or("Nostr client not initialized")?;
    let issuer = *BUG_HUNTER_ISSUER;
    let issuer_hex = issuer.to_hex();

    // Awards: kind 8 signed by the issuer, p-tagging this user.
    let award_filter = Filter::new()
        .author(issuer)
        .kind(Kind::Custom(8))
        .custom_tag(SingleLetterTag::lowercase(Alphabet::P), pubkey.to_hex())
        .limit(64);
    let mut awards: Vec<(EventId, u8)> = Vec::new();
    let mut stream = client
        .stream_events(award_filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    while let Some(ev) = stream.next().await {
        let coord = ev.tags.iter().find_map(|t| {
            let s = t.as_slice();
            if s.first().map(|k| k == "a").unwrap_or(false) {
                s.get(1).cloned()
            } else {
                None
            }
        });
        if let Some(c) = coord {
            if let Some(tier) = tier_from_coord(&c, &issuer_hex) {
                awards.push((ev.id, tier));
            }
        }
    }

    // Revocations: kind 5 from the issuer (NIP-09); each `e` tag names a revoked award.
    let revoke_filter = Filter::new()
        .author(issuer)
        .kind(Kind::Custom(5))
        .limit(256);
    let mut revoked: HashSet<EventId> = HashSet::new();
    let mut rstream = client
        .stream_events(revoke_filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    while let Some(ev) = rstream.next().await {
        for t in ev.tags.iter() {
            let s = t.as_slice();
            if s.first().map(|k| k == "e").unwrap_or(false) {
                if let Some(id) = s.get(1).and_then(|h| EventId::from_hex(h).ok()) {
                    revoked.insert(id);
                }
            }
        }
    }

    Ok((awards, revoked))
}

/// Highest non-revoked tier + whether a seen award was revoked, for displaying
/// another user's badge. Our own status goes through `refresh_own_bug_hunter`,
/// which also catches revocation of a since-purged award.
pub async fn fetch_bug_hunter_tier(pubkey: &PublicKey) -> Result<(u8, bool), String> {
    let (awards, revoked) = fetch_bug_hunter_raw(pubkey).await?;
    Ok(fold_bug_hunter(&awards, &revoked))
}

fn read_cached_award_ids() -> Vec<EventId> {
    crate::db::get_sql_setting(BADGE_BUG_HUNTER_AWARD_IDS_KEY.to_string())
        .ok()
        .flatten()
        .map(|s| s.split(',').filter_map(|h| EventId::from_hex(h.trim()).ok()).collect())
        .unwrap_or_default()
}

fn write_cached_award_ids(ids: &[EventId]) {
    let csv = ids.iter().map(|id| id.to_hex()).collect::<Vec<_>>().join(",");
    let _ = crate::db::set_sql_setting(BADGE_BUG_HUNTER_AWARD_IDS_KEY.to_string(), csv);
}

/// Resolve + persist our own Bug Hunter tier (called post-sync). Sticky cache:
/// upgrades apply immediately; a downgrade is honored ONLY on a seen revocation,
/// never a mere absent award (flaky relay). The ids of the awards backing the
/// current tier are cached so a revocation still resolves after the relays purge
/// the award itself (NIP-09 deletion). SessionGuard straddles the fetch so a
/// mid-fetch account swap can't write account A's tier into account B.
pub async fn refresh_own_bug_hunter() {
    let session = crate::state::SessionGuard::capture();
    let Some(pk) = crate::state::my_public_key() else {
        return;
    };

    let (awards, revoked) = match fetch_bug_hunter_raw(&pk).await {
        Ok(r) => r,
        Err(e) => {
            crate::log_warn!("[Badges] bug hunter fetch failed: {}", e);
            return;
        }
    };
    if !session.is_valid() {
        return;
    }

    // Highest non-revoked seen tier + the award ids backing it.
    let mut seen_tier = 0u8;
    let mut active_ids: Vec<EventId> = Vec::new();
    let mut seen_revoked = false;
    for (id, tier) in &awards {
        if revoked.contains(id) {
            seen_revoked = true;
        } else {
            seen_tier = seen_tier.max(*tier);
            active_ids.push(*id);
        }
    }
    // Also honor a revocation of an award we previously cached but that the relays
    // have since purged (so it's no longer in `awards` to match directly).
    let cached_revoked = read_cached_award_ids().iter().any(|id| revoked.contains(id));
    let saw_revocation = seen_revoked || cached_revoked;

    // Re-read the current account before any write: never persist A's state into B.
    if crate::state::my_public_key().as_ref() != Some(&pk) {
        return;
    }

    let cached = bug_hunter_tier();
    let new_tier = if seen_tier > cached {
        seen_tier
    } else if seen_tier < cached && saw_revocation {
        seen_tier
    } else {
        cached
    };

    // Remember the awards behind the current tier — but only when we actually saw
    // awards, so a flaky empty fetch never wipes the memory we need for revocation.
    if !awards.is_empty() {
        write_cached_award_ids(&active_ids);
    }
    if new_tier == cached {
        return;
    }
    let _ = crate::db::set_sql_setting(
        BADGE_BUG_HUNTER_TIER_KEY.to_string(),
        new_tier.to_string(),
    );
    crate::log_info!("[Badges] bug hunter tier {} -> {}", cached, new_tier);
    crate::traits::emit_event_json(
        "badges_updated",
        serde_json::json!({ "vector": has_vector_badge(), "tier": effective_tier(), "bug_hunter": bug_hunter_tier() }),
    );
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

    #[test]
    fn bug_hunter_issuer_npub_is_valid() {
        // A malformed hardcoded issuer would panic when the LazyLock forces.
        let _ = *BUG_HUNTER_ISSUER;
        assert!(PublicKey::from_bech32(BUG_HUNTER_ISSUER_NPUB).is_ok());
    }

    #[test]
    fn tier_from_coord_trusts_only_issuer_kind_and_slug() {
        let issuer = "abc123";
        assert_eq!(tier_from_coord("30009:abc123:bug-hunter-tier-1", issuer), Some(1));
        assert_eq!(tier_from_coord("30009:abc123:bug-hunter-tier-2", issuer), Some(2));
        assert_eq!(tier_from_coord("30009:abc123:bug-hunter-tier-3", issuer), Some(3));
        // Forged: a definition minted under another key.
        assert_eq!(tier_from_coord("30009:evil:bug-hunter-tier-3", issuer), None);
        // Wrong kind.
        assert_eq!(tier_from_coord("30008:abc123:bug-hunter-tier-3", issuer), None);
        // Unknown slug.
        assert_eq!(tier_from_coord("30009:abc123:bug-hunter-tier-9", issuer), None);
    }

    #[test]
    fn fold_bug_hunter_highest_non_revoked_with_revocation_flag() {
        let id = |b: u8| EventId::from_hex(&format!("{:02x}", b).repeat(32)).unwrap();
        // No revocations: highest tier, flag clear.
        assert_eq!(fold_bug_hunter(&[(id(1), 1), (id(3), 3), (id(2), 2)], &HashSet::new()), (3, false));
        // Revoke the tier-3 award: tier drops to 2, flag set.
        let revoked: HashSet<EventId> = [id(3)].into_iter().collect();
        assert_eq!(fold_bug_hunter(&[(id(1), 1), (id(3), 3), (id(2), 2)], &revoked), (2, true));
        // No awards: tier 0, no revocation.
        assert_eq!(fold_bug_hunter(&[], &HashSet::new()), (0, false));
    }
}
