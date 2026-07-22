//! Network utilities — SSRF protection, HTTP client helpers.

use url::Url;

/// Reject URLs that resolve to private/loopback/link-local addresses (SSRF protection).
pub fn validate_url_not_private(url_str: &str) -> Result<(), &'static str> {
    let parsed = Url::parse(url_str).map_err(|_| "Invalid URL")?;

    match parsed.scheme() {
        "http" | "https" => {}
        _ => return Err("Only HTTP(S) URLs are allowed"),
    }

    match parsed.host() {
        Some(url::Host::Ipv4(ip)) => {
            let o = ip.octets();
            if ip.is_loopback() || ip.is_private() || ip.is_link_local()
                || ip.is_broadcast() || ip.is_unspecified()
                || (o[0] == 100 && o[1] >= 64 && o[1] <= 127)
            {
                return Err("Private/internal IP addresses are not allowed");
            }
        }
        Some(url::Host::Ipv6(ip)) => {
            if ip.is_loopback() || ip.is_unspecified() || is_ipv6_private(&ip) {
                return Err("Private/internal IP addresses are not allowed");
            }
        }
        Some(url::Host::Domain(domain)) => {
            if domain == "localhost" || domain.ends_with(".local") || domain.ends_with(".internal") {
                return Err("Local hostnames are not allowed");
            }
        }
        None => return Err("URL has no host"),
    }

    Ok(())
}

fn is_ipv6_private(ip: &std::net::Ipv6Addr) -> bool {
    if let Some(ipv4) = ip.to_ipv4_mapped() {
        return ipv4.is_loopback() || ipv4.is_private() || ipv4.is_link_local();
    }
    let segments = ip.segments();
    if segments[0] & 0xfe00 == 0xfc00 { return true; } // Unique local
    if segments[0] & 0xffc0 == 0xfe80 { return true; } // Link-local
    false
}

/// Build an HTTP client with the given timeout.
///
/// Honors the Tor failsafe: when the user has Tor enabled, every connection
/// goes through Tor — period. If Tor is enabled but not currently running
/// (bootstrap in flight, mid-restart, service crashed), the returned client
/// is wired to a blackhole SOCKS proxy so requests fail at the TCP layer
/// without any chance of leaking clearnet traffic. Direct connections are
/// only ever issued when the user has explicitly disabled Tor.
///
/// Callers should use this rather than `reqwest::Client::builder()` directly
/// so the failsafe automatically covers their traffic. The `disallowed_methods`
/// clippy lint enforces this everywhere except this one canonical call site.
#[allow(clippy::disallowed_methods)]
pub fn build_http_client(timeout: std::time::Duration) -> Result<reqwest::Client, String> {
    build_http_client_with_options(timeout, None, true)
}

/// Like `build_http_client`, optionally without redirect-following.
/// Blossom PUT uses `false`: a 3xx mid-upload would re-issue as GET and
/// drop the body, so we surface the 3xx as the real failure status.
#[allow(clippy::disallowed_methods)]
pub fn build_http_client_with_options(
    timeout: std::time::Duration,
    read_timeout: Option<std::time::Duration>,
    follow_redirects: bool,
) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder().timeout(timeout);
    // Idle read timeout (opt-in): a server that accepts the connection but streams
    // nothing back for this long is treated as dead, so upload failover moves on fast
    // instead of waiting out the whole `timeout`. It resets on every received byte, so
    // a slow-but-progressing transfer survives. Left None for large uploads, whose
    // server can legitimately go quiet while it stores the blob.
    if let Some(rt) = read_timeout {
        builder = builder.read_timeout(rt);
    }
    if !follow_redirects {
        builder = builder.redirect(reqwest::redirect::Policy::none());
    } else {
        // Validate EVERY redirect hop, not just the initial URL: a public
        // host answering `302 Location: http://169.254.169.254/…` would
        // otherwise walk the request straight past the SSRF check.
        builder = builder.redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error("too many redirects");
            }
            match validate_url_not_private(attempt.url().as_str()) {
                Ok(()) => attempt.follow(),
                Err(e) => attempt.error(e),
            }
        }));
    }

    #[cfg(feature = "tor")]
    {
        match crate::tor::transport_state() {
            crate::tor::TorTransportState::Active(addr) => {
                // Use the addr from the variant directly — re-querying via
                // proxy_url() races against TorService::stop() and can panic.
                let url = format!("socks5h://{addr}");
                let proxy = reqwest::Proxy::all(&url)
                    .map_err(|e| format!("Tor proxy URL ({url}) invalid: {e}"))?;
                builder = builder.proxy(proxy);
            }
            crate::tor::TorTransportState::RequiredButInactive => {
                // Tor failsafe: route to a blackhole so connections fail safe
                // instead of leaking direct.
                let url = format!("socks5h://{}", crate::tor::blackhole_proxy_addr());
                let proxy = reqwest::Proxy::all(&url)
                    .map_err(|e| format!("blackhole proxy invalid: {e}"))?;
                builder = builder.proxy(proxy);
            }
            crate::tor::TorTransportState::Disabled => {
                // No proxy — user has Tor off.
            }
        }
    }

    builder
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))
}

// ============================================================================
// Shared HTTP client — proxy-aware + rebuildable on Tor toggle
// ============================================================================
//
// Some call sites (image-cache fetches, PIVX wallet polling) make frequent
// requests and benefit from a shared `reqwest::Client` to reuse connection
// pools / TLS sessions. A bare `LazyLock<Client>` doesn't work for us because
// a Tor toggle should affect future requests immediately — but the static is
// frozen at first init. Instead, we hold an `Arc<Client>` behind a `RwLock`
// and rebuild it via `rebuild_shared_http_client()` whenever the Tor state
// changes. In-flight requests finish on the old Arc; new requests pick up
// the new one.

use std::sync::{Arc, OnceLock, RwLock};

static SHARED_HTTP_CLIENT: OnceLock<RwLock<Arc<reqwest::Client>>> = OnceLock::new();

const DEFAULT_SHARED_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn shared_cell() -> &'static RwLock<Arc<reqwest::Client>> {
    SHARED_HTTP_CLIENT.get_or_init(|| {
        let client = build_http_client(DEFAULT_SHARED_TIMEOUT)
            .expect("initial shared HTTP client build cannot fail");
        RwLock::new(Arc::new(client))
    })
}

/// Get a shared HTTP client. Cheap clone (Arc), proxy-aware, picks up Tor
/// toggles on the next call after `rebuild_shared_http_client()` runs.
pub fn shared_http_client() -> Arc<reqwest::Client> {
    shared_cell().read().unwrap().clone()
}

/// Rebuild the shared client. Call this when Tor state flips so the next
/// request goes through the freshly-configured proxy. In-flight requests on
/// the old client continue to completion on the previous Arc.
pub fn rebuild_shared_http_client() -> Result<(), String> {
    let new = Arc::new(build_http_client(DEFAULT_SHARED_TIMEOUT)?);
    *shared_cell().write().unwrap() = new;
    Ok(())
}

/// Find the byte index where a bracket/paren group opened at `start` closes,
/// tracking nesting depth and honoring backslash escapes — markdown balances
/// both, so a naive first-closer scan desyncs on `[[claim]](evil)` or
/// `[claim\]](evil)` and lets the claim reach the URL scan. All compared
/// bytes are ASCII, so the returned index is char-boundary-safe.
fn md_group_close(bytes: &[u8], start: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 1usize;
    let mut escaped = false;
    let mut j = start;
    loop {
        match bytes.get(j).copied() {
            None => return None,
            Some(b'\\') if !escaped => escaped = true,
            Some(b) if b == open && !escaped => depth += 1,
            Some(b) if b == close && !escaped => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => escaped = false,
        }
        j += 1;
    }
}

/// Rewrite markdown links so a preview-URL scan sees only real DESTINATIONS:
/// `[text](href)` keeps the href and drops the display text — a URL claimed in
/// the text must never win the OG preview over where the link actually goes —
/// `[text](<href>)` drops entirely (angle brackets are the no-preview syntax),
/// and `[text][ref]` drops the label (its destination is a definition scanned
/// on its own elsewhere in the text). Images (`![alt](url)`) render as literal
/// text in chat, so they pass through untouched.
pub fn strip_md_link_claims(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'[' || (i > 0 && bytes[i - 1] == b'!') {
            i += 1;
            continue;
        }
        let Some(close) = md_group_close(bytes, i + 1, b'[', b']') else { break };
        match bytes.get(close + 1).copied() {
            // Inline link: drop the label, contribute the destination.
            Some(b'(') => {
                let Some(paren) = md_group_close(bytes, close + 2, b'(', b')') else {
                    i = close + 1;
                    continue;
                };
                out.push_str(&text[last..i]);
                let href = text[close + 2..paren].trim();
                if !(href.starts_with('<') && href.ends_with('>')) {
                    out.push(' ');
                    out.push_str(href);
                    out.push(' ');
                }
                i = paren + 1;
                last = i;
            }
            // Reference link: drop the label; the `[ref]: url` definition line
            // carries the real destination and gets scanned as plain text.
            Some(b'[') => {
                let Some(ref_close) = md_group_close(bytes, close + 2, b'[', b']') else {
                    i = close + 1;
                    continue;
                };
                out.push_str(&text[last..i]);
                i = ref_close + 1;
                last = i;
            }
            _ => {
                i = close + 1;
            }
        }
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // strip_md_link_claims — preview scan must see destinations, not claims
    // ========================================================================

    #[test]
    fn md_link_claim_text_dropped_href_kept() {
        // The spoof shape: claimed URL first in raw text, real destination second.
        let out = strip_md_link_claims("[https://your-bank.com](https://evil.io)");
        assert!(!out.contains("your-bank.com"), "claimed text must not reach the scan: {out}");
        assert!(out.contains("https://evil.io"), "real destination must reach the scan: {out}");
    }

    #[test]
    fn md_no_preview_link_dropped_entirely() {
        let out = strip_md_link_claims("see [docs](<https://vector.app/docs>) ok");
        assert!(!out.contains("vector.app"), "no-preview href must not reach the scan: {out}");
        assert!(out.contains("see ") && out.contains(" ok"));
    }

    #[test]
    fn md_image_passes_through() {
        let text = "![shot](https://host.io/img.png)";
        assert_eq!(strip_md_link_claims(text), text);
    }

    #[test]
    fn plain_text_and_bare_urls_untouched() {
        let text = "check https://vector.app and [also] (spaced) brackets";
        assert_eq!(strip_md_link_claims(text), text);
    }

    #[test]
    fn multiple_links_keep_document_order() {
        let out = strip_md_link_claims("[a](https://one.io) mid [b](https://two.io)");
        let one = out.find("https://one.io").expect("first href kept");
        let two = out.find("https://two.io").expect("second href kept");
        assert!(one < two);
    }

    #[test]
    fn nested_bracket_label_still_drops_claim() {
        let out = strip_md_link_claims("[[https://trusted.com]](https://evil.io)");
        assert!(!out.contains("trusted.com"), "nested-bracket claim must not reach the scan: {out}");
        assert!(out.contains("https://evil.io"));
    }

    #[test]
    fn escaped_bracket_label_still_drops_claim() {
        let out = strip_md_link_claims(r"[https://trusted.com\]](https://evil.io)");
        assert!(!out.contains("trusted.com"), "escaped-bracket claim must not reach the scan: {out}");
        assert!(out.contains("https://evil.io"));
    }

    #[test]
    fn paren_path_href_survives_whole() {
        let out = strip_md_link_claims("[wiki](https://en.wikipedia.org/wiki/Foo_(bar))");
        assert!(out.contains("https://en.wikipedia.org/wiki/Foo_(bar)"), "balanced-paren href kept intact: {out}");
    }

    #[test]
    fn reference_link_label_dropped_definition_scanned() {
        let out = strip_md_link_claims("[https://trusted.com][1]\n[1]: https://evil.io");
        assert!(!out.contains("trusted.com"), "reflink claim must not reach the scan: {out}");
        assert!(out.contains("https://evil.io"), "definition URL stays scannable: {out}");
    }

    #[test]
    fn multibyte_label_no_panic() {
        let out = strip_md_link_claims("[🔒 sécurisé — café](https://evil.io) 日本語");
        assert!(out.contains("https://evil.io"));
        assert!(out.contains("日本語"));
    }

    // ========================================================================
    // Valid public URLs — should pass
    // ========================================================================

    #[test]
    fn valid_public_https_url_passes() {
        assert!(validate_url_not_private("https://example.com/path").is_ok(),
            "https://example.com should be allowed");
    }

    #[test]
    fn valid_public_http_url_passes() {
        assert!(validate_url_not_private("http://example.com").is_ok(),
            "http://example.com should be allowed");
    }

    #[test]
    fn valid_public_ip_8888_passes() {
        assert!(validate_url_not_private("https://8.8.8.8/dns").is_ok(),
            "8.8.8.8 (Google DNS) is a public IP and should be allowed");
    }

    #[test]
    fn valid_public_ip_1111_passes() {
        assert!(validate_url_not_private("https://1.1.1.1").is_ok(),
            "1.1.1.1 (Cloudflare DNS) is a public IP and should be allowed");
    }

    #[test]
    fn valid_url_with_port_passes() {
        assert!(validate_url_not_private("https://example.com:8080/api").is_ok(),
            "URL with port on public domain should be allowed");
    }

    // ========================================================================
    // Loopback addresses — should be rejected
    // ========================================================================

    #[test]
    fn localhost_rejected() {
        let result = validate_url_not_private("http://localhost/secret");
        assert!(result.is_err(), "localhost should be rejected");
    }

    #[test]
    fn ip_127_0_0_1_rejected() {
        let result = validate_url_not_private("http://127.0.0.1/admin");
        assert!(result.is_err(), "127.0.0.1 (loopback) should be rejected");
    }

    #[test]
    fn ip_127_255_255_255_rejected() {
        let result = validate_url_not_private("http://127.255.255.255");
        assert!(result.is_err(), "127.255.255.255 (loopback range) should be rejected");
    }

    // ========================================================================
    // Private IP ranges — should be rejected
    // ========================================================================

    #[test]
    fn private_class_a_10_rejected() {
        let result = validate_url_not_private("http://10.0.0.1/internal");
        assert!(result.is_err(), "10.0.0.1 (private class A) should be rejected");
    }

    #[test]
    fn private_class_b_172_16_rejected() {
        let result = validate_url_not_private("http://172.16.0.1/internal");
        assert!(result.is_err(), "172.16.0.1 (private class B) should be rejected");
    }

    #[test]
    fn private_class_b_172_31_rejected() {
        let result = validate_url_not_private("http://172.31.255.255");
        assert!(result.is_err(), "172.31.255.255 (private class B upper bound) should be rejected");
    }

    #[test]
    fn private_class_c_192_168_rejected() {
        let result = validate_url_not_private("http://192.168.1.1/router");
        assert!(result.is_err(), "192.168.1.1 (private class C) should be rejected");
    }

    // ========================================================================
    // Special addresses — should be rejected
    // ========================================================================

    #[test]
    fn link_local_169_254_rejected() {
        let result = validate_url_not_private("http://169.254.1.1");
        assert!(result.is_err(), "169.254.1.1 (link-local) should be rejected");
    }

    #[test]
    fn cgn_100_64_rejected() {
        let result = validate_url_not_private("http://100.64.0.1");
        assert!(result.is_err(), "100.64.0.1 (CGN / shared address space) should be rejected");
    }

    #[test]
    fn cgn_100_127_rejected() {
        let result = validate_url_not_private("http://100.127.255.255");
        assert!(result.is_err(), "100.127.255.255 (CGN upper bound) should be rejected");
    }

    #[test]
    fn broadcast_255_rejected() {
        let result = validate_url_not_private("http://255.255.255.255");
        assert!(result.is_err(), "255.255.255.255 (broadcast) should be rejected");
    }

    #[test]
    fn unspecified_0_0_0_0_rejected() {
        let result = validate_url_not_private("http://0.0.0.0");
        assert!(result.is_err(), "0.0.0.0 (unspecified) should be rejected");
    }

    // ========================================================================
    // IPv6 addresses — should be rejected
    // ========================================================================

    #[test]
    fn ipv6_loopback_rejected() {
        let result = validate_url_not_private("http://[::1]/secret");
        assert!(result.is_err(), "::1 (IPv6 loopback) should be rejected");
    }

    #[test]
    fn ipv6_unique_local_fc00_rejected() {
        let result = validate_url_not_private("http://[fc00::1]");
        assert!(result.is_err(), "fc00::1 (IPv6 unique-local) should be rejected");
    }

    #[test]
    fn ipv6_unique_local_fd00_rejected() {
        let result = validate_url_not_private("http://[fd00::1]");
        assert!(result.is_err(), "fd00::1 (IPv6 unique-local) should be rejected");
    }

    #[test]
    fn ipv6_link_local_fe80_rejected() {
        let result = validate_url_not_private("http://[fe80::1]");
        assert!(result.is_err(), "fe80::1 (IPv6 link-local) should be rejected");
    }

    #[test]
    fn ipv4_mapped_ipv6_loopback_rejected() {
        let result = validate_url_not_private("http://[::ffff:127.0.0.1]");
        assert!(result.is_err(), "::ffff:127.0.0.1 (IPv4-mapped loopback) should be rejected");
    }

    #[test]
    fn ipv4_mapped_ipv6_private_rejected() {
        let result = validate_url_not_private("http://[::ffff:192.168.1.1]");
        assert!(result.is_err(), "::ffff:192.168.1.1 (IPv4-mapped private) should be rejected");
    }

    // ========================================================================
    // Domain name restrictions
    // ========================================================================

    #[test]
    fn dot_local_domain_rejected() {
        let result = validate_url_not_private("http://mydevice.local/api");
        assert!(result.is_err(), ".local domain should be rejected");
    }

    #[test]
    fn dot_internal_domain_rejected() {
        let result = validate_url_not_private("http://service.internal/health");
        assert!(result.is_err(), ".internal domain should be rejected");
    }

    // ========================================================================
    // Scheme restrictions
    // ========================================================================

    #[test]
    fn ftp_scheme_rejected() {
        let result = validate_url_not_private("ftp://example.com/file.txt");
        assert!(result.is_err(), "ftp:// scheme should be rejected");
        assert_eq!(result.unwrap_err(), "Only HTTP(S) URLs are allowed");
    }

    #[test]
    fn file_scheme_rejected() {
        let result = validate_url_not_private("file:///etc/passwd");
        assert!(result.is_err(), "file:// scheme should be rejected");
        assert_eq!(result.unwrap_err(), "Only HTTP(S) URLs are allowed");
    }

    #[test]
    fn javascript_scheme_rejected() {
        let result = validate_url_not_private("javascript:alert(1)");
        assert!(result.is_err(), "javascript: scheme should be rejected");
    }

    #[test]
    fn data_scheme_rejected() {
        let result = validate_url_not_private("data:text/html,<h1>hi</h1>");
        assert!(result.is_err(), "data: scheme should be rejected");
    }

    // ========================================================================
    // Missing / invalid URL
    // ========================================================================

    #[test]
    fn no_host_rejected() {
        // http:// with no host is actually an invalid URL for the url crate
        let result = validate_url_not_private("http://");
        assert!(result.is_err(), "URL with no host should be rejected");
    }

    #[test]
    fn invalid_url_rejected() {
        let result = validate_url_not_private("not a url at all");
        assert!(result.is_err(), "invalid URL string should be rejected");
        assert_eq!(result.unwrap_err(), "Invalid URL");
    }

    #[test]
    fn empty_string_rejected() {
        let result = validate_url_not_private("");
        assert!(result.is_err(), "empty string should be rejected");
    }

    // ========================================================================
    // Edge cases
    // ========================================================================

    #[test]
    fn cgn_100_63_not_rejected() {
        // 100.63.x.x is NOT in the CGN range (100.64-100.127)
        assert!(validate_url_not_private("http://100.63.255.255").is_ok(),
            "100.63.255.255 is outside CGN range and should be allowed");
    }

    #[test]
    fn cgn_100_128_not_rejected() {
        // 100.128.x.x is NOT in the CGN range
        assert!(validate_url_not_private("http://100.128.0.1").is_ok(),
            "100.128.0.1 is outside CGN range and should be allowed");
    }

    #[test]
    fn private_172_15_not_rejected() {
        // 172.15.x.x is NOT private (private is 172.16-172.31)
        assert!(validate_url_not_private("http://172.15.255.255").is_ok(),
            "172.15.255.255 is outside private class B range and should be allowed");
    }

    #[test]
    fn private_172_32_not_rejected() {
        // 172.32.x.x is NOT private
        assert!(validate_url_not_private("http://172.32.0.1").is_ok(),
            "172.32.0.1 is outside private class B range and should be allowed");
    }
}

// ============================================================================
// Remote File Size
// ============================================================================

/// Get the size of a remote file via HEAD request or Range fallback.
/// Returns None if the URL is private, unreachable, or size can't be determined.
pub async fn get_remote_file_size(url: &str) -> Option<u64> {
    validate_url_not_private(url).ok()?;
    let client = build_http_client(std::time::Duration::from_secs(8)).ok()?;

    // Method 1: HEAD request
    if let Ok(head_res) = client.head(url).send().await {
        if let Some(length) = head_res.content_length() {
            if length > 0 {
                return Some(length);
            }
        }
    }

    // Method 2: Range request fallback
    if let Ok(partial_res) = client
        .get(url)
        .header("Range", "bytes=0-1")
        .send()
        .await
    {
        if let Some(content_range) = partial_res.headers().get("content-range") {
            if let Ok(range_str) = content_range.to_str() {
                if let Some(size_part) = range_str.split('/').nth(1) {
                    if let Ok(size) = size_part.parse::<u64>() {
                        return Some(size);
                    }
                }
            }
        }
        if let Some(length) = partial_res.content_length() {
            if length > 100 {
                return Some(length);
            }
        }
    }

    None
}
