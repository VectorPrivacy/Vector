//! EXIF in the containers Vector sends (JPEG, PNG, WebP, TIFF): find it, and rebuild it as a
//! standalone block a re-encoded JPEG can carry.
//!
//! The rebuild keeps IFD0's descriptive tags and its Exif, GPS and Interop sub-IFDs, in the
//! source's byte order. It drops what described the original pixels (strips, tiles, dimensions,
//! colour profile), the IFD1 thumbnail (it shows the original, uncropped and unrotated), and the
//! MakerNote, whose vendor offsets point into the original file and break when moved.

use std::borrow::Cow;
use std::io::Read;

/// The TIFF-structured EXIF block `bytes` carries, by container. A TIFF file is one.
pub fn exif_block<'a>(bytes: &'a [u8], extension: &str) -> Option<Cow<'a, [u8]>> {
    match extension.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => jpeg_exif(bytes).map(Cow::Borrowed),
        "png" => png_exif(bytes),
        "webp" => webp_exif(bytes).map(Cow::Borrowed),
        "tif" | "tiff" => Some(Cow::Borrowed(bytes)),
        _ => None,
    }
}

fn jpeg_exif(b: &[u8]) -> Option<&[u8]> {
    if b.get(0..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut i = 2;
    loop {
        if *b.get(i)? != 0xFF {
            return None;
        }
        let marker = *b.get(i + 1)?;
        match marker {
            0xFF => {
                i += 1;
                continue;
            }
            0xDA | 0xD9 => return None,
            0x01 | 0xD0..=0xD7 => {
                i += 2;
                continue;
            }
            _ => {}
        }
        let len = u16::from_be_bytes([*b.get(i + 2)?, *b.get(i + 3)?]) as usize;
        len.checked_sub(2)?;
        let body = b.get(i + 4..i + 2 + len)?;
        if marker == 0xE1 {
            if let Some(tiff) = body.strip_prefix(b"Exif\0\0") {
                return Some(tiff);
            }
        }
        i += 2 + len;
    }
}

fn png_exif(b: &[u8]) -> Option<Cow<'_, [u8]>> {
    if b.get(0..8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut i = 8;
    loop {
        let len = u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?) as usize;
        let ty = b.get(i + 4..i + 8)?;
        let data = b.get(i + 8..(i + 8).checked_add(len)?)?;
        match ty {
            b"eXIf" => return Some(Cow::Borrowed(data)),
            b"tEXt" | b"zTXt" | b"iTXt" => {
                if let Some(tiff) = raw_profile_exif(ty, data) {
                    return Some(Cow::Owned(tiff));
                }
            }
            b"IEND" => return None,
            _ => {}
        }
        i += 12 + len;
    }
}

/// Upper bound on an inflated text chunk; an APP1 caps EXIF at 64 KiB, hex doubles it.
const MAX_TEXT: u64 = 1 << 20;

/// ImageMagick's and exiftool's pre-`eXIf` form: a "Raw profile type exif" text chunk holding
/// `\nexif\n<decimal length>\n<hex lines>` over an `Exif\0\0`-prefixed block.
fn raw_profile_exif(ty: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    let text = data.strip_prefix(b"Raw profile type exif\0")?;
    let (compressed, text) = match ty {
        b"tEXt" => (false, text),
        b"zTXt" => (true, text.strip_prefix(&[0])?),
        _ => {
            // iTXt: compression flag, method, language tag, translated keyword.
            let (&[flag, 0], rest) = text.split_first_chunk::<2>()? else { return None };
            let rest = &rest[rest.iter().position(|&c| c == 0)? + 1..];
            (flag != 0, &rest[rest.iter().position(|&c| c == 0)? + 1..])
        }
    };
    let inflated;
    let text = if compressed {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(text).take(MAX_TEXT).read_to_end(&mut out).ok()?;
        inflated = out;
        &inflated[..]
    } else {
        text
    };
    let mut fields = text.split(|c| c.is_ascii_whitespace()).filter(|f| !f.is_empty());
    if !fields.next()?.eq_ignore_ascii_case(b"exif") {
        return None;
    }
    let len: usize = std::str::from_utf8(fields.next()?).ok()?.parse().ok()?;
    let mut out = Vec::with_capacity(len.min(MAX_TEXT as usize));
    let mut hi = None;
    for c in fields.flatten() {
        let v = (*c as char).to_digit(16)? as u8;
        match hi.take() {
            None => hi = Some(v),
            Some(h) => out.push(h << 4 | v),
        }
        if out.len() == len {
            break;
        }
    }
    if out.len() != len {
        return None;
    }
    if out.starts_with(b"Exif\0\0") {
        out.drain(..6);
    }
    Some(out)
}

fn webp_exif(b: &[u8]) -> Option<&[u8]> {
    if b.get(0..4)? != b"RIFF" || b.get(8..12)? != b"WEBP" {
        return None;
    }
    let mut i = 12;
    loop {
        let ty = b.get(i..i + 4)?;
        let len = u32::from_le_bytes(b.get(i + 4..i + 8)?.try_into().ok()?) as usize;
        let data = b.get(i + 8..(i + 8).checked_add(len)?)?;
        if ty == b"EXIF" {
            // Some writers keep the JPEG APP1 prefix.
            return Some(data.strip_prefix(b"Exif\0\0").unwrap_or(data));
        }
        i += 8 + len + (len & 1);
    }
}

const TAG_ORIENTATION: u16 = 0x0112;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_GPS_IFD: u16 = 0x8825;
const TAG_INTEROP_IFD: u16 = 0xA005;

/// IFD0 tags that describe the original's pixels, not the photo.
const PIXEL_TAGS: &[u16] = &[
    0x00FE, 0x00FF, 0x0100, 0x0101, 0x0102, 0x0103, 0x0106, 0x0107, 0x0108, 0x0109, 0x010A, 0x0111, 0x0115,
    0x0116, 0x0117, 0x0118, 0x0119, 0x011C, 0x0122, 0x0123, 0x0124, 0x0125, 0x013D, 0x0140, 0x0142, 0x0143,
    0x0144, 0x0145, 0x014A, 0x0152, 0x0153, 0x0154, 0x0155, 0x0156, 0x0201, 0x0202, 0x0211, 0x0212, 0x0213,
    0x0214, 0x8773,
];

/// Exif IFD tags dropped: the MakerNote, and the original's pixel dimensions.
const EXIF_DROPPED: &[u16] = &[0x927C, 0xA002, 0xA003];

/// Caps that keep a hostile block from costing more than a real one.
const MAX_ENTRIES: usize = 1024;
const MAX_OUT: usize = 64 * 1024;

#[derive(Clone, Copy)]
struct Order {
    le: bool,
}

impl Order {
    fn u16(self, b: &[u8], at: usize) -> Option<u16> {
        let v: [u8; 2] = b.get(at..at + 2)?.try_into().ok()?;
        Some(if self.le { u16::from_le_bytes(v) } else { u16::from_be_bytes(v) })
    }
    fn u32(self, b: &[u8], at: usize) -> Option<u32> {
        let v: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
        Some(if self.le { u32::from_le_bytes(v) } else { u32::from_be_bytes(v) })
    }
    fn put16(self, v: u16) -> [u8; 2] {
        if self.le { v.to_le_bytes() } else { v.to_be_bytes() }
    }
    fn put32(self, v: u32) -> [u8; 4] {
        if self.le { v.to_le_bytes() } else { v.to_be_bytes() }
    }
}

fn type_size(ty: u16) -> Option<usize> {
    match ty {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 | 13 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Ifd {
    Zero,
    Exif,
    Gps,
    Interop,
}

/// `tiff` rebuilt as a standalone block with its Orientation set to `orientation`; `None` when
/// it isn't a readable TIFF structure.
pub fn rebuild(tiff: &[u8], orientation: u16) -> Option<Vec<u8>> {
    let order = match tiff.get(0..4)? {
        b"II*\0" => Order { le: true },
        b"MM\0*" => Order { le: false },
        _ => return None,
    };
    let mut out = Vec::with_capacity(tiff.len().min(MAX_OUT));
    out.extend_from_slice(&tiff[0..4]);
    out.extend_from_slice(&order.put32(8));
    let ifd0 = order.u32(tiff, 4)? as usize;
    write_ifd(tiff, order, ifd0, Ifd::Zero, Some(orientation), &mut out)?;
    (out.len() <= MAX_OUT).then_some(out)
}

/// Appends the IFD at `at` (then its values and sub-IFDs) to `out`; returns where it starts.
fn write_ifd(src: &[u8], order: Order, at: usize, kind: Ifd, orientation: Option<u16>, out: &mut Vec<u8>) -> Option<u32> {
    let count = order.u16(src, at)? as usize;
    if count > MAX_ENTRIES {
        return None;
    }
    struct Entry<'a> {
        tag: u16,
        ty: u16,
        count: u32,
        value: &'a [u8],
        sub: Option<Ifd>,
    }
    let mut entries: Vec<Entry> = Vec::with_capacity(count + 1);
    for k in 0..count {
        let e = at + 2 + k * 12;
        let tag = order.u16(src, e)?;
        let ty = order.u16(src, e + 2)?;
        let n = order.u32(src, e + 4)?;
        let dropped = match kind {
            Ifd::Zero => PIXEL_TAGS.contains(&tag),
            Ifd::Exif => EXIF_DROPPED.contains(&tag),
            _ => false,
        };
        let sub = match (kind, tag) {
            (Ifd::Zero, TAG_EXIF_IFD) => Some(Ifd::Exif),
            (Ifd::Zero, TAG_GPS_IFD) => Some(Ifd::Gps),
            (Ifd::Exif, TAG_INTEROP_IFD) => Some(Ifd::Interop),
            _ => None,
        };
        let Some(size) = type_size(ty) else { continue };
        if dropped || (kind == Ifd::Zero && tag == TAG_ORIENTATION) {
            continue;
        }
        let len = size.checked_mul(n as usize)?;
        let value = if len <= 4 {
            src.get(e + 8..e + 8 + len)?
        } else {
            let off = order.u32(src, e + 8)? as usize;
            src.get(off..off.checked_add(len)?)?
        };
        if sub.is_some() && (ty != 4 && ty != 13 || n != 1) {
            continue;
        }
        entries.push(Entry { tag, ty, count: n, value, sub });
    }
    let orientation_bytes = orientation.map(|o| order.put16(o));
    if let Some(o) = &orientation_bytes {
        entries.push(Entry { tag: TAG_ORIENTATION, ty: 3, count: 1, value: o, sub: None });
    }
    entries.sort_by_key(|e| e.tag);

    let start = out.len();
    out.extend_from_slice(&order.put16(entries.len() as u16));
    for e in &entries {
        out.extend_from_slice(&order.put16(e.tag));
        out.extend_from_slice(&order.put16(e.ty));
        out.extend_from_slice(&order.put32(e.count));
        let mut inline = [0u8; 4];
        if e.value.len() <= 4 {
            inline[..e.value.len()].copy_from_slice(e.value);
        }
        out.extend_from_slice(&inline);
    }
    out.extend_from_slice(&[0; 4]);
    for (k, e) in entries.iter().enumerate() {
        let slot = start + 2 + k * 12 + 8;
        let target = if let Some(sub) = e.sub {
            if out.len() & 1 == 1 {
                out.push(0);
            }
            let sub_at = order.u32(e.value, 0)? as usize;
            write_ifd(src, order, sub_at, sub, None, out)?
        } else if e.value.len() > 4 {
            if out.len() & 1 == 1 {
                out.push(0);
            }
            let at = out.len() as u32;
            out.extend_from_slice(e.value);
            at
        } else {
            continue;
        };
        if out.len() > MAX_OUT {
            return None;
        }
        out[slot..slot + 4].copy_from_slice(&order.put32(target));
    }
    u32::try_from(start).ok()
}

/// `jpeg` with `exif` inserted as an APP1 segment after SOI (and JFIF, when present).
pub fn insert_into_jpeg(jpeg: &[u8], exif: &[u8]) -> Option<Vec<u8>> {
    if jpeg.get(0..2)? != [0xFF, 0xD8] {
        return None;
    }
    let seg_len = u16::try_from(2 + 6 + exif.len()).ok()?;
    let mut at = 2;
    if jpeg.get(2..4)? == [0xFF, 0xE0] {
        at = 4 + u16::from_be_bytes([*jpeg.get(4)?, *jpeg.get(5)?]) as usize;
        jpeg.get(at..)?;
    }
    let mut out = Vec::with_capacity(jpeg.len() + 4 + 6 + exif.len());
    out.extend_from_slice(&jpeg[..at]);
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&seg_len.to_be_bytes());
    out.extend_from_slice(b"Exif\0\0");
    out.extend_from_slice(exif);
    out.extend_from_slice(&jpeg[at..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A little- or big-endian TIFF block: IFD0 {Make, Orientation=6, ImageWidth, ExifIFD, GPS},
    /// Exif {DateTimeOriginal, MakerNote, InteropIFD}, GPS {LatitudeRef}, Interop {Index}, IFD1.
    fn sample(le: bool) -> Vec<u8> {
        let o = Order { le };
        let mut b = Vec::new();
        b.extend_from_slice(if le { b"II*\0" } else { b"MM\0*" });
        b.extend_from_slice(&o.put32(8));
        let entry = |b: &mut Vec<u8>, tag: u16, ty: u16, n: u32, v: [u8; 4]| {
            b.extend_from_slice(&o.put16(tag));
            b.extend_from_slice(&o.put16(ty));
            b.extend_from_slice(&o.put32(n));
            b.extend_from_slice(&v);
        };
        // Offsets below are fixed by this layout.
        let (make_at, exif_at, gps_at, ifd1_at) = (8 + 2 + 5 * 12 + 4, 100, 160, 200);
        let (dto_at, interop_at) = (240, 280);
        b.extend_from_slice(&o.put16(5));
        entry(&mut b, 0x010F, 2, 8, o.put32(make_at));
        let mut six = [0u8; 4];
        six[..2].copy_from_slice(&o.put16(6));
        entry(&mut b, 0x0100, 4, 1, o.put32(4000));
        entry(&mut b, 0x0112, 3, 1, six);
        entry(&mut b, 0x8769, 4, 1, o.put32(exif_at));
        entry(&mut b, 0x8825, 4, 1, o.put32(gps_at));
        b.extend_from_slice(&o.put32(ifd1_at));
        b.extend_from_slice(b"Vectorm\0");
        b.resize(exif_at as usize, 0);
        b.extend_from_slice(&o.put16(3));
        entry(&mut b, 0x9003, 2, 20, o.put32(dto_at));
        entry(&mut b, 0x927C, 7, 4, *b"MAKR");
        entry(&mut b, 0xA005, 4, 1, o.put32(interop_at));
        b.extend_from_slice(&[0; 4]);
        b.resize(gps_at as usize, 0);
        b.extend_from_slice(&o.put16(1));
        entry(&mut b, 0x0001, 2, 2, *b"N\0\0\0");
        b.extend_from_slice(&[0; 4]);
        b.resize(ifd1_at as usize, 0);
        b.extend_from_slice(&o.put16(1));
        entry(&mut b, 0x0201, 4, 1, o.put32(0));
        b.extend_from_slice(&[0; 4]);
        b.resize(dto_at as usize, 0);
        b.extend_from_slice(b"2026:10:11 12:00:00\0");
        b.resize(interop_at as usize, 0);
        b.extend_from_slice(&o.put16(1));
        entry(&mut b, 0x0001, 2, 4, *b"R98\0");
        b.extend_from_slice(&[0; 4]);
        b
    }

    /// (ifd, tag, raw value) for every entry reachable from IFD0, plus IFD0's next pointer.
    fn walk(t: &[u8]) -> (Vec<(u8, u16, Vec<u8>)>, u32) {
        let o = Order { le: &t[0..2] == b"II" };
        let mut found = Vec::new();
        fn go(t: &[u8], o: Order, at: usize, ifd: u8, found: &mut Vec<(u8, u16, Vec<u8>)>) -> u32 {
            let n = o.u16(t, at).unwrap() as usize;
            for k in 0..n {
                let e = at + 2 + k * 12;
                let (tag, ty, cnt) = (o.u16(t, e).unwrap(), o.u16(t, e + 2).unwrap(), o.u32(t, e + 4).unwrap());
                let len = type_size(ty).unwrap() * cnt as usize;
                let v = if len <= 4 { t[e + 8..e + 8 + len].to_vec() } else {
                    let off = o.u32(t, e + 8).unwrap() as usize;
                    assert_eq!(off % 2, 0, "unaligned value");
                    t[off..off + len].to_vec()
                };
                match tag {
                    0x8769 => { go(t, o, o.u32(t, e + 8).unwrap() as usize, 1, found); }
                    0x8825 => { go(t, o, o.u32(t, e + 8).unwrap() as usize, 2, found); }
                    0xA005 => { go(t, o, o.u32(t, e + 8).unwrap() as usize, 3, found); }
                    _ => found.push((ifd, tag, v)),
                }
            }
            o.u32(t, at + 2 + n * 12).unwrap()
        }
        let next = go(t, o, o.u32(t, 4).unwrap() as usize, 0, &mut found);
        (found, next)
    }

    #[test]
    fn rebuild_keeps_the_photo_and_drops_the_pixels_thumbnail_and_makernote() {
        for le in [true, false] {
            let out = rebuild(&sample(le), 1).unwrap();
            let (tags, next) = walk(&out);
            let o = Order { le };
            assert_eq!(next, 0, "IFD1 thumbnail dropped");
            assert_eq!(
                tags,
                vec![
                    (0, 0x010F, b"Vectorm\0".to_vec()),
                    (0, 0x0112, o.put16(1).to_vec()),
                    (1, 0x9003, b"2026:10:11 12:00:00\0".to_vec()),
                    (3, 0x0001, b"R98\0".to_vec()),
                    (2, 0x0001, b"N\0".to_vec()),
                ],
                "le={le}"
            );
        }
    }

    #[test]
    fn rebuild_refuses_what_is_not_tiff_and_survives_hostile_offsets() {
        assert!(rebuild(b"JFIF....", 1).is_none());
        let mut bad = sample(true);
        bad[4..8].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        assert!(rebuild(&bad, 1).is_none());
        // A sub-IFD pointing at itself recurses only as far as the IFD kinds allow.
        let mut cyc = sample(true);
        cyc[100 + 2 + 2 * 12 + 8..100 + 2 + 2 * 12 + 12].copy_from_slice(&100u32.to_le_bytes());
        let _ = rebuild(&cyc, 1);
        for n in 0..sample(true).len() {
            let _ = rebuild(&sample(true)[..n], 1);
        }
    }

    #[test]
    fn exif_is_found_in_each_container_and_inserted_after_jfif() {
        let tiff = sample(true);
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16];
        jpeg.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0, 2, 0xFF, 0xD9]);
        let with = insert_into_jpeg(&jpeg, &tiff).unwrap();
        assert_eq!(&with[20..22], &[0xFF, 0xE1]);
        assert_eq!(exif_block(&with, "JPG").as_deref(), Some(tiff.as_slice()));
        assert_eq!(&with[with.len() - 6..], &jpeg[jpeg.len() - 6..]);

        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&(tiff.len() as u32).to_be_bytes());
        png.extend_from_slice(b"eXIf");
        png.extend_from_slice(&tiff);
        png.extend_from_slice(&[0; 4]);
        assert_eq!(exif_block(&png, "png").as_deref(), Some(tiff.as_slice()));

        let mut text = format!("\nexif\n{:8}\n", tiff.len() + 6);
        for (k, b) in b"Exif\0\0".iter().chain(&tiff).enumerate() {
            text.push_str(&format!("{b:02x}{}", if k % 36 == 35 { "\n" } else { "" }));
        }
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut z, text.as_bytes()).unwrap();
        let mut body = b"Raw profile type exif\0\0".to_vec();
        body.extend(z.finish().unwrap());
        let mut ztxt = b"\x89PNG\r\n\x1a\n".to_vec();
        ztxt.extend_from_slice(&(body.len() as u32).to_be_bytes());
        ztxt.extend_from_slice(b"zTXt");
        ztxt.extend_from_slice(&body);
        ztxt.extend_from_slice(&[0; 4]);
        assert_eq!(exif_block(&ztxt, "png").as_deref(), Some(tiff.as_slice()));

        let mut webp = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        webp.extend_from_slice(&10u32.to_le_bytes());
        webp.extend_from_slice(&[0; 10]);
        webp.extend_from_slice(b"EXIF");
        webp.extend_from_slice(&(tiff.len() as u32 + 6).to_le_bytes());
        webp.extend_from_slice(b"Exif\0\0");
        webp.extend_from_slice(&tiff);
        assert_eq!(exif_block(&webp, "webp").as_deref(), Some(tiff.as_slice()));
        assert_eq!(exif_block(&tiff, "tif").as_deref(), Some(tiff.as_slice()));
        assert_eq!(exif_block(&tiff, "gif"), None);
    }
}
