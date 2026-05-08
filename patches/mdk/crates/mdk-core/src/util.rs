use mdk_storage_traits::groups::types::GroupExporterSecret;
use nostr::base64::Engine;
use nostr::base64::engine::general_purpose::STANDARD as BASE64;
use nostr::nips::nip44;
use nostr::{Keys, SecretKey};
use openmls::prelude::{Ciphersuite, ExtensionType};

use crate::Error;

/// Trait for formatting MLS types as Nostr tag values
///
/// This trait provides a consistent way to format MLS types (Ciphersuite, ExtensionType)
/// as hex strings for use in Nostr tags. The format is always "0x" followed by 4 lowercase
/// hex digits.
pub(crate) trait NostrTagFormat {
    /// Convert to Nostr tag hex format (e.g., "0x0001")
    fn to_nostr_tag(&self) -> String;
}

impl NostrTagFormat for Ciphersuite {
    fn to_nostr_tag(&self) -> String {
        format!("0x{:04x}", u16::from(*self))
    }
}

impl NostrTagFormat for ExtensionType {
    fn to_nostr_tag(&self) -> String {
        format!("0x{:04x}", u16::from(*self))
    }
}

pub(crate) fn decrypt_with_exporter_secret(
    secret: &GroupExporterSecret,
    encrypted_content: &str,
) -> Result<Vec<u8>, Error> {
    // Convert that secret to nostr keys
    let secret_key: SecretKey = SecretKey::from_slice(secret.secret.as_ref())?;
    let export_nostr_keys = Keys::new(secret_key);

    // Decrypt message
    let message_bytes: Vec<u8> = nip44::decrypt_to_bytes(
        export_nostr_keys.secret_key(),
        &export_nostr_keys.public_key,
        encrypted_content,
    )?;

    Ok(message_bytes)
}

/// Encoding format for content fields
///
/// Only base64 encoding is supported per MIP-00/MIP-02.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContentEncoding {
    /// Base64 encoding
    #[default]
    Base64,
}

impl ContentEncoding {
    /// Returns the tag value for this encoding format
    pub fn as_tag_value(&self) -> &'static str {
        match self {
            ContentEncoding::Base64 => "base64",
        }
    }

    /// Parse encoding from tag value
    pub fn from_tag_value(value: &str) -> Option<Self> {
        match value.to_lowercase().as_str() {
            "base64" => Some(ContentEncoding::Base64),
            _ => None,
        }
    }

    /// Extracts the encoding format from an iterator of tags.
    ///
    /// Looks for an `["encoding", "base64"]` tag.
    ///
    /// # Arguments
    ///
    /// * `tags` - An iterator over tags (works with both Event and UnsignedEvent)
    ///
    /// # Returns
    ///
    /// The ContentEncoding specified by the tag, or None if no tag present or invalid encoding.
    /// Callers must handle None and reject events without valid encoding tags.
    pub fn from_tags<'a>(tags: impl Iterator<Item = &'a nostr::Tag>) -> Option<Self> {
        for tag in tags {
            let slice = tag.as_slice();
            if slice.len() >= 2
                && slice[0] == "encoding"
                && let Some(encoding) = Self::from_tag_value(&slice[1])
            {
                return Some(encoding);
            }
        }
        // SECURITY: No default - encoding tag must be present per MIP-00/MIP-02
        None
    }
}

/// Encodes content using base64 encoding
///
/// # Arguments
///
/// * `bytes` - The bytes to encode
/// * `encoding` - The encoding format (must be Base64)
///
/// # Returns
///
/// The base64-encoded string
pub(crate) fn encode_content(bytes: &[u8], encoding: ContentEncoding) -> String {
    match encoding {
        ContentEncoding::Base64 => BASE64.encode(bytes),
    }
}

/// Decodes content using base64 encoding
///
/// The encoding format must be determined from the `["encoding", "base64"]` tag on the event.
///
/// Per MIP-00/MIP-02, the encoding tag is required. Callers must extract the encoding
/// using `ContentEncoding::from_tags()` and handle the None case by rejecting the event.
///
/// # Arguments
///
/// * `content` - The base64-encoded string
/// * `encoding` - The encoding format (must be Base64)
/// * `label` - A label for the content type (e.g., "key package", "welcome") used in error messages
///
/// # Returns
///
/// A tuple of (decoded bytes, format description) on success, or an error message string.
pub(crate) fn decode_content(
    content: &str,
    encoding: ContentEncoding,
    label: &str,
) -> Result<(Vec<u8>, &'static str), String> {
    match encoding {
        ContentEncoding::Base64 => BASE64
            .decode(content)
            .map(|bytes| (bytes, "base64"))
            .map_err(|e| format!("Failed to decode {} as base64: {}", label, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::Tag;

    #[test]
    fn test_encode_decode_roundtrip() {
        let original = vec![0xde, 0xad, 0xbe, 0xef];

        // Base64 roundtrip
        let b64_encoded = encode_content(&original, ContentEncoding::Base64);
        let (b64_decoded, b64_fmt) =
            decode_content(&b64_encoded, ContentEncoding::Base64, "test").unwrap();
        assert_eq!(original, b64_decoded);
        assert_eq!(b64_fmt, "base64");
    }

    #[test]
    fn test_decode_invalid_content() {
        assert!(decode_content("!!!", ContentEncoding::Base64, "test").is_err());
    }

    #[test]
    fn test_content_encoding_tag_value_roundtrip() {
        assert_eq!(
            ContentEncoding::from_tag_value(ContentEncoding::Base64.as_tag_value()),
            Some(ContentEncoding::Base64)
        );
        assert_eq!(ContentEncoding::from_tag_value("invalid"), None);
        assert_eq!(ContentEncoding::from_tag_value("hex"), None);
    }

    #[test]
    fn test_from_tags_returns_encoding() {
        let tags_base64 = [Tag::custom(
            nostr::TagKind::Custom("encoding".into()),
            ["base64"],
        )];
        assert_eq!(
            ContentEncoding::from_tags(tags_base64.iter()),
            Some(ContentEncoding::Base64)
        );

        let tags_hex = [Tag::custom(
            nostr::TagKind::Custom("encoding".into()),
            ["hex"],
        )];
        assert_eq!(ContentEncoding::from_tags(tags_hex.iter()), None);

        let empty: [Tag; 0] = [];
        assert_eq!(ContentEncoding::from_tags(empty.iter()), None);
    }
}
