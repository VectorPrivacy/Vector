//! MLS messaging — send MLS messages to groups.

use mdk_core::prelude::GroupId;

use crate::mls::MlsService;

/// Send an MLS message (rumor) to a group.
///
/// Takes a group_id, an UnsignedEvent (rumor), and an optional pending_id.
/// Routes through the MLS protocol: engine.create_message → relay publish.
///
/// If pending_id is provided, updates the pending message with the real ID
/// and handles success/failure state updates via emit_event.
pub async fn send_mls_message(
    group_id: &str,
    rumor: nostr_sdk::UnsignedEvent,
    pending_id: Option<String>,
) -> Result<(), String> {
    let group_id = group_id.to_string();
    let pending_id = pending_id.clone();

    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async move {
            let client = crate::state::NOSTR_CLIENT.get()
                .ok_or_else(|| "Nostr client not initialized".to_string())?;

            let service = MlsService::new_persistent_static()
                .map_err(|e| format!("Failed to create MLS service: {}", e))?;

            // Look up group metadata for engine_group_id
            let groups = service.read_groups()
                .map_err(|e| format!("Failed to read groups: {}", e))?;

            let group_meta = groups.iter()
                .find(|g| g.group.group_id == group_id)
                .ok_or_else(|| format!("Group not found: {}", group_id))?;

            let engine_group_id = if group_meta.group.engine_group_id.is_empty() {
                return Err("Group has no engine_group_id".to_string());
            } else {
                GroupId::from_slice(&crate::hex::hex_string_to_bytes(&group_meta.group.engine_group_id))
            };

            // Create MLS message (no await while engine in scope)
            let mls_wrapper_result = {
                let engine = service.engine()
                    .map_err(|e| format!("Failed to get MLS engine: {}", e))?;
                engine.create_message(&engine_group_id, rumor.clone())
            }; // engine dropped

            let mls_wrapper = match mls_wrapper_result {
                Ok(wrapper) => wrapper,
                Err(e) => {
                    let error_msg = e.to_string();

                    // Eviction detection
                    if error_msg.contains("own leaf not found") ||
                       error_msg.contains("after being evicted") ||
                       error_msg.contains("evicted from it") ||
                       error_msg.contains("group not found") {
                        eprintln!("[MLS] Eviction detected while sending to group: {}", group_id);
                        if let Err(cleanup_err) = service.cleanup_evicted_group(&group_id).await {
                            eprintln!("[MLS] Failed to cleanup evicted group: {}", cleanup_err);
                        }
                    }

                    // Mark pending message as failed
                    if let Some(ref pid) = pending_id {
                        let msg_for_emit = {
                            let mut state = crate::state::STATE.lock().await;
                            state.update_message_in_chat(&group_id, pid, |msg| {
                                msg.set_failed(true);
                                msg.set_pending(false);
                            })
                        };
                        if let Some(msg) = msg_for_emit {
                            crate::traits::emit_event("message_update", &serde_json::json!({
                                "old_id": pid, "message": msg, "chat_id": &group_id
                            }));
                        }
                    }

                    return Err(format!("Failed to create MLS message: {}", e));
                }
            };

            let inner_event_id = rumor.id.map(|id| id.to_hex());

            // Typing indicators get a 30-second expiration on the wrapper
            let is_typing_indicator = rumor.kind == nostr_sdk::Kind::ApplicationSpecificData
                && rumor.content == "typing";

            let send_result = if is_typing_indicator {
                use nostr_sdk::{EventBuilder, Tag, Timestamp};

                let expiry_time = Timestamp::from_secs(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        + 30,
                );

                let mut wrapper_builder = EventBuilder::new(mls_wrapper.kind, &mls_wrapper.content);
                for tag in mls_wrapper.tags.iter() {
                    wrapper_builder = wrapper_builder.tag(tag.clone());
                }
                wrapper_builder = wrapper_builder.tag(Tag::expiration(expiry_time));

                let signer = crate::state::MY_SECRET_KEY.to_keys().expect("Keys not initialized");
                let wrapper_with_expiry = wrapper_builder.sign(&signer).await
                    .map_err(|e| format!("Failed to sign wrapper with expiration: {}", e))?;

                let urls = crate::inbox_relays::trusted_relay_urls();
                crate::inbox_relays::send_event_first_ok(client, urls, &wrapper_with_expiry).await
            } else {
                let urls = crate::inbox_relays::trusted_relay_urls();
                crate::inbox_relays::send_event_first_ok(client, urls, &mls_wrapper).await
            };

            // Track wrapper to dedup relay echo
            if send_result.is_ok() {
                let sent_wrapper_id = if is_typing_indicator {
                    None // ephemeral, don't track
                } else {
                    Some(mls_wrapper.id.to_hex())
                };
                if let Some(wrapper_id) = sent_wrapper_id {
                    let _ = crate::mls::track_mls_event_processed(
                        &wrapper_id,
                        &group_id,
                        mls_wrapper.created_at.as_secs(),
                    );
                }
            }

            // Update pending message based on send result
            if let (Some(ref pid), Some(ref real_id)) = (&pending_id, &inner_event_id) {
                match send_result {
                    Ok(_) => {
                        let result = {
                            let mut state = crate::state::STATE.lock().await;
                            state.finalize_pending_message(&group_id, pid, real_id)
                        };
                        if let Some((old_id, msg)) = result {
                            // Always persist — original gated on TAURI_APP which is a bug
                            crate::traits::emit_event("message_update", &serde_json::json!({
                                "old_id": &old_id, "message": &msg, "chat_id": &group_id
                            }));
                            let _ = crate::db::events::save_message(&group_id, &msg).await;
                        }
                    }
                    Err(e) => {
                        let msg_for_emit = {
                            let mut state = crate::state::STATE.lock().await;
                            state.update_message_in_chat(&group_id, pid, |msg| {
                                msg.set_failed(true);
                                msg.set_pending(false);
                            })
                        };
                        if let Some(msg) = msg_for_emit {
                            crate::traits::emit_event("message_update", &serde_json::json!({
                                "old_id": pid, "message": msg, "chat_id": &group_id
                            }));
                        }
                        return Err(format!("Failed to send MLS wrapper: {}", e));
                    }
                }
            } else {
                send_result.map_err(|e| format!("Failed to send MLS wrapper: {}", e))?;
            }

            Ok(())
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Emit a group metadata event to the frontend.
pub fn emit_group_metadata_event(meta: &crate::mls::MlsGroupFull) {
    crate::traits::emit_event("mls_group_metadata", &serde_json::json!({
        "metadata": crate::mls::metadata_to_frontend(meta)
    }));
}

#[cfg(test)]
mod tests {
    #[test]
    fn emit_group_metadata_event_does_not_panic() {
        // With no emitter set, should be a no-op (not crash)
        let meta = crate::mls::MlsGroupFull {
            group: crate::mls::MlsGroup {
                group_id: "test".into(),
                engine_group_id: "eng".into(),
                creator_pubkey: "pk".into(),
                created_at: 100,
                updated_at: 200,
                evicted: false,
            },
            profile: crate::mls::MlsGroupProfile {
                name: "Test".into(),
                description: None,
                avatar_ref: None,
                avatar_cached: None,
            },
        };
        super::emit_group_metadata_event(&meta);
        // No panic = pass
    }
}
