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
use std::sync::LazyLock;

use super::state::MiniAppsState;
use crate::STATE;

/// Content Security Policy for Mini Apps - very restrictive for security
/// Based on DeltaChat's implementation
static CSP: LazyLock<String> = LazyLock::new(|| {
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
    
    // Restrict connections to self, IPC, data/blob URLs, and localhost WebSocket
    // (the realtime WS server uses a random token for auth, so wildcard port is safe)
    m.insert(
        "connect-src".to_string(),
        CspDirectiveSources::List(vec![
            "'self'".to_owned(),
            "ipc:".to_owned(),
            "data:".to_owned(),
            "blob:".to_owned(),
            "ws://127.0.0.1:*".to_owned(),
        ]),
    );
    
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
    
    // Add custom schemes for Windows/Android compatibility
    #[cfg(any(target_os = "windows", target_os = "android"))]
    {
        // On Windows/Android, we use http://webxdc.localhost which needs to be in CSP
        csp.to_string().replace("'self'", "'self' http://webxdc.localhost")
    }
    #[cfg(not(any(target_os = "windows", target_os = "android")))]
    {
        csp.to_string()
    }
});

/// Base Permissions Policy that denies all sensitive APIs by default
/// This is a comprehensive list from DeltaChat based on W3C spec
/// https://github.com/w3c/webappsec-permissions-policy/blob/main/features.md
///
/// NOTE: Some permissions can be dynamically enabled if the user grants them.
/// See `build_permissions_policy()` for dynamic generation.
const PERMISSIONS_POLICY_DENY_ALL: &str = concat!(
    "accelerometer=(), ",
    "ambient-light-sensor=(), ",
    "attribution-reporting=(), ",
    "autoplay=(self), ",
    "battery=(), ",
    "bluetooth=(), ",
    "camera=(), ",
    "ch-ua=(), ",
    "ch-ua-arch=(), ",
    "ch-ua-bitness=(), ",
    "ch-ua-full-version=(), ",
    "ch-ua-full-version-list=(), ",
    "ch-ua-high-entropy-values=(), ",
    "ch-ua-mobile=(), ",
    "ch-ua-model=(), ",
    "ch-ua-platform=(), ",
    "ch-ua-platform-version=(), ",
    "ch-ua-wow64=(), ",
    "compute-pressure=(), ",
    "cross-origin-isolated=(), ",
    "direct-sockets=(), ",
    "display-capture=(), ",
    "encrypted-media=(), ",
    "execution-while-not-rendered=(), ",
    "execution-while-out-of-viewport=(), ",
    "fullscreen=(), ",
    "geolocation=(), ",
    "gyroscope=(), ",
    "hid=(), ",
    "identity-credentials-get=(), ",
    "idle-detection=(), ",
    "keyboard-map=(), ",
    "magnetometer=(), ",
    "mediasession=(), ",
    "microphone=(), ",
    "midi=(), ",
    "navigation-override=(), ",
    "otp-credentials=(), ",
    "payment=(), ",
    "picture-in-picture=(), ",
    "publickey-credentials-get=(), ",
    "screen-wake-lock=(), ",
    "serial=(), ",
    "sync-xhr=(), ",
    "storage-access=(), ",
    "usb=(), ",
    "web-share=(), ",
    "window-management=(), ",
    "xr-spatial-tracking=(), ",
    "autofill=(), ",
    "clipboard-read=(), ",
    "clipboard-write=(), ",
    "deferred-fetch=(), ",
    "gamepad=(self), ",
    "language-detector=(), ",
    "language-model=(), ",
    "manual-text=(), ",
    "rewriter=(), ",
    "speaker-selection=(), ",
    "summarizer=(), ",
    "translator=(), ",
    "writer=(), ",
    "all-screens-capture=(), ",
    "browsing-topics=(), ",
    "captured-surface-control=(), ",
    "conversion-measurement=(), ",
    "digital-credentials-get=(), ",
    "digital-credentials-create=(), ",
    "focus-without-user-activation=(), ",
    "join-ad-interest-group=(), ",
    "local-fonts=(), ",
    "monetization=(), ",
    "run-ad-auction=(), ",
    "smart-card=(), ",
    "sync-script=(), ",
    "trust-token-redemption=(), ",
    "unload=(), ",
    "vertical-scroll=(), ",
    "document-domain=(), ",
    "window-placement=()",
);

/// Permission policies that can be dynamically enabled based on user grants
/// Maps permission name -> (policy directive name, allow value when enabled)
const GRANTABLE_PERMISSIONS: &[(&str, &str)] = &[
    ("microphone", "microphone"),
    ("camera", "camera"),
    ("geolocation", "geolocation"),
    ("clipboard-read", "clipboard-read"),
    ("clipboard-write", "clipboard-write"),
    ("fullscreen", "fullscreen"),
    ("autoplay", "autoplay"),
    ("display-capture", "display-capture"),
    ("midi", "midi"),
    ("picture-in-picture", "picture-in-picture"),
    ("screen-wake-lock", "screen-wake-lock"),
    ("speaker-selection", "speaker-selection"),
    ("accelerometer", "accelerometer"),
    ("gyroscope", "gyroscope"),
    ("magnetometer", "magnetometer"),
    ("ambient-light-sensor", "ambient-light-sensor"),
    ("bluetooth", "bluetooth"),
];

/// Build a dynamic Permissions-Policy header based on granted permissions
///
/// This takes the base deny-all policy and enables specific permissions
/// that the user has granted for this app.
///
/// # Arguments
/// * `granted_permissions` - Comma-separated string of granted permission names
///
/// # Returns
/// The complete Permissions-Policy header value
fn build_permissions_policy(granted_permissions: &str) -> String {
    // Parse granted permissions into a set for fast lookup
    let granted: std::collections::HashSet<&str> = granted_permissions
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    // If no permissions granted, use the static deny-all policy
    if granted.is_empty() {
        return PERMISSIONS_POLICY_DENY_ALL.to_string();
    }

    // Build the policy by modifying the base policy
    // For each grantable permission, if granted, change from () to (self)
    let mut policy = PERMISSIONS_POLICY_DENY_ALL.to_string();

    for (perm_name, directive) in GRANTABLE_PERMISSIONS {
        if granted.contains(*perm_name) {
            // Replace "directive=()" with "directive=(self)"
            let deny_pattern = format!("{}=()", directive);
            let allow_pattern = format!("{}=(self)", directive);
            policy = policy.replace(&deny_pattern, &allow_pattern);
        }
    }

    policy
}

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

    // URI format:
    // macOS/Linux: webxdc://dummy.host/<path>
    // Windows/Android: http://webxdc.localhost/<path>

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
async fn get_user_info() -> (String, String) {
    // Get user's npub from Nostr client
    let user_npub = if let Some(&pk) = crate::MY_PUBLIC_KEY.get() {
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
                    if !profile.nickname.is_empty() {
                        profile.nickname.to_string()
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
fn inject_webxdc_script(html_data: &[u8], user_npub: &str, user_display_name: &str) -> Vec<u8> {
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
fn generate_webxdc_bridge_js(user_npub: &str, user_display_name: &str) -> String {
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
        .header(http::header::CONTENT_SECURITY_POLICY, &*CSP)
        // Ensure that the browser doesn't try to interpret the file incorrectly
        .header(http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        // Dynamic permissions policy based on user grants
        .header("Permissions-Policy", permissions_policy)
        // Cross-origin isolation: enables SharedArrayBuffer and high-resolution timers.
        // WASM-threaded games (Unity, Godot, etc.) need SharedArrayBuffer for multi-threaded
        // rendering; without these headers on Chromium/WebView2 they fall back to single-
        // threaded mode and run extremely slowly.  WKWebView (macOS) and WebKitGTK (Linux)
        // provide SharedArrayBuffer without these headers, so this mainly fixes Windows.
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
        .header(http::header::CONTENT_SECURITY_POLICY, &*CSP)
        .header(http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header("Permissions-Policy", permissions_policy)
        // Cross-origin isolation headers for SharedArrayBuffer (WASM threads)
        .header("Cross-Origin-Opener-Policy", "same-origin")
        .header("Cross-Origin-Embedder-Policy", "require-corp")
        .body(Cow::Owned(message.as_bytes().to_vec()))
        .unwrap()
}

fn get_mime_type(path: &str) -> String {
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