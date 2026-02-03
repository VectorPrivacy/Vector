//! ChatState struct and methods for managing application state.
//!
//! This module contains the core state management for profiles, chats, and sync status.

use nostr_sdk::prelude::*;
use tauri::Emitter;

use crate::message::compact::{CompactMessage, CompactAttachment, NpubInterner};
use crate::{Profile, Chat, ChatType, Message};
use crate::chat::SerializableChat;
use crate::db::SlimProfile;
use super::globals::{TAURI_APP, NOSTR_CLIENT};
use super::SyncMode;
#[cfg(debug_assertions)]
use super::stats::CacheStats;

/// Core application state containing profiles, chats, and sync status.
#[derive(Clone, Debug)]
pub struct ChatState {
    pub profiles: Vec<Profile>,
    pub chats: Vec<Chat>,
    /// Global npub interner - stores each unique npub string once
    pub interner: NpubInterner,
    pub is_syncing: bool,
    pub sync_window_start: u64,
    pub sync_window_end: u64,
    pub sync_mode: SyncMode,
    pub sync_empty_iterations: u8,
    pub sync_total_iterations: u8,
    /// Cache statistics for benchmarking (debug builds only)
    #[cfg(debug_assertions)]
    pub cache_stats: CacheStats,
}

impl ChatState {
    /// Create a new empty ChatState
    pub fn new() -> Self {
        Self {
            profiles: Vec::new(),
            chats: Vec::new(),
            interner: NpubInterner::new(),
            is_syncing: false,
            sync_window_start: 0,
            sync_window_end: 0,
            sync_mode: SyncMode::Finished,
            sync_empty_iterations: 0,
            sync_total_iterations: 0,
            #[cfg(debug_assertions)]
            cache_stats: CacheStats::new(),
        }
    }

    // ========================================================================
    // Profile Management
    // ========================================================================

    /// Merge multiple Vector Profiles from SlimProfile format into the state
    pub async fn merge_db_profiles(&mut self, slim_profiles: Vec<SlimProfile>) {
        let my_public_key = {
            let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
            let signer = client.signer().await.unwrap();
            signer.get_public_key().await.unwrap()
        };

        for slim in slim_profiles {
            if let Some(position) = self.profiles.iter().position(|profile| profile.id == slim.id) {
                let mut full_profile = slim.to_profile();
                if let Ok(profile_pubkey) = PublicKey::from_bech32(&full_profile.id) {
                    full_profile.mine = my_public_key == profile_pubkey;
                }
                self.profiles[position] = full_profile;
            } else {
                self.profiles.push(slim.to_profile());
            }
        }
    }

    pub fn get_profile(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn get_profile_mut(&mut self, id: &str) -> Option<&mut Profile> {
        self.profiles.iter_mut().find(|p| p.id == id)
    }

    // ========================================================================
    // Chat Management
    // ========================================================================

    pub fn get_chat(&self, id: &str) -> Option<&Chat> {
        self.chats.iter().find(|c| c.id == id)
    }

    pub fn get_chat_mut(&mut self, id: &str) -> Option<&mut Chat> {
        self.chats.iter_mut().find(|c| c.id == id)
    }

    /// Get a serializable version of a chat (for frontend)
    #[allow(dead_code)]
    pub fn get_chat_serializable(&self, id: &str) -> Option<SerializableChat> {
        self.get_chat(id).map(|c| c.to_serializable(&self.interner))
    }

    /// Create a new DM chat if it doesn't exist
    pub fn create_dm_chat(&mut self, their_npub: &str) -> String {
        if self.get_chat(their_npub).is_none() {
            let chat = Chat::new_dm(their_npub.to_string());
            self.chats.push(chat);
        }
        their_npub.to_string()
    }

    /// Create or get an MLS group chat
    pub fn create_or_get_mls_group_chat(&mut self, group_id: &str, participants: Vec<String>) -> String {
        if self.get_chat(group_id).is_none() {
            let chat = Chat::new_mls_group(group_id.to_string(), participants);
            self.chats.push(chat);
        }
        group_id.to_string()
    }

    // ========================================================================
    // Message Management
    // ========================================================================

    /// Add a message to a chat via its ID
    pub fn add_message_to_chat(&mut self, chat_id: &str, message: Message) -> bool {
        #[cfg(debug_assertions)]
        let start = std::time::Instant::now();

        // Convert to compact using our interner
        let compact = CompactMessage::from_message(&message, &mut self.interner);

        let is_msg_added = if let Some(chat) = self.chats.iter_mut().find(|c| c.id == chat_id) {
            chat.add_compact_message(compact)
        } else {
            // Chat doesn't exist, create it
            let mut chat = if chat_id.starts_with("npub1") {
                Chat::new_dm(chat_id.to_string())
            } else {
                Chat::new(chat_id.to_string(), ChatType::MlsGroup, vec![])
            };
            let was_added = chat.add_compact_message(compact);
            self.chats.push(chat);
            was_added
        };

        // Sort chats by last message time (newest first)
        self.chats.sort_by(|a, b| {
            b.last_message_time().cmp(&a.last_message_time())
        });

        // Track stats (debug builds only)
        #[cfg(debug_assertions)]
        if is_msg_added {
            self.cache_stats.record_insert(start.elapsed());
            if self.cache_stats.should_log(100) {
                self.update_and_log_stats();
            }
        }

        is_msg_added
    }

    /// Batch add messages to a chat - much faster for pagination/history loads.
    ///
    /// Sorts chats only once at the end instead of per-message.
    /// Returns the number of messages actually added.
    pub fn add_messages_to_chat_batch(&mut self, chat_id: &str, messages: Vec<Message>) -> usize {
        if messages.is_empty() {
            return 0;
        }

        // Convert all messages to compact format (zero-copy - moves strings!)
        let compact_messages: Vec<_> = messages.into_iter()
            .map(|msg| CompactMessage::from_message_owned(msg, &mut self.interner))
            .collect();

        // Find or create the chat
        let chat = if let Some(chat) = self.chats.iter_mut().find(|c| c.id == chat_id) {
            chat
        } else {
            // Chat doesn't exist, create it
            let chat = if chat_id.starts_with("npub1") {
                Chat::new_dm(chat_id.to_string())
            } else {
                Chat::new(chat_id.to_string(), ChatType::MlsGroup, vec![])
            };
            self.chats.push(chat);
            self.chats.last_mut().unwrap()
        };

        // Track if last message time changes (only happens when adding newer messages)
        let old_last_time = chat.messages.last_timestamp();

        // Batch insert all messages
        let added = chat.messages.insert_batch(compact_messages);

        // Only sort chats if the last message time changed (i.e., we added newer messages)
        // Prepending older messages (pagination) doesn't change chat order
        if added > 0 && chat.messages.last_timestamp() != old_last_time {
            self.chats.sort_by(|a, b| {
                b.last_message_time().cmp(&a.last_message_time())
            });
        }

        added
    }

    /// Add a message to a chat via participant npub
    pub fn add_message_to_participant(&mut self, their_npub: &str, message: Message) -> bool {
        // Ensure profile exists
        if self.get_profile(their_npub).is_none() {
            let mut profile = Profile::new();
            profile.id = their_npub.to_string();
            profile.mine = false;

            if let Some(handle) = TAURI_APP.get() {
                handle.emit("profile_update", &profile).unwrap();
            }

            self.profiles.push(profile);
        }

        let chat_id = self.create_dm_chat(their_npub);
        self.add_message_to_chat(&chat_id, message)
    }

    // ========================================================================
    // Message Lookup (O(n chats × log m messages))
    // ========================================================================

    /// Find a message by ID across all chats - O(n × log m)
    pub fn find_message(&self, message_id: &str) -> Option<(&Chat, Message)> {
        if message_id.is_empty() {
            return None;
        }

        for chat in &self.chats {
            if let Some(compact) = chat.get_compact_message(message_id) {
                return Some((chat, compact.to_message(&self.interner)));
            }
        }
        None
    }

    /// Find which chat contains a message - O(n × log m)
    #[allow(dead_code)]
    pub fn find_chat_for_message(&self, message_id: &str) -> Option<(usize, String)> {
        if message_id.is_empty() {
            return None;
        }

        for (idx, chat) in self.chats.iter().enumerate() {
            if chat.has_message(message_id) {
                return Some((idx, chat.id.clone()));
            }
        }
        None
    }

    /// Find a chat and message by message ID (mutable) - O(n × log m)
    #[allow(dead_code)]
    pub fn find_chat_and_message_mut(&mut self, message_id: &str) -> Option<(String, &mut CompactMessage)> {
        if message_id.is_empty() {
            return None;
        }

        // First find which chat has it
        let chat_idx = self.chats.iter()
            .position(|chat| chat.has_message(message_id))?;

        let chat_id = self.chats[chat_idx].id.clone();
        let msg = self.chats[chat_idx].get_compact_message_mut(message_id)?;

        Some((chat_id, msg))
    }

    /// Update a message by ID and return (chat_id, Message) for save/emit.
    ///
    /// This is the preferred pattern for mutating messages - it handles:
    /// 1. Finding the chat containing the message
    /// 2. Calling your mutation closure
    /// 3. Converting back to Message format for database save
    ///
    /// Example:
    /// ```
    /// let result = state.update_message(&msg_id, |msg| {
    ///     msg.preview_metadata = Some(Box::new(metadata));
    /// });
    /// if let Some((chat_id, message)) = result {
    ///     db::save_message(handle, &chat_id, &message).await;
    /// }
    /// ```
    pub fn update_message<F>(&mut self, message_id: &str, f: F) -> Option<(String, Message)>
    where
        F: FnOnce(&mut CompactMessage),
    {
        if message_id.is_empty() {
            return None;
        }

        // Find which chat has this message
        let chat_idx = self.chats.iter()
            .position(|chat| chat.has_message(message_id))?;

        // Mutate the message
        if let Some(msg) = self.chats[chat_idx].get_compact_message_mut(message_id) {
            f(msg);
        }

        // Get chat_id and convert to Message (reborrow is fine now)
        let chat_id = self.chats[chat_idx].id.clone();
        self.chats[chat_idx].get_compact_message(message_id)
            .map(|m| (chat_id, m.to_message(&self.interner)))
    }

    /// Update a message in a specific chat and return Message for save/emit.
    ///
    /// Use this when you already know the chat_id (avoids searching all chats).
    pub fn update_message_in_chat<F>(&mut self, chat_id: &str, message_id: &str, f: F) -> Option<Message>
    where
        F: FnOnce(&mut CompactMessage),
    {
        let chat_idx = self.chats.iter().position(|c| c.id == chat_id)?;

        if let Some(msg) = self.chats[chat_idx].get_compact_message_mut(message_id) {
            f(msg);
        }

        self.chats[chat_idx].get_compact_message(message_id)
            .map(|m| m.to_message(&self.interner))
    }

    /// Finalize a pending message by updating its ID to the real event ID.
    ///
    /// This is used when a message is confirmed sent and receives its final ID.
    /// Rebuilds the message index since the ID changed.
    /// Returns (old_id, Message) for emit/save.
    pub fn finalize_pending_message(&mut self, chat_id: &str, pending_id: &str, real_id: &str) -> Option<(String, Message)> {
        let chat_idx = self.chats.iter().position(|c| c.id == chat_id)?;

        // Update the message ID
        if let Some(msg) = self.chats[chat_idx].get_compact_message_mut(pending_id) {
            msg.id = crate::simd::hex_to_bytes_32(real_id);
            msg.set_pending(false);
        }

        // Rebuild index since ID changed
        self.chats[chat_idx].messages.rebuild_index();

        // Get the message with new ID for emit/save
        self.chats[chat_idx].get_compact_message(real_id)
            .map(|m| (pending_id.to_string(), m.to_message(&self.interner)))
    }

    /// Update an attachment within a message.
    ///
    /// Searches for the chat by hint (group_id for MLS, participant npub for DMs),
    /// finds the message, finds the attachment, and applies the mutation.
    /// Returns true if the attachment was found and updated.
    ///
    /// Example:
    /// ```
    /// state.update_attachment(&npub, &msg_id, &attachment_id, |att| {
    ///     att.set_downloaded(true);
    ///     att.path = path.into_boxed_str();
    /// });
    /// ```
    pub fn update_attachment<F>(&mut self, chat_hint: &str, msg_id: &str, attachment_id: &str, f: F) -> bool
    where
        F: FnOnce(&mut CompactAttachment),
    {
        for chat in &mut self.chats {
            let is_target = match &chat.chat_type {
                ChatType::MlsGroup => chat.id == chat_hint,
                ChatType::DirectMessage => chat.has_participant(chat_hint),
            };

            if is_target {
                if let Some(msg) = chat.messages.find_by_hex_id_mut(msg_id) {
                    if let Some(att) = msg.attachments.iter_mut().find(|a| a.id_eq(attachment_id)) {
                        f(att);
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Add an attachment to a message.
    ///
    /// Finds the message by ID in the specified chat and appends the attachment.
    /// Returns true if the message was found and attachment added.
    pub fn add_attachment_to_message(&mut self, chat_id: &str, msg_id: &str, attachment: CompactAttachment) -> bool {
        let chat_idx = match self.chats.iter().position(|c| c.id == chat_id || c.has_participant(chat_id)) {
            Some(idx) => idx,
            None => return false,
        };

        if let Some(msg) = self.chats[chat_idx].messages.find_by_hex_id_mut(msg_id) {
            msg.attachments.push(attachment);
            true
        } else {
            false
        }
    }

    /// Add a reaction to a message by ID - handles interner access via split borrowing
    /// Returns (chat_id, was_added) if the message was found
    pub fn add_reaction_to_message(&mut self, message_id: &str, reaction: crate::message::Reaction) -> Option<(String, bool)> {
        if message_id.is_empty() {
            return None;
        }

        // Find which chat has it
        let chat_idx = self.chats.iter()
            .position(|chat| chat.has_message(message_id))?;

        let chat_id = self.chats[chat_idx].id.clone();

        // Split borrow: access chats[chat_idx] and interner separately
        let msg = self.chats[chat_idx].get_compact_message_mut(message_id)?;
        let added = msg.add_reaction(reaction, &mut self.interner);

        Some((chat_id, added))
    }

    /// Check if a message exists - O(n × log m)
    #[allow(dead_code)]
    pub fn message_exists(&self, message_id: &str) -> bool {
        if message_id.is_empty() {
            return false;
        }
        self.chats.iter().any(|chat| chat.has_message(message_id))
    }

    // ========================================================================
    // Unread Count
    // ========================================================================

    /// Count unread messages across all chats
    pub fn count_unread_messages(&self) -> u32 {
        let mut total_unread = 0;

        for chat in &self.chats {
            if chat.muted {
                continue;
            }

            // Skip if profile is muted (for DMs)
            if matches!(chat.chat_type, ChatType::DirectMessage) {
                if let Some(profile) = self.get_profile(&chat.id) {
                    if profile.muted {
                        continue;
                    }
                }
            }

            let last_read_id = &chat.last_read;

            let mut unread_count = 0;
            for msg in chat.iter_compact().rev() {
                if msg.flags.is_mine() {
                    break;
                }
                if !last_read_id.is_empty() && msg.id_hex() == *last_read_id {
                    break;
                }
                unread_count += 1;
            }

            total_unread += unread_count;
        }

        total_unread
    }

    // ========================================================================
    // Statistics (debug builds only)
    // ========================================================================

    #[cfg(debug_assertions)]
    fn update_and_log_stats(&mut self) {
        self.cache_stats.chat_count = self.chats.len();
        self.cache_stats.message_count = self.chats.iter()
            .map(|c| c.message_count())
            .sum();
        self.cache_stats.total_memory_bytes =
            self.cache_stats.message_count * 500 + self.interner.memory_usage();
        self.cache_stats.log();
        println!("[Interner] {} unique npubs, {} bytes",
            self.interner.len(), self.interner.memory_usage());
    }

    #[cfg(debug_assertions)]
    pub fn log_cache_stats(&mut self) {
        self.update_and_log_stats();
    }

    #[cfg(debug_assertions)]
    pub fn cache_stats_summary(&mut self) -> String {
        self.cache_stats.chat_count = self.chats.len();
        self.cache_stats.message_count = self.chats.iter()
            .map(|c| c.message_count())
            .sum();
        format!("{} interner={} npubs",
            self.cache_stats.summary(),
            self.interner.len()
        )
    }
}

impl Default for ChatState {
    fn default() -> Self {
        Self::new()
    }
}
