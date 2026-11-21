//! Dates and times in the reader's language, with their region's order and clock.

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};

pub use vector_i18n::{Clock, DateStyle};

fn civil(t: &DateTime<Local>) -> vector_i18n::Civil {
    vector_i18n::Civil {
        year: t.year(),
        month: t.month() as u8,
        day: t.day() as u8,
        weekday: t.weekday().num_days_from_sunday() as u8,
        hour: t.hour() as u8,
        minute: t.minute() as u8,
        second: t.second() as u8,
    }
}

pub fn at_ms(ms: i64) -> Option<DateTime<Local>> {
    Local.timestamp_millis_opt(ms).single()
}

pub fn at_secs(secs: i64) -> Option<DateTime<Local>> {
    Local.timestamp_opt(secs, 0).single()
}

pub fn date(t: &DateTime<Local>, style: DateStyle) -> String {
    vector_i18n::locale().date(&civil(t), style)
}

pub fn time(t: &DateTime<Local>, clock: Clock) -> String {
    vector_i18n::locale().time(&civil(t), clock)
}

pub fn date_time(t: &DateTime<Local>, style: DateStyle, clock: Clock) -> String {
    vector_i18n::locale().date_time(&civil(t), style, clock)
}
