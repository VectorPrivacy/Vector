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

/// A thumbhash of `img`, drawn from a 100 px thumbnail.
pub fn generate_thumbhash_from_image(img: &image::DynamicImage) -> Option<String> {
    const THUMBHASH_SIZE: u32 = 100;
    let (width, height) = (img.width(), img.height());
    let (thumb_w, thumb_h) = if width > height {
        (THUMBHASH_SIZE, (THUMBHASH_SIZE * height / width).max(1))
    } else {
        ((THUMBHASH_SIZE * width / height).max(1), THUMBHASH_SIZE)
    };
    let rgba = crate::shared::image::thumbnail(img, thumb_w, thumb_h).to_rgba8();
    let hash = fast_thumbhash::rgba_to_thumb_hash(rgba.width() as usize, rgba.height() as usize, rgba.as_raw());
    Some(fast_thumbhash::base91_encode(&hash))
}
