use std::sync::Arc;
use ::image::{ImageBuffer, ImageEncoder, Rgba};
use blurhash;
use nostr_sdk::prelude::*;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::crypto;
use crate::db_migration::{save_chat, save_chat_messages};
use crate::net;
use crate::STATE;
use crate::util::{self, calculate_file_hash};
use crate::TAURI_APP;
use crate::NOSTR_CLIENT;
use crate::PRIVATE_NIP96_CONFIG;

#[cfg(target_os = "android")]
use crate::android::{clipboard, filesystem};

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Message {
    pub id: String,
    pub content: String,
    pub replied_to: String,
    pub preview_metadata: Option<net::SiteMetadata>,
    pub attachments: Vec<Attachment>,
    pub reactions: Vec<Reaction>,
    pub at: u64,
    pub pending: bool,
    pub failed: bool,
    pub mine: bool,
}

impl Default for Message {
    fn default() -> Self {
        Self {
            id: String::new(),
            content: String::new(),
            replied_to: String::new(),
            preview_metadata: None,
            attachments: Vec::new(),
            reactions: Vec::new(),
            at: 0,
            pending: false,
            failed: false,
            mine: false,
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
                if let Some(chat) = state.get_chat(&receiver) {
                    let all_messages = chat.messages.clone();
                    let _ = save_chat_messages(handle.clone(), &chat.id, &all_messages).await;
                }
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
        preview_metadata: None,
        at: current_time.as_millis() as u64,
        attachments: Vec::new(),
        reactions: Vec::new(),
        pending: true,
        failed: false,
        mine: true,
    };
    STATE.lock().await.add_message_to_participant(&receiver, msg.clone());

    // Grab our pubkey
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Convert the Bech32 String in to a PublicKey
    let receiver_pubkey = PublicKey::from_bech32(receiver.clone().as_str()).unwrap();

    // Prepare the NIP-17 rumor
    let handle = TAURI_APP.get().unwrap();
    let mut rumor = if file.is_none() {
        // Send the text message to our frontend
        handle.emit("message_new", serde_json::json!({
            "message": &msg,
            "chat_id": &receiver
        })).unwrap();

        // Text Message
        EventBuilder::private_msg_rumor(receiver_pubkey, msg.content)
    } else {
        let attached_file = file.unwrap();

        // Calculate the file hash first (before encryption)
        let file_hash = calculate_file_hash(&attached_file.bytes);

        // Check for existing attachment with same hash across all profiles BEFORE encrypting
        let existing_attachment = {
            let state = STATE.lock().await;
            let mut found_attachment: Option<(String, Attachment)> = None;
            
            // Search through all chats for an attachment with matching hash
            for chat in &state.chats {
                for message in &chat.messages {
                    for attachment in &message.attachments {
                        if attachment.id == file_hash && !attachment.url.is_empty() {
                            // Found a matching attachment with a valid URL
                            // Use the first participant as the profile ID for compatibility
                            if let Some(participant_id) = chat.participants.first() {
                                found_attachment = Some((participant_id.clone(), attachment.clone()));
                                break;
                            }
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
            
            found_attachment
        };

        // Only encrypt if we don't have an existing attachment (optimization)
        let (params, enc_file) = if existing_attachment.is_some() {
            // Skip encryption for duplicate files - we'll reuse existing encryption params
            (crypto::EncryptionParams { key: String::new(), nonce: String::new() }, Vec::new())
        } else {
            // Encrypt the attachment only if it's a new file
            let params = crypto::generate_encryption_params();
            let enc_file = crypto::encrypt_data(attached_file.bytes.as_slice(), &params).unwrap();
            (params, enc_file)
        };

        // Update the attachment in-state
        {
            // Use a clone of the Arc for this block
            let pending_id_clone = Arc::clone(&pending_id);
            
            // Retrieve the Pending Message
            let mut state = STATE.lock().await;
            let message = state.chats.iter_mut()
                .find(|chat| chat.has_participant(&receiver))
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
                downloaded: true
            });

            // Send the pending file upload to our frontend
            handle.emit("message_new", serde_json::json!({
                "message": &message,
                "chat_id": &receiver
            })).unwrap();
        }

        // Format a Mime Type from the file extension
        let mime_type = util::mime_from_extension(&attached_file.extension);

        // Check if we found an existing attachment with the same hash
        let mut should_upload = true;
        let attachment_rumor = if let Some((_found_profile_id, existing_attachment)) = existing_attachment {
            // Verify the URL is still live before reusing
            let url_is_live = match net::check_url_live(&existing_attachment.url).await {
                Ok(is_live) => is_live,
                Err(_) => false // Treat errors as dead URL
            };
            
            if url_is_live {
                should_upload = false;
                
                // Update our pending message with the existing URL
                {
                    let pending_id_for_update = Arc::clone(&pending_id);
                    let mut state = STATE.lock().await;
                    let message = state.chats.iter_mut()
                        .find(|chat| chat.has_participant(&receiver))
                        .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == *pending_id_for_update))
                        .unwrap();
                    if let Some(attachment) = message.attachments.last_mut() {
                        attachment.url = existing_attachment.url.clone();
                    }
                }
                
                // Create the attachment rumor with the existing URL
                let mut attachment_rumor = EventBuilder::new(Kind::from_u16(15), existing_attachment.url)
                
                // Append decryption keys and file metadata (using existing attachment's params)
                    .tag(Tag::public_key(receiver_pubkey))
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
            let conf = PRIVATE_NIP96_CONFIG.wait();
            let file_size = enc_file.len();
            // Clone the Arc outside the closure for use inside a seperate-threaded progress callback
            let pending_id_for_callback = Arc::clone(&pending_id);
            // Create a progress callback for file uploads
            let progress_callback: crate::upload::ProgressCallback = Box::new(move |percentage, _| {
                    if let Some(pct) = percentage {
                        handle.emit("attachment_upload_progress", serde_json::json!({
                            "id": pending_id_for_callback.as_ref(),
                            "progress": pct
                        })).unwrap();
                    }
                Ok(())
            });

            // Upload the file with both a Progress Emitter and multiple re-try attempts in case of connection instability
            match crate::upload::upload_data_with_progress(&signer, &conf, enc_file, Some(mime_type.as_str()), None, progress_callback, Some(3), Some(std::time::Duration::from_secs(2))).await {
                Ok(url) => {
                    // Update our pending message with the uploaded URL
                    {
                        let pending_id_for_url_update = Arc::clone(&pending_id);
                        let mut state = STATE.lock().await;
                        let message = state.chats.iter_mut()
                            .find(|chat| chat.has_participant(&receiver))
                            .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == *pending_id_for_url_update))
                            .unwrap();
                        if let Some(attachment) = message.attachments.last_mut() {
                            attachment.url = url.to_string();
                        }
                    }
                    
                    // Create the attachment rumor
                    let mut attachment_rumor = EventBuilder::new(Kind::from_u16(15), url.to_string())

                    // Append decryption keys and file metadata
                        .tag(Tag::public_key(receiver_pubkey))
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

                    attachment_rumor
                },
                Err(_) => {
                    // The file upload failed: so we mark the message as failed and notify of an error
                    mark_message_failed(Arc::clone(&pending_id), &receiver).await;
                    // Return the error
                    return Err(String::from("Failed to upload file"));
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
                    final_output = Some(output);
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
    match client
        .gift_wrap(&my_public_key, built_rumor, [])
        .await
    {
        Ok(_) => {
            // Mark the message as a success
            let pending_id_for_success = Arc::clone(&pending_id);
            let mut state = STATE.lock().await;
            let sent_msg = state.chats.iter_mut()
                .find(|chat| chat.has_participant(&receiver))
                .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == *pending_id_for_success))
                .unwrap();
            sent_msg.id = rumor_id.to_hex();
            sent_msg.pending = false;

            // Update the frontend
            handle.emit("message_update", serde_json::json!({
                "old_id": pending_id_for_success.as_ref(),
                "message": &sent_msg,
                "chat_id": &receiver
            })).unwrap();

            // Save the message to our DB
            let handle = TAURI_APP.get().unwrap();
            if let Some(chat) = state.get_chat(&receiver) {
                let _ = save_chat(handle.clone(), chat).await;
                let all_messages = chat.messages.clone();
                let _ = save_chat_messages(handle.clone(), &chat.id, &all_messages).await;
            }
            return Ok(true);
        }
        Err(_) => {
            // This is an odd case; the message was sent to the receiver, but NOT ourselves
            // We'll class it as sent, for now...
            let pending_id_for_partial = Arc::clone(&pending_id);
            let mut state = STATE.lock().await;
            let sent_ish_msg = state.chats.iter_mut()
                .find(|chat| chat.has_participant(&receiver))
                .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == *pending_id_for_partial))
                .unwrap();
            sent_ish_msg.id = rumor_id.to_hex();
            sent_ish_msg.pending = false;

            // Update the frontend
            handle.emit("message_update", serde_json::json!({
                "old_id": pending_id_for_partial.as_ref(),
                "message": &sent_ish_msg,
                "chat_id": &receiver
            })).unwrap();

            // Save the message to our DB
            let handle = TAURI_APP.get().unwrap();
            if let Some(chat) = state.get_chat(&receiver) {
                let _ = save_chat(handle.clone(), chat).await;
                let all_messages = chat.messages.clone();
                let _ = save_chat_messages(handle.clone(), &chat.id, &all_messages).await;
            }
            return Ok(true);
        }
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

    // Create the encoder directly with a Vec<u8>
    let mut png_data = Vec::new();
    let encoder = ::image::codecs::png::PngEncoder::new(&mut png_data);

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

    // Encode directly from pixels to PNG bytes
    encoder.write_image(
        &pixels,
        img.width(),
        img.height(),
        ::image::ExtendedColorType::Rgba8
    ).map_err(|e| e.to_string())?;

    // Generate image metadata with Blurhash and dimensions
    let img_meta: Option<ImageMetadata> = match blurhash::encode(4, 3, img.width(), img.height(), &pixels) {
        Ok(hash) => Some(ImageMetadata {
            blurhash: hash,
            width: img.width(),
            height: img.height(),
        }),
        Err(_) => None
    };

    // Generate an Attachment File
    let attachment_file = AttachmentFile {
        bytes: png_data,
        img_meta,
        extension: String::from("png")
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

#[tauri::command]
pub async fn file_message(receiver: String, replied_to: String, file_path: String) -> Result<bool, String> {
    // Load the file as AttachmentFile
    let mut attachment_file = {
        #[cfg(not(target_os = "android"))]
        {
            // Read file bytes
            let bytes = std::fs::read(&file_path)
                .map_err(|e| format!("Failed to read file: {}", e))?;

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
            filesystem::read_android_uri(file_path)?
        }
    };

    // Generate image metadata if the file is an image
    if matches!(attachment_file.extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp") {
        // Try to load and decode the image
        if let Ok(img) = ::image::load_from_memory(&attachment_file.bytes) {
            // Convert to RGBA8 format for blurhash
            let rgba_img = img.to_rgba8();
            let (width, height) = rgba_img.dimensions();
            let pixels = rgba_img.as_raw();

            // Generate Blurhash
            if let Ok(hash) = blurhash::encode(4, 3, width, height, pixels) {
                attachment_file.img_meta = Some(ImageMetadata {
                    blurhash: hash,
                    width,
                    height,
                });
            }
        }
    }

    // Message the file to the intended user
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

#[tauri::command]
pub async fn react(reference_id: String, npub: String, emoji: String) -> Result<bool, ()> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Prepare the EventID and Pubkeys for rumor building
    let reference_event = EventId::from_hex(reference_id.as_str()).unwrap();
    let receiver_pubkey = PublicKey::from_bech32(npub.as_str()).unwrap();

    // Grab our pubkey
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Build our NIP-25 Reaction rumor
    let rumor = EventBuilder::reaction_extended(
        reference_event,
        receiver_pubkey,
        Some(Kind::PrivateDirectMessage),
        &emoji,
    )
    .build(my_public_key);
    let rumor_id = rumor.id.unwrap();

    // Send reaction to the real receiver
    client
        .gift_wrap(&receiver_pubkey, rumor.clone(), [])
        .await
        .unwrap();

    // Send reaction to our own public key, to allow for recovering
    match client.gift_wrap(&my_public_key, rumor, []).await {
        Ok(_) => {
            // And add our reaction locally
            let reaction = Reaction {
                id: rumor_id.to_hex(),
                reference_id: reference_id.clone(),
                author_id: my_public_key.to_hex(),
                emoji,
            };

            // Commit it to our local state
            let mut state = STATE.lock().await;
            let msg = state.chats.iter_mut()
                .find(|chat| chat.has_participant(&npub))
                .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == reference_id))
                .unwrap();
            let chat_id = npub.clone();
            let was_reaction_added_to_state = msg.add_reaction(reaction, Some(&chat_id));
            if was_reaction_added_to_state {
                // Save the message's reaction to our DB
                let handle = TAURI_APP.get().unwrap();
                if let Some(chat) = state.get_chat(&npub) {
                    let all_messages = chat.messages.clone();
                    let _ = save_chat_messages(handle.clone(), &chat.id, &all_messages).await;
                }
            }
            return Ok(was_reaction_added_to_state);
        }
        Err(e) => {
            eprintln!("Error: {:?}", e);
            return Ok(false);
        }
    }
}

#[tauri::command]
pub async fn fetch_msg_metadata(npub: String, msg_id: String) -> bool {
    // Find the message we're extracting metadata from
    let text = {
        let mut state = STATE.lock().await;
        let message = state.chats.iter_mut()
            .find(|chat| chat.has_participant(&npub))
            .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == msg_id))
            .unwrap();
        message.content.clone()
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
                        .find(|chat| chat.has_participant(&npub))
                        .and_then(|chat| chat.messages.iter_mut().find(|m| m.id == msg_id))
                        .unwrap();
                    msg.preview_metadata = Some(metadata);

                    // Update the renderer
                    let handle = TAURI_APP.get().unwrap();
                    handle.emit("message_update", serde_json::json!({
                        "old_id": &msg_id,
                        "message": &msg,
                        "chat_id": &npub
                    })).unwrap();

                    // Save the new Metadata to the DB
                    if let Some(chat) = state.get_chat(&npub) {
                        let all_messages = chat.messages.clone();
                        let _ = save_chat_messages(handle.clone(), &chat.id, &all_messages).await;
                    }
                    return true;
                }
            }
            Err(_) => continue,
        }
    }
    false
}