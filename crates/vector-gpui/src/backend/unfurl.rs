//! Link previews (`fetch_msg_metadata`): through the user's Magnitude server when the privacy
//! setting is on; off it, or off Clearnet, the page's `<head>` is read here.

use serde_json::{Value, json};
use vector_core::types::SiteMetadata;
use vector_core::{Message, STATE, VectorCore, db, proxy};

#[path = "../../../../src-tauri/src/simd/html_meta.rs"]
#[allow(dead_code, unsafe_op_in_unsafe_fn)]
mod html_meta;

const INVITE_PREFIXES: [&str; 2] = ["https://vectorapp.io/invite", "https://www.vectorapp.io/invite"];

fn extract_https_urls(text: &str) -> Vec<String> {
    const DELIMITERS: &[u8] = b" \t\n\r\"<>)]}|";
    let mut urls = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find("https://") {
        let start = from + i;
        let rest = text[start..].as_bytes();
        let mut end = rest.iter().position(|b| DELIMITERS.contains(b)).unwrap_or(rest.len());
        while end > 0 && matches!(rest[end - 1], b'.' | b',' | b':' | b';') {
            end -= 1;
        }
        if end > "https://".len() {
            urls.push(text[start..start + end].to_string());
        }
        from = start + 1;
    }
    urls
}

fn off_clearnet() -> bool {
    vector_core::transport::preference().is_some_and(|k| k != vector_core::transport::Kind::Clearnet)
}

async fn site_metadata(url: &str) -> Result<SiteMetadata, String> {
    // A network-native page is reached inside its network; Magnitude could only fail on it.
    let native = reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(proxy::is_network_native)).unwrap_or(false);
    if !proxy::enabled() || native {
        return fetch_head(url, true).await;
    }
    match proxy::server_offering(proxy::UNFURL_EXT).await {
        Some(server) => unfurl_via(&server, url).await,
        // Allowed only because the address is hidden: a switch to Clearnet meanwhile fails it.
        None if off_clearnet() => fetch_head(url, false).await,
        None => Err("no configured server offers link previews".into()),
    }
}

async fn unfurl_via(server: &str, url: &str) -> Result<SiteMetadata, String> {
    let client = vector_core::net::build_http_client(std::time::Duration::from_secs(20))?;
    let endpoint = reqwest::Url::parse_with_params(&format!("{server}/unfurl"), [("url", url)]).map_err(|e| e.to_string())?;
    let response = client.get(endpoint).send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    let body: Value = response.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("unfurl refused: {status}"));
    }
    let proxy_server = proxy::server_offering(proxy::PROXY_EXT).await;
    Ok(from_answer(&body, url, proxy_server.as_deref()))
}

/// Magnitude's answer as the message keeps it; a picture with no proxy to fetch it is dropped.
fn from_answer(body: &Value, asked: &str, proxy_server: Option<&str>) -> SiteMetadata {
    let p = body.get("preview").cloned().unwrap_or(Value::Null);
    let s = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_string);
    let final_url = body.get("url").and_then(Value::as_str).unwrap_or(asked).to_string();
    let domain = reqwest::Url::parse(&final_url).map(|u| format!("{}/", u.origin().ascii_serialization())).unwrap_or_else(|_| final_url.clone());
    let via_proxy = |u: Option<String>| -> Option<String> {
        let u = u?;
        match proxy_server {
            Some(srv) if proxy::wants_proxy(&u) => Some(proxy::proxy_url(srv, &u)),
            Some(_) => Some(u),
            None => None,
        }
    };
    let image = p.get("image").and_then(|i| i.get("url")).and_then(Value::as_str).map(str::to_string);
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

fn client(timeout: u64, direct: bool) -> Result<vector_core::net::HttpClient, String> {
    let c = vector_core::net::build_http_client(std::time::Duration::from_secs(timeout))?;
    Ok(if direct { c } else { c.without_direct() })
}

/// http upgraded, protocol- and path-relative resolved against the page's origin.
fn normalize_url(url: &str, domain: &str) -> String {
    if url.starts_with("https://") {
        url.to_string()
    } else if let Some(rest) = url.strip_prefix("http://") {
        format!("https://{rest}")
    } else if url.starts_with("//") {
        format!("https:{url}")
    } else {
        let base = domain.trim_end_matches('/');
        if url.starts_with('/') { format!("{base}{url}") } else { format!("{base}/{url}") }
    }
}

async fn fetch_twitter(url: &str, direct: bool) -> Result<SiteMetadata, String> {
    let encoded = url.replace('&', "%26").replace('?', "%3F").replace('=', "%3D");
    let response = client(10, direct)?.get(format!("https://publish.twitter.com/oembed?url={encoded}")).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("oEmbed: {}", response.status()));
    }
    let data: Value = response.json().await.map_err(|e| e.to_string())?;
    let author = data["author_name"].as_str().unwrap_or("Twitter");
    let text = html_meta::extract_first_p_inner_html(data["html"].as_str().unwrap_or(""))
        .map(|h| h.split("<a ").next().unwrap_or("").trim().to_string())
        .unwrap_or_default();
    Ok(SiteMetadata {
        domain: "https://x.com/".into(),
        og_title: Some(format!("{author} on X")),
        og_description: Some(text),
        og_image: data["thumbnail_url"].as_str().map(str::to_string),
        og_url: Some(url.to_string()),
        og_type: Some("article".into()),
        title: Some(format!("{author} on X")),
        description: Some(format!("Post by {author}")),
        favicon: Some("https://abs.twimg.com/favicons/twitter.3.ico".into()),
    })
}

/// The page's `<head>`, read up to its close and no further than 512 KB.
async fn fetch_head(url: &str, direct: bool) -> Result<SiteMetadata, String> {
    vector_core::net::validate_url_not_private(url).map_err(|e| e.to_string())?;
    if url.contains("twitter.com") || url.contains("x.com") {
        return fetch_twitter(url, direct).await;
    }
    let domain = match reqwest::Url::parse(url) {
        Ok(u) => format!("{}/", u.origin().ascii_serialization()),
        Err(_) => format!("{}/", url.trim_end_matches('/')),
    };
    const MAX_HEAD: usize = 512 * 1024;
    let mut response = client(15, direct)?.get(url).header("Range", "bytes=0-32768").send().await.map_err(|e| e.to_string())?;
    let mut html = Vec::new();
    while let Some(data) = response.chunk().await.map_err(|e| e.to_string())? {
        let from = html.len().saturating_sub(6);
        html.extend_from_slice(&data);
        if let Some(end) = html_meta::find_closing_head(&html, from) {
            html.truncate(end);
            break;
        }
        if html.len() >= MAX_HEAD {
            html.truncate(MAX_HEAD);
            break;
        }
    }
    let html = String::from_utf8_lossy(&html).into_owned();
    let parsed = html_meta::extract_html_meta(&html);
    let base = domain.trim_end_matches('/');
    let candidates: Vec<(String, &str)> = parsed.favicons.iter().map(|f| (normalize_url(&f.href, &domain), f.rel.as_ref())).collect();
    let favicon = candidates
        .iter()
        .find(|(_, rel)| rel.eq_ignore_ascii_case("apple-touch-icon"))
        .or_else(|| candidates.iter().find(|(u, _)| u.ends_with(".png")))
        .or_else(|| candidates.iter().find(|(_, rel)| rel.eq_ignore_ascii_case("icon") || rel.eq_ignore_ascii_case("shortcut icon")))
        .map(|(u, _)| u.clone())
        .unwrap_or_else(|| format!("{base}/favicon.ico"));
    Ok(SiteMetadata {
        domain: domain.clone(),
        og_title: parsed.og_title.map(|c| c.into_owned()),
        og_description: parsed.og_description.map(|c| c.into_owned()),
        og_image: parsed.og_image.map(|u| normalize_url(&u, &domain)),
        og_url: parsed.og_url.map(|u| normalize_url(&u, &domain)).or(Some(url.to_string())),
        og_type: parsed.og_type.map(|c| c.into_owned()),
        title: parsed.title.map(|c| c.into_owned()),
        description: parsed.description.map(|c| c.into_owned()),
        favicon: Some(favicon),
    })
}

/// The first of a message's first three links that has a card, stored on the message.
pub async fn fetch_msg_metadata(chat_id: String, msg_id: String) -> bool {
    let Some((_, message)) = VectorCore.get_message(&msg_id).await else { return false };
    for url in extract_https_urls(&vector_core::net::strip_md_link_claims(&message.content)).into_iter().take(3) {
        if INVITE_PREFIXES.iter().any(|p| url.starts_with(p)) || vector_core::golink::is_go_url(&url) {
            continue;
        }
        // A link to a Nostr post, article or video gets its own card from relays.
        if vector_core::nostr_embed::EmbedRef::from_url(&url).is_some() {
            continue;
        }
        let Ok(metadata) = site_metadata(&url).await else { continue };
        let has_content = metadata.og_title.is_some()
            || metadata.og_description.is_some()
            || metadata.og_image.is_some()
            || metadata.title.is_some()
            || metadata.description.is_some();
        if !has_content {
            continue;
        }
        let resident = STATE.lock().await.update_message_in_chat(&chat_id, &msg_id, |m| m.set_preview_metadata(Some(metadata.clone())));
        let msg = resident.unwrap_or_else(|| Message { preview_metadata: Some(metadata), ..message.clone() });
        vector_core::emit_event("message_update", &json!({ "old_id": msg_id, "message": msg, "chat_id": chat_id }));
        let _ = db::events::save_message(&chat_id, &msg).await;
        return true;
    }
    false
}
