//! MLS KeyPackage generation + publishing.
//!
//! A KeyPackage is an MLS cryptographic bundle that lets others invite you
//! to encrypted groups. Each device publishes its own KeyPackage to Nostr
//! relays as Kind::MlsKeyPackage events. Without a published KeyPackage,
//! no one can invite you to MLS groups.

use nostr_sdk::prelude::*;
use rand::{distributions::Alphanumeric, thread_rng, Rng};

use super::{MlsService, MlsError};
use crate::state::{NOSTR_CLIENT, MY_PUBLIC_KEY, MY_SECRET_KEY, active_trusted_relays};

/// Result of publishing a keypackage.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublishedKeyPackage {
    pub device_id: String,
    pub owner_pubkey: String,
    pub keypackage_ref: String,
    /// True if reused an existing keypackage from relays.
    pub cached: bool,
}

/// Publish this device's MLS KeyPackage to trusted relays.
///
/// If `use_cache` is true and a valid cached keypackage exists on relay, reuse it.
/// Otherwise always generate and publish a fresh one.
pub async fn publish_keypackage(use_cache: bool) -> Result<PublishedKeyPackage, MlsError> {
    let client = NOSTR_CLIENT.get()
        .ok_or_else(|| MlsError::NostrMlsError("Not connected".into()))?;

    // Ensure persistent device_id
    let device_id = match crate::db::mls::load_mls_device_id() {
        Ok(Some(id)) => id,
        _ => {
            let id: String = thread_rng()
                .sample_iter(&Alphanumeric)
                .take(12)
                .map(char::from)
                .collect::<String>()
                .to_lowercase();
            let _ = crate::db::mls::save_mls_device_id(&id);
            id
        }
    };

    let my_pubkey = *MY_PUBLIC_KEY.get()
        .ok_or_else(|| MlsError::NostrMlsError("Not logged in".into()))?;
    let owner_pubkey_b32 = my_pubkey.to_bech32()
        .map_err(|e| MlsError::NostrMlsError(e.to_string()))?;

    // Resolve trusted relays once — consistent across cache check, engine call,
    // and retry loop to avoid publish/verify targeting different relay sets.
    let trusted_relays: Vec<&'static str> = active_trusted_relays().await;

    // Cache check: verify existing keypackage still exists on relay with encoding tag
    if use_cache {
        let cached_ref: Option<String> = crate::db::mls::load_mls_keypackages()
            .unwrap_or_default()
            .iter()
            .find(|entry| {
                entry.get("owner_pubkey").and_then(|v| v.as_str()) == Some(owner_pubkey_b32.as_str())
                    && entry.get("device_id").and_then(|v| v.as_str()) == Some(device_id.as_str())
            })
            .and_then(|existing| existing.get("keypackage_ref").and_then(|v| v.as_str()).map(|s| s.to_string()));

        if let Some(ref_id) = cached_ref {
            if let Ok(event_id) = EventId::from_hex(&ref_id) {
                let filter = Filter::new()
                    .id(event_id)
                    .kind(Kind::MlsKeyPackage)
                    .limit(1);

                if let Ok(mut events) = client.stream_events_from(
                    trusted_relays.clone(),
                    filter,
                    std::time::Duration::from_secs(5)
                ).await {
                    use futures_util::StreamExt;
                    if let Some(event) = events.next().await {
                        let has_encoding = event.tags.iter().any(|tag| {
                            let slice = tag.as_slice();
                            slice.len() >= 2 && slice[0] == "encoding" && slice[1] == "base64"
                        });
                        if has_encoding {
                            return Ok(PublishedKeyPackage {
                                device_id,
                                owner_pubkey: owner_pubkey_b32,
                                keypackage_ref: ref_id,
                                cached: true,
                            });
                        }
                    }
                }
            }
        }
    }

    // Resolve relays for engine (before non-Send scope)
    let relay_urls: Vec<RelayUrl> = trusted_relays.iter()
        .filter_map(|r| RelayUrl::parse(r).ok())
        .collect();

    // Create KeyPackage using engine (non-Send, no-await scope)
    let (kp_encoded, kp_tags, _hash_ref) = {
        let mls_service = MlsService::new_persistent_static()?;
        let engine = mls_service.engine()?;
        engine
            .create_key_package_for_event(&my_pubkey, relay_urls)
            .map_err(|e| MlsError::NostrMlsError(format!("create_key_package_for_event: {}", e)))?
    };

    // Filter out the protected tag "-" (MDK adds it, but breaks NIP-70 relays)
    let filtered_tags: Vec<_> = kp_tags
        .into_iter()
        .filter(|t| t.as_slice().first().map(|s| s.as_str()) != Some("-"))
        .collect();

    let signing_keys = MY_SECRET_KEY.to_keys()
        .ok_or_else(|| MlsError::NostrMlsError("Keys not initialized".into()))?;

    let kp_event = EventBuilder::new(Kind::MlsKeyPackage, kp_encoded)
        .tags(filtered_tags)
        .sign_with_keys(&signing_keys)
        .map_err(|e| MlsError::NostrMlsError(e.to_string()))?;

    // Publish with retry
    let mut last_error = String::new();
    let mut published = false;
    for attempt in 1..=3 {
        match client.send_event_to(trusted_relays.iter().copied(), &kp_event).await {
            Ok(result) => {
                if !result.success.is_empty() {
                    published = true;
                    break;
                } else {
                    last_error = format!("All relays failed: {:?}", result.failed);
                    if attempt < 3 {
                        tokio::time::sleep(std::time::Duration::from_secs((attempt * 5) as u64)).await;
                    }
                }
            }
            Err(e) => {
                last_error = e.to_string();
                if attempt < 3 {
                    tokio::time::sleep(std::time::Duration::from_secs((attempt * 5) as u64)).await;
                }
            }
        }
    }

    if !published {
        return Err(MlsError::NostrMlsError(format!("Failed to publish keypackage: {}", last_error)));
    }

    // Update local keypackage index
    let new_kp_ref = kp_event.id.to_hex();
    {
        let mut index = crate::db::mls::load_mls_keypackages().unwrap_or_default();
        let now = Timestamp::now().as_secs();

        index.retain(|entry| {
            let same_owner = entry.get("owner_pubkey").and_then(|v| v.as_str()) == Some(&owner_pubkey_b32);
            let same_device = entry.get("device_id").and_then(|v| v.as_str()) == Some(&device_id);
            let same_ref = entry.get("keypackage_ref").and_then(|v| v.as_str()) == Some(&new_kp_ref);
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

        let _ = crate::db::mls::save_mls_keypackages(&index);
    }

    Ok(PublishedKeyPackage {
        device_id,
        owner_pubkey: owner_pubkey_b32,
        keypackage_ref: new_kp_ref,
        cached: false,
    })
}
