//! Message search: one field where free words and `key:value` filters mix (`from:`, `mentions:`,
//! `has:`, `in:`, `before:`, `after:`, `during:`), suggestions for the filter being typed, and
//! results that open the message in its chat. The scan is `vector_core::search`'s.

use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use chrono::{Datelike, Local, NaiveDate, TimeZone};
use gpui::prelude::*;
use crate::kit::Tipped as _;
use gpui::{AnyElement, App, Entity, FontWeight, HighlightStyle, SharedString, StyledText, Window, div, px, svg};
use vector_core::search::{Corpus, Filter, Found, Has, Query, Record};

use crate::composer::TextInput;
use crate::inline::{Atom, Kind, Look};
use crate::markdown::Block;
use crate::data::Store;
use crate::t;
use crate::theme::*;

/// Results listed at once; the count still covers every match.
pub const LIMIT: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    From,
    Mentions,
    Has,
    In,
    Before,
    After,
    During,
}

pub const KEYS: [Key; 7] = [Key::From, Key::Mentions, Key::Has, Key::In, Key::Before, Key::After, Key::During];

impl Key {
    /// The syntax word, the same in every language.
    pub fn word(self) -> &'static str {
        // i18n-exempt: search syntax
        match self {
            Key::From => "from",
            Key::Mentions => "mentions",
            Key::Has => "has",
            Key::In => "in",
            Key::Before => "before",
            Key::After => "after",
            Key::During => "during",
        }
    }

    fn parse(word: &str) -> Option<Key> {
        KEYS.into_iter().find(|k| k.word().eq_ignore_ascii_case(word))
    }

    fn label(self) -> SharedString {
        match self {
            Key::From => t::search_opt_from(),
            Key::Mentions => t::search_opt_mentions(),
            Key::Has => t::search_opt_has(),
            Key::In => t::search_opt_in(),
            Key::Before => t::search_opt_before(),
            Key::After => t::search_opt_after(),
            Key::During => t::search_opt_during(),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Key::From | Key::Mentions => "icons/user-circle.svg",
            Key::Has => "icons/attachment.svg",
            Key::In => "icons/channel-hash.svg",
            Key::Before | Key::After | Key::During => "icons/clock.svg",
        }
    }
}

/// `has:` values: the syntax word and what it asks for.
const HAS: [(&str, Has); 12] = [
    ("link", Has::LINK),       // i18n-exempt: search syntax
    ("image", Has::IMAGE),     // i18n-exempt: search syntax
    ("video", Has::VIDEO),     // i18n-exempt: search syntax
    ("audio", Has::AUDIO),     // i18n-exempt: search syntax
    ("file", Has::FILE),       // i18n-exempt: search syntax
    ("sound", Has::AUDIO),     // i18n-exempt: search syntax
    ("img", Has::IMAGE),       // i18n-exempt: search syntax
    ("photo", Has::IMAGE),     // i18n-exempt: search syntax
    ("picture", Has::IMAGE),   // i18n-exempt: search syntax
    ("vid", Has::VIDEO),       // i18n-exempt: search syntax
    ("url", Has::LINK),        // i18n-exempt: search syntax
    ("attachment", Has::FILE), // i18n-exempt: search syntax
];

fn has_icon(h: Has) -> &'static str {
    if h == Has::LINK {
        "icons/globe.svg"
    } else if h == Has::IMAGE {
        "icons/image.svg"
    } else if h == Has::VIDEO {
        "icons/video.svg"
    } else if h == Has::AUDIO {
        "icons/volume-max.svg"
    } else {
        "icons/file.svg"
    }
}

fn has_label(h: Has) -> SharedString {
    if h == Has::LINK {
        t::search_has_link()
    } else if h == Has::IMAGE {
        t::search_has_image()
    } else if h == Has::VIDEO {
        t::search_has_video()
    } else if h == Has::AUDIO {
        t::search_has_audio()
    } else {
        t::search_has_file()
    }
}

/// A filter as typed: where it sits in the field, and what it reads as.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub key: Key,
    pub value: String,
    pub range: Range<usize>,
}

/// The field split into filters and the words left over.
#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    pub tokens: Vec<Token>,
    pub words: String,
}

/// Splits `text` on whitespace (a `"quoted value"` stays whole) into filters and free words.
pub fn parse(text: &str) -> Parsed {
    let mut out = Parsed::default();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let mut quoted = false;
        while i < b.len() && (quoted || !b[i].is_ascii_whitespace()) {
            if b[i] == b'"' {
                quoted = !quoted;
            }
            i += 1;
        }
        let word = &text[start..i];
        let filter = word.split_once(':').and_then(|(k, v)| Some((Key::parse(k)?, v)));
        match filter {
            Some((key, v)) => out.tokens.push(Token { key, value: v.trim_matches('"').to_string(), range: start..i }),
            None => {
                if !out.words.is_empty() {
                    out.words.push(' ');
                }
                out.words.push_str(word);
            }
        }
    }
    out
}

/// A person or chat a filter value may name.
#[derive(Clone, Debug)]
pub struct Named {
    pub id: String,
    pub name: String,
    folded: String,
}

/// The people and chats filters resolve against, read from the store as search opens.
#[derive(Clone, Default)]
pub struct Directory {
    pub me: String,
    pub people: Vec<Named>,
    pub chats: Vec<Named>,
}

impl Directory {
    pub fn read(store: &Store) -> Self {
        let named = |id: String, name: String| Named { folded: vector_core::search::fold(&name), id, name };
        let mut people: Vec<Named> = store
            .profiles
            .keys()
            .filter(|n| **n != store.my_npub)
            .map(|n| named(n.clone(), store.name(n)))
            .collect();
        people.sort_by(|a, b| a.folded.cmp(&b.folded));
        let mut chats = Vec::new();
        for chat in &store.chats {
            if chat.chat_type == vector_core::ChatType::DirectMessage && (store.listed(chat) || chat.id == store.my_npub) {
                chats.push(named(chat.id.clone(), store.name(&chat.id)));
            }
        }
        for com in &store.communities {
            for ch in com.channels.iter().filter(|c| c.view != "voice") {
                // i18n-exempt: channel sigil
                chats.push(named(ch.channel_id.clone(), format!("#{} · {}", ch.name, com.name)));
            }
        }
        Self { me: store.my_npub.clone(), people, chats }
    }

    /// A Community's members and channels, named as the Community names them.
    pub fn community(store: &Store, com: &crate::data::Community, members: impl Iterator<Item = String>) -> Self {
        let named = |id: String, name: String| Named { folded: vector_core::search::fold(&name), id, name };
        let mut people: Vec<Named> = members
            .filter(|n| *n != store.my_npub)
            .map(|n| {
                let name = store.name_in(&n, Some(&com.community_id));
                named(n, name)
            })
            .collect();
        people.sort_by(|a, b| a.folded.cmp(&b.folded));
        people.dedup_by(|a, b| a.id == b.id);
        let chats = com
            .channels
            .iter()
            .filter(|c| c.view != "voice")
            // i18n-exempt: channel sigil
            .map(|ch| named(ch.channel_id.clone(), format!("#{}", ch.name)))
            .collect();
        Self { me: store.my_npub.clone(), people, chats }
    }

    /// The best matches for `value`: exact names first, else names starting with it, else
    /// names holding it.
    fn matches<'a>(list: &'a [Named], value: &str) -> Vec<&'a Named> {
        let v = vector_core::search::fold(value.trim_start_matches(['@', '#']));
        if v.is_empty() {
            return Vec::new();
        }
        let exact: Vec<&Named> = list.iter().filter(|n| n.folded == v || n.id == value).collect();
        if !exact.is_empty() {
            return exact;
        }
        let bare = |n: &&Named| n.folded.trim_start_matches('#').to_string();
        let start: Vec<&Named> = list.iter().filter(|n| bare(n).starts_with(&v)).collect();
        if !start.is_empty() {
            return start;
        }
        list.iter().filter(|n| n.folded.contains(&v)).collect()
    }

    fn is_me(value: &str) -> bool {
        let v = vector_core::search::fold(value);
        // i18n-exempt: search syntax
        v == "me" || v == vector_core::search::fold(&t::search_me())
    }
}

/// Day bounds in local ms for `2026-11-18`, `2026-11`, `2026`, `today` or `yesterday`.
pub fn day_span(value: &str, now_ms: u64) -> Option<(u64, u64)> {
    let v = value.trim().to_ascii_lowercase();
    let today = Local.timestamp_millis_opt(now_ms as i64).single()?.date_naive();
    let (from, to) = match v.as_str() {
        // i18n-exempt: search syntax
        "today" => (today, today),
        // i18n-exempt: search syntax
        "yesterday" => (today.pred_opt()?, today.pred_opt()?),
        _ => {
            let parts: Vec<&str> = v.split(['-', '/', '.']).collect();
            let num = |i: usize| parts.get(i).and_then(|p| p.parse::<u32>().ok());
            let year = num(0).filter(|y| (1970..=9999).contains(y))? as i32;
            match parts.len() {
                1 => (NaiveDate::from_ymd_opt(year, 1, 1)?, NaiveDate::from_ymd_opt(year, 12, 31)?),
                2 => {
                    let m = num(1)?;
                    let first = NaiveDate::from_ymd_opt(year, m, 1)?;
                    let next = if m == 12 { NaiveDate::from_ymd_opt(year + 1, 1, 1)? } else { NaiveDate::from_ymd_opt(year, m + 1, 1)? };
                    (first, next.pred_opt()?)
                }
                3 => {
                    let d = NaiveDate::from_ymd_opt(year, num(1)?, num(2)?)?;
                    (d, d)
                }
                _ => return None,
            }
        }
    };
    let start = Local.from_local_datetime(&from.and_hms_opt(0, 0, 0)?).earliest()?.timestamp_millis();
    let end = Local.from_local_datetime(&to.and_hms_milli_opt(23, 59, 59, 999)?).latest()?.timestamp_millis();
    Some((start.max(0) as u64, end.max(0) as u64))
}

/// Matches nothing: a filter whose value names no one.
const NOBODY: &str = "\u{0}";

/// The filter the tokens ask for. An empty value (still being typed) asks for nothing.
pub fn filter_of(tokens: &[Token], dir: &Directory, now_ms: u64, oldest: bool) -> Filter {
    let mut f = Filter { oldest, ..Default::default() };
    for tok in tokens.iter().filter(|t| !t.value.is_empty()) {
        match tok.key {
            Key::From | Key::Mentions => {
                let mut ids: Vec<String> = Directory::matches(&dir.people, &tok.value).into_iter().map(|n| n.id.clone()).collect();
                let me = Directory::is_me(&tok.value);
                if tok.key == Key::From && me {
                    f.from_me = true;
                } else if me {
                    ids.push(dir.me.clone());
                }
                if ids.is_empty() && !me {
                    ids.push(NOBODY.into());
                }
                if tok.key == Key::From {
                    f.from.extend(ids);
                } else {
                    f.mentions.extend(ids);
                }
            }
            Key::Has => {
                let v = tok.value.to_ascii_lowercase();
                match HAS.iter().find(|(w, _)| *w == v || (v.len() >= 2 && w.starts_with(&v))) {
                    Some((_, h)) => f.has |= *h,
                    None => f.chats.push(NOBODY.into()),
                }
            }
            Key::In => {
                let found = Directory::matches(&dir.chats, &tok.value);
                if found.is_empty() {
                    f.chats.push(NOBODY.into());
                }
                f.chats.extend(found.into_iter().map(|n| n.id.clone()));
            }
            Key::Before | Key::After | Key::During => match day_span(&tok.value, now_ms) {
                Some((start, end)) => match tok.key {
                    Key::Before => f.before = Some(f.before.map_or(start - 1, |b| b.min(start - 1))),
                    Key::After => f.after = Some(f.after.map_or(end + 1, |a| a.max(end + 1))),
                    _ => {
                        f.after = Some(f.after.map_or(start, |a| a.max(start)));
                        f.before = Some(f.before.map_or(end, |b| b.min(end)));
                    }
                },
                None => f.chats.push(NOBODY.into()),
            },
        }
    }
    f
}

/// What the suggestion list offers for the word at the caret.
#[derive(Clone, Debug, PartialEq)]
pub enum Suggest {
    Key(Key),
    Value { key: Key, value: String, label: SharedString, npub: Option<String> },
}

/// The word under the caret, when the caret ends it.
fn word_at(text: &str, caret: usize) -> (Range<usize>, &str) {
    let caret = caret.min(text.len());
    let head = &text[..caret];
    let mut quotes = 0;
    let mut start = 0;
    for (i, ch) in head.char_indices() {
        if ch == '"' {
            quotes += 1;
        } else if ch.is_whitespace() && quotes % 2 == 0 {
            start = i + ch.len_utf8();
        }
    }
    (start..caret, &text[start..caret])
}

pub fn suggestions(text: &str, caret: usize, dir: &Directory, now_ms: u64) -> (Range<usize>, Vec<Suggest>) {
    let (range, word) = word_at(text, caret);
    if text.trim().is_empty() {
        return (range, KEYS.into_iter().map(Suggest::Key).collect());
    }
    let Some((k, partial)) = word.split_once(':') else {
        // A filter word half typed, such as `fr`.
        let keys: Vec<Suggest> = if word.len() >= 2 {
            KEYS.into_iter().filter(|k| k.word().starts_with(&word.to_ascii_lowercase())).map(Suggest::Key).collect()
        } else {
            Vec::new()
        };
        return (range, keys);
    };
    let Some(key) = Key::parse(k) else { return (range, Vec::new()) };
    let partial = partial.trim_start_matches('"');
    let value = |value: String, label: SharedString, npub: Option<String>| Suggest::Value { key, value, label, npub };
    let list = match key {
        Key::From | Key::Mentions => {
            let mut out = Vec::new();
            let me = t::search_me();
            if partial.is_empty() || vector_core::search::fold(&me).starts_with(&vector_core::search::fold(partial)) {
                // i18n-exempt: search syntax
                out.push(value("me".into(), me, Some(dir.me.clone())));
            }
            let people: Vec<&Named> = if partial.is_empty() { dir.people.iter().collect() } else { Directory::matches(&dir.people, partial) };
            out.extend(people.into_iter().take(8).map(|n| value(n.name.clone(), n.name.clone().into(), Some(n.id.clone()))));
            out
        }
        Key::In => {
            let chats: Vec<&Named> = if partial.is_empty() { dir.chats.iter().collect() } else { Directory::matches(&dir.chats, partial) };
            chats
                .into_iter()
                .take(8)
                .map(|n| {
                    let npub = n.id.starts_with("npub1").then(|| n.id.clone());
                    let short = n.name.split(" · ").next().unwrap_or(&n.name).to_string();
                    value(short, n.name.clone().into(), npub)
                })
                .collect()
        }
        Key::Has => HAS[..5]
            .iter()
            .filter(|(w, _)| w.starts_with(&partial.to_ascii_lowercase()))
            .map(|(w, h)| value(w.to_string(), has_label(*h), None))
            .collect(),
        Key::Before | Key::After | Key::During => {
            let mut out = vec![
                // i18n-exempt: search syntax
                value("today".into(), t::search_today(), None),
                // i18n-exempt: search syntax
                value("yesterday".into(), t::search_yesterday(), None),
            ];
            out.retain(|s| matches!(s, Suggest::Value { value, .. } if value.starts_with(&partial.to_ascii_lowercase())));
            if let Some(d) = Local.timestamp_millis_opt(now_ms as i64).single() {
                let iso = format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day());
                if iso.starts_with(partial) && !partial.is_empty() {
                    out.push(value(iso.clone(), iso.into(), None));
                }
            }
            if day_span(partial, now_ms).is_some() && !out.iter().any(|s| matches!(s, Suggest::Value { value, .. } if value == partial)) {
                out.insert(0, value(partial.to_string(), partial.to_string().into(), None));
            }
            out
        }
    };
    (range, list)
}

/// What picking a suggestion writes in place of the word at the caret.
pub fn completion(s: &Suggest) -> String {
    match s {
        Suggest::Key(k) => format!("{}:", k.word()),
        Suggest::Value { key, value, .. } if value.contains(' ') => format!("{}:\"{}\" ", key.word(), value),
        Suggest::Value { key, value, .. } => format!("{}:{} ", key.word(), value),
    }
}

/// The corpus for the fixtures: the messages the store holds.
pub fn fixture_corpus(store: &Store) -> Corpus {
    let mut records: Vec<Record> = Vec::new();
    for chat in &store.chats {
        for m in &chat.messages {
            records.push(Record {
                chat: chat.id.clone(),
                id: m.id.clone(),
                author: if m.mine { Some(store.my_npub.clone()) } else { m.npub.clone().or_else(|| Some(chat.id.clone())) },
                mine: m.mine,
                at: m.at,
                text: m.content.clone(),
                files: m.attachments.iter().map(|a| (a.extension.clone(), a.name.clone())).collect(),
                emoji: m.emoji_tags.clone(),
                spans: m.text_spans.clone(),
            });
        }
    }
    records.sort_by_key(|r| std::cmp::Reverse(r.at));
    build(&records, &names(store), &blocked(store))
}

/// Display names by npub, for naming mentions in what is searched.
pub fn names(store: &Store) -> std::collections::HashMap<String, String> {
    store.profiles.keys().map(|n| (n.clone(), store.name(n))).collect()
}

pub fn blocked(store: &Store) -> std::collections::HashSet<String> {
    store.profiles.iter().filter(|(_, p)| p.is_blocked).map(|(n, _)| n.clone()).collect()
}

/// A corpus from records: mentions named, blocked people left out. DMs carry no author on
/// their side's messages, so a DM's own id stands in.
pub fn build(records: &[Record], names: &std::collections::HashMap<String, String>, blocked: &std::collections::HashSet<String>) -> Corpus {
    let mut corpus = Corpus::new();
    for r in records {
        let mut r = std::borrow::Cow::Borrowed(r);
        if r.author.is_none() && r.chat.starts_with("npub1") && !r.mine {
            r.to_mut().author = Some(r.chat.clone());
        }
        if r.author.as_ref().is_some_and(|a| blocked.contains(a)) {
            continue;
        }
        let shown = vector_core::search::name_mentions(&r.text, |n| names.get(n).cloned());
        corpus.push(&r, &shown);
    }
    corpus
}

pub struct Search {
    pub input: Entity<TextInput>,
    pub dir: Directory,
    /// `None` while the messages are read.
    pub corpus: Option<Arc<Corpus>>,
    pub found: Found,
    pub parsed: Parsed,
    pub suggest: Vec<Suggest>,
    /// Where the word being completed sits in the field.
    pub suggest_at: Range<usize>,
    /// The highlighted row: of the suggestions while they show, else of the results.
    pub sel: usize,
    pub oldest: bool,
    pub scroll: gpui::ScrollHandle,
    /// How long the last scan took, in µs.
    pub took: u128,
    /// The chats searched when no `in:` names one; empty is every chat.
    pub scope: Vec<String>,
    pub loading: bool,
    /// Drawn snippets by corpus doc, for the query on screen.
    drawn: std::cell::RefCell<std::collections::HashMap<u32, Rc<[Atom]>>>,
    /// Attachments of results the open chats don't hold, by message: None while read.
    pub atts: std::collections::HashMap<String, Option<Vec<vector_core::types::Attachment>>>,
}

/// A result's picture or video still, as its row shows it.
#[derive(Clone)]
pub struct Thumb {
    pub image: Arc<gpui::RenderImage>,
    pub w: u32,
    pub h: u32,
    /// What a click opens; a video, or a picture not yet downloaded, opens its message instead.
    pub open: Option<crate::viewer::Open>,
    pub video: bool,
}

/// A thumbnail's height; widths follow each picture, within these.
pub const THUMB_H: f32 = 72.;
pub const THUMB_W: (f32, f32) = (48., 160.);
/// Thumbnails a row shows before counting the rest.
pub const THUMBS: usize = 3;

impl Search {
    pub fn new(input: Entity<TextInput>, dir: Directory) -> Self {
        Self {
            input,
            dir,
            corpus: None,
            found: Found::default(),
            parsed: Parsed::default(),
            suggest: Vec::new(),
            suggest_at: 0..0,
            sel: 0,
            oldest: false,
            scroll: gpui::ScrollHandle::new(),
            took: 0,
            scope: Vec::new(),
            loading: false,
            drawn: Default::default(),
            atts: Default::default(),
        }
    }

    /// Re-reads the field: filters, suggestions and results.
    pub fn refresh(&mut self, text: &str, caret: usize, now_ms: u64) {
        self.parsed = parse(text);
        let (at, list) = suggestions(text, caret, &self.dir, now_ms);
        self.suggest_at = at;
        self.suggest = list;
        let mut filter = filter_of(&self.parsed.tokens, &self.dir, now_ms, self.oldest);
        if filter.chats.is_empty() {
            filter.chats = self.scope.clone();
        }
        let query = Query::parse(&self.parsed.words);
        let t0 = std::time::Instant::now();
        self.found = match &self.corpus {
            Some(c) => c.search(&query, &filter, LIMIT),
            None => Found::default(),
        };
        self.took = t0.elapsed().as_micros();
        self.drawn.borrow_mut().clear();
        self.sel = 0;
        self.scroll.scroll_to_item(0);
    }

    /// `hit`'s snippet drawn as its message is, or `None` for the plain one.
    fn drawn(&self, hit: &vector_core::search::Hit, store: &Store, look: &Look) -> Option<Rc<[Atom]>> {
        if let Some(atoms) = self.drawn.borrow().get(&hit.doc) {
            return Some(atoms.clone());
        }
        let source = self.corpus.as_ref()?.source(hit.doc)?;
        let pending = std::cell::Cell::new(false);
        let atoms: Rc<[Atom]> = drawn(source, Query::parse(&self.parsed.words).terms(), look, store, &hit.chat, &pending)?.into();
        // A custom emoji still loading draws again once it lands.
        if !pending.get() {
            self.drawn.borrow_mut().insert(hit.doc, atoms.clone());
        }
        Some(atoms)
    }

    pub fn showing_suggestions(&self) -> bool {
        !self.suggest.is_empty()
    }

    fn rows(&self) -> usize {
        if self.showing_suggestions() { self.suggest.len() } else { self.found.hits.len() }
    }

    pub fn step(&mut self, down: bool) {
        let n = self.rows();
        if n == 0 {
            return;
        }
        self.sel = if down { (self.sel + 1) % n } else { (self.sel + n - 1) % n };
        let ix = if self.showing_suggestions() { self.sel } else { self.result_row(self.sel) };
        self.scroll.scroll_to_item(ix);
    }

    /// The list child index of result `i`, counting the chat headers above it.
    fn result_row(&self, i: usize) -> usize {
        let hits = &self.found.hits;
        let headers = (0..=i.min(hits.len().saturating_sub(1))).filter(|&j| j == 0 || hits[j].chat != hits[j - 1].chat).count();
        i + headers
    }
}

pub enum Act {
    Close,
    Pick(usize),
    /// A result's picture, full screen.
    View(crate::viewer::Open),
    Hover(usize),
    Suggest(usize),
    Sort,
    Unfilter(Range<usize>),
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

fn click(on: &OnAct, act: impl Fn() -> Act + 'static) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| {
        cx.stop_propagation();
        on(act(), w, cx)
    }
}

/// The header's magnifier: search the open chat.
pub fn button(on: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static) -> impl IntoElement {
    // i18n-exempt: key names
    let keys = if cfg!(target_os = "macos") { "\u{2318}F" } else { "Ctrl+F" };
    div()
        .id("search-btn")
        .tip(t::search_button_tip(keys.to_string()))
        .size(px(40.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .on_click(on)
        .child(svg().path("icons/search.svg").size(px(19.)).text_color(c(ICON_MUTED)))
}

/// Where a chat is: its name, and the picture that stands for it.
fn place(store: &Store, chat: &str) -> (String, Option<std::path::PathBuf>, bool) {
    if let Some(com) = store.community_of(chat) {
        let ch = com.channel(chat).map(|c| c.name.clone()).unwrap_or_default();
        // i18n-exempt: channel sigil
        return (format!("#{ch} · {}", com.name), store.icon(&com.community_id), false);
    }
    (store.name(chat), store.avatar_path(chat), true)
}

fn when(ms: u64, now_ms: u64) -> String {
    use crate::dates::{Clock, DateStyle};
    let Some(d) = crate::dates::at_ms(ms as i64) else { return String::new() };
    let now = crate::dates::at_ms(now_ms as i64);
    if now.is_some_and(|n| n.date_naive() == d.date_naive()) {
        return crate::dates::time(&d, Clock::default());
    }
    crate::dates::date_time(&d, DateStyle::Medium, Clock::default())
}

/// Characters of a snippet before its first match; twice as many follow it.
const CONTEXT: usize = 60;

/// A message drawn as the timeline draws it, with headers in bold and its blocks run into one
/// paragraph, cut to about [`CONTEXT`] characters around its first match and every match marked.
/// `None` when it shows nothing, as a file's message.
fn drawn(source: vector_core::search::Source, terms: &[String], look: &Look, store: &Store, chat: &str, pending: &std::cell::Cell<bool>) -> Option<Vec<Atom>> {
    let (text, colors) = crate::timeline::effects(source.text, source.spans);
    let text = demote_headers(&text);
    let media = crate::media::shared();
    let custom = crate::timeline::custom_emoji(store, &media, source.emoji);
    let custom = custom.as_ref().map(|f| {
        move |code: &str| {
            let got = f(code);
            pending.set(pending.get() || matches!(got, Some(crate::media::Custom::Pending)));
            got
        }
    });
    let revealed = std::collections::HashSet::new();
    let blocks = crate::markdown::parse(
        text.trim(),
        &crate::markdown::Ctx {
            base: look.clone(),
            revealed: &revealed,
            custom: custom.as_ref().map(|f| f as &dyn Fn(&str) -> Option<crate::media::Custom>),
            now: store.now_ms() as f64 / 1000.,
            mention: Some(&|npub: &str| store.chat_name(npub, chat)),
            tags: &[],
            mask: Some(&|t: &str| store.stream_mask(t)),
            colors: &colors,
            go: None,
        },
    );
    let mut atoms = Vec::new();
    flatten(&blocks, look, &mut atoms);
    (!atoms.is_empty()).then(|| window(atoms, terms, look, !colors.is_empty()))
}

/// Headers as bold lines and subtext as plain ones, as a chat's preview shows them. Colour marks
/// before a line's `#` stay where they are.
fn demote_headers(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let rest = line.trim_start_matches(|c: char| crate::textcolor::is_sentinel(c) || c == ' ' || c == '\t');
        let lead = &line[..line.len() - rest.len()];
        let hashes = rest.chars().take_while(|&c| c == '#').count();
        if let Some(sub) = rest.strip_prefix("-# ") {
            out.push_str(lead);
            out.push_str(sub);
        } else if (1..=6).contains(&hashes) && rest[hashes..].starts_with([' ', '\t']) && !rest[hashes..].trim().is_empty() {
            out.push_str(lead);
            out.push_str("**");
            out.push_str(rest[hashes..].trim());
            out.push_str("**");
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Blocks as one run of inline atoms: paragraphs, list items, quotes and table cells a space
/// apart, a code block as a code chip.
fn flatten(blocks: &[Block], look: &Look, out: &mut Vec<Atom>) {
    let gap = |out: &mut Vec<Atom>| {
        if !out.is_empty() {
            out.push(Atom::Text { text: " ".into(), look: look.clone(), kind: Kind::Plain });
        }
    };
    let para = |p: &crate::markdown::Para, out: &mut Vec<Atom>| {
        gap(out);
        out.extend(p.atoms.iter().map(|a| match a {
            Atom::Break => Atom::Text { text: " ".into(), look: look.clone(), kind: Kind::Plain },
            a => a.clone(),
        }));
    };
    for b in blocks {
        match b {
            Block::Para(p) => para(p, out),
            Block::Code { text, .. } => {
                let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if !one.is_empty() {
                    gap(out);
                    out.push(Atom::Text { text: one.into(), look: look.clone(), kind: Kind::Code });
                }
            }
            Block::Quote(inner) => flatten(inner, look, out),
            Block::List { items, .. } => items.iter().for_each(|item| flatten(item, look, out)),
            Block::Table { head, rows, .. } => head.iter().chain(rows.iter().flatten()).for_each(|p| para(p, out)),
            Block::Rule => {}
        }
    }
}

/// `atoms` cut to the window around the first match of `terms` in what they show, matches
/// marked: in the accent where the text is uncoloured, and bold.
fn window(atoms: Vec<Atom>, terms: &[String], look: &Look, coloured: bool) -> Vec<Atom> {
    // What each atom shows, end to end; a picture is one character that matches nothing.
    let mut shown = String::new();
    let mut at: Vec<Range<usize>> = Vec::with_capacity(atoms.len());
    for a in &atoms {
        let start = shown.len();
        match a {
            Atom::Text { text, .. } => shown.push_str(text),
            Atom::Image { .. } | Atom::Pill(_) => shown.push('\u{FFFC}'),
            _ => {}
        }
        at.push(start..shown.len());
    }
    let marks = vector_core::search::marks(&shown, terms);
    let (start, stop) = match marks.first() {
        Some(m) => (word_before(&shown, m.start, CONTEXT), chars_after(&shown, m.end, CONTEXT * 2)),
        None => (0, chars_after(&shown, 0, CONTEXT * 3)),
    };
    let ellipsis = |out: &mut Vec<Atom>| out.push(Atom::Text { text: "…".into(), look: look.clone(), kind: Kind::Plain });
    let mut out = Vec::with_capacity(atoms.len() + 2 * marks.len() + 2);
    if start > 0 {
        ellipsis(&mut out);
    }
    for (a, r) in atoms.into_iter().zip(at) {
        let (lo, hi) = (r.start.max(start), r.end.min(stop));
        if lo >= hi && !(r.is_empty() && r.start > start && r.start < stop) {
            continue;
        }
        let Atom::Text { text, look: l, kind } = a else {
            out.push(a);
            continue;
        };
        let mut cut = lo;
        for m in marks.iter().filter(|m| m.end > lo && m.start < hi) {
            let (ms, me) = (m.start.max(lo), m.end.min(hi));
            if ms > cut {
                out.push(Atom::Text { text: text[cut - r.start..ms - r.start].to_string().into(), look: l.clone(), kind: kind.clone() });
            }
            let mut hl = l.clone();
            hl.weight = FontWeight::SEMIBOLD;
            let effect = coloured && kind == Kind::Plain && hl.color != look.color;
            if !effect {
                hl.color = c(ACCENT);
            }
            out.push(Atom::Text { text: text[ms - r.start..me - r.start].to_string().into(), look: hl, kind: kind.clone() });
            cut = me;
        }
        if cut < hi {
            out.push(Atom::Text { text: text[cut - r.start..hi - r.start].to_string().into(), look: l, kind });
        }
    }
    if stop < shown.len() {
        ellipsis(&mut out);
    }
    out
}

/// About `n` characters before byte `at` of `text`, starting on a word when one starts nearby.
fn word_before(text: &str, at: usize, n: usize) -> usize {
    let head = &text[..at];
    let cut = head.char_indices().rev().nth(n.saturating_sub(1)).map_or(0, |(i, _)| i);
    if cut == 0 {
        return 0;
    }
    head[cut..].find(char::is_whitespace).filter(|&w| w < 16).map_or(cut, |w| cut + w + 1).min(at)
}

fn chars_after(text: &str, at: usize, n: usize) -> usize {
    text[at..].char_indices().nth(n).map_or(text.len(), |(i, _)| at + i)
}

fn snippet(text: &str, marks: &[Range<usize>]) -> StyledText {
    let mark = HighlightStyle {
        color: Some(c(ACCENT)),
        font_weight: Some(FontWeight::SEMIBOLD),
        ..Default::default()
    };
    StyledText::new(text.to_string()).with_highlights(marks.iter().map(|r| (r.clone(), mark)))
}

fn chip(tok: &Token, store: &Store, dir: &Directory, on: &OnAct) -> AnyElement {
    let shown: String = match tok.key {
        Key::From | Key::Mentions if Directory::is_me(&tok.value) => t::search_you().to_string(),
        Key::From | Key::Mentions => {
            let m = Directory::matches(&dir.people, &tok.value);
            match m.as_slice() {
                [one] => store.name(&one.id),
                _ => tok.value.clone(),
            }
        }
        Key::Has => HAS.iter().find(|(w, _)| *w == tok.value.to_ascii_lowercase()).map_or(tok.value.clone(), |(_, h)| has_label(*h).to_string()),
        _ => tok.value.clone(),
    };
    let range = tok.range.clone();
    div()
        .id(SharedString::from(format!("search-chip-{}", tok.range.start)))
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(26.))
        .pl(px(10.))
        .pr(px(4.))
        .rounded(px(13.))
        .bg(white_a(0.06))
        .border_1()
        .border_color(white_a(0.12))
        .text_size(px(13.))
        .child(div().text_color(c(ICON_MUTED)).font_weight(FontWeight::SEMIBOLD).child(format!("{}:", tok.key.word())))
        .child(div().text_color(c(TEXT)).max_w(px(180.)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(shown))
        .child(
            div()
                .id(SharedString::from(format!("search-chip-x-{}", tok.range.start)))
                .tip(t::search_remove_filter())
                .size(px(18.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(white_a(0.12)))
                .on_click(click(on, move || Act::Unfilter(range.clone())))
                .child(svg().path("icons/x.svg").size(px(10.)).text_color(white_a(0.7))),
        )
        .into_any_element()
}

fn suggestion_row(i: usize, s: &Suggest, sel: bool, store: &Store, compact: bool, on: &OnAct) -> AnyElement {
    let (lead_px, glyph) = if compact { (22., 12.) } else { (28., 15.) };
    let (lead, title, hint): (AnyElement, SharedString, Option<SharedString>) = match s {
        Suggest::Key(k) => (
            div()
                .size(px(lead_px))
                .rounded(px(lead_px / 4.))
                .bg(white_a(0.06))
                .flex()
                .items_center()
                .justify_center()
                .child(svg().path(k.icon()).size(px(glyph)).text_color(c(ICON_MUTED)))
                .into_any_element(),
            format!("{}:", k.word()).into(),
            Some(k.label()),
        ),
        Suggest::Value { key, label, npub, .. } => {
            let lead = match (npub, key) {
                (Some(n), _) => crate::ui::avatar(store, n, lead_px),
                (None, k) => {
                    let path = match (k, s) {
                        (Key::Has, Suggest::Value { value, .. }) => HAS.iter().find(|(w, _)| w == value).map_or(k.icon(), |(_, h)| has_icon(*h)),
                        _ => k.icon(),
                    };
                    div()
                        .size(px(lead_px))
                        .rounded(px(lead_px / 4.))
                        .bg(white_a(0.06))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(svg().path(path).size(px(glyph)).text_color(c(ICON_MUTED)))
                        .into_any_element()
                }
            };
            (lead, label.clone(), None)
        }
    };
    div()
        .id(SharedString::from(format!("search-suggest-{i}")))
        .flex()
        .items_center()
        .gap(px(if compact { 8. } else { 12. }))
        .h(px(if compact { 34. } else { 44. }))
        .px(px(if compact { 8. } else { 12. }))
        .when(!compact, |d| d.mx(px(8.)))
        .rounded(px(if compact { 6. } else { 8. }))
        .cursor_pointer()
        .when(sel, |d| d.bg(white_a(0.07)))
        .hover(|d| d.bg(white_a(0.05)))
        .on_mouse_move({
            let on = on.clone();
            move |_, w, cx| on(Act::Hover(i), w, cx)
        })
        .on_click(click(on, move || Act::Suggest(i)))
        .child(lead)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_baseline()
                .gap(px(10.))
                .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(if compact { 13. } else { 15. })).whitespace_nowrap().child(title))
                .children(hint.filter(|_| !compact).map(|h| div().text_size(px(13.)).text_color(c(LIST_TIME)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(h))),
        )
        .into_any_element()
}

fn header(store: &Store, chat: &str) -> AnyElement {
    let (name, pic, dm) = place(store, chat);
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(20.))
        .pt(px(14.))
        .pb(px(6.))
        .child(match pic {
            Some(p) => crate::pic::img_face(p, 18.).object_fit(gpui::ObjectFit::Cover).size(px(18.)).when(dm, |i| i.rounded_full()).when(!dm, |i| i.rounded(px(5.))).into_any_element(),
            None => svg().path(if dm { "icons/user-circle.svg" } else { "icons/chats.svg" }).size(px(14.)).text_color(c(LIST_TIME)).into_any_element(),
        })
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::BOLD)
                .text_color(c(LIST_TIME))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(name.to_uppercase()),
        )
        .into_any_element()
}

fn row_look() -> Look {
    Look::new(RUBIK, 14., 20., white_a(0.82))
}

fn mini_look() -> Look {
    Look::new(RUBIK, 12.5, 17., white_a(0.82))
}

/// A result's pictures: the first few shown, and how many more the message holds.
pub struct Strip {
    pub thumbs: Vec<Thumb>,
    pub more: usize,
}

fn thumb_strip(i: usize, strip: &Strip, on: &OnAct) -> AnyElement {
    let (thumbs, more) = (&strip.thumbs, strip.more);
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .mt(px(6.))
        .children(thumbs.iter().take(THUMBS).enumerate().map(|(k, t)| {
            let w = (THUMB_H * t.w as f32 / t.h.max(1) as f32).clamp(THUMB_W.0, THUMB_W.1);
            let act = t.open.clone();
            div()
                .id(SharedString::from(format!("search-thumb-{i}-{k}")))
                .relative()
                .w(px(w))
                .h(px(THUMB_H))
                .rounded(px(6.))
                .overflow_hidden()
                .bg(white_a(0.04))
                .cursor_pointer()
                .on_click(click(on, move || match &act {
                    Some(open) => Act::View(open.clone()),
                    None => Act::Pick(i),
                }))
                .child(crate::pic::img(t.image.clone()).size_full().object_fit(gpui::ObjectFit::Cover))
                .when(t.video, |d| {
                    d.child(
                        div().absolute().inset_0().flex().items_center().justify_center().child(
                            div()
                                .size(px(26.))
                                .rounded_full()
                                .bg(gpui::black().opacity(0.5))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(svg().path("icons/play.svg").size(px(12.)).text_color(gpui::white())),
                        ),
                    )
                })
        }))
        .when(more > 0, |d| {
            d.child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(c(LIST_TIME)).child(format!("+{}", crate::lang::num(more)))) // i18n-exempt: a count
        })
        .into_any_element()
}

/// An icon has no baseline, so it sits on the line by its bottom edge: dropped by this much,
/// its middle meets the 12 px digits'.
const ICON_DROP: f32 = 2.;

fn kind_icon(path: &'static str) -> impl IntoElement {
    svg().path(path).flex_none().size(px(13.)).relative().top(px(ICON_DROP)).text_color(c(LIST_TIME))
}

fn result_row(i: usize, hit: &vector_core::search::Hit, drawn: Option<Rc<[Atom]>>, thumbs: Option<&Strip>, sel: bool, store: &Store, now_ms: u64, on: &OnAct) -> AnyElement {
    let who = if hit.mine { store.my_npub.clone() } else { hit.author.clone().unwrap_or_else(|| hit.chat.clone()) };
    let name = if hit.mine { t::search_you().to_string() } else { store.chat_name(&who, &hit.chat) };
    let badges: Vec<&str> = [(Has::IMAGE, "icons/image.svg"), (Has::VIDEO, "icons/video.svg"), (Has::AUDIO, "icons/volume-max.svg")]
        .into_iter()
        .filter(|(h, _)| hit.has.contains(*h))
        .map(|(_, p)| p)
        .collect();
    let file_only = hit.has.contains(Has::FILE) && badges.is_empty();
    div()
        .id(SharedString::from(format!("search-hit-{i}")))
        .flex()
        .gap(px(12.))
        .px(px(12.))
        .py(px(10.))
        .mx(px(8.))
        .rounded(px(8.))
        .cursor_pointer()
        .border_1()
        .border_color(if sel { white_a(0.14) } else { gpui::transparent_black() })
        .bg(if sel { white_a(0.06) } else { white_a(0.025) })
        .hover(|d| d.bg(white_a(0.05)))
        .on_mouse_move({
            let on = on.clone();
            move |_, w, cx| on(Act::Hover(i), w, cx)
        })
        .on_click(click(on, move || Act::Pick(i)))
        .child(crate::ui::avatar(store, &who, 36.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(8.))
                        .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(15.)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(name))
                        .child(div().text_size(px(12.)).text_color(c(LIST_TIME)).whitespace_nowrap().child(when(hit.at, now_ms)))
                        .children(badges.into_iter().map(kind_icon))
                        .when(file_only, |d| d.child(kind_icon("icons/attachment.svg"))),
                )
                .map(|d| {
                    if hit.snippet.is_empty() && thumbs.is_some() {
                        d
                    } else if hit.snippet.is_empty() {
                        let kind = [Has::IMAGE, Has::VIDEO, Has::AUDIO, Has::FILE].into_iter().find(|h| hit.has.contains(*h)).unwrap_or(Has::FILE);
                        d.child(div().text_size(px(14.)).line_height(px(20.)).italic().text_color(c(LIST_TIME)).child(has_label(kind)))
                    } else if let Some(atoms) = drawn {
                        d.child(crate::inline::inline(atoms, row_look()).held())
                    } else {
                        d.child(div().text_size(px(14.)).line_height(px(20.)).text_color(white_a(0.82)).child(snippet(&hit.snippet, &hit.marks)))
                    }
                })
                .children(thumbs.map(|t| thumb_strip(i, t, on))),
        )
        .into_any_element()
}

fn mini_label(text: SharedString, count: Option<usize>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(28.))
        .px(px(8.))
        .text_size(px(11.))
        .font_weight(FontWeight::BOLD)
        .text_color(c(0x8a8a8a))
        .child(text.to_uppercase())
        .children(count.map(|n| div().text_color(white_a(0.32)).child(crate::lang::num(n))))
        .into_any_element()
}

fn mini_row(i: usize, hit: &vector_core::search::Hit, drawn: Option<Rc<[Atom]>>, sel: bool, open: &str, store: &Store, now_ms: u64, on: &OnAct) -> AnyElement {
    let who = if hit.mine { store.my_npub.clone() } else { hit.author.clone().unwrap_or_else(|| hit.chat.clone()) };
    let name = if hit.mine { t::search_you().to_string() } else { store.chat_name(&who, &hit.chat) };
    let channel = (hit.chat != open)
        .then(|| store.community_of(&hit.chat).and_then(|com| com.channel(&hit.chat)).map(|ch| format!("#{}", ch.name))) // i18n-exempt: channel sigil
        .flatten();
    div()
        .id(SharedString::from(format!("mini-hit-{i}")))
        .flex()
        .flex_col()
        .gap(px(2.))
        .px(px(8.))
        .py(px(6.))
        .rounded(px(6.))
        .cursor_pointer()
        .when(sel, |d| d.bg(white_a(0.06)))
        .hover(|d| d.bg(white_a(0.04)))
        .on_mouse_move({
            let on = on.clone();
            move |_, w, cx| on(Act::Hover(i), w, cx)
        })
        .on_click(click(on, move || Act::Pick(i)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(crate::ui::avatar(store, &who, 18.))
                .child(div().flex_shrink(1.).min_w_0().font_weight(FontWeight::SEMIBOLD).text_size(px(13.)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(name))
                .children(channel.map(|ch| div().flex_shrink(1.).min_w_0().text_size(px(11.)).text_color(c(LIST_TIME)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(ch)))
                .child(div().flex_1())
                .child(div().flex_shrink_0().text_size(px(11.)).text_color(c(LIST_TIME)).whitespace_nowrap().child(when(hit.at, now_ms))),
        )
        .map(|d| {
            if hit.snippet.is_empty() {
                let kind = [Has::IMAGE, Has::VIDEO, Has::AUDIO, Has::FILE].into_iter().find(|h| hit.has.contains(*h)).unwrap_or(Has::FILE);
                d.child(div().pl(px(24.)).text_size(px(12.5)).line_height(px(17.)).italic().text_color(c(LIST_TIME)).child(has_label(kind)))
            } else if let Some(atoms) = drawn {
                d.child(div().pl(px(24.)).child(crate::inline::inline(atoms, mini_look()).clamp(3).held()))
            } else {
                d.child(div().pl(px(24.)).text_size(px(12.5)).line_height(px(17.)).line_clamp(3).text_color(white_a(0.82)).child(snippet(&hit.snippet, &hit.marks)))
            }
        })
        .into_any_element()
}

/// The compact search in a Community's member pane: suggestions for the filter being typed,
/// else the members whose names match and the messages that do.
pub fn mini(s: &Search, store: &Store, open: &str, members: Vec<AnyElement>, matched: usize, on: &OnAct) -> Vec<AnyElement> {
    if s.showing_suggestions() {
        return s.suggest.iter().enumerate().map(|(i, sg)| suggestion_row(i, sg, i == s.sel, store, true, on)).collect();
    }
    let now = store.now_ms();
    let mut out = Vec::new();
    if !members.is_empty() {
        out.push(mini_label(t::community_section_members(), Some(matched)));
        out.extend(members);
    }
    let messages = (s.corpus.is_some() && !s.loading).then_some(s.found.total);
    out.push(div().when(!out.is_empty(), |d| d.mt(px(10.))).child(mini_label(t::search_mini_messages(), messages)).into_any_element());
    if s.corpus.is_none() || s.loading {
        out.push(div().px(px(8.)).py(px(6.)).child(crate::kit::processing(t::search_indexing())).into_any_element());
    } else if s.found.hits.is_empty() {
        out.push(div().px(px(8.)).py(px(6.)).text_size(px(12.5)).italic().text_color(c(LIST_TIME)).child(t::search_none()).into_any_element());
    } else {
        out.extend(s.found.hits.iter().enumerate().map(|(i, hit)| mini_row(i, hit, s.drawn(hit, store, &mini_look()), i == s.sel, open, store, now, on)));
    }
    out
}

/// The panel: a centred card on a wide window, the whole window on a slim one.
pub fn view(s: &Search, store: &Store, thumbs: &std::collections::HashMap<usize, Strip>, vp: (f32, f32), slim: bool, cx: &App, on: &OnAct) -> AnyElement {
    let now = store.now_ms();
    let text_empty = s.input.read(cx).text().trim().is_empty();
    let field = div()
        .flex()
        .items_center()
        .gap(px(10.))
        .h(px(52.))
        .px(px(16.))
        .child(svg().path("icons/search.svg").size(px(18.)).text_color(c(ICON_MUTED)))
        .child(div().flex_1().min_w_0().child(s.input.clone()))
        .child(
            div()
                .id("search-sort")
                .tip(t::search_sort())
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(28.))
                .px(px(10.))
                .rounded(px(14.))
                .border_1()
                .border_color(white_a(0.12))
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(white_a(0.75))
                .cursor_pointer()
                .hover(|d| d.bg(white_a(0.06)))
                .on_click(click(on, || Act::Sort))
                .child(if s.oldest { t::search_oldest() } else { t::search_newest() }),
        )
        .child(
            div()
                .id("search-close")
                .tip(t::search_close())
                .size(px(28.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|d| d.bg(white_a(0.08)))
                .on_click(click(on, || Act::Close))
                .child(svg().path("icons/x.svg").size(px(13.)).text_color(white_a(0.7))),
        );
    let set: Vec<&Token> = s.parsed.tokens.iter().filter(|t| !t.value.is_empty()).collect();
    let chips = (!set.is_empty()).then(|| {
        div()
            .flex()
            .flex_wrap()
            .gap(px(6.))
            .px(px(16.))
            .pb(px(12.))
            .children(set.iter().map(|tok| chip(tok, store, &s.dir, on)))
    });

    let mut list = div().id("search-list").flex_1().min_h_0().overflow_y_scroll().track_scroll(&s.scroll).flex().flex_col().pb(px(8.));
    let mut status: Option<AnyElement> = None;
    if s.showing_suggestions() {
        let dated = matches!(s.suggest.first(), Some(Suggest::Value { key: Key::Before | Key::After | Key::During, .. }));
        let title = if text_empty {
            Some(t::search_options())
        } else if dated {
            Some(t::search_date_hint())
        } else {
            None
        };
        list = list
            .children(title.map(|t| {
                div().px(px(20.)).pt(px(12.)).pb(px(6.)).text_size(px(12.)).font_weight(FontWeight::BOLD).text_color(c(LIST_TIME)).child(if text_empty { t.to_uppercase() } else { t.to_string() })
            }))
            .children(s.suggest.iter().enumerate().map(|(i, sg)| suggestion_row(i, sg, i == s.sel, store, false, on)));
    } else if s.corpus.is_none() {
        status = Some(crate::kit::processing(t::search_indexing()).into_any_element());
    } else if s.found.hits.is_empty() {
        status = Some(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(6.))
                .py(px(40.))
                .child(svg().path("icons/file-search.svg").size(px(40.)).text_color(white_a(0.25)))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(t::search_none()))
                .child(div().text_size(px(13.)).text_color(c(LIST_TIME)).child(t::search_none_hint()))
                .into_any_element(),
        );
    } else {
        let hits = &s.found.hits;
        let mut rows: Vec<AnyElement> = Vec::with_capacity(hits.len() * 2);
        for (i, hit) in hits.iter().enumerate() {
            if i == 0 || hits[i - 1].chat != hit.chat {
                rows.push(header(store, &hit.chat));
            }
            rows.push(result_row(i, hit, s.drawn(hit, store, &row_look()), thumbs.get(&i), i == s.sel, store, now, on));
        }
        list = list.children(rows);
    }
    let count = (!s.showing_suggestions() && s.corpus.is_some() && !s.found.hits.is_empty()).then(|| {
        let shown = s.found.hits.len();
        let label = if shown < s.found.total {
            if s.oldest { t::search_results_capped_oldest(shown, s.found.total) } else { t::search_results_capped(shown, s.found.total) }
        } else {
            t::search_results(s.found.total)
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .px(px(20.))
            .py(px(8.))
            .border_t_1()
            .border_color(c(DIVIDER))
            .text_size(px(12.))
            .text_color(c(LIST_TIME))
            .child(label)
            .child(div().text_color(white_a(0.3)).child(format!("{:.1} ms", s.took as f32 / 1000.))) // i18n-exempt: unit
    });
    let body = div()
        .flex()
        .flex_col()
        .size_full()
        .child(field)
        .child(div().h(px(1.)).bg(c(DIVIDER)).mb(px(if chips.is_some() { 12. } else { 0. })))
        .children(chips)
        .child(div().flex_1().min_h_0().flex().flex_col().children(status).child(list))
        .children(count);

    let (w, h) = vp;
    let card = if slim {
        div().absolute().inset_0().bg(c(APP_BG)).pt(px(28.)).child(body)
    } else {
        let width = (w - 48.).min(680.);
        let height = (h * 0.72).clamp(320., 640.);
        div()
            .absolute()
            .top(px((h * 0.1).max(36.)))
            .left(px((w - width) / 2.))
            .w(px(width))
            .h(px(height))
            .rounded(px(12.))
            .border_1()
            .border_color(c(0x262626))
            .bg(c(APP_BG))
            .overflow_hidden()
            .shadow(vec![gpui::BoxShadow {
                color: ca(0, 0, 0, 0.6),
                offset: gpui::point(px(0.), px(8.)),
                blur_radius: px(32.),
                spread_radius: px(0.),
                inset: false,
            }])
            .child(body)
    };
    gpui::deferred(
        div()
            .id("search-backdrop")
            .absolute()
            .inset_0()
            .bg(ca(0, 0, 0, if slim { 1. } else { 0.6 }))
            .occlude()
            .on_click(click(on, || Act::Close))
            .child(
                card.id("search-card")
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .font_family(INTER)
                    .text_color(c(TEXT)),
            ),
    )
    .with_priority(11)
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> Directory {
        let n = |id: &str, name: &str| Named { id: id.into(), name: name.into(), folded: vector_core::search::fold(name) };
        Directory {
            me: "npub1me".into(),
            people: vec![n("npub1al", "Alice"), n("npub1ali", "Alicia Keys"), n("npub1bo", "Bob")],
            chats: vec![n("npub1bo", "Bob"), n("chan1", "#general · Vector"), n("chan2", "#dev · Vector")],
        }
    }

    #[test]
    fn splits_filters_from_words() {
        let p = parse("hello from:alice has:image  world in:\"Alicia Keys\"");
        assert_eq!(p.words, "hello world");
        let keys: Vec<(Key, &str)> = p.tokens.iter().map(|t| (t.key, t.value.as_str())).collect();
        assert_eq!(keys, [(Key::From, "alice"), (Key::Has, "image"), (Key::In, "Alicia Keys")]);
        assert_eq!(&"hello from:alice has:image  world in:\"Alicia Keys\""[p.tokens[2].range.clone()], "in:\"Alicia Keys\"");
        // An unknown key is just a word.
        assert_eq!(parse("ratio:2").words, "ratio:2");
    }

    #[test]
    fn names_resolve_exact_first() {
        let d = dir();
        let f = filter_of(&parse("from:alice").tokens, &d, 0, false);
        assert_eq!(f.from, ["npub1al"]);
        let f = filter_of(&parse("from:ali").tokens, &d, 0, false);
        assert_eq!(f.from, ["npub1al", "npub1ali"]);
        let f = filter_of(&parse("from:me").tokens, &d, 0, false);
        assert!(f.from_me && f.from.is_empty());
        let f = filter_of(&parse("mentions:me").tokens, &d, 0, false);
        assert_eq!(f.mentions, ["npub1me"]);
        let f = filter_of(&parse("from:zed").tokens, &d, 0, false);
        assert_eq!(f.from, [NOBODY]);
    }

    #[test]
    fn chats_and_kinds() {
        let d = dir();
        let f = filter_of(&parse("in:#general has:img").tokens, &d, 0, false);
        assert_eq!(f.chats, ["chan1"]);
        assert_eq!(f.has, Has::IMAGE);
        let f = filter_of(&parse("in:vector").tokens, &d, 0, false);
        assert_eq!(f.chats, ["chan1", "chan2"]);
    }

    #[test]
    fn dates_bound_whole_days() {
        let now = Local.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap().timestamp_millis() as u64;
        let (s, e) = day_span("2026-11-18", now).unwrap();
        assert_eq!(e - s, 86_400_000 - 1);
        let (ts, _) = day_span("today", now).unwrap();
        assert_eq!(ts, e + 1);
        let (ms, me) = day_span("2026-02", now).unwrap();
        assert_eq!((me + 1 - ms) / 86_400_000, 28);
        assert!(day_span("2026-13-01", now).is_none());
        assert!(day_span("soon", now).is_none());
        let f = filter_of(&parse("during:2026-11-18").tokens, &dir(), now, false);
        assert_eq!((f.after, f.before), (Some(s), Some(e)));
        let f = filter_of(&parse("before:2026-11-18").tokens, &dir(), now, false);
        assert_eq!(f.before, Some(s - 1));
    }

    #[test]
    fn suggests_for_the_word_at_the_caret() {
        let d = dir();
        let (_, s) = suggestions("", 0, &d, 0);
        assert_eq!(s.len(), KEYS.len());
        let (r, s) = suggestions("hi fr", 5, &d, 0);
        assert_eq!(r, 3..5);
        assert_eq!(s, [Suggest::Key(Key::From)]);
        let (r, s) = suggestions("hi from:al", 10, &d, 0);
        assert_eq!(r, 3..10);
        let names: Vec<String> = s.iter().map(|s| match s { Suggest::Value { value, .. } => value.clone(), _ => String::new() }).collect();
        assert_eq!(names, ["Alice", "Alicia Keys"]);
        assert_eq!(completion(&s[1]), "from:\"Alicia Keys\" ");
        let (_, s) = suggestions("has:v", 5, &d, 0);
        assert_eq!(s.len(), 1);
        // Ordinary words offer nothing.
        assert!(suggestions("hello", 5, &d, 0).1.is_empty());
    }

    #[test]
    fn headers_show_bold_and_subtext_plain() {
        assert_eq!(demote_headers("# Release notes\nbody"), "**Release notes**\nbody");
        assert_eq!(demote_headers("###   spaced  \n-# small print"), "**spaced**\nsmall print");
        assert_eq!(demote_headers("#hashtag\n# \n####### seven"), "#hashtag\n# \n####### seven");
        let mark = char::from_u32(0xF0000).unwrap();
        assert_eq!(demote_headers(&format!("{mark}# Coloured")), format!("{mark}**Coloured**"));
    }

    fn text(a: &[Atom]) -> String {
        a.iter()
            .map(|a| match a {
                Atom::Text { text, .. } => text.to_string(),
                Atom::Image { .. } => "<img>".into(),
                _ => String::new(),
            })
            .collect()
    }

    fn plain(t: &str, look: &Look) -> Atom {
        Atom::Text { text: t.to_string().into(), look: look.clone(), kind: Kind::Plain }
    }

    #[test]
    fn windows_the_first_match_and_marks_every_one() {
        let look = row_look();
        let long = format!("{}needle here and Needle again{}", "lorem ipsum ".repeat(20), " dolor".repeat(40));
        let atoms = window(vec![plain(&long, &look)], &["needle".into()], &look, false);
        let shown = text(&atoms);
        assert!(shown.starts_with('…') && shown.ends_with('…'), "{shown}");
        assert!(shown.contains("needle here and Needle again"));
        let marked: Vec<String> = atoms
            .iter()
            .filter_map(|a| match a {
                Atom::Text { text, look: l, .. } if l.weight == FontWeight::SEMIBOLD => Some(text.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(marked, ["needle", "Needle"]);
    }

    #[test]
    fn a_match_across_atoms_is_marked_in_each() {
        let look = row_look();
        let mut bold = look.clone();
        bold.weight = FontWeight::BOLD;
        let atoms = vec![
            plain("say ", &look),
            Atom::Text { text: "hel".into(), look: bold, kind: Kind::Plain },
            plain("lo world", &look),
            Atom::Image { src: gpui::ImageSource::from(std::path::PathBuf::from("x.png")), size: 14., drop: 2., tip: None, alt: ":x:".into() },
        ];
        let out = window(atoms, &["hello".into()], &look, false);
        assert_eq!(text(&out), "say hello world<img>");
        let accent: String = out
            .iter()
            .filter_map(|a| match a {
                Atom::Text { text, look: l, .. } if l.color == c(ACCENT) => Some(text.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(accent, "hello");
    }

    #[test]
    fn coloured_matches_keep_their_colour() {
        let look = row_look();
        let mut red = look.clone();
        red.color = gpui::red();
        let out = window(vec![Atom::Text { text: "rainbow".into(), look: red, kind: Kind::Plain }], &["rain".into()], &look, true);
        let Atom::Text { look: l, .. } = &out[0] else { panic!() };
        assert_eq!((l.color, l.weight), (gpui::red(), FontWeight::SEMIBOLD));
    }
}
