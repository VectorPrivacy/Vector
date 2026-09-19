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

/// One step of a video ladder. `height` caps the picture: a camera is asked for
/// `width` by `height`, a screen is scaled to at most `height` tall (0 keeps it native).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rung {
    pub kbps: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

pub const CAMERA_LADDER: [Rung; 7] = [
    Rung { kbps: 150, width: 320, height: 180, fps: 15 },
    Rung { kbps: 300, width: 480, height: 270, fps: 15 },
    Rung { kbps: 500, width: 640, height: 360, fps: 24 },
    Rung { kbps: 800, width: 640, height: 360, fps: 30 },
    Rung { kbps: 1200, width: 960, height: 540, fps: 30 },
    Rung { kbps: 1800, width: 1280, height: 720, fps: 30 },
    Rung { kbps: 2500, width: 1280, height: 720, fps: 30 },
];
pub const SCREEN_LADDER: [Rung; 6] = [
    Rung { kbps: 300, width: 0, height: 720, fps: 5 },
    Rung { kbps: 600, width: 0, height: 1080, fps: 5 },
    Rung { kbps: 1000, width: 0, height: 1080, fps: 8 },
    Rung { kbps: 1500, width: 0, height: 1440, fps: 10 },
    Rung { kbps: 2500, width: 0, height: 0, fps: 15 },
    Rung { kbps: 4000, width: 0, height: 0, fps: 15 },
];
/// Where a fresh camera or screen starts, and the most a relayed path may carry:
/// the relays are not ours yet, and a video call must not be what fills them.
pub const CAMERA_START: usize = 4;
pub const CAMERA_RELAY_CAP: usize = 4;
pub const SCREEN_START: usize = 3;
pub const SCREEN_RELAY_CAP: usize = 3;

/// Video steps down sooner than audio, since both share one congestion window and
/// the voice must never be what gives.
const VIDEO_DOWN_AT_PCT: f32 = 2.0;
/// Round-trip growth over the call's floor that reads as a filling buffer.
const VIDEO_RTT_BLOAT_MS: u32 = 50;

/// What the video controller sees once a second.
#[derive(Debug, Clone, Copy, Default)]
pub struct VideoObservation {
    pub sent_packets: u64,
    pub lost_packets: u64,
    pub rtt_ms: u32,
    /// Audio datagrams the connection refused, cumulative: any growth is a step down.
    pub audio_send_dropped: u64,
    /// Frames still on the wire at the cap, or a frame reset, in the last second.
    pub backlog: bool,
}

pub struct VideoRate {
    ladder: &'static [Rung],
    rung: usize,
    cap: usize,
    clean_secs: u32,
    hold: u32,
    last_sent: u64,
    last_lost: u64,
    last_audio_dropped: u64,
    min_rtt: u32,
    primed: bool,
}

impl VideoRate {
    pub fn new(ladder: &'static [Rung], start: usize, cap: usize) -> Self {
        let cap = cap.min(ladder.len() - 1);
        Self {
            ladder,
            rung: start.min(cap),
            cap,
            clean_secs: 0,
            hold: 0,
            last_sent: 0,
            last_lost: 0,
            last_audio_dropped: 0,
            min_rtt: u32::MAX,
            primed: false,
        }
    }

    pub fn rung(&self) -> Rung {
        self.ladder[self.rung]
    }

    pub fn ladder(&self) -> &'static [Rung] {
        self.ladder
    }

    /// A new ceiling (the path changed, or the peer said how much it can take).
    /// Returns the rung when the ceiling pushed it down.
    pub fn set_cap(&mut self, cap: usize) -> Option<Rung> {
        self.cap = cap.min(self.ladder.len() - 1);
        if self.rung > self.cap {
            self.rung = self.cap;
            return Some(self.rung());
        }
        None
    }

    /// Once a second. Returns the rung to switch to when it changed.
    pub fn observe(&mut self, obs: VideoObservation) -> Option<Rung> {
        let sent = obs.sent_packets.saturating_sub(self.last_sent);
        let lost = obs.lost_packets.saturating_sub(self.last_lost);
        let audio_dropped = obs.audio_send_dropped.saturating_sub(self.last_audio_dropped);
        self.last_sent = obs.sent_packets;
        self.last_lost = obs.lost_packets;
        self.last_audio_dropped = obs.audio_send_dropped;
        if !self.primed {
            // The first call sets the baselines; the history before it is not this second's.
            self.primed = true;
            return None;
        }
        if obs.rtt_ms > 0 {
            self.min_rtt = self.min_rtt.min(obs.rtt_ms);
        }
        let loss = if sent >= 10 { 100.0 * lost as f32 / sent as f32 } else { 0.0 };
        let bloated = self.min_rtt != u32::MAX && obs.rtt_ms > self.min_rtt + VIDEO_RTT_BLOAT_MS;
        let trouble = loss >= VIDEO_DOWN_AT_PCT || audio_dropped > 0 || bloated || obs.backlog;
        let before = self.rung;
        if self.hold > 0 {
            self.hold -= 1;
        }
        if trouble {
            self.clean_secs = 0;
            if self.hold == 0 && self.rung > 0 {
                self.rung -= 1;
                self.hold = HOLD_SECS;
            }
        } else if loss < CLEAN_PCT {
            self.clean_secs += 1;
            if self.clean_secs >= CLIMB_AFTER_SECS && self.rung < self.cap {
                self.rung += 1;
                self.clean_secs = 0;
            }
        } else {
            self.clean_secs = 0;
        }
        (self.rung != before).then(|| self.rung())
    }
}

#[cfg(test)]
mod video_tests {
    use super::*;

    fn clean(sent: u64) -> VideoObservation {
        VideoObservation { sent_packets: sent, lost_packets: 0, rtt_ms: 40, audio_send_dropped: 0, backlog: false }
    }

    #[test]
    fn a_clean_link_climbs_to_the_cap_and_no_further() {
        let mut r = VideoRate::new(&CAMERA_LADDER, CAMERA_START, CAMERA_RELAY_CAP);
        assert_eq!(r.rung(), CAMERA_LADDER[4]);
        let mut sent = 0;
        let mut changes = 0;
        for _ in 0..40 {
            sent += 100;
            if r.observe(clean(sent)).is_some() {
                changes += 1;
            }
        }
        assert_eq!(changes, 0, "a relayed camera never passes the relay cap");
        assert_eq!(r.set_cap(6), None);
        for _ in 0..8 {
            sent += 100;
            r.observe(clean(sent));
        }
        assert_eq!(r.rung(), CAMERA_LADDER[5]);
    }

    #[test]
    fn audio_drops_and_bufferbloat_step_down_at_once_and_a_cap_pulls_down() {
        let mut r = VideoRate::new(&CAMERA_LADDER, CAMERA_START, 6);
        r.observe(clean(100));
        r.observe(clean(200));
        let dropped = VideoObservation { sent_packets: 300, lost_packets: 0, rtt_ms: 40, audio_send_dropped: 3, backlog: false };
        assert_eq!(r.observe(dropped), Some(CAMERA_LADDER[3]));
        // Held for a few seconds: one bad second costs one rung, not the ladder.
        let bloat = VideoObservation { sent_packets: 400, lost_packets: 0, rtt_ms: 140, audio_send_dropped: 3, backlog: false };
        assert_eq!(r.observe(bloat), None);
        let mut sent = 400;
        for _ in 0..HOLD_SECS {
            sent += 100;
            r.observe(VideoObservation { sent_packets: sent, lost_packets: 0, rtt_ms: 140, audio_send_dropped: 3, backlog: false });
        }
        assert_eq!(r.rung(), CAMERA_LADDER[2]);
        assert_eq!(r.set_cap(0), Some(CAMERA_LADDER[0]));
    }

    #[test]
    fn two_percent_loss_is_enough_for_video() {
        let mut r = VideoRate::new(&SCREEN_LADDER, SCREEN_START, 5);
        r.observe(clean(100));
        let lossy = VideoObservation { sent_packets: 200, lost_packets: 3, rtt_ms: 40, audio_send_dropped: 0, backlog: false };
        assert_eq!(r.observe(lossy), Some(SCREEN_LADDER[2]));
    }
}
