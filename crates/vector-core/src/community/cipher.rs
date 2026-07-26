//! Raw-key NIP-44 v2 sealing — the single symmetric-encryption primitive of the
//! Community protocol. The channel key (message plane) and the server-root key
//! (metadata plane) are both raw 32-byte `ConversationKey`s; ciphertext is
//! base64'd for carriage in an event's string `content` field.

use nostr_sdk::prelude::nip44::v2::{decrypt_to_bytes, encrypt_to_bytes_with_nonce, ConversationKey};
use rand::RngCore;

/// NIP-44 v2 encrypt with a freshly generated nonce.
///
/// The nonce MUST be unique per message under a given conversation key: reuse
/// repeats the ChaCha20 keystream and forfeits both confidentiality and MAC
/// integrity. Drawn from the OS CSPRNG here so no call site can supply its own.
/// Wire output is unchanged (`[2][nonce][ct][mac]`).
pub fn encrypt_with_random_nonce(
    ck: &ConversationKey,
    plaintext: &[u8],
) -> Result<Vec<u8>, String> {
    let mut nonce = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    encrypt_to_bytes_with_nonce(ck, plaintext, nonce).map_err(|e| e.to_string())
}

/// Encrypt `plaintext` under a raw 32-byte key, returning base64 for event content.
pub fn seal(key: &[u8; 32], plaintext: &[u8]) -> Result<String, String> {
    let ck = ConversationKey::new(*key);
    let ciphertext = encrypt_with_random_nonce(&ck, plaintext)?;
    Ok(base64_simd::STANDARD.encode_to_string(&ciphertext))
}

/// Inverse of [`seal`]: base64-decode then NIP-44-decrypt under the raw key. A
/// wrong key or tampered payload fails the MAC and returns `Err`.
pub fn open(key: &[u8; 32], content_b64: &str) -> Result<Vec<u8>, String> {
    let ciphertext = base64_simd::STANDARD
        .decode_to_vec(content_b64.as_bytes())
        .map_err(|e| e.to_string())?;
    let ck = ConversationKey::new(*key);
    decrypt_to_bytes(&ck, &ciphertext).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let key = [0x5au8; 32];
        let sealed = seal(&key, b"hello community").unwrap();
        assert_eq!(open(&key, &sealed).unwrap(), b"hello community");
    }

    #[test]
    fn wrong_key_fails() {
        let sealed = seal(&[1u8; 32], b"secret").unwrap();
        assert!(open(&[2u8; 32], &sealed).is_err());
    }

    #[test]
    fn distinct_ciphertext_per_call() {
        let key = [9u8; 32];
        assert_ne!(seal(&key, b"x").unwrap(), seal(&key, b"x").unwrap());
    }
}
