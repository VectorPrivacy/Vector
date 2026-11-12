//! Times that read the same everywhere (`text-time.js`): `<t:UNIX:STYLE>` shown in the
//! reader's zone, a typed phrase read as a time, and a draft's trailing phrase offered as one.

use std::sync::LazyLock;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone};
use regex::{Captures, Regex};
use std::rc::Rc;

use gpui::{
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

const UNITS: &[(&str, f64)] = &[
    ("^(s|secs?|seconds?)$", 1.),
    ("^(m|mins?|minutes?)$", 60.),
    ("^(h|hrs?|hours?)$", 3600.),
    ("^(d|days?)$", 86_400.),
    ("^(w|wks?|weeks?)$", 604_800.),
    ("^(mo|mos|months?)$", 2_592_000.),
    ("^(y|yrs?|years?)$", 31_536_000.),
];
const DAYS: [&str; 7] = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];
const MONTHS: [&str; 12] = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];

fn re(s: &str) -> Regex {
    Regex::new(s).expect("static pattern")
}

static UNIT_RES: LazyLock<Vec<(Regex, f64)>> = LazyLock::new(|| UNITS.iter().map(|(p, n)| (re(p), *n)).collect());
static DUR: LazyLock<Regex> = LazyLock::new(|| re(r"(\d+(?:\.\d+)?|half an?|an?|one|half) ([a-z]+)"));

/// "2h 30m", "an hour", "3 days and 4 hours": seconds, or None when any word isn't one.
pub fn duration(text: &str) -> Option<f64> {
    static AND: LazyLock<Regex> = LazyLock::new(|| re(r"\band\b"));
    static DL: LazyLock<Regex> = LazyLock::new(|| re(r"(\d)([a-z])"));
    static LD: LazyLock<Regex> = LazyLock::new(|| re(r"([a-z])(\d)"));
    static WS: LazyLock<Regex> = LazyLock::new(|| re(r"\s+"));
    let clean = AND.replace_all(text, " ");
    let clean = DL.replace_all(&clean, "$1 $2");
    let clean = LD.replace_all(&clean, "$1 $2");
    let clean = WS.replace_all(&clean, " ").trim().to_string();
    let mut total = 0.;
    for m in DUR.captures_iter(&clean) {
        let a = &m[1];
        let amount = if a.starts_with(|c: char| c.is_ascii_digit()) {
            a.parse::<f64>().ok()?
        } else if a.starts_with("half") {
            0.5
        } else {
            1.
        };
        let unit = UNIT_RES.iter().find(|(u, _)| u.is_match(&m[2]))?;
        total += amount * unit.1;
    }
    (total != 0. && DUR.replace_all(&clean, "").trim().is_empty()).then_some(total)
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

fn read(input: &str, now: DateTime<Local>) -> Option<Parsed> {
    static WS: LazyLock<Regex> = LazyLock::new(|| re(r"\s+"));
    static EPOCH: LazyLock<Regex> = LazyLock::new(|| re(r"^-?\d{9,13}$"));
    static IN: LazyLock<Regex> = LazyLock::new(|| re(r"^in (.+)$"));
    static AGO: LazyLock<Regex> = LazyLock::new(|| re(r"^(.+) (ago|from now)$"));
    static AMPM: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:at )?(\d{1,2})(?::(\d{2}))?(?::(\d{2}))? ?(am|pm|a\.m\.|p\.m\.|a|p)"));
    static H24: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:at )?([01]?\d|2[0-3]):([0-5]\d)(?::([0-5]\d))?\b"));
    static NOON: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:at )?(noon|midday|midnight)\b"));
    static AT: LazyLock<Regex> = LazyLock::new(|| re(r"\bat (\d{1,2})\b"));
    static PART: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:in the |this )?(morning|afternoon|evening|night)\b"));
    static ISO: LazyLock<Regex> = LazyLock::new(|| re(r"\b(\d{4})-(\d{1,2})-(\d{1,2})\b"));
    static SLASH: LazyLock<Regex> = LazyLock::new(|| re(r"\b(\d{1,2})[/.](\d{1,2})(?:[/.](\d{2,4}))?\b"));
    static MONTH: LazyLock<String> = LazyLock::new(|| {
        format!("({})\\.?", MONTHS.iter().map(|n| format!("{}(?:{})?", &n[..3], &n[3..])).collect::<Vec<_>>().join("|"))
    });
    static MD: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"\b{} (\d{{1,2}})(?:st|nd|rd|th)?(?: (\d{{4}}))?\b", *MONTH)));
    static DM: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"\b(\d{{1,2}})(?:st|nd|rd|th)? (?:of )?{}(?: (\d{{4}}))?\b", *MONTH)));
    static DAYWORD: LazyLock<Regex> = LazyLock::new(|| re(r"\b(today|tonight|tomorrow|tmrw|tmr|yesterday)\b"));
    static STEP: LazyLock<Regex> = LazyLock::new(|| re(r"\b(next|last|this) (week|month|year)\b"));
    static WEEKDAY: LazyLock<Regex> = LazyLock::new(|| {
        re(r"\b(?:(next|this|last|on|coming) )?(?:the )?(weekend|sun(?:day)?|mon(?:day)?|tue(?:s|sday)?|wed(?:nesday)?|thu(?:r|rs|rsday)?|fri(?:day)?|sat(?:urday)?)\b")
    });
    static ATON: LazyLock<Regex> = LazyLock::new(|| re(r"\b(at|on)\b"));

    let lower = input.to_lowercase().replace(',', " ");
    let mut text = WS.replace_all(lower.trim(), " ").to_string();
    let now_s = now.timestamp();
    if text.is_empty() || text == "now" {
        return Some(Parsed { unix: now_s.div_euclid(60) * 60, date_only: false });
    }
    if EPOCH.is_match(&text) {
        let n: i64 = text.parse().ok()?;
        let unix = if text.trim_start_matches('-').len() == 13 { n / 1000 } else { n };
        return Some(Parsed { unix, date_only: false });
    }
    let now_f = now.timestamp_millis() as f64 / 1000.;
    if let Some(m) = IN.captures(&text) {
        let d = duration(&m[1])?;
        return Some(Parsed { unix: (now_f + d).round() as i64, date_only: false });
    }
    if let Some(m) = AGO.captures(&text) {
        let d = duration(&m[1])?;
        let d = if &m[2] == "ago" { -d } else { d };
        return Some(Parsed { unix: (now_f + d).round() as i64, date_only: false });
    }

    let mut date = now.date_naive();
    let mut have_date = false;
    let mut yearless = false;
    let mut time: Option<[u32; 3]> = None;
    // Takes the first match `ok` accepts out of the text, as its captures' strings.
    let take = |text: &mut String, r: &Regex, ok: &dyn Fn(&str, usize) -> bool| -> Option<Vec<Option<String>>> {
        let found = r.captures_iter(text).find(|c| {
            let m = c.get(0).expect("whole match");
            ok(text, m.end())
        })?;
        let m = found.get(0).expect("whole match");
        let caps = (0..found.len()).map(|i| found.get(i).map(|g| g.as_str().to_string())).collect();
        *text = format!("{} {}", &text[..m.start()], &text[m.end()..]).trim().to_string();
        Some(caps)
    };
    let any = |_: &str, _: usize| true;
    let num = |c: &Option<String>| c.as_deref().and_then(|s| s.parse::<u32>().ok());

    let mut clock24 = false;
    // Not followed by a letter or a dot: "5 apples" isn't 5am.
    let ampm_end = |t: &str, end: usize| !t[end..].starts_with(|c: char| c.is_ascii_lowercase() || c == '.');
    if let Some(t) = take(&mut text, &AMPM, &ampm_end) {
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
    } else if let Some(t) = take(&mut text, &H24, &any) {
        time = Some([num(&t[1])?, num(&t[2])?, num(&t[3]).unwrap_or(0)]);
        clock24 = true;
    } else if let Some(t) = take(&mut text, &NOON, &any) {
        time = Some(if t[1].as_deref() == Some("midnight") { [0, 0, 0] } else { [12, 0, 0] });
    } else if let Some(t) = take(&mut text, &AT, &any) {
        let h = num(&t[1])?;
        if h > 23 {
            return None;
        }
        time = Some([h, 0, 0]);
        clock24 = true;
    }
    if let Some(p) = take(&mut text, &PART, &any) {
        let part = p[1].as_deref().unwrap_or_default();
        match time.as_mut() {
            None => time = Some([match part { "morning" => 9, "afternoon" => 15, "evening" => 18, _ => 20 }, 0, 0]),
            Some(t) if clock24 && part != "morning" && t[0] < 12 => t[0] += 12,
            _ => {}
        }
    }

    if let Some(d) = take(&mut text, &ISO, &any) {
        date = NaiveDate::from_ymd_opt(d[1].as_deref()?.parse().ok()?, num(&d[2])?, num(&d[3])?)?;
        have_date = true;
    } else if let Some(d) = take(&mut text, &SLASH, &any) {
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
        let found = take(&mut text, &MD, &any).map(|f| (f[1].clone(), f[2].clone(), f[3].clone())).or_else(|| take(&mut text, &DM, &any).map(|f| (f[2].clone(), f[1].clone(), f[3].clone())));
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
        if let Some(w) = take(&mut text, &DAYWORD, &any) {
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
        } else if let Some(w) = take(&mut text, &STEP, &any) {
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
        } else if let Some(w) = take(&mut text, &WEEKDAY, &any) {
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
    if !ATON.replace_all(&text, "").trim().is_empty() {
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

struct Form {
    re: Regex,
    relative: bool,
    alone: bool,
}

const AMOUNT: &str = r"(?:\d+(?:\.\d+)?|an?|half an?)";
const UNIT: &str = "[a-z]+";
const END: &str = r"[.,!?;:)]? ?$";
const SMONTH: &str = "(jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|june?|july?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)";
const SDAY: &str = r"\d{1,2}(st|nd|rd|th)?";
const SCLOCK: &str = r"(,? (?:at )?(?:\d{1,2}(?::\d{2})? ?(?:am|pm)|\d{1,2}:\d{2}))?";
const SYEAR: &str = r"(,? \d{4})?";
const STIME: &str = r"(?:\d{1,2}(?::\d{2})? ?(?:am|pm|a\.m\.|p\.m\.)|\d{1,2}:\d{2})";
const SWEEKDAY: &str = r"(?:(?:this|next|coming) )?(?:sun|mon|tues?|wed(?:nes)?|thu(?:rs)?|fri|sat(?:ur)?)day";

static FORMS: LazyLock<Vec<Form>> = LazyLock::new(|| {
    let span = format!(r"{AMOUNT} ?{UNIT}(?:,?(?: and)? \d+ ?{UNIT})?");
    let dayword = format!("(?:today|tonight|tomorrow|tmrw|tmr|{SWEEKDAY})");
    let f = |s: String, relative: bool, alone: bool| Form { re: re(&s), relative, alone };
    vec![
        f(format!(r"(?i)(?:^|[\s(])(in {span}|{span} from now){END}"), true, false),
        f(format!(r"(?i)(?:^|[\s(])({SMONTH}\.? {SDAY}{SYEAR}{SCLOCK}){END}"), false, false),
        f(format!(r"(?i)(?:^|[\s(])(?:the )?({SDAY} (?:of )?{SMONTH}{SYEAR}{SCLOCK}){END}"), false, false),
        f(format!(r"(?:^|[\s(])(\d{{4}}-\d{{2}}-\d{{2}}(?:[ T]\d{{1,2}}:\d{{2}})?){END}"), false, false),
        f(format!(r"(?i)(?:^|[\s(])(?:on )?({dayword}(?:,? (?:at )?{STIME}| at \d{{1,2}})){END}"), false, false),
        f(format!(r"(?i)(?:^|[\s(])((?:at )?{STIME},? (?:on )?{dayword}){END}"), false, false),
        f(format!(r"(?i)(?:^|[\s(])((?:at )?\d{{1,2}}(?::\d{{2}})? ?(?:am|pm|a\.m\.|p\.m\.)|at \d{{1,2}}:\d{{2}}){END}"), false, true),
    ]
});

/// A time phrase the draft ends with, worth offering as a time everyone reads in their own
/// zone (`ttSuggest`): "in 6 hours" a countdown, a date a date, a date with a time both.
pub fn suggest(before: &str, now: DateTime<Local>) -> Option<Suggestion> {
    static MAY: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(\d+\w* (of )?)?may\b"));
    static ORD: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\d(st|nd|rd|th)\b"));
    static TAIL: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"(?i){SYEAR}{SCLOCK}$")));
    static LEAD: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(?:day|night|week|month|year|\d)[,.]? *$"));
    static AMOUNTS: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(?:(\d+(?:\.\d+)?) ?|(half an?|an?) )([a-z]+)"));
    if before.chars().count() < 6 {
        return None;
    }
    let skip = before.char_indices().rev().nth(79).map_or(0, |(i, _)| i);
    let tail = &before[skip..];
    for form in FORMS.iter() {
        let Some(m) = form.re.captures(tail) else { continue };
        let whole = m.get(0)?;
        let phrase = m.get(1)?.as_str();
        if !form.relative && MAY.is_match(phrase) {
            let parts = TAIL.captures(phrase);
            let dated = parts.is_some_and(|p| p.get(1).is_some_and(|g| !g.as_str().is_empty()) || p.get(2).is_some_and(|g| !g.as_str().is_empty()));
            if !ORD.is_match(phrase) && !dated {
                return None;
            }
        }
        let start = skip + whole.start() + whole.as_str().find(phrase)?;
        let end = start + phrase.len();
        if form.alone {
            let from = before[..start].char_indices().rev().nth(23).map_or(0, |(i, _)| i);
            if LEAD.is_match(&before[from..start]) {
                return None;
            }
        }
        if vector_core::text_color::code_ranges(before).iter().any(|&(s, e)| start >= s && start < e) {
            return None;
        }
        let mut read = phrase.to_string();
        if form.relative {
            let mut first = true;
            let mut bad = false;
            read = AMOUNTS
                .replace_all(phrase, |c: &Captures| {
                    let amount = c.get(1).or(c.get(2)).map_or("", |g| g.as_str());
                    let unit = suggest_unit(&c[3], first, c.get(1).is_none());
                    first = false;
                    match unit {
                        Some(u) => format!("{amount} {u}"),
                        None => {
                            bad = true;
                            String::new()
                        }
                    }
                })
                .into_owned();
            if bad {
                return None;
            }
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

/// `<t:UNIX>` and `<t:UNIX:STYLE>` outside code: (byte start, byte end, unix, style).
pub fn codes(src: &str) -> Vec<(usize, usize, i64, char)> {
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| re(r"<t:(-?\d{1,12})(?::([tTdDfFsSR]))?>"));
    if !src.contains("<t:") {
        return Vec::new();
    }
    let code = vector_core::text_color::code_ranges(src);
    TOKEN
        .captures_iter(src)
        .filter_map(|c| {
            let m = c.get(0)?;
            if code.iter().any(|&(s, e)| m.start() >= s && m.start() < e) {
                return None;
            }
            let unix: i64 = c[1].parse().ok()?;
            vector_core::text_time::fallback(unix)?;
            Some((m.start(), m.end(), unix, c.get(2).and_then(|s| s.as_str().chars().next()).unwrap_or('f')))
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
mod tests {
    use super::*;
    use chrono::Timelike;

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
