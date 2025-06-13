use once_cell::sync::Lazy;
use std::collections::HashMap;
use sha2::{Sha256, Digest};
use std::path::Path;
use blurhash::decode;
use base64::{Engine as _, engine::general_purpose};
use image::ImageEncoder;

/// Extract all HTTPS URLs from a string
pub fn extract_https_urls(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut start_idx = 0;

    while let Some(https_idx) = text[start_idx..].find("https://") {
        let abs_start = start_idx + https_idx;
        let url_text = &text[abs_start..];

        // Find the end of the URL (first whitespace or common URL-ending chars)
        let mut end_idx = url_text
            .find(|c: char| {
                c.is_whitespace()
                    || c == '"'
                    || c == '<'
                    || c == '>'
                    || c == ')'
                    || c == ']'
                    || c == '}'
                    || c == '|'
            })
            .unwrap_or(url_text.len());

        // Trim trailing punctuation
        while end_idx > 0 {
            let last_char = url_text[..end_idx].chars().last().unwrap();
            if last_char == '.' || last_char == ',' || last_char == ':' || last_char == ';' {
                end_idx -= 1;
            } else {
                break;
            }
        }

        if end_idx > "https://".len() {
            urls.push(url_text[..end_idx].to_string());
        }

        start_idx = abs_start + 1;
    }

    urls
}

/// Creates a description of a file type based on its extension.
pub fn get_file_type_description(extension: &str) -> String {
    // Define file types with descriptions
    static FILE_TYPES: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
        let mut map = HashMap::new();

        // Images
        map.insert("png", "Picture");
        map.insert("jpg", "Picture");
        map.insert("jpeg", "Picture");
        map.insert("gif", "GIF Animation");
        map.insert("webp", "Picture");

        // Audio
        map.insert("wav", "Voice Message");
        map.insert("mp3", "Audio Clip");
        map.insert("m4a", "Audio Clip");
        map.insert("aac", "Audio Clip");
        map.insert("flac", "Audio Clip");
        map.insert("ogg", "Audio Clip");

        // Videos
        map.insert("mp4", "Video");
        map.insert("webm", "Video");
        map.insert("mov", "Video");
        map.insert("avi", "Video");
        map.insert("mkv", "Video");

        map
    });

    // Normalize the extension to lowercase
    let normalized_ext = extension.to_lowercase();

    // Return the file type description if found, otherwise return default value
    FILE_TYPES
        .get(normalized_ext.as_str())
        .copied()
        .unwrap_or("File")
        .to_string()
}

/// Convert a byte slice to a hex string
pub fn bytes_to_hex_string(bytes: &[u8]) -> String {
    // Pre-allocate the exact size needed (2 hex chars per byte)
    let mut result = String::with_capacity(bytes.len() * 2);
    
    // Use a lookup table for hex conversion
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
    
    for &b in bytes {
        // Extract high and low nibbles
        let high = b >> 4;
        let low = b & 0xF;
        result.push(HEX_CHARS[high as usize] as char);
        result.push(HEX_CHARS[low as usize] as char);
    }
    
    result
}

/// Convert hex string back to bytes for decryption
pub fn hex_string_to_bytes(s: &str) -> Vec<u8> {
    // Pre-allocate the result vector to avoid resize operations
    let mut result = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    
    // Process bytes directly to avoid UTF-8 decoding overhead
    let mut i = 0;
    while i + 1 < bytes.len() {
        // Convert two hex characters to a single byte
        let high = match bytes[i] {
            b'0'..=b'9' => bytes[i] - b'0',
            b'a'..=b'f' => bytes[i] - b'a' + 10,
            b'A'..=b'F' => bytes[i] - b'A' + 10,
            _ => 0,
        };
        
        let low = match bytes[i + 1] {
            b'0'..=b'9' => bytes[i + 1] - b'0',
            b'a'..=b'f' => bytes[i + 1] - b'a' + 10,
            b'A'..=b'F' => bytes[i + 1] - b'A' + 10,
            _ => 0,
        };
        
        result.push((high << 4) | low);
        i += 2;
    }
    
    result
}

/// Calculate SHA-256 hash of file data
pub fn calculate_file_hash(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Check if a filename looks like a nonce (shorter than a SHA-256 hash)
/// SHA-256 hashes are 64 characters, nonces are typically 32 characters
pub fn is_nonce_filename(filename: &str) -> bool {
    // Extract the base name without extension
    let base_name = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    
    // Check if it's hex and shorter than SHA-256 hash length
    base_name.len() < 64 && base_name.chars().all(|c| c.is_ascii_hexdigit())
}

/// Migrate a nonce-based file to hash-based naming
/// Returns the new hash-based filename if successful
pub fn migrate_nonce_file_to_hash(file_path: &Path) -> Result<String, std::io::Error> {
    // Read the file content
    let data = std::fs::read(file_path)?;
    
    // Calculate the hash
    let hash = calculate_file_hash(&data);
    
    // Get the extension
    let extension = file_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    
    // Create new hash-based filename
    let new_filename = if extension.is_empty() {
        hash.clone()
    } else {
        format!("{}.{}", hash, extension)
    };
    
    // Create new path in same directory
    let new_path = file_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&new_filename);
    
    // Copy to new location (don't delete original yet in case of errors)
    std::fs::copy(file_path, &new_path)?;
    
    // Remove original file only after successful copy
    std::fs::remove_file(file_path)?;
    
    Ok(new_filename)
}

/// Decode a blurhash string to a Base64-encoded PNG data URL
/// Returns a data URL string that can be used directly in an <img> src attribute
pub fn decode_blurhash_to_base64(blurhash: &str, width: u32, height: u32, punch: f32) -> String {
    const EMPTY_DATA_URL: &str = "data:image/png;base64,";
    
    let decoded_data = match decode(blurhash, width, height, punch) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("Failed to decode blurhash: {}", e);
            return EMPTY_DATA_URL.to_string();
        }
    };
    
    let pixel_count = (width * height) as usize;
    let bytes_per_pixel = decoded_data.len() / pixel_count;
    
    // Fast path for RGBA data
    if bytes_per_pixel == 4 {
        encode_rgba_to_png_base64(&decoded_data, width, height)
    } 
    // Convert RGB to RGBA
    else if bytes_per_pixel == 3 {
        // Pre-allocate exact size needed
        let mut rgba_data = Vec::with_capacity(pixel_count * 4);
        
        // Use chunks_exact for safe and efficient iteration
        for rgb_chunk in decoded_data.chunks_exact(3) {
            rgba_data.extend_from_slice(&[rgb_chunk[0], rgb_chunk[1], rgb_chunk[2], 255]);
        }
        
        encode_rgba_to_png_base64(&rgba_data, width, height)
    } else {
        eprintln!("Unexpected decoded data length: {} bytes for {} pixels", 
                 decoded_data.len(), pixel_count);
        EMPTY_DATA_URL.to_string()
    }
}

/// Helper function to encode RGBA data to PNG base64
#[inline]
fn encode_rgba_to_png_base64(rgba_data: &[u8], width: u32, height: u32) -> String {
    const EMPTY_DATA_URL: &str = "data:image/png;base64,";
    
    // Create image without additional allocation
    let img = match image::RgbaImage::from_raw(width, height, rgba_data.to_vec()) {
        Some(img) => img,
        None => {
            eprintln!("Failed to create image from RGBA data");
            return EMPTY_DATA_URL.to_string();
        }
    };
    
    // Pre-allocate PNG buffer with estimated size
    // PNG is typically smaller than raw RGBA, estimate 50% of original size
    let estimated_size = (rgba_data.len() / 2).max(1024);
    let mut png_data = Vec::with_capacity(estimated_size);
    
    let encoder = image::codecs::png::PngEncoder::new(&mut png_data);
    if let Err(e) = encoder.write_image(
        img.as_raw(),
        width,
        height,
        image::ExtendedColorType::Rgba8
    ) {
        eprintln!("Failed to encode PNG: {}", e);
        return EMPTY_DATA_URL.to_string();
    }
    
    // Encode as base64 with pre-allocated string
    // Base64 is 4/3 the size of input + padding
    let base64_capacity = ((png_data.len() * 4 / 3) + 4) + 22; // +22 for "data:image/png;base64,"
    let mut result = String::with_capacity(base64_capacity);
    result.push_str("data:image/png;base64,");
    general_purpose::STANDARD.encode_string(&png_data, &mut result);
    
    result
}
