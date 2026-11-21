//! Outbound pictures as JPEG XL. A JPEG is repacked losslessly (libjxl keeps what it needs to
//! rebuild the original file bit for bit), about a fifth smaller with not a pixel changed; a
//! still PNG goes as lossless JPEG XL.
//! GPUI only: the Tauri client keeps sending JPEG until it migrates.

use crate::message::types::CachedCompressedImage;
use std::ffi::c_void;
use std::sync::Arc;

#[allow(non_camel_case_types)]
type JxlEncoderStatus = i32;
const SUCCESS: JxlEncoderStatus = 0;
const NEED_MORE_OUTPUT: JxlEncoderStatus = 2;
const EFFORT: i32 = 0;

unsafe extern "C" {
    fn JxlEncoderCreate(memory_manager: *const c_void) -> *mut c_void;
    fn JxlEncoderDestroy(enc: *mut c_void);
    fn JxlEncoderUseContainer(enc: *mut c_void, use_container: i32) -> JxlEncoderStatus;
    fn JxlEncoderStoreJPEGMetadata(enc: *mut c_void, store: i32) -> JxlEncoderStatus;
    fn JxlEncoderFrameSettingsCreate(enc: *mut c_void, source: *const c_void) -> *mut c_void;
    fn JxlEncoderFrameSettingsSetOption(settings: *mut c_void, option: i32, value: i64) -> JxlEncoderStatus;
    fn JxlEncoderAddJPEGFrame(settings: *const c_void, buffer: *const u8, size: usize) -> JxlEncoderStatus;
    fn JxlEncoderCloseInput(enc: *mut c_void);
    fn JxlEncoderProcessOutput(enc: *mut c_void, next_out: *mut *mut u8, avail_out: *mut usize) -> JxlEncoderStatus;
    fn JxlEncoderSetParallelRunner(enc: *mut c_void, runner: *const c_void, opaque: *mut c_void) -> JxlEncoderStatus;
    fn JxlThreadParallelRunner(
        opaque: *mut c_void,
        jpegxl_opaque: *mut c_void,
        init: *const c_void,
        func: *const c_void,
        start: u32,
        end: u32,
    ) -> i32;
    fn JxlThreadParallelRunnerCreate(memory_manager: *const c_void, num_worker_threads: usize) -> *mut c_void;
    fn JxlThreadParallelRunnerDestroy(runner_opaque: *mut c_void);
    fn JxlThreadParallelRunnerDefaultNumWorkerThreads() -> usize;
    fn JxlEncoderInitBasicInfo(info: *mut BasicInfo);
    fn JxlEncoderSetBasicInfo(enc: *mut c_void, info: *const BasicInfo) -> JxlEncoderStatus;
    fn JxlColorEncodingSetToSRGB(color: *mut ColorEncoding, is_gray: i32);
    fn JxlEncoderSetColorEncoding(enc: *mut c_void, color: *const ColorEncoding) -> JxlEncoderStatus;
    fn JxlEncoderSetFrameLossless(settings: *mut c_void, lossless: i32) -> JxlEncoderStatus;
    fn JxlEncoderAddImageFrame(
        settings: *const c_void,
        format: *const PixelFormat,
        buffer: *const c_void,
        size: usize,
    ) -> JxlEncoderStatus;
}

/// `JxlBasicInfo`, field for field.
#[repr(C)]
struct BasicInfo {
    have_container: i32,
    xsize: u32,
    ysize: u32,
    bits_per_sample: u32,
    exponent_bits_per_sample: u32,
    intensity_target: f32,
    min_nits: f32,
    relative_to_max_display: i32,
    linear_below: f32,
    uses_original_profile: i32,
    have_preview: i32,
    have_animation: i32,
    orientation: i32,
    num_color_channels: u32,
    num_extra_channels: u32,
    alpha_bits: u32,
    alpha_exponent_bits: u32,
    alpha_premultiplied: i32,
    preview: [u32; 2],
    animation: [u32; 4],
    intrinsic_xsize: u32,
    intrinsic_ysize: u32,
    padding: [u8; 100],
}

/// `JxlColorEncoding`, only ever filled by libjxl: room to spare, double-aligned.
#[repr(C, align(8))]
struct ColorEncoding([u8; 256]);

/// `JxlPixelFormat`.
#[repr(C)]
struct PixelFormat {
    num_channels: u32,
    data_type: i32,
    endianness: i32,
    align: usize,
}
const TYPE_UINT8: i32 = 2;

struct Encoder {
    enc: *mut c_void,
    runner: *mut c_void,
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: both were created by libjxl and are used by nothing after this.
        unsafe {
            JxlEncoderDestroy(self.enc);
            if !self.runner.is_null() {
                JxlThreadParallelRunnerDestroy(self.runner);
            }
        }
    }
}

impl Encoder {
    fn new() -> Option<Self> {
        // SAFETY: plain libjxl calls; the encoder and runner are owned by the returned value.
        unsafe {
            let enc = JxlEncoderCreate(std::ptr::null());
            if enc.is_null() {
                return None;
            }
            let threads = JxlThreadParallelRunnerDefaultNumWorkerThreads().min(4);
            let e = Encoder { enc, runner: JxlThreadParallelRunnerCreate(std::ptr::null(), threads) };
            if !e.runner.is_null() {
                JxlEncoderSetParallelRunner(e.enc, JxlThreadParallelRunner as *const c_void, e.runner);
            }
            Some(e)
        }
    }

    fn settings(&self, effort: i64) -> Option<*mut c_void> {
        // SAFETY: the encoder is live; libjxl owns the settings it hands back.
        unsafe {
            let settings = JxlEncoderFrameSettingsCreate(self.enc, std::ptr::null());
            (!settings.is_null()).then(|| {
                JxlEncoderFrameSettingsSetOption(settings, EFFORT, effort);
                settings
            })
        }
    }

    /// Closes the input and collects the encoded file.
    fn finish(self, hint: usize) -> Option<Vec<u8>> {
        // SAFETY: each call writes only within `out`'s length, which grows between calls.
        unsafe {
            JxlEncoderCloseInput(self.enc);
            let mut out = vec![0u8; hint.max(4096)];
            let mut written = 0usize;
            loop {
                let mut next = out.as_mut_ptr().add(written);
                let mut avail = out.len() - written;
                let status = JxlEncoderProcessOutput(self.enc, &mut next, &mut avail);
                written = out.len() - avail;
                match status {
                    SUCCESS => break,
                    NEED_MORE_OUTPUT => out.resize(out.len() * 2, 0),
                    _ => return None,
                }
            }
            out.truncate(written);
            Some(out)
        }
    }
}

/// A JPEG as JPEG XL that reconstructs it exactly. None if libjxl refuses it.
pub fn repack_jpeg(jpeg: &[u8]) -> Option<Vec<u8>> {
    let e = Encoder::new()?;
    // SAFETY: plain libjxl calls on a live encoder; `jpeg` outlives the call that reads it.
    unsafe {
        if JxlEncoderUseContainer(e.enc, 1) != SUCCESS || JxlEncoderStoreJPEGMetadata(e.enc, 1) != SUCCESS {
            return None;
        }
        let settings = e.settings(7)?;
        if JxlEncoderAddJPEGFrame(settings, jpeg.as_ptr(), jpeg.len()) != SUCCESS {
            return None;
        }
    }
    e.finish(jpeg.len() / 2)
}

/// 8-bit sRGB pixels (RGB or RGBA) as lossless JPEG XL.
pub fn lossless(pixels: &[u8], width: u32, height: u32, alpha: bool) -> Option<Vec<u8>> {
    let channels = if alpha { 4 } else { 3 };
    if pixels.len() != width as usize * height as usize * channels as usize {
        return None;
    }
    let e = Encoder::new()?;
    // SAFETY: the structs are libjxl's own layouts, initialised by libjxl before use; `pixels`
    // outlives the call that copies it.
    unsafe {
        let mut info = std::mem::MaybeUninit::<BasicInfo>::zeroed();
        JxlEncoderInitBasicInfo(info.as_mut_ptr());
        let mut info = info.assume_init();
        info.xsize = width;
        info.ysize = height;
        info.bits_per_sample = 8;
        info.uses_original_profile = 1;
        info.num_color_channels = 3;
        if alpha {
            info.num_extra_channels = 1;
            info.alpha_bits = 8;
        }
        if JxlEncoderSetBasicInfo(e.enc, &info) != SUCCESS {
            return None;
        }
        let mut color = ColorEncoding([0; 256]);
        JxlColorEncodingSetToSRGB(&mut color, 0);
        if JxlEncoderSetColorEncoding(e.enc, &color) != SUCCESS {
            return None;
        }
        // Effort 5: a 2560px screenshot in about 0.2 s; 7 costs half as much again for 3-4% more.
        let settings = e.settings(5)?;
        if JxlEncoderSetFrameLossless(settings, 1) != SUCCESS {
            return None;
        }
        let format = PixelFormat { num_channels: channels, data_type: TYPE_UINT8, endianness: 0, align: 0 };
        if JxlEncoderAddImageFrame(settings, &format, pixels.as_ptr().cast(), pixels.len()) != SUCCESS {
            return None;
        }
    }
    e.finish(pixels.len() / 8)
}

/// A still PNG with no metadata to keep, as lossless JPEG XL.
fn from_png(png: &[u8]) -> Option<Vec<u8>> {
    if vector_core::crypto::animated_format(png).is_some()
        || crate::shared::image::image_bytes_have_metadata(png, "png")
    {
        return None;
    }
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
    let (w, h) = (img.width(), img.height());
    if img.color().has_alpha() {
        lossless(img.into_rgba8().as_raw(), w, h, true)
    } else {
        lossless(img.into_rgb8().as_raw(), w, h, false)
    }
}

/// The picture as it leaves this client: a JPEG or still PNG goes as its smaller, lossless JPEG XL.
pub fn outbound(mut pc: CachedCompressedImage) -> CachedCompressedImage {
    if pc.bytes.is_empty() {
        return pc;
    }
    let jxl = match pc.extension.as_str() {
        "jpg" | "jpeg" => repack_jpeg(&pc.bytes),
        "png" => from_png(&pc.bytes),
        _ => None,
    };
    if let Some(jxl) = jxl.filter(|j| j.len() < pc.bytes.len()) {
        pc.compressed_size = jxl.len() as u64;
        pc.bytes = Arc::new(jxl);
        pc.extension = "jxl".into();
    }
    pc
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_png_goes_lossless_with_its_alpha() {
        let img = image::RgbaImage::from_fn(300, 200, |x, y| image::Rgba([(x % 256) as u8, (y % 256) as u8, 40, ((x + y) % 256) as u8]));
        let jxl = super::lossless(img.as_raw(), 300, 200, true).expect("encodes");
        jxl_oxide::integration::register_image_decoding_hook();
        let back = image::ImageReader::new(std::io::Cursor::new(&jxl)).with_guessed_format().unwrap().decode().unwrap().to_rgba8();
        assert_eq!(back.as_raw(), img.as_raw());
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        assert!(super::from_png(&png).is_some_and(|j| j.len() < png.len()));
    }

    #[test]
    fn a_jpeg_repacks_smaller_and_decodes_to_the_same_pixels() {
        let img = image::RgbImage::from_fn(320, 240, |x, y| image::Rgb([(x % 256) as u8, (y * 2 % 256) as u8, ((x ^ y) % 256) as u8]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85)
            .encode_image(&img)
            .unwrap();
        let jxl = super::repack_jpeg(&jpeg).expect("repacks");
        assert!(jxl.len() < jpeg.len(), "{} >= {}", jxl.len(), jpeg.len());
        jxl_oxide::integration::register_image_decoding_hook();
        let back = image::ImageReader::new(std::io::Cursor::new(&jxl)).with_guessed_format().unwrap().decode().unwrap().to_rgb8();
        let want = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert_eq!(back.dimensions(), want.dimensions());
        let diffs: Vec<i32> = back.as_raw().iter().zip(want.as_raw()).map(|(a, b)| (*a as i32 - *b as i32).abs()).collect();
        let (max, mean) = (*diffs.iter().max().unwrap(), diffs.iter().sum::<i32>() as f64 / diffs.len() as f64);
        // Two decoders' IDCTs of the same coefficients: rounding is all that may differ.
        assert!(max <= 8 && mean < 1., "max {max}, mean {mean}");
    }
}
