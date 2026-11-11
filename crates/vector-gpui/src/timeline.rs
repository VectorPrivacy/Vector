//! One message row (`MessageRow.svelte` + `MessageContent.svelte`): the gutter avatar, the
//! header, a reply's quote, the markdown body and the reactions, with the `.dmsg-*` metrics
//! measured from the Tauri render.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div,
    img, prelude::*, px, svg,
};
use pulldown_cmark::Alignment;
use vector_core::chat::SerializableChat;
use vector_core::types::Message;

use crate::data::{self, Store};
use crate::emoji;
use crate::inline::{Atom, Kind, Look, OnClick, Target, inline};
use crate::markdown::{self, Block, MONO, Para, ParaKind};
use crate::media::Linked;
use crate::theme::*;

pub const DANGER: u32 = 0xff2ea9;
pub const TEXT_SIZE: f32 = 15.;
pub const TEXT_LH: f32 = 21.;

/// What the user did to a message.
#[derive(Clone)]
pub enum Act {
    Text(Target),
    Hover(bool),
    React,
    /// A reaction chip, or a quick pick: react with this emoji.
    ReactWith(String),
    Reply,
    Edit,
    Retry,
    Delete,
    /// Fetch this attachment (or retry it).
    Download(String),
    CancelDownload(String),
    /// Stop an upload still sending, by its pending message.
    CancelUpload(String),
    /// Tap a spoiler picture open.
    RevealMedia(String),
    /// A file on this device: show it, or save a held one.
    OpenFile(String),
    /// Open a picture full screen.
    View(crate::viewer::Open),
    /// An invite card's button: join its Community, or open it once joined.
    Invite(String),
    /// A Nostr card's controls.
    Embed(crate::embeds::Act),
    /// An emoji pack card's controls.
    Pack(crate::packs::Act),
    /// A reply's quote: go to the message it answers.
    Jump(String),
    /// An audio attachment's controls, by attachment.
    Audio(String, crate::audio::Act),
}

pub type OnAct = Rc<dyn Fn(&str, Act, &mut Window, &mut App)>;

/// What the rows of one chat share while drawing.
pub struct Ctx<'a> {
    pub store: &'a Store,
    pub chat: &'a SerializableChat,
    /// Spoilers revealed, by message.
    pub revealed: &'a HashMap<String, HashSet<u32>>,
    /// Seconds, for animated pieces.
    pub clock: f32,
    pub on_act: OnAct,
    /// The row under the pointer, which carries the toolbar.
    pub hovered: Option<&'a str>,
    /// The message the composer is replying to.
    pub replying: Option<&'a str>,
    /// The message whose reaction menu is open.
    pub picker: Option<&'a str>,
    /// Why a send failed, by message.
    pub fail_reasons: &'a HashMap<String, String>,
    /// Attachment pictures, blurs and transfers.
    pub media: &'a std::cell::RefCell<crate::media::Media>,
    /// Device pixels per pixel, for decoding pictures at the size they draw.
    pub scale: f32,
    /// A DM request: what its sender sent stays unloaded.
    pub held: bool,
    /// Invite links looked up, by invite key.
    pub invites: &'a HashMap<String, crate::newchat::Preview>,
    /// The message a jump landed on, and seconds since.
    pub jumped: Option<(&'a str, f32)>,
}

/// `highlightMessage`: in to 20%, held to 80%, out, over 1.5s eased out.
pub fn jump_flash(t: f32) -> f32 {
    let p = 1. - (1. - (t / 1.5).clamp(0., 1.)).powi(2);
    let a = if p < 0.2 {
        p / 0.2
    } else if p < 0.8 {
        1.
    } else {
        (1. - p) / 0.2
    };
    0.18 * a
}

pub fn base_look() -> Look {
    Look::new(RUBIK, TEXT_SIZE, TEXT_LH, white_a(0.85))
}

fn avatar(store: &Store, npub: &str, size: f32) -> AnyElement {
    match store.avatar_path(npub) {
        Some(path) => img(path)
            .size(px(size))
            .rounded_full()
            .flex_shrink_0()
            .into_any_element(),
        None => img("icons/user-placeholder.svg")
            .size(px(size))
            .rounded_full()
            .flex_shrink_0()
            .into_any_element(),
    }
}

/// The row of `chat.messages[index]`. `first` heads a streak; `gap` is the space above it.
pub fn message_row(ctx: &Ctx, index: usize, first: bool, gap: f32) -> AnyElement {
    let m = &ctx.chat.messages[index];
    let author = author_of(ctx, m);
    let quote = (!m.replied_to.is_empty())
        .then(|| reply_quote(ctx, m))
        .flatten();
    let has_quote = quote.is_some();
    let failed = m.mine && m.failed;
    let pending = m.mine && m.pending && !failed;
    let group: SharedString = format!("dmsg-{}", m.id).into();
    let replying = ctx.replying == Some(m.id.as_str());
    let hovered = ctx.hovered == Some(m.id.as_str());

    let gutter = div()
        .relative()
        .w(px(56.))
        .flex_shrink_0()
        .pr(px(16.))
        .when(first, |g| {
            g.child(
                div()
                    .mt(px(if has_quote { 26. } else { 2. }))
                    .child(avatar(ctx.store, &author, 40.)),
            )
        })
        .when(!first, |g| {
            g.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right(px(16.))
                    .text_center()
                    .text_size(px(10.))
                    .line_height(px(21.))
                    .text_color(white_a(0.4))
                    .whitespace_nowrap()
                    .invisible()
                    .group_hover(group.clone(), |s| s.visible())
                    .child(data::clock(m.at)),
            )
        });

    let mut body = div().flex_1().min_w_0().pr(px(12.)).flex().flex_col();
    if let Some(q) = quote {
        body = body.child(q);
    }
    if first {
        body = body.child(
            div()
                .h(px(24.))
                .mb(px(-2.))
                .flex()
                .items_center()
                .gap(px(8.))
                .min_w_0()
                .child(
                    div()
                        .relative()
                        .top(px(1.))
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_weight(FontWeight::BOLD)
                        .text_size(px(15.))
                        .line_height(px(19.5))
                        .text_color(c(0xffffff))
                        .child(ctx.store.chat_name(&author, &ctx.chat.id)),
                )
                .children(crate::community::author_badge(
                    ctx.store,
                    &ctx.chat.id,
                    &author,
                ))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(12.))
                        .text_color(white_a(0.4))
                        .child(data::clock(m.at)),
                ),
        );
    }
    body = body.child(
        div()
            .when(pending || failed, |d| d.opacity(0.6))
            .child(content(ctx, m))
            .children(crate::attachments::attachments(ctx, m))
            .children(crate::packs::cards(ctx, m))
            .when(!ctx.held && !m.content.is_empty(), |d| {
                d.children(
                    crate::newchat::invite_card_keys(&m.content)
                        .into_iter()
                        .map(|k| invite_card(ctx, m, k)),
                )
            })
            .children(crate::embeds::cards(ctx, m))
            .children(crate::linkcard::card(ctx, m)),
    );
    if let Some(reason) = ctx.fail_reasons.get(&m.id).filter(|_| failed) {
        body = body.child(
            div()
                .mt(px(4.))
                .text_size(px(12.))
                .line_height(px(16.2))
                .text_color(c(DANGER))
                .child(reason.clone()),
        );
    }
    if let Some(r) = reactions(ctx, m) {
        body = body.child(r);
    }

    div()
        .pt(px(gap))
        .child(
            div()
                .id(SharedString::from(m.id.clone()))
                .group(group)
                .relative()
                .on_hover({
                    let (act, id) = (ctx.on_act.clone(), m.id.clone());
                    move |h, w, cx| act(&id, Act::Hover(*h), w, cx)
                })
                .flex()
                .items_start()
                .pl(px(12.))
                .pr(px(12.))
                .pb(px(2.))
                .border_l(px(3.))
                .border_color(gpui::transparent_black())
                .font_family(RUBIK)
                .hover(|s| s.bg(white_a(0.03)))
                .when(replying, |d| d.bg(white_a(0.05)).border_color(white_a(0.5)))
                .when_some(ctx.jumped.filter(|(id, _)| *id == m.id), |d, (_, t)| {
                    d.bg(ca(90, 252, 180, jump_flash(t)))
                })
                .when(failed, |d| {
                    d.bg(ca(255, 46, 169, 0.1)).border_color(c(DANGER))
                })
                .child(gutter)
                .child(body)
                .when(hovered || ctx.picker == Some(m.id.as_str()), |d| {
                    d.children(toolbar(ctx, m, first))
                }),
        )
        .into_any_element()
}

fn author_of(ctx: &Ctx, m: &Message) -> String {
    if m.mine {
        ctx.store.my_npub.clone()
    } else {
        m.npub.clone().unwrap_or_else(|| ctx.chat.id.clone())
    }
}

/// A message's NIP-30 tags as the renderer asks for them: a web URL through the emoji cache,
/// a fixture's path from beside its file.
pub fn custom_emoji<'a>(
    store: &'a Store,
    media: &'a std::cell::RefCell<crate::media::Media>,
    tags: &'a [vector_core::types::EmojiTag],
) -> Option<impl Fn(&str) -> Option<crate::media::Custom> + 'a> {
    (!tags.is_empty()).then(move || {
        move |code: &str| {
            let tag = tags.iter().find(|t| t.shortcode == code)?;
            Some(
                media
                    .borrow_mut()
                    .custom(&emoji_source(store, &tag.url), false),
            )
        }
    })
}

pub fn emoji_source(store: &Store, url: &str) -> String {
    // Fixtures name their pictures under a reserved host, beside the fixture file.
    match url.strip_prefix("https://ref.invalid/") {
        Some(path) => store.media_root.join(path).to_string_lossy().into_owned(),
        None => url.to_string(),
    }
}

/// `.dmsg-content`: the text, then its trailing marks.
fn content(ctx: &Ctx, m: &Message) -> AnyElement {
    let empty = HashSet::new();
    let revealed = ctx.revealed.get(&m.id).unwrap_or(&empty);
    let id = m.id.clone();
    let act = ctx.on_act.clone();
    let click: OnClick = Rc::new(move |t, w, cx| act(&id, Act::Text(t), w, cx));
    let content = crate::texttime::spans_to_codes(&m.content, &m.text_spans);
    let mut text = crate::newchat::replace_invite_links(&content, "");
    if let std::borrow::Cow::Owned(s) = crate::packs::strip(&text) {
        text = s.into();
    }
    if !ctx.held && !ctx.media.borrow().web_previews_off {
        if let std::borrow::Cow::Owned(s) = crate::embeds::strip(&text) {
            text = s.into();
        }
    }

    // Emoji-only, up to six: jumbo glyphs (`.dmsg-emoji-only`).
    let headed = text.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("-# ")
            || (t.starts_with('#') && t.trim_start_matches('#').starts_with([' ', '\t']))
    });
    if !headed && let Some(n @ 1..=6) = emoji::only_emoji_count(&text) {
        let _ = n;
        let mut row = div().flex().items_center().py(px(4.));
        for g in unicode_segmentation::UnicodeSegmentation::graphemes(text.as_ref(), true) {
            if let Some(src) = emoji::twemoji(g) {
                row = row.child(img(src).size(px(50.)).flex_shrink_0());
            }
        }
        return div()
            .flex()
            .items_end()
            .child(row)
            .children(marks(m))
            .into_any_element();
    }

    let custom = custom_emoji(ctx.store, ctx.media, &m.emoji_tags);
    let mut blocks = markdown::parse(
        text.trim(),
        &markdown::Ctx {
            base: base_look(),
            revealed,
            now: ctx.store.now_ms() as f64 / 1000.,
            custom: custom
                .as_ref()
                .map(|f| f as &dyn Fn(&str) -> Option<crate::media::Custom>),
        },
    );
    let trail = trailing_atoms(m);
    if !trail.is_empty() {
        match blocks.last_mut() {
            Some(Block::Para(p)) if p.kind == ParaKind::Text || p.kind == ParaKind::Sub => {
                let mut atoms: Vec<Atom> = p.atoms.iter().cloned().collect();
                atoms.extend(trail);
                p.atoms = atoms.into();
            }
            _ => blocks.push(Block::Para(Para {
                atoms: trail.into(),
                base: base_look(),
                kind: ParaKind::Text,
            })),
        }
    }
    let pieces = linked_pictures(ctx, blocks);
    let mut col = div().flex().flex_col();
    let mut prev_bottom: Option<f32> = None;
    let mut prev_para = false;
    let count = pieces.len();
    for (i, piece) in pieces.iter().enumerate() {
        let b = match piece {
            Piece::Block(b, _) => b,
            Piece::Picture { image, w, h } => {
                // `.inline-image`: its own size, no taller than 400px nor wider than the bubble.
                let open = crate::viewer::Open {
                    image: image.clone(),
                    w: *w,
                    h: *h,
                    full: None,
                };
                let s = (400. / *h as f32).min(1.);
                let (w, h) = (*w as f32 * s, *h as f32 * s);
                let (on_act, mid) = (ctx.on_act.clone(), m.id.clone());
                col = col.child(
                    div()
                        .id(SharedString::from(format!("linked-{}-{i}", m.id)))
                        .w(px(w))
                        .max_w_full()
                        .cursor_pointer()
                        .on_click(move |_, win, cx| {
                            cx.stop_propagation();
                            on_act(&mid, Act::View(open.clone()), win, cx)
                        })
                        .child(
                            img(image.clone())
                                .w_full()
                                .aspect_ratio(w / h.max(1.))
                                .rounded(px(8.))
                                .hover(|s| s.opacity(0.9)),
                        ),
                );
                prev_bottom = Some(0.);
                prev_para = false;
                continue;
            }
        };
        let cont = matches!(piece, Piece::Block(_, true));
        let (top, bottom) = margins(b, i == 0, i + 1 == count);
        let is_para = matches!(b, Block::Para(p) if p.kind == ParaKind::Text);
        let gap = match prev_bottom {
            Some(_) if cont => 0.,
            // Consecutive paragraphs sit a blank line apart (`<br><br>`).
            Some(_) if is_para && prev_para => TEXT_LH,
            Some(pb) => pb.max(top),
            None => top,
        };
        col = col.child(div().mt(px(gap)).child(block(
            b,
            &format!("{}-{i}", m.id),
            &click,
            ctx.clock,
        )));
        prev_bottom = Some(bottom);
        prev_para = is_para;
    }
    if let Some(pb) = prev_bottom {
        col = col.child(div().h(px(pb)));
    }
    col.into_any_element()
}

enum Piece {
    /// A block, and whether it continues the paragraph a picture split.
    Block(Block, bool),
    Picture {
        image: Arc<gpui::RenderImage>,
        w: u32,
        h: u32,
    },
}

/// `isImageUrl`: the path ends in a picture's extension, or the text names one anywhere.
pub fn is_image_url(url: &str) -> bool {
    const EXTS: [&str; 8] = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg"];
    let lower = url.to_ascii_lowercase();
    let path = lower.split(['?', '#']).next().unwrap_or("");
    EXTS.iter()
        .any(|e| path.ends_with(&format!(".{e}")) || lower.contains(&format!(".{e}")))
}

/// Links to pictures, as `processInlineImages` swaps them: a ring beside the link while it
/// fetches, then the picture in its place, splitting the paragraph around it.
fn linked_pictures(ctx: &Ctx, blocks: Vec<Block>) -> Vec<Piece> {
    let mut out = Vec::with_capacity(blocks.len());
    for b in blocks {
        let Block::Para(p) = b else {
            out.push(Piece::Block(b, false));
            continue;
        };
        if !p
            .atoms
            .iter()
            .any(|a| matches!(a, Atom::Text { kind: Kind::Link(u), .. } if is_image_url(u)))
        {
            out.push(Piece::Block(Block::Para(p), false));
            continue;
        }
        let (base, kind) = (p.base.clone(), p.kind.clone());
        let para = |atoms: Vec<Atom>| {
            Block::Para(Para {
                atoms: atoms.into(),
                base: base.clone(),
                kind: kind.clone(),
            })
        };
        let mut run: Vec<Atom> = Vec::new();
        let mut cont = false;
        let atoms: Vec<Atom> = p.atoms.iter().cloned().collect();
        let mut i = 0;
        while i < atoms.len() {
            let url = match &atoms[i] {
                Atom::Text {
                    kind: Kind::Link(u),
                    ..
                } if is_image_url(u) => u.clone(),
                a => {
                    run.push(a.clone());
                    i += 1;
                    continue;
                }
            };
            // The link's text, however many runs it spans.
            let mut end = i;
            while end < atoms.len()
                && matches!(&atoms[end], Atom::Text { kind: Kind::Link(u), .. } if *u == url)
            {
                end += 1;
            }
            match ctx.media.borrow_mut().linked(&url) {
                Linked::Ready { image, w, h } => {
                    if !run.is_empty() {
                        out.push(Piece::Block(para(std::mem::take(&mut run)), cont));
                    }
                    out.push(Piece::Picture { image, w, h });
                    cont = true;
                    // A space the link was set off by goes with it.
                    if let Some(Atom::Text { text, look, kind }) = atoms.get(end)
                        && text.starts_with(' ')
                    {
                        let rest = text.trim_start_matches(' ');
                        if !rest.is_empty() {
                            run.push(Atom::Text {
                                text: rest.to_string().into(),
                                look: look.clone(),
                                kind: kind.clone(),
                            });
                        }
                        end += 1;
                    }
                }
                Linked::Loading(pct) => {
                    run.extend(atoms[i..end].iter().cloned());
                    run.push(Atom::Ring(pct));
                }
                Linked::Failed => run.extend(atoms[i..end].iter().cloned()),
            }
            i = end;
        }
        // Text left after a picture, less the break that led to it.
        while matches!(run.first(), Some(Atom::Break)) && cont {
            run.remove(0);
        }
        if !run.is_empty() {
            out.push(Piece::Block(para(run), cont));
        }
    }
    out
}

/// The space a block keeps above and below it, by its CSS margins.
fn margins(b: &Block, first: bool, last: bool) -> (f32, f32) {
    let (t, bt) = match b {
        Block::Para(p) => match p.kind {
            ParaKind::Heading(_) => (p.base.size * 0.4, p.base.size * 0.15),
            _ => (0., 0.),
        },
        Block::Code { .. } | Block::Quote(_) | Block::List { .. } => (8., 8.),
        Block::Rule => (16., 16.),
        Block::Table { .. } => (10., 10.),
    };
    // A heading first or last in the text drops that margin.
    let heading = matches!(b, Block::Para(p) if matches!(p.kind, ParaKind::Heading(_)));
    (
        if heading && first { 0. } else { t },
        if heading && last { 0. } else { bt },
    )
}

fn block(b: &Block, key: &str, click: &OnClick, clock: f32) -> AnyElement {
    match b {
        Block::Para(p) => inline(p.atoms.clone(), p.base.clone())
            .id(SharedString::from(key.to_string()))
            .on_click(click.clone())
            .clock(clock)
            .into_any_element(),
        Block::Rule => div()
            .min_w(px(192.))
            .w_full()
            .h(px(1.))
            .bg(white_a(0.2))
            .into_any_element(),
        Block::Quote(inner) => div()
            .border_l(px(3.))
            .border_color(c(ACCENT))
            .pl(px(12.))
            .child(stack(inner, key, click, clock, TEXT_LH))
            .into_any_element(),
        Block::List { start, items } => {
            let mut col = div().flex().flex_col().pl(px(24.));
            for (i, item) in items.iter().enumerate() {
                let marker: AnyElement = match start {
                    Some(n) => div()
                        .absolute()
                        .right_full()
                        .top_0()
                        .pr(px(4.))
                        .text_size(px(TEXT_SIZE))
                        .line_height(px(TEXT_LH))
                        .text_color(white_a(0.85))
                        .whitespace_nowrap()
                        .child(format!("{}.", n + i as u64))
                        .into_any_element(),
                    None => div()
                        .absolute()
                        .left(px(-15.))
                        .top(px(8.5))
                        .size(px(5.))
                        .rounded_full()
                        .bg(white_a(0.85))
                        .into_any_element(),
                };
                col = col.child(
                    div()
                        .relative()
                        .when(i > 0, |d| d.mt(px(4.)))
                        .child(marker)
                        .child(stack(item, &format!("{key}-{i}"), click, clock, TEXT_LH)),
                );
            }
            col.into_any_element()
        }
        Block::Code { lang, text } => code_block(lang, text, key, click),
        Block::Table { align, head, rows } => table(align, head, rows, key, click, clock),
    }
}

/// Blocks inside a quote or list item, spaced as at the top level.
pub fn stack(blocks: &[Block], key: &str, click: &OnClick, clock: f32, lh: f32) -> AnyElement {
    let mut col = div().flex().flex_col();
    let mut prev: Option<(f32, bool)> = None;
    for (i, b) in blocks.iter().enumerate() {
        let (top, bottom) = margins(b, i == 0, i + 1 == blocks.len());
        let is_para = matches!(b, Block::Para(p) if p.kind == ParaKind::Text);
        let gap = match prev {
            Some((_, true)) if is_para => lh,
            Some((pb, _)) => pb.max(top),
            None => 0.,
        };
        col = col.child(
            div()
                .mt(px(gap))
                .child(block(b, &format!("{key}-{i}"), click, clock)),
        );
        prev = Some((bottom, is_para));
    }
    col.into_any_element()
}

/// `.code-block-wrapper` + `pre code.hljs` (GitHub Dark).
fn code_block(lang: &str, text: &SharedString, key: &str, click: &OnClick) -> AnyElement {
    let mut lines = div().flex().flex_col();
    for (line, runs) in text
        .split('\n')
        .zip(crate::highlight::highlight(text, lang))
    {
        let font = gpui::font(MONO);
        let runs: Vec<gpui::TextRun> = runs
            .into_iter()
            .map(|(len, color)| gpui::TextRun {
                len,
                font: font.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            })
            .collect();
        let el: AnyElement = if line.is_empty() {
            div().child(" ").into_any_element()
        } else {
            gpui::StyledText::new(line.to_string())
                .with_runs(runs)
                .into_any_element()
        };
        lines = lines.child(div().h(px(19.5)).whitespace_nowrap().child(el));
    }
    let copy = click.clone();
    let code: SharedString = text.clone();
    div()
        .relative()
        .max_w(px(470.))
        .w_full()
        .rounded(px(6.))
        .border_1()
        .border_color(white_a(0.1))
        .bg(c(0x0d1117))
        .overflow_hidden()
        .child(
            div()
                .p(px(13.))
                .font_family(MONO)
                .text_size(px(13.))
                .line_height(px(19.5))
                .text_color(c(crate::highlight::PLAIN))
                .child(lines),
        )
        .child(
            div()
                .id(SharedString::from(format!("{key}-copy")))
                .absolute()
                .top(px(8.))
                .right(px(8.))
                .size(px(32.))
                .rounded(px(6.))
                .border_1()
                .border_color(ca(89, 252, 179, 0.3))
                .bg(ca(0, 0, 0, 0.6))
                .opacity(0.7)
                .hover(|s| {
                    s.opacity(1.)
                        .bg(ca(0, 0, 0, 0.8))
                        .border_color(ca(89, 252, 179, 0.5))
                })
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_center()
                .on_click(move |_, w, cx| copy(Target::Code(code.clone()), w, cx))
                .child(
                    svg()
                        .path("icons/copy.svg")
                        .size(px(16.))
                        .text_color(c(ACCENT)),
                ),
        )
        .into_any_element()
}

fn table(
    align: &[Alignment],
    head: &[Para],
    rows: &[Vec<Para>],
    key: &str,
    click: &OnClick,
    clock: f32,
) -> AnyElement {
    let cell = |p: &Para, i: usize, header: bool, k: String| {
        let mut base = p.base.clone();
        if header {
            base.size *= 0.82;
            base.weight = FontWeight::SEMIBOLD;
            base.color = c(ACCENT);
        } else {
            base.size *= 0.95;
        }
        let d = div().flex_1().px(px(14.)).py(px(10.));
        let d = match align.get(i) {
            Some(Alignment::Right) => d.flex().justify_end(),
            Some(Alignment::Center) => d.flex().justify_center(),
            _ => d,
        };
        d.child(
            inline(p.atoms.clone(), base)
                .id(SharedString::from(k))
                .on_click(click.clone())
                .clock(clock),
        )
    };
    let mut t = div()
        .flex()
        .flex_col()
        .rounded(px(8.))
        .border_1()
        .border_color(white_a(0.08))
        .bg(white_a(0.015))
        .overflow_hidden();
    let mut hrow = div()
        .flex()
        .bg(ca(89, 252, 179, 0.04))
        .border_b_1()
        .border_color(ca(89, 252, 179, 0.18));
    for (i, p) in head.iter().enumerate() {
        hrow = hrow.child(cell(p, i, true, format!("{key}-h{i}")));
    }
    t = t.child(hrow);
    for (r, row) in rows.iter().enumerate() {
        let mut d = div().flex().when(r + 1 < rows.len(), |d| {
            d.border_b_1().border_color(white_a(0.05))
        });
        for (i, p) in row.iter().enumerate() {
            d = d.child(cell(p, i, false, format!("{key}-{r}-{i}")));
        }
        t = t.child(d);
    }
    t.into_any_element()
}

/// `(edited)` (and later marks) at the end of the text's last line.
fn trailing_atoms(m: &Message) -> Vec<Atom> {
    let mut out = Vec::new();
    if m.edited {
        out.push(Atom::Text {
            text: " ".into(),
            look: base_look(),
            kind: Kind::Plain,
        });
        out.push(Atom::Gap(6.));
        out.push(Atom::Text {
            text: "(edited)".into(),
            look: Look::new(RUBIK, 10., 14., white_a(0.4)),
            kind: Kind::Plain,
        });
    }
    out
}

/// Marks beside a jumbo-emoji message.
fn marks(m: &Message) -> Option<AnyElement> {
    m.edited.then(|| {
        div()
            .ml(px(6.))
            .pb(px(4.))
            .text_size(px(10.))
            .text_color(white_a(0.4))
            .child("(edited)")
            .into_any_element()
    })
}

/// `.dmsg-reactions`: one chip per emoji in first-seen order, then the add button.
fn reactions(ctx: &Ctx, m: &Message) -> Option<AnyElement> {
    if m.reactions.is_empty() {
        return None;
    }
    let mut groups: Vec<(&str, usize, bool, Option<&str>)> = Vec::new();
    for r in &m.reactions {
        let mine = r.author_id == ctx.store.my_npub;
        match groups.iter_mut().find(|g| g.0 == r.emoji) {
            Some(g) => {
                g.1 += 1;
                g.2 |= mine;
                g.3 = g.3.or(r.emoji_url.as_deref());
            }
            None => groups.push((&r.emoji, 1, mine, r.emoji_url.as_deref())),
        }
    }
    let mut row = div().flex().flex_wrap().gap(px(4.)).mt(px(4.)).mb(px(4.));
    for (emoji_str, count, mine, url) in groups {
        let custom = url.map(|u| {
            ctx.media
                .borrow_mut()
                .custom(&emoji_source(ctx.store, u), false)
        });
        let glyph: AnyElement = match (custom, emoji::twemoji(emoji_str)) {
            (Some(crate::media::Custom::Ready(image)), _) => img(image)
                .size(px(16.))
                .aspect_ratio(1.)
                .object_fit(gpui::ObjectFit::Contain)
                .into_any_element(),
            (Some(crate::media::Custom::Pending), _) => div().size(px(16.)).into_any_element(),
            (_, Some(src)) => img(src).size(px(16.)).into_any_element(),
            _ => div()
                .text_size(px(12.))
                .child(emoji_str.to_string())
                .into_any_element(),
        };
        let (act, id, e) = (ctx.on_act.clone(), m.id.clone(), emoji_str.to_string());
        row = row.child(
            div()
                .id(SharedString::from(format!("{}-r-{emoji_str}", m.id)))
                .when(!mine, |d| {
                    d.cursor_pointer()
                        .hover(|s| s.bg(white_a(0.1)).opacity(1.))
                        .on_click(move |_, w, cx| act(&id, Act::ReactWith(e.clone()), w, cx))
                })
                .flex()
                .items_center()
                .gap(px(4.))
                .px(px(6.))
                .py(px(3.))
                .rounded(px(6.))
                .border_1()
                .opacity(0.9)
                .when(mine, |d| {
                    d.bg(ca(89, 252, 179, 0.18)).border_color(c(ACCENT))
                })
                .when(!mine, |d| d.bg(white_a(0.05)).border_color(white_a(0.08)))
                .text_size(px(12.))
                .line_height(px(12.))
                .text_color(white_a(0.85))
                .child(glyph)
                .child(count.to_string()),
        );
    }
    let (act, id) = (ctx.on_act.clone(), m.id.clone());
    row = row.child(
        div()
            .id(SharedString::from(format!("{}-r-add", m.id)))
            .cursor_pointer()
            .hover(|s| s.bg(white_a(0.1)).border_color(white_a(0.15)))
            .on_click(move |_, w, cx| act(&id, Act::React, w, cx))
            .flex()
            .items_center()
            .px(px(6.))
            .py(px(4.))
            .rounded(px(6.))
            .border_1()
            .border_color(white_a(0.08))
            .bg(white_a(0.04))
            .child(
                svg()
                    .path("icons/smile-face.svg")
                    .size(px(14.))
                    .text_color(white_a(0.55)),
            ),
    );
    Some(row.into_any_element())
}

/// `.dmsg-reply`: the quoted parent above a reply, one line, with the elbow from the avatar.
fn reply_quote(ctx: &Ctx, m: &Message) -> Option<AnyElement> {
    let parent = ctx.chat.messages.iter().find(|p| p.id == m.replied_to);
    let content = m
        .replied_to_content
        .clone()
        .or_else(|| parent.map(|p| p.content.clone()));
    let has_attachment = m.replied_to_has_attachment.unwrap_or(false)
        || parent.is_some_and(|p| !p.attachments.is_empty());
    if content.is_none() && !has_attachment {
        return None;
    }
    let mine = parent
        .map(|p| p.mine)
        .unwrap_or_else(|| m.replied_to_npub.as_deref() == Some(ctx.store.my_npub.as_str()));
    let npub = if mine {
        ctx.store.my_npub.clone()
    } else {
        m.replied_to_npub
            .clone()
            .or_else(|| parent.and_then(|p| p.npub.clone()))
            .unwrap_or_else(|| ctx.chat.id.clone())
    };
    let snippet: AnyElement = match content.filter(|c| !c.is_empty()) {
        Some(text) => {
            let mut look = Look::new(RUBIK, 13., 16.9, white_a(0.45));
            look.weight = FontWeight::NORMAL;
            let atoms = crate::preview::reply_atoms(&text, &look);
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .h(px(16.9))
                .child(inline(atoms, look))
                .into_any_element()
        }
        None => div()
            .text_color(white_a(0.45))
            .child("Attachment")
            .into_any_element(),
    };
    let elbow = div()
        .absolute()
        .left(px(20. - 56. - 1.))
        .top(px(20.9 / 2. - 2.))
        .w(px(56. - 20. - 2.))
        .h(px(12.))
        .border_l(px(2.))
        .border_t(px(2.))
        .border_color(white_a(0.25))
        .rounded_tl(px(8.));
    let (on, id, parent_id) = (ctx.on_act.clone(), m.id.clone(), m.replied_to.clone());
    let (on_name, name_id, name_npub) = (ctx.on_act.clone(), m.id.clone(), npub.clone());
    Some(
        div()
            .id(SharedString::from(format!("r-{}", m.id)))
            .cursor_pointer()
            .on_click(move |_, w, cx| on(&id, Act::Jump(parent_id.clone()), w, cx))
            .relative()
            .flex()
            .items_center()
            .gap(px(6.))
            .pt(px(2.))
            .pb(px(2.))
            .pl(px(3.))
            .mb(px(4.))
            .h(px(20.9))
            .text_size(px(13.))
            .line_height(px(16.9))
            .child(elbow)
            .child(avatar(ctx.store, &npub, 16.))
            .child(
                div()
                    .flex_shrink_0()
                    .max_w(gpui::relative(0.5))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(white_a(0.7))
                    .id("reply-name")
                    .hover(|s| s.underline())
                    .on_click(move |_, w, cx| {
                        cx.stop_propagation();
                        on_name(
                            &name_id,
                            Act::Text(crate::inline::Target::Mention(name_npub.clone().into())),
                            w,
                            cx,
                        )
                    })
                    .child(ctx.store.chat_name(&npub, &ctx.chat.id)),
            )
            .child(snippet)
            .into_any_element(),
    )
}

/// `#dmsg-toolbar`: the hover actions at the row's top right (`_dmsgToolbarView`).
fn toolbar(ctx: &Ctx, m: &Message, first: bool) -> Option<AnyElement> {
    let mut acts: Vec<(&'static str, &'static str, Act, bool)> = Vec::new();
    if m.mine && m.pending && !m.failed {
        return None;
    }
    if m.mine && m.failed {
        acts.push(("icons/refresh.svg", "Retry send", Act::Retry, false));
        acts.push((
            "icons/trash.svg",
            "Delete failed message",
            Act::Delete,
            true,
        ));
    } else {
        let unique: HashSet<&str> = m.reactions.iter().map(|r| r.emoji.as_str()).collect();
        if unique.len() < 8 {
            acts.push(("icons/smile-face.svg", "Add reaction", Act::React, false));
        }
        acts.push(("icons/reply.svg", "Reply", Act::Reply, false));
        if m.mine && !m.content.is_empty() && m.attachments.is_empty() {
            acts.push(("icons/edit.svg", "Edit", Act::Edit, false));
        }
        if ctx.chat.id.starts_with("npub1") || m.mine {
            acts.push(("icons/trash.svg", "Delete message", Act::Delete, true));
        }
    }
    // A continuation's text starts at its top edge: lift the bar clear of it.
    let lift = if first { 16. } else { 32. };
    let mut bar = div()
        .id(SharedString::from(format!("{}-toolbar", m.id)))
        .absolute()
        .top(px(-lift))
        .right(px(8.))
        .flex()
        .gap(px(2.))
        .p(px(4.))
        .rounded(px(8.))
        .border_1()
        .border_color(white_a(0.08))
        .bg(c(0x1a1a1a))
        .shadow(vec![gpui::BoxShadow {
            color: ca(0, 0, 0, 0.5),
            offset: gpui::point(px(0.), px(4.)),
            blur_radius: px(14.),
            spread_radius: px(0.),
            inset: false,
        }])
        .occlude()
        .on_hover({
            let (act, id) = (ctx.on_act.clone(), m.id.clone());
            move |h, w, cx| act(&id, Act::Hover(*h), w, cx)
        });
    for (i, (icon, _label, act, danger)) in acts.into_iter().enumerate() {
        let (on, id) = (ctx.on_act.clone(), m.id.clone());
        bar = bar.child(
            div()
                .id(i)
                .size(px(26.))
                .rounded(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .group("tb-btn")
                .hover(|s| s.bg(white_a(0.08)))
                .on_click(move |_, w, cx| on(&id, act.clone(), w, cx))
                .child(svg().path(icon).size(px(20.)).text_color(if danger {
                    c(DANGER)
                } else {
                    white_a(0.7)
                })),
        );
    }
    let root = div().absolute().top_0().right_0().child(bar);
    Some(root.into_any_element())
}

/// `InviteCard.svelte`: a Community invite link as its Community, with Join (or Open, once in).
fn invite_card(ctx: &Ctx, m: &Message, key: String) -> AnyElement {
    use crate::newchat::Preview;
    let state = ctx.invites.get(&key);
    let joined = match state {
        Some(Preview::Ready(i)) => ctx.store.community(&i.community_id),
        _ => None,
    };
    let (name, desc) = match (state, joined) {
        (_, Some(c)) => (Some(c.name.clone()), c.description.clone()),
        (Some(Preview::Ready(i)), None) => (Some(i.name.clone()), i.description.clone()),
        (Some(Preview::Failed), _) => (
            Some("Invite Unavailable".to_string()),
            "This invite could not be loaded — it may be revoked or expired.".to_string(),
        ),
        _ => (None, String::new()),
    };
    let invalid = matches!(state, Some(Preview::Failed));
    let ready = matches!(state, Some(Preview::Ready(_)));
    let skel = |w: f32, h: f32| div().w(px(w)).h(px(h)).rounded(px(4.)).bg(white_a(0.06));
    let id = m.id.clone();
    let act = ctx.on_act.clone();
    let button = (!invalid).then(|| {
        let k = key.clone();
        div()
            .id(SharedString::from(format!("cic-{}-{key}", m.id)))
            .flex_shrink_0()
            .px(px(18.))
            .h(px(33.))
            .flex()
            .items_center()
            .rounded(px(8.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(13.))
            .map(|d| {
                if joined.is_some() {
                    d.bg(white_a(0.08)).text_color(c(0xffffff))
                } else {
                    d.bg(c(ACCENT)).text_color(c(0x0e0e0e))
                }
            })
            .when(!ready, |d| d.opacity(0.55))
            .when(ready, |d| {
                d.cursor_pointer()
                    .hover(|d| d.opacity(0.9))
                    .on_click(move |_, w, cx| act(&id, Act::Invite(k.clone()), w, cx))
            })
            .child(if joined.is_some() { "Open" } else { "Join" })
    });
    div()
        .mt(px(8.))
        .max_w(px(446.))
        .p(px(12.))
        .rounded(px(10.))
        .border_1()
        .border_color(white_a(0.08))
        .bg(ca(10, 10, 10, 0.85))
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            crate::kit::spaced("COMMUNITY INVITE", 0.9)
                .font_weight(FontWeight::BOLD)
                .text_size(px(10.))
                .line_height(px(14.))
                .text_color(white_a(0.45)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .min_w_0()
                .child(
                    match joined
                        .and_then(|c| ctx.store.icon(&c.community_id))
                        .or_else(|| match state {
                            Some(Preview::Ready(i)) => i.icon.clone().map(std::path::PathBuf::from),
                            _ => None,
                        }) {
                        Some(path) => img(path).object_fit(gpui::ObjectFit::Cover),
                        None => img("icons/group-placeholder.svg"),
                    }
                    .size(px(42.))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(white_a(0.05))
                    .when(invalid, |d| d.opacity(0.5)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .map(|d| match name {
                            Some(name) => d
                                .child(
                                    div()
                                        .truncate()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_size(px(14.))
                                        .line_height(px(19.6))
                                        .text_color(if invalid {
                                            white_a(0.6)
                                        } else {
                                            c(0xffffff)
                                        })
                                        .child(name),
                                )
                                .when(!desc.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .text_size(px(11.))
                                            .line_height(px(14.85))
                                            .text_color(white_a(0.55))
                                            .line_clamp(2)
                                            .child(desc),
                                    )
                                }),
                            None => d.child(skel(140., 14.)).child(skel(200., 10.)),
                        }),
                )
                .children(button),
        )
        .into_any_element()
}
