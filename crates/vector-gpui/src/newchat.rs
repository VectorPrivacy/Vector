//! New Message (`community/CreateCommunity.svelte` in chat mode, with `people/ContactPicker`):
//! pick one person you have written to, or paste an npub, and start the chat.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Entity, FontWeight, SharedString, Window, div, px, svg};

use crate::composer::TextInput;
use crate::data::Store;
use crate::theme::*;

pub struct NewChat {
    pub filter: Entity<TextInput>,
    pub picked: Option<String>,
    /// Pasted npubs of people with no profile here yet, listed while picked.
    pub strangers: Vec<String>,
    /// Who is listable: the people this account has written with.
    pub contacts: Option<HashSet<String>>,
    pub joining: bool,
}

/// A pasted invite as it resolves.
#[derive(Clone)]
pub enum Preview {
    Loading,
    Ready(crate::backend::community::InvitePreview),
    Failed,
    Go(vector_core::golink::GoLink),
}

/// A Vector link anywhere in `text`, as `parseContactInput` finds one.
pub fn go_link(text: &str) -> Option<vector_core::golink::GoLink> {
    text.split_whitespace()
        .find_map(vector_core::golink::GoLink::parse_url)
}

/// The invite key (`naddr1…#fragment`) inside any wrapper, as `inviteKey` reads it.
pub fn invite_key(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find("naddr1").map(|i| i + from) {
        let body = &text[at + 6..];
        let n = body
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric())
            .count();
        if n >= 20
            && body[..n]
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            let rest = &body[n..];
            let hash = rest
                .bytes()
                .take_while(|b| *b != b'#' && !b.is_ascii_whitespace())
                .count();
            if rest.as_bytes().get(hash) == Some(&b'#') {
                let frag = &rest[hash + 1..];
                let m = frag
                    .bytes()
                    .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
                    .count();
                if m >= 20 {
                    return Some(format!("naddr1{}#{}", &body[..n], &frag[..m]));
                }
            }
        }
        from = at + 6;
    }
    None
}

/// Each shareable invite link in `text` (any host's `/invite/<naddr>#<key>`, or `vector://invite/`):
/// its byte span and invite key, as `COMMUNITY_INVITE_URL_REGEX` finds them.
pub fn invite_links(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let mut out = Vec::new();
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find("naddr1").map(|i| i + from) {
        from = at + 6;
        let n = lower[at + 6..]
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric())
            .count();
        if n < 20 || lower.as_bytes().get(at + 6 + n) != Some(&b'#') {
            continue;
        }
        let frag_at = at + 7 + n;
        let m = text[frag_at..]
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
            .count();
        if m < 20 {
            continue;
        }
        let token_start = lower[..at]
            .rfind(|ch: char| ch.is_whitespace())
            .map_or(0, |i| i + 1);
        let token = &lower[token_start..at];
        let start = if token.ends_with("vector://invite/") {
            Some(at - "vector://invite/".len())
        } else if token.ends_with("/invite/") {
            ["https://", "http://"]
                .iter()
                .filter_map(|scheme| token.find(scheme))
                .min()
                .filter(|&i| {
                    !token[i..].contains('#')
                        && token[i..]
                            .find("://")
                            .is_some_and(|s| token.len() - i > s + 3 + "/invite/".len())
                })
                .map(|i| token_start + i)
        } else {
            None
        };
        if let Some(start) = start {
            let end = frag_at + m;
            out.push((
                start..end,
                format!("{}#{}", &lower[at..at + 6 + n], &text[frag_at..end]),
            ));
            from = end;
        }
    }
    out
}

/// The distinct invite keys a message shows as cards: the first three.
pub fn invite_card_keys(text: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for (_, k) in invite_links(text) {
        if !keys.contains(&k) {
            keys.push(k);
        }
        if keys.len() == 3 {
            break;
        }
    }
    keys
}

/// `text` with its invite links swapped for `with` (empty strips them, tidying the gaps).
pub fn replace_invite_links<'a>(text: &'a str, with: &str) -> std::borrow::Cow<'a, str> {
    let links = invite_links(text);
    if links.is_empty() {
        return text.into();
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (span, _) in links {
        out.push_str(&text[last..span.start]);
        out.push_str(with);
        last = span.end;
    }
    out.push_str(&text[last..]);
    if !with.is_empty() {
        return out.into();
    }
    let mut tidy = String::with_capacity(out.len());
    let (mut blanks, mut newlines) = (0, 0);
    for ch in out.chars() {
        match ch {
            ' ' | '\t' => {
                blanks += 1;
                newlines = 0;
                if blanks == 1 {
                    tidy.push(ch);
                }
            }
            '\n' => {
                blanks = 0;
                newlines += 1;
                if newlines <= 2 {
                    tidy.push(ch);
                }
            }
            _ => {
                blanks = 0;
                newlines = 0;
                tidy.push(ch);
            }
        }
    }
    tidy.trim().to_string().into()
}

pub fn invite_url(key: &str) -> String {
    format!("https://vectorapp.io/invite/{key}")
}

pub enum Act {
    Toggle(String),
    Start,
    /// Join the pasted invite's Community, or open it when we're already in.
    Join,
    Cancel,
    Hub,
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

/// `extractNpub`: a bare npub, or a vectorapp.io profile link to one.
pub fn extract_npub(input: &str) -> Option<String> {
    let t = input.trim();
    let valid = |s: &str| {
        s.len() == 63
            && s[..5].eq_ignore_ascii_case("npub1")
            && s[5..].bytes().all(|b| b.is_ascii_alphanumeric())
    };
    if valid(t) {
        return Some(t.to_ascii_lowercase());
    }
    let lower = t.to_ascii_lowercase();
    for scheme in [
        "https://vectorapp.io/profile/",
        "http://vectorapp.io/profile/",
    ] {
        if let Some(rest) = lower.strip_prefix(scheme) {
            let id = &rest[..rest.len().min(63)];
            if valid(id) {
                return Some(id.to_string());
            }
        }
    }
    None
}

fn name_of(store: &Store, npub: &str) -> Option<String> {
    let p = store.profiles.get(npub)?;
    [&p.nickname, &p.name, &p.display_name]
        .into_iter()
        .find(|n| !n.is_empty())
        .map(|n| n.to_string())
}

fn display(store: &Store, npub: &str) -> String {
    name_of(store, npub).unwrap_or_else(|| {
        format!(
            "{}...{}",
            &npub[..10.min(npub.len())],
            &npub[npub.len().saturating_sub(6)..]
        )
    })
}

/// Strangers first, then contacts: the picked one, the most recent conversation, then by name.
fn rows(n: &NewChat, store: &Store, filter: &str) -> Vec<String> {
    let f = filter.trim().to_lowercase();
    let mut out: Vec<String> = n
        .strangers
        .iter()
        .filter(|s| n.picked.as_ref() == Some(*s))
        .cloned()
        .collect();
    let activity = |id: &str| store.chat(id).map(crate::data::last_activity).unwrap_or(0);
    let mut contacts: Vec<&str> = store
        .profiles
        .values()
        .filter(|p| p.id != store.my_npub && !p.is_blocked)
        .filter(|p| {
            n.contacts
                .as_ref()
                .is_none_or(|c| c.contains(p.id.as_str()))
        })
        .filter(|p| {
            f.is_empty()
                || format!("{} {}", name_of(store, &p.id).unwrap_or_default(), p.id)
                    .to_lowercase()
                    .contains(&f)
        })
        .map(|p| p.id.as_str())
        .filter(|id| !out.iter().any(|o| o == id))
        .collect();
    contacts.sort_by(|a, b| {
        let (sa, sb) = (
            n.picked.as_deref() == Some(*a),
            n.picked.as_deref() == Some(*b),
        );
        sb.cmp(&sa)
            .then_with(|| activity(b).cmp(&activity(a)))
            .then_with(|| display(store, a).cmp(&display(store, b)))
    });
    out.extend(contacts.into_iter().map(String::from));
    out
}

/// The listed people, in order, for a filter as typed.
pub fn rows_for(n: &NewChat, store: &Store) -> Vec<String> {
    rows(n, store, "")
}

/// `.member-pick-indicator.is-single`: a radio, filled when picked.
fn radio(on: bool) -> gpui::Div {
    let ring = div()
        .size(px(22.))
        .flex_shrink_0()
        .rounded_full()
        .border_2();
    if !on {
        return ring.border_color(white_a(0.28));
    }
    ring.border_color(c(PRIMARY))
        .bg(c(0x000000))
        .p(px(3.))
        .child(div().size_full().rounded_full().bg(c(PRIMARY)))
}

pub fn view(
    n: &NewChat,
    store: &Store,
    width: f32,
    invite: Option<&Preview>,
    on: &OnAct,
    cx: &App,
) -> AnyElement {
    let gutter = crate::kit::page_gutter(width);
    let filter_text = n.filter.read(cx).text().to_string();
    let member = |id: &str| store.community(id).is_some();
    let list = rows(n, store, &filter_text);
    let click = |a: fn() -> Act| {
        let on = on.clone();
        move |_: &gpui::ClickEvent, w: &mut Window, cx: &mut App| on(a(), w, cx)
    };
    let body: AnyElement = if list.is_empty() {
        let mut empty = div()
            .pt(px(6.))
            .pb(px(40.))
            .flex()
            .flex_col()
            .items_center()
            .text_size(px(14.))
            .text_color(white_a(0.5))
            .child(if filter_text.trim().is_empty() {
                "No contacts yet. Paste an npub to invite someone."
            } else {
                "No matches."
            });
        if filter_text.trim().is_empty() {
            empty = empty.child(
                div()
                    .flex()
                    .child(
                        div()
                            .id("cp-hub")
                            .cursor_pointer()
                            .text_color(c(ACCENT))
                            .on_click(click(|| Act::Hub))
                            .child("Explore the Vector Hub"),
                    )
                    .child("\u{a0}to discover communities to join and meet new people."),
            );
        }
        empty.into_any_element()
    } else {
        div()
            .children(list.into_iter().map(|npub| {
                let picked = n.picked.as_ref() == Some(&npub);
                let on = on.clone();
                let id = npub.clone();
                div()
                    .id(SharedString::from(format!("pick-{npub}")))
                    .h(px(35.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|s| s.bg(ca(255, 255, 255, 0.085)))
                    .on_click(move |_, w, cx| on(Act::Toggle(id.clone()), w, cx))
                    .child(
                        div()
                            .mr(px(10.))
                            .child(crate::ui::avatar(store, &npub, 25.)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(14.))
                            .line_height(px(24.))
                            .text_color(c(0xf7f4f4))
                            .child(display(store, &npub)),
                    )
                    .child(radio(picked))
            }))
            .into_any_element()
    };
    let ready = n.picked.is_some();
    let button = |id: &'static str, label: &'static str| {
        div()
            .id(id)
            .flex_1()
            .max_w(px(220.))
            .h(px(41.2))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .border_1()
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(16.))
            .child(label)
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(c(APP_BG))
        .child(
            // .create-group-content
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .pt(px(18.))
                .px(px(gutter))
                .child(
                    div()
                        .mt(px(10.))
                        .h(px(30.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(px(24.))
                        .line_height(px(30.))
                        .child(
                            div()
                                .w(px(50.))
                                .h(px(30.))
                                .flex()
                                .justify_center()
                                .opacity(0.7)
                                .child(
                                    svg()
                                        .path("icons/chats.svg")
                                        .size(px(30.))
                                        .text_color(c(0xffffff)),
                                ),
                        )
                        .child(crate::kit::spaced("New Message", 1.)),
                )
                .child(
                    div()
                        .mt(px(8.))
                        .mx(px(40.))
                        .h(px(1.))
                        .flex_shrink_0()
                        .bg(c(ACCENT)),
                )
                .child(
                    div()
                        .mt(px(28.))
                        .h(px(42.))
                        .flex_shrink_0()
                        .relative()
                        .flex()
                        .items_center()
                        .pl(px(44.))
                        .pr(px(12.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(ca(57, 57, 57, 0.5))
                        .child(
                            svg()
                                .path("icons/search.svg")
                                .absolute()
                                .left(px(12.))
                                .top(px(9.))
                                .size(px(24.))
                                .text_color(white_a(0.4)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .child(n.filter.clone()),
                        ),
                )
                .map(|d| match invite {
                    Some(p) => d.child(link_card(p, member)).child(
                        div()
                            .mt(px(14.))
                            .flex()
                            .justify_center()
                            .gap(px(10.))
                            .child({
                                let ready = matches!(p, Preview::Ready(_) | Preview::Go(_)) && !n.joining;
                                let label = match p {
                                    Preview::Go(_) => "Open Link",
                                    _ if n.joining => "Joining…",
                                    Preview::Ready(i) if member(&i.community_id) => {
                                        "Open Community"
                                    }
                                    _ => "Join Community",
                                };
                                button("create-group-create-btn", label)
                                    .bg(c(0x58fcb3))
                                    .border_color(ca(90, 252, 180, 0.65))
                                    .text_color(c(0x000000))
                                    .when(!ready, |d| d.opacity(0.45))
                                    .when(ready, |d| {
                                        d.cursor_pointer()
                                            .hover(|s| s.opacity(0.75))
                                            .on_click(click(|| Act::Join))
                                    })
                            })
                            .child(
                                button("create-group-cancel", "Cancel")
                                    .border_color(white_a(0.2))
                                    .text_color(c(0xffffff))
                                    .cursor_pointer()
                                    .hover(|s| s.opacity(0.75))
                                    .on_click(click(|| Act::Cancel)),
                            ),
                    ),
                    None => d.child(
                        div()
                            .id("create-group-list")
                            .mt(px(10.))
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .p(px(6.))
                            .child(body),
                    ),
                }),
        )
        .when(invite.is_none(), |d| {
            d.child(
                // .create-group-footer
                div()
                    .flex_shrink_0()
                    .px(px(gutter))
                    .p(px(15.))
                    .pt(px(25.))
                    .flex()
                    .justify_center()
                    .gap(px(10.))
                    .child(
                        button("create-group-create-btn", "Start Chat")
                            .bg(c(0x58fcb3))
                            .border_color(ca(90, 252, 180, 0.65))
                            .text_color(c(0x000000))
                            .when(!ready, |d| d.opacity(0.45))
                            .when(ready, |d| {
                                d.cursor_pointer()
                                    .hover(|s| s.opacity(0.75))
                                    .on_click(click(|| Act::Start))
                            }),
                    )
                    .child(
                        button("create-group-cancel", "Cancel")
                            .border_color(white_a(0.2))
                            .text_color(c(0xffffff))
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.75))
                            .on_click(click(|| Act::Cancel)),
                    ),
            )
        })
        .into_any_element()
}

/// `.cg-link`: the pasted invite's Community, or why it can't be shown.
fn link_card(p: &Preview, member: impl Fn(&str) -> bool) -> impl IntoElement {
    let icon = |path: gpui::ImageSource, pad: f32| {
        div()
            .size(px(88.))
            .flex_shrink_0()
            .overflow_hidden()
            .rounded(px(22.))
            .bg(c(0x0a0a0a))
            .flex()
            .items_center()
            .justify_center()
            .shadow(vec![
                gpui::BoxShadow {
                    color: white_a(0.06),
                    offset: gpui::point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(1.),
                    inset: false,
                },
                gpui::BoxShadow {
                    color: ca(0, 0, 0, 0.5),
                    offset: gpui::point(px(0.), px(10.)),
                    blur_radius: px(28.),
                    spread_radius: px(0.),
                    inset: false,
                },
            ])
            .child(
                gpui::img(path)
                    .size(px(88. - 2. * pad))
                    .rounded(px(22.))
                    .object_fit(if pad > 0. { gpui::ObjectFit::Contain } else { gpui::ObjectFit::Cover }),
            )
    };
    let eyebrow = |t: &'static str| {
        crate::kit::spaced(t.to_uppercase(), 1.1)
            .mb(px(18.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(11.))
            .line_height(px(24.))
            .text_color(c(PRIMARY))
    };
    let name = |t: String| {
        div()
            .mt(px(16.))
            .max_w_full()
            .truncate()
            .font_weight(FontWeight::BOLD)
            .text_size(px(22.))
            .line_height(px(24.))
            .text_color(c(0xffffff))
            .child(t)
    };
    let desc = |t: String| {
        div()
            .mt(px(8.))
            .text_center()
            .text_size(px(14.))
            .line_height(px(21.))
            .text_color(c(0xa8a8a8))
            .line_clamp(4)
            .child(t)
    };
    let card = div()
        .w_full()
        .max_w(px(450.))
        .mx_auto()
        .mt(px(28.))
        .pt(px(36.))
        .pb(px(34.))
        .px(px(24.))
        .flex()
        .flex_col()
        .items_center()
        .rounded(px(16.))
        .border_1()
        .border_color(c(0x262626))
        .bg(gpui::linear_gradient(
            180.,
            gpui::linear_color_stop(c(0x161616), 0.),
            gpui::linear_color_stop(c(0x0d0d0d), 1.),
        ))
        .shadow(vec![gpui::BoxShadow {
            color: ca(0, 0, 0, 0.45),
            offset: gpui::point(px(0.), px(18.)),
            blur_radius: px(50.),
            spread_radius: px(0.),
            inset: false,
        }]);
    match p {
        Preview::Loading => card
            .child(eyebrow("Community Invite"))
            .child(div().size(px(88.)).rounded(px(22.)).bg(white_a(0.06)))
            .child(div().mt(px(18.)).w(gpui::relative(0.58)).h(px(20.)).rounded(px(6.)).bg(white_a(0.06)))
            .child(div().mt(px(12.)).w(gpui::relative(0.82)).h(px(12.)).rounded(px(6.)).bg(white_a(0.06)))
            .child(div().mt(px(8.)).w(gpui::relative(0.5)).h(px(12.)).rounded(px(6.)).bg(white_a(0.06)))
            .child(div().mt(px(18.)).text_size(px(13.)).text_color(white_a(0.55)).child("Loading invite...")),
        Preview::Go(_) => card
            .child(eyebrow("Vector Link"))
            .child(icon("icons/vector-mark.svg".into(), 18.))
            .child(name("Open in Vector".into()))
            .child(desc("This link opens a place inside Vector.".into())),
        Preview::Failed => card
            .child(eyebrow("Community Invite"))
            .child(icon("icons/vector_warning.svg".into(), 18.))
            .child(name("Invite Unavailable".into()))
            .child(desc("This invite couldn't be loaded. It may have been revoked or expired, or its relays are unreachable.".into())),
        Preview::Ready(info) => {
            let joined = member(&info.community_id);
            card.child(eyebrow(if joined { "Your Community" } else { "You've been invited to join" }))
                .child(icon(info.icon.clone().map(|p| std::path::PathBuf::from(p).into()).unwrap_or_else(|| "icons/group-placeholder.svg".into()), 0.))
                .child(name(info.name.clone()))
                .when(!info.description.is_empty(), |d| d.child(desc(info.description.clone())))
                .when(joined, |d| {
                    d.child(
                        div()
                            .mt(px(16.))
                            .py(px(5.))
                            .pl(px(8.))
                            .pr(px(12.))
                            .rounded_full()
                            .bg(ca(89, 252, 179, 0.12))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(12.))
                            .text_color(c(ACCENT))
                            .child(svg().path("icons/check.svg").size(px(14.)).text_color(c(ACCENT)))
                            .child("You're already a member"),
                    )
                })
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn invite_keys_from_any_wrapper() {
        let key = "naddr1qvzqqqr4gupzqmjxss3dld622uu8q#Zk3pQ9vT2xLm8RbN4cWd7YhJ";
        assert_eq!(
            super::invite_key(&format!("https://vectorapp.io/invite/{key}")).as_deref(),
            Some(key)
        );
        assert_eq!(
            super::invite_key(&format!("look: vector://invite/{key} !")).as_deref(),
            Some(key)
        );
        assert_eq!(
            super::invite_key("naddr1short#Zk3pQ9vT2xLm8RbN4cWd7YhJ"),
            None
        );
        assert_eq!(super::invite_key("npub1abc"), None);
    }

    #[test]
    fn invite_links_strip_and_tag() {
        let key = "naddr1qvzqqqr4gupzqmjxss3dld622uu8q#Zk3pQ9vT2xLm8RbN4cWd7YhJ";
        let text = format!("Come  hang https://armada.buzz/invite/{key} out");
        assert_eq!(super::invite_card_keys(&text), vec![key.to_string()]);
        assert_eq!(super::replace_invite_links(&text, ""), "Come hang out");
        assert_eq!(
            super::replace_invite_links(&format!("hi vector://invite/{key}"), "Community Invite"),
            "hi Community Invite"
        );
        assert!(super::invite_links(&format!("bare {key}")).is_empty());
        assert!(super::invite_links(&format!("https://x.io/other/{key}")).is_empty());
    }
}
