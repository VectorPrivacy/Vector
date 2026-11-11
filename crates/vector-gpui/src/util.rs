//! What the desktop client's `util` gives the image code compiled in from it.

pub fn data_uri(mime: &str, bytes: &[u8]) -> String {
    let mut result = String::with_capacity(13 + mime.len() + bytes.len().div_ceil(3) * 4);
    result.push_str("data:");
    result.push_str(mime);
    result.push_str(";base64,");
    vector_core::simd::base64::STANDARD.encode_append(bytes, &mut result);
    result
}

pub use crate::simd::image::has_alpha_transparency;
