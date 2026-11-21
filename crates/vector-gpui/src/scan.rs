//! Matching for fixed text patterns without a regex engine: the classes regex reads as Unicode,
//! literals, and position sets for grammars that need every way a prefix can end.

use unicode_properties::{GeneralCategory as G, UnicodeGeneralCategory};

/// `\d`: any decimal digit, not only ASCII.
pub fn digit(c: char) -> bool {
    c.is_ascii_digit() || (!c.is_ascii() && c.general_category() == G::DecimalNumber)
}

/// `\w`.
pub fn word(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || c == '_'
        || (!c.is_ascii()
            && (c.is_alphabetic()
                || matches!(c, '\u{200C}' | '\u{200D}')
                || matches!(
                    c.general_category(),
                    G::NonspacingMark | G::SpacingMark | G::EnclosingMark | G::DecimalNumber | G::ConnectorPunctuation
                )))
}

/// `[\p{P}\p{S}]`.
pub fn punct(c: char) -> bool {
    c.is_ascii_punctuation()
        || (!c.is_ascii()
            && matches!(
                c.general_category(),
                G::ConnectorPunctuation
                    | G::DashPunctuation
                    | G::OpenPunctuation
                    | G::ClosePunctuation
                    | G::InitialPunctuation
                    | G::FinalPunctuation
                    | G::OtherPunctuation
                    | G::MathSymbol
                    | G::CurrencySymbol
                    | G::ModifierSymbol
                    | G::OtherSymbol
            ))
}

pub fn next(t: &str, at: usize) -> Option<char> {
    t.get(at..)?.chars().next()
}

pub fn prev(t: &str, at: usize) -> Option<char> {
    t.get(..at)?.chars().next_back()
}

/// `\b` at byte `at`.
pub fn boundary(t: &str, at: usize) -> bool {
    prev(t, at).is_some_and(word) != next(t, at).is_some_and(word)
}

/// `c` against an ASCII pattern byte as `(?i)` folds: `k` and `s` also take KELVIN SIGN and LONG S.
pub fn fold_eq(c: char, p: u8) -> bool {
    c.to_ascii_lowercase() == p.to_ascii_lowercase() as char
        || (p | 0x20 == b'k' && c == '\u{212A}')
        || (p | 0x20 == b's' && c == '\u{17F}')
}

/// `(?i)[a-z]`.
pub fn fold_letter(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '\u{212A}' || c == '\u{17F}'
}

pub fn lit(t: &str, at: usize, w: &str) -> Option<usize> {
    t.get(at..)?.starts_with(w).then_some(at + w.len())
}

/// `w` matched case-insensitively.
pub fn lit_i(t: &str, at: usize, w: &str) -> Option<usize> {
    let b = t.as_bytes();
    let mut i = at;
    for &p in w.as_bytes() {
        let &x = b.get(i)?;
        if x.is_ascii() {
            if x != p && !(p.is_ascii_alphabetic() && x | 0x20 == p | 0x20) {
                return None;
            }
            i += 1;
        } else {
            let c = t[i..].chars().next()?;
            if !fold_eq(c, p) {
                return None;
            }
            i += c.len_utf8();
        }
    }
    Some(i)
}

/// How many chars from `at` satisfy `f`, up to `max`, and where they end.
pub fn run(t: &str, at: usize, max: usize, f: impl Fn(char) -> bool) -> (usize, usize) {
    let mut n = 0;
    let mut end = at;
    for c in t[at..].chars() {
        if n == max || !f(c) {
            break;
        }
        n += 1;
        end += c.len_utf8();
    }
    (n, end)
}

/// Non-overlapping matches, leftmost first, as a regex's `find_iter` takes them.
pub fn matches<'a, C: 'a>(t: &'a str, at: impl Fn(&str, usize) -> Option<(usize, C)> + 'a) -> impl Iterator<Item = (usize, usize, C)> + 'a {
    matches_from(t, |_, p| Some(p), at)
}

/// [`matches`] trying only where `from` says one can start: the first such position at or after
/// its argument, or none left.
pub fn matches_from<'a, C: 'a>(
    t: &'a str,
    from: impl Fn(&str, usize) -> Option<usize> + 'a,
    at: impl Fn(&str, usize) -> Option<(usize, C)> + 'a,
) -> impl Iterator<Item = (usize, usize, C)> + 'a {
    let mut p = 0;
    std::iter::from_fn(move || {
        while p <= t.len() {
            p = from(t, p)?;
            if let Some((end, caps)) = at(t, p) {
                let start = p;
                p = if end > p { end } else { p + next(t, p).map_or(1, char::len_utf8) };
                return Some((start, end, caps));
            }
            p += next(t, p).map_or(1, char::len_utf8);
        }
        None
    })
}

/// The next of `bytes` at or after `p`: where a pattern opening with one of them can start.
pub fn lead(bytes: &'static [u8]) -> impl Fn(&str, usize) -> Option<usize> {
    move |t, p| {
        let h = t.as_bytes().get(p..)?;
        let i = match *bytes {
            [a] => memchr::memchr(a, h),
            [a, b] => memchr::memchr2(a, b, h),
            [a, b, c] => memchr::memchr3(a, b, c, h),
            _ => h.iter().position(|x| bytes.contains(x)),
        }?;
        Some(p + i)
    }
}

/// `t` with each match replaced by what `put` writes for it.
pub fn replace_all<C>(t: &str, at: impl Fn(&str, usize) -> Option<(usize, C)>, put: impl Fn(&mut String, &str, C)) -> String {
    replace_from(t, |_, p| Some(p), at, put)
}

/// [`replace_all`] trying only where `from` allows, as [`matches_from`].
pub fn replace_from<C>(
    t: &str,
    from: impl Fn(&str, usize) -> Option<usize>,
    at: impl Fn(&str, usize) -> Option<(usize, C)>,
    put: impl Fn(&mut String, &str, C),
) -> String {
    let mut out = String::with_capacity(t.len());
    let mut last = 0;
    for (s, e, caps) in matches_from(t, from, at) {
        out += &t[last..s];
        put(&mut out, t, caps);
        last = e;
    }
    out += &t[last..];
    out
}

/// A set of byte positions below 384: every place a pattern so far can have reached.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct At([u64; 6]);

impl At {
    pub const CAP: usize = 384;

    pub fn of(p: usize) -> Self {
        let mut s = Self::default();
        s.add(p);
        s
    }

    /// Every char boundary of `t`, its end included.
    pub fn all(t: &str) -> Self {
        let mut s = Self::default();
        t.char_indices().for_each(|(i, _)| s.add(i));
        s.add(t.len());
        s
    }

    pub fn add(&mut self, p: usize) {
        if p < Self::CAP {
            self.0[p / 64] |= 1 << (p % 64);
        }
    }

    pub fn has(&self, p: usize) -> bool {
        p < Self::CAP && self.0[p / 64] & (1 << (p % 64)) != 0
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|&w| w == 0)
    }

    pub fn meets(&self, o: &Self) -> bool {
        self.0.iter().zip(o.0).any(|(a, b)| a & b != 0)
    }

    pub fn or(mut self, o: Self) -> Self {
        self.0.iter_mut().zip(o.0).for_each(|(a, b)| *a |= b);
        self
    }

    /// The highest position held; 0 when empty.
    fn last(&self) -> usize {
        (0..6).rev().find(|&w| self.0[w] != 0).map_or(0, |w| w * 64 + 63 - self.0[w].leading_zeros() as usize)
    }

    pub fn iter(self) -> impl Iterator<Item = usize> {
        let mut w = 0;
        let mut bits = self.0[0];
        std::iter::from_fn(move || {
            while bits == 0 {
                w += 1;
                if w == 6 {
                    return None;
                }
                bits = self.0[w];
            }
            let b = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            Some(w * 64 + b)
        })
    }

    /// Where `f` can go from each position.
    pub fn step(self, f: impl Fn(usize, &mut At)) -> Self {
        let mut out = Self::default();
        if self.is_empty() {
            return out;
        }
        self.iter().for_each(|p| f(p, &mut out));
        out
    }

    /// `x?`
    pub fn opt(self, f: impl Fn(Self) -> Self) -> Self {
        self.or(f(self))
    }

    /// A literal, case-insensitive.
    pub fn li(self, t: &str, w: &str) -> Self {
        let (b, f) = (t.as_bytes(), w.as_bytes()[0]);
        self.step(|p, o| {
            // An ASCII byte that differs from the first, folded, can't open it.
            if b.get(p).is_some_and(|&x| x.is_ascii() && x != f && !(f.is_ascii_alphabetic() && x | 0x20 == f | 0x20)) {
                return;
            }
            if let Some(e) = lit_i(t, p, w) {
                o.add(e)
            }
        })
    }

    /// Any of `ws`, case-insensitive.
    pub fn any_li(self, t: &str, ws: &[&str]) -> Self {
        ws.iter().fold(Self::default(), |acc, w| acc.or(self.li(t, w)))
    }

    /// A literal, case-sensitive.
    pub fn lit(self, t: &str, w: &str) -> Self {
        self.step(|p, o| {
            if let Some(e) = lit(t, p, w) {
                o.add(e)
            }
        })
    }

    /// Between `min` and `max` chars of a class.
    pub fn class(self, t: &str, min: usize, max: usize, f: impl Fn(char) -> bool) -> Self {
        if max == usize::MAX && min <= 1 {
            return self.class_run(t, min, f);
        }
        self.step(|p, o| {
            if min == 0 {
                o.add(p);
            }
            let mut e = p;
            for (n, c) in t[p..].chars().take(max).enumerate() {
                if !f(c) {
                    break;
                }
                e += c.len_utf8();
                if n + 1 >= min {
                    o.add(e);
                }
            }
        })
    }

    /// `f*` or `f+` in one sweep: a run reached from any position in it reaches all of its rest.
    fn class_run(self, t: &str, min: usize, f: impl Fn(char) -> bool) -> Self {
        let mut out = if min == 0 { self } else { Self::default() };
        let Some(first) = self.iter().next() else { return out };
        let last = self.last();
        let mut live = false;
        for (i, c) in t[first..].char_indices() {
            let i = first + i;
            live |= self.has(i);
            if !live && i > last {
                break;
            }
            if f(c) {
                if live {
                    out.add(i + c.len_utf8());
                }
            } else {
                live = false;
            }
        }
        out
    }

    /// `\d{min,max}`.
    pub fn digits(self, t: &str, min: usize, max: usize) -> Self {
        self.class(t, min, max, digit)
    }
}
