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
        // Within budget and small enough: pass through untouched, animation and
        // all. Otherwise re-encode downscaled — the animation survives (frames
        // stream through the GIF encoder), only the pixels shrink.
        let within_dims = animated_dims(bytes)
            .is_some_and(|(w, h)| w <= max_dimension && h <= max_dimension);
        if bytes.len() <= animated_budget && within_dims {
            return Ok(EncodedImage { bytes: bytes.to_vec(), extension });
        }
        if let Ok(re) = transcode_animated_budgeted(bytes, max_dimension, animated_budget) {
            if re.bytes.len() <= animated_budget {
                return Ok(re);
            }
        }
        return Err(format!(
            "Animated image is too large ({} KB, max {} KB); please use a smaller one or a static image",
            bytes.len() / 1024,
            animated_budget / 1024,
        ));
    }

    // decode_image_bounded rejects decode-bombs and bakes EXIF orientation into
    // pixels; the re-encode then drops all remaining metadata.
    let img = vector_core::crypto::decode_image_bounded(bytes)
        .map_err(|_| "Image couldn't be read (unsupported or corrupt file)".to_string())?;
    compress_image_within_budget(&img, max_dimension, byte_budget, kind.resample_filter())
}

/// Ceiling for per-frame streaming decode: a frame is `w*h*4` bytes resident,
/// so 2048² (16 MB) is the most a hostile animation may cost at once. Anything
/// larger flattens to a still instead of being trusted.
pub const ANIMATED_MAX_SOURCE_DIM: u32 = 2048;
/// Frames kept when re-encoding an animation; the tail is dropped. Generous —
/// a 300-frame avatar loop is already ~30s at typical delays.
pub const ANIMATED_MAX_FRAMES: usize = 300;

/// Re-encode an animation downscaled to fit `max_dim`, preserving frame delays
/// and infinite loop. GIF, animated WebP and APNG all decode; the output is
/// always GIF (the one animated format every WebView renders cheaply and the
/// `image` crate can write). Frames stream one at a time, so peak memory is a
/// single frame regardless of input size.
///
/// This is the whole defence against animation-bombs shredding the webview:
/// WebKit decodes the FULL logical screen of a GIF for every <img> showing it,
/// so an 800px avatar rendered at 48px still costs 800px per instance, forever.
pub fn transcode_animated(bytes: &[u8], max_dim: u32, max_frames: usize) -> Result<EncodedImage, String> {
    transcode_animated_opts(bytes, max_dim, max_frames, DELTA_TOLERANCE, 1)
}

/// Escalate through quality rungs until the animation fits `byte_budget`:
/// full quality first, then smaller + lossier, finally half the frames too.
/// Returns the first rung that fits, or the smallest attempt — video-like
/// content (every pixel new each frame) can defeat GIF entirely, and a best
/// effort still beats shipping the original to the webview.
pub fn transcode_animated_budgeted(bytes: &[u8], max_dim: u32, byte_budget: usize) -> Result<EncodedImage, String> {
    let rungs: [(u32, i16, usize); 3] = [
        (max_dim, DELTA_TOLERANCE, 1),
        (max_dim * 7 / 10, 18, 1),
        (max_dim / 2, 18, 2),
    ];
    // One-shot rung selection: the source's own bytes-per-pixel-per-frame
    // predicts a rung's output within tens of percent (measured), so hopeless
    // rungs are skipped WITHOUT paying their full encode pass — a 260-frame
    // video banner goes straight to the rung that can fit, one pass, not
    // three. The estimate needs a frame count, which a GIF yields from a
    // header-only scan; when it can't be had, every rung runs as before.
    let est_src = animated_dims(bytes).zip(gif_frame_count(bytes)).map(|((w, h), frames)| {
        (bytes.len() as f64 / (frames.max(1) as f64 * f64::from(w * h)), w, h, frames)
    });
    let mut best: Option<EncodedImage> = None;
    let last_i = rungs.len() - 1;
    for (i, (dim, tol, step)) in rungs.into_iter().enumerate() {
        if let (Some((bpp, w, h, frames)), false) = (est_src, i == last_i) {
            let scale = (f64::from(dim) / f64::from(w.max(h))).min(1.0);
            let px = f64::from(w) * scale * f64::from(h) * scale;
            let kept = frames.min(ANIMATED_MAX_FRAMES).div_ceil(step);
            let est = bpp * kept as f64 * px;
            // 1.5x slack: the estimate ignores the tolerance's help, so it
            // overshoots on delta-friendly content — skip only the hopeless.
            if est > byte_budget as f64 * 1.5 {
                continue;
            }
        }
        let re = transcode_animated_opts(bytes, dim, ANIMATED_MAX_FRAMES, tol, step)?;
        if re.bytes.len() <= byte_budget {
            return Ok(re);
        }
        if best.as_ref().is_none_or(|b| re.bytes.len() < b.bytes.len()) {
            best = Some(re);
        }
    }
    best.ok_or_else(|| "no rung produced output".into())
}

/// Frame count from a GIF's block structure alone — image data is skipped,
/// never LZW-decoded, so this is milliseconds even on a multi-megabyte file.
fn gif_frame_count(bytes: &[u8]) -> Option<usize> {
    let mut opts = gif::DecodeOptions::new();
    opts.set_color_output(gif::ColorOutput::Indexed);
    opts.allow_unknown_blocks(true);
    let mut d = opts.read_info(Cursor::new(bytes)).ok()?;
    let mut n = 0usize;
    while let Ok(Some(_)) = d.next_frame_info() {
        n += 1;
        if n > 10_000 {
            return None;
        }
    }
    (n > 0).then_some(n)
}

fn transcode_animated_opts(
    bytes: &[u8],
    max_dim: u32,
    max_frames: usize,
    tolerance: i16,
    frame_step: usize,
) -> Result<EncodedImage, String> {
    use image::codecs::gif::GifDecoder;
    use image::AnimationDecoder;

    let cursor = Cursor::new(bytes);
    let frames: image::Frames = if bytes.starts_with(b"GIF8") {
        let dec = GifDecoder::new(cursor).map_err(|e| format!("gif decode: {e}"))?;
        check_animated_dims(image::ImageDecoder::dimensions(&dec))?;
        dec.into_frames()
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        let dec = image::codecs::webp::WebPDecoder::new(cursor).map_err(|e| format!("webp decode: {e}"))?;
        check_animated_dims(image::ImageDecoder::dimensions(&dec))?;
        dec.into_frames()
    } else if bytes.starts_with(b"\x89PNG") {
        let dec = image::codecs::png::PngDecoder::new(cursor).map_err(|e| format!("png decode: {e}"))?;
        check_animated_dims(image::ImageDecoder::dimensions(&dec))?;
        dec.apng().map_err(|e| format!("apng decode: {e}"))?.into_frames()
    } else {
        return Err("not an animated format".into());
    };

    let mut out: Vec<u8> = Vec::new();
    {
        // The raw `gif` crate, not the image facade: differencing is only
        // renderable with an explicit DisposalMethod::Keep, which the facade
        // cannot set — its default leaves reused pixels as holes in some
        // decoders (the image crate's own included).
        //
        // First frame peeled: it fixes the canvas dimensions the encoder
        // needs up front, and seeds the differencing baseline.
        let mut frames = frames;
        let first = match frames.next() {
            Some(f) => f.map_err(|e| format!("frame decode: {e}"))?,
            None => return Err("no frames decoded".into()),
        };
        let fit = |buf: image::RgbaImage| -> image::RgbaImage {
            if buf.width() > max_dim || buf.height() > max_dim {
                DynamicImage::ImageRgba8(buf)
                    .resize(max_dim, max_dim, ::image::imageops::FilterType::Triangle)
                    .into_rgba8()
            } else {
                buf
            }
        };
        let write_frame = |enc: &mut gif::Encoder<&mut Vec<u8>>,
                           mut rgba: Vec<u8>,
                           (sw, sh): (u32, u32),
                           (x0, y0): (u32, u32),
                           delay: image::Delay|
         -> Result<(), String> {
            // Speed 10 ≈ good quantization at a fraction of best-quality cost;
            // frames this small keep the whole pass in the tens of ms.
            let mut f = gif::Frame::from_rgba_speed(sw as u16, sh as u16, &mut rgba, 10);
            f.left = x0 as u16;
            f.top = y0 as u16;
            let (ms, _) = delay.numer_denom_ms();
            f.delay = (ms / 10).clamp(2, u32::from(u16::MAX)) as u16;
            f.dispose = gif::DisposalMethod::Keep;
            enc.write_frame(&f).map_err(|e| format!("gif encode: {e}"))
        };

        let first_delay = first.delay();
        let mut shown = fit(first.into_buffer());
        let canvas = shown.dimensions();
        let mut enc = gif::Encoder::new(&mut out, canvas.0 as u16, canvas.1 as u16, &[])
            .map_err(|e| format!("gif encode: {e}"))?;
        enc.set_repeat(gif::Repeat::Infinite).map_err(|e| format!("gif repeat: {e}"))?;
        write_frame(&mut enc, shown.as_raw().clone(), canvas, (0, 0), first_delay)?;

        // Inter-frame differencing: emit only pixels that changed beyond the
        // tolerance since what the viewer displays; the rest go transparent
        // and keep-disposal shows the old pixel through. A naive full-frame
        // re-encode INFLATES an already optimized GIF several-fold.
        let mut count = 1usize;
        // Frame decimation (`frame_step` > 1): skipped frames donate their
        // delay to the next kept one, so total duration — and perceived speed
        // — is unchanged, only the motion sampling coarsens.
        let mut skipped_ms: u32 = 0;
        for (i, frame) in frames.enumerate() {
            if count >= max_frames {
                break;
            }
            let frame = frame.map_err(|e| format!("frame decode: {e}"))?;
            let (ms, _) = frame.delay().numer_denom_ms();
            if frame_step > 1 && (i + 1) % frame_step != 0 {
                skipped_ms += ms;
                continue;
            }
            let delay = image::Delay::from_numer_denom_ms(ms + skipped_ms, 1);
            skipped_ms = 0;
            let resized = fit(frame.into_buffer());
            if resized.dimensions() != canvas {
                return Err("frame dimensions changed mid-animation".into());
            }
            // Keyframe flush: tolerance lets slow drift go stale, and over
            // enough frames the reused patches read as a dirty window. A full
            // frame at intervals bounds how long any residue can live.
            if count % 12 == 0 {
                shown = resized.clone();
                write_frame(&mut enc, resized.into_raw(), canvas, (0, 0), delay)?;
            } else {
                let (sub, x0, y0) = delta_frame(&mut shown, &resized, tolerance);
                let dims = sub.dimensions();
                write_frame(&mut enc, sub.into_raw(), dims, (x0, y0), delay)?;
            }
            count += 1;
        }
    }
    Ok(EncodedImage { bytes: out, extension: "gif" })
}

/// Header-only dimensions of an image, without decoding a pixel.
pub fn animated_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Per-channel tolerance for frame differencing: a pixel within this of what
/// the viewer already shows is REUSED (encoded transparent) instead of
/// re-emitted. The visible drift is bounded by this value per channel — the
/// comparison is always against the composited display, never the prior delta.
const DELTA_TOLERANCE: i16 = 8;

/// Encode the difference between what a decoder currently displays (`shown`)
/// and the next true frame: unchanged-within-tolerance pixels go transparent
/// (GIF keep-disposal shows the old pixel through), the rest are emitted and
/// written back into `shown`. Returns the cropped sub-frame and its offsets.
///
/// This is the transparency differencing every optimized GIF uses — without it
/// a re-encode of an optimized source INFLATES several-fold, because full
/// frames forfeit both the crop and the per-pixel reuse.
fn delta_frame(shown: &mut image::RgbaImage, next: &image::RgbaImage, tolerance: i16) -> (image::RgbaImage, u32, u32) {
    let (w, h) = next.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0u32, 0u32);
    let s = shown.as_raw();
    let n = next.as_raw();
    // Pass 1: bounding box of pixels that must be re-emitted.
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let differs = (0..3).any(|c| (s[i + c] as i16 - n[i + c] as i16).abs() > tolerance);
            if differs {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x0 >= x1 || y0 >= y1 {
        // Nothing changed: a 1x1 transparent frame still carries the delay.
        return (image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0])), 0, 0);
    }
    // Pass 2: build the sub-frame — reused pixels transparent, changed pixels
    // opaque and mirrored into `shown` so the next comparison sees the truth
    // the viewer sees.
    let (sw, sh) = (x1 - x0, y1 - y0);
    let mut sub = image::RgbaImage::new(sw, sh);
    let shown_raw: &mut [u8] = shown.as_mut();
    for y in 0..sh {
        for x in 0..sw {
            let i = (((y + y0) * w + (x + x0)) * 4) as usize;
            let differs = (0..3).any(|c| (shown_raw[i + c] as i16 - n[i + c] as i16).abs() > tolerance);
            if differs {
                sub.put_pixel(x, y, image::Rgba([n[i], n[i + 1], n[i + 2], 255]));
                shown_raw[i..i + 4].copy_from_slice(&n[i..i + 4]);
            }
        }
    }
    (sub, x0, y0)
}

fn check_animated_dims((w, h): (u32, u32)) -> Result<(), String> {
    if w == 0 || h == 0 || w > ANIMATED_MAX_SOURCE_DIM || h > ANIMATED_MAX_SOURCE_DIM {
        return Err(format!("animation dimensions {w}x{h} out of bounds"));
    }
    Ok(())
}

/// If `bytes` is an animated image we must not re-encode, return its extension
/// (`"gif"`/`"webp"`/`"png"`); otherwise `None`. Biased toward detecting
/// animation: a false positive only skips stripping/compression, whereas a false
/// negative would flatten the animation to a still.
pub fn animated_format(bytes: &[u8]) -> Option<&'static str> {
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


/// Whether a JPEG segment is structure a decoder needs rather than metadata: every
/// non-APPn segment, JFIF without a thumbnail, the ICC profile and the Adobe
/// transform. `head` need only be the payload's first 14 bytes.
fn jpeg_segment_is_structure(marker: u8, head: &[u8]) -> bool {
    match marker {
        // JFIF header only: the thumbnail dims (bytes 12, 13) must be zero.
        0xE0 => head.starts_with(b"JFIF\0") && head.len() >= 14 && head[12] == 0 && head[13] == 0,
        0xE2 => head.starts_with(b"ICC_PROFILE\0"),
        0xEE => head.starts_with(b"Adobe"),
        0xE1..=0xEF | 0xFE => false,
        _ => true,
    }
}

/// The PNG chunks rendering needs (image data, palette, transparency, colour space,
/// density, APNG frames); every other chunk is metadata.
const PNG_RENDER_CHUNKS: &[&[u8; 4]] = &[
    b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"iCCP",
    b"sBIT", b"cICP", b"mDCV", b"mDCv", b"cLLI", b"cLLi", b"bKGD", b"pHYs",
    b"acTL", b"fcTL", b"fdAT",
];

/// [`image_bytes_have_metadata`] for JPEG and PNG, walking segment headers and
/// seeking past everything else: a few KB are read however large the file. `None`
/// for any other format, or a file too broken to walk.
pub fn header_has_metadata<R: std::io::Read + std::io::Seek>(reader: &mut R, extension: &str) -> Option<bool> {
    match extension.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => jpeg_has_metadata(reader),
        "png" => png_has_metadata(reader),
        _ => None,
    }
}

fn read_n<R: std::io::Read>(reader: &mut R, n: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; n];
    reader.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// Whether an EXIF block says anything beyond Orientation: another IFD0 tag (the
/// camera, GPS and Exif sub-IFDs hang off it), or a second IFD (a thumbnail).
/// A block that can't be read counts as saying something.
fn exif_beyond_orientation(tiff: &[u8]) -> bool {
    let read = || -> Option<bool> {
        let le = match tiff.get(0..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let u16_at = |i: usize| tiff.get(i..i + 2).map(|b| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) });
        let u32_at = |i: usize| tiff.get(i..i + 4).map(|b| if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) });
        let ifd = u32_at(4)? as usize;
        let entries = u16_at(ifd)? as usize;
        for k in 0..entries {
            if u16_at(ifd + 2 + k * 12)? != 0x0112 {
                return Some(true);
            }
        }
        Some(u32_at(ifd + 2 + entries * 12)? != 0)
    };
    read().unwrap_or(true)
}

/// Whether the strip would drop a segment: anything but structure, where an EXIF
/// block holding only the orientation counts as structure too.
fn jpeg_has_metadata<R: std::io::Read + std::io::Seek>(r: &mut R) -> Option<bool> {
    use std::io::SeekFrom;
    if read_n(r, 2)? != [0xFF, 0xD8] {
        return None;
    }
    loop {
        let mut m = [0u8; 2];
        r.read_exact(&mut m).ok()?;
        if m[0] != 0xFF {
            return None;
        }
        match m[1] {
            0xFF => { r.seek(SeekFrom::Current(-1)).ok()?; continue; }
            0xDA | 0xD9 => return Some(false),
            0x01 | 0xD0..=0xD7 => continue,
            _ => {}
        }
        let len = u16::from_be_bytes(read_n(r, 2)?.try_into().ok()?) as usize;
        let body = len.checked_sub(2)?;
        if !(0xE0..=0xEF).contains(&m[1]) && m[1] != 0xFE {
            r.seek(SeekFrom::Current(body as i64)).ok()?;
            continue;
        }
        let head = read_n(r, body.min(14))?;
        if m[1] == 0xE1 && head.starts_with(b"Exif\0\0") {
            let mut tiff = head[6..].to_vec();
            tiff.extend(read_n(r, body - head.len())?);
            if exif_beyond_orientation(&tiff) {
                return Some(true);
            }
            continue;
        }
        if !jpeg_segment_is_structure(m[1], &head) {
            return Some(true);
        }
        r.seek(SeekFrom::Current((body - head.len()) as i64)).ok()?;
    }
}

/// Whether the strip would drop a chunk ahead of the pixels, where an `eXIf` holding
/// only the orientation counts as none.
fn png_has_metadata<R: std::io::Read + std::io::Seek>(r: &mut R) -> Option<bool> {
    use std::io::SeekFrom;
    // An EXIF block is a few KB; one claiming more is not something to read whole.
    const EXIF_READ_CAP: usize = 1 << 20;
    if read_n(r, 8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    loop {
        let head = read_n(r, 8)?;
        let len = u32::from_be_bytes(head[0..4].try_into().ok()?) as usize;
        let ty = &head[4..8];
        match ty {
            b"IDAT" | b"IEND" => return Some(false),
            b"eXIf" if len > EXIF_READ_CAP => return Some(true),
            b"eXIf" => {
                if exif_beyond_orientation(&read_n(r, len)?) {
                    return Some(true);
                }
                r.seek(SeekFrom::Current(4)).ok()?;
            }
            _ if !PNG_RENDER_CHUNKS.iter().any(|k| k.as_slice() == ty) => return Some(true),
            _ => { r.seek(SeekFrom::Current(len as i64 + 4)).ok()?; }
        }
    }
}

/// Whether an image carries strip-worthy metadata — EXIF tags beyond Orientation
/// (which we bake into pixels regardless), or JPEG XMP/IPTC/comment/APPn segments.
/// Screenshots, memes, and our own re-encoded sends have none, so the "Keep
/// Metadata" affordance can be hidden for them.
pub fn image_bytes_have_metadata(bytes: &[u8], extension: &str) -> bool {
    use little_exif::metadata::Metadata;
    use little_exif::exif_tag::ExifTag;

    if matches!(extension.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png") {
        return header_has_metadata(&mut std::io::Cursor::new(bytes), extension).unwrap_or(false);
    }
    let Some(filetype) = little_exif_filetype(extension) else { return false; };
    match Metadata::new_from_vec(&bytes.to_vec(), filetype) {
        Ok(md) => (&md).into_iter().any(|tag| !matches!(tag, ExifTag::Orientation(_))),
        Err(_) => false,
    }
}

/// Losslessly strip an image's metadata while keeping its Orientation.
///
/// The orientation tag reveals nothing (just which way is up), so keeping it lets
/// us drop the privacy-relevant tags (GPS, camera, timestamps, XMP, comments,
/// embedded thumbnails) without re-encoding the pixels: no quality loss, no file
/// growth, and none of the full decode + encode a re-encode costs.
///
/// JPEG and PNG are filtered segment by segment against an allowlist of what
/// rendering needs, so anything unrecognised goes. Returns `None` (the caller
/// re-encodes from pixels, which drops every metadata segment) for other
/// containers, malformed files, and a PNG whose orientation isn't upright.
pub fn strip_metadata_keep_orientation(bytes: &[u8], extension: &str) -> Option<Vec<u8>> {
    match extension.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => strip_jpeg_metadata(bytes),
        "png" => strip_png_metadata(bytes),
        _ => None,
    }
}

/// The EXIF orientation (1-8) a decoder would apply, read from the header alone.
/// JPEG and PNG are read directly; anything else asks the decoder.
fn header_orientation(bytes: &[u8]) -> Option<u8> {
    if let Some(h) = jpeg_header(bytes) {
        return h.orientation;
    }
    if let Some(h) = png_header(bytes) {
        return h.orientation;
    }
    use image::ImageDecoder;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    reader.limits(vector_core::crypto::bounded_image_limits());
    let mut decoder = reader.into_decoder().ok()?;
    decoder.orientation().ok().map(|o| o.to_exif())
}

/// An image's upright dimensions from its header alone: no pixels are decoded.
pub fn header_dimensions_oriented(bytes: &[u8]) -> Option<(u32, u32)> {
    let (w, h, orientation) = match (jpeg_header(bytes), png_header(bytes)) {
        (Some(j), _) => (j.width?, j.height?, j.orientation?),
        (_, Some(p)) => (p.width, p.height, p.orientation?),
        _ => {
            use image::ImageDecoder;
            let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
            reader.limits(vector_core::crypto::bounded_image_limits());
            let mut decoder = reader.into_decoder().ok()?;
            let (w, h) = decoder.dimensions();
            (w, h, decoder.orientation().ok()?.to_exif())
        }
    };
    // EXIF 5-8 turn the image a quarter, swapping its sides.
    Some(if orientation >= 5 { (h, w) } else { (w, h) })
}

/// The Orientation tag of a TIFF-structured EXIF block: 1 when absent, `None` when
/// the block can't be read (a caller then treats the orientation as unknown).
fn exif_orientation(tiff: &[u8]) -> Option<u8> {
    let le = match tiff.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |i: usize| tiff.get(i..i + 2).map(|b| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) });
    let u32_at = |i: usize| tiff.get(i..i + 4).map(|b| if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) });
    let ifd = u32_at(4)? as usize;
    let entries = u16_at(ifd)? as usize;
    for k in 0..entries {
        let entry = ifd + 2 + k * 12;
        if u16_at(entry)? == 0x0112 {
            let v = u16_at(entry + 8)?;
            return Some(if (1..=8).contains(&v) { v as u8 } else { 1 });
        }
    }
    Some(1)
}

/// What a JPEG's marker segments say before the scan: its size (from the frame
/// header) and orientation (from the first EXIF block).
struct JpegHeader {
    width: Option<u32>,
    height: Option<u32>,
    orientation: Option<u8>,
}

/// Each marker segment ahead of the first scan: `(offset, marker, total length)`,
/// or `None` for a file that isn't a JPEG or breaks off before its scan.
fn jpeg_segments(bytes: &[u8]) -> Option<(Vec<(usize, u8, usize)>, usize)> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut segments = Vec::new();
    let mut i = 2;
    loop {
        if i + 4 > bytes.len() || bytes[i] != 0xFF {
            return None;
        }
        let marker = bytes[i + 1];
        match marker {
            0xFF => { i += 1; continue; }
            0xDA => return Some((segments, i)),
            0xD9 => return None,
            0x01 | 0xD0..=0xD7 => { segments.push((i, marker, 2)); i += 2; continue; }
            _ => {}
        }
        let len = ((bytes[i + 2] as usize) << 8) | bytes[i + 3] as usize;
        if len < 2 || i + 2 + len > bytes.len() {
            return None;
        }
        segments.push((i, marker, 2 + len));
        i += 2 + len;
    }
}

fn jpeg_header(bytes: &[u8]) -> Option<JpegHeader> {
    let (segments, _) = jpeg_segments(bytes)?;
    let mut header = JpegHeader { width: None, height: None, orientation: Some(1) };
    let mut exif_seen = false;
    for (at, marker, len) in segments {
        let payload = &bytes[(at + 4).min(at + len)..at + len];
        match marker {
            // SOFn (not DHT, JPG or DAC): precision, height, width.
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) && payload.len() >= 5 => {
                header.height = Some(u16::from_be_bytes([payload[1], payload[2]]) as u32);
                header.width = Some(u16::from_be_bytes([payload[3], payload[4]]) as u32);
            }
            0xE1 if !exif_seen && payload.starts_with(b"Exif\0\0") => {
                exif_seen = true;
                header.orientation = exif_orientation(&payload[6..]);
            }
            _ => {}
        }
    }
    Some(header)
}

/// What a PNG's chunks say ahead of its pixels: its size (IHDR) and orientation (eXIf).
struct PngHeader {
    width: u32,
    height: u32,
    orientation: Option<u8>,
}

fn png_header(bytes: &[u8]) -> Option<PngHeader> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !bytes.starts_with(SIGNATURE) || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    let mut orientation = Some(1);
    let mut off = SIGNATURE.len();
    while off + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[off..off + 4].try_into().ok()?) as usize;
        let ty = &bytes[off + 4..off + 8];
        if ty == b"IDAT" || ty == b"IEND" {
            break;
        }
        if ty == b"eXIf" {
            orientation = exif_orientation(bytes.get(off + 8..off + 8 + len)?);
            break;
        }
        off = off.checked_add(12)?.checked_add(len)?;
    }
    Some(PngHeader { width, height, orientation })
}

/// A minimal EXIF APP1 segment carrying only the Orientation tag.
fn orientation_app1(orientation: u8) -> Vec<u8> {
    let mut v = vec![0xFF, 0xE1, 0x00, 0x22];
    v.extend_from_slice(b"Exif\0\0");
    v.extend_from_slice(b"MM\0\x2A\0\0\0\x08");
    // One IFD entry: tag 0x0112, SHORT, count 1, value; then no next IFD.
    v.extend_from_slice(&[0x00, 0x01, 0x01, 0x12, 0x00, 0x03, 0, 0, 0, 1, 0x00, orientation, 0, 0, 0, 0, 0, 0]);
    v
}

/// Where a JPEG's compressed data ends: just past EOI, walking marker segments by
/// their lengths and the entropy-coded runs between them, so bytes inside a table
/// can't pass for an EOI. `None` for a file that never reaches one.
fn jpeg_scan_end(bytes: &[u8], mut i: usize) -> Option<usize> {
    while i + 1 < bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        let m = bytes[i + 1];
        match m {
            0xD9 => return Some(i + 2),
            0xFF => { i += 1; continue; }
            0x01 | 0xD0..=0xD7 => i += 2,
            _ => {
                if i + 4 > bytes.len() {
                    return None;
                }
                let len = ((bytes[i + 2] as usize) << 8) | bytes[i + 3] as usize;
                if len < 2 {
                    return None;
                }
                i += 2 + len;
            }
        }
        // Entropy-coded data runs until a real marker; FF00 is a stuffed byte and
        // RSTn sits inside the scan. Only an FF can start either, so jump between them.
        loop {
            i += memchr::memchr(0xFF, bytes.get(i..)?)?;
            match bytes.get(i + 1)? {
                0x00 | 0xD0..=0xD7 => i += 2,
                _ => break,
            }
        }
    }
    None
}

/// Keep what a JPEG decoder needs (tables, frame, scans, JFIF without a thumbnail,
/// the ICC profile and the Adobe transform) and drop every other APPn and comment,
/// then everything after EOI (MPF secondary images, trailers).
fn strip_jpeg_metadata(bytes: &[u8]) -> Option<Vec<u8>> {
    let orientation = jpeg_header(bytes)?.orientation?;
    let (segments, scan) = jpeg_segments(bytes)?;
    let end = jpeg_scan_end(bytes, scan)?;
    let mut out = Vec::with_capacity(end);
    out.extend_from_slice(&[0xFF, 0xD8]);
    // The orientation segment follows a leading JFIF, which must stay first.
    let mut exif_at = 2;
    for (at, marker, len) in segments {
        let payload = &bytes[(at + 4).min(at + len)..at + len];
        if jpeg_segment_is_structure(marker, payload) {
            out.extend_from_slice(&bytes[at..at + len]);
            if marker == 0xE0 && out.len() == 2 + len {
                exif_at = out.len();
            }
        }
    }
    out.extend_from_slice(&bytes[scan..end]);
    if orientation != 1 {
        out.splice(exif_at..exif_at, orientation_app1(orientation));
    }
    Some(out)
}

/// Keep a PNG's rendering chunks (image data, palette, transparency, colour space,
/// density, APNG frames) and drop the rest: text, eXIf, timestamps and anything
/// private. Only an upright PNG: its orientation lives in the eXIf that goes.
fn strip_png_metadata(bytes: &[u8]) -> Option<Vec<u8>> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !bytes.starts_with(SIGNATURE) || header_orientation(bytes)? != 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(SIGNATURE);
    let mut off = SIGNATURE.len();
    loop {
        if off + 12 > bytes.len() {
            return None;
        }
        let len = u32::from_be_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]) as usize;
        let end = off.checked_add(12)?.checked_add(len)?;
        if end > bytes.len() {
            return None;
        }
        let ty = &bytes[off + 4..off + 8];
        if PNG_RENDER_CHUNKS.iter().any(|k| k.as_slice() == ty) {
            out.extend_from_slice(&bytes[off..end]);
        }
        off = end;
        if ty == b"IEND" {
            return Some(out);
        }
    }
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
    use super::{header_has_metadata, orientation_app1};

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
    fn scan(bytes: &[u8], ext: &str) -> Option<bool> {
        header_has_metadata(&mut std::io::Cursor::new(bytes), ext)
    }

    #[test]
    fn exif_iptc_xmp_and_comments_are_metadata() {
        let gps_ifd = b"Exif\0\0MM\0\x2A\0\0\0\x08\0\x01\x88\x25\0\x04\0\0\0\x01\0\0\0\0\0\0\0\0";
        assert_eq!(scan(&jpeg(&[seg(0xE1, gps_ifd)]), "jpg"), Some(true));
        assert_eq!(scan(&jpeg(&[seg(0xED, b"Photoshop")]), "jpg"), Some(true));
        assert_eq!(scan(&jpeg(&[seg(0xE1, b"http://ns.adobe.com/xap/1.0/\0")]), "jpg"), Some(true));
        assert_eq!(scan(&jpeg(&[seg(0xFE, b"a private note")]), "jpg"), Some(true));
        assert_eq!(scan(&jpeg(&[seg(0xE2, b"MPF\0offsets")]), "jpg"), Some(true), "the strip drops it");
    }

    #[test]
    fn an_orientation_alone_is_not_metadata() {
        assert_eq!(scan(&jpeg(&[orientation_app1(6)]), "jpeg"), Some(false));
    }

    #[test]
    fn benign_structure_is_ignored() {
        let j = jpeg(&[seg(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"), seg(0xE2, b"ICC_PROFILE\0"), seg(0xEE, b"Adobe")]);
        assert_eq!(scan(&j, "jpg"), Some(false));
    }

    #[test]
    fn other_formats_are_not_answered_here() {
        assert_eq!(scan(b"\x89PNG\r\n\x1a\n....", "jpg"), None);
        assert_eq!(scan(b"RIFF....WEBP", "webp"), None);
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
        // A REAL small animation: within dims and budget it must ride
        // byte-identical (no churn, no re-quantize).
        let mut gif = Vec::new();
        {
            use image::codecs::gif::GifEncoder;
            let mut enc = GifEncoder::new_with_speed(&mut gif, 30);
            for c in [[255u8, 0, 0, 255], [0u8, 255, 0, 255]] {
                let buf = image::RgbaImage::from_pixel(64, 64, image::Rgba(c));
                enc.encode_frame(image::Frame::new(buf)).unwrap();
            }
        }
        let ok = prepare_upload_image(&gif, UploadImageKind::Avatar).expect("passes");
        assert_eq!(ok.extension, "gif");
        assert_eq!(ok.bytes, gif); // untouched, animation preserved
        // An undecodable blob wearing a GIF header is refused, not uploaded.
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


#[cfg(test)]
mod animated_tests {
    use super::*;
    use image::codecs::gif::GifEncoder;
    use image::{Delay, Frame, RgbaImage};

    /// A small synthetic animation: `n` solid frames, each a different color,
    /// 120 ms apart.
    fn synth_gif(w: u32, h: u32, n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = GifEncoder::new_with_speed(&mut out, 30);
            enc.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
            for i in 0..n {
                let px = [((i * 80) % 255) as u8, 40, 200, 255];
                let buf = RgbaImage::from_pixel(w, h, image::Rgba(px));
                enc.encode_frame(Frame::from_parts(buf, 0, 0, Delay::from_numer_denom_ms(120, 1))).unwrap();
            }
        }
        out
    }

    fn decoded_frames(bytes: &[u8]) -> Vec<Frame> {
        use image::AnimationDecoder;
        let dec = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).unwrap();
        dec.into_frames().collect_frames().unwrap()
    }

    /// The whole point: a big animation comes back smaller, still moving, with
    /// its timing intact — not flattened to a still.
    #[test]
    fn a_transcoded_animation_shrinks_but_keeps_moving() {
        let src = synth_gif(400, 300, 3);
        let re = transcode_animated(&src, 160, ANIMATED_MAX_FRAMES).unwrap();
        assert_eq!(re.extension, "gif");
        let frames = decoded_frames(&re.bytes);
        assert_eq!(frames.len(), 3, "every frame survives");
        let f = &frames[0];
        assert!(f.buffer().width() <= 160 && f.buffer().height() <= 160, "downscaled to the target");
        let (ms, _) = f.delay().numer_denom_ms();
        assert!((60..=250).contains(&ms), "frame timing carried over, got {ms}ms");
    }

    /// Differencing writes transparent pixels for reused regions, which only
    /// renders if the disposal method says KEEP — the facade default left the
    /// reused region as literal holes. Composite the output and look at a
    /// pixel the second frame did not touch: it must still be the first
    /// frame's color, not blank.
    #[test]
    fn reused_pixels_carry_through_instead_of_becoming_holes() {
        let red = image::Rgba([200u8, 30, 30, 255]);
        let blue = image::Rgba([30u8, 30, 200, 255]);
        let base = RgbaImage::from_pixel(64, 64, red);
        let mut second = base.clone();
        for y in 20..30 {
            for x in 20..30 {
                second.put_pixel(x, y, blue);
            }
        }
        // Source GIF: two full frames.
        let mut src = Vec::new();
        {
            let mut enc = GifEncoder::new_with_speed(&mut src, 30);
            enc.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
            for f in [&base, &second] {
                enc.encode_frame(Frame::from_parts(f.clone(), 0, 0, Delay::from_numer_denom_ms(100, 1))).unwrap();
            }
        }
        let re = transcode_animated(&src, 160, 8).unwrap();
        let frames = decoded_frames(&re.bytes);
        assert_eq!(frames.len(), 2);
        let composited = frames[1].buffer();
        let far = composited.get_pixel(5, 5);
        assert!(far[3] == 255 && far[0] > 120, "an untouched pixel keeps frame 1's red, got {far:?}");
        let patch = composited.get_pixel(24, 24);
        assert!(patch[2] > 120, "the changed patch really is frame 2's blue, got {patch:?}");
    }

    /// The frame cap truncates a marathon animation instead of encoding forever.
    #[test]
    fn the_frame_cap_truncates_not_rejects() {
        let src = synth_gif(64, 64, 6);
        let re = transcode_animated(&src, 160, 4).unwrap();
        assert_eq!(decoded_frames(&re.bytes).len(), 4);
    }

    /// An oversized-canvas animation is refused (the caller falls back to a
    /// still or the verbatim bytes) — streaming decode must never be handed a
    /// frame worth more memory than the bound.
    #[test]
    fn an_animation_bomb_canvas_is_refused() {
        let src = synth_gif(64, 64, 2);
        // Forge the logical-screen descriptor to claim a giant canvas.
        let mut forged = src.clone();
        forged[6..8].copy_from_slice(&5000u16.to_le_bytes());
        assert!(transcode_animated(&forged, 160, 8).is_err());
    }

    /// The upload preparer keeps a small animation byte-identical (no churn),
    /// and downsizes an oversized one instead of refusing it.
    #[test]
    fn upload_prepare_downsizes_an_oversized_animated_avatar() {
        let small = synth_gif(128, 128, 2);
        let kept = prepare_upload_image(&small, UploadImageKind::Avatar).unwrap();
        assert_eq!(kept.bytes, small, "within budget and dims: untouched");

        let big = synth_gif(600, 600, 3);
        let prepared = prepare_upload_image(&big, UploadImageKind::Avatar).unwrap();
        assert_eq!(prepared.extension, "gif");
        let frames = decoded_frames(&prepared.bytes);
        assert_eq!(frames.len(), 3, "still animated after preparation");
        assert!(frames[0].buffer().width() <= 512, "resized to the avatar budget");
    }
}

#[cfg(test)]
mod animated_live_probe {
    use super::*;

    /// Manual probe: transcode a real file named by ANIMATED_FIXTURE and report
    /// sizes + timing. Not part of the suite.
    #[test]
    #[ignore]
    fn probe_real_animation() {
        let path = std::env::var("ANIMATED_FIXTURE").expect("set ANIMATED_FIXTURE=/path/to.gif");
        let target: u32 = std::env::var("ANIMATED_TARGET").ok().and_then(|v| v.parse().ok()).unwrap_or(160);
        let bytes = std::fs::read(&path).unwrap();
        let budget: usize = std::env::var("ANIMATED_BUDGET").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
        let t = std::time::Instant::now();
        let re = if budget == usize::MAX {
            transcode_animated(&bytes, target, ANIMATED_MAX_FRAMES).unwrap()
        } else {
            transcode_animated_budgeted(&bytes, target, budget).unwrap()
        };
        println!(
            "{}: {} KB -> {} KB at <={}px in {:?}",
            path, bytes.len() / 1024, re.bytes.len() / 1024, target, t.elapsed()
        );
        if let Ok(save) = std::env::var("PROBE_SAVE") {
            std::fs::write(save, &re.bytes).unwrap();
        }
    }
}

#[cfg(test)]
mod animated_visual_probe {
    use super::*;

    /// Manual probe: transcode ANIMATED_FIXTURE, then write composited frames
    /// 0 / mid / last of BOTH source and output to PROBE_OUT as PNGs.
    #[test]
    #[ignore]
    fn dump_frames_for_eyeballing() {
        use image::AnimationDecoder;
        let path = std::env::var("ANIMATED_FIXTURE").unwrap();
        let out_dir = std::env::var("PROBE_OUT").unwrap();
        let target: u32 = std::env::var("ANIMATED_TARGET").ok().and_then(|v| v.parse().ok()).unwrap_or(160);
        let bytes = std::fs::read(&path).unwrap();
        let budget: usize = std::env::var("ANIMATED_BUDGET").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
        let re = if budget == usize::MAX {
            transcode_animated(&bytes, target, ANIMATED_MAX_FRAMES).unwrap()
        } else {
            transcode_animated_budgeted(&bytes, target, budget).unwrap()
        };
        for (tag, data) in [("src", &bytes), ("out", &re.bytes)] {
            let dec = image::codecs::gif::GifDecoder::new(Cursor::new(data.as_slice())).unwrap();
            let frames = dec.into_frames().collect_frames().unwrap();
            let n = frames.len();
            for (label, idx) in [("first", 0), ("mid", n / 2), ("last", n - 1)] {
                frames[idx].buffer().save(format!("{out_dir}/{tag}_{label}.png")).unwrap();
            }
            println!("{tag}: {n} frames");
        }
    }
}

#[cfg(test)]
mod lossless_strip_tests {
    use super::*;

    fn seg(marker: u8, payload: &[u8]) -> Vec<u8> {
        let len = (payload.len() + 2) as u16;
        let mut v = vec![0xFF, marker, (len >> 8) as u8, (len & 0xFF) as u8];
        v.extend_from_slice(payload);
        v
    }

    fn photo() -> Vec<u8> {
        let img = image::RgbImage::from_fn(64, 32, |x, y| image::Rgb([(x * 4) as u8, (y * 8) as u8, 120]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90).encode_image(&img).unwrap();
        out.into_inner()
    }

    /// A camera-style JPEG: sideways EXIF, XMP, a comment, IPTC, and an MPF trailer.
    fn tagged_photo() -> Vec<u8> {
        let clean = photo();
        let mut v = vec![0xFF, 0xD8];
        v.extend(orientation_app1(6));
        v.extend(seg(0xE1, b"http://ns.adobe.com/xap/1.0/\0<gps>51.5,-0.12</gps>"));
        v.extend(seg(0xFE, b"a private note"));
        v.extend(seg(0xED, b"Photoshop 3.0\0secret-caption"));
        v.extend(seg(0xE2, b"MPF\0secondary-image-offsets"));
        v.extend_from_slice(&clean[2..]);
        v.extend_from_slice(b"\xFF\xD8trailing-depth-map-bytes\xFF\xD9");
        v
    }

    fn pixels(bytes: &[u8]) -> Vec<u8> {
        vector_core::crypto::decode_image_bounded(bytes).unwrap().to_rgb8().into_raw()
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn a_jpeg_loses_every_tag_but_its_orientation_and_keeps_its_pixels() {
        let tagged = tagged_photo();
        let out = strip_metadata_keep_orientation(&tagged, "jpg").expect("strips in place");
        assert_eq!(pixels(&out), pixels(&tagged), "not a pixel re-encoded");
        assert_eq!(header_orientation(&out), Some(6), "still upright on arrival");
        assert_eq!(header_dimensions_oriented(&out), Some((32, 64)), "a quarter turn swaps the sides");
        for secret in [&b"<gps>"[..], b"private note", b"secret-caption", b"MPF\0", b"depth-map"] {
            assert!(!contains(&out, secret), "{} survived", String::from_utf8_lossy(secret));
        }
        assert_eq!(&out[out.len() - 2..], b"\xFF\xD9", "nothing after EOI");
    }

    #[test]
    fn an_upright_jpeg_carries_no_exif_at_all() {
        let mut v = vec![0xFF, 0xD8];
        v.extend(seg(0xE1, b"Exif\0\0MM\0\x2A\0\0\0\x08\0\0\0\0\0\0"));
        v.extend_from_slice(&photo()[2..]);
        let out = strip_metadata_keep_orientation(&v, "jpeg").unwrap();
        assert!(!contains(&out, b"Exif"));
        assert_eq!(pixels(&out), pixels(&v));
    }

    #[test]
    fn a_jfif_thumbnail_goes_but_a_bare_header_stays() {
        let bare = [&b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"[..]].concat();
        let thumbed = [&b"JFIF\0\x01\x01\0\0\x01\0\x01\x01\x01"[..], &[9u8, 9, 9]].concat();
        for (payload, kept) in [(bare, true), (thumbed, false)] {
            let mut v = vec![0xFF, 0xD8];
            v.extend(seg(0xE0, &payload));
            v.extend_from_slice(&photo()[2..]);
            let out = strip_metadata_keep_orientation(&v, "jpg").unwrap();
            assert_eq!(contains(&out, &payload), kept);
        }
    }

    #[test]
    fn a_truncated_jpeg_falls_back_to_a_re_encode() {
        let t = tagged_photo();
        let cut = &t[..t.len() / 2];
        assert!(strip_metadata_keep_orientation(cut, "jpg").is_none());
    }

    fn png_chunk(ty: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = (data.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(ty);
        v.extend_from_slice(data);
        let mut crc = !0u32;
        for &b in ty.iter().chain(data) {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        v.extend_from_slice(&(!crc).to_be_bytes());
        v
    }

    /// A screenshot-style PNG: text, XMP, a private chunk and `eXIf` ahead of the pixels.
    fn tagged_png(orientation: u8) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(40, 20, |x, y| image::Rgba([x as u8 * 6, y as u8 * 12, 7, 255]));
        let mut clean = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img).write_to(&mut clean, image::ImageFormat::Png).unwrap();
        let clean = clean.into_inner();
        let ihdr_end = 8 + 12 + 13;
        let exif = &orientation_app1(orientation)[10..];
        let mut v = clean[..ihdr_end].to_vec();
        v.extend(png_chunk(b"tEXt", b"Comment\0taken at home"));
        v.extend(png_chunk(b"iTXt", b"XML:com.adobe.xmp\0\0\0\0\0<gps/>"));
        v.extend(png_chunk(b"caBX", b"provenance-manifest"));
        v.extend(png_chunk(b"eXIf", exif));
        v.extend(png_chunk(b"pHYs", &[0, 0, 0x16, 0x25, 0, 0, 0x16, 0x25, 1]));
        v.extend_from_slice(&clean[ihdr_end..]);
        v
    }

    #[test]
    fn a_png_keeps_its_pixels_and_density_and_drops_the_rest() {
        let tagged = tagged_png(1);
        let out = strip_metadata_keep_orientation(&tagged, "png").expect("strips in place");
        assert_eq!(pixels(&out), pixels(&tagged));
        assert!(contains(&out, b"pHYs"));
        for secret in [&b"taken at home"[..], b"<gps/>", b"provenance", b"eXIf"] {
            assert!(!contains(&out, secret), "{} survived", String::from_utf8_lossy(secret));
        }
    }

    #[test]
    fn what_the_strip_leaves_reads_as_clean() {
        let has = |b: &[u8], ext| header_has_metadata(&mut std::io::Cursor::new(b), ext);
        let jpeg = tagged_photo();
        assert_eq!(has(&jpeg, "jpg"), Some(true));
        assert_eq!(has(&strip_metadata_keep_orientation(&jpeg, "jpg").unwrap(), "jpg"), Some(false), "the kept orientation is not metadata");
        let png = tagged_png(1);
        assert_eq!(has(&png, "png"), Some(true));
        assert_eq!(has(&strip_metadata_keep_orientation(&png, "png").unwrap(), "png"), Some(false));
    }

    #[test]
    fn a_rotated_png_is_left_to_the_re_encode() {
        assert!(strip_metadata_keep_orientation(&tagged_png(6), "png").is_none());
    }
}


