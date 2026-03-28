//! GuardedSigner — NostrSigner backed by a GuardedKey vault.
//!
//! Any client using nostr-sdk needs a signer. This implementation reads the
//! secret key from the memory-hardened vault on every operation — the key
//! exists in plaintext only for microseconds during signing.

use nostr_sdk::prelude::*;

/// A `NostrSigner` backed by the `MY_SECRET_KEY` vault.
///
/// The secret key is never stored in this struct — it's fetched from the
/// GuardedKey vault on every operation and zeroized immediately after use.
#[derive(Debug)]
pub struct GuardedSigner {
    public_key: PublicKey,
}

impl GuardedSigner {
    pub fn new(public_key: PublicKey) -> Self {
        Self { public_key }
    }

    fn temp_keys(&self) -> Result<Keys, SignerError> {
        crate::state::MY_SECRET_KEY.to_keys()
            .ok_or_else(|| SignerError::from("Secret key not available"))
    }
}

impl NostrSigner for GuardedSigner {
    fn backend(&self) -> SignerBackend<'_> {
        SignerBackend::Keys
    }

    fn get_public_key(&self) -> BoxedFuture<'_, Result<PublicKey, SignerError>> {
        let pk = self.public_key;
        Box::pin(async move { Ok(pk) })
    }

    fn sign_event(&self, unsigned: UnsignedEvent) -> BoxedFuture<'_, Result<Event, SignerError>> {
        let keys = self.temp_keys();
        Box::pin(async move {
            let keys = keys?;
            unsigned.sign_with_keys(&keys).map_err(SignerError::backend)
        })
    }

    fn nip04_encrypt<'a>(
        &'a self, public_key: &'a PublicKey, content: &'a str,
    ) -> BoxedFuture<'a, Result<String, SignerError>> {
        let keys = self.temp_keys();
        Box::pin(async move { let keys = keys?; keys.nip04_encrypt(public_key, content).await })
    }

    fn nip04_decrypt<'a>(
        &'a self, public_key: &'a PublicKey, encrypted_content: &'a str,
    ) -> BoxedFuture<'a, Result<String, SignerError>> {
        let keys = self.temp_keys();
        Box::pin(async move { let keys = keys?; keys.nip04_decrypt(public_key, encrypted_content).await })
    }

    fn nip44_encrypt<'a>(
        &'a self, public_key: &'a PublicKey, content: &'a str,
    ) -> BoxedFuture<'a, Result<String, SignerError>> {
        let keys = self.temp_keys();
        Box::pin(async move { let keys = keys?; keys.nip44_encrypt(public_key, content).await })
    }

    fn nip44_decrypt<'a>(
        &'a self, public_key: &'a PublicKey, payload: &'a str,
    ) -> BoxedFuture<'a, Result<String, SignerError>> {
        let keys = self.temp_keys();
        Box::pin(async move { let keys = keys?; keys.nip44_decrypt(public_key, payload).await })
    }
}
