//! Settings (`settings/Settings.svelte` in widescreen): a `SectionNav` in the list column,
//! and the chosen category's headed blocks beside it.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::AnimationExt;
use gpui::{AnyElement, App, Entity, FontWeight, ScrollHandle, SharedString, Window, div, img, px, svg};

use crate::composer::TextInput;
use crate::theme::*;

pub struct Anchor {
    pub id: &'static str,
    pub label: &'static str,
    keys: &'static str,
    /// The release line it arrived in, for its NEW pill.
    since: Option<&'static str>,
}

pub struct Category {
    pub id: &'static str,
    pub label: &'static str,
    icon: &'static str,
    danger: bool,
    pub anchors: Vec<Anchor>,
}

const fn anchor(id: &'static str, label: &'static str, keys: &'static str) -> Anchor {
    Anchor { id, label, keys, since: None }
}

const fn fresh(id: &'static str, label: &'static str, since: &'static str, keys: &'static str) -> Anchor {
    Anchor { id, label, keys, since: Some(since) }
}

/// The categories this build offers, in the nav's order.
pub fn categories(offers: &Offers) -> Vec<Category> {
    let mut audio = Vec::new();
    if offers.voice {
        audio.push(anchor("voice", "Voice", "voice whisper transcribe transcription translate model download"));
    }
    audio.push(fresh("calls", "Calls", "0.5", "calls microphone speaker automatic gain echo cancellation noise suppression test mic device"));
    let mut privacy = vec![
        anchor("privacy", "Privacy", "web previews video players youtube embeds url tracking typing indicators proxy media"),
        fresh("streamer", "Streamer Mode", "0.5", "streamer stream streaming live obs twitch screen share hide names pictures notifications"),
    ];
    if offers.routing {
        privacy.push(fresh("routing", "Routing", "0.5", "routing network tor i2p onion standard clearnet bridges obfs4 circuit outproxy sam router anonymity"));
    }
    privacy.push(anchor("blocked", "Blocked Users", "blocked users block unblock"));
    let cat = |id, label, icon, anchors| Category { id, label, icon, danger: false, anchors };
    let mut out = vec![
        cat("appearance", "Appearance", "palette", vec![anchor(
            "display",
            "Display",
            "theme colour color vector satoshi chatstr cosmic purple neon xmr display image types background wallpaper rich composer emoticon suggestions time suggestions countdown autocorrect send on enter return key floating player main menu navbar navigation pins sidebar tabs",
        )]),
        cat("audio", "Audio", "volume-max", audio),
        cat("notifications", "Notifications", "bell", vec![anchor("notifications", "Notifications", "mute sounds everyone pings content privacy sound prelude techno custom")]),
        cat("network", "Network", "globe", vec![
            anchor("relays", "Nostr Relays", "network nostr relays add custom"),
            anchor("servers", "Media Servers", "network media servers blossom upload add custom"),
            anchor("clock", "Clock", "clock time date decentralised decentralized device relays slow fast timestamp"),
        ]),
        cat("storage", "Storage", "folder", vec![anchor(
            "storage",
            "Storage",
            "storage usage clear auto-download download limit gallery show save keep files unencrypted media video quality compression compress",
        )]),
        cat("privacy", "Privacy", "eye-off", privacy),
        cat("security", "Security", "locked", vec![anchor("security", "Security", "security pin password biometric signer bunker encryption key")]),
    ];
    if offers.updates {
        out.push(cat("updates", "Updates", "download", vec![anchor("updates", "Updates", "updates version release changelog download install")]));
    }
    out.push(Category {
        id: "danger",
        label: "Danger Zone",
        icon: "warning",
        danger: true,
        anchors: vec![
            anchor("advanced", "Advanced", "advanced mode developer bot ids copy id identifier"),
            anchor("logs", "Logs", "copy logs crash debug"),
            anchor("logout", "Logout", "logout log out sign out"),
        ],
    });
    out
}

/// What this build and platform can show.
pub struct Offers {
    pub voice: bool,
    pub routing: bool,
    pub updates: bool,
}

/// The Display block's toggles (`Display.svelte`).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Display {
    StartMenu,
    ImageTypes,
    ChatBg,
    Emoticons,
    TimeSuggestions,
    Autocorrect,
    SendOnEnter,
    FloatingPlayer,
}

impl Display {
    pub const ALL: [Display; 8] = [
        Display::StartMenu,
        Display::ImageTypes,
        Display::ChatBg,
        Display::Emoticons,
        Display::TimeSuggestions,
        Display::Autocorrect,
        Display::SendOnEnter,
        Display::FloatingPlayer,
    ];

    /// The account setting it persists to, with its default; None for this device's own.
    pub fn sql_key(self) -> Option<(&'static str, bool)> {
        Some(match self {
            Display::StartMenu | Display::SendOnEnter => return None,
            Display::ImageTypes => ("display_image_types", false),
            Display::ChatBg => ("chat_bg_enabled", true),
            Display::Emoticons => ("emoticon_suggestions", true),
            Display::TimeSuggestions => ("time_suggestions", true),
            Display::Autocorrect => ("autocorrect", true),
            Display::FloatingPlayer => ("floating_player", true),
        })
    }

    pub fn from_sql_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.sql_key().is_some_and(|(k, _)| k == key))
    }

    pub fn label(self) -> &'static str {
        match self {
            Display::StartMenu => "Main Menu",
            Display::ImageTypes => "Display Image Types",
            Display::ChatBg => "Background Wallpaper",
            Display::Emoticons => "Emoticon Suggestions",
            Display::TimeSuggestions => "Time Suggestions",
            Display::Autocorrect => "Autocorrect",
            Display::SendOnEnter => "Send on Enter",
            Display::FloatingPlayer => "Floating Player",
        }
    }

    fn group(self) -> &'static str {
        match self {
            Display::StartMenu => "Navigation",
            Display::ImageTypes | Display::ChatBg => "Chat",
            Display::Emoticons | Display::TimeSuggestions | Display::Autocorrect | Display::SendOnEnter => "Composer",
            Display::FloatingPlayer => "Media",
        }
    }

    /// The explainer its (i) opens: a title, and HTML-lite text.
    pub fn explainer(self) -> &'static str {
        match self {
            Display::StartMenu => "<b>When enabled:</b> your Profile, Chat, Bookmarks, Mini Apps and Settings live in a Main Menu that opens from your row at the bottom of the sidebar, beside pins for the apps, chats and places you use most.<br><br><b>When disabled:</b> they sit down the sidebar as tabs, as before.<br><br>This applies to this device only.",
            Display::ImageTypes => "When enabled, images in chat will display a <b>small badge showing the file type</b> (e.g., PNG, GIF, WEBP) in the corner.<br><br>This helps identify image formats at a glance.",
            Display::ChatBg => "This feature enables and disables background images inside of Chats (DMs & Communities).<br><br>Only applies to certain themes.",
            Display::TimeSuggestions => "When you type a time to come, like <b>in 6 hours</b> or <b>dec 25 at 8pm</b>, the composer offers to send it as a time everyone sees in their own zone: a live countdown, or the date and time where they are.<br><br>Press <b>Tab</b> or tap the suggestion to use it; keep typing to leave it as written.",
            Display::Emoticons => "When enabled, text emoticons suggest the matching emoji as you type:<br><br><b>:)</b> → 🙂&nbsp;&nbsp; <b>:D</b> → 😄&nbsp;&nbsp; <b>:P</b> → 😛&nbsp;&nbsp; <b>:3</b> → 😺<br><br>Turn it off to type emoticons as plain text (e.g. <b>:3</b>) without the emoji selector getting in the way.",
            Display::Autocorrect => "When enabled, your device corrects typos as you type in the chat box, using your system's autocorrect.<br><br>Turn it off if your system keeps \"fixing\" words you meant to type.",
            Display::SendOnEnter => "When enabled, <b>Enter</b> sends your message, and <b>Shift + Enter</b> starts a new line.<br><br>Turn it off to have Enter always start a new line; send with the Send button, or <b>Ctrl + Enter</b> (<b>Cmd + Enter</b> on a Mac).<br><br>On by default on computers and off on phones. This applies to this device only.",
            Display::FloatingPlayer => "When enabled, <b>audio and video keep playing</b> when you leave their chat, in a small player you can move and resize anywhere.<br><br>Runs of voice messages play through, and a video can shrink to sound only.<br><br>Turn it off to stop media when you leave its chat.",
        }
    }
}

pub struct Theme {
    pub value: &'static str,
    pub label: &'static str,
    /// The swatch's stops, top to bottom; Vector wears its logo instead.
    swatch: &'static [u32],
}

pub const THEMES: [Theme; 7] = [
    Theme { value: "vector", label: "Vector", swatch: &[] },
    Theme { value: "pivx", label: "Keep it Purple", swatch: &[0x560ec4, 0x1c0244] },
    Theme { value: "satoshi", label: "サトシ", swatch: &[0xe5953c, 0xae712c] },
    Theme { value: "chatstr", label: "Chatstr", swatch: &[0xb949cd, 0x5949db] },
    Theme { value: "gifverse", label: "Cosmic", swatch: &[0xeab1d3, 0xf0febd] },
    Theme { value: "monero", label: "XMR", swatch: &[0xed702d, 0xed702d] },
    Theme { value: "cyberpunk", label: "Neon", swatch: &[0x8bf2f0, 0xe46aad] },
];

pub struct Settings {
    pub cat: &'static str,
    pub search: Entity<TextInput>,
    pub scroll: ScrollHandle,
    pub offers: Offers,
    pub theme: String,
    pub theme_open: bool,
    pub display: std::collections::HashMap<Display, bool>,
    /// Toggles flipped while open: only those slide.
    pub flipped: HashSet<Display>,
    /// Anchors whose NEW pill has been seen.
    pub seen: HashSet<String>,
    /// The anchor a nav click scrolled to, until the reader scrolls.
    pub jumped: Option<&'static str>,
}

pub enum Act {
    Category(&'static str),
    Anchor(&'static str, &'static str),
    Display(Display, bool),
    Explain(Display),
    ThemeOpen,
    Theme(&'static str),
    Link(&'static str),
    ClearSearch,
    /// The reader scrolled the page: the spy takes over from a jump.
    Scrolled,
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

fn click(on: &OnAct, act: impl Fn() -> Act + 'static) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| on(act(), w, cx)
}

impl Settings {
    /// The release line NEW pills belong to.
    fn line() -> &'static str {
        let v = env!("CARGO_PKG_VERSION");
        let mut parts = v.splitn(3, '.');
        let (major, minor) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        &v[..major.len() + 1 + minor.len()]
    }

    /// Ids flagged NEW: at most three, past which the pill stops meaning "look here".
    pub fn fresh_ids(&self, cats: &[Category]) -> Vec<&'static str> {
        let line = Self::line();
        cats.iter()
            .flat_map(|c| &c.anchors)
            .filter(|a| a.since == Some(line) && !self.seen.contains(a.id))
            .take(3)
            .map(|a| a.id)
            .collect()
    }

    /// The block being read: the last whose top has passed the pane's top.
    pub fn active_anchor(&self, cat: &Category) -> &'static str {
        if let Some(j) = self.jumped {
            return j;
        }
        let last = cat.anchors.len().saturating_sub(1);
        cat.anchors.get(self.scroll.top_item().min(last)).map(|a| a.id).unwrap_or("")
    }
}

fn click_wheel(on: &OnAct) -> impl Fn(&gpui::ScrollWheelEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| on(Act::Scrolled, w, cx)
}

/// The `.neon-toggle` switch.
pub fn toggle(id: impl Into<SharedString>, on: bool, slide: bool, click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static) -> AnyElement {
    let id = id.into();
    let knob = |x: f32, white: bool| {
        div()
            .absolute()
            .top(px(2.))
            .left(px(2. + x))
            .size(px(20.))
            .rounded_full()
            .bg(if white { c(0xffffff) } else { c(0x333333) })
            .shadow(vec![gpui::BoxShadow {
                color: ca(0, 0, 0, 0.4),
                offset: gpui::point(px(0.), px(1.)),
                blur_radius: px(3.),
                spread_radius: px(0.),
                inset: false,
            }])
    };
    let track = div()
        .id(SharedString::from(format!("{id}-switch")))
        .relative()
        .w(px(50.))
        .h(px(24.))
        .flex_shrink_0()
        .rounded(px(12.))
        .cursor_pointer()
        .on_click(click)
        .bg(if on {
            gpui::linear_gradient(90., gpui::linear_color_stop(c(0x33db98), 0.), gpui::linear_color_stop(c(0x00ff9d), 1.))
        } else {
            c(0x1a1a1a).into()
        });
    if !slide {
        return track.child(knob(if on { 26. } else { 0. }, on)).into_any_element();
    }
    track
        .child(knob(0., on).with_animation(
            SharedString::from(format!("{id}-{on}")),
            gpui::Animation::new(std::time::Duration::from_millis(300)).with_easing(crate::kit::bezier(0.25, 0.1, 0.25, 1.)),
            move |d, t| d.left(px(2. + 26. * if on { t } else { 1. - t })),
        ))
        .into_any_element()
}

/// The (i) explainer, which only ever opens its popup.
pub fn info(id: impl Into<SharedString>, click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static) -> impl IntoElement {
    div()
        .id(id.into())
        .size(px(16.))
        .flex_shrink_0()
        .cursor_pointer()
        .on_click(click)
        .child(svg().path("icons/info.svg").size_full().text_color(c(0xffffff)).opacity(0.5))
}

/// `.toggle-container` in a settings page: a 16px label, its explainer, the switch flush right.
pub fn toggle_row(
    id: &str,
    label: impl Into<SharedString>,
    explain: Option<Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App)>>,
    switch: AnyElement,
    grouped: bool,
) -> impl IntoElement {
    let mut name = div().flex().items_center().gap(px(6.)).text_size(px(16.)).line_height(px(19.)).text_color(c(0xffffff)).child(label.into());
    if let Some(explain) = explain {
        name = name.child(info(SharedString::from(format!("{id}-info")), explain));
    }
    div().min_h(px(36.)).mb(px(if grouped { 5. } else { 10. })).flex().items_center().justify_between().gap(px(12.)).child(name).child(switch)
}

/// `.st-group.section-label`. Its 36px top margin collapses with the 5px under the row above.
fn group_label(text: &str, first: bool) -> impl IntoElement {
    div()
        .mt(px(if first { 0. } else { 31. }))
        .mb(px(7.))
        .h(px(14.))
        .flex()
        .text_size(px(11.))
        .line_height(px(14.))
        .font_weight(FontWeight::BOLD)
        .text_color(c(0x8a8a8a))
        .child(crate::kit::spaced(text.to_uppercase(), 11. * 0.09))
}

/// A theme's swatch (or Vector's mark) at `size`.
fn swatch(t: &Theme, size: f32) -> AnyElement {
    if t.swatch.is_empty() {
        return img("icons/vector-mark.svg").size(px(size)).flex_shrink_0().into_any_element();
    }
    div()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .bg(gpui::linear_gradient(180., gpui::linear_color_stop(c(t.swatch[0]), 0.), gpui::linear_color_stop(c(t.swatch[1]), 1.)))
        .into_any_element()
}

/// The theme picker: a `.vselect` whose rows wear each theme's swatch.
fn theme_picker(s: &Settings, on: &OnAct) -> AnyElement {
    let current = THEMES.iter().find(|t| t.value == s.theme).unwrap_or(&THEMES[0]);
    let open = s.theme_open;
    let caret = svg()
        .path("ui/caret-down.svg")
        .w(px(10.))
        .h(px(6.))
        .flex_shrink_0()
        .text_color(c(0x888888))
        .when(open, |d| d.with_transformation(gpui::Transformation::rotate(gpui::radians(std::f32::consts::PI))));
    let button = div()
        .id("theme-picker")
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(36.))
        .px(px(12.))
        .rounded(px(6.))
        .border_1()
        .border_color(c(if open { 0x474747 } else { 0x333333 }))
        .hover(|d| d.border_color(c(0x474747)))
        .bg(c(0x0a0a0a))
        .text_size(px(16.))
        .text_color(c(0xffffff))
        .cursor_pointer()
        .on_click(click(on, || Act::ThemeOpen))
        .child(div().size(px(18.)).flex().items_center().justify_center().child(swatch(current, 16.)))
        .child(div().flex_1().min_w_0().truncate().child(current.label))
        .child(caret);
    let mut root = div().relative().w(px(230.)).flex_shrink_0().child(button);
    if open {
        let mut list = div()
            .absolute()
            .top(px(42.))
            .left_0()
            .right_0()
            .p(px(4.))
            .rounded(px(8.))
            .border_1()
            .border_color(c(0x2a2a2a))
            .bg(c(0x111111))
            .shadow(vec![gpui::BoxShadow {
                color: ca(0, 0, 0, 0.55),
                offset: gpui::point(px(0.), px(12.)),
                blur_radius: px(32.),
                spread_radius: px(0.),
                inset: false,
            }])
            .occlude();
        for t in &THEMES {
            let selected = t.value == s.theme;
            let v = t.value;
            list = list.child(
                div()
                    .id(t.value)
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(34.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .text_size(px(16.))
                    .text_color(c(if selected { 0xffffff } else { 0xcfcfcf }))
                    .hover(|d| d.bg(c(0x1f1f1f)).text_color(c(0xffffff)))
                    .cursor_pointer()
                    .on_click(click(on, move || Act::Theme(v)))
                    .child(div().size(px(18.)).flex().items_center().justify_center().child(swatch(t, 16.)))
                    .child(div().flex_1().min_w_0().truncate().child(t.label))
                    .when(selected, |d| d.child(svg().path("icons/check.svg").size(px(14.)).text_color(c(ACCENT)))),
            );
        }
        root = root.child(gpui::deferred(list).with_priority(2));
    }
    root.into_any_element()
}

fn display_block(s: &Settings, on: &OnAct) -> AnyElement {
    let mut col = div().flex().flex_col().child(group_label("Appearance", true)).child(
        div()
            .min_h(px(36.))
            .mb(px(5.))
            .flex()
            .items_center()
            .gap(px(12.))
            .child(div().flex_1().text_size(px(16.)).line_height(px(19.)).text_color(c(0xffffff)).child("Theme"))
            .child(theme_picker(s, on)),
    );
    let mut last = "";
    for d in Display::ALL {
        if d.group() != last {
            col = col.child(group_label(d.group(), false));
            last = d.group();
        }
        let value = s.display.get(&d).copied().unwrap_or(true);
        let switch = toggle(SharedString::from(format!("display-{}", d as u8)), value, s.flipped.contains(&d), click(on, move || Act::Display(d, !value)));
        col = col.child(toggle_row(&format!("display-{}", d as u8), d.label(), Some(Box::new(click(on, move || Act::Explain(d)))), switch, true));
    }
    col.into_any_element()
}

fn block(id: &str, s: &Settings, on: &OnAct) -> Option<AnyElement> {
    match id {
        "display" => Some(display_block(s, on)),
        _ => None,
    }
}

fn nav_icon(name: &str, color: gpui::Hsla) -> impl IntoElement {
    svg().path(SharedString::from(format!("icons/{name}.svg"))).size(px(17.)).flex_shrink_0().text_color(color)
}

fn badge_new() -> impl IntoElement {
    div()
        .flex_shrink_0()
        .ml_auto()
        .px(px(7.))
        .py(px(2.))
        .rounded_full()
        .bg(c(ACCENT))
        .text_color(c(0x052b1e))
        .text_size(px(10.))
        .line_height(px(12.))
        .font_weight(FontWeight::BOLD)
        .child(crate::kit::spaced("NEW", 0.5))
}

/// `SectionNav`: the search, the title, each section with its anchors, the footer links.
fn nav(s: &Settings, cats: &[Category], fresh: &[&'static str], active: &'static str, on: &OnAct, cx: &App) -> AnyElement {
    let query = s.search.read(cx).text().trim().to_lowercase();
    let mut list = div().flex().flex_col();
    let mut any = false;
    for cat in cats {
        let hits: Vec<bool> = cat
            .anchors
            .iter()
            .map(|a| query.is_empty() || format!("{} {} {}", cat.label, a.label, a.keys).to_lowercase().contains(&query))
            .collect();
        if !hits.contains(&true) {
            continue;
        }
        any = true;
        let selected = query.is_empty() && cat.id == s.cat;
        let open = if query.is_empty() { cat.id == s.cat } else { true };
        let flag = !open && cat.anchors.iter().any(|a| fresh.contains(&a.id));
        let id = cat.id;
        let color = if cat.danger {
            c(crate::timeline::DANGER)
        } else if selected {
            c(0xffffff)
        } else {
            c(0xb2b2b2)
        };
        let icon = if cat.danger {
            c(crate::timeline::DANGER)
        } else if selected {
            c(0xffffff)
        } else {
            c(0x5b6060)
        };
        list = list.child(
            div()
                .id(SharedString::from(format!("cs-section-{id}")))
                .group(SharedString::from(format!("cs-section-{id}")))
                .h(px(38.))
                .mb(px(1.))
                .px(px(11.))
                .flex()
                .items_center()
                .gap(px(12.))
                .rounded(px(8.))
                .text_size(px(15.))
                .line_height(px(18.))
                .font_weight(if cat.danger { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                .text_color(color)
                .when(selected, |d| d.bg(c(0x1b1b1b)))
                .when(!selected, |d| d.hover(|d| d.bg(c(0x151515)).text_color(if cat.danger { c(crate::timeline::DANGER) } else { c(0xffffff) })))
                .cursor_pointer()
                .on_click(click(on, move || Act::Category(id)))
                .child(
                    svg()
                        .path(SharedString::from(format!("icons/{}.svg", if cat.icon == "warning" { "vector_warning" } else { cat.icon })))
                        .size(px(17.))
                        .flex_shrink_0()
                        .text_color(icon)
                        .when(!cat.danger, |d| d.group_hover(SharedString::from(format!("cs-section-{id}")), |d| d.text_color(c(0xffffff)))),
                )
                .child(div().min_w_0().truncate().child(cat.label))
                .when(flag, |d| d.child(badge_new())),
        );
        if open {
            let shown: Vec<&Anchor> = cat.anchors.iter().zip(&hits).filter(|(_, h)| **h).map(|(a, _)| a).collect();
            let marker = selected.then(|| shown.iter().position(|a| a.id == active)).flatten();
            let mut anchors = div()
                .relative()
                .pb(px(6.))
                .child(div().absolute().left(px(18.5)).top(px(6.)).bottom(px(12.)).w(px(2.)).bg(c(0x333333)));
            if let Some(i) = marker {
                anchors = anchors.child(div().absolute().left(px(18.5)).top(px(34. * i as f32 + 17. - 10.)).w(px(2.)).h(px(20.)).rounded(px(1.)).bg(c(0xffffff)));
            }
            for a in shown {
                let on_it = selected && a.id == active;
                let (cid, aid) = (cat.id, a.id);
                anchors = anchors.child(
                    div()
                        .id(SharedString::from(format!("cs-anchor-{aid}")))
                        .h(px(34.))
                        .pl(px(40.))
                        .pr(px(11.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(14.))
                        .line_height(px(17.))
                        .font_weight(if on_it { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                        .text_color(if on_it { c(0xffffff) } else { c(0xb2b2b2) })
                        .hover(|d| d.text_color(if on_it { c(0xffffff) } else { c(0xe6e6e6) }))
                        .cursor_pointer()
                        .on_click(click(on, move || Act::Anchor(cid, aid)))
                        .child(div().min_w_0().truncate().child(a.label))
                        .when(fresh.contains(&a.id), |d| d.child(badge_new())),
                );
            }
            list = list.child(anchors);
        }
    }
    if !any {
        list = list.child(div().mx(px(10.)).my(px(12.)).text_size(px(13.)).text_color(c(0x6f6f6f)).child("No matches"));
    }
    let link = |label: &'static str, key: &'static str| {
        div()
            .id(key)
            .text_size(px(12.))
            .line_height(px(14.))
            .text_color(c(0x5b6060))
            .hover(|d| d.text_color(c(0xffffff)))
            .cursor_pointer()
            .on_click(click(on, move || Act::Link(key)))
            .child(label)
    };
    let has_query = !query.is_empty();
    div()
        .w(px(crate::ui::LIST_W))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .bg(c(LIST_BG))
        .border_r_1()
        .border_color(c(DIVIDER))
        .child(
            // .cs-search
            div()
                .h(px(50.))
                .flex_shrink_0()
                .px(px(10.))
                .relative()
                .flex()
                .items_center()
                .border_b_1()
                .border_color(c(0x171717))
                .child(
                    div()
                        .w_full()
                        .h(px(34.))
                        .rounded(px(8.))
                        .bg(c(0x1f1f1f))
                        .pl(px(34.))
                        .pr(px(if has_query { 38. } else { 12. }))
                        .flex()
                        .items_center()
                        .child(div().flex_1().min_w_0().child(s.search.clone())),
                )
                .child(svg().path("icons/search.svg").absolute().left(px(20.5)).top(px(16.5)).size(px(17.)).text_color(c(0xb2b2b2)))
                .when(has_query, |d| {
                    d.child(
                        div()
                            .id("cs-search-clear")
                            .absolute()
                            .right(px(21.))
                            .top(px(16.))
                            .size(px(18.))
                            .cursor_pointer()
                            .group("cs-clear")
                            .on_click(click(on, || Act::ClearSearch))
                            .child(svg().path("ui/search-clear.svg").size_full().text_color(c(0xb2b2b2)).group_hover("cs-clear", |d| d.text_color(c(0xffffff)))),
                    )
                }),
        )
        .child(
            div()
                .id("cs-nav-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .pt(px(18.))
                .px(px(10.))
                .pb(px(24.))
                .child(
                    div()
                        .mx(px(10.))
                        .mb(px(17.))
                        .pb(px(16.))
                        .border_b_1()
                        .border_color(c(0x262626))
                        .text_size(px(20.))
                        .line_height(px(24.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(c(0xffffff))
                        .child("Settings"),
                )
                .child(list)
                .child(
                    div()
                        .mt(px(12.))
                        .pt(px(16.))
                        .px(px(8.))
                        .border_t_1()
                        .border_color(c(0x262626))
                        .flex()
                        .flex_wrap()
                        .gap_x(px(16.))
                        .gap_y(px(6.))
                        .child(link("Donate", "donate"))
                        .child(link("Docs", "gitbook"))
                        .child(link("Privacy Policy", "privacy")),
                ),
        )
        .into_any_element()
}

/// The whole screen: the nav in the list column and the page beside it, `width` wide.
pub fn view(s: &Settings, width: f32, on: &OnAct, cx: &App) -> AnyElement {
    let cats = categories(&s.offers);
    let fresh = s.fresh_ids(&cats);
    let cat = cats.iter().find(|c| c.id == s.cat).unwrap_or(&cats[0]);
    let active = s.active_anchor(cat);
    let pane = width - crate::ui::LIST_W;
    let gutter = crate::kit::page_gutter(pane);
    let blocks = cat.anchors.iter().enumerate().map(|(i, a)| {
        // `.cs-content.st-page`: a 720px column at the pane's left, inside its gutters.
        div().max_w(px(720. + 2. * gutter)).px(px(gutter)).child(
        div()
            .pt(px(30.))
            .pb(px(34.))
            .when(i > 0, |d| d.border_t_1().border_color(c(0x262626)))
            .child(div().mb(px(20.)).text_size(px(24.)).line_height(px(28.)).font_weight(FontWeight::BOLD).text_color(c(0xffffff)).child(a.label))
            .children(block(a.id, s, on))
            .with_animation(
                SharedString::from(format!("cs-in-{}", a.id)),
                gpui::Animation::new(std::time::Duration::from_millis(300)).with_easing(crate::kit::bezier(0.22, 1., 0.36, 1.)),
                |d, t| d.opacity(t).mt(px(8. * (1. - t))),
            ))
    });
    let main = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(c(APP_BG))
        .child(
            div()
                .h(px(50.))
                .flex_shrink_0()
                .pl(px(28.))
                .pr(px(18.))
                .flex()
                .items_center()
                .border_b_1()
                .border_color(c(0x171717))
                .text_size(px(16.))
                .line_height(px(19.))
                .text_color(c(0xffffff))
                .child(cat.label),
        )
        .child(
            div()
                .id(SharedString::from(format!("cs-scroll-{}", cat.id)))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&s.scroll)
                .when(s.jumped.is_some(), |d| d.on_scroll_wheel(click_wheel(on)))
                .pt(px(12.))
                .children(blocks)
                .child(div().h(px(140.))),
        );
    div().size_full().flex().child(nav(s, &cats, &fresh, active, on, cx)).child(main).into_any_element()
}
