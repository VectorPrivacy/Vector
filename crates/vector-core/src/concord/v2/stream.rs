//! CORD-01: Private Streams — the wrap/seal/rumor envelope.
//!
//! A stream event is a kind 1059 wrap that *reverses* NIP-59: fixed author
//! (the derived stream key), ephemeral `p` tag, `created_at` untweaked, and
//! the wrap encrypted under the stream's NIP-44 *self*-conversation key, never
//! the `p`-tagged key. The seal inside is signed by the author's real key and
//! declares its content form by kind: 20013 encrypted (double-wrapped, can
//! never be lifted out as a standalone public event), 20014 plaintext (the
//! Control Plane only — a byte-verbatim rumor whose signature survives a
//! compaction re-wrap).

use nostr_sdk::nips::nip44::v2::{decrypt_to_bytes, encrypt_to_bytes};
use nostr_sdk::prelude::*;

use super::derive::GroupKey;
use super::{combine_ms, kind, split_ms, ChannelId, Epoch, NIP44_MAX_PLAINTEXT};

/// Rumor tag names.
pub const TAG_CHANNEL: &str = "channel";
pub const TAG_EPOCH: &str = "epoch";
pub const TAG_MS: &str = "ms";

/// Which seal form a plane uses (CORD-02 §5). Fixed per protocol layer, never
/// a per-message choice; the seal's kind declares it so a reader never sniffs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealForm {
    /// Kind 20013 — the rumor NIP-44-encrypted inside the encrypted wrap.
    /// Chat, Guestbook, and rekey planes MUST use this.
    Encrypted,
    /// Kind 20014 — the rumor's serialized JSON byte-verbatim. Control Plane
    /// MUST use this (signatures must survive compaction re-encryption).
    Plaintext,
}

impl SealForm {
    pub fn kind(&self) -> u16 {
        match self {
            SealForm::Encrypted => kind::SEAL_ENCRYPTED,
            SealForm::Plaintext => kind::SEAL_PLAINTEXT,
        }
    }
}

#[derive(Debug)]
pub enum StreamError {
    /// A NIP-44 layer failed to encrypt/decrypt.
    Crypto(String),
    /// A layer exceeded the NIP-44 plaintext cap.
    TooLarge(usize),
    /// Signing failed.
    Sign(String),
    /// Malformed or unparseable layer.
    Malformed(String),
    /// The seal's signature does not verify.
    BadSignature,
    /// The seal's kind is not a seal kind.
    NotASeal(u16),
    /// The rumor's author differs from the seal's verified signer.
    AuthorMismatch,
    /// The rumor's channel/epoch binding doesn't match the opening key.
    BindingMismatch,
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamError::Crypto(e) => write!(f, "stream crypto: {e}"),
            StreamError::TooLarge(n) => write!(f, "layer exceeds NIP-44 plaintext cap: {n} bytes"),
            StreamError::Sign(e) => write!(f, "stream sign: {e}"),
            StreamError::Malformed(e) => write!(f, "malformed stream layer: {e}"),
            StreamError::BadSignature => write!(f, "seal signature invalid"),
            StreamError::NotASeal(k) => write!(f, "kind {k} is not a seal"),
            StreamError::AuthorMismatch => write!(f, "rumor author differs from seal signer"),
            StreamError::BindingMismatch => write!(f, "channel/epoch binding mismatch"),
        }
    }
}

impl std::error::Error for StreamError {}

/// A decrypted, signature-verified stream event.
#[derive(Debug, Clone)]
pub struct Opened {
    /// The inner rumor. Its id is computed (stable across re-wraps).
    pub rumor: UnsignedEvent,
    /// The verified real author (the seal's signer; the rumor is checked to
    /// agree — a keyholder can't attribute a rumor to someone else).
    pub author: PublicKey,
    /// Which seal form carried it.
    pub seal_form: SealForm,
    /// The outer wrap's event id — the transport dedup key.
    pub wrap_id: EventId,
    /// The seal's exact `content` bytes — for a plaintext seal this is the
    /// byte-verbatim rumor JSON a compaction must carry forward unmodified.
    pub seal_content: String,
}

impl Opened {
    /// The rumor's true millisecond time: `created_at * 1000 + ms`, `None` if
    /// the `ms` tag is malformed (the caller drops the entry, CORD-02 §5).
    pub fn timestamp_ms(&self) -> Option<u64> {
        let remainder = match tag_value(&self.rumor.tags, TAG_MS) {
            Some(v) => v.parse::<u64>().ok()?,
            None => 0,
        };
        combine_ms(self.rumor.created_at.as_secs(), remainder)
    }
}

fn cap(plaintext: &str) -> Result<&str, StreamError> {
    if plaintext.len() > NIP44_MAX_PLAINTEXT {
        return Err(StreamError::TooLarge(plaintext.len()));
    }
    Ok(plaintext)
}

fn tag_value<'a>(tags: &'a Tags, name: &str) -> Option<&'a str> {
    tags.iter()
        .find(|t| t.kind() == TagKind::Custom(name.into()))
        .and_then(|t| t.content())
}

/// Build an unsigned rumor carrying the CORD-03 §3 binding (`channel`,
/// `epoch`) plus the `ms` remainder, with `extra_tags` appended verbatim.
pub fn build_rumor(
    author: PublicKey,
    rumor_kind: u16,
    content: &str,
    channel: &ChannelId,
    epoch: Epoch,
    unix_ms: u64,
    extra_tags: Vec<Tag>,
) -> UnsignedEvent {
    let (secs, remainder) = split_ms(unix_ms);
    let mut tags = vec![
        Tag::custom(TagKind::Custom(TAG_CHANNEL.into()), [channel.to_hex()]),
        Tag::custom(TagKind::Custom(TAG_EPOCH.into()), [epoch.0.to_string()]),
        Tag::custom(TagKind::Custom(TAG_MS.into()), [remainder.to_string()]),
    ];
    tags.extend(extra_tags);
    let mut rumor = EventBuilder::new(Kind::Custom(rumor_kind), content)
        .tags(tags)
        .custom_created_at(Timestamp::from_secs(secs))
        .build(author);
    rumor.ensure_id();
    rumor
}

/// Build an unsigned rumor with no channel binding (Guestbook and rekey
/// rumors bind by plane address alone; they still carry `ms`).
pub fn build_plane_rumor(
    author: PublicKey,
    rumor_kind: u16,
    content: &str,
    unix_ms: u64,
    extra_tags: Vec<Tag>,
) -> UnsignedEvent {
    let (secs, remainder) = split_ms(unix_ms);
    let mut tags = vec![Tag::custom(TagKind::Custom(TAG_MS.into()), [remainder.to_string()])];
    tags.extend(extra_tags);
    let mut rumor = EventBuilder::new(Kind::Custom(rumor_kind), content)
        .tags(tags)
        .custom_created_at(Timestamp::from_secs(secs))
        .build(author);
    rumor.ensure_id();
    rumor
}

/// Build the unsigned seal around a rumor — for callers signing through a
/// `NostrSigner` (local keys or a NIP-46 bunker) before [`wrap_signed_seal`].
pub fn build_seal(
    group: &GroupKey,
    author: PublicKey,
    rumor: &UnsignedEvent,
    form: SealForm,
) -> Result<UnsignedEvent, StreamError> {
    let mut rumor = rumor.clone();
    rumor.ensure_id();
    let rumor_json = rumor.as_json();
    let seal_content = match form {
        // Byte-verbatim: the seal's content IS the rumor's serialized JSON.
        SealForm::Plaintext => cap(&rumor_json)?.to_string(),
        SealForm::Encrypted => encrypt_to_bytes(group.conversation_key(), cap(&rumor_json)?.as_bytes())
            .map(|ct| base64_encode(&ct))
            .map_err(|e| StreamError::Crypto(e.to_string()))?,
    };
    cap(&seal_content)?;
    Ok(EventBuilder::new(Kind::Custom(form.kind()), seal_content)
        .custom_created_at(rumor.created_at)
        .build(author))
}

/// Seal a rumor with the author's local keys and wrap it at the plane's
/// address. `ephemeral_p` is the wrap's throwaway `p` key — the caller may
/// retain its secret to NIP-09-delete the wrap later (CORD-01 §Deletions).
pub fn wrap_rumor(
    group: &GroupKey,
    author_keys: &Keys,
    rumor: &UnsignedEvent,
    form: SealForm,
    ephemeral_p: PublicKey,
) -> Result<Event, StreamError> {
    let seal = build_seal(group, author_keys.public_key(), rumor, form)?
        .sign_with_keys(author_keys)
        .map_err(|e| StreamError::Sign(e.to_string()))?;
    wrap_signed_seal(group, &seal, kind::WRAP, ephemeral_p)
}

/// Wrap an already-signed seal at the plane's address. This is the compaction
/// primitive (CORD-06 §3): a plaintext seal read from one epoch re-wraps under
/// another with its author signature intact, byte-verbatim.
pub fn wrap_signed_seal(
    group: &GroupKey,
    seal: &Event,
    wrap_kind: u16,
    ephemeral_p: PublicKey,
) -> Result<Event, StreamError> {
    let seal_json = seal.as_json();
    let content = encrypt_to_bytes(group.conversation_key(), cap(&seal_json)?.as_bytes())
        .map(|ct| base64_encode(&ct))
        .map_err(|e| StreamError::Crypto(e.to_string()))?;
    EventBuilder::new(Kind::Custom(wrap_kind), cap(&content)?)
        .tags([Tag::public_key(ephemeral_p)])
        .custom_created_at(seal.created_at)
        .build(group.public_key())
        .sign_with_keys(group.keys())
        .map_err(|e| StreamError::Sign(e.to_string()))
}

/// Open a stream event: decrypt the wrap under the plane's conversation key,
/// verify the seal's signature, extract the rumor, and check the rumor's
/// author agrees with the seal's signer.
pub fn open(group: &GroupKey, wrap: &Event) -> Result<Opened, StreamError> {
    let seal_json = decrypt_to_bytes(group.conversation_key(), &base64_decode(&wrap.content)?)
        .map_err(|e| StreamError::Crypto(e.to_string()))?;
    let seal = Event::from_json(&seal_json).map_err(|e| StreamError::Malformed(e.to_string()))?;
    if seal.verify().is_err() {
        return Err(StreamError::BadSignature);
    }

    let seal_kind = seal.kind.as_u16();
    let (form, rumor_json) = match seal_kind {
        kind::SEAL_PLAINTEXT => (SealForm::Plaintext, seal.content.clone().into_bytes()),
        kind::SEAL_ENCRYPTED => {
            let pt = decrypt_to_bytes(group.conversation_key(), &base64_decode(&seal.content)?)
                .map_err(|e| StreamError::Crypto(e.to_string()))?;
            (SealForm::Encrypted, pt)
        }
        other => return Err(StreamError::NotASeal(other)),
    };

    let mut rumor =
        UnsignedEvent::from_json(&rumor_json).map_err(|e| StreamError::Malformed(e.to_string()))?;
    rumor.ensure_id();

    // Authorship is the seal's verified signature; a rumor claiming another
    // pubkey is a splice attempt by a keyholder.
    if rumor.pubkey != seal.pubkey {
        return Err(StreamError::AuthorMismatch);
    }

    Ok(Opened {
        author: seal.pubkey,
        seal_form: form,
        wrap_id: wrap.id,
        seal_content: seal.content.clone(),
        rumor,
    })
}

/// CORD-03 §3 binding check: the rumor's committed `channel`/`epoch` tags must
/// strict-equal the Channel and epoch whose key decrypted the wrap, or a
/// keyholder could re-wrap a message into a context its author never chose.
pub fn check_binding(rumor: &UnsignedEvent, channel: &ChannelId, epoch: Epoch) -> Result<(), StreamError> {
    match tag_value(&rumor.tags, TAG_CHANNEL) {
        Some(c) if c == channel.to_hex() => {}
        _ => return Err(StreamError::BindingMismatch),
    }
    match tag_value(&rumor.tags, TAG_EPOCH) {
        Some(e) if e == epoch.0.to_string() => {}
        _ => return Err(StreamError::BindingMismatch),
    }
    Ok(())
}

fn base64_encode(bytes: &[u8]) -> String {
    base64_simd::STANDARD.encode_to_string(bytes)
}

fn base64_decode(s: &str) -> Result<Vec<u8>, StreamError> {
    base64_simd::STANDARD
        .decode_to_vec(s.as_bytes())
        .map_err(|e| StreamError::Malformed(format!("base64: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concord::v2::derive;
    use crate::concord::v2::{CommunityId, CommunityRoot};

    fn group() -> GroupKey {
        derive::control_key(&CommunityRoot([7u8; 32]), &CommunityId([0x11u8; 32]), Epoch(0))
    }

    fn channel_group() -> GroupKey {
        derive::public_channel_key(&CommunityRoot([7u8; 32]), &ChannelId([0x42u8; 32]), Epoch(0))
    }

    fn message_rumor(author: &Keys, ms: u64) -> UnsignedEvent {
        build_rumor(
            author.public_key(),
            kind::MESSAGE,
            "Hey chat!",
            &ChannelId([0x42u8; 32]),
            Epoch(0),
            ms,
            vec![],
        )
    }

    #[test]
    fn encrypted_seal_roundtrip() {
        let author = Keys::generate();
        let g = channel_group();
        let rumor = message_rumor(&author, 1_686_840_217_417);
        let wrap = wrap_rumor(&g, &author, &rumor, SealForm::Encrypted, Keys::generate().public_key()).unwrap();

        assert_eq!(wrap.kind.as_u16(), kind::WRAP);
        assert_eq!(wrap.pubkey, g.public_key(), "wrap is authored by the stream key");
        // created_at is never tweaked (CORD-01) — it matches the rumor's seconds.
        assert_eq!(wrap.created_at.as_secs(), 1_686_840_217);

        let opened = open(&g, &wrap).unwrap();
        assert_eq!(opened.author, author.public_key());
        assert_eq!(opened.seal_form, SealForm::Encrypted);
        assert_eq!(opened.rumor.content, "Hey chat!");
        assert_eq!(opened.timestamp_ms(), Some(1_686_840_217_417));
        check_binding(&opened.rumor, &ChannelId([0x42u8; 32]), Epoch(0)).unwrap();
    }

    #[test]
    fn plaintext_seal_roundtrip_is_byte_verbatim() {
        let author = Keys::generate();
        let g = group();
        let mut rumor = build_plane_rumor(author.public_key(), kind::CONTROL_EDITION, "{}", 1_700_000_000_123, vec![]);
        rumor.ensure_id();
        let wrap = wrap_rumor(&g, &author, &rumor, SealForm::Plaintext, Keys::generate().public_key()).unwrap();
        let opened = open(&g, &wrap).unwrap();
        assert_eq!(opened.seal_form, SealForm::Plaintext);
        // The seal's content is the rumor's exact serialized JSON.
        assert_eq!(opened.seal_content, rumor.as_json());
        assert_eq!(opened.rumor.id, rumor.id, "rumor id must be stable across the envelope");
    }

    #[test]
    fn wrong_key_cannot_open() {
        let author = Keys::generate();
        let g = channel_group();
        let wrap = wrap_rumor(&g, &author, &message_rumor(&author, 1), SealForm::Encrypted, Keys::generate().public_key()).unwrap();
        let other = derive::public_channel_key(&CommunityRoot([8u8; 32]), &ChannelId([0x42u8; 32]), Epoch(0));
        assert!(open(&other, &wrap).is_err());
        // The next epoch's key is a different universe too.
        let next = derive::public_channel_key(&CommunityRoot([7u8; 32]), &ChannelId([0x42u8; 32]), Epoch(1));
        assert!(open(&next, &wrap).is_err());
    }

    #[test]
    fn author_mismatch_is_rejected() {
        // A keyholder crafts a seal signed by themselves around a rumor
        // claiming someone else's pubkey.
        let mallory = Keys::generate();
        let victim = Keys::generate();
        let g = channel_group();
        let rumor = message_rumor(&victim, 1_686_840_217_417); // claims victim
        let wrap = wrap_rumor(&g, &mallory, &rumor, SealForm::Encrypted, Keys::generate().public_key()).unwrap();
        assert!(matches!(open(&g, &wrap), Err(StreamError::AuthorMismatch)));
    }

    #[test]
    fn tampered_seal_signature_is_rejected() {
        let author = Keys::generate();
        let g = channel_group();
        let rumor = message_rumor(&author, 5);
        // Build a seal whose signature is from a different event.
        let fake_seal = EventBuilder::new(Kind::Custom(kind::SEAL_ENCRYPTED), "junk")
            .build(author.public_key());
        let signed_other = EventBuilder::new(Kind::Custom(kind::SEAL_ENCRYPTED), "other")
            .build(author.public_key())
            .sign_with_keys(&author)
            .unwrap();
        // Graft the other event's sig onto this content.
        let mut forged = serde_json::to_value(&fake_seal).unwrap();
        forged["sig"] = serde_json::json!(signed_other.sig.to_string());
        forged["id"] = serde_json::json!(signed_other.id.to_string());
        forged["pubkey"] = serde_json::json!(author.public_key().to_string());
        forged["created_at"] = serde_json::json!(1_686_840_217);
        forged["kind"] = serde_json::json!(kind::SEAL_ENCRYPTED);
        forged["tags"] = serde_json::json!([]);
        let ct = encrypt_to_bytes(g.conversation_key(), forged.to_string().as_bytes()).unwrap();
        let wrap = EventBuilder::new(Kind::Custom(kind::WRAP), base64_encode(&ct))
            .tags([Tag::public_key(Keys::generate().public_key())])
            .build(g.public_key())
            .sign_with_keys(g.keys())
            .unwrap();
        assert!(matches!(open(&g, &wrap), Err(StreamError::BadSignature)));
        let _ = rumor;
    }

    #[test]
    fn binding_mismatch_detects_splice() {
        let author = Keys::generate();
        let g = channel_group();
        let wrap = wrap_rumor(&g, &author, &message_rumor(&author, 9), SealForm::Encrypted, Keys::generate().public_key()).unwrap();
        let opened = open(&g, &wrap).unwrap();
        // Replayed against a different channel or epoch: strict-equal fails.
        assert!(check_binding(&opened.rumor, &ChannelId([0x43u8; 32]), Epoch(0)).is_err());
        assert!(check_binding(&opened.rumor, &ChannelId([0x42u8; 32]), Epoch(1)).is_err());
    }

    #[test]
    fn compaction_rewrap_preserves_signature_and_rumor_id() {
        // Read a plaintext-sealed edition from epoch 0, re-wrap it under
        // epoch 1's control key: same seal bytes, same author sig, same rumor.
        let author = Keys::generate();
        let root = CommunityRoot([7u8; 32]);
        let cid = CommunityId([0x11u8; 32]);
        let g0 = derive::control_key(&root, &cid, Epoch(0));
        let g1 = derive::control_key(&CommunityRoot([9u8; 32]), &cid, Epoch(1));

        let rumor = build_plane_rumor(author.public_key(), kind::CONTROL_EDITION, "{\"name\":\"x\"}", 1_700_000_000_000, vec![]);
        let wrap0 = wrap_rumor(&g0, &author, &rumor, SealForm::Plaintext, Keys::generate().public_key()).unwrap();
        let opened0 = open(&g0, &wrap0).unwrap();

        // The compactor holds the decrypted seal: reconstruct and re-wrap verbatim.
        let seal_json = decrypt_to_bytes(g0.conversation_key(), &base64_decode(&wrap0.content).unwrap()).unwrap();
        let seal = Event::from_json(&seal_json).unwrap();
        let wrap1 = wrap_signed_seal(&g1, &seal, kind::WRAP, Keys::generate().public_key()).unwrap();

        let opened1 = open(&g1, &wrap1).unwrap();
        assert_eq!(opened1.author, author.public_key());
        assert_eq!(opened1.rumor.id, opened0.rumor.id, "byte-verbatim re-wrap must preserve the rumor hash");
        assert_eq!(opened1.seal_content, opened0.seal_content);
    }

    #[test]
    fn ephemeral_wrap_uses_the_ephemeral_kind() {
        let author = Keys::generate();
        let g = channel_group();
        let rumor = build_rumor(author.public_key(), kind::TYPING, "", &ChannelId([0x42u8; 32]), Epoch(0), 45, vec![]);
        let seal_ct = encrypt_to_bytes(g.conversation_key(), rumor.as_json().as_bytes()).unwrap();
        let seal = EventBuilder::new(Kind::Custom(kind::SEAL_ENCRYPTED), base64_encode(&seal_ct))
            .custom_created_at(rumor.created_at)
            .build(author.public_key())
            .sign_with_keys(&author)
            .unwrap();
        let wrap = wrap_signed_seal(&g, &seal, kind::WRAP_EPHEMERAL, Keys::generate().public_key()).unwrap();
        assert_eq!(wrap.kind.as_u16(), kind::WRAP_EPHEMERAL);
        let opened = open(&g, &wrap).unwrap();
        assert_eq!(opened.rumor.kind.as_u16(), kind::TYPING);
    }

    #[test]
    fn oversize_layer_is_refused_at_publish() {
        let author = Keys::generate();
        let g = channel_group();
        let big = "x".repeat(NIP44_MAX_PLAINTEXT + 1);
        let rumor = build_rumor(author.public_key(), kind::MESSAGE, &big, &ChannelId([0x42u8; 32]), Epoch(0), 1, vec![]);
        assert!(matches!(
            wrap_rumor(&g, &author, &rumor, SealForm::Encrypted, Keys::generate().public_key()),
            Err(StreamError::TooLarge(_))
        ));
    }

    #[test]
    fn malformed_ms_tag_yields_no_timestamp() {
        let author = Keys::generate();
        let g = channel_group();
        // A hostile ms tag (1000 would smuggle a future second).
        let rumor = EventBuilder::new(Kind::Custom(kind::MESSAGE), "x")
            .tags([
                Tag::custom(TagKind::Custom(TAG_CHANNEL.into()), [ChannelId([0x42u8; 32]).to_hex()]),
                Tag::custom(TagKind::Custom(TAG_EPOCH.into()), ["0".to_string()]),
                Tag::custom(TagKind::Custom(TAG_MS.into()), ["1000".to_string()]),
            ])
            .custom_created_at(Timestamp::from_secs(1_686_840_217))
            .build(author.public_key());
        let wrap = wrap_rumor(&g, &author, &rumor, SealForm::Encrypted, Keys::generate().public_key()).unwrap();
        let opened = open(&g, &wrap).unwrap();
        assert_eq!(opened.timestamp_ms(), None, "ms=1000 is malformed: dropped, never interpreted");
    }
}
