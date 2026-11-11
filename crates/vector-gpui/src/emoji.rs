//! Twemoji: the Tauri client's own SVG set, so emoji draw identically on every platform.

use std::collections::HashSet;
use std::sync::LazyLock;

use gpui::SharedString;
use unicode_segmentation::UnicodeSegmentation;

static NAMES: LazyLock<HashSet<String>> =
    LazyLock::new(|| crate::assets::twemoji_names().map(|n| n.trim_end_matches(".svg").to_string()).collect());

/// The file twemoji names a grapheme by: its code points in hex, with U+FE0F dropped unless
/// the sequence is joined (twemoji's `grabTheRightIcon`).
fn code(g: &str) -> String {
    let joined = g.contains('\u{200d}');
    g.chars()
        .filter(|&ch| joined || ch != '\u{fe0f}')
        .map(|ch| format!("{:x}", ch as u32))
        .collect::<Vec<_>>()
        .join("-")
}

/// The twemoji asset for `g`, if it is an emoji twemoji draws.
pub fn twemoji(g: &str) -> Option<SharedString> {
    let mut chars = g.chars();
    let first = chars.next()?;
    // Text-default symbols (©, digits, #) are emoji only when asked to be.
    if (first as u32) < 0x2000 && !g.contains('\u{fe0f}') && !g.contains('\u{20e3}') {
        return None;
    }
    let name = code(g);
    if NAMES.contains(&name) {
        return Some(format!("twemoji/{name}.svg").into());
    }
    // A sequence twemoji lacks still draws its base.
    let base = code(&first.to_string());
    (chars.next().is_some() && NAMES.contains(&base)).then(|| format!("twemoji/{base}.svg").into())
}

/// Emoji graphemes in `text`, with whitespace ignored; `None` once anything else appears.
pub fn only_emoji_count(text: &str) -> Option<usize> {
    let mut n = 0;
    for g in text.graphemes(true) {
        if g.chars().all(char::is_whitespace) {
            continue;
        }
        twemoji(g)?;
        n += 1;
    }
    Some(n)
}
