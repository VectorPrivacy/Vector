//! What the UI shows, in vector-core's own wire types, plus the derivations the Tauri
//! frontend makes from them (names, previews, unread marks, time labels). Each helper names
//! the JS function it mirrors, so a change there has one place to land here.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, TimeZone};
use serde::Deserialize;
use vector_core::chat::SerializableChat;
use vector_core::profile::SlimProfile;

use crate::t;

#[derive(Deserialize, Default)]
struct ChatlistDemo {
    pinned: Vec<String>,
    archived: HashMap<String, u64>,
}

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
    sync_report: Option<serde_json::Value>,
    #[serde(default)]
    roles_view: Option<serde_json::Value>,
    #[serde(default)]
    channel_perms: Option<serde_json::Value>,
    #[serde(default)]
    bot_commands: Option<serde_json::Value>,
    #[serde(default)]
    nicknames: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    timeouts: HashMap<String, HashMap<String, Timeout>>,
    #[serde(default)]
    send_denied: HashMap<String, HashSet<String>>,
    #[serde(default)]
    react_denied: HashMap<String, HashSet<String>>,
    #[serde(default)]
    unlisted: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    role_graphs: HashMap<String, serde_json::Value>,
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
    #[serde(default)]
    pivx: Option<serde_json::Value>,
    /// Pins and archived chats, which VECTOR_CHATLIST_DEMO=1 applies.
    #[serde(default)]
    chatlist_demo: Option<ChatlistDemo>,
    /// More communities for the rail, which VECTOR_RAIL_DEMO=1 adds.
    #[serde(default)]
    rail_demo: Vec<Community>,
    /// Who is in each voice call, and ours: VECTOR_VOICE=present|joined|held|video picks one.
    #[serde(default)]
    voice_demo: Option<serde_json::Value>,
    /// Each channel's pins, which VECTOR_PINS=1 applies.
    #[serde(default)]
    pins_demo: HashMap<String, serde_json::Value>,
    #[serde(default)]
    server_demo: HashMap<String, serde_json::Value>,
    /// Transport states by name, which VECTOR_NET=<name> shows; `i2p_config` is I2P's settings.
    #[serde(default)]
    net_demo: serde_json::Value,
    /// NIP-11 names, icons and AUTH by relay URL, as `get_relay_info` answers.
    #[serde(default)]
    relay_info: HashMap<String, serde_json::Value>,
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
                t::community_unnamed().into()
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
    /// The chat channel Join/Leave lines go to; `None` while they're off.
    pub system_channel: Option<String>,
    /// The channel new members open first; `None` for the default.
    pub landing_channel: Option<String>,
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
    #[serde(default = "yes")]
    pub readable: bool,
    pub category_id: Option<String>,
    pub topic: Option<String>,
    /// `chat` or `voice`.
    pub view: String,
    pub restricted: bool,
    pub can_post: bool,
    pub my_permissions: Option<String>,
    /// When a Private Channel stopped being public, in ms.
    pub public_until: Option<u64>,
}

/// Why a post collapses to a one-line placeholder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hidden {
    Blocked,
    Unauthorized,
    NoSend,
    TimedOut,
}

impl Hidden {
    pub fn label(self) -> gpui::SharedString {
        match self {
            Hidden::Blocked => t::hidden_blocked(),
            Hidden::Unauthorized => t::hidden_unauthorized(),
            Hidden::NoSend => t::hidden_no_send(),
            Hidden::TimedOut => t::hidden_timed_out(),
        }
    }
}

/// A running Timeout, in unix seconds.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Timeout {
    pub from: u64,
    pub until: u64,
}

fn yes() -> bool {
    true
}

impl Channel {
    pub fn is_voice(&self) -> bool {
        self.view == "voice"
    }

    /// Listed: a private channel only with its current key, and only with View.
    pub fn visible(&self) -> bool {
        const VIEW_CHANNEL: u64 = 1 << 13;
        !(self.private && !self.readable)
            && self.my_permissions.as_deref().and_then(|p| p.parse::<u64>().ok()).is_none_or(|p| p & VIEW_CHANNEL != 0)
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
        self.landing_channel
            .as_deref()
            .and_then(|id| self.channel(id))
            .filter(|c| c.visible())
            .or_else(|| self.primary_channel.as_deref().and_then(|id| self.channel(id)))
            .or_else(|| self.channels.iter().find(|c| !c.is_voice() && c.visible()))
    }
}

pub type Roster = (Vec<String>, Vec<String>, HashMap<String, String>);

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
    /// The fixtures' Sync Report reads: `{ control, epoch }`.
    pub fixture_sync_report: Option<serde_json::Value>,
    pub fixture_roles: Option<serde_json::Value>,
    pub fixture_channel_perms: Option<serde_json::Value>,
    /// Fixtures: each chat's bots and their commands, as `get_chat_commands` answers.
    pub fixture_bot_commands: Option<serde_json::Value>,
    /// `get_moderation_intel` and `policy_presets`, as they answer.
    pub fixture_moderation: Option<serde_json::Value>,
    /// Fixtures: `{ present: { channel: [member] }, session }` under VECTOR_VOICE.
    pub fixture_voice: Option<serde_json::Value>,
    pub fixture_pins: HashMap<String, serde_json::Value>,
    /// Media servers' dialogs, by URL: `info`, `stats`, `status` and `caps`.
    pub fixture_servers: HashMap<String, serde_json::Value>,
    /// The transport state VECTOR_NET picks, and I2P's settings.
    pub fixture_net: Option<serde_json::Value>,
    pub fixture_i2p: Option<serde_json::Value>,
    pub fixture_presets: Option<serde_json::Value>,
    /// `fetch_nostr_embed`'s answers, by reference.
    pub fixture_embeds: HashMap<String, serde_json::Value>,
    /// Who is typing, by chat: their npubs and when the list last changed (ms).
    pub typing: HashMap<String, (Vec<String>, u64)>,
    /// Each chat's system lines (joins, leaves, wallpaper and Pins changes), oldest first.
    pub system: HashMap<String, Vec<SysLine>>,
    /// Whether this account sees which invite link brought each member, by community: holding
    /// Create Invites. Absent until asked.
    pub invite_sight: HashMap<String, bool>,
    /// `fetch_emoji_pack_by_naddr`'s answers.
    pub fixture_packs: HashMap<String, vector_core::emoji_packs::EmojiPack>,
    pub fixture_market: Vec<vector_core::webxdc::MarketplaceApp>,
    /// The wallet's balance, price, promos and chat payments, as the mock answers them.
    pub fixture_pivx: Option<serde_json::Value>,
    /// PIVX payments shown in chats, by message id.
    pub pivx: HashMap<String, PivxPay>,
    /// Each payment bubble's claim state, by gift code.
    pub pivx_bubbles: HashMap<String, (PivxPhase, String)>,
    /// The PIVX price and its currency, once fetched.
    pub pivx_price: Option<(f64, String)>,
    /// Each community's nicknames, npub → name.
    pub nicknames: HashMap<String, HashMap<String, String>>,
    /// Each community's last roster read: members, admins, unlisted.
    pub rosters: HashMap<String, Roster>,
    /// Each community's running Timeouts, npub → its span.
    pub timeouts: HashMap<String, HashMap<String, Timeout>>,
    /// Per channel, the authors seen there who can't send in it.
    pub send_denied: HashMap<String, HashSet<String>>,
    /// Per channel, the reactors seen there who can't react in it: their reactions don't count.
    pub react_denied: HashMap<String, HashSet<String>>,
    /// Per channel, every reactor asked about.
    pub react_checked: HashMap<String, HashSet<String>>,
    /// Fixtures' members seen posting outside the Guestbook, by community.
    pub fixture_unlisted: HashMap<String, HashMap<String, String>>,
    /// Each community's role graph, once read: roster sections and name colours.
    pub role_graphs: HashMap<String, crate::community::Graph>,
    /// Each Community's taggable Roles and @everyone rule, once read.
    pub mention_rules: HashMap<String, crate::mentions::Rules>,
    /// Collapsed posts tapped open in the open chat.
    pub unveiled: HashSet<String>,
    /// Each community's logo and banner as local files, once resolved.
    pub art: HashMap<String, Art>,
    pub hidden_banners: Vec<String>,
    pub streamer: Streamer,
    /// The synced setting that switches the link cards' players off.
    pub players_off: bool,
    /// Channels whose start was reached but whose oldest epochs this account never held.
    pub history_limited: std::collections::HashSet<String>,
    /// Why YouTube refused to play a card's video here, by message: it then opens on YouTube.
    pub player_refused: HashMap<String, String>,
    /// Pinned chat-list rows in pin order: a DM's npub or a community's id.
    pub pinned: Vec<String>,
    /// Archived DMs: chat id → the newest message time when archived.
    pub archived: HashMap<String, u64>,
    /// The community rail's synced order and folders.
    pub rail: vector_core::rail_layout::RailLayout,
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
                // i18n-exempt: markup
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
    /// The player loads outside `vector_core::transport`, so only a settled Clearnet allows it.
    pub fn players_available(&self) -> bool {
        !self.players_off
            && (std::env::var_os("VECTOR_PLAYERS").is_some() || vector_core::transport::preference() == Some(vector_core::transport::Kind::Clearnet))
    }

    pub fn from_fixtures(path: &Path) -> anyhow::Result<Self> {
        let mut f: Fixtures = serde_json::from_slice(&std::fs::read(path)?)?;
        let demo = match std::env::var_os("VECTOR_CHATLIST_DEMO") {
            Some(_) => f.chatlist_demo.unwrap_or_default(),
            None => ChatlistDemo::default(),
        };
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
        // VECTOR_NOTIFY=<0|1|2>: every chat's resolved level. VECTOR_PINGS=1: each Community
        // channel's newest message tags us.
        if let Some(level) = std::env::var("VECTOR_NOTIFY").ok().and_then(|n| n.parse::<u8>().ok()) {
            for chat in &mut chats {
                chat.notify = level;
            }
        }
        // VECTOR_MUTED=1: every other chat muted (rung down to Mentions, as core resolves it).
        // VECTOR_COMMUNITY_MUTED=1: the Communities themselves.
        if std::env::var_os("VECTOR_MUTED").is_some() {
            for chat in chats.iter_mut().skip(1).step_by(2) {
                chat.muted = true;
                chat.notify = chat.notify.max(1);
            }
        }
        if std::env::var_os("VECTOR_COMMUNITY_MUTED").is_some() {
            for chat in chats.iter_mut().filter(|c| c.chat_type == vector_core::chat::ChatType::Community) {
                chat.community_muted = true;
                chat.muted = true;
                chat.notify = chat.notify.max(1);
            }
        }
        if std::env::var_os("VECTOR_PINGS").is_some() {
            for chat in chats.iter_mut().filter(|c| c.chat_type == vector_core::chat::ChatType::Community) {
                if let Some(m) = chat.messages.iter_mut().rev().find(|m| !m.mine) {
                    m.content = format!("@{} {}", f.npub, m.content);
                    chat.last_read.clear();
                }
            }
        }
        chats.sort_by_key(|c| std::cmp::Reverse(last_activity(c)));
        crate::relayinfo::set_fixture(
            std::mem::take(&mut f.relay_info),
            path.parent().unwrap_or(Path::new(".")).to_path_buf(),
        );
        let mut store = Self {
            my_npub: f.npub,
            profiles: f.profiles.into_iter().map(|p| (p.id.clone(), p)).collect(),
            chats,
            media_root: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            frozen_now_ms: Some(f.now),
            unread: None,
            mutuals: Default::default(),
            communities: match std::env::var_os("VECTOR_RAIL_DEMO") {
                Some(_) => f.communities.into_iter().chain(f.rail_demo).collect(),
                None => f.communities,
            },
            invites: f.invites.into_iter().map(Invite::from).collect(),
            fixture_audit: f.audit,
            fixture_sync_report: f.sync_report,
            fixture_roles: f.roles_view,
            fixture_channel_perms: f.channel_perms,
            fixture_bot_commands: f.bot_commands,
            fixture_moderation: f.moderation,
            fixture_voice: std::env::var("VECTOR_VOICE").ok().and_then(|k| {
                let demo = f.voice_demo.as_ref()?;
                // `held`: joined, with a moderator's mute on us.
                let (k, held) = match k.as_str() {
                    "held" => ("joined".to_string(), true),
                    _ => (k, false),
                };
                let mut session = match k.as_str() {
                    "joined" => demo.get("session").cloned(),
                    "video" => demo.get("session_video").cloned(),
                    _ => None,
                };
                if let Some(serde_json::Value::Object(s)) = session.as_mut().filter(|_| held) {
                    s.insert("server_muted".into(), true.into());
                    s.insert("muted".into(), true.into());
                }
                Some(serde_json::json!({ "present": demo.get(&k)?, "session": session }))
            }),
            fixture_pins: if std::env::var_os("VECTOR_PINS").is_some() { f.pins_demo } else { HashMap::new() },
            fixture_servers: f.server_demo,
            fixture_net: std::env::var("VECTOR_NET").ok().and_then(|n| f.net_demo.get(&n).cloned()),
            fixture_i2p: f.net_demo.get("i2p_config").cloned(),
            fixture_presets: f.policy_presets,
            fixture_embeds: f.embeds,
            typing: HashMap::new(),
            system: HashMap::new(),
            invite_sight: HashMap::new(),
            fixture_packs: f.packs,
            fixture_market: f.marketplace,
            fixture_pivx: f.pivx,
            pivx: HashMap::new(),
            pivx_bubbles: HashMap::new(),
            pivx_price: None,
            nicknames: f.nicknames,
            rosters: HashMap::new(),
            timeouts: f.timeouts,
            send_denied: f.send_denied,
            react_denied: f.react_denied,
            react_checked: HashMap::new(),
            unveiled: HashSet::new(),
            fixture_unlisted: f.unlisted,
            role_graphs: f.role_graphs.iter().map(|(k, v)| (k.clone(), crate::community::Graph::parse(v))).collect(),
            mention_rules: HashMap::new(),
            art: HashMap::new(),
            hidden_banners: Vec::new(),
            pinned: demo.pinned,
            archived: demo.archived,
            rail: Default::default(),
            players_off: std::env::var_os("VECTOR_PLAYERS").is_none(),
            history_limited: Default::default(),
            player_refused: HashMap::new(),
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
        };
        // Fixtures: VECTOR_SYSTEM=1 puts one line of each kind at the end of the first chat.
        if std::env::var_os("VECTOR_SYSTEM").is_some()
            && let Some(chat) = store.chats.first()
        {
            let (id, at) = (chat.id.clone(), chat.messages.last().map_or(0, |m| m.at));
            let npub = chat.messages.iter().rev().find_map(|m| m.npub.clone()).unwrap_or_else(|| id.clone());
            for kind in 0..6u8 {
                let via = (kind == 1).then(|| ("Friends".to_string(), npub.clone())); // i18n-exempt: fixture
                let line = SysLine { id: format!("sys-demo-{kind}"), at: at + 1 + kind as u64, kind, npub: npub.clone(), via };
                store.add_system(&id, line);
            }
        }
        Ok(store)
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
            fixture_sync_report: None,
            fixture_roles: None,
            fixture_channel_perms: None,
            fixture_bot_commands: None,
            fixture_moderation: None,
            fixture_voice: None,
            fixture_pins: HashMap::new(),
            fixture_servers: HashMap::new(),
            fixture_net: None,
            fixture_i2p: None,
            fixture_presets: None,
            fixture_embeds: HashMap::new(),
            typing: HashMap::new(),
            system: HashMap::new(),
            invite_sight: HashMap::new(),
            fixture_packs: HashMap::new(),
            fixture_market: Vec::new(),
            fixture_pivx: None,
            pivx: HashMap::new(),
            pivx_bubbles: HashMap::new(),
            pivx_price: None,
            nicknames: HashMap::new(),
            rosters: HashMap::new(),
            timeouts: HashMap::new(),
            send_denied: HashMap::new(),
            react_denied: HashMap::new(),
            react_checked: HashMap::new(),
            unveiled: HashSet::new(),
            fixture_unlisted: HashMap::new(),
            role_graphs: HashMap::new(),
            mention_rules: HashMap::new(),
            art: HashMap::new(),
            hidden_banners: Vec::new(),
            streamer: Default::default(),
            players_off: false,
            history_limited: Default::default(),
            player_refused: HashMap::new(),
            pinned: Vec::new(),
            archived: HashMap::new(),
            rail: Default::default(),
        }
    }

    /// `computeRowUnreadCount`: a muted chat or channel never counts as unread.
    pub fn unread_count(&self, chat: &SerializableChat) -> usize {
        if chat.muted {
            return 0;
        }
        match &self.unread {
            Some(counts) => counts.get(&chat.id).copied().unwrap_or(0) as usize,
            None => self.walk_unread(chat),
        }
    }

    /// `countUnreadMessages`: back to `last_read` or our own latest message; a Mark as Unread
    /// counts back to its chosen message instead, past our own replies. A blocked or muted
    /// sender's posts in a Community never count.
    pub fn walk_unread(&self, chat: &SerializableChat) -> usize {
        let from = chat.unread_from.as_str();
        let group = chat.chat_type == vector_core::chat::ChatType::Community;
        let mut n = 0;
        for m in chat.messages.iter().rev() {
            let reached = !from.is_empty() && m.id == from;
            if m.mine {
                if !from.is_empty() && !reached {
                    continue;
                }
                break;
            }
            if from.is_empty() && !chat.last_read.is_empty() && m.id == chat.last_read {
                break;
            }
            let hidden = group
                && m.npub.as_deref().is_some_and(|n| {
                    self.profiles.get(n).is_some_and(|p| p.is_blocked) || self.chat(n).is_some_and(|c| c.muted)
                });
            if !hidden {
                n += 1;
            }
            if reached {
                break;
            }
        }
        n
    }

    /// A system line, put in place by time. False when it is already known.
    pub fn add_system(&mut self, chat_id: &str, line: SysLine) -> bool {
        let lines = self.system.entry(chat_id.to_string()).or_default();
        if lines.iter().any(|l| l.id == line.id) {
            return false;
        }
        let at = lines.partition_point(|l| l.at <= line.at);
        lines.insert(at, line);
        true
    }

    /// A PIVX payment, as the message it shows as in its chat.
    pub fn add_pivx(&mut self, chat_id: &str, p: &serde_json::Value) -> bool {
        use serde_json::Value;
        let Some(id) = p.get("message_id").and_then(Value::as_str) else { return false };
        let sender = p.get("sender").and_then(Value::as_str).unwrap_or_default();
        let npub = if sender.starts_with("npub1") {
            sender.to_string()
        } else {
            nostr_sdk::prelude::PublicKey::from_hex(sender).ok().and_then(|k| nostr_sdk::prelude::ToBech32::to_bech32(&k).ok()).unwrap_or_default()
        };
        self.pivx.insert(
            id.to_string(),
            PivxPay {
                gift_code: p.get("gift_code").and_then(Value::as_str).unwrap_or_default().to_string(),
                amount_piv: p.get("amount_piv").and_then(Value::as_f64).unwrap_or(0.),
                address: p.get("address").and_then(Value::as_str).map(String::from),
            },
        );
        let msg = vector_core::types::Message {
            id: id.to_string(),
            at: p.get("at").and_then(Value::as_u64).unwrap_or(0),
            mine: p.get("is_mine").and_then(Value::as_bool).unwrap_or(false),
            npub: Some(npub).filter(|n| !n.is_empty()),
            ..Default::default()
        };
        self.insert_message(chat_id, msg)
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

    pub fn anyone_typing(&self) -> bool {
        self.typing.keys().any(|c| !self.typers(c).is_empty())
    }

    /// How long until the soonest typing notice lapses, so its strip can go on time.
    pub fn typing_due(&self) -> Option<std::time::Duration> {
        let now = self.now_ms();
        self.typing
            .values()
            .filter_map(|(who, at)| (!who.is_empty()).then(|| (at + TYPING_TTL_MS).checked_sub(now)).flatten())
            .min()
            .map(|ms| std::time::Duration::from_millis(ms + 1))
    }

    /// Who is typing in `chat` now, minus anyone blocked.
    pub fn typers(&self, chat: &str) -> Vec<String> {
        match self.typing.get(chat) {
            Some((who, at)) if self.now_ms().saturating_sub(*at) <= TYPING_TTL_MS => who
                .iter()
                .filter(|n| !self.profiles.get(*n).is_some_and(|p| p.is_blocked))
                .cloned()
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.frozen_now_ms
            .unwrap_or_else(vector_core::clock::now_ms)
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
    /// `chatIsArchived`: archived, and nothing said since the archiving device's newest message.
    pub fn is_archived(&self, chat: &SerializableChat) -> bool {
        chat.chat_type == vector_core::ChatType::DirectMessage
            && self.archived.get(&chat.id).is_some_and(|&at| last_activity(chat) <= at)
    }

    pub fn pin_rank(&self, id: &str) -> Option<usize> {
        self.pinned.iter().position(|p| p == id)
    }

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

    /// `npub`'s Timeout in `community`, while it runs.
    pub fn timeout(&self, community: &str, npub: &str) -> Option<Timeout> {
        let now = self.now_ms() / 1000;
        self.timeouts.get(community)?.get(npub).copied().filter(|t| t.until > now)
    }

    /// `channelAllowsReact`: whether I may react in `chat`; a DM always may.
    pub fn can_react(&self, chat: &str) -> bool {
        const ADD_REACTIONS: u64 = 1 << 17;
        self.community_of(chat)
            .and_then(|c| c.channel(chat))
            .and_then(|ch| ch.my_permissions.as_deref())
            .and_then(|p| p.parse::<u64>().ok())
            .is_none_or(|p| p & ADD_REACTIONS != 0)
    }

    /// `countedReactions`: `m`'s reactions less any by someone who can't react in `chat`.
    pub fn counted<'a>(&'a self, chat: &str, m: &'a vector_core::Message) -> impl Iterator<Item = &'a vector_core::types::Reaction> {
        let denied = self.react_denied.get(chat).filter(|d| !d.is_empty());
        m.reactions.iter().filter(move |r| denied.is_none_or(|d| !d.contains(&r.author_id)))
    }

    /// Why the composer can't send in `chat`, as its placeholder.
    pub fn send_lock(&self, chat: &str) -> Option<gpui::SharedString> {
        if self.chat(chat).is_some_and(|c| c.chat_type == vector_core::ChatType::DirectMessage)
            && self.profiles.get(chat).is_some_and(|p| p.is_blocked)
        {
            return Some(t::compose_lock_blocked());
        }
        let community = self.community_of(chat)?;
        let channel = community.channel(chat)?;
        if community.dissolved {
            return Some(t::compose_lock_dissolved());
        }
        const SEND_MESSAGES: u64 = 1 << 14;
        if channel.my_permissions.as_deref().and_then(|p| p.parse::<u64>().ok()).is_some_and(|p| p & SEND_MESSAGES == 0) {
            return Some(t::compose_lock_no_permission());
        }
        if channel.restricted && !channel.can_post {
            return Some(t::compose_lock_key_pending());
        }
        let t = self.timeout(&community.community_id, &self.my_npub)?;
        let until = crate::dates::at_secs(t.until as i64)?;
        Some(t::compose_lock_timed_out(crate::dates::date_time(&until, crate::dates::DateStyle::Weekday, crate::dates::Clock::PADDED)))
    }

    /// `_dmsgPostedTimedOut` and a blocked group author: why `m` shows collapsed, if it does.
    pub fn hidden(&self, chat: &SerializableChat, m: &vector_core::Message) -> Option<Hidden> {
        if self.unveiled.contains(&m.id) {
            return None;
        }
        self.hidden_for(chat, m)
    }

    /// Why a shown post was collapsed, for the line that collapses it again.
    pub fn revealed_as(&self, chat: &SerializableChat, m: &vector_core::Message) -> Option<Hidden> {
        if !self.unveiled.contains(&m.id) {
            return None;
        }
        self.hidden_for(chat, m)
    }

    fn hidden_for(&self, chat: &SerializableChat, m: &vector_core::Message) -> Option<Hidden> {
        if m.mine {
            return None;
        }
        let npub = m.npub.as_deref()?;
        if chat.chat_type != vector_core::ChatType::DirectMessage && self.profiles.get(npub).is_some_and(|p| p.is_blocked) {
            return Some(Hidden::Blocked);
        }
        if m.unauthorized {
            return Some(Hidden::Unauthorized);
        }
        let community = self.community_of(&chat.id)?;
        let was_public = community.channel(&chat.id).and_then(|c| c.public_until).is_some_and(|u| m.at < u);
        if !m.writer && !was_public && self.send_denied.get(&chat.id).is_some_and(|d| d.contains(npub)) {
            return Some(Hidden::NoSend);
        }
        let t = self.timeouts.get(&community.community_id)?.get(npub)?;
        let inside = |ms: u64| ms / 1000 >= t.from && ms / 1000 < t.until;
        let edited = m.edit_history.as_ref().filter(|h| h.len() > 1).and_then(|h| h.last()).is_some_and(|e| inside(e.edited_at));
        (inside(m.at) || edited).then_some(Hidden::TimedOut)
    }

    /// What closes the timeline: a blocked contact, or a community its owner dissolved.
    pub fn end_notice(&self, chat: &str) -> Option<gpui::SharedString> {
        if self.chat(chat).is_some_and(|c| c.chat_type == vector_core::ChatType::DirectMessage)
            && self.profiles.get(chat).is_some_and(|p| p.is_blocked)
        {
            return Some(t::chat_end_blocked());
        }
        let c = self.community_of(chat).filter(|c| c.dissolved)?;
        Some(if c.name.is_empty() { t::chat_end_dissolved() } else { t::chat_end_dissolved_named(c.name.as_str()) })
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
        crate::profilewatch::seen(npub);
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

/// `findLatestContactMessage`: what a read marks, since the backend's copy of a chat can hold
/// only our own newest message and would then mark nothing.
pub fn newest_theirs(messages: &[vector_core::Message]) -> Option<String> {
    messages.iter().rev().find(|m| !m.mine).map(|m| m.id.clone())
}

/// Messages after `last_read` that someone else sent: `computeListRowUnreadCount`.
pub fn unread_count(chat: &SerializableChat) -> usize {
    let Some(i) = chat.messages.iter().rposition(|m| m.id == chat.last_read) else {
        return chat.messages.iter().filter(|m| !m.mine).count();
    };
    chat.messages[i + 1..].iter().filter(|m| !m.mine).count()
}

/// Typing lapses this long after the last word of it.
pub const TYPING_TTL_MS: u64 = 30_000;

/// `generateTypingText`: a DM already knows who; a group names up to three.
pub fn typing_text(typers: &[String], group: bool, name: impl Fn(&str) -> String) -> Option<String> {
    Some(match typers {
        [] => return None,
        _ if !group => t::typing_dm().into(),
        [a] => t::typing_group(1, &name(a), "", "").into(),
        [a, b] => t::typing_group(2, &name(a), &name(b), "").into(),
        [a, b, c] => t::typing_group(3, &name(a), &name(b), &name(c)).into(),
        _ => t::typing_group(typers.len(), "", "", "").into(),
    })
}

/// The preview line: `chatPreviewText` (render/chatlist/preview.js).
pub fn preview_text(chat: &SerializableChat) -> String {
    let Some(m) = chat.messages.last() else {
        return String::new();
    };
    let body = if m.content.is_empty() && !m.attachments.is_empty() {
        t::chats_preview_file().to_string()
    } else {
        crate::newchat::replace_invite_links(&m.content, &t::chats_preview_invite())
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    };
    if m.mine { t::chats_preview_mine(body).to_string() } else { body }
}

/// `timeAgo` (misc.js).
pub fn time_ago(now_ms: u64, at_ms: u64) -> String {
    let s = now_ms.saturating_sub(at_ms) / 1000;
    match s {
        0..60 => t::ago_now().into(),
        60..3600 => t::ago_minutes(s / 60).into(),
        3600..86_400 => t::ago_hours(s / 3600).into(),
        86_400..604_800 => t::ago_days(s / 86_400).into(),
        604_800..2_592_000 => t::ago_weeks(s / 604_800).into(),
        2_592_000..31_536_000 => t::ago_months(s / 2_592_000).into(),
        _ => t::ago_years(s / 31_536_000).into(),
    }
}

pub fn local(ms: u64) -> DateTime<Local> {
    Local
        .timestamp_millis_opt(ms as i64)
        .single()
        // device-clock: only for a time chrono can't hold.
        .unwrap_or_else(Local::now)
}

/// A message's clock time, as `toLocaleTimeString([], {hour: 'numeric', minute: '2-digit'})`.
pub fn clock(ms: u64) -> String {
    crate::dates::time(&local(ms), crate::dates::Clock::SHORT)
}

/// The date divider's bold half: "Today", "Yesterday", a weekday or a date.
pub fn day_label(now_ms: u64, ms: u64) -> String {
    let (now, then) = (local(now_ms).date_naive(), local(ms).date_naive());
    match (now - then).num_days() {
        0 => t::day_today().into(),
        1 => t::day_yesterday().into(),
        _ => crate::dates::date(&local(ms), crate::dates::DateStyle::Numeric),
    }
}

/// One row of the message timeline; `Message` indexes the chat's `messages`.
pub enum Row {
    /// Above the oldest message: the chat's beginning, or older history on its way.
    Edge(Edge),
    Date {
        label: String,
        time: String,
    },
    Unread,
    /// Under the last message: why nothing new will arrive.
    Notice(String),
    /// `first` heads a streak (avatar and header); otherwise a continuation. `run` counts one
    /// author's blocked messages folded into this row; `shown` the run of one author's shown
    /// collapsed messages this row heads, which hide again together.
    Message {
        index: usize,
        first: bool,
        run: u32,
        shown: u32,
    },
    /// `index` into the chat's system lines; `count` folds back-to-back repeats of a mergeable one.
    System {
        index: usize,
        count: u32,
    },
}

/// `SystemEvent.svelte`'s line: who did it, and what (`SystemEventType`).
#[derive(Clone, Debug)]
pub struct SysLine {
    pub id: String,
    pub at: u64,
    pub kind: u8,
    pub npub: String,
    /// The invite link that brought a joiner: its label, and its creator's npub.
    pub via: Option<(String, String)>,
}

/// `systemEventSuffix`, as a sentence with the actor's name in `<b>`.
pub fn system_text(kind: u8, name: &str, via: Option<&str>, count: u32) -> gpui::SharedString {
    let name = crate::kit::escape(name);
    match (kind, via) {
        (0, _) => t::system_left(name),
        (1, Some(link)) => t::system_joined_via(crate::kit::escape(link), name),
        (1, None) => t::system_joined(name),
        (2, _) => t::system_removed(name),
        (3, _) => t::system_wallpaper_changed(count, name),
        (4, _) => t::system_wallpaper_removed(count, name),
        (5, _) => t::system_pins_modified(count, name),
        _ => format!("<b>{name}</b>").into(),
    }
}

/// `MERGEABLE_SYSTEM_EVENTS`: membership never merges, since who joined is the content.
fn system_merges(kind: u8) -> bool {
    matches!(kind, 3..=5)
}

#[derive(Clone, Copy, PartialEq)]
pub enum Edge {
    Start,
    Loading,
    Retry,
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

/// Where the NEW divider goes, fixed when the chat opens so reading doesn't move it.
#[derive(Clone, Debug, PartialEq)]
pub enum Unread {
    /// Above the first message from someone else after this one (`last_read`).
    After(String),
    /// Above this message (Mark as Unread's `unread_from`).
    At(String),
}

/// Lays a chat's messages out as the Tauri timeline does: a date divider before the first
/// message of each day, and the NEW divider where `read_anchor` puts it.
pub fn timeline(
    chat: &SerializableChat,
    system: &[SysLine],
    all_history: bool,
    now_ms: u64,
    read_anchor: Option<&Unread>,
    commands: Option<&HashSet<String>>,
    blocked: &dyn Fn(&vector_core::Message) -> bool,
    revealed: &dyn Fn(&vector_core::Message) -> bool,
) -> Vec<Row> {
    let mut rows = Vec::with_capacity(chat.messages.len() + 2);
    let mut last_day = None;
    let mut run_head: Option<usize> = None;
    let mut shown_head: Option<usize> = None;
    let at = |id: &str| chat.messages.iter().position(|m| m.id == id);
    let divider = match read_anchor {
        Some(Unread::At(id)) => at(id),
        Some(Unread::After(id)) => at(id).and_then(|r| (r + 1..chat.messages.len()).find(|&i| !chat.messages[i].mine)),
        None => None,
    };
    // Lines older than the oldest loaded message wait until that history is in.
    let floor = if all_history { 0 } else { chat.messages.first().map_or(u64::MAX, |m| m.at) };
    let mut sys = system.iter().enumerate().skip_while(|(_, l)| l.at < floor).peekable();
    let mut after_system = false;
    let mut push_system = |rows: &mut Vec<Row>, last_day: &mut Option<chrono::NaiveDate>, upto: u64| {
        let mut any = false;
        while let Some((k, l)) = sys.next_if(|(_, l)| l.at <= upto) {
            let day = local(l.at).date_naive();
            if *last_day != Some(day) {
                rows.push(Row::Date { label: day_label(now_ms, l.at), time: clock(l.at) });
                *last_day = Some(day);
            }
            if let Some(Row::System { index, count }) = rows.last_mut()
                && system[*index].kind == l.kind
                && system_merges(l.kind)
            {
                *count += 1;
            } else {
                rows.push(Row::System { index: k, count: 1 });
            }
            any = true;
        }
        any
    };
    for (i, m) in chat.messages.iter().enumerate() {
        if push_system(&mut rows, &mut last_day, m.at) {
            after_system = true;
            run_head = None;
            shown_head = None;
        }
        let day = local(m.at).date_naive();
        if last_day != Some(day) {
            rows.push(Row::Date {
                label: day_label(now_ms, m.at),
                time: clock(m.at),
            });
            last_day = Some(day);
            run_head = None;
            shown_head = None;
        }
        if divider == Some(i) {
            rows.push(Row::Unread);
        }
        // As `deriveWindow`: a day divider doesn't end a streak.
        let first = (i == 0 || after_system || !collapses(&chat.messages[i - 1], m)) && command(m, commands).is_none();
        after_system = false;
        if !blocked(m) {
            run_head = None;
        } else if let Some(Row::Message { index, run, .. }) = run_head.and_then(|h| rows.get_mut(h))
            && chat.messages[*index].npub == m.npub
        {
            *run += 1;
            continue;
        } else {
            run_head = Some(rows.len());
        }
        let mut shown = 0;
        if !revealed(m) {
            shown_head = None;
        } else if let Some(Row::Message { index, shown, .. }) = shown_head.and_then(|h| rows.get_mut(h))
            && chat.messages[*index].npub == m.npub
        {
            *shown += 1;
        } else {
            shown_head = Some(rows.len());
            shown = 1;
        }
        rows.push(Row::Message { index: i, first, run: 1, shown });
    }
    push_system(&mut rows, &mut last_day, u64::MAX);
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
pub fn file_type(extension: &str) -> (&'static str, gpui::SharedString) {
    let (icon, said): (&'static str, fn() -> gpui::SharedString) = match extension.to_ascii_lowercase().as_str() {
        "png" => ("image", t::file_type_picture),
        "jpg" => ("image", t::file_type_picture),
        "jpeg" => ("image", t::file_type_picture),
        "gif" => ("film", t::file_type_gif_animation),
        "webp" => ("image", t::file_type_picture),
        "jxl" => ("image", t::file_type_picture),
        "svg" => ("image", t::file_type_vector_image),
        "bmp" => ("image", t::file_type_bitmap_image),
        "ico" => ("image", t::file_type_icon),
        "tiff" => ("image", t::file_type_tiff_image),
        "tif" => ("image", t::file_type_tiff_image),
        "raw" => ("image", t::file_type_raw_image),
        "dng" => ("image", t::file_type_raw_image),
        "cr2" => ("image", t::file_type_canon_raw),
        "nef" => ("image", t::file_type_nikon_raw),
        "arw" => ("image", t::file_type_sony_raw),
        "orf" => ("image", t::file_type_olympus_raw),
        "rw2" => ("image", t::file_type_panasonic_raw),
        "wav" => ("mic-on", t::file_type_voice_message),
        "mp3" => ("mic-on", t::file_type_audio_clip),
        "m4a" => ("mic-on", t::file_type_audio_clip),
        "aac" => ("mic-on", t::file_type_audio_clip),
        "flac" => ("mic-on", t::file_type_audio_clip),
        "ogg" => ("mic-on", t::file_type_audio_clip),
        "wma" => ("mic-on", t::file_type_audio_clip),
        "opus" => ("mic-on", t::file_type_audio_clip),
        "ape" => ("mic-on", t::file_type_audio_clip),
        "wv" => ("mic-on", t::file_type_audio_clip),
        "aup" => ("mic-on", t::file_type_audacity_project),
        "flp" => ("mic-on", t::file_type_fl_studio_project),
        "als" => ("mic-on", t::file_type_ableton_project),
        "logic" => ("mic-on", t::file_type_logic_project),
        "band" => ("mic-on", t::file_type_garageband_project),
        "mp4" => ("film", t::file_type_video),
        "webm" => ("film", t::file_type_video),
        "mov" => ("film", t::file_type_video),
        "avi" => ("film", t::file_type_video),
        "mkv" => ("film", t::file_type_video),
        "flv" => ("film", t::file_type_flash_video),
        "wmv" => ("film", t::file_type_windows_video),
        "mpg" => ("film", t::file_type_mpeg_video),
        "mpeg" => ("film", t::file_type_mpeg_video),
        "m4v" => ("film", t::file_type_mpeg_4_video),
        "3gp" => ("film", t::file_type_3gp_video),
        "3g2" => ("film", t::file_type_3g2_video),
        "f4v" => ("film", t::file_type_flash_mp4_video),
        "asf" => ("film", t::file_type_advanced_systems_format),
        "rm" => ("film", t::file_type_realmedia),
        "vob" => ("film", t::file_type_dvd_video),
        "ogv" => ("film", t::file_type_ogg_video),
        "mxf" => ("film", t::file_type_material_exchange_format),
        "m2ts" => ("film", t::file_type_blu_ray_video),
        "pdf" => ("file", t::file_type_pdf_document),
        "doc" => ("file", t::file_type_word_document),
        "docx" => ("file", t::file_type_word_document),
        "xls" => ("file", t::file_type_excel_spreadsheet),
        "xlsx" => ("file", t::file_type_excel_spreadsheet),
        "ppt" => ("file", t::file_type_powerpoint_presentation),
        "pptx" => ("file", t::file_type_powerpoint_presentation),
        "odt" => ("file", t::file_type_opendocument_text),
        "ods" => ("file", t::file_type_opendocument_spreadsheet),
        "odp" => ("file", t::file_type_opendocument_presentation),
        "rtf" => ("file", t::file_type_rich_text_document),
        "tex" => ("file", t::file_type_latex_document),
        "pages" => ("file", t::file_type_pages_document),
        "numbers" => ("file", t::file_type_numbers_spreadsheet),
        "key" => ("file", t::file_type_keynote_presentation),
        "txt" => ("file", t::file_type_text_file),
        "md" => ("file", t::file_type_markdown_file),
        "log" => ("file", t::file_type_log_file),
        "csv" => ("file", t::file_type_csv_file),
        "tsv" => ("file", t::file_type_tsv_file),
        "json" => ("file", t::file_type_json_file),
        "xml" => ("file", t::file_type_xml_file),
        "yaml" => ("file", t::file_type_yaml_file),
        "yml" => ("file", t::file_type_yaml_file),
        "toml" => ("file", t::file_type_toml_file),
        "sql" => ("file", t::file_type_sql_file),
        "db" => ("file", t::file_type_database_file),
        "sqlite" => ("file", t::file_type_sqlite_database),
        "zip" => ("folder", t::file_type_zip_archive),
        "rar" => ("folder", t::file_type_rar_archive),
        "7z" => ("folder", t::file_type_7_zip_archive),
        "tar" => ("folder", t::file_type_tar_archive),
        "gz" => ("folder", t::file_type_gzip_archive),
        "bz2" => ("folder", t::file_type_bzip2_archive),
        "xz" => ("folder", t::file_type_xz_archive),
        "tgz" => ("folder", t::file_type_compressed_tar),
        "tbz" => ("folder", t::file_type_compressed_tar),
        "txz" => ("folder", t::file_type_compressed_tar),
        "cab" => ("folder", t::file_type_cabinet_archive),
        "iso" => ("file", t::file_type_disc_image),
        "dmg" => ("file", t::file_type_macos_disk_image),
        "pkg" => ("file", t::file_type_package_file),
        "deb" => ("file", t::file_type_debian_package),
        "rpm" => ("file", t::file_type_rpm_package),
        "apk" => ("file", t::file_type_android_package),
        "ipa" => ("file", t::file_type_ios_app),
        "jar" => ("file", t::file_type_java_archive),
        "war" => ("file", t::file_type_web_archive),
        "ear" => ("file", t::file_type_enterprise_archive),
        "obj" => ("file", t::file_type_3d_object),
        "fbx" => ("file", t::file_type_autodesk_fbx),
        "gltf" => ("file", t::file_type_gl_transmission_format),
        "glb" => ("file", t::file_type_gl_binary),
        "stl" => ("file", t::file_type_stereolithography),
        "ply" => ("file", t::file_type_polygon_file),
        "dae" => ("file", t::file_type_collada),
        "3ds" => ("file", t::file_type_3d_studio),
        "blend" => ("file", t::file_type_blender_file),
        "c4d" => ("file", t::file_type_cinema_4d),
        "max" => ("file", t::file_type_3ds_max),
        "ma" => ("file", t::file_type_maya_ascii),
        "mb" => ("file", t::file_type_maya_binary),
        "usdz" => ("file", t::file_type_universal_scene),
        "dwg" => ("file", t::file_type_autocad_drawing),
        "dxf" => ("file", t::file_type_drawing_exchange),
        "step" => ("file", t::file_type_step_cad),
        "stp" => ("file", t::file_type_step_cad),
        "iges" => ("file", t::file_type_iges_cad),
        "igs" => ("file", t::file_type_iges_cad),
        "sat" => ("file", t::file_type_acis_sat),
        "ipt" => ("file", t::file_type_inventor_part),
        "iam" => ("file", t::file_type_inventor_assembly),
        "prt" => ("file", t::file_type_part_file),
        "sldprt" => ("file", t::file_type_solidworks_part),
        "sldasm" => ("file", t::file_type_solidworks_assembly),
        "slddrw" => ("file", t::file_type_solidworks_drawing),
        "catpart" => ("file", t::file_type_catia_part),
        "catproduct" => ("file", t::file_type_catia_product),
        "js" => ("file", t::file_type_javascript),
        "ts" => ("file", t::file_type_typescript),
        "jsx" => ("file", t::file_type_react_jsx),
        "tsx" => ("file", t::file_type_react_tsx),
        "py" => ("file", t::file_type_python),
        "rs" => ("file", t::file_type_rust),
        "go" => ("file", t::file_type_go),
        "java" => ("file", t::file_type_java),
        "kt" => ("file", t::file_type_kotlin),
        "cpp" => ("file", t::file_type_cpp),
        "cc" => ("file", t::file_type_cpp),
        "cxx" => ("file", t::file_type_cpp),
        "c" => ("file", t::file_type_c),
        "h" => ("file", t::file_type_header_file),
        "hpp" => ("file", t::file_type_cpp_header),
        "cs" => ("file", t::file_type_c_sharp),
        "rb" => ("file", t::file_type_ruby),
        "php" => ("file", t::file_type_php),
        "swift" => ("file", t::file_type_swift),
        "m" => ("file", t::file_type_objective_c),
        "mm" => ("file", t::file_type_objective_cpp),
        "lua" => ("file", t::file_type_lua),
        "r" => ("file", t::file_type_r_script),
        "scala" => ("file", t::file_type_scala),
        "clj" => ("file", t::file_type_clojure),
        "dart" => ("file", t::file_type_dart),
        "ex" => ("file", t::file_type_elixir),
        "elm" => ("file", t::file_type_elm),
        "erl" => ("file", t::file_type_erlang),
        "fs" => ("file", t::file_type_f_sharp),
        "hs" => ("file", t::file_type_haskell),
        "jl" => ("file", t::file_type_julia),
        "nim" => ("file", t::file_type_nim),
        "pl" => ("file", t::file_type_perl),
        "sh" => ("file", t::file_type_shell_script),
        "bash" => ("file", t::file_type_bash_script),
        "zsh" => ("file", t::file_type_zsh_script),
        "fish" => ("file", t::file_type_fish_script),
        "ps1" => ("file", t::file_type_powershell),
        "bat" => ("file", t::file_type_batch_file),
        "cmd" => ("file", t::file_type_command_file),
        "vb" => ("file", t::file_type_visual_basic),
        "vbs" => ("file", t::file_type_vbscript),
        "asm" => ("file", t::file_type_assembly),
        "s" => ("file", t::file_type_assembly),
        "ini" => ("file", t::file_type_ini_config),
        "cfg" => ("file", t::file_type_config_file),
        "conf" => ("file", t::file_type_config_file),
        "config" => ("file", t::file_type_config_file),
        "env" => ("file", t::file_type_environment_file),
        "properties" => ("file", t::file_type_properties_file),
        "plist" => ("file", t::file_type_property_list),
        "gitignore" => ("file", t::file_type_git_ignore),
        "dockerignore" => ("file", t::file_type_docker_ignore),
        "editorconfig" => ("file", t::file_type_editor_config),
        "eslintrc" => ("file", t::file_type_eslint_config),
        "prettierrc" => ("file", t::file_type_prettier_config),
        "html" => ("file", t::file_type_html_file),
        "htm" => ("file", t::file_type_html_file),
        "css" => ("file", t::file_type_css_stylesheet),
        "scss" => ("file", t::file_type_scss_stylesheet),
        "sass" => ("file", t::file_type_sass_stylesheet),
        "less" => ("file", t::file_type_less_stylesheet),
        "vue" => ("file", t::file_type_vue_component),
        "svelte" => ("file", t::file_type_svelte_component),
        "eps" => ("file", t::file_type_encapsulated_postscript),
        "ai" => ("file", t::file_type_adobe_illustrator),
        "sketch" => ("file", t::file_type_sketch_file),
        "fig" => ("file", t::file_type_figma_file),
        "xd" => ("file", t::file_type_adobe_xd),
        "exe" => ("file", t::file_type_executable),
        "msi" => ("file", t::file_type_windows_installer),
        "app" => ("file", t::file_type_macos_application),
        "ttf" => ("file", t::file_type_truetype_font),
        "otf" => ("file", t::file_type_opentype_font),
        "woff" => ("file", t::file_type_web_font),
        "woff2" => ("file", t::file_type_web_font_2),
        "eot" => ("file", t::file_type_embedded_opentype),
        "ics" => ("file", t::file_type_calendar_file),
        "vcf" => ("file", t::file_type_vcard_contact),
        "torrent" => ("file", t::file_type_torrent_file),
        "xdc" => ("gift", t::file_type_mini_app),
        _ => ("file-unknown", t::file_type_unknown_file),
    };
    (icon, said())
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
        let (hrp, profile) = if b[i..].starts_with(b"npub1") {
            (5, false)
        } else if b[i..].starts_with(b"nprofile1") {
            (9, true)
        } else {
            i += 1;
            continue;
        };
        // Far past any real nprofile; an endless run would be rescanned from every match in it.
        let len = b[i + hrp..].iter().take(4096).take_while(|&&c| body(c)).count();
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

    #[test]
    fn bech_people_in_text_beyond_ascii() {
        let npub = "npub1v84qsqlcs56j8dmh6s22eccnpn2d87fdutxhl7rftseh67wzamhqfv9ft6";
        let text = format!("東京で café 👍🏽 @{npub} ｗ");
        let found = find_bech_people(&text);
        assert_eq!(found.len(), 1);
        assert_eq!(&text[found[0].0..found[0].1], format!("@{npub}"));
    }
}

#[derive(Clone, Debug)]
pub struct PivxPay {
    pub gift_code: String,
    pub amount_piv: f64,
    pub address: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PivxPhase {
    Syncing,
    Claimable,
    Claiming,
    Claimed,
    Failed,
}
