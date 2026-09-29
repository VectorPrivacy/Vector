//! Animated WebP through libwebp, streamed a frame at a time both ways, so memory stays one
//! canvas however long the animation. libwebp is the reference decoder: `image-webp`
//! mishandles ANIM disposal and blending.

use libwebp_sys as ffi;
use std::marker::PhantomData;

/// Composited RGBA frames and their display time in milliseconds.
pub struct Decoder<'a> {
    dec: *mut ffi::WebPAnimDecoder,
    width: u32,
    height: u32,
    last_ts: i32,
    _bytes: PhantomData<&'a [u8]>,
}

impl<'a> Decoder<'a> {
    pub fn new(bytes: &'a [u8]) -> Result<Self, String> {
        // SAFETY: the options are initialised by libwebp before use; the decoder borrows
        // `bytes`, which PhantomData ties to this value's lifetime.
        unsafe {
            let mut opts = std::mem::MaybeUninit::<ffi::WebPAnimDecoderOptions>::uninit();
            if ffi::WebPAnimDecoderOptionsInit(opts.as_mut_ptr()) == 0 {
                return Err("webp decode: libwebp version mismatch".into());
            }
            let mut opts = opts.assume_init();
            opts.color_mode = ffi::WEBP_CSP_MODE::MODE_RGBA;
            opts.use_threads = 1;
            let data = ffi::WebPData { bytes: bytes.as_ptr(), size: bytes.len() };
            let dec = ffi::WebPAnimDecoderNew(&data, &opts);
            if dec.is_null() {
                return Err("webp decode: not a valid WebP".into());
            }
            let mut info = ffi::WebPAnimInfo::default();
            if ffi::WebPAnimDecoderGetInfo(dec, &mut info) == 0 {
                ffi::WebPAnimDecoderDelete(dec);
                return Err("webp decode: unreadable header".into());
            }
            Ok(Decoder { dec, width: info.canvas_width, height: info.canvas_height, last_ts: 0, _bytes: PhantomData })
        }
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl Iterator for Decoder<'_> {
    type Item = Result<(image::RgbaImage, u32), String>;

    fn next(&mut self) -> Option<Self::Item> {
        // SAFETY: GetNext hands back a canvas-sized RGBA buffer owned by the decoder and valid
        // until the next call; it is copied out before then.
        unsafe {
            if ffi::WebPAnimDecoderHasMoreFrames(self.dec) == 0 {
                return None;
            }
            let (mut buf, mut ts) = (std::ptr::null_mut::<u8>(), 0);
            if ffi::WebPAnimDecoderGetNext(self.dec, &mut buf, &mut ts) == 0 || buf.is_null() {
                return Some(Err("webp decode: corrupt frame".into()));
            }
            let len = self.width as usize * self.height as usize * 4;
            let px = std::slice::from_raw_parts(buf, len).to_vec();
            // libwebp reports each frame's end time.
            let ms = (ts - self.last_ts).max(0) as u32;
            self.last_ts = ts;
            Some(image::RgbaImage::from_raw(self.width, self.height, px).map(|img| (img, ms)).ok_or_else(|| "webp decode: frame size".into()))
        }
    }
}

impl Drop for Decoder<'_> {
    fn drop(&mut self) {
        // SAFETY: created by WebPAnimDecoderNew and deleted exactly once.
        unsafe { ffi::WebPAnimDecoderDelete(self.dec) }
    }
}

/// Lossy animated WebP. Each frame may fall back to lossless where that is smaller (flat
/// art, pixel art), and colour is converted with sharp YUV so saturated edges stay crisp.
pub struct Encoder {
    enc: *mut ffi::WebPAnimEncoder,
    config: ffi::WebPConfig,
    width: u32,
    height: u32,
    ts: i32,
}

impl Encoder {
    pub fn new(width: u32, height: u32, quality: f32) -> Result<Self, String> {
        // SAFETY: options and config are initialised by libwebp before use.
        unsafe {
            let mut opts = std::mem::MaybeUninit::<ffi::WebPAnimEncoderOptions>::uninit();
            if ffi::WebPAnimEncoderOptionsInitInternal(opts.as_mut_ptr(), ffi::WEBP_MUX_ABI_VERSION as i32) == 0 {
                return Err("webp encode: libwebp version mismatch".into());
            }
            let mut opts = opts.assume_init();
            opts.allow_mixed = 1;
            opts.anim_params.loop_count = 0;
            let mut config = ffi::WebPConfig::new_with_preset(ffi::WebPPreset::WEBP_PRESET_DEFAULT, quality)
                .map_err(|_| String::from("webp encode: bad config"))?;
            config.method = 4;
            config.use_sharp_yuv = 1;
            config.thread_level = 1;
            if ffi::WebPValidateConfig(&config) == 0 {
                return Err("webp encode: bad config".into());
            }
            let enc = ffi::WebPAnimEncoderNewInternal(width as i32, height as i32, &opts, ffi::WEBP_MUX_ABI_VERSION as i32);
            if enc.is_null() {
                return Err("webp encode: encoder refused the canvas".into());
            }
            Ok(Encoder { enc, config, width, height, ts: 0 })
        }
    }

    /// Append a canvas-sized RGBA frame shown for `ms` milliseconds.
    pub fn add(&mut self, rgba: &[u8], ms: u32) -> Result<(), String> {
        if rgba.len() != self.width as usize * self.height as usize * 4 {
            return Err("webp encode: frame size".into());
        }
        // SAFETY: the picture is initialised, imports a copy of `rgba`, is handed to the
        // encoder (which copies what it keeps) and freed before returning.
        unsafe {
            let mut pic = ffi::WebPPicture::new().map_err(|_| String::from("webp encode: picture"))?;
            pic.use_argb = 1;
            pic.width = self.width as i32;
            pic.height = self.height as i32;
            let ok = ffi::WebPPictureImportRGBA(&mut pic, rgba.as_ptr(), self.width as i32 * 4) != 0
                && ffi::WebPAnimEncoderAdd(self.enc, &mut pic, self.ts, &self.config) != 0;
            ffi::WebPPictureFree(&mut pic);
            if !ok {
                return Err(self.error());
            }
        }
        self.ts = self.ts.saturating_add(ms.min(i32::MAX as u32) as i32);
        Ok(())
    }

    pub fn finish(self) -> Result<Vec<u8>, String> {
        // SAFETY: a null frame closes the stream at the last frame's end time; the assembled
        // buffer is copied out and released by WebPDataClear.
        unsafe {
            if ffi::WebPAnimEncoderAdd(self.enc, std::ptr::null_mut(), self.ts, std::ptr::null()) == 0 {
                return Err(self.error());
            }
            let mut data = ffi::WebPData::default();
            if ffi::WebPAnimEncoderAssemble(self.enc, &mut data) == 0 {
                return Err(self.error());
            }
            let out = std::slice::from_raw_parts(data.bytes, data.size).to_vec();
            ffi::WebPDataClear(&mut data);
            Ok(out)
        }
    }

    fn error(&self) -> String {
        // SAFETY: libwebp returns a NUL-terminated string it owns, or null.
        unsafe {
            let e = ffi::WebPAnimEncoderGetError(self.enc);
            let msg = if e.is_null() { "unknown".into() } else { std::ffi::CStr::from_ptr(e).to_string_lossy().into_owned() };
            format!("webp encode: {msg}")
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: created by WebPAnimEncoderNewInternal and deleted exactly once.
        unsafe { ffi::WebPAnimEncoderDelete(self.enc) }
    }
}

/// Join animations encoded apart into one, frame after frame, each part given with its total
/// duration. Every part after the first must open on a frame covering the whole canvas; that
/// frame is marked not to blend, so it repaints the canvas whatever the part before left.
/// `None` when a part can't be joined that way (its first frame was cropped).
pub fn concat(parts: &[(Vec<u8>, u32)], width: u32, height: u32) -> Option<Vec<u8>> {
    let mut frames: Vec<Vec<u8>> = Vec::new();
    let mut anim: Option<Vec<u8>> = None;
    let mut alpha = false;
    for (k, (file, ms)) in parts.iter().enumerate() {
        let chunks = riff_chunks(file)?;
        alpha |= chunks.iter().any(|(id, body)| (id == b"VP8X" && body.first().is_some_and(|f| f & 0x10 != 0)) || id == b"ALPH" || id == b"VP8L");
        if anim.is_none() {
            anim = chunks.iter().find(|(id, _)| id == b"ANIM").map(|(_, b)| b.to_vec());
        }
        let mut part: Vec<Vec<u8>> = chunks.iter().filter(|(id, _)| id == b"ANMF").map(|(_, b)| b.to_vec()).collect();
        if part.is_empty() {
            // A part that came out as one frame is a still image: frame it for the animation.
            let mut body = Vec::new();
            for f in [0, 0, width - 1, height - 1, *ms] {
                body.extend_from_slice(&f.to_le_bytes()[..3]);
            }
            body.push(0);
            for (id, b) in chunks.iter().filter(|(id, _)| matches!(id, b"ALPH" | b"VP8 " | b"VP8L")) {
                push_chunk(&mut body, id, b);
            }
            part.push(body);
        }
        if k > 0 {
            let first = part.first_mut()?;
            let u24 = |i: usize| u32::from_le_bytes([first[i], first[i + 1], first[i + 2], 0]);
            if first.len() < 16 || u24(0) != 0 || u24(3) != 0 || u24(6) + 1 != width || u24(9) + 1 != height {
                return None;
            }
            first[15] |= 0b10;
        }
        frames.extend(part);
    }
    let mut body = Vec::new();
    let mut vp8x = vec![0x02 | if alpha { 0x10 } else { 0 }, 0, 0, 0];
    vp8x.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
    vp8x.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
    push_chunk(&mut body, b"VP8X", &vp8x);
    push_chunk(&mut body, b"ANIM", &anim.unwrap_or_else(|| vec![0; 6]));
    for f in &frames {
        push_chunk(&mut body, b"ANMF", f);
    }
    let mut out = Vec::with_capacity(body.len() + 12);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(body.len() as u32 + 4).to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend(body);
    Some(out)
}

fn riff_chunks(file: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
    if file.len() < 12 || &file[..4] != b"RIFF" || &file[8..12] != b"WEBP" {
        return None;
    }
    let mut out = Vec::new();
    let mut i = 12;
    while i + 8 <= file.len() {
        let id: [u8; 4] = file[i..i + 4].try_into().ok()?;
        let n = u32::from_le_bytes(file[i + 4..i + 8].try_into().ok()?) as usize;
        out.push((id, file.get(i + 8..i + 8 + n)?));
        i += 8 + n + (n & 1);
    }
    Some(out)
}

fn push_chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}
