//! JNI callback functions for Mini Apps.
//!
//! This module provides the Kotlin → Rust JNI bridge. These functions are
//! called by the Kotlin code (MiniAppManager, MiniAppIpc, MiniAppWebViewClient)
//! and routed to the appropriate Rust handlers.

use jni::objects::{JClass, JString};
use jni::sys::{jint, jstring, jobject};
use jni::JNIEnv;
use std::io::Read;
use tauri::{Emitter, Manager};
use nostr_sdk::prelude::ToBech32;
use crate::util::bytes_to_hex_string;
use crate::TAURI_APP;

// ============================================================================
// Constants
// ============================================================================

/// Content Security Policy for Mini Apps.
/// - `default-src 'self'`: Only allow resources from same origin (webxdc.localhost)
/// - `webrtc 'block'`: Prevent IP leaks via WebRTC
/// - `unsafe-inline/eval`: Required for many Mini Apps to function
const CSP_HEADER: &str = r#"default-src 'self' http://webxdc.localhost; style-src 'self' http://webxdc.localhost 'unsafe-inline' blob:; font-src 'self' http://webxdc.localhost data: blob:; script-src 'self' http://webxdc.localhost 'unsafe-inline' 'unsafe-eval' blob:; connect-src 'self' http://webxdc.localhost ws://127.0.0.1:* ipc: data: blob:; img-src 'self' http://webxdc.localhost data: blob:; media-src 'self' http://webxdc.localhost data: blob:; webrtc 'block'"#;

/// Permissions Policy for Mini Apps (Android document responses).
/// Autoplay is allowed (self) for video streaming in Mini Apps.
/// Must be on the document response to take effect (not subresource responses).
const PERMISSIONS_POLICY_HEADER: &str = "accelerometer=(), ambient-light-sensor=(), autoplay=(self), battery=(), bluetooth=(), camera=(), clipboard-read=(), clipboard-write=(), display-capture=(), fullscreen=(), geolocation=(), gyroscope=(), magnetometer=(), microphone=(), midi=(), payment=(), picture-in-picture=(), screen-wake-lock=(), speaker-selection=(), usb=(), web-share=(), xr-spatial-tracking=()";

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
    let _miniapp_id: String = match env.get_string(&miniapp_id) {
        Ok(s) => s.into(),
        Err(e) => {
            log_error!("Failed to get miniapp_id: {:?}", e);
            return;
        }
    };

    let _chat_id: String = match env.get_string(&chat_id) {
        Ok(s) => s.into(),
        Err(e) => {
            log_error!("Failed to get chat_id: {:?}", e);
            return;
        }
    };

    let _message_id: String = match env.get_string(&message_id) {
        Ok(s) => s.into(),
        Err(e) => {
            log_error!("Failed to get message_id: {:?}", e);
            return;
        }
    };

    log_info!(
        "Mini App opened (JNI callback): {} (chat: {}, message: {})",
        _miniapp_id, _chat_id, _message_id
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
            log_error!("Failed to get miniapp_id: {:?}", e);
            return;
        }
    };

    log_info!("Mini App closed (JNI callback): {}", miniapp_id);

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
                        iroh.leave_channel(channel.topic, &miniapp_id_owned),
                    ).await {
                        Ok(Ok(())) => {
                            log_info!("[WEBXDC] Left Iroh channel on Mini App close: {}", miniapp_id_owned);
                        }
                        Ok(Err(e)) => {
                            log_warn!("[WEBXDC] Failed to leave Iroh channel on close: {}", e);
                        }
                        Err(_) => {
                            log_warn!("[WEBXDC] Timed out leaving Iroh channel on close: {}", miniapp_id_owned);
                        }
                    }

                    count
                } else {
                    0
                };

                // Remove ourselves from session peers
                if let Some(my_pk) = crate::MY_PUBLIC_KEY.get() {
                    let my_npub = my_pk.to_bech32().unwrap();
                    state.remove_session_peer(&channel.topic, &my_npub).await;
                }

                // Emit status update — session_peers is the single source of truth
                let session_peers = state.get_session_peers(&channel.topic).await;
                let session_count = session_peers.len();
                if let Some(main_window) = app.get_webview_window("main") {
                    let _ = main_window.emit("miniapp_realtime_status", serde_json::json!({
                        "topic": topic_encoded,
                        "peer_count": session_count,
                        "peers": session_peers,
                        "is_active": false,
                        "has_pending_peers": session_count > 0,
                    }));
                    log_info!("[WEBXDC] Emitted miniapp_realtime_status: active=false, peer_count={} for topic {}", session_count, topic_encoded);
                }
                // Send peer-left signal so other clients update their online indicators
                if let Some(instance) = state.get_instance(&miniapp_id_owned).await {
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
            state.remove_instance(&miniapp_id_owned).await;
        });
    }
}

/// Called when a Mini App's renderer process crashes.
/// Emits an event to the frontend so it can show a toast notification.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppManager_onMiniAppCrashed(
    mut env: JNIEnv,
    _class: JClass,
    miniapp_id: JString,
) {
    let miniapp_id: String = match env.get_string(&miniapp_id) {
        Ok(s) => s.into(),
        Err(e) => {
            log_error!("Failed to get miniapp_id: {:?}", e);
            return;
        }
    };

    log_error!("Mini App crashed (renderer process gone): {}", miniapp_id);

    if let Some(app) = TAURI_APP.get() {
        if let Some(main_window) = app.get_webview_window("main") {
            let _ = main_window.emit("miniapp_crashed", serde_json::json!({
                "miniapp_id": miniapp_id,
            }));
        }
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

    let _args: String = match env.get_string(&args) {
        Ok(s) => s.into(),
        Err(e) => return create_error_string(&mut env, &format!("Failed to get args: {:?}", e)),
    };

    log_debug!("[{}] invokeNative: {} (args: {})", miniapp_id, command, _args);

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
            log_warn!("[{}] Unknown command: {}", miniapp_id, command);
            format!(r#"{{"error":"Unknown command: {}"}}"#, command)
        }
    };

    match env.new_string(&result) {
        Ok(s) => s.into_raw(),
        Err(e) => {
            log_error!("Failed to create result string: {:?}", e);
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
    let _miniapp_id: String = match env.get_string(&miniapp_id) {
        Ok(s) => s.into(),
        Err(e) => {
            log_error!("Failed to get miniapp_id: {:?}", e);
            return;
        }
    };

    let _update: String = match env.get_string(&update) {
        Ok(s) => s.into(),
        Err(e) => {
            log_error!("Failed to get update: {:?}", e);
            return;
        }
    };

    let _description: String = match env.get_string(&description) {
        Ok(s) => s.into(),
        Err(e) => {
            log_error!("Failed to get description: {:?}", e);
            return;
        }
    };

    log_info!(
        "[{}] sendUpdate: {} ({})",
        _miniapp_id, _description, _update
    );

    // TODO: Store update and broadcast to other participants
}

/// Get updates since a serial number.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppIpc_getUpdatesNative(
    mut env: JNIEnv,
    _class: JClass,
    miniapp_id: JString,
    _last_known_serial: jint,
) -> jstring {
    let _miniapp_id: String = match env.get_string(&miniapp_id) {
        Ok(s) => s.into(),
        Err(e) => return create_error_string(&mut env, &format!("Failed to get miniapp_id: {:?}", e)),
    };

    log_debug!(
        "[{}] getUpdates since serial: {}",
        _miniapp_id, _last_known_serial
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
            log_error!("Failed to get miniapp_id: {:?}", e);
            return std::ptr::null_mut();
        }
    };

    log_info!("[{}] joinRealtimeChannel", miniapp_id);

    let app = match TAURI_APP.get() {
        Some(a) => a.clone(),
        None => {
            log_error!("[{}] TAURI_APP not initialized", miniapp_id);
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
            log_error!("[{}] Failed to create tokio runtime: {:?}", miniapp_id, e);
            return std::ptr::null_mut();
        }
    };

    let state = app.state::<crate::miniapps::state::MiniAppsState>();

    // Read instance, derive topic, set channel state, and eagerly init Iroh + WS server
    // synchronously. Setting channel state before async join prevents races.
    // Iroh init here ensures the WS URL is available for the return value.
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
            log_info!("[WEBXDC] Android: Realtime channel already active for: {}", miniapp_id);
            let ws_url = state.realtime.ws_url();
            return Ok((None, topic, topic_encoded, ws_url));
        }

        // Set channel state immediately
        state.set_realtime_channel(&miniapp_id, crate::miniapps::state::RealtimeChannelState {
            topic,
            active: true,
        }).await;

        // NOTE: Do NOT call get_or_init() here! This block runs on a temporary
        // single-threaded tokio runtime (rt) that is dropped after block_on().
        // Any tasks spawned by Endpoint::bind() would be killed when rt is dropped.
        // Iroh init happens in the tauri::async_runtime::spawn block below instead.
        //
        // BUT: start the WS server eagerly (sync bind + spawn on main runtime).
        // This ensures ws_url is available BEFORE returning to JS.
        state.realtime.ensure_ws_started();
        let ws_url = state.realtime.ws_url();

        Ok::<_, String>((Some(instance), topic, topic_encoded, ws_url))
    });
    drop(rt);

    let (instance_opt, topic, topic_encoded, ws_url) = match setup {
        Ok(r) => r,
        Err(e) => {
            log_error!("[{}] joinRealtimeChannel setup failed: {}", miniapp_id, e);
            return std::ptr::null_mut();
        }
    };

    // If already active, return existing result without spawning new tasks
    let instance = match instance_opt {
        Some(inst) => inst,
        None => {
            let result = serde_json::json!({ "topic": topic_encoded, "ws_url": ws_url, "label": miniapp_id });
            return match env.new_string(&result.to_string()) {
                Ok(s) => s.into_raw(),
                Err(_) => std::ptr::null_mut(),
            };
        }
    };

    log_info!("[{}] Joining realtime channel with topic: {}", miniapp_id, topic_encoded);

    // Create bounded mpsc channel for event delivery to Android WebView
    let (tx, rx) = tokio::sync::mpsc::channel::<crate::miniapps::realtime::RealtimeEvent>(256);

    // Spawn the async join work on the Tauri runtime
    let app_for_join = app.clone();
    let miniapp_id_for_join = miniapp_id.clone();
    let topic_encoded_for_join = topic_encoded.clone();
    tauri::async_runtime::spawn(async move {
        let state = app_for_join.state::<crate::miniapps::state::MiniAppsState>();

        // Wait for preconnect to finish (if it ran for this Mini App)
        if let Some(mut rx) = state.take_preconnect_signal(&miniapp_id_for_join).await {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                rx.wait_for(|ready| *ready),
            ).await;
        }

        // Initialize Iroh (instant if preconnect already ran)
        let iroh = match state.realtime.get_or_init().await {
            Ok(iroh) => iroh,
            Err(e) => {
                log_error!("[WEBXDC] Android: Failed to initialize Iroh: {}", e);
                // Clean up the channel state we set synchronously
                state.remove_realtime_channel(&miniapp_id_for_join).await;
                return;
            }
        };

        // Join the channel with mpsc event target
        let event_target = crate::miniapps::realtime::EventTarget::MpscSender(tx);
        let ws_targets = Some(state.realtime.ws_senders.clone());
        let is_rejoin = match iroh.join_channel(topic, vec![], Some(event_target), Some(app_for_join.clone()), miniapp_id_for_join.clone(), ws_targets).await {
            Ok((rejoin, _)) => {
                if rejoin {
                    log_info!("[WEBXDC] Android: Re-joined existing channel for topic: {}", topic_encoded_for_join);
                } else {
                    log_info!("[WEBXDC] Android: Joined new channel for topic: {}", topic_encoded_for_join);
                }
                rejoin
            }
            Err(e) => {
                log_error!("[WEBXDC] Android: Failed to join channel: {}", e);
                state.remove_realtime_channel(&miniapp_id_for_join).await;
                return;
            }
        };

        // Only connect peers + send advertisement if preconnect didn't
        if !is_rejoin {
            let cached_addrs = state.take_peer_addrs(&topic).await;
            if !cached_addrs.is_empty() {
                log_info!("[WEBXDC] Android: Connecting to {} cached peers", cached_addrs.len());
                for addr in cached_addrs {
                    let peer_id = addr.id;
                    if let Err(e) = iroh.add_peer(topic, addr).await {
                        log_warn!("[WEBXDC] Android: Failed to connect to cached peer {}: {}", peer_id, e);
                    }
                }
            }
        }

        // Get node address and send peer advertisements
        if !is_rejoin {
            let node_addr = iroh.get_node_addr();
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
                    log_warn!("[WEBXDC] Android: Failed to encode node addr: {}", e);
                }
            }
        } // if !is_rejoin (advertisement)

        // Add ourselves to session peers
        if let Some(my_pk) = crate::MY_PUBLIC_KEY.get() {
            let my_npub = my_pk.to_bech32().unwrap();
            state.add_session_peer(topic, my_npub).await;
        }

        // Emit status event — session_peers is the single source of truth
        if let Some(main_window) = app_for_join.get_webview_window("main") {
            let session_peers = state.get_session_peers(&topic).await;
            let peer_count = session_peers.len();
            let _ = main_window.emit("miniapp_realtime_status", serde_json::json!({
                "topic": topic_encoded_for_join,
                "peer_count": peer_count,
                "peers": session_peers,
                "is_active": true,
            }));
        }
    });

    // Spawn the delivery task that forwards events to Android WebView via JNI
    tauri::async_runtime::spawn(android_realtime_delivery_loop(rx));

    // Return JSON with topic + WS URL + label to Kotlin/JS
    let result = serde_json::json!({ "topic": topic_encoded, "ws_url": ws_url, "label": miniapp_id });
    match env.new_string(&result.to_string()) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
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
            log_error!("Failed to get miniapp_id: {:?}", e);
            return;
        }
    };

    log_info!("[{}] leaveRealtimeChannel", miniapp_id);

    let app = match TAURI_APP.get() {
        Some(a) => a.clone(),
        None => {
            log_error!("[{}] TAURI_APP not initialized", miniapp_id);
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
                    log_error!("[WEBXDC] Android leaveRealtimeChannel: failed to get Iroh: {}", e);
                    return;
                }
            };

            if let Err(e) = iroh.leave_channel(channel_state.topic, &miniapp_id_owned).await {
                log_error!("[WEBXDC] Android leaveRealtimeChannel: failed to leave: {}", e);
            } else {
                log_info!("[WEBXDC] Android: Left realtime channel for {}", miniapp_id_owned);
            }
        }
    });
}

/// Send realtime data via gossip (JNI bridge fallback when WS isn't available).
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppIpc_sendRealtimeDataNative(
    mut env: JNIEnv,
    _class: JClass,
    miniapp_id: JString,
    data: JString,
) {
    let miniapp_id: String = match env.get_string(&miniapp_id) {
        Ok(s) => s.into(),
        Err(e) => {
            log_error!("Failed to get miniapp_id: {:?}", e);
            return;
        }
    };

    let encoded: String = match env.get_string(&data) {
        Ok(s) => s.into(),
        Err(e) => {
            log_error!("Failed to get data string: {:?}", e);
            return;
        }
    };

    // Decode base91 to raw bytes
    let bytes = match fast_thumbhash::base91_decode(&encoded) {
        Ok(b) => b,
        Err(_) => {
            log_error!("[{}] sendRealtimeData: failed to decode base91", miniapp_id);
            return;
        }
    };

    if bytes.len() > 128_000 {
        log_error!("[{}] sendRealtimeData: data too large ({} bytes)", miniapp_id, bytes.len());
        return;
    }

    let app = match TAURI_APP.get() {
        Some(a) => a.clone(),
        None => {
            log_error!("[{}] TAURI_APP not initialized", miniapp_id);
            return;
        }
    };

    let data = bytes;
    tauri::async_runtime::spawn(async move {
        let state = app.state::<crate::miniapps::state::MiniAppsState>();

        let topic = match state.get_realtime_channel(&miniapp_id).await {
            Some(t) => t,
            None => {
                log_warn!("[WEBXDC] Android sendRealtimeData: no active channel for {}", miniapp_id);
                return;
            }
        };

        let iroh = match state.realtime.get_or_init().await {
            Ok(iroh) => iroh,
            Err(e) => {
                log_error!("[WEBXDC] Android sendRealtimeData: failed to get Iroh: {}", e);
                return;
            }
        };

        if let Err(e) = iroh.send_data(topic, data).await {
            log_error!("[WEBXDC] Android sendRealtimeData: failed to send: {}", e);
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
            log_error!("Failed to get package_path: {:?}", e);
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
            log_error!("Failed to get miniapp_id: {:?}", e);
            return std::ptr::null_mut();
        }
    };

    let package_path: String = match env.get_string(&package_path) {
        Ok(s) => s.into(),
        Err(e) => {
            log_error!("Failed to get package_path: {:?}", e);
            return std::ptr::null_mut();
        }
    };

    let path: String = match env.get_string(&path) {
        Ok(s) => s.into(),
        Err(e) => {
            log_error!("Failed to get path: {:?}", e);
            return std::ptr::null_mut();
        }
    };

    log_debug!("[{}] handleMiniAppRequest: {}", miniapp_id, path);

    // Serve file from .xdc package
    match serve_file_from_package(&mut env, &package_path, &path) {
        Ok(response) => response,
        Err(e) => {
            log_error!("[{}] Failed to serve {}: {}", miniapp_id, path, e);
            std::ptr::null_mut()
        }
    }
}

/// Generate the webxdc bridge JavaScript.
/// Single source of truth — used by both inline HTML injection and /webxdc.js requests.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_miniapp_MiniAppWebViewClient_generateBridgeJsNative(
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    let self_addr = get_user_npub();
    let self_name = get_user_display_name();
    let js = generate_android_webxdc_bridge(&self_addr, &self_name);

    match env.new_string(&js) {
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
    // STATE is a tokio::sync::Mutex — .lock() requires .await which we can't use
    // from a synchronous JNI thread. Handle::current() also panics here because
    // WebView's shouldInterceptRequest thread has no tokio runtime.
    // Retry try_lock() briefly to ride out transient lock contention on slow devices.
    for _ in 0..10 {
        if let Ok(state) = crate::STATE.try_lock() {
            if let Some(profile) = state.profiles.iter().find(|p| p.flags.is_mine()) {
                if !profile.nickname.is_empty() {
                    return profile.nickname.to_string();
                } else if !profile.name.is_empty() {
                    return profile.name.to_string();
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    "Unknown".to_string()
}

fn get_granted_permissions_for_package(package_path: &str) -> Result<String, String> {
    // Compute file hash for permission lookup
    let bytes = std::fs::read(package_path).map_err(|e| format!("Failed to read package: {}", e))?;

    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let file_hash = bytes_to_hex_string(&hasher.finalize());

    // Look up permissions from database using file_hash
    crate::db::get_miniapp_granted_permissions(&file_hash)
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

    // For HTML files, inject the webxdc bridge INLINE to ensure window.webxdc is available
    // before any game scripts run. This mirrors the desktop inject_webxdc_script approach.
    // A <script src="/webxdc.js"> tag approach is unreliable on Android WebView due to
    // caching and request interception timing.
    if mime_type == "text/html" {
        let html = String::from_utf8_lossy(&contents);

        let self_addr = get_user_npub();
        let self_name = get_user_display_name();
        let bridge_js = generate_android_webxdc_bridge(&self_addr, &self_name);
        let script_block = format!("<script>{}</script>", bridge_js);

        // Case-insensitive search on raw bytes to avoid to_lowercase() byte-length
        // mismatches with multi-byte Unicode characters before the tag.
        let html_bytes = html.as_bytes();
        let injected = if let Some(head_pos) = find_tag_ci(html_bytes, b"<head>") {
            let insert_pos = head_pos + b"<head>".len();
            log_info!("[MiniApp] Injected webxdc bridge after <head> for: {}", file_path);
            format!("{}{}{}", &html[..insert_pos], script_block, &html[insert_pos..])
        } else if let Some(html_pos) = find_tag_ci(html_bytes, b"<html") {
            if let Some(close) = html[html_pos..].find('>') {
                let insert_pos = html_pos + close + 1;
                log_info!("[MiniApp] Injected webxdc bridge after <html> for: {}", file_path);
                format!("{}{}{}", &html[..insert_pos], script_block, &html[insert_pos..])
            } else {
                log_info!("[MiniApp] Injected webxdc bridge at start for: {}", file_path);
                format!("{}{}", script_block, html)
            }
        } else {
            log_info!("[MiniApp] Injected webxdc bridge at start (no head/html tag) for: {}", file_path);
            format!("{}{}", script_block, html)
        };
        contents = injected.into_bytes();
    }

    // Create WebResourceResponse with security headers
    create_web_resource_response(env, &mime_type, &contents, CSP_HEADER)
}

/// Case-insensitive byte search for ASCII HTML tags.
/// Returns the byte offset in `haystack` where `needle` starts.
/// Uses raw bytes to avoid `to_lowercase()` byte-length mismatches with Unicode.
fn find_tag_ci(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
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

    // Cache-Control: prevent WebView from caching stale content
    let cc_key = env.new_string("Cache-Control").map_err(|e| format!("{:?}", e))?;
    let cc_val = env.new_string("no-cache, no-store").map_err(|e| format!("{:?}", e))?;
    unsafe {
        env.call_method_unchecked(
            &headers,
            put_method,
            jni::signature::ReturnType::Object,
            &[
                jni::sys::jvalue { l: cc_key.into_raw() },
                jni::sys::jvalue { l: cc_val.into_raw() },
            ],
        )
        .map_err(|e| format!("Failed to put Cache-Control header: {:?}", e))?;
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
    log_info!("[WEBXDC] Android realtime delivery loop started");
    let mut consecutive_failures: u32 = 0;

    while let Some(event) = rx.recv().await {
        if let crate::miniapps::realtime::RealtimeEvent::Data(encoded) = event {
            // Pass base91-encoded string directly to the WebView (JS decodes with b91d).
            // This avoids the decode→re-encode overhead of the old base64 path.
            // catch_unwind prevents a JNI panic from killing the delivery loop.
            // The JNI call is synchronous so this is safe (no async across unwind).
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::miniapp::send_realtime_data_to_miniapp(&encoded)
            }));

            match result {
                Ok(Ok(())) => {
                    consecutive_failures = 0;
                }
                Ok(Err(e)) => {
                    consecutive_failures += 1;
                    log_warn!("[WEBXDC] Android: Failed to deliver data to WebView: {} (failures: {})", e, consecutive_failures);
                }
                Err(_) => {
                    consecutive_failures += 1;
                    log_error!("[WEBXDC] Android: JNI delivery panicked, recovering (failures: {})", consecutive_failures);
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
    log_info!("[WEBXDC] Android realtime delivery loop ended (channel closed)");
}

/// Generate the Android webxdc bridge JavaScript for inline injection into HTML files.
/// Uses `__MINIAPP_IPC__` (JNI JavascriptInterface) instead of `__TAURI__`.
fn generate_android_webxdc_bridge(self_addr: &str, self_name: &str) -> String {
    // Escape for JS string literals inside an inline <script> block:
    // 1. Backslash and double-quote for JS string safety
    // 2. Newlines/carriage returns to prevent string literal breaks
    // 3. </ to prevent </script> from closing the enclosing script tag (XSS)
    let addr_escaped = self_addr.replace('\\', "\\\\").replace('"', "\\\"").replace("</", "<\\/");
    let name_escaped = self_name.replace('\\', "\\\\").replace('"', "\\\"")
        .replace('\n', "\\n").replace('\r', "\\r").replace("</", "<\\/");

    format!(r#"
(function() {{
    'use strict';

    // base91 codec (matches Rust fast-thumbhash alphabet, ~14% overhead vs base64's 33%)
    var B91='ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!#$%&()*+,./:;<=>?@[]^_`{{|}}~ ';
    var B91D=new Uint8Array(256);B91D.fill(255);for(var _i=0;_i<91;_i++)B91D[B91.charCodeAt(_i)]=_i;
    function b91e(buf){{var o='',n=0,b=0;for(var i=0;i<buf.length;i++){{n|=buf[i]<<b;b+=8;if(b>13){{var v=n&8191;if(v>88){{n>>=13;b-=13;}}else{{v=n&16383;n>>=14;b-=14;}}o+=B91[v%91]+B91[v/91|0];}}}}if(b>0){{o+=B91[n%91];if(b>7||n>90)o+=B91[n/91|0];}}return o;}}
    function b91d(s){{var o=[],n=0,b=0,q=-1;for(var i=0;i<s.length;i++){{var d=B91D[s.charCodeAt(i)];if(d===255)continue;if(q<0){{q=d;}}else{{var v=q+d*91;q=-1;n|=v<<b;b+=(v&8191)>88?13:14;while(b>=8){{o.push(n&255);n>>=8;b-=8;}}}}}}if(q>=0)o.push((n|(q<<b))&255);return new Uint8Array(o);}}

    var selfAddr = "{addr}";
    var selfName = "{name}";
    var updateListener = null;
    var lastKnownSerial = 0;

    // Global decoder for incoming realtime data
    window.__miniapp_b91d = b91d;

    // Realtime data notification handler: called from a tiny evaluateJavascript
    // notification. Pulls queued data from native via JNI (runs on background thread),
    // avoiding 170KB+ JS compilations on the UI thread that starve the video compositor.
    window.__miniapp_rt_notify = function() {{
        try {{
            var json = window.__MINIAPP_IPC__.pollRealtimeData();
            if (!json) return;
            var items = JSON.parse(json);
            var listener = window.__miniapp_realtime_listener;
            if (!listener) return;
            var decode = b91d;
            for (var i = 0; i < items.length; i++) {{
                listener(decode(items[i]));
            }}
        }} catch(e) {{
            console.error('rt_notify error:', e);
        }}
    }};

    window.webxdc = {{
        selfAddr: selfAddr,
        selfName: selfName,

        setUpdateListener: function(listener, serial) {{
            updateListener = listener;
            lastKnownSerial = serial || 0;
            try {{
                var updates = window.__MINIAPP_IPC__.getUpdates(lastKnownSerial);
                if (updates && updateListener) {{
                    var parsed = JSON.parse(updates);
                    parsed.forEach(function(update) {{
                        updateListener(update);
                    }});
                }}
            }} catch(e) {{
                console.error('Failed to get updates:', e);
            }}
            return Promise.resolve();
        }},

        sendUpdate: function(update, description) {{
            return new Promise(function(resolve, reject) {{
                try {{
                    window.__MINIAPP_IPC__.sendUpdate(
                        JSON.stringify(update),
                        description || ''
                    );
                    resolve();
                }} catch(e) {{
                    reject(e);
                }}
            }});
        }},

        sendToChat: function() {{
            return Promise.reject(new Error('Not implemented'));
        }},

        importFiles: function() {{
            return Promise.reject(new Error('Not implemented'));
        }},

        joinRealtimeChannel: function() {{
            var rtWs = null;
            var channel = {{
                _listener: null,
                setListener: function(listener) {{
                    this._listener = listener;
                    window.__miniapp_realtime_listener = listener;
                }},
                send: function(data) {{
                    var buf = data instanceof Uint8Array ? data : new Uint8Array(data);
                    // Fast path: WebSocket binary frame
                    if (rtWs && rtWs.readyState === 1) {{
                        rtWs.send(buf);
                    }} else {{
                        // Fallback: JNI bridge (always available on Android)
                        try {{
                            window.__MINIAPP_IPC__.sendRealtimeData(b91e(buf));
                        }} catch(e) {{}}
                    }}
                }},
                leave: function() {{
                    this._listener = null;
                    window.__miniapp_realtime_listener = null;
                    if (rtWs) {{ try {{ rtWs.close(); }} catch(e) {{}} rtWs = null; }}
                    try {{
                        window.__MINIAPP_IPC__.leaveRealtimeChannel();
                    }} catch(e) {{
                        console.error('Failed to leave realtime channel:', e);
                    }}
                }}
            }};
            try {{
                var resultJson = window.__MINIAPP_IPC__.joinRealtimeChannel();
                if (resultJson) {{
                    var result = JSON.parse(resultJson);
                    if (result.ws_url && result.label) {{
                        var label = encodeURIComponent(result.label);
                        rtWs = new WebSocket(result.ws_url + '/' + label);
                        rtWs.binaryType = 'arraybuffer';
                        rtWs.onclose = function() {{ rtWs = null; }};
                        rtWs.onerror = function() {{ try {{ rtWs.close(); }} catch(e) {{}} rtWs = null; }};
                        // Bi-directional: receive gossip data via WS (bypasses JNI starvation)
                        rtWs.onmessage = function(ev) {{
                            var fn_listener = window.__miniapp_realtime_listener;
                            if (fn_listener && ev.data) {{
                                // Data arrives as base91 string in binary frame
                                var bytes = new Uint8Array(ev.data);
                                var str = '';
                                for (var i = 0; i < bytes.length; i++) str += String.fromCharCode(bytes[i]);
                                fn_listener(new Uint8Array(b91d(str)));
                            }}
                        }};
                        // Detect stuck CONNECTING state (some WebViews silently block WS)
                        setTimeout(function() {{
                            if (rtWs && rtWs.readyState === 0) {{
                                console.warn('[webxdc] WS stuck in CONNECTING — falling back to JNI');
                                try {{ rtWs.close(); }} catch(e) {{}}
                                rtWs = null;
                            }}
                        }}, 1500);
                    }}
                }}
            }} catch(e) {{
                console.error('Failed to join realtime channel:', e);
            }}
            return channel;
        }}
    }};
}})();
"#, addr = addr_escaped, name = name_escaped)
}
