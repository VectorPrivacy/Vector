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

    /// A frame arrived at `now_ms`.
    pub fn push(&mut self, seq: u16, muted: bool, payload: &[u8], now_ms: u64, stats: &MediaStats) {
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

    #[test]
    fn frames_play_in_order_after_prefill_and_a_gap_uses_fec() {
        let stats = MediaStats::default();
        let mut j = Jitter::default();
        assert!(matches!(j.pop(0, 0, &stats), Pop::Wait));
        j.push(0, false, &frame(0), 0, &stats);
        assert!(matches!(j.pop(0, 0, &stats), Pop::Wait));
        j.push(1, false, &frame(1), 0, &stats);
        assert!(matches!(j.pop(0, 0, &stats), Pop::Frame(p) if p == frame(0)));
        assert!(matches!(j.pop(0, 0, &stats), Pop::Frame(p) if p == frame(1)));
        // Frame 2 lost, 3 arrives: one frame of grace, then 2 is rebuilt from 3's FEC,
        // then 3 plays.
        j.push(3, false, &frame(3), 0, &stats);
        assert!(matches!(j.pop(0, 0, &stats), Pop::Conceal));
        assert!(matches!(j.pop(1, 0, &stats), Pop::Fec(p) if p == frame(3)));
        assert!(matches!(j.pop(0, 0, &stats), Pop::Frame(p) if p == frame(3)));
        // Late arrival is dropped.
        j.push(2, false, &frame(2), 0, &stats);
        assert_eq!(stats.late.load(Ordering::Relaxed), 1);
        // An underrun conceals but keeps waiting for frame 4, which then plays.
        assert!(matches!(j.pop(0, 0, &stats), Pop::Conceal));
        assert_eq!(j.next, Some(4));
        j.push(4, false, &frame(4), 0, &stats);
        assert!(matches!(j.pop(1, 0, &stats), Pop::Frame(p) if p == frame(4)));
    }

    #[test]
    fn a_stretched_buffer_keeps_its_depth_at_first_then_sheds_to_target() {
        let stats = MediaStats::default();
        let mut j = Jitter::default();
        for s in 0..8u16 {
            j.push(s, false, &frame(s as u8), 0, &stats);
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
        j.push(65534, false, &frame(1), 0, &stats);
        j.push(65535, false, &frame(2), 0, &stats);
        j.push(0, false, &frame(3), 0, &stats);
        assert!(matches!(j.pop(0, 0, &stats), Pop::Frame(p) if p == frame(1)));
        assert_eq!(j.next, Some(65535));
        for s in 1..=(MAX_DEPTH as u16 + 2) {
            j.push(s, false, &frame(9), 0, &stats);
        }
        assert!(j.frames.len() <= MAX_DEPTH, "{}", j.frames.len());
        assert!(stats.lost.load(Ordering::Relaxed) > 0);
        // Playout resumed at the jump target, not at the frame it was on.
        assert!(j.next.unwrap() > 65537);
    }
}
