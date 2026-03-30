pub mod guarded_key;
pub use guarded_key::GuardedKey;

mod signer;
pub use signer::GuardedSigner;

use argon2::Argon2;
use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit};
use zeroize::Zeroize;

/// Derive a 32-byte key from a password using Argon2id.
/// Parameters: 150MB memory, 10 iterations (matches src-tauri).
pub async fn hash_pass(password: &str) -> [u8; 32] {
    let password = password.to_string();
    tokio::task::spawn_blocking(move || {
        let salt = b"vectorvectovectvecvev";
        let mut output = [0u8; 32];

        let params = argon2::Params::new(
            150_000, // 150 MB
            10,      // iterations
            1,       // parallelism
            Some(32),
        ).unwrap();

        let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        argon2.hash_password_into(password.as_bytes(), salt, &mut output).unwrap();

        output
    }).await.unwrap()
}

/// Encrypt a string with the global ENCRYPTION_KEY (ChaCha20-Poly1305).
pub fn encrypt_with_key(plaintext: &str, key: &[u8; 32]) -> Result<String, String> {
    use chacha20poly1305::aead::OsRng;
    use chacha20poly1305::AeadCore;

    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);

    let ciphertext = cipher.encrypt(&nonce, plaintext.as_bytes())
        .map_err(|e| format!("Encryption failed: {}", e))?;

    // Encode as hex: nonce + ciphertext
    let mut result = hex::encode(&nonce[..]);
    result.push_str(&hex::encode(&ciphertext));
    Ok(result)
}

/// Decrypt a hex-encoded ciphertext with a key.
pub fn decrypt_with_key(hex_data: &str, key: &[u8; 32]) -> Result<String, String> {
    if hex_data.len() < 24 {
        return Err("Ciphertext too short".to_string());
    }

    let nonce_hex = &hex_data[..24]; // 12 bytes = 24 hex chars
    let ciphertext_hex = &hex_data[24..];

    let nonce_bytes = hex::decode(nonce_hex)
        .map_err(|e| format!("Invalid nonce hex: {}", e))?;
    let ciphertext = hex::decode(ciphertext_hex)
        .map_err(|e| format!("Invalid ciphertext hex: {}", e))?;

    let nonce_arr: [u8; 12] = nonce_bytes.try_into()
        .map_err(|_| "Invalid nonce length".to_string())?;
    let nonce = chacha20poly1305::Nonce::from(nonce_arr);
    let cipher = ChaCha20Poly1305::new(key.into());

    let mut plaintext = cipher.decrypt(&nonce, ciphertext.as_ref())
        .map_err(|_| "Decryption failed (wrong key or corrupted data)".to_string())?;

    let result = String::from_utf8(plaintext.clone())
        .map_err(|_| "Decrypted data is not valid UTF-8".to_string())?;

    plaintext.zeroize();
    Ok(result)
}

/// Check if encryption is enabled in the database.
pub fn is_encryption_enabled() -> bool {
    crate::db::get_sql_setting("encryption_enabled".to_string())
        .ok().flatten()
        .map(|v| v != "false")
        .unwrap_or(false)
}

/// Simple hex encode/decode (for crypto module internal use).
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }

    pub fn decode(hex: &str) -> Result<Vec<u8>, String> {
        if hex.len() % 2 != 0 {
            return Err("Odd-length hex string".to_string());
        }
        (0..hex.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&hex[i..i + 2], 16)
                    .map_err(|e| format!("Invalid hex: {}", e))
            })
            .collect()
    }
}

// ============================================================================
// AES-256-GCM File Encryption (for NIP-96/Blossom attachments)
// ============================================================================

/// Parameters for AES-256-GCM file encryption (hex-encoded key + nonce).
#[derive(Debug)]
pub struct EncryptionParams {
    pub key: String,   // 32-byte key as hex
    pub nonce: String, // 16-byte nonce as hex (0xChat-compatible)
}

/// Generate random AES-256-GCM encryption parameters.
pub fn generate_encryption_params() -> EncryptionParams {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut key: [u8; 32] = rng.gen();
    let nonce: [u8; 16] = rng.gen();
    let params = EncryptionParams {
        key: hex::encode(&key),
        nonce: hex::encode(&nonce),
    };
    key.iter_mut().for_each(|b| *b = 0); // zeroize
    params
}

/// Encrypt data with AES-256-GCM using a 16-byte nonce (0xChat-compatible).
pub fn encrypt_data(data: &[u8], params: &EncryptionParams) -> Result<Vec<u8>, String> {
    use aes::Aes256;
    use aes::cipher::typenum::U16;
    use aes_gcm::{AesGcm, AeadInPlace, KeyInit as AesKeyInit};

    let key_bytes = hex::decode(&params.key).map_err(|e| format!("Invalid key: {}", e))?;
    let nonce_bytes = hex::decode(&params.nonce).map_err(|e| format!("Invalid nonce: {}", e))?;

    let cipher = AesGcm::<Aes256, U16>::new_from_slice(&key_bytes)
        .map_err(|_| "Invalid encryption key".to_string())?;

    let nonce_arr: [u8; 16] = nonce_bytes.try_into()
        .map_err(|_| "Invalid nonce length".to_string())?;
    let nonce = aes_gcm::Nonce::<U16>::from(nonce_arr);

    let mut buffer = data.to_vec();
    let tag = cipher.encrypt_in_place_detached(&nonce, &[], &mut buffer)
        .map_err(|_| "Encryption failed".to_string())?;

    buffer.extend_from_slice(&tag);
    Ok(buffer)
}

/// Decrypt data with AES-256-GCM using a 16-byte nonce (0xChat-compatible).
/// Input format: ciphertext || 16-byte auth tag.
pub fn decrypt_data(encrypted_data: &[u8], key_hex: &str, nonce_hex: &str) -> Result<Vec<u8>, String> {
    use aes::Aes256;
    use aes::cipher::typenum::U16;
    use aes_gcm::{AesGcm, AeadInPlace, KeyInit as AesKeyInit};

    if encrypted_data.len() < 16 {
        return Err(format!("Invalid Input: encrypted data too small ({} bytes, minimum 16 bytes required for authentication tag)", encrypted_data.len()));
    }

    let key_bytes = hex::decode(key_hex).map_err(|e| format!("Invalid key: {}", e))?;
    let nonce_bytes = hex::decode(nonce_hex).map_err(|e| format!("Invalid nonce: {}", e))?;

    let (ciphertext, tag_bytes) = encrypted_data.split_at(encrypted_data.len() - 16);

    let cipher = AesGcm::<Aes256, U16>::new_from_slice(&key_bytes)
        .map_err(|_| "Invalid decryption key".to_string())?;

    let nonce_arr: [u8; 16] = nonce_bytes.try_into()
        .map_err(|_| "Invalid nonce length".to_string())?;
    let nonce = aes_gcm::Nonce::<U16>::from(nonce_arr);
    let tag_arr: [u8; 16] = tag_bytes.try_into()
        .map_err(|_| "Invalid tag length".to_string())?;
    let tag = aes_gcm::Tag::<U16>::from(tag_arr);

    let mut buffer = ciphertext.to_vec();
    cipher.decrypt_in_place_detached(&nonce, &[], &mut buffer, &tag)
        .map_err(|e| e.to_string())?;

    Ok(buffer)
}

/// Calculate SHA-256 hash of data, returned as hex string.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(&hasher.finalize())
}

/// Get MIME type from file extension.
pub fn mime_from_extension(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "tiff" | "tif" => "image/tiff",
        "dng" => "image/x-adobe-dng",
        "cr2" => "image/x-canon-cr2",
        "nef" => "image/x-nikon-nef",
        "arw" => "image/x-sony-arw",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "mkv" => "video/x-matroska",
        "flv" => "video/x-flv",
        "wmv" => "video/x-ms-wmv",
        "mpg" | "mpeg" => "video/mpeg",
        "3gp" => "video/3gpp",
        "ogv" => "video/ogg",
        "ts" => "video/mp2t",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wma" => "audio/x-ms-wma",
        "opus" => "audio/opus",
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "rtf" => "application/rtf",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "json" => "application/json",
        "xml" => "application/xml",
        "yaml" | "yml" => "application/x-yaml",
        "toml" => "application/toml",
        "sql" => "application/sql",
        "zip" => "application/zip",
        "rar" => "application/vnd.rar",
        "7z" => "application/x-7z-compressed",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        "bz2" => "application/x-bzip2",
        "xz" => "application/x-xz",
        "iso" => "application/x-iso9660-image",
        "dmg" => "application/x-apple-diskimage",
        "apk" => "application/vnd.android.package-archive",
        "jar" => "application/java-archive",
        "xdc" => "application/vnd.webxdc+zip",
        "obj" => "model/obj",
        "gltf" => "model/gltf+json",
        "glb" => "model/gltf-binary",
        "stl" => "model/stl",
        "dae" => "model/vnd.collada+xml",
        "js" => "text/javascript",
        "py" => "text/x-python",
        "rs" => "text/x-rust",
        "go" => "text/x-go",
        "java" => "text/x-java",
        "c" => "text/x-c",
        "cpp" => "text/x-c++",
        "cs" => "text/x-csharp",
        "rb" => "text/x-ruby",
        "php" => "text/x-php",
        "swift" => "text/x-swift",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "exe" => "application/x-msdownload",
        "msi" => "application/x-msi",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// Convert a MIME type to a file extension.
/// Falls back to using the MIME subtype when unknown.
pub fn extension_from_mime(mime: &str) -> String {
    match mime.trim().to_lowercase().as_str() {
        // Images
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/bmp" | "image/x-ms-bmp" => "bmp",
        "image/x-icon" | "image/vnd.microsoft.icon" => "ico",
        "image/tiff" => "tiff",
        "image/x-adobe-dng" => "dng",
        "image/x-canon-cr2" => "cr2",
        "image/x-nikon-nef" => "nef",
        "image/x-sony-arw" => "arw",
        // Audio
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/mp3" | "audio/mpeg" => "mp3",
        "audio/flac" => "flac",
        "audio/ogg" => "ogg",
        "audio/mp4" => "m4a",
        "audio/aac" | "audio/x-aac" => "aac",
        "audio/x-ms-wma" => "wma",
        "audio/opus" => "opus",
        // Video
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "video/x-msvideo" => "avi",
        "video/x-matroska" => "mkv",
        "video/x-flv" => "flv",
        "video/x-ms-wmv" => "wmv",
        "video/mpeg" => "mpg",
        "video/3gpp" => "3gp",
        "video/ogg" => "ogv",
        "video/mp2t" => "ts",
        // Documents
        "application/pdf" => "pdf",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "application/vnd.oasis.opendocument.text" => "odt",
        "application/vnd.oasis.opendocument.spreadsheet" => "ods",
        "application/vnd.oasis.opendocument.presentation" => "odp",
        "application/rtf" => "rtf",
        // Text/Data
        "text/plain" => "txt",
        "text/markdown" => "md",
        "text/csv" => "csv",
        "application/json" => "json",
        "application/xml" | "text/xml" => "xml",
        "application/x-yaml" | "text/yaml" => "yaml",
        "application/toml" => "toml",
        "application/sql" => "sql",
        // Archives
        "application/zip" => "zip",
        "application/x-rar-compressed" | "application/vnd.rar" => "rar",
        "application/x-7z-compressed" => "7z",
        "application/x-tar" => "tar",
        "application/gzip" => "gz",
        "application/x-bzip2" => "bz2",
        "application/x-xz" => "xz",
        "application/x-iso9660-image" => "iso",
        "application/x-apple-diskimage" => "dmg",
        "application/vnd.android.package-archive" => "apk",
        "application/java-archive" => "jar",
        "application/vnd.webxdc+zip" => "xdc",
        // 3D
        "model/obj" => "obj",
        "model/gltf+json" => "gltf",
        "model/gltf-binary" => "glb",
        "model/stl" | "application/sla" => "stl",
        "model/vnd.collada+xml" => "dae",
        // Code
        "text/javascript" | "application/javascript" => "js",
        "text/typescript" | "application/typescript" => "ts",
        "text/x-python" | "application/x-python" => "py",
        "text/x-rust" => "rs",
        "text/x-go" => "go",
        "text/x-java" => "java",
        "text/x-c" => "c",
        "text/x-c++" => "cpp",
        "text/x-csharp" => "cs",
        "text/x-ruby" => "rb",
        "text/x-php" => "php",
        "text/x-swift" => "swift",
        // Web
        "text/html" => "html",
        "text/css" => "css",
        // Other
        "application/x-msdownload" | "application/x-dosexec" => "exe",
        "application/x-msi" => "msi",
        "application/x-font-ttf" | "font/ttf" => "ttf",
        "application/x-font-otf" | "font/otf" => "otf",
        "font/woff" => "woff",
        "font/woff2" => "woff2",
        // Fallback: extract subtype
        _ => {
            let lower = mime.trim().to_lowercase();
            return lower.split('/').nth(1).unwrap_or("bin").to_string();
        }
    }.to_string()
}

/// Sanitize a filename for safe filesystem use.
/// Strips path traversal, dangerous characters, and truncates to 64-char stem.
pub fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit('/').next().unwrap_or(name);
    let base = base.rsplit('\\').next().unwrap_or(base);

    let sanitized: String = base.chars().filter(|c| {
        !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0')
    }).collect();

    let sanitized = sanitized.trim_matches(|c: char| c == '.' || c == ' ');

    if sanitized.is_empty() {
        return String::new();
    }

    if let Some(dot_pos) = sanitized.rfind('.') {
        let stem = &sanitized[..dot_pos];
        let ext = &sanitized[dot_pos..];
        if stem.len() > 64 {
            let truncated = &stem[..stem.floor_char_boundary(64)];
            return format!("{}{}", truncated, ext);
        }
    } else if sanitized.len() > 64 {
        let truncated = &sanitized[..sanitized.floor_char_boundary(64)];
        return truncated.to_string();
    }

    sanitized.to_string()
}

/// Resolve a unique filename in `dir`, appending `-1`, `-2`, etc. on collision.
///
/// If `dir/name` doesn't exist, returns it as-is. Otherwise increments a
/// counter on the stem: `photo.jpg` → `photo-1.jpg` → `photo-2.jpg` ...
pub fn resolve_unique_filename(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }

    let stem = std::path::Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    let mut counter = 1u32;
    loop {
        let suffixed = if ext.is_empty() {
            format!("{}-{}", stem, counter)
        } else {
            format!("{}-{}.{}", stem, counter, ext)
        };
        let candidate = dir.join(&suffixed);
        if !candidate.exists() {
            return candidate;
        }
        counter += 1;
    }
}

/// Format bytes into human-readable format (KB, MB, GB).
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    if bytes < KB as u64 {
        format!("{} B", bytes)
    } else if bytes < MB as u64 {
        format!("{:.1} KB", bytes as f64 / KB)
    } else if bytes < GB as u64 {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{:.1} GB", bytes as f64 / GB)
    }
}

/// Returns true if the provided MIME type is an image/*.
pub fn is_image_mime(mime: &str) -> bool {
    mime.trim().starts_with("image/")
}

/// Convert a file extension to a MIME type, with an optional restriction to image/* types.
pub fn mime_from_extension_safe(extension: &str, image_only: bool) -> Result<String, String> {
    let mime = mime_from_extension(extension).to_string();
    if image_only && !is_image_mime(&mime) {
        return Err(mime);
    }
    Ok(mime)
}

/// Detect MIME type from file magic bytes.
/// Supports PNG, JPEG, GIF, WebP, TIFF, ICO, and SVG.
/// Returns "application/octet-stream" for unrecognized formats.
pub fn mime_from_magic_bytes(bytes: &[u8]) -> &'static str {
    if bytes.len() < 4 {
        return "application/octet-stream";
    }
    match bytes[0] {
        0x89 if bytes[1..4] == [0x50, 0x4E, 0x47] => "image/png",
        0xFF if bytes[1..3] == [0xD8, 0xFF] => "image/jpeg",
        b'G' if bytes.len() >= 6 && (bytes[..6] == *b"GIF87a" || bytes[..6] == *b"GIF89a") => "image/gif",
        b'R' if bytes.len() > 12 && bytes[..4] == *b"RIFF" && bytes[8..12] == *b"WEBP" => "image/webp",
        0x49 if bytes[1..4] == [0x49, 0x2A, 0x00] => "image/tiff",
        0x4D if bytes[1..4] == [0x4D, 0x00, 0x2A] => "image/tiff",
        0x00 if bytes[1..4] == [0x00, 0x01, 0x00] => "image/x-icon",
        b'<' if bytes.starts_with(b"<?xml") || bytes.starts_with(b"<svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // encrypt_with_key / decrypt_with_key roundtrip tests
    // ========================================================================

    fn test_key() -> [u8; 32] {
        [0x42u8; 32]
    }

    fn alt_key() -> [u8; 32] {
        [0x99u8; 32]
    }

    #[test]
    fn encrypt_decrypt_roundtrip_simple() {
        let key = test_key();
        let plaintext = "hello world";
        let encrypted = encrypt_with_key(plaintext, &key).expect("encryption should succeed");
        let decrypted = decrypt_with_key(&encrypted, &key).expect("decryption should succeed");
        assert_eq!(decrypted, plaintext, "roundtrip should preserve plaintext");
    }

    #[test]
    fn encrypt_decrypt_100_random_strings() {
        use rand::Rng;
        let key = test_key();
        let mut rng = rand::thread_rng();
        for i in 0..100 {
            let len = rng.gen_range(1..=200);
            let s: String = (0..len).map(|_| rng.gen_range(0x20u8..0x7f) as char).collect();
            let enc = encrypt_with_key(&s, &key)
                .unwrap_or_else(|e| panic!("encryption failed on iteration {}: {}", i, e));
            let dec = decrypt_with_key(&enc, &key)
                .unwrap_or_else(|e| panic!("decryption failed on iteration {}: {}", i, e));
            assert_eq!(dec, s, "roundtrip failed on iteration {}", i);
        }
    }

    #[test]
    fn encrypt_decrypt_empty_string() {
        let key = test_key();
        let encrypted = encrypt_with_key("", &key).expect("encrypting empty string should succeed");
        let decrypted = decrypt_with_key(&encrypted, &key).expect("decrypting empty string should succeed");
        assert_eq!(decrypted, "", "empty string roundtrip should produce empty string");
    }

    #[test]
    fn encrypt_decrypt_large_string() {
        let key = test_key();
        let plaintext = "A".repeat(10 * 1024); // 10 KB
        let encrypted = encrypt_with_key(&plaintext, &key).expect("encrypting 10KB should succeed");
        let decrypted = decrypt_with_key(&encrypted, &key).expect("decrypting 10KB should succeed");
        assert_eq!(decrypted, plaintext, "10KB roundtrip should preserve content");
    }

    #[test]
    fn decrypt_with_wrong_key_fails() {
        let key = test_key();
        let wrong_key = alt_key();
        let encrypted = encrypt_with_key("secret data", &key).expect("encryption should succeed");
        let result = decrypt_with_key(&encrypted, &wrong_key);
        assert!(result.is_err(), "decryption with wrong key should fail");
    }

    #[test]
    fn decrypt_corrupted_ciphertext_fails() {
        let key = test_key();
        let mut encrypted = encrypt_with_key("secret data", &key).expect("encryption should succeed");
        // Corrupt a byte in the ciphertext portion (past the 24-char nonce)
        let bytes: Vec<u8> = encrypted.bytes().collect();
        if bytes.len() > 30 {
            let mut chars: Vec<char> = encrypted.chars().collect();
            // Flip a hex digit in the ciphertext area
            chars[30] = if chars[30] == '0' { 'f' } else { '0' };
            encrypted = chars.into_iter().collect();
        }
        let result = decrypt_with_key(&encrypted, &key);
        assert!(result.is_err(), "decryption of corrupted ciphertext should fail");
    }

    #[test]
    fn different_keys_produce_different_ciphertext() {
        let key1 = test_key();
        let key2 = alt_key();
        let plaintext = "same plaintext";
        let enc1 = encrypt_with_key(plaintext, &key1).expect("enc1 should succeed");
        let enc2 = encrypt_with_key(plaintext, &key2).expect("enc2 should succeed");
        // Ciphertexts after the nonce portion should differ (nonces differ too since random)
        assert_ne!(enc1, enc2, "different keys should produce different ciphertext");
    }

    #[test]
    fn nonce_is_always_different() {
        let key = test_key();
        let plaintext = "same string encrypted twice";
        let enc1 = encrypt_with_key(plaintext, &key).expect("enc1 should succeed");
        let enc2 = encrypt_with_key(plaintext, &key).expect("enc2 should succeed");
        // The first 24 hex chars are the nonce
        let nonce1 = &enc1[..24];
        let nonce2 = &enc2[..24];
        assert_ne!(nonce1, nonce2, "nonces should differ between encryptions of the same plaintext");
    }

    #[test]
    fn unicode_content_preserved() {
        let key = test_key();
        let plaintext = "Hello \u{1F600} \u{1F4A9} \u{1F30D} \u{00E9}\u{00E0}\u{00FC} \u{4E16}\u{754C} \u{0410}\u{0411}\u{0412}";
        let encrypted = encrypt_with_key(plaintext, &key).expect("encrypting unicode should succeed");
        let decrypted = decrypt_with_key(&encrypted, &key).expect("decrypting unicode should succeed");
        assert_eq!(decrypted, plaintext, "unicode content should be preserved through encrypt/decrypt");
    }

    #[test]
    fn decrypt_too_short_ciphertext_fails() {
        let key = test_key();
        let result = decrypt_with_key("abcdef", &key);
        assert!(result.is_err(), "ciphertext shorter than 24 hex chars should fail");
        assert!(result.unwrap_err().contains("too short"), "error should mention too short");
    }

    #[test]
    fn encrypt_output_is_hex_encoded() {
        let key = test_key();
        let encrypted = encrypt_with_key("test", &key).expect("encryption should succeed");
        assert!(encrypted.chars().all(|c| c.is_ascii_hexdigit()),
            "encrypted output should be entirely hex characters");
    }

    #[test]
    fn encrypt_output_has_correct_structure() {
        let key = test_key();
        let encrypted = encrypt_with_key("test", &key).expect("encryption should succeed");
        // Must be at least 24 hex chars (nonce) + some ciphertext
        assert!(encrypted.len() > 24,
            "encrypted output should have nonce (24 hex chars) plus ciphertext");
        // Length should be even (hex pairs)
        assert_eq!(encrypted.len() % 2, 0,
            "encrypted output length should be even (hex pairs)");
    }

    #[test]
    fn encrypt_decrypt_special_characters() {
        let key = test_key();
        let plaintext = r#"!@#$%^&*()_+-=[]{}|;':",.<>?/\`~"#;
        let encrypted = encrypt_with_key(plaintext, &key).expect("encrypting special chars should succeed");
        let decrypted = decrypt_with_key(&encrypted, &key).expect("decrypting special chars should succeed");
        assert_eq!(decrypted, plaintext, "special characters should survive roundtrip");
    }

    #[test]
    fn encrypt_decrypt_newlines_and_whitespace() {
        let key = test_key();
        let plaintext = "line1\nline2\r\nline3\ttab\0null";
        let encrypted = encrypt_with_key(plaintext, &key).expect("encrypting whitespace should succeed");
        let decrypted = decrypt_with_key(&encrypted, &key).expect("decrypting whitespace should succeed");
        assert_eq!(decrypted, plaintext, "whitespace and control chars should survive roundtrip");
    }

    #[test]
    fn decrypt_invalid_hex_in_nonce_fails() {
        let key = test_key();
        // 24 chars of invalid hex + some ciphertext
        let bad = "zzzzzzzzzzzzzzzzzzzzzzzz0000000000000000";
        let result = decrypt_with_key(bad, &key);
        assert!(result.is_err(), "invalid hex in nonce should fail");
    }

    #[test]
    fn decrypt_invalid_hex_in_ciphertext_fails() {
        let key = test_key();
        // Valid nonce (24 hex chars) + invalid ciphertext hex
        let bad = "000000000000000000000000gggggggg";
        let result = decrypt_with_key(bad, &key);
        assert!(result.is_err(), "invalid hex in ciphertext should fail");
    }

    // ========================================================================
    // hex module tests
    // ========================================================================

    #[test]
    fn hex_encode_decode_roundtrip() {
        let data = vec![0x00, 0x01, 0xff, 0x80, 0x7f];
        let encoded = hex::encode(&data);
        let decoded = hex::decode(&encoded).expect("decode should succeed");
        assert_eq!(decoded, data, "hex encode/decode roundtrip should preserve bytes");
    }

    #[test]
    fn hex_decode_odd_length_error() {
        let result = hex::decode("abc");
        assert!(result.is_err(), "odd-length hex string should fail");
        assert!(result.unwrap_err().contains("Odd-length"), "error should mention odd-length");
    }

    #[test]
    fn hex_decode_invalid_hex_error() {
        let result = hex::decode("zzzz");
        assert!(result.is_err(), "invalid hex characters should fail");
    }

    #[test]
    fn hex_encode_empty() {
        assert_eq!(hex::encode(&[]), "", "encoding empty bytes should produce empty string");
    }

    #[test]
    fn hex_decode_empty() {
        let decoded = hex::decode("").expect("decoding empty string should succeed");
        assert!(decoded.is_empty(), "decoding empty string should produce empty vec");
    }

    // ========================================================================
    // hash_pass tests
    // ========================================================================

    #[tokio::test]
    async fn hash_pass_deterministic() {
        let key1 = hash_pass("my_password").await;
        let key2 = hash_pass("my_password").await;
        assert_eq!(key1, key2, "same password should always produce the same key");
    }

    #[tokio::test]
    async fn hash_pass_different_passwords_different_keys() {
        let key1 = hash_pass("password_one").await;
        let key2 = hash_pass("password_two").await;
        assert_ne!(key1, key2, "different passwords should produce different keys");
    }

    #[tokio::test]
    async fn hash_pass_output_is_32_bytes() {
        let key = hash_pass("test_password").await;
        assert_eq!(key.len(), 32, "hash_pass should produce exactly 32 bytes");
        // Ensure it is not all zeros (i.e. hashing actually happened)
        assert!(key.iter().any(|&b| b != 0), "hash output should not be all zeros");
    }
}
