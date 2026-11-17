//! What a row's badge says (`computeRowBadgeCount`, `countPingMessages`, `foldCommunity`):
//! a resolved level of 0 counts everything unread, 1 only pings in a Community, 2 nothing.

use vector_core::chat::{ChatType, SerializableChat};

use crate::data::Store;
use crate::mentions::Rules;

fn is_group(chat: &SerializableChat) -> bool {
    matches!(chat.chat_type, ChatType::Community)
}

/// `mayMentionEveryone`: the Community's rule once read, else its owner.
fn may_mention_everyone(store: &Store, chat: &SerializableChat, rules: Option<&Rules>, npub: &str) -> bool {
    match rules {
        Some(r) => r.anyone || r.pingers.iter().any(|p| p == npub),
        None => store.community_of(&chat.id).and_then(|c| c.owner_npub.as_deref()) == Some(npub),
    }
}

fn sender_muted(store: &Store, npub: &str) -> bool {
    store.chat(npub).is_some_and(|c| c.muted)
}

/// `countPingMessages`: unread messages that tag me, an allowed `@everyone`, or a Role I hold.
pub fn count(store: &Store, chat: &SerializableChat) -> usize {
    let group = is_group(chat);
    let rules = store.community_of(&chat.id).and_then(|c| store.mention_rules.get(&c.community_id));
    let me = store.my_npub.as_str();
    let from = chat.unread_from.as_str();
    let msgs = &chat.messages;
    let mut pings = 0;
    for i in (0..msgs.len()).rev() {
        let m = &msgs[i];
        // Past the chosen message of a "Mark as Unread" nothing is unread.
        if !from.is_empty() && i + 1 < msgs.len() && msgs[i + 1].id == from {
            break;
        }
        if m.mine {
            if from.is_empty() {
                break;
            }
            continue;
        }
        if from.is_empty() && !chat.last_read.is_empty() && m.id == chat.last_read {
            break;
        }
        let sender = m.npub.as_deref().unwrap_or_default();
        if group && !sender.is_empty() {
            if store.profiles.get(sender).is_some_and(|p| p.is_blocked) || sender_muted(store, sender) {
                continue;
            }
        }
        if m.content.is_empty() {
            continue;
        }
        let text = vector_core::types::outside_code(&m.content);
        let mentioned = !me.is_empty() && text.contains(&format!("@{me}"));
        let everyone = group
            && chat.everyone_pings
            && text.match_indices("@everyone").any(|(i, _)| {
                !text[i + 9..].chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_')
            })
            && may_mention_everyone(store, chat, rules, sender);
        let role = group
            && rules.is_some_and(|r| {
                let by_anyone = may_mention_everyone(store, chat, Some(r), sender);
                r.roles.iter().any(|role| {
                    role.held
                        && !role.name.is_empty()
                        && m.role_mentions.contains(&role.id)
                        && text.contains(&format!("@{}", role.name))
                        && (role.mentionable || by_anyone)
                })
            });
        if mentioned || everyone || role {
            pings += 1;
        }
    }
    pings
}

/// `computeRowBadgeCount`: the number a row may say out loud at its resolved level.
pub fn badge(store: &Store, chat: &SerializableChat) -> usize {
    let mut ring = chat.notify;
    if chat.muted && ring < 1 {
        ring = 1;
    }
    match ring {
        0 => match &store.unread {
            Some(counts) => counts.get(&chat.id).copied().unwrap_or(0) as usize,
            None => crate::data::unread_count(chat),
        },
        1 if is_group(chat) => count(store, chat),
        _ => 0,
    }
}

/// `computeCommunityPingCount`: every channel's pings, whatever its level.
pub fn community(store: &Store, community_id: &str) -> usize {
    store
        .community(community_id)
        .map(|c| {
            c.channels
                .iter()
                .filter_map(|ch| store.chat(&ch.channel_id))
                .map(|chat| count(store, chat))
                .sum()
        })
        .unwrap_or(0)
}

/// `countText`: past 99 the pill stops counting.
pub fn text(n: usize) -> String {
    if n > 99 { "99+".into() } else { n.to_string() }
}

/// `.chatlist-contact-count`: a row's badge.
pub fn row_pill(n: usize) -> gpui::Div {
    use gpui::{FontWeight, ParentElement, Styled, div, px};
    use crate::theme::{PRIMARY, c};
    div()
        .flex_shrink_0()
        .min_w(px(16.))
        .px(px(5.5))
        .py(px(2.5))
        .rounded_full()
        .bg(c(PRIMARY))
        .text_color(c(0x000000))
        .text_size(px(11.))
        .font_weight(FontWeight::BOLD)
        .line_height(px(11.))
        .text_center()
        .child(text(n))
}

/// `.chatlist-channel-badge`: a channel's pings.
pub fn channel_pill(n: usize) -> gpui::Div {
    use gpui::{FontWeight, ParentElement, Styled, div, px};
    use crate::theme::{PRIMARY, c};
    div()
        .flex_shrink_0()
        .min_w(px(18.))
        .px(px(5.))
        .rounded(px(9.))
        .bg(c(PRIMARY))
        .text_color(c(0x000000))
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .line_height(px(18.))
        .text_center()
        .child(text(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vector_core::types::Message;

    fn msg(id: &str, npub: &str, content: &str, mine: bool) -> Message {
        Message { id: id.into(), npub: Some(npub.into()), content: content.into(), mine, ..Default::default() }
    }

    #[test]
    fn counts_back_to_last_read_and_skips_code() {
        let mut store = Store::from_fixtures(std::path::Path::new(crate::FIXTURES)).unwrap();
        store.my_npub = "npub1me".into();
        let mut chat = store.chats.iter().find(|c| c.chat_type == ChatType::DirectMessage).unwrap().clone();
        store.unread = None;
        chat.unread_from.clear();
        chat.muted = false;
        chat.messages = vec![
            msg("a", "npub1bob", "@npub1me old", false),
            msg("b", "npub1bob", "hey @npub1me", false),
            msg("c", "npub1bob", "`@npub1me` quoted", false),
            msg("d", "npub1bob", "and @npub1me again", false),
        ];
        chat.last_read = "a".into();
        assert_eq!(count(&store, &chat), 2);
        chat.notify = 0;
        assert_eq!(badge(&store, &chat), 3);
        chat.notify = 1;
        assert_eq!(badge(&store, &chat), 0, "a DM has no ping tier");
        chat.notify = 2;
        assert_eq!(badge(&store, &chat), 0);
    }
}
