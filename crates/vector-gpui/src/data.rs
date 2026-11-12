//! What the UI shows, in vector-core's own wire types, plus the derivations the Tauri
//! frontend makes from them (names, previews, unread marks, time labels). Each helper names
//! the JS function it mirrors, so a change there has one place to land here.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, TimeZone};
use serde::Deserialize;
use vector_core::chat::SerializableChat;
use vector_core::profile::SlimProfile;

#[derive(Deserialize)]
struct Fixtures {
    now: u64,
    npub: String,
    profiles: Vec<SlimProfile>,
    chats: Vec<SerializableChat>,
    #[serde(default)]
    communities: Vec<Community>,
    #[serde(default)]
    invites: Vec<RawInvite>,
    #[serde(default)]
    audit: Option<serde_json::Value>,
    #[serde(default)]
    roles_view: Option<serde_json::Value>,
    #[serde(default)]
    channel_perms: Option<serde_json::Value>,
    #[serde(default)]
    bot_commands: Option<serde_json::Value>,
    #[serde(default)]
    nicknames: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    moderation: Option<serde_json::Value>,
    #[serde(default)]
    policy_presets: Option<serde_json::Value>,
    #[serde(default)]
    embeds: HashMap<String, serde_json::Value>,
    #[serde(default)]
    packs: HashMap<String, vector_core::emoji_packs::EmojiPack>,
    #[serde(default)]
    marketplace: Vec<vector_core::webxdc::MarketplaceApp>,
}

#[derive(Clone, Debug, Default)]
pub struct Art {
    pub icon: Option<String>,
    pub banner: Option<String>,
}

/// `list_community_invites`' row: the bundle still as its JSON.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RawInvite {
    pub community_id: String,
    pub bundle_json: String,
    pub inviter_npub: String,
    pub received_at: i64,
    pub sent_at: i64,
    pub ends_at: i64,
}

/// A Community invite waiting on Accept or Decline.
#[derive(Clone, Debug, Default)]
pub struct Invite {
    pub community_id: String,
    pub name: String,
    pub inviter: String,
    /// The owner's hex key, as the bundle carries it.
    pub owner: Option<String>,
    pub relays: Vec<String>,
    pub received_at: i64,
    pub sent_at: i64,
    pub ends_at: i64,
}

impl From<RawInvite> for Invite {
    fn from(r: RawInvite) -> Self {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Bundle {
            name: String,
            owner: Option<String>,
            relays: Vec<String>,
        }
        let b: Bundle = serde_json::from_str(&r.bundle_json).unwrap_or_default();
        Self {
            community_id: r.community_id,
            name: if b.name.is_empty() {
                "Community".into()
            } else {
                b.name
            },
            inviter: r.inviter_npub,
            owner: b.owner,
            relays: b.relays,
            received_at: r.received_at,
            sent_at: r.sent_at,
            ends_at: r.ends_at,
        }
    }
}

/// One Community as `list_communities` summarises it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Community {
    pub community_id: String,
    pub name: String,
    pub description: String,
    pub is_owner: bool,
    pub has_icon: bool,
    pub channels: Vec<Channel>,
    pub categories: Vec<Category>,
    pub primary_channel: Option<String>,
    pub owner_npub: Option<String>,
    pub dissolved: bool,
    /// Fixtures carry the roster; a live account asks for it on open.
    pub members: Vec<String>,
    pub relays: Vec<String>,
    pub voice_relays: Vec<String>,
    /// Fixtures name local files; a live community's resolve into `Store::art`.
    pub icon: Option<String>,
    pub banner: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Channel {
    pub channel_id: String,
    pub name: String,
    pub private: bool,
    pub readable: bool,
    pub category_id: Option<String>,
    pub topic: Option<String>,
    /// `chat` or `voice`.
    pub view: String,
    pub restricted: bool,
    pub can_post: bool,
}

impl Channel {
    pub fn is_voice(&self) -> bool {
        self.view == "voice"
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Category {
    pub category_id: String,
    pub name: String,
}

impl Community {
    pub fn channel(&self, id: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.channel_id == id)
    }

    /// The channel a Community opens on: its primary, else its first readable text channel.
    pub fn landing(&self) -> Option<&Channel> {
        self.primary_channel
            .as_deref()
            .and_then(|id| self.channel(id))
            .or_else(|| self.channels.iter().find(|c| !c.is_voice() && c.readable))
    }
}

pub struct Store {
    pub my_npub: String,
    pub profiles: HashMap<String, SlimProfile>,
    /// Newest activity first, as the chat list orders them.
    pub chats: Vec<SerializableChat>,
    /// Where relative `avatar_cached` paths resolve.
    pub media_root: PathBuf,
    /// A frozen clock for reproducible renders; `None` follows the wall clock.
    pub frozen_now_ms: Option<u64>,
    /// Unread counts from the database (a live account holds only each chat's last message);
    /// `None` counts from the messages held.
    pub unread: Option<HashMap<String, u32>>,
    /// Request senders a Community we hold counts as a member: their pictures show.
    pub mutuals: std::collections::HashSet<String>,
    pub communities: Vec<Community>,
    pub invites: Vec<Invite>,
    /// The fixtures' Audit Log, as `get_community_audit_log` answers.
    pub fixture_audit: Option<serde_json::Value>,
    pub fixture_roles: Option<serde_json::Value>,
    pub fixture_channel_perms: Option<serde_json::Value>,
    /// Fixtures: each chat's bots and their commands, as `get_chat_commands` answers.
    pub fixture_bot_commands: Option<serde_json::Value>,
    /// `get_moderation_intel` and `policy_presets`, as they answer.
    pub fixture_moderation: Option<serde_json::Value>,
    pub fixture_presets: Option<serde_json::Value>,
    /// `fetch_nostr_embed`'s answers, by reference.
    pub fixture_embeds: HashMap<String, serde_json::Value>,
    /// `fetch_emoji_pack_by_naddr`'s answers.
    pub fixture_packs: HashMap<String, vector_core::emoji_packs::EmojiPack>,
    pub fixture_market: Vec<vector_core::webxdc::MarketplaceApp>,
    /// Each community's nicknames, npub → name.
    pub nicknames: HashMap<String, HashMap<String, String>>,
    /// Each community's logo and banner as local files, once resolved.
    pub art: HashMap<String, Art>,
    pub hidden_banners: Vec<String>,
    pub streamer: Streamer,
}

/// What Streamer Mode shows for a hidden person's name: fixed length, so it says nothing.
pub const STREAM_DOTS: &str = "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}";

/// Streamer Mode, from the account's synced settings.
pub struct Streamer {
    pub on: bool,
    pub hide_wallpapers: bool,
    /// The stream's SipHash key; until a seed is read, one that never leaves memory.
    key: [u8; 16],
}

impl Default for Streamer {
    fn default() -> Self {
        let mut key = [0u8; 16];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut key);
        Self { on: false, hide_wallpapers: true, key }
    }
}

impl Streamer {
    pub fn set(&mut self, view: &vector_core::synced_prefs::StreamerSettings) {
        self.on = view.on;
        self.hide_wallpapers = view.hide_wallpapers;
        let digit = |b: u8| (b as char).to_digit(16);
        let bytes = view.seed.as_bytes();
        if bytes.len() == 32 && bytes.iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b)) {
            for (i, pair) in bytes.chunks(2).enumerate() {
                self.key[i] = (digit(pair[0]).unwrap() * 16 + digit(pair[1]).unwrap()) as u8;
            }
        }
    }

    /// The npub's 64-bit tag under this stream's key: tints take the low half, codes the high.
    fn tag(&self, npub: &str) -> u64 {
        #[allow(deprecated)]
        let mut h = std::hash::SipHasher::new_with_keys(
            u64::from_le_bytes(self.key[..8].try_into().unwrap()),
            u64::from_le_bytes(self.key[8..].try_into().unwrap()),
        );
        std::hash::Hasher::write(&mut h, npub.as_bytes());
        std::hash::Hasher::finish(&h)
    }

    /// A hidden person's avatar: a solid circle whose hue is all that tells them apart.
    pub fn tint(&self, npub: &str) -> PathBuf {
        let hue = (self.tag(npub) & 0xffff_ffff) % 360;
        let dir = std::env::temp_dir().join("vector-stream-tints-v1");
        let path = dir.join(format!("{hue}.svg"));
        if !path.exists() {
            let (r, g, b) = hsl(hue as f32, 0.48, 0.46);
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(
                &path,
                format!(r##"<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256"><circle cx="128" cy="128" r="128" fill="#{r:02x}{g:02x}{b:02x}"/></svg>"##),
            );
        }
        path
    }

    /// A short per-stream label for a hidden person: `len` hex digits of the tag's high half.
    pub fn code(&self, npub: &str, len: usize) -> String {
        format!("{:08x}", self.tag(npub) >> 32)[..len.min(8)].to_string()
    }
}

fn hsl(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let a = s * l.min(1. - l);
    let f = |n: f32| {
        let k = (n + h / 30.) % 12.;
        let v = l - a * (k - 3.).min(9. - k).clamp(-1., 1.);
        (v * 255.).round() as u8
    };
    (f(0.), f(8.), f(4.))
}

impl Store {
    pub fn from_fixtures(path: &Path) -> anyhow::Result<Self> {
        let f: Fixtures = serde_json::from_slice(&std::fs::read(path)?)?;
        let mut chats = f.chats;
        // VECTOR_REQUESTS=<n>: the last n chats lose our side, so they wait under Requests.
        let requests = std::env::var("VECTOR_REQUESTS")
            .ok()
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(0);
        let from = chats.len().saturating_sub(requests);
        for chat in &mut chats[from..] {
            chat.messages.retain(|m| !m.mine);
        }
        chats.sort_by_key(|c| std::cmp::Reverse(last_activity(c)));
        Ok(Self {
            my_npub: f.npub,
            profiles: f.profiles.into_iter().map(|p| (p.id.clone(), p)).collect(),
            chats,
            media_root: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            frozen_now_ms: Some(f.now),
            unread: None,
            mutuals: Default::default(),
            communities: f.communities,
            invites: f.invites.into_iter().map(Invite::from).collect(),
            fixture_audit: f.audit,
            fixture_roles: f.roles_view,
            fixture_channel_perms: f.channel_perms,
            fixture_bot_commands: f.bot_commands,
            fixture_moderation: f.moderation,
            fixture_presets: f.policy_presets,
            fixture_embeds: f.embeds,
            fixture_packs: f.packs,
            fixture_market: f.marketplace,
            nicknames: f.nicknames,
            art: HashMap::new(),
            hidden_banners: Vec::new(),
            // Fixtures: VECTOR_STREAMER=1 goes live under the mock's seed.
            streamer: {
                let mut st = Streamer::default();
                if std::env::var_os("VECTOR_STREAMER").is_some() {
                    st.set(&vector_core::synced_prefs::StreamerSettings {
                        on: true,
                        seed: "00112233445566778899aabbccddeeff".into(),
                        ..Default::default()
                    });
                }
                st
            },
        })
    }

    /// A live account's store, filled as its events arrive.
    pub fn live() -> Self {
        Self {
            my_npub: String::new(),
            profiles: HashMap::new(),
            chats: Vec::new(),
            media_root: PathBuf::from("/"),
            frozen_now_ms: None,
            unread: Some(HashMap::new()),
            mutuals: Default::default(),
            communities: Vec::new(),
            invites: Vec::new(),
            fixture_audit: None,
            fixture_roles: None,
            fixture_channel_perms: None,
            fixture_bot_commands: None,
            fixture_moderation: None,
            fixture_presets: None,
            fixture_embeds: HashMap::new(),
            fixture_packs: HashMap::new(),
            fixture_market: Vec::new(),
            nicknames: HashMap::new(),
            art: HashMap::new(),
            hidden_banners: Vec::new(),
            streamer: Default::default(),
        }
    }

    pub fn unread_count(&self, chat: &SerializableChat) -> usize {
        match &self.unread {
            Some(counts) => counts.get(&chat.id).copied().unwrap_or(0) as usize,
            None => unread_count(chat),
        }
    }

    /// A message arrived or was sent: appended, or put in place by time.
    pub fn insert_message(&mut self, chat_id: &str, msg: vector_core::types::Message) -> bool {
        let Some(chat) = self.chat_mut(chat_id) else {
            return false;
        };
        if chat.messages.iter().any(|m| m.id == msg.id) {
            return true;
        }
        let at = chat.messages.partition_point(|m| m.at <= msg.at);
        chat.messages.insert(at, msg);
        self.resort();
        true
    }

    /// A message changed (sent, failed, edited, reacted to): `old_id` is the one it replaces.
    pub fn replace_message(
        &mut self,
        chat_id: &str,
        old_id: &str,
        msg: vector_core::types::Message,
    ) {
        let Some(chat) = self.chat_mut(chat_id) else {
            return;
        };
        match chat
            .messages
            .iter()
            .position(|m| m.id == old_id || m.id == msg.id)
        {
            Some(i) => chat.messages[i] = msg,
            None => {
                let at = chat.messages.partition_point(|m| m.at <= msg.at);
                chat.messages.insert(at, msg);
            }
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.frozen_now_ms
            .unwrap_or_else(|| Local::now().timestamp_millis() as u64)
    }

    pub fn chat(&self, id: &str) -> Option<&SerializableChat> {
        self.chats.iter().find(|c| c.id == id)
    }

    /// A DM opens before its first message: an empty chat until one lands.
    pub fn ensure_dm(&mut self, npub: &str) {
        if self.chat(npub).is_none() {
            let mut interner = vector_core::compact::NpubInterner::new();
            let chat = vector_core::chat::Chat::new_dm(npub.to_string(), &mut interner);
            self.chats.push(chat.to_serializable(&interner));
            self.resort();
        }
    }

    pub fn chat_mut(&mut self, id: &str) -> Option<&mut SerializableChat> {
        self.chats.iter_mut().find(|c| c.id == id)
    }

    pub fn resort(&mut self) {
        self.chats
            .sort_by_key(|c| std::cmp::Reverse(last_activity(c)));
    }

    pub fn me(&self) -> Option<&SlimProfile> {
        self.profiles.get(&self.my_npub)
    }

    /// `getName` (main.js).
    pub fn name(&self, npub: &str) -> String {
        self.name_in(npub, None)
    }

    /// `streamHidden`: while streaming, anyone but this account who hasn't allowed streams,
    /// a profile not fetched yet included.
    pub fn stream_hidden(&self, npub: &str) -> bool {
        self.streamer.on
            && npub.starts_with("npub1")
            && npub != self.my_npub
            && !self.profiles.get(npub).is_some_and(|p| p.mine || p.stream_consent)
    }

    /// `streamMaskNpubs`: `text` with each hidden person's npub or nprofile as dots, or
    /// `None` when nothing in it is hidden.
    pub fn stream_mask(&self, text: &str) -> Option<String> {
        if !self.streamer.on {
            return None;
        }
        let spans = find_bech_people(text);
        let mut out = String::new();
        let mut last = 0;
        for (start, end, npub) in spans {
            if npub.as_deref().is_some_and(|n| !self.stream_hidden(n)) {
                continue;
            }
            out.push_str(&text[last..start]);
            out.push_str(STREAM_DOTS);
            last = end;
        }
        (last > 0).then(|| {
            out.push_str(&text[last..]);
            out
        })
    }

    /// `resolveMentionText`: tagged npubs as `@name`, for previews and notifications.
    pub fn resolve_mentions(&self, text: &str, community: Option<&str>) -> String {
        let spots = crate::markdown::find_npubs_where(text, |c| {
            c.is_ascii_alphanumeric() || "_/=?&#%.-".contains(c)
        });
        if spots.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for (start, end, npub) in spots {
            if !self.stream_hidden(npub) && !self.profiles.contains_key(npub) {
                continue;
            }
            out.push_str(&text[last..start]);
            out.push('@');
            out.push_str(&self.name_in(npub, community));
            last = end;
        }
        out.push_str(&text[last..]);
        out
    }

    /// `getName` with a community: its nickname ranks under your own, over their name.
    pub fn name_in(&self, npub: &str, community: Option<&str>) -> String {
        if self.stream_hidden(npub) {
            return STREAM_DOTS.into();
        }
        let nick = community
            .and_then(|c| self.nicknames.get(c))
            .and_then(|n| n.get(npub))
            .filter(|n| !n.is_empty());
        match self.profiles.get(npub) {
            Some(p) if !p.nickname.is_empty() => p.nickname.clone(),
            _ if nick.is_some() => nick.cloned().unwrap_or_default(),
            Some(p) if !p.name.is_empty() => p.name.clone(),
            Some(p) if !p.display_name.is_empty() => p.display_name.clone(),
            _ => format!("{}…", &npub[..npub.len().min(12)]),
        }
    }

    /// A DM neither answered nor accepted: it waits under Requests, its media unloaded.
    pub fn is_request(&self, chat: &SerializableChat) -> bool {
        chat.chat_type == vector_core::ChatType::DirectMessage
            && chat.id != self.my_npub
            && !chat.replied
            && !chat.metadata.contains_key(vector_core::chat::ACCEPTED_KEY)
            && !chat.messages.is_empty()
            && !chat.messages.iter().any(|m| m.mine)
    }

    /// Whether the chat gets a row: a DM once it has content and its sender isn't blocked.
    pub fn listed(&self, chat: &SerializableChat) -> bool {
        chat.chat_type == vector_core::ChatType::DirectMessage
            && !chat.messages.is_empty()
            && !self.profiles.get(&chat.id).is_some_and(|p| p.is_blocked)
    }

    pub fn icon(&self, id: &str) -> Option<PathBuf> {
        let p = self
            .art
            .get(id)
            .and_then(|a| a.icon.as_deref())
            .or_else(|| self.community(id).and_then(|c| c.icon.as_deref()))?;
        Some(self.media_root.join(p))
    }

    /// The banner the community pane shows: none once hidden.
    pub fn banner(&self, id: &str) -> Option<PathBuf> {
        let p = self
            .art
            .get(id)
            .and_then(|a| a.banner.as_deref())
            .or_else(|| self.community(id).and_then(|c| c.banner.as_deref()))?;
        Some(self.media_root.join(p))
    }

    pub fn community(&self, id: &str) -> Option<&Community> {
        self.communities.iter().find(|c| c.community_id == id)
    }

    /// The Community a channel chat belongs to.
    /// A name as `chat` shows it: a community channel uses that community's nicknames.
    pub fn chat_name(&self, npub: &str, chat: &str) -> String {
        self.name_in(
            npub,
            self.community_of(chat).map(|c| c.community_id.as_str()),
        )
    }

    pub fn community_of(&self, channel_id: &str) -> Option<&Community> {
        self.communities
            .iter()
            .find(|c| c.channel(channel_id).is_some())
    }

    /// A stranger whose request waits shows no pictures until it's accepted.
    pub fn pictures_held(&self, npub: &str) -> bool {
        self.chat(npub).is_some_and(|c| self.is_request(c)) && !self.mutuals.contains(npub)
    }

    pub fn avatar_path(&self, npub: &str) -> Option<PathBuf> {
        if self.stream_hidden(npub) {
            return Some(self.streamer.tint(npub));
        }
        if self.pictures_held(npub) {
            return None;
        }
        let p = self.profiles.get(npub)?;
        (!p.avatar_cached.is_empty()).then(|| self.media_root.join(&p.avatar_cached))
    }
}

pub fn last_activity(chat: &SerializableChat) -> u64 {
    chat.messages
        .iter()
        .map(|m| m.at)
        .max()
        .unwrap_or(chat.created_at * 1000)
}

/// Messages after `last_read` that someone else sent: `computeListRowUnreadCount`.
pub fn unread_count(chat: &SerializableChat) -> usize {
    let Some(i) = chat.messages.iter().position(|m| m.id == chat.last_read) else {
        return chat.messages.iter().filter(|m| !m.mine).count();
    };
    chat.messages[i + 1..].iter().filter(|m| !m.mine).count()
}

/// The preview line: `chatPreviewText` (render/chatlist/preview.js).
pub fn preview_text(chat: &SerializableChat) -> String {
    let Some(m) = chat.messages.last() else {
        return String::new();
    };
    let body = if m.content.is_empty() && !m.attachments.is_empty() {
        "Sent a file".to_string()
    } else {
        crate::newchat::replace_invite_links(&m.content, "Community Invite")
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    };
    if m.mine { format!("You: {body}") } else { body }
}

/// `timeAgo` (misc.js).
pub fn time_ago(now_ms: u64, at_ms: u64) -> String {
    let s = now_ms.saturating_sub(at_ms) / 1000;
    match s {
        0..60 => "Now".into(),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        86_400..604_800 => format!("{}d", s / 86_400),
        604_800..2_592_000 => format!("{}w", s / 604_800),
        2_592_000..31_536_000 => format!("{}mo", s / 2_592_000),
        _ => format!("{}y", s / 31_536_000),
    }
}

pub fn local(ms: u64) -> DateTime<Local> {
    Local
        .timestamp_millis_opt(ms as i64)
        .single()
        .unwrap_or_else(Local::now)
}

/// A message's clock time, as `toLocaleTimeString([], {hour: 'numeric', minute: '2-digit'})`.
pub fn clock(ms: u64) -> String {
    local(ms).format("%-I:%M %p").to_string()
}

/// The date divider's bold half: "Today", "Yesterday", a weekday or a date.
pub fn day_label(now_ms: u64, ms: u64) -> String {
    let (now, then) = (local(now_ms).date_naive(), local(ms).date_naive());
    match (now - then).num_days() {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => then.format("%-m/%-d/%Y").to_string(),
    }
}

/// One row of the message timeline; `Message` indexes the chat's `messages`.
pub enum Row {
    Date {
        label: String,
        time: String,
    },
    Unread,
    /// `first` heads a streak (avatar and header); otherwise a continuation.
    Message {
        index: usize,
        first: bool,
    },
}

const STREAK_WINDOW_MS: u64 = 7 * 60 * 1000;

/// `shouldCollapseStreak` (message-streak.js): same sender within 7 minutes, never a reply.
pub fn collapses(prev: &vector_core::types::Message, cur: &vector_core::types::Message) -> bool {
    if !cur.replied_to.is_empty() || prev.at == 0 || cur.at == 0 {
        return false;
    }
    if cur.at < prev.at || cur.at - prev.at >= STREAK_WINDOW_MS {
        return false;
    }
    match (prev.mine, cur.mine) {
        (true, true) => true,
        (false, false) => match (&prev.npub, &cur.npub) {
            (Some(a), Some(b)) => a == b,
            (None, None) => true,
            _ => false,
        },
        _ => false,
    }
}

/// Lays a chat's messages out as the Tauri timeline does: a date divider before the first
/// message of each day, and the NEW divider after the last read message.
/// `read_anchor` is `last_read` as it stood when the chat opened: the divider stays put while
/// the chat is on screen, even as reading moves `last_read` on.
pub fn timeline(
    chat: &SerializableChat,
    now_ms: u64,
    read_anchor: Option<&str>,
    commands: Option<&HashSet<String>>,
) -> Vec<Row> {
    let mut rows = Vec::with_capacity(chat.messages.len() + 2);
    let mut last_day = None;
    let read_at = read_anchor.and_then(|a| chat.messages.iter().position(|m| m.id == a));
    for (i, m) in chat.messages.iter().enumerate() {
        let day = local(m.at).date_naive();
        if last_day != Some(day) {
            rows.push(Row::Date {
                label: day_label(now_ms, m.at),
                time: clock(m.at),
            });
            last_day = Some(day);
        }
        if read_at.is_some_and(|r| i == r + 1) && !m.mine {
            rows.push(Row::Unread);
        }
        // As `deriveWindow`: a day divider doesn't end a streak.
        let first = (i == 0 || !collapses(&chat.messages[i - 1], m)) && command(m, commands).is_none();
        rows.push(Row::Message { index: i, first });
    }
    rows
}

/// A slash command worth the passive line: tagged to a bot, bare, or one a bot here declares.
/// An untagged `/word plus prose` nobody declares stays text, so no real content hides.
pub fn command(m: &vector_core::Message, known: Option<&HashSet<String>>) -> Option<(String, Option<String>)> {
    let content = m.content.trim();
    let rest = content.strip_prefix('/')?;
    let len = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = &rest[..len];
    if !(1..=32).contains(&name.len()) || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-') {
        return None;
    }
    let bot = m.addressed_bots.first().cloned();
    if bot.is_none() && len != rest.len() && !known.is_some_and(|k| k.contains(name)) {
        return None;
    }
    Some((name.to_string(), bot))
}

/// `getFileTypeInfo` (misc.js): an extension's icon and description.
pub fn file_type(extension: &str) -> (&'static str, &'static str) {
    match extension.to_ascii_lowercase().as_str() {
        "png" => ("image", "Picture"),
        "jpg" => ("image", "Picture"),
        "jpeg" => ("image", "Picture"),
        "gif" => ("film", "GIF Animation"),
        "webp" => ("image", "Picture"),
        "svg" => ("image", "Vector Image"),
        "bmp" => ("image", "Bitmap Image"),
        "ico" => ("image", "Icon"),
        "tiff" => ("image", "TIFF Image"),
        "tif" => ("image", "TIFF Image"),
        "raw" => ("image", "RAW Image"),
        "dng" => ("image", "RAW Image"),
        "cr2" => ("image", "Canon RAW"),
        "nef" => ("image", "Nikon RAW"),
        "arw" => ("image", "Sony RAW"),
        "orf" => ("image", "Olympus RAW"),
        "rw2" => ("image", "Panasonic RAW"),
        "wav" => ("mic-on", "Voice Message"),
        "mp3" => ("mic-on", "Audio Clip"),
        "m4a" => ("mic-on", "Audio Clip"),
        "aac" => ("mic-on", "Audio Clip"),
        "flac" => ("mic-on", "Audio Clip"),
        "ogg" => ("mic-on", "Audio Clip"),
        "wma" => ("mic-on", "Audio Clip"),
        "opus" => ("mic-on", "Audio Clip"),
        "ape" => ("mic-on", "Audio Clip"),
        "wv" => ("mic-on", "Audio Clip"),
        "aup" => ("mic-on", "Audacity Project"),
        "flp" => ("mic-on", "FL Studio Project"),
        "als" => ("mic-on", "Ableton Project"),
        "logic" => ("mic-on", "Logic Project"),
        "band" => ("mic-on", "GarageBand Project"),
        "mp4" => ("film", "Video"),
        "webm" => ("film", "Video"),
        "mov" => ("film", "Video"),
        "avi" => ("film", "Video"),
        "mkv" => ("film", "Video"),
        "flv" => ("film", "Flash Video"),
        "wmv" => ("film", "Windows Video"),
        "mpg" => ("film", "MPEG Video"),
        "mpeg" => ("film", "MPEG Video"),
        "m4v" => ("film", "MPEG-4 Video"),
        "3gp" => ("film", "3GP Video"),
        "3g2" => ("film", "3G2 Video"),
        "f4v" => ("film", "Flash MP4 Video"),
        "asf" => ("film", "Advanced Systems Format"),
        "rm" => ("film", "RealMedia"),
        "vob" => ("film", "DVD Video"),
        "ogv" => ("film", "Ogg Video"),
        "mxf" => ("film", "Material Exchange Format"),
        "m2ts" => ("film", "Blu-ray Video"),
        "pdf" => ("file", "PDF Document"),
        "doc" => ("file", "Word Document"),
        "docx" => ("file", "Word Document"),
        "xls" => ("file", "Excel Spreadsheet"),
        "xlsx" => ("file", "Excel Spreadsheet"),
        "ppt" => ("file", "PowerPoint Presentation"),
        "pptx" => ("file", "PowerPoint Presentation"),
        "odt" => ("file", "OpenDocument Text"),
        "ods" => ("file", "OpenDocument Spreadsheet"),
        "odp" => ("file", "OpenDocument Presentation"),
        "rtf" => ("file", "Rich Text Document"),
        "tex" => ("file", "LaTeX Document"),
        "pages" => ("file", "Pages Document"),
        "numbers" => ("file", "Numbers Spreadsheet"),
        "key" => ("file", "Keynote Presentation"),
        "txt" => ("file", "Text File"),
        "md" => ("file", "Markdown File"),
        "log" => ("file", "Log File"),
        "csv" => ("file", "CSV File"),
        "tsv" => ("file", "TSV File"),
        "json" => ("file", "JSON File"),
        "xml" => ("file", "XML File"),
        "yaml" => ("file", "YAML File"),
        "yml" => ("file", "YAML File"),
        "toml" => ("file", "TOML File"),
        "sql" => ("file", "SQL File"),
        "db" => ("file", "Database File"),
        "sqlite" => ("file", "SQLite Database"),
        "zip" => ("folder", "ZIP Archive"),
        "rar" => ("folder", "RAR Archive"),
        "7z" => ("folder", "7-Zip Archive"),
        "tar" => ("folder", "TAR Archive"),
        "gz" => ("folder", "GZip Archive"),
        "bz2" => ("folder", "BZip2 Archive"),
        "xz" => ("folder", "XZ Archive"),
        "tgz" => ("folder", "Compressed TAR"),
        "tbz" => ("folder", "Compressed TAR"),
        "txz" => ("folder", "Compressed TAR"),
        "cab" => ("folder", "Cabinet Archive"),
        "iso" => ("file", "Disc Image"),
        "dmg" => ("file", "macOS Disk Image"),
        "pkg" => ("file", "Package File"),
        "deb" => ("file", "Debian Package"),
        "rpm" => ("file", "RPM Package"),
        "apk" => ("file", "Android Package"),
        "ipa" => ("file", "iOS App"),
        "jar" => ("file", "Java Archive"),
        "war" => ("file", "Web Archive"),
        "ear" => ("file", "Enterprise Archive"),
        "obj" => ("file", "3D Object"),
        "fbx" => ("file", "Autodesk FBX"),
        "gltf" => ("file", "GL Transmission Format"),
        "glb" => ("file", "GL Binary"),
        "stl" => ("file", "Stereolithography"),
        "ply" => ("file", "Polygon File"),
        "dae" => ("file", "COLLADA"),
        "3ds" => ("file", "3D Studio"),
        "blend" => ("file", "Blender File"),
        "c4d" => ("file", "Cinema 4D"),
        "max" => ("file", "3ds Max"),
        "ma" => ("file", "Maya ASCII"),
        "mb" => ("file", "Maya Binary"),
        "usdz" => ("file", "Universal Scene"),
        "dwg" => ("file", "AutoCAD Drawing"),
        "dxf" => ("file", "Drawing Exchange"),
        "step" => ("file", "STEP CAD"),
        "stp" => ("file", "STEP CAD"),
        "iges" => ("file", "IGES CAD"),
        "igs" => ("file", "IGES CAD"),
        "sat" => ("file", "ACIS SAT"),
        "ipt" => ("file", "Inventor Part"),
        "iam" => ("file", "Inventor Assembly"),
        "prt" => ("file", "Part File"),
        "sldprt" => ("file", "SolidWorks Part"),
        "sldasm" => ("file", "SolidWorks Assembly"),
        "slddrw" => ("file", "SolidWorks Drawing"),
        "catpart" => ("file", "CATIA Part"),
        "catproduct" => ("file", "CATIA Product"),
        "js" => ("file", "JavaScript"),
        "ts" => ("file", "TypeScript"),
        "jsx" => ("file", "React JSX"),
        "tsx" => ("file", "React TSX"),
        "py" => ("file", "Python"),
        "rs" => ("file", "Rust"),
        "go" => ("file", "Go"),
        "java" => ("file", "Java"),
        "kt" => ("file", "Kotlin"),
        "cpp" => ("file", "C++"),
        "cc" => ("file", "C++"),
        "cxx" => ("file", "C++"),
        "c" => ("file", "C"),
        "h" => ("file", "Header File"),
        "hpp" => ("file", "C++ Header"),
        "cs" => ("file", "C#"),
        "rb" => ("file", "Ruby"),
        "php" => ("file", "PHP"),
        "swift" => ("file", "Swift"),
        "m" => ("file", "Objective-C"),
        "mm" => ("file", "Objective-C++"),
        "lua" => ("file", "Lua"),
        "r" => ("file", "R Script"),
        "scala" => ("file", "Scala"),
        "clj" => ("file", "Clojure"),
        "dart" => ("file", "Dart"),
        "ex" => ("file", "Elixir"),
        "elm" => ("file", "Elm"),
        "erl" => ("file", "Erlang"),
        "fs" => ("file", "F#"),
        "hs" => ("file", "Haskell"),
        "jl" => ("file", "Julia"),
        "nim" => ("file", "Nim"),
        "pl" => ("file", "Perl"),
        "sh" => ("file", "Shell Script"),
        "bash" => ("file", "Bash Script"),
        "zsh" => ("file", "Zsh Script"),
        "fish" => ("file", "Fish Script"),
        "ps1" => ("file", "PowerShell"),
        "bat" => ("file", "Batch File"),
        "cmd" => ("file", "Command File"),
        "vb" => ("file", "Visual Basic"),
        "vbs" => ("file", "VBScript"),
        "asm" => ("file", "Assembly"),
        "s" => ("file", "Assembly"),
        "ini" => ("file", "INI Config"),
        "cfg" => ("file", "Config File"),
        "conf" => ("file", "Config File"),
        "config" => ("file", "Config File"),
        "env" => ("file", "Environment File"),
        "properties" => ("file", "Properties File"),
        "plist" => ("file", "Property List"),
        "gitignore" => ("file", "Git Ignore"),
        "dockerignore" => ("file", "Docker Ignore"),
        "editorconfig" => ("file", "Editor Config"),
        "eslintrc" => ("file", "ESLint Config"),
        "prettierrc" => ("file", "Prettier Config"),
        "html" => ("file", "HTML File"),
        "htm" => ("file", "HTML File"),
        "css" => ("file", "CSS Stylesheet"),
        "scss" => ("file", "SCSS Stylesheet"),
        "sass" => ("file", "Sass Stylesheet"),
        "less" => ("file", "Less Stylesheet"),
        "vue" => ("file", "Vue Component"),
        "svelte" => ("file", "Svelte Component"),
        "eps" => ("file", "Encapsulated PostScript"),
        "ai" => ("file", "Adobe Illustrator"),
        "sketch" => ("file", "Sketch File"),
        "fig" => ("file", "Figma File"),
        "xd" => ("file", "Adobe XD"),
        "exe" => ("file", "Executable"),
        "msi" => ("file", "Windows Installer"),
        "app" => ("file", "macOS Application"),
        "ttf" => ("file", "TrueType Font"),
        "otf" => ("file", "OpenType Font"),
        "woff" => ("file", "Web Font"),
        "woff2" => ("file", "Web Font 2"),
        "eot" => ("file", "Embedded OpenType"),
        "ics" => ("file", "Calendar File"),
        "vcf" => ("file", "vCard Contact"),
        "torrent" => ("file", "Torrent File"),
        "xdc" => ("gift", "Mini App"),
        _ => ("file-unknown", "Unknown File"),
    }
}

/// Each `npub1…` or `nprofile1…` in `text`, with any `@` or `nostr:` before it, and the
/// npub it names (`None` for an nprofile that doesn't decode).
pub fn find_bech_people(text: &str) -> Vec<(usize, usize, Option<String>)> {
    use nostr_sdk::prelude::{FromBech32, Nip19Profile, ToBech32};
    let lower = text.to_ascii_lowercase();
    let b = lower.as_bytes();
    let body = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let (hrp, profile) = if lower[i..].starts_with("npub1") {
            (5, false)
        } else if lower[i..].starts_with("nprofile1") {
            (9, true)
        } else {
            i += 1;
            continue;
        };
        let len = b[i + hrp..].iter().take_while(|&&c| body(c)).count();
        let end = i + hrp + len;
        let npub = if profile {
            (len > 0).then_some(()).and_then(|_| Nip19Profile::from_bech32(&lower[i..end]).ok())
                .and_then(|p| p.public_key.to_bech32().ok())
        } else if len >= 58 {
            Some(lower[i..i + 63].to_string())
        } else {
            i = end.max(i + 1);
            continue;
        };
        let end = if profile { end } else { i + 63 };
        let start = if lower[..i].ends_with("nostr:") {
            i - 6
        } else if lower[..i].ends_with('@') {
            i - 1
        } else {
            i
        };
        out.push((start, end, npub));
        i = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bech_people_with_prefixes_and_nprofiles() {
        use nostr_sdk::prelude::{FromBech32, Nip19Profile, PublicKey, ToBech32};
        let npub = "npub1v84qsqlcs56j8dmh6s22eccnpn2d87fdutxhl7rftseh67wzamhqfv9ft6";
        let pk = PublicKey::from_bech32(npub).unwrap();
        let nprofile = Nip19Profile::new(pk, [nostr_sdk::prelude::RelayUrl::parse("wss://relay.example.com").unwrap()]).to_bech32().unwrap();
        let text = format!("hi nostr:{npub} and https://x.io/{nprofile}?a=1 @npub1short");
        let found = find_bech_people(&text);
        assert_eq!(found.len(), 2);
        assert_eq!(&text[found[0].0..found[0].1], format!("nostr:{npub}"));
        assert_eq!(found[1].2.as_deref(), Some(npub));
        assert_eq!(&text[found[1].0..found[1].1], nprofile);
    }
}
