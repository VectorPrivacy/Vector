//! JNI callback functions for Mini Apps.
//!
//! This module provides the Kotlin → Rust JNI bridge. These functions are
//! called by the Kotlin code (MiniAppManager, MiniAppIpc, MiniAppWebViewClient)
//! and routed to the appropriate Rust handlers.

use jni::objects::{JByteArray, JClass, JString};
use jni::sys::{jint, jstring, jobject};
use jni::JNIEnv;
use log::{debug, error, info, warn};
use std::io::Read;
use std::path::Path;
use tauri::{Emitter, Manager};
use crate::util::bytes_to_hex_string;
use crate::TAURI_APP;

// ============================================================================
// Constants
// ============================================================================

/// Content Security Policy for Mini Apps.
/// - `default-src 'self'`: Only allow resources from same origin (webxdc.localhost)
/// - `webrtc 'block'`: Prevent IP leaks via WebRTC
/// - `unsafe-inline/eval`: Required for many Mini Apps to function
const CSP_HEADER: &str = r#"default-src 'self' http://webxdc.localhost; style-src 'self' http://webxdc.localhost 'unsafe-inline' blob:; font-src 'self' http://webxdc.localhost data: blob:; script-src 'self' http://webxdc.localhost 'unsafe-inline' 'unsafe-eval' blob:; connect-src 'self' http://webxdc.localhost ipc: data: blob:; img-src 'self' http://webxdc.localhost data: blob:; media-src 'self' http://webxdc.localhost data: blob:; webrtc 'block'"#;

/// Permissions Policy for Mini Apps (Android document responses).
/// Autoplay is allowed (self) for video streaming in Mini Apps.
/// Must be on the document response to take effect (not subresource responses).
const PERMISSIONS_POLICY_HEADER: &str = "accelerometer=(), ambient-light-sensor=(), autoplay=(self), battery=(), bluetooth=(), camera=(), clipboard-read=(), clipboard-write=(), display-capture=(), fullscreen=(), geolocation=(), gyroscope=(), magnetometer=(), microphone=(), midi=(), payment=(), picture-in-picture=(), screen-wake-lock=(), speaker-selection=(), usb=(), web-share=(), xr-spatial-tracking=()";

/// Maximum size for realtime channel data (128 KB).
/// This matches the WebXDC specification limit.
#[allow(dead_code)]
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

    // Clean up realtime channel and instance state, notify frontend
    if let Some(app) = TAURI_APP.get() {
        let app = app.clone();
        let miniapp_id_owned = miniapp_id.clone();
        tauri::async_runtime::spawn(async move {
            let state = app.state::<crate::miniapps::state::MiniAppsState>();

            // Remove the realtime channel state (marks us as not playing)
            let channel_state = state.remove_realtime_channel(&miniapp_id_owned).await;

            if let Some(channel) = channel_state {
                let topic_encoded = crate::miniapps::realtime::encode_topic_id(&channel.topic);

                // Get current peer count and clear stale event target
                let peer_count = if let Ok(iroh) = state.realtime.get_or_init().await {
                    iroh.clear_event_target(&channel.topic).await;
                    let count = iroh.get_peer_count(&channel.topic).await;

                    // Leave the Iroh gossip channel (with timeout to avoid hanging)
                    match tokio::time::timeout(
                        tokio::time::Duration::from_secs(5),
                        iroh.leave_channel(channel.topic),
                    ).await {
                        Ok(Ok(())) => {
                            info!("[WEBXDC] Left Iroh channel on Mini App close: {}", miniapp_id_owned);
                        }
                        Ok(Err(e)) => {
                            warn!("[WEBXDC] Failed to leave Iroh channel on close: {}", e);
                        }
                        Err(_) => {
                            warn!("[WEBXDC] Timed out leaving Iroh channel on close: {}", miniapp_id_owned);
                        }
                    }

                    count
                } else {
                    0
                };

                // Emit status update to main window so frontend clears "Playing" state
                if let Some(main_window) = app.get_webview_window("main") {
                    let _ = main_window.emit("miniapp_realtime_status", serde_json::json!({
                        "topic": topic_encoded,
                        "peer_count": peer_count,
                        "is_active": false,
                        "has_pending_peers": peer_count > 0,
                    }));
                    info!("[WEBXDC] Emitted miniapp_realtime_status: active=false, peer_count={} for topic {}", peer_count, topic_encoded);
                }
            }

            // Remove the instance
            state.remove_instance(&miniapp_id_owned).await;
        });
    }
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

    let app = match TAURI_APP.get() {
        Some(a) => a.clone(),
        None => {
            error!("[{}] TAURI_APP not initialized", miniapp_id);
            return std::ptr::null_mut();
        }
    };

    // JNI runs on Android main thread, outside tokio. Use a lightweight
    // single-threaded runtime for synchronous state reads.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            error!("[{}] Failed to create tokio runtime: {:?}", miniapp_id, e);
            return std::ptr::null_mut();
        }
    };

    let state = app.state::<crate::miniapps::state::MiniAppsState>();

    // Read instance, derive topic, and set channel state synchronously.
    // Setting channel state here (before the async join) prevents a race where
    // sendRealtimeData arrives before the spawned join task completes.
    let setup = rt.block_on(async {
        let instance = state.get_instance(&miniapp_id).await
            .ok_or("Instance not found")?;

        let topic = if let Some(t) = instance.realtime_topic {
            t
        } else {
            crate::miniapps::realtime::derive_topic_id(
                &instance.package.manifest.name,
                &instance.chat_id,
                &instance.message_id,
            )
        };

        let topic_encoded = crate::miniapps::realtime::encode_topic_id(&topic);

        // If channel already active, just return the topic (skip re-join)
        if state.has_realtime_channel(&miniapp_id).await {
            info!("[WEBXDC] Android: Realtime channel already active for: {}", miniapp_id);
            return Ok((None, topic, topic_encoded));
        }

        // Set channel state immediately so sendRealtimeData can find the topic
        state.set_realtime_channel(&miniapp_id, crate::miniapps::state::RealtimeChannelState {
            topic,
            active: true,
        }).await;

        Ok::<_, String>((Some(instance), topic, topic_encoded))
    });
    drop(rt);

    let (instance_opt, topic, topic_encoded) = match setup {
        Ok(r) => r,
        Err(e) => {
            error!("[{}] joinRealtimeChannel setup failed: {}", miniapp_id, e);
            return std::ptr::null_mut();
        }
    };

    // If already active, return existing topic without spawning new tasks
    let instance = match instance_opt {
        Some(inst) => inst,
        None => {
            return match env.new_string(&topic_encoded) {
                Ok(s) => s.into_raw(),
                Err(_) => std::ptr::null_mut(),
            };
        }
    };

    info!("[{}] Joining realtime channel with topic: {}", miniapp_id, topic_encoded);

    // Create bounded mpsc channel for event delivery to Android WebView
    let (tx, rx) = tokio::sync::mpsc::channel::<crate::miniapps::realtime::RealtimeEvent>(256);

    // Spawn the async join work on the Tauri runtime
    let app_for_join = app.clone();
    let miniapp_id_for_join = miniapp_id.clone();
    let topic_encoded_for_join = topic_encoded.clone();
    tauri::async_runtime::spawn(async move {
        let state = app_for_join.state::<crate::miniapps::state::MiniAppsState>();

        // Initialize Iroh
        let iroh = match state.realtime.get_or_init().await {
            Ok(iroh) => iroh,
            Err(e) => {
                error!("[WEBXDC] Android: Failed to initialize Iroh: {}", e);
                // Clean up the channel state we set synchronously
                state.remove_realtime_channel(&miniapp_id_for_join).await;
                return;
            }
        };

        // Join the channel with mpsc event target
        let event_target = crate::miniapps::realtime::EventTarget::MpscSender(tx);
        match iroh.join_channel(topic, vec![], event_target, Some(app_for_join.clone())).await {
            Ok((is_rejoin, _)) => {
                if is_rejoin {
                    info!("[WEBXDC] Android: Re-joined existing channel for topic: {}", topic_encoded_for_join);
                } else {
                    info!("[WEBXDC] Android: Joined new channel for topic: {}", topic_encoded_for_join);
                }
            }
            Err(e) => {
                error!("[WEBXDC] Android: Failed to join channel: {}", e);
                // Clean up the channel state we set synchronously
                state.remove_realtime_channel(&miniapp_id_for_join).await;
                return;
            }
        }

        // Process pending peers
        let pending_peers = state.take_pending_peers(&topic).await;
        let pending_peer_count = pending_peers.len();
        if !pending_peers.is_empty() {
            info!("[WEBXDC] Android: Adding {} pending peers", pending_peer_count);
            for pending in pending_peers {
                let node_id = pending.node_addr.node_id;
                if let Err(e) = iroh.add_peer(topic, pending.node_addr).await {
                    warn!("[WEBXDC] Android: Failed to add pending peer {}: {}", node_id, e);
                }
            }
        }

        // Get node address and send peer advertisements
        match iroh.get_node_addr().await {
            Ok(node_addr) => {
                match crate::miniapps::realtime::encode_node_addr(&node_addr) {
                    Ok(node_addr_encoded) => {
                        let chat_id = instance.chat_id.clone();
                        let topic_for_ad = topic_encoded_for_join.clone();
                        let addr_for_ad = node_addr_encoded.clone();

                        // Send initial advertisement
                        let chat_id_1 = chat_id.clone();
                        let topic_1 = topic_for_ad.clone();
                        let addr_1 = addr_for_ad.clone();
                        tokio::spawn(async move {
                            crate::commands::realtime::send_webxdc_peer_advertisement(
                                chat_id_1, topic_1, addr_1,
                            ).await;
                        });

                        // Send delayed advertisement
                        let chat_id_2 = chat_id;
                        let topic_2 = topic_for_ad;
                        let addr_2 = addr_for_ad;
                        tokio::spawn(async move {
                            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                            crate::commands::realtime::send_webxdc_peer_advertisement(
                                chat_id_2, topic_2, addr_2,
                            ).await;
                        });
                    }
                    Err(e) => {
                        warn!("[WEBXDC] Android: Failed to encode node addr: {}", e);
                    }
                }
            }
            Err(e) => {
                warn!("[WEBXDC] Android: Failed to get node addr: {}", e);
            }
        }

        // Emit status event to main window so UI updates to show "Playing"
        if let Some(main_window) = app_for_join.get_webview_window("main") {
            let current_peer_count = iroh.get_peer_count(&topic).await;
            let effective_peer_count = std::cmp::max(current_peer_count, pending_peer_count);
            let _ = main_window.emit("miniapp_realtime_status", serde_json::json!({
                "topic": topic_encoded_for_join,
                "peer_count": effective_peer_count,
                "is_active": true,
            }));
        }
    });

    // Spawn the delivery task that forwards events to Android WebView via JNI
    tauri::async_runtime::spawn(android_realtime_delivery_loop(rx));

    // Return topic ID to Kotlin
    match env.new_string(&topic_encoded) {
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

    // Validate data size
    if bytes.len() > REALTIME_DATA_MAX_SIZE {
        error!("[{}] sendRealtimeData: data too large ({} bytes)", miniapp_id, bytes.len());
        return;
    }

    let app = match TAURI_APP.get() {
        Some(a) => a.clone(),
        None => {
            error!("[{}] TAURI_APP not initialized", miniapp_id);
            return;
        }
    };

    let miniapp_id_owned = miniapp_id;
    let data = bytes;
    tauri::async_runtime::spawn(async move {
        let state = app.state::<crate::miniapps::state::MiniAppsState>();

        // Get the topic for this instance
        let topic = match state.get_realtime_channel(&miniapp_id_owned).await {
            Some(t) => t,
            None => {
                warn!("[WEBXDC] Android sendRealtimeData: no active channel for {}", miniapp_id_owned);
                return;
            }
        };

        // Send via Iroh
        let iroh = match state.realtime.get_or_init().await {
            Ok(iroh) => iroh,
            Err(e) => {
                error!("[WEBXDC] Android sendRealtimeData: failed to get Iroh: {}", e);
                return;
            }
        };

        if let Err(e) = iroh.send_data(topic, data).await {
            error!("[WEBXDC] Android sendRealtimeData: failed to send: {}", e);
        }
    });
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

    let app = match TAURI_APP.get() {
        Some(a) => a.clone(),
        None => {
            error!("[{}] TAURI_APP not initialized", miniapp_id);
            return;
        }
    };

    let miniapp_id_owned = miniapp_id;
    tauri::async_runtime::spawn(async move {
        let state = app.state::<crate::miniapps::state::MiniAppsState>();

        // Remove and leave the realtime channel
        if let Some(channel_state) = state.remove_realtime_channel(&miniapp_id_owned).await {
            let iroh = match state.realtime.get_or_init().await {
                Ok(iroh) => iroh,
                Err(e) => {
                    error!("[WEBXDC] Android leaveRealtimeChannel: failed to get Iroh: {}", e);
                    return;
                }
            };

            if let Err(e) = iroh.leave_channel(channel_state.topic).await {
                error!("[WEBXDC] Android leaveRealtimeChannel: failed to leave: {}", e);
            } else {
                info!("[WEBXDC] Android: Left realtime channel for {}", miniapp_id_owned);
            }
        }
    });
}

/// Get the user's npub.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppIpc_getSelfAddrNative(
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    let env = env;
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
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    let env = env;
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
    _miniapp_id: JString,
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
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    let env = env;
    let npub = get_user_npub();
    match env.new_string(&npub) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Get the user's display name (also used by WebViewClient).
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppWebViewClient_getSelfNameNative(
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    let env = env;
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
    if let Some(&pubkey) = crate::MY_PUBLIC_KEY.get() {
        nostr_sdk::prelude::ToBech32::to_bech32(&pubkey)
            .unwrap_or_else(|_| "unknown".to_string())
    } else {
        "unknown".to_string()
    }
}

fn get_user_display_name() -> String {
    // Try to get display name from STATE
    if let Ok(state) = crate::STATE.try_lock() {
        if let Some(profile) = state.profiles.iter().find(|p| p.flags.is_mine()) {
            if !profile.nickname.is_empty() {
                return profile.nickname.to_string();
            } else if !profile.name.is_empty() {
                return profile.name.to_string();
            }
        }
    }
    "Unknown".to_string()
}

fn get_granted_permissions_for_package(package_path: &str) -> Result<String, String> {
    // Compute file hash for permission lookup - fs::read fails with NotFound if missing
    let bytes = std::fs::read(package_path).map_err(|e| format!("Failed to read package: {}", e))?;

    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let file_hash = bytes_to_hex_string(hasher.finalize().as_slice());

    // Look up permissions from database using file_hash
    let handle = TAURI_APP.get().ok_or("Tauri app not initialized")?;
    crate::db::get_miniapp_granted_permissions(handle, &file_hash)
}

fn serve_file_from_package(
    env: &mut JNIEnv,
    package_path: &str,
    path: &str,
) -> Result<jobject, String> {
    let file = std::fs::File::open(package_path).map_err(|e| format!("Failed to open package: {}", e))?;
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

    // Permissions-Policy (must be on document response to take effect)
    let pp_key = env.new_string("Permissions-Policy").map_err(|e| format!("{:?}", e))?;
    let pp_val = env.new_string(PERMISSIONS_POLICY_HEADER).map_err(|e| format!("{:?}", e))?;
    unsafe {
        env.call_method_unchecked(
            &headers,
            put_method,
            jni::signature::ReturnType::Object,
            &[
                jni::sys::jvalue { l: pp_key.into_raw() },
                jni::sys::jvalue { l: pp_val.into_raw() },
            ],
        )
        .map_err(|e| format!("Failed to put Permissions-Policy header: {:?}", e))?;
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

// ============================================================================
// Android Realtime Delivery
// ============================================================================

/// Background task that reads RealtimeEvents from the bounded mpsc channel
/// and delivers Data events to the Android WebView via JNI.
/// Status events (Connected, PeerJoined, PeerLeft) are already handled
/// by emit_realtime_status() in the subscribe loop.
///
/// Resilience: JNI panics are caught so a single bad delivery can't kill the
/// loop. Consecutive failures trigger exponential backoff (100ms → 200ms → …
/// capped at 2s) to avoid hammering a broken JNI bridge. The counter resets
/// on any successful delivery.
async fn android_realtime_delivery_loop(
    mut rx: tokio::sync::mpsc::Receiver<crate::miniapps::realtime::RealtimeEvent>,
) {
    info!("[WEBXDC] Android realtime delivery loop started");
    let mut consecutive_failures: u32 = 0;

    while let Some(event) = rx.recv().await {
        if let crate::miniapps::realtime::RealtimeEvent::Data(data) = event {
            // catch_unwind prevents a JNI panic from killing the delivery loop.
            // The JNI call is synchronous so this is safe (no async across unwind).
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::miniapp::send_realtime_data_to_miniapp(&data)
            }));

            match result {
                Ok(Ok(())) => {
                    consecutive_failures = 0;
                }
                Ok(Err(e)) => {
                    consecutive_failures += 1;
                    warn!("[WEBXDC] Android: Failed to deliver data to WebView: {} (failures: {})", e, consecutive_failures);
                }
                Err(_) => {
                    consecutive_failures += 1;
                    error!("[WEBXDC] Android: JNI delivery panicked, recovering (failures: {})", consecutive_failures);
                }
            }

            // Exponential backoff on consecutive failures to avoid spin-looping
            if consecutive_failures > 0 {
                let delay_ms = std::cmp::min(100u64 << (consecutive_failures - 1), 2000);
                tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            }
        }
        // Connected, PeerJoined, PeerLeft are handled by emit_realtime_status
    }
    info!("[WEBXDC] Android realtime delivery loop ended (channel closed)");
}
