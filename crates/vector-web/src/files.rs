//! Outbound files. Picked and dropped files are already in OPFS (the page stores
//! them); pasted bytes wait in memory until sent or cleared.
//!
//! Images go through the same privacy matrix as desktop: full resolution with
//! metadata ships untouched; stripping re-encodes with orientation baked in;
//! compression resizes to 1920px. GIFs always ship as-is to keep animation.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use image::{DynamicImage, ImageDecoder, ImageReader};
use serde_json::{json, Value};
use vector_core::types::ImageMetadata;

const MAX_DIMENSION: u32 = 1920;
const JPEG_QUALITY_STANDARD: u8 = 85;
const JPEG_QUALITY_HIGH: u8 = 95;
const IMAGE_EXTENSIONS: [&str; 8] = ["png", "jpg", "jpeg", "gif", "webp", "tiff", "tif", "ico"];

#[derive(Clone)]
struct Prepared {
    bytes: Arc<Vec<u8>>,
    extension: String,
    img_meta: Option<ImageMetadata>,
    original_size: u64,
}

struct Pasted {
    bytes: Arc<Vec<u8>>,
    name: String,
    extension: String,
}

thread_local! {
    static PASTED: RefCell<Option<Pasted>> = const { RefCell::new(None) };
    /// Pre-compressed images by source ("" = the pasted bytes). `None` while in flight.
    static PRECOMPRESSED: RefCell<HashMap<String, Option<Prepared>>> = RefCell::new(HashMap::new());
}

fn is_image(ext: &str) -> bool {
    IMAGE_EXTENSIONS.contains(&ext)
}

fn name_and_ext(path: &str) -> (String, String) {
    let name = path.rsplit('/').next().unwrap_or("").to_string();
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    (name, ext)
}

fn sanitized_name(name_override: &str, fallback: String) -> String {
    let s = vector_core::crypto::sanitize_filename(name_override);
    if s.is_empty() { fallback } else { s }
}

async fn read(path: &str) -> Result<Vec<u8>, String> {
    let bytes = vector_core::webfiles::read(Path::new(path)).await?;
    if bytes.is_empty() {
        return Err(format!("File is empty (0 bytes): {path}"));
    }
    Ok(bytes)
}

/// Decode with EXIF orientation applied, so re-encoded pixels come out upright.
pub(crate) fn decode_upright(bytes: &[u8]) -> Result<DynamicImage, String> {
    let mut decoder = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| format!("Failed to decode image: {e}"))?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| format!("Failed to decode image: {e}"))?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(img)
}

pub(crate) fn encode(img: &DynamicImage, keep_png: bool, quality: u8) -> Result<(Vec<u8>, &'static str), String> {
    let mut out = Vec::new();
    let transparent = img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p.0[3] < 255);
    if keep_png || transparent {
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).map_err(|e| e.to_string())?;
        return Ok((out, "png"));
    }
    let rgb = img.to_rgb8();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&rgb)
        .map_err(|e| e.to_string())?;
    Ok((out, "jpg"))
}

fn prepare_image(bytes: Arc<Vec<u8>>, extension: &str, compress: bool, keep_metadata: bool) -> Result<Prepared, String> {
    let original_size = bytes.len() as u64;
    if extension == "gif" || (keep_metadata && !compress) {
        let img_meta = vector_core::crypto::generate_image_metadata(&bytes);
        return Ok(Prepared { bytes, extension: extension.to_string(), img_meta, original_size });
    }
    let img = decode_upright(&bytes)?;
    let img = if compress && (img.width() > MAX_DIMENSION || img.height() > MAX_DIMENSION) {
        img.resize(MAX_DIMENSION, MAX_DIMENSION, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let keep_png = !compress && extension == "png";
    let (out, ext) = encode(&img, keep_png, if compress { JPEG_QUALITY_STANDARD } else { JPEG_QUALITY_HIGH })?;
    let img_meta = vector_core::crypto::generate_image_metadata(&out);
    Ok(Prepared { bytes: Arc::new(out), extension: ext.to_string(), img_meta, original_size })
}

fn estimate(p: &Prepared) -> Value {
    let size = p.bytes.len() as u64;
    let savings = if p.original_size > 0 && size < p.original_size {
        ((p.original_size - size) * 100 / p.original_size) as u32
    } else {
        0
    };
    json!({ "original_size": p.original_size, "estimated_size": size, "savings_percent": savings })
}

async fn send(receiver: String, replied_to: String, bytes: Arc<Vec<u8>>, name: String, extension: String, img_meta: Option<ImageMetadata>) -> Result<Value, String> {
    if receiver.starts_with("npub1") {
        return crate::messaging::send_file(receiver, replied_to, bytes, name, extension, img_meta).await;
    }
    crate::community::send_community_file(receiver, replied_to, bytes, name, extension, img_meta).await
}

pub async fn file_info(path: &str) -> Result<Value, String> {
    let (name, extension) = name_and_ext(path);
    let size = vector_core::webfiles::size(Path::new(path)).await.ok_or("File not found")?;
    Ok(json!({ "size": size, "name": name, "extension": extension }))
}

pub async fn start_precompression(key: String) -> Result<(), String> {
    PRECOMPRESSED.with(|m| m.borrow_mut().insert(key.clone(), None));
    let (bytes, ext) = if key.is_empty() {
        PASTED.with(|p| p.borrow().as_ref().map(|p| (p.bytes.clone(), p.extension.clone()))).ok_or("No cached file")?
    } else {
        (Arc::new(read(&key).await?), name_and_ext(&key).1)
    };
    // Yield first so the preview paints before the encode takes the thread.
    vector_core::rt::yield_now().await;
    match prepare_image(bytes, &ext, true, false) {
        Ok(p) => {
            PRECOMPRESSED.with(|m| m.borrow_mut().insert(key, Some(p)));
            Ok(())
        }
        Err(e) => {
            PRECOMPRESSED.with(|m| m.borrow_mut().remove(&key));
            Err(e)
        }
    }
}

pub fn compression_status(key: &str) -> Result<Value, String> {
    PRECOMPRESSED.with(|m| match m.borrow().get(key) {
        Some(Some(p)) => Ok(estimate(p)),
        Some(None) => Ok(Value::Null),
        None => Err("No compression in progress".into()),
    })
}

fn take_precompressed(key: &str) -> Option<Prepared> {
    PRECOMPRESSED.with(|m| m.borrow_mut().remove(key).flatten())
}

pub fn clear_compression(key: &str) {
    PRECOMPRESSED.with(|m| m.borrow_mut().remove(key));
}

/// Full-resolution send of a picked file.
pub async fn file_message(receiver: String, replied_to: String, path: String, keep_metadata: bool, name_override: String) -> Result<Value, String> {
    let (name, ext) = name_and_ext(&path);
    let bytes = Arc::new(read(&path).await?);
    clear_compression(&path);
    let name = sanitized_name(&name_override, name);
    if !is_image(&ext) {
        let sent = send(receiver, replied_to, bytes, name, ext, None).await?;
        forget_picked(&path).await;
        return Ok(sent);
    }
    let p = prepare_image(bytes, &ext, false, keep_metadata)?;
    let sent = send(receiver, replied_to, p.bytes, name, p.extension, p.img_meta).await?;
    forget_picked(&path).await;
    Ok(sent)
}

/// Compressed send of a picked image: the preview's pre-compression when it matches.
pub async fn send_compressed(receiver: String, replied_to: String, path: String, keep_metadata: bool, name_override: String) -> Result<Value, String> {
    let (name, ext) = name_and_ext(&path);
    let name = sanitized_name(&name_override, name);
    let p = match take_precompressed(&path).filter(|_| !keep_metadata) {
        Some(p) => p,
        None => prepare_image(Arc::new(read(&path).await?), &ext, true, keep_metadata)?,
    };
    let sent = send(receiver, replied_to, p.bytes, name, p.extension, p.img_meta).await?;
    forget_picked(&path).await;
    Ok(sent)
}

pub fn cache_bytes(bytes: Vec<u8>, name: String, extension: String) -> Value {
    let size = bytes.len();
    PASTED.with(|p| *p.borrow_mut() = Some(Pasted { bytes: Arc::new(bytes), name: name.clone(), extension: extension.clone() }));
    json!({ "size": size, "name": name, "extension": extension })
}

/// The pasted file's bytes and name, for previews that read it in place.
pub fn pasted_bytes() -> Option<(Arc<Vec<u8>>, String)> {
    PASTED.with(|p| p.borrow().as_ref().map(|p| (p.bytes.clone(), p.name.clone())))
}

pub fn cached_info() -> Value {
    PASTED.with(|p| {
        p.borrow()
            .as_ref()
            .map(|p| json!({ "size": p.bytes.len(), "name": p.name, "extension": p.extension }))
            .unwrap_or(Value::Null)
    })
}

pub fn clear_cached() {
    PASTED.with(|p| *p.borrow_mut() = None);
    clear_compression("");
}

/// The pasted image as a file the page can show.
pub async fn preview_cached() -> Result<Value, String> {
    let (bytes, ext) = PASTED.with(|p| p.borrow().as_ref().map(|p| (p.bytes.clone(), p.extension.clone()))).ok_or("No cached file")?;
    if vector_core::svg::looks_like_svg(&bytes) {
        return svg_preview(&bytes).await;
    }
    let hash = vector_core::crypto::sha256_hex(&bytes);
    let path = format!("/cache/previews/{hash}.{}", if ext.is_empty() { "png" } else { &ext });
    vector_core::webfiles::write(Path::new(&path), &bytes).await?;
    Ok(json!(path))
}

/// An SVG attachment drawn for display (the file itself stays as received); null when it refuses.
pub async fn render_svg(path: &str, max_dim: u32) -> Result<Value, String> {
    let bytes = read(path).await?;
    if bytes.len() > vector_core::svg::MAX_SVG_BYTES || !vector_core::svg::looks_like_svg(&bytes) {
        return Ok(Value::Null);
    }
    let dim = max_dim.clamp(256, vector_core::svg::MAX_RENDER_DIM).div_ceil(256) * 256;
    let out = format!("/cache/svg_renders/{}-{dim}.png", &vector_core::crypto::sha256_hex(&bytes)[..32]);
    if vector_core::webfiles::size(Path::new(&out)).await.is_some() {
        return Ok(json!(out));
    }
    let Ok(png) = vector_core::svg::rasterize_png(&bytes, dim) else { return Ok(Value::Null) };
    vector_core::webfiles::write(Path::new(&out), &png).await?;
    Ok(json!(out))
}

/// An SVG's preview is the pixels it renders to: its markup never reaches the page.
async fn svg_preview(bytes: &[u8]) -> Result<Value, String> {
    const PREVIEW_MAX_DIM: u32 = 1024;
    let png = vector_core::svg::rasterize_png(bytes, PREVIEW_MAX_DIM)?;
    let path = format!("/cache/previews/{}.png", vector_core::crypto::sha256_hex(bytes));
    vector_core::webfiles::write(Path::new(&path), &png).await?;
    Ok(json!(path))
}

pub async fn send_cached(receiver: String, replied_to: String, use_compression: bool, keep_metadata: bool, name_override: String) -> Result<Value, String> {
    let pasted = PASTED.with(|p| p.borrow_mut().take()).ok_or("No cached file")?;
    let pre = take_precompressed("");
    let name = sanitized_name(&name_override, pasted.name.clone());
    if !is_image(&pasted.extension) {
        return send(receiver, replied_to, pasted.bytes, name, pasted.extension, None).await;
    }
    let p = match pre.filter(|_| use_compression && !keep_metadata) {
        Some(p) => p,
        None => prepare_image(pasted.bytes, &pasted.extension, use_compression, keep_metadata)?,
    };
    send(receiver, replied_to, p.bytes, name, p.extension, p.img_meta).await
}

/// A picked image's blurred placeholder, as a data URL.
pub async fn thumbhash_preview(path: &str) -> Result<Value, String> {
    let bytes = if path.is_empty() {
        PASTED.with(|p| p.borrow().as_ref().map(|p| p.bytes.clone())).ok_or("No cached file and no file path provided")?
    } else {
        Arc::new(read(path).await?)
    };
    let meta = vector_core::crypto::generate_image_metadata(&bytes).ok_or("Failed to generate thumbhash")?;
    Ok(json!(thumbhash_data_url(&meta.thumbhash).ok_or("Failed to decode thumbhash")?))
}

/// A thumbhash as a PNG data URL; None when it does not decode, so a caller falls back to
/// its file box instead of sizing an empty image.
pub fn thumbhash_data_url(thumbhash: &str) -> Option<String> {
    let hash = fast_thumbhash::base91_decode(thumbhash).ok()?;
    let (w, h, rgba) = fast_thumbhash::thumb_hash_to_rgba(&hash).ok()?;
    let img = image::RgbaImage::from_raw(w as u32, h as u32, rgba)?;
    let mut png = Vec::new();
    DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some(format!("data:image/png;base64,{}", base64_simd::STANDARD.encode_to_string(&png)))
}

/// Whether a picked image carries metadata worth offering to strip.
pub async fn has_metadata(path: &str) -> bool {
    let (_, ext) = name_and_ext(path);
    let Ok(bytes) = read(path).await else { return false };
    let head = &bytes[..bytes.len().min(256 * 1024)];
    let has = |needle: &[u8]| head.windows(needle.len()).any(|w| w == needle);
    match ext.as_str() {
        "jpg" | "jpeg" => has(b"Exif\0\0") || has(b"http://ns.adobe.com/xap"),
        "png" => has(b"eXIf") || has(b"tEXt") || has(b"iTXt") || has(b"zTXt"),
        "webp" => has(b"EXIF") || has(b"XMP "),
        "tif" | "tiff" => true,
        _ => false,
    }
}

/// A picked file to preview inline: itself, if it really is an image.
pub async fn image_preview(path: &str) -> Result<Value, String> {
    let bytes = read(path).await?;
    if vector_core::svg::looks_like_svg(&bytes) {
        return svg_preview(&bytes).await;
    }
    let mime = vector_core::crypto::mime_from_magic_bytes(&bytes);
    if !matches!(mime, "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/tiff" | "image/x-icon" | "image/bmp") {
        return Err("not an image".into());
    }
    Ok(json!(path))
}

/// A recorded voice message: a nameless WAV, which receivers render as a voice player.
pub async fn send_voice(receiver: String, replied_to: String, path: String) -> Result<Value, String> {
    let bytes = std::sync::Arc::new(read(&path).await?);
    let sent = send(receiver, replied_to, bytes, String::new(), "wav".into(), None).await?;
    forget_picked(&path).await;
    Ok(sent)
}

/// The page's copy of a picked file, once sent: the attachment lives on under its hash.
async fn forget_picked(path: &str) {
    if path.starts_with("/picked/") {
        vector_core::webfiles::remove(Path::new(path)).await;
    }
}
