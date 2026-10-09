//! Coloured text outside code blocks.
//!
//! The sender writes `<rainbow>…</rainbow>`, `<gradient #a #b>…</gradient>`,
//! `<color pink>…</color>` or Element's `<font color="#hex">…</font>`. On send the
//! markup leaves the content and each span rides a tag,
//! `["color", from, to, effect, colours…]`, with offsets counted in chars of the plain
//! content. Other clients read clean text; a Vector paints it, applying its own
//! readability floor, so the tag can't make text vanish into the background.
//!
//! A tag with anything this version doesn't understand is ignored whole: a later
//! version can add arguments without an older one drawing them wrong.

use serde::{Deserialize, Serialize};

/// The rumor tag name: `["color", from, to, effect, colours…]`.
pub const TAG: &str = "color";
const MAX_SPANS: usize = 64;
const MAX_STOPS: usize = 6;

/// How a span's glyphs are coloured: one colour, a hue sweep, or a blend through 2-6 stops.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    Solid,
    Rainbow,
    Gradient,
}

impl Effect {
    fn as_str(self) -> &'static str {
        match self {
            Effect::Solid => "solid",
            Effect::Rainbow => "rainbow",
            Effect::Gradient => "gradient",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "solid" => Some(Effect::Solid),
            "rainbow" => Some(Effect::Rainbow),
            "gradient" => Some(Effect::Gradient),
            _ => None,
        }
    }

    fn takes(self, stops: usize) -> bool {
        match self {
            Effect::Solid => stops == 1,
            Effect::Rainbow => stops == 0,
            Effect::Gradient => (2..=MAX_STOPS).contains(&stops),
        }
    }
}

/// One coloured run of the plain content, `from..to` in chars. A later span paints
/// over an earlier one where they overlap, so markup nests the way it reads.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ColorSpan {
    pub from: u32,
    pub to: u32,
    pub effect: Effect,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub colors: Vec<String>,
}

impl ColorSpan {
    /// The span as stored and sent: the tag name, then its fields.
    pub fn to_tag_parts(&self) -> Vec<String> {
        let mut parts = vec![TAG.to_string(), self.from.to_string(), self.to.to_string(), self.effect.as_str().to_string()];
        parts.extend(self.colors.iter().cloned());
        parts
    }

    fn from_parts(parts: &[&str], content_chars: u32) -> Option<Self> {
        if parts.len() < 4 || parts[0] != TAG {
            return None;
        }
        let from: u32 = parts[1].parse().ok()?;
        let to: u32 = parts[2].parse().ok()?;
        if from >= to || to > content_chars {
            return None;
        }
        let effect = Effect::parse(parts[3])?;
        let colors: Vec<String> = parts[4..].iter().map(|c| hex(c).filter(|h| h.as_str() == *c)).collect::<Option<_>>()?;
        effect.takes(colors.len()).then_some(ColorSpan { from, to, effect, colors })
    }
}

/// The spans carried by a rumor's tags, checked against its content.
pub fn from_tags<'a, I>(tags: I, content: &str) -> Vec<ColorSpan>
where
    I: IntoIterator<Item = &'a nostr_sdk::prelude::Tag>,
{
    let n = content.chars().count() as u32;
    tags.into_iter()
        .filter_map(|t| {
            let parts: Vec<&str> = t.as_slice().iter().map(|s| s.as_str()).collect();
            ColorSpan::from_parts(&parts, n)
        })
        .take(MAX_SPANS)
        .collect()
}

/// The spans in a stored event's tags, checked against its content.
pub fn from_stored(tags: &[Vec<String>], content: &str) -> Vec<ColorSpan> {
    let n = content.chars().count() as u32;
    tags.iter()
        .filter_map(|t| {
            let parts: Vec<&str> = t.iter().map(|s| s.as_str()).collect();
            ColorSpan::from_parts(&parts, n)
        })
        .take(MAX_SPANS)
        .collect()
}

/// The spans as rumor tags.
pub fn to_nostr_tags(spans: &[ColorSpan]) -> impl Iterator<Item = nostr_sdk::prelude::Tag> + '_ {
    spans.iter().map(|s| {
        let mut parts = s.to_tag_parts();
        parts.remove(0);
        nostr_sdk::prelude::Tag::custom(TAG, parts)
    })
}

fn ascii_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_ascii_whitespace())
}

/// A named colour or `#rgb` / `#rrggbb`, as lowercase `#rrggbb`.
pub fn hex(s: &str) -> Option<String> {
    let s = ascii_trim(s);
    if let Some(h) = s.strip_prefix('#') {
        if !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        return match h.len() {
            6 => Some(format!("#{}", h.to_ascii_lowercase())),
            3 => Some(format!("#{}", h.chars().flat_map(|c| [c, c]).collect::<String>().to_ascii_lowercase())),
            _ => None,
        };
    }
    names().get(s.to_ascii_lowercase().as_str()).cloned()
}

/// Every colour name a sender can type: the CSS names, the basic ones tuned for the
/// dark chat, and a few friendly extras. The JS side reads the same file's contents.
fn names() -> &'static std::collections::HashMap<String, String> {
    static NAMES: std::sync::OnceLock<std::collections::HashMap<String, String>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| serde_json::from_str(include_str!("text_color_names.json")).expect("colour names parse"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Name {
    Rainbow,
    Gradient,
    Color,
    Font,
}

impl Name {
    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "rainbow" => Some(Name::Rainbow),
            "gradient" => Some(Name::Gradient),
            "color" | "colour" => Some(Name::Color),
            "font" => Some(Name::Font),
            _ => None,
        }
    }
}

/// A tag's colour arguments, or None when they don't make one.
fn spec(name: Name, args: &str) -> Option<(Effect, Vec<String>)> {
    match name {
        Name::Rainbow => ascii_trim(args).is_empty().then(|| (Effect::Rainbow, Vec::new())),
        Name::Gradient => {
            let stops: Vec<String> = args.split_ascii_whitespace().map(hex).collect::<Option<_>>()?;
            Effect::Gradient.takes(stops.len()).then_some((Effect::Gradient, stops))
        }
        Name::Color => {
            let arg = ascii_trim(args).trim_start_matches('=');
            let arg = arg.trim_matches(|c| c == '"' || c == '\'');
            Some((Effect::Solid, vec![hex(arg)?]))
        }
        Name::Font => {
            let value = attr(args, "data-mx-color").or_else(|| attr(args, "color"))?;
            Some((Effect::Solid, vec![hex(value)?]))
        }
    }
}

/// An HTML-style attribute's value: `name="v"`, `name='v'` or `name=v`.
fn attr<'a>(args: &'a str, name: &str) -> Option<&'a str> {
    let lower = args.to_ascii_lowercase();
    let mut search = 0;
    while let Some(i) = lower[search..].find(name) {
        let at = search + i;
        search = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = args[search..].trim_start_matches(|c: char| c.is_ascii_whitespace());
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let rest = rest[1..].trim_start_matches(|c: char| c.is_ascii_whitespace());
        return match rest.chars().next()? {
            q @ ('"' | '\'') => rest[1..].find(q).map(|end| &rest[1..1 + end]),
            _ => rest.split_ascii_whitespace().next(),
        };
    }
    None
}

/// Byte ranges markup must not reach into: fenced blocks and inline code.
fn code_ranges(content: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut fence_start: Option<usize> = None;
    let mut line_start = 0;
    for line in content.split_inclusive('\n') {
        let end = line_start + line.len();
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let indent = trimmed.len() - trimmed.trim_start_matches(' ').len();
        let is_fence = indent <= 3 && trimmed[indent..].starts_with("```");
        match fence_start {
            Some(start) if is_fence => {
                out.push((start, end));
                fence_start = None;
            }
            Some(_) => {}
            None if is_fence => fence_start = Some(line_start),
            None => {
                let bytes = trimmed.as_bytes();
                let mut i = 0;
                while i < bytes.len() {
                    if bytes[i] != b'`' {
                        i += 1;
                        continue;
                    }
                    let run = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                    let mut j = i + run;
                    let mut closed = None;
                    while j < bytes.len() {
                        if bytes[j] == b'`' {
                            let r = bytes[j..].iter().take_while(|&&b| b == b'`').count();
                            if r == run {
                                closed = Some(j + r);
                                break;
                            }
                            j += r;
                        } else {
                            j += 1;
                        }
                    }
                    match closed {
                        Some(e) => {
                            out.push((line_start + i, line_start + e));
                            i = e;
                        }
                        None => i += run,
                    }
                }
            }
        }
        line_start = end;
    }
    if let Some(start) = fence_start {
        out.push((start, content.len()));
    }
    out
}

struct Markup {
    at: usize,
    end: usize,
    name: Name,
    open: Option<(Effect, Vec<String>)>,
}

/// Every well-formed colour tag outside code, in order.
fn scan(content: &str) -> Vec<Markup> {
    let code = code_ranges(content);
    let in_code = |i: usize| code.iter().any(|&(s, e)| i >= s && i < e);
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = content[i..].find('<') {
        let at = i + off;
        i = at + 1;
        if in_code(at) {
            continue;
        }
        let Some(close) = content[at + 1..].find(['>', '\n', '<']).map(|c| at + 1 + c) else { break };
        if content.as_bytes()[close] != b'>' {
            continue;
        }
        let inner = &content[at + 1..close];
        let (closing, body) = match inner.strip_prefix('/') {
            Some(b) => (true, b),
            None => (false, inner),
        };
        let name_end = body.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(body.len());
        let Some(name) = Name::parse(&body[..name_end]) else { continue };
        let args = &body[name_end..];
        if closing {
            if !ascii_trim(args).is_empty() {
                continue;
            }
            out.push(Markup { at, end: close + 1, name, open: None });
        } else {
            if !args.is_empty() && !args.starts_with([' ', '=']) {
                continue;
            }
            let Some(s) = spec(name, args) else { continue };
            out.push(Markup { at, end: close + 1, name, open: Some(s) });
        }
        i = close + 1;
    }
    out
}

/// Take the colour markup out of `content`. Returns the plain text and its spans;
/// a tag without its partner stays as typed, and markup around nothing but
/// whitespace stays too, so a send is never emptied by its own tags.
pub fn extract(content: &str) -> (String, Vec<ColorSpan>) {
    if !content.contains('<') {
        return (content.to_string(), Vec::new());
    }
    let marks = scan(content);
    // Pair opens with closes like brackets; an unmatched close is left as text.
    let mut stack: Vec<usize> = Vec::new();
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for (idx, m) in marks.iter().enumerate() {
        if m.open.is_some() {
            stack.push(idx);
        } else if let Some(pos) = stack.iter().rposition(|&o| marks[o].name == m.name) {
            if pos == stack.len() - 1 {
                pairs.push((stack.pop().unwrap(), idx));
            }
        }
    }
    if pairs.is_empty() {
        return (content.to_string(), Vec::new());
    }
    pairs.sort_by_key(|&(o, _)| marks[o].at);

    let mut cut: Vec<(usize, usize)> = pairs.iter().flat_map(|&(o, c)| [(marks[o].at, marks[o].end), (marks[c].at, marks[c].end)]).collect();
    cut.sort_unstable();
    let mut plain = String::with_capacity(content.len());
    // Char offset in `plain` at each cut's edges, keyed by byte position. Every paired
    // mark's `at` and `end` is a cut edge, so the lookups below always land.
    let mut char_at = std::collections::HashMap::new();
    let mut pos = 0;
    let mut chars = 0u32;
    for &(s, e) in &cut {
        let piece = &content[pos..s];
        plain.push_str(piece);
        chars += piece.chars().count() as u32;
        char_at.insert(s, chars);
        char_at.insert(e, chars);
        pos = e;
    }
    plain.push_str(&content[pos..]);
    if ascii_trim(&plain).is_empty() {
        return (content.to_string(), Vec::new());
    }

    let spans = pairs
        .iter()
        .filter_map(|&(o, c)| {
            let (effect, colors) = marks[o].open.clone()?;
            let from = *char_at.get(&marks[o].end)?;
            let to = *char_at.get(&marks[c].at)?;
            (from < to).then_some(ColorSpan { from, to, effect, colors })
        })
        .take(MAX_SPANS)
        .collect();
    (plain, spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(from: u32, to: u32, effect: Effect, colors: &[&str]) -> ColorSpan {
        ColorSpan { from, to, effect, colors: colors.iter().map(|c| c.to_string()).collect() }
    }

    #[test]
    fn extracts_each_tag_form() {
        let (plain, spans) = extract("I <rainbow>love</rainbow> it, <gradient #fd9855 #D161A2>so pretty</gradient>, <color pink>right?</color>");
        assert_eq!(plain, "I love it, so pretty, right?");
        assert_eq!(spans, vec![
            span(2, 6, Effect::Rainbow, &[]),
            span(11, 20, Effect::Gradient, &["#fd9855", "#d161a2"]),
            span(22, 28, Effect::Solid, &["#f472b6"]),
        ]);
        let (plain, spans) = extract(r##"<font color="#f00">hi</font> <font data-mx-color='#00ff00'>yo</font> <colour=#abc>x</colour>"##);
        assert_eq!(plain, "hi yo x");
        assert_eq!(spans, vec![
            span(0, 2, Effect::Solid, &["#ff0000"]),
            span(3, 5, Effect::Solid, &["#00ff00"]),
            span(6, 7, Effect::Solid, &["#aabbcc"]),
        ]);
    }

    #[test]
    fn offsets_count_chars_not_bytes() {
        let (plain, spans) = extract("héllo 🌈 <rainbow>wörld 🎉</rainbow>!");
        assert_eq!(plain, "héllo 🌈 wörld 🎉!");
        assert_eq!(spans, vec![span(8, 15, Effect::Rainbow, &[])]);
        let tagged: String = plain.chars().skip(8).take(7).collect();
        assert_eq!(tagged, "wörld 🎉");
    }

    #[test]
    fn nests_and_leaves_strays() {
        let (plain, spans) = extract("<color red>a <color blue>b</color> c</color>");
        assert_eq!(plain, "a b c");
        assert_eq!(spans, vec![span(0, 5, Effect::Solid, &["#f04848"]), span(2, 3, Effect::Solid, &["#60a5fa"])]);
        assert_eq!(extract("1 < 2 and <rainbow>open").0, "1 < 2 and <rainbow>open");
        assert_eq!(extract("stray </rainbow> close").0, "stray </rainbow> close");
        assert_eq!(extract("<color nope>x</color>").0, "<color nope>x</color>");
        assert_eq!(extract("<gradient #fff>x</gradient>").0, "<gradient #fff>x</gradient>");
        assert_eq!(extract("<rainbowy>x</rainbowy>").0, "<rainbowy>x</rainbowy>");
        assert_eq!(extract("<rainbow\n>x</rainbow>").0, "<rainbow\n>x</rainbow>");
        assert_eq!(extract("<rainbow> </rainbow>").0, "<rainbow> </rainbow>", "markup alone never empties a send");
    }

    #[test]
    fn code_keeps_its_markup() {
        let (plain, spans) = extract("`<rainbow>x</rainbow>` and\n```\n<color red>y</color>\n```\n<rainbow>z</rainbow>");
        assert_eq!(plain, "`<rainbow>x</rainbow>` and\n```\n<color red>y</color>\n```\nz");
        assert_eq!(spans, vec![span(56, 57, Effect::Rainbow, &[])]);
        // Code inside a span stays code; the span still wraps it.
        let (plain, spans) = extract("<rainbow>a `b` c</rainbow>");
        assert_eq!(plain, "a `b` c");
        assert_eq!(spans, vec![span(0, 7, Effect::Rainbow, &[])]);
    }

    // The same cases run against `src/js/text-color.js` (scripts/test-text-color.mjs):
    // the composer previews what this sends.
    #[test]
    fn every_name_resolves_to_its_table_hex() {
        let table: std::collections::HashMap<String, String> = serde_json::from_str(include_str!("text_color_names.json")).unwrap();
        assert!(table.len() > 150);
        for (name, hex_value) in &table {
            assert_eq!(hex(name).as_deref(), Some(hex_value.as_str()), "{name}");
            assert_eq!(hex(&name.to_ascii_uppercase()).as_deref(), Some(hex_value.as_str()), "{name} uppercase");
        }
        assert_eq!(hex("brown").as_deref(), Some("#a52a2a"));
        assert_eq!(hex("notacolour"), None);
    }

    #[test]
    fn shared_cases_match_the_js_parser() {
        let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!("text_color_cases.json")).unwrap();
        for case in cases {
            let input = case["input"].as_str().unwrap();
            let (plain, spans) = extract(input);
            assert_eq!(plain, case["plain"].as_str().unwrap(), "plain of {input:?}");
            let want: Vec<ColorSpan> = case["spans"].as_array().unwrap().iter().map(|s| {
                let s = s.as_array().unwrap();
                ColorSpan {
                    from: s[0].as_u64().unwrap() as u32,
                    to: s[1].as_u64().unwrap() as u32,
                    effect: Effect::parse(s[2].as_str().unwrap()).unwrap(),
                    colors: s[3..].iter().map(|c| c.as_str().unwrap().to_string()).collect(),
                }
            }).collect();
            assert_eq!(spans, want, "spans of {input:?}");
        }
    }

    #[test]
    fn tags_round_trip_and_reject_what_they_dont_know() {
        let s = span(1, 4, Effect::Gradient, &["#112233", "#445566"]);
        let parts = s.to_tag_parts();
        let refs: Vec<&str> = parts.iter().map(|p| p.as_str()).collect();
        assert_eq!(ColorSpan::from_parts(&refs, 4), Some(s));
        let reject = |p: &[&str]| ColorSpan::from_parts(p, 10).is_none();
        assert!(reject(&["color", "0", "11", "rainbow"]), "past the content");
        assert!(reject(&["color", "3", "3", "rainbow"]), "empty");
        assert!(reject(&["color", "0", "2", "rainbow", "#ffffff"]), "rainbow takes no stops");
        assert!(reject(&["color", "0", "2", "solid", "red"]), "names never reach the wire");
        assert!(reject(&["color", "0", "2", "solid", "#FFFFFF"]), "uppercase isn't canonical");
        assert!(reject(&["color", "0", "2", "solid", "#ffffff", "bg:#000000"]), "unknown args drop the tag");
        assert!(reject(&["color", "0", "2", "sparkle"]));
    }
}
