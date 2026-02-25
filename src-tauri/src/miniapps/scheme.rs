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
fn serve_webxdc_js(
    _instance: &super::state::MiniAppInstance,
    user_npub: &str,
    user_display_name: &str,
    granted_permissions: &str,
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
            console.log('joinRealtimeChannel called');
            
            // Check if already joined
            if (window.__webxdc_realtime_channel && !window.__webxdc_realtime_channel._trashed) {{
                console.error('Realtime channel already exists');
                throw new Error('realtime listener already exists');
            }}
            
            // Check if Tauri is available - it should be since we're in a Tauri webview
            if (!window.__TAURI__) {{
                console.error('Tauri API not available - window.__TAURI__ is undefined');
                throw new Error('Tauri API not available');
            }}
            if (!window.__TAURI__.core) {{
                console.error('Tauri core API not available - window.__TAURI__.core is undefined');
                throw new Error('Tauri core API not available');
            }}
            
            // Create a Tauri channel for receiving realtime events
            const Channel = window.__TAURI__.core.Channel;
            if (!Channel) {{
                console.error('Tauri Channel class not available - window.__TAURI__.core.Channel is undefined');
                console.log('Available in window.__TAURI__.core:', Object.keys(window.__TAURI__.core));
                throw new Error('Tauri Channel class not available');
            }}
            console.log('Creating Tauri channel for realtime events');
            let eventChannel;
            try {{
                eventChannel = new Channel();
            }} catch (e) {{
                console.error('Failed to create Tauri channel:', e);
                throw e;
            }}
            
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
                if (channel._trashed) return;
                
                if (message.event === 'data' && message.data) {{
                    // Convert array back to Uint8Array
                    const data = new Uint8Array(message.data);
                    if (channel._listener) {{
                        channel._listener(data);
                    }} else {{
                        console.warn('No listener set for realtime data');
                    }}
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
            console.log('Attempting to join realtime channel...');
            window.__TAURI__.core.invoke('miniapp_join_realtime_channel', {{
                channel: eventChannel
            }})
                .then(function(topicId) {{
                    channel._joined = true;
                    console.log('Successfully joined realtime channel with topic:', topicId);
                }})
                .catch(function(err) {{
                    console.error('Failed to join realtime channel:', err);
                    channel._trashed = true;
                    window.__webxdc_realtime_channel = null;
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
    let webxdc_script = generate_inline_webxdc_script(user_npub, user_display_name);
    
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

/// Generate the inline webxdc script content (without the <script> tags)
fn generate_inline_webxdc_script(user_npub: &str, user_display_name: &str) -> String {
    format!(r#"
// Mini App Bridge for Vector (injected)
(function() {{
    'use strict';

    const selfAddr = {self_addr};
    const selfName = {self_name};

    // State tracking
    let updateListener = null;
    let lastKnownSerial = 0;
    let realtimeChannel = null;
    let realtimeListener = null;
    let tauriChannel = null;

    // Helper to wait for Tauri
    function waitForTauri(callback) {{
        if (window.__TAURI__ && window.__TAURI__.core) {{
            callback();
        }} else {{
            setTimeout(function() {{ waitForTauri(callback); }}, 50);
        }}
    }}
    
    // The Mini App API
    window.webxdc = {{
        // Get self info (immediately available, not a Promise)
        selfAddr: selfAddr,
        selfName: selfName,
        
        // Set the update listener
        setUpdateListener: function(listener, serial) {{
            updateListener = listener;
            lastKnownSerial = serial || 0;
            
            // Wait for Tauri to be ready, then request updates
            waitForTauri(function() {{
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
            }});
            
            return Promise.resolve();
        }},
        
        // Send an update
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
        
        // Send a file to chat (not implemented)
        sendToChat: function(content) {{
            console.warn('sendToChat is not yet implemented');
            return Promise.reject(new Error('Not implemented'));
        }},
        
        // Import files (not implemented)
        importFiles: function(filters) {{
            console.warn('importFiles is not yet implemented');
            return Promise.reject(new Error('Not implemented'));
        }},
        
        // Join a realtime channel - returns a SYNCHRONOUS channel object
        // Per WebXDC spec: https://webxdc.org/docs/spec/joinRealtimeChannel.html
        joinRealtimeChannel: function() {{
            console.log('[webxdc] joinRealtimeChannel called');
            
            // Check if already joined
            if (realtimeChannel !== null) {{
                throw new Error('Already joined realtime channel. Call leave() first.');
            }}
            
            // Create the channel object immediately (synchronous return)
            realtimeChannel = {{
                // Set the listener for incoming data
                setListener: function(listener) {{
                    console.log('[webxdc] realtimeChannel.setListener called');
                    realtimeListener = listener;
                }},
                
                // Send data to connected peers
                send: function(data) {{
                    if (realtimeChannel === null) {{
                        console.warn('[webxdc] Cannot send: channel has been left');
                        return;
                    }}
                    const arr = data instanceof Uint8Array ? Array.from(data) : Array.from(new Uint8Array(data));
                    waitForTauri(function() {{
                        window.__TAURI__.core.invoke('miniapp_send_realtime_data', {{
                            data: arr
                        }}).catch(function(err) {{
                            console.error('[webxdc] Failed to send realtime data:', err);
                        }});
                    }});
                }},
                
                // Leave the channel
                leave: function() {{
                    console.log('[webxdc] realtimeChannel.leave called');
                    realtimeListener = null;
                    realtimeChannel = null;
                    waitForTauri(function() {{
                        window.__TAURI__.core.invoke('miniapp_leave_realtime_channel', {{}}).catch(function(err) {{
                            console.error('[webxdc] Failed to leave realtime channel:', err);
                        }});
                    }});
                }}
            }};
            
            // Start the Tauri channel connection in the background
            waitForTauri(function() {{
                tauriChannel = new window.__TAURI__.core.Channel();
                tauriChannel.onmessage = function(event) {{
                    if (realtimeListener && event && event.data) {{
                        // Convert to Uint8Array and call the listener
                        const data = new Uint8Array(event.data);
                        realtimeListener(data);
                    }}
                }};
                
                window.__TAURI__.core.invoke('miniapp_join_realtime_channel', {{
                    channel: tauriChannel
                }}).then(function(topicId) {{
                    console.log('[webxdc] Joined realtime channel, topic:', topicId);
                }}).catch(function(err) {{
                    console.error('[webxdc] Failed to join realtime channel:', err);
                }});
            }});
            
            // Return the channel object immediately (synchronous)
            return realtimeChannel;
        }}
    }};
    
    console.log('[webxdc] Mini App bridge initialized');
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
        .header(http::header::CONTENT_SECURITY_POLICY, CSP.as_str())
        // Ensure that the browser doesn't try to interpret the file incorrectly
        .header(http::header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        // Dynamic permissions policy based on user grants
        .header("Permissions-Policy", permissions_policy)
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
        .header(http::header::CONTENT_SECURITY_POLICY, CSP.as_str())
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