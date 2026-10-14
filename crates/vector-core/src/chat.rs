//! Chat types and management — compact message storage.
//!
//! `Chat` uses `CompactMessageVec` internally for memory efficiency.
//! Use `to_serializable()` to convert to frontend-friendly `SerializableChat`.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use crate::compact::{CompactMessage, CompactMessageVec, NpubInterner, encode_message_id, decode_message_id};
use crate::types::Message;

// ============================================================================
// Chat (Internal Storage)
// ============================================================================

#[derive(Clone, Debug)]
pub struct Chat {
    pub id: String,
    pub chat_type: ChatType,
    pub participants: Vec<u16>,
    pub messages: CompactMessageVec,
    pub last_read: [u8; 32],
    /// "Mark as Unread" from a chosen message: while set, that message and every later message
    /// from others count as unread; our own messages after it don't end the count. A read clears it.
    pub unread_from: [u8; 32],
    pub created_at: u64,
    pub metadata: ChatMetadata,
    pub muted: bool,
    pub typing_participants: Vec<(u16, u64)>,
    /// Local cached file path for the per-DM wallpaper, or empty when unset.
    /// Populated from the most recent kind-30078 d=vector-wallpaper rumor
    /// received for this chat (the encrypted Blossom file is fetched and
    /// decrypted to this path).
    pub wallpaper_path: String,
    /// Rumor created_at (Unix seconds) that produced the current wallpaper.
    /// Used as the latest-write-wins tiebreaker on concurrent sets.
    pub wallpaper_ts: u64,
    /// Blur amount in pixels applied to the wallpaper background layer
    /// (0..=30, clamped on read). 0 means no blur.
    pub wallpaper_blur: u8,
    /// Brightness percent applied to the wallpaper background layer
    /// (0..=100, clamped on read). 100 means no darkening; 0 is fully
    /// dim. Default 70 keeps text readable on photographic wallpapers.
    pub wallpaper_dim: u8,
    /// Blossom URL of the encrypted wallpaper blob currently in effect.
    /// Used to DELETE the previous blob when we (or our other device)
    /// replace the wallpaper, so stale uploads don't linger on servers.
    pub wallpaper_url: String,
    /// npub (bech32) of whichever account uploaded the current wallpaper.
    /// Gate on this before issuing the Blossom DELETE — only the original
    /// uploader's signature satisfies the server's auth challenge.
    pub wallpaper_uploader: String,
}

impl Chat {
    pub fn new(id: String, chat_type: ChatType, participants: Vec<u16>) -> Self {
        Self {
            id,
            chat_type,
            participants,
            messages: CompactMessageVec::new(),
            last_read: [0u8; 32],
            unread_from: [0u8; 32],
            created_at: web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            metadata: ChatMetadata::new(),
            muted: false,
            typing_participants: Vec::new(),
            wallpaper_path: String::new(),
            wallpaper_ts: 0,
            wallpaper_blur: 0,
            wallpaper_dim: 50,
            wallpaper_url: String::new(),
            wallpaper_uploader: String::new(),
        }
    }

    pub fn new_dm(their_npub: String, interner: &mut NpubInterner) -> Self {
        let handle = interner.intern(&their_npub);
        Self::new(their_npub, ChatType::DirectMessage, vec![handle])
    }


    /// A Community channel chat (GROUP_PROTOCOL.md). `channel_id` is the channel's
    /// stable random id (hex); `participants` are the members known so far.
    pub fn new_community_channel(channel_id: String, participants: Vec<String>, interner: &mut NpubInterner) -> Self {
        let handles: Vec<u16> = participants.iter().map(|p| interner.intern(p)).collect();
        Self::new(channel_id, ChatType::Community, handles)
    }

    // ========================================================================
    // Message Access
    // ========================================================================

    #[inline]
    pub fn message_count(&self) -> usize { self.messages.len() }

    #[inline]
    pub fn is_empty(&self) -> bool { self.messages.is_empty() }

    #[inline]
    pub fn last_message_time(&self) -> Option<u64> { self.messages.last_timestamp() }

    #[inline]
    pub fn has_message(&self, id: &str) -> bool { self.messages.contains_hex_id(id) }

    #[inline]
    pub fn get_compact_message(&self, id: &str) -> Option<&CompactMessage> {
        self.messages.find_by_hex_id(id)
    }

    #[inline]
    pub fn get_compact_message_mut(&mut self, id: &str) -> Option<&mut CompactMessage> {
        self.messages.find_by_hex_id_mut(id)
    }

    pub fn get_message(&self, id: &str, interner: &NpubInterner) -> Option<Message> {
        self.messages.find_by_hex_id(id).map(|cm| cm.to_message(interner))
    }

    #[inline]
    pub fn iter_compact(&self) -> std::slice::Iter<'_, CompactMessage> {
        self.messages.iter()
    }

    pub fn get_all_messages(&self, interner: &NpubInterner) -> Vec<Message> {
        self.messages.iter().map(|cm| cm.to_message(interner)).collect()
    }

    pub fn get_last_messages(&self, n: usize, interner: &NpubInterner) -> Vec<Message> {
        let len = self.messages.len();
        let start = len.saturating_sub(n);
        self.messages.messages()[start..].iter().map(|cm| cm.to_message(interner)).collect()
    }

    // ========================================================================
    // Message Mutation
    // ========================================================================

    pub fn add_message(&mut self, message: Message, interner: &mut NpubInterner) -> bool {
        let compact = CompactMessage::from_message_owned(message, interner);
        self.messages.insert(compact)
    }

    #[inline]
    pub fn add_compact_message(&mut self, message: CompactMessage) -> bool {
        self.messages.insert(message)
    }

    pub fn set_as_read(&mut self) -> bool {
        for msg in self.messages.iter().rev() {
            if !msg.flags.is_mine() {
                self.mark_read_at(*msg.id());
                return true;
            }
        }
        false
    }

    /// Advance the read marker; reading ends a "Mark as Unread" from a chosen message.
    pub fn mark_read_at(&mut self, id: [u8; 32]) {
        self.last_read = id;
        self.unread_from = [0u8; 32];
    }

    pub fn internal_add_message(&mut self, message: Message, interner: &mut NpubInterner) -> bool {
        self.add_message(message, interner)
    }

    #[inline]
    pub fn get_message_mut(&mut self, id: &str) -> Option<&mut CompactMessage> {
        self.get_compact_message_mut(id)
    }

    // ========================================================================
    // Serialization
    // ========================================================================

    fn resolve_participants(&self, interner: &NpubInterner) -> Vec<String> {
        self.participants.iter()
            .filter_map(|&h| interner.resolve(h).map(|s| s.to_string()))
            .collect()
    }

    pub fn to_serializable(&self, interner: &NpubInterner) -> SerializableChat {
        let resolved = crate::notify::resolve_chat(self);
        SerializableChat {
            id: self.id.clone(),
            chat_type: self.chat_type.clone(),
            participants: self.resolve_participants(interner),
            messages: self.get_all_messages(interner),
            last_read: if self.last_read == [0u8; 32] { String::new() } else { decode_message_id(&self.last_read) },
            unread_from: if self.unread_from == [0u8; 32] { String::new() } else { decode_message_id(&self.unread_from) },
            created_at: self.created_at,
            metadata: self.metadata.clone(),
            muted: resolved.muted,
            notify: resolved.ring.as_u8(),
            everyone_pings: crate::notify::everyone_pings_for_chat(self),
            community_muted: crate::notify::community_muted_for_chat(self),
            wallpaper_path: self.wallpaper_path.clone(),
            wallpaper_ts: self.wallpaper_ts,
            wallpaper_blur: self.wallpaper_blur,
            wallpaper_dim: self.wallpaper_dim,
            wallpaper_url: self.wallpaper_url.clone(),
            wallpaper_uploader: self.wallpaper_uploader.clone(),
        }
    }

    pub fn to_serializable_with_last_n(&self, n: usize, interner: &NpubInterner) -> SerializableChat {
        let resolved = crate::notify::resolve_chat(self);
        SerializableChat {
            id: self.id.clone(),
            chat_type: self.chat_type.clone(),
            participants: self.resolve_participants(interner),
            messages: self.get_last_messages(n, interner),
            last_read: if self.last_read == [0u8; 32] { String::new() } else { decode_message_id(&self.last_read) },
            unread_from: if self.unread_from == [0u8; 32] { String::new() } else { decode_message_id(&self.unread_from) },
            created_at: self.created_at,
            metadata: self.metadata.clone(),
            muted: resolved.muted,
            notify: resolved.ring.as_u8(),
            everyone_pings: crate::notify::everyone_pings_for_chat(self),
            community_muted: crate::notify::community_muted_for_chat(self),
            wallpaper_path: self.wallpaper_path.clone(),
            wallpaper_ts: self.wallpaper_ts,
            wallpaper_blur: self.wallpaper_blur,
            wallpaper_dim: self.wallpaper_dim,
            wallpaper_url: self.wallpaper_url.clone(),
            wallpaper_uploader: self.wallpaper_uploader.clone(),
        }
    }

    // ========================================================================
    // Chat Metadata & Participants
    // ========================================================================

    pub fn get_other_participant(&self, my_npub: &str, interner: &NpubInterner) -> Option<String> {
        match self.chat_type {
            ChatType::DirectMessage => {
                let my_handle = interner.lookup(my_npub);
                self.participants.iter()
                    .find(|&&h| Some(h) != my_handle)
                    .and_then(|&h| interner.resolve(h).map(|s| s.to_string()))
            }
            // Community channels have no single "other" participant.
            ChatType::Community => None,
        }
    }

    pub fn is_dm_with(&self, npub: &str, interner: &NpubInterner) -> bool {
        matches!(self.chat_type, ChatType::DirectMessage)
            && interner.lookup(npub).is_some_and(|h| self.participants.contains(&h))
    }

    pub fn is_community(&self) -> bool { matches!(self.chat_type, ChatType::Community) }

    /// Whether this Community channel is the row its community actually shows in the chat
    /// list. Every channel is registered and synced, but a community surfaces as a SINGLE
    /// row: the one whose id matches its `primary_channel` stamp.
    ///
    /// Anything deciding what the user can SEE or CLEAR must gate on this, not on the
    /// presence of `community_id` — a sibling channel's unread would otherwise badge the
    /// OS with no row to open. A row written before the stamp existed is assumed surfaced
    /// (it re-stamps on the next registration), which matches the chat list's own
    /// fallback so the two can't disagree.
    pub fn is_surfaced_community_channel(&self) -> bool {
        if self.metadata.community_id().is_none() {
            return false; // bare persistence anchor from the message-persist path
        }
        match self.metadata.primary_channel() {
            Some(primary) => primary == self.id,
            None => true,
        }
    }

    pub fn has_participant(&self, npub: &str, interner: &NpubInterner) -> bool {
        interner.lookup(npub).is_some_and(|h| self.participants.contains(&h))
    }

    pub fn get_active_typers(&self, interner: &NpubInterner) -> Vec<String> {
        let now = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH).unwrap().as_secs();
        self.typing_participants.iter()
            .filter(|(_, exp)| *exp > now)
            .filter_map(|(h, _)| interner.resolve(*h).map(|s| s.to_string()))
            .collect()
    }

    pub fn update_typing_participant(&mut self, handle: u16, expires_at: u64) {
        if let Some(entry) = self.typing_participants.iter_mut().find(|(h, _)| *h == handle) {
            entry.1 = expires_at;
        } else {
            self.typing_participants.push((handle, expires_at));
        }
        let now = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH).unwrap().as_secs();
        self.typing_participants.retain(|(_, exp)| *exp > now);
    }

    pub fn id(&self) -> &String { &self.id }
    pub fn chat_type(&self) -> &ChatType { &self.chat_type }
    pub fn participants(&self) -> &[u16] { &self.participants }
    pub fn last_read(&self) -> &[u8; 32] { &self.last_read }
    pub fn created_at(&self) -> u64 { self.created_at }
    pub fn metadata(&self) -> &ChatMetadata { &self.metadata }
    pub fn muted(&self) -> bool { self.muted }
}

// ============================================================================
// SerializableChat (Frontend Communication)
// ============================================================================

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SerializableChat {
    pub id: String,
    pub chat_type: ChatType,
    pub participants: Vec<String>,
    pub messages: Vec<Message>,
    pub last_read: String,
    /// Hex id of the message a "Mark as Unread" counts from; empty when not set.
    #[serde(default)]
    pub unread_from: String,
    pub created_at: u64,
    pub metadata: ChatMetadata,
    /// Effective mute, after the community's own mute and any timer. The row
    /// greys on this; `notify` is what it badges with.
    pub muted: bool,
    /// The resolved level AFTER the mute clamp, as `NotifyLevel::as_u8`. The
    /// frontend badges from this alone rather than re-walking the chain.
    #[serde(default)]
    pub notify: u8,
    /// Whether an authorized @everyone still counts as a ping here, i.e. the
    /// community has not suppressed it.
    #[serde(default = "default_everyone_pings")]
    pub everyone_pings: bool,
    /// Whether the COMMUNITY this chat belongs to is silenced, as opposed to the
    /// chat itself. A surface standing for the whole space reads this one.
    #[serde(default)]
    pub community_muted: bool,
    #[serde(default)]
    pub wallpaper_path: String,
    #[serde(default)]
    pub wallpaper_ts: u64,
    #[serde(default)]
    pub wallpaper_blur: u8,
    #[serde(default = "default_wallpaper_dim")]
    pub wallpaper_dim: u8,
    #[serde(default)]
    pub wallpaper_url: String,
    #[serde(default)]
    pub wallpaper_uploader: String,
}

fn default_wallpaper_dim() -> u8 { 50 }
fn default_everyone_pings() -> bool { true }

impl SerializableChat {
    pub fn to_chat(self, interner: &mut NpubInterner) -> Chat {
        let handles: Vec<u16> = self.participants.iter().map(|p| interner.intern(p)).collect();
        let mut chat = Chat::new(self.id, self.chat_type, handles);
        chat.last_read = if self.last_read.is_empty() { [0u8; 32] } else { encode_message_id(&self.last_read) };
        chat.unread_from = if self.unread_from.is_empty() { [0u8; 32] } else { encode_message_id(&self.unread_from) };
        chat.created_at = self.created_at;
        chat.metadata = self.metadata;
        chat.muted = self.muted;
        chat.wallpaper_path = self.wallpaper_path;
        chat.wallpaper_ts = self.wallpaper_ts;
        chat.wallpaper_blur = self.wallpaper_blur;
        chat.wallpaper_dim = self.wallpaper_dim;
        chat.wallpaper_url = self.wallpaper_url;
        chat.wallpaper_uploader = self.wallpaper_uploader;
        for msg in self.messages {
            chat.add_message(msg, interner);
        }
        chat
    }
}

// ============================================================================
// Supporting Types
// ============================================================================

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ChatType {
    DirectMessage,
    /// A Community channel (GROUP_PROTOCOL.md). The chat `id`
    /// is the channel's stable random id.
    Community,
}

impl ChatType {
    // Discriminant 1 was the removed MlsGroup variant; Community keeps 2 for
    // on-disk compatibility. Legacy chat_type=1 rows are dropped at the DB load layer.
    pub fn to_i32(&self) -> i32 {
        match self {
            ChatType::DirectMessage => 0,
            ChatType::Community => 2,
        }
    }
    pub fn from_i32(value: i32) -> Self {
        match value {
            2 => ChatType::Community,
            _ => ChatType::DirectMessage,
        }
    }
}

/// A chat row's display metadata: string → string, which is all the frontend and
/// the database ever see (`{"custom_fields":{..}}`, keys in byte order).
///
/// A Community channel's keys are held typed, the community-wide ones in one
/// header its sibling rows share. Any other key, and a known key whose value the
/// typed form could not give back byte for byte, stays a plain pair.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ChatMetadata {
    community: Option<Box<CommunityStamp>>,
    /// Sorted by key; never a key whose typed slot is set.
    other: Vec<(Box<str>, Box<str>)>,
}

#[derive(Clone, Debug, PartialEq, Default)]
struct CommunityStamp {
    header: Arc<CommunityHeader>,
    channel_name: Option<Box<str>>,
}

/// The keys every channel row of one community carries alike.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct CommunityHeader {
    community_id: Option<Box<str>>,
    name: Option<Box<str>>,
    description: Option<Box<str>>,
    primary_channel: Option<Box<str>>,
    owner_npub: Option<Box<str>>,
    created_at: Option<u64>,
    is_owner: Option<bool>,
    dissolved: Option<bool>,
    /// Stored as `"1"`.
    icon: bool,
}

/// What a channel registration stamps onto its row.
pub(crate) struct CommunityRegistration<'a> {
    pub community_id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub primary_channel: &'a str,
    pub owner_npub: Option<&'a str>,
    /// `None` keeps whatever the row already holds.
    pub created_at_ms: Option<u64>,
    pub is_owner: bool,
    pub dissolved: bool,
    pub has_icon: bool,
}

/// The typed keys, in byte order of their names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    ChannelName,
    CommunityId,
    CreatedAt,
    Description,
    Dissolved,
    Icon,
    IsOwner,
    Name,
    OwnerNpub,
    PrimaryChannel,
}

impl Field {
    const ALL: [Field; 10] = [
        Field::ChannelName,
        Field::CommunityId,
        Field::CreatedAt,
        Field::Description,
        Field::Dissolved,
        Field::Icon,
        Field::IsOwner,
        Field::Name,
        Field::OwnerNpub,
        Field::PrimaryChannel,
    ];

    fn key(self) -> &'static str {
        match self {
            Field::ChannelName => "channel_name",
            Field::CommunityId => "community_id",
            Field::CreatedAt => "created_at",
            Field::Description => "description",
            Field::Dissolved => "dissolved",
            Field::Icon => "icon",
            Field::IsOwner => "is_owner",
            Field::Name => "name",
            Field::OwnerNpub => "owner_npub",
            Field::PrimaryChannel => "primary_channel",
        }
    }

    fn parse(key: &str) -> Option<Field> {
        Field::ALL.into_iter().find(|f| f.key() == key)
    }
}

fn bool_text(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

fn parse_bool_text(v: &str) -> Option<bool> {
    match v {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// A number only when printing it gives `v` back (no sign, no leading zero).
fn parse_ms_text(v: &str) -> Option<u64> {
    v.parse::<u64>().ok().filter(|n| n.to_string() == v)
}

impl CommunityHeader {
    fn text(&self, f: Field) -> Option<&Option<Box<str>>> {
        match f {
            Field::CommunityId => Some(&self.community_id),
            Field::Name => Some(&self.name),
            Field::Description => Some(&self.description),
            Field::PrimaryChannel => Some(&self.primary_channel),
            Field::OwnerNpub => Some(&self.owner_npub),
            _ => None,
        }
    }

    fn text_mut(&mut self, f: Field) -> Option<&mut Option<Box<str>>> {
        match f {
            Field::CommunityId => Some(&mut self.community_id),
            Field::Name => Some(&mut self.name),
            Field::Description => Some(&mut self.description),
            Field::PrimaryChannel => Some(&mut self.primary_channel),
            Field::OwnerNpub => Some(&mut self.owner_npub),
            _ => None,
        }
    }

    fn heap_size(&self) -> usize {
        [&self.community_id, &self.name, &self.description, &self.primary_channel, &self.owner_npub]
            .into_iter()
            .map(|s| s.as_deref().map_or(0, str::len))
            .sum()
    }
}

impl ChatMetadata {
    pub fn new() -> Self { Self::default() }

    pub fn set_name(&mut self, name: String) { self.insert("name", name); }
    pub fn get_name(&self) -> Option<&str> { self.header()?.name.as_deref() }
    pub fn set_member_count(&mut self, count: usize) { self.insert("member_count", count.to_string()); }
    pub fn get_member_count(&self) -> Option<usize> { self.get("member_count").and_then(|s| s.parse().ok()) }

    /// The owning community's id (hex) on a Community channel row.
    pub fn community_id(&self) -> Option<&str> { self.header()?.community_id.as_deref() }
    /// The community's primary channel id (hex), the one row the list renders.
    pub fn primary_channel(&self) -> Option<&str> { self.header()?.primary_channel.as_deref() }
    /// Whether the owner dissolved this row's community.
    pub fn is_dissolved(&self) -> bool { self.header().is_some_and(|h| h.dissolved == Some(true)) }

    fn header(&self) -> Option<&CommunityHeader> { self.community.as_deref().map(|s| &*s.header) }

    fn typed(&self, f: Field) -> Option<Cow<'_, str>> {
        let stamp = self.community.as_deref()?;
        let h = &*stamp.header;
        if let Some(text) = h.text(f) {
            return text.as_deref().map(Cow::Borrowed);
        }
        match f {
            Field::ChannelName => stamp.channel_name.as_deref().map(Cow::Borrowed),
            Field::CreatedAt => h.created_at.map(|n| Cow::Owned(n.to_string())),
            Field::IsOwner => h.is_owner.map(|b| Cow::Borrowed(bool_text(b))),
            Field::Dissolved => h.dissolved.map(|b| Cow::Borrowed(bool_text(b))),
            Field::Icon => h.icon.then_some(Cow::Borrowed("1")),
            _ => None,
        }
    }

    fn typed_present(&self, f: Field) -> bool {
        let Some(stamp) = self.community.as_deref() else { return false };
        let h = &*stamp.header;
        match h.text(f) {
            Some(text) => text.is_some(),
            None => match f {
                Field::ChannelName => stamp.channel_name.is_some(),
                Field::CreatedAt => h.created_at.is_some(),
                Field::IsOwner => h.is_owner.is_some(),
                Field::Dissolved => h.dissolved.is_some(),
                Field::Icon => h.icon,
                _ => false,
            },
        }
    }

    /// Hold `value` in `f`'s typed slot; `false` when the slot can't give it back exactly.
    fn set_typed(&mut self, f: Field, value: &str) -> bool {
        let (mut ms, mut flag) = (None, None);
        let fits = match f {
            Field::CreatedAt => {
                ms = parse_ms_text(value);
                ms.is_some()
            }
            Field::IsOwner | Field::Dissolved => {
                flag = parse_bool_text(value);
                flag.is_some()
            }
            Field::Icon => value == "1",
            _ => true,
        };
        if !fits {
            return false;
        }
        let stamp = self.community.get_or_insert_with(Default::default);
        if f == Field::ChannelName {
            stamp.channel_name = Some(value.into());
            return true;
        }
        let h = Arc::make_mut(&mut stamp.header);
        match h.text_mut(f) {
            Some(slot) => *slot = Some(value.into()),
            None => match f {
                Field::CreatedAt => h.created_at = ms,
                Field::IsOwner => h.is_owner = flag,
                Field::Dissolved => h.dissolved = flag,
                Field::Icon => h.icon = true,
                _ => {}
            },
        }
        true
    }

    fn clear_typed(&mut self, f: Field) {
        if !self.typed_present(f) {
            return;
        }
        let Some(stamp) = self.community.as_deref_mut() else { return };
        if f == Field::ChannelName {
            stamp.channel_name = None;
        } else {
            let h = Arc::make_mut(&mut stamp.header);
            match h.text_mut(f) {
                Some(slot) => *slot = None,
                None => match f {
                    Field::CreatedAt => h.created_at = None,
                    Field::IsOwner => h.is_owner = None,
                    Field::Dissolved => h.dissolved = None,
                    Field::Icon => h.icon = false,
                    _ => {}
                },
            }
        }
        if stamp.channel_name.is_none() && *stamp.header == CommunityHeader::default() {
            self.community = None;
        }
    }

    fn other_index(&self, key: &str) -> Result<usize, usize> {
        self.other.binary_search_by(|(k, _)| (**k).cmp(key))
    }

    /// One field's value, exactly as it serializes.
    pub fn get(&self, key: &str) -> Option<Cow<'_, str>> {
        if let Some(v) = Field::parse(key).and_then(|f| self.typed(f)) {
            return Some(v);
        }
        self.other_index(key).ok().map(|i| Cow::Borrowed(&*self.other[i].1))
    }

    pub fn contains_key(&self, key: &str) -> bool {
        Field::parse(key).is_some_and(|f| self.typed_present(f)) || self.other_index(key).is_ok()
    }

    pub fn insert(&mut self, key: &str, value: String) {
        if let Some(f) = Field::parse(key) {
            if self.set_typed(f, &value) {
                self.remove_other(key);
                return;
            }
            self.clear_typed(f);
        }
        match self.other_index(key) {
            Ok(i) => self.other[i].1 = value.into_boxed_str(),
            Err(i) => self.other.insert(i, (key.into(), value.into_boxed_str())),
        }
    }

    pub fn remove(&mut self, key: &str) {
        if let Some(f) = Field::parse(key) {
            self.clear_typed(f);
        }
        self.remove_other(key);
    }

    fn remove_other(&mut self, key: &str) {
        if let Ok(i) = self.other_index(key) {
            self.other.remove(i);
        }
    }

    /// Every field in byte order of its key, as it serializes.
    pub fn entries(&self) -> Vec<(&str, Cow<'_, str>)> {
        let mut out: Vec<(&str, Cow<'_, str>)> = Vec::with_capacity(Field::ALL.len() + self.other.len());
        out.extend(Field::ALL.into_iter().filter_map(|f| self.typed(f).map(|v| (f.key(), v))));
        out.extend(self.other.iter().map(|(k, v)| (&**k, Cow::Borrowed(&**v))));
        // Both halves are sorted and share no key, so only an interleave is left to settle.
        out.sort_unstable_by(|a, b| a.0.cmp(b.0));
        out
    }

    pub fn from_map(map: HashMap<String, String>) -> Self {
        let mut meta = Self::default();
        for (k, v) in map {
            meta.insert(&k, v);
        }
        meta
    }

    /// This row's header under `reg`, before any sharing.
    pub(crate) fn registration_header(&self, reg: &CommunityRegistration<'_>) -> CommunityHeader {
        CommunityHeader {
            community_id: Some(reg.community_id.into()),
            name: Some(reg.name.into()),
            description: Some(reg.description.into()),
            primary_channel: Some(reg.primary_channel.into()),
            owner_npub: reg.owner_npub.map(Into::into),
            created_at: reg.created_at_ms.or_else(|| self.header().and_then(|h| h.created_at)),
            is_owner: Some(reg.is_owner),
            dissolved: Some(reg.dissolved),
            icon: reg.has_icon,
        }
    }

    /// The header this row shares with its community's other rows.
    pub(crate) fn community_header(&self) -> Option<&Arc<CommunityHeader>> {
        self.community.as_deref().map(|s| &s.header)
    }

    /// Stamp a registration: every key it names now answers from `header`, except a
    /// `created_at` the registration left to the row.
    pub(crate) fn stamp_community(&mut self, header: Arc<CommunityHeader>, channel_name: &str, created_at_given: bool) {
        self.community = Some(Box::new(CommunityStamp { header, channel_name: Some(channel_name.into()) }));
        self.other.retain(|(k, _)| match Field::parse(k) {
            Some(Field::CreatedAt) => !created_at_given,
            Some(_) => false,
            None => true,
        });
    }

    /// Bytes held on the heap for this row; a shared header counts its share.
    pub fn heap_size(&self) -> usize {
        let other = self.other.capacity() * std::mem::size_of::<(Box<str>, Box<str>)>()
            + self.other.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>();
        let stamp = self.community.as_deref().map_or(0, |s| {
            std::mem::size_of::<CommunityStamp>()
                + s.channel_name.as_deref().map_or(0, str::len)
                + (std::mem::size_of::<CommunityHeader>() + s.header.heap_size()) / Arc::strong_count(&s.header)
        });
        other + stamp
    }
}

impl Serialize for ChatMetadata {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeStruct};
        struct Fields<'a>(&'a ChatMetadata);
        impl Serialize for Fields<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let entries = self.0.entries();
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (k, v) in &entries {
                    map.serialize_entry(k, v.as_ref())?;
                }
                map.end()
            }
        }
        let mut s = serializer.serialize_struct("ChatMetadata", 1)?;
        s.serialize_field("custom_fields", &Fields(self))?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for ChatMetadata {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename = "ChatMetadata")]
        struct Wire {
            custom_fields: HashMap<String, String>,
        }
        Ok(Self::from_map(Wire::deserialize(deserializer)?.custom_fields))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;
    use crate::compact::NpubInterner;
    use crate::simd::hex::bytes_to_hex_32;

    /// The single discriminator behind three user-visible behaviours: whether a channel
    /// gets a chat-list row, whether its unread badges the OS, and whether it rings a
    /// notification. Every channel is registered and synced; only the primary surfaces.
    #[test]
    fn only_the_primary_channel_of_a_community_is_surfaced() {
        let mut interner = NpubInterner::new();
        let primary_id = make_hex_id(0x21);
        let sibling_id = make_hex_id(0x22);

        let mut stamp = |id: &str, community: bool, primary: Option<&str>| {
            let mut chat = Chat::new_community_channel(id.to_string(), Vec::new(), &mut interner);
            if community {
                chat.metadata.insert("community_id", make_hex_id(0x99));
            }
            if let Some(p) = primary {
                chat.metadata.insert("primary_channel", p.to_string());
            }
            chat
        };

        // A bare persistence anchor (auto-created by the message persist) has no community
        // metadata at all and has never been surfaced.
        assert!(!stamp(&sibling_id, false, None).is_surfaced_community_channel());

        // The community's primary row: the one row the list renders.
        assert!(stamp(&primary_id, true, Some(&primary_id)).is_surfaced_community_channel());

        // A sibling channel: registered, synced, addressable — but no row, so its unread
        // must not badge and its messages must not ring.
        assert!(!stamp(&sibling_id, true, Some(&primary_id)).is_surfaced_community_channel());

        // Written before the stamp existed: assumed surfaced, so a community can't silently
        // vanish from the list (and it re-stamps on the next registration). Matches the chat
        // list's own fallback — the two must not disagree.
        assert!(stamp(&primary_id, true, None).is_surfaced_community_channel());
    }

    // ========================================================================
    // Helpers
    // ========================================================================

    /// Create a deterministic 64-char hex ID from a u8 seed.
    /// First byte is always >= 0x10 to avoid the pending ID marker (0x01).
    fn make_hex_id(seed: u8) -> String {
        let mut bytes = [seed; 32];
        bytes[0] = seed.wrapping_add(0x10) | 0x10; // never 0x00 or 0x01
        bytes[1] = seed.wrapping_mul(37);
        bytes_to_hex_32(&bytes)
    }

    /// Build a test Message with the given parameters.
    fn make_message(id_seed: u8, content: &str, timestamp_ms: u64, mine: bool) -> Message {
        Message {
            id: make_hex_id(id_seed),
            content: content.to_string(),
            at: timestamp_ms,
            mine,
            ..Default::default()
        }
    }

    // ========================================================================
    // Chat Construction
    // ========================================================================

    #[test]
    fn new_dm_creates_correct_type() {
        let mut interner = NpubInterner::new();
        let chat = Chat::new_dm("npub1alice".to_string(), &mut interner);

        assert_eq!(chat.id, "npub1alice", "DM chat id should be the peer's npub");
        assert_eq!(chat.chat_type, ChatType::DirectMessage, "should be DirectMessage type");
        assert_eq!(chat.participants.len(), 1, "DM should have one participant");
        assert!(chat.is_empty(), "new chat should have no messages");
        assert!(!chat.muted, "new chat should not be muted");
        assert_eq!(chat.last_read, [0u8; 32], "last_read should be zeroed");
    }

    #[test]
    fn new_community_channel_with_participants() {
        let mut interner = NpubInterner::new();
        let participants = vec![
            "npub1alice".to_string(),
            "npub1bob".to_string(),
            "npub1charlie".to_string(),
        ];
        let chat = Chat::new_community_channel("grp_abc".to_string(), participants, &mut interner);

        assert_eq!(chat.id, "grp_abc", "group chat id should match");
        assert_eq!(chat.chat_type, ChatType::Community, "should be Community type");
        assert_eq!(chat.participants.len(), 3, "should have 3 participants");
        assert!(chat.is_community(), "is_community() should return true");
    }

    #[test]
    fn new_chat_has_creation_timestamp() {
        let mut interner = NpubInterner::new();
        let chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        // created_at should be recent (within last 5 seconds)
        let now = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH).unwrap().as_secs();
        assert!(
            chat.created_at >= now - 5 && chat.created_at <= now + 1,
            "created_at ({}) should be close to now ({})",
            chat.created_at, now
        );
    }

    // ========================================================================
    // Message Operations
    // ========================================================================

    #[test]
    fn add_message_and_get_message_roundtrip() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        let msg = make_message(1, "hello world", 1700000000000, false);
        let msg_id = msg.id.clone();

        let added = chat.add_message(msg, &mut interner);
        assert!(added, "message should be added successfully");

        let retrieved = chat.get_message(&msg_id, &interner)
            .expect("message should be retrievable");
        assert_eq!(retrieved.content, "hello world", "content should roundtrip");
        assert_eq!(retrieved.id, msg_id, "id should roundtrip");
    }

    #[test]
    fn add_message_dedup() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        let msg1 = make_message(1, "first", 1700000000000, false);
        let msg2 = make_message(1, "duplicate id", 1700000001000, false);

        assert!(chat.add_message(msg1, &mut interner), "first add should succeed");
        assert!(!chat.add_message(msg2, &mut interner), "duplicate ID should be rejected");
        assert_eq!(chat.message_count(), 1, "should have only one message");
    }

    #[test]
    fn message_count_and_is_empty() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        assert!(chat.is_empty(), "new chat should be empty");
        assert_eq!(chat.message_count(), 0, "new chat should have 0 messages");

        chat.add_message(make_message(1, "a", 1700000000000, false), &mut interner);
        assert!(!chat.is_empty(), "chat with message should not be empty");
        assert_eq!(chat.message_count(), 1, "should have 1 message");

        chat.add_message(make_message(2, "b", 1700000001000, false), &mut interner);
        assert_eq!(chat.message_count(), 2, "should have 2 messages");
    }

    #[test]
    fn has_message_check() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        let msg = make_message(1, "test", 1700000000000, false);
        let msg_id = msg.id.clone();
        chat.add_message(msg, &mut interner);

        assert!(chat.has_message(&msg_id), "added message should be found");
        assert!(!chat.has_message(&make_hex_id(99)), "unknown id should not be found");
    }

    #[test]
    fn get_all_messages_returns_all() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        for i in 0..5u8 {
            chat.add_message(
                make_message(i, &format!("msg {}", i), 1700000000000 + i as u64 * 1000, false),
                &mut interner,
            );
        }

        let all = chat.get_all_messages(&interner);
        assert_eq!(all.len(), 5, "should return all 5 messages");
    }

    #[test]
    fn get_last_messages_returns_tail() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        for i in 0..10u8 {
            chat.add_message(
                make_message(i, &format!("msg {}", i), 1700000000000 + i as u64 * 1000, false),
                &mut interner,
            );
        }

        let last3 = chat.get_last_messages(3, &interner);
        assert_eq!(last3.len(), 3, "should return exactly 3 messages");
        assert_eq!(last3[0].content, "msg 7", "first of last 3 should be msg 7");
        assert_eq!(last3[2].content, "msg 9", "last should be msg 9");
    }

    #[test]
    fn last_message_time_tracks_newest() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        assert!(chat.last_message_time().is_none(), "empty chat should have no last time");

        chat.add_message(make_message(1, "a", 1700000001000, false), &mut interner);
        let t1 = chat.last_message_time().expect("should have a timestamp");

        chat.add_message(make_message(2, "b", 1700000005000, false), &mut interner);
        let t2 = chat.last_message_time().expect("should have a timestamp");

        assert!(t2 > t1, "last_message_time should increase with newer messages");
    }

    // ========================================================================
    // set_as_read
    // ========================================================================

    #[test]
    fn set_as_read_marks_last_non_mine() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        let msg_theirs = make_message(1, "from them", 1700000001000, false);
        let msg_mine = make_message(2, "from me", 1700000002000, true);
        let msg_theirs2 = make_message(3, "from them again", 1700000003000, false);
        let last_their_id = msg_theirs2.id.clone();

        chat.add_message(msg_theirs, &mut interner);
        chat.add_message(msg_mine, &mut interner);
        chat.add_message(msg_theirs2, &mut interner);

        let marked = chat.set_as_read();
        assert!(marked, "set_as_read should succeed when there are non-mine messages");

        let expected_bytes = crate::compact::encode_message_id(&last_their_id);
        assert_eq!(
            chat.last_read, expected_bytes,
            "last_read should point to the last non-mine message"
        );
    }

    #[test]
    fn set_as_read_all_mine_returns_false() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        chat.add_message(make_message(1, "mine 1", 1700000001000, true), &mut interner);
        chat.add_message(make_message(2, "mine 2", 1700000002000, true), &mut interner);

        let marked = chat.set_as_read();
        assert!(!marked, "set_as_read should return false when all messages are mine");
        assert_eq!(chat.last_read, [0u8; 32], "last_read should remain zeroed");
    }

    // ========================================================================
    // Serialization Roundtrip
    // ========================================================================

    #[test]
    fn to_serializable_and_back_via_to_chat() {
        let mut interner = NpubInterner::new();
        let participants = vec!["npub1alice".to_string(), "npub1bob".to_string()];
        let mut chat = Chat::new_community_channel("grp_test".to_string(), participants.clone(), &mut interner);

        chat.metadata.set_name("Test Group".to_string());
        chat.muted = true;

        // Add messages
        chat.add_message(make_message(1, "hello", 1700000001000, false), &mut interner);
        chat.add_message(make_message(2, "world", 1700000002000, true), &mut interner);

        // Set last_read
        chat.set_as_read();

        // Serialize to SerializableChat
        let serializable = chat.to_serializable(&interner);
        assert_eq!(serializable.id, "grp_test", "serialized id should match");
        assert_eq!(serializable.chat_type, ChatType::Community, "serialized type should match");
        assert_eq!(serializable.participants.len(), 2, "should have 2 participants");
        assert_eq!(serializable.messages.len(), 2, "should have 2 messages");
        assert!(serializable.muted, "muted should be preserved");
        assert_eq!(
            serializable.metadata.get_name(),
            Some("Test Group"),
            "metadata name should be preserved"
        );

        // Convert back to Chat
        let mut interner2 = NpubInterner::new();
        let restored = serializable.to_chat(&mut interner2);

        assert_eq!(restored.id, "grp_test", "restored id should match");
        assert_eq!(restored.chat_type, ChatType::Community, "restored type should match");
        assert_eq!(restored.participants.len(), 2, "restored participants count should match");
        assert_eq!(restored.message_count(), 2, "restored message count should match");
        assert!(restored.muted, "restored muted should be true");
        assert_ne!(restored.last_read, [0u8; 32], "restored last_read should be non-zero");
    }

    #[test]
    fn to_serializable_with_last_n() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        for i in 0..10u8 {
            chat.add_message(
                make_message(i, &format!("msg {}", i), 1700000000000 + i as u64 * 1000, false),
                &mut interner,
            );
        }

        let serialized = chat.to_serializable_with_last_n(3, &interner);
        assert_eq!(serialized.messages.len(), 3, "should only include last 3 messages");
    }

    // ========================================================================
    // Participants
    // ========================================================================

    #[test]
    fn has_participant_check() {
        let mut interner = NpubInterner::new();
        let chat = Chat::new_community_channel(
            "grp1".to_string(),
            vec!["npub1alice".to_string(), "npub1bob".to_string()],
            &mut interner,
        );

        assert!(
            chat.has_participant("npub1alice", &interner),
            "alice should be a participant"
        );
        assert!(
            chat.has_participant("npub1bob", &interner),
            "bob should be a participant"
        );
        assert!(
            !chat.has_participant("npub1charlie", &interner),
            "charlie should not be a participant"
        );
    }

    #[test]
    fn is_dm_with_check() {
        let mut interner = NpubInterner::new();
        let chat = Chat::new_dm("npub1alice".to_string(), &mut interner);

        assert!(
            chat.is_dm_with("npub1alice", &interner),
            "should be a DM with alice"
        );
        assert!(
            !chat.is_dm_with("npub1bob", &interner),
            "should not be a DM with bob"
        );
    }

    #[test]
    fn is_dm_with_returns_false_for_group() {
        let mut interner = NpubInterner::new();
        let chat = Chat::new_community_channel(
            "grp1".to_string(),
            vec!["npub1alice".to_string()],
            &mut interner,
        );

        assert!(
            !chat.is_dm_with("npub1alice", &interner),
            "community channel should not match is_dm_with even if participant matches"
        );
    }

    #[test]
    fn get_other_participant_dm() {
        let mut interner = NpubInterner::new();
        // In a DM, the participant list typically has the other person
        let chat = Chat::new_dm("npub1bob".to_string(), &mut interner);

        let other = chat.get_other_participant("npub1alice", &interner);
        assert_eq!(
            other, Some("npub1bob".to_string()),
            "should return bob as the other participant"
        );
    }

    #[test]
    fn get_other_participant_returns_none_for_group() {
        let mut interner = NpubInterner::new();
        let chat = Chat::new_community_channel(
            "grp1".to_string(),
            vec!["npub1alice".to_string(), "npub1bob".to_string()],
            &mut interner,
        );

        assert!(
            chat.get_other_participant("npub1alice", &interner).is_none(),
            "community channel should return None for get_other_participant"
        );
    }

    // ========================================================================
    // Typing Participants
    // ========================================================================

    #[test]
    fn typing_participants_with_expiry() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        let now = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH).unwrap().as_secs();

        // One active, one expired
        let active_handle = interner.intern("npub1active");
        let expired_handle = interner.intern("npub1expired");

        chat.update_typing_participant(active_handle, now + 300);
        chat.update_typing_participant(expired_handle, now - 10);

        let active = chat.get_active_typers(&interner);
        assert_eq!(active.len(), 1, "only the active typer should be returned");
        assert_eq!(active[0], "npub1active", "active typer should be npub1active");
    }

    #[test]
    fn update_typing_participant_refreshes() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1peer".to_string(), &mut interner);

        let now = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH).unwrap().as_secs();

        let handle = interner.intern("npub1typer");
        chat.update_typing_participant(handle, now + 100);
        chat.update_typing_participant(handle, now + 500);

        // Should still be one entry, not duplicated
        let active = chat.get_active_typers(&interner);
        assert_eq!(active.len(), 1, "refreshed typer should not duplicate");
    }

    #[test]
    fn typing_participants_empty_initially() {
        let interner = NpubInterner::new();
        let chat = Chat::new("testchat".to_string(), ChatType::DirectMessage, vec![]);

        let active = chat.get_active_typers(&interner);
        assert!(active.is_empty(), "new chat should have no typers");
    }

    // ========================================================================
    // ChatType
    // ========================================================================

    #[test]
    fn chat_type_i32_roundtrip() {
        assert_eq!(ChatType::from_i32(ChatType::DirectMessage.to_i32()), ChatType::DirectMessage);
        assert_eq!(ChatType::from_i32(ChatType::Community.to_i32()), ChatType::Community);
        assert_eq!(
            ChatType::from_i32(999), ChatType::DirectMessage,
            "unknown i32 should default to DirectMessage"
        );
    }

    // ========================================================================
    // ChatMetadata
    // ========================================================================

    #[test]
    fn chat_metadata_name_and_member_count() {
        let mut meta = ChatMetadata::new();

        assert!(meta.get_name().is_none(), "new metadata should have no name");
        assert!(meta.get_member_count().is_none(), "new metadata should have no member count");

        meta.set_name("My Group".to_string());
        meta.set_member_count(42);

        assert_eq!(meta.get_name(), Some("My Group"), "name should be set");
        assert_eq!(meta.get_member_count(), Some(42), "member count should be set");
    }

    /// The JSON shape the frontend and DB read: a plain string map. A
    /// `BTreeMap`, so its keys come out in the byte order the typed form promises.
    #[derive(Serialize, Deserialize)]
    struct PlainMetadata {
        custom_fields: std::collections::BTreeMap<String, String>,
    }

    fn plain(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn typed(pairs: &[(&str, &str)]) -> ChatMetadata {
        ChatMetadata::from_map(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    fn community_row(cid: &str, primary: &str) -> Vec<(&'static str, String)> {
        vec![
            ("name", "Rust Club".into()),
            ("channel_name", "general".into()),
            ("primary_channel", primary.into()),
            ("description", "".into()),
            ("community_id", cid.into()),
            ("is_owner", "false".into()),
            ("dissolved", "true".into()),
            ("created_at", "1712345678901".into()),
            ("owner_npub", "npub1owner".into()),
            ("icon", "1".into()),
            ("proto_version", "2".into()),
            ("member_count", "12".into()),
        ]
    }

    #[test]
    fn custom_fields_serialize_byte_for_byte_as_a_plain_map() {
        let (cid, primary) = (make_hex_id(0x41), make_hex_id(0x42));
        let full = community_row(&cid, &primary);
        let full: Vec<(&str, &str)> = full.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let rows: [&[(&str, &str)]; 5] = [
            // A DM.
            &[],
            // A Community channel, with keys outside the typed set alongside.
            &full,
            // Known keys holding values no typed slot gives back exactly, beside unknown ones.
            &[
                ("is_owner", "yes"),
                ("dissolved", "TRUE"),
                ("created_at", "0042"),
                ("icon", "true"),
                ("community_id", "Not-Hex!"),
                ("aaa", "first"),
                ("zzz", "last"),
                ("channel_name", ""),
            ],
            // A row stamped before most keys existed.
            &[("community_id", "c0ffee"), ("name", "old")],
            // A DM that was only ever named.
            &[("name", "Bob"), ("member_count", "2")],
        ];
        for pairs in rows {
            let expected = serde_json::to_string(&PlainMetadata { custom_fields: plain(pairs) }).unwrap();
            let meta = typed(pairs);
            let json = serde_json::to_string(&meta).unwrap();
            assert_eq!(json, expected, "the typed row must serialize exactly as the plain map");
            let back: ChatMetadata = serde_json::from_str(&json).unwrap();
            assert_eq!(back, meta, "a round trip rebuilds the same row");
            assert_eq!(serde_json::to_string(&back).unwrap(), expected, "and re-serializes identically");
            for (k, v) in pairs {
                assert_eq!(meta.get(k).as_deref(), Some(*v), "{k} reads back verbatim");
                assert!(meta.contains_key(k));
            }
            assert_eq!(meta.entries().len(), pairs.len());
        }
        assert_eq!(serde_json::to_string(&ChatMetadata::new()).unwrap(), r#"{"custom_fields":{}}"#);
    }

    #[test]
    fn custom_fields_deserialize_like_the_derived_map_did() {
        for json in [
            r#"{"custom_fields":{"name":"x","is_owner":"true"},"unknown":1}"#,
            r#"{"custom_fields":{}}"#,
            r#"{}"#,
            r#"{"custom_fields":{"name":null}}"#,
            r#"{"custom_fields":{"n":5}}"#,
            r#"{"custom_fields":[]}"#,
            r#"[]"#,
        ] {
            let plain = serde_json::from_str::<PlainMetadata>(json);
            let typed = serde_json::from_str::<ChatMetadata>(json);
            assert_eq!(plain.is_ok(), typed.is_ok(), "{json} must be accepted or refused alike");
            if let (Ok(p), Ok(t)) = (plain, typed) {
                assert_eq!(serde_json::to_string(&t).unwrap(), serde_json::to_string(&p).unwrap());
            }
        }
    }

    #[test]
    fn a_community_chat_serializes_its_metadata_unchanged() {
        let mut interner = NpubInterner::new();
        let (cid, primary) = (make_hex_id(0x51), make_hex_id(0x52));
        let mut chat = Chat::new_community_channel(primary.clone(), Vec::new(), &mut interner);
        let row = community_row(&cid, &primary);
        for (k, v) in &row {
            chat.metadata.insert(k, v.clone());
        }
        let pairs: Vec<(&str, &str)> = row.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let expected = serde_json::to_value(PlainMetadata { custom_fields: plain(&pairs) }).unwrap();
        let sent = serde_json::to_value(chat.to_serializable(&interner)).unwrap();
        assert_eq!(sent["metadata"], expected);
        assert!(chat.is_surfaced_community_channel());
        assert_eq!(chat.metadata.community_id(), Some(cid.as_str()));
        assert!(chat.metadata.is_dissolved());
    }

    #[test]
    fn rewriting_a_typed_key_with_an_untyped_value_moves_it_cleanly() {
        let mut meta = typed(&[("community_id", "abc"), ("is_owner", "true")]);
        meta.insert("is_owner", "maybe".into());
        assert_eq!(meta.get("is_owner").as_deref(), Some("maybe"));
        meta.insert("is_owner", "false".into());
        assert_eq!(meta.get("is_owner").as_deref(), Some("false"));
        assert_eq!(meta, typed(&[("community_id", "abc"), ("is_owner", "false")]), "one key, one place");
        meta.remove("community_id");
        meta.remove("is_owner");
        assert_eq!(meta, ChatMetadata::new(), "an emptied row holds nothing");
    }

    // ========================================================================
    // Accessor Methods
    // ========================================================================

    #[test]
    fn accessor_methods_work() {
        let mut interner = NpubInterner::new();
        let mut chat = Chat::new_dm("npub1test".to_string(), &mut interner);
        chat.muted = true;

        assert_eq!(chat.id(), "npub1test");
        assert_eq!(*chat.chat_type(), ChatType::DirectMessage);
        assert_eq!(chat.participants().len(), 1);
        assert_eq!(*chat.last_read(), [0u8; 32]);
        assert!(chat.created_at() > 0);
        assert!(chat.muted());
        assert_eq!(*chat.metadata(), ChatMetadata::new());
    }
}
