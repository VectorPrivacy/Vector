//! `CryptoAddress.svelte` over `payments.js`: the first coin address a message holds, as a
//! card in the coin's colours with a Pay button opening its payment URI.

use std::sync::LazyLock;

use gpui::prelude::*;
use gpui::{AnyElement, FontWeight, SharedString, div, px};
use crate::pic::img;
use regex::Regex;

use crate::theme::*;

pub struct Coin {
    pub coin: &'static str,
    color: u32,
    uri: &'static str,
    pub address: String,
}

struct Kind {
    coin: &'static str,
    color: u32,
    uri: &'static str,
    transparent: bool,
    re: Regex,
}

/// Highest priority first; `\b` is ASCII, as in JS.
static KINDS: LazyLock<Vec<Kind>> = LazyLock::new(|| {
    let pivx = |pat: &str, transparent| Kind {
        coin: "PIVX",
        color: 0x9530f4,
        uri: "https://app.mypivxwallet.org/?pay=",
        transparent,
        re: Regex::new(&format!(r"(?-u:\b){pat}(?-u:\b)")).unwrap(),
    };
    vec![pivx("(?i:ps1[a-z0-9]{50,120})", false), pivx("D[a-zA-Z0-9]{33,35}", true)]
});

const BASE58: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

fn base58(s: &str) -> bool {
    let n = s.len();
    s.chars().all(|ch| BASE58.contains(ch)) && ((25..=35).contains(&n) || (50..=120).contains(&n))
}

/// `detectCryptoAddress`: each kind's first match only, skipped when it sits inside a URL.
pub fn detect(text: &str) -> Option<Coin> {
    if text.encode_utf16().count() < 25 {
        return None;
    }
    for k in KINDS.iter() {
        let Some(m) = k.re.find(text) else { continue };
        let before = text[..m.start()].chars().next_back();
        if matches!(before, Some('/' | '.' | ':' | '=' | '?')) {
            continue;
        }
        if matches!(text[m.end()..].chars().next(), Some('/' | '.')) {
            continue;
        }
        if k.transparent && !base58(m.as_str()) {
            continue;
        }
        return Some(Coin { coin: k.coin, color: k.color, uri: k.uri, address: m.as_str().to_string() });
    }
    None
}

pub fn card(id: &str, coin: &Coin, on_pay: impl Fn(String, &mut gpui::Window, &mut gpui::App) + 'static) -> AnyElement {
    let (r, g, b) = ((coin.color >> 16) as u8, (coin.color >> 8) as u8, coin.color as u8);
    let uri = format!("{}{}", coin.uri, coin.address);
    div()
        .flex()
        .mt(px(10.))
        .p(px(10.))
        .rounded(px(16.))
        .border_1()
        .border_color(c(coin.color))
        .bg(ca(r, g, b, 0x40 as f32 / 255.))
        .child(img(format!("icons/{}.svg", coin.coin.to_lowercase())).h(px(50.)).w(px(43.5)).my_auto().mr(px(10.)).flex_shrink_0())
        .child(
            div()
                .my(px(18.675))
                .text_size(px(22.5))
                .line_height(px(31.5))
                .font_weight(FontWeight::BOLD)
                .child(coin.coin),
        )
        .child(
            div()
                .id(SharedString::from(format!("{id}-pay")))
                .ml_auto()
                .flex()
                .items_center()
                .px(px(18.))
                .border_1()
                .border_color(gpui::transparent_black())
                .rounded(px(8.))
                .bg(ca(0, 0, 0, 0.4))
                .text_color(c(0xffffff))
                .text_size(px(15.))
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
                .active(|s| s.opacity(0.75))
                .on_click(move |_, w, cx| on_pay(uri.clone(), w, cx))
                .child("Pay"),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::detect;

    #[test]
    fn finds_addresses_outside_urls() {
        assert_eq!(detect("Pay me here: DLabsktzGMnsK5K9uRTMCF6NoYNY6ET4Bb").unwrap().address, "DLabsktzGMnsK5K9uRTMCF6NoYNY6ET4Bb");
        assert!(detect("see https://x.io/DLabsktzGMnsK5K9uRTMCF6NoYNY6ET4Bb").is_none());
        // 0, O, I and l aren't Base58.
        assert!(detect("DLabsktzGMnsK5K9uRTMCF6NoYNY6ET40O").is_none());
        let shielded = format!("ps1{}", "q".repeat(60));
        assert_eq!(detect(&format!("PS1{}", "Q".repeat(60))).map(|c| c.coin), Some("PIVX"));
        assert!(detect(&shielded).is_some());
    }
}
