//! Images made ready to share: decoded, turned upright, fitted to a size and a byte budget, and
//! re-encoded. Re-encoding drops every byte of metadata the original carried, camera EXIF and
//! GPS included. Animated GIF and WebP keep their animation: they pass through when they
//! already fit, keeping only the blocks that draw them. An animated PNG is shared as its first
//! frame. [`strip`] does the same without re-encoding, for an image too large to.

use std::io::Cursor;

use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};

/// What the image is for, which sets its size and byte budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageUse {
    /// A profile picture or Community icon, shown small.
    Icon,
    /// A profile or Community banner, shown wide.
    Banner,
    /// A picture sent in a chat.
    Photo,
}

impl ImageUse {
    /// `(longest side px, static byte budget, animated byte budget)`.
    const fn budgets(self) -> (u32, usize, usize) {
        match self {
            ImageUse::Icon => (512, 256 * 1024, 2 * 1024 * 1024),
            ImageUse::Banner => (1500, 600 * 1024, 3 * 1024 * 1024),
            ImageUse::Photo => (2560, 1536 * 1024, 8 * 1024 * 1024),
        }
    }
}

/// A prepared image and the extension its bytes are.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
}

/// The MIME type for a [`Prepared::extension`].
pub fn mime_for(extension: &str) -> &'static str {
    match extension {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/jpeg",
    }
}

fn limits() -> Limits {
    let mut l = Limits::default();
    l.max_image_width = Some(16_384);
    l.max_image_height = Some(16_384);
    l.max_alloc = Some(256 * 1024 * 1024);
    l
}

/// The most pixels decoded to re-encode: past it a picture is stripped, not decoded.
const MAX_PIXELS: u64 = 50_000_000;

/// The size a header declares, read without decoding a pixel.
fn header_dims(bytes: &[u8], format: ImageFormat) -> Result<(u32, u32), String> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits());
    reader.into_dimensions().map_err(|e| format!("read image: {e}"))
}

/// The most pixels a GIF's canvas may declare before a frame is decoded: every frame is
/// composed onto a buffer that size.
const MAX_GIF_CANVAS: u64 = 4096 * 4096;

/// Animated GIF or WebP dimensions, `None` for a still image.
fn animation(bytes: &[u8]) -> Result<Option<(u32, u32, &'static str)>, String> {
    match crate::crypto::animated_format(bytes) {
        Some("gif") => {
            let (w, h) = header_dims(bytes, ImageFormat::Gif)?;
            if w as u64 * h as u64 > MAX_GIF_CANVAS {
                return Err(format!("this GIF declares a {w}x{h} canvas; at most 4096x4096"));
            }
            let mut d = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).map_err(|e| format!("read image: {e}"))?;
            d.set_limits(limits()).map_err(|e| format!("read image: {e}"))?;
            let frames = image::AnimationDecoder::into_frames(d).take(2).count();
            Ok((frames > 1).then_some((w, h, "gif")))
        }
        Some("webp") => {
            let (w, h) = header_dims(bytes, ImageFormat::WebP)?;
            Ok(Some((w, h, "webp")))
        }
        _ => Ok(None),
    }
}

/// A GIF keeping only what draws it: frames, their timing, and the looping block. Comments,
/// text and every other application block go, which is where an editor writes XMP and other
/// metadata. `None` for a GIF that doesn't parse.
fn strip_gif(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len());
    let screen = bytes.get(..13)?;
    out.extend_from_slice(screen);
    let mut i = 13;
    if screen[10] & 0x80 != 0 {
        let table = 3 << ((screen[10] & 0x07) + 1);
        out.extend_from_slice(bytes.get(i..i + table)?);
        i += table;
    }
    // A run of data sub-blocks ending in a zero-length one, from `at`; returns where it ends.
    let sub_blocks = |mut at: usize| -> Option<usize> {
        loop {
            let len = *bytes.get(at)? as usize;
            at += 1 + len;
            if len == 0 {
                return Some(at);
            }
            bytes.get(..at)?;
        }
    };
    loop {
        match *bytes.get(i)? {
            0x3B => {
                out.push(0x3B);
                return Some(out);
            }
            0x21 => {
                let label = *bytes.get(i + 1)?;
                let end = sub_blocks(i + 2)?;
                let keep = match label {
                    0xF9 => true,
                    0xFF => matches!(bytes.get(i + 3..i + 14), Some(b"NETSCAPE2.0") | Some(b"ANIMEXTS1.0")),
                    _ => false,
                };
                if keep {
                    out.extend_from_slice(bytes.get(i..end)?);
                }
                i = end;
            }
            0x2C => {
                let desc = bytes.get(i..i + 10)?;
                let mut end = i + 10;
                if desc[9] & 0x80 != 0 {
                    end += 3 << ((desc[9] & 0x07) + 1);
                }
                end = sub_blocks(end + 1)?;
                out.extend_from_slice(bytes.get(i..end)?);
                i = end;
            }
            _ => return None,
        }
    }
}

/// A WebP keeping only the chunks that draw it and its colour profile, the flags that announce
/// EXIF and XMP cleared. `None` for a WebP that doesn't parse.
fn strip_webp(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.get(..4)? != b"RIFF" || bytes.get(8..12)? != b"WEBP" {
        return None;
    }
    let mut out = bytes[..12].to_vec();
    let mut i = 12;
    while i < bytes.len() {
        let fourcc = bytes.get(i..i.checked_add(4)?)?;
        let size = u32::from_le_bytes(bytes.get(i + 4..i + 8)?.try_into().ok()?) as usize;
        let end = i.checked_add(8)?.checked_add(size)?.checked_add(size & 1)?;
        let chunk = bytes.get(i..end.min(bytes.len()))?;
        match fourcc {
            b"VP8X" => {
                let mut c = chunk.to_vec();
                *c.get_mut(8)? &= !(0x08 | 0x04);
                out.extend_from_slice(&c);
            }
            b"ANIM" | b"ANMF" | b"ALPH" | b"VP8 " | b"VP8L" | b"ICCP" => out.extend_from_slice(chunk),
            _ => {}
        }
        i = end;
    }
    let riff = u32::try_from(out.len() - 8).ok()?;
    out[4..8].copy_from_slice(&riff.to_le_bytes());
    Some(out)
}

/// A PNG keeping only the chunks that draw it: text, EXIF, timestamps and anything unknown go.
fn strip_png(bytes: &[u8]) -> Option<Vec<u8>> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.get(..8)? != SIGNATURE {
        return None;
    }
    let mut out = SIGNATURE.to_vec();
    let mut i = 8;
    loop {
        let len = u32::from_be_bytes(bytes.get(i..i + 4)?.try_into().ok()?) as usize;
        let kind = bytes.get(i + 4..i + 8)?;
        let end = i.checked_add(12)?.checked_add(len)?;
        let chunk = bytes.get(i..end)?;
        let keep = matches!(
            kind,
            b"IHDR" | b"PLTE" | b"IDAT" | b"IEND" | b"tRNS" | b"gAMA" | b"cHRM" | b"sRGB" | b"iCCP" | b"sBIT" | b"bKGD" | b"pHYs" | b"acTL" | b"fcTL" | b"fdAT"
        );
        if keep {
            out.extend_from_slice(chunk);
        }
        if kind == b"IEND" {
            return Some(out);
        }
        i = end;
    }
}

/// An EXIF block holding only `orientation`, so a stripped photo still stands the right way up.
fn orientation_only_exif(orientation: u16) -> Vec<u8> {
    let mut exif = b"Exif\0\0MM\0*\0\0\0\x08\0\x01".to_vec();
    exif.extend_from_slice(&[0x01, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01]);
    exif.extend_from_slice(&orientation.to_be_bytes());
    exif.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    exif
}

/// A JPEG keeping only what decodes it: JFIF, its colour profile and Adobe's colour transform.
/// EXIF, XMP, IPTC and comments go; a rotated photo keeps just its orientation.
fn strip_jpeg(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let orientation = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Jpeg)
        .into_decoder()
        .ok()
        .and_then(|mut d| d.orientation().ok())
        .map(|o| o.to_exif())
        .filter(|&o| o > 1);
    let mut out = vec![0xFF, 0xD8];
    if let Some(o) = orientation {
        let exif = orientation_only_exif(o as u16);
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&u16::try_from(exif.len() + 2).ok()?.to_be_bytes());
        out.extend_from_slice(&exif);
    }
    let mut i = 2;
    loop {
        if *bytes.get(i)? != 0xFF {
            return None;
        }
        let marker = *bytes.get(i + 1)?;
        match marker {
            // End of image: whatever follows (a second picture, a video, a vendor's trailer) goes.
            0xD9 => {
                out.extend_from_slice(&[0xFF, 0xD9]);
                return Some(out);
            }
            0xFF => {
                i += 1;
                continue;
            }
            _ => {}
        }
        let len = u16::from_be_bytes(bytes.get(i + 2..i + 4)?.try_into().ok()?) as usize;
        let end = i.checked_add(2)?.checked_add(len)?;
        let segment = bytes.get(i..end)?;
        let keep = match marker {
            0xE0 => matches!(segment.get(4..9), Some(b"JFIF\0") | Some(b"JFXX\0")),
            0xEE => true,
            0xE2 => segment.get(4..16) == Some(b"ICC_PROFILE\0"),
            0xE1 | 0xE3..=0xED | 0xEF | 0xFE => false,
            _ => true,
        };
        if keep {
            out.extend_from_slice(segment);
        }
        i = end;
        // A scan's data runs to the next marker that isn't a stuffed byte or a restart.
        if marker == 0xDA {
            let start = i;
            loop {
                if *bytes.get(i)? == 0xFF {
                    match *bytes.get(i + 1)? {
                        0x00 | 0xD0..=0xD7 => i += 2,
                        _ => break,
                    }
                } else {
                    i += 1;
                }
            }
            out.extend_from_slice(&bytes[start..i]);
        }
    }
}

/// `bytes` with every byte of metadata cut out and nothing re-encoded, for a picture too large
/// to decode. `None` for a format this can't strip, or one that doesn't parse.
pub fn strip(bytes: &[u8]) -> Option<Prepared> {
    match image::guess_format(bytes).ok()? {
        ImageFormat::Jpeg => strip_jpeg(bytes).map(|bytes| Prepared { bytes, extension: "jpg" }),
        ImageFormat::Png => strip_png(bytes).map(|bytes| Prepared { bytes, extension: "png" }),
        ImageFormat::Gif => strip_gif(bytes).map(|bytes| Prepared { bytes, extension: "gif" }),
        ImageFormat::WebP => strip_webp(bytes).map(|bytes| Prepared { bytes, extension: "webp" }),
        _ => None,
    }
}

fn decode_upright(bytes: &[u8], format: ImageFormat) -> Result<DynamicImage, String> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits());
    let mut decoder = reader.into_decoder().map_err(|e| format!("decode image: {e}"))?;
    let orientation = decoder.orientation().map_err(|e| format!("decode image: {e}"))?;
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| format!("decode image: {e}"))?;
    img.apply_orientation(orientation);
    Ok(img)
}

fn has_transparency(img: &DynamicImage) -> bool {
    img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p.0[3] < 255)
}

fn encode(img: &DynamicImage, png: bool, quality: u8) -> Result<Prepared, String> {
    let mut out = Vec::new();
    if png {
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png).map_err(|e| format!("encode image: {e}"))?;
        return Ok(Prepared { bytes: out, extension: "png" });
    }
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&img.to_rgb8())
        .map_err(|e| format!("encode image: {e}"))?;
    Ok(Prepared { bytes: out, extension: "jpg" })
}

/// Make `bytes` ready to share as `usage`: upright, within its size, within its byte budget, and
/// with no metadata. SVG is refused, as Vector never ships a foreign SVG to other people.
pub fn prepare(bytes: &[u8], usage: ImageUse) -> Result<Prepared, String> {
    if crate::svg::looks_like_svg(bytes) {
        return Err("SVG images aren't supported; use PNG, JPEG, GIF or WebP".into());
    }
    let format = image::guess_format(bytes).map_err(|_| "not an image Vector can read (PNG, JPEG, GIF or WebP)".to_string())?;
    let (max_side, budget, animated_budget) = usage.budgets();
    if let Some((w, h, extension)) = animation(bytes)? {
        if bytes.len() <= animated_budget && w <= max_side && h <= max_side {
            let stripped = if extension == "gif" { strip_gif(bytes) } else { strip_webp(bytes) };
            let bytes = stripped.ok_or_else(|| format!("this {} couldn't be read to strip its metadata", extension.to_uppercase()))?;
            return Ok(Prepared { bytes, extension });
        }
        return Err(format!(
            "this animation is too large: at most {max_side}px a side and {} KB",
            animated_budget / 1024
        ));
    }
    let (w, h) = header_dims(bytes, format)?;
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(format!("this image is {w}x{h}: too many pixels to re-encode"));
    }
    let mut img = decode_upright(bytes, format)?;
    if img.width() > max_side || img.height() > max_side {
        img = img.resize(max_side, max_side, FilterType::CatmullRom);
    }
    let alpha = has_transparency(&img);
    // A PNG stays one when it fits: a chart or a screenshot blurs as JPEG.
    if format == ImageFormat::Png && !alpha {
        let out = encode(&img, true, 0)?;
        if out.bytes.len() <= budget {
            return Ok(out);
        }
    }
    // Quality first, then size: a smaller image at good quality beats a blocky large one.
    let qualities: &[u8] = if alpha { &[0] } else { &[85, 75, 65] };
    for &quality in qualities {
        let out = encode(&img, alpha, quality)?;
        if out.bytes.len() <= budget {
            return Ok(out);
        }
    }
    while img.width().max(img.height()) > 64 {
        img = img.resize(img.width() * 3 / 4, img.height() * 3 / 4, FilterType::CatmullRom);
        let out = encode(&img, alpha, 75)?;
        if out.bytes.len() <= budget {
            return Ok(out);
        }
    }
    Err(format!("this image can't be brought under {} KB", budget / 1024))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg_with_exif(w: u32, h: u32) -> Vec<u8> {
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])));
        let mut plain = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut plain, 90).encode_image(&img).unwrap();
        // An APP1 Exif segment right after SOI, as a camera writes one.
        let payload = b"Exif\0\0MM\0*\0\0\0\x08\0\0GPS-SECRET";
        let mut out = plain[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(payload);
        out.extend_from_slice(&plain[2..]);
        out
    }

    #[test]
    fn a_photo_leaves_without_its_metadata_and_within_its_size() {
        let raw = jpeg_with_exif(2000, 1000);
        assert!(raw.windows(10).any(|w| w == b"GPS-SECRET"));
        let out = prepare(&raw, ImageUse::Icon).unwrap();
        assert_eq!(out.extension, "jpg");
        assert!(!out.bytes.windows(10).any(|w| w == b"GPS-SECRET"), "metadata is gone");
        let back = image::load_from_memory(&out.bytes).unwrap();
        assert_eq!((back.width(), back.height()), (512, 256));
        assert!(out.bytes.len() <= 256 * 1024);
    }

    #[test]
    fn transparency_stays_png() {
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(64, 64, |x, _| image::Rgba([0, 0, 0, if x < 32 { 0 } else { 255 }])));
        let mut png = Vec::new();
        img.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
        assert_eq!(prepare(&png, ImageUse::Icon).unwrap().extension, "png");
    }

    fn gif(frames: usize, comment: bool) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut out);
            enc.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
            for n in 0..frames {
                let px = image::RgbaImage::from_pixel(16, 16, image::Rgba([n as u8 * 40, 0, 0, 255]));
                enc.encode_frame(image::Frame::new(px)).unwrap();
            }
        }
        if comment {
            // A comment extension right before the trailer.
            let t = out.len() - 1;
            out.splice(t..t, [0x21, 0xFE, 10, b'G', b'P', b'S', b'-', b'S', b'E', b'C', b'R', b'E', b'T', 0x00]);
        }
        out
    }

    #[test]
    fn an_animated_gif_keeps_its_frames_and_loses_its_comments() {
        let raw = gif(3, true);
        assert!(raw.windows(10).any(|w| w == b"GPS-SECRET"));
        let out = prepare(&raw, ImageUse::Icon).unwrap();
        assert_eq!(out.extension, "gif");
        assert!(!out.bytes.windows(10).any(|w| w == b"GPS-SECRET"));
        let d = image::codecs::gif::GifDecoder::new(Cursor::new(&out.bytes)).unwrap();
        assert_eq!(image::AnimationDecoder::into_frames(d).count(), 3, "every frame survives");
    }

    #[test]
    fn a_gif_declaring_a_vast_canvas_is_refused_before_any_frame_decodes() {
        let mut raw = gif(1, false);
        raw[6..10].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(prepare(&raw, ImageUse::Icon).is_err(), "a 65535x65535 canvas is refused from its header");
        raw[6..10].copy_from_slice(&[0x00, 0x20, 0x00, 0x20]);
        assert!(prepare(&raw, ImageUse::Icon).unwrap_err().contains("canvas"), "and so is one past 4096x4096");
    }

    #[test]
    fn webp_metadata_chunks_are_cut_and_announced_no_more() {
        let vp8x = [b"VP8X".as_slice(), &10u32.to_le_bytes(), &[0x0E, 0, 0, 0, 15, 0, 0, 15, 0, 0]].concat();
        let exif = [b"EXIF".as_slice(), &4u32.to_le_bytes(), b"GPS!"].concat();
        let anim = [b"ANIM".as_slice(), &6u32.to_le_bytes(), &[0; 6]].concat();
        let body = [b"WEBP".as_slice(), &vp8x, &exif, &anim].concat();
        let raw = [b"RIFF".as_slice(), &(body.len() as u32).to_le_bytes(), &body].concat();
        let out = strip_webp(&raw).unwrap();
        assert!(!out.windows(4).any(|w| w == b"GPS!" || w == b"EXIF"));
        assert_eq!(out[20] & 0x0C, 0, "the EXIF and XMP flags are cleared");
        assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()) as usize, out.len() - 8);
    }

    #[tokio::test]
    async fn a_sent_picture_is_compressed_unless_asked_not_to() {
        let raw = jpeg_with_exif(3000, 1500);
        let (out, name) = crate::ready_to_send(raw.clone(), "holiday.jpeg", true).await.unwrap();
        assert_eq!(name, "holiday.jpeg", "a JPEG keeps its own name");
        assert!(out.len() < raw.len() && !out.windows(10).any(|w| w == b"GPS-SECRET"));
        assert_eq!(image::load_from_memory(&out).unwrap().width(), 2560);

        let (kept, name) = crate::ready_to_send(raw.clone(), "holiday.jpeg", false).await.unwrap();
        assert_eq!((kept, name.as_str()), (raw, "holiday.jpeg"), "compress(false) sends the bytes as they are");

        let notes = b"just some text".to_vec();
        let (same, name) = crate::ready_to_send(notes.clone(), "notes.txt", true).await.unwrap();
        assert_eq!((same, name.as_str()), (notes, "notes.txt"), "a file that isn't a picture goes untouched");
    }

    /// `jpeg_with_exif` with its EXIF block saying it's turned a quarter.
    fn rotated_jpeg_with_exif() -> Vec<u8> {
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_fn(40, 20, |x, _| image::Rgb([x as u8 * 6, 0, 90])));
        let mut plain = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut plain, 90).encode_image(&img).unwrap();
        let mut exif = orientation_only_exif(6);
        exif.extend_from_slice(b"GPS-SECRET");
        let mut out = plain[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&exif);
        out.extend_from_slice(&[0xFF, 0xFE, 0x00, 0x08]);
        out.extend_from_slice(b"secret");
        out.extend_from_slice(&plain[2..]);
        // A trailer past the end of the image, as a phone appends a second picture or a video.
        out.extend_from_slice(b"TRAILER-GPS-SECRET");
        out
    }

    #[test]
    fn a_stripped_jpeg_keeps_its_pixels_and_which_way_is_up_and_nothing_else() {
        let raw = rotated_jpeg_with_exif();
        let out = strip(&raw).unwrap();
        assert_eq!(out.extension, "jpg");
        assert!(!out.bytes.windows(10).any(|w| w == b"GPS-SECRET") && !out.bytes.windows(6).any(|w| w == b"secret"));
        assert_eq!(out.bytes[out.bytes.len() - 2..], [0xFF, 0xD9], "nothing after the end of the image");
        let mut d = ImageReader::with_format(Cursor::new(&out.bytes), ImageFormat::Jpeg).into_decoder().unwrap();
        assert_eq!(d.orientation().unwrap(), image::metadata::Orientation::Rotate90);
        assert_eq!(image::load_from_memory(&out.bytes).unwrap().to_rgb8(), image::load_from_memory(&raw).unwrap().to_rgb8(), "not re-encoded");
    }

    #[test]
    fn a_stripped_png_and_gif_lose_their_text() {
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([1, 2, 3])));
        let mut png = Vec::new();
        img.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
        // A tEXt chunk after IHDR, as an editor writes one.
        let text = b"Commentlocation: home";
        let mut chunk = (text.len() as u32).to_be_bytes().to_vec();
        chunk.extend_from_slice(b"tEXt");
        chunk.extend_from_slice(text);
        chunk.extend_from_slice(&[0; 4]);
        png.splice(33..33, chunk);
        let out = strip(&png).unwrap();
        assert!(!out.bytes.windows(4).any(|w| w == b"tEXt"));
        assert_eq!(image::load_from_memory(&out.bytes).unwrap().to_rgb8(), img.to_rgb8());

        // A plain-text extension, which draws text and carries whatever it says.
        let mut raw = gif(2, false);
        let t = raw.len() - 1;
        raw.splice(t..t, [0x21, 0x01, 12, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 6, b'h', b'i', b'd', b'd', b'e', b'n', 0x00]);
        let out = strip_gif(&raw).unwrap();
        assert!(!out.windows(6).any(|w| w == b"hidden"));
        let d = image::codecs::gif::GifDecoder::new(Cursor::new(&out)).unwrap();
        assert_eq!(image::AnimationDecoder::into_frames(d).count(), 2);
    }

    #[test]
    fn a_chart_stays_png() {
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_fn(800, 400, |x, y| if (x / 40 + y / 40) % 2 == 0 { image::Rgb([255, 255, 255]) } else { image::Rgb([20, 90, 200]) }));
        let mut png = Vec::new();
        img.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
        let out = prepare(&png, ImageUse::Photo).unwrap();
        assert_eq!(out.extension, "png", "lossless, so its edges stay sharp");
    }

    #[test]
    fn svg_and_noise_are_refused() {
        assert!(prepare(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>", ImageUse::Icon).is_err());
        assert!(prepare(b"definitely not an image", ImageUse::Icon).is_err());
    }
}
