//! Create Community (`community/CommunitySetup.svelte`): its name with a live preview, who can
//! join, then the way in — a link to share (Public) or friends to invite (Private). The
//! Community exists from the end of step 2; step 3 only hands out access to it.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Entity, FontWeight, RenderImage, SharedString, Window, div, img, px, svg,
};

use crate::composer::TextInput;
use crate::data::Store;
use crate::theme::*;

pub const NAME_MAX: usize = 32;

pub struct CreateCommunity {
    pub step: u8,
    pub name: Entity<TextInput>,
    pub public: bool,
    pub busy: bool,
    /// What the work under way is doing, and how far along it is.
    pub progress: Option<(String, f32)>,
    pub error: Option<String>,
    /// The Community and its first channel, once made.
    pub created: Option<(String, String)>,
    pub link: Option<String>,
    pub link_qr: Option<Arc<RenderImage>>,
    pub link_error: Option<String>,
    pub copied: bool,
    pub filter: Entity<TextInput>,
    pub picked: HashSet<String>,
    /// Who is listable: the people this account has written with.
    pub contacts: Option<HashSet<String>>,
    pub icon: Option<crate::commsettings::Pick>,
}

pub enum Act {
    Next,
    Back,
    Public(bool),
    PickIcon,
    Create,
    Copy,
    ShowQr,
    RetryLink,
    Toggle(String),
    Invite,
    Finish,
    Cancel,
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

const STEPS: [&str; 3] = ["Name", "Access", "Invite"];

fn click(
    on: &OnAct,
    act: impl Fn() -> Act + 'static,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| on(act(), w, cx)
}

fn steps(step: u8) -> impl IntoElement {
    div()
        .h(px(32.))
        .mt(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(28.))
        .children(STEPS.iter().enumerate().map(|(i, label)| {
            let n = i as u8 + 1;
            let (done, current) = (step > n, step == n);
            let dot = div()
                .size(px(22.))
                .rounded_full()
                .border_1()
                .flex()
                .items_center()
                .justify_center()
                .font_weight(FontWeight::BOLD)
                .text_size(px(11.));
            let dot = if done {
                dot.border_color(c(PRIMARY)).bg(c(PRIMARY)).child(
                    svg()
                        .path("icons/check.svg")
                        .size(px(12.))
                        .text_color(c(0x000000)),
                )
            } else if current {
                dot.border_color(c(PRIMARY))
                    .text_color(c(PRIMARY))
                    .child(n.to_string())
            } else {
                dot.border_color(white_a(0.2))
                    .text_color(white_a(0.4))
                    .child(n.to_string())
            };
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(px(13.))
                .line_height(px(24.))
                .text_color(if current { c(0xffffff) } else { white_a(0.4) })
                .child(dot)
                .child(*label)
        }))
}

fn radio(on: bool) -> gpui::Div {
    let ring = div()
        .size(px(18.))
        .flex_shrink_0()
        .rounded_full()
        .border_2();
    if on {
        ring.border_color(c(PRIMARY))
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(8.)).rounded_full().bg(c(PRIMARY)))
    } else {
        ring.border_color(white_a(0.25))
    }
}

fn choice(
    id: &'static str,
    icon: &'static str,
    title: &'static str,
    desc: &'static str,
    on_it: bool,
    act: Act,
    on: &OnAct,
) -> impl IntoElement {
    let on = on.clone();
    let act = Rc::new(std::cell::Cell::new(Some(act)));
    div()
        .id(id)
        .w_full()
        .flex()
        .items_center()
        .gap(px(14.))
        .p(px(16.))
        .rounded(px(12.))
        .border_1()
        .cursor_pointer()
        .when(on_it, |d| {
            d.border_color(c(PRIMARY)).bg(ca(51, 219, 152, 0.07))
        })
        .when(!on_it, |d| {
            d.border_color(white_a(0.1))
                .bg(white_a(0.02))
                .hover(|d| d.bg(white_a(0.04)))
        })
        .on_click(move |_, w, cx| {
            if let Some(a) = act.take() {
                on(a, w, cx);
            }
        })
        .child(
            div()
                .size(px(40.))
                .flex_shrink_0()
                .rounded(px(10.))
                .bg(white_a(0.06))
                .flex()
                .items_center()
                .justify_center()
                .child(svg().path(icon).size(px(20.)).text_color(if on_it {
                    c(PRIMARY)
                } else {
                    c(0xffffff)
                })),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(15.))
                        .line_height(px(24.))
                        .text_color(c(0xffffff))
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .line_height(px(18.2))
                        .text_color(white_a(0.55))
                        .child(desc),
                ),
        )
        .child(radio(on_it))
}

/// The channel pane's own head and one channel, so the preview is how it will look.
fn preview(name: &str, icon: Option<&crate::commsettings::Pick>) -> impl IntoElement {
    let name = if name.trim().is_empty() {
        "Your Community".to_string()
    } else {
        name.trim().to_string()
    };
    div()
        .mt(px(32.))
        .w(px(242.))
        .flex()
        .flex_col()
        .child(
            crate::kit::spaced("PREVIEW", 0.66)
                .justify_center()
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(11.))
                .line_height(px(24.))
                .text_color(white_a(0.4)),
        )
        .child(
            div()
                .mt(px(8.))
                .h(px(110.))
                .rounded(px(12.))
                .border_1()
                .border_color(c(0x222222))
                .bg(c(LIST_BG))
                .overflow_hidden()
                .child(
                    div()
                        .h(px(57.))
                        .rounded_t(px(11.))
                        .bg(c(0x171717))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .px(px(12.))
                        .child(match icon {
                            Some(p) => div()
                                .flex_shrink_0()
                                .child(crate::commsettings::cut(p, 36., 36., 11.))
                                .into_any_element(),
                            None => img("icons/group-placeholder.svg")
                                .size(px(36.))
                                .rounded(px(11.))
                                .flex_shrink_0()
                                .into_any_element(),
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .truncate()
                                        .font_family(RUBIK)
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_size(px(15.))
                                        .line_height(px(18.))
                                        .text_color(c(0xffffff))
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(2.5))
                                        .text_size(px(12.))
                                        .line_height(px(14.))
                                        .text_color(white_a(0.42))
                                        .child(
                                            svg()
                                                .path("icons/users-multi.svg")
                                                .size(px(13.))
                                                .text_color(white_a(0.42)),
                                        )
                                        .child("1 Member"),
                                ),
                        )
                        .child(
                            div()
                                .size(px(30.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    svg()
                                        .path("icons/chevron-down.svg")
                                        .size(px(19.5))
                                        .text_color(white_a(0.4)),
                                ),
                        ),
                )
                .child(
                    div().pt(px(8.)).px(px(8.)).child(
                        div()
                            .h(px(32.))
                            .rounded(px(4.))
                            .bg(white_a(0.09))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .px(px(8.))
                            .child(
                                svg()
                                    .path("icons/channel-hash.svg")
                                    .size(px(18.))
                                    .text_color(c(0xffffff)),
                            )
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .line_height(px(24.))
                                    .text_color(c(0xffffff))
                                    .child("general"),
                            ),
                    ),
                ),
        )
}

/// The link as people read it: no scheme, and the long key cut down the middle.
fn pretty(link: &str) -> String {
    let u = link
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let chars: Vec<char> = u.chars().collect();
    if chars.len() > 46 {
        format!(
            "{}…{}",
            chars[..32].iter().collect::<String>(),
            chars[chars.len() - 8..].iter().collect::<String>()
        )
    } else {
        u.to_string()
    }
}

fn pill(id: &'static str, primary: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(37.))
        .px(px(14.))
        .rounded(px(9.))
        .flex()
        .items_center()
        .gap(px(6.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(14.))
        .when(primary, |d| d.bg(c(ACCENT)).text_color(c(0x000000)))
        .when(!primary, |d| d.bg(white_a(0.06)).text_color(c(0xffffff)))
}

fn share(m: &CreateCommunity, narrow: bool, on: &OnAct) -> impl IntoElement {
    let ready = m.link.is_some();
    div()
        .w_full()
        .flex()
        .when(narrow, |d| d.flex_col().items_center().text_center())
        .gap(px(20.))
        .p(px(19.))
        .rounded(px(14.))
        .border_1()
        .border_color(white_a(0.1))
        .bg(white_a(0.024))
        .child(
            div()
                .id("cg-share-qr")
                .when(ready, |d| d.tooltip(crate::kit::tooltip("Enlarge QR code")))
                .size(px(132.))
                .flex_shrink_0()
                .rounded(px(12.))
                .bg(c(0xffffff))
                .p(px(9.))
                .when(ready, |d| d.cursor_pointer().on_click(click(on, || Act::ShowQr)))
                .children(m.link_qr.clone().map(|q| img(q).size(px(114.)))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .when(narrow, |d| d.w_full().items_center())
                .child(crate::kit::spaced("INVITE LINK", 0.99).font_weight(FontWeight::BOLD).text_size(px(11.)).line_height(px(24.)).text_color(c(0x8a8a8a)))
                .child(match (&m.link, &m.link_error) {
                    (Some(link), _) => div().mt(px(10.)).map(|d| if narrow { d.flex_none().whitespace_nowrap() } else { d.truncate() }).font_weight(FontWeight::MEDIUM).text_size(px(15.)).line_height(px(24.)).text_color(c(0xffffff)).child(pretty(link)),
                    (None, Some(e)) => div().mt(px(10.)).text_size(px(13.)).line_height(px(18.)).text_color(c(crate::timeline::DANGER)).child(format!("Couldn't create the link. {e}")),
                    (None, None) => div().mt(px(10.)).text_size(px(13.)).line_height(px(24.)).text_color(white_a(0.55)).child(m.progress.as_ref().map(|p| p.0.clone()).unwrap_or_else(|| "Getting ready".into())),
                })
                .child(
                    div().mt(px(10.)).flex().gap(px(8.)).map(|d| {
                        if m.link_error.is_some() {
                            d.child(pill("cg-retry", true).cursor_pointer().on_click(click(on, || Act::RetryLink)).child("Try Again"))
                        } else {
                            d.child(
                                pill("cg-copy", true)
                                    .when(!ready, |d| d.opacity(0.45))
                                    .when(ready, |d| d.cursor_pointer().on_click(click(on, || Act::Copy)))
                                    .child(svg().path(if m.copied { "icons/check.svg" } else { "icons/copy.svg" }).size(px(16.)).text_color(c(0x000000)))
                                    .child(if m.copied { "Copied" } else { "Copy Link" }),
                            )
                            .child(pill("cg-qr", false).when(!ready, |d| d.opacity(0.45)).when(ready, |d| d.cursor_pointer().on_click(click(on, || Act::ShowQr))).child("Show QR"))
                        }
                    }),
                )
                .child(div().mt(px(10.)).text_size(px(12.5)).line_height(px(17.5)).text_color(white_a(0.42)).child("You can make more links, or turn this one off, anytime in Invite People.")),
        )
}

fn invite_list(m: &CreateCommunity, store: &Store, on: &OnAct, cx: &App) -> impl IntoElement {
    let filter = m.filter.read(cx).text().to_string();
    let people = listed(store, m.contacts.as_ref(), &filter, &m.picked, |_| false);
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .child(
            div()
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
                        .top(px(8.))
                        .size(px(24.))
                        .text_color(white_a(0.4)),
                )
                .child(div().flex_1().min_w_0().child(m.filter.clone())),
        )
        .child(
            div()
                .id("cg-invite-list")
                .mt(px(10.))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p(px(6.))
                .children(people.into_iter().map(|npub| {
                    let id = npub.clone();
                    pick_row(
                        store,
                        &npub,
                        m.picked.contains(&npub),
                        false,
                        click(on, move || Act::Toggle(id.clone())),
                    )
                })),
        )
}

/// Who a multi-pick list shows, most recently talked to first: contacts matching `filter`, a
/// pasted npub, and anyone already picked from outside the contacts.
pub fn listed(
    store: &Store,
    contacts: Option<&HashSet<String>>,
    filter: &str,
    picked: &HashSet<String>,
    skip: impl Fn(&str) -> bool,
) -> Vec<String> {
    let filter = filter.trim().to_lowercase();
    let matches = |id: &str| {
        filter.is_empty()
            || format!("{} {}", store.name(id), id)
                .to_lowercase()
                .contains(&filter)
    };
    let mut people: Vec<String> = store
        .profiles
        .values()
        .filter(|p| p.id != store.my_npub && !p.is_blocked && !skip(&p.id))
        .filter(|p| contacts.is_none_or(|c| c.contains(p.id.as_str())))
        .filter(|p| matches(&p.id))
        .map(|p| p.id.to_string())
        .collect();
    // A contact whose profile never loaded is still a contact.
    if let Some(c) = contacts {
        people.extend(
            c.iter()
                .filter(|n| **n != store.my_npub && !store.profiles.contains_key(n.as_str()) && !skip(n) && matches(n))
                .cloned(),
        );
    }
    let activity = |id: &str| store.chat(id).map(crate::data::last_activity).unwrap_or(0);
    people.sort_by(|a, b| {
        activity(b)
            .cmp(&activity(a))
            .then_with(|| store.name(a).cmp(&store.name(b)))
    });
    let mut extra: Vec<String> = picked
        .iter()
        .filter(|n| !people.contains(n) && matches(n))
        .cloned()
        .collect();
    extra.sort();
    if let Some(n) = crate::newchat::extract_npub(&filter)
        .filter(|n| *n != store.my_npub && !skip(n) && !people.contains(n) && !extra.contains(n))
    {
        extra.insert(0, n);
    }
    extra.extend(people);
    extra
}

/// One contact in a multi-pick list. `quiet` keeps an unpicked box grey, as Invite People does.
pub fn pick_row<F: Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static>(
    store: &Store,
    npub: &str,
    picked: bool,
    quiet: bool,
    on: F,
) -> impl IntoElement + use<F> {
    div()
        .id(SharedString::from(format!("pick-{npub}")))
        .h(px(35.))
        .flex_shrink_0()
        .px(px(10.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .cursor_pointer()
        .hover(|s| s.bg(ca(255, 255, 255, 0.085)))
        .on_click(on)
        .child(div().mr(px(10.)).child(crate::ui::avatar(store, npub, 25.)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(14.))
                .line_height(px(24.))
                .text_color(c(0xf7f4f4))
                .child(store.name(npub)),
        )
        .child(
            div()
                .size(px(22.))
                .mr(px(10.))
                .rounded(px(4.))
                .border_2()
                .border_color(if quiet && !picked {
                    white_a(0.28)
                } else {
                    c(PRIMARY)
                })
                .when(picked, |d| {
                    d.bg(c(PRIMARY))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            svg()
                                .path("icons/check.svg")
                                .size(px(14.))
                                .text_color(c(0x000000)),
                        )
                }),
        )
}

fn button(
    id: &'static str,
    label: String,
    primary: bool,
    live: bool,
    act: Option<Act>,
    on: &OnAct,
) -> impl IntoElement {
    let on = on.clone();
    let act = Rc::new(std::cell::Cell::new(act));
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .max_w(px(220.))
        .min_h(px(41.2))
        .py(px(9.6))
        .px(px(19.2))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .border_1()
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(16.))
        .when(primary, |d| {
            d.bg(c(0x58fcb3))
                .border_color(ca(90, 252, 180, 0.65))
                .text_color(c(0x000000))
        })
        .when(!primary, |d| {
            d.border_color(white_a(0.2)).text_color(c(0xffffff))
        })
        .when(!live && primary, |d| d.bg(ca(88, 252, 179, 0.5)))
        .when(!live && !primary, |d| d.opacity(0.45))
        .when(live, |d| {
            d.cursor_pointer()
                .hover(|s| s.opacity(0.75))
                .on_click(move |_, w, cx| {
                    if let Some(a) = act.take() {
                        on(a, w, cx);
                    }
                })
        })
        .child(div().min_w_0().text_center().line_height(px(20.)).child(label))
}

pub fn view(m: &CreateCommunity, store: &Store, width: f32, on: &OnAct, cx: &App) -> AnyElement {
    let gutter = crate::kit::page_gutter(width);
    let name = m.name.read(cx).text().to_string();
    let name_ok = !name.trim().is_empty();
    let (title, lead) = match m.step {
        1 => (
            "Name your community",
            "Pick a name and, if you like, an icon. You can change both later.",
        ),
        2 => (
            "Who can join?",
            "You can change this later from Invite People.",
        ),
        _ if m.public => (
            "Share your invite link",
            "Your community is live. Send this link wherever your people are.",
        ),
        _ => (
            "Invite your friends",
            "Only the people you invite can join. Pick some now, or invite them later.",
        ),
    };
    let body: AnyElement = match m.step {
        1 => div()
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(420.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .id("cc-icon")
                            .relative()
                            .size(px(56.))
                            .flex_shrink_0()
                            .cursor_pointer()
                            .when(m.icon.is_none(), |d| d.opacity(0.7).hover(|s| s.opacity(1.)))
                            .tooltip(crate::kit::tooltip("Choose community icon"))
                            .on_click(click(on, || Act::PickIcon))
                            .child(match &m.icon {
                                Some(p) => crate::commsettings::cut(p, 56., 56., 28.),
                                None => img("ui/group-placeholder-dim.svg")
                                    .size(px(56.))
                                    .rounded_full()
                                    .into_any_element(),
                            })
                            .child(
                                div()
                                    .absolute()
                                    .left(px(31.))
                                    .top(px(31.))
                                    .size(px(26.))
                                    .rounded_full()
                                    .bg(ca(0, 0, 0, 0.85))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        svg()
                                            .path("icons/plus-circle.svg")
                                            .size(px(19.))
                                            .text_color(c(PRIMARY)),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h(px(42.))
                            .relative()
                            .rounded(px(8.))
                            .border_1()
                            .border_color(c(PRIMARY))
                            .flex()
                            .items_center()
                            .pl(px(12.))
                            .pr(px(52.))
                            .child(div().flex_1().min_w_0().child(m.name.clone()))
                            .child(
                                div()
                                    .absolute()
                                    .right(px(12.))
                                    .top(px(8.))
                                    .text_size(px(12.))
                                    .line_height(px(24.))
                                    .text_color(white_a(0.35))
                                    .child(format!("{}/{NAME_MAX}", name.chars().count())),
                            ),
                    ),
            )
            .child(preview(&name, m.icon.as_ref()))
            .into_any_element(),
        2 => div()
            .w_full()
            .max_w(px(460.))
            .mx_auto()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(choice(
                "cg-private",
                "icons/locked.svg",
                "Private",
                "Only members can invite other people in.",
                !m.public,
                Act::Public(false),
                on,
            ))
            .child(choice(
                "cg-public",
                "icons/globe.svg",
                "Public",
                "Anyone with an invite link can join. You'll get one to share right away.",
                m.public,
                Act::Public(true),
                on,
            ))
            .into_any_element(),
        _ if m.public => share(m, width <= 520., on).into_any_element(),
        _ => invite_list(m, store, on, cx).into_any_element(),
    };
    let n = m.picked.len();
    let buttons: Vec<AnyElement> = match m.step {
        1 => vec![
            button("cg-next", "Next".into(), true, name_ok, Some(Act::Next), on).into_any_element(),
            button(
                "cg-cancel",
                "Cancel".into(),
                false,
                true,
                Some(Act::Cancel),
                on,
            )
            .into_any_element(),
        ],
        2 => vec![
            button(
                "cg-create",
                "Create Community".into(),
                true,
                !m.busy,
                Some(Act::Create),
                on,
            )
            .into_any_element(),
            button(
                "cg-back",
                "Back".into(),
                false,
                !m.busy,
                Some(Act::Back),
                on,
            )
            .into_any_element(),
        ],
        _ if m.public => vec![
            button(
                "cg-open",
                "Open Community".into(),
                true,
                true,
                Some(Act::Finish),
                on,
            )
            .into_any_element(),
        ],
        _ => vec![
            button(
                "cg-invite",
                if n > 0 {
                    format!("Invite {n}")
                } else {
                    "Invite".into()
                },
                true,
                !m.busy && n > 0,
                Some(Act::Invite),
                on,
            )
            .into_any_element(),
            button(
                "cg-skip",
                "Skip for now".into(),
                false,
                !m.busy,
                Some(Act::Finish),
                on,
            )
            .into_any_element(),
        ],
    };
    let status = if m.busy {
        m.progress
            .as_ref()
            .map(|(label, pct)| format!("{label} · {}%", (pct * 100.).round() as u32))
    } else {
        m.error.clone()
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .bg(c(APP_BG))
        .child(
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
                                        .path("icons/users-multi.svg")
                                        .size(px(30.))
                                        .text_color(c(0xffffff)),
                                ),
                        )
                        .child(crate::kit::spaced("Create Community", 1.)),
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
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .px(px(15.))
                        .child(steps(m.step))
                        .child(
                            div()
                                .mt(px(22.))
                                .text_center()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_size(px(20.))
                                .line_height(px(24.))
                                .text_color(c(0xffffff))
                                .child(title),
                        )
                        .child(
                            div()
                                .mt(px(6.))
                                .mx_auto()
                                .max_w(px(420.))
                                .text_center()
                                .text_size(px(14.))
                                .line_height(px(20.3))
                                .text_color(white_a(0.55))
                                .child(lead),
                        )
                        .child(
                            div()
                                .mt(px(24.))
                                .flex_1()
                                .min_h_0()
                                .flex()
                                .flex_col()
                                .child(body),
                        ),
                ),
        )
        .child(
            div()
                .flex_shrink_0()
                .px(px(gutter + 15.))
                .pt(px(15.))
                .pb(px(15.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(20.))
                        .text_center()
                        .text_size(px(13.))
                        .line_height(px(20.))
                        .text_color(if m.busy {
                            white_a(0.55)
                        } else {
                            c(crate::timeline::DANGER)
                        })
                        .children(status),
                )
                .child(
                    div()
                        .mt(px(16.))
                        .flex()
                        .justify_center()
                        .gap(px(10.))
                        .children(buttons),
                ),
        )
        .into_any_element()
}
