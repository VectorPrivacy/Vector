//! CORD-01 Private Streams — the Concord v2 envelope.
//!
//! Every durable plane event is the same three-layer shape: a kind-1059 **wrap**
//! signed by the plane's derived group key (fixed author, random ephemeral `p`
//! tag — NIP-59 reversed), containing a **seal** signed by the author's real
//! key, containing the unsigned **rumor** that carries the functional kind.
//!
//! Two seal forms, fixed per plane (CORD-02 §5), declared by the seal's kind:
//!   - **20013 encrypted** (Chat, Guestbook, rekey planes): the rumor is
//!     NIP-44-encrypted *again* inside the already-encrypted wrap, so no layer
//!     can ever be lifted out as a standalone public event.
//!   - **20014 plaintext** (Control Plane ONLY): the seal's content is the
//!     rumor's serialized JSON string byte-verbatim, which is what lets a
//!     compaction re-wrap a signed edition into a new epoch with the signature
//!     intact. A re-wrap MUST carry those exact bytes forward, never
//!     re-serialize.
//!
//! Ephemeral actions (typing, voice presence) ride the identical structure at
//! kind 21059 — relays MUST NOT store it.
//!
//! NIP-44 hard-caps plaintext at 65,535 bytes and libraries are lenient, so the
//! cap is enforced HERE at every nesting layer — a lenient publisher mints
//! events a strict reader cannot decrypt.

use nostr_sdk::nips::nip44::v2::{decrypt_to_bytes, encrypt_to_bytes, ConversationKey};
use nostr_sdk::prelude::{Event, EventBuilder, EventId, JsonUtil, Keys, Kind, PublicKey, Tag, TagKind, Timestamp, UnsignedEvent};

use super::super::{ChannelId, Epoch};
use super::derive::GroupKey;

/// Durable stream wrap.
pub const KIND_WRAP: u16 = 1059;
/// Ephemeral stream wrap — identical structure, relays MUST NOT store it.
pub const KIND_WRAP_EPHEMERAL: u16 = 21059;
/// Encrypted seal (Chat / Guestbook / rekey planes).
pub const KIND_SEAL_ENCRYPTED: u16 = 20013;
/// Plaintext seal (Control Plane only).
pub const KIND_SEAL_PLAINTEXT: u16 = 20014;

/// NIP-44 v2 plaintext hard cap, enforced at every nesting layer.
pub const NIP44_MAX_PLAINTEXT: usize = 65_535;

const TAG_MS: &str = "ms";
const TAG_CHANNEL: &str = "channel";
const TAG_EPOCH: &str = "epoch";

/// Which seal form a plane uses (CORD-02 §5) — a fixed property of the plane,
/// never a per-message choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealForm {
    Encrypted,
    Plaintext,
}

impl SealForm {
    pub fn kind(self) -> u16 {
        match self {
            SealForm::Encrypted => KIND_SEAL_ENCRYPTED,
            SealForm::Plaintext => KIND_SEAL_PLAINTEXT,
        }
    }

    fn from_kind(kind: u16) -> Option<Self> {
        match kind {
            KIND_SEAL_ENCRYPTED => Some(SealForm::Encrypted),
            KIND_SEAL_PLAINTEXT => Some(SealForm::Plaintext),
            _ => None,
        }
    }
}

/// Errors from building or opening a v2 stream event.
#[derive(Debug)]
pub enum StreamError {
    Sign(String),
    Encrypt(String),
    Decrypt(String),
    Parse(String),
    /// A plaintext (wrap/seal/rumor JSON) exceeds the NIP-44 65,535-byte cap.
    Oversize(usize),
    /// Outer kind is neither 1059 nor 21059.
    BadWrapKind(u16),
    /// Wrap author isn't this plane's group key — not this stream's event.
    WrongStream,
    /// Seal kind is neither 20013 nor 20014.
    BadSealKind(u16),
    /// The seal's Schnorr signature (or id) failed to verify.
    BadSealSignature,
    /// Rumor pubkey ≠ seal pubkey — the seal doesn't vouch for this author.
    AuthorMismatch,
    /// Rumor's claimed id ≠ the hash of its serialized form.
    BadRumorId,
    /// `ms` tag present but not an integer in 0..=999 — the event is malformed
    /// and MUST be dropped, never clamped or interpreted.
    BadMs,
    /// Inner channel id ≠ the channel whose key decrypted this (cross-channel splice).
    ChannelMismatch,
    /// Inner epoch ≠ the epoch whose key decrypted this (cross-epoch splice/replay).
    EpochMismatch,
    MissingTag(&'static str),
    /// A binding tag appears more than once — ambiguous, rejected.
    DuplicateTag(&'static str),
    /// `rewrap` was handed a non-plaintext seal — only 20014 survives re-wrapping
    /// (a signature over ciphertext binds the old key).
    NotRewrappable,
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamError::Sign(e) => write!(f, "sign: {e}"),
            StreamError::Encrypt(e) => write!(f, "encrypt: {e}"),
            StreamError::Decrypt(e) => write!(f, "decrypt: {e}"),
            StreamError::Parse(e) => write!(f, "parse: {e}"),
            StreamError::Oversize(n) => write!(f, "plaintext {n} bytes exceeds NIP-44 cap"),
            StreamError::BadWrapKind(k) => write!(f, "not a stream wrap kind: {k}"),
            StreamError::WrongStream => write!(f, "wrap author is not this stream"),
            StreamError::BadSealKind(k) => write!(f, "not a seal kind: {k}"),
            StreamError::BadSealSignature => write!(f, "seal signature invalid"),
            StreamError::AuthorMismatch => write!(f, "rumor pubkey != seal pubkey"),
            StreamError::BadRumorId => write!(f, "rumor id != computed hash"),
            StreamError::BadMs => write!(f, "ms tag outside 0..=999"),
            StreamError::ChannelMismatch => write!(f, "channel-binding mismatch (splice)"),
            StreamError::EpochMismatch => write!(f, "epoch-binding mismatch (splice/replay)"),
            StreamError::MissingTag(t) => write!(f, "missing rumor tag: {t}"),
            StreamError::DuplicateTag(t) => write!(f, "duplicate rumor tag: {t}"),
            StreamError::NotRewrappable => write!(f, "only plaintext seals survive re-wrapping"),
        }
    }
}

impl std::error::Error for StreamError {}

/// A fully verified, opened stream event.
#[derive(Debug, Clone)]
pub struct OpenedStream {
    /// The unsigned rumor, id-verified against its serialized form.
    pub rumor: UnsignedEvent,
    /// The rumor's verified id (content-derived — the protocol's message id).
    pub rumor_id: EventId,
    /// The real author: the seal's Schnorr-verified pubkey (== rumor pubkey).
    pub author: PublicKey,
    /// Which seal form carried it.
    pub seal_form: SealForm,
    /// The verified seal, retained so a compaction can re-wrap a plaintext seal
    /// byte-verbatim (its content string carries the signed rumor bytes).
    pub seal: Event,
    /// The outer wrap's id (per-transport identity; differs per re-wrap).
    pub wrapper_id: EventId,
    /// True event time in ms: `created_at * 1000 + ms-tag` (tag absent = 0).
    pub at_ms: u64,
}

// ── millisecond ordering (CORD-02 §4) ────────────────────────────────────────

/// Split a full epoch-ms send time into (`created_at` seconds, `ms` remainder).
pub fn split_ms(at_ms: u64) -> (u64, u16) {
    (at_ms / 1000, (at_ms % 1000) as u16)
}

/// Resolve a rumor's true millisecond time — STRICT per CORD-02 §5: an absent
/// `ms` tag is offset 0; ANY present `ms` tag that isn't a lone integer in
/// 0..=999 makes the event malformed (`BadMs` — drop it, never clamp), or the
/// excess would smuggle arbitrary "future" past the coalesce clock checks.
///
/// A present-but-valueless `["ms"]` and a duplicated `ms` tag both count as
/// malformed here — the generic `unique_tag_unsigned` treats a valueless tag as
/// absent (correct for the binding path, which then rejects true absence as
/// MissingTag), but for `ms` "present yet uninterpretable" must be BadMs, not a
/// silent default, so this scans every occurrence of the tag name directly.
pub fn resolve_ms_strict(rumor: &UnsignedEvent) -> Result<u64, StreamError> {
    let secs = rumor.created_at.as_secs();
    let mut offset: Option<u64> = None;
    for t in rumor.tags.iter() {
        let s = t.as_slice();
        if s.first().map(|k| k.as_str()) != Some(TAG_MS) {
            continue;
        }
        if offset.is_some() {
            // A second ms occurrence (valued or not) is ambiguous.
            return Err(StreamError::BadMs);
        }
        // Present but valueless, or not a lone 0..=999 decimal without leading
        // zeros — malformed. Digit-only FIRST: `u64::from_str` would otherwise
        // accept a leading `+` ("+5", "+000"), a second byte-encoding a strict peer
        // rejects — the exact cross-impl divergence this gate exists to prevent.
        let raw = s.get(1).ok_or(StreamError::BadMs)?;
        if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
            return Err(StreamError::BadMs);
        }
        let n: u64 = raw.parse().map_err(|_| StreamError::BadMs)?;
        if n > 999 || (raw.len() > 1 && raw.starts_with('0')) {
            return Err(StreamError::BadMs);
        }
        offset = Some(n);
    }
    Ok(secs.saturating_mul(1000).saturating_add(offset.unwrap_or(0)))
}

// ── build side ───────────────────────────────────────────────────────────────

/// Build an unsigned rumor carrying a full epoch-ms timestamp: `created_at`
/// takes the seconds, an `["ms", <0..999>]` tag the remainder.
pub fn build_rumor_ms(
    kind: u16,
    author: PublicKey,
    content: &str,
    mut tags: Vec<Tag>,
    at_ms: u64,
) -> UnsignedEvent {
    let (secs, offset) = split_ms(at_ms);
    tags.push(Tag::custom(TagKind::Custom(TAG_MS.into()), [offset.to_string()]));
    build_rumor_secs(kind, author, content, tags, secs)
}

/// Build an unsigned rumor with a plain seconds timestamp and NO `ms` tag —
/// the Control Plane shape (editions fold by version, not time).
pub fn build_rumor_secs(
    kind: u16,
    author: PublicKey,
    content: &str,
    tags: Vec<Tag>,
    at_secs: u64,
) -> UnsignedEvent {
    let mut rumor = EventBuilder::new(Kind::Custom(kind), content)
        .tags(tags)
        .custom_created_at(Timestamp::from_secs(at_secs))
        .build(author);
    rumor.ensure_id();
    rumor
}

/// The seal's `content` for a rumor: NIP-44 ciphertext under the stream's
/// conversation key (encrypted form) or the rumor's serialized JSON string
/// verbatim (plaintext form). Split from signing so the caller can sign with
/// local keys OR a NIP-46 bunker — the seal is `(seal_form.kind(), content,
/// created_at = rumor.created_at)` signed by the author's real key.
pub fn seal_content(rumor: &UnsignedEvent, form: SealForm, group: &GroupKey) -> Result<String, StreamError> {
    let json = rumor.as_json();
    cap(json.len())?;
    match form {
        SealForm::Plaintext => Ok(json),
        SealForm::Encrypted => {
            let ct = encrypt_to_bytes(group.conv_key(), json.as_bytes()).map_err(|e| StreamError::Encrypt(e.to_string()))?;
            Ok(base64_simd::STANDARD.encode_to_string(&ct))
        }
    }
}

/// Local-keys convenience: build + sign the seal in one step. Wire-identical to
/// the split path (`seal_content` + caller-side signing), which bunker accounts
/// use instead.
pub fn build_seal(rumor: &UnsignedEvent, form: SealForm, group: &GroupKey, author_keys: &Keys) -> Result<Event, StreamError> {
    let content = seal_content(rumor, form, group)?;
    EventBuilder::new(Kind::Custom(form.kind()), content)
        .custom_created_at(rumor.created_at)
        .sign_with_keys(author_keys)
        .map_err(|e| StreamError::Sign(e.to_string()))
}

/// Wrap a signed seal into the outer stream event: content = NIP-44 under the
/// stream conversation key, signed by the group key, one random ephemeral `p`
/// tag (NIP-59 reversed). Returns the wrap and the ephemeral `p` keypair — a
/// client MAY retain the latter to best-effort NIP-09-scrub the wrap later.
///
/// `wrap_kind` is [`KIND_WRAP`] or [`KIND_WRAP_EPHEMERAL`]; `wrap_at` is the
/// wrap's `created_at` (untweaked wall clock — CORD-01 forbids NIP-59's
/// timestamp tweak on stream events).
pub fn wrap_seal(seal: &Event, group: &GroupKey, wrap_kind: u16, wrap_at: Timestamp) -> Result<(Event, Keys), StreamError> {
    if wrap_kind != KIND_WRAP && wrap_kind != KIND_WRAP_EPHEMERAL {
        return Err(StreamError::BadWrapKind(wrap_kind));
    }
    let seal_json = seal.as_json();
    cap(seal_json.len())?;
    let ct = encrypt_to_bytes(group.conv_key(), seal_json.as_bytes()).map_err(|e| StreamError::Encrypt(e.to_string()))?;
    let ephemeral = Keys::generate();
    let wrap = EventBuilder::new(Kind::Custom(wrap_kind), base64_simd::STANDARD.encode_to_string(&ct))
        .tags([Tag::public_key(ephemeral.public_key())])
        .custom_created_at(wrap_at)
        .sign_with_keys(group.keys())
        .map_err(|e| StreamError::Sign(e.to_string()))?;
    Ok((wrap, ephemeral))
}

/// Re-wrap an already-verified PLAINTEXT seal into another stream (a compaction
/// carrying a signed edition into a new epoch). The seal event is carried
/// whole — its content string holds the rumor bytes verbatim, so the rumor id
/// and the author's signature survive.
pub fn rewrap_seal(seal: &Event, new_group: &GroupKey, wrap_at: Timestamp) -> Result<(Event, Keys), StreamError> {
    if seal.kind.as_u16() != KIND_SEAL_PLAINTEXT {
        return Err(StreamError::NotRewrappable);
    }
    wrap_seal(seal, new_group, KIND_WRAP, wrap_at)
}

// ── open side ────────────────────────────────────────────────────────────────

/// Open and fully verify a stream wrap against the plane's group key.
///
/// Verification chain: wrap kind → wrap author == stream address → decrypt
/// (the NIP-44 MAC under the members-only conversation key is the envelope
/// gate — the wrap's own signature adds nothing an outsider couldn't also
/// forge-or-not, so it isn't re-checked) → seal kind → seal Schnorr verify →
/// rumor recover → rumor.pubkey == seal.pubkey → rumor.id == computed hash
/// (never trust a claimed id) → strict ms resolve.
pub fn open_wrap(wrap: &Event, group: &GroupKey) -> Result<OpenedStream, StreamError> {
    let wrap_kind = wrap.kind.as_u16();
    if wrap_kind != KIND_WRAP && wrap_kind != KIND_WRAP_EPHEMERAL {
        return Err(StreamError::BadWrapKind(wrap_kind));
    }
    if wrap.pubkey != group.pk() {
        return Err(StreamError::WrongStream);
    }

    let seal_json = open_nip44(group.conv_key(), &wrap.content)?;
    let seal: Event = Event::from_json(&seal_json).map_err(|e| StreamError::Parse(e.to_string()))?;
    let seal_form = SealForm::from_kind(seal.kind.as_u16()).ok_or(StreamError::BadSealKind(seal.kind.as_u16()))?;
    seal.verify().map_err(|_| StreamError::BadSealSignature)?;

    let rumor_json = match seal_form {
        SealForm::Plaintext => seal.content.clone(),
        SealForm::Encrypted => open_nip44(group.conv_key(), &seal.content)?,
    };
    let mut rumor: UnsignedEvent = UnsignedEvent::from_json(rumor_json.as_bytes()).map_err(|e| StreamError::Parse(e.to_string()))?;

    if rumor.pubkey != seal.pubkey {
        return Err(StreamError::AuthorMismatch);
    }
    // Never trust a claimed id: recompute from the serialized fields
    // unconditionally (`ensure_id` is a no-op when an id is present, so it
    // would wave a forged one through). An absent id just takes the computed one.
    let computed = EventId::new(&rumor.pubkey, &rumor.created_at, &rumor.kind, &rumor.tags, &rumor.content);
    if let Some(claimed) = rumor.id {
        if claimed != computed {
            return Err(StreamError::BadRumorId);
        }
    }
    rumor.id = Some(computed);
    let at_ms = resolve_ms_strict(&rumor)?;

    Ok(OpenedStream {
        rumor_id: computed,
        author: seal.pubkey,
        seal_form,
        seal,
        wrapper_id: wrap.id,
        at_ms,
        rumor,
    })
}

/// Enforce the Chat-plane binding (CORD-03 §3): the rumor MUST commit
/// `["channel", id]` + `["epoch", n]`, strict-equal to the coordinate whose key
/// decrypted the wrap; a mismatch (or duplicate/absent tag) is a splice — drop.
pub fn check_channel_binding(rumor: &UnsignedEvent, channel_id: &ChannelId, epoch: Epoch) -> Result<(), StreamError> {
    match unique_tag_unsigned(rumor, TAG_CHANNEL)? {
        Some(c) if c == channel_id.to_hex() => {}
        Some(_) => return Err(StreamError::ChannelMismatch),
        None => return Err(StreamError::MissingTag(TAG_CHANNEL)),
    }
    match unique_tag_unsigned(rumor, TAG_EPOCH)? {
        Some(e) if e == epoch.0.to_string() => {}
        Some(_) => return Err(StreamError::EpochMismatch),
        None => return Err(StreamError::MissingTag(TAG_EPOCH)),
    }
    Ok(())
}

/// The standard chat binding tags for a rumor: `["channel", id]` + `["epoch", n]`.
pub fn channel_binding_tags(channel_id: &ChannelId, epoch: Epoch) -> Vec<Tag> {
    vec![
        Tag::custom(TagKind::Custom(TAG_CHANNEL.into()), [channel_id.to_hex()]),
        Tag::custom(TagKind::Custom(TAG_EPOCH.into()), [epoch.0.to_string()]),
    ]
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn cap(len: usize) -> Result<(), StreamError> {
    if len > NIP44_MAX_PLAINTEXT {
        return Err(StreamError::Oversize(len));
    }
    Ok(())
}

fn open_nip44(conv_key: &ConversationKey, content_b64: &str) -> Result<String, StreamError> {
    let ct = base64_simd::STANDARD
        .decode_to_vec(content_b64.as_bytes())
        .map_err(|e| StreamError::Decrypt(e.to_string()))?;
    let pt = decrypt_to_bytes(conv_key, &ct).map_err(|e| StreamError::Decrypt(e.to_string()))?;
    String::from_utf8(pt).map_err(|e| StreamError::Parse(e.to_string()))
}

/// Value of the tag named `name` on an unsigned rumor, requiring it to appear
/// AT MOST ONCE (any keyholder can craft a rumor; a duplicated binding tag
/// makes first-match nondeterministic — reject).
fn unique_tag_unsigned(rumor: &UnsignedEvent, name: &'static str) -> Result<Option<String>, StreamError> {
    let mut found: Option<String> = None;
    for t in rumor.tags.iter() {
        let s = t.as_slice();
        if s.len() >= 2 && s[0] == name {
            if found.is_some() {
                return Err(StreamError::DuplicateTag(name));
            }
            found = Some(s[1].clone());
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::super::super::{ChannelId, Epoch};
    use super::super::derive::channel_group_key;
    use super::super::kind;
    use super::*;

    fn group() -> GroupKey {
        channel_group_key(&[7u8; 32], &chan(), Epoch(0))
    }

    fn chan() -> ChannelId {
        ChannelId([0xabu8; 32])
    }

    fn send(author: &Keys, group: &GroupKey, form: SealForm, content: &str, at_ms: u64) -> Event {
        let tags = channel_binding_tags(&chan(), Epoch(0));
        let rumor = build_rumor_ms(kind::MESSAGE, author.public_key(), content, tags, at_ms);
        let seal = build_seal(&rumor, form, group, author).unwrap();
        wrap_seal(&seal, group, KIND_WRAP, Timestamp::from_secs(1_700_000_000)).unwrap().0
    }

    #[test]
    fn encrypted_round_trip_preserves_author_content_and_ms() {
        let author = Keys::generate();
        let wrap = send(&author, &group(), SealForm::Encrypted, "Hey chat!", 1_686_840_217_417);
        assert_eq!(wrap.kind.as_u16(), KIND_WRAP);
        assert_eq!(wrap.pubkey, group().pk(), "wrap is signed by the stream key");

        let opened = open_wrap(&wrap, &group()).unwrap();
        assert_eq!(opened.author, author.public_key());
        assert_eq!(opened.rumor.content, "Hey chat!");
        assert_eq!(opened.at_ms, 1_686_840_217_417);
        assert_eq!(opened.seal_form, SealForm::Encrypted);
        check_channel_binding(&opened.rumor, &chan(), Epoch(0)).unwrap();
    }

    #[test]
    fn plaintext_seal_round_trip_carries_rumor_verbatim() {
        let author = Keys::generate();
        let wrap = send(&author, &group(), SealForm::Plaintext, "an edition", 1_686_840_217_000);
        let opened = open_wrap(&wrap, &group()).unwrap();
        assert_eq!(opened.seal_form, SealForm::Plaintext);
        // The seal's content IS the rumor's JSON — the compaction contract.
        assert_eq!(opened.seal.content, opened.rumor.as_json());
    }

    #[test]
    fn wrong_stream_key_cannot_open() {
        let author = Keys::generate();
        let wrap = send(&author, &group(), SealForm::Encrypted, "secret", 1_000);
        let other = channel_group_key(&[8u8; 32], &chan(), Epoch(0));
        // Different address entirely → WrongStream before any decrypt attempt.
        assert!(matches!(open_wrap(&wrap, &other), Err(StreamError::WrongStream)));
    }

    #[test]
    fn tampered_wrap_content_fails_the_mac() {
        let author = Keys::generate();
        let mut wrap = send(&author, &group(), SealForm::Encrypted, "x", 1_000);
        let mut json: serde_json::Value = serde_json::from_str(&wrap.as_json()).unwrap();
        let ct = json["content"].as_str().unwrap().to_string();
        // Flip a mid-payload character (the first char of a NIP-44 base64 payload
        // is always 'A' — the 0x02 version byte — so tampering there is a no-op).
        let mut bytes = ct.into_bytes();
        bytes[20] = if bytes[20] == b'B' { b'C' } else { b'B' };
        json["content"] = serde_json::Value::String(String::from_utf8(bytes).unwrap());
        wrap = Event::from_json(json.to_string()).unwrap();
        assert!(matches!(open_wrap(&wrap, &group()), Err(StreamError::Decrypt(_))));
    }

    #[test]
    fn forged_seal_signature_is_rejected() {
        let author = Keys::generate();
        let impostor = Keys::generate();
        let tags = channel_binding_tags(&chan(), Epoch(0));
        let rumor = build_rumor_ms(kind::MESSAGE, author.public_key(), "hi", tags, 1_000);
        // Seal signed by the impostor but CLAIMING the author's pubkey: rebuild
        // the seal event JSON with a swapped pubkey — the sig no longer matches.
        let seal = build_seal(&rumor, SealForm::Encrypted, &group(), &impostor).unwrap();
        let mut json: serde_json::Value = serde_json::from_str(&seal.as_json()).unwrap();
        json["pubkey"] = serde_json::Value::String(author.public_key().to_hex());
        // (id also changes with pubkey — recompute is not attempted; both id and
        // sig checks are downstream of Event::from_json/verify.)
        let forged = Event::from_json(json.to_string());
        let Ok(forged) = forged else { return }; // strict parsers may reject outright — equally a pass
        let (wrap, _) = wrap_seal(&forged, &group(), KIND_WRAP, Timestamp::from_secs(1)).unwrap();
        assert!(matches!(open_wrap(&wrap, &group()), Err(StreamError::BadSealSignature)));
    }

    #[test]
    fn rumor_author_must_match_seal_author() {
        let author = Keys::generate();
        let other = Keys::generate();
        let tags = channel_binding_tags(&chan(), Epoch(0));
        // Rumor claims `other` as its author, but the seal is signed by `author`.
        let rumor = build_rumor_ms(kind::MESSAGE, other.public_key(), "spoof", tags, 1_000);
        let seal = build_seal(&rumor, SealForm::Encrypted, &group(), &author).unwrap();
        let (wrap, _) = wrap_seal(&seal, &group(), KIND_WRAP, Timestamp::from_secs(1)).unwrap();
        assert!(matches!(open_wrap(&wrap, &group()), Err(StreamError::AuthorMismatch)));
    }

    #[test]
    fn forged_rumor_id_is_rejected() {
        let author = Keys::generate();
        let tags = channel_binding_tags(&chan(), Epoch(0));
        let rumor = build_rumor_ms(kind::MESSAGE, author.public_key(), "real", tags, 1_000);
        let mut json: serde_json::Value = serde_json::from_str(&rumor.as_json()).unwrap();
        json["id"] = serde_json::Value::String("00".repeat(32));
        let forged_json = json.to_string();
        // Hand-build a seal around the forged rumor bytes (plaintext form so the
        // bytes ride verbatim).
        let seal = EventBuilder::new(Kind::Custom(KIND_SEAL_PLAINTEXT), forged_json)
            .custom_created_at(rumor.created_at)
            .sign_with_keys(&author)
            .unwrap();
        let (wrap, _) = wrap_seal(&seal, &group(), KIND_WRAP, Timestamp::from_secs(1)).unwrap();
        assert!(matches!(open_wrap(&wrap, &group()), Err(StreamError::BadRumorId)));
    }

    #[test]
    fn ms_is_strict_absent_is_zero_invalid_is_dropped() {
        let author = Keys::generate();
        // Absent ms = offset 0.
        let rumor = build_rumor_secs(kind::MESSAGE, author.public_key(), "x", vec![], 1_000);
        assert_eq!(resolve_ms_strict(&rumor).unwrap(), 1_000_000);
        // 999 is the max valid offset.
        let ok = build_rumor_secs(
            kind::MESSAGE,
            author.public_key(),
            "x",
            vec![Tag::custom(TagKind::Custom("ms".into()), ["999".to_string()])],
            1_000,
        );
        assert_eq!(resolve_ms_strict(&ok).unwrap(), 1_000_999);
        // 1000, negatives, non-integers, leading zeros, and a leading '+' (which
        // `u64::from_str` would otherwise accept as a second byte-encoding):
        // malformed — DROP, never clamp.
        for bad in ["1000", "-1", "12.5", "abc", "007", "", "+5", "+0", "+000", "+999"] {
            let r = build_rumor_secs(
                kind::MESSAGE,
                author.public_key(),
                "x",
                vec![Tag::custom(TagKind::Custom("ms".into()), [bad.to_string()])],
                1_000,
            );
            assert!(
                matches!(resolve_ms_strict(&r), Err(StreamError::BadMs)),
                "ms={bad:?} must be malformed"
            );
        }
    }

    #[test]
    fn a_valueless_or_duplicated_ms_tag_is_malformed_not_silently_zero() {
        // A present-but-valueless ["ms"] must be BadMs, not treated as absent (a
        // silent offset-0 default would honor a rumor a spec-strict peer drops).
        let author = Keys::generate();
        let bare = build_rumor_secs(
            kind::MESSAGE,
            author.public_key(),
            "x",
            vec![Tag::custom(TagKind::Custom("ms".into()), Vec::<String>::new())],
            1_000,
        );
        assert!(matches!(resolve_ms_strict(&bare), Err(StreamError::BadMs)));
        // A valued ms plus a valueless one must not let the valued one win — two
        // ms occurrences are ambiguous.
        let two = build_rumor_secs(
            kind::MESSAGE,
            author.public_key(),
            "x",
            vec![
                Tag::custom(TagKind::Custom("ms".into()), Vec::<String>::new()),
                Tag::custom(TagKind::Custom("ms".into()), ["5".to_string()]),
            ],
            1_000,
        );
        assert!(matches!(resolve_ms_strict(&two), Err(StreamError::BadMs)));
        // Two valued ms tags are also ambiguous.
        let two_valued = build_rumor_secs(
            kind::MESSAGE,
            author.public_key(),
            "x",
            vec![
                Tag::custom(TagKind::Custom("ms".into()), ["1".to_string()]),
                Tag::custom(TagKind::Custom("ms".into()), ["2".to_string()]),
            ],
            1_000,
        );
        assert!(matches!(resolve_ms_strict(&two_valued), Err(StreamError::BadMs)));
    }

    #[test]
    fn binding_rejects_splices_and_duplicates() {
        let author = Keys::generate();
        let tags = channel_binding_tags(&chan(), Epoch(0));
        let rumor = build_rumor_ms(kind::MESSAGE, author.public_key(), "x", tags, 1_000);
        // Wrong channel and wrong epoch both reject.
        assert!(matches!(
            check_channel_binding(&rumor, &ChannelId([0xcd; 32]), Epoch(0)),
            Err(StreamError::ChannelMismatch)
        ));
        assert!(matches!(
            check_channel_binding(&rumor, &chan(), Epoch(1)),
            Err(StreamError::EpochMismatch)
        ));
        // A duplicated binding tag is ambiguous — rejected outright.
        let mut tags = channel_binding_tags(&chan(), Epoch(0));
        tags.extend(channel_binding_tags(&chan(), Epoch(0)));
        let dup = build_rumor_ms(kind::MESSAGE, author.public_key(), "x", tags, 1_000);
        assert!(matches!(
            check_channel_binding(&dup, &chan(), Epoch(0)),
            Err(StreamError::DuplicateTag(_))
        ));
        // Missing binding tags reject too.
        let bare = build_rumor_ms(kind::MESSAGE, author.public_key(), "x", vec![], 1_000);
        assert!(matches!(
            check_channel_binding(&bare, &chan(), Epoch(0)),
            Err(StreamError::MissingTag(_))
        ));
    }

    #[test]
    fn oversize_plaintext_is_refused_at_build_time() {
        let author = Keys::generate();
        let big = "x".repeat(NIP44_MAX_PLAINTEXT + 1);
        let rumor = build_rumor_ms(kind::MESSAGE, author.public_key(), &big, vec![], 1_000);
        assert!(matches!(
            seal_content(&rumor, SealForm::Encrypted, &group()),
            Err(StreamError::Oversize(_))
        ));
    }

    #[test]
    fn ephemeral_wrap_round_trips_and_bad_wrap_kind_rejects() {
        let author = Keys::generate();
        let tags = channel_binding_tags(&chan(), Epoch(0));
        let rumor = build_rumor_ms(kind::TYPING, author.public_key(), "", tags, 5_000);
        let seal = build_seal(&rumor, SealForm::Encrypted, &group(), &author).unwrap();
        let (wrap, _) = wrap_seal(&seal, &group(), KIND_WRAP_EPHEMERAL, Timestamp::from_secs(5)).unwrap();
        assert_eq!(wrap.kind.as_u16(), KIND_WRAP_EPHEMERAL);
        assert_eq!(open_wrap(&wrap, &group()).unwrap().rumor.kind.as_u16(), kind::TYPING);
        assert!(matches!(
            wrap_seal(&seal, &group(), 1058, Timestamp::from_secs(5)),
            Err(StreamError::BadWrapKind(1058))
        ));
    }

    #[test]
    fn rewrap_preserves_rumor_id_and_signature_across_epochs() {
        let author = Keys::generate();
        let wrap = send(&author, &group(), SealForm::Plaintext, "the head edition", 9_000);
        let opened = open_wrap(&wrap, &group()).unwrap();

        // Compaction: carry the verified seal into the next epoch's stream.
        let next = channel_group_key(&[7u8; 32], &chan(), Epoch(1));
        let (rewrapped, _) = rewrap_seal(&opened.seal, &next, Timestamp::from_secs(2_000)).unwrap();
        let reopened = open_wrap(&rewrapped, &next).unwrap();

        assert_eq!(reopened.rumor_id, opened.rumor_id, "rumor id survives the re-wrap");
        assert_eq!(reopened.author, author.public_key(), "authorship survives");
        assert_eq!(reopened.seal.sig, opened.seal.sig, "the original signature rides verbatim");
        assert_ne!(reopened.wrapper_id, opened.wrapper_id, "outer identity differs per wrap");

        // Encrypted seals must refuse to re-wrap (sig binds the old key's ciphertext).
        let enc = send(&author, &group(), SealForm::Encrypted, "no", 9_000);
        let enc_opened = open_wrap(&enc, &group()).unwrap();
        assert!(matches!(
            rewrap_seal(&enc_opened.seal, &next, Timestamp::from_secs(2_000)),
            Err(StreamError::NotRewrappable)
        ));
    }

    #[test]
    fn wrap_p_tag_is_ephemeral_not_the_stream_or_author() {
        let author = Keys::generate();
        let g = group();
        let tags = channel_binding_tags(&chan(), Epoch(0));
        let rumor = build_rumor_ms(kind::MESSAGE, author.public_key(), "x", tags, 1_000);
        let seal = build_seal(&rumor, SealForm::Encrypted, &g, &author).unwrap();
        let (wrap, ephemeral) = wrap_seal(&seal, &g, KIND_WRAP, Timestamp::from_secs(1)).unwrap();
        let p = wrap
            .tags
            .iter()
            .find_map(|t| {
                let s = t.as_slice();
                (s.len() >= 2 && s[0] == "p").then(|| s[1].clone())
            })
            .expect("wrap carries a p tag");
        assert_eq!(p, ephemeral.public_key().to_hex());
        assert_ne!(p, g.pk_hex());
        assert_ne!(p, author.public_key().to_hex());
    }
}
