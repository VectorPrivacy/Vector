//! Coloured text in a received message (`text-color.js`): each span's edges are marked with
//! private-use sentinels that markdown carries through as text, then the glyphs between
//! them are painted. Edges move off anything a sentinel would break, rule for rule as the
//! Tauri client places them, so both clients colour the same glyphs.

use gpui::Hsla;
use vector_core::text_color::{ColorSpan, Effect};

const OPEN: u32 = 0xF0000;
const SHUT: u32 = 0xF0100;
const MAX_SPANS: usize = 64;
/// A long run shares each colour across neighbouring glyphs.
const STEPS: usize = 128;

pub enum Edge {
    Open(usize),
    Shut(usize),
}

pub fn edge(c: char) -> Option<Edge> {
    let v = c as u32;
    if (OPEN..OPEN + 0x100).contains(&v) {
        Some(Edge::Open((v - OPEN) as usize))
    } else if (SHUT..SHUT + 0x100).contains(&v) {
        Some(Edge::Shut((v - SHUT) as usize))
    } else {
        None
    }
}

pub fn is_sentinel(c: char) -> bool {
    edge(c).is_some()
}

/// Runs a span edge must never split, as `text-color.js` finds them with
/// `!?\[[^\[\]\n]{0,512}\]\([^()\n]{0,2048}\)|<[^<>\s]{1,2048}>|(?:https?://|www\.)[^\s<]+|`
/// `(?:nostr:|@)?(?:npub1|nprofile1|naddr1|nevent1|note1)[02-9ac-hj-np-z]+|:[a-zA-Z0-9_~-]+:|`
/// `&#?[a-zA-Z0-9]+;|`+[^`\n]*`+|\x1b\[[\x20-\x3f]*[\x40-\x7e]?`, scanned by hand: no alternative
/// shares a first byte, so each candidate has one parser and no backtracking.
fn atomic_runs(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut resume = 0;
    let mut base = 0;
    let mut pad = [0u8; 17];
    while base < b.len() {
        let n = (b.len() - base).min(17);
        let window = match b[base..base + n].try_into() {
            Ok(w) => w,
            Err(_) => {
                pad[..n].copy_from_slice(&b[base..]);
                pad[n..].fill(0);
                &pad
            }
        };
        let mut mask = starts(window);
        while mask != 0 {
            let lane = mask.trailing_zeros() / STRIDE;
            mask &= !(((1u64 << STRIDE) - 1) << (lane * STRIDE));
            let i = base + lane as usize;
            if i >= resume
                && let Some(end) = atom_at(text, i)
            {
                out.push((i, end));
                resume = end;
            }
        }
        base += 16;
    }
    out
}

/// Bit `i * STRIDE` set where `w[i]`, `w[i + 1]` can open a run; `w` is 17 bytes so the pair test
/// sees the next chunk's first byte. Letters only count before the byte that follows them in every
/// run they open (`ht`, `ww`, `n[poae]`), which keeps ordinary words out.
#[cfg(target_arch = "x86_64")]
const STRIDE: u32 = 1;
#[cfg(target_arch = "x86_64")]
fn starts(w: &[u8; 17]) -> u64 {
    use std::arch::x86_64::*;
    // SAFETY: SSE2 is part of the x86_64 baseline; both unaligned loads are in bounds.
    unsafe {
        let v = _mm_loadu_si128(w.as_ptr().cast());
        let n = _mm_loadu_si128(w.as_ptr().add(1).cast());
        let eq = |x: __m128i, b: u8| _mm_cmpeq_epi8(x, _mm_set1_epi8(b as i8));
        let or = |a, b| _mm_or_si128(a, b);
        let and = |a, b| _mm_and_si128(a, b);
        let punct = or(or(or(eq(v, b'!'), eq(v, b'[')), or(eq(v, b'<'), eq(v, b'@'))), or(or(eq(v, b':'), eq(v, b'&')), or(eq(v, b'`'), eq(v, 0x1b))));
        let letters = or(
            or(and(eq(v, b'h'), eq(n, b't')), and(eq(v, b'w'), eq(n, b'w'))),
            and(eq(v, b'n'), or(or(eq(n, b'p'), eq(n, b'o')), or(eq(n, b'a'), eq(n, b'e')))),
        );
        _mm_movemask_epi8(or(punct, letters)) as u32 as u64
    }
}
#[cfg(target_arch = "aarch64")]
const STRIDE: u32 = 4;
#[cfg(target_arch = "aarch64")]
fn starts(w: &[u8; 17]) -> u64 {
    use std::arch::aarch64::*;
    // SAFETY: NEON is part of the aarch64 baseline; both loads are in bounds.
    unsafe {
        let v = vld1q_u8(w.as_ptr());
        let n = vld1q_u8(w.as_ptr().add(1));
        let eq = |x: uint8x16_t, b: u8| vceqq_u8(x, vdupq_n_u8(b));
        let or = |a, b| vorrq_u8(a, b);
        let and = |a, b| vandq_u8(a, b);
        let punct = or(or(or(eq(v, b'!'), eq(v, b'[')), or(eq(v, b'<'), eq(v, b'@'))), or(or(eq(v, b':'), eq(v, b'&')), or(eq(v, b'`'), eq(v, 0x1b))));
        let letters = or(
            or(and(eq(v, b'h'), eq(n, b't')), and(eq(v, b'w'), eq(n, b'w'))),
            and(eq(v, b'n'), or(or(eq(n, b'p'), eq(n, b'o')), or(eq(n, b'a'), eq(n, b'e')))),
        );
        vget_lane_u64(vreinterpret_u64_u8(vshrn_n_u16(vreinterpretq_u16_u8(or(punct, letters)), 4)), 0)
    }
}

/// The scalar form of the vector tests above.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const STRIDE: u32 = 1;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn starts(w: &[u8; 17]) -> u64 {
    (0..16).fold(0, |m, i| {
        let hit = match w[i] {
            b'!' | b'[' | b'<' | b'@' | b':' | b'&' | b'`' | 0x1b => true,
            b'h' => w[i + 1] == b't',
            b'w' => w[i + 1] == b'w',
            b'n' => matches!(w[i + 1], b'p' | b'o' | b'a' | b'e'),
            _ => false,
        };
        m | (hit as u64) << i
    })
}

/// Where the first whitespace or `stop` char at or after `from` sits, or the end.
fn until(text: &str, from: usize, stop: fn(char) -> bool) -> usize {
    let b = text.as_bytes();
    let mut at = from;
    while let Some(&x) = b.get(at) {
        if x.is_ascii() {
            if matches!(x, b'\t'..=b'\r' | b' ') || stop(x as char) {
                break;
            }
            at += 1;
        } else {
            let c = text[at..].chars().next().unwrap_or_default();
            if c.is_whitespace() || stop(c) {
                break;
            }
            at += c.len_utf8();
        }
    }
    at
}

fn chars_within(b: &[u8], max: usize) -> bool {
    b.len() <= max || b.iter().filter(|&&x| x & 0xC0 != 0x80).count() <= max
}

fn atom_at(text: &str, i: usize) -> Option<usize> {
    let b = text.as_bytes();
    let class = |from: usize, ok: fn(u8) -> bool| from + b[from..].iter().take_while(|&&x| ok(x)).count();
    let url_tail = |from: usize| {
        let end = until(text, from, |c| c == '<');
        (end > from).then_some(end)
    };
    match b[i] {
        b'!' => (b.get(i + 1) == Some(&b'[')).then(|| link_at(b, i + 1)).flatten(),
        b'[' => link_at(b, i),
        b'<' => {
            let end = until(text, i + 1, |c| c == '<' || c == '>');
            (b.get(end) == Some(&b'>') && end > i + 1 && chars_within(&b[i + 1..end], 2048)).then_some(end + 1)
        }
        b'h' => {
            let rest = &b[i..];
            let p = if rest.starts_with(b"https://") { 8 } else if rest.starts_with(b"http://") { 7 } else { return None };
            url_tail(i + p)
        }
        b'w' => b[i..].starts_with(b"www.").then(|| url_tail(i + 4)).flatten(),
        b'n' | b'@' => {
            let p = if b[i..].starts_with(b"nostr:") { i + 6 } else if b[i] == b'@' { i + 1 } else { i };
            let kw = [&b"npub1"[..], b"nprofile1", b"naddr1", b"nevent1", b"note1"].into_iter().find(|k| b[p..].starts_with(k))?;
            let from = p + kw.len();
            let end = class(from, |x| matches!(x, b'0' | b'2'..=b'9' | b'a' | b'c'..=b'h' | b'j'..=b'n' | b'p'..=b'z'));
            (end > from).then_some(end)
        }
        b':' => {
            let end = class(i + 1, |x| x.is_ascii_alphanumeric() || matches!(x, b'_' | b'~' | b'-'));
            (end > i + 1 && b.get(end) == Some(&b':')).then_some(end + 1)
        }
        b'&' => {
            let from = i + 1 + (b.get(i + 1) == Some(&b'#')) as usize;
            let end = class(from, |x| x.is_ascii_alphanumeric());
            (end > from && b.get(end) == Some(&b';')).then_some(end + 1)
        }
        b'`' => {
            let run = class(i, |x| x == b'`');
            match memchr::memchr2(b'`', b'\n', &b[run..]).map(|n| run + n) {
                Some(close) if b[close] == b'`' => Some(class(close, |x| x == b'`')),
                // The opening run gives one of its own back as the closer.
                _ => (run - i >= 2).then_some(run),
            }
        }
        0x1b if b.get(i + 1) == Some(&b'[') => {
            let end = class(i + 2, |x| (0x20..=0x3f).contains(&x));
            Some(end + b.get(end).is_some_and(|x| (0x40..=0x7e).contains(x)) as usize)
        }
        _ => None,
    }
}

/// `\[[^\[\]\n]{0,512}\]\([^()\n]{0,2048}\)` from the `[`.
fn link_at(b: &[u8], open: usize) -> Option<usize> {
    let shut = memchr::memchr3(b'[', b']', b'\n', &b[open + 1..]).map(|n| open + 1 + n)?;
    if b[shut] != b']' || b.get(shut + 1) != Some(&b'(') || !chars_within(&b[open + 1..shut], 512) {
        return None;
    }
    let paren = shut + 2;
    let end = memchr::memchr3(b'(', b')', b'\n', &b[paren..]).map(|n| paren + n)?;
    (b[end] == b')' && chars_within(&b[paren..end], 2048)).then_some(end + 1)
}

use crate::scan::digit;

/// How many chars from `at` satisfy `f`, counting no further than `limit`.
fn count_while(t: &[char], at: usize, limit: usize, f: impl Fn(char) -> bool) -> usize {
    t[at.min(t.len())..].iter().take(limit).take_while(|&&c| f(c)).count()
}

/// Length in chars of `^[ \t]{0,3}(?:(?:#{1,6}|-#|>|[-*]|\d{1,9}\.)[ \t]+|>[ \t]?)*`.
fn prefix_len(t: &[char]) -> usize {
    let blank = |c: char| c == ' ' || c == '\t';
    let mut at = count_while(t, 0, 3, blank);
    loop {
        let c = t.get(at).copied();
        let item = match c {
            Some('#') => Some(count_while(t, at, 7, |c| c == '#')).filter(|&n| n <= 6),
            Some('-') if t.get(at + 1) == Some(&'#') && t.get(at + 2).is_some_and(|&c| blank(c)) => Some(2),
            Some('>' | '-' | '*') => Some(1),
            Some(d) if digit(d) => {
                Some(count_while(t, at, 10, digit)).filter(|&n| n <= 9 && t.get(at + n) == Some(&'.')).map(|n| n + 1)
            }
            _ => None,
        };
        match item {
            Some(n) if t.get(at + n).is_some_and(|&c| blank(c)) => at += n + count_while(t, at + n, usize::MAX, blank),
            _ if c == Some('>') => at += 1,
            _ => return at,
        }
    }
}

fn punct(c: Option<char>) -> bool {
    c.is_some_and(crate::scan::punct)
}

/// `<[A-Za-z/!?][^<>]*$`: only the last `<` can open it.
fn html_tail(t: &[char]) -> bool {
    t.iter().rposition(|&c| c == '<').is_some_and(|i| {
        t.get(i + 1).is_some_and(|&c| c.is_ascii_alphabetic() || "/!?".contains(c)) && !t[i + 1..].contains(&'>')
    })
}

/// `^[ \t]{0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$`
fn is_rule(line: &[char]) -> bool {
    let lead = line.iter().take(3).take_while(|c| matches!(c, ' ' | '\t')).count();
    let rest = &line[lead..];
    let Some(&c) = rest.first().filter(|c| matches!(c, '-' | '*' | '_')) else { return false };
    rest.iter().all(|&x| x == c || x == ' ' || x == '\t') && rest.iter().filter(|&&x| x == c).count() >= 3
}

/// `^[ \t]{0,3}\|(?!\|)`
fn is_table_row(line: &[char]) -> bool {
    let lead = line.iter().take(3).take_while(|c| matches!(c, ' ' | '\t')).count();
    line.get(lead) == Some(&'|') && line.get(lead + 1) != Some(&'|')
}

/// Length in chars of `^(?: {0,3}(?:> ?|(?:[-*+]|\d{1,9}[.)])(?: |\t)))+`.
fn marker_len(t: &[char]) -> usize {
    let tab = |i: usize| matches!(t.get(i), Some(' ' | '\t'));
    let mut at = 0;
    loop {
        let m = at + count_while(t, at, 3, |c| c == ' ');
        let next = match t.get(m) {
            Some('>') => m + 1 + (t.get(m + 1) == Some(&' ')) as usize,
            Some('-' | '*' | '+') if tab(m + 1) => m + 2,
            Some(&d) if digit(d) => {
                let n = count_while(t, m, 10, digit);
                if n <= 9 && matches!(t.get(m + n), Some('.' | ')')) && tab(m + n + 1) { m + n + 2 } else { return at }
            }
            _ => return at,
        };
        at = next;
    }
}

/// `^ {0,3}#{1,6}(?:[ \t]|$)`
fn heading(t: &[char]) -> bool {
    let sp = count_while(t, 0, 3, |c| c == ' ');
    let n = count_while(t, sp, 7, |c| c == '#');
    (1..=6).contains(&n) && matches!(t.get(sp + n), None | Some(' ' | '\t'))
}

/// Indented code blocks, as char ranges: runs of lines four columns in that open after a
/// blank line, a heading or the start.
fn indented_code(t: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut open: Option<usize> = None;
    let mut opens = true;
    let mut at = 0;
    loop {
        let nl = t[at..].iter().position(|&c| c == '\n').map(|i| at + i);
        let line = &t[at..nl.unwrap_or(t.len())];
        // A marker followed by a tab keeps the tab: it is the code's indent.
        let marker_len = marker_len(line);
        let marker_len = marker_len - (marker_len > 0 && line[marker_len - 1] == '\t') as usize;
        let body = &line[marker_len..];
        let blank = body.iter().all(|c| c.is_whitespace());
        let deep = !blank && (body.starts_with(&[' '; 4]) || body.get(count_while(body, 0, 3, |c| c == ' ')) == Some(&'\t'));
        if open.is_none() && deep && (opens || marker_len > 0) {
            open = Some(at);
        } else if let Some(o) = open.filter(|_| !deep && !blank) {
            out.push((o, at));
            open = None;
        }
        opens = blank || (open.is_none() && heading(line));
        match nl {
            Some(nl) => at = nl + 1,
            None => break,
        }
        if at > t.len() {
            break;
        }
    }
    if let Some(o) = open {
        out.push((o, t.len()));
    }
    out
}

/// `text` with a sentinel at each colour span's edge, and the char offsets they went in at;
/// `None` when there is nothing to mark or the text already holds sentinels.
pub fn mark(text: &str, spans: &[ColorSpan], times: &[(usize, usize)]) -> Option<(String, Vec<usize>)> {
    if spans.is_empty() || text.chars().any(is_sentinel) {
        return None;
    }
    let t: Vec<char> = text.chars().collect();
    let n = t.len();
    let byte_to_char = {
        let mut map = vec![0usize; text.len() + 1];
        let mut ci = 0;
        for (bi, c) in text.char_indices() {
            for k in 0..c.len_utf8() {
                map[bi + k] = ci;
            }
            ci += 1;
        }
        map[text.len()] = ci;
        map
    };
    let ch = |i: isize| -> Option<char> { (i >= 0).then(|| t.get(i as usize).copied()).flatten() };
    let fences: Vec<(usize, usize)> = vector_core::text_color::code_ranges(text)
        .into_iter()
        .filter(|&(s, _)| {
            let rest = &text[s..];
            let sp = rest.bytes().take(3).take_while(|&b| b == b' ').count();
            rest[sp..].starts_with("```")
        })
        .map(|(s, e)| (byte_to_char[s], byte_to_char[e]))
        .chain(indented_code(&t))
        .collect();
    let atomic: Vec<(usize, usize)> = atomic_runs(text)
        .into_iter()
        .map(|(s, e)| (byte_to_char[s], byte_to_char[e]))
        .chain(times.iter().copied().filter(|&(_, e)| e <= n))
        .collect();
    let last_nl = |from: isize| -> isize {
        let from = from.max(0) as usize;
        (0..=from.min(n.saturating_sub(1))).rev().find(|&i| t.get(i) == Some(&'\n')).map_or(-1, |i| i as isize)
    };
    let line_of = |at: usize| -> (usize, usize) {
        let s = if n == 0 { 0 } else { (last_nl(at as isize - 1) + 1) as usize };
        let e = t[at.min(n)..].iter().position(|&c| c == '\n').map_or(n, |i| at + i);
        (s, e)
    };
    let in_atom = |at: usize| atomic.iter().any(|&(s, e)| at >= s && at <= e);
    let blank = |c: Option<char>| matches!(c, Some(' ' | '\t'));
    let ws = |c: Option<char>| c.is_some_and(char::is_whitespace);
    // The JS reads UTF-16 units: beside an astral char it sees a surrogate, neither space nor punctuation.
    let unit_punct = |c: Option<char>| c.is_some_and(|c| c <= '\u{ffff}') && punct(c);
    let word = |c: Option<char>| c.is_some_and(|c| !c.is_whitespace()) && !unit_punct(c);
    let emphasis = |c: Option<char>| matches!(c, Some('*' | '_'));
    let free = |c: Option<char>| c.is_none() || ws(c);
    let beyond_run = |mut i: isize, step: isize| {
        let c = ch(i);
        while ch(i) == c && c.is_some() {
            i += step;
        }
        ch(i)
    };
    let last_index_of = |pat: &[char], from: isize| -> isize {
        if from < 0 {
            return -1;
        }
        let from = (from as usize).min(n.saturating_sub(pat.len()));
        (0..=from).rev().find(|&i| t[i..].starts_with(pat)).map_or(-1, |i| i as isize)
    };
    let syntax = |at: isize| -> bool {
        let Some(c) = ch(at) else { return false };
        if !"*_~|\\`[]()<>!#".contains(c) {
            return false;
        }
        match c {
            '!' => ch(at + 1) == Some('[') || ch(at - 1) == Some('<'),
            '(' => ch(at - 1) == Some(']'),
            ')' => last_index_of(&[']', '('], at) > last_index_of(&['\n'], at),
            '#' => {
                (at == 0 || ch(at - 1).is_some_and(|p| p.is_whitespace() || p == '#' || p == '-'))
                    && (at + 1 >= n as isize || ch(at + 1).is_some_and(|x| x.is_whitespace() || x == '#'))
            }
            '<' => ch(at + 1).is_some_and(|x| x.is_ascii_alphabetic() || "/!?".contains(x)),
            '>' => {
                let s = (last_nl(at - 1) + 1) as usize;
                let before = &t[s..at as usize];
                before.iter().all(|x| matches!(x, ' ' | '\t' | '>')) || html_tail(before)
            }
            _ => true,
        }
    };
    let wedged = |at: usize| -> bool {
        let at = at as isize;
        let (prev, next) = (ch(at - 1), ch(at));
        if prev == Some('\\') || (prev == Some('<') && syntax(at - 1)) || (prev == Some('!') && ch(at - 2) == Some('<')) {
            return true;
        }
        if syntax(at - 1) && !word(next) {
            return true;
        }
        syntax(at) && !word(prev) && !(emphasis(next) && unit_punct(prev) && free(beyond_run(at, 1)))
    };
    // (zone start, zone end, whole line)
    let zone_of = |at: usize| -> (usize, usize, bool) {
        let (s, e) = line_of(at);
        // JS `slice`: a start past the end is empty, as for a line opening on a newline.
        let line = t.get(s..e).unwrap_or(&[]);
        if is_rule(line) || is_table_row(line) {
            return (s, e, true);
        }
        let len = prefix_len(&line[..line.len().min(64)]);
        (s, s + len, false)
    };
    let rest_blank = |mut at: usize| -> Option<usize> {
        while blank(t.get(at).copied()) {
            at += 1;
        }
        (at == n || t[at] == '\n').then_some(at)
    };
    let start_rules = |mut at: usize| -> usize {
        for &(s, e) in &fences {
            if at >= s && at < e {
                at = e;
            }
        }
        if let Some(eol) = rest_blank(at) {
            at = (eol + 1).min(n);
        }
        loop {
            if at >= n {
                return n;
            }
            let (ls, le) = line_of(at);
            let line: String = t.get(ls..le).unwrap_or(&[]).iter().collect();
            if !line.trim().is_empty() && !zone_of(at).2 {
                break;
            }
            at = (le + 1).min(n);
        }
        let (zs, ze, _) = zone_of(at);
        if at >= zs && at < ze {
            at = ze;
        }
        if in_atom(at) {
            while at < n && !t[at].is_whitespace() {
                at += 1;
            }
            while blank(t.get(at).copied()) {
                at += 1;
            }
        }
        while at < n && wedged(at) {
            at += 1;
        }
        at
    };
    let end_rules = |mut at: usize| -> usize {
        for &(s, e) in &fences {
            if at > s && at <= e {
                at = s;
            }
        }
        loop {
            let (zs, ze, whole) = zone_of(at);
            if at == 0 || !(whole || at == zs || at < ze) {
                break;
            }
            at = zs.saturating_sub(1);
        }
        if in_atom(at) {
            while at > 0 && !t[at - 1].is_whitespace() {
                at -= 1;
            }
            while at > 0 && blank(Some(t[at - 1])) {
                at -= 1;
            }
        }
        if rest_blank(at).is_some() {
            while at > 0 && blank(Some(t[at - 1])) {
                at -= 1;
            }
        }
        while at > 0 && wedged(at) {
            at -= 1;
        }
        at
    };
    let settle = |mut at: usize, rules: &dyn Fn(usize) -> usize| {
        for _ in 0..=n + 1 {
            let next = rules(at);
            if next == at {
                break;
            }
            at = next;
        }
        at
    };
    // (offset, 0 for an end and 1 for a start, sentinel)
    let mut inserts: Vec<(usize, u8, char)> = Vec::new();
    for (i, span) in spans.iter().take(MAX_SPANS).enumerate() {
        let (from, to) = (span.from as usize, span.to as usize);
        if from >= to || to > n {
            continue;
        }
        let (a, b) = (settle(from, &start_rules), settle(to, &end_rules));
        if a >= b {
            continue;
        }
        inserts.push((a, 1, char::from_u32(OPEN + i as u32).unwrap_or(' ')));
        inserts.push((b, 0, char::from_u32(SHUT + i as u32).unwrap_or(' ')));
    }
    if inserts.is_empty() {
        return None;
    }
    inserts.sort_by_key(|&(at, order, _)| (at, order));
    let mut out = String::with_capacity(text.len() + inserts.len() * 4);
    let mut k = 0;
    for i in 0..=n {
        while k < inserts.len() && inserts[k].0 == i {
            out.push(inserts[k].2);
            k += 1;
        }
        if i < n {
            out.push(t[i]);
        }
    }
    Some((out, inserts.iter().map(|x| x.0).collect()))
}

fn rgb(hex: &str) -> [f64; 3] {
    let h = hex.trim_start_matches('#');
    let p = |i: usize| h.get(i..i + 2).and_then(|s| u8::from_str_radix(s, 16).ok()).unwrap_or(255) as f64;
    [p(0), p(2), p(4)]
}

fn luminance(c: [f64; 3]) -> f64 {
    let f = |v: f64| {
        let v = v / 255.;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2])
}

/// Lifted toward white until it reads against the chat's near-black.
fn readable(c: [f64; 3]) -> Hsla {
    let bg = luminance([16., 16., 16.]);
    let mut out = c;
    let mut t = 0.0f64;
    while t <= 1.0001 {
        out = c.map(|v| (v + (255. - v) * t).round());
        if (luminance(out) + 0.05) / (bg + 0.05) >= 4.5 {
            break;
        }
        t += 0.05;
    }
    gpui::Rgba { r: (out[0] / 255.) as f32, g: (out[1] / 255.) as f32, b: (out[2] / 255.) as f32, a: 1. }.into()
}

fn hsl(h: f64, s: f64, l: f64) -> [f64; 3] {
    let a = s * l.min(1. - l);
    let f = |n: f64| {
        let k = (n + h / 30.) % 12.;
        ((l - a * (-1f64).max((k - 3.).min(9. - k).min(1.))) * 255.).round()
    };
    [f(0.), f(8.), f(4.)]
}

fn color_at(span: &ColorSpan, i: usize, n: usize) -> Hsla {
    match span.effect {
        Effect::Solid => readable(rgb(span.colors.first().map_or("#ffffff", |s| s.as_str()))),
        Effect::Rainbow => readable(hsl(360. * i as f64 / n.max(1) as f64, 1., 0.6)),
        Effect::Gradient => {
            let stops: Vec<[f64; 3]> = span.colors.iter().map(|c| rgb(c)).collect();
            if stops.len() < 2 {
                return readable(stops.first().copied().unwrap_or([255.; 3]));
            }
            let t = if n > 1 { i as f64 / (n - 1) as f64 } else { 0. };
            let seg = ((t * (stops.len() - 1) as f64).floor() as usize).min(stops.len() - 2);
            let local = t * (stops.len() - 1) as f64 - seg as f64;
            let (a, b) = (stops[seg], stops[seg + 1]);
            readable([0, 1, 2].map(|k| (a[k] + (b[k] - a[k]) * local).round()))
        }
    }
}

/// Glyph `i` of `n` → its colour.
pub struct Palette {
    colors: Vec<Hsla>,
    n: usize,
}

impl Palette {
    pub fn new(span: &ColorSpan, n: usize) -> Self {
        if span.effect == Effect::Solid {
            return Self { colors: vec![color_at(span, 0, 1)], n: 1 };
        }
        let steps = n.min(STEPS).max(1);
        let colors = (0..steps)
            .map(|b| color_at(span, if n > steps { b * n / steps } else { b }, n))
            .collect();
        Self { colors, n }
    }

    pub fn at(&self, i: usize) -> Hsla {
        let steps = self.colors.len();
        let k = if self.n > steps { i * steps / self.n.max(1) } else { i };
        self.colors[k.min(steps - 1)]
    }
}

/// The spelling offered, from the first English locale: `color` in the US and its
/// territories (and with no English at all), `colour` elsewhere. Both are always read.
pub fn color_word() -> &'static str {
    static WORD: std::sync::LazyLock<&'static str> = std::sync::LazyLock::new(|| {
        let langs = sys_locale::get_locales().map(|l| l.replace('_', "-"));
        let english = langs.into_iter().find(|l| {
            let l = l.to_ascii_lowercase();
            l == "en" || l.starts_with("en-") || l.starts_with("en.")
        });
        match english {
            None => "color",
            Some(l) => {
                let region = l.split(['-', '.', '@']).nth(1).unwrap_or("").to_ascii_uppercase();
                if region.is_empty() || ["US", "PH", "PR", "UM", "AS", "GU", "VI", "LR"].contains(&region.as_str()) {
                    "color"
                } else {
                    "colour"
                }
            }
        }
    });
    *WORD
}

/// The names a command's hint offers as one-tap swatches.
const CHIP_COLORS: [&str; 12] = ["red", "orange", "yellow", "green", "teal", "blue", "purple", "pink", "brown", "coral", "mint", "white"];

/// A colour as it will render, for a swatch.
pub fn swatch(hex: &str) -> Hsla {
    readable(rgb(hex))
}

/// Suggestions for a colour argument: a spread of examples while it's empty, then the
/// names that start with (then contain) what's typed, or the typed `#hex`.
pub fn color_choices(query: &str) -> Vec<(String, Hsla)> {
    let q = query.trim_matches(|c: char| c.is_ascii_whitespace()).to_ascii_lowercase();
    let named = |n: &str| vector_core::text_color::hex(n).map(|h| (n.to_string(), swatch(&h)));
    if q.is_empty() {
        return CHIP_COLORS.iter().filter_map(|n| named(n)).collect();
    }
    if q.starts_with('#') {
        return vector_core::text_color::hex(&q).map(|h| (q.clone(), swatch(&h))).into_iter().collect();
    }
    let mut names: Vec<&str> = vector_core::text_color::named().map(|(n, _)| n).collect();
    let common = |n: &str| !CHIP_COLORS.contains(&n);
    names.sort_by(|a, b| (common(a), a.len(), *a).cmp(&(common(b), b.len(), *b)));
    let starts = names.iter().filter(|n| n.starts_with(&q));
    let within = names.iter().filter(|n| !n.starts_with(&q) && n.contains(&q));
    starts.chain(within).take(12).filter_map(|n| named(n)).collect()
}

/// A colour command's effect as gradient stops, from the colour words typed so far;
/// a missing colour shows grey.
pub fn effect_preview(name: &str, words: &[String]) -> Option<Vec<Hsla>> {
    let at = |w: Option<&String>| {
        w.and_then(|w| vector_core::text_color::hex(w)).map_or(gpui::rgb(0x464646).into(), |h| swatch(&h))
    };
    match name {
        "rainbow" => Some((0..7).map(|i| readable(hsl(360. * i as f64 / 7., 1., 0.6))).collect()),
        "gradient" => Some(vec![at(words.first()), at(words.get(1))]),
        "color" | "colour" => Some(vec![at(words.first()), at(words.first())]),
        _ => None,
    }
}

/// A leading `/rainbow`, `/gradient a b` or `/color c` command (`tcCommand`).
#[derive(Debug, PartialEq)]
pub struct Command {
    /// `rainbow`, `gradient` or `color`.
    pub name: &'static str,
    pub typed: String,
    pub body: Result<(usize, Effect, Vec<String>), String>,
}

pub fn command(src: &str) -> Option<Command> {
    let rest = src.strip_prefix('/')?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let typed = &rest[..end];
    let name = match typed {
        "rainbow" => "rainbow",
        "gradient" => "gradient",
        "color" | "colour" => "color",
        _ => return None,
    };
    let want = match name {
        "gradient" => 2,
        "color" => 1,
        _ => 0,
    };
    let word = color_word();
    let mut at = 1 + end;
    let mut colors = Vec::new();
    let fail = |e: String| Some(Command { name, typed: typed.to_string(), body: Err(e) });
    for _ in 0..want {
        let tail = &src[at..];
        let gap = tail.len() - tail.trim_start().len();
        let arg_len = tail[gap..].find(char::is_whitespace).unwrap_or(tail.len() - gap);
        let hex = (gap > 0 && arg_len > 0).then(|| vector_core::text_color::hex(&tail[gap..gap + arg_len])).flatten();
        let Some(hex) = hex else {
            return fail(if name == "gradient" {
                crate::t::composer_color_needs_two(word).to_string()
            } else {
                crate::t::composer_color_needs_one(word).to_string()
            });
        };
        colors.push(hex);
        at += gap + arg_len;
    }
    let tail = &src[at..];
    let gap = tail.len() - tail.trim_start().len();
    if gap == 0 || tail.trim().is_empty() {
        return fail(crate::t::composer_color_needs_text(word).to_string());
    }
    let effect = match name {
        "rainbow" => Effect::Rainbow,
        "gradient" => Effect::Gradient,
        _ => Effect::Solid,
    };
    Some(Command { name, typed: typed.to_string(), body: Ok((at + gap, effect, colors)) })
}

/// A colour command rewritten as the markup core reads.
pub fn command_to_markup(src: &str) -> Option<String> {
    let c = command(src)?;
    let (from, _, colors) = c.body.ok()?;
    let args: String = colors.iter().map(|h| format!(" {h}")).collect();
    let body = &src[from..];
    let fence = |l: &str| {
        let sp = l.bytes().take(3).take_while(|&b| b == b' ').count();
        l[sp..].starts_with("```")
    };
    let lead = if fence(body) { "\n" } else { "" };
    let gap = if body.rsplit('\n').next().is_some_and(fence) { "\n" } else { "" };
    Some(format!("<{n}{args}>{lead}{body}{gap}</{n}>", n = c.name))
}

/// The composer's colour preview (`cmpApplyColor`): byte ranges of markup that recedes,
/// and each painted glyph's byte offset with its colour. `skip` holds ranges that keep
/// their own look.
pub fn composer_paint(src: &str, command_owned: bool, skip: &[std::ops::Range<usize>]) -> (Vec<std::ops::Range<usize>>, Vec<(std::ops::Range<usize>, Hsla)>) {
    use unicode_segmentation::UnicodeSegmentation;
    let mut marks = Vec::new();
    let mut spans: Vec<(std::ops::Range<usize>, ColorSpan)> = Vec::new();
    let span = |effect, colors| ColorSpan { from: 0, to: 0, effect, colors };
    if command_owned && let Some(Command { body: Ok((from, effect, colors)), .. }) = command(src) {
        marks.push(0..from);
        spans.push((from..src.len(), span(effect, colors)));
    }
    for p in vector_core::text_color::markup_pairs(src) {
        marks.push(p.open.clone());
        marks.push(p.close.clone());
        spans.push((p.open.end..p.close.start, span(p.effect, p.colors)));
    }
    let skipped = |i: usize| skip.iter().chain(marks.iter()).any(|r| r.contains(&i));
    let mut paint = Vec::new();
    for (r, s) in &spans {
        let glyphs: Vec<(usize, usize)> = src[r.clone()]
            .grapheme_indices(true)
            .filter(|(i, g)| !g.chars().all(char::is_whitespace) && !skipped(r.start + i))
            .map(|(i, g)| (r.start + i, g.len()))
            .collect();
        let palette = Palette::new(s, glyphs.len());
        paint.extend(glyphs.into_iter().enumerate().map(|(k, (at, len))| (at..at + len, palette.at(k))));
    }
    (marks, paint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(from: u32, to: u32) -> ColorSpan {
        ColorSpan { from, to, effect: Effect::Rainbow, colors: Vec::new() }
    }

    fn shown(text: &str, s: &[ColorSpan]) -> String {
        mark(text, s, &[])
            .map(|(m, _)| {
                m.chars()
                    .map(|c| match edge(c) {
                        Some(Edge::Open(_)) => '{',
                        Some(Edge::Shut(_)) => '}',
                        None => c,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn fuzz_cases() -> Vec<String> {
        const TOKENS: &[&str] = &[
            "[", "]", "(", ")", "!", "<", ">", " ", " ", "\t", "\n", "http://", "https://", "www.", "nostr:", "@", "npub1",
            "nprofile1", "note1", "nevent1", "naddr1", "n", "qpz", "1", ":", "a_~-", "&", "#", ";", "`", "``", "\x1b[", "0;3",
            "m", "é", "١", "٣", "\u{3000}", "*", "-", "+", ".", "x", "12", "/", "?", "|", "😀",
        ];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let long = |n: usize, c: char| std::iter::repeat_n(c, n).collect::<String>();
        let mut cases: Vec<String> = (0..20_000)
            .map(|_| (0..next() % 40).map(|_| TOKENS[(next() % TOKENS.len() as u64) as usize]).collect())
            .collect();
        cases.extend([
            "see [docs](https://x.y/z) and <https://a.b> :wave: &amp; `code` npub1abc".to_string(),
            format!("[{}](u) then :ok:", long(513, 'é')),
            format!("[{}](u)", long(512, 'é')),
            format!("[a]({}) https://tail.example", long(2049, 'x')),
            format!("[a](<{}>) x", long(2049, 'y')),
            format!("<{}> and <ok>", long(2049, 'z')),
            format!("<{}> and <ok>", long(2048, 'ß')),
            format!("![{}](<www.x.y>)", long(600, 'q')),
            "[a](b)[c](d) <e><f> www.g.h".to_string(),
        ]);
        cases
    }

    #[test]
    fn scanners_match_the_patterns_they_replace() {
        let re = |p: &str| regex::RegexBuilder::new(p).size_limit(64 << 20).build().unwrap();
        let atomic = re(concat!(
            r"!?\[[^\[\]\n]{0,512}\]\([^()\n]{0,2048}\)|<[^<>\s]{1,2048}>|(?:https?://|www\.)[^\s<]+|",
            r"(?:nostr:|@)?(?:npub1|nprofile1|naddr1|nevent1|note1)[02-9ac-hj-np-z]+|:[a-zA-Z0-9_~-]+:|&#?[a-zA-Z0-9]+;|",
            r"`+[^`\n]*`+|\x1b\[[\x20-\x3f]*[\x40-\x7e]?"
        ));
        let prefix = re(r"^[ \t]{0,3}(?:(?:#{1,6}|-#|>|[-*]|\d{1,9}\.)[ \t]+|>[ \t]?)*");
        let marker = re(r"^(?: {0,3}(?:> ?|(?:[-*+]|\d{1,9}[.)])(?: |\t)))+");
        let heading = re(r"^ {0,3}#{1,6}(?:[ \t]|$)");
        let html = re(r"<[A-Za-z/!?][^<>]*$");
        let chars_of = |m: Option<regex::Match>| m.map_or(0, |m| m.as_str().chars().count());
        for text in fuzz_cases() {
            let want: Vec<(usize, usize)> = atomic.find_iter(&text).map(|m| (m.start(), m.end())).collect();
            assert_eq!(super::atomic_runs(&text), want, "{text:?}");
            for line in text.split('\n') {
                let t: Vec<char> = line.chars().collect();
                assert_eq!(super::prefix_len(&t), chars_of(prefix.find(line)), "prefix {line:?}");
                assert_eq!(super::marker_len(&t), chars_of(marker.find(line)), "marker {line:?}");
                assert_eq!(super::heading(&t), heading.is_match(line), "heading {line:?}");
                assert_eq!(super::html_tail(&t), html.is_match(line), "html {line:?}");
            }
        }
        let punct = re(r"^[\p{P}\p{S}]$");
        let digit = re(r"^\d$");
        let (mut p, mut d) = (Vec::new(), Vec::new());
        for c in (0..=0x10FFFFu32).filter_map(char::from_u32) {
            let s = c.encode_utf8(&mut [0; 4]).to_string();
            if super::punct(Some(c)) != punct.is_match(&s) {
                p.push(c as u32);
            }
            if super::digit(c) != digit.is_match(&s) {
                d.push(c as u32);
            }
        }
        assert!(p.is_empty() && d.is_empty(), "punct {p:x?} digit {d:x?}");
    }

    #[test]
    fn edges_keep_off_syntax_links_and_prefixes() {
        assert_eq!(shown("hello world", &[span(0, 5)]), "{hello} world");
        assert_eq!(shown("# Title here", &[span(0, 12)]), "# {Title here}");
        assert_eq!(shown("see https://a.io/x now", &[span(4, 10)]), "");
        assert_eq!(shown("**bold** x", &[span(0, 8)]), "**{bold}** x");
        assert_eq!(shown("a\n\n---\n\nb", &[span(0, 9)]), "{a\n\n---\n\nb}");
    }

    #[test]
    fn edges_on_text_opening_with_a_newline_or_beside_emoji() {
        assert_eq!(shown("\nhi there", &[span(0, 9)]), "\n{hi there}");
        assert_eq!(shown("\n\n# Title", &[span(0, 9)]), "\n\n# {Title}");
        assert_eq!(shown("\n> quote", &[span(0, 8)]), "\n> {quote}");
        assert_eq!(shown("\n😀 _x_ 👍🏽", &[span(1, 7)]), "\n{😀 _x_ }👍🏽");
    }

    /// Differential check against `tcMarkSpans`: `TC_CASES=<json file> cargo test dump -- --ignored`.
    #[test]
    #[ignore]
    fn dump() {
        let cases: Vec<(String, Vec<(u32, u32)>)> =
            serde_json::from_str(&std::fs::read_to_string(std::env::var("TC_CASES").unwrap()).unwrap()).unwrap();
        let mut out = String::new();
        for (t, s) in cases {
            let spans: Vec<ColorSpan> = s.into_iter().map(|(a, b)| span(a, b)).collect();
            out += &serde_json::to_string(&shown(&t, &spans)).unwrap();
            out.push('\n');
        }
        std::fs::write(std::env::var("TC_OUT").unwrap(), out).unwrap();
    }

    #[test]
    fn colour_commands() {
        assert_eq!(command_to_markup("/rainbow hi there").as_deref(), Some("<rainbow>hi there</rainbow>"));
        assert_eq!(command_to_markup("/gradient pink #fff hi").as_deref(), Some("<gradient #f472b6 #ffffff>hi</gradient>"));
        assert_eq!(command_to_markup("/colour red x").as_deref(), Some("<color #f04848>x</color>"));
        assert!(command("/color nope x").unwrap().body.is_err());
        assert!(command("/rainbow ").unwrap().body.is_err());
        assert!(command("/rainbows x").is_none());
        assert_eq!(color_choices("").len(), 12);
        assert_eq!(color_choices("br")[0].0, "brown");
        let (marks, paint) = composer_paint("<rainbow>ab c</rainbow>", true, &[]);
        assert_eq!(marks, vec![0..9, 13..23]);
        assert_eq!(paint.iter().map(|p| p.0.start).collect::<Vec<_>>(), vec![9, 10, 12]);
    }

    #[test]
    fn palettes_floor_readability() {
        let dark = ColorSpan { from: 0, to: 1, effect: Effect::Solid, colors: vec!["#000000".into()] };
        let c = Palette::new(&dark, 1).at(0);
        assert!(c.l > 0.4);
        let r = Palette::new(&span(0, 300), 300);
        assert_eq!(r.colors.len(), STEPS);
    }
}
