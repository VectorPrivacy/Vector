//! Mini Apps (WebXDC): a package launched into an OS webview window of its own, its bridge
//! answered here, its realtime channel on the account's Iroh node. The core (packages, the
//! `webxdc://` scheme, the bridge script, realtime) is the desktop client's own, from `media/`.

use std::path::PathBuf;
use std::sync::Arc;

use nostr_sdk::prelude::ToBech32;
use serde::Deserialize;
use vector_media::miniapps::{self as core, realtime, state};

#[cfg(target_os = "linux")]
#[path = "host_gtk.rs"]
mod host;

#[cfg(not(target_os = "linux"))]
#[path = "host_view.rs"]
mod host;

// Type-checked where it can't run, so a Linux build catches its breakage.
#[cfg(all(test, target_os = "linux"))]
#[allow(dead_code)]
mod host_view;

/// Mini App windows that GPUI itself opens need its loop; a no-op where GTK hosts them.
pub fn attach(_cx: &mut gpui::App) {
    #[cfg(not(target_os = "linux"))]
    host::attach(_cx);
}

/// A window to open.
pub struct Spec {
    pub label: String,
    pub title: String,
    pub url: String,
    pub init: String,
    /// Where a first close sends the page, so its unload handlers run.
    pub farewell: String,
    /// The sink every request is pointed at: the app has no network.
    pub proxy: Option<(String, u16)>,
    pub isolated: Option<Isolated>,
}

/// A cross-origin isolated app: served from its loopback host, in a data store of its own.
pub struct Isolated {
    pub id: u64,
    pub port: u16,
    pub boot_url: String,
    pub data_dir: PathBuf,
    /// The null proxy's URI; the app's own origin and the realtime socket bypass it.
    pub sink: String,
}

/// What a window reports.
pub enum Event {
    Ipc(String, String),
    Closed(String),
    Failed(String, String),
    /// The player window's page: `{ vectorPlayer: 'ended' | 'error', code }`.
    Player(String),
    /// A page asked for a new window: it opens in the browser instead.
    Browse(String),
}

/// What the main window hears: a Mini App window came or went, or the player's news.
pub enum Notice {
    Opened,
    Closed,
    Failed(String),
    /// YouTube refused to play message `.0`'s video here, with its error code.
    Refused(String, i64),
    Browse(String),
}

/// The link cards' player window: the loopback page that frames the video, at `size`.
pub struct PlayerSpec {
    pub title: String,
    pub url: String,
    pub size: (f32, f32),
}

/// The window the link cards play in.
pub const PLAYER: &str = "player";

static PLAYING: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Message `msg`'s video or track in the player window, in place of whatever plays there.
pub fn play(msg: String, title: String, url: String, audio: bool) -> Result<(), String> {
    if let Ok(mut p) = PLAYING.lock() {
        *p = Some(msg);
    }
    let title = if title.is_empty() { "Player".to_string() } else { title };
    host::player(PlayerSpec { title, url, size: if audio { (520., 166.) } else { (640., 360.) } })
}

/// Listens for the player's `ended` and `error` relays, which post to the page's own window.
const PLAYER_SHIM: &str = "addEventListener('message',function(e){var d=e.data;if(d&&(d.vectorPlayer==='ended'||d.vectorPlayer==='error'))window.ipc.postMessage(JSON.stringify(d))});";

fn on_player(body: &str) {
    let Ok(news) = serde_json::from_str::<serde_json::Value>(body) else { return };
    let msg = PLAYING.lock().ok().and_then(|p| p.clone());
    if news.get("vectorPlayer").and_then(|v| v.as_str()) == Some("error")
        && let Some(msg) = msg
    {
        notify(Notice::Refused(msg, news.get("code").and_then(|c| c.as_i64()).unwrap_or(0)));
    }
    host::close(PLAYER);
}

/// `window.__TAURI__.core` over wry's message port: the desktop bridge script speaks it as is.
const IPC_SHIM: &str = r#"(function () {
    if (window.__TAURI__) return;
    var pending = new Map(), channels = new Map(), next = 1;
    function Channel() { this.id = next++; this.onmessage = function () {}; channels.set(this.id, this); }
    Channel.prototype.toJSON = function () { return '__CHANNEL__:' + this.id; };
    function invoke(cmd, args) {
        return new Promise(function (resolve, reject) {
            var id = next++;
            pending.set(id, [resolve, reject]);
            window.ipc.postMessage(JSON.stringify({ id: id, cmd: cmd, args: args || {} }));
        });
    }
    Object.defineProperty(window, '__vectorIpc', { value: Object.freeze({
        done: function (id, ok, v) { var p = pending.get(id); if (!p) return; pending.delete(id); p[ok ? 0 : 1](v); },
        chan: function (id, v) { var c = channels.get(id); if (c) c.onmessage(v); },
    }) });
    Object.defineProperty(window, '__TAURI__', { value: Object.freeze({
        core: Object.freeze({ invoke: invoke, Channel: Channel }),
    }) });
})();
"#;

static NOTICES: std::sync::OnceLock<Box<dyn Fn(Notice) + Send + Sync>> = std::sync::OnceLock::new();

fn notify(n: Notice) {
    if let Some(f) = NOTICES.get() {
        f(n);
    }
}

/// Wires the windows to the bridge; `notices` reaches the main window.
pub fn init(notices: impl Fn(Notice) + Send + Sync + 'static) {
    let _ = NOTICES.set(Box::new(notices));
    core::apps();
    host::start(|e| match e {
        Event::Ipc(label, body) => {
            // spawn-detached: an IPC call from a Mini App window is a command, unbound as one.
            tauri::async_runtime::spawn(answer(label, body));
        }
        Event::Closed(label) if label == PLAYER => {}
        Event::Closed(label) => {
            // spawn-detached: unbound, as the desktop's window-destroyed handler.
            tauri::async_runtime::spawn(async move { teardown(&label).await });
            notify(Notice::Closed);
        }
        Event::Failed(label, e) if label == PLAYER => notify(Notice::Failed(e)),
        Event::Failed(label, e) => {
            // spawn-detached: unbound, as the desktop's window-destroyed handler.
            tauri::async_runtime::spawn(async move { teardown(&label).await });
            notify(Notice::Failed(e));
        }
        Event::Player(body) => on_player(&body),
        Event::Browse(url) => notify(Notice::Browse(url)),
    });
}

fn md5_hash(input: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    input.hash(&mut h);
    h.finish()
}

fn icon_dir() -> Option<PathBuf> {
    let dir = vector_core::get_app_data_dir().ok()?.join("cache").join("miniapp_icons");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Browser storage keys on origin, so the partition is the origin's host.
async fn partition(file_hash: &str) -> String {
    file_hash[..32.min(file_hash.len())].to_ascii_lowercase()
}

/// `miniapp_open`: one window per message (or per package, solo), focused when already open.
pub async fn open(
    file_path: String,
    chat_id: String,
    message_id: String,
    href: Option<String>,
    topic_id: Option<String>,
) -> Result<(), String> {
    vector_core::db::scoped(async move {
        let opened = file_path.clone();
        let file_path = package_path(&file_path).await?;
        let path = PathBuf::from(&file_path);
        let id = format!("miniapp_{:x}", md5_hash(&file_path));
        let label = if chat_id == "solo" || (chat_id.is_empty() && message_id.is_empty()) {
            format!("miniapp:solo:{id}")
        } else {
            format!("miniapp:{chat_id}:{message_id}")
        };
        let apps = core::apps();
        if let Some(existing) = apps.get_instance(&label).await {
            let to = match &href {
                Some(h) => Some(format!(
                    "{}{}",
                    base_url(&partition(&existing.package.hash_hex()).await),
                    h.trim_start_matches('/')
                )),
                None => None,
            };
            host::focus(&label, to);
            return Ok(());
        }
        let Some(_opening) = apps.begin_opening(&label) else {
            return Ok(());
        };
        let package = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            apps.get_or_load_package(&id, path, icon_dir()),
        )
        .await
        .map_err(|_| "The Mini App took too long to load".to_string())?
        .map_err(|e| e.to_string())?;
        let store = partition(&package.hash_hex()).await;
        if package.cross_origin_isolated
            && let Some(other) = core::isolated::window_for_partition(&store, &label)
        {
            host::focus(&other, None);
            return Ok(());
        }
        let realtime_topic = topic_id.as_deref().and_then(|t| realtime::decode_topic_id(t).ok());
        let instance = state::MiniAppInstance {
            package: Arc::clone(&package),
            chat_id: chat_id.clone(),
            message_id: message_id.clone(),
            window_label: label.clone(),
            realtime_topic,
            instance_id: state::next_instance_id(),
            archive: Default::default(),
        };
        apps.add_instance(instance.clone()).await;

        // Preconnect: a realtime app's session is joined while the page loads.
        let (pc_tx, pc_rx) = tokio::sync::watch::channel(false);
        apps.set_preconnect_signal(&label, pc_rx).await;
        {
            let instance = instance.clone();
            vector_core::db::spawn_bound(async move {
                if !instance.package.uses_realtime {
                    drop(pc_tx);
                    return;
                }
                if let Err(e) = start_realtime(&instance, None).await {
                    vector_core::log_warn!("[WEBXDC] Preconnect failed: {e}");
                    return;
                }
                let _ = pc_tx.send(true);
            });
        }

        let isolated = if package.cross_origin_isolated {
            match start_isolated(&label, &store, href.as_deref()).await {
                Ok(Some(i)) => Some(i),
                Ok(None) => {
                    apps.remove_instance_if(&label, instance.instance_id).await;
                    return Ok(());
                }
                Err(e) => {
                    apps.remove_instance_if(&label, instance.instance_id).await;
                    return Err(format!("Mini App loopback host: {e}"));
                }
            }
        } else {
            None
        };
        let base = match &isolated {
            Some(i) => format!("{}/", core::isolated::origin(i.port)),
            None => base_url(&store),
        };
        let url = match &href {
            Some(h) => format!("{base}{}", h.trim_start_matches('/')),
            None => base.clone(),
        };
        #[cfg(target_os = "linux")]
        let proxy = {
            let Some(sink) = core::network_sink() else {
                apps.remove_instance_if(&label, instance.instance_id).await;
                return Err("Network isolation unavailable for Mini Apps".into());
            };
            Some((
                sink.host_str().unwrap_or("127.0.0.1").to_string(),
                sink.port().unwrap_or(0),
            ))
        };
        #[cfg(not(target_os = "linux"))]
        let proxy = None;
        apps.realtime.ensure_ws_started();
        let init = core::INIT_SCRIPT.replace("__VECTOR_DEBUG_BRIDGE__", "false");
        let spec = Spec {
            label: label.clone(),
            title: package.name.to_string(),
            url,
            init: format!("{IPC_SHIM}\n{init}"),
            farewell: format!("{base}webxdc.js"),
            proxy,
            isolated,
        };
        let isolated_id = spec.isolated.as_ref().map(|i| i.id);
        if let Err(e) = host::open(spec) {
            if let Some(id) = isolated_id {
                core::isolated::stop_if(&label, id);
            }
            apps.remove_instance_if(&label, instance.instance_id).await;
            return Err(e);
        }
        notify(Notice::Opened);
        if let Err(e) = vector_core::db::miniapps::record_miniapp_opened(
            package.name.to_string(),
            opened.clone(),
            opened,
        ) {
            vector_core::log_warn!("Failed to record Mini App to history: {e}");
        }
        Ok(())
    })
    .await
}

/// Starts the app's loopback host; None when another window already serves its store.
async fn start_isolated(label: &str, store: &str, href: Option<&str>) -> Result<Option<Isolated>, String> {
    let app = *vector_media::TAURI_APP.get().ok_or("The app isn't ready yet")?;
    #[cfg(target_os = "linux")]
    let sink = core::network_sink().ok_or("Network isolation unavailable for Mini Apps")?.to_string();
    #[cfg(not(target_os = "linux"))]
    let sink = String::new();
    let data_dir = tauri::Manager::path(&app)
        .app_data_dir()
        .map_err(|e| e.0)?
        .join("miniapp-isolated")
        .join(store);
    match core::isolated::start(&app, label, store, href).await {
        Ok(s) => Ok(Some(Isolated { id: s.id, port: s.port, boot_url: s.boot_url.to_string(), data_dir, sink })),
        Err(core::isolated::StartError::AlreadyOpen(other)) => {
            host::focus(&other, None);
            Ok(None)
        }
        Err(core::isolated::StartError::Failed(e)) => Err(e),
    }
}

fn base_url(partition: &str) -> String {
    // WebView2 serves a custom scheme as `http://<scheme>.<host>`.
    if cfg!(windows) {
        format!("http://webxdc.{partition}.host/")
    } else {
        format!("webxdc://{partition}.host/")
    }
}

#[derive(Deserialize)]
struct Call {
    id: u64,
    cmd: String,
    #[serde(default)]
    args: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum MiniAppCall {
    GetUpdates {
        #[allow(dead_code)]
        last_known_serial: u32,
    },
    SendUpdate {
        #[allow(dead_code)]
        update: serde_json::Value,
        #[allow(dead_code)]
        description: String,
    },
    JoinRealtimeChannel,
    LeaveRealtimeChannel,
    SendRealtimeData {
        data: Vec<u8>,
    },
    GrantedPermissions,
}

/// One bridge call from window `label`, answered into its page.
async fn answer(label: String, body: String) {
    let Ok(call) = serde_json::from_str::<Call>(&body) else {
        return;
    };
    let result = if call.cmd == "miniapp_call" {
        match serde_json::from_value::<MiniAppCall>(call.args["call"].clone()) {
            Ok(c) => dispatch(&label, c, call.args.get("channel")).await,
            Err(e) => Err(e.to_string()),
        }
    } else {
        Err(format!("Command {} not allowed by ACL", call.cmd))
    };
    let (ok, value) = match result {
        Ok(v) => (true, v),
        Err(e) => (false, serde_json::Value::String(e)),
    };
    host::eval(
        &label,
        format!("window.__vectorIpc.done({}, {ok}, {value})", call.id),
    );
}

async fn dispatch(
    label: &str,
    call: MiniAppCall,
    channel: Option<&serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let apps = core::apps();
    match call {
        MiniAppCall::GetUpdates { .. } => Ok(serde_json::Value::String("[]".into())),
        MiniAppCall::SendUpdate { .. } => Ok(serde_json::Value::Null),
        MiniAppCall::GrantedPermissions => {
            let instance = apps
                .get_instance(label)
                .await
                .ok_or("Could not find Mini App instance")?;
            vector_core::db::miniapps::get_miniapp_granted_permissions(&instance.package.hash_hex())
                .map(serde_json::Value::String)
        }
        MiniAppCall::LeaveRealtimeChannel => {
            apps.realtime.detach(label).await;
            Ok(serde_json::Value::Null)
        }
        MiniAppCall::SendRealtimeData { data } => {
            if data.len() > 128_000 {
                return Err(format!("Data too large: {} bytes", data.len()));
            }
            apps.realtime
                .send(label, data)
                .await
                .map(|_| serde_json::Value::Null)
                .map_err(|e| e.to_string())
        }
        MiniAppCall::JoinRealtimeChannel => {
            let id = channel
                .and_then(|c| c.as_str())
                .and_then(|c| c.strip_prefix("__CHANNEL__:"))
                .and_then(|c| c.parse::<u64>().ok())
                .ok_or("joining needs a channel")?;
            join(label, id).await
        }
    }
}

/// `miniapp_join_realtime_channel`: the window's session delivers to its JS channel from now.
async fn join(label: &str, channel: u64) -> Result<serde_json::Value, String> {
    vector_core::db::scoped(async move {
        let apps = core::apps();
        let instance = apps.get_instance(label).await.ok_or("Mini App instance not found")?;
        if let Some(mut rx) = apps.take_preconnect_signal(label).await {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(10), rx.wait_for(|r| *r)).await;
        }
        if apps.get_instance(label).await.map(|i| i.instance_id) != Some(instance.instance_id) {
            return Err("Mini App instance not found".to_string());
        }
        apps.realtime.ensure_ws_started();
        let to = label.to_string();
        let sink = tauri::ipc::Channel::new(move |v: serde_json::Value| {
            host::eval(&to, format!("window.__vectorIpc.chan({channel}, {v})"));
            true
        });
        let topic = start_realtime(&instance, Some(realtime::EventTarget::TauriChannel(sink))).await?;
        Ok(serde_json::json!({
            "topic": realtime::encode_topic_id(&topic),
            "ws_url": apps.realtime.ws_url_for(label),
        }))
    })
    .await
}

/// `start_realtime`: join the window's session (or attach to it) and enter the topic's lobby.
async fn start_realtime(
    instance: &state::MiniAppInstance,
    target: Option<realtime::EventTarget>,
) -> Result<realtime::TopicId, String> {
    let apps = core::apps();
    let still_open = || async {
        apps.get_instance(&instance.window_label).await.map(|i| i.instance_id) == Some(instance.instance_id)
    };
    if !still_open().await {
        return Err("the Mini App closed before joining".into());
    }
    let topic = instance.realtime_topic.unwrap_or_else(|| {
        realtime::derive_topic_id(&instance.package.name, &instance.chat_id, &instance.message_id)
    });
    let window = realtime::WindowSession {
        label: &instance.window_label,
        instance_id: instance.instance_id,
        chat_id: &instance.chat_id,
        topic,
        advertise: !(instance.chat_id.is_empty() || instance.chat_id == "solo"),
    };
    apps.realtime.open(window, target).await.map_err(|e| e.to_string())?;
    apps.set_realtime_channel(
        &instance.window_label,
        state::RealtimeChannelState {
            topic,
            active: true,
            instance_id: instance.instance_id,
        },
    )
    .await;
    if !still_open().await {
        apps.remove_realtime_channel_if(&instance.window_label, instance.instance_id).await;
        apps.realtime.close(&instance.window_label, instance.instance_id).await;
        return Err("the Mini App closed while joining".into());
    }
    let encoded = realtime::encode_topic_id(&topic);
    let me = vector_core::my_public_key()
        .and_then(|pk| pk.to_bech32().ok())
        .unwrap_or_default();
    if let Ok(records) =
        vector_core::db::miniapps::get_active_peer_advertisements_in(&encoded, &instance.chat_id, &me, 32)
    {
        for record in records {
            apps.add_session_peer(topic, record.npub).await;
        }
    }
    if !me.is_empty() {
        apps.add_session_peer(topic, me).await;
    }
    let peers = apps.get_session_peers(&topic).await;
    emit_lobby(&encoded, peers, true);
    Ok(topic)
}

/// `teardown_window`: the instance, its session (announcing the departure), our lobby seat.
async fn teardown(label: &str) {
    let apps = core::apps();
    let Some(instance) = apps.get_instance(label).await else {
        return;
    };
    let id = instance.instance_id;
    let instance = apps.remove_instance_if(label, id).await;
    let channel = apps.remove_realtime_channel_if(label, id).await;
    apps.realtime.close(label, id).await;
    let topic = channel
        .as_ref()
        .map(|c| c.topic)
        .or_else(|| instance.as_ref().and_then(|i| i.realtime_topic));
    if let Some(topic) = topic {
        let still_playing = apps.has_realtime_channel_for_topic(&topic).await;
        let peers = if channel.is_some() {
            if !still_playing
                && let Some(me) = vector_core::my_public_key().and_then(|pk| pk.to_bech32().ok())
            {
                apps.remove_session_peer(&topic, &me).await;
            }
            apps.get_session_peers(&topic).await
        } else {
            Vec::new()
        };
        emit_lobby(&realtime::encode_topic_id(&topic), peers, still_playing);
    }
}

/// An account swap: every session ends while the outgoing client can still say so.
pub async fn end_for_account_swap() {
    let apps = core::apps();
    apps.realtime.end_all().await;
    for (label, _) in apps.open_instances().await {
        host::destroy(&label);
        teardown(&label).await;
    }
}

pub fn close(label: &str) {
    host::close(label);
}

fn emit_lobby(topic: &str, peers: Vec<String>, is_active: bool) {
    vector_core::traits::emit_event_json(
        "miniapp_realtime_status",
        serde_json::json!({
            "topic": topic,
            "peer_count": peers.len(),
            "has_pending_peers": !peers.is_empty(),
            "peers": peers,
            "is_active": is_active,
        }),
    );
}

/// A package Vector holds (no file of its own) written out sealed, as the desktop keeps one.
async fn held_package(hash: [u8; 32]) -> Result<String, String> {
    let dir = dirs::cache_dir()
        .ok_or("No cache folder")?
        .join("io.vectorapp")
        .join("exports")
        .join("apps");
    let file = dir.join(format!("{}.xdc", vector_core::simd::hex::bytes_to_hex_string(&hash)));
    if !file.is_file() {
        let bytes = crate::backend::attachments::bytes(Some(hash), String::new())
            .await
            .ok_or("This app isn't on this device any more")?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't prepare the app: {e}"))?;
        vector_core::vcache::write(&file, &bytes).map_err(|e| format!("Couldn't prepare the app: {e}"))?;
    }
    Ok(file.to_string_lossy().into_owned())
}

/// What a package's card shows.
#[derive(Clone)]
pub struct Info {
    pub name: String,
    pub icon: Option<PathBuf>,
    pub uses_realtime: bool,
    pub hash: String,
    pub description: String,
    pub version: String,
    pub source: Option<String>,
}

/// `miniapp_load_info`: the package's listing, read once and kept by the package cache.
pub async fn info(file_path: String) -> Result<Info, String> {
    if file_path.is_empty() {
        return Err("Empty file path".into());
    }
    let file_path = package_path(&file_path).await?;
    let path = PathBuf::from(&file_path);
    if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Err(format!("File not found: {file_path}"));
    }
    let id = format!("miniapp_{:x}", md5_hash(&file_path));
    let dir = icon_dir();
    let package = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        core::apps().get_or_load_package(&id, path, dir.clone()),
    )
    .await
    .map_err(|_| "The Mini App took too long to read".to_string())?
    .map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || Info {
        name: package.name.to_string(),
        icon: dir.and_then(|d| package.icon_path(&d)),
        uses_realtime: package.uses_realtime,
        hash: package.hash_hex(),
        description: package.description.to_string(),
        version: package.version.to_string(),
        source: package.source_code_url.as_deref().map(str::to_string),
    })
    .await
    .map_err(|e| e.to_string())
}

/// A file, or a `vmedia:` ref to a package Vector holds, as a file the package loader reads.
async fn package_path(path_or_ref: &str) -> Result<String, String> {
    match vector_core::media::MediaRef::parse(path_or_ref).ok_or("Unknown file")? {
        vector_core::media::MediaRef::File(p) => Ok(p),
        vector_core::media::MediaRef::Held(h) => held_package(h).await,
    }
}
