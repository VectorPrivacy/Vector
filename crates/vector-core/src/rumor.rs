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
use std::path::Path;
use nostr_sdk::prelude::*;
use crate::types::{Message, Attachment, ImageMetadata, Reaction};
use crate::stored_event::{StoredEvent, StoredEventBuilder, event_kind};
use crate::crypto::{extension_from_mime, sanitize_filename};

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
    /// A leave request from a group member (admin should auto-remove them)
    LeaveRequest {
        /// The event ID of the leave request (for deduplication)
        event_id: String,
        /// The pubkey of the member requesting to leave (npub)
        member_pubkey: String,
    },
    /// A WebXDC peer advertisement for realtime channels
    WebxdcPeerAdvertisement {
        event_id: String,
        topic_id: String,
        node_addr: String,
        sender_npub: String,
        created_at: u64,
    },
    /// A WebXDC peer left signal (peer closed their Mini App)
    WebxdcPeerLeft {
        event_id: String,
        topic_id: String,
        sender_npub: String,
        created_at: u64,
    },
    /// Unknown event type - stored for future compatibility
    /// The frontend will render this as "Unknown Event" placeholder
    UnknownEvent(StoredEvent),
    /// A PIVX payment promo code sent in chat
    PivxPayment {
        /// The promo code (5-char Base58)
        gift_code: String,
        /// Amount in PIV
        amount_piv: f64,
        /// The PIVX address for balance checking (optional for older events)
        address: Option<String>,
        /// The message ID for this payment event
        message_id: String,
        /// The stored event for persistence
        event: StoredEvent,
    },
    /// Event was ignored (invalid, expired, or should not be stored)
    Ignored,
    /// A message edit event
    Edit {
        /// The ID of the message being edited
        message_id: String,
        /// The new content
        new_content: String,
        /// Timestamp of the edit (milliseconds)
        edited_at: u64,
        /// The stored event for persistence
        event: StoredEvent,
    },
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
/// * `download_dir` - Directory for file attachment paths
///
/// # Returns
///
/// A `RumorProcessingResult` indicating what type of event was processed,
/// or an error if processing failed.
pub fn process_rumor(
    rumor: RumorEvent,
    context: RumorContext,
    download_dir: &Path,
) -> Result<RumorProcessingResult, String> {
    match rumor.kind {
        // Text messages - Kind 9 (MLS/White Noise) or Kind 14 (DMs/legacy)
        Kind::PrivateDirectMessage => {
            process_text_message(rumor, context)
        }
        k if k.as_u16() == event_kind::MLS_CHAT_MESSAGE => {
            process_text_message(rumor, context)
        }
        // File attachments
        k if k.as_u16() == 15 => {
            process_file_attachment(rumor, context, download_dir)
        }
        // Message edits
        k if k.as_u16() == event_kind::MESSAGE_EDIT => {
            process_edit_event(rumor, context)
        }
        // Emoji reactions
        Kind::Reaction => {
            process_reaction(rumor, context)
        }
        // Application-specific data (typing indicators, etc.)
        Kind::ApplicationSpecificData => {
            process_app_specific(rumor, context)
        }
        // Unknown or unsupported kind - store for future compatibility
        _ => {
            process_unknown_event(rumor, context)
        }
    }
}

/// Process an unknown event type
///
/// Creates a StoredEvent for unknown kinds so they can be stored
/// and potentially displayed/processed in future versions.
fn process_unknown_event(
    rumor: RumorEvent,
    context: RumorContext,
) -> Result<RumorProcessingResult, String> {
    // Convert tags to Vec<Vec<String>> format
    let tags: Vec<Vec<String>> = rumor.tags.iter()
        .map(|tag| {
            tag.as_slice().iter().map(|s| s.to_string()).collect()
        })
        .collect();

    // Extract reference_id from e-tag if present
    let reference_id = rumor.tags
        .find(TagKind::e())
        .and_then(|tag| tag.content())
        .map(|s| s.to_string());

    let event = StoredEventBuilder::new()
        .id(rumor.id.to_hex())
        .kind(rumor.kind.as_u16())
        .content(rumor.content)
        .tags(tags)
        .reference_id(reference_id)
        .created_at(rumor.created_at.as_secs())
        .mine(context.is_mine)
        .npub(rumor.pubkey.to_bech32().ok())
        .build();

    Ok(RumorProcessingResult::UnknownEvent(event))
}

/// Process a text message rumor
///
/// Extracts text content, reply references, and millisecond-precision timestamps.
///
/// Note: MLS imeta attachments (MIP-04) are NOT parsed here — they require
/// MDK/MlsService which is Tauri-specific. The caller should handle MLS imeta
/// parsing separately and patch the result if needed.
fn process_text_message(
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
        replied_to_content: None, // Populated by get_message_views
        replied_to_npub: None,
        replied_to_has_attachment: None,
        preview_metadata: None,
        at: ms_timestamp,
        attachments: Vec::new(),
        reactions: Vec::new(),
        mine: context.is_mine,
        pending: false,
        failed: false,
        npub: if context.conversation_type == ConversationType::MlsGroup {
            rumor.pubkey.to_bech32().ok()
        } else {
            None
        },
        wrapper_event_id: None, // Set by caller after processing
        edited: false,
        edit_history: None,
    };

    Ok(RumorProcessingResult::TextMessage(msg))
}

/// Extract SHA256 hash from a Blossom URL
///
/// Blossom URLs typically follow the format: https://server.com/<sha256hash>[.ext]
pub fn extract_hash_from_blossom_url(url: &str) -> Option<String> {
    let path = url.split('/').last()?;
    let hash_part = path.split('.').next()?;
    if hash_part.len() == 64 && hash_part.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hash_part.to_string())
    } else {
        None
    }
}

/// Process a file attachment rumor
///
/// Handles encrypted file metadata including:
/// - Decryption keys and nonces
/// - Original file hashes (for deduplication)
/// - Image metadata (thumbhash, dimensions)
/// - File extensions and mime types
fn process_file_attachment(
    rumor: RumorEvent,
    context: RumorContext,
    download_dir: &Path,
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
        let thumbhash_opt = rumor.tags
            .find(TagKind::Custom(Cow::Borrowed("thumbhash")))
            .and_then(|tag| tag.content())
            .map(|s| s.to_string());

        let dimensions_opt = rumor.tags
            .find(TagKind::Custom(Cow::Borrowed("dim")))
            .and_then(|tag| tag.content())
            .and_then(|s| {
                let parts: Vec<&str> = s.split('x').collect();
                if parts.len() == 2 {
                    let width = parts[0].parse::<u32>().ok()?;
                    let height = parts[1].parse::<u32>().ok()?;
                    Some((width, height))
                } else {
                    None
                }
            });

        match (thumbhash_opt, dimensions_opt) {
            (Some(thumbhash), Some((width, height))) => {
                Some(ImageMetadata {
                    thumbhash,
                    width,
                    height,
                })
            },
            _ => None
        }
    };

    // Figure out the file extension: prefer the name tag's extension, fall back to MIME-derived
    let mime_type = rumor.tags
        .find(TagKind::Custom(Cow::Borrowed("file-type")))
        .and_then(|tag| tag.content())
        .ok_or("Missing file-type tag")?;
    let mime_extension = extension_from_mime(mime_type);

    // Extract filename from name tag (used for extension override and display name)
    let file_name = rumor.tags
        .find(TagKind::Custom(Cow::Borrowed("name")))
        .and_then(|tag| tag.content())
        .map(|s| sanitize_filename(s))
        .unwrap_or_default();

    // Use the extension from the original filename when available (more accurate than MIME for
    // uncommon types like .sh, .toml, .rs, etc. which all map to application/octet-stream)
    let extension = if !file_name.is_empty() {
        file_name.rsplit('.').next()
            .filter(|e| !e.is_empty() && *e != file_name)
            .map(|e| e.to_lowercase())
            .unwrap_or(mime_extension)
    } else {
        mime_extension
    };

    // Grab the reported file size
    let reported_size = rumor.tags
        .find(TagKind::Custom(Cow::Borrowed("size")))
        .and_then(|tag| tag.content())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    // Determine file path and download status
    let (file_hash, file_path, downloaded) = if let Some(ox_hash) = original_file_hash {
        let hash_file_path = download_dir.join(format!("{}.{}", ox_hash, extension));
        if hash_file_path.exists() {
            (ox_hash, hash_file_path.to_string_lossy().to_string(), true)
        } else {
            (ox_hash, hash_file_path.to_string_lossy().to_string(), false)
        }
    } else {
        let nonce_file_path = download_dir.join(format!("{}.{}", decryption_nonce, extension));
        (decryption_nonce.clone(), nonce_file_path.to_string_lossy().to_string(), false)
    };

    // Extract reply reference if present
    let replied_to = extract_reply_reference(&rumor);

    // Extract millisecond-precision timestamp
    let ms_timestamp = extract_millisecond_timestamp(&rumor);

    // Extract webxdc-topic for Mini Apps (realtime channel isolation)
    let webxdc_topic = rumor.tags
        .find(TagKind::Custom(Cow::Borrowed("webxdc-topic")))
        .and_then(|tag| tag.content())
        .map(|s| s.to_string());

    // Create the attachment
    let attachment = Attachment {
        id: file_hash.clone(),
        key: decryption_key,
        nonce: decryption_nonce,
        extension: extension.to_string(),
        name: file_name,
        url: content_url,
        path: file_path,
        size: reported_size,
        img_meta,
        downloading: false,
        downloaded,
        webxdc_topic,
        group_id: None,       // Kind 15 attachments use explicit key/nonce, not MLS
        original_hash: Some(file_hash), // ox tag value (original file hash)
        scheme_version: None, // Kind 15 uses explicit encryption, not MIP-04
        mls_filename: None,   // Kind 15 uses explicit encryption, not MIP-04
    };

    // Create the message with attachment
    let msg = Message {
        id: rumor.id.to_hex(),
        content: String::new(),
        replied_to,
        replied_to_content: None, // Populated by get_message_views
        replied_to_npub: None,
        replied_to_has_attachment: None,
        preview_metadata: None,
        at: ms_timestamp,
        attachments: vec![attachment],
        reactions: Vec::new(),
        mine: context.is_mine,
        pending: false,
        failed: false,
        npub: if context.conversation_type == ConversationType::MlsGroup {
            rumor.pubkey.to_bech32().ok()
        } else {
            None
        },
        wrapper_event_id: None, // Set by caller after processing
        edited: false,
        edit_history: None,
    };

    Ok(RumorProcessingResult::FileAttachment(msg))
}

/// Process a reaction rumor
///
/// Extracts emoji reactions to messages.
fn process_reaction(
    rumor: RumorEvent,
    _context: RumorContext,
) -> Result<RumorProcessingResult, String> {
    let reference_tag = rumor.tags
        .find(TagKind::e())
        .ok_or("Reaction missing reference event tag")?;

    let reference_id = reference_tag.content()
        .ok_or("Reaction reference tag has no content")?
        .to_string();

    let reaction = Reaction {
        id: rumor.id.to_hex(),
        reference_id,
        author_id: rumor.pubkey.to_bech32().unwrap_or_else(|_| rumor.pubkey.to_hex()),
        emoji: rumor.content,
    };

    Ok(RumorProcessingResult::Reaction(reaction))
}

/// Process a message edit rumor
///
/// Extracts the edited content and references the original message.
fn process_edit_event(
    rumor: RumorEvent,
    context: RumorContext,
) -> Result<RumorProcessingResult, String> {
    let reference_tag = rumor.tags
        .find(TagKind::e())
        .ok_or("Edit event missing reference event tag")?;

    let message_id = reference_tag.content()
        .ok_or("Edit reference tag has no content")?
        .to_string();

    let edited_at = extract_millisecond_timestamp(&rumor);

    let tags: Vec<Vec<String>> = rumor.tags.iter()
        .map(|tag| {
            tag.as_slice().iter().map(|s| s.to_string()).collect()
        })
        .collect();

    let event = StoredEventBuilder::new()
        .id(rumor.id.to_hex())
        .kind(event_kind::MESSAGE_EDIT)
        .content(rumor.content.clone())
        .tags(tags)
        .reference_id(Some(message_id.clone()))
        .created_at(rumor.created_at.as_secs())
        .mine(context.is_mine)
        .npub(rumor.pubkey.to_bech32().ok())
        .build();

    Ok(RumorProcessingResult::Edit {
        message_id,
        new_content: rumor.content,
        edited_at,
        event,
    })
}

/// Process application-specific data (typing indicators, etc.)
fn process_app_specific(
    rumor: RumorEvent,
    context: RumorContext,
) -> Result<RumorProcessingResult, String> {
    // Check if this is a typing indicator
    if is_typing_indicator(&rumor) {
        let expiry_tag = rumor.tags
            .find(TagKind::Expiration)
            .ok_or("Typing indicator missing expiration tag")?;

        let expiry_timestamp: u64 = expiry_tag.content()
            .ok_or("Expiration tag has no content")?
            .parse()
            .map_err(|_| "Invalid expiration timestamp")?;

        let current_timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| format!("System time error: {}", e))?
            .as_secs();

        if expiry_timestamp <= current_timestamp || expiry_timestamp > current_timestamp + 30 {
            return Ok(RumorProcessingResult::Ignored);
        }

        let profile_id = rumor.pubkey.to_bech32()
            .map_err(|e| format!("Failed to convert pubkey to bech32: {}", e))?;

        return Ok(RumorProcessingResult::TypingIndicator {
            profile_id,
            until: expiry_timestamp,
        });
    }

    // Check if this is a leave request
    if is_leave_request(&rumor) {
        let member_pubkey = rumor.pubkey.to_bech32()
            .map_err(|e| format!("Failed to convert pubkey to bech32: {}", e))?;

        return Ok(RumorProcessingResult::LeaveRequest {
            event_id: rumor.id.to_hex(),
            member_pubkey,
        });
    }

    // Check if this is a PIVX payment
    if is_pivx_payment(&rumor) {
        let gift_code = rumor.tags
            .find(TagKind::Custom(Cow::Borrowed("gift-code")))
            .and_then(|tag| tag.content())
            .ok_or("PIVX payment missing gift-code tag")?
            .to_string();

        let amount_str = rumor.tags
            .find(TagKind::Custom(Cow::Borrowed("amount")))
            .and_then(|tag| tag.content())
            .unwrap_or("0");
        let amount_piv = amount_str.parse::<u64>().unwrap_or(0) as f64 / 100_000_000.0;

        let address = rumor.tags
            .find(TagKind::Custom(Cow::Borrowed("address")))
            .and_then(|tag| tag.content())
            .map(|s| s.to_string());

        let message_id = rumor.id.to_hex();

        let tags: Vec<Vec<String>> = rumor.tags.iter()
            .map(|tag| tag.as_slice().iter().map(|s| s.to_string()).collect())
            .collect();

        let event = StoredEventBuilder::new()
            .id(&message_id)
            .kind(event_kind::APPLICATION_SPECIFIC)
            .chat_id(0) // Will be set by caller
            .content(&rumor.content)
            .tags(tags)
            .created_at(rumor.created_at.as_secs())
            .mine(context.is_mine)
            .npub(Some(rumor.pubkey.to_bech32().unwrap_or_default()))
            .build();

        return Ok(RumorProcessingResult::PivxPayment {
            gift_code,
            amount_piv,
            address,
            message_id,
            event,
        });
    }

    // Check if this is a WebXDC peer advertisement
    if is_webxdc_peer_advertisement(&rumor) {
        log_info!("[WEBXDC] Found peer advertisement rumor, is_mine={}, sender={}",
            context.is_mine,
            rumor.pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()));

        if context.is_mine {
            log_info!("[WEBXDC] Ignoring our own peer advertisement");
            return Ok(RumorProcessingResult::Ignored);
        }

        log_info!("[WEBXDC] Detected peer advertisement in rumor from another device");

        let topic_id = rumor.tags
            .find(TagKind::Custom(Cow::Borrowed("webxdc-topic")))
            .and_then(|tag| tag.content())
            .ok_or("Peer advertisement missing webxdc-topic tag")?
            .to_string();

        let node_addr = rumor.tags
            .find(TagKind::Custom(Cow::Borrowed("webxdc-node-addr")))
            .and_then(|tag| tag.content())
            .ok_or("Peer advertisement missing webxdc-node-addr tag")?
            .to_string();

        let sender_npub = rumor.pubkey.to_bech32().unwrap_or_default();
        return Ok(RumorProcessingResult::WebxdcPeerAdvertisement {
            event_id: rumor.id.to_hex(),
            topic_id,
            node_addr,
            sender_npub,
            created_at: rumor.created_at.as_secs(),
        });
    }

    // Check if this is a WebXDC peer-left signal
    if is_webxdc_peer_left(&rumor) {
        if context.is_mine {
            return Ok(RumorProcessingResult::Ignored);
        }

        log_info!("[WEBXDC] Detected peer-left signal from another device");

        let topic_id = rumor.tags
            .find(TagKind::Custom(Cow::Borrowed("webxdc-topic")))
            .and_then(|tag| tag.content())
            .ok_or("Peer-left missing webxdc-topic tag")?
            .to_string();

        let sender_npub = rumor.pubkey.to_bech32().unwrap_or_default();
        return Ok(RumorProcessingResult::WebxdcPeerLeft {
            event_id: rumor.id.to_hex(),
            topic_id,
            sender_npub,
            created_at: rumor.created_at.as_secs(),
        });
    }

    // Unknown application-specific data
    Ok(RumorProcessingResult::Ignored)
}

/// Check if a rumor is a WebXDC peer advertisement
fn is_webxdc_peer_advertisement(rumor: &RumorEvent) -> bool {
    rumor.content == "peer-advertisement"
        && rumor.tags.find(TagKind::Custom(Cow::Borrowed("webxdc-topic"))).is_some()
        && rumor.tags.find(TagKind::Custom(Cow::Borrowed("webxdc-node-addr"))).is_some()
}

/// Check if a rumor is a WebXDC peer-left signal
fn is_webxdc_peer_left(rumor: &RumorEvent) -> bool {
    rumor.content == "peer-left"
        && rumor.tags.find(TagKind::Custom(Cow::Borrowed("webxdc-topic"))).is_some()
}

/// Check if a rumor is a PIVX payment
fn is_pivx_payment(rumor: &RumorEvent) -> bool {
    rumor.tags
        .find(TagKind::d())
        .and_then(|tag| tag.content())
        .map(|content| content == "pivx-payment")
        .unwrap_or(false)
        && rumor.tags.find(TagKind::Custom(Cow::Borrowed("gift-code"))).is_some()
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
            if let Some(ms_str) = ms_tag.content() {
                if let Ok(ms_value) = ms_str.parse::<u64>() {
                    if ms_value <= 999 {
                        return rumor.created_at.as_secs() * 1000 + ms_value;
                    }
                }
            }
            rumor.created_at.as_secs() * 1000
        }
        None => rumor.created_at.as_secs() * 1000
    }
}

/// Extract reply reference from rumor tags
///
/// Looks for an "e" tag with the "reply" marker to identify
/// which message this rumor is replying to.
fn extract_reply_reference(rumor: &RumorEvent) -> String {
    match rumor.tags.find(TagKind::e()) {
        Some(tag) => {
            // Check via SDK method first, then fallback to manual marker check
            // (Tag::custom may not set internal reply flag)
            if tag.is_reply() {
                tag.content().unwrap_or("").to_string()
            } else {
                let slice = tag.as_slice();
                if slice.get(3).map(|s| s == "reply").unwrap_or(false) {
                    tag.content().unwrap_or("").to_string()
                } else {
                    String::new()
                }
            }
        }
        None => String::new(),
    }
}

/// Check if rumor is a typing indicator
fn is_typing_indicator(rumor: &RumorEvent) -> bool {
    let has_vector_tag = rumor.tags
        .find(TagKind::d())
        .and_then(|tag| tag.content())
        .map(|content| content == "vector")
        .unwrap_or(false);

    let is_typing_content = rumor.content == "typing";

    has_vector_tag && is_typing_content
}

/// Check if rumor is a leave request
fn is_leave_request(rumor: &RumorEvent) -> bool {
    let has_vector_tag = rumor.tags
        .find(TagKind::d())
        .and_then(|tag| tag.content())
        .map(|content| content == "vector")
        .unwrap_or(false);

    let is_leave_content = rumor.content == "leave";

    has_vector_tag && is_leave_content
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::*;

    fn test_keypair() -> Keys {
        Keys::generate()
    }

    /// Build a Tags collection from Tag items
    fn tags(items: Vec<Tag>) -> Tags {
        let mut t = Tags::new();
        for item in items {
            t.push(item);
        }
        t
    }

    /// Create a custom tag (e.g., ["ms", "456"])
    fn custom_tag(key: &str, values: &[&str]) -> Tag {
        let owned: Vec<String> = values.iter().map(|s| s.to_string()).collect();
        Tag::custom(TagKind::custom(key.to_string()), owned)
    }

    fn make_rumor(keys: &Keys, kind: Kind, content: &str, t: Tags) -> RumorEvent {
        RumorEvent {
            id: EventId::all_zeros(),
            kind,
            content: content.to_string(),
            tags: t,
            created_at: Timestamp::from_secs(1700000000),
            pubkey: keys.public_key(),
        }
    }

    fn dm_context(keys: &Keys) -> RumorContext {
        RumorContext {
            sender: keys.public_key(),
            is_mine: false,
            conversation_id: "npub1test".to_string(),
            conversation_type: ConversationType::DirectMessage,
        }
    }

    fn mls_context(keys: &Keys) -> RumorContext {
        RumorContext {
            sender: keys.public_key(),
            is_mine: false,
            conversation_id: "group123".to_string(),
            conversation_type: ConversationType::MlsGroup,
        }
    }

    fn temp_dir() -> std::path::PathBuf {
        std::env::temp_dir().join("vector-rumor-test")
    }

    // ========================================================================
    // Text Message Tests
    // ========================================================================

    #[test]
    fn test_text_message_dm() {
        let keys = test_keypair();
        let rumor = make_rumor(&keys, Kind::PrivateDirectMessage, "Hello world!", Tags::new());
        let ctx = dm_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();

        match result {
            RumorProcessingResult::TextMessage(msg) => {
                assert_eq!(msg.content, "Hello world!");
                assert!(!msg.mine);
                assert!(msg.npub.is_none());
                assert!(msg.attachments.is_empty());
            }
            _ => panic!("Expected TextMessage"),
        }
    }

    #[test]
    fn test_text_message_mls() {
        let keys = test_keypair();
        let rumor = make_rumor(&keys, Kind::from_u16(9), "Group hello!", Tags::new());
        let ctx = mls_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();

        match result {
            RumorProcessingResult::TextMessage(msg) => {
                assert_eq!(msg.content, "Group hello!");
                assert!(msg.npub.is_some());
            }
            _ => panic!("Expected TextMessage"),
        }
    }

    #[test]
    fn test_text_message_mine() {
        let keys = test_keypair();
        let rumor = make_rumor(&keys, Kind::PrivateDirectMessage, "My own message", Tags::new());
        let ctx = RumorContext {
            sender: keys.public_key(),
            is_mine: true,
            conversation_id: "npub1test".to_string(),
            conversation_type: ConversationType::DirectMessage,
        };
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();

        match result {
            RumorProcessingResult::TextMessage(msg) => {
                assert!(msg.mine);
            }
            _ => panic!("Expected TextMessage"),
        }
    }

    #[test]
    fn test_text_message_with_reply() {
        let keys = test_keypair();
        let t = tags(vec![
            Tag::custom(TagKind::e(), ["abc123def456".to_string(), String::new(), "reply".to_string()]),
        ]);
        let rumor = make_rumor(&keys, Kind::PrivateDirectMessage, "Reply text", t);
        let ctx = dm_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();

        match result {
            RumorProcessingResult::TextMessage(msg) => {
                assert_eq!(msg.replied_to, "abc123def456");
            }
            _ => panic!("Expected TextMessage"),
        }
    }

    #[test]
    fn test_text_message_with_ms_timestamp() {
        let keys = test_keypair();
        let t = tags(vec![custom_tag("ms", &["456"])]);
        let rumor = make_rumor(&keys, Kind::PrivateDirectMessage, "Precise time", t);
        let ctx = dm_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();

        match result {
            RumorProcessingResult::TextMessage(msg) => {
                assert_eq!(msg.at, 1700000000 * 1000 + 456);
            }
            _ => panic!("Expected TextMessage"),
        }
    }

    // ========================================================================
    // Reaction Tests
    // ========================================================================

    #[test]
    fn test_reaction() {
        let keys = test_keypair();
        let t = tags(vec![custom_tag("e", &["target_msg_id_hex"])]);
        let rumor = make_rumor(&keys, Kind::Reaction, "👍", t);
        let ctx = dm_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();

        match result {
            RumorProcessingResult::Reaction(reaction) => {
                assert_eq!(reaction.emoji, "👍");
                assert_eq!(reaction.reference_id, "target_msg_id_hex");
            }
            _ => panic!("Expected Reaction"),
        }
    }

    #[test]
    fn test_reaction_missing_e_tag() {
        let keys = test_keypair();
        let rumor = make_rumor(&keys, Kind::Reaction, "👍", Tags::new());
        let ctx = dm_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir());
        assert!(result.is_err());
    }

    // ========================================================================
    // Edit Tests
    // ========================================================================

    #[test]
    fn test_edit_event() {
        let keys = test_keypair();
        let t = tags(vec![custom_tag("e", &["original_msg_id"])]);
        let rumor = make_rumor(&keys, Kind::from_u16(16), "Edited content", t);
        let ctx = dm_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();

        match result {
            RumorProcessingResult::Edit { message_id, new_content, event, .. } => {
                assert_eq!(message_id, "original_msg_id");
                assert_eq!(new_content, "Edited content");
                assert_eq!(event.kind, event_kind::MESSAGE_EDIT);
            }
            _ => panic!("Expected Edit"),
        }
    }

    // ========================================================================
    // Typing Indicator Tests
    // ========================================================================

    #[test]
    fn test_typing_indicator_valid() {
        let keys = test_keypair();
        let future_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap()
            .as_secs() + 10;
        let t = tags(vec![
            Tag::identifier("vector"),
            Tag::expiration(Timestamp::from_secs(future_ts)),
        ]);
        let rumor = make_rumor(&keys, Kind::ApplicationSpecificData, "typing", t);
        let ctx = dm_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();

        match result {
            RumorProcessingResult::TypingIndicator { until, .. } => {
                assert_eq!(until, future_ts);
            }
            _ => panic!("Expected TypingIndicator"),
        }
    }

    #[test]
    fn test_typing_indicator_expired() {
        let keys = test_keypair();
        let t = tags(vec![
            Tag::identifier("vector"),
            Tag::expiration(Timestamp::from_secs(1000000000)),
        ]);
        let rumor = make_rumor(&keys, Kind::ApplicationSpecificData, "typing", t);
        let ctx = dm_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();

        assert!(matches!(result, RumorProcessingResult::Ignored));
    }

    // ========================================================================
    // Leave Request Tests
    // ========================================================================

    #[test]
    fn test_leave_request() {
        let keys = test_keypair();
        let t = tags(vec![Tag::identifier("vector")]);
        let rumor = make_rumor(&keys, Kind::ApplicationSpecificData, "leave", t);
        let ctx = mls_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();

        match result {
            RumorProcessingResult::LeaveRequest { member_pubkey, .. } => {
                assert!(!member_pubkey.is_empty());
                assert!(member_pubkey.starts_with("npub1"));
            }
            _ => panic!("Expected LeaveRequest"),
        }
    }

    // ========================================================================
    // PIVX Payment Tests
    // ========================================================================

    #[test]
    fn test_pivx_payment() {
        let keys = test_keypair();
        let t = tags(vec![
            Tag::identifier("pivx-payment"),
            custom_tag("gift-code", &["ABC12"]),
            custom_tag("amount", &["100000000"]),
            custom_tag("address", &["DTest123"]),
        ]);
        let rumor = make_rumor(&keys, Kind::ApplicationSpecificData, "pivx-payment", t);
        let ctx = dm_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();

        match result {
            RumorProcessingResult::PivxPayment { gift_code, amount_piv, address, .. } => {
                assert_eq!(gift_code, "ABC12");
                assert!((amount_piv - 1.0).abs() < f64::EPSILON);
                assert_eq!(address, Some("DTest123".to_string()));
            }
            _ => panic!("Expected PivxPayment"),
        }
    }

    // ========================================================================
    // WebXDC Tests
    // ========================================================================

    #[test]
    fn test_webxdc_peer_advertisement() {
        let keys = test_keypair();
        let t = tags(vec![
            custom_tag("webxdc-topic", &["topic123"]),
            custom_tag("webxdc-node-addr", &["addr456"]),
        ]);
        let rumor = make_rumor(&keys, Kind::ApplicationSpecificData, "peer-advertisement", t);
        let ctx = dm_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();

        match result {
            RumorProcessingResult::WebxdcPeerAdvertisement { topic_id, node_addr, .. } => {
                assert_eq!(topic_id, "topic123");
                assert_eq!(node_addr, "addr456");
            }
            _ => panic!("Expected WebxdcPeerAdvertisement"),
        }
    }

    #[test]
    fn test_webxdc_peer_advertisement_own_ignored() {
        let keys = test_keypair();
        let t = tags(vec![
            custom_tag("webxdc-topic", &["topic123"]),
            custom_tag("webxdc-node-addr", &["addr456"]),
        ]);
        let rumor = make_rumor(&keys, Kind::ApplicationSpecificData, "peer-advertisement", t);
        let ctx = RumorContext {
            sender: keys.public_key(),
            is_mine: true,
            conversation_id: "npub1test".to_string(),
            conversation_type: ConversationType::DirectMessage,
        };
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();
        assert!(matches!(result, RumorProcessingResult::Ignored));
    }

    #[test]
    fn test_webxdc_peer_left() {
        let keys = test_keypair();
        let t = tags(vec![custom_tag("webxdc-topic", &["topic123"])]);
        let rumor = make_rumor(&keys, Kind::ApplicationSpecificData, "peer-left", t);
        let ctx = dm_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();

        match result {
            RumorProcessingResult::WebxdcPeerLeft { topic_id, .. } => {
                assert_eq!(topic_id, "topic123");
            }
            _ => panic!("Expected WebxdcPeerLeft"),
        }
    }

    // ========================================================================
    // Unknown Event Tests
    // ========================================================================

    #[test]
    fn test_unknown_kind() {
        let keys = test_keypair();
        let rumor = make_rumor(&keys, Kind::from_u16(65535), "Mystery event", Tags::new());
        let ctx = dm_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();

        match result {
            RumorProcessingResult::UnknownEvent(event) => {
                assert_eq!(event.kind, 65535);
                assert_eq!(event.content, "Mystery event");
            }
            _ => panic!("Expected UnknownEvent"),
        }
    }

    // ========================================================================
    // File Attachment Tests
    // ========================================================================

    #[test]
    fn test_file_attachment() {
        let keys = test_keypair();
        let ox_hash = "deadbeef".repeat(8); // 64 hex chars
        let t = tags(vec![
            custom_tag("decryption-key", &["aabbccdd"]),
            custom_tag("decryption-nonce", &["11223344"]),
            custom_tag("ox", &[&ox_hash]),
            custom_tag("file-type", &["image/jpeg"]),
            custom_tag("name", &["photo.jpg"]),
            custom_tag("size", &["12345"]),
        ]);
        let rumor = make_rumor(&keys, Kind::from_u16(15), "https://blossom.example/deadbeef.jpg", t);
        let ctx = dm_context(&keys);
        let dir = temp_dir();
        let result = process_rumor(rumor, ctx, &dir).unwrap();

        match result {
            RumorProcessingResult::FileAttachment(msg) => {
                assert_eq!(msg.attachments.len(), 1);
                let att = &msg.attachments[0];
                assert_eq!(att.key, "aabbccdd");
                assert_eq!(att.nonce, "11223344");
                assert_eq!(att.extension, "jpg");
                assert_eq!(att.name, "photo.jpg");
                assert_eq!(att.size, 12345);
                assert!(!att.downloaded);
            }
            _ => panic!("Expected FileAttachment"),
        }
    }

    #[test]
    fn test_file_attachment_empty_hash_rejected() {
        let keys = test_keypair();
        let t = tags(vec![
            custom_tag("decryption-key", &["aabbccdd"]),
            custom_tag("decryption-nonce", &["11223344"]),
            custom_tag("file-type", &["image/jpeg"]),
        ]);
        let empty_hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let rumor = make_rumor(&keys, Kind::from_u16(15), &format!("https://blossom.example/{}", empty_hash), t);
        let ctx = dm_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir());
        assert!(result.is_err());
    }

    #[test]
    fn test_file_attachment_with_image_meta() {
        let keys = test_keypair();
        let ox_hash = "a".repeat(64);
        let t = tags(vec![
            custom_tag("decryption-key", &["aabbccdd"]),
            custom_tag("decryption-nonce", &["11223344"]),
            custom_tag("ox", &[&ox_hash]),
            custom_tag("file-type", &["image/png"]),
            custom_tag("thumbhash", &["base64data"]),
            custom_tag("dim", &["1920x1080"]),
            custom_tag("size", &["5000"]),
        ]);
        let rumor = make_rumor(&keys, Kind::from_u16(15), "https://blossom.example/aaa.png", t);
        let ctx = dm_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();

        match result {
            RumorProcessingResult::FileAttachment(msg) => {
                let att = &msg.attachments[0];
                let meta = att.img_meta.as_ref().unwrap();
                assert_eq!(meta.width, 1920);
                assert_eq!(meta.height, 1080);
                assert_eq!(meta.thumbhash, "base64data");
            }
            _ => panic!("Expected FileAttachment"),
        }
    }

    // ========================================================================
    // Helper Function Tests
    // ========================================================================

    #[test]
    fn test_extract_hash_from_blossom_url() {
        let hash = "a".repeat(64);
        let url = format!("https://blossom.example/{}.jpg", hash);
        assert_eq!(extract_hash_from_blossom_url(&url), Some(hash));

        assert_eq!(extract_hash_from_blossom_url("https://example.com/short"), None);
        assert_eq!(extract_hash_from_blossom_url("https://example.com/not-hex-at-all-but-exactly-sixty-four-characters-long-string-here!"), None);
    }

    #[test]
    fn test_unknown_app_specific_ignored() {
        let keys = test_keypair();
        let t = tags(vec![Tag::identifier("some-other-app")]);
        let rumor = make_rumor(&keys, Kind::ApplicationSpecificData, "unknown-content", t);
        let ctx = dm_context(&keys);
        let result = process_rumor(rumor, ctx, &temp_dir()).unwrap();
        assert!(matches!(result, RumorProcessingResult::Ignored));
    }
}
