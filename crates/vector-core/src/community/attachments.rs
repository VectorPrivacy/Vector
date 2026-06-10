//! Community message attachments (NIP-92 `imeta`).
//!
//! Unlike NIP-17 DMs (one media item per event), a Community message event carries its
//! caption in `content` plus one `imeta` tag per attachment — so a single message can
//! mix text and N files. Each `imeta` holds the per-file AES-GCM key+nonce (the NIP-17
//! attachment technique: fresh random key per file), so the Blossom ciphertext is only
//! decryptable by members who can open the event.

use std::path::Path;
use nostr_sdk::prelude::*;
use crate::types::{Attachment, ImageMetadata};

const IMETA: &str = "imeta";

/// Encode an [`Attachment`] as a NIP-92 `imeta` tag with Vector's encryption fields.
/// Entries are space-delimited `key value` strings (NIP-92 form); a value may contain
/// spaces (e.g. a filename) since only the first space delimits key from value.
pub fn attachment_to_imeta(att: &Attachment) -> Tag {
    let mut fields: Vec<String> = Vec::with_capacity(10);
    fields.push(format!("url {}", att.url));
    fields.push(format!("m {}", crate::crypto::mime_from_extension(&att.extension)));
    fields.push("encryption-algorithm aes-gcm".to_string());
    fields.push(format!("decryption-key {}", att.key));
    fields.push(format!("decryption-nonce {}", att.nonce));
    if att.size > 0 {
        fields.push(format!("size {}", att.size));
    }
    if let Some(h) = att.original_hash.as_deref().filter(|h| !h.is_empty()) {
        fields.push(format!("ox {}", h));
    }
    if !att.name.is_empty() {
        fields.push(format!("name {}", att.name));
    }
    if let Some(meta) = &att.img_meta {
        if !meta.thumbhash.is_empty() {
            fields.push(format!("thumb {}", meta.thumbhash));
        }
        fields.push(format!("dim {}x{}", meta.width, meta.height));
    }
    // Mini Apps: the send-time-minted realtime topic rides the imeta so every
    // member joins the same gossip topic (see `crate::webxdc::mint_topic_id`).
    if let Some(topic) = att.webxdc_topic.as_deref().filter(|t| !t.is_empty()) {
        fields.push(format!("webxdc-topic {}", topic));
    }
    Tag::custom(TagKind::Custom(IMETA.into()), fields)
}

/// Read a single `key value` field from an `imeta` tag's entries (value is everything
/// after the first space, so spaces in the value are preserved).
fn field<'a>(entries: &'a [String], key: &str) -> Option<&'a str> {
    entries.iter().find_map(|e| {
        e.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(' '))
    })
}

/// Parse a single `imeta` tag into an [`Attachment`]. `None` if the tag isn't an `imeta`
/// or is missing the required url / decryption fields. `download_dir` computes the
/// (not-yet-downloaded) local target path, mirroring the DM file-attachment path.
pub fn attachment_from_imeta(tag: &Tag, download_dir: &Path) -> Option<Attachment> {
    let entries = tag.as_slice();
    if entries.first().map(String::as_str) != Some(IMETA) {
        return None;
    }
    let body = &entries[1..];

    let url = field(body, "url")?.to_string();
    if url.is_empty() {
        return None;
    }
    let key = field(body, "decryption-key")?.to_string();
    let nonce = field(body, "decryption-nonce")?.to_string();

    let mime = field(body, "m").unwrap_or("application/octet-stream");
    let name = field(body, "name").map(crate::crypto::sanitize_filename).unwrap_or_default();
    // Prefer the filename's extension (accurate for .toml/.rs/etc. that MIME maps to
    // octet-stream); fall back to the MIME-derived extension.
    let extension = name
        .rsplit('.')
        .next()
        .filter(|e| !e.is_empty() && *e != name)
        .map(|e| e.to_lowercase())
        .unwrap_or_else(|| crate::crypto::extension_from_mime(mime));

    let size = field(body, "size").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let original_hash = field(body, "ox").map(|s| s.to_string()).filter(|s| !s.is_empty());

    let img_meta = {
        let thumb = field(body, "thumb").map(|s| s.to_string());
        let dim = field(body, "dim").and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?))
        });
        match (thumb, dim) {
            (Some(thumbhash), Some((width, height))) => Some(ImageMetadata { thumbhash, width, height }),
            _ => None,
        }
    };

    // Local path keyed on the original hash (dedup across messages) when present, else
    // the nonce (unique per send). The basis is author-controlled, so require it to be a
    // bounded hex string before joining it into a filesystem path — a hostile member can't
    // smuggle `../` traversal into the persisted `path` (defense-in-depth: `open_attachment`
    // also re-checks the path is inside the download dir).
    let basis = original_hash.clone().unwrap_or_else(|| nonce.clone());
    if basis.is_empty() || basis.len() > 128 || !basis.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let path = download_dir.join(format!("{}.{}", basis, extension));
    let downloaded = path.exists();

    // Bounded sanity on the author-controlled topic: base32 alphabet only, 32-byte
    // payload (52 chars). Anything else is dropped, not propagated to the realtime layer.
    let webxdc_topic = field(body, "webxdc-topic")
        .filter(|t| t.len() == 52 && t.bytes().all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b)))
        .map(|t| t.to_string());

    Some(Attachment {
        id: basis,
        key,
        nonce,
        extension,
        name,
        url,
        path: path.to_string_lossy().to_string(),
        size,
        img_meta,
        downloading: false,
        downloaded,
        webxdc_topic,
        group_id: None, // Community attachments use explicit key/nonce (NIP-17 technique).
        original_hash,
        scheme_version: None,
        mls_filename: None,
    })
}

/// Parse every `imeta` tag on an event into attachments, order preserved.
pub fn attachments_from_tags<'a>(
    tags: impl Iterator<Item = &'a Tag>,
    download_dir: &Path,
) -> Vec<Attachment> {
    tags.filter_map(|t| attachment_from_imeta(t, download_dir)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str, ext: &str, with_img: bool) -> Attachment {
        Attachment {
            id: "h".into(),
            key: "0".repeat(64),  // 32-byte key
            nonce: "1".repeat(32), // 16-byte (0xChat-compatible) nonce
            extension: ext.into(),
            name: name.into(),
            url: "https://blossom.example/abc".into(),
            path: String::new(),
            size: 4096,
            img_meta: with_img.then(|| ImageMetadata { thumbhash: "TH".into(), width: 800, height: 600 }),
            downloading: false,
            downloaded: false,
            webxdc_topic: None,
            group_id: None,
            original_hash: Some("a".repeat(64)),
            scheme_version: None,
            mls_filename: None,
        }
    }

    #[test]
    fn imeta_round_trip_preserves_crypto_and_meta() {
        let dir = std::env::temp_dir();
        let att = sample("my report.png", "png", true);
        let tag = attachment_to_imeta(&att);
        let back = attachment_from_imeta(&tag, &dir).expect("parses");
        assert_eq!(back.url, att.url);
        assert_eq!(back.key, att.key);
        assert_eq!(back.nonce, att.nonce);
        assert_eq!(back.size, att.size);
        assert_eq!(back.original_hash, att.original_hash);
        assert_eq!(back.name, "my report.png"); // space in filename survives
        assert_eq!(back.extension, "png");
        assert_eq!(back.group_id, None);
        let m = back.img_meta.expect("img meta");
        assert_eq!((m.width, m.height), (800, 600));
        assert_eq!(m.thumbhash, "TH");
    }

    #[test]
    fn spoiler_and_renamed_filenames_survive_imeta() {
        // Spoiler is detected receiver-side by a `SPOILER_` prefix on the attachment NAME,
        // so the name (incl. that prefix, and spaces) must round-trip through imeta intact —
        // this is what gives Community attachments spoiler/rename parity with DMs.
        let dir = std::env::temp_dir();
        let spoiler = attachment_from_imeta(&attachment_to_imeta(&sample("SPOILER_big reveal.png", "png", true)), &dir).unwrap();
        assert_eq!(spoiler.name, "SPOILER_big reveal.png");
        assert!(spoiler.name.to_uppercase().starts_with("SPOILER_"), "spoiler prefix preserved");
        assert_eq!(spoiler.extension, "png");

        let renamed = attachment_from_imeta(&attachment_to_imeta(&sample("Quarterly Report (final).pdf", "pdf", false)), &dir).unwrap();
        assert_eq!(renamed.name, "Quarterly Report (final).pdf");
        assert_eq!(renamed.extension, "pdf");
    }

    #[test]
    fn field_key_match_requires_a_following_space_no_prefix_bleed() {
        // `field(_, "m")` must NOT match a longer key like "mime ..." (shared prefix). The
        // "key + ' '" requirement guards this; lock it so future imeta fields can't collide.
        let entries = vec!["mime image/png".to_string(), "m image/jpeg".to_string()];
        assert_eq!(field(&entries, "m"), Some("image/jpeg"));
        assert_eq!(field(&entries, "mime"), Some("image/png"));
        assert_eq!(field(&["decryption-key-x abc".to_string()], "decryption-key"), None);
        // A key present with no value (no following space) yields None, not a panic.
        assert_eq!(field(&["url".to_string()], "url"), None);
    }

    #[test]
    fn multiple_imeta_tags_parse_in_order() {
        let dir = std::env::temp_dir();
        let tags = vec![
            Tag::custom(TagKind::Custom("z".into()), ["pseudonym"]),
            attachment_to_imeta(&sample("a.png", "png", false)),
            Tag::custom(TagKind::Custom("ms".into()), ["12"]),
            attachment_to_imeta(&sample("b.pdf", "pdf", false)),
        ];
        let atts = attachments_from_tags(tags.iter(), &dir);
        assert_eq!(atts.len(), 2);
        assert_eq!(atts[0].name, "a.png");
        assert_eq!(atts[1].name, "b.pdf");
        assert_eq!(atts[1].extension, "pdf");
    }

    #[test]
    fn non_imeta_and_incomplete_tags_are_skipped() {
        let dir = std::env::temp_dir();
        let not_imeta = Tag::custom(TagKind::Custom("e".into()), ["abc"]);
        assert!(attachment_from_imeta(&not_imeta, &dir).is_none());
        // imeta missing decryption fields → None.
        let bad = Tag::custom(TagKind::Custom("imeta".into()), ["url https://x/y"]);
        assert!(attachment_from_imeta(&bad, &dir).is_none());
    }

    #[test]
    fn imeta_crypto_params_actually_decrypt_the_ciphertext() {
        // End-to-end attachment crypto: encrypt a plaintext with the real params, carry the
        // key/nonce via imeta, parse them back out, and confirm they decrypt the ciphertext.
        // This is the receiver's download path in miniature (minus the Blossom fetch).
        let dir = std::env::temp_dir();
        let plaintext = b"the quick brown fox jumps over 13 lazy dogs".to_vec();
        let params = crate::crypto::generate_encryption_params();
        let ciphertext = crate::crypto::encrypt_data(&plaintext, &params).unwrap();

        let att = Attachment {
            id: "x".into(),
            key: params.key.clone(),
            nonce: params.nonce.clone(),
            extension: "txt".into(),
            name: "note.txt".into(),
            url: "https://blossom.example/blob".into(),
            path: String::new(),
            size: ciphertext.len() as u64,
            img_meta: None,
            downloading: false,
            downloaded: false,
            webxdc_topic: None,
            group_id: None,
            original_hash: Some("c".repeat(64)),
            scheme_version: None,
            mls_filename: None,
        };
        let parsed = attachment_from_imeta(&attachment_to_imeta(&att), &dir).expect("parses");
        // The parsed key/nonce (straight off the imeta) must decrypt the ciphertext.
        let decrypted = crate::crypto::decrypt_data(&ciphertext, &parsed.key, &parsed.nonce)
            .expect("decrypts with imeta-carried params");
        assert_eq!(decrypted, plaintext, "round-trip plaintext matches");
    }

    #[test]
    fn hostile_path_basis_is_rejected() {
        // A channel member authors the imeta, so the path basis (`ox`, else `nonce`) is
        // attacker-controlled. A non-hex / traversal basis must be refused, never joined
        // into a filesystem path.
        let dir = std::path::Path::new("/tmp/vector-test-dl");
        let traversal = Tag::custom(TagKind::Custom("imeta".into()), [
            "url https://x/y",
            "decryption-key 00",
            "decryption-nonce 11",
            "ox ../../../../etc/passwd",
        ]);
        assert!(attachment_from_imeta(&traversal, dir).is_none(), "traversal ox rejected");

        // Falls back to nonce when ox absent — a non-hex nonce is likewise rejected.
        let bad_nonce = Tag::custom(TagKind::Custom("imeta".into()), [
            "url https://x/y",
            "decryption-key 00",
            "decryption-nonce ../evil",
        ]);
        assert!(attachment_from_imeta(&bad_nonce, dir).is_none(), "traversal nonce rejected");

        // A legitimate hex basis still parses.
        let good = Tag::custom(TagKind::Custom("imeta".into()), [
            "url https://x/y".to_string(),
            "decryption-key 00".to_string(),
            "decryption-nonce 11".to_string(),
            format!("ox {}", "a".repeat(64)),
        ]);
        assert!(attachment_from_imeta(&good, dir).is_some(), "hex ox accepted");
    }

    #[test]
    fn webxdc_topic_round_trips_imeta_and_garbage_is_dropped() {
        let dir = std::env::temp_dir();
        let topic = crate::webxdc::mint_topic_id("hash", "sender");
        let mut att = sample("game.xdc", "xdc", false);
        att.webxdc_topic = Some(topic.clone());
        let back = attachment_from_imeta(&attachment_to_imeta(&att), &dir).expect("parses");
        assert_eq!(back.webxdc_topic.as_deref(), Some(topic.as_str()));

        // Author-controlled: wrong-length / off-alphabet topics are dropped, not propagated.
        for bad in ["short", &"A".repeat(53), &"a".repeat(52), &format!("{}!", "A".repeat(51))] {
            let mut att = sample("game.xdc", "xdc", false);
            att.webxdc_topic = Some(bad.to_string());
            let back = attachment_from_imeta(&attachment_to_imeta(&att), &dir).expect("parses");
            assert_eq!(back.webxdc_topic, None, "bad topic {:?} must be dropped", bad);
        }
    }

    #[test]
    fn malformed_imeta_does_not_panic_and_drops_gracefully() {
        let dir = std::env::temp_dir();
        // Garbage entries, duplicate keys, value-less keys, weird spacing — must not panic.
        let junk = Tag::custom(TagKind::Custom("imeta".into()), [
            "url",                 // no value
            "decryption-key",      // no value
            "random noise here",
            "  ",
            "url https://x/legit", // a later valid url
        ]);
        // Missing decryption-key/nonce → None (not a panic).
        assert!(attachment_from_imeta(&junk, &dir).is_none());

        // Empty imeta (just the tag name) → None.
        let empty = Tag::custom(TagKind::Custom("imeta".into()), Vec::<String>::new());
        assert!(attachment_from_imeta(&empty, &dir).is_none());
    }
}
