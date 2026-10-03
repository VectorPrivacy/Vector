//! The transfer's cryptography: SPAKE2 over the code, the commitment that stops either side picking
//! its message after seeing the other's, the transcript every key is bound to, the sealed messages
//! that follow, and the ML-KEM-1024 exchange whose secret joins SPAKE2's before the identity is
//! sealed, so a recording is safe from a future quantum computer too.

use hkdf::Hkdf;
use libcrux_ml_kem::mlkem1024;
use nostr_sdk::prelude::{Keys, PublicKey, SecretKey};
use rand::RngCore;
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::code::Code;
use crate::crypto::chachapoly::ChaCha20Poly1305;

const PROTOCOL: &[u8] = b"vector-transfer-v1";
/// A SPAKE2 message: the symmetric side byte and a compressed Edwards point.
pub const PAKE_MSG_LEN: usize = 33;
/// Digits in the number the user carries from the new device to the old one.
pub const SAS_DIGITS: usize = 6;

/// One side's SPAKE2 run over the code.
pub struct Pake {
    state: Spake2<Ed25519Group>,
    msg: Vec<u8>,
}

impl Pake {
    pub fn start(code: &Code) -> Self {
        let password = code.canonical();
        let (state, msg) = Spake2::<Ed25519Group>::start_symmetric(&Password::new(password.as_bytes()), &Identity::new(PROTOCOL));
        Self { state, msg }
    }

    pub fn msg(&self) -> &[u8] {
        &self.msg
    }

    /// The shared secret. Refuses our own message reflected back and a point of small order, the
    /// two checks spake2 leaves to its caller.
    pub fn finish(self, peer: &[u8]) -> Result<Zeroizing<Vec<u8>>, &'static str> {
        if peer.len() != PAKE_MSG_LEN {
            return Err("malformed pake message");
        }
        if peer == self.msg.as_slice() {
            return Err("reflected pake message");
        }
        let point: [u8; 32] = peer[1..].try_into().map_err(|_| "malformed pake message")?;
        match curve25519_dalek::edwards::CompressedEdwardsY(point).decompress() {
            Some(p) if !p.is_small_order() => {}
            _ => return Err("invalid pake point"),
        }
        self.state.finish(peer).map(Zeroizing::new).map_err(|_| "pake failed")
    }
}

/// What the joiner publishes before it sees the shower's message, so it can't choose its own later.
pub fn commitment(msg: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(PROTOCOL);
    h.update(b"/commit");
    h.update(msg);
    h.finalize().into()
}

/// The meeting room for a nameplate. Derived from the nameplate alone and public by design: a room
/// derived from the words would let anyone watching the relays search for the code offline.
pub fn room(nameplate: u16) -> PublicKey {
    let mut counter = 0u8;
    loop {
        let mut h = Sha256::new();
        h.update(PROTOCOL);
        h.update(b"/room");
        h.update(nameplate.to_be_bytes());
        h.update([counter]);
        let digest: [u8; 32] = h.finalize().into();
        if let Ok(secret) = SecretKey::from_slice(&digest) {
            return Keys::new(secret).public_key();
        }
        counter += 1;
    }
}

/// Everything both devices agreed on, in a fixed order with fixed lengths.
pub fn transcript(room: &PublicKey, shower: &PublicKey, joiner: &PublicKey, msg_shower: &[u8], msg_joiner: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(PROTOCOL);
    h.update(b"/transcript");
    h.update(room.to_bytes());
    h.update(shower.to_bytes());
    h.update(joiner.to_bytes());
    for msg in [msg_shower, msg_joiner] {
        h.update((msg.len() as u16).to_be_bytes());
        h.update(msg);
    }
    h.finalize().into()
}

/// Which way a sealed message travels; each direction has its own key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    ShowerToJoiner,
    JoinerToShower,
}

/// The sealed message kinds. Each is sent at most once per direction, so the kind is the nonce.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Sealed {
    Hello = 1,
    Bundle = 2,
    Deny = 3,
    Ack = 4,
}

/// An ML-KEM-1024 encapsulation key's size.
pub const KEM_PUBLIC_LEN: usize = 1568;
/// An ML-KEM-1024 ciphertext's size.
pub const KEM_CIPHERTEXT_LEN: usize = 1568;

/// The receiver's ML-KEM-1024 key, held as its 64-byte FIPS 203 seed: the expanded private key is
/// rebuilt only to decapsulate, then wiped.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct KemSeed([u8; 64]);

impl KemSeed {
    pub fn generate() -> Self {
        let mut seed = [0u8; 64];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        Self(seed)
    }

    fn expand(&self) -> (libcrux_ml_kem::MlKemPrivateKey<3168>, Vec<u8>) {
        let (sk, pk) = mlkem1024::generate_key_pair(self.0).into_parts();
        (sk, pk.as_slice().to_vec())
    }

    pub fn public_key(&self) -> Vec<u8> {
        let (mut sk, pk) = self.expand();
        sk[0..].zeroize();
        pk
    }

    /// The shared secret in `ciphertext`. A tampered ciphertext yields an unrelated secret (ML-KEM's
    /// implicit rejection), which the sealed bundle then refuses to open under.
    pub fn decapsulate(&self, ciphertext: &[u8]) -> Result<Zeroizing<[u8; 32]>, ()> {
        let ct = mlkem1024::MlKem1024Ciphertext::try_from(ciphertext).map_err(|_| ())?;
        let (mut sk, _) = self.expand();
        let shared = Zeroizing::new(mlkem1024::decapsulate(&sk, &ct));
        sk[0..].zeroize();
        Ok(shared)
    }
}

/// Whether `public_key` is a well-formed ML-KEM-1024 encapsulation key (FIPS 203's check).
pub fn kem_public_ok(public_key: &[u8]) -> bool {
    mlkem1024::MlKem1024PublicKey::try_from(public_key).is_ok_and(|pk| mlkem1024::validate_public_key(&pk))
}

/// Encapsulate to the receiver's key, once it passes FIPS 203's encapsulation key check.
pub fn kem_encapsulate(public_key: &[u8]) -> Result<(Vec<u8>, Zeroizing<[u8; 32]>), ()> {
    let pk = mlkem1024::MlKem1024PublicKey::try_from(public_key).map_err(|_| ())?;
    if !mlkem1024::validate_public_key(&pk) {
        return Err(());
    }
    let mut randomness = Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(randomness.as_mut());
    let (ct, shared) = mlkem1024::encapsulate(&pk, *randomness);
    Ok((ct.as_slice().to_vec(), Zeroizing::new(shared)))
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SessionKeys {
    shower_to_joiner: [u8; 32],
    joiner_to_shower: [u8; 32],
    transcript: [u8; 32],
    sas: [u8; SAS_DIGITS],
    /// What the post-quantum secret is mixed with; never used to encrypt by itself.
    chain: [u8; 32],
}

impl SessionKeys {
    pub fn derive(shared: &[u8], transcript: [u8; 32]) -> Self {
        let hk = Hkdf::<Sha256>::new(Some(&transcript), shared);
        let mut keys = Self { shower_to_joiner: [0; 32], joiner_to_shower: [0; 32], transcript, sas: [0; SAS_DIGITS], chain: [0; 32] };
        hk.expand(b"vector-transfer-v1/shower-to-joiner", &mut keys.shower_to_joiner).expect("32 bytes");
        hk.expand(b"vector-transfer-v1/joiner-to-shower", &mut keys.joiner_to_shower).expect("32 bytes");
        hk.expand(b"vector-transfer-v1/chain", &mut keys.chain).expect("32 bytes");
        let mut sas = Zeroizing::new([0u8; 8]);
        hk.expand(b"vector-transfer-v1/sas", sas.as_mut()).expect("8 bytes");
        let mut n = u64::from_be_bytes(*sas) % 10u64.pow(SAS_DIGITS as u32);
        for d in keys.sas.iter_mut().rev() {
            *d = b'0' + (n % 10) as u8;
            n /= 10;
        }
        keys
    }

    /// The keys the identity travels under: SPAKE2's secret and ML-KEM's together, bound to the
    /// encapsulation key and ciphertext. Reading the bundle takes breaking both.
    pub fn hybrid(&self, kem_public: &[u8], kem_ciphertext: &[u8], kem_shared: &[u8; 32]) -> Self {
        let mut salt = Sha256::new();
        salt.update(PROTOCOL);
        salt.update(b"/hybrid");
        salt.update(self.transcript);
        for part in [kem_public, kem_ciphertext] {
            salt.update((part.len() as u16).to_be_bytes());
            salt.update(part);
        }
        let salt: [u8; 32] = salt.finalize().into();
        let mut ikm = Zeroizing::new([0u8; 64]);
        ikm[..32].copy_from_slice(&self.chain);
        ikm[32..].copy_from_slice(kem_shared);
        let hk = Hkdf::<Sha256>::new(Some(&salt), ikm.as_ref());
        let mut keys = Self { shower_to_joiner: [0; 32], joiner_to_shower: [0; 32], transcript: salt, sas: self.sas, chain: [0; 32] };
        hk.expand(b"vector-transfer-v1/hybrid/shower-to-joiner", &mut keys.shower_to_joiner).expect("32 bytes");
        hk.expand(b"vector-transfer-v1/hybrid/joiner-to-shower", &mut keys.joiner_to_shower).expect("32 bytes");
        keys
    }

    /// The number both screens agree on: shown on the receiver, typed on the sender.
    pub fn sas(&self) -> String {
        String::from_utf8_lossy(&self.sas).into_owned()
    }

    /// Whether `typed` is the number, compared in constant time. Only its digits count, so a
    /// keypad's "123-456" or "123 456" reads the same.
    pub fn sas_matches(&self, typed: &str) -> bool {
        let digits: Vec<u8> = typed.bytes().filter(u8::is_ascii_digit).collect();
        digits.len() == SAS_DIGITS && digits.iter().zip(self.sas.iter()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
    }

    fn cipher(&self, dir: Dir) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(match dir {
            Dir::ShowerToJoiner => &self.shower_to_joiner,
            Dir::JoinerToShower => &self.joiner_to_shower,
        })
    }

    fn nonce_and_aad(&self, kind: Sealed) -> ([u8; 12], [u8; 33]) {
        let mut nonce = [0u8; 12];
        nonce[0] = kind as u8;
        let mut aad = [0u8; 33];
        aad[..32].copy_from_slice(&self.transcript);
        aad[32] = kind as u8;
        (nonce, aad)
    }

    pub fn seal(&self, dir: Dir, kind: Sealed, plain: &[u8]) -> Vec<u8> {
        let (nonce, aad) = self.nonce_and_aad(kind);
        let mut buf = plain.to_vec();
        let tag = self.cipher(dir).seal_in_place(&nonce, &aad, &mut buf).expect("transfer messages are small");
        buf.extend_from_slice(&tag);
        buf
    }

    pub fn open(&self, dir: Dir, kind: Sealed, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, ()> {
        if sealed.len() < 16 {
            return Err(());
        }
        let (nonce, aad) = self.nonce_and_aad(kind);
        let (body, tag) = sealed.split_at(sealed.len() - 16);
        let tag: [u8; 16] = tag.try_into().map_err(|_| ())?;
        let mut buf = Zeroizing::new(body.to_vec());
        self.cipher(dir).open_in_place(&nonce, &aad, &mut buf, &tag).map_err(|_| ())?;
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(code_a: &str, code_b: &str) -> (Result<Zeroizing<Vec<u8>>, &'static str>, Result<Zeroizing<Vec<u8>>, &'static str>) {
        let a = Pake::start(&Code::parse(code_a).unwrap());
        let b = Pake::start(&Code::parse(code_b).unwrap());
        let (ma, mb) = (a.msg().to_vec(), b.msg().to_vec());
        (a.finish(&mb), b.finish(&ma))
    }

    #[test]
    fn the_same_code_agrees_and_a_different_one_does_not() {
        let (a, b) = pair("7-orbit-lemon-stage", "7-orbit-lemon-stage");
        assert_eq!(a.unwrap(), b.unwrap());
        let (a, b) = pair("7-orbit-lemon-stage", "7-orbit-lemon-stamp");
        assert_ne!(a.unwrap(), b.unwrap());
    }

    #[test]
    fn a_reflected_or_degenerate_message_is_refused() {
        let code = Code::parse("7-orbit-lemon-stage").unwrap();
        let a = Pake::start(&code);
        let own = a.msg().to_vec();
        assert_eq!(a.finish(&own).unwrap_err(), "reflected pake message");
        let mut identity = [0u8; PAKE_MSG_LEN];
        identity[0] = 0x53;
        identity[1] = 1;
        assert_eq!(Pake::start(&code).finish(&identity).unwrap_err(), "invalid pake point");
        assert!(Pake::start(&code).finish(&[0x53; 5]).is_err());
    }

    #[test]
    fn sealed_messages_bind_direction_kind_and_transcript() {
        let keys = SessionKeys::derive(b"shared", [1; 32]);
        let sealed = keys.seal(Dir::JoinerToShower, Sealed::Hello, b"hi");
        assert_eq!(&*keys.open(Dir::JoinerToShower, Sealed::Hello, &sealed).unwrap(), b"hi");
        assert!(keys.open(Dir::ShowerToJoiner, Sealed::Hello, &sealed).is_err(), "reflected to the other direction");
        assert!(keys.open(Dir::JoinerToShower, Sealed::Bundle, &sealed).is_err(), "relabelled as another kind");
        let other = SessionKeys::derive(b"shared", [2; 32]);
        assert!(other.open(Dir::JoinerToShower, Sealed::Hello, &sealed).is_err(), "spliced into another transcript");
        let mut flipped = sealed.clone();
        flipped[0] ^= 1;
        assert!(keys.open(Dir::JoinerToShower, Sealed::Hello, &flipped).is_err());
    }

    #[test]
    fn the_sas_is_six_digits_and_matches_only_itself() {
        let keys = SessionKeys::derive(b"shared", [3; 32]);
        let sas = keys.sas();
        assert_eq!(sas.len(), SAS_DIGITS);
        assert!(sas.bytes().all(|b| b.is_ascii_digit()));
        assert!(keys.sas_matches(&sas));
        assert!(keys.sas_matches(&format!("{} {}", &sas[..3], &sas[3..])));
        for sep in ["-", ".", ",", "\u{2013}"] {
            assert!(keys.sas_matches(&format!("{}{sep}{}", &sas[..3], &sas[3..])), "a keypad separator {sep:?}");
        }
        let mut wrong = sas.clone().into_bytes();
        wrong[5] = if wrong[5] == b'9' { b'0' } else { wrong[5] + 1 };
        assert!(!keys.sas_matches(std::str::from_utf8(&wrong).unwrap()));
        assert!(!keys.sas_matches(&sas[..5]));
    }

    #[test]
    fn the_kem_round_trips_and_a_tampered_ciphertext_opens_nothing() {
        let seed = KemSeed::generate();
        let pk = seed.public_key();
        assert_eq!(pk.len(), KEM_PUBLIC_LEN);
        let (ct, sent) = kem_encapsulate(&pk).unwrap();
        assert_eq!(ct.len(), KEM_CIPHERTEXT_LEN);
        assert_eq!(*seed.decapsulate(&ct).unwrap(), *sent);
        let mut bent = ct.clone();
        bent[0] ^= 1;
        assert_ne!(*seed.decapsulate(&bent).unwrap(), *sent, "implicit rejection");
        assert!(seed.decapsulate(&ct[1..]).is_err());
        assert!(kem_encapsulate(&pk[1..]).is_err());
        let mut out_of_range = pk.clone();
        out_of_range[..2].copy_from_slice(&[0xff, 0xff]);
        assert!(kem_encapsulate(&out_of_range).is_err(), "FIPS 203 encapsulation key check");
    }

    #[test]
    fn the_hybrid_keys_need_both_secrets() {
        let classical = SessionKeys::derive(b"shared", [4; 32]);
        let seed = KemSeed::generate();
        let pk = seed.public_key();
        let (ct, ss) = kem_encapsulate(&pk).unwrap();
        let hybrid = classical.hybrid(&pk, &ct, &ss);
        let sealed = hybrid.seal(Dir::ShowerToJoiner, Sealed::Bundle, b"identity");
        assert!(classical.open(Dir::ShowerToJoiner, Sealed::Bundle, &sealed).is_err(), "SPAKE2 alone");
        let other_classical = SessionKeys::derive(b"other", [4; 32]);
        assert!(other_classical.hybrid(&pk, &ct, &ss).open(Dir::ShowerToJoiner, Sealed::Bundle, &sealed).is_err(), "ML-KEM alone");
        assert!(classical.hybrid(&pk, &ct, &[0; 32]).open(Dir::ShowerToJoiner, Sealed::Bundle, &sealed).is_err());
        let received = classical.hybrid(&pk, &ct, &seed.decapsulate(&ct).unwrap());
        assert_eq!(&*received.open(Dir::ShowerToJoiner, Sealed::Bundle, &sealed).unwrap(), b"identity");
    }

    /// libcrux against RustCrypto's independent FIPS 203 implementation, same seeds and randomness.
    #[test]
    fn ml_kem_matches_an_independent_implementation() {
        use ml_kem::kem::Decapsulate;
        use ml_kem::{EncapsulateDeterministic, EncodedSizeUser, KemCore, MlKem1024};
        for i in 0..20u8 {
            let mut seed = [0u8; 64];
            rand::rngs::OsRng.fill_bytes(&mut seed);
            seed[0] = i;
            let ours = mlkem1024::generate_key_pair(seed);
            let (d, z) = (seed[..32].try_into().unwrap(), seed[32..].try_into().unwrap());
            let (dk, ek) = MlKem1024::generate_deterministic(&d, &z);
            assert_eq!(ek.as_bytes().as_slice(), ours.pk().as_slice(), "encapsulation key");
            assert_eq!(dk.as_bytes().as_slice(), ours.sk().as_slice(), "decapsulation key");

            let mut m = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut m);
            let (ct, ss) = mlkem1024::encapsulate(ours.public_key(), m);
            let (their_ct, their_ss) = ek.encapsulate_deterministic(&m.into()).unwrap();
            assert_eq!(their_ct.as_slice(), ct.as_slice(), "ciphertext");
            assert_eq!(their_ss.as_slice(), ss.as_slice(), "shared secret");
            assert_eq!(dk.decapsulate(&their_ct).unwrap().as_slice(), mlkem1024::decapsulate(ours.private_key(), &ct).as_slice());
        }
    }

    #[test]
    fn the_room_depends_on_the_nameplate_only() {
        assert_eq!(room(7), room(7));
        assert_ne!(room(7), room(8));
    }
}
