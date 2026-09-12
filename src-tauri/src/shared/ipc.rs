//! Raw-bytes IPC helpers.
//!
//! Passing file bytes as a `Vec<u8>` command arg makes Tauri ship them as a JSON
//! `number[]` (~3.5x the size, plus a `stringify` on the JS side and a number
//! parse on the Rust side). Instead a command takes `tauri::ipc::Request`, reads
//! the bytes from the raw binary body, and reads small metadata from headers.
//!
//! Extract everything up front (both helpers return owned values) so no `Request`
//! borrow is held across an `.await` inside an async command.

/// Clone the binary body of a `Request`-based command.
///
/// Android has no raw IPC: every invoke rides `postMessage` as JSON, so a top-level
/// `Uint8Array` arrives as a number array. Accepting that shape keeps the command
/// working there, at JSON cost, while desktop stays on the raw body.
pub fn raw_body(request: &tauri::ipc::Request<'_>) -> Result<Vec<u8>, String> {
    match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => Ok(bytes.clone()),
        tauri::ipc::InvokeBody::Json(serde_json::Value::Array(items)) => {
            bytes_from_json_array(items).ok_or_else(|| "expected a byte array IPC body".to_string())
        }
        _ => Err("expected a raw byte IPC body".to_string()),
    }
}

/// A JSON array is bytes only when every element is an integer in 0..=255.
fn bytes_from_json_array(items: &[serde_json::Value]) -> Option<Vec<u8>> {
    items
        .iter()
        .map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::bytes_from_json_array;
    use serde_json::json;

    #[test]
    fn only_byte_valued_integers_are_bytes() {
        let ok = json!([0, 255, 7]);
        assert_eq!(bytes_from_json_array(ok.as_array().unwrap()), Some(vec![0, 255, 7]));
        for bad in [json!([256]), json!([-1]), json!([1.5]), json!(["a"]), json!([null]), json!([[1]])] {
            assert_eq!(bytes_from_json_array(bad.as_array().unwrap()), None, "{bad}");
        }
        assert_eq!(bytes_from_json_array(&[]), Some(vec![]));
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
