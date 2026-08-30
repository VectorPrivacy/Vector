//! Pins — CORD-04 §7. A pin does not quote a message; it proves one.
//!
//! One Pin List per Channel on the Control Plane (vsk 11, coordinate
//! `pins_locator(community_id, channel_id)`), replaced entire per edit like the
//! Banlist. Each entry carries the original kind-20013 seal verbatim plus the
//! message's disclosed NIP-44 keys, so any reader able to open the list's form
//! verifies author, words, Channel, and signed time — holding no history and no
//! old keys. Compaction re-wraps the head across rotations, which is the whole
//! point of the placement.
//!
//! Wire format shared with Armada's `pins.ts` — entry JSON, both content forms,
//! and the caps are cross-client surface. Divergence silently breaks pin
//! verification between clients.

use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use super::kind;
use super::pin_keys;
use super::stream::OpenedStream;

/// Structural caps (CORD-04 §7 Limits) — a violating edition reads as EMPTY.
pub const PIN_MAX_ENTRIES: usize = 25;
pub const PIN_MAX_CONTENT_BYTES: usize = 32_768;

/// The seal kind a proof requires (an encrypted chat seal).
const KIND_SEAL_ENCRYPTED: u16 = 20013;

/// An Edit's proof bundle: the same disclosure, for the revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinEditBundle {
    pub seal: Event,
    pub keys: String,
}

/// One wire entry. Optional fields are omitted when absent (matching the
/// reference implementation's JSON), and unknown fields are carried through
/// `extra` so republishing an entry never strips what a newer client added.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinEntry {
    /// The original kind-20013 seal event: fields carried exactly, content
    /// string unaltered.
    pub seal: Event,
    /// 76-byte lowercase hex: chacha_key[32] || chacha_nonce[12] || hmac_key[32].
    pub keys: String,
    /// Optional, UNVERIFIED locator hint for jump-to-context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrap: Option<String>,
    /// The newest provable Edit, for readers who hold no Chat plane (§7 Edits).
    /// At most one, ever: Edits target the ORIGINAL rumor and never each other,
    /// so a later Edit REPLACES this rather than appending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit: Option<PinEditBundle>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// A pin that passed the full §7 verification — safe to render.
#[derive(Debug, Clone, Serialize)]
pub struct VerifiedPin {
    /// Recomputed from the decrypted bytes; never the embedded field.
    pub rumor_id: String,
    /// The seal's signer == the rumor's author (hex).
    pub author: String,
    pub kind: u16,
    pub content: String,
    pub tags: Vec<Vec<String>>,
    /// The message's own epoch tag — derives the plane address for jump-to-context.
    pub epoch: Option<String>,
    /// Ordering basis: created_at*1000 + ms tag.
    pub ms: u64,
    pub created_at: u64,
    /// Untrusted locator hint, if the entry carried one.
    pub wrap_hint: Option<String>,
    /// Set when a proven Edit superseded the original's words.
    pub edited: Option<EditedContent>,
    /// The wire entry, verbatim — for republishing (re-wraps, omissions).
    #[serde(skip_serializing)]
    pub entry: PinEntry,
}

#[derive(Debug, Clone, Serialize)]
pub struct EditedContent {
    pub content: String,
    pub ms: u64,
}

/// Why a message could not be pinned — each cause needs a different answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinBuildFailure {
    /// The seal is not an encrypted chat seal (plaintext seals carry no NIP-44
    /// payload to disclose).
    NotEncrypted,
    /// The conversation key does not open this payload — the message is from an
    /// epoch this client does not hold, or the payload is malformed.
    BadPayload,
    /// The built entry failed its own verification; publishing it would burn
    /// list budget on a proof no reader accepts.
    Unverifiable,
}

fn tag_value<'a>(tags: &'a [Vec<String>], name: &str) -> Option<&'a str> {
    tags.iter()
        .find(|t| t.first().map(String::as_str) == Some(name))
        .and_then(|t| t.get(1))
        .map(String::as_str)
}

/// Ordering basis (CORD-02 §4): `created_at * 1000 + ms`, out-of-range ms as 0.
fn resolve_ms(created_at: u64, tags: &[Vec<String>]) -> u64 {
    let ms = tag_value(tags, "ms")
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms <= 999)
        .unwrap_or(0);
    created_at * 1000 + ms
}

/// Build a pin entry from an opened chat message, with the reason attached on
/// refusal. Requires the Channel's stream conversation key at the message's
/// epoch — i.e. the pinner can read what they pin. A pinner told "that message
/// is from an epoch you no longer hold" when the real cause is a non-encrypted
/// seal would retry forever, so the caller gets the distinction.
pub fn build_pin_entry(
    opened: &OpenedStream,
    conv_key: &[u8; 32],
    channel_id_hex: &str,
) -> Result<PinEntry, PinBuildFailure> {
    let seal = &opened.seal;
    if seal.kind.as_u16() != KIND_SEAL_ENCRYPTED {
        return Err(PinBuildFailure::NotEncrypted);
    }
    let keys =
        pin_keys::disclose_keys_for(&seal.content, conv_key).ok_or(PinBuildFailure::BadPayload)?;
    // Deriving keys is not the same as being able to read: the expansion
    // succeeds under ANY conversation key, and a message written under an epoch
    // we no longer hold only fails later, at the MAC. Check here so "you don't
    // hold these keys" and "this proof doesn't hold up" stay different answers.
    if pin_keys::decrypt_with_disclosed_keys(&seal.content, &keys).is_none() {
        return Err(PinBuildFailure::BadPayload);
    }
    let entry = PinEntry {
        seal: seal.clone(),
        keys: pin_keys::encode_message_keys(&keys),
        wrap: Some(opened.wrapper_id.to_hex()),
        edit: None,
        extra: Default::default(),
    };
    // Refuse to build an entry that would not verify — the same gate every
    // reader applies, run before it costs list budget.
    if verify_pin_entry(&entry, channel_id_hex).is_none() {
        return Err(PinBuildFailure::Unverifiable);
    }
    Ok(entry)
}

/// Build an Edit's proof bundle from its opened stream — the same disclosure
/// as an entry, for the revision (§7 Edits). Verification-mirrored like
/// [`build_pin_entry`]: refuse to build what `verify_edit_bundle` would drop.
pub fn build_pin_edit_bundle(
    edit_opened: &OpenedStream,
    conv_key: &[u8; 32],
    original_author: &str,
    original_rumor_id: &str,
    channel_id_hex: &str,
) -> Result<PinEditBundle, PinBuildFailure> {
    let seal = &edit_opened.seal;
    if seal.kind.as_u16() != KIND_SEAL_ENCRYPTED {
        return Err(PinBuildFailure::NotEncrypted);
    }
    let keys =
        pin_keys::disclose_keys_for(&seal.content, conv_key).ok_or(PinBuildFailure::BadPayload)?;
    if pin_keys::decrypt_with_disclosed_keys(&seal.content, &keys).is_none() {
        return Err(PinBuildFailure::BadPayload);
    }
    let bundle = PinEditBundle { seal: seal.clone(), keys: pin_keys::encode_message_keys(&keys) };
    if verify_edit_bundle(&bundle, original_author, original_rumor_id, channel_id_hex).is_none() {
        return Err(PinBuildFailure::Unverifiable);
    }
    Ok(bundle)
}

/// The rumor fields step 4 inspects, parsed strictly from the decrypted bytes.
#[derive(Deserialize)]
struct RumorFields {
    pubkey: String,
    kind: u16,
    content: String,
    created_at: u64,
    tags: Vec<Vec<String>>,
}

/// Recompute the rumor's NIP-01 id from its decrypted fields — an embedded
/// `id` is never trusted.
fn recompute_rumor_id(r: &RumorFields) -> Option<String> {
    let pubkey = PublicKey::from_hex(&r.pubkey).ok()?;
    let tags: Vec<Tag> = r.tags.iter().map(|t| Tag::parse(t.clone()).ok()).collect::<Option<_>>()?;
    let id = EventId::compute(
        &pubkey,
        &Timestamp::from(r.created_at),
        &Kind::from(r.kind),
        &Tags::from_list(tags),
        &r.content,
    );
    Some(id.to_hex())
}

/// The §7 verification, holding nothing but the pin and the list's Channel:
/// seal kind + signature → MAC → decrypt → rumor checks (author equality, chat
/// kind, channel binding) → recomputed id. `None` on ANY failure; a failed
/// entry is dropped alone, its edition folds normally.
pub fn verify_pin_entry(entry: &PinEntry, channel_id_hex: &str) -> Option<VerifiedPin> {
    let seal = &entry.seal;
    // Step 1 — an encrypted chat seal, honestly signed. `verify` checks both
    // the id-hash and the Schnorr signature.
    if seal.kind.as_u16() != KIND_SEAL_ENCRYPTED || seal.verify().is_err() {
        return None;
    }

    // Steps 2–3 — MAC, then decrypt, under the disclosed keys alone.
    let keys = pin_keys::decode_message_keys(&entry.keys)?;
    let plaintext = pin_keys::decrypt_with_disclosed_keys(&seal.content, &keys)?;

    // Step 4 — the rumor's own claims, each strict.
    let rumor: RumorFields = serde_json::from_str(&plaintext).ok()?;
    // NIP-59's impersonation check: renderers display rumor fields, so a seal
    // honestly signed around a rumor claiming another author must fail.
    if rumor.pubkey != seal.pubkey.to_hex() {
        return None;
    }
    if rumor.kind != kind::MESSAGE && rumor.kind != kind::COMMENT {
        return None;
    }
    // The rumor names its Channel under the author's signature (CORD-01
    // Binding); strict equality against the list's own Channel, absence
    // failing — without this, a private Channel's keyholder could pin its
    // messages into a public list, disclosing them Community-wide with proof.
    if channel_id_hex.len() != 64
        || !channel_id_hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        || tag_value(&rumor.tags, "channel") != Some(channel_id_hex)
    {
        return None;
    }

    // Step 5 — identity from the bytes, never from a field.
    let rumor_id = recompute_rumor_id(&rumor)?;

    // The Edit bundle, if the entry carries one. A bad bundle drops the EDIT,
    // never the pin: the original is still proven, and refusing it outright
    // would hide a message because someone attached a bad correction.
    let edited = entry
        .edit
        .as_ref()
        .and_then(|b| verify_edit_bundle(b, &rumor.pubkey, &rumor_id, channel_id_hex));

    Some(VerifiedPin {
        author: rumor.pubkey,
        kind: rumor.kind,
        content: edited.as_ref().map(|e| e.content.clone()).unwrap_or_else(|| rumor.content.clone()),
        epoch: tag_value(&rumor.tags, "epoch").map(str::to_string),
        ms: resolve_ms(rumor.created_at, &rumor.tags),
        created_at: rumor.created_at,
        tags: rumor.tags,
        rumor_id,
        wrap_hint: entry.wrap.clone(),
        edited,
        entry: entry.clone(),
    })
}

/// An Edit bundle proves the SAME author revised THIS message: the five steps
/// of [`verify_pin_entry`] with kind `3302` substituted, plus the fold's own
/// two rules — author equality (nobody else may revise another member's words)
/// and an `e` tag naming the original's recomputed rumor id.
fn verify_edit_bundle(
    bundle: &PinEditBundle,
    original_author: &str,
    original_rumor_id: &str,
    channel_id_hex: &str,
) -> Option<EditedContent> {
    let seal = &bundle.seal;
    if seal.kind.as_u16() != KIND_SEAL_ENCRYPTED {
        return None;
    }
    // Author equality is checkable before any crypto: a bundle sealed by anyone
    // but the original's author cannot revise it, whatever it decrypts to.
    if seal.pubkey.to_hex() != original_author {
        return None;
    }
    if seal.verify().is_err() {
        return None;
    }

    let keys = pin_keys::decode_message_keys(&bundle.keys)?;
    let plaintext = pin_keys::decrypt_with_disclosed_keys(&seal.content, &keys)?;
    let rumor: RumorFields = serde_json::from_str(&plaintext).ok()?;
    if rumor.pubkey != seal.pubkey.to_hex() {
        return None;
    }
    if rumor.kind != kind::EDIT {
        return None;
    }
    // The Edit binds to this Channel too — the revision path opens no door the
    // entry path closes.
    if tag_value(&rumor.tags, "channel") != Some(channel_id_hex) {
        return None;
    }
    if tag_value(&rumor.tags, "e") != Some(original_rumor_id) {
        return None;
    }
    Some(EditedContent {
        content: rumor.content,
        ms: resolve_ms(rumor.created_at, &rumor.tags),
    })
}

// ── The list's two self-describing content forms ─────────────────────────────

#[derive(Serialize, Deserialize)]
struct PlainForm {
    entries: Vec<PinEntry>,
}

/// Serialize a pin list's `content` for a PUBLIC Channel (plaintext — the
/// plane's wrap is the gate). Errors on a cap violation: a writer must never
/// publish an edition every reader would read as empty.
pub fn serialize_public_pin_list(entries: &[PinEntry]) -> Result<String, String> {
    let content = serde_json::to_string(&PlainForm { entries: entries.to_vec() })
        .map_err(|e| e.to_string())?;
    assert_caps(entries.len(), &content)?;
    Ok(content)
}

/// Serialize for a PRIVATE Channel: the entries sealed under the Channel's
/// group conversation key at `epoch`. Both caps are checked on the final
/// carried bytes, the sealed envelope living INSIDE the byte cap.
pub fn serialize_sealed_pin_list(
    entries: &[PinEntry],
    conv_key: &nostr_sdk::prelude::nip44::v2::ConversationKey,
    epoch: u64,
) -> Result<String, String> {
    if entries.len() > PIN_MAX_ENTRIES {
        return Err(format!("pin list exceeds {PIN_MAX_ENTRIES} entries"));
    }
    let inner = serde_json::to_string(&PlainForm { entries: entries.to_vec() })
        .map_err(|e| e.to_string())?;
    let sealed_raw = crate::community::cipher::encrypt_with_random_nonce(conv_key, inner.as_bytes())?;
    let sealed = base64_simd::STANDARD.encode_to_string(&sealed_raw);
    let content = serde_json::json!({ "epoch": epoch.to_string(), "sealed": sealed }).to_string();
    assert_caps(entries.len(), &content)?;
    Ok(content)
}

fn assert_caps(count: usize, content: &str) -> Result<(), String> {
    if count > PIN_MAX_ENTRIES {
        return Err(format!("pin list exceeds {PIN_MAX_ENTRIES} entries"));
    }
    let bytes = content.len();
    if bytes > PIN_MAX_CONTENT_BYTES {
        return Err(format!("pin list content is {bytes} bytes (cap {PIN_MAX_CONTENT_BYTES})"));
    }
    Ok(())
}

/// A read list: its entries, or the fact it stayed dark.
pub struct ReadPinList {
    pub entries: Vec<PinEntry>,
    /// The sealed form under an epoch key this reader lacks. Darkness, not
    /// violation — and a WRITER seeing this must withhold, never publish.
    pub sealed: bool,
}

/// Read a pin list edition's `content` (§7 Limits): the byte cap judged on the
/// exact carried bytes by every reader; the entry cap by whoever can open the
/// form. A violating or unreadable-as-JSON edition reads as an EMPTY list —
/// never refused from the fold.
pub fn read_pin_list(
    content: &str,
    unseal_key: impl Fn(u64) -> Option<[u8; 32]>,
) -> ReadPinList {
    const EMPTY: fn() -> ReadPinList = || ReadPinList { entries: Vec::new(), sealed: false };
    if content.len() > PIN_MAX_CONTENT_BYTES {
        return EMPTY();
    }
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content) else {
        return EMPTY();
    };

    // Public form: { "entries": [...] }
    if parsed.get("entries").is_some() {
        let Ok(form) = serde_json::from_value::<PlainForm>(parsed) else {
            return EMPTY();
        };
        if form.entries.len() > PIN_MAX_ENTRIES {
            return EMPTY();
        }
        return ReadPinList { entries: form.entries, sealed: false };
    }

    // Sealed form: { "epoch": "<decimal u64>", "sealed": "<base64>" }
    let (Some(epoch_str), Some(sealed)) = (
        parsed.get("epoch").and_then(|v| v.as_str()),
        parsed.get("sealed").and_then(|v| v.as_str()),
    ) else {
        return EMPTY();
    };
    if epoch_str != "0" && (epoch_str.is_empty() || epoch_str.starts_with('0')) {
        return EMPTY();
    }
    let Ok(epoch) = epoch_str.parse::<u64>() else {
        return EMPTY();
    };
    let Some(key) = unseal_key(epoch) else {
        return ReadPinList { entries: Vec::new(), sealed: true };
    };
    let Some(conv_key) = nostr_sdk::prelude::nip44::v2::ConversationKey::from_slice(&key).ok() else {
        return EMPTY();
    };
    let Ok(raw) = base64_simd::STANDARD.decode_to_vec(sealed.as_bytes()) else {
        return EMPTY();
    };
    let Ok(inner) = nostr_sdk::prelude::nip44::v2::decrypt_to_bytes(&conv_key, &raw) else {
        return EMPTY();
    };
    let Ok(form) = serde_json::from_slice::<PlainForm>(&inner) else {
        return EMPTY();
    };
    if form.entries.len() > PIN_MAX_ENTRIES {
        return EMPTY();
    }
    ReadPinList { entries: form.entries, sealed: false }
}

// ── Deletion (§7): self-erasure outranks curation ────────────────────────────

/// Whether a kind-5 kills this pin: matched by the RECOMPUTED rumor id against
/// the delete's `e` tags, honored only when the delete's author equals the
/// pin's proven author.
pub fn pin_killed_by(pin: &VerifiedPin, delete_author_hex: &str, delete_tags: &[Vec<String>]) -> bool {
    if delete_author_hex != pin.author {
        return false;
    }
    delete_tags
        .iter()
        .any(|t| t.first().map(String::as_str) == Some("e") && t.get(1).map(String::as_str) == Some(pin.rumor_id.as_str()))
}

/// Attach the newest provable Edit to an entry (§7 Edits). Requires the
/// Channel conversation key of the Edit's own epoch — i.e. the curator can
/// read it. Returns the entry unchanged when the Edit cannot be proven, so a
/// refresh never downgrades a good pin into a broken one.
pub fn with_proven_edit(
    entry: &PinEntry,
    edit_opened: &OpenedStream,
    conv_key: &[u8; 32],
    channel_id_hex: &str,
) -> PinEntry {
    let seal = &edit_opened.seal;
    if seal.kind.as_u16() != KIND_SEAL_ENCRYPTED {
        return entry.clone();
    }
    let Some(keys) = pin_keys::disclose_keys_for(&seal.content, conv_key) else {
        return entry.clone();
    };
    let mut candidate = entry.clone();
    candidate.edit = Some(PinEditBundle {
        seal: seal.clone(),
        keys: pin_keys::encode_message_keys(&keys),
    });
    // Only keep it if it actually verifies against this entry — the same gate a
    // reader will apply, run before it costs list budget.
    match verify_pin_entry(&candidate, channel_id_hex) {
        Some(v) if v.edited.is_some() => candidate,
        _ => entry.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::community::v2::chat::{
        build_edit_rumor, build_message_rumor, open_chat_event, seal_chat_rumor, ChatEvent,
    };
    use crate::community::v2::derive::channel_group_key;
    use crate::community::{ChannelId, Epoch};

    const AT: u64 = 1_686_840_217_417;
    const WRAP_AT: Timestamp = Timestamp::from_secs(1_700_000_000);

    fn chan() -> ChannelId {
        ChannelId([0xab; 32])
    }

    fn chan_hex() -> String {
        crate::simd::hex::bytes_to_hex_32(&chan().0)
    }

    fn group() -> crate::community::v2::derive::GroupKey {
        channel_group_key(&[7u8; 32], &chan(), Epoch(0))
    }

    fn conv_bytes() -> [u8; 32] {
        group().conv_key().as_bytes().try_into().unwrap()
    }

    /// A real opened chat message, through the production seal/open pipeline.
    fn opened_message(author: &Keys, text: &str) -> OpenedStream {
        let rumor = build_message_rumor(author.public_key(), &chan(), Epoch(0), text, None, &[], vec![], AT);
        let wrap = seal_chat_rumor(&rumor, &group(), author, WRAP_AT, false).unwrap().0;
        match open_chat_event(&wrap, &group(), &chan(), Epoch(0)).unwrap() {
            ChatEvent::Message { opened, .. } => opened,
            other => panic!("expected Message, got {other:?}"),
        }
    }

    fn entry_for(author: &Keys, text: &str) -> (PinEntry, OpenedStream) {
        let opened = opened_message(author, text);
        let entry = build_pin_entry(&opened, &conv_bytes(), &chan_hex()).unwrap();
        (entry, opened)
    }

    #[test]
    fn a_built_entry_verifies_and_proves_the_author_and_words() {
        let author = Keys::generate();
        let (entry, opened) = entry_for(&author, "pin me, I'm important");
        let v = verify_pin_entry(&entry, &chan_hex()).expect("verifies");
        assert_eq!(v.author, author.public_key().to_hex());
        assert_eq!(v.content, "pin me, I'm important");
        assert_eq!(v.rumor_id, opened.rumor_id.to_hex(), "identity = recomputed rumor id");
        assert_eq!(v.ms, AT);
        assert_eq!(v.epoch.as_deref(), Some("0"));
        assert!(v.edited.is_none());
    }

    /// The channel binding: a keyholder must not be able to pin channel X's
    /// message into channel Y's list, proof intact (§7 step 4).
    #[test]
    fn a_pin_cannot_cross_channels() {
        let author = Keys::generate();
        let (entry, _) = entry_for(&author, "private words");
        let other = crate::simd::hex::bytes_to_hex_32(&[0xcd; 32]);
        assert!(verify_pin_entry(&entry, &other).is_none());
        // And a malformed channel id fails closed.
        assert!(verify_pin_entry(&entry, "not-hex").is_none());
        assert!(verify_pin_entry(&entry, "").is_none());
    }

    #[test]
    fn tampering_with_the_disclosed_keys_or_seal_fails() {
        let author = Keys::generate();
        let (entry, _) = entry_for(&author, "immutable");
        // Wrong keys: MAC fails.
        let mut bad = entry.clone();
        bad.keys = format!("{}{}", &entry.keys[2..], "00");
        assert!(verify_pin_entry(&bad, &chan_hex()).is_none());
        // A different author's seal around the same payload: signature check
        // rejects a re-signed seal (id no longer matches its own content).
        let mut forged = entry.clone();
        forged.seal.pubkey = Keys::generate().public_key();
        assert!(verify_pin_entry(&forged, &chan_hex()).is_none());
    }

    #[test]
    fn a_proven_edit_replaces_the_words_and_a_foreign_one_is_refused() {
        let author = Keys::generate();
        let (entry, opened) = entry_for(&author, "teh typo");

        // The author's own edit, through the real pipeline.
        let edit_rumor = build_edit_rumor(author.public_key(), &chan(), Epoch(0), &opened.rumor_id.to_hex(), "the typo, fixed", &[], AT + 5_000);
        let wrap = seal_chat_rumor(&edit_rumor, &group(), &author, WRAP_AT, false).unwrap().0;
        let edit_opened = match open_chat_event(&wrap, &group(), &chan(), Epoch(0)).unwrap() {
            ChatEvent::Edit { opened, .. } => opened,
            other => panic!("expected Edit, got {other:?}"),
        };

        let refreshed = with_proven_edit(&entry, &edit_opened, &conv_bytes(), &chan_hex());
        let v = verify_pin_entry(&refreshed, &chan_hex()).unwrap();
        assert_eq!(v.content, "the typo, fixed", "edited words render as current");
        assert_eq!(v.edited.as_ref().unwrap().ms, AT + 5_000);

        // A STRANGER's edit of the same message: refused, entry unchanged.
        let stranger = Keys::generate();
        let foreign_rumor = build_edit_rumor(stranger.public_key(), &chan(), Epoch(0), &opened.rumor_id.to_hex(), "hijacked", &[], AT + 6_000);
        let wrap = seal_chat_rumor(&foreign_rumor, &group(), &stranger, WRAP_AT, false).unwrap().0;
        let foreign_opened = match open_chat_event(&wrap, &group(), &chan(), Epoch(0)).unwrap() {
            ChatEvent::Edit { opened, .. } => opened,
            other => panic!("expected Edit, got {other:?}"),
        };
        let unchanged = with_proven_edit(&entry, &foreign_opened, &conv_bytes(), &chan_hex());
        assert!(unchanged.edit.is_none(), "a stranger's edit must not attach");
    }

    #[test]
    fn public_list_round_trips_and_respects_caps() {
        let author = Keys::generate();
        let (entry, _) = entry_for(&author, "hello");
        let content = serialize_public_pin_list(&[entry.clone()]).unwrap();
        let read = read_pin_list(&content, |_| None);
        assert!(!read.sealed);
        assert_eq!(read.entries.len(), 1);
        assert!(verify_pin_entry(&read.entries[0], &chan_hex()).is_some(), "survives the round trip");

        // 26 entries: the writer refuses...
        let many: Vec<PinEntry> = (0..26).map(|_| entry.clone()).collect();
        assert!(serialize_public_pin_list(&many).is_err());
        // ...and a reader treats a hand-built violating edition as EMPTY.
        let violating = serde_json::json!({ "entries": many }).to_string();
        assert_eq!(read_pin_list(&violating, |_| None).entries.len(), 0);
    }

    #[test]
    fn sealed_list_is_dark_without_the_key_and_opens_with_it() {
        let author = Keys::generate();
        let (entry, _) = entry_for(&author, "private pin");
        let content = serialize_sealed_pin_list(&[entry], group().conv_key(), 4).unwrap();

        // No key: darkness, not violation — and NOT an empty public list.
        let dark = read_pin_list(&content, |_| None);
        assert!(dark.sealed);
        assert!(dark.entries.is_empty());

        // The right key at the named epoch opens it.
        let lit = read_pin_list(&content, |epoch| (epoch == 4).then(conv_bytes));
        assert!(!lit.sealed);
        assert_eq!(lit.entries.len(), 1);
        assert!(verify_pin_entry(&lit.entries[0], &chan_hex()).is_some());

        // A wrong key reads as empty (decrypt fails), never as a panic.
        let wrong = read_pin_list(&content, |_| Some([9u8; 32]));
        assert!(wrong.entries.is_empty());
    }

    #[test]
    fn garbage_content_reads_as_empty_never_panics() {
        for bad in ["", "not json", "[]", "42", r#"{"entries": 7}"#, r#"{"epoch":"x","sealed":"y"}"#, r#"{"epoch":"04","sealed":"y"}"#] {
            let read = read_pin_list(bad, |_| None);
            assert!(read.entries.is_empty(), "{bad}");
            assert!(!read.sealed, "{bad}");
        }
        // Hostile entries inside a well-formed list: dropped at verify, not a panic.
        let hostile = r#"{"entries":[null, 42, {"seal": null}, {"keys": "zz"}]}"#;
        let read = read_pin_list(hostile, |_| None);
        for e in &read.entries {
            assert!(verify_pin_entry(e, &chan_hex()).is_none());
        }
    }

    #[test]
    fn deletion_matches_by_recomputed_id_and_author_only() {
        let author = Keys::generate();
        let (entry, opened) = entry_for(&author, "delete me later");
        let v = verify_pin_entry(&entry, &chan_hex()).unwrap();
        let e_tag = vec![vec!["e".to_string(), opened.rumor_id.to_hex()]];

        // The author's own delete kills it.
        assert!(pin_killed_by(&v, &author.public_key().to_hex(), &e_tag));
        // Someone else's delete of the same id does not.
        assert!(!pin_killed_by(&v, &Keys::generate().public_key().to_hex(), &e_tag));
        // The author's delete of a DIFFERENT message does not.
        let other_tag = vec![vec!["e".to_string(), "ff".repeat(32)]];
        assert!(!pin_killed_by(&v, &author.public_key().to_hex(), &other_tag));
    }

    /// Wire-shape guarantees shared with Armada: optional fields absent when
    /// unset, unknown fields carried through a round trip.
    #[test]
    fn wire_json_matches_the_reference_shape() {
        let author = Keys::generate();
        let (mut entry, _) = entry_for(&author, "wire check");
        entry.wrap = None;
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("wrap").is_none(), "unset wrap must be omitted, not null");
        assert!(json.get("edit").is_none(), "unset edit must be omitted, not null");

        // An unknown field a future client added survives our round trip.
        let mut with_extra = serde_json::to_value(&entry).unwrap();
        with_extra["future_field"] = serde_json::json!({"x": 1});
        let reparsed: PinEntry = serde_json::from_value(with_extra).unwrap();
        assert_eq!(reparsed.extra.get("future_field").unwrap()["x"], 1);
        let re_serialized = serde_json::to_value(&reparsed).unwrap();
        assert_eq!(re_serialized["future_field"]["x"], 1, "republish must not strip it");
        // And the entry still verifies with the stranger field aboard.
        assert!(verify_pin_entry(&reparsed, &chan_hex()).is_some());
    }

    /// The build-refusal reasons stay distinct — the UI answers each differently.
    #[test]
    fn build_failures_are_distinguishable() {
        let author = Keys::generate();
        let opened = opened_message(&author, "reasons");
        // A conversation key that doesn't open this message: BadPayload.
        assert_eq!(
            build_pin_entry(&opened, &[3u8; 32], &chan_hex()).unwrap_err(),
            PinBuildFailure::BadPayload
        );
        // The right key builds fine.
        assert!(build_pin_entry(&opened, &conv_bytes(), &chan_hex()).is_ok());
    }
}
