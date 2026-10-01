//! Lightweight localhost WebSocket server for zero-overhead realtime sends.
//!
//! Mini App JS connects via `ws://127.0.0.1:{port}/{ticket}` and sends
//! raw binary frames — one syscall per message, no HTTP framing, no WebView IPC
//! bottleneck. This bypasses the WKWebView/WebView2 custom scheme pipeline that
//! limits `fetch()` to ~100 req/s.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

use super::realtime::SendHandle;

/// Info returned after starting the WS server.
pub(crate) struct WsInfo {
    pub port: u16,
}

/// Ticket → the Mini App window it was issued to. A connection's label comes from its
/// ticket, never from the client, so one app can't send or listen as another.
pub(crate) type WsTickets = Arc<std::sync::RwLock<HashMap<String, String>>>;

/// Shared state passed to each WS connection handler.
struct WsState {
    tickets: WsTickets,
    send_handles: Arc<std::sync::RwLock<HashMap<String, SendHandle>>>,
    /// Map of window label → WS sender. WS handler registers here on connect.
    /// join_channel looks up the sender and wires it into the event target.
    ws_senders: Arc<std::sync::RwLock<HashMap<String, tokio::sync::mpsc::Sender<Vec<u8>>>>>,
}



/// Run the WS accept loop. Public so RealtimeManager can call it directly
/// with a pre-bound listener (needed for Android JNI timing).
pub(crate) async fn run_accept_loop(
    listener: TcpListener,
    tickets: WsTickets,
    send_handles: Arc<std::sync::RwLock<HashMap<String, SendHandle>>>,
    ws_senders: Arc<std::sync::RwLock<HashMap<String, tokio::sync::mpsc::Sender<Vec<u8>>>>>,
) {
    let state = Arc::new(WsState {
        tickets,
        send_handles,
        ws_senders,
    });

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let st = Arc::clone(&state);
                // spawn-detached: miniapp websocket accept loop — process-lifetime transport, no account storage.
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, &st).await {
                        log_trace!("[WEBXDC] RT WS connection error: {e}");
                    }
                });
            }
            Err(e) => {
                log_warn!("[WEBXDC] RT WS accept error: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
}

/// Handle a single WebSocket connection.
///
/// The URL path is `/{ticket}`; anything after it is ignored.
/// After the upgrade handshake, binary frames are forwarded to `fast_send()`.
async fn handle_connection(
    stream: tokio::net::TcpStream,
    state: &WsState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Resolve the label from the ticket during the WebSocket handshake
    let label: Arc<std::sync::OnceLock<String>> = Arc::new(std::sync::OnceLock::new());
    let label_for_cb = label.clone();
    let tickets = state.tickets.clone();

    // The callback's error type is fixed by tungstenite.
    #[allow(clippy::result_large_err)]
    let ws_stream = tokio_tungstenite::accept_hdr_async(
        stream,
        move |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
              resp: tokio_tungstenite::tungstenite::handshake::server::Response|
              -> Result<
            tokio_tungstenite::tungstenite::handshake::server::Response,
            tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
        > {
            let ticket = req.uri().path().trim_start_matches('/').split('/').next().unwrap_or("");
            match label_for_ticket(&tickets, ticket) {
                Some(owner) => {
                    let _ = label_for_cb.set(owner);
                    Ok(resp)
                }
                None => Err(http::Response::builder()
                    .status(http::StatusCode::FORBIDDEN)
                    .body(None)
                    .unwrap()),
            }
        },
    )
    .await?;

    let label = match label.get() {
        Some(l) => l.clone(),
        None => return Ok(()), // handshake rejected
    };

    log_info!("[WEBXDC] RT WS connected: {label}");

    // Bi-directional: read from JS (send to gossip) AND write to JS (receive from gossip)
    let (mut sink, mut stream) = ws_stream.split();

    // Create a channel for incoming gossip data → WS write
    let (ws_tx, mut ws_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);

    // Store WS sender in a shared map so join_channel can find it later.
    // The WS connects before join_channel runs, so we can't look up the event
    // target here. Instead, join_channel looks up our sender and wires it in.
    {
        let mut senders = state.ws_senders.write().unwrap_or_else(|e| e.into_inner());
        senders.insert(label.clone(), ws_tx.clone());
        log_info!("[WEBXDC] RT WS sender registered for: {label}");
    }

    let mut msg_count: u64 = 0;
    let mut total_nanos: u64 = 0;
    let mut peak_nanos: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut recv_count: u64 = 0;
    let mut stats_interval = tokio::time::interval(std::time::Duration::from_secs(5));
    stats_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    stats_interval.tick().await;

    loop {
        tokio::select! {
            // JS → gossip (send path)
            msg = stream.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        let len = data.len();
                        if len <= 128_000 {
                            let t0 = std::time::Instant::now();
                            fast_send_inline(
                                &state.send_handles,
                                &label,
                                data.into(),
                            );
                            let elapsed = t0.elapsed().as_nanos() as u64;
                            msg_count += 1;
                            total_nanos += elapsed;
                            total_bytes += len as u64;
                            if elapsed > peak_nanos { peak_nanos = elapsed; }
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        let _ = sink.send(Message::Pong(payload)).await;
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
            // Gossip → JS (receive path via WS)
            Some(data) = ws_rx.recv() => {
                if sink.send(Message::Binary(data.into())).await.is_err() {
                    break;
                }
                recv_count += 1;
            }
            _ = stats_interval.tick() => {
                if msg_count > 0 || recv_count > 0 {
                    let avg_us = total_nanos.checked_div(msg_count).map_or(0, |n| n / 1_000);
                    let peak_us = peak_nanos / 1_000;
                    let avg_kb = total_bytes.checked_div(msg_count).map_or(0, |b| b / 1024);
                    log_info!(
                        "[WEBXDC] RT WS stats: {msg_count} sent/{recv_count} recv in 5s ({}/s), avg {avg_us}μs, peak {peak_us}μs, avg {avg_kb}KB/msg",
                        msg_count / 5
                    );
                    msg_count = 0;
                    recv_count = 0;
                    total_nanos = 0;
                    peak_nanos = 0;
                    total_bytes = 0;
                }
            }
        }
    }

    // Clean up WS sender
    {
        let mut senders = state.ws_senders.write().unwrap_or_else(|e| e.into_inner());
        senders.remove(&label);
    }

    log_info!("[WEBXDC] RT WS disconnected: {label}");
    Ok(())
}

/// Inline fast_send — adds gossip trailer and broadcasts via gossip protocol.
#[inline]
fn fast_send_inline(
    send_handles: &std::sync::RwLock<HashMap<String, SendHandle>>,
    label: &str,
    data: Vec<u8>,
) {
    let handles = send_handles.read().unwrap_or_else(|e| e.into_inner());
    let Some(handle) = handles.get(label) else {
        return;
    };

    // Add trailer: seq(4) + pubkey(32)
    let mut msg = data;
    msg.reserve(36);
    let seq_num = handle.seq.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    msg.extend_from_slice(&seq_num.to_le_bytes());
    msg.extend_from_slice(&handle.public_key_bytes);

    // Fire-and-forget broadcast (gossip is async, spawn a task)
    let sender = handle.sender.clone();
    vector_core::db::spawn_bound(async move {
        let _ = sender.broadcast(msg.into()).await;
    });
}

fn label_for_ticket(tickets: &WsTickets, ticket: &str) -> Option<String> {
    if ticket.is_empty() {
        return None;
    }
    tickets.read().unwrap_or_else(|e| e.into_inner()).get(ticket).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ticket_names_the_window() {
        let tickets: WsTickets = Default::default();
        tickets.write().unwrap().insert("aaaa".into(), "miniapp:chat:a".into());
        assert_eq!(label_for_ticket(&tickets, "aaaa").as_deref(), Some("miniapp:chat:a"));
        assert_eq!(label_for_ticket(&tickets, "bbbb"), None);
        assert_eq!(label_for_ticket(&tickets, ""), None);
    }
}
