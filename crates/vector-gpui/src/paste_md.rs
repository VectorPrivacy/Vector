//! Rich clipboard text as markdown, as the desktop composer's `cmpHtmlToMarkdown`: what an app
//! copied as styled HTML (bold, italic, strikethrough, code, links) marked up over the clipboard's
//! own plain text. The plain text stays the authority on every character and line break; the HTML
//! only says which characters are styled.

use dom_query::{Document, NodeRef};

#[derive(Clone, Default, PartialEq)]
struct Fmt {
    b: bool,
    i: bool,
    s: bool,
    code: bool,
    pre: bool,
    href: Option<String>,
}

impl Fmt {
    fn styled(&self) -> bool {
        self.b || self.i || self.s || self.code || self.pre || self.href.is_some()
    }
}

const SKIP: &[&str] = &[
    "script", "style", "head", "title", "meta", "link", "template", "noscript", "svg", "iframe",
    "object", "button", "select",
];

/// The clipboard's HTML, read beside GPUI's own clipboard, which carries no HTML.
pub fn clipboard_html() -> Option<String> {
    arboard::Clipboard::new().ok()?.get().html().ok()
}

fn style_props(style: &str) -> impl Iterator<Item = (String, String)> + '_ {
    style.split(';').filter_map(|decl| {
        let (k, v) = decl.split_once(':')?;
        Some((k.trim().to_ascii_lowercase(), v.trim().to_ascii_lowercase()))
    })
}

fn hidden(node: &NodeRef) -> bool {
    node.attr("style").is_some_and(|st| {
        style_props(&st).any(|(k, v)| (k == "display" && v == "none") || (k == "visibility" && v == "hidden"))
    })
}

/// An element's formatting, inherited from `parent`: its tag, then its inline style.
fn format(tag: &str, node: &NodeRef, parent: &Fmt) -> Fmt {
    let mut f = parent.clone();
    match tag {
        "b" | "strong" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => f.b = true,
        "i" | "em" => f.i = true,
        "s" | "strike" | "del" => f.s = true,
        "code" | "kbd" | "samp" | "tt" => f.code = true,
        "pre" => f.pre = true,
        "a" => {
            let href = node.attr("href").map(|h| h.trim().to_string()).unwrap_or_default();
            let lower = href.to_ascii_lowercase();
            if (lower.starts_with("http://") || lower.starts_with("https://"))
                && href.len() > href.find("//").unwrap_or(0) + 2
                && !href.chars().any(char::is_whitespace)
            {
                f.href = Some(href);
            }
        }
        _ => {}
    }
    // Docs editors style spans rather than using tags, and wrap whole documents in a
    // <b style="font-weight:normal">, so the inline style has the last word.
    if let Some(st) = node.attr("style") {
        for (k, v) in style_props(&st) {
            match k.as_str() {
                "font-weight" if !v.is_empty() => {
                    let n: u32 = v.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0);
                    f.b = v == "bold" || v == "bolder" || n >= 600;
                }
                "font-style" if !v.is_empty() => f.i = v == "italic" || v.starts_with("oblique"),
                "text-decoration" | "text-decoration-line" if v.contains("line-through") => f.s = true,
                _ => {}
            }
        }
    }
    f
}

fn walk(node: &NodeRef, f: &Fmt, chars: &mut Vec<(char, Fmt)>, styled: &mut bool) {
    for child in node.children() {
        if child.is_text() {
            chars.extend(child.text().chars().map(|c| (c, f.clone())));
            continue;
        }
        if !child.is_element() {
            continue;
        }
        let tag = child.node_name().map(|t| t.to_ascii_lowercase()).unwrap_or_default();
        if SKIP.contains(&tag.as_str()) || hidden(&child) {
            continue;
        }
        if tag == "img" {
            // An emoji copied as an image is its alt text in the plain copy.
            let alt = child.attr("alt").map(|a| a.to_string()).unwrap_or_default();
            if alt.chars().count() <= 16 {
                chars.extend(alt.chars().map(|c| (c, f.clone())));
            }
            continue;
        }
        let g = format(&tag, &child, f);
        *styled |= g.styled();
        walk(&child, &g, chars, styled);
    }
}

/// Markdown for a paste, or None when nothing is styled or the two copies disagree on the text.
pub fn html_to_markdown(html: &str, plain: &str) -> Option<String> {
    if html.is_empty() || html.len() > 2_000_000 || plain.len() > 200_000 {
        return None;
    }
    // Windows (CF_HTML) brackets the copied part inside a whole document.
    let mut html = html;
    if let (Some(a), Some(b)) = (html.find("<!--StartFragment-->"), html.find("<!--EndFragment-->")) {
        if b > a {
            html = &html[a + 20..b];
        }
    }
    let doc = Document::from(html);
    let body = doc.select("body").nodes().first().cloned()?;
    let mut chars = Vec::new();
    let mut styled = false;
    walk(&body, &Fmt::default(), &mut chars, &mut styled);
    if !styled {
        return None;
    }

    // Lay the HTML's characters over the plain text's, whitespace aside. A few the plain
    // copy adds (list bullets, numbering) are stepped over unstyled.
    let p: Vec<char> = plain.chars().collect();
    let mut fmt: Vec<Option<Fmt>> = vec![None; p.len()];
    let mut j = 0;
    for (c, f) in chars {
        if c.is_whitespace() {
            continue;
        }
        while j < p.len() && p[j].is_whitespace() {
            j += 1;
        }
        let mut k = j;
        while k < p.len() && k - j < 8 && p[k] != c {
            k += 1;
        }
        if p.get(k) != Some(&c) {
            return None;
        }
        fmt[k] = Some(f);
        j = k + 1;
    }

    let key = |i: usize| -> String {
        let Some(f) = &fmt[i] else { return String::new() };
        let look = if f.pre {
            "pre".to_string()
        } else if f.code {
            "code".to_string()
        } else {
            format!("{}{}{}", if f.b { "b" } else { "" }, if f.i { "i" } else { "" }, if f.s { "s" } else { "" })
        };
        if look.is_empty() && f.href.is_none() {
            String::new()
        } else {
            format!("{look}|{}", f.href.as_deref().unwrap_or(""))
        }
    };
    let slice = |a: usize, b: usize| -> String { p[a.min(p.len())..b.min(p.len())].iter().collect() };
    let has = |from: usize, to: usize, text: &str, m: &str| -> bool {
        let n = m.chars().count();
        (slice(from.saturating_sub(n), from) == m && slice(to, to + n) == m)
            || (text.chars().count() > 2 * n && text.starts_with(m) && text.ends_with(m))
    };
    let wrap = |text: String, f: &Fmt, from: usize, to: usize| -> String {
        let mut out = text.clone();
        if f.code {
            if !text.contains('`') && !has(from, to, &text, "`") {
                out = format!("`{text}`");
            }
        } else {
            let stars = match (f.b, f.i) {
                (true, true) => "***",
                (true, false) => "**",
                (false, true) => "*",
                _ => "",
            };
            if !stars.is_empty() && !has(from, to, &text, stars) {
                out = format!("{stars}{out}{stars}");
            }
            if f.s && !has(from, to, &text, "~~") {
                out = format!("~~{out}~~");
            }
        }
        let Some(href) = &f.href else { return out };
        let bare = |u: &str| -> String {
            let l = u.to_ascii_lowercase();
            let u = if l.starts_with("https://") { &u[8..] } else if l.starts_with("http://") { &u[7..] } else { u };
            u.strip_suffix('/').unwrap_or(u).to_string()
        };
        if bare(&text) == bare(href) {
            return text;
        }
        let href = href.replace(')', "%29");
        if text.contains('[') || text.contains(']') {
            format!("{out} ({href})")
        } else {
            format!("[{out}]({href})")
        }
    };

    let mut lines = Vec::new();
    let mut start = 0;
    for i in 0..=p.len() {
        if i < p.len() && p[i] != '\n' {
            continue;
        }
        lines.push((start, i));
        start = i + 1;
    }
    let is_pre = |(s, e): (usize, usize)| -> bool {
        let mut any = false;
        for i in s..e {
            if p[i].is_whitespace() {
                continue;
            }
            if !fmt[i].as_ref().is_some_and(|f| f.pre) {
                return false;
            }
            any = true;
        }
        any
    };
    let mut out: Vec<String> = Vec::new();
    let mut li = 0;
    while li < lines.len() {
        if is_pre(lines[li]) {
            // A preformatted block becomes a fence, its lines verbatim.
            let mut end = li;
            while end + 1 < lines.len()
                && (is_pre(lines[end + 1])
                    || (lines[end + 1].0 == lines[end + 1].1 && end + 2 < lines.len() && is_pre(lines[end + 2])))
            {
                end += 1;
            }
            out.push("```".into());
            for &(s, e) in &lines[li..=end] {
                out.push(slice(s, e));
            }
            out.push("```".into());
            li = end + 1;
            continue;
        }
        let (s, e) = lines[li];
        let mut line = String::new();
        let mut i = s;
        while i < e {
            let k = key(i);
            if p[i].is_whitespace() || k.is_empty() {
                line.push(p[i]);
                i += 1;
                continue;
            }
            // A run spans whitespace while the next styled character matches it, but never
            // ends on whitespace: a mark against a space isn't one.
            let mut last = i;
            for x in i + 1..e {
                if p[x].is_whitespace() {
                    continue;
                }
                if key(x) != k {
                    break;
                }
                last = x;
            }
            line.push_str(&wrap(slice(i, last + 1), fmt[i].as_ref().unwrap(), i, last + 1));
            i = last + 1;
        }
        out.push(line);
        li += 1;
    }
    let md = out.join("\n");
    (md != plain).then_some(md)
}

#[cfg(test)]
mod tests {
    use super::html_to_markdown as md;

    #[test]
    fn marks_styled_runs() {
        assert_eq!(md("<p>a <b>bold</b> and <i>it</i></p>", "a bold and it").as_deref(), Some("a **bold** and *it*"));
        assert_eq!(md("<span style=\"font-weight:700\">x y</span> z", "x y z").as_deref(), Some("**x y** z"));
        assert_eq!(md("<code>f()</code>", "f()").as_deref(), Some("`f()`"));
        assert_eq!(md("<del>gone</del>", "gone").as_deref(), Some("~~gone~~"));
    }

    #[test]
    fn links_and_bare_urls() {
        assert_eq!(md("<a href=\"https://x.io/a\">site</a>", "site").as_deref(), Some("[site](https://x.io/a)"));
        assert_eq!(md("<a href=\"https://x.io/\">x.io</a>", "x.io"), None);
    }

    #[test]
    fn docs_wrapper_is_not_bold() {
        assert_eq!(md("<b style=\"font-weight:normal\"><span>plain</span></b>", "plain"), None);
    }

    #[test]
    fn pre_becomes_a_fence() {
        assert_eq!(md("<pre>let a;\nlet b;</pre>", "let a;\nlet b;").as_deref(), Some("```\nlet a;\nlet b;\n```"));
    }

    #[test]
    fn disagreeing_text_stays_plain() {
        assert_eq!(md("<b>other</b>", "text"), None);
        assert_eq!(md("<p>unstyled</p>", "unstyled"), None);
    }

    #[test]
    fn list_bullets_are_stepped_over() {
        assert_eq!(md("<ul><li><b>one</b></li><li>two</li></ul>", "• one\n• two").as_deref(), Some("• **one**\n• two"));
    }

    #[test]
    fn cf_html_fragment() {
        let h = "Version:0.9\r\n<html><body><!--StartFragment--><em>hi</em><!--EndFragment--></body></html>";
        assert_eq!(md(h, "hi").as_deref(), Some("*hi*"));
    }
}
