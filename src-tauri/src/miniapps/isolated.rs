//! Loopback HTTP host for Mini Apps that opt into cross-origin isolation.
//!
//! SharedArrayBuffer (threaded WebAssembly) needs a page served as a real HTTP
//! response from a secure origin with COOP + COEP. WebKit never isolates a
//! custom-scheme page and WebView2 maps ours to a non-secure host, so an app
//! that sets `cross_origin_isolated = true` is served from
//! `http://localhost:<port>/` instead: one port per app, its window in a data
//! store of its own (see `commands::miniapp_open`).
//!
//! Only the app's own window can read anything here. Vector opens it at a
//! one-time boot URL that trades a single-use token for an HttpOnly session
//! cookie, and every other request must carry that cookie, the exact Host
//! (DNS rebinding) and a same-origin fetch context. Other local users, other
//! apps' windows and browser tabs get 403s.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{self, HeaderMap, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tokio::net::TcpListener;
use tokio::sync::{watch, Semaphore};

use super::state::MiniAppsState;

/// Below the ephemeral (outgoing) range of every OS: Linux allocates from
/// 32768, macOS and Windows from 49152.
const PORT_BASE: u16 = 20000;
const PORT_SPAN: u16 = 12000;
const PORT_ATTEMPTS: u16 = 64;
const BOOT_PATH: &str = "/__vector/boot";
const BOOT_TOKEN_TTL: Duration = Duration::from_secs(120);
/// Concurrent connections per app; an engine streaming assets needs a handful.
const MAX_CONNECTIONS: usize = 64;

/// A running host, owned by the window it serves.
struct IsolatedHost {
    id: u64,
    port: u16,
    partition: String,
    shutdown: watch::Sender<bool>,
}

/// A started host: the window must open `boot_url` first.
pub(crate) struct Started {
    pub id: u64,
    pub port: u16,
    pub boot_url: tauri::Url,
}

pub(crate) enum StartError {
    /// The app already runs in this window; focus it instead of opening a
    /// second one on the same data store.
    AlreadyOpen(String),
    Failed(String),
}

/// Stops the host unless disarmed: every early return after `start` is covered.
pub(crate) struct HostGuard {
    label: String,
    id: u64,
    armed: bool,
}

impl HostGuard {
    pub(crate) fn new(label: &str, id: u64) -> Self {
        Self { label: label.to_string(), id, armed: true }
    }
    pub(crate) fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for HostGuard {
    fn drop(&mut self) {
        if self.armed {
            stop_if(&self.label, self.id);
        }
    }
}

struct HostContext {
    app: AppHandle,
    window_label: String,
    gate: Gate,
}

/// Window label → running host.
static HOSTS: LazyLock<Mutex<HashMap<String, IsolatedHost>>> = LazyLock::new(Default::default);
/// Ports currently serving an app; read by the macOS pointer-lock grant.
static LIVE_PORTS: LazyLock<RwLock<HashSet<u16>>> = LazyLock::new(Default::default);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// Serialises the ports file's read-modify-write.
static PORTS_FILE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) fn origin(port: u16) -> String {
    format!("http://localhost:{port}")
}

pub(crate) fn is_live_port(port: u16) -> bool {
    LIVE_PORTS.read().map(|p| p.contains(&port)).unwrap_or(false)
}

/// Start serving `window_label`'s app.
pub(crate) async fn start(
    app: &AppHandle,
    window_label: &str,
    partition: &str,
    href: Option<&str>,
) -> Result<Started, StartError> {
    {
        let hosts = HOSTS.lock().map_err(|_| StartError::Failed("host registry poisoned".into()))?;
        if let Some((label, _)) = hosts.iter().find(|(l, h)| h.partition == partition && l.as_str() != window_label) {
            return Err(StartError::AlreadyOpen(label.clone()));
        }
    }
    stop(window_label);

    let (v4, v6, port) = bind_pair(app, partition).await.map_err(StartError::Failed)?;
    let boot_token = random_hex(32);
    let ctx = Arc::new(HostContext {
        app: app.clone(),
        window_label: window_label.to_string(),
        gate: Gate::new(port, random_hex(32), boot_token.clone()),
    });

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    for listener in [Some(v4), v6].into_iter().flatten() {
        let ctx = Arc::clone(&ctx);
        let rx = shutdown_rx.clone();
        let permits = Arc::clone(&permits);
        // spawn-detached: loopback accept loop for one Mini App window; ends on window close. Requests resolve the live account like the webxdc:// handler.
        tokio::spawn(accept_loop(listener, ctx, rx, permits));
    }

    LIVE_PORTS
        .write()
        .map_err(|_| StartError::Failed("port registry poisoned".into()))?
        .insert(port);
    HOSTS
        .lock()
        .map_err(|_| StartError::Failed("host registry poisoned".into()))?
        .insert(window_label.to_string(), IsolatedHost { id, port, partition: partition.to_string(), shutdown: shutdown_tx });

    let target = sanitize_target(href.unwrap_or("/")).unwrap_or_else(|| "/".to_string());
    let mut boot_url = tauri::Url::parse(&format!("{}{}", origin(port), BOOT_PATH))
        .map_err(|e| StartError::Failed(e.to_string()))?;
    boot_url.query_pairs_mut().append_pair("t", &boot_token).append_pair("to", &target);
    Ok(Started { id, port, boot_url })
}

/// The window already serving `partition`, other than `except_label`.
pub(crate) fn window_for_partition(partition: &str, except_label: &str) -> Option<String> {
    let hosts = HOSTS.lock().ok()?;
    hosts
        .iter()
        .find(|(label, host)| host.partition == partition && label.as_str() != except_label)
        .map(|(label, _)| label.clone())
}

/// Stop the host for `window_label`, if any. Open connections end with it.
pub(crate) fn stop(window_label: &str) {
    stop_matching(window_label, None);
}

/// Stop the host only if it is still generation `id`: a closing window's late
/// teardown must not take down a reopened successor under the same label.
pub(crate) fn stop_if(window_label: &str, id: u64) {
    stop_matching(window_label, Some(id));
}

fn stop_matching(window_label: &str, id: Option<u64>) {
    let removed = HOSTS.lock().ok().and_then(|mut hosts| {
        let matches = hosts.get(window_label).is_some_and(|h| id.is_none_or(|id| h.id == id));
        if matches { hosts.remove(window_label) } else { None }
    });
    if let Some(host) = removed {
        let _ = host.shutdown.send(true);
        if let Ok(mut live) = LIVE_PORTS.write() {
            live.remove(&host.port);
        }
    }
}

/// The data store an isolated app's window lives in (macOS 14+). Older macOS
/// has no named stores; the caller falls back to an ephemeral one.
#[cfg(target_os = "macos")]
pub(crate) fn store_identifier(partition: &str) -> Option<[u8; 16]> {
    use sha2::{Digest, Sha256};
    if objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion().majorVersion < 14 {
        return None;
    }
    let digest = Sha256::digest(format!("vector.miniapp.isolated/{partition}"));
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest[..16]);
    // RFC 4122 version 4 / variant bits: WebKit takes the identifier as an NSUUID.
    id[6] = (id[6] & 0x0f) | 0x40;
    id[8] = (id[8] & 0x3f) | 0x80;
    Some(id)
}

/// WebKitGTK ships the Storage API, the origin-private file system (where
/// engines keep saves) and WebGL in workers (threaded engines render from one)
/// switched off; WebKit on macOS has them on. Turned on for this app's settings
/// only; the switches arrived in WebKitGTK 2.42, so they are looked up at runtime.
#[cfg(target_os = "linux")]
pub(crate) fn enable_web_storage(view: &webkit2gtk::WebView) -> usize {
    use glib::translate::ToGlibPtr;
    use std::ffi::{c_char, c_int, c_void, CStr};
    use webkit2gtk::WebViewExt;

    const WANTED: [&str; 6] =
        ["StorageAPI", "StorageAPIEstimate", "FileSystem", "AccessHandle", "FileSystemWritableStream", "AllowWebGLInWorkers"];
    type AllFeatures = unsafe extern "C" fn() -> *mut c_void;
    type ListLength = unsafe extern "C" fn(*mut c_void) -> usize;
    type ListGet = unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void;
    type Identifier = unsafe extern "C" fn(*mut c_void) -> *const c_char;
    type SetEnabled = unsafe extern "C" fn(*mut c_void, *mut c_void, c_int);
    type Unref = unsafe extern "C" fn(*mut c_void);

    // SAFETY: symbols resolved from the WebKitGTK already loaded in this process,
    // called with their documented signatures; the list is released once, after use.
    unsafe {
        let find = |name: &CStr| libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr());
        let symbols = (
            find(c"webkit_settings_get_all_features"),
            find(c"webkit_feature_list_get_length"),
            find(c"webkit_feature_list_get"),
            find(c"webkit_feature_get_identifier"),
            find(c"webkit_settings_set_feature_enabled"),
            find(c"webkit_feature_list_unref"),
        );
        if [symbols.0, symbols.1, symbols.2, symbols.3, symbols.4, symbols.5].iter().any(|p| p.is_null()) {
            return 0;
        }
        let all: AllFeatures = std::mem::transmute(symbols.0);
        let length: ListLength = std::mem::transmute(symbols.1);
        let get: ListGet = std::mem::transmute(symbols.2);
        let identifier: Identifier = std::mem::transmute(symbols.3);
        let set_enabled: SetEnabled = std::mem::transmute(symbols.4);
        let unref: Unref = std::mem::transmute(symbols.5);

        let Some(settings) = WebViewExt::settings(view) else { return 0 };
        let settings_ptr: *mut webkit2gtk::ffi::WebKitSettings = settings.to_glib_none().0;
        let list = all();
        if list.is_null() {
            return 0;
        }
        let mut enabled = 0;
        for i in 0..length(list) {
            let feature = get(list, i);
            let id = identifier(feature);
            if feature.is_null() || id.is_null() {
                continue;
            }
            if WANTED.contains(&CStr::from_ptr(id).to_str().unwrap_or("")) {
                set_enabled(settings_ptr.cast(), feature, 1);
                enabled += 1;
            }
        }
        unref(list);
        enabled
    }
}

// ── Port allocation ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
struct PortsFile {
    /// Per-install: a web page can't predict which port an app would use.
    salt: String,
    ports: HashMap<String, u16>,
}

/// Storage keys on origin, and the origin includes the port, so an app keeps
/// its persisted port. If that port is busy the app runs on a session-only one
/// and keeps its saved port, rather than abandoning its storage for good.
async fn bind_pair(app: &AppHandle, partition: &str) -> Result<(TcpListener, Option<TcpListener>, u16), String> {
    let _file = PORTS_FILE.lock().await;
    let path = ports_file(app)?;
    let mut file = read_ports_file(&path);
    if file.salt.is_empty() {
        file.salt = random_hex(16);
        write_ports_file(&path, &file);
    }

    if let Some(&saved) = file.ports.get(partition) {
        for attempt in 0..3 {
            if !is_live_port(saved) {
                if let Some((v4, v6)) = try_bind(saved).await {
                    return Ok((v4, v6, saved));
                }
            }
            if attempt < 2 {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
        log_warn!("[WEBXDC] Mini App port {saved} is busy; this session runs on another (storage stays with {saved})");
        return probe(&file, partition).await;
    }

    let (v4, v6, port) = probe(&file, partition).await?;
    file.ports.insert(partition.to_string(), port);
    write_ports_file(&path, &file);
    Ok((v4, v6, port))
}

async fn probe(file: &PortsFile, partition: &str) -> Result<(TcpListener, Option<TcpListener>, u16), String> {
    let assigned: HashSet<u16> = file.ports.values().copied().collect();
    let mut candidate = preferred_port(&file.salt, partition);
    for _ in 0..PORT_ATTEMPTS {
        if !is_live_port(candidate) && !assigned.contains(&candidate) {
            if let Some((v4, v6)) = try_bind(candidate).await {
                return Ok((v4, v6, candidate));
            }
        }
        candidate = PORT_BASE + (candidate - PORT_BASE + 1) % PORT_SPAN;
    }
    Err("no free loopback port for the Mini App".into())
}

/// Both loopback stacks on one port: `localhost` resolves to ::1 first on
/// most systems, and a port we hold only on IPv4 could be squatted on IPv6 by
/// another process, which would then serve our app's origin and storage.
async fn try_bind(port: u16) -> Option<(TcpListener, Option<TcpListener>)> {
    let v4 = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await.ok()?;
    if !ipv6_loopback() {
        return Some((v4, None));
    }
    let v6 = TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, port))).await.ok()?;
    Some((v4, Some(v6)))
}

/// Whether this machine has an IPv6 loopback at all (not on `ipv6.disable=1`);
/// without one, `localhost` cannot resolve to ::1 and nobody can bind it.
fn ipv6_loopback() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).is_ok())
}

fn preferred_port(salt: &str, partition: &str) -> u16 {
    let mut h: u32 = 0x811c9dc5;
    for b in salt.bytes().chain(partition.bytes()) {
        h = (h ^ b as u32).wrapping_mul(0x01000193);
    }
    PORT_BASE + (h % PORT_SPAN as u32) as u16
}

fn ports_file(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("miniapp_isolated_ports.json"))
}

fn read_ports_file(path: &Path) -> PortsFile {
    let Ok(bytes) = std::fs::read(path) else {
        return PortsFile::default();
    };
    match serde_json::from_slice::<PortsFile>(&bytes) {
        Ok(file) => file,
        Err(_) => {
            // Keep the evidence; a reset here would move every app's storage.
            let aside = path.with_extension(format!("corrupt-{}", random_hex(4)));
            let _ = std::fs::rename(path, &aside);
            log_warn!("[WEBXDC] Mini App ports file unreadable; moved to {}", aside.display());
            PortsFile::default()
        }
    }
}

fn write_ports_file(path: &Path, file: &PortsFile) {
    let Ok(json) = serde_json::to_vec_pretty(file) else { return };
    let tmp = path.with_extension(format!("tmp-{}", random_hex(4)));
    if std::fs::write(&tmp, json).is_err() {
        return;
    }
    // It lists which apps the user runs.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

// ── Access gate ────────────────────────────────────────────────────────────

/// What a request may do once past the gate.
#[derive(Debug, PartialEq)]
enum Route {
    /// Set the session cookie and redirect here.
    Boot { location: String, cookie: String },
    Serve { path: String, head_only: bool },
}

/// Everything decided from the request line and headers alone, so it is testable.
struct Gate {
    host: String,
    cookie_name: String,
    session_secret: String,
    boot_token: Mutex<Option<(String, Instant)>>,
}

impl Gate {
    fn new(port: u16, session_secret: String, boot_token: String) -> Self {
        Self {
            host: format!("localhost:{port}"),
            // Cookies ignore ports; a per-port name keeps two apps' sessions apart.
            cookie_name: format!("vector_isolated_{port}"),
            session_secret,
            boot_token: Mutex::new(Some((boot_token, Instant::now()))),
        }
    }

    fn check(&self, method: &Method, raw_path: &str, query: Option<&str>, headers: &HeaderMap) -> Result<Route, (StatusCode, &'static str)> {
        let head_only = method == Method::HEAD;
        if method != Method::GET && !head_only {
            return Err((StatusCode::METHOD_NOT_ALLOWED, "GET and HEAD only"));
        }
        // Exact Host: a rebinding attacker's name resolving to 127.0.0.1 is refused.
        if headers.get(header::HOST).and_then(|h| h.to_str().ok()) != Some(self.host.as_str()) {
            return Err((StatusCode::MISDIRECTED_REQUEST, "Unknown host"));
        }
        // Only the app's own pages may fetch from it (another app's port is "same-site").
        if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
            if site != "same-origin" && site != "none" {
                return Err((StatusCode::FORBIDDEN, "Cross-origin request refused"));
            }
        }
        // No service workers: one could answer the boot path or outlive the app's version.
        if headers.contains_key("service-worker") {
            return Err((StatusCode::FORBIDDEN, "Service workers are not available"));
        }
        let path = decode_path(raw_path).ok_or((StatusCode::BAD_REQUEST, "Bad path"))?;
        if path == BOOT_PATH {
            if method != Method::GET {
                return Err((StatusCode::METHOD_NOT_ALLOWED, "GET only"));
            }
            return self.boot(query.unwrap_or(""));
        }
        if !self.has_session(headers) {
            return Err((StatusCode::FORBIDDEN, "Not this window's session"));
        }
        Ok(Route::Serve { path, head_only })
    }

    /// Trade the single-use boot token for this window's session cookie.
    fn boot(&self, query: &str) -> Result<Route, (StatusCode, &'static str)> {
        let mut token = None;
        let mut target = None;
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let v = decode_component(v);
            match k {
                "t" => token = v,
                "to" => target = v.as_deref().and_then(sanitize_target),
                _ => {}
            }
        }
        let presented = token.unwrap_or_default();
        let mut slot = self.boot_token.lock().map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Boot state poisoned"))?;
        let valid = matches!(slot.as_ref(), Some((expected, issued))
            if issued.elapsed() < BOOT_TOKEN_TTL && constant_time_eq(expected.as_bytes(), presented.as_bytes()));
        if !valid {
            // A wrong guess must not burn the real token; an expired one is dropped.
            if slot.as_ref().is_some_and(|(_, issued)| issued.elapsed() >= BOOT_TOKEN_TTL) {
                *slot = None;
            }
            return Err((StatusCode::FORBIDDEN, "Boot token invalid or used"));
        }
        *slot = None;
        Ok(Route::Boot {
            location: target.unwrap_or_else(|| "/".to_string()),
            cookie: format!("{}={}; Path=/; HttpOnly; SameSite=Strict", self.cookie_name, self.session_secret),
        })
    }

    fn has_session(&self, headers: &HeaderMap) -> bool {
        headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .filter_map(|c| c.trim().split_once('='))
            .any(|(k, v)| k == self.cookie_name && constant_time_eq(v.as_bytes(), self.session_secret.as_bytes()))
    }
}

// ── Serving ────────────────────────────────────────────────────────────────

async fn accept_loop(listener: TcpListener, ctx: Arc<HostContext>, mut shutdown: watch::Receiver<bool>, permits: Arc<Semaphore>) {
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    if !peer.ip().is_loopback() {
                        continue;
                    }
                    // A full house drops the connection: held sockets must not starve Vector's own.
                    let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                        continue;
                    };
                    let _ = stream.set_nodelay(true);
                    let ctx = Arc::clone(&ctx);
                    let mut rx = shutdown.clone();
                    // spawn-detached: one loopback HTTP connection for a Mini App window; resolves the live account like the webxdc:// handler.
                    tokio::spawn(async move {
                        let _permit = permit;
                        let service = hyper::service::service_fn(move |req| {
                            let ctx = Arc::clone(&ctx);
                            async move { Ok::<_, Infallible>(handle(&ctx, req).await) }
                        });
                        let conn = hyper::server::conn::http1::Builder::new()
                            .timer(TokioTimer::new())
                            .header_read_timeout(Duration::from_secs(15))
                            .max_buf_size(64 * 1024)
                            .serve_connection(TokioIo::new(stream), service);
                        tokio::pin!(conn);
                        tokio::select! {
                            _ = conn.as_mut() => {}
                            _ = rx.changed() => {}
                        }
                    });
                }
                Err(e) => {
                    log_warn!("[WEBXDC] isolated host accept error: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }
}

async fn handle(ctx: &HostContext, req: Request<Incoming>) -> Response<Full<Bytes>> {
    let route = ctx.gate.check(req.method(), req.uri().path(), req.uri().query(), req.headers());
    log_trace!("[WEBXDC] isolated {} {} -> {:?}", req.method(), req.uri().path(), route.as_ref().map(|_| "ok").map_err(|e| e.0));
    let (path, head_only) = match route {
        Err((status, message)) => {
            let mut res = error(status, message);
            if status == StatusCode::METHOD_NOT_ALLOWED {
                res.headers_mut().insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
            }
            // Nothing past a refusal is worth a kept-alive socket.
            res.headers_mut().insert(header::CONNECTION, HeaderValue::from_static("close"));
            return res;
        }
        Ok(Route::Boot { location, cookie }) => {
            let mut res = error(StatusCode::SEE_OTHER, "");
            if let (Ok(location), Ok(cookie)) = (HeaderValue::from_str(&location), HeaderValue::from_str(&cookie)) {
                res.headers_mut().insert(header::LOCATION, location);
                res.headers_mut().insert(header::SET_COOKIE, cookie);
            }
            return res;
        }
        Ok(Route::Serve { path, head_only }) => (path, head_only),
    };

    let state = ctx.app.state::<MiniAppsState>();
    let Some(instance) = state.get_instance(&ctx.window_label).await else {
        return error(StatusCode::NOT_FOUND, "Mini App not found");
    };
    let granted = crate::db::get_miniapp_granted_permissions(&instance.package.file_hash).unwrap_or_default();

    if path == "/webxdc.js" {
        let (npub, name) = super::scheme::get_user_info().await;
        let js = super::scheme::generate_webxdc_bridge_js(&npub, &name);
        return ok(js.into_bytes(), "text/javascript", &granted, head_only);
    }

    let file = if path == "/" { "index.html".to_string() } else { path.trim_start_matches('/').to_string() };
    let package = instance.package.clone();
    // Zip reads are blocking file I/O on a multi-megabyte archive.
    let loaded = tokio::task::spawn_blocking(move || match package.get_file(&file) {
        Ok(data) => Some((file, data)),
        Err(_) => package.get_file(&format!("{file}.html")).ok().map(|d| (format!("{file}.html"), d)),
    })
    .await
    .ok()
    .flatten();
    let Some((served, data)) = loaded else {
        return error(StatusCode::NOT_FOUND, "File not found");
    };

    let mime = super::scheme::get_mime_type(&served);
    if mime == "text/html" {
        let (npub, name) = super::scheme::get_user_info().await;
        let html = super::scheme::inject_webxdc_script(&data, &npub, &name);
        return ok(html, &mime, &granted, head_only);
    }
    ok(data, &mime, &granted, head_only)
}

fn ok(body: Vec<u8>, mime: &str, granted: &str, head_only: bool) -> Response<Full<Bytes>> {
    let len = body.len();
    let mut res = Response::new(Full::new(if head_only { Bytes::new() } else { Bytes::from(body) }));
    if let Ok(v) = HeaderValue::from_str(mime) {
        res.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    res.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    apply_security_headers(res.headers_mut(), granted);
    res
}

fn error(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    let mut res = Response::new(Full::new(Bytes::from(message.to_owned())));
    *res.status_mut() = status;
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    apply_security_headers(res.headers_mut(), "");
    res
}

/// Every response, errors included: an error page without CSP is an escape hatch.
fn apply_security_headers(headers: &mut HeaderMap, granted: &str) {
    let pairs: [(&str, String); 9] = [
        ("content-security-policy", super::scheme::isolated_csp().to_string()),
        ("permissions-policy", vector_core::webxdc_permissions::build_isolated_permissions_policy(granted)),
        ("cross-origin-opener-policy", "same-origin".into()),
        ("cross-origin-embedder-policy", "require-corp".into()),
        ("cross-origin-resource-policy", "same-origin".into()),
        ("x-content-type-options", "nosniff".into()),
        ("referrer-policy", "no-referrer".into()),
        // Pages carry the user's npub and name; nothing is worth caching across sessions.
        ("cache-control", "no-store".into()),
        ("x-frame-options", "SAMEORIGIN".into()),
    ];
    for (name, value) in pairs {
        if let Ok(v) = HeaderValue::from_str(&value) {
            headers.insert(name, v);
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    crate::util::bytes_to_hex_string(&buf)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn decode_component(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Percent-decode a request path and refuse anything that could leave the package.
fn decode_path(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let path = String::from_utf8(out).ok()?;
    if !path.starts_with('/') || path.contains('\\') || path.contains('\0') {
        return None;
    }
    // Empty segments collapse, as on any static server: engines build paths
    // like "/assets/maps/" + "/ui.map".
    let segments: Vec<&str> = path.split('/').filter(|seg| !seg.is_empty()).collect();
    if segments.iter().any(|seg| *seg == ".." || *seg == ".") {
        return None;
    }
    Some(format!("/{}", segments.join("/")))
}

/// Boot redirect target: a same-origin path only.
fn sanitize_target(raw: &str) -> Option<String> {
    let target = if raw.starts_with('/') { raw.to_string() } else { format!("/{raw}") };
    let ok = !target.starts_with("//")
        && !target.contains('\\')
        && target.len() <= 2048
        && !target.chars().any(|c| c.is_control());
    ok.then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "s3cr3t";
    const TOKEN: &str = "t0k3n";

    fn gate() -> Gate {
        Gate::new(20123, SECRET.into(), TOKEN.into())
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn session() -> String {
        format!("vector_isolated_20123={SECRET}")
    }

    #[test]
    fn a_window_with_its_session_is_served() {
        let g = gate();
        let h = headers(&[("host", "localhost:20123"), ("cookie", &session()), ("sec-fetch-site", "same-origin")]);
        assert_eq!(g.check(&Method::GET, "/assets/maps//ui.map", None, &h), Ok(Route::Serve { path: "/assets/maps/ui.map".into(), head_only: false }));
        assert_eq!(g.check(&Method::HEAD, "/", None, &h), Ok(Route::Serve { path: "/".into(), head_only: true }));
        // Other cookies alongside ours are fine.
        let mixed = headers(&[("host", "localhost:20123"), ("cookie", &format!("owner=A; {}", session()))]);
        assert!(g.check(&Method::GET, "/", None, &mixed).is_ok());
    }

    #[test]
    fn everyone_else_is_refused() {
        let g = gate();
        let refused = |method: Method, path: &str, h: HeaderMap| g.check(&method, path, None, &h).unwrap_err().0;
        let ok_host = ("host", "localhost:20123");
        let cookie = session();
        assert_eq!(refused(Method::GET, "/", headers(&[ok_host])), StatusCode::FORBIDDEN, "no cookie");
        assert_eq!(refused(Method::GET, "/", headers(&[ok_host, ("cookie", "vector_isolated_20123=guess")])), StatusCode::FORBIDDEN);
        // Another app's cookie (other port) does not open this one.
        assert_eq!(refused(Method::GET, "/", headers(&[ok_host, ("cookie", &format!("vector_isolated_20124={SECRET}"))])), StatusCode::FORBIDDEN);
        for host in ["evil.example:20123", "127.0.0.1:20123", "[::1]:20123", "localhost:20124", "localhost"] {
            assert_eq!(refused(Method::GET, "/", headers(&[("host", host), ("cookie", &cookie)])), StatusCode::MISDIRECTED_REQUEST, "{host}");
        }
        for site in ["cross-site", "same-site"] {
            assert_eq!(refused(Method::GET, "/", headers(&[ok_host, ("cookie", &cookie), ("sec-fetch-site", site)])), StatusCode::FORBIDDEN, "{site}");
        }
        assert_eq!(refused(Method::POST, "/", headers(&[ok_host, ("cookie", &cookie)])), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(refused(Method::GET, "/sw.js", headers(&[ok_host, ("cookie", &cookie), ("service-worker", "script")])), StatusCode::FORBIDDEN);
        assert_eq!(refused(Method::GET, "/%2e%2e/x", headers(&[ok_host, ("cookie", &cookie)])), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn the_boot_token_is_single_use_and_guesses_do_not_burn_it() {
        let g = gate();
        let h = headers(&[("host", "localhost:20123")]);
        let boot = |q: &str| g.check(&Method::GET, BOOT_PATH, Some(q), &h);
        assert_eq!(boot("t=wrong&to=/index.html").unwrap_err().0, StatusCode::FORBIDDEN);
        assert_eq!(g.check(&Method::HEAD, BOOT_PATH, Some(&format!("t={TOKEN}")), &h).unwrap_err().0, StatusCode::METHOD_NOT_ALLOWED);
        match boot(&format!("t={TOKEN}&to=%2Fgame.html%3Fx%3D1")) {
            Ok(Route::Boot { location, cookie }) => {
                assert_eq!(location, "/game.html?x=1");
                assert!(cookie.starts_with(&session()) && cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
            }
            other => panic!("boot failed: {other:?}"),
        }
        assert_eq!(boot(&format!("t={TOKEN}")).unwrap_err().0, StatusCode::FORBIDDEN, "replay");
    }

    #[test]
    fn boot_redirects_stay_on_origin() {
        let g = gate();
        let h = headers(&[("host", "localhost:20123")]);
        match g.check(&Method::GET, BOOT_PATH, Some(&format!("t={TOKEN}&to=%2F%2Fevil.example%2F")), &h) {
            Ok(Route::Boot { location, .. }) => assert_eq!(location, "/"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_expired_boot_token_is_refused() {
        let g = gate();
        *g.boot_token.lock().unwrap() = Some((TOKEN.into(), Instant::now() - BOOT_TOKEN_TTL - Duration::from_secs(1)));
        let h = headers(&[("host", "localhost:20123")]);
        assert_eq!(g.check(&Method::GET, BOOT_PATH, Some(&format!("t={TOKEN}")), &h).unwrap_err().0, StatusCode::FORBIDDEN);
        assert!(g.boot_token.lock().unwrap().is_none());
    }

    #[test]
    fn paths_cannot_escape_the_package() {
        assert_eq!(decode_path("/assets/maps/ui.map").as_deref(), Some("/assets/maps/ui.map"));
        assert_eq!(decode_path("/My%20File.png").as_deref(), Some("/My File.png"));
        assert_eq!(decode_path("/assets/maps//ui.map").as_deref(), Some("/assets/maps/ui.map"));
        assert_eq!(decode_path("/").as_deref(), Some("/"));
        for bad in ["/../x", "/a/../../b", "/%2e%2e/x", "/a/%2E%2E/b", "/a\\b", "/a%5cb", "/a%00b", "x", "/./a", "/%zz"] {
            assert!(decode_path(bad).is_none(), "{bad} should be refused");
        }
    }

    #[test]
    fn boot_targets_stay_on_origin() {
        assert_eq!(sanitize_target("/index.html?x=1").as_deref(), Some("/index.html?x=1"));
        assert_eq!(sanitize_target("game.html").as_deref(), Some("/game.html"));
        for bad in ["//evil.example/", "/\\evil", "/a\r\nSet-Cookie: x=y"] {
            assert!(sanitize_target(bad).is_none(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn preferred_ports_stay_in_range_and_depend_on_the_salt() {
        for p in ["abc", "", "z".repeat(200).as_str(), "halo-1234abcd"] {
            let port = preferred_port("salt", p);
            assert!((PORT_BASE..PORT_BASE + PORT_SPAN).contains(&port));
            assert!(port < 32768, "inside an ephemeral range");
        }
        assert_eq!(preferred_port("a", "same"), preferred_port("a", "same"));
        let differs = (0..32).any(|i| preferred_port(&format!("s{i}"), "same") != preferred_port("a", "same"));
        assert!(differs);
    }

    #[test]
    fn a_corrupt_ports_file_is_kept_aside_not_reset() {
        let dir = std::env::temp_dir().join(format!("vector-ports-{}", random_hex(4)));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("miniapp_isolated_ports.json");
        std::fs::write(&path, b"{not json").unwrap();
        let file = read_ports_file(&path);
        assert!(file.ports.is_empty() && !path.exists());
        assert!(std::fs::read_dir(&dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains("corrupt")));
        let mut fresh = PortsFile { salt: "x".into(), ..Default::default() };
        fresh.ports.insert("app".into(), 20001);
        write_ports_file(&path, &fresh);
        assert_eq!(read_ports_file(&path).ports.get("app"), Some(&20001));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn constant_time_eq_matches_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
