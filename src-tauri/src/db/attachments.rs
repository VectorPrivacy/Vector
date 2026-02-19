//! Attachment database operations.
//!
//! This module handles:
//! - [`AttachmentRef`] for file deduplication
//! - [`UltraPackedFileHashIndex`] - memory-efficient sorted index with binary search
//! - Lazy singleton caching via [`lookup_attachment_cached`]
//! - Background cache warming via [`warm_file_hash_cache`]
//! - Paginated message queries
//! - Wrapper event ID tracking for deduplication
//! - Attachment download status updates
//!
//! # Performance
//!
//! The file hash index uses several optimizations:
//! - Binary storage (`[u8; 32]`) instead of hex strings (50% memory savings)
//! - String interning for repeated values (chat IDs, URLs, extensions)
//! - Bitpacked indices in a single `u16`
//! - Sorted `Vec` with binary search instead of `HashMap` (no hash overhead)
//! - Lazy singleton pattern - built once, reused for all lookups
//! - NEON SIMD hex encoding on ARM64 (~1000x faster than `format!`)
//! - LUT fallback on other platforms (~43x faster than `format!`)
//!
//! Typical performance: ~10μs per lookup after initial ~250ms build.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::{Message, Attachment};
use crate::util::{bytes_to_hex_32, bytes_to_hex_string, hex_to_bytes_16, hex_to_bytes_32};
use super::{get_chat_id_by_identifier, get_message_views};

/// Lightweight attachment reference for file deduplication.
/// Contains only the data needed to reuse an existing upload.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AttachmentRef {
    /// The SHA256 hash of the original file (used as ID)
    pub hash: String,
    /// The encrypted file URL on the server
    pub url: String,
    /// The encryption key
    pub key: String,
    /// The encryption nonce
    pub nonce: String,
    /// The file extension
    pub extension: String,
    /// The encrypted file size
    pub size: u64,
}


// ============================================================================
// Ultra-Packed File Hash Index - Sorted Vec + Binary Search + Bitpacking
// ============================================================================

/// Ultra-packed attachment entry optimized for memory efficiency.
///
/// Uses fixed-size byte arrays instead of heap-allocated strings, and bitpacks
/// indices for interned strings. Total size: 92 bytes per entry.
///
/// # Memory Layout
///
/// | Field           | Size    | Description                              |
/// |-----------------|---------|------------------------------------------|
/// | `hash`          | 6 bytes | Truncated file hash (binary search key) |
/// | `url_file_hash` | 32 bytes| Encrypted hash from URL                 |
/// | `key`           | 32 bytes| Encryption key                          |
/// | `nonce`         | 16 bytes| AES-GCM nonce (128-bit for DM compat)   |
/// | `packed_indices`| 2 bytes | Bitpacked: base_url(6)+ext(6)+hash(4)   |
/// | `size`          | 4 bytes | File size (max 4GB)                     |
#[derive(Clone, Debug)]
#[repr(C)] // Ensure predictable memory layout
pub struct UltraPackedEntry {
    /// Truncated original file hash (first 6 bytes) — primary binary search key.
    /// Combined with 4 extra bits in `packed_indices` for 52-bit effective hash.
    pub hash: [u8; 6],
    /// Encrypted file hash extracted from URL (different from original).
    /// Required for URL reconstruction since encryption changes the hash.
    pub url_file_hash: [u8; 32],
    /// Encryption key for decrypting the file.
    pub key: [u8; 32],
    /// Encryption nonce (16 bytes for DM AES-256-GCM, 12 bytes used for MLS).
    pub nonce: [u8; 16],
    /// Bitpacked indices and extra hash bits.
    /// Layout: `[base_url: 6 bits][extension: 6 bits][hash_extra: 4 bits]`
    pub packed_indices: u16,
    /// Encrypted file size in bytes (max 4GB per file).
    pub size: u32,
}

impl UltraPackedEntry {
    /// Pack indices and extra hash bits into a single u16.
    /// Layout: `[base_url: 6 bits][extension: 6 bits][hash_extra: 4 bits]`
    #[inline]
    pub fn pack_indices(base_url: u8, extension: u8, hash_extra: u8) -> u16 {
        ((base_url as u16 & 0x3F) << 10)       // 6 bits, max 63
            | ((extension as u16 & 0x3F) << 4)  // 6 bits, max 63
            | (hash_extra as u16 & 0x0F)         // 4 bits of extra hash entropy
    }

    /// Unpack base_url index
    #[inline]
    pub fn base_url_idx(&self) -> u8 {
        ((self.packed_indices >> 10) & 0x3F) as u8
    }

    /// Unpack extension index
    #[inline]
    pub fn extension_idx(&self) -> u8 {
        ((self.packed_indices >> 4) & 0x3F) as u8
    }

    /// Extract the 4 extra hash bits (bits 48–51 of the original hash)
    #[inline]
    pub fn hash_extra_bits(&self) -> u8 {
        (self.packed_indices & 0x0F) as u8
    }
}

/// Truncate a 32-byte hash to a 6-byte prefix + 4 extra bits (52 bits total).
/// Returns `(prefix, extra)` where `extra` is the high nibble of byte 6.
#[inline]
fn truncate_hash(hash: &[u8; 32]) -> ([u8; 6], u8) {
    let mut t = [0u8; 6];
    t.copy_from_slice(&hash[..6]);
    (t, (hash[6] >> 4) & 0x0F)
}

/// Memory-efficient file hash index using sorted Vec + binary search.
///
/// This index enables O(log n) attachment lookup by file hash without the
/// memory overhead of a HashMap. String interning further reduces memory
/// by deduplicating repeated values like URLs and extensions.
///
/// # Memory Usage
///
/// For 6,800 attachments: ~583 KB total
/// - Entries: 92 bytes × 6,800 = ~610 KB
/// - String tables: ~3 KB (interned, highly deduplicated)
///
/// Compare to naive HashMap<String, AttachmentRef>: ~4.2 MB
///
/// # Lookup Performance
///
/// Binary search: O(log n) = ~13 comparisons for 6,800 entries
/// Typical lookup time: ~10μs
pub struct UltraPackedFileHashIndex {
    /// Interned base URLs (host + API path + uploader). Max 64 unique values (6-bit index).
    pub base_urls: Vec<String>,
    /// Interned file extensions. Max 64 unique values (6-bit index).
    pub extensions: Vec<String>,
    /// Entries sorted by `hash` field for binary search.
    pub entries: Vec<UltraPackedEntry>,
}

impl UltraPackedFileHashIndex {
    /// Build the index from all file attachments in the database.
    ///
    /// Queries all `kind=15` (FILE_ATTACHMENT) events, parses their attachment
    /// metadata, and builds a sorted index for binary search lookup.
    ///
    /// # Performance
    ///
    /// Build time scales linearly with attachment count:
    /// - 6,800 attachments: ~250ms
    ///
    /// # Note
    ///
    /// Prefer using [`lookup_attachment_cached`] which manages a singleton
    /// cache, rather than calling this directly.
    pub async fn build() -> Result<Self, String> {
        use crate::stored_event::event_kind;

        // String interning tables
        let mut base_url_map: HashMap<String, u8> = HashMap::new();
        let mut ext_map: HashMap<String, u8> = HashMap::new();

        let mut base_urls: Vec<String> = Vec::new();
        let mut extensions: Vec<String> = Vec::new();
        let mut entries: Vec<UltraPackedEntry> = Vec::new();

        fn intern_u8(s: &str, map: &mut HashMap<String, u8>, vec: &mut Vec<String>) -> Option<u8> {
            if let Some(&idx) = map.get(s) {
                Some(idx)
            } else {
                if vec.len() >= 63 { return None; } // Table full (6-bit max)
                let idx = vec.len() as u8;
                vec.push(s.to_string());
                map.insert(s.to_string(), idx);
                Some(idx)
            }
        }

        /// Extract the encrypted file hash from a URL
        /// URL format: https://host/api/uploader_hash/encrypted_hash.ext
        fn extract_url_file_hash(url: &str) -> [u8; 32] {
            let without_scheme = url.strip_prefix("https://")
                .or_else(|| url.strip_prefix("http://"))
                .unwrap_or(url);

            // Get the last path segment (filename)
            if let Some(filename) = without_scheme.rsplit('/').next() {
                // Remove extension to get the hash
                let hash_part = filename.split('.').next().unwrap_or(filename);
                return hex_to_bytes_32(hash_part);
            }
            [0u8; 32]
        }

        /// Extract base URL (everything before the encrypted file hash).
        ///
        /// Returns the URL path prefix including host, API path, and uploader hash.
        /// Example: `"host.com/media/uploader123/"` from full URL.
        fn extract_base_url(url: &str) -> String {
            let without_scheme = url.strip_prefix("https://")
                .or_else(|| url.strip_prefix("http://"))
                .unwrap_or(url);

            let parts: Vec<&str> = without_scheme.split('/').collect();
            if parts.len() >= 3 {
                // host/api/uploader/ or host/api/
                let host = parts[0];
                let api = parts[1];
                if parts.len() >= 4 {
                    // Has uploader path segment
                    let uploader = parts[2];
                    format!("{}/{}/{}/", host, api, uploader)
                } else {
                    format!("{}/{}/", host, api)
                }
            } else if parts.len() >= 2 {
                format!("{}/", parts[0])
            } else {
                without_scheme.to_string()
            }
        }

        // Query attachment data (only need tags - no chat_id or message_id needed)
        let attachment_data: Vec<String> = {
            let conn = crate::account_manager::get_db_connection_guard_static()?;
            let mut stmt = conn.prepare(
                "SELECT tags FROM events WHERE kind = ?1"
            ).map_err(|e| format!("Failed to prepare attachment query: {}", e))?;

            let rows = stmt.query_map(rusqlite::params![event_kind::FILE_ATTACHMENT], |row| {
                row.get::<_, String>(0)
            }).map_err(|e| format!("Failed to query attachments: {}", e))?;

            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("Failed to collect attachment rows: {}", e))?
        };

        const EMPTY_FILE_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

        for tags_json in attachment_data {
            let tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();
            let attachments_json = tags.iter()
                .find(|tag| tag.first().map(|s| s.as_str()) == Some("attachments"))
                .and_then(|tag| tag.get(1))
                .map(|s| s.as_str())
                .unwrap_or("[]");

            let parsed: Vec<crate::Attachment> = serde_json::from_str(attachments_json)
                .unwrap_or_default();

            for att in parsed {
                if att.id.is_empty() || att.id == EMPTY_FILE_HASH || att.url.is_empty() {
                    continue;
                }

                // Skip MLS attachments - they use rolling keys and can't be reused for deduplication.
                // MLS attachments have original_hash set (used for key derivation).
                if att.original_hash.is_some() {
                    continue;
                }

                // Extract encrypted file hash from URL (this is different from att.id!)
                let url_file_hash = extract_url_file_hash(&att.url);

                // Extract and intern the base URL (includes host/api/uploader/)
                let base_url = extract_base_url(&att.url);
                let Some(base_url_idx) = intern_u8(&base_url, &mut base_url_map, &mut base_urls) else { continue };
                let Some(extension_idx) = intern_u8(&att.extension, &mut ext_map, &mut extensions) else { continue };

                // Clamp size to u32 max (4GB)
                let size = if att.size > u32::MAX as u64 { u32::MAX } else { att.size as u32 };

                // Parse nonce (16 bytes for DM, 12 bytes for MLS - zero-padded)
                let nonce = hex_to_bytes_16(&att.nonce);

                let full_hash = hex_to_bytes_32(&att.id);
                let (hash_prefix, hash_extra) = truncate_hash(&full_hash);

                entries.push(UltraPackedEntry {
                    hash: hash_prefix,
                    url_file_hash,
                    key: hex_to_bytes_32(&att.key),
                    nonce,
                    packed_indices: UltraPackedEntry::pack_indices(base_url_idx, extension_idx, hash_extra),
                    size,
                });
            }
        }

        // Sort by hash prefix + extra bits for binary search
        entries.sort_unstable_by(|a, b| {
            a.hash.cmp(&b.hash)
                .then_with(|| a.hash_extra_bits().cmp(&b.hash_extra_bits()))
        });

        let index = Self { base_urls, extensions, entries };
        index.log_memory();
        Ok(index)
    }

    /// Find an entry by its original file hash using binary search.
    ///
    /// Returns a reference to the packed entry if found. Use [`get_full`]
    /// if you need the fully reconstructed [`AttachmentRef`].
    ///
    /// # Performance
    ///
    /// O(log n) binary search - ~13 comparisons for 6,800 entries.
    #[inline]
    pub fn get(&self, hash: &[u8; 32]) -> Option<&UltraPackedEntry> {
        let (prefix, extra) = truncate_hash(hash);
        self.entries
            .binary_search_by(|entry| {
                entry.hash.cmp(&prefix)
                    .then_with(|| entry.hash_extra_bits().cmp(&extra))
            })
            .ok()
            .map(|idx| &self.entries[idx])
    }

    /// Find an entry and reconstruct the full [`AttachmentRef`].
    ///
    /// This performs binary search lookup and then reconstructs all string
    /// fields from the interned tables, including the full URL.
    ///
    /// # URL Reconstruction
    ///
    /// The URL is reconstructed as: `https://{base_url}{url_file_hash}.{extension}`
    /// where `base_url` includes the host, API path, and uploader hash.
    pub fn get_full(&self, hash: &[u8; 32]) -> Option<AttachmentRef> {
        self.get(hash).map(|entry| {
            // Use SIMD-accelerated hex conversion (NEON on ARM64, LUT fallback elsewhere)
            let hash_hex = bytes_to_hex_32(hash);
            let url_hash_hex = bytes_to_hex_32(&entry.url_file_hash);
            let ext = self.extensions.get(entry.extension_idx() as usize)
                .map(|s| s.as_str()).unwrap_or("");

            // base_urls now includes the full path up to the filename
            // e.g., "host/api/uploader/" so we just append hash.ext
            let base = self.base_urls.get(entry.base_url_idx() as usize)
                .map(|s| s.as_str()).unwrap_or("");

            let url = format!("https://{}{}.{}", base, url_hash_hex, ext);

            AttachmentRef {
                hash: hash_hex,
                url,
                key: bytes_to_hex_32(&entry.key),
                nonce: bytes_to_hex_string(&entry.nonce),
                extension: ext.to_string(),
                size: entry.size as u64,
            }
        })
    }

    /// Log memory usage statistics (debug builds only)
    #[cfg(debug_assertions)]
    pub fn log_memory(&self) {
        let entry_size = std::mem::size_of::<UltraPackedEntry>();
        let entries_total = self.entries.len() * entry_size;
        let string_tables: usize = self.base_urls.iter().map(|s| s.capacity()).sum::<usize>()
            + self.extensions.iter().map(|s| s.capacity()).sum::<usize>();
        let total_bytes = entries_total + string_tables + 16; // +16 for Vec overhead (2 Vecs)

        println!("[FileHashIndex] {} entries, {:.1} KB ({}B/entry, sorted Vec + binary search)",
            self.entries.len(), total_bytes as f64 / 1024.0, entry_size);
    }

    /// No-op in release builds
    #[cfg(not(debug_assertions))]
    #[inline]
    pub fn log_memory(&self) {}
}

// ============================================================================
// Cached File Hash Index (Lazy Singleton)
// ============================================================================

use std::sync::OnceLock;
use tokio::sync::RwLock;

/// Global cached file hash index - built once, reused for all lookups
static CACHED_FILE_HASH_INDEX: OnceLock<RwLock<Option<UltraPackedFileHashIndex>>> = OnceLock::new();

/// Get or build the cached file hash index (lazy singleton).
///
/// Uses double-checked locking to ensure the index is only built once,
/// even if multiple tasks call this concurrently.
///
/// # Returns
///
/// A reference to the global `RwLock` containing the cached index.
/// The index will be built on first access if not already cached.
pub async fn get_cached_file_hash_index(
) -> Result<&'static RwLock<Option<UltraPackedFileHashIndex>>, String> {
    let lock = CACHED_FILE_HASH_INDEX.get_or_init(|| RwLock::new(None));

    // Fast path: check if already built (read lock)
    {
        let read_guard = lock.read().await;
        if read_guard.is_some() {
            return Ok(lock);
        }
    }

    // Slow path: acquire write lock and double-check before building
    {
        let mut write_guard = lock.write().await;

        // Double-check: another task may have built it while we waited for the write lock
        if write_guard.is_some() {
            return Ok(lock);
        }

        // Build the index (we hold the write lock, so only one task builds)
        let index = UltraPackedFileHashIndex::build().await?;
        *write_guard = Some(index);
    }

    Ok(lock)
}

/// Lookup an attachment by its original file hash using the cached index.
///
/// This is the primary lookup function for attachment deduplication.
/// Uses the lazy singleton cache, building it on first access if needed.
///
/// # Arguments
///
/// * `file_hash` - The SHA256 hash of the original (unencrypted) file
///
/// # Returns
///
/// * `Ok(Some(AttachmentRef))` - Found an existing attachment with this hash
/// * `Ok(None)` - No attachment found with this hash
/// * `Err(String)` - Database or cache error
///
/// # Performance
///
/// * First call: ~250ms (builds index from database)
/// * Subsequent calls: ~10μs (binary search in cached index)
pub async fn lookup_attachment_cached(
    file_hash: &str,
) -> Result<Option<AttachmentRef>, String> {
    let lock = get_cached_file_hash_index().await?;
    let guard = lock.read().await;

    if let Some(index) = guard.as_ref() {
        let hash_bytes = hex_to_bytes_32(file_hash);
        Ok(index.get_full(&hash_bytes))
    } else {
        Ok(None)
    }
}

/// Invalidate the cached index (call when new attachments are added)
#[allow(dead_code)] // Will be used when we add cache invalidation on new uploads
pub async fn invalidate_file_hash_cache() {
    if let Some(lock) = CACHED_FILE_HASH_INDEX.get() {
        let mut guard = lock.write().await;
        *guard = None;
    }
}

/// Check if the file hash cache is already built.
///
/// Non-blocking check using `try_read()`. Returns `false` if the lock
/// is currently held by a writer (cache being built).
pub fn is_file_hash_cache_built() -> bool {
    CACHED_FILE_HASH_INDEX.get()
        .map(|lock| lock.try_read().map(|g| g.is_some()).unwrap_or(false))
        .unwrap_or(false)
}

/// Pre-warm the file hash cache in the background.
///
/// Call this after sync completes to ensure fast lookups when the user
/// sends their first attachment. This function is safe to call multiple
/// times - it will skip if the cache is already built or if there are
/// no attachments in the database.
///
/// # When to call
///
/// - After initial sync completes
/// - After deep rescan completes
///
/// # Behavior
///
/// 1. Checks if cache is already built (skips if so)
/// 2. Checks if any attachments exist in database (skips if none)
/// 3. Builds the cache (~250ms for 6000+ attachments)
pub async fn warm_file_hash_cache() {
    // Skip if already built
    if is_file_hash_cache_built() {
        println!("[FileHashIndex] Cache already built, skipping warm-up");
        return;
    }

    // Check if there are any attachments worth caching (EXISTS is faster than COUNT)
    let has_attachments = {
        if let Ok(conn) = crate::account_manager::get_db_connection_guard_static() {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE kind = 15)",
                [],
                |row| row.get::<_, bool>(0)
            ).unwrap_or(false)
        } else {
            false
        }
    };

    if !has_attachments {
        println!("[FileHashIndex] No attachments found, skipping cache warm-up");
        return;
    }

    // Build the cache
    println!("[FileHashIndex] Warming cache in background...");
    let start = std::time::Instant::now();
    match get_cached_file_hash_index().await {
        Ok(_) => println!("[FileHashIndex] Cache warmed in {:?}", start.elapsed()),
        Err(e) => eprintln!("[FileHashIndex] Cache warm-up failed: {}", e),
    }
}

/// Get paginated messages for a chat (newest first, with offset)
/// This allows loading messages on-demand instead of all at once
///
/// Parameters:
/// - chat_id: The chat identifier (npub for DMs, group_id for groups)
/// - limit: Maximum number of messages to return
/// - offset: Number of messages to skip from the newest
///
/// Returns messages in chronological order (oldest first within the batch)
/// NOTE: This now uses the events table via get_message_views
pub async fn get_chat_messages_paginated(
    chat_id: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<Message>, String> {
    // Get integer chat ID
    let chat_int_id = get_chat_id_by_identifier(chat_id)?;
    // Use the events-based message views
    get_message_views(chat_int_id, limit, offset).await
}

/// Get the total message count for a chat
/// This is useful for the frontend to know how many messages exist without loading them all
pub async fn get_chat_message_count(
    chat_id: &str,
) -> Result<usize, String> {
    let conn = crate::account_manager::get_db_connection_guard_static()?;

    // Get integer chat ID from identifier
    let chat_int_id: i64 = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![chat_id],
        |row| row.get(0)
    ).map_err(|e| format!("Chat not found: {}", e))?;

    // Count message events (kind 9 = MLS chat, kind 14 = DM, kind 15 = file) from events table
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE chat_id = ?1 AND kind IN (9, 14, 15)",
        rusqlite::params![chat_int_id],
        |row| row.get(0)
    ).map_err(|e| format!("Failed to count messages: {}", e))?;



    Ok(count as usize)
}

/// Get messages around a specific message ID
/// Returns messages from (target - context_before) to the most recent
/// This is used for scrolling to old replied-to messages
pub async fn get_messages_around_id(
    chat_id: &str,
    target_message_id: &str,
    context_before: usize,
) -> Result<Vec<Message>, String> {
    let chat_int_id = get_chat_id_by_identifier(chat_id)?;

    // First, find the timestamp of the target message (don't require chat_id match in case of edge cases)
    let target_timestamp: i64 = {
        let conn = crate::account_manager::get_db_connection_guard_static()?;
        // Try to find in the specified chat first
        let ts_result = conn.query_row(
            "SELECT created_at FROM events WHERE id = ?1 AND chat_id = ?2",
            rusqlite::params![target_message_id, chat_int_id],
            |row| row.get(0)
        );

        let ts = match ts_result {
            Ok(t) => t,
            Err(_) => {
                // Message not found in specified chat, try finding it anywhere
                conn.query_row(
                    "SELECT created_at FROM events WHERE id = ?1",
                    rusqlite::params![target_message_id],
                    |row| row.get(0)
                ).map_err(|e| format!("Target message not found in any chat: {}", e))?
            }
        };
    
        ts
    };

    // Count how many messages are older than the target in this chat
    let older_count: i64 = {
        let conn = crate::account_manager::get_db_connection_guard_static()?;
        let count = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE chat_id = ?1 AND kind IN (9, 14, 15) AND created_at < ?2",
            rusqlite::params![chat_int_id, target_timestamp],
            |row| row.get(0)
        ).map_err(|e| format!("Failed to count older messages: {}", e))?;
    
        count
    };

    // Get total message count for this chat
    let total_count = get_chat_message_count(chat_id).await?;

    // Calculate the starting position (from oldest = 0)
    // We want messages from (target - context_before) to the newest
    let start_position = (older_count as usize).saturating_sub(context_before);

    // get_message_views uses ORDER BY created_at DESC, so:
    // - offset 0 = newest message
    // - To get messages from position P to newest with DESC ordering, use offset=0, limit=(total - P)
    let limit = total_count.saturating_sub(start_position);

    // offset = 0 to start from the newest and get all messages back to start_position
    get_message_views(chat_int_id, limit, 0).await
}

/// Check if a message/event exists in the database by its ID
/// This is used to prevent duplicate processing during sync
pub async fn message_exists_in_db(
    message_id: &str,
) -> Result<bool, String> {
    // Try to get a database connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return Ok(false), // No DB, let in-memory check handle it
    };

    // Check in events table (unified storage)
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE id = ?1)",
        rusqlite::params![message_id],
        |row| row.get(0)
    ).map_err(|e| format!("Failed to check event existence: {}", e))?;



    Ok(exists)
}

/// Check if a wrapper (giftwrap) event ID exists in the database
/// This allows skipping the expensive unwrap operation for already-processed events
pub async fn wrapper_event_exists(
    wrapper_event_id: &str,
) -> Result<bool, String> {
    // Try to get a database connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return Ok(false), // No DB, can't check
    };

    // Check in events table (unified storage)
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE wrapper_event_id = ?1)",
        rusqlite::params![wrapper_event_id],
        |row| row.get(0)
    ).map_err(|e| format!("Failed to check wrapper event existence: {}", e))?;



    Ok(exists)
}

/// Update the wrapper event ID for an existing event
/// This is called when we process an event that was previously stored without its wrapper ID
/// Returns: Ok(true) if updated, Ok(false) if event already had a wrapper_id (duplicate giftwrap)
pub async fn update_wrapper_event_id(
    event_id: &str,
    wrapper_event_id: &str,
) -> Result<bool, String> {
    // Try to get the write connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_write_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return Ok(false), // No DB, nothing to update
    };

    // Update in events table (unified storage)
    let rows_updated = conn.execute(
        "UPDATE events SET wrapper_event_id = ?1 WHERE id = ?2 AND (wrapper_event_id IS NULL OR wrapper_event_id = '')",
        rusqlite::params![wrapper_event_id, event_id],
    ).map_err(|e| format!("Failed to update wrapper event ID: {}", e))?;

    // Returns true if backfill succeeded, false if event already has a wrapper_id (duplicate giftwrap)
    Ok(rows_updated > 0)
}

/// Load recent wrapper_event_ids as raw bytes for the hybrid cache
/// This preloads wrapper_ids from the last N days to avoid SQL queries during sync
///
/// Returns Vec<[u8; 32]> for memory-efficient storage (76% less than HashSet<String>)
pub async fn load_recent_wrapper_ids(
    days: u64,
) -> Result<Vec<[u8; 32]>, String> {
    // Try to get a database connection - if it fails, we're not using DB mode
    let conn = match crate::account_manager::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return Ok(Vec::new()), // No DB, return empty vec
    };

    // Calculate timestamp for N days ago (in seconds, matching events.created_at)
    let cutoff_secs = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs())
        .saturating_sub(days * 24 * 60 * 60);

    // Query all wrapper_event_ids from recent events
    let result: Result<Vec<String>, _> = {
        let mut stmt = conn.prepare(
            "SELECT wrapper_event_id FROM events
             WHERE wrapper_event_id IS NOT NULL
             AND wrapper_event_id != ''
             AND created_at >= ?1"
        ).map_err(|e| format!("Failed to prepare wrapper_id query: {}", e))?;

        let rows = stmt.query_map(rusqlite::params![cutoff_secs as i64], |row| {
            row.get::<_, String>(0)
        }).map_err(|e| format!("Failed to query wrapper_ids: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to collect wrapper_ids: {}", e))
    };



    match result {
        Ok(hex_ids) => {
            // Convert hex strings to [u8; 32] using SIMD-accelerated decode
            let mut wrapper_ids = Vec::with_capacity(hex_ids.len());
            for hex in hex_ids {
                if hex.len() == 64 {
                    let bytes = crate::simd::hex::hex_to_bytes_32(&hex);
                    wrapper_ids.push(bytes);
                }
            }
            Ok(wrapper_ids)
        }
        Err(_) => {
            Ok(Vec::new()) // Return empty vec on error, will fall back to DB queries
        }
    }
}

/// Update the downloaded status of an attachment in the database
pub fn update_attachment_downloaded_status(
    _chat_id: &str,  // No longer needed - we query by event ID directly
    msg_id: &str,
    attachment_id: &str,
    downloaded: bool,
    path: &str,
) -> Result<(), String> {
    let conn = crate::account_manager::get_db_connection_guard_static()?;

    // Get the current tags JSON from the events table
    let tags_json: String = conn.query_row(
        "SELECT tags FROM events WHERE id = ?1",
        rusqlite::params![msg_id],
        |row| row.get(0)
    ).map_err(|e| format!("Event not found: {}", e))?;

    // Parse the tags
    let mut tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();

    // Find the "attachments" tag
    let attachments_tag_idx = tags.iter().position(|tag| {
        tag.first().map(|s| s.as_str()) == Some("attachments")
    });

    let attachments_json = attachments_tag_idx
        .and_then(|idx| tags.get(idx))
        .and_then(|tag| tag.get(1))
        .map(|s| s.as_str())
        .unwrap_or("[]");

    // Parse and update the attachment
    let mut attachments: Vec<Attachment> = serde_json::from_str(attachments_json).unwrap_or_default();

    if let Some(att) = attachments.iter_mut().find(|a| a.id == attachment_id) {
        att.downloaded = downloaded;
        att.downloading = false;
        att.path = path.to_string();
    } else {
        return Err("Attachment not found in event".to_string());
    }

    // Serialize the updated attachments back to JSON
    let updated_attachments_json = serde_json::to_string(&attachments)
        .map_err(|e| format!("Failed to serialize attachments: {}", e))?;

    // Update the tags array - either update existing "attachments" tag or add new one
    if let Some(idx) = attachments_tag_idx {
        tags[idx] = vec!["attachments".to_string(), updated_attachments_json];
    } else {
        tags.push(vec!["attachments".to_string(), updated_attachments_json]);
    }

    // Serialize the tags back to JSON
    let updated_tags_json = serde_json::to_string(&tags)
        .map_err(|e| format!("Failed to serialize tags: {}", e))?;

    // Update the event in the database
    conn.execute(
        "UPDATE events SET tags = ?1 WHERE id = ?2",
        rusqlite::params![updated_tags_json, msg_id],
    ).map_err(|e| format!("Failed to update event: {}", e))?;

    Ok(())
}

/// Check all downloaded attachments in the database for missing files.
/// Updates the database directly for any files that no longer exist.
/// Returns (total_checked, missing_count, elapsed_time).
pub async fn check_downloaded_attachments_integrity(
) -> Result<(usize, usize, std::time::Duration), String> {
    let start = std::time::Instant::now();

    // Query all events with file attachments that have downloaded files
    // Using JSON extract to filter only events with downloaded attachments
    let events_with_downloaded: Vec<(String, String)> = {
        let conn = crate::account_manager::get_db_connection_guard_static()?;

        // Query all file attachment events - we'll filter in Rust for downloaded=true
        // This is more reliable than JSON filtering in SQLite
        let mut stmt = conn.prepare(
            "SELECT id, tags FROM events WHERE kind = 15"
        ).map_err(|e| format!("Failed to prepare integrity query: {}", e))?;

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }).map_err(|e| format!("Failed to query attachments: {}", e))?;

        rows.filter_map(|r| r.ok()).collect()
    };

    let mut total_checked = 0;
    let mut missing_count = 0;
    let mut updates: Vec<(String, String)> = Vec::new(); // (event_id, updated_tags_json)

    for (event_id, tags_json) in events_with_downloaded {
        let mut tags: Vec<Vec<String>> = serde_json::from_str(&tags_json).unwrap_or_default();

        let attachments_tag_idx = tags.iter().position(|tag| {
            tag.first().map(|s| s.as_str()) == Some("attachments")
        });

        let Some(idx) = attachments_tag_idx else { continue };
        let Some(attachments_json) = tags.get(idx).and_then(|t| t.get(1)) else { continue };

        let mut attachments: Vec<crate::Attachment> = serde_json::from_str(attachments_json)
            .unwrap_or_default();

        let mut modified = false;
        for att in &mut attachments {
            if att.downloaded && !att.path.is_empty() {
                total_checked += 1;
                if !std::path::Path::new(&att.path).exists() {
                    att.downloaded = false;
                    att.path = String::new();
                    modified = true;
                    missing_count += 1;
                }
            }
        }

        if modified {
            let updated_attachments_json = serde_json::to_string(&attachments)
                .map_err(|e| format!("Failed to serialize: {}", e))?;
            tags[idx] = vec!["attachments".to_string(), updated_attachments_json];
            let updated_tags_json = serde_json::to_string(&tags)
                .map_err(|e| format!("Failed to serialize tags: {}", e))?;
            updates.push((event_id, updated_tags_json));
        }
    }

    // Batch update all modified events
    if !updates.is_empty() {
        let conn = crate::account_manager::get_db_connection_guard_static()?;
        for (event_id, tags_json) in updates {
            conn.execute(
                "UPDATE events SET tags = ?1 WHERE id = ?2",
                rusqlite::params![tags_json, event_id],
            ).ok(); // Ignore individual errors
        }
    }

    let elapsed = start.elapsed();
    println!(
        "[Integrity] Checked {} downloaded attachments in {:?}, {} missing files updated",
        total_checked, elapsed, missing_count
    );

    Ok((total_checked, missing_count, elapsed))
}
