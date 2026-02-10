//! Invite codes and badge Tauri commands.
//!
//! This module handles:
//! - Invite code generation and acceptance
//! - Tracking invited users
//! - Special event badges (Guy Fawkes Day 2025)

use std::borrow::Cow;

use nostr_sdk::prelude::*;
use rand::{thread_rng, Rng};
use rand::distributions::Alphanumeric;

use crate::{TAURI_APP, NOSTR_CLIENT, TRUSTED_RELAYS, PENDING_INVITE, PendingInviteAcceptance};
use crate::db;

// ============================================================================
// Constants
// ============================================================================

// Guy Fawkes Day 2025 - V for Vector Badge (Event Ended)
const FAWKES_DAY_START: u64 = 1762300800; // 2025-11-05 00:00:00 UTC
const FAWKES_DAY_END: u64 = 1762387200;   // 2025-11-06 00:00:00 UTC

// ============================================================================
// Helpers
// ============================================================================

/// Generate a random alphanumeric invite code
fn generate_invite_code() -> String {
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect::<String>()
        .to_uppercase()
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Generate or retrieve existing invite code for the current user
#[tauri::command]
pub async fn get_or_create_invite_code() -> Result<String, String> {
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?;

    // Check if we already have a stored invite code
    if let Ok(Some(existing_code)) = db::get_sql_setting(handle.clone(), "invite_code".to_string()) {
        return Ok(existing_code);
    }

    // No local code found, check the network
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;

    // Get our public key
    let my_public_key = *crate::MY_PUBLIC_KEY.get().ok_or("Public key not initialized")?;

    // Check if we've already published an invite on the network
    let filter = Filter::new()
        .author(my_public_key)
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::lowercase(Alphabet::D), "vector")
        .limit(100);

    let mut events = client
        .stream_events(filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;

    // Look for existing invite events
    while let Some(event) = events.next().await {
        if event.content == "vector_invite" {
            // Extract the r tag (invite code)
            if let Some(r_tag) = event.tags.find(TagKind::Custom(Cow::Borrowed("r"))) {
                if let Some(code) = r_tag.content() {
                    // Store it locally
                    db::set_sql_setting(handle.clone(), "invite_code".to_string(), code.to_string())
                        .map_err(|e| e.to_string())?;
                    return Ok(code.to_string());
                }
            }
        }
    }

    // No existing invite found anywhere, generate a new one
    let new_code = generate_invite_code();

    // Create and publish the invite event
    let event_builder = EventBuilder::new(Kind::ApplicationSpecificData, "vector_invite")
        .tag(Tag::custom(TagKind::d(), vec!["vector"]))
        .tag(Tag::custom(TagKind::Custom("r".into()), vec![new_code.as_str()]));

    // Build the event
    let event = client.sign_event_builder(event_builder).await.map_err(|e| e.to_string())?;

    // Send only to trusted relays
    client.send_event_to(TRUSTED_RELAYS.iter().copied(), &event).await.map_err(|e| e.to_string())?;

    // Store locally
    db::set_sql_setting(handle.clone(), "invite_code".to_string(), new_code.clone())
        .map_err(|e| e.to_string())?;

    Ok(new_code)
}

/// Accept an invite code from another user (deferred until after encryption setup)
#[tauri::command]
pub async fn accept_invite_code(invite_code: String) -> Result<String, String> {
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;

    // Validate invite code format (8 alphanumeric characters)
    if invite_code.len() != 8 || !invite_code.chars().all(|c| c.is_alphanumeric()) {
        return Err("Invalid invite code format".to_string());
    }

    // Search for the invite event
    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::lowercase(Alphabet::D), "vector")
        .custom_tag(SingleLetterTag::lowercase(Alphabet::R), &invite_code)
        .limit(1);


    // Find the invite event
    let mut events = client
        .stream_events_from(TRUSTED_RELAYS.to_vec(), filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;

    let invite_event = {
        let mut found: Option<nostr_sdk::Event> = None;
        while let Some(event) = events.next().await {
            if event.content == "vector_invite" {
                found = Some(event);
                break;
            }
        }
        found.ok_or("Invite code not found")?
    };

    // Get the inviter's public key
    let inviter_pubkey = invite_event.pubkey;
    let inviter_npub = inviter_pubkey.to_bech32().map_err(|e| e.to_string())?;

    // Get our public key
    let my_public_key = *crate::MY_PUBLIC_KEY.get().ok_or("Public key not initialized")?;

    // Check if we're trying to accept our own invite
    if inviter_pubkey == my_public_key {
        return Err("Cannot accept your own invite code".to_string());
    }

    // Store the pending invite acceptance (will be broadcast after encryption setup)
    let pending_invite = PendingInviteAcceptance {
        invite_code: invite_code.clone(),
        inviter_pubkey: inviter_pubkey.clone(),
    };

    // Try to set the pending invite, ignore if already set
    let _ = PENDING_INVITE.set(pending_invite);

    // Return the inviter's npub so the frontend can initiate a chat
    Ok(inviter_npub)
}

/// Get the count of unique users who accepted invites from a given npub
#[tauri::command]
pub async fn get_invited_users(npub: String) -> Result<u32, String> {
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;

    // Convert npub to PublicKey
    let inviter_pubkey = PublicKey::from_bech32(&npub).map_err(|e| e.to_string())?;

    // First, get the inviter's invite code from the trusted relays
    let filter = Filter::new()
        .author(inviter_pubkey)
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::lowercase(Alphabet::D), "vector")
        .limit(100);

    let mut events = client
        .stream_events_from(TRUSTED_RELAYS.to_vec(), filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;

    // Find the invite event and extract the invite code
    let mut invite_code_opt = None;
    while let Some(event) = events.next().await {
        if event.content == "vector_invite" {
            if let Some(tag) = event.tags.find(TagKind::Custom(Cow::Borrowed("r"))) {
                if let Some(content) = tag.content() {
                    invite_code_opt = Some(content.to_string());
                    break;
                }
            }
        }
    }
    let invite_code = invite_code_opt.ok_or("No invite code found for this user")?;

    // Now fetch all acceptance events for this invite code from the trusted relays
    let acceptance_filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::lowercase(Alphabet::D), invite_code)
        .limit(1000); // Allow fetching many acceptances

    let mut acceptance_events = client
        .stream_events_from(TRUSTED_RELAYS.to_vec(), acceptance_filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;

    // Filter for acceptance events that reference our inviter and collect unique acceptors
    let mut unique_acceptors = std::collections::HashSet::new();

    while let Some(event) = acceptance_events.next().await {
        if event.content == "vector_invite_accepted" {
            // Check if this acceptance references our inviter
            let references_inviter = event.tags
                .iter()
                .any(|tag| {
                    if let Some(TagStandard::PublicKey { public_key, .. }) = tag.as_standardized() {
                        *public_key == inviter_pubkey
                    } else {
                        false
                    }
                });

            if references_inviter {
                unique_acceptors.insert(event.pubkey);
            }
        }
    }

    Ok(unique_acceptors.len() as u32)
}

/// Check if a user has the Guy Fawkes Day badge
/// Verifies they have a valid badge claim event from the November 5, 2025 event
#[tauri::command]
pub async fn check_fawkes_badge(npub: String) -> Result<bool, String> {
    let client = NOSTR_CLIENT.get().ok_or("Nostr client not initialized")?;

    // Convert npub to PublicKey
    let user_pubkey = PublicKey::from_bech32(&npub).map_err(|e| e.to_string())?;

    // Fetch the user's badge claim event
    let filter = Filter::new()
        .author(user_pubkey)
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::lowercase(Alphabet::D), "fawkes_2025")
        .limit(10);

    let mut events = client
        .stream_events_from(TRUSTED_RELAYS.to_vec(), filter, std::time::Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;

    // Check if they have a valid badge claim from the event period
    while let Some(event) = events.next().await {
        if event.content == "fawkes_badge_claimed" {
            let timestamp = event.created_at.as_secs();
            // Verify the timestamp is within the valid event window
            if timestamp >= FAWKES_DAY_START && timestamp < FAWKES_DAY_END {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

// Handler list for this module (for reference):
// - get_or_create_invite_code
// - accept_invite_code
// - get_invited_users
// - check_fawkes_badge
