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
    /// A collapsed post, or a run of them, tapped open.
    Unveil(Vec<String>),
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
    /// The Community's mention rules, for `@everyone` and Role chips.
    pub rules: Option<&'a crate::mentions::Rules>,
    /// The command names the chat's bots declare, once known.
    pub commands: Option<&'a HashSet<String>>,
    pub anchors: &'a crate::kit::Anchors,
    /// The message column's width, for media to fit; 0 before the list has laid out.
    pub content_w: f32,
    /// The window's width: what `100vw` reads.
    pub vw: f32,
}

/// `_dmsgIsPinged`: a tag of me, an authorised `@everyone`, a Role I hold, or a reply to me.
fn pings_me(ctx: &Ctx, m: &Message) -> bool {
    if m.mine || m.npub.as_deref().is_some_and(|n| ctx.store.profiles.get(n).is_some_and(|p| p.is_blocked)) {
        return false;
    }
    let me = ctx.store.my_npub.as_str();
    if !me.is_empty() && m.content.contains(&format!("@{me}")) {
        return true;
    }
    if !m.replied_to.is_empty() {
        let to_me = match ctx.chat.messages.iter().find(|p| p.id == m.replied_to) {
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
        && m.content.match_indices("@everyone").any(|(i, _)| {
            !m.content[i + 9..].chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_')
        });
    everyone
        || rules.roles.iter().any(|r| {
            r.held
                && !r.name.is_empty()
                && m.role_mentions.contains(&r.id)
                && m.content.contains(&format!("@{}", r.name))
                && (r.mentionable || by_anyone)
        })
}

/// The `@everyone` and Role chips `m` may show (`standingRoleMentions`): names and colours.
fn tag_chips(ctx: &Ctx, m: &Message) -> Vec<(String, Option<u32>)> {
    let Some(rules) = ctx.rules else { return Vec::new() };
    let sender = if m.mine { ctx.store.my_npub.as_str() } else { m.npub.as_deref().unwrap_or_default() };
    let by_anyone = rules.anyone || rules.pingers.iter().any(|p| p == sender);
    let mut out: Vec<(String, Option<u32>)> = rules
        .roles
        .iter()
        .filter(|r| {
            !r.name.is_empty()
                && m.role_mentions.contains(&r.id)
                && m.content.contains(&format!("@{}", r.name))
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
fn author_menu(ctx: &Ctx, m: &Message, npub: &str, key: &SharedString) -> impl Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static {
    let (act, id, npub, key) = (ctx.on_act.clone(), m.id.clone(), npub.to_string(), key.clone());
    move |e, w, cx| {
        cx.stop_propagation();
        act(&id, Act::AuthorMenu(npub.clone(), key.clone(), e.position), w, cx)
    }
}

pub fn message_row(ctx: &Ctx, index: usize, first: bool, run: u32, gap: f32) -> AnyElement {
    let m = &ctx.chat.messages[index];
    let author = author_of(ctx, m);
    let hidden = ctx.store.hidden(ctx.chat, m);
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
                    .relative()
                    .mt(px(if has_quote { 26. } else { 2. }))
                    .cursor_pointer()
                    .on_click(move |_, w, cx| act(&id, Act::Mini(who.clone(), k.clone()), w, cx))
                    .on_mouse_down(gpui::MouseButton::Right, author_menu(ctx, m, &author, &key))
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
                        .text_color(c(0xffffff))
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
        let (act, id) = (ctx.on_act.clone(), m.id.clone());
        let label: SharedString = if run > 1 { format!("{run} blocked messages").into() } else { why.label().into() };
        let icon = if matches!(why, data::Hidden::NoSend | data::Hidden::TimedOut) { "icons/clock.svg" } else { "icons/cancel.svg" };
        body = body.child(
            div().flex().child(
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
                    .on_click(move |_, w, cx| act(&id, Act::Unveil(ids.clone()), w, cx))
                    .child(svg().path(icon).size(px(14.)).flex_shrink_0().text_color(white_a(0.3)))
                    .child(label),
            ),
        );
        return row_shell(ctx, m, gap, gutter, body, false, replying, failed, hovered, first);
    }
    body = body.child(
        div()
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
                                    format!("Message dissolves in {}", countdown(left as u64))
                                } else {
                                    "Dissolving...".into()
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
    row_shell(ctx, m, gap, gutter, body, pinged, replying, failed, hovered, first)
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
                .group(group)
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
    if !headed && let Some(n @ 1..=6) = emoji::only_emoji_count(&bare) {
        let _ = n;
        let mut row = div().flex().items_center().py(px(4.));
        for g in unicode_segmentation::UnicodeSegmentation::graphemes(bare.as_ref(), true) {
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

/// `.dmsg-author-timeout` / `.member-timeout`: a running Timeout beside a name.
pub fn timeout_mark(store: &Store, community: Option<&str>, npub: &str) -> Option<AnyElement> {
    let t = store.timeout(community?, npub)?;
    Some(
        div()
            .id(SharedString::from(format!("to:{npub}")))
            .flex_shrink_0()
            .child(svg().path("icons/clock-snooze.svg").size(px(14.)).text_color(c(DANGER)))
            .tooltip(crate::kit::countdown_tip(t.until, store.now_ms() / 1000, |left| {
                if left > 0 { format!("Timeout ends in {}", countdown(left as u64)) } else { "Timeout ending".into() }
            }))
            .into_any_element(),
    )
}

/// `fmtCountdown`: days and hours, hours and minutes, or m:ss.
pub fn countdown(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d {}h", secs / 86_400, secs % 86_400 / 3600)
    } else if secs >= 3600 {
        format!("{}h {}m", secs / 3600, secs % 3600 / 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
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
        row = row.child(
            div()
                .id(SharedString::from(format!("{}-r-{emoji_str}", m.id)))
                .relative()
                .on_hover(hover)
                .on_mouse_down(gpui::MouseButton::Right, details)
                .on_mouse_down(gpui::MouseButton::Left, press)
                .on_mouse_up(gpui::MouseButton::Left, release)
                .child(crate::kit::anchor(&ctx.anchors, key))
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
            let text = ctx.store.resolve_mentions(&text, ctx.store.community_of(&ctx.chat.id).map(|c| c.community_id.as_str()));
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
        if m.mine && !m.content.is_empty() && m.attachments.is_empty() && crate::data::command(m, ctx.commands).is_none() {
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

/// `PivxBubble`: the amount, its fiat line, and the claim state the balance check settles.
fn pivx_bubble(ctx: &Ctx, m: &Message, pay: &crate::data::PivxPay) -> AnyElement {
    use crate::data::PivxPhase;
    let (phase, hint) = match ctx.store.pivx_bubbles.get(&pay.gift_code) {
        Some((p, h)) => (*p, h.clone()),
        None => (PivxPhase::Claimable, if m.mine { "Click to reclaim" } else { "Click to claim" }.to_string()),
    };
    let fiat = ctx
        .store
        .pivx_price
        .as_ref()
        .map(|(price, cur)| format!("~{:.2} {cur}", pay.amount_piv * price));
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
                        .child(format!("{:.2} PIV", pay.amount_piv)),
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
