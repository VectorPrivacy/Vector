//! The `webxdc://` scheme: serves a Mini App's files out of its `.xdc` package, each
//! response under a locked-down Content Security Policy.

use std::borrow::Cow;
use tauri::{Manager, UriSchemeContext, UriSchemeResponder};

use nostr_sdk::prelude::ToBech32;
use std::sync::OnceLock;

use super::state::MiniAppsState;
use crate::STATE;

fn csp() -> Cow<'static, str> {
    static CACHED: OnceLock<String> = OnceLock::new();
    cached_csp(&CACHED, false)
}

/// The same policy for apps on the loopback host (`isolated.rs`).
#[cfg(not(target_os = "android"))]
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

#[cfg(any(target_os = "linux", target_os = "android"))]
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

/// Everything loads from the app's own origin, and 'self' alone: each app is served from
/// its own per-partition origin, and naming a fixed host would let it frame another
/// origin's storage. Inline code, eval and WebAssembly stay open because games and their
/// engines depend on them; 'wasm-unsafe-eval' is named apart, as Chromium stopped letting
/// 'unsafe-eval' cover WebAssembly compilation and interpreted WASM runs many times slower.
fn build_csp(isolated: bool) -> String {
    const LOCAL: &str = "'self' data: blob:";
    let mut connect = String::from("'self' ipc: data: blob:");
    if let Some(port) = RT_WS_PORT.get() {
        connect += &format!(" ws://127.0.0.1:{port}");
    }
    // WebView2's IPC endpoint; without it Tauri falls back to postMessage.
    if isolated && cfg!(windows) {
        connect += " http://ipc.localhost";
    }
    let mut policy = format!(
        "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval' 'wasm-unsafe-eval' blob:; \
         style-src 'self' 'unsafe-inline' blob:; img-src {LOCAL}; media-src {LOCAL}; font-src {LOCAL}; \
         connect-src {connect}; webrtc 'block'"
    );
    if isolated {
        policy += "; frame-ancestors 'self'";
    }
    policy
}

use vector_core::webxdc_permissions::build_permissions_policy;

/// Serves `webxdc://<partition>.host/<path>` to Mini App windows only. Windows delivers it
/// as `http://webxdc.<partition>.host/<path>`, which wry maps back. The work runs off the
/// webview's thread: answering inline can deadlock WebView2.
pub fn miniapp_protocol<R: tauri::Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: http::Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let label = ctx.webview_label().to_owned();
    log_trace!("miniapp scheme <- {label}: {}", request.uri());
    if !label.starts_with("miniapp:") {
        log_error!("webxdc:// request from a window that isn't a Mini App: {label}");
        responder.respond(respond(http::StatusCode::FORBIDDEN, "text/plain", b"Access denied".to_vec(), ""));
        return;
    }
    let app = ctx.app_handle().clone();
    tauri::async_runtime::spawn(async move {
        responder.respond(handle_miniapp_request(&app, &label, &request).await);
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
            return respond(http::StatusCode::NOT_FOUND, "text/plain", b"Mini App not found".to_vec(), "");
        }
    };

    let path = request.uri().path();

    // Handle special paths - serve webxdc.js bridge script
    if path == "/webxdc.js" {
        // Get user's npub and display name for selfAddr and selfName
        let (user_npub, user_display_name) = get_user_info().await;
        return serve_webxdc_js(&user_npub, &user_display_name);
    }

    let file = if path == "/" || path.is_empty() { "index.html".to_string() } else { path.trim_start_matches('/').to_string() };
    // Blocking file I/O: a large asset would hold a runtime worker while it inflates.
    let reading = instance.clone();
    let loaded = tokio::task::spawn_blocking(move || {
        let archive = reading.archive().ok()?;
        match archive.read(&file) {
            Ok(data) => Some((file, data)),
            Err(_) => {
                let html = format!("{file}.html");
                archive.read(&html).ok().map(|data| (html, data))
            }
        }
    })
    .await
    .ok()
    .flatten();
    let Some((served, data)) = loaded else {
        return respond(http::StatusCode::NOT_FOUND, "text/plain", b"File not found".to_vec(), "");
    };

    let mime_type = get_mime_type(&served);
    if mime_type == "text/html" {
        // A Permissions Policy binds to the document, so grants are read once per page load.
        let granted = crate::db::get_miniapp_granted_permissions(&instance.package.hash_hex()).unwrap_or_default();
        let (user_npub, user_display_name) = get_user_info().await;
        let injected = inject_webxdc_script(&data, &user_npub, &user_display_name);
        return respond(http::StatusCode::OK, &mime_type, injected, &granted);
    }
    respond(http::StatusCode::OK, &mime_type, data, "")
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
                    } else if !profile.name().is_empty() {
                        profile.name().to_string()
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
fn serve_webxdc_js(user_npub: &str, user_display_name: &str) -> http::Response<Cow<'static, [u8]>> {
    let js = generate_webxdc_bridge_js(user_npub, user_display_name);
    respond(http::StatusCode::OK, "text/javascript", js.into_bytes(), "")
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
                window.__TAURI__.core.invoke('miniapp_call', {{
                    call: {{ op: 'get_updates', last_known_serial: lastKnownSerial }}
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
                    window.__TAURI__.core.invoke('miniapp_call', {{
                        call: {{ op: 'send_update', update: update, description: description || '' }}
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
                            window.__TAURI__.core.invoke('miniapp_call', {{
                                call: {{ op: 'send_realtime_data', data: Array.from(buf) }}
                            }});
                        }});
                    }}
                }},

                leave: function() {{
                    realtimeListener = null;
                    realtimeChannel = null;
                    if (rtWs) {{ try {{ rtWs.close(); }} catch(e) {{}} rtWs = null; }}
                    waitForTauri(function() {{
                        window.__TAURI__.core.invoke('miniapp_call', {{ call: {{ op: 'leave_realtime_channel' }} }}).catch(function(err) {{
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
                window.__TAURI__.core.invoke('miniapp_call', {{
                    call: {{ op: 'join_realtime_channel' }},
                    channel: tauriChannel
                }}).then(function(result) {{
                    // Open WebSocket fast-path if backend returned a URL
                    if (result && result.ws_url) {{
                        var wsUrl = result.ws_url;
                        var wsRetried = false;
                        function connectWs() {{
                            try {{
                                rtWs = new WebSocket(wsUrl);
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

/// Every response carries the full policy, errors included: a page served without one
/// (a missing file opened in an iframe, say) would run unconfined beside its parent.
fn respond(status: http::StatusCode, content_type: &str, body: Vec<u8>, granted_permissions: &str) -> http::Response<Cow<'static, [u8]>> {
    http::Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, content_type)
        .header(http::header::CONTENT_SECURITY_POLICY, csp().into_owned())
        .header(http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header("Permissions-Policy", build_permissions_policy(granted_permissions))
        // No custom-scheme page is ever cross-origin isolated (WebKit enforces COOP
        // only on network loads; WebView2 maps the scheme to a non-secure host), so
        // these do not grant SharedArrayBuffer here. Apps that need it opt in to the
        // loopback host in `isolated.rs`.
        .header("Cross-Origin-Opener-Policy", "same-origin")
        .header("Cross-Origin-Embedder-Policy", "require-corp")
        .body(Cow::Owned(body))
        .expect("Mini App response headers are built from fixed values")
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
        // Never a PDF: an engine's built-in viewer runs outside the page's CSP.
        "pdf" => "application/octet-stream",
        _ => "application/octet-stream",
    }.to_string()
}
#[cfg(test)]
mod tests {
    /// Grants and the bridge go to responses typed exactly "text/html", and only to those.
    #[test]
    fn only_pages_are_served_as_documents() {
        for page in ["index.html", "Game.HTM", "levels/one.html"] {
            assert_eq!(super::get_mime_type(page), "text/html", "{page}");
        }
        for asset in ["app.js", "icon.svg", "data.xml", "level.json", "game"] {
            assert_ne!(super::get_mime_type(asset), "text/html", "{asset}");
        }
    }
}
