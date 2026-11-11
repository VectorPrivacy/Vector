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
}

pub enum Act {
    Toggle(String),
    Start,
    Cancel,
    Hub,
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

/// `extractNpub`: a bare npub, or a vectorapp.io profile link to one.
pub fn extract_npub(input: &str) -> Option<String> {
    let t = input.trim();
    let valid = |s: &str| s.len() == 63 && s[..5].eq_ignore_ascii_case("npub1") && s[5..].bytes().all(|b| b.is_ascii_alphanumeric());
    if valid(t) {
        return Some(t.to_ascii_lowercase());
    }
    let lower = t.to_ascii_lowercase();
    for scheme in ["https://vectorapp.io/profile/", "http://vectorapp.io/profile/"] {
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
    [&p.nickname, &p.name, &p.display_name].into_iter().find(|n| !n.is_empty()).map(|n| n.to_string())
}

fn display(store: &Store, npub: &str) -> String {
    name_of(store, npub).unwrap_or_else(|| format!("{}...{}", &npub[..10.min(npub.len())], &npub[npub.len().saturating_sub(6)..]))
}

/// Strangers first, then contacts: the picked one, the most recent conversation, then by name.
fn rows(n: &NewChat, store: &Store, filter: &str) -> Vec<String> {
    let f = filter.trim().to_lowercase();
    let mut out: Vec<String> = n.strangers.iter().filter(|s| n.picked.as_ref() == Some(*s)).cloned().collect();
    let activity = |id: &str| store.chat(id).map(crate::data::last_activity).unwrap_or(0);
    let mut contacts: Vec<&str> = store
        .profiles
        .values()
        .filter(|p| p.id != store.my_npub && !p.is_blocked)
        .filter(|p| n.contacts.as_ref().is_none_or(|c| c.contains(p.id.as_str())))
        .filter(|p| f.is_empty() || format!("{} {}", name_of(store, &p.id).unwrap_or_default(), p.id).to_lowercase().contains(&f))
        .map(|p| p.id.as_str())
        .filter(|id| !out.iter().any(|o| o == id))
        .collect();
    contacts.sort_by(|a, b| {
        let (sa, sb) = (n.picked.as_deref() == Some(*a), n.picked.as_deref() == Some(*b));
        sb.cmp(&sa).then_with(|| activity(b).cmp(&activity(a))).then_with(|| display(store, a).cmp(&display(store, b)))
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
    let ring = div().size(px(22.)).flex_shrink_0().rounded_full().border_2();
    if !on {
        return ring.border_color(white_a(0.28));
    }
    ring.border_color(c(PRIMARY)).bg(c(0x000000)).p(px(3.)).child(div().size_full().rounded_full().bg(c(PRIMARY)))
}

pub fn view(n: &NewChat, store: &Store, width: f32, on: &OnAct, cx: &App) -> AnyElement {
    let gutter = crate::kit::page_gutter(width);
    let filter_text = n.filter.read(cx).text().to_string();
    let list = rows(n, store, &filter_text);
    let click = |a: fn() -> Act| {
        let on = on.clone();
        move |_: &gpui::ClickEvent, w: &mut Window, cx: &mut App| on(a(), w, cx)
    };
    let body: AnyElement = if list.is_empty() {
        let mut empty = div().pt(px(6.)).pb(px(40.)).flex().flex_col().items_center().text_size(px(14.)).text_color(white_a(0.5)).child(
            if filter_text.trim().is_empty() { "No contacts yet. Paste an npub to invite someone." } else { "No matches." },
        );
        if filter_text.trim().is_empty() {
            empty = empty.child(
                div()
                    .flex()
                    .child(div().id("cp-hub").cursor_pointer().text_color(c(ACCENT)).on_click(click(|| Act::Hub)).child("Explore the Vector Hub"))
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
                    .child(div().mr(px(10.)).child(crate::ui::avatar(store, &npub, 25.)))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(14.)).line_height(px(24.)).text_color(c(0xf7f4f4)).child(display(store, &npub)))
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
                        .child(div().w(px(50.)).h(px(30.)).flex().justify_center().opacity(0.7).child(svg().path("icons/chats.svg").size(px(30.)).text_color(c(0xffffff))))
                        .child(crate::kit::spaced("New Message", 1.)),
                )
                .child(div().mt(px(8.)).mx(px(40.)).h(px(1.)).flex_shrink_0().bg(c(ACCENT)))
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
                        .child(svg().path("icons/search.svg").absolute().left(px(12.)).top(px(9.)).size(px(24.)).text_color(white_a(0.4)))
                        .child(div().flex_1().min_w_0().child(n.filter.clone())),
                )
                .child(div().id("create-group-list").mt(px(10.)).flex_1().min_h_0().overflow_y_scroll().p(px(6.)).child(body)),
        )
        .child(
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
                        .when(ready, |d| d.cursor_pointer().hover(|s| s.opacity(0.75)).on_click(click(|| Act::Start))),
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
        .into_any_element()
}
