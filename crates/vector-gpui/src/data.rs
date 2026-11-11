//! What the UI shows, in vector-core's own wire types, plus the derivations the Tauri
//! frontend makes from them (names, previews, unread marks, time labels). Each helper names
//! the JS function it mirrors, so a change there has one place to land here.

use std::collections::HashMap;
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
}

impl Store {
    pub fn from_fixtures(path: &Path) -> anyhow::Result<Self> {
        let f: Fixtures = serde_json::from_slice(&std::fs::read(path)?)?;
        let mut chats = f.chats;
        chats.sort_by_key(|c| std::cmp::Reverse(last_activity(c)));
        Ok(Self {
            my_npub: f.npub,
            profiles: f.profiles.into_iter().map(|p| (p.id.clone(), p)).collect(),
            chats,
            media_root: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            frozen_now_ms: Some(f.now),
            unread: None,
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
        let Some(chat) = self.chat_mut(chat_id) else { return false };
        if chat.messages.iter().any(|m| m.id == msg.id) {
            return true;
        }
        let at = chat.messages.partition_point(|m| m.at <= msg.at);
        chat.messages.insert(at, msg);
        self.resort();
        true
    }

    /// A message changed (sent, failed, edited, reacted to): `old_id` is the one it replaces.
    pub fn replace_message(&mut self, chat_id: &str, old_id: &str, msg: vector_core::types::Message) {
        let Some(chat) = self.chat_mut(chat_id) else { return };
        match chat.messages.iter().position(|m| m.id == old_id || m.id == msg.id) {
            Some(i) => chat.messages[i] = msg,
            None => {
                let at = chat.messages.partition_point(|m| m.at <= msg.at);
                chat.messages.insert(at, msg);
            }
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.frozen_now_ms.unwrap_or_else(|| Local::now().timestamp_millis() as u64)
    }

    pub fn chat(&self, id: &str) -> Option<&SerializableChat> {
        self.chats.iter().find(|c| c.id == id)
    }

    pub fn chat_mut(&mut self, id: &str) -> Option<&mut SerializableChat> {
        self.chats.iter_mut().find(|c| c.id == id)
    }

    pub fn resort(&mut self) {
        self.chats.sort_by_key(|c| std::cmp::Reverse(last_activity(c)));
    }

    pub fn me(&self) -> Option<&SlimProfile> {
        self.profiles.get(&self.my_npub)
    }

    /// `getName` (main.js).
    pub fn name(&self, npub: &str) -> String {
        match self.profiles.get(npub) {
            Some(p) if !p.nickname.is_empty() => p.nickname.clone(),
            Some(p) if !p.name.is_empty() => p.name.clone(),
            Some(p) if !p.display_name.is_empty() => p.display_name.clone(),
            _ => format!("{}…", &npub[..npub.len().min(12)]),
        }
    }

    pub fn avatar_path(&self, npub: &str) -> Option<PathBuf> {
        let p = self.profiles.get(npub)?;
        (!p.avatar_cached.is_empty()).then(|| self.media_root.join(&p.avatar_cached))
    }
}

pub fn last_activity(chat: &SerializableChat) -> u64 {
    chat.messages.iter().map(|m| m.at).max().unwrap_or(chat.created_at * 1000)
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
        m.content.lines().next().unwrap_or_default().to_string()
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
    Local.timestamp_millis_opt(ms as i64).single().unwrap_or_else(Local::now)
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
    Date { label: String, time: String },
    Unread,
    /// `first` heads a streak (avatar and header); otherwise a continuation.
    Message { index: usize, first: bool },
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
pub fn timeline(chat: &SerializableChat, now_ms: u64, read_anchor: Option<&str>) -> Vec<Row> {
    let mut rows = Vec::with_capacity(chat.messages.len() + 2);
    let mut last_day = None;
    let read_at = read_anchor.and_then(|a| chat.messages.iter().position(|m| m.id == a));
    for (i, m) in chat.messages.iter().enumerate() {
        let day = local(m.at).date_naive();
        if last_day != Some(day) {
            rows.push(Row::Date { label: day_label(now_ms, m.at), time: clock(m.at) });
            last_day = Some(day);
        }
        if read_at.is_some_and(|r| i == r + 1) && !m.mine {
            rows.push(Row::Unread);
        }
        // As `deriveWindow`: a day divider doesn't end a streak.
        let first = i == 0 || !collapses(&chat.messages[i - 1], m);
        rows.push(Row::Message { index: i, first });
    }
    rows
}
