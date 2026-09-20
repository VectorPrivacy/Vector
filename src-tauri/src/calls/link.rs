//! The loopback socket the webview pushes its encoded video through and pulls the
//! peer's frames from. Bound once per process on first use; a per-boot token in
//! the path and the app's own origin are the door. The URL reaches the page only
//! through a command, never through HTML.

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use std::sync::{Mutex, OnceLock};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// One socket, split: what goes to the webview and what comes from it.
pub struct LinkConn {
    pub to_web: mpsc::Sender<Bytes>,
    pub from_web: mpsc::Receiver<Bytes>,
}

struct Server {
    port: u16,
    token: String,
}

static SERVER: OnceLock<Server> = OnceLock::new();
/// Where a new socket goes: the video track of the call in progress, if any.
static TAKER: Mutex<Option<mpsc::Sender<LinkConn>>> = Mutex::new(None);

/// Frames queued toward the webview before the oldest are dropped.
const TO_WEB_DEPTH: usize = 8;
/// Frames queued from the webview before its socket reads stall.
const FROM_WEB_DEPTH: usize = 8;

pub fn set_taker(taker: Option<mpsc::Sender<LinkConn>>) {
    *TAKER.lock().unwrap_or_else(|e| e.into_inner()) = taker;
}

fn origin_allowed(origin: &str) -> bool {
    if matches!(origin, "tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost") {
        return true;
    }
    // The dev server.
    cfg!(debug_assertions) && (origin.starts_with("http://localhost:") || origin.starts_with("http://127.0.0.1:"))
}

/// The URL the webview connects to, binding the server on first use.
pub fn url() -> Result<String, String> {
    if let Some(s) = SERVER.get() {
        return Ok(format!("ws://127.0.0.1:{}/{}", s.port, s.token));
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let token = {
        let mut bytes = [0u8; 16];
        use rand::RngCore;
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        crate::util::bytes_to_hex_16(&bytes)
    };
    let server = Server { port, token: token.clone() };
    if SERVER.set(server).is_err() {
        return url();
    }
    let url = format!("ws://127.0.0.1:{port}/{token}");
    // spawn-detached: process-lifetime loopback listener, no account state.
    tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(l) => l,
            Err(e) => {
                log_warn!("[CALLS] Video link listener failed: {e}");
                return;
            }
        };
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                // Out of descriptors, most likely; spinning would not help.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            };
            let token = token.clone();
            // spawn-detached: one socket's lifetime, no account state.
            tokio::spawn(async move {
                if let Err(e) = handle(stream, &token).await {
                    log_trace!("[CALLS] Video link socket: {e}");
                }
            });
        }
    });
    Ok(url)
}

/// Equal length and equal bytes, taking the same time either way.
fn token_matches(given: &str, token: &str) -> bool {
    let (a, b) = (given.as_bytes(), token.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..b.len() {
        diff |= a.get(i).copied().unwrap_or(0) ^ b[i];
    }
    diff == 0
}

/// A local process that opens the port and never finishes the handshake holds a descriptor.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

async fn handle(stream: tokio::net::TcpStream, token: &str) -> Result<(), String> {
    let handshake = tokio_tungstenite::accept_hdr_async(stream, |req: &http::Request<()>, resp: http::Response<()>| {
        let path_ok = token_matches(req.uri().path().trim_start_matches('/'), token);
        let origin_ok = req
            .headers()
            .get("origin")
            .and_then(|v| v.to_str().ok())
            .map_or(false, origin_allowed);
        if path_ok && origin_ok {
            Ok(resp)
        } else {
            Err(http::Response::builder().status(http::StatusCode::FORBIDDEN).body(None).unwrap())
        }
    });
    let ws = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake)
        .await
        .map_err(|_| "handshake timed out".to_string())?
        .map_err(|e| e.to_string())?;

    let (to_web_tx, mut to_web_rx) = mpsc::channel::<Bytes>(TO_WEB_DEPTH);
    let (from_web_tx, from_web_rx) = mpsc::channel::<Bytes>(FROM_WEB_DEPTH);
    let taken = {
        let taker = TAKER.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match taker {
            Some(t) => t.try_send(LinkConn { to_web: to_web_tx, from_web: from_web_rx }).is_ok(),
            None => false,
        }
    };
    if !taken {
        return Err("no call to attach the video link to".into());
    }
    let (mut sink, mut source) = ws.split();
    // spawn-detached: drains for this socket's lifetime, no account state.
    let writer = tokio::spawn(async move {
        while let Some(b) = to_web_rx.recv().await {
            if sink.send(Message::Binary(b)).await.is_err() {
                break;
            }
        }
    });
    while let Some(Ok(msg)) = source.next().await {
        match msg {
            Message::Binary(b) => {
                if from_web_tx.send(b).await.is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    writer.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_must_match_exactly() {
        assert!(token_matches("abc123", "abc123"));
        assert!(!token_matches("abc124", "abc123"));
        assert!(!token_matches("abc12", "abc123"));
        assert!(!token_matches("abc1234", "abc123"));
        assert!(!token_matches("", "abc123"));
    }

    #[test]
    fn only_the_apps_own_origins_pass() {
        assert!(origin_allowed("tauri://localhost"));
        assert!(origin_allowed("http://tauri.localhost"));
        assert!(!origin_allowed("https://evil.example"));
        assert!(!origin_allowed("null"));
        assert!(!origin_allowed(""));
    }
}
