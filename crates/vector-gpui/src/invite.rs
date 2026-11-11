//! Invite People (`community/InviteModal.svelte` + `InviteLinks.svelte`): the Community's
//! invite links, then a contact picker for direct invites behind a Done / Invite N button.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{div, px, svg, App, Entity, FontWeight, SharedString, Window};

use crate::composer::TextInput;
use crate::data::Store;
use crate::theme::*;

pub struct Link {
    pub url: String,
    pub token: String,
    pub label: Option<String>,
    pub joins: u64,
}

pub struct Invite {
    pub community: String,
    pub name: String,
    pub links: Vec<Link>,
    pub public: bool,
    pub others: Vec<(String, usize)>,
    pub expanded: bool,
    pub copied: Option<String>,
    pub revoking: Option<String>,
    pub creating: bool,
    /// A link change or an invite is in flight: nothing else may start, nor the panel close.
    pub busy: bool,
    pub status: Option<(String, bool)>,
    pub filter: Entity<TextInput>,
    pub picked: HashSet<String>,
    pub contacts: Option<HashSet<String>>,
    /// Already inside, or banned: never listed.
    pub excluded: HashSet<String>,
}

pub enum Act {
    Close,
    Copy(String),
    Revoke(String),
    Create,
    Expand,
    Toggle(String),
    Cta,
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

fn click(on: &OnAct, act: impl Fn() -> Act + 'static) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| on(act(), w, cx)
}

fn section_head(icon: &'static str, title: impl IntoElement, desc: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_start()
        .gap(px(10.))
        .child(svg().path(icon).mt(px(4.)).size(px(17.)).flex_shrink_0().text_color(c(ACCENT)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(div().flex().items_center().gap(px(8.)).font_weight(FontWeight::BOLD).text_size(px(14.)).line_height(px(24.)).text_color(c(TEXT)).child(title))
                .child(div().text_size(px(12.)).line_height(px(17.4)).text_color(c(TEXT)).opacity(0.45).child(desc)),
        )
}

fn icon_btn(id: SharedString, icon: &'static str, live: bool, on: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static) -> impl IntoElement {
    div()
        .id(id)
        .w(px(38.4))
        .h(px(30.))
        .flex_shrink_0()
        .rounded(px(7.))
        .bg(white_a(0.06))
        .flex()
        .items_center()
        .justify_center()
        .child(svg().path(icon).size(px(16.)).text_color(c(0xffffff)))
        .when(!live, |d| d.opacity(0.45))
        .when(live, |d| d.cursor_pointer().hover(|d| d.bg(white_a(0.1))).on_click(on))
}

fn links(m: &Invite, store: &Store, on: &OnAct) -> impl IntoElement {
    let pill = div()
        .h(px(26.))
        .px(px(8.))
        .rounded_full()
        .flex()
        .items_center()
        .font_weight(FontWeight::BOLD)
        .text_size(px(10.))
        .map(|d| if m.public { d.bg(ca(89, 252, 179, 0.18)).text_color(c(ACCENT)) } else { d.bg(white_a(0.08)).text_color(white_a(0.6)) })
        .child(crate::kit::spaced(if m.public { "PUBLIC" } else { "PRIVATE" }, 0.6));
    let others_total: usize = m.others.iter().map(|o| o.1).sum();
    let others = (!m.others.is_empty()).then(|| {
        let collapsed = m.others.len() > 1 && !m.expanded;
        div()
            .mb(px(10.))
            .rounded(px(8.))
            .when(!collapsed, |d| d.px(px(10.)).py(px(8.)).bg(white_a(0.04)))
            .when(collapsed, |d| d.pl(px(27.)))
            .text_size(px(12.5))
            .line_height(px(18.))
            .map(|d| {
                if collapsed {
                    d.child(
                        div()
                            .id("cmt-others")
                            .cursor_pointer()
                            .text_color(c(ACCENT))
                            .hover(|d| d.underline())
                            .on_click(click(on, || Act::Expand))
                            .child(format!("View {others_total} other invite{}", if others_total == 1 { "" } else { "s" })),
                    )
                } else {
                    d.children(m.others.iter().map(|(npub, n)| {
                        div().py(px(1.)).text_color(white_a(0.6)).child(format!("{} has {n} active invite link{}", store.name(npub), if *n == 1 { "" } else { "s" }))
                    }))
                }
            })
    });
    let empty = m.links.is_empty().then(|| {
        div().mb(px(11.)).p(px(2.)).text_size(px(12.5)).line_height(px(18.)).text_color(white_a(0.4)).child(if m.others.is_empty() {
            "No active links yet. Create one to start inviting."
        } else {
            "You have no links of your own yet. Create one to start inviting."
        })
    });
    let live = !m.busy;
    div()
        .flex()
        .flex_col()
        .child(section_head("icons/share.svg", div().flex().items_center().gap(px(8.)).child("Invite Links").child(pill), "Anyone with a link can join. Revoke every link to go private again."))
        .child(
            div().mt(px(13.)).flex().flex_col().children(others).children(empty).children(m.links.iter().map(|link| {
                let label = link.label.clone().filter(|l| !l.trim().is_empty()).unwrap_or_else(|| {
                    let tail: String = if link.token.is_empty() { &link.url } else { &link.token }.chars().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect();
                    format!("Invite · {tail}")
                });
                let copied = m.copied.as_deref() == Some(link.token.as_str());
                let revoking = m.revoking.as_deref() == Some(link.token.as_str());
                let (tok_c, tok_r) = (link.token.clone(), link.token.clone());
                div()
                    .h(px(46.))
                    .mb(px(8.))
                    .pl(px(13.))
                    .pr(px(8.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(white_a(0.06))
                    .bg(ca(0, 0, 0, 0.35))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(7.))
                            .text_size(px(13.))
                            .line_height(px(24.))
                            .text_color(c(TEXT))
                            .opacity(0.9)
                            .child(svg().path("icons/share.svg").size(px(14.)).flex_shrink_0().text_color(c(ACCENT)).opacity(0.85))
                            .child(div().min_w_0().truncate().child(label)),
                    )
                    .child(
                        div()
                            .mr(px(6.))
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .text_size(px(12.))
                            .line_height(px(24.))
                            .text_color(c(TEXT))
                            .opacity(0.7)
                            .child(svg().path("icons/users-multi.svg").size(px(12.)).text_color(c(TEXT)))
                            .child(link.joins.to_string()),
                    )
                    .child(icon_btn(SharedString::from(format!("cmt-copy-{}", link.token)), if copied { "icons/check.svg" } else { "icons/copy.svg" }, live, click(on, move || Act::Copy(tok_c.clone()))))
                    .child(icon_btn(SharedString::from(format!("cmt-revoke-{}", link.token)), if revoking { "icons/loading.svg" } else { "icons/trash.svg" }, live, click(on, move || Act::Revoke(tok_r.clone()))))
            })),
        )
        .child(
            div()
                .id("cmt-create-link")
                .h(px(39.))
                .rounded(px(9.))
                .border_1()
                .border_color(ca(89, 252, 179, 0.28))
                .bg(ca(89, 252, 179, 0.12))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(7.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(14.))
                .text_color(c(ACCENT))
                .when(!live, |d| d.opacity(0.5))
                .when(live, |d| d.cursor_pointer().hover(|d| d.bg(ca(89, 252, 179, 0.18))).on_click(click(on, || Act::Create)))
                .child(svg().path(if m.creating { "icons/loading.svg" } else { "icons/plus.svg" }).size(px(14.)).text_color(c(ACCENT)))
                .child(if m.creating { "Creating…" } else { "Create invite link" }),
        )
}

pub fn view(m: &Invite, store: &Store, filter: &str, on: &OnAct) -> impl IntoElement + use<> {
    let people = crate::createcommunity::listed(store, m.contacts.as_ref(), filter, &m.picked, |n| m.excluded.contains(n));
    let n = m.picked.len();
    let close = on.clone();
    let cta_live = !m.busy;
    let card = div()
        .id("cmt-modal")
        .w(px(530.))
        .max_h(gpui::relative(1.))
        .flex()
        .flex_col()
        .pt(px(23.))
        .pb(px(23.))
        .px(px(25.))
        .rounded(px(12.))
        .border_1()
        .border_color(white_a(0.08))
        .bg(c(0x0a0a0a))
        .occlude()
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(
            div()
                .h(px(46.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(13.))
                .child(div().size(px(46.)).flex_shrink_0().rounded(px(13.)).bg(ca(89, 252, 179, 0.15)).flex().items_center().justify_center().child(svg().path("icons/users-multi.svg").size(px(24.)).text_color(c(ACCENT))))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().truncate().font_weight(FontWeight::BOLD).text_size(px(18.)).line_height(px(19.8)).text_color(c(TEXT)).child(format!("Invite to {}", m.name)))
                        .child(div().text_size(px(13.)).line_height(px(16.9)).text_color(c(TEXT)).opacity(0.5).child("Bring people into your community.")),
                )
                .child(
                    div()
                        .id("cmt-close")
                        .size(px(32.))
                        .self_start()
                        .rounded(px(6.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(24.))
                        .text_color(white_a(0.6))
                        .when(!m.busy, |d| d.cursor_pointer().hover(|d| d.text_color(c(0xffffff))).on_click(click(on, || Act::Close)))
                        .child("×"),
                ),
        )
        .child(
            div()
                .id("cmt-body")
                .mt(px(20.))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .child(links(m, store, on))
                .child(
                    div()
                        .mt(px(18.))
                        .flex()
                        .flex_col()
                        .child(section_head("icons/add-user.svg", "Direct Invites", "Pick contacts to invite, or paste an npub to add someone new."))
                        .child(
                            div()
                                .mt(px(13.))
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
                                .child(svg().path("icons/search.svg").absolute().left(px(12.)).top(px(8.)).size(px(24.)).text_color(white_a(0.4)))
                                .child(div().flex_1().min_w_0().child(m.filter.clone())),
                        )
                        .child(div().mt(px(8.)).mb(px(12.)).flex().flex_col().children(people.into_iter().map(|npub| {
                            let id = npub.clone();
                            crate::createcommunity::pick_row(store, &npub, m.picked.contains(&npub), true, click(on, move || Act::Toggle(id.clone())))
                        }))),
                ),
        )
        .child(
            div()
                .flex_shrink_0()
                .pt(px(10.))
                .children(m.status.clone().map(|(text, error)| {
                    div().mx(px(2.)).mt(px(4.)).mb(px(10.)).text_size(px(13.)).line_height(px(18.)).text_color(if error { c(crate::timeline::DANGER) } else { c(ACCENT) }).child(text)
                }))
                .child(
                    div()
                        .id("cmt-cta")
                        .h(px(37.))
                        .rounded(px(9.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(7.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(14.))
                        .map(|d| if n > 0 { d.bg(c(ACCENT)).text_color(c(0x000000)) } else { d.bg(white_a(0.06)).text_color(c(0xffffff)) })
                        .when(!cta_live, |d| d.opacity(0.5))
                        .when(cta_live, |d| d.cursor_pointer().hover(|d| d.opacity(0.9)).on_click(click(on, || Act::Cta)))
                        .when(n > 0, |d| d.child(svg().path("icons/send.svg").size(px(15.)).text_color(c(0x000000))).child(format!("Invite {n}")))
                        .when(n == 0, |d| d.child("Done")),
                ),
        );
    let busy = m.busy;
    gpui::deferred(
        div()
            .id("cmt-overlay")
            .absolute()
            .inset_0()
            .bg(ca(0, 0, 0, 0.72))
            .flex()
            .items_center()
            .justify_center()
            .py(px(37.))
            .px(px(16.))
            .occlude()
            .on_click(move |_, w, cx| {
                if !busy {
                    close(Act::Close, w, cx)
                }
            })
            .child(card),
    )
    .with_priority(13)
}
