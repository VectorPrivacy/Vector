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
use super::realtime::{RealtimeEvent, EventTarget, TopicId, encode_topic_id};
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
    /// Whether the app calls the realtime (Iroh) API — drives the frontend's
    /// Tor IP-exposure consent prompt. `default` so older persisted JSON parses.
    #[serde(default)]
    pub uses_realtime: bool,
}

/// A Mini App icon as a data URI the webview can show: an SVG as the pixels it draws, never its
/// markup; one that refuses to render shows no icon.
fn icon_data_uri(bytes: &[u8]) -> Option<String> {
    if vector_core::svg::looks_like_svg(bytes) {
        let png = vector_core::svg::rasterize_png(bytes, 256).ok()?;
        return Some(crate::util::data_uri("image/png", &png));
    }
    Some(crate::util::data_uri(crate::util::mime_from_magic_bytes(bytes), bytes))
}

impl MiniAppInfo {
    pub fn from_package(pkg: &super::state::MiniAppPackage) -> Self {
        let icon_data = pkg.get_icon().and_then(|bytes| icon_data_uri(&bytes));

        Self {
            id: pkg.id.clone(),
            name: pkg.manifest.name.clone(),
            description: pkg.manifest.description.clone(),
            version: pkg.manifest.version.clone(),
            has_icon: icon_data.is_some(),
            icon_data,
            source_code_url: pkg.manifest.source_code_url.clone(),
            file_hash: Some(pkg.file_hash.clone()),
            uses_realtime: super::state::MiniAppPackage::scan_for_realtime_api(&pkg.path),
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

// Secure-context APIs a Mini App has no use for (a loopback-hosted app is a
// secure context): local file pickers, WebAuthn and the engine's own
// notification prompt. Best effort; the Permissions-Policy denies WebAuthn too.
try {
    ['showOpenFilePicker', 'showSaveFilePicker', 'showDirectoryPicker'].forEach(function (name) {
        try { Object.defineProperty(window, name, { value: undefined, configurable: false }); } catch (e) {}
    });
    if ('credentials' in navigator) {
        Object.defineProperty(Navigator.prototype, 'credentials', { get: function () { return undefined; }, configurable: false });
    }
    if (window.Notification) {
        Object.defineProperty(Notification, 'requestPermission', { value: function () { return Promise.resolve('denied'); }, configurable: false });
    }
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

        // The MCP debug bridge's own commands pass in debug builds only; release also
        // grants them no capability.
        const debugBridge = __VECTOR_DEBUG_BRIDGE__;
        wrappedCore.invoke = async (cmd, args) => {
            if (allowedCommands.includes(cmd) || (debugBridge && cmd.startsWith('plugin:mcp-bridge|'))) {
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

/// Storage partition for a Mini App. Browser storage keys on ORIGIN, so the
/// partition becomes the origin's host: reserved marketplace ids keep their
/// storage across app updates, everything else keys on content — an unknown
/// app's new version starts clean. Reserved ids are host-sanitized and
/// suffixed with a digest so distinct ids stay distinct after sanitizing.
/// Android serves this as an `http://` hostname label (63-char cap, no edge
/// hyphens) — the digest carries uniqueness, the readable part is a courtesy.
#[allow(dead_code)] // Unused on Windows only
async fn miniapp_storage_partition(file_hash: &str) -> String {
    let reserved = {
        let state = MARKETPLACE_STATE.read().await;
        state.get_app_by_hash(file_hash).map(|a| a.id.clone())
    };
    match reserved {
        Some(id) if super::marketplace::is_safe_app_id(&id) => {
            use sha2::{Digest, Sha256};
            let mut sanitized: String = id
                .to_ascii_lowercase()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            sanitized.truncate(24);
            let sanitized = sanitized.trim_matches('-');
            let mut h = Sha256::new();
            h.update(id.as_bytes());
            let tag = bytes_to_hex_string(&h.finalize());
            if sanitized.is_empty() {
                format!("app-{}", &tag[..8])
            } else {
                format!("{}-{}", sanitized, &tag[..8])
            }
        }
        _ => file_hash[..32.min(file_hash.len())].to_ascii_lowercase(),
    }
}

/// IPC for a window on the loopback host. Tauri treats that origin as remote,
/// so it needs its own capability: this window, this exact origin, and the
/// same commands as `capabilities/miniapp.json`.
#[cfg(not(target_os = "android"))]
const ISOLATED_IPC_PERMISSIONS: [&str; 7] = [
    // No core:event permissions: Vector broadcasts app events (decrypted messages
    // among them) to every webview, and the bridge needs none of them.
    "allow-miniapp-get-updates",
    "allow-miniapp-send-update",
    "allow-miniapp-join-realtime-channel",
    "allow-miniapp-leave-realtime-channel",
    "allow-miniapp-send-realtime-data",
    "allow-miniapp-get-granted-permissions-for-window",
    "notification:allow-is-permission-granted",
];

#[cfg(not(target_os = "android"))]
fn grant_isolated_ipc(app: &AppHandle, window_label: &str, port: u16) -> Result<(), Error> {
    use std::collections::HashSet;
    use std::sync::{LazyLock, Mutex};
    use tauri::ipc::CapabilityBuilder;
    // Grants can't be revoked, so each (window, port) is added once.
    static GRANTED: LazyLock<Mutex<HashSet<(String, u16)>>> = LazyLock::new(Default::default);
    // Tauri resolves capabilities with an unwrap under a lock every IPC call
    // takes: a label it can't parse would take all IPC down with it.
    if window_label.is_empty()
        || !window_label.chars().all(|c| c.is_ascii_alphanumeric() || "-/:_".contains(c))
    {
        return Err(Error::Anyhow(anyhow::anyhow!("invalid Mini App window label")));
    }
    let key = (window_label.to_string(), port);
    let mut granted = GRANTED.lock().map_err(|_| Error::Anyhow(anyhow::anyhow!("capability registry poisoned")))?;
    if granted.contains(&key) {
        return Ok(());
    }
    let mut capability = CapabilityBuilder::new(format!("miniapp-isolated-{port}-{}", granted.len()))
        .local(false)
        .remote(super::isolated::origin(port))
        .window(window_label);
    for permission in ISOLATED_IPC_PERMISSIONS {
        capability = capability.permission(permission);
    }
    app.add_capability(capability).map_err(Error::Tauri)?;
    granted.insert(key);
    Ok(())
}

#[cfg(all(test, not(target_os = "android")))]
mod isolated_ipc_tests {
    /// An unknown permission id panics inside Tauri; stay a copy of the static capability.
    #[test]
    fn the_isolated_grant_matches_the_miniapp_capability() {
        let file: serde_json::Value =
            serde_json::from_str(include_str!("../../capabilities/miniapp.json")).unwrap();
        let listed: Vec<&str> = file["permissions"].as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect();
        let mut ours: Vec<&str> = super::ISOLATED_IPC_PERMISSIONS.to_vec();
        let mut theirs = listed.clone();
        ours.sort();
        theirs.sort();
        assert_eq!(ours, theirs);
        assert!(!listed.iter().any(|p| p.starts_with("core:event")));
    }
}

/// Get the base URL for Mini Apps based on platform
#[allow(dead_code)] // Used on desktop only
fn get_miniapp_base_url(partition: &str) -> Result<tauri::Url, Error> {
    // URI format:
    // mac/linux:  webxdc://<partition>.host/<path>
    // windows:    http://webxdc.<partition>.host/<path> — wry's WebView2
    //             workaround intercepts by PREFIX (`http://webxdc.*`) and
    //             strips it back to `webxdc://<partition>.host/`, so
    //             per-partition hosts ride the existing filter untouched
    // android:    unused (mini-apps run in the native overlay WebView)
    #[cfg(target_os = "windows")]
    {
        format!("http://webxdc.{}.host/", partition)
            .parse()
            .map_err(|e: url::ParseError| Error::Anyhow(e.into()))
    }
    #[cfg(target_os = "android")]
    {
        let _ = partition;
        "http://webxdc.localhost/"
            .parse()
            .map_err(|e: url::ParseError| Error::Anyhow(e.into()))
    }
    #[cfg(not(any(target_os = "windows", target_os = "android")))]
    {
        format!("webxdc://{}.host/", partition)
            .parse()
            .map_err(|e: url::ParseError| Error::Anyhow(e.into()))
    }
}

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

    // Android content:// URIs (share / clipboard paste) aren't real filesystem
    // paths — the existence check below would fail. Read the bytes via the content
    // resolver and parse in-memory; the manifest supplies the real name + icon.
    #[cfg(target_os = "android")]
    {
        if file_path.starts_with("content://") {
            let (bytes, _ext) = crate::android::filesystem::read_android_uri_bytes(file_path.clone())
                .map_err(|e| Error::InvalidPackage(format!("Failed to read content URI: {}", e)))?;
            return load_info_from_bytes(&bytes, "app.xdc");
        }
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

/// Load Mini App info for the file the composer just cached (`cache_file_bytes`):
/// the bytes are already in the backend, so nothing crosses IPC again.
#[tauri::command]
pub async fn miniapp_load_info_from_cached_file() -> Result<MiniAppInfo, Error> {
    let (bytes, file_name) = {
        let cache = crate::message::files::JS_FILE_CACHE.lock().unwrap();
        let (bytes, name, _) = cache
            .as_ref()
            .ok_or_else(|| Error::Anyhow(anyhow::anyhow!("no cached file to inspect")))?;
        (bytes.clone(), name.clone())
    };
    load_info_from_bytes(&bytes[..], &file_name)
}

/// Mini App info from an in-memory archive (a cached paste, or an Android content URI).
fn load_info_from_bytes(bytes: &[u8], file_name: &str) -> Result<MiniAppInfo, Error> {
    // Extract name without extension for fallback
    let fallback_name = file_name
        .rsplit('.').nth(1)
        .unwrap_or(file_name)
        .to_string();

    // Compute SHA-256 hash of the bytes for permission identification
    let file_hash = bytes_to_hex_string(&vector_core::crypto::sha256::digest(bytes));

    let (manifest, icon_bytes) = MiniAppPackage::load_info_from_bytes(bytes, &fallback_name)?;

    // Convert icon bytes to base64 data URL
    let icon_data = icon_bytes.and_then(|bytes| icon_data_uri(&bytes));

    Ok(MiniAppInfo {
        id: format!("miniapp_preview_{}", md5_hash(file_name)),
        name: manifest.name,
        description: manifest.description,
        version: manifest.version,
        has_icon: icon_data.is_some(),
        icon_data,
        source_code_url: manifest.source_code_url,
        file_hash: Some(file_hash),
        // Bytes-only preview (publish dialog) — never a launch path, so the
        // realtime flag isn't consulted; skip the scan.
        uses_realtime: false,
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
    vector_core::db::scoped(async move {
        log_info!("[WEBXDC] miniapp_open called: chat={}, msg={}", chat_id, message_id);
        let path = PathBuf::from(&file_path);

        // Generate unique ID from file hash
        let id = format!("miniapp_{:x}", md5_hash(&file_path));
        // A solo launch is one window per package, whatever placeholder message id the
        // launcher minted; a chat launch is one per message.
        let window_label = if chat_id == "solo" || (chat_id.is_empty() && message_id.is_empty()) {
            format!("miniapp:solo:{}", id)
        } else {
            format!("miniapp:{}:{}", chat_id, message_id)
        };
    
        log_trace!("Opening Mini App: {} ({}, {}) with href: {:?}, topic: {:?}", window_label, chat_id, message_id, href, topic_id);

        let state = app.state::<MiniAppsState>();

        // Check if already open
        log_trace!("[MiniApp] Checking for existing instance...");
        if let Some(existing_instance) = state.get_instance(&window_label).await {
            let existing_label = window_label.clone();
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
                    log_warn!("Instance exists but overlay closed, full cleanup: {}", existing_label);
                    teardown_window(&app, &existing_label, existing_instance.instance_id).await;
                }
            }

            #[cfg(not(target_os = "android"))]
            {
                // Desktop: Focus existing window
                if let Some(window) = app.get_webview_window(&existing_label) {
                    // If href is provided, navigate to it
                    if let Some(ref href_value) = href {
                        let mut nav_url = window.url().map_err(Error::Tauri)?;
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
                    teardown_window(&app, &existing_label, existing_instance.instance_id).await;
                }
            }
        }
    
        // The package load can take seconds; a second click meanwhile must not race a
        // second window under the same label.
        let Some(_opening) = state.begin_opening(&window_label) else {
            log_trace!("[MiniApp] Already opening {}, ignoring", window_label);
            return Ok(());
        };

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

        #[cfg(target_os = "android")]
        if package.manifest.cross_origin_isolated && !crate::android::miniapp::THREADED_MINIAPPS {
            let msg = format!("{} needs the Vector app on a computer for now", package.manifest.name);
            vector_core::traits::emit_event("show_toast", &msg);
            return Err(Error::Anyhow(anyhow::anyhow!(msg)));
        }

        // One window per isolated app: two would share (and fight over) one data store.
        #[cfg(not(target_os = "android"))]
        if package.manifest.cross_origin_isolated {
            let partition = miniapp_storage_partition(&package.file_hash).await;
            if let Some(other) = super::isolated::window_for_partition(&partition, &window_label) {
                if let Some(window) = app.get_webview_window(&other) {
                    window.show()?;
                    window.set_focus()?;
                    return Ok(());
                }
            }
        }

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
            instance_id: super::state::next_instance_id(),
        };
    
        // Register the instance before creating the window
        state.add_instance(instance.clone()).await;

        // Preconnect: if this Mini App uses the realtime API, join its session now in
        // the background; the app's joinRealtimeChannel() then only attaches to it.
        let (pc_tx, pc_rx) = tokio::sync::watch::channel(false);
        state.set_preconnect_signal(&window_label, pc_rx).await;
        {
            let app_pc = app.clone();
            let pkg_path = package.path.clone();
            let pkg_name = package.manifest.name.clone();
            let instance_pc = instance.clone();
            vector_core::db::spawn_bound(async move {
                let uses_rt = tokio::task::spawn_blocking(move || {
                    MiniAppPackage::scan_for_realtime_api(&pkg_path)
                }).await.unwrap_or(false);

                if !uses_rt {
                    log_info!("[WEBXDC] Preconnect: '{}' does NOT use realtime API, skipping", pkg_name);
                    drop(pc_tx);
                    return;
                }
                if let Err(e) = start_realtime(&app_pc, &instance_pc, None).await {
                    log_warn!("[WEBXDC] Preconnect for '{}' failed: {e}", pkg_name);
                    return;
                }
                log_info!("[WEBXDC] Preconnect: realtime ready for '{}'", instance_pc.window_label);
                let _ = pc_tx.send(true);
            });
        }

        // ========================================
        // Android: Use native WebView overlay
        // ========================================
        #[cfg(target_os = "android")]
        {
            log_info!("Opening Mini App on Android: {} in overlay", package.manifest.name);

            // Its port goes into the app's CSP, so it must exist before the page loads.
            state.realtime.ensure_ws_started();

            // Open the native overlay WebView
            crate::android::miniapp::open_miniapp_overlay(
                &window_label,
                &file_path,
                &chat_id,
                &message_id,
                href.as_deref(),
                &miniapp_storage_partition(&package.file_hash).await,
                package.manifest.cross_origin_isolated,
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
        let partition = miniapp_storage_partition(&package.file_hash).await;
        let isolated = package.manifest.cross_origin_isolated;

        // Apps that opt into cross-origin isolation are served from a loopback
        // origin of their own (isolated.rs); everything else keeps the custom scheme.
        let (initial_url, first_url, isolated_boot, isolated_host) = if isolated {
            let started = match super::isolated::start(&app, &window_label, &partition, href.as_deref()).await {
                Ok(started) => started,
                Err(super::isolated::StartError::AlreadyOpen(other)) => {
                    state.remove_instance_if(&window_label, instance.instance_id).await;
                    if let Some(window) = app.get_webview_window(&other) {
                        window.show()?;
                        window.set_focus()?;
                    }
                    return Ok(());
                }
                Err(super::isolated::StartError::Failed(e)) => {
                    state.remove_instance_if(&window_label, instance.instance_id).await;
                    return Err(Error::Anyhow(anyhow::anyhow!("Mini App loopback host: {e}")));
                }
            };
            // Stops the host on any early return below; disarmed once the window exists.
            let guard = super::isolated::HostGuard::new(&window_label, started.id);
            let port = started.port;
            let boot_url = started.boot_url;
            grant_isolated_ipc(&app, &window_label, port)?;
            let origin: tauri::Url = format!("{}/", super::isolated::origin(port))
                .parse()
                .map_err(|e: url::ParseError| Error::Anyhow(e.into()))?;
            // Linux opens blank first: the proxy that keeps the app off the network
            // must be in place (with this origin exempt) before the app's code runs.
            #[cfg(target_os = "linux")]
            let first = WebviewUrl::External("about:blank".parse().map_err(|e: url::ParseError| Error::Anyhow(e.into()))?);
            #[cfg(not(target_os = "linux"))]
            let first = WebviewUrl::External(boot_url.clone());
            (origin, first, Some((port, boot_url)), Some((guard, started.id)))
        } else {
            let mut url = get_miniapp_base_url(&partition)?;
            if let Some(ref href_value) = href {
                // Append href to the base URL (href should start with / or be a relative path)
                let href_path = href_value.trim_start_matches('/');
                url.set_path(&format!("/{}", href_path));
                log_trace!("Mini App will open at: {}", url);
            }
            let first = WebviewUrl::CustomProtocol(url.clone());
            (url, first, None, None)
        };
        let initial_url_clone = initial_url.clone();
    
        // Get the dummy proxy URL for network isolation (Linux only)
        // macOS: skipped due to version requirements
        // Windows: skipped due to WebView2 freeze issues
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        let dummy_proxy_url = DUMMY_LOCALHOST_PROXY_URL
            .as_ref()
            .map_err(|_| Error::BlackholeProxyUnavailable)?;
    
        let init_script = INIT_SCRIPT.replace(
            "__VECTOR_DEBUG_BRIDGE__",
            if cfg!(debug_assertions) { "true" } else { "false" },
        );
        #[cfg(target_os = "macos")]
        super::pointer_lock::install(&app).await;
        // Its port goes into every Mini App page's CSP, so it must exist first.
        state.realtime.ensure_ws_started();

        let mut window_builder = WebviewWindowBuilder::new(
            &app,
            &window_label,
            first_url,
        )
        .title(&package.manifest.name)
        .inner_size(480.0, 640.0)
        .min_inner_size(320.0, 480.0)
        .resizable(true)
        .focused(true)
        // Use initialization_script_for_all_frames like DeltaChat does
        .initialization_script_for_all_frames(&init_script)
        // Enable devtools in debug mode only
        .devtools(cfg!(debug_assertions))
        .on_navigation({
            // Pin navigation to this window's own origin: serving routes by
            // window label while storage keys on origin, so hopping to another
            // partition's host would run this app's code against that app's
            // storage.
            let own_scheme = initial_url.scheme().to_string();
            let own_host = initial_url.host_str().map(str::to_string);
            let own_port = initial_url.port_or_known_default();
            let blank_ok = cfg!(target_os = "linux") && isolated;
            move |url| {
                let allowed = (url.scheme() == own_scheme
                    && url.host_str().map(str::to_string) == own_host
                    && url.port_or_known_default() == own_port)
                    || (blank_ok && url.as_str() == "about:blank");
                if !allowed {
                    log_warn!("Blocked navigation to: {}", url);
                }
                allowed
            }
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
        if !isolated {
            window_builder = window_builder.proxy_url(dummy_proxy_url.clone());
        }

        // An isolated app gets a data store of its own: cookies, storage and cache
        // never meet another app's (or the main window's), whatever its origin.
        if isolated {
            #[cfg(target_os = "macos")]
            {
                window_builder = match super::isolated::store_identifier(&partition) {
                    Some(id) => window_builder.data_store_identifier(id),
                    None => window_builder.incognito(true),
                };
            }
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            {
                let dir = app
                    .path()
                    .app_data_dir()
                    .map_err(Error::Tauri)?
                    .join("miniapp-isolated")
                    .join(&partition);
                window_builder = window_builder.data_directory(dir);
            }
        }

        let window = Arc::new(window_builder.build()?);

        #[cfg(target_os = "linux")]
        if let Some((port, boot_url)) = isolated_boot.clone() {
            let blackhole = dummy_proxy_url.to_string();
            let (tx, rx) = tokio::sync::oneshot::channel();
            let queued = window.with_webview(move |wv| {
                use webkit2gtk::{NetworkProxyMode, NetworkProxySettings, WebContextExt, WebViewExt, WebsiteDataManagerExt};
                let view = wv.inner();
                super::isolated::enable_web_storage(&view);
                let applied = match view.context().and_then(|c| c.website_data_manager()) {
                    Some(manager) => {
                        // Its own origin, and the realtime WebSocket (token-gated, the
                        // only local port its CSP allows) so sends skip IPC.
                        let mut hosts = vec![format!("localhost:{port}")];
                        if let Some(rt) = super::scheme::rt_ws_port() {
                            hosts.push(format!("127.0.0.1:{rt}"));
                        }
                        let hosts: Vec<&str> = hosts.iter().map(String::as_str).collect();
                        let mut settings = NetworkProxySettings::new(Some(blackhole.as_str()), &hosts);
                        manager.set_network_proxy_settings(NetworkProxyMode::Custom, Some(&mut settings));
                        true
                    }
                    None => false,
                };
                let _ = tx.send(applied);
            });
            // Fail closed: without the blackhole the app would have the network.
            let applied = queued.is_ok()
                && matches!(tokio::time::timeout(std::time::Duration::from_secs(5), rx).await, Ok(Ok(true)));
            if !applied || window.navigate(boot_url).is_err() {
                let _ = window.destroy();
                return Err(Error::Anyhow(anyhow::anyhow!("could not isolate the Mini App's network")));
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = isolated_boot;
        let isolated_id = isolated_host.map(|(guard, id)| {
            guard.disarm();
            id
        });

        // A covered game must keep its socket: WebKit throttles a page whose window is
        // occluded (timers at 1 Hz, no animation frames, a suppressed process), and a
        // multiplayer session times out behind another window. Occlusion is ignored here;
        // minimising still counts as hidden.
        #[cfg(target_os = "macos")]
        {
            let _ = window.with_webview(|wv| unsafe {
                use objc2::{msg_send, runtime::AnyObject};
                let view = wv.inner() as *mut AnyObject;
                let _: () = msg_send![view, _setWindowOcclusionDetectionEnabled: false];
            });
        }

        // Set up window close handler
        let window_label_for_handler = window_label.clone();
        let app_handle_for_handler = app.app_handle().clone();
        // The instance this window is for, fixed now: a reopen of the same label
        // registers a new one before this window's teardown runs.
        let instance_id_for_handler = instance.instance_id;
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
                    if let Some(id) = isolated_id {
                        super::isolated::stop_if(&window_label_for_handler, id);
                    }
                    let app_handle = app_handle_for_handler.clone();
                    let label = window_label_for_handler.clone();
                    tauri::async_runtime::spawn(async move {
                        teardown_window(&app_handle, &label, instance_id_for_handler).await;
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
    })
    .await
}

/// Close a Mini App window
#[tauri::command]
pub async fn miniapp_close(
    app: AppHandle,
    chat_id: String,
    message_id: String,
) -> Result<(), Error> {
    let state = app.state::<MiniAppsState>();

    if let Some((label, instance)) = state.get_instance_by_message(&chat_id, &message_id).await {
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

        // Whichever of this and the window's own teardown runs second finds nothing to do.
        teardown_window(&app, &label, instance.instance_id).await;
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

/// The app joins its realtime channel: the window's session (usually joined
/// already by the open's preconnect) delivers to `channel` from now on,
/// starting with what it held while the app loaded.
#[tauri::command]
pub async fn miniapp_join_realtime_channel(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, MiniAppsState>,
    channel: Channel<RealtimeEvent>,
) -> Result<JoinRealtimeResult, Error> {
    vector_core::db::scoped(async move {
        let label = window.label();

        if !label.starts_with("miniapp:") {
            return Err(Error::InstanceNotFoundByLabel(label.to_string()));
        }

        let instance = state.get_instance(label).await
            .ok_or_else(|| Error::InstanceNotFoundByLabel(label.to_string()))?;

        // Preconnect usually has the session joined already; give it a moment to.
        if let Some(mut rx) = state.take_preconnect_signal(label).await {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                rx.wait_for(|ready| *ready),
            ).await;
        }

        // The window can close while this waits; a dead game must not rejoin.
        if state.get_instance(label).await.map(|i| i.instance_id) != Some(instance.instance_id) {
            return Err(Error::InstanceNotFoundByLabel(label.to_string()));
        }

        state.realtime.ensure_ws_started();
        let topic = start_realtime(&app, &instance, Some(EventTarget::TauriChannel(channel)))
            .await
            .map_err(Error::Realtime)?;

        let ws_url = state.realtime.ws_url_for(label);
        Ok(JoinRealtimeResult { topic: encode_topic_id(&topic), ws_url })
    })
    .await
}

/// Join a window's realtime session, or attach `target` to the one it already
/// has (a window joins once), and put us in the topic's lobby.
pub(crate) async fn start_realtime(app: &AppHandle, instance: &MiniAppInstance, target: Option<EventTarget>) -> Result<TopicId, String> {
    let state = app.state::<MiniAppsState>();
    let still_open = || async {
        state.get_instance(&instance.window_label).await.map(|i| i.instance_id) == Some(instance.instance_id)
    };
    if !still_open().await {
        return Err("the Mini App closed before joining".into());
    }
    let topic = instance.realtime_topic.unwrap_or_else(|| {
        super::realtime::derive_topic_id(&instance.package.manifest.name, &instance.chat_id, &instance.message_id)
    });
    let window = super::realtime::WindowSession {
        label: &instance.window_label,
        instance_id: instance.instance_id,
        chat_id: &instance.chat_id,
        topic,
        advertise: !(instance.chat_id.is_empty() || instance.chat_id == "solo"),
    };
    state.realtime.open(window, target).await.map_err(|e| e.to_string())?;
    state.set_realtime_channel(&instance.window_label, RealtimeChannelState {
        topic,
        active: true,
        instance_id: instance.instance_id,
    }).await;
    // Teardown removes the instance before anything else, so a window that
    // closed while this joined is caught here and its session ended.
    if !still_open().await {
        state.remove_realtime_channel_if(&instance.window_label, instance.instance_id).await;
        state.realtime.close(&instance.window_label, instance.instance_id).await;
        return Err("the Mini App closed while joining".into());
    }

    // The lobby: whoever this chat says is playing, and us.
    let topic_encoded = encode_topic_id(&topic);
    let me = crate::my_public_key().and_then(|pk| ToBech32::to_bech32(&pk).ok()).unwrap_or_default();
    if let Ok(records) = vector_core::db::miniapps::get_active_peer_advertisements_in(&topic_encoded, &instance.chat_id, &me, 32) {
        for record in records {
            state.add_session_peer(topic, record.npub).await;
        }
    }
    if !me.is_empty() {
        state.add_session_peer(topic, me).await;
    }
    let peers = state.get_session_peers(&topic).await;
    emit_lobby(&topic_encoded, peers, true);
    Ok(topic)
}

/// End what a Mini App window leaves behind, once, and only what `instance_id`
/// owns (a reopen of the same label registers a new instance first): the
/// instance itself; its realtime session, announcing the departure; our place
/// in its lobby.
pub(crate) async fn teardown_window(app: &AppHandle, label: &str, instance_id: u64) {
    let state = app.state::<MiniAppsState>();
    let instance = state.remove_instance_if(label, instance_id).await;
    let channel = state.remove_realtime_channel_if(label, instance_id).await;
    state.realtime.close(label, instance_id).await;

    // The topic whose in-chat status to clear: the channel's, else a solo app's own.
    let topic = channel.as_ref().map(|c| c.topic).or_else(|| instance.as_ref().and_then(|i| i.realtime_topic));
    if let Some(topic) = topic {
        let still_playing = state.has_realtime_channel_for_topic(&topic).await;
        let peers = if channel.is_some() {
            if !still_playing {
                if let Some(me) = crate::my_public_key().and_then(|pk| ToBech32::to_bech32(&pk).ok()) {
                    state.remove_session_peer(&topic, &me).await;
                }
            }
            state.get_session_peers(&topic).await
        } else {
            Vec::new()
        };
        emit_lobby(&encode_topic_id(&topic), peers, still_playing);
    }
}

/// End every Mini App window's session and close the windows, for an account
/// swap: run while the outgoing account's client can still announce departures.
pub(crate) async fn end_for_account_swap(app: &AppHandle) {
    let state = app.state::<MiniAppsState>();
    state.realtime.end_all().await;
    for (label, instance_id) in state.open_instances().await {
        #[cfg(not(target_os = "android"))]
        if let Some(window) = app.get_webview_window(&label) {
            let _ = window.destroy();
        }
        #[cfg(target_os = "android")]
        {
            let _ = crate::android::miniapp::close_miniapp_overlay();
        }
        teardown_window(app, &label, instance_id).await;
    }
}

/// The main window's lobby view of a topic: who is in it, and whether we are.
pub(crate) fn emit_lobby(topic_encoded: &str, peers: Vec<String>, is_active: bool) {
    vector_core::traits::emit_event_json(
        "miniapp_realtime_status",
        serde_json::json!({
            "topic": topic_encoded,
            "peer_count": peers.len(),
            "has_pending_peers": !peers.is_empty(),
            "peers": peers,
            "is_active": is_active,
        }),
    );
}

/// Send realtime data via invoke fallback (used when WS fast-path isn't available).
/// Accepts raw bytes (Array.from(Uint8Array)) to avoid base91 encode/decode overhead.
#[tauri::command]
pub async fn miniapp_send_realtime_data(
    window: WebviewWindow,
    state: State<'_, MiniAppsState>,
    data: Vec<u8>,
) -> Result<(), Error> {
    if data.len() > 128_000 {
        return Err(Error::Realtime(format!("Data too large: {} bytes", data.len())));
    }
    state.realtime.send(window.label(), data).await
        .map_err(|e| Error::Realtime(e.to_string()))
}

/// The app left the channel. Its window keeps the session (the departure goes out
/// when the window closes), so a rejoin needs no new advertisement.
#[tauri::command]
pub async fn miniapp_leave_realtime_channel(
    window: WebviewWindow,
    state: State<'_, MiniAppsState>,
) -> Result<(), Error> {
    let label = window.label();
    if !label.starts_with("miniapp:") {
        return Err(Error::InstanceNotFoundByLabel(label.to_string()));
    }
    state.realtime.detach(label).await;
    Ok(())
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
        .map_err(|e| Error::Realtime(e.to_string()))?;

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
        .map_err(Error::Database)
}

/// Get the Mini Apps history (recently used apps)
/// Returns a list of Mini Apps sorted by last opened time (most recent first)
#[tauri::command]
pub async fn miniapp_get_history(
    _app: AppHandle,
    limit: Option<i64>,
) -> Result<Vec<crate::db::MiniAppHistoryEntry>, Error> {
    crate::db::get_miniapps_history(limit)
        .map_err(Error::Database)
}

/// Removes a Mini App from history by name
#[tauri::command]
pub async fn miniapp_remove_from_history(
    _app: AppHandle,
    name: String,
) -> Result<(), Error> {
    crate::db::remove_miniapp_from_history(&name)
        .map_err(Error::Database)
}

#[tauri::command]
pub async fn miniapp_toggle_favorite(
    _app: AppHandle,
    id: i64,
) -> Result<bool, Error> {
    crate::db::toggle_miniapp_favorite(id)
        .map_err(Error::Database)
}

#[tauri::command]
pub async fn miniapp_set_favorite(
    _app: AppHandle,
    id: i64,
    is_favorite: bool,
) -> Result<(), Error> {
    crate::db::set_miniapp_favorite(id, is_favorite)
        .map_err(Error::Database)
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
    super::marketplace::sync_install_status_from_disk(&app).await;
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
// Tauri commands take their arguments individually from JS; a struct would change the IPC shape.
#[allow(clippy::too_many_arguments)]
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
    use crate::{nostr_client, get_blossom_servers};

    let _client = nostr_client()
        .ok_or_else(|| Error::Anyhow(anyhow::anyhow!("Nostr client not initialized")))?;

    let signer = vector_core::signer::active_signer()
        .map_err(|e| Error::Anyhow(anyhow::anyhow!("Failed to get signer: {}", e)))?;

    let blossom_servers = get_blossom_servers();

    // Convert categories to &str for the function
    let category_refs: Vec<&str> = categories.iter().map(|s| s.as_str()).collect();

    let listing = super::marketplace::MarketplaceListing {
        app_id: &app_id,
        name: &name,
        description: &description,
        version: &version,
        categories: category_refs,
        changelog: changelog.as_deref(),
        developer: developer.as_deref(),
        source_url: source_url.as_deref(),
        permissions: permissions.as_deref(),
    };
    super::marketplace::publish_to_marketplace(signer, &file_path, &listing, blossom_servers)
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

/// A URL-shared Mini App resolved to a playable local package.
#[derive(Serialize, Clone)]
pub struct UrlXdcInfo {
    pub path: String,
    pub hash: String,
    pub name: String,
    pub topic: String,
}

struct UrlXdcProgressReporter<'a> {
    url: &'a str,
}

impl crate::net::ProgressReporter for UrlXdcProgressReporter<'_> {
    fn report_progress(&self, percentage: Option<u8>, _bytes: Option<u64>, _speed: Option<f64>) -> Result<(), &'static str> {
        // Gated emitter: progress for an account no longer on screen paints nothing
        vector_core::traits::emit_event("webxdc_url_progress", &serde_json::json!({
            "url": self.url,
            "progress": percentage.unwrap_or(0),
        }));
        Ok(())
    }

    fn report_complete(&self) -> Result<(), &'static str> {
        vector_core::traits::emit_event("webxdc_url_progress", &serde_json::json!({
            "url": self.url,
            "progress": 100,
        }));
        Ok(())
    }
}

/// Resolve a pasted `.xdc` URL into a playable local package.
///
/// `download: false` is the render-time cache probe and must never touch the
/// network — merely receiving a message must not become a tracking beacon.
/// The package cache is content-addressed and account-agnostic (public bytes,
/// same trust shape as the marketplace); only the url→hash latch lives in
/// per-account settings.
///
/// The realtime topic is derived, not minted: a URL has no file event to
/// carry a topic tag, so every recipient computes the same
/// `derive_url_topic_id(url, msg_id)`. The bytes stay out of it — servers
/// rebuild identical apps into new hashes, and two players who tapped the
/// same card at different times must still land in the same session.
#[tauri::command]
pub async fn miniapp_resolve_url_xdc(
    app: AppHandle,
    url: String,
    msg_id: String,
    download: bool,
) -> Result<Option<UrlXdcInfo>, Error> {
    // Commands are unbound: pin the whole operation to the account that asked,
    // so the latch lands in ITS settings and a swap mid-download refuses the result
    vector_core::db::scoped_result(resolve_url_xdc_inner(app, url, msg_id, download)).await
}

async fn resolve_url_xdc_inner(
    app: AppHandle,
    url: String,
    msg_id: String,
    download: bool,
) -> Result<Option<UrlXdcInfo>, Error> {
    use sha2::{Digest, Sha256};
    let url_key = {
        let mut h = Sha256::new();
        h.update(url.as_bytes());
        bytes_to_hex_string(&h.finalize())
    };
    let dir = app.path().app_data_dir().map_err(Error::Tauri)?.join("miniapps").join("url");
    let kv_key = format!("xdcurl:{}", url_key);
    // Per-message latch: a message resolved once stays pinned to that exact
    // version — its card, bytes and realtime topic never drift. Freshness is
    // decided per NEW message, at tap time, against the server.
    let msg_key = {
        let mut h = Sha256::new();
        h.update(url.as_bytes());
        h.update(b"|");
        h.update(msg_id.as_bytes());
        format!("xdcmsg:{}", bytes_to_hex_string(&h.finalize()))
    };

    let read_latch = |raw: &str| -> Option<(String, String)> {
        let v: serde_json::Value = serde_json::from_str(raw).ok()?;
        let hash = v.get("hash")?.as_str()?.to_string();
        let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("Mini App").to_string();
        // The stored hash becomes a path component — never trust it raw
        (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some((hash, name))
    };

    if let Ok(Some(saved)) = crate::db::get_sql_setting(msg_key.clone()) {
        if let Some((hash, name)) = read_latch(&saved) {
            let path = dir.join(format!("{}.xdc", hash));
            if path.exists() {
                return Ok(Some(UrlXdcInfo {
                    path: path.to_string_lossy().into_owned(),
                    topic: vector_core::webxdc::derive_url_topic_id(&url, &msg_id),
                    hash,
                    name,
                }));
            }
        }
    }
    if !download {
        return Ok(None);
    }

    // Only http(s), only .xdc paths — anything else never leaves the app
    let lower = url.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) || !lower.ends_with(".xdc") {
        return Err(Error::InvalidPackage("Not a Mini App URL".to_string()));
    }

    // Revalidate: a re-post of a known URL reuses the cache only when the
    // server says the content is unchanged — the same URL legitimately changes
    // between posts during rapid xdc development. No validator = re-download.
    vector_core::net::validate_url_not_private(&url).map_err(|e| Error::InvalidPackage(e.to_string()))?;
    let current_validator = fetch_url_validator(&url).await;
    if let (Ok(Some(saved)), Some(cur)) = (crate::db::get_sql_setting(kv_key.clone()), current_validator.as_deref()) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&saved) {
            let stored = v.get("etag").and_then(|e| e.as_str()).unwrap_or_default();
            if !stored.is_empty() && stored == cur {
                if let Some((hash, name)) = read_latch(&saved) {
                    let path = dir.join(format!("{}.xdc", hash));
                    if path.exists() {
                        let _ = crate::db::set_sql_setting(
                            msg_key.clone(),
                            serde_json::json!({ "hash": hash, "name": name }).to_string(),
                        );
                        return Ok(Some(UrlXdcInfo {
                            path: path.to_string_lossy().into_owned(),
                            topic: vector_core::webxdc::derive_url_topic_id(&url, &msg_id),
                            hash,
                            name,
                        }));
                    }
                }
            }
        }
    }

    let reporter = UrlXdcProgressReporter { url: &url };
    let bytes = crate::net::download_with_reporter(&url, &reporter, None)
        .await
        .map_err(|e| Error::InvalidPackage(e.to_string()))?;

    // Validate BEFORE the cache write — garbage never lands on disk, and the
    // gates match the opener's exactly, or the latch pins a package that can
    // never open
    let fallback = lower.rsplit('/').next().unwrap_or("app.xdc").to_string();
    MiniAppPackage::validate_bytes_openable(&bytes)?;
    let (manifest, _icon) = MiniAppPackage::load_info_from_bytes(&bytes, &fallback)?;

    let hash = bytes_to_hex_string(&vector_core::crypto::sha256::digest(&bytes));
    std::fs::create_dir_all(&dir).map_err(Error::Io)?;
    let path = dir.join(format!("{}.xdc", hash));
    // Temp + atomic rename: a crash mid-write must never leave a truncated
    // file at the content-addressed path, and a re-download heals one
    let tmp = dir.join(format!("{}.xdc.tmp-{}", hash, std::process::id()));
    std::fs::write(&tmp, &bytes).map_err(Error::Io)?;
    std::fs::rename(&tmp, &path).map_err(Error::Io)?;
    let name = if manifest.name.is_empty() { fallback } else { manifest.name.clone() };
    let _ = crate::db::set_sql_setting(
        kv_key,
        serde_json::json!({ "hash": hash, "name": name, "etag": current_validator.unwrap_or_default() }).to_string(),
    );
    let _ = crate::db::set_sql_setting(
        msg_key,
        serde_json::json!({ "hash": hash, "name": name }).to_string(),
    );

    Ok(Some(UrlXdcInfo {
        path: path.to_string_lossy().into_owned(),
        topic: vector_core::webxdc::derive_url_topic_id(&url, &msg_id),
        hash,
        name,
    }))
}

/// The URL's freshness validator: ETag preferred, Last-Modified fallback.
/// None (no validator, or HEAD failed) means a re-post cannot prove the cache
/// is current and must re-download.
async fn fetch_url_validator(url: &str) -> Option<String> {
    let client = vector_core::net::build_http_client_with_options(
        Some(std::time::Duration::from_secs(20)),
        Some(std::time::Duration::from_secs(10)),
        true,
    )
    .ok()?;
    let res = vector_core::net::proxied_request(&client, reqwest::Method::HEAD, url).await.send().await.ok()?;
    let headers = res.headers();
    headers
        .get("etag")
        .or_else(|| headers.get("last-modified"))?
        .to_str()
        .ok()
        .map(|s| s.to_string())
}
