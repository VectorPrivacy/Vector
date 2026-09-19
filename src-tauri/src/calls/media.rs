//! The media engine: microphone to Opus datagrams, datagrams to the speaker.
//!
//! Everything runs at `ENGINE_RATE` mono in 20 ms frames. Two rings cross the
//! audio callbacks: the microphone's samples come in on one, and what the mixer
//! is about to play goes out on another so the echo canceller has its reference.

use super::aec::{EchoCanceller, SpeexAec};
use super::codec::{Decoder, Encoder, ShareDecoder, ShareEncoder};
use super::settings::AudioSettings;
use super::declick::Declicker;
use super::jitter::{Jitter, Pop, MAX_PLC_RUN};
use super::rate::RateControl;
use super::resample::Resampler;
use super::ring::SpscRing;
use super::settings;
use super::transport::{pack, unpack};
use super::{AEC_TAIL_MS, ENGINE_RATE, FRAME, FRAME_MS};
pub use super::stats::MediaStats;
use crate::audio_engine::{AudioEngine, LiveLink};
use cpal::traits::{DeviceTrait, StreamTrait};
use iroh::endpoint::Connection;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Milliseconds on a clock that starts with the process: what the jitter buffer times by.
fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Datagram flag: the sender is muted, the payload is empty, play silence.
const FLAG_MUTED: u16 = 1;
/// Datagram flag: a frame of the shared screen's sound, stereo, its own sequence.
const FLAG_SHARE: u16 = 2;

/// A shared screen's sound as the webview captures it: interleaved samples at the
/// capture's own rate, waiting for the share thread. Lives on the call, not the
/// engine, so a device change does not lose what was in flight.
pub struct ShareInput {
    ring: SpscRing,
    rate: std::sync::atomic::AtomicU32,
    channels: std::sync::atomic::AtomicU32,
    pub active: AtomicBool,
}

impl ShareInput {
    pub fn new() -> Self {
        Self {
            // Two seconds of stereo at 48 kHz.
            ring: SpscRing::new(192_000),
            rate: std::sync::atomic::AtomicU32::new(48_000),
            channels: std::sync::atomic::AtomicU32::new(2),
            active: AtomicBool::new(false),
        }
    }

    pub fn push(&self, rate: u32, channels: u8, samples: &[f32]) {
        self.rate.store(rate, Ordering::Relaxed);
        self.channels.store(channels as u32, Ordering::Relaxed);
        self.ring.push(samples);
    }
}

/// How far ahead of the speaker the decoder keeps the play ring. Small on purpose:
/// the jitter buffer holds the margin, the ring only covers the callback's stride.
const PLAY_AHEAD_MS: u32 = 30;

/// A frame's loudness on a 0 to 1 scale: -60 dBFS is silence, 0 dBFS full.
fn level_of(frame: &[i16]) -> f32 {
    let sum: f64 = frame.iter().map(|s| (*s as f64).powi(2)).sum();
    let rms = (sum / frame.len().max(1) as f64).sqrt() / 32768.0;
    if rms <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0) as f32
}

pub struct MediaEngine {
    stop: Arc<AtomicBool>,
    pub muted: Arc<AtomicBool>,
    pub stats: Arc<MediaStats>,
    pub link: Arc<LiveLink>,
    on_mixer: bool,
    threads: Vec<std::thread::JoinHandle<()>>,
    rx_task: Option<tokio::task::JoinHandle<()>>,
}

impl MediaEngine {
    /// With a connection this is a call; without one it is a microphone test: the
    /// same capture chain, levels only, nothing sent and nothing played.
    /// `stats` carries the counters across a restart: the liveness check and the
    /// stats UI hold one Arc for the call's life, and fresh ones would read as a
    /// peer gone silent.
    pub fn start(
        conn: Option<Connection>,
        volume: f32,
        stats: Option<Arc<MediaStats>>,
        share: Option<Arc<ShareInput>>,
        share_volume: f32,
    ) -> Result<Self, String> {
        let mixer = AudioEngine::get()?;
        let out_rate = mixer.device_sample_rate();
        let link = Arc::new(LiveLink {
            play: SpscRing::new(out_rate as usize),
            tap: SpscRing::new(out_rate as usize),
            gain: std::sync::atomic::AtomicU32::new(volume.to_bits()),
            share_play: SpscRing::new(out_rate as usize * 2),
            share_tap: SpscRing::new(out_rate as usize),
            share_gain: std::sync::atomic::AtomicU32::new(share_volume.to_bits()),
        });
        let on_mixer = conn.is_some();
        if on_mixer {
            mixer.attach_live(Arc::clone(&link))?;
        }
        settings::load();

        let stop = Arc::new(AtomicBool::new(false));
        let muted = Arc::new(AtomicBool::new(false));
        let stats = stats.unwrap_or_else(|| Arc::new(MediaStats::default()));
        let jitter = Arc::new(Mutex::new(Jitter::default()));
        let share_jitter = Arc::new(Mutex::new(Jitter::default()));

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
                if on_mixer {
                    mixer.detach_live();
                }
                stop.store(true, Ordering::SeqCst);
                let _ = capture.join();
                return Err(e);
            }
            Err(_) => {
                if on_mixer {
                    mixer.detach_live();
                }
                stop.store(true, Ordering::SeqCst);
                return Err("microphone did not start".into());
            }
        }

        let Some(conn) = conn else {
            return Ok(Self { stop, muted, stats, link, on_mixer, threads: vec![capture], rx_task: None });
        };

        // Receive: datagrams into the jitter buffers until the connection closes; the
        // voice and the shared screen's sound each keep their own.
        let rx_task = {
            let jitter = Arc::clone(&jitter);
            let share_jitter = Arc::clone(&share_jitter);
            let stats = Arc::clone(&stats);
            let conn = conn.clone();
            vector_core::db::spawn_bound(async move {
                while let Ok(datagram) = conn.read_datagram().await {
                    if let Some(frame) = unpack(&datagram) {
                        if frame.flags & FLAG_SHARE != 0 {
                            stats.share_received.fetch_add(1, Ordering::Relaxed);
                            let mut j = share_jitter.lock().unwrap_or_else(|e| e.into_inner());
                            j.push(frame.seq, false, frame.payload, now_ms(), &stats);
                        } else {
                            stats.received.fetch_add(1, Ordering::Relaxed);
                            let mut j = jitter.lock().unwrap_or_else(|e| e.into_inner());
                            j.push(frame.seq, frame.flags & FLAG_MUTED != 0, frame.payload, now_ms(), &stats);
                        }
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

        // The shared screen's sound, both ways.
        let share_out = {
            let stop = Arc::clone(&stop);
            let stats = Arc::clone(&stats);
            let link = Arc::clone(&link);
            std::thread::Builder::new()
                .name("calls-share-playout".into())
                .spawn(move || share_playout_thread(link, out_rate, share_jitter, stop, stats))
                .map_err(|e| e.to_string())?
        };
        let mut threads = vec![capture, playout, share_out];
        if let Some(share) = share {
            let stop = Arc::clone(&stop);
            let stats = Arc::clone(&stats);
            let link = Arc::clone(&link);
            let conn = conn.clone();
            let t = std::thread::Builder::new()
                .name("calls-share".into())
                .spawn(move || share_thread(conn, share, link, out_rate, stop, stats))
                .map_err(|e| e.to_string())?;
            threads.push(t);
        }

        Ok(Self { stop, muted, stats, link, on_mixer, threads, rx_task: Some(rx_task) })
    }
}

impl Drop for MediaEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.rx_task.take() {
            t.abort();
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        if self.on_mixer {
            if let Ok(mixer) = AudioEngine::get() {
                mixer.detach_live();
            }
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
    conn: Option<Connection>,
    link: Arc<LiveLink>,
    out_rate: u32,
    stop: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    stats: Arc<MediaStats>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let dead = Arc::new(AtomicBool::new(false));
    let setup = || -> Result<(cpal::Stream, Arc<SpscRing>, u32), String> {
        let device = crate::audio_devices::resolve_input().ok_or("No input device found")?;
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
                {
                    let dead = Arc::clone(&dead);
                    move |err| {
                        eprintln!("[Calls] input stream error: {err}");
                        dead.store(true, Ordering::Relaxed);
                    }
                },
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
    let mut declick = Declicker::new(ENGINE_RATE);
    let mut far_rs = Resampler::new(out_rate, ENGINE_RATE);
    let mut near = Vec::with_capacity(ENGINE_RATE as usize);
    let mut far = Vec::with_capacity(ENGINE_RATE as usize);
    let mut scratch = vec![0f32; 4096];
    let mut near_i16 = [0i16; FRAME];
    let mut far_i16 = [0i16; FRAME];
    let mut clean = [0i16; FRAME];
    let mut packet = [0u8; 1500];
    let started = Instant::now();
    let mut seq: u16 = stats.next_seq.load(Ordering::Relaxed) as u16;
    let mut netsim = NetSim::from_env();
    let mut rate = RateControl::new();
    if let Some(conn) = conn.as_ref() {
        let net = conn.stats();
        rate.prime(net.udp_tx.datagrams, net.lost_packets);
    }
    stats.bitrate_kbps.store(rate.kbps(), Ordering::Relaxed);
    let mut rate_tick = Instant::now();
    // Far-end may run ahead of near-end by this much before the excess is dropped.
    let far_slack = FRAME * 5;

    while !stop.load(Ordering::Relaxed) {
        if dead.swap(false, Ordering::Relaxed) {
            // The microphone vanished: the mixer's watchdog reopens everything.
            AudioEngine::kick();
        }
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
            declick.process(&mut near_i16);
            stats.clicks_cut.store(declick.triggers() as u64, Ordering::Relaxed);
            aec.configure(settings::current());
            aec.process(&near_i16, &far_i16, &mut clean);
            stats.mic_level.store(level_of(&clean).to_bits(), Ordering::Relaxed);

            let Some(conn) = conn.as_ref() else {
                seq = seq.wrapping_add(1);
                stats.next_seq.store(seq as u32, Ordering::Relaxed);
                continue;
            };
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
            stats.next_seq.store(seq as u32, Ordering::Relaxed);
        }
        if let (Some(sim), Some(conn)) = (netsim.as_mut(), conn.as_ref()) {
            sim.pump(conn, &stats);
        }
        if let Some(conn) = conn.as_ref() {
            if rate_tick.elapsed() >= Duration::from_secs(1) {
                rate_tick = Instant::now();
                // Every QUIC packet goes out as one UDP datagram, so this is packets sent.
                let net = conn.stats();
                if let Some(setting) = rate.observe(net.udp_tx.datagrams, net.lost_packets) {
                    if enc.set_rate(setting.kbps, setting.fec_pct).is_ok() {
                        stats.bitrate_kbps.store(setting.kbps, Ordering::Relaxed);
                    }
                }
                stats.net_loss.store(rate.loss_pct().to_bits(), Ordering::Relaxed);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The shared screen's sound out: the webview's capture, minus everything this
/// process played (the voices most of all), as stereo Opus on its own datagrams.
/// A loopback capture is a digital mix, so the canceller has no room to contend
/// with, only the delay between the mixer's output and the capture's return.
fn share_thread(
    conn: Connection,
    share: Arc<ShareInput>,
    link: Arc<LiveLink>,
    out_rate: u32,
    stop: Arc<AtomicBool>,
    stats: Arc<MediaStats>,
) {
    let (mut aec_l, mut aec_r) = match (SpeexAec::new(ENGINE_RATE, FRAME, AEC_TAIL_MS), SpeexAec::new(ENGINE_RATE, FRAME, AEC_TAIL_MS)) {
        (Ok(l), Ok(r)) => (l, r),
        _ => {
            eprintln!("[Calls] share canceller unavailable");
            return;
        }
    };
    // Cancel only: gain riding and noise suppression would chew on music.
    let music = AudioSettings { auto_gain: false, echo_cancel: true, noise_suppress: false };
    aec_l.configure(music);
    aec_r.configure(music);
    let mut enc = match ShareEncoder::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[Calls] {e}");
            return;
        }
    };
    let mut src_rate = 0u32;
    let mut rs_l = Resampler::new(48_000, ENGINE_RATE);
    let mut rs_r = Resampler::new(48_000, ENGINE_RATE);
    let mut far_rs = Resampler::new(out_rate, ENGINE_RATE);
    let mut scratch = vec![0f32; 8192];
    let (mut in_l, mut in_r) = (Vec::with_capacity(8192), Vec::with_capacity(8192));
    let (mut left, mut right, mut far) = (Vec::with_capacity(ENGINE_RATE as usize), Vec::with_capacity(ENGINE_RATE as usize), Vec::with_capacity(ENGINE_RATE as usize));
    let (mut near_l, mut near_r, mut far_i16) = ([0i16; FRAME], [0i16; FRAME], [0i16; FRAME]);
    let (mut clean_l, mut clean_r) = ([0i16; FRAME], [0i16; FRAME]);
    let mut stereo = [0i16; FRAME * 2];
    let mut packet = [0u8; 4000];
    let started = Instant::now();
    let mut seq: u16 = stats.next_share_seq.load(Ordering::Relaxed) as u16;
    let mut was_active = false;
    let far_slack = FRAME * 5;

    while !stop.load(Ordering::Relaxed) {
        let active = share.active.load(Ordering::Relaxed);
        if !active {
            // Nothing to send: keep both rings from filling, and resync when it starts.
            share.ring.skip(share.ring.len());
            link.share_tap.skip(link.share_tap.len());
            was_active = false;
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        if !was_active {
            was_active = true;
            left.clear();
            right.clear();
            far.clear();
        }
        let rate = share.rate.load(Ordering::Relaxed);
        if rate != src_rate {
            src_rate = rate;
            rs_l = Resampler::new(rate, ENGINE_RATE);
            rs_r = Resampler::new(rate, ENGINE_RATE);
        }
        let channels = share.channels.load(Ordering::Relaxed).clamp(1, 2) as usize;
        let n = share.ring.pop(&mut scratch);
        if n > 0 {
            in_l.clear();
            in_r.clear();
            for frame in scratch[..n - n % channels].chunks(channels) {
                in_l.push(frame[0]);
                in_r.push(frame[channels - 1]);
            }
            rs_l.process(&in_l, &mut left);
            rs_r.process(&in_r, &mut right);
        }
        let m = link.share_tap.pop(&mut scratch);
        if m > 0 {
            far_rs.process(&scratch[..m], &mut far);
        }
        if far.len() > left.len() + far_slack {
            let excess = far.len() - left.len() - far_slack;
            far.drain(..excess);
        }
        while left.len() >= FRAME && right.len() >= FRAME {
            for (dst, src) in near_l.iter_mut().zip(left.drain(..FRAME)) {
                *dst = (src.clamp(-1.0, 1.0) * 32767.0) as i16;
            }
            for (dst, src) in near_r.iter_mut().zip(right.drain(..FRAME)) {
                *dst = (src.clamp(-1.0, 1.0) * 32767.0) as i16;
            }
            if far.len() >= FRAME {
                for (dst, src) in far_i16.iter_mut().zip(far.drain(..FRAME)) {
                    *dst = (src.clamp(-1.0, 1.0) * 32767.0) as i16;
                }
            } else {
                far_i16.fill(0);
            }
            aec_l.process(&near_l, &far_i16, &mut clean_l);
            aec_r.process(&near_r, &far_i16, &mut clean_r);
            for i in 0..FRAME {
                stereo[i * 2] = clean_l[i];
                stereo[i * 2 + 1] = clean_r[i];
            }
            let ts = started.elapsed().as_millis() as u32;
            if let Ok(len) = enc.encode(&stereo, &mut packet) {
                match conn.send_datagram(pack(seq, ts, FLAG_SHARE, &packet[..len])) {
                    Ok(()) => stats.share_sent.fetch_add(1, Ordering::Relaxed),
                    Err(_) => stats.send_dropped.fetch_add(1, Ordering::Relaxed),
                };
            }
            seq = seq.wrapping_add(1);
            stats.next_share_seq.store(seq as u32, Ordering::Relaxed);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The peer's shared screen sound in: stereo Opus off its own jitter buffer, onto the
/// mixer's second live source, so it plays through the same output as everything
/// else and lands in the echo canceller's reference like everything else.
fn share_playout_thread(
    link: Arc<LiveLink>,
    out_rate: u32,
    jitter: Arc<Mutex<Jitter>>,
    stop: Arc<AtomicBool>,
    stats: Arc<MediaStats>,
) {
    let mut dec = match ShareDecoder::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[Calls] {e}");
            return;
        }
    };
    let mut rs_l = Resampler::new(ENGINE_RATE, out_rate);
    let mut rs_r = Resampler::new(ENGINE_RATE, out_rate);
    let ahead = (out_rate * PLAY_AHEAD_MS / 1000) as usize * 2;
    let mut pcm = [0i16; FRAME * 2];
    let (mut l, mut r) = ([0f32; FRAME], [0f32; FRAME]);
    let (mut out_l, mut out_r) = (Vec::with_capacity(4096), Vec::with_capacity(4096));
    let mut interleaved = Vec::with_capacity(8192);
    let mut plc_run = 0u32;

    while !stop.load(Ordering::Relaxed) {
        if link.share_play.len() >= ahead {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let next = jitter.lock().unwrap_or_else(|e| e.into_inner()).pop(plc_run, now_ms(), &stats);
        let decoded = match next {
            Pop::Frame(p) => {
                plc_run = 0;
                dec.decode(&p, &mut pcm).is_ok()
            }
            Pop::Fec(successor) => {
                plc_run = 0;
                dec.decode_fec(&successor, &mut pcm).is_ok()
            }
            Pop::Muted => {
                plc_run = 0;
                pcm.fill(0);
                true
            }
            Pop::Conceal => {
                plc_run += 1;
                let ok = dec.conceal(&mut pcm).is_ok();
                let gain = 1.0 - plc_run as f32 / (MAX_PLC_RUN as f32 + 1.0);
                for s in pcm.iter_mut() {
                    *s = (*s as f32 * gain) as i16;
                }
                ok
            }
            Pop::Wait => {
                // Silence between shares must not be concealed into a held tone; the
                // buffer simply waits, and the mixer plays nothing for this source.
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
        };
        if !decoded {
            pcm.fill(0);
        }
        for i in 0..FRAME {
            l[i] = pcm[i * 2] as f32 / 32768.0;
            r[i] = pcm[i * 2 + 1] as f32 / 32768.0;
        }
        rs_l.process(&l, &mut out_l);
        rs_r.process(&r, &mut out_r);
        interleaved.clear();
        for i in 0..out_l.len().min(out_r.len()) {
            interleaved.push(out_l[i]);
            interleaved.push(out_r[i]);
        }
        link.share_play.push(&interleaved);
        out_l.clear();
        out_r.clear();
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
            let next = j.pop(plc_run, now_ms(), &stats);
            let depth = j.buffered() as u32 * FRAME_MS + (link.play.len() as u32 * 1000 / out_rate);
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
        stats.peer_level.store(level_of(&pcm).to_bits(), Ordering::Relaxed);
        for (dst, src) in pcm_f32.iter_mut().zip(pcm.iter()) {
            *dst = *src as f32 / 32768.0;
        }
        rs.process(&pcm_f32, &mut resampled);
        link.play.push(&resampled);
        resampled.clear();
    }
}
