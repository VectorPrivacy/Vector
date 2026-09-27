//! Cryptographic functions — Tauri-specific wrappers around vector-core.
//!
//! Core crypto lives in vector-core (AES-GCM, ChaCha20, Argon2, GuardedKey,
//! maybe_encrypt/decrypt, looks_encrypted). This module provides:
//! - hash_pass with owned-String zeroization
//! - encrypt_with_key/decrypt_with_key for re-keying flows
//! - Re-exports for backward compatibility

use zeroize::Zeroize;

// Re-export from vector-core. `is_encryption_enabled` is intentionally NOT
// re-exported: src-tauri callers now go through
// `vector_core::state::is_encryption_enabled_fast()` (atomic, seeded by
// `init_encryption_enabled()` via the canonical resolver). Routing through
// the atomic keeps every code site in agreement about the missing-row case.
pub use vector_core::crypto::{
    maybe_encrypt, maybe_decrypt,
    encrypt_blob_with_key, decrypt_blob_with_key,
};

/// Hash a password using Argon2id (with zeroization of the owned password).
pub async fn hash_pass(mut password: String) -> [u8; 32] {
    let key = vector_core::crypto::hash_pass(&password).await;
    password.zeroize();
    key
}

/// Encrypt with an explicit key (for re-keying — doesn't touch ENCRYPTION_KEY global).
pub fn encrypt_with_key(input: &str, key: &[u8; 32]) -> String {
    vector_core::crypto::encrypt_with_key(input, key).expect("Encryption should not fail")
}

/// Decrypt with an explicit key (for re-keying — doesn't touch ENCRYPTION_KEY global).
pub fn decrypt_with_key(ciphertext: &str, key: &[u8; 32]) -> Result<String, ()> {
    vector_core::crypto::decrypt_with_key(ciphertext, key).map_err(|_| ())
}

// Backward-compat aliases — these now delegate to vector-core
pub async fn internal_encrypt(input: String, password: Option<String>) -> String {
    vector_core::crypto::maybe_encrypt_inner(input, password).await
}

pub async fn internal_decrypt(ciphertext: String, password: Option<String>) -> Result<String, ()> {
    vector_core::crypto::maybe_decrypt_inner(ciphertext, password).await
}
