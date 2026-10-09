//! Times that read the same everywhere, Discord's `<t:UNIX:STYLE>`.
//!
//! On send each `<t:…>` outside code becomes a fixed UTC rendering, `2026-10-09 14:00 UTC`,
//! and a `["time", from, to, unix, style]` tag. Other clients read a real time; a Vector
//! shows it in the reader's own zone and locale. A tag only counts when the text it covers
//! is exactly that rendering of its own instant, so it can never stand in for other words.

use serde::{Deserialize, Serialize};

pub const TAG: &str = "time";
/// Discord's styles: t T (time), d D (date), f F (date and time, `f` the default),
/// s S (short date with time), R (relative).
pub const STYLES: &str = "tTdDfFsSR";
const MAX_SPANS: usize = 64;
/// Years 0000-9999, so the fallback always has its fixed shape.
const MIN_UNIX: i64 = -62_167_219_200;
const MAX_UNIX: i64 = 253_402_300_799;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TimeSpan {
    pub from: u32,
    pub to: u32,
    pub unix: i64,
    pub style: char,
}

impl TimeSpan {
    /// The span as stored and sent: the tag name, then its fields.
    pub fn to_tag_parts(&self) -> Vec<String> {
        vec![TAG.to_string(), self.from.to_string(), self.to.to_string(), self.unix.to_string(), self.style.to_string()]
    }

    /// A tag that covers exactly its own fallback in `content`, or None.
    pub(crate) fn from_parts(parts: &[&str], content: &[char]) -> Option<Self> {
        if parts.len() != 5 || parts[0] != TAG {
            return None;
        }
        let from: u32 = parts[1].parse().ok()?;
        let to: u32 = parts[2].parse().ok()?;
        let unix: i64 = parts[3].parse().ok()?;
        let mut style = parts[4].chars();
        let style = style.next().filter(|c| STYLES.contains(*c) && style.next().is_none())?;
        let expect = fallback(unix)?;
        // The fallback is ASCII: its length in chars is its length in bytes.
        if to.checked_sub(from)? as usize != expect.len() {
            return None;
        }
        let covered = content.get(from as usize..to as usize)?;
        covered.iter().copied().eq(expect.chars()).then_some(TimeSpan { from, to, unix, style })
    }
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// The fixed UTC text a time travels as: `YYYY-MM-DD HH:MM UTC`, with `:SS` when not on the minute.
pub fn fallback(unix: i64) -> Option<String> {
    if !(MIN_UNIX..=MAX_UNIX).contains(&unix) {
        return None;
    }
    let (y, mo, d) = civil(unix.div_euclid(86_400));
    let secs = unix.rem_euclid(86_400);
    let (h, mi, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    Some(if s == 0 {
        format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02} UTC")
    } else {
        format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC")
    })
}

/// Every `<t:UNIX>` / `<t:UNIX:STYLE>` outside code: (byte start, byte end, unix, style).
pub(crate) fn tokens(content: &str) -> Vec<(usize, usize, i64, char)> {
    if !content.contains("<t:") {
        return Vec::new();
    }
    let code = crate::text_color::code_ranges(content);
    let in_code = |i: usize| code.iter().any(|&(s, e)| i >= s && i < e);
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = content[i..].find("<t:") {
        let at = i + off;
        i = at + 3;
        if in_code(at) {
            continue;
        }
        let rest = &content[at + 3..];
        let digits = rest.char_indices().take_while(|&(k, c)| c.is_ascii_digit() || (k == 0 && c == '-')).count();
        let num = &rest[..digits];
        let after = &rest[digits..];
        let (style, len) = if after.starts_with('>') {
            ('f', 1)
        } else if after.len() >= 3 && after.as_bytes()[0] == b':' && after.as_bytes()[2] == b'>' {
            (after.as_bytes()[1] as char, 3)
        } else {
            continue;
        };
        let Ok(unix) = num.parse::<i64>() else { continue };
        if !STYLES.contains(style) || num.trim_start_matches('-').len() > 12 || fallback(unix).is_none() {
            continue;
        }
        let end = at + 3 + digits + len;
        out.push((at, end, unix, style));
        if out.len() == MAX_SPANS {
            break;
        }
        i = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_is_fixed_utc() {
        assert_eq!(fallback(0).unwrap(), "1970-01-01 00:00 UTC");
        assert_eq!(fallback(1_618_953_630).unwrap(), "2021-04-20 21:20:30 UTC");
        assert_eq!(fallback(1_709_208_000).unwrap(), "2024-02-29 12:00 UTC", "a leap day");
        assert_eq!(fallback(-1).unwrap(), "1969-12-31 23:59:59 UTC");
        assert_eq!(fallback(MAX_UNIX).unwrap(), "9999-12-31 23:59:59 UTC");
        assert_eq!(fallback(MIN_UNIX).unwrap(), "0000-01-01 00:00 UTC");
        assert!(fallback(MAX_UNIX + 1).is_none());
    }

    #[test]
    fn tokens_take_discords_forms_outside_code() {
        let t = tokens("at <t:1618953630:R> and <t:0> but `<t:5:R>` and <t:9:x> <t:12:Rx> <t:abc:R>");
        assert_eq!(t.iter().map(|x| (x.2, x.3)).collect::<Vec<_>>(), vec![(1_618_953_630, 'R'), (0, 'f')]);
    }

    #[test]
    fn a_tag_only_covers_its_own_fallback() {
        let content: Vec<char> = "meet 2021-04-20 21:20:30 UTC ok".chars().collect();
        let good = ["time", "5", "28", "1618953630", "R"];
        assert!(TimeSpan::from_parts(&good, &content).is_some());
        assert!(TimeSpan::from_parts(&["time", "5", "28", "1618953631", "R"], &content).is_none(), "another instant");
        assert!(TimeSpan::from_parts(&["time", "0", "4", "1618953630", "R"], &content).is_none(), "other words");
        assert!(TimeSpan::from_parts(&["time", "5", "28", "1618953630", "Q"], &content).is_none());
        assert!(TimeSpan::from_parts(&["time", "5", "28", "1618953630", "RR"], &content).is_none());
        assert!(TimeSpan::from_parts(&["time", "5", "28", "1618953630", "R", "x"], &content).is_none(), "unknown args void it");
    }
}
