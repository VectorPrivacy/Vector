//! One message row (`MessageRow.svelte` + `MessageContent.svelte`): the gutter avatar, the
//! header, a reply's quote, the markdown body and the reactions, with the `.dmsg-*` metrics
//! measured from the Tauri render.

use crate::kit::Tipped as _;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AnyElement, App, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px, svg};
use crate::pic::img;
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
    /// A link card's play button: its video or track in the player window.
    Play(crate::linkcard::Play),
    /// A collapsed post, or a run of them, tapped open.
    Unveil(Vec<String>),
    /// Collapse shown messages again.
    Veil(Vec<String>),
    /// Older history failed to load: ask again.
    RetryHistory,
    /// An avatar or name tapped: their mini profile, beside what was painted under the key.
    Mini(String, SharedString),
    /// A reaction chip hovered, or left.
    ReactionHover(String, bool),
    /// A reaction chip right-clicked: who reacted.
    ReactionDetails(String),
    /// A reaction chip pressed (true) or released; a long press opens its details.
    ReactionPress(String, bool),
    /// Right-click on a message: its menu, at the pointer.
    MessageMenu(gpui::Point<gpui::Pixels>),
    /// Right-clicked: in a community, their member menu.
    AuthorMenu(String, SharedString, gpui::Point<gpui::Pixels>),
    Text(Target),
    Hover(bool),
    React,
    /// A reaction chip, or a quick pick: react with this emoji.
    ReactWith(String),
    /// Our own reaction chip pressed: take it back.
    Unreact(String),
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
    /// A downloaded attachment's toolbar buttons: reveal it (or save a held one), or copy it.
    RevealFile(String),
    CopyFile(String),
    /// A Mini App link: fetch it (or retry), or open what it fetched.
    XdcUrl(String),
    OpenXdcUrl(String),
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
    Video(String, crate::video::Act),
    Model(String, crate::model3d::stage::Act),
    /// An invocation line's command name: the composer takes it, to run again.
    Rerun(String),
    /// A PIVX payment's bubble: claim it, by gift code.
    PivxClaim(String),
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
    /// The message heading the hovered row's streak.
    pub hover_head: Option<usize>,
    /// The message the composer is replying to.
    pub replying: Option<&'a str>,
    /// The message whose reaction menu is open.
    pub picker: Option<&'a str>,
    /// Why a send failed, by message.
    pub fail_reasons: &'a HashMap<String, String>,
    /// What the hover toolbar's delete offers, by message, once the backend has said.
    pub delete_meta: &'a HashMap<String, crate::backend::actions::DeleteOptions>,
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
    /// The Community's mention rules, for `@everyone` and Role chips.
    pub rules: Option<&'a crate::mentions::Rules>,
    /// The command names the chat's bots declare, once known.
    pub commands: Option<&'a HashSet<String>>,
    pub anchors: &'a crate::kit::Anchors,
    /// The message column's width, for media to fit; 0 before the list has laid out.
    pub content_w: f32,
    /// The chat's message positions by id; a stale entry falls back to a scan.
    pub by_id: &'a HashMap<String, usize>,
    /// The window's width: what `100vw` reads.
    pub vw: f32,
    /// The message the keyboard is on, while the timeline has focus.
    pub cursor: Option<&'a str>,
}

/// The open chat's message `id`.
pub fn find<'b>(ctx: &Ctx<'b>, id: &str) -> Option<&'b Message> {
    match ctx.by_id.get(id).and_then(|&i| ctx.chat.messages.get(i)) {
        Some(m) if m.id == id => Some(m),
        _ => ctx.chat.messages.iter().rev().find(|m| m.id == id),
    }
}

/// `_dmsgIsPinged`: a tag of me, an authorised `@everyone`, a Role I hold, or a reply to me.
fn pings_me(ctx: &Ctx, m: &Message) -> bool {
    if m.mine || m.npub.as_deref().is_some_and(|n| ctx.store.profiles.get(n).is_some_and(|p| p.is_blocked)) {
        return false;
    }
    let me = ctx.store.my_npub.as_str();
    let text = vector_core::types::outside_code(&m.content);
    if !me.is_empty() && text.contains(&format!("@{me}")) {
        return true;
    }
    if !m.replied_to.is_empty() {
        let to_me = match find(ctx, &m.replied_to) {
            Some(p) => p.mine,
            None => m.replied_to_npub.as_deref() == Some(me),
        };
        if to_me {
            return true;
        }
    }
    let Some(rules) = ctx.rules else { return false };
    let sender = m.npub.as_deref().unwrap_or_default();
    let by_anyone = rules.anyone || rules.pingers.iter().any(|p| p == sender);
    let everyone = by_anyone
        && text.match_indices("@everyone").any(|(i, _)| {
            !text[i + 9..].chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_')
        });
    everyone
        || rules.roles.iter().any(|r| {
            r.held
                && !r.name.is_empty()
                && m.role_mentions.contains(&r.id)
                && text.contains(&format!("@{}", r.name))
                && (r.mentionable || by_anyone)
        })
}

/// The `@everyone` and Role chips `m` may show (`standingRoleMentions`): names and colours.
fn tag_chips(ctx: &Ctx, m: &Message) -> Vec<(String, Option<u32>)> {
    let Some(rules) = ctx.rules else { return Vec::new() };
    let sender = if m.mine { ctx.store.my_npub.as_str() } else { m.npub.as_deref().unwrap_or_default() };
    let by_anyone = rules.anyone || rules.pingers.iter().any(|p| p == sender);
    let text = vector_core::types::outside_code(&m.content);
    let mut out: Vec<(String, Option<u32>)> = rules
        .roles
        .iter()
        .filter(|r| {
            !r.name.is_empty()
                && m.role_mentions.contains(&r.id)
                && text.contains(&format!("@{}", r.name))
                && (r.mentionable || by_anyone)
        })
        .map(|r| (r.name.clone(), Some(r.color.map_or(PRIMARY, |c| c & 0xff_ffff))))
        .collect();
    if by_anyone {
        out.push(("everyone".into(), None));
    }
    out
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
        Some(path) => crate::pic::img_face(path, size).object_fit(gpui::ObjectFit::Cover)
            .face()
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
fn author_menu(ctx: &Ctx, m: &Message, npub: &str, key: &SharedString) -> impl Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static {
    let (act, id, npub, key) = (ctx.on_act.clone(), m.id.clone(), npub.to_string(), key.clone());
    move |e, w, cx| {
        cx.stop_propagation();
        act(&id, Act::AuthorMenu(npub.clone(), key.clone(), e.position), w, cx)
    }
}

pub fn message_row(ctx: &Ctx, index: usize, first: bool, run: u32, shown: u32, gap: f32) -> AnyElement {
    let m = &ctx.chat.messages[index];
    let author = author_of(ctx, m);
    let hidden = ctx.store.hidden(ctx.chat, m);
    let revealed = ctx.store.revealed_as(ctx.chat, m);
    // A shown blocked post dims what it says; its author reads as it did collapsed.
    let dim = |d: gpui::Div| d.when(matches!(revealed, Some(data::Hidden::Blocked | data::Hidden::Unauthorized)), |d| d.opacity(0.4));
    let quote = (!m.replied_to.is_empty() && hidden.is_none())
        .then(|| reply_quote(ctx, m))
        .flatten();
    let has_quote = quote.is_some();
    let failed = m.mine && m.failed;
    let pending = m.mine && m.pending && !failed;
    let group: SharedString = format!("dmsg-{}", m.id).into();
    let replying = ctx.replying == Some(m.id.as_str());
    let hovered = ctx.hovered == Some(m.id.as_str());
    let pinged = pings_me(ctx, m);

    let gutter = div()
        .relative()
        .w(px(56.))
        .flex_shrink_0()
        .pr(px(16.))
        .when(first, |g| {
            let key = SharedString::from(format!("av:{}", m.id));
            let (act, id, who, k) = (ctx.on_act.clone(), m.id.clone(), author.clone(), key.clone());
            g.child(
                div()
                    .id(key.clone())
                    .aria_label(ctx.store.chat_name(&author, &ctx.chat.id))
                    .relative()
                    .mt(px(if has_quote { 26. } else { 2. }))
                    .cursor_pointer()
                    .on_click(move |_, w, cx| act(&id, Act::Mini(who.clone(), k.clone()), w, cx))
                    .on_mouse_down(gpui::MouseButton::Right, author_menu(ctx, m, &author, &key))
                    .when(ctx.hover_head == Some(index), |d| d.motion_live())
                    .child(avatar(ctx.store, &author, 40.))
                    .child(crate::kit::anchor(ctx.anchors, key)),
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
    if let Some(pay) = ctx.store.pivx.get(&m.id) {
        body = body.child(pivx_bubble(ctx, m, pay));
        return row_shell(ctx, m, gap, gutter, body, pinged, replying, failed, hovered, first);
    }
    if let Some(q) = quote {
        body = body.child(dim(div()).child(q));
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
                .child({
                    let key = SharedString::from(format!("au:{}", m.id));
                    let (act, id, who, k) = (ctx.on_act.clone(), m.id.clone(), author.clone(), key.clone());
                    div()
                        .id(key.clone())
                        .relative()
                        .top(px(1.))
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_weight(FontWeight::BOLD)
                        .text_size(px(15.))
                        .line_height(px(19.5))
                        .text_color(c(crate::community::author_colour(ctx.store, &ctx.chat.id, &author).unwrap_or(0xffffff)))
                        .cursor_pointer()
                        .hover(|d| d.underline())
                        .on_click(move |_, w, cx| act(&id, Act::Mini(who.clone(), k.clone()), w, cx))
                        .on_mouse_down(gpui::MouseButton::Right, author_menu(ctx, m, &author, &key))
                        .child(ctx.store.chat_name(&author, &ctx.chat.id))
                        .child(crate::kit::anchor(ctx.anchors, key))
                })
                .children(crate::community::author_badge(
                    ctx.store,
                    &ctx.chat.id,
                    &author,
                ))
                .children(timeout_mark(
                    ctx.store,
                    ctx.store.community_of(&ctx.chat.id).map(|c| c.community_id.as_str()),
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
    if let Some(why) = hidden {
        let ids: Vec<String> = if run > 1 {
            ctx.chat.messages[index..]
                .iter()
                .filter(|o| o.npub == m.npub && ctx.store.hidden(ctx.chat, o) == Some(data::Hidden::Blocked))
                .take(run as usize)
                .map(|o| o.id.clone())
                .collect()
        } else {
            vec![m.id.clone()]
        };
        let label: SharedString = if run > 1 { crate::t::hidden_blocked_run(run) } else { why.label() };
        body = body.child(hidden_toggle(ctx, m, why, label, Act::Unveil(ids)));
        return row_shell(ctx, m, gap, gutter, body, false, replying, failed, hovered, first);
    }
    if let Some(why) = revealed.filter(|_| shown > 0) {
        let ids = ctx.chat.messages[index..].iter().take(shown as usize).map(|o| o.id.clone()).collect();
        let what = if shown > 1 && why == data::Hidden::Blocked { crate::t::hidden_blocked_run(shown) } else { why.label() };
        body = body.child(hidden_toggle(ctx, m, why, crate::t::hidden_hide(what.as_ref()), Act::Veil(ids)));
    }
    body = body.child(
        dim(div())
            .when(pending || failed, |d| d.opacity(0.6))
            .child(content(ctx, m))
            .children(crate::attachments::attachments(ctx, m))
            .children(crate::payaddr::detect(&m.content).map(|coin| {
                let (act, id) = (ctx.on_act.clone(), m.id.clone());
                crate::payaddr::card(&m.id, &coin, move |uri, w, cx| act(&id, Act::Text(Target::Link(uri.into())), w, cx))
            }))
            .children(crate::packs::cards(ctx, m))
            .when_some(m.expiration.filter(|_| !m.attachments.is_empty()), |d, at| {
                d.child(
                    div().h(px(21.)).relative().child(
                        div()
                            .id(SharedString::from(format!("{}-sd", m.id)))
                            .absolute()
                            .top(px(11.1))
                            .left(px(6.))
                            .size(px(12.))
                            .opacity(0.4)
                            .hover(|s| s.opacity(1.))
                            .tooltip(move |w, cx| {
                                let left = at as i64 - chrono::Utc::now().timestamp();
                                let text = if left > 0 {
                                    crate::t::msg_dissolves_in(countdown(left as u64))
                                } else {
                                    crate::t::msg_dissolving()
                                };
                                crate::kit::tooltip(text)(w, cx)
                            })
                            .child(svg().path("ui/self-destruct.svg").size_full().text_color(c(ACCENT))),
                    ),
                )
            })
            .when(!ctx.held && !m.content.is_empty(), |d| {
                d.children(
                    crate::newchat::invite_card_keys(&m.content)
                        .into_iter()
                        .map(|k| invite_card(ctx, m, k)),
                )
            })
            .children(crate::embeds::cards(ctx, m))
            .children(crate::attachments::xdc_url_card(ctx, m))
            .when(!m.attachments.is_empty() || crate::backend::xdcurl::find(&m.content).is_none(), |d| {
                d.children(crate::linkcard::card(ctx, m))
            }),
    );
    if let Some(reason) = ctx.fail_reasons.get(&m.id).filter(|_| failed) {
        body = body.child(
            dim(div())
                .mt(px(4.))
                .text_size(px(12.))
                .line_height(px(16.2))
                .text_color(c(DANGER))
                .child(reason.clone()),
        );
    }
    if let Some(r) = reactions(ctx, m) {
        body = body.child(dim(div()).child(r));
    }
    row_shell(ctx, m, gap, gutter, body, pinged, replying, failed, hovered, first)
}

/// `.dmsg-hidden-toggle`: what a collapsed row says in its place, or the line that collapses a
/// shown run again.
fn hidden_toggle(ctx: &Ctx, m: &Message, why: data::Hidden, label: SharedString, on: Act) -> AnyElement {
    let (act, id) = (ctx.on_act.clone(), m.id.clone());
    let icon = if matches!(why, data::Hidden::NoSend | data::Hidden::TimedOut) { "icons/clock.svg" } else { "icons/cancel.svg" };
    div()
        .flex()
        .child(
            div()
                .id(SharedString::from(format!("unveil:{}", m.id)))
                .flex()
                .items_center()
                .gap(px(5.))
                .cursor_pointer()
                .italic()
                .font_family(RUBIK)
                .text_size(px(15.))
                .line_height(px(21.))
                .text_color(white_a(0.3))
                .on_click(move |_, w, cx| act(&id, on.clone(), w, cx))
                .child(svg().path(icon).size(px(14.)).flex_shrink_0().text_color(white_a(0.3)))
                .child(label),
        )
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn row_shell(
    ctx: &Ctx,
    m: &Message,
    gap: f32,
    gutter: gpui::Div,
    body: gpui::Div,
    pinged: bool,
    replying: bool,
    failed: bool,
    hovered: bool,
    first: bool,
) -> AnyElement {
    let group: SharedString = format!("dmsg-{}", m.id).into();
    div()
        .pt(px(gap))
        .child(
            div()
                .id(SharedString::from(m.id.clone()))
                .role(gpui::Role::ListItem)
                .when(ctx.cursor == Some(m.id.as_str()), |d| {
                    d.aria_active_descendant().child(
                        div().absolute().inset_0().border_2().border_color(c(ACCENT)).rounded(px(6.)),
                    )
                })
                .group(group)
                .motion_host()
                .relative()
                .on_hover({
                    let (act, id) = (ctx.on_act.clone(), m.id.clone());
                    move |h, w, cx| act(&id, Act::Hover(*h), w, cx)
                })
                .on_mouse_down(gpui::MouseButton::Right, {
                    let (act, id) = (ctx.on_act.clone(), m.id.clone());
                    move |e, w, cx| act(&id, Act::MessageMenu(e.position), w, cx)
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
                .when(pinged, |d| d.bg(c(PRIMARY).opacity(0.13)).border_color(c(PRIMARY)))
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

enum Jumbo {
    Stock(SharedString),
    /// A tagged `:shortcode:`, drawn once it has loaded.
    Custom(Option<std::sync::Arc<gpui::RenderImage>>),
}

/// `.dmsg-emoji-only`: one to six emoji, stock or the message's own `:shortcode:`s, and nothing else.
fn jumbo(ctx: &Ctx, m: &Message, text: &str) -> Option<Vec<Jumbo>> {
    let custom = custom_emoji_at(ctx.store, ctx.media, &m.emoji_tags, true);
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let (stock, code, after) = match rest.find(':') {
            Some(i) => {
                let tail = &rest[i + 1..];
                let end = tail.find(':');
                let code = end.map(|e| &tail[..e]).filter(|c| {
                    !c.is_empty()
                        && c.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '~' | '-'))
                        && m.emoji_tags.iter().any(|t| t.shortcode == *c)
                });
                match code {
                    Some(c) => (&rest[..i], Some(c), &tail[c.len() + 1..]),
                    // A colon that opens no known shortcode is text.
                    None => return None,
                }
            }
            None => (rest, None, ""),
        };
        emoji::only_emoji_count(stock)?;
        for g in unicode_segmentation::UnicodeSegmentation::graphemes(stock, true) {
            if let Some(src) = emoji::twemoji(g) {
                out.push(Jumbo::Stock(src));
            }
        }
        if let Some(code) = code {
            match custom.as_ref().and_then(|f| f(code))? {
                crate::media::Custom::Ready(image) => out.push(Jumbo::Custom(Some(image))),
                crate::media::Custom::Pending => out.push(Jumbo::Custom(None)),
                crate::media::Custom::Missing(_) => return None,
            }
        }
        if out.len() > 6 {
            return None;
        }
        rest = after;
    }
    (!out.is_empty()).then_some(out)
}

/// A message's NIP-30 tags as the renderer asks for them: a web URL through the emoji cache,
/// a fixture's path from beside its file.
pub fn custom_emoji<'a>(
    store: &'a Store,
    media: &'a std::cell::RefCell<crate::media::Media>,
    tags: &'a [vector_core::types::EmojiTag],
) -> Option<impl Fn(&str) -> Option<crate::media::Custom> + 'a> {
    custom_emoji_at(store, media, tags, false)
}

/// [`custom_emoji`], `big` for a jumbo row's close-up rather than text size.
fn custom_emoji_at<'a>(
    store: &'a Store,
    media: &'a std::cell::RefCell<crate::media::Media>,
    tags: &'a [vector_core::types::EmojiTag],
    big: bool,
) -> Option<impl Fn(&str) -> Option<crate::media::Custom> + 'a> {
    (!tags.is_empty()).then(move || {
        move |code: &str| {
            let tag = tags.iter().find(|t| t.shortcode == code)?;
            let src = emoji_source(store, &tag.url);
            let mut media = media.borrow_mut();
            Some(if big { media.custom(&src, false) } else { media.custom_small(&src) })
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

/// `.dmsg-command-line`: "JSKitty ran /roll with Concordia", the arguments left out.
fn command_line(ctx: &Ctx, m: &Message, name: String, bot: Option<String>) -> AnyElement {
    let author = if m.mine { ctx.store.my_npub.clone() } else { m.npub.clone().unwrap_or_default() };
    let to = |npub: String| {
        let act = ctx.on_act.clone();
        let id = m.id.clone();
        move |_: &gpui::ClickEvent, w: &mut Window, cx: &mut App| {
            act(&id, Act::Text(Target::Mention(npub.clone().into())), w, cx)
        }
    };
    let word = |t: &'static str| div().child(t);
    let accent = |d: gpui::Stateful<gpui::Div>| {
        d.not_italic()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(c(PRIMARY))
            .cursor_pointer()
            .hover(|s| s.underline())
    };
    let rerun = {
        let act = ctx.on_act.clone();
        let (id, name) = (m.id.clone(), name.clone());
        move |_: &gpui::ClickEvent, w: &mut Window, cx: &mut App| act(&id, Act::Rerun(name.clone()), w, cx)
    };
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(4.))
        .text_size(px(13.))
        .line_height(px(21.))
        .italic()
        .text_color(white_a(0.45))
        .child(
            div()
                .id(SharedString::from(format!("cmd-author-{}", m.id)))
                .not_italic()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(white_a(0.8))
                .cursor_pointer()
                .child(ctx.store.name(&author))
                .on_click(to(author.clone())),
        )
        .child(word("ran"))
        .child(accent(div().id(SharedString::from(format!("cmd-name-{}", m.id))).child(format!("/{name}"))).on_click(rerun))
        .when_some(bot, |d, bot| {
            d.child(word("with")).child(
                div()
                    .id(SharedString::from(format!("cmd-bot-{}", m.id)))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(avatar(ctx.store, &bot, 15.))
                    .child(accent(div().id("bot-name").max_w(px(22. * 7.)).truncate().child(ctx.store.name(&bot))))
                    .on_click(to(bot.clone())),
            )
        })
        .into_any_element()
}

/// `.dmsg-content`: the text, then its trailing marks.
fn content(ctx: &Ctx, m: &Message) -> AnyElement {
    if let Some((name, bot)) = crate::data::command(m, ctx.commands) {
        return command_line(ctx, m, name, bot);
    }
    let empty = HashSet::new();
    let revealed = ctx.revealed.get(&m.id).unwrap_or(&empty);
    let id = m.id.clone();
    let act = ctx.on_act.clone();
    let click: OnClick = Rc::new(move |t, w, cx| act(&id, Act::Text(t), w, cx));
    let _selectable = crate::select::Scope::enter(m.at, &m.id);
    use vector_core::text_spans::TextSpan;
    let colors: Vec<vector_core::text_color::ColorSpan> = m
        .text_spans
        .iter()
        .filter_map(|s| match s {
            TextSpan::Color(c) => Some(c.clone()),
            _ => None,
        })
        .collect();
    let times: Vec<(usize, usize)> = m
        .text_spans
        .iter()
        .filter_map(|s| match s {
            TextSpan::Time(t) => Some((t.from as usize, t.to as usize)),
            _ => None,
        })
        .collect();
    let marked = crate::textcolor::mark(&m.content, &colors, &times);
    let content = match &marked {
        Some((text, at)) => {
            let moved = |x: u32, inclusive: bool| {
                x + at.iter().filter(|&&a| if inclusive { a <= x as usize } else { a < x as usize }).count() as u32
            };
            let spans: Vec<TextSpan> = m
                .text_spans
                .iter()
                .filter_map(|s| match s {
                    TextSpan::Time(t) => Some(TextSpan::Time(vector_core::text_time::TimeSpan {
                        from: moved(t.from, true),
                        to: moved(t.to, false),
                        ..t.clone()
                    })),
                    _ => None,
                })
                .collect();
            std::borrow::Cow::Owned(crate::texttime::spans_to_codes(text, &spans).into_owned())
        }
        None => crate::texttime::spans_to_codes(&m.content, &m.text_spans),
    };
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
    let bare: std::borrow::Cow<str> = if marked.is_some() {
        text.chars().filter(|&c| !crate::textcolor::is_sentinel(c)).collect::<String>().into()
    } else {
        text.as_ref().into()
    };
    if !headed && let Some(glyphs) = jumbo(ctx, m, &bare) {
        let mut row = div().flex().items_center().py(px(4.));
        for glyph in glyphs {
            row = row.child(match glyph {
                Jumbo::Stock(src) => img(src).size(px(50.)).flex_shrink_0().into_any_element(),
                Jumbo::Custom(Some(image)) => img(gpui::ImageSource::Render(image))
                    .size(px(50.))
                    .object_fit(gpui::ObjectFit::Contain)
                    .flex_shrink_0()
                    .into_any_element(),
                Jumbo::Custom(None) => crate::kit::emoji_loading(50.).into_any_element(),
            });
        }
        return div()
            .flex()
            .items_end()
            .child(row)
            .children(marks(m))
            .into_any_element();
    }

    let custom = custom_emoji(ctx.store, ctx.media, &m.emoji_tags);
    let chips = tag_chips(ctx, m);
    let here = ctx.store.community_of(&ctx.chat.id).map(|c| c.community_id.clone());
    let mut blocks = markdown::parse(
        text.trim(),
        &markdown::Ctx {
            base: base_look(),
            revealed,
            now: ctx.store.now_ms() as f64 / 1000.,
            mention: Some(&|npub: &str| ctx.store.chat_name(npub, &ctx.chat.id)),
            tags: &chips,
            mask: Some(&|t: &str| ctx.store.stream_mask(t)),
            colors: if marked.is_some() { &colors } else { &[] },
            go: Some(&|url: &str, look: &crate::inline::Look| crate::golinks::pill(ctx.store, url, here.as_deref(), look)),
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
    let mut col = dir_auto(div().flex().flex_col(), &blocks);
    let pieces = linked_pictures(ctx, &m.id, blocks);
    let mut prev_bottom: Option<f32> = None;
    let mut prev_para = false;
    let count = pieces.len();
    for (i, piece) in pieces.iter().enumerate() {
        let b = match piece {
            Piece::Block(b, _) => b,
            Piece::Picture { image, w, h, ext, src } => {
                // `.inline-image`: its own size, no taller than 400px nor wider than the bubble.
                let open = crate::viewer::Open {
                    image: image.clone(),
                    w: *w,
                    h: *h,
                    full: Some(crate::viewer::Full::linked(src)),
                };
                let s = (400. / *h as f32).min(1.);
                let (mut w, mut h) = (*w as f32 * s, *h as f32 * s);
                // Sized in pixels: left to its ratio, the picture is measured at its decoded
                // height, which a big animation shrinks, and painted taller over the next row.
                if ctx.content_w > 0. && w > ctx.content_w {
                    h *= ctx.content_w / w;
                    w = ctx.content_w;
                }
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
                            img(image.clone()).object_fit(gpui::ObjectFit::Cover)
                                .w_full()
                                .h(px(h))
                                .rounded(px(8.))
                                .hover(|s| s.opacity(0.9)),
                        )
                        .when(ctx.media.borrow().image_types, |d| d.children(crate::attachments::ext_badge(ext, w, h))),
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
        /// The type its link names (`getExtensionFromUrl`), for the badge.
        ext: String,
        /// What the linked cache reads it from, for the viewer's full-size copy.
        src: String,
    },
}

/// `getExtensionFromUrl`: what follows the last dot of the link's path.
fn url_ext(url: &str) -> String {
    let path = url::Url::parse(url).map(|u| u.path().to_string()).unwrap_or_else(|_| url.split(['?', '#']).next().unwrap_or("").to_string());
    match path.rfind('.') {
        Some(i) if i + 1 < path.len() && !path[i + 1..].contains('/') => path[i + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// `isImageUrl`: the path ends in a picture's extension, or the text names one anywhere.
pub fn is_image_url(url: &str) -> bool {
    const EXTS: [&str; 9] = ["png", "jpg", "jpeg", "gif", "webp", "jxl", "bmp", "ico", "svg"];
    let lower = url.to_ascii_lowercase();
    let path = lower.split(['?', '#']).next().unwrap_or("");
    EXTS.iter()
        .any(|e| path.ends_with(&format!(".{e}")) || lower.contains(&format!(".{e}")))
}

/// Links to pictures, as `processInlineImages` swaps them: a ring beside the link while it
/// fetches, then the picture in its place, splitting the paragraph around it.
fn linked_pictures(ctx: &Ctx, msg: &str, blocks: Vec<Block>) -> Vec<Piece> {
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
            let src = emoji_source(ctx.store, &url);
            match ctx.media.borrow_mut().linked_in(&src, msg) {
                Linked::Ready { image, w, h } => {
                    if !run.is_empty() {
                        out.push(Piece::Block(para(std::mem::take(&mut run)), cont));
                    }
                    out.push(Piece::Picture { image, w, h, ext: url_ext(&url), src });
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
pub(crate) fn margins(b: &Block, first: bool, last: bool) -> (f32, f32) {
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

pub(crate) fn block(b: &Block, key: &str, click: &OnClick, clock: f32) -> AnyElement {
    match b {
        Block::Para(p) => {
            let el = inline(p.atoms.clone(), p.base.clone())
                .dir_auto()
                .id(SharedString::from(key.to_string()))
                .on_click(click.clone())
                .clock(clock);
            match crate::select::key_for(key) {
                Some(k) => el.selectable(k).into_any_element(),
                None => el.into_any_element(),
            }
        }
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
/// The direction `dir="auto"` gives `blocks`: their first strong letter's, if any. Code has none.
fn starts_rtl(blocks: &[Block]) -> Option<bool> {
    fn para(p: &Para) -> Option<bool> {
        p.atoms.iter().find_map(|a| match a {
            Atom::Text { text, .. } => text.chars().find_map(|ch| match unicode_bidi::bidi_class(ch) {
                unicode_bidi::BidiClass::L => Some(false),
                unicode_bidi::BidiClass::R | unicode_bidi::BidiClass::AL => Some(true),
                _ => None,
            }),
            _ => None,
        })
    }
    blocks.iter().find_map(|b| match b {
        Block::Para(p) => para(p),
        Block::Quote(inner) => starts_rtl(inner),
        Block::List { items, .. } => items.iter().find_map(|i| starts_rtl(i)),
        Block::Table { head, rows, .. } => head.iter().chain(rows.iter().flatten()).find_map(para),
        Block::Code { .. } | Block::Rule => None,
    })
}

/// Left-to-right text keeps its bullets and quote bars on its own side in a mirrored UI.
fn dir_auto(col: gpui::Div, blocks: &[Block]) -> gpui::Div {
    col.when(gpui::layout_rtl() && starts_rtl(blocks) == Some(false), |d| d.keep_ltr())
}

pub fn stack(blocks: &[Block], key: &str, click: &OnClick, clock: f32, lh: f32) -> AnyElement {
    let mut col = dir_auto(div().flex().flex_col(), blocks);
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

/// A code block's sideways scroll: the offset, the gesture it has latched and a thumb drag,
/// kept by its key so they outlast the renders between.
#[derive(Clone, Default)]
struct CodeScroll {
    handle: gpui::ScrollHandle,
    /// The gesture in hand, while it is the block's: when its last event came.
    latched: Rc<std::cell::Cell<Option<std::time::Instant>>>,
    /// The thumb held: where the press began and the offset then.
    drag: Rc<std::cell::Cell<Option<(gpui::Pixels, gpui::Pixels)>>>,
}

fn code_scroll(key: &str) -> CodeScroll {
    thread_local!(static SCROLLS: std::cell::RefCell<HashMap<String, CodeScroll>> = Default::default());
    SCROLLS.with(|s| {
        let mut s = s.borrow_mut();
        if s.len() > 512 && !s.contains_key(key) {
            s.clear();
        }
        s.entry(key.to_string()).or_default().clone()
    })
}

/// How long a pause ends a gesture, for wheels that report no phases.
const GESTURE_GAP: std::time::Duration = std::time::Duration::from_millis(150);

impl CodeScroll {
    /// A browser's scroll latching: a gesture that opens sideways over a block that can scroll
    /// belongs to the block until it ends, its up-and-down drift with it, so the chat holds still.
    /// One that opens upright goes on to the chat untouched.
    fn latch(&self, e: &gpui::ScrollWheelEvent, cx: &mut App) {
        let now = std::time::Instant::now();
        let d = e.delta.pixel_delta(px(19.5));
        let ended = matches!(e.touch_phase, gpui::TouchPhase::Ended | gpui::TouchPhase::Cancelled);
        let fresh = e.touch_phase == gpui::TouchPhase::Started || self.latched.get().is_none_or(|t| now - t >= GESTURE_GAP);
        if fresh {
            let sideways = d.x.abs() > d.y.abs() && self.handle.max_offset().x > px(0.5);
            self.latched.set(sideways.then_some(now));
        } else if self.latched.get().is_some() {
            self.latched.set(Some(now));
        }
        if self.latched.get().is_some() {
            cx.stop_propagation();
        }
        if ended {
            self.latched.set(None);
        }
    }
}

/// `pre::-webkit-scrollbar`: the thumb, in the block's bottom padding, lit while hovered or
/// held; dragged, it scrolls in proportion, and a press on the track pages.
fn code_scrollbar(scroll: CodeScroll) -> AnyElement {
    use gpui::{Bounds, DispatchPhase, HitboxBehavior, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent};
    gpui::canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::BlockMouse),
        move |bounds: Bounds<gpui::Pixels>, hitbox, window, _| {
            let h = &scroll.handle;
            let (view, max) = (h.bounds().size.width, h.max_offset().x);
            if max <= px(0.5) || view <= px(0.) {
                return;
            }
            let track = bounds.size.width - px(4.);
            let w = (track * (view / (view + max))).max(px(24.));
            let room = (track - w).max(px(1.));
            let at = room * (-h.offset().x / max).clamp(0., 1.);
            let thumb = Bounds::new(gpui::point(bounds.origin.x + px(2.) + at, bounds.origin.y + px(2.)), gpui::size(w, px(6.)));
            let lit = scroll.drag.get().is_some() || (hitbox.is_hovered(window) && thumb.contains(&window.mouse_position()));
            let color = if lit { ca(180, 180, 180, 0.8) } else { ca(71, 71, 71, 0.6) };
            window.paint_quad(gpui::fill(thumb, color).corner_radii(px(4.)));
            let view_id = window.current_view();
            let set = {
                let h = h.clone();
                move |x: gpui::Pixels, cx: &mut App| {
                    let y = h.offset().y;
                    h.set_offset(gpui::point(x.clamp(-max, px(0.)), y));
                    cx.notify(view_id);
                }
            };
            {
                let (scroll, set, hitbox) = (scroll.clone(), set.clone(), hitbox.clone());
                window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || e.button != MouseButton::Left || !hitbox.is_hovered(window) {
                        return;
                    }
                    cx.stop_propagation();
                    let off = scroll.handle.offset().x;
                    if thumb.contains(&e.position) {
                        scroll.drag.set(Some((e.position.x, off)));
                        cx.notify(view_id);
                    } else {
                        // Page toward the press, a view at a time.
                        let toward = if e.position.x < thumb.origin.x { 1. } else { -1. };
                        set(off + view * toward, cx);
                    }
                });
            }
            {
                let scroll = scroll.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, _, _, cx| {
                    if let Some((x0, off0)) = scroll.drag.get() {
                        set(off0 - (e.position.x - x0) * (max / room), cx);
                    }
                });
            }
            window.on_mouse_event(move |_: &MouseUpEvent, _, _, cx| {
                if scroll.drag.take().is_some() {
                    cx.notify(view_id);
                }
            });
        },
    )
    .absolute()
    .left_0()
    .right_0()
    .bottom_0()
    .h(px(10.))
    .into_any_element()
}

/// `.code-block-wrapper` + `pre code.hljs` (GitHub Dark).
fn code_block(lang: &str, text: &SharedString, key: &str, click: &OnClick) -> AnyElement {
    let mut lines = div().flex().flex_col();
    let plain = c(crate::highlight::PLAIN);
    for (n, pieces) in crate::highlight::colored(text, lang).into_iter().enumerate() {
        // Each line its own paragraph, coloured run by run, so a drag selects code like prose.
        let atoms: Vec<Atom> = pieces
            .into_iter()
            .map(|(piece, color)| Atom::Text { text: piece.into(), look: Look::new(MONO, 13., 19.5, color), kind: Kind::Plain })
            .collect();
        let line_key = format!("{key}-{n}");
        let el = inline(atoms, Look::new(MONO, 13., 19.5, plain)).nowrap().ltr();
        let el = match crate::select::key_for(&line_key) {
            Some(k) => el.selectable(k),
            None => el,
        };
        lines = lines.child(div().h(px(19.5)).flex().child(div().flex_none().child(el.id(SharedString::from(line_key)))));
    }
    let copy = click.clone();
    let code: SharedString = crate::highlight::shown(text, lang).into();
    let scroll = code_scroll(key);
    div()
        .relative()
        .keep_ltr()
        .max_w(px(470.))
        .w_full()
        .rounded(px(6.))
        .border_1()
        .border_color(white_a(0.1))
        .bg(c(0x0d1117))
        .overflow_hidden()
        .child(
            // Sideways only: a vertical wheel goes on to the timeline.
            div()
                .id(SharedString::from(format!("{key}-code")))
                .overflow_x_scroll()
                .restrict_scroll_to_axis()
                .track_scroll(&scroll.handle)
                .on_scroll_wheel({
                    let scroll = scroll.clone();
                    move |e, _, cx| scroll.latch(e, cx)
                })
                // A row, so the lines keep their own width and the overflow is there to scroll.
                .flex()
                .p(px(13.))
                .font_family(MONO)
                .text_size(px(13.))
                .line_height(px(19.5))
                .text_color(c(crate::highlight::PLAIN))
                .child(lines.flex_none()),
        )
        // `pre { overflow-x: auto }`'s thumb, laid in the bottom padding so the block keeps its height.
        .child(code_scrollbar(scroll))
        .child(
            div()
                .id(SharedString::from(format!("{key}-copy")))
                .absolute()
                .top(px(8.))
                .right(px(8.))
                .size(px(32.))
                .rounded(px(6.))
                .border_1()
                // Opaque, so code scrolled under it doesn't show through: `.code-copy-btn`'s
                // black at 60%, at 70% opacity, as it falls on the block.
                .border_color(ca(89, 252, 179, 0.21))
                .bg(c(0x050709))
                .hover(|s| s.bg(c(0x030405)).border_color(ca(89, 252, 179, 0.5)))
                .group(SharedString::from(format!("{key}-copy")))
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_center()
                .on_click(move |_, w, cx| copy(Target::Code(code.clone()), w, cx))
                .child(
                    svg()
                        .path("icons/copy.svg")
                        .size(px(16.))
                        .text_color(c(ACCENT).opacity(0.7))
                        .group_hover(SharedString::from(format!("{key}-copy")), |s| s.text_color(c(ACCENT))),
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
        let el = inline(p.atoms.clone(), base).dir_auto().on_click(click.clone()).clock(clock);
        let el = match crate::select::key_for(&k) {
            Some(sel) => el.selectable(sel),
            None => el,
        };
        d.child(el.id(SharedString::from(k)))
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

/// `.dmsg-author-timeout` / `.member-timeout`: a running Timeout beside a name.
pub fn timeout_mark(store: &Store, community: Option<&str>, npub: &str) -> Option<AnyElement> {
    let t = store.timeout(community?, npub)?;
    Some(
        div()
            .id(SharedString::from(format!("to:{npub}")))
            .flex_shrink_0()
            .child(svg().path("icons/clock-snooze.svg").size(px(14.)).text_color(c(DANGER)))
            .tooltip(crate::kit::countdown_tip(t.until, store.now_ms() / 1000, |left| {
                if left > 0 { crate::t::member_timeout_ends(countdown(left as u64)).into() } else { crate::t::member_timeout_ending().into() }
            }))
            .into_any_element(),
    )
}

/// `fmtCountdown`: days and hours, hours and minutes, or m:ss.
pub fn countdown(secs: u64) -> String {
    if secs >= 86_400 {
        crate::t::countdown_days(secs / 86_400, secs % 86_400 / 3600).into()
    } else if secs >= 3600 {
        crate::t::countdown_hours(secs / 3600, secs % 3600 / 60).into()
    } else {
        crate::lang::digits(format!("{}:{:02}", secs / 60, secs % 60))
    }
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
        let history = m.edit_history.as_ref().is_some_and(|h| !h.is_empty());
        out.push(Atom::Text {
            text: "(edited)".into(),
            look: Look::new(RUBIK, 10., 14., white_a(0.4)),
            kind: if history { Kind::Action("edited".into()) } else { Kind::Plain },
        });
    }
    // `.dmsg-selfdestruct`: the message dissolves at its NIP-40 expiry; after any attachment, a
    // line of its own.
    if m.expiration.is_some() && m.attachments.is_empty() {
        if !m.edited {
            out.push(Atom::Text { text: " ".into(), look: base_look(), kind: Kind::Plain });
        }
        out.push(Atom::Gap(6.));
        out.push(Atom::Image {
            src: SharedString::from("ui/self-destruct-chip.svg").into(),
            size: 12.,
            drop: -2.,
            tip: m.expiration.map(|at| crate::inline::Tip::Dissolve(at as i64)),
            alt: SharedString::default(),
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
    let mut groups: Vec<(&str, usize, bool, Option<&str>)> = Vec::new();
    for r in ctx.store.counted(&ctx.chat.id, m) {
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
    if groups.is_empty() {
        ctx.media.borrow_mut().settle.arrived(&m.id, &m.id);
        return None;
    }
    let mut row = div()
        .flex()
        .flex_wrap()
        .gap(px(4.))
        .mt(px(4.))
        .mb(px(4.))
        .on_mouse_down(gpui::MouseButton::Right, |_, _, cx| cx.stop_propagation());
    for (emoji_str, count, mine, url) in groups {
        let custom = url.map(|u| {
            ctx.media
                .borrow_mut()
                .custom_small(&emoji_source(ctx.store, u))
        });
        let glyph: AnyElement = match (custom, emoji::twemoji(emoji_str)) {
            (Some(crate::media::Custom::Ready(image)), _) => img(image)
                .size(px(16.))
                .aspect_ratio(1.)
                .object_fit(gpui::ObjectFit::Contain)
                .into_any_element(),
            (Some(crate::media::Custom::Pending), _) => crate::kit::emoji_loading(16.).into_any_element(),
            (_, Some(src)) => img(src).size(px(16.)).into_any_element(),
            _ => div()
                .text_size(px(12.))
                .child(emoji_str.to_string())
                .into_any_element(),
        };
        let (act, id, e) = (ctx.on_act.clone(), m.id.clone(), emoji_str.to_string());
        let hover = {
            let (act, id, e) = (act.clone(), id.clone(), e.clone());
            move |h: &bool, w: &mut Window, cx: &mut App| act(&id, Act::ReactionHover(e.clone(), *h), w, cx)
        };
        let details = {
            let (act, id, e) = (act.clone(), id.clone(), e.clone());
            move |_: &gpui::MouseDownEvent, w: &mut Window, cx: &mut App| {
                cx.stop_propagation();
                act(&id, Act::ReactionDetails(e.clone()), w, cx)
            }
        };
        let press = {
            let (act, id, e) = (act.clone(), id.clone(), e.clone());
            move |_: &gpui::MouseDownEvent, w: &mut Window, cx: &mut App| act(&id, Act::ReactionPress(e.clone(), true), w, cx)
        };
        let release = {
            let (act, id, e) = (act.clone(), id.clone(), e.clone());
            move |_: &gpui::MouseUpEvent, w: &mut Window, cx: &mut App| act(&id, Act::ReactionPress(e.clone(), false), w, cx)
        };
        let key = crate::reactpop::chip_key(&m.id, emoji_str);
        let entering = ctx.media.borrow_mut().settle.arrived(&m.id, &key);
        let chip = div()
                .id(SharedString::from(format!("{}-r-{emoji_str}", m.id)))
                .relative()
                .on_hover(hover)
                .on_mouse_down(gpui::MouseButton::Right, details)
                .on_mouse_down(gpui::MouseButton::Left, press)
                .on_mouse_up(gpui::MouseButton::Left, release)
                .child(crate::kit::anchor(&ctx.anchors, key.clone()))
                .cursor_pointer()
                .when(!mine, |d| d.hover(|s| s.bg(white_a(0.1)).opacity(1.)))
                .on_click(move |_, w, cx| {
                    act(&id, if mine { Act::Unreact(e.clone()) } else { Act::ReactWith(e.clone()) }, w, cx)
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
                .child(crate::lang::num(count));
        row = row.child(if entering {
            crate::kit::chip_pop(SharedString::from(format!("{key}-in")), chip).into_any_element()
        } else {
            chip.into_any_element()
        });
    }
    if !ctx.store.can_react(&ctx.chat.id) {
        return Some(row.into_any_element());
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
    let parent = find(ctx, &m.replied_to);
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
            let text = ctx.store.resolve_mentions(&text, ctx.store.community_of(&ctx.chat.id).map(|c| c.community_id.as_str()));
            let mut atoms = crate::preview::reply_atoms(&text, &look);
            // Only the parent's own tags: a reader's pack would turn plain text into emoji.
            let tags = parent
                .map(|p| p.emoji_tags.as_slice())
                .filter(|t| !t.is_empty())
                .or(m.replied_to_emoji_tags.as_deref())
                .unwrap_or_default();
            if let Some(custom) = custom_emoji(ctx.store, ctx.media, tags) {
                atoms = crate::preview::with_customs(atoms, &custom);
            }
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .h(px(16.9))
                .child(inline(atoms, look))
                .into_any_element()
        }
        None => {
            let ext = m
                .replied_to_attachment_extension
                .clone()
                .or_else(|| parent.and_then(|p| p.attachments.first()).map(|a| a.extension.clone()))
                .filter(|e| !e.is_empty());
            let (icon, label) = ext.as_deref().map_or_else(|| ("attachment", crate::t::file_type_attachment()), crate::data::file_type);
            div()
                .flex()
                .items_center()
                .gap(px(5.))
                .text_color(white_a(0.45))
                .child(svg().path(SharedString::from(format!("icons/{icon}.svg"))).size(px(18.)).flex_shrink_0().text_color(white_a(0.45)))
                .child(label)
                .into_any_element()
        }
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
    // Icon, label (its tooltip), action, danger, dimmed.
    let mut acts: Vec<(&'static str, SharedString, Act, bool, bool)> = Vec::new();
    let community = ctx.store.community_of(&ctx.chat.id);
    let dissolved = community.is_some_and(|c| c.dissolved);
    if m.mine && m.pending && !m.failed {
        // An upload can be called off until its last byte has left; a text send has no cancel.
        let publishing = ctx.media.borrow().transfers.get(&m.id).is_some_and(|t| t.publishing());
        if m.attachments.is_empty() || publishing {
            return None;
        }
        acts.push(("icons/x.svg", crate::t::msg_cancel_upload(), Act::CancelUpload(m.id.clone()), true, false));
    } else if dissolved && !m.mine {
        return None;
    } else if m.mine && m.failed {
        let retry = match ctx.fail_reasons.get(&m.id) {
            Some(why) => crate::t::msg_bar_retry_why(why.as_str()),
            None => crate::t::msg_retry(),
        };
        acts.push(("icons/refresh.svg", retry, Act::Retry, false, false));
        acts.push(("icons/trash.svg", crate::t::msg_bar_delete_failed(), Act::Delete, true, false));
    } else {
        let unique: HashSet<&str> = m.reactions.iter().map(|r| r.emoji.as_str()).collect();
        if !dissolved && unique.len() < 8 && ctx.store.can_react(&ctx.chat.id) {
            acts.push(("icons/smile-face.svg", crate::t::msg_bar_react(), Act::React, false, false));
        }
        if !dissolved {
            acts.push(("icons/reply.svg", crate::t::msg_reply(), Act::Reply, false, false));
        }
        if !dissolved && m.mine && !m.content.is_empty() && m.attachments.is_empty() && crate::data::command(m, ctx.commands).is_none() {
            acts.push(("icons/edit.svg", crate::t::msg_edit(), Act::Edit, false, false));
        }
        if let Some(a) = m.attachments.iter().find(|a| a.downloaded && (a.resident || !a.path.is_empty())) {
            acts.push(("icons/copy.svg", crate::t::msg_copy(), Act::CopyFile(a.id.clone()), false, false));
            acts.push(if a.resident {
                ("icons/download.svg", crate::t::msg_save(), Act::RevealFile(a.id.clone()), false, false)
            } else {
                ("icons/file-search.svg", crate::t::msg_reveal(), Act::RevealFile(a.id.clone()), false, false)
            });
        }
        let meta = ctx.delete_meta.get(&m.id);
        if m.mine {
            let limited = meta.is_some_and(|o| o.limited);
            let label = if limited { crate::t::msg_bar_delete_limited() } else { crate::t::msg_bar_delete() };
            acts.push(("icons/trash.svg", label, Act::Delete, true, limited));
        } else if community.is_none() && ctx.chat.id.starts_with("npub1") {
            acts.push(("icons/trash.svg", crate::t::msg_bar_delete(), Act::Delete, true, false));
        } else if community.is_some() && meta.is_some_and(|o| o.hide) {
            acts.push(("icons/trash.svg", crate::t::msg_bar_hide(), Act::Delete, true, false));
        }
    }
    // A continuation's text starts at its top edge: lift the bar clear of it.
    let lift = if first { 16. } else { 32. };
    let mut bar = div()
        .id(SharedString::from(format!("{}-toolbar", m.id)))
        .absolute()
        .top(px(-lift))
        // Clear of the 22px scrollbar strip, which would otherwise take the last button's edge.
        .right(px(26.))
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
        .block_mouse_except_scroll()
        .on_hover({
            let (act, id) = (ctx.on_act.clone(), m.id.clone());
            move |h, w, cx| act(&id, Act::Hover(*h), w, cx)
        });
    for (i, (icon, label, act, danger, dim)) in acts.into_iter().enumerate() {
        let (on, id) = (ctx.on_act.clone(), m.id.clone());
        bar = bar.child(
            div()
                .id(i)
                .tip(label)
                .when(dim, |d| d.opacity(0.45))
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
            Some(crate::t::invite_card_unavailable().into()),
            crate::t::invite_card_unavailable_text().into(),
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
            .child(if joined.is_some() { crate::t::invite_card_open() } else { crate::t::invite_card_join() })
    });
    let card = div()
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
            crate::kit::spaced(&crate::t::invite_card_title().to_uppercase(), 0.9)
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
        );
    let fresh = ctx.media.borrow_mut().settle.fresh(&format!("cic-{key}"), state.is_some());
    if fresh && ready {
        crate::kit::settle_pop(SharedString::from(format!("cic-pop-{}-{key}", m.id)), card).into_any_element()
    } else {
        card.into_any_element()
    }
}

/// `PivxBubble`: the amount, its fiat line, and the claim state the balance check settles.
fn pivx_bubble(ctx: &Ctx, m: &Message, pay: &crate::data::PivxPay) -> AnyElement {
    use crate::data::PivxPhase;
    let (phase, hint) = match ctx.store.pivx_bubbles.get(&pay.gift_code) {
        Some((p, h)) => (*p, h.clone()),
        None => (PivxPhase::Claimable, if m.mine { crate::t::pivx_bubble_reclaim() } else { crate::t::pivx_bubble_claim() }.to_string()),
    };
    let fiat = ctx
        .store
        .pivx_price
        .as_ref()
        .map(|(price, cur)| crate::t::pivx_fiat(pay.amount_piv * price, cur.as_str()));
    let busy = matches!(phase, PivxPhase::Syncing | PivxPhase::Claiming);
    let open = !busy && phase != PivxPhase::Claimed;
    let (act, code) = (ctx.on_act.clone(), pay.gift_code.clone());
    let id = m.id.clone();
    div()
        .id(SharedString::from(format!("{}-pivx", m.id)))
        .flex()
        .items_center()
        .p(px(10.))
        .mt(px(10.))
        .rounded(px(16.))
        .border_1()
        .border_color(c(0x9530f4))
        .bg(ca(0x95, 0x30, 0xf4, 0x40 as f32 / 255.))
        .opacity(match phase {
            PivxPhase::Syncing => 0.6,
            PivxPhase::Claiming => 0.7,
            PivxPhase::Claimed => 0.5,
            _ => 1.,
        })
        .when(open, |d| {
            d.cursor_pointer()
                .hover(|s| s.bg(ca(0x95, 0x30, 0xf4, 0x60 as f32 / 255.)))
                .on_click(move |_, w, cx| act(&id, Act::PivxClaim(code.clone()), w, cx))
        })
        .child(img("icons/pivx.svg").h(px(50.)).w(px(50.)).mr(px(10.)).flex_shrink_0())
        .child(
            div()
                .ml_auto()
                .flex()
                .flex_col()
                .items_end()
                .child(
                    div()
                        .text_size(px(18.))
                        .line_height(px(18.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c(TEXT))
                        .child(crate::t::pivx_amount(pay.amount_piv)),
                )
                .children(fiat.map(|f| div().text_size(px(11.)).line_height(px(13.)).text_color(c(TEXT)).opacity(0.7).child(f)))
                .child(
                    div()
                        .mt(px(2.))
                        .text_size(px(11.))
                        .line_height(px(15.4))
                        .text_color(c(TEXT))
                        .child(hint),
                ),
        )
        .into_any_element()
}
