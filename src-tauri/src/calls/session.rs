//! One call at a time: the state machine, its signalling over gift-wrapped rumors,
//! and the Iroh connection that carries the media once both sides have agreed.
//!
//! Every deferred step (a ring timeout, a connect, a closed watcher) carries the
//! call id it was started for and does nothing if the call it finds is another.

use super::media::{MediaEngine, ShareInput};
use super::transport::{read_control, write_control, Control, Tracks, VideoCodec, VideoKind, CALL_ALPN};
use super::video::{Hooks, Prefs, VideoSnapshot, VideoTrack};
use crate::miniapps::realtime::{decode_node_addr, encode_node_addr, IrohState};
use crate::miniapps::state::MiniAppsState;
use crate::{active_trusted_relays, my_public_key, nostr_client, TAURI_APP};
use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt};
use iroh::EndpointAddr;
use nostr_sdk::prelude::*;
use serde::Serialize;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::Manager;
use tokio::task::JoinHandle;
use vector_core::event_ext::FinalizeUnsignedWithId;

/// An offer older than this never rings: it was a call that already ended.
const OFFER_TTL_SECS: u64 = 60;
const RING_TIMEOUT: Duration = Duration::from_secs(45);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Seconds without a single frame from the peer before the call is declared dead.
const PEER_SILENCE_SECS: u32 = 8;

#[derive(Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Ringing,
    Connecting,
    Active,
    Ended,
}

#[derive(Serialize, Clone, Debug)]
pub struct CallState {
    pub id: String,
    pub peer: String,
    pub outgoing: bool,
    pub phase: Phase,
    pub reason: Option<String>,
    pub muted: bool,
    pub peer_muted: bool,
    /// Listener-side volume, 1.0 is unity.
    pub volume: f32,
    /// Milliseconds since the call went active; 0 before that.
    pub active_ms: u64,
    /// What I am sending on video, and what the peer is: either, both or neither.
    pub video_mine: Tracks,
    pub video_peer: Tracks,
    /// Codecs the peer can decode, from their offer or answer; empty means no video.
    pub peer_decodes: Vec<String>,
    /// The offer asked for a video call.
    pub video_offered: bool,
    /// The peer has hidden our picture; the camera can rest.
    pub paused_by_peer: bool,
    /// The shared screen's sound travels with it, each way.
    pub share_audio_mine: bool,
    pub share_audio_peer: bool,
    /// Listener-side volume for their screen's sound, 1.0 is unity.
    pub share_volume: f32,
}

#[derive(Serialize, Clone, Debug)]
pub struct LevelPayload {
    pub id: String,
    pub mic: f32,
    pub peer: f32,
}

#[derive(Serialize, Clone, Debug)]
pub struct CallStats {
    pub id: String,
    pub rtt_ms: u32,
    pub path: &'static str,
    pub sent: u64,
    pub send_dropped: u64,
    pub received: u64,
    pub lost: u64,
    pub shed: u64,
    pub rebuilt: u64,
    pub concealed: u64,
    pub late: u64,
    pub depth_ms: u32,
    pub jitter_ms: u32,
    pub clicks_cut: u64,
    pub bitrate_kbps: u32,
    /// Percent of our packets the network dropped in the last second.
    pub net_loss: f32,
    pub video: VideoSnapshot,
    pub share_sent: u64,
    pub share_received: u64,
}

struct Call {
    id: String,
    peer: String,
    outgoing: bool,
    phase: Phase,
    conn: Option<Connection>,
    media: Option<MediaEngine>,
    control: Option<Arc<tokio::sync::Mutex<SendStream>>>,
    tasks: Vec<JoinHandle<()>>,
    active_since: Option<Instant>,
    muted: bool,
    peer_muted: bool,
    volume: f32,
    video: Option<VideoTrack>,
    video_mine: Tracks,
    video_peer: Tracks,
    peer_decodes: Vec<String>,
    video_offered: bool,
    paused_by_peer: bool,
    /// The machine stays awake for the call, and the display too once there is video.
    awake: Option<crate::awake::Hold>,
    /// The shared screen's sound from the webview, for the engine's share thread.
    share: Arc<ShareInput>,
    share_audio_mine: bool,
    share_audio_peer: bool,
    share_volume: f32,
}

impl Call {
    /// The hold the call needs right now; dropping the old one after taking the new
    /// keeps the machine covered in between.
    fn refresh_awake(&mut self) {
        let level = if self.phase != Phase::Active {
            None
        } else if self.video_mine.any() || self.video_peer.any() {
            Some(crate::awake::Level::Display)
        } else {
            Some(crate::awake::Level::System)
        };
        let next = level.map(crate::awake::hold);
        self.awake = next;
    }

    fn state(&self, reason: Option<String>) -> CallState {
        CallState {
            id: self.id.clone(),
            peer: self.peer.clone(),
            outgoing: self.outgoing,
            phase: self.phase,
            reason,
            muted: self.muted,
            peer_muted: self.peer_muted,
            volume: self.volume,
            active_ms: self.active_since.map_or(0, |t| t.elapsed().as_millis() as u64),
            video_mine: self.video_mine,
            video_peer: self.video_peer,
            peer_decodes: self.peer_decodes.clone(),
            video_offered: self.video_offered,
            paused_by_peer: self.paused_by_peer,
            share_audio_mine: self.share_audio_mine,
            share_audio_peer: self.share_audio_peer,
            share_volume: self.share_volume,
        }
    }
}

/// What this device's webview can encode and decode, from its boot probe. Sent on
/// every offer and answer so the other side knows whether to offer video at all.
static VIDEO_CAPS: Mutex<(Vec<String>, Vec<String>)> = Mutex::new((Vec::new(), Vec::new()));

pub fn set_video_caps(encode: Vec<String>, decode: Vec<String>) {
    *VIDEO_CAPS.lock().unwrap_or_else(|e| e.into_inner()) = (encode, decode);
}

fn my_decodes() -> Vec<String> {
    VIDEO_CAPS.lock().unwrap_or_else(|e| e.into_inner()).1.clone()
}

/// The codec to send with: the first of ours the peer decodes, H.264 first.
fn pick_codec(peer_decodes: &[String]) -> Option<VideoCodec> {
    let mine = VIDEO_CAPS.lock().unwrap_or_else(|e| e.into_inner()).0.clone();
    [VideoCodec::H264, VideoCodec::Vp8]
        .into_iter()
        .find(|c| mine.iter().any(|m| m == c.name()) && peer_decodes.iter().any(|p| p == c.name()))
}

fn parse_decodes(tag: Option<&str>) -> Vec<String> {
    tag.unwrap_or("")
        .split(',')
        .filter_map(|c| VideoCodec::from_name(c.trim()).map(|c| c.name().to_string()))
        .collect()
}

static CALL: OnceLock<Mutex<Option<Call>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Call>> {
    CALL.get_or_init(|| Mutex::new(None))
}

fn with_call<R>(f: impl FnOnce(&mut Call) -> R) -> Option<R> {
    let mut guard = slot().lock().unwrap_or_else(|e| e.into_inner());
    guard.as_mut().map(f)
}

fn with_call_id<R>(id: &str, f: impl FnOnce(&mut Call) -> R) -> Option<R> {
    let mut guard = slot().lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_mut() {
        Some(call) if call.id == id => Some(f(call)),
        _ => None,
    }
}

fn emit_state(state: &CallState) {
    vector_core::traits::emit_event("call_state", state);
}

pub fn snapshot() -> Option<CallState> {
    with_call(|c| c.state(None))
}

async fn iroh() -> Result<Arc<IrohState>, String> {
    let app = TAURI_APP.get().ok_or("App not ready")?;
    let state = app.state::<MiniAppsState>();
    state.realtime.get_or_init().await.map_err(|e| format!("Iroh unavailable: {e}"))
}

/// Gift-wraps one signal to the peer. The relay-only address rides along on offers and
/// answers; an offer also carries an expiration so a stale one is dropped by relays too.
async fn send_signal(peer: &str, call_id: &str, signal: &str, addr: Option<&str>, offer_video: bool) -> bool {
    let Some(client) = nostr_client() else { return false };
    let Some(me) = my_public_key() else { return false };
    let Ok(pubkey) = PublicKey::from_bech32(peer) else { return false };

    let mut builder = EventBuilder::new(Kind::ApplicationSpecificData, "call")
        .tag(Tag::public_key(pubkey))
        .tag(Tag::custom("d", vec!["vector-call"]))
        .tag(Tag::custom("call-id", vec![call_id]))
        .tag(Tag::custom("call-signal", vec![signal]));
    if let Some(addr) = addr {
        builder = builder.tag(Tag::custom("call-node-addr", vec![addr]));
    }
    if signal == "offer" {
        builder = builder.tag(Tag::expiration(Timestamp::now() + OFFER_TTL_SECS));
        builder = builder.tag(Tag::custom("call-media", vec![if offer_video { "video" } else { "audio" }]));
    }
    if signal == "offer" || signal == "answer" {
        let decodes = my_decodes();
        if !decodes.is_empty() {
            builder = builder.tag(Tag::custom("call-video", vec![decodes.join(",")]));
        }
    }
    let rumor = builder.finalize_unsigned_with_id(me);
    let relays = active_trusted_relays().await;
    match vector_core::send_gift_wrap(&client, relays.into_iter(), &pubkey, rumor, []).await {
        Ok(_) => true,
        Err(e) => {
            log_warn!("[CALLS] Failed to send {signal} to {peer}: {e}");
            false
        }
    }
}

/// Tears the call down and tells the UI why. Idempotent: a second end is a no-op.
pub fn end(id: &str, reason: &str) {
    let call = {
        let mut guard = slot().lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(c) if c.id == id => guard.take(),
            _ => None,
        }
    };
    let Some(mut call) = call else { return };
    log_info!("[CALLS] Call {} ended: {reason}", call.id);
    for t in call.tasks.drain(..) {
        t.abort();
    }
    // Media first: its threads hold the connection and the mixer slot. The video
    // track's drop closes the webview's socket, which stops the camera.
    drop(call.video.take());
    drop(call.media.take());
    if let Some(conn) = call.conn.take() {
        conn.close(VarInt::from_u32(0), b"bye");
    }
    call.phase = Phase::Ended;
    emit_state(&call.state(Some(reason.to_string())));
}

/// Account swap or shutdown: whatever is up comes down without a signal.
pub fn end_all(reason: &str) {
    if let Some(id) = with_call(|c| c.id.clone()) {
        end(&id, reason);
    }
}

pub async fn start(peer: String, video: bool) -> Result<CallState, String> {
    if PublicKey::from_bech32(&peer).is_err() {
        return Err("Not a valid npub".into());
    }
    let iroh = iroh().await?;
    let addr = encode_node_addr(&iroh.get_node_addr()).map_err(|e| e.to_string())?;
    let id = format!("{:032x}", rand::random::<u128>());

    let state = {
        let mut guard = slot().lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return Err("Already in a call".into());
        }
        let call = Call {
            id: id.clone(),
            peer: peer.clone(),
            outgoing: true,
            phase: Phase::Ringing,
            conn: None,
            media: None,
            control: None,
            tasks: Vec::new(),
            active_since: None,
            muted: false,
            peer_muted: false,
            volume: 1.0,
            video: None,
            video_mine: Tracks::default(),
            video_peer: Tracks::default(),
            peer_decodes: Vec::new(),
            video_offered: video,
            paused_by_peer: false,
            awake: None,
            share: Arc::new(ShareInput::new()),
            share_audio_mine: false,
            share_audio_peer: false,
            share_volume: 1.0,
        };
        let state = call.state(None);
        *guard = Some(call);
        state
    };
    emit_state(&state);

    if !send_signal(&peer, &id, "offer", Some(&addr), video).await {
        end(&id, "send_failed");
        return Err("Could not reach the relays".into());
    }
    spawn_ring_timeout(id.clone(), peer, true);
    Ok(state)
}

fn spawn_ring_timeout(id: String, peer: String, outgoing: bool) {
    let task = vector_core::db::spawn_bound(async move {
        tokio::time::sleep(RING_TIMEOUT).await;
        let still_ringing = with_call_id(&id, |c| c.phase == Phase::Ringing).unwrap_or(false);
        if !still_ringing {
            return;
        }
        if outgoing {
            send_signal(&peer, &id, "hangup", None, false).await;
            end(&id, "no_answer");
        } else {
            end(&id, "missed");
        }
    });
    with_call_id_push_task(task);
}

fn with_call_id_push_task(task: JoinHandle<()>) {
    if with_call(|c| c.tasks.push(task)).is_none() {
        // The call ended between spawn and registration; nothing to keep.
    }
}

pub async fn accept() -> Result<(), String> {
    let (id, peer) = with_call(|c| {
        if c.outgoing || c.phase != Phase::Ringing {
            return Err("No incoming call to accept".to_string());
        }
        c.phase = Phase::Connecting;
        Ok((c.id.clone(), c.peer.clone()))
    })
    .unwrap_or(Err("No call".into()))?;
    if let Some(s) = snapshot() {
        emit_state(&s);
    }

    let iroh = iroh().await?;
    let addr = encode_node_addr(&iroh.get_node_addr()).map_err(|e| e.to_string())?;
    if !send_signal(&peer, &id, "answer", Some(&addr), false).await {
        end(&id, "send_failed");
        return Err("Could not reach the relays".into());
    }
    let timeout_id = id.clone();
    let task = vector_core::db::spawn_bound(async move {
        tokio::time::sleep(CONNECT_TIMEOUT).await;
        if with_call_id(&timeout_id, |c| c.phase == Phase::Connecting).unwrap_or(false) {
            end(&timeout_id, "connect_failed");
        }
    });
    with_call_id_push_task(task);
    Ok(())
}

pub async fn reject() -> Result<(), String> {
    let (id, peer) = with_call(|c| (c.id.clone(), c.peer.clone())).ok_or("No call")?;
    send_signal(&peer, &id, "reject", None, false).await;
    end(&id, "rejected");
    Ok(())
}

pub async fn hangup() -> Result<(), String> {
    let (id, peer, control) = with_call(|c| (c.id.clone(), c.peer.clone(), c.control.clone())).ok_or("No call")?;
    if let Some(control) = control {
        let mut send = control.lock().await;
        let _ = tokio::time::timeout(Duration::from_secs(1), write_control(&mut *send, &Control::Bye)).await;
    }
    // Local teardown first: the peer's close must not be what ends our side.
    end(&id, "hangup");
    // The signal reaches a peer whose connection never came up, or one still ringing.
    send_signal(&peer, &id, "hangup", None, false).await;
    Ok(())
}

/// Hang up from a synchronous context, waiting briefly for the Bye and the signal.
pub fn hangup_blocking() {
    if snapshot().is_none() {
        return;
    }
    let _ = tauri::async_runtime::block_on(tokio::time::timeout(Duration::from_secs(3), hangup()));
}

pub async fn set_muted(on: bool) -> Result<(), String> {
    let control = with_call(|c| {
        c.muted = on;
        if let Some(m) = c.media.as_ref() {
            m.muted.store(on, Ordering::Relaxed);
        }
        c.control.clone()
    })
    .ok_or("No call")?;
    if let Some(s) = snapshot() {
        emit_state(&s);
    }
    if let Some(control) = control {
        let mut send = control.lock().await;
        let _ = tokio::time::timeout(Duration::from_secs(1), write_control(&mut *send, &Control::Mute { on })).await;
    }
    Ok(())
}

/// Listener-side volume for the call in progress.
pub async fn set_volume(volume: f32) -> Result<(), String> {
    let volume = volume.clamp(0.0, 4.0);
    with_call(|c| {
        c.volume = volume;
        if let Some(m) = c.media.as_ref() {
            m.link.set_gain(volume);
        }
    })
    .ok_or("No call")?;
    if let Some(s) = snapshot() {
        emit_state(&s);
    }
    Ok(())
}

/// The socket the webview pushes its video through.
pub fn video_link_url() -> Result<String, String> {
    super::link::url()
}

/// Start or stop sending one of our pictures. The codec is the first of ours the
/// peer decodes.
pub async fn set_video(kind: VideoKind, on: bool) -> Result<(), String> {
    let tracks = with_call(|c| {
        if c.phase != Phase::Active {
            return Err("Not in a call".to_string());
        }
        let Some(track) = c.video.as_ref() else { return Err("No video track".to_string()) };
        let tracks = c.video_mine.with(kind, on);
        if tracks.any() {
            let codec = pick_codec(&c.peer_decodes).ok_or("They cannot receive video")?;
            track.set_codec(codec);
        }
        track.set_sending(tracks);
        c.video_mine = tracks;
        if !tracks.screen {
            c.share_audio_mine = false;
            c.share.active.store(false, Ordering::Relaxed);
        }
        c.refresh_awake();
        Ok((tracks, c.control.clone()))
    })
    .unwrap_or(Err("No call".into()))?;
    tell_tracks(tracks).await;
    Ok(())
}

/// Every send of what we are sending goes through here: the UI and the peer both hear.
async fn tell_tracks((tracks, control): (Tracks, Option<Arc<tokio::sync::Mutex<SendStream>>>)) {
    let audio = with_call(|c| c.share_audio_mine).unwrap_or(false);
    if let Some(s) = snapshot() {
        emit_state(&s);
    }
    if let Some(control) = control {
        let mut send = control.lock().await;
        let _ = tokio::time::timeout(Duration::from_secs(1), write_control(&mut *send, &Control::Video { camera: tracks.camera, screen: tracks.screen, audio })).await;
    }
}

/// The shared screen's sound goes with it, or stops; it cannot outlive the screen.
pub async fn set_share_audio(on: bool) -> Result<(), String> {
    let tracks = with_call(|c| {
        if c.phase != Phase::Active {
            return Err("Not in a call".to_string());
        }
        let on = on && c.video_mine.screen;
        c.share_audio_mine = on;
        c.share.active.store(on, Ordering::Relaxed);
        Ok((c.video_mine, c.control.clone()))
    })
    .unwrap_or(Err("No call".into()))?;
    tell_tracks(tracks).await;
    Ok(())
}

/// Listener-side volume for their screen's sound.
pub async fn set_share_volume(volume: f32) -> Result<(), String> {
    let volume = volume.clamp(0.0, 4.0);
    with_call(|c| {
        c.share_volume = volume;
        if let Some(m) = c.media.as_ref() {
            m.link.set_share_gain(volume);
        }
    })
    .ok_or("No call")?;
    if let Some(s) = snapshot() {
        emit_state(&s);
    }
    Ok(())
}

/// The user's quality and frame rate for one of our pictures; None means the ladder decides.
pub fn set_video_prefs(kind: VideoKind, rung: Option<usize>, fps: Option<u32>) -> Result<(), String> {
    with_call(|c| {
        let Some(track) = c.video.as_ref() else { return Err("No video track".to_string()) };
        track.set_prefs(kind, Prefs { rung, fps });
        Ok(())
    })
    .unwrap_or(Err("No call".into()))
}

/// Our view of their picture is hidden (or shown again): they may stop sending.
pub async fn set_video_pause(on: bool) -> Result<(), String> {
    let control = with_call(|c| c.control.clone()).ok_or("No call")?;
    if let Some(control) = control {
        let mut send = control.lock().await;
        let _ = tokio::time::timeout(Duration::from_secs(1), write_control(&mut *send, &Control::VideoPause { on })).await;
    }
    Ok(())
}

/// The webview's socket closed under a call: whatever it was sending has stopped.
async fn on_link_closed(id: &str) {
    let tracks = with_call_id(id, |c| {
        if !c.video_mine.any() {
            return None;
        }
        c.video_mine = Tracks::default();
        c.share_audio_mine = false;
        c.share.active.store(false, Ordering::Relaxed);
        if let Some(v) = c.video.as_ref() {
            v.set_sending(Tracks::default());
        }
        c.refresh_awake();
        Some((Tracks::default(), c.control.clone()))
    })
    .flatten();
    if let Some(t) = tracks {
        tell_tracks(t).await;
    }
}

/// The microphone test outside a call: the capture chain alone, feeding the meter.
struct MicTest {
    engine: MediaEngine,
    task: JoinHandle<()>,
}

static MIC_TEST: OnceLock<Mutex<Option<MicTest>>> = OnceLock::new();

fn mic_test_slot() -> &'static Mutex<Option<MicTest>> {
    MIC_TEST.get_or_init(|| Mutex::new(None))
}

pub fn mic_test_running() -> bool {
    mic_test_slot().lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

pub async fn mic_test_start() -> Result<(), String> {
    mic_test_stop();
    let engine = tokio::task::spawn_blocking(|| MediaEngine::start(None, 1.0, None, None, 1.0))
        .await
        .map_err(|e| e.to_string())??;
    let stats = Arc::clone(&engine.stats);
    let task = vector_core::db::spawn_bound(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            vector_core::traits::emit_event("mic_level", &LevelPayload {
                id: String::new(),
                mic: stats.mic_level(),
                peer: 0.0,
            });
        }
    });
    *mic_test_slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(MicTest { engine, task });
    Ok(())
}

pub fn mic_test_stop() {
    let taken = mic_test_slot().lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(t) = taken {
        t.task.abort();
        drop(t.engine);
    }
}

/// Called once at startup: when the default microphone or speaker changes, an
/// active call reopens its audio on the new devices.
pub fn install_device_follow() {
    if let Ok(mixer) = crate::audio_engine::AudioEngine::get() {
        mixer.on_device_change(Box::new(|| {
            if mic_test_running() {
                vector_core::db::spawn_bound(async move {
                    let _ = mic_test_start().await;
                });
            }
            let Some((id, conn)) = with_call(|c| (c.id.clone(), c.conn.clone())) else { return };
            let Some(conn) = conn else { return };
            vector_core::db::spawn_bound(async move {
                restart_media(&id, conn).await;
            });
        }));
    }
}

async fn restart_media(id: &str, conn: Connection) {
    // Drop the old engine first: it holds the mixer slot and the old input stream.
    let (old, muted, volume, share, share_volume) = match with_call_id(id, |c| (c.media.take(), c.muted, c.volume, Arc::clone(&c.share), c.share_volume)) {
        Some(v) => v,
        None => return,
    };
    // The counters outlive the engine: the liveness check reads them for the
    // call's life, and a reset reads as the peer having gone quiet.
    let stats = old.as_ref().map(|m| Arc::clone(&m.stats));
    drop(old);
    let media =
        tokio::task::spawn_blocking(move || MediaEngine::start(Some(conn), volume, stats, Some(share), share_volume)).await;
    match media {
        Ok(Ok(m)) => {
            m.muted.store(muted, Ordering::Relaxed);
            let installed = with_call_id(id, |c| c.media = Some(m)).is_some();
            if installed {
                log_info!("[CALLS] Audio reopened on the new default devices");
            }
        }
        _ => {
            log_warn!("[CALLS] Could not reopen audio after a device change");
            end(id, "audio_failed");
        }
    }
}

/// A signal from the peer, already validated as coming from `sender`.
pub async fn on_signal(sender: &str, call_id: &str, signal: &str, node_addr: Option<&str>, created_at: u64, video: Option<&str>, media: Option<&str>) {
    log_info!("[CALLS] {signal} from {sender} for call {call_id}");
    match signal {
        "offer" => {
            let now = Timestamp::now().as_secs();
            if created_at + OFFER_TTL_SECS < now {
                log_info!("[CALLS] Ignoring an offer {}s old", now.saturating_sub(created_at));
                return;
            }
            if node_addr.and_then(|a| decode_node_addr(a).ok()).is_none() {
                log_warn!("[CALLS] Offer without a usable address");
                return;
            }
            let state = {
                let mut guard = slot().lock().unwrap_or_else(|e| e.into_inner());
                match guard.as_ref() {
                    Some(c) if c.id == call_id => return,
                    Some(_) => None,
                    None => {
                        let call = Call {
                            id: call_id.to_string(),
                            peer: sender.to_string(),
                            outgoing: false,
                            phase: Phase::Ringing,
                            conn: None,
                            media: None,
                            control: None,
                            tasks: Vec::new(),
                            active_since: None,
                            muted: false,
                            peer_muted: false,
                            volume: 1.0,
                            video: None,
                            video_mine: Tracks::default(),
                            video_peer: Tracks::default(),
                            peer_decodes: parse_decodes(video),
                            video_offered: media == Some("video"),
                            paused_by_peer: false,
                            awake: None,
                            share: Arc::new(ShareInput::new()),
                            share_audio_mine: false,
                            share_audio_peer: false,
                            share_volume: 1.0,
                        };
                        let state = call.state(None);
                        *guard = Some(call);
                        Some(state)
                    }
                }
            };
            match state {
                Some(state) => {
                    emit_state(&state);
                    spawn_ring_timeout(call_id.to_string(), sender.to_string(), false);
                }
                None => {
                    send_signal(sender, call_id, "busy", None, false).await;
                }
            }
        }
        "answer" => {
            let Some(addr) = node_addr.and_then(|a| decode_node_addr(a).ok()) else {
                log_warn!("[CALLS] Answer without a usable address");
                return;
            };
            let accepted = with_call_id(call_id, |c| {
                if c.outgoing && c.peer == sender && c.phase == Phase::Ringing {
                    c.phase = Phase::Connecting;
                    c.peer_decodes = parse_decodes(video);
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
            if !accepted {
                return;
            }
            if let Some(s) = snapshot() {
                emit_state(&s);
            }
            // The peer's other devices are still ringing; tell them which one answered.
            send_signal(sender, call_id, "taken", node_addr, false).await;
            let id = call_id.to_string();
            let peer = sender.to_string();
            let task = vector_core::db::spawn_bound(async move {
                match connect(addr, &id).await {
                    Ok((conn, send, recv)) => attach(&id, conn, send, recv).await,
                    Err(e) => {
                        log_warn!("[CALLS] Connect failed: {e}");
                        send_signal(&peer, &id, "hangup", None, false).await;
                        end(&id, "connect_failed");
                    }
                }
            });
            with_call_id_push_task(task);
        }
        "reject" | "busy" | "hangup" => {
            if with_call_id(call_id, |c| c.peer == sender).unwrap_or(false) {
                end(call_id, signal);
            }
        }
        // Another of our devices answered: only a device still ringing stands down.
        "taken" => {
            let ringing = with_call_id(call_id, |c| c.peer == sender && !c.outgoing && c.phase == Phase::Ringing).unwrap_or(false);
            if ringing {
                end(call_id, "answered_elsewhere");
            }
        }
        other => log_warn!("[CALLS] Unknown signal {other}"),
    }
}

async fn connect(addr: EndpointAddr, call_id: &str) -> Result<(Connection, SendStream, RecvStream), String> {
    let iroh = iroh().await?;
    let conn = tokio::time::timeout(CONNECT_TIMEOUT, iroh.endpoint.connect(addr, CALL_ALPN))
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let (mut send, recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
    write_control(&mut send, &Control::Hello { call_id: call_id.to_string() }).await?;
    Ok((conn, send, recv))
}

/// The accept loop hands over a connection with our ALPN. It belongs to the call
/// whose id its Hello names, if that call is waiting for one.
pub async fn on_incoming(conn: Connection) {
    let handshake = tokio::time::timeout(Duration::from_secs(10), async {
        let (send, mut recv) = conn.accept_bi().await.map_err(|e| e.to_string())?;
        let hello = read_control(&mut recv).await?;
        Ok::<_, String>((send, recv, hello))
    })
    .await;
    let (send, recv, hello) = match handshake {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            log_warn!("[CALLS] Incoming connection without a Hello: {e}");
            conn.close(VarInt::from_u32(1), b"no hello");
            return;
        }
        Err(_) => {
            conn.close(VarInt::from_u32(1), b"hello timeout");
            return;
        }
    };
    let Control::Hello { call_id } = hello else {
        conn.close(VarInt::from_u32(1), b"expected hello");
        return;
    };
    let waiting = with_call_id(&call_id, |c| !c.outgoing && c.phase == Phase::Connecting).unwrap_or(false);
    if !waiting {
        log_warn!("[CALLS] Connection for call {call_id} that is not waiting");
        conn.close(VarInt::from_u32(2), b"not expected");
        return;
    }
    attach(&call_id, conn, send, recv).await;
}

/// Media up, watchers on, Active.
async fn attach(id: &str, conn: Connection, send: SendStream, mut recv: RecvStream) {
    let media_conn = conn.clone();
    let (volume, share, share_volume) = with_call_id(id, |c| (c.volume, Arc::clone(&c.share), c.share_volume)).unwrap_or((1.0, Arc::new(ShareInput::new()), 1.0));
    let media_share = Arc::clone(&share);
    let media = tokio::task::spawn_blocking(move || MediaEngine::start(Some(media_conn), volume, None, Some(media_share), share_volume)).await;
    let media = match media {
        Ok(Ok(m)) => m,
        Ok(Err(e)) => {
            log_warn!("[CALLS] Media failed: {e}");
            let peer = with_call_id(id, |c| c.peer.clone());
            if let Some(peer) = peer {
                send_signal(&peer, id, "hangup", None, false).await;
            }
            conn.close(VarInt::from_u32(3), b"no audio");
            end(id, "audio_failed");
            return;
        }
        Err(_) => {
            end(id, "audio_failed");
            return;
        }
    };
    let muted = with_call_id(id, |c| c.muted).unwrap_or(false);
    media.muted.store(muted, Ordering::Relaxed);
    let control = Arc::new(tokio::sync::Mutex::new(send));
    let stats = Arc::clone(&media.stats);

    let peer_video = with_call_id(id, |c| !c.peer_decodes.is_empty()).unwrap_or(false);
    let video = VideoTrack::start(
        conn.clone(),
        Hooks {
            call_id: id.to_string(),
            control: Arc::clone(&control),
            on_link_closed: Arc::new(|id: &str| {
                let id = id.to_string();
                vector_core::db::spawn_bound(async move { on_link_closed(&id).await });
            }),
            on_caps: Arc::new(|encode, decode| set_video_caps(encode, decode)),
            audio: Arc::clone(&stats),
            peer_video,
            share,
        },
    );
    let installed = with_call_id(id, |c| {
        c.conn = Some(conn.clone());
        c.media = Some(media);
        c.control = Some(Arc::clone(&control));
        c.video = Some(video);
        c.phase = Phase::Active;
        c.active_since = Some(Instant::now());
        c.refresh_awake();
    })
    .is_some();
    if !installed {
        conn.close(VarInt::from_u32(0), b"gone");
        return;
    }
    if let Some(s) = snapshot() {
        emit_state(&s);
    }

    // Peer's control messages.
    let ctl_id = id.to_string();
    let ctl = vector_core::db::spawn_bound(async move {
        loop {
            match read_control(&mut recv).await {
                Ok(Control::Mute { on }) => {
                    with_call_id(&ctl_id, |c| c.peer_muted = on);
                    if let Some(s) = snapshot() {
                        emit_state(&s);
                    }
                }
                Ok(Control::Bye) => {
                    end(&ctl_id, "hangup");
                    break;
                }
                Ok(Control::Video { camera, screen, audio }) => {
                    let tracks = Tracks { camera, screen };
                    with_call_id(&ctl_id, |c| {
                        c.video_peer = tracks;
                        c.share_audio_peer = audio && screen;
                        if let Some(v) = c.video.as_ref() {
                            v.set_peer(tracks);
                        }
                        c.refresh_awake();
                    });
                    if let Some(s) = snapshot() {
                        emit_state(&s);
                    }
                }
                Ok(Control::KeyframeRequest { kind }) => {
                    with_call_id(&ctl_id, |c| {
                        if let Some(v) = c.video.as_ref() {
                            v.force_keyframe(kind);
                        }
                    });
                }
                Ok(Control::VideoPause { on }) => {
                    with_call_id(&ctl_id, |c| {
                        c.paused_by_peer = on;
                        if let Some(v) = c.video.as_ref() {
                            v.set_paused(on);
                        }
                    });
                    if let Some(s) = snapshot() {
                        emit_state(&s);
                    }
                }
                Ok(Control::VideoUnsupported { codec }) => {
                    // Send with what is left; with nothing left, video stops and the UI says so.
                    let repick = with_call_id(&ctl_id, |c| {
                        c.peer_decodes.retain(|d| d != codec.name());
                        if !c.video_mine.any() {
                            return None;
                        }
                        match (pick_codec(&c.peer_decodes), c.video.as_ref()) {
                            (Some(next), Some(track)) => {
                                track.set_codec(next);
                                Some(true)
                            }
                            _ => Some(false),
                        }
                    })
                    .flatten();
                    if repick == Some(false) {
                        log_warn!("[CALLS] Peer cannot decode any codec we encode");
                        let _ = set_video(VideoKind::Camera, false).await;
                        let _ = set_video(VideoKind::Screen, false).await;
                        vector_core::traits::emit_event("call_video_refused", &serde_json::json!({ "id": ctl_id }));
                    }
                }
                Ok(Control::Hello { .. }) | Ok(Control::Unknown) => {}
                Err(_) => break,
            }
        }
    });
    // The transport going away under us.
    let closed_id = id.to_string();
    let closed_conn = conn.clone();
    let closed = vector_core::db::spawn_bound(async move {
        // A close carrying our own "bye" is a hangup; the Bye on the control stream can
        // still be in flight when the close lands.
        let reason = match closed_conn.closed().await {
            iroh::endpoint::ConnectionError::ApplicationClosed(ac) if ac.reason.as_ref() == b"bye" => "hangup",
            _ => "disconnected",
        };
        end(&closed_id, reason);
    });
    // A stats line every second, and the liveness check: the peer sends fifty frames
    // a second even muted, so a silent stretch means they are gone long before the
    // transport's idle timeout would say so.
    let stats_id = id.to_string();
    let stats_conn = conn.clone();
    let stats_task = vector_core::db::spawn_bound(async move {
        let mut last_received = 0u64;
        let mut silent_secs = 0u32;
        let mut tick = 0u32;
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            // Meters ten times a second; the rest once a second.
            vector_core::traits::emit_event("call_level", &LevelPayload {
                id: stats_id.clone(),
                mic: stats.mic_level(),
                peer: stats.peer_level(),
            });
            tick += 1;
            if tick % 10 != 0 {
                continue;
            }
            let received = stats.received.load(Ordering::Relaxed);
            silent_secs = if received == last_received { silent_secs + 1 } else { 0 };
            last_received = received;
            if silent_secs >= PEER_SILENCE_SECS {
                end(&stats_id, "disconnected");
                return;
            }
            let (rtt_ms, path) = {
                let paths = stats_conn.paths();
                match paths.iter().find(|p| p.is_selected()).or_else(|| paths.iter().next()) {
                    Some(p) => (p.rtt().as_millis() as u32, if p.is_relay() { "relay" } else { "direct" }),
                    None => (0, "none"),
                }
            };
            let payload = CallStats {
                id: stats_id.clone(),
                rtt_ms,
                path,
                sent: stats.sent.load(Ordering::Relaxed),
                send_dropped: stats.send_dropped.load(Ordering::Relaxed),
                received: stats.received.load(Ordering::Relaxed),
                lost: stats.lost.load(Ordering::Relaxed),
                shed: stats.shed.load(Ordering::Relaxed),
                rebuilt: stats.rebuilt.load(Ordering::Relaxed),
                concealed: stats.concealed.load(Ordering::Relaxed),
                late: stats.late.load(Ordering::Relaxed),
                clicks_cut: stats.clicks_cut.load(Ordering::Relaxed),
                depth_ms: stats.depth_ms.load(Ordering::Relaxed),
                jitter_ms: stats.jitter_ms.load(Ordering::Relaxed),
                bitrate_kbps: stats.bitrate_kbps.load(Ordering::Relaxed),
                net_loss: stats.net_loss(),
                video: with_call_id(&stats_id, |c| c.video.as_ref().map(|v| v.snapshot())).flatten().unwrap_or_default(),
                share_sent: stats.share_sent.load(Ordering::Relaxed),
                share_received: stats.share_received.load(Ordering::Relaxed),
            };
            vector_core::traits::emit_event("call_stats", &payload);
        }
    });
    let mut tasks = Some(vec![ctl, closed, stats_task]);
    with_call_id(id, |c| c.tasks.extend(tasks.take().unwrap_or_default()));
    if let Some(orphans) = tasks {
        for t in orphans {
            t.abort();
        }
        end(id, "hangup");
    }
}
