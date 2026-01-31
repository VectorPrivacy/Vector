//! MLS (Messaging Layer Security) Tauri commands.
//!
//! This module handles MLS group messaging operations:
//! - Device and keypackage management
//! - Group creation and membership
//! - Welcome message handling
//! - Group metadata and member queries

use nostr_sdk::prelude::*;
use rand::{thread_rng, Rng};
use rand::distributions::Alphanumeric;
use tauri::Emitter;
use crate::{db, mls, MlsService, NotificationData, show_notification_generic, NOSTR_CLIENT, NOTIFIED_WELCOMES, STATE, TAURI_APP, TRUSTED_RELAYS};

// ============================================================================
// Device & KeyPackage Read Commands
// ============================================================================

/// Load MLS device ID for the current account
#[tauri::command]
pub async fn load_mls_device_id() -> Result<Option<String>, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
    match db::load_mls_device_id(&handle).await {
        Ok(Some(id)) => Ok(Some(id)),
        Ok(None) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Load MLS keypackages for the current account
#[tauri::command]
pub async fn load_mls_keypackages() -> Result<Vec<serde_json::Value>, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
    db::load_mls_keypackages(&handle).await
        .map_err(|e| e.to_string())
}

/// Regenerate this device's MLS KeyPackage. If `cache` is true, attempt to reuse an existing
/// cached KeyPackage if it exists on the relay; otherwise always generate a fresh one.
#[tauri::command]
pub async fn regenerate_device_keypackage(cache: bool) -> Result<serde_json::Value, String> {
    // Access handle and client
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;

    // Ensure a persistent device_id exists
    let device_id: String = match db::load_mls_device_id(&handle).await {
        Ok(Some(id)) => id,
        _ => {
            let id: String = thread_rng()
                .sample_iter(&Alphanumeric)
                .take(12)
                .map(char::from)
                .collect::<String>()
                .to_lowercase();
            let _ = db::save_mls_device_id(handle.clone(), &id).await;
            id
        }
    };

    // Resolve my pubkey (awaits before any MLS engine is created)
    let signer = client.signer().await.map_err(|e| e.to_string())?;
    let my_pubkey = signer.get_public_key().await.map_err(|e| e.to_string())?;
    let owner_pubkey_b32 = my_pubkey.to_bech32().map_err(|e| e.to_string())?;

    // If caching is requested, attempt to load and verify an existing KeyPackage
    if cache {
        // Load existing keypackage index and verify it exists on relay before returning cached
        let cached_kp_ref: Option<String> = {
            let index = db::load_mls_keypackages(&handle).await.unwrap_or_default();

            index.iter().find(|entry| {
                entry.get("owner_pubkey").and_then(|v| v.as_str()) == Some(owner_pubkey_b32.as_str())
                    && entry.get("device_id").and_then(|v| v.as_str()) == Some(device_id.as_str())
            })
            .and_then(|existing| existing.get("keypackage_ref").and_then(|v| v.as_str()).map(|s| s.to_string()))
        };

        // If we have a cached reference, verify it exists on the relay
        if let Some(ref_id) = cached_kp_ref {
            println!("[MLS][KeyPackage] Found cached reference {}, verifying on relay...", ref_id);

            // Try to fetch the event from the relay to verify it exists
            if let Ok(event_id) = nostr_sdk::EventId::from_hex(&ref_id) {
                let filter = Filter::new()
                    .id(event_id)
                    .kind(Kind::MlsKeyPackage)
                    .limit(1);

                match client.stream_events_from(
                    TRUSTED_RELAYS.to_vec(),
                    filter,
                    std::time::Duration::from_secs(5)
                ).await {
                    Ok(mut events) => {
                        // Check if we got any events - if so, verify it has the encoding tag
                        if let Some(event) = events.next().await {
                            // Check for encoding tag (MIP-00/MIP-02 requirement)
                            let has_encoding = event.tags.iter().any(|tag| {
                                let slice = tag.as_slice();
                                slice.len() >= 2 && slice[0] == "encoding" && slice[1] == "base64"
                            });

                            if has_encoding {
                                println!("[MLS][KeyPackage] Cached keypackage has encoding tag, using cached");
                                return Ok(serde_json::json!({
                                    "device_id": device_id,
                                    "owner_pubkey": owner_pubkey_b32,
                                    "keypackage_ref": ref_id,
                                    "cached": true
                                }));
                            } else {
                                println!("[MLS][KeyPackage] Cached keypackage missing encoding tag, will regenerate");
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Create device KeyPackage using persistent MLS engine inside a no-await scope
    let (kp_encoded, kp_tags) = {
        let mls_service = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
        let engine = mls_service.engine().map_err(|e| e.to_string())?;
        let relay_urls: Vec<nostr_sdk::RelayUrl> = TRUSTED_RELAYS
            .iter()
            .filter_map(|r| nostr_sdk::RelayUrl::parse(r).ok())
            .collect();
        engine
            .create_key_package_for_event(&my_pubkey, relay_urls)
            .map_err(|e| e.to_string())?
    }; // engine and mls_service dropped here before any await

    // Filter out the protected tag ("-") which causes many relays to reject the event.
    // MDK adds this tag but it breaks compatibility with relays enforcing NIP-70.
    let filtered_tags: Vec<_> = kp_tags
        .into_iter()
        .filter(|t| t.as_slice().first().map(|s| s.as_str()) != Some("-"))
        .collect();

    // Build and sign event with nostr client
    let kp_event = client
        .sign_event_builder(EventBuilder::new(Kind::MlsKeyPackage, kp_encoded).tags(filtered_tags))
        .await
        .map_err(|e| e.to_string())?;

    // Debug: Print event details before publishing
    println!("[MLS KeyPackage] Event ID: {}", kp_event.id.to_hex());
    println!("[MLS KeyPackage] Kind: {}", kp_event.kind.as_u16());
    println!("[MLS KeyPackage] Tags count: {}", kp_event.tags.len());
    for (i, tag) in kp_event.tags.iter().enumerate() {
        println!("[MLS KeyPackage] Tag {}: {:?}", i, tag.as_slice());
    }

    // Publish to TRUSTED_RELAYS with retry logic for slow connections
    let mut send_result = None;
    let mut last_error = String::new();
    for attempt in 1..=3 {
        match client.send_event_to(TRUSTED_RELAYS.iter().copied(), &kp_event).await {
            Ok(result) => {
                // Check if at least one relay succeeded
                if !result.success.is_empty() {
                    println!("[MLS KeyPackage] Publish succeeded on attempt {}: {:?}", attempt, result);
                    send_result = Some(result);
                    break;
                } else {
                    // All relays failed, retry
                    println!("[MLS KeyPackage] Attempt {}/3: all relays failed, retrying in {}s...",
                        attempt, attempt * 5);
                    last_error = format!("All relays failed: {:?}", result.failed);
                    if attempt < 3 {
                        tokio::time::sleep(std::time::Duration::from_secs((attempt * 5) as u64)).await;
                    }
                }
            }
            Err(e) => {
                println!("[MLS KeyPackage] Attempt {}/3 error: {}", attempt, e);
                last_error = e.to_string();
                if attempt < 3 {
                    tokio::time::sleep(std::time::Duration::from_secs((attempt * 5) as u64)).await;
                }
            }
        }
    }

    // If no successful publish after retries, return error
    let send_result = send_result.ok_or_else(|| {
        format!("Failed to publish keypackage after 3 attempts: {}", last_error)
    })?;

    println!("[MLS KeyPackage] Publish result: {:?}", send_result);

    // Upsert into mls_keypackage_index
    {
        let mut index = db::load_mls_keypackages(&handle).await.unwrap_or_default();
        let now = Timestamp::now().as_secs();
        let new_kp_ref = kp_event.id.to_hex();

        // Remove any existing entries that match either:
        // 1. Same owner+device (old keypackage from this device)
        // 2. Same keypackage_ref (stale network entry with wrong device_id)
        index.retain(|entry| {
            let same_owner = entry.get("owner_pubkey").and_then(|v| v.as_str()) == Some(&owner_pubkey_b32);
            let same_device = entry.get("device_id").and_then(|v| v.as_str()) == Some(&device_id);
            let same_ref = entry.get("keypackage_ref").and_then(|v| v.as_str()) == Some(&new_kp_ref);
            // Keep entries that don't match either condition
            !((same_owner && same_device) || same_ref)
        });

        index.push(serde_json::json!({
            "owner_pubkey": owner_pubkey_b32,
            "device_id": device_id,
            "keypackage_ref": new_kp_ref,
            "created_at": kp_event.created_at.as_secs(),
            "fetched_at": now,
            "expires_at": 0u64
        }));

        let _ = db::save_mls_keypackages(handle.clone(), &index).await;
    }

    Ok(serde_json::json!({
        "device_id": device_id,
        "owner_pubkey": owner_pubkey_b32,
        "keypackage_ref": kp_event.id.to_hex(),
        "cached": false
    }))
}

// ============================================================================
// Group Query Commands
// ============================================================================

/// List all MLS group IDs
#[tauri::command]
pub async fn list_mls_groups() -> Result<Vec<String>, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
    match db::load_mls_groups(&handle).await {
        Ok(groups) => {
            let ids = groups.into_iter()
                .map(|g| g.group_id)
                .collect();
            Ok(ids)
        }
        Err(e) => Err(format!("Failed to load MLS groups: {}", e)),
    }
}

/// Get metadata for all MLS groups (filtered to non-evicted groups)
#[tauri::command]
pub async fn get_mls_group_metadata() -> Result<Vec<serde_json::Value>, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
    let groups = db::load_mls_groups(&handle)
        .await
        .map_err(|e| format!("Failed to load MLS group metadata: {}", e))?;

    Ok(groups
        .iter()
        .filter(|meta| !meta.evicted)
        .map(|meta| mls::metadata_to_frontend(meta))
        .collect())
}

/// List cursors for all MLS groups (for debugging/QA)
#[tauri::command]
pub async fn list_group_cursors() -> Result<serde_json::Value, String> {
    tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            let cursors = mls.read_event_cursors().await.map_err(|e| e.to_string())?;
            serde_json::to_value(&cursors).map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

// ============================================================================
// Group Management Commands
// ============================================================================

/// Leave an MLS group
#[tauri::command]
pub async fn leave_mls_group(group_id: String) -> Result<(), String> {
    // Run non-Send MLS engine work on a blocking thread
    tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            mls.leave_group(&group_id)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
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
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            // Initialise persistent MLS
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            // Map wire-id/engine-id using encrypted metadata
            let meta_groups = mls.read_groups().await.unwrap_or_default();
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
            if let Ok(gid_bytes) = hex::decode(&engine_id) {
                // Decode engine id to GroupId
                let gid = GroupId::from_slice(&gid_bytes);

                // Get members via engine API
                if let Ok(pk_list) = engine.get_members(&gid) {
                    members = pk_list
                        .into_iter()
                        .filter_map(|pk| pk.to_bech32().ok())
                        .collect();
                }

                // Get admins from the group
                if let Ok(groups) = engine.get_groups() {
                    for g in groups {
                        let gid_hex = hex::encode(g.mls_group_id.as_slice());
                        if gid_hex == engine_id {
                            admins = g.admin_pubkeys.iter()
                                .filter_map(|pk| pk.to_bech32().ok())
                                .collect();
                            break;
                        }
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

/// Refresh keypackages for a contact from TRUSTED_RELAY
/// Fetches Kind::MlsKeyPackage from the contact, updates local index, and returns (device_id, keypackage_ref)
#[tauri::command]
pub async fn refresh_keypackages_for_contact(
    npub: String,
) -> Result<Vec<(String, String)>, String> {
    // Resolve contact pubkey
    let contact_pubkey = PublicKey::from_bech32(&npub).map_err(|e| e.to_string())?;

    // Access client
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;

    // Build filter: author(contact) + MlsKeyPackage
    let filter = Filter::new()
        .author(contact_pubkey)
        .kind(Kind::MlsKeyPackage)
        // Only need the newest KeyPackage
        .limit(1);

    // Fetch from TRUSTED_RELAYS with short timeout
    let mut events = client
        .stream_events_from(TRUSTED_RELAYS.to_vec(), filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;

    // Prepare results and index entries
    let owner_pubkey_b32 = contact_pubkey.to_bech32().map_err(|e| e.to_string())?;
    let mut results: Vec<(String, String)> = Vec::new();
    let mut new_entries: Vec<serde_json::Value> = Vec::new();

    while let Some(e) = events.next().await {
        // Use event id as synthetic device_id when not explicitly provided by remote
        let device_id = e.id.to_hex();
        let keypackage_ref = e.id.to_hex();

        results.push((device_id.clone(), keypackage_ref.clone()));

        new_entries.push(serde_json::json!({
            "owner_pubkey": owner_pubkey_b32,
            "device_id": device_id,
            "keypackage_ref": keypackage_ref,
            "created_at": e.created_at.as_secs(),
            "fetched_at": Timestamp::now().as_secs(),
            "expires_at": 0u64
        }));
    }

    // Update local plaintext index after network await
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();

    // Load existing index
    let mut index = db::load_mls_keypackages(&handle).await.unwrap_or_default();

    // Dedup existing entries by keypackage_ref — keep first occurrence per ref.
    // This cleans up stale duplicates where the same keypackage was stored twice
    // (once with the real device_id from local generation, once with event_id from network).
    {
        let mut seen_refs = std::collections::HashSet::new();
        index.retain(|entry| {
            let r = entry.get("keypackage_ref").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            seen_refs.insert(r)
        });
    }

    // Merge new entries into the index, preserving local entries that share the same
    // keypackage_ref (they have the correct device_id, whereas network entries use event_id).
    for new_entry in new_entries {
        let new_ref = new_entry.get("keypackage_ref").and_then(|v| v.as_str()).unwrap_or_default();
        let new_owner = new_entry.get("owner_pubkey").and_then(|v| v.as_str()).unwrap_or_default();
        let new_device = new_entry.get("device_id").and_then(|v| v.as_str()).unwrap_or_default();

        // Skip if a local entry already has this keypackage_ref (preserves correct device_id)
        let ref_exists = index.iter().any(|entry| {
            entry.get("keypackage_ref").and_then(|v| v.as_str()) == Some(new_ref)
        });
        if ref_exists {
            continue;
        }

        // Remove any existing entry for the same owner+device_id, then add the new one
        index.retain(|entry| {
            let same_owner = entry.get("owner_pubkey").and_then(|v| v.as_str()) == Some(new_owner);
            let same_device = entry.get("device_id").and_then(|v| v.as_str()) == Some(new_device);
            !(same_owner && same_device)
        });
        index.push(new_entry);
    }

    let _ = db::save_mls_keypackages(handle.clone(), &index).await;

    Ok(results)
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
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            mls.add_member_device(&group_id, &member_npub, &device_id)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

// ============================================================================
// Group Creation & Sync Commands
// ============================================================================

/// Create a new MLS group with initial member devices
#[tauri::command]
pub async fn create_mls_group(
    name: String,
    avatar_ref: Option<String>,
    initial_member_devices: Vec<(String, String)>,
) -> Result<String, String> {
    // Use tokio::task::spawn_blocking to run the non-Send MlsService in a blocking context
    tokio::task::spawn_blocking(move || {
        // Get handle in blocking context
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();

        // Use tokio runtime to run async code from blocking context
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            mls.create_group(&name, avatar_ref.as_deref(), &initial_member_devices)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
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
/// Frontend will invoke this command via: invoke('create_group_chat', { groupName, memberIds })
#[tauri::command]
pub async fn create_group_chat(group_name: String, member_ids: Vec<String>) -> Result<String, String> {
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
    // avatar_ref: None for now (out of scope for this subtask)
    let result = create_mls_group(name.to_string(), None, initial_member_devices).await;

    if result.is_ok() {
        tokio::spawn(async {
            // regenerate_device_keypackage remains in lib.rs for now
            if let Err(err) = regenerate_device_keypackage(false).await {
                eprintln!("[MLS] Failed to regenerate device KeyPackage after group creation: {}", err);
            }
        });
    }

    result
}

/// Invite a new member to an existing MLS group
/// Similar to create_group_chat, this refreshes the member's keypackages and adds them to the group
#[tauri::command]
pub async fn invite_member_to_group(
    group_id: String,
    member_npub: String,
) -> Result<(), String> {
    // Refresh keypackages for the new member
    let devices = refresh_keypackages_for_contact(member_npub.clone()).await.map_err(|e| {
        format!("Failed to refresh device keypackage for {}: {}", member_npub, e)
    })?;

    // Choose the first device (same policy as group creation)
    let (device_id, _kp_ref) = devices
        .into_iter()
        .next()
        .ok_or_else(|| format!("No device keypackages found for {}", member_npub))?;

    // Run non-Send MLS engine work on a blocking thread
    let group_id_clone = group_id.clone();
    tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            mls.add_member_device(&group_id_clone, &member_npub, &device_id)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))??;

    // Sync participants array after adding member
    sync_mls_group_participants(group_id).await?;

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
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            mls.remove_member_device(&group_id_clone, &member_npub, &device_id)
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))??;

    // Sync participants array after removing member
    sync_mls_group_participants(group_id).await?;

    Ok(())
}

/// Sync MLS groups with the network
/// If group_id is provided, sync only that group
/// If None, sync all groups
#[tauri::command]
pub async fn sync_mls_groups_now(
    group_id: Option<String>,
) -> Result<(u32, u32), String> {
    // Run non-Send MLS engine work on blocking thread; drive async via current runtime
    tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;

            if let Some(id) = group_id {
                // Sync specific group since last cursor
                mls.sync_group_since_cursor(&id)
                    .await
                    .map_err(|e| e.to_string())
            } else {
                // Multi-group sync: load MLS groups from SQL and sync each
                let group_ids: Vec<String> = match db::load_mls_groups(&handle).await {
                    Ok(groups) => {
                        groups.into_iter()
                            .filter(|g| !g.evicted) // Skip evicted groups
                            .map(|g| g.group_id)
                            .collect()
                    }
                    Err(e) => {
                        eprintln!("Failed to load MLS groups: {}", e);
                        Vec::new()
                    }
                };

                let mut total_processed: u32 = 0;
                let mut total_new: u32 = 0;

                for gid in group_ids {
                    match mls.sync_group_since_cursor(&gid).await {
                        Ok((processed, new_msgs)) => {
                            total_processed = total_processed.saturating_add(processed);
                            total_new = total_new.saturating_add(new_msgs);
                        }
                        Err(e) => {
                            eprintln!("[MLS] sync_group_since_cursor failed for {}: {}", gid, e);
                        }
                    }

                    // Sync participants array to ensure it matches actual group members
                    if let Err(e) = sync_mls_group_participants(gid.clone()).await {
                        eprintln!("[MLS] Failed to sync participants for group {}: {}", gid, e);
                    }
                }

                Ok((total_processed, total_new))
            }
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Sync the participants array for an MLS group chat with the actual members from the engine
/// This ensures chat.participants is always up-to-date
/// (Internal helper - not a Tauri command)
pub async fn sync_mls_group_participants(group_id: String) -> Result<(), String> {
    // Get actual members from the engine
    let group_members = get_mls_group_members(group_id.clone()).await?;

    // Update the chat's participants array
    let mut state = STATE.lock().await;
    if let Some(chat) = state.get_chat_mut(&group_id) {
        let old_count = chat.participants.len();
        chat.participants = group_members.members.clone();
        let new_count = chat.participants.len();

        if old_count != new_count {
            eprintln!(
                "[MLS] Synced participants for group {}: {} -> {} members",
                &group_id[..8],
                old_count,
                new_count
            );
        }

        // Save updated chat to disk
        let chat_clone = chat.clone();
        drop(state);

        if let Some(handle) = TAURI_APP.get() {
            if let Err(e) = db::save_chat(handle.clone(), &chat_clone).await {
                eprintln!("[MLS] Failed to save chat after syncing participants: {}", e);
            }
        }
    } else {
        drop(state);
        eprintln!("[MLS] Chat not found when syncing participants: {}", group_id);
    }

    Ok(())
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

/// List pending MLS welcomes (invites)
#[tauri::command]
pub async fn list_pending_mls_welcomes() -> Result<Vec<SimpleWelcome>, String> {
    // Run non-Send MLS engine work on blocking thread; drive async via current runtime
    let welcomes: Vec<SimpleWelcome> = tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;
            let engine = mls.engine().map_err(|e| e.to_string())?;

            let pending = engine.get_pending_welcomes(None).map_err(|e| e.to_string())?;

            let mut out: Vec<SimpleWelcome> = Vec::with_capacity(pending.len());
            for w in pending {
                out.push(SimpleWelcome {
                    id: w.id.to_hex(),
                    wrapper_event_id: w.wrapper_event_id.to_hex(),
                    nostr_group_id: hex::encode(w.nostr_group_id),
                    group_name: w.group_name.clone(),
                    group_description: Some(w.group_description.clone()),
                    group_image_url: None, // MDK uses group_image_hash/key/nonce instead of URL
                    group_admin_pubkeys: w.group_admin_pubkeys.iter()
                        .filter_map(|pk| pk.to_bech32().ok())
                        .collect(),
                    group_relays: w.group_relays.iter().map(|r| r.to_string()).collect(),
                    welcomer: w.welcomer.to_bech32().map_err(|e| e.to_string())?,
                    member_count: w.member_count,
                    created_at: w.event.created_at.as_secs(),
                });
            }

            // Deduplicate welcomes by nostr_group_id, keeping only the most recent one
            // (based on event timestamp, not member count which can decrease with kicks)
            let mut deduped: std::collections::HashMap<String, SimpleWelcome> = std::collections::HashMap::new();
            for welcome in out {
                let group_id = welcome.nostr_group_id.clone();
                if let Some(existing) = deduped.get(&group_id) {
                    // Keep the one with the later timestamp (most recent invite)
                    if welcome.created_at > existing.created_at {
                        deduped.insert(group_id, welcome);
                    }
                } else {
                    deduped.insert(group_id, welcome);
                }
            }
            let out: Vec<SimpleWelcome> = deduped.into_values().collect();

            Ok::<Vec<SimpleWelcome>, String>(out)
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))??;

    // Send notifications for new welcomes (outside blocking task)
    // Only notify for welcomes we haven't notified about before
    {
        let mut notified = NOTIFIED_WELCOMES.lock().await;

        for welcome in &welcomes {
            // Skip if we've already notified about this welcome
            if notified.contains(&welcome.wrapper_event_id) {
                continue;
            }

            // Get inviter's display name
            let inviter_name = {
                let state = STATE.lock().await;
                if let Some(profile) = state.get_profile(&welcome.welcomer) {
                    if !profile.nickname.is_empty() {
                        profile.nickname.clone()
                    } else if !profile.name.is_empty() {
                        profile.name.clone()
                    } else {
                        "Someone".to_string()
                    }
                } else {
                    "Someone".to_string()
                }
            };

            let notification = NotificationData::group_invite(welcome.group_name.clone(), inviter_name);
            show_notification_generic(notification);

            // Mark this welcome as notified
            notified.insert(welcome.wrapper_event_id.clone());
        }
    }

    Ok(welcomes)
}

/// Accept an MLS welcome by its welcome (rumor) event id hex
#[tauri::command]
pub async fn accept_mls_welcome(welcome_event_id_hex: String) -> Result<bool, String> {
    // Run non-Send MLS engine work on blocking thread; drive async via current runtime
    let accepted = tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get().ok_or("App handle not initialized")?.clone();
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let mls = MlsService::new_persistent(&handle).map_err(|e| e.to_string())?;

            // Get welcome details and accept it (engine work in no-await scope)
            let (nostr_group_id, engine_group_id, group_name, welcomer_hex, wrapper_event_id_hex, invite_sent_at) = {
                let engine = mls.engine().map_err(|e| e.to_string())?;

                let id = nostr_sdk::EventId::from_hex(&welcome_event_id_hex).map_err(|e| e.to_string())?;
                let welcome_opt = engine.get_welcome(&id).map_err(|e| e.to_string())?;
                let welcome = welcome_opt.ok_or_else(|| "Welcome not found".to_string())?;

                // Extract metadata before accepting
                let nostr_group_id_bytes = welcome.nostr_group_id.clone();
                let group_name = welcome.group_name.clone();
                let welcomer_hex = welcome.welcomer.to_hex();
                let wrapper_event_id_hex = welcome.wrapper_event_id.to_hex();
                // Get the invite-sent timestamp from the welcome event (not acceptance time!)
                // This is critical for accurate sync windows
                let invite_sent_at = welcome.event.created_at.as_secs();

                // Accept the welcome - this updates engine state internally
                engine.accept_welcome(&welcome).map_err(|e| e.to_string())?;

                // The nostr_group_id is used for wire protocol (h tag on relays)
                let nostr_group_id = hex::encode(&nostr_group_id_bytes);

                // After accepting the welcome, get the actual group from the engine to find its internal ID
                // This follows the pattern from the SDK example
                let engine_group_id = {
                    // Get all groups from the engine (should include the one we just joined)
                    let groups = engine.get_groups()
                        .map_err(|e| format!("Failed to get groups after accepting welcome: {}", e))?;

                    // Find the group that matches our nostr_group_id
                    let matching_group = groups.iter()
                        .find(|g| hex::encode(&g.nostr_group_id) == nostr_group_id);

                    if let Some(group) = matching_group {
                        // Found the group - use its internal MLS group ID
                        let engine_id = hex::encode(group.mls_group_id.as_slice());
                        println!("[MLS] Found group in engine after accept:");
                        println!("[MLS]   - nostr_group_id matches: {}", nostr_group_id);
                        println!("[MLS]   - engine mls_group_id: {}", engine_id);
                        engine_id
                    } else {
                        // This shouldn't happen, but fallback to nostr_group_id
                        eprintln!("[MLS] Warning: Could not find group in engine after accepting welcome");
                        eprintln!("[MLS] Groups in engine: {}", groups.len());
                        for g in groups.iter() {
                            eprintln!("[MLS]   - Group: nostr_id={}, mls_id={}",
                                     hex::encode(&g.nostr_group_id),
                                     hex::encode(g.mls_group_id.as_slice()));
                        }
                        // Use the nostr_group_id as fallback
                        nostr_group_id.clone()
                    }
                };

                // Log for debugging
                println!("[MLS] Welcome accepted:");
                println!("[MLS]   - wire_id (h tag): {}", nostr_group_id);
                println!("[MLS]   - engine_group_id: {}", engine_group_id);
                println!("[MLS]   - group_name: {}", group_name);
                println!("[MLS]   - invite_sent_at: {}", invite_sent_at);

                (nostr_group_id, engine_group_id, group_name, welcomer_hex, wrapper_event_id_hex, invite_sent_at)
            }; // engine dropped here

            // Now persist the group metadata (awaitable section)
            let mut groups = mls.read_groups().await.map_err(|e| e.to_string())?;

            // Check if group already exists or was previously evicted
            let existing_index = groups.iter().position(|g| g.group_id == nostr_group_id);

            if let Some(idx) = existing_index {
                // Group exists - check if it was evicted and we're being re-invited
                if groups[idx].evicted {
                    println!("[MLS] Re-invited to previously evicted group: {}", nostr_group_id);
                    // Clear the evicted flag and update metadata
                    groups[idx].evicted = false;
                    // CRITICAL: Update created_at to the NEW invite time, not the old one.
                    // The cursor was removed on eviction, so sync_group_since_cursor will use
                    // created_at as the starting point. If we don't update it, sync will try
                    // to process old events from before the kick that can't be decrypted.
                    groups[idx].created_at = invite_sent_at;
                    groups[idx].updated_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|e| e.to_string())?
                        .as_secs();
                    // Update only the specific group instead of all groups
                    db::save_mls_group(handle.clone(), &groups[idx]).await.map_err(|e| e.to_string())?;
                    mls::emit_group_metadata_event(&groups[idx]);
                } else {
                    println!("[MLS] Group already exists in metadata: group_id={}", nostr_group_id);
                }
            } else {
                // Build metadata for the accepted group
                // Use invite_sent_at (from welcome event) for created_at so sync fetches from the right time
                let now_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| e.to_string())?
                    .as_secs();

                let metadata = mls::MlsGroupMetadata {
                    group_id: nostr_group_id.clone(),         // Wire ID for relay filtering (h tag)
                    engine_group_id: engine_group_id.clone(), // Internal engine ID for local operations
                    creator_pubkey: welcomer_hex,             // The welcomer becomes the creator from our perspective
                    name: group_name,
                    avatar_ref: None,
                    created_at: invite_sent_at,               // Use invite-sent time, NOT acceptance time!
                    updated_at: now_secs,
                    evicted: false,                           // Accepting a welcome means we're joining, not evicted
                };

                db::save_mls_group(handle.clone(), &metadata).await.map_err(|e| e.to_string())?;
                mls::emit_group_metadata_event(&metadata);

                // Create the Chat in STATE with metadata and save to disk
                {
                    let mut state = STATE.lock().await;
                    let chat_id = state.create_or_get_mls_group_chat(&nostr_group_id, vec![]);

                    // Set metadata from MlsGroupMetadata
                    if let Some(chat) = state.get_chat_mut(&chat_id) {
                        chat.metadata.set_name(metadata.name.clone());
                        // Member count will be updated during sync when we process messages
                    }

                    // Save chat to disk
                    if let Some(chat) = state.get_chat(&chat_id) {
                        if let Err(e) = db::save_chat(handle.clone(), chat).await {
                            eprintln!("[MLS] Failed to save chat after welcome acceptance: {}", e);
                        }
                    }
                }

                println!("[MLS] Persisted group metadata after accept: group_id={}", nostr_group_id);
            }

            // Remove this welcome from the notified set since it's been accepted
            {
                let mut notified = NOTIFIED_WELCOMES.lock().await;
                notified.remove(&wrapper_event_id_hex);
            }

            // Emit event so the UI can refresh welcome lists and group lists
            if let Some(app) = TAURI_APP.get() {
                let _ = app.emit("mls_welcome_accepted", serde_json::json!({
                    "welcome_event_id": welcome_event_id_hex,
                    "group_id": nostr_group_id
                }));
            }

            // Sync the participants array with actual group members from the engine
            if let Err(e) = sync_mls_group_participants(nostr_group_id.clone()).await {
                eprintln!("[MLS] Failed to sync participants after welcome accept: {}", e);
            }

            // Immediately prefetch recent MLS messages for this group so the chat list shows previews
            // and ordering without requiring the user to open the chat. This loads a recent slice
            // (48h window by default in sync_group_since_cursor) rather than full history.
            match mls.sync_group_since_cursor(&nostr_group_id).await {
                Ok((processed, new_msgs)) => {
                    println!("[MLS] Post-accept initial sync: processed={}, new={}", processed, new_msgs);
                    // Optional: let UI know initial sync finished for this group
                    if let Some(app) = TAURI_APP.get() {
                        let _ = app.emit("mls_group_initial_sync", serde_json::json!({
                            "group_id": nostr_group_id,
                            "processed": processed,
                            "new": new_msgs
                        }));
                    }
                }
                Err(e) => {
                    eprintln!("[MLS] Post-accept initial sync failed for group {}: {}", nostr_group_id, e);
                }
            }

            Ok::<bool, String>(true)
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))??;

    if accepted {
        tokio::spawn(async {
            if let Err(err) = regenerate_device_keypackage(false).await {
                eprintln!("[MLS] Failed to regenerate device KeyPackage after accepting welcome: {}", err);
            }
        });
    }

    Ok(accepted)
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
