//! The live account, on vector-core. Core runs on its own tokio runtime; the UI hears back
//! through [`UiEvent`]s and awaits each call's result through a one-shot channel.

pub mod account;
pub mod actions;
pub mod archive;
pub mod attachments;
pub mod chatlist;
pub mod badges;
pub mod bunker;
pub mod embedvideo;
pub mod xdcurl;
pub mod calls;
pub mod community;
pub mod community_files;
pub mod device;
pub mod events;
pub mod gifs;
pub mod images;
pub mod inline;
pub mod market;
pub mod mediaport;
pub mod pivx;
pub mod rail;
pub mod voice;
pub mod messaging;
pub mod mic;
pub mod network;
pub mod notify;
pub mod packs;
pub mod prefs;
pub mod security;
pub mod selfsync;
pub mod sound;
pub mod storage;
pub mod transcribe;
pub mod sync;
pub mod transfer;
pub mod transport;
pub mod unfurl;
pub mod install;
pub mod updates;
pub mod upload;

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use futures::channel::{mpsc, oneshot};
use vector_core::{SerializableChat, SlimProfile};

static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static SINK: OnceLock<mpsc::UnboundedSender<UiEvent>> = OnceLock::new();

/// What the backend tells the UI.
pub enum UiEvent {
    /// An event core (or this backend) emitted, by the name the Tauri frontend listens for.
    Core(String, serde_json::Value),
    /// The account's chats and profiles, loaded from its database: the list can paint.
    Booted(Boot),
}

pub struct Boot {
    pub npub: String,
    pub chats: Vec<SerializableChat>,
    pub profiles: Vec<SlimProfile>,
}

pub(crate) fn send(ev: UiEvent) {
    if let Some(tx) = SINK.get() {
        let _ = tx.unbounded_send(ev);
    }
}

/// For events this shell emits itself rather than through core.
pub(crate) fn emit(event: &str, payload: impl serde::Serialize) {
    if let Ok(v) = serde_json::to_value(payload) {
        send(UiEvent::Core(event.to_string(), v));
    }
}

struct Emitter;

impl vector_core::EventEmitter for Emitter {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        // Every way a message leaves a chat ends in `message_removed`.
        if event == "message_removed"
            && let Some(id) = payload.get("id").and_then(|v| v.as_str())
        {
            notify::deleted(id);
        }
        if event == "unread_counts_stale" {
            notify::recount();
        }
        send(UiEvent::Core(event.to_string(), payload));
    }
}

/// Where accounts live: the Tauri client's own folder (its app_data_dir), so either client
/// resumes the other's accounts. `--data-dir` points it anywhere else.
pub fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("io.vectorapp")
}

/// Start core: the runtime, the emitter, the profile processor and the remembered account.
pub fn start(data_dir: PathBuf) -> mpsc::UnboundedReceiver<UiEvent> {
    let (tx, rx) = mpsc::unbounded();
    let _ = SINK.set(tx);

    // Every session starts blocked until its stored network is loaded, as on desktop.
    vector_core::transport::strict_boot();
    vector_core::net::set_app_version(env!("CARGO_PKG_VERSION"));
    vector_core::db::set_app_version(env!("CARGO_PKG_VERSION").to_string());
    if let Some(dl) = dirs::download_dir() {
        vector_core::db::set_download_dir(dl.join("vector"));
    }
    let _ = std::fs::create_dir_all(&data_dir);
    device::init(Some(data_dir.clone()));
    vector_core::VectorCore::init(vector_core::CoreConfig {
        data_dir,
        event_emitter: Some(Box::new(Emitter)),
    })
    .expect("vector-core init");
    vector_core::requests::enable_shield();

    let rt = RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .thread_name("vector-core")
            .enable_all()
            .build()
            .expect("tokio runtime")
    });
    tauri::async_runtime::set(rt.handle().clone());
    tauri::set_local_data_dir(models_home());
    if let Ok(dir) = vector_core::db::get_app_data_dir() {
        tauri::set_data_dir(dir.clone());
    }
    let _ = vector_media::TAURI_APP.set(tauri::AppHandle::new(engine_sink));
    notify::install();
    {
        let _guard = rt.enter();
        crate::calls::session::install();
    }
    crate::miniapp::init(|n| match n {
        crate::miniapp::Notice::Failed(e) => emit("show_toast", serde_json::Value::String(e)),
        crate::miniapp::Notice::Refused(msg, code) => emit("player_refused", serde_json::json!({ "msg": msg, "code": code })),
        crate::miniapp::Notice::Browse(url) => emit("player_browse", serde_json::Value::String(url)),
        crate::miniapp::Notice::Opened | crate::miniapp::Notice::Closed => {}
    });
    {
        let _guard = rt.enter();
        transport::spawn_ready_listener();
    }
    // spawn-detached: the sweep expires messages for whichever account is live, as on desktop.
    rt.spawn(vector_core::self_destruct::run_sweeper_loop());
    // spawn-detached: the processor serves whichever account is live, as on desktop.
    rt.spawn(vector_core::profile::sync::start_profile_sync_processor(
        Arc::new(sync::ProfileHandler),
    ));
    // spawn-detached: housekeeping for whichever account is live, on the desktop's 45 s beat.
    rt.spawn(async {
        let mut beat = tokio::time::interval(std::time::Duration::from_secs(45));
        loop {
            beat.tick().await;
            maintenance().await;
        }
    });
    rx
}

/// Retry profiles that never loaded, then let go of what sat idle: decoded sounds, the
/// Whisper model, and messages of chats the timeline isn't showing (it pages from the
/// database, so core keeps only each chat's tail).
async fn maintenance() {
    vector_core::profile::sync::sync_all_profiles().await;
    let mut freed = false;
    if let Ok(engine) = vector_media::audio_engine::AudioEngine::get() {
        freed |= engine.evict_idle() > 0;
    }
    freed |= vector_media::whisper::check_ctx_ttl();
    freed |= vector_core::STATE.lock().await.trim_unheld_chats() > 0;
    if freed {
        crate::trim::soon();
    }
}

fn engine_sink(event: &str, payload: serde_json::Value) {
    if (event.starts_with("audio_") && event != "audio_devices_changed") || event.starts_with("whisper_") {
        crate::audio::engine_event(event, payload);
    } else {
        emit(event, payload);
    }
}

/// Without core (fixtures), the shared audio engine still needs a runtime and its events.
pub fn fixture_runtime() {
    static AUDIO_RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let rt = AUDIO_RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("vector-audio")
            .enable_all()
            .build()
            .expect("tokio runtime")
    });
    tauri::async_runtime::set(rt.handle().clone());
    tauri::set_local_data_dir(models_home());
    tauri::set_data_dir(std::env::temp_dir().join("vector-gpui-fixtures"));
    let _ = vector_media::TAURI_APP.set(tauri::AppHandle::new(crate::audio::engine_event));
}

/// The desktop client's local data folder: Whisper models live under it, so one download
/// serves both clients.
fn models_home() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("io.vectorapp")
}

/// Run `fut` on core's runtime; the receiver resolves with its output. Like a Tauri command,
/// the call is unbound: account work it starts binds itself with `db::spawn_bound`. Without a
/// backend (fixtures) the receiver closes unanswered.
pub fn run<T, F>(fut: F) -> oneshot::Receiver<T>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    let (tx, rx) = oneshot::channel();
    if let Some(rt) = RT.get() {
        // spawn-detached: an unbound UI call, the equivalent of a Tauri command.
        rt.spawn(async move {
            let _ = tx.send(fut.await);
        });
    }
    rx
}
