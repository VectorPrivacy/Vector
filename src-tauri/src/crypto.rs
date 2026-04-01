//! Cryptographic functions — Tauri-specific wrappers around vector-core.
//!
//! Core crypto lives in vector-core (AES-GCM, ChaCha20, Argon2, GuardedKey,
//! maybe_encrypt/decrypt, looks_encrypted). This module provides:
//! - hash_pass with owned-String zeroization
//! - encrypt_with_key/decrypt_with_key for re-keying flows
//! - Re-exports for backward compatibility

use crate::rand;
use crate::rand::Rng;
use crate::util::{bytes_to_hex_string, hex_string_to_bytes};
use chacha20poly1305::{
    aead::Aead,
    ChaCha20Poly1305, KeyInit, Nonce
};
use zeroize::Zeroize;

// Re-export from vector-core
pub use vector_core::crypto::{
    decrypt_data, looks_encrypted, is_encryption_enabled,
    maybe_encrypt, maybe_decrypt,
};

/// Hash a password using Argon2id (with zeroization of the owned password).
pub async fn hash_pass(mut password: String) -> [u8; 32] {
    let key = vector_core::crypto::hash_pass(&password).await;
    password.zeroize();
    key
}

/// Encrypt with an explicit key (for re-keying — doesn't touch ENCRYPTION_KEY global).
pub fn encrypt_with_key(input: &str, key: &[u8; 32]) -> String {
    let mut rng = rand::thread_rng();
    let nonce_bytes: [u8; 12] = rng.gen();

    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .expect("Key should be valid");
    let nonce: Nonce = nonce_bytes.into();
    let ciphertext = cipher
        .encrypt(&nonce, input.as_bytes())
        .expect("Encryption should not fail");

    let mut buffer = Vec::with_capacity(nonce_bytes.len() + ciphertext.len());
    buffer.extend_from_slice(&nonce_bytes);
    buffer.extend_from_slice(&ciphertext);

    bytes_to_hex_string(&buffer)
}

/// Decrypt with an explicit key (for re-keying — doesn't touch ENCRYPTION_KEY global).
pub fn decrypt_with_key(ciphertext: &str, key: &[u8; 32]) -> Result<String, ()> {
    let encrypted_data = hex_string_to_bytes(ciphertext);
    if encrypted_data.len() < 12 {
        return Err(());
    }

    let (nonce_bytes, actual_ciphertext) = encrypted_data.split_at(12);

    let cipher = match ChaCha20Poly1305::new_from_slice(key) {
        Ok(c) => c,
        Err(_) => return Err(()),
    };

    let nonce_arr: [u8; 12] = nonce_bytes.try_into().map_err(|_| ())?;
    let nonce: Nonce = nonce_arr.into();
    let plaintext = match cipher.decrypt(&nonce, actual_ciphertext) {
        Ok(pt) => pt,
        Err(_) => return Err(()),
    };

    // SAFETY: plaintext was originally valid UTF-8, authenticated decryption ensures integrity
    unsafe { Ok(String::from_utf8_unchecked(plaintext)) }
}

// Backward-compat aliases — these now delegate to vector-core
pub async fn internal_encrypt(input: String, password: Option<String>) -> String {
    vector_core::crypto::maybe_encrypt_inner(input, password).await
}

pub async fn internal_decrypt(ciphertext: String, password: Option<String>) -> Result<String, ()> {
    vector_core::crypto::maybe_decrypt_inner(ciphertext, password).await
}
