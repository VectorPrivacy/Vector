//! NIP-07 browser signer — a `window.nostr` extension (Alby, nos2x, ...) in
//! the page hosting a web build.
//!
//! Like NIP-55 the account keeps nothing secret on this device; the identity
//! key stays in the extension. The page owns `window.nostr` while vector-core
//! may run elsewhere (a worker), so [`Nip07Backend`] is a request/response
//! hook the host registers: it forwards each call to the page and resolves
//! with the extension's answer.
//!
//! Only extensions with NIP-44 qualify: gift-wrapped DMs need it.

use std::sync::{LazyLock, OnceLock};

use nostr_sdk::prelude::*;

use crate::signer::{BoxedFuture, SignerError};

/// The host transport to the page's `window.nostr`.
///
/// `method` is the NIP-07 call (`getPublicKey`, `signEvent`, `nip44.encrypt`,
/// `nip44.decrypt`, `nip04.encrypt`, `nip04.decrypt`), `params` its arguments
/// as a JSON array; the result is whatever the extension resolved with.
pub trait Nip07Backend: Send + Sync + 'static {
    fn request(
        &self,
        method: &'static str,
        params: serde_json::Value,
    ) -> BoxedFuture<'static, Result<serde_json::Value, String>>;
}

static NIP07_BACKEND: OnceLock<Box<dyn Nip07Backend>> = OnceLock::new();

/// Register the page transport. Only a web host has one.
pub fn set_nip07_backend(backend: Box<dyn Nip07Backend>) {
    let _ = NIP07_BACKEND.set(backend);
}

#[inline]
pub fn nip07_backend() -> Option<&'static dyn Nip07Backend> {
    NIP07_BACKEND.get().map(|b| b.as_ref())
}

/// Extensions prompt per request; a deep sync would otherwise stack thousands.
const NIP07_MAX_CONCURRENT_OPS: usize = 8;

static NIP07_SEMAPHORE: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(NIP07_MAX_CONCURRENT_OPS));

async fn call(method: &'static str, params: serde_json::Value) -> Result<serde_json::Value, SignerError> {
    let backend = nip07_backend().ok_or("no browser signer available")?;
    let _permit = NIP07_SEMAPHORE
        .acquire()
        .await
        .map_err(|_| SignerError::from("browser signer queue closed"))?;
    backend.request(method, params).await.map_err(SignerError::from)
}

async fn call_str(method: &'static str, params: serde_json::Value) -> Result<String, SignerError> {
    match call(method, params).await? {
        serde_json::Value::String(s) => Ok(s),
        other => Err(SignerError::from(format!("browser signer returned {other} for {method}"))),
    }
}

/// Ask the extension for its identity. Used at pairing and re-authorization.
pub async fn nip07_get_public_key() -> Result<PublicKey, String> {
    let pk = call_str("getPublicKey", serde_json::json!([])).await.map_err(|e| e.to_string())?;
    PublicKey::parse(&pk).map_err(|e| format!("browser signer returned an invalid pubkey: {e}"))
}

/// A `VectorSigner` over the page's extension. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Nip07Signer {
    user_pubkey: PublicKey,
}

impl Nip07Signer {
    pub fn new(user_pubkey: PublicKey) -> Self {
        Self { user_pubkey }
    }
}

impl AsyncGetPublicKey for Nip07Signer {
    type Error = SignerError;

    // Cached: resolved on every hot path, and an extension may prompt for it.
    fn get_public_key_async(&self) -> BoxedFuture<'_, Result<PublicKey, Self::Error>> {
        Box::pin(async move { Ok(self.user_pubkey) })
    }
}

impl AsyncSignEvent for Nip07Signer {
    type Error = SignerError;

    fn sign_event_async(&self, unsigned: UnsignedEvent) -> BoxedFuture<'_, Result<Event, Self::Error>> {
        Box::pin(async move {
            let template = serde_json::json!({
                "kind": unsigned.kind.as_u16(),
                "created_at": unsigned.created_at.as_secs(),
                "tags": unsigned.tags,
                "content": unsigned.content,
                "pubkey": self.user_pubkey.to_hex(),
            });
            let signed = call("signEvent", serde_json::json!([template])).await?;
            accept_signed(&unsigned, self.user_pubkey, signed)
        })
    }
}

/// Take the extension's signed event only if it is exactly the one we asked
/// for, by our identity. Fails closed: a different extension account, or an
/// edited event, would fork the wire under an id nothing downstream expects.
fn accept_signed(unsigned: &UnsignedEvent, me: PublicKey, signed: serde_json::Value) -> Result<Event, SignerError> {
    let event: Event = serde_json::from_value(signed).map_err(SignerError::backend)?;
    if event.pubkey != me {
        return Err(SignerError::from(format!(
            "browser signer signed as {} (expected {})",
            event.pubkey.to_hex(),
            me.to_hex()
        )));
    }
    if event.kind != unsigned.kind
        || event.created_at != unsigned.created_at
        || event.content != unsigned.content
        || event.tags != unsigned.tags
    {
        return Err(SignerError::from("browser signer altered the event it signed"));
    }
    event.verify().map_err(SignerError::backend)?;
    Ok(event)
}

impl AsyncNip04 for Nip07Signer {
    type Error = SignerError;

    fn nip04_encrypt_async<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> BoxedFuture<'a, Result<String, Self::Error>> {
        Box::pin(async move { call_str("nip04.encrypt", serde_json::json!([public_key.to_hex(), content])).await })
    }

    fn nip04_decrypt_async<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> BoxedFuture<'a, Result<String, Self::Error>> {
        Box::pin(async move { call_str("nip04.decrypt", serde_json::json!([public_key.to_hex(), content])).await })
    }
}

impl AsyncNip44 for Nip07Signer {
    type Error = SignerError;

    fn nip44_encrypt_async<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> BoxedFuture<'a, Result<String, Self::Error>> {
        Box::pin(async move { call_str("nip44.encrypt", serde_json::json!([public_key.to_hex(), content])).await })
    }

    fn nip44_decrypt_async<'a>(
        &'a self,
        public_key: &'a PublicKey,
        payload: &'a str,
    ) -> BoxedFuture<'a, Result<String, Self::Error>> {
        Box::pin(async move { call_str("nip44.decrypt", serde_json::json!([public_key.to_hex(), payload])).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_ext::FinalizeUnsignedWithId;

    fn unsigned(pk: PublicKey, content: &str) -> UnsignedEvent {
        EventBuilder::new(Kind::TextNote, content)
            .tag(Tag::hashtag("vector"))
            .finalize_unsigned_with_id(pk)
    }

    #[test]
    fn accepts_exactly_the_event_asked_for() {
        let keys = Keys::generate();
        let asked = unsigned(keys.public_key(), "hi");
        let signed = asked.clone().finalize(&keys).unwrap();
        let event = accept_signed(&asked, keys.public_key(), serde_json::to_value(&signed).unwrap()).unwrap();
        assert_eq!(Some(event.id), asked.id);
    }

    #[test]
    fn rejects_another_identity_an_edit_or_a_bad_signature() {
        let me = Keys::generate();
        let asked = unsigned(me.public_key(), "hi");

        let other = Keys::generate();
        let foreign = unsigned(other.public_key(), "hi").finalize(&other).unwrap();
        assert!(accept_signed(&asked, me.public_key(), serde_json::to_value(&foreign).unwrap()).is_err());

        let edited = unsigned(me.public_key(), "hello").finalize(&me).unwrap();
        assert!(accept_signed(&asked, me.public_key(), serde_json::to_value(&edited).unwrap()).is_err());

        let mut forged = serde_json::to_value(asked.clone().finalize(&me).unwrap()).unwrap();
        let sig = forged["sig"].as_str().unwrap().to_string();
        let flipped = if sig.starts_with('0') { format!("1{}", &sig[1..]) } else { format!("0{}", &sig[1..]) };
        forged["sig"] = serde_json::json!(flipped);
        assert!(accept_signed(&asked, me.public_key(), forged).is_err());
    }

    #[tokio::test]
    async fn no_backend_is_a_clean_error() {
        if nip07_backend().is_some() {
            return;
        }
        let keys = Keys::generate();
        let signer = Nip07Signer::new(keys.public_key());
        assert_eq!(signer.get_public_key_async().await.unwrap(), keys.public_key());
        assert!(signer.sign_event_async(unsigned(keys.public_key(), "hi")).await.is_err());
    }
}
