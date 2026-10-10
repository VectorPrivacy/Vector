//! Emoji pack authoring and the animated-emoji sprite sheets pack grids draw from.
//!
//! Desktop re-encodes WebP through libwebp; the browser build has no WebP
//! encoder, so cropped stills come out PNG and cropped animations GIF.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::io::Cursor;
use std::path::PathBuf;
use std::pin::Pin;

use image::{AnimationDecoder, DynamicImage, Frame, RgbaImage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use vector_core::emoji_packs::{self, EmojiPack, PackEmoji};

use crate::commands::{to_value, Args};
use crate::images::Kind;

const MAX_EMOJI_BYTES: usize = 256 * 1024;
const MIN_EMOJI_DIM: u32 = 48;
const MAX_UPLOAD_DIM: u32 = 256;
const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAX_DECODE_BYTES: usize = 1024 * 1024;
const FRAME_SIZE: u32 = 56;
const MAX_SHEET_FRAMES: usize = 256;

pub fn dispatch<'a>(cmd: &'a str, a: &'a Args) -> Pin<Box<dyn Future<Output = Option<Result<Value, String>>> + 'a>> {
    Box::pin(async move {
        Some(match cmd {
            "fetch_emoji_pack_by_naddr" => emoji_packs::fetch_pack_by_naddr(&opt!(a.str("naddr"))).await.and_then(to_value),
            "unsubscribe_emoji_pack" => emoji_packs::unsubscribe_pack(&opt!(a.str("id"))).await.map(|_| Value::Null),
            "reorder_emoji_packs" => {
                emoji_packs::reorder_emoji_packs(opt!(a.de::<Vec<String>>("orderedIds"))).map(|_| Value::Null)
            }
            "emoji_pack_create" => create(opt!(a.de::<CreateInput>("input"))).await,
            "emoji_pack_delete" => {
                let id = opt!(a.str("id"));
                vector_core::db::scoped(async move { emoji_packs::delete_own_pack(&id).await }).await.map(|_| Value::Null)
            }
            "emoji_pack_delete_blob" => delete_blob(opt!(a.str("url"))).await,
            "emoji_pack_upload_image" => upload(opt!(a.str("bytesB64")), a.opt_str("kind")).await,
            "import_image_for_emoji" => import(&opt!(a.str("source"))).await,
            "emoji_crop_and_reencode" => crop_command(a),
            "decode_animated_emoji" => decode_animated(opt!(a.str("url"))).await.and_then(to_value),
            "cached_emoji_sheets" => cached_sheets(opt!(a.de::<Vec<String>>("urls"))).await,
            _ => return None,
        })
    })
}

/// An argument error is the command's result, not "not mine".
macro_rules! opt {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        }
    };
}
use opt;

// --- Packs -------------------------------------------------------------------

#[derive(Deserialize)]
struct EmojiInput {
    shortcode: String,
    url: String,
}

#[derive(Deserialize)]
struct CreateInput {
    identifier: Option<String>,
    title: String,
    image_url: Option<String>,
    description: Option<String>,
    emojis: Vec<EmojiInput>,
}

fn new_identifier() -> String {
    use rand::distributions::Alphanumeric;
    use rand::Rng;
    rand::thread_rng().sample_iter(&Alphanumeric).take(12).map(char::from).collect()
}

async fn create(input: CreateInput) -> Result<Value, String> {
    vector_core::db::scoped(async move {
        let identifier = input.identifier.filter(|s| !s.trim().is_empty()).unwrap_or_else(new_identifier);
        let emojis: Vec<PackEmoji> = input
            .emojis
            .into_iter()
            .filter(|e| !e.shortcode.trim().is_empty() && !e.url.trim().is_empty())
            .map(|e| PackEmoji { shortcode: e.shortcode.trim().into(), url: e.url.trim().into(), sha256: None })
            .collect();
        if emojis.is_empty() {
            return Err("A pack needs at least one emoji.".to_string());
        }
        let pack = EmojiPack {
            id: String::new(),
            pubkey: String::new(),
            identifier,
            title: input.title.trim().into(),
            image_url: input.image_url.unwrap_or_default().trim().into(),
            description: input.description.unwrap_or_default().trim().into(),
            emojis,
            is_own: true,
            updated_at: 0,
            status: emoji_packs::PACK_STATUS_ACTIVE,
        };
        emoji_packs::publish_pack(&pack).await.and_then(to_value)
    })
    .await
}

async fn delete_blob(url: String) -> Result<Value, String> {
    vector_core::state::nostr_client().ok_or("Nostr client not initialised")?;
    let signer = vector_core::signer::active_signer().map_err(|e| format!("Failed to get signer: {e}"))?;
    vector_core::blossom::delete_blob_by_url(signer, &url).await.map(|_| Value::Null)
}

fn b64(bytes: &[u8]) -> String {
    base64_simd::STANDARD.encode_to_string(bytes)
}

fn unb64(s: &str) -> Result<Vec<u8>, String> {
    base64_simd::STANDARD.decode_to_vec(s).map_err(|e| format!("bytes base64: {e}"))
}

fn is_animated(bytes: &[u8]) -> bool {
    match vector_core::crypto::mime_from_magic_bytes(bytes) {
        "image/gif" => frames(bytes).is_ok_and(|f| f.len() > 1),
        "image/webp" => image::codecs::webp::WebPDecoder::new(Cursor::new(bytes)).is_ok_and(|d| d.has_animation()),
        "image/png" => is_apng(bytes),
        _ => false,
    }
}

/// Strip metadata and fit the upload budget; animations pass untouched when they already fit.
fn prepare_upload(bytes: &[u8]) -> Result<(Vec<u8>, &'static str), String> {
    if is_animated(bytes) {
        if bytes.len() > MAX_EMOJI_BYTES {
            return Err(format!(
                "Animated image is too large ({} KB, max {} KB); please use a smaller one or a static image",
                bytes.len() / 1024,
                MAX_EMOJI_BYTES / 1024
            ));
        }
        let mime = match vector_core::crypto::mime_from_magic_bytes(bytes) {
            "image/webp" => "image/webp",
            "image/png" => "image/apng",
            _ => "image/gif",
        };
        return Ok((bytes.to_vec(), mime));
    }
    let img = crate::files::decode_upright(bytes).map_err(|_| "Image couldn't be read (unsupported or corrupt file)".to_string())?;
    let img = if img.width() > MAX_UPLOAD_DIM || img.height() > MAX_UPLOAD_DIM {
        img.resize(MAX_UPLOAD_DIM, MAX_UPLOAD_DIM, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let out = fit_budget(&[(rgba, 0)], Some(MAX_EMOJI_BYTES), |f| encode_png(&f[0].0))?;
    Ok((out, "image/png"))
}

async fn upload(bytes_b64: String, kind: Option<String>) -> Result<Value, String> {
    vector_core::db::scoped(async move {
        let bytes = unb64(&bytes_b64)?;
        if bytes.is_empty() {
            return Err("File is empty.".to_string());
        }
        if bytes.len() > MAX_EMOJI_BYTES {
            return Err(format!("File is {} KB, max is {} KB.", bytes.len() / 1024, MAX_EMOJI_BYTES / 1024));
        }
        vector_core::state::nostr_client().ok_or("Nostr client not initialised")?;
        let signer = vector_core::signer::active_signer().map_err(|e| format!("Failed to get signer: {e}"))?;
        let servers = vector_core::blossom_servers::compute_enabled_servers();
        if servers.is_empty() {
            return Err("No Blossom servers configured.".to_string());
        }
        let (prepared, mime) = prepare_upload(&bytes)?;
        let prepared = std::sync::Arc::new(prepared);
        let url = vector_core::blossom::upload_blob_with_failover(
            signer,
            servers,
            prepared.clone(),
            Some(mime),
            Some(std::time::Duration::from_secs(10)),
        )
        .await?;
        let kind = if kind.as_deref() == Some("emoji_pack_icon") { Kind::EmojiPackIcon } else { Kind::Emoji };
        crate::images::precache(&url, kind, &prepared).await;
        Ok(json!(url))
    })
    .await
}

/// A picked image as base64 for the creator's cropper; formats the page can't show become PNG.
async fn import(source: &str) -> Result<Value, String> {
    let bytes = vector_core::webfiles::read(std::path::Path::new(source)).await?;
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err("Image is too large.".into());
    }
    if matches!(vector_core::crypto::mime_from_magic_bytes(&bytes), "image/png" | "image/jpeg" | "image/gif" | "image/webp") {
        return Ok(json!(b64(&bytes)));
    }
    let img = vector_core::crypto::decode_image_bounded(&bytes)
        .map_err(|_| "Image couldn't be read (unsupported or corrupt file)".to_string())?;
    Ok(json!(b64(&encode_png(&img.to_rgba8())?)))
}

// --- Cropping ----------------------------------------------------------------

fn crop_command(a: &Args) -> Result<Value, String> {
    let bytes = unb64(&a.str("sourceB64")?)?;
    let (x, y, w, h) = (a.de::<u32>("x")?, a.de::<u32>("y")?, a.de::<u32>("w")?, a.de::<u32>("h")?);
    if w != h {
        return Err("crop must be square".into());
    }
    if w == 0 {
        return Err("crop must be non-empty".into());
    }
    if bytes.len() > MAX_EMOJI_BYTES * 4 {
        return Err("source too large".into());
    }
    Ok(json!(b64(&crop(&bytes, &a.opt_str("mime").unwrap_or_default().to_lowercase(), x, y, w, h, Some(MAX_EMOJI_BYTES))?)))
}

/// Crop any image to a source-pixel rectangle at full resolution, animation kept. The caller
/// sizes it afterwards (community images go through `prepare_image`).
pub(crate) fn crop_image(bytes: &[u8], x: u32, y: u32, w: u32, h: u32) -> Result<Vec<u8>, String> {
    if w == 0 || h == 0 {
        return Err("crop must be non-empty".into());
    }
    crop(bytes, vector_core::crypto::mime_from_magic_bytes(bytes), x, y, w, h, None)
}

/// A crop drawn on whatever copy the page showed (maybe a scaled preview), made on the image itself.
pub(crate) fn crop_drawn(bytes: &[u8], crop: vector_core::image_crop::CropRect) -> Result<Vec<u8>, String> {
    let dims = vector_core::image_crop::upright_dims(bytes).ok_or("Couldn't read the image")?;
    let (x, y, w, h) = vector_core::image_crop::map_crop(crop, dims);
    crop_image(bytes, x, y, w, h)
}

fn orientation(bytes: &[u8]) -> Option<image::metadata::Orientation> {
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_decoder().ok()?;
    decoder.orientation().ok()
}

/// `max_bytes`: shrink until the output fits, or `None` to keep the native size.
fn crop(bytes: &[u8], mime: &str, x: u32, y: u32, w: u32, h: u32, max_bytes: Option<usize>) -> Result<Vec<u8>, String> {
    let bounds = |fw: u32, fh: u32| {
        if x.saturating_add(w) > fw || y.saturating_add(h) > fh {
            Err("crop outside source bounds".to_string())
        } else {
            Ok(())
        }
    };
    if is_animated(bytes) {
        let decoded = frames(bytes)?;
        let first = decoded.first().ok_or("no frames decoded")?;
        bounds(first.0.width(), first.0.height())?;
        let cropped: Vec<(RgbaImage, u32)> =
            decoded.into_iter().map(|(img, d)| (image::imageops::crop_imm(&img, x, y, w, h).to_image(), d.max(20))).collect();
        return fit_budget(&cropped, max_bytes, encode_gif);
    }
    let mut img = vector_core::crypto::decode_image_bounded(bytes).map_err(|e| format!("decode: {e}"))?;
    // Upright, as the cropper showed it: the page applies EXIF orientation.
    if let Some(o) = orientation(bytes) {
        img.apply_orientation(o);
    }
    bounds(img.width(), img.height())?;
    let cropped = img.crop_imm(x, y, w, h).to_rgba8();
    let jpeg = mime.contains("jpeg") || mime.contains("jpg");
    fit_budget(&[(cropped, 0)], max_bytes, |f| if jpeg { encode_jpeg(&f[0].0) } else { encode_png(&f[0].0) })
}

/// Encode, then step the size down until it fits `max_bytes` or the minimum edge.
fn fit_budget(frames: &[(RgbaImage, u32)], max_bytes: Option<usize>, encode: impl Fn(&[(RgbaImage, u32)]) -> Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
    let out = encode(frames)?;
    let src_dim = frames.first().map(|(img, _)| img.width().max(img.height())).unwrap_or(0);
    let Some(max_bytes) = max_bytes else { return Ok(out) };
    if out.len() <= max_bytes || src_dim <= MIN_EMOJI_DIM {
        return Ok(out);
    }
    let mut dim = src_dim;
    loop {
        dim = ((dim * 88) / 100).max(MIN_EMOJI_DIM);
        let scaled: Vec<(RgbaImage, u32)> = frames
            .iter()
            .map(|(img, d)| {
                let (w, h) = (img.width(), img.height());
                let (nw, nh) = if w >= h { (dim, (h * dim / w.max(1)).max(1)) } else { ((w * dim / h.max(1)).max(1), dim) };
                (image::imageops::resize(img, nw, nh, image::imageops::FilterType::Lanczos3), *d)
            })
            .collect();
        let out = encode(&scaled)?;
        if out.len() <= max_bytes || dim <= MIN_EMOJI_DIM {
            return Ok(out);
        }
    }
}

fn encode_png(rgba: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    DynamicImage::ImageRgba8(rgba.clone())
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| format!("png encode: {e}"))?;
    Ok(out)
}

fn encode_jpeg(rgba: &RgbaImage) -> Result<Vec<u8>, String> {
    let rgb = image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y).0;
        let (a, inv) = (p[3] as u32, 255 - p[3] as u32);
        image::Rgb([0, 1, 2].map(|c| ((p[c] as u32 * a + 255 * inv) / 255) as u8))
    });
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88).encode_image(&rgb).map_err(|e| format!("jpeg encode: {e}"))?;
    Ok(out)
}

fn encode_gif(frames: &[(RgbaImage, u32)]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new_with_speed(&mut out, 10);
        encoder.set_repeat(image::codecs::gif::Repeat::Infinite).map_err(|e| format!("gif repeat: {e}"))?;
        for (img, ms) in frames {
            let delay = image::Delay::from_numer_denom_ms((*ms).max(20), 1);
            encoder.encode_frame(Frame::from_parts(img.clone(), 0, 0, delay)).map_err(|e| format!("gif frame: {e}"))?;
        }
    }
    Ok(out)
}

// --- Frames and sprite sheets -----------------------------------------------

fn is_apng(bytes: &[u8]) -> bool {
    image::codecs::png::PngDecoder::new(Cursor::new(bytes)).is_ok_and(|d| d.is_apng().unwrap_or(false))
}

fn collect(frames: image::Frames<'_>) -> Result<Vec<(RgbaImage, u32)>, String> {
    let mut out = Vec::new();
    for frame in frames.take(MAX_SHEET_FRAMES) {
        let frame = frame.map_err(|e| format!("decode: {e}"))?;
        let (n, d) = frame.delay().numer_denom_ms();
        out.push((frame.into_buffer(), (n / d.max(1)).max(20)));
    }
    Ok(out)
}

/// Every frame, composited to full canvas, with its delay; a still is one frame.
fn frames(bytes: &[u8]) -> Result<Vec<(RgbaImage, u32)>, String> {
    let cursor = || Cursor::new(bytes);
    match vector_core::crypto::mime_from_magic_bytes(bytes) {
        "image/gif" => {
            let d = image::codecs::gif::GifDecoder::new(cursor()).map_err(|e| format!("decode: {e}"))?;
            return collect(d.into_frames());
        }
        "image/webp" => {
            let d = image::codecs::webp::WebPDecoder::new(cursor()).map_err(|e| format!("decode: {e}"))?;
            if d.has_animation() {
                return collect(d.into_frames());
            }
        }
        "image/png" if is_apng(bytes) => {
            let d = image::codecs::png::PngDecoder::new(cursor()).map_err(|e| format!("decode: {e}"))?;
            return collect(d.apng().map_err(|e| format!("decode: {e}"))?.into_frames());
        }
        _ => {}
    }
    let img = vector_core::crypto::decode_image_bounded(bytes).map_err(|e| format!("decode: {e}"))?;
    Ok(vec![(img.to_rgba8(), 100)])
}

#[derive(Serialize, Deserialize, Clone)]
pub struct EmojiSheet {
    path: String,
    x: u32,
    y: u32,
    frame_count: u32,
    frame_size: u32,
    frame_durations_ms: Vec<u32>,
}

thread_local! {
    static SHEETS: RefCell<HashMap<String, EmojiSheet>> = RefCell::new(HashMap::new());
}

fn sheet_paths(url: &str) -> (PathBuf, PathBuf) {
    let key = &vector_core::crypto::sha256_hex(url.as_bytes())[..32];
    (PathBuf::from(format!("/cache/emoji_sheets/{key}.png")), PathBuf::from(format!("/cache/emoji_sheets/{key}.json")))
}

async fn stored_sheet(url: &str) -> Option<EmojiSheet> {
    if let Some(s) = SHEETS.with(|m| m.borrow().get(url).cloned()) {
        return Some(s);
    }
    let meta = vector_core::webfiles::read(&sheet_paths(url).1).await.ok()?;
    let sheet: EmojiSheet = serde_json::from_slice(&meta).ok()?;
    SHEETS.with(|m| m.borrow_mut().insert(url.to_string(), sheet.clone()));
    Some(sheet)
}

async fn cached_sheets(urls: Vec<String>) -> Result<Value, String> {
    let mut out = Vec::with_capacity(urls.len());
    for url in &urls {
        out.push(stored_sheet(url).await);
    }
    to_value(out)
}

/// One 56px column per emoji: frame i sits at y = i * 56.
async fn decode_animated(url: String) -> Result<EmojiSheet, String> {
    if let Some(sheet) = stored_sheet(&url).await {
        return Ok(sheet);
    }
    vector_core::net::validate_url_not_private(&url).map_err(|e| e.to_string())?;
    let bytes = crate::images::fetch(&url, MAX_DECODE_BYTES).await?;
    let decoded = frames(&bytes)?;
    if decoded.is_empty() {
        return Err("no frames decoded".into());
    }
    let mut sheet = RgbaImage::new(FRAME_SIZE, FRAME_SIZE * decoded.len() as u32);
    let mut durations = Vec::with_capacity(decoded.len());
    for (i, (frame, ms)) in decoded.iter().enumerate() {
        let resized = image::imageops::resize(frame, FRAME_SIZE, FRAME_SIZE, image::imageops::FilterType::Triangle);
        image::imageops::overlay(&mut sheet, &resized, 0, (i as u32 * FRAME_SIZE) as i64);
        durations.push(*ms);
    }
    let (png_path, meta_path) = sheet_paths(&url);
    vector_core::webfiles::write(&png_path, &encode_png(&sheet)?).await?;
    let result = EmojiSheet {
        path: png_path.to_string_lossy().into_owned(),
        x: 0,
        y: 0,
        frame_count: decoded.len() as u32,
        frame_size: FRAME_SIZE,
        frame_durations_ms: durations,
    };
    vector_core::webfiles::write(&meta_path, &serde_json::to_vec(&result).map_err(|e| e.to_string())?).await?;
    SHEETS.with(|m| m.borrow_mut().insert(url, result.clone()));
    Ok(result)
}
