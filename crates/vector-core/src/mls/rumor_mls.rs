//! MLS-aware rumor processing — extends vector-core's process_rumor with MIP-04 imeta parsing.

use std::borrow::Cow;
use std::path::Path;
use nostr_sdk::prelude::TagKind;

use crate::rumor::{RumorEvent, RumorContext, ConversationType, RumorProcessingResult};
use crate::types::{Attachment, ImageMetadata};
use crate::mls::MlsService;

/// Process a rumor with MLS imeta support.
///
/// Calls vector-core's `process_rumor`, then for MLS group text messages,
/// parses MIP-04 imeta tags to extract encrypted file attachments.
pub async fn process_rumor_with_mls(
    rumor: &RumorEvent,
    context: &RumorContext,
    download_dir: &Path,
) -> Result<RumorProcessingResult, String> {
    let result = crate::rumor::process_rumor(rumor.clone(), context.clone(), download_dir)?;

    // Only MLS groups need imeta parsing
    if context.conversation_type != ConversationType::MlsGroup {
        return Ok(result);
    }

    // For MLS text messages, check for imeta attachments
    match result {
        RumorProcessingResult::TextMessage(mut msg) => {
            let mut attachments = parse_mls_imeta_attachments(rumor, context, download_dir);
            if attachments.is_empty() {
                return Ok(RumorProcessingResult::TextMessage(msg));
            }

            // Apply name tag to first attachment (MLS events carry one attachment per message)
            if let Some(name_tag) = rumor.tags.find(TagKind::Custom(Cow::Borrowed("name"))) {
                if let Some(name) = name_tag.content() {
                    let file_name = crate::crypto::sanitize_filename(name);
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

/// Parse MIP-04 imeta tags from an MLS group message.
///
/// Extracts file attachments from imeta tags using MDK's encrypted media parser.
/// Returns a list of Attachment objects with group_id set for MLS decryption.
pub fn parse_mls_imeta_attachments(
    rumor: &RumorEvent,
    context: &RumorContext,
    download_dir: &Path,
) -> Vec<Attachment> {
    use nostr_sdk::prelude::Tag;

    // Find all imeta tags
    let imeta_tags: Vec<&Tag> = rumor.tags.iter()
        .filter(|t| t.kind() == TagKind::Custom(Cow::Borrowed("imeta")))
        .collect();

    if imeta_tags.is_empty() {
        return Vec::new();
    }

    let mls_service = match MlsService::new_persistent_static() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[MIP-04] Failed to create MLS service: {}", e);
            return Vec::new();
        }
    };

    // Look up group metadata for engine_group_id
    let groups = match mls_service.read_groups() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("[MIP-04] Failed to read groups: {}", e);
            return Vec::new();
        }
    };

    let group_meta = match groups.iter().find(|g| g.group.group_id == context.conversation_id) {
        Some(g) => g,
        None => {
            eprintln!("[MIP-04] Group not found: {}", context.conversation_id);
            return Vec::new();
        }
    };

    if group_meta.group.engine_group_id.is_empty() {
        eprintln!("[MIP-04] Group has no engine_group_id");
        return Vec::new();
    }

    let engine_gid_bytes = crate::simd::hex::hex_string_to_bytes(&group_meta.group.engine_group_id);
    let gid = mdk_core::GroupId::from_slice(&engine_gid_bytes);

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
        let tag_values: Vec<String> = tag.as_slice().iter().map(|s| s.to_string()).collect();
        let mdk_tag = match nostr::Tag::parse(&tag_values) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[MIP-04] Failed to parse imeta tag: {}", e);
                continue;
            }
        };

        let media_ref = match media_manager.parse_imeta_tag(&mdk_tag) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[MIP-04] Failed to parse imeta: {}", e);
                continue;
            }
        };

        let extension = media_ref.filename
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_string();

        let encrypted_hash = crate::rumor::extract_hash_from_blossom_url(&media_ref.url)
            .unwrap_or_else(|| crate::simd::hex::bytes_to_hex_string(&media_ref.original_hash));

        let img_meta = media_ref.dimensions.and_then(|(width, height)| {
            let thumbhash = tag.as_slice().iter()
                .find(|s| s.starts_with("thumbhash "))
                .map(|s| s.strip_prefix("thumbhash ").unwrap_or("").to_string());

            match thumbhash {
                Some(th) if !th.is_empty() => Some(ImageMetadata { thumbhash: th, width, height }),
                _ => Some(ImageMetadata { thumbhash: String::new(), width, height }),
            }
        });

        let file_path = download_dir.join(format!("{}.{}", &encrypted_hash, &extension))
            .to_string_lossy()
            .to_string();

        let downloaded = std::path::Path::new(&file_path).exists();

        let size = tag.as_slice().iter()
            .find(|s| s.starts_with("size "))
            .and_then(|s| s.strip_prefix("size "))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        attachments.push(Attachment {
            id: encrypted_hash,
            key: String::new(),
            nonce: crate::simd::hex::bytes_to_hex_string(&media_ref.nonce),
            extension,
            name: String::new(),
            url: media_ref.url.clone(),
            path: file_path,
            size,
            img_meta,
            downloading: false,
            downloaded,
            webxdc_topic: None,
            group_id: Some(context.conversation_id.clone()),
            original_hash: Some(crate::simd::hex::bytes_to_hex_string(&media_ref.original_hash)),
            scheme_version: Some(media_ref.scheme_version.clone()),
            mls_filename: Some(media_ref.filename.clone()),
        });
    }

    attachments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rumor::{RumorEvent, RumorContext, ConversationType};

    #[test]
    fn process_rumor_with_mls_passthrough_for_dm() {
        // DM context should pass through without MLS processing
        let rumor = RumorEvent {
            id: nostr_sdk::EventId::all_zeros(),
            kind: nostr_sdk::Kind::from_u16(14),
            content: "hello".into(),
            tags: nostr_sdk::Tags::default(),
            created_at: nostr_sdk::Timestamp::now(),
            pubkey: nostr_sdk::Keys::generate().public_key(),
        };
        let context = RumorContext {
            sender: rumor.pubkey,
            is_mine: false,
            conversation_id: "npub1test".into(),
            conversation_type: ConversationType::DirectMessage,
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(process_rumor_with_mls(&rumor, &context, std::path::Path::new("/tmp")));
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), RumorProcessingResult::TextMessage(_)));
    }

    #[test]
    fn parse_mls_imeta_empty_tags() {
        // No imeta tags should return empty
        let rumor = RumorEvent {
            id: nostr_sdk::EventId::all_zeros(),
            kind: nostr_sdk::Kind::from_u16(14),
            content: "hello".into(),
            tags: nostr_sdk::Tags::default(),
            created_at: nostr_sdk::Timestamp::now(),
            pubkey: nostr_sdk::Keys::generate().public_key(),
        };
        let context = RumorContext {
            sender: rumor.pubkey,
            is_mine: false,
            conversation_id: "test-group".into(),
            conversation_type: ConversationType::MlsGroup,
        };

        let result = parse_mls_imeta_attachments(&rumor, &context, std::path::Path::new("/tmp"));
        assert!(result.is_empty());
    }
}
