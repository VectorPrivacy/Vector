//! SIMD-accelerated HTML metadata extractor
//!
//! Single forward pass over HTML bytes to extract `<meta>`, `<link>`, `<title>`, and `<p>` tags.
//! Uses NEON (aarch64) / SSE2 (x86_64) to scan for `<` at 16 bytes/iteration,
//! then does lightweight scalar parsing of tag names and attributes.
//!
//! All public functions require valid UTF-8 input (as from `str::as_bytes()`).

use std::borrow::Cow;

/// Metadata extracted from an HTML `<head>` section.
///
/// Fields borrow from the input slice when no HTML entity decoding was needed (zero-copy).
pub struct HtmlMeta<'a> {
    pub og_title: Option<Cow<'a, str>>,
    pub og_description: Option<Cow<'a, str>>,
    pub og_image: Option<Cow<'a, str>>,
    pub og_url: Option<Cow<'a, str>>,
    pub og_type: Option<Cow<'a, str>>,
    pub title: Option<Cow<'a, str>>,
    pub description: Option<Cow<'a, str>>,
    pub favicons: Vec<FaviconCandidate<'a>>,
}

/// A favicon candidate extracted from a `<link>` tag.
pub struct FaviconCandidate<'a> {
    pub href: Cow<'a, str>,
    pub rel: Cow<'a, str>,
}

// ── SIMD: find '<' ──────────────────────────────────────────────────────────

/// Find the index of the first `<` byte using SIMD, starting from `start`.
/// Returns `bytes.len()` if not found.
#[inline]
fn find_lt(bytes: &[u8], start: usize) -> usize {
    let len = bytes.len();
    let mut i = start;

    #[cfg(target_arch = "aarch64")]
    unsafe {
        use std::arch::aarch64::*;
        let lt = vdupq_n_u8(b'<');
        while i + 16 <= len {
            let chunk = vld1q_u8(bytes.as_ptr().add(i));
            let hits = vceqq_u8(chunk, lt);
            if vmaxvq_u8(hits) != 0 {
                for j in 0..16 {
                    if bytes[i + j] == b'<' {
                        return i + j;
                    }
                }
            }
            i += 16;
        }
    }

    #[cfg(target_arch = "x86_64")]
    unsafe {
        use std::arch::x86_64::*;
        let lt = _mm_set1_epi8(b'<' as i8);
        while i + 16 <= len {
            let chunk = _mm_loadu_si128(bytes.as_ptr().add(i) as *const __m128i);
            let hits = _mm_cmpeq_epi8(chunk, lt);
            let mask = _mm_movemask_epi8(hits);
            if mask != 0 {
                return i + mask.trailing_zeros() as usize;
            }
            i += 16;
        }
    }

    // Scalar remainder
    while i < len {
        if bytes[i] == b'<' {
            return i;
        }
        i += 1;
    }
    len
}

/// Find `</head>` (case-insensitive) using SIMD to locate `<`, then scalar verify.
/// Returns the byte position **after** the closing `>`, or `None` if not found.
///
/// Designed for incremental chunk scanning — pass `start` as the overlap offset.
pub fn find_closing_head(bytes: &[u8], start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut pos = start;
    loop {
        pos = find_lt(bytes, pos);
        if pos + 6 >= len {
            return None;
        }
        if bytes[pos + 1] == b'/'
            && (bytes[pos + 2] | 0x20) == b'h'
            && (bytes[pos + 3] | 0x20) == b'e'
            && (bytes[pos + 4] | 0x20) == b'a'
            && (bytes[pos + 5] | 0x20) == b'd'
            && bytes[pos + 6] == b'>'
        {
            return Some(pos + 7);
        }
        pos += 1;
    }
}

// ── Scalar helpers ──────────────────────────────────────────────────────────

/// Read a tag name starting at `pos` (the byte after `<`).
/// Returns (tag_name_slice, position_after_tag_name).
/// Handles `</tagname` by including the leading `/`.
#[inline]
fn read_tag_name(bytes: &[u8], pos: usize) -> (&[u8], usize) {
    let start = pos;
    let len = bytes.len();
    let mut i = pos;
    // Allow leading '/' for closing tags like </head>
    if i < len && bytes[i] == b'/' {
        i += 1;
    }
    while i < len {
        let b = bytes[i];
        if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' || b == b'>' || b == b'/' {
            break;
        }
        i += 1;
    }
    (&bytes[start..i], i)
}

/// Case-insensitive comparison of a byte slice against an ASCII lowercase target.
#[inline]
fn eq_ignore_case(src: &[u8], target: &[u8]) -> bool {
    if src.len() != target.len() {
        return false;
    }
    for i in 0..src.len() {
        if (src[i] | 0x20) != target[i] {
            return false;
        }
    }
    true
}

/// Skip whitespace bytes.
#[inline]
fn skip_ws(bytes: &[u8], mut pos: usize) -> usize {
    let len = bytes.len();
    while pos < len {
        match bytes[pos] {
            b' ' | b'\t' | b'\n' | b'\r' => pos += 1,
            _ => break,
        }
    }
    pos
}

/// Skip to the byte past the next `>`.
#[inline]
fn skip_to_gt(bytes: &[u8], mut pos: usize) -> usize {
    let len = bytes.len();
    while pos < len && bytes[pos] != b'>' {
        pos += 1;
    }
    if pos < len {
        pos + 1
    } else {
        len
    }
}

// ── Zero-alloc callback-based attribute scanner ─────────────────────────────

/// Scan attributes from `start` until `>`, calling `on_attr(name, value)` for each.
/// Returns position after the closing `>`.
///
/// The callback receives raw byte slices borrowing from `bytes` — no heap allocation.
#[inline]
fn scan_attrs<'a, F>(bytes: &'a [u8], start: usize, mut on_attr: F) -> usize
where
    F: FnMut(&'a [u8], &'a [u8]),
{
    let len = bytes.len();
    let mut pos = start;

    loop {
        pos = skip_ws(bytes, pos);
        if pos >= len {
            break;
        }
        let b = bytes[pos];
        if b == b'>' {
            pos += 1;
            break;
        }
        if b == b'/' {
            pos += 1;
            if pos < len && bytes[pos] == b'>' {
                pos += 1;
            }
            break;
        }

        // Read attribute name
        let name_start = pos;
        while pos < len && bytes[pos] != b'=' && bytes[pos] != b' ' && bytes[pos] != b'\t'
            && bytes[pos] != b'\n' && bytes[pos] != b'\r' && bytes[pos] != b'>' && bytes[pos] != b'/'
        {
            pos += 1;
        }
        let name = &bytes[name_start..pos];

        pos = skip_ws(bytes, pos);
        if pos >= len || bytes[pos] != b'=' {
            // Valueless attribute (e.g. `disabled`) — skip
            continue;
        }
        pos += 1; // skip '='
        pos = skip_ws(bytes, pos);
        if pos >= len {
            break;
        }

        // Read attribute value
        let quote = bytes[pos];
        let value;
        if quote == b'"' || quote == b'\'' {
            pos += 1;
            let val_start = pos;
            while pos < len && bytes[pos] != quote {
                pos += 1;
            }
            value = &bytes[val_start..pos];
            if pos < len {
                pos += 1;
            }
        } else {
            // Unquoted value — read until whitespace or >
            let val_start = pos;
            while pos < len && bytes[pos] != b' ' && bytes[pos] != b'\t'
                && bytes[pos] != b'\n' && bytes[pos] != b'\r' && bytes[pos] != b'>'
            {
                pos += 1;
            }
            value = &bytes[val_start..pos];
        }

        on_attr(name, value);
    }

    pos
}

// ── Entity decoder (returns Cow — borrows when no entities) ─────────────────

/// Decode common HTML entities. Returns `Cow::Borrowed` when no `&` is present (zero-copy).
fn decode_entities(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if bytes[i] == b'&' {
            if i + 3 < len && bytes[i + 1] == b'l' && bytes[i + 2] == b't' && bytes[i + 3] == b';' {
                result.push('<');
                i += 4;
            } else if i + 3 < len && bytes[i + 1] == b'g' && bytes[i + 2] == b't' && bytes[i + 3] == b';' {
                result.push('>');
                i += 4;
            } else if i + 4 < len && bytes[i + 1] == b'a' && bytes[i + 2] == b'm' && bytes[i + 3] == b'p' && bytes[i + 4] == b';' {
                result.push('&');
                i += 5;
            } else if i + 5 < len && bytes[i + 1] == b'q' && bytes[i + 2] == b'u' && bytes[i + 3] == b'o' && bytes[i + 4] == b't' && bytes[i + 5] == b';' {
                result.push('"');
                i += 6;
            } else if i + 5 < len && bytes[i + 1] == b'a' && bytes[i + 2] == b'p' && bytes[i + 3] == b'o' && bytes[i + 4] == b's' && bytes[i + 5] == b';' {
                result.push('\'');
                i += 6;
            } else if i + 4 < len && bytes[i + 1] == b'#' && bytes[i + 2] == b'3' && bytes[i + 3] == b'9' && bytes[i + 4] == b';' {
                result.push('\'');
                i += 5;
            } else if i + 5 < len && bytes[i + 1] == b'#' && bytes[i + 2] == b'x' {
                // &#xNN;
                if let Some(semi) = bytes[i + 3..len].iter().position(|&b| b == b';') {
                    let hex = &s[i + 3..i + 3 + semi];
                    if let Ok(code) = u32::from_str_radix(hex, 16) {
                        if let Some(ch) = char::from_u32(code) {
                            result.push(ch);
                            i += 4 + semi;
                            continue;
                        }
                    }
                }
                result.push('&');
                i += 1;
            } else if i + 3 < len && bytes[i + 1] == b'#' && bytes[i + 2].is_ascii_digit() {
                // &#NN;
                if let Some(semi) = bytes[i + 2..len].iter().position(|&b| b == b';') {
                    let num_str = &s[i + 2..i + 2 + semi];
                    if let Ok(code) = num_str.parse::<u32>() {
                        if let Some(ch) = char::from_u32(code) {
                            result.push(ch);
                            i += 3 + semi;
                            continue;
                        }
                    }
                }
                result.push('&');
                i += 1;
            } else if i + 5 < len && bytes[i + 1] == b'n' && bytes[i + 2] == b'b' && bytes[i + 3] == b's' && bytes[i + 4] == b'p' && bytes[i + 5] == b';' {
                result.push('\u{00A0}');
                i += 6;
            } else {
                result.push('&');
                i += 1;
            }
        } else {
            // Fast path: copy run of non-entity bytes as a single slice
            let start = i;
            i += 1;
            while i < len && bytes[i] != b'&' {
                i += 1;
            }
            result.push_str(&s[start..i]);
        }
    }
    Cow::Owned(result)
}

/// Convert a byte sub-slice to `&str` without redundant UTF-8 re-validation.
///
/// SAFETY: the input to `extract_html_meta` / `extract_first_p_inner_html` must be valid
/// UTF-8 (guaranteed by callers which pass `String::as_bytes()`). Any sub-slice of valid
/// UTF-8 is itself valid UTF-8.
#[inline]
unsafe fn as_str_unchecked(bytes: &[u8]) -> &str {
    std::str::from_utf8_unchecked(bytes)
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Extract metadata from an HTML byte slice (expects `<head>` content, <=32KB).
///
/// Input **must** be valid UTF-8 (e.g. from `str::as_bytes()`).
pub fn extract_html_meta(html: &[u8]) -> HtmlMeta<'_> {
    let mut meta = HtmlMeta {
        og_title: None,
        og_description: None,
        og_image: None,
        og_url: None,
        og_type: None,
        title: None,
        description: None,
        favicons: Vec::new(),
    };

    let len = html.len();
    let mut pos = 0;

    loop {
        pos = find_lt(html, pos);
        if pos >= len {
            break;
        }
        pos += 1; // skip '<'
        if pos >= len {
            break;
        }

        // Skip HTML comments <!-- ... -->
        if pos + 2 < len && html[pos] == b'!' && html[pos + 1] == b'-' && html[pos + 2] == b'-' {
            pos += 3;
            loop {
                if pos + 2 >= len {
                    pos = len;
                    break;
                }
                if html[pos] == b'-' && html[pos + 1] == b'-' && html[pos + 2] == b'>' {
                    pos += 3;
                    break;
                }
                pos += 1;
            }
            continue;
        }

        let (tag_name, after_name) = read_tag_name(html, pos);
        if tag_name.is_empty() {
            continue;
        }

        // First-byte discriminant: avoids full comparison against every known tag
        match tag_name[0] | 0x20 {
            b'm' if eq_ignore_case(tag_name, b"meta") => {
                // ── <meta> — inline attr scan, zero Vec allocation ──
                let mut property: Option<&[u8]> = None;
                let mut name: Option<&[u8]> = None;
                let mut content: Option<&[u8]> = None;

                pos = scan_attrs(html, after_name, |k, v| {
                    if eq_ignore_case(k, b"property") {
                        property = Some(v);
                    } else if eq_ignore_case(k, b"name") {
                        name = Some(v);
                    } else if eq_ignore_case(k, b"content") {
                        content = Some(v);
                    }
                });

                let content = match content {
                    Some(c) => c,
                    None => continue,
                };

                // Check property (OG tags) — only convert to str if we match
                if let Some(prop) = property {
                    if prop.len() > 3 && (prop[0] | 0x20) == b'o' && (prop[1] | 0x20) == b'g' && prop[2] == b':' {
                        // SAFETY: caller guarantees valid UTF-8 input
                        let s = unsafe { as_str_unchecked(content) };
                        match prop[3] | 0x20 {
                            b't' if eq_ignore_case(prop, b"og:title") => {
                                meta.og_title = Some(decode_entities(s));
                            }
                            b'd' if eq_ignore_case(prop, b"og:description") => {
                                meta.og_description = Some(decode_entities(s));
                            }
                            b'i' if eq_ignore_case(prop, b"og:image") => {
                                meta.og_image = Some(decode_entities(s));
                            }
                            b'u' if eq_ignore_case(prop, b"og:url") => {
                                meta.og_url = Some(decode_entities(s));
                            }
                            b't' if eq_ignore_case(prop, b"og:type") => {
                                meta.og_type = Some(decode_entities(s));
                            }
                            _ => {}
                        }
                    }
                }

                // Check name (standard meta + Twitter cards) — only convert if matched
                if let Some(n) = name {
                    if eq_ignore_case(n, b"description") {
                        let s = unsafe { as_str_unchecked(content) };
                        meta.description = Some(decode_entities(s));
                    } else if n.len() > 8 && (n[0] | 0x20) == b't' {
                        // twitter:* prefix check
                        if eq_ignore_case(n, b"twitter:title") && meta.og_title.is_none() {
                            let s = unsafe { as_str_unchecked(content) };
                            meta.og_title = Some(decode_entities(s));
                        } else if eq_ignore_case(n, b"twitter:description") && meta.og_description.is_none() {
                            let s = unsafe { as_str_unchecked(content) };
                            meta.og_description = Some(decode_entities(s));
                        } else if eq_ignore_case(n, b"twitter:image") && meta.og_image.is_none() {
                            let s = unsafe { as_str_unchecked(content) };
                            meta.og_image = Some(decode_entities(s));
                        }
                    }
                }
            }

            b'l' if eq_ignore_case(tag_name, b"link") => {
                // ── <link> — inline attr scan, zero Vec allocation ──
                let mut rel: Option<&[u8]> = None;
                let mut href: Option<&[u8]> = None;

                pos = scan_attrs(html, after_name, |k, v| {
                    if eq_ignore_case(k, b"rel") {
                        rel = Some(v);
                    } else if eq_ignore_case(k, b"href") {
                        href = Some(v);
                    }
                });

                let rel_bytes = match rel {
                    Some(r) => r,
                    None => continue,
                };
                let href_bytes = match href {
                    Some(h) => h,
                    None => continue,
                };

                // Case-insensitive check against known favicon rel values — no alloc
                if eq_ignore_case(rel_bytes, b"icon")
                    || eq_ignore_case(rel_bytes, b"shortcut icon")
                    || eq_ignore_case(rel_bytes, b"apple-touch-icon")
                {
                    // SAFETY: valid UTF-8 sub-slices
                    let href_str = unsafe { as_str_unchecked(href_bytes) };
                    let rel_str = unsafe { as_str_unchecked(rel_bytes) };
                    meta.favicons.push(FaviconCandidate {
                        href: decode_entities(href_str),
                        rel: Cow::Borrowed(rel_str),
                    });
                }
            }

            b't' if eq_ignore_case(tag_name, b"title") => {
                // ── <title> — SIMD scan to closing </title> ──
                let p = skip_to_gt(html, after_name);
                let text_start = p;
                let close = find_lt(html, p);
                if close <= len {
                    // SAFETY: valid UTF-8 sub-slice
                    let text = unsafe { as_str_unchecked(&html[text_start..close]) };
                    meta.title = Some(decode_entities(text.trim()));
                }
                pos = close;
            }

            b'/' => {
                // Closing tag — check for </head> to stop early
                if tag_name.len() == 5 && eq_ignore_case(&tag_name[1..], b"head") {
                    break;
                }
                pos = skip_to_gt(html, after_name);
            }

            _ => {
                // Unknown tag — skip to '>'
                pos = skip_to_gt(html, after_name);
            }
        }
    }

    meta
}

/// Extract the first `<p>` tag's inner HTML (for Twitter oEmbed snippets).
///
/// Input **must** be valid UTF-8. Returns `Cow::Borrowed` when no entities need decoding.
pub fn extract_first_p_inner_html(html: &[u8]) -> Option<Cow<'_, str>> {
    let len = html.len();
    let mut pos = 0;

    loop {
        pos = find_lt(html, pos);
        if pos >= len {
            return None;
        }
        pos += 1; // skip '<'
        if pos >= len {
            return None;
        }

        let (tag_name, after_name) = read_tag_name(html, pos);

        if tag_name.len() == 1 && (tag_name[0] | 0x20) == b'p' {
            let p = skip_to_gt(html, after_name);

            // Collect everything until </p>
            let content_start = p;
            let mut scan = p;
            loop {
                let lt = find_lt(html, scan);
                if lt >= len {
                    // No closing tag — return what we have
                    // SAFETY: valid UTF-8 sub-slice
                    let s = unsafe { as_str_unchecked(&html[content_start..len]) };
                    return Some(decode_entities(s));
                }
                // Check for </p>
                if lt + 3 < len
                    && html[lt + 1] == b'/'
                    && (html[lt + 2] | 0x20) == b'p'
                    && (html[lt + 3] == b'>' || html[lt + 3] == b' ')
                {
                    // SAFETY: valid UTF-8 sub-slice
                    let s = unsafe { as_str_unchecked(&html[content_start..lt]) };
                    return Some(decode_entities(s));
                }
                scan = lt + 1;
            }
        } else {
            pos = skip_to_gt(html, after_name);
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_og_tags() {
        let html = br#"<html><head>
            <meta property="og:title" content="Test Title">
            <meta property="og:description" content="Test Description">
            <meta property="og:image" content="https://example.com/image.png">
            <meta property="og:url" content="https://example.com">
            <meta property="og:type" content="website">
            <title>Page Title</title>
        </head></html>"#;

        let meta = extract_html_meta(html);
        assert_eq!(meta.og_title.as_deref(), Some("Test Title"));
        assert_eq!(meta.og_description.as_deref(), Some("Test Description"));
        assert_eq!(meta.og_image.as_deref(), Some("https://example.com/image.png"));
        assert_eq!(meta.og_url.as_deref(), Some("https://example.com"));
        assert_eq!(meta.og_type.as_deref(), Some("website"));
        assert_eq!(meta.title.as_deref(), Some("Page Title"));
    }

    #[test]
    fn test_twitter_card_fallback() {
        let html = br#"<head>
            <meta name="twitter:title" content="Tweet Title">
            <meta name="twitter:description" content="Tweet Desc">
            <meta name="twitter:image" content="https://img.twitter.com/pic.jpg">
            <meta name="description" content="Standard desc">
        </head>"#;

        let meta = extract_html_meta(html);
        assert_eq!(meta.og_title.as_deref(), Some("Tweet Title"));
        assert_eq!(meta.og_description.as_deref(), Some("Tweet Desc"));
        assert_eq!(meta.og_image.as_deref(), Some("https://img.twitter.com/pic.jpg"));
        assert_eq!(meta.description.as_deref(), Some("Standard desc"));
    }

    #[test]
    fn test_og_takes_priority_over_twitter() {
        let html = br#"<head>
            <meta property="og:title" content="OG Title">
            <meta name="twitter:title" content="Twitter Title">
        </head>"#;

        let meta = extract_html_meta(html);
        assert_eq!(meta.og_title.as_deref(), Some("OG Title"));
    }

    #[test]
    fn test_favicons() {
        let html = br#"<head>
            <link rel="icon" href="/favicon.ico">
            <link rel="apple-touch-icon" href="/apple-icon.png">
            <link rel="shortcut icon" href="/shortcut.ico">
            <link rel="stylesheet" href="/style.css">
        </head>"#;

        let meta = extract_html_meta(html);
        assert_eq!(meta.favicons.len(), 3);
        assert_eq!(meta.favicons[0].href.as_ref(), "/favicon.ico");
        assert_eq!(meta.favicons[0].rel.as_ref(), "icon");
        assert_eq!(meta.favicons[1].href.as_ref(), "/apple-icon.png");
        assert_eq!(meta.favicons[1].rel.as_ref(), "apple-touch-icon");
        assert_eq!(meta.favicons[2].href.as_ref(), "/shortcut.ico");
        assert_eq!(meta.favicons[2].rel.as_ref(), "shortcut icon");
    }

    #[test]
    fn test_self_closing_meta() {
        let html = br#"<head>
            <meta property="og:title" content="Self Close" />
            <meta name="description" content="Also self close"/>
        </head>"#;

        let meta = extract_html_meta(html);
        assert_eq!(meta.og_title.as_deref(), Some("Self Close"));
        assert_eq!(meta.description.as_deref(), Some("Also self close"));
    }

    #[test]
    fn test_html_entities() {
        let html = br#"<head>
            <meta property="og:title" content="Tom &amp; Jerry &lt;3">
            <title>A &quot;quoted&quot; title</title>
        </head>"#;

        let meta = extract_html_meta(html);
        assert_eq!(meta.og_title.as_deref(), Some("Tom & Jerry <3"));
        assert_eq!(meta.title.as_deref(), Some("A \"quoted\" title"));
    }

    #[test]
    fn test_single_quoted_attributes() {
        let html = br#"<head>
            <meta property='og:title' content='Single Quotes'>
        </head>"#;

        let meta = extract_html_meta(html);
        assert_eq!(meta.og_title.as_deref(), Some("Single Quotes"));
    }

    #[test]
    fn test_case_insensitive_tags() {
        let html = br#"<HEAD>
            <META PROPERTY="og:title" CONTENT="Upper Case">
            <TITLE>Upper Title</TITLE>
        </HEAD>"#;

        let meta = extract_html_meta(html);
        assert_eq!(meta.og_title.as_deref(), Some("Upper Case"));
        assert_eq!(meta.title.as_deref(), Some("Upper Title"));
    }

    #[test]
    fn test_stops_at_head_close() {
        let html = br#"<head>
            <meta property="og:title" content="In Head">
        </head>
        <body>
            <meta property="og:description" content="In Body">
        </body>"#;

        let meta = extract_html_meta(html);
        assert_eq!(meta.og_title.as_deref(), Some("In Head"));
        assert!(meta.og_description.is_none());
    }

    #[test]
    fn test_extract_first_p() {
        let html = br#"<blockquote><p>Hello world<br>Second line<a href="/">link</a></p></blockquote>"#;
        let p = extract_first_p_inner_html(html);
        assert_eq!(p.as_deref(), Some("Hello world<br>Second line<a href=\"/\">link</a>"));
    }

    #[test]
    fn test_extract_p_with_entities() {
        let html = br#"<p>Tom &amp; Jerry</p>"#;
        let p = extract_first_p_inner_html(html);
        assert_eq!(p.as_deref(), Some("Tom & Jerry"));
    }

    #[test]
    fn test_empty_html() {
        let meta = extract_html_meta(b"");
        assert!(meta.og_title.is_none());
        assert!(meta.title.is_none());
        assert!(meta.favicons.is_empty());

        let p = extract_first_p_inner_html(b"");
        assert!(p.is_none());
    }

    #[test]
    fn test_html_comment_skipped() {
        let html = br#"<head>
            <!-- <meta property="og:title" content="Commented Out"> -->
            <meta property="og:title" content="Real Title">
        </head>"#;

        let meta = extract_html_meta(html);
        assert_eq!(meta.og_title.as_deref(), Some("Real Title"));
    }

    #[test]
    fn test_find_lt_simd() {
        let data = b"0123456789abcdef<tag>";
        assert_eq!(find_lt(data, 0), 16);

        let data2 = b"<immediate>";
        assert_eq!(find_lt(data2, 0), 0);

        let data3 = b"no tags here at all";
        assert_eq!(find_lt(data3, 0), data3.len());
    }

    #[test]
    fn test_numeric_entity() {
        let html = br#"<head><title>Test&#39;s &amp; &#x27;stuff&#x27;</title></head>"#;
        let meta = extract_html_meta(html);
        assert_eq!(meta.title.as_deref(), Some("Test's & 'stuff'"));
    }

    #[test]
    fn test_zero_copy_no_entities() {
        // When no entities are present, Cow should be Borrowed (zero-alloc)
        let html = br#"<head><meta property="og:title" content="Plain Title"></head>"#;
        let meta = extract_html_meta(html);
        assert!(matches!(meta.og_title, Some(Cow::Borrowed(_))));

        let html2 = br#"<p>No entities here</p>"#;
        let p = extract_first_p_inner_html(html2);
        assert!(matches!(p, Some(Cow::Borrowed(_))));
    }

    #[test]
    fn test_owned_with_entities() {
        // When entities are present, Cow should be Owned (decoded)
        let html = br#"<head><meta property="og:title" content="A &amp; B"></head>"#;
        let meta = extract_html_meta(html);
        assert!(matches!(meta.og_title, Some(Cow::Owned(_))));
        assert_eq!(meta.og_title.as_deref(), Some("A & B"));
    }
}
