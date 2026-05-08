//! MLS Group Context Extension for Nostr Groups (MIP-01)
//!
//! This module provides the Marmot Group Data Extension and related functionality:
//! - Extension types for storing group metadata in MLS
//! - Group image encryption/decryption for avatars
//! - Blossom upload keypair derivation for cleanup

pub mod group_image;
pub mod types;

// Re-export main extension types
pub use types::NostrGroupDataExtension;

// Re-export group image types and functions
pub use group_image::{
    GroupImageEncryptionInfo, GroupImageError, GroupImageUpload, decrypt_group_image,
    derive_upload_keypair, migrate_group_image_v1_to_v2, prepare_group_image_for_upload,
    prepare_group_image_for_upload_with_options,
};
