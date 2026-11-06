//! Holes in a channel's held history.
//!
//! A relay walk newest-first stops on its page budget. When it stops while still finding
//! messages it had never seen, and messages older than where it stopped are already held, the
//! stretch between is unread: a reader scrolling up from the walk's last page would otherwise
//! step straight onto the older held rows and never see it. Each such stretch is kept here, per
//! channel, until a walk through it reaches held history again.

use serde::{Deserialize, Serialize};

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

/// A walk from `from` (None: the newest) ended at `stopped` (its next cursor, None when it reached
/// held history or the start), after its pages were stored. `held_below(t)` is the newest message
/// held before `t`.
pub fn note_walk(chat_id: &str, from: Option<u64>, stopped: Option<u64>, held_below: impl Fn(u64) -> Option<u64>) {
    let before = gaps(chat_id);
    let after = walked(&before, from, stopped, held_below);
    if after != before {
        save(chat_id, &after);
    }
}

fn walked(gaps: &[Gap], from: Option<u64>, stopped: Option<u64>, held_below: impl Fn(u64) -> Option<u64>) -> Vec<Gap> {
    let top = from.unwrap_or(u64::MAX);
    let mut out: Vec<Gap> = Vec::with_capacity(gaps.len() + 1);
    match stopped {
        // Stopped with history still unread: everything down to the newest held row is a hole,
        // whatever this walk covered above it no longer is.
        Some(stop) => {
            let Some(lo) = held_below(stop) else {
                // Nothing held below: an ordinary older page reaches it, there is no hole to cross.
                return gaps.iter().copied().filter(|g| g.lo >= top || g.hi < stop).collect();
            };
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
        let out = walked(&[], None, Some(5000), |t| (t > 1000).then_some(1000));
        assert_eq!(out, vec![g(1000, 5000)]);
        assert_eq!(find(&out, 5000), Some(g(1000, 5000)), "a scroll from the walk's last page crosses it");
        assert_eq!(find(&out, 900), None, "below the hole is ordinary held history");
    }

    #[test]
    fn nothing_held_below_is_no_hole() {
        assert!(walked(&[], None, Some(5000), |_| None).is_empty());
    }

    #[test]
    fn a_fill_page_lowers_the_top_and_a_finished_one_closes_it() {
        let held = |t: u64| (t > 1000).then_some(1000);
        let filled = walked(&[g(1000, 5000)], Some(5000), Some(3000), held);
        assert_eq!(filled, vec![g(1000, 3000)]);
        let closed = walked(&filled, Some(3000), None, held);
        assert!(closed.is_empty());
    }

    #[test]
    fn a_later_catch_up_keeps_an_older_hole_and_adds_its_own() {
        // An old hole at 1000..5000, then held 8000; offline again, walked down to 20000.
        let out = walked(&[g(1000, 5000)], None, Some(20_000), |t| (t > 8000).then_some(8000));
        assert_eq!(out, vec![g(8000, 20_000), g(1000, 5000)]);
    }

    #[test]
    fn a_catch_up_that_meets_held_history_leaves_older_holes_alone() {
        assert_eq!(walked(&[g(1000, 5000)], None, None, |_| None), vec![g(1000, 5000)]);
    }
}
