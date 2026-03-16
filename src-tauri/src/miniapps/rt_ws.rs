//! Lightweight localhost WebSocket server for zero-overhead realtime sends.
//!
//! Mini App JS connects via `ws://127.0.0.1:{port}/{token}/{label}` and sends
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
    pub token: String,
}

/// Shared state passed to each WS connection handler.
struct WsState {
    token: String,
    send_handles: Arc<std::sync::RwLock<HashMap<String, SendHandle>>>,
}

/// Start the realtime WebSocket server on a random localhost port.
///
/// Returns `(port, token)`. The server runs as a background tokio task.
pub(crate) async fn start(
    send_handles: Arc<std::sync::RwLock<HashMap<String, SendHandle>>>,
) -> Result<WsInfo, String> {
    // Random 32-char hex token (128-bit security)
    let token = {
        let mut bytes = [0u8; 16];
        use rand::RngCore;
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        crate::simd::hex::bytes_to_hex_16(&bytes)
    };

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind RT WS server: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to get local addr: {e}"))?
        .port();

    log_info!("[WEBXDC] Realtime WS server listening on 127.0.0.1:{port}");

    let state = Arc::new(WsState {
        token: token.clone(),
        send_handles,
    });

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let st = Arc::clone(&state);
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
    });

    Ok(WsInfo { port, token })
}

/// Handle a single WebSocket connection.
///
/// The URL path is `/{token}/{percent_encoded_label}`.
/// After the upgrade handshake, binary frames are forwarded to `fast_send()`.
async fn handle_connection(
    stream: tokio::net::TcpStream,
    state: &WsState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Extract label from the URL during the WebSocket handshake
    let label: Arc<std::sync::OnceLock<String>> = Arc::new(std::sync::OnceLock::new());
    let label_for_cb = label.clone();
    let token_ref = state.token.clone();

    let ws_stream = tokio_tungstenite::accept_hdr_async(
        stream,
        move |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
              resp: tokio_tungstenite::tungstenite::handshake::server::Response|
              -> Result<
            tokio_tungstenite::tungstenite::handshake::server::Response,
            tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
        > {
            let path = req.uri().path();
            // Parse /{token}/{percent_encoded_label}
            let trimmed = path.trim_start_matches('/');
            let (req_token, rest) = match trimmed.split_once('/') {
                Some(pair) => pair,
                None => {
                    return Err(http::Response::builder()
                        .status(http::StatusCode::BAD_REQUEST)
                        .body(None)
                        .unwrap());
                }
            };
            if req_token != token_ref {
                return Err(http::Response::builder()
                    .status(http::StatusCode::FORBIDDEN)
                    .body(None)
                    .unwrap());
            }
            // Percent-decode the label (window labels contain colons)
            let decoded = percent_decode(rest);
            let _ = label_for_cb.set(decoded);
            Ok(resp)
        },
    )
    .await?;

    let label = match label.get() {
        Some(l) => l.clone(),
        None => return Ok(()), // handshake rejected
    };

    log_info!("[WEBXDC] RT WS connected: {label}");

    // Read loop: binary frames → fast_send, with periodic stats
    let (mut _sink, mut stream) = ws_stream.split();
    let mut msg_count: u64 = 0;
    let mut total_nanos: u64 = 0;
    let mut peak_nanos: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut stats_interval = tokio::time::interval(std::time::Duration::from_secs(5));
    stats_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Skip the first immediate tick
    stats_interval.tick().await;

    loop {
        tokio::select! {
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
                        let _ = _sink.send(Message::Pong(payload)).await;
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
            _ = stats_interval.tick() => {
                if msg_count > 0 {
                    let avg_us = (total_nanos / msg_count) / 1_000;
                    let peak_us = peak_nanos / 1_000;
                    let avg_kb = total_bytes / msg_count / 1024;
                    log_info!(
                        "[WEBXDC] RT WS stats: {msg_count} msgs in 5s ({}/s), avg {avg_us}μs, peak {peak_us}μs, avg {avg_kb}KB/msg",
                        msg_count / 5
                    );
                    msg_count = 0;
                    total_nanos = 0;
                    peak_nanos = 0;
                    total_bytes = 0;
                }
            }
        }
    }

    log_info!("[WEBXDC] RT WS disconnected: {label}");
    Ok(())
}

/// Inline fast_send — forwards raw payload directly to the send queue.
/// No trailer needed: raw QUIC connections identify senders by connection
/// and guarantee no duplicates, so seq/pubkey trailers are unnecessary.
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

    // Non-blocking enqueue — drops packet on overload
    if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) = handle.send_tx.try_send(data) {
        handle.drops.fetch_add(1, Ordering::Relaxed);
    }
}

/// Simple percent-decode for URL path segments.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                result.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(result).unwrap_or_else(|_| input.to_string())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
