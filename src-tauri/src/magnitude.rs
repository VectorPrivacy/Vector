//! Link previews through the user's Magnitude server. The proxy itself
//! lives in `vector_core::proxy` so every crate fetches through it; this
//! module re-exports it and adds the preview side, which needs the message
//! metadata shape.
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

use std::time::Duration;

use vector_core::types::SiteMetadata;
pub use vector_core::proxy::{enabled, forget_picks, proxy_url, server_offering, wants_proxy, PROXY_EXT, SETTING_KEY, UNFURL_EXT};

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

}
