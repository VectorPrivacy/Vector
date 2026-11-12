//! Vector's palette and type, as the Tauri stylesheets resolve them (`src/styles.css`,
//! `src/widescreen.css`, `src/themes/vector/dark.css`). Values that the CSS gives as a
//! translucent layer over a known backdrop are stored pre-composited, so one fill paints them.

use gpui::{Hsla, Rgba, rgb};

pub const RUBIK: &str = "Rubik";
pub const INTER: &str = "Inter";
/// What a webview's `monospace` resolves to on each platform.
#[cfg(target_os = "macos")]
pub const MONO: &str = "Menlo";
#[cfg(target_os = "windows")]
pub const MONO: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const MONO: &str = "DejaVu Sans Mono";

pub fn c(hex: u32) -> Hsla {
    rgb(hex).into()
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

// Accent family.
pub const PRIMARY: u32 = 0x33db98;
pub const ACCENT: u32 = 0x59fcb3;
pub const ACCENT_DEEP: u32 = 0x2b976c;

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
