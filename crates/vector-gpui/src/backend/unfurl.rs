//! Link previews (`fetch_msg_metadata`): through the user's Magnitude server when the privacy
//! setting is on; off it, or off Clearnet, the page's `<head>` is read here.

use serde_json::{Value, json};
use vector_core::types::SiteMetadata;
use vector_core::{Message, STATE, VectorCore, db, proxy};

#[path = "../../../../src-tauri/src/simd/html_meta.rs"]
#[allow(dead_code, unsafe_op_in_unsafe_fn)]
mod html_meta;

const INVITE_PREFIXES: [&str; 2] = [
    "https://vectorapp.io/invite",
    "https://www.vectorapp.io/invite",
];

/// Links in the order they start, overlapping ones included, read lazily: a message of
/// nothing but `https://` names one link per byte, each running to its end.
fn https_urls(text: &str) -> impl Iterator<Item = &str> {
    const DELIMITERS: &[u8] = b" \t\n\r\"<>)]}|";
    let mut from = 0;
    std::iter::from_fn(move || loop {
        let start = from + text.get(from..)?.find("https://")?;
        from = start + 1;
        let rest = &text.as_bytes()[start..];
        let mut end = rest
            .iter()
            .position(|b| DELIMITERS.contains(b))
            .unwrap_or(rest.len());
        while end > 0 && matches!(rest[end - 1], b'.' | b',' | b':' | b';') {
            end -= 1;
        }
        if end > "https://".len() {
            return Some(&text[start..start + end]);
        }
    })
}

fn off_clearnet() -> bool {
    vector_core::transport::preference()
        .is_some_and(|k| k != vector_core::transport::Kind::Clearnet)
}

async fn site_metadata(url: &str) -> Result<SiteMetadata, String> {
    // A network-native page is reached inside its network; Magnitude could only fail on it.
    let native = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(proxy::is_network_native))
        .unwrap_or(false);
    if !proxy::enabled() || native {
        return fetch_head(url, true).await;
    }
    match proxy::server_offering(proxy::UNFURL_EXT).await {
        Some(server) => unfurl_via(&server, url).await,
        // Allowed only because the address is hidden: a switch to Clearnet meanwhile fails it.
        None if off_clearnet() => fetch_head(url, false).await,
        None => Err("no configured server offers link previews".into()), // i18n-exempt: developer error
    }
}

async fn unfurl_via(server: &str, url: &str) -> Result<SiteMetadata, String> {
    let client = vector_core::net::build_http_client(std::time::Duration::from_secs(20))?;
    let endpoint = reqwest::Url::parse_with_params(&format!("{server}/unfurl"), [("url", url)])
        .map_err(|e| e.to_string())?;
    let response = client
        .get(endpoint)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status();
    let body: Value = response.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("unfurl refused: {status}")); // i18n-exempt: developer error
    }
    let proxy_server = proxy::server_offering(proxy::PROXY_EXT).await;
    Ok(from_answer(&body, url, proxy_server.as_deref()))
}

/// Magnitude's answer as the message keeps it; a picture with no proxy to fetch it is dropped.
fn from_answer(body: &Value, asked: &str, proxy_server: Option<&str>) -> SiteMetadata {
    let p = body.get("preview").cloned().unwrap_or(Value::Null);
    let s = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_string);
    let final_url = body
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or(asked)
        .to_string();
    let domain = reqwest::Url::parse(&final_url)
        .map(|u| format!("{}/", u.origin().ascii_serialization()))
        .unwrap_or_else(|_| final_url.clone());
    let via_proxy = |u: Option<String>| -> Option<String> {
        let u = u?;
        match proxy_server {
            Some(srv) if proxy::wants_proxy(&u) => Some(proxy::proxy_url(srv, &u)),
            Some(_) => Some(u),
            None => None,
        }
    };
    let image = p
        .get("image")
        .and_then(|i| i.get("url"))
        .and_then(Value::as_str)
        .map(str::to_string);
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
        if url.starts_with('/') {
            format!("{base}{url}")
        } else {
            format!("{base}/{url}")
        }
    }
}

async fn fetch_twitter(url: &str, direct: bool) -> Result<SiteMetadata, String> {
    let encoded = url
        .replace('&', "%26")
        .replace('?', "%3F")
        .replace('=', "%3D");
    let response = client(10, direct)?
        .get(format!("https://publish.twitter.com/oembed?url={encoded}"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("oEmbed: {}", response.status())); // i18n-exempt: developer error
    }
    let data: Value = response.json().await.map_err(|e| e.to_string())?;
    let author = data["author_name"].as_str().unwrap_or("Twitter"); // i18n-exempt: stored link preview metadata, as Tauri writes it
    let text = html_meta::extract_first_p_inner_html(data["html"].as_str().unwrap_or(""))
        .map(|h| h.split("<a ").next().unwrap_or("").trim().to_string())
        .unwrap_or_default();
    Ok(SiteMetadata {
        domain: "https://x.com/".into(),
        og_title: Some(format!("{author} on X")), // i18n-exempt: stored link preview metadata, as Tauri writes it
        og_description: Some(text),
        og_image: data["thumbnail_url"].as_str().map(str::to_string),
        og_url: Some(url.to_string()),
        og_type: Some("article".into()),
        title: Some(format!("{author} on X")), // i18n-exempt: stored link preview metadata, as Tauri writes it
        description: Some(format!("Post by {author}")), // i18n-exempt: stored link preview metadata, as Tauri writes it
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
    let mut response = client(15, direct)?
        .get(url)
        .header("Range", "bytes=0-32768") // i18n-exempt: HTTP header
        .send()
        .await
        .map_err(|e| e.to_string())?;
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
    let candidates: Vec<(String, &str)> = parsed
        .favicons
        .iter()
        .map(|f| (normalize_url(&f.href, &domain), f.rel.as_ref()))
        .collect();
    let favicon = candidates
        .iter()
        .find(|(_, rel)| rel.eq_ignore_ascii_case("apple-touch-icon"))
        .or_else(|| candidates.iter().find(|(u, _)| u.ends_with(".png")))
        .or_else(|| {
            candidates.iter().find(|(_, rel)| {
                rel.eq_ignore_ascii_case("icon") || rel.eq_ignore_ascii_case("shortcut icon") // i18n-exempt: HTML attribute value
            })
        })
        .map(|(u, _)| u.clone())
        .unwrap_or_else(|| format!("{base}/favicon.ico"));
    Ok(SiteMetadata {
        domain: domain.clone(),
        og_title: parsed.og_title.map(|c| c.into_owned()),
        og_description: parsed.og_description.map(|c| c.into_owned()),
        og_image: parsed.og_image.map(|u| normalize_url(&u, &domain)),
        og_url: parsed
            .og_url
            .map(|u| normalize_url(&u, &domain))
            .or(Some(url.to_string())),
        og_type: parsed.og_type.map(|c| c.into_owned()),
        title: parsed.title.map(|c| c.into_owned()),
        description: parsed.description.map(|c| c.into_owned()),
        favicon: Some(favicon),
    })
}

/// The first of a message's first three links that has a card, stored on the message.
pub async fn fetch_msg_metadata(chat_id: String, msg_id: String) -> bool {
    let Some((_, message)) = VectorCore.get_message(&msg_id).await else {
        return false;
    };
    let text = vector_core::net::strip_md_link_claims(&message.content);
    for url in https_urls(&text).take(3) {
        if INVITE_PREFIXES.iter().any(|p| url.starts_with(p))
            || vector_core::golink::is_go_url(url)
        {
            continue;
        }
        // A link to a Nostr post, article or video gets its own card from relays.
        if vector_core::nostr_embed::EmbedRef::from_url(url).is_some() {
            continue;
        }
        // A video file plays in its own card; its "page" is only a file name.
        if crate::timeline::is_video_url(url) {
            continue;
        }
        let Ok(metadata) = site_metadata(url).await else {
            continue;
        };
        let has_content = metadata.og_title.is_some()
            || metadata.og_description.is_some()
            || metadata.og_image.is_some()
            || metadata.title.is_some()
            || metadata.description.is_some();
        if !has_content {
            continue;
        }
        let resident = STATE
            .lock()
            .await
            .update_message_in_chat(&chat_id, &msg_id, |m| {
                m.set_preview_metadata(Some(metadata.clone()))
            });
        let msg = resident.unwrap_or_else(|| Message {
            preview_metadata: Some(metadata),
            ..message.clone()
        });
        vector_core::emit_event(
            "message_update",
            &json!({ "old_id": msg_id, "message": msg, "chat_id": chat_id }),
        );
        let _ = db::events::save_message(&chat_id, &msg).await;
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::https_urls;

    /// The scan as it was: every link collected before any is used.
    fn eager(text: &str) -> Vec<String> {
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

    #[test]
    fn lazy_links_are_the_links_found_eagerly() {
        const PARTS: &[&str] = &["https://", "http://", "a.io", "/x", " ", "\n", ".", ",", ":", ";", ")", "]", "|", "\"", "<", "é", "日本", "[a](", "https://b.c/https://d.e"];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let text: String = (0..next() % 24).map(|_| PARTS[(next() % PARTS.len() as u64) as usize]).collect();
            let lazy: Vec<&str> = https_urls(&text).collect();
            assert_eq!(lazy, eager(&text), "{text:?}");
        }
    }

    /// The scanner hands out `&str`s cut from bytes unchecked: every cut must land on a character.
    #[test]
    fn page_heads_of_any_shape_scan_to_whole_characters() {
        use super::html_meta::{extract_first_p_inner_html, extract_html_meta};
        const PARTS: &[&str] = &[
            "<", ">", "/", "<meta", "<link", "<title>", "</title>", "</head>", "<p>", "</p>", "<!--", "-->", " ", "\n", "=", "\"", "'",
            "property=", "name=", "content=", "rel=", "href=", "og:title", "og:image", "description", "icon", "apple-touch-icon",
            "&amp;", "&#x1F600;", "&#128512;", "&#xD800;", "&#99999999;", "&#x;", "&", ";", "é", "日本語", "😀", "\u{301}", "a",
        ];
        let ok = |c: &Option<std::borrow::Cow<str>>| c.as_ref().is_none_or(|s| std::str::from_utf8(s.as_bytes()).is_ok());
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let html: String = (0..next() % 48).map(|_| PARTS[(next() % PARTS.len() as u64) as usize]).collect();
            let m = extract_html_meta(&html);
            for f in [&m.og_title, &m.og_description, &m.og_image, &m.og_url, &m.og_type, &m.title, &m.description] {
                assert!(ok(f), "{html:?}");
            }
            assert!(m.favicons.iter().all(|f| std::str::from_utf8(f.href.as_bytes()).is_ok() && std::str::from_utf8(f.rel.as_bytes()).is_ok()), "{html:?}");
            assert!(ok(&extract_first_p_inner_html(&html)), "{html:?}");
        }
    }

    #[test]
    fn a_message_of_bare_schemes_reads_only_the_links_it_uses() {
        let text = "https://".repeat(20_000);
        let t = std::time::Instant::now();
        assert_eq!(https_urls(&text).take(3).count(), 3);
        assert!(t.elapsed() < std::time::Duration::from_secs(1), "{:?}", t.elapsed());
    }
}
