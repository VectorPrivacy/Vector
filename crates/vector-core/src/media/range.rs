//! One HTTP `Range` header against a known length, for every place that serves media.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// No usable range: the whole body, 200.
    Whole,
    /// Bytes `start..=end`, 206.
    Part { start: u64, end: u64 },
    /// A well-formed range that starts past the end, 416.
    Unsatisfiable,
}

impl ByteRange {
    /// The `Content-Range` value a 206 or 416 carries.
    pub fn content_range(&self, len: u64) -> Option<String> {
        match *self {
            ByteRange::Whole => None,
            ByteRange::Part { start, end } => Some(format!("bytes {start}-{end}/{len}")),
            ByteRange::Unsatisfiable => Some(format!("bytes */{len}")),
        }
    }
}

/// Only the first range of a list is served: media elements never ask for more, and one
/// part needs no multipart body. A header that doesn't parse is ignored, as RFC 9110 allows.
pub fn parse(header: Option<&str>, len: u64) -> ByteRange {
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else {
        return ByteRange::Whole;
    };
    let first = spec.split(',').next().unwrap_or_default().trim();
    let Some((from, to)) = first.split_once('-') else {
        return ByteRange::Whole;
    };
    let (from, to) = (from.trim(), to.trim());

    if from.is_empty() {
        let Some(n) = number(to) else { return ByteRange::Whole };
        return match n {
            0 => ByteRange::Unsatisfiable,
            _ if len == 0 => ByteRange::Unsatisfiable,
            n => ByteRange::Part { start: len.saturating_sub(n), end: len - 1 },
        };
    }

    let Some(start) = number(from) else { return ByteRange::Whole };
    let end = if to.is_empty() {
        u64::MAX
    } else {
        match number(to) {
            Some(end) if end >= start => end,
            _ => return ByteRange::Whole,
        }
    };
    if start >= len {
        return ByteRange::Unsatisfiable;
    }
    ByteRange::Part { start, end: end.min(len - 1) }
}

/// Decimal digits, saturating: a number too big for u64 still means "past any end".
fn number(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(s.bytes().fold(0u64, |n, b| n.saturating_mul(10).saturating_add(u64::from(b - b'0'))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ByteRange::*;

    #[test]
    fn it_reads_the_forms_media_elements_send() {
        assert_eq!(parse(Some("bytes=0-"), 100), Part { start: 0, end: 99 });
        assert_eq!(parse(Some("bytes=10-19"), 100), Part { start: 10, end: 19 });
        assert_eq!(parse(Some("bytes=90-500"), 100), Part { start: 90, end: 99 }, "an end past the body is clipped");
        assert_eq!(parse(Some("bytes=-10"), 100), Part { start: 90, end: 99 });
        assert_eq!(parse(Some("bytes=-500"), 100), Part { start: 0, end: 99 }, "a suffix longer than the body is all of it");
        assert_eq!(parse(Some(" bytes= 5 - 6 , 8-9"), 100), Part { start: 5, end: 6 }, "first range only");
    }

    #[test]
    fn a_range_past_the_end_is_unsatisfiable() {
        assert_eq!(parse(Some("bytes=100-"), 100), Unsatisfiable);
        assert_eq!(parse(Some("bytes=99999999999999999999999-"), 100), Unsatisfiable);
        assert_eq!(parse(Some("bytes=-0"), 100), Unsatisfiable);
        assert_eq!(parse(Some("bytes=0-"), 0), Unsatisfiable, "an empty body has no bytes to give");
        assert_eq!(parse(Some("bytes=-5"), 0), Unsatisfiable);
    }

    #[test]
    fn anything_malformed_means_the_whole_body() {
        for bad in ["", "bytes=", "bytes=-", "bytes=a-b", "bytes=5-3", "items=0-1", "bytes=1", "bytes=+1-2", "bytes=-x"] {
            assert_eq!(parse(Some(bad), 100), Whole, "{bad:?}");
        }
        assert_eq!(parse(None, 100), Whole);
    }

    #[test]
    fn it_writes_the_content_range_each_answer_needs() {
        assert_eq!(Part { start: 2, end: 4 }.content_range(10).as_deref(), Some("bytes 2-4/10"));
        assert_eq!(Unsatisfiable.content_range(10).as_deref(), Some("bytes */10"));
        assert_eq!(Whole.content_range(10), None);
    }
}
