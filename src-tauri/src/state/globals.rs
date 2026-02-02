//! Global static variables used throughout the application.
//!
//! These globals provide shared access to core application state and configuration.

use lazy_static::lazy_static;
use nostr_sdk::prelude::*;
use once_cell::sync::OnceCell;
use tokio::sync::Mutex;
use tauri::AppHandle;
use std::collections::HashSet;

use super::ChatState;

/// Hybrid cache for wrapper event IDs: sorted Vec for historical + HashSet for pending
///
/// Memory efficient (24% of HashSet<String>) with fast lookups:
/// - Historical data: sorted Vec with binary search O(log n)
/// - New inserts during sync: small HashSet O(1)
///
/// Load time is 5x faster than HashSet due to simple sort vs hash table construction.
pub struct WrapperIdCache {
    /// Sorted array of historical wrapper IDs loaded from DB
    historical: Vec<[u8; 32]>,
    /// New wrapper IDs added during sync (not yet in DB)
    pending: HashSet<[u8; 32]>,
}

impl WrapperIdCache {
    pub fn new() -> Self {
        Self {
            historical: Vec::new(),
            pending: HashSet::new(),
        }
    }

    /// Load historical wrapper IDs from database (call once at init)
    pub fn load(&mut self, mut ids: Vec<[u8; 32]>) {
        ids.sort_unstable();
        self.historical = ids;
        self.pending.clear();
    }

    /// Check if a wrapper ID exists in the cache
    #[inline]
    pub fn contains(&self, id: &[u8; 32]) -> bool {
        // Binary search historical first (likely hit for recent events)
        self.historical.binary_search(id).is_ok() || self.pending.contains(id)
    }

    /// Insert a new wrapper ID (goes to pending set)
    #[inline]
    pub fn insert(&mut self, id: [u8; 32]) {
        self.pending.insert(id);
    }

    /// Clear all cached data (call when sync finishes)
    pub fn clear(&mut self) {
        self.historical.clear();
        self.historical.shrink_to_fit();
        self.pending.clear();
        self.pending.shrink_to_fit();
    }

    /// Get number of cached entries
    pub fn len(&self) -> usize {
        self.historical.len() + self.pending.len()
    }

    /// Check if cache is empty
    pub fn is_empty(&self) -> bool {
        self.historical.is_empty() && self.pending.is_empty()
    }
}

impl Default for WrapperIdCache {
    fn default() -> Self {
        Self::new()
    }
}

/// # Trusted Relays
///
/// The 'Trusted Relays' handle events that MAY have a small amount of public-facing metadata attached (i.e: Expiration tags).
///
/// These relays may be used for events like Typing Indicators, Key Exchanges (forward-secrecy setup) and more.
/// Multiple relays provide redundancy for critical operations.
pub static TRUSTED_RELAYS: &[&str] = &[
    "wss://jskitty.cat/nostr",
    "wss://asia.vectorapp.io/nostr",
    "wss://nostr.computingcache.com",
];

/// # Blossom Media Servers
///
/// A list of Blossom servers for file uploads with automatic failover.
/// The system will try each server in order until one succeeds.
pub static BLOSSOM_SERVERS: OnceCell<std::sync::Mutex<Vec<String>>> = OnceCell::new();

/// Initialize default Blossom servers
pub fn init_blossom_servers() -> Vec<String> {
    vec![
        "https://blossom.primal.net".to_string(),
    ]
}

/// Get the list of Blossom servers (internal function)
pub fn get_blossom_servers() -> Vec<String> {
    BLOSSOM_SERVERS
        .get_or_init(|| std::sync::Mutex::new(init_blossom_servers()))
        .lock()
        .unwrap()
        .clone()
}

/// Mnemonic seed for wallet/key derivation
pub static MNEMONIC_SEED: OnceCell<String> = OnceCell::new();

/// Encryption key derived from seed
pub static ENCRYPTION_KEY: OnceCell<[u8; 32]> = OnceCell::new();

/// Global Nostr client instance
pub static NOSTR_CLIENT: OnceCell<Client> = OnceCell::new();

/// Global Tauri app handle for accessing app resources
pub static TAURI_APP: OnceCell<AppHandle> = OnceCell::new();

/// Pending invite acceptance data
#[derive(Clone)]
pub struct PendingInviteAcceptance {
    pub invite_code: String,
    pub inviter_pubkey: PublicKey,
}

/// Static storage for pending invite acceptance
pub static PENDING_INVITE: OnceCell<PendingInviteAcceptance> = OnceCell::new();

lazy_static! {
    /// Track which MLS welcomes we've already sent notifications for (by wrapper_event_id)
    pub static ref NOTIFIED_WELCOMES: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
}

lazy_static! {
    /// TEMPORARY cache of wrapper_event_ids for fast duplicate detection during INIT SYNC ONLY
    /// - Populated at init with recent wrapper_ids (last 30 days) to avoid SQL queries for each historical event
    /// - Only used for historical sync events (is_new = false), NOT for real-time new events
    /// - Cleared when sync finishes to free memory
    ///
    /// Uses hybrid approach: sorted Vec (historical) + HashSet (pending) for 76% memory reduction
    pub static ref WRAPPER_ID_CACHE: Mutex<WrapperIdCache> = Mutex::new(WrapperIdCache::new());
}

lazy_static! {
    /// Global chat state containing profiles, chats, and sync status
    pub static ref STATE: Mutex<ChatState> = Mutex::new(ChatState::new());
}
