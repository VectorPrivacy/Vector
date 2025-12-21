//! Tauri commands for Mini Apps

use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri::ipc::Channel;
use log::{info, warn, error, trace};
use serde::{Deserialize, Serialize};

use super::error::Error;
use super::state::{MiniAppInstance, MiniAppsState, MiniAppPackage, RealtimeChannelState};
use super::realtime::{RealtimeEvent, encode_topic_id, encode_node_addr};

// Network isolation proxy is only used on non-macOS platforms
#[cfg(not(target_os = "macos"))]
// Network isolation proxy - only used on Linux (not macOS due to version requirements, not Windows due to WebView2 freeze)
#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
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
}

impl MiniAppInfo {
    pub fn from_package(pkg: &super::state::MiniAppPackage) -> Self {
        let icon_data = pkg.get_icon().map(|bytes| {
            // Detect MIME type from bytes
            let mime = if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
                "image/png"
            } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
                "image/jpeg"
            } else if bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") {
                "image/svg+xml"
            } else if bytes.starts_with(b"GIF") {
                "image/gif"
            } else {
                "application/octet-stream"
            };
            
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            format!("data:{};base64,{}", mime, b64)
        });
        
        Self {
            id: pkg.id.clone(),
            name: pkg.manifest.name.clone(),
            description: pkg.manifest.description.clone(),
            version: pkg.manifest.version.clone(),
            has_icon: icon_data.is_some(),
            icon_data,
        }
    }
}

/// Initialization script - runs in all frames
/// Based on DeltaChat's implementation
const INIT_SCRIPT: &str = r#"
// Mini App initialization script
// This runs in all frames to ensure security

console.log("Mini App INIT_SCRIPT running");

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

// Wrap Tauri's __TAURI__ API to restrict access to only allowed commands
// We need to wait for Tauri to initialize first
try {
    const setupTauriRestrictions = () => {
        if (!window.__TAURI__ || !window.__TAURI__.core) {
            // Tauri not ready yet, try again
            setTimeout(setupTauriRestrictions, 10);
            return;
        }
        
        // Mock the notification plugin
        window.__TAURI__.notification = {
            isPermissionGranted: async () => false,
            requestPermission: async () => 'denied',
            sendNotification: () => {},
        };
        
        // Wrap the core invoke to reject all calls except our allowed ones
        const originalInvoke = window.__TAURI__.core.invoke;
        const originalChannel = window.__TAURI__.core.Channel;
        
        window.__TAURI__.core.invoke = async (cmd, args) => {
            // Allow our miniapp commands
            const allowedCommands = [
                'miniapp_get_updates',
                'miniapp_send_update',
                'miniapp_join_realtime_channel',
                'miniapp_send_realtime_data',
                'miniapp_leave_realtime_channel',
                'miniapp_add_realtime_peer',
                'miniapp_get_realtime_node_addr'
            ];
            if (allowedCommands.includes(cmd)) {
                return originalInvoke.call(window.__TAURI__.core, cmd, args);
            }
            console.warn('Mini App tried to invoke blocked Tauri command:', cmd);
            throw new Error('Tauri command not available in Mini Apps: ' + cmd);
        };
        
        // Ensure Channel class is still available (needed for realtime)
        if (originalChannel) {
            window.__TAURI__.core.Channel = originalChannel;
        }
        
        console.log("Mini App Tauri restrictions applied");
    };
    
    // Start checking for Tauri
    setupTauriRestrictions();
} catch (e) {
    console.warn("Failed to setup Tauri restrictions:", e);
}
"#;

/// Get the base URL for Mini Apps based on platform
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
    let path = PathBuf::from(&file_path);
    
    // Generate ID from file path hash
    let id = format!("miniapp_{:x}", md5_hash(&file_path));
    
    let state = app.state::<MiniAppsState>();
    let package = state.get_or_load_package(&id, path).await?;
    
    Ok(MiniAppInfo::from_package(package.as_ref()))
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
    
    let (manifest, icon_bytes) = MiniAppPackage::load_info_from_bytes(&bytes, &fallback_name)?;
    
    // Convert icon bytes to base64 data URL
    let icon_data = icon_bytes.map(|bytes| {
        // Detect MIME type from bytes
        let mime = if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
            "image/png"
        } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            "image/jpeg"
        } else if bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") {
            "image/svg+xml"
        } else if bytes.starts_with(b"GIF") {
            "image/gif"
        } else {
            "application/octet-stream"
        };
        
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        format!("data:{};base64,{}", mime, b64)
    });
    
    Ok(MiniAppInfo {
        id: format!("miniapp_preview_{}", md5_hash(&file_name)),
        name: manifest.name,
        description: manifest.description,
        version: manifest.version,
        has_icon: icon_data.is_some(),
        icon_data,
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
    let path = PathBuf::from(&file_path);
    
    // Generate unique ID from file hash
    let id = format!("miniapp_{:x}", md5_hash(&file_path));
    let window_label = format!("miniapp:{}:{}", chat_id, message_id);
    
    trace!("Opening Mini App: {} ({}, {}) with href: {:?}, topic: {:?}", window_label, chat_id, message_id, href, topic_id);
    
    let state = app.state::<MiniAppsState>();
    
    // Check if already open
    if let Some((existing_label, _existing_instance)) = state.get_instance_by_message(&chat_id, &message_id).await {
        // Focus existing window
        if let Some(window) = app.get_webview_window(&existing_label) {
            // If href is provided, navigate to it
            if let Some(ref href_value) = href {
                let mut nav_url = get_miniapp_base_url()?;
                // Append href to the base URL (href should start with / or be a relative path)
                let href_path = href_value.trim_start_matches('/');
                nav_url.set_path(&format!("/{}", href_path));
                trace!("Navigating existing Mini App to: {}", nav_url);
                window.navigate(nav_url)?;
            }
            window.show()?;
            window.set_focus()?;
            return Ok(());
        } else {
            // Window was closed but instance still exists, clean up
            warn!("Instance exists but window missing, cleaning up: {}", existing_label);
            state.remove_instance(&existing_label).await;
        }
    }
    
    // Load the package
    let package = state.get_or_load_package(&id, path).await?;
    
    // Parse the topic ID if provided (from the message's webxdc-topic tag)
    let realtime_topic = if let Some(ref topic_str) = topic_id {
        match super::realtime::decode_topic_id(topic_str) {
            Ok(topic) => Some(topic),
            Err(e) => {
                warn!("Failed to decode topic ID '{}': {}", topic_str, e);
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
    
    // Build the initial URL - append href if provided
    let mut initial_url = get_miniapp_base_url()?;
    if let Some(ref href_value) = href {
        // Append href to the base URL (href should start with / or be a relative path)
        let href_path = href_value.trim_start_matches('/');
        initial_url.set_path(&format!("/{}", href_path));
        trace!("Mini App will open at: {}", initial_url);
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
    // Use initialization_script_for_all_frames like DeltaChat does
    .initialization_script_for_all_frames(INIT_SCRIPT)
    // Enable devtools in debug mode only
    .devtools(cfg!(debug_assertions))
    .on_navigation(move |url| {
        // Only allow navigation within the webxdc:// scheme or webxdc.localhost
        let scheme = url.scheme();
        let allowed = scheme == "webxdc" || (scheme == "http" && url.host_str() == Some("webxdc.localhost"));
        if !allowed {
            warn!("Blocked navigation to: {}", url);
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
                info!("Mini App window destroyed: {}", window_label_for_handler);
                let app_handle = app_handle_for_handler.clone();
                let label = window_label_for_handler.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_handle.state::<MiniAppsState>();
                    
                    // Get the channel state before removing to get the topic
                    // We remove the channel state (marking us as not playing) but DON'T leave the Iroh channel
                    // This way we can still see other players' peer count
                    let channel_state = state.remove_realtime_channel(&label).await;
                    
                    if let Some(channel) = channel_state {
                        let topic_encoded = super::realtime::encode_topic_id(&channel.topic);
                        println!("[WEBXDC] Window destroyed, marking inactive for topic: {}", topic_encoded);
                        
                        // Get current peer count from the channel (we're still connected)
                        let peer_count = if let Ok(iroh) = state.realtime.get_or_init().await {
                            iroh.get_peer_count(&channel.topic).await
                        } else {
                            0
                        };
                        
                        // Emit status update to main window - we're no longer playing but can still see peers
                        if let Some(main_window) = app_handle.get_webview_window("main") {
                            let _ = main_window.emit("miniapp_realtime_status", serde_json::json!({
                                "topic": topic_encoded,
                                "peer_count": peer_count,
                                "is_active": false,
                                "has_pending_peers": peer_count > 0,
                            }));
                            println!("[WEBXDC] Emitted miniapp_realtime_status: active=false, peer_count={} for topic {}", peer_count, topic_encoded);
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
                    trace!("Second CloseRequested event, closing now");
                    return;
                }
                
                trace!("CloseRequested on Mini App window, will delay close");
                
                // Navigate to webxdc.js to trigger unload events
                // This allows sendUpdate() calls in visibilitychange/unload handlers to complete
                if let Err(err) = window_clone.navigate(webxdc_js_url.clone()) {
                    error!("Failed to navigate before close: {err}");
                    return;
                }
                
                // Hide the window immediately for better UX
                window_clone.hide()
                    .inspect_err(|err| warn!("Failed to hide window: {err}"))
                    .ok();
                
                api.prevent_close();
                
                let window_clone2 = Arc::clone(&window_clone);
                tauri::async_runtime::spawn(async move {
                    // Wait a bit for any pending sendUpdate() calls
                    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
                    trace!("Delay elapsed, closing Mini App window");
                    window_clone2.close()
                        .inspect_err(|err| error!("Failed to close window: {err}"))
                        .ok();
                });
            }
            _ => {}
        }
    });
    
    info!("Opened Mini App: {} in window {}", package.manifest.name, window_label);
    
    Ok(())
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
        if let Some(window) = app.get_webview_window(&label) {
            window.close()?;
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
    trace!("Mini App {} requesting updates since serial {}", label, last_known_serial);
    
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
    
    info!(
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

/// Join the realtime channel for a Mini App
/// Returns the topic ID that can be shared with other participants
#[tauri::command]
pub async fn miniapp_join_realtime_channel(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, MiniAppsState>,
    channel: Channel<RealtimeEvent>,
) -> Result<String, Error> {
    let label = window.label();
    println!("[WEBXDC] miniapp_join_realtime_channel called for window: {}", label);
    
    if !label.starts_with("miniapp:") {
        println!("[WEBXDC] ERROR: Window label doesn't start with 'miniapp:': {}", label);
        return Err(Error::InstanceNotFoundByLabel(label.to_string()));
    }
    
    // Check if already has an active channel
    if state.has_realtime_channel(label).await {
        println!("[WEBXDC] WARNING: Realtime channel already active for: {}", label);
        return Err(Error::RealtimeChannelAlreadyActive);
    }
    
    // Get the instance to get the topic from the webxdc-topic tag
    let instance = state.get_instance(label).await
        .ok_or_else(|| {
            println!("[WEBXDC] ERROR: Instance not found for label: {}", label);
            Error::InstanceNotFoundByLabel(label.to_string())
        })?;
    
    println!("[WEBXDC] Found instance for Mini App: {} (chat: {}, message: {})",
        instance.package.manifest.name, instance.chat_id, instance.message_id);
    
    // Use the topic from the Nostr event's webxdc-topic tag if available
    // Otherwise, derive a topic from the chat_id and message_id for local testing
    let topic = if let Some(t) = instance.realtime_topic {
        println!("[WEBXDC] Using webxdc-topic from message tag");
        t
    } else {
        // Generate a deterministic topic from chat_id and message_id
        // This allows local testing but won't work for cross-device sync
        // (since the topic won't match what other devices have)
        println!("[WEBXDC] WARNING: No webxdc-topic tag found, generating local topic for Mini App: {}", label);
        super::realtime::derive_topic_id(&instance.package.manifest.name, &instance.chat_id, &instance.message_id)
    };
    
    println!("[WEBXDC] Initializing Iroh realtime manager...");
    // Get the realtime manager and join the channel
    let iroh = state.realtime.get_or_init().await
        .map_err(|e| {
            println!("[WEBXDC] ERROR: Failed to initialize Iroh: {}", e);
            Error::RealtimeError(e.to_string())
        })?;
    println!("[WEBXDC] Iroh realtime manager initialized successfully");
    
    // Join the Iroh gossip channel with no initial peers
    // Peers will be added via advertisements
    let (is_rejoin, _join_rx) = iroh.join_channel(topic, vec![], channel.clone(), Some(app.clone())).await
        .map_err(|e| Error::RealtimeError(e.to_string()))?;
    
    let topic_encoded = encode_topic_id(&topic);
    
    if is_rejoin {
        // Re-joining an existing channel - just update the event channel
        info!("Re-joined existing realtime channel for Mini App: {} (topic: {})", label, topic_encoded);
        println!("[WEBXDC] Re-joining existing channel, updating event channel for window: {}", label);
    } else {
        info!("Joined new realtime channel for Mini App: {} (topic: {})", label, topic_encoded);
    }
    
    // Store/update the channel state with the new event channel
    // This is important for re-joins: the old event channel is stale (old window closed)
    let channel_state = RealtimeChannelState {
        topic,
        event_channel: Some(channel),
        active: true,
    };
    state.set_realtime_channel(label, channel_state).await;
    
    // Check for pending peers that advertised before we joined
    let pending_peers = state.take_pending_peers(&topic).await;
    let pending_peer_count = pending_peers.len();
    println!("[WEBXDC] Checking for pending peers for topic {}: found {}", topic_encoded, pending_peer_count);
    if !pending_peers.is_empty() {
        println!("[WEBXDC] Found {} pending peers for topic {}", pending_peer_count, topic_encoded);
        for pending in pending_peers {
            println!("[WEBXDC] Adding pending peer {} to channel", pending.node_addr.node_id);
            match iroh.add_peer(topic, pending.node_addr.clone()).await {
                Ok(_) => {
                    println!("[WEBXDC] Successfully added pending peer {} to realtime channel", pending.node_addr.node_id);
                }
                Err(e) => {
                    println!("[WEBXDC] ERROR: Failed to add pending peer: {}", e);
                }
            }
        }
    } else {
        println!("[WEBXDC] No pending peers found for topic {}", topic_encoded);
    }
    
    // Get our node address and send a peer advertisement to the chat
    // This allows other participants to discover and connect to us
    let node_addr = iroh.get_node_addr().await
        .map_err(|e| Error::RealtimeError(e.to_string()))?;
    let node_addr_encoded = encode_node_addr(&node_addr)
        .map_err(|e| Error::RealtimeError(e.to_string()))?;
    
    // Send peer advertisement to the chat
    // We send it immediately and then periodically while the channel is active
    let chat_id = instance.chat_id.clone();
    let topic_for_advert = topic_encoded.clone();
    let node_addr_for_advert = node_addr_encoded.clone();
    
    println!("[WEBXDC] Sending initial peer advertisement...");
    
    // Send initial advertisement
    let chat_id_clone = chat_id.clone();
    let topic_clone = topic_for_advert.clone();
    let addr_clone = node_addr_for_advert.clone();
    tokio::spawn(async move {
        println!("[WEBXDC] In spawn: sending initial peer advertisement");
        if crate::send_webxdc_peer_advertisement(chat_id_clone, topic_clone, addr_clone).await {
            println!("[WEBXDC] Sent initial peer advertisement successfully");
        } else {
            println!("[WEBXDC] ERROR: Failed to send initial peer advertisement");
        }
    });
    
    // Send a second advertisement after a short delay (helps with timing issues)
    let chat_id_clone2 = chat_id.clone();
    let topic_clone2 = topic_for_advert.clone();
    let addr_clone2 = node_addr_for_advert.clone();
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        println!("[WEBXDC] In spawn: sending delayed peer advertisement");
        if crate::send_webxdc_peer_advertisement(chat_id_clone2, topic_clone2, addr_clone2).await {
            println!("[WEBXDC] Sent delayed peer advertisement successfully");
        } else {
            println!("[WEBXDC] ERROR: Failed to send delayed peer advertisement");
        }
    });
    
    // Emit status event to main window so UI updates to show "Playing"
    // Use the pending peer count since NeighborUp events haven't been received yet
    if let Some(main_window) = app.get_webview_window("main") {
        let current_peer_count = iroh.get_peer_count(&topic).await;
        // Use whichever is higher - the actual peer count or the pending peers we just added
        let effective_peer_count = std::cmp::max(current_peer_count, pending_peer_count);
        let _ = main_window.emit("miniapp_realtime_status", serde_json::json!({
            "topic": topic_encoded.clone(),
            "peer_count": effective_peer_count,
            "is_active": true,
            "has_pending_peers": false,
        }));
        println!("[WEBXDC] Emitted miniapp_realtime_status event: topic={}, peer_count={} (current={}, pending={}), active=true",
            topic_encoded, effective_peer_count, current_peer_count, pending_peer_count);
    }
    
    Ok(topic_encoded)
}

/// Send data through the realtime channel
#[tauri::command]
pub async fn miniapp_send_realtime_data(
    window: WebviewWindow,
    state: State<'_, MiniAppsState>,
    data: Vec<u8>,
) -> Result<(), Error> {
    let label = window.label();
    
    if !label.starts_with("miniapp:") {
        return Err(Error::InstanceNotFoundByLabel(label.to_string()));
    }
    
    // Check data size (max 128 KB as per WebXDC spec)
    if data.len() > 128_000 {
        return Err(Error::RealtimeDataTooLarge(data.len()));
    }
    
    // Get the topic for this instance
    let topic = state.get_realtime_channel(label).await
        .ok_or(Error::RealtimeChannelNotActive)?;
    
    // Send the data
    let iroh = state.realtime.get_or_init().await
        .map_err(|e| Error::RealtimeError(e.to_string()))?;
    
    iroh.send_data(topic, data).await
        .map_err(|e| Error::RealtimeError(e.to_string()))?;
    
    trace!("Sent realtime data for Mini App: {}", label);
    
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
        
        iroh.leave_channel(channel_state.topic).await
            .map_err(|e| Error::RealtimeError(e.to_string()))?;
        
        info!("Left realtime channel for Mini App: {} (topic: {})", label, encode_topic_id(&channel_state.topic));
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
    
    info!("Added peer to realtime channel for Mini App: {}", label);
    
    Ok(())
}

/// Get our node address for sharing with peers (via Nostr)
#[tauri::command]
pub async fn miniapp_get_realtime_node_addr(
    state: State<'_, MiniAppsState>,
) -> Result<String, Error> {
    let iroh = state.realtime.get_or_init().await
        .map_err(|e| Error::RealtimeError(e.to_string()))?;
    
    let addr = iroh.get_node_addr().await
        .map_err(|e| Error::RealtimeError(e.to_string()))?;
    
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
    
    println!("[WEBXDC] miniapp_get_realtime_status called for topic: {}", topic_id);
    
    // Get pending peer count (these are peers that advertised before we joined)
    let pending_peer_count = state.get_pending_peer_count(&topic).await;
    println!("[WEBXDC] Pending peer count: {}", pending_peer_count);
    
    // Check if WE are actively playing (have a Mini App window open for this topic)
    // This is different from whether we have an Iroh channel (which we keep open to see peers)
    let we_are_playing = {
        let channels = state.realtime_channels.read().await;
        channels.values().any(|ch| ch.topic == topic && ch.active)
    };
    println!("[WEBXDC] We are playing: {}", we_are_playing);
    
    // Check if we have an active Iroh instance
    let iroh_result = state.realtime.get_or_init().await;
    
    match iroh_result {
        Ok(iroh) => {
            let has_channel = iroh.has_channel(&topic).await;
            let peer_count = iroh.get_peer_count(&topic).await;
            
            println!("[WEBXDC] miniapp_get_realtime_status: we_are_playing={}, has_channel={}, peer_count={}, pending={}",
                we_are_playing, has_channel, peer_count, pending_peer_count);
            
            Ok(RealtimeChannelInfo {
                active: we_are_playing,
                peer_count,
                pending_peer_count,
                topic_id,
            })
        }
        Err(e) => {
            // Iroh not initialized, no active channels
            println!("[WEBXDC] miniapp_get_realtime_status: Iroh not initialized: {}", e);
            Ok(RealtimeChannelInfo {
                active: false,
                peer_count: 0,
                pending_peer_count,
                topic_id,
            })
        }
    }
}