use std::sync::Arc;
use ::image::ImageEncoder;
use nostr_sdk::prelude::*;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::crypto;
use crate::db;
use crate::Attachment;
use crate::AttachmentFile;
use crate::Message;
use crate::STATE;
use crate::TAURI_APP;
use crate::NOSTR_CLIENT;
use crate::PRIVATE_NIP96_CONFIG;

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
        at: current_time.as_secs(),
        attachments: Vec::new(),
        reactions: Vec::new(),
        pending: true,
        failed: false,
        mine: true,
    };
    STATE.lock().await.add_message(&receiver, msg.clone());

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

        // Encrypt the attachment
        let params = crypto::generate_encryption_params();
        let enc_file = crypto::encrypt_data(attached_file.bytes.as_slice(), &params).unwrap();

        // Update the attachment in-state
        {
            // Use a clone of the Arc for this block
            let pending_id_clone = Arc::clone(&pending_id);
            
            // Retrieve the Pending Message
            let mut state = STATE.lock().await;
            let chat = state.get_profile_mut(&receiver).unwrap();
            let message = chat.get_message_mut(pending_id_clone.as_ref()).unwrap();

            // Choose the appropriate base directory based on platform
            let base_directory = if cfg!(target_os = "ios") {
                tauri::path::BaseDirectory::Document
            } else {
                tauri::path::BaseDirectory::Download
            };

            // Resolve the directory path using the determined base directory
            let dir = handle.path().resolve("vector", base_directory).unwrap();

            // Store the nonce-based file name on-disk for future reference
            let nonce_file_path = dir.join(format!("{}.{}", &params.nonce, &attached_file.extension));

            // Create the vector directory if it doesn't exist
            std::fs::create_dir_all(&dir).unwrap();

            // Save the nonce-named file
            std::fs::write(&nonce_file_path, &attached_file.bytes).unwrap();

            // Add the Attachment in-state (with our local path, to prevent re-downloading it accidentally from server)
            message.attachments.push(Attachment {
                // Temp: id will soon become a SHA256 hash of the file
                id: params.nonce.clone(),
                key: params.key.clone(),
                nonce: params.nonce.clone(),
                extension: attached_file.extension.clone(),
                url: String::new(),
                path: nonce_file_path.to_string_lossy().to_string(),
                size: enc_file.len() as u64,
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
        let mime_type = match attached_file.extension.as_str() {
            // Images
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            // Audio
            "wav" => "audio/wav",
            "mp3" => "audio/mp3",
            // Videos
            "mp4" => "video/mp4",
            "webm" => "video/webm",
            "mov" => "video/quicktime",
            "avi" => "video/x-msvideo",
            "mkv" => "video/x-matroska",
            // Unknown
            _ => "application/octet-stream",
        };

        // Upload the file to the server
        let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");
        let signer = client.signer().await.unwrap();
        let conf = PRIVATE_NIP96_CONFIG.wait();
        let file_size = enc_file.len();
        // Clone the Arc outside the closure for use inside a seperate-threaded progress callback
        let pending_id_for_callback = Arc::clone(&pending_id);
        // Create a progress callback for file uploads
        let progress_callback: crate::upload::ProgressCallback = Box::new(move |percentage, _| {
                // This is a simple callback that logs progress but could be enhanced to emit events
                if let Some(pct) = percentage {
                    handle.emit("attachment_upload_progress", serde_json::json!({
                        "id": pending_id_for_callback.as_ref(),
                        "progress": pct
                    })).unwrap();
                }
            Ok(())
        });

        match crate::upload::upload_data_with_progress(&signer, &conf, enc_file, Some(mime_type), None, progress_callback).await {
            Ok(url) => {
                // Create the attachment rumor
                let attachment_rumor = EventBuilder::new(Kind::from_u16(15), url.to_string());

                // Append decryption keys and file metadata
                attachment_rumor
                    .tag(Tag::public_key(receiver_pubkey))
                    .tag(Tag::custom(TagKind::custom("file-type"), [mime_type]))
                    .tag(Tag::custom(TagKind::custom("size"), [file_size.to_string()]))
                    .tag(Tag::custom(TagKind::custom("encryption-algorithm"), ["aes-gcm"]))
                    .tag(Tag::custom(TagKind::custom("decryption-key"), [params.key.as_str()]))
                    .tag(Tag::custom(TagKind::custom("decryption-nonce"), [params.nonce.as_str()]))
            },
            Err(_) => {
                // The file upload failed: so we mark the message as failed and notify of an error
                let pending_id_for_failure = Arc::clone(&pending_id);
                let mut state = STATE.lock().await;
                let chat = state.get_profile_mut(&receiver).unwrap();
                let failed_msg = chat.get_message_mut(pending_id_for_failure.as_ref()).unwrap();
                failed_msg.failed = true;

                // Update the frontend
                handle.emit("message_update", serde_json::json!({
                    "old_id": pending_id_for_failure.as_ref(),
                    "message": &failed_msg,
                    "chat_id": &receiver
                })).unwrap();

                // Return the error
                return Err(String::from("Failed to upload file"));
            }
        }
    };

    // If a reply reference is included, add the tag
    if !msg.replied_to.is_empty() {
        rumor = rumor.tag(Tag::custom(
            TagKind::e(),
            [msg.replied_to, String::from(""), String::from("reply")],
        ));
    }

    // Build the rumor with our key (unsigned)
    let built_rumor = rumor.build(my_public_key);
    let rumor_id = built_rumor.id.unwrap();

    // Send message to the real receiver
    match client
        .gift_wrap(&receiver_pubkey, built_rumor.clone(), [])
        .await
    {
        Ok(_) => {
            // Send message to our own public key, to allow for message recovering
            match client
                .gift_wrap(&my_public_key, built_rumor, [])
                .await
            {
                Ok(_) => {
                    // Mark the message as a success
                    let pending_id_for_success = Arc::clone(&pending_id);
                    let mut state = STATE.lock().await;
                    let chat = state.get_profile_mut(&receiver).unwrap();
                    let sent_msg = chat.get_message_mut(pending_id_for_success.as_ref()).unwrap();
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
                    db::save_message(handle.clone(), sent_msg.clone(), receiver).await.unwrap();
                    return Ok(true);
                }
                Err(_) => {
                    // This is an odd case; the message was sent to the receiver, but NOT ourselves
                    // We'll class it as sent, for now...
                    let pending_id_for_partial = Arc::clone(&pending_id);
                    let mut state = STATE.lock().await;
                    let chat = state.get_profile_mut(&receiver).unwrap();
                    let sent_ish_msg = chat.get_message_mut(pending_id_for_partial.as_ref()).unwrap();
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
                    db::save_message(handle.clone(), sent_ish_msg.clone(), receiver).await.unwrap();
                    return Ok(true);
                }
            }
        }
        Err(_) => {
            // Mark the message as a failure, bad message, bad!
            let pending_id_for_final = Arc::clone(&pending_id);
            let mut state = STATE.lock().await;
            let chat = state.get_profile_mut(&receiver).unwrap();
            let failed_msg = chat.get_message_mut(pending_id_for_final.as_ref()).unwrap();
            failed_msg.failed = true;
            return Ok(false);
        }
    }
}

#[tauri::command]
pub async fn paste_message<R: Runtime>(handle: AppHandle<R>, receiver: String, replied_to: String, transparent: bool) -> Result<bool, String> {
    // Copy the image from the clipboard
    let img = handle.clipboard().read_image().unwrap();

    // Create the encoder directly with a Vec<u8>
    let mut png_data = Vec::new();
    let encoder = ::image::codecs::png::PngEncoder::new(&mut png_data);

    // Get original pixels
    let original_pixels = img.rgba();

    // Windows: check that every image has a non-zero-ish Alpha channel, if not, this is probably a non-PNG/GIF which has had it's Alpha channel nuked
    let mut _transparency_bug_search = false;
    #[cfg(target_os = "windows")]
    {
        _transparency_bug_search = original_pixels.iter().skip(3).step_by(4).all(|&a| a <= 2);
    }

    // For non-transparent images: we need to manually account for the zero'ing out of the Alpha channel
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
        &pixels,               // raw pixels
        img.width(),           // width
        img.height(),          // height
        ::image::ExtendedColorType::Rgba8                  // color type
    ).map_err(|e| e.to_string()).unwrap();

    // Generate an Attachment File
    let attachment_file = AttachmentFile {
        bytes: png_data,
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
        extension: String::from("wav")
    };

    // Message the file to the intended user
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}

#[tauri::command]
pub async fn file_message(receiver: String, replied_to: String, file_path: String) -> Result<bool, String> {
    // Parse the file extension
    let ext = file_path.clone().rsplit('.').next().unwrap_or("").to_lowercase();

    // Load the file
    let bytes = std::fs::read(file_path.as_str()).unwrap();

    // Generate an Attachment File
    let attachment_file = AttachmentFile {
        bytes,
        extension: ext.to_string()
    };

    // Message the file to the intended user
    message(receiver, String::new(), replied_to, Some(attachment_file)).await
}
