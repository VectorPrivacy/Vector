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
//! cookie, and every other request must carry that cookie, an exact Host
//! (DNS rebinding) and a same-origin fetch context. Other local users, other
//! apps' windows and browser tabs get 403s.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{self, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use tauri::{AppHandle, Manager};
use tokio::net::TcpListener;
use tokio::sync::watch;

use super::state::MiniAppsState;

/// Ports are drawn from here: clear of the ephemeral range on every OS and of
/// the usual dev-server ports.
const PORT_BASE: u16 = 41000;
const PORT_SPAN: u16 = 18000;
const PORT_ATTEMPTS: u16 = 64;
const BOOT_PATH: &str = "/__vector/boot";
const COOKIE_NAME: &str = "vector_isolated";
const BOOT_TOKEN_TTL: Duration = Duration::from_secs(120);

/// A running host, owned by the window it serves.
pub(crate) struct IsolatedHost {
    pub port: u16,
    shutdown: watch::Sender<bool>,
}

struct HostContext {
    app: AppHandle,
    window_label: String,
    hosts: [String; 3],
    session_secret: String,
    boot_token: Mutex<Option<(String, Instant)>>,
}

/// Window label → running host.
static HOSTS: LazyLock<Mutex<HashMap<String, IsolatedHost>>> = LazyLock::new(Default::default);
/// Ports currently serving an app; read by the macOS pointer-lock grant.
static LIVE_PORTS: LazyLock<RwLock<HashSet<u16>>> = LazyLock::new(Default::default);

pub(crate) fn origin(port: u16) -> String {
    format!("http://localhost:{port}")
}

pub(crate) fn is_live_port(port: u16) -> bool {
    LIVE_PORTS.read().map(|p| p.contains(&port)).unwrap_or(false)
}

/// Start serving `window_label`'s app. Returns the port and the one-time boot
/// URL the window must open first (it redirects to `href` or `/`).
pub(crate) async fn start(
    app: &AppHandle,
    window_label: &str,
    partition: &str,
    href: Option<&str>,
) -> Result<(u16, tauri::Url), String> {
    stop(window_label);

    let (v4, v6, port) = bind_pair(app, partition).await?;
    let session_secret = random_hex(32);
    let boot_token = random_hex(32);
    let ctx = Arc::new(HostContext {
        app: app.clone(),
        window_label: window_label.to_string(),
        hosts: [
            format!("localhost:{port}"),
            format!("127.0.0.1:{port}"),
            format!("[::1]:{port}"),
        ],
        session_secret,
        boot_token: Mutex::new(Some((boot_token.clone(), Instant::now()))),
    });

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    for listener in [Some(v4), v6].into_iter().flatten() {
        let ctx = Arc::clone(&ctx);
        let rx = shutdown_rx.clone();
        // spawn-detached: loopback accept loop for one Mini App window — no account storage; ends on window close.
        tokio::spawn(accept_loop(listener, ctx, rx));
    }

    LIVE_PORTS.write().map_err(|_| "port registry poisoned")?.insert(port);
    HOSTS
        .lock()
        .map_err(|_| "host registry poisoned")?
        .insert(window_label.to_string(), IsolatedHost { port, shutdown: shutdown_tx });

    let target = sanitize_target(href.unwrap_or("/")).unwrap_or_else(|| "/".to_string());
    let mut boot = tauri::Url::parse(&format!("{}{}", origin(port), BOOT_PATH)).map_err(|e| e.to_string())?;
    boot.query_pairs_mut().append_pair("t", &boot_token).append_pair("to", &target);
    Ok((port, boot))
}

/// Stop the host for `window_label`, if any. Open connections end with it.
pub(crate) fn stop(window_label: &str) {
    let removed = HOSTS.lock().ok().and_then(|mut h| h.remove(window_label));
    if let Some(host) = removed {
        let _ = host.shutdown.send(true);
        if let Ok(mut live) = LIVE_PORTS.write() {
            live.remove(&host.port);
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn port_for(window_label: &str) -> Option<u16> {
    HOSTS.lock().ok()?.get(window_label).map(|h| h.port)
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

// ── Port allocation ────────────────────────────────────────────────────────

/// Storage keys on origin, and the origin includes the port, so an app keeps
/// its port across launches (persisted per partition) unless it is taken.
async fn bind_pair(app: &AppHandle, partition: &str) -> Result<(TcpListener, Option<TcpListener>, u16), String> {
    let map_path = ports_file(app)?;
    let mut map = read_port_map(&map_path);
    let taken_by_others: HashSet<u16> = map
        .iter()
        .filter(|(k, _)| k.as_str() != partition)
        .map(|(_, v)| *v)
        .collect();
    let preferred = map.get(partition).copied().unwrap_or_else(|| preferred_port(partition));

    let mut candidate = preferred;
    for _ in 0..PORT_ATTEMPTS {
        let live = is_live_port(candidate);
        if !live && (candidate == preferred || !taken_by_others.contains(&candidate)) {
            if let Some((v4, v6)) = try_bind(candidate).await {
                if map.get(partition) != Some(&candidate) {
                    map.insert(partition.to_string(), candidate);
                    write_port_map(&map_path, &map);
                }
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
    match TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, port))).await {
        Ok(v6) => Some((v4, Some(v6))),
        // No IPv6 loopback on this machine: `localhost` cannot resolve to ::1 either.
        Err(e) if e.kind() == std::io::ErrorKind::AddrNotAvailable => Some((v4, None)),
        Err(_) => None,
    }
}

fn preferred_port(partition: &str) -> u16 {
    let mut h: u32 = 0x811c9dc5;
    for b in partition.bytes() {
        h = (h ^ b as u32).wrapping_mul(0x01000193);
    }
    PORT_BASE + (h % PORT_SPAN as u32) as u16
}

fn ports_file(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("miniapp_isolated_ports.json"))
}

fn read_port_map(path: &PathBuf) -> HashMap<String, u16> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<HashMap<String, u16>>(&b).ok())
        .map(|m| m.into_iter().filter(|(_, p)| (PORT_BASE..PORT_BASE + PORT_SPAN).contains(p)).collect())
        .unwrap_or_default()
}

fn write_port_map(path: &PathBuf, map: &HashMap<String, u16>) {
    if let Ok(json) = serde_json::to_vec(map) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

// ── Serving ────────────────────────────────────────────────────────────────

async fn accept_loop(listener: TcpListener, ctx: Arc<HostContext>, mut shutdown: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    if !peer.ip().is_loopback() {
                        continue;
                    }
                    let _ = stream.set_nodelay(true);
                    let ctx = Arc::clone(&ctx);
                    let mut rx = shutdown.clone();
                    // spawn-detached: one loopback HTTP connection for a Mini App window — no account storage.
                    tokio::spawn(async move {
                        let service = hyper::service::service_fn(move |req| {
                            let ctx = Arc::clone(&ctx);
                            async move {
                                let line = format!("{} {}", req.method(), req.uri().path());
                                let res = handle(&ctx, req).await;
                                log_trace!("[WEBXDC] isolated {} -> {}", line, res.status().as_u16());
                                Ok::<_, Infallible>(res)
                            }
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
    let head_only = req.method() == Method::HEAD;
    if req.method() != Method::GET && !head_only {
        let mut res = error(StatusCode::METHOD_NOT_ALLOWED, "GET and HEAD only");
        res.headers_mut().insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return res;
    }

    // Exact Host: a rebinding attacker's name resolving to 127.0.0.1 is refused.
    let host_ok = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| ctx.hosts.iter().any(|allowed| allowed == h));
    if !host_ok {
        return error(StatusCode::MISDIRECTED_REQUEST, "Unknown host");
    }

    // Only the app's own pages may fetch from it (other ports are "same-site").
    if let Some(site) = req.headers().get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        if site != "same-origin" && site != "none" {
            return error(StatusCode::FORBIDDEN, "Cross-origin request refused");
        }
    }

    let Some(path) = decode_path(req.uri().path()) else {
        return error(StatusCode::BAD_REQUEST, "Bad path");
    };

    if path == BOOT_PATH {
        return boot(ctx, req.uri().query().unwrap_or(""));
    }

    if !has_session_cookie(ctx, &req) {
        return error(StatusCode::FORBIDDEN, "Not this window's session");
    }

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
    let loaded = {
        let file = file.clone();
        // Zip reads are blocking file I/O on a multi-megabyte archive.
        tokio::task::spawn_blocking(move || {
            match package.get_file(&file) {
                Ok(data) => Some((file, data)),
                Err(_) => package.get_file(&format!("{file}.html")).ok().map(|d| (format!("{file}.html"), d)),
            }
        })
        .await
        .ok()
        .flatten()
    };
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

/// Trade the single-use boot token for this window's session cookie.
fn boot(ctx: &HostContext, query: &str) -> Response<Full<Bytes>> {
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
    let valid = {
        let Ok(mut slot) = ctx.boot_token.lock() else {
            return error(StatusCode::INTERNAL_SERVER_ERROR, "Boot state poisoned");
        };
        match slot.take() {
            Some((expected, issued)) if issued.elapsed() < BOOT_TOKEN_TTL && constant_time_eq(expected.as_bytes(), presented.as_bytes()) => true,
            other => {
                // A wrong guess must not burn the real token.
                if let Some((expected, issued)) = other {
                    if issued.elapsed() < BOOT_TOKEN_TTL {
                        *slot = Some((expected, issued));
                    }
                }
                false
            }
        }
    };
    if !valid {
        return error(StatusCode::FORBIDDEN, "Boot token invalid or used");
    }
    let mut res = error(StatusCode::SEE_OTHER, "");
    let location = target.unwrap_or_else(|| "/".to_string());
    if let Ok(v) = HeaderValue::from_str(&location) {
        res.headers_mut().insert(header::LOCATION, v);
    }
    let cookie = format!("{COOKIE_NAME}={}; Path=/; HttpOnly; SameSite=Strict", ctx.session_secret);
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        res.headers_mut().insert(header::SET_COOKIE, v);
    }
    res
}

fn has_session_cookie(ctx: &HostContext, req: &Request<Incoming>) -> bool {
    req.headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .any(|(k, v)| k == COOKIE_NAME && constant_time_eq(v.as_bytes(), ctx.session_secret.as_bytes()))
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
fn apply_security_headers(headers: &mut hyper::HeaderMap, granted: &str) {
    let pairs: [(&str, String); 9] = [
        ("content-security-policy", super::scheme::isolated_csp().to_string()),
        ("permissions-policy", vector_core::webxdc_permissions::build_isolated_permissions_policy(granted)),
        ("cross-origin-opener-policy", "same-origin".into()),
        ("cross-origin-embedder-policy", "require-corp".into()),
        ("cross-origin-resource-policy", "same-origin".into()),
        ("x-content-type-options", "nosniff".into()),
        ("referrer-policy", "no-referrer".into()),
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
                let hex = bytes.get(i + 1..i + 3)?;
                let s = std::str::from_utf8(hex).ok()?;
                out.push(u8::from_str_radix(s, 16).ok()?);
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
    fn preferred_ports_stay_in_range() {
        for p in ["abc", "", "z".repeat(200).as_str(), "halo-1234abcd"] {
            let port = preferred_port(p);
            assert!((PORT_BASE..PORT_BASE + PORT_SPAN).contains(&port));
        }
        assert_eq!(preferred_port("same"), preferred_port("same"));
    }

    #[test]
    fn constant_time_eq_matches_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
