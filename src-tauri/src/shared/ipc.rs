//! Raw-bytes IPC helpers.
//!
//! Passing file bytes as a `Vec<u8>` command arg makes Tauri ship them as a JSON
//! `number[]` (~3.5x the size, plus a `stringify` on the JS side and a number
//! parse on the Rust side). Instead a command takes `tauri::ipc::Request`, reads
//! the bytes from the raw binary body, and reads small metadata from headers.
//!
//! Extract everything up front (both helpers return owned values) so no `Request`
//! borrow is held across an `.await` inside an async command.

/// Clone the raw binary body of a `Request`-based command.
pub fn raw_body(request: &tauri::ipc::Request<'_>) -> Result<Vec<u8>, String> {
    match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => Ok(bytes.clone()),
        _ => Err("expected a raw byte IPC body".to_string()),
    }
}

/// An owned copy of a string request header, if present and valid UTF-8.
pub fn header(request: &tauri::ipc::Request<'_>, name: &str) -> Option<String> {
    request
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// A base64-decoded string header. Header values are ASCII-only, so any field
/// that may carry non-ASCII (e.g. a filename) is base64'd by the caller.
pub fn header_b64(request: &tauri::ipc::Request<'_>, name: &str) -> Option<String> {
    let raw = header(request, name)?;
    let bytes = base64_simd::STANDARD.decode_to_vec(raw).ok()?;
    String::from_utf8(bytes).ok()
}
