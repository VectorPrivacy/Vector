//! Tauri commands for Mini Apps

use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri::ipc::Channel;

#[cfg(not(target_os = "android"))]
use std::sync::Arc;
#[cfg(not(target_os = "android"))]
use tauri::{WebviewUrl, WebviewWindowBuilder};
use serde::{Deserialize, Serialize};

use nostr_sdk::prelude::ToBech32;
use super::error::Error;
use super::state::{MiniAppInstance, MiniAppsState, MiniAppPackage, RealtimeChannelState};
use super::realtime::{RealtimeEvent, EventTarget, encode_topic_id, encode_node_addr};
use crate::util::bytes_to_hex_string;

// Network isolation proxy - only used on Linux (not macOS due to version requirements, not Windows due to WebView2 freeze, not Android)
#[cfg(all(not(target_os = "macos"), not(target_os = "windows"), not(target_os = "android")))]
use super::network_isolation::DUMMY_LOCALHOST_PROXY_URL;

/// Information about a Mini App for the frontend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiniAppInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub has_icon: bool,
    /// Base64-encoded icon data URL (e.g., "data:image/png;base64,...")
    pub icon_data: Option<String>,
    /// Optional source code URL from manifest
    pub source_code_url: Option<String>,
    /// SHA-256 hash of the .xdc file (used for permission identification)
    pub file_hash: Option<String>,
}

impl MiniAppInfo {
    pub fn from_package(pkg: &super::state::MiniAppPackage) -> Self {
        let icon_data = pkg.get_icon().map(|bytes| {
            let mime = crate::util::mime_from_magic_bytes(&bytes);
            crate::util::data_uri(mime, &bytes)
        });
        
        Self {
            id: pkg.id.clone(),
            name: pkg.manifest.name.clone(),
            description: pkg.manifest.description.clone(),
            version: pkg.manifest.version.clone(),
            has_icon: icon_data.is_some(),
            icon_data,
            source_code_url: pkg.manifest.source_code_url.clone(),
            file_hash: Some(pkg.file_hash.clone()),
        }
    }
}

/// Initialization script - runs in all frames
/// Based on DeltaChat's implementation
#[allow(dead_code)] // Used on desktop only
const INIT_SCRIPT: &str = r#"
// Mini App initialization script
// This runs in all frames to ensure security

// ============================================================================
// WebGL ANGLE performance shims (Windows)
//
// Every WebGL call that *reads* state back from the GPU forces a synchronous
// pipeline flush through ANGLE's OpenGL→D3D11 translation layer.  On macOS
// (Metal) and Linux (native GL) these round-trips are cheap; on Windows/ANGLE
// they are devastating — a single getError() per draw call drops a 60fps game
// to ~3fps.
//
// Strategy:
//   • getError()              → stub to NO_ERROR (0)
//   • getParameter()          → cache by GLenum (caps/limits never change)
//   • getUniformLocation()    → cache by (program, name) — stable after link
//   • getAttribLocation()     → cache by (program, name) — stable after link
//   • getSupportedExtensions()→ cache once per context
//   • getExtension()          → cache by name
//   • getShaderPrecisionFormat() → cache by (shaderType, precisionType)
// ============================================================================
(function() {
    var NO_ERROR = 0;

    function shimProto(proto) {
        // --- getError: always NO_ERROR ---
        proto.getError = function() { return NO_ERROR; };

        // --- getParameter: cache by GLenum ---
        var realGetParam = proto.getParameter;
        proto.getParameter = function(pname) {
            var c = this.__gpC || (this.__gpC = {});
            if (pname in c) return c[pname];
            return (c[pname] = realGetParam.call(this, pname));
        };

        // --- getUniformLocation: cache by (program, name) ---
        // Result never changes after program linking.  Uses a numeric ID stamped
        // on the program object as map key since WebGLProgram is opaque.
        var realGetUniLoc = proto.getUniformLocation;
        var progId = 0;
        proto.getUniformLocation = function(program, name) {
            if (!program) return realGetUniLoc.call(this, program, name);
            var id = program.__pid;
            if (id === undefined) { id = program.__pid = ++progId; }
            var c = this.__ulC || (this.__ulC = {});
            var key = id + '/' + name;
            if (key in c) return c[key];
            return (c[key] = realGetUniLoc.call(this, program, name));
        };

        // --- getAttribLocation: cache by (program, name) ---
        var realGetAttrLoc = proto.getAttribLocation;
        proto.getAttribLocation = function(program, name) {
            if (!program) return realGetAttrLoc.call(this, program, name);
            var id = program.__pid;
            if (id === undefined) { id = program.__pid = ++progId; }
            var c = this.__alC || (this.__alC = {});
            var key = id + '/' + name;
            if (key in c) return c[key];
            return (c[key] = realGetAttrLoc.call(this, program, name));
        };

        // --- getSupportedExtensions: cache once ---
        var realGetSupExt = proto.getSupportedExtensions;
        proto.getSupportedExtensions = function() {
            var c = this.__seC;
            if (c !== undefined) return c;
            return (this.__seC = realGetSupExt.call(this));
        };

        // --- getExtension: cache by name ---
        var realGetExt = proto.getExtension;
        proto.getExtension = function(name) {
            var c = this.__exC || (this.__exC = {});
            if (name in c) return c[name];
            return (c[name] = realGetExt.call(this, name));
        };

        // --- getShaderPrecisionFormat: cache by (shaderType, precisionType) ---
        var realGetSPF = proto.getShaderPrecisionFormat;
        proto.getShaderPrecisionFormat = function(shaderType, precisionType) {
            var c = this.__spfC || (this.__spfC = {});
            var key = shaderType + '/' + precisionType;
            if (key in c) return c[key];
            return (c[key] = realGetSPF.call(this, shaderType, precisionType));
        };
    }

    try {
        if (typeof WebGLRenderingContext !== 'undefined') {
            shimProto(WebGLRenderingContext.prototype);
        }
        if (typeof WebGL2RenderingContext !== 'undefined') {
            shimProto(WebGL2RenderingContext.prototype);
        }
    } catch (e) {}
})();

// Disable WebRTC to prevent IP leaks
try {
    window.RTCPeerConnection = () => {};
    RTCPeerConnection = () => {};
} catch (e) {
    console.error("Failed to disable RTCPeerConnection:", e);
}
try {
    window.webkitRTCPeerConnection = () => {};
    webkitRTCPeerConnection = () => {};
} catch (e) {}

// ============================================================================
// Media API Permission Guards
// WebKit/WKWebView ignores Permissions-Policy headers, so we must enforce
// permissions at the JavaScript level by wrapping getUserMedia/getDisplayMedia
// ============================================================================
(function() {
    'use strict';

    // Store original APIs before any app code can access them
    const originalGetUserMedia = navigator.mediaDevices?.getUserMedia?.bind(navigator.mediaDevices);
    const originalGetDisplayMedia = navigator.mediaDevices?.getDisplayMedia?.bind(navigator.mediaDevices);
    const originalEnumerateDevices = navigator.mediaDevices?.enumerateDevices?.bind(navigator.mediaDevices);
    const originalGeolocation = navigator.geolocation;
    const originalGetCurrentPosition = navigator.geolocation?.getCurrentPosition?.bind(navigator.geolocation);
    const originalWatchPosition = navigator.geolocation?.watchPosition?.bind(navigator.geolocation);
    const originalClipboardReadText = navigator.clipboard?.readText?.bind(navigator.clipboard);
    const originalClipboardWriteText = navigator.clipboard?.writeText?.bind(navigator.clipboard);
    const originalClipboardRead = navigator.clipboard?.read?.bind(navigator.clipboard);
    const originalClipboardWrite = navigator.clipboard?.write?.bind(navigator.clipboard);

    // Permission cache to avoid repeated Tauri calls
    let permissionCache = null;
    let permissionCacheTime = 0;
    const CACHE_TTL = 5000; // 5 seconds

    // Helper to check permission via Tauri
    async function checkPermission(permissionName) {
        // Wait for Tauri to be ready
        const waitForTauri = () => new Promise((resolve) => {
            const check = () => {
                if (window.__TAURI__?.core?.invoke) {
                    resolve();
                } else {
                    setTimeout(check, 10);
                }
            };
            check();
        });

        await waitForTauri();

        // Use cached permissions if fresh
        const now = Date.now();
        if (permissionCache && (now - permissionCacheTime) < CACHE_TTL) {
            return permissionCache.includes(permissionName);
        }

        try {
            // Get granted permissions from backend
            const granted = await window.__TAURI__.core.invoke('miniapp_get_granted_permissions_for_window');
            permissionCache = granted ? granted.split(',').map(p => p.trim()) : [];
            permissionCacheTime = now;
            return permissionCache.includes(permissionName);
        } catch (e) {
            console.warn('[MiniApp] Failed to check permission:', e);
            return false;
        }
    }

    // Create a NotAllowedError like browsers do
    function createNotAllowedError(message) {
        const error = new DOMException(message, 'NotAllowedError');
        return error;
    }

    // Wrap getUserMedia
    if (navigator.mediaDevices && originalGetUserMedia) {
        navigator.mediaDevices.getUserMedia = async function(constraints) {
            const needsMic = constraints?.audio;
            const needsCam = constraints?.video;

            if (needsMic) {
                const allowed = await checkPermission('microphone');
                if (!allowed) {
                    console.warn('[MiniApp] Microphone access denied - permission not granted');
                    throw createNotAllowedError('Microphone permission denied by Vector');
                }
            }

            if (needsCam) {
                const allowed = await checkPermission('camera');
                if (!allowed) {
                    console.warn('[MiniApp] Camera access denied - permission not granted');
                    throw createNotAllowedError('Camera permission denied by Vector');
                }
            }

            // Permission granted, call original
            return originalGetUserMedia(constraints);
        };
    }

    // Wrap getDisplayMedia
    if (navigator.mediaDevices && originalGetDisplayMedia) {
        navigator.mediaDevices.getDisplayMedia = async function(constraints) {
            const allowed = await checkPermission('display-capture');
            if (!allowed) {
                console.warn('[MiniApp] Screen capture denied - permission not granted');
                throw createNotAllowedError('Screen capture permission denied by Vector');
            }
            return originalGetDisplayMedia(constraints);
        };
    }

    // Wrap enumerateDevices to hide devices when no permission
    if (navigator.mediaDevices && originalEnumerateDevices) {
        navigator.mediaDevices.enumerateDevices = async function() {
            const devices = await originalEnumerateDevices();
            const hasMic = await checkPermission('microphone');
            const hasCam = await checkPermission('camera');
            const hasSpeaker = await checkPermission('speaker-selection');

            // Filter devices based on permissions
            return devices.filter(device => {
                if (device.kind === 'audioinput' && !hasMic) return false;
                if (device.kind === 'videoinput' && !hasCam) return false;
                if (device.kind === 'audiooutput' && !hasSpeaker) return false;
                return true;
            }).map(device => {
                // If permission not granted, hide device labels (like browsers do)
                const hasPermission =
                    (device.kind === 'audioinput' && hasMic) ||
                    (device.kind === 'videoinput' && hasCam) ||
                    (device.kind === 'audiooutput' && hasSpeaker);

                if (!hasPermission) {
                    return {
                        deviceId: device.deviceId,
                        kind: device.kind,
                        label: '',
                        groupId: device.groupId
                    };
                }
                return device;
            });
        };
    }

    // Wrap Geolocation API
    if (originalGeolocation && originalGetCurrentPosition) {
        navigator.geolocation.getCurrentPosition = async function(success, error, options) {
            const allowed = await checkPermission('geolocation');
            if (!allowed) {
                console.warn('[MiniApp] Geolocation denied - permission not granted');
                if (error) {
                    error({ code: 1, message: 'Geolocation permission denied by Vector', PERMISSION_DENIED: 1 });
                }
                return;
            }
            return originalGetCurrentPosition(success, error, options);
        };

        navigator.geolocation.watchPosition = async function(success, error, options) {
            const allowed = await checkPermission('geolocation');
            if (!allowed) {
                console.warn('[MiniApp] Geolocation watch denied - permission not granted');
                if (error) {
                    error({ code: 1, message: 'Geolocation permission denied by Vector', PERMISSION_DENIED: 1 });
                }
                return 0;
            }
            return originalWatchPosition(success, error, options);
        };
    }

    // Wrap Clipboard API (both text and binary methods)
    if (navigator.clipboard) {
        if (originalClipboardReadText) {
            navigator.clipboard.readText = async function() {
                const allowed = await checkPermission('clipboard-read');
                if (!allowed) {
                    console.warn('[MiniApp] Clipboard read denied - permission not granted');
                    throw createNotAllowedError('Clipboard read permission denied by Vector');
                }
                return originalClipboardReadText();
            };
        }

        if (originalClipboardWriteText) {
            navigator.clipboard.writeText = async function(text) {
                const allowed = await checkPermission('clipboard-write');
                if (!allowed) {
                    console.warn('[MiniApp] Clipboard write denied - permission not granted');
                    throw createNotAllowedError('Clipboard write permission denied by Vector');
                }
                return originalClipboardWriteText(text);
            };
        }

        // Binary clipboard methods (read/write ClipboardItem objects)
        if (originalClipboardRead) {
            navigator.clipboard.read = async function() {
                const allowed = await checkPermission('clipboard-read');
                if (!allowed) {
                    console.warn('[MiniApp] Clipboard read denied - permission not granted');
                    throw createNotAllowedError('Clipboard read permission denied by Vector');
                }
                return originalClipboardRead();
            };
        }

        if (originalClipboardWrite) {
            navigator.clipboard.write = async function(data) {
                const allowed = await checkPermission('clipboard-write');
                if (!allowed) {
                    console.warn('[MiniApp] Clipboard write denied - permission not granted');
                    throw createNotAllowedError('Clipboard write permission denied by Vector');
                }
                return originalClipboardWrite(data);
            };
        }
    }

    // Wrap Bluetooth API
    if (navigator.bluetooth) {
        const originalRequestDevice = navigator.bluetooth.requestDevice?.bind(navigator.bluetooth);
        if (originalRequestDevice) {
            navigator.bluetooth.requestDevice = async function(options) {
                const allowed = await checkPermission('bluetooth');
                if (!allowed) {
                    console.warn('[MiniApp] Bluetooth denied - permission not granted');
                    throw createNotAllowedError('Bluetooth permission denied by Vector');
                }
                return originalRequestDevice(options);
            };
        }
    }

    // Wrap MIDI API
    if (navigator.requestMIDIAccess) {
        const originalRequestMIDI = navigator.requestMIDIAccess.bind(navigator);
        navigator.requestMIDIAccess = async function(options) {
            const allowed = await checkPermission('midi');
            if (!allowed) {
                console.warn('[MiniApp] MIDI access denied - permission not granted');
                throw createNotAllowedError('MIDI permission denied by Vector');
            }
            return originalRequestMIDI(options);
        };
    }

    // Wrap Screen Wake Lock API
    if (navigator.wakeLock) {
        const originalWakeLockRequest = navigator.wakeLock.request?.bind(navigator.wakeLock);
        if (originalWakeLockRequest) {
            navigator.wakeLock.request = async function(type) {
                const allowed = await checkPermission('screen-wake-lock');
                if (!allowed) {
                    console.warn('[MiniApp] Wake lock denied - permission not granted');
                    throw createNotAllowedError('Screen wake lock permission denied by Vector');
                }
                return originalWakeLockRequest(type);
            };
        }
    }

    // Wrap navigator.permissions.query() to return Vector's permission state
    // Many apps check this before calling getUserMedia, so we need to reflect our state
    if (navigator.permissions && navigator.permissions.query) {
        const originalQuery = navigator.permissions.query.bind(navigator.permissions);
        navigator.permissions.query = async function(descriptor) {
            const name = descriptor?.name;

            // Map permission names to our Vector permission names
            const permissionMap = {
                'microphone': 'microphone',
                'camera': 'camera',
                'geolocation': 'geolocation',
                'clipboard-read': 'clipboard-read',
                'clipboard-write': 'clipboard-write',
                'midi': 'midi',
                'screen-wake-lock': 'screen-wake-lock',
                'display-capture': 'display-capture',
                'speaker-selection': 'speaker-selection',
                'accelerometer': 'accelerometer',
                'gyroscope': 'gyroscope',
                'magnetometer': 'magnetometer',
                'ambient-light-sensor': 'ambient-light-sensor',
                'bluetooth': 'bluetooth',
            };

            const vectorPermission = permissionMap[name];
            if (vectorPermission) {
                const allowed = await checkPermission(vectorPermission);
                // Return a PermissionStatus-like object
                // We return 'granted' if allowed, 'prompt' if not (to encourage the app to try)
                // Using 'prompt' instead of 'denied' lets apps attempt the action and get our proper error
                const state = allowed ? 'granted' : 'prompt';
                return {
                    state: state,
                    name: name,
                    onchange: null,
                    addEventListener: () => {},
                    removeEventListener: () => {},
                    dispatchEvent: () => false,
                };
            }

            // For unknown permissions, fall through to original
            return originalQuery(descriptor);
        };
    }

})();

// Wrap Tauri's __TAURI__ API to restrict access to only allowed commands
// Uses property interception to ensure ZERO timing window for bypass
(function() {
    'use strict';

    const allowedCommands = [
        'miniapp_get_updates',
        'miniapp_send_update',
        'miniapp_join_realtime_channel',
        'miniapp_leave_realtime_channel',
        'miniapp_send_realtime_data',
        'miniapp_add_realtime_peer',
        'miniapp_get_realtime_node_addr',
        'miniapp_get_granted_permissions_for_window'
    ];

    function wrapTauriApi(tauriObj) {
        if (!tauriObj || !tauriObj.core) return tauriObj;

        const originalCore = tauriObj.core;
        const originalInvoke = originalCore.invoke;

        // Build a plain wrapper object — avoids Proxy invariant violations
        // on frozen/non-configurable properties
        const wrappedCore = {};
        for (const key of Object.getOwnPropertyNames(originalCore)) {
            if (key === 'invoke') continue;
            try {
                Object.defineProperty(wrappedCore, key, {
                    get() { return originalCore[key]; },
                    configurable: true,
                    enumerable: true
                });
            } catch(_) {
                wrappedCore[key] = originalCore[key];
            }
        }
        // Copy prototype methods (Channel, etc.)
        const proto = Object.getPrototypeOf(originalCore);
        if (proto && proto !== Object.prototype) {
            for (const key of Object.getOwnPropertyNames(proto)) {
                if (key === 'constructor' || key === 'invoke' || key in wrappedCore) continue;
                try {
                    Object.defineProperty(wrappedCore, key, {
                        get() { return originalCore[key]; },
                        configurable: true,
                        enumerable: true
                    });
                } catch(_) {}
            }
        }

        wrappedCore.invoke = async (cmd, args) => {
            if (allowedCommands.includes(cmd)) {
                return originalInvoke.call(originalCore, cmd, args);
            }
            console.warn('Mini App tried to invoke blocked Tauri command:', cmd);
            throw new Error('Tauri command not available in Mini Apps: ' + cmd);
        };

        // Build a plain wrapper for the top-level __TAURI__ object
        const wrapped = {};
        for (const key of Object.getOwnPropertyNames(tauriObj)) {
            if (key === 'core') continue;
            try {
                Object.defineProperty(wrapped, key, {
                    get() { return tauriObj[key]; },
                    configurable: true,
                    enumerable: true
                });
            } catch(_) {
                wrapped[key] = tauriObj[key];
            }
        }
        wrapped.core = wrappedCore;

        return wrapped;
    }

    // Intercept any assignment to __TAURI__ (zero timing window)
    let _tauriValue = window.__TAURI__ ? wrapTauriApi(window.__TAURI__) : undefined;
    Object.defineProperty(window, '__TAURI__', {
        get() {
            return _tauriValue;
        },
        set(newValue) {
            _tauriValue = wrapTauriApi(newValue);
        },
        configurable: false,  // Prevent re-definition
        enumerable: true
    });
})();
"#;

/// Get the base URL for Mini Apps based on platform
#[allow(dead_code)] // Used on desktop only
fn get_miniapp_base_url() -> Result<tauri::Url, Error> {
    // URI format:
    // mac/linux:         webxdc://dummy.host/<path>
    // windows/android:   http://webxdc.localhost/<path>
    #[cfg(any(target_os = "windows", target_os = "android"))]
    {
        "http://webxdc.localhost/"
            .parse()
            .map_err(|e: url::ParseError| Error::Anyhow(e.into()))
    }
    #[cfg(not(any(target_os = "windows", target_os = "android")))]
    {
        "webxdc://dummy.host/"
            .parse()
            .map_err(|e: url::ParseError| Error::Anyhow(e.into()))
    }
}

/// Get Chromium hardening browser args for Windows
/// This disables WebRTC, blocks DNS queries, and sets up the dummy proxy
// Note: Chromium hardening browser args were removed for Windows because they cause WebView2 to freeze.
// The CSP (Content Security Policy) provides the primary security layer for mini apps.
// See: https://delta.chat/en/2023-05-22-webxdc-security for background on webxdc security.

/// Load Mini App info from a file path
#[tauri::command]
pub async fn miniapp_load_info(
    app: AppHandle,
    file_path: String,
) -> Result<MiniAppInfo, Error> {
    // Guard: empty paths
    if file_path.is_empty() {
        return Err(Error::InvalidPackage("Empty file path".to_string()));
    }

    // 10-second timeout so this command can NEVER hang forever
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        // Check file existence on blocking thread (avoid sync I/O on async runtime)
        let path = PathBuf::from(&file_path);
        let path_check = path.clone();
        let exists = tokio::task::spawn_blocking(move || path_check.exists())
            .await
            .unwrap_or(false);
        if !exists {
            return Err(Error::InvalidPackage(format!("File not found: {}", file_path)));
        }

        // Generate ID from file path hash
        let id = format!("miniapp_{:x}", md5_hash(&file_path));

        let state = app.state::<MiniAppsState>();
        let package = state.get_or_load_package(&id, path).await?;

        // Build MiniAppInfo on blocking thread (get_icon() does sync file I/O)
        let info = tokio::task::spawn_blocking(move || {
            MiniAppInfo::from_package(&package)
        }).await.map_err(|e| Error::Anyhow(anyhow::anyhow!("{}", e)))?;

        Ok(info)
    }).await;

    match result {
        Ok(inner) => inner,
        Err(_) => Err(Error::Anyhow(anyhow::anyhow!(
            "miniapp_load_info timed out after 10s for: {}", file_path
        ))),
    }
}

/// Load Mini App info from bytes (in-memory, no file needed)
/// This is more efficient for preview when the file is already cached in memory
#[tauri::command]
pub async fn miniapp_load_info_from_bytes(
    bytes: Vec<u8>,
    file_name: String,
) -> Result<MiniAppInfo, Error> {
    // Extract name without extension for fallback
    let fallback_name = file_name
        .rsplit('.')
        .skip(1)
        .next()
        .unwrap_or(&file_name)
        .to_string();

    // Compute SHA-256 hash of the bytes for permission identification
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let file_hash = bytes_to_hex_string(&hasher.finalize());

    let (manifest, icon_bytes) = MiniAppPackage::load_info_from_bytes(&bytes, &fallback_name)?;

    // Convert icon bytes to base64 data URL
    let icon_data = icon_bytes.map(|bytes| {
        let mime = crate::util::mime_from_magic_bytes(&bytes);
        crate::util::data_uri(mime, &bytes)
    });

    Ok(MiniAppInfo {
        id: format!("miniapp_preview_{}", md5_hash(&file_name)),
        name: manifest.name,
        description: manifest.description,
        version: manifest.version,
        has_icon: icon_data.is_some(),
        icon_data,
        source_code_url: manifest.source_code_url,
        file_hash: Some(file_hash),
    })
}

/// Open a Mini App in a new window
///
/// If `href` is provided (from update.href), it will be appended to the root URL
/// as per WebXDC spec: "the webxdc app MUST be started with the root URL for the
/// webview with the value of update.href appended"
#[tauri::command]
pub async fn miniapp_open(
    app: AppHandle,
    file_path: String,
    chat_id: String,
    message_id: String,
    href: Option<String>,
    topic_id: Option<String>,
) -> Result<(), Error> {
    log_info!("[WEBXDC] miniapp_open called: chat={}, msg={}", chat_id, message_id);
    let path = PathBuf::from(&file_path);

    // Generate unique ID from file hash
    let id = format!("miniapp_{:x}", md5_hash(&file_path));
    // For marketplace apps (empty chat/message), use the app id as the window label
    // This ensures a valid label like "miniapp:solo:abc123" instead of "miniapp::"
    let window_label = if chat_id.is_empty() && message_id.is_empty() {
        format!("miniapp:solo:{}", id)
    } else {
        format!("miniapp:{}:{}", chat_id, message_id)
    };
    
    log_trace!("Opening Mini App: {} ({}, {}) with href: {:?}, topic: {:?}", window_label, chat_id, message_id, href, topic_id);

    let state = app.state::<MiniAppsState>();

    // Check if already open
    log_trace!("[MiniApp] Checking for existing instance...");
    if let Some((existing_label, _existing_instance)) = state.get_instance_by_message(&chat_id, &message_id).await {
        #[cfg(target_os = "android")]
        {
            // On Android, navigate the existing overlay if open
            if crate::android::miniapp::is_miniapp_open().unwrap_or(false) {
                if let Some(ref href_value) = href {
                    let _ = crate::android::miniapp::send_to_miniapp("navigate", href_value);
                }
                return Ok(());
            } else {
                // Overlay was closed but state was never cleaned up.
                // Full Iroh shutdown — next preconnect creates a fresh instance.
                log_warn!("Instance exists but overlay closed, full cleanup: {}", existing_label);

                let channel_state = state.remove_realtime_channel(&existing_label).await;
                if let Some(channel) = channel_state {
                    let topic_encoded = super::realtime::encode_topic_id(&channel.topic);
                    if let Some(my_pk) = crate::MY_PUBLIC_KEY.get() {
                        if let Ok(my_npub) = my_pk.to_bech32() {
                            state.remove_session_peer(&channel.topic, &my_npub).await;
                        }
                    }
                    let chat_id_clone = chat_id.clone();
                    tokio::spawn(async move {
                        crate::commands::realtime::send_webxdc_peer_left(chat_id_clone, topic_encoded).await;
                    });
                }
                // Destroy Iroh completely — guarantees clean state for session 2
                state.realtime.shutdown_iroh().await;
                state.remove_instance(&existing_label).await;
            }
        }

        #[cfg(not(target_os = "android"))]
        {
            // Desktop: Focus existing window
            if let Some(window) = app.get_webview_window(&existing_label) {
                // If href is provided, navigate to it
                if let Some(ref href_value) = href {
                    let mut nav_url = get_miniapp_base_url()?;
                    // Append href to the base URL (href should start with / or be a relative path)
                    let href_path = href_value.trim_start_matches('/');
                    nav_url.set_path(&format!("/{}", href_path));
                    log_trace!("Navigating existing Mini App to: {}", nav_url);
                    window.navigate(nav_url)?;
                }
                window.show()?;
                window.set_focus()?;
                return Ok(());
            } else {
                // Window was closed but instance still exists, clean up
                log_warn!("Instance exists but window missing, cleaning up: {}", existing_label);
                state.remove_instance(&existing_label).await;
            }
        }
    }
    
    // Load the package (with timeout to prevent infinite hang)
    log_trace!("[MiniApp] Loading package for {}...", window_label);
    let package = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        state.get_or_load_package(&id, path)
    ).await
    .map_err(|_| {
        log_error!("[MiniApp] Package load TIMED OUT after 15s for: {}", file_path);
        Error::Anyhow(anyhow::anyhow!("miniapp_open: package load timed out after 15s for: {}", file_path))
    })??;
    log_trace!("[MiniApp] Package loaded successfully: {}", package.manifest.name);
    
    // Parse the topic ID if provided (from the message's webxdc-topic tag)
    let realtime_topic = if let Some(ref topic_str) = topic_id {
        match super::realtime::decode_topic_id(topic_str) {
            Ok(topic) => Some(topic),
            Err(e) => {
                log_warn!("Failed to decode topic ID '{}': {}", topic_str, e);
                None
            }
        }
    } else {
        None
    };
    
    // Create the instance
    let instance = MiniAppInstance {
        package: (*package).clone(),
        chat_id: chat_id.clone(),
        message_id: message_id.clone(),
        window_label: window_label.clone(),
        realtime_topic,
    };
    
    // Register the instance before creating the window
    state.add_instance(instance.clone()).await;

    // Preconnect: if this Mini App uses the realtime API, create the gossip channel
    // and connect to peers in the background. joinRealtimeChannel() awaits the
    // signal, then just attaches the event listener — preconnect is the sole initiator.
    let (pc_tx, pc_rx) = tokio::sync::watch::channel(false);
    state.set_preconnect_signal(&window_label, pc_rx).await;
    {
        let app_pc = app.clone();
        let pkg_path = package.path.clone();
        let pkg_name = package.manifest.name.clone();
        let chat_id_pc = chat_id.clone();
        let msg_id_pc = message_id.clone();
        let topic_str = topic_id.clone();
        let rt_topic = instance.realtime_topic;
        let label_pc = window_label.clone();
        tokio::spawn(async move {
            log_info!("[WEBXDC] Preconnect: scanning '{}' for realtime API (path: {:?})", pkg_name, pkg_path);
            let uses_rt = tokio::task::spawn_blocking(move || {
                MiniAppPackage::scan_for_realtime_api(&pkg_path)
            }).await.unwrap_or(false);

            if !uses_rt {
                log_info!("[WEBXDC] Preconnect: '{}' does NOT use realtime API, skipping", pkg_name);
                drop(pc_tx);
                return;
            }
            log_info!("[WEBXDC] Preconnect: '{}' uses realtime API, initializing Iroh", pkg_name);

            // Compute topic (same logic as joinRealtimeChannel)
            let topic = rt_topic.unwrap_or_else(|| {
                super::realtime::derive_topic_id(&pkg_name, &chat_id_pc, &msg_id_pc)
            });
            let topic_encoded = match topic_str {
                Some(ts) => ts,
                None => super::realtime::encode_topic_id(&topic),
            };

            let state = app_pc.state::<super::state::MiniAppsState>();
            let iroh = match state.realtime.get_or_init().await {
                Ok(i) => i,
                Err(e) => { log_warn!("[WEBXDC] Preconnect: Iroh init failed: {e}"); return; }
            };

            // Collect any cached peer addresses (from advertisements that arrived
            // before we opened) + persisted peers from the DB to use as bootstrap.
            let mut bootstrap_peers: Vec<iroh::EndpointAddr> = Vec::new();

            // Cached from recent Nostr advertisements
            let cached = state.take_peer_addrs(&topic).await;
            bootstrap_peers.extend(cached);

            // Persisted from DB
            let my_npub = crate::MY_PUBLIC_KEY.get()
                .and_then(|pk| nostr_sdk::prelude::ToBech32::to_bech32(pk).ok())
                .unwrap_or_default();
            if let Ok(records) = crate::db::get_active_peer_advertisements(&topic_encoded, &my_npub) {
                for record in &records {
                    if let Ok(addr) = super::realtime::decode_node_addr(&record.node_addr_encoded) {
                        bootstrap_peers.push(addr);
                    }
                }
            }

            log_info!("[WEBXDC] Preconnect: joining with {} bootstrap peers", bootstrap_peers.len());

            // Create gossip channel with bootstrap peers and NO event target.
            // Incoming data is BUFFERED (not dropped) until joinRealtimeChannel
            // sets the target and flushes.
            if let Err(e) = iroh.join_channel(topic, bootstrap_peers, None, Some(app_pc.clone()), label_pc.clone(), None).await {
                log_warn!("[WEBXDC] Preconnect: join_channel failed: {e}");
                return;
            }

            state.set_realtime_channel(&label_pc, super::state::RealtimeChannelState {
                topic, active: true,
            }).await;

            // Send advertisement so peers know we're online
            let node_addr = iroh.get_node_addr();
            if let Ok(encoded) = super::realtime::encode_node_addr(&node_addr) {
                crate::commands::realtime::send_webxdc_peer_advertisement(
                    chat_id_pc, topic_encoded.clone(), encoded,
                ).await;
            }

            if let Some(pk) = crate::MY_PUBLIC_KEY.get() {
                let npub = pk.to_bech32().unwrap();
                state.add_session_peer(topic, npub).await;
            }

            if let Some(main_window) = app_pc.get_webview_window("main") {
                let session_peers = state.get_session_peers(&topic).await;
                let _ = main_window.emit("miniapp_realtime_status", serde_json::json!({
                    "topic": topic_encoded,
                    "peer_count": session_peers.len(),
                    "peers": session_peers,
                    "is_active": true,
                }));
            }

            log_info!("[WEBXDC] Preconnect: Iroh ready for '{}'", label_pc);
            let _ = pc_tx.send(true);

            // Connect to known peers (runs after signal — doesn't block joinRealtimeChannel).
            // Single attempt, 5s timeout — stale peers fail fast.
            let my_npub = crate::MY_PUBLIC_KEY.get()
                .and_then(|pk| nostr_sdk::prelude::ToBech32::to_bech32(pk).ok())
                .unwrap_or_default();
            let mut connected_ids = std::collections::HashSet::new();

            if let Ok(records) = crate::db::get_active_peer_advertisements(&topic_encoded, &my_npub) {
                if !records.is_empty() {
                    log_info!("[WEBXDC] Preconnect: trying {} persisted peers", records.len());
                    for record in &records {
                        state.add_session_peer(topic, record.npub.clone()).await;
                    }
                    let peers: Vec<_> = records.iter().filter_map(|r| {
                        match super::realtime::decode_node_addr(&r.node_addr_encoded) {
                            Ok(addr) => { connected_ids.insert(addr.id); Some(addr) }
                            Err(e) => { log_warn!("[WEBXDC] Preconnect: bad peer addr: {e}"); None }
                        }
                    }).collect();
                    for addr in peers {
                        let peer_id = addr.id;
                        match tokio::time::timeout(
                            std::time::Duration::from_secs(5),
                            iroh.try_add_peer(&topic, &addr),
                        ).await {
                            Ok(Ok(_)) => log_info!("[WEBXDC] Preconnect: connected to peer {}", peer_id),
                            Ok(Err(e)) => log_trace!("[WEBXDC] Preconnect: peer {} failed: {e}", peer_id),
                            Err(_) => log_trace!("[WEBXDC] Preconnect: peer {} timed out (stale?)", peer_id),
                        }
                    }
                }
            }

            let cached = state.take_peer_addrs(&topic).await;
            for addr in cached.into_iter().filter(|a| !connected_ids.contains(&a.id)) {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    iroh.try_add_peer(&topic, &addr),
                ).await;
            }
        });
    }

    // ========================================
    // Android: Use native WebView overlay
    // ========================================
    #[cfg(target_os = "android")]
    {
        log_info!("Opening Mini App on Android: {} in overlay", package.manifest.name);

        // Open the native overlay WebView
        crate::android::miniapp::open_miniapp_overlay(
            &window_label,
            &file_path,
            &chat_id,
            &message_id,
            href.as_deref(),
        ).map_err(|e| Error::Anyhow(anyhow::anyhow!("Failed to open Mini App overlay: {}", e)))?;

        // Record to Mini Apps history
        let attachment_ref = file_path.clone();
        if let Err(e) = crate::db::record_miniapp_opened(
            package.manifest.name.clone(),
            file_path.clone(),
            attachment_ref,
        ) {
            log_warn!("Failed to record Mini App to history: {}", e);
        }

        return Ok(());
    }

    // ========================================
    // Desktop: Use WebviewWindowBuilder
    // ========================================
    #[cfg(not(target_os = "android"))]
    {
    // Build the initial URL - append href if provided
    let mut initial_url = get_miniapp_base_url()?;
    if let Some(ref href_value) = href {
        // Append href to the base URL (href should start with / or be a relative path)
        let href_path = href_value.trim_start_matches('/');
        initial_url.set_path(&format!("/{}", href_path));
        log_trace!("Mini App will open at: {}", initial_url);
    }
    let initial_url_clone = initial_url.clone();
    
    // Get the dummy proxy URL for network isolation (Linux only)
    // macOS: skipped due to version requirements
    // Windows: skipped due to WebView2 freeze issues
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let dummy_proxy_url = DUMMY_LOCALHOST_PROXY_URL
        .as_ref()
        .map_err(|_| Error::BlackholeProxyUnavailable)?;
    
    let mut window_builder = WebviewWindowBuilder::new(
        &app,
        &window_label,
        WebviewUrl::CustomProtocol(initial_url.clone()),
    )
    .title(&package.manifest.name)
    .inner_size(480.0, 640.0)
    .min_inner_size(320.0, 480.0)
    .resizable(true)
    .focused(true)
    // Use initialization_script_for_all_frames like DeltaChat does
    .initialization_script_for_all_frames(INIT_SCRIPT)
    // Enable devtools in debug mode only
    .devtools(cfg!(debug_assertions))
    .on_navigation(move |url| {
        // Only allow navigation within the webxdc:// scheme or webxdc.localhost
        let scheme = url.scheme();
        let allowed = scheme == "webxdc" || (scheme == "http" && url.host_str() == Some("webxdc.localhost"));
        if !allowed {
            log_warn!("Blocked navigation to: {}", url);
        }
        allowed
    });
    
    // Platform-specific security settings
    
    // macOS: Disable link preview
    #[cfg(target_os = "macos")]
    {
        window_builder = window_builder.allow_link_preview(false);
    }
    
    // Non-macOS/non-Windows: Use dummy proxy for network isolation
    // Note: On macOS, proxy_url increases minimum version to 14, so we skip it
    // Note: On Windows, both proxy_url and additional_browser_args cause WebView2 to freeze
    //       We rely on CSP for security on Windows instead
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        window_builder = window_builder.proxy_url(dummy_proxy_url.clone());
    }
    
    let window = Arc::new(window_builder.build()?);
    
    // Set up window close handler
    let window_label_for_handler = window_label.clone();
    let app_handle_for_handler = app.app_handle().clone();
    let window_clone = Arc::clone(&window);
    
    // Track if we're already closing
    let is_closing = std::sync::atomic::AtomicBool::new(false);
    
    // URL for navigating before close (to trigger unload events)
    let webxdc_js_url = {
        let mut url = initial_url_clone.clone();
        url.set_path("/webxdc.js");
        url
    };
    
    window.on_window_event(move |event| {
        match event {
            tauri::WindowEvent::Destroyed => {
                log_info!("Mini App window destroyed: {}", window_label_for_handler);
                let app_handle = app_handle_for_handler.clone();
                let label = window_label_for_handler.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_handle.state::<MiniAppsState>();

                    // Full teardown: remove channel state, leave QUIC, clean session peers
                    let channel_state = state.remove_realtime_channel(&label).await;

                    if let Some(channel) = channel_state {
                        let topic_encoded = super::realtime::encode_topic_id(&channel.topic);

                        // Tear down the QUIC channel completely (close connections, abort tasks)
                        if let Ok(iroh) = state.realtime.get_or_init().await {
                            if let Err(e) = iroh.leave_channel(channel.topic, &label).await {
                                log_warn!("[WEBXDC] Failed to leave channel on close: {}", e);
                            }
                        }

                        // Remove ourselves from session peers
                        if let Some(my_pk) = crate::MY_PUBLIC_KEY.get() {
                            let my_npub = my_pk.to_bech32().unwrap();
                            state.remove_session_peer(&channel.topic, &my_npub).await;
                        }

                        // Emit status update — session_peers is the single source of truth
                        let session_peers = state.get_session_peers(&channel.topic).await;
                        let peer_count = session_peers.len();
                        if let Some(main_window) = app_handle.get_webview_window("main") {
                            let _ = main_window.emit("miniapp_realtime_status", serde_json::json!({
                                "topic": topic_encoded,
                                "peer_count": peer_count,
                                "peers": session_peers,
                                "is_active": false,
                                "has_pending_peers": peer_count > 0,
                            }));
                        }

                        // Send peer-left via Nostr so other clients update their lobby state
                        if let Some(instance) = state.get_instance(&label).await {
                            let chat_id = instance.chat_id.clone();
                            let topic_for_left = topic_encoded.clone();
                            tokio::spawn(async move {
                                if !crate::commands::realtime::send_webxdc_peer_left(chat_id, topic_for_left).await {
                                    log_warn!("[WEBXDC] Failed to send peer-left signal");
                                }
                            });
                        }
                    }

                    // Remove the instance
                    state.remove_instance(&label).await;
                });
            }
            tauri::WindowEvent::CloseRequested { api, .. } => {
                // Handle close gracefully to allow sendUpdate() calls to complete
                // This is a workaround for https://github.com/deltachat/deltachat-desktop/issues/3321
                let is_closing_already = is_closing.swap(true, std::sync::atomic::Ordering::Relaxed);
                if is_closing_already {
                    log_trace!("Second CloseRequested event, closing now");
                    return;
                }
                
                log_trace!("CloseRequested on Mini App window, will delay close");
                
                // Navigate to webxdc.js to trigger unload events
                // This allows sendUpdate() calls in visibilitychange/unload handlers to complete
                if let Err(err) = window_clone.navigate(webxdc_js_url.clone()) {
                    log_error!("Failed to navigate before close: {err}");
                    return;
                }
                
                // Hide the window immediately for better UX
                window_clone.hide()
                    .inspect_err(|err| log_warn!("Failed to hide window: {err}"))
                    .ok();
                
                api.prevent_close();
                
                let window_clone2 = Arc::clone(&window_clone);
                tauri::async_runtime::spawn(async move {
                    // Wait a bit for any pending sendUpdate() calls
                    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
                    log_trace!("Delay elapsed, closing Mini App window");
                    window_clone2.close()
                        .inspect_err(|err| log_error!("Failed to close window: {err}"))
                        .ok();
                });
            }
            _ => {}
        }
    });
    
    log_info!("Opened Mini App: {} in window {}", package.manifest.name, window_label);
    
    // Record to Mini Apps history
    // Use file_path as attachment_ref since it uniquely identifies the Mini App
    let attachment_ref = file_path.clone();
    if let Err(e) = crate::db::record_miniapp_opened(
        package.manifest.name.clone(),
        file_path.clone(),
        attachment_ref,
    ) {
        log_warn!("Failed to record Mini App to history: {}", e);
    }

    Ok(())
    } // End of #[cfg(not(target_os = "android"))] block
}

/// Close a Mini App window
#[tauri::command]
pub async fn miniapp_close(
    app: AppHandle,
    chat_id: String,
    message_id: String,
) -> Result<(), Error> {
    let state = app.state::<MiniAppsState>();

    if let Some((label, _)) = state.get_instance_by_message(&chat_id, &message_id).await {
        #[cfg(target_os = "android")]
        {
            // Close Android overlay
            crate::android::miniapp::close_miniapp_overlay()
                .map_err(|e| Error::Anyhow(anyhow::anyhow!("Failed to close Mini App overlay: {}", e)))?;
        }

        #[cfg(not(target_os = "android"))]
        {
            // Desktop: Close window
            if let Some(window) = app.get_webview_window(&label) {
                window.close()?;
            }
        }

        // Full teardown: remove channel state, shut down Iroh entirely on Android
        let channel_state = state.remove_realtime_channel(&label).await;
        if let Some(channel) = channel_state {
            let topic_encoded = super::realtime::encode_topic_id(&channel.topic);

            // Remove ourselves from session peers
            if let Some(my_pk) = crate::MY_PUBLIC_KEY.get() {
                if let Ok(my_npub) = my_pk.to_bech32() {
                    state.remove_session_peer(&channel.topic, &my_npub).await;
                }
            }

            // Send peer-left via Nostr
            let chat_id_clone = chat_id.clone();
            tokio::spawn(async move {
                crate::commands::realtime::send_webxdc_peer_left(chat_id_clone, topic_encoded).await;
            });
        }

        // On Android: full Iroh shutdown so next session gets a clean slate.
        // On desktop: WindowEvent::Destroyed handles leave_channel.
        #[cfg(target_os = "android")]
        {
            state.realtime.shutdown_iroh().await;
        }

        state.remove_instance(&label).await;
    }

    Ok(())
}

/// Get updates for a Mini App (called from the Mini App itself)
#[tauri::command]
pub async fn miniapp_get_updates(
    window: WebviewWindow,
    _state: State<'_, MiniAppsState>,
    last_known_serial: u32,
) -> Result<String, Error> {
    let label = window.label();
    
    if !label.starts_with("miniapp:") {
        return Err(Error::InstanceNotFoundByLabel(label.to_string()));
    }
    
    // TODO: Implement actual update storage and retrieval
    // For now, return empty array
    log_trace!("Mini App {} requesting updates since serial {}", label, last_known_serial);
    
    Ok("[]".to_string())
}

/// Send an update from a Mini App
#[tauri::command]
pub async fn miniapp_send_update(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, MiniAppsState>,
    update: serde_json::Value,
    description: String,
) -> Result<(), Error> {
    let label = window.label();
    
    if !label.starts_with("miniapp:") {
        return Err(Error::InstanceNotFoundByLabel(label.to_string()));
    }
    
    let instance = state.get_instance(label).await
        .ok_or_else(|| Error::InstanceNotFoundByLabel(label.to_string()))?;
    
    log_info!(
        "Mini App {} sending update: {} ({})",
        instance.package.manifest.name,
        description,
        serde_json::to_string(&update).unwrap_or_default()
    );
    
    // TODO: Store the update and broadcast to other participants
    // For now, just emit to the main window for display
    if let Some(main_window) = app.get_webview_window("main") {
        let _ = main_window.emit("miniapp_update_sent", serde_json::json!({
            "chat_id": instance.chat_id,
            "message_id": instance.message_id,
            "update": update,
            "description": description,
        }));
    }
    
    Ok(())
}

/// List all open Mini App instances
#[tauri::command]
pub async fn miniapp_list_open(
    _state: State<'_, MiniAppsState>,
) -> Result<Vec<MiniAppInfo>, Error> {
    // This is a simplified version - in a full implementation,
    // we'd return more detailed instance info
    Ok(vec![])
}

/// Simple MD5-like hash for generating IDs (not cryptographic)
fn md5_hash(input: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    hasher.finish()
}

// ============================================================================
// Realtime Channel Commands (Iroh P2P)
// ============================================================================

/// Result of joining a realtime channel
#[derive(Serialize)]
pub struct JoinRealtimeResult {
    /// Encoded topic ID
    pub topic: String,
    /// WebSocket URL for the zero-overhead fast path (if WS server is running)
    pub ws_url: Option<String>,
}

/// Join the realtime channel for a Mini App.
/// Preconnect (spawned in miniapp_open) pre-initializes Iroh and sends the advertisement.
/// This function creates the gossip channel WITH the event target (so no data is dropped),
/// then connects to known peers. Thanks to preconnect, Iroh is already init'd (~300ms vs 2-5s).
#[tauri::command]
pub async fn miniapp_join_realtime_channel(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, MiniAppsState>,
    channel: Channel<RealtimeEvent>,
) -> Result<JoinRealtimeResult, Error> {
    let label = window.label();

    if !label.starts_with("miniapp:") {
        return Err(Error::InstanceNotFoundByLabel(label.to_string()));
    }

    let instance = state.get_instance(label).await
        .ok_or_else(|| Error::InstanceNotFoundByLabel(label.to_string()))?;

    let topic = if let Some(t) = instance.realtime_topic {
        t
    } else {
        log_info!("[WEBXDC] No webxdc-topic tag, deriving local topic for: {}", label);
        super::realtime::derive_topic_id(&instance.package.manifest.name, &instance.chat_id, &instance.message_id)
    };

    let topic_encoded = encode_topic_id(&topic);

    // Wait for preconnect to finish initializing Iroh (up to 10s).
    // Preconnect handles: Iroh init (2-5s), advertisement, session peers, status.
    // If preconnect already finished, this returns instantly.
    if let Some(mut rx) = state.take_preconnect_signal(label).await {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            rx.wait_for(|ready| *ready),
        ).await;
    }

    // Iroh is pre-initialized by preconnect — this is instant (~5ns atomic load)
    let iroh = state.realtime.get_or_init().await
        .map_err(|e| Error::RealtimeError(e.to_string()))?;

    // Create the gossip channel WITH the event target — no data can be dropped
    let event_target = EventTarget::TauriChannel(channel);
    let ws_targets = Some(state.realtime.ws_senders.clone());
    let (is_rejoin, _) = iroh.join_channel(topic, vec![], Some(event_target), Some(app.clone()), label.to_string(), ws_targets).await
        .map_err(|e| Error::RealtimeError(e.to_string()))?;

    let topic_encoded_clone = topic_encoded.clone();
    if is_rejoin {
        log_info!("[WEBXDC] Re-joined existing channel: {} (topic: {})", label, topic_encoded);
    } else {
        log_info!("[WEBXDC] Joined new channel: {} (topic: {})", label, topic_encoded);
    }

    state.set_realtime_channel(label, RealtimeChannelState { topic, active: true }).await;

    if !is_rejoin {
        // Preconnect didn't run (scan false negative or non-realtime app).
        // Do peer connections + advertisement here as fallback.
        let my_npub = crate::MY_PUBLIC_KEY.get()
            .and_then(|pk| ToBech32::to_bech32(pk).ok())
            .unwrap_or_default();
        let mut connected_ids = std::collections::HashSet::new();

        if let Ok(records) = crate::db::get_active_peer_advertisements(&topic_encoded, &my_npub) {
            if !records.is_empty() {
                log_info!("[WEBXDC] Connecting to {} persisted peers for topic {}", records.len(), topic_encoded);
                for record in &records {
                    state.add_session_peer(topic, record.npub.clone()).await;
                }
                let peers: Vec<_> = records.iter().filter_map(|record| {
                    match super::realtime::decode_node_addr(&record.node_addr_encoded) {
                        Ok(addr) => { connected_ids.insert(addr.id); Some(addr) }
                        Err(e) => { log_warn!("[WEBXDC] Failed to decode persisted peer addr: {}", e); None }
                    }
                }).collect();
                for addr in peers {
                    let peer_id = addr.id;
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        iroh.try_add_peer(&topic, &addr),
                    ).await {
                        Ok(Ok(_)) => log_info!("[WEBXDC] Connected to persisted peer {}", peer_id),
                        Ok(Err(e)) => log_trace!("[WEBXDC] Peer {} failed: {e}", peer_id),
                        Err(_) => log_trace!("[WEBXDC] Peer {} timed out (stale?)", peer_id),
                    }
                }
            }
        }

        let cached_addrs = state.take_peer_addrs(&topic).await;
        for addr in cached_addrs.into_iter().filter(|a| !connected_ids.contains(&a.id)) {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                iroh.try_add_peer(&topic, &addr),
            ).await;
        }
    }

    // Send advertisement if preconnect didn't
    if !is_rejoin {
        let node_addr = iroh.get_node_addr();
        if let Ok(encoded) = encode_node_addr(&node_addr) {
            let chat_id = instance.chat_id.clone();
            let te = topic_encoded.clone();
            tokio::spawn(async move {
                crate::commands::realtime::send_webxdc_peer_advertisement(chat_id, te, encoded).await;
            });
        }
    }

    // Add self + emit status
    if let Some(my_pk) = crate::MY_PUBLIC_KEY.get() {
        let npub = my_pk.to_bech32().unwrap();
        state.add_session_peer(topic, npub).await;
    }

    if let Some(main_window) = app.get_webview_window("main") {
        let session_peers = state.get_session_peers(&topic).await;
        let _ = main_window.emit("miniapp_realtime_status", serde_json::json!({
            "topic": topic_encoded_clone,
            "peer_count": session_peers.len(),
            "peers": session_peers,
            "is_active": true,
        }));
    }

    let ws_url = state.realtime.ws_url();
    Ok(JoinRealtimeResult { topic: topic_encoded, ws_url })
}

/// Send realtime data via invoke fallback (used when WS fast-path isn't available).
/// Accepts raw bytes (Array.from(Uint8Array)) to avoid base91 encode/decode overhead.
#[tauri::command]
pub async fn miniapp_send_realtime_data(
    window: WebviewWindow,
    state: State<'_, MiniAppsState>,
    data: Vec<u8>,
) -> Result<(), Error> {
    let label = window.label();

    if data.len() > 128_000 {
        return Err(Error::RealtimeError(format!("Data too large: {} bytes", data.len())));
    }

    // Get the topic for this instance
    let topic = state.get_realtime_channel(label).await
        .ok_or(Error::RealtimeChannelNotActive)?;

    let iroh = state.realtime.get_or_init().await
        .map_err(|e| Error::RealtimeError(e.to_string()))?;

    iroh.send_data(topic, data).await
        .map_err(|e| Error::RealtimeError(e.to_string()))?;

    Ok(())
}

/// Leave the realtime channel
#[tauri::command]
pub async fn miniapp_leave_realtime_channel(
    window: WebviewWindow,
    state: State<'_, MiniAppsState>,
) -> Result<(), Error> {
    let label = window.label();
    
    if !label.starts_with("miniapp:") {
        return Err(Error::InstanceNotFoundByLabel(label.to_string()));
    }
    
    // Get and remove the channel state
    if let Some(channel_state) = state.remove_realtime_channel(label).await {
        // Leave the Iroh channel
        let iroh = state.realtime.get_or_init().await
            .map_err(|e| Error::RealtimeError(e.to_string()))?;
        
        iroh.leave_channel(channel_state.topic, label).await
            .map_err(|e| Error::RealtimeError(e.to_string()))?;
        
        log_info!("Left realtime channel for Mini App: {} (topic: {})", label, encode_topic_id(&channel_state.topic));
    }
    
    Ok(())
}

/// Add a peer to the realtime channel (called when receiving peer advertisement via Nostr)
#[tauri::command]
pub async fn miniapp_add_realtime_peer(
    window: WebviewWindow,
    state: State<'_, MiniAppsState>,
    peer_addr: String,
) -> Result<(), Error> {
    let label = window.label();
    
    if !label.starts_with("miniapp:") {
        return Err(Error::InstanceNotFoundByLabel(label.to_string()));
    }
    
    // Get the topic for this instance
    let topic = state.get_realtime_channel(label).await
        .ok_or(Error::RealtimeChannelNotActive)?;
    
    // Decode the peer address
    let peer = super::realtime::decode_node_addr(&peer_addr)
        .map_err(|e| Error::RealtimeError(format!("Invalid peer address: {}", e)))?;
    
    // Add the peer
    let iroh = state.realtime.get_or_init().await
        .map_err(|e| Error::RealtimeError(e.to_string()))?;
    
    iroh.add_peer(topic, peer).await
        .map_err(|e| Error::RealtimeError(e.to_string()))?;
    
    log_info!("Added peer to realtime channel for Mini App: {}", label);
    
    Ok(())
}

/// Get our node address for sharing with peers (via Nostr)
#[tauri::command]
pub async fn miniapp_get_realtime_node_addr(
    state: State<'_, MiniAppsState>,
) -> Result<String, Error> {
    let iroh = state.realtime.get_or_init().await
        .map_err(|e| Error::RealtimeError(e.to_string()))?;
    
    let addr = iroh.get_node_addr();

    super::realtime::encode_node_addr(&addr)
        .map_err(|e| Error::RealtimeError(e.to_string()))
}

/// Realtime channel status info
#[derive(serde::Serialize)]
pub struct RealtimeChannelInfo {
    /// Whether the channel is active
    pub active: bool,
    /// Number of connected peers (in active channel)
    pub peer_count: usize,
    /// Number of pending peers (waiting to connect)
    pub pending_peer_count: usize,
    /// Topic ID (encoded)
    pub topic_id: String,
    /// Npubs of peers in the session (for avatar display)
    pub peers: Vec<String>,
}

/// Get the realtime channel status for a topic
/// This is used by the main window to show player count on Mini App attachments
#[tauri::command]
pub async fn miniapp_get_realtime_status(
    state: State<'_, MiniAppsState>,
    topic_id: String,
) -> Result<RealtimeChannelInfo, Error> {
    let topic = super::realtime::decode_topic_id(&topic_id)
        .map_err(|e| Error::RealtimeError(e.to_string()))?;

    // Check if WE are actively playing (have a Mini App window open for this topic)
    let we_are_playing = {
        let channels = state.realtime_channels.read().await;
        channels.values().any(|ch| ch.topic == topic && ch.active)
    };

    // session_peers is the single source of truth for both count and avatars
    let peer_npubs = state.get_session_peers(&topic).await;
    let peer_count = peer_npubs.len();

    Ok(RealtimeChannelInfo {
        active: we_are_playing,
        peer_count,
        pending_peer_count: 0,
        topic_id,
        peers: peer_npubs,
    })
}

// ============================================================================
// Mini Apps History Commands
// ============================================================================

/// Record that a Mini App was opened
/// This tracks the app name, source URL, and the attachment reference for quick re-opening
#[tauri::command]
pub async fn miniapp_record_opened(
    _app: AppHandle,
    name: String,
    src_url: String,
    attachment_ref: String,
) -> Result<(), Error> {
    crate::db::record_miniapp_opened(name, src_url, attachment_ref)
        .map_err(|e| Error::DatabaseError(e))
}

/// Get the Mini Apps history (recently used apps)
/// Returns a list of Mini Apps sorted by last opened time (most recent first)
#[tauri::command]
pub async fn miniapp_get_history(
    _app: AppHandle,
    limit: Option<i64>,
) -> Result<Vec<crate::db::MiniAppHistoryEntry>, Error> {
    crate::db::get_miniapps_history(limit)
        .map_err(|e| Error::DatabaseError(e))
}

/// Removes a Mini App from history by name
#[tauri::command]
pub async fn miniapp_remove_from_history(
    _app: AppHandle,
    name: String,
) -> Result<(), Error> {
    crate::db::remove_miniapp_from_history(&name)
        .map_err(|e| Error::DatabaseError(e))
}

#[tauri::command]
pub async fn miniapp_toggle_favorite(
    _app: AppHandle,
    id: i64,
) -> Result<bool, Error> {
    crate::db::toggle_miniapp_favorite(id)
        .map_err(|e| Error::DatabaseError(e))
}

#[tauri::command]
pub async fn miniapp_set_favorite(
    _app: AppHandle,
    id: i64,
    is_favorite: bool,
) -> Result<(), Error> {
    crate::db::set_miniapp_favorite(id, is_favorite)
        .map_err(|e| Error::DatabaseError(e))
}

// ============================================================================
// Mini Apps Marketplace Commands
// ============================================================================

use super::marketplace::{MarketplaceApp, InstallStatus, MARKETPLACE_STATE};

/// Fetch available apps from the marketplace
/// If trusted_only is true, only apps from trusted publishers are returned
#[tauri::command]
pub async fn marketplace_fetch_apps(
    trusted_only: Option<bool>,
) -> Result<Vec<MarketplaceApp>, Error> {
    let trusted = trusted_only.unwrap_or(true);
    super::marketplace::fetch_marketplace_apps(trusted)
        .await
        .map_err(|e| Error::Anyhow(anyhow::anyhow!(e)))
}

/// Get cached marketplace apps (without fetching from network)
#[tauri::command]
pub async fn marketplace_get_cached_apps() -> Result<Vec<MarketplaceApp>, Error> {
    let state = MARKETPLACE_STATE.read().await;
    Ok(state.get_apps())
}

/// Get a specific marketplace app by ID
#[tauri::command]
pub async fn marketplace_get_app(
    app_id: String,
) -> Result<Option<MarketplaceApp>, Error> {
    let state = MARKETPLACE_STATE.read().await;
    Ok(state.get_app(&app_id).cloned())
}

/// Get a marketplace app by its blossom hash (SHA-256 of the .xdc file)
/// This is useful for looking up marketplace info for apps shared via chat
#[tauri::command]
pub async fn marketplace_get_app_by_hash(
    file_hash: String,
) -> Result<Option<MarketplaceApp>, Error> {
    let state = MARKETPLACE_STATE.read().await;
    Ok(state.get_app_by_hash(&file_hash).cloned())
}

/// Get the installation status of a marketplace app
#[tauri::command]
pub async fn marketplace_get_install_status(
    app_id: String,
) -> Result<InstallStatus, Error> {
    let state = MARKETPLACE_STATE.read().await;
    Ok(state.get_install_status(&app_id))
}

/// Install a marketplace app (download from Blossom)
#[tauri::command]
pub async fn marketplace_install_app(
    app: AppHandle,
    app_id: String,
) -> Result<String, Error> {
    super::marketplace::install_marketplace_app(&app, &app_id)
        .await
        .map_err(|e| Error::Anyhow(anyhow::anyhow!(e)))
}

/// Check if a marketplace app is already installed locally
#[tauri::command]
pub async fn marketplace_check_installed(
    app: AppHandle,
    app_id: String,
) -> Result<Option<String>, Error> {
    Ok(super::marketplace::check_app_installed(&app, &app_id).await)
}

/// Sync installation status for all cached apps
/// This checks which apps are already downloaded locally and if updates are available
#[tauri::command]
pub async fn marketplace_sync_install_status(
    app: AppHandle,
) -> Result<(), Error> {
    let apps_info: Vec<(String, String)> = {
        let state = MARKETPLACE_STATE.read().await;
        state.get_apps().iter().map(|a| (a.id.clone(), a.version.clone())).collect()
    };

    for (app_id, marketplace_version) in apps_info {
        if let Some(path) = super::marketplace::check_app_installed(&app, &app_id).await {
            // App is installed, check version for updates
            let installed_version = crate::db::get_miniapp_installed_version(&app_id)
                .unwrap_or(None);

            let update_available = match &installed_version {
                Some(installed_ver) => installed_ver != &marketplace_version,
                None => false, // No version recorded - assume current (file exists, so treat as up-to-date)
            };

            let mut state = MARKETPLACE_STATE.write().await;
            state.set_install_status(&app_id, InstallStatus::Installed { path });

            // Update version info on the cached app
            state.set_app_version_info(&app_id, installed_version, update_available);
        } else {
            // File doesn't exist, mark as not installed
            let mut state = MARKETPLACE_STATE.write().await;
            state.set_install_status(&app_id, InstallStatus::NotInstalled);

            // Clear version info
            state.set_app_version_info(&app_id, None, false);
        }
    }

    Ok(())
}

/// Add a trusted publisher to the marketplace
#[tauri::command]
pub async fn marketplace_add_trusted_publisher(
    npub: String,
) -> Result<(), Error> {
    let mut state = MARKETPLACE_STATE.write().await;
    state.add_trusted_publisher(npub);
    Ok(())
}

/// Open a marketplace app (install if needed, then launch)
#[tauri::command]
pub async fn marketplace_open_app(
    app: AppHandle,
    app_id: String,
) -> Result<(), Error> {
    // Check if already installed
    let local_path = super::marketplace::check_app_installed(&app, &app_id).await;
    
    let file_path = match local_path {
        Some(path) => path,
        None => {
            // Install first
            super::marketplace::install_marketplace_app(&app, &app_id)
                .await
                .map_err(|e| Error::Anyhow(anyhow::anyhow!(e)))?
        }
    };

    // Open the Mini App
    // Use empty chat_id and message_id for marketplace apps (solo play)
    miniapp_open(
        app,
        file_path,
        "".to_string(),
        "".to_string(),
        None,
        None,
    ).await
}

/// Uninstall a marketplace app
#[tauri::command]
pub async fn marketplace_uninstall_app(
    app: AppHandle,
    app_id: String,
    app_name: String,
) -> Result<(), Error> {
    super::marketplace::uninstall_marketplace_app(&app, &app_id, &app_name)
        .await
        .map_err(|e| Error::Anyhow(anyhow::anyhow!(e)))
}

/// Update a marketplace app to the latest version
/// Downloads to a temp file first, verifies hash, then replaces the old file
/// This ensures the old version is only deleted after the new version is successfully downloaded
#[tauri::command]
pub async fn marketplace_update_app(
    app: AppHandle,
    app_id: String,
) -> Result<String, Error> {
    super::marketplace::update_marketplace_app(&app, &app_id)
        .await
        .map_err(|e| Error::Anyhow(anyhow::anyhow!(e)))
}

/// Publish a Mini App to the marketplace
/// This uploads the .xdc file to Blossom and publishes a Nostr event with the metadata
#[tauri::command]
pub async fn marketplace_publish_app(
    _app: AppHandle,
    file_path: String,
    app_id: String,
    name: String,
    description: String,
    version: String,
    categories: Vec<String>,
    changelog: Option<String>,
    developer: Option<String>,
    source_url: Option<String>,
    permissions: Option<String>,
) -> Result<String, Error> {
    use crate::{NOSTR_CLIENT, get_blossom_servers};

    let client = NOSTR_CLIENT.get()
        .ok_or_else(|| Error::Anyhow(anyhow::anyhow!("Nostr client not initialized")))?;

    let signer = client.signer().await
        .map_err(|e| Error::Anyhow(anyhow::anyhow!("Failed to get signer: {}", e)))?;

    let blossom_servers = get_blossom_servers();

    // Convert categories to &str for the function
    let category_refs: Vec<&str> = categories.iter().map(|s| s.as_str()).collect();

    super::marketplace::publish_to_marketplace(
        signer,
        &file_path,
        &app_id,
        &name,
        &description,
        &version,
        category_refs,
        changelog.as_deref(),
        developer.as_deref(),
        source_url.as_deref(),
        permissions.as_deref(),
        blossom_servers,
    )
    .await
    .map_err(|e| Error::Anyhow(anyhow::anyhow!(e)))
}

/// Get the trusted publisher npub for the marketplace
#[tauri::command]
pub async fn marketplace_get_trusted_publisher() -> Result<String, Error> {
    Ok(super::marketplace::TRUSTED_PUBLISHER.to_string())
}

// ============================================================================
// Mini App Permissions Commands
// ============================================================================

/// Get all available Mini App permissions for UI display
#[tauri::command]
pub async fn miniapp_get_available_permissions() -> Result<Vec<super::permissions::PermissionInfo>, Error> {
    Ok(super::permissions::get_all_permission_info())
}

/// Get granted permissions for a specific Mini App by file hash
#[tauri::command]
pub async fn miniapp_get_granted_permissions(
    _app: AppHandle,
    file_hash: String,
) -> Result<String, Error> {
    crate::db::get_miniapp_granted_permissions(&file_hash)
        .map_err(|e| Error::Anyhow(anyhow::anyhow!(e)))
}

/// Set a permission for a Mini App by file hash (grant or revoke)
#[tauri::command]
pub async fn miniapp_set_permission(
    _app: AppHandle,
    file_hash: String,
    permission: String,
    granted: bool,
) -> Result<(), Error> {
    crate::db::set_miniapp_permission(&file_hash, &permission, granted)
        .map_err(|e| Error::Anyhow(anyhow::anyhow!(e)))
}

/// Set multiple permissions at once for a Mini App by file hash
#[tauri::command]
pub async fn miniapp_set_permissions(
    _app: AppHandle,
    file_hash: String,
    permissions: Vec<(String, bool)>,
) -> Result<(), Error> {
    let perm_refs: Vec<(&str, bool)> = permissions.iter()
        .map(|(p, g)| (p.as_str(), *g))
        .collect();
    crate::db::set_miniapp_permissions(&file_hash, &perm_refs)
        .map_err(|e| Error::Anyhow(anyhow::anyhow!(e)))
}

/// Check if an app has been prompted for permissions yet (by file hash)
#[tauri::command]
pub async fn miniapp_has_permission_prompt(
    _app: AppHandle,
    file_hash: String,
) -> Result<bool, Error> {
    crate::db::has_miniapp_permission_prompt(&file_hash)
        .map_err(|e| Error::Anyhow(anyhow::anyhow!(e)))
}

/// Revoke all permissions for a Mini App by file hash
#[tauri::command]
pub async fn miniapp_revoke_all_permissions(
    _app: AppHandle,
    file_hash: String,
) -> Result<(), Error> {
    crate::db::revoke_all_miniapp_permissions(&file_hash)
        .map_err(|e| Error::Anyhow(anyhow::anyhow!(e)))
}

/// Get granted permissions for the Mini App calling this command
/// This is called from within the Mini App's JS context to check permissions
/// Uses the file hash from the loaded package for permission lookup
#[tauri::command]
pub async fn miniapp_get_granted_permissions_for_window(
    app: AppHandle,
    webview_window: WebviewWindow,
) -> Result<String, Error> {
    let label = webview_window.label();

    if !label.starts_with("miniapp:") {
        return Err(Error::Anyhow(anyhow::anyhow!("Not a Mini App window")));
    }

    // Get the app instance from state to find the package
    let state = app.state::<MiniAppsState>();
    if let Some(instance) = state.get_instance(label).await {
        // Use the file hash for permission lookup - this is secure and content-based
        crate::db::get_miniapp_granted_permissions(&instance.package.file_hash)
            .map_err(|e| Error::Anyhow(anyhow::anyhow!(e)))
    } else {
        Err(Error::Anyhow(anyhow::anyhow!("Could not find Mini App instance")))
    }
}
