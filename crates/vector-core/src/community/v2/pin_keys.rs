//! NIP-44 v2 per-message key disclosure — the primitive Pins are built on
//! (CORD-04 §7).
//!
//! NIP-44 v2 never encrypts two messages under the same key: it derives
//! per-message keys as `hkdf-expand(conversation_key, nonce, 76)`, split
//! `chacha_key[32] || chacha_nonce[12] || hmac_key[32]`. That expansion is
//! one-way, so disclosing ONE message's 76 bytes exposes exactly that message —
//! never the conversation key, the epoch, or the author's other traffic.
//!
//! The nostr crate keeps its message-key expansion private, so it is reproduced
//! here over the same audited RustCrypto primitives and round-tripped against
//! nostr's own encrypt in tests. This file is wire format shared with Armada's
//! `nip44keys.ts`: a divergence here silently breaks pin verification across
//! clients.

use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha20;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Serialized disclosure length: 32 + 12 + 32.
pub const MESSAGE_KEYS_BYTES: usize = 76;

/// The disclosed material: exactly one message's keys.
#[derive(Clone)]
pub struct MessageKeys {
    pub chacha_key: [u8; 32],
    pub chacha_nonce: [u8; 12],
    pub hmac_key: [u8; 32],
}

/// The per-message expansion. Requires the conversation key, so only a member
/// holding the channel key at that epoch can produce a disclosure.
///
/// NIP-44's conversation key IS the HKDF PRK (extraction happened at ECDH), so
/// this is expand-only — `from_prk`, not `new`.
pub fn get_message_keys(conversation_key: &[u8; 32], nonce: &[u8]) -> MessageKeys {
    let hk = Hkdf::<Sha256>::from_prk(conversation_key).expect("32-byte PRK is always valid");
    let mut okm = [0u8; MESSAGE_KEYS_BYTES];
    hk.expand(nonce, &mut okm).expect("76 bytes is within HKDF-SHA256 bounds");
    MessageKeys {
        chacha_key: okm[0..32].try_into().unwrap(),
        chacha_nonce: okm[32..44].try_into().unwrap(),
        hmac_key: okm[44..76].try_into().unwrap(),
    }
}

/// Serialize a disclosure as the wire's 76-byte lowercase hex.
pub fn encode_message_keys(keys: &MessageKeys) -> String {
    let mut packed = [0u8; MESSAGE_KEYS_BYTES];
    packed[0..32].copy_from_slice(&keys.chacha_key);
    packed[32..44].copy_from_slice(&keys.chacha_nonce);
    packed[44..76].copy_from_slice(&keys.hmac_key);
    crate::simd::hex::bytes_to_hex_string(&packed)
}

/// Parse a 76-byte lowercase-hex disclosure; `None` if malformed. Uppercase is
/// refused: the wire form is canonical, and two encodings of one disclosure
/// would break entry-identity caching.
pub fn decode_message_keys(hex: &str) -> Option<MessageKeys> {
    if hex.len() != MESSAGE_KEYS_BYTES * 2 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let bytes = crate::simd::hex::hex_string_to_bytes_checked(hex)?;
    Some(MessageKeys {
        chacha_key: bytes[0..32].try_into().ok()?,
        chacha_nonce: bytes[32..44].try_into().ok()?,
        hmac_key: bytes[44..76].try_into().ok()?,
    })
}

/// A NIP-44 v2 payload split into its public parts.
struct Payload {
    nonce: [u8; 32],
    ciphertext: Vec<u8>,
    mac: [u8; 32],
}

/// Decode a NIP-44 v2 payload; `None` if malformed. Mirrors the reference
/// decoder: min length 132, `#` prefix refused (an unsupported-version escape),
/// decoded form `version(1) || nonce(32) || ciphertext || mac(32)` with
/// version byte 2 and at least one ciphertext byte... the 99-byte floor.
fn decode_payload(payload: &str) -> Option<Payload> {
    if payload.len() < 132 || payload.starts_with('#') {
        return None;
    }
    let data = base64_simd::STANDARD.decode_to_vec(payload.as_bytes()).ok()?;
    if data.len() < 99 || data[0] != 2 {
        return None;
    }
    let mac_at = data.len() - 32;
    Some(Payload {
        nonce: data[1..33].try_into().ok()?,
        ciphertext: data[33..mac_at].to_vec(),
        mac: data[mac_at..].try_into().ok()?,
    })
}

/// Open a NIP-44 v2 payload using DISCLOSED keys instead of the conversation
/// key — the reader half of a pin's proof. `None` on any failure (malformed
/// payload, MAC mismatch, bad padding), never a panic: a hostile entry is
/// dropped, not an exception.
///
/// The MAC is `hmac(sha256, hmac_key, nonce || ciphertext)`, and both nonce and
/// ciphertext ride in the payload itself, so the disclosed keys are the only
/// secret input — which is exactly what makes a pin verifiable by a member who
/// holds none of the channel's history.
pub fn decrypt_with_disclosed_keys(payload: &str, keys: &MessageKeys) -> Option<String> {
    let decoded = decode_payload(payload)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&keys.hmac_key).ok()?;
    mac.update(&decoded.nonce);
    mac.update(&decoded.ciphertext);
    // Constant-time comparison — the verify path must not become a MAC oracle.
    mac.verify_slice(&decoded.mac).ok()?;

    let mut padded = decoded.ciphertext;
    let mut cipher = ChaCha20::new((&keys.chacha_key).into(), (&keys.chacha_nonce).into());
    cipher.apply_keystream(&mut padded);
    unpad(&padded)
}

/// Produce the disclosure for one already-encrypted payload. Requires the
/// conversation key — i.e. the caller can read the message they are pinning.
pub fn disclose_keys_for(payload: &str, conversation_key: &[u8; 32]) -> Option<MessageKeys> {
    let decoded = decode_payload(payload)?;
    Some(get_message_keys(conversation_key, &decoded.nonce))
}

/// Unpad a decrypted NIP-44 plaintext; `None` if the padding is invalid.
/// Big-endian u16 length prefix, or `0x0000` + u32 for plaintexts ≥ 65536 —
/// and the total padded length must equal exactly what that length pads to.
fn unpad(padded: &[u8]) -> Option<String> {
    if padded.len() < 2 {
        return None;
    }
    let first_two = u16::from_be_bytes([padded[0], padded[1]]) as usize;
    let (unpadded_len, prefix_len) = if first_two == 0 {
        if padded.len() < 6 {
            return None;
        }
        let long = u32::from_be_bytes([padded[2], padded[3], padded[4], padded[5]]) as usize;
        if long < 65536 {
            return None;
        }
        (long, 6usize)
    } else {
        (first_two, 2usize)
    };
    let unpadded = padded.get(prefix_len..prefix_len.checked_add(unpadded_len)?)?;
    if unpadded_len < 1 || padded.len() != prefix_len + calc_padded_len(unpadded_len)? {
        return None;
    }
    String::from_utf8(unpadded.to_vec()).ok()
}

/// NIP-44's padded length for a plaintext of `len` bytes.
fn calc_padded_len(len: usize) -> Option<usize> {
    if len < 1 {
        return None;
    }
    if len <= 32 {
        return Some(32);
    }
    // next_power = 2^(floor(log2(len - 1)) + 1)
    let next_power = 1usize.checked_shl(usize::BITS - ((len - 1).leading_zeros()))?;
    let chunk = if next_power <= 256 { 32 } else { next_power / 8 };
    Some(chunk * ((len - 1) / chunk + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::nip44::v2::{self, ConversationKey};

    fn conv_key() -> ([u8; 32], ConversationKey) {
        let keys = nostr_sdk::prelude::Keys::generate();
        let ck = ConversationKey::derive(keys.secret_key(), &keys.public_key()).unwrap();
        let bytes: [u8; 32] = ck.as_bytes().try_into().unwrap();
        (bytes, ck)
    }

    /// The load-bearing claim: our expand + our decrypt open what the nostr
    /// crate's own encrypt produced, via disclosure alone.
    #[test]
    fn disclosed_keys_open_a_nostr_encrypted_payload() {
        let (bytes, ck) = conv_key();
        let long = "long ".repeat(500);
        for msg in ["a", "hello world", long.as_str(), "emoji 🐱 and unicode ünïcode"] {
            let payload = nostr_encrypt(&ck, msg.as_bytes());
            let keys = disclose_keys_for(&payload, &bytes).expect("disclosable");
            assert_eq!(
                decrypt_with_disclosed_keys(&payload, &keys).as_deref(),
                Some(msg),
                "round-trip failed for {} bytes",
                msg.len()
            );
        }
    }

    /// One message's disclosure must open nothing else.
    #[test]
    fn a_disclosure_is_scoped_to_its_message() {
        let (bytes, ck) = conv_key();
        let p1 = nostr_encrypt(&ck, b"first");
        let p2 = nostr_encrypt(&ck, b"second");
        let k1 = disclose_keys_for(&p1, &bytes).unwrap();
        assert!(decrypt_with_disclosed_keys(&p2, &k1).is_none());
    }

    #[test]
    fn tampered_payload_fails_the_mac() {
        let (bytes, ck) = conv_key();
        let raw = v2::encrypt_to_bytes_with_nonce(&ck, b"pin me", rand_nonce()).unwrap();
        let keys = disclose_keys_for(&base64_simd::STANDARD.encode_to_string(&raw), &bytes).unwrap();
        // Flip one ciphertext bit and re-encode.
        let mut bad = raw.clone();
        bad[40] ^= 1;
        assert!(decrypt_with_disclosed_keys(&base64_simd::STANDARD.encode_to_string(&bad), &keys).is_none());
    }

    #[test]
    fn keys_hex_round_trips_and_refuses_noncanonical() {
        let (bytes, _) = conv_key();
        let keys = get_message_keys(&bytes, &[7u8; 32]);
        let hex = encode_message_keys(&keys);
        assert_eq!(hex.len(), 152);
        let back = decode_message_keys(&hex).unwrap();
        assert_eq!(back.chacha_key, keys.chacha_key);
        assert_eq!(back.chacha_nonce, keys.chacha_nonce);
        assert_eq!(back.hmac_key, keys.hmac_key);
        // Uppercase, short, and non-hex all refused.
        assert!(decode_message_keys(&hex.to_uppercase()).is_none());
        assert!(decode_message_keys(&hex[..150]).is_none());
        assert!(decode_message_keys(&format!("zz{}", &hex[2..])).is_none());
    }

    #[test]
    fn malformed_payloads_are_refused_not_panicked() {
        let (bytes, _) = conv_key();
        let keys = get_message_keys(&bytes, &[1u8; 32]);
        for bad in ["", "#v3payload", "AAAA", &"A".repeat(131), &"!not base64!".repeat(20)] {
            assert!(decrypt_with_disclosed_keys(bad, &keys).is_none());
            assert!(disclose_keys_for(bad, &bytes).is_none());
        }
        // Valid base64, right length, wrong version byte.
        let mut fake = vec![1u8; 100];
        fake[0] = 1;
        assert!(decrypt_with_disclosed_keys(&base64_simd::STANDARD.encode_to_string(&fake), &keys).is_none());
    }

    /// Padding edges: exactly-32, the 32/33 chunk boundary, and a large body.
    #[test]
    fn padding_boundaries_round_trip() {
        let (bytes, ck) = conv_key();
        // Top end is the nostr crate's own cap (65536 - 128), not NIP-44's
        // theoretical 65535 — encrypting past it is refused before padding.
        for len in [1usize, 31, 32, 33, 255, 256, 257, 65_408] {
            let msg = "x".repeat(len);
            let payload = nostr_encrypt(&ck, msg.as_bytes());
            let keys = disclose_keys_for(&payload, &bytes).unwrap();
            assert_eq!(
                decrypt_with_disclosed_keys(&payload, &keys).map(|s| s.len()),
                Some(len),
                "len {len}"
            );
        }
    }

    // 32 fresh random bytes without a new dev-dep: a secret key IS one.
    fn rand_nonce() -> [u8; 32] {
        nostr_sdk::prelude::Keys::generate().secret_key().to_secret_bytes()
    }

    // Encrypt under the nostr crate's OWN implementation — what our disclosure
    // path must open.
    fn nostr_encrypt(ck: &ConversationKey, msg: &[u8]) -> String {
        let raw = v2::encrypt_to_bytes_with_nonce(ck, msg, rand_nonce()).expect("encrypt");
        base64_simd::STANDARD.encode_to_string(&raw)
    }
}
