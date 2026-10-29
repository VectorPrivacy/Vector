//! What Vector shows of a relay: its NIP-11 name and icon, and whether it asks clients to AUTH.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;

#[derive(serde::Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct RelayInfo {
    pub name: Option<String>,
    /// An `https` URL, for the backend image cache: the webview never loads it itself.
    pub icon: Option<String>,
    /// The relay says it requires NIP-42 AUTH, or it has challenged Vector since launch.
    pub auth: bool,
}

const FRESH: Duration = Duration::from_secs(60 * 60);
const FAILED_FRESH: Duration = Duration::from_secs(5 * 60);
const DOC_MAX_BYTES: usize = 64 * 1024;
const NAME_MAX_CHARS: usize = 48;
const ICON_MAX_BYTES: usize = 2048;

struct Memo;
type Entries = Mutex<HashMap<String, (RelayInfo, web_time::Instant, Duration)>>;

fn memo() -> Arc<Entries> {
    crate::db::current_session().scoped::<Memo, _>()
}

/// The relay's info, fetched over the network in use at most once an hour per account. A relay
/// that serves no document still reports a challenge it sent since launch.
pub async fn relay_info(relay_url: &str) -> Result<RelayInfo, String> {
    let url = crate::transport::validate_relay_url(relay_url)?;
    let key = crate::inbox_relays::normalize_relay_url(&url);
    let cached = memo().lock().unwrap_or_else(|e| e.into_inner()).get(&key).filter(|(_, at, fresh)| at.elapsed() < *fresh).map(|(i, ..)| i.clone());
    let mut info = match cached {
        Some(info) => info,
        None => {
            let doc = fetch_document(&url).await;
            let fresh = if doc.is_some() { FRESH } else { FAILED_FRESH };
            let info = doc.as_ref().map(from_document).unwrap_or_default();
            memo().lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone(), (info.clone(), web_time::Instant::now(), fresh));
            info
        }
    };
    info.auth |= crate::community::v2::streamauth::challenged(&url);
    Ok(info)
}

async fn fetch_document(relay_url: &str) -> Option<serde_json::Value> {
    let url = crate::transport::nip11_url(relay_url).ok()?;
    crate::net::validate_url_not_private(&url).ok()?;
    let timeout = crate::transport::budget(crate::transport::budget::Op::HttpTotal, Duration::from_secs(10));
    let client = crate::net::build_http_client(timeout).ok()?;
    let resp = client.get(url).header("Accept", "application/nostr+json").send().await.ok()?;
    if !resp.status().is_success() || resp.content_length().is_some_and(|n| n > DOC_MAX_BYTES as u64) {
        return None;
    }
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk.ok()?);
        if body.len() > DOC_MAX_BYTES {
            return None;
        }
    }
    serde_json::from_slice(&body).ok()
}

/// The parts of a NIP-11 document Vector shows, each dropped when it isn't what it should be.
pub fn from_document(doc: &serde_json::Value) -> RelayInfo {
    let name = doc.get("name").and_then(|v| v.as_str()).map(clean_name).filter(|n| !n.is_empty());
    let icon = doc
        .get("icon")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| s.len() <= ICON_MAX_BYTES)
        .and_then(|s| url::Url::parse(s).ok())
        .filter(|u| u.scheme() == "https" && u.host_str().is_some())
        .map(String::from);
    let auth = doc.pointer("/limitation/auth_required").and_then(|v| v.as_bool()) == Some(true);
    RelayInfo { name, icon, auth }
}

/// One line of at most [`NAME_MAX_CHARS`]: control and format characters go, runs of spaces close up.
fn clean_name(raw: &str) -> String {
    let mut out = String::new();
    for word in raw.split(|c: char| c.is_whitespace() || c.is_control()).filter(|w| !w.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.extend(word.chars().filter(|c| !is_format(*c)));
    }
    if out.chars().count() > NAME_MAX_CHARS {
        out = out.chars().take(NAME_MAX_CHARS - 1).collect::<String>().trim_end().to_string() + "…";
    }
    out
}

/// Bidi overrides and zero-width characters: they can make a name read as something else.
fn is_format(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_document_gives_its_name_icon_and_auth_requirement() {
        let doc = json!({ "name": "  Vector\tRelay \n", "icon": "https://relay.example/icon.png", "limitation": { "auth_required": true } });
        assert_eq!(from_document(&doc), RelayInfo { name: Some("Vector Relay".into()), icon: Some("https://relay.example/icon.png".into()), auth: true });
    }

    #[test]
    fn what_a_document_gets_wrong_is_left_out() {
        for icon in ["http://relay.example/icon.png", "data:image/png;base64,AAAA", "javascript:alert(1)", "not a url"] {
            assert_eq!(from_document(&json!({ "icon": icon })).icon, None, "{icon}");
        }
        assert_eq!(from_document(&json!({ "icon": format!("https://relay.example/{}", "a".repeat(ICON_MAX_BYTES)) })).icon, None);
        assert_eq!(from_document(&json!({ "name": " \u{202E}\u{200B} " })).name, None);
        assert!(!from_document(&json!({ "limitation": { "auth_required": "yes" } })).auth);
        assert_eq!(from_document(&json!(["not", "an", "object"])), RelayInfo::default());
    }

    #[test]
    fn a_name_is_one_short_line_that_reads_as_itself() {
        assert_eq!(clean_name("Evil\u{202E}gnp.exe"), "Evilgnp.exe");
        assert_eq!(clean_name("a\u{0}b\r\nc"), "a b c");
        let long = clean_name(&"x".repeat(200));
        assert_eq!(long.chars().count(), NAME_MAX_CHARS);
        assert!(long.ends_with('…'));
    }

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        use nostr_sdk::prelude::ToBech32;
        let tmp = tempfile::tempdir().unwrap();
        let account = nostr_sdk::prelude::Keys::generate().public_key().to_bech32().unwrap();
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard)
    }

    #[tokio::test]
    async fn auth_shows_for_a_live_challenge_never_for_one_recorded_long_ago() {
        let (_tmp, _guard) = init_test_db();
        let url = "wss://quiet.relay.invalid";
        crate::db::set_sql_setting(format!("auth_gate:{}", crate::inbox_relays::normalize_relay_url(url)), "1".into()).unwrap();
        assert!(!relay_info(url).await.unwrap().auth, "a relay that challenged under other software once is not AUTH now");
        crate::community::v2::streamauth::note_challenge(&format!("{url}/"));
        assert!(relay_info(url).await.unwrap().auth, "a cached answer still reads the live challenge");
        assert!(relay_info("https://not.a.relay").await.is_err());
    }
}
