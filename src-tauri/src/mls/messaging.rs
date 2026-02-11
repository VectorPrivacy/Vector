//! MLS messaging functions.
//!
//! This module handles:
//! - Sending MLS messages to groups
//! - Emitting group metadata events to frontend

use mdk_core::prelude::GroupId;
use tauri::Emitter;
use crate::{TAURI_APP, NOSTR_CLIENT};
use crate::util::hex_string_to_bytes;
use super::{MlsService, MlsGroupMetadata};

/// Send an MLS message (rumor) to a group
///
/// This function takes a group_id, an UnsignedEvent (rumor), and an optional pending_id,
/// and sends it through the MLS protocol.
/// It's used by the protocol-agnostic message sending system to route group messages through MLS.
///
/// If a pending_id is provided, it will update that pending message with the real message ID
/// and handle success/failure state updates.
pub async fn send_mls_message(group_id: &str, rumor: nostr_sdk::UnsignedEvent, pending_id: Option<String>) -> Result<(), String> {
    let group_id = group_id.to_string();
    let pending_id = pending_id.clone();
    
    // Run non-Send MLS engine work on blocking thread
    tokio::task::spawn_blocking(move || {
        let handle = TAURI_APP.get()
            .ok_or_else(|| "App handle not initialized".to_string())?
            .clone();
        
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            // Get the Nostr client
            let client = NOSTR_CLIENT.get()
                .ok_or_else(|| "Nostr client not initialized".to_string())?;
            
            // Create MLS service instance
            let service = MlsService::new_persistent(&handle)
                .map_err(|e| format!("Failed to create MLS service: {}", e))?;
            
            // Look up the group to get the engine_group_id (do this before getting engine)
            let groups = service.read_groups().await
                .map_err(|e| format!("Failed to read groups: {}", e))?;
            
            let group_meta = groups.iter()
                .find(|g| g.group_id == group_id)
                .ok_or_else(|| format!("Group not found: {}", group_id))?;
            
            // Parse the engine group ID
            let engine_group_id = if group_meta.engine_group_id.is_empty() {
                return Err("Group has no engine_group_id".to_string());
            } else {
                GroupId::from_slice(&hex_string_to_bytes(&group_meta.engine_group_id))
            };
            
            // Now get the MLS engine and create message (no await while engine is in scope)
            let mls_wrapper_result = {
                let engine = service.engine()
                    .map_err(|e| format!("Failed to get MLS engine: {}", e))?;
                
                engine.create_message(&engine_group_id, rumor.clone())
            }; // engine dropped here
            
            // Check for eviction errors after engine is dropped
            let mls_wrapper = match mls_wrapper_result {
                Ok(wrapper) => wrapper,
                Err(e) => {
                    let error_msg = e.to_string();
                    
                    // Check if this is an eviction error
                    if error_msg.contains("own leaf not found") ||
                       error_msg.contains("after being evicted") ||
                       error_msg.contains("evicted from it") ||
                       error_msg.contains("group not found") {
                        eprintln!("[MLS] Eviction detected while sending to group: {}", group_id);
                        
                        // Perform cleanup (we're in an async context now)
                        if let Err(cleanup_err) = service.cleanup_evicted_group(&group_id).await {
                            eprintln!("[MLS] Failed to cleanup evicted group: {}", cleanup_err);
                        }
                    }
                    
                    // Mark pending message as failed if we have a pending_id
                    if let Some(ref pid) = pending_id {
                        let msg_for_emit = {
                            let mut state = crate::STATE.lock().await;
                            state.update_message_in_chat(&group_id, pid, |msg| {
                                msg.set_failed(true);
                                msg.set_pending(false);
                            })
                        };

                        if let (Some(handle), Some(msg)) = (TAURI_APP.get(), msg_for_emit) {
                            handle.emit("message_update", serde_json::json!({
                                "old_id": pid,
                                "message": msg,
                                "chat_id": &group_id
                            })).ok();
                        }
                    }
                    
                    return Err(format!("Failed to create MLS message: {}", e));
                }
            };
            
            // Get the inner rumor ID for the final update
            let inner_event_id = rumor.id.map(|id| id.to_hex());
            
            // Check if this is a typing indicator and add expiration to wrapper if so
            let is_typing_indicator = rumor.kind == nostr_sdk::Kind::ApplicationSpecificData
                && rumor.content == "typing";
            
            // Send the message and handle success/failure
            let send_result = if is_typing_indicator {
                // For typing indicators, add a 30-second expiration to the wrapper event
                use nostr_sdk::{EventBuilder, Tag, Timestamp};
                
                let expiry_time = Timestamp::from_secs(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        + 30,
                );
                
                // Create a new wrapper event with expiration tag
                let mut wrapper_builder = EventBuilder::new(mls_wrapper.kind, &mls_wrapper.content);
                
                // Copy all existing tags
                for tag in mls_wrapper.tags.iter() {
                    wrapper_builder = wrapper_builder.tag(tag.clone());
                }
                
                // Add expiration tag
                wrapper_builder = wrapper_builder.tag(Tag::expiration(expiry_time));
                
                // Build and sign the wrapper
                let signer = crate::MY_KEYS.get().expect("Keys not initialized").clone();
                let wrapper_with_expiry = wrapper_builder.sign(&signer).await
                    .map_err(|e| format!("Failed to sign wrapper with expiration: {}", e))?;
                
                // Send the wrapper with expiration (first-ACK for faster pending→sent)
                let urls = crate::inbox_relays::trusted_relay_urls();
                crate::inbox_relays::send_event_first_ok(client, urls, &wrapper_with_expiry).await
            } else {
                // Send normal wrapper without expiration (first-ACK for faster pending→sent)
                let urls = crate::inbox_relays::trusted_relay_urls();
                crate::inbox_relays::send_event_first_ok(client, urls, &mls_wrapper).await
            };
            
            // Update pending message based on send result
            if let (Some(ref pid), Some(ref real_id)) = (&pending_id, &inner_event_id) {
                match send_result {
                    Ok(_) => {
                        // Mark message as successfully sent and update ID
                        let result = {
                            let mut state = crate::STATE.lock().await;
                            state.finalize_pending_message(&group_id, pid, real_id)
                        };

                        if let Some((old_id, msg)) = result {
                            if let Some(handle) = TAURI_APP.get() {
                                handle.emit("message_update", serde_json::json!({
                                    "old_id": &old_id,
                                    "message": &msg,
                                    "chat_id": &group_id
                                })).ok();
                                let _ = crate::db::save_message(handle.clone(), &group_id, &msg).await;
                            }
                        }
                    }
                    Err(e) => {
                        // Mark message as failed (keep pending ID)
                        let msg_for_emit = {
                            let mut state = crate::STATE.lock().await;
                            state.update_message_in_chat(&group_id, pid, |msg| {
                                msg.set_failed(true);
                                msg.set_pending(false);
                            })
                        };

                        if let (Some(handle), Some(msg)) = (TAURI_APP.get(), msg_for_emit) {
                            handle.emit("message_update", serde_json::json!({
                                "old_id": pid,
                                "message": msg,
                                "chat_id": &group_id
                            })).ok();
                        }
                        return Err(format!("Failed to send MLS wrapper: {}", e));
                    }
                }
            } else {
                // No pending_id provided, just return the send result
                send_result.map_err(|e| format!("Failed to send MLS wrapper: {}", e))?;
            }
            
            Ok(())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Emit a frontend event whenever MLS group metadata changes so the UI can hydrate quickly.
pub fn emit_group_metadata_event(meta: &MlsGroupMetadata) {
    if let Some(handle) = TAURI_APP.get() {
        if let Err(e) = handle.emit(
            "mls_group_metadata",
            serde_json::json!({ "metadata": metadata_to_frontend(meta) }),
        ) {
            eprintln!("[MLS] Failed to emit mls_group_metadata event: {}", e);
        }
    }
}

fn seconds_to_millis(value: u64) -> u64 {
    value.saturating_mul(1000)
}

pub fn metadata_to_frontend(meta: &MlsGroupMetadata) -> serde_json::Value {
    serde_json::json!({
        "group_id": meta.group_id,
        "engine_group_id": meta.engine_group_id,
        "creator_pubkey": meta.creator_pubkey,
        "name": meta.name,
        "avatar_ref": meta.avatar_ref,
        "created_at": seconds_to_millis(meta.created_at),
        "updated_at": seconds_to_millis(meta.updated_at),
        "evicted": meta.evicted,
    })
}