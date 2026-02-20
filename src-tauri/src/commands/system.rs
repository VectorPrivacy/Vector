//! System and platform Tauri commands.
//!
//! This module handles system-level operations:
//! - Platform feature detection
//! - Storage management (info and cleanup)
//! - Periodic maintenance tasks

use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::{STATE, TAURI_APP};
use crate::{db, image_cache, util::format_bytes};

#[cfg(desktop)]
use crate::audio;

#[cfg(all(not(target_os = "android"), feature = "whisper"))]
use crate::whisper;

// ============================================================================
// Types
// ============================================================================

/// Platform feature list structure
#[derive(serde::Serialize, Clone)]
pub struct PlatformFeatures {
    pub transcription: bool,
    pub notification_sounds: bool,
    pub os: String,
    pub is_mobile: bool,
    pub debug_mode: bool,
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Returns a list of platform-specific features available
#[tauri::command]
pub async fn get_platform_features() -> PlatformFeatures {
    let os = if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    };

    let is_mobile = cfg!(target_os = "android") || cfg!(target_os = "ios");

    PlatformFeatures {
        transcription: cfg!(all(not(target_os = "android"), feature = "whisper")),
        notification_sounds: cfg!(desktop),
        os: os.to_string(),
        is_mobile,
        debug_mode: cfg!(debug_assertions),
    }
}

/// Run periodic maintenance tasks to keep memory usage low
/// Called every ~45s from the JS profile sync loop
///
/// Current tasks:
/// - Purge expired notification sound cache (10 min TTL, desktop only)
/// - Cleanup stale in-progress download tracking entries
///
/// Future tasks could include:
/// - Image cache cleanup
/// - Temporary file cleanup
/// - Memory pressure responses
#[tauri::command]
pub async fn run_maintenance() {
    // Audio: purge expired notification sound cache (desktop only)
    #[cfg(desktop)]
    audio::check_cache_ttl();

    // Cleanup stale download tracking entries
    image_cache::cleanup_stale_downloads().await;
}

/// Get storage information for the Vector directory
#[tauri::command]
pub async fn get_storage_info() -> Result<serde_json::Value, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?;

    // Determine the base directory (Downloads on most platforms, Documents on iOS)
    let base_directory = if cfg!(target_os = "ios") {
        tauri::path::BaseDirectory::Document
    } else {
        tauri::path::BaseDirectory::Download
    };

    // Resolve the vector directory path
    let vector_dir = handle.path().resolve("vector", base_directory)
        .map_err(|e| format!("Failed to resolve vector directory: {}", e))?;

    // Check if directory exists
    if !vector_dir.exists() {
        return Ok(serde_json::json!({
            "path": vector_dir.to_string_lossy().to_string(),
            "total_bytes": 0,
            "file_count": 0,
            "type_distribution": {}
        }));
    }

    // Calculate total size and file count
    let mut total_bytes = 0;
    let mut file_count = 0;

    // Track file type distribution by size
    let mut type_distribution = std::collections::HashMap::new();

    // Walk through all files in the directory
    if let Ok(entries) = std::fs::read_dir(&vector_dir) {
        for entry in entries.flatten() {
            if let Ok(metadata) = entry.metadata() {
                if metadata.is_file() {
                    let file_size = metadata.len();
                    total_bytes += file_size;
                    file_count += 1;

                    // Get file extension
                    if let Some(extension) = entry.file_name().to_string_lossy().split('.').last() {
                        let extension = extension.to_lowercase();
                        *type_distribution.entry(extension).or_insert(0) += file_size;
                    }
                }
            }
        }
    }

    // Calculate Whisper models size if whisper feature is enabled
    #[cfg(all(not(target_os = "android"), feature = "whisper"))]
    {
        // Calculate total size of downloaded Whisper models
        let mut ai_models_size = 0;
        for model in whisper::MODELS {
            if whisper::is_model_downloaded(&handle, model.name) {
                // Convert MB to bytes (model sizes are in MB)
                ai_models_size += (model.size as u64) * 1024 * 1024;
            }
        }

        if ai_models_size > 0 {
            // Add AI models to type distribution
            *type_distribution.entry("ai_models".to_string()).or_insert(0) += ai_models_size;
            total_bytes += ai_models_size;
        }
    }

    // Calculate image cache size (avatars, banners, miniapp icons)
    // Cache is global (not per-account) for deduplication across accounts
    if let Ok(cache_size) = image_cache::get_cache_size(handle) {
        if cache_size > 0 {
            *type_distribution.entry("cache".to_string()).or_insert(0) += cache_size;
            total_bytes += cache_size;
        }
    }

    // Return storage information with type distribution
    Ok(serde_json::json!({
        "path": vector_dir.to_string_lossy().to_string(),
        "total_bytes": total_bytes,
        "file_count": file_count,
        "total_formatted": format_bytes(total_bytes),
        "type_distribution": type_distribution
    }))
}

/// Clear all downloaded attachments from messages and return freed storage space
#[tauri::command]
pub async fn clear_storage<R: Runtime>(handle: AppHandle<R>) -> Result<serde_json::Value, String> {
    // First, get the total storage size before clearing
    let storage_info_before = get_storage_info().await.map_err(|e| format!("Failed to get storage info before clearing: {}", e))?;
    let total_bytes_before = storage_info_before["total_bytes"].as_u64().unwrap_or(0);

    // Lock the state to access all chats and messages
    let mut state = STATE.lock().await;

    // Track which chats have been updated to avoid duplicate saves
    let mut updated_chats = std::collections::HashSet::new();

    // Process each chat to clear attachment metadata in messages
    // Use index-based iteration to allow interner access for Message conversion
    for chat_idx in 0..state.chats.len() {
        let mut updated_msg_ids = Vec::new();

        // Iterate through all messages in this chat
        for message in state.chats[chat_idx].messages.iter_mut() {
            let mut attachment_updated = false;

            // Iterate through all attachments and reset their properties
            for attachment in &mut message.attachments {
                if attachment.downloaded() || !attachment.path.is_empty() {
                    // Delete the file (ignore error if it doesn't exist)
                    let _ = std::fs::remove_file(&*attachment.path);
                    // Reset attachment properties
                    attachment.set_downloaded(false);
                    attachment.set_downloading(false);
                    attachment.path = String::new().into_boxed_str();
                    attachment_updated = true;
                }
            }

            // If any attachment was updated, track this message for save/emit
            if attachment_updated {
                updated_msg_ids.push(message.id);
            }
        }

        // If we have messages to update, save them to the database
        if !updated_msg_ids.is_empty() {
            let chat_id = state.chats[chat_idx].id().to_string();

            // Convert updated messages to Message format for save and emit
            let messages_to_update: Vec<crate::Message> = updated_msg_ids.iter()
                .filter_map(|msg_id| {
                    let hex_id = crate::simd::bytes_to_hex_32(msg_id);
                    state.chats[chat_idx].messages.find_by_hex_id(&hex_id)
                        .map(|m| m.to_message(&state.interner))
                })
                .collect();

            // Save updated messages to database
            db::save_chat_messages(&chat_id, &messages_to_update).await
                .map_err(|e| format!("Failed to save updated messages for chat {}: {}", chat_id, e))?;

            // Emit message_update events for each updated message
            for message in &messages_to_update {
                handle.emit("message_update", serde_json::json!({
                    "old_id": &message.id,
                    "message": message,
                    "chat_id": &chat_id
                })).map_err(|e| format!("Failed to emit message_update for chat {}: {}", chat_id, e))?;
            }

            updated_chats.insert(chat_id);
        }
    }

    // Clear all disk caches (images, sounds, etc.) by nuking the cache directory
    let cache_dir = handle.path().app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?
        .join("cache");
    if cache_dir.exists() {
        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    // Clear in-memory notification sound cache (desktop only)
    #[cfg(desktop)]
    audio::purge_sound_cache();

    // Clear cached paths from all profiles in state and database
    let mut cleared_ids = Vec::new();
    for profile in &mut state.profiles {
        if !profile.avatar_cached.is_empty() || !profile.banner_cached.is_empty() {
            profile.avatar_cached = Box::<str>::default();
            profile.banner_cached = Box::<str>::default();
            cleared_ids.push(profile.id);
        }
    }
    for id in cleared_ids {
        if let Some(slim) = state.serialize_profile(id) {
            db::set_profile(slim).await.ok();
        }
    }

    // Clear cached avatar paths from all MLS groups in DB and notify frontend
    if db::clear_all_mls_group_avatar_cache().is_ok() {
        // Reload (now all avatar_cached = NULL) and emit events so frontend switches to placeholders
        if let Ok(groups) = db::load_mls_groups().await {
            for meta in groups.iter().filter(|g| !g.evicted) {
                crate::mls::emit_group_metadata_event(meta);
            }
        }
    }

    // Get storage info after clearing to calculate freed space
    // Need to drop the state lock first since get_storage_info needs it
    drop(state);
    let storage_info_after = get_storage_info().await.map_err(|e| format!("Failed to get storage info after clearing: {}", e))?;
    let total_bytes_after = storage_info_after["total_bytes"].as_u64().unwrap_or(0);

    // Calculate freed space
    let freed_bytes = total_bytes_before.saturating_sub(total_bytes_after);

    // Return the freed storage information
    Ok(serde_json::json!({
        "freed_bytes": freed_bytes,
        "freed_formatted": format_bytes(freed_bytes),
        "updated_chats": updated_chats.len()
    }))
}

// ============================================================================
// Battery Optimization & Background Service Commands
// ============================================================================

/// Check whether the app is exempt from battery optimizations (Android only).
/// Returns `true` on non-Android platforms (no optimization needed).
#[tauri::command]
pub async fn check_battery_optimized() -> bool {
    #[cfg(target_os = "android")]
    {
        call_battery_helper_bool("isIgnoringBatteryOptimizations")
    }
    #[cfg(not(target_os = "android"))]
    {
        true
    }
}

/// Open the system battery optimization dialog (Android only). No-op on other platforms.
#[tauri::command]
pub async fn request_battery_optimization() {
    #[cfg(target_os = "android")]
    {
        call_battery_helper_void("requestBatteryOptimization");
    }
}

/// Get the background service enabled preference.
/// Returns `true` on non-Android platforms.
#[tauri::command]
pub async fn get_background_service_enabled() -> bool {
    #[cfg(target_os = "android")]
    {
        call_battery_helper_bool("getBackgroundServiceEnabled")
    }
    #[cfg(not(target_os = "android"))]
    {
        true
    }
}

/// Set the background service enabled preference and start/stop the service accordingly.
#[tauri::command]
pub async fn set_background_service_enabled(enabled: bool) {
    #[cfg(target_os = "android")]
    {
        call_battery_helper_set_enabled(enabled);
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = enabled;
    }
}

/// Check whether the user has been prompted for background service setup.
/// Returns `true` on non-Android platforms (no prompt needed).
#[tauri::command]
pub async fn get_background_service_prompted() -> bool {
    #[cfg(target_os = "android")]
    {
        call_battery_helper_bool("getBackgroundServicePrompted")
    }
    #[cfg(not(target_os = "android"))]
    {
        true
    }
}

/// Mark the user as having been prompted for background service setup.
#[tauri::command]
pub async fn set_background_service_prompted() {
    #[cfg(target_os = "android")]
    {
        call_battery_helper_void("setBackgroundServicePrompted");
    }
}

// ============================================================================
// Android JNI helpers for VectorBatteryHelper
// Uses ndk_context (Tauri's Activity context) — always available when Tauri
// commands execute, unlike BG_JAVA_VM which depends on service startup timing.
// ============================================================================

#[cfg(target_os = "android")]
fn call_battery_helper_bool(method: &str) -> bool {
    let default = method == "getBackgroundServiceEnabled";
    crate::android::utils::with_android_context(|env, activity| {
        let class_loader = env.call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
            .map_err(|e| format!("{:?}", e))?.l().map_err(|e| format!("{:?}", e))?;

        let class_name = env.new_string("io.vectorapp.VectorBatteryHelper")
            .map_err(|e| format!("{:?}", e))?;
        let helper_class = env.call_method(&class_loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;",
            &[jni::objects::JValue::Object(&class_name)])
            .map_err(|e| format!("{:?}", e))?.l().map_err(|e| format!("{:?}", e))?;

        let helper_jclass = jni::objects::JClass::from(helper_class);
        let val = env.call_static_method(&helper_jclass, method, "(Landroid/content/Context;)Z",
            &[activity.into()])
            .map_err(|e| format!("{:?}", e))?.z().map_err(|e| format!("{:?}", e))?;
        Ok(val)
    }).unwrap_or(default)
}

#[cfg(target_os = "android")]
fn call_battery_helper_void(method: &str) {
    let _ = crate::android::utils::with_android_context(|env, activity| {
        let class_loader = env.call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
            .map_err(|e| format!("{:?}", e))?.l().map_err(|e| format!("{:?}", e))?;

        let class_name = env.new_string("io.vectorapp.VectorBatteryHelper")
            .map_err(|e| format!("{:?}", e))?;
        let helper_class = env.call_method(&class_loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;",
            &[jni::objects::JValue::Object(&class_name)])
            .map_err(|e| format!("{:?}", e))?.l().map_err(|e| format!("{:?}", e))?;

        let helper_jclass = jni::objects::JClass::from(helper_class);
        env.call_static_method(&helper_jclass, method, "(Landroid/content/Context;)V",
            &[activity.into()])
            .map_err(|e| format!("{:?}", e))?;
        Ok(())
    });
}

#[cfg(target_os = "android")]
fn call_battery_helper_set_enabled(enabled: bool) {
    let _ = crate::android::utils::with_android_context(|env, activity| {
        let class_loader = env.call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
            .map_err(|e| format!("{:?}", e))?.l().map_err(|e| format!("{:?}", e))?;

        let class_name = env.new_string("io.vectorapp.VectorBatteryHelper")
            .map_err(|e| format!("{:?}", e))?;
        let helper_class = env.call_method(&class_loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;",
            &[jni::objects::JValue::Object(&class_name)])
            .map_err(|e| format!("{:?}", e))?.l().map_err(|e| format!("{:?}", e))?;

        let helper_jclass = jni::objects::JClass::from(helper_class);

        // Set the preference
        env.call_static_method(&helper_jclass, "setBackgroundServiceEnabled",
            "(Landroid/content/Context;Z)V",
            &[activity.into(), jni::objects::JValue::Bool(enabled as u8)])
            .map_err(|e| format!("{:?}", e))?;

        // Start or stop the service
        let service_method = if enabled { "startBackgroundService" } else { "stopBackgroundService" };
        env.call_static_method(&helper_jclass, service_method, "(Landroid/content/Context;)V",
            &[activity.into()])
            .map_err(|e| format!("{:?}", e))?;

        Ok(())
    });
}

// Handler list for this module (for reference):
// - get_platform_features
// - run_maintenance
// - get_storage_info
// - clear_storage
// - check_battery_optimized
// - request_battery_optimization
// - get_background_service_enabled
// - set_background_service_enabled
// - get_background_service_prompted
// - set_background_service_prompted
