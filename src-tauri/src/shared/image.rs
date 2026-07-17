//! Unified image encoding utilities to eliminate duplicate PNG/JPEG encoding patterns.
//!
//! This module consolidates the 16+ duplicate image encoding blocks found throughout
//! the codebase (primarily in message.rs) into reusable functions.

use image::{DynamicImage, ExtendedColorType};
use image::codecs::png::{PngEncoder, CompressionType, FilterType};
use image::codecs::jpeg::JpegEncoder;
use image::ImageEncoder;
use std::io::Cursor;

/// Maximum dimension for image compression (1920px on longest side)
pub const MAX_DIMENSION: u32 = 1920;

/// Default JPEG quality for standard compression (0-100)
pub const JPEG_QUALITY_STANDARD: u8 = 85;
/// JPEG quality for full-resolution re-encodes (metadata strip without the
/// user asking to compress) — near-visually-lossless.
pub const JPEG_QUALITY_HIGH: u8 = 95;
/// JPEG quality for higher compression (smaller files)
pub const JPEG_QUALITY_COMPRESSED: u8 = 70;
/// JPEG quality for UI previews (fast encoding, small size)
/// Mobile uses lower quality (25) since screens are smaller - faster encode + smaller base64
#[cfg(target_os = "android")]
pub const JPEG_QUALITY_PREVIEW: u8 = 25;
#[cfg(not(target_os = "android"))]
pub const JPEG_QUALITY_PREVIEW: u8 = 50;

/// Result of image encoding with format metadata
pub struct EncodedImage {
    /// The encoded image bytes
    pub bytes: Vec<u8>,
    /// File extension (e.g., "png" or "jpg")
    pub extension: &'static str,
}

impl EncodedImage {
    /// Convert to a base64 data URI (e.g., "data:image/png;base64,...")
    ///
    /// Pre-allocates exact capacity and encodes directly into the result string,
    /// avoiding an intermediate base64 string allocation.
    #[inline]
    pub fn to_data_uri(&self) -> String {
        let mime = if self.extension == "png" { "image/png" } else { "image/jpeg" };
        crate::util::data_uri(mime, &self.bytes)
    }
}

/// Minimum dimension threshold for JPEG encoding.
/// Images smaller than this (in both width AND height) use PNG to avoid artifacts.
/// This preserves quality for pixel art and small icons.
pub const SMALL_IMAGE_THRESHOLD: u32 = 200;

/// Encode RGBA pixel data as PNG with best compression.
///
/// Uses adaptive filtering and best compression for smallest file sizes
/// while preserving alpha transparency.
///
/// # Arguments
/// * `pixels` - RGBA pixel data (4 bytes per pixel)
/// * `width` - Image width in pixels
/// * `height` - Image height in pixels
///
/// # Returns
/// Encoded PNG bytes or an error string
pub fn encode_png(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    // Pre-allocate: PNG with best compression is typically 20-40% of raw RGBA size
    let estimated_size = pixels.len() / 3;
    let mut png_data = Vec::with_capacity(estimated_size);
    let encoder = PngEncoder::new_with_quality(
        &mut png_data,
        CompressionType::Best,
        FilterType::Adaptive,
    );
    encoder.write_image(
        pixels,
        width,
        height,
        ExtendedColorType::Rgba8
    ).map_err(|e| format!("Failed to encode PNG: {}", e))?;
    Ok(png_data)
}

/// Convert RGBA pixel data to RGB by dropping the alpha channel.
///
/// Convert RGBA to RGB, stripping the alpha channel.
/// Uses SIMD acceleration on ARM64 (NEON vld4/vst3).
#[inline]
fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    crate::simd::image::rgba_to_rgb(rgba)
}

/// Encode RGB pixel data as JPEG with specified quality.
///
/// # Arguments
/// * `pixels` - RGB pixel data (3 bytes per pixel)
/// * `width` - Image width in pixels
/// * `height` - Image height in pixels
/// * `quality` - JPEG quality (0-100), use JPEG_QUALITY_* constants
///
/// # Returns
/// Encoded JPEG bytes or an error string
pub fn encode_jpeg(pixels: &[u8], width: u32, height: u32, quality: u8) -> Result<Vec<u8>, String> {
    // Pre-allocate: JPEG is typically 5-15% of raw RGB size depending on quality
    // Use ~10% as a reasonable estimate
    let estimated_size = pixels.len() / 10;
    let mut jpeg_data = Vec::with_capacity(estimated_size.max(1024));
    let mut cursor = Cursor::new(&mut jpeg_data);
    let encoder = JpegEncoder::new_with_quality(&mut cursor, quality);
    encoder.write_image(
        pixels,
        width,
        height,
        ExtendedColorType::Rgb8
    ).map_err(|e| format!("Failed to encode JPEG: {}", e))?;
    Ok(jpeg_data)
}

/// Encode a DynamicImage choosing PNG or JPEG based on alpha transparency.
///
/// If the image has transparency (alpha < 255), PNG is used to preserve it.
/// Otherwise, JPEG is used for better compression.
///
/// # Arguments
/// * `img` - The image to encode
/// * `jpeg_quality` - Quality for JPEG encoding if used (0-100)
///
/// # Returns
/// EncodedImage with bytes and format extension, or an error string
pub fn encode_image_auto(img: &DynamicImage, jpeg_quality: u8) -> Result<EncodedImage, String> {
    let width = img.width();
    let height = img.height();

    // Fast path: if source format has no alpha channel, go straight to JPEG
    // This avoids allocating an RGBA buffer just to check for alpha
    match img {
        DynamicImage::ImageRgb8(_) |
        DynamicImage::ImageRgb16(_) |
        DynamicImage::ImageRgb32F(_) |
        DynamicImage::ImageLuma8(_) |
        DynamicImage::ImageLuma16(_) => {
            // No alpha channel possible - encode as JPEG directly
            let rgb = img.to_rgb8();
            let bytes = encode_jpeg(rgb.as_raw(), width, height, jpeg_quality)?;
            return Ok(EncodedImage {
                bytes,
                extension: "jpg",
            });
        }
        _ => {}
    }

    // Source has alpha channel - need to check if it's actually used
    let rgba = img.to_rgba8();
    let pixels = rgba.as_raw();

    if crate::util::has_alpha_transparency(pixels) {
        let bytes = encode_png(pixels, width, height)?;
        Ok(EncodedImage {
            bytes,
            extension: "png",
        })
    } else {
        // Has alpha channel but not used - convert to RGB for JPEG
        let rgb_data = rgba_to_rgb(pixels);
        let bytes = encode_jpeg(&rgb_data, width, height, jpeg_quality)?;
        Ok(EncodedImage {
            bytes,
            extension: "jpg",
        })
    }
}

/// Compress an image by resizing it to fit within max dimensions.
///
/// Maintains aspect ratio while ensuring neither dimension exceeds max_dimension.
/// Then encodes using PNG (with alpha) or JPEG (without alpha).
///
/// # Arguments
/// * `img` - The image to compress
/// * `max_dimension` - Maximum width or height in pixels
/// * `jpeg_quality` - Quality for JPEG encoding if used (0-100)
///
/// # Returns
/// EncodedImage with compressed bytes and format extension, or an error string
pub fn compress_image(img: &DynamicImage, max_dimension: u32, jpeg_quality: u8) -> Result<EncodedImage, String> {
    // Resize if needed, maintaining aspect ratio
    if img.width() > max_dimension || img.height() > max_dimension {
        let resized = img.resize(max_dimension, max_dimension, image::imageops::FilterType::Lanczos3);
        encode_image_auto(&resized, jpeg_quality)
    } else {
        // No resize needed - encode directly without cloning
        encode_image_auto(img, jpeg_quality)
    }
}

/// Downscale `img` to fit within `max_dim` using `filter`, borrowing it
/// unchanged when it's already small enough (never upscales, never clones needlessly).
fn fit_within<'a>(
    img: &'a DynamicImage,
    max_dim: u32,
    filter: ::image::imageops::FilterType,
) -> std::borrow::Cow<'a, DynamicImage> {
    if img.width() > max_dim || img.height() > max_dim {
        std::borrow::Cow::Owned(img.resize(max_dim, max_dim, filter))
    } else {
        std::borrow::Cow::Borrowed(img)
    }
}

/// Escalate compression until `img` fits within both `max_dimension` and
/// `byte_budget`, returning the smallest encoding that fits. Opaque images step
/// JPEG quality down first (re-encoding the *same* resized pixels, no resample),
/// then shrink the canvas; images with alpha (PNG output, where quality is a
/// no-op) shrink the canvas only. Errors if even the most aggressive step can't
/// get under budget (a pathological input).
pub fn compress_image_within_budget(
    img: &DynamicImage,
    max_dimension: u32,
    byte_budget: usize,
    filter: ::image::imageops::FilterType,
) -> Result<EncodedImage, String> {
    // Resample ONCE. The quality ladder below re-encodes these pixels rather
    // than resizing again per attempt (resampling is the dominant cost).
    let base = fit_within(img, max_dimension, filter);
    let first = encode_image_auto(&base, JPEG_QUALITY_STANDARD)?;
    if first.bytes.len() <= byte_budget {
        return Ok(first);
    }

    // JPEG output shrinks with quality alone (no resample); PNG output (alpha)
    // ignores quality, so for it only a smaller canvas helps.
    if first.extension == "jpg" {
        for quality in [JPEG_QUALITY_COMPRESSED, 55] {
            let enc = encode_image_auto(&base, quality)?;
            if enc.bytes.len() <= byte_budget {
                return Ok(enc);
            }
        }
    }

    // Still over budget: shrink the canvas. Resample from the ORIGINAL (not the
    // already-shrunk base) so each smaller size keeps maximum detail.
    let mut best = first;
    for dim in [max_dimension * 3 / 4, max_dimension / 2] {
        let smaller = fit_within(img, dim.max(1), filter);
        let quality = if best.extension == "png" { JPEG_QUALITY_STANDARD } else { 60 };
        let enc = encode_image_auto(&smaller, quality)?;
        if enc.bytes.len() <= byte_budget {
            return Ok(enc);
        }
        if enc.bytes.len() < best.bytes.len() {
            best = enc;
        }
    }

    Err(format!(
        "Image is too detailed to fit under {} KB even after compression (smallest was {} KB); please pick a simpler or smaller image",
        byte_budget / 1024,
        best.bytes.len() / 1024,
    ))
}

/// The non-message image being uploaded — selects the resize + byte budgets.
/// Every kind strips metadata by re-encoding (privacy by default); message
/// attachments are NOT here (they honour the per-send keep-metadata choice).
#[derive(Clone, Copy, Debug)]
pub enum UploadImageKind {
    /// Profile or community icon — rendered small.
    Avatar,
    /// Profile or community banner — a wide hero image.
    Banner,
    /// Custom emoji or emoji-pack icon — rendered tiny.
    Emoji,
}

impl UploadImageKind {
    /// `(max_dimension_px, static_byte_budget, animated_byte_budget)`.
    const fn budgets(self) -> (u32, usize, usize) {
        match self {
            UploadImageKind::Avatar => (512, 256 * 1024, 2 * 1024 * 1024),
            UploadImageKind::Banner => (1500, 600 * 1024, 3 * 1024 * 1024),
            UploadImageKind::Emoji => (256, 256 * 1024, 256 * 1024),
        }
    }

    /// Downscale filter. Avatars/banners are shown sharp, so they get CatmullRom
    /// (bicubic, ~2x faster than Lanczos3 with near-identical quality). Emoji are
    /// tiny, so Triangle (bilinear, fastest) is imperceptible and best for slow devices.
    const fn resample_filter(self) -> ::image::imageops::FilterType {
        match self {
            UploadImageKind::Avatar | UploadImageKind::Banner => ::image::imageops::FilterType::CatmullRom,
            UploadImageKind::Emoji => ::image::imageops::FilterType::Triangle,
        }
    }
}

/// Prepare a non-message image for upload: strip metadata (re-encode, keeping
/// only orientation), resize to fit, and cap the byte size, per `kind`.
///
/// Animated images (GIF / animated WebP / APNG) pass through untouched to keep
/// their animation — they can't be re-encoded without flattening to a still, and
/// in practice never carry camera EXIF — but are still size-capped. Everything
/// else is decoded and re-encoded, which drops every metadata segment.
pub fn prepare_upload_image(bytes: &[u8], kind: UploadImageKind) -> Result<EncodedImage, String> {
    let (max_dimension, byte_budget, animated_budget) = kind.budgets();

    if let Some(extension) = animated_format(bytes) {
        if bytes.len() > animated_budget {
            return Err(format!(
                "Animated image is too large ({} KB, max {} KB); please use a smaller one or a static image",
                bytes.len() / 1024,
                animated_budget / 1024,
            ));
        }
        return Ok(EncodedImage { bytes: bytes.to_vec(), extension });
    }

    // decode_image_bounded rejects decode-bombs and bakes EXIF orientation into
    // pixels; the re-encode then drops all remaining metadata.
    let img = vector_core::crypto::decode_image_bounded(bytes)
        .map_err(|_| "Image couldn't be read (unsupported or corrupt file)".to_string())?;
    compress_image_within_budget(&img, max_dimension, byte_budget, kind.resample_filter())
}

/// If `bytes` is an animated image we must not re-encode, return its extension
/// (`"gif"`/`"webp"`/`"png"`); otherwise `None`. Biased toward detecting
/// animation: a false positive only skips stripping/compression, whereas a false
/// negative would flatten the animation to a still.
fn animated_format(bytes: &[u8]) -> Option<&'static str> {
    // GIF: any GIF may hold multiple frames.
    if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        return Some("gif");
    }
    // Animated WebP: RIFF....WEBP with a VP8X chunk whose animation flag (0x02)
    // is set, or an explicit ANIM chunk near the header.
    if bytes.len() >= 16 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        let vp8x_anim = bytes.len() >= 21 && &bytes[12..16] == b"VP8X" && bytes[20] & 0x02 != 0;
        let anim_chunk = bytes[..bytes.len().min(64)].windows(4).any(|w| w == b"ANIM");
        if vp8x_anim || anim_chunk {
            return Some("webp");
        }
    }
    // APNG: a PNG with an acTL chunk before the first IDAT.
    if bytes.len() >= 8 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" && png_has_actl(bytes) {
        return Some("png");
    }
    None
}

/// Walk PNG chunks looking for `acTL` (animation control) before the first
/// `IDAT` — the marker that distinguishes an APNG from a plain PNG.
fn png_has_actl(bytes: &[u8]) -> bool {
    let mut off = 8; // past the 8-byte PNG signature
    while off + 8 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]) as usize;
        match &bytes[off + 4..off + 8] {
            b"acTL" => return true,
            b"IDAT" => return false,
            _ => {}
        }
        off = off.saturating_add(12).saturating_add(len); // len(4) + type(4) + data + crc(4)
    }
    false
}

/// Mime type for an upload-prepared image extension.
pub fn upload_mime_for(extension: &str) -> &'static str {
    match extension {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/jpeg",
    }
}

/// Encode RGBA image data from raw components, choosing format based on alpha and size.
///
/// Uses PNG for:
/// - Images with alpha transparency
/// - Small images (both dimensions < 200px) to preserve pixel art quality
///
/// Uses JPEG for everything else (better compression for photos).
///
/// # Arguments
/// * `pixels` - RGBA pixel data (4 bytes per pixel)
/// * `width` - Image width in pixels
/// * `height` - Image height in pixels
/// * `jpeg_quality` - Quality for JPEG encoding if used (0-100)
///
/// # Returns
/// EncodedImage with bytes and format extension, or an error string
pub fn encode_rgba_auto(pixels: &[u8], width: u32, height: u32, jpeg_quality: u8) -> Result<EncodedImage, String> {
    let has_alpha = crate::util::has_alpha_transparency(pixels);
    let is_small = width < SMALL_IMAGE_THRESHOLD && height < SMALL_IMAGE_THRESHOLD;

    // Use PNG for alpha transparency OR small images (preserves pixel art)
    if has_alpha || is_small {
        let bytes = encode_png(pixels, width, height)?;
        Ok(EncodedImage {
            bytes,
            extension: "png",
        })
    } else {
        // Convert RGBA to RGB inline (avoids full buffer clone)
        let rgb_data = rgba_to_rgb(pixels);
        let bytes = encode_jpeg(&rgb_data, width, height, jpeg_quality)?;
        Ok(EncodedImage {
            bytes,
            extension: "jpg",
        })
    }
}

/// Map a file extension to the `little_exif` reader for that container. Covers
/// the EXIF-bearing formats Android/desktop actually send (JPEG, TIFF, WebP,
/// PNG); GIF/ICO and unknowns return None (no EXIF to read).
fn little_exif_filetype(extension: &str) -> Option<little_exif::filetype::FileExtension> {
    use little_exif::filetype::FileExtension;
    match extension.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some(FileExtension::JPEG),
        "tiff" | "tif" => Some(FileExtension::TIFF),
        "webp" => Some(FileExtension::WEBP),
        "png" => Some(FileExtension::PNG { as_zTXt_chunk: false }),
        _ => None,
    }
}

/// Scan a JPEG's marker segments for non-EXIF metadata, returning
/// `(has_metadata, has_unclearable)`:
/// - `has_metadata`: any XMP (APP1 without the `Exif\0\0` header), IPTC/Photoshop
///   (APP13), Ducky (APP12), comment (COM), or other non-standard APPn is present.
/// - `has_unclearable`: at least one of those can't be removed losslessly by
///   little_exif (XMP, COM, and misc APP3-11/15) — so a strip must re-encode.
///   APP12/APP13 are clearable in place, so they count as metadata but not as
///   unclearable. JFIF (APP0), ICC (APP2), and Adobe (APP14) are benign structure
///   and ignored entirely.
fn jpeg_metadata_scan(bytes: &[u8]) -> (bool, bool) {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return (false, false); // not a JPEG
    }
    let (mut has_metadata, mut has_unclearable) = (false, false);
    let mut i = 2;
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xFF {
            break; // left the marker section
        }
        let marker = bytes[i + 1];
        // Standalone markers (RSTn/SOI/EOI/TEM) carry no length field.
        if marker == 0x01 || (0xD0..=0xD9).contains(&marker) {
            i += 2;
            continue;
        }
        if marker == 0xDA {
            break; // Start of Scan — compressed pixel data follows.
        }
        let len = ((bytes[i + 2] as usize) << 8) | (bytes[i + 3] as usize);
        if len < 2 || i + 2 + len > bytes.len() {
            break; // malformed
        }
        let payload = &bytes[i + 4..i + 2 + len];
        match marker {
            0xE1 => if !payload.starts_with(b"Exif\0\0") { has_metadata = true; has_unclearable = true; }, // XMP
            0xEC | 0xED => has_metadata = true, // APP12 / APP13 (IPTC): clearable in place
            0xFE => { has_metadata = true; has_unclearable = true; } // COM
            0xE0 | 0xE2 | 0xEE => {} // JFIF / ICC / Adobe: benign structure
            0xE3..=0xEF => { has_metadata = true; has_unclearable = true; } // other APPn
            _ => {} // DQT/DHT/SOF/... structural
        }
        i += 2 + len;
    }
    (has_metadata, has_unclearable)
}

/// Whether an image carries strip-worthy metadata — EXIF tags beyond Orientation
/// (which we bake into pixels regardless), or JPEG XMP/IPTC/comment/APPn segments.
/// Screenshots, memes, and our own re-encoded sends have none, so the "Keep
/// Metadata" affordance can be hidden for them.
pub fn image_bytes_have_metadata(bytes: &[u8], extension: &str) -> bool {
    use little_exif::metadata::Metadata;
    use little_exif::exif_tag::ExifTag;

    // little_exif only reads EXIF; JPEG can also carry GPS in XMP/IPTC/APPn.
    if matches!(extension.to_ascii_lowercase().as_str(), "jpg" | "jpeg")
        && jpeg_metadata_scan(bytes).0
    {
        return true;
    }

    let Some(filetype) = little_exif_filetype(extension) else { return false; };
    match Metadata::new_from_vec(&bytes.to_vec(), filetype) {
        Ok(md) => (&md).into_iter().any(|tag| !matches!(tag, ExifTag::Orientation(_))),
        Err(_) => false,
    }
}

/// Losslessly strip a JPEG's EXIF while keeping its Orientation tag.
///
/// The orientation tag reveals nothing (just which way is up), so keeping it lets
/// us drop the privacy-relevant tags (GPS, camera, timestamps) without re-encoding
/// the pixels — no quality loss and no file growth. The receiver's `<img>` still
/// renders upright from the surviving tag.
///
/// Restricted to JPEG: clears the EXIF (APP1), IPTC (APP13), and Ducky (APP12)
/// segments in place — which covers iPhone/Android camera output and their
/// Photoshop-IRB screenshots — then re-attaches only the orientation. Returns
/// `None` (caller falls back to a re-encode that rebuilds from pixels, dropping
/// every metadata segment) for non-JPEG containers or JPEGs carrying metadata
/// little_exif can't remove in place (XMP, comments, misc APPn), keeping the
/// privacy guarantee intact.
pub fn strip_metadata_keep_orientation(bytes: &[u8], extension: &str) -> Option<Vec<u8>> {
    use little_exif::metadata::Metadata;
    use little_exif::exif_tag::ExifTag;
    use little_exif::filetype::FileExtension;

    if !matches!(extension.to_ascii_lowercase().as_str(), "jpg" | "jpeg") {
        return None;
    }
    if jpeg_metadata_scan(bytes).1 {
        return None; // unclearable metadata present — re-encode drops everything
    }

    let orientation: Option<u16> = Metadata::new_from_vec(&bytes.to_vec(), FileExtension::JPEG)
        .ok()
        .and_then(|md| (&md).into_iter().find_map(|t| match t {
            ExifTag::Orientation(v) => v.first().copied(),
            _ => None,
        }));

    let mut out = bytes.to_vec();
    // Any failure here means we can't guarantee a clean strip — bail to re-encode.
    Metadata::clear_metadata(&mut out, FileExtension::JPEG).ok()?;       // EXIF (APP1)
    Metadata::clear_app13_segment(&mut out, FileExtension::JPEG).ok()?;  // IPTC / Photoshop IRB
    Metadata::clear_app12_segment(&mut out, FileExtension::JPEG).ok()?;  // Ducky

    if matches!(orientation, Some(o) if o != 1) {
        let mut md = Metadata::new();
        md.set_tag(ExifTag::Orientation(vec![orientation.unwrap()]));
        md.write_to_vec(&mut out, FileExtension::JPEG).ok()?;
    }
    Some(out)
}

/// Re-attach the original photo's EXIF metadata (GPS, camera, timestamps) onto
/// freshly re-encoded JPEG bytes, with Orientation forced to 1.
///
/// Used when the user opts to keep metadata on a *compressed* send: compression
/// re-encodes to a clean JPEG, so without this the GPS/camera/date tags are
/// lost. The orientation tag is normalised because the pixels were already
/// rotated upright during decode; carrying the original rotate value would make
/// the receiver's `<img>` rotate an already-upright image.
///
/// The original is read as `original_extension`'s container (JPEG/TIFF/WebP/PNG),
/// so an Android TIFF or WebP photo keeps its GPS/camera tags through a compress.
///
/// Best-effort: returns `compressed_jpeg` unchanged if the original carries no
/// readable EXIF or the write fails. Only meaningful for JPEG output — callers
/// should gate on the encoded extension.
pub fn reattach_exif_jpeg(compressed_jpeg: Vec<u8>, original: &[u8], original_extension: &str) -> Vec<u8> {
    use little_exif::metadata::Metadata;
    use little_exif::exif_tag::ExifTag;
    use little_exif::filetype::FileExtension;

    let Some(src_filetype) = little_exif_filetype(original_extension) else { return compressed_jpeg; };
    let original_vec = original.to_vec();
    let mut md = match Metadata::new_from_vec(&original_vec, src_filetype) {
        Ok(m) => m,
        Err(_) => return compressed_jpeg,
    };
    md.set_tag(ExifTag::Orientation(vec![1u16]));

    // Write into a clone so a mid-write failure can't hand back a corrupt image.
    let mut out = compressed_jpeg.clone();
    match md.write_to_vec(&mut out, FileExtension::JPEG) {
        Ok(()) => out,
        Err(_) => compressed_jpeg,
    }
}

/// Calculate target dimensions to fit within max_dimension while preserving aspect ratio.
///
/// Returns the original dimensions if both are already within the limit.
/// Otherwise, scales down proportionally so the longest side equals max_dimension.
///
/// # Arguments
/// * `width` - Original image width
/// * `height` - Original image height
/// * `max_dimension` - Maximum allowed size for either dimension
///
/// # Returns
/// Tuple of (new_width, new_height)
#[inline]
pub fn calculate_resize_dimensions(width: u32, height: u32, max_dimension: u32) -> (u32, u32) {
    if width <= max_dimension && height <= max_dimension {
        (width, height)
    } else if width > height {
        let ratio = max_dimension as f32 / width as f32;
        (max_dimension, (height as f32 * ratio) as u32)
    } else {
        let ratio = max_dimension as f32 / height as f32;
        ((width as f32 * ratio) as u32, max_dimension)
    }
}

/// Calculate preview dimensions based on a quality percentage.
///
/// # Arguments
/// * `width` - Original image width
/// * `height` - Original image height
/// * `quality` - Percentage (1-100) of original size
///
/// # Returns
/// Tuple of (new_width, new_height), both at least 1
#[inline]
pub fn calculate_preview_dimensions(width: u32, height: u32, quality: u32) -> (u32, u32) {
    let quality = quality.clamp(1, 100);
    (
        ((width * quality) / 100).max(1),
        ((height * quality) / 100).max(1),
    )
}

/// Maximum preview dimensions for UI display
/// Mobile: 300x400 (chat bubbles are small)
/// Desktop: 512x512 (larger display area)
#[cfg(target_os = "android")]
pub const PREVIEW_MAX_WIDTH: u32 = 300;
#[cfg(target_os = "android")]
pub const PREVIEW_MAX_HEIGHT: u32 = 400;

#[cfg(not(target_os = "android"))]
pub const PREVIEW_MAX_WIDTH: u32 = 800;
#[cfg(not(target_os = "android"))]
pub const PREVIEW_MAX_HEIGHT: u32 = 800;

/// Calculate preview dimensions capped to UI display size.
///
/// Only downscales, never upscales:
/// - Large photos are scaled to fit within max bounds
/// - Small photos keep original dimensions
///
/// Maintains aspect ratio.
#[inline]
pub fn calculate_capped_preview_dimensions(width: u32, height: u32) -> (u32, u32) {
    // If already smaller than max, keep original (never upscale)
    if width <= PREVIEW_MAX_WIDTH && height <= PREVIEW_MAX_HEIGHT {
        return (width, height);
    }

    // Scale down to fit within bounds while maintaining aspect ratio
    let width_ratio = PREVIEW_MAX_WIDTH as f32 / width as f32;
    let height_ratio = PREVIEW_MAX_HEIGHT as f32 / height as f32;
    // Use smaller ratio to fit within both bounds, cap at 1.0 to never upscale
    let ratio = width_ratio.min(height_ratio).min(1.0);

    (
        ((width as f32 * ratio) as u32).max(1),
        ((height as f32 * ratio) as u32).max(1),
    )
}

/// Read a file into memory with a 0-byte corruption check.
///
/// # Arguments
/// * `path` - Path to the file
///
/// # Returns
/// Memory-mapped file bytes or an error string
pub fn read_file_checked(path: &str) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|e| format!("Failed to read file metadata: {}", e))?;

    if metadata.len() == 0 {
        return Err(format!("File is empty (0 bytes): {}", path));
    }

    let bytes = std::fs::read(path)
        .map_err(|e| format!("Failed to read file: {}", e))?;
    Ok(bytes)
}

#[cfg(test)]
mod metadata_scan_tests {
    use super::jpeg_metadata_scan;

    // A JPEG marker segment: FF <marker> <len:u16 including these 2 bytes> <payload>.
    fn seg(marker: u8, payload: &[u8]) -> Vec<u8> {
        let len = (payload.len() + 2) as u16;
        let mut v = vec![0xFF, marker, (len >> 8) as u8, (len & 0xFF) as u8];
        v.extend_from_slice(payload);
        v
    }
    fn jpeg(segments: &[Vec<u8>]) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8]; // SOI
        for s in segments { v.extend_from_slice(s); }
        v.extend_from_slice(&[0xFF, 0xDA]); // SOS (stop)
        v
    }

    #[test]
    fn exif_and_iptc_are_clearable_not_unclearable() {
        // EXIF (APP1 "Exif") + IPTC (APP13) — like an iOS screenshot.
        let j = jpeg(&[seg(0xE1, b"Exif\0\0MM"), seg(0xED, b"Photoshop")]);
        assert_eq!(jpeg_metadata_scan(&j), (true, false));
    }

    #[test]
    fn xmp_and_comment_are_unclearable() {
        let xmp = jpeg(&[seg(0xE1, b"http://ns.adobe.com/xap/1.0/\0")]);
        assert_eq!(jpeg_metadata_scan(&xmp), (true, true));
        let com = jpeg(&[seg(0xFE, b"a private note")]);
        assert_eq!(jpeg_metadata_scan(&com), (true, true));
    }

    #[test]
    fn benign_structure_is_ignored() {
        // JFIF (APP0) + ICC (APP2) + Adobe (APP14) carry no privacy data.
        let j = jpeg(&[seg(0xE0, b"JFIF\0"), seg(0xE2, b"ICC_PROFILE\0"), seg(0xEE, b"Adobe")]);
        assert_eq!(jpeg_metadata_scan(&j), (false, false));
    }

    #[test]
    fn non_jpeg_scans_clean() {
        assert_eq!(jpeg_metadata_scan(b"\x89PNG\r\n\x1a\n...."), (false, false));
    }
}

#[cfg(test)]
mod budget_compression_tests {
    use super::{
        animated_format, compress_image_within_budget, prepare_upload_image, upload_mime_for,
        UploadImageKind,
    };

    // Structured (not random) pixels: high-frequency enough to be a real encode,
    // but with DCT structure JPEG can actually compress.
    fn structured(w: u32, h: u32) -> image::DynamicImage {
        image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([
                ((x.wrapping_mul(37) ^ y.wrapping_mul(101)) & 0xFF) as u8,
                ((x.wrapping_mul(59) ^ y.wrapping_mul(17)) & 0xFF) as u8,
                ((x.wrapping_mul(83) ^ y.wrapping_mul(7)) & 0xFF) as u8,
            ])
        }))
    }

    // Minimal container headers just large enough for the sniffers.
    fn animated_webp() -> Vec<u8> {
        // RIFF <size> WEBP VP8X <chunklen> <flags with anim bit> ...
        let mut v = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        v.extend_from_slice(&[10, 0, 0, 0]); // VP8X chunk length
        v.push(0x02); // flags: animation bit set
        v.extend_from_slice(&[0u8; 9]);
        v
    }
    fn apng() -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        // IHDR chunk (len 13) then an acTL chunk before any IDAT.
        v.extend_from_slice(&[0, 0, 0, 13]);
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&[0u8; 13 + 4]); // data + crc
        v.extend_from_slice(&[0, 0, 0, 8]);
        v.extend_from_slice(b"acTL");
        v.extend_from_slice(&[0u8; 8 + 4]);
        v
    }

    #[test]
    fn detects_every_animated_format_and_leaves_static_alone() {
        assert_eq!(animated_format(b"GIF89a...."), Some("gif"));
        assert_eq!(animated_format(b"GIF87a...."), Some("gif"));
        assert_eq!(animated_format(&animated_webp()), Some("webp"));
        assert_eq!(animated_format(&apng()), Some("png"));
        assert_eq!(animated_format(b"\x89PNG\r\n\x1a\n-plain-png"), None);
        assert_eq!(animated_format(b"\xFF\xD8\xFF-jpeg"), None);
    }

    #[test]
    fn downscales_a_large_image_under_budget() {
        let img = structured(2000, 2000);
        let out = compress_image_within_budget(&img, 512, 256 * 1024, ::image::imageops::FilterType::CatmullRom).expect("fits");
        assert!(out.bytes.len() <= 256 * 1024, "over budget: {}", out.bytes.len());
        assert_eq!(out.extension, "jpg"); // opaque source -> JPEG
        let dec = image::load_from_memory(&out.bytes).unwrap();
        assert!(dec.width() <= 512 && dec.height() <= 512, "not resized to fit 512");
    }

    #[test]
    fn an_impossible_budget_is_rejected_not_silently_oversized() {
        let img = structured(1000, 1000);
        assert!(compress_image_within_budget(&img, 512, 1, ::image::imageops::FilterType::Triangle).is_err());
    }

    #[test]
    fn static_image_is_stripped_and_capped_via_the_kind_api() {
        // Encode a real opaque PNG, then confirm the avatar path re-encodes it
        // under budget (a re-encode inherently drops any metadata).
        let png = super::encode_png(&structured(1200, 1200).to_rgba8(), 1200, 1200).unwrap();
        let out = prepare_upload_image(&png, UploadImageKind::Avatar).expect("prepared");
        assert!(out.bytes.len() <= 256 * 1024);
        let dec = image::load_from_memory(&out.bytes).unwrap();
        assert!(dec.width() <= 512 && dec.height() <= 512);
    }

    #[test]
    fn animated_passes_through_under_budget_and_is_rejected_over() {
        let gif = b"GIF89a-pretend-frames".to_vec();
        let ok = prepare_upload_image(&gif, UploadImageKind::Avatar).expect("passes");
        assert_eq!(ok.extension, "gif");
        assert_eq!(ok.bytes, gif); // untouched, animation preserved
        // Emoji animated budget is 256 KB; a larger fake GIF is rejected.
        let mut big = b"GIF89a".to_vec();
        big.resize(300 * 1024, 0);
        assert!(prepare_upload_image(&big, UploadImageKind::Emoji).is_err());
    }

    #[test]
    fn upload_mime_matches_extension() {
        assert_eq!(upload_mime_for("png"), "image/png");
        assert_eq!(upload_mime_for("gif"), "image/gif");
        assert_eq!(upload_mime_for("webp"), "image/webp");
        assert_eq!(upload_mime_for("jpg"), "image/jpeg");
    }
}

