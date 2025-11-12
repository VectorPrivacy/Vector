//! Protocol-Agnostic Rumor Processing Module
//!
//! This module provides unified event processing for all messaging protocols (NIP-17 DMs, MLS Groups, etc.).
//! The core insight is that "rumors" (the inner decrypted events) are protocol-agnostic - only the
//! wrapping/unwrapping differs between protocols.
//!
//! ## Architecture
//!
//! ```text
//! Event → Protocol Handler (unwrap) → RumorEvent
//!                                       ↓
//!                             process_rumor() [SHARED]
//!                                       ↓
//!                             RumorProcessingResult
//!                                       ↓
//!                     Storage Handler (protocol-specific)
//!                                       ↓
//!                             Emit to UI [SHARED]
//! ```
//!
//! ## Supported Rumor Types
//!
//! - **Text Messages**: `Kind::PrivateDirectMessage` - Plain text with optional replies
//! - **File Attachments**: `Kind::from_u16(15)` - Encrypted files with metadata
//! - **Reactions**: `Kind::Reaction` - Emoji reactions to messages
//! - **Typing Indicators**: `Kind::ApplicationSpecificData` - Real-time typing status

use std::borrow::Cow;
use nostr_sdk::prelude::*;
use tauri::Manager;
use crate::{Message, Attachment, Reaction, TAURI_APP};
use crate::message::ImageMetadata;

/// Protocol-agnostic rumor event representation
///
/// This is the unified format for all decrypted events, regardless of whether
/// they came from NIP-17 giftwraps or MLS encryption.
#[derive(Debug, Clone)]
pub struct RumorEvent {
    pub id: EventId,
    pub kind: Kind,
    pub content: String,
    pub tags: Tags,
    pub created_at: Timestamp,
    pub pubkey: PublicKey,
}

/// Context for processing a rumor
///
/// Provides the necessary context to process a rumor correctly,
/// including who sent it and what conversation it belongs to.
#[derive(Debug, Clone)]
pub struct RumorContext {
    /// The sender's public key
    pub sender: PublicKey,
    /// Whether this rumor is from ourselves
    pub is_mine: bool,
    /// The conversation ID (npub for DMs, group_id for MLS)
    pub conversation_id: String,
    /// The type of conversation
    pub conversation_type: ConversationType,
}

/// Type of conversation
#[derive(Debug, Clone, PartialEq)]
pub enum ConversationType {
    /// Direct message (NIP-17)
    DirectMessage,
    /// MLS group chat
    MlsGroup,
}

/// Result of processing a rumor
///
/// Represents the different types of events that can result from
/// processing a rumor. The caller is responsible for storing these
/// results appropriately based on the conversation type.
#[derive(Debug, Clone)]
pub enum RumorProcessingResult {
    /// A text message (with optional reply reference)
    TextMessage(Message),
    /// A file attachment message
    FileAttachment(Message),
    /// An emoji reaction to a message
    Reaction(Reaction),
    /// A typing indicator update
    TypingIndicator {
        profile_id: String,
        until: u64,
    },
    /// Event was ignored (unknown type or invalid)
    Ignored,
}

/// Main rumor processor - protocol agnostic
///
/// This is the single entry point for processing all rumor types.
/// It handles text messages, file attachments, reactions, and typing indicators
/// in a unified way, regardless of the underlying protocol.
///
/// # Arguments
///
/// * `rumor` - The rumor event to process
/// * `context` - Context about the rumor (sender, conversation, etc.)
///
/// # Returns
///
/// A `RumorProcessingResult` indicating what type of event was processed,
/// or an error if processing failed.
pub async fn process_rumor(
    rumor: RumorEvent,
    context: RumorContext,
) -> Result<RumorProcessingResult, String> {
    match rumor.kind {
        // Text messages
        Kind::PrivateDirectMessage => {
            process_text_message(rumor, context).await
        }
        // File attachments
        k if k.as_u16() == 15 => {
            process_file_attachment(rumor, context).await
        }
        // Emoji reactions
        Kind::Reaction => {
            process_reaction(rumor, context).await
        }
        // Application-specific data (typing indicators, etc.)
        Kind::ApplicationSpecificData => {
            process_app_specific(rumor, context).await
        }
        // Unknown or unsupported kind
        _ => Ok(RumorProcessingResult::Ignored),
    }
}

/// Process a text message rumor
///
/// Extracts text content, reply references, and millisecond-precision timestamps.
async fn process_text_message(
    rumor: RumorEvent,
    context: RumorContext,
) -> Result<RumorProcessingResult, String> {
    // Extract reply reference if present
    let replied_to = extract_reply_reference(&rumor);
    
    // Extract millisecond-precision timestamp
    let ms_timestamp = extract_millisecond_timestamp(&rumor);
    
    // Create the message
    let msg = Message {
        id: rumor.id.to_hex(),
        content: rumor.content,
        replied_to,
        preview_metadata: None,
        at: ms_timestamp,
        attachments: Vec::new(),
        reactions: Vec::new(),
        mine: context.is_mine,
        pending: false,
        failed: false,
        npub: if context.conversation_type == ConversationType::MlsGroup {
            // For group chats, include sender's npub
            rumor.pubkey.to_bech32().ok()
        } else {
            // For DMs, npub is implicit (the other participant)
            None
        },
    };
    
    Ok(RumorProcessingResult::TextMessage(msg))
}

/// Process a file attachment rumor
///
/// Handles encrypted file metadata including:
/// - Decryption keys and nonces
/// - Original file hashes (for deduplication)
/// - Image metadata (blurhash, dimensions)
/// - File extensions and mime types
async fn process_file_attachment(
    rumor: RumorEvent,
    context: RumorContext,
) -> Result<RumorProcessingResult, String> {
    // Extract decryption parameters
    let decryption_key = rumor.tags
        .find(TagKind::Custom(Cow::Borrowed("decryption-key")))
        .and_then(|tag| tag.content())
        .ok_or("Missing decryption-key tag")?
        .to_string();
    
    let decryption_nonce = rumor.tags
        .find(TagKind::Custom(Cow::Borrowed("decryption-nonce")))
        .and_then(|tag| tag.content())
        .ok_or("Missing decryption-nonce tag")?
        .to_string();
    
    // Extract original file hash (ox tag) if present
    let original_file_hash = rumor.tags
        .find(TagKind::Custom(Cow::Borrowed("ox")))
        .and_then(|tag| tag.content())
        .map(|s| s.to_string());
    
    // Extract content storage URL
    let content_url = rumor.content.clone();
    
    // Skip attachments with empty file hash - these are corrupted uploads
    const EMPTY_FILE_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    if content_url.contains(EMPTY_FILE_HASH) {
        eprintln!("Skipping attachment with empty file hash in URL: {}", content_url);
        return Err("Attachment contains empty file hash - skipping".to_string());
    }
    
    // Extract image metadata if provided
    let img_meta: Option<ImageMetadata> = {
        let blurhash_opt = rumor.tags
            .find(TagKind::Custom(Cow::Borrowed("blurhash")))
            .and_then(|tag| tag.content())
            .map(|s| s.to_string());
        
        let dimensions_opt = rumor.tags
            .find(TagKind::Custom(Cow::Borrowed("dim")))
            .and_then(|tag| tag.content())
            .and_then(|s| {
                // Parse "widthxheight" format
                let parts: Vec<&str> = s.split('x').collect();
                if parts.len() == 2 {
                    let width = parts[0].parse::<u32>().ok()?;
                    let height = parts[1].parse::<u32>().ok()?;
                    Some((width, height))
                } else {
                    None
                }
            });
        
        // Only create ImageMetadata if we have all required fields
        match (blurhash_opt, dimensions_opt) {
            (Some(blurhash), Some((width, height))) => {
                Some(ImageMetadata {
                    blurhash,
                    width,
                    height,
                })
            },
            _ => None
        }
    };
    
    // Figure out the file extension from the mime-type
    let mime_type = rumor.tags
        .find(TagKind::Custom(Cow::Borrowed("file-type")))
        .and_then(|tag| tag.content())
        .ok_or("Missing file-type tag")?;
    let extension = crate::util::extension_from_mime(mime_type);
    
    // Get the handle for path resolution
    let handle = TAURI_APP.get().ok_or("App handle not initialized")?;
    
    // Choose the appropriate base directory based on platform
    let base_directory = if cfg!(target_os = "ios") {
        tauri::path::BaseDirectory::Document
    } else {
        tauri::path::BaseDirectory::Download
    };
    
    // Resolve the directory path
    let dir = handle.path()
        .resolve("vector", base_directory)
        .map_err(|e| format!("Failed to resolve directory: {}", e))?;
    
    // Grab the reported file size
    let reported_size = rumor.tags
        .find(TagKind::Custom(Cow::Borrowed("size")))
        .and_then(|tag| tag.content())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    
    // Determine file path and download status
    let (file_hash, file_path, downloaded) = if let Some(ox_hash) = original_file_hash {
        // We have an original hash - check if file exists locally
        let hash_file_path = dir.join(format!("{}.{}", ox_hash, extension));
        if hash_file_path.exists() {
            // File already exists locally
            (ox_hash, hash_file_path.to_string_lossy().to_string(), true)
        } else {
            // File doesn't exist yet - will need to be downloaded
            (ox_hash, hash_file_path.to_string_lossy().to_string(), false)
        }
    } else {
        // No original hash - use nonce as placeholder ID
        let nonce_file_path = dir.join(format!("{}.{}", decryption_nonce, extension));
        (decryption_nonce.clone(), nonce_file_path.to_string_lossy().to_string(), false)
    };
    
    // Extract reply reference if present
    let replied_to = extract_reply_reference(&rumor);
    
    // Extract millisecond-precision timestamp
    let ms_timestamp = extract_millisecond_timestamp(&rumor);
    
    // Create the attachment
    let attachment = Attachment {
        id: file_hash,
        key: decryption_key,
        nonce: decryption_nonce,
        extension: extension.to_string(),
        url: content_url,
        path: file_path,
        size: reported_size,
        img_meta,
        downloading: false,
        downloaded,
    };
    
    // Create the message with attachment
    let msg = Message {
        id: rumor.id.to_hex(),
        content: String::new(),
        replied_to,
        preview_metadata: None,
        at: ms_timestamp,
        attachments: vec![attachment],
        reactions: Vec::new(),
        mine: context.is_mine,
        pending: false,
        failed: false,
        npub: if context.conversation_type == ConversationType::MlsGroup {
            // For group chats, include sender's npub
            rumor.pubkey.to_bech32().ok()
        } else {
            // For DMs, npub is implicit (the other participant)
            None
        },
    };
    
    Ok(RumorProcessingResult::FileAttachment(msg))
}

/// Process a reaction rumor
///
/// Extracts emoji reactions to messages.
async fn process_reaction(
    rumor: RumorEvent,
    _context: RumorContext,
) -> Result<RumorProcessingResult, String> {
    // Find the reference event (the message being reacted to)
    let reference_tag = rumor.tags
        .find(TagKind::e())
        .ok_or("Reaction missing reference event tag")?;
    
    let reference_id = reference_tag.content()
        .ok_or("Reaction reference tag has no content")?
        .to_string();
    
    // Create the reaction
    let reaction = Reaction {
        id: rumor.id.to_hex(),
        reference_id,
        author_id: rumor.pubkey.to_hex(),
        emoji: rumor.content,
    };
    
    Ok(RumorProcessingResult::Reaction(reaction))
}

/// Process application-specific data (typing indicators, etc.)
///
/// Currently handles typing indicators for real-time status updates.
async fn process_app_specific(
    rumor: RumorEvent,
    _context: RumorContext,
) -> Result<RumorProcessingResult, String> {
    // Check if this is a typing indicator
    if is_typing_indicator(&rumor) {
        // Validate expiration tag (must be within 30 seconds)
        let expiry_tag = rumor.tags
            .find(TagKind::Expiration)
            .ok_or("Typing indicator missing expiration tag")?;
        
        let expiry_timestamp: u64 = expiry_tag.content()
            .ok_or("Expiration tag has no content")?
            .parse()
            .map_err(|_| "Invalid expiration timestamp")?;
        
        // Check if the expiry timestamp is reasonable (not expired, and not too far in the future)
        let current_timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| format!("System time error: {}", e))?
            .as_secs();
        
        // Reject expired or future-dated typing indicators (more than 30 sec in the future)
        if expiry_timestamp <= current_timestamp || expiry_timestamp > current_timestamp + 30 {
            return Ok(RumorProcessingResult::Ignored);
        }
        
        // Valid typing indicator (not expired and within reasonable time window)
        let profile_id = rumor.pubkey.to_bech32()
            .map_err(|e| format!("Failed to convert pubkey to bech32: {}", e))?;
        
        return Ok(RumorProcessingResult::TypingIndicator {
            profile_id,
            until: expiry_timestamp,
        });
    }
    
    // Unknown application-specific data
    Ok(RumorProcessingResult::Ignored)
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Extract millisecond-precision timestamp from rumor
///
/// Combines the rumor's created_at (seconds) with a custom "ms" tag
/// to provide millisecond precision for accurate message ordering.
fn extract_millisecond_timestamp(rumor: &RumorEvent) -> u64 {
    match rumor.tags.find(TagKind::Custom(Cow::Borrowed("ms"))) {
        Some(ms_tag) => {
            // Get the ms value and append it to the timestamp
            if let Some(ms_str) = ms_tag.content() {
                if let Ok(ms_value) = ms_str.parse::<u64>() {
                    // Validate that ms is between 0-999
                    if ms_value <= 999 {
                        return rumor.created_at.as_u64() * 1000 + ms_value;
                    }
                }
            }
            // Fallback to seconds if ms tag is invalid
            rumor.created_at.as_u64() * 1000
        }
        None => rumor.created_at.as_u64() * 1000
    }
}

/// Extract reply reference from rumor tags
///
/// Looks for an "e" tag with the "reply" marker to identify
/// which message this rumor is replying to.
fn extract_reply_reference(rumor: &RumorEvent) -> String {
    match rumor.tags.find(TagKind::e()) {
        Some(tag) => {
            if tag.is_reply() {
                tag.content().unwrap_or("").to_string()
            } else {
                String::new()
            }
        }
        None => String::new(),
    }
}

/// Check if rumor is a typing indicator
///
/// Validates that the rumor has:
/// - d tag with value "vector"
/// - content "typing"
fn is_typing_indicator(rumor: &RumorEvent) -> bool {
    // Check d tag
    let has_vector_tag = rumor.tags
        .find(TagKind::d())
        .and_then(|tag| tag.content())
        .map(|content| content == "vector")
        .unwrap_or(false);
    
    // Check content
    let is_typing_content = rumor.content == "typing";
    
    has_vector_tag && is_typing_content
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_extract_millisecond_timestamp_with_ms_tag() {
        // Test with valid ms tag
        let mut tags = Tags::new();
        tags.push(Tag::custom(TagKind::Custom(Cow::Borrowed("ms")), vec!["123"]));
        
        let rumor = RumorEvent {
            id: EventId::all_zeros(),
            kind: Kind::PrivateDirectMessage,
            content: String::new(),
            tags,
            created_at: Timestamp::from_secs(1000),
            pubkey: PublicKey::from_slice(&[0u8; 32]).unwrap(),
        };
        
        assert_eq!(extract_millisecond_timestamp(&rumor), 1000123);
    }
    
    #[test]
    fn test_extract_millisecond_timestamp_without_ms_tag() {
        // Test without ms tag
        let rumor = RumorEvent {
            id: EventId::all_zeros(),
            kind: Kind::PrivateDirectMessage,
            content: String::new(),
            tags: Tags::new(),
            created_at: Timestamp::from_secs(1000),
            pubkey: PublicKey::from_slice(&[0u8; 32]).unwrap(),
        };
        
        assert_eq!(extract_millisecond_timestamp(&rumor), 1000000);
    }
    
    #[test]
    fn test_is_typing_indicator() {
        // Valid typing indicator
        let mut tags = Tags::new();
        tags.push(Tag::custom(TagKind::d(), vec!["vector"]));
        
        let rumor = RumorEvent {
            id: EventId::all_zeros(),
            kind: Kind::ApplicationSpecificData,
            content: "typing".to_string(),
            tags,
            created_at: Timestamp::now(),
            pubkey: PublicKey::from_slice(&[0u8; 32]).unwrap(),
        };
        
        assert!(is_typing_indicator(&rumor));
        
        // Invalid - wrong content
        let mut rumor2 = rumor.clone();
        rumor2.content = "not_typing".to_string();
        
        assert!(!is_typing_indicator(&rumor2));
    }
}