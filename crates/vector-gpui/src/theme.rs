//! Vector's palette and type, as the Tauri stylesheets resolve them (`src/styles.css`,
//! `src/widescreen.css`, `src/themes/vector/dark.css`). Values that the CSS gives as a
//! translucent layer over a known backdrop are stored pre-composited, so one fill paints them.

use gpui::{Hsla, Rgba, rgb};

pub const RUBIK: &str = "Rubik"; // i18n-exempt: font name
pub const INTER: &str = "Inter"; // i18n-exempt: font name
/// What a webview's `monospace` resolves to on each platform.
#[cfg(target_os = "macos")]
pub const MONO: &str = "Menlo"; // i18n-exempt: font name
#[cfg(target_os = "windows")]
pub const MONO: &str = "Consolas"; // i18n-exempt: font name
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const MONO: &str = "DejaVu Sans Mono"; // i18n-exempt: font name

pub fn c(v: u32) -> Hsla {
    rgb(hex(v)).into()
}

/// A theme colour at alpha `a`, as `rgba(...)` over the chosen theme's value.
pub fn ta(token: u32, a: f32) -> Hsla {
    let h = hex(token);
    ca((h >> 16) as u8, (h >> 8) as u8, h as u8, a)
}

/// `rgba(r, g, b, a)` from CSS.
pub fn ca(r: u8, g: u8, b: u8, a: f32) -> Hsla {
    Rgba {
        r: r as f32 / 255.,
        g: g as f32 / 255.,
        b: b as f32 / 255.,
        a,
    }
    .into()
}

pub fn white_a(a: f32) -> Hsla {
    ca(255, 255, 255, a)
}

// Theme colours are tokens above 24 bits, so no stored colour (a role's, a coin's) is ever
// mistaken for one; `c` and `ta` resolve them against the chosen theme.
const TOKEN: u32 = 0x100_0000;
/// `--primary-color`.
pub const PRIMARY: u32 = TOKEN;
/// `--accent-color`.
pub const ACCENT: u32 = TOKEN + 1;
/// `--danger-color`.
pub const DANGER: u32 = TOKEN + 2;
/// `--badge-ink`.
pub const BADGE_INK: u32 = TOKEN + 3;
/// `--slider-fill`.
pub const SLIDER: u32 = TOKEN + 4;
/// `.input-box`'s gradient, first and last stops.
pub const INPUT_FROM: u32 = TOKEN + 5;
pub const ACCENT_DEEP: u32 = TOKEN + 6;
/// `.progress-bar-fill`'s gradient.
pub const PROGRESS_FROM: u32 = TOKEN + 7;
pub const PROGRESS_TO: u32 = TOKEN + 8;
/// A checked `.neon-toggle`'s gradient.
pub const TOGGLE_FROM: u32 = TOKEN + 9;
pub const TOGGLE_TO: u32 = TOKEN + 10;
/// `.file-preview-btn-send`'s gradient.
pub const SEND_FROM: u32 = TOKEN + 11;
pub const SEND_TO: u32 = TOKEN + 12;
const TOKENS: usize = 13;

/// Vector's own values, also what a theme leaves out falls back to.
const VECTOR: [u32; TOKENS] = [
    0x33db98, 0x59fcb3, 0xff2ea9, 0x052b1e, 0x6dd89d, 0x59fcb3, 0x2b976c, 0x33db98, 0x00ff9d, 0x33db98, 0x00ff9d,
    0x33db98, 0x00ff9d,
];

static PALETTE: [std::sync::atomic::AtomicU32; TOKENS] = {
    let mut p = [const { std::sync::atomic::AtomicU32::new(0) }; TOKENS];
    let mut i = 0;
    while i < TOKENS {
        p[i] = std::sync::atomic::AtomicU32::new(VECTOR[i]);
        i += 1;
    }
    p
};
static CHOSEN: std::sync::Mutex<&'static str> = std::sync::Mutex::new("vector");
/// The chosen theme's `.input-box` and `.progress-bar-fill` gradients, every stop.
static STOPS: std::sync::Mutex<[Vec<(u32, f32)>; 2]> = std::sync::Mutex::new([Vec::new(), Vec::new()]);

/// A gradient a theme may give more than two stops.
#[derive(Clone, Copy)]
pub enum Grad {
    Input,
    Progress,
}

/// The chosen theme's stops for `g`, positions 0..1; empty for a plain two-stop one.
pub fn stops(g: Grad) -> Vec<(u32, f32)> {
    STOPS.lock().unwrap_or_else(|e| e.into_inner())[g as usize].clone()
}

/// A colour's value now: a theme token's under the chosen theme, any other as it is.
pub fn hex(v: u32) -> u32 {
    match v.checked_sub(TOKEN) {
        Some(i) if (i as usize) < TOKENS => PALETTE[i as usize].load(std::sync::atomic::Ordering::Relaxed),
        _ => v,
    }
}

/// The Tauri themes' stylesheets, read at build time so both clients wear the same colours.
const SHEETS: [(&str, &str); 7] = [
    ("vector", include_str!("../../../src/themes/vector/dark.css")),
    ("cyberpunk", include_str!("../../../src/themes/cyberpunk/dark.css")),
    ("monero", include_str!("../../../src/themes/monero/dark.css")),
    ("pivx", include_str!("../../../src/themes/pivx/dark.css")),
    ("satoshi", include_str!("../../../src/themes/satoshi/dark.css")),
    ("chatstr", include_str!("../../../src/themes/chatstr/dark.css")),
    ("gifverse", include_str!("../../../src/themes/gifverse/dark.css")),
];

/// Wear theme `name` (a Tauri theme id; unknown ones wear Vector). Returns whether it changed.
pub fn set_theme(name: &str) -> bool {
    let (id, css) = SHEETS.iter().find(|(id, _)| *id == name).copied().unwrap_or(SHEETS[0]);
    let mut chosen = CHOSEN.lock().unwrap_or_else(|e| e.into_inner());
    if *chosen == id {
        return false;
    }
    *chosen = id;
    for (slot, v) in PALETTE.iter().zip(palette(css)) {
        slot.store(v, std::sync::atomic::Ordering::Relaxed);
    }
    let vars = vars(css);
    let many = |selector: &str| {
        let s = block(css, selector)
            .and_then(|b| b.split(';').find_map(|d| d.trim().strip_prefix("background:")))
            .map(|v| positioned(v, &vars))
            .unwrap_or_default();
        if s.len() > 2 { s } else { Vec::new() }
    };
    *STOPS.lock().unwrap_or_else(|e| e.into_inner()) = [many(".input-box"), many(".progress-bar-fill")];
    true
}

/// A gradient's stops with their places, unplaced ones spread evenly as CSS does.
fn positioned(value: &str, vars: &[(&str, &str)]) -> Vec<(u32, f32)> {
    let mut out: Vec<(u32, Option<f32>)> = Vec::new();
    for part in value.split(',') {
        let Some(&colour) = colours(part, vars).first() else { continue };
        let at = part.split_whitespace().last().and_then(|w| w.strip_suffix('%')).and_then(|p| p.parse::<f32>().ok()).map(|p| p / 100.);
        out.push((colour, at));
    }
    let n = out.len();
    if n == 0 {
        return Vec::new();
    }
    if out[0].1.is_none() {
        out[0].1 = Some(0.);
    }
    if out[n - 1].1.is_none() {
        out[n - 1].1 = Some(1.);
    }
    let mut i = 0;
    while i < n {
        if out[i].1.is_some() {
            i += 1;
            continue;
        }
        let from = i - 1;
        let to = (i..n).find(|&j| out[j].1.is_some()).unwrap_or(n - 1);
        let (a, b) = (out[from].1.unwrap_or(0.), out[to].1.unwrap_or(1.));
        for (k, stop) in out.iter_mut().enumerate().take(to).skip(i) {
            stop.1 = Some(a + (b - a) * (k - from) as f32 / (to - from) as f32);
        }
        i = to;
    }
    out.into_iter().map(|(c, at)| (c, at.unwrap_or(0.))).collect()
}

pub fn theme() -> &'static str {
    *CHOSEN.lock().unwrap_or_else(|e| e.into_inner())
}

/// The picture a theme lays faintly behind the messages, and how strongly.
pub fn chat_backdrop() -> Option<(&'static [u8], f32)> {
    let id = theme();
    let png: &'static [u8] = match id {
        "cyberpunk" => include_bytes!("../../../src/themes/cyberpunk/chat-bg.png"),
        "monero" => include_bytes!("../../../src/themes/monero/chat-bg.png"),
        "pivx" => include_bytes!("../../../src/themes/pivx/chat-bg.png"),
        _ => return None,
    };
    let css = SHEETS.iter().find(|(t, _)| *t == id)?.1;
    let opacity = vars(css).into_iter().find(|(k, _)| *k == "chat-bg-opacity").and_then(|(_, v)| v.trim().parse().ok());
    Some((png, opacity.unwrap_or(0.1)))
}

/// Every token's value under one theme's stylesheet.
fn palette(css: &str) -> [u32; TOKENS] {
    let vars = vars(css);
    let var = |name: &str| -> Option<u32> {
        let mut v = vars.iter().find(|(k, _)| *k == name)?.1;
        // `var(--other)` chains, as `--badge-bg: var(--accent-color)` does.
        for _ in 0..4 {
            match v.trim().strip_prefix("var(--").and_then(|r| r.strip_suffix(')')) {
                Some(next) => v = vars.iter().find(|(k, _)| *k == next)?.1,
                None => break,
            }
        }
        colours(v, &vars).first().copied()
    };
    let stops = |selector: &str| -> Option<(u32, u32)> {
        let body = block(css, selector)?;
        let value = body.split(';').find_map(|d| d.trim().strip_prefix("background:").or_else(|| d.trim().strip_prefix("background-color:")))?;
        let found = colours(value, &vars);
        let (first, last) = (*found.first()?, *found.last()?);
        Some(if value.contains("270deg") { (last, first) } else { (first, last) })
    };
    let mut out = VECTOR;
    let primary = var("primary-color");
    let set = |out: &mut [u32; TOKENS], token: u32, v: Option<u32>| {
        if let Some(v) = v {
            out[(token - TOKEN) as usize] = v;
        }
    };
    set(&mut out, PRIMARY, primary);
    set(&mut out, ACCENT, var("accent-color"));
    // styles.css's default, which only Vector's sheet overrides.
    out[(DANGER - TOKEN) as usize] = var("danger-color").unwrap_or(0xfc595c);
    set(&mut out, BADGE_INK, var("badge-ink"));
    set(&mut out, SLIDER, var("slider-fill").or(var("icon-color-primary")));
    for (selector, from, to) in [
        (".input-box", INPUT_FROM, ACCENT_DEEP),
        (".progress-bar-fill", PROGRESS_FROM, PROGRESS_TO),
        ("input[type=\"checkbox\"]:checked + .neon-toggle", TOGGLE_FROM, TOGGLE_TO), // i18n-exempt: a CSS selector
        (".file-preview-btn-send", SEND_FROM, SEND_TO),
    ] {
        if let Some((a, b)) = stops(selector) {
            set(&mut out, from, Some(a));
            set(&mut out, to, Some(b));
        }
    }
    out
}

/// `:root`'s custom properties, names without their `--`.
fn vars(css: &str) -> Vec<(&str, &str)> {
    block(css, ":root")
        .map(|b| b.split(';').filter_map(|d| d.trim().strip_prefix("--")?.split_once(':')).map(|(k, v)| (k.trim(), v.trim())).collect())
        .unwrap_or_default()
}

/// The declarations of the first rule whose selector is exactly `selector`.
fn block<'a>(css: &'a str, selector: &str) -> Option<&'a str> {
    let mut rest = css;
    while let Some(open) = rest.find('{') {
        let head = rest[..open].rsplit('}').next().unwrap_or("").trim();
        let close = open + rest[open..].find('}')?;
        if head == selector {
            return Some(&rest[open + 1..close]);
        }
        rest = &rest[close + 1..];
    }
    None
}

/// The colours a value names, in order: `#rgb`, `#rrggbb(aa)`, `rgb(a)(...)` and `var(--x)`.
fn colours(value: &str, vars: &[(&str, &str)]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('#') {
            let n = r.bytes().take_while(u8::is_ascii_hexdigit).count();
            let h = &r[..n];
            let v = match n {
                3 => u32::from_str_radix(h, 16).ok().map(|v| ((v >> 8) & 0xf) * 0x110000 + ((v >> 4) & 0xf) * 0x1100 + (v & 0xf) * 0x11),
                6 => u32::from_str_radix(h, 16).ok(),
                8 => u32::from_str_radix(&h[..6], 16).ok(),
                _ => None,
            };
            out.extend(v);
            rest = &r[n..];
        } else if let Some(r) = rest.strip_prefix("rgba(").or_else(|| rest.strip_prefix("rgb(")) {
            let end = r.find(')').unwrap_or(r.len());
            let parts: Vec<u32> = r[..end].split(',').take(3).filter_map(|p| p.trim().parse().ok()).collect();
            if let [r, g, b] = parts[..] {
                out.push((r.min(255) << 16) | (g.min(255) << 8) | b.min(255));
            }
            rest = r.get(end + 1..).unwrap_or("");
        } else if let Some(r) = rest.strip_prefix("var(--") {
            let end = r.find(')').unwrap_or(r.len());
            if let Some((_, v)) = vars.iter().find(|(k, _)| *k == &r[..end]) {
                out.extend(colours(v, vars).first());
            }
            rest = r.get(end + 1..).unwrap_or("");
        } else {
            let skip = rest.chars().next().map_or(1, char::len_utf8);
            rest = &rest[skip..];
        }
    }
    out
}

// Surfaces.
pub const APP_BG: u32 = 0x030303;
/// `#window-chrome` over the app background.
pub const CHROME_BG: u32 = 0x090909;
/// The rail: rgba(255,255,255,.024) over the app background.
pub const RAIL_BG: u32 = 0x080808;
/// The chat list: rgba(255,255,255,.01) over the app background.
pub const LIST_BG: u32 = 0x050505;
/// A raised tile on the rail: rgba(255,255,255,.043) over the rail.
pub const RAIL_TILE: u32 = 0x121212;
pub const DIVIDER: u32 = 0x171717;
/// `--ws-hover` over the chrome.
pub const HOVER: u32 = 0x1c1c1c;
pub const CHAT_BG: u32 = 0x171717;
pub const CHAT_HEADER_BG: u32 = 0x0c0c0c;
pub const MESSAGES_BG: u32 = 0x030303;

// Text.
pub const TEXT: u32 = 0xf6f6f6;
pub const ICON_MUTED: u32 = 0xb2b2b2;
pub const CHROME_HELP: u32 = 0x474747;
pub const LIST_TIME: u32 = 0x808080;
pub const PREVIEW_UNREAD: u32 = 0xa0a0a0;

thread_local! {
    static SLIM: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Set once per frame from the window width, so far-off builders can follow the slim layout.
pub fn set_slim(slim: bool) {
    SLIM.with(|s| s.set(slim));
}

pub fn is_slim() -> bool {
    SLIM.with(|s| s.get())
}

/// `.chat-header`: slim's is unseamed, rgba(22,22,22,.9) over the messages; `body.ws` sits it on a seam.
pub fn chat_header_ground<D: gpui::Styled>(d: D, papered: bool) -> D {
    if is_slim() {
        d.h(gpui::px(60.)).bg(gpui::rgb(0x141414))
    } else if papered {
        d.bg(gpui::rgba(0x161616e6)).border_b_1().border_color(gpui::rgb(DIVIDER))
    } else {
        d.bg(gpui::rgb(CHAT_HEADER_BG)).border_b_1().border_color(gpui::rgb(DIVIDER))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_reads_like_its_stylesheet() {
        let hex = |p: [u32; TOKENS], t: u32| p[(t - TOKEN) as usize];
        let v = palette(SHEETS[0].1);
        assert_eq!(v, VECTOR, "Vector's sheet gives Vector's values");
        let cyber = palette(SHEETS[1].1);
        assert_eq!((hex(cyber, PRIMARY), hex(cyber, DANGER), hex(cyber, BADGE_INK)), (0x59fcf4, 0xfc59ad, 0x030303));
        assert_eq!((hex(cyber, INPUT_FROM), hex(cyber, ACCENT_DEEP)), (0x59fcf4, 0xfc59ad), "var() stops resolve");
        assert_eq!((hex(cyber, TOGGLE_FROM), hex(cyber, TOGGLE_TO)), (0xfc59ad, 0x59fcf4), "270deg runs right to left");
        let monero = palette(SHEETS[2].1);
        assert_eq!((hex(monero, ACCENT), hex(monero, DANGER), hex(monero, SLIDER)), (0xff8e43, 0xfc595c, 0xff6600));
        assert_eq!((hex(monero, INPUT_FROM), hex(monero, ACCENT_DEEP)), (0xff6600, 0xff6600), "a solid fill is both stops");
        let gif = palette(SHEETS[6].1);
        assert_eq!((hex(gif, INPUT_FROM), hex(gif, ACCENT_DEEP)), (0xf7ffae, 0xffaece));
        for (id, css) in SHEETS {
            let p = palette(css);
            assert!(p.iter().all(|v| *v <= 0xff_ffff), "{id}");
        }
    }

    #[test]
    fn stops_spread_and_keep_their_places() {
        let v = positioned("linear-gradient(90deg, #F7FFAE, #AEFFDF, #B6AEFF, #FFAECE)", &[]);
        assert_eq!(v.iter().map(|s| s.1).collect::<Vec<_>>(), [0., 1. / 3., 2. / 3., 1.]);
        let v = positioned("linear-gradient(90deg, #F7FFAE 40%, #AEFFDF 60%, #B6AEFF 80%, #FFAECE 100%)", &[]);
        assert_eq!((v[0], v[3]), ((0xf7ffae, 0.4), (0xffaece, 1.)));
        let v = positioned("linear-gradient(90deg, #CD3DD3, #b188e2, #5749E4)", &[]);
        assert_eq!(v, [(0xcd3dd3, 0.), (0xb188e2, 0.5), (0x5749e4, 1.)]);
    }

    #[test]
    fn stored_colours_are_never_themed() {
        assert_eq!(hex(0x59fcb3), 0x59fcb3);
        assert_eq!(hex(0xffffff), 0xffffff);
    }
}
