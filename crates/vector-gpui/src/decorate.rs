//! The composer's decorated source (`cmpTokenize`): markdown, tags and headers styled in
//! place while the text stays exactly what gets sent. Markers stay visible but recede.

use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Deco {
    /// A marker (`**`, `# `, a list bullet): it recedes.
    Mark,
    Bold,
    Italic,
    BoldItalic,
    Strike,
    Spoiler,
    /// Monospace; the ticks are `CodeMark`.
    Code,
    CodeMark,
    Mention,
    Header,
    Subtext,
}

/// Typographic variants macOS substitutes as you type, folded so a tag survives them.
fn fold(c: char) -> char {
    match c {
        '\u{2018}' | '\u{2019}' => '\'',
        '\u{201C}' | '\u{201D}' => '"',
        '\u{2013}' | '\u{2014}' => '-',
        c => c.to_lowercase().next().unwrap_or(c),
    }
}

/// The byte length at `src[at..]` of the longest tracked name there, as `resolveMention`.
fn tracked_at(src: &str, at: usize, names: &[String]) -> Option<usize> {
    let mut best: Option<(usize, usize)> = None;
    for name in names.iter().filter(|n| !n.is_empty()) {
        let count = name.chars().count();
        if best.is_some_and(|(c, _)| count <= c) {
            continue;
        }
        let mut have = src[at..].char_indices();
        let mut end = at;
        let fits = name.chars().all(|want| match have.next() {
            Some((i, got)) if fold(got) == fold(want) => {
                end = at + i + got.len_utf8();
                true
            }
            _ => false,
        });
        if fits {
            best = Some((count, end - at));
        }
    }
    best.map(|(_, len)| len)
}

/// `X(?!\s)[^X\n]+(?<!\s)X` at `at`, for a run of `n` copies of the marker byte `m`.
fn delimited(src: &str, at: usize, m: u8, n: usize) -> Option<usize> {
    let b = src.as_bytes();
    if b.len() < at + 2 * n + 1 || b[at..at + n].iter().any(|&c| c != m) {
        return None;
    }
    let body = at + n;
    let rest = &src[body..];
    let len = rest.find(|c| c == m as char || c == '\n').unwrap_or(rest.len());
    if len == 0 || b.len() < body + len + n || b[body + len..body + len + n].iter().any(|&c| c != m) {
        return None;
    }
    let inner = &rest[..len];
    if inner.starts_with(char::is_whitespace) || inner.ends_with(char::is_whitespace) {
        return None;
    }
    Some(body + len + n)
}

/// `||…||`: anything but a pipe or newline inside.
fn spoiler(src: &str, at: usize) -> Option<usize> {
    let rest = src[at..].strip_prefix("||")?;
    let len = rest.find(['|', '\n']).unwrap_or(rest.len());
    (len > 0 && rest[len..].starts_with("||")).then_some(at + 2 + len + 2)
}

fn code(src: &str, at: usize) -> Option<usize> {
    let rest = src[at..].strip_prefix('`')?;
    let len = rest.find(['`', '\n']).unwrap_or(rest.len());
    (len > 0 && rest[len..].starts_with('`')).then_some(at + 1 + len + 1)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A line-level format at a line start: `(header|subtext|list, marker length, end)`.
fn line_format(src: &str, at: usize) -> Option<(Deco, usize, usize)> {
    let line_end = src[at..].find('\n').map_or(src.len(), |i| at + i);
    let line = &src[at..line_end];
    let hashes = line.bytes().take_while(|&c| c == b'#').count();
    if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
        return Some((Deco::Header, hashes + 1, line_end));
    }
    if line.starts_with("-# ") {
        return Some((Deco::Subtext, 3, line_end));
    }
    let spaces = line.bytes().take_while(|&c| c == b' ').count();
    if spaces <= 3 {
        let rest = &line[spaces..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let marker = if rest.starts_with("- ") || rest.starts_with("* ") {
            Some(1)
        } else if (1..=9).contains(&digits) && rest[digits..].starts_with(". ") {
            Some(digits + 1)
        } else {
            None
        };
        if let Some(m) = marker {
            return Some((Deco::Mark, spaces + m + 1, at + spaces + m + 1));
        }
    }
    None
}

/// The styled spans of `src`, in order and never overlapping.
pub fn tokenize(src: &str, names: &[String]) -> Vec<(Range<usize>, Deco)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < src.len() {
        let line_start = i == 0 || src.as_bytes()[i - 1] == b'\n';
        let c = src[i..].chars().next().unwrap_or(' ');
        let prev = src[..i].chars().next_back();
        let mut wrap = |out: &mut Vec<(Range<usize>, Deco)>, end: usize, mark: usize, body: Deco| {
            out.push((i..i + mark, Deco::Mark));
            out.push((i + mark..end - mark, body));
            out.push((end - mark..end, Deco::Mark));
            end
        };
        let next = match c {
            '`' => code(src, i).map(|end| {
                out.push((i..i + 1, Deco::CodeMark));
                out.push((i + 1..end - 1, Deco::Code));
                out.push((end - 1..end, Deco::CodeMark));
                end
            }),
            '*' => delimited(src, i, b'*', 3)
                .map(|e| wrap(&mut out, e, 3, Deco::BoldItalic))
                .or_else(|| delimited(src, i, b'*', 2).map(|e| wrap(&mut out, e, 2, Deco::Bold)))
                .or_else(|| (line_start).then(|| line_format(src, i)).flatten().and_then(|(d, m, e)| {
                    (d == Deco::Mark).then(|| {
                        out.push((i..i + m, Deco::Mark));
                        e
                    })
                }))
                .or_else(|| delimited(src, i, b'*', 1).map(|e| wrap(&mut out, e, 1, Deco::Italic))),
            '~' => delimited(src, i, b'~', 2).map(|e| wrap(&mut out, e, 2, Deco::Strike)),
            '|' => spoiler(src, i).map(|e| wrap(&mut out, e, 2, Deco::Spoiler)),
            '_' => {
                let found = [(3, Deco::BoldItalic), (2, Deco::Bold), (1, Deco::Italic)]
                    .into_iter()
                    .find_map(|(n, d)| delimited(src, i, b'_', n).map(|e| (e, n, d)));
                match found {
                    Some((end, n, d)) => {
                        let opens = !prev.is_some_and(is_word);
                        let closes = !src[end..].chars().next().is_some_and(is_word);
                        if opens && closes {
                            Some(wrap(&mut out, end, n, d))
                        } else {
                            Some(i + 1)
                        }
                    }
                    None => None,
                }
            }
            '@' => {
                let opens = prev.is_none_or(char::is_whitespace);
                let len = tracked_at(src, i + 1, names);
                let closes = len.is_some_and(|l| {
                    src[i + 1 + l..].chars().next().is_none_or(|c| c.is_whitespace() || ".,!?;:".contains(c))
                });
                (opens && closes).then(|| {
                    let end = i + 1 + len.unwrap_or(0);
                    out.push((i..end, Deco::Mention));
                    end
                })
            }
            _ if line_start => line_format(src, i).map(|(d, m, end)| {
                out.push((i..i + m, Deco::Mark));
                if d != Deco::Mark {
                    out.push((i + m..end, d));
                }
                end
            }),
            _ => None,
        };
        i = next.unwrap_or(i + c.len_utf8());
    }
    out.retain(|(r, _)| !r.is_empty());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str, names: &[&str]) -> Vec<(&'static str, String)> {
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        tokenize(src, &names)
            .into_iter()
            .map(|(r, d)| {
                let k = match d {
                    Deco::Mark | Deco::CodeMark => "mark",
                    Deco::Bold => "bold",
                    Deco::Italic => "italic",
                    Deco::BoldItalic => "bolditalic",
                    Deco::Strike => "strike",
                    Deco::Spoiler => "spoiler",
                    Deco::Code => "code",
                    Deco::Mention => "mention",
                    Deco::Header => "header",
                    Deco::Subtext => "subtext",
                };
                (k, src[r].to_string())
            })
            .collect()
    }

    #[test]
    fn markdown_keeps_its_markers() {
        assert_eq!(
            kinds("a **b** _c_ ~~d~~", &[]),
            vec![
                ("mark", "**".into()),
                ("bold", "b".into()),
                ("mark", "**".into()),
                ("mark", "_".into()),
                ("italic", "c".into()),
                ("mark", "_".into()),
                ("mark", "~~".into()),
                ("strike", "d".into()),
                ("mark", "~~".into()),
            ]
        );
        assert!(kinds("snake_case_name and ** b **", &[]).is_empty());
        assert_eq!(kinds("# Hi", &[]), vec![("mark", "# ".into()), ("header", "Hi".into())]);
        assert_eq!(kinds("- item", &[]), vec![("mark", "- ".into())]);
    }

    #[test]
    fn tags_take_the_longest_tracked_name() {
        assert_eq!(
            kinds("hi @walter white, ok", &["Walter", "Walter White"]),
            vec![("mention", "@walter white".into())]
        );
        assert!(kinds("x@Walter", &["Walter"]).is_empty());
        assert!(kinds("@Walterx", &["Walter"]).is_empty());
        assert_eq!(kinds("@O\u{2019}Neil!", &["O'Neil"]), vec![("mention", "@O\u{2019}Neil".into())]);
    }
}
