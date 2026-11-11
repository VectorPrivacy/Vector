//! The `@mention` and `#channel` autocompletes (`mentions.js`, `MentionPopup.svelte`), and the
//! send-time swap of each picked `@Name` for what it names.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, FontWeight, IntoElement, ParentElement, SharedString, Styled, StyledImage,
    Window, div, img, px, svg,
};
use serde::Deserialize;
use vector_core::SerializableChat;

use crate::data::Store;
use crate::shortcodes::{Row, panel};
use crate::theme::*;

/// A Community's taggable Roles and who may ping everyone (`get_community_mentions`).
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct Rules {
    pub anyone: bool,
    pub pingers: Vec<String>,
    pub roles: Vec<Role>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct Role {
    pub id: String,
    pub name: String,
    pub color: Option<u32>,
    pub mentionable: bool,
}

#[derive(Clone)]
pub enum Kind {
    Member { avatar: Option<std::path::PathBuf> },
    Role(Option<u32>),
    Everyone,
    Channel { voice: bool, url: String },
}

#[derive(Clone)]
pub struct Item {
    /// What the draft's `@name` becomes on send: an npub, `role:<id>` or `everyone`.
    pub npub: String,
    pub name: String,
    /// The real name behind a nickname, for matching.
    pub search: String,
    pub kind: Kind,
}

pub struct Popup {
    /// The `@` or `#`'s offset in the draft.
    pub start: usize,
    pub items: Vec<Item>,
    pub active: usize,
}

/// The `sigil` the caret is typing after: at the start or after whitespace, no space since.
pub fn trigger(text: &str, caret: usize, sigil: char) -> Option<(usize, &str)> {
    let before = text.get(..caret)?;
    for (i, ch) in before.char_indices().rev() {
        if ch == sigil {
            return before[..i]
                .chars()
                .next_back()
                .is_none_or(char::is_whitespace)
                .then(|| (i, &before[i + 1..]));
        }
        if ch.is_whitespace() {
            return None;
        }
    }
    None
}

/// `getMentionCandidates`: who in this chat can be tagged, most recently active first, then
/// the Roles and @everyone a Community lets me use.
pub fn candidates(
    store: &Store,
    chat: &SerializableChat,
    roster: &[String],
    rules: Option<&Rules>,
) -> Vec<Item> {
    let me = store.my_npub.as_str();
    let community = store.community_of(&chat.id);
    let mut last: HashMap<&str, u64> = HashMap::new();
    for m in chat.messages.iter().rev() {
        let sender = match (&m.npub, m.mine) {
            (_, true) => me,
            (Some(n), _) => n.as_str(),
            _ => chat.id.as_str(),
        };
        last.entry(sender).or_insert(m.at);
    }
    let mut npubs: Vec<&str> = chat.participants.iter().map(String::as_str).collect();
    if community.is_some() {
        npubs.extend(last.keys().copied());
        npubs.extend(roster.iter().map(String::as_str));
        npubs.extend(
            community
                .map(|c| c.members.iter().map(String::as_str))
                .into_iter()
                .flatten(),
        );
    }
    let mut seen = std::collections::HashSet::new();
    npubs.retain(|n| n.starts_with("npub1") && *n != me && seen.insert(*n));
    let mut out: Vec<(u64, Item)> = npubs
        .into_iter()
        .map(|npub| {
            let shown = store.chat_name(npub, &chat.id);
            let real = store.name(npub);
            (
                last.get(npub).copied().unwrap_or(0),
                Item {
                    npub: npub.to_string(),
                    search: if real != shown { real } else { String::new() },
                    name: shown,
                    kind: Kind::Member {
                        avatar: store.avatar_path(npub),
                    },
                },
            )
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out: Vec<Item> = out.into_iter().map(|(_, i)| i).collect();
    let mut count: HashMap<String, usize> = HashMap::new();
    for i in &out {
        *count.entry(i.name.clone()).or_default() += 1;
    }
    for i in &mut out {
        if count[&i.name] > 1 {
            i.name = format!("{} (~{})", i.name, &i.npub[5..9]);
        }
    }
    if let Some(c) = community {
        let none = Rules::default();
        let rules = rules.unwrap_or(&none);
        let all = c.is_owner
            || c.owner_npub.as_deref() == Some(me)
            || rules.anyone
            || rules.pingers.iter().any(|p| p == me);
        for r in rules.roles.iter().filter(|r| all || r.mentionable) {
            out.push(Item {
                npub: format!("role:{}", r.id),
                name: r.name.clone(),
                search: String::new(),
                kind: Kind::Role(r.color),
            });
        }
        if all {
            out.push(Item {
                npub: "everyone".into(),
                name: "everyone".into(),
                search: String::new(),
                kind: Kind::Everyone,
            });
        }
    }
    out
}

/// The open Community's channels a `#` can name, as links.
pub fn channels(store: &Store, chat: &str) -> Vec<Item> {
    let Some(c) = store.community_of(chat) else {
        return Vec::new();
    };
    c.channels
        .iter()
        .map(|ch| Item {
            npub: ch.channel_id.clone(),
            name: if ch.name.is_empty() {
                "channel".into()
            } else {
                ch.name.clone()
            },
            search: String::new(),
            kind: Kind::Channel {
                voice: ch.view == "voice",
                url: vector_core::golink::GoLink::Channel {
                    community: c.community_id.clone(),
                    channel: ch.channel_id.clone(),
                }
                .url(),
            },
        })
        .collect()
}

/// Name prefix, then substring, then (for people only) the npub; at most `max`.
pub fn filter(pool: &[Item], query: &str, max: usize) -> Vec<Item> {
    let q = query.to_lowercase();
    let tier = |c: &Item| {
        let n = c.name.to_lowercase();
        let s = c.search.to_lowercase();
        if q.is_empty() || n.starts_with(&q) || (!s.is_empty() && s.starts_with(&q)) {
            0
        } else if n.contains(&q) || (!s.is_empty() && s.contains(&q)) {
            1
        } else if matches!(c.kind, Kind::Member { .. }) && c.npub.to_lowercase().contains(&q) {
            2
        } else {
            3
        }
    };
    let mut v: Vec<(u8, &Item)> = pool
        .iter()
        .map(|c| (tier(c), c))
        .filter(|(t, _)| *t < 3)
        .collect();
    v.sort_by_key(|(t, _)| *t);
    v.into_iter().take(max).map(|(_, c)| c.clone()).collect()
}

/// What a pick puts in place of `@query` or `#query`.
pub fn insert(item: &Item) -> String {
    match &item.kind {
        Kind::Channel { url, .. } => format!("{url} "),
        _ => format!("@{} ", item.name),
    }
}

/// Every tracked `@Name` standing alone becomes `@<npub>`, longest name first.
pub fn resolve(text: &str, tracked: &[(String, String)]) -> String {
    let mut sorted: Vec<&(String, String)> = tracked.iter().collect();
    sorted.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
    let mut out = text.to_string();
    for (name, npub) in sorted {
        let needle = format!("@{name}");
        let mut res = String::with_capacity(out.len());
        let mut rest = out.as_str();
        let mut prev: Option<char> = None;
        while let Some(at) = rest.find(&needle) {
            let before = if at == 0 {
                prev
            } else {
                rest[..at].chars().next_back()
            };
            let after = rest[at + needle.len()..].chars().next();
            let opens = before.is_none_or(|c| c.is_whitespace() || c == '>');
            let closes = after.is_none_or(|c| c.is_whitespace() || ".,!?;:<".contains(c));
            res.push_str(&rest[..at]);
            if opens && closes {
                res.push('@');
                res.push_str(npub);
            } else {
                res.push_str(&needle);
            }
            prev = needle.chars().next_back();
            rest = &rest[at + needle.len()..];
        }
        res.push_str(rest);
        out = res;
    }
    out
}

pub fn view(p: &Popup, pick: Rc<dyn Fn(usize, &mut Window, &mut App)>) -> AnyElement {
    let header = match p.items.first().map(|i| &i.kind) {
        Some(Kind::Channel { .. }) => "Channels",
        _ => "Members",
    };
    let rows = p
        .items
        .iter()
        .map(|item| {
            let (art, hint): (AnyElement, Option<&str>) = match &item.kind {
                Kind::Member { avatar } => (
                    img(avatar.clone().map_or_else(
                        || gpui::ImageSource::from("icons/user-placeholder.svg"),
                        gpui::ImageSource::from,
                    ))
                    .size(px(30.))
                    .flex_shrink_0()
                    .rounded_full()
                    .border(px(1.5))
                    .border_color(white_a(0.1))
                    .object_fit(gpui::ObjectFit::Cover)
                    .into_any_element(),
                    None,
                ),
                Kind::Everyone => (
                    img("icons/user-placeholder.svg")
                        .size(px(30.))
                        .flex_shrink_0()
                        .rounded_full()
                        .border(px(1.5))
                        .border_color(white_a(0.1))
                        .into_any_element(),
                    None,
                ),
                Kind::Role(color) => {
                    let tint = color.map_or(c(PRIMARY), |c| gpui::rgb(c & 0xffffff).into());
                    (
                        div()
                            .size(px(28.))
                            .flex_shrink_0()
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(tint)
                            .bg(tint.opacity(0.16))
                            .child("@")
                            .into_any_element(),
                        Some("Role"),
                    )
                }
                Kind::Channel { voice, .. } => (
                    div()
                        .size(px(28.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            svg()
                                .path(if *voice {
                                    "icons/volume-max.svg"
                                } else {
                                    "icons/channel-hash.svg"
                                })
                                .size(px(18.))
                                .text_color(c(0xb2b2b2)),
                        )
                        .into_any_element(),
                    Some(if *voice { "Voice" } else { "Text" }),
                ),
            };
            Row {
                art,
                label: SharedString::from(item.name.clone()),
                hint: hint.map(SharedString::from),
            }
        })
        .collect();
    panel(
        "mention-selector",
        header.into(),
        rows,
        p.active,
        342.,
        pick,
    )
}

#[cfg(test)]
mod tests {
    use super::{resolve, trigger};

    #[test]
    fn an_at_opens_only_after_space_or_at_the_start() {
        assert_eq!(trigger("@al", 3, '@'), Some((0, "al")));
        assert_eq!(trigger("hi @al", 6, '@'), Some((3, "al")));
        assert_eq!(trigger("me@al", 5, '@'), None);
        assert_eq!(trigger("hi @al x", 8, '@'), None);
    }

    #[test]
    fn a_picked_name_sends_as_its_npub_longest_first() {
        let t = vec![
            ("Al".to_string(), "npub1al".to_string()),
            ("Alice".to_string(), "npub1alice".to_string()),
        ];
        assert_eq!(
            resolve("hey @Alice and @Al, @Alfred", &t),
            "hey @npub1alice and @npub1al, @Alfred"
        );
        assert_eq!(resolve("x@Al", &t), "x@Al");
    }
}
