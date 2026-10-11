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

    fn temp_secret(&self) -> Result<SecretKey, SignerError> {
        crate::state::MY_SECRET_KEY
            .to_secret_key()
            .ok_or_else(|| SignerError::from("Secret key not available"))
    }

    /// NIP-59 unwrap with one vault read for both layers. The generic
    /// `UnwrappedGift::from_gift_wrap` reads the vault per layer, and each read
    /// is deliberately expensive. Same checks, same order.
    pub fn unwrap_gift_wrap(&self, gift_wrap: &Event) -> Result<nip59::UnwrappedGift, SignerError> {
        if gift_wrap.kind != Kind::GiftWrap {
            return Err("not a gift wrap".into());
        }
        // Authenticate before the secret leaves the vault.
        gift_wrap.verify().map_err(SignerError::backend)?;

        let secret = self.temp_secret()?;
        let seal = nip44::decrypt(&secret, &gift_wrap.pubkey, &gift_wrap.content)
            .map_err(SignerError::backend)?;
        let seal = Event::from_json(seal).map_err(SignerError::backend)?;
        seal.verify().map_err(SignerError::backend)?;
        let rumor = nip44::decrypt(&secret, &seal.pubkey, &seal.content)
            .map_err(SignerError::backend)?;
        drop(secret);

        let rumor = UnsignedEvent::from_json(rumor).map_err(SignerError::backend)?;
        if rumor.pubkey != seal.pubkey {
            return Err("rumor author does not match the seal".into());
        }
        Ok(nip59::UnwrappedGift { sender: seal.pubkey, rumor })
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
        let secret = self.temp_secret()?;
        nip04::encrypt(&secret, public_key, content)
            .map_err(SignerError::backend)
    }

    fn nip04_decrypt(&self, public_key: &PublicKey, payload: &str) -> Result<String, Self::Error> {
        let secret = self.temp_secret()?;
        nip04::decrypt(&secret, public_key, payload)
            .map_err(SignerError::backend)
    }
}

impl Nip44 for GuardedSigner {
    type Error = SignerError;

    fn nip44_encrypt(&self, public_key: &PublicKey, content: &str) -> Result<String, Self::Error> {
        let secret = self.temp_secret()?;
        nip44::encrypt(&secret, public_key, content, nip44::Version::default())
            .map_err(SignerError::backend)
    }

    fn nip44_decrypt(&self, public_key: &PublicKey, payload: &str) -> Result<String, Self::Error> {
        let secret = self.temp_secret()?;
        nip44::decrypt(&secret, public_key, payload)
            .map_err(SignerError::backend)
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

    fn with_vault_key<R>(f: impl FnOnce(&Keys, &GuardedSigner) -> R) -> R {
        let _guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let me = Keys::generate();
        crate::state::MY_SECRET_KEY.store_from_keys(&me, &[&crate::state::ENCRYPTION_KEY]);
        let out = f(&me, &GuardedSigner::new(me.public_key()));
        crate::state::MY_SECRET_KEY.clear(&[&crate::state::ENCRYPTION_KEY]);
        out
    }

    fn rumor(author: &Keys, to: &PublicKey) -> UnsignedEvent {
        EventBuilder::new(Kind::PrivateDirectMessage, "hi")
            .tag(Tag::public_key(*to))
            .finalize_unsigned(author.public_key())
    }

    #[test]
    fn unwrap_matches_nostr_from_gift_wrap() {
        with_vault_key(|me, signer| {
            let sender = Keys::generate();
            let wrap = nip59::GiftWrapBuilder::new(me.public_key(), rumor(&sender, &me.public_key()))
                .finalize(&sender)
                .unwrap();
            let ours = signer.unwrap_gift_wrap(&wrap).unwrap();
            let theirs = nip59::UnwrappedGift::from_gift_wrap(me, &wrap).unwrap();
            assert_eq!(ours.sender, theirs.sender);
            assert_eq!(ours.rumor.as_json(), theirs.rumor.as_json());
        });
    }

    #[test]
    fn unwrap_rejects_what_nostr_rejects() {
        with_vault_key(|me, signer| {
            let sender = Keys::generate();
            let good = nip59::GiftWrapBuilder::new(me.public_key(), rumor(&sender, &me.public_key()))
                .finalize(&sender)
                .unwrap();

            let mut tampered = good.clone();
            tampered.content.push('A');
            assert!(signer.unwrap_gift_wrap(&tampered).is_err(), "outer signature must be checked");

            let other = Keys::generate();
            let not_mine = nip59::GiftWrapBuilder::new(other.public_key(), rumor(&sender, &other.public_key()))
                .finalize(&sender)
                .unwrap();
            assert!(signer.unwrap_gift_wrap(&not_mine).is_err());

            // Seal signed by `sender`, rumor claiming `other`.
            let forged = nip59::GiftWrapBuilder::new(me.public_key(), rumor(&other, &me.public_key()))
                .finalize(&sender)
                .unwrap();
            assert!(nip59::UnwrappedGift::from_gift_wrap(me, &forged).is_err());
            assert!(signer.unwrap_gift_wrap(&forged).is_err(), "rumor author must match the seal");

            let note = EventBuilder::new(Kind::TextNote, "x").finalize(&sender).unwrap();
            assert!(signer.unwrap_gift_wrap(&note).is_err());
        });
    }
}
