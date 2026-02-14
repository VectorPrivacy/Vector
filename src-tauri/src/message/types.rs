//! Message types and data structures.
//!
//! This module contains:
//! - Message, Attachment, Reaction, EditEntry structs
//! - Image metadata and compression cache types

use std::collections::HashMap;
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::Mutex as TokioMutex;
use once_cell::sync::Lazy;

use crate::net;
use crate::TAURI_APP;

/// Cached compressed image data
#[derive(Clone)]
pub struct CachedCompressedImage {
    pub bytes: Arc<Vec<u8>>,
    pub extension: String,
    pub img_meta: Option<ImageMetadata>,
    pub original_size: u64,
    pub compressed_size: u64,
}

/// Global cache for pre-compressed images
pub static COMPRESSION_CACHE: Lazy<TokioMutex<HashMap<String, Option<CachedCompressedImage>>>> =
    Lazy::new(|| TokioMutex::new(HashMap::new()));

/// Cache for Android file bytes: uri -> (bytes, extension, name, size)
/// This is used to cache file bytes immediately after file selection on Android,
/// before the temporary content URI permission expires.
pub static ANDROID_FILE_CACHE: Lazy<std::sync::Mutex<HashMap<String, (Arc<Vec<u8>>, String, String, u64)>>> =
    Lazy::new(|| std::sync::Mutex::new(HashMap::new()));

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Message {
    pub id: String,
    pub content: String,
    pub replied_to: String,
    /// Content preview of the replied-to message (fetched from DB, not dependent on cache)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replied_to_content: Option<String>,
    /// Sender's npub of the replied-to message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replied_to_npub: Option<String>,
    /// Whether the replied-to message has attachments (for showing attachment icon)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replied_to_has_attachment: Option<bool>,
    pub preview_metadata: Option<net::SiteMetadata>,
    pub attachments: Vec<Attachment>,
    pub reactions: Vec<Reaction>,
    pub at: u64,
    pub pending: bool,
    pub failed: bool,
    pub mine: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub npub: Option<String>, // Sender's npub (for group chats)
    #[serde(skip_serializing, default)]
    pub wrapper_event_id: Option<String>, // Public giftwrap event ID (for duplicate detection)
    /// Whether this message has been edited
    #[serde(default)]
    pub edited: bool,
    /// Full edit history (original + all edits), ordered chronologically
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit_history: Option<Vec<EditEntry>>,
}

impl Default for Message {
    fn default() -> Self {
        Self {
            id: String::new(),
            content: String::new(),
            replied_to: String::new(),
            replied_to_content: None,
            replied_to_npub: None,
            replied_to_has_attachment: None,
            preview_metadata: None,
            attachments: Vec::new(),
            reactions: Vec::new(),
            at: 0,
            pending: false,
            failed: false,
            mine: false,
            npub: None,
            wrapper_event_id: None,
            edited: false,
            edit_history: None,
        }
    }
}

impl Message {
    /// Get an attachment by ID
    /*
    fn get_attachment(&self, id: &str) -> Option<&Attachment> {
        self.attachments.iter().find(|p| p.id == id)
    }
    */

    /// Get an attachment by ID
    pub fn get_attachment_mut(&mut self, id: &str) -> Option<&mut Attachment> {
        self.attachments.iter_mut().find(|p| p.id == id)
    }

    /// Apply an edit to this message, updating content and tracking history
    /// Handles deduplication, sorting, and ensures content reflects the latest edit
    pub fn apply_edit(&mut self, new_content: String, edited_at: u64) {
        // Initialize edit history with original content if not present
        if self.edit_history.is_none() {
            self.edit_history = Some(vec![EditEntry {
                content: self.content.clone(),
                edited_at: self.at,
            }]);
        }

        if let Some(ref mut history) = self.edit_history {
            // Deduplicate: skip if we already have this edit (by timestamp)
            if history.iter().any(|e| e.edited_at == edited_at) {
                return;
            }

            // Add new edit to history
            history.push(EditEntry {
                content: new_content,
                edited_at,
            });

            // Sort by timestamp to ensure correct order
            history.sort_by_key(|e| e.edited_at);

            // Content is always the latest edit (last in sorted history)
            if let Some(latest) = history.last() {
                self.content = latest.content.clone();
            }
        }

        self.edited = true;
    }

    /// Add a Reaction - if it was not already added
    pub fn add_reaction(&mut self, reaction: Reaction, chat_id: Option<&str>) -> bool {
        // Make sure we don't add the same reaction twice
        if !self.reactions.iter().any(|r| r.id == reaction.id) {
            self.reactions.push(reaction);

            // Update the frontend if a Chat ID was provided
            if let Some(chat) = chat_id {
                let handle = TAURI_APP.get().unwrap();
                handle.emit("message_update", serde_json::json!({
                    "old_id": &self.id,
                    "message": &self,
                    "chat_id": chat
                })).unwrap();
            }
            true
        } else {
            // Reaction was already added previously
            false
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct ImageMetadata {
    /// The Blurhash preview
    pub blurhash: String,
    /// Image pixel width
    pub width: u32,
    /// Image pixel height
    pub height: u32,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Attachment {
    /// The SHA256 hash of the file as a unique file ID
    pub id: String,
    // The encryption key (empty for MLS - derived from group secret)
    pub key: String,
    // The encryption nonce (empty for MLS - derived from group secret)
    pub nonce: String,
    /// The file extension
    pub extension: String,
    /// The host URL, typically a NIP-96 server
    pub url: String,
    /// The storage directory path (typically the ~/Downloads folder)
    pub path: String,
    /// The download size of the encrypted file
    pub size: u64,
    /// Image metadata (Visual Media only, i.e: Images, Video Thumbnail, etc)
    pub img_meta: Option<ImageMetadata>,
    /// Whether the file is currently being downloaded or not
    pub downloading: bool,
    /// Whether the file has been downloaded or not
    pub downloaded: bool,
    /// WebXDC topic ID for realtime channels (Mini Apps only)
    /// This is transmitted in the Nostr event and used to derive the realtime channel topic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webxdc_topic: Option<String>,
    /// MLS group ID for key derivation (MIP-04)
    /// When present, encryption key is derived from MLS group secret instead of explicit key/nonce
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    /// Original file hash before encryption (MIP-04 'x' field)
    /// Used for file deduplication and integrity verification
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_hash: Option<String>,
    /// MIP-04 scheme version (e.g., "mip04-v1", "mip04-v2")
    /// Required for correct key derivation during decryption
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheme_version: Option<String>,
    /// MIP-04 filename used during encryption (for AAD matching)
    /// Must be stored exactly as used during encryption for decryption to succeed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mls_filename: Option<String>,
}

impl Default for Attachment {
    fn default() -> Self {
        Self {
            id: String::new(),
            key: String::new(),
            nonce: String::new(),
            extension: String::new(),
            url: String::new(),
            path: String::new(),
            size: 0,
            img_meta: None,
            downloading: false,
            downloaded: true,
            webxdc_topic: None,
            group_id: None,
            original_hash: None,
            scheme_version: None,
            mls_filename: None,
        }
    }
}

/// A simple pre-upload format to associate a byte stream with a file extension
/// Note: This type is used internally - the bytes field is not serialized.
/// For Tauri commands, AttachmentFile is constructed in Rust, not deserialized from JS.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AttachmentFile {
    #[serde(skip, default = "default_arc_bytes")]
    pub bytes: Arc<Vec<u8>>,
    /// Image metadata (for images only)
    pub img_meta: Option<ImageMetadata>,
    pub extension: String,
}

fn default_arc_bytes() -> Arc<Vec<u8>> {
    Arc::new(Vec::new())
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Reaction {
    pub id: String,
    /// The HEX Event ID of the message being reacted to
    pub reference_id: String,
    /// The HEX ID of the author
    pub author_id: String,
    /// The emoji of the reaction
    pub emoji: String,
}

/// A single entry in a message's edit history
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct EditEntry {
    /// The content at this point in history
    pub content: String,
    /// When this version was created (Unix ms)
    pub edited_at: u64,
}