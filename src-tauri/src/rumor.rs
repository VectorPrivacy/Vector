//! Rumor Processing — Tauri-specific extensions
//!
//! Core rumor processing lives in `vector_core::rumor`. This module provides
//! Tauri-specific additions that require TAURI_APP, MlsService, or MDK.

pub use vector_core::rumor::*;

use std::borrow::Cow;
use std::path::PathBuf;
use nostr_sdk::prelude::*;
use tauri::Manager;

/// Resolve the platform-appropriate download directory for file attachments.
/// Uses TAURI_APP when available, falls back to app data dir.
pub fn resolve_download_dir() -> PathBuf {
    if let Some(handle) = TAURI_APP.get() {
        let base = if cfg!(target_os = "ios") {
            tauri::path::BaseDirectory::Document
        } else {
            tauri::path::BaseDirectory::Download
        };
        handle.path().resolve("vector", base).unwrap_or_default()
    } else if let Ok(data_dir) = crate::account_manager::get_app_data_dir() {
        data_dir.join("vector_downloads")
    } else {
        PathBuf::from("/tmp/vector_downloads")
    }
}
use std::path::Path;
use crate::{Attachment, TAURI_APP};
use crate::message::ImageMetadata;
use crate::util::{bytes_to_hex_string, hex_string_to_bytes};
use crate::mls::MlsService;

/// Process a rumor with MLS imeta attachment support.
///
/// Wraps vector_core's `process_rumor` and adds MLS-specific MIP-04 imeta parsing
/// that requires TAURI_APP + MlsService (cannot live in vector-core).
///
/// For DMs, this is identical to `vector_core::rumor::process_rumor`.
/// For MLS groups, it additionally parses imeta tags into file attachments.
pub async fn process_rumor_with_mls(
    rumor: &RumorEvent,
    context: &RumorContext,
    download_dir: &Path,
) -> Result<RumorProcessingResult, String> {
    let result = vector_core::rumor::process_rumor(rumor.clone(), context.clone(), download_dir)?;

    // Only MLS groups need imeta parsing
    if context.conversation_type != ConversationType::MlsGroup {
        return Ok(result);
    }

    // For MLS text messages, check for imeta attachments
    match result {
        RumorProcessingResult::TextMessage(mut msg) => {
            let mut attachments = parse_mls_imeta_attachments(rumor, context).await;
            if attachments.is_empty() {
                return Ok(RumorProcessingResult::TextMessage(msg));
            }

            // Apply name tag to first attachment (MLS events carry one attachment per message)
            if let Some(name_tag) = rumor.tags.find(TagKind::Custom(Cow::Borrowed("name"))) {
                if let Some(name) = name_tag.content() {
                    let file_name = vector_core::crypto::sanitize_filename(name);
                    if !file_name.is_empty() {
                        if let Some(att) = attachments.first_mut() {
                            att.name = file_name;
                        }
                    }
                }
            }

            // Apply webxdc-topic to all attachments
            if let Some(topic_tag) = rumor.tags.find(TagKind::Custom(Cow::Borrowed("webxdc-topic"))) {
                if let Some(topic) = topic_tag.content() {
                    let topic = topic.to_string();
                    for att in &mut attachments {
                        att.webxdc_topic = Some(topic.clone());
                    }
                }
            }

            msg.attachments = attachments;
            Ok(RumorProcessingResult::FileAttachment(msg))
        }
        other => Ok(other),
    }
}

/// Parse MIP-04 imeta tags from an MLS group message
///
/// Extracts file attachments from imeta tags using MDK's encrypted media parser.
/// Returns a list of Attachment objects with group_id set for MLS decryption.
///
/// This function requires TAURI_APP and MlsService — it cannot live in vector-core.
pub async fn parse_mls_imeta_attachments(
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

    let mls_service = match MlsService::new_persistent_static() {
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
        let encrypted_hash = vector_core::rumor::extract_hash_from_blossom_url(&media_ref.url)
            .unwrap_or_else(|| bytes_to_hex_string(&media_ref.original_hash));

        // Build image metadata from dimensions if available
        let img_meta = media_ref.dimensions.and_then(|(width, height)| {
            let thumbhash = tag.as_slice().iter()
                .find(|s| s.starts_with("thumbhash "))
                .map(|s| s.strip_prefix("thumbhash ").unwrap_or("").to_string());

            match thumbhash {
                Some(th) if !th.is_empty() => Some(ImageMetadata {
                    thumbhash: th,
                    width,
                    height,
                }),
                _ => {
                    Some(ImageMetadata {
                        thumbhash: String::new(),
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
            name: String::new(),  // Set by caller from top-level name tag
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
