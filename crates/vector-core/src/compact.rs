//! Compact message storage with binary IDs and interned strings.
//!
//! Part of vector-core — the single source of truth for compact message types.
//!
//! This module provides memory-efficient message storage:
//! - `[u8; 32]` for IDs instead of hex strings (saves ~56 bytes per ID)
//! - Interned npubs via `NpubInterner` (each unique npub stored once)
//! - Bitflags for boolean states (1 byte instead of 4+)
//! - Binary search for O(log n) message lookup
//! - Boxed optional fields (replied_to, wrapper_id) to save inline space
//! - Compact timestamp (u32 seconds since 2020 epoch)

use crate::types::{Attachment, EditEntry, ImageMetadata, Reaction, SiteMetadata};
use crate::simd::hex::{bytes_to_hex_32, hex_to_bytes_32};

/// Convert an arbitrary-length byte slice to a lowercase hex string.
fn bytes_to_hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Decode a hex string of up to 32 hex chars into [u8; 16], left-aligned.
/// Pads short inputs with '0' on the right before decoding.
fn hex_to_bytes_16(hex: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let h = hex.as_bytes();
    // Pad to 32 hex chars with '0'
    let mut padded = [b'0'; 32];
    let copy_len = h.len().min(32);
    padded[..copy_len].copy_from_slice(&h[..copy_len]);
    for i in 0..16 {
        let hi = hex_nibble(padded[i * 2]);
        let lo = hex_nibble(padded[i * 2 + 1]);
        out[i] = (hi << 4) | lo;
    }
    out
}

#[inline]
fn hex_nibble(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

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
pub fn encode_message_id(id: &str) -> [u8; 32] {
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
pub fn decode_message_id(bytes: &[u8; 32]) -> String {
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

/// Convert milliseconds timestamp to compact storage.
/// Stores full u64 milliseconds — no precision loss.
#[inline]
pub fn timestamp_to_compact(ms: u64) -> u64 {
    ms
}

/// Convert compact timestamp back to milliseconds.
#[inline]
pub fn timestamp_from_compact(compact: u64) -> u64 {
    compact
}

/// Custom epoch in seconds: 2020-01-01 00:00:00 UTC
const EPOCH_2020_SECS: u64 = 1577836800;

/// Convert Unix seconds timestamp to compact u32 (seconds since 2020).
/// Preserves 0 as sentinel for "never set".
#[inline]
pub fn secs_to_compact(secs: u64) -> u32 {
    if secs == 0 { return 0; }
    secs.saturating_sub(EPOCH_2020_SECS) as u32
}

/// Convert compact u32 back to Unix seconds timestamp.
/// Preserves 0 as sentinel for "never set".
#[inline]
pub fn secs_from_compact(compact: u32) -> u64 {
    if compact == 0 { return 0; }
    EPOCH_2020_SECS + compact as u64
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
    /// Original filename (e.g. "memories.zip"). Empty = fallback to {hash}.{ext}
    pub name: String,
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
            bytes_to_hex_string(&self.nonce[..12])
        } else {
            // 16-byte nonce (DM/0xChat)
            bytes_to_hex_string(&self.nonce)
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
            name: att.name.clone(),
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
            name: att.name,
        }
    }

    /// Convert back to regular Attachment
    pub fn to_attachment(&self) -> Attachment {
        Attachment {
            id: self.id_hex(),
            key: self.key_hex(),
            nonce: self.nonce_hex(),
            extension: self.extension.to_string(),
            name: self.name.clone(),
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

/// Parse a hex nonce string into [u8; 16], left-aligned, zero-allocation.
/// Both DM (32 hex chars) and MLS (24 hex chars) nonces are decoded.
/// Short nonces are right-padded with '0' to reach 32 chars before decode.
#[inline]
fn parse_nonce(hex: &str) -> [u8; 16] {
    hex_to_bytes_16(hex)
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

    /// Look up an npub without inserting. Returns its handle if already interned.
    pub fn lookup(&self, npub: &str) -> Option<u16> {
        self.sorted.binary_search_by(|&idx| {
            self.npubs[idx as usize].as_str().cmp(npub)
        }).ok().map(|pos| self.sorted[pos])
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
    /// Message ID as binary (64 hex chars -> 32 bytes)
    pub id: [u8; 32],
    /// Timestamp in milliseconds (full precision for sub-second ordering)
    pub at: u64,
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

    /// Remove a message by hex ID string. Returns true if removed.
    pub fn remove_by_hex_id(&mut self, id_str: &str) -> bool {
        if id_str.is_empty() {
            return false;
        }
        let id = encode_message_id(id_str);
        // Find position in id_index
        let idx_pos = match self.id_index.binary_search_by(|(idx_id, _)| idx_id.cmp(&id)) {
            Ok(pos) => pos,
            Err(_) => return false,
        };
        let msg_pos = self.id_index[idx_pos].1 as usize;
        // Remove from messages vec
        self.messages.remove(msg_pos);
        // Rebuild index since positions shifted
        self.rebuild_index();
        true
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

use crate::types::Message;

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
        println!("    Per message: {:.3} us ({} ns)",
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
        println!("    Per message: {:.3} us ({} ns)",
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
        println!("  Per message: {} -> {} bytes (avg)",
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
        println!("    Per lookup: {:.2} us", lookup_elapsed.as_micros() as f64 / total_lookups as f64);
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
        println!("    Per lookup: {:.2} us", linear_elapsed.as_micros() as f64 / total_lookups as f64);
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

    /// Benchmark: Profile lookup -- linear scan vs string binary search vs handle binary search
    ///
    /// Compares three approaches for finding a Profile in a Vec:
    /// 1. Linear scan with string equality (old -- O(n) x 63-byte strcmp, Profile.id was String)
    /// 2. Binary search by npub string (intermediate -- O(log n) x 63-byte strcmp)
    /// 3. Direct u16 handle binary search (current -- O(log n) x 2-byte int cmp, Profile.id is u16)
    #[test]
    fn benchmark_profile_lookup() {
        use std::time::Instant;
        use std::hint::black_box;

        const NUM_PROFILES: usize = 60;
        const NUM_LOOKUPS: usize = 100_000;

        println!("\n========================================");
        println!("  PROFILE LOOKUP BENCHMARK");
        println!("  {} profiles, {} lookups each method", NUM_PROFILES, NUM_LOOKUPS);
        println!("========================================\n");

        // Generate realistic npubs (63 chars each: "npub1" + 58 hex-like chars)
        let npubs: Vec<String> = (0..NUM_PROFILES)
            .map(|i| format!("npub1{:0>58}", format!("{:x}", i * 7919 + 1000))) // spread out values
            .collect();

        // --- Setup: Method 1 - Linear scan (old approach, simulating id: String) ---
        let old_ids: Vec<String> = npubs.iter().rev().cloned().collect(); // reversed = worst case

        // --- Setup: Method 2 - String binary search (intermediate approach) ---
        let mut sorted_ids: Vec<String> = npubs.clone();
        sorted_ids.sort();

        // --- Setup: Method 3 - Direct u16 handle lookup (current approach, id: u16) ---
        let mut interner = NpubInterner::new();
        let mut profiles: Vec<crate::profile::Profile> = npubs.iter().map(|npub| {
            let mut p = crate::profile::Profile::new();
            p.id = interner.intern(npub);
            p
        }).collect();
        profiles.sort_by(|a, b| a.id.cmp(&b.id));

        // Build lookup targets: cycle through all profiles
        let lookup_targets: Vec<&str> = (0..NUM_LOOKUPS)
            .map(|i| npubs[i % NUM_PROFILES].as_str())
            .collect();

        // Pre-resolve handles for method 3
        let handle_targets: Vec<u16> = lookup_targets.iter()
            .map(|&npub| interner.lookup(npub).unwrap())
            .collect();

        // ===== BENCHMARK 1: Linear scan (old -- id: String) =====
        let start = Instant::now();
        let mut found = 0u64;
        for &target in &lookup_targets {
            if old_ids.iter().any(|id| id == target) {
                found += 1;
            }
        }
        let linear_elapsed = start.elapsed();
        assert_eq!(found, NUM_LOOKUPS as u64);

        // ===== BENCHMARK 2: String binary search (intermediate) =====
        let start = Instant::now();
        found = 0;
        for &target in &lookup_targets {
            if sorted_ids.binary_search_by(|id| id.as_str().cmp(target)).is_ok() {
                found += 1;
            }
        }
        let string_bs_elapsed = start.elapsed();
        assert_eq!(found, NUM_LOOKUPS as u64);

        // ===== BENCHMARK 3: Direct u16 handle lookup (current -- id: u16) =====
        let start = Instant::now();
        found = 0;
        for &handle in &handle_targets {
            if profiles.binary_search_by(|p| p.id.cmp(black_box(&handle))).is_ok() {
                found += 1;
            }
        }
        let direct_elapsed = start.elapsed();
        assert_eq!(found, NUM_LOOKUPS as u64);

        // ===== RESULTS =====
        println!("--- LOOKUP METHODS ---");
        println!("  1. Linear scan (old id: String):");
        println!("     {:?} total, {:.0} ns/lookup",
            linear_elapsed,
            linear_elapsed.as_nanos() as f64 / NUM_LOOKUPS as f64);
        println!();
        println!("  2. String binary search (intermediate):");
        println!("     {:?} total, {:.0} ns/lookup",
            string_bs_elapsed,
            string_bs_elapsed.as_nanos() as f64 / NUM_LOOKUPS as f64);
        println!("     vs linear: {:.1}x faster",
            linear_elapsed.as_nanos() as f64 / string_bs_elapsed.as_nanos() as f64);
        println!();
        println!("  3. Direct u16 handle lookup (current id: u16):");
        println!("     {:?} total, {:.0} ns/lookup",
            direct_elapsed,
            direct_elapsed.as_nanos() as f64 / NUM_LOOKUPS as f64);
        println!("     vs linear: {:.1}x faster",
            linear_elapsed.as_nanos() as f64 / direct_elapsed.as_nanos() as f64);
        println!("     vs string BS: {:.1}x faster",
            string_bs_elapsed.as_nanos() as f64 / direct_elapsed.as_nanos() as f64);
        println!();

        // ===== MEMORY COMPARISON =====
        println!("--- MEMORY PER PROFILE ---");
        println!("  Old (id: String):    ~87 bytes (24 String header + ~63 heap)");
        println!("  Current (id: u16):     2 bytes (inline)");
        println!("  Savings: ~85 bytes/profile, ~{} bytes for {} profiles",
            85 * NUM_PROFILES, NUM_PROFILES);
        println!("  Interner (shared):   {} bytes (shared with message system)",
            interner.memory_usage());
        println!();

        println!("========================================");
        println!("  BENCHMARK COMPLETE");
        println!("========================================\n");

        // Correctness: ensure all methods find the same profiles
        for npub in &npubs {
            assert!(old_ids.iter().any(|id| id == npub));
            assert!(sorted_ids.binary_search_by(|id| id.as_str().cmp(npub.as_str())).is_ok());
            let h = interner.lookup(npub).unwrap();
            assert!(profiles.binary_search_by(|p| p.id.cmp(&h)).is_ok());
        }
    }

    // ========================================================================
    // Pending ID Encoding Tests
    // ========================================================================

    #[test]
    fn pending_id_roundtrip_regular_hex() {
        let hex = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let encoded = encode_message_id(hex);
        let decoded = decode_message_id(&encoded);
        assert_eq!(decoded, hex, "regular hex ID should roundtrip exactly");
    }

    #[test]
    fn pending_id_roundtrip_pending() {
        let id = "pending-1234567890123456789";
        let encoded = encode_message_id(id);
        let decoded = decode_message_id(&encoded);
        assert_eq!(decoded, id, "pending ID should roundtrip exactly");
    }

    #[test]
    fn pending_id_zero_timestamp() {
        let id = "pending-0";
        let encoded = encode_message_id(id);
        assert_eq!(encoded[0], PENDING_ID_MARKER, "first byte should be marker");
        let decoded = decode_message_id(&encoded);
        assert_eq!(decoded, id, "pending-0 should roundtrip");
    }

    #[test]
    fn pending_id_max_u128_timestamp() {
        let max = u128::MAX;
        let id = format!("pending-{}", max);
        let encoded = encode_message_id(&id);
        let decoded = decode_message_id(&encoded);
        assert_eq!(decoded, id, "pending with max u128 should roundtrip");
    }

    #[test]
    fn pending_id_all_zero_hex() {
        let hex = "0000000000000000000000000000000000000000000000000000000000000000";
        let encoded = encode_message_id(hex);
        let decoded = decode_message_id(&encoded);
        assert_eq!(decoded, hex, "all-zero hex ID should roundtrip");
        // All-zero should NOT be detected as pending since byte 0 is 0x00, not 0x01
        assert_ne!(encoded[0], PENDING_ID_MARKER);
    }

    #[test]
    fn pending_id_all_ff_hex() {
        let hex = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let encoded = encode_message_id(hex);
        let decoded = decode_message_id(&encoded);
        assert_eq!(decoded, hex, "all-ff hex ID should roundtrip");
        assert_eq!(encoded, [0xff; 32], "all-ff should decode to all 0xff bytes");
    }

    #[test]
    fn pending_id_mixed_case_hex() {
        let lower = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
        let mixed = "ABCDEF0123456789abcdef0123456789ABCDEF0123456789abcdef0123456789";
        let encoded_lower = encode_message_id(lower);
        let encoded_mixed = encode_message_id(mixed);
        assert_eq!(encoded_lower, encoded_mixed, "mixed case should produce same bytes as lowercase");
    }

    #[test]
    fn pending_id_short_hex_partial_decode() {
        // SIMD hex_to_bytes_32 partially decodes short input (decodes what it can)
        let short = "abcdef";
        let encoded = encode_message_id(short);
        // Short input is decoded as far as possible, remaining bytes are zero
        assert_eq!(encoded[0..14], [0u8; 14], "leading bytes should be zero for short input");
    }

    #[test]
    fn pending_id_marker_distinguishes_from_real_id() {
        // A real event ID starting with 01 should NOT be confused with pending
        let hex = "0100000000000000000000000000000000000000000000000000000000000000";
        let encoded = encode_message_id(hex);
        // The first byte is 0x01 which matches PENDING_ID_MARKER - but decode_message_id
        // will treat it as pending. This is by design since real SHA256 IDs with first
        // byte 0x01 are extremely rare and the probability is 1/256.
        let decoded = decode_message_id(&encoded);
        // This will decode as pending since byte[0] == 0x01
        assert!(decoded.starts_with("pending-"), "ID starting with 0x01 byte is treated as pending by design");
    }

    #[test]
    fn pending_id_large_timestamp() {
        let id = "pending-99999999999999999";
        let encoded = encode_message_id(&id);
        let decoded = decode_message_id(&encoded);
        assert_eq!(decoded, id, "large but valid timestamp should roundtrip");
    }

    #[test]
    fn pending_id_invalid_timestamp_becomes_zero() {
        // If the timestamp part can't be parsed, it stays as all-zeros in bytes 1..17
        let id = "pending-notanumber";
        let encoded = encode_message_id(id);
        assert_eq!(encoded[0], PENDING_ID_MARKER);
        // Bytes 1..17 should all be 0
        assert_eq!(&encoded[1..17], &[0u8; 16]);
        let decoded = decode_message_id(&encoded);
        assert_eq!(decoded, "pending-0", "invalid timestamp parses as pending-0");
    }

    // ========================================================================
    // Timestamp Tests
    // ========================================================================

    #[test]
    fn timestamp_to_compact_and_back_roundtrip() {
        // A representative timestamp: 2024-01-15 12:00:00 UTC in milliseconds
        let ms: u64 = 1705320000000;
        let compact = timestamp_to_compact(ms);
        let restored = timestamp_from_compact(compact);
        // The sub-second part is lost (ms -> seconds -> ms), so restored is floored to seconds
        assert_eq!(restored / 1000, ms / 1000, "roundtrip should preserve seconds");
    }

    #[test]
    fn secs_to_compact_and_back_roundtrip() {
        let secs: u64 = 1705320000; // 2024-01-15 12:00:00 UTC
        let compact = secs_to_compact(secs);
        let restored = secs_from_compact(compact);
        assert_eq!(restored, secs, "secs should roundtrip exactly");
    }

    #[test]
    fn timestamp_zero_preservation() {
        // Zero is a sentinel for "never set" in secs_to_compact/secs_from_compact
        assert_eq!(secs_to_compact(0), 0, "zero secs should produce zero compact");
        assert_eq!(secs_from_compact(0), 0, "zero compact should produce zero secs");
    }

    #[test]
    fn timestamp_epoch_boundary() {
        // Exactly 2020-01-01 00:00:00 UTC = EPOCH_2020_SECS
        let epoch_secs: u64 = 1577836800;
        let compact = secs_to_compact(epoch_secs);
        assert_eq!(compact, 0, "epoch boundary should map to compact 0");
        let restored = secs_from_compact(compact);
        // secs_from_compact(0) returns 0 (sentinel), not epoch
        assert_eq!(restored, 0, "compact 0 returns sentinel 0");
    }

    #[test]
    fn timestamp_epoch_boundary_ms() {
        let epoch_ms: u64 = 1577836800000;
        let compact = timestamp_to_compact(epoch_ms);
        assert_eq!(compact, epoch_ms, "full u64 — identity function");
        let restored = timestamp_from_compact(compact);
        assert_eq!(restored, epoch_ms, "roundtrip preserves value");
    }

    #[test]
    fn timestamp_current_time_roundtrip() {
        // Simulate a current-ish timestamp: 2026-03-27 in seconds
        let secs: u64 = 1774800000;
        let compact = secs_to_compact(secs);
        let restored = secs_from_compact(compact);
        assert_eq!(restored, secs, "current-era timestamp should roundtrip");
        assert!(compact > 0, "current time should be past epoch");
    }

    #[test]
    fn timestamp_far_future_year_2100() {
        // 2100-01-01 00:00:00 UTC
        let secs: u64 = 4102444800;
        let compact = secs_to_compact(secs);
        let restored = secs_from_compact(compact);
        assert_eq!(restored, secs, "year 2100 should roundtrip");
        // Verify it fits in u32
        assert!(compact <= u32::MAX, "year 2100 should fit in u32");
    }

    #[test]
    fn timestamp_pre_epoch_saturates_to_zero() {
        // A timestamp before 2020 epoch
        let secs: u64 = 1500000000; // ~2017
        let compact = secs_to_compact(secs);
        // saturating_sub means this becomes 0
        assert_eq!(compact, 0, "pre-epoch timestamp should saturate to 0");
    }

    #[test]
    fn timestamp_ms_sub_second_precision_preserved() {
        let ms: u64 = 1705320000999;
        let compact = timestamp_to_compact(ms);
        let restored = timestamp_from_compact(compact);
        assert_eq!(restored, ms, "sub-second ms must be preserved");
    }

    #[test]
    fn timestamp_one_second_after_epoch() {
        let secs: u64 = 1577836801; // one second after epoch
        let compact = secs_to_compact(secs);
        assert_eq!(compact, 1, "one second after epoch should be compact 1");
        let restored = secs_from_compact(compact);
        assert_eq!(restored, secs, "should restore to original");
    }

    // ========================================================================
    // MessageFlags Tests
    // ========================================================================

    #[test]
    fn message_flags_mine_independent() {
        let mut flags = MessageFlags::NONE;
        flags.set_mine(true);
        assert!(flags.is_mine(), "mine should be set");
        assert!(!flags.is_pending(), "pending should not be set");
        assert!(!flags.is_failed(), "failed should not be set");
    }

    #[test]
    fn message_flags_pending_independent() {
        let mut flags = MessageFlags::NONE;
        flags.set_pending(true);
        assert!(!flags.is_mine(), "mine should not be set");
        assert!(flags.is_pending(), "pending should be set");
        assert!(!flags.is_failed(), "failed should not be set");
    }

    #[test]
    fn message_flags_failed_independent() {
        let mut flags = MessageFlags::NONE;
        flags.set_failed(true);
        assert!(!flags.is_mine(), "mine should not be set");
        assert!(!flags.is_pending(), "pending should not be set");
        assert!(flags.is_failed(), "failed should be set");
    }

    #[test]
    fn message_flags_from_bools_all_false() {
        let flags = MessageFlags::from_bools(false, false, false);
        assert!(!flags.is_mine());
        assert!(!flags.is_pending());
        assert!(!flags.is_failed());
        assert_eq!(flags, MessageFlags::NONE, "all false should equal NONE");
    }

    #[test]
    fn message_flags_from_bools_all_true() {
        let flags = MessageFlags::from_bools(true, true, true);
        assert!(flags.is_mine(), "mine should be set");
        assert!(flags.is_pending(), "pending should be set");
        assert!(flags.is_failed(), "failed should be set");
    }

    #[test]
    fn message_flags_from_bools_various_combos() {
        let flags = MessageFlags::from_bools(true, false, true);
        assert!(flags.is_mine());
        assert!(!flags.is_pending());
        assert!(flags.is_failed());

        let flags = MessageFlags::from_bools(false, true, false);
        assert!(!flags.is_mine());
        assert!(flags.is_pending());
        assert!(!flags.is_failed());
    }

    #[test]
    fn message_flags_from_all_replied_to_none() {
        let flags = MessageFlags::from_all(false, false, false, None);
        assert_eq!(flags.replied_to_has_attachment(), None, "None should roundtrip");
    }

    #[test]
    fn message_flags_from_all_replied_to_some_false() {
        let flags = MessageFlags::from_all(false, false, false, Some(false));
        assert_eq!(flags.replied_to_has_attachment(), Some(false), "Some(false) should roundtrip");
    }

    #[test]
    fn message_flags_from_all_replied_to_some_true() {
        let flags = MessageFlags::from_all(false, false, false, Some(true));
        assert_eq!(flags.replied_to_has_attachment(), Some(true), "Some(true) should roundtrip");
    }

    #[test]
    fn message_flags_multiple_set_simultaneously() {
        let flags = MessageFlags::from_all(true, true, false, Some(true));
        assert!(flags.is_mine());
        assert!(flags.is_pending());
        assert!(!flags.is_failed());
        assert_eq!(flags.replied_to_has_attachment(), Some(true));
    }

    #[test]
    fn message_flags_default_is_all_false() {
        let flags = MessageFlags::default();
        assert!(!flags.is_mine());
        assert!(!flags.is_pending());
        assert!(!flags.is_failed());
        assert_eq!(flags.replied_to_has_attachment(), None);
        assert_eq!(flags, MessageFlags::NONE);
    }

    #[test]
    fn message_flags_bit_patterns_correct() {
        assert_eq!(MessageFlags::MINE.0, 0b00001, "MINE bit pattern");
        assert_eq!(MessageFlags::PENDING.0, 0b00010, "PENDING bit pattern");
        assert_eq!(MessageFlags::FAILED.0, 0b00100, "FAILED bit pattern");
    }

    #[test]
    fn message_flags_set_then_clear() {
        let mut flags = MessageFlags::from_bools(true, true, true);
        flags.set_mine(false);
        assert!(!flags.is_mine(), "mine should be cleared");
        assert!(flags.is_pending(), "pending should remain set");
        assert!(flags.is_failed(), "failed should remain set");
    }

    #[test]
    fn message_flags_replied_to_overwrite() {
        let mut flags = MessageFlags::from_all(false, false, false, Some(true));
        assert_eq!(flags.replied_to_has_attachment(), Some(true));
        flags.set_replied_to_has_attachment(Some(false));
        assert_eq!(flags.replied_to_has_attachment(), Some(false), "overwrite should work");
        flags.set_replied_to_has_attachment(None);
        assert_eq!(flags.replied_to_has_attachment(), None, "clearing to None should work");
    }

    #[test]
    fn message_flags_replied_to_does_not_interfere_with_other_bits() {
        let mut flags = MessageFlags::from_bools(true, true, true);
        flags.set_replied_to_has_attachment(Some(true));
        assert!(flags.is_mine(), "mine should still be set");
        assert!(flags.is_pending(), "pending should still be set");
        assert!(flags.is_failed(), "failed should still be set");
        assert_eq!(flags.replied_to_has_attachment(), Some(true));
    }

    // ========================================================================
    // AttachmentFlags Tests
    // ========================================================================

    #[test]
    fn attachment_flags_downloading_independent() {
        let mut flags = AttachmentFlags::NONE;
        flags.set_downloading(true);
        assert!(flags.is_downloading());
        assert!(!flags.is_downloaded());
        assert!(!flags.is_short_nonce());
    }

    #[test]
    fn attachment_flags_downloaded_independent() {
        let mut flags = AttachmentFlags::NONE;
        flags.set_downloaded(true);
        assert!(!flags.is_downloading());
        assert!(flags.is_downloaded());
        assert!(!flags.is_short_nonce());
    }

    #[test]
    fn attachment_flags_short_nonce_independent() {
        let mut flags = AttachmentFlags::NONE;
        flags.set_short_nonce(true);
        assert!(!flags.is_downloading());
        assert!(!flags.is_downloaded());
        assert!(flags.is_short_nonce());
    }

    #[test]
    fn attachment_flags_from_bools() {
        let flags = AttachmentFlags::from_bools(true, false);
        assert!(flags.is_downloading());
        assert!(!flags.is_downloaded());

        let flags = AttachmentFlags::from_bools(false, true);
        assert!(!flags.is_downloading());
        assert!(flags.is_downloaded());
    }

    #[test]
    fn attachment_flags_all_set() {
        let mut flags = AttachmentFlags::NONE;
        flags.set_downloading(true);
        flags.set_downloaded(true);
        flags.set_short_nonce(true);
        assert!(flags.is_downloading());
        assert!(flags.is_downloaded());
        assert!(flags.is_short_nonce());
    }

    #[test]
    fn attachment_flags_set_then_clear() {
        let mut flags = AttachmentFlags::NONE;
        flags.set_downloading(true);
        flags.set_downloaded(true);
        flags.set_downloading(false);
        assert!(!flags.is_downloading(), "downloading should be cleared");
        assert!(flags.is_downloaded(), "downloaded should remain set");
    }

    #[test]
    fn attachment_flags_default_none() {
        let flags = AttachmentFlags::NONE;
        assert!(!flags.is_downloading());
        assert!(!flags.is_downloaded());
        assert!(!flags.is_short_nonce());
        assert_eq!(flags, AttachmentFlags::default());
    }

    #[test]
    fn attachment_flags_bit_values() {
        // Verify the bit constants are distinct
        let mut flags = AttachmentFlags::NONE;
        flags.set_downloading(true);
        assert_eq!(flags.0, 0b0001);

        let mut flags = AttachmentFlags::NONE;
        flags.set_downloaded(true);
        assert_eq!(flags.0, 0b0010);

        let mut flags = AttachmentFlags::NONE;
        flags.set_short_nonce(true);
        assert_eq!(flags.0, 0b0100);
    }

    // ========================================================================
    // NpubInterner Tests
    // ========================================================================

    #[test]
    fn interner_returns_incrementing_handles() {
        let mut interner = NpubInterner::new();
        let h0 = interner.intern("npub1aaa");
        let h1 = interner.intern("npub1bbb");
        let h2 = interner.intern("npub1ccc");
        assert_eq!(h0, 0, "first intern should be handle 0");
        assert_eq!(h1, 1, "second intern should be handle 1");
        assert_eq!(h2, 2, "third intern should be handle 2");
    }

    #[test]
    fn interner_lookup_finds_interned() {
        let mut interner = NpubInterner::new();
        let h = interner.intern("npub1alice");
        let found = interner.lookup("npub1alice");
        assert_eq!(found, Some(h), "lookup should find interned string");
    }

    #[test]
    fn interner_lookup_returns_none_for_unknown() {
        let interner = NpubInterner::new();
        assert_eq!(interner.lookup("npub1unknown"), None, "lookup on empty interner should be None");
    }

    #[test]
    fn interner_lookup_returns_none_for_not_interned() {
        let mut interner = NpubInterner::new();
        interner.intern("npub1alice");
        assert_eq!(interner.lookup("npub1bob"), None, "lookup for non-interned should be None");
    }

    #[test]
    fn interner_resolve_returns_string() {
        let mut interner = NpubInterner::new();
        let h = interner.intern("npub1test123");
        assert_eq!(interner.resolve(h), Some("npub1test123"), "resolve should return the original string");
    }

    #[test]
    fn interner_resolve_returns_none_for_no_npub() {
        let interner = NpubInterner::new();
        assert_eq!(interner.resolve(NO_NPUB), None, "resolve(NO_NPUB) should be None");
    }

    #[test]
    fn interner_resolve_returns_none_for_out_of_bounds() {
        let mut interner = NpubInterner::new();
        interner.intern("npub1only");
        assert_eq!(interner.resolve(999), None, "out-of-bounds handle should resolve to None");
    }

    #[test]
    fn interner_duplicate_returns_same_handle() {
        let mut interner = NpubInterner::new();
        let h1 = interner.intern("npub1dup");
        let h2 = interner.intern("npub1dup");
        let h3 = interner.intern("npub1dup");
        assert_eq!(h1, h2, "duplicate intern should return same handle");
        assert_eq!(h2, h3, "duplicate intern should return same handle");
        assert_eq!(interner.len(), 1, "duplicates should not increase length");
    }

    #[test]
    fn interner_100_unique_npubs_stress() {
        let mut interner = NpubInterner::new();
        let mut handles = Vec::new();

        for i in 0..100 {
            let npub = format!("npub1stress{:04}", i);
            let h = interner.intern(&npub);
            handles.push((h, npub));
        }

        assert_eq!(interner.len(), 100, "should have 100 unique npubs");

        // Verify all handles resolve correctly
        for (h, npub) in &handles {
            assert_eq!(interner.resolve(*h), Some(npub.as_str()),
                "handle {} should resolve to {}", h, npub);
        }

        // Verify all lookups work
        for (h, npub) in &handles {
            assert_eq!(interner.lookup(npub), Some(*h),
                "lookup for {} should return handle {}", npub, h);
        }

        // Re-interning should return same handles
        for (h, npub) in &handles {
            assert_eq!(interner.intern(npub), *h,
                "re-interning {} should return same handle {}", npub, h);
        }
        assert_eq!(interner.len(), 100, "re-interning should not grow interner");
    }

    #[test]
    fn interner_memory_usage_reasonable() {
        let mut interner = NpubInterner::new();
        for i in 0..50 {
            interner.intern(&format!("npub1{:0>62}", i));
        }
        let mem = interner.memory_usage();
        // Should be in the ballpark of 50 * (64 bytes string + overhead)
        assert!(mem > 0, "memory usage should be positive");
        assert!(mem < 100_000, "memory usage for 50 npubs should be under 100KB, was {}", mem);
    }

    #[test]
    fn interner_empty() {
        let interner = NpubInterner::new();
        assert_eq!(interner.len(), 0);
        assert!(interner.is_empty());
        assert_eq!(interner.resolve(0), None);
        assert_eq!(interner.lookup("anything"), None);
        assert!(interner.memory_usage() > 0, "even empty interner has struct overhead");
    }

    #[test]
    fn interner_intern_opt_none() {
        let mut interner = NpubInterner::new();
        let h = interner.intern_opt(None);
        assert_eq!(h, NO_NPUB, "intern_opt(None) should return NO_NPUB");
        assert_eq!(interner.len(), 0, "None should not add to interner");
    }

    #[test]
    fn interner_intern_opt_empty_string() {
        let mut interner = NpubInterner::new();
        let h = interner.intern_opt(Some(""));
        assert_eq!(h, NO_NPUB, "intern_opt(Some('')) should return NO_NPUB");
    }

    #[test]
    fn interner_intern_opt_some_value() {
        let mut interner = NpubInterner::new();
        let h = interner.intern_opt(Some("npub1real"));
        assert_ne!(h, NO_NPUB, "intern_opt(Some(value)) should not return NO_NPUB");
        assert_eq!(interner.resolve(h), Some("npub1real"));
    }

    // ========================================================================
    // CompactMessageVec Tests
    // ========================================================================

    /// Helper to create a minimal CompactMessage with given hex ID and timestamp
    fn make_compact_msg(hex_id: &str, timestamp: u64) -> CompactMessage {
        CompactMessage {
            id: encode_message_id(hex_id),
            at: timestamp,
            flags: MessageFlags::NONE,
            npub_idx: NO_NPUB,
            replied_to: None,
            replied_to_npub_idx: NO_NPUB,
            wrapper_id: None,
            content: "test".into(),
            replied_to_content: None,
            attachments: TinyVec::new(),
            reactions: TinyVec::new(),
            edit_history: None,
            preview_metadata: None,
        }
    }

    #[test]
    fn compact_vec_insert_single_message() {
        let mut vec = CompactMessageVec::new();
        let msg = make_compact_msg(
            "1111111111111111111111111111111111111111111111111111111111111111",
            100,
        );
        assert!(vec.insert(msg), "insert should succeed");
        assert_eq!(vec.len(), 1);
        assert!(!vec.is_empty());
    }

    #[test]
    fn compact_vec_insert_duplicate_rejected() {
        let mut vec = CompactMessageVec::new();
        let id = "2222222222222222222222222222222222222222222222222222222222222222";
        let msg1 = make_compact_msg(id, 100);
        let msg2 = make_compact_msg(id, 200); // same ID, different timestamp
        assert!(vec.insert(msg1), "first insert should succeed");
        assert!(!vec.insert(msg2), "duplicate ID should be rejected");
        assert_eq!(vec.len(), 1);
    }

    #[test]
    fn compact_vec_insert_batch_multiple() {
        let mut vec = CompactMessageVec::new();
        let msgs = vec![
            make_compact_msg("aa00000000000000000000000000000000000000000000000000000000000000", 100),
            make_compact_msg("bb00000000000000000000000000000000000000000000000000000000000000", 200),
            make_compact_msg("cc00000000000000000000000000000000000000000000000000000000000000", 300),
        ];
        let added = vec.insert_batch(msgs);
        assert_eq!(added, 3, "all 3 should be added");
        assert_eq!(vec.len(), 3);
    }

    #[test]
    fn compact_vec_insert_batch_dedup() {
        let mut vec = CompactMessageVec::new();
        let id = "dd00000000000000000000000000000000000000000000000000000000000000";
        vec.insert(make_compact_msg(id, 100));

        let msgs = vec![
            make_compact_msg(id, 200), // duplicate
            make_compact_msg("ee00000000000000000000000000000000000000000000000000000000000000", 300),
        ];
        let added = vec.insert_batch(msgs);
        assert_eq!(added, 1, "only non-duplicate should be added");
        assert_eq!(vec.len(), 2);
    }

    #[test]
    fn compact_vec_find_by_hex_id() {
        let mut vec = CompactMessageVec::new();
        let id = "ff00000000000000000000000000000000000000000000000000000000000001";
        let mut msg = make_compact_msg(id, 500);
        msg.content = "found me".into();
        vec.insert(msg);

        let found = vec.find_by_hex_id(id);
        assert!(found.is_some(), "should find by hex id");
        assert_eq!(&*found.unwrap().content, "found me");
    }

    #[test]
    fn compact_vec_find_by_hex_id_not_found() {
        let mut vec = CompactMessageVec::new();
        vec.insert(make_compact_msg(
            "aa00000000000000000000000000000000000000000000000000000000000000", 100,
        ));
        let found = vec.find_by_hex_id(
            "bb00000000000000000000000000000000000000000000000000000000000000",
        );
        assert!(found.is_none(), "should not find non-existent ID");
    }

    #[test]
    fn compact_vec_find_by_hex_id_empty_string() {
        let mut vec = CompactMessageVec::new();
        vec.insert(make_compact_msg(
            "aa00000000000000000000000000000000000000000000000000000000000000", 100,
        ));
        assert!(vec.find_by_hex_id("").is_none(), "empty string should return None");
    }

    #[test]
    fn compact_vec_find_by_hex_id_mut() {
        let mut vec = CompactMessageVec::new();
        let id = "ff00000000000000000000000000000000000000000000000000000000000002";
        vec.insert(make_compact_msg(id, 500));

        let found = vec.find_by_hex_id_mut(id);
        assert!(found.is_some(), "should find mutable ref by hex id");
        found.unwrap().content = "modified".into();

        // Verify modification stuck
        let found = vec.find_by_hex_id(id);
        assert_eq!(&*found.unwrap().content, "modified");
    }

    #[test]
    fn compact_vec_contains_hex_id() {
        let mut vec = CompactMessageVec::new();
        let id = "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
        vec.insert(make_compact_msg(id, 100));

        assert!(vec.contains_hex_id(id), "should contain inserted ID");
        assert!(!vec.contains_hex_id(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "should not contain non-inserted ID");
        assert!(!vec.contains_hex_id(""), "empty string should not match");
    }

    #[test]
    fn compact_vec_remove_by_hex_id() {
        let mut vec = CompactMessageVec::new();
        let id1 = "1100000000000000000000000000000000000000000000000000000000000000";
        let id2 = "2200000000000000000000000000000000000000000000000000000000000000";
        vec.insert(make_compact_msg(id1, 100));
        vec.insert(make_compact_msg(id2, 200));
        assert_eq!(vec.len(), 2);

        assert!(vec.remove_by_hex_id(id1), "remove should succeed");
        assert_eq!(vec.len(), 1);
        assert!(!vec.contains_hex_id(id1), "removed ID should not be found");
        assert!(vec.contains_hex_id(id2), "remaining ID should still be found");
    }

    #[test]
    fn compact_vec_remove_nonexistent() {
        let mut vec = CompactMessageVec::new();
        vec.insert(make_compact_msg(
            "1100000000000000000000000000000000000000000000000000000000000000", 100,
        ));
        assert!(!vec.remove_by_hex_id(
            "9900000000000000000000000000000000000000000000000000000000000000"),
            "removing non-existent should return false");
        assert!(!vec.remove_by_hex_id(""), "removing empty should return false");
        assert_eq!(vec.len(), 1);
    }

    #[test]
    fn compact_vec_last_timestamp() {
        let mut vec = CompactMessageVec::new();
        assert_eq!(vec.last_timestamp(), None, "empty vec should have no last timestamp");

        vec.insert(make_compact_msg(
            "aa00000000000000000000000000000000000000000000000000000000000000", 100,
        ));
        vec.insert(make_compact_msg(
            "bb00000000000000000000000000000000000000000000000000000000000000", 300,
        ));
        vec.insert(make_compact_msg(
            "cc00000000000000000000000000000000000000000000000000000000000000", 200,
        ));

        let last_ts = vec.last_timestamp().unwrap();
        // Messages are sorted by timestamp, so last should be the one with at=300
        let expected = timestamp_from_compact(300);
        assert_eq!(last_ts, expected, "last timestamp should be the largest");
    }

    #[test]
    fn compact_vec_empty_operations() {
        let vec = CompactMessageVec::new();
        assert!(vec.is_empty());
        assert_eq!(vec.len(), 0);
        assert!(vec.last().is_none());
        assert!(vec.first().is_none());
        assert!(vec.last_timestamp().is_none());
        assert!(!vec.contains_hex_id("anything"));
        assert!(vec.find_by_hex_id("anything").is_none());
    }

    #[test]
    fn compact_vec_1000_message_stress() {
        let mut vec = CompactMessageVec::new();

        // Insert 1000 messages
        for i in 0..1000u64 {
            let id = format!("{:0>64x}", i);
            let msg = make_compact_msg(&id, i * 10);
            assert!(vec.insert(msg), "insert {} should succeed", i);
        }
        assert_eq!(vec.len(), 1000);

        // Verify every message can be found
        for i in 0..1000u64 {
            let id = format!("{:0>64x}", i);
            assert!(vec.contains_hex_id(&id), "should find message {}", i);
            let found = vec.find_by_hex_id(&id).unwrap();
            assert_eq!(found.at, i * 10, "timestamp should match for message {}", i);
        }

        // Verify non-existent IDs are not found
        for i in 1000..1010u64 {
            let id = format!("{:0>64x}", i);
            assert!(!vec.contains_hex_id(&id), "should not find non-existent {}", i);
        }

        // Messages should be in timestamp order
        let timestamps: Vec<u64> = vec.iter().map(|m| m.at).collect();
        for w in timestamps.windows(2) {
            assert!(w[0] <= w[1], "messages should be sorted by timestamp: {} <= {}", w[0], w[1]);
        }
    }

    #[test]
    fn compact_vec_rebuild_index_after_id_change() {
        let mut vec = CompactMessageVec::new();
        let old_id = "aa00000000000000000000000000000000000000000000000000000000000000";
        let new_id = "ff00000000000000000000000000000000000000000000000000000000000000";
        vec.insert(make_compact_msg(old_id, 100));

        // Mutate the message's ID directly (simulating an ID update like pending -> confirmed)
        vec.messages_mut()[0].id = encode_message_id(new_id);
        // Index is now stale
        assert!(!vec.contains_hex_id(new_id), "stale index should not find new ID");

        // Rebuild index
        vec.rebuild_index();
        assert!(vec.contains_hex_id(new_id), "after rebuild, new ID should be found");
        assert!(!vec.contains_hex_id(old_id), "after rebuild, old ID should not be found");
    }

    #[test]
    fn compact_vec_pending_id_lookup() {
        let mut vec = CompactMessageVec::new();
        let pending = "pending-9876543210";
        vec.insert(make_compact_msg(pending, 500));

        assert!(vec.contains_hex_id(pending), "should find pending ID");
        let found = vec.find_by_hex_id(pending);
        assert!(found.is_some(), "should find pending message");
        assert_eq!(found.unwrap().id_hex(), pending, "id_hex should match pending string");
    }

    #[test]
    fn compact_vec_out_of_order_insert() {
        let mut vec = CompactMessageVec::new();
        // Insert messages out of timestamp order
        vec.insert(make_compact_msg(
            "bb00000000000000000000000000000000000000000000000000000000000000", 300,
        ));
        vec.insert(make_compact_msg(
            "aa00000000000000000000000000000000000000000000000000000000000000", 100,
        ));
        vec.insert(make_compact_msg(
            "cc00000000000000000000000000000000000000000000000000000000000000", 200,
        ));

        assert_eq!(vec.len(), 3);
        // Verify sorted by timestamp
        let timestamps: Vec<u64> = vec.iter().map(|m| m.at).collect();
        assert_eq!(timestamps, vec![100, 200, 300], "should be sorted by timestamp");

        // All lookups should still work
        assert!(vec.contains_hex_id("aa00000000000000000000000000000000000000000000000000000000000000"));
        assert!(vec.contains_hex_id("bb00000000000000000000000000000000000000000000000000000000000000"));
        assert!(vec.contains_hex_id("cc00000000000000000000000000000000000000000000000000000000000000"));
    }

    #[test]
    fn compact_vec_batch_prepend() {
        let mut vec = CompactMessageVec::new();
        // First insert newer messages
        vec.insert(make_compact_msg(
            "cc00000000000000000000000000000000000000000000000000000000000000", 300,
        ));
        vec.insert(make_compact_msg(
            "dd00000000000000000000000000000000000000000000000000000000000000", 400,
        ));

        // Then batch-insert older messages (pagination scenario)
        let older = vec![
            make_compact_msg("aa00000000000000000000000000000000000000000000000000000000000000", 100),
            make_compact_msg("bb00000000000000000000000000000000000000000000000000000000000000", 200),
        ];
        let added = vec.insert_batch(older);
        assert_eq!(added, 2);
        assert_eq!(vec.len(), 4);

        // Verify order
        let timestamps: Vec<u64> = vec.iter().map(|m| m.at).collect();
        assert_eq!(timestamps, vec![100, 200, 300, 400]);

        // All lookups should work
        assert!(vec.contains_hex_id("aa00000000000000000000000000000000000000000000000000000000000000"));
        assert!(vec.contains_hex_id("dd00000000000000000000000000000000000000000000000000000000000000"));
    }

    #[test]
    fn compact_vec_clear() {
        let mut vec = CompactMessageVec::new();
        vec.insert(make_compact_msg(
            "aa00000000000000000000000000000000000000000000000000000000000000", 100,
        ));
        vec.insert(make_compact_msg(
            "bb00000000000000000000000000000000000000000000000000000000000000", 200,
        ));
        assert_eq!(vec.len(), 2);

        vec.clear();
        assert!(vec.is_empty());
        assert_eq!(vec.len(), 0);
        assert!(!vec.contains_hex_id("aa00000000000000000000000000000000000000000000000000000000000000"));
    }

    // ========================================================================
    // CompactMessage from_message / to_message Tests
    // ========================================================================

    /// Helper to create a full Message with all fields populated
    fn make_full_message() -> Message {
        Message {
            id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
            content: "Hello, world!".into(),
            replied_to: "1111111111111111111111111111111111111111111111111111111111111111".into(),
            replied_to_content: Some("Original message".into()),
            replied_to_npub: Some("npub1replier".into()),
            replied_to_has_attachment: Some(true),
            preview_metadata: Some(SiteMetadata {
                domain: "example.com".into(),
                og_title: Some("Test Page".into()),
                og_description: Some("A test description".into()),
                og_image: Some("https://example.com/img.png".into()),
                og_url: Some("https://example.com".into()),
                og_type: Some("website".into()),
                title: Some("Test".into()),
                description: Some("Desc".into()),
                favicon: Some("https://example.com/favicon.ico".into()),
            }),
            attachments: vec![Attachment {
                id: "aaaa000000000000000000000000000000000000000000000000000000000000".into(),
                key: "bbbb000000000000000000000000000000000000000000000000000000000000".into(),
                nonce: "cccccccccccccccccccccccccccccccc".into(), // 32 hex chars = 16 bytes
                extension: "png".into(),
                name: "photo.png".into(),
                url: "https://blossom.example.com".into(),
                path: "/tmp/photo.png".into(),
                size: 12345,
                img_meta: Some(ImageMetadata {
                    thumbhash: "abc123".into(),
                    width: 800,
                    height: 600,
                }),
                downloading: false,
                downloaded: true,
                webxdc_topic: None,
                group_id: None,
                original_hash: None,
                scheme_version: None,
                mls_filename: None,
            }],
            reactions: vec![Reaction {
                id: "dddd000000000000000000000000000000000000000000000000000000000000".into(),
                reference_id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
                author_id: "npub1reactor".into(),
                emoji: "\u{1f44d}".into(), // thumbs up
            }],
            at: 1705320000000, // 2024-01-15 12:00:00 UTC ms
            pending: false,
            failed: false,
            mine: true,
            npub: Some("npub1sender".into()),
            wrapper_event_id: Some("eeee000000000000000000000000000000000000000000000000000000000000".into()),
            edited: true,
            edit_history: Some(vec![
                EditEntry { content: "Original".into(), edited_at: 1705320000000 },
                EditEntry { content: "Edited".into(), edited_at: 1705320060000 },
            ]),
        }
    }

    #[test]
    fn compact_message_from_message_roundtrip_all_fields() {
        let msg = make_full_message();
        let mut interner = NpubInterner::new();
        let compact = CompactMessage::from_message(&msg, &mut interner);
        let restored = compact.to_message(&interner);

        assert_eq!(restored.id, msg.id, "id mismatch");
        assert_eq!(restored.content, msg.content, "content mismatch");
        assert_eq!(restored.mine, msg.mine, "mine mismatch");
        assert_eq!(restored.pending, msg.pending, "pending mismatch");
        assert_eq!(restored.failed, msg.failed, "failed mismatch");
        assert_eq!(restored.npub, msg.npub, "npub mismatch");
        assert_eq!(restored.replied_to, msg.replied_to, "replied_to mismatch");
        assert_eq!(restored.replied_to_content, msg.replied_to_content, "replied_to_content mismatch");
        assert_eq!(restored.replied_to_npub, msg.replied_to_npub, "replied_to_npub mismatch");
        assert_eq!(restored.replied_to_has_attachment, msg.replied_to_has_attachment, "replied_to_has_attachment mismatch");
        assert_eq!(restored.wrapper_event_id, msg.wrapper_event_id, "wrapper_event_id mismatch");
        assert_eq!(restored.edited, msg.edited, "edited mismatch");
        assert_eq!(restored.edit_history, msg.edit_history, "edit_history mismatch");
        assert_eq!(restored.preview_metadata, msg.preview_metadata, "preview_metadata mismatch");
        // Timestamp loses sub-second precision but seconds should match
        assert_eq!(restored.at / 1000, msg.at / 1000, "timestamp seconds mismatch");
        // Attachments
        assert_eq!(restored.attachments.len(), 1, "should have 1 attachment");
        assert_eq!(restored.attachments[0].id, msg.attachments[0].id);
        assert_eq!(restored.attachments[0].name, msg.attachments[0].name);
        assert_eq!(restored.attachments[0].size, msg.attachments[0].size);
        // Reactions
        assert_eq!(restored.reactions.len(), 1, "should have 1 reaction");
        assert_eq!(restored.reactions[0].emoji, msg.reactions[0].emoji);
    }

    #[test]
    fn compact_message_from_message_owned_roundtrip() {
        let msg = make_full_message();
        let msg_clone = msg.clone();
        let mut interner = NpubInterner::new();
        let compact = CompactMessage::from_message_owned(msg, &mut interner);
        let restored = compact.to_message(&interner);

        assert_eq!(restored.id, msg_clone.id, "id mismatch");
        assert_eq!(restored.content, msg_clone.content, "content mismatch");
        assert_eq!(restored.mine, msg_clone.mine, "mine mismatch");
        assert_eq!(restored.npub, msg_clone.npub, "npub mismatch");
        assert_eq!(restored.edit_history, msg_clone.edit_history, "edit_history mismatch");
    }

    #[test]
    fn compact_message_pending_flag() {
        let msg = Message {
            id: "pending-1234567890".into(),
            pending: true,
            ..Message::default()
        };
        let mut interner = NpubInterner::new();
        let compact = CompactMessage::from_message(&msg, &mut interner);
        assert!(compact.is_pending(), "pending flag should be set");
        let restored = compact.to_message(&interner);
        assert!(restored.pending, "pending should roundtrip");
        assert_eq!(restored.id, "pending-1234567890", "pending ID should roundtrip");
    }

    #[test]
    fn compact_message_failed_flag() {
        let msg = Message {
            id: "pending-999".into(),
            failed: true,
            pending: true,
            ..Message::default()
        };
        let mut interner = NpubInterner::new();
        let compact = CompactMessage::from_message(&msg, &mut interner);
        assert!(compact.is_failed(), "failed flag should be set");
        assert!(compact.is_pending(), "pending flag should also be set");
        let restored = compact.to_message(&interner);
        assert!(restored.failed);
        assert!(restored.pending);
    }

    #[test]
    fn compact_message_with_attachments_roundtrip() {
        let msg = Message {
            id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
            attachments: vec![
                Attachment {
                    id: "1111111111111111111111111111111111111111111111111111111111111111".into(),
                    extension: "jpg".into(),
                    name: "sunset.jpg".into(),
                    size: 5000,
                    downloaded: true,
                    ..Attachment::default()
                },
                Attachment {
                    id: "2222222222222222222222222222222222222222222222222222222222222222".into(),
                    extension: "mp4".into(),
                    name: "video.mp4".into(),
                    size: 50000,
                    downloaded: false,
                    downloading: true,
                    ..Attachment::default()
                },
            ],
            ..Message::default()
        };
        let mut interner = NpubInterner::new();
        let compact = CompactMessage::from_message(&msg, &mut interner);
        assert_eq!(compact.attachments.len(), 2);

        let restored = compact.to_message(&interner);
        assert_eq!(restored.attachments.len(), 2);
        assert_eq!(restored.attachments[0].name, "sunset.jpg");
        assert_eq!(restored.attachments[0].extension, "jpg");
        assert!(restored.attachments[0].downloaded);
        assert_eq!(restored.attachments[1].name, "video.mp4");
        assert!(restored.attachments[1].downloading);
        assert!(!restored.attachments[1].downloaded);
    }

    #[test]
    fn compact_message_with_reactions_roundtrip() {
        let msg = Message {
            id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
            reactions: vec![
                Reaction {
                    id: "aaa0000000000000000000000000000000000000000000000000000000000000".into(),
                    reference_id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
                    author_id: "npub1alice".into(),
                    emoji: "\u{2764}".into(), // heart
                },
                Reaction {
                    id: "bbb0000000000000000000000000000000000000000000000000000000000000".into(),
                    reference_id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
                    author_id: "npub1bob".into(),
                    emoji: "\u{1f525}".into(), // fire
                },
            ],
            ..Message::default()
        };
        let mut interner = NpubInterner::new();
        let compact = CompactMessage::from_message(&msg, &mut interner);
        let restored = compact.to_message(&interner);

        assert_eq!(restored.reactions.len(), 2);
        assert_eq!(restored.reactions[0].emoji, "\u{2764}");
        assert_eq!(restored.reactions[0].author_id, "npub1alice");
        assert_eq!(restored.reactions[1].emoji, "\u{1f525}");
        assert_eq!(restored.reactions[1].author_id, "npub1bob");
    }

    #[test]
    fn compact_message_with_edit_history_roundtrip() {
        let msg = Message {
            id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
            content: "Final version".into(),
            edited: true,
            edit_history: Some(vec![
                EditEntry { content: "First draft".into(), edited_at: 1000 },
                EditEntry { content: "Second draft".into(), edited_at: 2000 },
                EditEntry { content: "Final version".into(), edited_at: 3000 },
            ]),
            ..Message::default()
        };
        let mut interner = NpubInterner::new();
        let compact = CompactMessage::from_message(&msg, &mut interner);
        assert!(compact.is_edited());

        let restored = compact.to_message(&interner);
        assert!(restored.edited);
        let history = restored.edit_history.unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].content, "First draft");
        assert_eq!(history[2].content, "Final version");
    }

    #[test]
    fn compact_message_with_preview_metadata_roundtrip() {
        let meta = SiteMetadata {
            domain: "example.com".into(),
            og_title: Some("Title".into()),
            og_description: Some("Desc".into()),
            og_image: Some("https://example.com/img.png".into()),
            og_url: None,
            og_type: None,
            title: None,
            description: None,
            favicon: None,
        };
        let msg = Message {
            id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
            preview_metadata: Some(meta.clone()),
            ..Message::default()
        };
        let mut interner = NpubInterner::new();
        let compact = CompactMessage::from_message(&msg, &mut interner);
        let restored = compact.to_message(&interner);

        let restored_meta = restored.preview_metadata.unwrap();
        assert_eq!(restored_meta.domain, "example.com");
        assert_eq!(restored_meta.og_title, Some("Title".into()));
        assert_eq!(restored_meta.og_image, Some("https://example.com/img.png".into()));
        assert_eq!(restored_meta.og_url, None);
    }

    #[test]
    fn compact_message_with_replied_to_roundtrip() {
        let msg = Message {
            id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
            replied_to: "1111111111111111111111111111111111111111111111111111111111111111".into(),
            replied_to_content: Some("Original text".into()),
            replied_to_npub: Some("npub1original".into()),
            replied_to_has_attachment: Some(false),
            ..Message::default()
        };
        let mut interner = NpubInterner::new();
        let compact = CompactMessage::from_message(&msg, &mut interner);
        assert!(compact.has_reply());

        let restored = compact.to_message(&interner);
        assert_eq!(restored.replied_to, "1111111111111111111111111111111111111111111111111111111111111111");
        assert_eq!(restored.replied_to_content, Some("Original text".into()));
        assert_eq!(restored.replied_to_npub, Some("npub1original".into()));
        assert_eq!(restored.replied_to_has_attachment, Some(false));
    }

    #[test]
    fn compact_message_empty_roundtrip() {
        let msg = Message::default();
        let mut interner = NpubInterner::new();
        let compact = CompactMessage::from_message(&msg, &mut interner);
        let restored = compact.to_message(&interner);

        assert_eq!(restored.id, "0000000000000000000000000000000000000000000000000000000000000000",
            "empty ID should decode as all zeros hex");
        assert_eq!(restored.content, "");
        assert!(!restored.mine);
        assert!(!restored.pending);
        assert!(!restored.failed);
        assert!(!restored.edited);
        assert_eq!(restored.npub, None);
        assert!(restored.replied_to.is_empty() || restored.replied_to == "0000000000000000000000000000000000000000000000000000000000000000");
        assert_eq!(restored.replied_to_content, None);
        assert_eq!(restored.replied_to_npub, None);
        assert_eq!(restored.replied_to_has_attachment, None);
        assert_eq!(restored.wrapper_event_id, None);
        assert!(restored.attachments.is_empty());
        assert!(restored.reactions.is_empty());
        assert_eq!(restored.edit_history, None);
        assert_eq!(restored.preview_metadata, None);
    }

    // ========================================================================
    // CompactAttachment Tests
    // ========================================================================

    #[test]
    fn compact_attachment_from_attachment_roundtrip() {
        let att = Attachment {
            id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
            key: "1111111111111111111111111111111111111111111111111111111111111111".into(),
            nonce: "aabbccddaabbccddaabbccddaabbccdd".into(), // 32 hex = 16-byte DM nonce
            extension: "zip".into(),
            name: "archive.zip".into(),
            url: "https://blossom.test.com".into(),
            path: "/downloads/archive.zip".into(),
            size: 99999,
            img_meta: None,
            downloading: false,
            downloaded: true,
            webxdc_topic: None,
            group_id: None,
            original_hash: None,
            scheme_version: None,
            mls_filename: None,
        };

        let compact = CompactAttachment::from_attachment(&att);
        let restored = compact.to_attachment();

        assert_eq!(restored.id, att.id, "id mismatch");
        assert_eq!(restored.key, att.key, "key mismatch");
        assert_eq!(restored.nonce, att.nonce, "nonce mismatch");
        assert_eq!(restored.extension, att.extension, "extension mismatch");
        assert_eq!(restored.name, att.name, "name mismatch");
        assert_eq!(restored.url, att.url, "url mismatch");
        assert_eq!(restored.path, att.path, "path mismatch");
        assert_eq!(restored.size, att.size, "size mismatch");
        assert_eq!(restored.downloading, att.downloading, "downloading mismatch");
        assert_eq!(restored.downloaded, att.downloaded, "downloaded mismatch");
    }

    #[test]
    fn compact_attachment_from_attachment_owned_roundtrip() {
        let att = Attachment {
            id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
            key: "1111111111111111111111111111111111111111111111111111111111111111".into(),
            nonce: "aabbccddaabbccddaabbccddaabbccdd".into(),
            extension: "pdf".into(),
            name: "document.pdf".into(),
            url: "https://server.com".into(),
            path: "".into(),
            size: 1024,
            img_meta: None,
            downloading: false,
            downloaded: false,
            webxdc_topic: None,
            group_id: None,
            original_hash: None,
            scheme_version: None,
            mls_filename: None,
        };
        let att_clone = att.clone();

        let compact = CompactAttachment::from_attachment_owned(att);
        let restored = compact.to_attachment();

        assert_eq!(restored.id, att_clone.id);
        assert_eq!(restored.name, att_clone.name);
        assert_eq!(restored.size, att_clone.size);
    }

    #[test]
    fn compact_attachment_key_nonce_zeros() {
        let att = Attachment {
            id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
            key: "".into(), // empty key = MLS derived
            nonce: "".into(), // empty nonce
            ..Attachment::default()
        };
        let compact = CompactAttachment::from_attachment(&att);
        assert_eq!(compact.key, [0u8; 32], "empty key should be all zeros");
        assert_eq!(compact.nonce, [0u8; 16], "empty nonce should be all zeros");

        let restored = compact.to_attachment();
        assert_eq!(restored.key, "", "zero key should restore as empty string");
        assert_eq!(restored.nonce, "", "zero nonce should restore as empty string");
    }

    #[test]
    fn compact_attachment_short_nonce_mls_12byte() {
        // MLS nonce is 12 bytes = 24 hex chars
        let att = Attachment {
            id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
            nonce: "aabbccddaabbccddaabbccdd".into(), // 24 hex chars = 12 bytes
            ..Attachment::default()
        };
        let compact = CompactAttachment::from_attachment(&att);
        assert!(compact.flags.is_short_nonce(), "12-byte nonce should set short_nonce flag");

        let restored = compact.to_attachment();
        assert_eq!(restored.nonce, "aabbccddaabbccddaabbccdd", "short nonce should roundtrip");
    }

    #[test]
    fn compact_attachment_long_nonce_dm_16byte() {
        // DM nonce is 16 bytes = 32 hex chars
        let att = Attachment {
            id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
            nonce: "aabbccddaabbccddaabbccddaabbccdd".into(), // 32 hex chars = 16 bytes
            ..Attachment::default()
        };
        let compact = CompactAttachment::from_attachment(&att);
        assert!(!compact.flags.is_short_nonce(), "16-byte nonce should NOT set short_nonce flag");

        let restored = compact.to_attachment();
        assert_eq!(restored.nonce, "aabbccddaabbccddaabbccddaabbccdd", "long nonce should roundtrip");
    }

    #[test]
    fn compact_attachment_id_eq_comparison() {
        let att = Attachment {
            id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
            ..Attachment::default()
        };
        let compact = CompactAttachment::from_attachment(&att);

        assert!(compact.id_eq("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"),
            "id_eq should match same hex");
        assert!(!compact.id_eq("1111111111111111111111111111111111111111111111111111111111111111"),
            "id_eq should not match different hex");
    }

    #[test]
    fn compact_attachment_all_optional_fields_none() {
        let att = Attachment::default();
        let compact = CompactAttachment::from_attachment(&att);

        assert!(compact.img_meta.is_none());
        assert!(compact.group_id.is_none());
        assert!(compact.original_hash.is_none());
        assert!(compact.webxdc_topic.is_none());
        assert!(compact.mls_filename.is_none());
        assert!(compact.scheme_version.is_none());

        let restored = compact.to_attachment();
        assert!(restored.img_meta.is_none());
        assert!(restored.group_id.is_none());
        assert!(restored.original_hash.is_none());
        assert!(restored.webxdc_topic.is_none());
        assert!(restored.mls_filename.is_none());
        assert!(restored.scheme_version.is_none());
    }

    #[test]
    fn compact_attachment_all_optional_fields_some() {
        let att = Attachment {
            id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
            key: "1111111111111111111111111111111111111111111111111111111111111111".into(),
            nonce: "aabbccddaabbccddaabbccddaabbccdd".into(),
            extension: "xdc".into(),
            name: "app.xdc".into(),
            url: "https://server.com/file".into(),
            path: "/local/app.xdc".into(),
            size: 50000,
            img_meta: Some(ImageMetadata {
                thumbhash: "hash123".into(),
                width: 1920,
                height: 1080,
            }),
            downloading: false,
            downloaded: true,
            webxdc_topic: Some("game-state".into()),
            group_id: Some("cccc000000000000000000000000000000000000000000000000000000000000".into()),
            original_hash: Some("dddd000000000000000000000000000000000000000000000000000000000000".into()),
            scheme_version: Some("mip04-v1".into()),
            mls_filename: Some("encrypted.bin".into()),
        };

        let compact = CompactAttachment::from_attachment(&att);

        assert!(compact.img_meta.is_some());
        assert!(compact.group_id.is_some());
        assert!(compact.original_hash.is_some());
        assert!(compact.webxdc_topic.is_some());
        assert!(compact.mls_filename.is_some());
        assert!(compact.scheme_version.is_some());

        let restored = compact.to_attachment();
        let meta = restored.img_meta.unwrap();
        assert_eq!(meta.thumbhash, "hash123");
        assert_eq!(meta.width, 1920);
        assert_eq!(meta.height, 1080);
        assert_eq!(restored.webxdc_topic, Some("game-state".into()));
        assert_eq!(restored.group_id.unwrap(), att.group_id.unwrap());
        assert_eq!(restored.original_hash.unwrap(), att.original_hash.unwrap());
        assert_eq!(restored.scheme_version, Some("mip04-v1".into()));
        assert_eq!(restored.mls_filename, Some("encrypted.bin".into()));
    }

    // ========================================================================
    // CompactReaction Tests
    // ========================================================================

    #[test]
    fn compact_reaction_from_reaction_roundtrip() {
        let reaction = Reaction {
            id: "aaaa000000000000000000000000000000000000000000000000000000000000".into(),
            reference_id: "bbbb000000000000000000000000000000000000000000000000000000000000".into(),
            author_id: "npub1alice".into(),
            emoji: "+".into(),
        };
        let mut interner = NpubInterner::new();
        let compact = CompactReaction::from_reaction(&reaction, &mut interner);
        let restored = compact.to_reaction(&interner);

        assert_eq!(restored.id, reaction.id, "id mismatch");
        assert_eq!(restored.reference_id, reaction.reference_id, "reference_id mismatch");
        assert_eq!(restored.author_id, reaction.author_id, "author_id mismatch");
        assert_eq!(restored.emoji, reaction.emoji, "emoji mismatch");
    }

    #[test]
    fn compact_reaction_author_resolved_via_interner() {
        let mut interner = NpubInterner::new();
        let alice_handle = interner.intern("npub1alice");

        let reaction = Reaction {
            id: "aaaa000000000000000000000000000000000000000000000000000000000000".into(),
            reference_id: "bbbb000000000000000000000000000000000000000000000000000000000000".into(),
            author_id: "npub1alice".into(),
            emoji: "+".into(),
        };
        let compact = CompactReaction::from_reaction(&reaction, &mut interner);
        assert_eq!(compact.author_idx, alice_handle, "should reuse existing interner handle");

        let resolved = interner.resolve(compact.author_idx).unwrap();
        assert_eq!(resolved, "npub1alice");
    }

    #[test]
    fn compact_reaction_unicode_emoji() {
        let reaction = Reaction {
            id: "aaaa000000000000000000000000000000000000000000000000000000000000".into(),
            reference_id: "bbbb000000000000000000000000000000000000000000000000000000000000".into(),
            author_id: "npub1test".into(),
            emoji: "\u{1f431}\u{200d}\u{1f4bb}".into(), // cat with laptop (ZWJ sequence)
        };
        let mut interner = NpubInterner::new();
        let compact = CompactReaction::from_reaction(&reaction, &mut interner);
        let restored = compact.to_reaction(&interner);
        assert_eq!(restored.emoji, "\u{1f431}\u{200d}\u{1f4bb}", "complex unicode emoji should roundtrip");
    }

    #[test]
    fn compact_reaction_custom_emoji() {
        let reaction = Reaction {
            id: "aaaa000000000000000000000000000000000000000000000000000000000000".into(),
            reference_id: "bbbb000000000000000000000000000000000000000000000000000000000000".into(),
            author_id: "npub1test".into(),
            emoji: ":cat_heart_eyes:".into(),
        };
        let mut interner = NpubInterner::new();
        let compact = CompactReaction::from_reaction(&reaction, &mut interner);
        let restored = compact.to_reaction(&interner);
        assert_eq!(restored.emoji, ":cat_heart_eyes:", "custom emoji shortcode should roundtrip");
    }

    #[test]
    fn compact_reaction_owned_conversion() {
        let reaction = Reaction {
            id: "aaaa000000000000000000000000000000000000000000000000000000000000".into(),
            reference_id: "bbbb000000000000000000000000000000000000000000000000000000000000".into(),
            author_id: "npub1bob".into(),
            emoji: "\u{1f44d}".into(), // thumbs up
        };
        let reaction_clone = reaction.clone();
        let mut interner = NpubInterner::new();
        let compact = CompactReaction::from_reaction_owned(reaction, &mut interner);
        let restored = compact.to_reaction(&interner);

        assert_eq!(restored.id, reaction_clone.id);
        assert_eq!(restored.author_id, reaction_clone.author_id);
        assert_eq!(restored.emoji, reaction_clone.emoji);
    }

    // ========================================================================
    // TinyVec Tests
    // ========================================================================

    #[test]
    fn tinyvec_empty() {
        let tv: TinyVec<u32> = TinyVec::new();
        assert!(tv.is_empty());
        assert_eq!(tv.len(), 0);
        assert_eq!(tv.as_slice(), &[] as &[u32]);
        assert_eq!(tv.first(), None);
        assert_eq!(tv.last(), None);
    }

    #[test]
    fn tinyvec_from_vec_and_back() {
        let original = vec![1u32, 2, 3, 4, 5];
        let tv = TinyVec::from_vec(original.clone());
        assert_eq!(tv.len(), 5);
        assert_eq!(tv.to_vec(), original);
    }

    #[test]
    fn tinyvec_indexing() {
        let tv = TinyVec::from_vec(vec![10u32, 20, 30]);
        assert_eq!(tv[0], 10);
        assert_eq!(tv[1], 20);
        assert_eq!(tv[2], 30);
        assert_eq!(tv.get(0), Some(&10));
        assert_eq!(tv.get(3), None);
    }

    #[test]
    fn tinyvec_push() {
        let mut tv = TinyVec::from_vec(vec![1u32, 2]);
        tv.push(3);
        assert_eq!(tv.len(), 3);
        assert_eq!(tv.to_vec(), vec![1, 2, 3]);
    }

    #[test]
    fn tinyvec_clone() {
        let tv = TinyVec::from_vec(vec!["hello".to_string(), "world".to_string()]);
        let cloned = tv.clone();
        assert_eq!(cloned.len(), 2);
        assert_eq!(cloned[0], "hello");
        assert_eq!(cloned[1], "world");
    }

    #[test]
    fn tinyvec_retain() {
        let mut tv = TinyVec::from_vec(vec![1u32, 2, 3, 4, 5]);
        tv.retain(|&x| x % 2 == 0);
        assert_eq!(tv.to_vec(), vec![2, 4]);
    }

    #[test]
    fn tinyvec_empty_from_empty_vec() {
        let tv = TinyVec::<u32>::from_vec(vec![]);
        assert!(tv.is_empty());
        assert_eq!(tv.len(), 0);
    }

    #[test]
    fn tinyvec_iter() {
        let tv = TinyVec::from_vec(vec![10u32, 20, 30]);
        let sum: u32 = tv.iter().sum();
        assert_eq!(sum, 60);
    }

    #[test]
    fn tinyvec_any() {
        let tv = TinyVec::from_vec(vec![1u32, 2, 3]);
        assert!(tv.any(|&x| x == 2));
        assert!(!tv.any(|&x| x == 99));
    }

    // ========================================================================
    // hex_to_bytes_16 / bytes_to_hex_string Tests
    // ========================================================================

    #[test]
    fn hex_to_bytes_16_full_32_chars() {
        let hex = "aabbccddaabbccddaabbccddaabbccdd";
        let bytes = hex_to_bytes_16(hex);
        assert_eq!(bytes[0], 0xaa);
        assert_eq!(bytes[1], 0xbb);
        assert_eq!(bytes[15], 0xdd);
    }

    #[test]
    fn hex_to_bytes_16_short_input_padded() {
        // Short input gets right-padded with '0' before decode
        let hex = "aabb";
        let bytes = hex_to_bytes_16(hex);
        assert_eq!(bytes[0], 0xaa);
        assert_eq!(bytes[1], 0xbb);
        // Remaining bytes should be 0 (from padding)
        for i in 2..16 {
            assert_eq!(bytes[i], 0, "byte {} should be 0 from padding", i);
        }
    }

    #[test]
    fn bytes_to_hex_string_roundtrip() {
        let bytes: Vec<u8> = vec![0xaa, 0xbb, 0xcc, 0xdd];
        let hex = bytes_to_hex_string(&bytes);
        assert_eq!(hex, "aabbccdd");
    }
}
