//! Headless media for Vector: hear and speak in voice Channels without a microphone or a
//! speaker. Audio comes in as 48 kHz mono PCM, one 20 ms frame per speaker at a time, and goes
//! out from PCM a program queues, encoded with Opus and paced in real time.
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
/// The Opus bitrate: music-grade for one voice, as a voice Channel mesh sends to each peer.
const KBPS: i32 = 48;
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

/// One call's media: what it hears, and what it says.
pub struct Engine {
    peers: Mutex<HashMap<[u8; 32], Peer>>,
    heard: HeardQueue,
    outgoing: Mutex<VecDeque<i16>>,
    muted: AtomicBool,
    deafened: AtomicBool,
    stopped: AtomicBool,
    level: AtomicU32,
    sender: Mutex<Option<tokio::task::JoinHandle<()>>>,
    drained: Notify,
}

impl Engine {
    fn new() -> Arc<Engine> {
        let engine = Arc::new(Engine {
            peers: Mutex::new(HashMap::new()),
            heard: HeardQueue::default(),
            outgoing: Mutex::new(VecDeque::new()),
            muted: AtomicBool::new(false),
            deafened: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            level: AtomicU32::new(0),
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
        self.outgoing.lock().unwrap_or_else(|e| e.into_inner()).extend(pcm.iter().copied());
        true
    }

    /// How much is still queued to say.
    pub fn queued(&self) -> Duration {
        let samples = self.outgoing.lock().unwrap_or_else(|e| e.into_inner()).len() as u64;
        Duration::from_millis(samples * 1000 / ENGINE_RATE as u64)
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
            if self.outgoing.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
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

/// One frame every 20 ms to every peer: audio while there is some queued, a muted frame between.
async fn send_loop(engine: std::sync::Weak<Engine>) {
    let mut encoder = match opus::Encoder::new(ENGINE_RATE, opus::Channels::Mono, opus::Application::Audio) {
        Ok(mut e) => {
            let _ = e.set_bitrate(opus::Bitrate::Bits(KBPS * 1000));
            let _ = e.set_inband_fec(true);
            let _ = e.set_packet_loss_perc(10);
            e
        }
        Err(e) => {
            vector_core::log_warn!("[voice] no Opus encoder: {e}");
            return;
        }
    };
    let started = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(vector_core::calls::FRAME_MS as u64));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut seq: u16 = 0;
    let mut frame = vec![0i16; FRAME];
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
            if queue.is_empty() {
                false
            } else {
                for s in frame.iter_mut() {
                    *s = queue.pop_front().unwrap_or(0);
                }
                if queue.is_empty() {
                    engine.drained.notify_waiters();
                }
                true
            }
        };
        let datagram = if have {
            engine.level.store(level_of(&frame).to_bits(), Ordering::Relaxed);
            match encoder.encode(&frame, &mut packet) {
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
