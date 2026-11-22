//! Twemoji: the Tauri client's own SVG set, so emoji draw identically on every platform.

use std::collections::HashSet;
use std::sync::LazyLock;

use gpui::SharedString;
use unicode_segmentation::UnicodeSegmentation;

static NAMES: LazyLock<HashSet<String>> = LazyLock::new(|| {
    crate::assets::twemoji_names()
        .map(|n| n.trim_end_matches(".svg").to_string())
        .collect()
});

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
    if is_text_symbol(first) {
        return None;
    }
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

/// ©, ® and ™, drawn as plain symbols in the text's own font and colour: Twemoji draws them
/// dark, which a dark theme loses.
fn is_text_symbol(ch: char) -> bool {
    matches!(ch, '\u{a9}' | '\u{ae}' | '\u{2122}')
}

/// `text` with ©, ® and ™ out of their emoji form (`plainTextSymbols`).
pub fn plain_text_symbols(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\u{fe0f}') {
        return text.into();
    }
    let mut out = String::with_capacity(text.len());
    let mut after_symbol = false;
    for ch in text.chars() {
        if !(after_symbol && ch == '\u{fe0f}') {
            out.push(ch);
        }
        after_symbol = is_text_symbol(ch);
    }
    if out.len() == text.len() { text.into() } else { out.into() }
}

/// The symbol a completed `:tm:`, `:copyright:` or `:registered:` turns into where it stands.
pub fn text_symbol_code(name: &str) -> Option<&'static str> {
    match name {
        "tm" => Some("\u{2122}"),
        "copyright" => Some("\u{a9}"),
        "registered" => Some("\u{ae}"),
        _ => None,
    }
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

/// One stock emoji, as the Tauri dataset lists it.
pub struct Emoji {
    pub emoji: &'static str,
    /// Lowercase search words.
    pub name: &'static str,
    pub display: &'static str,
    pub shortcode: &'static str,
    /// Unicode popularity rank; 99 unranked.
    pub freq: u8,
}

include!(concat!(env!("OUT_DIR"), "/emoji_data.rs"));

/// `searchEmojis`'s per-pick score reduction.
pub const USAGE_WEIGHT: f64 = 0.5;

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != *cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// `searchEmojis`: shortcode, display and word matches with typo tolerance, then personal use;
/// each hit carries its score so custom emoji can merge in.
pub fn search_scored(query: &str, used: impl Fn(&str) -> f64) -> Vec<(&'static Emoji, f64)> {
    let q = query.to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let qlen = q.chars().count();
    let qchars: HashSet<char> = q.chars().collect();
    struct Hit {
        e: &'static Emoji,
        score: f64,
        word: &'static str,
        index: usize,
    }
    let mut hits: Vec<Hit> = Vec::new();
    let local = vector_i18n::emoji_names();
    // A script without spaces (中文, 日本語) has no word starts: any part of a word may match.
    let anywhere = qlen >= 2 || !q.is_ascii();
    for e in EMOJIS {
        let mut best: Option<(f64, &'static str, usize)> = None;
        let mut consider = |score: f64, word: &'static str, i: usize| {
            if best.is_none_or(|b| score < b.0) {
                best = Some((score, word, i));
            }
        };
        // The reader's language first: its name, then its search words.
        for (i, w) in local.as_ref().map_or(&[][..], |n| n.words(e.emoji)).iter().enumerate() {
            let pos = i as f64 * 0.02;
            if **w == *q {
                consider(if i == 0 { -1.0 } else { -0.8 + pos }, e.name, i);
            } else if w.starts_with(q.as_str()) {
                consider(0.1 + pos, e.name, i);
            } else if anywhere && w.contains(q.as_str()) {
                consider(0.4 + pos, e.name, i);
            }
        }
        let sc = e.shortcode;
        if !sc.is_empty() {
            if sc == q || sc.replace('_', " ") == q {
                consider(-2.0, sc, 0);
            } else if sc.starts_with(&format!("{q}_")) || sc.starts_with(&format!("{q} ")) {
                consider(-1.5, sc, 0);
            }
        }
        let display = e.display;
        let dl = display.to_lowercase();
        if dl == q {
            consider(-1.0, display, 0);
        } else if dl.starts_with(&format!("{q} ")) {
            consider(-0.5, display, 0);
        }
        for (i, word) in e.name.split(' ').enumerate() {
            let pos = i as f64 * 0.02;
            if word == q {
                consider(pos, word, i);
                continue;
            }
            let wlen = word.chars().count();
            if word.starts_with(&q) && qlen >= 2 {
                consider(0.1 + pos, word, i);
            }
            if qlen >= 3 {
                let diff = wlen.abs_diff(qlen);
                if diff <= 1 && levenshtein(word, &q) == 1 {
                    let common = qchars.iter().filter(|c| word.contains(**c)).count();
                    if common + 1 >= qlen {
                        let mut score = 0.5;
                        if wlen > qlen && word.starts_with(&q) {
                            score = 0.3;
                        } else if wlen > qlen
                            && word.chars().next() == q.chars().next()
                            && word.chars().last() == q.chars().last()
                        {
                            score = 0.35;
                        }
                        consider(score + pos, word, i);
                    }
                }
                if wlen > qlen && diff <= 3 {
                    let start: String = word.chars().take(qlen).collect();
                    if levenshtein(&start, &q) == 1 {
                        consider(0.6 + pos, word, i);
                    }
                }
            }
            if qlen >= 5 && wlen.abs_diff(qlen) <= 1 && levenshtein(word, &q) == 2 {
                consider(0.8 + pos, word, i);
            }
        }
        if let Some((score, word, index)) = best {
            hits.push(Hit {
                e,
                score: score - used(e.emoji) * USAGE_WEIGHT,
                word,
                index,
            });
        }
    }
    if hits.is_empty() {
        for e in EMOJIS.iter().filter(|e| e.name.contains(&q)) {
            hits.push(Hit {
                e,
                score: 1.0 - used(e.emoji) * USAGE_WEIGHT,
                word: e.name,
                index: 99,
            });
        }
    }
    // Scores a hundredth apart tie, as `searchEmojis` means them to; by bucket, since "within a
    // hundredth" is no order (a ties b, b ties c, a beats c) and the sort refuses one.
    let bucket = |s: f64| (s * 100.).round() as i64;
    hits.sort_by(|a, b| {
        bucket(a.score)
            .cmp(&bucket(b.score))
            .then(a.index.cmp(&b.index))
            .then(a.e.freq.cmp(&b.e.freq))
            .then(a.word.len().cmp(&b.word.len()))
            .then(a.e.name.len().cmp(&b.e.name.len()))
            .then(a.word.cmp(b.word))
    });
    hits.into_iter().map(|h| (h.e, h.score)).collect()
}

/// What the reader calls `e`: its name in the active language, else the dataset's.
pub fn name_of(e: &Emoji) -> gpui::SharedString {
    vector_i18n::emoji_names()
        .and_then(|n| n.name(e.emoji).map(|s| gpui::SharedString::from(s.to_string())))
        .unwrap_or(gpui::SharedString::new_static(e.name))
}

/// Whether `q` (lowercased) is part of `e`'s name or words, in English or the reader's language.
pub fn named(e: &Emoji, q: &str) -> bool {
    e.name.to_lowercase().contains(q) || vector_i18n::emoji_names().is_some_and(|n| n.words(e.emoji).iter().any(|w| w.contains(q)))
}

/// The 24 most used, the dataset's order breaking ties (`getMostUsedEmojis`).
pub fn by_char(ch: &str) -> Option<&'static Emoji> {
    EMOJIS.iter().find(|e| e.emoji == ch)
}

pub fn most_used(used: impl Fn(&str) -> f64) -> Vec<&'static Emoji> {
    let mut all: Vec<&'static Emoji> = EMOJIS.iter().collect();
    all.sort_by(|a, b| used(b.emoji).total_cmp(&used(a.emoji)));
    all.truncate(24);
    all
}

/// The distinct emoji in a message, for its usage record.
pub fn in_text(text: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for g in text.graphemes(true) {
        if twemoji(g).is_some() && !seen.iter().any(|s: &String| s == g) {
            seen.push(g.to_string());
        }
    }
    seen
}

pub struct Category {
    pub name: &'static str,
    pub items: &'static [Kaomoji],
}

pub struct Kaomoji {
    pub face: &'static str,
    pub name: &'static str,
    pub tags: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/kaomoji_data.rs"));

/// Each face once, as its first category lists it.
fn kaomoji_faces() -> impl Iterator<Item = (&'static Kaomoji, &'static str)> {
    let mut seen = HashSet::new();
    KAOMOJI
        .iter()
        .flat_map(|c| c.items.iter().map(move |k| (k, c.name)))
        .filter(move |(k, _)| seen.insert(k.face))
}

/// Every character the kaomoji draw beyond ASCII, each once, in runs short enough that shaping
/// one never holds the text system for long.
pub fn kaomoji_glyphs() -> Vec<String> {
    let mut seen = HashSet::new();
    let chars: Vec<char> = KAOMOJI
        .iter()
        .flat_map(|c| c.items.iter())
        .flat_map(|k| k.face.chars())
        .filter(|ch| !ch.is_ascii() && seen.insert(*ch))
        .collect();
    chars.chunks(24).map(|c| c.iter().collect()).collect()
}

/// A kaomoji by its face.
pub fn kaomoji(face: &str) -> Option<&'static Kaomoji> {
    KAOMOJI
        .iter()
        .flat_map(|c| c.items.iter())
        .find(|k| k.face == face)
}

/// Faces matching `query`, ranked by name, tags, category, then the face itself (`KaomojiGrid`).
pub fn search_kaomoji(query: &str) -> Vec<&'static Kaomoji> {
    let q = query.trim().to_lowercase();
    let mut hits: Vec<(u8, &Kaomoji)> = kaomoji_faces()
        .filter_map(|(k, cat)| {
            let name = k.name.to_lowercase();
            let rank = if name.starts_with(&q) {
                0
            } else if k.tags.split(' ').any(|t| t.starts_with(&q)) {
                1
            } else if name.contains(&q) {
                2
            } else if cat.to_lowercase().starts_with(&q) {
                3
            } else if k.face.contains(&q) {
                4
            } else {
                return None;
            };
            Some((rank, k))
        })
        .collect();
    hits.sort_by_key(|(r, _)| *r);
    hits.into_iter().map(|(_, k)| k).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_symbols_lose_their_emoji_form_and_their_art() {
        assert_eq!(plain_text_symbols("Vector\u{2122}\u{fe0f} \u{a9}\u{fe0f}\u{ae}"), "Vector\u{2122} \u{a9}\u{ae}");
        assert!(matches!(plain_text_symbols("\u{2764}\u{fe0f}"), std::borrow::Cow::Borrowed(_)));
        for g in ["\u{2122}", "\u{2122}\u{fe0f}", "\u{a9}\u{fe0f}", "\u{ae}\u{fe0f}"] {
            assert_eq!(twemoji(g), None);
        }
        assert!(twemoji("\u{2764}\u{fe0f}").is_some());
    }

    #[test]
    fn scores_a_hair_apart_sort_without_a_panic() {
        // Usage a few thousandths apart chains near-ties across every hit.
        let used = |e: &str| e.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) as f64 % 40. * 0.0025;
        for q in ["face", "cat", "heart", "smile", "hand"] {
            let hits = search_scored(q, used);
            assert!(hits.len() > 20, "{q}: {}", hits.len());
            assert!(hits.windows(2).all(|w| (w[0].1 * 100.).round() <= (w[1].1 * 100.).round()), "{q}");
        }
    }
}
