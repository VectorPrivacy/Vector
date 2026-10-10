//! Embedded video players: a link's card becomes the video, inline in the chat, on a click.
//!
//! YouTube refuses an embed that arrives without a Referer, and a page on a custom scheme
//! (`tauri://`) sends none. So the card frames a page served from loopback, which frames the
//! player and gives it `http://localhost:<port>/` to send: a named host, since some videos
//! refuse a bare IP. The player loads over the webview's own network stack, outside
//! `vector_core::transport`, so it is offered on Clearnet only.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OnceCell, Semaphore};

const YOUTUBE_EMBED: &str = "https://www.youtube-nocookie.com/embed/";
/// A webview's GET for the host page is well under this; anything longer is not the webview.
const MAX_REQUEST: usize = 4096;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONNECTIONS: usize = 16;
/// Only the app's own page may frame the host page: `tauri://localhost` on macOS and Linux,
/// `http(s)://tauri.localhost` on Windows and Android. `npm run dev` serves it from loopback.
const FRAME_ANCESTORS: &str = if cfg!(debug_assertions) {
    "tauri://localhost http://tauri.localhost https://tauri.localhost http://127.0.0.1:*"
} else {
    "tauri://localhost http://tauri.localhost https://tauri.localhost"
};

/// Bound on the first play and kept for the process: nothing listens until someone presses play.
static PORT: OnceCell<u16> = OnceCell::const_new();
static PERMITS: Semaphore = Semaphore::const_new(MAX_CONNECTIONS);

/// The URL a card frames to play `id`, or why it can't.
#[tauri::command]
pub async fn player_url(provider: String, id: String, start: Option<u32>) -> Result<String, String> {
    if provider != "youtube" || !is_youtube_id(&id) {
        return Err("Not a video Vector can play".into());
    }
    let players_off = vector_core::db::scoped(async { vector_core::synced_prefs::load_settings().players_off }).await;
    if let Some(why) = refusal(vector_core::transport::preference(), players_off) {
        return Err(why.into());
    }
    let port = PORT.get_or_try_init(listen).await?;
    let start = start.filter(|s| *s > 0).map(|s| format!("?start={s}")).unwrap_or_default();
    Ok(format!("http://localhost:{port}/youtube/{id}{start}"))
}

/// Why no player may load now. The player loads outside `vector_core::transport`, so only
/// a settled Clearnet allows it.
fn refusal(network: Option<vector_core::transport::Kind>, players_off: bool) -> Option<&'static str> {
    if network != Some(vector_core::transport::Kind::Clearnet) {
        return Some("Video players are off while Vector uses Tor or I2P");
    }
    players_off.then_some("Video players are switched off in Settings")
}

fn is_youtube_id(id: &str) -> bool {
    id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

async fn listen() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|e| format!("Couldn't start the player: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    // spawn-detached: the player host serves static pages for the process lifetime and holds no account state.
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            };
            let Ok(permit) = PERMITS.try_acquire() else { continue };
            // spawn-detached: one connection off that listener, same lifetime and reason.
            tokio::spawn(async move {
                let _permit = permit;
                let _ = serve(stream, port).await;
            });
        }
    });
    Ok(port)
}

async fn serve(mut stream: TcpStream, port: u16) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let read = tokio::time::timeout(READ_TIMEOUT, async {
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut chunk).await?;
            if n == 0 || buf.len() + n > MAX_REQUEST {
                return Ok::<bool, std::io::Error>(false);
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        Ok(true)
    })
    .await;
    if !matches!(read, Ok(Ok(true))) {
        return Ok(());
    }
    let request = String::from_utf8_lossy(&buf);
    let response = match page(&request, port) {
        Some(html) => respond("200 OK", &html),
        None => respond("404 Not Found", ""),
    };
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

/// The host page for a well-formed request on our own Host, else nothing.
fn page(request: &str, port: u16) -> Option<String> {
    let mut lines = request.split("\r\n");
    let target = lines.next()?.strip_prefix("GET ")?.strip_suffix(" HTTP/1.1")?;
    // Only the name we hand out: one that merely resolves here (DNS rebinding) is not us.
    let host = format!("localhost:{port}");
    if !lines.any(|l| l.split_once(':').is_some_and(|(k, v)| k.eq_ignore_ascii_case("host") && v.trim() == host)) {
        return None;
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let id = path.strip_prefix("/youtube/").filter(|id| is_youtube_id(id))?;
    let start = query
        .strip_prefix("start=")
        .filter(|s| !s.is_empty() && s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit()))
        .map(|s| format!("&start={s}"))
        .unwrap_or_default();
    Some(format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Player</title>\
<style>html,body{{margin:0;height:100%;background:#000;overflow:hidden}}iframe{{border:0;width:100%;height:100%}}</style>\
<iframe src=\"{YOUTUBE_EMBED}{id}?autoplay=1&playsinline=1{start}\" referrerpolicy=\"strict-origin-when-cross-origin\" \
allow=\"autoplay; encrypted-media; picture-in-picture; fullscreen\" allowfullscreen></iframe>"
    ))
}

fn respond(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\n\
Content-Type: text/html; charset=utf-8\r\n\
Content-Length: {}\r\n\
Cache-Control: no-store\r\n\
Referrer-Policy: strict-origin-when-cross-origin\r\n\
X-Content-Type-Options: nosniff\r\n\
Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; frame-src https://www.youtube-nocookie.com; frame-ancestors {FRAME_ANCESTORS}\r\n\
Connection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQ: &str = "GET /youtube/aqz-KE-bpKQ?start=90 HTTP/1.1\r\nHost: localhost:4000\r\n\r\n";

    #[test]
    fn the_host_page_frames_only_a_well_formed_video() {
        let html = page(REQ, 4000).unwrap();
        assert!(html.contains("youtube-nocookie.com/embed/aqz-KE-bpKQ?autoplay=1&playsinline=1&start=90\""));
        assert!(page(&REQ.replace("4000", "4001"), 4000).is_none(), "another Host is refused");
        assert!(page(&REQ.replace("aqz-KE-bpKQ", "aqz-KE-bpK\""), 4000).is_none());
        assert!(page(&REQ.replace("/youtube/", "/vimeo/"), 4000).is_none());
        assert!(page(&REQ.replace("start=90", "start=9\"0"), 4000).unwrap().ends_with("playsinline=1\" referrerpolicy=\"strict-origin-when-cross-origin\" allow=\"autoplay; encrypted-media; picture-in-picture; fullscreen\" allowfullscreen></iframe>"));
        assert!(page(&REQ.replace("GET", "POST"), 4000).is_none());
    }

    #[test]
    fn a_player_loads_only_on_a_settled_clearnet_with_players_on() {
        use vector_core::transport::Kind;
        assert_eq!(refusal(Some(Kind::Clearnet), false), None);
        assert!(refusal(Some(Kind::Clearnet), true).is_some());
        assert!(refusal(Some(Kind::Tor), false).is_some());
        assert!(refusal(Some(Kind::I2p), false).is_some());
        assert!(refusal(None, false).is_some(), "a preference not loaded yet refuses");
    }

    async fn ask(port: u16, request: &[u8]) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let _ = s.write_all(request).await;
        let mut out = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(3), s.read_to_end(&mut out)).await;
        String::from_utf8_lossy(&out).into_owned()
    }

    #[tokio::test]
    async fn the_host_serves_its_own_host_name_only() {
        let port = listen().await.unwrap();
        let get = |host: &str| format!("GET /youtube/aqz-KE-bpKQ HTTP/1.1\r\nHost: {host}\r\n\r\n");
        assert!(ask(port, get(&format!("localhost:{port}")).as_bytes()).await.starts_with("HTTP/1.1 200"));
        assert!(ask(port, get(&format!("127.0.0.1:{port}")).as_bytes()).await.starts_with("HTTP/1.1 404"));
        assert!(ask(port, get(&format!("localhost.evil.example:{port}")).as_bytes()).await.starts_with("HTTP/1.1 404"));
        assert!(ask(port, get("evil.example").as_bytes()).await.starts_with("HTTP/1.1 404"));
        assert_eq!(ask(port, &[b'a'; MAX_REQUEST + 1024]).await, "", "an oversized request gets no answer");
    }

    #[test]
    fn video_ids_are_exactly_eleven_url_safe_characters() {
        assert!(is_youtube_id("dQw4w9WgXcQ") && is_youtube_id("aqz-KE-bpKQ"));
        assert!(!is_youtube_id("dQw4w9WgXc") && !is_youtube_id("dQw4w9WgXcQQ") && !is_youtube_id("dQw4w9Wg/cQ"));
    }
}

// Handlers: player_url
