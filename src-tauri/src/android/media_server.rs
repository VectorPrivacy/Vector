//! Lightweight localhost HTTP server for Android media streaming.
//!
//! Android WebView's `asset://` protocol doesn't support HTTP Range requests,
//! which breaks `<video>` and `<audio>` seeking/streaming. This module spins up
//! a minimal HTTP file server on `127.0.0.1` with proper Range header support
//! (HTTP 206 Partial Content) so media elements work correctly on Android.
//!
//! Desktop platforms don't need this — their WebViews support range requests
//! natively via `asset://`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use crate::simd::hex::bytes_to_hex_16;
use crate::util::mime_from_extension_static;

/// Max concurrent connections (prevents FD exhaustion from malicious local apps).
const MAX_CONNECTIONS: usize = 64;

/// Timeout for reading the initial HTTP request (prevents slow-loris stalls).
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Shared server state.
struct ServerState {
    /// Random hex token for request authentication (prevents other apps from accessing files).
    token: String,
    /// Canonicalized directories the server is allowed to serve files from.
    allowed_dirs: Vec<PathBuf>,
    /// Connection limiter.
    semaphore: Semaphore,
}

impl ServerState {
    /// Validates that the requested path falls within an allowed directory.
    fn is_path_allowed(&self, path: &std::path::Path) -> bool {
        self.allowed_dirs.iter().any(|dir| path.starts_with(dir))
    }
}

/// Starts the media server. Returns `(port, token)`.
///
/// The server binds to `127.0.0.1:0` (random available port) and runs as a
/// background tokio task. URLs are formatted as:
///
/// ```text
/// http://127.0.0.1:{port}/{token}/{percent_encoded_filepath}
/// ```
pub async fn start(allowed_dirs: Vec<PathBuf>) -> Result<(u16, String), String> {
    // Generate a random 32-char hex token using SIMD-accelerated encoding
    let token = {
        let mut bytes = [0u8; 16];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut bytes);
        bytes_to_hex_16(&bytes)
    };

    // Canonicalize allowed directories at startup (so is_path_allowed compares canonical paths)
    let mut canonical_dirs = Vec::with_capacity(allowed_dirs.len());
    for dir in &allowed_dirs {
        // Create the directory if it doesn't exist yet (common on first launch)
        let _ = tokio::fs::create_dir_all(dir).await;
        match tokio::fs::canonicalize(dir).await {
            Ok(c) => canonical_dirs.push(c),
            Err(_) => canonical_dirs.push(dir.clone()), // keep original if canonicalize fails
        }
    }

    // Bind to a random available port on localhost, retrying if the port is taken
    let listener = {
        let mut attempts = 0;
        loop {
            match TcpListener::bind("127.0.0.1:0").await {
                Ok(l) => break l,
                Err(e) => {
                    attempts += 1;
                    if attempts >= 5 {
                        return Err(format!("failed to bind to localhost after {attempts} attempts: {e}"));
                    }
                    eprintln!("[media_server] bind attempt {attempts} failed: {e}, retrying...");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    };
    let port = listener.local_addr()
        .map_err(|e| format!("failed to get local addr: {e}"))?
        .port();

    let state = Arc::new(ServerState {
        token: token.clone(),
        allowed_dirs: canonical_dirs,
        semaphore: Semaphore::new(MAX_CONNECTIONS),
    });

    println!("[media_server] listening on 127.0.0.1:{port}");

    tokio::spawn(async move {
        loop {
            let (stream, _addr) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    eprintln!("[media_server] accept error: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                // Acquire a connection permit (or drop the connection if at capacity)
                let _permit = match state.semaphore.try_acquire() {
                    Ok(p) => p,
                    Err(_) => return, // at capacity, silently drop
                };
                if let Err(e) = handle_connection(stream, &state).await {
                    eprintln!("[media_server] connection error: {e}");
                }
            });
        }
    });

    Ok((port, token))
}

/// HTTP method, parsed from the first byte for zero-cost dispatch.
#[derive(PartialEq)]
enum Method { Get, Head, Options, Other }

/// Handle a single HTTP connection.
///
/// All parsing is done on raw `&[u8]` — no String allocation until we need
/// the decoded file path. Method/token/header comparisons are byte-level.
async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    state: &ServerState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Stack buffer — 8KB is plenty for any request we'll see (avoids heap alloc)
    let mut buf = [0u8; 8192];

    // Read with timeout to prevent slow-loris stalls
    let n = match tokio::time::timeout(READ_TIMEOUT, stream.read(&mut buf)).await {
        Ok(Ok(n)) => n,
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => return Ok(()), // timeout — silently close
    };
    if n == 0 {
        return Ok(());
    }
    let req = &buf[..n];

    // Parse method from the first bytes (no allocation, no collect)
    let method = match req.first() {
        Some(b'G') if req.starts_with(b"GET ") => Method::Get,
        Some(b'H') if req.starts_with(b"HEAD ") => Method::Head,
        Some(b'O') if req.starts_with(b"OPTIONS ") => Method::Options,
        _ => Method::Other,
    };

    // Handle CORS preflight
    if method == Method::Options {
        send_response(
            &mut stream,
            204,
            "No Content",
            &[
                ("Access-Control-Allow-Origin", "*"),
                ("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS"),
                ("Access-Control-Allow-Headers", "Range"),
            ],
            &[],
        )
        .await?;
        return Ok(());
    }

    if method == Method::Other {
        send_response(&mut stream, 405, "Method Not Allowed", &[], &[]).await?;
        return Ok(());
    }

    // Find the URI: skip "METHOD " to get start, then find the space before "HTTP/1.1"
    let uri_start = match method {
        Method::Get => 4,   // "GET "
        Method::Head => 5,  // "HEAD "
        _ => unreachable!(),
    };
    let uri_end = match memchr::memchr(b' ', &req[uri_start..]) {
        Some(pos) => uri_start + pos,
        None => {
            send_response(&mut stream, 400, "Bad Request", &[], &[]).await?;
            return Ok(());
        }
    };
    let uri = &req[uri_start..uri_end];

    // Parse path: /{token}/{percent_encoded_filepath}
    // Skip leading '/'
    let path = if uri.first() == Some(&b'/') { &uri[1..] } else { uri };
    let slash_pos = match memchr::memchr(b'/', path) {
        Some(pos) => pos,
        None => {
            send_response(&mut stream, 400, "Bad Request", &[], &[]).await?;
            return Ok(());
        }
    };
    let req_token = &path[..slash_pos];
    let encoded_path = &path[slash_pos + 1..];

    // Validate token (byte-level comparison, no String needed)
    if req_token != state.token.as_bytes() {
        send_response(&mut stream, 403, "Forbidden", &[], &[]).await?;
        return Ok(());
    }

    // Decode the file path (only allocation in the happy path)
    let file_path_str = match percent_decode_bytes(encoded_path) {
        Some(s) => s,
        None => {
            send_response(&mut stream, 400, "Bad Request", &[], &[]).await?;
            return Ok(());
        }
    };

    // Canonicalize to resolve any `..` traversal, then validate against allowed dirs
    let file_path = match tokio::fs::canonicalize(&file_path_str).await {
        Ok(p) => p,
        Err(_) => {
            send_response(&mut stream, 404, "Not Found", &[], &[]).await?;
            return Ok(());
        }
    };

    if !state.is_path_allowed(&file_path) {
        send_response(&mut stream, 403, "Forbidden", &[], &[]).await?;
        return Ok(());
    }

    // Open the file
    let mut file = match tokio::fs::File::open(&file_path).await {
        Ok(f) => f,
        Err(_) => {
            send_response(&mut stream, 404, "Not Found", &[], &[]).await?;
            return Ok(());
        }
    };

    let metadata = file.metadata().await?;
    let total_size = metadata.len();

    // Determine MIME type from extension (zero-alloc — returns &'static str)
    let mime = file_path
        .extension()
        .and_then(|e| e.to_str())
        .map(mime_from_extension_static)
        .unwrap_or("application/octet-stream");

    // Parse Range header (byte-level scan, no to_ascii_lowercase allocation)
    let range = parse_range_header(req, total_size);

    let is_head = method == Method::Head;

    // Common trailing headers (static, written once per response)
    const COMMON_HEADERS: &[u8] = b"Accept-Ranges: bytes\r\n\
        Access-Control-Allow-Origin: *\r\n\
        Access-Control-Expose-Headers: Content-Range, Content-Length, Accept-Ranges\r\n\
        Connection: close\r\n\r\n";

    match range {
        Some((start, end)) => {
            // 206 Partial Content — use write! to a pre-sized buffer
            let content_length = end - start + 1;
            use std::io::Write;
            let mut hdr = Vec::with_capacity(256);
            let _ = write!(hdr,
                "HTTP/1.1 206 Partial Content\r\n\
                 Content-Type: {mime}\r\n\
                 Content-Length: {content_length}\r\n\
                 Content-Range: bytes {start}-{end}/{total_size}\r\n");
            hdr.extend_from_slice(COMMON_HEADERS);
            stream.write_all(&hdr).await?;

            if !is_head {
                file.seek(std::io::SeekFrom::Start(start)).await?;
                send_file_chunk(&mut stream, &mut file, content_length).await?;
            }
        }
        None => {
            // 200 OK — full file
            use std::io::Write;
            let mut hdr = Vec::with_capacity(256);
            let _ = write!(hdr,
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: {mime}\r\n\
                 Content-Length: {total_size}\r\n");
            hdr.extend_from_slice(COMMON_HEADERS);
            stream.write_all(&hdr).await?;

            if !is_head {
                send_file_chunk(&mut stream, &mut file, total_size).await?;
            }
        }
    }

    Ok(())
}

/// Send file data in chunks to avoid loading the entire file into memory.
async fn send_file_chunk(
    stream: &mut tokio::net::TcpStream,
    file: &mut tokio::fs::File,
    mut remaining: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buf = vec![0u8; 64 * 1024]; // 64KB chunks
    while remaining > 0 {
        let to_read = (remaining as usize).min(buf.len());
        let n = file.read(&mut buf[..to_read]).await?;
        if n == 0 {
            break;
        }
        stream.write_all(&buf[..n]).await?;
        remaining -= n as u64;
    }
    Ok(())
}

/// Parse the `Range` header from raw HTTP request bytes.
///
/// Scans line-by-line using byte operations — no `to_ascii_lowercase()`,
/// no `String` allocation. Returns `Some((start, end))` inclusive byte range.
fn parse_range_header(req: &[u8], total_size: u64) -> Option<(u64, u64)> {
    // Empty files have no valid byte ranges
    if total_size == 0 {
        return None;
    }

    // Scan for "\r\nRange:" or "\r\nrange:" (case-insensitive first char)
    let mut pos = 0;
    while pos < req.len() {
        // Find next line start (after \r\n or \n)
        let line_start = if pos == 0 {
            // Skip the request line — find first \n
            match memchr::memchr(b'\n', &req[pos..]) {
                Some(p) => pos + p + 1,
                None => return None,
            }
        } else {
            pos
        };

        if line_start >= req.len() {
            break;
        }

        // Find end of this header line
        let line_end = match memchr::memchr(b'\n', &req[line_start..]) {
            Some(p) => line_start + p,
            None => req.len(),
        };
        let line = &req[line_start..line_end];

        // Check if this line starts with "Range:" (case-insensitive)
        if line.len() >= 6 && line[5] == b':'
            && (line[0] == b'R' || line[0] == b'r')
            && (line[1] == b'a' || line[1] == b'A')
            && (line[2] == b'n' || line[2] == b'N')
            && (line[3] == b'g' || line[3] == b'G')
            && (line[4] == b'e' || line[4] == b'E')
        {
            // Found Range header — parse the value
            let value = trim_bytes(&line[6..]);

            // Must start with "bytes="
            if !value.starts_with(b"bytes=") {
                return None;
            }
            let spec = &value[6..];

            // Take first range (before any comma)
            let range_end = memchr::memchr(b',', spec).unwrap_or(spec.len());
            let range_part = trim_bytes(&spec[..range_end]);

            if range_part.is_empty() {
                return None;
            }

            if range_part[0] == b'-' {
                // bytes=-N (last N bytes)
                let n = parse_u64(&range_part[1..])?;
                if n == 0 || n > total_size {
                    return None;
                }
                return Some((total_size - n, total_size - 1));
            }

            let dash = memchr::memchr(b'-', range_part)?;
            let start = parse_u64(trim_bytes(&range_part[..dash]))?;
            let after_dash = trim_bytes(&range_part[dash + 1..]);

            let end = if after_dash.is_empty() {
                total_size - 1
            } else {
                parse_u64(after_dash)?
            };

            if start > end || start >= total_size {
                return None;
            }

            return Some((start, end.min(total_size - 1)));
        }

        pos = line_end + 1;
    }
    None
}

/// Parse a u64 from ASCII digit bytes (no allocation, no str conversion).
#[inline]
fn parse_u64(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    let mut n: u64 = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add((b - b'0') as u64)?;
    }
    Some(n)
}

/// Trim leading/trailing ASCII whitespace from a byte slice.
#[inline]
fn trim_bytes(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|&c| c != b' ' && c != b'\t' && c != b'\r').unwrap_or(b.len());
    let end = b.iter().rposition(|&c| c != b' ' && c != b'\t' && c != b'\r').map_or(start, |p| p + 1);
    &b[start..end]
}

/// Send a simple HTTP response.
async fn send_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    reason: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut response = format!("HTTP/1.1 {status} {reason}\r\n");
    for (key, val) in headers {
        response.push_str(&format!("{key}: {val}\r\n"));
    }
    if !body.is_empty() {
        response.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    response.push_str("Connection: close\r\n\r\n");
    stream.write_all(response.as_bytes()).await?;
    if !body.is_empty() {
        stream.write_all(body).await?;
    }
    Ok(())
}

/// Percent-decode a URL path from raw bytes.
fn percent_decode_bytes(input: &[u8]) -> Option<String> {
    let mut result = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' {
            if i + 2 >= input.len() {
                return None;
            }
            let hi = hex_digit(input[i + 1])?;
            let lo = hex_digit(input[i + 2])?;
            result.push((hi << 4) | lo);
            i += 3;
        } else {
            result.push(input[i]);
            i += 1;
        }
    }
    String::from_utf8(result).ok()
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
