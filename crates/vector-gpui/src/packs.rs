//! Emoji packs shared in a message, as cards (`PackPreviewCard.svelte`, `js/emoji/pack-previews.js`):
//! the pack's thumbs, logo, title and count, with Copy and Add / Remove.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AnyElement, FontWeight, IntoElement, ParentElement, SharedString, Styled, div, img, prelude::*, px, svg};
use vector_core::emoji_packs::EmojiPack;
use vector_core::types::Message;

use crate::media::Custom;
use crate::theme::*;
use crate::timeline::{Act as RowAct, Ctx};

const ERR_TTL: Duration = Duration::from_secs(10);
pub const KIND_EMOJI_SET: u16 = 30030;
const CARD_W: f32 = 420.;
const META_W: f32 = 140.;
const CELL_H: f32 = 32.;
const THUMB: f32 = 28.;
const GAP: f32 = 4.;

#[derive(Clone)]
pub enum State {
    Loading,
    Ready(Arc<EmojiPack>),
    Failed(String, Instant),
}

#[derive(Default)]
pub struct Cache {
    states: HashMap<String, State>,
    pub queue: Vec<String>,
    /// Equipped packs, by id; a theme's pinned pack is not one.
    pub subscribed: HashSet<String>,
    /// Packs mid Add or Remove.
    pub busy: HashSet<String>,
    /// The pack whose share link was just copied.
    pub copied: Option<(String, Instant)>,
}

impl Cache {
    pub fn get(&mut self, naddr: &str) -> State {
        let key = naddr.to_ascii_lowercase();
        match self.states.get(&key) {
            Some(State::Failed(_, at)) if at.elapsed() >= ERR_TTL => {}
            Some(s) => return s.clone(),
            None => {}
        }
        self.states.insert(key.clone(), State::Loading);
        self.queue.push(key);
        State::Loading
    }

    pub fn settle(&mut self, naddr: &str, res: Result<EmojiPack, String>) {
        let state = match res {
            Ok(p) => State::Ready(Arc::new(p)),
            Err(e) => State::Failed(e, Instant::now()),
        };
        self.states.insert(naddr.to_ascii_lowercase(), state);
    }
}

#[derive(Clone)]
pub enum Act {
    Copy(String),
    Toggle(String),
}

const BECH32: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

fn naddr_len(s: &[u8]) -> Option<usize> {
    if s.len() < 6 || !s[..6].eq_ignore_ascii_case(b"naddr1") {
        return None;
    }
    let body = s[6..].iter().take_while(|b| BECH32.contains(&b.to_ascii_lowercase())).count();
    (body >= 20).then_some(6 + body)
}

fn is_pack(naddr: &str) -> bool {
    vector_core::emoji_packs::parse_naddr(naddr).is_ok_and(|a| a.kind == KIND_EMOJI_SET)
}

/// Where each pack reference sits: a vectorapp.io share link, a `nostr:` URI, or a bare
/// naddr at a text boundary (one inside some other site's link is that site's).
fn scan(text: &str) -> Vec<(usize, usize, String)> {
    let b = text.as_bytes();
    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let rest = &lower[i..];
        let prefix = ["https://www.vectorapp.io/emojis/pack/", "http://www.vectorapp.io/emojis/pack/", "https://vectorapp.io/emojis/pack/", "http://vectorapp.io/emojis/pack/", "nostr:"]
            .iter()
            .find(|p| rest.starts_with(**p))
            .map(|p| p.len());
        if let Some(p) = prefix
            && let Some(len) = naddr_len(&b[i + p..])
        {
            let naddr = lower[i + p..i + p + len].to_string();
            let mut end = i + p + len;
            if lower[end..].starts_with(".html") {
                end += 5;
            }
            if b.get(end) == Some(&b'/') {
                end += 1;
            }
            out.push((i, end, naddr));
            i = end;
            continue;
        }
        let boundary = i == 0 || b" \t\n\r([{<\"'".contains(&b[i - 1]);
        if boundary && let Some(len) = naddr_len(&b[i..]) {
            out.push((i, i + len, lower[i..i + len].to_string()));
            i += len;
            continue;
        }
        if let Some(len) = naddr_len(&b[i..]) {
            i += len;
            continue;
        }
        i += 1;
    }
    out.retain(|(_, _, n)| is_pack(n));
    out
}

/// The distinct packs a message shares, in order.
pub fn refs(text: &str) -> Vec<String> {
    if !text.contains("naddr1") && !text.to_ascii_lowercase().contains("naddr1") {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    scan(text).into_iter().map(|(_, _, n)| n).filter(|n| seen.insert(n.clone())).collect()
}

/// The text without the pack references its cards stand in for.
pub fn strip(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.to_ascii_lowercase().contains("naddr1") {
        return text.into();
    }
    let cuts = scan(text);
    if cuts.is_empty() {
        return text.into();
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (s, e, _) in cuts {
        out.push_str(&text[at..s]);
        at = e;
    }
    out.push_str(&text[at..]);
    crate::embeds::tidy(&out).into()
}

fn skel() -> gpui::Div {
    div().rounded(px(6.)).bg(white_a(0.06))
}

/// `_packPreviewLayout`: as many 32px cells as the column fits, three rows (two when narrow).
fn layout(width: f32) -> (usize, usize) {
    let rows = if width < 240. { 2 } else { 3 };
    let cols = (((width + GAP) / (CELL_H + GAP)).floor() as usize).max(4);
    (cols, rows)
}

pub fn cards(ctx: &Ctx, m: &Message) -> Vec<AnyElement> {
    if ctx.held || m.content.is_empty() {
        return Vec::new();
    }
    refs(&m.content).into_iter().map(|n| card(ctx, m, n)).collect()
}

fn card(ctx: &Ctx, m: &Message, naddr: String) -> AnyElement {
    let state = ctx.media.borrow_mut().packs.get(&naddr);
    let key = format!("pk-{}-{naddr}", m.id);
    let grid_w = CARD_W - 12. - META_W;
    let (cols, rows) = layout(grid_w);
    let cell_w = (grid_w - GAP * (cols - 1) as f32) / cols as f32;
    let send = {
        let (act, id) = (ctx.on_act.clone(), m.id.clone());
        move |a: Act, w: &mut gpui::Window, cx: &mut gpui::App| act(&id, RowAct::Pack(a), w, cx)
    };

    let (grid, meta): (AnyElement, Vec<AnyElement>) = match &state {
        State::Loading => (
            div().w(px(grid_w)).flex().flex_wrap().gap(px(GAP)).children((0..6).map(|_| skel().w(px((grid_w - GAP * 5.) / 6.)).h(px(28.)))).into_any_element(),
            vec![
                div().w_full().flex().items_center().gap(px(6.)).child(skel().size(px(22.)).rounded_full()).child(skel().w(px(META_W * 0.6)).h(px(14.)).rounded(px(4.))).into_any_element(),
                skel().w(px(META_W * 0.38)).h(px(11.)).mt(px(6.)).rounded(px(4.)).into_any_element(),
                skel().w_full().h(px(28.)).mt(px(10.)).into_any_element(),
            ],
        ),
        State::Failed(err, _) => (
            div().w(px(grid_w)).into_any_element(),
            vec![
                div().font_weight(FontWeight::SEMIBOLD).text_size(px(14.)).line_height(px(19.6)).text_color(c(0xffffff)).child("Pack unavailable").into_any_element(),
                div().text_size(px(11.)).line_height(px(14.85)).text_color(white_a(0.55)).line_clamp(2).child(if err.is_empty() { "Failed to fetch".to_string() } else { err.clone() }).into_any_element(),
            ],
        ),
        State::Ready(pack) => {
            let count = pack.emojis.len();
            let grid: AnyElement = if count == 0 {
                div().w(px(grid_w)).py(px(20.)).text_center().text_size(px(13.)).text_color(white_a(0.4)).child("Empty pack").into_any_element()
            } else {
                let shown = &pack.emojis[..count.min(cols * rows)];
                let used_rows = shown.len().div_ceil(cols);
                let mut g = div().relative().w(px(grid_w)).h(px((CELL_H * used_rows as f32).min(96.))).overflow_hidden().rounded(px(6.));
                for (i, e) in shown.iter().enumerate() {
                    let (col, row) = (i % cols, i / cols);
                    let x = col as f32 * (cell_w + GAP) + (cell_w - THUMB) / 2.;
                    let y = row as f32 * CELL_H + (CELL_H - THUMB) / 2.;
                    let src = crate::timeline::emoji_source(ctx.store, &e.url);
                    if let Custom::Ready(image) = ctx.media.borrow_mut().custom(&src, false) {
                        g = g.child(img(image).absolute().left(px(x)).top(px(y)).size(px(THUMB)).aspect_ratio(1.).object_fit(gpui::ObjectFit::Contain));
                    }
                }
                if count > cols * (rows - 1) {
                    g = g.child(div().absolute().left_0().right_0().bottom_0().h(px(32.)).bg(gpui::linear_gradient(
                        180.,
                        gpui::linear_color_stop(ca(10, 10, 10, 0.), 0.),
                        gpui::linear_color_stop(ca(10, 10, 10, 0.85), 1.),
                    )));
                }
                g.into_any_element()
            };
            let logo = (!pack.image_url.is_empty())
                .then(|| ctx.media.borrow_mut().custom(&crate::timeline::emoji_source(ctx.store, &pack.image_url), true))
                .map(|c| match c {
                    Custom::Ready(image) => img(image).size(px(22.)).aspect_ratio(1.).rounded(px(4.)).object_fit(gpui::ObjectFit::Cover).flex_shrink_0().into_any_element(),
                    _ => div().size(px(22.)).flex_shrink_0().into_any_element(),
                });
            let media = ctx.media.borrow();
            let subscribed = media.packs.subscribed.contains(&pack.id);
            let busy = media.packs.busy.contains(&pack.id);
            let copied = media.packs.copied.as_ref().is_some_and(|(n, at)| *n == naddr && at.elapsed() < Duration::from_millis(1500));
            drop(media);
            let label = match (busy, subscribed) {
                (true, true) => "Removing…",
                (true, false) => "Adding…",
                (false, true) => "Remove",
                (false, false) => "Add Pack",
            };
            let (s1, s2, n1, n2) = (send.clone(), send, naddr.clone(), naddr.clone());
            let actions = div()
                .mt_auto()
                .w_full()
                .h(px(26.))
                .flex()
                .gap(px(6.))
                .child(
                    div()
                        .id(SharedString::from(format!("{key}-copy")))
                        .w(px(28.))
                        .h_full()
                        .flex_shrink_0()
                        .rounded(px(6.))
                        .bg(white_a(0.06))
                        .hover(|s| s.bg(white_a(0.12)))
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_center()
                        .on_click(move |_, w, cx| {
                            cx.stop_propagation();
                            s1(Act::Copy(n1.clone()), w, cx)
                        })
                        .child(svg().path(if copied { "icons/check.svg" } else { "icons/copy.svg" }).size(px(14.)).text_color(c(0xffffff))),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("{key}-add")))
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .rounded(px(6.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(12.))
                        .map(|d| if subscribed { d.bg(white_a(0.08)).text_color(white_a(0.85)) } else { d.bg(c(PRIMARY)).text_color(c(0x000000)) })
                        .when(busy, |d| d.opacity(0.5))
                        .when(!busy, |d| {
                            d.cursor_pointer().hover(|s| s.opacity(0.85)).on_click(move |_, w, cx| {
                                cx.stop_propagation();
                                s2(Act::Toggle(n2.clone()), w, cx)
                            })
                        })
                        .child(label),
                );
            (
                grid,
                vec![
                    div()
                        .w_full()
                        .h(px(22.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .min_w_0()
                        .children(logo)
                        .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).text_size(px(14.)).line_height(px(19.6)).text_color(c(0xffffff)).child(if pack.title.is_empty() { pack.identifier.clone() } else { pack.title.clone() }))
                        .into_any_element(),
                    div().text_size(px(11.)).line_height(px(15.4)).text_color(white_a(0.5)).child(format!("{count} emoji{}", if count == 1 { "" } else { "s" })).into_any_element(),
                    (!pack.description.is_empty())
                        .then(|| div().text_size(px(11.)).line_height(px(14.85)).text_color(white_a(0.55)).line_clamp(2).child(pack.description.clone()).into_any_element())
                        .unwrap_or_else(|| div().into_any_element()),
                    actions.into_any_element(),
                ],
            )
        }
    };
    div()
        .mt(px(8.))
        .w_full()
        .max_w(px(CARD_W + 22.))
        .p(px(10.))
        .rounded(px(10.))
        .border_1()
        .border_color(white_a(0.08))
        .bg(ca(10, 10, 10, 0.85))
        .flex()
        .gap(px(12.))
        .child(div().flex_shrink_0().child(grid))
        .child(div().w(px(META_W)).flex_shrink_0().flex().flex_col().items_start().gap(px(2.)).min_w_0().children(meta))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACK: &str = "naddr1qqzkymr0vfesygypkcma3lxjcmdxxk0xjcc38gghphnete9hykuy68stfn7ea3vvaypsgqqqw48qsh6h7r";
    const ARTICLE: &str = "naddr1qqx8yetvv9uj6er9wd5kwmszyq4aspkf0u8qptc6rlpn9ra8vw5jd9ererdcltz0jwhhrkccd4hfqqcyqqq823cyhyg2k";

    #[test]
    fn share_links_and_bare_packs_are_found_and_cut() {
        let text = format!("Grab this pack https://vectorapp.io/emojis/pack/{PACK}");
        assert_eq!(refs(&text), vec![PACK.to_string()]);
        assert_eq!(strip(&text), "Grab this pack");
        assert_eq!(strip(&format!("a {PACK} b")), "a b");
    }

    #[test]
    fn other_kinds_and_foreign_links_are_left() {
        assert!(refs(&format!("nostr:{ARTICLE}")).is_empty());
        assert!(refs(&format!("https://njump.me/{PACK}")).is_empty());
    }
}
