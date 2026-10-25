//! `vmedia`: media this account holds, served to the main window by content hash, and Vector
//! Cache files (`c/<path>`), which are sealed on disk.
//!
//! Every URL carries a token drawn at launch. The app's own pages know it; an embedded player's
//! frame inside the same window never does, and the webview label alone can't tell them apart.

use std::borrow::Cow;
use std::sync::OnceLock;

use tauri::http::{self, header, StatusCode};
use tauri::{UriSchemeContext, UriSchemeResponder};
use vector_core::media::range::{self, ByteRange};

pub const SCHEME: &str = "vmedia";

/// The most one response carries: a player asks for the rest as it plays, and a whole video
/// is never copied for one request.
const MAX_PART: u64 = 4 * 1024 * 1024;

type Response = http::Response<Cow<'static, [u8]>>;

pub fn token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        use rand::RngCore;
        let mut bytes = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        crate::util::bytes_to_hex_string(&bytes)
    })
}

/// What a src is built on: `<base>/<hash>`. Android's webview can't seek a custom scheme, so
/// its media server serves held media instead.
#[cfg_attr(target_os = "android", allow(dead_code))]
pub fn base_url() -> String {
    if cfg!(windows) {
        format!("http://{SCHEME}.localhost/{}", token())
    } else {
        format!("{SCHEME}://localhost/{}", token())
    }
}

pub fn protocol<R: tauri::Runtime>(ctx: UriSchemeContext<'_, R>, request: http::Request<Vec<u8>>, responder: UriSchemeResponder) {
    if ctx.webview_label() != "main" {
        responder.respond(status(StatusCode::FORBIDDEN));
        return;
    }
    let origin = app_origin(&request).map(str::to_string);
    // Off the webview's thread: a response is a copy out of memory, and soon a decrypt.
    tauri::async_runtime::spawn_blocking(move || {
        let mut response = respond_to(&request);
        // Errors too: a fetch that can't read a 404 sees only a network failure.
        if let Some(value) = origin.and_then(|o| o.parse().ok()) {
            response.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
        }
        responder.respond(response)
    });
}

fn respond_to(request: &http::Request<Vec<u8>>) -> Response {
    let mut parts = request.uri().path().trim_start_matches('/').splitn(2, '/');
    let (Some(given), Some(rest)) = (parts.next(), parts.next()) else {
        return status(StatusCode::NOT_FOUND);
    };
    if !same(given.as_bytes(), token().as_bytes()) {
        return status(StatusCode::FORBIDDEN);
    }
    // `c/<path under the app's folder>`: a Vector Cache file, opened sealed or from before.
    let src = if let Some(rel) = rest.strip_prefix("c/") {
        let Some(path) = vector_core::vcache::decode_rel(rel).and_then(|rel| vector_core::vcache::resolve(&rel)) else {
            return status(StatusCode::NOT_FOUND);
        };
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
        vector_core::vcache::open(&path, vector_core::crypto::mime_from_extension(ext))
    } else {
        vector_core::simd::hex::hex_to_bytes_32_checked(rest).and_then(|hash| vector_core::media::open(&hash))
    };
    let Some(src) = src else {
        return status(StatusCode::NOT_FOUND);
    };
    // A held SVG is shown only through its render, never as a document of its own.
    let Some(content_type) = passive_type(src.mime()) else {
        return status(StatusCode::FORBIDDEN);
    };

    let len = src.len();
    let asked = request.headers().get(header::RANGE).and_then(|v| v.to_str().ok());
    let (code, start, end) = match range::parse(asked, len) {
        ByteRange::Whole => (StatusCode::OK, 0, len),
        ByteRange::Part { start, end } => (StatusCode::PARTIAL_CONTENT, start, end.min(start + MAX_PART - 1) + 1),
        ByteRange::Unsatisfiable => {
            let mut response = status(StatusCode::RANGE_NOT_SATISFIABLE);
            if let Ok(value) = format!("bytes */{len}").parse() {
                response.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            return response;
        }
    };

    let mut body = vec![0u8; (end - start) as usize];
    let mut filled = 0;
    while filled < body.len() {
        match src.read_at(start + filled as u64, &mut body[filled..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => filled += n,
        }
    }
    body.truncate(filled);

    let mut builder = http::Response::builder()
        .status(code)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, body.len())
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'");
    if code == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(header::CONTENT_RANGE, format!("bytes {}-{}/{len}", start, start + body.len() as u64 - 1));
    }
    builder.body(Cow::Owned(body)).unwrap_or_else(|_| status(StatusCode::INTERNAL_SERVER_ERROR))
}

/// Media types a page can only display, never run.
pub(crate) fn passive_type(mime: &str) -> Option<&str> {
    let kind = mime.split('/').next().unwrap_or_default();
    match kind {
        _ if mime.starts_with("image/svg") => None,
        "image" | "audio" | "video" | "model" => Some(mime),
        _ => Some("application/octet-stream"),
    }
}

/// The app's own origin, echoed so its fetches (the 3D loader) can read the body. A dev build
/// serves the app from a loopback dev server.
fn app_origin(request: &http::Request<Vec<u8>>) -> Option<&str> {
    let origin = request.headers().get(header::ORIGIN)?.to_str().ok()?;
    let dev_server = cfg!(debug_assertions) && (origin.starts_with("http://127.0.0.1:") || origin.starts_with("http://localhost:"));
    (matches!(origin, "tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost") || dev_server).then_some(origin)
}

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn status(code: StatusCode) -> Response {
    http::Response::builder()
        .status(code)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Cow::Borrowed(&[][..]))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(path: &str, range: Option<&str>) -> Response {
        let mut request = http::Request::builder().uri(format!("vmedia://localhost{path}"));
        if let Some(range) = range {
            request = request.header(header::RANGE, range);
        }
        respond_to(&request.body(Vec::new()).unwrap())
    }

    fn held(byte: u8, len: usize, mime: &str) -> String {
        let hash = [byte; 32];
        vector_core::media::ram::cache().insert(hash, vec![byte; len], mime).unwrap();
        format!("/{}/{}", token(), crate::util::bytes_to_hex_string(&hash))
    }

    #[test]
    fn it_serves_what_the_account_holds_by_range() {
        // Held media is the live session's: no other test may swap it out mid-check.
        let _session = crate::commands::transport::tests::TEST_LOCK.blocking_lock();
        let path = held(0x11, 10, "video/mp4");
        let whole = get(&path, None);
        assert_eq!((whole.status(), whole.body().len()), (StatusCode::OK, 10));
        assert_eq!(whole.headers()[header::CACHE_CONTROL], "no-store");
        let part = get(&path, Some("bytes=2-4"));
        assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(part.headers()[header::CONTENT_RANGE], "bytes 2-4/10");
        assert_eq!(part.body().as_ref(), &[0x11; 3]);
        assert_eq!(get(&path, Some("bytes=10-")).status(), StatusCode::RANGE_NOT_SATISFIABLE);
    }

    #[test]
    fn an_open_range_is_answered_a_part_at_a_time() {
        // Held media is the live session's: no other test may swap it out mid-check.
        let _session = crate::commands::transport::tests::TEST_LOCK.blocking_lock();
        let path = held(0x22, (MAX_PART + 10) as usize, "video/mp4");
        let first = get(&path, Some("bytes=0-"));
        assert_eq!(first.body().len() as u64, MAX_PART);
        assert_eq!(first.headers()[header::CONTENT_RANGE], format!("bytes 0-{}/{}", MAX_PART - 1, MAX_PART + 10));
        let rest = get(&path, Some(&format!("bytes={MAX_PART}-")));
        assert_eq!(rest.body().len(), 10);
    }

    #[test]
    fn a_cache_file_is_served_opened_and_nothing_else_under_the_folder_is() {
        let root = crate::util::test_app_data();
        let avatars = root.join("cache").join("avatars");
        std::fs::create_dir_all(&avatars).unwrap();
        vector_core::vcache::write(&avatars.join("face test.png"), b"\x89PNG the face").unwrap();
        std::fs::write(root.join("vector.db"), b"not for pages").unwrap();

        let whole = get(&format!("/{}/c/cache/avatars/face%20test.png", token()), None);
        assert_eq!((whole.status(), whole.body().as_ref()), (StatusCode::OK, b"\x89PNG the face".as_slice()));
        assert_eq!(whole.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(whole.headers()[header::CACHE_CONTROL], "no-store");
        let part = get(&format!("/{}/c/cache/avatars/face%20test.png", token()), Some("bytes=5-7"));
        assert_eq!(part.body().as_ref(), b"the");
        for refused in ["cache/avatars/../../vector.db", "cache/..%2F..%2Fvector.db", "vector.db", "cache", "npub1x/vector.db"] {
            assert_eq!(get(&format!("/{}/c/{refused}", token()), None).status(), StatusCode::NOT_FOUND, "{refused}");
        }
    }

    #[test]
    fn a_wrong_token_an_unknown_hash_or_a_raw_svg_is_refused() {
        // Held media is the live session's: no other test may swap it out mid-check.
        let _session = crate::commands::transport::tests::TEST_LOCK.blocking_lock();
        let path = held(0x33, 4, "image/png");
        let hash = path.rsplit('/').next().unwrap().to_string();
        assert_eq!(get(&format!("/{}/{hash}", "0".repeat(32)), None).status(), StatusCode::FORBIDDEN);
        assert_eq!(get(&format!("/{}/{}", token(), "ab".repeat(32)), None).status(), StatusCode::NOT_FOUND);
        assert_eq!(get(&format!("/{}/nothex", token()), None).status(), StatusCode::NOT_FOUND);
        assert_eq!(get(&held(0x44, 4, "image/svg+xml"), None).status(), StatusCode::FORBIDDEN);
        assert_eq!(get(&held(0x55, 4, "text/html"), None).headers()[header::CONTENT_TYPE], "application/octet-stream");
    }
}
