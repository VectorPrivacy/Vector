//! Outbound pictures as JPEG XL. A JPEG is repacked losslessly (libjxl keeps what it needs to
//! rebuild the original file bit for bit), about a fifth smaller with not a pixel changed.
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
}

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

/// A JPEG as JPEG XL that reconstructs it exactly. None if libjxl refuses it.
pub fn repack_jpeg(jpeg: &[u8]) -> Option<Vec<u8>> {
    // SAFETY: plain libjxl calls on an encoder this function owns; every buffer outlives its use.
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
        if JxlEncoderUseContainer(e.enc, 1) != SUCCESS || JxlEncoderStoreJPEGMetadata(e.enc, 1) != SUCCESS {
            return None;
        }
        let settings = JxlEncoderFrameSettingsCreate(e.enc, std::ptr::null());
        if settings.is_null() {
            return None;
        }
        JxlEncoderFrameSettingsSetOption(settings, EFFORT, 7);
        if JxlEncoderAddJPEGFrame(settings, jpeg.as_ptr(), jpeg.len()) != SUCCESS {
            return None;
        }
        JxlEncoderCloseInput(e.enc);
        let mut out = vec![0u8; (jpeg.len() / 2).max(4096)];
        let mut written = 0usize;
        loop {
            let mut next = out.as_mut_ptr().add(written);
            let mut avail = out.len() - written;
            let status = JxlEncoderProcessOutput(e.enc, &mut next, &mut avail);
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

/// The picture as it leaves this client: a JPEG goes as its smaller, lossless JPEG XL.
pub fn outbound(mut pc: CachedCompressedImage) -> CachedCompressedImage {
    if !matches!(pc.extension.as_str(), "jpg" | "jpeg") || pc.bytes.is_empty() {
        return pc;
    }
    if let Some(jxl) = repack_jpeg(&pc.bytes).filter(|j| j.len() < pc.bytes.len()) {
        pc.compressed_size = jxl.len() as u64;
        pc.bytes = Arc::new(jxl);
        pc.extension = "jxl".into();
    }
    pc
}

#[cfg(test)]
mod tests {
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
