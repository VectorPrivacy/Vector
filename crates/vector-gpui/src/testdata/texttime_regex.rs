//! The regex forms of `texttime`'s readers, kept as the reference its scanners are tested against.
#![allow(dead_code)]

use std::sync::LazyLock;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate};
use regex::{Captures, Regex};

use super::{add_months, local_at, suggest_unit, Parsed, Suggestion};

pub fn parse_full(input: &str, now: DateTime<Local>) -> Option<Parsed> {
    let p = read(input, now)?;
    vector_core::text_time::fallback(p.unix).map(|_| p)
}

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

pub fn read(input: &str, now: DateTime<Local>) -> Option<Parsed> {
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
        let (a, b) = (num(&d[1])?, num(&d[2])?);
        let (day, month) = if vector_i18n::locale().dates.day_first() { (a, b) } else { (b, a) };
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
                    // "next friday" is the one in next week, not merely the next one to come.
                    let week_start = vector_i18n::locale().dates.week_start as i64;
                    let to_week = match (week_start - today + 7) % 7 {
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


