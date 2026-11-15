//! `CryptoAddress.svelte` over `payments.js`: the first coin address a message holds, as a
//! card in the coin's colours with a Pay button opening its payment URI.

use gpui::prelude::*;
use gpui::{AnyElement, FontWeight, SharedString, div, px};
use crate::pic::img;

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
    find: fn(&str) -> Option<(usize, usize)>,
}

fn ascii_word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `\b` as JS reads it: only ASCII letters, digits and `_` are word chars.
fn ascii_boundary(t: &str, at: usize) -> bool {
    ascii_word(crate::scan::prev(t, at)) != ascii_word(crate::scan::next(t, at))
}

/// `\b{head}[class]{min,max}\b`, leftmost, longest first.
fn first_run(t: &str, head: impl Fn(&str, usize) -> Option<usize>, min: usize, max: usize, class: impl Fn(char) -> bool) -> Option<(usize, usize)> {
    t.char_indices().filter(|&(i, _)| ascii_boundary(t, i)).find_map(|(i, _)| {
        let from = head(t, i)?;
        let mut ends = Vec::with_capacity(max);
        let mut e = from;
        for c in t[from..].chars().take(max) {
            if !class(c) {
                break;
            }
            e += c.len_utf8();
            ends.push(e);
        }
        ends.iter().skip(min.saturating_sub(1)).rev().find(|&&e| ascii_boundary(t, e)).map(|&e| (i, e))
    })
}

/// Highest priority first.
const KINDS: [Kind; 2] = [
    Kind {
        coin: "PIVX",
        color: 0x9530f4,
        uri: "https://app.mypivxwallet.org/?pay=",
        transparent: false,
        // `(?i:ps1[a-z0-9]{50,120})`
        find: |t| first_run(t, |t, i| crate::scan::lit_i(t, i, "ps1"), 50, 120, |c| crate::scan::fold_letter(c) || c.is_ascii_digit()),
    },
    Kind {
        coin: "PIVX",
        color: 0x9530f4,
        uri: "https://app.mypivxwallet.org/?pay=",
        transparent: true,
        // `D[a-zA-Z0-9]{33,35}`
        find: |t| first_run(t, |t, i| crate::scan::lit(t, i, "D"), 33, 35, |c| c.is_ascii_alphanumeric()),
    },
];

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
        let Some((start, end)) = (k.find)(text) else { continue };
        let found = &text[start..end];
        let before = text[..start].chars().next_back();
        if matches!(before, Some('/' | '.' | ':' | '=' | '?')) {
            continue;
        }
        if matches!(text[end..].chars().next(), Some('/' | '.')) {
            continue;
        }
        if k.transparent && !base58(found) {
            continue;
        }
        return Some(Coin { coin: k.coin, color: k.color, uri: k.uri, address: found.to_string() });
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
    fn finders_agree_with_the_patterns_they_replace() {
        let shielded = regex::Regex::new(r"(?-u:\b)(?i:ps1[a-z0-9]{50,120})(?-u:\b)").unwrap();
        let transparent = regex::Regex::new(r"(?-u:\b)D[a-zA-Z0-9]{33,35}(?-u:\b)").unwrap();
        const PARTS: &[&str] = &["ps1", "PS1", "pſ1", "D", "abc123", "qwertyuiop", "Z", "\u{212A}", "ſ", "é", " ", "/", "_", "x9"];
        let mut seed = 0xbadc_0ffeu64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..30_000 {
            let s: String = (0..1 + next() % 30).map(|_| PARTS[(next() % PARTS.len() as u64) as usize]).collect();
            let m = |r: &regex::Regex| r.find(&s).map(|m| (m.start(), m.end()));
            assert_eq!((super::KINDS[0].find)(&s), m(&shielded), "{s:?}");
            assert_eq!((super::KINDS[1].find)(&s), m(&transparent), "{s:?}");
        }
    }

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
