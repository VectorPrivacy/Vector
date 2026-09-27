//! GuardedSigner — a signer backed by a GuardedKey vault.
//!
//! Reads the secret key from the memory-hardened vault on every operation, so
//! the key exists in plaintext only for microseconds during signing.
//!
//! Implements the synchronous capability traits as the primary form: a vault
//! read plus local crypto never awaits, so the async impls just wrap them. The
//! async side exists only to satisfy `VectorSigner`, which has to stay async for
//! the bunker and NIP-55 backends.

use nostr_sdk::prelude::*;
use zeroize::Zeroize;

use crate::signer::{BoxedFuture, SignerError};

/// A signer backed by the `MY_SECRET_KEY` vault.
///
/// The secret key is never stored in this struct — it's fetched from the
/// GuardedKey vault on every operation and zeroized immediately after use.
#[derive(Debug, Clone)]
pub struct GuardedSigner {
    public_key: PublicKey,
}

impl GuardedSigner {
    pub fn new(public_key: PublicKey) -> Self {
        Self { public_key }
    }

    fn temp_keys(&self) -> Result<Keys, SignerError> {
        crate::state::MY_SECRET_KEY
            .to_keys()
            .ok_or_else(|| SignerError::from("Secret key not available"))
    }

    /// The bare secret key, for operations that never need the public half: a
    /// `Keys` derives it on construction, an elliptic-curve multiply per call.
    fn temp_secret_key() -> Result<SecretKey, SignerError> {
        let mut bytes = crate::state::MY_SECRET_KEY
            .get()
            .ok_or_else(|| SignerError::from("Secret key not available"))?;
        let key = SecretKey::from_slice(&bytes).map_err(SignerError::backend);
        bytes.zeroize();
        key
    }

    /// NIP-59 unwrap under one vault read. Checks what nostr's `from_gift_wrap` checks,
    /// but opens both layers before verifying the seal, so the key lives only for the two
    /// NIP-44 opens and not across a signature check. Opening an unverified seal is safe:
    /// its content authenticates under ECDH with its claimed author, and nothing from it
    /// is trusted until that author's signature verifies.
    ///
    /// No identity check against `my_public_key`: a vault key that isn't the identity can
    /// only open wraps sealed to itself, which then authenticate their sender like any
    /// DM. Signing keeps `active_signer`'s fail-closed check.
    pub fn unwrap_gift_wrap(gift_wrap: &Event) -> Result<UnwrappedGift, SignerError> {
        if gift_wrap.kind != Kind::GiftWrap {
            return Err(SignerError::from("not a gift wrap"));
        }
        // Not redundant with nostr-sdk's relay check: its verification cache accepts any
        // event whose id it has already verified without re-hashing it, so a relay can pair
        // a verified id with other content. This is also what binds the wrap's tags and
        // created_at to its ephemeral key, which the NIP-44 MAC does not.
        gift_wrap.verify().map_err(SignerError::backend)?;

        let (seal, rumor) = {
            let key = Self::temp_secret_key()?;
            let seal = nip44::decrypt(&key, &gift_wrap.pubkey, &gift_wrap.content).map_err(SignerError::backend)?;
            let seal = Event::from_json(seal).map_err(SignerError::backend)?;
            let rumor = nip44::decrypt(&key, &seal.pubkey, &seal.content).map_err(SignerError::backend)?;
            (seal, rumor)
        };

        seal.verify().map_err(SignerError::backend)?;
        let rumor = UnsignedEvent::from_json(rumor).map_err(SignerError::backend)?;
        if rumor.pubkey != seal.pubkey {
            return Err(SignerError::from("sender mismatch"));
        }
        Ok(UnwrappedGift { sender: seal.pubkey, rumor })
    }
}

// ---------------------------------------------------------------------------
// Synchronous capabilities
// ---------------------------------------------------------------------------

impl GetPublicKey for GuardedSigner {
    type Error = SignerError;

    #[inline]
    fn get_public_key(&self) -> Result<PublicKey, Self::Error> {
        Ok(self.public_key)
    }
}

impl SignEvent for GuardedSigner {
    type Error = SignerError;

    fn sign_event(&self, unsigned: UnsignedEvent) -> Result<Event, Self::Error> {
        let keys = self.temp_keys()?;
        SignEvent::sign_event(&keys, unsigned).map_err(SignerError::backend)
    }
}

impl Nip04 for GuardedSigner {
    type Error = SignerError;

    fn nip04_encrypt(&self, public_key: &PublicKey, content: &str) -> Result<String, Self::Error> {
        nip04::encrypt(&Self::temp_secret_key()?, public_key, content).map_err(SignerError::backend)
    }

    fn nip04_decrypt(&self, public_key: &PublicKey, payload: &str) -> Result<String, Self::Error> {
        nip04::decrypt(&Self::temp_secret_key()?, public_key, payload).map_err(SignerError::backend)
    }
}

impl Nip44 for GuardedSigner {
    type Error = SignerError;

    fn nip44_encrypt(&self, public_key: &PublicKey, content: &str) -> Result<String, Self::Error> {
        nip44::encrypt(&Self::temp_secret_key()?, public_key, content, nip44::Version::default())
            .map_err(SignerError::backend)
    }

    fn nip44_decrypt(&self, public_key: &PublicKey, payload: &str) -> Result<String, Self::Error> {
        nip44::decrypt(&Self::temp_secret_key()?, public_key, payload).map_err(SignerError::backend)
    }
}

// ---------------------------------------------------------------------------
// Async forwarding — only so GuardedSigner satisfies `VectorSigner`
// ---------------------------------------------------------------------------

impl AsyncGetPublicKey for GuardedSigner {
    type Error = SignerError;

    #[inline]
    fn get_public_key_async(&self) -> BoxedFuture<'_, Result<PublicKey, Self::Error>> {
        let res = GetPublicKey::get_public_key(self);
        Box::pin(async move { res })
    }
}

impl AsyncSignEvent for GuardedSigner {
    type Error = SignerError;

    #[inline]
    fn sign_event_async(&self, unsigned: UnsignedEvent) -> BoxedFuture<'_, Result<Event, Self::Error>> {
        let res = SignEvent::sign_event(self, unsigned);
        Box::pin(async move { res })
    }
}

impl AsyncNip04 for GuardedSigner {
    type Error = SignerError;

    #[inline]
    fn nip04_encrypt_async<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> BoxedFuture<'a, Result<String, Self::Error>> {
        let res = Nip04::nip04_encrypt(self, public_key, content);
        Box::pin(async move { res })
    }

    #[inline]
    fn nip04_decrypt_async<'a>(
        &'a self,
        public_key: &'a PublicKey,
        encrypted_content: &'a str,
    ) -> BoxedFuture<'a, Result<String, Self::Error>> {
        let res = Nip04::nip04_decrypt(self, public_key, encrypted_content);
        Box::pin(async move { res })
    }
}

impl AsyncNip44 for GuardedSigner {
    type Error = SignerError;

    #[inline]
    fn nip44_encrypt_async<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> BoxedFuture<'a, Result<String, Self::Error>> {
        let res = Nip44::nip44_encrypt(self, public_key, content);
        Box::pin(async move { res })
    }

    #[inline]
    fn nip44_decrypt_async<'a>(
        &'a self,
        public_key: &'a PublicKey,
        payload: &'a str,
    ) -> BoxedFuture<'a, Result<String, Self::Error>> {
        let res = Nip44::nip44_decrypt(self, public_key, payload);
        Box::pin(async move { res })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rumor(from: &PublicKey, to: &PublicKey) -> UnsignedEvent {
        EventBuilder::new(Kind::PrivateDirectMessage, "see you at nine")
            .tag(Tag::public_key(*to))
            .finalize_unsigned(*from)
    }

    fn seal(author: &Keys, rumor: &UnsignedEvent, to: &PublicKey) -> Event {
        let content = nip44::encrypt(author.secret_key(), to, rumor.as_json(), nip44::Version::default()).unwrap();
        EventBuilder::new(Kind::Seal, content).finalize(author).unwrap()
    }

    fn wrap(seal: &Event, to: &PublicKey) -> Event {
        let ephemeral = Keys::generate();
        let content = nip44::encrypt(ephemeral.secret_key(), to, seal.as_json(), nip44::Version::default()).unwrap();
        EventBuilder::new(Kind::GiftWrap, content).tag(Tag::public_key(*to)).finalize(&ephemeral).unwrap()
    }

    fn flip_sig(mut e: Event) -> Event {
        let mut sig = e.sig.to_bytes();
        sig[10] ^= 1;
        e.sig = Signature::from_slice(&sig).unwrap();
        e
    }

    /// Runs `f` with `me` in the vault; the vault is a process global, so this holds the
    /// suite's shared guard.
    fn with_me<R>(me: &Keys, f: impl FnOnce() -> R) -> R {
        let _guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::state::MY_SECRET_KEY.store_from_keys(me, &[]);
        let out = f();
        crate::state::MY_SECRET_KEY.clear(&[]);
        out
    }

    #[test]
    fn unwrap_matches_nostr_on_a_valid_wrap() {
        let (me, sender) = (Keys::generate(), Keys::generate());
        let gift = wrap(&seal(&sender, &rumor(&sender.public_key(), &me.public_key()), &me.public_key()), &me.public_key());
        let reference = UnwrappedGift::from_gift_wrap(&me, &gift).unwrap();
        let ours = with_me(&me, || GuardedSigner::unwrap_gift_wrap(&gift)).unwrap();
        assert_eq!(ours, reference);
        assert_eq!(ours.sender, sender.public_key());
    }

    #[test]
    fn unwrap_rejects_everything_nostr_rejects() {
        let (me, sender, stranger) = (Keys::generate(), Keys::generate(), Keys::generate());
        let good_rumor = rumor(&sender.public_key(), &me.public_key());
        let good_seal = seal(&sender, &good_rumor, &me.public_key());

        let cases: Vec<(&str, Event)> = vec![
            ("forged wrap signature", flip_sig(wrap(&good_seal, &me.public_key()))),
            // Decrypts cleanly: only the signature check can refuse it.
            ("forged seal signature", wrap(&flip_sig(good_seal.clone()), &me.public_key())),
            ("rumor author is not the seal author",
                wrap(&seal(&sender, &rumor(&stranger.public_key(), &me.public_key()), &me.public_key()), &me.public_key())),
            ("wrap sealed to someone else", wrap(&good_seal, &stranger.public_key())),
            ("seal sealed to someone else",
                wrap(&seal(&sender, &good_rumor, &stranger.public_key()), &me.public_key())),
            ("not a gift wrap", good_seal.clone()),
        ];
        for (what, gift) in cases {
            assert!(UnwrappedGift::from_gift_wrap(&me, &gift).is_err(), "nostr accepts: {what}");
            assert!(with_me(&me, || GuardedSigner::unwrap_gift_wrap(&gift)).is_err(), "we accept: {what}");
        }
    }

    #[test]
    fn unwrap_with_an_empty_vault_fails() {
        let (me, sender) = (Keys::generate(), Keys::generate());
        let gift = wrap(&seal(&sender, &rumor(&sender.public_key(), &me.public_key()), &me.public_key()), &me.public_key());
        let _guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::state::MY_SECRET_KEY.clear(&[]);
        assert!(GuardedSigner::unwrap_gift_wrap(&gift).is_err());
    }

    #[test]
    fn nip44_and_nip04_match_keys_both_ways() {
        let (me, peer) = (Keys::generate(), Keys::generate());
        let signer = GuardedSigner::new(me.public_key());
        with_me(&me, || {
            let sealed = Nip44::nip44_encrypt(&signer, &peer.public_key(), "hello").unwrap();
            assert_eq!(peer.nip44_decrypt(&me.public_key(), &sealed).unwrap(), "hello");
            let from_peer = peer.nip44_encrypt(&me.public_key(), "back").unwrap();
            assert_eq!(Nip44::nip44_decrypt(&signer, &peer.public_key(), &from_peer).unwrap(), "back");

            let sealed = Nip04::nip04_encrypt(&signer, &peer.public_key(), "old").unwrap();
            assert_eq!(peer.nip04_decrypt(&me.public_key(), &sealed).unwrap(), "old");
            let from_peer = peer.nip04_encrypt(&me.public_key(), "school").unwrap();
            assert_eq!(Nip04::nip04_decrypt(&signer, &peer.public_key(), &from_peer).unwrap(), "school");
        });
    }
}
