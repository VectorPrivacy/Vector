//! The vaults and inputs the attack demos and the benchmarks share.

use super::*;

pub(super) static PARTNER: GuardedKey = GuardedKey::empty();
pub(super) static SETTER: GuardedKey = GuardedKey::empty();

pub(super) fn setup(partner_key: [u8; 32]) {
    ensure_vaults();
    PARTNER.clear(&[&SETTER]);
    SETTER.clear(&[&PARTNER]);
    PARTNER.set(partner_key, &[&SETTER]);
}

/// A hot path's name, the vault it reads, and the read.
pub(super) type HotPath<'a> = (&'static str, &'static GuardedKey, Box<dyn Fn() + 'a>);

/// The same key in PARTNER and both standing vaults, and inputs for every hot path that reads one.
pub(super) struct HotPaths {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) key: Box<[u8; 32]>,
    peer: nostr_sdk::prelude::Keys,
    signer: crate::crypto::GuardedSigner,
    sealed: String,
    to_me: String,
    unsigned: nostr_sdk::prelude::UnsignedEvent,
    wrap: nostr_sdk::prelude::Event,
}

impl HotPaths {
    pub(super) fn new() -> Self {
        use nostr_sdk::prelude::*;
        // No repeated bytes, so every match is a copy and never part of a run.
        let key = Box::new(*blake3::hash(b"stack residue").as_bytes());
        let me = Keys::new(SecretKey::from_slice(&key[..]).unwrap()).public_key();
        let peer = Keys::generate();
        setup(*key);
        crate::state::ENCRYPTION_KEY.set(*key, &[&PARTNER]);
        crate::state::MY_SECRET_KEY.set(*key, &[&PARTNER]);
        let rumor = EventBuilder::new(Kind::PrivateDirectMessage, "probe").finalize_unsigned(peer.public_key());
        let seal_body = nip44::encrypt(peer.secret_key(), &me, rumor.as_json(), nip44::Version::default()).unwrap();
        let seal = EventBuilder::new(Kind::Seal, seal_body).finalize(&peer).unwrap();
        let eph = Keys::generate();
        let body = nip44::encrypt(eph.secret_key(), &me, seal.as_json(), nip44::Version::default()).unwrap();
        Self {
            sealed: crate::crypto::encrypt_with_key("probe", &key).unwrap(),
            to_me: nip44::encrypt(peer.secret_key(), &me, "probe", nip44::Version::default()).unwrap(),
            signer: crate::crypto::GuardedSigner::new(me),
            unsigned: EventBuilder::new(Kind::TextNote, "probe").finalize_unsigned(me),
            wrap: EventBuilder::new(Kind::GiftWrap, body).tag(Tag::public_key(me)).finalize(&eph).unwrap(),
            key,
            peer,
        }
    }

    /// Each path by name, and the vault it reads.
    pub(super) fn paths(&self) -> Vec<HotPath<'_>> {
        use crate::crypto::chachapoly::ChaCha20Poly1305;
        use nostr_sdk::prelude::*;
        let at_rest = &crate::state::ENCRYPTION_KEY;
        let identity = &crate::state::MY_SECRET_KEY;
        vec![
            ("get(), caller wipes", &PARTNER, Box::new(|| {
                let mut k = PARTNER.get().unwrap();
                std::hint::black_box(&k);
                k.zeroize();
            })),
            ("get() into a cipher, then dropped", &PARTNER, Box::new(|| {
                let mut cipher = None;
                if let Some(mut k) = PARTNER.get() {
                    cipher = Some(ChaCha20Poly1305::new(&k));
                    k.zeroize();
                }
                std::hint::black_box(&cipher);
            })),
            ("to_keys() for its public key", &PARTNER, Box::new(|| {
                // owned-key: measures what an owned read leaves behind.
                std::hint::black_box(PARTNER.to_keys().map(|k| k.public_key()));
            })),
            ("with_key(), nothing kept", &PARTNER, Box::new(|| {
                std::hint::black_box(PARTNER.with_key(|k| std::hint::black_box(k)[0]));
            })),
            ("with_key() into a cipher, then dropped", &PARTNER, Box::new(|| {
                let mut cipher = None;
                PARTNER.with_key(|k| cipher = Some(ChaCha20Poly1305::new(k)));
                std::hint::black_box(&cipher);
            })),
            ("with_keys() for its public key", &PARTNER, Box::new(|| {
                std::hint::black_box(PARTNER.with_keys(|k| k.public_key()));
            })),
            ("at rest: maybe_decrypt_text", at_rest, Box::new(|| {
                std::hint::black_box(crate::crypto::maybe_decrypt_text(&self.sealed));
            })),
            ("at rest: open_batch, two values", at_rest, Box::new(|| {
                std::hint::black_box(crate::crypto::open_batch(|o| {
                    (o.open_or_keep(self.sealed.clone()), o.open_or_keep(self.sealed.clone()))
                }));
            })),
            ("signer: sign_event", identity, Box::new(|| {
                std::hint::black_box(self.signer.sign_event(self.unsigned.clone()).unwrap());
            })),
            ("signer: nip44_encrypt", identity, Box::new(|| {
                std::hint::black_box(self.signer.nip44_encrypt(&self.peer.public_key(), "probe").unwrap());
            })),
            ("signer: nip44_decrypt", identity, Box::new(|| {
                std::hint::black_box(self.signer.nip44_decrypt(&self.peer.public_key(), &self.to_me).unwrap());
            })),
            ("signer: unwrap_gift_wrap", identity, Box::new(|| {
                std::hint::black_box(crate::crypto::GuardedSigner::unwrap_gift_wrap(&self.wrap).unwrap());
            })),
        ]
    }
}

impl Drop for HotPaths {
    fn drop(&mut self) {
        crate::state::MY_SECRET_KEY.clear(&[&PARTNER]);
        crate::state::ENCRYPTION_KEY.clear(&[&PARTNER]);
        PARTNER.clear(&[]);
    }
}
