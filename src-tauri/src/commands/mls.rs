//! MLS (Messaging Layer Security) Tauri commands.
//!
//! This module handles MLS group messaging operations:
//! - Device and keypackage management
//! - Group creation and membership
//! - Welcome message handling
//! - Group metadata and member queries

use nostr_sdk::prelude::*;
use tauri::Emitter;
use std::sync::Arc;
#[cfg(not(target_os = "android"))]
use tauri_plugin_fs::FsExt;
use crate::{db, mls, MlsService, NotificationData, show_notification_generic, NOTIFIED_WELCOMES, STATE, TAURI_APP};
use crate::util::{bytes_to_hex_string, hex_string_to_bytes};

// ============================================================================
// Device & KeyPackage Read Commands
// ============================================================================

/// Load MLS device ID for the current account
#[tauri::command]
pub async fn load_mls_device_id() -> Result<Option<String>, String> {
    match db::load_mls_device_id().await {
        Ok(Some(id)) => Ok(Some(id)),
        Ok(None) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Load MLS keypackages for the current account
#[tauri::command]
pub async fn load_mls_keypackages() -> Result<Vec<serde_json::Value>, String> {
    db::load_mls_keypackages().await
        .map_err(|e| e.to_string())
}

/// Regenerate this device's MLS KeyPackage. Delegates to vector-core.
///
/// If `cache` is true, attempts to reuse an existing cached KeyPackage from relay.
/// Otherwise always generates a fresh one.
#[tauri::command]
pub async fn regenerate_device_keypackage(cache: bool) -> Result<serde_json::Value, String> {
    let core = vector_core::VectorCore;
    let kp = core.publish_keypackage(cache).await
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "device_id": kp.device_id,
        "owner_pubkey": kp.owner_pubkey,
        "keypackage_ref": kp.keypackage_ref,
        "cached": kp.cached,
    }))
}

// ============================================================================
// Group Query Commands
// ============================================================================

/// List all MLS group IDs
#[tauri::command]
pub async fn list_mls_groups() -> Result<Vec<String>, String> {
    match db::load_mls_groups().await {
        Ok(groups) => {
            let ids = groups.into_iter()
                .map(|g| g.group.group_id)
                .collect();
            Ok(ids)
        }
        Err(e) => Err(format!("Failed to load MLS groups: {}", e)),
    }
}

/// Get metadata for all MLS groups (filtered to non-evicted groups)
#[tauri::command]
pub async fn get_mls_group_metadata() -> Result<Vec<serde_json::Value>, String> {
    let groups = db::load_mls_groups()
        .await
        .map_err(|e| format!("Failed to load MLS group metadata: {}", e))?;

    Ok(groups
        .iter()
        .filter(|meta| !meta.evicted)
        .map(|meta| mls::metadata_to_frontend(meta))
        .collect())
}


// ============================================================================
// Group Management Commands
// ============================================================================

/// Leave an MLS group
#[tauri::command]
pub async fn leave_mls_group(group_id: String) -> Result<(), String> {
    // Run non-Send MLS engine work on a blocking thread
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent_static().map_err(|e| e.to_string())?;
            mls.leave_group(&group_id)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))??;

    // Refresh the live MLS subscription to remove the left group
    crate::services::subscription_handler::refresh_mls_subscription().await;
    Ok(())
}

// ============================================================================
// Group Members Query Commands
// ============================================================================

#[derive(serde::Serialize, Clone)]
pub struct GroupMembers {
    pub group_id: String,
    pub members: Vec<String>, // npubs
    pub admins: Vec<String>,  // admin npubs
}

/// Get members (npubs) of an MLS group from the persistent engine (on-demand)
#[tauri::command]
pub async fn get_mls_group_members(group_id: String) -> Result<GroupMembers, String> {
    // Run engine operations on a blocking thread so the outer future is Send
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            // Initialise persistent MLS
            let mls = MlsService::new_persistent_static().map_err(|e| e.to_string())?;
            // Map wire-id/engine-id using encrypted metadata
            let meta_groups = mls.read_groups().unwrap_or_default();
            let (wire_id, engine_id) = if let Some(m) = meta_groups
                .iter()
                .find(|g| g.group_id == group_id || (!g.engine_group_id.is_empty() && g.engine_group_id == group_id))
            {
                (
                    m.group_id.clone(),
                    if !m.engine_group_id.is_empty() { m.engine_group_id.clone() } else { m.group_id.clone() },
                )
            } else {
                (group_id.clone(), group_id.clone())
            };

            // Acquire non-Send engine; all calls below must be non-await while engine is in scope
            let engine = mls.engine().map_err(|e| e.to_string())?;
            use mdk_core::prelude::GroupId;

            let mut members: Vec<String> = Vec::new();
            let mut admins: Vec<String> = Vec::new();
            let gid_bytes = hex_string_to_bytes(&engine_id);
            if !gid_bytes.is_empty() {
                // Decode engine id to GroupId
                let gid = GroupId::from_slice(&gid_bytes);

                // Get members via engine API
                match engine.get_members(&gid) {
                    Ok(pk_list) => {
                        members = pk_list
                            .into_iter()
                            .filter_map(|pk| pk.to_bech32().ok())
                            .collect();
                    }
                    Err(e) => {
                        eprintln!("[MLS] get_members failed for engine_id={}: {}", engine_id, e);
                    }
                }

                // Get admins from the group
                match engine.get_groups() {
                    Ok(groups) => {
                        for g in groups {
                            let gid_hex = bytes_to_hex_string(g.mls_group_id.as_slice());
                            if gid_hex == engine_id {
                                admins = g.admin_pubkeys.iter()
                                    .filter_map(|pk| pk.to_bech32().ok())
                                    .collect();
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[MLS] get_groups failed: {}", e);
                    }
                }
            }

            // Fallback: If admins list is empty, use creator_pubkey from stored metadata
            // This ensures non-admins can still see who the group owner/admin is
            if admins.is_empty() {
                if let Some(meta) = meta_groups.iter().find(|g| g.group_id == wire_id) {
                    if !meta.creator_pubkey.is_empty() {
                        admins.push(meta.creator_pubkey.clone());
                    }
                }
            }

            Ok(GroupMembers {
                group_id: wire_id,
                members,
                admins,
            })
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

// ============================================================================
// KeyPackage Refresh Commands
// ============================================================================

/// Refresh keypackages for a contact from TRUSTED_RELAY.
///
/// Delegates to vector-core's `fetch_keypackages` which handles relay fetch,
/// dedup, and index persistence. Returns (device_id, keypackage_ref) pairs —
/// duplicated because they're the same value in this design.
#[tauri::command]
pub async fn refresh_keypackages_for_contact(
    npub: String,
) -> Result<Vec<(String, String)>, String> {
    let core = vector_core::VectorCore;
    let packages = core.fetch_keypackages(&npub).await
        .map_err(|e| e.to_string())?;
    // device_id and keypackage_ref are the same value (the event ID hex)
    Ok(packages.into_iter().map(|(id, _)| (id.clone(), id)).collect())
}

// ============================================================================
// Member Management Commands
// ============================================================================

/// Add a member device to an MLS group
#[tauri::command]
pub async fn add_mls_member_device(
    group_id: String,
    member_npub: String,
    device_id: String,
) -> Result<(), String> {
    // Run non-Send MLS engine work on a blocking thread; drive async via current runtime
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent_static().map_err(|e| e.to_string())?;
            mls.add_member_device(&group_id, &member_npub, &device_id)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

// ============================================================================
// Group Avatar Upload
// ============================================================================

/// Encrypt and upload a group avatar image to Blossom
///
/// 1. Reads image file from disk
/// 2. Encrypts with ChaCha20-Poly1305 via MDK (CPU-only, no network)
/// 3. Uploads encrypted blob to Blossom servers with progress events
/// 4. Returns image_hash, image_key, image_nonce, blob_url as hex strings
#[tauri::command]
pub async fn upload_group_avatar(filepath: String) -> Result<serde_json::Value, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();

    // Read file from disk
    let bytes = {
        #[cfg(not(target_os = "android"))]
        {
            handle.fs().read(std::path::Path::new(&filepath))
                .map_err(|_| "Image couldn't be loaded from disk")?
        }
        #[cfg(target_os = "android")]
        {
            let att = crate::android::filesystem::read_android_uri(filepath.clone())?;
            Arc::try_unwrap(att.bytes).unwrap_or_else(|arc| (*arc).clone())
        }
    };

    // Determine MIME type from extension
    let extension = filepath
        .rsplit('.')
        .next()
        .unwrap_or("bin")
        .to_lowercase();
    let mime_type = crate::util::mime_from_extension_safe(&extension, true)
        .map_err(|_| "File type is not allowed (only images are permitted)")?;

    // Encrypt the image using MDK (CPU-only, instant)
    let prepared = mdk_core::extension::prepare_group_image_for_upload(&bytes, &mime_type)
        .map_err(|e| format!("Failed to prepare group image: {}", e))?;

    // Set up progress callback
    let handle_clone = handle.clone();
    let progress_callback: crate::blossom::ProgressCallback = Arc::new(move |percentage, bytes_uploaded| {
        let payload = serde_json::json!({
            "type": "group_avatar",
            "progress": percentage.unwrap_or(0),
            "bytes": bytes_uploaded.unwrap_or(0)
        });
        handle_clone.emit("profile_upload_progress", payload)
            .map_err(|_| "Failed to emit progress event".to_string())
    });

    // Upload encrypted blob to Blossom using the derived upload keypair
    let servers = crate::get_blossom_servers();
    let encrypted_data = Arc::new(prepared.encrypted_data.as_ref().to_vec());
    let blob_url = crate::blossom::upload_blob_with_progress_and_failover(
        prepared.upload_keypair,
        servers,
        encrypted_data,
        Some("application/octet-stream"),
        /* is_encrypted */ true,
        progress_callback,
        None,
        None,
        None, // No cancel flag
    )
    .await?;

    // Pre-cache the original (decrypted) image so the creator sees it instantly
    let cached_path = match crate::image_cache::precache_image_bytes(
        &handle,
        &blob_url,
        &bytes,
        crate::image_cache::ImageType::Avatar,
    ) {
        crate::image_cache::CacheResult::Cached(p) | crate::image_cache::CacheResult::AlreadyCached(p) => Some(p),
        _ => None,
    };

    // Return encryption metadata as hex strings + cached path
    Ok(serde_json::json!({
        "image_hash": bytes_to_hex_string(&prepared.encrypted_hash),
        "image_key": bytes_to_hex_string(prepared.image_key.as_ref()),
        "image_nonce": bytes_to_hex_string(prepared.image_nonce.as_ref()),
        "blob_url": blob_url,
        "cached_path": cached_path,
    }))
}

/// Download, decrypt, and cache a group avatar from Blossom.
///
/// Reads image encryption metadata (hash/key/nonce) from the MDK engine's stored Group,
/// downloads the encrypted blob from Blossom, decrypts with ChaCha20-Poly1305,
/// and caches the result locally using the image cache system.
#[tauri::command]
pub async fn cache_group_avatar(
    group_id: String,
    blob_url: Option<String>,
    image_hash: Option<String>,
    image_key: Option<String>,
    image_nonce: Option<String>,
) -> Result<Option<String>, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();

    // If direct image params are provided (admin just uploaded), use them directly
    // instead of reading from MLS engine state (which may not have merged the commit yet)
    let direct_params = if let (Some(ref h), Some(ref k), Some(ref n)) = (&image_hash, &image_key, &image_nonce) {
        if !h.is_empty() && !k.is_empty() && !n.is_empty() {
            let hash_bytes: [u8; 32] = hex_string_to_bytes(h)
                .try_into().map_err(|_| "Invalid image_hash length")?;
            let key_bytes: [u8; 32] = hex_string_to_bytes(k)
                .try_into().map_err(|_| "Invalid image_key length")?;
            let nonce_bytes: [u8; 12] = hex_string_to_bytes(n)
                .try_into().map_err(|_| "Invalid image_nonce length")?;
            Some((hash_bytes, key_bytes, nonce_bytes))
        } else {
            None
        }
    } else {
        None
    };

    // Load group metadata from SQL
    let groups = db::load_mls_groups().await
        .map_err(|e| format!("Failed to load MLS groups: {}", e))?;
    let meta = groups.iter().find(|g| g.group_id == group_id)
        .ok_or_else(|| format!("Group not found: {}", group_id))?;

    // Only use cached path if we're NOT doing a direct update (no direct params)
    if direct_params.is_none() {
        if let Some(ref cached) = meta.profile.avatar_cached {
            if !cached.is_empty() {
                return Ok(Some(cached.clone()));
            }
        }
    }

    let avatar_ref = meta.profile.avatar_ref.clone();
    let engine_group_id = meta.engine_group_id.clone();

    let (image_hash_bytes, image_key_bytes, image_nonce_bytes) = if let Some(params) = direct_params {
        params
    } else {
        // Read image encryption data from the MDK engine's stored Group
        let image_data = tokio::task::spawn_blocking({
            move || -> Result<Option<([u8; 32], [u8; 32], [u8; 12])>, String> {
                let mls = MlsService::new_persistent_static().map_err(|e| e.to_string())?;
                let engine = mls.engine().map_err(|e| e.to_string())?;

                // Find our group in the engine by engine_group_id
                let engine_gid_bytes = hex_string_to_bytes(&engine_group_id);
                let mls_group_id = mdk_core::prelude::GroupId::from_slice(&engine_gid_bytes);
                let group = engine.get_group(&mls_group_id)
                    .map_err(|e| format!("Engine error: {}", e))?
                    .ok_or_else(|| "Group not found in engine".to_string())?;

                // All three fields must be present for decryption
                match (group.image_hash, &group.image_key, &group.image_nonce) {
                    (Some(hash), Some(key), Some(nonce)) => {
                        Ok(Some((hash, *key.as_ref(), *nonce.as_ref())))
                    }
                    _ => Ok(None), // No image data — group has no avatar
                }
            }
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))??;

        match image_data {
            Some(data) => data,
            None => return Ok(None), // Group has no avatar — not an error
        }
    };

    // Construct download URL: prefer blob_url param, then avatar_ref, then Blossom servers by hash
    let hash_hex = bytes_to_hex_string(&image_hash_bytes);
    let download_urls: Vec<String> = if let Some(ref url) = blob_url {
        if !url.is_empty() {
            vec![url.clone()]
        } else if let Some(ref url) = avatar_ref {
            if !url.is_empty() { vec![url.clone()] } else {
                crate::get_blossom_servers().iter()
                    .map(|s| format!("{}/{}", s.trim_end_matches('/'), hash_hex))
                    .collect()
            }
        } else {
            crate::get_blossom_servers().iter()
                .map(|s| format!("{}/{}", s.trim_end_matches('/'), hash_hex))
                .collect()
        }
    } else if let Some(ref url) = avatar_ref {
        if !url.is_empty() {
            vec![url.clone()]
        } else {
            crate::get_blossom_servers().iter()
                .map(|s| format!("{}/{}", s.trim_end_matches('/'), hash_hex))
                .collect()
        }
    } else {
        crate::get_blossom_servers().iter()
            .map(|s| format!("{}/{}", s.trim_end_matches('/'), hash_hex))
            .collect()
    };

    // Try downloading from each URL until one succeeds. Build via vector-core
    // so the Tor failsafe (block clearnet when Tor is enabled but not active)
    // applies to MLS group avatar downloads too.
    let client = vector_core::net::build_http_client(std::time::Duration::from_secs(30))?;

    const MAX_AVATAR_SIZE: usize = 10 * 1024 * 1024; // 10 MB
    let mut encrypted_data: Option<Vec<u8>> = None;
    let mut successful_url = String::new();
    for url in &download_urls {
        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                // Reject oversized responses before buffering
                if let Some(len) = resp.content_length() {
                    if len as usize > MAX_AVATAR_SIZE { continue; }
                }
                match resp.bytes().await {
                    Ok(data) if data.len() <= MAX_AVATAR_SIZE => {
                        successful_url = url.clone();
                        encrypted_data = Some(data.to_vec());
                        break;
                    }
                    _ => continue,
                }
            }
            _ => continue,
        }
    }

    let encrypted = encrypted_data
        .ok_or_else(|| "Failed to download group avatar from any Blossom server".to_string())?;

    // Decrypt the image using MDK
    let image_key_secret = mdk_storage_traits::Secret::new(image_key_bytes);
    let image_nonce_secret = mdk_storage_traits::Secret::new(image_nonce_bytes);
    let decrypted = mdk_core::extension::decrypt_group_image(
        &encrypted,
        Some(&image_hash_bytes),
        &image_key_secret,
        &image_nonce_secret,
    ).map_err(|e| format!("Failed to decrypt group avatar: {}", e))?;

    // Cache the decrypted image
    let cache_url = if !successful_url.is_empty() { &successful_url } else { &hash_hex };
    let cached_path = match crate::image_cache::precache_image_bytes(
        &handle,
        cache_url,
        &decrypted,
        crate::image_cache::ImageType::Avatar,
    ) {
        crate::image_cache::CacheResult::Cached(p) | crate::image_cache::CacheResult::AlreadyCached(p) => p,
        crate::image_cache::CacheResult::Failed(e) => return Err(format!("Failed to cache avatar: {}", e)),
    };

    // Update avatar_cached in DB with a targeted UPDATE (no full reload)
    let needs_ref = avatar_ref.is_none() || avatar_ref.as_deref() == Some("");
    let ref_to_set = if needs_ref && !successful_url.is_empty() { Some(successful_url.as_str()) } else { None };
    db::update_mls_group_avatar(&group_id, &cached_path, ref_to_set)
        .map_err(|e| format!("Failed to update group avatar in DB: {}", e))?;

    // Emit metadata event from the already-loaded metadata (mutated in place)
    let mut updated_meta = meta.clone();
    updated_meta.profile.avatar_cached = Some(cached_path.clone());
    if let Some(url) = ref_to_set {
        updated_meta.profile.avatar_ref = Some(url.to_string());
    }
    mls::emit_group_metadata_event(&updated_meta);

    println!("[MLS] Cached group avatar for {}: {}", &group_id[..8.min(group_id.len())], cached_path);
    Ok(Some(cached_path))
}

/// Cache a group avatar from a pending invite's image encryption data.
/// Unlike cache_group_avatar which reads keys from the engine, this takes
/// the image hash/key/nonce directly from the welcome metadata.
#[tauri::command]
pub async fn cache_invite_avatar(
    image_hash: String,
    image_key: String,
    image_nonce: String,
) -> Result<Option<String>, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();

    // Parse hex strings to byte arrays
    let hash_bytes: [u8; 32] = hex_string_to_bytes(&image_hash)
        .try_into().map_err(|_| "Invalid image_hash length")?;
    let key_bytes: [u8; 32] = hex_string_to_bytes(&image_key)
        .try_into().map_err(|_| "Invalid image_key length")?;
    let nonce_bytes: [u8; 12] = hex_string_to_bytes(&image_nonce)
        .try_into().map_err(|_| "Invalid image_nonce length")?;

    // Check if already cached by hash
    let cache_key = &image_hash;
    if let Some(existing) = crate::image_cache::get_cached_path(&handle, cache_key, crate::image_cache::ImageType::Avatar) {
        return Ok(Some(existing));
    }

    // Build download URLs from Blossom servers
    let download_urls: Vec<String> = crate::get_blossom_servers().iter()
        .map(|s| format!("{}/{}", s.trim_end_matches('/'), image_hash))
        .collect();

    // Download encrypted blob via vector-core so the Tor failsafe applies.
    let client = vector_core::net::build_http_client(std::time::Duration::from_secs(30))?;

    const MAX_AVATAR_SIZE: usize = 10 * 1024 * 1024; // 10 MB
    let mut encrypted_data: Option<Vec<u8>> = None;
    for url in &download_urls {
        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Some(len) = resp.content_length() {
                    if len as usize > MAX_AVATAR_SIZE { continue; }
                }
                match resp.bytes().await {
                    Ok(data) if data.len() <= MAX_AVATAR_SIZE => {
                        encrypted_data = Some(data.to_vec());
                        break;
                    }
                    _ => continue,
                }
            }
            _ => continue,
        }
    }

    let encrypted = encrypted_data
        .ok_or_else(|| "Failed to download invite avatar from any Blossom server".to_string())?;

    // Decrypt
    let key_secret = mdk_storage_traits::Secret::new(key_bytes);
    let nonce_secret = mdk_storage_traits::Secret::new(nonce_bytes);
    let decrypted = mdk_core::extension::decrypt_group_image(
        &encrypted, Some(&hash_bytes), &key_secret, &nonce_secret,
    ).map_err(|e| format!("Failed to decrypt invite avatar: {}", e))?;

    // Cache
    let cached_path = match crate::image_cache::precache_image_bytes(
        &handle, cache_key, &decrypted, crate::image_cache::ImageType::Avatar,
    ) {
        crate::image_cache::CacheResult::Cached(p) | crate::image_cache::CacheResult::AlreadyCached(p) => p,
        crate::image_cache::CacheResult::Failed(e) => return Err(format!("Failed to cache invite avatar: {}", e)),
    };

    Ok(Some(cached_path))
}

// ============================================================================
// Group Creation & Sync Commands
// ============================================================================

/// Create a new MLS group with initial member devices
#[tauri::command]
pub async fn create_mls_group(
    name: String,
    avatar_ref: Option<String>,
    avatar_cached: Option<String>,
    initial_member_devices: Vec<(String, String)>,
    admin_ids: Vec<String>,
    description: Option<String>,
    image_hash: Option<String>,
    image_key: Option<String>,
    image_nonce: Option<String>,
) -> Result<String, String> {
    // Parse hex strings to byte arrays
    let image_hash_bytes: Option<[u8; 32]> = image_hash.as_deref().and_then(|h| {
        let bytes = hex_string_to_bytes(h);
        if bytes.len() == 32 { Some(bytes.try_into().unwrap()) } else { None }
    });
    let image_key_bytes: Option<[u8; 32]> = image_key.as_deref().and_then(|k| {
        let bytes = hex_string_to_bytes(k);
        if bytes.len() == 32 { Some(bytes.try_into().unwrap()) } else { None }
    });
    let image_nonce_bytes: Option<[u8; 12]> = image_nonce.as_deref().and_then(|n| {
        let bytes = hex_string_to_bytes(n);
        if bytes.len() == 12 { Some(bytes.try_into().unwrap()) } else { None }
    });

    // Use tokio::task::spawn_blocking to run the non-Send MlsService in a blocking context
    let group_id = tokio::task::spawn_blocking(move || {
        TAURI_APP.get().ok_or("App handle not initialized")?;

        // Use tokio runtime to run async code from blocking context
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent_static().map_err(|e| e.to_string())?;
            mls.create_group(
                &name,
                avatar_ref.as_deref(),
                avatar_cached.as_deref(),
                &initial_member_devices,
                description.as_deref(),
                image_hash_bytes,
                image_key_bytes,
                image_nonce_bytes,
                &admin_ids,
            )
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))??;

    // Refresh the live MLS subscription to include the new group
    crate::services::subscription_handler::refresh_mls_subscription().await;

    Ok(group_id)
}

/// Create an MLS group from a group name + member npubs (multi-device aware)
/// - Validates non-empty group name and at least one member
/// - For each member npub, refreshes their latest device keypackage(s)
/// - If any member fails refresh or has zero keypackages, aborts with a clear error
/// - Creates the MLS group and persists metadata so it's immediately discoverable
///
/// Note on device selection policy:
/// - refresh_keypackages_for_contact(npub) returns Vec<(device_id, keypackage_ref)>
/// - For now we choose the first returned device as the member's device to add
///   This can be evolved to pick "newest" by fetched_at if exposed; UI can later allow device selection.
///
/// Frontend will invoke this command via: invoke('create_group_chat', { groupName, memberIds, ... })
#[tauri::command]
pub async fn create_group_chat(
    group_name: String,
    member_ids: Vec<String>,
    admin_ids: Vec<String>,
    group_description: Option<String>,
    image_hash: Option<String>,
    image_key: Option<String>,
    image_nonce: Option<String>,
    avatar_blob_url: Option<String>,
    avatar_cached: Option<String>,
) -> Result<String, String> {
    // Input validation
    let name = group_name.trim();
    if name.is_empty() {
        return Err("Group name must not be empty".to_string());
    }
    if member_ids.is_empty() {
        return Err("Select at least one member to create a group".to_string());
    }

    // For each member id (npub), refresh keypackages and pick one device to add
    let mut initial_member_devices: Vec<(String, String)> = Vec::with_capacity(member_ids.len());

    for npub in member_ids {
        // Attempt to refresh and fetch device keypackages for this contact
        // If this fails for any reason, abort group creation with actionable error text
        let devices = refresh_keypackages_for_contact(npub.clone()).await.map_err(|e| {
            format!("Failed to refresh device keypackage for {}: {}", npub, e)
        })?;

        // Choose a device. Currently: first entry. Future: prefer newest by fetched_at if available.
        let (device_id, _kp_ref) = devices
            .into_iter()
            .next()
            .ok_or_else(|| format!("No device keypackages found for {}", npub))?;

        // Shape required by create_mls_group: (member_npub, device_id)
        initial_member_devices.push((npub, device_id));
    }

    // Delegate to existing helper that persists metadata, publishes welcomes and emits UI events
    let result = create_mls_group(
        name.to_string(),
        avatar_blob_url,
        avatar_cached,
        initial_member_devices,
        admin_ids,
        group_description,
        image_hash,
        image_key,
        image_nonce,
    ).await;

    // Note: vector-core's create_group() auto-rotates the keypackage after success.
    result
}

/// Invite one or more members to an existing MLS group in a single commit
#[tauri::command]
pub async fn invite_member_to_group(
    group_id: String,
    member_npubs: Vec<String>,
) -> Result<(), String> {
    // Resolve keypackages for all members upfront (fail early if any member has no device)
    let mut member_devices: Vec<(String, String)> = Vec::new();
    for npub in &member_npubs {
        let devices = refresh_keypackages_for_contact(npub.clone()).await.map_err(|e| {
            format!("Failed to refresh device keypackage for {}: {}", npub, e)
        })?;

        let (device_id, _kp_ref) = devices
            .into_iter()
            .next()
            .ok_or_else(|| format!("No device keypackages found for {}", npub))?;

        member_devices.push((npub.clone(), device_id));
    }

    // Run non-Send MLS engine work on a blocking thread
    let group_id_clone = group_id.clone();
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent_static().map_err(|e| e.to_string())?;
            mls.add_member_devices(&group_id_clone, &member_devices)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))??;

    // Participant sync happens inside the background task after merge completes

    Ok(())
}

/// Remove a member device from an MLS group
#[tauri::command]
pub async fn remove_mls_member_device(
    group_id: String,
    member_npub: String,
    device_id: String,
) -> Result<(), String> {
    // Run non-Send MLS engine work on a blocking thread; drive async via current runtime
    let group_id_clone = group_id.clone();
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent_static().map_err(|e| e.to_string())?;
            mls.remove_member_device(&group_id_clone, &member_npub, &device_id)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))??;

    // Participant sync happens inside the background task after merge completes

    Ok(())
}

/// Update group metadata (name, description, avatar, admins) — admin only
#[tauri::command]
pub async fn update_group_metadata(
    group_id: String,
    name: Option<String>,
    description: Option<String>,
    admin_ids: Option<Vec<String>>,
    image_hash: Option<String>,
    image_key: Option<String>,
    image_nonce: Option<String>,
) -> Result<(), String> {
    // Parse image fields: None = no change, Some("") = clear, Some(hex) = set
    let image_hash_parsed: Option<Option<[u8; 32]>> = image_hash.as_deref().map(|h| {
        if h.is_empty() { return None; }
        let bytes = hex_string_to_bytes(h);
        if bytes.len() == 32 { Some(bytes.try_into().unwrap()) } else { None }
    });
    let image_key_parsed: Option<Option<[u8; 32]>> = image_key.as_deref().map(|k| {
        if k.is_empty() { return None; }
        let bytes = hex_string_to_bytes(k);
        if bytes.len() == 32 { Some(bytes.try_into().unwrap()) } else { None }
    });
    let image_nonce_parsed: Option<Option<[u8; 12]>> = image_nonce.as_deref().map(|n| {
        if n.is_empty() { return None; }
        let bytes = hex_string_to_bytes(n);
        if bytes.len() == 12 { Some(bytes.try_into().unwrap()) } else { None }
    });

    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent_static().map_err(|e| e.to_string())?;
            mls.update_group_data(
                &group_id,
                name,
                description,
                admin_ids,
                image_hash_parsed,
                image_key_parsed,
                image_nonce_parsed,
            )
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Sync MLS groups with the network via NIP-77 negentropy.
/// If `group_id` is provided, sync only that group; otherwise sync all groups.
/// Full-set reconciliation (no recency window) — the catch-up / archive path.
#[tauri::command]
pub async fn sync_mls_groups_now(
    group_id: Option<String>,
) -> Result<(u32, u32), String> {
    reconcile_and_apply_mls(group_id, None).await
}

/// Quick MLS group sync — negentropy reconciliation over the last 7 days.
/// Near-instant when already up to date; dormant groups simply have no
/// in-window delta and are caught up by the archive path instead.
pub async fn sync_mls_groups_quick() -> Result<(u32, u32), String> {
    let since = Timestamp::now().as_secs().saturating_sub(7 * 24 * 3600);
    reconcile_and_apply_mls(None, Some(since)).await
}

/// Archive MLS group sync — full-set negentropy reconciliation across all
/// non-evicted groups. This is the long-offline recovery path: negentropy
/// finds every event we're missing regardless of age, and the per-group
/// apply ratchets them in order.
pub async fn sync_mls_groups_archive() -> Result<(u32, u32), String> {
    reconcile_and_apply_mls(None, None).await
}

/// Reconcile MLS group messages via NIP-77 negentropy and apply them in order.
///
/// Thin Tauri-side wrapper: the engine is non-Send, so the work runs on a
/// blocking thread. All logic lives in vector-core's `reconcile_and_apply`.
///
/// - `only_group`: restrict to one group (`None` = all non-evicted).
/// - `since`: `Some(ts)` bounds the reconcile window (quick); `None` floors at
///   the oldest group's `created_at` for a full catch-up.
async fn reconcile_and_apply_mls(
    only_group: Option<String>,
    since: Option<u64>,
) -> Result<(u32, u32), String> {
    // Pin to the session that requested this sync. The fetched events belong to
    // the account active now; a mid-flight swap would otherwise apply account
    // A's messages into account B's storage. (reconcile_and_apply re-checks too.)
    let session = vector_core::state::SessionGuard::capture();
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            if !session.is_valid() { return Ok((0u32, 0u32)); }
            let mls = MlsService::new_persistent_static().map_err(|e| e.to_string())?;
            mls.reconcile_and_apply(only_group.as_deref(), since)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

// ============================================================================
// Welcome/Invite Commands
// ============================================================================

/// Simplified representation of a pending MLS Welcome for UI
#[derive(serde::Serialize)]
pub struct SimpleWelcome {
    // Welcome event id (rumor id) hex
    pub id: String,
    // Wrapper id carrying the welcome (giftwrap id) hex
    pub wrapper_event_id: String,
    // Group metadata
    pub nostr_group_id: String,
    pub group_name: String,
    pub group_description: Option<String>,
    pub group_image_url: Option<String>,
    pub avatar_cached: Option<String>,
    // Image encryption data (hex-encoded) for frontend-triggered caching
    pub image_hash: Option<String>,
    pub image_key: Option<String>,
    pub image_nonce: Option<String>,
    // Admins (npub strings if possible are not available here; expose hex pubkeys)
    pub group_admin_pubkeys: Vec<String>,
    // Relay URLs
    pub group_relays: Vec<String>,
    // Welcomer (hex)
    pub welcomer: String,
    pub member_count: u32,
    // Timestamp of the welcome event (for deduplication - keep most recent)
    pub created_at: u64,
}

/// List pending MLS welcomes (invites).
///
/// Delegates the fetch to vector-core, then layers Tauri-specific
/// OS notifications + NOTIFIED_WELCOMES tracking on top.
#[tauri::command]
pub async fn list_pending_mls_welcomes() -> Result<Vec<SimpleWelcome>, String> {
    let core = vector_core::VectorCore;
    let invites = core.list_invites().await.map_err(|e| e.to_string())?;

    // Convert to Tauri's SimpleWelcome shape for frontend compatibility
    let welcomes: Vec<SimpleWelcome> = invites.into_iter().map(|i| SimpleWelcome {
        id: i.welcome_event_id,
        wrapper_event_id: i.wrapper_event_id,
        nostr_group_id: i.group_id,
        group_name: i.group_name,
        group_description: i.group_description,
        group_image_url: None,
        avatar_cached: None, // Filled by cache_invite_avatar
        image_hash: i.image_hash,
        image_key: i.image_key,
        image_nonce: i.image_nonce,
        group_admin_pubkeys: i.admin_npubs,
        group_relays: i.relays,
        welcomer: i.welcomer_npub,
        member_count: i.member_count,
        created_at: i.created_at,
    }).collect();

    // Tauri-specific: OS notifications for new invites
    {
        let mut notified = NOTIFIED_WELCOMES.lock().await;
        for welcome in &welcomes {
            if notified.contains(&welcome.wrapper_event_id) {
                continue;
            }

            let (inviter_name, avatar) = {
                let state = STATE.lock().await;
                if let Some(profile) = state.get_profile(&welcome.welcomer) {
                    let name = if !profile.nickname.is_empty() {
                        profile.nickname.to_string()
                    } else if !profile.name.is_empty() {
                        profile.name.to_string()
                    } else {
                        "Someone".to_string()
                    };
                    let cached = if !profile.avatar_cached.is_empty() {
                        Some(profile.avatar_cached.to_string())
                    } else {
                        None
                    };
                    (name, cached)
                } else {
                    ("Someone".to_string(), None)
                }
            };

            let notification = NotificationData::group_invite(welcome.group_name.clone(), inviter_name, avatar);
            show_notification_generic(notification);
            notified.insert(welcome.wrapper_event_id.clone());
        }
    }

    Ok(welcomes)
}

/// Accept an MLS welcome by its welcome (rumor) event id hex.
///
/// Delegates core MLS join logic (accept, persist, sync) to vector-core.
/// Adds Tauri-specific concerns: avatar caching, keypackage regeneration,
/// UI event emit, and notification cleanup.
#[tauri::command]
pub async fn accept_mls_welcome(welcome_event_id_hex: String) -> Result<bool, String> {
    let core = vector_core::VectorCore;

    // Capture wrapper_event_id + welcomer for notification cleanup (before welcome is consumed)
    let wrapper_event_id_hex: Option<String> = tokio::task::spawn_blocking({
        let id = welcome_event_id_hex.clone();
        move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                let mls = MlsService::new_persistent_static().ok()?;
                let engine = mls.engine().ok()?;
                let ev_id = nostr_sdk::EventId::from_hex(&id).ok()?;
                let welcome = engine.get_welcome(&ev_id).ok()??;
                Some(welcome.wrapper_event_id.to_hex())
            })
        }
    })
    .await
    .unwrap_or(None);

    // Core accept flow — delegates to vector-core
    let nostr_group_id = core.accept_invite(&welcome_event_id_hex).await
        .map_err(|e| e.to_string())?;

    // Tauri-specific: remove from notified set
    if let Some(wid) = wrapper_event_id_hex {
        let mut notified = NOTIFIED_WELCOMES.lock().await;
        notified.remove(&wid);
    }

    // Tauri-specific: emit UI event
    if let Some(app) = TAURI_APP.get() {
        let _ = app.emit("mls_welcome_accepted", serde_json::json!({
            "welcome_event_id": welcome_event_id_hex,
            "group_id": nostr_group_id
        }));
    }

    // Tauri-specific: emit initial sync event (core already did the sync)
    if let Some(app) = TAURI_APP.get() {
        let _ = app.emit("mls_group_initial_sync", serde_json::json!({
            "group_id": nostr_group_id,
        }));
    }

    // Note: vector-core's accept_invite() auto-rotates the keypackage after success.
    let gid_for_avatar = nostr_group_id.clone();
    // SessionGuard: cache path is account-scoped; without this gate
    // account A's group avatar could land in account B's cache dir.
    let session = vector_core::state::SessionGuard::capture();
    tokio::spawn(async move {
        if !session.is_valid() { return; }
        match cache_group_avatar(gid_for_avatar.clone(), None, None, None, None).await {
            Ok(Some(path)) => println!("[MLS] Cached group avatar after welcome: {}", path),
            Ok(None) => {}
            Err(e) => eprintln!("[MLS] Failed to cache group avatar after welcome for {}: {}", &gid_for_avatar[..8.min(gid_for_avatar.len())], e),
        }
    });

    Ok(true)
}

// Handler list for this module (18 commands):
// - load_mls_device_id
// - load_mls_keypackages
// - list_mls_groups
// - get_mls_group_metadata
// - list_group_cursors
// - leave_mls_group
// - get_mls_group_members
// - refresh_keypackages_for_contact
// - add_mls_member_device
// - create_mls_group
// - create_group_chat
// - invite_member_to_group
// - remove_mls_member_device
// - sync_mls_groups_now
// - sync_mls_group_participants (pub(crate) helper)
// - list_pending_mls_welcomes (+SimpleWelcome struct)
// - regenerate_device_keypackage
// - accept_mls_welcome
