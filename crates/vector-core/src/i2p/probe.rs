//! I2P's side of twin checks: the shared ones in `transport::twins`, plus a probe through an
//! instance that need not be the one installed.

use crate::transport::twins;
use crate::transport::{ConnectError, Kind, Lane};

pub use twins::{check_alias, find_alias, FindAlias};

use super::I2pTransport;

/// Stream to `via:443` on the Shared session, then a TLS handshake for `host`. Ok means the
/// twin serves `host`'s certificate.
pub async fn probe_twin(t: &I2pTransport, host: &str, via: &str) -> Result<(), String> {
    let stream = match t.inner.nick(Lane::Shared) {
        Some(nick) => t.inner.open(&nick, via, 443).await,
        None => Err(ConnectError::NotReady(Some(Kind::I2p))),
    };
    let check = twins::tls_check(host, stream).await;
    match check.state {
        crate::transport::aliases::CheckState::Ok => Ok(()),
        _ => Err(check.text.unwrap_or_else(|| twins::not_served_text(host))),
    }
}

/// The first `privacy_addresses` entry that names a `.b32.i2p` host, as that host.
pub fn first_b32(doc: &serde_json::Value) -> Option<String> {
    twins::first_privacy_address(doc, Kind::I2p)
}

/// Check every unchecked twin now, when I2P is up for the account on screen.
pub fn check_unchecked() {
    twins::check_pending();
}
