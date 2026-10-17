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
use crate::simd::hex::{bytes_to_hex_32, bytes_to_hex_string, hex_to_bytes_32};

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

/// Marker byte for pending IDs (first byte = 0x01).
const PENDING_ID_MARKER: u8 = 0x01;

/// Trailing sentinel for encoded pending ids (bytes 17..32). The marker byte
/// alone is NOT safe: real event ids are uniform hashes, so 1 in 256 begins
/// with 0x01 and would decode as a phantom "pending-…" string — mangling read
/// markers, frontend-facing ids, every decode consumer. Requiring marker AND
/// sentinel makes that misread a ~2^-120 event.
const PENDING_ID_SENTINEL: [u8; 15] = [0xFE; 15];

/// Encode an ID string to 32 bytes, handling pending IDs specially.
/// - Pending IDs ("pending-{nanoseconds}") are encoded as marker byte +
///   timestamp + sentinel tail
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
        bytes[17..32].copy_from_slice(&PENDING_ID_SENTINEL);
        bytes
    } else {
        hex_to_bytes_32(id)
    }
}

/// Decode 32 bytes back to an ID string, handling pending IDs specially.
#[inline]
pub fn decode_message_id(bytes: &[u8; 32]) -> String {
    if bytes[0] == PENDING_ID_MARKER && bytes[17..32] == PENDING_ID_SENTINEL {
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

/// Distinct-emoji ceiling on a single message's reaction set. Joining an
/// existing group is never bounded by this; opening a group past it is
/// refused at the STATE chokepoint (inbound and outbound alike). The UI's
/// reaction row mirrors this number.
pub const MAX_REACTION_GROUPS: usize = 12;

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

use std::alloc::{alloc, dealloc, realloc, Layout};
use std::marker::PhantomData;
use std::ptr::NonNull;

/// Ultra-compact vector using a thin pointer (8 bytes on stack).
///
/// Memory layout:
/// - Stack: single pointer (8 bytes) - null for empty
/// - Heap: `[len: u32][items: T...]` - exact-sized, only allocated when non-empty
///
/// Compared to standard types:
/// - `Vec<T>`: 24 bytes (ptr + len + cap), plus growth slack
/// - `Box<[T]>`: 16 bytes (fat pointer)
/// - `TinyVec<T>`: 8 bytes (thin pointer)
///
/// No spare capacity: a push reallocates in place where the allocator can, which suits
/// collections that are mostly empty or tiny and grow one item at a time.
pub struct TinyVec<T> {
    /// Null = empty, otherwise points to: [len: u32][items: T...]
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
        let mut out = Self::new();
        if vec.is_empty() {
            return out;
        }
        let len = vec.len();
        let (layout, items_offset) = Self::layout_for(len);
        unsafe {
            let ptr = alloc(layout);
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            *(ptr as *mut u32) = len as u32;
            let items_ptr = ptr.add(items_offset) as *mut T;
            for (i, item) in vec.into_iter().enumerate() {
                std::ptr::write(items_ptr.add(i), item);
            }
            out.ptr = NonNull::new(ptr);
        }
        out
    }

    /// Calculate layout for allocation
    fn layout_for(len: usize) -> (Layout, usize) {
        let header_layout = Layout::new::<u32>();
        let items_layout = Layout::array::<T>(len).unwrap();
        header_layout.extend(items_layout).unwrap()
    }

    /// Number of items
    #[inline]
    pub fn len(&self) -> usize {
        match self.ptr {
            None => 0,
            Some(ptr) => unsafe { *(ptr.as_ptr() as *const u32) as usize },
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ptr.is_none()
    }

    /// Get items offset within allocation
    #[inline]
    fn items_offset() -> usize {
        Self::layout_for(1).1
    }

    #[inline]
    fn items_ptr(&self) -> *mut T {
        match self.ptr {
            None => NonNull::dangling().as_ptr(),
            Some(ptr) => unsafe { ptr.as_ptr().add(Self::items_offset()) as *mut T },
        }
    }

    /// Get a slice of the items
    #[inline]
    pub fn as_slice(&self) -> &[T] {
        unsafe { std::slice::from_raw_parts(self.items_ptr(), self.len()) }
    }

    /// Get a mutable slice of the items
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        unsafe { std::slice::from_raw_parts_mut(self.items_ptr(), self.len()) }
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

    /// Append an item, growing the allocation by exactly one slot (moved, never cloned).
    pub fn push(&mut self, item: T) {
        let len = self.len();
        assert!(len < u32::MAX as usize, "TinyVec length overflow");
        let (new_layout, items_offset) = Self::layout_for(len + 1);
        unsafe {
            let ptr = match self.ptr {
                None => alloc(new_layout),
                Some(old) => realloc(old.as_ptr(), Self::layout_for(len).0, new_layout.size()),
            };
            if ptr.is_null() {
                std::alloc::handle_alloc_error(new_layout);
            }
            std::ptr::write((ptr.add(items_offset) as *mut T).add(len), item);
            *(ptr as *mut u32) = (len + 1) as u32;
            self.ptr = NonNull::new(ptr);
        }
    }

    /// Keep the items matching a predicate, compacting in place and shrinking to fit.
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&T) -> bool,
    {
        let len = self.len();
        if len == 0 {
            return;
        }
        let items = self.items_ptr();
        // Detached while items move: a panicking predicate leaks the block, never double-drops.
        let base = self.ptr.take().unwrap().as_ptr();
        let mut kept = 0;
        unsafe {
            for i in 0..len {
                if f(&*items.add(i)) {
                    if i != kept {
                        std::ptr::copy_nonoverlapping(items.add(i), items.add(kept), 1);
                    }
                    kept += 1;
                } else {
                    std::ptr::drop_in_place(items.add(i));
                }
            }
            if kept == 0 {
                dealloc(base, Self::layout_for(len).0);
                return;
            }
            let ptr = if kept == len {
                base
            } else {
                realloc(base, Self::layout_for(len).0, Self::layout_for(kept).0.size())
            };
            if ptr.is_null() {
                std::alloc::handle_alloc_error(Self::layout_for(kept).0);
            }
            *(ptr as *mut u32) = kept as u32;
            self.ptr = NonNull::new(ptr);
        }
    }

    /// Bytes this holds on the heap (header and items; nothing the items own).
    pub fn heap_bytes(&self) -> usize {
        if self.is_empty() { 0 } else { Self::layout_for(self.len()).0.size() }
    }

    /// Check if any item matches a predicate
    pub fn any<F>(&self, f: F) -> bool
    where
        F: FnMut(&T) -> bool,
    {
        self.as_slice().iter().any(f)
    }
}

impl<T> FromIterator<T> for TinyVec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::from_vec(iter.into_iter().collect())
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
                let len = self.len();
                std::ptr::drop_in_place(std::ptr::slice_from_raw_parts_mut(self.items_ptr(), len));
                dealloc(ptr.as_ptr(), Self::layout_for(len).0);
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

/// Memory-efficient reaction: binary id, interned author, interned (emoji, url) pair.
/// 40 bytes and `Copy`, so adding or removing one never allocates per reaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompactReaction {
    /// Reaction event ID as binary
    pub id: [u8; 32],
    /// The (emoji, NIP-30 url) pair, interned via [`NpubInterner::intern_emoji`]
    pub emoji_idx: u32,
    /// Author npub index (interned via NpubInterner)
    pub author_idx: u16,
}

impl CompactReaction {
    /// Get reaction ID as hex string
    #[inline]
    pub fn id_hex(&self) -> String {
        bytes_to_hex_32(&self.id)
    }

    /// The emoji, resolved through the interner it was interned with.
    #[inline]
    pub fn emoji<'a>(&self, interner: &'a NpubInterner) -> &'a str {
        interner.emoji(self.emoji_idx).0
    }

    /// Convert from regular Reaction, interning author and emoji
    pub fn from_reaction(reaction: &Reaction, interner: &mut NpubInterner) -> Self {
        Self {
            id: hex_to_bytes_32(&reaction.id),
            emoji_idx: interner.intern_emoji(&reaction.emoji, reaction.emoji_url.as_deref()),
            author_idx: interner.intern(&reaction.author_id),
        }
    }

    /// Convert from regular Reaction (owned), interning author and emoji
    pub fn from_reaction_owned(reaction: Reaction, interner: &mut NpubInterner) -> Self {
        Self::from_reaction(&reaction, interner)
    }

    /// Convert back to regular Reaction, resolving author from interner.
    /// `parent_hex` is the reacted-to message's id: a CompactReaction is always nested
    /// inside that message, so its id IS the reference.
    pub fn to_reaction(&self, parent_hex: &str, interner: &NpubInterner) -> Reaction {
        let (emoji, emoji_url) = interner.emoji(self.emoji_idx);
        Reaction {
            id: self.id_hex(),
            reference_id: parent_hex.to_string(),
            author_id: interner.resolve(self.author_idx)
                .map(|s| s.to_string())
                .unwrap_or_default(),
            emoji: emoji.to_string(),
            emoji_url: emoji_url.map(|s| s.to_string()),
        }
    }
}

// ============================================================================
// Compact Attachment
// ============================================================================

/// Packed attachment state: the three bools, where the local path comes from, and
/// whether the original hash is the id.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttachmentFlags(u8);

impl AttachmentFlags {
    pub const NONE: Self = Self(0);
    const DOWNLOADING: u8 = 0b0001;
    const DOWNLOADED: u8  = 0b0010;
    const SHORT_NONCE: u8 = 0b0100; // 12-byte nonce (legacy) vs 16-byte (DM)
    const PATH_MASK: u8 = 0b1_1000;
    /// `{download dir}{id}.{ext}`
    const PATH_HASHED: u8 = 0b0_1000;
    /// `{download dir}{name}`
    const PATH_NAMED: u8 = 0b1_0000;
    /// Held verbatim in the cold box
    const PATH_CUSTOM: u8 = 0b1_1000;
    const ORIGINAL_IS_ID: u8 = 0b10_0000;

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

    #[inline]
    fn path_kind(self) -> u8 { self.0 & Self::PATH_MASK }

    #[inline]
    fn set_path_kind(&mut self, kind: u8) { self.0 = (self.0 & !Self::PATH_MASK) | kind; }

    pub fn from_bools(downloading: bool, downloaded: bool) -> Self {
        let mut flags = Self::NONE;
        flags.set_downloading(downloading);
        flags.set_downloaded(downloaded);
        flags
    }
}

/// Extensions held as their index here; anything else rides the cold box as [`EXT_COLD`].
const EXTENSIONS: [&str; 93] = [
    "", "jpg", "jpeg", "png", "gif", "webp", "avif", "heic", "heif", "bmp", "tiff", "tif", "ico", "svg", "jfif",
    "mp4", "mov", "webm", "mkv", "avi", "m4v", "3gp", "mpg", "mpeg", "ogv", "flv", "wmv",
    "mp3", "m4a", "ogg", "oga", "opus", "wav", "flac", "aac", "wma", "weba", "caf", "amr",
    "pdf", "txt", "md", "csv", "json", "xml", "yaml", "yml", "toml", "rtf", "log", "html", "htm", "css",
    "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "epub",
    "zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "iso", "dmg", "apk", "jar", "exe", "deb",
    "xdc", "js", "ts", "py", "rs", "sql", "sh",
    "glb", "gltf", "obj", "stl", "dae", "arw", "cr2", "dng", "nef",
];
const EXT_COLD: u8 = u8::MAX;

fn extension_code(ext: &str) -> u8 {
    EXTENSIONS.iter().position(|&e| e == ext).map_or(EXT_COLD, |i| i as u8)
}

/// The download directory as a path prefix, captured once so a derived path rebuilds
/// to the same string for the life of the process, whatever the directory does later.
fn download_prefix() -> &'static str {
    static PREFIX: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    PREFIX.get_or_init(|| {
        let probe = crate::db::get_download_dir().join("x");
        let probe = probe.to_string_lossy();
        probe[..probe.len() - 1].into()
    })
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// `s` is exactly the lowercase hex of `id`, as [`bytes_to_hex_32`] writes it.
fn is_hex_of(s: &[u8], id: &[u8; 32]) -> bool {
    s.len() == 64
        && id.iter().enumerate().all(|(i, b)| s[2 * i] == HEX[(b >> 4) as usize] && s[2 * i + 1] == HEX[(b & 0xf) as usize])
}

/// Which derivation reproduces `path` byte for byte, if any.
fn classify_path(path: &str, id: &[u8; 32], ext: &str, name: &str) -> u8 {
    if path.is_empty() {
        return 0;
    }
    let Some(file) = path.strip_prefix(download_prefix()).map(str::as_bytes) else {
        return AttachmentFlags::PATH_CUSTOM;
    };
    if !name.is_empty() && file == name.as_bytes() {
        return AttachmentFlags::PATH_NAMED;
    }
    if file.len() == 65 + ext.len() && file[64] == b'.' && file[65..] == *ext.as_bytes() && is_hex_of(&file[..64], id) {
        return AttachmentFlags::PATH_HASHED;
    }
    AttachmentFlags::PATH_CUSTOM
}

/// The fields few attachments carry, boxed together so the rest pay 8 bytes.
#[derive(Clone, Debug, Default)]
struct AttachmentCold {
    /// Set when the extension has no code
    extension: Box<str>,
    /// Set when the path has no derivation
    path: Box<str>,
    /// Mirror URLs for the same ciphertext
    fallback_urls: Box<[Box<str>]>,
    /// WebXDC topic (Mini Apps only)
    webxdc_topic: Option<Box<str>>,
    /// Original file hash, when it differs from the id
    original_hash: Option<Box<[u8; 32]>>,
    /// Legacy group ID for key derivation
    group_id: Option<Box<[u8; 32]>>,
}

impl AttachmentCold {
    fn is_empty(&self) -> bool {
        self.extension.is_empty()
            && self.path.is_empty()
            && self.fallback_urls.is_empty()
            && self.webxdc_topic.is_none()
            && self.original_hash.is_none()
            && self.group_id.is_none()
    }
}

/// Memory-efficient attachment: binary hashes, the extension as a code, the local path
/// derived from the id or name when it can be, image metadata in one allocation, and
/// the rarely-set fields behind one box.
#[derive(Clone, Debug)]
pub struct CompactAttachment {
    /// SHA256 file hash as binary. A derived path follows it.
    id: [u8; 32],
    /// Encryption key - 32 bytes (empty = legacy derived)
    pub key: [u8; 32],
    /// Encryption nonce - 16 bytes (AES-256-GCM with 0xChat compatibility)
    pub nonce: [u8; 16],
    /// File size in bytes
    pub size: u64,
    /// Host URL (blossom server, etc.)
    pub url: Box<str>,
    /// Original filename (e.g. "memories.zip"). Empty = fallback to {hash}.{ext}. A
    /// derived path follows it.
    name: Box<str>,
    /// `[width u32 LE][height u32 LE][thumbhash]`; empty when there is none
    img_meta: TinyVec<u8>,
    cold: Option<Box<AttachmentCold>>,
    /// Packed boolean flags, path kind and original-hash bit
    flags: AttachmentFlags,
    /// Index into [`EXTENSIONS`], or [`EXT_COLD`]
    ext: u8,
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
            // 12-byte nonce (legacy/MIP-04)
            bytes_to_hex_string(&self.nonce[..12])
        } else {
            // 16-byte nonce (DM/0xChat)
            bytes_to_hex_string(&self.nonce)
        }
    }

    #[inline]
    fn cold(&self) -> Option<&AttachmentCold> {
        self.cold.as_deref()
    }

    fn cold_mut(&mut self) -> &mut AttachmentCold {
        self.cold.get_or_insert_with(Default::default)
    }

    fn drop_empty_cold(&mut self) {
        if self.cold.as_ref().is_some_and(|c| c.is_empty()) {
            self.cold = None;
        }
    }

    /// File extension (e.g., "png", "mp4")
    #[inline]
    pub fn extension(&self) -> &str {
        match EXTENSIONS.get(self.ext as usize) {
            Some(ext) => ext,
            None => self.cold().map_or("", |c| &c.extension),
        }
    }

    /// Whether a local path is set, without building it.
    #[inline]
    pub fn has_path(&self) -> bool {
        self.flags.path_kind() != 0
    }

    /// Local file path (empty if not downloaded). Allocates only for a derived path.
    pub fn path(&self) -> std::borrow::Cow<'_, str> {
        use std::borrow::Cow;
        match self.flags.path_kind() {
            AttachmentFlags::PATH_HASHED => {
                let (prefix, ext) = (download_prefix(), self.extension());
                let mut path = String::with_capacity(prefix.len() + 65 + ext.len());
                path.push_str(prefix);
                for b in self.id {
                    path.push(HEX[(b >> 4) as usize] as char);
                    path.push(HEX[(b & 0xf) as usize] as char);
                }
                path.push('.');
                path.push_str(ext);
                Cow::Owned(path)
            }
            AttachmentFlags::PATH_NAMED => Cow::Owned([download_prefix(), &self.name].concat()),
            AttachmentFlags::PATH_CUSTOM => Cow::Borrowed(self.cold().map_or("", |c| &c.path)),
            _ => Cow::Borrowed(""),
        }
    }

    #[inline]
    pub fn id(&self) -> &[u8; 32] {
        &self.id
    }

    #[inline]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[inline]
    pub fn flags(&self) -> AttachmentFlags {
        self.flags
    }

    /// The path and original hash stay what they were: both may be derived from the id.
    pub fn set_id(&mut self, id: [u8; 32]) {
        if id == self.id {
            return;
        }
        let path = self.path().into_owned();
        if self.flags.0 & AttachmentFlags::ORIGINAL_IS_ID != 0 {
            self.flags.0 &= !AttachmentFlags::ORIGINAL_IS_ID;
            let old = self.id;
            self.cold_mut().original_hash = Some(Box::new(old));
        }
        self.id = id;
        self.set_path(path);
    }

    /// The path stays what it was: it may be derived from the name.
    pub fn set_name(&mut self, name: &str) {
        if *name == *self.name {
            return;
        }
        let path = self.path().into_owned();
        self.name = name.into();
        self.set_path(path);
    }

    pub fn set_path(&mut self, path: String) {
        let kind = classify_path(&path, &self.id, self.extension(), &self.name);
        self.flags.set_path_kind(kind);
        if kind == AttachmentFlags::PATH_CUSTOM {
            self.cold_mut().path = path.into_boxed_str();
        } else if let Some(cold) = self.cold.as_mut() {
            cold.path = Box::default();
            self.drop_empty_cold();
        }
    }

    /// Image dimensions and thumbhash (only for images/videos)
    pub fn img_meta(&self) -> Option<ImageMetadata> {
        let b = self.img_meta.as_slice();
        (b.len() >= 8).then(|| ImageMetadata {
            width: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            height: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            thumbhash: std::str::from_utf8(&b[8..]).unwrap_or_default().to_string(),
        })
    }

    #[inline]
    pub fn has_img_meta(&self) -> bool {
        !self.img_meta.is_empty()
    }

    pub fn set_img_meta(&mut self, meta: Option<&ImageMetadata>) {
        self.img_meta = pack_img_meta(meta);
    }

    /// Original file hash before encryption
    pub fn original_hash(&self) -> Option<[u8; 32]> {
        if self.flags.0 & AttachmentFlags::ORIGINAL_IS_ID != 0 {
            return Some(self.id);
        }
        self.cold().and_then(|c| c.original_hash.as_deref().copied())
    }

    /// Legacy group ID for key derivation
    pub fn group_id(&self) -> Option<[u8; 32]> {
        self.cold().and_then(|c| c.group_id.as_deref().copied())
    }

    /// WebXDC topic (Mini Apps only)
    #[inline]
    pub fn webxdc_topic(&self) -> Option<&str> {
        self.cold().and_then(|c| c.webxdc_topic.as_deref())
    }

    /// Mirror URLs for the same ciphertext
    #[inline]
    pub fn fallback_urls(&self) -> &[Box<str>] {
        self.cold().map_or(&[], |c| &c.fallback_urls)
    }

    pub fn set_fallback_urls(&mut self, urls: impl IntoIterator<Item = Box<str>>) {
        let urls: Box<[Box<str>]> = urls.into_iter().collect();
        if !urls.is_empty() {
            self.cold_mut().fallback_urls = urls;
        } else if let Some(cold) = self.cold.as_mut() {
            cold.fallback_urls = Box::default();
            self.drop_empty_cold();
        }
    }

    /// Convert from regular Attachment (borrowed)
    pub fn from_attachment(att: &Attachment) -> Self {
        Self::compact(
            att,
            att.url.as_str().into(),
            att.name.as_str().into(),
            att.fallback_urls.iter().map(|s| s.as_str().into()).collect(),
            att.webxdc_topic.as_deref().map(Into::into),
        )
    }

    /// Convert from regular Attachment (owned) - zero-copy where possible
    pub fn from_attachment_owned(mut att: Attachment) -> Self {
        let url = std::mem::take(&mut att.url).into_boxed_str();
        let name = std::mem::take(&mut att.name).into_boxed_str();
        let fallback_urls = std::mem::take(&mut att.fallback_urls).into_iter().map(String::into_boxed_str).collect();
        let webxdc_topic = att.webxdc_topic.take().map(String::into_boxed_str);
        Self::compact(&att, url, name, fallback_urls, webxdc_topic)
    }

    fn compact(
        att: &Attachment,
        url: Box<str>,
        name: Box<str>,
        fallback_urls: Box<[Box<str>]>,
        webxdc_topic: Option<Box<str>>,
    ) -> Self {
        let id = hex_to_bytes_32(&att.id);
        let ext = extension_code(&att.extension);
        let mut flags = AttachmentFlags::from_bools(att.downloading, att.downloaded);
        // Detect short nonce (12 bytes = 24 hex chars) for legacy attachments
        flags.set_short_nonce(att.nonce.len() == 24);
        let path_kind = classify_path(&att.path, &id, &att.extension, &name);
        flags.set_path_kind(path_kind);

        let original = att.original_hash.as_deref().map(hex_to_bytes_32);
        if original == Some(id) {
            flags.0 |= AttachmentFlags::ORIGINAL_IS_ID;
        }
        let cold = AttachmentCold {
            extension: if ext == EXT_COLD { att.extension.as_str().into() } else { Box::default() },
            path: if path_kind == AttachmentFlags::PATH_CUSTOM { att.path.as_str().into() } else { Box::default() },
            fallback_urls,
            webxdc_topic,
            original_hash: original.filter(|h| *h != id).map(Box::new),
            group_id: att.group_id.as_deref().map(|s| Box::new(hex_to_bytes_32(s))),
        };

        Self {
            id,
            key: if att.key.is_empty() { [0u8; 32] } else { hex_to_bytes_32(&att.key) },
            nonce: if att.nonce.is_empty() { [0u8; 16] } else { parse_nonce(&att.nonce) },
            size: att.size,
            url,
            name,
            img_meta: pack_img_meta(att.img_meta.as_ref()),
            cold: (!cold.is_empty()).then(|| Box::new(cold)),
            flags,
            ext,
        }
    }

    /// Convert back to regular Attachment
    pub fn to_attachment(&self) -> Attachment {
        Attachment {
            id: self.id_hex(),
            key: self.key_hex(),
            nonce: self.nonce_hex(),
            extension: self.extension().to_string(),
            name: self.name.to_string(),
            url: self.url.to_string(),
            path: self.path().into_owned(),
            size: self.size,
            img_meta: self.img_meta(),
            downloading: self.flags.is_downloading(),
            downloaded: self.flags.is_downloaded(),
            webxdc_topic: self.webxdc_topic().map(str::to_string),
            group_id: self.group_id().map(|b| bytes_to_hex_32(&b)),
            original_hash: self.original_hash().map(|b| bytes_to_hex_32(&b)),
            fallback_urls: self.fallback_urls().iter().map(|u| u.to_string()).collect(),
        }
    }

    /// Heap bytes held beyond the struct itself (DeepSize).
    #[cfg(debug_assertions)]
    pub(crate) fn heap_bytes(&self) -> usize {
        self.url.len()
            + self.name.len()
            + self.img_meta.heap_bytes()
            + self.cold().map_or(0, |c| {
                std::mem::size_of::<AttachmentCold>()
                    + c.extension.len()
                    + c.path.len()
                    + std::mem::size_of_val(&*c.fallback_urls)
                    + c.fallback_urls.iter().map(|u| u.len()).sum::<usize>()
                    + c.webxdc_topic.as_ref().map_or(0, |t| t.len())
                    + c.original_hash.as_ref().map_or(0, |_| 32)
                    + c.group_id.as_ref().map_or(0, |_| 32)
            })
    }
}

fn pack_img_meta(meta: Option<&ImageMetadata>) -> TinyVec<u8> {
    let Some(m) = meta else { return TinyVec::new() };
    let mut b = Vec::with_capacity(8 + m.thumbhash.len());
    b.extend_from_slice(&m.width.to_le_bytes());
    b.extend_from_slice(&m.height.to_le_bytes());
    b.extend_from_slice(m.thumbhash.as_bytes());
    TinyVec::from_vec(b)
}

/// Parse a hex nonce string into [u8; 16], left-aligned, zero-allocation.
/// Both DM (32 hex chars) and legacy (24 hex chars) nonces are decoded.
/// Short nonces are right-padded with '0' to reach 32 chars before decode.
#[inline]
fn parse_nonce(hex: &str) -> [u8; 16] {
    hex_to_bytes_16(hex)
}

// ============================================================================
// Npub Interner
// ============================================================================

/// Strings laid end to end in one buffer: entry `i` is `bytes[ends[i - 1]..ends[i]]`.
#[derive(Clone, Debug, Default)]
struct StrArena {
    bytes: Vec<u8>,
    ends: Vec<u32>,
}

impl StrArena {
    /// Whether `extra` more bytes keep every offset within u32.
    #[inline]
    fn fits(&self, extra: usize) -> bool {
        u32::try_from(self.bytes.len() + extra).is_ok()
    }

    /// Append `s` and return its index. Callers check [`Self::fits`] first; a truncated end
    /// offset would let `get` slice through a character.
    fn push(&mut self, s: &str) -> usize {
        let end = u32::try_from(self.bytes.len() + s.len()).expect("arena past u32");
        self.bytes.extend_from_slice(s.as_bytes());
        self.ends.push(end);
        self.ends.len() - 1
    }

    #[inline]
    fn get(&self, i: usize) -> Option<&str> {
        let end = *self.ends.get(i)? as usize;
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        let bytes = &self.bytes[start..end];
        debug_assert!(std::str::from_utf8(bytes).is_ok(), "arena entry {i} split a character");
        // SAFETY: `push` appends whole `&str`s only, and nothing else writes the buffer.
        Some(unsafe { std::str::from_utf8_unchecked(bytes) })
    }

    #[inline]
    fn len(&self) -> usize {
        self.ends.len()
    }

    fn reserve(&mut self, entries: usize, bytes: usize) {
        self.ends.reserve(entries);
        self.bytes.reserve(bytes);
    }

    fn shrink_to_fit(&mut self) {
        self.ends.shrink_to_fit();
        self.bytes.shrink_to_fit();
    }

    fn heap_bytes(&self) -> usize {
        self.bytes.capacity() + self.ends.capacity() * std::mem::size_of::<u32>()
    }
}

/// Length of a bech32 npub, for sizing the arena ahead of a bulk load.
const NPUB_LEN: usize = 63;

/// String interner for npubs using sorted Vec + binary search.
///
/// Each unique npub is stored exactly once. Messages reference npubs by u16 index.
/// - `intern()`: O(log n) lookup + O(n) insert for new strings
/// - `resolve()`: O(1) by index
///
/// Memory: the npub bytes in one arena, plus a 4-byte offset and a 2-byte sorted index each.
#[derive(Clone, Debug, Default)]
pub struct NpubInterner {
    /// npubs in insertion order - index is the stable ID used by messages
    npubs: StrArena,
    /// Indices into npubs, sorted alphabetically for binary search
    sorted: Vec<u16>,
    /// Reaction (emoji, NIP-30 url) pairs in insertion order, two entries each; a few hundred per session.
    emojis: StrArena,
    /// Per pair: whether it has a url, since an absent url and an empty one are different keys.
    emoji_has_url: Vec<bool>,
    /// Indices into emojis, sorted by pair for binary search
    emojis_sorted: Vec<u32>,
}

/// Sentinel value for "no npub" (avoids Option overhead)
pub const NO_NPUB: u16 = u16::MAX;

impl NpubInterner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-allocate capacity for expected number of unique npubs
    pub fn with_capacity(capacity: usize) -> Self {
        let mut interner = Self::default();
        interner.reserve(capacity);
        interner
    }

    /// Room for `additional` more npubs, so a bulk load grows each buffer once.
    pub fn reserve(&mut self, additional: usize) {
        self.npubs.reserve(additional, additional * NPUB_LEN);
        self.sorted.reserve(additional);
    }

    /// Drop the growth slack, once a bulk load is done.
    pub fn shrink_to_fit(&mut self) {
        self.npubs.shrink_to_fit();
        self.sorted.shrink_to_fit();
        self.emojis.shrink_to_fit();
        self.emoji_has_url.shrink_to_fit();
        self.emojis_sorted.shrink_to_fit();
    }

    #[inline]
    fn emoji_pair(&self, idx: usize) -> Option<(&str, Option<&str>)> {
        let at = idx.checked_mul(2)?;
        let emoji = self.emojis.get(at)?;
        let url = self.emojis.get(at + 1)?;
        Some((emoji, self.emoji_has_url[idx].then_some(url)))
    }

    /// Intern a reaction's (emoji, url) pair, returning its stable index.
    pub fn intern_emoji(&mut self, emoji: &str, url: Option<&str>) -> u32 {
        let key = (emoji, url);
        let found = self.emojis_sorted.binary_search_by(|&i| {
            self.emoji_pair(i as usize).unwrap_or(("", None)).cmp(&key)
        });
        match found {
            Ok(pos) => self.emojis_sorted[pos],
            Err(pos) => {
                // Past u32 the pair reads as no emoji, as an unknown index does.
                let Ok(idx) = u32::try_from(self.emoji_has_url.len()) else { return u32::MAX };
                if idx == u32::MAX || !self.emojis.fits(emoji.len() + url.map_or(0, str::len)) {
                    return u32::MAX;
                }
                self.emojis.push(emoji);
                self.emojis.push(url.unwrap_or(""));
                self.emoji_has_url.push(url.is_some());
                self.emojis_sorted.insert(pos, idx);
                idx
            }
        }
    }

    /// Resolve an interned (emoji, url) pair; an unknown index reads as no emoji.
    #[inline]
    pub fn emoji(&self, idx: u32) -> (&str, Option<&str>) {
        self.emoji_pair(idx as usize).unwrap_or(("", None))
    }

    #[inline]
    fn npub_at(&self, idx: u16) -> &str {
        self.npubs.get(idx as usize).unwrap_or("")
    }

    /// Intern an npub string, returning its stable index.
    ///
    /// If the npub already exists, returns the existing index.
    /// If new, stores it and returns a new index.
    pub fn intern(&mut self, npub: &str) -> u16 {
        // Binary search in sorted order
        let result = self.sorted.binary_search_by(|&idx| self.npub_at(idx).cmp(npub));

        match result {
            Ok(pos) => self.sorted[pos], // Found existing
            Err(insert_pos) => {
                // u16::MAX is the NO_NPUB sentinel; at/past it new indices would alias the
                // sentinel then wrap, misattributing authors — degrade to authorless instead.
                if self.npubs.len() >= NO_NPUB as usize || !self.npubs.fits(npub.len()) {
                    return NO_NPUB;
                }
                let new_idx = self.npubs.push(npub) as u16;
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
        self.sorted.binary_search_by(|&idx| self.npub_at(idx).cmp(npub)).ok().map(|pos| self.sorted[pos])
    }

    /// Resolve an index back to the npub string.
    ///
    /// Returns None for NO_NPUB sentinel or out-of-bounds index.
    #[inline]
    pub fn resolve(&self, idx: u16) -> Option<&str> {
        if idx == NO_NPUB {
            return None;
        }
        self.npubs.get(idx as usize)
    }

    /// Number of unique npubs stored
    #[inline]
    pub fn len(&self) -> usize {
        self.npubs.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.npubs.len() == 0
    }

    /// Total memory used by the interner (approximate)
    pub fn memory_usage(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.npubs.heap_bytes()
            + self.sorted.capacity() * std::mem::size_of::<u16>()
            + self.emojis.heap_bytes()
            + self.emoji_has_url.capacity()
            + self.emojis_sorted.capacity() * std::mem::size_of::<u32>()
    }
}

#[cfg(test)]
mod interner_arena_tests {
    use super::*;

    #[test]
    fn arena_entries_of_any_length_read_back() {
        let mut a = StrArena::default();
        let words = ["", "a", "npub1xyz", "", "猫🐈 multi-byte", &"z".repeat(70_000), "end"];
        for (i, w) in words.iter().enumerate() {
            assert!(a.fits(w.len()));
            assert_eq!(a.push(w), i);
        }
        for (i, w) in words.iter().enumerate() {
            assert_eq!(a.get(i), Some(*w), "entry {i}");
        }
        assert_eq!(a.get(words.len()), None);
        assert_eq!(a.bytes.len(), words.iter().map(|w| w.len()).sum::<usize>(), "no separators, no padding");
    }

    #[test]
    fn npubs_intern_resolve_and_look_up_through_the_arena() {
        let npubs: Vec<String> = (0..500).rev().map(|i| format!("npub1{i:0>58}")).collect();
        let mut interner = NpubInterner::new();
        let handles: Vec<u16> = npubs.iter().map(|n| interner.intern(n)).collect();
        assert_eq!(handles, (0..500).collect::<Vec<u16>>(), "handles follow insertion order");
        for (n, &h) in npubs.iter().zip(&handles) {
            assert_eq!(interner.resolve(h), Some(n.as_str()));
            assert_eq!(interner.lookup(n), Some(h));
            assert_eq!(interner.intern(n), h, "a repeat keeps its handle");
        }
        assert_eq!(interner.len(), 500);
        assert_eq!(interner.lookup("npub1missing"), None);
        assert_eq!(interner.npubs.bytes.len(), 500 * 63);
    }

    #[test]
    fn the_handle_space_stops_short_of_the_sentinel() {
        let mut interner = NpubInterner::new();
        for i in 0..NO_NPUB as usize {
            assert_eq!(interner.intern(&format!("n{i:06}")), i as u16);
        }
        assert_eq!(interner.intern("n999999"), NO_NPUB, "the 65536th npub degrades to authorless");
        assert_eq!(interner.len(), NO_NPUB as usize);
        assert_eq!(interner.intern("n000000"), 0, "known npubs still resolve");
        assert_eq!(interner.resolve(65534), Some("n065534"));
        assert_eq!(interner.resolve(NO_NPUB), None);
    }

    #[test]
    fn reserve_and_shrink_keep_every_entry() {
        let mut interner = NpubInterner::new();
        interner.reserve(100);
        assert!(interner.npubs.bytes.capacity() >= 100 * NPUB_LEN);
        let a = interner.intern("npub1a");
        let e = interner.intern_emoji("🔥", None);
        interner.shrink_to_fit();
        assert_eq!(interner.npubs.bytes.capacity(), interner.npubs.bytes.len());
        assert_eq!(interner.resolve(a), Some("npub1a"));
        assert_eq!(interner.emoji(e), ("🔥", None));
    }

    #[test]
    fn emoji_pairs_keep_an_absent_url_apart_from_an_empty_one() {
        let mut interner = NpubInterner::new();
        let none = interner.intern_emoji("party", None);
        let empty = interner.intern_emoji("party", Some(""));
        let url = interner.intern_emoji("party", Some("https://e/p.png"));
        let fire = interner.intern_emoji("🔥", None);
        assert_eq!([none, empty, url, fire], [0, 1, 2, 3]);
        assert_eq!(interner.emoji(none), ("party", None));
        assert_eq!(interner.emoji(empty), ("party", Some("")));
        assert_eq!(interner.emoji(url), ("party", Some("https://e/p.png")));
        assert_eq!(interner.emoji(fire), ("🔥", None));
        assert_eq!(interner.intern_emoji("party", Some("")), empty);
        assert_eq!(interner.intern_emoji("🔥", None), fire);
        assert_eq!(interner.emoji(4), ("", None));
        assert_eq!(interner.emoji(u32::MAX), ("", None));
    }

    #[test]
    fn the_arena_costs_less_than_a_string_per_npub() {
        let mut interner = NpubInterner::new();
        interner.reserve(2000);
        for i in 0..2000 {
            interner.intern(&format!("npub1{i:0>58}"));
        }
        let per = interner.memory_usage() / 2000;
        assert!(per <= 63 + 4 + 2 + 1, "{per} B per npub");
    }
}

// ============================================================================
// Compact Message
// ============================================================================

/// Memory-efficient message: binary ids, interned npubs and emoji, packed flags, and the
/// fields most messages never carry (a reply, edits, a link preview, custom emoji, bot
/// targets) behind two optional boxes. 96 bytes inline.
#[derive(Clone, Debug)]
pub struct CompactMessage {
    /// Message ID as binary (64 hex chars -> 32 bytes)
    id: [u8; 32],
    /// Timestamp in milliseconds (full precision for sub-second ordering)
    pub at: u64,
    /// Message content (Box<str> = 16 bytes vs String's 24 bytes)
    content: Box<str>,
    /// File attachments
    pub attachments: TinyVec<CompactAttachment>,
    /// Emoji reactions
    pub reactions: TinyVec<CompactReaction>,
    /// What this message replies to, when it does
    pub reply: Option<Box<CompactReply>>,
    /// Edit history, link preview, custom emoji and bot targets, when any are present
    pub extras: Option<Box<MessageExtras>>,
    /// NIP-40 expiry as unix SECONDS (0 = permanent). Inline: the self-destruct sweep
    /// scans it across every message.
    pub expiration_secs: u32,
    /// Index into NpubInterner for sender's npub (NO_NPUB if none)
    pub npub_idx: u16,
    /// Packed boolean flags (mine, pending, failed, replied_to_has_attachment)
    pub flags: MessageFlags,
}

/// The replied-to message, as quoted on this one.
#[derive(Clone, Debug)]
pub struct CompactReply {
    pub id: [u8; 32],
    /// Index into NpubInterner for the replied-to author (NO_NPUB if none)
    pub npub_idx: u16,
    pub content: Option<Box<str>>,
    /// The quote's NIP-30 emoji, interned (see [`NpubInterner::intern_emoji`])
    pub emoji_tags: TinyVec<u32>,
}

/// One revision of an edited message.
#[derive(Clone, Debug)]
struct CompactEdit {
    /// Empty for the newest revision while [`MessageExtras::tail_is_content`]: that text is the message's own.
    content: Box<str>,
    edited_at: u64,
}

/// A text span as held: a colour run's stops packed to `0xRRGGBB` when every one is
/// canonical `#rrggbb`, which is all the parsers emit.
#[derive(Clone, Debug)]
enum HeldSpan {
    Color { from: u32, to: u32, effect: crate::text_color::Effect, colors: TinyVec<u32> },
    Time(crate::text_spans::TimeSpan),
    Raw(Box<crate::text_spans::ColorSpan>),
}

fn pack_color(c: &str) -> Option<u32> {
    let h = c.strip_prefix('#')?.as_bytes();
    if h.len() != 6 || !h.iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b)) {
        return None;
    }
    Some(h.iter().fold(0u32, |v, &b| (v << 4) | (b as char).to_digit(16).unwrap_or(0)))
}

impl HeldSpan {
    fn hold(span: crate::text_spans::TextSpan) -> Self {
        use crate::text_spans::TextSpan;
        match span {
            TextSpan::Time(t) => HeldSpan::Time(t),
            TextSpan::Color(c) => match c.colors.iter().map(|s| pack_color(s)).collect::<Option<Vec<u32>>>() {
                Some(colors) => HeldSpan::Color { from: c.from, to: c.to, effect: c.effect, colors: TinyVec::from_vec(colors) },
                None => HeldSpan::Raw(Box::new(c)),
            },
        }
    }

    fn to_span(&self) -> crate::text_spans::TextSpan {
        use crate::text_spans::{ColorSpan, TextSpan};
        match self {
            HeldSpan::Color { from, to, effect, colors } => TextSpan::Color(ColorSpan {
                from: *from,
                to: *to,
                effect: *effect,
                colors: colors.iter().map(|v| format!("#{v:06x}")).collect(),
            }),
            HeldSpan::Time(t) => TextSpan::Time(t.clone()),
            HeldSpan::Raw(c) => TextSpan::Color((**c).clone()),
        }
    }

    #[cfg(debug_assertions)]
    fn heap_bytes(&self) -> usize {
        match self {
            HeldSpan::Color { colors, .. } => colors.heap_bytes(),
            HeldSpan::Time(_) => 0,
            HeldSpan::Raw(c) => {
                std::mem::size_of::<crate::text_spans::ColorSpan>()
                    + c.colors.capacity() * std::mem::size_of::<String>()
                    + c.colors.iter().map(|s| s.capacity()).sum::<usize>()
            }
        }
    }
}

fn hold_spans(spans: Vec<crate::text_spans::TextSpan>) -> TinyVec<HeldSpan> {
    spans.into_iter().map(HeldSpan::hold).collect()
}

/// Preview presence bits, in [`preview_fields`] order, then the two "same as og_" bits.
const PREVIEW_TITLE_IS_OG: u16 = 1 << 8;
const PREVIEW_DESC_IS_OG: u16 = 1 << 9;

fn preview_fields(m: &SiteMetadata) -> [&Option<String>; 8] {
    [&m.og_title, &m.og_description, &m.og_image, &m.og_url, &m.og_type, &m.title, &m.description, &m.favicon]
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    let mut n = s.len();
    while n >= 0x80 {
        out.push(n as u8 | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
    out.extend_from_slice(s.as_bytes());
}

fn take_str<'a>(b: &'a [u8], pos: &mut usize) -> &'a str {
    let (mut n, mut shift) = (0usize, 0);
    while let Some(&byte) = b.get(*pos) {
        *pos += 1;
        n |= ((byte & 0x7f) as usize) << shift;
        if byte < 0x80 {
            break;
        }
        shift += 7;
    }
    let s = b.get(*pos..*pos + n).and_then(|s| std::str::from_utf8(s).ok()).unwrap_or_default();
    *pos += n;
    s
}

/// A link preview in one allocation: presence bits, then the domain and each present
/// string length-prefixed; a title or description equal to its og_ twin is stored once.
fn pack_preview(m: &SiteMetadata) -> TinyVec<u8> {
    let fields = preview_fields(m);
    let mut bits = 0u16;
    if m.title.is_some() && m.title == m.og_title {
        bits |= PREVIEW_TITLE_IS_OG;
    }
    if m.description.is_some() && m.description == m.og_description {
        bits |= PREVIEW_DESC_IS_OG;
    }
    let len = fields.iter().filter_map(|f| f.as_ref()).map(|s| s.len() + 3).sum::<usize>() + m.domain.len() + 5;
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(&[0, 0]);
    put_str(&mut out, &m.domain);
    for (i, f) in fields.iter().enumerate() {
        let Some(s) = f else { continue };
        bits |= 1 << i;
        let deduped = (i == 5 && bits & PREVIEW_TITLE_IS_OG != 0) || (i == 6 && bits & PREVIEW_DESC_IS_OG != 0);
        if !deduped {
            put_str(&mut out, s);
        }
    }
    out[..2].copy_from_slice(&bits.to_le_bytes());
    TinyVec::from_vec(out)
}

fn unpack_preview(b: &[u8]) -> Option<SiteMetadata> {
    let bits = u16::from_le_bytes([*b.first()?, *b.get(1)?]);
    let mut pos = 2;
    let domain = take_str(b, &mut pos).to_string();
    let mut f: [Option<String>; 8] = Default::default();
    for i in 0..8 {
        if bits & (1 << i) == 0 {
            continue;
        }
        f[i] = Some(match i {
            5 if bits & PREVIEW_TITLE_IS_OG != 0 => f[0].clone().unwrap_or_default(),
            6 if bits & PREVIEW_DESC_IS_OG != 0 => f[1].clone().unwrap_or_default(),
            _ => take_str(b, &mut pos).to_string(),
        });
    }
    let [og_title, og_description, og_image, og_url, og_type, title, description, favicon] = f;
    Some(SiteMetadata { domain, og_title, og_description, og_image, og_url, og_type, title, description, favicon })
}

/// The rarely-set parts of a message, boxed together so a plain message pays 8 bytes.
#[derive(Clone, Debug, Default)]
pub struct MessageExtras {
    /// Oldest first
    edits: TinyVec<CompactEdit>,
    /// Packed by [`pack_preview`]; empty when there is none
    preview: TinyVec<u8>,
    /// NIP-30 emoji tags travelling with this rumor, interned
    pub emoji_tags: TinyVec<u32>,
    /// Bot routing targets as interned npub handles
    pub addressed_bots: TinyVec<u16>,
    spans: TinyVec<HeldSpan>,
    role_mentions: TinyVec<[u8; 32]>,
    /// The newest edit's text is the message content, held there once
    tail_is_content: bool,
}

impl MessageExtras {
    fn is_empty(&self) -> bool {
        self.edits.is_empty()
            && self.preview.is_empty()
            && self.emoji_tags.is_empty()
            && self.addressed_bots.is_empty()
            && self.spans.is_empty()
            && self.role_mentions.is_empty()
    }

    fn edit_history(&self, content: &str) -> Vec<EditEntry> {
        let last = self.edits.len().wrapping_sub(1);
        self.edits
            .iter()
            .enumerate()
            .map(|(i, e)| EditEntry {
                content: if i == last && self.tail_is_content { content.to_string() } else { e.content.to_string() },
                edited_at: e.edited_at,
            })
            .collect()
    }

    /// Heap bytes held beyond the box itself (DeepSize).
    #[cfg(debug_assertions)]
    pub(crate) fn heap_bytes(&self) -> usize {
        self.edits.heap_bytes()
            + self.edits.iter().map(|e| e.content.len()).sum::<usize>()
            + self.preview.heap_bytes()
            + self.emoji_tags.heap_bytes()
            + self.addressed_bots.heap_bytes()
            + self.spans.heap_bytes()
            + self.spans.iter().map(HeldSpan::heap_bytes).sum::<usize>()
            + self.role_mentions.heap_bytes()
    }
}

fn intern_emoji_tags(tags: &[crate::types::EmojiTag], interner: &mut NpubInterner) -> TinyVec<u32> {
    tags.iter().map(|t| interner.intern_emoji(&t.shortcode, Some(&t.url))).collect()
}

fn resolve_emoji_tags(tags: &TinyVec<u32>, interner: &NpubInterner) -> Vec<crate::types::EmojiTag> {
    tags.iter()
        .map(|&i| {
            let (shortcode, url) = interner.emoji(i);
            crate::types::EmojiTag { shortcode: shortcode.to_string(), url: url.unwrap_or_default().to_string() }
        })
        .collect()
}

impl CompactMessage {
    /// Check if this message has a replied-to reference
    #[inline]
    pub fn has_reply(&self) -> bool {
        self.reply.is_some()
    }

    /// Check if this message has been edited
    #[inline]
    pub fn is_edited(&self) -> bool {
        self.extras.as_ref().is_some_and(|x| !x.edits.is_empty())
    }

    /// The edit history, oldest first; empty when never edited.
    pub fn edit_history(&self) -> Vec<EditEntry> {
        self.extras.as_ref().map_or_else(Vec::new, |x| x.edit_history(&self.content))
    }

    pub fn preview_metadata(&self) -> Option<SiteMetadata> {
        self.extras.as_ref().and_then(|x| unpack_preview(x.preview.as_slice()))
    }

    pub fn set_preview_metadata(&mut self, metadata: Option<SiteMetadata>) {
        self.extras_mut().preview = metadata.as_ref().map_or_else(TinyVec::new, pack_preview);
        self.drop_empty_extras();
    }

    /// The extras box, created on first use.
    fn extras_mut(&mut self) -> &mut MessageExtras {
        self.extras.get_or_insert_with(Default::default)
    }

    fn drop_empty_extras(&mut self) {
        if self.extras.as_ref().is_some_and(|x| x.is_empty()) {
            self.extras = None;
        }
    }

    /// Fixed once stored: the index orders positions by it.
    #[inline]
    pub fn id(&self) -> &[u8; 32] {
        &self.id
    }

    /// Also the newest revision of the edit history; change it through an edit.
    #[inline]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Get the message ID as a string (hex for event IDs, "pending-..." for pending)
    #[inline]
    pub fn id_hex(&self) -> String {
        decode_message_id(&self.id)
    }

    /// Get the replied-to ID as a hex string, or empty if none
    #[inline]
    pub fn replied_to_hex(&self) -> String {
        match &self.reply {
            Some(r) => bytes_to_hex_32(&r.id),
            None => String::new(),
        }
    }

    /// Get timestamp as milliseconds (for compatibility with frontend)
    #[inline]
    pub fn timestamp_ms(&self) -> u64 {
        timestamp_from_compact(self.at)
    }

    /// Apply an edit to this message.
    ///
    /// `emoji_tags` are the NIP-30 custom-emoji tags resolved from the new
    /// content; they're adopted only when this edit is the newest revision so
    /// an out-of-order older edit can't clobber the live content's emoji.
    pub fn apply_edit(&mut self, mut new_content: String, edited_at: u64, emoji_tags: Vec<crate::types::EmojiTag>, text_spans: Vec<crate::text_spans::TextSpan>, interner: &mut NpubInterner) {
        let original_at = self.timestamp_ms();
        let extras = self.extras.get_or_insert_with(Default::default);
        // Initialize edit history with original content if not present
        if extras.edits.is_empty() {
            extras.edits.push(CompactEdit { content: Box::default(), edited_at: original_at });
            extras.tail_is_content = true;
        }
        // Deduplicate: skip if we already have this edit
        if extras.edits.iter().any(|e| e.edited_at == edited_at) {
            return;
        }
        // Only the newest revision drives the visible content + emoji.
        let is_latest = extras.edits.iter().all(|e| e.edited_at < edited_at);
        if std::mem::take(&mut extras.tail_is_content) {
            if let Some(tail) = extras.edits.last_mut() {
                tail.content = if is_latest { std::mem::take(&mut self.content) } else { self.content.clone() };
            }
        }
        let content = if is_latest { Box::default() } else { std::mem::take(&mut new_content).into_boxed_str() };
        extras.edits.push(CompactEdit { content, edited_at });
        extras.edits.as_mut_slice().sort_by_key(|e| e.edited_at);
        if is_latest {
            self.content = new_content.into_boxed_str();
            extras.tail_is_content = true;
            extras.emoji_tags = intern_emoji_tags(&emoji_tags, interner);
            extras.spans = hold_spans(text_spans);
        } else if let Some(tail) = extras.edits.last_mut().filter(|t| t.content == self.content) {
            tail.content = Box::default();
            extras.tail_is_content = true;
        }
    }

    /// Get replied_to_has_attachment from flags
    #[inline]
    pub fn replied_to_has_attachment(&self) -> Option<bool> {
        self.flags.replied_to_has_attachment()
    }

    /// Add a reaction to this message
    /// (see [`MAX_REACTION_GROUPS`] for the distinct-emoji ceiling)
    pub fn add_reaction(&mut self, reaction: Reaction, interner: &mut NpubInterner) -> bool {
        let reaction_id = hex_to_bytes_32(&reaction.id);
        if self.reactions.iter().any(|r| r.id == reaction_id) {
            return false;
        }

        // The interned form of `Reaction::same_slot`: the set is keyed by
        // (author, emoji), so a second event from the same author carrying the
        // same emoji is the reaction we already hold, not another one.
        let author_idx = interner.intern(&reaction.author_id);
        let emoji = reaction.emoji.as_str();
        if self.reactions.iter().any(|r| r.author_idx == author_idx && r.emoji(interner) == emoji) {
            return false;
        }

        // Distinct-emoji ceiling: joining an existing group is always fine,
        // but a reaction opening a group past the cap is refused — the same
        // bound the UI draws, enforced against hostile inbound too.
        if !self.reactions.iter().any(|r| r.emoji(interner) == emoji) {
            let mut groups: [&str; MAX_REACTION_GROUPS] = [""; MAX_REACTION_GROUPS];
            let mut n = 0;
            for r in self.reactions.iter() {
                let e = r.emoji(interner);
                if !groups[..n].contains(&e) {
                    if n == MAX_REACTION_GROUPS {
                        return false;
                    }
                    groups[n] = e;
                    n += 1;
                }
            }
            if n >= MAX_REACTION_GROUPS {
                return false;
            }
        }

        let compact = CompactReaction {
            id: reaction_id,
            emoji_idx: interner.intern_emoji(emoji, reaction.emoji_url.as_deref()),
            author_idx,
        };
        self.reactions.push(compact);
        true
    }

    /// Remove a reaction by its hex event id. Returns true if one was removed.
    pub fn remove_reaction(&mut self, reaction_id: &str) -> bool {
        let target = hex_to_bytes_32(reaction_id);
        if !self.reactions.iter().any(|r| r.id == target) {
            return false;
        }
        self.reactions.retain(|r| r.id != target);
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
/// Messages are stored sorted by timestamp. A separate index of positions, ordered by
/// each message's id, gives O(log n) lookup by binary search through the positions.
#[derive(Clone, Debug, Default)]
pub struct CompactMessageVec {
    /// Messages sorted by timestamp (ascending)
    messages: Vec<CompactMessage>,
    /// Positions in `messages`, sorted by the id at each
    id_index: Vec<u32>,
}

/// Room for `additional` more, growing by an eighth (at least 8) when full: a large
/// chat's next live message must not double its buffers.
fn reserve_slack<T>(v: &mut Vec<T>, additional: usize) {
    if v.capacity() - v.len() < additional {
        v.reserve_exact(additional.max(v.len() / 8).max(8));
    }
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

    /// Get a mutable reference to all messages. Call `rebuild_index()` after changing
    /// any id or the order.
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

    /// Binary search of the index: `Ok(rank)` where `id` is held, else where it would go.
    /// A stale position (between a bulk change and `rebuild_index`) sorts last rather than panicking.
    #[inline]
    fn search(&self, id: &[u8; 32]) -> Result<usize, usize> {
        let messages = &self.messages;
        self.id_index.binary_search_by(|&pos| {
            messages.get(pos as usize).map_or(std::cmp::Ordering::Greater, |m| m.id.cmp(id))
        })
    }

    /// Find a message by ID using binary search - O(log n)
    pub fn find_by_id(&self, id: &[u8; 32]) -> Option<&CompactMessage> {
        let rank = self.search(id).ok()?;
        self.messages.get(self.id_index[rank] as usize)
    }

    /// Find a message by ID (mutable) - O(log n)
    pub fn find_by_id_mut(&mut self, id: &[u8; 32]) -> Option<&mut CompactMessage> {
        let rank = self.search(id).ok()?;
        let pos = self.id_index[rank] as usize;
        self.messages.get_mut(pos)
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
        self.search(id).is_ok()
    }

    /// Remove a message by hex ID string. Returns true if removed.
    pub fn remove_by_hex_id(&mut self, id_str: &str) -> bool {
        if id_str.is_empty() {
            return false;
        }
        self.remove_by_id(&encode_message_id(id_str))
    }

    /// Remove a message by binary ID. Returns true if removed.
    pub fn remove_by_id(&mut self, id: &[u8; 32]) -> bool {
        let Ok(rank) = self.search(id) else { return false };
        let msg_pos = self.id_index.remove(rank);
        self.messages.remove(msg_pos as usize);
        // Positions past the hole shift down by one; the id order is untouched.
        for pos in &mut self.id_index {
            if *pos > msg_pos {
                *pos -= 1;
            }
        }
        true
    }

    /// Give a message a new id in place (a pending send taking its event id), moving only
    /// its own index entry. Returns false if `old` isn't held or `new` already is.
    pub fn rekey(&mut self, old: &[u8; 32], new: [u8; 32]) -> bool {
        if old == &new {
            return self.contains_id(old);
        }
        let Ok(old_rank) = self.search(old) else { return false };
        let Err(new_rank) = self.search(&new) else { return false };
        let msg_pos = self.id_index.remove(old_rank);
        let new_rank = if new_rank > old_rank { new_rank - 1 } else { new_rank };
        self.id_index.insert(new_rank, msg_pos);
        self.messages[msg_pos as usize].id = new;
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
    /// **Performance**: append costs a binary search plus one index shift (ids are random, so
    /// the shift averages half the index); an out-of-order insert is O(n).
    pub fn insert(&mut self, msg: CompactMessage) -> bool {
        let Err(rank) = self.search(&msg.id) else { return false };
        reserve_slack(&mut self.messages, 1);
        reserve_slack(&mut self.id_index, 1);

        // Fast path: append if message is newer than or equal to last (common case)
        if self.messages.last().is_none_or(|last| msg.at >= last.at) {
            self.id_index.insert(rank, self.messages.len() as u32);
            self.messages.push(msg);
            return true;
        }

        // Slow path: out-of-order insert (rare for real-time chat)
        let msg_pos = match self.messages.binary_search_by(|m| m.at.cmp(&msg.at)) {
            Ok(pos) => pos,
            Err(pos) => pos,
        } as u32;

        // Positions at or past the insert shift up; the id order is untouched - O(n)
        for pos in &mut self.id_index {
            if *pos >= msg_pos {
                *pos += 1;
            }
        }
        self.messages.insert(msg_pos as usize, msg);
        self.id_index.insert(rank, msg_pos);
        true
    }

    /// Rebuild the ID index (call after bulk modifications)
    pub fn rebuild_index(&mut self) {
        let messages = &self.messages;
        let mut index: Vec<u32> = (0..messages.len() as u32).collect();
        index.sort_unstable_by(|&a, &b| messages[a as usize].id.cmp(&messages[b as usize].id).then(a.cmp(&b)));
        self.id_index = index;
    }

    /// The positions in `range`, ordered by id.
    fn ranked(&self, range: std::ops::Range<usize>) -> Vec<u32> {
        let messages = &self.messages;
        let mut fresh: Vec<u32> = (range.start as u32..range.end as u32).collect();
        fresh.sort_unstable_by(|&a, &b| messages[a as usize].id.cmp(&messages[b as usize].id).then(a.cmp(&b)));
        fresh
    }

    /// Merge id-ordered `fresh` positions into the index in O(n + k).
    fn merge_into_index(&mut self, fresh: Vec<u32>) {
        let messages = &self.messages;
        let id = |pos: u32| &messages[pos as usize].id;
        let old = std::mem::take(&mut self.id_index);
        let mut merged = Vec::with_capacity(old.len() + fresh.len());
        let (mut a, mut b) = (old.into_iter().peekable(), fresh.into_iter().peekable());
        loop {
            let take_old = match (a.peek(), b.peek()) {
                (Some(&o), Some(&n)) => id(o) < id(n),
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            merged.extend(if take_old { a.next() } else { b.next() });
        }
        self.id_index = merged;
    }

    /// Batch insert messages - optimized for different scenarios.
    ///
    /// Returns the number of messages actually added (excludes duplicates).
    ///
    /// **Performance**:
    /// - Append case (newer msgs): O(k log k + n)
    /// - Prepend case (older msgs): O(k log k + n)
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
            // All new messages are NEWER - append path (common for real-time + catch-up).
            // Appended positions are past every existing one, so none shifts.
            to_add.sort_by_key(|m| m.at);
            let base = self.messages.len();
            reserve_slack(&mut self.messages, to_add.len());
            self.messages.extend(to_add);
            let fresh = self.ranked(base..self.messages.len());
            self.merge_into_index(fresh);
        } else if their_max <= our_first.unwrap() {
            // All new messages are OLDER - prepend path (common for pagination)
            to_add.sort_by_key(|m| m.at);
            let prepend_count = to_add.len();
            for pos in &mut self.id_index {
                *pos += prepend_count as u32;
            }
            let mut joined = Vec::with_capacity(prepend_count + self.messages.len());
            joined.extend(to_add);
            joined.append(&mut self.messages);
            self.messages = joined;
            let fresh = self.ranked(0..prepend_count);
            self.merge_into_index(fresh);
        } else {
            // Mixed timestamps - fall back to full sort
            reserve_slack(&mut self.messages, to_add.len());
            self.messages.extend(to_add);
            self.messages.sort_by_key(|m| m.at);
            self.rebuild_index();
        }

        added
    }

    /// Heap held by the two vectors, at capacity (not what the messages own)
    pub fn memory_usage(&self) -> usize {
        self.messages.capacity() * std::mem::size_of::<CompactMessage>()
            + self.id_index.capacity() * std::mem::size_of::<u32>()
            // Note: doesn't include heap allocations inside CompactMessage
    }

    /// Keep the newest `keep` messages plus any still pending (a pending send exists nowhere
    /// else until it lands), then hand the freed capacity back. Returns how many went.
    pub fn retain_newest(&mut self, keep: usize) -> usize {
        let n = self.messages.len();
        if n <= keep {
            return 0;
        }
        let cut = n - keep;
        let mut i = 0;
        self.messages.retain(|m| {
            let kept = i >= cut || m.is_pending();
            i += 1;
            kept
        });
        self.messages.shrink_to_fit();
        self.rebuild_index();
        n - self.messages.len()
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

    /// Clear all messages, releasing both buffers
    pub fn clear(&mut self) {
        self.messages = Vec::new();
        self.id_index = Vec::new();
    }
}

// ============================================================================
// Conversion from/to Message
// ============================================================================

use crate::types::Message;

impl CompactMessage {
    /// Convert from a regular Message (borrowed), interning npubs
    pub fn from_message(msg: &Message, interner: &mut NpubInterner) -> Self {
        Self::from_message_owned(
            Message {
                attachments: Vec::new(),
                reactions: Vec::new(),
                ..msg.clone()
            },
            interner,
        )
        .with_parts(
            msg.attachments.iter().map(CompactAttachment::from_attachment).collect(),
            msg.reactions.iter().map(|r| CompactReaction::from_reaction(r, interner)).collect(),
        )
    }

    fn with_parts(mut self, attachments: TinyVec<CompactAttachment>, reactions: TinyVec<CompactReaction>) -> Self {
        self.attachments = attachments;
        self.reactions = reactions;
        self
    }

    /// Convert from a regular Message (owned), moving its strings rather than copying them.
    pub fn from_message_owned(msg: Message, interner: &mut NpubInterner) -> Self {
        let reply = (!msg.replied_to.is_empty()).then(|| {
            Box::new(CompactReply {
                id: hex_to_bytes_32(&msg.replied_to),
                npub_idx: interner.intern_opt(msg.replied_to_npub.as_deref()),
                content: msg.replied_to_content.map(String::into_boxed_str),
                emoji_tags: msg.replied_to_emoji_tags.as_deref().map_or_else(TinyVec::new, |t| intern_emoji_tags(t, interner)),
            })
        });
        let history = msg.edit_history.unwrap_or_default();
        let tail_is_content = history.last().is_some_and(|e| e.content == msg.content);
        let tail = history.len().wrapping_sub(1);
        let extras = MessageExtras {
            edits: history
                .into_iter()
                .enumerate()
                .map(|(i, e)| CompactEdit {
                    content: if tail_is_content && i == tail { Box::default() } else { e.content.into_boxed_str() },
                    edited_at: e.edited_at,
                })
                .collect(),
            preview: msg.preview_metadata.as_ref().map_or_else(TinyVec::new, pack_preview),
            emoji_tags: intern_emoji_tags(&msg.emoji_tags, interner),
            addressed_bots: msg.addressed_bots.iter().map(|n| interner.intern(n)).collect(),
            spans: hold_spans(msg.text_spans),
            role_mentions: msg.role_mentions.iter().filter_map(|id| crate::simd::hex::hex_to_bytes_32_checked(id)).collect(),
            tail_is_content,
        };
        Self {
            id: encode_message_id(&msg.id),
            at: timestamp_to_compact(msg.at),
            content: msg.content.into_boxed_str(),
            attachments: msg.attachments.into_iter().map(CompactAttachment::from_attachment_owned).collect(),
            reactions: msg.reactions.into_iter().map(|r| CompactReaction::from_reaction_owned(r, interner)).collect(),
            reply,
            extras: (!extras.is_empty()).then(|| Box::new(extras)),
            expiration_secs: msg.expiration.map(|e| e as u32).unwrap_or(0),
            npub_idx: interner.intern_opt(msg.npub.as_deref()),
            flags: MessageFlags::from_all(msg.mine, msg.pending, msg.failed, msg.replied_to_has_attachment),
        }
    }

    /// Convert back to a regular Message, resolving npubs and emoji from the interner
    pub fn to_message(&self, interner: &NpubInterner) -> Message {
        let id = self.id_hex();
        let reactions = self.reactions.iter().map(|r| r.to_reaction(&id, interner)).collect();
        let reply = self.reply.as_deref();
        let extras = self.extras.as_deref();
        Message {
            id,
            reactions,
            at: self.timestamp_ms(),
            expiration: if self.expiration_secs == 0 { None } else { Some(self.expiration_secs as u64) },
            mine: self.flags.is_mine(),
            pending: self.flags.is_pending(),
            failed: self.flags.is_failed(),
            edited: self.is_edited(),
            npub: interner.resolve(self.npub_idx).map(|s| s.to_string()),
            replied_to: self.replied_to_hex(),
            replied_to_content: reply.and_then(|r| r.content.as_deref()).map(str::to_string),
            replied_to_emoji_tags: reply.filter(|r| !r.emoji_tags.is_empty()).map(|r| resolve_emoji_tags(&r.emoji_tags, interner)),
            replied_to_npub: reply.and_then(|r| interner.resolve(r.npub_idx)).map(|s| s.to_string()),
            replied_to_has_attachment: self.flags.replied_to_has_attachment(),
            // Re-resolved per get_message_views / populate_reply_context; the compact
            // form keeps only the bool, so this stays None on the RAM path.
            replied_to_attachment_extension: None,
            // Never held in memory: every first save carries the original, and a re-save
            // keeps the stored link (`save_event` COALESCEs it).
            wrapper_event_id: None,
            content: self.content.to_string(),
            attachments: self.attachments.iter().map(|a| a.to_attachment()).collect(),
            edit_history: extras.filter(|x| !x.edits.is_empty()).map(|x| x.edit_history(&self.content)),
            preview_metadata: extras.and_then(|x| unpack_preview(x.preview.as_slice())),
            emoji_tags: extras.map(|x| resolve_emoji_tags(&x.emoji_tags, interner)).unwrap_or_default(),
            addressed_bots: extras
                .map(|x| x.addressed_bots.iter().filter_map(|&i| interner.resolve(i).map(|s| s.to_string())).collect())
                .unwrap_or_default(),
            text_spans: extras.map(|x| x.spans.iter().map(HeldSpan::to_span).collect()).unwrap_or_default(),
            role_mentions: extras
                .map(|x| x.role_mentions.iter().map(crate::simd::hex::bytes_to_hex_32).collect())
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_new_id_keeps_the_original_hash_and_path() {
        let ox = "ab".repeat(32);
        let att = crate::types::Attachment {
            id: ox.clone(),
            extension: "png".into(),
            path: "/elsewhere/file.png".into(),
            original_hash: Some(ox.clone()),
            ..Default::default()
        };
        let mut c = CompactAttachment::from_attachment(&att);
        c.set_id([0xcd; 32]);
        let back = c.to_attachment();
        assert_eq!(back.id, "cd".repeat(32));
        assert_eq!(back.original_hash.as_deref(), Some(ox.as_str()), "the original hash is not the new id");
        assert_eq!(back.path, "/elsewhere/file.png");
    }
    use super::*;

    // A real event id that happens to start with the pending marker byte must
    // round-trip EXACTLY — the marker alone once misread 1 in 256 real ids as
    // phantom "pending-…" strings, wedging read markers among other consumers.
    #[test]
    fn real_id_starting_with_marker_byte_roundtrips() {
        let id = "01b95c05179c6d6abbc60b9a35198fdee6e0ce5b80a0b7ac8d465d81d038a73d";
        let encoded = encode_message_id(id);
        assert_eq!(decode_message_id(&encoded), id);
    }

    #[test]
    fn pending_id_roundtrips() {
        let id = "pending-306878031314";
        let encoded = encode_message_id(id);
        assert_eq!(encoded[0], PENDING_ID_MARKER);
        assert_eq!(decode_message_id(&encoded), id);
    }

    #[test]
    fn marker_byte_without_sentinel_decodes_as_hex() {
        // Marker byte + arbitrary tail (no sentinel) is a real id, not pending.
        let mut bytes = [0xABu8; 32];
        bytes[0] = PENDING_ID_MARKER;
        let decoded = decode_message_id(&bytes);
        assert!(!decoded.starts_with("pending-"));
        assert_eq!(decoded.len(), 64);
    }

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
            expiration_secs: 0,
            flags: MessageFlags::NONE,
            npub_idx: interner.intern("npub1test"),
            content: "First message".to_string().into_boxed_str(),
            attachments: TinyVec::new(),
            reactions: TinyVec::new(),
            reply: None,
            extras: None,
        };

        let msg2 = CompactMessage {
            id: hex_to_bytes_32("0000000000000000000000000000000000000000000000000000000000000002"),
            at: 2000,
            expiration_secs: 0,
            flags: MessageFlags::MINE,
            npub_idx: interner.intern("npub1me"),
            content: "Second message".to_string().into_boxed_str(),
            attachments: TinyVec::new(),
            reactions: TinyVec::new(),
            reply: None,
            extras: None,
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
            expiration_secs: 0,
            flags: MessageFlags::NONE,
            npub_idx: NO_NPUB,
            content: "Test".to_string().into_boxed_str(),
            attachments: TinyVec::new(),
            reactions: TinyVec::new(),
            reply: None,
            extras: None,
        };

        assert!(vec.insert(msg.clone()));
        assert!(!vec.insert(msg)); // Duplicate rejected
        assert_eq!(vec.len(), 1);
    }

    /// Comprehensive benchmark test for memory reduction and performance
    #[test]
    fn benchmark_compact_vs_message() {
        use web_time::Instant;

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
                    expiration: None,
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
                    replied_to_emoji_tags: None,
                    replied_to_npub: if i > 0 && i % 5 == 0 {
                        Some(users[(i - 1) % NUM_UNIQUE_USERS].clone())
                    } else {
                        None
                    },
                    replied_to_has_attachment: None,
                    replied_to_attachment_extension: None,
                    wrapper_event_id: Some(format!("{:0>64x}", i + 1000000)),
                    content: format!("This is message number {} with some typical content length.", i),
                    attachments: vec![],
                    reactions: vec![],
                    edit_history: None,
                    preview_metadata: None,
                    emoji_tags: Vec::new(),
                    addressed_bots: Vec::new(),
                    text_spans: Vec::new(),
                    role_mentions: Vec::new(),
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
        let compact_heap_estimate: usize = compact_vec.iter()
            .map(|m| crate::stats::DeepSize::deep_size(m) - std::mem::size_of::<CompactMessage>())
            .sum();
        let compact_struct_mem = compact_vec.len() * std::mem::size_of::<CompactMessage>();
        let compact_index_mem = compact_vec.len() * std::mem::size_of::<u32>();
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
        use web_time::Instant;
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
        // A real event ID starting with 0x01 must NOT be confused with pending:
        // 1 in 256 real ids begin with the marker byte, and misreading them
        // mangled read markers into phantom "pending-…" strings. Only the
        // marker + sentinel combination reads as pending.
        let hex = "0100000000000000000000000000000000000000000000000000000000000000";
        let encoded = encode_message_id(hex);
        let decoded = decode_message_id(&encoded);
        assert_eq!(decoded, hex, "marker-leading real id must roundtrip exactly");
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
            expiration_secs: 0,
            flags: MessageFlags::NONE,
            npub_idx: NO_NPUB,
            content: "test".into(),
            attachments: TinyVec::new(),
            reactions: TinyVec::new(),
            reply: None,
            extras: None,
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
    fn compact_vec_append_batch_merge_keeps_index_consistent() {
        // Seed older messages, then append a large batch of NEWER ones — exercises the
        // O(n+k) append-merge index build. Every id must stay findable (proves the merged
        // id_index is complete + sorted) and dedup must still work off it.
        let mut vec = CompactMessageVec::new();
        let mk = |n: u32, at: u64| make_compact_msg(&format!("{:064x}", n), at);
        let seed: Vec<_> = (0..50).map(|n| mk(n, n as u64)).collect();
        assert_eq!(vec.insert_batch(seed), 50);
        let batch: Vec<_> = (1000..1200).map(|n| mk(n, n as u64)).collect();
        assert_eq!(vec.insert_batch(batch), 200, "all newer messages appended");
        assert_eq!(vec.len(), 250);
        for n in (0..50).chain(1000..1200) {
            assert!(vec.find_by_hex_id(&format!("{:064x}", n)).is_some(), "id {n} must be findable via the merged index");
        }
        assert!(vec.find_by_hex_id(&format!("{:064x}", 9999u32)).is_none(), "never-inserted id must not be found");
        let dup: Vec<_> = (1000..1200).map(|n| mk(n, n as u64)).collect();
        assert_eq!(vec.insert_batch(dup), 0, "re-appended batch fully deduped via the merged index");
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
        let others = ["bb00000000000000000000000000000000000000000000000000000000000000", "cc00000000000000000000000000000000000000000000000000000000000000"];
        vec.insert(make_compact_msg(old_id, 100));
        vec.insert(make_compact_msg(others[0], 200));
        vec.insert(make_compact_msg(others[1], 300));

        // Mutate the message's ID directly (simulating an ID update like pending -> confirmed):
        // it now sorts last but sits first in the index until a rebuild.
        vec.messages_mut()[0].id = encode_message_id(new_id);

        // Rebuild index
        vec.rebuild_index();
        assert!(vec.contains_hex_id(new_id), "after rebuild, new ID should be found");
        assert!(!vec.contains_hex_id(old_id), "after rebuild, old ID should not be found");
        assert!(others.iter().all(|id| vec.contains_hex_id(id)), "the untouched ids still resolve");
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
            expiration: Some(1893456000),
            id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
            content: "Hello, world!".into(),
            replied_to: "1111111111111111111111111111111111111111111111111111111111111111".into(),
            replied_to_content: Some("Original message".into()),
            replied_to_emoji_tags: Some(vec![crate::types::EmojiTag {
                shortcode: "catJAM".into(),
                url: "https://example.com/catjam.png".into(),
            }]),
            replied_to_npub: Some("npub1replier".into()),
            replied_to_has_attachment: Some(true),
            replied_to_attachment_extension: None,
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
                fallback_urls: Vec::new(),
            }],
            reactions: vec![Reaction {
                id: "dddd000000000000000000000000000000000000000000000000000000000000".into(),
                reference_id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
                author_id: "npub1reactor".into(),
                emoji: "\u{1f44d}".into(), // thumbs up
                emoji_url: None,
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
            emoji_tags: Vec::new(),
            addressed_bots: vec!["npub1botrouting0000000000000000000000000000000000000000000000".into()],
            role_mentions: vec!["ab".repeat(32), "cd".repeat(32)],
            text_spans: vec![crate::text_spans::TextSpan::Color(crate::text_color::ColorSpan { from: 0, to: 2, effect: crate::text_color::Effect::Rainbow, colors: Vec::new() })],
        }
    }

    #[test]
    fn compact_add_reaction_rejects_a_resend_under_a_new_event_id() {
        let mut interner = NpubInterner::new();
        let mut compact = CompactMessage::from_message(&Message::default(), &mut interner);
        // Real hex — the id check would mask the slot check if these collapsed.
        let slot = |id: String, emoji: &str| Reaction {
            id,
            reference_id: "a".repeat(64),
            author_id: "npub1author".to_string(),
            emoji: emoji.to_string(),
            emoji_url: None,
        };
        assert!(compact.add_reaction(slot("1".repeat(64), "\u{1F44D}"), &mut interner));
        assert!(
            !compact.add_reaction(slot("2".repeat(64), "\u{1F44D}"), &mut interner),
            "distinct event ids but one (author, emoji) slot — this is the double-count"
        );
        assert_eq!(compact.reactions.len(), 1);
        assert!(
            compact.add_reaction(slot("3".repeat(64), "\u{2764}\u{FE0F}"), &mut interner),
            "a different emoji from the same author is its own reaction"
        );
        assert_eq!(compact.reactions.len(), 2);
    }

    #[test]
    fn a_message_caps_at_twelve_reaction_groups_but_joins_stay_open() {
        let mut interner = NpubInterner::new();
        let mut compact = CompactMessage::from_message(&Message::default(), &mut interner);
        let react = |i: usize, author: &str, emoji: &str| Reaction {
            id: format!("{:064x}", i + 1),
            reference_id: "a".repeat(64),
            author_id: author.to_string(),
            emoji: emoji.to_string(),
            emoji_url: None,
        };
        for i in 0..MAX_REACTION_GROUPS {
            assert!(
                compact.add_reaction(react(i, "npub1opener", &format!("emoji{i}")), &mut interner),
                "group {i} fits under the ceiling"
            );
        }
        assert!(
            !compact.add_reaction(react(100, "npub1opener", "emoji-thirteen"), &mut interner),
            "a thirteenth distinct emoji is refused"
        );
        assert!(
            compact.add_reaction(react(101, "npub1joiner", "emoji0"), &mut interner),
            "joining an existing group is never bounded by the ceiling"
        );
        assert_eq!(compact.reactions.len(), MAX_REACTION_GROUPS + 1);
    }

    /// The hot structs' footprint, pinned so a new field can't quietly grow every message.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn compact_layouts_hold_their_size() {
        assert_eq!(std::mem::size_of::<CompactMessage>(), 96);
        assert_eq!(std::mem::size_of::<CompactReaction>(), 40);
        assert_eq!(std::mem::size_of::<TinyVec<CompactReaction>>(), 8);
        assert_eq!(std::mem::size_of::<CompactAttachment>(), 144);
        assert_eq!(std::mem::size_of::<MessageExtras>(), 56);
        assert_eq!(std::mem::size_of::<CompactEdit>(), 24);
        assert_eq!(std::mem::size_of::<HeldSpan>(), 32);
    }

    #[test]
    fn targeted_rekey_and_remove_match_a_full_rebuild() {
        let mut interner = NpubInterner::new();
        let id = |n: u64| {
            let mut b = [0u8; 32];
            b[..8].copy_from_slice(&n.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_le_bytes());
            b
        };
        let mut v = CompactMessageVec::new();
        for n in 0..200u64 {
            let msg = Message { id: bytes_to_hex_32(&id(n)), at: n * 10, ..Default::default() };
            v.insert(CompactMessage::from_message(&msg, &mut interner));
        }
        let check = |v: &CompactMessageVec| {
            let mut rebuilt = v.clone();
            rebuilt.rebuild_index();
            assert_eq!(v.id_index, rebuilt.id_index);
        };
        for n in (0..200u64).step_by(7) {
            assert!(v.rekey(&id(n), id(n + 10_000)));
            check(&v);
        }
        assert!(!v.rekey(&id(1), id(2)), "a held id is never overwritten");
        for n in (3..200u64).step_by(11).filter(|n| n % 7 != 0) {
            assert!(v.remove_by_id(&id(n)));
            check(&v);
        }
        assert!(v.find_by_id(&id(10_007)).is_some_and(|m| m.at == 70));
    }

    #[test]
    fn a_group_grows_past_255_reactions() {
        let mut interner = NpubInterner::new();
        let mut compact = CompactMessage::from_message(&Message::default(), &mut interner);
        let react = |i: usize| Reaction {
            id: format!("{:064x}", i + 1),
            reference_id: "a".repeat(64),
            author_id: format!("npub1joiner{i}"),
            emoji: "👍".to_string(),
            emoji_url: None,
        };
        for i in 0..300 {
            assert!(compact.add_reaction(react(i), &mut interner), "reaction {i} lands");
        }
        assert_eq!(compact.reactions.len(), 300);
        let loaded = Message { reactions: (0..300).map(react).collect(), ..Default::default() };
        assert_eq!(CompactMessage::from_message(&loaded, &mut interner).reactions.len(), 300, "history loads whole");
        compact.remove_reaction(&format!("{:064x}", 1));
        assert_eq!(compact.reactions.len(), 299);
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
        assert_eq!(restored.wrapper_event_id, None, "the wrapper link lives in the DB, not in memory");
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
        // Bot routing targets round-trip through the interner.
        assert_eq!(restored.addressed_bots, msg.addressed_bots, "addressed_bots mismatch");
        assert_eq!(restored.role_mentions, msg.role_mentions, "role_mentions mismatch");
    }

    // The compact form deliberately keeps only the "replied-to has an attachment"
    // bool, so the extension does NOT survive a trip through STATE. Every path
    // that emits a message read back out of RAM has to re-run
    // populate_reply_context, or a quoted "GIF Animation" renders as a generic
    // "Attachment" the moment a reaction re-emits the row.
    #[test]
    fn compact_drops_replied_to_attachment_extension() {
        let mut msg = make_full_message();
        msg.replied_to_has_attachment = Some(true);
        msg.replied_to_attachment_extension = Some("gif".to_string());

        let mut interner = NpubInterner::new();
        let restored = CompactMessage::from_message(&msg, &mut interner).to_message(&interner);

        assert_eq!(restored.replied_to_has_attachment, Some(true), "the bool survives RAM");
        assert_eq!(
            restored.replied_to_attachment_extension, None,
            "the extension does not survive RAM — emitters must re-resolve it"
        );
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
                    emoji_url: None,
                },
                Reaction {
                    id: "bbb0000000000000000000000000000000000000000000000000000000000000".into(),
                    reference_id: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".into(),
                    author_id: "npub1bob".into(),
                    emoji: "\u{1f525}".into(), // fire
                    emoji_url: None,
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
            fallback_urls: Vec::new(),
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
            fallback_urls: Vec::new(),
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
            key: "".into(), // empty key = legacy derived
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
        // Legacy short nonce is 12 bytes = 24 hex chars
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

        assert!(!compact.has_img_meta());
        assert!(compact.group_id().is_none());
        assert!(compact.original_hash().is_none());
        assert!(compact.webxdc_topic().is_none());

        let restored = compact.to_attachment();
        assert!(restored.img_meta.is_none());
        assert!(restored.group_id.is_none());
        assert!(restored.original_hash.is_none());
        assert!(restored.webxdc_topic.is_none());
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
            fallback_urls: Vec::new(),
        };

        let compact = CompactAttachment::from_attachment(&att);

        assert!(compact.has_img_meta());
        assert!(compact.group_id().is_some());
        assert!(compact.original_hash().is_some());
        assert!(compact.webxdc_topic().is_some());

        let restored = compact.to_attachment();
        let meta = restored.img_meta.unwrap();
        assert_eq!(meta.thumbhash, "hash123");
        assert_eq!(meta.width, 1920);
        assert_eq!(meta.height, 1080);
        assert_eq!(restored.webxdc_topic, Some("game-state".into()));
        assert_eq!(restored.group_id.unwrap(), att.group_id.unwrap());
        assert_eq!(restored.original_hash.unwrap(), att.original_hash.unwrap());
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
            emoji_url: None,
        };
        let mut interner = NpubInterner::new();
        let compact = CompactReaction::from_reaction(&reaction, &mut interner);
        let restored = compact.to_reaction(&reaction.reference_id, &interner);

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
            emoji_url: None,
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
            emoji_url: None,
        };
        let mut interner = NpubInterner::new();
        let compact = CompactReaction::from_reaction(&reaction, &mut interner);
        let restored = compact.to_reaction(&reaction.reference_id, &interner);
        assert_eq!(restored.emoji, "\u{1f431}\u{200d}\u{1f4bb}", "complex unicode emoji should roundtrip");
    }

    #[test]
    fn compact_reaction_custom_emoji() {
        let reaction = Reaction {
            id: "aaaa000000000000000000000000000000000000000000000000000000000000".into(),
            reference_id: "bbbb000000000000000000000000000000000000000000000000000000000000".into(),
            author_id: "npub1test".into(),
            emoji: ":cat_heart_eyes:".into(),
            emoji_url: None,
        };
        let mut interner = NpubInterner::new();
        let compact = CompactReaction::from_reaction(&reaction, &mut interner);
        let restored = compact.to_reaction(&reaction.reference_id, &interner);
        assert_eq!(restored.emoji, ":cat_heart_eyes:", "custom emoji shortcode should roundtrip");
    }

    #[test]
    fn compact_reaction_owned_conversion() {
        let reaction = Reaction {
            id: "aaaa000000000000000000000000000000000000000000000000000000000000".into(),
            reference_id: "bbbb000000000000000000000000000000000000000000000000000000000000".into(),
            author_id: "npub1bob".into(),
            emoji: "\u{1f44d}".into(), // thumbs up
            emoji_url: None,
        };
        let reaction_clone = reaction.clone();
        let mut interner = NpubInterner::new();
        let compact = CompactReaction::from_reaction_owned(reaction, &mut interner);
        let restored = compact.to_reaction(&reaction_clone.reference_id, &interner);

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

    // ========================================================================
    // Resident representation: exact round trips
    // ========================================================================

    fn hex64(n: u64) -> String {
        format!("{:064x}", n.wrapping_mul(0x9E37_79B9_7F4A_7C15))
    }

    fn att_with(id: &str, ext: &str, name: &str, path: String) -> Attachment {
        Attachment {
            id: id.into(),
            key: hex64(7),
            nonce: "aabbccddaabbccddaabbccddaabbccdd".into(),
            extension: ext.into(),
            name: name.into(),
            url: format!("https://blossom.example/{id}"),
            path,
            size: 4242,
            img_meta: None,
            downloading: false,
            downloaded: true,
            webxdc_topic: None,
            group_id: None,
            original_hash: Some(id.into()),
            fallback_urls: Vec::new(),
        }
    }

    fn path_kind_of(att: &Attachment) -> u8 {
        CompactAttachment::from_attachment(att).flags.path_kind()
    }

    /// Both conversions, then back: the boundary type must come out identical.
    fn assert_exact(msg: &Message) {
        let mut interner = NpubInterner::new();
        let borrowed = CompactMessage::from_message(msg, &mut interner).to_message(&interner);
        assert_eq!(&borrowed, msg, "from_message round trip");
        let owned = CompactMessage::from_message_owned(msg.clone(), &mut interner).to_message(&interner);
        assert_eq!(&owned, msg, "from_message_owned round trip");
    }

    /// A message whose every field survives RAM: the wrapper link and quoted extension
    /// never do, and a reaction's reference is its parent.
    fn rich_message() -> Message {
        let id = hex64(1);
        let mut msg = make_full_message();
        msg.id = id.clone();
        msg.wrapper_event_id = None;
        msg.replied_to_attachment_extension = None;
        msg.content = "Edited".into();
        for r in &mut msg.reactions {
            r.reference_id = id.clone();
        }
        msg.attachments = vec![att_with(&hex64(2), "png", "photo.png", format!("{}photo.png", download_prefix()))];
        msg
    }

    #[test]
    fn a_rich_message_round_trips_exactly() {
        assert_exact(&rich_message());
        assert_exact(&Message { id: hex64(3), content: "plain".into(), at: 1_700_000_000_123, ..Default::default() });
    }

    #[test]
    fn every_path_kind_round_trips_exactly() {
        let id = hex64(10);
        let prefix = download_prefix();
        let cases = [
            (String::new(), 0, "empty"),
            (format!("{prefix}{id}.jpg"), AttachmentFlags::PATH_HASHED, "hashed"),
            (format!("{prefix}holiday.jpg"), AttachmentFlags::PATH_NAMED, "named"),
            (format!("{prefix}holiday-1.jpg"), AttachmentFlags::PATH_CUSTOM, "collision suffix"),
            (format!("{prefix}{}.jpg", id.to_uppercase()), AttachmentFlags::PATH_CUSTOM, "uppercase hash"),
            (format!("{prefix}{id}.png"), AttachmentFlags::PATH_CUSTOM, "hash with another extension"),
            (format!("{prefix}{}.jpg", hex64(11)), AttachmentFlags::PATH_CUSTOM, "another hash"),
            (format!("{prefix}{id}.jpg.part"), AttachmentFlags::PATH_CUSTOM, "hashed plus a tail"),
            ("/elsewhere/holiday.jpg".to_string(), AttachmentFlags::PATH_CUSTOM, "another directory"),
            (format!("{prefix}sub/holiday.jpg"), AttachmentFlags::PATH_CUSTOM, "a subdirectory"),
        ];
        for (path, kind, label) in cases {
            let att = att_with(&id, "jpg", "holiday.jpg", path);
            assert_eq!(path_kind_of(&att), kind, "{label}");
            assert_eq!(CompactAttachment::from_attachment(&att).to_attachment(), att, "{label}");
            assert_eq!(CompactAttachment::from_attachment_owned(att.clone()).to_attachment(), att, "{label} (owned)");
        }
        // No name: the prefix alone is not a named path.
        let bare = att_with(&id, "jpg", "", prefix.to_string());
        assert_eq!(path_kind_of(&bare), AttachmentFlags::PATH_CUSTOM);
        assert_eq!(CompactAttachment::from_attachment(&bare).to_attachment(), bare);
        // A name equal to the hashed file name reproduces either way.
        let both = att_with(&id, "jpg", &format!("{id}.jpg"), format!("{prefix}{id}.jpg"));
        assert_eq!(CompactAttachment::from_attachment(&both).to_attachment(), both);
    }

    #[test]
    fn a_derived_path_follows_set_path() {
        let id = hex64(20);
        let mut c = CompactAttachment::from_attachment(&att_with(&id, "mp4", "clip.mp4", String::new()));
        assert!(!c.has_path());
        assert!(c.cold.is_none());
        c.set_path(format!("{}clip.mp4", download_prefix()));
        assert_eq!(c.flags.path_kind(), AttachmentFlags::PATH_NAMED);
        assert!(c.cold.is_none(), "a derived path needs no cold box");
        c.set_path("/custom/clip.mp4".into());
        assert_eq!(c.path(), "/custom/clip.mp4");
        assert!(c.cold.is_some());
        c.set_path(String::new());
        assert!(!c.has_path());
        assert_eq!(c.path(), "");
        assert!(c.cold.is_none(), "clearing the only cold field drops the box");
        assert!(c.downloaded(), "the path kind leaves the other flags alone");
    }

    #[test]
    fn cold_fields_round_trip_exactly() {
        let id = hex64(30);
        let mut att = att_with(&id, "tar.zst", "", "/var/x".into());
        att.original_hash = Some(hex64(31));
        att.group_id = Some(hex64(32));
        att.webxdc_topic = Some("ABCDEFGHIJKLMNOPQRSTUVWXYZ234567ABCDEFGHIJKLMNOPQRST".into());
        att.fallback_urls = vec!["https://a.example/x".into(), "https://b.example/x".into()];
        att.img_meta = Some(ImageMetadata { thumbhash: "ẞ-thumb".into(), width: 7, height: 9 });
        let c = CompactAttachment::from_attachment(&att);
        assert_eq!(c.ext, EXT_COLD);
        assert_eq!(c.to_attachment(), att);

        // Unknown or differently-cased extensions keep their exact spelling.
        for ext in ["JPG", "Png", "weird", ""] {
            let a = att_with(&id, ext, "", String::new());
            assert_eq!(CompactAttachment::from_attachment(&a).to_attachment(), a, "{ext:?}");
        }
        // An image with an empty thumbhash is still an image.
        let mut a = att_with(&id, "png", "", String::new());
        a.img_meta = Some(ImageMetadata { thumbhash: String::new(), width: 0, height: 0 });
        assert_eq!(CompactAttachment::from_attachment(&a).to_attachment(), a);
        // No original hash, and one that differs only from the id.
        a.original_hash = None;
        assert_eq!(CompactAttachment::from_attachment(&a).to_attachment(), a);
        assert!(CompactAttachment::from_attachment(&a).cold.is_none());
    }

    #[test]
    fn the_common_attachment_holds_nothing_cold() {
        let id = hex64(40);
        let att = att_with(&id, "webp", "", format!("{}{id}.webp", download_prefix()));
        let c = CompactAttachment::from_attachment(&att);
        assert!(c.cold.is_none(), "known extension, hashed path and original == id need no box");
        assert_eq!(c.to_attachment(), att);
        for ext in ["png", "mp4", "", "tar.zst"] {
            let a = att_with(&id, ext, "", format!("{}{id}.{ext}", download_prefix()));
            assert_eq!(path_kind_of(&a), AttachmentFlags::PATH_HASHED, "{ext:?}");
            assert_eq!(CompactAttachment::from_attachment(&a).to_attachment(), a, "{ext:?}");
        }
        assert_eq!(c.flags.0 & AttachmentFlags::ORIGINAL_IS_ID, AttachmentFlags::ORIGINAL_IS_ID);
        assert_eq!(c.original_hash(), Some(c.id));
    }

    #[test]
    fn every_extension_code_reads_back() {
        for (i, ext) in EXTENSIONS.iter().enumerate() {
            assert_eq!(extension_code(ext) as usize, i, "{ext} must appear once");
        }
        assert!(EXTENSIONS.len() < EXT_COLD as usize);
    }

    fn preview(title: Option<&str>, og_title: Option<&str>, desc: Option<&str>, og_desc: Option<&str>) -> SiteMetadata {
        SiteMetadata {
            domain: "example.com".into(),
            og_title: og_title.map(Into::into),
            og_description: og_desc.map(Into::into),
            og_image: Some("https://example.com/i.png".into()),
            og_url: None,
            og_type: Some("article".into()),
            title: title.map(Into::into),
            description: desc.map(Into::into),
            favicon: Some(String::new()),
        }
    }

    #[test]
    fn previews_round_trip_exactly() {
        let long = "ü".repeat(9000);
        let cases = [
            preview(Some("Same"), Some("Same"), Some("D"), Some("D")),
            preview(Some("Mine"), Some("Theirs"), Some("D1"), Some("D2")),
            preview(Some("Only title"), None, None, Some("Only og")),
            preview(None, Some("Only og"), Some("Only desc"), None),
            preview(None, None, None, None),
            preview(Some(""), Some(""), Some(""), None),
            preview(Some(&long), Some(&long), Some(&long[..200]), Some(&long)),
        ];
        for meta in cases {
            assert_eq!(unpack_preview(pack_preview(&meta).as_slice()).as_ref(), Some(&meta));
            let msg = Message { id: hex64(50), preview_metadata: Some(meta.clone()), ..Default::default() };
            assert_exact(&msg);
            let mut interner = NpubInterner::new();
            let mut c = CompactMessage::from_message(&Message { id: hex64(51), ..Default::default() }, &mut interner);
            c.set_preview_metadata(Some(meta.clone()));
            assert_eq!(c.preview_metadata(), Some(meta));
            c.set_preview_metadata(None);
            assert!(c.extras.is_none(), "removing the only extra drops the box");
        }
        let same = preview(Some("Same title"), Some("Same title"), Some("Same desc"), Some("Same desc"));
        let differ = preview(Some("Same title"), Some("Other title"), Some("Same desc"), Some("Other desc"));
        assert!(pack_preview(&same).len() + 18 < pack_preview(&differ).len(), "equal twins are stored once");
    }

    #[test]
    fn spans_round_trip_exactly() {
        use crate::text_spans::{ColorSpan, TextSpan, TimeSpan};
        use crate::text_color::Effect;
        let color = |colors: &[&str]| TextSpan::Color(ColorSpan {
            from: 1,
            to: 4,
            effect: Effect::Gradient,
            colors: colors.iter().map(|c| c.to_string()).collect(),
        });
        let spans = vec![
            color(&["#000000", "#ffffff", "#0a1b2c"]),
            color(&["#ABCDEF", "#abcdef"]),
            color(&["red"]),
            color(&["#abc"]),
            TextSpan::Color(ColorSpan { from: 0, to: 9, effect: Effect::Rainbow, colors: Vec::new() }),
            TextSpan::Time(TimeSpan { from: 2, to: 8, unix: -62_167_219_200, style: 'R' }),
        ];
        let held: Vec<_> = spans.iter().cloned().map(HeldSpan::hold).collect();
        assert!(matches!(held[0], HeldSpan::Color { .. }), "canonical stops pack");
        assert!(matches!(held[1], HeldSpan::Raw(_)), "a non-canonical stop keeps the span verbatim");
        assert!(matches!(held[2], HeldSpan::Raw(_)));
        assert!(matches!(held[3], HeldSpan::Raw(_)));
        let msg = Message { id: hex64(60), content: "0123456789".into(), text_spans: spans, ..Default::default() };
        assert_exact(&msg);
    }

    #[test]
    fn edit_history_round_trips_exactly() {
        let edits = |v: &[(&str, u64)]| Some(v.iter().map(|&(c, at)| EditEntry { content: c.into(), edited_at: at }).collect::<Vec<_>>());
        let base = |content: &str, history| Message { id: hex64(70), content: content.into(), at: 1000, edited: true, edit_history: history, ..Default::default() };
        let cases = [
            base("v2", edits(&[("v1", 1000), ("v2", 2000)])),
            base("v1", edits(&[("v1", 1000), ("v2", 2000)])),
            base("", edits(&[("", 1000)])),
            base("x", edits(&[("b", 3000), ("x", 1000)])),
            base("same", edits(&[("same", 1000), ("same", 2000)])),
        ];
        for msg in cases {
            assert_exact(&msg);
        }
        let mut interner = NpubInterner::new();
        let tail = CompactMessage::from_message(&base("v2", edits(&[("v1", 1000), ("v2", 2000)])), &mut interner);
        let x = tail.extras.as_deref().unwrap();
        assert!(x.tail_is_content && x.edits[1].content.is_empty(), "the newest revision is held once");
    }

    /// The edit rules on plain data, which the shared-tail history must reproduce.
    fn reference_apply(content: &mut String, history: &mut Vec<EditEntry>, at: u64, new: &str, edited_at: u64) {
        if history.is_empty() {
            history.push(EditEntry { content: content.clone(), edited_at: at });
        }
        if history.iter().any(|e| e.edited_at == edited_at) {
            return;
        }
        history.push(EditEntry { content: new.to_string(), edited_at });
        history.sort_by_key(|e| e.edited_at);
        if history.last().is_none_or(|e| e.edited_at == edited_at) {
            *content = new.to_string();
        }
    }

    #[test]
    fn apply_edit_matches_the_reference_rules() {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for round in 0..400 {
            let at = 1000 + next(5) * 1000;
            let mut content = format!("c{}", next(3));
            let mut history: Vec<EditEntry> = Vec::new();
            // Some messages arrive already edited, a few out of order.
            if next(3) == 0 {
                for _ in 0..1 + next(3) {
                    history.push(EditEntry { content: format!("c{}", next(3)), edited_at: next(8) * 1000 });
                }
                history.dedup_by_key(|e| e.edited_at);
                if next(2) == 0 {
                    content = history.last().unwrap().content.clone();
                }
            }
            let msg = Message {
                id: hex64(round),
                content: content.clone(),
                at,
                edited: !history.is_empty(),
                edit_history: (!history.is_empty()).then(|| history.clone()),
                ..Default::default()
            };
            let mut interner = NpubInterner::new();
            let mut compact = CompactMessage::from_message(&msg, &mut interner);
            for _ in 0..next(6) {
                let new = format!("c{}", next(3));
                let edited_at = next(8) * 1000;
                reference_apply(&mut content, &mut history, at, &new, edited_at);
                compact.apply_edit(new, edited_at, Vec::new(), Vec::new(), &mut interner);
                let out = compact.to_message(&interner);
                assert_eq!(out.content, content, "round {round}");
                assert_eq!(out.edit_history.unwrap_or_default(), history, "round {round}");
                assert_eq!(compact.edit_history(), history, "round {round}");
            }
        }
    }

    #[test]
    fn a_newest_edit_brings_its_emoji_and_spans() {
        use crate::text_spans::{ColorSpan, TextSpan};
        let mut interner = NpubInterner::new();
        let mut c = CompactMessage::from_message(&Message { id: hex64(80), content: "a".into(), at: 1000, ..Default::default() }, &mut interner);
        let span = TextSpan::Color(ColorSpan { from: 0, to: 1, effect: crate::text_color::Effect::Solid, colors: vec!["#f472b6".into()] });
        let tag = crate::types::EmojiTag { shortcode: "cat".into(), url: "https://e.example/cat.png".into() };
        c.apply_edit("b".into(), 2000, vec![tag.clone()], vec![span.clone()], &mut interner);
        c.apply_edit("older".into(), 1500, Vec::new(), Vec::new(), &mut interner);
        let out = c.to_message(&interner);
        assert_eq!(out.content, "b");
        assert_eq!(out.text_spans, vec![span]);
        assert_eq!(out.emoji_tags, vec![tag]);
        assert_eq!(out.edit_history.unwrap().iter().map(|e| e.content.as_str()).collect::<Vec<_>>(), ["a", "older", "b"]);
    }

    // ========================================================================
    // Position index
    // ========================================================================

    /// The index must hold every position once, ordered by the id at each.
    fn assert_index_sound(v: &CompactMessageVec) {
        assert_eq!(v.id_index.len(), v.messages.len());
        let mut seen = vec![false; v.messages.len()];
        for &p in &v.id_index {
            assert!(!std::mem::replace(&mut seen[p as usize], true), "position {p} twice");
        }
        for w in v.id_index.windows(2) {
            assert!(v.messages[w[0] as usize].id < v.messages[w[1] as usize].id, "index out of id order");
        }
        for w in v.messages.windows(2) {
            assert!(w[0].at <= w[1].at, "messages out of time order");
        }
        for m in &v.messages {
            assert_eq!(v.find_by_id(&m.id).map(|f| f.id), Some(m.id));
        }
    }

    #[test]
    fn the_index_stays_sound_through_every_path() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut v = CompactMessageVec::new();
        let mut held: Vec<String> = Vec::new();
        let (mut serial, mut rekeys) = (0u64, 0u64);
        let mut fresh = |at: u64, held: &mut Vec<String>| {
            serial += 1;
            let id = hex64(serial);
            held.push(id.clone());
            make_compact_msg(&id, at)
        };
        for _ in 0..600 {
            let first = v.first().map_or(10_000, |m| m.at);
            let last = v.last().map_or(10_000, |m| m.at);
            match next(7) {
                0 => assert!(v.insert(fresh(last + next(3), &mut held))),
                1 => assert!(v.insert(fresh(first + next(last - first + 1), &mut held))),
                2 => {
                    let batch: Vec<_> = (0..1 + next(30)).map(|_| fresh(last + next(50), &mut held)).collect();
                    assert_eq!(v.insert_batch(batch.clone()), batch.len());
                    assert_eq!(v.insert_batch(batch), 0, "a repeated batch adds nothing");
                }
                3 => {
                    let batch: Vec<_> = (0..1 + next(30)).map(|_| fresh(first.saturating_sub(next(50)), &mut held)).collect();
                    v.insert_batch(batch);
                }
                4 => {
                    let batch: Vec<_> = (0..1 + next(30)).map(|_| fresh(first + next(last - first + 100), &mut held)).collect();
                    v.insert_batch(batch);
                }
                5 if !held.is_empty() => {
                    let id = held.swap_remove(next(held.len() as u64) as usize);
                    assert!(v.remove_by_hex_id(&id));
                    assert!(!v.contains_hex_id(&id));
                }
                6 if !held.is_empty() => {
                    let i = next(held.len() as u64) as usize;
                    rekeys += 1;
                    let new = hex64(1 << 40 | rekeys);
                    assert!(v.rekey(&encode_message_id(&held[i]), encode_message_id(&new)));
                    held[i] = new;
                }
                _ => {}
            }
            assert_index_sound(&v);
            assert_eq!(v.len(), held.len());
        }
        assert!(v.len() > 50);
        assert_eq!(v.retain_newest(50), held.len() - 50);
        assert_index_sound(&v);
        assert_eq!(v.memory_usage(), 50 * (std::mem::size_of::<CompactMessage>() + 4), "eviction hands capacity back");
    }

    #[test]
    fn a_live_message_grows_a_loaded_chat_by_an_eighth() {
        let mut v = CompactMessageVec::new();
        v.insert_batch((0..1000u64).map(|n| make_compact_msg(&hex64(n), n)));
        let loaded = v.memory_usage();
        assert_eq!(loaded, 1000 * (std::mem::size_of::<CompactMessage>() + 4), "a page loads at exact capacity");
        assert!(v.insert(make_compact_msg(&hex64(5000), 5000)));
        assert_eq!(v.memory_usage(), 1125 * (std::mem::size_of::<CompactMessage>() + 4));
        // Older pages join without slack; the next arrival grows again by an eighth.
        v.insert_batch((0..200u64).map(|n| make_compact_msg(&hex64(10_000 + n), 0)));
        assert_index_sound(&v);
        assert_eq!(v.messages.capacity(), 1201);
    }
}
