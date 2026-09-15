//! What a Blossom server said when it refused — as data, not prose.
//!
//! Magnitude answers every refusal three ways at once: `X-Error-Code`, a
//! stable identifier that is a header so a BUD-06 `HEAD` preflight carries it
//! too; `X-Retry`, one of `never` / `later` / `elsewhere`; and a JSON body
//! `{error, message, retry, status, ...}` whose extra fields hold the numbers
//! behind the sentence (`limit`, `used`, `retry_after`, `mime`). A server that
//! speaks that vocabulary is routed on it. One that does not falls back to the
//! status-and-keyword heuristics in `blossom_capabilities`, which is all a
//! plain BUD-02 server gives us to go on.
//!
//! The difference matters most where the heuristics are wrong in a way only
//! the code can reveal: a quota refusal is a 413, and a 413 read as "the blob
//! is too big" would demote the server for every file of that size for days;
//! an expired signature is a 401, and a 401 read as "this server will never
//! take our uploads" would bury a good server under a clock skew.

use reqwest::header::HeaderMap;
use serde_json::{json, Map, Value};
use std::time::Duration;

/// A server's advice on what to do with a refused request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// The request is wrong. Sending it again changes nothing.
    Never,
    /// This server may accept it later.
    Later,
    /// This server will not accept it, but another might.
    Elsewhere,
}

impl Retry {
    fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "never" => Some(Retry::Never),
            "later" => Some(Retry::Later),
            "elsewhere" => Some(Retry::Elsewhere),
            _ => None,
        }
    }
}

/// A non-2xx answer to an upload, preflight, or mirror request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub status: u16,
    /// The server's stable identifier for this refusal, when it sent one.
    /// `None` means a server without the vocabulary, to be read by status.
    pub code: Option<String>,
    pub retry: Option<Retry>,
    /// For a person: the body's `message`, else `X-Reason`, else the raw body.
    pub message: String,
    /// The numbers behind the message: `limit`, `used`, `retry_after`, `mime`.
    pub details: Map<String, Value>,
}

/// A code is a short snake_case word; anything else in the header is noise.
fn valid_code(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 48
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl Refusal {
    /// Read a refusal out of a response. `body` may be empty (a `HEAD` has
    /// none), in which case the headers carry everything Magnitude sends.
    pub fn from_response(status: u16, headers: &HeaderMap, body: &str) -> Self {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        let mut json: Option<Map<String, Value>> = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|v| v.as_object().cloned());
        let json_str = |j: &Option<Map<String, Value>>, key: &str| {
            j.as_ref()
                .and_then(|m| m.get(key))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        };

        let code = header("x-error-code")
            .or_else(|| json_str(&json, "error"))
            .filter(|c| valid_code(c));
        let retry = header("x-retry")
            .as_deref()
            .and_then(Retry::parse)
            .or_else(|| json_str(&json, "retry").as_deref().and_then(Retry::parse));
        let message = json_str(&json, "message")
            .or_else(|| header("x-reason"))
            .or_else(|| {
                let trimmed = body.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            })
            .unwrap_or_else(|| format!("HTTP {}", status));

        let mut details = json
            .take()
            .map(|mut m| {
                for k in ["error", "message", "retry", "status"] {
                    m.remove(k);
                }
                m
            })
            .unwrap_or_default();
        if let Some(secs) = header("retry-after").and_then(|s| s.parse::<u64>().ok()) {
            details.entry("retry_after").or_insert(json!(secs));
        }

        Self { status, code, retry, message, details }
    }

    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    fn detail_u64(&self, key: &str) -> Option<u64> {
        self.details.get(key).and_then(|v| v.as_u64())
    }

    /// The per-file limit the server named, for a `blob_too_large`.
    pub fn limit(&self) -> Option<u64> {
        self.detail_u64("limit")
    }

    pub fn retry_after(&self) -> Option<Duration> {
        self.detail_u64("retry_after").map(Duration::from_secs)
    }

    /// The file is bigger than this server takes — a property of the file's
    /// size, so worth remembering. A quota refusal is deliberately NOT this:
    /// it shares the 413 but says nothing about size.
    pub fn is_size_limit(&self) -> bool {
        match self.code() {
            Some(c) => c == "blob_too_large",
            None => self.status == 413,
        }
    }

    /// This server does not take this kind of file — worth remembering.
    pub fn is_mime(&self) -> bool {
        match self.code() {
            Some(c) => c == "unsupported_media_type",
            None => crate::blossom_capabilities::is_mime_rejection(Some(self.status), &self.message),
        }
    }

    /// Whether sending the same request to the same server again, right now,
    /// has any chance. A coded server is believed; an uncoded one is retried
    /// unless the status is one that never changes on a retry.
    pub fn worth_retrying_here(&self) -> bool {
        match self.code() {
            // `later` covers tomorrow's quota as much as a 2 s rate limit, so
            // only the codes where "later" plausibly means "in a moment".
            Some("rate_limited") => self
                .retry_after()
                .is_none_or(|d| d <= MAX_RETRY_AFTER),
            Some("internal") | Some("mirror_source_unreachable") => true,
            Some(_) => false,
            None => {
                !self.is_mime()
                    && !self.is_size_limit()
                    && !is_gateway_status(self.status)
            }
        }
    }

    /// One sentence for the person waiting on the upload.
    pub fn describe(&self, host: &str) -> String {
        let bytes = crate::crypto::format_bytes;
        match self.code() {
            Some("quota_daily_exceeded") => match (self.detail_u64("used"), self.limit()) {
                (Some(used), Some(limit)) => format!(
                    "Today's {} upload allowance on {} is used up ({} uploaded so far). It resets at midnight UTC.",
                    bytes(limit), host, bytes(used),
                ),
                _ => format!("Today's upload allowance on {} is used up. It resets at midnight UTC.", host),
            },
            Some("quota_storage_exceeded") => match (self.detail_u64("used"), self.limit()) {
                (Some(used), Some(limit)) => format!(
                    "Your storage on {} is full ({} of {}). Delete some files there, or use another server.",
                    host, bytes(used), bytes(limit),
                ),
                _ => format!("Your storage on {} is full.", host),
            },
            Some("blob_too_large") => match self.limit() {
                Some(limit) => format!("{} allows files up to {} for your account.", host, bytes(limit)),
                None => format!("{} won't accept a file this large.", host),
            },
            Some("unsupported_media_type") => match self.details.get("mime").and_then(|v| v.as_str()) {
                Some(mime) => format!("{} doesn't accept {} files.", host, mime),
                None => format!("{} doesn't accept this kind of file.", host),
            },
            Some("client_not_permitted") => format!(
                "{} only takes uploads from Vector accounts it recognises. {}",
                host, self.message,
            ),
            Some("rate_limited") => match self.retry_after() {
                Some(d) => format!("{} is rate-limiting you; try again in {}s.", host, d.as_secs()),
                None => format!("{} is rate-limiting you; try again shortly.", host),
            },
            Some("server_full") => format!("{} is out of space.", host),
            Some("blob_blocked") => format!("{} has blocked this file.", host),
            Some("auth_expired") | Some("auth_from_the_future") => format!(
                "{} rejected the upload signature's timestamp — this device's clock may be wrong.",
                host,
            ),
            Some(c) if c.starts_with("auth_") => {
                format!("{} rejected the upload authorization ({}).", host, self.message)
            }
            Some("internal") => format!("{} hit an internal error.", host),
            Some(_) => format!("{}: {}", host, self.message),
            None => match self.status {
                413 => format!("{} won't accept a file this large.", host),
                415 => format!("{} doesn't accept this kind of file.", host),
                401 | 402 => format!("{} refused Vector's upload signature ({}).", host, self.message),
                429 => format!("{} is rate-limiting you; try again shortly.", host),
                s if s >= 500 => format!("{} is having trouble (HTTP {}).", host, s),
                s => format!("{}: HTTP {} {}", host, s, self.message),
            },
        }
    }
}

/// A `later` this much later is still "now" to someone watching a spinner.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(15);

/// 502 / 503 / 504 and the Cloudflare 52x family: the origin behind a proxy
/// cannot ingest the upload, and the next attempt hits the same wall.
pub fn is_gateway_status(status: u16) -> bool {
    matches!(status, 502 | 503 | 504 | 520..=526)
}

/// Why one upload attempt did not produce a blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadFailure {
    /// The user stopped it.
    Cancelled,
    /// The server answered, and the answer was no.
    Refused(Refusal),
    /// The bytes never fully arrived: connection dropped, stalled, refused.
    Transport(String),
    /// The server ACKed a different hash than was sent — it re-encoded the
    /// blob, which is fatal for ciphertext.
    Integrity(String),
    /// Something before or after the wire: signing, URL, parsing.
    Other(String),
}

impl UploadFailure {
    pub fn refusal(&self) -> Option<&Refusal> {
        match self {
            UploadFailure::Refused(r) => Some(r),
            _ => None,
        }
    }

    /// The connection went away with the body in flight. Above a few MB this
    /// is almost always a size policy enforced by dropping, not a blip.
    pub fn is_mid_stream_drop(&self) -> bool {
        let UploadFailure::Transport(msg) = self else {
            return false;
        };
        [
            "Upload request failed",
            "error sending request",
            "connection reset",
            "connection closed",
            "connection refused",
            "body write",
            "IncompleteMessage",
            "broken pipe",
        ]
        .iter()
        .any(|needle| msg.contains(needle))
    }

    /// One sentence for the person waiting on the upload.
    pub fn describe(&self, host: &str) -> String {
        match self {
            UploadFailure::Cancelled => "Upload cancelled".to_string(),
            UploadFailure::Refused(r) => r.describe(host),
            UploadFailure::Transport(m) if m.starts_with("Upload stalled") => {
                format!("{} stopped accepting the file mid-way.", host)
            }
            UploadFailure::Transport(_) => format!("{} could not be reached.", host),
            UploadFailure::Integrity(_) => format!("{} altered the file instead of storing it.", host),
            UploadFailure::Other(m) => m.clone(),
        }
    }
}

impl std::fmt::Display for UploadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UploadFailure::Cancelled => write!(f, "Upload cancelled"),
            // The "with status N" shape is load-bearing: `parse_status_from_error`
            // reads it wherever this crosses into a String.
            UploadFailure::Refused(r) => match r.code() {
                Some(c) => write!(f, "Upload failed with status {} [{}]: {}", r.status, c, r.message),
                None => write!(f, "Upload failed with status {}: {}", r.status, r.message),
            },
            UploadFailure::Transport(m) | UploadFailure::Integrity(m) | UploadFailure::Other(m) => {
                write!(f, "{}", m)
            }
        }
    }
}

impl From<UploadFailure> for String {
    fn from(e: UploadFailure) -> String {
        e.to_string()
    }
}

/// The host a server URL names, for messages: `magnitude.jskitty.com`, not
/// `https://magnitude.jskitty.com/`.
pub fn host_of(server_url: &str) -> String {
    url::Url::parse(server_url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_else(|| server_url.trim_end_matches('/').to_string())
}

/// What to tell the user when every server said no.
pub fn summarise_failures(failures: &[(String, UploadFailure)]) -> String {
    match failures {
        [] => "No media server is configured. Add one in Settings → Network.".to_string(),
        [(host, e)] => e.describe(host),
        many => {
            let mut out = String::from("No media server accepted the file:");
            for (host, e) in many {
                out.push_str("\n• ");
                out.push_str(&e.describe(host));
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn a_magnitude_refusal_is_read_from_headers_and_body() {
        let h = headers(&[("x-error-code", "quota_daily_exceeded"), ("x-retry", "later"), ("x-reason", "prose")]);
        let body = r#"{"error":"quota_daily_exceeded","message":"Blob would exceed today's upload allowance: 900 of 1000 bytes already uploaded","retry":"later","status":413,"used":900,"limit":1000}"#;
        let r = Refusal::from_response(413, &h, body);
        assert_eq!(r.code(), Some("quota_daily_exceeded"));
        assert_eq!(r.retry, Some(Retry::Later));
        assert_eq!(r.limit(), Some(1000));
        assert_eq!(r.details.get("used").and_then(|v| v.as_u64()), Some(900));
        assert!(r.message.starts_with("Blob would exceed"));
        assert!(!r.details.contains_key("error"), "envelope fields are not details");
    }

    #[test]
    fn a_head_preflight_carries_the_code_with_no_body() {
        let h = headers(&[("x-error-code", "client_not_permitted"), ("x-retry", "elsewhere"), ("x-reason", "Uploads need a Vector profile")]);
        let r = Refusal::from_response(403, &h, "");
        assert_eq!(r.code(), Some("client_not_permitted"));
        assert_eq!(r.retry, Some(Retry::Elsewhere));
        assert_eq!(r.message, "Uploads need a Vector profile");
    }

    #[test]
    fn a_plain_server_has_no_code_and_keeps_its_body() {
        let r = Refusal::from_response(400, &HeaderMap::new(), "Content-Type header does not match the file content");
        assert_eq!(r.code(), None);
        assert_eq!(r.retry, None);
        assert!(r.is_mime(), "the keyword heuristic still applies without a code");
    }

    #[test]
    fn a_quota_413_is_not_a_size_limit_but_a_plain_413_is() {
        // Both are 413.
        let h = headers(&[("x-error-code", "quota_daily_exceeded")]);
        assert!(!Refusal::from_response(413, &h, "").is_size_limit());
        let h = headers(&[("x-error-code", "blob_too_large")]);
        assert!(Refusal::from_response(413, &h, "").is_size_limit());
        assert!(Refusal::from_response(413, &HeaderMap::new(), "Payload Too Large").is_size_limit());
    }

    #[test]
    fn a_coded_401_is_not_a_mime_rejection_but_an_uncoded_one_still_is() {
        let h = headers(&[("x-error-code", "auth_expired")]);
        assert!(!Refusal::from_response(401, &h, "").is_mime());
        // Pre-existing behaviour for servers without the vocabulary, unchanged.
        assert!(Refusal::from_response(401, &HeaderMap::new(), "Unauthorized").is_mime());
    }

    #[test]
    fn a_garbage_code_header_is_ignored() {
        let h = headers(&[("x-error-code", "Not A Code!")]);
        assert_eq!(Refusal::from_response(500, &h, "").code(), None);
    }

    #[test]
    fn retry_after_comes_from_the_header_or_the_body() {
        let h = headers(&[("x-error-code", "rate_limited"), ("retry-after", "7")]);
        assert_eq!(Refusal::from_response(429, &h, "").retry_after(), Some(Duration::from_secs(7)));
        let r = Refusal::from_response(429, &headers(&[("x-error-code", "rate_limited")]), r#"{"retry_after":3}"#);
        assert_eq!(r.retry_after(), Some(Duration::from_secs(3)));
    }

    #[test]
    fn only_a_short_rate_limit_or_an_internal_error_is_retried_on_the_same_server() {
        let coded = |code: &str, extra: &str| {
            Refusal::from_response(400, &headers(&[("x-error-code", code)]), extra)
        };
        assert!(coded("rate_limited", r#"{"retry_after":5}"#).worth_retrying_here());
        assert!(!coded("rate_limited", r#"{"retry_after":600}"#).worth_retrying_here());
        assert!(coded("internal", "").worth_retrying_here());
        assert!(!coded("quota_daily_exceeded", "").worth_retrying_here(), "tomorrow is not a retry");
        assert!(!coded("client_not_permitted", "").worth_retrying_here());
        assert!(!coded("auth_expired", "").worth_retrying_here());
        assert!(!coded("blob_too_large", "").worth_retrying_here());
    }

    #[test]
    fn an_uncoded_server_is_retried_unless_the_status_is_final() {
        let plain = |status: u16, body: &str| Refusal::from_response(status, &HeaderMap::new(), body);
        assert!(plain(500, "Internal Server Error").worth_retrying_here());
        assert!(plain(408, "").worth_retrying_here());
        assert!(!plain(413, "").worth_retrying_here());
        assert!(!plain(415, "").worth_retrying_here());
        assert!(!plain(503, "").worth_retrying_here(), "gateway statuses route around, as before");
        assert!(!plain(524, "").worth_retrying_here());
    }

    #[test]
    fn descriptions_name_the_host_and_the_numbers() {
        let h = headers(&[("x-error-code", "quota_daily_exceeded")]);
        let r = Refusal::from_response(413, &h, r#"{"used":1073741824,"limit":1073741824}"#);
        assert_eq!(
            r.describe("magnitude.jskitty.com"),
            "Today's 1.0 GB upload allowance on magnitude.jskitty.com is used up (1.0 GB uploaded so far). It resets at midnight UTC.",
        );
        let h = headers(&[("x-error-code", "blob_too_large")]);
        let r = Refusal::from_response(413, &h, r#"{"limit":26214400}"#);
        assert_eq!(r.describe("m.example"), "m.example allows files up to 25.0 MB for your account.");
        let r = Refusal::from_response(413, &HeaderMap::new(), "");
        assert_eq!(r.describe("m.example"), "m.example won't accept a file this large.");
    }

    #[test]
    fn display_keeps_the_status_shape_the_string_parsers_rely_on() {
        let h = headers(&[("x-error-code", "blob_too_large")]);
        let e = UploadFailure::Refused(Refusal::from_response(413, &h, r#"{"message":"too big"}"#));
        assert_eq!(e.to_string(), "Upload failed with status 413 [blob_too_large]: too big");
        assert_eq!(UploadFailure::Cancelled.to_string(), "Upload cancelled");
    }

    #[test]
    fn mid_stream_drops_are_recognised_only_on_transport_failures() {
        assert!(UploadFailure::Transport("Upload request failed: connection reset".into()).is_mid_stream_drop());
        assert!(!UploadFailure::Transport("Upload stalled: x accepted nothing".into()).is_mid_stream_drop());
        assert!(!UploadFailure::Other("connection reset".into()).is_mid_stream_drop());
    }

    #[test]
    fn a_summary_lists_every_server_when_there_are_several() {
        let a = UploadFailure::Transport("Upload request failed: x".into());
        let b = UploadFailure::Refused(Refusal::from_response(415, &HeaderMap::new(), ""));
        assert_eq!(summarise_failures(&[]), "No media server is configured. Add one in Settings → Network.");
        assert_eq!(summarise_failures(&[("a.example".into(), a.clone())]), "a.example could not be reached.");
        assert_eq!(
            summarise_failures(&[("a.example".into(), a), ("b.example".into(), b)]),
            "No media server accepted the file:\n• a.example could not be reached.\n• b.example doesn't accept this kind of file.",
        );
    }

    #[test]
    fn host_of_strips_the_url_down_to_its_name() {
        assert_eq!(host_of("https://magnitude.jskitty.com/"), "magnitude.jskitty.com");
        assert_eq!(host_of("http://localhost:8080"), "localhost");
        assert_eq!(host_of("not a url"), "not a url");
    }
}
