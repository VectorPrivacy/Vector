//! Cryptographic functions — Tauri-specific wrappers + local encryption helpers.
//!
//! Core crypto (AES-GCM, ChaCha20, Argon2, GuardedKey) lives in vector-core.
//! This module provides:
//! - Signature-compatible wrappers for encrypt_with_key/decrypt_with_key
//! - hash_pass with owned-String zeroization
//! - Conditional encryption helpers (maybe_encrypt/decrypt) using ENCRYPTION_KEY vault
//! - SIMD-accelerated looks_encrypted detection

use crate::rand;
use crate::rand::Rng;
use crate::util::{bytes_to_hex_string, hex_string_to_bytes};
use chacha20poly1305::{
    aead::Aead,
    ChaCha20Poly1305, KeyInit, Nonce
};
use zeroize::Zeroize;

// Re-export shared crypto from vector-core
pub use vector_core::crypto::{
    EncryptionParams, generate_encryption_params, encrypt_data, decrypt_data,
};

/// Hash a password using Argon2id (with zeroization of the owned password).
pub async fn hash_pass(mut password: String) -> [u8; 32] {
    let key = vector_core::crypto::hash_pass(&password).await;
    password.zeroize();
    key
}

/// Encrypt with an explicit key (for re-keying — doesn't touch ENCRYPTION_KEY global).
/// Infallible wrapper around vector-core's Result-returning version.
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

    // SAFETY: The plaintext bytes are guaranteed to be valid UTF-8, because:
    // 1. They were originally created from a valid UTF-8 string
    // 2. ChaCha20-Poly1305 authenticated decryption ensures the data is intact
    unsafe { Ok(String::from_utf8_unchecked(plaintext)) }
}

// ============================================================================
// Conditional Encryption Helpers - Check encryption_enabled setting
// ============================================================================

/// Check if local encryption is enabled.
/// Uses cached AtomicBool (~1ns) instead of SQLite query (~5-20µs).
#[inline]
pub fn is_encryption_enabled() -> bool {
    crate::state::is_encryption_enabled_fast()
}

/// Internal function for encryption logic using ChaCha20Poly1305
pub async fn internal_encrypt(mut input: String, password: Option<String>) -> String {
    let mut key: [u8; 32] = if password.is_none() {
        crate::ENCRYPTION_KEY.get().expect("Encryption key must be set")
    } else {
        hash_pass(password.unwrap()).await
    };

    let mut rng = rand::thread_rng();
    let nonce_bytes: [u8; 12] = rng.gen();

    let cipher = ChaCha20Poly1305::new_from_slice(&key)
        .expect("Key should be valid");
    let nonce: Nonce = nonce_bytes.into();

    let ciphertext = cipher
        .encrypt(&nonce, input.as_bytes())
        .expect("Encryption should not fail");
    input.zeroize();

    let mut buffer = Vec::with_capacity(nonce_bytes.len() + ciphertext.len());
    buffer.extend_from_slice(&nonce_bytes);
    buffer.extend_from_slice(&ciphertext);

    if !crate::ENCRYPTION_KEY.has_key() {
        crate::ENCRYPTION_KEY.set(key, &[&crate::MY_SECRET_KEY]);
    }

    key.zeroize();

    bytes_to_hex_string(&buffer)
}

/// Internal function for decryption logic using ChaCha20Poly1305
pub async fn internal_decrypt(ciphertext: String, password: Option<String>) -> Result<String, ()> {
    let has_password = password.is_some();

    let mut key: [u8; 32] = if let Some(pass) = password {
        hash_pass(pass).await
    } else {
        match crate::ENCRYPTION_KEY.get() {
            Some(k) => k,
            None => return Err(()),
        }
    };

    let encrypted_data = match hex_string_to_bytes(ciphertext.as_str()) {
        bytes if bytes.len() >= 12 => bytes,
        _ => { key.zeroize(); return Err(()) }
    };

    let (nonce_bytes, actual_ciphertext) = encrypted_data.split_at(12);

    let cipher = match ChaCha20Poly1305::new_from_slice(&key) {
        Ok(c) => c,
        Err(_) => { key.zeroize(); return Err(()) }
    };

    let nonce_arr: [u8; 12] = nonce_bytes.try_into().map_err(|_| ())?;
    let nonce: Nonce = nonce_arr.into();
    let plaintext = match cipher.decrypt(&nonce, actual_ciphertext) {
        Ok(pt) => pt,
        Err(_) => { key.zeroize(); return Err(()) }
    };

    if has_password && !crate::ENCRYPTION_KEY.has_key() {
        crate::ENCRYPTION_KEY.set(key, &[&crate::MY_SECRET_KEY]);
    }

    key.zeroize();

    unsafe {
        Ok(String::from_utf8_unchecked(plaintext))
    }
}

/// Conditionally encrypt content based on encryption_enabled setting.
pub async fn maybe_encrypt(input: String) -> String {
    if is_encryption_enabled() {
        internal_encrypt(input, None).await
    } else {
        input
    }
}

/// Conditionally decrypt content based on encryption_enabled setting.
/// Handles crash recovery gracefully — if decryption fails on non-encrypted-looking content,
/// returns it as-is (handles partially-migrated state from interrupted migrations).
pub async fn maybe_decrypt(input: String) -> Result<String, ()> {
    if is_encryption_enabled() {
        match internal_decrypt(input.clone(), None).await {
            Ok(decrypted) => Ok(decrypted),
            Err(_) => {
                if looks_encrypted(&input) {
                    Err(())
                } else {
                    Ok(input)
                }
            }
        }
    } else {
        if looks_encrypted(&input) {
            match internal_decrypt(input.clone(), None).await {
                Ok(decrypted) => Ok(decrypted),
                Err(_) => Ok(input),
            }
        } else {
            Ok(input)
        }
    }
}

/// Check if a string looks like encrypted content (hex-encoded ChaCha20 output).
/// Minimum (empty message): 12 + 0 + 16 = 28 bytes = 56 hex chars.
#[inline]
pub fn looks_encrypted(s: &str) -> bool {
    if s.len() < 56 { return false; }
    is_all_lowercase_hex(s.as_bytes())
}

/// NEON: check if all bytes are lowercase hex [0-9a-f] using 16-byte SIMD range checks.
#[cfg(target_arch = "aarch64")]
#[inline]
fn is_all_lowercase_hex(bytes: &[u8]) -> bool {
    use std::arch::aarch64::*;
    unsafe {
        let mut i = 0;
        while i + 16 <= bytes.len() {
            let chunk = vld1q_u8(bytes.as_ptr().add(i));
            let is_digit = vandq_u8(vcgeq_u8(chunk, vdupq_n_u8(b'0')),
                                    vcleq_u8(chunk, vdupq_n_u8(b'9')));
            let is_af = vandq_u8(vcgeq_u8(chunk, vdupq_n_u8(b'a')),
                                 vcleq_u8(chunk, vdupq_n_u8(b'f')));
            if vminvq_u8(vorrq_u8(is_digit, is_af)) == 0 { return false; }
            i += 16;
        }
        while i < bytes.len() {
            let b = bytes[i];
            if !matches!(b, b'0'..=b'9' | b'a'..=b'f') { return false; }
            i += 1;
        }
    }
    true
}

/// SSE2: check if all bytes are lowercase hex [0-9a-f] using saturating subtract range checks.
#[cfg(target_arch = "x86_64")]
#[inline]
fn is_all_lowercase_hex(bytes: &[u8]) -> bool {
    use std::arch::x86_64::*;
    unsafe {
        let mut i = 0;
        while i + 16 <= bytes.len() {
            let chunk = _mm_loadu_si128(bytes.as_ptr().add(i) as *const __m128i);
            let is_digit = _mm_cmpeq_epi8(
                _mm_subs_epu8(_mm_sub_epi8(chunk, _mm_set1_epi8(b'0' as i8)), _mm_set1_epi8(9)),
                _mm_setzero_si128());
            let is_af = _mm_cmpeq_epi8(
                _mm_subs_epu8(_mm_sub_epi8(chunk, _mm_set1_epi8(b'a' as i8)), _mm_set1_epi8(5)),
                _mm_setzero_si128());
            if _mm_movemask_epi8(_mm_or_si128(is_digit, is_af)) != 0xFFFF { return false; }
            i += 16;
        }
        while i < bytes.len() {
            let b = bytes[i];
            if !matches!(b, b'0'..=b'9' | b'a'..=b'f') { return false; }
            i += 1;
        }
    }
    true
}

/// Scalar fallback for platforms without SIMD.
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline]
fn is_all_lowercase_hex(bytes: &[u8]) -> bool {
    const IS_LOWER_HEX: [bool; 256] = {
        let mut t = [false; 256];
        t[b'0' as usize] = true; t[b'1' as usize] = true; t[b'2' as usize] = true;
        t[b'3' as usize] = true; t[b'4' as usize] = true; t[b'5' as usize] = true;
        t[b'6' as usize] = true; t[b'7' as usize] = true; t[b'8' as usize] = true;
        t[b'9' as usize] = true; t[b'a' as usize] = true; t[b'b' as usize] = true;
        t[b'c' as usize] = true; t[b'd' as usize] = true; t[b'e' as usize] = true;
        t[b'f' as usize] = true;
        t
    };
    bytes.iter().all(|&b| IS_LOWER_HEX[b as usize])
}
