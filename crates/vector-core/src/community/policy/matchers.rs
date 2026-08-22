//! Text and link matching (§6) — the deterministic rules.
//!
//! Hit counting, shared by every text rule: patterns compile into one set,
//! matching is leftmost-first and non-overlapping, and ONE hit is one distinct
//! matched span (ten patterns hitting one span is one hit). Patterns sort
//! bytewise before matching so their declaration order cannot decide which span
//! wins.

use super::document::{ExemptKind, ExemptPatterns};
use super::normalize::is_token_char;
use super::types::Span;

/// One matched span in the normalized text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hit {
    pub start: usize,
    pub end: usize,
}

impl Hit {
    pub fn span(self) -> Span {
        Span { start: self.start as u32, end: self.end as u32 }
    }
}

/// A keyword pattern's anchoring, from the Discord-compatible grammar:
/// bare = token-anchored (the Scunthorpe guard), `*w` / `w*` / `*w*` relax an
/// edge. `\*` is a literal asterisk.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Keyword {
    needle: String,
    open_left: bool,
    open_right: bool,
}

fn parse_keyword(pattern: &str) -> Keyword {
    let mut s = pattern;
    let mut open_left = false;
    let mut open_right = false;
    if let Some(rest) = s.strip_prefix('*') {
        open_left = true;
        s = rest;
    }
    if s.ends_with('*') && !s.ends_with("\\*") {
        open_right = true;
        s = &s[..s.len() - 1];
    }
    Keyword { needle: s.replace("\\*", "*"), open_left, open_right }
}

/// Find every non-overlapping hit for a set of keyword patterns in already
/// normalized text. Patterns are sorted bytewise first; at one start position
/// the lowest sorted index wins.
pub fn keyword_hits(text: &str, patterns: &[String]) -> Vec<Hit> {
    let mut kws: Vec<Keyword> = patterns.iter().map(|p| parse_keyword(p)).filter(|k| !k.needle.is_empty()).collect();
    kws.sort_by(|a, b| a.needle.cmp(&b.needle));
    let bytes = text.as_bytes();
    let mut hits: Vec<Hit> = Vec::new();
    let mut cursor = 0usize;
    while cursor < text.len() {
        let mut best: Option<Hit> = None;
        for k in &kws {
            let mut from = cursor;
            while let Some(rel) = text[from..].find(&k.needle) {
                let start = from + rel;
                let end = start + k.needle.len();
                let left_ok = k.open_left || start == 0 || !is_token_char(prev_char(text, start));
                let right_ok = k.open_right || end == text.len() || !is_token_char(next_char(text, end));
                if left_ok && right_ok {
                    // Leftmost wins; at equal starts the longer span, then the
                    // lower sorted index (already the iteration order).
                    let candidate = Hit { start, end };
                    best = match best {
                        Some(b) if (b.start, std::cmp::Reverse(b.end)) <= (candidate.start, std::cmp::Reverse(candidate.end)) => Some(b),
                        _ => Some(candidate),
                    };
                    break;
                }
                let step = next_char_boundary(bytes, start);
                if step <= from {
                    break;
                }
                from = step;
            }
        }
        match best {
            Some(h) => {
                cursor = h.end.max(next_char_boundary(bytes, h.start));
                hits.push(h);
            }
            None => break,
        }
    }
    hits.sort();
    hits
}

fn prev_char(text: &str, at: usize) -> char {
    text[..at].chars().next_back().unwrap_or(' ')
}

fn next_char(text: &str, at: usize) -> char {
    text[at..].chars().next().unwrap_or(' ')
}

fn next_char_boundary(bytes: &[u8], from: usize) -> usize {
    let mut i = from + 1;
    while i < bytes.len() && (bytes[i] & 0xC0) == 0x80 {
        i += 1;
    }
    i
}

/// Cancel hits an exemption covers: a hit is cancelled when an exempt match
/// CONTAINS or EQUALS its span. Exemptions match under the citing rule's
/// normalizer, so both live in one coordinate space.
pub fn cancel_exempt_hits(text: &str, hits: Vec<Hit>, exempts: &[&ExemptPatterns]) -> Vec<Hit> {
    let mut covers: Vec<Hit> = Vec::new();
    for e in exempts {
        match e.kind {
            Some(ExemptKind::Literal) | None => {
                for v in &e.values {
                    let mut from = 0usize;
                    while let Some(rel) = text[from..].find(v.as_str()) {
                        let start = from + rel;
                        covers.push(Hit { start, end: start + v.len() });
                        from = start + v.len().max(1);
                    }
                }
            }
            Some(ExemptKind::Wildcard) => {
                for v in &e.values {
                    covers.extend(keyword_hits(text, std::slice::from_ref(v)));
                }
            }
            // Domain exemptions belong to `link`, which matches in the raw
            // registrable-domain space, not here.
            Some(ExemptKind::Domain) => {}
        }
    }
    hits.into_iter().filter(|h| !covers.iter().any(|c| c.start <= h.start && c.end >= h.end)).collect()
}

// ── Links (§7.4 extraction grammar; raw registrable-domain space) ────────────

/// Extract the registrable domains a message links to. Absolute URLs need an
/// explicit http(s) scheme; a bare host must look like `label(.label)+`.
/// Trailing punctuation is stripped, userinfo resolves as host (not path), and
/// IDN maps to punycode BEFORE anchoring.
pub fn extract_domains(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| c.is_whitespace() || c == '<' || c == '>' || c == '"' || c == '(' || c == ')') {
        let token = raw.trim_end_matches(|c| ".,;:!?)]}'\"".contains(c));
        if token.is_empty() {
            continue;
        }
        let lowered = token.to_lowercase();
        let after_scheme = match lowered.split_once("://") {
            Some((scheme, rest)) => {
                if scheme != "http" && scheme != "https" {
                    continue;
                }
                rest
            }
            None => lowered.as_str(),
        };
        // Host ends at the first path/query/fragment separator; userinfo before
        // '@' is discarded so `evil.com/x@good.io` cannot masquerade.
        let hostport = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
        let host = hostport.rsplit_once('@').map(|(_, h)| h).unwrap_or(hostport);
        let host = host.split(':').next().unwrap_or("").trim_end_matches('.');
        if host.is_empty() || !host.contains('.') {
            continue;
        }
        if !host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
            continue;
        }
        let d = registrable_domain(host);
        if !d.is_empty() && !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

/// The registrable domain, anchored so `vectorapp.io.evil.com` and
/// `evil.com/vectorapp.io` can never pass an allowlist.
///
/// Bundle v1 ships a small public-suffix set: the full PSL snapshot is a
/// ratification artifact, so a multi-label suffix outside this set falls back
/// to the last two labels. That is the one approximation in the matcher, and it
/// is conservative — it never merges two distinct registrable domains.
fn registrable_domain(host: &str) -> String {
    const MULTI: &[&str] = &[
        "co.uk", "org.uk", "ac.uk", "gov.uk", "co.jp", "or.jp", "ne.jp", "com.au", "net.au", "org.au", "co.nz",
        "com.br", "com.cn", "com.mx", "co.za", "co.in", "co.kr", "github.io", "gitlab.io", "pages.dev",
        "workers.dev", "vercel.app", "netlify.app", "herokuapp.com", "s3.amazonaws.com",
    ];
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return host.to_string();
    }
    let last2 = labels[labels.len() - 2..].join(".");
    if MULTI.contains(&last2.as_str()) && labels.len() >= 3 {
        return labels[labels.len() - 3..].join(".");
    }
    last2
}

/// Non-exempt domains in a message: the `link` rule's hits.
pub fn link_hits(text: &str, patterns: &[String], exempts: &[&ExemptPatterns]) -> Vec<String> {
    let allow: Vec<String> = exempts
        .iter()
        .filter(|e| matches!(e.kind, Some(ExemptKind::Domain) | Some(ExemptKind::Literal) | None))
        .flat_map(|e| e.values.iter().map(|v| registrable_domain(&v.to_lowercase())))
        .collect();
    extract_domains(text)
        .into_iter()
        .filter(|d| {
            // Cancellation is by EQUALITY on the hit's own value: these hits
            // carry no span, so substring containment has no meaning here.
            if allow.iter().any(|a| a == d) {
                return false;
            }
            patterns.is_empty() || patterns.iter().any(|p| registrable_domain(&p.to_lowercase()) == *d)
        })
        .collect()
}

/// How many distinct people a message named.
///
/// Vector has no mention TAG: a mention is an npub written in the body, which the
/// renderer turns into a pill. The only `p` tags a community message carries are
/// its reply parent and reaction target, so counting those scored 1 on an
/// ordinary reply and 0 on a message that named fifteen people.
///
/// Grammar comes from [`crate::types::extract_mentions`] — the same scanner the
/// rest of the app already trusts, rather than a second opinion on what an npub
/// looks like. Distinct people, not occurrences: naming someone three times is
/// still one person called, and a rule about mass-pinging cares how many were
/// pulled in.
pub fn count_mentions(text: &str) -> u32 {
    let mut seen: Vec<&str> = Vec::new();
    for npub in crate::types::extract_mentions(text) {
        if seen.contains(&npub) {
            continue;
        }
        // One npub can appear twice: linked once and called once. Being linked
        // anywhere does not cancel being called somewhere else.
        if text.match_indices(npub).any(|(at, _)| !is_linked(text, at)) {
            seen.push(npub);
        }
    }
    seen.len() as u32
}

/// Whether the npub at `at` is part of a URL rather than someone being called.
///
/// `extract_mentions` rejects only an ALPHANUMERIC neighbour, which leaves
/// `…/profile/npub1…` looking like a mention. Linking to a profile is not
/// pinging its owner, so the policy engine applies the frontend's wider guard
/// here rather than tightening a shared helper other callers rely on.
fn is_linked(text: &str, at: usize) -> bool {
    let bytes = text.as_bytes();
    let mut lead = at;
    if lead >= 1 && bytes[lead - 1] == b'@' {
        lead -= 1;
    } else if lead >= 6 && text.get(lead - 6..lead).is_some_and(|p| p.eq_ignore_ascii_case("nostr:")) {
        lead -= 6;
    }
    if lead == 0 {
        return false;
    }
    let prev = text[..lead].chars().next_back().unwrap_or(' ');
    matches!(prev, '/' | '=' | '?' | '&' | '#' | '%' | '.' | '-' | '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pats(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bare_patterns_are_token_anchored() {
        // The Scunthorpe guard: a bare word never matches inside another.
        assert_eq!(keyword_hits("classic assassin", &pats(&["ass"])).len(), 0);
        assert_eq!(keyword_hits("what an ass", &pats(&["ass"])).len(), 1);
        // Separators bound a token; digits continue it.
        assert_eq!(keyword_hits("beta. beta_x", &pats(&["beta"])).len(), 2);
        assert_eq!(keyword_hits("beta2", &pats(&["beta"])).len(), 0);
    }

    #[test]
    fn wildcards_relax_the_edges() {
        assert_eq!(keyword_hits("scammer", &pats(&["scam*"])).len(), 1);
        assert_eq!(keyword_hits("scammer", &pats(&["*scam"])).len(), 0, "left-open still anchors the right");
        assert_eq!(keyword_hits("descammer", &pats(&["*scam*"])).len(), 1);
        // An escaped asterisk is a literal.
        assert_eq!(keyword_hits("buy * now", &pats(&["\\*"])).len(), 1);
    }

    #[test]
    fn one_span_is_one_hit_regardless_of_pattern_count() {
        let hits = keyword_hits("free airdrop free airdrop", &pats(&["free", "airdrop"]));
        assert_eq!(hits.len(), 4, "four distinct spans");
        // Overlapping patterns over one span collapse to a single hit.
        let hits = keyword_hits("*scam*", &pats(&["*scam*", "*sca*", "*cam*"]));
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn declaration_order_never_decides_a_span() {
        let a = keyword_hits("free airdrop", &pats(&["free", "airdrop"]));
        let b = keyword_hits("free airdrop", &pats(&["airdrop", "free"]));
        assert_eq!(a, b);
    }

    #[test]
    fn exemptions_cancel_by_containment() {
        let text = "join the free airdrop channel";
        let hits = keyword_hits(text, &pats(&["airdrop"]));
        assert_eq!(hits.len(), 1);
        let ex = ExemptPatterns { kind: Some(ExemptKind::Literal), values: pats(&["free airdrop"]) };
        assert!(cancel_exempt_hits(text, hits, &[&ex]).is_empty(), "an exempt phrase covers the hit inside it");
    }

    #[test]
    fn domains_anchor_on_the_registrable_name() {
        assert_eq!(extract_domains("see https://vectorapp.io/x"), vec!["vectorapp.io"]);
        // The oldest allowlist bypasses.
        assert_eq!(extract_domains("https://vectorapp.io.evil.com/a"), vec!["evil.com"]);
        assert_eq!(extract_domains("https://evil.com/vectorapp.io"), vec!["evil.com"]);
        // Userinfo resolves as host, not path.
        assert_eq!(extract_domains("https://vectorapp.io@evil.com/"), vec!["evil.com"]);
        // Trailing punctuation and ports fall away; a bare host still counts.
        assert_eq!(extract_domains("go to bit.ly:8080, now"), vec!["bit.ly"]);
        // Multi-label suffixes keep their registrable label.
        assert_eq!(extract_domains("https://foo.github.io/p"), vec!["foo.github.io"]);
        assert_eq!(extract_domains("https://a.b.co.uk/p"), vec!["b.co.uk"]);
        // Non-http schemes and bare words are not links.
        assert!(extract_domains("ftp://x.com nothing here").is_empty());
    }

    #[test]
    fn link_allowlists_cancel_by_equality_only() {
        let allow = ExemptPatterns { kind: Some(ExemptKind::Domain), values: pats(&["vectorapp.io"]) };
        assert!(link_hits("https://vectorapp.io/a", &[], &[&allow]).is_empty());
        // Substring must never cancel: app.io does not exempt myapp.io.
        let near = ExemptPatterns { kind: Some(ExemptKind::Domain), values: pats(&["app.io"]) };
        assert_eq!(link_hits("https://myapp.io/a", &[], &[&near]), vec!["myapp.io"]);
        // Distinct domains per message, deduped.
        let hits = link_hits("a bit.ly/x and bit.ly/y and tr.ee/z", &[], &[]);
        assert_eq!(hits, vec!["bit.ly", "tr.ee"]);
    }

    #[test]
    fn a_denylist_matches_only_its_own_domains() {
        let deny = pats(&["bit.ly", "tr.ee"]);
        assert_eq!(link_hits("see bit.ly/x", &deny, &[]), vec!["bit.ly"]);
        assert!(link_hits("see github.com/x", &deny, &[]).is_empty());
    }

    /// Built, not typed: a hand-counted 58-char literal is a test that fails for
    /// the wrong reason.
    fn npub_of(c: char) -> String {
        format!("npub1{}", std::iter::repeat(c).take(58).collect::<String>())
    }

    #[test]
    fn a_mention_is_an_npub_in_the_body_however_it_is_written() {
        let a = npub_of('q');
        assert_eq!(count_mentions(&format!("hey @{a} look")), 1);
        assert_eq!(count_mentions(&format!("hey nostr:{a} look")), 1);
        assert_eq!(count_mentions(&format!("hey {a} look")), 1);
    }

    #[test]
    fn naming_one_person_twice_is_still_one_person() {
        let a = npub_of('q');
        let b = npub_of('z');
        assert_eq!(count_mentions(&format!("@{a} and again @{a}")), 1);
        assert_eq!(count_mentions(&format!("@{a} and @{b}")), 2);
    }

    /// The shape the rule exists for.
    #[test]
    fn a_mass_ping_counts_everyone_it_named() {
        let a = npub_of('q');
        let b = npub_of('z');
        assert_eq!(count_mentions(&format!("@{a} @{b} @{a} ping")), 2);
    }

    /// A profile URL is not someone calling your name, and neither is an npub
    /// welded to a word.
    #[test]
    fn an_npub_inside_a_url_or_a_word_is_not_a_mention() {
        let a = npub_of('q');
        assert_eq!(count_mentions(&format!("https://vectorapp.io/profile/{a}")), 0);
        assert_eq!(count_mentions(&format!("x{a}")), 0);
        assert_eq!(count_mentions(&format!("?p={a}")), 0);
    }

    #[test]
    fn a_malformed_npub_is_not_a_mention() {
        assert_eq!(count_mentions("npub1short"), 0);
        // 59 data chars: a longer string that merely starts like an npub.
        assert_eq!(count_mentions(&format!("{}q", npub_of('q'))), 0);
    }

    #[test]
    fn plain_text_names_nobody() {
        assert_eq!(count_mentions("just a normal message @everyone"), 0);
    }
}
