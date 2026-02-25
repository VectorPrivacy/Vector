//! A dummy TCP listener that does nothing except occupy a port.
//!
//! This TCP listener is to be used as a proxy for Mini App windows
//! where we need to ensure network isolation.
//! Setting an invalid proxy should make the browser unable to perform requests,
//! at least to some degree.
//!
//! Providing just any address (e.g. `127.0.0.1:123`)
//! where we don't expect a valid proxy is not good, because:
//! 1. Theoretically there could actually be a proxy listening there.
//! 2. If something is listening on that port, even if that is not a valid proxy,
//!    we still don't want to direct any traffic there.
//!    This could be classified as the "confused deputy" problem
//!    (CWE-441: Unintended Proxy or Intermediary).
//!
//! So we need to occupy the port ourselves.
//!
//! Note: This is only used on non-macOS platforms. On macOS, we skip the proxy
//! because it increases the minimum macOS version requirement to 14.

// Only used on Linux (not macOS due to version requirements, not Windows due to WebView2 freeze, not Android)
#[cfg(all(not(target_os = "macos"), not(target_os = "windows"), not(target_os = "android")))]
use std::net::{SocketAddr, SocketAddrV4, TcpListener};
#[cfg(all(not(target_os = "macos"), not(target_os = "windows"), not(target_os = "android")))]
use std::str::FromStr;

#[cfg(all(not(target_os = "macos"), not(target_os = "windows"), not(target_os = "android")))]
use anyhow::{anyhow, Context};
#[cfg(all(not(target_os = "macos"), not(target_os = "windows"), not(target_os = "android")))]


#[cfg(all(not(target_os = "macos"), not(target_os = "windows"), not(target_os = "android")))]
use std::sync::LazyLock;
#[cfg(all(not(target_os = "macos"), not(target_os = "windows"), not(target_os = "android")))]
use url::Url;

/// The URL is in the form of `socks5://127.0.0.1:54321`,
/// where only port is variable.
#[cfg(all(not(target_os = "macos"), not(target_os = "windows"), not(target_os = "android")))]
pub static DUMMY_LOCALHOST_PROXY_URL: LazyLock<Result<Url, ()>> = LazyLock::new(|| {
    DUMMY_LOCALHOST_PROXY_AND_URL
        .as_ref()
        .map(|(_listener, url)| url.clone())
        .map_err(|err| *err)
});

// Do _not_ try to simplify this to only store `Url` and not `TcpListener`,
// because dropping `TcpListener` will automatically cause it
// to stop listening.
#[cfg(all(not(target_os = "macos"), not(target_os = "windows"), not(target_os = "android")))]
static DUMMY_LOCALHOST_PROXY_AND_URL: LazyLock<Result<(TcpListener, Url), ()>> = LazyLock::new(|| {
    listen()
        .context("failed to make dummy blackhole proxy listener")
        // It's a pain to try to clone Error for users of `DUMMY_PROXY_URL`,
        // so let's just print and return `Err(())`.
        .inspect_err(|err| log_error!("{err}"))
        .map_err(|_err| ())
});

#[cfg(all(not(target_os = "macos"), not(target_os = "windows"), not(target_os = "android")))]
fn listen() -> anyhow::Result<(TcpListener, Url)> {
    // Regarding SOCKS5 and UDP: apparently we don't need to also listen on UDP,
    // because SOCKS5 clients are not supposed to be sending UDP
    // prior to establishing the SOCKS5 connection over TCP.
    // This is implied by the following in the RFC (https://www.rfc-editor.org/rfc/rfc1928#section-6):
    // > A UDP association terminates when the TCP connection that the UDP
    // > ASSOCIATE request arrived on terminates.

    // We're using `:0` port instead of a fixed one because binding would fail
    // if something else is already listening on that fixed port.
    let listener = TcpListener::bind("127.0.0.1:0").context("failed to bind TCP listener")?;

    let listen_addr: SocketAddrV4 =
        match listener.local_addr().context("failed to get local_addr")? {
            SocketAddr::V4(addr) => addr,
            _ => {
                // This shouldn't happen.
                return Err(anyhow!("expected a SocketAddrV4"));
            }
        };

    log_info!("Dummy blackhole proxy listening on {listen_addr}");

    Ok((
        listener,
        Url::from_str(&format!(
            "socks5://{}:{}",
            listen_addr.ip(),
            listen_addr.port()
        ))
        .context("failed to convert addr to Url")?,
    ))
}