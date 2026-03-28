//! Core type definitions for Vector.
//!
//! These types are the canonical definitions used by all Vector clients, SDKs,
//! and interfaces. They mirror the types in src-tauri but are Tauri-independent.

use std::sync::Arc;

// ============================================================================
// Message
// ============================================================================

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Message {
    pub id: String,
    pub content: String,
    pub replied_to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replied_to_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replied_to_npub: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replied_to_has_attachment: Option<bool>,
    pub preview_metadata: Option<SiteMetadata>,
    pub attachments: Vec<Attachment>,
    pub reactions: Vec<Reaction>,
    pub at: u64,
    pub pending: bool,
    pub failed: bool,
    pub mine: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub npub: Option<String>,
    #[serde(skip_serializing, default)]
    pub wrapper_event_id: Option<String>,
    #[serde(default)]
    pub edited: bool,
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
    pub fn get_attachment_mut(&mut self, id: &str) -> Option<&mut Attachment> {
        self.attachments.iter_mut().find(|p| p.id == id)
    }

    /// Apply an edit to this message, updating content and tracking history.
    pub fn apply_edit(&mut self, new_content: String, edited_at: u64) {
        if self.edit_history.is_none() {
            self.edit_history = Some(vec![EditEntry {
                content: self.content.clone(),
                edited_at: self.at,
            }]);
        }

        if let Some(ref mut history) = self.edit_history {
            if history.iter().any(|e| e.edited_at == edited_at) {
                return;
            }
            history.push(EditEntry { content: new_content, edited_at });
            history.sort_by_key(|e| e.edited_at);
            if let Some(latest) = history.last() {
                self.content = latest.content.clone();
            }
        }

        self.edited = true;
    }

    /// Add a reaction. Returns true if the reaction was new.
    ///
    /// Unlike the src-tauri version, this does NOT emit events — the caller
    /// is responsible for notifying the UI via `emit_event`.
    pub fn add_reaction(&mut self, reaction: Reaction) -> bool {
        if !self.reactions.iter().any(|r| r.id == reaction.id) {
            self.reactions.push(reaction);
            true
        } else {
            false
        }
    }
}

// ============================================================================
// Attachment
// ============================================================================

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Attachment {
    pub id: String,
    pub key: String,
    pub nonce: String,
    pub extension: String,
    pub name: String,
    pub url: String,
    pub path: String,
    pub size: u64,
    pub img_meta: Option<ImageMetadata>,
    pub downloading: bool,
    pub downloaded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webxdc_topic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheme_version: Option<String>,
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
            name: String::new(),
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

// ============================================================================
// Supporting Types
// ============================================================================

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct ImageMetadata {
    pub thumbhash: String,
    pub width: u32,
    pub height: u32,
}

/// Pre-upload file data.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AttachmentFile {
    #[serde(skip, default = "default_arc_bytes")]
    pub bytes: Arc<Vec<u8>>,
    pub img_meta: Option<ImageMetadata>,
    pub extension: String,
    pub name: String,
}

fn default_arc_bytes() -> Arc<Vec<u8>> {
    Arc::new(Vec::new())
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Reaction {
    pub id: String,
    pub reference_id: String,
    pub author_id: String,
    pub emoji: String,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct EditEntry {
    pub content: String,
    pub edited_at: u64,
}

// ============================================================================
// Site Metadata (for URL previews)
// ============================================================================

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct SiteMetadata {
    pub domain: String,
    pub og_title: Option<String>,
    pub og_description: Option<String>,
    pub og_image: Option<String>,
    pub og_url: Option<String>,
    pub og_type: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub favicon: Option<String>,
}

// ============================================================================
// Login Result
// ============================================================================

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct LoginResult {
    pub npub: String,
    pub has_encryption: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // Message default tests
    // ========================================================================

    #[test]
    fn message_default_fields() {
        let msg = Message::default();
        assert_eq!(msg.id, "", "default id should be empty");
        assert_eq!(msg.content, "", "default content should be empty");
        assert_eq!(msg.replied_to, "", "default replied_to should be empty");
        assert!(msg.replied_to_content.is_none(), "default replied_to_content should be None");
        assert!(msg.replied_to_npub.is_none(), "default replied_to_npub should be None");
        assert!(msg.replied_to_has_attachment.is_none(), "default replied_to_has_attachment should be None");
        assert!(msg.preview_metadata.is_none(), "default preview_metadata should be None");
        assert!(msg.attachments.is_empty(), "default attachments should be empty");
        assert!(msg.reactions.is_empty(), "default reactions should be empty");
        assert_eq!(msg.at, 0, "default timestamp should be 0");
        assert!(!msg.pending, "default pending should be false");
        assert!(!msg.failed, "default failed should be false");
        assert!(!msg.mine, "default mine should be false");
        assert!(msg.npub.is_none(), "default npub should be None");
        assert!(msg.wrapper_event_id.is_none(), "default wrapper_event_id should be None");
        assert!(!msg.edited, "default edited should be false");
        assert!(msg.edit_history.is_none(), "default edit_history should be None");
    }

    // ========================================================================
    // Message::apply_edit tests
    // ========================================================================

    #[test]
    fn apply_edit_tracks_history() {
        let mut msg = Message {
            content: "original".to_string(),
            at: 1000,
            ..Default::default()
        };
        msg.apply_edit("edited content".to_string(), 2000);

        assert!(msg.edited, "edited flag should be set after apply_edit");
        let history = msg.edit_history.as_ref().expect("edit_history should exist");
        assert_eq!(history.len(), 2, "history should have 2 entries (original + edit)");
        assert_eq!(history[0].content, "original", "first history entry should be original content");
        assert_eq!(history[0].edited_at, 1000, "first history entry timestamp should be original timestamp");
        assert_eq!(history[1].content, "edited content", "second history entry should be new content");
        assert_eq!(history[1].edited_at, 2000, "second history entry timestamp should be edit timestamp");
    }

    #[test]
    fn apply_edit_dedup_by_timestamp() {
        let mut msg = Message {
            content: "original".to_string(),
            at: 1000,
            ..Default::default()
        };
        msg.apply_edit("edit1".to_string(), 2000);
        msg.apply_edit("duplicate".to_string(), 2000); // same timestamp

        let history = msg.edit_history.as_ref().expect("edit_history should exist");
        assert_eq!(history.len(), 2, "duplicate timestamp edit should be ignored, history should have 2 entries");
        assert_eq!(msg.content, "edit1", "content should remain from first edit at timestamp 2000");
    }

    #[test]
    fn apply_edit_content_updated_to_latest() {
        let mut msg = Message {
            content: "original".to_string(),
            at: 1000,
            ..Default::default()
        };
        msg.apply_edit("edit1".to_string(), 2000);
        msg.apply_edit("edit2".to_string(), 3000);
        msg.apply_edit("edit3".to_string(), 4000);

        assert_eq!(msg.content, "edit3", "content should reflect the latest edit by timestamp");
        let history = msg.edit_history.as_ref().expect("edit_history should exist");
        assert_eq!(history.len(), 4, "history should have 4 entries (original + 3 edits)");
    }

    #[test]
    fn apply_edit_out_of_order_timestamps() {
        let mut msg = Message {
            content: "original".to_string(),
            at: 1000,
            ..Default::default()
        };
        // Apply edits out of order
        msg.apply_edit("late edit".to_string(), 5000);
        msg.apply_edit("early edit".to_string(), 2000);

        assert_eq!(msg.content, "late edit", "content should be the edit with the highest timestamp");
        let history = msg.edit_history.as_ref().expect("edit_history should exist");
        // Sorted by edited_at
        assert_eq!(history[0].edited_at, 1000, "history should be sorted: original first");
        assert_eq!(history[1].edited_at, 2000, "history should be sorted: early edit second");
        assert_eq!(history[2].edited_at, 5000, "history should be sorted: late edit third");
    }

    #[test]
    fn apply_edit_preserves_original_in_history() {
        let mut msg = Message {
            content: "keep this".to_string(),
            at: 500,
            ..Default::default()
        };
        msg.apply_edit("new content".to_string(), 600);
        let history = msg.edit_history.as_ref().unwrap();
        assert_eq!(history[0].content, "keep this", "original content should be preserved as first history entry");
        assert_eq!(history[0].edited_at, 500, "original timestamp should be preserved");
    }

    // ========================================================================
    // Message::add_reaction tests
    // ========================================================================

    #[test]
    fn add_reaction_returns_true_for_new() {
        let mut msg = Message::default();
        let reaction = Reaction {
            id: "r1".to_string(),
            reference_id: "msg1".to_string(),
            author_id: "user1".to_string(),
            emoji: "\u{1F44D}".to_string(),
        };
        let added = msg.add_reaction(reaction);
        assert!(added, "add_reaction should return true for a new reaction");
        assert_eq!(msg.reactions.len(), 1, "reactions vec should have 1 entry");
    }

    #[test]
    fn add_reaction_returns_false_for_duplicate() {
        let mut msg = Message::default();
        let reaction = Reaction {
            id: "r1".to_string(),
            reference_id: "msg1".to_string(),
            author_id: "user1".to_string(),
            emoji: "\u{1F44D}".to_string(),
        };
        msg.add_reaction(reaction.clone());
        let added = msg.add_reaction(reaction);
        assert!(!added, "add_reaction should return false for duplicate reaction (same id)");
        assert_eq!(msg.reactions.len(), 1, "duplicate reaction should not be added");
    }

    #[test]
    fn add_reaction_different_ids_all_added() {
        let mut msg = Message::default();
        for i in 0..5 {
            let reaction = Reaction {
                id: format!("r{}", i),
                reference_id: "msg1".to_string(),
                author_id: format!("user{}", i),
                emoji: "\u{2764}\u{FE0F}".to_string(),
            };
            assert!(msg.add_reaction(reaction), "reaction {} should be new", i);
        }
        assert_eq!(msg.reactions.len(), 5, "all 5 unique reactions should be added");
    }

    // ========================================================================
    // Message serde tests
    // ========================================================================

    #[test]
    fn message_serde_roundtrip() {
        let msg = Message {
            id: "abc123".to_string(),
            content: "Hello world".to_string(),
            replied_to: "def456".to_string(),
            replied_to_content: Some("previous msg".to_string()),
            replied_to_npub: Some("npub1xyz".to_string()),
            replied_to_has_attachment: Some(true),
            preview_metadata: None,
            attachments: vec![Attachment::default()],
            reactions: vec![Reaction {
                id: "r1".to_string(),
                reference_id: "abc123".to_string(),
                author_id: "user1".to_string(),
                emoji: "\u{1F44D}".to_string(),
            }],
            at: 1700000000,
            pending: false,
            failed: false,
            mine: true,
            npub: Some("npub1me".to_string()),
            wrapper_event_id: Some("wrap1".to_string()),
            edited: true,
            edit_history: Some(vec![EditEntry {
                content: "original".to_string(),
                edited_at: 1699999999,
            }]),
        };

        let json = serde_json::to_string(&msg).expect("serialize should succeed");
        let deserialized: Message = serde_json::from_str(&json).expect("deserialize should succeed");

        assert_eq!(deserialized.id, msg.id, "id should survive serde roundtrip");
        assert_eq!(deserialized.content, msg.content, "content should survive serde roundtrip");
        assert_eq!(deserialized.at, msg.at, "timestamp should survive serde roundtrip");
        assert_eq!(deserialized.mine, msg.mine, "mine flag should survive serde roundtrip");
        assert_eq!(deserialized.edited, msg.edited, "edited flag should survive serde roundtrip");
        assert_eq!(deserialized.reactions.len(), 1, "reactions should survive serde roundtrip");
        assert_eq!(deserialized.attachments.len(), 1, "attachments should survive serde roundtrip");
        assert_eq!(deserialized.replied_to_content, msg.replied_to_content, "replied_to_content should survive roundtrip");
        assert_eq!(deserialized.npub, msg.npub, "npub should survive serde roundtrip");
    }

    #[test]
    fn message_serde_skip_serializing_if_works() {
        let msg = Message {
            id: "test".to_string(),
            replied_to_content: None,
            replied_to_npub: None,
            replied_to_has_attachment: None,
            npub: None,
            edit_history: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&msg).expect("serialize should succeed");
        assert!(!json.contains("replied_to_content"), "None replied_to_content should be omitted from JSON");
        assert!(!json.contains("replied_to_npub"), "None replied_to_npub should be omitted from JSON");
        assert!(!json.contains("replied_to_has_attachment"), "None replied_to_has_attachment should be omitted");
        assert!(!json.contains("\"npub\""), "None npub should be omitted from JSON");
        assert!(!json.contains("edit_history"), "None edit_history should be omitted from JSON");
    }

    #[test]
    fn message_serde_wrapper_event_id_skip_serializing() {
        let msg = Message {
            wrapper_event_id: Some("should_not_appear".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&msg).expect("serialize should succeed");
        assert!(!json.contains("wrapper_event_id"), "wrapper_event_id should always be skipped during serialization");
    }

    #[test]
    fn message_serde_wrapper_event_id_deserializes_default() {
        // wrapper_event_id has skip_serializing + default, so it should deserialize as None from JSON
        let json = r#"{"id":"x","content":"","replied_to":"","attachments":[],"reactions":[],"at":0,"pending":false,"failed":false,"mine":false,"edited":false}"#;
        let msg: Message = serde_json::from_str(json).expect("deserialize should succeed");
        assert!(msg.wrapper_event_id.is_none(), "wrapper_event_id should default to None when absent from JSON");
    }

    // ========================================================================
    // Attachment tests
    // ========================================================================

    #[test]
    fn attachment_default_has_downloaded_true() {
        let att = Attachment::default();
        assert!(att.downloaded, "default attachment should have downloaded=true");
    }

    #[test]
    fn attachment_default_values() {
        let att = Attachment::default();
        assert_eq!(att.id, "", "default id should be empty");
        assert_eq!(att.key, "", "default key should be empty");
        assert_eq!(att.nonce, "", "default nonce should be empty");
        assert_eq!(att.extension, "", "default extension should be empty");
        assert_eq!(att.name, "", "default name should be empty");
        assert_eq!(att.url, "", "default url should be empty");
        assert_eq!(att.path, "", "default path should be empty");
        assert_eq!(att.size, 0, "default size should be 0");
        assert!(att.img_meta.is_none(), "default img_meta should be None");
        assert!(!att.downloading, "default downloading should be false");
        assert!(att.webxdc_topic.is_none(), "default webxdc_topic should be None");
        assert!(att.group_id.is_none(), "default group_id should be None");
        assert!(att.original_hash.is_none(), "default original_hash should be None");
        assert!(att.scheme_version.is_none(), "default scheme_version should be None");
        assert!(att.mls_filename.is_none(), "default mls_filename should be None");
    }

    #[test]
    fn attachment_serde_roundtrip() {
        let att = Attachment {
            id: "att1".to_string(),
            key: "key123".to_string(),
            nonce: "nonce456".to_string(),
            extension: "png".to_string(),
            name: "photo.png".to_string(),
            url: "https://example.com/photo.png".to_string(),
            path: "/tmp/photo.png".to_string(),
            size: 12345,
            img_meta: Some(ImageMetadata {
                thumbhash: "abc".to_string(),
                width: 800,
                height: 600,
            }),
            downloading: false,
            downloaded: true,
            webxdc_topic: Some("game".to_string()),
            group_id: Some("g1".to_string()),
            original_hash: Some("sha256hash".to_string()),
            scheme_version: Some("v2".to_string()),
            mls_filename: Some("encrypted.bin".to_string()),
        };

        let json = serde_json::to_string(&att).expect("serialize should succeed");
        let deserialized: Attachment = serde_json::from_str(&json).expect("deserialize should succeed");
        assert_eq!(deserialized, att, "attachment should survive serde roundtrip");
    }

    #[test]
    fn attachment_optional_fields_none_by_default() {
        let json = r#"{"id":"a","key":"k","nonce":"n","extension":"jpg","name":"f.jpg","url":"","path":"","size":0,"downloading":false,"downloaded":true}"#;
        let att: Attachment = serde_json::from_str(json).expect("deserialize should succeed");
        assert!(att.img_meta.is_none(), "img_meta should be None when absent");
        assert!(att.webxdc_topic.is_none(), "webxdc_topic should be None when absent");
        assert!(att.group_id.is_none(), "group_id should be None when absent");
        assert!(att.original_hash.is_none(), "original_hash should be None when absent");
        assert!(att.scheme_version.is_none(), "scheme_version should be None when absent");
        assert!(att.mls_filename.is_none(), "mls_filename should be None when absent");
    }

    #[test]
    fn attachment_serde_skip_serializing_if_nones() {
        let att = Attachment::default();
        let json = serde_json::to_string(&att).expect("serialize should succeed");
        assert!(!json.contains("webxdc_topic"), "None webxdc_topic should be omitted");
        assert!(!json.contains("group_id"), "None group_id should be omitted");
        assert!(!json.contains("original_hash"), "None original_hash should be omitted");
        assert!(!json.contains("scheme_version"), "None scheme_version should be omitted");
        assert!(!json.contains("mls_filename"), "None mls_filename should be omitted");
    }

    // ========================================================================
    // ImageMetadata tests
    // ========================================================================

    #[test]
    fn image_metadata_default() {
        let meta = ImageMetadata::default();
        assert_eq!(meta.thumbhash, "", "default thumbhash should be empty");
        assert_eq!(meta.width, 0, "default width should be 0");
        assert_eq!(meta.height, 0, "default height should be 0");
    }

    // ========================================================================
    // EditEntry tests
    // ========================================================================

    #[test]
    fn edit_entry_serde_roundtrip() {
        let entry = EditEntry {
            content: "edited text".to_string(),
            edited_at: 1700000000,
        };
        let json = serde_json::to_string(&entry).expect("serialize should succeed");
        let deserialized: EditEntry = serde_json::from_str(&json).expect("deserialize should succeed");
        assert_eq!(deserialized, entry, "EditEntry should survive serde roundtrip");
    }
}
