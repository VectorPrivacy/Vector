//! The language-specific half of numbers, sizes, durations, lists and dates, and the functions
//! translations call to reach it (`NUMBER`, `BYTES`, `DURATION`, `LIST`).

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use fluent_bundle::types::{FluentNumber, FluentNumberOptions};
use fluent_bundle::{FluentArgs, FluentResource, FluentValue, concurrent::FluentBundle};
use intl_memoizer::Memoizable;
use intl_memoizer::concurrent::IntlLangMemoizer;
use unic_langid::LanguageIdentifier;

/// What a language pack says about its numbers and dates (from CLDR).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LocaleData {
    pub decimal: Cow<'static, str>,
    pub group: Cow<'static, str>,
    /// Digits per group: 3 for most languages.
    pub group_size: usize,
    /// `Bytes, KB, MB, GB, TB`.
    pub units: [Cow<'static, str>; 5],
    /// Between list items, before the last, and between exactly two.
    pub list_sep: Cow<'static, str>,
    pub list_last: Cow<'static, str>,
    pub list_pair: Cow<'static, str>,
    pub dates: crate::dates::Dates,
}

impl Default for LocaleData {
    fn default() -> Self {
        Self {
            decimal: ".".into(),
            group: ",".into(),
            group_size: 3,
            units: ["Bytes".into(), "KB".into(), "MB".into(), "GB".into(), "TB".into()],
            list_sep: ", ".into(),
            list_last: " and ".into(),
            list_pair: " and ".into(),
            dates: Default::default(),
        }
    }
}

impl LocaleData {
    /// `n` with `decimals` places (trailing zeros kept only if `pad`) and grouped thousands.
    pub fn number(&self, n: f64, decimals: usize, pad: bool, grouping: bool) -> String {
        let s = format!("{:.decimals$}", n.abs());
        let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
        let frac = if pad { frac } else { frac.trim_end_matches('0') };
        let mut out = String::with_capacity(s.len() + 4);
        if n < 0. && s.bytes().any(|b| b.is_ascii_digit() && b != b'0') {
            out.push('-');
        }
        let size = self.group_size.max(1);
        for (i, ch) in int.chars().enumerate() {
            if grouping && i > 0 && (int.len() - i) % size == 0 {
                out.push_str(&self.group);
            }
            out.push(ch);
        }
        if !frac.is_empty() {
            out.push_str(&self.decimal);
            out.push_str(frac);
        }
        out
    }

    /// A size in the 1024 steps the app uses everywhere: `0 Bytes`, `1.5 KB`, `12.25 MB`.
    pub fn bytes(&self, n: f64, decimals: usize, pad: bool) -> String {
        if n == 0. {
            return format!("0 {}", self.units[0]);
        }
        let (mut v, mut i) = (n, 0);
        while v >= 1024. && i < self.units.len() - 1 {
            v /= 1024.;
            i += 1;
        }
        format!("{} {}", self.number(v, decimals, pad, false), self.units[i])
    }

    /// A running clock: `04:05`, or `1:02:03` past the hour.
    pub fn duration(&self, secs: u64) -> String {
        let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
        if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
    }

    /// `A`, `A and B`, `A, B and C`.
    pub fn list(&self, items: &[impl AsRef<str>]) -> String {
        match items {
            [] => String::new(),
            [a] => a.as_ref().to_string(),
            [a, b] => format!("{}{}{}", a.as_ref(), self.list_pair, b.as_ref()),
            [rest @ .., last] => {
                let mut out = String::new();
                for (i, s) in rest.iter().enumerate() {
                    if i > 0 {
                        out.push_str(&self.list_sep);
                    }
                    out.push_str(s.as_ref());
                }
                out.push_str(&self.list_last);
                out.push_str(last.as_ref());
                out
            }
        }
    }
}

fn num(v: &FluentValue) -> Option<f64> {
    match v {
        FluentValue::Number(n) => Some(n.value),
        FluentValue::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn named_usize(args: &FluentArgs, key: &str) -> Option<usize> {
    args.get(key).and_then(num).map(|n| n.max(0.) as usize)
}

fn named_bool(args: &FluentArgs, key: &str) -> Option<bool> {
    match args.get(key)? {
        FluentValue::String(s) => Some(s != "false" && s != "0"),
        FluentValue::Number(n) => Some(n.value != 0.),
        _ => None,
    }
}

static DATA: LazyLock<RwLock<HashMap<String, Arc<LocaleData>>>> = LazyLock::new(Default::default);

/// Makes `data` what the formatter uses for `tag`; any other tag gets en-GB's.
pub(crate) fn register(tag: &str, data: Arc<LocaleData>) {
    DATA.write().unwrap().insert(tag.to_string(), data);
}

pub(crate) fn data_for(tag: &str) -> Arc<LocaleData> {
    DATA.read().unwrap().get(tag).cloned().unwrap_or_default()
}

/// A bundle's language data, constructed once per bundle by Fluent's memoizer.
struct Data(Arc<LocaleData>);

impl Memoizable for Data {
    type Args = ();
    type Error = ();
    fn construct(lang: LanguageIdentifier, _: ()) -> Result<Self, ()> {
        Ok(Data(data_for(&lang.to_string())))
    }
}

/// How every number placed in a message is written: the language's marks and grouping, unless
/// `NUMBER($n, useGrouping: "false")` turns grouping off. Digits that aren't a quantity (a
/// year, a port) are passed as text.
pub(crate) fn formatter(v: &FluentValue, intls: &IntlLangMemoizer) -> Option<String> {
    let FluentValue::Number(n) = v else { return None };
    let o = &n.options;
    let decimals = o.maximum_fraction_digits.unwrap_or(3).max(o.minimum_fraction_digits.unwrap_or(0));
    intls
        .with_try_get::<Data, _, _>((), |d| d.0.number(n.value, decimals, o.minimum_fraction_digits.is_some(), o.use_grouping))
        .ok()
}

/// A number as the code passes it. Default options, or `[1]` variant keys wouldn't match it.
pub(crate) fn plain(n: f64) -> FluentValue<'static> {
    FluentValue::Number(FluentNumber::new(n, FluentNumberOptions::default()))
}

/// Gives a bundle the functions translations may call, over its language's data.
pub(crate) fn install(bundle: &mut FluentBundle<FluentResource>, data: Arc<LocaleData>) {
    bundle.set_formatter(Some(formatter));
    // Stays a number, so a select on it still picks plural forms.
    let _ = bundle.add_function("NUMBER", |pos, named| {
        let Some(FluentValue::Number(n)) = pos.first() else {
            return pos.first().and_then(num).map_or(FluentValue::Error, FluentValue::from);
        };
        let mut n = n.clone();
        n.options.merge(named);
        FluentValue::Number(n)
    });
    let d = data.clone();
    let _ = bundle.add_function("BYTES", move |pos, named| {
        let Some(n) = pos.first().and_then(num) else { return FluentValue::Error };
        let decimals = named_usize(named, "decimals").unwrap_or(2);
        FluentValue::String(d.bytes(n, decimals, named_bool(named, "pad").unwrap_or(false)).into())
    });
    let d = data;
    let _ = bundle.add_function("DURATION", move |pos, _| {
        let Some(n) = pos.first().and_then(num) else { return FluentValue::Error };
        FluentValue::String(d.duration(n.max(0.) as u64).into())
    });
    // Code joins the list before formatting, with the active language's words.
    let _ = bundle.add_function("LIST", |pos, _| pos.first().cloned().unwrap_or(FluentValue::Error));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn en_gb_numbers_sizes_clocks_and_lists() {
        let d = LocaleData::default();
        assert_eq!(d.number(1234567.891, 2, false, true), "1,234,567.89");
        assert_eq!(d.number(1000., 2, false, true), "1,000");
        assert_eq!(d.number(-0.001, 2, false, true), "0");
        assert_eq!(d.number(-12.5, 1, true, true), "-12.5");
        assert_eq!(d.bytes(0., 2, false), "0 Bytes");
        assert_eq!(d.bytes(1536., 2, false), "1.5 KB");
        assert_eq!(d.bytes(12.25 * 1048576., 2, false), "12.25 MB");
        assert_eq!(d.bytes(1048576., 2, true), "1.00 MB");
        assert_eq!(d.duration(65), "01:05");
        assert_eq!(d.duration(3723), "1:02:03");
        assert_eq!(d.list(&["Alice"]), "Alice");
        assert_eq!(d.list(&["Alice", "Bob"]), "Alice and Bob");
        assert_eq!(d.list(&["Alice", "Bob", "Carol"]), "Alice, Bob and Carol");
    }

    #[test]
    fn other_marks_and_group_sizes() {
        let d = LocaleData { decimal: ",".into(), group: "\u{202f}".into(), ..Default::default() };
        assert_eq!(d.number(12345.6, 1, false, true), "12\u{202f}345,6");
        assert_eq!(d.bytes(1536., 2, false), "1,5 KB");
    }
}
