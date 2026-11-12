//! Vector links in messages as pills naming the place (`golinks.js`): a community, a channel,
//! a message, or a place in the app. What a pill says comes from this device, never the link.

use std::rc::Rc;

use gpui::SharedString;
use vector_core::golink::GoLink;

use crate::data::Store;
use crate::inline::{Look, Part, Pill, Target};

/// The pill for `url`, when it is a Vector link; `here` is the community the message is in.
pub fn pill(store: &Store, url: &str, here: Option<&str>, look: &Look) -> Option<Rc<Pill>> {
    let (parts, tip, dim) = if let Some(link) = GoLink::parse_url(url) {
        go(store, &link, here)
    } else if let Some(path) = place_path(url) {
        let (icon, names) = place_names(store, &path);
        let mut parts = vec![icon];
        for (i, n) in names.iter().enumerate() {
            if i > 0 {
                parts.push(sep());
            }
            parts.push(text(n.clone()));
        }
        (parts, Some(names.join(" › ")), false)
    } else if vector_core::golink::is_go_url(url) || url.starts_with("vector://go") {
        (
            vec![Part::Icon("icons/vector_warning.svg"), text("Broken link".into())],
            Some("This link is broken or incomplete".to_string()),
            true,
        )
    } else {
        return None;
    };
    let mut look = look.clone();
    look.color = if dim {
        crate::theme::white_a(0.55)
    } else {
        gpui::rgb(crate::theme::PRIMARY).into()
    };
    look.weight = if dim { gpui::FontWeight::NORMAL } else { gpui::FontWeight::MEDIUM };
    Some(Rc::new(Pill {
        parts,
        look,
        tip: tip.filter(|t| !t.is_empty()).map(SharedString::from),
        target: Target::Link(url.to_string().into()),
    }))
}

fn text(t: String) -> Part {
    Part::Text { text: t.into(), alpha: 1., strike: false }
}

fn sep() -> Part {
    Part::Text { text: "›".into(), alpha: 0.5, strike: false }
}

fn community_icon(store: &Store, community: &str) -> Part {
    Part::Avatar(match store.icon(community) {
        Some(p) => p.into(),
        None => "icons/group-placeholder.svg".into(),
    })
}

fn go(store: &Store, link: &GoLink, here: Option<&str>) -> (Vec<Part>, Option<String>, bool) {
    let (community, channel, message) = match link {
        GoLink::Community { community } => (Some(community), None, None),
        GoLink::Channel { community, channel } => (Some(community), Some(channel), None),
        GoLink::ChannelMessage { community, channel, message } => (Some(community), Some(channel), Some(message)),
        GoLink::Message { message } => (None, None, Some(message)),
    };
    let Some(cid) = community else {
        return dm_message(store, message.map(String::as_str).unwrap_or(""));
    };
    let Some(com) = store.community(cid) else {
        return (
            vec![Part::Icon("icons/users-multi.svg"), text("Unknown community".into())],
            Some("A community you're not in".into()),
            true,
        );
    };
    let name = if com.name.is_empty() { "Community".to_string() } else { com.name.clone() };
    let Some(chid) = channel else {
        return (vec![community_icon(store, cid), text(name.clone())], Some(name), false);
    };
    let mut parts = Vec::new();
    if here != Some(cid.as_str()) {
        parts.push(community_icon(store, cid));
        parts.push(Part::Text { text: name.clone().into(), alpha: 0.8, strike: false });
        parts.push(sep());
    }
    let Some(ch) = com.channel(chid) else {
        parts.push(Part::Icon("icons/channel-hash.svg"));
        parts.push(text("Unknown channel".into()));
        return (parts, Some("A channel that isn't available to you".into()), true);
    };
    let room = if ch.name.is_empty() { "channel".to_string() } else { ch.name.clone() };
    let voice = ch.is_voice();
    let room_icon = if voice { "icons/volume-max.svg" } else { "icons/channel-hash.svg" };
    let room_ref = if voice { room.clone() } else { format!("#{room}") };
    if !ch.visible() {
        parts.push(Part::Icon("icons/locked.svg"));
        parts.push(text(room));
        return (parts, Some(format!("Private channel in {name}")), true);
    }
    parts.push(Part::Icon(room_icon));
    parts.push(text(room));
    if message.is_some() {
        parts.push(sep());
        parts.push(Part::Icon("icons/chat-bubble.svg"));
        return (parts, Some(format!("Message in {room_ref}, {name}")), false);
    }
    (parts, Some(format!("{room_ref} in {name}")), false)
}

fn dm_message(store: &Store, message: &str) -> (Vec<Part>, Option<String>, bool) {
    let chat = store
        .chats
        .iter()
        .find(|c| c.messages.iter().any(|m| m.id == message));
    match chat {
        Some(c) if store.community_of(&c.id).is_none() => {
            let name = store.name(&c.id);
            (
                vec![text(format!("@{name}")), sep(), Part::Icon("icons/chat-bubble.svg")],
                Some(format!("Message with {name}")),
                false,
            )
        }
        _ => (
            vec![Part::Icon("icons/chat-bubble.svg"), text("Message".into())],
            None,
            false,
        ),
    }
}

/// A whole URL naming a place in the app, as its path (`settings/privacy`).
fn place_path(url: &str) -> Option<String> {
    let a = vector_core::deep_link::parse_deep_link(url)?;
    match a.action_type.as_str() {
        "settings" if a.target.is_empty() => Some("settings".into()),
        "settings" => Some(format!("settings/{}", a.target)),
        "nexus" if a.target.is_empty() => Some("nexus".into()),
        "nexus" => Some(format!("nexus/{}", a.target)),
        "requests" => Some("requests".into()),
        _ => None,
    }
}

fn place_names(store: &Store, path: &str) -> (Part, Vec<String>) {
    let mut it = path.split('/');
    let kind = it.next().unwrap_or("");
    let (first, second) = (it.next(), it.next());
    match kind {
        "nexus" => {
            let names = match first {
                Some(id) => {
                    let name = store
                        .fixture_market
                        .iter()
                        .find(|a| a.id == id)
                        .map(|a| a.name.clone())
                        .unwrap_or_else(|| id.to_string());
                    vec!["App Store".into(), name]
                }
                None => vec!["App Store".into()],
            };
            (Part::Icon("icons/nexus.svg"), names)
        }
        "requests" => (Part::Icon("icons/mail.svg"), vec!["Requests".into()]),
        _ => {
            let cats = crate::settings::categories(&crate::settings::Offers {
                voice: true,
                routing: true,
                updates: true,
            });
            let icon = Part::Icon("icons/settings.svg");
            let first = first.unwrap_or("");
            let direct = cats.iter().find(|c| c.id == first);
            let cat = direct.or_else(|| {
                second
                    .is_none()
                    .then(|| cats.iter().find(|c| c.anchors.iter().any(|a| a.id == first)))
                    .flatten()
            });
            let Some(cat) = cat else {
                return (icon, vec!["Settings".into()]);
            };
            let want = second.unwrap_or(if direct.is_some() { "" } else { first });
            match cat.anchors.iter().find(|a| a.id == want) {
                Some(a) => (icon, vec![cat.label.to_string(), a.label.to_string()]),
                None => (icon, vec![cat.label.to_string()]),
            }
        }
    }
}
