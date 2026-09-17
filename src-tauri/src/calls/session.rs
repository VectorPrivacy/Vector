//! One call at a time: the state machine, its signalling over gift-wrapped rumors,
//! and the Iroh connection that carries the media once both sides have agreed.
//!
//! Every deferred step (a ring timeout, a connect, a closed watcher) carries the
//! call id it was started for and does nothing if the call it finds is another.

use super::media::MediaEngine;
use super::transport::{read_control, write_control, Control, CALL_ALPN};
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
}

impl Call {
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
        }
    }
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
async fn send_signal(peer: &str, call_id: &str, signal: &str, addr: Option<&str>) -> bool {
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
    // Media first: its threads hold the connection and the mixer slot.
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

pub async fn start(peer: String) -> Result<CallState, String> {
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
        };
        let state = call.state(None);
        *guard = Some(call);
        state
    };
    emit_state(&state);

    if !send_signal(&peer, &id, "offer", Some(&addr)).await {
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
            send_signal(&peer, &id, "hangup", None).await;
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
    if !send_signal(&peer, &id, "answer", Some(&addr)).await {
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
    send_signal(&peer, &id, "reject", None).await;
    end(&id, "rejected");
    Ok(())
}

pub async fn hangup() -> Result<(), String> {
    let (id, peer, control) = with_call(|c| (c.id.clone(), c.peer.clone(), c.control.clone())).ok_or("No call")?;
    if let Some(control) = control {
        let mut send = control.lock().await;
        let _ = tokio::time::timeout(Duration::from_secs(1), write_control(&mut send, &Control::Bye)).await;
    }
    // Local teardown first: the peer's close must not be what ends our side.
    end(&id, "hangup");
    // The signal reaches a peer whose connection never came up, or one still ringing.
    send_signal(&peer, &id, "hangup", None).await;
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
        let _ = tokio::time::timeout(Duration::from_secs(1), write_control(&mut send, &Control::Mute { on })).await;
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
    let engine = tokio::task::spawn_blocking(|| MediaEngine::start(None, 1.0, None))
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
    let (old, muted, volume) = match with_call_id(id, |c| (c.media.take(), c.muted, c.volume)) {
        Some(v) => v,
        None => return,
    };
    // The counters outlive the engine: the liveness check reads them for the
    // call's life, and a reset reads as the peer having gone quiet.
    let stats = old.as_ref().map(|m| Arc::clone(&m.stats));
    drop(old);
    let media =
        tokio::task::spawn_blocking(move || MediaEngine::start(Some(conn), volume, stats)).await;
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
pub async fn on_signal(sender: &str, call_id: &str, signal: &str, node_addr: Option<&str>, created_at: u64) {
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
                    send_signal(sender, call_id, "busy", None).await;
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
            send_signal(sender, call_id, "taken", node_addr).await;
            let id = call_id.to_string();
            let peer = sender.to_string();
            let task = vector_core::db::spawn_bound(async move {
                match connect(addr, &id).await {
                    Ok((conn, send, recv)) => attach(&id, conn, send, recv).await,
                    Err(e) => {
                        log_warn!("[CALLS] Connect failed: {e}");
                        send_signal(&peer, &id, "hangup", None).await;
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
    let volume = with_call_id(id, |c| c.volume).unwrap_or(1.0);
    let media = tokio::task::spawn_blocking(move || MediaEngine::start(Some(media_conn), volume, None)).await;
    let media = match media {
        Ok(Ok(m)) => m,
        Ok(Err(e)) => {
            log_warn!("[CALLS] Media failed: {e}");
            let peer = with_call_id(id, |c| c.peer.clone());
            if let Some(peer) = peer {
                send_signal(&peer, id, "hangup", None).await;
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

    let installed = with_call_id(id, |c| {
        c.conn = Some(conn.clone());
        c.media = Some(media);
        c.control = Some(Arc::clone(&control));
        c.phase = Phase::Active;
        c.active_since = Some(Instant::now());
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
                Ok(Control::Hello { .. }) => {}
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
