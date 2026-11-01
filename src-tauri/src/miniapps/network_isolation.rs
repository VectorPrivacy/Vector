//! Linux Mini App webviews are pointed at a proxy that never answers, so a request
//! goes nowhere. The socket is ours and held open: an arbitrary closed port could be
//! taken by another program that would relay the traffic.

use std::net::{Ipv4Addr, TcpListener};
use std::sync::OnceLock;

use url::Url;

static SINK: OnceLock<Option<(TcpListener, Url)>> = OnceLock::new();

/// The proxy every Mini App webview uses, or None if the socket couldn't be held.
pub(crate) fn proxy_url() -> Option<&'static Url> {
    SINK.get_or_init(|| match open_sink() {
        Ok(sink) => Some(sink),
        Err(e) => {
            log_error!("Mini App network isolation unavailable: {e}");
            None
        }
    })
    .as_ref()
    .map(|(_, url)| url)
}

/// Never accepted from: a connection sits in the backlog until the webview gives up.
fn open_sink() -> std::io::Result<(TcpListener, Url)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let url = Url::parse(&format!("socks5://127.0.0.1:{port}")).map_err(std::io::Error::other)?;
    log_info!("Mini App network sink on 127.0.0.1:{port}");
    Ok((listener, url))
}
