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
    for e in EMOJIS {
        let mut best: Option<(f64, &'static str, usize)> = None;
        let mut consider = |score: f64, word: &'static str, i: usize| {
            if best.is_none_or(|b| score < b.0) {
                best = Some((score, word, i));
            }
        };
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
            if qlen >= 5 && levenshtein(word, &q) == 2 && wlen.abs_diff(qlen) <= 1 {
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
    hits.sort_by(|a, b| {
        if (a.score - b.score).abs() > 0.01 {
            return a.score.total_cmp(&b.score);
        }
        a.index
            .cmp(&b.index)
            .then(a.e.freq.cmp(&b.e.freq))
            .then(a.word.len().cmp(&b.word.len()))
            .then(a.e.name.len().cmp(&b.e.name.len()))
            .then(a.word.cmp(b.word))
    });
    hits.into_iter().map(|h| (h.e, h.score)).collect()
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
