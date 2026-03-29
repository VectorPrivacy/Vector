//! Global state management — ChatState, globals, processing gate.
//!
//! All Tauri-specific globals (TAURI_APP) have been removed. Event emission
//! uses the `EventEmitter` trait via `crate::traits::emit_event`.

use nostr_sdk::prelude::*;
use std::sync::OnceLock;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;
use tokio::sync::Mutex;

use crate::chat::{Chat, ChatType};
use crate::compact::{CompactMessage, CompactAttachment, NpubInterner, NO_NPUB};
use crate::profile::{Profile, SlimProfile};
use crate::types::{Message, Reaction};
use crate::traits::emit_event;

// ============================================================================
// WrapperIdCache — Hybrid duplicate detection during sync
// ============================================================================

pub struct WrapperIdCache {
    historical: Vec<[u8; 32]>,
    pending: HashSet<[u8; 32]>,
}

impl WrapperIdCache {
    pub fn new() -> Self { Self { historical: Vec::new(), pending: HashSet::new() } }

    pub fn load(&mut self, mut ids: Vec<[u8; 32]>) {
        ids.sort_unstable();
        self.historical = ids;
        self.pending.clear();
    }

    #[inline]
    pub fn contains(&self, id: &[u8; 32]) -> bool {
        self.historical.binary_search(id).is_ok() || self.pending.contains(id)
    }

    #[inline]
    pub fn insert(&mut self, id: [u8; 32]) { self.pending.insert(id); }

    pub fn clear(&mut self) {
        self.historical.clear();
        self.historical.shrink_to_fit();
        self.pending.clear();
        self.pending.shrink_to_fit();
    }

    pub fn len(&self) -> usize { self.historical.len() + self.pending.len() }
}

impl Default for WrapperIdCache {
    fn default() -> Self { Self::new() }
}

// ============================================================================
// Globals
// ============================================================================

pub static TRUSTED_RELAYS: &[&str] = &[
    "wss://jskitty.cat/nostr",
    "wss://asia.vectorapp.io/nostr",
    "wss://nostr.computingcache.com",
];

pub async fn active_trusted_relays() -> Vec<&'static str> {
    let Some(client) = NOSTR_CLIENT.get() else { return Vec::new() };
    let pool_relays = client.relays().await;
    TRUSTED_RELAYS.iter().copied()
        .filter(|url| {
            let normalized = url.trim_end_matches('/');
            pool_relays.keys().any(|r| r.as_str().trim_end_matches('/') == normalized)
        })
        .collect()
}

/// Blossom media servers with failover.
pub static BLOSSOM_SERVERS: OnceLock<std::sync::Mutex<Vec<String>>> = OnceLock::new();

pub fn init_blossom_servers() -> Vec<String> {
    vec!["https://blossom.primal.net".to_string()]
}

pub fn get_blossom_servers() -> Vec<String> {
    BLOSSOM_SERVERS
        .get_or_init(|| std::sync::Mutex::new(init_blossom_servers()))
        .lock().unwrap().clone()
}

pub static MNEMONIC_SEED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
pub static PENDING_NSEC: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub static ENCRYPTION_KEY: crate::crypto::GuardedKey = crate::crypto::GuardedKey::empty();

pub static ENCRYPTION_ENABLED: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn is_encryption_enabled_fast() -> bool { ENCRYPTION_ENABLED.load(Ordering::Acquire) }

#[inline]
pub fn set_encryption_enabled(enabled: bool) { ENCRYPTION_ENABLED.store(enabled, Ordering::Release); }

pub fn init_encryption_enabled() {
    let enabled = crate::db::get_sql_setting("encryption_enabled".to_string())
        .ok().flatten()
        .map(|v| v != "false")
        .unwrap_or(true);
    set_encryption_enabled(enabled);
}

pub static NOSTR_CLIENT: OnceLock<Client> = OnceLock::new();

pub static MY_SECRET_KEY: crate::crypto::GuardedKey = crate::crypto::GuardedKey::empty();

pub static MY_PUBLIC_KEY: OnceLock<PublicKey> = OnceLock::new();

#[derive(Clone)]
pub struct PendingInviteAcceptance {
    pub invite_code: String,
    pub inviter_pubkey: PublicKey,
}

pub static PENDING_INVITE: OnceLock<PendingInviteAcceptance> = OnceLock::new();

pub static NOTIFIED_WELCOMES: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

pub static WRAPPER_ID_CACHE: LazyLock<Mutex<WrapperIdCache>> = LazyLock::new(|| Mutex::new(WrapperIdCache::new()));

pub static STATE: LazyLock<Mutex<ChatState>> = LazyLock::new(|| Mutex::new(ChatState::new()));

// ============================================================================
// Processing Gate — Controls event processing during encryption migration
// ============================================================================

pub static PROCESSING_GATE: AtomicBool = AtomicBool::new(true);
pub static PENDING_EVENTS: LazyLock<Mutex<Vec<(Event, bool)>>> = LazyLock::new(|| Mutex::new(Vec::new()));

#[inline]
pub fn is_processing_allowed() -> bool { PROCESSING_GATE.load(Ordering::Acquire) }
pub fn close_processing_gate() { PROCESSING_GATE.store(false, Ordering::Release); }
pub fn open_processing_gate() { PROCESSING_GATE.store(true, Ordering::Release); }

// ============================================================================
// ChatState
// ============================================================================

#[derive(Clone, Debug)]
pub struct ChatState {
    pub profiles: Vec<Profile>,
    pub chats: Vec<Chat>,
    pub interner: NpubInterner,
    pub is_syncing: bool,
    pub db_loaded: bool,
    #[cfg(debug_assertions)]
    pub cache_stats: crate::stats::CacheStats,
}

impl ChatState {
    pub fn new() -> Self {
        Self {
            profiles: Vec::new(),
            chats: Vec::new(),
            interner: NpubInterner::new(),
            is_syncing: false,
            db_loaded: false,
            #[cfg(debug_assertions)]
            cache_stats: crate::stats::CacheStats::new(),
        }
    }

    // ========================================================================
    // Profile Management
    // ========================================================================

    pub fn merge_db_profiles(&mut self, slim_profiles: Vec<SlimProfile>, my_npub: &str) {
        for slim in slim_profiles {
            let mut full_profile = slim.to_profile();
            full_profile.flags.set_mine(slim.id == my_npub);
            self.insert_or_replace_profile(&slim.id, full_profile);
        }
    }

    pub fn insert_or_replace_profile(&mut self, npub: &str, mut profile: Profile) {
        let id = self.interner.intern(npub);
        profile.id = id;
        match self.profiles.binary_search_by(|p| p.id.cmp(&id)) {
            Ok(idx) => self.profiles[idx] = profile,
            Err(idx) => self.profiles.insert(idx, profile),
        }
    }

    pub fn get_profile(&self, npub: &str) -> Option<&Profile> {
        self.interner.lookup(npub).and_then(|id| self.get_profile_by_id(id))
    }

    pub fn get_profile_mut(&mut self, npub: &str) -> Option<&mut Profile> {
        self.interner.lookup(npub).and_then(move |id| self.get_profile_mut_by_id(id))
    }

    #[inline]
    pub fn get_profile_by_id(&self, id: u16) -> Option<&Profile> {
        self.profiles.binary_search_by(|p| p.id.cmp(&id)).ok().map(|idx| &self.profiles[idx])
    }

    #[inline]
    pub fn get_profile_mut_by_id(&mut self, id: u16) -> Option<&mut Profile> {
        self.profiles.binary_search_by(|p| p.id.cmp(&id)).ok().map(move |idx| &mut self.profiles[idx])
    }

    pub fn serialize_profile(&self, id: u16) -> Option<SlimProfile> {
        self.get_profile_by_id(id).map(|p| SlimProfile::from_profile(p, &self.interner))
    }

    // ========================================================================
    // Chat Management
    // ========================================================================

    pub fn get_chat(&self, id: &str) -> Option<&Chat> { self.chats.iter().find(|c| c.id == id) }
    pub fn get_chat_mut(&mut self, id: &str) -> Option<&mut Chat> { self.chats.iter_mut().find(|c| c.id == id) }

    pub fn create_dm_chat(&mut self, their_npub: &str) -> String {
        if self.get_chat(their_npub).is_none() {
            let chat = Chat::new_dm(their_npub.to_string(), &mut self.interner);
            self.chats.push(chat);
        }
        their_npub.to_string()
    }

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

    pub fn add_message_to_chat(&mut self, chat_id: &str, message: Message) -> bool {
        let compact = CompactMessage::from_message(&message, &mut self.interner);

        let (is_msg_added, chat_idx) = if let Some(idx) = self.chats.iter().position(|c| c.id == chat_id) {
            let added = self.chats[idx].add_compact_message(compact);
            (added, idx)
        } else {
            let mut chat = if chat_id.starts_with("npub1") {
                Chat::new_dm(chat_id.to_string(), &mut self.interner)
            } else {
                Chat::new(chat_id.to_string(), ChatType::MlsGroup, vec![])
            };
            let was_added = chat.add_compact_message(compact);
            self.chats.push(chat);
            (was_added, self.chats.len() - 1)
        };

        if is_msg_added && chat_idx > 0 {
            let this_time = self.chats[chat_idx].last_message_time();
            let target = self.chats[..chat_idx].iter()
                .position(|c| c.last_message_time() <= this_time)
                .unwrap_or(chat_idx);
            if target < chat_idx {
                self.chats[target..=chat_idx].rotate_right(1);
            }
        }

        is_msg_added
    }

    pub fn add_messages_to_chat_batch(&mut self, chat_id: &str, messages: Vec<Message>) -> usize {
        if messages.is_empty() { return 0; }

        let compact_messages: Vec<_> = messages.into_iter()
            .map(|msg| CompactMessage::from_message_owned(msg, &mut self.interner))
            .collect();

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

        let old_last_time = self.chats[chat_idx].messages.last_timestamp();
        let added = self.chats[chat_idx].messages.insert_batch(compact_messages);

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

    /// Add a message to a participant's DM chat. Creates profile if missing.
    ///
    /// Unlike the src-tauri version, emitting `profile_update` is the caller's responsibility.
    pub fn add_message_to_participant(&mut self, their_npub: &str, message: Message) -> bool {
        let id = self.interner.intern(their_npub);
        if self.get_profile_by_id(id).is_none() {
            let profile = Profile::new();
            self.insert_or_replace_profile(their_npub, profile);

            // Emit profile update via EventEmitter trait (replaces TAURI_APP.emit)
            if let Some(slim) = self.serialize_profile(id) {
                emit_event("profile_update", &slim);
            }
        }

        let chat_id = self.create_dm_chat(their_npub);
        self.add_message_to_chat(&chat_id, message)
    }

    // ========================================================================
    // Message Lookup
    // ========================================================================

    pub fn find_message(&self, message_id: &str) -> Option<(&Chat, Message)> {
        if message_id.is_empty() { return None; }
        for chat in &self.chats {
            if let Some(compact) = chat.get_compact_message(message_id) {
                return Some((chat, compact.to_message(&self.interner)));
            }
        }
        None
    }

    pub fn find_chat_for_message(&self, message_id: &str) -> Option<(usize, String)> {
        if message_id.is_empty() { return None; }
        for (idx, chat) in self.chats.iter().enumerate() {
            if chat.has_message(message_id) { return Some((idx, chat.id.clone())); }
        }
        None
    }

    pub fn update_message<F>(&mut self, message_id: &str, f: F) -> Option<(String, Message)>
    where F: FnOnce(&mut CompactMessage)
    {
        if message_id.is_empty() { return None; }
        let chat_idx = self.chats.iter().position(|chat| chat.has_message(message_id))?;
        if let Some(msg) = self.chats[chat_idx].get_compact_message_mut(message_id) { f(msg); }
        let chat_id = self.chats[chat_idx].id.clone();
        self.chats[chat_idx].get_compact_message(message_id).map(|m| (chat_id, m.to_message(&self.interner)))
    }

    pub fn update_message_in_chat<F>(&mut self, chat_id: &str, message_id: &str, f: F) -> Option<Message>
    where F: FnOnce(&mut CompactMessage)
    {
        let chat_idx = self.chats.iter().position(|c| c.id == chat_id)?;
        if let Some(msg) = self.chats[chat_idx].get_compact_message_mut(message_id) { f(msg); }
        self.chats[chat_idx].get_compact_message(message_id).map(|m| m.to_message(&self.interner))
    }

    pub fn finalize_pending_message(&mut self, chat_id: &str, pending_id: &str, real_id: &str) -> Option<(String, Message)> {
        let chat_idx = self.chats.iter().position(|c| c.id == chat_id)?;
        if let Some(msg) = self.chats[chat_idx].get_compact_message_mut(pending_id) {
            msg.id = crate::hex::hex_to_bytes_32(real_id);
            msg.set_pending(false);
        }
        self.chats[chat_idx].messages.rebuild_index();
        self.chats[chat_idx].get_compact_message(real_id)
            .map(|m| (pending_id.to_string(), m.to_message(&self.interner)))
    }

    pub fn update_attachment<F>(&mut self, chat_hint: &str, msg_id: &str, attachment_id: &str, f: F) -> bool
    where F: FnOnce(&mut CompactAttachment)
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

    pub fn add_attachment_to_message(&mut self, chat_id: &str, msg_id: &str, attachment: CompactAttachment) -> bool {
        let chat_idx = match self.chats.iter().position(|c| c.id == chat_id || c.has_participant(chat_id, &self.interner)) {
            Some(idx) => idx,
            None => return false,
        };
        if let Some(msg) = self.chats[chat_idx].messages.find_by_hex_id_mut(msg_id) {
            msg.attachments.push(attachment);
            true
        } else { false }
    }

    pub fn add_reaction_to_message(&mut self, message_id: &str, reaction: Reaction) -> Option<(String, bool)> {
        if message_id.is_empty() { return None; }
        let chat_idx = self.chats.iter().position(|chat| chat.has_message(message_id))?;
        let chat_id = self.chats[chat_idx].id.clone();
        let msg = self.chats[chat_idx].get_compact_message_mut(message_id)?;
        let added = msg.add_reaction(reaction, &mut self.interner);
        Some((chat_id, added))
    }

    pub fn remove_message(&mut self, message_id: &str) -> Option<(String, Message)> {
        if message_id.is_empty() { return None; }
        for chat in &mut self.chats {
            if let Some(compact) = chat.messages.find_by_hex_id(message_id) {
                let msg = compact.to_message(&self.interner);
                let chat_id = chat.id.clone();
                chat.messages.remove_by_hex_id(message_id);
                return Some((chat_id, msg));
            }
        }
        None
    }

    pub fn message_exists(&self, message_id: &str) -> bool {
        !message_id.is_empty() && self.chats.iter().any(|chat| chat.has_message(message_id))
    }

    // ========================================================================
    // Unread Count
    // ========================================================================

    pub fn count_unread_messages(&self) -> u32 {
        let mut total_unread = 0;
        for chat in &self.chats {
            if chat.muted { continue; }
            let is_group = chat.is_mls_group();
            if !is_group {
                if let Some(id) = self.interner.lookup(&chat.id) {
                    if self.get_profile_by_id(id).map_or(false, |p| p.flags.is_blocked()) { continue; }
                }
            }
            let mut unread_count = 0u32;
            for msg in chat.iter_compact().rev() {
                if msg.flags.is_mine() { break; }
                if chat.last_read != [0u8; 32] && msg.id == chat.last_read { break; }
                if is_group && msg.npub_idx != NO_NPUB {
                    if self.get_profile_by_id(msg.npub_idx).map_or(false, |p| p.flags.is_blocked()) { continue; }
                }
                unread_count += 1;
            }
            // Debug: log which chat has unread messages
            #[cfg(debug_assertions)]
            if unread_count > 0 {
                let last_read_hex = crate::compact::decode_message_id(&chat.last_read);
                let last_msg_hex = chat.messages.last().map(|m| crate::compact::decode_message_id(&m.id)).unwrap_or_default();
                let msg_count = chat.message_count();
                eprintln!("[Unread] chat={} unread={} msgs_in_memory={} last_read={} last_msg={}",
                    &chat.id[..20.min(chat.id.len())], unread_count, msg_count,
                    &last_read_hex[..16.min(last_read_hex.len())], &last_msg_hex[..16.min(last_msg_hex.len())]);
            }
            total_unread += unread_count;
        }
        total_unread
    }

    // ========================================================================
    // Typing Indicators
    // ========================================================================

    pub fn update_typing_and_get_active(&mut self, chat_id: &str, npub: &str, expires_at: u64) -> Vec<String> {
        let handle = self.interner.intern(npub);
        if let Some(chat) = self.chats.iter_mut().find(|c| c.id == chat_id) {
            chat.update_typing_participant(handle, expires_at);
            chat.get_active_typers(&self.interner)
        } else {
            Vec::new()
        }
    }
}

impl Default for ChatState {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;
    use crate::profile::{Profile, SlimProfile, Status};
    use crate::hex::bytes_to_hex_32;

    // ========================================================================
    // Helpers
    // ========================================================================

    /// Create a deterministic 64-char hex ID from a u8 seed.
    /// First byte is always >= 0x10 to avoid the pending ID marker (0x01).
    fn make_hex_id(seed: u8) -> String {
        let mut bytes = [seed; 32];
        bytes[0] = seed.wrapping_add(0x10) | 0x10; // never 0x00 or 0x01
        bytes[1] = seed.wrapping_mul(37);
        bytes_to_hex_32(&bytes)
    }

    /// Build a test Message with the given parameters.
    fn make_message(id_seed: u8, content: &str, timestamp_ms: u64, mine: bool) -> Message {
        Message {
            id: make_hex_id(id_seed),
            content: content.to_string(),
            at: timestamp_ms,
            mine,
            ..Default::default()
        }
    }

    /// Build a message with an npub sender.
    fn make_message_from(id_seed: u8, content: &str, timestamp_ms: u64, npub: &str) -> Message {
        Message {
            id: make_hex_id(id_seed),
            content: content.to_string(),
            at: timestamp_ms,
            mine: false,
            npub: Some(npub.to_string()),
            ..Default::default()
        }
    }

    /// Build a SlimProfile for testing.
    fn make_slim_profile(id: &str, name: &str) -> SlimProfile {
        SlimProfile {
            id: id.to_string(),
            name: name.to_string(),
            display_name: String::new(),
            nickname: String::new(),
            lud06: String::new(),
            lud16: String::new(),
            banner: String::new(),
            avatar: String::new(),
            about: String::new(),
            website: String::new(),
            nip05: String::new(),
            status: Status::new(),
            last_updated: 0,
            mine: false,
            bot: false,
            is_blocked: false,
            avatar_cached: String::new(),
            banner_cached: String::new(),
        }
    }

    // ========================================================================
    // Profile Management
    // ========================================================================

    #[test]
    fn insert_or_replace_profile_creates_new() {
        let mut state = ChatState::new();
        let profile = Profile::new();
        state.insert_or_replace_profile("npub1alice", profile);

        assert!(
            state.get_profile("npub1alice").is_some(),
            "newly inserted profile should be retrievable"
        );
        assert_eq!(state.profiles.len(), 1, "should have exactly one profile");
    }

    #[test]
    fn insert_or_replace_profile_updates_existing() {
        let mut state = ChatState::new();
        let mut p1 = Profile::new();
        p1.name = "Alice".to_string().into_boxed_str();
        state.insert_or_replace_profile("npub1alice", p1);

        let mut p2 = Profile::new();
        p2.name = "Alice Updated".to_string().into_boxed_str();
        state.insert_or_replace_profile("npub1alice", p2);

        let fetched = state.get_profile("npub1alice").expect("profile should exist");
        assert_eq!(
            &*fetched.name, "Alice Updated",
            "profile name should be updated after replace"
        );
        assert_eq!(state.profiles.len(), 1, "should still be one profile, not duplicated");
    }

    #[test]
    fn get_profile_by_npub() {
        let mut state = ChatState::new();
        let mut profile = Profile::new();
        profile.name = "Bob".to_string().into_boxed_str();
        state.insert_or_replace_profile("npub1bob", profile);

        let fetched = state.get_profile("npub1bob").expect("profile should be found");
        assert_eq!(&*fetched.name, "Bob", "fetched profile name should match");
    }

    #[test]
    fn get_profile_returns_none_for_unknown() {
        let state = ChatState::new();
        assert!(
            state.get_profile("npub1unknown").is_none(),
            "unknown npub should return None"
        );
    }

    #[test]
    fn get_profile_by_id_works() {
        let mut state = ChatState::new();
        let mut profile = Profile::new();
        profile.name = "Charlie".to_string().into_boxed_str();
        state.insert_or_replace_profile("npub1charlie", profile);

        let id = state.interner.lookup("npub1charlie").expect("npub should be interned");
        let fetched = state.get_profile_by_id(id).expect("profile should be found by id");
        assert_eq!(&*fetched.name, "Charlie", "profile looked up by id should match");
    }

    #[test]
    fn get_profile_by_id_returns_none_for_invalid() {
        let state = ChatState::new();
        assert!(
            state.get_profile_by_id(9999).is_none(),
            "invalid interner id should return None"
        );
    }

    #[test]
    fn merge_db_profiles_sets_mine_flag() {
        let mut state = ChatState::new();
        let slim_mine = make_slim_profile("npub1me", "Me");
        let slim_other = make_slim_profile("npub1other", "Other");

        state.merge_db_profiles(vec![slim_mine, slim_other], "npub1me");

        let me = state.get_profile("npub1me").expect("my profile should exist");
        assert!(me.flags.is_mine(), "my profile should have mine flag set");

        let other = state.get_profile("npub1other").expect("other profile should exist");
        assert!(!other.flags.is_mine(), "other profile should not have mine flag");
    }

    #[test]
    fn serialize_profile_roundtrip() {
        let mut state = ChatState::new();
        let mut profile = Profile::new();
        profile.name = "Roundtrip".to_string().into_boxed_str();
        profile.about = "Test about".to_string().into_boxed_str();
        profile.flags.set_blocked(true);
        state.insert_or_replace_profile("npub1round", profile);

        let id = state.interner.lookup("npub1round").unwrap();
        let slim = state.serialize_profile(id).expect("serialization should succeed");

        assert_eq!(slim.id, "npub1round", "serialized id should match");
        assert_eq!(slim.name, "Roundtrip", "serialized name should match");
        assert_eq!(slim.about, "Test about", "serialized about should match");
        assert!(slim.is_blocked, "serialized blocked flag should be true");

        // Convert back to profile and re-insert
        let restored = slim.to_profile();
        assert_eq!(&*restored.name, "Roundtrip", "restored name should match");
        assert!(restored.flags.is_blocked(), "restored blocked flag should be true");
    }

    #[test]
    fn binary_search_maintains_sorted_order_with_100_profiles() {
        let mut state = ChatState::new();

        // Insert 100 profiles in random-ish order
        let npubs: Vec<String> = (0..100).map(|i| format!("npub1user{:04}", i)).collect();
        let mut shuffled = npubs.clone();
        // Simple deterministic shuffle
        for i in (1..shuffled.len()).rev() {
            let j = (i * 37 + 13) % (i + 1);
            shuffled.swap(i, j);
        }

        for npub in &shuffled {
            let mut profile = Profile::new();
            profile.name = npub.clone().into_boxed_str();
            state.insert_or_replace_profile(npub, profile);
        }

        // All should be findable
        for npub in &npubs {
            assert!(
                state.get_profile(npub).is_some(),
                "profile {} should be retrievable after bulk insert",
                npub
            );
        }

        // Internal profiles vec should be sorted by id
        for window in state.profiles.windows(2) {
            assert!(
                window[0].id < window[1].id,
                "profiles should be sorted by interner id"
            );
        }

        assert_eq!(state.profiles.len(), 100, "should have exactly 100 profiles");
    }

    #[test]
    fn insert_same_npub_twice_updates_not_duplicates() {
        let mut state = ChatState::new();

        for i in 0..5 {
            let mut profile = Profile::new();
            profile.name = format!("version_{}", i).into_boxed_str();
            state.insert_or_replace_profile("npub1repeated", profile);
        }

        assert_eq!(state.profiles.len(), 1, "repeated inserts should not create duplicates");
        let p = state.get_profile("npub1repeated").unwrap();
        assert_eq!(&*p.name, "version_4", "should retain the last update");
    }

    #[test]
    fn get_profile_mut_modifies_in_place() {
        let mut state = ChatState::new();
        let profile = Profile::new();
        state.insert_or_replace_profile("npub1mutable", profile);

        let p = state.get_profile_mut("npub1mutable").expect("profile should exist");
        p.name = "Mutated".to_string().into_boxed_str();

        let fetched = state.get_profile("npub1mutable").unwrap();
        assert_eq!(&*fetched.name, "Mutated", "mutation should persist");
    }

    // ========================================================================
    // Chat Management
    // ========================================================================

    #[test]
    fn create_dm_chat_creates_new() {
        let mut state = ChatState::new();
        let id = state.create_dm_chat("npub1peer");

        assert_eq!(id, "npub1peer", "returned id should match the npub");
        assert!(state.get_chat("npub1peer").is_some(), "chat should be created");
        assert_eq!(state.chats.len(), 1, "should have exactly one chat");
    }

    #[test]
    fn create_dm_chat_is_idempotent() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");
        state.create_dm_chat("npub1peer");
        state.create_dm_chat("npub1peer");

        assert_eq!(state.chats.len(), 1, "repeated creates should not duplicate");
    }

    #[test]
    fn create_or_get_mls_group_chat_works() {
        let mut state = ChatState::new();
        let id = state.create_or_get_mls_group_chat(
            "group123",
            vec!["npub1a".to_string(), "npub1b".to_string()],
        );

        assert_eq!(id, "group123", "returned id should match group_id");
        let chat = state.get_chat("group123").expect("group chat should exist");
        assert!(chat.is_mls_group(), "should be an MLS group chat");
        assert_eq!(chat.participants.len(), 2, "should have two participants");
    }

    #[test]
    fn create_or_get_mls_group_chat_idempotent() {
        let mut state = ChatState::new();
        state.create_or_get_mls_group_chat("grp1", vec!["npub1a".to_string()]);
        state.create_or_get_mls_group_chat("grp1", vec!["npub1a".to_string(), "npub1b".to_string()]);

        assert_eq!(state.chats.len(), 1, "second call should not create a duplicate");
    }

    #[test]
    fn get_chat_by_id() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1x");

        let chat = state.get_chat("npub1x").expect("chat should exist");
        assert_eq!(chat.id, "npub1x", "chat id should match");
    }

    #[test]
    fn get_chat_returns_none_for_missing() {
        let state = ChatState::new();
        assert!(state.get_chat("nonexistent").is_none(), "missing chat should return None");
    }

    #[test]
    fn get_chat_mut_modifies_in_place() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1editable");

        let chat = state.get_chat_mut("npub1editable").expect("chat should exist");
        chat.muted = true;

        let refetched = state.get_chat("npub1editable").unwrap();
        assert!(refetched.muted, "muted flag should persist after mutation");
    }

    #[test]
    fn multiple_different_chats() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1alice");
        state.create_dm_chat("npub1bob");
        state.create_or_get_mls_group_chat("grp1", vec!["npub1alice".to_string()]);

        assert_eq!(state.chats.len(), 3, "should have three distinct chats");
    }

    // ========================================================================
    // Message Management
    // ========================================================================

    #[test]
    fn add_message_to_chat_single() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let msg = make_message(1, "hello", 1700000000000, false);
        let added = state.add_message_to_chat("npub1peer", msg);

        assert!(added, "first message should be added successfully");
        let chat = state.get_chat("npub1peer").unwrap();
        assert_eq!(chat.message_count(), 1, "chat should have one message");
    }

    #[test]
    fn add_message_to_chat_dedup_rejects_same_id() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let msg1 = make_message(1, "hello", 1700000000000, false);
        let msg2 = make_message(1, "duplicate", 1700000001000, false);

        let added1 = state.add_message_to_chat("npub1peer", msg1);
        let added2 = state.add_message_to_chat("npub1peer", msg2);

        assert!(added1, "first insert should succeed");
        assert!(!added2, "duplicate ID should be rejected");
        assert_eq!(
            state.get_chat("npub1peer").unwrap().message_count(), 1,
            "should still have only one message"
        );
    }

    #[test]
    fn add_messages_to_chat_batch_works() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let msgs: Vec<Message> = (0..10).map(|i| {
            make_message(i, &format!("msg {}", i), 1700000000000 + i as u64 * 1000, false)
        }).collect();

        let added = state.add_messages_to_chat_batch("npub1peer", msgs);
        assert_eq!(added, 10, "all 10 messages should be added");
        assert_eq!(
            state.get_chat("npub1peer").unwrap().message_count(), 10,
            "chat should have 10 messages"
        );
    }

    #[test]
    fn add_messages_to_chat_batch_dedup() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        // Add first batch
        let msgs1: Vec<Message> = (0..5).map(|i| {
            make_message(i, &format!("msg {}", i), 1700000000000 + i as u64 * 1000, false)
        }).collect();
        state.add_messages_to_chat_batch("npub1peer", msgs1);

        // Add overlapping batch (IDs 3,4 overlap, 5,6,7 are new)
        let msgs2: Vec<Message> = (3..8).map(|i| {
            make_message(i, &format!("msg {}", i), 1700000000000 + i as u64 * 1000, false)
        }).collect();
        let added = state.add_messages_to_chat_batch("npub1peer", msgs2);

        assert_eq!(added, 3, "only 3 new messages should be added (5, 6, 7)");
        assert_eq!(
            state.get_chat("npub1peer").unwrap().message_count(), 8,
            "total should be 8 unique messages"
        );
    }

    #[test]
    fn add_message_to_participant_creates_profile_and_chat() {
        let mut state = ChatState::new();

        let msg = make_message(1, "hi there", 1700000000000, false);
        let added = state.add_message_to_participant("npub1stranger", msg);

        assert!(added, "message should be added");
        assert!(
            state.get_profile("npub1stranger").is_some(),
            "profile should be auto-created for unknown participant"
        );
        assert!(
            state.get_chat("npub1stranger").is_some(),
            "DM chat should be auto-created"
        );
    }

    #[test]
    fn add_message_to_participant_uses_existing_profile() {
        let mut state = ChatState::new();

        // Pre-create profile
        let mut profile = Profile::new();
        profile.name = "Known User".to_string().into_boxed_str();
        state.insert_or_replace_profile("npub1known", profile);

        let msg = make_message(1, "hello", 1700000000000, false);
        state.add_message_to_participant("npub1known", msg);

        // Profile should not be replaced
        let p = state.get_profile("npub1known").unwrap();
        assert_eq!(&*p.name, "Known User", "existing profile should not be overwritten");
    }

    #[test]
    fn find_message_across_chats() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1a");
        state.create_dm_chat("npub1b");

        let msg_a = make_message(1, "in chat a", 1700000000000, false);
        let msg_b = make_message(2, "in chat b", 1700000001000, false);
        let msg_id_b = msg_b.id.clone();

        state.add_message_to_chat("npub1a", msg_a);
        state.add_message_to_chat("npub1b", msg_b);

        let (chat, found_msg) = state.find_message(&msg_id_b).expect("message should be found");
        assert_eq!(chat.id, "npub1b", "should find in correct chat");
        assert_eq!(found_msg.content, "in chat b", "content should match");
    }

    #[test]
    fn find_message_returns_none_for_unknown() {
        let state = ChatState::new();
        assert!(
            state.find_message(&make_hex_id(99)).is_none(),
            "unknown message id should return None"
        );
    }

    #[test]
    fn find_message_empty_id_returns_none() {
        let state = ChatState::new();
        assert!(state.find_message("").is_none(), "empty id should return None");
    }

    #[test]
    fn update_message_mutates_and_returns() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let msg = make_message(1, "original", 1700000000000, false);
        let msg_id = msg.id.clone();
        state.add_message_to_chat("npub1peer", msg);

        let result = state.update_message(&msg_id, |cm| {
            cm.content = "updated content".to_string().into_boxed_str();
        });

        let (chat_id, updated) = result.expect("update should return Some");
        assert_eq!(chat_id, "npub1peer", "should return correct chat id");
        assert_eq!(updated.content, "updated content", "content should be updated");
    }

    #[test]
    fn update_message_returns_none_for_missing() {
        let mut state = ChatState::new();
        let result = state.update_message(&make_hex_id(99), |_cm| {});
        assert!(result.is_none(), "updating nonexistent message should return None");
    }

    #[test]
    fn finalize_pending_message_changes_id() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let mut msg = make_message(1, "pending msg", 1700000000000, true);
        msg.pending = true;
        let pending_id = msg.id.clone();
        state.add_message_to_chat("npub1peer", msg);

        let real_id = make_hex_id(2);
        let result = state.finalize_pending_message("npub1peer", &pending_id, &real_id);

        let (old_id, finalized) = result.expect("finalize should succeed");
        assert_eq!(old_id, pending_id, "should return old pending id");
        assert_eq!(finalized.id, real_id, "message id should now be the real id");
        assert!(!finalized.pending, "message should no longer be pending");

        // Old ID should no longer be findable
        assert!(
            state.find_message(&pending_id).is_none(),
            "pending id should no longer resolve"
        );
        // New ID should be findable
        assert!(
            state.find_message(&real_id).is_some(),
            "real id should now resolve"
        );
    }

    #[test]
    fn remove_message_works() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let msg = make_message(1, "deleteme", 1700000000000, false);
        let msg_id = msg.id.clone();
        state.add_message_to_chat("npub1peer", msg);

        let result = state.remove_message(&msg_id);
        assert!(result.is_some(), "remove should return the removed message");

        let (chat_id, removed) = result.unwrap();
        assert_eq!(chat_id, "npub1peer", "should return correct chat id");
        assert_eq!(removed.content, "deleteme", "content should match");

        assert!(
            state.find_message(&msg_id).is_none(),
            "removed message should no longer be findable"
        );
    }

    #[test]
    fn remove_message_returns_none_for_missing() {
        let mut state = ChatState::new();
        assert!(
            state.remove_message(&make_hex_id(99)).is_none(),
            "removing nonexistent message should return None"
        );
    }

    #[test]
    fn message_exists_check() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let msg = make_message(1, "exists", 1700000000000, false);
        let msg_id = msg.id.clone();
        state.add_message_to_chat("npub1peer", msg);

        assert!(state.message_exists(&msg_id), "added message should exist");
        assert!(!state.message_exists(&make_hex_id(99)), "unknown id should not exist");
        assert!(!state.message_exists(""), "empty id should not exist");
    }

    #[test]
    fn chat_reordering_newest_first_after_message_add() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1old");
        state.create_dm_chat("npub1new");

        // Add an old message to the first chat
        let old_msg = make_message(1, "old", 1700000000000, false);
        state.add_message_to_chat("npub1old", old_msg);

        // Add a newer message to the second chat
        let new_msg = make_message(2, "new", 1700000002000, false);
        state.add_message_to_chat("npub1new", new_msg);

        assert_eq!(
            state.chats[0].id, "npub1new",
            "chat with newest message should be first"
        );
        assert_eq!(
            state.chats[1].id, "npub1old",
            "chat with older message should be second"
        );
    }

    #[test]
    fn batch_add_does_not_reorder_for_old_messages() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1active");
        state.create_dm_chat("npub1history");

        // Give active chat a recent message
        let recent = make_message(1, "recent", 1700000010000, false);
        state.add_message_to_chat("npub1active", recent);

        // Batch-add old messages to history chat (pagination loading)
        let old_msgs: Vec<Message> = (10..15).map(|i| {
            make_message(i, &format!("old {}", i), 1700000000000 + i as u64 * 100, false)
        }).collect();
        state.add_messages_to_chat_batch("npub1history", old_msgs);

        assert_eq!(
            state.chats[0].id, "npub1active",
            "active chat should remain first when batch has only old messages"
        );
    }

    #[test]
    fn stress_test_50_messages_in_5_chats() {
        let mut state = ChatState::new();

        for i in 0..5 {
            state.create_dm_chat(&format!("npub1chat{}", i));
        }

        let mut total_added = 0;
        for i in 0..50u8 {
            let chat_idx = i as usize % 5;
            let chat_id = format!("npub1chat{}", chat_idx);
            let msg = make_message(i, &format!("msg {}", i), 1700000000000 + i as u64 * 1000, i % 3 == 0);
            if state.add_message_to_chat(&chat_id, msg) {
                total_added += 1;
            }
        }

        assert_eq!(total_added, 50, "all 50 unique messages should be added");

        let total_in_chats: usize = state.chats.iter().map(|c| c.message_count()).sum();
        assert_eq!(total_in_chats, 50, "total messages across all chats should be 50");

        // Each chat should have 10 messages
        for i in 0..5 {
            let chat = state.get_chat(&format!("npub1chat{}", i)).unwrap();
            assert_eq!(
                chat.message_count(), 10,
                "chat {} should have 10 messages",
                i
            );
        }

        // All messages should be findable
        for i in 0..50u8 {
            assert!(
                state.message_exists(&make_hex_id(i)),
                "message {} should exist",
                i
            );
        }
    }

    #[test]
    fn add_message_auto_creates_dm_chat() {
        let mut state = ChatState::new();

        // Add message to a chat that doesn't exist yet (npub-style ID)
        let msg = make_message(1, "auto create", 1700000000000, false);
        let added = state.add_message_to_chat("npub1auto", msg);

        assert!(added, "message should be added");
        assert!(state.get_chat("npub1auto").is_some(), "DM chat should be auto-created");
    }

    #[test]
    fn add_message_auto_creates_mls_group_chat() {
        let mut state = ChatState::new();

        // Add message to a non-npub ID (should create MLS group)
        let msg = make_message(1, "group msg", 1700000000000, false);
        let added = state.add_message_to_chat("group_abc123", msg);

        assert!(added, "message should be added");
        let chat = state.get_chat("group_abc123").expect("group chat should be auto-created");
        assert!(chat.is_mls_group(), "auto-created non-npub chat should be MLS group");
    }

    // ========================================================================
    // Unread Count
    // ========================================================================

    #[test]
    fn count_unread_messages_basic() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        for i in 0..5u8 {
            let msg = make_message(i, &format!("msg {}", i), 1700000000000 + i as u64 * 1000, false);
            state.add_message_to_chat("npub1peer", msg);
        }

        assert_eq!(state.count_unread_messages(), 5, "all 5 non-mine messages should be unread");
    }

    #[test]
    fn count_unread_muted_chat_skipped() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1muted");

        let msg = make_message(1, "muted msg", 1700000000000, false);
        state.add_message_to_chat("npub1muted", msg);

        state.get_chat_mut("npub1muted").unwrap().muted = true;

        assert_eq!(state.count_unread_messages(), 0, "muted chat should not count toward unread");
    }

    #[test]
    fn count_unread_blocked_user_skipped() {
        let mut state = ChatState::new();

        let mut profile = Profile::new();
        profile.flags.set_blocked(true);
        state.insert_or_replace_profile("npub1blocked", profile);
        state.create_dm_chat("npub1blocked");

        let msg = make_message(1, "blocked msg", 1700000000000, false);
        state.add_message_to_chat("npub1blocked", msg);

        assert_eq!(state.count_unread_messages(), 0, "blocked user DM should not count");
    }

    #[test]
    fn count_unread_own_messages_break_count() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        // 3 from them, then 1 from me, then 2 from them
        let msg1 = make_message(1, "them 1", 1700000001000, false);
        let msg2 = make_message(2, "them 2", 1700000002000, false);
        let msg3 = make_message(3, "them 3", 1700000003000, false);
        let msg_mine = make_message(4, "me", 1700000004000, true);
        let msg5 = make_message(5, "them 4", 1700000005000, false);
        let msg6 = make_message(6, "them 5", 1700000006000, false);

        for m in [msg1, msg2, msg3, msg_mine, msg5, msg6] {
            state.add_message_to_chat("npub1peer", m);
        }

        // Counting from the end: msg6 (unread), msg5 (unread), then msg_mine breaks
        assert_eq!(
            state.count_unread_messages(), 2,
            "only messages after last 'mine' should count as unread"
        );
    }

    #[test]
    fn count_unread_last_read_marker_breaks_count() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let msg1 = make_message(1, "old", 1700000001000, false);
        let msg2 = make_message(2, "read up to here", 1700000002000, false);
        let msg3 = make_message(3, "new 1", 1700000003000, false);
        let msg4 = make_message(4, "new 2", 1700000004000, false);
        let read_marker_id = msg2.id.clone();

        for m in [msg1, msg2, msg3, msg4] {
            state.add_message_to_chat("npub1peer", m);
        }

        // Set last_read to msg2's ID
        let chat = state.get_chat_mut("npub1peer").unwrap();
        chat.last_read = crate::hex::hex_to_bytes_32(&read_marker_id);

        assert_eq!(
            state.count_unread_messages(), 2,
            "only messages after last_read marker should count"
        );
    }

    #[test]
    fn count_unread_empty_chats_is_zero() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1empty1");
        state.create_dm_chat("npub1empty2");

        assert_eq!(state.count_unread_messages(), 0, "empty chats should have zero unread");
    }

    #[test]
    fn count_unread_blocked_group_member_messages_skipped() {
        let mut state = ChatState::new();

        // Create a blocked profile
        let mut blocked_profile = Profile::new();
        blocked_profile.flags.set_blocked(true);
        state.insert_or_replace_profile("npub1blockedmember", blocked_profile);

        // Create a normal profile
        let normal_profile = Profile::new();
        state.insert_or_replace_profile("npub1normal", normal_profile);

        state.create_or_get_mls_group_chat(
            "grp1",
            vec!["npub1blockedmember".to_string(), "npub1normal".to_string()],
        );

        // Message from blocked member
        let msg_blocked = make_message_from(1, "blocked says hi", 1700000001000, "npub1blockedmember");
        state.add_message_to_chat("grp1", msg_blocked);

        // Message from normal member
        let msg_normal = make_message_from(2, "normal says hi", 1700000002000, "npub1normal");
        state.add_message_to_chat("grp1", msg_normal);

        assert_eq!(
            state.count_unread_messages(), 1,
            "only the non-blocked member's message should count"
        );
    }

    #[test]
    fn count_unread_multiple_chats_summed() {
        let mut state = ChatState::new();

        for i in 0..3 {
            let npub = format!("npub1chat{}", i);
            state.create_dm_chat(&npub);
            for j in 0..3u8 {
                let msg = make_message(
                    i * 10 + j,
                    &format!("msg {}-{}", i, j),
                    1700000000000 + j as u64 * 1000,
                    false,
                );
                state.add_message_to_chat(&npub, msg);
            }
        }

        assert_eq!(
            state.count_unread_messages(), 9,
            "3 chats x 3 unread each = 9 total"
        );
    }

    // ========================================================================
    // Typing Indicators
    // ========================================================================

    #[test]
    fn update_typing_and_get_active_basic() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        // Set a far-future expiry so it's definitely active
        let far_future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 300;

        let active = state.update_typing_and_get_active("npub1peer", "npub1typer", far_future);
        assert_eq!(active.len(), 1, "should have one active typer");
        assert_eq!(active[0], "npub1typer", "typer npub should match");
    }

    #[test]
    fn update_typing_expired_typers_filtered() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        // Expired timestamp (in the past)
        let expired = 1000;
        let active = state.update_typing_and_get_active("npub1peer", "npub1expired", expired);

        assert!(active.is_empty(), "expired typer should be filtered out");
    }

    #[test]
    fn update_typing_multiple_typers() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let far_future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 300;

        state.update_typing_and_get_active("npub1peer", "npub1typer1", far_future);
        let active = state.update_typing_and_get_active("npub1peer", "npub1typer2", far_future);

        assert_eq!(active.len(), 2, "should have two active typers");
    }

    #[test]
    fn update_typing_unknown_chat_returns_empty() {
        let mut state = ChatState::new();
        let far_future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 300;

        let active = state.update_typing_and_get_active("npub1nonexistent", "npub1typer", far_future);
        assert!(active.is_empty(), "unknown chat should return empty typers");
    }

    #[test]
    fn update_typing_refreshes_existing_typer() {
        let mut state = ChatState::new();
        state.create_dm_chat("npub1peer");

        let far_future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 300;

        state.update_typing_and_get_active("npub1peer", "npub1typer", far_future);
        // Update the same typer with a new expiry
        let active = state.update_typing_and_get_active("npub1peer", "npub1typer", far_future + 100);

        assert_eq!(active.len(), 1, "should still have only one typer entry after refresh");
    }

    // ========================================================================
    // WrapperIdCache
    // ========================================================================

    #[test]
    fn wrapper_id_cache_historical_and_pending() {
        let mut cache = WrapperIdCache::new();

        let id1 = [1u8; 32];
        let id2 = [2u8; 32];
        let id3 = [3u8; 32];

        cache.load(vec![id1, id2]);
        cache.insert(id3);

        assert!(cache.contains(&id1), "historical id should be found");
        assert!(cache.contains(&id2), "historical id should be found");
        assert!(cache.contains(&id3), "pending id should be found");
        assert!(!cache.contains(&[4u8; 32]), "unknown id should not be found");
        assert_eq!(cache.len(), 3, "total count should be 3");
    }

    #[test]
    fn wrapper_id_cache_clear() {
        let mut cache = WrapperIdCache::new();
        cache.load(vec![[1u8; 32]]);
        cache.insert([2u8; 32]);

        cache.clear();

        assert_eq!(cache.len(), 0, "cache should be empty after clear");
        assert!(!cache.contains(&[1u8; 32]), "cleared historical should not be found");
        assert!(!cache.contains(&[2u8; 32]), "cleared pending should not be found");
    }
}
