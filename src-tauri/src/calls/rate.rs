//! Bitrate control for the encoder, fed by QUIC's own loss accounting: every
//! datagram rides in a packet the peer acknowledges, so the sender learns what
//! went missing without a report channel. A lossy link steps the ladder down at
//! once; a clean one climbs back a rung at a time.

/// Bitrates the encoder may use, in kbit/s. Opus picks the audio bandwidth to
/// match: narrowband at the bottom, fullband from the middle up.
pub const LADDER: [u32; 8] = [12, 16, 20, 24, 32, 40, 48, 64];
pub const START_KBPS: u32 = 32;
/// Loss in one second that costs a rung.
const DOWN_AT_PCT: f32 = 4.0;
/// Loss a second must stay under to count towards climbing.
const CLEAN_PCT: f32 = 1.0;
/// Clean seconds in a row before the next rung up.
const CLIMB_AFTER_SECS: u32 = 8;
/// Seconds a rung down is held before loss may take another. Loss is reported
/// about a round trip late, so a step needs time to show in the numbers.
const HOLD_SECS: u32 = 3;
/// The FEC budget follows the loss actually seen, within these bounds.
const FEC_MIN_PCT: u32 = 5;
const FEC_MAX_PCT: u32 = 30;

pub struct RateControl {
    rung: usize,
    clean_secs: u32,
    hold: u32,
    last_sent: u64,
    last_lost: u64,
    /// Loss smoothed over a few seconds, for the FEC budget.
    loss_avg: f32,
}

/// What the encoder should be set to after an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    pub kbps: u32,
    pub fec_pct: u32,
}

impl RateControl {
    pub fn new() -> Self {
        let rung = LADDER.iter().position(|&k| k == START_KBPS).unwrap_or(0);
        Self { rung, clean_secs: 0, hold: 0, last_sent: 0, last_lost: 0, loss_avg: 0.0 }
    }

    /// Sets the baseline so the first observation covers only the next second.
    pub fn prime(&mut self, sent_packets: u64, lost_packets: u64) {
        self.last_sent = sent_packets;
        self.last_lost = lost_packets;
    }

    pub fn kbps(&self) -> u32 {
        LADDER[self.rung]
    }

    /// Loss seen in the last observed second, in percent.
    pub fn loss_pct(&self) -> f32 {
        self.loss_avg
    }

    /// Once a second, with the path's cumulative packet counts. Returns a new
    /// setting when the encoder should change.
    pub fn observe(&mut self, sent_packets: u64, lost_packets: u64) -> Option<Setting> {
        let sent = sent_packets.saturating_sub(self.last_sent);
        let lost = lost_packets.saturating_sub(self.last_lost);
        self.last_sent = sent_packets;
        self.last_lost = lost_packets;
        // Too few packets to judge: a call sends fifty a second.
        if sent < 10 {
            return None;
        }
        let loss = 100.0 * lost as f32 / sent as f32;
        self.loss_avg += (loss - self.loss_avg) * 0.3;
        let before = self.setting();
        if self.hold > 0 {
            self.hold -= 1;
        }
        if loss >= DOWN_AT_PCT {
            self.clean_secs = 0;
            if self.hold == 0 && self.rung > 0 {
                self.rung -= 1;
                self.hold = HOLD_SECS;
            }
        } else if loss < CLEAN_PCT {
            self.clean_secs += 1;
            if self.clean_secs >= CLIMB_AFTER_SECS && self.rung + 1 < LADDER.len() {
                self.rung += 1;
                self.clean_secs = 0;
            }
        } else {
            self.clean_secs = 0;
        }
        let after = self.setting();
        (after != before).then_some(after)
    }

    fn setting(&self) -> Setting {
        let fec = (self.loss_avg.round() as u32).clamp(FEC_MIN_PCT, FEC_MAX_PCT);
        Setting { kbps: self.kbps(), fec_pct: fec }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loss_steps_down_at_once_and_a_clean_link_climbs_slowly() {
        let mut rc = RateControl::new();
        assert_eq!(rc.kbps(), 32);
        // 10% loss for one second: one rung, then held.
        assert_eq!(rc.observe(50, 5).map(|s| s.kbps), Some(24));
        assert_eq!(rc.observe(100, 10), None);
        assert_eq!(rc.observe(150, 15), None);
        assert_eq!(rc.observe(200, 20).map(|s| s.kbps), Some(20));
        // Clean seconds climb one rung every CLIMB_AFTER_SECS.
        let mut sent = 200;
        let mut changes = vec![];
        for _ in 0..(CLIMB_AFTER_SECS * 4) {
            sent += 50;
            if let Some(s) = rc.observe(sent, 20) {
                changes.push(s.kbps);
            }
        }
        assert_eq!(changes, vec![24, 32, 40, 48]);
    }

    #[test]
    fn the_fec_budget_follows_the_loss_and_a_quiet_second_is_ignored() {
        let mut rc = RateControl::new();
        assert_eq!(rc.observe(5, 0), None);
        assert_eq!(rc.observe(55, 10).map(|s| s.fec_pct), Some(6));
        let mut sent = 55;
        let mut last = None;
        for _ in 0..6 {
            sent += 50;
            if let Some(s) = rc.observe(sent, 10 + (sent - 55) / 5) {
                last = Some(s);
            }
        }
        assert_eq!(last.map(|s| s.fec_pct), Some(18));
    }
}
