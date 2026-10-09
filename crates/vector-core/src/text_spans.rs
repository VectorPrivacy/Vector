//! Inline annotations a sender writes as markup and the wire carries as tags: colour
//! (`text_color`) and times (`text_time`). Core takes the markup out at every send and
//! edit; each span names a char range of the plain content that other clients read.

use serde::{Deserialize, Serialize};

pub use crate::text_color::ColorSpan;
pub use crate::text_time::TimeSpan;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TextSpan {
    Color(ColorSpan),
    Time(TimeSpan),
}

impl TextSpan {
    /// The span as stored and sent: the tag name, then its fields.
    pub fn to_tag_parts(&self) -> Vec<String> {
        match self {
            TextSpan::Color(s) => s.to_tag_parts(),
            TextSpan::Time(s) => s.to_tag_parts(),
        }
    }

}

/// The spans in `tags` (each a tag's parts), checked against `content`. Its chars are
/// only collected for a time tag, the one kind that reads the text it covers.
fn collect<'a>(tags: impl Iterator<Item = Vec<&'a str>>, content: &str) -> Vec<TextSpan> {
    let len = std::cell::OnceCell::new();
    let chars = std::cell::OnceCell::new();
    let mut out: Vec<TextSpan> = Vec::new();
    for parts in tags {
        let span = match parts.first().copied() {
            Some(crate::text_color::TAG) => {
                ColorSpan::from_parts(&parts, *len.get_or_init(|| content.chars().count() as u32)).map(TextSpan::Color)
            }
            Some(crate::text_time::TAG) => {
                let chars = chars.get_or_init(|| content.chars().collect::<Vec<char>>());
                TimeSpan::from_parts(&parts, chars)
                    .filter(|t| !out.iter().any(|s| matches!(s, TextSpan::Time(o) if o.from == t.from)))
                    .map(TextSpan::Time)
            }
            _ => None,
        };
        out.extend(span);
        if out.len() == 128 {
            break;
        }
    }
    out
}

/// Take the markup out of `content`: colour tags, then `<t:…>` times. Returns the plain
/// text and its spans, offsets in chars of that text.
pub fn extract(content: &str) -> (String, Vec<TextSpan>) {
    let (plain, colors) = crate::text_color::extract(content);
    // A code split by colour markup isn't one: its edges would fall inside the time.
    let (mut seen, mut seen_chars) = (0usize, 0u32);
    let tokens: Vec<_> = crate::text_time::tokens(&plain)
        .into_iter()
        .filter(|&(start, end, ..)| {
            seen_chars += plain[seen..start].chars().count() as u32;
            seen = start;
            let (s, e) = (seen_chars, seen_chars + plain[start..end].chars().count() as u32);
            !colors.iter().any(|c| (c.from > s && c.from < e) || (c.to > s && c.to < e))
        })
        .collect();
    let mut spans: Vec<TextSpan> = Vec::with_capacity(colors.len() + tokens.len());
    if tokens.is_empty() {
        spans.extend(colors.into_iter().map(TextSpan::Color));
        return (plain, spans);
    }
    // Each token becomes its fallback; colour edges after it shift by the difference.
    let mut out = String::with_capacity(plain.len() + tokens.len() * 8);
    let mut shifts: Vec<(u32, i64)> = Vec::with_capacity(tokens.len());
    let mut times = Vec::with_capacity(tokens.len());
    let (mut pos, mut chars, mut delta) = (0usize, 0u32, 0i64);
    for (start, end, unix, style) in tokens {
        let before = &plain[pos..start];
        out.push_str(before);
        chars += before.chars().count() as u32;
        let token_chars = plain[start..end].chars().count() as u32;
        let text = crate::text_time::fallback(unix).expect("tokens only yield times in range");
        let text_chars = text.chars().count() as u32;
        times.push(TimeSpan { from: chars, to: chars + text_chars, unix, style });
        out.push_str(&text);
        let original_end = (chars as i64 - delta) as u32 + token_chars;
        delta += text_chars as i64 - token_chars as i64;
        shifts.push((original_end, delta));
        chars += text_chars;
        pos = end;
    }
    out.push_str(&plain[pos..]);
    let shift = |p: u32| -> u32 {
        let d = shifts.iter().rev().find(|&&(at, _)| p >= at).map_or(0, |&(_, d)| d);
        (p as i64 + d) as u32
    };
    spans.extend(colors.into_iter().map(|c| TextSpan::Color(ColorSpan { from: shift(c.from), to: shift(c.to), ..c })));
    spans.extend(times.into_iter().map(TextSpan::Time));
    (out, spans)
}

/// The spans a rumor's tags carry, checked against its content.
pub fn from_tags<'a, I>(tags: I, content: &str) -> Vec<TextSpan>
where
    I: IntoIterator<Item = &'a nostr_sdk::prelude::Tag>,
{
    let tags = tags.into_iter().filter(|t| is_span_tag(t.as_slice().first().map(|s| s.as_str())));
    collect(tags.map(|t| t.as_slice().iter().map(|s| s.as_str()).collect()), content)
}

/// The spans in a stored event's tags, checked against its content.
pub fn from_stored(tags: &[Vec<String>], content: &str) -> Vec<TextSpan> {
    let tags = tags.iter().filter(|t| is_span_tag(t.first().map(|s| s.as_str())));
    collect(tags.map(|t| t.iter().map(|s| s.as_str()).collect()), content)
}

fn is_span_tag(name: Option<&str>) -> bool {
    matches!(name, Some(crate::text_color::TAG | crate::text_time::TAG))
}

/// The spans as rumor tags.
pub fn to_nostr_tags(spans: &[TextSpan]) -> impl Iterator<Item = nostr_sdk::prelude::Tag> + '_ {
    spans.iter().map(|s| {
        let mut parts = s.to_tag_parts();
        parts.remove(0);
        let name = match s {
            TextSpan::Color(_) => crate::text_color::TAG,
            TextSpan::Time(_) => crate::text_time::TAG,
        };
        nostr_sdk::prelude::Tag::custom(name, parts)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_color::Effect;

    #[test]
    fn times_and_colours_extract_together() {
        let (plain, spans) = extract("<rainbow>Party</rainbow> at <t:1618953600:F>, <color pink>be there</color> <t:0:R>!");
        assert_eq!(plain, "Party at 2021-04-20 21:20 UTC, be there 1970-01-01 00:00 UTC!");
        assert_eq!(spans, vec![
            TextSpan::Color(ColorSpan { from: 0, to: 5, effect: Effect::Rainbow, colors: vec![] }),
            TextSpan::Color(ColorSpan { from: 31, to: 39, effect: Effect::Solid, colors: vec!["#f472b6".into()] }),
            TextSpan::Time(TimeSpan { from: 9, to: 29, unix: 1_618_953_600, style: 'F' }),
            TextSpan::Time(TimeSpan { from: 40, to: 60, unix: 0, style: 'R' }),
        ]);
        let chars: Vec<char> = plain.chars().collect();
        assert_eq!(chars[31..39].iter().collect::<String>(), "be there", "colour shifted past the time");
    }

    #[test]
    fn a_colour_around_a_time_stretches_with_it() {
        let (plain, spans) = extract("<color teal>doors <t:1618953600:t></color>");
        assert_eq!(plain, "doors 2021-04-20 21:20 UTC");
        assert_eq!(spans[0], TextSpan::Color(ColorSpan { from: 0, to: 26, effect: Effect::Solid, colors: vec!["#2dd4bf".into()] }));
    }

    #[test]
    fn tags_round_trip_through_the_wire() {
        let (plain, spans) = extract("go <t:1618953630:R> <rainbow>now</rainbow>");
        let tags: Vec<nostr_sdk::prelude::Tag> = to_nostr_tags(&spans).collect();
        let mut back = from_tags(tags.iter(), &plain);
        let mut want = spans.clone();
        back.sort_by_key(|s| format!("{s:?}"));
        want.sort_by_key(|s| format!("{s:?}"));
        assert_eq!(back, want);
        let stored: Vec<Vec<String>> = spans.iter().map(|s| s.to_tag_parts()).collect();
        assert_eq!(from_stored(&stored, &plain).len(), 2);
        let doubled: Vec<Vec<String>> = stored.iter().chain(stored.iter()).cloned().collect();
        assert_eq!(from_stored(&doubled, &plain).iter().filter(|s| matches!(s, TextSpan::Time(_))).count(), 1, "one chip per range");
        assert!(from_tags(tags.iter(), "something else entirely").iter().all(|s| matches!(s, TextSpan::Color(_))), "a time can't cover other text");
    }

    // The same cases run against the JS extractor (scripts/test-text-color.mjs), which
    // shows an edit's result before core sends it.
    #[test]
    fn shared_cases_match_the_js_extractor() {
        let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!("text_spans_cases.json")).unwrap();
        for case in cases {
            let input = case["input"].as_str().unwrap();
            let (plain, spans) = extract(input);
            assert_eq!(plain, case["plain"].as_str().unwrap(), "plain of {input:?}");
            assert_eq!(serde_json::to_value(&spans).unwrap(), case["spans"], "spans of {input:?}");
        }
    }

    #[test]
    fn serde_names_the_kind() {
        let json = serde_json::to_string(&TextSpan::Time(TimeSpan { from: 1, to: 2, unix: 3, style: 'R' })).unwrap();
        assert_eq!(json, r#"{"kind":"time","from":1,"to":2,"unix":3,"style":"R"}"#);
        let json = serde_json::to_string(&TextSpan::Color(ColorSpan { from: 0, to: 1, effect: Effect::Rainbow, colors: vec![] })).unwrap();
        assert_eq!(json, r#"{"kind":"color","from":0,"to":1,"effect":"rainbow"}"#);
    }
}
