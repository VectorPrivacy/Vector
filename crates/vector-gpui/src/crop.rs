//! Crop and re-encode in the source's own format, a moving picture kept moving: the
//! desktop client's emoji-pack crop, so avatars and emojis come out byte for byte alike.

use std::io::Cursor;

use image::{AnimationDecoder, ImageEncoder};

/// Downscale floor when shrinking an oversized crop to fit a byte cap.
const MIN_EMOJI_DIM: u32 = 48;
/// Static emojis upload at most this wide (`UploadImageKind::Emoji`).
const MAX_UPLOAD_DIM: u32 = 256;

fn decode_webp_frames(bytes: &[u8]) -> Result<Vec<(image::RgbaImage, u32)>, String> {
    // Animated WebPs go through libwebp, which composites disposal and blending;
    // `image-webp` 0.2.x leaves transparent regions holding the prior frame.
    if let Ok(anim) = crate::shared::animated_webp::Decoder::new(bytes) {
        if anim.frame_count() > 1 {
            if let Ok(frames) = anim.map(|f| f.map(|(rgba, ms)| (rgba, ms.max(20)))).collect() {
                return Ok(frames);
            }
        }
    }

    // Still WebP (or an animation libwebp could not finish, as its first frame): image-webp.
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::WebP);
    reader.limits(vector_core::crypto::bounded_image_limits());
    let img = reader
        .decode()
        .map_err(|e| format!("webp still: {}", e))?
        .into_rgba8();
    Ok(vec![(img, 100)])
}

fn decode_gif_frames(bytes: &[u8]) -> Result<Vec<(image::RgbaImage, u32)>, String> {
    let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))
        .map_err(|e| format!("gif decoder: {}", e))?;
    let frames: Vec<image::Frame> = decoder.into_frames().collect_frames()
        .map_err(|e| format!("gif frames: {}", e))?;
    Ok(frames.into_iter().map(extract_frame).collect())
}

fn decode_apng_frames(bytes: &[u8]) -> Result<Vec<(image::RgbaImage, u32)>, String> {
    let decoder = image::codecs::png::PngDecoder::new(Cursor::new(bytes))
        .map_err(|e| format!("png decoder: {}", e))?
        .apng()
        .map_err(|e| format!("apng: {}", e))?;
    let frames: Vec<image::Frame> = decoder.into_frames().collect_frames()
        .map_err(|e| format!("apng frames: {}", e))?;
    Ok(frames.into_iter().map(extract_frame).collect())
}

fn decode_static_fallback(bytes: &[u8]) -> Result<Vec<(image::RgbaImage, u32)>, String> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("sniff: {}", e))?;
    reader.limits(vector_core::crypto::bounded_image_limits());
    let img = reader
        .decode()
        .map_err(|e| format!("decode: {}", e))?
        .into_rgba8();
    Ok(vec![(img, 100)])
}

fn extract_frame(f: image::Frame) -> (image::RgbaImage, u32) {
    // image::Frame::delay is `Delay` which gives (numer, denom) for ms.
    let (numer, denom) = f.delay().numer_denom_ms();
    let dur = numer.checked_div(denom).map_or(100, |d| d.max(20));
    (f.into_buffer(), dur)
}


pub struct EmojiCropInput {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}


pub(crate) fn crop_image(bytes: Vec<u8>, x: u32, y: u32, w: u32, h: u32) -> Result<Vec<u8>, String> {
    if w == 0 || h == 0 {
        return Err("crop must be non-empty".to_string());
    }
    let mime = vector_core::crypto::mime_from_magic_bytes(&bytes).to_string();
    crop_and_reencode_blocking(EmojiCropInput { bytes, mime, x, y, w, h }, None)
}

/// A crop drawn on whatever copy the page showed (maybe a scaled preview), made on the image itself.
pub(crate) fn crop_drawn(bytes: Vec<u8>, crop: vector_core::image_crop::CropRect) -> Result<Vec<u8>, String> {
    let dims = vector_core::image_crop::upright_dims(&bytes).ok_or("Couldn't read the image")?;
    let (x, y, w, h) = vector_core::image_crop::map_crop(crop, dims);
    crop_image(bytes, x, y, w, h)
}

/// `max_bytes`: shrink until the output fits, or `None` to keep the native size.
fn crop_and_reencode_blocking(input: EmojiCropInput, max_bytes: Option<usize>) -> Result<Vec<u8>, String> {
    let EmojiCropInput { bytes, mime, x, y, w, h } = input;
    let mime = mime.to_lowercase();

    let format = if mime.contains("jpeg") || mime.contains("jpg") {
        "jpeg"
    } else if mime.contains("png") {
        "png"
    } else if mime.contains("gif") {
        "gif"
    } else if mime.contains("webp") {
        // Animated WebP routes to the animated round-trip; libwebp's frame count, so a
        // one-frame ANIM counts as a still.
        if crate::shared::animated_webp::Decoder::new(&bytes).is_ok_and(|d| d.frame_count() > 1) {
            return crop_animated_webp(&bytes, x, y, w, h, max_bytes);
        }
        "webp"
    } else {
        return Err(format!("unsupported mime: {}", mime));
    };

    if format == "gif" {
        return crop_gif(&bytes, x, y, w, h, max_bytes);
    }

    let mut reader = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|e| format!("sniff: {}", e))?;
    reader.limits(vector_core::crypto::bounded_image_limits());
    // Upright, as the cropper showed it: a webview applies EXIF orientation, so the
    // rectangle is in rotated pixels.
    let mut decoder = reader.into_decoder().map_err(|e| format!("decode: {}", e))?;
    let orientation = image::ImageDecoder::orientation(&mut decoder).ok();
    let mut dynimg = image::DynamicImage::from_decoder(decoder).map_err(|e| format!("decode: {}", e))?;
    if let Some(o) = orientation {
        dynimg.apply_orientation(o);
    }
    let (sw, sh) = (dynimg.width(), dynimg.height());
    validate_crop_bounds(x, y, w, h, sw, sh)?;
    let mut cropped = dynimg.crop_imm(x, y, w, h).to_rgba8();
    // An emoji uploads at most MAX_UPLOAD_DIM wide: sized once here, the budget is met in one encode.
    if max_bytes.is_some() && w > MAX_UPLOAD_DIM {
        cropped = image::imageops::resize(&cropped, MAX_UPLOAD_DIM, MAX_UPLOAD_DIM, image::imageops::FilterType::Lanczos3);
    }
    encode_within_budget(&[(cropped, 0u32)], max_bytes, |scaled| encode_static(&scaled[0].0, format))
}

/// Encode `frames` via `encode`, shrinking them (aspect kept) until the output
/// fits `max_bytes` (or the `MIN_EMOJI_DIM` floor on the width). The common path
/// encodes once at native size; only an oversized result (e.g. a GIF the
/// re-encoder inflated past the cap) triggers downscaling. Emojis render tiny,
/// so shrinking is imperceptible and is the only way such an emote can publish
/// at all instead of being rejected.
fn encode_within_budget<F>(
    frames: &[(image::RgbaImage, u32)],
    max_bytes: Option<usize>,
    encode: F,
) -> Result<Vec<u8>, String>
where
    F: Fn(&[(image::RgbaImage, u32)]) -> Result<Vec<u8>, String>,
{
    let out = encode(frames)?;
    let (src_w, src_h) = frames.first().map(|(img, _)| img.dimensions()).unwrap_or((0, 0));
    // No budget, fits, or already at/below the floor (shrinking a tiny input
    // would only upscale it) — take the native encode.
    let Some(max_bytes) = max_bytes else { return Ok(out) };
    if out.len() <= max_bytes || src_w <= MIN_EMOJI_DIM {
        return Ok(out);
    }
    let mut w = src_w;
    loop {
        // ~12% smaller per step (~0.77x area) — converges in a couple passes.
        w = ((w * 88) / 100).max(MIN_EMOJI_DIM);
        let h = ((w as u64 * src_h as u64) / src_w as u64).max(1) as u32;
        let scaled: Vec<(image::RgbaImage, u32)> = frames
            .iter()
            .map(|(img, d)| {
                (
                    image::imageops::resize(img, w, h, image::imageops::FilterType::Lanczos3),
                    *d,
                )
            })
            .collect();
        let out = encode(&scaled)?;
        if out.len() <= max_bytes || w <= MIN_EMOJI_DIM {
            return Ok(out);
        }
    }
}

fn crop_animated_webp(bytes: &[u8], x: u32, y: u32, w: u32, h: u32, max_bytes: Option<usize>) -> Result<Vec<u8>, String> {
    // Reuse the existing libwebp-based decoder — it already returns
    // fully-composed RGBA frames with per-frame deltas (the spec's
    // disposal + blend logic is handled inside libwebp).
    let frames = decode_webp_frames(bytes)?;
    if frames.is_empty() {
        return Err("webp: no frames".to_string());
    }
    let (fw, fh) = (frames[0].0.width(), frames[0].0.height());
    validate_crop_bounds(x, y, w, h, fw, fh)?;

    let cropped: Vec<(image::RgbaImage, u32)> = frames
        .into_iter()
        .map(|(rgba, duration)| {
            (image::imageops::crop_imm(&rgba, x, y, w, h).to_image(), duration.max(20))
        })
        .collect();

    encode_within_budget(&cropped, max_bytes, |scaled| {
        let (fw, fh) = scaled[0].0.dimensions();
        let mut encoder = crate::shared::animated_webp::Encoder::lossy(fw, fh, 80.0)?;
        for (img, duration) in scaled.iter() {
            encoder.add(img.as_raw(), *duration)?;
        }
        encoder.finish()
    })
}

fn crop_gif(bytes: &[u8], x: u32, y: u32, w: u32, h: u32, max_bytes: Option<usize>) -> Result<Vec<u8>, String> {
    if let Some(out) = vector_core::image_crop::crop_gif(bytes, x, y, w, h) {
        if max_bytes.is_none_or(|max| out.len() <= max) {
            return Ok(out);
        }
    }
    let frames = decode_gif_frames(bytes)?;
    if frames.is_empty() {
        return Err("gif: no frames".to_string());
    }
    let (fw, fh) = (frames[0].0.width(), frames[0].0.height());
    validate_crop_bounds(x, y, w, h, fw, fh)?;

    let cropped: Vec<(image::RgbaImage, u32)> = frames
        .into_iter()
        .map(|(rgba, duration_ms)| {
            (image::imageops::crop_imm(&rgba, x, y, w, h).to_image(), duration_ms)
        })
        .collect();

    encode_within_budget(&cropped, max_bytes, |scaled| {
        let mut out = Vec::new();
        {
            // Speed 10 = fastest encode at lowest CPU. Emoji-scale GIFs are
            // small enough that quality difference vs default is negligible.
            let mut encoder = image::codecs::gif::GifEncoder::new_with_speed(&mut out, 10);
            encoder
                .set_repeat(image::codecs::gif::Repeat::Infinite)
                .map_err(|e| format!("gif repeat: {}", e))?;
            for (rgba, duration_ms) in scaled {
                let delay = image::Delay::from_numer_denom_ms((*duration_ms).max(20), 1);
                let frame = image::Frame::from_parts(rgba.clone(), 0, 0, delay);
                encoder
                    .encode_frame(frame)
                    .map_err(|e| format!("gif frame: {}", e))?;
            }
        }
        Ok(out)
    })
}

fn validate_crop_bounds(x: u32, y: u32, w: u32, h: u32, sw: u32, sh: u32) -> Result<(), String> {
    if x.saturating_add(w) > sw || y.saturating_add(h) > sh {
        return Err("crop outside source bounds".to_string());
    }
    Ok(())
}

fn encode_static(rgba: &image::RgbaImage, format: &str) -> Result<Vec<u8>, String> {
    if format == "webp" {
        // libwebp lossy: much smaller than image-webp's lossless-only encoder for natural images.
        return crate::shared::animated_webp::encode_still(rgba.as_raw(), rgba.width(), rgba.height(), 88.0);
    }
    let mut out = Vec::with_capacity((rgba.width() * rgba.height()) as usize);
    match format {
        "png" => {
            image::codecs::png::PngEncoder::new_with_quality(
                &mut out,
                image::codecs::png::CompressionType::Best,
                image::codecs::png::FilterType::Adaptive,
            )
            .write_image(rgba.as_raw(), rgba.width(), rgba.height(), image::ExtendedColorType::Rgba8)
            .map_err(|e| format!("png encode: {}", e))?;
        }
        "jpeg" => {
            // JPEG has no alpha — composite onto white before encode so
            // transparent regions don't render as random noise.
            let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
            for (dst, src) in rgb.pixels_mut().zip(rgba.pixels()) {
                let a = src.0[3] as u32;
                let inv = 255 - a;
                dst.0[0] = ((src.0[0] as u32 * a + 255 * inv) / 255) as u8;
                dst.0[1] = ((src.0[1] as u32 * a + 255 * inv) / 255) as u8;
                dst.0[2] = ((src.0[2] as u32 * a + 255 * inv) / 255) as u8;
            }
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88)
                .write_image(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
                .map_err(|e| format!("jpeg encode: {}", e))?;
        }
        _ => return Err(format!("encode: unsupported format {}", format)),
    }
    Ok(out)
}

