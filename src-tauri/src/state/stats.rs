//! Cache statistics and memory measurement for benchmarking.
//!
//! This module provides tools for measuring memory usage and performance
//! of the message cache, enabling before/after comparison during optimization.

use std::time::Duration;

use crate::message::{Message, Attachment, Reaction, EditEntry, ImageMetadata};
use crate::message::compact::{CompactMessage, CompactMessageVec, CompactReaction, CompactAttachment, MessageFlags, NpubInterner};
use crate::net::SiteMetadata;
use crate::Chat;

/// Statistics for message cache operations
#[derive(Debug, Default, Clone)]
pub struct CacheStats {
    /// Total number of messages across all chats
    pub message_count: usize,
    /// Total number of chats
    pub chat_count: usize,
    /// Estimated total memory in bytes
    pub total_memory_bytes: usize,
    /// Duration of last insert operation
    pub last_insert_duration: Duration,
    /// Average insert duration in nanoseconds
    pub avg_insert_duration_ns: u64,
    /// Number of insert operations recorded
    pub insert_count: u64,
    /// Total nanoseconds spent inserting
    insert_total_ns: u64,
}

impl CacheStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an insert operation's duration
    pub fn record_insert(&mut self, duration: Duration) {
        self.last_insert_duration = duration;
        self.insert_count += 1;
        self.insert_total_ns += duration.as_nanos() as u64;
        self.avg_insert_duration_ns = self.insert_total_ns / self.insert_count;
    }

    /// Update memory stats from current state
    pub fn update_from_chats(&mut self, chats: &[Chat]) {
        self.chat_count = chats.len();
        self.message_count = chats.iter().map(|c| c.messages.len()).sum();
        self.total_memory_bytes = chats.iter().map(|c| c.deep_size()).sum();
    }

    /// Print current stats
    pub fn log(&self) {
        println!(
            "[CacheStats] chats={} messages={} memory={} last_insert={:?} avg_insert={}ns inserts={}",
            self.chat_count,
            self.message_count,
            format_bytes(self.total_memory_bytes),
            self.last_insert_duration,
            self.avg_insert_duration_ns,
            self.insert_count,
        );
    }

    /// Get a summary string
    #[allow(dead_code)]
    pub fn summary(&self) -> String {
        format!(
            "chats={} msgs={} mem={} avg_insert={}ns",
            self.chat_count,
            self.message_count,
            format_bytes(self.total_memory_bytes),
            self.avg_insert_duration_ns,
        )
    }

    /// Check if we should log (every N inserts)
    pub fn should_log(&self, interval: u64) -> bool {
        self.insert_count > 0 && self.insert_count.is_multiple_of(interval)
    }
}

/// Format bytes as human-readable string
fn format_bytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.2}MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.2}KB", bytes as f64 / 1024.0)
    } else {
        format!("{}B", bytes)
    }
}

/// Trait for calculating deep/heap memory size of a value
pub trait DeepSize {
    fn deep_size(&self) -> usize;
}

// === Primitive implementations ===

impl DeepSize for String {
    #[inline]
    fn deep_size(&self) -> usize {
        std::mem::size_of::<String>() + self.capacity()
    }
}

impl DeepSize for str {
    #[inline]
    fn deep_size(&self) -> usize {
        self.len()
    }
}

impl DeepSize for u64 {
    #[inline]
    fn deep_size(&self) -> usize {
        std::mem::size_of::<u64>()
    }
}

impl DeepSize for u32 {
    #[inline]
    fn deep_size(&self) -> usize {
        std::mem::size_of::<u32>()
    }
}

impl DeepSize for bool {
    #[inline]
    fn deep_size(&self) -> usize {
        std::mem::size_of::<bool>()
    }
}

// === Generic implementations ===

impl<T: DeepSize> DeepSize for Vec<T> {
    fn deep_size(&self) -> usize {
        std::mem::size_of::<Vec<T>>()
            + self.capacity() * std::mem::size_of::<T>()
            + self.iter().map(|item| item.deep_size() - std::mem::size_of::<T>()).sum::<usize>()
    }
}

impl<T: DeepSize> DeepSize for Option<T> {
    fn deep_size(&self) -> usize {
        std::mem::size_of::<Option<T>>()
            + self.as_ref().map(|v| v.deep_size() - std::mem::size_of::<T>()).unwrap_or(0)
    }
}

// === Domain type implementations ===

impl DeepSize for ImageMetadata {
    fn deep_size(&self) -> usize {
        std::mem::size_of::<ImageMetadata>() + self.blurhash.capacity()
    }
}

impl DeepSize for Attachment {
    fn deep_size(&self) -> usize {
        std::mem::size_of::<Attachment>()
            + self.id.capacity()
            + self.key.capacity()
            + self.nonce.capacity()
            + self.extension.capacity()
            + self.url.capacity()
            + self.path.capacity()
            + self.img_meta.as_ref().map(|m| m.blurhash.capacity()).unwrap_or(0)
            + self.webxdc_topic.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + self.group_id.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + self.original_hash.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + self.scheme_version.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + self.mls_filename.as_ref().map(|s| s.capacity()).unwrap_or(0)
    }
}

impl DeepSize for Reaction {
    fn deep_size(&self) -> usize {
        std::mem::size_of::<Reaction>()
            + self.id.capacity()
            + self.reference_id.capacity()
            + self.author_id.capacity()
            + self.emoji.capacity()
    }
}

impl DeepSize for EditEntry {
    fn deep_size(&self) -> usize {
        std::mem::size_of::<EditEntry>() + self.content.capacity()
    }
}

impl DeepSize for SiteMetadata {
    fn deep_size(&self) -> usize {
        std::mem::size_of::<SiteMetadata>()
            + self.domain.capacity()
            + self.og_title.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + self.og_description.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + self.og_image.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + self.og_url.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + self.og_type.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + self.title.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + self.description.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + self.favicon.as_ref().map(|s| s.capacity()).unwrap_or(0)
    }
}

impl DeepSize for Message {
    fn deep_size(&self) -> usize {
        std::mem::size_of::<Message>()
            + self.id.capacity()
            + self.content.capacity()
            + self.replied_to.capacity()
            + self.replied_to_content.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + self.replied_to_npub.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + self.npub.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + self.wrapper_event_id.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + self.preview_metadata.as_ref().map(|m| m.deep_size()).unwrap_or(0)
            + self.attachments.iter().map(|a| a.deep_size()).sum::<usize>()
            + self.reactions.iter().map(|r| r.deep_size()).sum::<usize>()
            + self.edit_history.as_ref().map(|h| h.iter().map(|e| e.deep_size()).sum::<usize>()).unwrap_or(0)
    }
}

impl DeepSize for Chat {
    fn deep_size(&self) -> usize {
        std::mem::size_of::<Chat>()
            + self.id.capacity()
            + 32  // [u8; 32] is inline, no heap
            + self.participants.capacity() * std::mem::size_of::<u16>()  // Vec<u16> heap buffer
            + self.messages.deep_size()
            // metadata.custom_fields HashMap
            + self.metadata.custom_fields.iter()
                .map(|(k, v)| k.capacity() + v.capacity())
                .sum::<usize>()
            // typing_participants Vec<(u16, u64)> heap buffer
            + self.typing_participants.capacity() * std::mem::size_of::<(u16, u64)>()
    }
}

// === Compact message type implementations ===

impl DeepSize for MessageFlags {
    #[inline]
    fn deep_size(&self) -> usize {
        std::mem::size_of::<MessageFlags>()
    }
}

impl DeepSize for CompactReaction {
    fn deep_size(&self) -> usize {
        std::mem::size_of::<CompactReaction>()
            + self.emoji.len()  // Box<str> heap allocation
    }
}

impl DeepSize for CompactAttachment {
    fn deep_size(&self) -> usize {
        std::mem::size_of::<CompactAttachment>()
            + self.extension.len()  // Box<str> heap allocation
            + self.url.len()
            + self.path.len()
            + self.img_meta.as_ref().map(|m| m.deep_size()).unwrap_or(0)
            + self.group_id.as_ref().map(|_| 32).unwrap_or(0)
            + self.original_hash.as_ref().map(|_| 32).unwrap_or(0)
            + self.webxdc_topic.as_ref().map(|s| s.len()).unwrap_or(0)
            + self.mls_filename.as_ref().map(|s| s.len()).unwrap_or(0)
            + self.scheme_version.as_ref().map(|s| s.len()).unwrap_or(0)
    }
}

impl DeepSize for CompactMessage {
    fn deep_size(&self) -> usize {
        std::mem::size_of::<CompactMessage>()
            + self.content.len()  // Box<str> has no over-allocation, len == allocated
            + self.replied_to_content.as_ref().map(|s| s.len()).unwrap_or(0)
            + self.preview_metadata.as_ref().map(|m| m.deep_size()).unwrap_or(0)
            + self.attachments.iter().map(|a| a.deep_size()).sum::<usize>()
            + self.reactions.iter().map(|r| r.deep_size()).sum::<usize>()
            + self.edit_history.as_ref().map(|h| h.iter().map(|e| e.deep_size()).sum::<usize>()).unwrap_or(0)
    }
}

impl DeepSize for CompactMessageVec {
    fn deep_size(&self) -> usize {
        std::mem::size_of::<CompactMessageVec>()
            + std::mem::size_of_val(self.messages())
            + self.iter().map(|m| m.deep_size() - std::mem::size_of::<CompactMessage>()).sum::<usize>()
            // id_index: each entry is ([u8; 32], u32) = 36 bytes
            + self.len() * std::mem::size_of::<([u8; 32], u32)>()
    }
}

impl DeepSize for NpubInterner {
    fn deep_size(&self) -> usize {
        self.memory_usage()
    }
}

