//! Per-account emoji "frecency" (most-used) tracker — SQL-backed.
//!
//! One row per distinct emoji in `emoji_usage` (PK `(kind,id)`); a reuse is an
//! in-place UPSERT, so the table holds at most one row per emoji and never grows
//! on repeat use. It's also pruned to the top `MAX_ENTRIES` by score.
//!
//! ## The log-space score (why writes and reads are both O(log n))
//!
//! Each use at time `t` adds `2^((t - EPOCH)/half_life)` points to the emoji's
//! `score`. To rank "right now" you'd multiply every score by the same factor
//! `2^(-(now - EPOCH)/half_life)` — but a uniform factor doesn't change order,
//! so ranking is just `ORDER BY score DESC` off an index — no per-row decay math
//! at read time. Newer uses are worth exponentially more, so old favourites fade
//! automatically as tastes change.
//!
//! A soft cap (`score = MIN(score + inc, CAP·inc)`, enforced in the UPSERT) stops
//! a one-off burst from pinning an emoji forever: a burst saturates at the cap,
//! and a newer use — worth more per point — overtakes it within a half-life or
//! two. `ranked()` converts the stored log-space score back to a normalised
//! 0..CAP "effective" value for the UI.
//!
//! The row shape (kind, id, url, score, last_used) is also the cross-device sync
//! payload: log-space scores from two devices are directly additive (sum =
//! combined usage). Sync itself is future work.

use serde::{Deserialize, Serialize};

/// Compact, indexable kind discriminant.
const KIND_UNICODE: i64 = 0;
const KIND_CUSTOM: i64 = 1;

/// Half-life: an emoji unused for this long loses half its standing.
const HALF_LIFE_SECS: f64 = 21.0 * 24.0 * 3600.0; // 21 days
/// Reference epoch (2025-01-01 UTC) keeps the log-space exponent small. It grows
/// ~17/year and f64 overflows near 2^1024, so this is safe for ~50 years before
/// a rebase would ever be needed; `bump` guards against a non-finite increment.
const EPOCH_SECS: f64 = 1_735_689_600.0;
/// Effective-score ceiling — a burst saturates here instead of running away.
const SCORE_CAP: f64 = 25.0;
/// Keep the table bounded; pruned to the top-N by score after writes.
const MAX_ENTRIES: i64 = 256;

/// A ranked usage row handed to the frontend. `score` is the normalised
/// effective (decayed-to-now) value in `0..=SCORE_CAP`, not the raw log-space
/// store — so the UI can treat it as a plain 0..25 weight.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EmojiUsageEntry {
    pub kind: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub url: Option<String>,
    pub score: f64,
}

/// One use to record (a sent message's distinct emojis, or a reaction).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EmojiUse {
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub url: Option<String>,
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as f64)
        .unwrap_or(0.0)
}

fn kind_code(kind: &str) -> i64 {
    if kind == "custom" { KIND_CUSTOM } else { KIND_UNICODE }
}

fn kind_label(code: i64) -> &'static str {
    if code == KIND_CUSTOM { "custom" } else { "unicode" }
}

/// Log-space points one use contributes at `now`: `2^((now - EPOCH)/half_life)`.
fn use_increment(now: f64) -> f64 {
    ((now - EPOCH_SECS) / HALF_LIFE_SECS).exp2()
}

/// Factor that converts a stored log-space score to its effective value at `now`.
fn decay_to_now(now: f64) -> f64 {
    (-(now - EPOCH_SECS) / HALF_LIFE_SECS).exp2()
}

// ============================================================================
// Writes
// ============================================================================

/// Record several uses in one transaction (one sent message → one DB write).
pub fn bump_batch(uses: &[EmojiUse]) -> Result<(), String> {
    if uses.is_empty() {
        return Ok(());
    }
    let now = now_secs();
    let inc = use_increment(now);
    // Decades-out overflow guard: never write a non-finite score.
    if !inc.is_finite() {
        return Ok(());
    }
    let cap = SCORE_CAP * inc;
    let now_i = now as i64;

    let conn = crate::db::get_write_connection_guard_static()?;
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("emoji_usage tx: {e}"))?;
    {
        let mut up = tx
            .prepare_cached(
                "INSERT INTO emoji_usage (kind, id, url, score, last_used)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(kind, id) DO UPDATE SET
                     score     = MIN(score + ?4, ?6),
                     last_used = ?5,
                     url       = COALESCE(?3, url)",
            )
            .map_err(|e| e.to_string())?;
        for u in uses {
            if u.id.is_empty() {
                continue;
            }
            up.execute(rusqlite::params![kind_code(&u.kind), u.id, u.url, inc, now_i, cap])
                .map_err(|e| format!("emoji_usage upsert: {e}"))?;
        }
        // Bound the table: drop everything outside the top-N by score. Cheap —
        // the score index orders the subquery and a small table makes it a no-op
        // until the cap is actually exceeded.
        tx.execute(
            "DELETE FROM emoji_usage
             WHERE (kind, id) NOT IN (
                 SELECT kind, id FROM emoji_usage ORDER BY score DESC LIMIT ?1
             )",
            rusqlite::params![MAX_ENTRIES],
        )
        .map_err(|e| format!("emoji_usage prune: {e}"))?;
    }
    tx.commit().map_err(|e| format!("emoji_usage commit: {e}"))?;
    Ok(())
}

/// Record one use (reactions; single emoji).
pub fn bump(kind: &str, id: &str, url: Option<&str>) -> Result<(), String> {
    if id.is_empty() {
        return Ok(());
    }
    bump_batch(&[EmojiUse {
        kind: kind.to_string(),
        id: id.to_string(),
        url: url.map(|s| s.to_string()),
    }])
}

// ============================================================================
// Reads
// ============================================================================

/// Ranked usage, highest frecency first. `limit` caps the set (`None` = all).
pub fn ranked(limit: Option<usize>) -> Vec<EmojiUsageEntry> {
    let now = now_secs();
    let decay = decay_to_now(now);
    let lim: i64 = limit.map(|l| l as i64).unwrap_or(-1); // SQLite: LIMIT -1 = no cap

    let conn = match crate::db::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut stmt = match conn
        .prepare_cached("SELECT kind, id, url, score FROM emoji_usage ORDER BY score DESC LIMIT ?1")
    {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = stmt.query_map(rusqlite::params![lim], |row| {
        let kind_c: i64 = row.get(0)?;
        let id: String = row.get(1)?;
        let url: Option<String> = row.get(2)?;
        let raw: f64 = row.get(3)?;
        Ok(EmojiUsageEntry {
            kind: kind_label(kind_c).to_string(),
            id,
            url,
            score: raw * decay, // normalise log-space → effective 0..=CAP
        })
    });
    match rows {
        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: f64 = 24.0 * 3600.0;
    const T0: f64 = EPOCH_SECS + 365.0 * DAY; // ~1 year past the epoch

    /// Pure simulation of the stored log-space score for a sequence of use
    /// times (applies the per-write soft cap), so the algorithm is testable
    /// without a DB. Ranking compares these at a common `now`, and the uniform
    /// decay factor cancels — so comparing raw scores IS the ranking.
    fn simulate(times: &[f64]) -> f64 {
        let mut score = 0.0f64;
        for &t in times {
            let inc = use_increment(t);
            score = (score + inc).min(SCORE_CAP * inc);
        }
        score
    }

    #[test]
    fn kind_code_label_round_trip() {
        assert_eq!(kind_code("unicode"), KIND_UNICODE);
        assert_eq!(kind_code("custom"), KIND_CUSTOM);
        assert_eq!(kind_label(KIND_UNICODE), "unicode");
        assert_eq!(kind_label(KIND_CUSTOM), "custom");
        // Unknown kinds default to unicode rather than panicking.
        assert_eq!(kind_code("???"), KIND_UNICODE);
    }

    #[test]
    fn increment_is_finite_and_monotonic_this_era() {
        let a = use_increment(T0);
        let b = use_increment(T0 + 30.0 * DAY);
        assert!(a.is_finite() && b.is_finite());
        assert!(b > a, "a later use must be worth more in log-space");
    }

    #[test]
    fn effective_of_one_use_is_one() {
        // A single use, decayed back to its own time, is worth exactly 1.
        let s = simulate(&[T0]);
        let eff = s * decay_to_now(T0);
        assert!((eff - 1.0).abs() < 1e-9, "got {eff}");
    }

    #[test]
    fn burst_saturates_at_cap() {
        let s = simulate(&[T0; 1000]);
        let eff = s * decay_to_now(T0);
        assert!((eff - SCORE_CAP).abs() < 1e-6, "burst effective {eff} should hit the cap");
    }

    #[test]
    fn a_burst_does_not_pin_forever_new_favourite_overtakes() {
        // The user's worry: paste one emoji 1000× — does everything else need
        // 1000 uses to win? No. The burst caps; a newer steady habit overtakes.
        let burst = simulate(&[T0; 1000]);
        let mut new_times = Vec::new();
        for d in 0..14 {
            new_times.push(T0 + (30.0 + d as f64) * DAY);
        }
        let fresh = simulate(&new_times);
        // Compared at a common instant the decay factor cancels, so raw score
        // order IS the rank: the 14-use recent favourite beats the 1000× burst.
        assert!(fresh > burst, "fresh {fresh} should outrank burst {burst}");
    }

    #[test]
    fn recent_use_outranks_an_equal_older_one() {
        let older = simulate(&[T0, T0 + DAY, T0 + 2.0 * DAY]);
        let newer = simulate(&[T0 + 10.0 * DAY, T0 + 11.0 * DAY, T0 + 12.0 * DAY]);
        assert!(newer > older, "same use count, more recent should rank higher");
    }
}
