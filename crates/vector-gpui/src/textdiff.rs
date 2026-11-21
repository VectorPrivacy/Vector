//! `textdiff.js`: word-level diffs between two revisions of a message, folded to what changed.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Eq,
    Del,
    Ins,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Run {
    Text(Op, String),
    /// Unchanged words folded away, and whether they span lines.
    Gap { words: usize, lines: bool },
}

fn is_punct(c: char) -> bool {
    matches!(c, '.' | ',' | '!' | '?' | ';')
}

fn is_delim(c: char) -> bool {
    matches!(c, '*' | '~' | '`' | '|')
}

fn is_code_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '~' | '-')
}

/// Words, the whitespace between them, and sentence punctuation and markdown delimiters on their
/// own. A `:shortcode:` stays one token.
pub fn tokenize(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut i = 0;
    let s = text;
    while i < s.len() {
        let rest = &s[i..];
        let c = rest.chars().next().unwrap();
        if c == ':' {
            let body = &rest[1..];
            let n = body.find(|ch: char| !is_code_char(ch)).unwrap_or(body.len());
            if n > 0 && body[n..].starts_with(':') {
                out.push(&rest[..n + 2]);
                i += n + 2;
                continue;
            }
        }
        let class = |ch: char| {
            if ch.is_whitespace() {
                0
            } else if is_punct(ch) {
                1
            } else if is_delim(ch) {
                2
            } else {
                3
            }
        };
        let k = class(c);
        let end = rest.char_indices().find(|&(_, ch)| class(ch) != k).map_or(rest.len(), |(j, _)| j);
        out.push(&rest[..end]);
        i += end;
    }
    out
}

fn is_word(t: &str) -> bool {
    t.chars().any(char::is_alphanumeric)
}

/// Past this many cells the middle is taken as replaced whole: the table stays bounded.
const MAX_CELLS: usize = 4_000_000;

/// The edit from `before` to `after`: a common head and tail first, the rest by longest common
/// subsequence of tokens.
pub fn diff_words(before: &str, after: &str) -> Vec<(Op, String)> {
    let (a, b) = (tokenize(before), tokenize(after));
    let mut head = 0;
    while head < a.len() && head < b.len() && a[head] == b[head] {
        head += 1;
    }
    let mut tail = 0;
    while tail < a.len() - head && tail < b.len() - head && a[a.len() - 1 - tail] == b[b.len() - 1 - tail] {
        tail += 1;
    }
    let (am, bm) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    let mut toks: Vec<(Op, &str)> = a[..head].iter().map(|t| (Op::Eq, *t)).collect();
    if am.len() * bm.len() > MAX_CELLS {
        toks.extend(am.iter().map(|t| (Op::Del, *t)));
        toks.extend(bm.iter().map(|t| (Op::Ins, *t)));
    } else {
        let (n, m) = (am.len(), bm.len());
        let w = m + 1;
        let mut lcs = vec![0u32; (n + 1) * w];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i * w + j] = if am[i] == bm[j] {
                    lcs[(i + 1) * w + j + 1] + 1
                } else {
                    lcs[(i + 1) * w + j].max(lcs[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && am[i] == bm[j] {
                toks.push((Op::Eq, am[i]));
                i += 1;
                j += 1;
            } else if j < m && (i >= n || lcs[i * w + j + 1] >= lcs[(i + 1) * w + j]) {
                toks.push((Op::Ins, bm[j]));
                j += 1;
            } else {
                toks.push((Op::Del, am[i]));
                i += 1;
            }
        }
    }
    toks.extend(a[a.len() - tail..].iter().map(|t| (Op::Eq, *t)));
    slide_right(&mut toks);
    let mut ops = Vec::new();
    for (op, t) in toks {
        add_to(&mut ops, op, t);
    }
    absorb_spaces(ops)
}

/// Push each pure insertion or deletion as far right as the text allows.
fn slide_right(toks: &mut [(Op, &str)]) {
    let mut s = 0;
    while s < toks.len() {
        if toks[s].0 == Op::Eq {
            s += 1;
            continue;
        }
        let op = toks[s].0;
        let mut e = s;
        while e < toks.len() && toks[e].0 == op {
            e += 1;
        }
        if e < toks.len() && toks[e].0 != Op::Eq {
            s = e;
            continue;
        }
        while e < toks.len() && toks[e].0 == Op::Eq && toks[e].1 == toks[s].1 {
            toks[s].0 = Op::Eq;
            toks[e].0 = op;
            s += 1;
            e += 1;
        }
        s = e;
    }
}

fn add_to(list: &mut Vec<(Op, String)>, op: Op, text: &str) {
    match list.last_mut() {
        Some((o, t)) if *o == op => t.push_str(text),
        _ => list.push((op, text.to_string())),
    }
}

/// A space alone between two changes reads as part of them, and each change shows as everything
/// it removed, then everything it added.
fn absorb_spaces(ops: Vec<(Op, String)>) -> Vec<(Op, String)> {
    let mut out: Vec<(Op, String)> = Vec::new();
    let (mut del, mut ins, mut open) = (String::new(), String::new(), false);
    let flush = |out: &mut Vec<(Op, String)>, del: &mut String, ins: &mut String, open: &mut bool| {
        let (dc, ic): (Vec<char>, Vec<char>) = (del.chars().collect(), ins.chars().collect());
        let mut lead = 0;
        while lead < dc.len() && lead < ic.len() && dc[lead] == ic[lead] && dc[lead].is_whitespace() {
            lead += 1;
        }
        let mut trail = 0;
        while trail < dc.len() - lead
            && trail < ic.len() - lead
            && dc[dc.len() - 1 - trail] == ic[ic.len() - 1 - trail]
            && dc[dc.len() - 1 - trail].is_whitespace()
        {
            trail += 1;
        }
        let s = |v: &[char]| v.iter().collect::<String>();
        if lead > 0 {
            add_to(out, Op::Eq, &s(&dc[..lead]));
        }
        let d = s(&dc[lead..dc.len() - trail]);
        let n = s(&ic[lead..ic.len() - trail]);
        if !d.is_empty() {
            out.push((Op::Del, d));
        }
        if !n.is_empty() {
            out.push((Op::Ins, n));
        }
        if trail > 0 {
            out.push((Op::Eq, s(&dc[dc.len() - trail..])));
        }
        del.clear();
        ins.clear();
        *open = false;
    };
    for k in 0..ops.len() {
        let (op, text) = &ops[k];
        let lone = *op == Op::Eq
            && !text.is_empty()
            && text.chars().all(char::is_whitespace)
            && open
            && ops.get(k + 1).is_some_and(|n| n.0 != Op::Eq);
        if *op == Op::Eq && !lone {
            flush(&mut out, &mut del, &mut ins, &mut open);
            add_to(&mut out, Op::Eq, text);
            continue;
        }
        open = true;
        if *op != Op::Ins {
            del.push_str(text);
        }
        if *op != Op::Del {
            ins.push_str(text);
        }
    }
    flush(&mut out, &mut del, &mut ins, &mut open);
    out
}

/// Fold the unchanged stretches down to the lines a change sits on: each changed line shows
/// whole up to `line` words (else `context` words either side), and the lines between fold.
pub fn fold_unchanged(ops: &[(Op, String)], context: usize, line: usize) -> Vec<Run> {
    let words = |toks: &[&str]| toks.iter().filter(|t| is_word(t)).count();
    let mut out = Vec::new();
    for (k, (op, text)) in ops.iter().enumerate() {
        if *op != Op::Eq {
            out.push(Run::Text(*op, text.clone()));
            continue;
        }
        let toks = tokenize(text);
        let take = |from: isize, count: usize, dir: isize| -> isize {
            let (mut seen, mut idx) = (0, from);
            while idx >= 0 && (idx as usize) < toks.len() && seen < count {
                if is_word(toks[idx as usize]) {
                    seen += 1;
                }
                idx += dir;
            }
            idx
        };
        let mut head_end = 0usize;
        if k > 0 {
            let line_end = toks.iter().position(|t| t.contains('\n')).unwrap_or(toks.len());
            head_end = if words(&toks[..line_end]) <= line { line_end } else { take(0, context, 1) as usize };
        }
        let mut tail_start = toks.len();
        if k + 1 < ops.len() {
            let nl = toks.iter().rposition(|t| t.contains('\n')).map_or(0, |i| i + 1);
            tail_start = if words(&toks[nl..]) <= line {
                nl
            } else {
                (take(toks.len() as isize - 1, context, -1) + 1) as usize
            };
        }
        let hidden = &toks[head_end..tail_start.max(head_end)];
        let hidden_words = words(hidden);
        let lines = hidden.iter().any(|t| t.contains('\n'));
        if head_end >= tail_start || hidden_words == 0 || (hidden_words <= 3 && !lines) {
            out.push(Run::Text(Op::Eq, text.clone()));
            continue;
        }
        if head_end > 0 {
            out.push(Run::Text(Op::Eq, toks[..head_end].concat()));
        }
        out.push(Run::Gap { words: hidden_words, lines });
        if tail_start < toks.len() {
            out.push(Run::Text(Op::Eq, toks[tail_start..].concat()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rebuild(ops: &[(Op, String)], side: Op) -> String {
        ops.iter().filter(|(o, _)| *o == Op::Eq || *o == side).map(|(_, t)| t.as_str()).collect()
    }

    fn r(op: Op, t: &str) -> (Op, String) {
        (op, t.to_string())
    }

    #[test]
    fn both_sides_rebuild_exactly() {
        for (a, b) in [
            ("", "hello"),
            ("hello", ""),
            ("a b c", "a b c"),
            ("the quick brown fox", "the slow brown dog"),
            ("For any questions, tag @Team.", "For any questions, tag @Team or @Mods.\n\nThanks!"),
            ("x  y\n\nz", "x y\nz w"),
            (":cat: says hi", ":dog: says hi :cat:"),
        ] {
            let ops = diff_words(a, b);
            assert_eq!(rebuild(&ops, Op::Del), a);
            assert_eq!(rebuild(&ops, Op::Ins), b);
        }
    }

    #[test]
    fn changes_read_as_the_web_shows_them() {
        assert_eq!(
            diff_words("the quick brown fox", "the slow brown fox"),
            [r(Op::Eq, "the "), r(Op::Del, "quick"), r(Op::Ins, "slow"), r(Op::Eq, " brown fox")]
        );
        assert_eq!(
            diff_words("one two three", "uno dos three"),
            [r(Op::Del, "one two"), r(Op::Ins, "uno dos"), r(Op::Eq, " three")]
        );
        assert!(diff_words("hi :cat:", "hi :cats:").contains(&r(Op::Del, ":cat:")));
        assert_eq!(
            diff_words("with kindness.", "with kindness and patience."),
            [r(Op::Eq, "with kindness"), r(Op::Ins, " and patience"), r(Op::Eq, ".")]
        );
        assert_eq!(
            diff_words("Off-topic discussion.", "Off-topic discussion.\n\nFor questions, tag @Team."),
            [r(Op::Eq, "Off-topic discussion."), r(Op::Ins, "\n\nFor questions, tag @Team.")]
        );
        let ops = diff_words("with messages, images, or links", "with messages or links");
        assert!(!ops.iter().any(|(o, _)| *o == Op::Ins));
        assert_eq!(
            diff_words("**• Rule 3**", "**• Rule Three**"),
            [r(Op::Eq, "**• Rule "), r(Op::Del, "3"), r(Op::Ins, "Three"), r(Op::Eq, "**")]
        );
        assert_eq!(
            diff_words("Really happy! :love~3: :love~3:", "Really happy! :love~3:"),
            [r(Op::Eq, "Really happy! :love~3:"), r(Op::Del, " :love~3:")]
        );
    }

    #[test]
    fn folds() {
        let words: Vec<String> = (0..200).map(|i| format!("w{i}")).collect();
        let before = words.join(" ");
        let mut with = words[..100].to_vec();
        with.push("NEW".into());
        with.extend_from_slice(&words[100..]);
        let folded = fold_unchanged(&diff_words(&before, &with.join(" ")), 5, 40);
        assert_eq!(folded.len(), 5);
        assert!(matches!(folded[0], Run::Gap { words: 95, .. }));
        assert_eq!(folded[1], Run::Text(Op::Eq, "w95 w96 w97 w98 w99 ".into()));
        assert_eq!(folded[2], Run::Text(Op::Ins, "NEW ".into()));
        assert!(matches!(folded[4], Run::Gap { words: 95, .. }));

        let ten = "a b c d e f g h i j";
        assert_eq!(fold_unchanged(&diff_words(ten, ten), 8, 40), [Run::Gap { words: 10, lines: false }]);

        let before = "# Rules\n\n**• Respect** Treat everyone well.\n**• Clean** Keep it clean.\n\nFor any questions, tag @Admin.";
        let after = before.replace("@Admin", "@Team");
        let folded = fold_unchanged(&diff_words(before, &after), 8, 40);
        assert!(matches!(folded[0], Run::Gap { lines: true, .. }));
        assert_eq!(folded[1], Run::Text(Op::Eq, "For any questions, tag ".into()));
    }

    #[test]
    fn long_rewrites_stay_bounded() {
        let a: Vec<String> = (0..3000).map(|i| format!("a{i}")).collect();
        let b: Vec<String> = (0..3000).map(|i| format!("b{i}")).collect();
        let (a, b) = (a.join(" "), b.join(" "));
        let ops = diff_words(&a, &b);
        assert_eq!(rebuild(&ops, Op::Del), a);
        assert_eq!(rebuild(&ops, Op::Ins), b);
    }
}
