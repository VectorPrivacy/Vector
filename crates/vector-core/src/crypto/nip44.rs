//! NIP-44 v2 on Vector's own ChaCha20 and SHA-256: the bytes of rust-nostr's `nip44::v2`,
//! decrypting in place in the buffer the payload was decoded into.
//!
//! Like `nip44::v2::decrypt_to_bytes`, decryption leaves the version byte to the caller.

use nostr_sdk::prelude::nip44::v2::ConversationKey;

use super::chachapoly::{chacha20_xor, wipe};
use super::sha256::Sha256;

const NONCE_SIZE: usize = 32;
const MAC_SIZE: usize = 32;
const MIN_PAYLOAD_SIZE: usize = 1 + NONCE_SIZE + 2 + 32 + MAC_SIZE;
const EXTENDED_PREFIX_THRESHOLD: u64 = 65_536;
const MAX_PLAINTEXT_SIZE: u64 = u32::MAX as u64;
const MAX_PAYLOAD_SIZE: u64 = 1 + NONCE_SIZE as u64 + 6 + calc_padding(MAX_PLAINTEXT_SIZE) + MAC_SIZE as u64;
/// The longest base64 payload rust-nostr's `nip44::decrypt` takes.
pub const MAX_ENCODED_PAYLOAD_SIZE: u64 = MAX_PAYLOAD_SIZE.div_ceil(3) * 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    PayloadTooShort,
    MessageEmpty,
    MessageTooLong,
    InvalidHmac,
    InvalidPadding,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::PayloadTooShort => "payload size is too short",
            Error::MessageEmpty => "message empty",
            Error::MessageTooLong => "message too long",
            Error::InvalidHmac => "invalid HMAC",
            Error::InvalidPadding => "invalid padding",
        })
    }
}

impl std::error::Error for Error {}

/// HMAC-SHA256 under one key, its key schedule absorbed once.
#[derive(Clone)]
struct Hmac {
    inner: Sha256,
    outer: Sha256,
}

impl Hmac {
    /// `key` is at most one block: a conversation key or a message's auth key.
    fn new(key: &[u8; 32]) -> Self {
        let mut block = [0u8; 64];
        block[..32].copy_from_slice(key);
        let (mut inner, mut outer) = (Sha256::new(), Sha256::new());
        let mut pad = block.map(|b| b ^ 0x36);
        inner.update(pad);
        pad = block.map(|b| b ^ 0x5c);
        outer.update(pad);
        wipe(&mut pad);
        wipe(&mut block);
        Hmac { inner, outer }
    }

    fn mac(&self, parts: &[&[u8]]) -> [u8; 32] {
        let mut inner = self.inner.clone();
        for part in parts {
            inner.update(part);
        }
        let mut outer = self.outer.clone();
        outer.update(inner.finalize());
        outer.finalize()
    }
}

/// The 76 message key bytes (RFC 5869 expand of the conversation key over the nonce):
/// ChaCha20 key, ChaCha20 nonce, HMAC key.
struct MessageKeys([u8; 96]);

impl MessageKeys {
    fn derive(ck: &ConversationKey, nonce: &[u8]) -> Self {
        let prk = Hmac::new(ck.as_bytes().try_into().expect("a conversation key is 32 bytes"));
        let mut out = [0u8; 96];
        let t1 = prk.mac(&[nonce, &[1]]);
        let t2 = prk.mac(&[&t1, nonce, &[2]]);
        let t3 = prk.mac(&[&t2, nonce, &[3]]);
        out[..32].copy_from_slice(&t1);
        out[32..64].copy_from_slice(&t2);
        out[64..].copy_from_slice(&t3);
        MessageKeys(out)
    }

    fn cipher_key(&self) -> &[u8; 32] {
        self.0[..32].try_into().expect("32 bytes")
    }

    fn cipher_nonce(&self) -> &[u8; 12] {
        self.0[32..44].try_into().expect("12 bytes")
    }

    fn auth(&self) -> Hmac {
        Hmac::new(self.0[44..76].try_into().expect("32 bytes"))
    }
}

impl Drop for MessageKeys {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

const fn calc_padding(len: u64) -> u64 {
    if len <= 32 {
        return 32;
    }
    let nextpower = 1 << ((u64::BITS - 1 - (len - 1).leading_zeros()) + 1);
    let chunk = if nextpower <= 256 { 32 } else { nextpower / 8 };
    chunk * (((len - 1) / chunk) + 1)
}

/// Encrypt `plaintext` under `nonce`: `[2][nonce][length prefix | plaintext | padding][mac]`.
pub fn encrypt_with_nonce(ck: &ConversationKey, plaintext: &[u8], nonce: [u8; 32]) -> Result<Vec<u8>, Error> {
    let len = plaintext.len() as u64;
    if len == 0 {
        return Err(Error::MessageEmpty);
    }
    if len > MAX_PLAINTEXT_SIZE {
        return Err(Error::MessageTooLong);
    }
    let extended = len >= EXTENDED_PREFIX_THRESHOLD;
    let ciphertext_len = if extended { 6 } else { 2 } + calc_padding(len) as usize;
    let mac_at = 1 + NONCE_SIZE + ciphertext_len;
    let mut payload = Vec::with_capacity(mac_at + MAC_SIZE);
    payload.push(2);
    payload.extend_from_slice(&nonce);
    if extended {
        payload.extend_from_slice(&[0, 0]);
        payload.extend_from_slice(&(len as u32).to_be_bytes());
    } else {
        payload.extend_from_slice(&(len as u16).to_be_bytes());
    }
    payload.extend_from_slice(plaintext);
    payload.resize(mac_at, 0);
    let keys = MessageKeys::derive(ck, &nonce);
    chacha20_xor(keys.cipher_key(), keys.cipher_nonce(), 0, &mut payload[1 + NONCE_SIZE..]);
    let mac = keys.auth().mac(&[&payload[1..]]);
    payload.extend_from_slice(&mac);
    Ok(payload)
}

/// Decrypt a decoded v2 `payload`, reusing its buffer for the plaintext.
pub fn decrypt(ck: &ConversationKey, mut payload: Vec<u8>) -> Result<Vec<u8>, Error> {
    let len = payload.len();
    if len < MIN_PAYLOAD_SIZE {
        return Err(Error::PayloadTooShort);
    }
    if len as u64 > MAX_PAYLOAD_SIZE {
        return Err(Error::MessageTooLong);
    }
    let mac_at = len - MAC_SIZE;
    let keys = MessageKeys::derive(ck, &payload[1..1 + NONCE_SIZE]);
    let mac = keys.auth().mac(&[&payload[1..mac_at]]);
    if !mac_eq(&mac, &payload[mac_at..]) {
        return Err(Error::InvalidHmac);
    }
    let body = &mut payload[1 + NONCE_SIZE..mac_at];
    chacha20_xor(keys.cipher_key(), keys.cipher_nonce(), 0, body);
    let framed = (|| {
        let (prefix, unpadded) = plaintext_length(body)?;
        let end = prefix.checked_add(unpadded).ok_or(Error::InvalidPadding)?;
        if end > body.len() {
            return Err(Error::InvalidPadding);
        }
        if unpadded == 0 {
            return Err(Error::MessageEmpty);
        }
        let expected = (prefix as u64).checked_add(calc_padding(unpadded as u64)).ok_or(Error::InvalidPadding)?;
        if body.len() as u64 != expected {
            return Err(Error::InvalidPadding);
        }
        Ok((prefix, unpadded))
    })();
    let (prefix, unpadded) = match framed {
        Ok(framed) => framed,
        Err(e) => {
            // It decrypted: what it holds now is plaintext.
            super::chachapoly::wipe_vec(&mut payload);
            return Err(e);
        }
    };
    let start = 1 + NONCE_SIZE + prefix;
    payload.copy_within(start..start + unpadded, 0);
    payload.truncate(unpadded);
    Ok(payload)
}

fn plaintext_length(body: &[u8]) -> Result<(usize, usize), Error> {
    let short = u16::from_be_bytes(body.get(..2).ok_or(Error::InvalidPadding)?.try_into().expect("2 bytes"));
    if short != 0 {
        return Ok((2, short as usize));
    }
    let wide = u32::from_be_bytes(body.get(2..6).ok_or(Error::InvalidPadding)?.try_into().expect("4 bytes"));
    if (wide as u64) < EXTENDED_PREFIX_THRESHOLD {
        return Err(Error::InvalidPadding);
    }
    Ok((6, usize::try_from(wide).map_err(|_| Error::MessageTooLong)?))
}

fn mac_eq(a: &[u8; 32], b: &[u8]) -> bool {
    b.len() == 32 && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::nip44::v2 as theirs;

    fn lengths() -> impl Iterator<Item = usize> {
        (1..=600).chain([1000, 1023, 1024, 1025, 4096, 8193, 32_768, 65_535, 65_536, 65_537, 100_000, 300_001])
    }

    #[test]
    fn encrypts_byte_for_byte_as_rust_nostr() {
        let mut seed = 0x51u8;
        for len in lengths() {
            seed = seed.wrapping_mul(31).wrapping_add(7);
            let ck = ConversationKey::new([seed; 32]);
            let nonce = [seed ^ 0xa5; 32];
            let plaintext: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(seed)).collect();
            let ours = encrypt_with_nonce(&ck, &plaintext, nonce).unwrap();
            assert_eq!(ours, theirs::encrypt_to_bytes_with_nonce(&ck, &plaintext, nonce).unwrap(), "len {len}");
            assert_eq!(decrypt(&ck, ours.clone()).unwrap(), plaintext, "len {len}");
            assert_eq!(theirs::decrypt_to_bytes(&ck, &ours).unwrap(), plaintext, "len {len}");
        }
        assert_eq!(encrypt_with_nonce(&ConversationKey::new([1; 32]), &[], [0; 32]), Err(Error::MessageEmpty));
    }

    #[test]
    fn refuses_exactly_what_rust_nostr_refuses() {
        let ck = ConversationKey::new([9; 32]);
        let valid = theirs::encrypt_to_bytes_with_nonce(&ck, b"a message long enough to pad past one chunk, and then some more", [3; 32]).unwrap();
        let agree = |payload: &[u8], what: &str| {
            let ours = decrypt(&ck, payload.to_vec());
            let them = theirs::decrypt_to_bytes(&ck, payload);
            assert_eq!(ours.as_ref().ok(), them.as_ref().ok(), "{what}: {ours:?} vs {them:?}");
        };
        // Every single-byte change, the version byte included (neither checks it).
        for i in 0..valid.len() {
            let mut p = valid.clone();
            p[i] ^= 0x40;
            agree(&p, &format!("byte {i}"));
        }
        for cut in [0, 1, 33, 98, 99, 100, valid.len() - 1] {
            agree(&valid[..cut], &format!("cut to {cut}"));
        }
        agree(&[valid.clone(), vec![0; 32]].concat(), "trailing bytes");
        // Correct MACs over plaintexts whose framing lies: a length past the padding, a zero length,
        // an extended length below the threshold, padding of the wrong size.
        let keys = MessageKeys::derive(&ck, &[5; 32]);
        let forge = |body: Vec<u8>| {
            let mut p = vec![2];
            p.extend_from_slice(&[5; 32]);
            let mut b = body;
            chacha20_xor(keys.cipher_key(), keys.cipher_nonce(), 0, &mut b);
            p.extend_from_slice(&b);
            let mac = keys.auth().mac(&[&p[1..]]);
            p.extend_from_slice(&mac);
            p
        };
        for (what, body) in [
            ("length past the padding", [&[0u8, 40][..], &[1; 32]].concat()),
            ("zero length", [&[0u8, 0, 0, 0, 0, 0][..], &[1; 28]].concat()),
            ("short extended length", [&[0u8, 0, 0, 0, 0, 20][..], &[1; 28]].concat()),
            ("too much padding", [&[0u8, 3][..], &[1; 62]].concat()),
            ("exact fit", [&[0u8, 32][..], &[1; 32]].concat()),
        ] {
            agree(&forge(body), what);
        }
    }

    #[test]
    fn decrypts_rust_nostrs_payloads_under_random_keys() {
        use nostr_sdk::prelude::Keys;
        for len in [1usize, 31, 32, 33, 200, 1500, 70_000] {
            let (a, b) = (Keys::generate(), Keys::generate());
            let ck = ConversationKey::derive(a.secret_key(), &b.public_key()).unwrap();
            let plaintext = vec![0x42u8; len];
            let payload = theirs::encrypt_to_bytes_with_nonce(&ck, &plaintext, [len as u8; 32]).unwrap();
            assert_eq!(decrypt(&ck, payload).unwrap(), plaintext);
        }
    }
}
