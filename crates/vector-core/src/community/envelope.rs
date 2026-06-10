//! Message envelope (GROUP_PROTOCOL.md).
//!
//! A Community message is an inner Nostr event signed by the author's real key
//! (the intra-group authorship proof), NIP-44-v2-encrypted under the shared channel
//! key, and wrapped in an ephemeral-signed outer event tagged with the per-epoch
//! pseudonym. Single NIP-44 pass, O(1) broadcast — not gift wrap's per-recipient
//! double-wrap.
//!
//! `open_message` enforces the binding triad: inner Schnorr signature valid, and
//! inner `kind`/`channel`/`epoch` equal to the outer kind and to the *specific*
//! channel/epoch whose key decrypted the payload (strict equality, never a
//! membership test). That defeats insider replay/splice across type, channel, or
//! epoch — the threat that any member, holding the channel key, could otherwise lift
//! another member's signed content into a different context.

use nostr_sdk::prelude::*;

use super::cipher;
use super::derive::channel_pseudonym;
use super::{ChannelId, ChannelKey, Epoch};
use crate::stored_event::event_kind;

/// Outer protocol-version tag value (forward-compat hook #2). Checked before any
/// decryption so an unknown version is rejected gracefully.
const PROTOCOL_VERSION: &str = "1";

const TAG_VERSION: &str = "v";
const TAG_CHANNEL: &str = "channel";
const TAG_EPOCH: &str = "epoch";
const TAG_MS: &str = "ms";

/// Errors from sealing or opening a Community message envelope.
#[derive(Debug)]
pub enum EnvelopeError {
    Sign(String),
    Encrypt(String),
    Decrypt(String),
    InnerParse(String),
    /// Outer `v` tag is absent or names a version we don't speak.
    BadVersion(Option<String>),
    KindMismatch { outer: u16, inner: u16 },
    /// Inner channel id ≠ the channel whose key decrypted this (cross-channel splice).
    ChannelMismatch,
    /// Inner epoch ≠ the epoch whose key decrypted this (cross-epoch splice/replay).
    EpochMismatch,
    /// Inner author signature failed to verify.
    BadSignature,
    MissingTag(&'static str),
    /// A binding tag appears more than once — the inner event is ambiguous, so the
    /// wire form isn't deterministic. Any channel-key holder can craft the inner
    /// event, so we reject rather than trust first-match.
    DuplicateTag(&'static str),
    /// The outer `z` pseudonym matches none of the member's held epoch keys (an epoch we were never a
    /// recipient of, or a foreign channel). Not ours to read — dropped, not an error condition.
    NoHeldEpoch,
}

impl std::fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvelopeError::Sign(e) => write!(f, "sign: {e}"),
            EnvelopeError::Encrypt(e) => write!(f, "encrypt: {e}"),
            EnvelopeError::Decrypt(e) => write!(f, "decrypt: {e}"),
            EnvelopeError::InnerParse(e) => write!(f, "inner parse: {e}"),
            EnvelopeError::BadVersion(v) => write!(f, "unsupported protocol version: {v:?}"),
            EnvelopeError::KindMismatch { outer, inner } => {
                write!(f, "kind mismatch: outer {outer} != inner {inner}")
            }
            EnvelopeError::ChannelMismatch => write!(f, "channel-binding mismatch (splice)"),
            EnvelopeError::EpochMismatch => write!(f, "epoch-binding mismatch (splice/replay)"),
            EnvelopeError::BadSignature => write!(f, "inner author signature invalid"),
            EnvelopeError::MissingTag(t) => write!(f, "missing inner tag: {t}"),
            EnvelopeError::DuplicateTag(t) => write!(f, "duplicate inner tag: {t}"),
            EnvelopeError::NoHeldEpoch => write!(f, "no held epoch key for this pseudonym"),
        }
    }
}

impl std::error::Error for EnvelopeError {}

/// A successfully opened and fully-verified Community message.
#[derive(Debug, Clone)]
pub struct OpenedMessage {
    /// Inner event id — the `message_id`, the dedup/display key.
    pub message_id: EventId,
    /// Verified real author.
    pub author: PublicKey,
    pub content: String,
    pub channel_id: ChannelId,
    pub epoch: Epoch,
    /// Ordering timestamp (epoch ms) via the SHARED `rumor::resolve_message_timestamp` (ms convention +
    /// clamp). Kept here because the transport sorts fetched events by it before they become
    /// `Message`s; reply-ref + emoji parsing, by contrast, is solely `process_rumor`'s job off `tags`.
    pub ms: Option<u64>,
    /// Inner event's real send time (NOT randomized).
    pub created_at: Timestamp,
    /// Append-plane sub-kind: 3300 message, 3301 reaction, 3302 edit.
    pub kind: u16,
    /// File attachments (one per NIP-92 `imeta` tag). A Community message can carry a
    /// caption (`content`) plus N attachments in the same event — see `attachments` module.
    pub attachments: Vec<crate::types::Attachment>,
    /// The authority citation (`vac` tag), if the inner carried one — present on a non-owner
    /// moderation-hide (3305) naming the grant the hider claims authority under. The hide consumer
    /// (`apply_delete`) resolves it against the persisted grant head (version-pinned authority).
    pub citation: Option<super::edition::AuthorityCitation>,
    /// The OUTER wire event id (the relay-addressable event that carried this inner). The
    /// transport's dedup key — persisted as the inner's `wrapper_event_id` so a re-fetched
    /// channel page skips it pre-decryption, exactly as a DM gift-wrap id does.
    pub wrapper_id: EventId,
    /// The verified inner event's raw tags — handed to the SHARED `process_rumor` content parser so
    /// reply/emoji/ms parsing is identical across transports (the binding tags were already checked).
    pub tags: Tags,
}

/// Seal a plaintext message into an outer wire event. The outer event is signed by
/// a fresh one-time key (no persistent author↔channel linkage on the wire).
///
/// This convenience form discards that key, so the resulting message is NOT
/// self-deletable. **The product send path should use
/// [`seal_message_with_ephemeral`] and RETAIN the key** (like Vector's
/// `nip17_wrap_keys` for DMs) so the sender can later NIP-09-delete their own
/// message. Ephemeral-on-the-wire and retained-locally are complementary: the
/// relay still sees only one-time keys (no corpus-wide deletion authority), while
/// the sender keeps a per-message key to delete just their own.
pub fn seal_message(
    author_keys: &Keys,
    channel_key: &ChannelKey,
    channel_id: &ChannelId,
    epoch: Epoch,
    content: &str,
    ms: u64,
) -> Result<Event, EnvelopeError> {
    seal_message_with_ephemeral(&Keys::generate(), author_keys, channel_key, channel_id, epoch, content, ms)
}

/// Like [`seal_message`] but the caller supplies (and may retain) the ephemeral
/// outer-signing key. Retaining it enables a later NIP-09 deletion of this exact
/// outer event (the deletion must be signed by the same key — deletable
/// messages), which is also how on-relay tests clean up after themselves.
pub fn seal_message_with_ephemeral(
    ephemeral: &Keys,
    author_keys: &Keys,
    channel_key: &ChannelKey,
    channel_id: &ChannelId,
    epoch: Epoch,
    content: &str,
    ms: u64,
) -> Result<Event, EnvelopeError> {
    // Local-keys convenience: build the inner event, sign it with the author's keys, and
    // seal. Identical wire output to the signer path (golden-vector stable).
    let inner = build_inner_event(author_keys.public_key(), channel_id, epoch, content, ms, None)
        .sign_with_keys(author_keys)
        .map_err(|e| EnvelopeError::Sign(e.to_string()))?;
    seal_with_signed_inner(ephemeral, &inner, channel_key, channel_id, epoch)
}

/// Build the inner authorship-proof event UNSIGNED, so the caller can sign it with
/// whatever the account uses — local keys OR a NIP-46 remote bunker (parity with the
/// DM send path, which signs through the active `NostrSigner`). `author` is the
/// identity pubkey the signer will sign as.
///
/// `ms` is the full send time in epoch-milliseconds, split the way Vector's DMs do:
/// `created_at` carries the seconds, the `ms` tag carries only the sub-second offset
/// (0..999). The open side reconstructs `created_at*1000 + ms`.
pub fn build_inner_event(
    author: PublicKey,
    channel_id: &ChannelId,
    epoch: Epoch,
    content: &str,
    ms: u64,
    reply_to: Option<&str>,
) -> UnsignedEvent {
    build_inner_typed(author, channel_id, epoch, event_kind::COMMUNITY_MESSAGE, content, ms, reply_to, &[])
}

/// Like [`build_inner_event`] but for any append-plane sub-type — a reaction (3301) or
/// edit (3302) as well as a message (3300). The `reference` `e` tag points at the target:
/// the replied-to message (3300), the reacted-to message (3301), or the edited message
/// (3302). The inner kind is mirrored to the outer on seal, and the receiver enforces the
/// binding triad (kind/channel/epoch).
pub fn build_inner_typed(
    author: PublicKey,
    channel_id: &ChannelId,
    epoch: Epoch,
    kind: u16,
    content: &str,
    ms: u64,
    reference: Option<&str>,
    emoji_tags: &[crate::types::EmojiTag],
) -> UnsignedEvent {
    build_inner_full(author, channel_id, epoch, kind, content, ms, reference, emoji_tags, &[])
}

/// Like [`build_inner_typed`] but also carries a slice of EXTRA inner tags appended verbatim —
/// used for NIP-92 `imeta` attachment tags (a 3300 message mixing a caption with N files, via
/// `attachments::attachment_to_imeta`). They are added before signing, so the inner signature
/// covers them; readers pick out what they need by exact tag name.
pub fn build_inner_full(
    author: PublicKey,
    channel_id: &ChannelId,
    epoch: Epoch,
    kind: u16,
    content: &str,
    ms: u64,
    reference: Option<&str>,
    emoji_tags: &[crate::types::EmojiTag],
    extra_tags: &[Tag],
) -> UnsignedEvent {
    let created_secs = ms / 1000;
    let ms_offset = ms % 1000;
    let mut tags = vec![
        Tag::custom(TagKind::Custom(TAG_CHANNEL.into()), [channel_id.to_hex()]),
        Tag::custom(TagKind::Custom(TAG_EPOCH.into()), [epoch.0.to_string()]),
        Tag::custom(TagKind::Custom(TAG_MS.into()), [ms_offset.to_string()]),
    ];
    // Target reference: an `e` tag marked "reply" (Vector's DM convention) — the
    // replied-to / reacted-to / edited message's inner id.
    if let Some(target) = reference.filter(|t| !t.is_empty()) {
        tags.push(Tag::custom(TagKind::e(), [target.to_string(), String::new(), "reply".to_string()]));
    }
    // NIP-30 custom emoji: ["emoji", shortcode, url] for each `:shortcode:` used in the
    // content (so custom-emoji messages + reactions render the image, parity with DMs).
    for et in emoji_tags {
        tags.push(Tag::custom(TagKind::Custom("emoji".into()), [et.shortcode.clone(), et.url.clone()]));
    }
    // Extra inner tags appended verbatim (NIP-92 `imeta` attachment tags).
    tags.extend(extra_tags.iter().cloned());
    EventBuilder::new(Kind::Custom(kind), content)
        .tags(tags)
        .custom_created_at(Timestamp::from_secs(created_secs))
        .build(author)
}

/// Seal an already-signed inner authorship event into the outer wire event. The inner
/// may have been signed by local keys or a remote bunker — this stage is signer-agnostic.
/// Defensively re-checks the binding (kind/channel/epoch) so a caller can never seal
/// an inner that the receiver would then reject as a splice.
pub fn seal_with_signed_inner(
    ephemeral: &Keys,
    inner: &Event,
    channel_key: &ChannelKey,
    channel_id: &ChannelId,
    epoch: Epoch,
) -> Result<Event, EnvelopeError> {
    // Only the community append-plane sub-kinds (message/reaction/edit + cooperative delete +
    // presence + cooperative kick + webxdc peer signal) may be sealed. Rekey (3303) and InviteBundle
    // (3304) have their own carriers, so the contiguous 3300..=3302 range is admitted alongside the
    // explicit 3305/3306/3309/3310.
    let inner_kind = inner.kind.as_u16();
    let allowed = (event_kind::COMMUNITY_MESSAGE..=event_kind::COMMUNITY_EDIT).contains(&inner_kind)
        || inner_kind == event_kind::COMMUNITY_DELETE
        || inner_kind == event_kind::COMMUNITY_PRESENCE
        || inner_kind == event_kind::COMMUNITY_KICK
        || inner_kind == event_kind::COMMUNITY_WEBXDC;
    if !allowed {
        return Err(EnvelopeError::KindMismatch {
            outer: event_kind::COMMUNITY_MESSAGE,
            inner: inner_kind,
        });
    }
    match unique_tag(inner, TAG_CHANNEL)? {
        Some(c) if c == channel_id.to_hex() => {}
        _ => return Err(EnvelopeError::ChannelMismatch),
    }
    match unique_tag(inner, TAG_EPOCH)? {
        Some(e) if e == epoch.0.to_string() => {}
        _ => return Err(EnvelopeError::EpochMismatch),
    }

    // Single NIP-44 v2 pass under the raw channel key.
    let content_b64 = cipher::seal(channel_key.as_bytes(), inner.as_json().as_bytes())
        .map_err(EnvelopeError::Encrypt)?;

    // Outer event: ephemeral signer (no author↔channel linkage on the wire), tagged
    // with the per-epoch pseudonym (relay-filterable `z`) and the version. Outer kind
    // mirrors the inner (the binding the receiver enforces).
    let pseudonym = channel_pseudonym(channel_key, channel_id, epoch);
    EventBuilder::new(Kind::Custom(inner_kind), content_b64)
        .tags([
            Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Z)),
                [pseudonym.to_hex()],
            ),
            Tag::custom(TagKind::Custom(TAG_VERSION.into()), [PROTOCOL_VERSION.to_string()]),
        ])
        .sign_with_keys(ephemeral)
        .map_err(|e| EnvelopeError::Sign(e.to_string()))
}

/// Open and fully verify an outer wire event, given the channel key + the exact
/// channel/epoch coordinate that key belongs to.
pub fn open_message(
    outer: &Event,
    channel_key: &ChannelKey,
    channel_id: &ChannelId,
    epoch: Epoch,
) -> Result<OpenedMessage, EnvelopeError> {
    // Version-check BEFORE attempting decryption (hook #2).
    match find_tag(outer, TAG_VERSION).as_deref() {
        Some(PROTOCOL_VERSION) => {}
        other => return Err(EnvelopeError::BadVersion(other.map(str::to_string))),
    }

    let plaintext = cipher::open(channel_key.as_bytes(), &outer.content)
        .map_err(EnvelopeError::Decrypt)?;
    let json = String::from_utf8(plaintext).map_err(|e| EnvelopeError::InnerParse(e.to_string()))?;
    let inner = Event::from_json(&json).map_err(|e| EnvelopeError::InnerParse(e.to_string()))?;

    // Inner author signature (the authorship proof).
    inner.verify().map_err(|_| EnvelopeError::BadSignature)?;

    // Binding triad — type, channel, epoch.
    if inner.kind.as_u16() != outer.kind.as_u16() {
        return Err(EnvelopeError::KindMismatch {
            outer: outer.kind.as_u16(),
            inner: inner.kind.as_u16(),
        });
    }
    let inner_channel = unique_tag(&inner, TAG_CHANNEL)?.ok_or(EnvelopeError::MissingTag(TAG_CHANNEL))?;
    if inner_channel != channel_id.to_hex() {
        return Err(EnvelopeError::ChannelMismatch);
    }
    let inner_epoch = unique_tag(&inner, TAG_EPOCH)?.ok_or(EnvelopeError::MissingTag(TAG_EPOCH))?;
    if inner_epoch != epoch.0.to_string() {
        return Err(EnvelopeError::EpochMismatch);
    }

    // Reply-ref + emoji parsing is the SHARED parser's job (runs off `tags` in `process_rumor`), so it
    // lives in ONE place. The transport keeps only: the ordering `ms` (used to sort fetched events
    // before they're parsed — via the SAME shared resolver), NIP-92 `imeta` attachments, and the 
    // authority citation.
    let ms = Some(crate::rumor::resolve_message_timestamp(
        inner.created_at.as_secs(),
        unique_tag(&inner, TAG_MS)?.as_deref(),
    ));
    let attachments = super::attachments::attachments_from_tags(
        inner.tags.iter(),
        &crate::db::get_download_dir(),
    );
    Ok(OpenedMessage {
        message_id: inner.id,
        author: inner.pubkey,
        content: inner.content.clone(),
        channel_id: *channel_id,
        epoch,
        ms,
        created_at: inner.created_at,
        kind: inner.kind.as_u16(),
        attachments,
        citation: super::edition::AuthorityCitation::from_tags(&inner.tags),
        wrapper_id: outer.id,
        tags: inner.tags.clone(),
    })
}

/// Open an outer wire event when the member may hold MULTIPLE epoch keys (post-rekey catch-up): select
/// the decryption key by the outer's `z` pseudonym tag — each epoch addresses a distinct pseudonym we
/// can recompute — then open under that exact epoch. `epoch_keys` is the member's retained `(epoch, key)`
/// set for this channel. A `z` matching no held epoch yields [`EnvelopeError::NoHeldEpoch`] (not ours to
/// read), keeping the per-event cost one pseudonym derivation per held epoch (a handful), no trial-decrypt.
pub fn open_message_multi(
    outer: &Event,
    channel_id: &ChannelId,
    epoch_keys: &[(Epoch, ChannelKey)],
) -> Result<OpenedMessage, EnvelopeError> {
    let z = find_tag(outer, "z").ok_or(EnvelopeError::MissingTag("z"))?;
    for (epoch, key) in epoch_keys {
        if channel_pseudonym(key, channel_id, *epoch).to_hex() == z {
            return open_message(outer, key, channel_id, *epoch);
        }
    }
    Err(EnvelopeError::NoHeldEpoch)
}

/// First value of the first tag named `name` (for outer routing tags like `v`,
/// which the ephemeral signer controls and which carry no binding weight).
fn find_tag(event: &Event, name: &str) -> Option<String> {
    event.tags.iter().find_map(|t| {
        let s = t.as_slice();
        (s.len() >= 2 && s[0] == name).then(|| s[1].clone())
    })
}

/// Value of the tag named `name`, requiring it to appear AT MOST ONCE. The inner
/// event is constructable by any channel-key holder, so a duplicated binding tag
/// would make first-match nondeterministic — reject it.
fn unique_tag(event: &Event, name: &'static str) -> Result<Option<String>, EnvelopeError> {
    let mut found: Option<String> = None;
    for t in event.tags.iter() {
        let s = t.as_slice();
        if s.len() >= 2 && s[0] == name {
            if found.is_some() {
                return Err(EnvelopeError::DuplicateTag(name));
            }
            found = Some(s[1].clone());
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> ChannelKey {
        ChannelKey([0x11u8; 32])
    }
    fn chan() -> ChannelId {
        ChannelId([0xaau8; 32])
    }

    fn channel_tag() -> Tag {
        Tag::custom(TagKind::Custom(TAG_CHANNEL.into()), [chan().to_hex()])
    }
    fn epoch0_tag() -> Tag {
        Tag::custom(TagKind::Custom(TAG_EPOCH.into()), ["0".to_string()])
    }

    /// Seal an arbitrary inner event into a correctly-tagged outer (epoch 0).
    fn wrap_inner(inner: &Event) -> Event {
        let content = cipher::seal(key().as_bytes(), inner.as_json().as_bytes()).unwrap();
        EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), content)
            .tags([
                Tag::custom(
                    TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Z)),
                    [super::super::derive::channel_pseudonym(&key(), &chan(), Epoch(0)).to_hex()],
                ),
                Tag::custom(TagKind::Custom(TAG_VERSION.into()), [PROTOCOL_VERSION.to_string()]),
            ])
            .sign_with_keys(&Keys::generate())
            .unwrap()
    }

    #[test]
    fn round_trip() {
        let author = Keys::generate();
        let outer = seal_message(&author, &key(), &chan(), Epoch(0), "gm fren", 1_700_000_000_000)
            .expect("seal");
        let opened = open_message(&outer, &key(), &chan(), Epoch(0)).expect("open");
        assert_eq!(opened.content, "gm fren");
        assert_eq!(opened.author, author.public_key());
        assert_eq!(opened.ms, Some(1_700_000_000_000));
        assert_eq!(opened.channel_id, chan());
        assert_eq!(opened.epoch, Epoch(0));
    }

    #[tokio::test]
    async fn signer_path_matches_local_keys_path() {
        // Parity with DMs: the inner event can be signed through the async
        // `NostrSigner` (the same path a NIP-46 bunker uses) instead of local keys.
        // `Keys` implements `NostrSigner`, so signing the unsigned inner via `.sign()`
        // exercises that path; the sealed result must open identically.
        let author = Keys::generate();
        let ephemeral = Keys::generate();

        let unsigned = build_inner_event(author.public_key(), &chan(), Epoch(0), "via signer", 1_700_000_000_777, None);
        let inner: Event = unsigned.sign(&author).await.expect("remote-style sign");
        let outer = seal_with_signed_inner(&ephemeral, &inner, &key(), &chan(), Epoch(0)).expect("seal");

        let opened = open_message(&outer, &key(), &chan(), Epoch(0)).expect("open");
        assert_eq!(opened.content, "via signer");
        assert_eq!(opened.author, author.public_key(), "authorship is the identity key, not ephemeral");
        assert_eq!(opened.ms, Some(1_700_000_000_777));
        // Retained ephemeral is the outer signer (so it's still self-deletable).
        assert_eq!(outer.pubkey, ephemeral.public_key());
    }

    #[test]
    fn reply_reference_round_trips() {
        // A reply target (inner `e` tag) survives seal→open and surfaces on the parsed Message via the
        // shared parser (reply parsing now lives in `process_rumor`, exercised end-to-end via build_message).
        let author = Keys::generate();
        let target = "a".repeat(64);
        let inner = build_inner_event(author.public_key(), &chan(), Epoch(0), "re: hi", 5, Some(&target))
            .sign_with_keys(&author)
            .unwrap();
        let outer = seal_with_signed_inner(&Keys::generate(), &inner, &key(), &chan(), Epoch(0)).unwrap();
        let opened = open_message(&outer, &key(), &chan(), Epoch(0)).unwrap();
        let msg = crate::community::inbound::build_message(&opened, &Keys::generate().public_key());
        assert_eq!(msg.replied_to, target);

        // No reply target → replied_to is empty.
        let plain = seal_message(&author, &key(), &chan(), Epoch(0), "hi", 6).unwrap();
        let opened2 = open_message(&plain, &key(), &chan(), Epoch(0)).unwrap();
        assert!(crate::community::inbound::build_message(&opened2, &Keys::generate().public_key()).replied_to.is_empty());
    }

    #[test]
    fn custom_emoji_tags_round_trip() {
        // NIP-30 `["emoji", shortcode, url]` tags survive seal→open and surface on the parsed Message
        // (shared parser), so custom-emoji messages render the image — parity with DMs.
        let author = Keys::generate();
        let tags = vec![crate::types::EmojiTag {
            shortcode: "fire".into(),
            url: "https://blossom/fire.png".into(),
        }];
        let inner = build_inner_typed(
            author.public_key(), &chan(), Epoch(0),
            crate::stored_event::event_kind::COMMUNITY_MESSAGE, "gm :fire:", 1, None, &tags,
        )
        .sign_with_keys(&author)
        .unwrap();
        let outer = seal_with_signed_inner(&Keys::generate(), &inner, &key(), &chan(), Epoch(0)).unwrap();
        let opened = open_message(&outer, &key(), &chan(), Epoch(0)).unwrap();
        let msg = crate::community::inbound::build_message(&opened, &Keys::generate().public_key());
        assert_eq!(msg.emoji_tags.len(), 1);
        assert_eq!(msg.emoji_tags[0].shortcode, "fire");
        assert_eq!(msg.emoji_tags[0].url, "https://blossom/fire.png");
    }

    #[test]
    fn far_future_inner_timestamp_is_clamped() {
        // W3: the inner created_at isn't relay-clamped (only the outer is published), so a
        // hostile member could stamp it absurdly far in the future to pin the message to
        // the top forever. open_message clamps an implausible ms back to receipt time.
        let author = Keys::generate();
        let far_future = 99_999_999_999_999u64; // ~year 5138 in epoch-ms
        let outer = seal_message(&author, &key(), &chan(), Epoch(0), "from the future", far_future).unwrap();
        let opened = open_message(&outer, &key(), &chan(), Epoch(0)).unwrap();
        assert!(opened.ms.unwrap() < far_future, "far-future ordering ms must be clamped to ~now");
    }

    #[test]
    fn duplicate_reply_tag_on_a_message_is_tolerated() {
        // A message's reply pointer is cosmetic, so a duplicate `e` no longer drops the whole message —
        // the content is preserved (open succeeds). The security-critical disambiguation moved to the
        // SHARED parser, which rejects an ambiguous TARGET for reactions/edits/deletes
        // (see `rumor::unique_event_ref`); that's covered by the rumor-level tests.
        let author = Keys::generate();
        let inner = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), "x")
            .tags([
                Tag::custom(TagKind::Custom(TAG_CHANNEL.into()), [chan().to_hex()]),
                Tag::custom(TagKind::Custom(TAG_EPOCH.into()), ["0".to_string()]),
                Tag::custom(TagKind::Custom(TAG_MS.into()), ["1".to_string()]),
                Tag::custom(TagKind::e(), ["aa".repeat(32), String::new(), "reply".to_string()]),
                Tag::custom(TagKind::e(), ["bb".repeat(32), String::new(), "reply".to_string()]),
            ])
            .sign_with_keys(&author)
            .unwrap();
        let outer = seal_with_signed_inner(&Keys::generate(), &inner, &key(), &chan(), Epoch(0)).unwrap();
        assert!(open_message(&outer, &key(), &chan(), Epoch(0)).is_ok(),
            "a message must not be dropped over an ambiguous (cosmetic) reply pointer");
    }

    #[test]
    fn multi_attachment_message_round_trips_caption_and_imeta() {
        // The protocol's multi-attachment capability: ONE event carries a caption
        // (content) plus N attachments (one imeta each), optionally replying. Verify the
        // full seal → open path reconstructs the caption, the reply target, and every
        // attachment's crypto + metadata in order.
        use crate::types::{Attachment, ImageMetadata};
        let mk = |name: &str, ext: &str, img: bool| Attachment {
            id: "x".into(),
            key: "0".repeat(64),
            nonce: format!("{:0<24}", name),
            extension: ext.into(),
            name: name.into(),
            url: format!("https://blossom.example/{name}"),
            path: String::new(),
            size: 1234,
            img_meta: img.then(|| ImageMetadata { thumbhash: "TH".into(), width: 64, height: 48 }),
            downloading: false,
            downloaded: false,
            webxdc_topic: None,
            group_id: None,
            original_hash: Some("a".repeat(64)),
            scheme_version: None,
            mls_filename: None,
        };
        let imetas = vec![
            super::super::attachments::attachment_to_imeta(&mk("photo.png", "png", true)),
            super::super::attachments::attachment_to_imeta(&mk("notes.pdf", "pdf", false)),
        ];
        let author = Keys::generate();
        let reply_target = "bb".repeat(32);
        let inner = build_inner_full(
            author.public_key(), &chan(), Epoch(0),
            event_kind::COMMUNITY_MESSAGE, "look at these", 1_700_000_000_123,
            Some(&reply_target), &[], &imetas,
        ).sign_with_keys(&author).unwrap();
        let outer = seal_with_signed_inner(&Keys::generate(), &inner, &key(), &chan(), Epoch(0)).unwrap();

        let opened = open_message(&outer, &key(), &chan(), Epoch(0)).unwrap();
        assert_eq!(opened.content, "look at these");
        // Reply ref is parsed by the shared parser; the built Message carries it. Attachments are the
        // transport-specific imeta, still parsed at open.
        let msg = crate::community::inbound::build_message(&opened, &Keys::generate().public_key());
        assert_eq!(msg.replied_to, reply_target);
        assert_eq!(opened.attachments.len(), 2, "both attachments parse");
        assert_eq!(opened.attachments[0].name, "photo.png");
        assert_eq!(opened.attachments[0].key, "0".repeat(64));
        assert_eq!(opened.attachments[0].extension, "png");
        assert!(opened.attachments[0].img_meta.is_some(), "image carries thumbhash/dim");
        assert!(opened.attachments[0].group_id.is_none(), "Community attachment uses key/nonce, not MLS");
        assert_eq!(opened.attachments[1].name, "notes.pdf");
        assert_eq!(opened.attachments[1].extension, "pdf");
        assert!(opened.attachments[1].img_meta.is_none());
        // A caption-only message (no imeta) opens with zero attachments.
        let plain = seal_message(&author, &key(), &chan(), Epoch(0), "just text", 1).unwrap();
        assert!(open_message(&plain, &key(), &chan(), Epoch(0)).unwrap().attachments.is_empty());
    }

    #[test]
    fn seal_rejects_inner_bound_to_wrong_channel() {
        // seal_with_signed_inner must refuse an inner whose binding doesn't match the
        // coordinate being sealed under (can't produce an unopenable/spliced message).
        let author = Keys::generate();
        let other = ChannelId([0xbbu8; 32]);
        let inner = build_inner_event(author.public_key(), &other, Epoch(0), "x", 1, None)
            .sign_with_keys(&author)
            .unwrap();
        let err = seal_with_signed_inner(&Keys::generate(), &inner, &key(), &chan(), Epoch(0));
        assert!(matches!(err, Err(EnvelopeError::ChannelMismatch)), "got {err:?}");
    }

    #[test]
    fn ms_splits_to_created_at_and_offset_and_reconstructs() {
        // Full epoch-ms in → created_at(secs) + ms-offset(0..999) on the wire →
        // exact full ms back out (lossless, and matches Vector's DM convention).
        let author = Keys::generate();
        let outer = seal_message(&author, &key(), &chan(), Epoch(0), "ts", 1_234_567).unwrap();
        // On the wire: created_at is seconds, the ms tag is only the 0..999 offset.
        assert_eq!(outer_inner_created_at_secs(&outer), 1234);
        let opened = open_message(&outer, &key(), &chan(), Epoch(0)).unwrap();
        assert_eq!(opened.ms, Some(1_234_567), "full ms reconstructed from secs*1000 + offset");
        assert_eq!(opened.created_at.as_secs(), 1234);
    }

    /// Decrypt + read the inner event's created_at (test helper).
    fn outer_inner_created_at_secs(outer: &Event) -> u64 {
        let pt = cipher::open(key().as_bytes(), &outer.content).unwrap();
        let inner = Event::from_json(&String::from_utf8(pt).unwrap()).unwrap();
        inner.created_at.as_secs()
    }

    #[test]
    fn outer_signer_is_ephemeral_not_author() {
        // The wire event must NOT be signed by the author's real key (no linkage).
        let author = Keys::generate();
        let outer = seal_message(&author, &key(), &chan(), Epoch(0), "hi", 1).unwrap();
        assert_ne!(
            outer.pubkey,
            author.public_key(),
            "outer event must be ephemeral-signed, not author-signed"
        );
        // ...but the recovered author (inner) IS the real key.
        let opened = open_message(&outer, &key(), &chan(), Epoch(0)).unwrap();
        assert_eq!(opened.author, author.public_key());
    }

    #[test]
    fn identical_plaintext_yields_distinct_ciphertext() {
        let author = Keys::generate();
        let a = seal_message(&author, &key(), &chan(), Epoch(0), "same", 1).unwrap();
        let b = seal_message(&author, &key(), &chan(), Epoch(0), "same", 1).unwrap();
        assert_ne!(a.content, b.content, "per-message nonce must randomize ciphertext");
    }

    #[test]
    fn wrong_key_is_rejected() {
        let author = Keys::generate();
        let outer = seal_message(&author, &key(), &chan(), Epoch(0), "secret", 1).unwrap();
        let wrong = ChannelKey([0x22u8; 32]);
        let err = open_message(&outer, &wrong, &chan(), Epoch(0));
        assert!(matches!(err, Err(EnvelopeError::Decrypt(_))), "got {err:?}");
    }

    #[test]
    fn cross_channel_splice_is_rejected() {
        // Decrypt succeeds under a key we hold, but the inner channel tag names a
        // different channel than the key's channel → strict-equality check fires.
        let author = Keys::generate();
        // Seal for channel A.
        let outer = seal_message(&author, &key(), &chan(), Epoch(0), "for A", 1).unwrap();
        // Attempt to open as if it belonged to a DIFFERENT channel B, using the SAME
        // key (simulating a member who re-published it under B's coordinate).
        let chan_b = ChannelId([0xbbu8; 32]);
        let err = open_message(&outer, &key(), &chan_b, Epoch(0));
        assert!(matches!(err, Err(EnvelopeError::ChannelMismatch)), "got {err:?}");
    }

    #[test]
    fn cross_epoch_splice_is_rejected() {
        let author = Keys::generate();
        let outer = seal_message(&author, &key(), &chan(), Epoch(0), "epoch 0", 1).unwrap();
        let err = open_message(&outer, &key(), &chan(), Epoch(1));
        assert!(matches!(err, Err(EnvelopeError::EpochMismatch)), "got {err:?}");
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        // Flip a byte of the base64 payload → NIP-44 MAC must fail on decrypt.
        let author = Keys::generate();
        let mut outer =
            seal_message(&author, &key(), &chan(), Epoch(0), "integrity", 1).unwrap();
        // Rebuild an outer event with a corrupted content (events are immutable, so
        // re-sign a mutated copy with a fresh ephemeral key).
        let mut bytes = base64_simd::STANDARD.decode_to_vec(outer.content.as_bytes()).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        let corrupted = base64_simd::STANDARD.encode_to_string(&bytes);
        let ephemeral = Keys::generate();
        outer = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), corrupted)
            .tags([
                Tag::custom(
                    TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Z)),
                    [super::super::derive::channel_pseudonym(&key(), &chan(), Epoch(0)).to_hex()],
                ),
                Tag::custom(TagKind::Custom(TAG_VERSION.into()), [PROTOCOL_VERSION.to_string()]),
            ])
            .sign_with_keys(&ephemeral)
            .unwrap();
        let err = open_message(&outer, &key(), &chan(), Epoch(0));
        assert!(matches!(err, Err(EnvelopeError::Decrypt(_))), "got {err:?}");
    }

    #[test]
    fn missing_version_tag_is_rejected() {
        let author = Keys::generate();
        let inner = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), "x")
            .sign_with_keys(&author)
            .unwrap();
        let content = cipher::seal(key().as_bytes(), inner.as_json().as_bytes()).unwrap();
        let ephemeral = Keys::generate();
        let outer = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), content)
            .sign_with_keys(&ephemeral)
            .unwrap();
        assert!(matches!(
            open_message(&outer, &key(), &chan(), Epoch(0)),
            Err(EnvelopeError::BadVersion(None))
        ));
    }

    #[test]
    fn two_members_exchange() {
        // The e2e seed: Alice and Bob hold the same channel key; each can open the
        // other's sealed message and recover the correct real author.
        let alice = Keys::generate();
        let bob = Keys::generate();
        let shared = key();

        let from_alice = seal_message(&alice, &shared, &chan(), Epoch(0), "yo bob", 10).unwrap();
        let seen_by_bob = open_message(&from_alice, &shared, &chan(), Epoch(0)).unwrap();
        assert_eq!(seen_by_bob.author, alice.public_key());
        assert_eq!(seen_by_bob.content, "yo bob");

        let from_bob = seal_message(&bob, &shared, &chan(), Epoch(0), "hey alice", 11).unwrap();
        let seen_by_alice = open_message(&from_bob, &shared, &chan(), Epoch(0)).unwrap();
        assert_eq!(seen_by_alice.author, bob.public_key());
        assert_eq!(seen_by_alice.content, "hey alice");

        // message_ids differ (distinct inner events).
        assert_ne!(seen_by_bob.message_id, seen_by_alice.message_id);
    }

    #[test]
    fn unknown_version_is_rejected_before_decrypt() {
        // Hand-build an outer event with a bogus version tag; must reject on version,
        // never reaching decryption.
        let author = Keys::generate();
        let inner = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), "x")
            .sign_with_keys(&author)
            .unwrap();
        let content = cipher::seal(key().as_bytes(), inner.as_json().as_bytes()).unwrap();
        let ephemeral = Keys::generate();
        let outer = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), content)
            .tags([Tag::custom(TagKind::Custom(TAG_VERSION.into()), ["999".to_string()])])
            .sign_with_keys(&ephemeral)
            .unwrap();
        let err = open_message(&outer, &key(), &chan(), Epoch(0));
        assert!(matches!(err, Err(EnvelopeError::BadVersion(Some(ref v))) if v == "999"), "got {err:?}");
    }

    #[test]
    fn inner_kind_mismatch_is_rejected() {
        // A signed REACTION inner re-wrapped inside a MESSAGE outer → type splice.
        let author = Keys::generate();
        let inner = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_REACTION), "+")
            .tags([channel_tag(), epoch0_tag()])
            .sign_with_keys(&author)
            .unwrap();
        let outer = wrap_inner(&inner);
        let err = open_message(&outer, &key(), &chan(), Epoch(0));
        assert!(
            matches!(err, Err(EnvelopeError::KindMismatch { outer: 3300, inner: 3301 })),
            "got {err:?}"
        );
    }

    #[test]
    fn forged_inner_signature_is_rejected() {
        // Tamper the inner content after signing: id/sig no longer verify.
        let author = Keys::generate();
        let inner = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), "real")
            .tags([channel_tag(), epoch0_tag()])
            .sign_with_keys(&author)
            .unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&inner.as_json()).unwrap();
        v["content"] = serde_json::Value::String("forged".into());
        let tampered = serde_json::to_string(&v).unwrap();
        let content = cipher::seal(key().as_bytes(), tampered.as_bytes()).unwrap();
        let outer = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), content)
            .tags([
                Tag::custom(
                    TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Z)),
                    [super::super::derive::channel_pseudonym(&key(), &chan(), Epoch(0)).to_hex()],
                ),
                Tag::custom(TagKind::Custom(TAG_VERSION.into()), [PROTOCOL_VERSION.to_string()]),
            ])
            .sign_with_keys(&Keys::generate())
            .unwrap();
        let err = open_message(&outer, &key(), &chan(), Epoch(0));
        assert!(matches!(err, Err(EnvelopeError::BadSignature)), "got {err:?}");
    }

    #[test]
    fn missing_channel_tag_is_rejected() {
        // Inner has a valid sig + matching kind + epoch, but no channel tag.
        let author = Keys::generate();
        let inner = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), "hi")
            .tags([epoch0_tag()])
            .sign_with_keys(&author)
            .unwrap();
        let outer = wrap_inner(&inner);
        let err = open_message(&outer, &key(), &chan(), Epoch(0));
        assert!(matches!(err, Err(EnvelopeError::MissingTag(t)) if t == TAG_CHANNEL), "got {err:?}");
    }

    #[test]
    fn duplicate_channel_tag_is_rejected() {
        // Two channel tags → ambiguous inner; must reject (don't trust first-match).
        let author = Keys::generate();
        let inner = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), "hi")
            .tags([channel_tag(), channel_tag(), epoch0_tag()])
            .sign_with_keys(&author)
            .unwrap();
        let outer = wrap_inner(&inner);
        let err = open_message(&outer, &key(), &chan(), Epoch(0));
        assert!(matches!(err, Err(EnvelopeError::DuplicateTag(t)) if t == TAG_CHANNEL), "got {err:?}");
    }

    #[test]
    fn version_is_checked_before_decryption() {
        // Bogus version AND wrong key: must fail on version, NOT decrypt. This proves
        // the ordering — a regression moving the version check after decrypt would
        // instead surface a Decrypt error here (wrong key), so this catches it where
        // the correct-key version test cannot.
        let author = Keys::generate();
        let inner = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), "x")
            .tags([channel_tag(), epoch0_tag()])
            .sign_with_keys(&author)
            .unwrap();
        let content = cipher::seal(key().as_bytes(), inner.as_json().as_bytes()).unwrap();
        let outer = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), content)
            .tags([Tag::custom(TagKind::Custom(TAG_VERSION.into()), ["7".to_string()])])
            .sign_with_keys(&Keys::generate())
            .unwrap();
        let wrong_key = ChannelKey([0x99u8; 32]);
        let err = open_message(&outer, &wrong_key, &chan(), Epoch(0));
        assert!(matches!(err, Err(EnvelopeError::BadVersion(Some(ref v))) if v == "7"), "got {err:?}");
    }

    #[test]
    fn truncated_ciphertext_is_rejected() {
        // Distinct from the bit-flip test: chop bytes off the payload → length/MAC fail.
        let author = Keys::generate();
        let outer = seal_message(&author, &key(), &chan(), Epoch(0), "intact", 1).unwrap();
        let mut bytes = base64_simd::STANDARD.decode_to_vec(outer.content.as_bytes()).unwrap();
        bytes.truncate(bytes.len().saturating_sub(5));
        let truncated = base64_simd::STANDARD.encode_to_string(&bytes);
        let mangled = EventBuilder::new(Kind::Custom(event_kind::COMMUNITY_MESSAGE), truncated)
            .tags([
                Tag::custom(
                    TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Z)),
                    [super::super::derive::channel_pseudonym(&key(), &chan(), Epoch(0)).to_hex()],
                ),
                Tag::custom(TagKind::Custom(TAG_VERSION.into()), [PROTOCOL_VERSION.to_string()]),
            ])
            .sign_with_keys(&Keys::generate())
            .unwrap();
        assert!(matches!(open_message(&mangled, &key(), &chan(), Epoch(0)), Err(EnvelopeError::Decrypt(_))));
    }

    #[test]
    fn seal_uses_matching_inner_and_outer_kind() {
        // The binding triad requires inner.kind == outer.kind; seal must produce that.
        let author = Keys::generate();
        let outer = seal_message(&author, &key(), &chan(), Epoch(0), "x", 1).unwrap();
        assert_eq!(outer.kind.as_u16(), event_kind::COMMUNITY_MESSAGE);
        // Decrypt the inner and confirm its kind mirrors the outer.
        let plaintext = cipher::open(key().as_bytes(), &outer.content).unwrap();
        let inner = Event::from_json(&String::from_utf8(plaintext).unwrap()).unwrap();
        assert_eq!(inner.kind.as_u16(), outer.kind.as_u16());
    }

    #[test]
    fn seal_tags_outer_with_correct_pseudonym_and_version() {
        // the relay-filterable `z` tag must carry the correct epoch pseudonym,
        // and `v` must be "1" — a regression here silently breaks relay querying.
        let author = Keys::generate();
        let outer = seal_message(&author, &key(), &chan(), Epoch(0), "x", 1).unwrap();
        let expected = super::super::derive::channel_pseudonym(&key(), &chan(), Epoch(0)).to_hex();
        assert_eq!(find_tag(&outer, "z").as_deref(), Some(expected.as_str()));
        assert_eq!(find_tag(&outer, TAG_VERSION).as_deref(), Some("1"));
    }
}
