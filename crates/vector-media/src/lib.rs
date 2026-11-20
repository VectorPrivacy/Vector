//! Headless media for Vector: hear and speak in voice Channels without a microphone or a
//! speaker. Audio comes in as 48 kHz mono PCM, one 20 ms frame per speaker at a time, and goes
//! out from PCM a program queues, mono or stereo, encoded with Opus and paced in real time.
//!
//! The SDK's `voice` feature is built on this; use it directly only for a client of your own.
//! [`install`] once per process, then every voice Channel joined through `vector-core` gets an
//! engine: [`engine_in`] finds a call's.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use iroh::endpoint::Connection;
use tokio::sync::{mpsc, Notify};
use vector_core::calls::platform::{AudioStart, CallAudio, CallPlatform, LinkConn};
use vector_core::calls::voice::VoiceMedia;
use vector_core::calls::wire::{pack, unpack};
pub use vector_core::calls::{ENGINE_RATE, FRAME};

/// A frame carries no audio: its sender is muted or silent.
const FLAG_MUTED: u16 = 1;
/// A frame of a shared screen's sound, not the voice.
const FLAG_SHARE: u16 = 2;
/// How many heard frames may wait for the program before the oldest is dropped: 40 s of one
/// speaker, or a few seconds of a busy call.
const HEARD_QUEUE: usize = 2048;
/// The Opus bitrate a call starts at, mono or stereo, in kbit/s. A mesh uploads it once per peer.
pub const DEFAULT_KBPS: u32 = 64;
/// The lowest bitrate a call accepts, in kbit/s.
pub const MIN_KBPS: u32 = 8;
/// The highest bitrate a call accepts, in kbit/s: about 800 bytes a frame, under [`MAX_PACKET`]
/// with room for a burst.
pub const MAX_KBPS: u32 = 320;
/// The most one encoded frame may take: with its header it fits the smallest QUIC datagram.
const MAX_PACKET: usize = 1000;
/// After someone speaks, this many frames of their quiet (2 s) reach the program as silence, so
/// it can tell where they paused; then nothing until they speak again.
const QUIET_TAIL: u32 = 100;

/// One 20 ms frame someone in the call said, decoded.
#[derive(Debug, Clone)]
pub struct Heard {
    /// Their iroh endpoint id. `vector-core` names its owner: `calls::voice::peer_author`.
    pub endpoint: [u8; 32],
    /// [`FRAME`] samples, 48 kHz mono. Empty when they stopped being heard for good (they left,
    /// lost Speak, or the call deafened): whatever they were saying has ended.
    pub pcm: Vec<i16>,
}

/// What the call heard, waiting for the program: the oldest goes first when it falls behind.
#[derive(Default)]
struct HeardQueue {
    frames: Mutex<(VecDeque<Heard>, bool)>,
    ready: Notify,
}

impl HeardQueue {
    fn push(&self, heard: Heard) {
        {
            let mut held = self.frames.lock().unwrap_or_else(|e| e.into_inner());
            if held.1 {
                return;
            }
            // An end of speech outlives the frames around it: without it, two utterances merge.
            if held.0.len() >= HEARD_QUEUE {
                let oldest = held.0.iter().position(|h| !h.pcm.is_empty()).unwrap_or(0);
                held.0.remove(oldest);
            }
            held.0.push_back(heard);
        }
        self.ready.notify_one();
    }

    fn close(&self) {
        self.frames.lock().unwrap_or_else(|e| e.into_inner()).1 = true;
        self.ready.notify_waiters();
        self.ready.notify_one();
    }

    async fn next(&self) -> Option<Heard> {
        loop {
            let ready = self.ready.notified();
            {
                let mut held = self.frames.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(h) = held.0.pop_front() {
                    return Some(h);
                }
                if held.1 {
                    return None;
                }
            }
            ready.await;
        }
    }
}

struct Peer {
    conn: Connection,
    rx: tokio::task::JoinHandle<()>,
    audible: Arc<AtomicBool>,
    level: Arc<AtomicU32>,
}

/// What a call is yet to say: interleaved stereo, mono queued as the same sample on both sides.
#[derive(Default)]
struct Outgoing {
    samples: VecDeque<i16>,
    /// A left sample whose right has not been queued yet: the next `say_stereo` completes it.
    half: Option<i16>,
}

impl Outgoing {
    fn mono(&mut self, pcm: &[i16]) {
        self.half = None;
        self.samples.extend(pcm.iter().flat_map(|s| [*s, *s]));
    }

    fn stereo(&mut self, interleaved: &[i16]) {
        let mut rest = interleaved;
        if let Some(l) = self.half.take() {
            let Some((r, tail)) = rest.split_first() else {
                self.half = Some(l);
                return;
            };
            self.samples.extend([l, *r]);
            rest = tail;
        }
        let whole = rest.len() - rest.len() % 2;
        self.samples.extend(rest[..whole].iter().copied());
        self.half = rest.get(whole).copied();
    }

    fn clear(&mut self) {
        self.samples.clear();
        self.half = None;
    }

    /// Fill `frame` (FRAME * 2 samples) from the front, padded with silence: false when nothing
    /// was queued.
    fn take(&mut self, frame: &mut [i16]) -> bool {
        if self.samples.is_empty() {
            return false;
        }
        for s in frame.iter_mut() {
            *s = self.samples.pop_front().unwrap_or(0);
        }
        true
    }
}

/// One call's media: what it hears, and what it says.
pub struct Engine {
    peers: Mutex<HashMap<[u8; 32], Peer>>,
    heard: HeardQueue,
    outgoing: Mutex<Outgoing>,
    muted: AtomicBool,
    deafened: AtomicBool,
    stopped: AtomicBool,
    level: AtomicU32,
    kbps: AtomicU32,
    sender: Mutex<Option<tokio::task::JoinHandle<()>>>,
    drained: Notify,
}

impl Engine {
    fn new() -> Arc<Engine> {
        let engine = Arc::new(Engine {
            peers: Mutex::new(HashMap::new()),
            heard: HeardQueue::default(),
            outgoing: Mutex::new(Outgoing::default()),
            muted: AtomicBool::new(false),
            deafened: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            level: AtomicU32::new(0),
            kbps: AtomicU32::new(DEFAULT_KBPS),
            sender: Mutex::new(None),
            drained: Notify::new(),
        });
        let weak = Arc::downgrade(&engine);
        // spawn-detached: paces one call's outgoing frames; holds the engine weakly, no account state.
        let task = tokio::spawn(async move { send_loop(weak).await });
        *engine.sender.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
        engine
    }

    /// The next frame the call heard, or `None` once it has ended.
    pub async fn listen(&self) -> Option<Heard> {
        self.heard.next().await
    }

    /// Whether the call has ended: it says and hears nothing more.
    pub fn ended(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Queue 48 kHz mono samples to say, after whatever is already queued. False once the call
    /// has ended: nothing is queued.
    pub fn say(&self, pcm: &[i16]) -> bool {
        if self.ended() {
            return false;
        }
        self.outgoing.lock().unwrap_or_else(|e| e.into_inner()).mono(pcm);
        true
    }

    /// Queue 48 kHz stereo samples, interleaved left then right, after whatever is already
    /// queued. Listeners hear it in stereo. A slice may end between a left and its right: the
    /// next call carries on from there, so a stream can be queued in chunks of any length.
    /// False once the call has ended: nothing is queued.
    pub fn say_stereo(&self, interleaved: &[i16]) -> bool {
        if self.ended() {
            return false;
        }
        self.outgoing.lock().unwrap_or_else(|e| e.into_inner()).stereo(interleaved);
        true
    }

    /// Set the Opus bitrate for what this call says, from the next frame on, in kbit/s.
    /// Clamped to [`MIN_KBPS`]..=[`MAX_KBPS`]; returns the rate set.
    pub fn set_bitrate(&self, kbps: u32) -> u32 {
        let kbps = kbps.clamp(MIN_KBPS, MAX_KBPS);
        self.kbps.store(kbps, Ordering::Relaxed);
        kbps
    }

    /// The Opus bitrate for what this call says, in kbit/s.
    pub fn bitrate(&self) -> u32 {
        self.kbps.load(Ordering::Relaxed)
    }

    /// How much is still queued to say.
    pub fn queued(&self) -> Duration {
        let frames = self.outgoing.lock().unwrap_or_else(|e| e.into_inner()).samples.len() as u64 / 2;
        Duration::from_millis(frames * 1000 / ENGINE_RATE as u64)
    }

    /// Drop whatever is queued to say.
    pub fn hush(&self) {
        self.outgoing.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.drained.notify_waiters();
    }

    /// Wait until everything queued has been said: true, or false when the call ended first.
    pub async fn finished(&self) -> bool {
        loop {
            let notified = self.drained.notified();
            if self.ended() {
                return false;
            }
            if self.outgoing.lock().unwrap_or_else(|e| e.into_inner()).samples.is_empty() {
                return true;
            }
            notified.await;
        }
    }

    /// How many peers the call is connected to.
    pub fn connected(&self) -> usize {
        self.peers.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    fn connections(&self) -> Vec<Connection> {
        self.peers.lock().unwrap_or_else(|e| e.into_inner()).values().map(|p| p.conn.clone()).collect()
    }
}

fn level_of(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum: f64 = pcm.iter().map(|s| (*s as f64 / i16::MAX as f64).powi(2)).sum();
    ((sum / pcm.len() as f64).sqrt() * 2.0).min(1.0) as f32
}

/// One stereo encoder that sends a frame with matching sides as a mono packet: the whole
/// bitrate goes to one channel when a second would carry nothing.
struct Voice {
    enc: opus::Encoder,
    stereo: Option<bool>,
    kbps: u32,
}

impl Voice {
    fn new() -> Result<Self, opus::Error> {
        let mut enc = opus::Encoder::new(ENGINE_RATE, opus::Channels::Stereo, opus::Application::Audio)?;
        enc.set_inband_fec(true)?;
        enc.set_packet_loss_perc(10)?;
        Ok(Voice { enc, stereo: None, kbps: 0 })
    }

    /// Sides that differ leave the choice to Opus, which still goes mono at a starved rate.
    fn shape(&mut self, stereo: bool, kbps: u32) -> Result<(), opus::Error> {
        if self.stereo != Some(stereo) {
            self.enc.set_force_channels(if stereo { None } else { Some(opus::Channels::Mono) })?;
            self.stereo = Some(stereo);
        }
        if self.kbps != kbps {
            self.enc.set_bitrate(opus::Bitrate::Bits(kbps as i32 * 1000))?;
            self.kbps = kbps;
        }
        Ok(())
    }

    /// One interleaved stereo frame (FRAME * 2 samples) at `kbps` into `out`, returning the
    /// packet length.
    fn encode(&mut self, frame: &[i16], kbps: u32, out: &mut [u8]) -> Result<usize, opus::Error> {
        self.shape(frame.chunks_exact(2).any(|lr| lr[0] != lr[1]), kbps)?;
        let room = out.len().min(MAX_PACKET);
        self.enc.encode(frame, &mut out[..room])
    }
}

/// One frame every 20 ms to every peer: audio while there is some queued, a muted frame between.
async fn send_loop(engine: std::sync::Weak<Engine>) {
    let mut encoder = match Voice::new() {
        Ok(v) => v,
        Err(e) => {
            vector_core::log_warn!("[voice] no Opus encoder: {e}");
            return;
        }
    };
    let started = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(vector_core::calls::FRAME_MS as u64));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut seq: u16 = 0;
    let mut frame = vec![0i16; FRAME * 2];
    let mut packet = vec![0u8; 1500];
    loop {
        tick.tick().await;
        let Some(engine) = engine.upgrade() else { return };
        if engine.stopped.load(Ordering::Relaxed) {
            return;
        }
        let ts = started.elapsed().as_millis() as u32;
        let have = if engine.muted.load(Ordering::Relaxed) {
            false
        } else {
            let mut queue = engine.outgoing.lock().unwrap_or_else(|e| e.into_inner());
            let had = queue.take(&mut frame);
            if had && queue.samples.is_empty() {
                engine.drained.notify_waiters();
            }
            had
        };
        let datagram = if have {
            engine.level.store(level_of(&frame).to_bits(), Ordering::Relaxed);
            match encoder.encode(&frame, engine.bitrate(), &mut packet) {
                Ok(len) => pack(seq, ts, 0, &packet[..len]),
                Err(_) => pack(seq, ts, FLAG_MUTED, &[]),
            }
        } else {
            engine.level.store(0, Ordering::Relaxed);
            pack(seq, ts, FLAG_MUTED, &[])
        };
        for conn in engine.connections() {
            let _ = conn.send_datagram(datagram.clone());
        }
        seq = seq.wrapping_add(1);
    }
}

/// Read one peer's frames, decode them in order, and hand them to the program.
async fn receive(endpoint: [u8; 32], conn: Connection, engine: std::sync::Weak<Engine>, audible: Arc<AtomicBool>, level: Arc<AtomicU32>) {
    let Ok(mut decoder) = opus::Decoder::new(ENGINE_RATE, opus::Channels::Mono) else { return };
    let mut last: Option<u16> = None;
    let mut pcm = vec![0i16; FRAME * 6];
    let mut quiet = QUIET_TAIL;
    while let Ok(datagram) = conn.read_datagram().await {
        let Some(frame) = unpack(&datagram) else { continue };
        if frame.flags & FLAG_SHARE != 0 {
            continue;
        }
        // A frame older than one already played arrives too late to be heard.
        if let Some(prev) = last {
            if frame.seq.wrapping_sub(prev) as i16 <= 0 {
                continue;
            }
        }
        last = Some(frame.seq);
        if frame.flags & FLAG_MUTED != 0 || frame.payload.is_empty() {
            level.store(0, Ordering::Relaxed);
            if quiet >= QUIET_TAIL {
                continue;
            }
            quiet += 1;
            let Some(engine) = engine.upgrade() else { return };
            if !engine.deafened.load(Ordering::Relaxed) && audible.load(Ordering::Relaxed) {
                engine.heard.push(Heard { endpoint, pcm: vec![0; FRAME] });
            }
            continue;
        }
        quiet = 0;
        let Ok(n) = decoder.decode(frame.payload, &mut pcm, false) else { continue };
        level.store(level_of(&pcm[..n]).to_bits(), Ordering::Relaxed);
        let Some(engine) = engine.upgrade() else { return };
        if engine.deafened.load(Ordering::Relaxed) || !audible.load(Ordering::Relaxed) {
            continue;
        }
        // A program that stops reading loses the oldest audio, never the call.
        engine.heard.push(Heard { endpoint, pcm: pcm[..n].to_vec() });
    }
}

/// Whoever stopped being heard for good: what they were saying has ended.
fn ended_for(engine: &Engine, endpoint: [u8; 32]) {
    engine.heard.push(Heard { endpoint, pcm: Vec::new() });
}

/// What core holds of an engine: the same engine, shared with whoever joined.
struct Shared(Arc<Engine>);

impl VoiceMedia for Shared {
    fn add_peer(&self, endpoint: [u8; 32], conn: Connection) {
        let (audible, level) = (Arc::new(AtomicBool::new(true)), Arc::new(AtomicU32::new(0)));
        // spawn-detached: reads one connection's datagrams; holds the engine weakly, no account state.
        let rx = tokio::spawn(receive(endpoint, conn.clone(), Arc::downgrade(&self.0), audible.clone(), level.clone()));
        if let Some(old) = self.0.peers.lock().unwrap_or_else(|e| e.into_inner()).insert(endpoint, Peer { conn, rx, audible, level }) {
            old.rx.abort();
        }
    }

    fn remove_peer(&self, endpoint: &[u8; 32]) {
        let gone = self.0.peers.lock().unwrap_or_else(|e| e.into_inner()).remove(endpoint);
        if let Some(p) = gone {
            p.rx.abort();
            ended_for(&self.0, *endpoint);
        }
    }

    fn set_muted(&self, on: bool) {
        self.0.muted.store(on, Ordering::Relaxed);
    }

    fn set_deafened(&self, on: bool) {
        if !self.0.deafened.swap(on, Ordering::Relaxed) && on {
            let peers: Vec<[u8; 32]> = self.0.peers.lock().unwrap_or_else(|e| e.into_inner()).keys().copied().collect();
            for ep in peers {
                ended_for(&self.0, ep);
            }
        }
    }

    fn set_audible(&self, endpoint: &[u8; 32], on: bool) {
        let was = self.0.peers.lock().unwrap_or_else(|e| e.into_inner()).get(endpoint).map(|p| p.audible.swap(on, Ordering::Relaxed));
        if was == Some(true) && !on {
            ended_for(&self.0, *endpoint);
        }
    }

    fn stop(&self) {
        if self.0.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        for (_, p) in self.0.peers.lock().unwrap_or_else(|e| e.into_inner()).drain() {
            p.rx.abort();
        }
        if let Some(t) = self.0.sender.lock().unwrap_or_else(|e| e.into_inner()).take() {
            t.abort();
        }
        self.0.outgoing.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.0.heard.close();
        self.0.drained.notify_waiters();
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn levels(&self) -> Vec<(Option<[u8; 32]>, f32)> {
        let mut out = vec![(None, f32::from_bits(self.0.level.load(Ordering::Relaxed)))];
        out.extend(self.0.peers.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(ep, p)| (Some(*ep), f32::from_bits(p.level.load(Ordering::Relaxed)))));
        out
    }
}

/// The calls platform for a process with no devices: voice Channels get a headless engine, and
/// 1:1 calls are refused.
struct Headless;

#[async_trait::async_trait]
impl CallPlatform for Headless {
    async fn endpoint(&self) -> Result<iroh::Endpoint, String> {
        Err("1:1 calls aren't available here, only voice Channels".into())
    }

    async fn local_addr(&self) -> Result<String, String> {
        Err("1:1 calls aren't available here".into())
    }

    fn decode_addr(&self, _addr: &str) -> Option<iroh::EndpointAddr> {
        None
    }

    async fn start_audio(&self, _conn: Connection, _start: AudioStart) -> Result<Box<dyn CallAudio>, String> {
        Err("1:1 calls aren't available here".into())
    }

    async fn start_voice(&self) -> Result<Box<dyn VoiceMedia>, String> {
        Ok(Box::new(Shared(Engine::new())))
    }

    fn set_video_taker(&self, _taker: Option<mpsc::Sender<LinkConn>>) {}
}

/// Make this process's voice Channel calls headless. Once per process; later calls do nothing.
/// Returns false when another platform was installed first.
pub fn install() -> bool {
    static MINE: OnceLock<bool> = OnceLock::new();
    *MINE.get_or_init(|| {
        let platform: Arc<dyn CallPlatform> = Arc::new(Headless);
        vector_core::calls::platform::install(platform.clone());
        vector_core::calls::platform::installed_is(&platform)
    })
}

/// The engine of this account's call in `channel_hex`, if the bot is in one with audio.
pub fn engine_in(channel_hex: &str) -> Option<Arc<Engine>> {
    let media = vector_core::calls::voice::media_in(channel_hex)?;
    media.as_any()?.downcast_ref::<Shared>().map(|s| s.0.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_round_trips_a_tone() {
        let mut enc = opus::Encoder::new(ENGINE_RATE, opus::Channels::Mono, opus::Application::Audio).unwrap();
        let mut dec = opus::Decoder::new(ENGINE_RATE, opus::Channels::Mono).unwrap();
        let tone: Vec<i16> = (0..FRAME * 10).map(|i| ((i as f32 * 440.0 * std::f32::consts::TAU / ENGINE_RATE as f32).sin() * 12_000.0) as i16).collect();
        let mut packet = vec![0u8; 1500];
        let mut out = vec![0i16; FRAME];
        let mut energy = 0.0;
        for chunk in tone.chunks(FRAME) {
            let n = enc.encode(chunk, &mut packet).unwrap();
            let got = dec.decode(&packet[..n], &mut out, false).unwrap();
            assert_eq!(got, FRAME);
            energy += level_of(&out);
        }
        assert!(energy / 10.0 > 0.3, "the tone survives the codec");
    }

    #[test]
    fn a_frame_goes_out_stereo_only_when_its_sides_differ() {
        let mut voice = Voice::new().unwrap();
        let mut packet = vec![0u8; 1500];
        let mono: Vec<i16> = (0..FRAME).flat_map(|i| {
            let s = ((i as f32 * 440.0 * std::f32::consts::TAU / ENGINE_RATE as f32).sin() * 12_000.0) as i16;
            [s, s]
        }).collect();
        let wide: Vec<i16> = (0..FRAME).flat_map(|i| {
            let t = i as f32 * std::f32::consts::TAU / ENGINE_RATE as f32;
            [((t * 440.0).sin() * 12_000.0) as i16, ((t * 660.0).sin() * 12_000.0) as i16]
        }).collect();
        let channels = |voice: &mut Voice, frame: &[i16], packet: &mut [u8]| {
            let n = voice.encode(frame, DEFAULT_KBPS, packet).unwrap();
            opus::packet::get_nb_channels(&packet[..n]).unwrap()
        };
        assert_eq!(channels(&mut voice, &mono, &mut packet), opus::Channels::Mono);
        assert_eq!(channels(&mut voice, &wide, &mut packet), opus::Channels::Stereo);
        // Opus eases back to mono over a couple of frames rather than cutting the second side.
        let back = (0..4).map(|_| channels(&mut voice, &mono, &mut packet)).collect::<Vec<_>>();
        assert_eq!(back.last(), Some(&opus::Channels::Mono), "{back:?}");

        let mut stereo_ear = opus::Decoder::new(ENGINE_RATE, opus::Channels::Stereo).unwrap();
        let mut out = vec![0i16; FRAME * 2];
        let (mut left, mut apart) = (0f64, 0f64);
        for _ in 0..5 {
            let n = voice.encode(&wide, DEFAULT_KBPS, &mut packet).unwrap();
            stereo_ear.decode(&packet[..n], &mut out, false).unwrap();
        }
        for lr in out.chunks_exact(2) {
            left += (lr[0] as f64).powi(2);
            apart += (lr[1] as f64 - lr[0] as f64).powi(2);
        }
        assert!(left > 0.0 && apart > left * 0.1, "the two sides arrive as two different signals");
    }

    #[test]
    fn a_higher_bitrate_spends_more_and_never_outgrows_a_datagram() {
        let noise: Vec<i16> = {
            let mut x = 0x1234_5678u32;
            (0..FRAME * 2 * 20).map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as i16) / 2
            }).collect()
        };
        let average = |kbps: u32| {
            let mut voice = Voice::new().unwrap();
            let mut packet = vec![0u8; 1500];
            let sizes: Vec<usize> = noise.chunks(FRAME * 2).map(|f| voice.encode(f, kbps, &mut packet).unwrap()).collect();
            assert!(sizes.iter().all(|n| *n <= MAX_PACKET), "{kbps} kbit/s: {sizes:?}");
            sizes[5..].iter().sum::<usize>() / (sizes.len() - 5)
        };
        let (low, default, high) = (average(32), average(DEFAULT_KBPS), average(MAX_KBPS));
        assert!(low < default && default < high, "{low} < {default} < {high} bytes a frame");
        assert!((default as i64 - (DEFAULT_KBPS as i64 * 20 / 8)).abs() < 40, "64 kbit/s is about 160 bytes a frame: {default}");
    }

    #[tokio::test]
    async fn a_bitrate_outside_the_range_is_clamped() {
        let engine = Engine::new();
        assert_eq!(engine.bitrate(), DEFAULT_KBPS);
        assert_eq!(engine.set_bitrate(1), MIN_KBPS);
        assert_eq!(engine.set_bitrate(10_000), MAX_KBPS);
        assert_eq!(engine.set_bitrate(128), 128);
        assert_eq!(engine.bitrate(), 128);
        Shared(engine.clone()).stop();
    }

    #[tokio::test]
    async fn mono_and_stereo_queue_as_the_same_length_of_time() {
        let engine = Engine::new();
        engine.say(&vec![1; FRAME]);
        engine.say_stereo(&vec![1; FRAME * 2 + 1]);
        assert_eq!(engine.queued(), Duration::from_millis(40));
        Shared(engine.clone()).stop();
    }

    #[test]
    fn what_is_queued_goes_out_in_order_with_its_sides_kept() {
        let mut q = Outgoing::default();
        q.mono(&[7; FRAME]);
        // A stereo stream in odd-length chunks: left 1, right -1 throughout.
        let lr: Vec<i16> = (0..FRAME).flat_map(|_| [1, -1]).collect();
        for chunk in lr.chunks(333) {
            q.stereo(chunk);
        }
        let mut frame = vec![0i16; FRAME * 2];
        assert!(q.take(&mut frame));
        assert!(frame.iter().all(|s| *s == 7), "the mono frame, on both sides");
        assert!(q.take(&mut frame));
        assert!(frame.chunks_exact(2).all(|p| p == [1, -1]), "no chunk boundary swapped the sides");
        assert!(!q.take(&mut frame), "nothing left");

        q.stereo(&[5]);
        q.mono(&[2; 3]);
        assert!(q.take(&mut frame));
        assert_eq!(&frame[..8], &[2, 2, 2, 2, 2, 2, 0, 0], "a half left over is dropped by mono, then silence pads");
    }

    #[test]
    fn a_starved_rate_leaves_stereo_to_opus() {
        let mut voice = Voice::new().unwrap();
        let mut packet = vec![0u8; 1500];
        let wide: Vec<i16> = (0..FRAME).flat_map(|i| {
            let t = i as f32 * std::f32::consts::TAU / ENGINE_RATE as f32;
            [((t * 440.0).sin() * 12_000.0) as i16, ((t * 660.0).sin() * 12_000.0) as i16]
        }).collect();
        let last = (0..6).map(|_| {
            let n = voice.encode(&wide, MIN_KBPS, &mut packet).unwrap();
            opus::packet::get_nb_channels(&packet[..n]).unwrap()
        }).last();
        assert_eq!(last, Some(opus::Channels::Mono), "at 8 kbit/s Opus spends it all on one channel");
    }

    #[tokio::test]
    async fn queued_speech_drains_in_real_time() {
        let engine = Engine::new();
        engine.say(&vec![1000; FRAME * 5]);
        assert_eq!(engine.queued(), Duration::from_millis(100));
        let t = Instant::now();
        engine.finished().await;
        let took = t.elapsed();
        assert!(took >= Duration::from_millis(60) && took < Duration::from_millis(400), "{took:?}");
        Shared(engine.clone()).stop();
    }

    #[tokio::test]
    async fn an_ended_call_stops_listening_and_speaking() {
        let engine = Engine::new();
        let ep = [1; 32];
        engine.heard.push(Heard { endpoint: ep, pcm: vec![5; FRAME] });
        engine.say(&vec![1000; FRAME * 500]);
        let shared = Shared(engine.clone());
        let waiting = {
            let engine = engine.clone();
            tokio::spawn(async move { engine.finished().await })
        };
        shared.stop();
        assert!(!waiting.await.unwrap(), "what was queued didn't finish: the call ended");
        assert!(!engine.say(&[1; FRAME]), "nothing queues after");
        assert_eq!(engine.queued(), Duration::ZERO);
        assert!(engine.listen().await.is_some(), "what was heard before still reads");
        assert!(engine.listen().await.is_none(), "then the call is over");
    }

    #[test]
    fn a_program_that_falls_behind_loses_the_oldest_frames() {
        let q = HeardQueue::default();
        for i in 0..HEARD_QUEUE + 3 {
            q.push(Heard { endpoint: [0; 32], pcm: vec![i as i16] });
        }
        let first = q.frames.lock().unwrap().0.front().unwrap().pcm[0];
        assert_eq!(first, 3, "the three oldest went");
    }
}
