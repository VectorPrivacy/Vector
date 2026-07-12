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
///                        (orientation normalised, since pixels are baked upright)
/// - full-res + strip  -> re-encode at full resolution (metadata dropped, orientation baked)
/// - full-res + keep   -> ship the original bytes untouched (all metadata + orientation intact)
///
/// GIFs are always shipped as-is to preserve animation.
pub(crate) fn prepare_outbound_image(
    bytes: Arc<Vec<u8>>,
    extension: &str,
    compress: bool,
    keep_metadata: bool,
) -> Result<CachedCompressedImage, String> {
    use crate::shared::image::{
        calculate_resize_dimensions, encode_rgba_auto, reattach_exif_jpeg,
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
    let meta_opt = |b: &[u8]| vector_core::crypto::decode_image_bounded(b).ok()
        .and_then(|img| meta_from(&img));

    // GIF: never re-encode (would drop animation). Metadata is read off the
    // first frame; GIFs don't carry EXIF anyway.
    if extension == "gif" {
        let img_meta = meta_opt(&bytes);
        return Ok(CachedCompressedImage {
            bytes, extension: "gif".to_string(), img_meta,
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
    // can't be stripped in place (non-JPEG, or a JPEG carrying XMP/IPTC).
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

    // Re-encode paths. decode_image_bounded bakes EXIF orientation into pixels.
    let img = vector_core::crypto::decode_image_bounded(&bytes)?;
    let (w, h) = (img.width(), img.height());
    let (nw, nh) = if compress {
        calculate_resize_dimensions(w, h, MAX_DIMENSION)
    } else {
        (w, h)
    };
    let resized = if nw != w || nh != h {
        img.resize(nw, nh, ::image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let (aw, ah) = (resized.width(), resized.height());
    let img_meta = meta_from(&resized);
    let rgba = resized.to_rgba8();
    // Full-resolution sends (compression declined) use higher quality, and keep
    // a PNG source lossless rather than re-encoding a screenshot to JPEG just to
    // strip its metadata. Compression still picks the smaller format by content.
    let (out_bytes, out_ext): (Vec<u8>, &'static str) = if !compress && extension.eq_ignore_ascii_case("png") {
        (crate::shared::image::encode_png(rgba.as_raw(), aw, ah)?, "png")
    } else {
        let quality = if compress { JPEG_QUALITY_STANDARD } else { JPEG_QUALITY_HIGH };
        let encoded = encode_rgba_auto(rgba.as_raw(), aw, ah, quality)?;
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

    // For GIFs, skip compression to preserve animation
    if extension == "gif" {
        let img = ::image::load_from_memory(&bytes)
            .map_err(|e| format!("Failed to decode GIF: {}", e))?;

        let (width, height) = (img.width(), img.height());

        let img_meta = crate::util::generate_thumbhash_from_image(&img)
            .map(|thumbhash| ImageMetadata {
                thumbhash,
                width,
                height,
            });

        return Ok(CachedCompressedImage {
            bytes,
            extension: "gif".to_string(),
            img_meta,
            original_size,
            compressed_size: original_size,
        });
    }

    // Load and decode the image (EXIF orientation baked into pixels)
    let img = vector_core::crypto::decode_image_bounded(&bytes)?;

    // Determine target dimensions (max 1920px on longest side)
    use crate::shared::image::{calculate_resize_dimensions, MAX_DIMENSION};
    let (width, height) = (img.width(), img.height());
    let (new_width, new_height) = calculate_resize_dimensions(width, height, MAX_DIMENSION);

    // Resize if needed
    let resized_img = if new_width != width || new_height != height {
        img.resize(new_width, new_height, ::image::imageops::FilterType::Lanczos3)
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

    let rgba_img = resized_img.to_rgba8();

    // Encode as PNG (alpha/small) or JPEG (standard)
    use crate::shared::image::{encode_rgba_auto, JPEG_QUALITY_STANDARD};
    let encoded = encode_rgba_auto(rgba_img.as_raw(), actual_width, actual_height, JPEG_QUALITY_STANDARD)?;
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
pub(super) fn process_image_for_send(
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
    prepare_outbound_image(original_bytes, extension, use_compression, keep_metadata)
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

        // For GIFs, skip compression entirely to preserve animation
        // Just decode first frame for thumbhash, then return original bytes
        if extension == "gif" {
            // Decode just to get dimensions and generate thumbhash from first frame
            let img = ::image::load_from_memory(&file_data)
                .map_err(|e| format!("Failed to decode GIF: {}", e))?;

            let (width, height) = (img.width(), img.height());

            let img_meta = crate::util::generate_thumbhash_from_image(&img)
                .map(|thumbhash| ImageMetadata {
                    thumbhash,
                    width,
                    height,
                });

            return Ok(CachedCompressedImage {
                bytes: Arc::new(file_data),
                extension: "gif".to_string(),
                img_meta,
                original_size,
                compressed_size: original_size,
            });
        }

        // Try to load and decode the image (EXIF orientation baked into pixels)
        let img = vector_core::crypto::decode_image_bounded(&file_data)?;

        // Determine target dimensions (max 1920px on longest side)
        use crate::shared::image::{calculate_resize_dimensions, MAX_DIMENSION, encode_rgba_auto, JPEG_QUALITY_STANDARD};
        let (width, height) = (img.width(), img.height());
        let (new_width, new_height) = calculate_resize_dimensions(width, height, MAX_DIMENSION);

        // Resize if needed
        let resized_img = if new_width != width || new_height != height {
            img.resize(new_width, new_height, ::image::imageops::FilterType::Lanczos3)
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

        let rgba_img = resized_img.to_rgba8();
        let encoded = encode_rgba_auto(rgba_img.as_raw(), actual_width, actual_height, JPEG_QUALITY_STANDARD)?;
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

        // For GIFs, skip compression entirely to preserve animation
        if extension == "gif" {
            let img = ::image::load_from_memory(&bytes)
                .map_err(|e| format!("Failed to decode GIF: {}", e))?;

            let (width, height) = (img.width(), img.height());

            let img_meta = crate::util::generate_thumbhash_from_image(&img)
                .map(|thumbhash| ImageMetadata {
                    thumbhash,
                    width,
                    height,
                });

            return Ok(CachedCompressedImage {
                bytes,
                extension: "gif".to_string(),
                img_meta,
                original_size,
                compressed_size: original_size,
            });
        }

        // Try to load and decode the image (EXIF orientation baked into pixels)
        let img = vector_core::crypto::decode_image_bounded(&bytes)?;

        // Determine target dimensions (max 1920px on longest side)
        use crate::shared::image::{calculate_resize_dimensions, MAX_DIMENSION, encode_rgba_auto, JPEG_QUALITY_STANDARD};
        let (width, height) = (img.width(), img.height());
        let (new_width, new_height) = calculate_resize_dimensions(width, height, MAX_DIMENSION);

        // Resize if needed
        let resized_img = if new_width != width || new_height != height {
            img.resize(new_width, new_height, ::image::imageops::FilterType::Lanczos3)
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

        let rgba_img = resized_img.to_rgba8();
        let encoded = encode_rgba_auto(rgba_img.as_raw(), actual_width, actual_height, JPEG_QUALITY_STANDARD)?;
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