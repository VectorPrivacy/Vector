//! One-line previews of a message (`contentToPreviewText` / `contentToPreviewHtml`, main.js):
//! the chat list's last message and a reply's quote. Block markdown is flattened, headers
//! demote to bold, and only bold, italic, strikes and (unrevealable) spoilers survive.

use gpui::FontWeight;
use unicode_segmentation::UnicodeSegmentation;

use crate::emoji;
use crate::inline::{Atom, Kind, Look};

const KNOWN_TAGS: &[&str] = &[
    "a",
    "abbr",
    "b",
    "blockquote",
    "br",
    "code",
    "del",
    "details",
    "div",
    "em",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "i",
    "li",
    "ol",
    "p",
    "pre",
    "s",
    "span",
    "strong",
    "sub",
    "summary",
    "sup",
    "table",
    "tbody",
    "td",
    "th",
    "thead",
    "tr",
    "u",
    "ul",
];

/// `<br>` to a space, known tags stripped, unknown angle text kept.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let name_start = after.strip_prefix('/').unwrap_or(after);
        let name: String = name_start
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        let known = KNOWN_TAGS.contains(&name.to_ascii_lowercase().as_str());
        match after.find('>') {
            Some(j) if known => {
                if name.eq_ignore_ascii_case("br") {
                    out.push(' ');
                }
                rest = &after[j + 1..];
            }
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `contentToPreviewText`: one line of prose.
pub fn plain(content: &str) -> String {
    let text = strip_tags(content);
    let mut lines: Vec<String> = Vec::new();
    let mut in_fence = false;
    let mut ruled = false;
    for line in text.split('\n') {
        if let Some(info) = line.strip_prefix("```") {
            if in_fence {
                in_fence = false;
            } else {
                in_fence = true;
                let lang = info.split_whitespace().next().unwrap_or("");
                lines.push(format!(" {} ", codeblock_label(lang)));
            }
            continue;
        }
        if in_fence {
            continue;
        }
        let mut l = line;
        let hashes = l.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && l[hashes..].starts_with(char::is_whitespace) {
            l = l[hashes..].trim_start();
        }
        if let Some(q) = l.strip_prefix('>') {
            l = q.strip_prefix(' ').unwrap_or(q);
        }
        if l.len() >= 3 && (l.chars().all(|c| c == '-') || l.chars().all(|c| c == '*')) {
            ruled = true;
            continue;
        }
        lines.push(l.to_string());
    }
    let joined = lines.join("\n");
    // Inline code keeps its text.
    let mut out = String::with_capacity(joined.len());
    let mut parts = joined.split('`');
    if let Some(first) = parts.next() {
        out.push_str(first);
    }
    let rest: Vec<&str> = parts.collect();
    let n = rest.len();
    for (i, p) in rest.iter().enumerate() {
        // An odd backtick out keeps its tick.
        if n % 2 == 1 && i == n - 1 {
            out.push('`');
        }
        out.push_str(p);
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() && ruled {
        "Divider".into()
    } else {
        collapsed
    }
}

/// "Rust Codeblock" for a fence's language, else "Codeblock".
fn codeblock_label(lang: &str) -> String {
    let name = match lang.to_ascii_lowercase().as_str() {
        "rust" | "rs" => "Rust",
        "js" | "javascript" => "JavaScript",
        "ts" | "typescript" => "TypeScript",
        "py" | "python" => "Python",
        "go" | "golang" => "Go",
        "c" => "C",
        "cpp" | "c++" => "C++",
        "java" => "Java",
        "json" => "JSON",
        "bash" | "sh" | "shell" => "Bash",
        "html" | "xml" => "HTML",
        "css" => "CSS",
        "sql" => "SQL",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "kotlin" | "kt" => "Kotlin",
        "swift" => "Swift",
        "ruby" | "rb" => "Ruby",
        "php" => "PHP",
        "lua" => "Lua",
        "markdown" | "md" => "Markdown",
        "diff" => "Diff",
        _ => "",
    };
    if name.is_empty() {
        "Codeblock".into()
    } else {
        format!("{name} Codeblock")
    }
}

/// `truncateEmojiAware`: at most `max` graphemes, a `:shortcode:` priced as two.
fn truncate(text: &str, max: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for g in text.graphemes(true) {
        if used + 1 > max {
            out.push('…');
            return out;
        }
        out.push_str(g);
        used += 1;
    }
    out
}

/// `balanceInlineMarkdown`: re-close delimiters the cut left open.
fn balance(mut t: String) -> String {
    if t.matches("**").count() % 2 == 1 {
        t.push_str("**");
    }
    let singles = t.replace("**", "").matches('*').count();
    if singles % 2 == 1 {
        t.push('*');
    }
    if t.matches("~~").count() % 2 == 1 {
        t.push_str("~~");
    }
    if t.matches("||").count() % 2 == 1 {
        t.push_str("||");
    }
    t
}

/// A reply quote's snippet (`buildReplyPreviewHtml`): flattened before it is cut, so headers
/// don't demote to bold.
pub fn reply_atoms(content: &str, look: &Look) -> Vec<Atom> {
    atoms(&balance(truncate(&plain(content), 50)), usize::MAX, look)
}

/// The preview as inline atoms, in `look` (`contentToPreviewHtml`).
pub fn atoms(content: &str, max: usize, look: &Look) -> Vec<Atom> {
    // Headers demote to bold.
    let demoted: String = content
        .split('\n')
        .map(|l| {
            let h = l.chars().take_while(|&c| c == '#').count();
            if (1..=6).contains(&h)
                && l[h..].starts_with(char::is_whitespace)
                && !l[h..].trim().is_empty()
            {
                format!("**{}**", l[h..].trim())
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let flat = plain(&demoted);
    let text = if max == usize::MAX {
        flat
    } else {
        balance(truncate(&flat, max))
    };
    let mut out = Vec::new();
    let (mut bold, mut italic, mut strike, mut spoiler) = (false, false, false, false);
    let mut buf = String::new();
    let flush = |buf: &mut String,
                 out: &mut Vec<Atom>,
                 bold: bool,
                 italic: bool,
                 strike: bool,
                 spoiler: bool| {
        if buf.is_empty() {
            return;
        }
        let mut l = look.clone();
        if bold {
            l.weight = FontWeight::BOLD;
        }
        l.italic = italic;
        l.strike = strike;
        let kind = if spoiler {
            Kind::Spoiler {
                id: u32::MAX,
                revealed: false,
            }
        } else {
            Kind::Plain
        };
        let mut start = 0;
        for (i, g) in buf.grapheme_indices(true) {
            if let Some(src) = emoji::twemoji(g) {
                if i > start {
                    out.push(Atom::Text {
                        text: buf[start..i].to_string().into(),
                        look: l.clone(),
                        kind: kind.clone(),
                    });
                }
                out.push(Atom::Image {
                    src: src.into(),
                    size: l.size * 1.15,
                    drop: l.size * 0.2,
                });
                start = i + g.len();
            }
        }
        if start < buf.len() {
            out.push(Atom::Text {
                text: buf[start..].to_string().into(),
                look: l,
                kind,
            });
        }
        buf.clear();
    };
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let toggle = if rest.starts_with("**") {
            Some((2, 0))
        } else if rest.starts_with("~~") {
            Some((2, 2))
        } else if rest.starts_with("||") {
            Some((2, 3))
        } else if rest.starts_with('*') {
            Some((1, 1))
        } else {
            None
        };
        if let Some((len, which)) = toggle {
            // Only a delimiter with a partner later toggles (`.+?` in the JS).
            let delim = &rest[..len];
            if rest[len..].contains(delim) || [bold, italic, strike, spoiler][which] {
                flush(&mut buf, &mut out, bold, italic, strike, spoiler);
                match which {
                    0 => bold = !bold,
                    1 => italic = !italic,
                    2 => strike = !strike,
                    _ => spoiler = !spoiler,
                }
                rest = &rest[len..];
                continue;
            }
        }
        let ch = rest.chars().next().unwrap();
        buf.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    flush(&mut buf, &mut out, bold, italic, strike, spoiler);
    out
}
