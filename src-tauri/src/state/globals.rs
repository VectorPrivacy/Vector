//! Global static variables used throughout the application.
//!
//! These globals provide shared access to core application state and configuration.

use nostr_sdk::prelude::*;
use std::sync::OnceLock;
use tokio::sync::Mutex;
use tauri::AppHandle;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;

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

/// Return only the trusted relay URLs that are currently in the client's relay pool.
///
/// The user may have disabled some default relays. `nostr_sdk`'s `send_event_to` /
/// `fetch_events_from` / `stream_events_from` all require every requested URL to be
/// in the pool — otherwise the entire call fails with `RelayNotFound`. This helper
/// filters to avoid that.
pub async fn active_trusted_relays() -> Vec<&'static str> {
    let Some(client) = NOSTR_CLIENT.get() else {
        return Vec::new();
    };
    let pool_relays = client.relays().await;
    TRUSTED_RELAYS
        .iter()
        .copied()
        .filter(|url| {
            let normalized = url.trim_end_matches('/');
            pool_relays
                .keys()
                .any(|r| r.as_str().trim_end_matches('/') == normalized)
        })
        .collect()
}

/// # Blossom Media Servers
///
/// A list of Blossom servers for file uploads with automatic failover.
/// The system will try each server in order until one succeeds.
pub static BLOSSOM_SERVERS: OnceLock<std::sync::Mutex<Vec<String>>> = OnceLock::new();

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
pub static MNEMONIC_SEED: OnceLock<String> = OnceLock::new();

/// Temporary nsec storage between create_account/login and setup_encryption/skip_encryption.
/// The private key is set here and consumed by encryption setup — it never crosses IPC.
pub static PENDING_NSEC: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Encryption key derived from user's PIN via Argon2.
/// Uses RwLock to allow clearing/updating (for PIN changes, encryption toggle).
pub static ENCRYPTION_KEY: std::sync::RwLock<Option<[u8; 32]>> = std::sync::RwLock::new(None);

/// Cached encryption-enabled flag. Read by every maybe_encrypt/maybe_decrypt call.
/// Default: false (safe — no encryption until explicitly initialized, avoids panic if
/// events arrive before key is set). Set to true by init_encryption_enabled at boot.
/// Updated by: init_encryption_enabled (boot), enable/disable/skip_encryption (runtime).
pub static ENCRYPTION_ENABLED: AtomicBool = AtomicBool::new(false);

/// Read the cached encryption-enabled flag (~1ns atomic load).
#[inline]
pub fn is_encryption_enabled_fast() -> bool {
    ENCRYPTION_ENABLED.load(Ordering::Acquire)
}

/// Update the cached encryption-enabled flag.
#[inline]
pub fn set_encryption_enabled(enabled: bool) {
    ENCRYPTION_ENABLED.store(enabled, Ordering::Release);
}

/// Initialize the encryption-enabled flag from the database at boot.
/// Call once after DB is available (e.g., in login_from_stored_key).
pub fn init_encryption_enabled() {
    let enabled = crate::db::get_sql_setting("encryption_enabled".to_string())
        .ok()
        .flatten()
        .map(|v| v != "false")
        .unwrap_or(true);
    set_encryption_enabled(enabled);
}

/// Global Nostr client instance
pub static NOSTR_CLIENT: OnceLock<Client> = OnceLock::new();

/// Cached signer keys — set once at login, never changes during a session.
/// `Keys` implements `NostrSigner`, so this can be used directly for signing.
pub static MY_KEYS: OnceLock<Keys> = OnceLock::new();

/// Cached public key — set once at login, never changes during a session.
/// Avoids redundant async signer→get_public_key derivations.
pub static MY_PUBLIC_KEY: OnceLock<PublicKey> = OnceLock::new();

/// Global Tauri app handle for accessing app resources
pub static TAURI_APP: OnceLock<AppHandle> = OnceLock::new();

/// Pending invite acceptance data
#[derive(Clone)]
pub struct PendingInviteAcceptance {
    pub invite_code: String,
    pub inviter_pubkey: PublicKey,
}

/// Static storage for pending invite acceptance
pub static PENDING_INVITE: OnceLock<PendingInviteAcceptance> = OnceLock::new();

/// Track which MLS welcomes we've already sent notifications for (by wrapper_event_id)
pub static NOTIFIED_WELCOMES: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// TEMPORARY cache of wrapper_event_ids for fast duplicate detection during INIT SYNC ONLY
/// - Populated at init with recent wrapper_ids (last 30 days) to avoid SQL queries for each historical event
/// - Only used for historical sync events (is_new = false), NOT for real-time new events
/// - Cleared when sync finishes to free memory
///
/// Uses hybrid approach: sorted Vec (historical) + HashSet (pending) for 76% memory reduction
pub static WRAPPER_ID_CACHE: LazyLock<Mutex<WrapperIdCache>> = LazyLock::new(|| Mutex::new(WrapperIdCache::new()));

/// Global chat state containing profiles, chats, and sync status
pub static STATE: LazyLock<Mutex<ChatState>> = LazyLock::new(|| Mutex::new(ChatState::new()));

// ============================================================================
// Processing Gate - Controls event processing during encryption migration
// ============================================================================

/// Gate controlling event processing. When false, events are queued instead of processed.
/// Used during bulk encryption/decryption migrations to ensure atomic state transitions.
pub static PROCESSING_GATE: AtomicBool = AtomicBool::new(true);

/// Queue for events received while the processing gate is closed.
/// Events are drained and processed after migration completes.
pub static PENDING_EVENTS: LazyLock<Mutex<Vec<(Event, bool)>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Check if event processing is allowed (gate is open)
#[inline]
pub fn is_processing_allowed() -> bool {
    PROCESSING_GATE.load(Ordering::Acquire)
}

/// Close the processing gate - events will be queued instead of processed
pub fn close_processing_gate() {
    PROCESSING_GATE.store(false, Ordering::Release);
}

/// Open the processing gate - resume normal event processing
pub fn open_processing_gate() {
    PROCESSING_GATE.store(true, Ordering::Release);
}
