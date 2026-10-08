//! Twin checks and NIP-11 lookups. A twin is checked by a full TLS handshake for the server's
//! own name over the I2P stream: a twin that can't present that certificate is never used.
//! Both run only while I2P is ready, so neither ever leaves on another network.

use std::sync::Arc;
use std::time::Duration;

use crate::transport::aliases::{self, AliasCheck, AliasEntry, CheckState};
use crate::transport::i2p_config::I2pConfig;
use crate::transport::{ConnectError, ExitPolicy, Kind, Lane};

use super::{I2pTransport, Inner};

const TLS_DEADLINE: Duration = Duration::from_secs(60);
const NIP11_TIMEOUT: Duration = Duration::from_secs(60);

fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

pub fn served_text(host: &str) -> String {
    format!("Checked. It serves {host}.")
}

pub fn not_served_text(host: &str) -> String {
    format!("This address doesn't serve {host}. Vector won't use it.")
}

/// What a check found: the twin serves `host` (Ok), doesn't (Failed), or couldn't be reached
/// (Unchecked, with the reason; tried again on the next Ready).
pub(crate) async fn probe_inner(inner: &Inner, host: &str, via: &str) -> AliasCheck {
    let unchecked = |text: String| AliasCheck { state: CheckState::Unchecked, at: None, text: Some(text) };
    let Some(nick) = inner.nick(Lane::Shared) else {
        return unchecked(ConnectError::NotReady(Some(Kind::I2p)).text());
    };
    let stream = match inner.open(&nick, via, 443).await {
        Ok(s) => s,
        Err(e) => return unchecked(e.text()),
    };
    let Ok(name) = rustls::pki_types::ServerName::try_from(host.to_string()) else {
        return AliasCheck { state: CheckState::Failed, at: None, text: Some(not_served_text(host)) };
    };
    let config = match tls_config() {
        Ok(c) => c,
        Err(e) => return unchecked(e),
    };
    let connector = tokio_rustls::TlsConnector::from(config);
    match tokio::time::timeout(TLS_DEADLINE, connector.connect(name, stream)).await {
        Ok(Ok(_)) => AliasCheck { state: CheckState::Ok, at: None, text: Some(served_text(host)) },
        Ok(Err(e)) => {
            crate::log_debug!("[I2P] twin check for {host}: {e}");
            AliasCheck { state: CheckState::Failed, at: None, text: Some(not_served_text(host)) }
        }
        Err(_) => unchecked(ConnectError::Timeout.text()),
    }
}

/// Stream to `via:443` on the Shared session, then a TLS handshake for `host`. Ok means the
/// twin serves `host`'s certificate.
pub async fn probe_twin(t: &I2pTransport, host: &str, via: &str) -> Result<(), String> {
    let check = probe_inner(&t.inner, host, via).await;
    match check.state {
        CheckState::Ok => Ok(()),
        _ => Err(check.text.unwrap_or_else(|| not_served_text(host))),
    }
}

/// The live account's I2P instance, ready, or the reason it can't check anything now.
fn ready_instance() -> Result<Arc<I2pTransport>, String> {
    match super::active() {
        Some(t) if t.inner.is_ready() && crate::transport::preference() == Some(Kind::I2p) => Ok(t),
        _ => Err("Connect to I2P to check this address.".into()),
    }
}

/// Check `host`'s I2P twin now and record the result.
pub async fn check_alias(host: &str) -> Result<AliasEntry, String> {
    let host = aliases::normalize_host(host)?;
    let entry = aliases::table().get(&host).cloned().ok_or_else(|| "This server has no I2P address yet.".to_string())?;
    let via = entry.twins.get(&Kind::I2p).cloned().ok_or_else(|| "This server has no I2P address yet.".to_string())?;
    let t = ready_instance()?;
    let check = probe_inner(&t.inner, &host, &via).await;
    aliases::record_check_for(&host, Kind::I2p, &via, check)?;
    aliases::table().get(&host).cloned().ok_or_else(|| "This server has no I2P address yet.".to_string())
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct FindAlias {
    pub found: Option<String>,
    pub check: Option<AliasCheck>,
    pub text: String,
}

/// The relay's own NIP-11 `privacy_addresses`, then a check of the first b32 it lists. Nothing
/// is saved: the user decides.
pub async fn find_alias(url: &str) -> Result<FindAlias, String> {
    let host = aliases::normalize_host(url)?;
    let t = ready_instance()?;
    let cfg = crate::transport::prefs::current().config(Kind::I2p).downcast_ref::<I2pConfig>().cloned().unwrap_or_default();
    if cfg.exit == ExitPolicy::Off {
        return Err("Can't look it up in I2P-Only mode.".into());
    }
    match find_privacy_address(&host).await? {
        None => Ok(FindAlias { found: None, check: None, text: "This relay doesn't list an I2P address.".into() }),
        Some(b32) => {
            let mut check = probe_inner(&t.inner, &host, &b32).await;
            check.at = Some(now_secs());
            Ok(FindAlias { found: Some(b32), check: Some(check), text: "This relay lists an I2P address.".into() })
        }
    }
}

fn now_secs() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// GET the relay's NIP-11 document over the account's network and return the first
/// `privacy_addresses` entry whose host is a `.b32.i2p` address.
pub async fn find_privacy_address(host: &str) -> Result<Option<String>, String> {
    let client = crate::net::build_http_client(NIP11_TIMEOUT)?;
    let resp = client
        .get(format!("https://{host}/"))
        .header("Accept", "application/nostr+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let doc: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(first_b32(&doc))
}

/// The first `privacy_addresses` entry that names a `.b32.i2p` host, as that host.
pub fn first_b32(doc: &serde_json::Value) -> Option<String> {
    let list = doc.get("privacy_addresses")?.as_array()?;
    list.iter().filter_map(|v| v.as_str()).find_map(|s| {
        let s = s.trim();
        let host = match url::Url::parse(s) {
            Ok(u) if u.host_str().is_some() => u.host_str().map(str::to_ascii_lowercase),
            _ => Some(s.split(['/', ':']).next().unwrap_or("").to_ascii_lowercase()),
        }?;
        crate::transport::route::is_b32(&host).then_some(host)
    })
}

/// Check every unchecked I2P twin of the owning account, one at a time, once its sessions are
/// up. Bound to that account's session, so a result never lands in another account.
pub(crate) fn check_pending_soon(inner: &Arc<Inner>) {
    if !inner.owner_live() {
        return;
    }
    let session = crate::db::live_session();
    if session.id() != inner.owner {
        return;
    }
    let pending: Vec<(String, String)> = crate::transport::prefs::of(&session)
        .aliases()
        .entries()
        .into_iter()
        .filter(|e| e.check.state == CheckState::Unchecked)
        .filter_map(|e| e.twins.get(&Kind::I2p).cloned().map(|v| (e.host, v)))
        .collect();
    if pending.is_empty() {
        return;
    }
    let inner = inner.clone();
    // spawn-detached: pinned to the owner's session by with_session; it checks only that account's twins.
    drop(crate::transport::spawn_on(crate::db::with_session(session, async move {
        for (host, via) in pending {
            if !inner.is_ready() || !inner.owner_live() {
                return;
            }
            let check = probe_inner(&inner, &host, &via).await;
            if check.state == CheckState::Unchecked {
                continue;
            }
            if let Err(e) = aliases::record_check_for(&host, Kind::I2p, &via, check) {
                crate::log_warn!("[I2P] could not save a twin check: {e}");
            }
        }
    })));
}

/// Check every unchecked twin now, when I2P is up for the account on screen (after the user
/// saves an address).
pub fn check_unchecked() {
    if let Some(t) = super::active().filter(|t| t.inner.is_ready()) {
        check_pending_soon(&t.inner);
    }
}
