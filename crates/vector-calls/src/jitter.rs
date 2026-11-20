//! The adaptive jitter buffer: holds a few frames so the network's unevenness is
//! smoothed out before the decoder, grows on late arrivals, and takes the latency
//! back when the path settles. Time comes in as milliseconds from the caller.

use super::stats::MediaStats;
use super::FRAME_MS;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

/// Frames buffered before playout starts.
pub const PREFILL: usize = 2;
/// Deeper than this and playout jumps forward: latency costs more than the gap.
pub const MAX_DEPTH: usize = 25;
/// The deepest target the jitter estimate may ask for.
pub const MAX_TARGET: usize = 20;
/// How fast a remembered delay swing fades, in milliseconds per frame received:
/// a 200 ms burst still shapes the target ten seconds later.
const PEAK_DECAY_MS: f32 = 0.4;
/// How long the buffer must sit above its target before it trims a frame. A burst
/// leaves it deep; trimming at once would meet the next burst empty again.
const SHED_AFTER_MS: u64 = 3000;
/// Concealed frames in a row before playout stops guessing and waits. Each one
/// stretches the last sound a little further; past three that reads as a slur,
/// and a clean gap is easier on the ear.
pub const MAX_PLC_RUN: u32 = 3;

/// The quickest-delivery window: 24 buckets of 5 s, two minutes.
const FRESH_BUCKET_MS: u64 = 5_000;
const FRESH_BUCKETS: usize = 24;
/// Headroom over the buffer's own target before a frame counts as stale, and the least it gets.
const STALE_MARGIN_MS: i64 = 250;
const STALE_MIN_MS: i64 = 300;
/// Nothing but stale frames for this long, at a steady lateness: the slower delivery is the
/// path now, not a backlog draining or a queue growing.
const STALE_RESET_MS: u64 = 2_000;
/// How far lateness may move over [`STALE_RESET_MS`] and still read as steady.
const STEADY_MS: i64 = 200;
/// A sender timestamp this far behind its predecessor is a restarted media clock.
const TS_RESET_MS: i64 = 1_000;

/// How late frames arrive against the quickest delivery lately. The sender's timestamp read
/// against our clock: the two clocks never agree, but their gap only grows by queueing, so the
/// smallest gap seen is the path itself and anything above it is time spent waiting somewhere.
#[derive(Default)]
struct Freshness {
    /// The least lateness in each bucket, in ms on an unwrapped sender clock.
    buckets: [Option<i64>; FRESH_BUCKETS],
    at: usize,
    bucket_at: u64,
    /// The last timestamp, raw and unwrapped.
    ts: Option<(u32, i64)>,
    /// When the frames turned stale, and how late they were then.
    stale_since: Option<(u64, i64)>,
}

impl Freshness {
    /// How far behind its quickest recent delivery a frame stamped `ts` arrived at `now`, and
    /// whether that is past `limit`. Starts over on a restarted sender clock, and once nothing
    /// but stale frames has come for [`STALE_RESET_MS`] at a steady lateness.
    fn check(&mut self, ts: u32, now: u64, limit: i64) -> (i64, bool) {
        let unwrapped = match self.ts {
            Some((raw, last)) => last + ts.wrapping_sub(raw) as i32 as i64,
            None => ts as i64,
        };
        let restarted = self.ts.is_some_and(|(_, last)| unwrapped < last - TS_RESET_MS);
        self.ts = Some((ts, unwrapped));
        if restarted {
            self.reset(now);
        }
        self.rotate(now);
        let late = now as i64 - unwrapped;
        let behind = self.buckets.iter().flatten().min().map_or(0, |floor| (late - floor).max(0));
        if behind > limit {
            let (since, then) = *self.stale_since.get_or_insert((now, late));
            if now.saturating_sub(since) < STALE_RESET_MS {
                return (behind, true);
            }
            if (late - then).abs() > STEADY_MS {
                self.stale_since = Some((now, late));
                return (behind, true);
            }
            self.reset(now);
            self.note(late);
            return (0, false);
        }
        self.stale_since = None;
        self.note(late);
        (behind, false)
    }

    fn note(&mut self, late: i64) {
        let b = &mut self.buckets[self.at];
        *b = Some(b.map_or(late, |m| m.min(late)));
    }

    fn rotate(&mut self, now: u64) {
        let steps = now.saturating_sub(self.bucket_at) / FRESH_BUCKET_MS;
        for _ in 0..steps.min(FRESH_BUCKETS as u64) {
            self.at = (self.at + 1) % FRESH_BUCKETS;
            self.buckets[self.at] = None;
        }
        self.bucket_at += steps * FRESH_BUCKET_MS;
    }

    fn reset(&mut self, now: u64) {
        self.buckets = [None; FRESH_BUCKETS];
        self.bucket_at = now;
        self.stale_since = None;
    }
}

pub enum Pop {
    Frame(Vec<u8>),
    /// The frame is missing; rebuild it from the FEC data in its successor.
    Fec(Vec<u8>),
    Muted,
    Conceal,
    Wait,
}

pub struct Buffered {
    payload: Vec<u8>,
    muted: bool,
}

#[derive(Default)]
pub struct Jitter {
    frames: BTreeMap<u64, Buffered>,
    /// Next sequence to play; None until prefilled.
    next: Option<u64>,
    /// Unwrapped sequence of the newest arrival, for the 16-bit wrap.
    newest: Option<u64>,
    /// When the newest frame arrived, in the caller's milliseconds, and its sequence.
    last_arrival: Option<(u64, u64)>,
    /// RFC 3550 style smoothed inter-arrival jitter, in milliseconds.
    jitter_ms: f32,
    /// The largest recent inter-arrival swing, decaying: bursts, not the average.
    peak_ms: f32,
    /// Since when the buffer has been deeper than its target.
    over_since: Option<u64>,
    shed_after: Option<u64>,
    fresh: Freshness,
}

impl Jitter {
    /// Frames waiting, for the depth readout.
    pub fn buffered(&self) -> usize {
        self.frames.len()
    }

    /// Frames to hold: enough for three times the smoothed jitter, or for the
    /// biggest swing seen lately with a little to spare, whichever is more.
    fn target_frames(&self) -> usize {
        let need_ms = (self.jitter_ms * 3.0).max(self.peak_ms * 1.2);
        let frames = (need_ms / FRAME_MS as f32).ceil() as usize;
        (PREFILL + frames).clamp(PREFILL, MAX_TARGET)
    }

    fn unwrap_seq(&mut self, seq: u16) -> u64 {
        let s = match self.newest {
            None => seq as u64,
            Some(newest) => {
                let delta = seq.wrapping_sub(newest as u16) as i16 as i64;
                (newest as i64 + delta).max(0) as u64
            }
        };
        if self.newest.map_or(true, |n| s > n) {
            self.newest = Some(s);
        }
        s
    }

    /// A frame stamped `ts_ms` by its sender arrived at `now_ms`.
    pub fn push(&mut self, seq: u16, ts_ms: u32, muted: bool, payload: &[u8], now_ms: u64, stats: &MediaStats) {
        let s = self.unwrap_seq(seq);
        let now = now_ms;
        if let Some((at, prev_seq)) = self.last_arrival {
            let expected = (s as i64 - prev_seq as i64) as f32 * FRAME_MS as f32;
            let actual = now.saturating_sub(at) as f32;
            let d = (actual - expected).abs();
            self.jitter_ms += (d - self.jitter_ms) / 16.0;
            self.peak_ms = d.max(self.peak_ms - PEAK_DECAY_MS);
            stats.jitter_ms.store(self.jitter_ms as u32, Ordering::Relaxed);
        }
        self.last_arrival = Some((now, s));

        // A frame that waited out a stall somewhere is played as a gap, not as a delay that stays.
        let limit = (self.target_frames() as i64 * FRAME_MS as i64 + STALE_MARGIN_MS).max(STALE_MIN_MS);
        let (behind, stale) = self.fresh.check(ts_ms, now, limit);
        stats.behind_ms.store(behind.clamp(0, u32::MAX as i64) as u32, Ordering::Relaxed);
        if stale {
            stats.stale.fetch_add(1, Ordering::Relaxed);
            return;
        }

        if let Some(next) = self.next {
            if s < next {
                stats.late.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        self.frames.insert(s, Buffered { payload: payload.to_vec(), muted });

        if self.frames.len() > MAX_DEPTH {
            // Jump to the newest PREFILL frames; what came before is late already.
            let keep_from = *self.frames.keys().nth(self.frames.len() - PREFILL).unwrap();
            let dropped = self.frames.range(..keep_from).count() as u64;
            self.frames = self.frames.split_off(&keep_from);
            stats.lost.fetch_add(dropped, Ordering::Relaxed);
            self.next = Some(keep_from);
        }
    }

    /// The next thing to play, decided at `now_ms`.
    pub fn pop(&mut self, plc_run: u32, now_ms: u64, stats: &MediaStats) -> Pop {
        let next = match self.next {
            Some(n) => n,
            None => {
                if self.frames.len() < PREFILL {
                    return Pop::Wait;
                }
                let first = *self.frames.keys().next().unwrap();
                self.next = Some(first);
                first
            }
        };
        if self.frames.len() > self.target_frames() {
            // Underruns stretched the buffer; once that has held for a while, take the
            // latency back a frame at a time.
            let now = now_ms;
            let since = *self.over_since.get_or_insert(now);
            if now.saturating_sub(since) >= self.shed_after.unwrap_or(SHED_AFTER_MS) {
                if let Some(oldest) = self.frames.keys().next().copied() {
                    self.frames.remove(&oldest);
                    stats.shed.fetch_add(1, Ordering::Relaxed);
                    self.next = Some(oldest + 1);
                    self.over_since = Some(now);
                    return self.pop(plc_run, now_ms, stats);
                }
            }
        } else {
            self.over_since = None;
        }
        if let Some(f) = self.frames.remove(&next) {
            self.next = Some(next + 1);
            return if f.muted { Pop::Muted } else { Pop::Frame(f.payload) };
        }
        match self.frames.keys().next().copied() {
            Some(min) if min == next + 1 => {
                // One slot of grace: on a jittery path the frame is usually just behind
                // its successor. Only when the buffer is already full enough is it lost.
                if plc_run == 0 && self.frames.len() < self.target_frames() {
                    return Pop::Conceal;
                }
                self.next = Some(min);
                Pop::Fec(self.frames[&min].payload.clone())
            }
            Some(min) => {
                // A hole wider than one frame: guess twice, then skip to what we have.
                if plc_run < 2 {
                    self.next = Some(next + 1);
                    Pop::Conceal
                } else {
                    stats.lost.fetch_add(min - next, Ordering::Relaxed);
                    self.next = Some(min);
                    self.pop(0, now_ms, stats)
                }
            }
            None => {
                // Nothing has arrived: fill the time without giving up on this frame, so
                // its late arrival still plays and the buffer grows by the underrun.
                if plc_run < MAX_PLC_RUN {
                    Pop::Conceal
                } else {
                    Pop::Wait
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(n: u8) -> Vec<u8> {
        vec![n]
    }

    /// Plays a path out on the playout clock: each frame `(seq, ts, arrival)` is pushed once its
    /// arrival comes, and one frame is popped every 20 ms. Returns each played frame's sequence
    /// with how long after it was sent it played.
    fn play(frames: &[(u16, u32, u64)], until: u64, stats: &MediaStats) -> Vec<(u16, i64)> {
        let mut j = Jitter::default();
        let mut sorted: Vec<_> = frames.to_vec();
        sorted.sort_by_key(|f| f.2);
        let sent: std::collections::HashMap<u16, u32> = frames.iter().map(|f| (f.0, f.1)).collect();
        let (mut i, mut plc, mut out) = (0, 0u32, Vec::new());
        let mut now = 0;
        while now < until {
            while i < sorted.len() && sorted[i].2 <= now {
                let (seq, ts, at) = sorted[i];
                j.push(seq, ts, false, &seq.to_be_bytes(), at, stats);
                i += 1;
            }
            match j.pop(plc, now, stats) {
                Pop::Frame(p) => {
                    plc = 0;
                    let seq = u16::from_be_bytes([p[0], p[1]]);
                    out.push((seq, now as i64 - sent[&seq] as i64));
                }
                Pop::Fec(_) | Pop::Muted => plc = 0,
                Pop::Conceal => plc += 1,
                Pop::Wait => {}
            }
            now += FRAME_MS as u64;
        }
        out
    }

    /// One frame every 20 ms from `ts0` on the sender's clock, arriving `delay(n)` ms after it was sent.
    fn path(n: u16, ts0: u32, delay: impl Fn(u16) -> u64) -> Vec<(u16, u32, u64)> {
        (0..n).map(|s| (s, ts0.wrapping_add(s as u32 * 20), s as u64 * 20 + delay(s))).collect()
    }

    #[test]
    fn a_steady_or_jittery_path_loses_nothing_to_the_guard() {
        for (name, frames) in [
            ("steady", path(1500, 9_000, |_| 60)),
            ("jittery", path(1500, 9_000, |s| 60 + (s as u64 * 37 % 9) * 12)),
            ("spiky", path(1500, 9_000, |s| if s % 50 == 25 { 360 } else { 60 })),
        ] {
            let stats = MediaStats::default();
            play(&frames, 31_000, &stats);
            assert_eq!(stats.stale.load(Ordering::Relaxed), 0, "{name}");
        }
    }

    #[test]
    fn a_backlog_after_a_stall_plays_as_a_gap_not_a_delay() {
        // Clean, then a 3 s stall whose backlog trickles out a little faster than real time:
        // the same shape as a relay's TCP recovering from loss.
        let frames = path(1200, 0, |s| match s {
            0..=299 => 60,
            _ => (60u64 + 3_000).saturating_sub((s as u64 - 300) * 6).max(60),
        });
        let stats = MediaStats::default();
        let played = play(&frames, 24_000, &stats);
        let worst = played.iter().filter(|(s, _)| *s >= 300).map(|(_, lag)| *lag).max().unwrap();
        assert!(stats.stale.load(Ordering::Relaxed) > 0, "the backlog's oldest frames are dropped");
        assert!(worst <= 1_000, "nothing plays a stall's length late: worst {worst} ms");
        let tail = played.iter().rev().take(50).map(|(_, lag)| *lag).max().unwrap();
        assert!(tail <= 60 + (MAX_TARGET as i64) * FRAME_MS as i64, "and back within the buffer's own target: {tail} ms");
    }

    #[test]
    fn a_growing_queue_is_never_taken_for_the_new_path() {
        // Delay builds half a second every second for 20 s, then drains at once.
        let frames = path(1600, 0, |s| match s {
            0..=299 => 60,
            300..=1299 => 60 + (s as u64 - 300) * 10,
            _ => 60,
        });
        let stats = MediaStats::default();
        let played = play(&frames, 32_000, &stats);
        let worst = played.iter().map(|(_, lag)| *lag).max().unwrap();
        assert!(worst <= 1_200, "the queue's growth plays as silence, not as delay: worst {worst} ms");
        assert!(played.iter().any(|(s, _)| *s > 1_500), "and live again once it drains");
    }

    #[test]
    fn a_lasting_slower_path_costs_one_short_gap() {
        let frames = path(1000, 0, |s| if s < 300 { 60 } else { 1_060 });
        let stats = MediaStats::default();
        let played = play(&frames, 22_000, &stats);
        let stale = stats.stale.load(Ordering::Relaxed);
        assert!((90..=110).contains(&stale), "about two seconds dropped, then accepted: {stale}");
        assert!(played.iter().any(|(s, _)| *s > 900), "the call goes on at the new delay");
    }

    #[test]
    fn a_restarted_sender_clock_or_a_wrapped_one_costs_nothing() {
        // The sender's media clock starts over at frame 300; another wraps past u32::MAX.
        let restarted: Vec<_> = path(800, 70_000, |_| 60)
            .into_iter()
            .map(|(s, ts, at)| (s, if s >= 300 { (s as u32 - 300) * 20 } else { ts }, at))
            .collect();
        let wrapped = path(800, u32::MAX - 5_000, |_| 60);
        for (name, frames) in [("restarted", restarted), ("wrapped", wrapped)] {
            let stats = MediaStats::default();
            let played = play(&frames, 17_000, &stats);
            assert_eq!(stats.stale.load(Ordering::Relaxed), 0, "{name}");
            assert!(played.len() > 750, "{name}: {}", played.len());
        }
    }

    #[test]
    fn frames_play_in_order_after_prefill_and_a_gap_uses_fec() {
        let stats = MediaStats::default();
        let mut j = Jitter::default();
        assert!(matches!(j.pop(0, 0, &stats), Pop::Wait));
        j.push(0, 0, false, &frame(0), 0, &stats);
        assert!(matches!(j.pop(0, 0, &stats), Pop::Wait));
        j.push(1, 0, false, &frame(1), 0, &stats);
        assert!(matches!(j.pop(0, 0, &stats), Pop::Frame(p) if p == frame(0)));
        assert!(matches!(j.pop(0, 0, &stats), Pop::Frame(p) if p == frame(1)));
        // Frame 2 lost, 3 arrives: one frame of grace, then 2 is rebuilt from 3's FEC,
        // then 3 plays.
        j.push(3, 0, false, &frame(3), 0, &stats);
        assert!(matches!(j.pop(0, 0, &stats), Pop::Conceal));
        assert!(matches!(j.pop(1, 0, &stats), Pop::Fec(p) if p == frame(3)));
        assert!(matches!(j.pop(0, 0, &stats), Pop::Frame(p) if p == frame(3)));
        // Late arrival is dropped.
        j.push(2, 0, false, &frame(2), 0, &stats);
        assert_eq!(stats.late.load(Ordering::Relaxed), 1);
        // An underrun conceals but keeps waiting for frame 4, which then plays.
        assert!(matches!(j.pop(0, 0, &stats), Pop::Conceal));
        assert_eq!(j.next, Some(4));
        j.push(4, 0, false, &frame(4), 0, &stats);
        assert!(matches!(j.pop(1, 0, &stats), Pop::Frame(p) if p == frame(4)));
    }

    #[test]
    fn a_stretched_buffer_keeps_its_depth_at_first_then_sheds_to_target() {
        let stats = MediaStats::default();
        let mut j = Jitter::default();
        for s in 0..8u16 {
            j.push(s, 0, false, &frame(s as u8), 0, &stats);
        }
        // Instant pushes read as jitter to the estimator; pin it so the target is PREFILL.
        j.jitter_ms = 0.0;
        j.peak_ms = 0.0;
        // A fresh overshoot is kept.
        assert_eq!(j.target_frames(), PREFILL);
        assert!(matches!(j.pop(0, 0, &stats), Pop::Frame(p) if p == frame(0)));
        assert_eq!(stats.shed.load(Ordering::Relaxed), 0);
        // Once the overshoot has lasted long enough, frames go one per pop until in range.
        j.shed_after = Some(0);
        assert!(matches!(j.pop(0, 0, &stats), Pop::Frame(p) if p == frame(6)));
        assert_eq!(stats.shed.load(Ordering::Relaxed), 5);
        assert_eq!(stats.lost.load(Ordering::Relaxed), 0);
        assert_eq!(j.frames.len(), 1);
    }

    #[test]
    fn the_target_grows_with_jitter_and_remembers_bursts() {
        let mut j = Jitter::default();
        j.jitter_ms = 43.0;
        assert_eq!(j.target_frames(), PREFILL + 7);
        j.peak_ms = 300.0;
        assert_eq!(j.target_frames(), PREFILL + 18);
        j.jitter_ms = 1000.0;
        assert_eq!(j.target_frames(), MAX_TARGET);
    }

    #[test]
    fn sequence_wraps_and_deep_buffers_jump_forward() {
        let stats = MediaStats::default();
        let mut j = Jitter::default();
        j.push(65534, 0, false, &frame(1), 0, &stats);
        j.push(65535, 0, false, &frame(2), 0, &stats);
        j.push(0, 0, false, &frame(3), 0, &stats);
        assert!(matches!(j.pop(0, 0, &stats), Pop::Frame(p) if p == frame(1)));
        assert_eq!(j.next, Some(65535));
        for s in 1..=(MAX_DEPTH as u16 + 2) {
            j.push(s, 0, false, &frame(9), 0, &stats);
        }
        assert!(j.frames.len() <= MAX_DEPTH, "{}", j.frames.len());
        assert!(stats.lost.load(Ordering::Relaxed) > 0);
        // Playout resumed at the jump target, not at the frame it was on.
        assert!(j.next.unwrap() > 65537);
    }
}
