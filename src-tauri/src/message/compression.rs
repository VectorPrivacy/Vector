//! Image compression functions.
//!
//! This module handles:
//! - Image compression with resize to max 1920px
//! - GIF preservation (skip compression to keep animation)
//! - PNG for transparent images, JPEG for opaque
//! - ThumbHash generation for previews

use std::sync::Arc;

use super::types::{CachedCompressedImage, ImageMetadata};

#[cfg(target_os = "android")]
use super::types::ANDROID_FILE_CACHE;
#[cfg(target_os = "android")]
use crate::android::filesystem;

/// Prepare an image for sending, honouring the compress + keep-metadata choices.
///
/// The 2x2 of behaviours:
/// - compress + strip  -> resize to MAX_DIMENSION and re-encode (metadata dropped)
/// - compress + keep   -> resize + re-encode, then re-attach the original EXIF
///   (orientation normalised, since pixels are baked upright)
/// - full-res + strip  -> re-encode at full resolution (metadata dropped, orientation baked)
/// - full-res + keep   -> ship the original bytes untouched (all metadata + orientation intact)
///
/// Animations (GIF, animated WebP, APNG) keep moving: compressing re-encodes them as animated
/// WebP, otherwise they ship as-is.
///
/// `thumbhash_hint` is a thumbhash already made from these pixels (the preview's
/// pre-compression): a branch that keeps the pixels as they are pairs it with the
/// header's dimensions instead of decoding the whole image for them.
pub(crate) fn prepare_outbound_image(
    bytes: Arc<Vec<u8>>,
    extension: &str,
    compress: bool,
    keep_metadata: bool,
    thumbhash_hint: Option<&str>,
) -> Result<CachedCompressedImage, String> {
    use crate::shared::image::{
        calculate_resize_dimensions, reattach_exif_jpeg,
        MAX_DIMENSION, JPEG_QUALITY_STANDARD, JPEG_QUALITY_HIGH,
    };

    let original_size = bytes.len() as u64;

    let meta_from = |img: &::image::DynamicImage| -> Option<ImageMetadata> {
        let (w, h) = (img.width(), img.height());
        crate::util::generate_thumbhash_from_image(img)
            .map(|thumbhash| ImageMetadata { thumbhash, width: w, height: h })
    };

    // These passthrough/lossless branches only decode to build preview metadata,
    // so a decode failure (e.g. an image past the bounded-decoder's size limit)
    // must NOT fail the send — ship the bytes with img_meta = None.
    let meta_opt = |b: &[u8]| {
        thumbhash_hint
            .filter(|t| !t.is_empty())
            .and_then(|t| {
                let (width, height) = crate::shared::image::header_dimensions_oriented(b)?;
                Some(ImageMetadata { thumbhash: t.to_string(), width, height })
            })
            .or_else(|| {
                // The hash reads 100 pixels a side; the dimensions come from the header.
                let img = crate::shared::image::decode_image(b, 100).ok()?;
                let (width, height) = crate::shared::image::header_dimensions_oriented(b).unwrap_or((img.width(), img.height()));
                crate::util::generate_thumbhash_from_image(&img).map(|thumbhash| ImageMetadata { thumbhash, width, height })
            })
    };

    // Stripping or re-encoding as a still would drop every frame but the first, so an
    // animation takes only the compress choice.
    if crate::shared::image::animated_format(&bytes).is_some() {
        if compress {
            return Ok(compress_animation(bytes, extension));
        }
        let img_meta = meta_opt(&bytes);
        return Ok(CachedCompressedImage {
            bytes, extension: extension.to_string(), img_meta,
            original_size, compressed_size: original_size,
        });
    }

    // Keep metadata at full resolution: ship the original file untouched. EXIF
    // (including orientation) stays intact and the receiver's <img> applies it;
    // dims/thumbhash come from the oriented decode so the preview box matches.
    if keep_metadata && !compress {
        let img_meta = meta_opt(&bytes);
        return Ok(CachedCompressedImage {
            bytes, extension: extension.to_string(), img_meta,
            original_size, compressed_size: original_size,
        });
    }

    // Strip metadata at full resolution: drop the privacy tags losslessly while
    // keeping orientation, so the pixels are never re-encoded (no quality loss,
    // no file growth). Falls through to the re-encode below when a container
    // can't be stripped in place (not JPEG or PNG, malformed, or a rotated PNG).
    if !keep_metadata && !compress {
        if let Some(stripped) = crate::shared::image::strip_metadata_keep_orientation(&bytes, extension) {
            let img_meta = meta_opt(&bytes);
            let compressed_size = stripped.len() as u64;
            return Ok(CachedCompressedImage {
                bytes: Arc::new(stripped),
                extension: extension.to_string(),
                img_meta,
                original_size,
                compressed_size,
            });
        }
    }

    // Re-encode paths. Decoding bakes EXIF orientation into pixels.
    let img = crate::shared::image::decode_image(&bytes, if compress { MAX_DIMENSION } else { 0 })?;
    let (w, h) = (img.width(), img.height());
    let (nw, nh) = if compress {
        calculate_resize_dimensions(w, h, MAX_DIMENSION)
    } else {
        (w, h)
    };
    let resized = if nw != w || nh != h {
        crate::shared::image::resize_fit(&img, nw, nh, ::image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let (aw, ah) = (resized.width(), resized.height());
    let img_meta = meta_from(&resized);
    // Full-resolution sends (compression declined) use higher quality, and keep
    // a PNG source lossless rather than re-encoding a screenshot to JPEG just to
    // strip its metadata. Compression still picks the smaller format by content.
    let (out_bytes, out_ext): (Vec<u8>, &'static str) = if !compress && extension.eq_ignore_ascii_case("png") {
        (crate::shared::image::encode_png(resized.to_rgba8().as_raw(), aw, ah)?, "png")
    } else {
        let quality = if compress { JPEG_QUALITY_STANDARD } else { JPEG_QUALITY_HIGH };
        let encoded = crate::shared::image::encode_decoded_auto(&resized, quality)?;
        (encoded.bytes, encoded.extension)
    };
    let mut out_bytes = out_bytes;

    // Keep + re-encode: carry the original EXIF onto the JPEG output. The source
    // is read as its own container so TIFF/WebP photos keep their tags too.
    if keep_metadata && out_ext == "jpg" {
        out_bytes = reattach_exif_jpeg(out_bytes, &bytes, extension);
    }

    let compressed_size = out_bytes.len() as u64;
    Ok(CachedCompressedImage {
        bytes: Arc::new(out_bytes),
        extension: out_ext.to_string(),
        img_meta,
        original_size,
        compressed_size,
    })
}

/// An animation compressed for sending (animated WebP), or the original when that doesn't save
/// at least a tenth, where re-encoding would only cost detail.
fn compress_animation(bytes: Arc<Vec<u8>>, extension: &str) -> CachedCompressedImage {
    let original_size = bytes.len() as u64;
    let smaller = crate::shared::image::compress_animated_for_send(&bytes)
        .ok()
        .filter(|e| (e.bytes.len() as u64) * 10 <= original_size * 9);
    let (bytes, extension) = match smaller {
        Some(e) => (Arc::new(e.bytes), e.extension),
        None => (bytes, extension),
    };
    // The first frame gives the preview; a decode failure only costs the placeholder.
    let img_meta = crate::shared::image::decode_image(&bytes, 100).ok().and_then(|img| {
        let (width, height) = (img.width(), img.height());
        crate::util::generate_thumbhash_from_image(&img).map(|thumbhash| ImageMetadata { thumbhash, width, height })
    });
    CachedCompressedImage { compressed_size: bytes.len() as u64, bytes, extension: extension.to_string(), img_meta, original_size }
}

/// Internal function to compress bytes
/// Takes Arc<Vec<u8>> for zero-copy sharing.
/// If `min_savings_percent` is Some and compression doesn't meet threshold,
/// returns original bytes with metadata (no wasted clone).
pub(super) fn compress_bytes_internal(
    bytes: Arc<Vec<u8>>,
    extension: &str,
    min_savings_percent: Option<u64>,
) -> Result<CachedCompressedImage, String> {
    let original_size = bytes.len() as u64;

    if crate::shared::image::animated_format(&bytes).is_some() {
        return Ok(compress_animation(bytes, extension));
    }

    // Determine target dimensions (max 1920px on longest side)
    use crate::shared::image::{calculate_resize_dimensions, MAX_DIMENSION};

    // Load and decode the image (EXIF orientation baked into pixels)
    let img = crate::shared::image::decode_image(&bytes, MAX_DIMENSION)?;
    let (width, height) = (img.width(), img.height());
    let (new_width, new_height) = calculate_resize_dimensions(width, height, MAX_DIMENSION);

    // Resize if needed
    let resized_img = if new_width != width || new_height != height {
        crate::shared::image::resize_fit(&img, new_width, new_height, ::image::imageops::FilterType::Lanczos3)
    } else {
        img
    };

    let actual_width = resized_img.width();
    let actual_height = resized_img.height();

    // Generate metadata from final image only (avoid redundant thumbhash generation)
    let final_meta = crate::util::generate_thumbhash_from_image(&resized_img)
        .map(|thumbhash| ImageMetadata {
            thumbhash,
            width: actual_width,
            height: actual_height,
        });

    // Keep reference to original metadata for fallback path
    let img_meta = final_meta.clone();

    // Encode as PNG (alpha/small) or JPEG (standard)
    use crate::shared::image::JPEG_QUALITY_STANDARD;
    let encoded = crate::shared::image::encode_decoded_auto(&resized_img, JPEG_QUALITY_STANDARD)?;
    let compressed_bytes = encoded.bytes;
    let new_extension = encoded.extension;

    let compressed_size = compressed_bytes.len() as u64;

    // Check if compression meets minimum savings threshold
    if let Some(min_percent) = min_savings_percent {
        let savings_percent = if original_size > 0 && compressed_size < original_size {
            ((original_size - compressed_size) * 100) / original_size
        } else {
            0
        };

        if savings_percent < min_percent {
            // Compression not worth it - return original
            return Ok(CachedCompressedImage {
                bytes,
                extension: extension.to_string(),
                img_meta,
                original_size,
                compressed_size: original_size,
            });
        }
    }

    Ok(CachedCompressedImage {
        bytes: Arc::new(compressed_bytes),
        extension: new_extension.to_string(),
        img_meta: final_meta,
        original_size,
        compressed_size,
    })
}

/// Route an outbound image to the right processing, reusing a pre-compressed
/// result only for the default (strip + compress) hot path.
///
/// `precompressed` is the background pre-compression output (always the
/// stripped + resized version). It's reused only when the user wants exactly
/// that; every other combination re-derives from `original_bytes` so metadata
/// and full-resolution choices are honoured.
pub(crate) fn process_image_for_send(
    original_bytes: Arc<Vec<u8>>,
    extension: &str,
    use_compression: bool,
    keep_metadata: bool,
    precompressed: Option<CachedCompressedImage>,
) -> Result<CachedCompressedImage, String> {
    if !keep_metadata && use_compression {
        if let Some(pc) = precompressed {
            return Ok(pc);
        }
    }
    let hint = precompressed.as_ref().and_then(|pc| pc.img_meta.as_ref()).map(|m| m.thumbhash.as_str());
    prepare_outbound_image(original_bytes, extension, use_compression, keep_metadata, hint)
}

/// Internal function to compress an image and return cached data
pub(super) fn compress_image_internal(file_path: &str) -> Result<CachedCompressedImage, String> {
    #[cfg(not(target_os = "android"))]
    {
        // Get extension early to check if it's a GIF
        let extension = file_path
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_lowercase();

        // Read the file into memory
        let file_data = std::fs::read(file_path)
            .map_err(|e| format!("Failed to read file: {}", e))?;

        let original_size = file_data.len() as u64;

        if crate::shared::image::animated_format(&file_data).is_some() {
            return Ok(compress_animation(Arc::new(file_data), &extension));
        }

        // Try to load and decode the image (EXIF orientation baked into pixels)
        let img = crate::shared::image::decode_image(&file_data, crate::shared::image::MAX_DIMENSION)?;

        // Determine target dimensions (max 1920px on longest side)
        use crate::shared::image::{calculate_resize_dimensions, MAX_DIMENSION, JPEG_QUALITY_STANDARD};
        let (width, height) = (img.width(), img.height());
        let (new_width, new_height) = calculate_resize_dimensions(width, height, MAX_DIMENSION);

        // Resize if needed
        let resized_img = if new_width != width || new_height != height {
            crate::shared::image::resize_fit(&img, new_width, new_height, ::image::imageops::FilterType::Lanczos3)
        } else {
            img
        };

        let actual_width = resized_img.width();
        let actual_height = resized_img.height();

        let img_meta = crate::util::generate_thumbhash_from_image(&resized_img)
            .map(|thumbhash| ImageMetadata {
                thumbhash,
                width: actual_width,
                height: actual_height,
            });

        let encoded = crate::shared::image::encode_decoded_auto(&resized_img, JPEG_QUALITY_STANDARD)?;
        let compressed_bytes = encoded.bytes;
        let extension = encoded.extension;

        let compressed_size = compressed_bytes.len() as u64;

        Ok(CachedCompressedImage {
            bytes: Arc::new(compressed_bytes),
            extension: extension.to_string(),
            img_meta,
            original_size,
            compressed_size,
        })
    }
    #[cfg(target_os = "android")]
    {
        // Check if we have cached bytes for this URI
        let (bytes, extension) = {
            let cache = ANDROID_FILE_CACHE.lock().unwrap();
            if let Some((cached_bytes, ext, _, _)) = cache.get(file_path) {
                (cached_bytes.clone(), ext.clone())
            } else {
                drop(cache);
                // Fall back to reading directly (may fail if permission expired)
                let (raw_bytes, ext) = filesystem::read_android_uri_bytes(file_path.to_string())?;
                (Arc::new(raw_bytes), ext)
            }
        };
        let original_size = bytes.len() as u64;

        if crate::shared::image::animated_format(&bytes).is_some() {
            return Ok(compress_animation(bytes, &extension));
        }

        // Try to load and decode the image (EXIF orientation baked into pixels)
        let img = crate::shared::image::decode_image(&bytes, crate::shared::image::MAX_DIMENSION)?;

        // Determine target dimensions (max 1920px on longest side)
        use crate::shared::image::{calculate_resize_dimensions, MAX_DIMENSION, JPEG_QUALITY_STANDARD};
        let (width, height) = (img.width(), img.height());
        let (new_width, new_height) = calculate_resize_dimensions(width, height, MAX_DIMENSION);

        // Resize if needed
        let resized_img = if new_width != width || new_height != height {
            crate::shared::image::resize_fit(&img, new_width, new_height, ::image::imageops::FilterType::Lanczos3)
        } else {
            img
        };

        let actual_width = resized_img.width();
        let actual_height = resized_img.height();

        let img_meta = crate::util::generate_thumbhash_from_image(&resized_img)
            .map(|thumbhash| ImageMetadata {
                thumbhash,
                width: actual_width,
                height: actual_height,
            });

        let encoded = crate::shared::image::encode_decoded_auto(&resized_img, JPEG_QUALITY_STANDARD)?;
        let compressed_bytes = encoded.bytes;
        let extension = encoded.extension;

        let compressed_size = compressed_bytes.len() as u64;

        Ok(CachedCompressedImage {
            bytes: Arc::new(compressed_bytes),
            extension: extension.to_string(),
            img_meta,
            original_size,
            compressed_size,
        })
    }
}

#[cfg(test)]
mod gif_send_tests {
    use super::*;

    fn gif(w: u16, h: u16, frames: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = gif::Encoder::new(&mut out, w, h, &[]).unwrap();
            enc.set_repeat(gif::Repeat::Infinite).unwrap();
            for i in 0..frames {
                // Grain that shifts every frame, like a dithered video.
                let mut px: Vec<u8> = (0..u32::from(w) * u32::from(h))
                    .flat_map(|p| {
                        let (x, y) = (p % u32::from(w) + i * 7, p / u32::from(w));
                        let n = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) >> 27;
                        [(x / 4 % 256) as u8, (y / 3 % 256) as u8, (n * 8) as u8, 255]
                    })
                    .collect();
                let mut f = gif::Frame::from_rgba_speed(w, h, &mut px, 30);
                f.delay = 7;
                enc.write_frame(&f).unwrap();
            }
        }
        out
    }

    #[test]
    fn a_large_gif_is_sent_smaller_and_still_animated() {
        let src = gif(1100, 620, 6);
        let sent = prepare_outbound_image(Arc::new(src.clone()), "gif", true, false, None).unwrap();
        assert_eq!(sent.extension, "webp");
        assert!(sent.compressed_size * 10 <= sent.original_size * 9, "{} of {}", sent.compressed_size, sent.original_size);
        let frames: Vec<(image::RgbaImage, u32)> = crate::shared::animated_webp::Decoder::new(&sent.bytes).unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(frames.len(), 6, "every frame kept");
        assert!(frames.iter().all(|(_, ms)| *ms == 70), "timing kept");
        let (w, h) = frames[0].0.dimensions();
        assert_eq!(w.max(h), crate::shared::image::ANIMATED_SEND_MAX_DIM);
        let meta = sent.img_meta.expect("preview metadata");
        assert_eq!((meta.width, meta.height), (w, h));
    }

    fn animated_webp(w: u32, h: u32, frames: u32) -> Vec<u8> {
        let cfg = webp::WebPConfig::new().unwrap();
        let px: Vec<Vec<u8>> = (0..frames).map(|i| (0..w * h).flat_map(|p| [(p % w * 2 + i * 40) as u8, (p / w) as u8, 90, 255]).collect()).collect();
        let mut enc = webp::AnimEncoder::new(w, h, &cfg);
        enc.set_loop_count(0);
        for (i, f) in px.iter().enumerate() {
            enc.add_frame(webp::AnimFrame::from_rgba(f, w, h, i as i32 * 100));
        }
        enc.try_encode().unwrap().to_vec()
    }

    /// Every combination of the send options keeps an animated WebP moving.
    #[test]
    fn an_animated_webp_is_never_flattened_to_a_still() {
        let src = animated_webp(300, 200, 4);
        assert_eq!(crate::shared::image::animated_format(&src), Some("webp"));
        for (compress, keep) in [(true, false), (true, true), (false, false), (false, true)] {
            let sent = prepare_outbound_image(Arc::new(src.clone()), "webp", compress, keep, None).unwrap();
            assert!(crate::shared::image::animated_format(&sent.bytes).is_some(), "compress {compress}, keep {keep}: flattened to {}", sent.extension);
        }
        assert!(crate::shared::image::animated_format(&compress_bytes_internal(Arc::new(src), "webp", None).unwrap().bytes).is_some());
    }

    #[test]
    fn a_gif_that_would_not_shrink_ships_untouched() {
        let src = gif(6, 4, 2);
        for compress in [true, false] {
            let sent = prepare_outbound_image(Arc::new(src.clone()), "gif", compress, false, None).unwrap();
            assert_eq!(*sent.bytes, src);
            assert_eq!(sent.compressed_size, sent.original_size);
        }
        assert_eq!(*compress_bytes_internal(Arc::new(src.clone()), "gif", None).unwrap().bytes, src);
    }
}
