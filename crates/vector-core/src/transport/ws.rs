//! Relay egress: every relay socket Vector opens goes through [`VectorWs`], which asks
//! [`super::egress`] per connect and guards the socket it opens.
//!
//! The Direct path is the TCP connect, TLS and upgrade request nostr-sdk's default transport
//! makes, byte for byte.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;

use nostr_sdk::prelude::{ClientBuilder, RelayOptions};
use nostr_sdk::transport::websocket::{WebSocketSink, WebSocketStream, WebSocketTransport};

use super::Lane;

type ConnectFuture<'a> = Pin<Box<dyn Future<Output = Result<(WebSocketSink, WebSocketStream), nostr_sdk::prelude::Error>> + Send + 'a>>;

#[derive(Debug, Clone)]
pub struct VectorWs {
    owner: u64,
    lane: Lane,
    /// Never direct: relays strangers named, dialed only because an anonymity network was chosen.
    no_direct: bool,
}

impl VectorWs {
    pub fn new(owner: u64, lane: Lane) -> Self {
        VectorWs { owner, lane, no_direct: false }
    }
}

/// What nostr-sdk's default transport names itself in the upgrade request.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const UPGRADE_USER_AGENT: &str = "nostr-sdk/0.45.1";

fn refused(e: super::ConnectError) -> nostr_sdk::prelude::Error {
    nostr_sdk::prelude::Error::transport(e.text())
}

impl WebSocketTransport for VectorWs {
    fn support_ping(&self) -> bool {
        true
    }

    fn connect<'a>(&'a self, url: &'a url::Url, _proxy: Option<SocketAddr>) -> ConnectFuture<'a> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Box::pin(native::connect(self.owner, self.lane, self.no_direct, url))
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (self.owner, self.lane, self.no_direct);
            Box::pin(async move {
                let host = url.host_str().unwrap_or_default();
                if let Some(dest) = super::Dest::parse(host) {
                    if let Some(r) = super::route::pre_route(super::Kind::Clearnet, &dest) {
                        return Err(refused(super::ConnectError::Refused(r)));
                    }
                }
                nostr_sdk::transport::websocket::DefaultWebsocketTransport.connect(url, None).await
            })
        }
    }
}

/// The outer timeout nostr-sdk applies to pool-initiated connects: the highest floor any compiled
/// kind may need, since `VectorWs` applies the current one itself.
fn outer_connect_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(15).max(super::budget::highest_compiled_floor(super::Op::RelayConnect))
}

/// NIP-42 auth and the events tracker stay the caller's; the transport and its timeout are ours.
pub fn apply_transport(builder: ClientBuilder, lane: Lane) -> ClientBuilder {
    with_ws(builder, VectorWs::new(crate::db::current_session_id(), lane))
}

/// [`apply_transport`] for relays a stranger named, allowed only off Clearnet: a switch to
/// Clearnet before they connect fails them rather than dialing from this device's address.
pub fn apply_transport_without_direct(builder: ClientBuilder, lane: Lane) -> ClientBuilder {
    with_ws(builder, VectorWs { no_direct: true, ..VectorWs::new(crate::db::current_session_id(), lane) })
}

fn with_ws(builder: ClientBuilder, ws: VectorWs) -> ClientBuilder {
    builder
        .websocket_transport(ws)
        .connect_timeout(outer_connect_timeout())
        .database(crate::events_tracker::LazyEventsTracker::default())
}

/// For stock clients that only take a proxy `SocketAddr` (the NIP-46 signer): a restricted bridge
/// port that serves one connection per permit, issued here for the host being dialed. Its connect
/// budget is the network's at build: the signer is rebuilt on every switch.
pub fn transport_relay_options() -> RelayOptions {
    let opts = RelayOptions::default().connect_timeout(crate::relay_connect_timeout(std::time::Duration::from_secs(15)));
    #[cfg(not(target_arch = "wasm32"))]
    let opts = {
        let owner = crate::db::current_session_id();
        opts.proxy(nostr_sdk::prelude::Proxy::custom(move |url| restricted_target(owner, url.as_str())))
    };
    opts
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn restricted_target(owner: u64, url: &str) -> Option<SocketAddr> {
    use super::bridge;
    let Some((host, port)) = super::host_port(url) else {
        return Some(bridge::restricted_addr().unwrap_or(bridge::NOWHERE));
    };
    match super::egress(owner, Lane::Account, &host, port) {
        super::Egress::Direct => None,
        super::Egress::Proxy(t) => match bridge::restricted_addr() {
            Ok(addr) => {
                bridge::permit(t, &host, port);
                Some(addr)
            }
            Err(_) => Some(bridge::NOWHERE),
        },
        super::Egress::Refuse(_) => Some(bridge::restricted_addr().unwrap_or(bridge::NOWHERE)),
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod native {
    use super::*;
    use async_wsocket::message::CloseFrame;
    use async_wsocket::Message;
    use futures_util::stream::SplitSink;
    use futures_util::{Sink, SinkExt, StreamExt};
    use std::task::{Context, Poll};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::{HeaderMap, HeaderValue};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    pub(crate) async fn connect(
        owner: u64,
        lane: Lane,
        no_direct: bool,
        url: &url::Url,
    ) -> Result<(WebSocketSink, WebSocketStream), nostr_sdk::prelude::Error> {
        let budget = super::super::budget(super::super::Op::RelayConnect, std::time::Duration::from_secs(15));
        match crate::rt::time::timeout(budget, open(owner, lane, no_direct, url)).await {
            Ok(r) => r,
            Err(_) => Err(nostr_sdk::prelude::Error::transport("connection timed out")),
        }
    }

    async fn open(owner: u64, lane: Lane, no_direct: bool, url: &url::Url) -> Result<(WebSocketSink, WebSocketStream), nostr_sdk::prelude::Error> {
        use nostr_sdk::prelude::Error;
        let host = url.host_str().ok_or_else(|| Error::transport("empty host"))?;
        let port = url.port_or_known_default().ok_or_else(|| Error::transport("invalid port"))?;
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        // Read before deciding: a decision that a switch overtakes trips its guard at once.
        let epoch = super::super::epoch();
        let stream = match super::super::egress(owner, lane, bare, port) {
            super::super::Egress::Refuse(e) => return Err(refused(e)),
            super::super::Egress::Direct if no_direct => return Err(refused(super::super::ConnectError::Stale)),
            super::super::Egress::Direct => {
                // Raced against a switch: a connect still trying further addresses must stop
                // sending SYNs over the OS network the moment it no longer may.
                let dial = tokio_happy_eyeballs::connect(format!("{host}:{port}"));
                let changed = super::super::changed(epoch);
                tokio::pin!(dial, changed);
                let tcp = tokio::select! {
                    biased;
                    _ = &mut changed => return Err(refused(super::super::ConnectError::Stale)),
                    r = &mut dial => r.map_err(Error::transport)?,
                };
                if super::super::epoch() != epoch || crate::db::live_session_id() != owner {
                    return Err(refused(super::super::ConnectError::Stale));
                }
                super::super::guard::Guarded::new(tcp, epoch, owner)
            }
            super::super::Egress::Proxy(t) => {
                let bridge = super::super::bridge::addr().map_err(refused)?;
                let (user, pass) = super::super::bridge::credentials(&t);
                let s = tokio_socks::tcp::Socks5Stream::connect_with_password(bridge, (bare, port), &user, &pass)
                    .await
                    .map_err(|e| Error::transport(socks_error_text(&e, bare)))?;
                super::super::guard::Guarded::new(s.into_inner(), t.epoch, owner)
            }
        };
        let request = upgrade_request(url).map_err(Error::transport)?;
        let (ws, _) = Box::pin(tokio_tungstenite::client_async_tls(request, stream)).await.map_err(Error::transport)?;
        let (tx, rx) = ws.split();
        let sink: WebSocketSink = Box::pin(VectorSink(tx));
        let stream: WebSocketStream = Box::pin(rx.filter_map(|m| async move {
            match m {
                Ok(WsMessage::Frame(_)) => None,
                Ok(m) => Some(Ok(from_tungstenite(m))),
                Err(e) => Some(Err(nostr_sdk::prelude::Error::transport(e))),
            }
        }));
        Ok((sink, stream))
    }

    /// A bridge refusal's reason, when one was recorded for the host; the SOCKS text otherwise.
    fn socks_error_text(e: &tokio_socks::Error, host: &str) -> String {
        use super::super::{ConnectError, Kind};
        super::super::host::active()
            .and_then(|a| {
                let (at, err) = a.last_failure(host)?;
                // Under Tor an arti failure keeps the SOCKS text, as it always read.
                let keep = a.kind == Kind::Tor && matches!(err, ConnectError::Unreachable(_));
                (at.elapsed() < std::time::Duration::from_secs(30) && !keep).then(|| err.text())
            })
            .unwrap_or_else(|| e.to_string())
    }

    pub(crate) fn upgrade_request(
        url: &url::Url,
    ) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request, tokio_tungstenite::tungstenite::Error> {
        let mut request = url.as_str().into_client_request()?;
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", HeaderValue::from_static(UPGRADE_USER_AGENT));
        request.headers_mut().extend(headers);
        Ok(request)
    }

    fn from_tungstenite(m: WsMessage) -> Message {
        match m {
            WsMessage::Text(t) => Message::Text(t.to_string()),
            WsMessage::Binary(b) => Message::Binary(b.to_vec()),
            WsMessage::Ping(b) => Message::Ping(b.to_vec()),
            WsMessage::Pong(b) => Message::Pong(b.to_vec()),
            WsMessage::Close(f) => Message::Close(f.map(CloseFrame::from)),
            WsMessage::Frame(_) => Message::Binary(Vec::new()),
        }
    }

    type Inner = SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<super::super::guard::Guarded<tokio::net::TcpStream>>,
        >,
        WsMessage,
    >;

    /// Mapped by hand: `sink_map_err` can panic under nostr-sdk's polling.
    struct VectorSink(Inner);

    impl Sink<Message> for VectorSink {
        type Error = nostr_sdk::prelude::Error;

        fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Pin::new(&mut self.0).poll_ready_unpin(cx).map_err(nostr_sdk::prelude::Error::transport)
        }

        fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            Pin::new(&mut self.0).start_send_unpin(WsMessage::from(item)).map_err(nostr_sdk::prelude::Error::transport)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Pin::new(&mut self.0).poll_flush_unpin(cx).map_err(nostr_sdk::prelude::Error::transport)
        }

        fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Pin::new(&mut self.0).poll_close_unpin(cx).map_err(nostr_sdk::prelude::Error::transport)
        }
    }
}
