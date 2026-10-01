//! Calls in the browser. The session is core's (`vector_core::calls::session`);
//! this is the platform under it. The worker's Iroh endpoint carries the call.
//! The page captures and plays on AudioWorklets, where its browser cancels the
//! echo. Opus runs through WebCodecs in this worker's JS (`web/calls-media.js`).
//! The datagrams, the jitter buffer and the rate ladder stay here.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use vector_core::calls::jitter::{Jitter, Pop, MAX_PLC_RUN};
use vector_core::calls::platform::{AudioStart, CallAudio, CallPlatform, LinkConn};
use vector_core::calls::rate::RateControl;
use vector_core::calls::session;
use vector_core::calls::settings::{self, AudioSettings};
use vector_core::calls::stats::MediaStats;
use vector_core::calls::wire::{pack, unpack, VideoKind};
use vector_core::calls::FRAME_MS;
use vector_core::rt::JoinHandle;
use wasm_bindgen::prelude::*;
use web_time::Instant;

use crate::commands::{to_value, Args};
use crate::miniapps::realtime;

/// Datagram flags, as desktop's engine sets them.
const FLAG_MUTED: u16 = 1;
const FLAG_SHARE: u16 = 2;
/// Time the page has to bring its microphone up once the call connects.
const AUDIO_START_TIMEOUT: Duration = Duration::from_secs(20);

pub struct WebCalls;

#[async_trait::async_trait(?Send)]
impl CallPlatform for WebCalls {
    async fn endpoint(&self) -> Result<Endpoint, String> {
        Ok(realtime::iroh().await?.endpoint().clone())
    }

    async fn local_addr(&self) -> Result<String, String> {
        realtime::encode_node_addr(&realtime::iroh().await?.node_addr())
    }

    fn decode_addr(&self, addr: &str) -> Option<EndpointAddr> {
        realtime::decode_node_addr(addr).ok()
    }

    async fn start_audio(&self, conn: Connection, start: AudioStart) -> Result<Box<dyn CallAudio>, String> {
        WebAudio::start(conn, start).await
    }

    fn set_video_taker(&self, taker: Option<mpsc::Sender<LinkConn>>) {
        let closing = taker.is_none();
        *TAKER.lock().unwrap_or_else(|e| e.into_inner()) = taker;
        if closing {
            LINK.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
    }

    fn ended(&self) {
        tell("ended", Value::Null);
    }
}

pub fn install() {
    vector_core::calls::platform::install(Arc::new(WebCalls));
}

// --- The worker's media half ----------------------------------------------------

thread_local! {
    static SINK: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
}

/// `web/calls-media.js` hears the engine's instructions through this.
#[wasm_bindgen]
pub fn set_call_sink(sink: js_sys::Function) {
    SINK.with(|s| *s.borrow_mut() = Some(sink));
}

fn tell(op: &str, payload: Value) {
    SINK.with(|s| {
        if let Some(f) = s.borrow().as_ref() {
            let _ = f.call2(&JsValue::NULL, &JsValue::from_str(op), &JsValue::from_str(&payload.to_string()));
        }
    });
}

fn tell_bytes(op: &str, payload: Value, bytes: &[u8]) {
    SINK.with(|s| {
        if let Some(f) = s.borrow().as_ref() {
            let data = js_sys::Uint8Array::from(bytes);
            let _ = f.call3(&JsValue::NULL, &JsValue::from_str(op), &JsValue::from_str(&payload.to_string()), &data);
        }
    });
}

struct Engine {
    conn: Connection,
    stats: Arc<MediaStats>,
    muted: AtomicBool,
    started: Instant,
    jitter: Mutex<Jitter>,
    plc_run: AtomicU32,
    rate: Mutex<(RateControl, Instant)>,
}

static ENGINE: Mutex<Option<Arc<Engine>>> = Mutex::new(None);
static READY: Mutex<Option<oneshot::Sender<Result<(), String>>>> = Mutex::new(None);

fn engine() -> Option<Arc<Engine>> {
    ENGINE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn now_ms() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

struct WebAudio {
    engine: Arc<Engine>,
    rx: JoinHandle<()>,
}

impl WebAudio {
    async fn start(conn: Connection, start: AudioStart) -> Result<Box<dyn CallAudio>, String> {
        let stats = start.stats.unwrap_or_default();
        let mut rate = RateControl::new();
        let net = conn.stats();
        rate.prime(net.udp_tx.datagrams, net.lost_packets);
        stats.bitrate_kbps.store(rate.kbps(), Ordering::Relaxed);
        let kbps = rate.kbps();
        let engine = Arc::new(Engine {
            conn: conn.clone(),
            stats,
            muted: AtomicBool::new(false),
            started: Instant::now(),
            jitter: Mutex::new(Jitter::default()),
            plc_run: AtomicU32::new(0),
            rate: Mutex::new((rate, Instant::now())),
        });

        let (tx, rx) = oneshot::channel();
        *READY.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        *ENGINE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&engine));
        tell("start", json!({ "kbps": kbps, "fec": 10, "volume": start.volume, "settings": settings::load() }));
        let ready = vector_core::rt::time::timeout(AUDIO_START_TIMEOUT, rx).await;
        let failure = match ready {
            Ok(Ok(Ok(()))) => None,
            Ok(Ok(Err(e))) => Some(e),
            _ => Some("The microphone did not start".to_string()),
        };
        if let Some(e) = failure {
            release(&engine);
            return Err(e);
        }

        let rx_engine = Arc::clone(&engine);
        let rx = vector_core::db::spawn_bound(async move {
            while let Ok(datagram) = rx_engine.conn.read_datagram().await {
                let Some(frame) = unpack(&datagram) else { continue };
                if frame.flags & FLAG_SHARE != 0 {
                    // A screen's sound: counted, not yet played here.
                    rx_engine.stats.share_received.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                rx_engine.stats.received.fetch_add(1, Ordering::Relaxed);
                let mut j = rx_engine.jitter.lock().unwrap_or_else(|e| e.into_inner());
                j.push(frame.seq, frame.flags & FLAG_MUTED != 0, frame.payload, now_ms(), &rx_engine.stats);
            }
        });
        Ok(Box::new(WebAudio { engine, rx }))
    }
}

/// Takes the engine down, if it is still the live one, and tells the page.
fn release(engine: &Arc<Engine>) {
    let mut slot = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_some_and(|e| Arc::ptr_eq(e, engine)) {
        *slot = None;
        drop(slot);
        tell("stop", Value::Null);
    }
}

impl CallAudio for WebAudio {
    fn stats(&self) -> Arc<MediaStats> {
        Arc::clone(&self.engine.stats)
    }

    fn set_muted(&self, on: bool) {
        self.engine.muted.store(on, Ordering::Relaxed);
    }

    fn set_volume(&self, volume: f32) {
        tell("volume", json!({ "volume": volume }));
    }

    fn set_share_volume(&self, _volume: f32) {}
}

impl Drop for WebAudio {
    fn drop(&mut self) {
        self.rx.abort();
        release(&self.engine);
    }
}

/// The page's microphone and speaker are up, or could not be.
#[wasm_bindgen]
pub fn call_audio_ready(error: Option<String>) {
    if let Some(tx) = READY.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = tx.send(error.map_or(Ok(()), Err));
    }
}

/// One encoded 20 ms frame from the microphone. `ok` false means the encoder had
/// nothing: a muted frame keeps the peer's liveness count moving.
#[wasm_bindgen]
pub fn call_audio_frame(packet: &[u8], ok: bool) {
    let Some(e) = engine() else { return };
    let seq = e.stats.next_seq.load(Ordering::Relaxed) as u16;
    let ts = e.started.elapsed().as_millis() as u32;
    let datagram = if !ok || e.muted.load(Ordering::Relaxed) { pack(seq, ts, FLAG_MUTED, &[]) } else { pack(seq, ts, 0, packet) };
    match e.conn.send_datagram(datagram) {
        Ok(()) => e.stats.sent.fetch_add(1, Ordering::Relaxed),
        Err(_) => e.stats.send_dropped.fetch_add(1, Ordering::Relaxed),
    };
    e.stats.next_seq.store(seq.wrapping_add(1) as u32, Ordering::Relaxed);

    let mut rate = e.rate.lock().unwrap_or_else(|e| e.into_inner());
    if rate.1.elapsed() >= Duration::from_secs(1) {
        rate.1 = Instant::now();
        // Every QUIC packet goes out as one UDP datagram, so this is packets sent.
        let net = e.conn.stats();
        if let Some(setting) = rate.0.observe(net.udp_tx.datagrams, net.lost_packets) {
            e.stats.bitrate_kbps.store(setting.kbps, Ordering::Relaxed);
            tell("rate", json!({ "kbps": setting.kbps, "fec": setting.fec_pct }));
        }
        e.stats.net_loss.store(rate.0.loss_pct().to_bits(), Ordering::Relaxed);
    }
}

/// The speaker ran dry for `frames` frames' worth: gaps the listener heard,
/// counted with the concealed frames the call's quality readout is built on.
#[wasm_bindgen]
pub fn call_audio_starved(frames: u32) {
    if let Some(e) = engine() {
        e.stats.concealed.fetch_add(frames as u64, Ordering::Relaxed);
    }
}

/// Loudness for the meters, 0 to 1: `peer` false is the microphone.
#[wasm_bindgen]
pub fn call_audio_level(peer: bool, level: f32) {
    let Some(e) = engine() else { return };
    let slot = if peer { &e.stats.peer_level } else { &e.stats.mic_level };
    slot.store(level.to_bits(), Ordering::Relaxed);
}

/// The speaker wants its next 20 ms. `ahead_ms` is what it still holds. Returns
/// `{k}`: 0 nothing yet, 1 a packet in `d`, 2 silence, 3 a gap to conceal.
#[wasm_bindgen]
pub fn call_audio_pull(ahead_ms: u32) -> JsValue {
    let Some(e) = engine() else { return JsValue::NULL };
    let plc_run = e.plc_run.load(Ordering::Relaxed);
    let next = {
        let mut j = e.jitter.lock().unwrap_or_else(|e| e.into_inner());
        let next = j.pop(plc_run, now_ms(), &e.stats);
        e.stats.depth_ms.store(j.buffered() as u32 * FRAME_MS + ahead_ms, Ordering::Relaxed);
        next
    };
    let out = js_sys::Object::new();
    let set = |k: &str, v: &JsValue| {
        let _ = js_sys::Reflect::set(&out, &JsValue::from_str(k), v);
    };
    let kind = match next {
        Pop::Frame(p) => {
            e.plc_run.store(0, Ordering::Relaxed);
            set("d", &js_sys::Uint8Array::from(p.as_slice()).into());
            1
        }
        Pop::Muted => {
            e.plc_run.store(0, Ordering::Relaxed);
            2
        }
        // WebCodecs can neither run Opus's concealment nor decode a frame from its
        // successor's FEC, so both fade out the last frame instead.
        Pop::Fec(_) | Pop::Conceal => {
            e.plc_run.store((plc_run + 1).min(MAX_PLC_RUN), Ordering::Relaxed);
            e.stats.concealed.fetch_add(1, Ordering::Relaxed);
            set("run", &JsValue::from(plc_run + 1));
            3
        }
        Pop::Wait => 0,
    };
    set("k", &JsValue::from(kind));
    out.into()
}

// --- The video link ----------------------------------------------------------------
//
// The page's video worker captures, encodes, decodes and paints, as on desktop; its
// link is a MessagePort to this worker instead of a loopback socket.

/// Frames queued toward the page before the track's own wait gives up on them.
const TO_WEB_DEPTH: usize = 8;
/// Frames queued from the page; a port cannot stall its sender, so this is roomier.
const FROM_WEB_DEPTH: usize = 32;

/// Where a new link goes: the video track of the call in progress, if any.
static TAKER: Mutex<Option<mpsc::Sender<LinkConn>>> = Mutex::new(None);
/// The open link's id and its inbound half; dropping it closes the link.
static LINK: Mutex<Option<(u32, mpsc::Sender<bytes::Bytes>)>> = Mutex::new(None);
static NEXT_LINK: AtomicU32 = AtomicU32::new(0);

/// The page opened a link; 0 when no call is taking one. A new link replaces the old.
#[wasm_bindgen]
pub fn call_link_open() -> u32 {
    let Some(taker) = TAKER.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return 0 };
    let (to_web, mut to_page) = mpsc::channel::<bytes::Bytes>(TO_WEB_DEPTH);
    let (from_page, from_web) = mpsc::channel::<bytes::Bytes>(FROM_WEB_DEPTH);
    let id = NEXT_LINK.fetch_add(1, Ordering::Relaxed) + 1;
    *LINK.lock().unwrap_or_else(|e| e.into_inner()) = Some((id, from_page));
    if taker.try_send(LinkConn { to_web, from_web }).is_err() {
        LINK.lock().unwrap_or_else(|e| e.into_inner()).take();
        return 0;
    }
    vector_core::db::spawn_bound(async move {
        while let Some(frame) = to_page.recv().await {
            tell_bytes("link-frame", json!({ "id": id }), &frame);
        }
        tell("link-close", json!({ "id": id }));
    });
    id
}

/// One message from the page's end of link `id`.
#[wasm_bindgen]
pub fn call_link_send(id: u32, bytes: &[u8]) {
    let link = LINK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((open, tx)) = link.as_ref() {
        if *open == id && tx.try_send(bytes::Bytes::copy_from_slice(bytes)).is_err() {
            vector_core::log_warn!("[CALLS] video link full, a message from the page was dropped");
        }
    }
}

/// The page closed link `id`.
#[wasm_bindgen]
pub fn call_link_close(id: u32) {
    let mut link = LINK.lock().unwrap_or_else(|e| e.into_inner());
    if link.as_ref().is_some_and(|(open, _)| *open == id) {
        link.take();
    }
}

// --- Commands ---------------------------------------------------------------------

pub fn dispatch<'a>(cmd: &'a str, a: &'a Args) -> Pin<Box<dyn Future<Output = Option<Result<Value, String>>> + 'a>> {
    Box::pin(async move {
        let result = match cmd {
            "call_start" => match a.str("npub") {
                Ok(npub) => session::start(npub, a.bool("video").unwrap_or(false)).await.and_then(to_value),
                Err(e) => Err(e),
            },
            "call_accept" => session::accept().await.and_then(to_value),
            "call_reject" => session::reject().await.and_then(to_value),
            "call_hangup" => session::hangup().await.and_then(to_value),
            "call_set_muted" => match a.bool("muted") {
                Some(on) => session::set_muted(on).await.and_then(to_value),
                None => Err("Missing muted".into()),
            },
            "call_set_volume" => match a.de::<f32>("volume") {
                Ok(v) => session::set_volume(v).await.and_then(to_value),
                Err(e) => Err(e),
            },
            "call_set_share_volume" => match a.de::<f32>("volume") {
                Ok(v) => session::set_share_volume(v).await.and_then(to_value),
                Err(e) => Err(e),
            },
            "call_video_set" => match (a.de::<VideoKind>("kind"), a.bool("on")) {
                (Ok(kind), Some(on)) => session::set_video(kind, on).await.and_then(to_value),
                (Err(e), _) => Err(e),
                (_, None) => Err("Missing on".into()),
            },
            "call_video_pause" => match a.bool("on") {
                Some(on) => session::set_video_pause(on).await.and_then(to_value),
                None => Err("Missing on".into()),
            },
            "call_video_prefs" => match a.de::<VideoKind>("kind") {
                Ok(kind) => {
                    let rung = a.de::<Option<u32>>("rung").ok().flatten().map(|r| r as usize);
                    let fps = a.de::<Option<u32>>("fps").ok().flatten();
                    session::set_video_prefs(kind, rung, fps).and_then(to_value)
                }
                Err(e) => Err(e),
            },
            "call_video_caps" => match (a.de::<Vec<String>>("encode"), a.de::<Vec<String>>("decode")) {
                (Ok(encode), Ok(decode)) => {
                    session::set_video_caps(encode, decode);
                    Ok(Value::Null)
                }
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            "call_share_audio" => Err("Not available in the browser".into()),
            "call_audio_settings_get" => vector_core::db::scoped_result(async { Ok(settings::load()) }).await.and_then(to_value),
            "call_audio_settings_set" => match a.de::<AudioSettings>("settings") {
                Ok(s) => vector_core::db::scoped_result(async move { settings::set(s) }).await.map(|_| {
                    tell("settings", json!({ "settings": settings::current() }));
                    Value::Null
                }),
                Err(e) => Err(e),
            },
            "call_status" => to_value(session::snapshot()),
            _ => return None,
        };
        Some(result)
    })
}

