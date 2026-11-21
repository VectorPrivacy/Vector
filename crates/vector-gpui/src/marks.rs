//! The composer's formatting shortcuts write markdown into the text, as the desktop's
//! `cmpToggleMark` does: each line of the selection is marked on its trimmed text, or the
//! mark comes off every line when all of them already carry it.

use std::ops::Range;

/// Whether `mark` already wraps `src[from..to]`, around it (`outer`) or as the selection's own
/// ends. Stars count as a run, so `***x***` is both bold and italic.
fn state(src: &str, from: usize, to: usize, mark: &str) -> (bool, bool) {
    let b = src.as_bytes();
    if mark == "*" || mark == "**" {
        let before = b[..from].iter().rev().take_while(|&&c| c == b'*').count();
        let after = b[to..].iter().take_while(|&&c| c == b'*').count();
        let mut k = before.min(after);
        let mut outer = true;
        if k == 0 {
            let lead = b[from..to].iter().take_while(|&&c| c == b'*').count();
            let trail = b[from + lead..to].iter().rev().take_while(|&&c| c == b'*').count();
            if lead.min(trail) > 0 {
                k = lead.min(trail);
                outer = false;
            }
        }
        let active = if mark == "**" { k >= 2 } else { k == 1 || k >= 3 };
        return (active, outer);
    }
    let n = mark.len();
    let m = mark.as_bytes();
    if from >= n && &b[from - n..from] == m && b.len() >= to + n && &b[to..to + n] == m {
        return (true, true);
    }
    let inner = to - from > 2 * n && &b[from..from + n] == m && &b[to - n..to] == m;
    (inner, !inner)
}

/// Toggle `mark` over `sel`; the new text and the selection over the same words.
pub fn toggle(src: &str, sel: Range<usize>, mark: &str) -> (String, Range<usize>) {
    let n = mark.len();
    let mut segs = Vec::new();
    let mut a = sel.start;
    while a < sel.end {
        let b = src[a..].find('\n').map_or(sel.end, |i| (a + i).min(sel.end));
        let line = &src[a..b];
        let lead = line.len() - line.trim_start().len();
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            segs.push((a + lead, a + lead + trimmed.len()));
        }
        a = b + 1;
    }
    if segs.is_empty() {
        let out = format!("{}{mark}{mark}{}", &src[..sel.start], &src[sel.end..]);
        return (out, sel.start + n..sel.start + n);
    }
    let states: Vec<_> = segs.iter().map(|&(f, t)| state(src, f, t, mark)).collect();
    let off = states.iter().all(|s| s.0);
    let mut out = src.to_string();
    let mut shift: isize = 0;
    let mut range: Option<Range<usize>> = None;
    for (&(f, t), &(active, outer)) in segs.iter().zip(&states) {
        let mut from = (f as isize + shift) as usize;
        let mut to = (t as isize + shift) as usize;
        if off && outer {
            out = format!("{}{}{}", &out[..from - n], &out[from..to], &out[to + n..]);
            from -= n;
            to -= n;
            shift -= 2 * n as isize;
        } else if off {
            out = format!("{}{}{}", &out[..from], &out[from + n..to - n], &out[to..]);
            to -= 2 * n;
            shift -= 2 * n as isize;
        } else if !active {
            out = format!("{}{mark}{}{mark}{}", &out[..from], &out[from..to], &out[to..]);
            if outer {
                from += n;
                to += n;
            } else {
                to += 2 * n;
            }
            shift += 2 * n as isize;
        }
        range = Some(range.map_or(from..to, |r| r.start..to));
    }
    (out, range.unwrap_or(sel))
}

#[cfg(test)]
mod tests {
    use super::toggle;

    #[test]
    fn wraps_and_unwraps() {
        assert_eq!(toggle("say hi now", 4..6, "**"), ("say **hi** now".into(), 6..8));
        assert_eq!(toggle("say **hi** now", 6..8, "**"), ("say hi now".into(), 4..6));
        assert_eq!(toggle("say **hi** now", 4..10, "**"), ("say hi now".into(), 4..6));
    }

    #[test]
    fn stars_count_as_a_run() {
        assert_eq!(toggle("**hi**", 2..4, "*"), ("***hi***".into(), 3..5));
        assert_eq!(toggle("***hi***", 3..5, "*"), ("**hi**".into(), 2..4));
        assert_eq!(toggle("***hi***", 3..5, "**"), ("*hi*".into(), 1..3));
    }

    #[test]
    fn empty_selection_drops_a_pair() {
        assert_eq!(toggle("ab", 1..1, "`"), ("a``b".into(), 2..2));
    }

    #[test]
    fn each_line_is_marked_trimmed() {
        assert_eq!(toggle(" a \nb", 0..5, "~~"), (" ~~a~~ \n~~b~~".into(), 3..11));
    }
}
