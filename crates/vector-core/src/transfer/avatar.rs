//! The sender's avatar for the new device's screen: a small thumbnail that rides in the sealed hello,
//! so the new device fetches nothing before sign-in. The receiver re-encodes whatever arrives, so the
//! page only ever renders a PNG this code produced.

use std::io::Cursor;

use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, ImageReader, Limits, Rgba, RgbaImage};

/// Thumbnail edge, in pixels: crisp at the card's 44 px on a 3x screen.
const SIDE: u32 = 128;
/// The largest thumbnail a sender includes or a receiver accepts.
pub const MAX_BYTES: usize = 16 * 1024;
/// What transparent corners settle on: the identity card's surface.
const BACKDROP: Rgba<u8> = Rgba([22, 23, 24, 255]);

/// A small square JPEG of the cached avatar `bytes`, or `None` if it won't decode or shrink enough.
pub fn thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let square = img.resize_to_fill(SIDE, SIDE, FilterType::Lanczos3).to_rgba8();
    let mut flat = RgbaImage::from_pixel(SIDE, SIDE, BACKDROP);
    image::imageops::overlay(&mut flat, &square, 0, 0);
    let rgb = DynamicImage::ImageRgba8(flat).to_rgb8();
    for quality in [88u8, 78, 65] {
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode_image(&rgb).ok()?;
        if out.len() <= MAX_BYTES {
            return Some(out);
        }
    }
    None
}

/// A PNG data URL re-encoded from untrusted thumbnail `bytes`: raster formats only, small
/// dimensions only, never handed to the page as received.
pub fn sanitize(bytes: &[u8]) -> Option<String> {
    if bytes.len() > MAX_BYTES {
        return None;
    }
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    if !matches!(reader.format(), Some(ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP)) {
        return None;
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(512);
    limits.max_image_height = Some(512);
    limits.max_alloc = Some(4 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let side = SIDE.min(img.width()).min(img.height()).max(1);
    let square = img.resize_to_fill(side, side, FilterType::Lanczos3);
    let mut png = Vec::new();
    square.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).ok()?;
    Some(format!("data:image/png;base64,{}", base64_simd::STANDARD.encode_to_string(&png)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, alpha: u8) -> Vec<u8> {
        let img = RgbaImage::from_fn(width, height, |x, y| Rgba([(x % 256) as u8, (y % 256) as u8, 200, alpha]));
        let mut out = Vec::new();
        DynamicImage::ImageRgba8(img).write_to(&mut Cursor::new(&mut out), ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn a_large_avatar_becomes_a_small_square_jpeg() {
        let thumb = thumbnail(&png(1200, 800, 128)).unwrap();
        assert!(thumb.len() <= MAX_BYTES);
        assert_eq!(image::guess_format(&thumb).unwrap(), ImageFormat::Jpeg);
        let back = image::load_from_memory(&thumb).unwrap();
        assert_eq!((back.width(), back.height()), (SIDE, SIDE));
        assert!(thumbnail(b"not an image").is_none());
        assert!(thumbnail(&png(5000, 10, 255)).is_none(), "a source past the decode limits");
    }

    #[test]
    fn only_a_small_raster_reaches_the_page_and_always_as_png() {
        let url = sanitize(&thumbnail(&png(300, 300, 255)).unwrap()).unwrap();
        let body = url.strip_prefix("data:image/png;base64,").expect("png data url");
        let bytes = base64_simd::STANDARD.decode_to_vec(body).unwrap();
        assert_eq!(image::guess_format(&bytes).unwrap(), ImageFormat::Png);

        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
        assert!(sanitize(svg).is_none(), "never svg");
        assert!(sanitize(b"<html>").is_none());
        assert!(sanitize(&png(2000, 40, 255)).is_none(), "dimensions past the limit");
        assert!(sanitize(&vec![0u8; MAX_BYTES + 1]).is_none(), "size past the limit");
    }
}
