//! Cryptographic operations for encrypted media
//!
//! This module handles all encryption and decryption operations for media files,
//! including key derivation, nonce generation, and ChaCha20-Poly1305 AEAD operations
//! according to the Marmot protocol specification.

use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit},
};
use hkdf::Hkdf;
use nostr::secp256k1::rand::{RngCore, rngs::OsRng};
use sha2::Sha256;

use mdk_storage_traits::{MdkStorageProvider, Secret};

use crate::encrypted_media::types::EncryptedMediaError;
use crate::{GroupId, MDK};

/// Default scheme version for MIP-04 encryption
pub const DEFAULT_SCHEME_VERSION: &str = "mip04-v2";

/// Check if a scheme version is supported for decryption
///
/// This function determines if the given version string corresponds to a supported
/// encryption scheme. Currently, "mip04-v2" is the standard supported version.
/// "mip04-v1" is NOT supported due to security vulnerabilities.
pub fn is_scheme_version_supported(version: &str) -> bool {
    match version {
        "mip04-v2" => true,
        // mip04-v1 is explicitly unsupported
        // Future versions can be added here
        _ => false,
    }
}

/// Get scheme label bytes from version string
///
/// This function maps version strings to their corresponding scheme labels
/// used in AAD and HKDF contexts. This allows for versioned encryption
/// schemes while maintaining backward compatibility.
fn get_scheme_label(version: &str) -> Result<&[u8], EncryptedMediaError> {
    match version {
        "mip04-v2" => Ok(b"mip04-v2"),
        // Future versions can be added here
        _ => Err(EncryptedMediaError::UnknownSchemeVersion(
            version.to_string(),
        )),
    }
}

/// Build HKDF context for key/nonce derivation with scheme label for domain separation
fn build_hkdf_context(
    scheme_label: &[u8],
    file_hash: &[u8; 32],
    mime_type: &str,
    filename: &str,
    suffix: &[u8],
) -> Vec<u8> {
    let mut context = Vec::new();
    context.extend_from_slice(scheme_label);
    context.push(0x00);
    context.extend_from_slice(file_hash);
    context.push(0x00);
    context.extend_from_slice(mime_type.as_bytes());
    context.push(0x00);
    context.extend_from_slice(filename.as_bytes());
    context.push(0x00);
    context.extend_from_slice(suffix);
    context
}

/// Build AAD (Associated Authenticated Data) for AEAD encryption with scheme label
fn build_aad(
    scheme_label: &[u8],
    file_hash: &[u8; 32],
    mime_type: &str,
    filename: &str,
) -> Vec<u8> {
    let mut aad = Vec::new();
    aad.extend_from_slice(scheme_label);
    aad.push(0x00);
    aad.extend_from_slice(file_hash);
    aad.push(0x00);
    aad.extend_from_slice(mime_type.as_bytes());
    aad.push(0x00);
    aad.extend_from_slice(filename.as_bytes());
    aad
}

/// Derive encryption key from the current epoch's MLS group secret
///
/// Looks up the current epoch's exporter secret and derives the encryption
/// key according to the Marmot protocol 04.md specification:
/// file_key = HKDF-Expand(exporter_secret, SCHEME_LABEL || 0x00 || file_hash_bytes || 0x00 || mime_type_bytes || 0x00 || filename_bytes || 0x00 || "key", 32)
pub fn derive_encryption_key<Storage>(
    mdk: &MDK<Storage>,
    group_id: &GroupId,
    scheme_version: &str,
    original_hash: &[u8; 32],
    mime_type: &str,
    filename: &str,
) -> Result<Secret<[u8; 32]>, EncryptedMediaError>
where
    Storage: MdkStorageProvider,
{
    let exporter_secret = mdk
        .exporter_secret(group_id)
        .map_err(|_| EncryptedMediaError::GroupNotFound)?;

    derive_encryption_key_with_secret(
        &exporter_secret.secret,
        scheme_version,
        original_hash,
        mime_type,
        filename,
    )
}

/// Derive encryption key from an explicit exporter secret
///
/// This variant accepts a raw exporter secret instead of looking it up.
/// Used by the epoch fallback logic in media decryption, which needs to try
/// multiple historical epoch secrets when the current epoch's key doesn't work.
pub(crate) fn derive_encryption_key_with_secret(
    exporter_secret: &Secret<[u8; 32]>,
    scheme_version: &str,
    original_hash: &[u8; 32],
    mime_type: &str,
    filename: &str,
) -> Result<Secret<[u8; 32]>, EncryptedMediaError> {
    let scheme_label = get_scheme_label(scheme_version)?;
    let context = build_hkdf_context(scheme_label, original_hash, mime_type, filename, b"key");

    let hk = Hkdf::<Sha256>::new(None, exporter_secret.as_ref());
    let mut key = [0u8; 32];
    hk.expand(&context, &mut key)
        .map_err(|e| EncryptedMediaError::EncryptionFailed {
            reason: format!("Key derivation failed: {}", e),
        })?;

    Ok(Secret::new(key))
}

/// Generate a random encryption nonce
///
/// This function generates a cryptographically secure random 96-bit (12-byte) nonce
/// for ChaCha20-Poly1305 encryption. The nonce must be stored with the encrypted data
/// (e.g., in the IMETA tag) and provided during decryption.
pub fn generate_encryption_nonce() -> Secret<[u8; 12]> {
    let mut nonce = [0u8; 12];
    let mut rng = OsRng;
    rng.fill_bytes(&mut nonce);
    Secret::new(nonce)
}

/// Encrypt data using ChaCha20-Poly1305 AEAD with Associated Authenticated Data
///
/// As specified in MIP-04, the AAD includes:
/// aad = SCHEME_LABEL || 0x00 || file_hash_bytes || 0x00 || mime_type_bytes || 0x00 || filename_bytes
pub fn encrypt_data_with_aad(
    data: &[u8],
    key: &Secret<[u8; 32]>,
    nonce: &Secret<[u8; 12]>,
    scheme_version: &str,
    file_hash: &[u8; 32],
    mime_type: &str,
    filename: &str,
) -> Result<Vec<u8>, EncryptedMediaError> {
    let cipher = ChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|e| {
        EncryptedMediaError::EncryptionFailed {
            reason: format!("Failed to create cipher: {}", e),
        }
    })?;

    let nonce_arr = Nonce::from_slice(nonce.as_ref());

    let scheme_label = get_scheme_label(scheme_version)?;
    let aad = build_aad(scheme_label, file_hash, mime_type, filename);

    cipher
        .encrypt(
            nonce_arr,
            chacha20poly1305::aead::Payload {
                msg: data,
                aad: &aad,
            },
        )
        .map_err(|e| EncryptedMediaError::EncryptionFailed {
            reason: format!("Encryption failed: {}", e),
        })
}

/// Decrypt data using ChaCha20-Poly1305 AEAD with Associated Authenticated Data
///
/// As specified in MIP-04, the AAD includes:
/// aad = SCHEME_LABEL || 0x00 || file_hash_bytes || 0x00 || mime_type_bytes || 0x00 || filename_bytes
///
/// This function attempts decryption with the provided scheme version. If decryption
/// fails, it may be due to a version mismatch. The caller should ensure the correct
/// scheme_version is provided from the MediaReference parsed from the IMETA tag.
pub fn decrypt_data_with_aad(
    encrypted_data: &[u8],
    key: &Secret<[u8; 32]>,
    nonce: &Secret<[u8; 12]>,
    scheme_version: &str,
    file_hash: &[u8; 32],
    mime_type: &str,
    filename: &str,
) -> Result<Vec<u8>, EncryptedMediaError> {
    let cipher = ChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|e| {
        EncryptedMediaError::DecryptionFailed {
            reason: format!("Failed to create cipher: {}", e),
        }
    })?;

    let nonce_arr = Nonce::from_slice(nonce.as_ref());

    let scheme_label = get_scheme_label(scheme_version)?;
    let aad = build_aad(scheme_label, file_hash, mime_type, filename);

    cipher
        .decrypt(
            nonce_arr,
            chacha20poly1305::aead::Payload {
                msg: encrypted_data,
                aad: &aad,
            },
        )
        .map_err(|e| EncryptedMediaError::DecryptionFailed {
            reason: format!("Decryption failed: {}", e),
        })
}

#[cfg(test)]
mod tests {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    use sha2::Digest;

    use mdk_memory_storage::MdkMemoryStorage;

    use super::*;

    fn create_test_mdk() -> MDK<MdkMemoryStorage> {
        MDK::new(MdkMemoryStorage::default())
    }

    #[test]
    fn test_errors_without_group() {
        let mdk = create_test_mdk();
        let group_id = GroupId::from_slice(&[1, 2, 3, 4]);

        let original_data =
            b"This is test image data that should be encrypted and decrypted properly";
        let mime_type = "image/jpeg";
        let filename = "test.jpg";

        let original_hash: [u8; 32] = Sha256::digest(original_data).into();

        // Test key derivation (will fail without a proper group, but we can test the logic)
        let key_result = derive_encryption_key(
            &mdk,
            &group_id,
            DEFAULT_SCHEME_VERSION,
            &original_hash,
            mime_type,
            filename,
        );

        // Should fail gracefully since we don't have a real MLS group
        assert!(key_result.is_err());

        // Verify the error is the expected "GroupNotFound" error
        if let Err(EncryptedMediaError::GroupNotFound) = key_result {
            // Expected behavior
        } else {
            panic!("Expected GroupNotFound error for key derivation");
        }
    }

    #[test]
    fn test_encrypt_decrypt_with_known_key() {
        // Test encryption/decryption with a known key and nonce
        let key = Secret::new([0x42u8; 32]);
        let nonce = Secret::new([0x24u8; 12]);
        let original_data = b"Hello, encrypted world!";
        let file_hash = [0x01u8; 32];
        let mime_type = "image/jpeg";
        let filename = "test.jpg";

        // Encrypt the data
        let encrypted_result = encrypt_data_with_aad(
            original_data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        );
        assert!(encrypted_result.is_ok());
        let encrypted_data = encrypted_result.unwrap();

        // Verify encrypted data is different from original
        assert_ne!(encrypted_data.as_slice(), original_data);
        assert!(encrypted_data.len() > original_data.len()); // Should include auth tag

        // Decrypt the data
        let decrypted_result = decrypt_data_with_aad(
            &encrypted_data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        );
        assert!(decrypted_result.is_ok());
        let decrypted_data = decrypted_result.unwrap();

        // Verify decrypted data matches original
        assert_eq!(decrypted_data.as_slice(), original_data);
    }

    #[test]
    fn test_encrypt_decrypt_with_different_aad() {
        // Test that changing AAD components causes decryption to fail
        let key = Secret::new([0x42u8; 32]);
        let nonce = Secret::new([0x24u8; 12]);
        let original_data = b"Hello, encrypted world!";
        let file_hash = [0x01u8; 32];
        let mime_type = "image/jpeg";
        let filename = "test.jpg";

        // Encrypt with original parameters
        let encrypted_data = encrypt_data_with_aad(
            original_data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        )
        .unwrap();

        // Try to decrypt with different file hash (should fail)
        let different_hash = [0x02u8; 32];
        let result = decrypt_data_with_aad(
            &encrypted_data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &different_hash,
            mime_type,
            filename,
        );
        assert!(result.is_err());

        // Try to decrypt with different MIME type (should fail)
        let result = decrypt_data_with_aad(
            &encrypted_data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            "image/png",
            filename,
        );
        assert!(result.is_err());

        // Try to decrypt with different filename (should fail)
        let result = decrypt_data_with_aad(
            &encrypted_data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            "different.jpg",
        );
        assert!(result.is_err());

        // Decrypt with correct parameters (should succeed)
        let result = decrypt_data_with_aad(
            &encrypted_data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_slice(), original_data);
    }

    #[test]
    fn test_encrypt_decrypt_with_wrong_key() {
        // Test that using wrong key causes decryption to fail
        let key = Secret::new([0x42u8; 32]);
        let wrong_key = Secret::new([0x43u8; 32]);
        let nonce = Secret::new([0x24u8; 12]);
        let original_data = b"Hello, encrypted world!";
        let file_hash = [0x01u8; 32];
        let mime_type = "image/jpeg";
        let filename = "test.jpg";

        // Encrypt with original key
        let encrypted_data = encrypt_data_with_aad(
            original_data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        )
        .unwrap();

        // Try to decrypt with wrong key (should fail)
        let result = decrypt_data_with_aad(
            &encrypted_data,
            &wrong_key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        );
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(EncryptedMediaError::DecryptionFailed { .. })
        ));
    }

    #[test]
    fn test_encrypt_decrypt_with_wrong_nonce() {
        // Test that using wrong nonce causes decryption to fail
        let key = Secret::new([0x42u8; 32]);
        let nonce = Secret::new([0x24u8; 12]);
        let wrong_nonce = Secret::new([0x25u8; 12]);
        let original_data = b"Hello, encrypted world!";
        let file_hash = [0x01u8; 32];
        let mime_type = "image/jpeg";
        let filename = "test.jpg";

        // Encrypt with original nonce
        let encrypted_data = encrypt_data_with_aad(
            original_data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        )
        .unwrap();

        // Try to decrypt with wrong nonce (should fail)
        let result = decrypt_data_with_aad(
            &encrypted_data,
            &key,
            &wrong_nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        );
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(EncryptedMediaError::DecryptionFailed { .. })
        ));
    }

    #[test]
    fn test_encrypt_empty_data() {
        // Test encryption of empty data
        let key = Secret::new([0x42u8; 32]);
        let nonce = Secret::new([0x24u8; 12]);
        let empty_data = b"";
        let file_hash = [0x01u8; 32];
        let mime_type = "image/jpeg";
        let filename = "empty.jpg";

        // Encrypt empty data
        let encrypted_result = encrypt_data_with_aad(
            empty_data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        );
        assert!(encrypted_result.is_ok());
        let encrypted_data = encrypted_result.unwrap();

        // Should still have auth tag even for empty data
        assert!(!encrypted_data.is_empty());

        // Decrypt and verify
        let decrypted_result = decrypt_data_with_aad(
            &encrypted_data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        );
        assert!(decrypted_result.is_ok());
        assert_eq!(decrypted_result.unwrap().as_slice(), empty_data);
    }

    #[test]
    fn test_aad_construction() {
        // Test that AAD is constructed correctly by verifying different components
        // cause different encrypted outputs
        let key = Secret::new([0x42u8; 32]);
        let nonce = Secret::new([0x24u8; 12]);
        let data = b"test data";
        let file_hash = [0x01u8; 32];

        // Encrypt with first set of AAD components
        let encrypted1 = encrypt_data_with_aad(
            data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            "image/jpeg",
            "photo.jpg",
        )
        .unwrap();

        // Encrypt with different MIME type
        let encrypted2 = encrypt_data_with_aad(
            data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            "image/png",
            "photo.jpg",
        )
        .unwrap();

        // Encrypt with different filename
        let encrypted3 = encrypt_data_with_aad(
            data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            "image/jpeg",
            "image.jpg",
        )
        .unwrap();

        // All encrypted outputs should be different due to different AAD
        assert_ne!(encrypted1, encrypted2);
        assert_ne!(encrypted1, encrypted3);
        assert_ne!(encrypted2, encrypted3);
    }

    #[test]
    fn test_secret_accessors() {
        // Test that Secret properly wraps values and can be accessed
        let original_key = [0xAAu8; 32];
        let secret_key = Secret::new(original_key);

        // Verify we can access the secret value
        assert_eq!(secret_key.as_ref(), &original_key);
        assert_eq!(*secret_key, original_key);

        // Test cloning preserves the value
        let cloned = secret_key.clone();
        assert_eq!(*cloned, original_key);
        assert_eq!(*secret_key, original_key);

        // Test mut access
        let mut mut_secret = Secret::new([0xBBu8; 32]);
        *mut_secret.as_mut() = [0xCCu8; 32];
        assert_eq!(*mut_secret, [0xCCu8; 32]);
    }

    #[test]
    fn test_secret_debug_format() {
        // Test that Debug formatting doesn't leak secrets
        let secret_key = Secret::new([0xAAu8; 32]);
        let debug_str = format!("{:?}", secret_key);
        assert_eq!(debug_str, "Secret(***)");
        assert!(!debug_str.contains("AA"));
    }

    #[test]
    fn test_decrypt_corrupted_data() {
        // Test decryption with corrupted encrypted data
        let key = Secret::new([0x42u8; 32]);
        let nonce = Secret::new([0x24u8; 12]);
        let original_data = b"Hello, encrypted world!";
        let file_hash = [0x01u8; 32];
        let mime_type = "image/jpeg";
        let filename = "test.jpg";

        // Encrypt valid data
        let mut encrypted_data = encrypt_data_with_aad(
            original_data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        )
        .unwrap();

        // Corrupt the encrypted data (flip a bit)
        encrypted_data[0] ^= 0xFF;

        // Decryption should fail
        let result = decrypt_data_with_aad(
            &encrypted_data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        );
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(EncryptedMediaError::DecryptionFailed { .. })
        ));
    }

    #[test]
    fn test_scheme_version_mismatch_causes_decryption_failure() {
        // Test that encrypting with one scheme version and decrypting with another fails
        let key = Secret::new([0x42u8; 32]);
        let nonce = Secret::new([0x24u8; 12]);
        let original_data = b"Test data for version mismatch";
        let file_hash = [0x01u8; 32];
        let mime_type = "image/jpeg";
        let filename = "test.jpg";

        // Encrypt with default scheme version
        let encrypted_data = encrypt_data_with_aad(
            original_data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        )
        .unwrap();

        // Try to decrypt with a different scheme version ("mip04-v1")
        // This should produce different AAD and cause decryption failure
        let result = decrypt_data_with_aad(
            &encrypted_data,
            &key,
            &nonce,
            "mip04-v1", // Mismatched version
            &file_hash,
            mime_type,
            filename,
        );
        assert!(result.is_err());
        // mip04-v1 is no longer supported, so we expect UnknownSchemeVersion
        match result {
            Err(EncryptedMediaError::UnknownSchemeVersion(v)) => assert_eq!(v, "mip04-v1"),
            Err(e) => panic!("Expected UnknownSchemeVersion, got {:?}", e),
            Ok(_) => panic!("Should have failed"),
        }
    }

    #[test]
    fn test_decrypt_too_short_data() {
        // Test decryption with data that's too short to be valid
        let key = Secret::new([0x42u8; 32]);
        let nonce = Secret::new([0x24u8; 12]);
        let file_hash = [0x01u8; 32];
        let mime_type = "image/jpeg";
        let filename = "test.jpg";

        // Try to decrypt data that's too short (less than auth tag size)
        let too_short = vec![0u8; 5];

        let result = decrypt_data_with_aad(
            &too_short,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        );
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(EncryptedMediaError::DecryptionFailed { .. })
        ));
    }

    #[test]
    fn test_encrypt_large_data() {
        // Test encryption/decryption of large data
        let key = Secret::new([0x42u8; 32]);
        let nonce = Secret::new([0x24u8; 12]);
        let large_data = vec![0xABu8; 1024 * 1024]; // 1MB
        let file_hash = [0x01u8; 32];
        let mime_type = "application/octet-stream";
        let filename = "large.bin";

        // Encrypt large data
        let encrypted_result = encrypt_data_with_aad(
            &large_data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        );
        assert!(encrypted_result.is_ok());
        let encrypted_data = encrypted_result.unwrap();

        // Verify encrypted data is larger (includes auth tag)
        assert!(encrypted_data.len() > large_data.len());

        // Decrypt and verify
        let decrypted_result = decrypt_data_with_aad(
            &encrypted_data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        );
        assert!(decrypted_result.is_ok());
        assert_eq!(decrypted_result.unwrap(), large_data);
    }

    #[test]
    fn test_encrypt_special_characters() {
        // Test encryption with special characters in filename and MIME type
        let key = Secret::new([0x42u8; 32]);
        let nonce = Secret::new([0x24u8; 12]);
        let data = b"test data";
        let file_hash = [0x01u8; 32];

        // Test with special characters in filename
        let encrypted1 = encrypt_data_with_aad(
            data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            "image/jpeg",
            "test file (1).jpg",
        )
        .unwrap();

        // Test with unicode characters
        let encrypted2 = encrypt_data_with_aad(
            data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            "image/jpeg",
            "тест.jpg",
        )
        .unwrap();

        // Test with complex MIME type
        let encrypted3 = encrypt_data_with_aad(
            data,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "document.docx",
        )
        .unwrap();

        // All should encrypt successfully
        assert!(!encrypted1.is_empty());
        assert!(!encrypted2.is_empty());
        assert!(!encrypted3.is_empty());

        // Verify decryption works
        let decrypted1 = decrypt_data_with_aad(
            &encrypted1,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            "image/jpeg",
            "test file (1).jpg",
        )
        .unwrap();
        assert_eq!(decrypted1, data);

        let decrypted2 = decrypt_data_with_aad(
            &encrypted2,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            "image/jpeg",
            "тест.jpg",
        )
        .unwrap();
        assert_eq!(decrypted2, data);
    }

    #[test]
    fn test_multiple_encryption_cycles() {
        // Test multiple encryption/decryption cycles (fresh nonce per encryption)
        let key = Secret::new([0x42u8; 32]);
        let file_hash = [0x01u8; 32];
        let mime_type = "image/jpeg";
        let filename = "test.jpg";

        let data1 = b"First encryption";
        let data2 = b"Second encryption";
        let data3 = b"Third encryption";

        let nonce1 = generate_encryption_nonce();
        let nonce2 = generate_encryption_nonce();
        let nonce3 = generate_encryption_nonce();

        let enc1 = encrypt_data_with_aad(
            data1,
            &key,
            &nonce1,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        )
        .unwrap();
        let enc2 = encrypt_data_with_aad(
            data2,
            &key,
            &nonce2,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        )
        .unwrap();
        let enc3 = encrypt_data_with_aad(
            data3,
            &key,
            &nonce3,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        )
        .unwrap();

        // All should decrypt correctly
        let dec1 = decrypt_data_with_aad(
            &enc1,
            &key,
            &nonce1,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        )
        .unwrap();
        let dec2 = decrypt_data_with_aad(
            &enc2,
            &key,
            &nonce2,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        )
        .unwrap();
        let dec3 = decrypt_data_with_aad(
            &enc3,
            &key,
            &nonce3,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            mime_type,
            filename,
        )
        .unwrap();

        assert_eq!(dec1, data1);
        assert_eq!(dec2, data2);
        assert_eq!(dec3, data3);
    }

    #[test]
    fn test_error_messages() {
        // Test that error messages are properly formatted
        let mdk = create_test_mdk();
        let group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let file_hash = [0x01u8; 32];

        // Test GroupNotFound error message
        let key_result = derive_encryption_key(
            &mdk,
            &group_id,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            "image/jpeg",
            "test.jpg",
        );
        assert!(matches!(
            key_result,
            Err(EncryptedMediaError::GroupNotFound)
        ));

        // Test DecryptionFailed error message format
        let key = Secret::new([0x42u8; 32]);
        let nonce = Secret::new([0x24u8; 12]);
        let corrupted = vec![0u8; 10];

        let result = decrypt_data_with_aad(
            &corrupted,
            &key,
            &nonce,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            "image/jpeg",
            "test.jpg",
        );
        match result {
            Err(EncryptedMediaError::DecryptionFailed { reason }) => {
                assert!(!reason.is_empty());
                assert!(reason.contains("Decryption failed"));
            }
            other => panic!("Expected DecryptionFailed, got {:?}", other),
        }
    }

    #[test]
    fn test_secret_access_methods() {
        // Test all Secret access methods (as_ref, as_mut, deref, deref_mut)
        let mut secret_key = Secret::new([0xAAu8; 32]);
        let original = [0xAAu8; 32];

        // Test as_ref
        assert_eq!(secret_key.as_ref(), &original);

        // Test deref
        assert_eq!(*secret_key, original);

        // Test as_mut
        *secret_key.as_mut() = [0xBBu8; 32];
        assert_eq!(*secret_key, [0xBBu8; 32]);

        // Test deref_mut
        *secret_key = [0xCCu8; 32];
        assert_eq!(*secret_key, [0xCCu8; 32]);
    }

    #[test]
    fn test_secret_equality() {
        // Test Secret equality and hashing
        let secret1 = Secret::new([0xAAu8; 32]);
        let secret2 = Secret::new([0xAAu8; 32]);
        let secret3 = Secret::new([0xBBu8; 32]);

        // Equal secrets should be equal
        assert_eq!(secret1, secret2);
        assert_ne!(secret1, secret3);

        // Test hashing (equal secrets should have same hash)
        let mut hasher1 = DefaultHasher::new();
        secret1.hash(&mut hasher1);
        let hash1 = hasher1.finish();

        let mut hasher2 = DefaultHasher::new();
        secret2.hash(&mut hasher2);
        let hash2 = hasher2.finish();

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_key_derivation_error() {
        // Test key derivation error path
        let mdk = create_test_mdk();
        let group_id = GroupId::from_slice(&[1, 2, 3, 4]);
        let file_hash = [0x01u8; 32];

        let result = derive_encryption_key(
            &mdk,
            &group_id,
            DEFAULT_SCHEME_VERSION,
            &file_hash,
            "image/jpeg",
            "test.jpg",
        );
        assert!(result.is_err());
        assert!(matches!(result, Err(EncryptedMediaError::GroupNotFound)));
    }

    #[test]
    fn test_secret_ordering() {
        // Test Secret ordering (PartialOrd, Ord)
        let secret1 = Secret::new([0xAAu8; 32]);
        let secret2 = Secret::new([0xBBu8; 32]);
        let secret3 = Secret::new([0xAAu8; 32]);

        // Test PartialOrd
        assert!(secret1 < secret2);
        assert!(secret2 > secret1);
        assert!(secret1 <= secret3);
        assert!(secret1 >= secret3);
    }

    #[test]
    fn test_unknown_scheme_version() {
        let result = get_scheme_label("unknown-version");
        assert!(result.is_err());
        match result {
            Err(EncryptedMediaError::UnknownSchemeVersion(v)) => assert_eq!(v, "unknown-version"),
            _ => panic!("Expected UnknownSchemeVersion error"),
        }
    }
}
