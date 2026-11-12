//! The camera, on its own thread: each frame it gives is turned into I420 at once, from
//! whichever format the device speaks, and handed to the link worker.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution};
use vector_core::calls::wire::VideoKind;

use super::yuv::I420;
use super::{In, clock_us};

pub struct Capture {
    stop: Arc<AtomicBool>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// macOS asks once for the camera; the other systems gate it in their own settings.
fn permitted() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if nokhwa::nokhwa_check() {
            return Ok(());
        }
        let (tx, rx) = std::sync::mpsc::channel();
        nokhwa::nokhwa_initialize(move |ok| {
            let _ = tx.send(ok);
        });
        match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(true) => Ok(()),
            Ok(false) => Err("Camera access is blocked in your system privacy settings".into()),
            Err(_) => Err("The camera never answered".into()),
        }
    }
    #[cfg(not(target_os = "macos"))]
    Ok(())
}

fn to_i420(frame: &nokhwa::Buffer) -> Option<I420> {
    let res = frame.resolution();
    let (w, h) = (res.width() as usize, res.height() as usize);
    let raw = frame.buffer();
    match frame.source_frame_format() {
        FrameFormat::YUYV => I420::from_yuyv(raw, w, h),
        FrameFormat::NV12 => I420::from_nv12(raw, w, h),
        FrameFormat::RAWRGB => I420::from_rgb(raw, w, h, w * 3, 3, false),
        FrameFormat::RAWBGR => I420::from_rgb(raw, w, h, w * 3, 3, true),
        FrameFormat::GRAY => {
            let mut pic = I420::new(w, h);
            let (pw, ph) = (pic.w, pic.h);
            for row in 0..ph {
                pic.data[row * pw..(row + 1) * pw].copy_from_slice(raw.get(row * w..row * w + pw)?);
            }
            pic.data[pw * ph..].fill(128);
            Some(pic)
        }
        FrameFormat::MJPEG => {
            let img = frame.decode_image::<RgbFormat>().ok()?;
            let (iw, ih) = (img.width() as usize, img.height() as usize);
            I420::from_rgb(img.as_raw(), iw, ih, iw * 3, 3, false)
        }
    }
}

/// Opens the first camera at 1280x720, 30 fps or the nearest it offers; the error is the
/// user's to read.
pub fn open(kind: VideoKind, to: Sender<In>) -> Result<Capture, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let flag = Arc::clone(&stop);
    std::thread::Builder::new()
        .name("camera".into())
        .spawn(move || {
            if let Err(e) = permitted() {
                let _ = ready_tx.send(Err(e));
                return;
            }
            let want = CameraFormat::new(Resolution::new(1280, 720), FrameFormat::MJPEG, 30);
            let req = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(want));
            let mut cam = match nokhwa::Camera::new(CameraIndex::Index(0), req) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("camera: {e}");
                    let _ = ready_tx.send(Err("Could not open the camera".into()));
                    return;
                }
            };
            if let Err(e) = cam.open_stream() {
                log::warn!("camera stream: {e}");
                let _ = ready_tx.send(Err("Could not open the camera".into()));
                return;
            }
            let _ = ready_tx.send(Ok(()));
            let mut failures = 0;
            while !flag.load(Ordering::Relaxed) {
                match cam.frame() {
                    Ok(frame) => {
                        failures = 0;
                        let ts_us = clock_us();
                        if let Some(pic) = to_i420(&frame)
                            && to.send(In::Frame { kind, pic, ts_us }).is_err()
                        {
                            break;
                        }
                    }
                    Err(e) => {
                        failures += 1;
                        if failures > 50 {
                            log::warn!("camera lost: {e}");
                            let _ = to.send(In::SourceLost(kind));
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
            }
            let _ = cam.stop_stream();
        })
        .map_err(|e| e.to_string())?;
    match ready_rx.recv_timeout(Duration::from_secs(150)) {
        Ok(Ok(())) => Ok(Capture { stop }),
        Ok(Err(e)) => Err(e),
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            Err("The camera never answered".into())
        }
    }
}
