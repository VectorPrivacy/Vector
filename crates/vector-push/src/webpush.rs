//! RFC 8291 Web Push message encryption, `aes128gcm` (RFC 8188), one record.
//!
//! The sender encrypts straight to the device's subscription keys, so the pusher forwards
//! bytes it cannot open. Every body is padded to [`BODY_LEN`], so its size says nothing
//! about the message inside.

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use hkdf::Hkdf;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use sha2::Sha256;

use crate::{Error, Result};

/// Salt (16), record size (4), key id length (1), the sender's public key (65).
pub const HEADER_LEN: usize = 86;
const TAG_LEN: usize = 16;
/// Small enough for every push service, Firefox for Android's (about 3015) included.
pub const BODY_LEN: usize = 3000;
/// What fits once the header, the tag and the padding delimiter are paid for.
pub const MAX_PLAINTEXT: usize = BODY_LEN - HEADER_LEN - TAG_LEN - 1;
const RECORD_SIZE: u32 = 4096;

/// Encrypt `plaintext` to a subscription's `p256dh` key and `auth` secret.
pub fn encrypt(ua_public: &[u8], auth: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let sender = SecretKey::random(&mut rand::rngs::OsRng);
    encrypt_with(ua_public, auth, plaintext, &sender, crate::random::<16>(), BODY_LEN)
}

fn encrypt_with(
    ua_public: &[u8],
    auth: &[u8],
    plaintext: &[u8],
    sender: &SecretKey,
    salt: [u8; 16],
    body_len: usize,
) -> Result<Vec<u8>> {
    if auth.len() != 16 {
        return Err(Error::Key("auth secret must be 16 bytes"));
    }
    if plaintext.len() > body_len - HEADER_LEN - TAG_LEN - 1 {
        return Err(Error::Format("notification too large".into()));
    }
    let ua = PublicKey::from_sec1_bytes(ua_public).map_err(|_| Error::Key("bad p256dh key"))?;
    let ua_bytes = ua.to_encoded_point(false);
    let as_public = sender.public_key().to_encoded_point(false);
    let shared = p256::ecdh::diffie_hellman(sender.to_nonzero_scalar(), ua.as_affine());
    let (cek, nonce) = derive(shared.raw_secret_bytes(), auth, ua_bytes.as_bytes(), as_public.as_bytes(), &salt)?;

    let mut record = Vec::with_capacity(body_len - HEADER_LEN);
    record.extend_from_slice(plaintext);
    record.push(0x02);
    record.resize(body_len - HEADER_LEN - TAG_LEN, 0);
    let tag = Aes128Gcm::new(&cek.into())
        .encrypt_in_place_detached(Nonce::from_slice(&nonce), &[], &mut record)
        .map_err(|_| Error::Crypto("encryption failed"))?;

    let mut body = Vec::with_capacity(body_len);
    body.extend_from_slice(&salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(65);
    body.extend_from_slice(as_public.as_bytes());
    body.extend_from_slice(&record);
    body.extend_from_slice(&tag);
    Ok(body)
}

/// Open a body with the subscription's private key: what a browser or the UnifiedPush
/// connector does on receipt. Here for tests and for native receivers.
pub fn decrypt(ua_secret: &SecretKey, auth: &[u8], body: &[u8]) -> Result<Vec<u8>> {
    if body.len() < HEADER_LEN + TAG_LEN + 1 || body[20] != 65 {
        return Err(Error::Format("bad aes128gcm header".into()));
    }
    let salt: [u8; 16] = body[..16].try_into().unwrap();
    let as_public = &body[21..86];
    let sender = PublicKey::from_sec1_bytes(as_public).map_err(|_| Error::Key("bad sender key"))?;
    let ua_public = ua_secret.public_key().to_encoded_point(false);
    let shared = p256::ecdh::diffie_hellman(ua_secret.to_nonzero_scalar(), sender.as_affine());
    let (cek, nonce) = derive(shared.raw_secret_bytes(), auth, ua_public.as_bytes(), as_public, &salt)?;

    let (ciphertext, tag) = body[HEADER_LEN..].split_at(body.len() - HEADER_LEN - TAG_LEN);
    let mut record = ciphertext.to_vec();
    Aes128Gcm::new(&cek.into())
        .decrypt_in_place_detached(Nonce::from_slice(&nonce), &[], &mut record, tag.into())
        .map_err(|_| Error::Crypto("decryption failed"))?;
    let end = record.iter().rposition(|&b| b != 0).ok_or(Error::Format("no padding delimiter".into()))?;
    if record[end] != 0x02 {
        return Err(Error::Format("not a final record".into()));
    }
    record.truncate(end);
    Ok(record)
}

fn derive(ecdh: &[u8], auth: &[u8], ua_public: &[u8], as_public: &[u8], salt: &[u8; 16]) -> Result<([u8; 16], [u8; 12])> {
    let mut key_info = Vec::with_capacity(14 + 130);
    key_info.extend_from_slice(b"WebPush: info\0");
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(as_public);
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth), ecdh)
        .expand(&key_info, &mut ikm)
        .map_err(|_| Error::Crypto("hkdf"))?;

    let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
    let mut cek = [0u8; 16];
    let mut nonce = [0u8; 12];
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).map_err(|_| Error::Crypto("hkdf"))?;
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).map_err(|_| Error::Crypto("hkdf"))?;
    Ok((cek, nonce))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{b64url, unb64url};

    // RFC 8291 §5 and Appendix A.
    const PLAINTEXT: &str = "When I grow up, I want to be a watermelon";
    const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
    const UA_PUBLIC: &str = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const UA_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
    const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
    const SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";
    const BODY: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";

    #[test]
    fn matches_the_rfc_example() {
        let sender = SecretKey::from_slice(&unb64url(AS_PRIVATE).unwrap()).unwrap();
        let salt: [u8; 16] = unb64url(SALT).unwrap().try_into().unwrap();
        let body = encrypt_with(
            &unb64url(UA_PUBLIC).unwrap(),
            &unb64url(AUTH).unwrap(),
            PLAINTEXT.as_bytes(),
            &sender,
            salt,
            144,
        )
        .unwrap();
        assert_eq!(b64url(&body), BODY);
    }

    #[test]
    fn opens_the_rfc_example() {
        let ua = SecretKey::from_slice(&unb64url(UA_PRIVATE).unwrap()).unwrap();
        let out = decrypt(&ua, &unb64url(AUTH).unwrap(), &unb64url(BODY).unwrap()).unwrap();
        assert_eq!(out, PLAINTEXT.as_bytes());
    }

    #[test]
    fn every_body_is_the_same_size() {
        let ua = SecretKey::random(&mut rand::rngs::OsRng);
        let ua_public = ua.public_key().to_encoded_point(false);
        let auth = crate::random::<16>();
        for text in ["", "hi", &"x".repeat(MAX_PLAINTEXT)] {
            let body = encrypt(ua_public.as_bytes(), &auth, text.as_bytes()).unwrap();
            assert_eq!(body.len(), BODY_LEN);
            assert_eq!(decrypt(&ua, &auth, &body).unwrap(), text.as_bytes());
        }
        assert!(encrypt(ua_public.as_bytes(), &auth, "x".repeat(MAX_PLAINTEXT + 1).as_bytes()).is_err());
    }

    #[test]
    fn a_wrong_auth_secret_fails() {
        let ua = SecretKey::random(&mut rand::rngs::OsRng);
        let ua_public = ua.public_key().to_encoded_point(false);
        let body = encrypt(ua_public.as_bytes(), &[1; 16], b"hi").unwrap();
        assert!(decrypt(&ua, &[2; 16], &body).is_err());
    }
}
