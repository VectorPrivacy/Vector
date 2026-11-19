//! On-device message search: every chat's text folded into one buffer and scanned with
//! `memchr::memmem`, so a query is a single SIMD pass with nothing sent anywhere.
//!
//! A [`Corpus`] holds plaintext, so it lives only as long as the search surface that built it.

use std::ops::Range;

use memchr::memmem;

/// One searchable message.
struct Doc {
    id: Box<str>,
    chat: u32,
    author: Option<Box<str>>,
    mine: bool,
    at: u64,
    has: Has,
    mentions: Box<[Box<str>]>,
    /// Span in `Corpus::text`.
    text: Range<u32>,
    /// Span in `Corpus::folded`.
    folded: Range<u32>,
}

/// Messages to search, newest first.
#[derive(Default)]
pub struct Corpus {
    chats: Vec<Box<str>>,
    docs: Vec<Doc>,
    text: String,
    /// Each doc folded by [`fold`], NUL-separated so a match never spans two.
    folded: String,
}

/// What [`Corpus::search`] found.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Found {
    /// The newest `limit` matches.
    pub hits: Vec<Hit>,
    /// Every match, including those past `limit`.
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: String,
    pub chat: String,
    pub author: Option<String>,
    pub mine: bool,
    pub at: u64,
    pub has: Has,
    /// A window of the message around its first match, on one line.
    pub snippet: String,
    /// Each term's occurrences within `snippet`, in order.
    pub marks: Vec<Range<usize>>,
}

/// What a message carries, for `has:` filters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Has(u8);

impl Has {
    pub const LINK: Has = Has(1);
    pub const IMAGE: Has = Has(2);
    pub const VIDEO: Has = Has(4);
    pub const AUDIO: Has = Has(8);
    /// Any attachment, media included.
    pub const FILE: Has = Has(16);

    pub fn contains(self, other: Has) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// From a message's text and its attachments' extensions.
    pub fn of<'a>(text: &str, extensions: impl IntoIterator<Item = &'a str>) -> Has {
        let mut has = Has::default();
        if memmem::find(text.as_bytes(), b"https://").is_some() || memmem::find(text.as_bytes(), b"http://").is_some() {
            has |= Has::LINK;
        }
        for ext in extensions {
            has |= Has::FILE;
            let mime = crate::crypto::mime_from_extension(ext);
            if mime.starts_with("image/") {
                has |= Has::IMAGE;
            } else if mime.starts_with("video/") {
                has |= Has::VIDEO;
            } else if mime.starts_with("audio/") {
                has |= Has::AUDIO;
            }
        }
        has
    }
}

impl std::ops::BitOr for Has {
    type Output = Has;
    fn bitor(self, rhs: Has) -> Has {
        Has(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Has {
    fn bitor_assign(&mut self, rhs: Has) {
        self.0 |= rhs.0;
    }
}

/// Narrows a search. Each list matches any of its entries; the lists all have to match.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    /// Authors by npub. `from_me` counts this account's own messages too.
    pub from: Vec<String>,
    pub from_me: bool,
    /// Mentioned npubs.
    pub mentions: Vec<String>,
    pub has: Has,
    /// Chat ids: DM npubs or channel ids.
    pub chats: Vec<String>,
    /// Inclusive bounds in ms.
    pub after: Option<u64>,
    pub before: Option<u64>,
    /// Oldest first instead of newest.
    pub oldest: bool,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        self.from.is_empty()
            && !self.from_me
            && self.mentions.is_empty()
            && self.has.is_empty()
            && self.chats.is_empty()
            && self.after.is_none()
            && self.before.is_none()
    }
}

/// One message as [`load`] reads it, before it joins a [`Corpus`].
#[derive(Debug, Clone, Default)]
pub struct Record {
    pub chat: String,
    pub id: String,
    pub author: Option<String>,
    pub mine: bool,
    pub at: u64,
    pub text: String,
    /// Attachments' `(extension, file name)`.
    pub files: Vec<(String, String)>,
}

/// A parsed query: whitespace-separated terms that must all appear, `"quoted phrases"` kept whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    terms: Vec<String>,
}

impl Query {
    pub fn parse(input: &str) -> Self {
        let mut terms = Vec::new();
        let mut rest = input;
        while let Some(start) = rest.find(|c: char| !c.is_whitespace()) {
            rest = &rest[start..];
            let (raw, next) = if let Some(body) = rest.strip_prefix('"') {
                match body.find('"') {
                    Some(end) => (&body[..end], &body[end + 1..]),
                    None => (body, ""),
                }
            } else {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                (&rest[..end], &rest[end..])
            };
            let mut folded = String::new();
            fold_into(raw, &mut folded, None);
            if !folded.trim().is_empty() && !terms.contains(&folded) {
                terms.push(folded);
            }
            rest = next;
        }
        // The longest term scans the buffer; the rest only confirm its candidates.
        terms.sort_by_key(|t| std::cmp::Reverse(t.len()));
        Self { terms }
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
}

/// Case and accent folding: `É` and `e` match, as do `ß`/`ss`-free lowercase forms. Scripts
/// without case (CJK, Arabic) pass through, so substring matching needs no word breaks.
pub fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    fold_into(text, &mut out, None);
    out
}

/// [`fold`], appending to `out`; `map` gets, per folded byte, the source byte it came from.
fn fold_into(text: &str, out: &mut String, mut map: Option<&mut Vec<u32>>) {
    for (at, ch) in text.char_indices() {
        let before = out.len();
        if ch.is_ascii() {
            // NUL separates documents.
            out.push(if ch == '\0' { ' ' } else { ch.to_ascii_lowercase() });
        } else {
            unicode_normalization::char::decompose_canonical(ch, |d| {
                if !unicode_normalization::char::is_combining_mark(d) {
                    out.extend(d.to_lowercase());
                }
            });
        }
        if let Some(m) = map.as_deref_mut() {
            m.extend(std::iter::repeat_n(at as u32, out.len() - before));
        }
    }
    if let Some(m) = map {
        m.push(text.len() as u32);
    }
}

impl Corpus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Adds a message as `shown`: its text with mentions already named. Push newest first.
    pub fn push(&mut self, record: &Record, shown: &str) {
        let names = record.files.iter().map(|(_, n)| n.as_str()).filter(|n| !n.is_empty());
        let has = Has::of(&record.text, record.files.iter().map(|(e, _)| e.as_str()));
        if shown.trim().is_empty() && has.is_empty() {
            return;
        }
        let chat = match self.chats.iter().rposition(|c| **c == *record.chat) {
            Some(ix) => ix as u32,
            None => {
                self.chats.push(record.chat.as_str().into());
                (self.chats.len() - 1) as u32
            }
        };
        let t0 = self.text.len() as u32;
        self.text.push_str(shown.trim());
        for name in names {
            if self.text.len() as u32 > t0 {
                self.text.push('\n');
            }
            self.text.push_str(name);
        }
        let t1 = self.text.len() as u32;
        let f0 = self.folded.len() as u32;
        fold_into(&self.text[t0 as usize..], &mut self.folded, None);
        let f1 = self.folded.len() as u32;
        self.folded.push('\0');
        let mentions: Vec<Box<str>> = crate::types::extract_mentions(&record.text).into_iter().map(Into::into).collect();
        self.docs.push(Doc {
            id: record.id.as_str().into(),
            chat,
            author: record.author.as_deref().map(Into::into),
            mine: record.mine,
            at: record.at,
            has,
            mentions: mentions.into_boxed_slice(),
            text: t0..t1,
            folded: f0..f1,
        });
    }

    /// Messages holding every term and passing `filter`. A query with no terms lists every
    /// message the filter passes.
    pub fn search(&self, query: &Query, filter: &Filter, limit: usize) -> Found {
        let mut found = Found::default();
        if query.is_empty() && filter.is_empty() {
            return found;
        }
        let chats: Vec<u32> = filter
            .chats
            .iter()
            .filter_map(|c| self.chats.iter().position(|x| **x == **c).map(|i| i as u32))
            .collect();
        if !filter.chats.is_empty() && chats.is_empty() {
            return found;
        }
        let passes = |d: &Doc| {
            (chats.is_empty() || chats.contains(&d.chat))
                && (filter.from.is_empty() && !filter.from_me
                    || filter.from_me && d.mine
                    || d.author.as_deref().is_some_and(|a| filter.from.iter().any(|f| f == a)))
                && (filter.mentions.is_empty() || d.mentions.iter().any(|m| filter.mentions.iter().any(|f| **f == **m)))
                && d.has.contains(filter.has)
                && filter.after.is_none_or(|a| d.at >= a)
                && filter.before.is_none_or(|b| d.at <= b)
        };
        let take = |found: &mut Found, d: &Doc| {
            found.total += 1;
            if filter.oldest {
                // Walking newest first: keep the last `limit` seen.
                found.hits.push(self.hit(d, query));
                if found.hits.len() > limit {
                    found.hits.remove(0);
                }
            } else if found.hits.len() < limit {
                found.hits.push(self.hit(d, query));
            }
        };
        let Some((lead, rest)) = query.terms.split_first() else {
            for d in self.docs.iter().filter(|d| passes(d)) {
                take(&mut found, d);
            }
            if filter.oldest {
                found.hits.reverse();
            }
            return found;
        };
        let finders: Vec<memmem::Finder> = rest.iter().map(|t| memmem::Finder::new(t.as_bytes())).collect();
        let hay = self.folded.as_bytes();
        let lead = memmem::Finder::new(lead.as_bytes());
        let mut cursor = 0usize;
        let mut doc_ix = 0usize;
        while cursor < hay.len() {
            let Some(pos) = lead.find(&hay[cursor..]).map(|p| p + cursor) else { break };
            // Docs are laid out in order, so the owner is at or after the last one found.
            doc_ix += self.docs[doc_ix..].partition_point(|d| (d.folded.end as usize) < pos);
            let Some(doc) = self.docs.get(doc_ix) else { break };
            cursor = doc.folded.end as usize + 1;
            doc_ix += 1;
            let body = &hay[doc.folded.start as usize..doc.folded.end as usize];
            if passes(doc) && finders.iter().all(|f| f.find(body).is_some()) {
                take(&mut found, doc);
            }
        }
        if filter.oldest {
            found.hits.reverse();
        }
        found
    }

    fn hit(&self, doc: &Doc, query: &Query) -> Hit {
        let text = &self.text[doc.text.start as usize..doc.text.end as usize];
        let (snippet, marks) = snippet(text, &query.terms, 60);
        Hit {
            id: doc.id.to_string(),
            chat: self.chats[doc.chat as usize].to_string(),
            author: doc.author.as_deref().map(str::to_string),
            mine: doc.mine,
            at: doc.at,
            has: doc.has,
            snippet,
            marks,
        }
    }
}

/// `text` cut to about `context` characters either side of its first match, whitespace runs
/// collapsed, with every term's occurrences marked in the result.
fn snippet(text: &str, terms: &[String], context: usize) -> (String, Vec<Range<usize>>) {
    let mut folded = String::with_capacity(text.len());
    let mut map = Vec::with_capacity(text.len() + 1);
    fold_into(text, &mut folded, Some(&mut map));
    let mut spans: Vec<Range<usize>> = Vec::new();
    for term in terms {
        let mut from = 0;
        while let Some(at) = memmem::find(&folded.as_bytes()[from..], term.as_bytes()).map(|p| p + from) {
            let end = at + term.len();
            spans.push(map[at] as usize..source_end(&map, end, text));
            from = end;
        }
    }
    spans.sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(spans.len());
    for s in spans {
        match merged.last_mut() {
            Some(last) if s.start <= last.end => last.end = last.end.max(s.end),
            _ => merged.push(s),
        }
    }
    let first = merged.first().map_or(0, |r| r.start);
    let start = back_chars(text, first, context);
    let end = merged.first().map_or(0, |r| r.end);
    let stop = fwd_chars(text, end, context * 2);

    let mut out = String::with_capacity(stop - start + 8);
    let mut marks = Vec::new();
    if start > 0 {
        out.push('…');
    }
    let mut open: Option<usize> = None;
    let mut spans = merged.iter().peekable();
    let mut space = false;
    for (i, ch) in text[start..stop].char_indices() {
        let at = start + i;
        while spans.peek().is_some_and(|s| s.end <= at) {
            let s = spans.next().unwrap();
            if let Some(o) = open.take() {
                marks.push(o..out.len());
            }
            let _ = s;
        }
        if open.is_none() && spans.peek().is_some_and(|s| s.start <= at) {
            if space {
                out.push(' ');
                space = false;
            }
            open = Some(out.len());
        }
        if ch.is_whitespace() {
            space = !out.is_empty() && !out.ends_with('…');
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(ch);
    }
    if let Some(o) = open.take() {
        marks.push(o..out.len());
    }
    if stop < text.len() {
        out.push('…');
    }
    (out, marks)
}

/// The source byte after folded byte `end - 1`: the end of the character it came from.
fn source_end(map: &[u32], end: usize, text: &str) -> usize {
    let src = map[end - 1] as usize;
    src + text[src..].chars().next().map_or(0, char::len_utf8)
}

fn back_chars(text: &str, from: usize, n: usize) -> usize {
    let head = &text[..from];
    let cut = head.char_indices().rev().nth(n.saturating_sub(1)).map_or(0, |(i, _)| i);
    if cut == 0 {
        return 0;
    }
    // Start on a word where one is near.
    head[cut..].find(char::is_whitespace).filter(|&w| w < 16).map_or(cut, |w| cut + w + 1).min(from)
}

fn fwd_chars(text: &str, from: usize, n: usize) -> usize {
    text[from..].char_indices().nth(n).map_or(text.len(), |(i, _)| from + i)
}

/// `text` with each mentioned npub (`@npub1…`, `nostr:npub1…` or bare) shown as `@name`,
/// so a search for the name finds the mention.
pub fn name_mentions(text: &str, name: impl Fn(&str) -> Option<String>) -> String {
    let mentions = crate::types::extract_mentions(text);
    if mentions.is_empty() {
        return text.to_string();
    }
    let base = text.as_ptr() as usize;
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for m in mentions {
        let mut start = m.as_ptr() as usize - base;
        let end = start + m.len();
        if text[..start].ends_with("nostr:") {
            start -= 6;
        }
        if text[..start].ends_with('@') {
            start -= 1;
        }
        if start < last {
            continue;
        }
        out.push_str(&text[last..start]);
        match name(m) {
            Some(n) => {
                out.push('@');
                out.push_str(&n);
            }
            None => out.push_str(&text[start..end]),
        }
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

/// Every stored message's current text (edits applied), newest first, for the account the
/// caller is bound to. Banned authors' posts are left out, as the timeline leaves them out.
#[cfg(not(target_arch = "wasm32"))]
pub fn load() -> Result<Vec<Record>, String> {
    use std::collections::HashMap;
    use crate::stored_event::event_kind;

    struct Row {
        id: String,
        kind: u16,
        chat: String,
        content: String,
        reference: Option<String>,
        at: u64,
        mine: bool,
        npub: Option<String>,
    }

    let (rows, communities, files) = {
        let conn = crate::db::get_db_connection_guard_static()?;
        let mut stmt = conn
            .prepare(
                "SELECT e.id, e.kind, c.chat_identifier, e.content, e.reference_id, e.created_at, e.tags, e.mine, e.npub \
                 FROM events e JOIN chats c ON c.id = e.chat_id \
                 WHERE e.kind IN (?1, ?2, ?3, ?4) \
                 ORDER BY e.created_at DESC, e.received_at DESC",
            )
            .map_err(|e| format!("Failed to prepare search query: {e}"))?;
        let rows: Vec<Row> = stmt
            .query_map(
                rusqlite::params![
                    event_kind::CHAT_MESSAGE as i32,
                    event_kind::PRIVATE_DIRECT_MESSAGE as i32,
                    event_kind::FILE_ATTACHMENT as i32,
                    event_kind::MESSAGE_EDIT as i32,
                ],
                |r| {
                    let tags: String = r.get(6)?;
                    let secs = r.get::<_, i64>(5)? as u64;
                    Ok(Row {
                        id: r.get(0)?,
                        kind: r.get::<_, i32>(1)? as u16,
                        chat: r.get(2)?,
                        content: r.get(3)?,
                        reference: r.get(4)?,
                        at: secs * 1000 + ms_of(&tags),
                        mine: r.get::<_, i32>(7)? != 0,
                        npub: r.get(8)?,
                    })
                },
            )
            .map_err(|e| format!("Failed to run search query: {e}"))?
            .filter_map(Result::ok)
            .collect();
        let communities: HashMap<String, String> = conn
            .prepare("SELECT channel_id, community_id FROM community_channels")
            .and_then(|mut s| {
                s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                    .map(|it| it.filter_map(Result::ok).collect())
            })
            .unwrap_or_default();
        let files: Vec<(String, String, String)> = conn
            .prepare("SELECT event_id, extension, name FROM attachments ORDER BY event_id, att_index")
            .and_then(|mut s| {
                s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
                    .map(|it| it.filter_map(Result::ok).collect())
            })
            .unwrap_or_default();
        (rows, communities, files)
    };

    let mut banned: HashMap<&str, std::collections::HashSet<String>> = HashMap::new();
    for community in communities.values() {
        banned.entry(community.as_str()).or_insert_with(|| {
            use nostr_sdk::prelude::ToBech32;
            crate::db::community::banned_set(community)
                .iter()
                .filter_map(|b| nostr_sdk::prelude::PublicKey::from_slice(b).ok()?.to_bech32().ok())
                .collect()
        });
    }

    crate::crypto::open_batch(|at_rest| {
        let mut by_event: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for (event, ext, name) in files {
            by_event.entry(event).or_default().push((ext, at_rest.open_or_keep(name)));
        }
        // Newest edit per message: rows arrive newest first, so the first seen wins.
        let mut edits: HashMap<String, String> = HashMap::new();
        for row in rows.iter().filter(|r| r.kind == event_kind::MESSAGE_EDIT) {
            let Some(target) = &row.reference else { continue };
            if !edits.contains_key(target) {
                if let Ok(text) = at_rest.open(row.content.clone()) {
                    edits.insert(target.clone(), text);
                }
            }
        }
        let mut out = Vec::with_capacity(rows.len());
        for row in rows.into_iter().filter(|r| r.kind != event_kind::MESSAGE_EDIT) {
            let is_banned = match (&row.npub, communities.get(&row.chat)) {
                (Some(author), Some(community)) => banned.get(community.as_str()).is_some_and(|b| b.contains(author)),
                _ => false,
            };
            if is_banned {
                continue;
            }
            let files = by_event.remove(&row.id).unwrap_or_default();
            let text = match edits.remove(&row.id) {
                Some(edited) => edited,
                // A file event's content is its URL, not words.
                None if row.kind == event_kind::FILE_ATTACHMENT => String::new(),
                None => match at_rest.open(row.content) {
                    Ok(t) => t,
                    Err(_) => continue,
                },
            };
            out.push(Record { chat: row.chat, id: row.id, author: row.npub, mine: row.mine, at: row.at, text, files });
        }
        Ok(out)
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn ms_of(tags: &str) -> u64 {
    // `["ms","123"]` without parsing every row's whole tag list.
    let Some(at) = tags.find("[\"ms\",\"") else { return 0 };
    let digits = &tags[at + 7..];
    digits[..digits.find('"').unwrap_or(0)].parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(chat: &str, id: &str, author: Option<&str>, mine: bool, at: u64, text: &str) -> Record {
        Record { chat: chat.into(), id: id.into(), author: author.map(Into::into), mine, at, text: text.into(), files: Vec::new() }
    }

    fn add(c: &mut Corpus, r: Record) {
        let shown = r.text.clone();
        c.push(&r, &shown);
    }

    fn corpus() -> Corpus {
        let mut c = Corpus::new();
        add(&mut c, rec("npub1a", "m4", Some("npub1a"), false, 4000, "Café at nine? Bring the BLUEPRINTS https://x.io"));
        add(&mut c, rec("chan", "m3", Some("npub1b"), false, 3000, "the blueprints are in the shared folder"));
        add(&mut c, rec("npub1a", "m2", None, true, 2000, "see you at the café"));
        add(&mut c, rec("chan", "m1", Some("npub1c"), false, 1000, "東京で会いましょう"));
        let mut pic = rec("chan", "m0", Some("npub1b"), false, 500, "");
        pic.files.push(("png".into(), "harbour.png".into()));
        add(&mut c, pic);
        c
    }

    fn find(c: &Corpus, q: &str) -> Found {
        c.search(&Query::parse(q), &Filter::default(), 10)
    }

    fn ids(f: &Found) -> Vec<&str> {
        f.hits.iter().map(|h| h.id.as_str()).collect()
    }

    #[test]
    fn folds_case_and_accents() {
        let c = corpus();
        assert_eq!(ids(&find(&c, "CAFE")), ["m4", "m2"]);
        assert_eq!(ids(&find(&c, "blueprints")), ["m4", "m3"]);
    }

    #[test]
    fn every_term_must_match_in_any_order() {
        let c = corpus();
        assert_eq!(ids(&find(&c, "folder blueprints")), ["m3"]);
        assert!(find(&c, "folder café").hits.is_empty());
    }

    #[test]
    fn phrases_stay_whole() {
        let c = corpus();
        assert_eq!(ids(&find(&c, "\"at the\"")), ["m2"]);
        assert_eq!(ids(&find(&c, "at the")), ["m4", "m2"]);
    }

    #[test]
    fn cjk_matches_without_word_breaks() {
        let c = corpus();
        assert_eq!(ids(&find(&c, "会い")), ["m1"]);
    }

    fn chat(id: &str) -> Filter {
        Filter { chats: vec![id.into()], ..Default::default() }
    }

    #[test]
    fn scope_and_limit() {
        let c = corpus();
        assert_eq!(ids(&c.search(&Query::parse("the"), &chat("chan"), 10)), ["m3"]);
        let f = c.search(&Query::parse("e"), &Filter::default(), 2);
        assert_eq!(ids(&f), ["m4", "m3"]);
        assert_eq!(f.total, 3);
        assert!(c.search(&Query::parse("the"), &chat("nowhere"), 10).hits.is_empty());
    }

    #[test]
    fn oldest_first_keeps_the_oldest() {
        let c = corpus();
        let f = c.search(&Query::parse("e"), &Filter { oldest: true, ..Default::default() }, 2);
        assert_eq!(ids(&f), ["m2", "m3"]);
        assert_eq!(f.total, 3);
    }

    #[test]
    fn filters_alone_list_messages() {
        let c = corpus();
        let from = Filter { from: vec!["npub1b".into()], ..Default::default() };
        assert_eq!(ids(&c.search(&Query::parse(""), &from, 10)), ["m3", "m0"]);
        let me = Filter { from_me: true, ..Default::default() };
        assert_eq!(ids(&c.search(&Query::parse("cafe"), &me, 10)), ["m2"]);
        let image = Filter { has: Has::IMAGE, ..Default::default() };
        assert_eq!(ids(&c.search(&Query::parse(""), &image, 10)), ["m0"]);
        let link = Filter { has: Has::LINK, ..Default::default() };
        assert_eq!(ids(&c.search(&Query::parse(""), &link, 10)), ["m4"]);
        let span = Filter { after: Some(1000), before: Some(3000), ..Default::default() };
        assert_eq!(ids(&c.search(&Query::parse(""), &span, 10)), ["m3", "m2", "m1"]);
    }

    #[test]
    fn file_names_are_searchable() {
        let c = corpus();
        assert_eq!(ids(&find(&c, "harbour")), ["m0"]);
    }

    #[test]
    fn mentions_filter_and_names() {
        let npub = "npub1sg6plzptd64u62a878hep2kev88swjh3tw00gjsfl8f237lmu63q0uf63m";
        let mut c = Corpus::new();
        let r = rec("chan", "a", Some("npub1x"), false, 2, &format!("ping @{npub} about lunch"));
        let shown = name_mentions(&r.text, |n| (n == npub).then(|| "Alice".to_string()));
        assert_eq!(shown, "ping @Alice about lunch");
        c.push(&r, &shown);
        add(&mut c, rec("chan", "b", Some("npub1x"), false, 1, "lunch?"));
        let f = Filter { mentions: vec![npub.into()], ..Default::default() };
        assert_eq!(ids(&c.search(&Query::parse("lunch"), &f, 10)), ["a"]);
        assert_eq!(ids(&find(&c, "alice")), ["a"]);
    }

    #[test]
    fn no_match_across_messages() {
        let mut c = Corpus::new();
        add(&mut c, rec("x", "a", None, true, 2, "end"));
        add(&mut c, rec("x", "b", None, true, 1, "start"));
        assert!(find(&c, "endstart").hits.is_empty());
        assert!(find(&c, "dst").hits.is_empty());
    }

    #[test]
    fn marks_point_at_the_original_text() {
        let c = corpus();
        let hit = &find(&c, "cafe blueprints").hits[0];
        let marked: Vec<&str> = hit.marks.iter().map(|r| &hit.snippet[r.clone()]).collect();
        assert_eq!(marked, ["Café", "BLUEPRINTS"]);
    }

    #[test]
    fn long_text_is_cut_around_the_match() {
        let mut c = Corpus::new();
        let long = format!("{} needle {}", "lorem ipsum ".repeat(40), "dolor sit ".repeat(40));
        add(&mut c, rec("x", "a", None, true, 1, &long));
        let hit = &find(&c, "needle").hits[0];
        assert!(hit.snippet.starts_with('…') && hit.snippet.ends_with('…'), "{}", hit.snippet);
        assert!(hit.snippet.chars().count() < 200);
        assert_eq!(&hit.snippet[hit.marks[0].clone()], "needle");
    }

    #[test]
    fn whitespace_collapses_in_snippets() {
        let mut c = Corpus::new();
        add(&mut c, rec("x", "a", None, true, 1, "line one\n\n  line   two"));
        let hit = &find(&c, "two").hits[0];
        assert_eq!(hit.snippet, "line one line two");
        assert_eq!(&hit.snippet[hit.marks[0].clone()], "two");
    }

    #[test]
    fn blank_queries_find_nothing() {
        let c = corpus();
        assert!(Query::parse("   ").is_empty());
        assert!(Query::parse("\"\"").is_empty());
        assert_eq!(find(&c, ""), Found::default());
    }

    #[test]
    fn ms_tag_is_read() {
        assert_eq!(ms_of(r#"[["p","x"],["ms","417"]]"#), 417);
        assert_eq!(ms_of("[]"), 0);
    }
}
