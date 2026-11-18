//! Dates and times in the language's own patterns (CLDR's), with the region's clock and week.

use std::borrow::Cow;
use std::fmt::Write as _;

/// A moment as the reader's calendar sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i32,
    /// 1 to 12.
    pub month: u8,
    pub day: u8,
    /// 0 is Sunday.
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// The ways the app writes a date, each one of CLDR's skeletons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum DateStyle {
    /// `yMd`: 11/18/2026, 18/11/2026.
    Numeric,
    /// `yMMdd`: two-digit day and month.
    Padded,
    /// `yMMMd`: Nov 18, 2026.
    Medium,
    /// `yMMMMd`: November 18, 2026.
    Long,
    /// `yMMMMEEEEd`: Wednesday, November 18, 2026.
    Full,
    /// `MMMd`: Nov 18.
    DayMonth,
    /// `MMMEd`: Wed, Nov 18.
    WeekdayDayMonth,
    /// `yMMMEd`: Wed, Nov 18, 2026.
    WeekdayDayMonthYear,
    /// `E`: Wed.
    Weekday,
}

const STYLES: usize = 9;

/// How a time of day is written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clock {
    pub seconds: bool,
    /// `02:05 PM` rather than `2:05 PM`; a 24-hour clock always pads.
    pub pad_hour: bool,
}

impl Clock {
    pub const SHORT: Clock = Clock { seconds: false, pad_hour: false };
    pub const SECONDS: Clock = Clock { seconds: true, pad_hour: false };
    pub const PADDED: Clock = Clock { seconds: false, pad_hour: true };
}

/// What a language says about its calendar.
#[derive(Clone, Debug, PartialEq)]
pub struct Dates {
    pub months: [Cow<'static, str>; 12],
    pub months_short: [Cow<'static, str>; 12],
    /// Sunday first.
    pub weekdays: [Cow<'static, str>; 7],
    pub weekdays_short: [Cow<'static, str>; 7],
    pub am_pm: [Cow<'static, str>; 2],
    /// One per [`DateStyle`], in its order.
    pub patterns: [Cow<'static, str>; STYLES],
    /// `h:mm a`, `h:mm:ss a`, `HH:mm`, `HH:mm:ss`.
    pub times: [Cow<'static, str>; 4],
    /// A date, then a time: `{0}, {1}`.
    pub date_time: Cow<'static, str>,
    /// A weekday, then a time: `{0} {1}`.
    pub weekday_time: Cow<'static, str>,
    /// The region's clock, unless the system says otherwise.
    pub hour12: bool,
    /// 0 is Sunday.
    pub week_start: u8,
}

fn names<const N: usize>(list: [&'static str; N]) -> [Cow<'static, str>; N] {
    list.map(Cow::Borrowed)
}

const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const WEEKDAYS_SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

impl Default for Dates {
    /// en-GB.
    fn default() -> Self {
        Self {
            months: names(MONTHS),
            months_short: names(["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sept", "Oct", "Nov", "Dec"]),
            weekdays: names(WEEKDAYS),
            weekdays_short: names(WEEKDAYS_SHORT),
            am_pm: names(["am", "pm"]),
            patterns: names(["dd/MM/y", "dd/MM/y", "d MMM y", "d MMMM y", "EEEE d MMMM y", "d MMM", "EEE d MMM", "EEE, d MMM y", "EEE"]),
            times: names(["h:mm\u{202f}a", "h:mm:ss\u{202f}a", "HH:mm", "HH:mm:ss"]),
            date_time: "{0}, {1}".into(),
            weekday_time: "{0} {1}".into(),
            hour12: false,
            week_start: 1,
        }
    }
}

impl Dates {
    /// en-US: the same names in American order, a 12-hour clock and Sunday weeks.
    pub fn en_us() -> Self {
        Self {
            months_short: names(["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]),
            am_pm: names(["AM", "PM"]),
            patterns: names([
                "M/d/y",
                "MM/dd/y",
                "MMM d, y",
                "MMMM d, y",
                "EEEE, MMMM d, y",
                "MMM d",
                "EEE, MMM d",
                "EEE, MMM d, y",
                "EEE",
            ]),
            hour12: true,
            week_start: 0,
            ..Self::default()
        }
    }

    pub fn date(&self, c: &Civil, style: DateStyle) -> String {
        self.render(&self.patterns[style as usize], c, false)
    }

    pub fn time(&self, c: &Civil, clock: Clock) -> String {
        let i = if self.hour12 { 0 } else { 2 } + clock.seconds as usize;
        self.render(&self.times[i], c, clock.pad_hour)
    }

    pub fn date_time(&self, c: &Civil, style: DateStyle, clock: Clock) -> String {
        let joiner = if style == DateStyle::Weekday { &self.weekday_time } else { &self.date_time };
        joiner.replace("{0}", &self.date(c, style)).replace("{1}", &self.time(c, clock))
    }

    /// Whether `4/5` is the 4th of May here.
    pub fn day_first(&self) -> bool {
        let p = &self.patterns[DateStyle::Numeric as usize];
        p.find('d') < p.find('M')
    }

    fn render(&self, pattern: &str, c: &Civil, pad_hour: bool) -> String {
        let mut out = String::with_capacity(pattern.len() + 8);
        let mut chars = pattern.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\'' {
                // 'quoted' text is literal, and '' is an apostrophe.
                if chars.peek() == Some(&'\'') {
                    chars.next();
                    out.push('\'');
                    continue;
                }
                while let Some(q) = chars.next() {
                    if q == '\'' {
                        if chars.peek() != Some(&'\'') {
                            break;
                        }
                        chars.next();
                    }
                    out.push(q);
                }
                continue;
            }
            if !ch.is_ascii_alphabetic() {
                out.push(ch);
                continue;
            }
            let mut n = 1;
            while chars.peek() == Some(&ch) {
                chars.next();
                n += 1;
            }
            let h12 = match c.hour % 12 {
                0 => 12,
                h => h,
            };
            let _ = match (ch, n) {
                ('y', 2) => write!(out, "{:02}", c.year.rem_euclid(100)),
                ('y', _) => write!(out, "{}", c.year),
                ('M' | 'L', 1) => write!(out, "{}", c.month),
                ('M' | 'L', 2) => write!(out, "{:02}", c.month),
                ('M' | 'L', 3) => write!(out, "{}", self.months_short[(c.month as usize + 11) % 12]),
                ('M' | 'L', _) => write!(out, "{}", self.months[(c.month as usize + 11) % 12]),
                ('d', 1) => write!(out, "{}", c.day),
                ('d', _) => write!(out, "{:02}", c.day),
                ('E' | 'c', 1..=3) => write!(out, "{}", self.weekdays_short[c.weekday as usize % 7]),
                ('E' | 'c', _) => write!(out, "{}", self.weekdays[c.weekday as usize % 7]),
                ('h', 1) if !pad_hour => write!(out, "{h12}"),
                ('h', _) => write!(out, "{h12:02}"),
                ('H', 1) => write!(out, "{}", c.hour),
                ('H', _) => write!(out, "{:02}", c.hour),
                ('m', 1) => write!(out, "{}", c.minute),
                ('m', _) => write!(out, "{:02}", c.minute),
                ('s', 1) => write!(out, "{}", c.second),
                ('s', _) => write!(out, "{:02}", c.second),
                ('a', _) => write!(out, "{}", self.am_pm[(c.hour >= 12) as usize]),
                _ => Ok(()),
            };
        }
        out
    }
}

/// Regions whose English writes dates month first, as Intl's `en` does.
const MONTH_FIRST: [&str; 8] = ["", "US", "PR", "UM", "AS", "GU", "VI", "MP"];

/// The region in a BCP 47 or POSIX tag: `en-US`, `en_GB.UTF-8`, `zh-Hant-TW` → `US`, `GB`, `TW`.
pub(crate) fn region_of(tag: &str) -> String {
    tag.split(['.', '@'])
        .next()
        .unwrap_or("")
        .split(['-', '_'])
        .skip(1)
        .find(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_alphabetic()) || p.len() == 3 && p.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_ascii_uppercase)
        .unwrap_or_default()
}

/// The built-in English's calendar for the system's locale: American for the US and its
/// territories (and a bare `en`), British everywhere else.
pub(crate) fn english_for(system: &str) -> Dates {
    let english = system.split(['-', '_', '.']).next().is_some_and(|l| l.eq_ignore_ascii_case("en"));
    if english && MONTH_FIRST.contains(&region_of(system).as_str()) { Dates::en_us() } else { Dates::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AFTERNOON: Civil = Civil { year: 2026, month: 11, day: 8, weekday: 0, hour: 14, minute: 5, second: 9 };
    const MIDNIGHT: Civil = Civil { year: 2026, month: 9, day: 30, weekday: 3, hour: 0, minute: 0, second: 0 };

    #[test]
    fn british_dates_and_a_24_hour_clock() {
        let d = Dates::default();
        assert_eq!(d.date(&AFTERNOON, DateStyle::Numeric), "08/11/2026");
        assert_eq!(d.date(&AFTERNOON, DateStyle::Medium), "8 Nov 2026");
        assert_eq!(d.date(&AFTERNOON, DateStyle::Long), "8 November 2026");
        assert_eq!(d.date(&AFTERNOON, DateStyle::Full), "Sunday 8 November 2026");
        assert_eq!(d.date(&MIDNIGHT, DateStyle::DayMonth), "30 Sept");
        assert_eq!(d.date(&AFTERNOON, DateStyle::WeekdayDayMonth), "Sun 8 Nov");
        assert_eq!(d.time(&AFTERNOON, Clock::SHORT), "14:05");
        assert_eq!(d.time(&MIDNIGHT, Clock::SECONDS), "00:00:00");
        assert_eq!(d.date_time(&AFTERNOON, DateStyle::Medium, Clock::SHORT), "8 Nov 2026, 14:05");
        assert_eq!(d.date_time(&AFTERNOON, DateStyle::Weekday, Clock::PADDED), "Sun 14:05");
        assert!(d.day_first());
    }

    #[test]
    fn american_dates_and_a_12_hour_clock() {
        let d = Dates::en_us();
        assert_eq!(d.date(&AFTERNOON, DateStyle::Numeric), "11/8/2026");
        assert_eq!(d.date(&AFTERNOON, DateStyle::Padded), "11/08/2026");
        assert_eq!(d.date(&AFTERNOON, DateStyle::Full), "Sunday, November 8, 2026");
        assert_eq!(d.date(&MIDNIGHT, DateStyle::WeekdayDayMonthYear), "Wed, Sep 30, 2026");
        assert_eq!(d.time(&AFTERNOON, Clock::SHORT), "2:05\u{202f}PM");
        assert_eq!(d.time(&AFTERNOON, Clock::PADDED), "02:05\u{202f}PM");
        assert_eq!(d.time(&MIDNIGHT, Clock::SECONDS), "12:00:00\u{202f}AM");
        assert_eq!(d.date_time(&AFTERNOON, DateStyle::DayMonth, Clock::PADDED), "Nov 8, 02:05\u{202f}PM");
        assert!(!d.day_first());
    }

    #[test]
    fn quoted_text_is_literal() {
        let d = Dates { patterns: names(["d 'de' MMMM 'd''y'", "", "", "", "", "", "", "", ""]), ..Dates::default() };
        assert_eq!(d.date(&AFTERNOON, DateStyle::Numeric), "8 de November d'y");
    }

    #[test]
    fn the_system_locale_picks_the_english_calendar() {
        assert_eq!(region_of("en_GB.UTF-8"), "GB");
        assert_eq!(region_of("zh-Hant-TW"), "TW");
        assert_eq!(region_of("es-419"), "419");
        assert!(english_for("en-US").hour12);
        assert!(english_for("en").hour12);
        assert!(english_for("en_PR.UTF-8").hour12);
        assert!(!english_for("en-GB").hour12);
        assert!(!english_for("en-AU").hour12);
        assert!(!english_for("de-DE").hour12);
        assert!(!english_for("").hour12);
    }
}
