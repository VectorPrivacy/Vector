//! Bitrate control for the encoder, fed by QUIC's own loss accounting: every
//! datagram rides in a packet the peer acknowledges, so the sender learns what
//! went missing without a report channel. A lossy link steps the ladder down at
//! once; a clean one climbs back a rung at a time.
//!
//! Loss is not the whole story. A relay carries QUIC inside TCP, where a full pipe
//! shows as delay and never as loss, so the round trip's growth over the path's
//! floor counts as trouble too.

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
/// Round trip over the path's floor that reads as a standing queue: at least this
/// much and at least the floor again, so an intercontinental path's ordinary swing
/// is not mistaken for one.
const QUEUE_MIN_MS: u32 = 300;
/// A queue this deep, or this much loss in a second, is past a rung at a time.
const SEVERE_QUEUE_MS: u32 = 1500;
const SEVERE_LOSS_PCT: f32 = 15.0;
/// Where a severe second puts the voice at once: still wideband.
const SEVERE_KBPS: u32 = 32;

/// Seconds per bucket of a round-trip floor, and how many buckets it keeps.
const FLOOR_BUCKET_SECS: u32 = 10;
const FLOOR_BUCKETS: usize = 30;

/// The lowest round trip a path has shown over the last few minutes: what it costs
/// with no queue of ours in it. Kept per path kind, so falling back to a relay
/// neither borrows the direct path's floor nor relearns its own from a queue we filled.
#[derive(Default)]
pub struct RttFloor {
    paths: [FloorWindow; 2],
}

#[derive(Clone, Copy)]
struct FloorWindow {
    buckets: [u32; FLOOR_BUCKETS],
    at: usize,
    secs: u32,
}

impl Default for FloorWindow {
    fn default() -> Self {
        Self { buckets: [u32::MAX; FLOOR_BUCKETS], at: 0, secs: 0 }
    }
}

impl RttFloor {
    /// Once a second, with the selected path's round trip (0 when unknown). Returns
    /// that path's floor once it has one.
    pub fn observe(&mut self, rtt_ms: u32, relay: bool) -> Option<u32> {
        let w = &mut self.paths[relay as usize];
        if rtt_ms > 0 {
            if w.secs == FLOOR_BUCKET_SECS {
                w.at = (w.at + 1) % FLOOR_BUCKETS;
                w.buckets[w.at] = u32::MAX;
                w.secs = 0;
            }
            w.buckets[w.at] = w.buckets[w.at].min(rtt_ms);
            w.secs += 1;
        }
        let floor = w.buckets.iter().copied().min().unwrap_or(u32::MAX);
        (floor != u32::MAX).then_some(floor)
    }
}

/// A round trip that has grown past what the path's ordinary swing explains.
fn queued(rtt_ms: u32, floor: Option<u32>) -> bool {
    floor.is_some_and(|f| rtt_ms > f.saturating_add(QUEUE_MIN_MS.max(f)))
}

fn severe(rtt_ms: u32, floor: Option<u32>, loss_pct: f32) -> bool {
    loss_pct >= SEVERE_LOSS_PCT || floor.is_some_and(|f| rtt_ms > f.saturating_add(SEVERE_QUEUE_MS))
}

pub struct RateControl {
    rung: usize,
    clean_secs: u32,
    hold: u32,
    last_sent: u64,
    last_lost: u64,
    /// Loss smoothed over a few seconds, for the FEC budget.
    loss_avg: f32,
    floor: RttFloor,
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
        Self { rung, clean_secs: 0, hold: 0, last_sent: 0, last_lost: 0, loss_avg: 0.0, floor: RttFloor::default() }
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

    /// Once a second, with the path's cumulative packet counts and the selected
    /// path's round trip (0 when unknown). Returns a new setting when the encoder
    /// should change.
    pub fn observe(&mut self, sent_packets: u64, lost_packets: u64, rtt_ms: u32, relay: bool) -> Option<Setting> {
        let sent = sent_packets.saturating_sub(self.last_sent);
        let lost = lost_packets.saturating_sub(self.last_lost);
        self.last_sent = sent_packets;
        self.last_lost = lost_packets;
        let floor = self.floor.observe(rtt_ms, relay);
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
        let severe = severe(rtt_ms, floor, loss);
        if severe && self.kbps() > SEVERE_KBPS {
            self.rung = LADDER.iter().position(|&k| k == SEVERE_KBPS).unwrap_or(0);
            self.clean_secs = 0;
            self.hold = HOLD_SECS;
        } else if severe || loss >= DOWN_AT_PCT || queued(rtt_ms, floor) {
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

    /// The encoder setting this ladder stands at now.
    pub fn setting(&self) -> Setting {
        let fec = (self.loss_avg.round() as u32).clamp(FEC_MIN_PCT, FEC_MAX_PCT);
        Setting { kbps: self.kbps(), fec_pct: fec }
    }
}

/// One link's cumulative counts and path, once a second.
#[derive(Debug, Clone, Copy)]
pub struct LinkSample {
    pub id: u64,
    pub sent_packets: u64,
    pub lost_packets: u64,
    pub rtt_ms: u32,
    pub relay: bool,
}

/// A voice Channel encodes each frame once for every peer, so the encoder runs at its
/// weakest link: each link keeps its own ladder, the lowest rung and the largest FEC
/// budget win, and the whole stays under a ceiling a mesh's upload can afford.
pub struct MeshRate {
    links: Vec<(u64, RateControl)>,
    ceiling_kbps: u32,
    current: Option<Setting>,
}

impl MeshRate {
    pub fn new(ceiling_kbps: u32) -> Self {
        Self { links: Vec::new(), ceiling_kbps, current: None }
    }

    /// Once a second, with every live link. Returns a new setting when the encoder
    /// should change; a link seen for the first time only sets its baseline.
    pub fn observe(&mut self, samples: &[LinkSample]) -> Option<Setting> {
        self.links.retain(|(id, _)| samples.iter().any(|s| s.id == *id));
        for s in samples {
            match self.links.iter_mut().find(|(id, _)| *id == s.id) {
                Some((_, rc)) => {
                    rc.observe(s.sent_packets, s.lost_packets, s.rtt_ms, s.relay);
                }
                None => {
                    let mut rc = RateControl::new();
                    rc.prime(s.sent_packets, s.lost_packets);
                    self.links.push((s.id, rc));
                }
            }
        }
        let worst = self
            .links
            .iter()
            .map(|(_, rc)| rc.setting())
            .reduce(|a, b| Setting { kbps: a.kbps.min(b.kbps), fec_pct: a.fec_pct.max(b.fec_pct) })?;
        let next = Setting { kbps: worst.kbps.min(self.ceiling_kbps), ..worst };
        (self.current != Some(next)).then(|| {
            self.current = Some(next);
            next
        })
    }

    /// The worst link's recent loss, in percent.
    pub fn loss_pct(&self) -> f32 {
        self.links.iter().map(|(_, rc)| rc.loss_pct()).fold(0.0, f32::max)
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
        assert_eq!(rc.observe(50, 5, 0, false).map(|s| s.kbps), Some(24));
        assert_eq!(rc.observe(100, 10, 0, false), None);
        assert_eq!(rc.observe(150, 15, 0, false), None);
        assert_eq!(rc.observe(200, 20, 0, false).map(|s| s.kbps), Some(20));
        // Clean seconds climb one rung every CLIMB_AFTER_SECS.
        let mut sent = 200;
        let mut changes = vec![];
        for _ in 0..(CLIMB_AFTER_SECS * 4) {
            sent += 50;
            if let Some(s) = rc.observe(sent, 20, 0, false) {
                changes.push(s.kbps);
            }
        }
        assert_eq!(changes, vec![24, 32, 40, 48]);
    }

    #[test]
    fn the_fec_budget_follows_the_loss_and_a_quiet_second_is_ignored() {
        let mut rc = RateControl::new();
        assert_eq!(rc.observe(5, 0, 0, false), None);
        assert_eq!(rc.observe(55, 10, 0, false).map(|s| s.fec_pct), Some(6));
        let mut sent = 55;
        let mut last = None;
        for _ in 0..6 {
            sent += 50;
            if let Some(s) = rc.observe(sent, 10 + (sent - 55) / 5, 0, false) {
                last = Some(s);
            }
        }
        assert_eq!(last.map(|s| s.fec_pct), Some(18));
    }

    /// Seconds of a clean call at `rtt`, from `sent` packets on; returns the packet count.
    fn clean_secs(rc: &mut RateControl, mut sent: u64, secs: u32, rtt: u32, relay: bool) -> u64 {
        for _ in 0..secs {
            sent += 50;
            rc.observe(sent, 0, rtt, relay);
        }
        sent
    }

    #[test]
    fn a_queue_on_a_relay_steps_the_voice_down_without_any_loss() {
        let mut rc = RateControl::new();
        let sent = clean_secs(&mut rc, 0, 3, 400, true);
        // TCP under the relay turns a full pipe into delay: no packet is ever lost.
        assert_eq!(rc.observe(sent + 50, 0, 900, true).map(|s| s.kbps), Some(24));
    }

    #[test]
    fn an_intercontinental_swing_is_not_a_queue() {
        let mut rc = RateControl::new();
        let mut sent = 0;
        let mut kbps = vec![];
        for i in 0..(CLIMB_AFTER_SECS * 3) {
            sent += 50;
            let rtt = if i % 2 == 0 { 300 } else { 550 };
            if let Some(s) = rc.observe(sent, 0, rtt, true) {
                kbps.push(s.kbps);
            }
        }
        assert_eq!(kbps, vec![40, 48, 64], "300 to 550 ms with no loss is a clean call");
        // A slow path's floor widens its own tolerance: double the floor before it's a queue.
        assert!(!queued(1400, Some(800)));
        assert!(queued(1700, Some(800)));
    }

    #[test]
    fn a_severe_queue_drops_the_voice_to_wideband_at_once_and_keeps_stepping() {
        let mut rc = RateControl::new();
        let sent = clean_secs(&mut rc, 0, CLIMB_AFTER_SECS * 3, 400, true);
        assert_eq!(rc.kbps(), 64);
        assert_eq!(rc.observe(sent + 50, 0, 6000, true).map(|s| s.kbps), Some(SEVERE_KBPS));
        let mut sent = sent + 50;
        for _ in 0..HOLD_SECS {
            sent += 50;
            rc.observe(sent, 0, 6000, true);
        }
        assert_eq!(rc.kbps(), 24, "a queue that stays keeps costing rungs");
        // Severe loss is the same emergency without the delay.
        let mut lossy = RateControl::new();
        let sent = clean_secs(&mut lossy, 0, CLIMB_AFTER_SECS * 3, 0, false);
        assert_eq!(lossy.observe(sent + 50, 10, 0, false).map(|s| s.kbps), Some(SEVERE_KBPS));
    }

    #[test]
    fn the_floor_is_kept_per_path_and_forgets_after_its_window() {
        let mut f = RttFloor::default();
        assert_eq!(f.observe(0, false), None, "an unknown round trip teaches nothing");
        assert_eq!(f.observe(50, false), Some(50));
        assert_eq!(f.observe(400, true), Some(400), "a relay never borrows the direct path's floor");
        assert_eq!(f.observe(6000, true), Some(400), "nor relearns its own from a queue");
        assert_eq!(f.observe(80, false), Some(50));
        for _ in 0..(FLOOR_BUCKET_SECS * FLOOR_BUCKETS as u32) {
            f.observe(700, true);
        }
        assert_eq!(f.observe(700, true), Some(700), "a path that really got slower is relearned");
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

pub static CAMERA_LADDER: [Rung; 7] = [
    Rung { kbps: 150, width: 320, height: 180, fps: 15 },
    Rung { kbps: 300, width: 480, height: 270, fps: 15 },
    Rung { kbps: 500, width: 640, height: 360, fps: 24 },
    Rung { kbps: 800, width: 640, height: 360, fps: 30 },
    Rung { kbps: 1200, width: 960, height: 540, fps: 30 },
    Rung { kbps: 1800, width: 1280, height: 720, fps: 30 },
    Rung { kbps: 2500, width: 1280, height: 720, fps: 30 },
];
/// The top three are for games: 60 frames a second, and enough bits to keep them crisp.
pub static SCREEN_LADDER: [Rung; 9] = [
    Rung { kbps: 300, width: 0, height: 720, fps: 5 },
    Rung { kbps: 600, width: 0, height: 1080, fps: 8 },
    Rung { kbps: 1000, width: 0, height: 1080, fps: 10 },
    Rung { kbps: 1500, width: 0, height: 1440, fps: 15 },
    Rung { kbps: 2500, width: 0, height: 0, fps: 30 },
    Rung { kbps: 4000, width: 0, height: 0, fps: 30 },
    Rung { kbps: 6000, width: 0, height: 1080, fps: 60 },
    Rung { kbps: 10000, width: 0, height: 0, fps: 60 },
    Rung { kbps: 16000, width: 0, height: 0, fps: 60 },
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
/// Clean seconds before video takes the next rung: sooner than the voice, since a
/// rung of video is worth more and costs less to give back.
const VIDEO_CLIMB_AFTER_SECS: u32 = 5;

/// The round trip the video's in-flight budget is sized to: the path's, but never
/// past twice its floor. Beyond that the round trip is our own queue, and a budget
/// that grew with it would keep it full.
pub fn budget_rtt(rtt_ms: u32, floor: Option<u32>) -> u32 {
    floor.map_or(rtt_ms, |f| rtt_ms.min(f.saturating_mul(2)))
}

/// What the video controller sees once a second.
#[derive(Debug, Clone, Copy, Default)]
pub struct VideoObservation {
    pub sent_packets: u64,
    pub lost_packets: u64,
    pub rtt_ms: u32,
    /// The selected path's [`RttFloor`], once it has one.
    pub floor_ms: Option<u32>,
    /// Audio datagrams the connection refused, cumulative: any growth is a step down.
    pub audio_send_dropped: u64,
    /// Frames still on the wire at the cap, or a frame reset, in the last second.
    pub backlog: bool,
}

pub struct VideoRate {
    ladder: &'static [Rung],
    camera: bool,
    rung: usize,
    cap: usize,
    clean_secs: u32,
    hold: u32,
    last_sent: u64,
    last_lost: u64,
    last_audio_dropped: u64,
    primed: bool,
    /// A rung the user chose; the ladder only leaves it to protect the voice.
    pin: Option<usize>,
    /// A frame rate the user chose, over whatever the rung says.
    fps: Option<u32>,
}

impl VideoRate {
    pub fn camera(cap: usize) -> Self {
        Self::new(&CAMERA_LADDER, true, CAMERA_START, cap)
    }

    pub fn screen(cap: usize) -> Self {
        Self::new(&SCREEN_LADDER, false, SCREEN_START, cap)
    }

    pub fn new(ladder: &'static [Rung], camera: bool, start: usize, cap: usize) -> Self {
        let cap = cap.min(ladder.len() - 1);
        Self {
            ladder,
            camera,
            rung: start.min(cap),
            cap,
            clean_secs: 0,
            hold: 0,
            last_sent: 0,
            last_lost: 0,
            last_audio_dropped: 0,
            primed: false,
            pin: None,
            fps: None,
        }
    }

    pub fn rung(&self) -> Rung {
        let mut r = self.ladder[self.rung];
        if let Some(fps) = self.fps {
            r.fps = fps;
        }
        r
    }

    pub fn rungs(&self) -> usize {
        self.ladder.len()
    }

    /// Hold a rung, or let the ladder run again. A held rung may sit above the
    /// path's cap: the cap is a default for the ladder, the pin is the user's word.
    /// Returns the rung when it moved.
    pub fn set_pin(&mut self, pin: Option<usize>) -> Option<Rung> {
        let before = self.rung;
        self.pin = pin.map(|p| p.min(self.ladder.len() - 1));
        if let Some(p) = self.pin {
            self.rung = p;
        } else if self.rung > self.cap {
            self.rung = self.cap;
        }
        self.clean_secs = 0;
        (self.rung != before || pin.is_some()).then(|| self.rung())
    }

    pub fn set_fps(&mut self, fps: Option<u32>) -> Rung {
        self.fps = fps;
        self.rung()
    }

    pub fn is_camera(&self) -> bool {
        self.camera
    }

    /// A new ceiling (the path changed, or the peer said how much it can take).
    /// Returns the rung when the ceiling pushed it down.
    pub fn set_cap(&mut self, cap: usize) -> Option<Rung> {
        self.cap = cap.min(self.ladder.len() - 1);
        if self.pin.is_none() && self.rung > self.cap {
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
        let loss = if sent >= 10 { 100.0 * lost as f32 / sent as f32 } else { 0.0 };
        let bloated = obs.floor_ms.is_some_and(|f| obs.rtt_ms > f.saturating_add(VIDEO_RTT_BLOAT_MS));
        // A queue this deep is already costing the voice, which rides behind it.
        let severe = severe(obs.rtt_ms, obs.floor_ms, loss);
        // A pinned rung answers only to the voice: the one thing video may never cost.
        let trouble = if self.pin.is_some() {
            audio_dropped > 0 || severe
        } else {
            loss >= VIDEO_DOWN_AT_PCT || audio_dropped > 0 || bloated || obs.backlog || severe
        };
        let ceiling = self.pin.unwrap_or(self.cap);
        let before = self.rung;
        if self.hold > 0 {
            self.hold -= 1;
        }
        if severe && self.rung > 0 {
            // Past what a rung at a time can drain.
            self.rung = 0;
            self.clean_secs = 0;
            self.hold = HOLD_SECS;
        } else if trouble {
            self.clean_secs = 0;
            if self.hold == 0 && self.rung > 0 {
                self.rung -= 1;
                self.hold = HOLD_SECS;
            }
        } else if loss < CLEAN_PCT {
            self.clean_secs += 1;
            if self.clean_secs >= VIDEO_CLIMB_AFTER_SECS && self.rung < ceiling {
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
        VideoObservation { sent_packets: sent, lost_packets: 0, rtt_ms: 40, floor_ms: Some(40), audio_send_dropped: 0, backlog: false }
    }

    #[test]
    fn a_clean_link_climbs_to_the_cap_and_no_further() {
        let mut r = VideoRate::camera(CAMERA_RELAY_CAP);
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
        for _ in 0..VIDEO_CLIMB_AFTER_SECS {
            sent += 100;
            r.observe(clean(sent));
        }
        assert_eq!(r.rung(), CAMERA_LADDER[5]);
    }

    #[test]
    fn audio_drops_and_bufferbloat_step_down_at_once_and_a_cap_pulls_down() {
        let mut r = VideoRate::camera(6);
        r.observe(clean(100));
        r.observe(clean(200));
        let dropped = VideoObservation { sent_packets: 300, lost_packets: 0, rtt_ms: 40, floor_ms: Some(40), audio_send_dropped: 3, backlog: false };
        assert_eq!(r.observe(dropped), Some(CAMERA_LADDER[3]));
        // Held for a few seconds: one bad second costs one rung, not the ladder.
        let bloat = VideoObservation { sent_packets: 400, lost_packets: 0, rtt_ms: 140, floor_ms: Some(40), audio_send_dropped: 3, backlog: false };
        assert_eq!(r.observe(bloat), None);
        let mut sent = 400;
        for _ in 0..HOLD_SECS {
            sent += 100;
            r.observe(VideoObservation { sent_packets: sent, lost_packets: 0, rtt_ms: 140, floor_ms: Some(40), audio_send_dropped: 3, backlog: false });
        }
        assert_eq!(r.rung(), CAMERA_LADDER[2]);
        assert_eq!(r.set_cap(0), Some(CAMERA_LADDER[0]));
    }

    #[test]
    fn a_pinned_rung_ignores_loss_but_still_yields_to_the_voice() {
        let mut r = VideoRate::camera(6);
        assert_eq!(r.set_pin(Some(6)), Some(CAMERA_LADDER[6]));
        r.observe(clean(100));
        let lossy = VideoObservation { sent_packets: 200, lost_packets: 10, rtt_ms: 400, floor_ms: Some(40), audio_send_dropped: 0, backlog: true };
        assert_eq!(r.observe(lossy), None);
        let voice = VideoObservation { sent_packets: 300, lost_packets: 10, rtt_ms: 400, floor_ms: Some(40), audio_send_dropped: 1, backlog: true };
        assert_eq!(r.observe(voice), Some(CAMERA_LADDER[5]));
        let mut sent = 300;
        for _ in 0..(HOLD_SECS + VIDEO_CLIMB_AFTER_SECS) {
            sent += 100;
            r.observe(VideoObservation { sent_packets: sent, lost_packets: 10, rtt_ms: 400, floor_ms: Some(40), audio_send_dropped: 1, backlog: true });
        }
        assert_eq!(r.rung(), CAMERA_LADDER[6], "climbs back to the pin once the voice is fine");
        assert_eq!(r.set_fps(Some(15)).fps, 15);
        assert_eq!(r.set_pin(None), None, "unpinning where the ladder already stands changes nothing");
        // A pin above the path's cap holds: the relay cap is a default, not a rule.
        let mut relayed = VideoRate::screen(SCREEN_RELAY_CAP);
        assert_eq!(relayed.set_pin(Some(8)), Some(SCREEN_LADDER[8]));
        assert_eq!(relayed.set_cap(SCREEN_RELAY_CAP), None);
        assert_eq!(relayed.rung(), SCREEN_LADDER[8]);
        assert_eq!(relayed.set_pin(None), Some(SCREEN_LADDER[SCREEN_RELAY_CAP]), "letting go drops back under the cap");
    }

    #[test]
    fn a_severe_queue_drops_video_to_the_bottom_even_when_pinned() {
        let deep = |sent| VideoObservation { sent_packets: sent, lost_packets: 0, rtt_ms: 6000, floor_ms: Some(400), audio_send_dropped: 0, backlog: false };
        let mut r = VideoRate::screen(SCREEN_RELAY_CAP);
        r.observe(clean(100));
        assert_eq!(r.observe(deep(200)), Some(SCREEN_LADDER[0]));
        let mut pinned = VideoRate::screen(SCREEN_RELAY_CAP);
        pinned.set_pin(Some(5));
        pinned.observe(clean(100));
        assert_eq!(pinned.observe(deep(200)), Some(SCREEN_LADDER[0]), "the queue is already costing the voice");
        let mut sent = 200;
        for _ in 0..(HOLD_SECS + VIDEO_CLIMB_AFTER_SECS * 2) {
            sent += 100;
            assert_eq!(pinned.observe(deep(sent)), None, "no climbing back while the queue stands");
        }
    }

    #[test]
    fn the_in_flight_budget_stops_growing_at_twice_the_floor() {
        assert_eq!(budget_rtt(6000, Some(400)), 800);
        assert_eq!(budget_rtt(550, Some(400)), 550);
        assert_eq!(budget_rtt(550, None), 550);
    }

    #[test]
    fn two_percent_loss_is_enough_for_video() {
        let mut r = VideoRate::screen(5);
        r.observe(clean(100));
        let lossy = VideoObservation { sent_packets: 200, lost_packets: 3, rtt_ms: 40, floor_ms: Some(40), audio_send_dropped: 0, backlog: false };
        assert_eq!(r.observe(lossy), Some(SCREEN_LADDER[2]));
    }
}

#[cfg(test)]
mod mesh_tests {
    use super::*;

    fn sample(id: u64, sent: u64, lost: u64) -> LinkSample {
        LinkSample { id, sent_packets: sent, lost_packets: lost, rtt_ms: 80, relay: true }
    }

    #[test]
    fn the_weakest_link_sets_the_rate_and_its_leaving_lifts_it() {
        let mut mesh = MeshRate::new(START_KBPS);
        assert_eq!(mesh.observe(&[sample(1, 0, 0), sample(2, 0, 0)]).map(|s| s.kbps), Some(START_KBPS));
        // Link 2 loses a fifth of a second's fifty frames; link 1 is clean.
        let down = mesh.observe(&[sample(1, 50, 0), sample(2, 50, 10)]).expect("a lossy link moves the encoder");
        assert!(down.kbps < START_KBPS, "the encoder follows the lossy link down");
        assert!(down.fec_pct > FEC_MIN_PCT, "and spends more on FEC");
        let up = mesh.observe(&[sample(1, 100, 0)]).expect("the lossy link left");
        assert_eq!(up.kbps, START_KBPS, "the clean link's rung is back at once");
    }

    #[test]
    fn a_clean_mesh_never_climbs_past_its_ceiling() {
        let mut mesh = MeshRate::new(START_KBPS);
        let mut changes = Vec::new();
        for sec in 0..120u64 {
            if let Some(s) = mesh.observe(&[sample(7, sec * 50, 0)]) {
                changes.push(s.kbps);
            }
        }
        assert!(changes.iter().all(|k| *k <= START_KBPS), "climbing stops at the ceiling: {changes:?}");
    }

    #[test]
    fn a_new_link_counts_from_when_it_joined() {
        let mut mesh = MeshRate::new(START_KBPS);
        mesh.observe(&[sample(1, 0, 0)]);
        // A link whose connection already lost packets before the call saw it.
        assert_eq!(mesh.observe(&[sample(1, 50, 0), sample(3, 9_000, 900)]), None, "its history is not this second's loss");
    }
}
