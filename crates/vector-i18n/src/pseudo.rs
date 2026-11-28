//! Languages made from English at run time, never downloaded: `en-XA` accents and stretches
//! every string so untranslated or clipped text stands out, and `en-XI` shows message ids.

use std::borrow::Cow;

/// Accents every letter outside `<tag>` markup; Fluent applies it to text, not to values.
pub(crate) fn accent(s: &str) -> Cow<'_, str> {
    if !s.bytes().any(|b| b.is_ascii_alphabetic()) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() * 2);
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ => {}
        }
        out.push(if in_tag { ch } else { accented(ch) });
    }
    Cow::Owned(out)
}

fn accented(c: char) -> char {
    const LOWER: &str = "ȧƀƈḓḗƒɠħīĵķŀḿƞǿƥɋřşŧŭṽẇẋẏẑ";
    const UPPER: &str = "ȦƁƇḒḖƑƓĦĪĴĶĿḾȠǾƤɊŘŞŦŬṼẆẊẎẐ";
    match c {
        'a'..='z' => LOWER.chars().nth(c as usize - 'a' as usize).unwrap_or(c),
        'A'..='Z' => UPPER.chars().nth(c as usize - 'A' as usize).unwrap_or(c),
        _ => c,
    }
}

/// Brackets show where one message ends and the next begins; the padding is the ~30% a
/// longer language needs.
pub(crate) fn stretch(s: &str) -> String {
    let n = s.chars().count();
    let pad = n.div_ceil(3);
    let mut out = String::with_capacity(s.len() + pad * 2 + 2);
    out.push('[');
    out.push_str(s);
    out.extend(std::iter::repeat_n('·', pad));
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accents_text_but_not_markup() {
        assert_eq!(accent("Hi <b>Bob</b>!"), "Ħī <b>Ɓǿƀ</b>!");
        assert_eq!(accent("12:30"), "12:30");
        assert_eq!(stretch("Call"), "[Call··]");
    }
}
