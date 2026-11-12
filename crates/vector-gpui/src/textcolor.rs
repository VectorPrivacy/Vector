//! Coloured text in a received message (`text-color.js`): each span's edges are marked with
//! private-use sentinels that markdown carries through as text, then the glyphs between
//! them are painted. Edges move off anything a sentinel would break, rule for rule as the
//! Tauri client places them, so both clients colour the same glyphs.

use std::sync::LazyLock;

use gpui::Hsla;
use regex::Regex;
use vector_core::text_color::{ColorSpan, Effect};

const OPEN: u32 = 0xF0000;
const SHUT: u32 = 0xF0100;
const MAX_SPANS: usize = 64;
/// A long run shares each colour across neighbouring glyphs.
const STEPS: usize = 128;

pub enum Edge {
    Open(usize),
    Shut(usize),
}

pub fn edge(c: char) -> Option<Edge> {
    let v = c as u32;
    if (OPEN..OPEN + 0x100).contains(&v) {
        Some(Edge::Open((v - OPEN) as usize))
    } else if (SHUT..SHUT + 0x100).contains(&v) {
        Some(Edge::Shut((v - SHUT) as usize))
    } else {
        None
    }
}

pub fn is_sentinel(c: char) -> bool {
    edge(c).is_some()
}

/// Runs a span edge must never split: links, nostr references, shortcodes, entities,
/// inline code and escape codes.
static ATOMIC: LazyLock<Regex> = LazyLock::new(|| {
    regex::RegexBuilder::new(
        r"!?\[[^\[\]\n]{0,512}\]\([^()\n]{0,2048}\)|<[^<>\s]{1,2048}>|(?:https?://|www\.)[^\s<]+|(?:nostr:|@)?(?:npub1|nprofile1|naddr1|nevent1|note1)[02-9ac-hj-np-z]+|:[a-zA-Z0-9_~-]+:|&#?[a-zA-Z0-9]+;|`+[^`\n]*`+|\x1b\[[\x20-\x3f]*[\x40-\x7e]?",
    )
    .size_limit(64 << 20)
    .build()
    .expect("atomic runs")
});
static PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[ \t]{0,3}(?:(?:#{1,6}|-#|>|[-*]|\d{1,9}\.)[ \t]+|>[ \t]?)*").expect("block prefix")
});
static PUNCT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\p{P}\p{S}]$").expect("punct"));
static HTML_TAIL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[A-Za-z/!?][^<>]*$").expect("html"));

fn punct(c: Option<char>) -> bool {
    c.is_some_and(|c| PUNCT.is_match(c.encode_utf8(&mut [0; 4])))
}

/// `^[ \t]{0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$`
fn is_rule(line: &[char]) -> bool {
    let lead = line.iter().take(3).take_while(|c| matches!(c, ' ' | '\t')).count();
    let rest = &line[lead..];
    let Some(&c) = rest.first().filter(|c| matches!(c, '-' | '*' | '_')) else { return false };
    rest.iter().all(|&x| x == c || x == ' ' || x == '\t') && rest.iter().filter(|&&x| x == c).count() >= 3
}

/// `^[ \t]{0,3}\|(?!\|)`
fn is_table_row(line: &[char]) -> bool {
    let lead = line.iter().take(3).take_while(|c| matches!(c, ' ' | '\t')).count();
    line.get(lead) == Some(&'|') && line.get(lead + 1) != Some(&'|')
}

/// Indented code blocks, as char ranges: runs of lines four columns in that open after a
/// blank line, a heading or the start.
fn indented_code(t: &[char]) -> Vec<(usize, usize)> {
    static MARKER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(?: {0,3}(?:> ?|(?:[-*+]|\d{1,9}[.)])(?: |\t)))+").expect("marker"));
    static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ {0,3}#{1,6}(?:[ \t]|$)").expect("heading"));
    let mut out = Vec::new();
    let mut open: Option<usize> = None;
    let mut opens = true;
    let mut at = 0;
    loop {
        let nl = t[at..].iter().position(|&c| c == '\n').map(|i| at + i);
        let line: String = t[at..nl.unwrap_or(t.len())].iter().collect();
        // A marker followed by a tab keeps the tab: it is the code's indent.
        let marker_len = MARKER.find(&line).map_or(0, |m| {
            let s = m.as_str();
            if s.ends_with('\t') { s.len() - 1 } else { s.len() }
        });
        let body = &line[marker_len..];
        let blank = body.trim().is_empty();
        let deep = !blank && (body.starts_with("    ") || {
            let sp = body.bytes().take(3).take_while(|&b| b == b' ').count();
            body[sp..].starts_with('\t')
        });
        if open.is_none() && deep && (opens || marker_len > 0) {
            open = Some(at);
        } else if let Some(o) = open.filter(|_| !deep && !blank) {
            out.push((o, at));
            open = None;
        }
        opens = blank || (open.is_none() && HEADING.is_match(&line));
        match nl {
            Some(nl) => at = nl + 1,
            None => break,
        }
        if at > t.len() {
            break;
        }
    }
    if let Some(o) = open {
        out.push((o, t.len()));
    }
    out
}

/// `text` with a sentinel at each colour span's edge, and the char offsets they went in at;
/// `None` when there is nothing to mark or the text already holds sentinels.
pub fn mark(text: &str, spans: &[ColorSpan], times: &[(usize, usize)]) -> Option<(String, Vec<usize>)> {
    if spans.is_empty() || text.chars().any(is_sentinel) {
        return None;
    }
    let t: Vec<char> = text.chars().collect();
    let n = t.len();
    let byte_to_char = {
        let mut map = vec![0usize; text.len() + 1];
        let mut ci = 0;
        for (bi, c) in text.char_indices() {
            for k in 0..c.len_utf8() {
                map[bi + k] = ci;
            }
            ci += 1;
        }
        map[text.len()] = ci;
        map
    };
    let ch = |i: isize| -> Option<char> { (i >= 0).then(|| t.get(i as usize).copied()).flatten() };
    let fences: Vec<(usize, usize)> = vector_core::text_color::code_ranges(text)
        .into_iter()
        .filter(|&(s, _)| {
            let rest = &text[s..];
            let sp = rest.bytes().take(3).take_while(|&b| b == b' ').count();
            rest[sp..].starts_with("```")
        })
        .map(|(s, e)| (byte_to_char[s], byte_to_char[e]))
        .chain(indented_code(&t))
        .collect();
    let atomic: Vec<(usize, usize)> = ATOMIC
        .find_iter(text)
        .map(|m| (byte_to_char[m.start()], byte_to_char[m.end()]))
        .chain(times.iter().copied().filter(|&(_, e)| e <= n))
        .collect();
    let last_nl = |from: isize| -> isize {
        let from = from.max(0) as usize;
        (0..=from.min(n.saturating_sub(1))).rev().find(|&i| t.get(i) == Some(&'\n')).map_or(-1, |i| i as isize)
    };
    let line_of = |at: usize| -> (usize, usize) {
        let s = if n == 0 { 0 } else { (last_nl(at as isize - 1) + 1) as usize };
        let e = t[at.min(n)..].iter().position(|&c| c == '\n').map_or(n, |i| at + i);
        (s, e)
    };
    let in_atom = |at: usize| atomic.iter().any(|&(s, e)| at >= s && at <= e);
    let blank = |c: Option<char>| matches!(c, Some(' ' | '\t'));
    let ws = |c: Option<char>| c.is_some_and(char::is_whitespace);
    let word = |c: Option<char>| c.is_some_and(|c| !c.is_whitespace()) && !punct(c);
    let emphasis = |c: Option<char>| matches!(c, Some('*' | '_'));
    let free = |c: Option<char>| c.is_none() || ws(c);
    let beyond_run = |mut i: isize, step: isize| {
        let c = ch(i);
        while ch(i) == c && c.is_some() {
            i += step;
        }
        ch(i)
    };
    let last_index_of = |pat: &[char], from: isize| -> isize {
        if from < 0 {
            return -1;
        }
        let from = (from as usize).min(n.saturating_sub(pat.len()));
        (0..=from).rev().find(|&i| t[i..].starts_with(pat)).map_or(-1, |i| i as isize)
    };
    let syntax = |at: isize| -> bool {
        let Some(c) = ch(at) else { return false };
        if !"*_~|\\`[]()<>!#".contains(c) {
            return false;
        }
        match c {
            '!' => ch(at + 1) == Some('[') || ch(at - 1) == Some('<'),
            '(' => ch(at - 1) == Some(']'),
            ')' => last_index_of(&[']', '('], at) > last_index_of(&['\n'], at),
            '#' => {
                (at == 0 || ch(at - 1).is_some_and(|p| p.is_whitespace() || p == '#' || p == '-'))
                    && (at + 1 >= n as isize || ch(at + 1).is_some_and(|x| x.is_whitespace() || x == '#'))
            }
            '<' => ch(at + 1).is_some_and(|x| x.is_ascii_alphabetic() || "/!?".contains(x)),
            '>' => {
                let s = (last_nl(at - 1) + 1) as usize;
                let before: String = t[s..at as usize].iter().collect();
                before.chars().all(|x| matches!(x, ' ' | '\t' | '>')) || HTML_TAIL.is_match(&before)
            }
            _ => true,
        }
    };
    let wedged = |at: usize| -> bool {
        let at = at as isize;
        let (prev, next) = (ch(at - 1), ch(at));
        if prev == Some('\\') || (prev == Some('<') && syntax(at - 1)) || (prev == Some('!') && ch(at - 2) == Some('<')) {
            return true;
        }
        if syntax(at - 1) && !word(next) {
            return true;
        }
        syntax(at) && !word(prev) && !(emphasis(next) && punct(prev) && free(beyond_run(at, 1)))
    };
    // (zone start, zone end, whole line)
    let zone_of = |at: usize| -> (usize, usize, bool) {
        let (s, e) = line_of(at);
        let line = &t[s..e];
        if is_rule(line) || is_table_row(line) {
            return (s, e, true);
        }
        let head: String = line.iter().take(64).collect();
        let len = PREFIX.find(&head).map_or(0, |m| m.as_str().chars().count());
        (s, s + len, false)
    };
    let rest_blank = |mut at: usize| -> Option<usize> {
        while blank(t.get(at).copied()) {
            at += 1;
        }
        (at == n || t[at] == '\n').then_some(at)
    };
    let start_rules = |mut at: usize| -> usize {
        for &(s, e) in &fences {
            if at >= s && at < e {
                at = e;
            }
        }
        if let Some(eol) = rest_blank(at) {
            at = (eol + 1).min(n);
        }
        loop {
            if at >= n {
                return n;
            }
            let (ls, le) = line_of(at);
            let line: String = t[ls..le].iter().collect();
            if !line.trim().is_empty() && !zone_of(at).2 {
                break;
            }
            at = (le + 1).min(n);
        }
        let (zs, ze, _) = zone_of(at);
        if at >= zs && at < ze {
            at = ze;
        }
        if in_atom(at) {
            while at < n && !t[at].is_whitespace() {
                at += 1;
            }
            while blank(t.get(at).copied()) {
                at += 1;
            }
        }
        while at < n && wedged(at) {
            at += 1;
        }
        at
    };
    let end_rules = |mut at: usize| -> usize {
        for &(s, e) in &fences {
            if at > s && at <= e {
                at = s;
            }
        }
        loop {
            let (zs, ze, whole) = zone_of(at);
            if at == 0 || !(whole || at == zs || at < ze) {
                break;
            }
            at = zs.saturating_sub(1);
        }
        if in_atom(at) {
            while at > 0 && !t[at - 1].is_whitespace() {
                at -= 1;
            }
            while at > 0 && blank(Some(t[at - 1])) {
                at -= 1;
            }
        }
        if rest_blank(at).is_some() {
            while at > 0 && blank(Some(t[at - 1])) {
                at -= 1;
            }
        }
        while at > 0 && wedged(at) {
            at -= 1;
        }
        at
    };
    let settle = |mut at: usize, rules: &dyn Fn(usize) -> usize| {
        for _ in 0..=n + 1 {
            let next = rules(at);
            if next == at {
                break;
            }
            at = next;
        }
        at
    };
    // (offset, 0 for an end and 1 for a start, sentinel)
    let mut inserts: Vec<(usize, u8, char)> = Vec::new();
    for (i, span) in spans.iter().take(MAX_SPANS).enumerate() {
        let (from, to) = (span.from as usize, span.to as usize);
        if from >= to || to > n {
            continue;
        }
        let (a, b) = (settle(from, &start_rules), settle(to, &end_rules));
        if a >= b {
            continue;
        }
        inserts.push((a, 1, char::from_u32(OPEN + i as u32).unwrap_or(' ')));
        inserts.push((b, 0, char::from_u32(SHUT + i as u32).unwrap_or(' ')));
    }
    if inserts.is_empty() {
        return None;
    }
    inserts.sort_by_key(|&(at, order, _)| (at, order));
    let mut out = String::with_capacity(text.len() + inserts.len() * 4);
    let mut k = 0;
    for i in 0..=n {
        while k < inserts.len() && inserts[k].0 == i {
            out.push(inserts[k].2);
            k += 1;
        }
        if i < n {
            out.push(t[i]);
        }
    }
    Some((out, inserts.iter().map(|x| x.0).collect()))
}

fn rgb(hex: &str) -> [f64; 3] {
    let h = hex.trim_start_matches('#');
    let p = |i: usize| h.get(i..i + 2).and_then(|s| u8::from_str_radix(s, 16).ok()).unwrap_or(255) as f64;
    [p(0), p(2), p(4)]
}

fn luminance(c: [f64; 3]) -> f64 {
    let f = |v: f64| {
        let v = v / 255.;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2])
}

/// Lifted toward white until it reads against the chat's near-black.
fn readable(c: [f64; 3]) -> Hsla {
    let bg = luminance([16., 16., 16.]);
    let mut out = c;
    let mut t = 0.0f64;
    while t <= 1.0001 {
        out = c.map(|v| (v + (255. - v) * t).round());
        if (luminance(out) + 0.05) / (bg + 0.05) >= 4.5 {
            break;
        }
        t += 0.05;
    }
    gpui::Rgba { r: (out[0] / 255.) as f32, g: (out[1] / 255.) as f32, b: (out[2] / 255.) as f32, a: 1. }.into()
}

fn hsl(h: f64, s: f64, l: f64) -> [f64; 3] {
    let a = s * l.min(1. - l);
    let f = |n: f64| {
        let k = (n + h / 30.) % 12.;
        ((l - a * (-1f64).max((k - 3.).min(9. - k).min(1.))) * 255.).round()
    };
    [f(0.), f(8.), f(4.)]
}

fn color_at(span: &ColorSpan, i: usize, n: usize) -> Hsla {
    match span.effect {
        Effect::Solid => readable(rgb(span.colors.first().map_or("#ffffff", |s| s.as_str()))),
        Effect::Rainbow => readable(hsl(360. * i as f64 / n.max(1) as f64, 1., 0.6)),
        Effect::Gradient => {
            let stops: Vec<[f64; 3]> = span.colors.iter().map(|c| rgb(c)).collect();
            if stops.len() < 2 {
                return readable(stops.first().copied().unwrap_or([255.; 3]));
            }
            let t = if n > 1 { i as f64 / (n - 1) as f64 } else { 0. };
            let seg = ((t * (stops.len() - 1) as f64).floor() as usize).min(stops.len() - 2);
            let local = t * (stops.len() - 1) as f64 - seg as f64;
            let (a, b) = (stops[seg], stops[seg + 1]);
            readable([0, 1, 2].map(|k| (a[k] + (b[k] - a[k]) * local).round()))
        }
    }
}

/// Glyph `i` of `n` → its colour.
pub struct Palette {
    colors: Vec<Hsla>,
    n: usize,
}

impl Palette {
    pub fn new(span: &ColorSpan, n: usize) -> Self {
        if span.effect == Effect::Solid {
            return Self { colors: vec![color_at(span, 0, 1)], n: 1 };
        }
        let steps = n.min(STEPS).max(1);
        let colors = (0..steps)
            .map(|b| color_at(span, if n > steps { b * n / steps } else { b }, n))
            .collect();
        Self { colors, n }
    }

    pub fn at(&self, i: usize) -> Hsla {
        let steps = self.colors.len();
        let k = if self.n > steps { i * steps / self.n.max(1) } else { i };
        self.colors[k.min(steps - 1)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(from: u32, to: u32) -> ColorSpan {
        ColorSpan { from, to, effect: Effect::Rainbow, colors: Vec::new() }
    }

    fn shown(text: &str, s: &[ColorSpan]) -> String {
        mark(text, s, &[])
            .map(|(m, _)| {
                m.chars()
                    .map(|c| match edge(c) {
                        Some(Edge::Open(_)) => '{',
                        Some(Edge::Shut(_)) => '}',
                        None => c,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn edges_keep_off_syntax_links_and_prefixes() {
        assert_eq!(shown("hello world", &[span(0, 5)]), "{hello} world");
        assert_eq!(shown("# Title here", &[span(0, 12)]), "# {Title here}");
        assert_eq!(shown("see https://a.io/x now", &[span(4, 10)]), "");
        assert_eq!(shown("**bold** x", &[span(0, 8)]), "**{bold}** x");
        assert_eq!(shown("a\n\n---\n\nb", &[span(0, 9)]), "{a\n\n---\n\nb}");
    }

    /// Differential check against `tcMarkSpans`: `TC_CASES=<json file> cargo test dump -- --ignored`.
    #[test]
    #[ignore]
    fn dump() {
        let cases: Vec<(String, Vec<(u32, u32)>)> =
            serde_json::from_str(&std::fs::read_to_string(std::env::var("TC_CASES").unwrap()).unwrap()).unwrap();
        let mut out = String::new();
        for (t, s) in cases {
            let spans: Vec<ColorSpan> = s.into_iter().map(|(a, b)| span(a, b)).collect();
            out += &serde_json::to_string(&shown(&t, &spans)).unwrap();
            out.push('\n');
        }
        std::fs::write(std::env::var("TC_OUT").unwrap(), out).unwrap();
    }

    #[test]
    fn palettes_floor_readability() {
        let dark = ColorSpan { from: 0, to: 1, effect: Effect::Solid, colors: vec!["#000000".into()] };
        let c = Palette::new(&dark, 1).at(0);
        assert!(c.l > 0.4);
        let r = Palette::new(&span(0, 300), 300);
        assert_eq!(r.colors.len(), STEPS);
    }
}
