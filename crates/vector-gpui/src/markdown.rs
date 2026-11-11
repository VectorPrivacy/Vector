//! Message markdown, as `js/markdown.js` configures Marked: GFM with single newlines as breaks,
//! `||spoilers||`, `-# subtext`, `-`/`*` lists only, strict blockquotes, no setext headings,
//! no images, no autolinked bare text beyond Vector's own linkify, and only http(s)/mailto
//! links whose text doesn't claim another site.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::{FontWeight, Hsla, SharedString};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use unicode_segmentation::UnicodeSegmentation;

use crate::emoji;
use crate::inline::{Atom, Kind, Look};

pub const MONO: &str = if cfg!(target_os = "macos") {
    "Menlo"
} else if cfg!(target_os = "windows") {
    "Consolas"
} else {
    "DejaVu Sans Mono"
};

/// First code point of the markers standing in for spoilers and shrugs while Marked's grammar
/// parses around them (Supplementary Private Use Area-A: never typed).
const MARK: u32 = 0xF0000;

#[derive(Clone, Copy, PartialEq)]
pub enum ParaKind {
    Text,
    Heading(u8),
    Sub,
}

pub struct Para {
    pub atoms: Rc<[Atom]>,
    pub base: Look,
    pub kind: ParaKind,
}

pub enum Block {
    Para(Para),
    Code {
        lang: SharedString,
        text: SharedString,
    },
    Quote(Vec<Block>),
    List {
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Rule,
    Table {
        align: Vec<Alignment>,
        head: Vec<Para>,
        rows: Vec<Vec<Para>>,
    },
}

/// What the text is set in, and what has been revealed.
pub struct Ctx<'a> {
    pub base: Look,
    pub revealed: &'a HashSet<u32>,
    /// NIP-30 custom emoji: what a `:shortcode:` draws as, when the message names it.
    pub custom: Option<&'a dyn Fn(&str) -> Option<crate::media::Custom>>,
}

enum Swap {
    Spoiler(String),
    Shrug,
}

/// Source lines rewritten so CommonMark reads them as Marked's configuration does, with
/// spoilers and shrugs swapped for markers. Returns the chunks (`true` = a subtext line).
fn prepare(src: &str, swaps: &mut Vec<Swap>) -> Vec<(bool, String)> {
    let mut chunks: Vec<(bool, String)> = Vec::new();
    let mut fence: Option<String> = None;
    let mut prev_blank = true;
    let mut prev_quote = false;
    for line in src.split('\n') {
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        if let Some(f) = &fence {
            if trimmed.starts_with(f.as_str()) && indent <= 3 {
                fence = None;
            }
            push_line(&mut chunks, false, line.to_string());
            continue;
        }
        if indent <= 3 && (trimmed.starts_with("```") || trimmed.starts_with("~~~")) {
            let ch = trimmed.chars().next().unwrap();
            fence = Some(trimmed.chars().take_while(|&c| c == ch).collect());
            push_line(&mut chunks, false, line.to_string());
            prev_blank = false;
            prev_quote = false;
            continue;
        }
        let blank = trimmed.trim().is_empty();
        if let Some(rest) = line.strip_prefix("-# ") {
            chunks.push((true, swap_inline(rest, swaps)));
            prev_blank = true;
            prev_quote = false;
            continue;
        }
        let mut out = String::new();
        let quote = indent <= 3 && trimmed.starts_with('>');
        // Strict blockquotes: a line without `>` ends the quote.
        if prev_quote && !quote && !blank {
            out.push('\n');
        }
        let body = trimmed.trim_end();
        if indent <= 3 && !prev_blank && !body.is_empty() {
            if body.chars().all(|c| c == '=') {
                // Not a setext heading: literal text.
                out.push_str(&" ".repeat(indent));
                out.push('\\');
                out.push_str(trimmed);
                push_line(&mut chunks, false, out);
                prev_blank = false;
                prev_quote = false;
                continue;
            }
            if body.chars().all(|c| c == '-') {
                if body.len() >= 3 {
                    // A divider under text, not a heading.
                    out.push('\n');
                    out.push_str(line);
                } else {
                    out.push_str(&" ".repeat(indent));
                    out.push('\\');
                    out.push_str(trimmed);
                }
                push_line(&mut chunks, false, out);
                prev_blank = false;
                prev_quote = false;
                continue;
            }
        }
        // Only `-` and `*` open a list.
        if indent <= 3 && (trimmed.starts_with("+ ") || trimmed == "+") {
            out.push_str(&" ".repeat(indent));
            out.push('\\');
            out.push_str(&swap_inline(trimmed, swaps));
        } else {
            out.push_str(&swap_inline(line, swaps));
        }
        push_line(&mut chunks, false, out);
        prev_blank = blank;
        prev_quote = quote;
    }
    chunks
}

fn push_line(chunks: &mut Vec<(bool, String)>, sub: bool, line: String) {
    match chunks.last_mut() {
        Some((false, s)) if !sub => {
            s.push('\n');
            s.push_str(&line);
        }
        _ => chunks.push((sub, line)),
    }
}

fn marker(i: usize) -> char {
    char::from_u32(MARK + i as u32).unwrap_or('\u{fffd}')
}

/// Spoilers and shrugs outside code spans become markers.
fn swap_inline(line: &str, swaps: &mut Vec<Swap>) -> String {
    let mut out = String::with_capacity(line.len());
    let b = line.as_bytes();
    let mut i = 0;
    while i < line.len() {
        if b[i] == b'`' {
            let run = b[i..].iter().take_while(|&&c| c == b'`').count();
            let ticks = &line[i..i + run];
            if let Some(close) = find_run(&line[i + run..], ticks) {
                let end = i + run + close + run;
                out.push_str(&line[i..end]);
                i = end;
            } else {
                out.push_str(ticks);
                i += run;
            }
            continue;
        }
        let rest = &line[i..];
        if let Some(inner) = rest.strip_prefix("||") {
            if let Some(end) = inner.find('|') {
                if end > 0 && inner[end..].starts_with("||") {
                    out.push(marker(swaps.len()));
                    swaps.push(Swap::Spoiler(inner[..end].to_string()));
                    i += 2 + end + 2;
                    continue;
                }
            }
        }
        for shrug in ["¯\\\\_(ツ)_/¯", "¯\\_(ツ)_/¯"] {
            if rest.starts_with(shrug) {
                out.push(marker(swaps.len()));
                swaps.push(Swap::Shrug);
                i += shrug.len();
                break;
            }
        }
        if i < line.len() && line[i..].as_ptr() == rest.as_ptr() {
            let ch = rest.chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Where a backtick run exactly `ticks` long next starts.
fn find_run(s: &str, ticks: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'`' {
            let run = b[i..].iter().take_while(|&&c| c == b'`').count();
            if run == ticks.len() {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

/// Where inline content goes while it is being collected.
#[derive(Default)]
struct Flags {
    bold: u32,
    italic: u32,
    strike: u32,
    link: Vec<Option<SharedString>>,
    image: Option<(String, String)>,
}

enum Frame {
    Root(Vec<Block>),
    Quote(Vec<Block>),
    List {
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Item(Vec<Block>),
    Table {
        align: Vec<Alignment>,
        head: Vec<Para>,
        rows: Vec<Vec<Para>>,
        row: Vec<Para>,
        in_head: bool,
    },
}

impl Frame {
    fn blocks(&mut self) -> Option<&mut Vec<Block>> {
        match self {
            Frame::Root(b) | Frame::Quote(b) | Frame::Item(b) => Some(b),
            _ => None,
        }
    }
}

struct Builder<'a> {
    ctx: &'a Ctx<'a>,
    swaps: &'a [Swap],
    stack: Vec<Frame>,
    atoms: Vec<Atom>,
    para: Option<ParaKind>,
    flags: Flags,
    code: Option<(SharedString, String)>,
    quote_depth: u32,
    spoilers: u32,
}

impl Builder<'_> {
    /// The block's own look, before inline styling.
    fn block_look(&self, kind: ParaKind) -> Look {
        let mut l = self.ctx.base.clone();
        if self.quote_depth > 0 {
            l.color = gpui::white().opacity(0.8);
            l.italic = true;
        }
        match kind {
            ParaKind::Text => {}
            ParaKind::Heading(n) => {
                let f = [2.0, 1.5, 1.17, 1.0, 0.83, 0.67][(n as usize).clamp(1, 6) - 1];
                l.size *= f;
                l.line_height = l.size * 1.2;
                l.weight = FontWeight::BOLD;
            }
            ParaKind::Sub => {
                l.size *= 0.8;
                l.line_height = l.size * 1.4;
                l.color = l.color.opacity(l.color.a * 0.7);
            }
        }
        l
    }

    fn look(&self) -> Look {
        let mut l = self.block_look(self.para.unwrap_or(ParaKind::Text));
        if self.flags.bold > 0 {
            l.weight = FontWeight::BOLD;
        }
        if self.flags.italic > 0 {
            l.italic = true;
            l.color = l.color.opacity(l.color.a * 0.7);
        }
        if self.flags.strike > 0 {
            l.strike = true;
        }
        l
    }

    fn link(&self) -> Option<SharedString> {
        self.flags.link.last().cloned().flatten()
    }

    fn open_para(&mut self, kind: ParaKind) {
        self.flush_para();
        self.para = Some(kind);
    }

    /// Text outside a paragraph (a tight list item, a table cell) opens one.
    fn ensure_para(&mut self) {
        if self.para.is_none() {
            self.para = Some(ParaKind::Text);
        }
    }

    fn flush_para(&mut self) {
        let Some(kind) = self.para.take() else { return };
        let atoms = std::mem::take(&mut self.atoms);
        if atoms.is_empty() {
            return;
        }
        let base = self.block_look(kind);
        let para = Para {
            atoms: atoms.into(),
            base,
            kind,
        };
        match self.stack.last_mut() {
            Some(Frame::Table { row, .. }) => row.push(para),
            Some(f) => {
                if let Some(blocks) = f.blocks() {
                    blocks.push(Block::Para(para));
                }
            }
            None => {}
        }
    }

    fn push_block(&mut self, b: Block) {
        self.flush_para();
        if let Some(blocks) = self.stack.last_mut().and_then(Frame::blocks) {
            blocks.push(b);
        }
    }

    fn text(&mut self, t: &str) {
        if let Some((_, alt)) = &mut self.flags.image {
            alt.push_str(t);
            return;
        }
        self.ensure_para();
        let mut rest = t;
        while !rest.is_empty() {
            let at = rest
                .char_indices()
                .find(|(_, c)| (*c as u32) >= MARK && (*c as u32) < MARK + self.swaps.len() as u32);
            let Some((i, ch)) = at else {
                self.prose(rest);
                break;
            };
            self.prose(&rest[..i]);
            match &self.swaps[(ch as u32 - MARK) as usize] {
                Swap::Shrug => self.styled("¯\\_(ツ)_/¯", Kind::Plain),
                Swap::Spoiler(s) => {
                    let id = self.spoilers;
                    self.spoilers += 1;
                    let kind = Kind::Spoiler {
                        id,
                        revealed: self.ctx.revealed.contains(&id),
                    };
                    let s = s.clone();
                    self.emoji_text(&s, kind);
                }
            }
            rest = &rest[i + ch.len_utf8()..];
        }
    }

    /// Plain prose: bare http(s) URLs become links (`linkifyUrls`).
    fn prose(&mut self, t: &str) {
        if t.is_empty() {
            return;
        }
        if let Some(url) = self.link() {
            self.emoji_text(t, Kind::Link(url));
            return;
        }
        let mut last = 0;
        for (start, end) in find_urls(t) {
            self.emoji_text(&t[last..start], Kind::Plain);
            let url: SharedString = t[start..end].to_string().into();
            self.styled(&t[start..end], Kind::Link(url));
            last = end;
        }
        self.emoji_text(&t[last..], Kind::Plain);
    }

    /// Text with its `:shortcode:`s swapped for the custom emoji the message carries.
    fn emoji_text(&mut self, t: &str, kind: Kind) {
        let Some(custom) = self.ctx.custom else {
            return self.twemoji_text(t, kind);
        };
        let mut last = 0;
        for (start, end) in shortcodes(t) {
            let code = &t[start + 1..end - 1];
            let Some(c) = custom(code) else { continue };
            let size = self.look().size;
            match c {
                crate::media::Custom::Missing(_) => continue,
                crate::media::Custom::Ready(image) => {
                    self.twemoji_text(&t[last..start], kind.clone());
                    self.atoms.push(Atom::Image {
                        src: image.into(),
                        size: size * 1.15,
                        drop: size * 0.2,
                    });
                }
                crate::media::Custom::Pending => {
                    self.twemoji_text(&t[last..start], kind.clone());
                    self.atoms.push(Atom::Gap(size * 1.15));
                }
            }
            last = end;
        }
        self.twemoji_text(&t[last..], kind);
    }

    /// Text with its emoji swapped for twemoji images.
    fn twemoji_text(&mut self, t: &str, kind: Kind) {
        let mut run_start = 0;
        for (i, g) in t.grapheme_indices(true) {
            if let Some(src) = emoji::twemoji(g) {
                self.styled(&t[run_start..i], kind.clone());
                let size = self.look().size;
                self.atoms.push(Atom::Image {
                    src: src.into(),
                    size: size * 1.15,
                    drop: size * 0.2,
                });
                run_start = i + g.len();
            }
        }
        self.styled(&t[run_start..], kind);
    }

    fn styled(&mut self, t: &str, kind: Kind) {
        if t.is_empty() {
            return;
        }
        let mut look = self.look();
        if let Kind::Link(_) = kind {
            look.color = gpui::rgb(0x59fcb3).into();
        }
        // Merge into the previous run when nothing differs.
        if let Some(Atom::Text {
            text,
            look: l,
            kind: k,
        }) = self.atoms.last_mut()
        {
            if *l == look && *k == kind && !matches!(kind, Kind::Spoiler { .. } | Kind::Code) {
                *text = format!("{text}{t}").into();
                return;
            }
        }
        self.atoms.push(Atom::Text {
            text: t.to_string().into(),
            look,
            kind,
        });
    }

    fn code_span(&mut self, t: &str) {
        self.ensure_para();
        let mut look = self.look();
        look.family = MONO.into();
        look.size *= 0.9;
        look.line_height = look.size * 1.4;
        self.atoms.push(Atom::Text {
            text: t.replace('\n', " ").into(),
            look,
            kind: Kind::Code,
        });
    }

    fn inline_html(&mut self, html: &str) {
        let tag = html.trim_start_matches('<').trim_start_matches('/');
        let name: String = tag
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        let closing = html.starts_with("</");
        let delta = |n: &mut u32| {
            if closing {
                *n = n.saturating_sub(1)
            } else {
                *n += 1
            }
        };
        match name.as_str() {
            "b" | "strong" => delta(&mut self.flags.bold),
            "i" | "em" => delta(&mut self.flags.italic),
            "s" | "del" => delta(&mut self.flags.strike),
            "br" => {
                self.ensure_para();
                self.atoms.push(Atom::Break);
            }
            "u" | "span" | "sub" | "sup" | "abbr" | "small" | "code" => {}
            _ => self.text(html),
        }
    }

    fn event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if let Some((_, code)) = &mut self.code {
                    code.push_str(&t);
                } else {
                    self.text(&t);
                }
            }
            Event::Code(t) => self.code_span(&t),
            Event::InlineHtml(h) => self.inline_html(&h),
            Event::Html(h) => {
                self.ensure_para();
                self.text(h.trim_end_matches('\n'));
            }
            Event::SoftBreak | Event::HardBreak => {
                self.ensure_para();
                self.atoms.push(Atom::Break);
            }
            Event::Rule => self.push_block(Block::Rule),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => self.open_para(ParaKind::Text),
            Tag::Heading { level, .. } => {
                let n = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                self.open_para(ParaKind::Heading(n));
            }
            Tag::BlockQuote(_) => {
                self.flush_para();
                self.quote_depth += 1;
                self.stack.push(Frame::Quote(Vec::new()));
            }
            Tag::CodeBlock(kind) => {
                self.flush_para();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_ascii_lowercase(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang.into(), String::new()));
            }
            Tag::List(start) => {
                self.flush_para();
                self.stack.push(Frame::List {
                    start,
                    items: Vec::new(),
                });
            }
            Tag::Item => {
                self.flush_para();
                self.stack.push(Frame::Item(Vec::new()));
            }
            Tag::Emphasis => self.flags.italic += 1,
            Tag::Strong => self.flags.bold += 1,
            Tag::Strikethrough => self.flags.strike += 1,
            Tag::Link { dest_url, .. } => self.flags.link.push(Some(dest_url.to_string().into())),
            Tag::Image { dest_url, .. } => {
                self.flags.image = Some((dest_url.to_string(), String::new()))
            }
            Tag::Table(align) => {
                self.flush_para();
                self.stack.push(Frame::Table {
                    align,
                    head: Vec::new(),
                    rows: Vec::new(),
                    row: Vec::new(),
                    in_head: false,
                });
            }
            Tag::TableHead => {
                if let Some(Frame::Table { in_head, .. }) = self.stack.last_mut() {
                    *in_head = true;
                }
            }
            Tag::TableCell => self.para = Some(ParaKind::Text),
            Tag::HtmlBlock => self.ensure_para(),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::HtmlBlock => self.flush_para(),
            TagEnd::BlockQuote(_) => {
                self.flush_para();
                self.quote_depth -= 1;
                if let Some(Frame::Quote(blocks)) = self.stack.pop() {
                    self.push_block(Block::Quote(blocks));
                }
            }
            TagEnd::CodeBlock => {
                if let Some((lang, mut text)) = self.code.take() {
                    if text.ends_with('\n') {
                        text.pop();
                    }
                    self.push_block(Block::Code {
                        lang,
                        text: text.into(),
                    });
                }
            }
            TagEnd::Item => {
                self.flush_para();
                if let Some(Frame::Item(blocks)) = self.stack.pop() {
                    if let Some(Frame::List { items, .. }) = self.stack.last_mut() {
                        items.push(blocks);
                    }
                }
            }
            TagEnd::List(_) => {
                self.flush_para();
                if let Some(Frame::List { start, items }) = self.stack.pop() {
                    self.push_block(Block::List { start, items });
                }
            }
            TagEnd::Emphasis => self.flags.italic = self.flags.italic.saturating_sub(1),
            TagEnd::Strong => self.flags.bold = self.flags.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.flags.strike = self.flags.strike.saturating_sub(1),
            TagEnd::Link => {
                self.flags.link.pop();
            }
            TagEnd::Image => {
                if let Some((url, alt)) = self.flags.image.take() {
                    self.text(&format!("![{alt}]({url})"));
                }
            }
            TagEnd::TableCell => {
                self.ensure_para();
                if self.atoms.is_empty() {
                    self.atoms.push(Atom::Text {
                        text: "".into(),
                        look: self.look(),
                        kind: Kind::Plain,
                    });
                }
                self.flush_para();
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(Frame::Table {
                    head,
                    rows,
                    row,
                    in_head,
                    ..
                }) = self.stack.last_mut()
                {
                    let cells = std::mem::take(row);
                    if *in_head {
                        *head = cells;
                        *in_head = false;
                    } else {
                        rows.push(cells);
                    }
                }
            }
            TagEnd::Table => {
                if let Some(Frame::Table {
                    align, head, rows, ..
                }) = self.stack.pop()
                {
                    self.push_block(Block::Table { align, head, rows });
                }
            }
            _ => {}
        }
    }
}

/// A markdown link renders as one only to http(s)/mailto, and only when its text claims no
/// other site (`mdLinkTextSpoofsHref`); otherwise its source shows.
fn link_allowed(text: &str, href: &str) -> bool {
    let lower = href.trim().to_ascii_lowercase();
    if !(lower.starts_with("http:") || lower.starts_with("https:") || lower.starts_with("mailto:"))
    {
        return false;
    }
    let actual = if lower.starts_with("mailto:") {
        lower[7..]
            .split(['?', '#'])
            .next()
            .and_then(|a| a.rsplit_once('@'))
            .map(|(_, d)| d.to_string())
    } else {
        host_of(&lower)
    };
    for frag in text.split(|c: char| c.is_whitespace() || "<>\"'()[]\\".contains(c)) {
        let f = frag.to_ascii_lowercase();
        let claimed = if f.starts_with("http://") || f.starts_with("https://") {
            host_of(&f)
        } else if f.starts_with("www.") || bare_domain(&f) {
            host_of(&format!("https://{f}"))
        } else {
            continue;
        };
        let (Some(c), Some(a)) = (claimed, actual.as_deref()) else {
            return false;
        };
        if !(c == a || c.ends_with(&format!(".{a}")) || a.ends_with(&format!(".{c}"))) {
            return false;
        }
    }
    true
}

const BARE_TLDS: &[&str] = &[
    "com",
    "net",
    "org",
    "io",
    "app",
    "dev",
    "co",
    "me",
    "ai",
    "gg",
    "xyz",
    "info",
    "to",
    "tv",
    "is",
    "fi",
    "de",
    "uk",
    "us",
    "ca",
    "au",
    "fr",
    "it",
    "nl",
    "es",
    "jp",
    "cn",
    "in",
    "br",
    "ch",
    "at",
    "se",
    "no",
    "dk",
    "pl",
    "eu",
    "online",
    "site",
    "store",
    "cash",
    "social",
    "chat",
    "link",
    "click",
    "download",
    "live",
    "news",
    "space",
    "world",
    "pro",
    "cloud",
    "tech",
    "money",
    "vip",
    "win",
    "top",
    "club",
    "zone",
    "fund",
    "gold",
    "trade",
    "page",
    "land",
    "life",
    "network",
    "digital",
    "finance",
    "exchange",
    "foundation",
];

fn bare_domain(f: &str) -> bool {
    let host = f.split(['/', ':']).next().unwrap_or("");
    let mut parts = host.rsplit('.');
    let Some(tld) = parts.next() else {
        return false;
    };
    parts.next().is_some_and(|p| !p.is_empty()) && BARE_TLDS.contains(&tld)
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = host.split(':').next()?.trim_end_matches('.');
    let host = host.strip_prefix("www.").unwrap_or(host);
    (!host.is_empty()).then(|| host.to_string())
}

/// Bare http(s) URLs (`linkifyUrls`): to whitespace or a delimiter, less trailing prose
/// punctuation and unbalanced closing parentheses.
pub fn find_urls(t: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let lower = t.to_ascii_lowercase();
    let mut from = 0;
    while let Some(off) = lower[from..].find("http") {
        let start = from + off;
        let rest = &lower[start..];
        let scheme = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            from = start + 4;
            continue;
        };
        let mut end = start
            + t[start..]
                .find(|c: char| c.is_whitespace() || "<>\"{}|\\^`[]".contains(c))
                .unwrap_or(t.len() - start);
        let trim = |end: &mut usize| {
            while *end > start && t[..*end].ends_with(['.', ',', ';', ':', '!', '?']) {
                *end -= 1;
            }
        };
        trim(&mut end);
        let opens = t[start..end].matches('(').count();
        let mut closes = t[start..end].matches(')').count();
        let mut strips = 0;
        while strips < 8 && t[..end].ends_with(')') && closes > opens {
            end -= 1;
            trim(&mut end);
            closes -= 1;
            strips += 1;
        }
        if end > start + scheme {
            out.push((start, end));
        }
        from = end.max(start + scheme);
    }
    out
}

/// Parses a message's text into blocks.
/// Where `:shortcode:`s sit in `t` (`~` is the disambiguation separator); a clock's
/// `21:20:30` is not one.
pub fn shortcodes(t: &str) -> Vec<(usize, usize)> {
    let b = t.as_bytes();
    let ok = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'~' | b'-');
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b':' {
            i += 1;
            continue;
        }
        let len = b[i + 1..].iter().take_while(|c| ok(**c)).count();
        let end = i + 1 + len;
        if len == 0 || b.get(end) != Some(&b':') {
            i += 1;
            continue;
        }
        let digits = b[i + 1..end].iter().all(u8::is_ascii_digit);
        if digits && i > 0 && b[i - 1].is_ascii_digit() {
            i += 1;
            continue;
        }
        out.push((i, end + 1));
        i = end + 1;
    }
    out
}

pub fn parse(src: &str, ctx: &Ctx) -> Vec<Block> {
    let mut swaps = Vec::new();
    let chunks = prepare(src, &mut swaps);
    let mut b = Builder {
        ctx,
        swaps: &swaps,
        stack: vec![Frame::Root(Vec::new())],
        atoms: Vec::new(),
        para: None,
        flags: Flags::default(),
        code: None,
        quote_depth: 0,
        spoilers: 0,
    };
    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    for (sub, text) in &chunks {
        if *sub {
            b.open_para(ParaKind::Sub);
            for ev in Parser::new_ext(text, opts) {
                match ev {
                    Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => {}
                    ev => b.event(ev),
                }
            }
            b.flush_para();
            continue;
        }
        let mut events = Parser::new_ext(text, opts).peekable();
        while let Some(ev) = events.next() {
            // A markdown link that may not render as one shows its source.
            if let Event::Start(Tag::Link {
                dest_url,
                link_type,
                ..
            }) = &ev
            {
                let mut inner = Vec::new();
                for e in events.by_ref() {
                    if matches!(e, Event::End(TagEnd::Link)) {
                        break;
                    }
                    inner.push(e);
                }
                let text: String = inner
                    .iter()
                    .filter_map(|e| {
                        if let Event::Text(t) | Event::Code(t) = e {
                            Some(t.as_ref())
                        } else {
                            None
                        }
                    })
                    .collect();
                let autolink = matches!(
                    link_type,
                    pulldown_cmark::LinkType::Autolink | pulldown_cmark::LinkType::Email
                );
                if autolink || link_allowed(&text, dest_url) {
                    let url = dest_url.to_string();
                    b.flags.link.push(Some(url.into()));
                    for e in inner {
                        b.event(e);
                    }
                    b.flags.link.pop();
                } else if text == dest_url.as_ref() {
                    b.text(&text);
                } else {
                    b.text(&format!("[{text}]({dest_url})"));
                }
                continue;
            }
            b.event(ev);
        }
        b.flush_para();
    }
    b.flush_para();
    match b.stack.into_iter().next() {
        Some(Frame::Root(blocks)) => blocks,
        _ => Vec::new(),
    }
}

/// Plain text for a one-line preview: markdown and markers dropped (the reply quote's snippet).
pub fn flatten(src: &str) -> String {
    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    let mut out = String::new();
    for ev in Parser::new_ext(src, opts) {
        match ev {
            Event::Text(t) | Event::Code(t) => out.push_str(&t),
            Event::SoftBreak
            | Event::HardBreak
            | Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item) => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            _ => {}
        }
    }
    out.trim().to_string()
}

#[allow(dead_code)]
pub fn base_color(l: &Look) -> Hsla {
    l.color
}

#[cfg(test)]
mod custom_tests {
    use super::*;

    #[test]
    fn shortcodes_become_images() {
        let revealed = HashSet::new();
        let pic = std::sync::Arc::new(gpui::RenderImage::new(Vec::new()));
        let f = |c: &str| (c == "grin").then(|| crate::media::Custom::Ready(pic.clone()));
        let ctx = Ctx {
            base: Look::new("Rubik", 15., 21., gpui::white()),
            revealed: &revealed,
            custom: Some(&f),
        };
        let blocks = parse("Have you seen this? :grin: and 21:20:30", &ctx);
        let Block::Para(p) = &blocks[0] else { panic!() };
        assert!(p.atoms.iter().any(|a| matches!(
            a,
            Atom::Image {
                src: gpui::ImageSource::Render(_),
                ..
            }
        )));
        assert_eq!(shortcodes("21:20:30"), vec![]);
    }
}
