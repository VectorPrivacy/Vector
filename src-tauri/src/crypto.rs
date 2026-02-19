use crate::rand;
use crate::rand::Rng;
use crate::util::{bytes_to_hex_string, hex_string_to_bytes};
use aes::Aes256;
use aes::cipher::typenum::U16;
use aes_gcm::{AesGcm, AeadInPlace, KeyInit};
use argon2::{Argon2, Params, Version};
use chacha20poly1305::{
    aead::Aead,
    ChaCha20Poly1305, Nonce
};

/// Represents encryption parameters
#[derive(Debug)]
pub struct EncryptionParams {
    pub key: String,    // Hex string
    pub nonce: String,  // Hex string
}

/// Generates random encryption parameters (key and nonce)
pub fn generate_encryption_params() -> EncryptionParams {
    let mut rng = rand::thread_rng();
    
    // Generate 32 byte key (for AES-256)
    let key: [u8; 32] = rng.gen();
    // Generate 16 byte nonce (to match 0xChat)
    let nonce: [u8; 16] = rng.gen();
    
    EncryptionParams {
        key: bytes_to_hex_string(&key),
        nonce: bytes_to_hex_string(&nonce),
    }
}

/// Encrypts data using AES-256-GCM with a 16-byte nonce
pub fn encrypt_data(data: &[u8], params: &EncryptionParams) -> Result<Vec<u8>, String> {
    // Decode key and nonce from hex
    let key_bytes = hex_string_to_bytes(&params.key);
    let nonce_bytes = hex_string_to_bytes(&params.nonce);

    // Initialize AES-GCM cipher
    let cipher = AesGcm::<Aes256, U16>::new_from_slice(&key_bytes)
        .map_err(|_| String::from("Invalid encryption key"))?;

    // Prepare nonce
    let nonce_arr: [u8; 16] = nonce_bytes.try_into()
        .map_err(|_| String::from("Invalid nonce length"))?;
    let nonce = aes_gcm::Nonce::<U16>::from(nonce_arr);

    // Create output buffer
    let mut buffer = data.to_vec();

    // Encrypt in place and get authentication tag
    let tag = cipher
        .encrypt_in_place_detached(&nonce, &[], &mut buffer)
        .map_err(|_| String::from("Failed to Encrypt Data"))?;

    // Append the authentication tag to the encrypted data
    buffer.extend_from_slice(&tag);

    Ok(buffer)
}

/// Hash a password using Argon2id
pub async fn hash_pass(password: String) -> [u8; 32] {
    // 150000 KiB memory size
    let memory = 150000;
    // 10 iterations
    let iterations = 10;
    let params = Params::new(memory, iterations, 1, Some(32)).unwrap();

    // TODO: create a random on-disk salt at first init
    // However, with the nature of this being local software, it won't help a user whom has their system compromised in the first place
    let salt = "vectorvectovectvecvev".as_bytes();

    // Prepare derivation
    let argon = Argon2::new(argon2::Algorithm::Argon2id, Version::V0x13, params);
    let mut key: [u8; 32] = [0; 32];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .unwrap();

    key
}

/// Internal function for encryption logic using ChaCha20Poly1305
pub async fn internal_encrypt(input: String, password: Option<String>) -> String {
    // Hash our password with Argon2 and use it as the key
    let key: [u8; 32] = if password.is_none() {
        // Read the cached key
        let guard = crate::ENCRYPTION_KEY.read().unwrap();
        *guard.as_ref().expect("Encryption key must be set")
    } else {
        hash_pass(password.unwrap()).await
    };

    // Generate a random 12-byte nonce
    let mut rng = rand::thread_rng();
    let nonce_bytes: [u8; 12] = rng.gen();

    // Create the cipher instance
    let cipher = ChaCha20Poly1305::new_from_slice(&key)
        .expect("Key should be valid");

    // Create the nonce
    let nonce: Nonce = nonce_bytes.into();

    // Encrypt the input
    let ciphertext = cipher
        .encrypt(&nonce, input.as_bytes())
        .expect("Encryption should not fail");

    // Prepend the nonce to our ciphertext
    let mut buffer = Vec::with_capacity(nonce_bytes.len() + ciphertext.len());
    buffer.extend_from_slice(&nonce_bytes);
    buffer.extend_from_slice(&ciphertext);

    // Cache the key if not already set
    {
        let mut guard = crate::ENCRYPTION_KEY.write().unwrap();
        if guard.is_none() {
            *guard = Some(key);
        }
    }

    // Convert the encrypted bytes to a hex string for safe storage/transmission
    bytes_to_hex_string(&buffer)
}

/// Internal function for decryption logic using ChaCha20Poly1305
pub async fn internal_decrypt(ciphertext: String, password: Option<String>) -> Result<String, ()> {
    // Check if we're using a password before we potentially move it
    let has_password = password.is_some();

    // Get the key - either from password or cached
    let key: [u8; 32] = if let Some(pass) = password {
        // Hash the password
        hash_pass(pass).await
    } else {
        // Try to use cached key
        let guard = crate::ENCRYPTION_KEY.read().unwrap();
        match guard.as_ref() {
            Some(k) => *k,
            None => return Err(()),
        }
    };

    // Convert hex to bytes - use reference to avoid copying the string
    let encrypted_data = match hex_string_to_bytes(ciphertext.as_str()) {
        bytes if bytes.len() >= 12 => bytes,
        _ => return Err(())
    };

    // Extract nonce and encrypted data - use slices to avoid copying data
    let (nonce_bytes, actual_ciphertext) = encrypted_data.split_at(12);

    // Create the cipher instance
    let cipher = match ChaCha20Poly1305::new_from_slice(&key) {
        Ok(c) => c,
        Err(_) => return Err(())
    };

    // Create the nonce and decrypt
    let nonce_arr: [u8; 12] = nonce_bytes.try_into().map_err(|_| ())?;
    let nonce: Nonce = nonce_arr.into();
    let plaintext = match cipher.decrypt(&nonce, actual_ciphertext) {
        Ok(pt) => pt,
        Err(_) => return Err(())
    };

    // Cache the key if needed - only set if we came from password path
    if has_password {
        let mut guard = crate::ENCRYPTION_KEY.write().unwrap();
        if guard.is_none() {
            *guard = Some(key);
        }
    }

    // Convert decrypted bytes to string using unsafe version, because SPEED!
    // SAFETY: The plaintext bytes are guaranteed to be valid UTF-8, making this safe, because:
    // 1. They were originally created from a valid UTF-8 string (typically JSON or plaintext)
    // 2. ChaCha20-Poly1305 authenticated decryption ensures the data is intact
    // 3. The decryption process preserves the exact byte patterns
    unsafe {
        Ok(String::from_utf8_unchecked(plaintext))
    }
}

/// Encrypt with an explicit key (for re-keying — doesn't touch ENCRYPTION_KEY global)
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

/// Decrypt with an explicit key (for re-keying — doesn't touch ENCRYPTION_KEY global)
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

    unsafe { Ok(String::from_utf8_unchecked(plaintext)) }
}

// ============================================================================
// Conditional Encryption Helpers - Check encryption_enabled setting
// ============================================================================

/// Check if local encryption is enabled.
/// Uses cached AtomicBool (~1ns) instead of SQLite query (~5-20µs).
/// The cache is initialized at boot and updated by migration functions.
#[inline]
pub fn is_encryption_enabled() -> bool {
    crate::state::is_encryption_enabled_fast()
}

/// Conditionally encrypt content based on encryption_enabled setting.
/// If encryption is disabled, returns the input unchanged.
pub async fn maybe_encrypt(input: String) -> String {
    if is_encryption_enabled() {
        internal_encrypt(input, None).await
    } else {
        input
    }
}

/// Conditionally decrypt content based on encryption_enabled setting.
/// Handles crash recovery gracefully - if decryption fails on non-encrypted-looking content,
/// returns it as-is (handles partially-migrated state from interrupted migrations).
///
/// IMPORTANT: This function is designed to be crash-safe:
/// - If migration crashes mid-way, some content may be plaintext while encryption_enabled=true
/// - We detect this by checking if failed-to-decrypt content "looks encrypted"
/// - If it doesn't look encrypted, it's probably already-decrypted plaintext → return as-is
/// - If it does look encrypted but decryption failed, that's a genuine error → propagate
pub async fn maybe_decrypt(input: String) -> Result<String, ()> {
    if is_encryption_enabled() {
        // Encryption enabled - try to decrypt
        match internal_decrypt(input.clone(), None).await {
            Ok(decrypted) => Ok(decrypted),
            Err(_) => {
                // Decryption failed - check if content actually looks encrypted
                if looks_encrypted(&input) {
                    // Looks encrypted but failed to decrypt - genuine error
                    // (corrupted data, wrong key, etc.)
                    Err(())
                } else {
                    // Doesn't look encrypted - probably plaintext from crash recovery
                    // This handles the case where migration crashed mid-way:
                    // some content was already decrypted but encryption_enabled wasn't set to false yet
                    Ok(input)
                }
            }
        }
    } else {
        // Encryption disabled - but check if this specific content might still be encrypted
        // (for backwards compatibility during migration or mixed states)
        if looks_encrypted(&input) {
            // Attempt decryption, but gracefully return original if it fails
            // This handles two cases:
            // 1. Legitimately encrypted content from before migration completed
            // 2. User-sent hex content (tx IDs, hashes) that isn't actually encrypted
            match internal_decrypt(input.clone(), None).await {
                Ok(decrypted) => Ok(decrypted),
                Err(_) => Ok(input), // Not actually encrypted, return as-is
            }
        } else {
            Ok(input)
        }
    }
}

/// Check if a string looks like encrypted content (hex-encoded ChaCha20 output).
/// Encrypted content format: 12-byte nonce + ciphertext + 16-byte auth tag, all hex-encoded.
/// Minimum (empty message): 12 + 0 + 16 = 28 bytes = 56 hex chars.
///
/// Strictly lowercase: our encryption always outputs lowercase hex via bytes_to_hex_string.
/// Rejecting uppercase reduces false positives on user-sent content (tx IDs, pubkeys, etc.).
///
/// # Performance
/// - NEON (ARM64): ~2 ns for 80B, ~8 ns for 320B (12-14x faster than LUT)
/// - SSE2 (x86_64): comparable gains via 16-byte range checks
/// - Scalar fallback: ~27 ns for 80B (LUT-based)
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
            // is_digit = (b >= '0') & (b <= '9')
            let is_digit = vandq_u8(vcgeq_u8(chunk, vdupq_n_u8(b'0')),
                                    vcleq_u8(chunk, vdupq_n_u8(b'9')));
            // is_af = (b >= 'a') & (b <= 'f')
            let is_af = vandq_u8(vcgeq_u8(chunk, vdupq_n_u8(b'a')),
                                 vcleq_u8(chunk, vdupq_n_u8(b'f')));
            // All lanes must be 0xFF
            if vminvq_u8(vorrq_u8(is_digit, is_af)) == 0 { return false; }
            i += 16;
        }
        // Scalar remainder
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
            // is_digit: (b - '0') <= 9  (unsigned: subs_epu8 saturates to 0 if in range)
            let is_digit = _mm_cmpeq_epi8(
                _mm_subs_epu8(_mm_sub_epi8(chunk, _mm_set1_epi8(b'0' as i8)), _mm_set1_epi8(9)),
                _mm_setzero_si128());
            // is_af: (b - 'a') <= 5
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

// ============================================================================
// AES-256-GCM Decryption for File Attachments
// ============================================================================

pub fn decrypt_data(encrypted_data: &[u8], key_hex: &str, nonce_hex: &str) -> Result<Vec<u8>, String> {
    // Verify minimum size requirements (need at least 16 bytes for the authentication tag)
    if encrypted_data.len() < 16 {
        return Err(format!("Invalid Input: encrypted data too small ({} bytes, minimum 16 bytes required for authentication tag)", encrypted_data.len()));
    }

    // Decode key and nonce from hex
    let key_bytes = hex_string_to_bytes(key_hex);
    let nonce_bytes = hex_string_to_bytes(nonce_hex);

    // Split input into ciphertext and authentication tag
    let (ciphertext, tag_bytes) = encrypted_data.split_at(encrypted_data.len() - 16);

    // Initialize AES-GCM cipher
    let cipher = AesGcm::<Aes256, U16>::new_from_slice(&key_bytes)
        .map_err(|_| String::from("Invalid decryption key"))?;

    // Prepare nonce and tag
    let nonce_arr: [u8; 16] = nonce_bytes.try_into()
        .map_err(|_| String::from("Invalid nonce length"))?;
    let nonce = aes_gcm::Nonce::<U16>::from(nonce_arr);
    let tag_arr: [u8; 16] = tag_bytes.try_into()
        .map_err(|_| String::from("Invalid tag length"))?;
    let tag = aes_gcm::Tag::<U16>::from(tag_arr);

    // Create output buffer
    let mut buffer = ciphertext.to_vec();

    // Perform decryption
    let decryption = cipher
        .decrypt_in_place_detached(&nonce, &[], &mut buffer, &tag);

    // Check that it went well
    if decryption.is_err() {
        return Err(decryption.unwrap_err().to_string());
    }

    Ok(buffer)
}

