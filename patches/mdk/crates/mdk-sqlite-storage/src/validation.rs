//! Input validation constants and utilities for SQLite storage.
//!
//! These limits prevent unbounded user input from causing disk and CPU exhaustion.

use crate::error::Error;

/// Maximum size for message content (1 MB)
pub const MAX_MESSAGE_CONTENT_SIZE: usize = 1024 * 1024;

/// Maximum size for serialized tags JSON (100 KB)
pub const MAX_TAGS_JSON_SIZE: usize = 100 * 1024;

/// Maximum size for serialized event JSON (100 KB)
pub const MAX_EVENT_JSON_SIZE: usize = 100 * 1024;

/// Maximum length for group name (255 bytes, UTF-8 encoded)
pub const MAX_GROUP_NAME_LENGTH: usize = 255;

/// Maximum length for group description (2000 bytes, UTF-8 encoded)
pub const MAX_GROUP_DESCRIPTION_LENGTH: usize = 2000;

/// Maximum size for serialized admin pubkeys JSON (50 KB)
pub const MAX_ADMIN_PUBKEYS_JSON_SIZE: usize = 50 * 1024;

/// Maximum size for serialized group relays JSON (50 KB)
pub const MAX_GROUP_RELAYS_JSON_SIZE: usize = 50 * 1024;

/// Validate that a byte slice does not exceed the specified maximum size.
#[inline]
pub fn validate_size(data: &[u8], max_size: usize, field_name: &str) -> Result<(), Error> {
    if data.len() > max_size {
        return Err(Error::Validation {
            field_name: field_name.to_string(),
            max_size,
            actual_size: data.len(),
        });
    }
    Ok(())
}

/// Validate that a string does not exceed the specified maximum length in bytes.
///
/// Note: This validates UTF-8 byte length, not Unicode character count.
/// Multi-byte characters (e.g., emoji) will count as multiple bytes.
#[inline]
pub fn validate_string_length(s: &str, max_length: usize, field_name: &str) -> Result<(), Error> {
    if s.len() > max_length {
        return Err(Error::Validation {
            field_name: field_name.to_string(),
            max_size: max_length,
            actual_size: s.len(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_size_within_limit() {
        let data = vec![0u8; 100];
        let result = validate_size(&data, 200, "test_field");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_size_at_limit() {
        let data = vec![0u8; 100];
        let result = validate_size(&data, 100, "test_field");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_size_exceeds_limit() {
        let data = vec![0u8; 150];
        let result = validate_size(&data, 100, "test_field");
        assert!(result.is_err());
        match result {
            Err(Error::Validation {
                field_name,
                max_size,
                actual_size,
            }) => {
                assert_eq!(field_name, "test_field");
                assert_eq!(max_size, 100);
                assert_eq!(actual_size, 150);
            }
            _ => panic!("Expected Validation error"),
        }
    }

    #[test]
    fn test_validate_size_empty() {
        let data = vec![];
        let result = validate_size(&data, 100, "test_field");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_string_length_within_limit() {
        let s = "hello";
        let result = validate_string_length(s, 10, "test_field");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_string_length_at_limit() {
        let s = "hello";
        let result = validate_string_length(s, 5, "test_field");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_string_length_exceeds_limit() {
        let s = "hello world";
        let result = validate_string_length(s, 5, "test_field");
        assert!(result.is_err());
        match result {
            Err(Error::Validation {
                field_name,
                max_size,
                actual_size,
            }) => {
                assert_eq!(field_name, "test_field");
                assert_eq!(max_size, 5);
                assert_eq!(actual_size, 11);
            }
            _ => panic!("Expected Validation error"),
        }
    }

    #[test]
    fn test_validate_string_length_empty() {
        let s = "";
        let result = validate_string_length(s, 100, "test_field");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_string_length_unicode_multibyte() {
        // "世界" is 6 bytes in UTF-8 (3 bytes per character)
        let s = "世界";
        let result = validate_string_length(s, 6, "test_field");
        assert!(result.is_ok());

        // Should fail if limit is less than 6 bytes
        let result = validate_string_length(s, 5, "test_field");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_string_length_emoji() {
        // Emoji are typically 4 bytes in UTF-8
        let s = "🎉";
        let result = validate_string_length(s, 4, "test_field");
        assert!(result.is_ok());

        // Should fail if limit is less than 4 bytes
        let result = validate_string_length(s, 3, "test_field");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_size_large_data() {
        let data = vec![0u8; MAX_MESSAGE_CONTENT_SIZE];
        let result = validate_size(&data, MAX_MESSAGE_CONTENT_SIZE, "message_content");
        assert!(result.is_ok());

        // Should fail if exceeds limit
        let data = vec![0u8; MAX_MESSAGE_CONTENT_SIZE + 1];
        let result = validate_size(&data, MAX_MESSAGE_CONTENT_SIZE, "message_content");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_string_length_max_group_name() {
        // Create a string exactly at the limit
        let s = "a".repeat(MAX_GROUP_NAME_LENGTH);
        let result = validate_string_length(&s, MAX_GROUP_NAME_LENGTH, "group_name");
        assert!(result.is_ok());

        // Should fail if exceeds limit
        let s = "a".repeat(MAX_GROUP_NAME_LENGTH + 1);
        let result = validate_string_length(&s, MAX_GROUP_NAME_LENGTH, "group_name");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_string_length_max_group_description() {
        // Create a string exactly at the limit
        let s = "a".repeat(MAX_GROUP_DESCRIPTION_LENGTH);
        let result = validate_string_length(&s, MAX_GROUP_DESCRIPTION_LENGTH, "group_description");
        assert!(result.is_ok());

        // Should fail if exceeds limit
        let s = "a".repeat(MAX_GROUP_DESCRIPTION_LENGTH + 1);
        let result = validate_string_length(&s, MAX_GROUP_DESCRIPTION_LENGTH, "group_description");
        assert!(result.is_err());
    }
}
