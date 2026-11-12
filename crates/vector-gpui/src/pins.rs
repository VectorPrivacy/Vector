//! `PinsDrawer.svelte` + `PinRow.svelte`: a Concord v2 channel's verified pins, sheeted under
//! the chat header. A row jumps to its message when this device holds it, else expands to read
//! it in full; the conversation behind dims while the drawer is open.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, FontWeight, SharedString, Window, div, img, px, svg};
use serde_json::Value;

use crate::data::Store;
use crate::inline::{Atom, Kind, Look, inline};
use crate::markdown::{self, Block, ParaKind};
use crate::theme::*;

#[derive(Clone)]
pub struct Pin {
    pub id: String,
    /// The proven edit's words, else the original's.
    pub content: String,
    pub ms: u64,
    /// Each NIP-92 attachment's name and extension.
    pub files: Vec<(String, String)>,
    /// `image` or `video`, when the first attachment can preview inline.
    pub media: Option<&'static str>,
    pub jumpable: bool,
}

impl Pin {
    pub fn from_json(v: &Value) -> Option<Self> {
        let id = v["rumor_id"].as_str()?.to_string();
        let content = v["edited"]["content"].as_str().or(v["content"].as_str()).unwrap_or_default().to_string();
        let imeta: Vec<HashMap<&str, &str>> = v["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_array)
            .filter(|t| t.first().and_then(Value::as_str) == Some("imeta"))
            .map(|t| t.iter().skip(1).filter_map(Value::as_str).filter_map(|s| s.split_once(' ')).collect())
            .collect();
        let files = imeta
            .iter()
            .map(|f| {
                let mime = f.get("m").copied().unwrap_or_default();
                let sub = mime.split('/').nth(1).unwrap_or_default();
                let name = f.get("name").copied().filter(|n| !n.is_empty()).unwrap_or(if sub.is_empty() { "file" } else { sub });
                let ext = match name.rsplit_once('.') {
                    Some((_, e)) => e,
                    None => sub,
                };
                (name.to_string(), ext.to_ascii_lowercase())
            })
            .collect();
        let media = imeta.iter().find_map(|f| f.get("m")).and_then(|m| {
            if m.starts_with("image/") {
                Some("image")
            } else if m.starts_with("video/") {
                Some("video")
            } else {
                None
            }
        });
        Some(Self { id, content, ms: v["ms"].as_u64().unwrap_or(0), files, media, jumpable: false })
    }

    fn lines(&self) -> usize {
        self.content.lines().filter(|l| !l.trim().is_empty()).count()
    }
}

#[derive(Default)]
pub struct Pins {
    pub community: String,
    /// The channel these pins are for; a different open chat shows none of it.
    pub channel: String,
    pub can_pin: bool,
    pub list: Vec<Pin>,
    pub sealed: bool,
    pub open: bool,
    pub expanded: HashSet<String>,
    /// Each channel's last known button state, so moving between channels doesn't flash it.
    pub shown: HashMap<String, bool>,
    /// Decrypted previews of media pins, by rumor id.
    pub previews: HashMap<String, PathBuf>,
    /// Rows whose one-line preview is cut, as last laid out.
    pub clips: Rc<RefCell<HashMap<String, Rc<Cell<bool>>>>>,
}

impl Pins {
    /// The header button for `chat`: `Some(open)` while it shows.
    pub fn button(&self, chat: &str) -> Option<bool> {
        if self.channel != chat {
            return None;
        }
        self.shown.get(chat).copied().unwrap_or(false).then_some(self.open)
    }

    pub fn pinned(&self, id: &str) -> bool {
        self.list.iter().any(|p| p.id == id)
    }

    fn clip(&self, id: &str) -> Rc<Cell<bool>> {
        self.clips.borrow_mut().entry(id.to_string()).or_default().clone()
    }
}

#[derive(Clone)]
pub enum Act {
    Toggle,
    Close,
    Row(String),
    Expand(String),
    Open(String),
    Unpin(String),
    Text(String, crate::inline::Target),
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

fn click(on: &OnAct, act: Act) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| {
        cx.stop_propagation();
        on(act.clone(), w, cx)
    }
}

/// `pinsFormatDate`: MM/DD/YYYY, local.
fn date(ms: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms as i64)
        .single()
        .map(|d| d.format("%m/%d/%Y").to_string())
        .unwrap_or_default()
}

/// `pinsFirstDisplayLine`: rules, fences and blank lines never lead a preview.
fn first_line(source: &str) -> &str {
    source
        .split('\n')
        .map(str::trim)
        .find(|l| {
            !l.is_empty()
                && !l.starts_with("```")
                && !(l.len() >= 3 && ["-", "*", "_"].iter().any(|c| l.chars().all(|x| x.to_string() == *c)))
        })
        .unwrap_or(source)
}

fn look() -> Look {
    Look::new(INTER, 14., 24., c(TEXT))
}

/// The attachment kinds, bold-ish after the words (`pinsAppendAttachmentKinds`).
fn kinds(pin: &Pin) -> Vec<Atom> {
    let mut l = look();
    l.weight = FontWeight::MEDIUM;
    pin.files
        .iter()
        .map(|(_, ext)| Atom::Text {
            text: if ext.is_empty() { "Attachment".into() } else { crate::data::file_type(ext).1.into() },
            look: l.clone(),
            kind: Kind::Plain,
        })
        .collect()
}

fn control(id: SharedString, path: &'static str, hover: u32, on: &OnAct, act: Act) -> impl IntoElement {
    div()
        .id(id.clone())
        .group(id.clone())
        .size(px(22.))
        .flex_shrink_0()
        .cursor_pointer()
        .child(svg().path(path).size_full().text_color(c(ICON_MUTED)).group_hover(id, move |s| s.text_color(c(hover))))
        .on_click(click(on, act))
}

fn row(pins: &Pins, pin: &Pin, store: &Store, revealed: &HashMap<String, HashSet<u32>>, clock: f32, on: &OnAct) -> AnyElement {
    let none = HashSet::new();
    let revealed = revealed.get(&pin.id).unwrap_or(&none);
    let expanded = pins.expanded.contains(&pin.id);
    let clipped = pins.clip(&pin.id);
    let expandable = clipped.get() || pin.media.is_some() || pin.lines() > 1;
    let can_open = !pin.files.is_empty() && pin.media.is_none();
    let hot = pin.jumpable || expandable;
    let icon = pin.files.first().map(|(_, ext)| crate::data::file_type(ext).0);

    let mut controls = div()
        .flex_shrink_0()
        .h(px(24.))
        .ml(px(10.))
        .flex()
        .items_center()
        .gap(px(10.))
        .child(div().text_size(px(13.)).opacity(0.55).child(date(pin.ms)));
    if expandable {
        let up = expanded;
        controls = controls.child(
            div()
                .id(SharedString::from(format!("pin-more-{}", pin.id)))
                .size(px(22.))
                .cursor_pointer()
                .on_click(click(on, Act::Expand(pin.id.clone())))
                .child(
                    svg()
                        .path("icons/chevron-down.svg")
                        .size_full()
                        .text_color(c(ICON_MUTED))
                        .when(up, |s| s.with_transformation(gpui::Transformation::rotate(gpui::radians(std::f32::consts::PI)))),
                ),
        );
    }
    if can_open {
        controls = controls.child(control(
            SharedString::from(format!("pin-open-{}", pin.id)),
            "icons/file-search.svg",
            ACCENT,
            on,
            Act::Open(pin.id.clone()),
        ));
    }
    if pins.can_pin {
        controls = controls.child(control(
            SharedString::from(format!("pin-unpin-{}", pin.id)),
            "icons/x.svg",
            crate::timeline::DANGER,
            on,
            Act::Unpin(pin.id.clone()),
        ));
    }

    let body: AnyElement = if expanded {
        let mut col = div().flex().flex_col();
        let source = demote(&pin.content);
        let blocks = markdown::parse(
            source.trim(),
            &markdown::Ctx {
                base: look(),
                revealed,
                custom: None,
                now: store.now_ms() as f64 / 1000.,
                mention: Some(&|npub: &str| store.name_in(npub, Some(&pins.community))),
                tags: &[],
                mask: Some(&|t: &str| store.stream_mask(t)),
                colors: &[],
            },
        );
        let on_text: crate::inline::OnClick = {
            let (on, id) = (on.clone(), pin.id.clone());
            Rc::new(move |t, w, cx| on(Act::Text(id.clone(), t), w, cx))
        };
        let mut blocks = blocks;
        let tail = kinds(pin);
        if !tail.is_empty() {
            blocks.push(Block::Para(markdown::Para { atoms: tail.into(), base: look(), kind: ParaKind::Text }));
        }
        let mut prev = None;
        for (i, b) in blocks.iter().enumerate() {
            let (top, bottom) = crate::timeline::margins(b, i == 0, i + 1 == blocks.len());
            let gap = prev.map_or(top, |p: f32| p.max(top));
            col = col.child(div().mt(px(gap)).child(crate::timeline::block(b, &format!("pin-{}-{i}", pin.id), &on_text, clock)));
            prev = Some(bottom);
        }
        col = col.children(prev.filter(|&p| p > 0.).map(|p| div().h(px(p))));
        if let Some(path) = pins.previews.get(&pin.id).filter(|_| pin.media == Some("image")) {
            col = col.child(img(path.clone()).mt(px(8.)).max_w_full().max_h(px(240.)).rounded(px(8.)));
        }
        col.into_any_element()
    } else {
        let source = store.resolve_mentions(first_line(&pin.content), Some(&pins.community));
        let mut atoms = crate::preview::atoms(&source, usize::MAX, &look());
        atoms.extend(kinds(pin));
        inline(atoms, look()).single_line().report_clip(clipped).into_any_element()
    };

    let id = pin.id.clone();
    div()
        .id(SharedString::from(format!("pin-{}", pin.id)))
        .flex()
        .items_start()
        .gap(px(6.))
        .px(px(16.))
        .py(px(8.))
        .border_b_1()
        .border_color(white_a(0.06))
        .text_size(px(14.))
        .line_height(px(24.))
        .text_color(c(TEXT))
        .when(hot, |d| d.cursor_pointer().hover(|d| d.bg(white_a(0.04))))
        .on_click(click(on, Act::Row(id)))
        .child(
            svg()
                .path(match icon {
                    Some(name) => SharedString::from(format!("icons/{name}.svg")),
                    None => "ui/pin-row.svg".into(),
                })
                .size(px(20.))
                .mt(px(2.))
                .flex_shrink_0()
                .text_color(c(ACCENT)),
        )
        .child(div().flex_1().min_w_0().child(body))
        .child(controls)
        .into_any_element()
}

/// Headers demote to bold body text inside the drawer.
fn demote(source: &str) -> String {
    source
        .split('\n')
        .map(|l| {
            let h = l.chars().take_while(|&c| c == '#').count();
            if (1..=6).contains(&h) && l[h..].starts_with(char::is_whitespace) && !l[h..].trim().is_empty() {
                format!("**{}**", l[h..].trim())
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The drawer and the dimmed conversation behind it, laid over the message list.
pub fn drawer(pins: &Pins, store: &Store, revealed: &HashMap<String, HashSet<u32>>, clock: f32, max_h: f32, on: &OnAct) -> AnyElement {
    let notice = |text: &'static str| {
        div().p(px(16.)).text_size(px(13.)).opacity(0.6).text_center().child(text).into_any_element()
    };
    let list: Vec<AnyElement> = if pins.sealed {
        vec![notice("This channel's pins are protected by a key you don't hold yet.")]
    } else if pins.list.is_empty() {
        vec![notice("No pinned messages yet.")]
    } else {
        pins.list.iter().map(|p| row(pins, p, store, revealed, clock, on)).collect()
    };
    div()
        .absolute()
        .size_full()
        .top_0()
        .left_0()
        .child(
            div()
                .id("pins-scrim")
                .absolute()
                .size_full()
                // `blur(6px) brightness(.45)` without a blur: darker, so the words drop back as far.
                .bg(ca(0, 0, 0, 0.72))
                .occlude()
                .on_click(click(on, Act::Close)),
        )
        .child(
            div()
                .id("pins-drawer")
                .occlude()
                .absolute()
                .top(px(-1.))
                .left_0()
                .right_0()
                .bg(c(0x0d0d0d))
                .border_1()
                .border_t_0()
                .border_color(white_a(0.12))
                .rounded_b(px(12.))
                .text_color(c(TEXT))
                .child(div().id("pins-list").max_h(px(max_h)).overflow_y_scroll().children(list))
                .child(
                    div()
                        .id("pins-close")
                        .h(px(41.))
                        .border_t_1()
                        .border_color(white_a(0.12))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(4.))
                        .cursor_pointer()
                        .text_size(px(14.))
                        .on_click(click(on, Act::Close))
                        .child("Click to Close")
                        .child(svg().path("ui/pins-close.svg").size(px(20.)).text_color(c(ACCENT))),
                ),
        )
        .into_any_element()
}

/// `.pins-header-btn`: the 40px toggle left of the ⋯. The header's own colour rule outranks
/// `.pins-open`, so it stays grey either way.
pub fn button(on: OnAct) -> impl IntoElement {
    div()
        .id("pins-btn")
        .tooltip(crate::kit::tooltip("Pinned Messages"))
        .size(px(40.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .on_click(click(&on, Act::Toggle))
        .child(svg().path("ui/pin.svg").size(px(20.5)).text_color(c(ICON_MUTED)))
}
