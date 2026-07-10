//! CORD-03 Chat Plane — a Channel's messages over the v2 stream envelope.
//!
//! Every chat action is an unsigned rumor in an **encrypted seal** (CORD-02 §5
//! makes 20013 mandatory here — chat content must never be liftable as a
//! standalone public event) inside a wrap at the Channel's stream address.
//! The rumor kinds reuse standard Nostr shapes wherever one fits:
//!   - kind 9 message (NIP-C7: content = text, replies via a `q` tag naming
//!     the parent RUMOR id — never the outer wrap's, which differs per re-wrap)
//!   - kind 7 reaction (NIP-25: `e`/`p`/`k` name the target)
//!   - kind 5 delete (NIP-09: `e` = the author's own rumor id, `k` its kind)
//!   - kind 3302 edit (`e` = own message rumor id, content = replacement text)
//!   - kind 3310 WebXDC peer signal (payload opaque to the protocol)
//!   - kind 23311 typing (ephemeral tier — rides the 21059 wrap)
//!
//! Two Vector inner-tag conventions carry over from v1: NIP-30
//! `["emoji", shortcode, url]` tags and verbatim extra tags (NIP-92 `imeta`
//! attachments) ride inside the signed rumor, so they are author-committed.
//!
//! Every rumor MUST commit `["channel", id]` + `["epoch", n]`, checked
//! strict-equal against the coordinate whose key decrypted the wrap (CORD-03
//! §3) — the rumor's own claim is never trusted, so a keyholder of two planes
//! cannot re-seal a rumor across Channels or replay it across epochs.
//!
//! The wrap kind (1059/21059) is a transport tier, not a content authority:
//! the open side admits any allowlisted rumor kind on either wrap and lets the
//! rumor kind govern meaning. Publishers still MUST put typing on 21059
//! (relays MUST NOT store it) — that is a send-side duty, not a read gate.

use nostr_sdk::prelude::{
    Alphabet, Event, Keys, PublicKey, SingleLetterTag, Tag, TagKind, Timestamp, UnsignedEvent,
};

use super::super::{ChannelId, Epoch};
use super::derive::{channel_group_key, GroupKey};
use super::kind;
use super::stream::{self, OpenedStream, SealForm, StreamError};

const TAG_QUOTE: &str = "q";
const TAG_TARGET: &str = "e";
const TAG_TARGET_AUTHOR: &str = "p";
const TAG_TARGET_KIND: &str = "k";
const TAG_EMOJI: &str = "emoji";

/// Errors from the chat plane layer (envelope errors ride inside).
#[derive(Debug)]
pub enum ChatError {
    Stream(StreamError),
    /// A chat rumor arrived in a plaintext seal — CORD-02 §5 requires the
    /// encrypted form on this plane, a strict reader drops the violation.
    NotEncryptedSealed,
    /// The rumor kind isn't in the chat-plane registry (retired numbers stay
    /// burned — a 3300 is v1 traffic, never a v2 message).
    UnknownKind(u16),
    MissingTag(&'static str),
    /// A target-bearing tag appears more than once — ambiguous, rejected
    /// (same discipline as the stream module's binding tags).
    DuplicateTag(&'static str),
    /// A tag value failed its shape check (64-hex id, pubkey, or integer kind).
    BadTag(&'static str),
    /// The wrap's author matches no held `(epoch, key)` — not ours to open.
    NoHeldEpoch,
}

impl std::fmt::Display for ChatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatError::Stream(e) => write!(f, "stream: {e}"),
            ChatError::NotEncryptedSealed => write!(f, "chat rumor must ride an encrypted seal"),
            ChatError::UnknownKind(k) => write!(f, "rumor kind {k} is not a chat-plane kind"),
            ChatError::MissingTag(t) => write!(f, "missing chat tag: {t}"),
            ChatError::DuplicateTag(t) => write!(f, "duplicate chat tag: {t}"),
            ChatError::BadTag(t) => write!(f, "malformed chat tag: {t}"),
            ChatError::NoHeldEpoch => write!(f, "wrap author matches no held epoch key"),
        }
    }
}

impl std::error::Error for ChatError {}

impl From<StreamError> for ChatError {
    fn from(e: StreamError) -> Self {
        ChatError::Stream(e)
    }
}

// ── Keying (CORD-03 §1) ──────────────────────────────────────────────────────

/// A Channel's Chat Plane group key. `secret` is whatever feeds the Channel at
/// this epoch: the `community_root` for a Public Channel (at the root epoch),
/// or the Channel's independent key for a Private one (at its own channel
/// epoch). The channel id inside the derivation gives every Channel a distinct
/// address regardless of which secret feeds it.
pub fn chat_group_key(secret: &[u8; 32], channel_id: &ChannelId, epoch: Epoch) -> GroupKey {
    channel_group_key(secret, channel_id, epoch)
}

// ── Rumor builders ───────────────────────────────────────────────────────────
//
// All builders take the author's pubkey (not keys) so bunker accounts build
// identical rumors — signing happens at the seal, not here. `at_ms` is the
// full epoch-ms send time (CORD-02 §4); the binding tags are always attached.

/// Build a kind-9 message rumor. `reply_to` is the parent's
/// `(rumor_id_hex, author_hex)` — the NIP-C7 `q` tag; `emoji` the NIP-30
/// `(shortcode, url)` pairs for any `:shortcode:` in the content; `extra_tags`
/// ride verbatim (NIP-92 `imeta` attachments), author-committed by the seal.
#[allow(clippy::too_many_arguments)]
pub fn build_message_rumor(
    author: PublicKey,
    channel_id: &ChannelId,
    epoch: Epoch,
    content: &str,
    reply_to: Option<(&str, &str)>,
    emoji: &[(&str, &str)],
    extra_tags: Vec<Tag>,
    at_ms: u64,
) -> UnsignedEvent {
    let mut tags = stream::channel_binding_tags(channel_id, epoch);
    if let Some((parent_id, parent_author)) = reply_to {
        tags.push(Tag::custom(
            TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::Q)),
            [parent_id.to_string(), String::new(), parent_author.to_string()],
        ));
    }
    for (shortcode, url) in emoji {
        tags.push(emoji_tag(shortcode, url));
    }
    tags.extend(extra_tags);
    stream::build_rumor_ms(kind::MESSAGE, author, content, tags, at_ms)
}

/// Build a kind-1111 threaded-reply rumor (NIP-22, CORD-03 §3). Uppercase
/// `K`/`E`/`P` pin the immutable thread ROOT, lowercase `k`/`e`/`p` the
/// immediate PARENT — all rumor ids. `parent_root` names the parent's own root
/// when the parent is itself a reply (inherited verbatim, so the root stays
/// stable at any depth — the exact shape Armada builds); `None` means the
/// parent IS the root.
#[allow(clippy::too_many_arguments)]
pub fn build_comment_rumor(
    author: PublicKey,
    channel_id: &ChannelId,
    epoch: Epoch,
    content: &str,
    parent_id_hex: &str,
    parent_kind: u16,
    parent_author_hex: &str,
    parent_root: Option<(&str, u16, &str)>,
    emoji: &[(&str, &str)],
    at_ms: u64,
) -> UnsignedEvent {
    let mut tags = stream::channel_binding_tags(channel_id, epoch);
    let (root_id, root_kind, root_author) = parent_root.unwrap_or((parent_id_hex, parent_kind, parent_author_hex));
    tags.push(Tag::custom(TagKind::SingleLetter(SingleLetterTag::uppercase(Alphabet::K)), [root_kind.to_string()]));
    tags.push(Tag::custom(
        TagKind::SingleLetter(SingleLetterTag::uppercase(Alphabet::E)),
        [root_id.to_string(), String::new(), root_author.to_string()],
    ));
    tags.push(Tag::custom(TagKind::SingleLetter(SingleLetterTag::uppercase(Alphabet::P)), [root_author.to_string()]));
    tags.push(Tag::custom(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::K)), [parent_kind.to_string()]));
    tags.push(Tag::custom(
        TagKind::e(),
        [parent_id_hex.to_string(), String::new(), parent_author_hex.to_string()],
    ));
    tags.push(Tag::custom(TagKind::p(), [parent_author_hex.to_string()]));
    for (shortcode, url) in emoji {
        tags.push(emoji_tag(shortcode, url));
    }
    stream::build_rumor_ms(kind::COMMENT, author, content, tags, at_ms)
}

/// Build a kind-7 reaction rumor (NIP-25): `e` = the target rumor id, `p` its
/// author, `k` = the target's kind (`9` for a message, `1111` for a threaded
/// reply). `emoji_content` is the reaction itself (`"+"`, an emoji, or a
/// `:shortcode:`); `emoji` carries the NIP-30 pair when the content is a
/// custom-emoji shortcode.
#[allow(clippy::too_many_arguments)]
pub fn build_reaction_rumor(
    author: PublicKey,
    channel_id: &ChannelId,
    epoch: Epoch,
    target_rumor_id_hex: &str,
    target_author_hex: &str,
    target_kind: u16,
    emoji_content: &str,
    emoji: Option<(&str, &str)>,
    at_ms: u64,
) -> UnsignedEvent {
    let mut tags = stream::channel_binding_tags(channel_id, epoch);
    tags.push(Tag::custom(TagKind::e(), [target_rumor_id_hex.to_string()]));
    tags.push(Tag::custom(TagKind::p(), [target_author_hex.to_string()]));
    tags.push(Tag::custom(
        TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::K)),
        [target_kind.to_string()],
    ));
    if let Some((shortcode, url)) = emoji {
        tags.push(emoji_tag(shortcode, url));
    }
    stream::build_rumor_ms(kind::REACTION, author, emoji_content, tags, at_ms)
}

/// Build a kind-5 delete rumor (NIP-09): `e` = the author's OWN rumor id,
/// `k` = its kind. Semantic within the plane only — members stop rendering;
/// the wrap ciphertext on relays needs a separate NIP-09 scrub by its `p` tag.
pub fn build_delete_rumor(
    author: PublicKey,
    channel_id: &ChannelId,
    epoch: Epoch,
    target_rumor_id_hex: &str,
    target_kind: u16,
    at_ms: u64,
) -> UnsignedEvent {
    let mut tags = stream::channel_binding_tags(channel_id, epoch);
    tags.push(Tag::custom(TagKind::e(), [target_rumor_id_hex.to_string()]));
    tags.push(Tag::custom(
        TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::K)),
        [target_kind.to_string()],
    ));
    stream::build_rumor_ms(kind::DELETE, author, "", tags, at_ms)
}

/// Build a kind-3302 edit rumor: `e` = the author's own message rumor id,
/// content = the replacement text (fields unpinned upstream; this shape
/// matches the CORD examples).
pub fn build_edit_rumor(
    author: PublicKey,
    channel_id: &ChannelId,
    epoch: Epoch,
    target_rumor_id_hex: &str,
    new_content: &str,
    at_ms: u64,
) -> UnsignedEvent {
    let mut tags = stream::channel_binding_tags(channel_id, epoch);
    tags.push(Tag::custom(TagKind::e(), [target_rumor_id_hex.to_string()]));
    stream::build_rumor_ms(kind::EDIT, author, new_content, tags, at_ms)
}

/// Build a kind-3310 WebXDC peer-signal rumor: `content` and `extra_tags` are
/// the app payload, opaque to the protocol, carried verbatim.
pub fn build_webxdc_rumor(
    author: PublicKey,
    channel_id: &ChannelId,
    epoch: Epoch,
    content: &str,
    extra_tags: Vec<Tag>,
    at_ms: u64,
) -> UnsignedEvent {
    let mut tags = stream::channel_binding_tags(channel_id, epoch);
    tags.extend(extra_tags);
    stream::build_rumor_ms(kind::WEBXDC, author, content, tags, at_ms)
}

/// Build a kind-23311 typing rumor — presence of the event is the signal, it
/// carries nothing. Seal it with `ephemeral: true` so relays never store it.
pub fn build_typing_rumor(author: PublicKey, channel_id: &ChannelId, epoch: Epoch, at_ms: u64) -> UnsignedEvent {
    let tags = stream::channel_binding_tags(channel_id, epoch);
    stream::build_rumor_ms(kind::TYPING, author, "", tags, at_ms)
}

// ── Seal / open over the stream ──────────────────────────────────────────────

/// Seal a chat rumor into its wrap: encrypted seal (mandatory on this plane),
/// then the durable 1059 wrap — or the ephemeral 21059 when `ephemeral` (the
/// typing tier). Refuses non-chat rumor kinds so a control edition can never
/// be published onto a Channel's stream by mistake. Returns the wrap plus the
/// ephemeral `p` keypair (retain it to best-effort NIP-09-scrub the wrap
/// later). Local-keys convenience; bunker accounts use [`stream::seal_content`]
/// + their remote signer + [`stream::wrap_seal`] for identical wire output.
pub fn seal_chat_rumor(
    rumor: &UnsignedEvent,
    group: &GroupKey,
    author_keys: &Keys,
    wrap_at: Timestamp,
    ephemeral: bool,
) -> Result<(Event, Keys), ChatError> {
    let k = rumor.kind.as_u16();
    if !is_chat_kind(k) {
        return Err(ChatError::UnknownKind(k));
    }
    let seal = stream::build_seal(rumor, SealForm::Encrypted, group, author_keys)?;
    let wrap_kind = if ephemeral { stream::KIND_WRAP_EPHEMERAL } else { stream::KIND_WRAP };
    Ok(stream::wrap_seal(&seal, group, wrap_kind, wrap_at)?)
}

/// A parsed reply reference — the NIP-C7 `q` tag's parent rumor id and (when
/// carried, a SHOULD upstream) its author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyRef {
    pub id: [u8; 32],
    pub author: Option<PublicKey>,
}

/// A fully verified, typed chat event. Every variant keeps its
/// [`OpenedStream`] — the proven author, rumor id, and ms time live there.
#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// Kind 9 (message; `reply_to` = its `q` inline quote) or kind 1111
    /// (threaded reply; `reply_to` = its immediate parent, the lowercase `e`).
    /// Text lives in `opened.rumor.content`; `emoji` is the NIP-30
    /// `(shortcode, url)` pairs (attachments ride the rumor's `imeta` tags).
    /// `opened.rumor.kind` distinguishes the two when a caller needs to.
    Message {
        opened: OpenedStream,
        reply_to: Option<ReplyRef>,
        emoji: Vec<(String, String)>,
    },
    /// Kind 7 — `emoji` is the reaction content; `emoji_url` the NIP-30 image
    /// when the content is a custom `:shortcode:`.
    Reaction {
        opened: OpenedStream,
        target: [u8; 32],
        target_author: PublicKey,
        emoji: String,
        emoji_url: Option<String>,
    },
    /// Kind 5 — authorization (self, or a moderator) is the fold's job, not
    /// the envelope's.
    Delete {
        opened: OpenedStream,
        target: [u8; 32],
        target_kind: Option<u16>,
    },
    /// Kind 3302 — author-only validity is likewise judged at fold time.
    Edit {
        opened: OpenedStream,
        target: [u8; 32],
        new_content: String,
    },
    /// Kind 3310 — payload opaque, read `opened.rumor` directly.
    Webxdc { opened: OpenedStream },
    /// Kind 23311 — the event's presence is the whole signal.
    Typing { opened: OpenedStream },
}

impl ChatEvent {
    /// The verified envelope facts (author, ms, rumor id) behind any variant.
    pub fn opened(&self) -> &OpenedStream {
        match self {
            ChatEvent::Message { opened, .. }
            | ChatEvent::Reaction { opened, .. }
            | ChatEvent::Delete { opened, .. }
            | ChatEvent::Edit { opened, .. }
            | ChatEvent::Webxdc { opened }
            | ChatEvent::Typing { opened } => opened,
        }
    }
}

/// Open and fully verify one chat wrap against the `(channel, epoch)` whose
/// key is being tried: envelope verification ([`stream::open_wrap`]), the
/// encrypted-seal gate, the strict channel/epoch binding, the kind allowlist,
/// then the typed parse. Malformed targets are errors, never panics.
pub fn open_chat_event(
    wrap: &Event,
    group: &GroupKey,
    channel_id: &ChannelId,
    epoch: Epoch,
) -> Result<ChatEvent, ChatError> {
    let opened = stream::open_wrap(wrap, group)?;
    if opened.seal_form != SealForm::Encrypted {
        return Err(ChatError::NotEncryptedSealed);
    }
    stream::check_channel_binding(&opened.rumor, channel_id, epoch)?;
    parse_chat_rumor(opened)
}

/// Open a chat wrap against every `(epoch, secret)` a client holds for one
/// Channel — the CORD-03 §3 read path, where history spanning a rekey is
/// queried across all held epoch pubkeys. Selection is by the wrap's author
/// (each epoch's derived address), NEVER trial decryption, and the binding is
/// then enforced against the epoch that actually matched — so an epoch-N rumor
/// re-sealed under epoch M's key still dies as a splice.
///
/// `secret` per entry is whatever feeds the Channel at that epoch (CORD-03 §1:
/// `community_root` for Public epochs, the channel key for Private ones).
/// Derivation costs an HKDF + keypair per entry per call — batch readers
/// should derive their [`GroupKey`]s once and match `wrap.pubkey` themselves.
pub fn open_chat_event_multi(
    wrap: &Event,
    held: &[(Epoch, [u8; 32])],
    channel_id: &ChannelId,
) -> Result<(ChatEvent, Epoch), ChatError> {
    for (epoch, secret) in held {
        let group = channel_group_key(secret, channel_id, *epoch);
        if wrap.pubkey == group.pk() {
            return open_chat_event(wrap, &group, channel_id, *epoch).map(|ev| (ev, *epoch));
        }
    }
    Err(ChatError::NoHeldEpoch)
}

// ── Parse (rumor → typed event) ──────────────────────────────────────────────

fn is_chat_kind(k: u16) -> bool {
    matches!(
        k,
        kind::MESSAGE | kind::COMMENT | kind::REACTION | kind::DELETE | kind::EDIT | kind::WEBXDC | kind::TYPING
    )
}

/// Type an ALREADY-VERIFIED rumor (one produced by [`stream::open_wrap`] and
/// binding-checked). Target ids come from UNIQUE tags — a duplicated `e`/`p`/
/// `q` is ambiguous (which target did the author sign off on?) and rejected.
fn parse_chat_rumor(opened: OpenedStream) -> Result<ChatEvent, ChatError> {
    match opened.rumor.kind.as_u16() {
        kind::MESSAGE => {
            let reply_to = match unique_tag(&opened.rumor, TAG_QUOTE)? {
                None => None,
                Some(s) => {
                    let id = decode_id32(value_of(s, TAG_QUOTE)?, TAG_QUOTE)?;
                    // NIP-C7 q tag: [q, id, relay-hint, pubkey] — the author
                    // slot is a SHOULD; absent or empty parses as unknown.
                    let author = match s.get(3).map(String::as_str).filter(|a| !a.is_empty()) {
                        Some(hex) => Some(PublicKey::from_hex(hex).map_err(|_| ChatError::BadTag(TAG_QUOTE))?),
                        None => None,
                    };
                    Some(ReplyRef { id, author })
                }
            };
            let emoji = collect_emoji(&opened.rumor);
            Ok(ChatEvent::Message { opened, reply_to, emoji })
        }
        kind::COMMENT => {
            // A threaded reply (NIP-22, CORD-03 §3). Vector's timeline renders it
            // INLINE: the immediate parent (the lowercase `e`) becomes the reply
            // context, exactly like a kind-9 quote — never dropped. The uppercase
            // root tags stay on the rumor for a future thread view.
            let reply_to = match unique_tag(&opened.rumor, TAG_TARGET)? {
                None => None, // a parentless comment still renders as plain text.
                Some(s) => {
                    let id = decode_id32(value_of(s, TAG_TARGET)?, TAG_TARGET)?;
                    // NIP-22 e tag: [e, id, relay-hint, pubkey] — author optional.
                    let author = match s.get(3).map(String::as_str).filter(|a| !a.is_empty()) {
                        Some(hex) => Some(PublicKey::from_hex(hex).map_err(|_| ChatError::BadTag(TAG_TARGET))?),
                        None => None,
                    };
                    Some(ReplyRef { id, author })
                }
            };
            let emoji = collect_emoji(&opened.rumor);
            Ok(ChatEvent::Message { opened, reply_to, emoji })
        }
        kind::REACTION => {
            let target = decode_id32(required_tag(&opened.rumor, TAG_TARGET)?, TAG_TARGET)?;
            let target_author = PublicKey::from_hex(required_tag(&opened.rumor, TAG_TARGET_AUTHOR)?)
                .map_err(|_| ChatError::BadTag(TAG_TARGET_AUTHOR))?;
            let emoji_url = collect_emoji(&opened.rumor).into_iter().next().map(|(_, url)| url);
            let emoji = opened.rumor.content.clone();
            Ok(ChatEvent::Reaction { opened, target, target_author, emoji, emoji_url })
        }
        kind::DELETE => {
            let target = decode_id32(required_tag(&opened.rumor, TAG_TARGET)?, TAG_TARGET)?;
            let target_kind = match unique_tag(&opened.rumor, TAG_TARGET_KIND)? {
                None => None,
                Some(s) => Some(
                    value_of(s, TAG_TARGET_KIND)?
                        .parse::<u16>()
                        .map_err(|_| ChatError::BadTag(TAG_TARGET_KIND))?,
                ),
            };
            Ok(ChatEvent::Delete { opened, target, target_kind })
        }
        kind::EDIT => {
            let target = decode_id32(required_tag(&opened.rumor, TAG_TARGET)?, TAG_TARGET)?;
            let new_content = opened.rumor.content.clone();
            Ok(ChatEvent::Edit { opened, target, new_content })
        }
        kind::WEBXDC => Ok(ChatEvent::Webxdc { opened }),
        kind::TYPING => Ok(ChatEvent::Typing { opened }),
        k => Err(ChatError::UnknownKind(k)),
    }
}

// ── Tag helpers ──────────────────────────────────────────────────────────────

fn emoji_tag(shortcode: &str, url: &str) -> Tag {
    Tag::custom(TagKind::Custom(TAG_EMOJI.into()), [shortcode.to_string(), url.to_string()])
}

/// The unique tag named `name`, or None. More than one match = ambiguous,
/// rejected (any keyholder can craft a rumor; first-match is nondeterministic).
fn unique_tag<'a>(rumor: &'a UnsignedEvent, name: &'static str) -> Result<Option<&'a [String]>, ChatError> {
    let mut found: Option<&[String]> = None;
    for t in rumor.tags.iter() {
        let s = t.as_slice();
        if s.first().map(|n| n == name).unwrap_or(false) {
            if found.is_some() {
                return Err(ChatError::DuplicateTag(name));
            }
            found = Some(s);
        }
    }
    Ok(found)
}

/// The unique tag's value, requiring the tag to be present.
fn required_tag<'a>(rumor: &'a UnsignedEvent, name: &'static str) -> Result<&'a str, ChatError> {
    let s = unique_tag(rumor, name)?.ok_or(ChatError::MissingTag(name))?;
    value_of(s, name)
}

fn value_of<'a>(slice: &'a [String], name: &'static str) -> Result<&'a str, ChatError> {
    slice.get(1).map(String::as_str).ok_or(ChatError::BadTag(name))
}

fn decode_id32(hex: &str, field: &'static str) -> Result<[u8; 32], ChatError> {
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ChatError::BadTag(field));
    }
    Ok(crate::simd::hex::hex_to_bytes_32(hex))
}

/// All well-formed NIP-30 `(shortcode, url)` pairs; malformed emoji tags are
/// skipped, not fatal (worst case the raw `:shortcode:` text renders).
fn collect_emoji(rumor: &UnsignedEvent) -> Vec<(String, String)> {
    rumor
        .tags
        .iter()
        .filter_map(|t| {
            let s = t.as_slice();
            (s.len() >= 3 && s[0] == TAG_EMOJI).then(|| (s[1].clone(), s[2].clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: u64 = 1_686_840_217_417;
    const WRAP_AT: Timestamp = Timestamp::from_secs(1_700_000_000);

    fn chan() -> ChannelId {
        ChannelId([0xab; 32])
    }

    fn secret() -> [u8; 32] {
        [7u8; 32]
    }

    fn group() -> GroupKey {
        chat_group_key(&secret(), &chan(), Epoch(0))
    }

    fn open(wrap: &Event) -> Result<ChatEvent, ChatError> {
        open_chat_event(wrap, &group(), &chan(), Epoch(0))
    }

    fn seal(rumor: &UnsignedEvent, author: &Keys) -> Event {
        seal_chat_rumor(rumor, &group(), author, WRAP_AT, false).unwrap().0
    }

    #[test]
    fn message_round_trip_carries_reply_emoji_and_extra_tags_verbatim() {
        let author = Keys::generate();
        let parent = Keys::generate();
        let parent_id = "aa".repeat(32);
        let imeta = Tag::custom(
            TagKind::Custom("imeta".into()),
            ["url https://x/f.png".to_string(), "m image/png".to_string()],
        );
        let rumor = build_message_rumor(
            author.public_key(),
            &chan(),
            Epoch(0),
            "welcome :catJAM:",
            Some((&parent_id, &parent.public_key().to_hex())),
            &[("catJAM", "https://x/cat.gif")],
            vec![imeta.clone()],
            AT,
        );
        let wrap = seal(&rumor, &author);

        let ChatEvent::Message { opened, reply_to, emoji } = open(&wrap).unwrap() else {
            panic!("expected a Message");
        };
        assert_eq!(opened.author, author.public_key());
        assert_eq!(opened.rumor.content, "welcome :catJAM:");
        assert_eq!(opened.at_ms, AT);
        let reply = reply_to.expect("reply parses back");
        assert_eq!(reply.id, [0xaa; 32]);
        assert_eq!(reply.author, Some(parent.public_key()));
        assert_eq!(emoji, vec![("catJAM".to_string(), "https://x/cat.gif".to_string())]);
        // The imeta tag rides the signed rumor byte-verbatim.
        assert!(opened.rumor.tags.iter().any(|t| t.as_slice() == imeta.as_slice()));
    }

    #[test]
    fn message_without_q_has_no_reply() {
        let author = Keys::generate();
        let rumor = build_message_rumor(author.public_key(), &chan(), Epoch(0), "hi", None, &[], vec![], AT);
        let ChatEvent::Message { reply_to, emoji, .. } = open(&seal(&rumor, &author)).unwrap() else {
            panic!("expected a Message");
        };
        assert_eq!(reply_to, None);
        assert!(emoji.is_empty());
    }

    #[test]
    fn a_threaded_reply_round_trips_as_a_message_with_its_parent_as_reply_context() {
        // CORD-03 §3 kind-1111: Vector renders a thread reply inline — the
        // immediate parent (lowercase e) is the reply context, never dropped.
        let author = Keys::generate();
        let root_author = Keys::generate();
        let root_id = "cd".repeat(32);
        let rumor = build_comment_rumor(
            author.public_key(),
            &chan(),
            Epoch(0),
            "replying in the thread!",
            &root_id,
            kind::MESSAGE,
            &root_author.public_key().to_hex(),
            None, // the parent IS the root
            &[],
            AT,
        );
        // The wire shape matches the spec example: K/E/P root + k/e/p parent.
        assert!(rumor.tags.iter().any(|t| t.as_slice() == ["K", "9"]));
        assert!(rumor.tags.iter().any(|t| t.as_slice()[0] == "E" && t.as_slice()[1] == root_id));
        assert!(rumor.tags.iter().any(|t| t.as_slice() == ["k", "9"]));

        let ChatEvent::Message { opened, reply_to, .. } = open(&seal(&rumor, &author)).unwrap() else {
            panic!("a threaded reply parses as a Message");
        };
        assert_eq!(opened.rumor.kind.as_u16(), kind::COMMENT, "the wire kind is preserved on the rumor");
        let reply = reply_to.expect("the immediate parent is the reply context");
        assert_eq!(reply.id, [0xcd; 32]);
        assert_eq!(reply.author, Some(root_author.public_key()));
    }

    #[test]
    fn an_armada_shaped_threaded_reply_parses_verbatim() {
        // The EXACT tag shape from the spec example (examples.md §2.2) built by
        // hand — proving we parse the cross-client wire form, not just our own
        // builder's output.
        let author = Keys::generate();
        let root_author = Keys::generate();
        let parent_author = Keys::generate();
        let root_id = "ef".repeat(32);
        let parent_id = "12".repeat(32);
        let tags = vec![
            Tag::custom(TagKind::Custom("channel".into()), [crate::simd::hex::bytes_to_hex_32(&chan().0)]),
            Tag::custom(TagKind::Custom("epoch".into()), ["0".to_string()]),
            Tag::custom(TagKind::SingleLetter(SingleLetterTag::uppercase(Alphabet::K)), ["9".to_string()]),
            Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::uppercase(Alphabet::E)),
                [root_id.clone(), String::new(), root_author.public_key().to_hex()],
            ),
            Tag::custom(TagKind::SingleLetter(SingleLetterTag::uppercase(Alphabet::P)), [root_author.public_key().to_hex()]),
            Tag::custom(TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::K)), ["1111".to_string()]),
            Tag::custom(TagKind::e(), [parent_id.clone(), String::new(), parent_author.public_key().to_hex()]),
            Tag::custom(TagKind::p(), [parent_author.public_key().to_hex()]),
        ];
        let rumor = stream::build_rumor_ms(kind::COMMENT, author.public_key(), "nested reply", tags, AT);
        let ChatEvent::Message { reply_to, .. } = open(&seal(&rumor, &author)).unwrap() else {
            panic!("expected a Message");
        };
        // A NESTED reply: the immediate parent (lowercase e), not the root, is
        // the inline context.
        let reply = reply_to.expect("parent parses");
        assert_eq!(reply.id, [0x12; 32]);
        assert_eq!(reply.author, Some(parent_author.public_key()));
    }

    #[test]
    fn a_nested_comment_inherits_its_root_tags_verbatim() {
        let author = Keys::generate();
        let root_id = "ab".repeat(32);
        let root_author_hex = Keys::generate().public_key().to_hex();
        let parent_id = "cd".repeat(32);
        let parent_author_hex = Keys::generate().public_key().to_hex();
        let rumor = build_comment_rumor(
            author.public_key(),
            &chan(),
            Epoch(0),
            "deep",
            &parent_id,
            kind::COMMENT, // the parent is itself a reply
            &parent_author_hex,
            Some((&root_id, kind::MESSAGE, &root_author_hex)),
            &[],
            AT,
        );
        // Root pinned to the ORIGINAL root (stable at any depth), parent to the
        // immediate reply.
        assert!(rumor.tags.iter().any(|t| t.as_slice()[0] == "E" && t.as_slice()[1] == root_id));
        assert!(rumor.tags.iter().any(|t| t.as_slice() == ["K", "9"]));
        assert!(rumor.tags.iter().any(|t| t.as_slice() == ["k", "1111"]));
        assert!(rumor.tags.iter().any(|t| t.as_slice()[0] == "e" && t.as_slice()[1] == parent_id));
    }

    #[test]
    fn a_comment_with_duplicate_parent_tags_is_rejected() {
        // Two lowercase `e` tags = ambiguous parent (which did the author sign
        // off on?) — same discipline as every target-bearing tag.
        let author = Keys::generate();
        let mut tags = stream::channel_binding_tags(&chan(), Epoch(0));
        for id in ["ab", "ff"] {
            tags.push(Tag::custom(TagKind::e(), [
                id.repeat(32),
                String::new(),
                Keys::generate().public_key().to_hex(),
            ]));
        }
        let rumor = stream::build_rumor_ms(kind::COMMENT, author.public_key(), "ambiguous", tags, AT);
        let got = open(&seal(&rumor, &author));
        assert!(matches!(&got, Err(ChatError::DuplicateTag("e"))), "got: {got:?}");
    }

    #[test]
    fn a_reaction_to_a_threaded_reply_carries_k_1111() {
        let author = Keys::generate();
        let rumor = build_reaction_rumor(
            author.public_key(),
            &chan(),
            Epoch(0),
            &"bc".repeat(32),
            &Keys::generate().public_key().to_hex(),
            kind::COMMENT,
            "🔥",
            None,
            AT,
        );
        assert!(rumor.tags.iter().any(|t| t.as_slice() == ["k", "1111"]), "the k tag names the target's kind");
        assert!(matches!(open(&seal(&rumor, &author)).unwrap(), ChatEvent::Reaction { .. }));
    }

    #[test]
    fn reaction_round_trip_and_nip25_shape() {
        let author = Keys::generate();
        let target_author = Keys::generate();
        let target_id = "bc".repeat(32);
        let rumor = build_reaction_rumor(
            author.public_key(),
            &chan(),
            Epoch(0),
            &target_id,
            &target_author.public_key().to_hex(),
            kind::MESSAGE,
            "🔥",
            None,
            AT,
        );
        // NIP-25 shape: the k tag names the reacted-to kind.
        assert!(rumor.tags.iter().any(|t| t.as_slice() == ["k", "9"]));

        let ChatEvent::Reaction { opened, target, target_author: ta, emoji, emoji_url } =
            open(&seal(&rumor, &author)).unwrap()
        else {
            panic!("expected a Reaction");
        };
        assert_eq!(opened.author, author.public_key());
        assert_eq!(target, [0xbc; 32]);
        assert_eq!(ta, target_author.public_key());
        assert_eq!(emoji, "🔥");
        assert_eq!(emoji_url, None);
        assert_eq!(opened.at_ms, AT);
    }

    #[test]
    fn reaction_custom_emoji_carries_the_nip30_url() {
        let author = Keys::generate();
        let rumor = build_reaction_rumor(
            author.public_key(),
            &chan(),
            Epoch(0),
            &"bc".repeat(32),
            &Keys::generate().public_key().to_hex(),
            kind::MESSAGE,
            ":catJAM:",
            Some(("catJAM", "https://x/cat.gif")),
            AT,
        );
        let ChatEvent::Reaction { emoji, emoji_url, .. } = open(&seal(&rumor, &author)).unwrap() else {
            panic!("expected a Reaction");
        };
        assert_eq!(emoji, ":catJAM:");
        assert_eq!(emoji_url, Some("https://x/cat.gif".to_string()));
    }

    #[test]
    fn delete_round_trip_and_optional_target_kind() {
        let author = Keys::generate();
        let rumor = build_delete_rumor(author.public_key(), &chan(), Epoch(0), &"cd".repeat(32), kind::MESSAGE, AT);
        let ChatEvent::Delete { target, target_kind, .. } = open(&seal(&rumor, &author)).unwrap() else {
            panic!("expected a Delete");
        };
        assert_eq!(target, [0xcd; 32]);
        assert_eq!(target_kind, Some(kind::MESSAGE));

        // A k-less delete (the tag is optional in NIP-09) parses with None.
        let mut tags = stream::channel_binding_tags(&chan(), Epoch(0));
        tags.push(Tag::custom(TagKind::e(), ["cd".repeat(32)]));
        let bare = stream::build_rumor_ms(kind::DELETE, author.public_key(), "", tags, AT);
        let ChatEvent::Delete { target_kind, .. } = open(&seal(&bare, &author)).unwrap() else {
            panic!("expected a Delete");
        };
        assert_eq!(target_kind, None);
    }

    #[test]
    fn edit_round_trip_replaces_content() {
        let author = Keys::generate();
        let rumor = build_edit_rumor(author.public_key(), &chan(), Epoch(0), &"de".repeat(32), "fixed the typo", AT);
        let ChatEvent::Edit { opened, target, new_content } = open(&seal(&rumor, &author)).unwrap() else {
            panic!("expected an Edit");
        };
        assert_eq!(opened.author, author.public_key());
        assert_eq!(target, [0xde; 32]);
        assert_eq!(new_content, "fixed the typo");
    }

    #[test]
    fn webxdc_round_trip_is_opaque() {
        let author = Keys::generate();
        let app_tag = Tag::custom(TagKind::Custom("xdc".into()), ["state-update".to_string()]);
        let rumor = build_webxdc_rumor(
            author.public_key(),
            &chan(),
            Epoch(0),
            "{\"move\":\"e4\"}",
            vec![app_tag.clone()],
            AT,
        );
        let ChatEvent::Webxdc { opened } = open(&seal(&rumor, &author)).unwrap() else {
            panic!("expected a Webxdc");
        };
        assert_eq!(opened.rumor.content, "{\"move\":\"e4\"}");
        assert!(opened.rumor.tags.iter().any(|t| t.as_slice() == app_tag.as_slice()));
    }

    #[test]
    fn typing_rides_ephemeral_and_wrap_tier_is_not_content_authority() {
        let author = Keys::generate();
        let typing = build_typing_rumor(author.public_key(), &chan(), Epoch(0), AT);
        let (wrap, _) = seal_chat_rumor(&typing, &group(), &author, WRAP_AT, true).unwrap();
        assert_eq!(wrap.kind.as_u16(), stream::KIND_WRAP_EPHEMERAL);
        let ChatEvent::Typing { opened } = open(&wrap).unwrap() else {
            panic!("expected a Typing");
        };
        assert_eq!(opened.author, author.public_key());
        assert_eq!(opened.rumor.content, "");

        // A kind-9 on a 21059 wrap still opens: the wrap kind is a transport
        // tier (storage policy), the rumor-kind allowlist governs content.
        let msg = build_message_rumor(author.public_key(), &chan(), Epoch(0), "live", None, &[], vec![], AT);
        let (wrap, _) = seal_chat_rumor(&msg, &group(), &author, WRAP_AT, true).unwrap();
        assert!(matches!(open(&wrap), Ok(ChatEvent::Message { .. })));
    }

    #[test]
    fn wrong_channel_and_wrong_epoch_are_rejected() {
        let author = Keys::generate();
        let rumor = build_message_rumor(author.public_key(), &chan(), Epoch(0), "x", None, &[], vec![], AT);
        let wrap = seal(&rumor, &author);
        // Same key, wrong claimed coordinate: the strict-equal binding gates it.
        assert!(matches!(
            open_chat_event(&wrap, &group(), &ChannelId([0xcd; 32]), Epoch(0)),
            Err(ChatError::Stream(StreamError::ChannelMismatch))
        ));
        // A rumor bound to epoch 1 sealed under epoch 0's key (replay shape).
        let stale = build_message_rumor(author.public_key(), &chan(), Epoch(1), "x", None, &[], vec![], AT);
        let wrap = seal(&stale, &author);
        assert!(matches!(open(&wrap), Err(ChatError::Stream(StreamError::EpochMismatch))));
    }

    #[test]
    fn reaction_bound_to_channel_a_under_channel_b_key_is_rejected() {
        // Cross-channel splice: a keyholder of both channels re-seals a rumor
        // bound to A under B's key — the binding must be judged against the
        // key that decrypted, never the rumor's own claim.
        let author = Keys::generate();
        let chan_a = ChannelId([0xaa; 32]);
        let chan_b = ChannelId([0xbb; 32]);
        let group_b = chat_group_key(&secret(), &chan_b, Epoch(0));
        let rumor = build_reaction_rumor(
            author.public_key(),
            &chan_a,
            Epoch(0),
            &"bc".repeat(32),
            &Keys::generate().public_key().to_hex(),
            kind::MESSAGE,
            "🔥",
            None,
            AT,
        );
        let (wrap, _) = seal_chat_rumor(&rumor, &group_b, &author, WRAP_AT, false).unwrap();
        assert!(matches!(
            open_chat_event(&wrap, &group_b, &chan_b, Epoch(0)),
            Err(ChatError::Stream(StreamError::ChannelMismatch))
        ));
    }

    #[test]
    fn multi_epoch_opens_each_wrap_under_its_own_epoch() {
        let author = Keys::generate();
        let key0 = [1u8; 32];
        let key1 = [2u8; 32];
        let held = [(Epoch(0), key0), (Epoch(1), key1)];

        let m0 = build_message_rumor(author.public_key(), &chan(), Epoch(0), "before the rekey", None, &[], vec![], AT);
        let g0 = chat_group_key(&key0, &chan(), Epoch(0));
        let (w0, _) = seal_chat_rumor(&m0, &g0, &author, WRAP_AT, false).unwrap();

        let m1 = build_message_rumor(author.public_key(), &chan(), Epoch(1), "after the rekey", None, &[], vec![], AT + 1);
        let g1 = chat_group_key(&key1, &chan(), Epoch(1));
        let (w1, _) = seal_chat_rumor(&m1, &g1, &author, WRAP_AT, false).unwrap();

        let (ev0, e0) = open_chat_event_multi(&w0, &held, &chan()).unwrap();
        assert_eq!(e0, Epoch(0));
        assert_eq!(ev0.opened().rumor.content, "before the rekey");
        let (ev1, e1) = open_chat_event_multi(&w1, &held, &chan()).unwrap();
        assert_eq!(e1, Epoch(1));
        assert_eq!(ev1.opened().rumor.content, "after the rekey");
    }

    #[test]
    fn multi_epoch_unheld_wrap_is_not_ours() {
        let author = Keys::generate();
        let held = [(Epoch(0), [1u8; 32]), (Epoch(1), [2u8; 32])];
        let m2 = build_message_rumor(author.public_key(), &chan(), Epoch(2), "future", None, &[], vec![], AT);
        let g2 = chat_group_key(&[3u8; 32], &chan(), Epoch(2));
        let (w2, _) = seal_chat_rumor(&m2, &g2, &author, WRAP_AT, false).unwrap();
        assert!(matches!(open_chat_event_multi(&w2, &held, &chan()), Err(ChatError::NoHeldEpoch)));
    }

    #[test]
    fn multi_epoch_cross_epoch_splice_is_rejected() {
        // A rumor bound to epoch 0 re-sealed under epoch 1's key: selection by
        // wrap author picks epoch 1, and the binding must then fail — held-key
        // selection can never launder a stale-epoch rumor.
        let author = Keys::generate();
        let key0 = [1u8; 32];
        let key1 = [2u8; 32];
        let held = [(Epoch(0), key0), (Epoch(1), key1)];
        let stale = build_message_rumor(author.public_key(), &chan(), Epoch(0), "replay", None, &[], vec![], AT);
        let g1 = chat_group_key(&key1, &chan(), Epoch(1));
        let (wrap, _) = seal_chat_rumor(&stale, &g1, &author, WRAP_AT, false).unwrap();
        assert!(matches!(
            open_chat_event_multi(&wrap, &held, &chan()),
            Err(ChatError::Stream(StreamError::EpochMismatch))
        ));
    }

    #[test]
    fn duplicate_e_tag_on_a_reaction_is_rejected() {
        let author = Keys::generate();
        let mut tags = stream::channel_binding_tags(&chan(), Epoch(0));
        tags.push(Tag::custom(TagKind::e(), ["aa".repeat(32)]));
        tags.push(Tag::custom(TagKind::e(), ["bb".repeat(32)]));
        tags.push(Tag::custom(TagKind::p(), [Keys::generate().public_key().to_hex()]));
        let rumor = stream::build_rumor_ms(kind::REACTION, author.public_key(), "+", tags, AT);
        assert!(matches!(
            open(&seal(&rumor, &author)),
            Err(ChatError::DuplicateTag(TAG_TARGET))
        ));
    }

    #[test]
    fn unknown_rumor_kind_is_rejected_on_both_sides() {
        let author = Keys::generate();
        // 3300 is a RETIRED v1 number — burned forever, never a v2 chat kind.
        let tags = stream::channel_binding_tags(&chan(), Epoch(0));
        let rumor = stream::build_rumor_ms(3300, author.public_key(), "v1 ghost", tags, AT);
        assert!(matches!(
            seal_chat_rumor(&rumor, &group(), &author, WRAP_AT, false),
            Err(ChatError::UnknownKind(3300))
        ));
        // And a wrap hand-built around it (bypassing the send gate) dies on open.
        let seal = stream::build_seal(&rumor, SealForm::Encrypted, &group(), &author).unwrap();
        let (wrap, _) = stream::wrap_seal(&seal, &group(), stream::KIND_WRAP, WRAP_AT).unwrap();
        assert!(matches!(open(&wrap), Err(ChatError::UnknownKind(3300))));
    }

    #[test]
    fn plaintext_sealed_chat_event_is_rejected() {
        let author = Keys::generate();
        let rumor = build_message_rumor(author.public_key(), &chan(), Epoch(0), "leaky", None, &[], vec![], AT);
        let seal = stream::build_seal(&rumor, SealForm::Plaintext, &group(), &author).unwrap();
        let (wrap, _) = stream::wrap_seal(&seal, &group(), stream::KIND_WRAP, WRAP_AT).unwrap();
        assert!(matches!(open(&wrap), Err(ChatError::NotEncryptedSealed)));
    }

    #[test]
    fn malformed_targets_are_errors_not_panics() {
        let author = Keys::generate();
        // Reaction: 64 chars but not hex.
        let rumor = build_reaction_rumor(
            author.public_key(),
            &chan(),
            Epoch(0),
            &"zz".repeat(32),
            &Keys::generate().public_key().to_hex(),
            kind::MESSAGE,
            "+",
            None,
            AT,
        );
        assert!(matches!(open(&seal(&rumor, &author)), Err(ChatError::BadTag(TAG_TARGET))));
        // Message: truncated q id.
        let rumor = build_message_rumor(
            author.public_key(),
            &chan(),
            Epoch(0),
            "x",
            Some(("abcd", &Keys::generate().public_key().to_hex())),
            &[],
            vec![],
            AT,
        );
        assert!(matches!(open(&seal(&rumor, &author)), Err(ChatError::BadTag(TAG_QUOTE))));
        // Delete: a k tag that isn't an integer kind.
        let mut tags = stream::channel_binding_tags(&chan(), Epoch(0));
        tags.push(Tag::custom(TagKind::e(), ["cd".repeat(32)]));
        tags.push(Tag::custom(
            TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::K)),
            ["nine".to_string()],
        ));
        let rumor = stream::build_rumor_ms(kind::DELETE, author.public_key(), "", tags, AT);
        assert!(matches!(open(&seal(&rumor, &author)), Err(ChatError::BadTag(TAG_TARGET_KIND))));
        // Reaction missing its p target author entirely.
        let mut tags = stream::channel_binding_tags(&chan(), Epoch(0));
        tags.push(Tag::custom(TagKind::e(), ["cd".repeat(32)]));
        let rumor = stream::build_rumor_ms(kind::REACTION, author.public_key(), "+", tags, AT);
        assert!(matches!(
            open(&seal(&rumor, &author)),
            Err(ChatError::MissingTag(TAG_TARGET_AUTHOR))
        ));
    }
}
