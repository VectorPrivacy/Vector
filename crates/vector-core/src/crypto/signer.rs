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

use crate::signer::SignerError;

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
        let keys = self.temp_keys()?;
        keys.nip04_encrypt(public_key, content)
            .map_err(SignerError::backend)
    }

    fn nip04_decrypt(&self, public_key: &PublicKey, payload: &str) -> Result<String, Self::Error> {
        let keys = self.temp_keys()?;
        keys.nip04_decrypt(public_key, payload)
            .map_err(SignerError::backend)
    }
}

impl Nip44 for GuardedSigner {
    type Error = SignerError;

    fn nip44_encrypt(&self, public_key: &PublicKey, content: &str) -> Result<String, Self::Error> {
        let keys = self.temp_keys()?;
        keys.nip44_encrypt(public_key, content)
            .map_err(SignerError::backend)
    }

    fn nip44_decrypt(&self, public_key: &PublicKey, payload: &str) -> Result<String, Self::Error> {
        let keys = self.temp_keys()?;
        keys.nip44_decrypt(public_key, payload)
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
