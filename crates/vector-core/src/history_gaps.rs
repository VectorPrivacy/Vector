//! Holes in a channel's held history.
//!
//! A relay walk newest-first stops on its page budget. When it stops while still finding
//! messages it had never seen, and messages older than where it stopped are already held, the
//! stretch between is unread: a reader scrolling up from the walk's last page would otherwise
//! step straight onto the older held rows and never see it. Each such stretch is kept here, per
//! channel, until a walk through it reaches held history again.
//!
//! Holding a message proves nothing about the ones before it: live delivery and the boot paint
//! land the newest rows while the stretch under them was never read. So a channel also keeps
//! where its history was last proven whole, and a catch-up walks down to there, not to the
//! first page it already holds.

use serde::{Deserialize, Serialize};

/// A channel with no proof yet trusts nothing newer than this (seconds before now).
const FIRST_LOOK_SECS: u64 = 7 * 24 * 60 * 60;

/// A catch-up proves history up to a little before it began: a message dated just before the
/// walk can reach the relays after it.
const PROOF_SLACK_SECS: u64 = 10 * 60;

/// Messages dated after `lo` and at or before `hi` (Unix seconds) may be missing.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    pub lo: u64,
    pub hi: u64,
}

/// A channel keeps at most this many: past it the oldest go, where a reader rarely reaches.
const MAX_GAPS: usize = 32;

fn key(chat_id: &str) -> String {
    format!("history_gaps:{chat_id}")
}

/// The holes held for `chat_id`, newest first.
pub fn gaps(chat_id: &str) -> Vec<Gap> {
    crate::db::get_sql_setting(key(chat_id))
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

fn save(chat_id: &str, gaps: &[Gap]) {
    let value = if gaps.is_empty() { String::new() } else { serde_json::to_string(gaps).unwrap_or_default() };
    let _ = crate::db::set_sql_setting(key(chat_id), value);
}

/// The hole a scroll up from `before` (Unix seconds) is about to cross.
pub fn gap_at(chat_id: &str, before: u64) -> Option<Gap> {
    find(&gaps(chat_id), before)
}

fn find(gaps: &[Gap], before: u64) -> Option<Gap> {
    // A second's slack: the walk's cursor and a message's own stamp round apart.
    gaps.iter().copied().find(|g| g.lo < before && before <= g.hi + 1)
}

fn proof_key(chat_id: &str) -> String {
    format!("history_proven:{chat_id}")
}

/// Where `chat_id`'s held history was last proven whole (Unix seconds): below it, only the
/// ledger's holes are missing.
pub fn proven(chat_id: &str) -> Option<u64> {
    crate::db::get_sql_setting(proof_key(chat_id)).ok().flatten().and_then(|v| v.parse().ok())
}

/// A catch-up that began at `began` (Unix seconds) has walked to proven history or left a hole
/// for the rest.
pub fn prove(chat_id: &str, began: u64) {
    let at = began.saturating_sub(PROOF_SLACK_SECS);
    if proven(chat_id).is_none_or(|p| at > p) {
        let _ = crate::db::set_sql_setting(proof_key(chat_id), at.to_string());
    }
}

/// How far down a walk from `from` (None: the newest) must go before a page it already holds
/// ends it: a hole's floor inside one, the proof above it, nothing below.
pub fn floor(chat_id: &str, from: Option<u64>, now: u64) -> Option<u64> {
    floor_of(&gaps(chat_id), proven(chat_id), from, now)
}

fn floor_of(gaps: &[Gap], proven: Option<u64>, from: Option<u64>, now: u64) -> Option<u64> {
    if let Some(g) = from.and_then(|f| find(gaps, f)) {
        return Some(g.lo);
    }
    let proof = proven.unwrap_or_else(|| now.saturating_sub(FIRST_LOOK_SECS));
    from.is_none_or(|f| f > proof).then_some(proof)
}

/// The lower bound a walk asks the relays for: its own `since`, raised to its floor, since below
/// the floor was read before.
pub fn reach(since: Option<u64>, floor: Option<u64>) -> Option<u64> {
    match (since, floor) {
        (Some(s), Some(f)) => Some(s.max(f)),
        (s, f) => s.or(f),
    }
}

/// A walk from `from` (None: the newest) ended at `stopped` (its next cursor, None when it reached
/// held history or the start), after its pages were stored. `floor` is the [`floor`] it walked
/// to: held rows above it were unproven, so a hole left there reaches down to it. `held_below(t)`
/// is the newest message held before `t`.
pub fn note_walk(chat_id: &str, from: Option<u64>, stopped: Option<u64>, floor: Option<u64>, held_below: impl Fn(u64) -> Option<u64>) {
    let before = gaps(chat_id);
    let after = walked(&before, from, stopped, floor, held_below);
    if after != before {
        save(chat_id, &after);
    }
}

fn walked(gaps: &[Gap], from: Option<u64>, stopped: Option<u64>, floor: Option<u64>, held_below: impl Fn(u64) -> Option<u64>) -> Vec<Gap> {
    let top = from.unwrap_or(u64::MAX);
    let mut out: Vec<Gap> = Vec::with_capacity(gaps.len() + 1);
    match stopped {
        // Stopped with history still unread: everything down to the newest held row is a hole,
        // whatever this walk covered above it no longer is.
        Some(stop) => {
            let Some(held) = held_below(stop) else {
                // Nothing held below: an ordinary older page reaches it, there is no hole to cross.
                return gaps.iter().copied().filter(|g| g.lo >= top || g.hi < stop).collect();
            };
            let lo = floor.filter(|f| *f < stop).map_or(held, |f| f.min(held));
            let mut hole = Gap { lo, hi: stop };
            for g in gaps {
                if g.hi < hole.lo || g.lo >= top {
                    out.push(*g);
                } else if g.lo < hole.lo {
                    // Reaches further down than this walk's view of it: keep its floor.
                    hole.lo = g.lo;
                }
            }
            out.push(hole);
        }
        // Reached held history: the hole it started in is closed.
        None => out.extend(gaps.iter().copied().filter(|g| from.is_none_or(|f| find(std::slice::from_ref(g), f).is_none()))),
    }
    out.sort_by(|a, b| b.hi.cmp(&a.hi));
    out.truncate(MAX_GAPS);
    out
}

/// Close the hole a scroll from `before` sits in: a walk through it can't move on.
pub fn abandon(chat_id: &str, before: u64) {
    let mut gaps = gaps(chat_id);
    let len = gaps.len();
    gaps.retain(|g| find(std::slice::from_ref(g), before).is_none());
    if gaps.len() != len {
        save(chat_id, &gaps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn g(lo: u64, hi: u64) -> Gap {
        Gap { lo, hi }
    }

    #[test]
    fn a_catch_up_that_runs_out_of_pages_leaves_a_hole_down_to_held_history() {
        // Held up to 1000; the catch-up walked down from now to 5000 and stopped.
        let out = walked(&[], None, Some(5000), None, |t| (t > 1000).then_some(1000));
        assert_eq!(out, vec![g(1000, 5000)]);
        assert_eq!(find(&out, 5000), Some(g(1000, 5000)), "a scroll from the walk's last page crosses it");
        assert_eq!(find(&out, 900), None, "below the hole is ordinary held history");
    }

    #[test]
    fn nothing_held_below_is_no_hole() {
        assert!(walked(&[], None, Some(5000), None, |_| None).is_empty());
    }

    #[test]
    fn a_fill_page_lowers_the_top_and_a_finished_one_closes_it() {
        let held = |t: u64| (t > 1000).then_some(1000);
        let filled = walked(&[g(1000, 5000)], Some(5000), Some(3000), None, held);
        assert_eq!(filled, vec![g(1000, 3000)]);
        let closed = walked(&filled, Some(3000), None, None, held);
        assert!(closed.is_empty());
    }

    #[test]
    fn a_later_catch_up_keeps_an_older_hole_and_adds_its_own() {
        // An old hole at 1000..5000, then held 8000; offline again, walked down to 20000.
        let out = walked(&[g(1000, 5000)], None, Some(20_000), None, |t| (t > 8000).then_some(8000));
        assert_eq!(out, vec![g(8000, 20_000), g(1000, 5000)]);
    }

    #[test]
    fn a_catch_up_that_meets_held_history_leaves_older_holes_alone() {
        assert_eq!(walked(&[g(1000, 5000)], None, None, None, |_| None), vec![g(1000, 5000)]);
    }

    #[test]
    fn a_catch_up_walks_to_the_proof_not_to_the_first_held_page() {
        // Proven to 1000; the newest page is held (live delivery), what lies under it is not.
        assert_eq!(floor_of(&[], Some(1000), None, 50_000), Some(1000));
        // A continuation above the proof keeps going to it; one below needs no floor.
        assert_eq!(floor_of(&[], Some(1000), Some(4000), 50_000), Some(1000));
        assert_eq!(floor_of(&[], Some(1000), Some(900), 50_000), None);
    }

    #[test]
    fn a_walk_inside_a_hole_goes_to_its_floor() {
        assert_eq!(floor_of(&[g(1000, 5000)], Some(8000), Some(4000), 50_000), Some(1000));
    }

    #[test]
    fn with_no_proof_the_last_week_is_unproven() {
        let now = 30 * 24 * 3600;
        assert_eq!(floor_of(&[], None, None, now), Some(now - FIRST_LOOK_SECS));
    }

    #[test]
    fn a_hole_under_unproven_rows_reaches_down_to_the_proof() {
        // Proven to 1000; rows at 6000 and 7000 arrived live. A walk from the top ran out of
        // pages at 8000: the newest held row below it is 7000, but nothing above 1000 is proven.
        let held = |t: u64| [7000, 6000, 1000].into_iter().find(|h| *h < t);
        let out = walked(&[], None, Some(8000), Some(1000), held);
        assert_eq!(out, vec![g(1000, 8000)]);
        // Below the proof, the hole starts at the newest held row as before.
        let out = walked(&[], None, Some(800), Some(1000), |t| (t > 500).then_some(500));
        assert_eq!(out, vec![g(500, 800)]);
    }

    #[test]
    fn a_walk_asks_only_from_its_floor() {
        assert_eq!(reach(None, Some(1000)), Some(1000), "a catch-up from the top: from the proof");
        assert_eq!(reach(Some(1500), Some(1000)), Some(1500));
        assert_eq!(reach(Some(500), Some(1000)), Some(1000));
        assert_eq!(reach(None, None), None, "a back-page in proven history asks for everything older");
    }
}
