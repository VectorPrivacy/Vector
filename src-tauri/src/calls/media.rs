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
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Datagram flag: the sender is muted, the payload is empty, play silence.
const FLAG_MUTED: u16 = 1;

/// Frames buffered before playout starts.
const PREFILL: usize = 2;
/// Deeper than this and playout jumps forward: latency costs more than the gap.
const MAX_DEPTH: usize = 10;
/// How far ahead of the speaker the decoder keeps the play ring.
const PLAY_AHEAD_MS: u32 = 60;
/// Concealed frames in a row before playout stops guessing and waits.
const MAX_PLC_RUN: u32 = 5;

#[derive(Default)]
pub struct MediaStats {
    pub sent: AtomicU64,
    pub send_dropped: AtomicU64,
    pub received: AtomicU64,
    pub lost: AtomicU64,
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
            match conn.send_datagram(datagram) {
                Ok(()) => stats.sent.fetch_add(1, Ordering::Relaxed),
                Err(_) => stats.send_dropped.fetch_add(1, Ordering::Relaxed),
            };
            seq = seq.wrapping_add(1);
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
                stats.lost.fetch_add(1, Ordering::Relaxed);
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
                dec.conceal(&mut pcm).is_ok()
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
}

impl Jitter {
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
        if let Some(f) = self.frames.remove(&next) {
            self.next = Some(next + 1);
            return if f.muted { Pop::Muted } else { Pop::Frame(f.payload) };
        }
        match self.frames.keys().next().copied() {
            Some(min) if min == next + 1 => {
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
                if plc_run < MAX_PLC_RUN {
                    self.next = Some(next + 1);
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
        // Frame 2 lost, 3 arrives: 2 is rebuilt from 3's FEC, then 3 plays.
        j.push(3, false, &frame(3), &stats);
        assert!(matches!(j.pop(0, &stats), Pop::Fec(p) if p == frame(3)));
        assert!(matches!(j.pop(0, &stats), Pop::Frame(p) if p == frame(3)));
        // Late arrival is dropped.
        j.push(2, false, &frame(2), &stats);
        assert_eq!(stats.late.load(Ordering::Relaxed), 1);
        assert!(matches!(j.pop(0, &stats), Pop::Conceal));
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
