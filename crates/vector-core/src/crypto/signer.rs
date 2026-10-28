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

    /// Run `f` on the vault keypair, refused unless it is this signer's identity: the check
    /// rides on the keypair derivation signing needs anyway, and tests the very key about to
    /// be used. The keypair is wiped when `f` returns.
    fn with_keys<R>(&self, f: impl FnOnce(&Keys) -> Result<R, SignerError>) -> Result<R, SignerError> {
        crate::state::MY_SECRET_KEY
            .with_keys(|keys| {
                if keys.public_key() != self.public_key {
                    return Err(SignerError::from(
                        "local key does not match the active identity (remote-signer account with no live signer)",
                    ));
                }
                f(keys)
            })
            .unwrap_or_else(|| Err(SignerError::from("no signer available (no local key)")))
    }

    /// The bare secret key, for operations that never need the public half: a `Keys` derives
    /// it on construction, an elliptic-curve multiply per call. `f` always runs once, given
    /// the key or why there is none.
    fn with_secret_key<R>(f: impl FnOnce(Result<&SecretKey, SignerError>) -> R) -> R {
        crate::state::MY_SECRET_KEY.read(|key| match key.map(|k| SecretKey::from_slice(k)) {
            Some(Ok(key)) => f(Ok(&key)),
            Some(Err(e)) => f(Err(SignerError::backend(e))),
            None => f(Err(SignerError::from("Secret key not available"))),
        })
    }

    /// NIP-59 seal under one vault read; the trait path reads it to encrypt and again to
    /// sign. The identity check rides on the keypair signing derives anyway.
    pub fn seal(identity: &PublicKey, rumor: UnsignedEvent, receiver: &PublicKey) -> Result<Event, SignerError> {
        Self::new(*identity)
            .with_keys(|keys| GiftWrapSealBuilder::new(rumor, *receiver).finalize(keys).map_err(SignerError::backend))
    }

    /// [`Self::seal`] plus the ephemeral-key wrap, still one vault read.
    pub fn gift_wrap(
        identity: &PublicKey,
        receiver: &PublicKey,
        rumor: UnsignedEvent,
        extra_tags: Vec<Tag>,
    ) -> Result<Event, SignerError> {
        Self::new(*identity).with_keys(|keys| {
            GiftWrapBuilder::new(*receiver, rumor)
                .extra_tags(extra_tags)
                .finalize(keys)
                .map_err(SignerError::backend)
        })
    }

    /// NIP-59 unwrap under one vault read; see [`Self::unwrap_batch`].
    pub fn unwrap_gift_wrap(gift_wrap: &Event) -> Result<UnwrappedGift, SignerError> {
        Self::unwrap_batch(&[gift_wrap]).pop().unwrap_or_else(|| Err(SignerError::from("empty batch")))
    }

    /// NIP-59 unwrap of many wraps under one vault read, one result per wrap in order.
    /// Checks what nostr's `from_gift_wrap` checks, but opens every layer before verifying
    /// any seal, so the key lives only for the NIP-44 opens and never across a signature
    /// check. Opening an unverified seal is safe: its content authenticates under ECDH with
    /// its claimed author, and nothing from it is trusted until that author's signature
    /// verifies.
    ///
    /// A sender's conversation key is derived once per batch and wiped with the secret key.
    ///
    /// No identity check against `my_public_key`: a vault key that isn't the identity can
    /// only open wraps sealed to itself, which then authenticate their sender like any
    /// DM. Signing and encrypting check the identity in [`Self::with_keys`].
    pub fn unwrap_batch(wraps: &[&Event]) -> Vec<Result<UnwrappedGift, SignerError>> {
        let checked: Vec<Result<(), SignerError>> = wraps
            .iter()
            .map(|wrap| {
                if wrap.kind != Kind::GiftWrap {
                    return Err(SignerError::from("not a gift wrap"));
                }
                // A wrap's signature is a throwaway key's and proves nothing (NIP-59); the seal's
                // does. Its id, which dedup and the ledger key on, was checked on arrival: the
                // nostr-sdk fork verifies the id of every event a relay sends, and every wrap
                // reaches here that way (syncs, catch-up, the live intake, the Android service).
                Ok(())
            })
            .collect();

        let mut senders: Vec<(PublicKey, nip44::v2::ConversationKey)> = Vec::new();
        let any_open = checked.iter().any(Result::is_ok);
        let open_all = |key: Result<&SecretKey, SignerError>| -> Vec<Result<(Event, String), SignerError>> {
            wraps
                .iter()
                .zip(checked)
                .map(|(wrap, check)| {
                    check?;
                    match &key {
                        Ok(key) => open_layers(key, wrap, &mut senders),
                        Err(e) => Err(SignerError::from(e.to_string())),
                    }
                })
                .collect()
        };
        // No wrap passed its checks: each fails on its own error before the key is asked for.
        let opened = if any_open { Self::with_secret_key(open_all) } else { open_all(Err(SignerError::from("unreachable"))) };
        for (_, ck) in senders.iter_mut() {
            wipe_conversation_key(ck);
        }

        // Every seal whose id matches has its signature checked in one batch. A seal that
        // fails either goes through `seal.verify()`, which names the failure as before.
        let id_ok: Vec<bool> = opened.iter().map(|slot| slot.as_ref().is_ok_and(|(seal, _)| seal.verify_id())).collect();
        let to_check: Vec<&Event> = opened
            .iter()
            .zip(&id_ok)
            .filter_map(|(slot, ok)| slot.as_ref().ok().filter(|_| *ok).map(|(seal, _)| seal))
            .collect();
        let mut sig_ok = super::batch_verify::signatures(&to_check).into_iter();
        opened
            .into_iter()
            .zip(id_ok)
            .map(|(slot, id_ok)| {
                let (seal, rumor) = slot?;
                if !(id_ok && sig_ok.next() == Some(true)) {
                    seal.verify().map_err(SignerError::backend)?;
                }
                let rumor = crate::event_json::unsigned(&rumor).map_err(SignerError::backend)?;
                if rumor.pubkey != seal.pubkey {
                    return Err(SignerError::from("sender mismatch"));
                }
                Ok(UnwrappedGift { sender: seal.pubkey, rumor })
            })
            .collect()
    }
}

/// Opens a wrap's two layers: the seal event, and the rumor's JSON (seal unverified).
fn open_layers(
    key: &SecretKey,
    wrap: &Event,
    senders: &mut Vec<(PublicKey, nip44::v2::ConversationKey)>,
) -> Result<(Event, String), SignerError> {
    let mut wrap_key = nip44::v2::ConversationKey::derive(key, &wrap.pubkey).map_err(SignerError::backend)?;
    let seal = nip44_open(&wrap_key, &wrap.content);
    wipe_conversation_key(&mut wrap_key);
    let seal = crate::event_json::event(&seal?).map_err(SignerError::backend)?;

    let at = match senders.iter().position(|(pk, _)| *pk == seal.pubkey) {
        Some(at) => at,
        None => {
            let ck = nip44::v2::ConversationKey::derive(key, &seal.pubkey).map_err(SignerError::backend)?;
            senders.push((seal.pubkey, ck));
            senders.len() - 1
        }
    };
    let rumor = nip44_open(&senders[at].1, &seal.content)?;
    Ok((seal, rumor))
}

/// nostr's own bound on an encoded v2 payload, checked before decoding allocates.
const NIP44_MAX_ENCODED: usize = 87_472;

/// `nip44::decrypt` with the conversation key already derived.
fn nip44_open(ck: &nip44::v2::ConversationKey, payload: &str) -> Result<String, SignerError> {
    if payload.len() > NIP44_MAX_ENCODED {
        return Err(SignerError::from("nip44 payload too long"));
    }
    let bytes = crate::simd::base64::STANDARD.decode_to_vec(payload.as_bytes()).map_err(SignerError::backend)?;
    if bytes.first() != Some(&nip44::Version::V2.as_u8()) {
        return Err(SignerError::from("unsupported nip44 version"));
    }
    let plain = nip44::v2::decrypt_to_bytes(ck, &bytes).map_err(SignerError::backend)?;
    String::from_utf8(plain).map_err(SignerError::backend)
}

fn wipe_conversation_key(ck: &mut nip44::v2::ConversationKey) {
    // SAFETY: `ck` is a valid, aligned, exclusive reference; the type has no Drop to skip.
    unsafe { core::ptr::write_volatile(ck, nip44::v2::ConversationKey::new([0u8; 32])) };
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
        self.with_keys(|keys| SignEvent::sign_event(keys, unsigned).map_err(SignerError::backend))
    }
}

impl Nip04 for GuardedSigner {
    type Error = SignerError;

    fn nip04_encrypt(&self, public_key: &PublicKey, content: &str) -> Result<String, Self::Error> {
        self.with_keys(|keys| nip04::encrypt(keys.secret_key(), public_key, content).map_err(SignerError::backend))
    }

    fn nip04_decrypt(&self, public_key: &PublicKey, payload: &str) -> Result<String, Self::Error> {
        Self::with_secret_key(|key| nip04::decrypt(key?, public_key, payload).map_err(SignerError::backend))
    }
}

impl Nip44 for GuardedSigner {
    type Error = SignerError;

    fn nip44_encrypt(&self, public_key: &PublicKey, content: &str) -> Result<String, Self::Error> {
        self.with_keys(|keys| {
            nip44::encrypt(keys.secret_key(), public_key, content, nip44::Version::default()).map_err(SignerError::backend)
        })
    }

    fn nip44_decrypt(&self, public_key: &PublicKey, payload: &str) -> Result<String, Self::Error> {
        Self::with_secret_key(|key| nip44::decrypt(key?, public_key, payload).map_err(SignerError::backend))
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
        // With its id, as the seal builder encrypts it.
        let mut rumor = EventBuilder::new(Kind::PrivateDirectMessage, "see you at nine")
            .tag(Tag::public_key(*to))
            .finalize_unsigned(*from);
        rumor.ensure_id();
        rumor
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

    /// Keeps the id and signature of `e` but not the fields they cover.
    fn retime(mut e: Event) -> Event {
        e.created_at = Timestamp::from_secs(e.created_at.as_secs() - 1);
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

    /// The wrap's id and signature belong to the relay layer: the nostr-sdk fork checks every
    /// arriving event's id (its `verify_and_cache` test refuses a retimed wrap), and a wrap's
    /// signature proves nothing. The unwrap checks neither; the seal carries the sender.
    #[test]
    fn a_wrap_is_checked_for_neither_its_id_nor_its_signature() {
        let (me, sender) = (Keys::generate(), Keys::generate());
        let good = wrap(&seal(&sender, &rumor(&sender.public_key(), &me.public_key()), &me.public_key()), &me.public_key());
        let forged_sig = flip_sig(good.clone());
        let retimed = retime(good.clone());
        assert!(forged_sig.verify().is_err() && !retimed.verify_id());
        let opened = with_me(&me, || GuardedSigner::unwrap_gift_wrap(&good)).unwrap();
        for odd in [&forged_sig, &retimed] {
            assert_eq!(with_me(&me, || GuardedSigner::unwrap_gift_wrap(odd)).unwrap(), opened);
        }
    }

    #[test]
    fn a_batch_matches_unwrapping_one_by_one() {
        let (me, stranger) = (Keys::generate(), Keys::generate());
        let senders: Vec<Keys> = (0..3).map(|_| Keys::generate()).collect();
        let mut gifts = Vec::new();
        for i in 0..12 {
            let from = &senders[i % 3];
            let r = rumor(&from.public_key(), &me.public_key());
            gifts.push(match i {
                4 => wrap(&flip_sig(seal(from, &r, &me.public_key())), &me.public_key()),
                7 => wrap(&seal(from, &r, &stranger.public_key()), &me.public_key()),
                10 => seal(from, &r, &me.public_key()),
                _ => wrap(&seal(from, &r, &me.public_key()), &me.public_key()),
            });
        }
        let refs: Vec<&Event> = gifts.iter().collect();
        let (batch, single) = with_me(&me, || {
            let batch = GuardedSigner::unwrap_batch(&refs);
            let single: Vec<_> = gifts.iter().map(GuardedSigner::unwrap_gift_wrap).collect();
            (batch, single)
        });
        assert_eq!(batch.len(), gifts.len());
        for (i, ((b, s), gift)) in batch.iter().zip(&single).zip(&gifts).enumerate() {
            match (b, s) {
                (Ok(b), Ok(s)) => {
                    assert_eq!(b, s, "wrap {i}");
                    assert_eq!(*b, UnwrappedGift::from_gift_wrap(&me, gift).unwrap(), "wrap {i}");
                    assert_eq!(b.sender, senders[i % 3].public_key());
                }
                (Err(_), Err(_)) => assert!([4, 7, 10].contains(&i), "wrap {i} refused"),
                _ => panic!("wrap {i}: batch and single disagree"),
            }
        }
    }

    #[test]
    fn a_batch_with_an_empty_vault_fails_every_wrap() {
        let (me, sender) = (Keys::generate(), Keys::generate());
        let gift = wrap(&seal(&sender, &rumor(&sender.public_key(), &me.public_key()), &me.public_key()), &me.public_key());
        let _guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::state::MY_SECRET_KEY.clear(&[]);
        assert!(GuardedSigner::unwrap_batch(&[&gift, &gift]).iter().all(Result::is_err));
        assert!(GuardedSigner::unwrap_batch(&[]).is_empty());
    }

    #[test]
    fn nip44_open_accepts_and_refuses_exactly_what_nostr_does() {
        let (a, b) = (Keys::generate(), Keys::generate());
        let ck = nip44::v2::ConversationKey::derive(b.secret_key(), &a.public_key()).unwrap();
        let same = |p: &str| {
            let theirs = nip44::decrypt(b.secret_key(), &a.public_key(), p).ok();
            let ours = nip44_open(&ck, p).ok();
            assert_eq!(ours, theirs, "payload {p:?}");
        };
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for len in [1usize, 2, 31, 32, 33, 100, 255, 256, 257, 1000, 5000, 65_000] {
            let text: String = (0..len).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
            let p = nip44::encrypt(a.secret_key(), &b.public_key(), &text, nip44::Version::V2).unwrap();
            same(&p);
            for cut in 1..5 {
                same(&p[..p.len() - cut]);
            }
            for extra in ["=", "A", "AAAA", " ", "\n"] {
                same(&format!("{p}{extra}"));
            }
            let tail = p.trim_end_matches('=').len();
            let spots: Vec<usize> = (1..=3).map(|k| tail - k).chain((0..40).map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed as usize) % p.len()
            })).collect();
            for at in spots {
                for c in ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'Q', 'g', 'w', '/', '+', '=', '-', '_', ' '] {
                    let mut q = p.clone().into_bytes();
                    q[at] = c as u8;
                    same(&String::from_utf8(q).unwrap());
                }
            }
        }
        same("");
        same("Ag==");
        same(&"A".repeat(NIP44_MAX_ENCODED + 4));
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

    #[test]
    fn seal_opens_like_the_trait_paths_seal() {
        let (me, peer) = (Keys::generate(), Keys::generate());
        let r = rumor(&me.public_key(), &peer.public_key());
        let ours = with_me(&me, || GuardedSigner::seal(&me.public_key(), r.clone(), &peer.public_key())).unwrap();
        let reference = GiftWrapSealBuilder::new(r.clone(), peer.public_key()).finalize(&me).unwrap();
        for s in [&ours, &reference] {
            s.verify().unwrap();
            assert_eq!((s.kind, s.pubkey), (Kind::Seal, me.public_key()));
            assert_eq!(peer.nip44_decrypt(&me.public_key(), &s.content).unwrap(), r.as_json());
        }
        let gift = wrap(&ours, &peer.public_key());
        let opened = UnwrappedGift::from_gift_wrap(&peer, &gift).unwrap();
        assert_eq!((opened.sender, opened.rumor.as_json()), (me.public_key(), r.as_json()));
    }

    #[test]
    fn seal_refuses_a_vault_key_that_is_not_the_identity() {
        let (me, client_key, peer) = (Keys::generate(), Keys::generate(), Keys::generate());
        let r = rumor(&me.public_key(), &peer.public_key());
        assert!(with_me(&client_key, || GuardedSigner::seal(&me.public_key(), r.clone(), &peer.public_key())).is_err());
        let _guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::state::MY_SECRET_KEY.clear(&[]);
        assert!(GuardedSigner::seal(&me.public_key(), r, &peer.public_key()).is_err());
    }

    #[test]
    fn a_vault_key_that_is_not_the_identity_never_signs_or_encrypts() {
        let (me, client_key, peer) = (Keys::generate(), Keys::generate(), Keys::generate());
        let signer = GuardedSigner::new(me.public_key());
        let from_peer = peer.nip44_encrypt(&client_key.public_key(), "to the vault key").unwrap();
        with_me(&client_key, || {
            let note = EventBuilder::new(Kind::TextNote, "x").finalize_unsigned(me.public_key());
            assert!(SignEvent::sign_event(&signer, note).is_err());
            assert!(Nip44::nip44_encrypt(&signer, &peer.public_key(), "x").is_err());
            assert!(Nip04::nip04_encrypt(&signer, &peer.public_key(), "x").is_err());
            // Opening is harmless: only what was sealed to the vault key opens.
            assert_eq!(Nip44::nip44_decrypt(&signer, &peer.public_key(), &from_peer).unwrap(), "to the vault key");
        });
        with_me(&me, || {
            let note = EventBuilder::new(Kind::TextNote, "x").finalize_unsigned(me.public_key());
            let signed = SignEvent::sign_event(&signer, note).unwrap();
            signed.verify().unwrap();
            assert_eq!(signed.pubkey, me.public_key());
        });
    }

    #[test]
    fn gift_wrap_opens_for_the_receiver_with_its_extra_tags() {
        let (me, peer) = (Keys::generate(), Keys::generate());
        let r = rumor(&me.public_key(), &peer.public_key());
        let expiry = Tag::expiration(Timestamp::from_secs(Timestamp::now().as_secs() + 30));
        let gift = with_me(&me, || {
            GuardedSigner::gift_wrap(&me.public_key(), &peer.public_key(), r.clone(), vec![expiry.clone()])
        })
        .unwrap();
        assert_eq!(gift.kind, Kind::GiftWrap);
        assert!(gift.tags.iter().any(|t| *t == expiry));
        let opened = UnwrappedGift::from_gift_wrap(&peer, &gift).unwrap();
        assert_eq!((opened.sender, opened.rumor.as_json()), (me.public_key(), r.as_json()));

        let client_key = Keys::generate();
        assert!(with_me(&client_key, || GuardedSigner::gift_wrap(&me.public_key(), &peer.public_key(), r, vec![])).is_err());
    }
}
