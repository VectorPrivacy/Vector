//! Where each relay's copy of a Guestbook has been read to, so a sync asks every relay only for
//! what it hasn't given yet.
//!
//! A relay that doesn't answer keeps its place and is read from there once it's back. One that
//! answers and runs out of plane has given everything dated up to when the read began, which
//! becomes its cursor. What no cursor's window can see (an entry dated in the past on purpose, or
//! published long after its date) a whole read of each relay finds, once a day.


use serde::{Deserialize, Serialize};

/// Each read reaches this far behind its cursor: a slow relay's retry or a skewed clock dates an
/// entry a little before it arrives.
pub(super) const OVERLAP_SECS: u64 = 60 * 60;

/// How often each relay is read whole again. A restored Join and a lifted ban's departure are
/// dated in the past by design, so no cursor's window ever holds them.
pub(super) const REREAD_SECS: u64 = 24 * 60 * 60;

/// One relay's read of one Guestbook plane.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RelayRead {
    /// Everything this relay held dated up to here (Unix seconds) has been read.
    #[serde(default)]
    pub cursor: Option<u64>,
    /// When it was last read whole.
    #[serde(default)]
    pub whole_at: u64,
    /// A read longer than one pass, still going.
    #[serde(default)]
    pub pass: Option<Pass>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Pass {
    /// Where its next page starts.
    pub until: u64,
    /// Its lower bound: 0 for a whole read.
    pub since: u64,
    /// When it began, the cursor it earns.
    pub began: u64,
}

/// The read a relay gets next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub since: u64,
    pub until: Option<u64>,
    pub began: u64,
}

impl RelayRead {
    pub fn plan(&self, now: u64) -> Plan {
        if let Some(p) = self.pass {
            return Plan { since: p.since, until: Some(p.until), began: p.began };
        }
        let since = match self.cursor {
            Some(c) if now.saturating_sub(self.whole_at) < REREAD_SECS => c.saturating_sub(OVERLAP_SECS),
            _ => 0,
        };
        Plan { since, until: None, began: now }
    }

    /// After `plan` ran to the end of the plane (`stopped` None), or ran out of pages at `stopped`.
    pub fn after(self, plan: &Plan, stopped: Option<u64>) -> RelayRead {
        match stopped {
            None => RelayRead {
                cursor: Some(self.cursor.unwrap_or(0).max(plan.began)),
                whole_at: if plan.since == 0 { self.whole_at.max(plan.began) } else { self.whole_at },
                pass: None,
            },
            Some(until) => RelayRead { pass: Some(Pass { until, since: plan.since, began: plan.began }), ..self },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    #[test]
    fn a_relay_never_read_or_read_whole_a_day_ago_is_read_whole() {
        assert_eq!(RelayRead::default().plan(10 * DAY).since, 0);
        let read = RelayRead { cursor: Some(9 * DAY), whole_at: 9 * DAY - 10, pass: None };
        assert_eq!(read.plan(10 * DAY).since, 0, "a day since its last whole read");
        assert_eq!(read.plan(9 * DAY + 600).since, 9 * DAY - OVERLAP_SECS, "otherwise from its cursor, an hour back");
    }

    #[test]
    fn a_read_to_the_end_moves_the_cursor_to_when_it_began_and_a_short_one_keeps_its_place() {
        let read = RelayRead { cursor: Some(10_000), whole_at: 9_000, pass: None };
        let plan = read.plan(10_500);
        assert_eq!(read.after(&plan, None), RelayRead { cursor: Some(10_500), whole_at: 9_000, pass: None });
        let short = read.after(&plan, Some(10_200));
        assert_eq!(short.cursor, Some(10_000), "a read that stopped short earns nothing yet");
        assert_eq!(short.plan(99_999), Plan { since: plan.since, until: Some(10_200), began: 10_500 }, "and goes on from where it stopped");
        assert_eq!(short.after(&short.plan(99_999), None).cursor, Some(10_500), "finishing earns the cursor of when it began");
    }

    #[test]
    fn a_whole_read_finishing_marks_when() {
        let plan = RelayRead::default().plan(5_000);
        assert_eq!(RelayRead::default().after(&plan, None), RelayRead { cursor: Some(5_000), whole_at: 5_000, pass: None });
    }
}
