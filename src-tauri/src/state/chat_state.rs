//! ChatState struct and methods for managing application state.
//!
//! This module contains the core state management for profiles, chats, and sync status.

use tauri::Emitter;

use crate::message::compact::{CompactMessage, CompactAttachment, NpubInterner};
use crate::{Profile, Chat, ChatType, Message};
use crate::chat::SerializableChat;
use crate::db::SlimProfile;
use super::globals::TAURI_APP;
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
    /// Whether the initial DB load (profiles, chats, messages) has completed.
    /// Used instead of `profiles.len()` heuristics, which break when the
    /// background service preloads profiles into STATE before the GUI init.
    pub db_loaded: bool,
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
            db_loaded: false,
            #[cfg(debug_assertions)]
            cache_stats: CacheStats::new(),
        }
    }

    // ========================================================================
    // Profile Management
    // ========================================================================

    /// Merge multiple Vector Profiles from SlimProfile format into the state.
    ///
    /// Profiles are kept sorted by interner handle for O(log n) integer binary search.
    pub fn merge_db_profiles(&mut self, slim_profiles: Vec<SlimProfile>, my_npub: &str) {
        for slim in slim_profiles {
            let mut full_profile = slim.to_profile();
            full_profile.flags.set_mine(slim.id == my_npub);
            self.insert_or_replace_profile(&slim.id, full_profile);
        }
    }

    /// Insert a profile in sorted order by interner handle, or replace if it already exists.
    ///
    /// Interns the npub to assign the profile its `id` handle.
    pub fn insert_or_replace_profile(&mut self, npub: &str, mut profile: Profile) {
        let id = self.interner.intern(npub);
        profile.id = id;
        match self.profiles.binary_search_by(|p| p.id.cmp(&id)) {
            Ok(idx) => self.profiles[idx] = profile,
            Err(idx) => self.profiles.insert(idx, profile),
        }
    }

    /// Look up a profile by npub string (boundary method).
    ///
    /// Delegates through the interner: O(log n) string search → O(log n) integer search.
    /// Prefer `get_profile_by_id` when a handle is already available.
    pub fn get_profile(&self, npub: &str) -> Option<&Profile> {
        let id = self.interner.lookup(npub)?;
        self.get_profile_by_id(id)
    }

    pub fn get_profile_mut(&mut self, npub: &str) -> Option<&mut Profile> {
        let id = self.interner.lookup(npub)?;
        self.get_profile_mut_by_id(id)
    }

    /// Look up a profile by interner handle — O(log n) integer binary search.
    #[inline]
    pub fn get_profile_by_id(&self, id: u16) -> Option<&Profile> {
        self.profiles.binary_search_by(|p| p.id.cmp(&id))
            .ok().map(|idx| &self.profiles[idx])
    }

    #[inline]
    pub fn get_profile_mut_by_id(&mut self, id: u16) -> Option<&mut Profile> {
        self.profiles.binary_search_by(|p| p.id.cmp(&id))
            .ok().map(move |idx| &mut self.profiles[idx])
    }

    /// Serialize a profile for frontend/DB boundary (resolves u16 id → npub string).
    pub fn serialize_profile(&self, id: u16) -> Option<SlimProfile> {
        self.get_profile_by_id(id)
            .map(|p| SlimProfile::from_profile(p, &self.interner))
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
            let chat = Chat::new_dm(their_npub.to_string(), &mut self.interner);
            self.chats.push(chat);
        }
        their_npub.to_string()
    }

    /// Create or get an MLS group chat
    pub fn create_or_get_mls_group_chat(&mut self, group_id: &str, participants: Vec<String>) -> String {
        if self.get_chat(group_id).is_none() {
            let chat = Chat::new_mls_group(group_id.to_string(), participants, &mut self.interner);
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

        let (is_msg_added, chat_idx) = if let Some(idx) = self.chats.iter().position(|c| c.id == chat_id) {
            let added = self.chats[idx].add_compact_message(compact);
            (added, idx)
        } else {
            // Chat doesn't exist, create it
            let mut chat = if chat_id.starts_with("npub1") {
                Chat::new_dm(chat_id.to_string(), &mut self.interner)
            } else {
                Chat::new(chat_id.to_string(), ChatType::MlsGroup, vec![])
            };
            let was_added = chat.add_compact_message(compact);
            self.chats.push(chat);
            (was_added, self.chats.len() - 1)
        };

        // Move chat to its correct sorted position (newest first).
        // Common case: new message makes this the most recent chat → move to front.
        // This is O(n) rotate at worst but O(1) when already in position (the common
        // case for an active conversation that's already at the front of the list).
        if is_msg_added && chat_idx > 0 {
            let this_time = self.chats[chat_idx].last_message_time();
            // Find where this chat belongs (first chat with an older or equal timestamp)
            let target = self.chats[..chat_idx].iter()
                .position(|c| c.last_message_time() <= this_time)
                .unwrap_or(chat_idx);
            if target < chat_idx {
                self.chats[target..=chat_idx].rotate_right(1);
            }
        }

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
    /// Only repositions the chat if newer messages were added (pagination won't trigger re-sort).
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
        let chat_idx = if let Some(idx) = self.chats.iter().position(|c| c.id == chat_id) {
            idx
        } else {
            let chat = if chat_id.starts_with("npub1") {
                Chat::new_dm(chat_id.to_string(), &mut self.interner)
            } else {
                Chat::new(chat_id.to_string(), ChatType::MlsGroup, vec![])
            };
            self.chats.push(chat);
            self.chats.len() - 1
        };

        // Track if last message time changes (only happens when adding newer messages)
        let old_last_time = self.chats[chat_idx].messages.last_timestamp();

        // Batch insert all messages
        let added = self.chats[chat_idx].messages.insert_batch(compact_messages);

        // Only reposition if the last message time changed (i.e., we added newer messages)
        // Prepending older messages (pagination) doesn't change chat order
        if added > 0 && self.chats[chat_idx].messages.last_timestamp() != old_last_time && chat_idx > 0 {
            let this_time = self.chats[chat_idx].last_message_time();
            let target = self.chats[..chat_idx].iter()
                .position(|c| c.last_message_time() <= this_time)
                .unwrap_or(chat_idx);
            if target < chat_idx {
                self.chats[target..=chat_idx].rotate_right(1);
            }
        }

        added
    }

    /// Add a message to a chat via participant npub
    pub fn add_message_to_participant(&mut self, their_npub: &str, message: Message) -> bool {
        // Ensure profile exists
        let id = self.interner.intern(their_npub);
        if self.get_profile_by_id(id).is_none() {
            let profile = Profile::new();
            self.insert_or_replace_profile(their_npub, profile);

            if let Some(handle) = TAURI_APP.get() {
                let slim = self.serialize_profile(id).unwrap();
                handle.emit("profile_update", &slim).unwrap();
            }
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
    ///     db::save_message(&chat_id, &message).await;
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
                ChatType::DirectMessage => chat.has_participant(chat_hint, &self.interner),
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
        let chat_idx = match self.chats.iter().position(|c| c.id == chat_id || c.has_participant(chat_id, &self.interner)) {
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

            let mut unread_count = 0;
            for msg in chat.iter_compact().rev() {
                if msg.flags.is_mine() {
                    break;
                }
                if chat.last_read != [0u8; 32] && msg.id == chat.last_read {
                    break;
                }
                unread_count += 1;
            }

            total_unread += unread_count;
        }

        total_unread
    }

    // ========================================================================
    // Typing Indicators
    // ========================================================================

    /// Update typing indicator for an npub in a chat and return active typers.
    ///
    /// Handles split borrowing of `self.interner` and `self.chats`:
    /// interns the npub, finds the chat, updates typing, returns active typers.
    pub fn update_typing_and_get_active(&mut self, chat_id: &str, npub: &str, expires_at: u64) -> Vec<String> {
        let handle = self.interner.intern(npub);
        if let Some(chat) = self.chats.iter_mut().find(|c| c.id == chat_id) {
            chat.update_typing_participant(handle, expires_at);
            chat.get_active_typers(&self.interner)
        } else {
            Vec::new()
        }
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

}

impl Default for ChatState {
    fn default() -> Self {
        Self::new()
    }
}
