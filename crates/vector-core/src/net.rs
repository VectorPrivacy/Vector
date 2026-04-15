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

/// Build an HTTP client with optional timeout.
pub fn build_http_client(timeout: std::time::Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;

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
