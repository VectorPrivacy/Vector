pub mod guarded_key;
pub use guarded_key::GuardedKey;

mod signer;
pub use signer::GuardedSigner;

use argon2::Argon2;
use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit};
use zeroize::Zeroize;

/// Derive a 32-byte key from a password using Argon2id.
/// Parameters: 150MB memory, 10 iterations (matches src-tauri).
pub async fn hash_pass(password: &str) -> [u8; 32] {
    let password = password.to_string();
    tokio::task::spawn_blocking(move || {
        let salt = b"vectorvectovectvecvev";
        let mut output = [0u8; 32];

        let params = argon2::Params::new(
            150_000, // 150 MB
            10,      // iterations
            1,       // parallelism
            Some(32),
        ).unwrap();

        let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        argon2.hash_password_into(password.as_bytes(), salt, &mut output).unwrap();

        output
    }).await.unwrap()
}

/// Encrypt a string with the global ENCRYPTION_KEY (ChaCha20-Poly1305).
pub fn encrypt_with_key(plaintext: &str, key: &[u8; 32]) -> Result<String, String> {
    use chacha20poly1305::aead::OsRng;
    use chacha20poly1305::AeadCore;

    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);

    let ciphertext = cipher.encrypt(&nonce, plaintext.as_bytes())
        .map_err(|e| format!("Encryption failed: {}", e))?;

    // Encode as hex: nonce + ciphertext
    let mut result = hex::encode(&nonce[..]);
    result.push_str(&hex::encode(&ciphertext));
    Ok(result)
}

/// Decrypt a hex-encoded ciphertext with a key.
pub fn decrypt_with_key(hex_data: &str, key: &[u8; 32]) -> Result<String, String> {
    if hex_data.len() < 24 {
        return Err("Ciphertext too short".to_string());
    }

    let nonce_hex = &hex_data[..24]; // 12 bytes = 24 hex chars
    let ciphertext_hex = &hex_data[24..];

    let nonce_bytes = hex::decode(nonce_hex)
        .map_err(|e| format!("Invalid nonce hex: {}", e))?;
    let ciphertext = hex::decode(ciphertext_hex)
        .map_err(|e| format!("Invalid ciphertext hex: {}", e))?;

    let nonce_arr: [u8; 12] = nonce_bytes.try_into()
        .map_err(|_| "Invalid nonce length".to_string())?;
    let nonce = chacha20poly1305::Nonce::from(nonce_arr);
    let cipher = ChaCha20Poly1305::new(key.into());

    let mut plaintext = cipher.decrypt(&nonce, ciphertext.as_ref())
        .map_err(|_| "Decryption failed (wrong key or corrupted data)".to_string())?;

    let result = String::from_utf8(plaintext.clone())
        .map_err(|_| "Decrypted data is not valid UTF-8".to_string())?;

    plaintext.zeroize();
    Ok(result)
}

/// Check if encryption is enabled in the database.
pub fn is_encryption_enabled() -> bool {
    crate::db::get_sql_setting("encryption_enabled".to_string())
        .ok().flatten()
        .map(|v| v != "false")
        .unwrap_or(false)
}

/// Simple hex encode/decode (for crypto module internal use).
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }

    pub fn decode(hex: &str) -> Result<Vec<u8>, String> {
        if hex.len() % 2 != 0 {
            return Err("Odd-length hex string".to_string());
        }
        (0..hex.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&hex[i..i + 2], 16)
                    .map_err(|e| format!("Invalid hex: {}", e))
            })
            .collect()
    }
}

// ============================================================================
// AES-256-GCM File Encryption (for NIP-96/Blossom attachments)
// ============================================================================

/// Parameters for AES-256-GCM file encryption (hex-encoded key + nonce).
#[derive(Debug)]
pub struct EncryptionParams {
    pub key: String,   // 32-byte key as hex
    pub nonce: String, // 16-byte nonce as hex (0xChat-compatible)
}

/// Generate random AES-256-GCM encryption parameters.
pub fn generate_encryption_params() -> EncryptionParams {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut key: [u8; 32] = rng.gen();
    let nonce: [u8; 16] = rng.gen();
    let params = EncryptionParams {
        key: hex::encode(&key),
        nonce: hex::encode(&nonce),
    };
    key.iter_mut().for_each(|b| *b = 0); // zeroize
    params
}

/// Encrypt data with AES-256-GCM using a 16-byte nonce (0xChat-compatible).
pub fn encrypt_data(data: &[u8], params: &EncryptionParams) -> Result<Vec<u8>, String> {
    use aes::Aes256;
    use aes::cipher::typenum::U16;
    use aes_gcm::{AesGcm, AeadInPlace, KeyInit as AesKeyInit};

    let key_bytes = hex::decode(&params.key).map_err(|e| format!("Invalid key: {}", e))?;
    let nonce_bytes = hex::decode(&params.nonce).map_err(|e| format!("Invalid nonce: {}", e))?;

    let cipher = AesGcm::<Aes256, U16>::new_from_slice(&key_bytes)
        .map_err(|_| "Invalid encryption key".to_string())?;

    let nonce_arr: [u8; 16] = nonce_bytes.try_into()
        .map_err(|_| "Invalid nonce length".to_string())?;
    let nonce = aes_gcm::Nonce::<U16>::from(nonce_arr);

    let mut buffer = data.to_vec();
    let tag = cipher.encrypt_in_place_detached(&nonce, &[], &mut buffer)
        .map_err(|_| "Encryption failed".to_string())?;

    buffer.extend_from_slice(&tag);
    Ok(buffer)
}

/// Calculate SHA-256 hash of data, returned as hex string.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(&hasher.finalize())
}

/// Get MIME type from file extension.
pub fn mime_from_extension(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "xdc" => "application/xdc+zip",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // encrypt_with_key / decrypt_with_key roundtrip tests
    // ========================================================================

    fn test_key() -> [u8; 32] {
        [0x42u8; 32]
    }

    fn alt_key() -> [u8; 32] {
        [0x99u8; 32]
    }

    #[test]
    fn encrypt_decrypt_roundtrip_simple() {
        let key = test_key();
        let plaintext = "hello world";
        let encrypted = encrypt_with_key(plaintext, &key).expect("encryption should succeed");
        let decrypted = decrypt_with_key(&encrypted, &key).expect("decryption should succeed");
        assert_eq!(decrypted, plaintext, "roundtrip should preserve plaintext");
    }

    #[test]
    fn encrypt_decrypt_100_random_strings() {
        use rand::Rng;
        let key = test_key();
        let mut rng = rand::thread_rng();
        for i in 0..100 {
            let len = rng.gen_range(1..=200);
            let s: String = (0..len).map(|_| rng.gen_range(0x20u8..0x7f) as char).collect();
            let enc = encrypt_with_key(&s, &key)
                .unwrap_or_else(|e| panic!("encryption failed on iteration {}: {}", i, e));
            let dec = decrypt_with_key(&enc, &key)
                .unwrap_or_else(|e| panic!("decryption failed on iteration {}: {}", i, e));
            assert_eq!(dec, s, "roundtrip failed on iteration {}", i);
        }
    }

    #[test]
    fn encrypt_decrypt_empty_string() {
        let key = test_key();
        let encrypted = encrypt_with_key("", &key).expect("encrypting empty string should succeed");
        let decrypted = decrypt_with_key(&encrypted, &key).expect("decrypting empty string should succeed");
        assert_eq!(decrypted, "", "empty string roundtrip should produce empty string");
    }

    #[test]
    fn encrypt_decrypt_large_string() {
        let key = test_key();
        let plaintext = "A".repeat(10 * 1024); // 10 KB
        let encrypted = encrypt_with_key(&plaintext, &key).expect("encrypting 10KB should succeed");
        let decrypted = decrypt_with_key(&encrypted, &key).expect("decrypting 10KB should succeed");
        assert_eq!(decrypted, plaintext, "10KB roundtrip should preserve content");
    }

    #[test]
    fn decrypt_with_wrong_key_fails() {
        let key = test_key();
        let wrong_key = alt_key();
        let encrypted = encrypt_with_key("secret data", &key).expect("encryption should succeed");
        let result = decrypt_with_key(&encrypted, &wrong_key);
        assert!(result.is_err(), "decryption with wrong key should fail");
    }

    #[test]
    fn decrypt_corrupted_ciphertext_fails() {
        let key = test_key();
        let mut encrypted = encrypt_with_key("secret data", &key).expect("encryption should succeed");
        // Corrupt a byte in the ciphertext portion (past the 24-char nonce)
        let bytes: Vec<u8> = encrypted.bytes().collect();
        if bytes.len() > 30 {
            let mut chars: Vec<char> = encrypted.chars().collect();
            // Flip a hex digit in the ciphertext area
            chars[30] = if chars[30] == '0' { 'f' } else { '0' };
            encrypted = chars.into_iter().collect();
        }
        let result = decrypt_with_key(&encrypted, &key);
        assert!(result.is_err(), "decryption of corrupted ciphertext should fail");
    }

    #[test]
    fn different_keys_produce_different_ciphertext() {
        let key1 = test_key();
        let key2 = alt_key();
        let plaintext = "same plaintext";
        let enc1 = encrypt_with_key(plaintext, &key1).expect("enc1 should succeed");
        let enc2 = encrypt_with_key(plaintext, &key2).expect("enc2 should succeed");
        // Ciphertexts after the nonce portion should differ (nonces differ too since random)
        assert_ne!(enc1, enc2, "different keys should produce different ciphertext");
    }

    #[test]
    fn nonce_is_always_different() {
        let key = test_key();
        let plaintext = "same string encrypted twice";
        let enc1 = encrypt_with_key(plaintext, &key).expect("enc1 should succeed");
        let enc2 = encrypt_with_key(plaintext, &key).expect("enc2 should succeed");
        // The first 24 hex chars are the nonce
        let nonce1 = &enc1[..24];
        let nonce2 = &enc2[..24];
        assert_ne!(nonce1, nonce2, "nonces should differ between encryptions of the same plaintext");
    }

    #[test]
    fn unicode_content_preserved() {
        let key = test_key();
        let plaintext = "Hello \u{1F600} \u{1F4A9} \u{1F30D} \u{00E9}\u{00E0}\u{00FC} \u{4E16}\u{754C} \u{0410}\u{0411}\u{0412}";
        let encrypted = encrypt_with_key(plaintext, &key).expect("encrypting unicode should succeed");
        let decrypted = decrypt_with_key(&encrypted, &key).expect("decrypting unicode should succeed");
        assert_eq!(decrypted, plaintext, "unicode content should be preserved through encrypt/decrypt");
    }

    #[test]
    fn decrypt_too_short_ciphertext_fails() {
        let key = test_key();
        let result = decrypt_with_key("abcdef", &key);
        assert!(result.is_err(), "ciphertext shorter than 24 hex chars should fail");
        assert!(result.unwrap_err().contains("too short"), "error should mention too short");
    }

    #[test]
    fn encrypt_output_is_hex_encoded() {
        let key = test_key();
        let encrypted = encrypt_with_key("test", &key).expect("encryption should succeed");
        assert!(encrypted.chars().all(|c| c.is_ascii_hexdigit()),
            "encrypted output should be entirely hex characters");
    }

    #[test]
    fn encrypt_output_has_correct_structure() {
        let key = test_key();
        let encrypted = encrypt_with_key("test", &key).expect("encryption should succeed");
        // Must be at least 24 hex chars (nonce) + some ciphertext
        assert!(encrypted.len() > 24,
            "encrypted output should have nonce (24 hex chars) plus ciphertext");
        // Length should be even (hex pairs)
        assert_eq!(encrypted.len() % 2, 0,
            "encrypted output length should be even (hex pairs)");
    }

    #[test]
    fn encrypt_decrypt_special_characters() {
        let key = test_key();
        let plaintext = r#"!@#$%^&*()_+-=[]{}|;':",.<>?/\`~"#;
        let encrypted = encrypt_with_key(plaintext, &key).expect("encrypting special chars should succeed");
        let decrypted = decrypt_with_key(&encrypted, &key).expect("decrypting special chars should succeed");
        assert_eq!(decrypted, plaintext, "special characters should survive roundtrip");
    }

    #[test]
    fn encrypt_decrypt_newlines_and_whitespace() {
        let key = test_key();
        let plaintext = "line1\nline2\r\nline3\ttab\0null";
        let encrypted = encrypt_with_key(plaintext, &key).expect("encrypting whitespace should succeed");
        let decrypted = decrypt_with_key(&encrypted, &key).expect("decrypting whitespace should succeed");
        assert_eq!(decrypted, plaintext, "whitespace and control chars should survive roundtrip");
    }

    #[test]
    fn decrypt_invalid_hex_in_nonce_fails() {
        let key = test_key();
        // 24 chars of invalid hex + some ciphertext
        let bad = "zzzzzzzzzzzzzzzzzzzzzzzz0000000000000000";
        let result = decrypt_with_key(bad, &key);
        assert!(result.is_err(), "invalid hex in nonce should fail");
    }

    #[test]
    fn decrypt_invalid_hex_in_ciphertext_fails() {
        let key = test_key();
        // Valid nonce (24 hex chars) + invalid ciphertext hex
        let bad = "000000000000000000000000gggggggg";
        let result = decrypt_with_key(bad, &key);
        assert!(result.is_err(), "invalid hex in ciphertext should fail");
    }

    // ========================================================================
    // hex module tests
    // ========================================================================

    #[test]
    fn hex_encode_decode_roundtrip() {
        let data = vec![0x00, 0x01, 0xff, 0x80, 0x7f];
        let encoded = hex::encode(&data);
        let decoded = hex::decode(&encoded).expect("decode should succeed");
        assert_eq!(decoded, data, "hex encode/decode roundtrip should preserve bytes");
    }

    #[test]
    fn hex_decode_odd_length_error() {
        let result = hex::decode("abc");
        assert!(result.is_err(), "odd-length hex string should fail");
        assert!(result.unwrap_err().contains("Odd-length"), "error should mention odd-length");
    }

    #[test]
    fn hex_decode_invalid_hex_error() {
        let result = hex::decode("zzzz");
        assert!(result.is_err(), "invalid hex characters should fail");
    }

    #[test]
    fn hex_encode_empty() {
        assert_eq!(hex::encode(&[]), "", "encoding empty bytes should produce empty string");
    }

    #[test]
    fn hex_decode_empty() {
        let decoded = hex::decode("").expect("decoding empty string should succeed");
        assert!(decoded.is_empty(), "decoding empty string should produce empty vec");
    }

    // ========================================================================
    // hash_pass tests
    // ========================================================================

    #[tokio::test]
    async fn hash_pass_deterministic() {
        let key1 = hash_pass("my_password").await;
        let key2 = hash_pass("my_password").await;
        assert_eq!(key1, key2, "same password should always produce the same key");
    }

    #[tokio::test]
    async fn hash_pass_different_passwords_different_keys() {
        let key1 = hash_pass("password_one").await;
        let key2 = hash_pass("password_two").await;
        assert_ne!(key1, key2, "different passwords should produce different keys");
    }

    #[tokio::test]
    async fn hash_pass_output_is_32_bytes() {
        let key = hash_pass("test_password").await;
        assert_eq!(key.len(), 32, "hash_pass should produce exactly 32 bytes");
        // Ensure it is not all zeros (i.e. hashing actually happened)
        assert!(key.iter().any(|&b| b != 0), "hash output should not be all zeros");
    }
}
