//! The OpenGraph card under a message with a link (`LinkPreview.svelte`): favicon and title,
//! the description with its line breaks, then the picture, or a video's frame that opens it.

use gpui::{AnyElement, IntoElement, ParentElement, Styled, div, img, prelude::*, px, svg};
use vector_core::types::{Message, SiteMetadata};

use crate::inline::Target;
use crate::media::Linked;
use crate::theme::*;
use crate::timeline::{Act, Ctx, TEXT_LH, TEXT_SIZE};

const MAX_W: f32 = 480.;

/// A link a card can play: a YouTube video or a SoundCloud track.
struct Playable {
    youtube: bool,
    id: String,
    href: Option<String>,
}

fn host(u: &reqwest::Url) -> String {
    u.host_str().unwrap_or("").to_ascii_lowercase()
}

/// `youtubeVideo` / `soundcloudTrack`.
fn playable(href: &str) -> Option<Playable> {
    let u = reqwest::Url::parse(href).ok()?;
    let h = host(&u);
    let yt = h.trim_start_matches("www.").trim_start_matches("m.").trim_start_matches("music.");
    let id = if yt == "youtu.be" {
        u.path().split('/').nth(1).map(str::to_string)
    } else if yt == "youtube.com" || yt == "youtube-nocookie.com" {
        if u.path() == "/watch" {
            u.query_pairs().find(|(k, _)| k == "v").map(|(_, v)| v.into_owned())
        } else {
            let mut parts = u.path().split('/').skip(1);
            match (parts.next(), parts.next()) {
                (Some("shorts" | "embed" | "live" | "v"), Some(id)) => Some(id.to_string()),
                _ => None,
            }
        }
    } else {
        None
    };
    if let Some(id) = id.filter(|id| id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')) {
        return Some(Playable { youtube: true, id, href: None });
    }
    const PAGES: [&str; 21] = [
        "discover", "stream", "you", "search", "upload", "charts", "tags", "stations", "pages", "settings", "messages", "notifications",
        "people", "popular", "terms-of-use", "pro", "mobile", "jobs", "imprint", "logout", "signin",
    ];
    if h.trim_start_matches("www.").trim_start_matches("m.") != "soundcloud.com" {
        return None;
    }
    let slug = |s: &str| (1..=255).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    let mut parts: Vec<&str> = u.path().trim_end_matches('/').split('/').skip(1).collect();
    if parts.first().is_none_or(|p| !slug(p) || PAGES.contains(&p.to_ascii_lowercase().as_str())) {
        return None;
    }
    let secret = (parts.len() > 2 && parts.last().is_some_and(|s| s.len() > 2 && s.starts_with("s-") && s[2..].bytes().all(|b| b.is_ascii_alphanumeric())))
        .then(|| parts.pop())
        .flatten();
    let ok = (parts.len() == 2 && slug(parts[1]) && parts[1] != "sets") || (parts.len() == 3 && parts[1] == "sets" && slug(parts[2]));
    ok.then(|| Playable { youtube: false, id: parts.into_iter().chain(secret).collect::<Vec<_>>().join("/"), href: None })
}

fn urls(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c.is_whitespace()).filter(|t| t.starts_with("http://") || t.starts_with("https://"))
}

/// `_dmsgPreviewVideo`: what the card plays, with the link as it was sent.
fn video(m: &Message, meta: &SiteMetadata) -> Option<Playable> {
    let sent: Vec<Playable> = urls(&m.content).filter_map(|href| playable(href).map(|p| Playable { href: Some(href.to_string()), ..p })).collect();
    let mut card = match &meta.og_url {
        Some(u) => playable(u),
        None => sent.first().map(|p| Playable { youtube: p.youtube, id: p.id.clone(), href: p.href.clone() }),
    };
    let youtube_host = reqwest::Url::parse(&meta.domain).is_ok_and(|u| {
        matches!(host(&u).trim_start_matches("www.").trim_start_matches("m.").trim_start_matches("music."), "youtube.com" | "youtu.be")
    });
    if card.is_none() && youtube_host {
        let thumb = meta.og_image.clone().unwrap_or_default();
        let thumb = percent_decode(&thumb);
        card = sent
            .iter()
            .find(|v| thumb.contains(&format!("/vi/{}/", v.id)))
            .or(sent.first())
            .map(|p| Playable { youtube: p.youtube, id: p.id.clone(), href: p.href.clone() });
    }
    let card = card?;
    Some(match sent.into_iter().find(|v| v.id == card.id) {
        Some(v) => v,
        None => card,
    })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A link a page's own metadata names: only http(s) reaches the opener.
fn safe_url(url: &str) -> Option<String> {
    let has_scheme = url.split_once(':').is_some_and(|(s, _)| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c)));
    let u = reqwest::Url::parse(&if has_scheme { url.to_string() } else { format!("https://{url}") }).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.to_string())
}

fn is_image_text(text: &str) -> bool {
    urls(text).any(crate::timeline::is_image_url)
}

/// The card for `m`, or `None`; a message with a link and no metadata yet asks for it once.
pub fn card(ctx: &Ctx, m: &Message) -> Option<AnyElement> {
    if m.pending || m.failed || m.content.is_empty() {
        return None;
    }
    let lower = m.content.to_ascii_lowercase();
    if lower.contains("vectorapp.io/profile/npub1") {
        return None;
    }
    let shared = |u: &str| {
        let u = u.to_ascii_lowercase();
        let u = u.trim_start_matches("https://").trim_start_matches("http://").trim_start_matches("www.");
        u.starts_with("vectorapp.io/emojis/pack/") || u.starts_with("vectorapp.io/invite")
    };
    if ctx.held {
        return None;
    }
    let Some(meta) = &m.preview_metadata else {
        // Links a Nostr card covers, invites and pack shares are not pages to preview.
        let previewable = urls(&m.content).any(|u| {
            u.starts_with("https://") && !shared(u) && vector_core::nostr_embed::EmbedRef::from_url(u).is_none() && !u.starts_with('<')
        });
        if previewable && !is_image_text(&m.content) {
            let mut media = ctx.media.borrow_mut();
            if media.meta_asked.insert(m.id.clone()) {
                media.meta_queue.push((ctx.chat.id.clone(), m.id.clone()));
            }
        }
        return None;
    };
    if meta.og_url.as_deref().is_some_and(shared) {
        return None;
    }
    if !urls(&m.content).any(|u| u.starts_with("https://") && vector_core::nostr_embed::EmbedRef::from_url(u).is_none()) {
        return None;
    }
    let title = meta.title.clone().or(meta.og_title.clone());
    let description = meta.og_description.clone().or(meta.description.clone()).unwrap_or_default();
    if meta.og_image.is_none() && title.is_none() && description.is_empty() {
        return None;
    }
    let video = video(m, meta);
    let url = video.as_ref().and_then(|v| v.href.clone()).or(meta.og_url.clone()).unwrap_or(meta.domain.clone());
    let image = meta.og_image.clone();
    let audio = video.as_ref().is_some_and(|v| !v.youtube);

    let open = {
        let (act, id, url) = (ctx.on_act.clone(), m.id.clone(), url.clone());
        move |_: &gpui::ClickEvent, w: &mut gpui::Window, cx: &mut gpui::App| {
            if let Some(u) = safe_url(&url) {
                act(&id, Act::Text(Target::Link(u.into())), w, cx);
            }
        }
    };

    let favicon: AnyElement = match meta.favicon.as_deref().map(|f| ctx.media.borrow_mut().linked(f)) {
        Some(Linked::Ready { image, w, h }) => {
            let wide = 15. * w as f32 / h.max(1) as f32;
            img(image).h(px(15.)).w(px(wide)).mr(px(8.)).mt(px(1.)).flex_shrink_0().into_any_element()
        }
        // An image with no picture yet is only its margin; one that failed is gone.
        Some(Linked::Loading(_)) => div().w(px(8.)).into_any_element(),
        _ => div().into_any_element(),
    };
    let lines: Vec<String> = {
        let parts: Vec<&str> = split_br(&description);
        let n = parts.len();
        parts.iter().enumerate().flat_map(|(i, part)| {
            let mut subs: Vec<String> = part.split('\n').map(str::to_string).collect();
            if i + 1 < n {
                subs.push(String::new());
            }
            subs
        }).collect()
    };
    let picture = image.as_deref().map(|u| ctx.media.borrow_mut().linked(u));

    let mut card = div()
        .id(gpui::SharedString::from(format!("card-{}", m.id)))
        .relative()
        .mt(px(8.))
        .max_w(px(MAX_W + 22.))
        .px(px(10.))
        .pt(px(8.))
        .pb(px(if !description.is_empty() && image.is_some() && !audio { 0. } else { 8. }))
        .rounded(px(8.))
        .border_1()
        .border_color(white_a(0.08))
        .bg(ca(0, 0, 0, 0.3))
        .cursor_pointer()
        .on_click(open.clone())
        // The left edge in the accent, uniform with the rest.
        .child(div().absolute().left(px(-1.)).top(px(7.)).bottom(px(7.)).w(px(1.)).bg(c(PRIMARY)))
        .child(
            div()
                .flex()
                .items_start()
                .text_size(px(TEXT_SIZE))
                .line_height(px(TEXT_LH))
                .text_color(white_a(0.85))
                .child(favicon)
                .child(div().min_w_0().child(title.unwrap_or_else(|| "Link Preview".into()))),
        );
    if !description.is_empty() {
        let mut d = div().mt(px(4.)).text_size(px(13.)).line_height(px(18.2)).text_color(white_a(0.7)).flex().flex_col();
        for l in lines {
            d = d.child(div().min_h(px(18.2)).child(l));
        }
        card = card.child(d);
    }
    if let Some(v) = &video {
        let mut frame = div().relative().mt(px(6.)).w_full().rounded(px(4.)).overflow_hidden().bg(c(0x000000));
        frame = if audio { frame.h(px(166.)) } else { frame.aspect_ratio(16. / 9.).min_h(px(200.)) };
        if let Some(Linked::Ready { image, .. }) = &picture {
            frame = frame.child(img(image.clone()).absolute().inset_0().size_full().object_fit(gpui::ObjectFit::Cover));
        }
        let _ = v;
        frame = frame.child(
            div()
                .id("play")
                .group("play")
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .bg(ca(0, 0, 0, 0.25))
                .hover(|s| s.bg(ca(0, 0, 0, 0.4)))
                .child(
                    div()
                        .size(px(58.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(ca(0, 0, 0, 0.6))
                        .border_1()
                        .border_color(white_a(0.15))
                        .child(svg().path("icons/external.svg").size(px(22.)).text_color(c(0xffffff))),
                )
                .child(
                    div()
                        .px(px(8.))
                        .py(px(3.))
                        .rounded(px(10.))
                        .bg(ca(0, 0, 0, 0.6))
                        .text_size(px(12.))
                        .line_height(px(16.8))
                        .text_color(c(0xffffff))
                        .child("Opens in your browser"),
                ),
        );
        card = card.child(frame);
    } else if let Some(Linked::Ready { image, w, h }) = picture {
        let (w, h) = (w as f32, h.max(1) as f32);
        card = card.child(
            img(image)
                .mt(px(6.))
                .w_full()
                .aspect_ratio(w / h)
                .max_h(px(350.))
                .object_fit(gpui::ObjectFit::Cover)
                .rounded(px(4.)),
        );
    }
    Some(card.into_any_element())
}

/// The description split at `<br>`, `<br/>` and `<br />`, any case.
fn split_br(s: &str) -> Vec<&str> {
    let lower = s.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut at = 0;
    let mut from = 0;
    while let Some(i) = lower[from..].find("<br") {
        let start = from + i;
        let rest = &lower[start + 3..];
        let tail = rest.trim_start_matches(char::is_whitespace);
        let tail = tail.strip_prefix('/').unwrap_or(tail);
        if let Some(after) = tail.strip_prefix('>') {
            let end = lower.len() - after.len();
            out.push(&s[at..start]);
            at = end;
            from = end;
        } else {
            from = start + 3;
        }
    }
    out.push(&s[at..]);
    out
}
