//! Times that read the same everywhere (`text-time.js`): `<t:UNIX:STYLE>` shown in the
//! reader's zone, a typed phrase read as a time, and a draft's trailing phrase offered as one.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone};
use crate::scan::{boundary, digit, fold_letter, lit, lit_i, matches, next, prev, run, word, At};
use std::rc::Rc;

use gpui::{StatefulInteractiveElement, 
    AnyElement, App, InteractiveElement, IntoElement, ParentElement, Styled, Window, div,
    px, svg, AnimationExt,
};

use crate::shortcodes::Row;
use crate::theme::*;

pub const STYLES: &str = "tTdDfFsSR";

fn at(unix: i64) -> Option<DateTime<Local>> {
    Local.timestamp_opt(unix, 0).single()
}

fn clock(t: &DateTime<Local>, seconds: bool) -> String {
    if seconds { t.format("%-I:%M:%S %p").to_string() } else { t.format("%-I:%M %p").to_string() }
}

/// "in 2 hours", "3 days ago": the unit grows with the distance, always a count.
pub fn relative(unix: i64, now: f64) -> String {
    let diff = unix as f64 - now;
    let abs = diff.abs();
    if abs < 0.5 {
        return "now".into();
    }
    let (n, unit) = if abs < 59.5 {
        (diff, "second")
    } else if abs < 2700. {
        (diff / 60., "minute")
    } else if abs < 79_200. {
        (diff / 3600., "hour")
    } else if abs < 2_246_400. {
        (diff / 86_400., "day")
    } else if abs < 28_512_000. {
        (diff / 2_592_000., "month")
    } else {
        (diff / 31_536_000., "year")
    };
    // Intl rounds half away from zero on the magnitude, and keeps the sign for the direction.
    let k = n.abs().round() as i64;
    let unit = if k == 1 { unit.to_string() } else { format!("{unit}s") };
    if diff >= 0. { format!("in {k} {unit}") } else { format!("{k} {unit} ago") }
}

/// A time in the reader's zone, in one of Discord's styles.
pub fn format(unix: i64, style: char, now: f64) -> String {
    if style == 'R' {
        return relative(unix, now);
    }
    let Some(t) = at(unix) else { return String::new() };
    match style {
        't' => clock(&t, false),
        'T' => clock(&t, true),
        'd' => t.format("%m/%d/%Y").to_string(),
        'D' => t.format("%B %-d, %Y").to_string(),
        'F' => format!("{} at {}", t.format("%A, %B %-d, %Y"), clock(&t, false)),
        's' => format!("{}, {}", t.format("%m/%d/%Y"), clock(&t, false)),
        'S' => format!("{}, {}", t.format("%m/%d/%Y"), clock(&t, true)),
        _ => format!("{} at {}", t.format("%B %-d, %Y"), clock(&t, false)),
    }
}

fn zone(t: &DateTime<Local>) -> String {
    let secs = t.offset().local_minus_utc();
    if secs == 0 {
        return "UTC".into();
    }
    let (sign, secs) = if secs < 0 { ('-', -secs) } else { ('+', secs) };
    let (h, m) = (secs / 3600, secs % 3600 / 60);
    if m == 0 { format!("GMT{sign}{h}") } else { format!("GMT{sign}{h}:{m:02}") }
}

/// The hover text: the full date and time with the zone; just the date for a day.
pub fn full(unix: i64, date_only: bool) -> String {
    let Some(t) = at(unix) else { return String::new() };
    if date_only {
        return t.format("%A, %B %-d, %Y").to_string();
    }
    format!("{} at {} {}", t.format("%A, %B %-d, %Y"), clock(&t, true), zone(&t))
}

/// A chip's hover (`ttHint`): how far off a written time is, or a countdown's moment.
pub fn hint(unix: i64, style: char, now: f64) -> String {
    if style != 'R' {
        return relative(unix, now);
    }
    if (unix as f64 - now).abs() < 86_400. {
        return format(unix, 't', now);
    }
    let Some(t) = at(unix) else { return String::new() };
    let this_year = at(now as i64).is_some_and(|n| n.year() == t.year());
    if this_year { t.format("%a, %b %-d").to_string() } else { t.format("%a, %b %-d, %Y").to_string() }
}

/// A countdown's moment, briefly: the time today, else a short day and date too.
pub fn when_at(unix: i64, now: f64) -> String {
    let Some(t) = at(unix) else { return String::new() };
    if at(now as i64).is_some_and(|n| n.date_naive() == t.date_naive()) {
        return clock(&t, false);
    }
    format!("{}, {}", t.format("%a, %b %-d"), clock(&t, false))
}

// ---- reading a typed time ----

const UNITS: &[(&[&str], f64)] = &[
    (&["s", "sec", "secs", "second", "seconds"], 1.),
    (&["m", "min", "mins", "minute", "minutes"], 60.),
    (&["h", "hr", "hrs", "hour", "hours"], 3600.),
    (&["d", "day", "days"], 86_400.),
    (&["w", "wk", "wks", "week", "weeks"], 604_800.),
    (&["mo", "mos", "month", "months"], 2_592_000.),
    (&["y", "yr", "yrs", "year", "years"], 31_536_000.),
];
const DAYS: [&str; 7] = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];
const MONTHS: [&str; 12] = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];

/// Whitespace runs as one space, as `\s+` → " " does.
fn squeeze(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut gap = false;
    for c in t.chars() {
        if c.is_whitespace() {
            gap = true;
        } else {
            if gap {
                out.push(' ');
                gap = false;
            }
            out.push(c);
        }
    }
    if gap {
        out.push(' ');
    }
    out
}

/// Whether everything outside the matches is whitespace: `replace_all(..., "").trim().is_empty()`.
fn only_matches<C>(t: &str, found: impl Iterator<Item = (usize, usize, C)>) -> bool {
    let mut last = 0;
    for (s, e, _) in found {
        if !t[last..s].trim().is_empty() {
            return false;
        }
        last = e;
    }
    t[last..].trim().is_empty()
}

type Span = Option<(usize, usize)>;

/// `(\d+(?:\.\d+)?|half an?|an?|one|half) ([a-z]+)`
fn dur_at(t: &str, p: usize) -> Option<(usize, [Span; 2])> {
    let letters = |from: usize| {
        let (n, e) = run(t, lit(t, from, " ")?, usize::MAX, |c| c.is_ascii_lowercase());
        (n > 0).then_some((from, e))
    };
    let (n, mut e) = run(t, p, usize::MAX, digit);
    if n > 0 {
        if let Some(f) = lit(t, e, ".") {
            let (m, fe) = run(t, f, usize::MAX, digit);
            if m > 0 {
                e = fe;
            }
        }
        return letters(e).map(|(a, end)| (end, [Some((p, a)), Some((a + 1, end))]));
    }
    ["half an", "half a", "an", "a", "one", "half"].iter().find_map(|w| {
        let a = lit(t, p, w)?;
        letters(a).map(|(a, end)| (end, [Some((p, a)), Some((a + 1, end))]))
    })
}

/// "2h 30m", "an hour", "3 days and 4 hours": seconds, or None when any word isn't one.
pub fn duration(text: &str) -> Option<f64> {
    // Every reading opens with an amount: a digit, "a", "an", "and", "half" or "one".
    if !text.trim_start().starts_with(|c: char| digit(c) || matches!(c, 'a' | 'h' | 'o')) {
        return None;
    }
    let mut clean = String::with_capacity(text.len() + 8);
    let mut last = 0;
    for (s, e, ()) in crate::scan::matches_from(text, crate::scan::lead(b"a"), |t, p| (boundary(t, p) && lit(t, p, "and").is_some_and(|e| boundary(t, e))).then_some((p + 3, ()))) {
        clean += &text[last..s];
        clean.push(' ');
        last = e;
    }
    clean += &text[last..];
    // `(\d)([a-z])` then `([a-z])(\d)` → "$1 $2": pairs of either kind can't overlap.
    let mut spaced = String::with_capacity(clean.len() + 8);
    let mut before: Option<char> = None;
    for c in clean.chars() {
        if before.is_some_and(digit) && c.is_ascii_lowercase() {
            spaced.push(' ');
        }
        before = Some(c);
        spaced.push(c);
    }
    let mut both = String::with_capacity(spaced.len() + 8);
    let mut before: Option<char> = None;
    for c in spaced.chars() {
        if before.is_some_and(|b| b.is_ascii_lowercase()) && digit(c) {
            both.push(' ');
        }
        before = Some(c);
        both.push(c);
    }
    let clean = squeeze(&both);
    let clean = clean.trim();
    let mut total = 0.;
    let mut last = 0;
    for (s, e, [a, u]) in matches(clean, dur_at) {
        if !clean[last..s].trim().is_empty() {
            return None;
        }
        last = e;
        let (a, u) = (span(clean, a), span(clean, u));
        let amount = if a.starts_with(|c: char| c.is_ascii_digit()) {
            a.parse::<f64>().ok()?
        } else if a.starts_with("half") {
            0.5
        } else {
            1.
        };
        let unit = UNITS.iter().find(|(words, _)| words.contains(&u))?;
        total += amount * unit.1;
    }
    (total != 0. && clean[last..].trim().is_empty()).then_some(total)
}

fn span(t: &str, s: Span) -> &str {
    s.map_or("", |(a, b)| &t[a..b])
}

pub struct Parsed {
    pub unix: i64,
    pub date_only: bool,
}

/// A typed time, or None when it isn't one (`ttParseFull`).
pub fn parse_full(input: &str, now: DateTime<Local>) -> Option<Parsed> {
    let p = read(input, now)?;
    vector_core::text_time::fallback(p.unix).map(|_| p)
}

fn local_at(date: NaiveDate, h: u32, m: u32, s: u32) -> Option<i64> {
    let naive = date.and_hms_opt(h, m, s)?;
    Local.from_local_datetime(&naive).earliest().map(|t| t.timestamp())
}

fn add_months(date: NaiveDate, months: i32) -> Option<NaiveDate> {
    let total = date.year() * 12 + date.month0() as i32 + months;
    let (y, m0) = (total.div_euclid(12), total.rem_euclid(12) as u32);
    let last = (28..=31).rev().find(|d| NaiveDate::from_ymd_opt(y, m0 + 1, *d).is_some())?;
    NaiveDate::from_ymd_opt(y, m0 + 1, date.day().min(last))
}

type Caps = [Span; 5];

/// `at ` if it's there.
fn at_prefix(t: &str, p: usize) -> usize {
    lit(t, p, "at ").unwrap_or(p)
}

/// The first of `words` at `p` that ends on a word boundary, and where it ends.
fn word_of(t: &str, p: usize, words: &[&str]) -> Option<(usize, usize)> {
    words.iter().enumerate().find_map(|(i, w)| lit(t, p, w).filter(|&e| boundary(t, e)).map(|e| (i, e)))
}

/// `\b(?:at )?(\d{1,2})(?::(\d{2}))?(?::(\d{2}))? ?(am|pm|a\.m\.|p\.m\.|a|p)`
fn ampm_at(t: &str, p: usize) -> Option<(usize, Caps)> {
    if !boundary(t, p) {
        return None;
    }
    let q = at_prefix(t, p);
    let (n, mut e) = run(t, q, 2, digit);
    if n == 0 {
        return None;
    }
    let mut caps: Caps = [None, Some((q, e)), None, None, None];
    for slot in [2, 3] {
        let Some(c) = lit(t, e, ":") else { break };
        let (n, ce) = run(t, c, 2, digit);
        if n < 2 {
            break;
        }
        caps[slot] = Some((c, ce));
        e = ce;
    }
    let s = lit(t, e, " ").unwrap_or(e);
    let end = ["am", "pm", "a.m.", "p.m.", "a", "p"].iter().find_map(|w| lit(t, s, w))?;
    caps[4] = Some((s, end));
    Some((end, caps))
}

/// `\b(?:at )?([01]?\d|2[0-3]):([0-5]\d)(?::([0-5]\d))?\b`
fn h24_at(t: &str, p: usize) -> Option<(usize, Caps)> {
    if !boundary(t, p) {
        return None;
    }
    let q = at_prefix(t, p);
    let mut cs = t[q..].char_indices().map(|(i, c)| (q + i, c));
    let (c0, c1, c2) = (cs.next(), cs.next(), cs.next());
    let hour_end = match (c0, c1, c2) {
        (Some((_, '0' | '1')), Some((_, d)), Some((i, ':'))) if digit(d) => i,
        (Some((_, d)), Some((i, ':')), _) if digit(d) => i,
        (Some((_, '2')), Some((_, '0'..='3')), Some((i, ':'))) => i,
        _ => return None,
    };
    let two = |from: usize| {
        let mut cs = t[from..].chars();
        match (cs.next(), cs.next()) {
            (Some('0'..='5'), Some(d)) if digit(d) => Some(from + 1 + d.len_utf8()),
            _ => None,
        }
    };
    let m = hour_end + 1;
    let me = two(m)?;
    if let Some(se) = lit(t, me, ":").and_then(two).filter(|&se| boundary(t, se)) {
        return Some((se, [None, Some((q, hour_end)), Some((m, me)), Some((me + 1, se)), None]));
    }
    boundary(t, me).then_some((me, [None, Some((q, hour_end)), Some((m, me)), None, None]))
}

/// `\b(?:at )?(noon|midday|midnight)\b`
fn noon_at(t: &str, p: usize) -> Option<(usize, Caps)> {
    if !boundary(t, p) {
        return None;
    }
    let q = at_prefix(t, p);
    let (_, e) = word_of(t, q, &["noon", "midday", "midnight"])?;
    Some((e, [None, Some((q, e)), None, None, None]))
}

/// `\bat (\d{1,2})\b`
fn at_at(t: &str, p: usize) -> Option<(usize, Caps)> {
    if !boundary(t, p) {
        return None;
    }
    let q = lit(t, p, "at ")?;
    let (n, e) = run(t, q, 2, digit);
    (n > 0 && !next(t, e).is_some_and(word)).then_some((e, [None, Some((q, e)), None, None, None]))
}

/// `\b(?:in the |this )?(morning|afternoon|evening|night)\b`
fn part_at(t: &str, p: usize) -> Option<(usize, Caps)> {
    if !boundary(t, p) {
        return None;
    }
    [lit(t, p, "in the "), lit(t, p, "this "), Some(p)].into_iter().flatten().find_map(|q| {
        let (_, e) = word_of(t, q, &["morning", "afternoon", "evening", "night"])?;
        Some((e, [None, Some((q, e)), None, None, None]))
    })
}

/// `\d{1,2}` then a char `then` accepts: the run must not be longer.
fn digits_then(t: &str, p: usize, then: impl Fn(Option<char>) -> bool) -> Option<usize> {
    let (n, e) = run(t, p, 2, digit);
    (n > 0 && then(next(t, e))).then_some(e)
}

fn sep(c: Option<char>) -> bool {
    matches!(c, Some('/' | '.'))
}

fn no_word(c: Option<char>) -> bool {
    !c.is_some_and(word)
}

/// `\b(\d{4})-(\d{1,2})-(\d{1,2})\b`
fn iso_at(t: &str, p: usize) -> Option<(usize, Caps)> {
    if !boundary(t, p) {
        return None;
    }
    let (n, y) = run(t, p, 4, digit);
    if n < 4 {
        return None;
    }
    let m = lit(t, y, "-")?;
    let me = digits_then(t, m, |c| c == Some('-'))?;
    let d = me + 1;
    let de = digits_then(t, d, no_word)?;
    Some((de, [None, Some((p, y)), Some((m, me)), Some((d, de)), None]))
}

/// `\b(\d{1,2})[/.](\d{1,2})(?:[/.](\d{2,4}))?\b`
fn slash_at(t: &str, p: usize) -> Option<(usize, Caps)> {
    if !boundary(t, p) {
        return None;
    }
    let a = digits_then(t, p, sep)?;
    let b = a + 1;
    let (n, be) = run(t, b, 2, digit);
    if n == 0 {
        return None;
    }
    if sep(next(t, be)) {
        let y = be + 1;
        let (n, ye) = run(t, y, 4, digit);
        if n >= 2 && no_word(next(t, ye)) {
            return Some((ye, [None, Some((p, a)), Some((b, be)), Some((y, ye)), None]));
        }
    }
    no_word(next(t, be)).then_some((be, [None, Some((p, a)), Some((b, be)), None, None]))
}

/// `({month})\.?` as `read` spells them, each way it can end, in the order a regex tries them.
fn month_ends(t: &str, p: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
    let name = MONTHS.iter().find(|n| lit(t, p, &n[..3]).is_some());
    name.into_iter().flat_map(move |n| {
        let short = p + 3;
        let full = (n.len() > 3).then(|| lit(t, p, n)).flatten();
        full.into_iter().chain([short]).flat_map(move |m| lit(t, m, ".").into_iter().chain([m]).map(move |e| (m, e)))
    })
}

/// `\d{1,2}` then `(?:st|nd|rd|th)?`, each way.
fn day_ends(t: &str, p: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
    let (n, e) = run(t, p, 2, digit);
    let e = (n > 0 && !next(t, e).is_some_and(digit)).then_some(e);
    e.into_iter().flat_map(move |d| ["st", "nd", "rd", "th"].iter().find_map(|s| lit(t, d, s)).into_iter().chain([d]).map(move |s| (d, s)))
}

/// `(?: (\d{4}))?\b` from `at`: where it ends and the year.
fn year_tail(t: &str, at: usize) -> Option<(usize, Span)> {
    if let Some(y) = lit(t, at, " ") {
        let (n, ye) = run(t, y, 4, digit);
        if n == 4 && boundary(t, ye) {
            return Some((ye, Some((y, ye))));
        }
    }
    boundary(t, at).then_some((at, None))
}

/// `\b{month} (\d{1,2})(?:st|nd|rd|th)?(?: (\d{4}))?\b`
fn md_at(t: &str, p: usize) -> Option<(usize, Caps)> {
    if !boundary(t, p) {
        return None;
    }
    month_ends(t, p).find_map(|(m, me)| {
        let d = lit(t, me, " ")?;
        day_ends(t, d).find_map(|(de, se)| {
            let (end, year) = year_tail(t, se)?;
            Some((end, [None, Some((p, m)), Some((d, de)), year, None]))
        })
    })
}

/// `\b(\d{1,2})(?:st|nd|rd|th)? (?:of )?{month}(?: (\d{4}))?\b`
fn dm_at(t: &str, p: usize) -> Option<(usize, Caps)> {
    if !boundary(t, p) {
        return None;
    }
    day_ends(t, p).find_map(|(de, se)| {
        let sp = lit(t, se, " ")?;
        [lit(t, sp, "of "), Some(sp)].into_iter().flatten().find_map(|m| {
            month_ends(t, m).find_map(|(mn, me)| {
                let (end, year) = year_tail(t, me)?;
                Some((end, [None, Some((p, de)), Some((m, mn)), year, None]))
            })
        })
    })
}

/// `\b(today|tonight|tomorrow|tmrw|tmr|yesterday)\b`
fn dayword_at(t: &str, p: usize) -> Option<(usize, Caps)> {
    if !boundary(t, p) {
        return None;
    }
    let (_, e) = word_of(t, p, &["today", "tonight", "tomorrow", "tmrw", "tmr", "yesterday"])?;
    Some((e, [None, Some((p, e)), None, None, None]))
}

/// `\b(next|last|this) (week|month|year)\b`
fn step_at(t: &str, p: usize) -> Option<(usize, Caps)> {
    if !boundary(t, p) {
        return None;
    }
    let w = ["next", "last", "this"].iter().find_map(|w| lit(t, p, w))?;
    let u = lit(t, w, " ")?;
    let (_, e) = word_of(t, u, &["week", "month", "year"])?;
    Some((e, [None, Some((p, w)), Some((u, e)), None, None]))
}

/// `\b(?:(next|this|last|on|coming) )?(?:the )?(weekend|sun(?:day)?|…|sat(?:urday)?)\b`
fn weekday_at(t: &str, p: usize) -> Option<(usize, Caps)> {
    const NAMES: [&str; 18] = [
        "weekend", "sunday", "sun", "monday", "mon", "tues", "tuesday", "tue", "wednesday", "wed", "thur", "thurs",
        "thursday", "thu", "friday", "fri", "saturday", "sat",
    ];
    if !boundary(t, p) {
        return None;
    }
    let which = ["next", "this", "last", "on", "coming"].iter().filter_map(|w| lit(t, p, w).and_then(|e| lit(t, e, " ").map(|q| (Some((p, e)), q))));
    which.chain([(None, p)]).find_map(|(w, q)| {
        [lit(t, q, "the "), Some(q)].into_iter().flatten().find_map(|n| {
            let (_, e) = word_of(t, n, &NAMES)?;
            Some((e, [None, w, Some((n, e)), None, None]))
        })
    })
}

fn read(input: &str, now: DateTime<Local>) -> Option<Parsed> {
    let lower = input.to_lowercase().replace(',', " ");
    let mut text = squeeze(lower.trim());
    let now_s = now.timestamp();
    if text.is_empty() || text == "now" {
        return Some(Parsed { unix: now_s.div_euclid(60) * 60, date_only: false });
    }
    let digits_only = text.strip_prefix('-').unwrap_or(&text);
    if (9..=13).contains(&digits_only.chars().count()) && digits_only.chars().all(digit) {
        let n: i64 = text.parse().ok()?;
        let unix = if text.trim_start_matches('-').len() == 13 { n / 1000 } else { n };
        return Some(Parsed { unix, date_only: false });
    }
    let now_f = now.timestamp_millis() as f64 / 1000.;
    if let Some(rest) = text.strip_prefix("in ").filter(|r| !r.is_empty()) {
        let d = duration(rest)?;
        return Some(Parsed { unix: (now_f + d).round() as i64, date_only: false });
    }
    let ago = [(" ago", -1.), (" from now", 1.)]
        .into_iter()
        .find_map(|(w, sign)| text.strip_suffix(w).filter(|r| !r.is_empty()).map(|r| (r, sign)));
    if let Some((rest, sign)) = ago {
        let d = duration(rest)?;
        return Some(Parsed { unix: (now_f + sign * d).round() as i64, date_only: false });
    }

    let mut date = now.date_naive();
    let mut have_date = false;
    let mut yearless = false;
    let mut time: Option<[u32; 3]> = None;
    // Takes the first match `ok` accepts out of the text, as its captures' strings.
    let take = |text: &mut String, at: fn(&str, usize) -> Option<(usize, Caps)>, ok: &dyn Fn(&str, usize) -> bool| -> Option<Vec<Option<String>>> {
        let found = matches(text.as_str(), at).find(|&(_, e, _)| ok(text, e));
        let (s, e, caps) = found?;
        let caps = std::iter::once(Some(text[s..e].to_string())).chain(caps[1..].iter().map(|c| c.map(|(a, b)| text[a..b].to_string()))).collect();
        *text = format!("{} {}", &text[..s], &text[e..]).trim().to_string();
        Some(caps)
    };
    let any = |_: &str, _: usize| true;
    let num = |c: &Option<String>| c.as_deref().and_then(|s| s.parse::<u32>().ok());

    let mut clock24 = false;
    // Not followed by a letter or a dot: "5 apples" isn't 5am.
    let ampm_end = |t: &str, end: usize| !t[end..].starts_with(|c: char| c.is_ascii_lowercase() || c == '.');
    if let Some(t) = take(&mut text, ampm_at, &ampm_end) {
        let h0 = num(&t[1])?;
        let mins = num(&t[2]).unwrap_or(0);
        if h0 > 12 || mins > 59 {
            return None;
        }
        let mut h = h0 % 12;
        if t[4].as_deref().is_some_and(|s| s.starts_with('p')) {
            h += 12;
        }
        time = Some([h, mins, num(&t[3]).unwrap_or(0)]);
    } else if let Some(t) = take(&mut text, h24_at, &any) {
        time = Some([num(&t[1])?, num(&t[2])?, num(&t[3]).unwrap_or(0)]);
        clock24 = true;
    } else if let Some(t) = take(&mut text, noon_at, &any) {
        time = Some(if t[1].as_deref() == Some("midnight") { [0, 0, 0] } else { [12, 0, 0] });
    } else if let Some(t) = take(&mut text, at_at, &any) {
        let h = num(&t[1])?;
        if h > 23 {
            return None;
        }
        time = Some([h, 0, 0]);
        clock24 = true;
    }
    if let Some(p) = take(&mut text, part_at, &any) {
        let part = p[1].as_deref().unwrap_or_default();
        match time.as_mut() {
            None => time = Some([match part { "morning" => 9, "afternoon" => 15, "evening" => 18, _ => 20 }, 0, 0]),
            Some(t) if clock24 && part != "morning" && t[0] < 12 => t[0] += 12,
            _ => {}
        }
    }

    if let Some(d) = take(&mut text, iso_at, &any) {
        date = NaiveDate::from_ymd_opt(d[1].as_deref()?.parse().ok()?, num(&d[2])?, num(&d[3])?)?;
        have_date = true;
    } else if let Some(d) = take(&mut text, slash_at, &any) {
        let (month, day) = (num(&d[1])?, num(&d[2])?);
        let year = match d[3].as_deref() {
            Some(y) if y.len() == 2 => 2000 + y.parse::<i32>().ok()?,
            Some(y) => y.parse().ok()?,
            None => date.year(),
        };
        date = NaiveDate::from_ymd_opt(year, month, day)?;
        yearless = d[3].is_none();
        have_date = true;
    } else {
        let found = take(&mut text, md_at, &any).map(|f| (f[1].clone(), f[2].clone(), f[3].clone())).or_else(|| take(&mut text, dm_at, &any).map(|f| (f[2].clone(), f[1].clone(), f[3].clone())));
        if let Some((month, day, y)) = found {
            let month = month?;
            let mi = MONTHS.iter().position(|n| n.starts_with(&month[..3]))? as u32;
            let year = match &y {
                Some(y) => y.parse().ok()?,
                None => date.year(),
            };
            date = NaiveDate::from_ymd_opt(year, mi + 1, num(&day)?)?;
            yearless = y.is_none();
            have_date = true;
        }
    }
    if !have_date {
        if let Some(w) = take(&mut text, dayword_at, &any) {
            let word = w[1].as_deref().unwrap_or_default();
            let shift = match word {
                "tomorrow" | "tmrw" | "tmr" => 1,
                "yesterday" => -1,
                _ => 0,
            };
            date += Duration::days(shift);
            if word == "tonight" {
                match time.as_mut() {
                    None => time = Some([20, 0, 0]),
                    Some(t) if clock24 && t[0] < 12 => t[0] += 12,
                    _ => {}
                }
            }
            have_date = true;
        } else if let Some(w) = take(&mut text, step_at, &any) {
            let step = match w[1].as_deref() {
                Some("next") => 1,
                Some("last") => -1,
                _ => 0,
            };
            date = match w[2].as_deref() {
                Some("week") => date + Duration::days(7 * step as i64),
                Some("month") => add_months(date, step)?,
                _ => add_months(date, 12 * step)?,
            };
            have_date = true;
        } else if let Some(w) = take(&mut text, weekday_at, &any) {
            let which = w[1].as_deref();
            let name = w[2].as_deref().unwrap_or_default();
            let weekend = name == "weekend";
            let target = if weekend { 6 } else { DAYS.iter().position(|d| d.starts_with(&name[..3]))? as i64 };
            let today = date.weekday().num_days_from_sunday() as i64;
            let ahead = if which == Some("last") {
                -(match (today - target + 7) % 7 {
                    0 => 7,
                    n => n,
                })
            } else if weekend && today == 0 && which != Some("next") {
                0
            } else {
                let mut a = (target - today + 7) % 7;
                if which == Some("next") {
                    if a == 0 {
                        a = 7;
                    }
                    // The week starts on Sunday here: "next friday" is in next week.
                    let to_week = match (0 - today + 7) % 7 {
                        0 => 7,
                        n => n,
                    };
                    if a < to_week {
                        a += 7;
                    }
                }
                a
            };
            date += Duration::days(ahead);
            have_date = true;
        }
    }
    let aton = |t: &str, p: usize| (boundary(t, p)).then(|| word_of(t, p, &["at", "on"])).flatten().map(|(_, e)| (e, ()));
    if !only_matches(&text, matches(&text, aton)) {
        return None;
    }
    if !have_date && time.is_none() {
        return None;
    }
    if yearless && date < now.date_naive() {
        date = NaiveDate::from_ymd_opt(date.year() + 1, date.month(), date.day())?;
    }
    match time {
        Some([h, m, s]) => Some(Parsed { unix: local_at(date, h, m, s)?, date_only: false }),
        None => Some(Parsed { unix: local_at(date, 12, 0, 0)?, date_only: true }),
    }
}

// ---- suggesting a time as it's typed ----

pub struct Suggestion {
    /// Byte offsets of the phrase in the text before the caret.
    pub start: usize,
    pub end: usize,
    pub unix: i64,
    pub style: char,
}

fn suggest_unit(word: &str, first: bool, spelled: bool) -> Option<&'static str> {
    let word = word.to_lowercase();
    if spelled && word.len() < 2 {
        return None;
    }
    if word == "m" {
        return (!first).then_some("minutes");
    }
    let alias = match word.as_str() {
        "min" | "mins" => Some("minutes"),
        "hr" | "hrs" => Some("hours"),
        "wk" | "wks" => Some("weeks"),
        "yr" | "yrs" => Some("years"),
        _ => None,
    };
    if alias.is_some() {
        return alias;
    }
    for (unit, least) in [("minutes", 2), ("hours", 1), ("days", 1), ("weeks", 1), ("months", 3), ("years", 1)] {
        if word.len() >= least && unit.starts_with(&word) {
            return Some(unit);
        }
        if word == unit[..unit.len() - 1] {
            return Some(unit);
        }
    }
    None
}

// The draft's phrases, as `text-color.js` spells them, read as sets of where each can end:
// a phrase is anchored at both ends, so every way through has to be kept.

/// `(?:\d+(?:\.\d+)?|an?|half an?) ?[a-z]+(?:,?(?: and)? \d+ ?[a-z]+)?`
fn g_span(t: &str, s: At) -> At {
    let num = s.digits(t, 1, usize::MAX);
    let amount = num.opt(|n| n.li(t, ".").digits(t, 1, usize::MAX)).or(s.any_li(t, &["a", "an", "half a", "half an"]));
    let unit = amount.opt(|a| a.li(t, " ")).class(t, 1, usize::MAX, fold_letter);
    unit.or(unit.opt(|u| u.li(t, ",")).opt(|u| u.li(t, " and")).li(t, " ").digits(t, 1, usize::MAX).opt(|x| x.li(t, " ")).class(t, 1, usize::MAX, fold_letter))
}

/// `jan(?:uary)?|…|sep(?:t(?:ember)?)?|…`
fn g_month(t: &str, s: At) -> At {
    let mut out = s.any_li(t, &["may"]);
    for (short, long) in [("jan", "uary"), ("feb", "ruary"), ("mar", "ch"), ("apr", "il"), ("jun", "e"), ("jul", "y"), ("aug", "ust"), ("oct", "ober"), ("nov", "ember"), ("dec", "ember")] {
        out = out.or(s.li(t, short).opt(|x| x.li(t, long)));
    }
    out.or(s.li(t, "sep").opt(|x| x.li(t, "t").opt(|y| y.li(t, "ember"))))
}

/// `\d{1,2}(st|nd|rd|th)?`
fn g_day(t: &str, s: At) -> At {
    s.digits(t, 1, 2).opt(|d| d.any_li(t, &["st", "nd", "rd", "th"]))
}

/// `(,? \d{4})?`
fn g_year(t: &str, s: At) -> At {
    s.opt(|x| x.opt(|y| y.li(t, ",")).li(t, " ").digits(t, 4, 4))
}

/// `\d{1,2}(?::\d{2})? ?` before an am/pm.
fn g_hour(t: &str, s: At) -> At {
    s.digits(t, 1, 2).opt(|h| h.li(t, ":").digits(t, 2, 2)).opt(|h| h.li(t, " "))
}

/// `\d{1,2}:\d{2}`
fn g_clock24(t: &str, s: At) -> At {
    s.digits(t, 1, 2).li(t, ":").digits(t, 2, 2)
}

/// `(,? (?:at )?(?:\d{1,2}(?::\d{2})? ?(?:am|pm)|\d{1,2}:\d{2}))?`
fn g_clock(t: &str, s: At) -> At {
    s.opt(|x| {
        let x = x.opt(|y| y.li(t, ",")).li(t, " ").opt(|y| y.li(t, "at "));
        g_hour(t, x).any_li(t, &["am", "pm"]).or(g_clock24(t, x))
    })
}

/// `\d{1,2}(?::\d{2})? ?(?:am|pm|a\.m\.|p\.m\.)|\d{1,2}:\d{2}`
fn g_time(t: &str, s: At) -> At {
    g_hour(t, s).any_li(t, &["am", "pm", "a.m.", "p.m."]).or(g_clock24(t, s))
}

/// `today|tonight|tomorrow|tmrw|tmr|(?:(?:this|next|coming) )?(?:sun|mon|tues?|wed(?:nes)?|thu(?:rs)?|fri|sat(?:ur)?)day`
fn g_dayword(t: &str, s: At) -> At {
    let named = s.opt(|x| x.any_li(t, &["this ", "next ", "coming "]));
    let named = named.any_li(t, &["sun", "mon", "tue", "tues", "wed", "wednes", "thu", "thurs", "fri", "sat", "satur"]).li(t, "day");
    s.any_li(t, &["today", "tonight", "tomorrow", "tmrw", "tmr"]).or(named)
}

/// Each draft form: what may sit between the delimiter and the phrase, and the phrase.
type Grammar = fn(&str, At) -> At;
struct Form {
    lead: &'static str,
    phrase: Grammar,
    /// What the phrase's last char can be.
    last: fn(char) -> bool,
    relative: bool,
    alone: bool,
    /// What the lead or phrase can open with.
    first: fn(char) -> bool,
    /// The most chars the lead, phrase and closing mark can span, when bounded.
    span: Option<usize>,
}

/// `c` lowercased is in `set`, LONG S reading as `s`.
fn opens(c: char, set: &str) -> bool {
    set.contains(if c == '\u{17F}' { 's' } else { c.to_ascii_lowercase() })
}

const FORMS: [Form; 7] = [
    Form {
        lead: "",
        phrase: |t, s| {
            let ahead = g_span(t, s.li(t, "in "));
            // Only a draft that has "from now" in it can end in it; neither word folds past ASCII.
            let has_from = t.as_bytes().windows(8).any(|w| w.eq_ignore_ascii_case(b"from now"));
            if has_from { ahead.or(g_span(t, s).li(t, " from now")) } else { ahead }
        },
        last: fold_letter, relative: true,
        alone: false, first: |c| digit(c) || opens(c, "ahi"), span: None,
    },
    Form { lead: "", phrase: |t, s| g_clock(t, g_year(t, g_day(t, g_month(t, s).opt(|m| m.li(t, ".")).li(t, " ")))), last: |c| digit(c) || matches!(c.to_ascii_lowercase(), 't' | 'd' | 'h' | 'm'), relative: false, alone: false, first: |c| opens(c, "jfmasond"), span: Some(40) },
    Form { lead: "the ", phrase: |t, s| g_clock(t, g_year(t, g_month(t, g_day(t, s).li(t, " ").opt(|x| x.li(t, "of "))))), last: |c| digit(c) || matches!(c.to_ascii_lowercase(), 'n' | 'y' | 'b' | 'r' | 'h' | 'l' | 'e' | 'g' | 't' | 'p' | 'v' | 'c' | 'm'), relative: false, alone: false, first: |c| digit(c) || opens(c, "t"), span: Some(48) },
    Form {
        lead: "",
        phrase: |t, s| {
            let d = s.digits(t, 4, 4).lit(t, "-").digits(t, 2, 2).lit(t, "-").digits(t, 2, 2);
            d.opt(|x| x.class(t, 1, 1, |c| c == ' ' || c == 'T').digits(t, 1, 2).lit(t, ":").digits(t, 2, 2))
        },
        last: digit, relative: false,
        alone: false, first: digit, span: Some(24),
    },
    Form {
        lead: "on ",
        phrase: |t, s| {
            let d = g_dayword(t, s);
            g_time(t, d.opt(|x| x.li(t, ",")).li(t, " ").opt(|x| x.li(t, "at "))).or(d.li(t, " at ").digits(t, 1, 2))
        },
        last: |c| digit(c) || matches!(c, 'm' | 'M' | '.'), relative: false,
        alone: false, first: |c| opens(c, "otncsmwf"), span: Some(48),
    },
    Form {
        lead: "",
        phrase: |t, s| g_dayword(t, g_time(t, s.opt(|x| x.li(t, "at "))).opt(|x| x.li(t, ",")).li(t, " ").opt(|x| x.li(t, "on "))),
        last: |c| matches!(c.to_ascii_lowercase(), 'y' | 't' | 'w' | 'r'), relative: false,
        alone: false, first: |c| digit(c) || opens(c, "a"), span: Some(48),
    },
    Form {
        lead: "",
        phrase: |t, s| g_hour(t, s.opt(|x| x.li(t, "at "))).any_li(t, &["am", "pm", "a.m.", "p.m."]).or(g_clock24(t, s.li(t, "at "))),
        last: |c| digit(c) || matches!(c, 'm' | 'M' | '.'), relative: false,
        alone: true, first: |c| digit(c) || opens(c, "a"), span: Some(24),
    },
];

/// `(?:^|[\s(])(?:{lead})?({phrase})[.,!?;:)]? ?$`: the phrase's start and end.
fn form_at(form: &Form, t: &str) -> Option<(usize, usize)> {
    let mut ends = At::default();
    for e in t.len().saturating_sub(2)..=t.len() {
        let fits = t.get(e..).is_some_and(|r| {
            let r = r.strip_suffix(' ').unwrap_or(r);
            r.is_empty() || (r.len() == 1 && ".,!?;:)".contains(r))
        });
        if fits {
            ends.add(e);
        }
    }
    if !ends.iter().any(|e| prev(t, e).is_some_and(form.last)) {
        return None;
    }
    let leads = |s: At| if form.lead.is_empty() { s } else { s.or(s.li(t, form.lead)) };
    // Too far from the end for a bounded form to reach it.
    let near = form.span.map_or(0, |n| t.char_indices().rev().nth(n).map_or(0, |(i, _)| i));
    let can = |p: usize| p >= near && next(t, p).is_some_and(form.first);
    let mut starts = [0usize; 81];
    let mut count = 0;
    if can(0) {
        starts[0] = 0;
        count = 1;
    }
    for (i, c) in t.char_indices().filter(|&(_, c)| c.is_whitespace() || c == '(') {
        if count < starts.len() && can(i + c.len_utf8()) {
            starts[count] = i + c.len_utf8();
            count += 1;
        }
    }
    let starts = &starts[..count];
    if starts.is_empty() {
        return None;
    }
    let reaches = |n: usize| {
        let mut s = At::default();
        starts[..n].iter().for_each(|&p| s.add(p));
        (form.phrase)(t, leads(s)).meets(&ends)
    };
    // All starts at once, then halving to the leftmost that reaches an end.
    if !reaches(starts.len()) {
        return None;
    }
    let (mut lo, mut hi) = (0, starts.len());
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if reaches(mid) { hi = mid } else { lo = mid }
    }
    let p = starts[lo];
    let leads = [(!form.lead.is_empty()).then(|| lit_i(t, p, form.lead)).flatten(), Some(p)];
    leads.into_iter().flatten().find_map(|ps| {
        let reach = (form.phrase)(t, At::of(ps));
        let mut best = None;
        for e in reach.iter() {
            if ends.has(e) {
                best = Some(e);
            }
        }
        best.map(|e| (ps, e))
    })
}

/// `(?i)^(\d+\w* (of )?)?may\b`
fn opens_with_may(t: &str) -> bool {
    let s = At::of(0);
    let lead = s.digits(t, 1, usize::MAX).class(t, 0, usize::MAX, word).li(t, " ").opt(|x| x.li(t, "of "));
    s.or(lead).li(t, "may").iter().any(|e| boundary(t, e))
}

/// `(?i)\d(st|nd|rd|th)\b`
fn has_ordinal(t: &str) -> bool {
    At::all(t).digits(t, 1, 1).any_li(t, &["st", "nd", "rd", "th"]).iter().any(|e| boundary(t, e))
}

/// Whether `(?i)(,? \d{4})?(,? (?:at )?…)?$` takes a year or a clock off the phrase's end.
fn ends_dated(t: &str) -> bool {
    At::all(t).iter().filter(|&p| p < t.len()).any(|p| g_clock(t, g_year(t, At::of(p))).has(t.len()))
}

/// `(?i)(?:day|night|week|month|year|\d)[,.]? *$`
fn after_amount(t: &str) -> bool {
    let all = At::all(t);
    let x = all.any_li(t, &["day", "night", "week", "month", "year"]).or(all.digits(t, 1, 1));
    x.opt(|y| y.class(t, 1, 1, |c| c == ',' || c == '.')).class(t, 0, usize::MAX, |c| c == ' ').has(t.len())
}

/// `(?i)\b(?:(\d+(?:\.\d+)?) ?|(half an?|an?) )([a-z]+)`: the number, the spelled amount, the unit.
fn amount_at(t: &str, p: usize) -> Option<(usize, [Span; 3])> {
    if !boundary(t, p) {
        return None;
    }
    let units = |from: usize| {
        let (n, e) = run(t, from, usize::MAX, fold_letter);
        (n > 0).then_some(e)
    };
    let (n, mut e) = run(t, p, usize::MAX, digit);
    if n > 0 {
        if let Some(f) = lit(t, e, ".") {
            let (m, fe) = run(t, f, usize::MAX, digit);
            if m > 0 {
                e = fe;
            }
        }
        let u = lit(t, e, " ").unwrap_or(e);
        return units(u).map(|end| (end, [Some((p, e)), None, Some((u, end))]));
    }
    ["half an ", "half a ", "an ", "a "].iter().find_map(|w| {
        let u = lit_i(t, p, w)?;
        units(u).map(|end| (end, [None, Some((p, u - 1)), Some((u, end))]))
    })
}

/// A time phrase the draft ends with, worth offering as a time everyone reads in their own
/// zone (`ttSuggest`): "in 6 hours" a countdown, a date a date, a date with a time both.
pub fn suggest(before: &str, now: DateTime<Local>) -> Option<Suggestion> {
    if before.len() < 24 && before.chars().count() < 6 {
        return None;
    }
    let skip = before.char_indices().rev().nth(79).map_or(0, |(i, _)| i);
    let tail = &before[skip..];
    // Every form but the countdown has a digit at most 28 chars from its end ("5 p.m., on
    // coming wednesday" and its closing mark).
    let dated = tail.chars().rev().take(32).any(digit);
    // The countdown needs its "in " or "from now"; neither folds past ASCII.
    let counts = tail.as_bytes().windows(3).any(|w| w.eq_ignore_ascii_case(b"in "))
        || tail.as_bytes().windows(8).any(|w| w.eq_ignore_ascii_case(b"from now"));
    for form in FORMS.iter().filter(|f| if f.relative { counts } else { dated }) {
        let Some((ps, pe)) = form_at(form, tail) else { continue };
        let phrase = &tail[ps..pe];
        if !form.relative && opens_with_may(phrase) && !has_ordinal(phrase) && !ends_dated(phrase) {
            return None;
        }
        let start = skip + ps;
        let end = start + phrase.len();
        if form.alone {
            let from = before[..start].char_indices().rev().nth(23).map_or(0, |(i, _)| i);
            if after_amount(&before[from..start]) {
                return None;
            }
        }
        if vector_core::text_color::code_ranges(before).iter().any(|&(s, e)| start >= s && start < e) {
            return None;
        }
        let mut read = phrase.to_string();
        if form.relative {
            let mut first = true;
            read.clear();
            let mut last = 0;
            for (s, e, [num, spelled, unit]) in matches(phrase, amount_at) {
                let unit = suggest_unit(span(phrase, unit), first, num.is_none())?;
                first = false;
                read += &phrase[last..s];
                read += span(phrase, num.or(spelled));
                read.push(' ');
                read += unit;
                last = e;
            }
            read += &phrase[last..];
        }
        let parsed = parse_full(&read, now)?;
        let ahead = parsed.unix as f64 - now.timestamp_millis() as f64 / 1000.;
        if ahead < if form.relative { 590. } else { 60. } {
            return None;
        }
        let style = if form.relative {
            'R'
        } else if parsed.date_only {
            'D'
        } else {
            'f'
        };
        return Some(Suggestion { start, end, unix: parsed.unix, style });
    }
    None
}

/// `<t:(-?\d{1,12})(?::([tTdDfFsSR]))?>` at `p`: the number and the style.
fn token_at(t: &str, p: usize) -> Option<(usize, (Span, Option<char>))> {
    let q = lit(t, p, "<t:")?;
    let d = lit(t, q, "-").unwrap_or(q);
    let (n, de) = run(t, d, 12, digit);
    if n == 0 {
        return None;
    }
    let styled = lit(t, de, ":").and_then(|c| next(t, c).filter(|s| STYLES.contains(*s)).map(|s| (c + 1, s)));
    if let Some(end) = styled.and_then(|(c, _)| lit(t, c, ">")) {
        return Some((end, (Some((q, de)), styled.map(|(_, s)| s))));
    }
    lit(t, de, ">").map(|end| (end, (Some((q, de)), None)))
}

/// `<t:UNIX>` and `<t:UNIX:STYLE>` outside code: (byte start, byte end, unix, style).
pub fn codes(src: &str) -> Vec<(usize, usize, i64, char)> {
    static OPEN: std::sync::LazyLock<memchr::memmem::Finder<'static>> = std::sync::LazyLock::new(|| memchr::memmem::Finder::new("<t:"));
    let Some(first) = OPEN.find(src.as_bytes()) else { return Vec::new() };
    let code = vector_core::text_color::code_ranges(src);
    let tokens = crate::scan::matches_from(&src[first..], crate::scan::lead(b"<"), token_at);
    tokens
        .filter_map(|(s, e, (num, style))| {
            let (s, e) = (first + s, first + e);
            if code.iter().any(|&(a, b)| s >= a && s < b) {
                return None;
            }
            let unix: i64 = span(&src[first..], num).parse().ok()?;
            vector_core::text_time::fallback(unix)?;
            Some((s, e, unix, style.unwrap_or('f')))
        })
        .take(64)
        .collect()
}


// ---- the composer's @time picker and suggestion chip ----

/// The `@time` (or `/time `) the caret is in: where it starts and what's typed after it.
pub fn find(text: &str, caret: usize) -> Option<(usize, &str)> {
    let before = text.get(..caret)?;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let line = &before[line_start..];
    let lower = line.to_ascii_lowercase();
    let mut at = None;
    for (i, _) in lower.match_indices("time") {
        let Some(sigil) = i.checked_sub(1) else { continue };
        let sig = lower.as_bytes()[sigil];
        if sig != b'@' && sig != b'/' {
            continue;
        }
        if !lower[..sigil].chars().next_back().is_none_or(char::is_whitespace) {
            continue;
        }
        // The slash form only opens past its space.
        match lower[i + 4..].chars().next() {
            Some(c) if c.is_whitespace() => {}
            None if sig == b'@' => {}
            _ => continue,
        }
        at = Some(line_start + sigil);
    }
    let at = at?;
    let query = &before[at + 5..];
    (query.chars().count() <= 60).then_some((at, query))
}

pub struct Picker {
    pub at: usize,
    pub query: String,
    pub parsed: Option<Parsed>,
    pub active: usize,
}

impl Picker {
    pub fn styles(&self) -> &'static [char] {
        match &self.parsed {
            None => &[],
            Some(p) if p.date_only => &['D', 'R'],
            Some(_) => &['f', 'R', 't', 'D'],
        }
    }
}

/// The picker for the draft, keeping the highlighted row while the `@time` stays put.
pub fn picker(text: &str, caret: usize, now: DateTime<Local>, was: Option<&Picker>) -> Option<Picker> {
    let (at, query) = find(text, caret)?;
    let mut p = Picker {
        at,
        query: query.trim().to_string(),
        parsed: parse_full(query, now),
        active: was.filter(|w| w.at == at).map_or(0, |w| w.active),
    };
    if p.active >= p.styles().len() {
        p.active = 0;
    }
    Some(p)
}

fn style_name(style: char, unix: i64, now: f64) -> &'static str {
    match style {
        'R' if unix as f64 >= now => "Countdown",
        'R' => "Time since",
        't' => "Time",
        'D' => "Date",
        _ => "Date and time",
    }
}

fn note(text: String) -> AnyElement {
    div()
        .pt(px(2.))
        .px(px(12.))
        .pb(px(6.))
        .text_size(px(12.))
        .text_color(white_a(0.55))
        .child(text)
        .into_any_element()
}

/// `.time-selector`: each style, previewed for the time typed so far.
pub fn picker_view(p: &Picker, now: f64, pick: Rc<dyn Fn(usize, &mut Window, &mut App)>) -> AnyElement {
    let header = crate::shortcodes::title("Time", (!p.query.is_empty()).then(|| format!("· {}", p.query)));
    let (lead, rows) = match &p.parsed {
        None => (note("Try “tomorrow 5pm”, “next monday”, “dec 25” or “in 2 hours”".into()), Vec::new()),
        Some(t) => (
            note(full(t.unix, t.date_only)),
            p.styles()
                .iter()
                .map(|&style| Row {
                    art: None,
                    label: format(t.unix, style, now).into(),
                    hint: Some(style_name(style, t.unix, now).into()),
                })
                .collect(),
        ),
    };
    crate::shortcodes::panel("time-selector", header, Some(lead), rows, p.active, 342., pick)
}

pub struct Chip {
    pub found: Suggestion,
    pub phrase: String,
}

impl Chip {
    pub fn key(&self) -> String {
        format!("{}:{}", self.found.start, self.phrase)
    }
}

/// `.time-suggest`: the phrase as everyone would read it, taken on Tab or a tap.
pub fn chip_view(chip: &Chip, now: f64, left: f32, accept: Rc<dyn Fn(&mut Window, &mut App)>) -> AnyElement {
    let s = &chip.found;
    let preview = if s.style == 'R' {
        format!("{} · {}", format(s.unix, 'R', now), when_at(s.unix, now))
    } else {
        format(s.unix, s.style, now)
    };
    div()
        .flex()
        .pl(px(left.max(0.)))
        .child(
            div()
                .id("time-suggest")
                .occlude()
                .max_w(px(360.))
                .min_w_0()
                .bg(c(0x141414))
                .border_1()
                .border_color(white_a(0.12))
                .rounded_full()
                .shadow(vec![gpui::BoxShadow {
                    color: ca(0, 0, 0, 0.45),
                    offset: gpui::point(px(0.), px(4.)),
                    blur_radius: px(16.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .flex()
                .items_center()
                .gap(px(7.))
                .pl(px(10.))
                .pr(px(6.))
                .py(px(5.))
                .font_family(INTER)
                .text_size(px(13.))
                .cursor_pointer()
                .tooltip(crate::kit::tooltip(format!(
                    "{}: {}",
                    if s.style == 'R' { "Make it a countdown" } else { "Show in everyone\u{2019}s time" },
                    full(s.unix, s.style == 'D')
                )))
                .group("time-suggest")
                .on_mouse_down(gpui::MouseButton::Left, move |_, w, cx| {
                    cx.stop_propagation();
                    accept(w, cx)
                })
                .child(svg().path("icons/clock.svg").size(px(14.)).flex_shrink_0().text_color(white_a(0.6)))
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(white_a(0.85))
                        .group_hover("time-suggest", |s| s.text_color(gpui::white()))
                        .child(preview),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(10.))
                        .line_height(px(16.))
                        .px(px(7.))
                        .rounded_full()
                        .border_1()
                        .border_color(white_a(0.18))
                        .text_color(white_a(0.5))
                        .child("Tab"),
                )
                .with_animation(
                    gpui::ElementId::NamedInteger("time-suggest".into(), s.start as u64),
                    gpui::Animation::new(std::time::Duration::from_millis(200)).with_easing(crate::kit::bezier(0.4, 0., 0.2, 1.)),
                    |d, t| d.relative().top(px(6. * (1. - t))).opacity((t * 1.33).min(1.)),
                ),
        )
        .into_any_element()
}

/// `content` with each time span's fallback text back as its `<t:UNIX:STYLE>` code, for the
/// renderer to draw as a chip. Spans index code points; one whose text isn't its fallback stays.
pub fn spans_to_codes<'a>(content: &'a str, spans: &[vector_core::text_spans::TextSpan]) -> std::borrow::Cow<'a, str> {
    let mut times: Vec<&vector_core::text_time::TimeSpan> = spans
        .iter()
        .filter_map(|s| match s {
            vector_core::text_spans::TextSpan::Time(t) => Some(t),
            _ => None,
        })
        .collect();
    if times.is_empty() {
        return content.into();
    }
    times.sort_by_key(|t| std::cmp::Reverse(t.from));
    let bytes: Vec<usize> = content.char_indices().map(|(i, _)| i).chain([content.len()]).collect();
    let code = vector_core::text_color::code_ranges(content);
    let mut out = content.to_string();
    for t in times {
        let (Some(&a), Some(&b)) = (bytes.get(t.from as usize), bytes.get(t.to as usize)) else { continue };
        if a >= b || code.iter().any(|&(s, e)| a >= s && a < e) {
            continue;
        }
        if vector_core::text_time::fallback(t.unix).as_deref() != Some(&content[a..b]) {
            continue;
        }
        out.replace_range(a..b, &format!("<t:{}:{}>", t.unix, t.style));
    }
    out.into()
}

/// `content` with each time span shown as the reader would see it, for one-line previews.
pub fn localize<'a>(content: &'a str, spans: &[vector_core::text_spans::TextSpan], now: f64) -> std::borrow::Cow<'a, str> {
    let coded = spans_to_codes(content, spans);
    if !coded.contains("<t:") {
        return coded;
    }
    let mut out = String::with_capacity(coded.len());
    let mut rest: &str = &coded;
    while let Some(at) = rest.find("<t:") {
        out.push_str(&rest[..at]);
        match code_at(&rest[at..]) {
            Some((len, unix, style)) => {
                out.push_str(&format(unix, style, now));
                rest = &rest[at + len..];
            }
            None => {
                out.push_str("<t:");
                rest = &rest[at + 3..];
            }
        }
    }
    out.push_str(rest);
    out.into()
}

/// A `<t:…>` code at the start of `s`: its length, time and style.
pub fn code_at(s: &str) -> Option<(usize, i64, char)> {
    let rest = s.strip_prefix("<t:")?;
    let digits = rest.strip_prefix('-').unwrap_or(rest);
    let n = digits.bytes().take_while(u8::is_ascii_digit).count();
    if n == 0 || n > 12 {
        return None;
    }
    let num_len = rest.len() - digits.len() + n;
    let unix: i64 = rest[..num_len].parse().ok()?;
    let tail = &rest[num_len..];
    let (style, len) = match tail.as_bytes() {
        [b'>', ..] => ('f', 1),
        [b':', st, b'>', ..] if STYLES.as_bytes().contains(st) => (*st as char, 3),
        _ => return None,
    };
    fallback_ok(unix).then_some((3 + num_len + len, unix, style))
}

fn fallback_ok(unix: i64) -> bool {
    vector_core::text_time::fallback(unix).is_some()
}

#[cfg(test)]
#[path = "testdata/texttime_regex.rs"]
mod regex_reference;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn scanners_read_as_the_regexes_did() {
        const TOKENS: &[&str] = &[
            "in", "at", "on", "the", "of", "tomorrow", "tonight", "today", "tmr", "tmrw", "yesterday", "next", "last",
            "this", "coming", "week", "month", "year", "weekend", "monday", "mon", "tues", "tue", "tuesday", "thurs",
            "thu", "thur", "fri", "friday", "sat", "saturday", "wed", "wednesday", "sun", "jan", "january", "may", "sep",
            "sept", "september", "dec", "december", "mar", "march", "jun", "june", "morning", "afternoon", "evening",
            "night", "noon", "midnight", "midday", "am", "pm", "a.m.", "p.m.", "a", "p", "half", "an", "and", "hours",
            "h", "hr", "min", "m", "mins", "minutes", "minu", "days", "d", "wks", "from now", "ago", "1", "5", "12", "23",
            "25", "123", "3rd", "1st", "2026", "2027", "20261", "10:30", "5:30", "23:59", "12:30:45", "1:2", "2026-12-01",
            "2026-1-2", "12/25", "1/2/27", "3.4.2026", "1.5", "0.25", ",", ".", "!", "?", ")", "(", "-", "٣", "\u{212A}",
            "\u{17F}", "é", "Tomorrow", "MAY", "5PM", "x", "apples", "<t:", "<t:1700000000>", "<t:1700000000:R>",
            "<t:-5:x>", ":", "T", "2h", "30m", "5pm", "8pm", "7am", "\t", "`", "now", "1700000000", "the 5th",
        ];
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let now = noon();
        let parsed = |p: Option<Parsed>| p.map(|p| (p.unix, p.date_only));
        let suggested = |s: Option<Suggestion>| s.map(|s| (s.start, s.end, s.unix, s.style));
        for _ in 0..30_000 {
            let mut text = String::new();
            for _ in 0..1 + next() % 8 {
                if !text.is_empty() && next() % 5 != 0 {
                    text.push(' ');
                }
                text += TOKENS[(next() % TOKENS.len() as u64) as usize];
            }
            assert_eq!(parsed(parse_full(&text, now)), parsed(regex_reference::parse_full(&text, now)), "read {text:?}");
            assert_eq!(duration(&text), regex_reference::duration(&text), "duration {text:?}");
            let draft = format!("see you {text}");
            assert_eq!(suggested(suggest(&draft, now)), suggested(regex_reference::suggest(&draft, now)), "suggest {draft:?}");
            assert_eq!(suggested(suggest(&text, now)), suggested(regex_reference::suggest(&text, now)), "suggest {text:?}");
            assert_eq!(codes(&text), regex_reference::codes(&text), "codes {text:?}");
        }
    }

    fn noon() -> DateTime<Local> {
        Local.from_local_datetime(&NaiveDate::from_ymd_opt(2026, 11, 11).unwrap().and_hms_opt(12, 0, 0).unwrap()).unwrap()
    }

    fn when(s: &str) -> Option<(NaiveDate, u32, u32, bool)> {
        let p = parse_full(s, noon())?;
        let t = Local.timestamp_opt(p.unix, 0).unwrap();
        Some((t.date_naive(), t.hour(), t.minute(), p.date_only))
    }

    #[test]
    fn phrases_read_as_times() {
        let d = |m, d| NaiveDate::from_ymd_opt(2026, m, d).unwrap();
        assert_eq!(when("tomorrow 5pm"), Some((d(11, 12), 17, 0, false)));
        assert_eq!(when("friday 9:30"), Some((d(11, 13), 9, 30, false)));
        assert_eq!(when("next monday"), Some((d(11, 16), 12, 0, true)));
        assert_eq!(when("tonight at 8"), Some((d(11, 11), 20, 0, false)));
        assert_eq!(when("dec 25"), Some((d(12, 25), 12, 0, true)));
        assert_eq!(when("25th of december at 7pm"), Some((d(12, 25), 19, 0, false)));
        assert_eq!(when("jan 3").map(|w| w.0.year()), Some(2027));
        assert_eq!(when("5 apples"), None);
        assert_eq!(when("feb 30"), None);
        assert!(when("in 2 hours").is_some_and(|w| w.1 == 14));
    }

    #[test]
    fn a_draft_offers_its_trailing_time() {
        let s = suggest("see you in 6 hours", noon()).unwrap();
        assert_eq!((s.start, s.style), (8, 'R'));
        assert!(suggest("see you tomorrow", noon()).is_none());
        assert_eq!(suggest("party on dec 25 at 8pm", noon()).map(|s| s.style), Some('f'));
        assert!(suggest("i may 5", noon()).is_none());
        assert!(suggest("in 10 minu", noon()).is_some());
    }

    #[test]
    fn at_time_opens_at_a_word_start() {
        assert_eq!(find("@time", 5), Some((0, "")));
        assert_eq!(find("see @time tomorrow", 18), Some((4, " tomorrow")));
        assert_eq!(find("x@time 5pm", 10), None);
        assert_eq!(find("/time", 5), None);
        assert_eq!(find("/time 5pm", 9), Some((0, " 5pm")));
        assert_eq!(find("@timer", 6), None);
    }

    #[test]
    fn codes_read_from_the_text() {
        assert_eq!(code_at("<t:100:R> x"), Some((9, 100, 'R')));
        assert_eq!(code_at("<t:-5>"), Some((6, -5, 'f')));
        assert_eq!(code_at("<t:5:x>"), None);
        assert_eq!(code_at("<t:>"), None);
    }

    #[test]
    fn relative_counts_by_the_growing_unit() {
        assert_eq!(relative(3600 * 2, 0.), "in 2 hours");
        assert_eq!(relative(-86_400 * 3, 0.), "3 days ago");
        assert_eq!(relative(1, 0.), "in 1 second");
    }
}
