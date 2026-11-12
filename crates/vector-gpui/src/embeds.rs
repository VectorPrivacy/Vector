//! Nostr posts, articles and videos a message references, as cards (`NostrEmbedCard.svelte`,
//! `js/nostr-embeds.js`). Only the backend fetches: the event from relays, its pictures
//! through the linked-image cache.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, FontWeight, IntoElement, ParentElement, RenderImage, SharedString, Styled, div,
    img, prelude::*, px, svg,
};
use serde::Deserialize;
use vector_core::types::Message;

use crate::inline::{Look, OnClick};
use crate::media::Linked;
use crate::theme::*;
use crate::timeline::{Act as RowAct, Ctx};

pub const PER_MSG: usize = 3;
const ERR_TTL: Duration = Duration::from_secs(30);
const CARD_W: f32 = 420.;
const MEDIA_SHOWN: usize = 4;

#[derive(Clone, Debug, Deserialize)]
pub struct Media {
    pub url: String,
    pub is_video: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub poster: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReplyTo {
    pub kind: Option<u16>,
    pub entity: Option<String>,
    pub external: Option<String>,
}

/// `vector_core::nostr_embed::Embed`, as the card reads it.
#[derive(Clone, Debug, Deserialize)]
pub struct Embed {
    pub class: String,
    pub kind: u16,
    pub author: String,
    pub created_at: u64,
    pub published_at: Option<u64>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub image: Option<String>,
    pub text: String,
    pub media: Vec<Media>,
    pub video: Option<Media>,
    pub link: Option<String>,
    pub content_warning: Option<String>,
    #[serde(default)]
    pub emoji: Vec<vector_core::types::EmojiTag>,
    pub bech32: String,
    pub reposted_by: Option<String>,
    pub reply_to: Option<ReplyTo>,
}

impl Embed {
    pub fn from_core(e: vector_core::nostr_embed::Embed) -> Result<Self, String> {
        serde_json::to_value(e)
            .and_then(serde_json::from_value)
            .map_err(|e| e.to_string())
    }
}

#[derive(Clone)]
pub enum State {
    Loading,
    Ready(Arc<Embed>),
    Failed(String, Instant),
}

/// Resolved references, by lowercased entity; a miss expires so a relay blip doesn't blank a card for good.
#[derive(Default)]
pub struct Cache {
    states: HashMap<String, State>,
    /// Entities the last render asked for and nobody has fetched yet.
    pub queue: Vec<String>,
    /// Content warnings tapped open, by message and entity.
    pub revealed: HashSet<String>,
}

impl Cache {
    pub fn get(&mut self, entity: &str) -> State {
        let key = entity.to_ascii_lowercase();
        match self.states.get(&key) {
            Some(State::Failed(_, at)) if at.elapsed() >= ERR_TTL => {}
            Some(s) => return s.clone(),
            None => {}
        }
        self.states.insert(key, State::Loading);
        self.queue.push(entity.to_string());
        State::Loading
    }

    pub fn settle(&mut self, entity: &str, res: Result<Embed, String>) {
        let state = match res {
            Ok(e) => State::Ready(Arc::new(e)),
            Err(e) => State::Failed(e, Instant::now()),
        };
        self.states.insert(entity.to_ascii_lowercase(), state);
    }
}

/// What a card's controls ask of the app.
#[derive(Clone)]
pub enum Act {
    OpenUrl(String),
    Copy(String, String),
    Reveal(String),
    Profile(String),
    /// A reference inside a card, or a comment's parent.
    Ref(String),
    Expand(String, Option<String>),
    View(crate::viewer::Open),
    /// The expanded view: back a step, close, or open a quote in it.
    Back,
    Close,
    Push(String, Option<String>),
}

// ---------------------------------------------------------------------------------------------
// Finding references.

/// One reference a message carries: where it sits, so the bare ones can be cut.
#[derive(Clone, Debug, PartialEq)]
pub struct Ref {
    pub entity: String,
    pub url: Option<String>,
    start: usize,
    end: usize,
}

const BECH32: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

fn bech_char(b: u8) -> bool {
    BECH32.contains(&b.to_ascii_lowercase())
}

/// The length of an entity starting at `s`, when one does (`(note|nevent|naddr)1` + 20 or more).
fn entity_len(s: &[u8]) -> Option<usize> {
    let lower = |n: usize| s.get(..n).map(|p| p.to_ascii_lowercase());
    let head = ["note1", "nevent1", "naddr1"]
        .iter()
        .find(|h| lower(h.len()).as_deref() == Some(h.as_bytes()))?;
    let body = s[head.len()..]
        .iter()
        .take_while(|b| bech_char(**b))
        .count();
    (body >= 20).then_some(head.len() + body)
}

fn entity_only(s: &str) -> bool {
    entity_len(s.as_bytes()) == Some(s.len())
}

/// The reference a web link ends in (njump, primal, habla and the like).
fn entity_from_url(url: &str) -> Option<String> {
    let path = url.get(8..)?.split(['?', '#']).next()?;
    let (_, path) = path.split_once('/')?;
    let last = path.trim_end_matches('/').rsplit('/').next()?;
    let last = if last.len() > 5 && last[last.len() - 5..].eq_ignore_ascii_case(".html") {
        &last[..last.len() - 5]
    } else {
        last
    };
    let last = if last.len() > 6 && last[..6].eq_ignore_ascii_case("nostr:") {
        &last[6..]
    } else {
        last
    };
    entity_only(last).then(|| last.to_string())
}

fn blank(out: &mut [u8], from: usize, to: usize) {
    out[from..to].fill(b' ');
}

/// Code spans and fences, blanked byte for byte so offsets still index the original.
fn without_code(text: &str) -> Vec<u8> {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        let fence = [b"```", b"~~~"]
            .into_iter()
            .find(|f| b[i..].starts_with(*f));
        if let Some(f) = fence {
            if let Some(end) = memchr::memmem::find(&b[i + 3..], f) {
                let to = i + 3 + end + 3;
                blank(&mut out, i, to);
                i = to;
                continue;
            }
        } else if b[i] == b'`'
            && let Some(end) = b[i + 1..].iter().position(|c| *c == b'`' || *c == b'\n')
            && b[i + 1 + end] == b'`'
        {
            let to = i + end + 2;
            blank(&mut out, i, to);
            i = to;
            continue;
        }
        i += 1;
    }
    out
}

fn url_char(b: u8) -> bool {
    !b.is_ascii_whitespace() && !b"<>\"'`{}|\\^[]".contains(&b)
}

fn scan(text: &str) -> Vec<Ref> {
    let mut code = without_code(text);
    let mut found = Vec::new();
    let mut i = 0;
    while i + 8 <= code.len() {
        if !code[i..i + 8].eq_ignore_ascii_case(b"https://") {
            i += 1;
            continue;
        }
        let end = i + code[i..].iter().take_while(|b| url_char(**b)).count();
        let raw = &text[i..end];
        let url = raw.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '\'', '"']);
        // `<https://…>` is the no-preview form.
        let quiet = i > 0 && code[i - 1] == b'<' && code.get(end) == Some(&b'>');
        if !quiet && let Some(entity) = entity_from_url(url) {
            found.push(Ref {
                entity,
                url: Some(url.to_string()),
                start: i,
                end: i,
            });
        }
        blank(&mut code, i, end);
        i = end;
    }
    let opens = b" \t\n\r([{<\"'";
    let closes = b")]}>\"'.,;:!?";
    let mut i = 0;
    while i < code.len() {
        if i > 0 && !opens.contains(&code[i - 1]) {
            i += 1;
            continue;
        }
        let start = i;
        let prefix = if code[i..].len() >= 6 && code[i..i + 6].eq_ignore_ascii_case(b"nostr:") {
            6
        } else {
            0
        };
        let Some(len) = entity_len(&code[i + prefix..]) else {
            i += 1;
            continue;
        };
        let end = i + prefix + len;
        let next = code.get(end).copied();
        if next.is_some_and(|n| !n.is_ascii_whitespace() && !closes.contains(&n)) {
            i = end;
            continue;
        }
        // A markdown link's target or label: cutting it would break the link.
        let target = start >= 2 && code[start - 1] == b'(' && code[start - 2] == b']';
        let label = next == Some(b']') && code.get(end + 1) == Some(&b'(');
        if !target && !label {
            found.push(Ref {
                entity: text[i + prefix..end].to_string(),
                url: None,
                start,
                end,
            });
        }
        i = end;
    }
    found.sort_by_key(|r| r.start);
    let mut seen = HashSet::new();
    let mut refs = Vec::new();
    for f in found {
        let key = f.entity.to_ascii_lowercase();
        if seen.contains(&key) || vector_core::nostr_embed::EmbedRef::parse(&key).is_none() {
            continue;
        }
        seen.insert(key);
        refs.push(f);
        if refs.len() == PER_MSG {
            break;
        }
    }
    refs
}

/// A cheap first look: some `1` follows `note`, `nevent` or `naddr`.
fn names_entity(text: &str) -> bool {
    let b = text.as_bytes();
    memchr::memchr_iter(b'1', b).any(|i| {
        ["note", "nevent", "naddr"]
            .iter()
            .any(|h| i >= h.len() && b[i - h.len()..i].eq_ignore_ascii_case(h.as_bytes()))
    })
}

thread_local! {
    static SCANS: RefCell<HashMap<String, Rc<[Ref]>>> = RefCell::new(HashMap::new());
}

/// The references in `text` that get a card: the first few, distinct, never inside code.
pub fn refs(text: &str) -> Rc<[Ref]> {
    if !names_entity(text) {
        return Rc::from([]);
    }
    SCANS.with(|c| {
        if let Some(r) = c.borrow().get(text) {
            return r.clone();
        }
        let r: Rc<[Ref]> = scan(text).into();
        let mut c = c.borrow_mut();
        if c.len() > 500 {
            c.clear();
        }
        c.insert(text.to_string(), r.clone());
        r
    })
}

/// The text without the bare and `nostr:` references its cards stand in for.
pub fn strip(text: &str) -> std::borrow::Cow<'_, str> {
    let refs = refs(text);
    let cuts: Vec<&Ref> = refs.iter().filter(|r| r.url.is_none()).collect();
    if cuts.is_empty() {
        return text.into();
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for r in cuts {
        out.push_str(&text[at..r.start]);
        at = r.end;
    }
    out.push_str(&text[at..]);
    tidy(&out).into()
}

/// Runs of blanks to one space, three or more breaks to two, then trimmed.
pub fn tidy(out: &str) -> String {
    let mut tidy = String::with_capacity(out.len());
    let (mut blanks, mut breaks) = (0, 0);
    for ch in out.chars() {
        match ch {
            ' ' | '\t' => {
                blanks += 1;
                if blanks == 1 {
                    tidy.push(ch);
                } else if blanks == 2 {
                    tidy.pop();
                    tidy.push(' ');
                }
                breaks = 0;
            }
            '\n' => {
                breaks += 1;
                blanks = 0;
                if breaks <= 2 {
                    tidy.push('\n');
                }
            }
            c => {
                blanks = 0;
                breaks = 0;
                tidy.push(c);
            }
        }
    }
    tidy.trim().to_string()
}

/// The event kind a nevent or naddr names (its TLV 3), when it names one.
fn ref_kind(entity: &str) -> Option<u16> {
    match vector_core::nostr_embed::EmbedRef::parse(entity)? {
        vector_core::nostr_embed::EmbedRef::Event { kind, .. } => kind,
        vector_core::nostr_embed::EmbedRef::Address { coordinate, .. } => {
            Some(coordinate.kind.as_u16())
        }
    }
}

/// What a reference reads as in a chat-list snippet or a chip.
pub fn noun(entity: &str) -> &'static str {
    match ref_kind(entity) {
        Some(30023) => "Article",
        Some(21 | 34235) => "Video",
        Some(22 | 34236) => "Short",
        Some(6 | 16) => "Repost",
        Some(1111) => "Comment",
        _ => "Post",
    }
}

/// Chat-list snippets: a bare reference reads as what it points at.
pub fn for_preview(text: &str) -> std::borrow::Cow<'_, str> {
    let refs = refs(text);
    if refs.iter().all(|r| r.url.is_some()) {
        return text.into();
    }
    let mut out = String::new();
    let mut at = 0;
    for r in refs.iter().filter(|r| r.url.is_none()) {
        out.push_str(&text[at..r.start]);
        out.push_str("Nostr ");
        out.push_str(noun(&r.entity));
        at = r.end;
    }
    (out + &text[at..]).into()
}

// ---------------------------------------------------------------------------------------------
// The card.

fn ref_label(entity: &str) -> &'static str {
    let l = entity.to_ascii_lowercase();
    if l.starts_with("naddr") {
        "naddr"
    } else if l.starts_with("note") {
        "note ID"
    } else {
        "nevent"
    }
}

fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// What a card's pieces draw from, in a message or the expanded view.
pub struct Env<'a> {
    pub store: &'a crate::data::Store,
    pub media: &'a RefCell<crate::media::Media>,
    pub clock: f32,
    pub send: Send,
    /// The expanded view: Inter on the 24px rhythm it inherits.
    pub modal: bool,
}

impl Env<'_> {
    fn family(&self) -> &'static str {
        if self.modal { INTER } else { RUBIK }
    }
}

enum Pic {
    Ready(Arc<RenderImage>, u32, u32),
    Loading,
    Gone,
}

/// A picture the card shows: a web one through the linked cache, a fixture one from disk.
fn pic(env: &Env, url: &str) -> Pic {
    let key = if url.starts_with("https://") {
        url.to_string()
    } else {
        env.store
            .media_root
            .join(url)
            .to_string_lossy()
            .into_owned()
    };
    match env.media.borrow_mut().linked(&key) {
        Linked::Ready { image, w, h } => Pic::Ready(image, w, h),
        Linked::Loading(_) => Pic::Loading,
        Linked::Failed => Pic::Gone,
    }
}

fn skel() -> gpui::Div {
    div().rounded(px(4.)).bg(white_a(0.06))
}

fn btn(
    id: String,
    label: impl Into<SharedString>,
    primary: bool,
    on: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id(SharedString::from(id))
        .px(px(12.))
        .h(px(28.))
        .flex()
        .items_center()
        .rounded(px(8.))
        .border_1()
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .map(|d| {
            if primary {
                d.border_color(gpui::transparent_black())
                    .bg(c(ACCENT))
                    .text_color(c(0x0e0e0e))
                    .hover(|s| s.opacity(0.92))
            } else {
                d.border_color(white_a(0.12))
                    .bg(white_a(0.06))
                    .text_color(c(0xffffff))
                    .hover(|s| s.bg(white_a(0.12)).border_color(white_a(0.2)))
            }
        })
        .on_click(move |_, w, cx| {
            cx.stop_propagation();
            on(w, cx)
        })
        .child(label.into())
        .into_any_element()
}

/// The cards of a message, when web previews are on.
pub fn cards(ctx: &Ctx, m: &Message) -> Vec<AnyElement> {
    if ctx.held || m.content.is_empty() || ctx.media.borrow().web_previews_off {
        return Vec::new();
    }
    refs(&m.content).iter().map(|r| card(ctx, m, r)).collect()
}

fn card(ctx: &Ctx, m: &Message, r: &Ref) -> AnyElement {
    let state = ctx.media.borrow_mut().embeds.get(&r.entity);
    let key = format!("ne-{}-{}", m.id, r.entity);
    let send = {
        let (act, id) = (ctx.on_act.clone(), m.id.clone());
        let f: Send = Rc::new(move |a: Act, w: &mut gpui::Window, cx: &mut gpui::App| {
            act(&id, RowAct::Embed(a), w, cx)
        });
        f
    };
    let env = Env {
        store: ctx.store,
        media: ctx.media,
        clock: ctx.clock,
        send: send.clone(),
        modal: false,
    };
    let e = match &state {
        State::Ready(e) => Some(e.clone()),
        _ => None,
    };
    let eyebrow = match &e {
        Some(e) if e.reposted_by.is_some() => format!(
            "Reposted by {}",
            ctx.store.name(e.reposted_by.as_deref().unwrap_or_default())
        ),
        Some(e) if e.kind == 1111 => "Nostr Comment".into(),
        Some(e) => match e.class.as_str() {
            "post" => "Nostr Post",
            "article" => "Nostr Article",
            "video" => "Nostr Video",
            "short" => "Nostr Short",
            _ => "Nostr",
        }
        .into(),
        None => "Nostr".into(),
    };
    let mut col = div()
        .mt(px(8.))
        .w_full()
        .max_w(px(CARD_W + 26.))
        .min_w_0()
        .p(px(12.))
        .rounded(px(10.))
        .border_1()
        .border_color(white_a(0.08))
        .bg(ca(10, 10, 10, 0.85))
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            crate::kit::spaced(eyebrow.to_uppercase(), 0.9)
                .font_weight(FontWeight::BOLD)
                .text_size(px(10.))
                .line_height(px(14.))
                .text_color(white_a(0.45)),
        );

    let Some(e) = e else {
        return match state {
            State::Failed(err, _) => {
                let (entity, label) = (r.entity.clone(), ref_label(&r.entity));
                let s = send.clone();
                let copy = btn(
                    format!("{key}-copy"),
                    format!("Copy {label}"),
                    false,
                    move |w, cx| s(Act::Copy(entity.clone(), format!("The {label}")), w, cx),
                );
                let open = r.url.clone().map(|u| {
                    let s = send.clone();
                    btn(
                        format!("{key}-ext"),
                        "Open Externally",
                        false,
                        move |w, cx| s(Act::OpenUrl(u.clone()), w, cx),
                    )
                });
                col.child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(14.))
                        .line_height(px(19.6))
                        .text_color(white_a(0.6))
                        .child("This post couldn't be loaded"),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .line_height(px(15.4))
                        .text_color(white_a(0.45))
                        .child(err),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_end()
                        .gap(px(8.))
                        .child(copy)
                        .children(open),
                )
                .into_any_element()
            }
            _ => col
                .child(
                    div()
                        .h(px(22.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(skel().size(px(22.)).rounded_full())
                        .child(skel().w(px(CARD_W * 0.4)).h(px(12.))),
                )
                .child(skel().w(px(CARD_W * 0.9)).h(px(12.)))
                .child(skel().w(px(CARD_W * 0.6)).h(px(12.)))
                .into_any_element(),
        };
    };

    col = col.child(author(&env, &e, &key, &send));
    if let Some(reply) = &e.reply_to {
        col = col.child(reply_to(reply, &key, &send));
    }
    let reveal_key = format!("{}:{}", m.id, r.entity.to_ascii_lowercase());
    let veiled =
        e.content_warning.is_some() && !ctx.media.borrow().embeds.revealed.contains(&reveal_key);
    if veiled {
        let s = send.clone();
        let warn = e.content_warning.clone().filter(|w| !w.is_empty());
        return col
            .child(
                div()
                    .id(SharedString::from(format!("{key}-veil")))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(10.))
                    .p(px(14.))
                    .border_1()
                    .border_dashed()
                    .border_color(white_a(0.18))
                    .rounded(px(8.))
                    .bg(white_a(0.03))
                    .text_size(px(12.))
                    .line_height(px(16.8))
                    .cursor_pointer()
                    .on_click(move |_, w, cx| {
                        cx.stop_propagation();
                        s(Act::Reveal(reveal_key.clone()), w, cx)
                    })
                    .child(div().min_w_0().text_color(c(0xffffff)).child(match warn {
                        Some(w) => format!("Content warning: {w}"),
                        None => "Content warning".into(),
                    }))
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c(ACCENT))
                            .child("Show"),
                    ),
            )
            .into_any_element();
    }

    let expand = {
        let (s, entity, url) = (send.clone(), r.entity.clone(), r.url.clone());
        let f: Rc<dyn Fn(&mut gpui::Window, &mut gpui::App)> =
            Rc::new(move |w: &mut gpui::Window, cx: &mut gpui::App| {
                s(Act::Expand(entity.clone(), url.clone()), w, cx)
            });
        f
    };
    let mut clamped = false;
    if e.class == "article" {
        let ex = expand.clone();
        let cover = e.image.as_deref().map(|u| match pic(&env, u) {
            Pic::Ready(image, w, h) => {
                let natural = CARD_W * h as f32 / w.max(1) as f32;
                let h = natural.min(200.);
                img(image)
                    .w_full()
                    .h(px(h))
                    .aspect_ratio(CARD_W / h)
                    .object_fit(gpui::ObjectFit::Cover)
                    .rounded(px(8.))
                    .into_any_element()
            }
            Pic::Loading => skel()
                .w_full()
                .h(px(140.))
                .rounded(px(8.))
                .into_any_element(),
            Pic::Gone => div().into_any_element(),
        });
        col = col.child(
            div()
                .id(SharedString::from(format!("{key}-article")))
                .flex()
                .flex_col()
                .gap(px(6.))
                .cursor_pointer()
                .on_click(move |_, w, cx| {
                    cx.stop_propagation();
                    ex(w, cx)
                })
                .children(cover)
                .children(e.title.clone().map(|t| title(t)))
                .children(e.summary.clone().map(|s| {
                    div()
                        .text_size(px(12.))
                        .line_height(px(16.8))
                        .text_color(white_a(0.6))
                        .line_clamp(3)
                        .child(s)
                })),
        );
    } else {
        if let Some(link) = e.link.clone().filter(|_| e.video.is_none()) {
            col = col.child(poster(&env, &e, &key, Some(link), &send));
        } else if let Some(v) = e.video.clone() {
            col = col.child(poster(&env, &e, &key, Some(v.url), &send));
        }
        if let Some(t) = e.title.clone().filter(|_| e.class != "post") {
            col = col.child(title(t));
        }
        if !e.text.is_empty() {
            let (text, c2) = body(&env, &e, &key, &send, &expand);
            clamped = c2;
            col = col.child(text);
        }
        if !e.media.is_empty() {
            col = col.child(media(&env, &e.media, &key));
        }
    }
    let article = e.class == "article";
    if article || clamped || r.url.is_some() {
        let primary = if article {
            let ex = expand.clone();
            Some(btn(
                format!("{key}-read"),
                "Read Article",
                true,
                move |w, cx| ex(w, cx),
            ))
        } else if clamped {
            let ex = expand.clone();
            Some(btn(
                format!("{key}-more"),
                "Show More",
                false,
                move |w, cx| ex(w, cx),
            ))
        } else {
            None
        };
        let ext = r.url.clone().map(|u| {
            let s = send.clone();
            btn(
                format!("{key}-ext"),
                "Open Externally",
                false,
                move |w, cx| s(Act::OpenUrl(u.clone()), w, cx),
            )
        });
        col = col.child(
            div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap(px(8.))
                .children(primary)
                .children(ext),
        );
    }
    col.into_any_element()
}

fn title(t: String) -> gpui::Div {
    div()
        .font_weight(FontWeight::BOLD)
        .text_size(px(15.))
        .line_height(px(19.5))
        .text_color(c(0xffffff))
        .child(t)
}

pub type Send = Rc<dyn Fn(Act, &mut gpui::Window, &mut gpui::App)>;

/// `EmbedAuthor.svelte`: avatar and name open their profile; then how long ago.
fn author(env: &Env, e: &Embed, key: &str, send: &Send) -> AnyElement {
    let avatar = match env.store.avatar_path(&e.author) {
        Some(p) => img(p),
        None => img("icons/user-placeholder.svg"),
    };
    let (s, npub) = (send.clone(), e.author.clone());
    let at = e.published_at.unwrap_or(e.created_at);
    div()
        .h(px(if env.modal { 24. } else { 22. }))
        .flex()
        .items_center()
        .gap(px(8.))
        .min_w_0()
        .child(
            div()
                .id(SharedString::from(format!("{key}-who")))
                .flex()
                .items_center()
                .gap(px(8.))
                .min_w_0()
                .cursor_pointer()
                .on_click(move |_, w, cx| {
                    cx.stop_propagation();
                    s(Act::Profile(npub.clone()), w, cx)
                })
                .child(avatar.size(px(22.)).rounded_full().flex_shrink_0())
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(13.))
                        .line_height(px(if env.modal { 24. } else { 18.2 }))
                        .text_color(c(0xffffff))
                        .child(env.store.name(&e.author)),
                ),
        )
        .when(at > 0, |d| {
            d.child(
                div()
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .line_height(px(if env.modal { 24. } else { 15.4 }))
                    .text_color(white_a(0.45))
                    .child(crate::data::time_ago(env.store.now_ms(), at * 1000)),
            )
        })
        .into_any_element()
}

/// `EmbedReplyTo.svelte`: what a comment answers.
fn reply_to(reply: &ReplyTo, key: &str, send: &Send) -> AnyElement {
    let noun = match reply.kind {
        Some(1) => "a post",
        Some(1111) => "a comment",
        Some(30023) => "an article",
        Some(21 | 34235) => "a video",
        Some(22 | 34236) => "a short",
        Some(1063) => "a file",
        Some(30311) => "a live stream",
        Some(34550) => "a community",
        _ => "a Nostr event",
    };
    let web = reply.external.clone().filter(|x| {
        x.to_ascii_lowercase().starts_with("http://")
            || x.to_ascii_lowercase().starts_with("https://")
    });
    let size = 12.75;
    let tail: AnyElement = match (&reply.entity, web) {
        (Some(entity), _) => {
            let (s, entity) = (send.clone(), entity.clone());
            chip(
                format!("{key}-reply"),
                noun.to_string(),
                size,
                move |w, cx| s(Act::Ref(entity.clone()), w, cx),
            )
        }
        (None, Some(url)) => {
            let s = send.clone();
            chip(format!("{key}-reply"), host(&url), size, move |w, cx| {
                s(Act::OpenUrl(url.clone()), w, cx)
            })
        }
        _ => div()
            .text_color(white_a(0.75))
            .child(reply.external.clone().unwrap_or(noun.to_string()))
            .into_any_element(),
    };
    div()
        .mt(px(2.))
        .mb(px(6.))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(4.))
        .text_size(px(size))
        .line_height(px(size * 1.4))
        .text_color(white_a(0.55))
        .child("Replying to")
        .child(tail)
        .into_any_element()
}

/// `.ne-chip`: a reference that opens when asked.
fn chip(
    id: String,
    label: String,
    size: f32,
    on: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id(SharedString::from(id))
        .px(px(8.))
        .rounded(px(10.))
        .bg(gpui::Hsla {
            a: 0.14,
            ..c(ACCENT)
        })
        .text_color(c(ACCENT))
        .text_size(px(size * 0.9))
        .line_height(px(size * 0.9 * 1.6))
        .font_weight(FontWeight::SEMIBOLD)
        .whitespace_nowrap()
        .cursor_pointer()
        .on_click(move |_, w, cx| {
            cx.stop_propagation();
            on(w, cx)
        })
        .child(label)
        .into_any_element()
}

/// A rough line count for the clamp: the text's width over the card's.
fn lines(text: &str, size: f32) -> usize {
    let per_line = (CARD_W / (size * 0.52)).max(1.) as usize;
    text.lines()
        .map(|l| l.chars().count().div_ceil(per_line).max(1))
        .sum()
}

/// `.ne-text`: a post's text rendered like a message, clamped to eight lines (three when small).
fn body(
    env: &Env,
    e: &Embed,
    key: &str,
    _send: &Send,
    expand: &Rc<dyn Fn(&mut gpui::Window, &mut gpui::App)>,
) -> (AnyElement, bool) {
    let small = e.class != "post";
    let (size, lh, color, max) = if small {
        (12., 16.8, white_a(0.65), 3)
    } else {
        (14., 19.6, white_a(0.88), 8)
    };
    let text: String = e.text.chars().take(4000).collect();
    let clamped = lines(&text, size) > max;
    let ex = expand.clone();
    let fade = clamped.then(|| {
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .h(px(lh * max as f32 * 0.35))
            .bg(gpui::linear_gradient(
                180.,
                gpui::linear_color_stop(ca(9, 9, 9, 0.), 0.),
                gpui::linear_color_stop(c(0x090909), 1.),
            ))
    });
    let el = div()
        .id(SharedString::from(format!("{key}-text")))
        .relative()
        .max_h(px(lh * max as f32))
        .overflow_hidden()
        .when(clamped, |d| {
            d.cursor_pointer().on_click(move |_, w, cx| ex(w, cx))
        })
        .child(rich(
            env,
            &text,
            &e.emoji,
            key,
            Look::new(RUBIK, size, lh, color),
            None,
        ))
        .children(fade)
        .into_any_element();
    (el, clamped)
}

/// Text rendered like a message: markdown, links and mentions.
fn rich(
    env: &Env,
    text: &str,
    emoji: &[vector_core::types::EmojiTag],
    key: &str,
    mut look: Look,
    para_gap: Option<f32>,
) -> AnyElement {
    let empty = HashSet::new();
    look.family = env.family().into();
    let lh = para_gap.unwrap_or(look.line_height);
    let custom = crate::timeline::custom_emoji(env.store, env.media, emoji);
    let blocks = crate::markdown::parse(
        text.trim(),
        &crate::markdown::Ctx {
            base: look,
            revealed: &empty,
            now: env.store.now_ms() as f64 / 1000.,
            mention: Some(&|npub: &str| env.store.name(npub)),
            tags: &[],
            mask: Some(&|t: &str| env.store.stream_mask(t)),
            custom: custom
                .as_ref()
                .map(|f| f as &dyn Fn(&str) -> Option<crate::media::Custom>),
        },
    );
    let s = env.send.clone();
    let click: OnClick = Rc::new(move |t, w, cx| match t {
        crate::inline::Target::Link(u) => s(Act::OpenUrl(u.to_string()), w, cx),
        crate::inline::Target::Mention(npub) => s(Act::Profile(npub.to_string()), w, cx),
        _ => {}
    });
    crate::timeline::stack(&blocks, key, &click, env.clock, lh)
}

/// `.ne-video`: a poster with its play button; the clip or page opens outside the app.
fn poster(env: &Env, e: &Embed, key: &str, open: Option<String>, send: &Send) -> AnyElement {
    let short = e.class == "short";
    let art = e
        .image
        .as_deref()
        .or(e.video.as_ref().and_then(|v| v.poster.as_deref()))
        .map(|u| match pic(env, u) {
            Pic::Ready(image, ..) => img(image)
                .absolute()
                .inset_0()
                .size_full()
                .object_fit(gpui::ObjectFit::Cover)
                .into_any_element(),
            Pic::Loading => skel()
                .absolute()
                .inset_0()
                .size_full()
                .rounded(px(0.))
                .into_any_element(),
            Pic::Gone => div().into_any_element(),
        });
    let badge = open.as_deref().filter(|_| e.video.is_none()).map(host);
    let s = send.clone();
    let frame = div()
        .id(SharedString::from(format!("{key}-poster")))
        .group(SharedString::from(format!("{key}-poster")))
        .relative()
        .overflow_hidden()
        .rounded(px(8.))
        .bg(c(0x0b0b0b))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .map(|d| {
            if short {
                d.h(px(360.)).w(px(202.5)).mx_auto()
            } else {
                d.w_full().h(px(CARD_W * 9. / 16.))
            }
        })
        .when_some(open, |d, u| {
            d.on_click(move |_, w, cx| {
                cx.stop_propagation();
                s(Act::OpenUrl(u.clone()), w, cx)
            })
        })
        .children(art)
        .child(
            div()
                .relative()
                .size(px(56.))
                .rounded_full()
                .border_1()
                .border_color(white_a(0.15))
                .bg(ca(0, 0, 0, 0.6))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    svg()
                        .path("icons/play.svg")
                        .size(px(26.))
                        .text_color(c(0xffffff)),
                ),
        )
        .children(badge.map(|b| {
            div()
                .absolute()
                .right(px(8.))
                .bottom(px(8.))
                .px(px(6.))
                .py(px(2.))
                .rounded(px(6.))
                .bg(ca(0, 0, 0, 0.72))
                .text_color(c(0xffffff))
                .text_size(px(11.))
                .line_height(px(14.3))
                .font_weight(FontWeight::SEMIBOLD)
                .child(b)
        }));
    frame.into_any_element()
}

/// `EmbedMedia.svelte`: up to four pictures, two across when there are several.
fn media(env: &Env, all: &[Media], key: &str) -> AnyElement {
    let shown = &all[..all.len().min(MEDIA_SHOWN)];
    let grid = shown.len() > 1;
    let side = (CARD_W - 4.) / 2.;
    let tiles = shown.iter().enumerate().map(|(i, m)| {
        let url = if m.is_video {
            m.poster.as_deref()
        } else {
            Some(m.url.as_str())
        };
        let ratio = match (m.width, m.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some(w as f32 / h as f32),
            _ => None,
        };
        let size = |d: gpui::Div, natural: Option<f32>| {
            if grid {
                d.w(px(side)).h(px(side))
            } else {
                let r = natural.or(ratio).unwrap_or(16. / 9.);
                d.w_full().h(px((CARD_W / r).min(320.)))
            }
        };
        match url.map(|u| pic(env, u)) {
            Some(Pic::Ready(image, w, h)) => {
                let open = crate::viewer::Open {
                    image: image.clone(),
                    w,
                    h,
                    full: None,
                };
                let send = env.send.clone();
                let natural = Some(w as f32 / h.max(1) as f32);
                let (tw, th) = if grid {
                    (side, side)
                } else {
                    (CARD_W, (CARD_W * h as f32 / w.max(1) as f32).min(320.))
                };
                size(div(), natural)
                    .id(SharedString::from(format!("{key}-m{i}")))
                    .cursor_pointer()
                    .on_click(move |_, win, cx| {
                        cx.stop_propagation();
                        send(Act::View(open.clone()), win, cx)
                    })
                    .child(
                        img(image)
                            .w(px(tw))
                            .h(px(th))
                            .aspect_ratio(tw / th)
                            .object_fit(gpui::ObjectFit::Cover)
                            .rounded(px(8.)),
                    )
                    .into_any_element()
            }
            Some(Pic::Loading) => size(skel(), None)
                .h(px(if grid { side } else { 96. }))
                .rounded(px(8.))
                .into_any_element(),
            _ => div().into_any_element(),
        }
    });
    let more = (all.len() > MEDIA_SHOWN).then(|| {
        div()
            .absolute()
            .right(px(8.))
            .bottom(px(8.))
            .px(px(8.))
            .py(px(2.))
            .rounded(px(10.))
            .bg(ca(0, 0, 0, 0.72))
            .text_color(c(0xffffff))
            .text_size(px(12.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(format!("+{}", all.len() - MEDIA_SHOWN))
    });
    div()
        .relative()
        .flex()
        .gap(px(4.))
        .map(|d| if grid { d.flex_wrap() } else { d.flex_col() })
        .children(tiles)
        .children(more)
        .into_any_element()
}

// ---------------------------------------------------------------------------------------------
// The expanded view (`NostrEmbedModal.svelte`).

/// One post the expanded view shows; Back returns to the one under it.
pub struct View {
    pub embed: Arc<Embed>,
    pub url: Option<String>,
    pub revealed: bool,
}

pub struct Modal {
    pub stack: Vec<View>,
    pub scroll: gpui::ScrollHandle,
}

fn round_btn(
    id: &'static str,
    icon: &'static str,
    on: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .absolute()
        .top(px(12.))
        .size(px(30.))
        .rounded_full()
        .bg(white_a(0.06))
        .border_1()
        .border_color(white_a(0.08))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(white_a(0.12)).border_color(white_a(0.18)))
        .on_click(move |_, w, cx| {
            cx.stop_propagation();
            on(w, cx)
        })
        .child(svg().path(icon).size(px(14.)).text_color(c(0xffffff)))
}

pub fn modal(m: &Modal, env: &Env, viewport_h: f32) -> AnyElement {
    let Some(v) = m.stack.last() else {
        return div().into_any_element();
    };
    let e = &v.embed;
    let back = m.stack.len() > 1;
    let article = e.class == "article";
    let key = format!("nem-{}", m.stack.len());
    let send = env.send.clone();
    let reposter = e.reposted_by.as_deref().map(|n| env.store.name(n));
    let veiled = e.content_warning.is_some() && !v.revealed;
    let pad_first = |d: gpui::Div| if back { d.pl(px(36.)) } else { d };

    let mut col = div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .px(px(20.))
        .pt(px(20.))
        .pb(px(12.));
    let mut first = true;
    if let Some(name) = reposter {
        col = col.child(
            pad_first(div()).child(
                crate::kit::spaced(format!("REPOSTED BY {}", name.to_uppercase()), 0.9)
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(10.))
                    .line_height(px(14.))
                    .text_color(white_a(0.45)),
            ),
        );
        first = false;
    }
    let author_row = |first: bool| {
        let row = div().pr(px(36.)).child(author(env, e, &key, &send));
        if first && back { row.pl(px(36.)) } else { row }
    };
    if veiled {
        let s = send.clone();
        col = col.child(author_row(first)).child(
            div()
                .id("nem-veil")
                .flex()
                .items_center()
                .justify_between()
                .gap(px(10.))
                .p(px(14.))
                .border_1()
                .border_dashed()
                .border_color(white_a(0.18))
                .rounded(px(8.))
                .bg(white_a(0.03))
                .text_size(px(12.))
                .line_height(px(16.8))
                .cursor_pointer()
                .on_click(move |_, w, cx| s(Act::Reveal(String::new()), w, cx))
                .child(div().min_w_0().text_color(c(0xffffff)).child(
                    match e.content_warning.as_deref().filter(|w| !w.is_empty()) {
                        Some(w) => format!("Content warning: {w}"),
                        None => "Content warning".into(),
                    },
                ))
                .child(
                    div()
                        .flex_shrink_0()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c(ACCENT))
                        .child("Show"),
                ),
        );
    } else {
        let width = if article { 720. } else { 560. };
        if article && let Some(u) = e.image.as_deref() {
            let cover = match pic(env, u) {
                Pic::Ready(image, w, h) => {
                    let h = (width * h as f32 / w.max(1) as f32).min(280.);
                    Some(
                        img(image)
                            .w(px(width))
                            .h(px(h))
                            .aspect_ratio(width / h)
                            .object_fit(gpui::ObjectFit::Cover)
                            .into_any_element(),
                    )
                }
                Pic::Loading => Some(
                    skel()
                        .w(px(width))
                        .h(px(180.))
                        .rounded(px(0.))
                        .into_any_element(),
                ),
                Pic::Gone => None,
            };
            if let Some(cover) = cover {
                col = col.child(div().mx(px(-20.)).mt(px(-20.)).child(cover));
            }
        }
        col = col.child(author_row(first && !(article && e.image.is_some())));
        if let Some(r) = &e.reply_to {
            col = col.child(reply_to(r, &key, &send));
        }
        if let Some(t) = &e.title {
            col = col.child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(22.))
                    .line_height(px(27.5))
                    .text_color(c(0xffffff))
                    .child(t.clone()),
            );
        }
        if let Some(s) = e.summary.clone().filter(|_| article) {
            col = col.child(
                div()
                    .text_size(px(14.))
                    .line_height(px(20.3))
                    .text_color(white_a(0.6))
                    .child(s),
            );
        }
        if let Some(v) = e.video.clone() {
            col = col.child(poster(env, e, &key, Some(v.url), &send));
        } else if let Some(link) = e.link.clone() {
            col = col.child(poster(env, e, &key, Some(link), &send));
        }
        let quotes = refs(&e.text);
        if !e.text.is_empty() {
            // An article's paragraphs keep `.markdown-paragraph`'s 0.8em under them.
            let gap = article.then_some(23.25 * 2. + 12.);
            col = col.child(div().when(article, |d| d.pb(px(12.))).child(rich(
                env,
                &strip(&e.text),
                &e.emoji,
                &key,
                Look::new(RUBIK, 15., 23.25, white_a(0.9)),
                gap,
            )));
        }
        if !e.media.is_empty() {
            col = col.child(media(env, &e.media, &key));
        }
        for q in quotes.iter() {
            col = col.child(quote(env, q, &key));
        }
    }

    let label = if e.bech32.starts_with("naddr") {
        "naddr"
    } else {
        "nevent"
    };
    let actions = {
        let (s, bech, lbl) = (send.clone(), e.bech32.clone(), format!("The {label}"));
        let copy = btn(
            "nem-copy".into(),
            format!("Copy {label}"),
            false,
            move |w, cx| s(Act::Copy(bech.clone(), lbl.clone()), w, cx),
        );
        let ext = v.url.clone().map(|u| {
            let s = send.clone();
            btn("nem-ext".into(), "Open Externally", false, move |w, cx| {
                s(Act::OpenUrl(u.clone()), w, cx)
            })
        });
        div()
            .flex()
            .flex_wrap()
            .justify_end()
            .gap(px(8.))
            .px(px(20.))
            .pt(px(12.))
            .pb(px(16.))
            .border_t_1()
            .border_color(white_a(0.06))
            .child(copy)
            .children(ext)
    };
    let (s1, s2, s3) = (send.clone(), send.clone(), send.clone());
    let card = div()
        .id("nem-card")
        .font_family(INTER)
        .relative()
        .w_full()
        .max_w(px(if article { 720. } else { 560. }))
        .max_h(px(viewport_h - 48.))
        .bg(c(0x141414))
        .border_1()
        .border_color(white_a(0.08))
        .rounded(px(18.))
        .shadow(vec![gpui::BoxShadow {
            color: ca(0, 0, 0, 0.6),
            offset: gpui::point(px(0.), px(24.)),
            blur_radius: px(60.),
            spread_radius: px(0.),
            inset: false,
        }])
        .overflow_hidden()
        .flex()
        .flex_col()
        .occlude()
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(
            div()
                .id("nem-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&m.scroll)
                .child(col),
        )
        .child(actions)
        .when(back, |d| {
            d.child(
                round_btn("nem-back", "icons/chevron-left.svg", move |w, cx| {
                    s1(Act::Back, w, cx)
                })
                .left(px(12.)),
            )
        })
        .child(
            round_btn("nem-close", "icons/x.svg", move |w, cx| {
                s2(Act::Close, w, cx)
            })
            .right(px(12.)),
        );
    gpui::deferred(
        div()
            .id("nem-overlay")
            .absolute()
            .inset_0()
            .p(px(24.))
            .bg(ca(0, 0, 0, 0.78))
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .on_click(move |_, w, cx| s3(Act::Close, w, cx))
            .child(card),
    )
    .with_priority(13)
    .into_any_element()
}

/// `QuoteCard.svelte`: a post the expanded one quotes. Opening it moves the view there; what
/// it quotes in turn waits to be asked for.
fn quote(env: &Env, r: &Ref, key: &str) -> AnyElement {
    let state = env.media.borrow_mut().embeds.get(&r.entity);
    let id = SharedString::from(format!("{key}-q-{}", r.entity));
    let e = match &state {
        State::Ready(e) => Some(e.clone()),
        _ => None,
    };
    let label = match &e {
        Some(e) if e.reposted_by.is_some() => "Quoted Repost",
        Some(e) if e.kind == 1111 => "Quoted Comment",
        Some(e) => match e.class.as_str() {
            "post" => "Quoted Post",
            "article" => "Quoted Article",
            "video" => "Quoted Video",
            "short" => "Quoted Short",
            _ => "Quote",
        },
        None => "Quote",
    };
    let mut col = div()
        .id(id)
        .flex()
        .flex_col()
        .gap(px(6.))
        .px(px(12.))
        .py(px(10.))
        .rounded(px(10.))
        .border_1()
        .border_color(white_a(0.1))
        .bg(white_a(0.03))
        .min_w_0()
        .child(
            crate::kit::spaced(label.to_uppercase(), 0.9)
                .font_weight(FontWeight::BOLD)
                .text_size(px(10.))
                .line_height(px(24.))
                .text_color(white_a(0.45)),
        );
    match (&state, e) {
        (State::Failed(err, _), _) => {
            col = col.child(
                div()
                    .text_size(px(11.))
                    .line_height(px(15.4))
                    .text_color(white_a(0.45))
                    .child(err.clone()),
            )
        }
        (_, Some(e)) => {
            let veiled = e.content_warning.is_some();
            let thumb = (!veiled)
                .then(|| {
                    e.image
                        .clone()
                        .or_else(|| e.media.iter().find(|m| !m.is_video).map(|m| m.url.clone()))
                        .or_else(|| e.media.iter().find_map(|m| m.poster.clone()))
                })
                .flatten()
                .and_then(|u| match pic(env, &u) {
                    Pic::Ready(image, ..) => Some(
                        img(image)
                            .size(px(56.))
                            .aspect_ratio(1.)
                            .flex_shrink_0()
                            .object_fit(gpui::ObjectFit::Cover)
                            .rounded(px(6.))
                            .into_any_element(),
                    ),
                    Pic::Loading => Some(
                        skel()
                            .size(px(56.))
                            .flex_shrink_0()
                            .rounded(px(6.))
                            .into_any_element(),
                    ),
                    Pic::Gone => None,
                });
            let text = if veiled {
                Some(
                    div()
                        .italic()
                        .text_size(px(13.))
                        .line_height(px(18.2))
                        .text_color(white_a(0.5))
                        .child(
                            match e.content_warning.as_deref().filter(|w| !w.is_empty()) {
                                Some(w) => format!("Content warning: {w}"),
                                None => "Content warning".into(),
                            },
                        )
                        .into_any_element(),
                )
            } else if !e.text.is_empty() {
                let t: String = e.text.chars().take(600).collect();
                Some(
                    div()
                        .max_h(px(18.2 * 3.))
                        .overflow_hidden()
                        .child(rich(
                            env,
                            &t,
                            &e.emoji,
                            &format!("{key}-qt"),
                            Look::new(RUBIK, 13., 18.2, white_a(0.75)),
                            None,
                        ))
                        .into_any_element(),
                )
            } else {
                None
            };
            let (s, entity, url) = (env.send.clone(), r.entity.clone(), r.url.clone());
            col = col
                .cursor_pointer()
                .hover(|d| {
                    d.border_color(gpui::Hsla {
                        a: 0.45,
                        ..c(ACCENT)
                    })
                })
                .on_click(move |_, w, cx| {
                    cx.stop_propagation();
                    s(Act::Push(entity.clone(), url.clone()), w, cx)
                })
                .child(author(env, &e, key, &env.send))
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .min_w_0()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .children(e.title.clone().map(|t| {
                                    div()
                                        .font_weight(FontWeight::BOLD)
                                        .text_size(px(13.))
                                        .text_color(c(0xffffff))
                                        .child(t)
                                }))
                                .children(text),
                        )
                        .children(thumb),
                );
        }
        _ => {
            col = col
                .child(
                    div()
                        .h(px(22.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(skel().size(px(22.)).rounded_full())
                        .child(skel().w(px(160.)).h(px(12.))),
                )
                .child(skel().w(px(300.)).h(px(12.)));
        }
    }
    col.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "note1wg33qsaurqr7da6qkg67kag3ajeny4dx5d65x433r9k732t4p49sk04fpd";
    const NADDR: &str = "naddr1qqx8yetvv9uj6er9wd5kwmszyq4aspkf0u8qptc6rlpn9ra8vw5jd9ererdcltz0jwhhrkccd4hfqqcyqqq823cyhyg2k";

    #[test]
    fn bare_and_prefixed_refs_are_cut() {
        let text = format!("Have you seen this? nostr:{NOTE} so good");
        assert_eq!(refs(&text).len(), 1);
        assert_eq!(strip(&text), "Have you seen this? so good");
    }

    #[test]
    fn a_web_link_keeps_its_text() {
        let text = format!("Great https://njump.me/{NADDR}.");
        let r = refs(&text);
        assert_eq!(r.len(), 1);
        assert_eq!(
            r[0].url.as_deref(),
            Some(format!("https://njump.me/{NADDR}").as_str())
        );
        assert_eq!(strip(&text), text);
    }

    #[test]
    fn code_and_link_labels_are_left_alone() {
        assert!(refs(&format!("`{NOTE}`")).is_empty());
        assert!(refs(&format!("[label](nostr:{NOTE})")).is_empty());
        assert!(refs(&format!("<https://njump.me/{NOTE}>")).is_empty());
        assert!(refs(&format!("x{NOTE}")).is_empty());
    }

    #[test]
    fn duplicates_count_once() {
        assert_eq!(refs(&format!("{NOTE} nostr:{NOTE}")).len(), 1);
    }

    #[test]
    fn previews_name_what_they_point_at() {
        assert_eq!(
            for_preview(&format!("look nostr:{NADDR}")),
            "look Nostr Article"
        );
    }
}
