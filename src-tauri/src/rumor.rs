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
use crate::{Message, Attachment, Reaction, TAURI_APP, StoredEvent, StoredEventBuilder};
use crate::message::ImageMetadata;
use crate::util::{bytes_to_hex_string, hex_string_to_bytes};
use crate::mls::MlsService;

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
        topic_id: String,
        node_addr: String,
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
        // Text messages - Kind 9 (MLS/White Noise) or Kind 14 (DMs/legacy)
        Kind::PrivateDirectMessage => {
            process_text_message(rumor, context).await
        }
        k if k.as_u16() == crate::stored_event::event_kind::MLS_CHAT_MESSAGE => {
            process_text_message(rumor, context).await
        }
        // File attachments
        k if k.as_u16() == 15 => {
            process_file_attachment(rumor, context).await
        }
        // Message edits
        k if k.as_u16() == crate::stored_event::event_kind::MESSAGE_EDIT => {
            process_edit_event(rumor, context).await
        }
        // Emoji reactions
        Kind::Reaction => {
            process_reaction(rumor, context).await
        }
        // Application-specific data (typing indicators, etc.)
        Kind::ApplicationSpecificData => {
            process_app_specific(rumor, context).await
        }
        // Unknown or unsupported kind - store for future compatibility
        _ => {
            process_unknown_event(rumor, context).await
        }
    }
}

/// Process an unknown event type
///
/// Creates a StoredEvent for unknown kinds so they can be stored
/// and potentially displayed/processed in future versions.
async fn process_unknown_event(
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
/// For MLS groups, also checks for imeta tags (MIP-04 file attachments).
async fn process_text_message(
    rumor: RumorEvent,
    context: RumorContext,
) -> Result<RumorProcessingResult, String> {
    // Extract reply reference if present
    let replied_to = extract_reply_reference(&rumor);

    // Extract millisecond-precision timestamp
    let ms_timestamp = extract_millisecond_timestamp(&rumor);

    // Check for imeta tags (MIP-04 file attachments in MLS groups)
    let attachments = if context.conversation_type == ConversationType::MlsGroup {
        parse_mls_imeta_attachments(&rumor, &context).await
    } else {
        Vec::new()
    };

    // Extract webxdc-topic for Mini Apps (realtime channel isolation)
    let webxdc_topic = rumor.tags
        .find(TagKind::Custom(Cow::Borrowed("webxdc-topic")))
        .and_then(|tag| tag.content())
        .map(|s| s.to_string());

    // If we have attachments, apply webxdc_topic to them
    let attachments = if let Some(topic) = webxdc_topic {
        attachments.into_iter().map(|mut att| {
            att.webxdc_topic = Some(topic.clone());
            att
        }).collect()
    } else {
        attachments
    };

    // Determine result type based on whether we have attachments
    let has_attachments = !attachments.is_empty();

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
        attachments,
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
        wrapper_event_id: None, // Set by caller after processing
        edited: false,
        edit_history: None,
    };

    // Return as FileAttachment if we have MIP-04 attachments, otherwise TextMessage
    if has_attachments {
        Ok(RumorProcessingResult::FileAttachment(msg))
    } else {
        Ok(RumorProcessingResult::TextMessage(msg))
    }
}

/// Parse MIP-04 imeta tags from an MLS group message
///
/// Extracts file attachments from imeta tags using MDK's encrypted media parser.
/// Returns a list of Attachment objects with group_id set for MLS decryption.
async fn parse_mls_imeta_attachments(
    rumor: &RumorEvent,
    context: &RumorContext,
) -> Vec<Attachment> {
    // Find all imeta tags
    let imeta_tags: Vec<&Tag> = rumor.tags.iter()
        .filter(|t| t.kind() == TagKind::Custom(Cow::Borrowed("imeta")))
        .collect();

    if imeta_tags.is_empty() {
        return Vec::new();
    }

    // Try to get MDK media manager for this group
    let handle = match TAURI_APP.get() {
        Some(h) => h,
        None => {
            eprintln!("[MIP-04] App handle not available for imeta parsing");
            return Vec::new();
        }
    };

    let mls_service = match MlsService::new_persistent(handle) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[MIP-04] Failed to create MLS service: {}", e);
            return Vec::new();
        }
    };

    // Look up the group metadata to get the engine_group_id
    let groups = match mls_service.read_groups().await {
        Ok(g) => g,
        Err(e) => {
            eprintln!("[MIP-04] Failed to read groups: {}", e);
            return Vec::new();
        }
    };

    let group_meta = match groups.iter().find(|g| g.group_id == context.conversation_id) {
        Some(g) => g,
        None => {
            eprintln!("[MIP-04] Group not found: {}", context.conversation_id);
            return Vec::new();
        }
    };

    if group_meta.engine_group_id.is_empty() {
        eprintln!("[MIP-04] Group has no engine_group_id");
        return Vec::new();
    }

    // Parse the engine group ID
    let engine_gid_bytes = hex_string_to_bytes(&group_meta.engine_group_id);
    let gid = mdk_core::GroupId::from_slice(&engine_gid_bytes);

    // Get MDK engine and media manager
    let mdk = match mls_service.engine() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[MIP-04] Failed to get MDK engine: {}", e);
            return Vec::new();
        }
    };
    let media_manager = mdk.media_manager(gid);

    let mut attachments = Vec::new();

    for tag in imeta_tags {
        // Convert nostr_sdk::Tag to nostr::Tag for MDK parsing
        let tag_values: Vec<String> = tag.as_slice().iter().map(|s| s.to_string()).collect();
        let mdk_tag = match nostr::Tag::parse(&tag_values) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[MIP-04] Failed to parse imeta tag: {}", e);
                continue;
            }
        };

        // Parse the imeta tag using MDK
        let media_ref = match media_manager.parse_imeta_tag(&mdk_tag) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[MIP-04] Failed to parse imeta: {}", e);
                continue;
            }
        };

        // Extract file extension from filename or URL
        let extension = media_ref.filename
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_string();

        // Extract hash from URL (Blossom URLs typically have hash as the path)
        let encrypted_hash = extract_hash_from_blossom_url(&media_ref.url)
            .unwrap_or_else(|| bytes_to_hex_string(&media_ref.original_hash));

        // Build image metadata from dimensions if available
        let img_meta = media_ref.dimensions.and_then(|(width, height)| {
            // Look for blurhash in the original tag
            let blurhash = tag.as_slice().iter()
                .find(|s| s.starts_with("blurhash "))
                .map(|s| s.strip_prefix("blurhash ").unwrap_or("").to_string());

            // Only create ImageMetadata if we have a valid blurhash (at least 6 chars)
            match blurhash {
                Some(bh) if bh.len() >= 6 => Some(ImageMetadata {
                    blurhash: bh,
                    width,
                    height,
                }),
                _ => {
                    // Still return dimensions without blurhash for sizing purposes
                    Some(ImageMetadata {
                        blurhash: String::new(),
                        width,
                        height,
                    })
                }
            }
        });

        // Get download directory for file path
        let base_directory = if cfg!(target_os = "ios") {
            tauri::path::BaseDirectory::Document
        } else {
            tauri::path::BaseDirectory::Download
        };
        let dir = handle.path().resolve("vector", base_directory)
            .unwrap_or_default();
        let file_path = dir.join(format!("{}.{}", &encrypted_hash, &extension))
            .to_string_lossy()
            .to_string();

        // Check if file already exists locally
        let downloaded = std::path::Path::new(&file_path).exists();

        // Extract size from imeta tag (format: "size 12345")
        let size = tag.as_slice().iter()
            .find(|s| s.starts_with("size "))
            .and_then(|s| s.strip_prefix("size "))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        attachments.push(Attachment {
            id: encrypted_hash,
            key: String::new(),  // MLS uses derived keys
            nonce: bytes_to_hex_string(&media_ref.nonce),
            extension,
            url: media_ref.url.clone(),
            path: file_path,
            size,
            img_meta,
            downloading: false,
            downloaded,
            webxdc_topic: None,  // Set by caller if present
            group_id: Some(context.conversation_id.clone()),
            original_hash: Some(bytes_to_hex_string(&media_ref.original_hash)),
            scheme_version: Some(media_ref.scheme_version.clone()),
            mls_filename: Some(media_ref.filename.clone()),
        });
    }

    attachments
}

/// Extract SHA256 hash from a Blossom URL
///
/// Blossom URLs typically follow the format: https://server.com/<sha256hash>[.ext]
fn extract_hash_from_blossom_url(url: &str) -> Option<String> {
    // Get the path component
    let path = url.split('/').last()?;
    // Remove file extension if present
    let hash_part = path.split('.').next()?;
    // Validate it looks like a SHA256 hash (64 hex chars)
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
            // For group chats, include sender's npub
            rumor.pubkey.to_bech32().ok()
        } else {
            // For DMs, npub is implicit (the other participant)
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

/// Process a message edit rumor
///
/// Extracts the edited content and references the original message.
async fn process_edit_event(
    rumor: RumorEvent,
    context: RumorContext,
) -> Result<RumorProcessingResult, String> {
    // Find the reference event (the message being edited)
    let reference_tag = rumor.tags
        .find(TagKind::e())
        .ok_or("Edit event missing reference event tag")?;

    let message_id = reference_tag.content()
        .ok_or("Edit reference tag has no content")?
        .to_string();

    // Extract millisecond-precision timestamp
    let edited_at = extract_millisecond_timestamp(&rumor);

    // Convert tags to Vec<Vec<String>> format for storage
    let tags: Vec<Vec<String>> = rumor.tags.iter()
        .map(|tag| {
            tag.as_slice().iter().map(|s| s.to_string()).collect()
        })
        .collect();

    // Create StoredEvent for persistence
    let event = StoredEventBuilder::new()
        .id(rumor.id.to_hex())
        .kind(crate::stored_event::event_kind::MESSAGE_EDIT)
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
///
/// Currently handles typing indicators for real-time status updates.
async fn process_app_specific(
    rumor: RumorEvent,
    context: RumorContext,
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

    // Check if this is a leave request (member wants to leave the group)
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
        // Extract gift code from tags
        let gift_code = rumor.tags
            .find(TagKind::Custom(Cow::Borrowed("gift-code")))
            .and_then(|tag| tag.content())
            .ok_or("PIVX payment missing gift-code tag")?
            .to_string();

        // Extract amount from tags (in satoshis, convert to PIV)
        let amount_str = rumor.tags
            .find(TagKind::Custom(Cow::Borrowed("amount")))
            .and_then(|tag| tag.content())
            .unwrap_or("0");
        let amount_piv = amount_str.parse::<u64>().unwrap_or(0) as f64 / 100_000_000.0;

        // Extract address from tags (for balance checking, optional for older events)
        let address = rumor.tags
            .find(TagKind::Custom(Cow::Borrowed("address")))
            .and_then(|tag| tag.content())
            .map(|s| s.to_string());

        let message_id = rumor.id.to_hex();

        // Convert rumor tags to StoredEvent format
        let tags: Vec<Vec<String>> = rumor.tags.iter()
            .map(|tag| tag.as_slice().iter().map(|s| s.to_string()).collect())
            .collect();

        // Create StoredEvent for persistence (chat_id will be set by caller)
        let event = StoredEventBuilder::new()
            .id(&message_id)
            .kind(crate::stored_event::event_kind::APPLICATION_SPECIFIC)
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
        println!("[WEBXDC] Found peer advertisement rumor, is_mine={}, sender={}",
            context.is_mine,
            rumor.pubkey.to_bech32().unwrap_or_else(|_| "unknown".to_string()));
        
        // Skip our own peer advertisements - we don't need to connect to ourselves
        if context.is_mine {
            println!("[WEBXDC] Ignoring our own peer advertisement");
            return Ok(RumorProcessingResult::Ignored);
        }
        
        println!("[WEBXDC] Detected peer advertisement in rumor from another device");
        
        // Extract topic ID and node address
        let topic_id = rumor.tags
            .find(TagKind::Custom(std::borrow::Cow::Borrowed("webxdc-topic")))
            .and_then(|tag| tag.content())
            .ok_or("Peer advertisement missing webxdc-topic tag")?
            .to_string();
        
        let node_addr = rumor.tags
            .find(TagKind::Custom(std::borrow::Cow::Borrowed("webxdc-node-addr")))
            .and_then(|tag| tag.content())
            .ok_or("Peer advertisement missing webxdc-node-addr tag")?
            .to_string();
        
        // Validate expiration (peer advertisements should be short-lived)
        if let Some(expiry_tag) = rumor.tags.find(TagKind::Expiration) {
            if let Some(expiry_str) = expiry_tag.content() {
                if let Ok(expiry_timestamp) = expiry_str.parse::<u64>() {
                    let current_timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|e| format!("System time error: {}", e))?
                        .as_secs();
                    
                    // Reject expired advertisements
                    if expiry_timestamp <= current_timestamp {
                        return Ok(RumorProcessingResult::Ignored);
                    }
                }
            }
        }
        
        return Ok(RumorProcessingResult::WebxdcPeerAdvertisement {
            topic_id,
            node_addr,
        });
    }
    
    // Unknown application-specific data
    Ok(RumorProcessingResult::Ignored)
}

/// Check if a rumor is a WebXDC peer advertisement
fn is_webxdc_peer_advertisement(rumor: &RumorEvent) -> bool {
    rumor.content == "peer-advertisement"
        && rumor.tags.find(TagKind::Custom(std::borrow::Cow::Borrowed("webxdc-topic"))).is_some()
        && rumor.tags.find(TagKind::Custom(std::borrow::Cow::Borrowed("webxdc-node-addr"))).is_some()
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
            // Get the ms value and append it to the timestamp
            if let Some(ms_str) = ms_tag.content() {
                if let Ok(ms_value) = ms_str.parse::<u64>() {
                    // Validate that ms is between 0-999
                    if ms_value <= 999 {
                        return rumor.created_at.as_secs() * 1000 + ms_value;
                    }
                }
            }
            // Fallback to seconds if ms tag is invalid
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

/// Check if rumor is a leave request
///
/// Validates that the rumor has:
/// - d tag with value "vector"
/// - content "leave"
fn is_leave_request(rumor: &RumorEvent) -> bool {
    // Check d tag
    let has_vector_tag = rumor.tags
        .find(TagKind::d())
        .and_then(|tag| tag.content())
        .map(|content| content == "vector")
        .unwrap_or(false);

    // Check content
    let is_leave_content = rumor.content == "leave";

    has_vector_tag && is_leave_content
}