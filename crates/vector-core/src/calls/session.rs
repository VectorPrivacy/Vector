//! One call at a time: the state machine, its signalling over gift-wrapped rumors,
//! and the Iroh connection that carries the media once both sides have agreed.
//!
//! Every deferred step (a ring timeout, a connect, a closed watcher) carries the
//! call id it was started for and does nothing if the call it finds is another.

use super::platform::{self, AudioStart, AwakeLevel, CallAudio};
use super::share::ShareInput;
use super::video::{Hooks, Prefs, VideoSnapshot, VideoTrack};
use super::wire::{read_control, write_control, Control, Tracks, VideoCodec, VideoKind, CALL_ALPN};
use crate::event_ext::FinalizeUnsignedWithId;
use crate::rt::time::{sleep, timeout};
use crate::rt::JoinHandle;
use crate::state::{active_trusted_relays, my_public_key, nostr_client};
use iroh::endpoint::{Connection, RecvStream, SendStream, VarInt};
use iroh::EndpointAddr;
use nostr_sdk::prelude::*;
use serde::Serialize;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use web_time::Instant;

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
    /// The call has no microphone (access refused, or none there): it stays muted.
    pub mic_off: bool,
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
    /// This platform can capture the screen's sound itself, without the webview.
    pub share_audio_native: bool,
    /// The system has refused screen recording and will not ask again.
    pub share_audio_denied: bool,
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
    media: Option<Box<dyn CallAudio>>,
    control: Option<Arc<tokio::sync::Mutex<SendStream>>>,
    tasks: Vec<JoinHandle<()>>,
    active_since: Option<Instant>,
    muted: bool,
    peer_muted: bool,
    mic_off: bool,
    volume: f32,
    video: Option<VideoTrack>,
    video_mine: Tracks,
    video_peer: Tracks,
    peer_decodes: Vec<String>,
    video_offered: bool,
    paused_by_peer: bool,
    /// The machine stays awake for the call, and the display too once there is video.
    awake: Option<Box<dyn std::any::Any + Send>>,
    /// The shared screen's sound from the webview, for the engine's share thread.
    share: Arc<ShareInput>,
    share_audio_mine: bool,
    share_audio_peer: bool,
    share_volume: f32,
    share_audio_denied: bool,
}

impl Call {
    /// The hold the call needs right now; dropping the old one after taking the new
    /// keeps the machine covered in between.
    fn refresh_awake(&mut self) {
        let level = if self.phase != Phase::Active {
            None
        } else if self.video_mine.any() || self.video_peer.any() {
            Some(AwakeLevel::Display)
        } else {
            Some(AwakeLevel::System)
        };
        let next = level.and_then(|l| platform::get().ok()?.hold_awake(l));
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
            mic_off: self.mic_off,
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
            share_audio_native: platform::get().is_ok_and(|p| p.share_audio_native()),
            share_audio_denied: self.share_audio_denied,
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

/// Offers seen within `OFFER_TTL_SECS`: a relay can re-serve one under a new wrapper id, and
/// it must not ring again once its call ended. Call ids are random, so no per-account scope.
static SEEN_OFFERS: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

/// True the first time `call_id` is offered within the TTL window.
fn first_offer(call_id: &str, now: u64) -> bool {
    let mut seen = SEEN_OFFERS.lock().unwrap_or_else(|e| e.into_inner());
    seen.retain(|(_, at)| at + OFFER_TTL_SECS >= now);
    if seen.iter().any(|(id, _)| id == call_id) {
        return false;
    }
    seen.push((call_id.to_string(), now));
    true
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
    crate::traits::emit_event("call_state", state);
}

pub fn snapshot() -> Option<CallState> {
    with_call(|c| c.state(None))
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
    match crate::send_gift_wrap(&client, relays, &pubkey, rumor, []).await {
        Ok(_) => true,
        Err(e) => {
            log_warn!("[CALLS] Failed to send {signal} to {peer}: {e}");
            false
        }
    }
}

/// Tears the call down and tells the UI why. Idempotent: a second end is a no-op.
pub fn end(id: &str, reason: &str) {
    if take_down(id, reason) {
        if let Ok(p) = platform::get() {
            p.ended();
        }
    }
}

/// Account swap or shutdown: whatever is up comes down without a signal.
pub fn end_all(reason: &str) {
    if let Some(id) = with_call(|c| c.id.clone()) {
        take_down(&id, reason);
    }
}

fn take_down(id: &str, reason: &str) -> bool {
    let call = {
        let mut guard = slot().lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(c) if c.id == id => guard.take(),
            _ => None,
        }
    };
    let Some(mut call) = call else { return false };
    log_info!("[CALLS] Call {} ended: {reason}", call.id);
    for t in call.tasks.drain(..) {
        t.abort();
    }
    // Media first: its threads hold the connection and the mixer slot. The video
    // track's drop closes the link, which stops the camera.
    stop_share_audio();
    drop(call.video.take());
    drop(call.media.take());
    if let Some(conn) = call.conn.take() {
        conn.close(VarInt::from_u32(0), b"bye");
    }
    call.phase = Phase::Ended;
    emit_state(&call.state(Some(reason.to_string())));
    true
}

pub async fn start(peer: String, video: bool) -> Result<CallState, String> {
    if PublicKey::from_bech32(&peer).is_err() {
        return Err("Not a valid npub".into());
    }
    crate::transport::realtime::check()?;
    let platform = platform::get()?;
    let id = format!("{:032x}", rand::random::<u128>());

    // In the slot before the node is read: a client that closes an idle node
    // must see this call, or the offer could name a node already gone.
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
            mic_off: false,
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
            share_audio_denied: false,
        };
        let state = call.state(None);
        *guard = Some(call);
        state
    };
    let addr = match platform.local_addr().await {
        Ok(a) => a,
        Err(e) => {
            let mut guard = slot().lock().unwrap_or_else(|e| e.into_inner());
            if guard.as_ref().is_some_and(|c| c.id == id) {
                *guard = None;
            }
            return Err(e);
        }
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
    let task = crate::db::spawn_bound(async move {
        sleep(RING_TIMEOUT).await;
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
    crate::transport::realtime::check()?;
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

    let addr = match platform::get() {
        Ok(p) => p.local_addr().await,
        Err(e) => Err(e),
    };
    let addr = match addr {
        Ok(a) => a,
        Err(e) => {
            end(&id, "connect_failed");
            return Err(e);
        }
    };
    if !send_signal(&peer, &id, "answer", Some(&addr), false).await {
        end(&id, "send_failed");
        return Err("Could not reach the relays".into());
    }
    let timeout_id = id.clone();
    let task = crate::db::spawn_bound(async move {
        sleep(CONNECT_TIMEOUT).await;
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
        let _ = timeout(Duration::from_secs(1), write_control(&mut *send, &Control::Bye)).await;
    }
    // Local teardown first: the peer's close must not be what ends our side.
    end(&id, "hangup");
    // The signal reaches a peer whose connection never came up, or one still ringing.
    send_signal(&peer, &id, "hangup", None, false).await;
    Ok(())
}

pub async fn set_muted(on: bool) -> Result<(), String> {
    let control = with_call(|c| {
        c.muted = on;
        if let Some(m) = c.media.as_ref() {
            m.set_muted(on);
        }
        c.control.clone()
    })
    .ok_or("No call")?;
    if let Some(s) = snapshot() {
        emit_state(&s);
    }
    if let Some(control) = control {
        let mut send = control.lock().await;
        let _ = timeout(Duration::from_secs(1), write_control(&mut *send, &Control::Mute { on })).await;
    }
    Ok(())
}

/// Listener-side volume for the call in progress.
pub async fn set_volume(volume: f32) -> Result<(), String> {
    let volume = volume.clamp(0.0, 4.0);
    with_call(|c| {
        c.volume = volume;
        if let Some(m) = c.media.as_ref() {
            m.set_volume(volume);
        }
    })
    .ok_or("No call")?;
    if let Some(s) = snapshot() {
        emit_state(&s);
    }
    Ok(())
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
            stop_share_audio();
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
        let _ = timeout(Duration::from_secs(1), write_control(&mut *send, &Control::Video { camera: tracks.camera, screen: tracks.screen, audio })).await;
    }
}

/// The shared screen's sound goes with it, or stops; it cannot outlive the screen.
/// `native` asks this platform to capture it itself, for a picker that gave none.
pub async fn set_share_audio(on: bool, native: bool) -> Result<(), String> {
    let (on, share) = with_call(|c| {
        if c.phase != Phase::Active {
            return Err("Not in a call".to_string());
        }
        Ok((on && c.video_mine.screen, Arc::clone(&c.share)))
    })
    .unwrap_or(Err("No call".into()))?;
    let platform = platform::get()?;
    if on && native {
        if let Err(e) = platform.start_share_audio(share).await {
            // A declined prompt is a plain no; a standing refusal is for the panel to show.
            let denied = !platform.share_audio_permitted();
            with_call(|c| c.share_audio_denied = denied);
            if let Some(s) = snapshot() {
                emit_state(&s);
            }
            return Err(if denied { "Screen recording is off for Vector in System Settings".to_string() } else { e });
        }
        with_call(|c| c.share_audio_denied = false);
    } else {
        platform.stop_share_audio().await;
    }
    let tracks = with_call(|c| {
        c.share_audio_mine = on;
        c.share.active.store(on, Ordering::Relaxed);
        (c.video_mine, c.control.clone())
    })
    .ok_or("No call")?;
    tell_tracks(tracks).await;
    Ok(())
}

/// Listener-side volume for their screen's sound.
pub async fn set_share_volume(volume: f32) -> Result<(), String> {
    let volume = volume.clamp(0.0, 4.0);
    with_call(|c| {
        c.share_volume = volume;
        if let Some(m) = c.media.as_ref() {
            m.set_share_volume(volume);
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
        let _ = timeout(Duration::from_secs(1), write_control(&mut *send, &Control::VideoPause { on })).await;
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

/// Reopens the call's audio, on whatever devices are now the defaults.
pub async fn restart_audio() {
    let Some((id, Some(conn))) = with_call(|c| (c.id.clone(), c.conn.clone())) else { return };
    let Ok(platform) = platform::get() else { return };
    // Drop the old engine first: it holds the devices.
    let Some((old, muted, volume, share, share_volume)) =
        with_call_id(&id, |c| (c.media.take(), c.muted, c.volume, Arc::clone(&c.share), c.share_volume))
    else {
        return;
    };
    // The counters outlive the engine: the liveness check reads them for the
    // call's life, and a reset reads as the peer having gone quiet.
    let stats = old.as_ref().map(|m| m.stats());
    drop(old);
    match platform.start_audio(conn, AudioStart { volume, share_volume, stats, share }).await {
        Ok(m) => {
            m.set_muted(muted);
            let mic_off = !m.has_mic();
            if with_call_id(&id, |c| {
                c.mic_off = mic_off;
                c.media = Some(m);
            })
            .is_some()
            {
                log_info!("[CALLS] Audio reopened on the new default devices");
                if let Some(s) = snapshot() {
                    emit_state(&s);
                }
            }
        }
        Err(e) => {
            log_warn!("[CALLS] Could not reopen audio after a device change: {e}");
            end(&id, "audio_failed");
        }
    }
}

/// Stops the platform's own capture of the screen's sound, if it had one going.
fn stop_share_audio() {
    if let Ok(p) = platform::get() {
        crate::db::spawn_bound(async move { p.stop_share_audio().await });
    }
}

/// A signal from the peer, already validated as coming from `sender`.
pub async fn on_signal(sender: &str, call_id: &str, signal: &str, node_addr: Option<&str>, created_at: u64, video: Option<&str>, media: Option<&str>) {
    log_info!("[CALLS] {signal} from {sender} for call {call_id}");
    let Ok(platform) = platform::get() else { return };
    match signal {
        "offer" => {
            let now = Timestamp::now().as_secs();
            if created_at + OFFER_TTL_SECS < now {
                log_info!("[CALLS] Ignoring an offer {}s old", now.saturating_sub(created_at));
                return;
            }
            if node_addr.and_then(|a| platform.decode_addr(a)).is_none() {
                log_warn!("[CALLS] Offer without a usable address");
                return;
            }
            if !first_offer(call_id, now) {
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
                            mic_off: false,
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
                            share_audio_denied: false,
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
            let Some(addr) = node_addr.and_then(|a| platform.decode_addr(a)) else {
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
            let task = crate::db::spawn_bound(async move {
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
    let endpoint = platform::get()?.endpoint().await?;
    let conn = timeout(CONNECT_TIMEOUT, endpoint.connect(addr, CALL_ALPN))
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
    let handshake = timeout(Duration::from_secs(10), async {
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
    let platform = match platform::get() {
        Ok(p) => p,
        Err(_) => {
            conn.close(VarInt::from_u32(3), b"no audio");
            end(id, "audio_failed");
            return;
        }
    };
    let (volume, share, share_volume) = with_call_id(id, |c| (c.volume, Arc::clone(&c.share), c.share_volume)).unwrap_or((1.0, Arc::new(ShareInput::new()), 1.0));
    let start = AudioStart { volume, share_volume, stats: None, share: Arc::clone(&share) };
    let media = match platform.start_audio(conn.clone(), start).await {
        Ok(m) => m,
        Err(e) => {
            log_warn!("[CALLS] Media failed: {e}");
            let peer = with_call_id(id, |c| c.peer.clone());
            if let Some(peer) = peer {
                send_signal(&peer, id, "hangup", None, false).await;
            }
            conn.close(VarInt::from_u32(3), b"no audio");
            end(id, "audio_failed");
            return;
        }
    };
    let muted = with_call_id(id, |c| c.muted).unwrap_or(false);
    media.set_muted(muted);
    let control = Arc::new(tokio::sync::Mutex::new(send));
    let stats = media.stats();

    let peer_video = with_call_id(id, |c| !c.peer_decodes.is_empty()).unwrap_or(false);
    let video = VideoTrack::start(
        conn.clone(),
        Hooks {
            call_id: id.to_string(),
            control: Arc::clone(&control),
            on_link_closed: Arc::new(|id: &str| {
                let id = id.to_string();
                crate::db::spawn_bound(async move { on_link_closed(&id).await });
            }),
            on_caps: Arc::new(set_video_caps),
            audio: Arc::clone(&stats),
            peer_video,
            share,
        },
        platform,
    );
    let mic_off = !media.has_mic();
    let installed = with_call_id(id, |c| {
        c.conn = Some(conn.clone());
        c.mic_off = mic_off;
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
    let ctl = crate::db::spawn_bound(async move {
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
                        crate::traits::emit_event("call_video_refused", &serde_json::json!({ "id": ctl_id }));
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
    let closed = crate::db::spawn_bound(async move {
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
    let stats_task = crate::db::spawn_bound(async move {
        let mut last_received = 0u64;
        let mut silent_secs = 0u32;
        let mut tick = 0u32;
        loop {
            sleep(Duration::from_millis(100)).await;
            // Meters ten times a second; the rest once a second.
            crate::traits::emit_event("call_level", &LevelPayload {
                id: stats_id.clone(),
                mic: stats.mic_level(),
                peer: stats.peer_level(),
            });
            tick += 1;
            if !tick.is_multiple_of(10) {
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
            crate::traits::emit_event("call_stats", &payload);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offer_rings_once_until_its_ttl_passes() {
        let id = format!("{:032x}", rand::random::<u128>());
        assert!(first_offer(&id, 1_000));
        assert!(!first_offer(&id, 1_000 + OFFER_TTL_SECS));
        assert!(first_offer(&format!("{id}x"), 1_000));
        assert!(first_offer(&id, 1_001 + OFFER_TTL_SECS));
    }
}
