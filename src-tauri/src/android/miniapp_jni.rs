//! JNI callback functions for Mini Apps.
//!
//! This module provides the Kotlin → Rust JNI bridge. These functions are
//! called by the Kotlin code (MiniAppManager, MiniAppIpc, MiniAppWebViewClient)
//! and routed to the appropriate Rust handlers.

use jni::objects::{JByteArray, JClass, JObject, JString};
use jni::sys::{jbyteArray, jint, jstring, jobject};
use jni::JNIEnv;
use log::{debug, error, info, warn};
use std::io::Read;
use std::path::Path;

// ============================================================================
// Constants
// ============================================================================

/// Content Security Policy for Mini Apps.
/// - `default-src 'self'`: Only allow resources from same origin (webxdc.localhost)
/// - `webrtc 'block'`: Prevent IP leaks via WebRTC
/// - `unsafe-inline/eval`: Required for many Mini Apps to function
const CSP_HEADER: &str = r#"default-src 'self' http://webxdc.localhost; style-src 'self' http://webxdc.localhost 'unsafe-inline' blob:; font-src 'self' http://webxdc.localhost data: blob:; script-src 'self' http://webxdc.localhost 'unsafe-inline' 'unsafe-eval' blob:; connect-src 'self' http://webxdc.localhost ipc: data: blob:; img-src 'self' http://webxdc.localhost data: blob:; media-src 'self' http://webxdc.localhost data: blob:; webrtc 'block'"#;

/// Maximum size for realtime channel data (128 KB).
/// This matches the WebXDC specification limit.
pub const REALTIME_DATA_MAX_SIZE: usize = 128_000;

// ============================================================================
// MiniAppManager Callbacks
// ============================================================================

/// Called when a Mini App is opened.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppManager_onMiniAppOpened(
    mut env: JNIEnv,
    _class: JClass,
    miniapp_id: JString,
    chat_id: JString,
    message_id: JString,
) {
    let miniapp_id: String = match env.get_string(&miniapp_id) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get miniapp_id: {:?}", e);
            return;
        }
    };

    let chat_id: String = match env.get_string(&chat_id) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get chat_id: {:?}", e);
            return;
        }
    };

    let message_id: String = match env.get_string(&message_id) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get message_id: {:?}", e);
            return;
        }
    };

    info!(
        "Mini App opened (JNI callback): {} (chat: {}, message: {})",
        miniapp_id, chat_id, message_id
    );

    // TODO: Update state tracking if needed
}

/// Called when a Mini App is closed.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppManager_onMiniAppClosed(
    mut env: JNIEnv,
    _class: JClass,
    miniapp_id: JString,
) {
    let miniapp_id: String = match env.get_string(&miniapp_id) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get miniapp_id: {:?}", e);
            return;
        }
    };

    info!("Mini App closed (JNI callback): {}", miniapp_id);

    // TODO: Clean up state if needed
}

// ============================================================================
// MiniAppIpc Callbacks
// ============================================================================

/// Invoke a Mini App command.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppIpc_invokeNative(
    mut env: JNIEnv,
    _class: JClass,
    miniapp_id: JString,
    package_path: JString,
    command: JString,
    args: JString,
) -> jstring {
    let miniapp_id: String = match env.get_string(&miniapp_id) {
        Ok(s) => s.into(),
        Err(e) => return create_error_string(&mut env, &format!("Failed to get miniapp_id: {:?}", e)),
    };

    let package_path: String = match env.get_string(&package_path) {
        Ok(s) => s.into(),
        Err(e) => return create_error_string(&mut env, &format!("Failed to get package_path: {:?}", e)),
    };

    let command: String = match env.get_string(&command) {
        Ok(s) => s.into(),
        Err(e) => return create_error_string(&mut env, &format!("Failed to get command: {:?}", e)),
    };

    let args: String = match env.get_string(&args) {
        Ok(s) => s.into(),
        Err(e) => return create_error_string(&mut env, &format!("Failed to get args: {:?}", e)),
    };

    debug!("[{}] invokeNative: {} (args: {})", miniapp_id, command, args);

    // Route to appropriate handler
    let result = match command.as_str() {
        "get_granted_permissions" => {
            // Return granted permissions for this Mini App
            match get_granted_permissions_for_package(&package_path) {
                Ok(perms) => perms,
                Err(e) => format!(r#"{{"error":"{}"}}"#, e),
            }
        }
        _ => {
            warn!("[{}] Unknown command: {}", miniapp_id, command);
            format!(r#"{{"error":"Unknown command: {}"}}"#, command)
        }
    };

    match env.new_string(&result) {
        Ok(s) => s.into_raw(),
        Err(e) => {
            error!("Failed to create result string: {:?}", e);
            std::ptr::null_mut()
        }
    }
}

/// Send an update from the Mini App.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppIpc_sendUpdateNative(
    mut env: JNIEnv,
    _class: JClass,
    miniapp_id: JString,
    update: JString,
    description: JString,
) {
    let miniapp_id: String = match env.get_string(&miniapp_id) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get miniapp_id: {:?}", e);
            return;
        }
    };

    let update: String = match env.get_string(&update) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get update: {:?}", e);
            return;
        }
    };

    let description: String = match env.get_string(&description) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get description: {:?}", e);
            return;
        }
    };

    info!(
        "[{}] sendUpdate: {} ({})",
        miniapp_id, description, update
    );

    // TODO: Store update and broadcast to other participants
}

/// Get updates since a serial number.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppIpc_getUpdatesNative(
    mut env: JNIEnv,
    _class: JClass,
    miniapp_id: JString,
    last_known_serial: jint,
) -> jstring {
    let miniapp_id: String = match env.get_string(&miniapp_id) {
        Ok(s) => s.into(),
        Err(e) => return create_error_string(&mut env, &format!("Failed to get miniapp_id: {:?}", e)),
    };

    debug!(
        "[{}] getUpdates since serial: {}",
        miniapp_id, last_known_serial
    );

    // TODO: Implement actual update retrieval
    // For now, return empty array
    match env.new_string("[]") {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Join the realtime channel.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppIpc_joinRealtimeChannelNative(
    mut env: JNIEnv,
    _class: JClass,
    miniapp_id: JString,
) -> jstring {
    let miniapp_id: String = match env.get_string(&miniapp_id) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get miniapp_id: {:?}", e);
            return std::ptr::null_mut();
        }
    };

    info!("[{}] joinRealtimeChannel", miniapp_id);

    // TODO: Implement realtime channel join via Iroh
    // For now, return a placeholder topic ID
    match env.new_string("android-realtime-placeholder") {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Send realtime data.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppIpc_sendRealtimeDataNative(
    mut env: JNIEnv,
    _class: JClass,
    miniapp_id: JString,
    data: JByteArray,
) {
    let miniapp_id: String = match env.get_string(&miniapp_id) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get miniapp_id: {:?}", e);
            return;
        }
    };

    let bytes = match env.convert_byte_array(data) {
        Ok(b) => b,
        Err(e) => {
            error!("Failed to convert byte array: {:?}", e);
            return;
        }
    };

    debug!(
        "[{}] sendRealtimeData: {} bytes",
        miniapp_id,
        bytes.len()
    );

    // TODO: Send via Iroh
}

/// Leave the realtime channel.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppIpc_leaveRealtimeChannelNative(
    mut env: JNIEnv,
    _class: JClass,
    miniapp_id: JString,
) {
    let miniapp_id: String = match env.get_string(&miniapp_id) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get miniapp_id: {:?}", e);
            return;
        }
    };

    info!("[{}] leaveRealtimeChannel", miniapp_id);

    // TODO: Leave Iroh channel
}

/// Get the user's npub.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppIpc_getSelfAddrNative(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    // Get npub from Nostr client
    let npub = get_user_npub();

    match env.new_string(&npub) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Get the user's display name.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppIpc_getSelfNameNative(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    // Get display name from profile
    let name = get_user_display_name();

    match env.new_string(&name) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Get granted permissions for this Mini App.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppIpc_getGrantedPermissionsNative(
    mut env: JNIEnv,
    _class: JClass,
    miniapp_id: JString,
    package_path: JString,
) -> jstring {
    let package_path: String = match env.get_string(&package_path) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get package_path: {:?}", e);
            return std::ptr::null_mut();
        }
    };

    let perms = get_granted_permissions_for_package(&package_path).unwrap_or_default();

    match env.new_string(&perms) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

// ============================================================================
// MiniAppWebViewClient Callbacks
// ============================================================================

/// Handle a request for a file from the Mini App package.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppWebViewClient_handleMiniAppRequest(
    mut env: JNIEnv,
    _class: JClass,
    miniapp_id: JString,
    package_path: JString,
    path: JString,
) -> jobject {
    let miniapp_id: String = match env.get_string(&miniapp_id) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get miniapp_id: {:?}", e);
            return std::ptr::null_mut();
        }
    };

    let package_path: String = match env.get_string(&package_path) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get package_path: {:?}", e);
            return std::ptr::null_mut();
        }
    };

    let path: String = match env.get_string(&path) {
        Ok(s) => s.into(),
        Err(e) => {
            error!("Failed to get path: {:?}", e);
            return std::ptr::null_mut();
        }
    };

    debug!("[{}] handleMiniAppRequest: {}", miniapp_id, path);

    // Serve file from .xdc package
    match serve_file_from_package(&mut env, &package_path, &path) {
        Ok(response) => response,
        Err(e) => {
            error!("[{}] Failed to serve {}: {}", miniapp_id, path, e);
            std::ptr::null_mut()
        }
    }
}

/// Get the user's npub (also used by WebViewClient).
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppWebViewClient_getSelfAddrNative(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let npub = get_user_npub();
    match env.new_string(&npub) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Get the user's display name (also used by WebViewClient).
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppWebViewClient_getSelfNameNative(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let name = get_user_display_name();
    match env.new_string(&name) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

fn create_error_string(env: &mut JNIEnv, error: &str) -> jstring {
    let json = format!(r#"{{"error":"{}"}}"#, error.replace('"', "\\\""));
    match env.new_string(&json) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

fn get_user_npub() -> String {
    // Try to get npub from NOSTR_CLIENT
    if let Some(client) = crate::NOSTR_CLIENT.get() {
        // JNI callbacks run on Android's main thread, outside tokio runtime.
        // Create a temporary runtime to execute the async signer operations.
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                error!("Failed to create tokio runtime for npub lookup: {:?}", e);
                return "unknown".to_string();
            }
        };

        let result = rt.block_on(async {
            let signer = client.signer().await.ok()?;
            let pubkey = signer.get_public_key().await.ok()?;
            nostr_sdk::prelude::ToBech32::to_bech32(&pubkey).ok()
        });

        if let Some(npub) = result {
            return npub;
        }
    }
    "unknown".to_string()
}

fn get_user_display_name() -> String {
    // Try to get display name from STATE
    if let Ok(state) = crate::STATE.try_lock() {
        if let Some(profile) = state.profiles.iter().find(|p| p.mine) {
            if !profile.nickname.is_empty() {
                return profile.nickname.clone();
            } else if !profile.name.is_empty() {
                return profile.name.clone();
            }
        }
    }
    "Unknown".to_string()
}

fn get_granted_permissions_for_package(package_path: &str) -> Result<String, String> {
    // Compute file hash for permission lookup
    let path = Path::new(package_path);
    if !path.exists() {
        return Err("Package file not found".to_string());
    }

    let bytes = std::fs::read(path).map_err(|e| format!("Failed to read package: {}", e))?;

    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let file_hash = hex::encode(hasher.finalize());

    // TODO: Look up permissions from database
    // For now, return empty (no permissions granted)
    Ok(String::new())
}

fn serve_file_from_package(
    env: &mut JNIEnv,
    package_path: &str,
    path: &str,
) -> Result<jobject, String> {
    use std::io::Cursor;

    let package_file = Path::new(package_path);
    if !package_file.exists() {
        return Err("Package file not found".to_string());
    }

    let file = std::fs::File::open(package_file).map_err(|e| format!("Failed to open package: {}", e))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Failed to read ZIP: {}", e))?;

    // Normalize path
    let file_path = if path.is_empty() || path == "/" {
        "index.html"
    } else {
        path.strip_prefix("/").unwrap_or(path)
    };

    // Security: Block path traversal attempts
    // A malicious .xdc could try paths like "../../../etc/passwd" or "foo/../../../sensitive"
    if file_path.contains("..") {
        return Err("Path traversal not allowed".to_string());
    }

    // Try to find the file in the archive
    let mut zip_file = archive
        .by_name(file_path)
        .map_err(|_| format!("File not found in package: {}", file_path))?;

    // Read contents
    let mut contents = Vec::new();
    zip_file
        .read_to_end(&mut contents)
        .map_err(|e| format!("Failed to read file: {}", e))?;

    // Determine MIME type
    let mime_type = get_mime_type(file_path);

    // Create WebResourceResponse with security headers
    create_web_resource_response(env, &mime_type, &contents, CSP_HEADER)
}

fn get_mime_type(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext.to_lowercase().as_str() {
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
        "pdf" => "application/octet-stream", // Block PDF for security
        _ => "application/octet-stream",
    }
    .to_string()
}

fn create_web_resource_response(
    env: &mut JNIEnv,
    mime_type: &str,
    data: &[u8],
    csp: &str,
) -> Result<jobject, String> {
    // Create headers map
    let map_class = env
        .find_class("java/util/HashMap")
        .map_err(|e| format!("Failed to find HashMap class: {:?}", e))?;

    let headers = env
        .new_object(&map_class, "()V", &[])
        .map_err(|e| format!("Failed to create HashMap: {:?}", e))?;

    // Add headers
    let put_method = env
        .get_method_id(
            &map_class,
            "put",
            "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
        )
        .map_err(|e| format!("Failed to get put method: {:?}", e))?;

    // Content-Security-Policy
    let csp_key = env.new_string("Content-Security-Policy").map_err(|e| format!("{:?}", e))?;
    let csp_val = env.new_string(csp).map_err(|e| format!("{:?}", e))?;
    unsafe {
        env.call_method_unchecked(
            &headers,
            put_method,
            jni::signature::ReturnType::Object,
            &[
                jni::sys::jvalue { l: csp_key.into_raw() },
                jni::sys::jvalue { l: csp_val.into_raw() },
            ],
        )
        .map_err(|e| format!("Failed to put CSP header: {:?}", e))?;
    }

    // X-Content-Type-Options
    let xcto_key = env.new_string("X-Content-Type-Options").map_err(|e| format!("{:?}", e))?;
    let xcto_val = env.new_string("nosniff").map_err(|e| format!("{:?}", e))?;
    unsafe {
        env.call_method_unchecked(
            &headers,
            put_method,
            jni::signature::ReturnType::Object,
            &[
                jni::sys::jvalue { l: xcto_key.into_raw() },
                jni::sys::jvalue { l: xcto_val.into_raw() },
            ],
        )
        .map_err(|e| format!("Failed to put X-Content-Type-Options header: {:?}", e))?;
    }

    // Create ByteArrayInputStream
    let byte_array = env
        .byte_array_from_slice(data)
        .map_err(|e| format!("Failed to create byte array: {:?}", e))?;

    let bais_class = env
        .find_class("java/io/ByteArrayInputStream")
        .map_err(|e| format!("Failed to find ByteArrayInputStream class: {:?}", e))?;

    let input_stream = env
        .new_object(bais_class, "([B)V", &[(&byte_array).into()])
        .map_err(|e| format!("Failed to create ByteArrayInputStream: {:?}", e))?;

    // Create WebResourceResponse
    let wrr_class = env
        .find_class("android/webkit/WebResourceResponse")
        .map_err(|e| format!("Failed to find WebResourceResponse class: {:?}", e))?;

    let j_mime = env.new_string(mime_type).map_err(|e| format!("{:?}", e))?;
    let j_encoding = env.new_string("UTF-8").map_err(|e| format!("{:?}", e))?;
    let j_reason = env.new_string("OK").map_err(|e| format!("{:?}", e))?;

    let response = env
        .new_object(
            wrr_class,
            "(Ljava/lang/String;Ljava/lang/String;ILjava/lang/String;Ljava/util/Map;Ljava/io/InputStream;)V",
            &[
                (&j_mime).into(),
                (&j_encoding).into(),
                jni::objects::JValue::Int(200),
                (&j_reason).into(),
                (&headers).into(),
                (&input_stream).into(),
            ],
        )
        .map_err(|e| format!("Failed to create WebResourceResponse: {:?}", e))?;

    Ok(response.into_raw())
}
