//! A voice Channel's media: the 1:1 capture chain (echo cancelled, encoded once) sent to every
//! peer, and each peer's own jitter buffer and decoder, mixed into one playout that is also the
//! canceller's reference.

use super::codec::Decoder;
use super::jitter::{Jitter, Pop, MAX_PLC_RUN};
use super::media::{capture_thread, level_of, now_ms, MediaStats, Outbound, FLAG_MUTED, FLAG_SHARE, PLAY_AHEAD_MS};
use super::resample::Resampler;
use super::ring::SpscRing;
use super::transport::unpack;
use super::{ENGINE_RATE, FRAME};
use crate::audio_engine::{AudioEngine, LiveLink};
use iroh::endpoint::Connection;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vector_core::calls::voice::VoiceMedia;

struct Peer {
    jitter: Arc<Mutex<Jitter>>,
    stats: Arc<MediaStats>,
    audible: Arc<AtomicBool>,
    level: Arc<AtomicU32>,
    rx: tokio::task::JoinHandle<()>,
}

pub struct VoiceEngine {
    stop: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    deafened: Arc<AtomicBool>,
    /// False once the call went ahead without a microphone.
    mic: Arc<AtomicBool>,
    stats: Arc<MediaStats>,
    conns: Arc<Mutex<Vec<([u8; 32], Connection)>>>,
    sends: Arc<Mutex<Vec<Connection>>>,
    peers: Arc<Mutex<HashMap<[u8; 32], Peer>>>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl VoiceEngine {
    pub fn start() -> Result<Self, String> {
        let mixer = AudioEngine::get()?;
        let out_rate = mixer.device_sample_rate()?;
        let link = Arc::new(LiveLink {
            play: SpscRing::new(out_rate as usize),
            tap: SpscRing::new(out_rate as usize),
            gain: AtomicU32::new(super::settings::speaker_gain().to_bits()),
            share_play: SpscRing::new(out_rate as usize * 2),
            share_tap: SpscRing::new(out_rate as usize),
            share_gain: AtomicU32::new(1f32.to_bits()),
        });
        mixer.attach_live(Arc::clone(&link))?;
        super::settings::load();
        let stop = Arc::new(AtomicBool::new(false));
        let muted = Arc::new(AtomicBool::new(false));
        let deafened = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(MediaStats::default());
        let sends = Arc::new(Mutex::new(Vec::new()));
        let peers: Arc<Mutex<HashMap<[u8; 32], Peer>>> = Arc::new(Mutex::new(HashMap::new()));

        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let mic = Arc::new(AtomicBool::new(true));
        let capture = {
            let (stop, muted, stats, link) = (Arc::clone(&stop), Arc::clone(&muted), Arc::clone(&stats), Arc::clone(&link));
            let out = Some(Outbound::Many(Arc::clone(&sends)));
            let mic = Arc::clone(&mic);
            std::thread::Builder::new()
                .name("voice-capture".into())
                .spawn(move || capture_thread(out, link, out_rate, stop, muted, mic, stats, ready_tx, true, Arc::new(AtomicBool::new(false))))
                .map_err(|e| e.to_string())?
        };
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                stop.store(true, Ordering::SeqCst);
                let _ = capture.join();
                mixer.detach_live();
                return Err(e);
            }
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                mixer.detach_live();
                return Err("the microphone did not start".into());
            }
        }
        let playout = {
            let (stop, deafened, peers, link) = (Arc::clone(&stop), Arc::clone(&deafened), Arc::clone(&peers), Arc::clone(&link));
            std::thread::Builder::new()
                .name("voice-playout".into())
                .spawn(move || playout_thread(link, out_rate, peers, stop, deafened))
                .map_err(|e| e.to_string())?
        };
        Ok(Self { stop, muted, deafened, mic, stats, conns: Arc::new(Mutex::new(Vec::new())), sends, peers, threads: vec![capture, playout] })
    }
}

impl VoiceMedia for VoiceEngine {
    fn add_peer(&self, endpoint: [u8; 32], conn: Connection) {
        let jitter = Arc::new(Mutex::new(Jitter::default()));
        let stats = Arc::new(MediaStats::default());
        let rx = {
            let (jitter, stats, conn) = (Arc::clone(&jitter), Arc::clone(&stats), conn.clone());
            // spawn-detached: reads one connection's datagrams into its jitter buffer; holds no account state.
            tokio::spawn(async move {
                while let Ok(datagram) = conn.read_datagram().await {
                    let Some(frame) = unpack(&datagram) else { continue };
                    if frame.flags & FLAG_SHARE != 0 {
                        continue;
                    }
                    stats.received.fetch_add(1, Ordering::Relaxed);
                    jitter.lock().unwrap_or_else(|e| e.into_inner()).push(frame.seq, frame.flags & FLAG_MUTED != 0, frame.payload, now_ms(), &stats);
                }
            })
        };
        self.sends.lock().unwrap_or_else(|e| e.into_inner()).push(conn.clone());
        self.conns.lock().unwrap_or_else(|e| e.into_inner()).push((endpoint, conn));
        let peer = Peer { jitter, stats, audible: Arc::new(AtomicBool::new(true)), level: Arc::new(AtomicU32::new(0)), rx };
        if let Some(old) = self.peers.lock().unwrap_or_else(|e| e.into_inner()).insert(endpoint, peer) {
            old.rx.abort();
        }
    }

    fn remove_peer(&self, endpoint: &[u8; 32]) {
        if let Some(p) = self.peers.lock().unwrap_or_else(|e| e.into_inner()).remove(endpoint) {
            p.rx.abort();
        }
        let mut conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
        let gone: Vec<usize> = conns.iter().filter(|(ep, _)| ep == endpoint).map(|(_, c)| c.stable_id()).collect();
        conns.retain(|(ep, _)| ep != endpoint);
        self.sends.lock().unwrap_or_else(|e| e.into_inner()).retain(|c| !gone.contains(&c.stable_id()));
    }

    fn set_muted(&self, on: bool) {
        self.muted.store(on, Ordering::Relaxed);
    }

    fn set_deafened(&self, on: bool) {
        self.deafened.store(on, Ordering::Relaxed);
    }

    fn has_mic(&self) -> bool {
        self.mic.load(Ordering::Relaxed)
    }

    fn set_audible(&self, endpoint: &[u8; 32], on: bool) {
        if let Some(p) = self.peers.lock().unwrap_or_else(|e| e.into_inner()).get(endpoint) {
            p.audible.store(on, Ordering::Relaxed);
        }
    }

    fn levels(&self) -> Vec<(Option<[u8; 32]>, f32)> {
        let mut out = vec![(None, f32::from_bits(self.stats.mic_level.load(Ordering::Relaxed)))];
        out.extend(self.peers.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(ep, p)| (Some(*ep), f32::from_bits(p.level.load(Ordering::Relaxed)))));
        out
    }
}

impl Drop for VoiceEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for (_, p) in self.peers.lock().unwrap_or_else(|e| e.into_inner()).drain() {
            p.rx.abort();
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        if let Ok(mixer) = AudioEngine::get() {
            mixer.detach_live();
        }
    }
}

/// Each peer's decoder state, kept beside the engine's map so a peer gone mid-frame is simply skipped.
struct Voicing {
    dec: Decoder,
    plc_run: u32,
}

fn playout_thread(link: Arc<LiveLink>, out_rate: u32, peers: Arc<Mutex<HashMap<[u8; 32], Peer>>>, stop: Arc<AtomicBool>, deafened: Arc<AtomicBool>) {
    let mut rs = Resampler::new(ENGINE_RATE, out_rate);
    let ahead = (out_rate * PLAY_AHEAD_MS / 1000) as usize;
    let mut decoders: HashMap<[u8; 32], Voicing> = HashMap::new();
    let mut pcm = [0i16; FRAME];
    let mut mix = [0i32; FRAME];
    let mut mix_f32 = [0f32; FRAME];
    let mut resampled = Vec::with_capacity(4096);

    while !stop.load(Ordering::Relaxed) {
        if link.play.len() >= ahead {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        mix.fill(0);
        let mut any = false;
        {
            let map = peers.lock().unwrap_or_else(|e| e.into_inner());
            decoders.retain(|ep, _| map.contains_key(ep));
            for (ep, peer) in map.iter() {
                let Some(v) = (match decoders.get_mut(ep) {
                    Some(v) => Some(v),
                    None => Decoder::new().ok().map(|dec| decoders.entry(*ep).or_insert(Voicing { dec, plc_run: 0 })),
                }) else {
                    continue;
                };
                let next = peer.jitter.lock().unwrap_or_else(|e| e.into_inner()).pop(v.plc_run, now_ms(), &peer.stats);
                let decoded = match next {
                    Pop::Frame(p) => {
                        v.plc_run = 0;
                        v.dec.decode(&p, &mut pcm).is_ok()
                    }
                    Pop::Fec(successor) => {
                        v.plc_run = 0;
                        v.dec.decode_fec(&successor, &mut pcm).is_ok()
                    }
                    Pop::Muted => {
                        v.plc_run = 0;
                        pcm.fill(0);
                        true
                    }
                    Pop::Conceal => {
                        v.plc_run += 1;
                        let ok = v.dec.conceal(&mut pcm).is_ok();
                        let gain = 1.0 - v.plc_run as f32 / (MAX_PLC_RUN as f32 + 1.0);
                        for s in pcm.iter_mut() {
                            *s = (*s as f32 * gain) as i16;
                        }
                        ok
                    }
                    Pop::Wait => continue,
                };
                any = true;
                if !decoded {
                    pcm.fill(0);
                }
                peer.level.store(level_of(&pcm).to_bits(), Ordering::Relaxed);
                if peer.audible.load(Ordering::Relaxed) {
                    for (m, s) in mix.iter_mut().zip(pcm.iter()) {
                        *m += *s as i32;
                    }
                }
            }
        }
        if !any {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let silent = deafened.load(Ordering::Relaxed);
        for (dst, m) in mix_f32.iter_mut().zip(mix.iter()) {
            *dst = if silent { 0.0 } else { soft_clip(*m as f32 / 32768.0) };
        }
        rs.process(&mix_f32, &mut resampled);
        link.play.push(&resampled);
        resampled.clear();
    }
}

/// Several voices at once can sum past full scale: past 0.8 the mix bends toward ±1 instead of clipping.
fn soft_clip(x: f32) -> f32 {
    const KNEE: f32 = 0.8;
    if x.abs() <= KNEE {
        x
    } else {
        let over = (x.abs() - KNEE) / (1.0 - KNEE);
        x.signum() * (KNEE + (1.0 - KNEE) * over.tanh())
    }
}

#[cfg(test)]
mod tests {
    use super::soft_clip;

    #[test]
    fn a_mix_never_leaves_full_scale_and_quiet_voices_pass_untouched() {
        assert_eq!(soft_clip(0.5), 0.5);
        assert_eq!(soft_clip(-0.8), -0.8);
        for x in [0.9f32, 1.5, 3.0, 10.0] {
            let y = soft_clip(x);
            assert!(y > 0.8 && y <= 1.0, "{x} → {y}");
            assert_eq!(soft_clip(-x), -y);
        }
        assert!(soft_clip(1.5) < soft_clip(3.0), "louder stays louder");
    }
}
