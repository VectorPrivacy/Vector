//! The media engine: microphone to Opus datagrams, datagrams to the speaker.
//!
//! Everything runs at `ENGINE_RATE` mono in 20 ms frames. Two rings cross the
//! audio callbacks: the microphone's samples come in on one, and what the mixer
//! is about to play goes out on another so the echo canceller has its reference.

use super::aec::{EchoCanceller, SpeexAec};
use super::codec::{Decoder, Encoder};
use super::resample::Resampler;
use super::ring::SpscRing;
use super::transport::{pack, unpack};
use super::{AEC_TAIL_MS, ENGINE_RATE, FRAME, FRAME_MS};
use crate::audio_engine::{AudioEngine, LiveLink};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use iroh::endpoint::Connection;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Datagram flag: the sender is muted, the payload is empty, play silence.
const FLAG_MUTED: u16 = 1;

/// Frames buffered before playout starts.
const PREFILL: usize = 2;
/// Deeper than this and playout jumps forward: latency costs more than the gap.
const MAX_DEPTH: usize = 25;
/// The deepest target the jitter estimate may ask for.
const MAX_TARGET: usize = 20;
/// How fast a remembered delay swing fades, in milliseconds per frame received:
/// a 200 ms burst still shapes the target ten seconds later.
const PEAK_DECAY_MS: f32 = 0.4;
/// How long the buffer must sit above its target before it trims a frame. A burst
/// leaves it deep; trimming at once would meet the next burst empty again.
const SHED_AFTER: Duration = Duration::from_secs(3);
/// How far ahead of the speaker the decoder keeps the play ring. Small on purpose:
/// the jitter buffer holds the margin, the ring only covers the callback's stride.
const PLAY_AHEAD_MS: u32 = 30;
/// Concealed frames in a row before playout stops guessing and waits. Each one
/// stretches the last sound a little further; past three that reads as a slur,
/// and a clean gap is easier on the ear.
const MAX_PLC_RUN: u32 = 3;

#[derive(Default)]
pub struct MediaStats {
    pub sent: AtomicU64,
    pub send_dropped: AtomicU64,
    pub received: AtomicU64,
    /// Frames skipped over: a hole wider than FEC covers, or a jump past a backlog.
    pub lost: AtomicU64,
    /// Frames the buffer discarded on purpose to take latency back.
    pub shed: AtomicU64,
    /// Frames rebuilt from the FEC data in their successor.
    pub rebuilt: AtomicU64,
    pub concealed: AtomicU64,
    pub late: AtomicU64,
    pub depth_ms: AtomicU32,
    pub jitter_ms: AtomicU32,
}

pub struct MediaEngine {
    stop: Arc<AtomicBool>,
    pub muted: Arc<AtomicBool>,
    pub stats: Arc<MediaStats>,
    threads: Vec<std::thread::JoinHandle<()>>,
    rx_task: tokio::task::JoinHandle<()>,
}

impl MediaEngine {
    pub fn start(conn: Connection) -> Result<Self, String> {
        let mixer = AudioEngine::get()?;
        let out_rate = mixer.device_sample_rate();
        let link = Arc::new(LiveLink {
            play: SpscRing::new(out_rate as usize),
            tap: SpscRing::new(out_rate as usize),
        });
        mixer.attach_live(Arc::clone(&link))?;

        let stop = Arc::new(AtomicBool::new(false));
        let muted = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(MediaStats::default());
        let jitter = Arc::new(Mutex::new(Jitter::default()));

        // Capture: the thread owns the cpal stream (it is not Send) and runs the encode loop.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let capture = {
            let stop = Arc::clone(&stop);
            let muted = Arc::clone(&muted);
            let stats = Arc::clone(&stats);
            let link = Arc::clone(&link);
            let conn = conn.clone();
            std::thread::Builder::new()
                .name("calls-capture".into())
                .spawn(move || capture_thread(conn, link, out_rate, stop, muted, stats, ready_tx))
                .map_err(|e| e.to_string())?
        };
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                mixer.detach_live();
                stop.store(true, Ordering::SeqCst);
                let _ = capture.join();
                return Err(e);
            }
            Err(_) => {
                mixer.detach_live();
                stop.store(true, Ordering::SeqCst);
                return Err("microphone did not start".into());
            }
        }

        // Receive: datagrams into the jitter buffer until the connection closes.
        let rx_task = {
            let jitter = Arc::clone(&jitter);
            let stats = Arc::clone(&stats);
            let conn = conn.clone();
            vector_core::db::spawn_bound(async move {
                while let Ok(datagram) = conn.read_datagram().await {
                    if let Some(frame) = unpack(&datagram) {
                        stats.received.fetch_add(1, Ordering::Relaxed);
                        let mut j = jitter.lock().unwrap_or_else(|e| e.into_inner());
                        j.push(frame.seq, frame.flags & FLAG_MUTED != 0, frame.payload, &stats);
                    }
                }
            })
        };

        // Playout: decode into the play ring as the speaker drains it.
        let playout = {
            let stop = Arc::clone(&stop);
            let stats = Arc::clone(&stats);
            let link = Arc::clone(&link);
            std::thread::Builder::new()
                .name("calls-playout".into())
                .spawn(move || playout_thread(link, out_rate, jitter, stop, stats))
                .map_err(|e| e.to_string())?
        };

        Ok(Self { stop, muted, stats, threads: vec![capture, playout], rx_task })
    }
}

impl Drop for MediaEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.rx_task.abort();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        if let Ok(mixer) = AudioEngine::get() {
            mixer.detach_live();
        }
    }
}

/// Debug builds only: `VECTOR_CALL_NETSIM=delay_ms,jitter_ms,loss_pct` holds each outgoing
/// datagram for delay plus a random share of jitter, drops the given percentage, and lets
/// the random delays reorder, so a bad link can be heard on a good one.
struct NetSim {
    delay: Duration,
    jitter: Duration,
    loss: f32,
    queue: VecDeque<(Instant, bytes::Bytes)>,
}

impl NetSim {
    fn from_env() -> Option<Self> {
        if !cfg!(debug_assertions) {
            return None;
        }
        let spec = std::env::var("VECTOR_CALL_NETSIM").ok()?;
        let mut parts = spec.split(',').map(|p| p.trim().parse::<f32>().unwrap_or(0.0));
        let delay = parts.next().unwrap_or(0.0);
        let jitter = parts.next().unwrap_or(0.0);
        let loss = parts.next().unwrap_or(0.0);
        log_info!("[CALLS] NetSim on: delay {delay} ms, jitter {jitter} ms, loss {loss}%");
        Some(Self {
            delay: Duration::from_millis(delay as u64),
            jitter: Duration::from_millis(jitter as u64),
            loss: loss / 100.0,
            queue: VecDeque::new(),
        })
    }

    fn offer(&mut self, datagram: bytes::Bytes) {
        if rand::random::<f32>() < self.loss {
            return;
        }
        let extra = self.jitter.mul_f32(rand::random::<f32>());
        self.queue.push_back((Instant::now() + self.delay + extra, datagram));
    }

    /// Sends what is due, in due order, so random delays can overtake each other.
    fn pump(&mut self, conn: &Connection, stats: &MediaStats) {
        let now = Instant::now();
        let mut i = 0;
        while i < self.queue.len() {
            if self.queue[i].0 <= now {
                let (_, d) = self.queue.remove(i).unwrap();
                match conn.send_datagram(d) {
                    Ok(()) => stats.sent.fetch_add(1, Ordering::Relaxed),
                    Err(_) => stats.send_dropped.fetch_add(1, Ordering::Relaxed),
                };
            } else {
                i += 1;
            }
        }
    }
}

fn capture_thread(
    conn: Connection,
    link: Arc<LiveLink>,
    out_rate: u32,
    stop: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    stats: Arc<MediaStats>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let setup = || -> Result<(cpal::Stream, Arc<SpscRing>, u32), String> {
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or("No input device found")?;
        let supported = device.default_input_config().map_err(|e| e.to_string())?;
        let in_rate = supported.sample_rate().0;
        let config: cpal::StreamConfig = supported.into();
        let channels = config.channels.max(1) as usize;
        let ring = Arc::new(SpscRing::new(in_rate as usize));
        let cb_ring = Arc::clone(&ring);
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[f32], _: &_| {
                    let mut mono = [0f32; 512];
                    for chunk in data.chunks(channels * mono.len()) {
                        let mut n = 0;
                        for frame in chunk.chunks(channels) {
                            mono[n] = frame.iter().sum::<f32>() / channels as f32;
                            n += 1;
                        }
                        cb_ring.push(&mono[..n]);
                    }
                },
                |err| eprintln!("[Calls] input stream error: {err}"),
                None,
            )
            .map_err(|e| format!("Failed to build input stream: {e}"))?;
        stream.play().map_err(|e| format!("Failed to start input stream: {e}"))?;
        Ok((stream, ring, in_rate))
    };

    let (_stream, cap_ring, in_rate) = match setup() {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut aec = match SpeexAec::new(ENGINE_RATE, FRAME, AEC_TAIL_MS) {
        Ok(a) => a,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut enc = match Encoder::new() {
        Ok(e) => e,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    // Both rings start together so far-end frame k precedes the echo it causes.
    link.tap.skip(link.tap.len());
    cap_ring.skip(cap_ring.len());
    let _ = ready.send(Ok(()));

    let mut near_rs = Resampler::new(in_rate, ENGINE_RATE);
    let mut far_rs = Resampler::new(out_rate, ENGINE_RATE);
    let mut near = Vec::with_capacity(ENGINE_RATE as usize);
    let mut far = Vec::with_capacity(ENGINE_RATE as usize);
    let mut scratch = vec![0f32; 4096];
    let mut near_i16 = [0i16; FRAME];
    let mut far_i16 = [0i16; FRAME];
    let mut clean = [0i16; FRAME];
    let mut packet = [0u8; 1500];
    let started = Instant::now();
    let mut seq: u16 = 0;
    let mut netsim = NetSim::from_env();
    // Far-end may run ahead of near-end by this much before the excess is dropped.
    let far_slack = FRAME * 5;

    while !stop.load(Ordering::Relaxed) {
        let n = cap_ring.pop(&mut scratch);
        if n > 0 {
            near_rs.process(&scratch[..n], &mut near);
        }
        let m = link.tap.pop(&mut scratch);
        if m > 0 {
            far_rs.process(&scratch[..m], &mut far);
        }
        if far.len() > near.len() + far_slack {
            let excess = far.len() - near.len() - far_slack;
            far.drain(..excess);
        }

        while near.len() >= FRAME {
            for (dst, src) in near_i16.iter_mut().zip(near.drain(..FRAME)) {
                *dst = (src.clamp(-1.0, 1.0) * 32767.0) as i16;
            }
            if far.len() >= FRAME {
                for (dst, src) in far_i16.iter_mut().zip(far.drain(..FRAME)) {
                    *dst = (src.clamp(-1.0, 1.0) * 32767.0) as i16;
                }
            } else {
                far_i16.fill(0);
            }
            aec.process(&near_i16, &far_i16, &mut clean);

            let ts = started.elapsed().as_millis() as u32;
            let datagram = if muted.load(Ordering::Relaxed) {
                pack(seq, ts, FLAG_MUTED, &[])
            } else {
                match enc.encode(&clean, &mut packet) {
                    Ok(len) => pack(seq, ts, 0, &packet[..len]),
                    Err(_) => pack(seq, ts, FLAG_MUTED, &[]),
                }
            };
            match netsim.as_mut() {
                Some(sim) => sim.offer(datagram),
                None => {
                    match conn.send_datagram(datagram) {
                        Ok(()) => stats.sent.fetch_add(1, Ordering::Relaxed),
                        Err(_) => stats.send_dropped.fetch_add(1, Ordering::Relaxed),
                    };
                }
            }
            seq = seq.wrapping_add(1);
        }
        if let Some(sim) = netsim.as_mut() {
            sim.pump(&conn, &stats);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn playout_thread(
    link: Arc<LiveLink>,
    out_rate: u32,
    jitter: Arc<Mutex<Jitter>>,
    stop: Arc<AtomicBool>,
    stats: Arc<MediaStats>,
) {
    let mut dec = match Decoder::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[Calls] {e}");
            return;
        }
    };
    let mut rs = Resampler::new(ENGINE_RATE, out_rate);
    let ahead = (out_rate * PLAY_AHEAD_MS / 1000) as usize;
    let mut pcm = [0i16; FRAME];
    let mut pcm_f32 = [0f32; FRAME];
    let mut resampled = Vec::with_capacity(4096);
    let mut plc_run = 0u32;

    while !stop.load(Ordering::Relaxed) {
        if link.play.len() >= ahead {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let next = {
            let mut j = jitter.lock().unwrap_or_else(|e| e.into_inner());
            let next = j.pop(plc_run, &stats);
            let depth = j.frames.len() as u32 * FRAME_MS + (link.play.len() as u32 * 1000 / out_rate);
            stats.depth_ms.store(depth, Ordering::Relaxed);
            next
        };
        let decoded = match next {
            Pop::Frame(p) => {
                plc_run = 0;
                dec.decode(&p, &mut pcm).is_ok()
            }
            Pop::Fec(successor) => {
                plc_run = 0;
                stats.rebuilt.fetch_add(1, Ordering::Relaxed);
                dec.decode_fec(&successor, &mut pcm).is_ok()
            }
            Pop::Muted => {
                plc_run = 0;
                pcm.fill(0);
                true
            }
            Pop::Conceal => {
                plc_run += 1;
                stats.concealed.fetch_add(1, Ordering::Relaxed);
                let ok = dec.conceal(&mut pcm).is_ok();
                // Fade the guesses out so a run ends in silence, not a held vowel.
                let gain = 1.0 - plc_run as f32 / (MAX_PLC_RUN as f32 + 1.0);
                for s in pcm.iter_mut() {
                    *s = (*s as f32 * gain) as i16;
                }
                ok
            }
            Pop::Wait => {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
        };
        if !decoded {
            pcm.fill(0);
        }
        for (dst, src) in pcm_f32.iter_mut().zip(pcm.iter()) {
            *dst = *src as f32 / 32768.0;
        }
        rs.process(&pcm_f32, &mut resampled);
        link.play.push(&resampled);
        resampled.clear();
    }
}

enum Pop {
    Frame(Vec<u8>),
    /// The frame is missing; rebuild it from the FEC data in its successor.
    Fec(Vec<u8>),
    Muted,
    Conceal,
    Wait,
}

struct Buffered {
    payload: Vec<u8>,
    muted: bool,
}

#[derive(Default)]
struct Jitter {
    frames: BTreeMap<u64, Buffered>,
    /// Next sequence to play; None until prefilled.
    next: Option<u64>,
    /// Unwrapped sequence of the newest arrival, for the 16-bit wrap.
    newest: Option<u64>,
    last_arrival: Option<(Instant, u64)>,
    /// RFC 3550 style smoothed inter-arrival jitter, in milliseconds.
    jitter_ms: f32,
    /// The largest recent inter-arrival swing, decaying: bursts, not the average.
    peak_ms: f32,
    /// Since when the buffer has been deeper than its target.
    over_since: Option<Instant>,
    shed_after: Option<Duration>,
}

impl Jitter {
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

    fn push(&mut self, seq: u16, muted: bool, payload: &[u8], stats: &MediaStats) {
        let s = self.unwrap_seq(seq);
        let now = Instant::now();
        if let Some((at, prev_seq)) = self.last_arrival {
            let expected = (s as i64 - prev_seq as i64) as f32 * FRAME_MS as f32;
            let actual = now.duration_since(at).as_secs_f32() * 1000.0;
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

    fn pop(&mut self, plc_run: u32, stats: &MediaStats) -> Pop {
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
            let now = Instant::now();
            let since = *self.over_since.get_or_insert(now);
            if now.duration_since(since) >= self.shed_after.unwrap_or(SHED_AFTER) {
                if let Some(oldest) = self.frames.keys().next().copied() {
                    self.frames.remove(&oldest);
                    stats.shed.fetch_add(1, Ordering::Relaxed);
                    self.next = Some(oldest + 1);
                    self.over_since = Some(now);
                    return self.pop(plc_run, stats);
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
                    self.pop(0, stats)
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
        assert!(matches!(j.pop(0, &stats), Pop::Wait));
        j.push(0, false, &frame(0), &stats);
        assert!(matches!(j.pop(0, &stats), Pop::Wait));
        j.push(1, false, &frame(1), &stats);
        assert!(matches!(j.pop(0, &stats), Pop::Frame(p) if p == frame(0)));
        assert!(matches!(j.pop(0, &stats), Pop::Frame(p) if p == frame(1)));
        // Frame 2 lost, 3 arrives: one frame of grace, then 2 is rebuilt from 3's FEC,
        // then 3 plays.
        j.push(3, false, &frame(3), &stats);
        assert!(matches!(j.pop(0, &stats), Pop::Conceal));
        assert!(matches!(j.pop(1, &stats), Pop::Fec(p) if p == frame(3)));
        assert!(matches!(j.pop(0, &stats), Pop::Frame(p) if p == frame(3)));
        // Late arrival is dropped.
        j.push(2, false, &frame(2), &stats);
        assert_eq!(stats.late.load(Ordering::Relaxed), 1);
        // An underrun conceals but keeps waiting for frame 4, which then plays.
        assert!(matches!(j.pop(0, &stats), Pop::Conceal));
        assert_eq!(j.next, Some(4));
        j.push(4, false, &frame(4), &stats);
        assert!(matches!(j.pop(1, &stats), Pop::Frame(p) if p == frame(4)));
    }

    #[test]
    fn a_stretched_buffer_keeps_its_depth_at_first_then_sheds_to_target() {
        let stats = MediaStats::default();
        let mut j = Jitter::default();
        for s in 0..8u16 {
            j.push(s, false, &frame(s as u8), &stats);
        }
        // Instant pushes read as jitter to the estimator; pin it so the target is PREFILL.
        j.jitter_ms = 0.0;
        j.peak_ms = 0.0;
        // A fresh overshoot is kept.
        assert_eq!(j.target_frames(), PREFILL);
        assert!(matches!(j.pop(0, &stats), Pop::Frame(p) if p == frame(0)));
        assert_eq!(stats.shed.load(Ordering::Relaxed), 0);
        // Once the overshoot has lasted long enough, frames go one per pop until in range.
        j.shed_after = Some(Duration::ZERO);
        assert!(matches!(j.pop(0, &stats), Pop::Frame(p) if p == frame(6)));
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
        j.push(65534, false, &frame(1), &stats);
        j.push(65535, false, &frame(2), &stats);
        j.push(0, false, &frame(3), &stats);
        assert!(matches!(j.pop(0, &stats), Pop::Frame(p) if p == frame(1)));
        assert_eq!(j.next, Some(65535));
        for s in 1..=(MAX_DEPTH as u16 + 2) {
            j.push(s, false, &frame(9), &stats);
        }
        assert!(j.frames.len() <= MAX_DEPTH, "{}", j.frames.len());
        assert!(stats.lost.load(Ordering::Relaxed) > 0);
        // Playout resumed at the jump target, not at the frame it was on.
        assert!(j.next.unwrap() > 65537);
    }
}
