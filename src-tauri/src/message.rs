use std::sync::Arc;
use std::collections::HashMap;
use ::image::{ImageBuffer, ImageEncoder, Rgba};
use nostr_sdk::prelude::*;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tokio::sync::Mutex as TokioMutex;
use once_cell::sync::Lazy;

use crate::crypto;
use crate::db::{self, save_chat};
use crate::net;
use crate::STATE;
use crate::util::{self, calculate_file_hash};
use crate::TAURI_APP;
use crate::NOSTR_CLIENT;
use crate::miniapps::realtime::{generate_topic_id, encode_topic_id};

/// Cached compressed image data
#[derive(Clone)]
pub struct CachedCompressedImage {
    pub bytes: Vec<u8>,
    pub extension: String,
    pub img_meta: Option<ImageMetadata>,
    pub original_size: u64,
    pub compressed_size: u64,
}

/// Global cache for pre-compressed images
static COMPRESSION_CACHE: Lazy<TokioMutex<HashMap<String, Option<CachedCompressedImage>>>> =
    Lazy::new(|| TokioMutex::new(HashMap::new()));

/// Cache for Android file bytes: uri -> (bytes, extension, name, size)
/// This is used to cache file bytes immediately after file selection on Android,
/// before the temporary content URI permission expires.
static ANDROID_FILE_CACHE: Lazy<std::sync::Mutex<HashMap<String, (Vec<u8>, String, String, u64)>>> =
    Lazy::new(|| std::sync::Mutex::new(HashMap::new()));

#[cfg(target_os = "android")]
use crate::android::{clipboard, filesystem};

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
    // The encryption key
    pub key: String,
    // The encryption nonce
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
        }
    }
}

/// A simple pre-upload format to associate a byte stream with a file extension
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct AttachmentFile {
    pub bytes: Vec<u8>,
    /// Image metadata (for images only)
    pub img_meta: Option<ImageMetadata>,
    pub extension: String,
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

/// Helper function to mark message as failed and update frontend
async fn mark_message_failed(pending_id: Arc<String>, receiver: &str) {
    // Find the message in chats and mark it as failed
    let mut state = STATE.lock().await;
    
    // Search through all chats to find the message with this pending ID
    for chat in &mut state.chats {
        if chat.has_participant(receiver) {
            if let Some(message) = chat.messages.iter_mut().find(|m| m.id == *pending_id) {
                // Mark the message as failed
                message.failed = true;
                message.pending = false;
                
                // Update the frontend
                let handle = TAURI_APP.get().unwrap();
                handle.emit("message_update", serde_json::json!({
                    "old_id": pending_id.as_ref(),
                    "message": message,
                    "chat_id": receiver
                })).unwrap();
                
                // Save the failed message to our DB
                let message_to_save = message.clone();
                drop(state); // Release lock before async DB operation
                let _ = crate::db::save_message(handle.clone(), receiver, &message_to_save).await;
                break;
            }
        }
    }
}

#[tauri::command]
pub async fn message(receiver: String, content: String, replied_to: String, file: Option<AttachmentFile>) -> Result<bool, String> {
    // Immediately add the message to our state as "Pending" with an ID derived from the current nanosecond, we'll update it as either Sent (non-pending) or Failed in the future
    let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
    // Create persistent pending_id that will live for the entire function
    let pending_id = Arc::new(String::from("pending-") + &current_time.as_nanos().to_string());
    let msg = Message {
        id: pending_id.as_ref().clone(),
        content,
        replied_to,
        replied_to_content: None, // Will be populated when loaded from DB
        replied_to_npub: None,
        replied_to_has_attachment: None,
        preview_metadata: None,
        at: current_time.as_millis() as u64,
        attachments: Vec::new(),
        reactions: Vec::new(),
        pending: true,
        failed: false,
        mine: true,
        npub: None, // Pending messages don't need npub (they're always mine)
        wrapper_event_id: None, // Will be set when message is sent
        edited: false,
        edit_history: None,
    };
    // Grab our pubkey first
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Detect if this is a group chat or DM
    // First check if a chat already exists and use its type
    // Otherwise, check if receiver is a valid bech32 npub (DM) or not (group)
    let is_group_chat = {
        let state = STATE.lock().await;
        if let Some(chat) = state.get_chat(&receiver) {
            // Chat exists, use its type
            chat.is_mls_group()
        } else {
            // Chat doesn't exist, detect by receiver format
            // If it's a valid npub (starts with "npub1"), it's a DM
            // Otherwise it's a group_id
            !receiver.starts_with("npub1")
        }
    };
    
    // Add message to appropriate chat type
    {
        let mut state = STATE.lock().await;
        if is_group_chat {
            // For groups, create or get the MLS group chat
            state.create_or_get_mls_group_chat(&receiver, vec![]);
            state.add_message_to_chat(&receiver, msg.clone());
        } else {
            // For DMs, use the existing participant-based method
            state.add_message_to_participant(&receiver, msg.clone());
        }
    }

    // For DMs, convert the Bech32 String to a PublicKey
    // For groups, we'll handle it differently below
    let receiver_pubkey = if !is_group_chat {
        PublicKey::from_bech32(receiver.clone().as_str())
            .map_err(|e| format!("Invalid npub: {}", e))?
    } else {
        // For groups, we don't need a receiver_pubkey for the rumor
        // We'll use a placeholder that won't be used
        my_public_key
    };

    // Prepare the rumor
    let handle = TAURI_APP.get().unwrap();
    let mut rumor = if file.is_none() {
        // Send the text message to our frontend with appropriate event
        if is_group_chat {
            handle.emit("mls_message_new", serde_json::json!({
                "group_id": &receiver,
                "message": &msg
            })).unwrap();
        } else {
            handle.emit("message_new", serde_json::json!({
                "message": &msg,
                "chat_id": &receiver
            })).unwrap();
        }

        // Text Message
        if !is_group_chat {
            EventBuilder::private_msg_rumor(receiver_pubkey, msg.content)
        } else {
            // For MLS groups, create a basic rumor without p-tag
            EventBuilder::new(Kind::from_u16(14), msg.content)
        }
    } else {
        let attached_file = file.unwrap();

        // Calculate the file hash first (before encryption)
        let file_hash = calculate_file_hash(&attached_file.bytes);
        
        // The SHA-256 hash of an empty file - we should never reuse this
        const EMPTY_FILE_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

        // Check for existing attachment with same hash across all profiles BEFORE encrypting
        // BUT: Never reuse empty file hashes - always force a new upload
        let existing_attachment = if file_hash == EMPTY_FILE_HASH {
            None
        } else {
            let mut found_attachment: Option<(String, Attachment)> = None;
            
            // First, search through in-memory state (fastest check)
            {
                let state = STATE.lock().await;
                for chat in &state.chats {
                    for message in &chat.messages {
                        for attachment in &message.attachments {
                            if attachment.id == file_hash && !attachment.url.is_empty() {
                                // Found a matching attachment with a valid URL
                                // For DMs, use first participant; for groups, use chat ID
                                let chat_identifier = if let Some(participant_id) = chat.participants.first() {
                                    participant_id.clone()
                                } else {
                                    // Group chat - use the chat ID itself
                                    chat.id.clone()
                                };
                                found_attachment = Some((chat_identifier, attachment.clone()));
                                break;
                            }
                        }
                        if found_attachment.is_some() {
                            break;
                        }
                    }
                    if found_attachment.is_some() {
                        break;
                    }
                }
            }
            
            // Fallback: check database index if not found in memory (covers all stored attachments)
            if found_attachment.is_none() {
                if let Ok(index) = db::build_file_hash_index(handle).await {
                    if let Some(attachment_ref) = index.get(&file_hash) {
                        // Found in database index - convert AttachmentRef to Attachment
                        found_attachment = Some((attachment_ref.chat_id.clone(), Attachment {
                            id: attachment_ref.hash.clone(),
                            url: attachment_ref.url.clone(),
                            key: attachment_ref.key.clone(),
                            nonce: attachment_ref.nonce.clone(),
                            extension: attachment_ref.extension.clone(),
                            size: attachment_ref.size,
                            path: String::new(),
                            img_meta: None,
                            downloading: false,
                            downloaded: false,
                            webxdc_topic: None, // Not stored in attachment index
                        }));
                    }
                }
            }
            
            found_attachment
        };

        // Determine if we need to encrypt based on whether we'll reuse an existing attachment
        let will_reuse_existing = if let Some((_, ref existing)) = existing_attachment {
            // Check if URL contains empty hash - never reuse those
            const EMPTY_FILE_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
            if existing.url.contains(EMPTY_FILE_HASH) {
                false
            } else {
                // Check if URL is live
                match net::check_url_live(&existing.url).await {
                    Ok(is_live) => is_live,
                    Err(_) => false
                }
            }
        } else {
            false
        };

        // Only encrypt if we won't reuse an existing attachment
        let (params, enc_file) = if will_reuse_existing {
            // Skip encryption for duplicate files - we'll reuse existing encryption params
            (crypto::EncryptionParams { key: String::new(), nonce: String::new() }, Vec::new())
        } else {
            // Encrypt the attachment - either it's new or the existing URL is dead
            let params = crypto::generate_encryption_params();
            let enc_file = crypto::encrypt_data(attached_file.bytes.as_slice(), &params).unwrap();
            (params, enc_file)
        };

        // For WebXDC (.xdc) files, generate topic ID upfront so it's available immediately
        // This needs to be outside the block so it's available when building the Nostr event
        let webxdc_topic = if attached_file.extension.to_lowercase() == "xdc" {
            let topic_id = generate_topic_id();
            Some(encode_topic_id(&topic_id))
        } else {
            None
        };

        // Update the attachment in-state
        {
            // Use a clone of the Arc for this block
            let pending_id_clone = Arc::clone(&pending_id);
            
            // Retrieve the Pending Message
            let mut state = STATE.lock().await;
            let message = state.chats.iter_mut()
                .find(|chat| {
                    // For DMs, check if receiver is a participant
                    // For MLS groups, check if receiver matches the chat ID
                    chat.id() == &receiver || chat.has_participant(&receiver)
                })
                .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == *pending_id_clone))
                .unwrap();

            // Choose the appropriate base directory based on platform
            let base_directory = if cfg!(target_os = "ios") {
                tauri::path::BaseDirectory::Document
            } else {
                tauri::path::BaseDirectory::Download
            };

            // Resolve the directory path using the determined base directory
            let dir = handle.path().resolve("vector", base_directory).unwrap();

            // Store the hash-based file name on-disk for future reference
            let hash_file_path = dir.join(format!("{}.{}", &file_hash, &attached_file.extension));

            // Create the vector directory if it doesn't exist
            std::fs::create_dir_all(&dir).unwrap();

            // Save the hash-named file
            std::fs::write(&hash_file_path, &attached_file.bytes).unwrap();

            // Determine encryption params and file size based on whether we found an existing attachment
            let (attachment_key, attachment_nonce, file_size) = if let Some((_, ref existing)) = existing_attachment {
                // Reuse existing encryption params
                (existing.key.clone(), existing.nonce.clone(), existing.size)
            } else {
                // Use new encryption params and encrypted file size
                (params.key.clone(), params.nonce.clone(), enc_file.len() as u64)
            };

            // Add the Attachment in-state (with our local path, to prevent re-downloading it accidentally from server)
            message.attachments.push(Attachment {
                // Use SHA256 hash as the ID
                id: file_hash.clone(),
                key: attachment_key,
                nonce: attachment_nonce,
                extension: attached_file.extension.clone(),
                url: String::new(),
                path: hash_file_path.to_string_lossy().to_string(),
                size: file_size,
                img_meta: attached_file.img_meta.clone(),
                downloading: false,
                downloaded: true,
                webxdc_topic: webxdc_topic.clone(),
            });

            // Send the pending file upload to our frontend with appropriate event
            // This provides immediate UI feedback for the sender
            if is_group_chat {
                handle.emit("mls_message_new", serde_json::json!({
                    "group_id": &receiver,
                    "message": &message
                })).unwrap();
            } else {
                handle.emit("message_new", serde_json::json!({
                    "message": &message,
                    "chat_id": &receiver
                })).unwrap();
            }
        }

        // Format a Mime Type from the file extension
        let mime_type = util::mime_from_extension(&attached_file.extension);

        // Check if we found an existing attachment with the same hash
        let mut should_upload = true;
        let attachment_rumor = if let Some((_found_profile_id, existing_attachment)) = existing_attachment {
            // Never reuse URLs with the empty file hash
            const EMPTY_FILE_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
            let is_empty_hash = existing_attachment.url.contains(EMPTY_FILE_HASH);
            
            // Verify the URL is still live before reusing (but skip if it's an empty hash)
            let url_is_live = if is_empty_hash {
                false
            } else {
                match net::check_url_live(&existing_attachment.url).await {
                    Ok(is_live) => is_live,
                    Err(_) => false // Treat errors as dead URL
                }
            };
            
            if url_is_live {
                should_upload = false;
                
                // Update our pending message with the existing URL
                {
                    let pending_id_for_update = Arc::clone(&pending_id);
                    let mut state = STATE.lock().await;
                    let message = state.chats.iter_mut()
                        .find(|chat| chat.id() == &receiver || chat.has_participant(&receiver))
                        .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == *pending_id_for_update))
                        .unwrap();
                    if let Some(attachment) = message.attachments.last_mut() {
                        attachment.url = existing_attachment.url.clone();
                    }
                }
                
                // Create the attachment rumor with the existing URL
                let mut attachment_rumor = EventBuilder::new(Kind::from_u16(15), existing_attachment.url);
                
                // Only add p-tag for DMs, not for MLS groups
                if !is_group_chat {
                    attachment_rumor = attachment_rumor.tag(Tag::public_key(receiver_pubkey));
                }
                
                // Append decryption keys and file metadata (using existing attachment's params)
                attachment_rumor = attachment_rumor
                    .tag(Tag::custom(TagKind::custom("file-type"), [mime_type.as_str()]))
                    .tag(Tag::custom(TagKind::custom("size"), [existing_attachment.size.to_string()]))
                    .tag(Tag::custom(TagKind::custom("encryption-algorithm"), ["aes-gcm"]))
                    .tag(Tag::custom(TagKind::custom("decryption-key"), [existing_attachment.key.as_str()]))
                    .tag(Tag::custom(TagKind::custom("decryption-nonce"), [existing_attachment.nonce.as_str()]))
                    .tag(Tag::custom(TagKind::custom("ox"), [file_hash.clone()]));
                
                // Append image metadata if available
                if let Some(ref img_meta) = attached_file.img_meta {
                    attachment_rumor = attachment_rumor
                        .tag(Tag::custom(TagKind::custom("blurhash"), [&img_meta.blurhash]))
                        .tag(Tag::custom(TagKind::custom("dim"), [format!("{}x{}", img_meta.width, img_meta.height)]));
                }

                // For WebXDC (.xdc) files, use the topic ID from the attachment (generated earlier)
                if let Some(ref topic_encoded) = webxdc_topic {
                    attachment_rumor = attachment_rumor
                        .tag(Tag::custom(TagKind::custom("webxdc-topic"), [topic_encoded.clone()]));
                }

                attachment_rumor
            } else {
                // URL is dead, need to upload
                should_upload = true;
                EventBuilder::new(Kind::from_u16(15), String::new()) // Placeholder
            }
        } else {
            // No existing attachment found
            EventBuilder::new(Kind::from_u16(15), String::new()) // Placeholder
        };
        
        // Final attachment rumor - either reused or newly uploaded
        let final_attachment_rumor = if should_upload {
            // Upload the file to the server
            let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
            let signer = client.signer().await.unwrap();
            let servers = crate::get_blossom_servers();
            let file_size = enc_file.len();
            // Clone the Arc outside the closure for use inside a seperate-threaded progress callback
            let pending_id_for_callback = Arc::clone(&pending_id);
            // Create a progress callback for file uploads
            let progress_callback: crate::blossom::ProgressCallback = std::sync::Arc::new(move |percentage, _bytes| {
                    if let Some(pct) = percentage {
                        handle.emit("attachment_upload_progress", serde_json::json!({
                            "id": pending_id_for_callback.as_ref(),
                            "progress": pct
                        })).unwrap();
                    }
                Ok(())
            });

            // Upload the file with progress, retries, and automatic server failover
            match crate::blossom::upload_blob_with_progress_and_failover(signer.clone(), servers, enc_file, Some(mime_type.as_str()), progress_callback, Some(3), Some(std::time::Duration::from_secs(2))).await {
                Ok(url) => {
                    // Update our pending message with the uploaded URL
                    {
                        let pending_id_for_url_update = Arc::clone(&pending_id);
                        let mut state = STATE.lock().await;
                        let message = state.chats.iter_mut()
                            .find(|chat| chat.id() == &receiver || chat.has_participant(&receiver))
                            .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == *pending_id_for_url_update))
                            .unwrap();
                        if let Some(attachment) = message.attachments.last_mut() {
                            attachment.url = url.clone();
                        }
                    }
                    
                    // Create the attachment rumor
                    let mut attachment_rumor = EventBuilder::new(Kind::from_u16(15), url);
                    
                    // Only add p-tag for DMs, not for MLS groups
                    if !is_group_chat {
                        attachment_rumor = attachment_rumor.tag(Tag::public_key(receiver_pubkey));
                    }

                    // Append decryption keys and file metadata
                    attachment_rumor = attachment_rumor
                        .tag(Tag::custom(TagKind::custom("file-type"), [mime_type.as_str()]))
                        .tag(Tag::custom(TagKind::custom("size"), [file_size.to_string()]))
                        .tag(Tag::custom(TagKind::custom("encryption-algorithm"), ["aes-gcm"]))
                        .tag(Tag::custom(TagKind::custom("decryption-key"), [params.key.as_str()]))
                        .tag(Tag::custom(TagKind::custom("decryption-nonce"), [params.nonce.as_str()]))
                        .tag(Tag::custom(TagKind::custom("ox"), [file_hash.clone()]));

                    // Append image metadata if available
                    if let Some(ref img_meta) = attached_file.img_meta {
                        attachment_rumor = attachment_rumor
                            .tag(Tag::custom(TagKind::custom("blurhash"), [&img_meta.blurhash]))
                            .tag(Tag::custom(TagKind::custom("dim"), [format!("{}x{}", img_meta.width, img_meta.height)]));
                    }

                    // For WebXDC (.xdc) files, use the topic ID from the attachment (generated earlier)
                    if let Some(ref topic_encoded) = webxdc_topic {
                        attachment_rumor = attachment_rumor
                            .tag(Tag::custom(TagKind::custom("webxdc-topic"), [topic_encoded.clone()]));
                    }

                    attachment_rumor
                },
                Err(e) => {
                    // The file upload failed: so we mark the message as failed and notify of an error
                    mark_message_failed(Arc::clone(&pending_id), &receiver).await;
                    // Return the error
                    eprintln!("[Blossom Error] Upload failed: {}", e);
                    return Err(format!("Failed to upload file: {}", e));
                }
            }
        } else {
            // We already have a valid attachment_rumor from the reuse logic
            attachment_rumor
        };
        
        // Return the final attachment rumor as the main rumor
        final_attachment_rumor
    };

    // If a reply reference is included, add the tag
    if !msg.replied_to.is_empty() {
        rumor = rumor.tag(Tag::custom(
            TagKind::e(),
            [msg.replied_to, String::from(""), String::from("reply")],
        ));
    }

    // Get fresh timestamp with milliseconds right before giftwrapping
    let final_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let milliseconds = final_time.as_millis() % 1000;

    // Add millisecond precision tag for accurate message ordering
    rumor = rumor.tag(Tag::custom(
        TagKind::custom("ms"),
        [milliseconds.to_string()],
    ));

    // Build the rumor with our key (unsigned)
    let built_rumor = rumor.build(my_public_key);
    let rumor_id = built_rumor.id.unwrap();

    // Route to appropriate protocol handler
    if is_group_chat {
        // MLS Group Chat - send through MLS engine
        // Note: send_mls_message handles all state management internally:
        // - Uses the pending message we created above (via pending_id)
        // - Updates message ID when processed
        // - Marks as success/failure after network confirmation
        // - Saves to database
        match crate::mls::send_mls_message(&receiver, built_rumor.clone(), Some(pending_id.to_string())).await {
            Ok(_) => return Ok(true),
            Err(e) => {
                eprintln!("Failed to send MLS message: {:?}", e);
                return Ok(false);
            }
        }
    } else {
        // DM - use NIP-17 giftwrap
        // Send message to the real receiver with retry logic
        let mut send_attempts = 0;
        const MAX_ATTEMPTS: u32 = 12;
        const RETRY_DELAY: u64 = 5; // 5 seconds

        let mut final_output = None;

        while send_attempts < MAX_ATTEMPTS {
            send_attempts += 1;
            
            match client
                .gift_wrap(&receiver_pubkey, built_rumor.clone(), [])
                .await
            {
                Ok(output) => {
                    // Check if at least one relay acknowledged the message
                    if !output.success.is_empty() {
                        // Success! Message was acknowledged by at least one relay
                        // Extract wrapper_event_id BEFORE moving output
                        let wrapper_id = output.id().to_hex();
                        final_output = Some(output);
                        
                        // Immediately update frontend and save to DB
                        // This provides faster visual feedback without waiting for the self-send
                        {
                            let pending_id_for_early_update = Arc::clone(&pending_id);
                            let mut state = STATE.lock().await;
                            if let Some(msg) = state.chats.iter_mut()
                                .find(|chat| chat.id() == &receiver || chat.has_participant(&receiver))
                                .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == *pending_id_for_early_update))
                            {
                                // Update the message ID and clear pending state
                                msg.id = rumor_id.to_hex();
                                msg.pending = false;
                                msg.wrapper_event_id = Some(wrapper_id);
                                
                                // Emit update to frontend for immediate visual feedback
                                let handle = TAURI_APP.get().unwrap();
                                let _ = handle.emit("message_update", serde_json::json!({
                                    "old_id": pending_id_for_early_update.as_ref(),
                                    "message": &msg,
                                    "chat_id": &receiver
                                }));
                                
                                // Save to DB immediately (don't wait for self-send)
                                let message_to_save = msg.clone();
                                let chat_to_save = state.get_chat(&receiver).cloned();
                                drop(state); // Release lock before async DB operations
                                
                                if let Some(chat) = chat_to_save {
                                    let _ = save_chat(handle.clone(), &chat).await;
                                    let _ = crate::db::save_message(handle.clone(), &receiver, &message_to_save).await;
                                }
                            }
                        }
                        
                        break;
                    } else if output.failed.is_empty() {
                        // No success but also no failures - this might be a temporary network issue
                        // Continue retrying
                    } else {
                        // We have failures but no successes
                        if send_attempts == MAX_ATTEMPTS {
                            // Final attempt failed
                            mark_message_failed(Arc::clone(&pending_id), &receiver).await;
                            return Ok(false);
                        }
                    }
                    
                    // If we're here and haven't reached max attempts, wait before retrying
                    if send_attempts < MAX_ATTEMPTS {
                        tokio::time::sleep(tokio::time::Duration::from_secs(RETRY_DELAY)).await;
                    }
                }
                Err(e) => {
                    // Network or other error - log and retry if we haven't exceeded attempts
                    eprintln!("Failed to send message (attempt {}/{}): {:?}", send_attempts, MAX_ATTEMPTS, e);
                    
                    if send_attempts == MAX_ATTEMPTS {
                        // Final attempt failed
                        mark_message_failed(Arc::clone(&pending_id), &receiver).await;
                        return Ok(false);
                    }
                    
                    // Wait before retrying
                    tokio::time::sleep(tokio::time::Duration::from_secs(RETRY_DELAY)).await;
                }
            }
        }
        
        // If we get here without final_output, all attempts failed
        if final_output.is_none() {
            mark_message_failed(Arc::clone(&pending_id), &receiver).await;
            return Ok(false);
        }

        // Send message to our own public key, to allow for message recovering
        let _ = client
            .gift_wrap(&my_public_key, built_rumor, [])
            .await;

        Ok(true)
    }
}

#[tauri::command]
pub async fn paste_message<R: Runtime>(handle: AppHandle<R>, receiver: String, replied_to: String, transparent: bool) -> Result<bool, String> {
    // Platform-specific clipboard reading
    #[cfg(target_os = "android")]
    let img = {
        use crate::android::clipboard::read_image_from_clipboard;
        read_image_from_clipboard()?
    };

    #[cfg(not(target_os = "android"))]
    let img = {
        let tauri_img = handle.clipboard().read_image()
            .map_err(|e| format!("Failed to read clipboard: {:?}", e))?;

        // Get RGBA data - this returns &[u8], not a Result
        let rgba_data = tauri_img.rgba();

        // Convert to ImageBuffer
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(
            tauri_img.width(),
            tauri_img.height(),
            rgba_data.to_vec()
        ).ok_or_else(|| "Failed to create image buffer".to_string())?
    };

    // Get original pixels
    let original_pixels = img.as_raw();

    // Windows: check that every image has a non-zero-ish Alpha channel
    let mut _transparency_bug_search = false;
    #[cfg(target_os = "windows")]
    {
        _transparency_bug_search = original_pixels.iter().skip(3).step_by(4).all(|&a| a <= 2);
    }

    // For non-transparent images: manually account for the zero'ing out of the Alpha channel
    let pixels = if !transparent || _transparency_bug_search {
        // Only clone if we need to modify
        let mut modified = original_pixels.to_vec();
        modified.iter_mut().skip(3).step_by(4).for_each(|a| *a = 255);
        std::borrow::Cow::Owned(modified)
    } else {
        // No modification needed, use the original data
        std::borrow::Cow::Borrowed(original_pixels)
    };

    // Check if image has alpha transparency
    let has_alpha = crate::util::has_alpha_transparency(&pixels);
    
    let (encoded_bytes, extension) = if has_alpha {
        // Encode to PNG to preserve transparency with best compression
        let mut png_data = Vec::new();
        let encoder = ::image::codecs::png::PngEncoder::new_with_quality(
            &mut png_data,
            ::image::codecs::png::CompressionType::Best,
            ::image::codecs::png::FilterType::Adaptive,
        );
        encoder.write_image(
            &pixels,
            img.width(),
            img.height(),
            ::image::ExtendedColorType::Rgba8
        ).map_err(|e| e.to_string())?;
        (png_data, String::from("png"))
    } else {
        // Convert to JPEG for better compression (no alpha needed)
        let rgb_img = ::image::DynamicImage::ImageRgba8(
            ::image::RgbaImage::from_raw(img.width(), img.height(), pixels.to_vec())
                .ok_or_else(|| "Failed to create RGBA image".to_string())?
        ).to_rgb8();
        
        let mut jpeg_data = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut jpeg_data);
        let encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 85);
        encoder.write_image(
            rgb_img.as_raw(),
            img.width(),
            img.height(),
            ::image::ExtendedColorType::Rgb8
        ).map_err(|e| e.to_string())?;
        (jpeg_data, String::from("jpg"))
    };

    // Generate image metadata with Blurhash and dimensions
    let img_meta: Option<ImageMetadata> = util::generate_blurhash_from_rgba(
        img.as_raw(),
        img.width(),
        img.height()
    ).map(|blurhash| ImageMetadata {
        blurhash,
        width: img.width(),
        height: img.height(),
    });

    // Generate an Attachment File
    let attachment_file = AttachmentFile {
        bytes: encoded_bytes,
        img_meta,
        extension,
    };

    // Message the file to the intended user
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

#[tauri::command]
pub async fn voice_message(receiver: String, replied_to: String, bytes: Vec<u8>) -> Result<bool, String> {
    // Generate an Attachment File
    let attachment_file = AttachmentFile {
        bytes,
        img_meta: None,
        extension: String::from("wav")
    };

    // Message the file to the intended user
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

/// Cache for bytes received from JavaScript (for Android file handling)
static JS_FILE_CACHE: Lazy<std::sync::Mutex<Option<(Vec<u8>, String, String)>>> =
    Lazy::new(|| std::sync::Mutex::new(None));

/// Cache for compressed bytes from JavaScript file
static JS_COMPRESSION_CACHE: Lazy<TokioMutex<Option<CachedCompressedImage>>> =
    Lazy::new(|| TokioMutex::new(None));

/// Response from caching file bytes, includes preview for images
#[derive(serde::Serialize)]
pub struct CacheFileBytesResult {
    pub size: u64,
    pub name: String,
    pub extension: String,
    /// Base64 data URL for image preview (only for supported image types)
    pub preview: Option<String>,
}

/// Cache file bytes received from JavaScript (for Android)
/// This is called immediately when a file is selected via the WebView file input
/// Returns file info and a thumbnail preview for images
#[tauri::command]
pub fn cache_file_bytes(bytes: Vec<u8>, file_name: String, extension: String) -> Result<CacheFileBytesResult, String> {
    let size = bytes.len() as u64;
    
    // Generate preview for supported image types
    let preview = if matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico") {
        generate_image_preview_from_bytes(&bytes, 15).ok()
    } else {
        None
    };
    
    let mut cache = JS_FILE_CACHE.lock().unwrap();
    *cache = Some((bytes, file_name.clone(), extension.clone()));
    
    Ok(CacheFileBytesResult {
        size,
        name: file_name,
        extension,
        preview,
    })
}

/// Get cached file info (for preview display)
#[tauri::command]
pub fn get_cached_file_info() -> Result<Option<FileInfo>, String> {
    let cache = JS_FILE_CACHE.lock().unwrap();
    match &*cache {
        Some((bytes, name, ext)) => Ok(Some(FileInfo {
            size: bytes.len() as u64,
            name: name.clone(),
            extension: ext.clone(),
        })),
        None => Ok(None),
    }
}

/// Get base64 preview of cached image bytes
/// Uses ultra-fast nearest-neighbor downsampling for performance
#[tauri::command]
pub fn get_cached_image_preview(quality: u32) -> Result<String, String> {
    let quality = quality.clamp(1, 100);
    
    let cache = JS_FILE_CACHE.lock().unwrap();
    let (bytes, _, _) = cache.as_ref().ok_or("No cached file")?;
    let bytes = bytes.clone();
    drop(cache);
    
    let img = ::image::load_from_memory(&bytes)
        .map_err(|e| format!("Failed to decode image: {}", e))?;
    
    let (width, height) = (img.width(), img.height());
    let new_width = ((width * quality) / 100).max(1);
    let new_height = ((height * quality) / 100).max(1);
    
    // Convert to RGBA8 for fast nearest-neighbor downsampling
    let rgba = img.to_rgba8();
    let pixels = rgba.as_raw();
    
    // Use ultra-fast nearest-neighbor downsampling
    let resized_pixels = crate::util::nearest_neighbor_downsample(
        pixels,
        width,
        height,
        new_width,
        new_height,
    );
    
    // Check if image has alpha transparency
    let has_alpha = crate::util::has_alpha_transparency(&resized_pixels);
    
    use base64::Engine;
    
    if has_alpha {
        // Encode to PNG to preserve transparency with best compression
        let mut png_bytes = Vec::new();
        let encoder = ::image::codecs::png::PngEncoder::new_with_quality(
            &mut png_bytes,
            ::image::codecs::png::CompressionType::Best,
            ::image::codecs::png::FilterType::Adaptive,
        );
        encoder.write_image(
            &resized_pixels,
            new_width,
            new_height,
            ::image::ExtendedColorType::Rgba8,
        ).map_err(|e| format!("Failed to encode preview: {}", e))?;
        
        let base64_str = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
        Ok(format!("data:image/png;base64,{}", base64_str))
    } else {
        // Convert RGBA to RGB for JPEG encoding (no alpha needed)
        let rgb_pixels: Vec<u8> = resized_pixels
            .chunks_exact(4)
            .flat_map(|rgba| [rgba[0], rgba[1], rgba[2]])
            .collect();
        
        // Encode to JPEG
        let mut jpeg_bytes = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut jpeg_bytes);
        let encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 70);
        encoder.write_image(
            &rgb_pixels,
            new_width,
            new_height,
            ::image::ExtendedColorType::Rgb8,
        ).map_err(|e| format!("Failed to encode preview: {}", e))?;
        
        let base64_str = base64::engine::general_purpose::STANDARD.encode(&jpeg_bytes);
        Ok(format!("data:image/jpeg;base64,{}", base64_str))
    }
}

/// Start compression of cached bytes
#[tauri::command]
pub async fn start_cached_bytes_compression() -> Result<(), String> {
    // Get bytes from cache
    let (bytes, _, extension) = {
        let cache = JS_FILE_CACHE.lock().unwrap();
        cache.clone().ok_or("No cached file")?
    };
    
    // Clear any previous compression result
    {
        let mut comp_cache = JS_COMPRESSION_CACHE.lock().await;
        *comp_cache = None;
    }
    
    // Spawn compression task
    tokio::spawn(async move {
        let result = compress_bytes_internal(&bytes, &extension);
        let mut comp_cache = JS_COMPRESSION_CACHE.lock().await;
        *comp_cache = result.ok();
    });
    
    Ok(())
}

/// Get compression status for cached bytes
#[tauri::command]
pub async fn get_cached_bytes_compression_status() -> Result<Option<CompressionEstimate>, String> {
    let comp_cache = JS_COMPRESSION_CACHE.lock().await;
    
    match &*comp_cache {
        Some(cached) => {
            let savings_percent = if cached.original_size > 0 && cached.compressed_size < cached.original_size {
                ((cached.original_size - cached.compressed_size) * 100 / cached.original_size) as u32
            } else {
                0
            };
            
            Ok(Some(CompressionEstimate {
                original_size: cached.original_size,
                estimated_size: cached.compressed_size,
                savings_percent,
            }))
        }
        None => Ok(None),
    }
}

/// Send cached file (with optional compression)
#[tauri::command]
pub async fn send_cached_file(receiver: String, replied_to: String, use_compression: bool) -> Result<bool, String> {
    const MIN_SAVINGS_PERCENT: u64 = 10;
    
    if use_compression {
        // Check if compression is complete
        let comp_cache = JS_COMPRESSION_CACHE.lock().await;
        if let Some(compressed) = &*comp_cache {
            // Check if compression provides significant savings
            let savings_percent = if compressed.original_size > 0 && compressed.compressed_size < compressed.original_size {
                ((compressed.original_size - compressed.compressed_size) * 100) / compressed.original_size
            } else {
                0
            };
            
            if savings_percent >= MIN_SAVINGS_PERCENT {
                // Use compressed version
                let attachment_file = AttachmentFile {
                    bytes: compressed.bytes.clone(),
                    extension: compressed.extension.clone(),
                    img_meta: compressed.img_meta.clone(),
                };
                drop(comp_cache);
                
                // Clear caches
                *JS_FILE_CACHE.lock().unwrap() = None;
                *JS_COMPRESSION_CACHE.lock().await = None;
                
                return message(receiver, String::new(), replied_to, Some(attachment_file)).await;
            }
        }
        drop(comp_cache);
    }
    
    // Use original bytes - compress on-the-fly if use_compression is true
    let (original_bytes, _, original_extension) = {
        let mut cache = JS_FILE_CACHE.lock().unwrap();
        cache.take().ok_or("No cached file")?
    };
    
    // Clear compression cache
    *JS_COMPRESSION_CACHE.lock().await = None;
    
    // Process images: compress if use_compression is true, otherwise just generate metadata
    let (bytes, extension, img_meta) = if matches!(original_extension.as_str(), "png" | "jpg" | "jpeg" | "webp" | "tiff" | "tif" | "ico") {
        if let Ok(img) = ::image::load_from_memory(&original_bytes) {
            let rgba_img = img.to_rgba8();
            let blurhash_meta = crate::util::generate_blurhash_from_rgba(
                rgba_img.as_raw(),
                img.width(),
                img.height()
            ).map(|blurhash| ImageMetadata {
                blurhash,
                width: img.width(),
                height: img.height(),
            });
            
            if use_compression {
                // Compress on-the-fly since pre-compression wasn't ready
                let has_alpha = crate::util::has_alpha_transparency(rgba_img.as_raw());
                
                if has_alpha {
                    // Keep as PNG with best compression
                    let mut png_bytes = Vec::new();
                    let encoder = ::image::codecs::png::PngEncoder::new_with_quality(
                        &mut png_bytes,
                        ::image::codecs::png::CompressionType::Best,
                        ::image::codecs::png::FilterType::Adaptive,
                    );
                    if encoder.write_image(
                        rgba_img.as_raw(),
                        img.width(),
                        img.height(),
                        ::image::ExtendedColorType::Rgba8,
                    ).is_ok() {
                        (png_bytes, "png".to_string(), blurhash_meta)
                    } else {
                        (original_bytes, original_extension, blurhash_meta)
                    }
                } else {
                    // Convert to JPEG for better compression
                    let rgb_img = img.to_rgb8();
                    let mut jpeg_bytes = Vec::new();
                    let mut cursor = std::io::Cursor::new(&mut jpeg_bytes);
                    let encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 85);
                    if encoder.write_image(
                        rgb_img.as_raw(),
                        img.width(),
                        img.height(),
                        ::image::ExtendedColorType::Rgb8,
                    ).is_ok() {
                        (jpeg_bytes, "jpg".to_string(), blurhash_meta)
                    } else {
                        (original_bytes, original_extension, blurhash_meta)
                    }
                }
            } else {
                // No compression - just use original bytes with metadata
                (original_bytes, original_extension, blurhash_meta)
            }
        } else {
            (original_bytes, original_extension, None)
        }
    } else if original_extension == "gif" {
        // For GIFs, just generate metadata but keep original bytes
        let img_meta = if let Ok(img) = ::image::load_from_memory(&original_bytes) {
            let rgba_img = img.to_rgba8();
            crate::util::generate_blurhash_from_rgba(
                rgba_img.as_raw(),
                img.width(),
                img.height()
            ).map(|blurhash| ImageMetadata {
                blurhash,
                width: img.width(),
                height: img.height(),
            })
        } else {
            None
        };
        (original_bytes, original_extension, img_meta)
    } else {
        (original_bytes, original_extension, None)
    };
    
    let attachment_file = AttachmentFile {
        bytes,
        extension,
        img_meta,
    };
    
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

/// Clear cached file bytes
#[tauri::command]
pub async fn clear_cached_file() -> Result<(), String> {
    *JS_FILE_CACHE.lock().unwrap() = None;
    *JS_COMPRESSION_CACHE.lock().await = None;
    Ok(())
}

/// Clear Android file cache for a specific file path
/// This should be called when the user cancels file selection or after sending
#[tauri::command]
pub fn clear_android_file_cache(file_path: String) -> Result<(), String> {
    let mut cache = ANDROID_FILE_CACHE.lock().unwrap();
    cache.remove(&file_path);
    Ok(())
}

/// Clear all Android file cache entries
/// This is a cleanup function to ensure no stale data remains
#[tauri::command]
pub fn clear_all_android_file_cache() -> Result<(), String> {
    let mut cache = ANDROID_FILE_CACHE.lock().unwrap();
    cache.clear();
    Ok(())
}

/// Send file bytes directly from the frontend (used for Android optimized flow)
/// This receives the file bytes from JavaScript and sends them as an attachment
#[tauri::command]
pub async fn send_file_bytes(
    receiver: String,
    replied_to: String,
    file_bytes: Vec<u8>,
    file_name: String,
    use_compression: bool
) -> Result<bool, String> {
    const MIN_SAVINGS_PERCENT: u64 = 10;
    
    // Extract extension from filename
    let extension = file_name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase();
    
    let is_image = matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico");
    
    // Try compression if requested and it's an image (not GIF)
    if use_compression && is_image && extension != "gif" {
        match compress_bytes_internal(&file_bytes, &extension) {
            Ok(compressed) => {
                // Check if compression provides significant savings
                let savings_percent = if compressed.original_size > 0 && compressed.compressed_size < compressed.original_size {
                    ((compressed.original_size - compressed.compressed_size) * 100) / compressed.original_size
                } else {
                    0
                };
                
                if savings_percent >= MIN_SAVINGS_PERCENT {
                    // Use compressed version
                    let attachment_file = AttachmentFile {
                        bytes: compressed.bytes,
                        extension: compressed.extension,
                        img_meta: compressed.img_meta,
                    };
                    
                    return message(receiver, String::new(), replied_to, Some(attachment_file)).await;
                }
            }
            Err(e) => {
                // Log compression error but continue with original
                eprintln!("Compression failed, using original: {}", e);
            }
        }
    }
    
    // Use original bytes
    // Generate image metadata if applicable
    let img_meta = if is_image {
        if let Ok(img) = ::image::load_from_memory(&file_bytes) {
            let rgba_img = img.to_rgba8();
            crate::util::generate_blurhash_from_rgba(
                rgba_img.as_raw(),
                img.width(),
                img.height()
            ).map(|blurhash| ImageMetadata {
                blurhash,
                width: img.width(),
                height: img.height(),
            })
        } else {
            None
        }
    } else {
        None
    };
    
    let attachment_file = AttachmentFile {
        bytes: file_bytes,
        extension,
        img_meta,
    };
    
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

/// Internal function to compress bytes
fn compress_bytes_internal(bytes: &[u8], extension: &str) -> Result<CachedCompressedImage, String> {
    let original_size = bytes.len() as u64;
    
    // For GIFs, skip compression to preserve animation
    if extension == "gif" {
        let img = ::image::load_from_memory(bytes)
            .map_err(|e| format!("Failed to decode GIF: {}", e))?;
        
        let (width, height) = (img.width(), img.height());
        let rgba_img = img.to_rgba8();
        
        let img_meta = crate::util::generate_blurhash_from_rgba(
            rgba_img.as_raw(),
            width,
            height
        ).map(|blurhash| ImageMetadata {
            blurhash,
            width,
            height,
        });
        
        return Ok(CachedCompressedImage {
            bytes: bytes.to_vec(),
            extension: "gif".to_string(),
            img_meta,
            original_size,
            compressed_size: original_size,
        });
    }
    
    // Load and decode the image
    let img = ::image::load_from_memory(bytes)
        .map_err(|e| format!("Failed to decode image: {}", e))?;
    
    // Determine target dimensions (max 1920px on longest side)
    let (width, height) = (img.width(), img.height());
    let max_dimension = 1920u32;
    
    let (new_width, new_height) = if width > max_dimension || height > max_dimension {
        if width > height {
            let ratio = max_dimension as f32 / width as f32;
            (max_dimension, (height as f32 * ratio) as u32)
        } else {
            let ratio = max_dimension as f32 / height as f32;
            ((width as f32 * ratio) as u32, max_dimension)
        }
    } else {
        (width, height)
    };
    
    // Resize if needed
    let resized_img = if new_width != width || new_height != height {
        img.resize(new_width, new_height, ::image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    
    let rgba_img = resized_img.to_rgba8();
    let actual_width = rgba_img.width();
    let actual_height = rgba_img.height();
    
    // Check if image has alpha transparency
    let has_alpha = crate::util::has_alpha_transparency(rgba_img.as_raw());
    
    let mut compressed_bytes = Vec::new();
    let extension: String;
    
    if has_alpha {
        // Encode to PNG to preserve transparency with best compression
        let encoder = ::image::codecs::png::PngEncoder::new_with_quality(
            &mut compressed_bytes,
            ::image::codecs::png::CompressionType::Best,
            ::image::codecs::png::FilterType::Adaptive,
        );
        encoder.write_image(
            rgba_img.as_raw(),
            actual_width,
            actual_height,
            ::image::ExtendedColorType::Rgba8,
        ).map_err(|e| format!("Failed to encode PNG: {}", e))?;
        extension = "png".to_string();
    } else {
        // Convert to RGB for JPEG (no alpha needed)
        let mut cursor = std::io::Cursor::new(&mut compressed_bytes);
        let rgb_img = resized_img.to_rgb8();
        let mut encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 85);
        encoder.encode(
            rgb_img.as_raw(),
            actual_width,
            actual_height,
            ::image::ExtendedColorType::Rgb8.into()
        ).map_err(|e| format!("Failed to encode JPEG: {}", e))?;
        extension = "jpg".to_string();
    }
    
    let img_meta = crate::util::generate_blurhash_from_rgba(
        rgba_img.as_raw(),
        actual_width,
        actual_height
    ).map(|blurhash| ImageMetadata {
        blurhash,
        width: actual_width,
        height: actual_height,
    });
    
    let compressed_size = compressed_bytes.len() as u64;
    
    Ok(CachedCompressedImage {
        bytes: compressed_bytes,
        extension,
        img_meta,
        original_size,
        compressed_size,
    })
}

#[tauri::command]
pub async fn file_message(receiver: String, replied_to: String, file_path: String) -> Result<bool, String> {
    // Load the file as AttachmentFile
    let mut attachment_file = {
        #[cfg(not(target_os = "android"))]
        {
            let path = std::path::Path::new(&file_path);
            
            // Check if file exists first
            if !path.exists() {
                return Err(format!("File does not exist: {}", file_path));
            }
            
            // Read file bytes
            let bytes = std::fs::read(&file_path)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            
            // Check if file is empty
            if bytes.is_empty() {
                return Err(format!("File is empty (0 bytes): {}", file_path));
            }

            // Extract extension from filepath
            let extension = file_path
                .rsplit('.')
                .next()
                .unwrap_or("bin")
                .to_lowercase();

            AttachmentFile {
                bytes,
                img_meta: None,
                extension,
            }
        }
        #[cfg(target_os = "android")]
        {
            // First check if we have cached bytes for this URI
            let cache = ANDROID_FILE_CACHE.lock().unwrap();
            if let Some((cached_bytes, ext, _, _)) = cache.get(&file_path) {
                let bytes = cached_bytes.clone();
                let extension = ext.clone();
                drop(cache);
                
                // Clear the cache after use
                ANDROID_FILE_CACHE.lock().unwrap().remove(&file_path);
                
                AttachmentFile {
                    bytes,
                    img_meta: None,
                    extension,
                }
            } else {
                drop(cache);
                // Fall back to reading directly (may fail if permission expired)
                filesystem::read_android_uri(file_path)?
            }
        }
    };

    // Generate image metadata if the file is an image
    if matches!(attachment_file.extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico") {
        // Try to load and decode the image
        if let Ok(img) = ::image::load_from_memory(&attachment_file.bytes) {
            let rgba_img = img.to_rgba8();
            attachment_file.img_meta = util::generate_blurhash_from_rgba(
                rgba_img.as_raw(),
                img.width(),
                img.height()
            ).map(|blurhash| ImageMetadata {
                blurhash,
                width: img.width(),
                height: img.height(),
            });
        }
    }

    // Message the file to the intended user
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

/// File info structure for the frontend
#[derive(serde::Serialize)]
pub struct FileInfo {
    pub size: u64,
    pub name: String,
    pub extension: String,
}

/// Response from caching an Android file, includes preview for images
#[derive(serde::Serialize)]
pub struct AndroidFileCacheResult {
    pub size: u64,
    pub name: String,
    pub extension: String,
    /// Base64 data URL for image preview (only for supported image types)
    pub preview: Option<String>,
}

/// Cache an Android content URI's bytes immediately after file selection.
/// This must be called immediately after the file picker returns, before the permission expires.
/// On non-Android platforms, this just returns file info without caching.
/// For Android, also generates a compressed base64 preview for images.
#[tauri::command]
pub fn cache_android_file(file_path: String) -> Result<AndroidFileCacheResult, String> {
    #[cfg(not(target_os = "android"))]
    {
        // On non-Android platforms, just return file info without caching
        let path = std::path::Path::new(&file_path);
        
        if !path.exists() {
            return Err(format!("File does not exist: {}", file_path));
        }
        
        let metadata = std::fs::metadata(&file_path)
            .map_err(|e| format!("Failed to get file metadata: {}", e))?;
        
        let name = path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        
        let extension = path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        
        Ok(AndroidFileCacheResult {
            size: metadata.len(),
            name,
            extension,
            preview: None, // Desktop doesn't need preview from this function
        })
    }
    #[cfg(target_os = "android")]
    {
        // Read the file using the same method as avatar upload (read_android_uri)
        // This uses getType() instead of query() which may have different permission behavior
        let attachment = filesystem::read_android_uri(file_path.clone())?;
        let bytes = attachment.bytes;
        let extension = attachment.extension.clone();
        let size = bytes.len() as u64;
        
        // For Android content URIs, we can't easily get the display name without query()
        // which may fail due to permissions. Use a generic name with the extension.
        let name = format!("file.{}", extension);
        
        // Generate preview for supported image types
        let preview = if matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico") {
            generate_image_preview_from_bytes(&bytes, 15).ok()
        } else {
            None
        };
        
        // Cache the bytes
        let mut cache = ANDROID_FILE_CACHE.lock().unwrap();
        cache.insert(file_path, (bytes, extension.clone(), name.clone(), size));
        
        Ok(AndroidFileCacheResult {
            size,
            name,
            extension,
            preview,
        })
    }
}

/// Generate a compressed base64 preview from image bytes
/// Quality is a percentage (1-100) that determines the preview size
/// Uses ultra-fast nearest-neighbor downsampling for performance
/// For files smaller than 5MB or GIFs, returns the original image as base64 (no resizing)
fn generate_image_preview_from_bytes(bytes: &[u8], quality: u32) -> Result<String, String> {
    use base64::Engine;
    
    const SKIP_RESIZE_THRESHOLD: usize = 5 * 1024 * 1024; // 5MB
    
    // Detect if this is a GIF (we never resize GIFs to preserve animation)
    let is_gif = bytes.starts_with(b"GIF");
    
    // For small files or GIFs, just return the original as base64 (skip resizing)
    if bytes.len() < SKIP_RESIZE_THRESHOLD || is_gif {
        let base64_str = base64::engine::general_purpose::STANDARD.encode(bytes);
        
        // Detect image type from magic bytes for correct MIME type
        let mime_type = if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            "image/jpeg"
        } else if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
            "image/png"
        } else if is_gif {
            "image/gif"
        } else if bytes.starts_with(b"RIFF") && bytes.len() > 12 && &bytes[8..12] == b"WEBP" {
            "image/webp"
        } else if bytes.len() >= 4 && ((bytes[0..2] == [0x49, 0x49] && bytes[2..4] == [0x2A, 0x00]) ||
                                        (bytes[0..2] == [0x4D, 0x4D] && bytes[2..4] == [0x00, 0x2A])) {
            // TIFF: II (little-endian) or MM (big-endian) followed by 42
            "image/tiff"
        } else if bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
            // ICO format
            "image/x-icon"
        } else {
            "image/jpeg" // Default fallback
        };
        
        return Ok(format!("data:{};base64,{}", mime_type, base64_str));
    }
    
    let quality = quality.clamp(1, 100);
    
    let img = ::image::load_from_memory(bytes)
        .map_err(|e| format!("Failed to decode image: {}", e))?;
    
    let (width, height) = (img.width(), img.height());
    let new_width = ((width * quality) / 100).max(1);
    let new_height = ((height * quality) / 100).max(1);
    
    // Convert to RGBA8 for fast nearest-neighbor downsampling
    let rgba = img.to_rgba8();
    let pixels = rgba.as_raw();
    
    // Use ultra-fast nearest-neighbor downsampling
    let resized_pixels = crate::util::nearest_neighbor_downsample(
        pixels,
        width,
        height,
        new_width,
        new_height,
    );
    
    // Check if image has alpha transparency
    let has_alpha = crate::util::has_alpha_transparency(&resized_pixels);
    
    if has_alpha {
        // Encode to PNG to preserve transparency with best compression
        let mut png_bytes = Vec::new();
        let encoder = ::image::codecs::png::PngEncoder::new_with_quality(
            &mut png_bytes,
            ::image::codecs::png::CompressionType::Best,
            ::image::codecs::png::FilterType::Adaptive,
        );
        encoder.write_image(
            &resized_pixels,
            new_width,
            new_height,
            ::image::ExtendedColorType::Rgba8,
        ).map_err(|e| format!("Failed to encode preview: {}", e))?;
        
        let base64_str = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
        Ok(format!("data:image/png;base64,{}", base64_str))
    } else {
        // Convert RGBA to RGB for JPEG encoding (no alpha needed)
        let rgb_pixels: Vec<u8> = resized_pixels
            .chunks_exact(4)
            .flat_map(|rgba| [rgba[0], rgba[1], rgba[2]])
            .collect();
        
        // Encode to JPEG
        let mut jpeg_bytes = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut jpeg_bytes);
        let encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 70);
        encoder.write_image(
            &rgb_pixels,
            new_width,
            new_height,
            ::image::ExtendedColorType::Rgb8,
        ).map_err(|e| format!("Failed to encode preview: {}", e))?;
        
        let base64_str = base64::engine::general_purpose::STANDARD.encode(&jpeg_bytes);
        Ok(format!("data:image/jpeg;base64,{}", base64_str))
    }
}

/// Get file information (size, name, extension)
#[tauri::command]
pub fn get_file_info(file_path: String) -> Result<FileInfo, String> {
    #[cfg(not(target_os = "android"))]
    {
        let path = std::path::Path::new(&file_path);
        
        if !path.exists() {
            return Err(format!("File does not exist: {}", file_path));
        }
        
        let metadata = std::fs::metadata(&file_path)
            .map_err(|e| format!("Failed to get file metadata: {}", e))?;
        
        let name = path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        
        let extension = path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        
        Ok(FileInfo {
            size: metadata.len(),
            name,
            extension,
        })
    }
    #[cfg(target_os = "android")]
    {
        // First check if we have cached bytes for this URI
        let cache = ANDROID_FILE_CACHE.lock().unwrap();
        if let Some((bytes, extension, name, _)) = cache.get(&file_path) {
            return Ok(FileInfo {
                size: bytes.len() as u64,
                name: name.clone(),
                extension: extension.clone(),
            });
        }
        drop(cache);
        
        // Fall back to querying the URI directly (may fail if permission expired)
        filesystem::get_android_uri_info(file_path)
    }
}

/// Get a base64 preview of an image (for Android where convertFileSrc doesn't work)
/// The quality parameter (1-100) determines the resize percentage
/// Uses ultra-fast nearest-neighbor downsampling for performance
#[tauri::command]
pub fn get_image_preview_base64(file_path: String, quality: u32) -> Result<String, String> {
    let quality = quality.clamp(1, 100);
    
    #[cfg(not(target_os = "android"))]
    {
        let bytes = std::fs::read(&file_path)
            .map_err(|e| format!("Failed to read file: {}", e))?;
        
        let img = ::image::load_from_memory(&bytes)
            .map_err(|e| format!("Failed to decode image: {}", e))?;
        
        let (width, height) = (img.width(), img.height());
        let new_width = ((width * quality) / 100).max(1);
        let new_height = ((height * quality) / 100).max(1);
        
        // Convert to RGBA8 for fast nearest-neighbor downsampling
        let rgba = img.to_rgba8();
        let pixels = rgba.as_raw();
        
        // Use ultra-fast nearest-neighbor downsampling
        let resized_pixels = crate::util::nearest_neighbor_downsample(
            pixels,
            width,
            height,
            new_width,
            new_height,
        );
        
        // Check if image has alpha transparency
        let has_alpha = crate::util::has_alpha_transparency(&resized_pixels);
        
        use base64::Engine;
        
        if has_alpha {
            // Encode to PNG to preserve transparency with best compression
            let mut png_bytes = Vec::new();
            let encoder = ::image::codecs::png::PngEncoder::new_with_quality(
                &mut png_bytes,
                ::image::codecs::png::CompressionType::Best,
                ::image::codecs::png::FilterType::Adaptive,
            );
            encoder.write_image(
                &resized_pixels,
                new_width,
                new_height,
                ::image::ExtendedColorType::Rgba8,
            ).map_err(|e| format!("Failed to encode preview: {}", e))?;
            
            let base64_str = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
            Ok(format!("data:image/png;base64,{}", base64_str))
        } else {
            // Convert RGBA to RGB for JPEG encoding (no alpha needed)
            let rgb_pixels: Vec<u8> = resized_pixels
                .chunks_exact(4)
                .flat_map(|rgba| [rgba[0], rgba[1], rgba[2]])
                .collect();
            
            // Encode to JPEG
            let mut jpeg_bytes = Vec::new();
            let mut cursor = std::io::Cursor::new(&mut jpeg_bytes);
            let encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 70);
            encoder.write_image(
                &rgb_pixels,
                new_width,
                new_height,
                ::image::ExtendedColorType::Rgb8,
            ).map_err(|e| format!("Failed to encode preview: {}", e))?;
            
            let base64_str = base64::engine::general_purpose::STANDARD.encode(&jpeg_bytes);
            Ok(format!("data:image/jpeg;base64,{}", base64_str))
        }
    }
    
    #[cfg(target_os = "android")]
    {
        // First check if we have cached bytes for this URI
        let bytes = {
            let cache = ANDROID_FILE_CACHE.lock().unwrap();
            if let Some((cached_bytes, _, _, _)) = cache.get(&file_path) {
                cached_bytes.clone()
            } else {
                drop(cache);
                // Fall back to reading directly (may fail if permission expired)
                filesystem::read_android_uri_bytes(file_path)?.0
            }
        };
        
        let img = ::image::load_from_memory(&bytes)
            .map_err(|e| format!("Failed to decode image: {}", e))?;
        
        let (width, height) = (img.width(), img.height());
        let new_width = ((width * quality) / 100).max(1);
        let new_height = ((height * quality) / 100).max(1);
        
        // Convert to RGBA8 for fast nearest-neighbor downsampling
        let rgba = img.to_rgba8();
        let pixels = rgba.as_raw();
        
        // Use ultra-fast nearest-neighbor downsampling
        let resized_pixels = crate::util::nearest_neighbor_downsample(
            pixels,
            width,
            height,
            new_width,
            new_height,
        );
        
        // Check if image has alpha transparency
        let has_alpha = crate::util::has_alpha_transparency(&resized_pixels);
        
        use base64::Engine;
        
        if has_alpha {
            // Encode to PNG to preserve transparency with best compression
            let mut png_bytes = Vec::new();
            let encoder = ::image::codecs::png::PngEncoder::new_with_quality(
                &mut png_bytes,
                ::image::codecs::png::CompressionType::Best,
                ::image::codecs::png::FilterType::Adaptive,
            );
            encoder.write_image(
                &resized_pixels,
                new_width,
                new_height,
                ::image::ExtendedColorType::Rgba8,
            ).map_err(|e| format!("Failed to encode preview: {}", e))?;
            
            let base64_str = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
            Ok(format!("data:image/png;base64,{}", base64_str))
        } else {
            // Convert RGBA to RGB for JPEG encoding (no alpha needed)
            let rgb_pixels: Vec<u8> = resized_pixels
                .chunks_exact(4)
                .flat_map(|rgba| [rgba[0], rgba[1], rgba[2]])
                .collect();
            
            // Encode to JPEG
            let mut jpeg_bytes = Vec::new();
            let mut cursor = std::io::Cursor::new(&mut jpeg_bytes);
            let encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 70);
            encoder.write_image(
                &rgb_pixels,
                new_width,
                new_height,
                ::image::ExtendedColorType::Rgb8,
            ).map_err(|e| format!("Failed to encode preview: {}", e))?;
            
            let base64_str = base64::engine::general_purpose::STANDARD.encode(&jpeg_bytes);
            Ok(format!("data:image/jpeg;base64,{}", base64_str))
        }
    }
}

/// Send a file with compression (for images)
#[tauri::command]
pub async fn file_message_compressed(receiver: String, replied_to: String, file_path: String) -> Result<bool, String> {
    // Load the file as AttachmentFile
    let mut attachment_file = {
        #[cfg(not(target_os = "android"))]
        {
            let path = std::path::Path::new(&file_path);
            
            // Check if file exists first
            if !path.exists() {
                return Err(format!("File does not exist: {}", file_path));
            }
            
            // Read file bytes
            let bytes = std::fs::read(&file_path)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            
            // Check if file is empty
            if bytes.is_empty() {
                return Err(format!("File is empty (0 bytes): {}", file_path));
            }

            // Extract extension from filepath
            let extension = file_path
                .rsplit('.')
                .next()
                .unwrap_or("bin")
                .to_lowercase();

            AttachmentFile {
                bytes,
                img_meta: None,
                extension,
            }
        }
        #[cfg(target_os = "android")]
        {
            // First check if we have cached bytes for this URI
            let cache = ANDROID_FILE_CACHE.lock().unwrap();
            if let Some((cached_bytes, ext, _, _)) = cache.get(&file_path) {
                let bytes = cached_bytes.clone();
                let extension = ext.clone();
                drop(cache);
                
                // Clear the cache after use
                ANDROID_FILE_CACHE.lock().unwrap().remove(&file_path);
                
                AttachmentFile {
                    bytes,
                    img_meta: None,
                    extension,
                }
            } else {
                drop(cache);
                // Fall back to reading directly (may fail if permission expired)
                filesystem::read_android_uri(file_path)?
            }
        }
    };

    // Compress the image if it's a supported format
    if matches!(attachment_file.extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "tiff" | "tif" | "ico") {
        if let Ok(img) = ::image::load_from_memory(&attachment_file.bytes) {
            // Determine target dimensions (max 1920px on longest side)
            let (width, height) = (img.width(), img.height());
            let max_dimension = 1920u32;
            
            let (new_width, new_height) = if width > max_dimension || height > max_dimension {
                if width > height {
                    let ratio = max_dimension as f32 / width as f32;
                    (max_dimension, (height as f32 * ratio) as u32)
                } else {
                    let ratio = max_dimension as f32 / height as f32;
                    ((width as f32 * ratio) as u32, max_dimension)
                }
            } else {
                (width, height)
            };
            
            // Resize if needed
            let resized_img = if new_width != width || new_height != height {
                img.resize(new_width, new_height, ::image::imageops::FilterType::Lanczos3)
            } else {
                img
            };
            
            // Get RGBA image for alpha check and blurhash
            let rgba_img = resized_img.to_rgba8();
            let actual_width = rgba_img.width();
            let actual_height = rgba_img.height();
            
            // Check if image has alpha transparency
            let has_alpha = crate::util::has_alpha_transparency(rgba_img.as_raw());
            
            let mut compressed_bytes = Vec::new();
            
            // Use JPEG for lossy compression (except for GIFs which should stay as GIF, and images with alpha)
            if attachment_file.extension == "gif" {
                // For GIFs, just resize but keep format
                let mut cursor = std::io::Cursor::new(&mut compressed_bytes);
                let mut encoder = ::image::codecs::gif::GifEncoder::new(&mut cursor);
                encoder.encode(
                    rgba_img.as_raw(),
                    actual_width,
                    actual_height,
                    ::image::ExtendedColorType::Rgba8.into()
                ).map_err(|e| format!("Failed to encode GIF: {}", e))?;
            } else if has_alpha {
                // Encode to PNG to preserve transparency with best compression
                let encoder = ::image::codecs::png::PngEncoder::new_with_quality(
                    &mut compressed_bytes,
                    ::image::codecs::png::CompressionType::Best,
                    ::image::codecs::png::FilterType::Adaptive,
                );
                encoder.write_image(
                    rgba_img.as_raw(),
                    actual_width,
                    actual_height,
                    ::image::ExtendedColorType::Rgba8,
                ).map_err(|e| format!("Failed to encode PNG: {}", e))?;
                
                // Update extension to png since we're preserving alpha
                attachment_file.extension = "png".to_string();
            } else {
                // Convert to RGB for JPEG (no alpha needed)
                let mut cursor = std::io::Cursor::new(&mut compressed_bytes);
                let rgb_img = resized_img.to_rgb8();
                let mut encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 85);
                encoder.encode(
                    rgb_img.as_raw(),
                    actual_width,
                    actual_height,
                    ::image::ExtendedColorType::Rgb8.into()
                ).map_err(|e| format!("Failed to encode JPEG: {}", e))?;
                
                // Update extension to jpg since we converted
                attachment_file.extension = "jpg".to_string();
            }
            
            attachment_file.bytes = compressed_bytes;
            
            // Generate blurhash from the resized image
            attachment_file.img_meta = crate::util::generate_blurhash_from_rgba(
                rgba_img.as_raw(),
                actual_width,
                actual_height
            ).map(|blurhash| ImageMetadata {
                blurhash,
                width: actual_width,
                height: actual_height,
            });
        }
    }

    // Message the file to the intended user
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

/// Compression estimate result
#[derive(serde::Serialize, Clone)]
pub struct CompressionEstimate {
    pub original_size: u64,
    pub estimated_size: u64,
    pub savings_percent: u32,
}

/// Start pre-compressing an image and cache the result
/// This is called when the file preview opens
#[tauri::command]
pub async fn start_image_precompression(file_path: String) -> Result<(), String> {
    // Mark as "in progress" by inserting None
    {
        let mut cache = COMPRESSION_CACHE.lock().await;
        cache.insert(file_path.clone(), None);
    }
    
    // Spawn the compression task
    let path_clone = file_path.clone();
    tokio::spawn(async move {
        let result = compress_image_internal(&path_clone);
        let mut cache = COMPRESSION_CACHE.lock().await;
        
        // Only store if still in cache (not cancelled)
        if cache.contains_key(&path_clone) {
            cache.insert(path_clone, result.ok());
        }
    });
    
    Ok(())
}

/// Get the compression status/result for a file
#[tauri::command]
pub async fn get_compression_status(file_path: String) -> Result<Option<CompressionEstimate>, String> {
    let cache = COMPRESSION_CACHE.lock().await;
    
    match cache.get(&file_path) {
        Some(Some(cached)) => {
            // Compression complete
            let savings_percent = if cached.original_size > 0 && cached.compressed_size < cached.original_size {
                ((cached.original_size - cached.compressed_size) * 100 / cached.original_size) as u32
            } else {
                0
            };
            
            Ok(Some(CompressionEstimate {
                original_size: cached.original_size,
                estimated_size: cached.compressed_size,
                savings_percent,
            }))
        }
        Some(None) => {
            // Still compressing
            Ok(None)
        }
        None => {
            // Not in cache
            Err("File not in compression cache".to_string())
        }
    }
}

/// Clear the compression cache for a file (called on cancel)
#[tauri::command]
pub async fn clear_compression_cache(file_path: String) -> Result<(), String> {
    // Clear compression cache
    let mut cache = COMPRESSION_CACHE.lock().await;
    cache.remove(&file_path);
    drop(cache);
    
    // Also clear Android file cache
    let mut android_cache = ANDROID_FILE_CACHE.lock().unwrap();
    android_cache.remove(&file_path);
    
    Ok(())
}

/// Send a file using the cached compressed version if available
#[tauri::command]
pub async fn send_cached_compressed_file(receiver: String, replied_to: String, file_path: String) -> Result<bool, String> {
    // Minimum savings threshold (10%) - if compression doesn't save at least this much, send original
    const MIN_SAVINGS_PERCENT: u64 = 10;
    
    // First check if compression is complete or still in progress
    let status = {
        let cache = COMPRESSION_CACHE.lock().await;
        cache.get(&file_path).cloned()
    };
    
    match status {
        Some(Some(compressed)) => {
            // Compression complete - remove from cache
            {
                let mut cache = COMPRESSION_CACHE.lock().await;
                cache.remove(&file_path);
            }
            
            // Check if compression provides significant savings
            let savings_percent = if compressed.original_size > 0 && compressed.compressed_size < compressed.original_size {
                ((compressed.original_size - compressed.compressed_size) * 100) / compressed.original_size
            } else {
                0 // No savings or compression made it bigger
            };
            
            if savings_percent >= MIN_SAVINGS_PERCENT {
                // Compression provides significant savings - send compressed
                let attachment_file = AttachmentFile {
                    bytes: compressed.bytes,
                    extension: compressed.extension,
                    img_meta: compressed.img_meta,
                };
                message(receiver, String::new(), replied_to, Some(attachment_file)).await
            } else {
                // No significant savings - send original file
                file_message(receiver, replied_to, file_path).await
            }
        }
        Some(None) => {
            // Still compressing - wait for it
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                let cache = COMPRESSION_CACHE.lock().await;
                match cache.get(&file_path) {
                    Some(Some(_)) => break,
                    Some(None) => continue,
                    None => {
                        // Cache was cleared - fall back to compressing now
                        drop(cache);
                        return file_message_compressed(receiver, replied_to, file_path).await;
                    }
                }
            }
            
            // Now get the result
            let cached = {
                let mut cache = COMPRESSION_CACHE.lock().await;
                cache.remove(&file_path)
            };
            
            if let Some(Some(compressed)) = cached {
                // Check if compression provides significant savings
                let savings_percent = if compressed.original_size > 0 && compressed.compressed_size < compressed.original_size {
                    ((compressed.original_size - compressed.compressed_size) * 100) / compressed.original_size
                } else {
                    0 // No savings or compression made it bigger
                };
                
                if savings_percent >= MIN_SAVINGS_PERCENT {
                    // Compression provides significant savings - send compressed
                    let attachment_file = AttachmentFile {
                        bytes: compressed.bytes,
                        extension: compressed.extension,
                        img_meta: compressed.img_meta,
                    };
                    message(receiver, String::new(), replied_to, Some(attachment_file)).await
                } else {
                    // No significant savings - send original file
                    file_message(receiver, replied_to, file_path).await
                }
            } else {
                Err("Failed to get compressed image".to_string())
            }
        }
        None => {
            // Not in cache, compress now
            file_message_compressed(receiver, replied_to, file_path).await
        }
    }
}

/// Internal function to compress an image and return cached data
fn compress_image_internal(file_path: &str) -> Result<CachedCompressedImage, String> {
    #[cfg(not(target_os = "android"))]
    {
        let path = std::path::Path::new(file_path);
        
        if !path.exists() {
            return Err(format!("File does not exist: {}", file_path));
        }
        
        // Get extension early to check if it's a GIF
        let extension = file_path
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_lowercase();
        
        // Read file bytes
        let bytes = std::fs::read(file_path)
            .map_err(|e| format!("Failed to read file: {}", e))?;
        
        let original_size = bytes.len() as u64;
        
        // For GIFs, skip compression entirely to preserve animation
        // Just decode first frame for blurhash, then return original bytes
        if extension == "gif" {
            // Decode just to get dimensions and generate blurhash from first frame
            let img = ::image::load_from_memory(&bytes)
                .map_err(|e| format!("Failed to decode GIF: {}", e))?;
            
            let (width, height) = (img.width(), img.height());
            let rgba_img = img.to_rgba8();
            
            let img_meta = crate::util::generate_blurhash_from_rgba(
                rgba_img.as_raw(),
                width,
                height
            ).map(|blurhash| ImageMetadata {
                blurhash,
                width,
                height,
            });
            
            // Return original bytes unchanged to preserve animation
            return Ok(CachedCompressedImage {
                bytes,
                extension: "gif".to_string(),
                img_meta,
                original_size,
                compressed_size: original_size, // Same size, no compression
            });
        }
        
        // Try to load and decode the image
        let img = ::image::load_from_memory(&bytes)
            .map_err(|e| format!("Failed to decode image: {}", e))?;
        
        // Determine target dimensions (max 1920px on longest side)
        let (width, height) = (img.width(), img.height());
        let max_dimension = 1920u32;
        
        let (new_width, new_height) = if width > max_dimension || height > max_dimension {
            if width > height {
                let ratio = max_dimension as f32 / width as f32;
                (max_dimension, (height as f32 * ratio) as u32)
            } else {
                let ratio = max_dimension as f32 / height as f32;
                ((width as f32 * ratio) as u32, max_dimension)
            }
        } else {
            (width, height)
        };
        
        // Resize if needed
        let resized_img = if new_width != width || new_height != height {
            img.resize(new_width, new_height, ::image::imageops::FilterType::Lanczos3)
        } else {
            img
        };
        
        let rgba_img = resized_img.to_rgba8();
        let actual_width = rgba_img.width();
        let actual_height = rgba_img.height();
        
        // Check if image has alpha transparency
        let has_alpha = crate::util::has_alpha_transparency(rgba_img.as_raw());
        
        let mut compressed_bytes = Vec::new();
        let extension: String;
        
        if has_alpha {
            // Encode to PNG to preserve transparency with best compression
            let encoder = ::image::codecs::png::PngEncoder::new_with_quality(
                &mut compressed_bytes,
                ::image::codecs::png::CompressionType::Best,
                ::image::codecs::png::FilterType::Adaptive,
            );
            encoder.write_image(
                rgba_img.as_raw(),
                actual_width,
                actual_height,
                ::image::ExtendedColorType::Rgba8,
            ).map_err(|e| format!("Failed to encode PNG: {}", e))?;
            extension = "png".to_string();
        } else {
            // Convert to RGB for JPEG (no alpha needed)
            let mut cursor = std::io::Cursor::new(&mut compressed_bytes);
            let rgb_img = resized_img.to_rgb8();
            let mut encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 85);
            encoder.encode(
                rgb_img.as_raw(),
                actual_width,
                actual_height,
                ::image::ExtendedColorType::Rgb8.into()
            ).map_err(|e| format!("Failed to encode JPEG: {}", e))?;
            extension = "jpg".to_string();
        }
        
        let img_meta = crate::util::generate_blurhash_from_rgba(
            rgba_img.as_raw(),
            actual_width,
            actual_height
        ).map(|blurhash| ImageMetadata {
            blurhash,
            width: actual_width,
            height: actual_height,
        });
        
        let compressed_size = compressed_bytes.len() as u64;
        
        Ok(CachedCompressedImage {
            bytes: compressed_bytes,
            extension,
            img_meta,
            original_size,
            compressed_size,
        })
    }
    #[cfg(target_os = "android")]
    {
        // First check if we have cached bytes for this URI
        let (bytes, extension) = {
            let cache = ANDROID_FILE_CACHE.lock().unwrap();
            if let Some((cached_bytes, ext, _, _)) = cache.get(file_path) {
                (cached_bytes.clone(), ext.clone())
            } else {
                drop(cache);
                // Fall back to reading directly (may fail if permission expired)
                filesystem::read_android_uri_bytes(file_path.to_string())?
            }
        };
        let original_size = bytes.len() as u64;
        
        // For GIFs, skip compression entirely to preserve animation
        if extension == "gif" {
            let img = ::image::load_from_memory(&bytes)
                .map_err(|e| format!("Failed to decode GIF: {}", e))?;
            
            let (width, height) = (img.width(), img.height());
            let rgba_img = img.to_rgba8();
            
            let img_meta = crate::util::generate_blurhash_from_rgba(
                rgba_img.as_raw(),
                width,
                height
            ).map(|blurhash| ImageMetadata {
                blurhash,
                width,
                height,
            });
            
            return Ok(CachedCompressedImage {
                bytes,
                extension: "gif".to_string(),
                img_meta,
                original_size,
                compressed_size: original_size,
            });
        }
        
        // Try to load and decode the image
        let img = ::image::load_from_memory(&bytes)
            .map_err(|e| format!("Failed to decode image: {}", e))?;
        
        // Determine target dimensions (max 1920px on longest side)
        let (width, height) = (img.width(), img.height());
        let max_dimension = 1920u32;
        
        let (new_width, new_height) = if width > max_dimension || height > max_dimension {
            if width > height {
                let ratio = max_dimension as f32 / width as f32;
                (max_dimension, (height as f32 * ratio) as u32)
            } else {
                let ratio = max_dimension as f32 / height as f32;
                ((width as f32 * ratio) as u32, max_dimension)
            }
        } else {
            (width, height)
        };
        
        // Resize if needed
        let resized_img = if new_width != width || new_height != height {
            img.resize(new_width, new_height, ::image::imageops::FilterType::Lanczos3)
        } else {
            img
        };
        
        let rgba_img = resized_img.to_rgba8();
        let actual_width = rgba_img.width();
        let actual_height = rgba_img.height();
        
        // Check if image has alpha transparency
        let has_alpha = crate::util::has_alpha_transparency(rgba_img.as_raw());
        
        let mut compressed_bytes = Vec::new();
        let extension: String;
        
        if has_alpha {
            // Encode to PNG to preserve transparency with best compression
            let encoder = ::image::codecs::png::PngEncoder::new_with_quality(
                &mut compressed_bytes,
                ::image::codecs::png::CompressionType::Best,
                ::image::codecs::png::FilterType::Adaptive,
            );
            encoder.write_image(
                rgba_img.as_raw(),
                actual_width,
                actual_height,
                ::image::ExtendedColorType::Rgba8,
            ).map_err(|e| format!("Failed to encode PNG: {}", e))?;
            extension = "png".to_string();
        } else {
            // Convert to RGB for JPEG (no alpha needed)
            let mut cursor = std::io::Cursor::new(&mut compressed_bytes);
            let rgb_img = resized_img.to_rgb8();
            let mut encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, 85);
            encoder.encode(
                rgb_img.as_raw(),
                actual_width,
                actual_height,
                ::image::ExtendedColorType::Rgb8.into()
            ).map_err(|e| format!("Failed to encode JPEG: {}", e))?;
            extension = "jpg".to_string();
        }
        
        let img_meta = crate::util::generate_blurhash_from_rgba(
            rgba_img.as_raw(),
            actual_width,
            actual_height
        ).map(|blurhash| ImageMetadata {
            blurhash,
            width: actual_width,
            height: actual_height,
        });
        
        let compressed_size = compressed_bytes.len() as u64;
        
        Ok(CachedCompressedImage {
            bytes: compressed_bytes,
            extension,
            img_meta,
            original_size,
            compressed_size,
        })
    }
}

/// Protocol-agnostic reaction function that works for both DMs and Group Chats
#[tauri::command]
pub async fn react_to_message(reference_id: String, chat_id: String, emoji: String) -> Result<bool, String> {
    use crate::chat::ChatType;
    
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await.map_err(|e| e.to_string())?;
    let my_public_key = signer.get_public_key().await.map_err(|e| e.to_string())?;
    
    // Determine chat type
    let state = STATE.lock().await;
    let chat = state.chats.iter().find(|c| c.id == chat_id)
        .ok_or_else(|| "Chat not found".to_string())?;
    let chat_type = chat.chat_type.clone();
    drop(state);
    
    match chat_type {
        ChatType::DirectMessage => {
            // For DMs, send gift-wrapped reaction
            let reference_event = EventId::from_hex(&reference_id).map_err(|e| e.to_string())?;
            let receiver_pubkey = PublicKey::from_bech32(&chat_id).map_err(|e| e.to_string())?;
            
            // Build NIP-25 Reaction rumor
            let rumor = EventBuilder::reaction_extended(
                reference_event,
                receiver_pubkey,
                Some(Kind::PrivateDirectMessage),
                &emoji,
            )
            .build(my_public_key);
            let rumor_id = rumor.id.ok_or("Failed to get rumor ID")?.to_hex();
            
            // Send reaction to the receiver
            client
                .gift_wrap(&receiver_pubkey, rumor.clone(), [])
                .await
                .map_err(|e| e.to_string())?;
            
            // Send reaction to ourselves for recovery
            client
                .gift_wrap(&my_public_key, rumor, [])
                .await
                .map_err(|e| e.to_string())?;
            
            // Add reaction to local state
            let reaction = Reaction {
                id: rumor_id,
                reference_id: reference_id.clone(),
                author_id: my_public_key.to_hex(),
                emoji,
            };
            
            let mut state = STATE.lock().await;
            if let Some(chat) = state.chats.iter_mut().find(|c| c.has_participant(&chat_id)) {
                if let Some(msg) = chat.messages.iter_mut().find(|m| m.id == reference_id) {
                    let was_added = msg.add_reaction(reaction, Some(&chat_id));
                    
                    if was_added {
                        // Save the updated message to database
                        if let Some(handle) = TAURI_APP.get() {
                            let updated_message = msg.clone();
                            let chat_id = chat.id.clone();
                            drop(state); // Release lock before async operation
                            let _ = crate::db::save_message(handle.clone(), &chat_id, &updated_message).await;
                            return Ok(true);
                        }
                    }
                    
                    return Ok(was_added);
                }
            }
            
            Ok(false)
        }
        ChatType::MlsGroup => {
            // For group chats, send reaction through MLS
            let reference_event = EventId::from_hex(&reference_id).map_err(|e| e.to_string())?;
            
            // Build reaction rumor manually (simpler than using the builder for group chats)
            let rumor = EventBuilder::new(Kind::Reaction, &emoji)
                .tag(Tag::event(reference_event))
                .build(my_public_key);
            let rumor_id = rumor.id.ok_or("Failed to get rumor ID")?.to_hex();
            
            // Send through MLS
            crate::mls::send_mls_message(&chat_id, rumor, None).await?;
            
            // Add reaction to local state
            let reaction = Reaction {
                id: rumor_id,
                reference_id: reference_id.clone(),
                author_id: my_public_key.to_hex(),
                emoji,
            };
            
            let mut state = STATE.lock().await;
            if let Some(chat) = state.chats.iter_mut().find(|c| c.id == chat_id) {
                if let Some(msg) = chat.messages.iter_mut().find(|m| m.id == reference_id) {
                    let was_added = msg.add_reaction(reaction, Some(&chat_id));
                    
                    if was_added {
                        // Save the updated message to database
                        if let Some(handle) = TAURI_APP.get() {
                            let updated_message = msg.clone();
                            let chat_id_clone = chat.id.clone();
                            drop(state); // Release lock before async operation
                            let _ = crate::db::save_message(handle.clone(), &chat_id_clone, &updated_message).await;
                            return Ok(true);
                        }
                    }
                    
                    return Ok(was_added);
                }
            }
            
            Ok(false)
        }
    }
}

#[tauri::command]
pub async fn fetch_msg_metadata(chat_id: String, msg_id: String) -> bool {
    // Find the message we're extracting metadata from
    let text = {
        let mut state = STATE.lock().await;
        let message = state.chats.iter_mut()
            .find(|chat| chat.id == chat_id)
            .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == msg_id));

        // Message might not be in backend state (e.g., loaded via get_messages_around_id)
        match message {
            Some(msg) => msg.content.clone(),
            None => return false,
        }
    };

    // Extract URLs from the message
    const MAX_URLS_TO_TRY: usize = 3;
    let urls = util::extract_https_urls(text.as_str());
    if urls.is_empty() {
        return false;
    }

    // Only try the first few URLs
    for url in urls.into_iter().take(MAX_URLS_TO_TRY) {
        match net::fetch_site_metadata(&url).await {
            Ok(metadata) => {
                let has_content = metadata.og_title.is_some()
                    || metadata.og_description.is_some()
                    || metadata.og_image.is_some()
                    || metadata.title.is_some()
                    || metadata.description.is_some();

                // Extracted metadata!
                if has_content {
                    // Re-fetch the message and add our metadata
                    let mut state = STATE.lock().await;
                    let msg = state.chats.iter_mut()
                        .find(|chat| chat.id == chat_id)
                        .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == msg_id))
                        .unwrap();
                    msg.preview_metadata = Some(metadata);

                    // Update the renderer
                    let handle = TAURI_APP.get().unwrap();
                    handle.emit("message_update", serde_json::json!({
                        "old_id": &msg_id,
                        "message": &msg,
                        "chat_id": &chat_id
                    })).unwrap();

                    // Save the updated message with metadata to the DB
                    let message_to_save = msg.clone();
                    drop(state); // Release lock before async DB operation
                    let _ = crate::db::save_message(handle.clone(), &chat_id, &message_to_save).await;
                    return true;
                }
            }
            Err(_) => continue,
        }
    }
    false
}

/// Forward an attachment from one message to a different chat
/// This is used for "Play & Invite" functionality in Mini Apps
/// Returns the new message ID if successful
#[tauri::command]
pub async fn forward_attachment(
    source_msg_id: String,
    source_attachment_id: String,
    target_chat_id: String,
) -> Result<String, String> {
    // Find the source message and attachment
    let attachment_path = {
        let state = STATE.lock().await;
        
        // Search through all chats to find the message
        let mut found_path: Option<String> = None;
        for chat in &state.chats {
            if let Some(msg) = chat.messages.iter().find(|m| m.id == source_msg_id) {
                // Find the attachment in the message
                if let Some(attachment) = msg.attachments.iter().find(|a| a.id == source_attachment_id) {
                    if !attachment.path.is_empty() && attachment.downloaded {
                        found_path = Some(attachment.path.clone());
                    }
                }
                break;
            }
        }
        
        found_path.ok_or_else(|| "Attachment not found or not downloaded".to_string())?
    };
    
    // Verify the file exists
    if !std::path::Path::new(&attachment_path).exists() {
        return Err("Attachment file not found on disk".to_string());
    }
    
    // Send the file to the target chat using the existing file_message function
    // The hash-based reuse will automatically avoid re-uploading
    file_message(target_chat_id, String::new(), attachment_path).await?;
    
    // Return success - the new message ID will be emitted via the normal message flow
    Ok("forwarded".to_string())
}

/// Edit a message by sending an edit event
/// Returns the edit event ID if successful
#[tauri::command]
pub async fn edit_message(
    message_id: String,
    chat_id: String,
    new_content: String,
) -> Result<String, String> {
    use crate::chat::ChatType;
    use crate::stored_event::event_kind;

    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await.map_err(|e| e.to_string())?;
    let my_public_key = signer.get_public_key().await.map_err(|e| e.to_string())?;
    let my_npub = my_public_key.to_bech32().map_err(|e| e.to_string())?;

    // Determine chat type and get db chat_id
    let (chat_type, db_chat_id) = {
        let state = STATE.lock().await;
        let chat = state.chats.iter().find(|c| c.id == chat_id)
            .ok_or_else(|| "Chat not found".to_string())?;
        let chat_type = chat.chat_type.clone();

        // Get db chat ID
        let handle = TAURI_APP.get().ok_or("App handle not available")?;
        let db_chat_id = crate::db::get_chat_id_by_identifier(handle, &chat_id)?;

        (chat_type, db_chat_id)
    };

    // Build the edit rumor
    let reference_event = EventId::from_hex(&message_id).map_err(|e| e.to_string())?;
    let rumor = EventBuilder::new(Kind::from_u16(event_kind::MESSAGE_EDIT), &new_content)
        .tag(Tag::event(reference_event))
        .build(my_public_key);
    let edit_id = rumor.id.ok_or("Failed to get edit rumor ID")?.to_hex();
    let created_at = rumor.created_at.as_u64();

    match chat_type {
        ChatType::DirectMessage => {
            // For DMs, send gift-wrapped edit
            let receiver_pubkey = PublicKey::from_bech32(&chat_id).map_err(|e| e.to_string())?;

            // Send edit to the receiver
            client
                .gift_wrap(&receiver_pubkey, rumor.clone(), [])
                .await
                .map_err(|e| e.to_string())?;

            // Send edit to ourselves for recovery
            client
                .gift_wrap(&my_public_key, rumor, [])
                .await
                .map_err(|e| e.to_string())?;
        }
        ChatType::MlsGroup => {
            // For group chats, send edit through MLS
            crate::mls::send_mls_message(&chat_id, rumor, None).await?;
        }
    }

    // Save edit event to database
    if let Some(handle) = TAURI_APP.get() {
        crate::db::save_edit_event(
            handle,
            &edit_id,
            &message_id,
            &new_content,
            db_chat_id,
            None, // user_id derived from npub stored in event
            &my_npub,
        ).await?;
    }

    // Update local state
    let mut state = STATE.lock().await;
    if let Some(chat) = state.chats.iter_mut().find(|c| c.id == chat_id) {
        if let Some(msg) = chat.messages.iter_mut().find(|m| m.id == message_id) {
            // Build edit history if not present
            if msg.edit_history.is_none() {
                // First edit: save original content
                msg.edit_history = Some(vec![EditEntry {
                    content: msg.content.clone(),
                    edited_at: msg.at,
                }]);
            }

            // Add new edit to history
            let edit_timestamp_ms = created_at * 1000;
            if let Some(ref mut history) = msg.edit_history {
                history.push(EditEntry {
                    content: new_content.clone(),
                    edited_at: edit_timestamp_ms,
                });
            }

            // Update the message content and flag
            msg.content = new_content;
            msg.edited = true;

            // Emit update to frontend
            if let Some(handle) = TAURI_APP.get() {
                handle.emit("message_update", serde_json::json!({
                    "old_id": &message_id,
                    "message": &msg,
                    "chat_id": &chat_id
                })).ok();
            }
        }
    }

    Ok(edit_id)
}