//! Compact message storage with binary IDs and interned strings.
//!
//! This module provides memory-efficient message storage:
//! - `[u8; 32]` for IDs instead of hex strings (saves ~56 bytes per ID)
//! - Interned npubs via `NpubInterner` (each unique npub stored once)
//! - Bitflags for boolean states (1 byte instead of 4+)
//! - Binary search for O(log n) message lookup
//! - Boxed optional fields (replied_to, wrapper_id) to save inline space
//! - Compact timestamp (u32 seconds since 2020 epoch)

use crate::message::{Attachment, EditEntry, ImageMetadata, Reaction};
use crate::net::SiteMetadata;
use crate::simd::{bytes_to_hex_32, hex_to_bytes_32, hex_string_to_bytes};

// ============================================================================
// Pending ID Encoding
// ============================================================================

/// Marker byte for pending IDs (first byte = 0x01)
/// Real event IDs are random SHA256 hashes, so this is safe.
const PENDING_ID_MARKER: u8 = 0x01;

/// Encode an ID string to 32 bytes, handling pending IDs specially.
/// - Pending IDs ("pending-{nanoseconds}") are encoded with marker byte + timestamp
/// - Regular hex IDs are decoded normally
#[inline]
fn encode_message_id(id: &str) -> [u8; 32] {
    if let Some(timestamp_str) = id.strip_prefix("pending-") {
        // Encode pending ID: marker byte + timestamp as u128 (16 bytes)
        let mut bytes = [0u8; 32];
        bytes[0] = PENDING_ID_MARKER;
        if let Ok(timestamp) = timestamp_str.parse::<u128>() {
            bytes[1..17].copy_from_slice(&timestamp.to_le_bytes());
        }
        bytes
    } else {
        hex_to_bytes_32(id)
    }
}

/// Decode 32 bytes back to an ID string, handling pending IDs specially.
#[inline]
fn decode_message_id(bytes: &[u8; 32]) -> String {
    if bytes[0] == PENDING_ID_MARKER {
        // Decode pending ID: extract timestamp from bytes 1-16
        let mut timestamp_bytes = [0u8; 16];
        timestamp_bytes.copy_from_slice(&bytes[1..17]);
        let timestamp = u128::from_le_bytes(timestamp_bytes);
        format!("pending-{}", timestamp)
    } else {
        bytes_to_hex_32(bytes)
    }
}

// ============================================================================
// Compact Timestamp
// ============================================================================

/// Custom epoch: 2020-01-01 00:00:00 UTC (in milliseconds)
/// This allows us to use u32 for timestamps until ~2156
const EPOCH_2020_MS: u64 = 1577836800000;

/// Convert milliseconds timestamp to compact u32 (seconds since 2020)
#[inline]
pub fn timestamp_to_compact(ms: u64) -> u32 {
    ((ms.saturating_sub(EPOCH_2020_MS)) / 1000) as u32
}

/// Convert compact u32 back to milliseconds timestamp
#[inline]
pub fn timestamp_from_compact(compact: u32) -> u64 {
    EPOCH_2020_MS + (compact as u64 * 1000)
}

// ============================================================================
// Message Flags
// ============================================================================

/// Bitflags for message state (1 byte instead of 4+ bytes for separate bools)
///
/// Layout (bits): 0=mine, 1=pending, 2=failed, 3-4=replied_to_has_attachment
/// replied_to_has_attachment: 00=None, 01=Some(false), 10=Some(true)
///
/// Note: No EDITED flag - check `edit_history.is_some()` instead
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MessageFlags(u8);

impl MessageFlags {
    pub const NONE: Self = Self(0);
    pub const MINE: Self = Self(0b00001);
    pub const PENDING: Self = Self(0b00010);
    pub const FAILED: Self = Self(0b00100);
    // Bits 3-4 for replied_to_has_attachment:
    // 00 = None, 01 = Some(false), 10 = Some(true)
    const REPLY_ATTACH_MASK: u8 = 0b11000;
    const REPLY_ATTACH_SHIFT: u8 = 3;

    #[inline]
    pub fn is_mine(self) -> bool {
        self.0 & Self::MINE.0 != 0
    }

    #[inline]
    pub fn is_pending(self) -> bool {
        self.0 & Self::PENDING.0 != 0
    }

    #[inline]
    pub fn is_failed(self) -> bool {
        self.0 & Self::FAILED.0 != 0
    }

    /// Get replied_to_has_attachment as Option<bool>
    /// Returns None (unknown), Some(false), or Some(true)
    #[inline]
    pub fn replied_to_has_attachment(self) -> Option<bool> {
        match (self.0 & Self::REPLY_ATTACH_MASK) >> Self::REPLY_ATTACH_SHIFT {
            0b00 => None,           // Unknown
            0b01 => Some(false),    // No attachment
            0b10 => Some(true),     // Has attachment
            _ => None,              // Invalid, treat as unknown
        }
    }

    #[inline]
    pub fn set_mine(&mut self, value: bool) {
        if value {
            self.0 |= Self::MINE.0;
        } else {
            self.0 &= !Self::MINE.0;
        }
    }

    #[inline]
    pub fn set_pending(&mut self, value: bool) {
        if value {
            self.0 |= Self::PENDING.0;
        } else {
            self.0 &= !Self::PENDING.0;
        }
    }

    #[inline]
    pub fn set_failed(&mut self, value: bool) {
        if value {
            self.0 |= Self::FAILED.0;
        } else {
            self.0 &= !Self::FAILED.0;
        }
    }

    /// Set replied_to_has_attachment from Option<bool>
    #[inline]
    pub fn set_replied_to_has_attachment(&mut self, value: Option<bool>) {
        // Clear existing bits
        self.0 &= !Self::REPLY_ATTACH_MASK;
        // Set new value
        let bits = match value {
            None => 0b00,
            Some(false) => 0b01,
            Some(true) => 0b10,
        };
        self.0 |= bits << Self::REPLY_ATTACH_SHIFT;
    }

    /// Create flags from individual booleans
    #[inline]
    pub fn from_bools(mine: bool, pending: bool, failed: bool) -> Self {
        let mut flags = Self::NONE;
        flags.set_mine(mine);
        flags.set_pending(pending);
        flags.set_failed(failed);
        flags
    }

    /// Create flags from all values including replied_to_has_attachment
    #[inline]
    pub fn from_all(mine: bool, pending: bool, failed: bool, replied_to_has_attachment: Option<bool>) -> Self {
        let mut flags = Self::from_bools(mine, pending, failed);
        flags.set_replied_to_has_attachment(replied_to_has_attachment);
        flags
    }
}

// ============================================================================
// TinyVec - 8-byte thin pointer for small collections
// ============================================================================

use std::alloc::{alloc, dealloc, Layout};
use std::marker::PhantomData;
use std::ptr::NonNull;

/// Ultra-compact vector using a thin pointer (8 bytes on stack).
///
/// Memory layout:
/// - Stack: single pointer (8 bytes) - null for empty
/// - Heap: `[len: u8][items: T...]` - only allocated when non-empty
///
/// Compared to standard types:
/// - `Vec<T>`: 24 bytes (ptr + len + cap)
/// - `Box<[T]>`: 16 bytes (fat pointer)
/// - `TinyVec<T>`: 8 bytes (thin pointer)
///
/// Limitations:
/// - Max 255 items (u8 length)
/// - Immutable after creation (no push/pop - recreate to modify)
/// - Perfect for attachments/reactions which rarely change
pub struct TinyVec<T> {
    /// Null = empty, otherwise points to: [len: u8][items: T...]
    ptr: Option<NonNull<u8>>,
    _marker: PhantomData<T>,
}

impl<T> TinyVec<T> {
    /// Create an empty TinyVec (no allocation)
    #[inline]
    pub const fn new() -> Self {
        Self {
            ptr: None,
            _marker: PhantomData,
        }
    }

    /// Create from a Vec, consuming it
    pub fn from_vec(vec: Vec<T>) -> Self {
        if vec.is_empty() {
            return Self::new();
        }

        let len = vec.len().min(255) as u8;

        // Calculate layout: 1 byte for length + items
        let (layout, items_offset) = Self::layout_for(len as usize);

        unsafe {
            // Allocate
            let ptr = alloc(layout);
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }

            // Write length
            *ptr = len;

            // Move items (no clone!)
            let items_ptr = ptr.add(items_offset) as *mut T;
            for (i, item) in vec.into_iter().take(len as usize).enumerate() {
                std::ptr::write(items_ptr.add(i), item);
            }

            Self {
                ptr: NonNull::new(ptr),
                _marker: PhantomData,
            }
        }
    }

    /// Calculate layout for allocation
    fn layout_for(len: usize) -> (Layout, usize) {
        let header_layout = Layout::new::<u8>();
        let items_layout = Layout::array::<T>(len).unwrap();
        header_layout.extend(items_layout).unwrap()
    }

    /// Number of items
    #[inline]
    pub fn len(&self) -> usize {
        match self.ptr {
            None => 0,
            Some(ptr) => unsafe { *ptr.as_ptr() as usize },
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ptr.is_none()
    }

    /// Get items offset within allocation
    #[inline]
    fn items_offset() -> usize {
        let header_layout = Layout::new::<u8>();
        let items_layout = Layout::new::<T>();
        header_layout.extend(items_layout).map(|(_, offset)| offset).unwrap_or(1)
    }

    /// Get a slice of the items
    #[inline]
    pub fn as_slice(&self) -> &[T] {
        match self.ptr {
            None => &[],
            Some(ptr) => unsafe {
                let base = ptr.as_ptr();
                let len = *base as usize;
                let items_ptr = base.add(Self::items_offset()) as *const T;
                std::slice::from_raw_parts(items_ptr, len)
            },
        }
    }

    /// Get a mutable slice of the items
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        match self.ptr {
            None => &mut [],
            Some(ptr) => unsafe {
                let base = ptr.as_ptr();
                let len = *base as usize;
                let items_ptr = base.add(Self::items_offset()) as *mut T;
                std::slice::from_raw_parts_mut(items_ptr, len)
            },
        }
    }

    /// Iterate over items
    #[inline]
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.as_slice().iter()
    }

    /// Iterate mutably
    #[inline]
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.as_mut_slice().iter_mut()
    }

    /// Convert to Vec (clones items)
    pub fn to_vec(&self) -> Vec<T>
    where
        T: Clone,
    {
        self.as_slice().to_vec()
    }

    /// Get the first item (immutable)
    #[inline]
    pub fn first(&self) -> Option<&T> {
        self.as_slice().first()
    }

    /// Get the last item (immutable)
    #[inline]
    pub fn last(&self) -> Option<&T> {
        self.as_slice().last()
    }

    /// Get the last item (mutable)
    #[inline]
    pub fn last_mut(&mut self) -> Option<&mut T> {
        self.as_mut_slice().last_mut()
    }

    /// Get item by index (immutable)
    #[inline]
    pub fn get(&self, index: usize) -> Option<&T> {
        self.as_slice().get(index)
    }

    /// Get item by index (mutable)
    #[inline]
    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        self.as_mut_slice().get_mut(index)
    }

    /// Push an item (rebuilds the entire allocation - use sparingly!)
    pub fn push(&mut self, item: T)
    where
        T: Clone,
    {
        let mut vec = self.to_vec();
        vec.push(item);
        *self = Self::from_vec(vec);
    }

    /// Retain items matching a predicate (rebuilds the allocation)
    pub fn retain<F>(&mut self, f: F)
    where
        T: Clone,
        F: FnMut(&T) -> bool,
    {
        let mut vec = self.to_vec();
        vec.retain(f);
        *self = Self::from_vec(vec);
    }

    /// Check if any item matches a predicate
    pub fn any<F>(&self, f: F) -> bool
    where
        F: FnMut(&T) -> bool,
    {
        self.as_slice().iter().any(f)
    }
}

// Index trait for direct indexing (msg.attachments[0])
impl<T> std::ops::Index<usize> for TinyVec<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        &self.as_slice()[index]
    }
}

impl<T> std::ops::IndexMut<usize> for TinyVec<T> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.as_mut_slice()[index]
    }
}

// IntoIterator for &TinyVec
impl<'a, T> IntoIterator for &'a TinyVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

// IntoIterator for &mut TinyVec
impl<'a, T> IntoIterator for &'a mut TinyVec<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_mut_slice().iter_mut()
    }
}

impl<T> Default for TinyVec<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone> Clone for TinyVec<T> {
    fn clone(&self) -> Self {
        Self::from_vec(self.to_vec())
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for TinyVec<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.as_slice()).finish()
    }
}

impl<T> Drop for TinyVec<T> {
    fn drop(&mut self) {
        if let Some(ptr) = self.ptr {
            unsafe {
                let base = ptr.as_ptr();
                let len = *base as usize;
                let items_ptr = base.add(Self::items_offset()) as *mut T;

                // Drop each item
                for i in 0..len {
                    std::ptr::drop_in_place(items_ptr.add(i));
                }

                // Deallocate
                let (layout, _) = Self::layout_for(len);
                dealloc(base, layout);
            }
        }
    }
}

// Safety: TinyVec is Send/Sync if T is
unsafe impl<T: Send> Send for TinyVec<T> {}
unsafe impl<T: Sync> Sync for TinyVec<T> {}

// ============================================================================
// Compact Reaction
// ============================================================================

/// Memory-efficient reaction with binary IDs and interned author.
///
/// Compared to the regular `Reaction` struct (~292 bytes with heap):
/// - IDs use `[u8; 32]` instead of hex String (saves ~56 bytes each)
/// - Author uses u16 index into interner (saves ~86 bytes)
/// - Emoji uses Box<str> (saves 8 bytes, supports custom emoji like `:cat_heart_eyes:`)
/// - Total: ~82 bytes vs ~292 bytes (72% savings!)
#[derive(Clone, Debug)]
pub struct CompactReaction {
    /// Reaction event ID as binary
    pub id: [u8; 32],
    /// Message being reacted to (binary event ID)
    pub reference_id: [u8; 32],
    /// Author npub index (interned via NpubInterner)
    pub author_idx: u16,
    /// Emoji string (supports standard emoji and custom like `:cat_heart_eyes:`)
    pub emoji: Box<str>,
}

impl CompactReaction {
    /// Get reaction ID as hex string
    #[inline]
    pub fn id_hex(&self) -> String {
        bytes_to_hex_32(&self.id)
    }

    /// Get reference ID as hex string
    #[inline]
    pub fn reference_id_hex(&self) -> String {
        bytes_to_hex_32(&self.reference_id)
    }

    /// Convert from regular Reaction, interning author
    pub fn from_reaction(reaction: &Reaction, interner: &mut NpubInterner) -> Self {
        Self {
            id: hex_to_bytes_32(&reaction.id),
            reference_id: hex_to_bytes_32(&reaction.reference_id),
            author_idx: interner.intern(&reaction.author_id),
            emoji: reaction.emoji.clone().into_boxed_str(),
        }
    }

    /// Convert from regular Reaction (owned), interning author
    pub fn from_reaction_owned(reaction: Reaction, interner: &mut NpubInterner) -> Self {
        Self {
            id: hex_to_bytes_32(&reaction.id),
            reference_id: hex_to_bytes_32(&reaction.reference_id),
            author_idx: interner.intern(&reaction.author_id),
            emoji: reaction.emoji.into_boxed_str(),
        }
    }

    /// Convert back to regular Reaction, resolving author from interner
    pub fn to_reaction(&self, interner: &NpubInterner) -> Reaction {
        Reaction {
            id: self.id_hex(),
            reference_id: self.reference_id_hex(),
            author_id: interner.resolve(self.author_idx)
                .map(|s| s.to_string())
                .unwrap_or_default(),
            emoji: self.emoji.to_string(),
        }
    }
}

// ============================================================================
// Compact Attachment
// ============================================================================

/// Packed flags for attachment state (1 byte instead of multiple bools)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttachmentFlags(u8);

impl AttachmentFlags {
    pub const NONE: Self = Self(0);
    const DOWNLOADING: u8 = 0b0001;
    const DOWNLOADED: u8  = 0b0010;
    const SHORT_NONCE: u8 = 0b0100; // 12-byte nonce (MLS) vs 16-byte (DM)

    #[inline]
    pub fn is_downloading(self) -> bool { self.0 & Self::DOWNLOADING != 0 }
    #[inline]
    pub fn is_downloaded(self) -> bool { self.0 & Self::DOWNLOADED != 0 }
    #[inline]
    pub fn is_short_nonce(self) -> bool { self.0 & Self::SHORT_NONCE != 0 }

    #[inline]
    pub fn set_downloading(&mut self, value: bool) {
        if value { self.0 |= Self::DOWNLOADING; } else { self.0 &= !Self::DOWNLOADING; }
    }
    #[inline]
    pub fn set_downloaded(&mut self, value: bool) {
        if value { self.0 |= Self::DOWNLOADED; } else { self.0 &= !Self::DOWNLOADED; }
    }
    #[inline]
    pub fn set_short_nonce(&mut self, value: bool) {
        if value { self.0 |= Self::SHORT_NONCE; } else { self.0 &= !Self::SHORT_NONCE; }
    }

    pub fn from_bools(downloading: bool, downloaded: bool) -> Self {
        let mut flags = Self::NONE;
        flags.set_downloading(downloading);
        flags.set_downloaded(downloaded);
        flags
    }
}

/// Memory-efficient attachment with binary hashes and compact strings.
///
/// Compared to the regular `Attachment` struct (~320+ bytes):
/// - id (SHA256): `[u8; 32]` instead of hex String (saves ~56 bytes)
/// - key: `[u8; 32]` instead of String (saves ~56 bytes)
/// - nonce: `[u8; 16]` instead of String (saves ~32 bytes)
/// - Bools packed into AttachmentFlags (saves padding)
/// - Strings use Box<str> (saves 8 bytes each)
/// - Rare fields boxed (saves ~100+ bytes when None)
/// - Total: ~120 bytes vs ~320 bytes (62% savings!)
#[derive(Clone, Debug)]
pub struct CompactAttachment {
    // === Fixed binary fields ===
    /// SHA256 file hash as binary (was hex String)
    pub id: [u8; 32],
    /// Encryption key - 32 bytes (empty = MLS derived)
    pub key: [u8; 32],
    /// Encryption nonce - 16 bytes (AES-256-GCM with 0xChat compatibility)
    pub nonce: [u8; 16],
    /// File size in bytes
    pub size: u64,
    /// Packed boolean flags (downloading, downloaded)
    pub flags: AttachmentFlags,

    // === Variable fields (Box<str> = 16 bytes each vs String's 24) ===
    /// File extension (e.g., "png", "mp4")
    pub extension: Box<str>,
    /// Host URL (blossom server, etc.)
    pub url: Box<str>,
    /// Local file path (empty if not downloaded)
    pub path: Box<str>,

    // === Optional fields - boxed to save space when None ===
    /// Image metadata (only for images/videos)
    pub img_meta: Option<Box<ImageMetadata>>,
    /// MLS group ID for key derivation
    pub group_id: Option<Box<[u8; 32]>>,
    /// Original file hash before encryption
    pub original_hash: Option<Box<[u8; 32]>>,
    /// WebXDC topic (Mini Apps only - very rare)
    pub webxdc_topic: Option<Box<str>>,
    /// MLS filename for AAD
    pub mls_filename: Option<Box<str>>,
    /// Scheme version (e.g., "mip04-v1")
    pub scheme_version: Option<Box<str>>,
}

impl CompactAttachment {
    // === Convenience accessors for flags ===
    #[inline]
    pub fn downloaded(&self) -> bool { self.flags.is_downloaded() }
    #[inline]
    pub fn downloading(&self) -> bool { self.flags.is_downloading() }
    #[inline]
    pub fn set_downloaded(&mut self, value: bool) { self.flags.set_downloaded(value); }
    #[inline]
    pub fn set_downloading(&mut self, value: bool) { self.flags.set_downloading(value); }

    /// Check if this attachment's ID matches a hex string
    #[inline]
    pub fn id_eq(&self, hex_id: &str) -> bool {
        self.id == hex_to_bytes_32(hex_id)
    }

    /// Get file ID as hex string
    #[inline]
    pub fn id_hex(&self) -> String {
        bytes_to_hex_32(&self.id)
    }

    /// Get encryption key as hex string (empty if zeros)
    pub fn key_hex(&self) -> String {
        if self.key == [0u8; 32] {
            String::new()
        } else {
            bytes_to_hex_32(&self.key)
        }
    }

    /// Get nonce as hex string (empty if zeros, respects original length)
    pub fn nonce_hex(&self) -> String {
        if self.nonce == [0u8; 16] {
            String::new()
        } else if self.flags.is_short_nonce() {
            // 12-byte nonce (MLS/MIP-04)
            crate::simd::bytes_to_hex_string(&self.nonce[..12])
        } else {
            // 16-byte nonce (DM/0xChat)
            crate::simd::bytes_to_hex_string(&self.nonce)
        }
    }

    /// Convert from regular Attachment (borrowed)
    pub fn from_attachment(att: &Attachment) -> Self {
        // Detect short nonce (12 bytes = 24 hex chars) for MLS attachments
        let is_short_nonce = att.nonce.len() == 24;
        let mut flags = AttachmentFlags::from_bools(att.downloading, att.downloaded);
        flags.set_short_nonce(is_short_nonce);

        Self {
            id: hex_to_bytes_32(&att.id),
            key: if att.key.is_empty() { [0u8; 32] } else { hex_to_bytes_32(&att.key) },
            nonce: if att.nonce.is_empty() { [0u8; 16] } else { parse_nonce(&att.nonce) },
            size: att.size,
            flags,
            extension: att.extension.clone().into_boxed_str(),
            url: att.url.clone().into_boxed_str(),
            path: att.path.clone().into_boxed_str(),
            img_meta: att.img_meta.clone().map(Box::new),
            group_id: att.group_id.as_ref().map(|s| Box::new(hex_to_bytes_32(s))),
            original_hash: att.original_hash.as_ref().map(|s| Box::new(hex_to_bytes_32(s))),
            webxdc_topic: att.webxdc_topic.clone().map(|s| s.into_boxed_str()),
            mls_filename: att.mls_filename.clone().map(|s| s.into_boxed_str()),
            scheme_version: att.scheme_version.clone().map(|s| s.into_boxed_str()),
        }
    }

    /// Convert from regular Attachment (owned) - zero-copy where possible
    pub fn from_attachment_owned(att: Attachment) -> Self {
        // Detect short nonce (12 bytes = 24 hex chars) for MLS attachments
        let is_short_nonce = att.nonce.len() == 24;
        let mut flags = AttachmentFlags::from_bools(att.downloading, att.downloaded);
        flags.set_short_nonce(is_short_nonce);

        Self {
            id: hex_to_bytes_32(&att.id),
            key: if att.key.is_empty() { [0u8; 32] } else { hex_to_bytes_32(&att.key) },
            nonce: if att.nonce.is_empty() { [0u8; 16] } else { parse_nonce(&att.nonce) },
            size: att.size,
            flags,
            extension: att.extension.into_boxed_str(),
            url: att.url.into_boxed_str(),
            path: att.path.into_boxed_str(),
            img_meta: att.img_meta.map(Box::new),
            group_id: att.group_id.map(|s| Box::new(hex_to_bytes_32(&s))),
            original_hash: att.original_hash.map(|s| Box::new(hex_to_bytes_32(&s))),
            webxdc_topic: att.webxdc_topic.map(|s| s.into_boxed_str()),
            mls_filename: att.mls_filename.map(|s| s.into_boxed_str()),
            scheme_version: att.scheme_version.map(|s| s.into_boxed_str()),
        }
    }

    /// Convert back to regular Attachment
    pub fn to_attachment(&self) -> Attachment {
        Attachment {
            id: self.id_hex(),
            key: self.key_hex(),
            nonce: self.nonce_hex(),
            extension: self.extension.to_string(),
            url: self.url.to_string(),
            path: self.path.to_string(),
            size: self.size,
            img_meta: self.img_meta.as_ref().map(|b| (**b).clone()),
            downloading: self.flags.is_downloading(),
            downloaded: self.flags.is_downloaded(),
            webxdc_topic: self.webxdc_topic.as_ref().map(|s| s.to_string()),
            group_id: self.group_id.as_ref().map(|b| bytes_to_hex_32(b)),
            original_hash: self.original_hash.as_ref().map(|b| bytes_to_hex_32(b)),
            scheme_version: self.scheme_version.as_ref().map(|s| s.to_string()),
            mls_filename: self.mls_filename.as_ref().map(|s| s.to_string()),
        }
    }
}

/// Parse a hex nonce string into [u8; 16]
fn parse_nonce(hex: &str) -> [u8; 16] {
    let mut result = [0u8; 16];
    let bytes = hex_string_to_bytes(hex);
    let len = bytes.len().min(16);
    result[..len].copy_from_slice(&bytes[..len]);
    result
}

// ============================================================================
// Npub Interner
// ============================================================================

/// String interner for npubs using sorted Vec + binary search.
///
/// Each unique npub is stored exactly once. Messages reference npubs by u16 index.
/// - `intern()`: O(log n) lookup + O(n) insert for new strings
/// - `resolve()`: O(1) by index
///
/// Memory: ~2 bytes per npub for the sorted index, plus the strings themselves.
#[derive(Clone, Debug, Default)]
pub struct NpubInterner {
    /// npubs in insertion order - index is the stable ID used by messages
    npubs: Vec<String>,
    /// Indices into npubs, sorted alphabetically for binary search
    sorted: Vec<u16>,
}

/// Sentinel value for "no npub" (avoids Option overhead)
pub const NO_NPUB: u16 = u16::MAX;

impl NpubInterner {
    pub fn new() -> Self {
        Self {
            npubs: Vec::new(),
            sorted: Vec::new(),
        }
    }

    /// Pre-allocate capacity for expected number of unique npubs
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            npubs: Vec::with_capacity(capacity),
            sorted: Vec::with_capacity(capacity),
        }
    }

    /// Intern an npub string, returning its stable index.
    ///
    /// If the npub already exists, returns the existing index.
    /// If new, stores it and returns a new index.
    pub fn intern(&mut self, npub: &str) -> u16 {
        // Binary search in sorted order
        let result = self.sorted.binary_search_by(|&idx| {
            self.npubs[idx as usize].as_str().cmp(npub)
        });

        match result {
            Ok(pos) => self.sorted[pos], // Found existing
            Err(insert_pos) => {
                // New npub - add to both vectors
                let new_idx = self.npubs.len() as u16;
                self.npubs.push(npub.to_string());
                self.sorted.insert(insert_pos, new_idx);
                new_idx
            }
        }
    }

    /// Intern an optional npub, returning NO_NPUB sentinel for None.
    #[inline]
    pub fn intern_opt(&mut self, npub: Option<&str>) -> u16 {
        match npub {
            Some(s) if !s.is_empty() => self.intern(s),
            _ => NO_NPUB,
        }
    }

    /// Resolve an index back to the npub string.
    ///
    /// Returns None for NO_NPUB sentinel or out-of-bounds index.
    #[inline]
    pub fn resolve(&self, idx: u16) -> Option<&str> {
        if idx == NO_NPUB {
            return None;
        }
        self.npubs.get(idx as usize).map(|s| s.as_str())
    }

    /// Number of unique npubs stored
    #[inline]
    pub fn len(&self) -> usize {
        self.npubs.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.npubs.is_empty()
    }

    /// Total memory used by the interner (approximate)
    pub fn memory_usage(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.npubs.capacity() * std::mem::size_of::<String>()
            + self.npubs.iter().map(|s| s.capacity()).sum::<usize>()
            + self.sorted.capacity() * std::mem::size_of::<u16>()
    }
}

// ============================================================================
// Compact Message
// ============================================================================

/// Memory-efficient message with binary IDs and interned npubs.
///
/// Compared to the regular `Message` struct:
/// - IDs use `[u8; 32]` instead of hex String (saves ~56 bytes each)
/// - npubs use u16 index into interner (saves ~85 bytes each)
/// - Booleans packed into MessageFlags (saves ~24 bytes + 2 for replied_to_has_attachment)
/// - Boxed optional IDs (replied_to, wrapper_id) save ~40 bytes when None
/// - Compact timestamp (u32 seconds since 2020) saves 4 bytes
/// - TinyVec for attachments/reactions (8 bytes vs 24 = saves 32 bytes)
/// - Box<str> for content (8 bytes vs 24 = saves 16 bytes)
/// - Total savings: ~350+ bytes per message
#[derive(Clone, Debug)]
pub struct CompactMessage {
    /// Message ID as binary (64 hex chars → 32 bytes)
    pub id: [u8; 32],
    /// Compact timestamp: seconds since 2020-01-01 (u32 = good until 2156)
    pub at: u32,
    /// Packed boolean flags (mine, pending, failed, replied_to_has_attachment)
    pub flags: MessageFlags,
    /// Index into NpubInterner for sender's npub (NO_NPUB if none)
    pub npub_idx: u16,
    /// Replied-to message ID (boxed - None for ~70% of messages saves 24 bytes)
    pub replied_to: Option<Box<[u8; 32]>>,
    /// Index into NpubInterner for replied-to author (NO_NPUB if none)
    pub replied_to_npub_idx: u16,
    /// Wrapper event ID for gift-wrapped messages (boxed - saves 25 bytes when None)
    pub wrapper_id: Option<Box<[u8; 32]>>,

    // Variable-length fields - optimized for memory
    /// Message content (Box<str> = 16 bytes vs String's 24 bytes)
    pub content: Box<str>,
    /// Content of replied-to message
    pub replied_to_content: Option<Box<str>>,
    /// File attachments (CompactAttachment = ~120 bytes vs Attachment's ~320 bytes)
    pub attachments: TinyVec<CompactAttachment>,
    /// Emoji reactions (CompactReaction = ~82 bytes vs Reaction's ~292 bytes)
    pub reactions: TinyVec<CompactReaction>,
    /// Edit history - boxed since <1% of messages are edited (saves 16 bytes inline)
    #[allow(clippy::box_collection)]
    pub edit_history: Option<Box<Vec<EditEntry>>>,
    /// Link preview metadata - boxed since ~216 bytes but rare (saves ~208 bytes)
    pub preview_metadata: Option<Box<SiteMetadata>>,
}

impl CompactMessage {
    /// Check if this message has a replied-to reference
    #[inline]
    pub fn has_reply(&self) -> bool {
        self.replied_to.is_some()
    }

    /// Check if this message has been edited
    #[inline]
    pub fn is_edited(&self) -> bool {
        self.edit_history.is_some()
    }

    /// Get the message ID as a string (hex for event IDs, "pending-..." for pending)
    #[inline]
    pub fn id_hex(&self) -> String {
        decode_message_id(&self.id)
    }

    /// Get the replied-to ID as a hex string, or empty if none
    #[inline]
    pub fn replied_to_hex(&self) -> String {
        match &self.replied_to {
            Some(id) => bytes_to_hex_32(id),
            None => String::new(),
        }
    }

    /// Get wrapper ID as hex string if present
    #[inline]
    pub fn wrapper_id_hex(&self) -> Option<String> {
        self.wrapper_id.as_ref().map(|id| bytes_to_hex_32(id))
    }

    /// Get timestamp as milliseconds (for compatibility with frontend)
    #[inline]
    pub fn timestamp_ms(&self) -> u64 {
        timestamp_from_compact(self.at)
    }

    /// Apply an edit to this message
    pub fn apply_edit(&mut self, new_content: String, edited_at: u64) {
        // Initialize edit history with original content if not present
        if self.edit_history.is_none() {
            self.edit_history = Some(Box::new(vec![EditEntry {
                content: self.content.to_string(),
                edited_at: self.timestamp_ms(), // Convert compact to ms
            }]));
        }

        if let Some(ref mut history) = self.edit_history {
            // Deduplicate: skip if we already have this edit
            if history.iter().any(|e| e.edited_at == edited_at) {
                return;
            }

            // Add new edit to history
            history.push(EditEntry {
                content: new_content.clone(),
                edited_at,
            });

            // Sort by timestamp
            history.sort_by_key(|e| e.edited_at);
        }

        // Update current content (convert to Box<str>)
        self.content = new_content.into_boxed_str();
    }

    /// Get replied_to_has_attachment from flags
    #[inline]
    pub fn replied_to_has_attachment(&self) -> Option<bool> {
        self.flags.replied_to_has_attachment()
    }

    /// Add a reaction to this message
    /// Note: Since TinyVec is immutable, this rebuilds the entire reactions list
    pub fn add_reaction(&mut self, reaction: Reaction, interner: &mut NpubInterner) -> bool {
        // Convert to binary ID for comparison
        let reaction_id = hex_to_bytes_32(&reaction.id);

        // Check if already exists
        if self.reactions.iter().any(|r| r.id == reaction_id) {
            return false;
        }

        // Convert to compact and rebuild
        let compact = CompactReaction::from_reaction_owned(reaction, interner);
        let mut reactions = self.reactions.to_vec();
        reactions.push(compact);
        self.reactions = TinyVec::from_vec(reactions);
        true
    }

    // Flag accessors for compatibility
    #[inline]
    pub fn is_mine(&self) -> bool { self.flags.is_mine() }
    #[inline]
    pub fn is_pending(&self) -> bool { self.flags.is_pending() }
    #[inline]
    pub fn is_failed(&self) -> bool { self.flags.is_failed() }

    // Flag setters
    #[inline]
    pub fn set_pending(&mut self, value: bool) { self.flags.set_pending(value); }
    #[inline]
    pub fn set_failed(&mut self, value: bool) { self.flags.set_failed(value); }
    #[inline]
    pub fn set_mine(&mut self, value: bool) { self.flags.set_mine(value); }
}

// ============================================================================
// Compact Message Vec with Binary Search
// ============================================================================

/// Sorted message storage with O(log n) lookup by ID.
///
/// Messages are stored sorted by timestamp. A separate index provides
/// O(log n) lookup by message ID using binary search.
#[derive(Clone, Debug, Default)]
pub struct CompactMessageVec {
    /// Messages sorted by timestamp (ascending)
    messages: Vec<CompactMessage>,
    /// Index for ID lookup: (id, position in messages), sorted by id
    id_index: Vec<([u8; 32], u32)>,
}

impl CompactMessageVec {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            id_index: Vec::new(),
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            messages: Vec::with_capacity(capacity),
            id_index: Vec::with_capacity(capacity),
        }
    }

    /// Number of messages
    #[inline]
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Get all messages (sorted by timestamp)
    #[inline]
    pub fn messages(&self) -> &[CompactMessage] {
        &self.messages
    }

    /// Get a mutable reference to all messages
    #[inline]
    pub fn messages_mut(&mut self) -> &mut Vec<CompactMessage> {
        &mut self.messages
    }

    /// Iterate over messages (supports .rev())
    #[inline]
    pub fn iter(&self) -> std::slice::Iter<'_, CompactMessage> {
        self.messages.iter()
    }

    /// Iterate over messages mutably
    #[inline]
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, CompactMessage> {
        self.messages.iter_mut()
    }

    /// Get the last message
    #[inline]
    pub fn last(&self) -> Option<&CompactMessage> {
        self.messages.last()
    }

    /// Get last message timestamp (in milliseconds)
    #[inline]
    pub fn last_timestamp(&self) -> Option<u64> {
        self.messages.last().map(|m| timestamp_from_compact(m.at))
    }

    /// Get the first message
    #[inline]
    pub fn first(&self) -> Option<&CompactMessage> {
        self.messages.first()
    }

    /// Find a message by ID using binary search - O(log n)
    pub fn find_by_id(&self, id: &[u8; 32]) -> Option<&CompactMessage> {
        let pos = self.id_index
            .binary_search_by(|(idx_id, _)| idx_id.cmp(id))
            .ok()?;
        let msg_pos = self.id_index[pos].1 as usize;
        self.messages.get(msg_pos)
    }

    /// Find a message by ID (mutable) - O(log n)
    pub fn find_by_id_mut(&mut self, id: &[u8; 32]) -> Option<&mut CompactMessage> {
        let pos = self.id_index
            .binary_search_by(|(idx_id, _)| idx_id.cmp(id))
            .ok()?;
        let msg_pos = self.id_index[pos].1 as usize;
        self.messages.get_mut(msg_pos)
    }

    /// Find a message by ID string (hex or pending) - O(log n)
    pub fn find_by_hex_id(&self, id_str: &str) -> Option<&CompactMessage> {
        if id_str.is_empty() {
            return None;
        }
        let id = encode_message_id(id_str);
        self.find_by_id(&id)
    }

    /// Find a message by ID string (mutable) - O(log n)
    pub fn find_by_hex_id_mut(&mut self, id_str: &str) -> Option<&mut CompactMessage> {
        if id_str.is_empty() {
            return None;
        }
        let id = encode_message_id(id_str);
        self.find_by_id_mut(&id)
    }

    /// Check if a message with the given ID exists - O(log n)
    pub fn contains_id(&self, id: &[u8; 32]) -> bool {
        self.id_index
            .binary_search_by(|(idx_id, _)| idx_id.cmp(id))
            .is_ok()
    }

    /// Check if a message with the given ID string exists - O(log n)
    pub fn contains_hex_id(&self, id_str: &str) -> bool {
        if id_str.is_empty() {
            return false;
        }
        let id = encode_message_id(id_str);
        self.contains_id(&id)
    }

    /// Insert a message, maintaining sort order by timestamp.
    ///
    /// Returns true if the message was added, false if duplicate ID.
    ///
    /// **Performance**: O(log n) for append (common case), O(n) for out-of-order insert.
    pub fn insert(&mut self, msg: CompactMessage) -> bool {
        // Check for duplicate ID - O(log n)
        if self.contains_id(&msg.id) {
            return false;
        }

        let msg_id = msg.id;

        // Fast path: append if message is newer than or equal to last (common case)
        // This is O(log n) for the index insert only
        if self.messages.last().is_none_or(|last| msg.at >= last.at) {
            let msg_pos = self.messages.len() as u32;
            self.messages.push(msg);

            // Insert into id_index (maintain sorted order by ID) - O(log n) search + O(n) shift
            // But the shift is typically small since IDs are random/sequential
            let idx_pos = self.id_index
                .binary_search_by(|(id, _)| id.cmp(&msg_id))
                .unwrap_err();
            self.id_index.insert(idx_pos, (msg_id, msg_pos));

            return true;
        }

        // Slow path: out-of-order insert (rare for real-time chat)
        // Find insertion position by timestamp
        let msg_pos = match self.messages.binary_search_by(|m| m.at.cmp(&msg.at)) {
            Ok(pos) => pos,
            Err(pos) => pos,
        };

        // Update id_index positions for messages that will shift - O(n)
        for (_, pos) in &mut self.id_index {
            if *pos >= msg_pos as u32 {
                *pos += 1;
            }
        }

        // Insert into messages - O(n)
        self.messages.insert(msg_pos, msg);

        // Insert into id_index - O(n)
        let idx_pos = self.id_index
            .binary_search_by(|(id, _)| id.cmp(&msg_id))
            .unwrap_err();
        self.id_index.insert(idx_pos, (msg_id, msg_pos as u32));

        true
    }

    /// Rebuild the ID index (call after bulk modifications)
    pub fn rebuild_index(&mut self) {
        self.id_index.clear();
        self.id_index.reserve(self.messages.len());
        for (pos, msg) in self.messages.iter().enumerate() {
            self.id_index.push((msg.id, pos as u32));
        }
        self.id_index.sort_by(|(a, _), (b, _)| a.cmp(b));
    }

    /// Batch insert messages - optimized for different scenarios.
    ///
    /// Returns the number of messages actually added (excludes duplicates).
    ///
    /// **Performance**:
    /// - Append case (newer msgs): O(k log n) where k = new messages
    /// - Prepend case (older msgs): O(k log n + k)
    /// - Mixed: O(n log n) full sort
    pub fn insert_batch(&mut self, messages: impl IntoIterator<Item = CompactMessage>) -> usize {
        let messages: Vec<_> = messages.into_iter().collect();
        if messages.is_empty() {
            return 0;
        }

        // Quick dedup check using the index
        let mut to_add: Vec<CompactMessage> = Vec::with_capacity(messages.len());
        for msg in messages {
            if !self.contains_id(&msg.id) {
                to_add.push(msg);
            }
        }

        if to_add.is_empty() {
            return 0;
        }

        let added = to_add.len();

        // Determine the insertion strategy based on timestamps
        let our_first = self.messages.first().map(|m| m.at);
        let our_last = self.messages.last().map(|m| m.at);
        let their_min = to_add.iter().map(|m| m.at).min().unwrap();
        let their_max = to_add.iter().map(|m| m.at).max().unwrap();

        if self.messages.is_empty() {
            // Empty vec - just add and sort
            self.messages = to_add;
            self.messages.sort_by_key(|m| m.at);
            self.rebuild_index();
        } else if their_min >= our_last.unwrap() {
            // All new messages are NEWER - append path (common for real-time)
            to_add.sort_by_key(|m| m.at);
            let base_pos = self.messages.len() as u32;
            for (i, msg) in to_add.into_iter().enumerate() {
                let msg_id = msg.id;
                self.messages.push(msg);
                // Insert into index
                let idx_pos = self.id_index
                    .binary_search_by(|(id, _)| id.cmp(&msg_id))
                    .unwrap_err();
                self.id_index.insert(idx_pos, (msg_id, base_pos + i as u32));
            }
        } else if their_max <= our_first.unwrap() {
            // All new messages are OLDER - prepend path (common for pagination)
            to_add.sort_by_key(|m| m.at);
            let prepend_count = to_add.len();

            // Shift all existing index positions
            for (_, pos) in &mut self.id_index {
                *pos += prepend_count as u32;
            }

            // Build new index entries (already sorted by construction since to_add is sorted by timestamp)
            let mut new_index_entries: Vec<_> = to_add.iter()
                .enumerate()
                .map(|(i, msg)| (msg.id, i as u32))
                .collect();
            new_index_entries.sort_by(|(a, _), (b, _)| a.cmp(b));

            // Merge sorted index entries in O(n + k) instead of O(k * n)
            let old_index = std::mem::take(&mut self.id_index);
            self.id_index.reserve(old_index.len() + new_index_entries.len());

            let mut old_iter = old_index.into_iter().peekable();
            let mut new_iter = new_index_entries.into_iter().peekable();

            while old_iter.peek().is_some() || new_iter.peek().is_some() {
                match (old_iter.peek(), new_iter.peek()) {
                    (Some((old_id, _)), Some((new_id, _))) => {
                        if old_id < new_id {
                            self.id_index.push(old_iter.next().unwrap());
                        } else {
                            self.id_index.push(new_iter.next().unwrap());
                        }
                    }
                    (Some(_), None) => self.id_index.push(old_iter.next().unwrap()),
                    (None, Some(_)) => self.id_index.push(new_iter.next().unwrap()),
                    (None, None) => break,
                }
            }

            // Prepend messages
            let mut new_messages = to_add;
            new_messages.append(&mut self.messages);
            self.messages = new_messages;
        } else {
            // Mixed timestamps - fall back to full sort
            self.messages.extend(to_add);
            self.messages.sort_by_key(|m| m.at);
            self.rebuild_index();
        }

        added
    }

    /// Total memory used (approximate)
    pub fn memory_usage(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.messages.capacity() * std::mem::size_of::<CompactMessage>()
            + self.id_index.capacity() * std::mem::size_of::<([u8; 32], u32)>()
            // Note: doesn't include heap allocations inside CompactMessage
    }

    /// Drain messages from a range (rebuilds index after)
    pub fn drain(&mut self, range: std::ops::Range<usize>) -> std::vec::Drain<'_, CompactMessage> {
        let drain = self.messages.drain(range);
        // Note: caller should call rebuild_index() after consuming the drain
        drain
    }

    /// Sort messages by a key (rebuilds index after)
    pub fn sort_by_key<K, F>(&mut self, f: F)
    where
        F: FnMut(&CompactMessage) -> K,
        K: Ord,
    {
        self.messages.sort_by_key(f);
        self.rebuild_index();
    }

    /// Clear all messages
    pub fn clear(&mut self) {
        self.messages.clear();
        self.id_index.clear();
    }
}

// ============================================================================
// Conversion from/to Message
// ============================================================================

use crate::Message;

impl CompactMessage {
    /// Convert from a regular Message (borrowed), interning npubs
    pub fn from_message(msg: &Message, interner: &mut NpubInterner) -> Self {
        Self {
            id: encode_message_id(&msg.id),
            at: timestamp_to_compact(msg.at),
            flags: MessageFlags::from_all(msg.mine, msg.pending, msg.failed, msg.replied_to_has_attachment),
            npub_idx: interner.intern_opt(msg.npub.as_deref()),
            // Box replied_to only when present (saves 24 bytes when None)
            replied_to: if msg.replied_to.is_empty() {
                None
            } else {
                Some(Box::new(hex_to_bytes_32(&msg.replied_to)))
            },
            replied_to_npub_idx: interner.intern_opt(msg.replied_to_npub.as_deref()),
            // Box wrapper_id (saves 25 bytes when None)
            wrapper_id: msg.wrapper_event_id.as_ref().map(|s| Box::new(hex_to_bytes_32(s))),
            // Box<str> for content (saves 8 bytes per field)
            content: msg.content.clone().into_boxed_str(),
            replied_to_content: msg.replied_to_content.as_ref().map(|s| s.clone().into_boxed_str()),
            // Convert attachments to compact format
            attachments: TinyVec::from_vec(
                msg.attachments.iter()
                    .map(CompactAttachment::from_attachment)
                    .collect()
            ),
            // Convert reactions to compact format
            reactions: TinyVec::from_vec(
                msg.reactions.iter()
                    .map(|r| CompactReaction::from_reaction(r, interner))
                    .collect()
            ),
            // Box rare fields to save inline space
            edit_history: msg.edit_history.clone().map(Box::new),
            preview_metadata: msg.preview_metadata.clone().map(Box::new),
        }
    }

    /// Convert from a regular Message (owned) - ZERO-COPY for strings!
    ///
    /// Takes ownership of the Message and moves strings directly.
    /// Use this when you don't need the original Message anymore.
    pub fn from_message_owned(msg: Message, interner: &mut NpubInterner) -> Self {
        Self {
            id: encode_message_id(&msg.id),
            at: timestamp_to_compact(msg.at),
            flags: MessageFlags::from_all(msg.mine, msg.pending, msg.failed, msg.replied_to_has_attachment),
            npub_idx: interner.intern_opt(msg.npub.as_deref()),
            // Box replied_to only when present (saves 24 bytes when None)
            replied_to: if msg.replied_to.is_empty() {
                None
            } else {
                Some(Box::new(hex_to_bytes_32(&msg.replied_to)))
            },
            replied_to_npub_idx: interner.intern_opt(msg.replied_to_npub.as_deref()),
            // Box wrapper_id (saves 25 bytes when None)
            wrapper_id: msg.wrapper_event_id.as_ref().map(|s| Box::new(hex_to_bytes_32(s))),
            // Zero-copy: into_boxed_str() reuses the String's buffer!
            content: msg.content.into_boxed_str(),
            replied_to_content: msg.replied_to_content.map(|s| s.into_boxed_str()),
            // Convert attachments to compact format (zero-copy where possible)
            attachments: TinyVec::from_vec(
                msg.attachments.into_iter()
                    .map(CompactAttachment::from_attachment_owned)
                    .collect()
            ),
            // Convert reactions to compact format (zero-copy for emoji string)
            reactions: TinyVec::from_vec(
                msg.reactions.into_iter()
                    .map(|r| CompactReaction::from_reaction_owned(r, interner))
                    .collect()
            ),
            // Box rare fields to save inline space
            edit_history: msg.edit_history.map(Box::new),
            preview_metadata: msg.preview_metadata.map(Box::new),
        }
    }

    /// Convert back to a regular Message, resolving npubs from interner
    pub fn to_message(&self, interner: &NpubInterner) -> Message {
        Message {
            id: self.id_hex(),
            at: self.timestamp_ms(), // Convert compact back to ms
            mine: self.flags.is_mine(),
            pending: self.flags.is_pending(),
            failed: self.flags.is_failed(),
            edited: self.is_edited(),
            npub: interner.resolve(self.npub_idx).map(|s| s.to_string()),
            replied_to: self.replied_to_hex(),
            replied_to_content: self.replied_to_content.as_ref().map(|s| s.to_string()),
            replied_to_npub: interner.resolve(self.replied_to_npub_idx).map(|s| s.to_string()),
            replied_to_has_attachment: self.flags.replied_to_has_attachment(),
            wrapper_event_id: self.wrapper_id_hex(),
            content: self.content.to_string(),
            // Convert compact attachments back to regular Attachment
            attachments: self.attachments.iter()
                .map(|a| a.to_attachment())
                .collect(),
            // Convert compact reactions back to regular Reaction
            reactions: self.reactions.iter()
                .map(|r| r.to_reaction(interner))
                .collect(),
            // Unbox rare fields
            edit_history: self.edit_history.as_ref().map(|b| (**b).clone()),
            preview_metadata: self.preview_metadata.as_ref().map(|b| (**b).clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_flags() {
        let mut flags = MessageFlags::NONE;
        assert!(!flags.is_mine());
        assert!(!flags.is_pending());
        assert!(!flags.is_failed());

        flags.set_mine(true);
        assert!(flags.is_mine());

        flags.set_pending(true);
        assert!(flags.is_pending());
        assert!(flags.is_mine()); // Still set

        flags.set_mine(false);
        assert!(!flags.is_mine());
        assert!(flags.is_pending()); // Still set
    }

    #[test]
    fn test_npub_interner() {
        let mut interner = NpubInterner::new();

        let idx1 = interner.intern("npub1alice");
        let idx2 = interner.intern("npub1bob");
        let idx3 = interner.intern("npub1alice"); // Duplicate

        assert_eq!(idx1, idx3); // Same string = same index
        assert_ne!(idx1, idx2);

        assert_eq!(interner.resolve(idx1), Some("npub1alice"));
        assert_eq!(interner.resolve(idx2), Some("npub1bob"));
        assert_eq!(interner.resolve(NO_NPUB), None);
    }

    #[test]
    fn test_compact_message_vec_insert_and_find() {
        let mut vec = CompactMessageVec::new();
        let mut interner = NpubInterner::new();

        let msg1 = CompactMessage {
            id: hex_to_bytes_32("0000000000000000000000000000000000000000000000000000000000000001"),
            at: 1000,
            flags: MessageFlags::NONE,
            npub_idx: interner.intern("npub1test"),
            replied_to: None,
            replied_to_npub_idx: NO_NPUB,
            wrapper_id: None,
            content: "First message".to_string().into_boxed_str(),
            replied_to_content: None,
            attachments: TinyVec::new(),
            reactions: TinyVec::new(),
            edit_history: None,
            preview_metadata: None,  // Boxed, but None = 8 bytes
        };

        let msg2 = CompactMessage {
            id: hex_to_bytes_32("0000000000000000000000000000000000000000000000000000000000000002"),
            at: 2000,
            flags: MessageFlags::MINE,
            npub_idx: interner.intern("npub1me"),
            replied_to: None,
            replied_to_npub_idx: NO_NPUB,
            wrapper_id: None,
            content: "Second message".to_string().into_boxed_str(),
            replied_to_content: None,
            attachments: TinyVec::new(),
            reactions: TinyVec::new(),
            edit_history: None,
            preview_metadata: None,  // Boxed, but None = 8 bytes
        };

        assert!(vec.insert(msg1));
        assert!(vec.insert(msg2));
        assert_eq!(vec.len(), 2);

        // Find by ID
        let found = vec.find_by_hex_id("0000000000000000000000000000000000000000000000000000000000000001");
        assert!(found.is_some());
        assert_eq!(&*found.unwrap().content, "First message");

        // Find non-existent
        let not_found = vec.find_by_hex_id("0000000000000000000000000000000000000000000000000000000000000099");
        assert!(not_found.is_none());
    }

    #[test]
    fn test_duplicate_insert_rejected() {
        let mut vec = CompactMessageVec::new();

        let msg = CompactMessage {
            id: hex_to_bytes_32("abcd000000000000000000000000000000000000000000000000000000000000"),
            at: 1000,
            flags: MessageFlags::NONE,
            npub_idx: NO_NPUB,
            replied_to: None,
            replied_to_npub_idx: NO_NPUB,
            wrapper_id: None,
            content: "Test".to_string().into_boxed_str(),
            replied_to_content: None,
            attachments: TinyVec::new(),
            reactions: TinyVec::new(),
            edit_history: None,
            preview_metadata: None,  // Boxed
        };

        assert!(vec.insert(msg.clone()));
        assert!(!vec.insert(msg)); // Duplicate rejected
        assert_eq!(vec.len(), 1);
    }

    /// Comprehensive benchmark test for memory reduction and performance
    #[test]
    fn benchmark_compact_vs_message() {
        use std::time::Instant;

        const NUM_MESSAGES: usize = 10_000;
        const NUM_UNIQUE_USERS: usize = 50; // Realistic chat scenario

        println!("\n========================================");
        println!("  COMPACT MESSAGE BENCHMARK");
        println!("  {} messages, {} unique users", NUM_MESSAGES, NUM_UNIQUE_USERS);
        println!("========================================\n");

        // Generate test data
        let users: Vec<String> = (0..NUM_UNIQUE_USERS)
            .map(|i| format!("npub1{:0>62}", i))
            .collect();

        // Create regular Messages
        let messages: Vec<Message> = (0..NUM_MESSAGES)
            .map(|i| {
                let user_idx = i % NUM_UNIQUE_USERS;
                Message {
                    id: format!("{:0>64x}", i),
                    at: 1700000000000 + (i as u64 * 1000),
                    mine: user_idx == 0,
                    pending: false,
                    failed: false,
                    edited: false,
                    npub: Some(users[user_idx].clone()),
                    replied_to: if i > 0 && i % 5 == 0 {
                        format!("{:0>64x}", i - 1)
                    } else {
                        String::new()
                    },
                    replied_to_content: if i > 0 && i % 5 == 0 {
                        Some("Previous message content".to_string())
                    } else {
                        None
                    },
                    replied_to_npub: if i > 0 && i % 5 == 0 {
                        Some(users[(i - 1) % NUM_UNIQUE_USERS].clone())
                    } else {
                        None
                    },
                    replied_to_has_attachment: None,
                    wrapper_event_id: Some(format!("{:0>64x}", i + 1000000)),
                    content: format!("This is message number {} with some typical content length.", i),
                    attachments: vec![],
                    reactions: vec![],
                    edit_history: None,
                    preview_metadata: None,
                }
            })
            .collect();

        // ===== MEMORY COMPARISON =====
        println!("--- STRUCT SIZES ---");
        println!("  Message struct:        {} bytes", std::mem::size_of::<Message>());
        println!("  CompactMessage struct: {} bytes", std::mem::size_of::<CompactMessage>());
        println!("  Savings per struct:    {} bytes ({:.1}%)",
            std::mem::size_of::<Message>().saturating_sub(std::mem::size_of::<CompactMessage>()),
            (1.0 - std::mem::size_of::<CompactMessage>() as f64 / std::mem::size_of::<Message>() as f64) * 100.0
        );
        println!();

        // Measure Message storage (simulating Vec<Message>)
        let msg_heap_estimate: usize = messages.iter().map(|m| {
            m.id.capacity()
                + m.npub.as_ref().map(|s| s.capacity()).unwrap_or(0)
                + m.replied_to.capacity()
                + m.replied_to_content.as_ref().map(|s| s.capacity()).unwrap_or(0)
                + m.replied_to_npub.as_ref().map(|s| s.capacity()).unwrap_or(0)
                + m.wrapper_event_id.as_ref().map(|s| s.capacity()).unwrap_or(0)
                + m.content.capacity()
        }).sum();
        let msg_total = messages.len() * std::mem::size_of::<Message>() + msg_heap_estimate;

        // ===== CONVERSION + INSERT BENCHMARK =====
        println!("--- INSERT BENCHMARK ---");

        // Test 1: Sequential inserts (simulates real-time message arrival)
        let mut interner = NpubInterner::with_capacity(NUM_UNIQUE_USERS);
        let mut compact_vec = CompactMessageVec::with_capacity(NUM_MESSAGES);

        let insert_start = Instant::now();
        for msg in &messages {
            let compact = CompactMessage::from_message(msg, &mut interner);
            compact_vec.insert(compact);
        }
        let insert_elapsed = insert_start.elapsed();

        println!("  Sequential insert (optimized append path):");
        println!("    {} messages in {:?}", NUM_MESSAGES, insert_elapsed);
        println!("    Rate: {:.0} msgs/sec", NUM_MESSAGES as f64 / insert_elapsed.as_secs_f64());
        println!("    Per message: {:.3} µs ({} ns)",
            insert_elapsed.as_micros() as f64 / NUM_MESSAGES as f64,
            insert_elapsed.as_nanos() / NUM_MESSAGES as u128);
        println!();

        // Test 2: Batch insert (simulates pagination/history loading)
        let mut interner2 = NpubInterner::with_capacity(NUM_UNIQUE_USERS);
        let mut compact_vec2 = CompactMessageVec::with_capacity(NUM_MESSAGES);

        let batch_start = Instant::now();
        let compact_messages: Vec<_> = messages.iter()
            .map(|msg| CompactMessage::from_message(msg, &mut interner2))
            .collect();
        let batch_added = compact_vec2.insert_batch(compact_messages);
        let batch_elapsed = batch_start.elapsed();

        println!("  Batch insert (pagination/history load):");
        println!("    {} messages in {:?}", batch_added, batch_elapsed);
        println!("    Rate: {:.0} msgs/sec", NUM_MESSAGES as f64 / batch_elapsed.as_secs_f64());
        println!("    Per message: {:.3} µs ({} ns)",
            batch_elapsed.as_micros() as f64 / NUM_MESSAGES as f64,
            batch_elapsed.as_nanos() / NUM_MESSAGES as u128);
        println!();

        // ===== COMPACT MEMORY USAGE =====
        println!("--- MEMORY COMPARISON ---");
        let compact_heap_estimate: usize = compact_vec.iter().map(|m| {
            m.content.len()  // Box<str> has no capacity, just len
                + m.replied_to_content.as_ref().map(|s| s.len()).unwrap_or(0)
                + m.attachments.len() * std::mem::size_of::<Attachment>() + if m.attachments.is_empty() { 0 } else { 1 }
                + m.reactions.len() * std::mem::size_of::<Reaction>() + if m.reactions.is_empty() { 0 } else { 1 }
        }).sum();
        let compact_struct_mem = compact_vec.len() * std::mem::size_of::<CompactMessage>();
        let compact_index_mem = compact_vec.len() * std::mem::size_of::<([u8; 32], u32)>();
        let interner_mem = interner.memory_usage();
        let compact_total = compact_struct_mem + compact_heap_estimate + compact_index_mem + interner_mem;

        println!("  Regular Message storage:");
        println!("    Struct memory:     {:>10} bytes", messages.len() * std::mem::size_of::<Message>());
        println!("    Heap (strings):    {:>10} bytes", msg_heap_estimate);
        println!("    TOTAL:             {:>10} bytes ({:.2} MB)", msg_total, msg_total as f64 / 1_000_000.0);
        println!();
        println!("  CompactMessage storage:");
        println!("    Struct memory:     {:>10} bytes", compact_struct_mem);
        println!("    Heap (strings):    {:>10} bytes", compact_heap_estimate);
        println!("    ID index:          {:>10} bytes", compact_index_mem);
        println!("    Interner:          {:>10} bytes ({} unique npubs)", interner_mem, interner.len());
        println!("    TOTAL:             {:>10} bytes ({:.2} MB)", compact_total, compact_total as f64 / 1_000_000.0);
        println!();
        println!("  SAVINGS: {} bytes ({:.1}%)",
            msg_total.saturating_sub(compact_total),
            (1.0 - compact_total as f64 / msg_total as f64) * 100.0
        );
        println!("  Per message: {} → {} bytes (avg)",
            msg_total / NUM_MESSAGES,
            compact_total / NUM_MESSAGES
        );
        println!();

        // ===== LOOKUP BENCHMARK =====
        println!("--- LOOKUP BENCHMARK ---");

        // Generate random lookup IDs (mix of existing and non-existing)
        let lookup_ids: Vec<String> = (0..1000)
            .map(|i| format!("{:0>64x}", i * 10)) // Every 10th message
            .collect();

        // Benchmark binary search lookup (CompactMessageVec)
        let lookup_start = Instant::now();
        let mut found_count = 0;
        for _ in 0..100 { // 100 iterations
            for id in &lookup_ids {
                if compact_vec.find_by_hex_id(id).is_some() {
                    found_count += 1;
                }
            }
        }
        let lookup_elapsed = lookup_start.elapsed();
        let total_lookups = 100 * lookup_ids.len();

        println!("  Binary search (CompactMessageVec):");
        println!("    {} lookups in {:?}", total_lookups, lookup_elapsed);
        println!("    Rate: {:.0} lookups/sec", total_lookups as f64 / lookup_elapsed.as_secs_f64());
        println!("    Per lookup: {:.2} µs", lookup_elapsed.as_micros() as f64 / total_lookups as f64);
        println!("    Found: {} / {}", found_count, total_lookups);
        println!();

        // Benchmark linear search (simulating Vec<Message>)
        let linear_start = Instant::now();
        let mut linear_found = 0;
        for _ in 0..100 {
            for id in &lookup_ids {
                if messages.iter().find(|m| &m.id == id).is_some() {
                    linear_found += 1;
                }
            }
        }
        let linear_elapsed = linear_start.elapsed();

        println!("  Linear search (Vec<Message>):");
        println!("    {} lookups in {:?}", total_lookups, linear_elapsed);
        println!("    Rate: {:.0} lookups/sec", total_lookups as f64 / linear_elapsed.as_secs_f64());
        println!("    Per lookup: {:.2} µs", linear_elapsed.as_micros() as f64 / total_lookups as f64);
        println!();

        let speedup = linear_elapsed.as_nanos() as f64 / lookup_elapsed.as_nanos() as f64;
        println!("  SPEEDUP: {:.1}x faster with binary search!", speedup);
        println!();

        // ===== INTERNER EFFICIENCY =====
        println!("--- INTERNER EFFICIENCY ---");
        let npub_string_size = 63 + 1; // "npub1" + 58 chars + null
        let naive_npub_mem = NUM_MESSAGES * npub_string_size * 2; // npub + replied_to_npub
        let actual_npub_mem = interner_mem;
        println!("  Naive (every msg stores npubs): {} bytes", naive_npub_mem);
        println!("  Interned ({} unique):           {} bytes", interner.len(), actual_npub_mem);
        println!("  SAVINGS: {} bytes ({:.1}%)",
            naive_npub_mem.saturating_sub(actual_npub_mem),
            (1.0 - actual_npub_mem as f64 / naive_npub_mem as f64) * 100.0
        );
        println!();

        println!("========================================");
        println!("  BENCHMARK COMPLETE");
        println!("========================================\n");

        // Verify correctness
        assert_eq!(compact_vec.len(), NUM_MESSAGES);
        assert_eq!(interner.len(), NUM_UNIQUE_USERS);
        assert_eq!(found_count, linear_found);
    }
}
