//! Previews and pictures fetched by the user's Magnitude server instead of
//! by this device.
//!
//! A link preview fetched here, or a picture loaded here, is a request from
//! this device's address to whoever hosts the thing, and a link arrives
//! without a click: sending someone a link is enough to learn where they
//! are. Magnitude offers two endpoints that fetch on the client's behalf
//! (`magnitude-unfurl` for a page's metadata, `magnitude-proxy` for media),
//! advertised in its information document. This module finds the first
//! configured server that offers them and routes through it, under one
//! privacy setting, on by default.
//!
//! When the setting is on and no configured server offers previews, there is
//! no preview: falling back to fetching the page here would be the leak with
//! a new name. Pictures do fall back to a direct load when no server offers
//! the proxy, because an avatar that never appears is not a privacy feature
//! anyone chose; the setting's own help text says so.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use vector_core::types::SiteMetadata;

pub const SETTING_KEY: &str = "privacy_proxy_media";
pub const UNFURL_EXT: &str = "magnitude-unfurl";
pub const PROXY_EXT: &str = "magnitude-proxy";

/// The setting, read fresh each time: it is one SQLite row and a change in
/// Settings must take effect on the next picture, not the next launch.
pub fn enabled() -> bool {
    match vector_core::db::get_sql_setting(SETTING_KEY.to_string()) {
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
    let mut found = None;
    for server in crate::get_blossom_servers() {
        // A document no older than an hour, refetched otherwise. Never the
        // persisted copy at any age: this account's was written before the
        // server offered previews or the proxy, and trusting it meant every
        // picture loaded directly while the toggle said otherwise.
        let info = match vector_core::signer::active_signer() {
            Ok(signer) => vector_core::blossom_info::refresh(&signer, &server, Duration::from_secs(60 * 60))
                .await
                .ok()
                .flatten(),
            // No signer to ask with: the last copy is all there is.
            Err(_) => vector_core::blossom_info::cached(&server),
        };
        if info.map(|i| i.extensions.iter().any(|e| e == extension)).unwrap_or(false) {
            found = Some(server.trim_end_matches('/').to_string());
            break;
        }
    }
    match &found {
        Some(s) => log_debug!("[Proxy] {} offered by {}", extension, s),
        None => log_warn!("[Proxy] no configured Blossom server offers {}; falling back", extension),
    }
    if let Ok(mut picks) = PICKS.lock() {
        picks.retain(|p| p.extension != extension);
        picks.push(Pick { extension, server: found.clone(), at: Instant::now() });
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

fn proxy_url(server: &str, url: &str) -> String {
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
fn wants_proxy(url: &str) -> bool {
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
    let signer = vector_core::signer::active_signer().ok()?;
    let server_url = url::Url::parse(server).ok()?;
    vector_core::blossom_info::build_get_auth_header(&signer, &server_url, "Proxied fetch")
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

/// A page's metadata, as the message stores it: via Magnitude when the
/// setting is on, or fetched here when it is off.
pub async fn site_metadata(url: &str) -> Result<SiteMetadata, String> {
    if !enabled() {
        return crate::net::fetch_site_metadata(url).await;
    }
    let Some(server) = server_offering(UNFURL_EXT).await else {
        return Err("no configured server offers link previews; not fetching the page from this device".into());
    };
    unfurl_via(&server, url).await
}

async fn unfurl_via(server: &str, url: &str) -> Result<SiteMetadata, String> {
    let client = vector_core::net::build_http_client(Duration::from_secs(20))?;
    let response = client
        .get(format!("{server}/unfurl"))
        .query(&[("url", url)])
        .send()
        .await
        .map_err(|e| format!("unfurl request failed: {e}"))?;
    let status = response.status();
    let body: serde_json::Value = response.json().await.map_err(|e| format!("unfurl answer unreadable: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "unfurl refused: {} {}",
            body.get("error").and_then(|e| e.as_str()).unwrap_or("?"),
            body.get("message").and_then(|m| m.as_str()).unwrap_or("")
        ));
    }
    let proxy_server = server_offering(PROXY_EXT).await;
    Ok(from_answer(&body, url, proxy_server.as_deref()))
}

/// Magnitude's answer in the shape the message keeps and the UI reads.
/// Pictures are rewritten to the proxy where one is offered and dropped
/// where none is: a preview without a picture beats a picture fetched from
/// this device.
fn from_answer(body: &serde_json::Value, asked: &str, proxy_server: Option<&str>) -> SiteMetadata {
    let p = body.get("preview").cloned().unwrap_or(serde_json::Value::Null);
    let s = |k: &str| p.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let final_url = body.get("url").and_then(|u| u.as_str()).unwrap_or(asked).to_string();
    let domain = url::Url::parse(&final_url)
        .ok()
        .map(|u| format!("{}/", u.origin().ascii_serialization()))
        .unwrap_or_else(|| final_url.clone());
    let via_proxy = |u: Option<String>| -> Option<String> {
        let u = u?;
        match proxy_server {
            Some(srv) if wants_proxy(&u) => Some(proxy_url(srv, &u)),
            Some(_) => Some(u),
            None => None,
        }
    };
    let image = p.get("image").and_then(|i| i.get("url")).and_then(|u| u.as_str()).map(str::to_string);
    SiteMetadata {
        domain,
        og_title: s("title"),
        og_description: s("description"),
        og_image: via_proxy(image),
        og_url: s("canonical").or(Some(final_url)),
        og_type: s("kind"),
        title: s("title"),
        description: s("description"),
        favicon: via_proxy(s("favicon")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_becomes_message_metadata_with_pictures_via_the_proxy() {
        let body: serde_json::Value = serde_json::from_str(r#"{
            "url": "https://example.com/posts/1", "cached": false,
            "preview": {"title": "T", "description": "D", "kind": "article",
                        "image": {"url": "https://cdn.example.net/a.jpg", "width": 10},
                        "favicon": "https://example.com/favicon.ico",
                        "canonical": "https://example.com/posts/1?ref=x"}}"#).unwrap();
        let m = from_answer(&body, "https://example.com/posts/1", Some("https://magnitude.example"));
        assert_eq!(m.og_title.as_deref(), Some("T"));
        assert_eq!(m.domain, "https://example.com/");
        assert_eq!(m.og_type.as_deref(), Some("article"));
        assert_eq!(m.og_url.as_deref(), Some("https://example.com/posts/1?ref=x"));
        assert_eq!(
            m.og_image.as_deref(),
            Some("https://magnitude.example/proxy?url=https%3A%2F%2Fcdn.example.net%2Fa.jpg")
        );
        assert!(m.favicon.as_deref().unwrap().starts_with("https://magnitude.example/proxy?url="));

        // No proxy offered: the picture is dropped rather than fetched here.
        let m = from_answer(&body, "https://example.com/posts/1", None);
        assert!(m.og_image.is_none() && m.favicon.is_none());
        assert_eq!(m.og_title.as_deref(), Some("T"));
    }

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
