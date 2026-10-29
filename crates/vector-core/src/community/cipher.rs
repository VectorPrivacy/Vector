//! Raw-key NIP-44 v2 sealing with a nonce no call site can choose.

use nostr_sdk::prelude::nip44::v2::ConversationKey;
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
    crate::crypto::nip44::encrypt_with_nonce(ck, plaintext, nonce).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::nip44::v2::decrypt_to_bytes;

    #[test]
    fn round_trips_and_never_repeats_a_nonce() {
        let ck = ConversationKey::new([0x5au8; 32]);
        let a = encrypt_with_random_nonce(&ck, b"hello community").unwrap();
        let b = encrypt_with_random_nonce(&ck, b"hello community").unwrap();
        assert_ne!(a, b);
        assert_eq!(decrypt_to_bytes(&ck, &a).unwrap(), b"hello community");
        assert!(decrypt_to_bytes(&ConversationKey::new([1u8; 32]), &a).is_err());
    }
}
