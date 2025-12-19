//! Custom URI scheme handler for Mini Apps
//!
//! This provides the `webxdc://` protocol that serves content from .xdc packages
//! in an isolated context with strict CSP.

use std::borrow::Cow;
use std::collections::HashMap;
use tauri::{
    utils::config::{Csp, CspDirectiveSources},
    Manager, UriSchemeContext,
};
use log::{error, trace};
use nostr_sdk::prelude::ToBech32;
use once_cell::sync::Lazy;

use super::state::MiniAppsState;
use crate::{NOSTR_CLIENT, STATE};

/// Content Security Policy for Mini Apps - very restrictive for security
/// Based on DeltaChat's implementation
static CSP: Lazy<String> = Lazy::new(|| {
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
    
    // Allow inline scripts and eval (needed for many web apps)
    m.insert(
        "script-src".to_string(),
        CspDirectiveSources::List(vec![
            "'self'".to_owned(),
            "'unsafe-inline'".to_owned(),
            "'unsafe-eval'".to_owned(),
            "blob:".to_owned(),
        ]),
    );
    
    // Restrict connections to self, IPC, and data/blob URLs only
    m.insert(
        "connect-src".to_string(),
        CspDirectiveSources::List(vec![
            "'self'".to_owned(),
            "ipc:".to_owned(),
            "data:".to_owned(),
            "blob:".to_owned(),
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

/// Permissions Policy to deny ALL sensitive APIs
/// This is a comprehensive list from DeltaChat based on W3C spec
/// https://github.com/w3c/webappsec-permissions-policy/blob/main/features.md
const PERMISSIONS_POLICY_DENY_ALL: &str = concat!(
    "accelerometer=(), ",
    "ambient-light-sensor=(), ",
    "attribution-reporting=(), ",
    "autoplay=(), ",
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
    "gamepad=(), ",
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

/// Handle requests to the webxdc:// protocol (synchronous version for Tauri 2)
pub fn miniapp_protocol<R: tauri::Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: http::Request<Vec<u8>>,
) -> http::Response<Cow<'static, [u8]>> {
    trace!(
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
        error!(
            "Prevented non-miniapp window from accessing webxdc:// scheme (webview label: {webview_label})"
        );
        return make_error_response(http::StatusCode::FORBIDDEN, "Access denied");
    }

    let app_handle = ctx.app_handle().clone();
    
    // Handle the request synchronously using block_on
    // This is necessary because the protocol handler must be synchronous
    tauri::async_runtime::block_on(async move {
        handle_miniapp_request(&app_handle, &webview_label, &request).await
    })
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
            error!("Mini App instance not found for window: {window_label}");
            return make_error_response(http::StatusCode::NOT_FOUND, "Mini App not found");
        }
    };

    let path = request.uri().path();
    
    // Handle special paths - serve webxdc.js bridge script
    if path == "/webxdc.js" {
        // Get user's npub and display name for selfAddr and selfName
        let (user_npub, user_display_name) = get_user_info().await;
        return serve_webxdc_js(&instance, &user_npub, &user_display_name);
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
            make_success_response(data, &mime_type)
        }
        Err(_) => {
            // Try with .html extension
            let html_path = format!("{}.html", file_path);
            match instance.package.get_file(&html_path) {
                Ok(data) => make_success_response(data, "text/html"),
                Err(_) => make_error_response(http::StatusCode::NOT_FOUND, "File not found"),
            }
        }
    }
}

/// Get the current user's npub and display name
async fn get_user_info() -> (String, String) {
    // Get user's npub from Nostr client
    let user_npub = if let Some(client) = NOSTR_CLIENT.get() {
        if let Ok(signer) = client.signer().await {
            if let Ok(pubkey) = signer.get_public_key().await {
                pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string())
            } else {
                "unknown".to_string()
            }
        } else {
            "unknown".to_string()
        }
    } else {
        "unknown".to_string()
    };
    
    // Get user's display name from their profile in STATE
    let user_display_name = {
        let state = STATE.lock().await;
        // Find the user's own profile (where mine == true)
        if let Some(profile) = state.profiles.iter().find(|p| p.mine) {
            if !profile.nickname.is_empty() {
                profile.nickname.clone()
            } else if !profile.name.is_empty() {
                profile.name.clone()
            } else {
                // Fallback to npub if no name set
                user_npub.clone()
            }
        } else {
            // No profile found, use npub
            user_npub.clone()
        }
    };
    
    (user_npub, user_display_name)
}

/// Serve the webxdc.js bridge script
fn serve_webxdc_js(
    _instance: &super::state::MiniAppInstance,
    user_npub: &str,
    user_display_name: &str,
) -> http::Response<Cow<'static, [u8]>> {
    let js = format!(r#"
// Mini App Bridge for Vector
// This provides the webxdc-compatible API for Mini Apps

(function() {{
    'use strict';
    
    const selfAddr = {self_addr};
    const selfName = {self_name};
    
    // State tracking
    let updateListener = null;
    let lastKnownSerial = 0;
    
    // The Mini App API
    window.webxdc = {{
        // Get self info
        selfAddr: selfAddr,
        selfName: selfName,
        
        // Set the update listener
        setUpdateListener: function(listener, serial) {{
            updateListener = listener;
            lastKnownSerial = serial || 0;
            
            // Request updates since last known serial
            window.__TAURI__.core.invoke('miniapp_get_updates', {{
                lastKnownSerial: lastKnownSerial
            }}).then(function(updates) {{
                if (updates && updateListener) {{
                    const parsed = JSON.parse(updates);
                    parsed.forEach(function(update) {{
                        updateListener(update);
                    }});
                }}
            }}).catch(function(err) {{
                console.error('Failed to get updates:', err);
            }});
            
            return Promise.resolve();
        }},
        
        // Send an update
        sendUpdate: function(update, description) {{
            return window.__TAURI__.core.invoke('miniapp_send_update', {{
                update: update,
                description: description || ''
            }});
        }},
        
        // Send a file to chat (not implemented in basic version)
        sendToChat: function(content) {{
            console.warn('sendToChat is not yet implemented');
            return Promise.reject(new Error('Not implemented'));
        }},
        
        // Import files (not implemented in basic version)
        importFiles: function(filters) {{
            console.warn('importFiles is not yet implemented');
            return Promise.reject(new Error('Not implemented'));
        }},
        
        // Join a realtime channel (for multiplayer games)
        // Based on DeltaChat's implementation for cross-compatibility
        joinRealtimeChannel: function() {{
            // Check if already joined
            if (window.__webxdc_realtime_channel && !window.__webxdc_realtime_channel._trashed) {{
                throw new Error('realtime listener already exists');
            }}
            
            // Create a Tauri channel for receiving realtime events
            const Channel = window.__TAURI__.core.Channel;
            const eventChannel = new Channel();
            
            // Create the realtime channel object
            const channel = {{
                _listener: null,
                _trashed: false,
                _joined: false,
                _eventChannel: eventChannel,
                
                setListener: function(listener) {{
                    if (this._trashed) {{
                        throw new Error('realtime listener is trashed and can no longer be used');
                    }}
                    this._listener = listener;
                }},
                
                send: function(data) {{
                    if (!(data instanceof Uint8Array)) {{
                        throw new Error('realtime listener data must be a Uint8Array');
                    }}
                    if (this._trashed) {{
                        throw new Error('realtime listener is trashed and can no longer be used');
                    }}
                    if (data.length > 128000) {{
                        throw new Error('realtime data exceeds maximum size of 128000 bytes');
                    }}
                    
                    // Convert Uint8Array to regular array for Tauri
                    const dataArray = Array.from(data);
                    window.__TAURI__.core.invoke('miniapp_send_realtime_data', {{
                        data: dataArray
                    }}).catch(function(err) {{
                        console.error('Failed to send realtime data:', err);
                    }});
                }},
                
                leave: function() {{
                    if (this._trashed) return;
                    this._trashed = true;
                    this._listener = null;
                    
                    window.__TAURI__.core.invoke('miniapp_leave_realtime_channel')
                        .catch(function(err) {{
                            console.error('Failed to leave realtime channel:', err);
                        }});
                    
                    window.__webxdc_realtime_channel = null;
                }}
            }};
            
            // Set up event handler
            eventChannel.onmessage = function(message) {{
                if (channel._trashed || !channel._listener) return;
                
                if (message.event === 'data' && message.data) {{
                    // Convert array back to Uint8Array
                    const data = new Uint8Array(message.data);
                    channel._listener(data);
                }} else if (message.event === 'connected') {{
                    console.log('Realtime channel connected');
                }} else if (message.event === 'peerJoined') {{
                    console.log('Peer joined:', message.data);
                }} else if (message.event === 'peerLeft') {{
                    console.log('Peer left:', message.data);
                }}
            }};
            
            // Store reference
            window.__webxdc_realtime_channel = channel;
            
            // Join the channel on the backend (pass the event channel)
            window.__TAURI__.core.invoke('miniapp_join_realtime_channel', {{
                channel: eventChannel
            }})
                .then(function(topicId) {{
                    channel._joined = true;
                    console.log('Joined realtime channel with topic:', topicId);
                }})
                .catch(function(err) {{
                    console.error('Failed to join realtime channel:', err);
                    channel._trashed = true;
                }});
            
            return channel;
        }}
    }};
    
    // Listen for updates from the backend
    window.__TAURI__.event.listen('miniapp_update', function(event) {{
        if (updateListener && event.payload) {{
            updateListener(event.payload);
        }}
    }});
    
    console.log('Mini App bridge initialized');
}})();
"#,
        self_addr = serde_json::to_string(user_npub).unwrap_or_else(|_| "\"unknown\"".to_string()),
        self_name = serde_json::to_string(user_display_name).unwrap_or_else(|_| "\"Unknown\"".to_string()),
    );
    
    make_success_response(js.into_bytes(), "text/javascript")
}

fn make_success_response(body: Vec<u8>, content_type: &str) -> http::Response<Cow<'static, [u8]>> {
    http::Response::builder()
        .status(http::StatusCode::OK)
        .header(http::header::CONTENT_TYPE, content_type)
        .header(http::header::CONTENT_SECURITY_POLICY, CSP.as_str())
        // Ensure that the browser doesn't try to interpret the file incorrectly
        .header(http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        // Deny all permissions - comprehensive list from DeltaChat
        .header("Permissions-Policy", PERMISSIONS_POLICY_DENY_ALL)
        .body(Cow::Owned(body))
        .unwrap_or_else(|_| make_error_response(http::StatusCode::INTERNAL_SERVER_ERROR, "Failed to build response"))
}

fn make_error_response(status: http::StatusCode, message: &str) -> http::Response<Cow<'static, [u8]>> {
    // IMPORTANT: Set CSP on ALL responses including errors
    // Failing to set CSP might result in the app being able to create
    // an <iframe> with no CSP, e.g. `<iframe src="/no_such_file.lol">`
    // within which they can then do whatever through the parent frame
    // See: "XDC-01-002 WP1: Full CSP bypass via desktop app webxdc.js"
    // https://public.opentech.fund/documents/XDC-01-report_2_1.pdf
    http::Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "text/plain")
        .header(http::header::CONTENT_SECURITY_POLICY, CSP.as_str())
        .header(http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header("Permissions-Policy", PERMISSIONS_POLICY_DENY_ALL)
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