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
    /// File extension of the replied-to attachment, when known. Lets the reply
    /// quote show the file type (Photo/Video/...) for off-screen targets the
    /// in-memory message can't supply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replied_to_attachment_extension: Option<String>,
    pub preview_metadata: Option<SiteMetadata>,
    pub attachments: Vec<Attachment>,
    pub reactions: Vec<Reaction>,
    pub at: u64,
    /// NIP-40 expiry (unix seconds). None = permanent. Set on Self-Destruct
    /// Timer messages; drives the live countdown and the local purge sweep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiration: Option<u64>,
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
    /// NIP-30 custom-emoji `["emoji", shortcode, url]` tags that travelled
    /// with the rumor. Empty for stock-emoji messages; frontend renderer
    /// uses this map to swap `:shortcode:` for `<img>` before twemoji runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emoji_tags: Vec<EmojiTag>,
    /// `["bot", <pubkey hex>]` recipient tags that travelled with the rumor
    /// (bot-command addressing), surfaced as npubs. Empty = untagged/broadcast:
    /// any matching bot may answer. Populated on live parse only (not persisted
    /// — commands are actioned at delivery, never replayed from history).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addressed_bots: Vec<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct EmojiTag {
    pub shortcode: String,
    pub url: String,
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
            replied_to_attachment_extension: None,
            preview_metadata: None,
            attachments: Vec::new(),
            reactions: Vec::new(),
            at: 0,
            expiration: None,
            pending: false,
            failed: false,
            mine: false,
            npub: None,
            wrapper_event_id: None,
            edited: false,
            edit_history: None,
            emoji_tags: Vec::new(),
            addressed_bots: Vec::new(),
        }
    }
}

impl EmojiTag {
    /// Pull NIP-30 `["emoji", shortcode, url]` triples out of an event's
    /// tag list. Invalid shortcodes are dropped to match `emoji_packs`'
    /// parser — keeps the wire format and renderer aligned.
    pub fn extract_from_tags<'a, I>(tags: I) -> Vec<EmojiTag>
    where
        I: IntoIterator<Item = &'a nostr_sdk::Tag>,
    {
        let mut out: Vec<EmojiTag> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for tag in tags {
            let parts: Vec<&str> = tag.as_slice().iter().map(|s| s.as_str()).collect();
            if parts.len() < 3 || parts[0] != "emoji" {
                continue;
            }
            let shortcode = parts[1];
            // `~` is the reserved separator for duplicate-shortcode disambiguation
            // (`love~2`); pack-defined codes never contain it, but message tags do.
            if shortcode.is_empty()
                || !shortcode.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '~')
            {
                continue;
            }
            if !seen.insert(shortcode.to_string()) {
                continue;
            }
            out.push(EmojiTag {
                shortcode: shortcode.to_string(),
                url: parts[2].to_string(),
            });
        }
        out
    }

    /// Same as `extract_from_tags` but operates on the flat
    /// `Vec<Vec<String>>` representation used by `StoredEvent`. Lets
    /// DB readers round-trip emoji tags without going through nostr-sdk.
    pub fn extract_from_stored(tags: &[Vec<String>]) -> Vec<EmojiTag> {
        let mut out: Vec<EmojiTag> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for tag in tags {
            if tag.len() < 3 || tag[0] != "emoji" { continue; }
            let shortcode = &tag[1];
            // `~` is the reserved disambiguation separator (see `extract_from_tags`).
            if shortcode.is_empty()
                || !shortcode.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '~')
            {
                continue;
            }
            if !seen.insert(shortcode.clone()) { continue; }
            out.push(EmojiTag {
                shortcode: shortcode.clone(),
                url: tag[2].clone(),
            });
        }
        out
    }
}

impl Message {
    pub fn get_attachment_mut(&mut self, id: &str) -> Option<&mut Attachment> {
        self.attachments.iter_mut().find(|p| p.id == id)
    }

    /// Apply an edit to this message, updating content and tracking history.
    ///
    /// `emoji_tags` are the NIP-30 custom-emoji tags resolved from the new
    /// content. They're adopted only when this edit becomes the newest
    /// revision, so an out-of-order older edit can't clobber the live
    /// content's emoji.
    pub fn apply_edit(&mut self, new_content: String, edited_at: u64, emoji_tags: Vec<EmojiTag>) {
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
                if latest.edited_at == edited_at {
                    self.emoji_tags = emoji_tags;
                }
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

    // ========================================================================
    // Mention / Tag API
    // ========================================================================

    /// Check if this message mentions a specific npub (e.g. `@npub1abc...`).
    pub fn mentions(&self, npub: &str) -> bool {
        self.content.contains(&format!("@{}", npub))
    }

    /// Check if this message mentions the current user.
    pub fn mentions_me(&self) -> bool {
        crate::state::my_public_key()
            .and_then(|pk| nostr_sdk::prelude::ToBech32::to_bech32(&pk).ok())
            .map_or(false, |my_npub| self.content.contains(&format!("@{}", my_npub)))
    }

    /// Check if this message contains an `@everyone` ping.
    pub fn mentions_everyone(&self) -> bool {
        self.content.contains("@everyone")
    }

    /// Extract all mentioned npubs from the message content.
    ///
    /// Returns a list of npub strings (without the `@` prefix) found in the message.
    pub fn mentioned_npubs(&self) -> Vec<&str> {
        extract_mentions(&self.content)
    }
}

// ============================================================================
// Mention Utilities
// ============================================================================

const BECH32_CHARS: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

/// Format an npub as a mention tag for embedding in message content.
///
/// ```
/// assert_eq!(vector_core::types::mention("npub1abc"), "@npub1abc");
/// ```
pub fn mention(npub: &str) -> String {
    format!("@{}", npub)
}

/// Extract every npub mentioned in a string: `@npub1...`, `nostr:npub1...`,
/// or a bare `npub1...`. A full npub standing alone IS a mention (matching
/// the render layer, which pills all three shapes); one glued into a longer
/// alphanumeric token is not.
///
/// Returns npub strings without any prefix. Validates bech32 characters.
pub fn extract_mentions(content: &str) -> Vec<&str> {
    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut mentions = Vec::new();
    let mut i = 0;

    // npub = "npub1" (5) + 58 bech32 chars = 63 bytes
    while i + 63 <= len {
        if &bytes[i..i + 5] == b"npub1" {
            let npub_end = i + 63;
            let prev_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
            let next_ok = npub_end >= len || !bytes[npub_end].is_ascii_alphanumeric();
            let valid = prev_ok
                && next_ok
                && bytes[i + 5..npub_end]
                    .iter()
                    .all(|b| BECH32_CHARS.contains(&b.to_ascii_lowercase()));
            if valid {
                mentions.push(&content[i..npub_end]);
                i = npub_end;
                continue;
            }
        }
        i += 1;
    }

    mentions
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
    /// NIP-30 custom-emoji image URL. Present when the reaction content is
    /// `:shortcode:` and the originating event carried an `["emoji", code,
    /// url]` tag. Frontend renders the image directly when present, so the
    /// reaction chip survives reloads + missing pack subscriptions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji_url: Option<String>,
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
    use nostr_sdk::prelude::ToBech32;

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
        msg.apply_edit("edited content".to_string(), 2000, Vec::new());

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
        msg.apply_edit("edit1".to_string(), 2000, Vec::new());
        msg.apply_edit("duplicate".to_string(), 2000, Vec::new()); // same timestamp

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
        msg.apply_edit("edit1".to_string(), 2000, Vec::new());
        msg.apply_edit("edit2".to_string(), 3000, Vec::new());
        msg.apply_edit("edit3".to_string(), 4000, Vec::new());

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
        msg.apply_edit("late edit".to_string(), 5000, Vec::new());
        msg.apply_edit("early edit".to_string(), 2000, Vec::new());

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
        msg.apply_edit("new content".to_string(), 600, Vec::new());
        let history = msg.edit_history.as_ref().unwrap();
        assert_eq!(history[0].content, "keep this", "original content should be preserved as first history entry");
        assert_eq!(history[0].edited_at, 500, "original timestamp should be preserved");
    }

    #[test]
    fn apply_edit_adopts_latest_emoji_tags() {
        let mut msg = Message {
            content: "hi :wave:".to_string(),
            at: 1000,
            emoji_tags: vec![EmojiTag { shortcode: "wave".into(), url: "u/wave".into() }],
            ..Default::default()
        };
        // Edit swaps the custom emoji — the new content's tags must win.
        msg.apply_edit("hi :tada:".to_string(), 2000,
            vec![EmojiTag { shortcode: "tada".into(), url: "u/tada".into() }]);
        assert_eq!(msg.emoji_tags.len(), 1);
        assert_eq!(msg.emoji_tags[0].shortcode, "tada");

        // An out-of-order OLDER edit must not clobber the live content's emoji.
        msg.apply_edit("hi :wave:".to_string(), 1500,
            vec![EmojiTag { shortcode: "wave".into(), url: "u/wave".into() }]);
        assert_eq!(msg.content, "hi :tada:", "newest content wins");
        assert_eq!(msg.emoji_tags[0].shortcode, "tada", "newest content's emoji wins");
    }

    #[test]
    fn apply_edit_clears_emoji_when_edit_removes_them() {
        let mut msg = Message {
            content: "hi :wave:".to_string(),
            at: 1000,
            emoji_tags: vec![EmojiTag { shortcode: "wave".into(), url: "u/wave".into() }],
            ..Default::default()
        };
        msg.apply_edit("plain text".to_string(), 2000, Vec::new());
        assert!(msg.emoji_tags.is_empty(), "editing out the emoji should drop its tag");
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
            emoji_url: None,
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
            emoji_url: None,
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
                emoji_url: None,
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
            expiration: Some(1893456000),
            id: "abc123".to_string(),
            content: "Hello world".to_string(),
            replied_to: "def456".to_string(),
            replied_to_content: Some("previous msg".to_string()),
            replied_to_npub: Some("npub1xyz".to_string()),
            replied_to_has_attachment: Some(true),
            replied_to_attachment_extension: Some("png".to_string()),
            preview_metadata: None,
            attachments: vec![Attachment::default()],
            reactions: vec![Reaction {
                id: "r1".to_string(),
                reference_id: "abc123".to_string(),
                author_id: "user1".to_string(),
                emoji: "\u{1F44D}".to_string(),
                emoji_url: None,
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
            emoji_tags: Vec::new(),
            addressed_bots: Vec::new(),
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
        assert_eq!(deserialized.replied_to_attachment_extension, msg.replied_to_attachment_extension, "replied_to_attachment_extension should survive roundtrip");
        assert_eq!(deserialized.npub, msg.npub, "npub should survive serde roundtrip");
    }

    #[test]
    fn message_serde_skip_serializing_if_works() {
        let msg = Message {
            id: "test".to_string(),
            replied_to_content: None,
            replied_to_npub: None,
            replied_to_has_attachment: None,
            replied_to_attachment_extension: None,
            npub: None,
            edit_history: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&msg).expect("serialize should succeed");
        assert!(!json.contains("replied_to_content"), "None replied_to_content should be omitted from JSON");
        assert!(!json.contains("replied_to_npub"), "None replied_to_npub should be omitted from JSON");
        assert!(!json.contains("replied_to_has_attachment"), "None replied_to_has_attachment should be omitted");
        assert!(!json.contains("replied_to_attachment_extension"), "None replied_to_attachment_extension should be omitted");
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

    // ========================================================================
    // Mention / Tag API tests
    // ========================================================================

    #[test]
    fn mentions_specific_npub() {
        // Generate a real npub for testing
        let keys = nostr_sdk::Keys::generate();
        let npub = keys.public_key().to_bech32().unwrap();
        let msg = Message {
            content: format!("hey @{} check this", npub),
            ..Default::default()
        };
        assert!(msg.mentions(&npub));
        assert!(!msg.mentions("npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdmf9lg"));
    }

    #[test]
    fn mentions_everyone_detected() {
        let msg = Message { content: "hey @everyone look at this".into(), ..Default::default() };
        assert!(msg.mentions_everyone());

        let msg2 = Message { content: "hey everyone".into(), ..Default::default() };
        assert!(!msg2.mentions_everyone());
    }

    #[test]
    fn extract_mentions_multiple() {
        let keys1 = nostr_sdk::Keys::generate();
        let keys2 = nostr_sdk::Keys::generate();
        let npub1 = keys1.public_key().to_bech32().unwrap();
        let npub2 = keys2.public_key().to_bech32().unwrap();
        let content = format!("cc @{} and @{} for review", npub1, npub2);
        let mentions = super::extract_mentions(&content);
        assert_eq!(mentions.len(), 2);
        assert!(mentions.contains(&npub1.as_str()));
        assert!(mentions.contains(&npub2.as_str()));
    }

    #[test]
    fn extract_mentions_none() {
        let mentions = super::extract_mentions("no mentions here");
        assert!(mentions.is_empty());
    }

    #[test]
    fn extract_mentions_invalid_bech32() {
        // 'b', 'i', 'o' are NOT valid bech32 characters
        let content = "@npub1bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let mentions = super::extract_mentions(content);
        assert!(mentions.is_empty());
    }

    #[test]
    fn mention_format() {
        assert_eq!(super::mention("npub1abc"), "@npub1abc");
    }

    #[test]
    fn extract_mentions_at_everyone_not_npub() {
        let mentions = super::extract_mentions("hey @everyone");
        assert!(mentions.is_empty());
    }

    #[test]
    fn extract_mentions_bare_and_prefixed_shapes() {
        let npub = nostr_sdk::Keys::generate().public_key().to_bech32().unwrap();
        for content in [
            format!("{}", npub),
            format!("rep given to nostr:{}! nice", npub),
            format!("is {} yours?", npub),
            format!("https://vectorapp.io/profile/{}", npub),
        ] {
            let mentions = super::extract_mentions(&content);
            assert_eq!(mentions, vec![npub.as_str()], "shape: {}", content);
        }
    }

    #[test]
    fn extract_mentions_rejects_glued_tokens() {
        let npub = nostr_sdk::Keys::generate().public_key().to_bech32().unwrap();
        // Glued into a longer alphanumeric token on either side = not a mention.
        assert!(super::extract_mentions(&format!("x{}", npub)).is_empty());
        assert!(super::extract_mentions(&format!("{}9", npub)).is_empty());
    }
}
