//! Twin checks and NIP-11 lookups, through the network in use. A twin is checked by a full TLS
//! handshake for the server's own name through that network: one that can't present the
//! certificate is never used. Both run only while the network is ready, so neither ever leaves
//! on another one.

use std::sync::Arc;
use std::time::Duration;

use super::aliases::{self, AliasCheck, AliasEntry, CheckState};
use super::host::ActiveTransport;
use super::{ConnectError, ExitPolicy, Kind, Lane, Route};

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

/// What a check of a stream to the twin found: it serves `host` (Ok), doesn't (Failed), or
/// couldn't be reached (Unchecked, with the reason; tried again on the next Ready).
pub async fn tls_check<S>(host: &str, stream: Result<S, ConnectError>) -> AliasCheck
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let unchecked = |text: String| AliasCheck { state: CheckState::Unchecked, at: None, text: Some(text) };
    let stream = match stream {
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
            crate::log_debug!("[Twins] check for {host}: {e}");
            AliasCheck { state: CheckState::Failed, at: None, text: Some(not_served_text(host)) }
        }
        Err(_) => unchecked(ConnectError::Timeout.text()),
    }
}

/// `via:443` through `a` on the Shared lane, then [`tls_check`] for `host`.
pub async fn probe(a: &ActiveTransport, host: &str, via: &str) -> AliasCheck {
    let route = Route::Twin { host: host.to_string(), via: via.to_string(), port: 443 };
    tls_check(host, a.transport().dial(&route, Lane::Shared).await.map(|(s, _)| s)).await
}

/// The live account's network, ready, or the reason no twin can be checked now.
fn ready_active() -> Result<Arc<ActiveTransport>, String> {
    let chosen = super::preference().filter(|k| *k != Kind::Clearnet);
    match (chosen, super::host::active()) {
        (Some(k), Some(a)) if a.kind == k && a.serves_live() && a.transport().ready() => Ok(a),
        (Some(k), _) => Err(format!("Connect to {} to check this address.", k.label())),
        (None, _) => Err("Use Tor or I2P to check this address.".into()),
    }
}

/// Check `host`'s twin on the network in use now, and record the result.
pub async fn check_alias(host: &str) -> Result<AliasEntry, String> {
    let host = aliases::normalize_host(host)?;
    let a = ready_active()?;
    let missing = || format!("This server has no {} yet.", a.kind.address_noun());
    let via = aliases::table().get(&host).and_then(|e| e.twins.get(&a.kind).cloned()).ok_or_else(missing)?;
    let check = probe(&a, &host, &via).await;
    aliases::record_check_for(&host, a.kind, &via, check)?;
    aliases::table().get(&host).cloned().ok_or_else(missing)
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct FindAlias {
    pub found: Option<String>,
    pub check: Option<AliasCheck>,
    pub text: String,
}

/// The relay's own NIP-11 `privacy_addresses`, then a check of the first one on the network in
/// use. Nothing is saved: the user decides.
pub async fn find_alias(url: &str) -> Result<FindAlias, String> {
    let host = aliases::normalize_host(url)?;
    let a = ready_active()?;
    if a.kind == Kind::I2p {
        let cfg = super::prefs::current().config(Kind::I2p);
        if cfg.downcast_ref::<super::i2p_config::I2pConfig>().is_some_and(|c| c.exit == ExitPolicy::Off) {
            return Err("Can't look it up in I2P-Only mode.".into());
        }
    }
    let noun = a.kind.address_noun();
    match find_privacy_address(&host, a.kind).await? {
        None => Ok(FindAlias { found: None, check: None, text: format!("This relay doesn't list an {noun}.") }),
        Some(addr) => {
            let mut check = probe(&a, &host, &addr).await;
            check.at = Some(now_secs());
            Ok(FindAlias { found: Some(addr), check: Some(check), text: format!("This relay lists an {noun}.") })
        }
    }
}

fn now_secs() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// GET the relay's NIP-11 document over the account's network and return the first
/// `privacy_addresses` entry inside `kind`.
pub async fn find_privacy_address(host: &str, kind: Kind) -> Result<Option<String>, String> {
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
    Ok(first_privacy_address(&doc, kind))
}

/// The first `privacy_addresses` entry whose host is `kind`'s own address (a `.b32.i2p`, a v3
/// `.onion`), as that host. Names need the router's address book, so they never count.
pub fn first_privacy_address(doc: &serde_json::Value, kind: Kind) -> Option<String> {
    let list = doc.get("privacy_addresses")?.as_array()?;
    list.iter().filter_map(|v| v.as_str()).find_map(|s| {
        let s = s.trim();
        let host = match url::Url::parse(s) {
            Ok(u) if u.host_str().is_some() => u.host_str().map(str::to_ascii_lowercase),
            _ => Some(s.split(['/', ':']).next().unwrap_or("").to_ascii_lowercase()),
        }?;
        let ours = match kind {
            Kind::I2p => super::route::is_b32(&host),
            Kind::Tor => super::route::is_onion(&host),
            Kind::Clearnet => false,
        };
        ours.then_some(host)
    })
}

/// Check every unchecked twin of the network in use, one at a time, while it stays ready for
/// the account on screen. Bound to that account's session, so a result never lands in another.
pub fn check_pending() {
    let Some(a) = super::host::active().filter(|a| a.serves_live() && a.transport().ready()) else { return };
    let session = crate::db::live_session();
    let pending: Vec<(String, String)> = super::prefs::of(&session)
        .aliases()
        .entries()
        .into_iter()
        .filter(|e| e.check.state == CheckState::Unchecked)
        .filter_map(|e| e.twins.get(&a.kind).cloned().map(|v| (e.host, v)))
        .collect();
    if pending.is_empty() {
        return;
    }
    // spawn-detached: pinned to the owner's session by with_session; it checks only that account's twins.
    drop(super::spawn_on(crate::db::with_session(session, async move {
        for (host, via) in pending {
            if !a.serves_live() || !a.transport().ready() {
                return;
            }
            let check = probe(&a, &host, &via).await;
            if check.state == CheckState::Unchecked {
                continue;
            }
            if let Err(e) = aliases::record_check_for(&host, a.kind, &via, check) {
                crate::log_warn!("[Twins] could not save a twin check: {e}");
            }
        }
    })));
}
