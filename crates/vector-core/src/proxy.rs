//! Pictures, files and previews fetched by the user's Magnitude server
//! instead of by this device.
//!
//! A picture loaded here, a file downloaded here, or a page previewed here
//! is a request from this device's address to whoever hosts the thing, and
//! most of it arrives without a click: sending someone a link or an emoji
//! is enough to learn where they are. Magnitude fetches on the client's
//! behalf (`magnitude-unfurl` for a page's metadata, `magnitude-proxy` for
//! everything else), advertised in its information document. This module
//! finds the first configured server that offers it and routes through it,
//! under one privacy setting, on by default.
//!
//! Every outbound request in every crate resolves its destination through
//! [`crate::net::egress`], which asks here. The only requests that never come
//! through are the ones that cannot: Nostr relay sockets, and the signed
//! writes (upload, mirror, delete, discovery) to a Blossom server the user
//! chose, which already carry the user's key.
//!
//! When the setting is on and no configured server offers previews, there is
//! no preview: falling back to fetching the page here would be the leak with
//! a new name. Pictures do fall back to a direct load when no server offers
//! the proxy, because an avatar that never appears is not a privacy feature
//! anyone chose; the setting's own help text says so.

use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const SETTING_KEY: &str = "privacy_proxy_media";
pub const UNFURL_EXT: &str = "magnitude-unfurl";
pub const PROXY_EXT: &str = "magnitude-proxy";

/// The setting, read fresh each time: it is one SQLite row and a change in
/// Settings must take effect on the next picture, not the next launch.
pub fn enabled() -> bool {
    match crate::db::get_sql_setting(SETTING_KEY.to_string()) {
        Ok(Some(v)) => v == "true" || v == "1",
        _ => true,
    }
}

/// Which server offers what, remembered briefly so a chat full of pictures
/// does not scan the server list per picture.
struct Pick {
    extension: &'static str,
    server: Option<String>,
    at: Instant,
}

static PICKS: Mutex<Vec<Pick>> = Mutex::new(Vec::new());
const PICK_TTL: Duration = Duration::from_secs(10 * 60);
/// How long "no server offers it" is believed before asking again.
const NEGATIVE_PICK_TTL: Duration = Duration::from_secs(60);

/// The first configured Blossom server whose information document lists
/// `extension`, as an origin without a trailing slash.
pub async fn server_offering(extension: &'static str) -> Option<String> {
    if let Ok(picks) = PICKS.lock() {
        if let Some(p) = picks.iter().find(|p| p.extension == extension) {
            if p.at.elapsed() < PICK_TTL {
                return p.server.clone();
            }
        }
    }
    // The list is installed at login, and the first pictures are asked for
    // in the same seconds; a pick made against an empty list is not "no
    // server offers it", it is "not yet". Wait a little for it.
    let mut servers = crate::state::get_blossom_servers();
    for _ in 0..25 {
        if !servers.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        servers = crate::state::get_blossom_servers();
    }
    let mut found = None;
    // Whether every server was actually asked. A document that could not
    // be fetched says nothing about what the server offers.
    let mut every_answer_known = !servers.is_empty();
    for server in servers {
        // A document no older than an hour, refetched otherwise. Never the
        // persisted copy at any age: this account's was written before the
        // server offered previews or the proxy, and trusting it meant every
        // picture loaded directly while the toggle said otherwise.
        let info = match crate::signer::active_signer() {
            Ok(signer) => match crate::blossom_info::refresh(&signer, &server, Duration::from_secs(60 * 60)).await {
                Ok(i) => i,
                Err(_) => {
                    every_answer_known = false;
                    None
                }
            },
            // No signer to ask with: the last copy is all there is.
            Err(_) => {
                every_answer_known = false;
                crate::blossom_info::cached(&server)
            }
        };
        if info.map(|i| i.extensions.iter().any(|e| e == extension)).unwrap_or(false) {
            found = Some(server.trim_end_matches('/').to_string());
            break;
        }
    }
    match &found {
        Some(s) => crate::log_debug!("[Proxy] {} offered by {}", extension, s),
        None if every_answer_known => crate::log_warn!("[Proxy] no configured Blossom server offers {}; falling back", extension),
        None => crate::log_warn!("[Proxy] could not learn which server offers {}; asking again next time", extension),
    }
    // A found server is remembered; a settled "none" (every server asked,
    // none offers it) only briefly, so a server that starts offering it is
    // seen soon; a failure to learn is not remembered at all.
    if let Ok(mut picks) = PICKS.lock() {
        picks.retain(|p| p.extension != extension);
        match (&found, every_answer_known) {
            (Some(_), _) => picks.push(Pick { extension, server: found.clone(), at: Instant::now() }),
            // Dated back so it ages out after the short window, not the long.
            (None, true) => {
                if let Some(at) = Instant::now().checked_sub(PICK_TTL - NEGATIVE_PICK_TTL) {
                    picks.push(Pick { extension, server: None, at });
                }
            }
            (None, false) => {}
        }
    }
    found
}

/// Forget the remembered picks: the server list or a setting changed.
pub fn forget_picks() {
    if let Ok(mut picks) = PICKS.lock() {
        picks.clear();
    }
}

/// Where to actually load `url` from. The proxy's address when the setting is
/// on, a server offers it, and the URL is somebody else's; `None` means load
/// it directly. A blob on one of the user's own servers is already theirs to
/// fetch, and a proxy URL is not proxied again.
pub async fn proxied(url: &str) -> Option<String> {
    if !enabled() || !wants_proxy(url) {
        return None;
    }
    let server = server_offering(PROXY_EXT).await?;
    Some(proxy_url(&server, url))
}

pub fn proxy_url(server: &str, url: &str) -> String {
    let encoded: String = url::form_urlencoded::byte_serialize(url.as_bytes()).collect();
    format!("{server}/proxy?url={encoded}")
}

/// Domains that are ours: a Magnitude answers these, so loading from them
/// directly reveals nothing to anyone else, and routing them through a
/// Magnitude would be Magnitude proxying Magnitude.
const OWN_DOMAINS: &[&str] = &["vectorapp.io", "jskitty.com"];

fn is_own_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    OWN_DOMAINS.iter().any(|d| h == *d || h.ends_with(&format!(".{d}")))
}

/// Everything over http(s) that is not ours goes through the proxy: a
/// picture, an avatar, an emoji, and an attachment on somebody's Blossom
/// server, which otherwise learns the recipient's address on every
/// download. Only our own domains load directly, and a URL that is already
/// a proxy or preview request is never wrapped again.
pub fn wants_proxy(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else { return false };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    if parsed.path() == "/proxy" || parsed.path() == "/unfurl" {
        return false;
    }
    !parsed.host_str().map(is_own_host).unwrap_or(true)
}

/// A signed `Authorization` for a proxied request, so the proxy charges the
/// account rather than the address and a recognised account gets its own
/// allowance. `None` when nobody is signed in; the proxy then treats the
/// request as a stranger's, which still works.
pub async fn proxy_authorization(server: &str) -> Option<reqwest::header::HeaderValue> {
    let signer = crate::signer::active_signer().ok()?;
    let server_url = url::Url::parse(server).ok()?;
    crate::blossom_info::build_get_auth_header(&signer, &server_url, "Proxied fetch")
        .await
        .ok()
}

/// The proxy server a proxied URL points at, so its authorization can be
/// scoped to it.
pub fn proxy_server_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    if parsed.path() != "/proxy" {
        return None;
    }
    Some(parsed.origin().ascii_serialization())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_but_our_own_domains_wants_the_proxy() {
        assert!(wants_proxy("https://cdn.example.net/a.jpg"));
        assert!(wants_proxy("https://blossom.primal.net/abc.bin"), "somebody's Blossom server is somebody's");
        assert!(wants_proxy("https://image.nostr.build/x.png"));
        assert!(!wants_proxy("https://magnitude.jskitty.com/abc.png"), "ours");
        assert!(!wants_proxy("https://us.magnitude.jskitty.com/abc.png"), "ours, an edge");
        assert!(!wants_proxy("https://vectorapp.io/assets/x.png"), "ours");
        assert!(wants_proxy("https://notjskitty.com/x.png"), "a suffix is not a subdomain");
        assert!(!wants_proxy("data:image/png;base64,AAAA"));
        assert!(!wants_proxy("asset://localhost/x.png"));
        assert!(!wants_proxy("https://magnitude.example/proxy?url=x"));
        assert_eq!(proxy_server_of("https://us.magnitude.jskitty.com/proxy?url=x").as_deref(), Some("https://us.magnitude.jskitty.com"));
        assert!(proxy_server_of("https://cdn.example.net/a.jpg").is_none());
    }
}
