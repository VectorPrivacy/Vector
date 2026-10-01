//! Custom URI scheme handler for Mini Apps
//!
//! This provides the `webxdc://` protocol that serves content from .xdc packages
//! in an isolated context with strict CSP.

use std::borrow::Cow;
use std::collections::HashMap;
use tauri::{
    utils::config::{Csp, CspDirectiveSources},
    Manager, UriSchemeContext, UriSchemeResponder,
};

use nostr_sdk::prelude::ToBech32;
use std::sync::OnceLock;

use super::state::MiniAppsState;
use crate::STATE;

/// Content Security Policy for Mini Apps - very restrictive for security
/// Based on DeltaChat's implementation
fn csp() -> Cow<'static, str> {
    static CACHED: OnceLock<String> = OnceLock::new();
    cached_csp(&CACHED, false)
}

/// The same policy for apps on the loopback host (`isolated.rs`).
pub(super) fn isolated_csp() -> Cow<'static, str> {
    static CACHED: OnceLock<String> = OnceLock::new();
    cached_csp(&CACHED, true)
}

/// The realtime WebSocket's port, so the policy can name it instead of every
/// local port (any local WebSocket service, a debug build's MCP bridge included).
static RT_WS_PORT: OnceLock<u16> = OnceLock::new();

pub(crate) fn set_rt_ws_port(port: u16) {
    let _ = RT_WS_PORT.set(port);
}

#[cfg(target_os = "linux")]
pub(crate) fn rt_ws_port() -> Option<u16> {
    RT_WS_PORT.get().copied()
}

fn cached_csp(cache: &'static OnceLock<String>, isolated: bool) -> Cow<'static, str> {
    if let Some(policy) = cache.get() {
        return Cow::Borrowed(policy);
    }
    // Before the realtime server exists there is no port to allow; don't cache that.
    if RT_WS_PORT.get().is_none() {
        return Cow::Owned(build_csp(isolated));
    }
    Cow::Borrowed(cache.get_or_init(|| build_csp(isolated)))
}

fn build_csp(isolated: bool) -> String {
    let mut m: HashMap<String, CspDirectiveSources> = HashMap::new();
    
    // Only allow resources from self (the webxdc:// origin)
    m.insert(
        "default-src".to_owned(),
        CspDirectiveSources::List(vec!["'self'".to_owned()]),
    );
    
    // Allow inline styles and blob URLs for styles
    m.insert(
        "style-src".to_string(),
        CspDirectiveSources::List(vec![
            "'self'".to_owned(),
            "'unsafe-inline'".to_owned(),
            "blob:".to_owned(),
        ]),
    );
    
    // Allow data URLs and blob URLs for fonts
    m.insert(
        "font-src".to_string(),
        CspDirectiveSources::List(vec![
            "'self'".to_owned(),
            "data:".to_owned(),
            "blob:".to_owned(),
        ]),
    );
    
    // Allow inline scripts, eval, and WASM compilation (needed for many web apps).
    // 'wasm-unsafe-eval' is required: Chromium 145+ no longer grants WASM JIT
    // compilation permission from 'unsafe-eval' alone, causing V8 to interpret
    // WASM bytecode (~50-100x slower) instead of compiling it to native code.
    m.insert(
        "script-src".to_string(),
        CspDirectiveSources::List(vec![
            "'self'".to_owned(),
            "'unsafe-inline'".to_owned(),
            "'unsafe-eval'".to_owned(),
            "'wasm-unsafe-eval'".to_owned(),
            "blob:".to_owned(),
        ]),
    );
    
    // Restrict connections to self, IPC, data/blob URLs, and the realtime WebSocket
    let mut connect = vec![
        "'self'".to_owned(),
        "ipc:".to_owned(),
        "data:".to_owned(),
        "blob:".to_owned(),
    ];
    if let Some(port) = RT_WS_PORT.get() {
        connect.push(format!("ws://127.0.0.1:{port}"));
    }
    if isolated {
        // WebView2's IPC endpoint; without it Tauri falls back to postMessage.
        if cfg!(windows) {
            connect.push("http://ipc.localhost".to_owned());
        }
        m.insert(
            "frame-ancestors".to_string(),
            CspDirectiveSources::List(vec!["'self'".to_owned()]),
        );
    }
    m.insert("connect-src".to_string(), CspDirectiveSources::List(connect));
    
    // Allow data URLs and blob URLs for images
    m.insert(
        "img-src".to_string(),
        CspDirectiveSources::List(vec![
            "'self'".to_owned(),
            "data:".to_owned(),
            "blob:".to_owned(),
        ]),
    );
    
    // Allow data URLs and blob URLs for media
    m.insert(
        "media-src".to_string(),
        CspDirectiveSources::List(vec![
            "'self'".to_owned(),
            "data:".to_owned(),
            "blob:".to_owned(),
        ]),
    );
    
    // CSP "WEBRTC: block" directive is specified, but not yet implemented by browsers
    // - see https://delta.chat/en/2023-05-22-webxdc-security#browsers-please-implement-the-w3c-webrtc-block-directive
    m.insert(
        "webrtc".to_string(),
        CspDirectiveSources::List(vec!["'block'".to_owned()]),
    );
    
    let csp = Csp::DirectiveMap(m);

    // 'self' only: each app's window is served from its own per-partition
    // origin, and naming a fixed host here would let an app iframe another
    // origin's storage (including the legacy shared one).
    csp.to_string()
}

use vector_core::webxdc_permissions::build_permissions_policy;

/// Handle requests to the webxdc:// protocol (async version for Tauri 2)
/// Uses UriSchemeResponder to avoid blocking the WebView thread on Windows
pub fn miniapp_protocol<R: tauri::Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: http::Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    log_trace!(
        "webxdc_protocol: {} {}",
        request.uri(),
        request.uri().path()
    );

    // URI format (host = per-app storage partition):
    // macOS/Linux: webxdc://<partition>.host/<path>
    // Windows: arrives as http://webxdc.<partition>.host/<path>, reverted by
    //          wry's prefix workaround to webxdc://<partition>.host/<path>

    let webview_label = ctx.webview_label().to_owned();

    // Security: Only allow Mini App windows to access this scheme
    if !webview_label.starts_with("miniapp:") {
        log_error!(
            "Prevented non-miniapp window from accessing webxdc:// scheme (webview label: {webview_label})"
        );
        responder.respond(make_error_response(http::StatusCode::FORBIDDEN, "Access denied", ""));
        return;
    }

    let app_handle = ctx.app_handle().clone();

    // Spawn an async task to handle the request without blocking
    // This is the pattern used by DeltaChat to avoid deadlocks on Windows
    tauri::async_runtime::spawn(async move {
        let response = handle_miniapp_request(&app_handle, &webview_label, &request).await;
        responder.respond(response);
    });
}

async fn handle_miniapp_request<R: tauri::Runtime>(
    app_handle: &tauri::AppHandle<R>,
    window_label: &str,
    request: &http::Request<Vec<u8>>,
) -> http::Response<Cow<'static, [u8]>> {
    // Get the Mini App instance for this window
    let state = app_handle.state::<MiniAppsState>();
    let instance = match state.get_instance(window_label).await {
        Some(inst) => inst,
        None => {
            log_error!("Mini App instance not found for window: {window_label}");
            return make_error_response(http::StatusCode::NOT_FOUND, "Mini App not found", "");
        }
    };

    // Look up granted permissions for this app using the file hash (content-based security)
    let granted_permissions = crate::db::get_miniapp_granted_permissions(&instance.package.file_hash)
        .unwrap_or_default();

    let path = request.uri().path();

    // Handle special paths - serve webxdc.js bridge script
    if path == "/webxdc.js" {
        // Get user's npub and display name for selfAddr and selfName
        let (user_npub, user_display_name) = get_user_info().await;
        return serve_webxdc_js(&instance, &user_npub, &user_display_name, &granted_permissions);
    }

    // Serve file from the package
    let file_path = if path == "/" || path.is_empty() {
        "index.html"
    } else {
        path.trim_start_matches('/')
    };

    match instance.package.get_file(file_path) {
        Ok(data) => {
            let mime_type = get_mime_type(file_path);
            // For HTML files, inject the webxdc.js script automatically
            if mime_type == "text/html" {
                let (user_npub, user_display_name) = get_user_info().await;
                let injected = inject_webxdc_script(&data, &user_npub, &user_display_name);
                make_success_response(injected, &mime_type, &granted_permissions)
            } else {
                make_success_response(data, &mime_type, &granted_permissions)
            }
        }
        Err(_) => {
            // Try with .html extension
            let html_path = format!("{}.html", file_path);
            match instance.package.get_file(&html_path) {
                Ok(data) => {
                    let (user_npub, user_display_name) = get_user_info().await;
                    let injected = inject_webxdc_script(&data, &user_npub, &user_display_name);
                    make_success_response(injected, "text/html", &granted_permissions)
                }
                Err(_) => make_error_response(http::StatusCode::NOT_FOUND, "File not found", &granted_permissions),
            }
        }
    }
}

/// Get the current user's npub and display name
/// Note: This function avoids locking STATE to prevent potential deadlocks
/// when called from the protocol handler
pub(super) async fn get_user_info() -> (String, String) {
    // Get user's npub from Nostr client
    let user_npub = if let Some(pk) = crate::my_public_key() {
        pk.to_bech32().unwrap_or_else(|_| "unknown".to_string())
    } else {
        "unknown".to_string()
    };
    
    // Get user's display name from their profile in STATE
    // Use try_lock to avoid blocking if STATE is locked
    let user_display_name = {
        match STATE.try_lock() {
            Ok(state) => {
                // Find the user's own profile (where mine == true)
                if let Some(profile) = state.profiles.iter().find(|p| p.flags.is_mine()) {
                    if !profile.nickname().is_empty() {
                        profile.nickname().to_string()
                    } else if !profile.name.is_empty() {
                        profile.name.to_string()
                    } else {
                        user_npub.clone()
                    }
                } else {
                    user_npub.clone()
                }
            }
            Err(_) => {
                // STATE is locked, use npub as fallback
                log_trace!("STATE is locked, using npub as display name fallback");
                user_npub.clone()
            }
        }
    };
    
    (user_npub, user_display_name)
}

/// Serve the webxdc.js bridge script
/// Delegates to the canonical `generate_webxdc_bridge_js` to avoid maintaining two copies.
fn serve_webxdc_js(
    _instance: &super::state::MiniAppInstance,
    user_npub: &str,
    user_display_name: &str,
    granted_permissions: &str,
) -> http::Response<Cow<'static, [u8]>> {
    let js = generate_webxdc_bridge_js(user_npub, user_display_name);
    make_success_response(js.into_bytes(), "text/javascript", granted_permissions)
}

/// Inject the webxdc.js script inline into HTML content
/// This ensures window.webxdc is available before any other scripts run
/// If the HTML already includes webxdc.js, we skip injection to avoid duplicates
pub(super) fn inject_webxdc_script(html_data: &[u8], user_npub: &str, user_display_name: &str) -> Vec<u8> {
    let html_str = String::from_utf8_lossy(html_data);
    
    // Check if the HTML already includes webxdc.js - if so, don't inject
    // This prevents duplicate initialization if the mini app manually includes it
    let html_lower = html_str.to_lowercase();
    if html_lower.contains("webxdc.js") {
        // Already includes webxdc.js, return original HTML
        return html_data.to_vec();
    }
    
    // Generate the inline webxdc script
    let webxdc_script = generate_webxdc_bridge_js(user_npub, user_display_name);
    
    // Try to inject after <head> tag, or at the start of the document
    let injected = if let Some(head_pos) = html_lower.find("<head>") {
        let insert_pos = head_pos + 6; // After "<head>"
        format!(
            "{}<script>{}</script>{}",
            &html_str[..insert_pos],
            webxdc_script,
            &html_str[insert_pos..]
        )
    } else if let Some(html_pos) = html_lower.find("<html") {
        // Find the end of the <html> tag
        if let Some(close_pos) = html_str[html_pos..].find('>') {
            let insert_pos = html_pos + close_pos + 1;
            format!(
                "{}<script>{}</script>{}",
                &html_str[..insert_pos],
                webxdc_script,
                &html_str[insert_pos..]
            )
        } else {
            // Fallback: prepend to document
            format!("<script>{}</script>{}", webxdc_script, html_str)
        }
    } else {
        // Fallback: prepend to document
        format!("<script>{}</script>{}", webxdc_script, html_str)
    };
    
    injected.into_bytes()
}

/// Generate the canonical webxdc.js bridge script (used by both serve and inline injection).
/// All console.log/warn calls stripped from hot paths — only console.error for actual failures.
pub(super) fn generate_webxdc_bridge_js(user_npub: &str, user_display_name: &str) -> String {
    format!(r#"
(function() {{
    'use strict';

    // base91 codec (matches Rust fast-thumbhash alphabet, ~14% overhead vs base64's 33%)
    var B91='ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!#$%&()*+,./:;<=>?@[]^_`{{|}}~ ';
    var B91D=new Uint8Array(256);B91D.fill(255);for(var _i=0;_i<91;_i++)B91D[B91.charCodeAt(_i)]=_i;
    function b91e(buf){{var o='',n=0,b=0;for(var i=0;i<buf.length;i++){{n|=buf[i]<<b;b+=8;if(b>13){{var v=n&8191;if(v>88){{n>>=13;b-=13;}}else{{v=n&16383;n>>=14;b-=14;}}o+=B91[v%91]+B91[v/91|0];}}}}if(b>0){{o+=B91[n%91];if(b>7||n>90)o+=B91[n/91|0];}}return o;}}
    function b91d(s){{var o=[],n=0,b=0,q=-1;for(var i=0;i<s.length;i++){{var d=B91D[s.charCodeAt(i)];if(d===255)continue;if(q<0){{q=d;}}else{{var v=q+d*91;q=-1;n|=v<<b;b+=(v&8191)>88?13:14;while(b>=8){{o.push(n&255);n>>=8;b-=8;}}}}}}if(q>=0)o.push((n|(q<<b))&255);return new Uint8Array(o);}}

    var selfAddr = {self_addr};
    var selfName = {self_name};

    var updateListener = null;
    var lastKnownSerial = 0;
    var realtimeChannel = null;
    var realtimeListener = null;
    var tauriChannel = null;
    var rtWs = null; // WebSocket for realtime fast-path
    var rtWsFailed = false; // true if WS can't connect (e.g. Linux/WebKitGTK)

    function waitForTauri(callback) {{
        if (window.__TAURI__ && window.__TAURI__.core) {{
            callback();
        }} else {{
            setTimeout(function() {{ waitForTauri(callback); }}, 50);
        }}
    }}

    window.webxdc = {{
        selfAddr: selfAddr,
        selfName: selfName,

        setUpdateListener: function(listener, serial) {{
            updateListener = listener;
            lastKnownSerial = serial || 0;
            waitForTauri(function() {{
                window.__TAURI__.core.invoke('miniapp_get_updates', {{
                    lastKnownSerial: lastKnownSerial
                }}).then(function(updates) {{
                    if (updates && updateListener) {{
                        var parsed = JSON.parse(updates);
                        parsed.forEach(function(update) {{ updateListener(update); }});
                    }}
                }}).catch(function(err) {{
                    console.error('[webxdc] Failed to get updates:', err);
                }});
            }});
            return Promise.resolve();
        }},

        sendUpdate: function(update, description) {{
            return new Promise(function(resolve, reject) {{
                waitForTauri(function() {{
                    window.__TAURI__.core.invoke('miniapp_send_update', {{
                        update: update,
                        description: description || ''
                    }}).then(resolve).catch(reject);
                }});
            }});
        }},

        sendToChat: function() {{
            return Promise.reject(new Error('Not implemented'));
        }},

        importFiles: function() {{
            return Promise.reject(new Error('Not implemented'));
        }},

        joinRealtimeChannel: function() {{
            if (realtimeChannel !== null) {{
                return realtimeChannel;
            }}

            realtimeChannel = {{
                setListener: function(listener) {{
                    realtimeListener = listener;
                }},

                send: function(data) {{
                    if (realtimeChannel === null) return;
                    var buf = data instanceof Uint8Array ? data : new Uint8Array(data);
                    // Fast path: WebSocket binary frame (persistent TCP, ~1μs per msg)
                    if (rtWs && rtWs.readyState === 1) {{
                        rtWs.send(buf);
                    }} else {{
                        // Fallback: Tauri invoke via waitForTauri (queues until __TAURI__ is injected).
                        // On Android, __TAURI__ is injected asynchronously — direct checks fail.
                        waitForTauri(function() {{
                            window.__TAURI__.core.invoke('miniapp_send_realtime_data', {{
                                data: Array.from(buf)
                            }});
                        }});
                    }}
                }},

                leave: function() {{
                    realtimeListener = null;
                    realtimeChannel = null;
                    if (rtWs) {{ try {{ rtWs.close(); }} catch(e) {{}} rtWs = null; }}
                    waitForTauri(function() {{
                        window.__TAURI__.core.invoke('miniapp_leave_realtime_channel', {{}}).catch(function(err) {{
                            console.error('[webxdc] Failed to leave realtime channel:', err);
                        }});
                    }});
                }}
            }};

            waitForTauri(function() {{
                tauriChannel = new window.__TAURI__.core.Channel();
                tauriChannel.onmessage = function(event) {{
                    if (realtimeListener && event && event.event === 'data' && event.data) {{
                        realtimeListener(b91d(event.data));
                    }}
                }};
                window.__TAURI__.core.invoke('miniapp_join_realtime_channel', {{
                    channel: tauriChannel
                }}).then(function(result) {{
                    // Open WebSocket fast-path if backend returned a URL
                    if (result && result.ws_url) {{
                        var wsUrl = result.ws_url;
                        var label = encodeURIComponent(window.__TAURI_INTERNALS__.metadata.currentWebview.label || '');
                        var wsRetried = false;
                        function connectWs() {{
                            try {{
                                rtWs = new WebSocket(wsUrl + '/' + label);
                                rtWs.binaryType = 'arraybuffer';
                                rtWs.onclose = function() {{ rtWs = null; }};
                                rtWs.onerror = function() {{
                                    try {{ rtWs.close(); }} catch(e) {{}}
                                    rtWs = null;
                                    // Retry once after 200ms (accept loop may not have polled yet)
                                    if (!wsRetried) {{
                                        wsRetried = true;
                                        setTimeout(connectWs, 200);
                                    }} else {{
                                        rtWsFailed = true;
                                    }}
                                }};
                                // Detect WebKitGTK silent WS block: if still CONNECTING after 1.5s, fall back to invoke
                                setTimeout(function() {{
                                    if (rtWs && rtWs.readyState === 0) {{
                                        console.warn('[webxdc] WebSocket stuck in CONNECTING — falling back to invoke');
                                        try {{ rtWs.close(); }} catch(e) {{}}
                                        rtWs = null;
                                        rtWsFailed = true;
                                    }}
                                }}, 1500);
                            }} catch(e) {{
                                rtWs = null;
                                rtWsFailed = true;
                            }}
                        }}
                        connectWs();
                    }}
                }}).catch(function(err) {{
                    console.error('[webxdc] Failed to join realtime channel:', err);
                }});
            }});

            return realtimeChannel;
        }}
    }};
}})();
"#,
        self_addr = serde_json::to_string(user_npub).unwrap_or_else(|_| "\"unknown\"".to_string()),
        self_name = serde_json::to_string(user_display_name).unwrap_or_else(|_| "\"Unknown\"".to_string()),
    )
}

fn make_success_response(body: Vec<u8>, content_type: &str, granted_permissions: &str) -> http::Response<Cow<'static, [u8]>> {
    let permissions_policy = build_permissions_policy(granted_permissions);
    http::Response::builder()
        .status(http::StatusCode::OK)
        .header(http::header::CONTENT_TYPE, content_type)
        .header(http::header::CONTENT_SECURITY_POLICY, csp().into_owned())
        // Ensure that the browser doesn't try to interpret the file incorrectly
        .header(http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        // Dynamic permissions policy based on user grants
        .header("Permissions-Policy", permissions_policy)
        // No custom-scheme page is ever cross-origin isolated (WebKit enforces COOP
        // only on network loads; WebView2 maps the scheme to a non-secure host), so
        // these do not grant SharedArrayBuffer here. Apps that need it opt in to the
        // loopback host in `isolated.rs`.
        .header("Cross-Origin-Opener-Policy", "same-origin")
        .header("Cross-Origin-Embedder-Policy", "require-corp")
        .body(Cow::Owned(body))
        .unwrap_or_else(|_| make_error_response(http::StatusCode::INTERNAL_SERVER_ERROR, "Failed to build response", ""))
}

fn make_error_response(status: http::StatusCode, message: &str, granted_permissions: &str) -> http::Response<Cow<'static, [u8]>> {
    // IMPORTANT: Set CSP on ALL responses including errors
    // Failing to set CSP might result in the app being able to create
    // an <iframe> with no CSP, e.g. `<iframe src="/no_such_file.lol">`
    // within which they can then do whatever through the parent frame
    // See: "XDC-01-002 WP1: Full CSP bypass via desktop app webxdc.js"
    // https://public.opentech.fund/documents/XDC-01-report_2_1.pdf
    let permissions_policy = build_permissions_policy(granted_permissions);
    http::Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "text/plain")
        .header(http::header::CONTENT_SECURITY_POLICY, csp().into_owned())
        .header(http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header("Permissions-Policy", permissions_policy)
        // Cross-origin isolation headers for SharedArrayBuffer (WASM threads)
        .header("Cross-Origin-Opener-Policy", "same-origin")
        .header("Cross-Origin-Embedder-Policy", "require-corp")
        .body(Cow::Owned(message.as_bytes().to_vec()))
        .unwrap()
}

pub(super) fn get_mime_type(path: &str) -> String {
    let extension = path.rsplit('.').next().unwrap_or("");
    match extension.to_lowercase().as_str() {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "wasm" => "application/wasm",
        "xml" => "application/xml",
        "txt" => "text/plain",
        "md" => "text/markdown",
        // Block PDF to prevent CSP bypass
        // The PDF viewer allows the app to bypass CSP, at least on Chromium.
        // See https://delta.chat/en/2023-05-22-webxdc-security,
        // "XDC-01-005 WP1: Full CSP bypass via desktop app PDF embed".
        "pdf" => "application/octet-stream",
        _ => "application/octet-stream",
    }.to_string()
}