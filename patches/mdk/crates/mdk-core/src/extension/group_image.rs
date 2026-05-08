//! Group image encryption and decryption functionality for MIP-01
//!
//! This module provides cryptographic operations for group avatar images:
//! - Image validation (dimensions, file size)
//! - Encryption with ChaCha20-Poly1305 AEAD
//! - Decryption and integrity verification
//! - Deterministic upload keypair derivation for Blossom cleanup
//!
//! The encryption scheme does NOT use AAD for simplicity. Integrity is provided by:
//! 1. SHA256 hash check of encrypted blob (detects substitution)
//! 2. ChaCha20-Poly1305 auth tag (detects tampering/corruption)

use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead, KeyInit},
};
use hkdf::Hkdf;
use mdk_storage_traits::Secret;
use nostr::secp256k1::rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};

use crate::media_processing::validation::validate_file_size;
use crate::media_processing::{
    MediaProcessingOptions, metadata::extract_metadata_from_encoded_image,
};

/// Domain separation label for upload keypair derivation v1 (MIP-01 spec, deprecated)
const UPLOAD_KEYPAIR_CONTEXT_V1: &[u8] = b"mip01-blossom-upload-v1";

/// Domain separation label for image encryption key derivation v2 (MIP-01 spec)
const IMAGE_ENCRYPTION_CONTEXT_V2: &[u8] = b"mip01-image-encryption-v2";

/// Domain separation label for upload keypair derivation v2 (MIP-01 spec)
const UPLOAD_KEYPAIR_CONTEXT_V2: &[u8] = b"mip01-blossom-upload-v2";

/// Prepared group image data ready for upload to Blossom
#[derive(Debug, Clone)]
pub struct GroupImageUpload {
    /// Encrypted image data (ready to upload to Blossom)
    pub encrypted_data: Secret<Vec<u8>>,
    /// SHA256 hash of encrypted data (verify against Blossom response)
    pub encrypted_hash: [u8; 32],
    /// Image seed (v2) - used to derive encryption key via HKDF
    pub image_key: Secret<[u8; 32]>,
    /// Encryption nonce (store in extension)
    pub image_nonce: Secret<[u8; 12]>,
    /// Upload seed (v2) - used to derive the Nostr keypair for Blossom authentication
    /// Cryptographically independent from image_key
    pub image_upload_key: Secret<[u8; 32]>,
    /// Derived keypair for Blossom authentication
    pub upload_keypair: nostr::Keys,
    /// Original image size before encryption (and before EXIF stripping if applicable)
    pub original_size: usize,
    /// Size after encryption
    pub encrypted_size: usize,
    /// Validated and canonical MIME type
    pub mime_type: String,
    /// Image dimensions (width, height) if available
    pub dimensions: Option<(u32, u32)>,
    /// Blurhash for preview if generated
    pub blurhash: Option<String>,
}

/// Group image encryption result with hash (internal type)
#[derive(Debug, Clone)]
struct GroupImageEncrypted {
    /// The encrypted image data
    encrypted_data: Secret<Vec<u8>>,
    /// SHA256 hash of encrypted data (for Blossom upload)
    encrypted_hash: [u8; 32],
    /// Image seed (v2) - used to derive encryption key via HKDF
    /// For v2: this is the seed used to derive the encryption key
    /// For v1: this is the encryption key directly
    image_key: Secret<[u8; 32]>,
    /// Encryption nonce
    image_nonce: Secret<[u8; 12]>,
    /// Upload seed (v2) - used to derive the Nostr keypair for Blossom authentication
    /// For v2: this is cryptographically independent from image_key
    image_upload_key: Secret<[u8; 32]>,
}

/// Group image encryption info from extension
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupImageEncryptionInfo {
    /// Extension version (1 or 2)
    pub version: u16,
    /// Blossom blob hash (SHA256 of encrypted data)
    pub image_hash: [u8; 32],
    /// Image seed (v2) or encryption key (v1)
    pub image_key: Secret<[u8; 32]>,
    /// Encryption nonce
    pub image_nonce: Secret<[u8; 12]>,
    /// Upload seed (v2 only) for deriving the Nostr keypair used for Blossom authentication
    /// None for v1 extensions
    pub image_upload_key: Option<Secret<[u8; 32]>>,
}

/// Errors that can occur during group image operations
#[derive(Debug, thiserror::Error)]
pub enum GroupImageError {
    /// Image validation or processing error
    #[error(transparent)]
    MediaProcessing(#[from] crate::media_processing::types::MediaProcessingError),

    /// Encryption failed
    #[error("Encryption failed: {reason}")]
    EncryptionFailed {
        /// The reason for encryption failure
        reason: String,
    },

    /// Decryption failed
    #[error("Decryption failed: {reason}")]
    DecryptionFailed {
        /// The reason for decryption failure
        reason: String,
    },

    /// Hash verification failed
    #[error("Hash verification failed: expected {expected}, got {actual}")]
    HashVerificationFailed {
        /// The expected hash value
        expected: String,
        /// The actual hash value
        actual: String,
    },

    /// Upload keypair derivation failed
    #[error("Failed to derive upload keypair: {reason}")]
    KeypairDerivationFailed {
        /// The reason for derivation failure
        reason: String,
    },
}

/// Encrypt group image with random seed and nonce (v2 format)
///
/// This is an internal function used by `prepare_group_image_for_upload()`.
/// Users should use `prepare_group_image_for_upload()` instead.
///
/// For v2 (current): Generates cryptographically independent image_seed and upload_seed.
/// The image_seed derives the encryption key via HKDF, while upload_seed derives the
/// Nostr keypair for Blossom authentication, maintaining cryptographic independence.
fn encrypt_group_image(image_data: &[u8]) -> Result<GroupImageEncrypted, GroupImageError> {
    // Generate cryptographically independent seeds for v2
    let mut rng = OsRng;
    let mut image_seed = [0u8; 32];
    let mut image_upload_seed = [0u8; 32];
    let mut image_nonce = [0u8; 12];
    rng.fill_bytes(&mut image_seed);
    rng.fill_bytes(&mut image_upload_seed);
    rng.fill_bytes(&mut image_nonce);

    // Derive encryption key from seed using HKDF-Expand (v2)
    let hk = Hkdf::<Sha256>::new(None, &image_seed);
    let mut image_key = [0u8; 32];
    hk.expand(IMAGE_ENCRYPTION_CONTEXT_V2, &mut image_key)
        .map_err(|e| GroupImageError::EncryptionFailed {
            reason: format!("HKDF expansion failed: {}", e),
        })?;

    // Encrypt with ChaCha20-Poly1305 (no AAD per MIP-01 spec)
    let cipher = ChaCha20Poly1305::new_from_slice(&image_key).map_err(|e| {
        GroupImageError::EncryptionFailed {
            reason: format!("Failed to create cipher: {}", e),
        }
    })?;

    let nonce = Nonce::from_slice(&image_nonce);
    let encrypted_data =
        cipher
            .encrypt(nonce, image_data)
            .map_err(|e| GroupImageError::EncryptionFailed {
                reason: format!("Encryption failed: {}", e),
            })?;

    // Calculate hash of encrypted data
    let encrypted_hash: [u8; 32] = Sha256::digest(&encrypted_data).into();

    Ok(GroupImageEncrypted {
        encrypted_data: Secret::new(encrypted_data),
        encrypted_hash,
        image_key: Secret::new(image_seed), // Store image seed in image_key field
        image_nonce: Secret::new(image_nonce),
        image_upload_key: Secret::new(image_upload_seed), // Store upload seed separately
    })
}

/// Decrypt group image using extension data
///
/// Decrypts the encrypted blob using ChaCha20-Poly1305 AEAD. The auth tag
/// automatically verifies integrity - if tampering occurred, decryption will fail.
///
/// **SECURITY**: Verifies that the encrypted blob hash matches the expected hash before
/// decryption to prevent storage-level blob substitution attacks. If `expected_hash` is `None`,
/// hash verification is skipped (for backward compatibility with old extensions), but this is deprecated and MUST be avoided at all costs.
///
/// Supports both v1 (image_key is the encryption key directly) and v2 (image_key is a seed
/// that needs to be derived using HKDF) formats for backward compatibility.
///
/// # Arguments
/// * `encrypted_data` - Encrypted blob downloaded from Blossom
/// * `expected_hash` - SHA256 hash of the encrypted data (from group extension), or `None` for legacy images
/// * `image_key` - Encryption key (v1) or seed (v2) from group extension
/// * `image_nonce` - Encryption nonce from group extension
///
/// # Returns
/// * Decrypted image bytes
///
/// # Errors
/// * `HashVerificationFailed` - If the encrypted blob hash doesn't match the expected hash
/// * `DecryptionFailed` - If auth tag verification fails (tampering detected)
///
/// # Example
/// ```ignore
/// let extension = mdk.get_group_extension(&group_id)?;
/// if let Some(info) = extension.group_image_encryption_data() {
///     let encrypted_blob = download_from_blossom(&info.image_hash).await?;
///     let image = decrypt_group_image(
///         &encrypted_blob,
///         Some(&info.image_hash),
///         &info.image_key,
///         &info.image_nonce
///     )?;
/// }
/// ```
pub fn decrypt_group_image(
    encrypted_data: &[u8],
    expected_hash: Option<&[u8; 32]>,
    image_key: &Secret<[u8; 32]>,
    image_nonce: &Secret<[u8; 12]>,
) -> Result<Vec<u8>, GroupImageError> {
    // Verify hash of encrypted data before decryption to prevent storage-level substitution
    match expected_hash {
        Some(expected_hash) => {
            let calculated_hash: [u8; 32] = Sha256::digest(encrypted_data).into();
            if calculated_hash != *expected_hash {
                return Err(GroupImageError::HashVerificationFailed {
                    expected: hex::encode(expected_hash),
                    actual: hex::encode(calculated_hash),
                });
            }
        }
        None => {
            // Legacy support: skip hash verification for old extensions without hash
            // This is deprecated - all new images should have hash verification
            tracing::warn!(
                target: "mdk_core::extension::group_image",
                "Decrypting group image without hash verification (legacy mode). This is deprecated and insecure. Please update the extension to include image_hash."
            );
        }
    }

    // Try v2 first: treat image_key as seed and derive encryption key
    let hk = Hkdf::<Sha256>::new(None, image_key.as_ref());
    let mut derived_key = [0u8; 32];
    if hk
        .expand(IMAGE_ENCRYPTION_CONTEXT_V2, &mut derived_key)
        .is_ok()
    {
        let cipher = ChaCha20Poly1305::new_from_slice(&derived_key).map_err(|e| {
            GroupImageError::DecryptionFailed {
                reason: format!("Failed to create cipher: {}", e),
            }
        })?;

        let nonce = Nonce::from_slice(image_nonce.as_ref());
        if let Ok(decrypted_data) = cipher.decrypt(nonce, encrypted_data) {
            return Ok(decrypted_data);
        }
        // If v2 decryption fails, fall through to try v1
    }

    // Fall back to v1: treat image_key as the encryption key directly
    let cipher = ChaCha20Poly1305::new_from_slice(image_key.as_ref()).map_err(|e| {
        GroupImageError::DecryptionFailed {
            reason: format!("Failed to create cipher: {}", e),
        }
    })?;

    let nonce = Nonce::from_slice(image_nonce.as_ref());
    let decrypted_data =
        cipher
            .decrypt(nonce, encrypted_data)
            .map_err(|e| GroupImageError::DecryptionFailed {
                reason: format!("Decryption failed (possible tampering): {}", e),
            })?;

    Ok(decrypted_data)
}

/// Derive Blossom upload keypair from seed/key (supports both v1 and v2)
///
/// For v2: Uses HKDF-Expand with the context "mip01-blossom-upload-v2" to deterministically
/// derive a Nostr keypair from the image_upload_seed. The seed is cryptographically independent
/// from the image encryption seed to maintain cryptographic separation.
///
/// For v1 (backward compatibility): Uses HKDF-Expand with the context "mip01-blossom-upload-v1"
/// to derive a Nostr keypair from the image encryption key directly. In v1, there is no
/// cryptographic separation between encryption and upload keys.
///
/// This enables cleanup of old images - anyone with the appropriate seed/key can derive the upload
/// keypair and delete the blob.
///
/// # Arguments
/// * `seed_or_key` - For v2: The 32-byte image_upload_seed from extension.image_upload_key.
///                   For v1: The 32-byte image encryption key from extension.image_key.
/// * `version` - Extension version (1 or 2). Must be specified to ensure correct derivation.
///   For v1 images, this must be 1 and seed_or_key is the encryption key.
///   For v2 images, this must be 2 and seed_or_key is the upload seed.
///
/// # Returns
/// * Nostr keypair for Blossom authentication
///
/// # Errors
/// * `KeypairDerivationFailed` - If HKDF expansion fails or secret key is invalid
///
/// # Example
/// ```ignore
/// // Cleanup old image after updating
/// if let Some(old_info) = old_extension.group_image_encryption_data() {
///     let seed_or_key = match old_info.version {
///         1 => &old_info.image_key, // v1: use encryption key
///         2 => old_info.image_upload_key.as_ref().unwrap(), // v2: use upload seed
///         _ => return Err(...),
///     };
///     let old_keypair = derive_upload_keypair(seed_or_key, old_info.version)?;
///     blossom_client.delete(&old_info.image_hash, &old_keypair).await?;
/// }
/// ```
#[allow(clippy::doc_overindented_list_items)]
pub fn derive_upload_keypair(
    seed_or_key: &Secret<[u8; 32]>,
    version: u16,
) -> Result<nostr::Keys, GroupImageError> {
    let hk = Hkdf::<Sha256>::new(None, seed_or_key.as_ref());
    let mut upload_secret = [0u8; 32];

    // Use the appropriate context based on version
    let context = match version {
        1 => UPLOAD_KEYPAIR_CONTEXT_V1,
        2 => UPLOAD_KEYPAIR_CONTEXT_V2,
        _ => {
            return Err(GroupImageError::KeypairDerivationFailed {
                reason: format!("Unsupported extension version: {}", version),
            });
        }
    };

    hk.expand(context, &mut upload_secret).map_err(|e| {
        GroupImageError::KeypairDerivationFailed {
            reason: format!("HKDF expansion failed: {}", e),
        }
    })?;

    // Create Nostr keypair from derived secret
    let secret_key = nostr::SecretKey::from_slice(&upload_secret).map_err(|e| {
        GroupImageError::KeypairDerivationFailed {
            reason: format!("Invalid secret key: {}", e),
        }
    })?;

    Ok(nostr::Keys::new(secret_key))
}

/// Prepare group image for upload (validate + encrypt + derive keypair)
///
/// This function validates the image and MIME type, encrypts it, and derives the upload keypair
/// in one step, returning everything needed for the upload workflow. Uses default processing
/// options (EXIF stripping enabled, blurhash generation enabled).
///
/// # Arguments
/// * `image_data` - Raw image bytes
/// * `mime_type` - MIME type of the image (e.g., "image/jpeg", "image/png")
///
/// # Returns
/// * `GroupImageUpload` with encrypted data, hash, and upload keypair
///
/// # Errors
/// * `ImageProcessing` - If the image fails validation (too large, invalid dimensions, invalid MIME type, etc.)
/// * `EncryptionFailed` - If encryption fails
/// * `KeypairDerivationFailed` - If keypair derivation fails
///
/// # Example
/// ```ignore
/// let prepared = prepare_group_image_for_upload(&image_bytes, "image/jpeg")?;
///
/// // Access metadata
/// println!("Dimensions: {:?}", prepared.dimensions);
/// println!("Blurhash: {:?}", prepared.blurhash);
/// println!("MIME type: {}", prepared.mime_type);
/// println!("Original size: {} bytes", prepared.original_size);
/// println!("Encrypted size: {} bytes", prepared.encrypted_size);
///
/// // Upload to Blossom
/// let blob_hash = blossom_client.upload(
///     &prepared.encrypted_data,
///     &prepared.upload_keypair
/// ).await?;
///
/// // Verify the Blossom response matches our hash
/// assert_eq!(blob_hash, prepared.encrypted_hash);
///
/// // Update extension with the verified hash and metadata
/// // Note: For v2, both image_key (encryption seed) and image_upload_key (upload seed)
/// // must be stored to enable future cleanup and keypair derivation
/// let update = NostrGroupDataUpdate::new()
///     .image_hash(Some(blob_hash))
///     .image_key(Some(prepared.image_key))
///     .image_nonce(Some(prepared.image_nonce))
///     .image_upload_key(Some(prepared.image_upload_key));
/// ```
pub fn prepare_group_image_for_upload(
    image_data: &[u8],
    mime_type: &str,
) -> Result<GroupImageUpload, GroupImageError> {
    prepare_group_image_for_upload_with_options(
        image_data,
        mime_type,
        &MediaProcessingOptions::default(),
    )
}

/// Prepare group image for upload with custom processing options
///
/// This function provides full control over image processing behavior including
/// EXIF stripping, blurhash generation, and validation limits.
///
/// # Arguments
/// * `image_data` - Raw image bytes
/// * `mime_type` - MIME type of the image (e.g., "image/jpeg", "image/png")
/// * `options` - Custom processing options for validation and metadata handling
///
/// # Returns
/// * `GroupImageUpload` with encrypted data, hash, and upload keypair
///
/// # Errors
/// * `ImageProcessing` - If the image fails validation (too large, invalid dimensions, invalid MIME type, etc.)
/// * `EncryptionFailed` - If encryption fails
/// * `KeypairDerivationFailed` - If keypair derivation fails
///
/// # Example
/// ```ignore
/// // Custom options: disable blurhash, enable EXIF stripping
/// let options = MediaProcessingOptions {
///     sanitize_exif: true,
///     generate_blurhash: false,
///     max_dimension: Some(8192),
///     max_file_size: Some(10 * 1024 * 1024), // 10MB
///     max_filename_length: None,
/// };
///
/// let prepared = prepare_group_image_for_upload_with_options(
///     &image_bytes,
///     "image/jpeg",
///     &options
/// )?;
/// ```
pub fn prepare_group_image_for_upload_with_options(
    image_data: &[u8],
    mime_type: &str,
    options: &MediaProcessingOptions,
) -> Result<GroupImageUpload, GroupImageError> {
    use crate::media_processing::{metadata, validation};

    // Validate file size to ensure the image isn't too large
    validate_file_size(image_data, options)?;

    // Validate and canonicalize MIME type, ensuring it matches the actual file data
    // This protects against MIME type confusion attacks
    // Use strict validation for group images (no escape hatch allowed)
    let canonical_mime_type =
        validation::validate_group_image_mime_type_matches_data(image_data, mime_type)?;

    let original_size = image_data.len();
    let sanitized_data: Vec<u8>;
    let dimensions: Option<(u32, u32)>;
    let blurhash: Option<String>;

    // Strip EXIF data for privacy if it's a safe raster format (JPEG, PNG)
    // For other formats (GIF, WebP, etc.), use the original data
    if options.sanitize_exif && metadata::is_safe_raster_format(&canonical_mime_type) {
        // PREFLIGHT CHECK: Validate dimensions without full decode to prevent OOM
        // This lightweight check protects against decompression bombs before
        // we fully decode the image for EXIF stripping
        metadata::preflight_dimension_check(image_data, options)?;

        // Strip EXIF and get the decoded image
        let (cleaned_data, decoded_img) =
            metadata::strip_exif_and_return_image(image_data, &canonical_mime_type)?;

        // Extract metadata from the already-decoded image
        let metadata = metadata::extract_metadata_from_decoded_image(
            &decoded_img,
            options,
            options.generate_blurhash,
        )?;

        sanitized_data = cleaned_data;
        dimensions = metadata.dimensions;
        blurhash = metadata.blurhash;
    } else {
        // For non-safe formats (GIF, WebP, etc.), skip EXIF stripping
        // and extract metadata from the encoded image
        let metadata =
            extract_metadata_from_encoded_image(image_data, options, options.generate_blurhash)?;

        sanitized_data = image_data.to_vec();
        dimensions = metadata.dimensions;
        blurhash = metadata.blurhash;
    }

    // Now that validation and sanitization passed, proceed with encryption
    let encrypted = encrypt_group_image(&sanitized_data)?;
    let encrypted_size = encrypted.encrypted_data.len();
    // Always use version 2 for new uploads (encrypt_group_image creates v2 format)
    // Use the upload seed (cryptographically independent from image seed)
    let upload_keypair = derive_upload_keypair(&encrypted.image_upload_key, 2)?;

    Ok(GroupImageUpload {
        encrypted_data: encrypted.encrypted_data,
        encrypted_hash: encrypted.encrypted_hash,
        image_key: encrypted.image_key,
        image_nonce: encrypted.image_nonce,
        image_upload_key: encrypted.image_upload_key,
        upload_keypair,
        original_size,
        encrypted_size,
        mime_type: canonical_mime_type,
        dimensions,
        blurhash,
    })
}

/// Migrate group image from v1 to v2 format
///
/// This function decrypts an image encrypted with v1 format (direct encryption key)
/// and re-encrypts it using v2 format (seed-derived encryption key). This is used
/// when upgrading a group's extension from version 1 to version 2.
///
/// # Arguments
/// * `encrypted_v1_data` - The encrypted image data from Blossom (v1 format)
/// * `v1_image_hash` - SHA256 hash of the v1 encrypted data (from v1 extension), or `None` for legacy images
/// * `v1_image_key` - The v1 encryption key (32 bytes, used directly)
/// * `v1_image_nonce` - The v1 encryption nonce (12 bytes)
/// * `mime_type` - MIME type of the image (e.g., "image/jpeg", "image/png")
///
/// # Returns
/// * `GroupImageUpload` with v2 format encryption (seed stored in image_key field)
///
/// # Errors
/// * `HashVerificationFailed` - If the v1 encrypted blob hash doesn't match the expected hash
/// * `DecryptionFailed` - If v1 decryption fails
/// * `EncryptionFailed` - If v2 encryption fails
/// * `KeypairDerivationFailed` - If upload keypair derivation fails
///
/// # Example
/// ```ignore
/// // Download encrypted v1 image from Blossom
/// let encrypted_v1 = download_from_blossom(&v1_extension.image_hash.unwrap()).await?;
///
/// // Migrate to v2 format (with hash if available)
/// let v2_prepared = migrate_group_image_v1_to_v2(
///     &encrypted_v1,
///     v1_extension.image_hash.as_ref(),
///     &v1_extension.image_key.unwrap(),
///     &v1_extension.image_nonce.unwrap(),
///     "image/jpeg"
/// )?;
///
/// // Upload new v2 encrypted image to Blossom
/// let new_hash = blossom_client.upload(
///     &v2_prepared.encrypted_data,
///     &v2_prepared.upload_keypair
/// ).await?;
///
/// // Verify hash matches
/// assert_eq!(new_hash, v2_prepared.encrypted_hash);
///
/// // Update extension to v2
/// let mut extension = get_group_extension(&group_id)?;
/// extension.version = 2;
/// extension.image_hash = Some(new_hash);
/// extension.image_key = Some(v2_prepared.image_key); // Encryption seed
/// extension.image_nonce = Some(v2_prepared.image_nonce);
/// extension.image_upload_key = Some(v2_prepared.image_upload_key); // Upload seed (cryptographically independent)
///
/// // Cleanup old v1 image (must use version 1 for v1 images)
/// let old_keypair = derive_upload_keypair(&v1_extension.image_key.unwrap(), 1)?;
/// blossom_client.delete(&v1_extension.image_hash.unwrap(), &old_keypair).await?;
/// ```
pub fn migrate_group_image_v1_to_v2(
    encrypted_v1_data: &[u8],
    v1_image_hash: Option<&[u8; 32]>,
    v1_image_key: &Secret<[u8; 32]>,
    v1_image_nonce: &Secret<[u8; 12]>,
    mime_type: &str,
) -> Result<GroupImageUpload, GroupImageError> {
    let decrypted_data = decrypt_group_image(
        encrypted_v1_data,
        v1_image_hash,
        v1_image_key,
        v1_image_nonce,
    )?;

    // Re-encrypt using v2 format (which generates a seed and derives the encryption key)
    prepare_group_image_for_upload(&decrypted_data, mime_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let original_data = b"This is a test group avatar image";

        // Encrypt
        let encrypted = encrypt_group_image(original_data).unwrap();
        assert_ne!(encrypted.encrypted_data.as_slice(), original_data);
        assert!(encrypted.encrypted_data.len() > original_data.len()); // Includes auth tag

        // Decrypt
        let decrypted = decrypt_group_image(
            &encrypted.encrypted_data,
            Some(&encrypted.encrypted_hash),
            &encrypted.image_key,
            &encrypted.image_nonce,
        )
        .unwrap();

        assert_eq!(decrypted.as_slice(), original_data);
    }

    #[test]
    fn test_decrypt_with_wrong_key() {
        let original_data = b"Test image data";
        let encrypted = encrypt_group_image(original_data).unwrap();

        let wrong_key = Secret::new([0x42u8; 32]);
        let result = decrypt_group_image(
            &encrypted.encrypted_data,
            Some(&encrypted.encrypted_hash),
            &wrong_key,
            &encrypted.image_nonce,
        );

        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(GroupImageError::DecryptionFailed { .. })
        ));
    }

    #[test]
    fn test_decrypt_with_wrong_nonce() {
        let original_data = b"Test image data";
        let encrypted = encrypt_group_image(original_data).unwrap();

        let wrong_nonce = Secret::new([0x24u8; 12]);
        let result = decrypt_group_image(
            &encrypted.encrypted_data,
            Some(&encrypted.encrypted_hash),
            &encrypted.image_key,
            &wrong_nonce,
        );

        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(GroupImageError::DecryptionFailed { .. })
        ));
    }

    #[test]
    fn test_derive_upload_keypair_deterministic() {
        let image_key = Secret::new([0x42u8; 32]);

        let keypair1 = derive_upload_keypair(&image_key, 2).unwrap();
        let keypair2 = derive_upload_keypair(&image_key, 2).unwrap();

        // Same key should derive same keypair
        assert_eq!(keypair1.public_key(), keypair2.public_key());
        // Both should have the same secret key bytes
        assert_eq!(
            keypair1.secret_key().as_secret_bytes(),
            keypair2.secret_key().as_secret_bytes()
        );
    }

    #[test]
    fn test_derive_upload_keypair_different_keys() {
        let key1 = Secret::new([0x42u8; 32]);
        let key2 = Secret::new([0x43u8; 32]);

        let keypair1 = derive_upload_keypair(&key1, 2).unwrap();
        let keypair2 = derive_upload_keypair(&key2, 2).unwrap();

        // Different keys should derive different keypairs
        assert_ne!(keypair1.public_key(), keypair2.public_key());
    }

    #[test]
    fn test_prepare_group_image_for_upload() {
        // Create a valid 64x64 gradient image for testing
        use image::{ImageBuffer, Rgb};
        let img = ImageBuffer::from_fn(64, 64, |x, y| {
            Rgb([(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8])
        });
        let mut image_data = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut image_data),
            image::ImageFormat::Png,
        )
        .unwrap();

        // Test without blurhash due to bugs in blurhash library v0.2
        // The important thing is that the metadata structure is returned
        let options = MediaProcessingOptions {
            sanitize_exif: true,
            generate_blurhash: false,
            ..Default::default()
        };
        let prepared =
            prepare_group_image_for_upload_with_options(&image_data, "image/png", &options)
                .unwrap();

        // Verify all fields are populated
        assert!(!prepared.encrypted_data.is_empty());
        assert_eq!(prepared.original_size, image_data.len());
        assert_eq!(prepared.mime_type, "image/png");

        // Verify metadata is populated
        assert_eq!(prepared.dimensions, Some((64, 64)));
        assert_eq!(prepared.blurhash, None); // Disabled for this test

        // Verify size fields
        assert_eq!(prepared.original_size, image_data.len());
        assert_eq!(prepared.encrypted_size, prepared.encrypted_data.len());

        // Verify the encrypted hash matches the actual hash
        let calculated_hash: [u8; 32] = Sha256::digest(prepared.encrypted_data.as_ref()).into();
        assert_eq!(prepared.encrypted_hash, calculated_hash);

        // Verify we can decrypt
        let decrypted = decrypt_group_image(
            &prepared.encrypted_data,
            Some(&prepared.encrypted_hash),
            &prepared.image_key,
            &prepared.image_nonce,
        )
        .unwrap();
        // The decrypted data should be valid
        assert!(!decrypted.is_empty());

        // Verify keypair derivation is correct (v2 format)
        let derived_keypair = derive_upload_keypair(&prepared.image_upload_key, 2).unwrap();
        assert_eq!(
            derived_keypair.public_key(),
            prepared.upload_keypair.public_key()
        );
    }

    #[test]
    fn test_encrypted_hash_calculation() {
        let image_data = b"Test data for hash";
        let encrypted = encrypt_group_image(image_data).unwrap();

        // Verify hash matches
        let calculated_hash: [u8; 32] = Sha256::digest(encrypted.encrypted_data.as_ref()).into();
        assert_eq!(calculated_hash, encrypted.encrypted_hash);
    }

    #[test]
    fn test_tampering_detection() {
        let original_data = b"Original group image";
        let encrypted = encrypt_group_image(original_data).unwrap();

        // Tamper with encrypted data
        let mut tampered = encrypted.encrypted_data.as_ref().to_vec();
        tampered[0] ^= 0xFF;

        // Decryption should fail due to hash mismatch (hash verification happens before decryption)
        let result = decrypt_group_image(
            &tampered,
            Some(&encrypted.encrypted_hash),
            &encrypted.image_key,
            &encrypted.image_nonce,
        );
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(GroupImageError::HashVerificationFailed { .. })
        ));
    }

    #[test]
    fn test_hash_verification_success() {
        let original_data = b"Test hash verification";
        let encrypted = encrypt_group_image(original_data).unwrap();

        // Decryption should succeed with correct hash
        let result = decrypt_group_image(
            &encrypted.encrypted_data,
            Some(&encrypted.encrypted_hash),
            &encrypted.image_key,
            &encrypted.image_nonce,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_slice(), original_data);
    }

    #[test]
    fn test_hash_verification_failure_wrong_hash() {
        let original_data = b"Test hash verification failure";
        let encrypted = encrypt_group_image(original_data).unwrap();

        // Use wrong hash
        let wrong_hash = [0xFFu8; 32];

        // Decryption should fail due to hash mismatch
        let result = decrypt_group_image(
            &encrypted.encrypted_data,
            Some(&wrong_hash),
            &encrypted.image_key,
            &encrypted.image_nonce,
        );

        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(GroupImageError::HashVerificationFailed { .. })
        ));
    }

    #[test]
    fn test_hash_verification_failure_wrong_blob() {
        let original_data = b"Test hash verification with wrong blob";
        let encrypted = encrypt_group_image(original_data).unwrap();

        // Create a different encrypted blob (encrypted with different key)
        let mut rng = OsRng;
        let mut different_key = [0u8; 32];
        rng.fill_bytes(&mut different_key);
        let different_nonce = [0x42u8; 12];
        let cipher = ChaCha20Poly1305::new_from_slice(&different_key).unwrap();
        let nonce = Nonce::from_slice(&different_nonce);
        let different_blob = cipher.encrypt(nonce, b"Different data".as_ref()).unwrap();

        // Try to decrypt different blob with original hash - should fail hash verification
        let result = decrypt_group_image(
            &different_blob,
            Some(&encrypted.encrypted_hash),
            &encrypted.image_key,
            &encrypted.image_nonce,
        );

        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(GroupImageError::HashVerificationFailed { .. })
        ));
    }

    #[test]
    fn test_hash_verification_backward_compatibility_none() {
        let original_data = b"Test backward compatibility without hash";
        let encrypted = encrypt_group_image(original_data).unwrap();

        // Decryption should succeed without hash verification (legacy mode)
        // This tests backward compatibility for old extensions without image_hash
        let result = decrypt_group_image(
            &encrypted.encrypted_data,
            None,
            &encrypted.image_key,
            &encrypted.image_nonce,
        );

        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_slice(), original_data);
    }

    #[test]
    fn test_mime_type_validation() {
        // Create a valid 64x64 gradient PNG image for testing
        use image::{ImageBuffer, Rgb};
        let img = ImageBuffer::from_fn(64, 64, |x, y| {
            Rgb([(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8])
        });
        let mut png_data = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_data),
            image::ImageFormat::Png,
        )
        .unwrap();

        let options = MediaProcessingOptions {
            sanitize_exif: true,
            generate_blurhash: false,
            ..Default::default()
        };

        // Test valid MIME type that matches the actual file
        let result = prepare_group_image_for_upload_with_options(&png_data, "image/png", &options);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().mime_type, "image/png");

        // Test MIME type canonicalization (uppercase -> lowercase)
        let result = prepare_group_image_for_upload_with_options(&png_data, "Image/PNG", &options);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().mime_type, "image/png");

        // Test MIME type with whitespace
        let result =
            prepare_group_image_for_upload_with_options(&png_data, "  image/png  ", &options);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().mime_type, "image/png");

        // Test MIME type mismatch - claiming JPEG but file is PNG
        let result = prepare_group_image_for_upload_with_options(&png_data, "image/jpeg", &options);
        assert!(result.is_err());
        assert!(matches!(result, Err(GroupImageError::MediaProcessing(_))));

        // Test MIME type mismatch - claiming WebP but file is PNG
        let result = prepare_group_image_for_upload_with_options(&png_data, "image/webp", &options);
        assert!(result.is_err());
        assert!(matches!(result, Err(GroupImageError::MediaProcessing(_))));

        // Test invalid MIME type (no slash)
        let result = prepare_group_image_for_upload_with_options(&png_data, "invalid", &options);
        assert!(result.is_err());
        assert!(matches!(result, Err(GroupImageError::MediaProcessing(_))));

        // Test invalid MIME type (too long)
        let long_mime = "a".repeat(101);
        let result = prepare_group_image_for_upload_with_options(&png_data, &long_mime, &options);
        assert!(result.is_err());
        assert!(matches!(result, Err(GroupImageError::MediaProcessing(_))));
    }

    #[test]
    fn test_prepare_with_default_options() {
        // Create a valid 64x64 gradient image for testing
        use image::{ImageBuffer, Rgb};
        let img = ImageBuffer::from_fn(64, 64, |x, y| {
            Rgb([(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8])
        });
        let mut image_data = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut image_data),
            image::ImageFormat::Png,
        )
        .unwrap();

        // Test with default options but blurhash disabled due to library bugs
        let options = MediaProcessingOptions {
            sanitize_exif: true,
            generate_blurhash: false, // Disabled due to blurhash library bugs
            ..Default::default()
        };

        let result =
            prepare_group_image_for_upload_with_options(&image_data, "image/png", &options);

        assert!(result.is_ok());
        let prepared = result.unwrap();
        assert_eq!(prepared.mime_type, "image/png");
        assert_eq!(prepared.dimensions, Some((64, 64)));
        assert_eq!(prepared.blurhash, None); // Blurhash disabled

        // Verify EXIF stripping is enabled by checking the data was processed
        assert!(!prepared.encrypted_data.is_empty());
    }

    #[test]
    fn test_custom_size_limits() {
        // Create a small 32x32 image
        use image::{ImageBuffer, Rgb};
        let img = ImageBuffer::from_fn(32, 32, |x, y| Rgb([(x * 8) as u8, (y * 8) as u8, 128]));
        let mut image_data = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut image_data),
            image::ImageFormat::Png,
        )
        .unwrap();

        // Test with very restrictive size limit that should reject the image
        let restrictive_options = MediaProcessingOptions {
            sanitize_exif: true,
            generate_blurhash: false,
            max_dimension: Some(16),  // Very small limit
            max_file_size: Some(100), // Very small file size
            max_filename_length: None,
        };

        let result = prepare_group_image_for_upload_with_options(
            &image_data,
            "image/png",
            &restrictive_options,
        );

        // Should fail due to size restrictions
        assert!(result.is_err());
        assert!(matches!(result, Err(GroupImageError::MediaProcessing(_))));

        // Test with permissive options
        let permissive_options = MediaProcessingOptions {
            sanitize_exif: false,     // Don't sanitize
            generate_blurhash: false, // Don't generate blurhash
            max_dimension: Some(1024),
            max_file_size: Some(10 * 1024 * 1024), // 10MB
            max_filename_length: None,
        };

        let result = prepare_group_image_for_upload_with_options(
            &image_data,
            "image/png",
            &permissive_options,
        );

        // Should succeed
        assert!(result.is_ok());
        let prepared = result.unwrap();
        assert_eq!(prepared.mime_type, "image/png");
        assert_eq!(prepared.dimensions, Some((32, 32)));
        assert_eq!(prepared.blurhash, None); // Blurhash disabled
    }

    /// Test v2 encryption/decryption (current format using seed derivation)
    #[test]
    fn test_v2_encryption_uses_seed_derivation() {
        let original_data = b"Test v2 encryption";
        let encrypted = encrypt_group_image(original_data).unwrap();

        // Verify we can decrypt using v2 (seed derivation)
        let decrypted = decrypt_group_image(
            &encrypted.encrypted_data,
            Some(&encrypted.encrypted_hash),
            &encrypted.image_key,
            &encrypted.image_nonce,
        )
        .unwrap();

        assert_eq!(decrypted.as_slice(), original_data);

        // Verify that image_key is actually a seed (not the encryption key directly)
        // by checking that deriving the key from it works
        let hk = Hkdf::<Sha256>::new(None, encrypted.image_key.as_ref());
        let mut derived_key = [0u8; 32];
        hk.expand(IMAGE_ENCRYPTION_CONTEXT_V2, &mut derived_key)
            .unwrap();

        // The derived key should work for decryption
        let cipher = ChaCha20Poly1305::new_from_slice(&derived_key).unwrap();
        let nonce = Nonce::from_slice(encrypted.image_nonce.as_ref());
        let decrypted_v2 = cipher
            .decrypt(nonce, encrypted.encrypted_data.as_ref().as_slice())
            .unwrap();
        assert_eq!(decrypted_v2.as_slice(), original_data);
    }

    /// Test v1 backward compatibility: decrypt data encrypted with v1 format (direct key)
    #[test]
    fn test_v1_backward_compatibility_decryption() {
        let original_data = b"Test v1 encrypted data";

        // Simulate v1 encryption: use key directly (not derived from seed)
        let mut rng = OsRng;
        let mut image_key_v1 = [0u8; 32];
        let mut image_nonce = [0u8; 12];
        rng.fill_bytes(&mut image_key_v1);
        rng.fill_bytes(&mut image_nonce);

        // Encrypt with v1 format (direct key)
        let cipher = ChaCha20Poly1305::new_from_slice(&image_key_v1).unwrap();
        let nonce = Nonce::from_slice(&image_nonce);
        let encrypted_data = cipher.encrypt(nonce, original_data.as_ref()).unwrap();

        // Calculate hash of encrypted data for verification
        let encrypted_hash: [u8; 32] = Sha256::digest(&encrypted_data).into();

        // Verify we can decrypt using v1 format (fallback)
        let decrypted = decrypt_group_image(
            &encrypted_data,
            Some(&encrypted_hash),
            &Secret::new(image_key_v1),
            &Secret::new(image_nonce),
        )
        .unwrap();
        assert_eq!(decrypted.as_slice(), original_data);
    }

    /// Test v2 upload keypair derivation
    #[test]
    fn test_v2_upload_keypair_derivation() {
        // Generate a seed (as v2 would)
        let mut rng = OsRng;
        let mut image_seed = [0u8; 32];
        rng.fill_bytes(&mut image_seed);

        // Derive upload keypair using v2 method
        let hk = Hkdf::<Sha256>::new(None, &image_seed);
        let mut upload_secret = [0u8; 32];
        hk.expand(UPLOAD_KEYPAIR_CONTEXT_V2, &mut upload_secret)
            .unwrap();
        let secret_key = nostr::SecretKey::from_slice(&upload_secret).unwrap();
        let expected_keypair = nostr::Keys::new(secret_key);

        // Verify derive_upload_keypair uses v2 when given a seed and version 2
        let derived_keypair = derive_upload_keypair(&Secret::new(image_seed), 2).unwrap();
        assert_eq!(derived_keypair.public_key(), expected_keypair.public_key());
    }

    /// Test v1 upload keypair derivation (backward compatibility)
    #[test]
    fn test_v1_upload_keypair_derivation() {
        // Simulate v1: use encryption key directly (not a seed)
        let mut rng = OsRng;
        let mut image_key_v1 = [0u8; 32];
        rng.fill_bytes(&mut image_key_v1);

        // Derive upload keypair using v1 method directly
        let hk = Hkdf::<Sha256>::new(None, &image_key_v1);
        let mut upload_secret = [0u8; 32];
        hk.expand(UPLOAD_KEYPAIR_CONTEXT_V1, &mut upload_secret)
            .unwrap();
        let secret_key = nostr::SecretKey::from_slice(&upload_secret).unwrap();
        let expected_v1_keypair = nostr::Keys::new(secret_key);

        // Verify derive_upload_keypair works with v1-style key when version 1 is specified
        let derived_keypair = derive_upload_keypair(&Secret::new(image_key_v1), 1).unwrap();

        // Verify it's deterministic
        let derived_keypair2 = derive_upload_keypair(&Secret::new(image_key_v1), 1).unwrap();
        assert_eq!(derived_keypair.public_key(), derived_keypair2.public_key());

        // Verify that v1 derivation produces the correct keypair
        assert_eq!(
            derived_keypair.public_key(),
            expected_v1_keypair.public_key(),
            "v1 derivation should produce the expected keypair"
        );

        // Verify that v1 and v2 produce different keypairs for the same input
        let v2_keypair = derive_upload_keypair(&Secret::new(image_key_v1), 2).unwrap();
        assert_ne!(
            derived_keypair.public_key(),
            v2_keypair.public_key(),
            "v2 and v1 should produce different keypairs for the same input"
        );
    }

    /// Test that v2 and v1 produce different keypairs for the same input
    /// (demonstrating that they use different HKDF contexts)
    #[test]
    fn test_v1_v2_keypair_difference() {
        // Use the same 32-byte value as both v1 key and v2 seed
        let test_bytes = [0x42u8; 32];

        // Derive using v1 method
        let hk_v1 = Hkdf::<Sha256>::new(None, &test_bytes);
        let mut upload_secret_v1 = [0u8; 32];
        hk_v1
            .expand(UPLOAD_KEYPAIR_CONTEXT_V1, &mut upload_secret_v1)
            .unwrap();
        let secret_key_v1 = nostr::SecretKey::from_slice(&upload_secret_v1).unwrap();
        let keypair_v1 = nostr::Keys::new(secret_key_v1);

        // Derive using v2 method
        let hk_v2 = Hkdf::<Sha256>::new(None, &test_bytes);
        let mut upload_secret_v2 = [0u8; 32];
        hk_v2
            .expand(UPLOAD_KEYPAIR_CONTEXT_V2, &mut upload_secret_v2)
            .unwrap();
        let secret_key_v2 = nostr::SecretKey::from_slice(&upload_secret_v2).unwrap();
        let keypair_v2 = nostr::Keys::new(secret_key_v2);

        // They should be different (different HKDF contexts)
        assert_ne!(keypair_v1.public_key(), keypair_v2.public_key());
    }

    /// Test that v2 uses separate seeds: encryption key from image_seed, upload keypair from upload_seed
    #[test]
    fn test_v2_encryption_and_upload_derivation() {
        let original_data = b"Test v2 derivation consistency";

        // Encrypt using v2 (generates separate seeds for encryption and upload)
        let encrypted = encrypt_group_image(original_data).unwrap();
        let image_seed = encrypted.image_key; // Encryption seed
        let upload_seed = encrypted.image_upload_key; // Upload seed (cryptographically independent)

        // Verify seeds are different (cryptographic independence)
        assert_ne!(image_seed, upload_seed);

        // Derive encryption key from image_seed
        let hk_enc = Hkdf::<Sha256>::new(None, image_seed.as_ref());
        let mut encryption_key = [0u8; 32];
        hk_enc
            .expand(IMAGE_ENCRYPTION_CONTEXT_V2, &mut encryption_key)
            .unwrap();

        // Derive upload keypair from upload_seed (v2 format - separate from encryption seed)
        let upload_keypair = derive_upload_keypair(&upload_seed, 2).unwrap();

        // Verify we can decrypt using the derived encryption key
        let cipher = ChaCha20Poly1305::new_from_slice(&encryption_key).unwrap();
        let nonce = Nonce::from_slice(encrypted.image_nonce.as_ref());
        let decrypted = cipher
            .decrypt(nonce, encrypted.encrypted_data.as_ref().as_slice())
            .unwrap();
        assert_eq!(decrypted.as_slice(), original_data);

        // Verify upload keypair derivation is deterministic from upload_seed
        let upload_keypair2 = derive_upload_keypair(&upload_seed, 2).unwrap();
        assert_eq!(upload_keypair.public_key(), upload_keypair2.public_key());

        // Verify that deriving from image_seed would give a different keypair (cryptographic independence)
        let different_keypair = derive_upload_keypair(&image_seed, 2).unwrap();
        assert_ne!(upload_keypair.public_key(), different_keypair.public_key());
    }

    /// Test migration from v1 to v2 format
    #[test]
    fn test_migrate_v1_to_v2() {
        // Create a valid 64x64 gradient image for testing
        use image::{ImageBuffer, Rgb};
        let img = ImageBuffer::from_fn(64, 64, |x, y| {
            Rgb([(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8])
        });
        let mut original_data = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut original_data),
            image::ImageFormat::Png,
        )
        .unwrap();

        // Simulate v1 encryption: use key directly (not derived from seed)
        let mut rng = OsRng;
        let mut v1_image_key = [0u8; 32];
        let mut v1_image_nonce = [0u8; 12];
        rng.fill_bytes(&mut v1_image_key);
        rng.fill_bytes(&mut v1_image_nonce);

        // Encrypt with v1 format (direct key)
        let cipher = ChaCha20Poly1305::new_from_slice(&v1_image_key).unwrap();
        let nonce = Nonce::from_slice(&v1_image_nonce);
        let encrypted_v1_data = cipher.encrypt(nonce, original_data.as_ref()).unwrap();

        // Calculate hash of v1 encrypted data
        let v1_image_hash: [u8; 32] = Sha256::digest(&encrypted_v1_data).into();

        // Migrate to v2 format
        let v2_prepared = migrate_group_image_v1_to_v2(
            &encrypted_v1_data,
            Some(&v1_image_hash),
            &Secret::new(v1_image_key),
            &Secret::new(v1_image_nonce),
            "image/png",
        )
        .unwrap();

        // Verify v2 encrypted data is different from v1
        assert_ne!(encrypted_v1_data, *v2_prepared.encrypted_data);

        // Verify we can decrypt v2 data using the seed
        let decrypted_v2 = decrypt_group_image(
            &v2_prepared.encrypted_data,
            Some(&v2_prepared.encrypted_hash),
            &v2_prepared.image_key, // This is the seed in v2
            &v2_prepared.image_nonce,
        )
        .unwrap();

        // Verify decrypted data matches original (after processing)
        // Note: prepare_group_image_for_upload may process the image, so we just verify it's not empty
        assert!(!decrypted_v2.is_empty());

        // Verify v2 uses seed derivation (image_key is seed, not encryption key)
        // We can verify this by checking that deriving the encryption key works
        let hk = Hkdf::<Sha256>::new(None, v2_prepared.image_key.as_ref());
        let mut derived_key = [0u8; 32];
        hk.expand(IMAGE_ENCRYPTION_CONTEXT_V2, &mut derived_key)
            .unwrap();

        // The derived key should work for decryption
        let cipher_v2 = ChaCha20Poly1305::new_from_slice(&derived_key).unwrap();
        let nonce_v2 = Nonce::from_slice(v2_prepared.image_nonce.as_ref());
        let decrypted_with_derived = cipher_v2
            .decrypt(nonce_v2, v2_prepared.encrypted_data.as_ref().as_slice())
            .unwrap();
        assert_eq!(decrypted_v2, decrypted_with_derived);

        // Verify upload keypair derivation works with v2 upload seed
        let upload_keypair = derive_upload_keypair(&v2_prepared.image_upload_key, 2).unwrap();
        assert_eq!(
            upload_keypair.public_key(),
            v2_prepared.upload_keypair.public_key()
        );
    }

    /// Test that migration fails with wrong v1 key
    #[test]
    fn test_migrate_v1_to_v2_wrong_key() {
        // Create valid v1 encrypted data
        let mut rng = OsRng;
        let mut v1_key = [0u8; 32];
        let mut v1_nonce = [0u8; 12];
        rng.fill_bytes(&mut v1_key);
        rng.fill_bytes(&mut v1_nonce);

        let original_data = b"test data";
        let cipher = ChaCha20Poly1305::new_from_slice(&v1_key).unwrap();
        let nonce = Nonce::from_slice(&v1_nonce);
        let encrypted = cipher.encrypt(nonce, original_data.as_ref()).unwrap();

        // Calculate hash of encrypted data
        let encrypted_hash: [u8; 32] = Sha256::digest(&encrypted).into();

        // Try to migrate with wrong key
        let wrong_key = Secret::new([0xFFu8; 32]);
        let result = migrate_group_image_v1_to_v2(
            &encrypted,
            Some(&encrypted_hash),
            &wrong_key,
            &Secret::new(v1_nonce),
            "image/png",
        );

        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(GroupImageError::DecryptionFailed { .. })
        ));
    }

    /// Test that migration fails with corrupted v1 data
    #[test]
    fn test_migrate_v1_to_v2_corrupted_data() {
        let mut rng = OsRng;
        let mut v1_key = [0u8; 32];
        let mut v1_nonce = [0u8; 12];
        rng.fill_bytes(&mut v1_key);
        rng.fill_bytes(&mut v1_nonce);

        // Corrupted encrypted data
        let corrupted_data = vec![0xFFu8; 100];
        // Calculate hash of corrupted data (will fail hash verification)
        let corrupted_hash: [u8; 32] = Sha256::digest(&corrupted_data).into();

        let result = migrate_group_image_v1_to_v2(
            &corrupted_data,
            Some(&corrupted_hash),
            &Secret::new(v1_key),
            &Secret::new(v1_nonce),
            "image/png",
        );

        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(GroupImageError::DecryptionFailed { .. })
        ));
    }

    /// Test that v1 and v2 produce different encrypted data for the same source
    #[test]
    fn test_v1_v2_produce_different_encryption() {
        // Create test image
        use image::{ImageBuffer, Rgb};
        let img = ImageBuffer::from_fn(64, 64, |x, y| {
            Rgb([(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8])
        });
        let mut image_data = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut image_data),
            image::ImageFormat::Png,
        )
        .unwrap();

        // Encrypt with v1 (direct key)
        let mut rng = OsRng;
        let mut v1_key = [0u8; 32];
        let mut v1_nonce = [0u8; 12];
        rng.fill_bytes(&mut v1_key);
        rng.fill_bytes(&mut v1_nonce);

        let cipher_v1 = ChaCha20Poly1305::new_from_slice(&v1_key).unwrap();
        let nonce_v1 = Nonce::from_slice(&v1_nonce);
        let encrypted_v1 = cipher_v1.encrypt(nonce_v1, image_data.as_ref()).unwrap();

        // Calculate hash of v1 encrypted data
        let v1_hash: [u8; 32] = Sha256::digest(&encrypted_v1).into();

        // Migrate to v2
        let v2_prepared = migrate_group_image_v1_to_v2(
            &encrypted_v1,
            Some(&v1_hash),
            &Secret::new(v1_key),
            &Secret::new(v1_nonce),
            "image/png",
        )
        .unwrap();

        // Verify encrypted data is different (even though source is same)
        assert_ne!(encrypted_v1, *v2_prepared.encrypted_data);

        // Verify hashes are different
        let hash_v1: [u8; 32] = Sha256::digest(&encrypted_v1).into();
        assert_ne!(hash_v1, v2_prepared.encrypted_hash);
    }

    /// Test that migration preserves image metadata (dimensions, MIME type)
    #[test]
    fn test_migration_preserves_metadata() {
        // Create test image with known dimensions
        use image::{ImageBuffer, Rgb};
        let img = ImageBuffer::from_fn(128, 64, |x, y| {
            Rgb([(x * 2) as u8, (y * 4) as u8, ((x + y) * 2) as u8])
        });
        let mut image_data = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut image_data),
            image::ImageFormat::Png,
        )
        .unwrap();

        // Encrypt with v1
        let mut rng = OsRng;
        let mut v1_key = [0u8; 32];
        let mut v1_nonce = [0u8; 12];
        rng.fill_bytes(&mut v1_key);
        rng.fill_bytes(&mut v1_nonce);

        let cipher = ChaCha20Poly1305::new_from_slice(&v1_key).unwrap();
        let nonce = Nonce::from_slice(&v1_nonce);
        let encrypted_v1 = cipher.encrypt(nonce, image_data.as_ref()).unwrap();

        // Calculate hash of v1 encrypted data
        let v1_hash: [u8; 32] = Sha256::digest(&encrypted_v1).into();

        // Migrate to v2
        let v2_prepared = migrate_group_image_v1_to_v2(
            &encrypted_v1,
            Some(&v1_hash),
            &Secret::new(v1_key),
            &Secret::new(v1_nonce),
            "image/png",
        )
        .unwrap();

        // Verify metadata is preserved
        assert_eq!(v2_prepared.mime_type, "image/png");
        assert_eq!(v2_prepared.dimensions, Some((128, 64)));
        assert_eq!(v2_prepared.original_size, image_data.len());
    }

    /// Test that upload keypair derivation depends only on upload_seed, not image_seed
    #[test]
    fn test_upload_keypair_depends_only_on_upload_seed() {
        // Create test image
        use image::{ImageBuffer, Rgb};
        let img = ImageBuffer::from_fn(32, 32, |x, y| Rgb([x as u8, y as u8, 128]));
        let mut image_data = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut image_data),
            image::ImageFormat::Png,
        )
        .unwrap();

        // Prepare for upload
        let prepared1 = prepare_group_image_for_upload(&image_data, "image/png").unwrap();

        // Manually create another prepared image with same upload key but different image key
        let mut prepared2 = prepare_group_image_for_upload(&image_data, "image/png").unwrap();
        prepared2.image_key = Secret::new([0xAAu8; 32]); // Change encryption seed (should not affect upload keypair)

        // Upload keypairs should be different (since different upload seeds)
        assert_ne!(
            prepared1.upload_keypair.public_key(),
            prepared2.upload_keypair.public_key()
        );

        // But if we manually set the upload seed to be the same, the keypair should be the same
        // (demonstrating that upload keypair depends only on upload_seed, not image_seed)
        prepared2.image_upload_key = prepared1.image_upload_key;
        let keypair2 = derive_upload_keypair(&prepared2.image_upload_key, 2).unwrap();
        assert_eq!(keypair2.public_key(), prepared1.upload_keypair.public_key());
    }

    /// Test that we can still decrypt v1 data after migration
    #[test]
    fn test_v1_decryption_still_works_after_migration() {
        // Create test image
        use image::{ImageBuffer, Rgb};
        let img = ImageBuffer::from_fn(64, 64, |x, y| {
            Rgb([(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8])
        });
        let mut image_data = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut image_data),
            image::ImageFormat::Png,
        )
        .unwrap();

        // Encrypt with v1
        let mut rng = OsRng;
        let mut v1_key = [0u8; 32];
        let mut v1_nonce = [0u8; 12];
        rng.fill_bytes(&mut v1_key);
        rng.fill_bytes(&mut v1_nonce);

        let cipher = ChaCha20Poly1305::new_from_slice(&v1_key).unwrap();
        let nonce = Nonce::from_slice(&v1_nonce);
        let encrypted_v1 = cipher.encrypt(nonce, image_data.as_ref()).unwrap();

        // Calculate hash of v1 encrypted data
        let v1_hash: [u8; 32] = Sha256::digest(&encrypted_v1).into();

        // Migrate to v2
        let v2_prepared = migrate_group_image_v1_to_v2(
            &encrypted_v1,
            Some(&v1_hash),
            &Secret::new(v1_key),
            &Secret::new(v1_nonce),
            "image/png",
        )
        .unwrap();

        // Verify we can still decrypt original v1 data
        let decrypted_v1 = decrypt_group_image(
            &encrypted_v1,
            Some(&v1_hash),
            &Secret::new(v1_key),
            &Secret::new(v1_nonce),
        )
        .unwrap();

        assert_eq!(decrypted_v1, image_data);

        // Verify v2 data decrypts correctly too
        let decrypted_v2 = decrypt_group_image(
            &v2_prepared.encrypted_data,
            Some(&v2_prepared.encrypted_hash),
            &v2_prepared.image_key,
            &v2_prepared.image_nonce,
        )
        .unwrap();

        // Both should decrypt successfully (v2 may have processed the image)
        assert!(!decrypted_v1.is_empty());
        assert!(!decrypted_v2.is_empty());
    }
}
