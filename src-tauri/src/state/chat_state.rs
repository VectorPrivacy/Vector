//! ChatState struct and methods for managing application state.
//!
//! This module contains the core state management for profiles, chats, and sync status.

use nostr_sdk::prelude::*;
use tauri::Emitter;

use crate::{Profile, Chat, ChatType, Message};
use crate::db::SlimProfile;
use super::globals::{TAURI_APP, NOSTR_CLIENT};
use super::SyncMode;

/// Core application state containing profiles, chats, and sync status.
#[derive(serde::Serialize, Clone, Debug)]
pub struct ChatState {
    pub(crate) profiles: Vec<Profile>,
    pub(crate) chats: Vec<Chat>,
    pub(crate) is_syncing: bool,
    pub(crate) sync_window_start: u64,
    pub(crate) sync_window_end: u64,
    pub(crate) sync_mode: SyncMode,
    pub(crate) sync_empty_iterations: u8,
    pub(crate) sync_total_iterations: u8,
}

impl ChatState {
    /// Create a new empty ChatState
    pub fn new() -> Self {
        Self {
            profiles: Vec::new(),
            chats: Vec::new(),
            is_syncing: false,
            sync_window_start: 0,
            sync_window_end: 0,
            sync_mode: SyncMode::Finished,
            sync_empty_iterations: 0,
            sync_total_iterations: 0,
        }
    }

    /// Load a Vector Profile into the state from our SlimProfile database format
    pub async fn from_db_profile(&mut self, slim: SlimProfile) {
        // Check if profile already exists
        if let Some(position) = self.profiles.iter().position(|profile| profile.id == slim.id) {
            // Replace existing profile
            let mut full_profile = slim.to_profile();

            // Check if this is our profile: we need to mark it as such
            let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
            let signer = client.signer().await.unwrap();
            let my_public_key = signer.get_public_key().await.unwrap();
            let profile_pubkey = PublicKey::from_bech32(&full_profile.id).unwrap();
            full_profile.mine = my_public_key == profile_pubkey;

            self.profiles[position] = full_profile;
        } else {
            // Add new profile
            self.profiles.push(slim.to_profile());
        }
    }

    /// Merge multiple Vector Profiles from SlimProfile format into the state at once
    pub async fn merge_db_profiles(&mut self, slim_profiles: Vec<SlimProfile>) {
        for slim in slim_profiles {
            self.from_db_profile(slim).await;
        }
    }

    /// Get a profile by ID
    pub fn get_profile(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    /// Get a mutable profile by ID
    pub fn get_profile_mut(&mut self, id: &str) -> Option<&mut Profile> {
        self.profiles.iter_mut().find(|p| p.id == id)
    }

    /// Get a chat by ID
    pub fn get_chat(&self, id: &str) -> Option<&Chat> {
        self.chats.iter().find(|c| c.id == id)
    }

    /// Get a mutable chat by ID
    pub fn get_chat_mut(&mut self, id: &str) -> Option<&mut Chat> {
        self.chats.iter_mut().find(|c| c.id == id)
    }

    /// Create a new chat for a DM with a specific user
    pub fn create_dm_chat(&mut self, their_npub: &str) -> String {
        // Check if chat already exists
        if self.get_chat(their_npub).is_none() {
            let chat = Chat::new_dm(their_npub.to_string());
            self.chats.push(chat);
        }

        their_npub.to_string()
    }

    /// Create or get an MLS group chat
    pub fn create_or_get_mls_group_chat(&mut self, group_id: &str, participants: Vec<String>) -> String {
        // Check if chat already exists
        if self.get_chat(group_id).is_none() {
            let chat = Chat::new_mls_group(group_id.to_string(), participants);
            self.chats.push(chat);
        }

        group_id.to_string()
    }

    /// Add a message to a chat via its ID
    pub fn add_message_to_chat(&mut self, chat_id: &str, message: Message) -> bool {
        let is_msg_added = match self.get_chat_mut(chat_id) {
            Some(chat) => {
                // Add the message to the existing chat
                chat.internal_add_message(message)
            },
            None => {
                // Chat doesn't exist, create it and add the message
                // Determine chat type based on chat_id format
                let chat = if chat_id.starts_with("npub1") {
                    // DM chat: use the chat_id as the participant
                    Chat::new_dm(chat_id.to_string())
                } else {
                    // MLS group: participants will be set later
                    Chat::new(chat_id.to_string(), ChatType::MlsGroup, vec![])
                };
                let mut chat = chat;
                let was_added = chat.internal_add_message(message);
                self.chats.push(chat);
                was_added
            }
        };

        // Sort our chat positions based on last message time
        self.chats.sort_by(|a, b| {
            // Get last message time for both chats
            let a_time = a.last_message_time();
            let b_time = b.last_message_time();

            // Compare timestamps in reverse order (newest first)
            b_time.cmp(&a_time)
        });

        is_msg_added
    }

    /// Add a message to a chat via its participant npub
    pub fn add_message_to_participant(&mut self, their_npub: &str, message: Message) -> bool {
        // Ensure profiles exist for the participant
        if self.get_profile(their_npub).is_none() {
            // Create a basic profile for the participant
            let mut profile = Profile::new();
            profile.id = their_npub.to_string();
            profile.mine = false; // It's not our profile

            // Update the frontend about the new profile
            if let Some(handle) = TAURI_APP.get() {
                handle.emit("profile_update", &profile).unwrap();
            }

            // Add to our profiles list
            self.profiles.push(profile);
        }

        // Create or get the chat ID
        let chat_id = self.create_dm_chat(their_npub);

        // Add the message to the chat
        self.add_message_to_chat(&chat_id, message)
    }

    /// Count unread messages across all profiles
    pub fn count_unread_messages(&self) -> u32 {
        let mut total_unread = 0;

        // Count unread messages in all chats
        for chat in &self.chats {
            // Skip muted chats entirely
            if chat.muted {
                continue;
            }

            // Skip chats where the corresponding profile is muted (for DMs)
            let mut skip_for_profile_mute = false;
            match chat.chat_type {
                ChatType::DirectMessage => {
                    // For DMs, chat.id is the other participant's npub
                    if let Some(profile) = self.get_profile(&chat.id) {
                        if profile.muted {
                            skip_for_profile_mute = true;
                        }
                    }
                }
                ChatType::MlsGroup => {
                    // For MLS groups, muting is handled at the chat level (already checked above)
                    // No additional profile-level muting needed
                }
            }
            if skip_for_profile_mute {
                continue;
            }

            // Find the last read message ID for this chat
            let last_read_id = &chat.last_read;

            // Walk backwards from the end to count unread messages
            // Stop when we hit: 1) our own message, or 2) the last_read message
            let mut unread_count = 0;
            for msg in chat.messages.iter().rev() {
                // If we hit our own message, stop - we clearly read everything before it
                if msg.mine {
                    break;
                }

                // If we hit the last_read message, stop - everything at and before this is read
                if !last_read_id.is_empty() && msg.id == *last_read_id {
                    break;
                }

                // Count this message as unread
                unread_count += 1;
            }

            total_unread += unread_count as u32;
        }

        total_unread
    }

    /// Find a message by its ID across all chats
    pub fn find_message(&self, message_id: &str) -> Option<(&Chat, &Message)> {
        for chat in &self.chats {
            if let Some(message) = chat.messages.iter().find(|m| m.id == message_id) {
                return Some((chat, message));
            }
        }
        None
    }

    /// Find a chat and message by message ID across all chats (mutable)
    pub fn find_chat_and_message_mut(&mut self, message_id: &str) -> Option<(&str, &mut Message)> {
        for chat in &mut self.chats {
            if let Some(message) = chat.messages.iter_mut().find(|m| m.id == message_id) {
                return Some((&chat.id, message));
            }
        }
        None
    }
}

impl Default for ChatState {
    fn default() -> Self {
        Self::new()
    }
}
