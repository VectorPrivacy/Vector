//! The camera, on its own thread: each frame it gives is turned into I420 at once, from
//! whichever format the device speaks, and handed to the link worker. On Android the shell's
//! Camera2 feed stands in for nokhwa.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
#[cfg(not(target_os = "android"))]
use std::time::Duration;

#[cfg(not(target_os = "android"))]
use nokhwa::pixel_format::RgbFormat;
#[cfg(not(target_os = "android"))]
use nokhwa::utils::{CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution};
use vector_core::calls::wire::VideoKind;

#[cfg(not(target_os = "android"))]
use super::yuv::I420;
use super::In;
#[cfg(not(target_os = "android"))]
use super::clock_us;

pub struct Capture {
    pub(super) stop: Arc<AtomicBool>,
    /// What else ends the source, where a flag alone doesn't.
    pub(super) release: Option<Box<dyn FnOnce() + Send>>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

/// The camera a call shows: the one facing the user, where a phone has both.
pub fn open(kind: VideoKind, to: Sender<In>) -> Result<Capture, String> {
    open_facing(kind, true, to)
}

/// The camera facing away, for reading a code held in front of it.
pub fn open_back(kind: VideoKind, to: Sender<In>) -> Result<Capture, String> {
    open_facing(kind, false, to)
}

#[cfg(target_os = "android")]
fn open_facing(kind: VideoKind, front: bool, to: Sender<In>) -> Result<Capture, String> {
    android::open(kind, front, to)
}

/// macOS asks once for the camera; the other systems gate it in their own settings.
#[cfg(not(target_os = "android"))]
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
            Ok(false) => Err(crate::t::call_camera_blocked().into()),
            Err(_) => Err(crate::t::call_camera_silent().into()),
        }
    }
    #[cfg(not(target_os = "macos"))]
    Ok(())
}

#[cfg(not(target_os = "android"))]
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
#[cfg(not(target_os = "android"))]
fn open_facing(kind: VideoKind, _front: bool, to: Sender<In>) -> Result<Capture, String> {
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
                    let _ = ready_tx.send(Err(crate::t::call_camera_open_failed().into()));
                    return;
                }
            };
            if let Err(e) = cam.open_stream() {
                log::warn!("camera stream: {e}");
                let _ = ready_tx.send(Err(crate::t::call_camera_open_failed().into()));
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
        Ok(Ok(())) => Ok(Capture { stop, release: None }),
        Ok(Err(e)) => Err(e),
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            Err(crate::t::call_camera_silent().into())
        }
    }
}

/// Camera2 through the shell's `CameraFeed`: frames arrive on its camera thread with their
/// planes borrowed, are copied upright into I420 there, and go to the feed's sender.
#[cfg(target_os = "android")]
mod android {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
    use std::sync::mpsc::Sender;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    use jni::JNIEnv;
    use jni::objects::{JByteBuffer, JClass, JObject, JValue};
    use jni::sys::{jboolean, jint, jlong};
    use vector_core::calls::wire::VideoKind;

    use super::super::yuv::I420;
    use super::super::{In, clock_us};
    use super::Capture;
    use crate::android::utils::{with_android_activity, with_android_context};

    const PERMISSION: &str = "android.permission.CAMERA";
    const PERMISSION_REQUEST: jint = 9877;

    static NEXT: AtomicI64 = AtomicI64::new(1);
    static FEEDS: Mutex<Option<HashMap<i64, (VideoKind, Sender<In>)>>> = Mutex::new(None);
    static ANSWER: Mutex<Option<Arc<(Mutex<Option<bool>>, Condvar)>>> = Mutex::new(None);

    fn feeds<R>(f: impl FnOnce(&mut HashMap<i64, (VideoKind, Sender<In>)>) -> R) -> R {
        let mut g = FEEDS.lock().unwrap_or_else(|e| e.into_inner());
        f(g.get_or_insert_with(HashMap::new))
    }

    fn err(e: jni::errors::Error) -> String {
        format!("{e:?}")
    }

    fn feed_class<'a>(env: &mut JNIEnv<'a>, ctx: &JObject<'a>) -> Result<JClass<'a>, String> {
        let loader = env.call_method(ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[]).and_then(|v| v.l()).map_err(err)?;
        let name = env.new_string("io.vectorapp.gpui.CameraFeed").map_err(err)?;
        let class = env
            .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])
            .and_then(|v| v.l())
            .map_err(err)?;
        Ok(JClass::from(class))
    }

    fn granted() -> bool {
        with_android_context(|env, ctx| {
            let name = env.new_string(PERMISSION).map_err(err)?;
            let status = env.call_method(ctx, "checkSelfPermission", "(Ljava/lang/String;)I", &[JValue::Object(&name)]).and_then(|v| v.i()).map_err(err)?;
            Ok(status == 0)
        })
        .unwrap_or(false)
    }

    /// Asks once if not yet allowed, and waits for the answer.
    fn permitted() -> Result<(), String> {
        if granted() {
            return Ok(());
        }
        let answer = Arc::new((Mutex::new(None), Condvar::new()));
        *ANSWER.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&answer));
        with_android_activity(|env, activity| {
            let name = env.new_string(PERMISSION).map_err(err)?;
            let list = env.new_object_array(1, "java/lang/String", JObject::from(name)).map_err(err)?;
            env.call_method(activity, "requestPermissions", "([Ljava/lang/String;I)V", &[JValue::Object(&JObject::from(list)), JValue::Int(PERMISSION_REQUEST)])
                .map_err(err)?;
            Ok(())
        })
        .map_err(|e| {
            log::warn!("camera permission: {e}");
            String::from(crate::t::call_camera_blocked())
        })?;
        let (lock, cvar) = &*answer;
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let (guard, _) = cvar.wait_timeout_while(guard, Duration::from_secs(120), |a| a.is_none()).unwrap_or_else(|e| e.into_inner());
        match *guard {
            Some(true) => Ok(()),
            Some(false) => Err(crate::t::call_camera_blocked().into()),
            None => Err(crate::t::call_camera_silent().into()),
        }
    }

    pub fn open(kind: VideoKind, front: bool, to: Sender<In>) -> Result<Capture, String> {
        permitted()?;
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        feeds(|f| f.insert(id, (kind, to)));
        let started = with_android_context(|env, ctx| {
            let class = feed_class(env, ctx)?;
            env.call_static_method(
                &class,
                "start",
                "(Landroid/content/Context;JZII)Z",
                &[JValue::Object(ctx), JValue::Long(id), JValue::Bool(front.into()), JValue::Int(1280), JValue::Int(720)],
            )
            .and_then(|v| v.z())
            .map_err(err)
        });
        if !matches!(started, Ok(true)) {
            if let Err(e) = started {
                log::warn!("camera: {e}");
            }
            feeds(|f| f.remove(&id));
            return Err(crate::t::call_camera_open_failed().into());
        }
        let release = move || {
            feeds(|f| f.remove(&id));
            let _ = with_android_context(|env, ctx| {
                let class = feed_class(env, ctx)?;
                env.call_static_method(&class, "stop", "(J)V", &[JValue::Long(id)]).map(drop).map_err(err)
            });
        };
        Ok(Capture { stop: Arc::new(AtomicBool::new(false)), release: Some(Box::new(release)) })
    }

    /// A plane's bytes, borrowed for as long as the shell holds its image.
    fn plane<'a>(env: &JNIEnv<'a>, buf: &JByteBuffer<'a>) -> Option<&'a [u8]> {
        let ptr = env.get_direct_buffer_address(buf).ok()?;
        let len = env.get_direct_buffer_capacity(buf).ok()?;
        // SAFETY: a direct buffer of the Image the shell closes only after this call returns.
        (!ptr.is_null()).then(|| unsafe { std::slice::from_raw_parts(ptr, len) })
    }

    #[unsafe(no_mangle)]
    #[allow(non_snake_case, clippy::too_many_arguments)]
    pub extern "system" fn Java_io_vectorapp_gpui_CameraFeed_nativeFrame<'a>(
        env: JNIEnv<'a>,
        _class: JClass<'a>,
        id: jlong,
        y: JByteBuffer<'a>,
        u: JByteBuffer<'a>,
        v: JByteBuffer<'a>,
        y_row: jint,
        uv_row: jint,
        uv_pixel: jint,
        width: jint,
        height: jint,
        rotation: jint,
    ) {
        let Some((kind, to)) = feeds(|f| f.get(&id).cloned()) else { return };
        let (Some(yb), Some(ub), Some(vb)) = (plane(&env, &y), plane(&env, &u), plane(&env, &v)) else { return };
        let ts_us = clock_us();
        let Some(pic) = I420::from_planes(yb, ub, vb, y_row.max(0) as usize, uv_row.max(0) as usize, uv_pixel.max(0) as usize, width.max(0) as usize, height.max(0) as usize) else { return };
        let pic = pic.turned(rotation);
        if to.send(In::Frame { kind, pic, ts_us }).is_err() {
            feeds(|f| f.remove(&id));
        }
    }

    #[unsafe(no_mangle)]
    #[allow(non_snake_case)]
    pub extern "system" fn Java_io_vectorapp_gpui_CameraFeed_nativeLost<'a>(_env: JNIEnv<'a>, _class: JClass<'a>, id: jlong) {
        if let Some((kind, to)) = feeds(|f| f.remove(&id)) {
            log::warn!("camera lost");
            let _ = to.send(In::SourceLost(kind));
        }
    }

    #[unsafe(no_mangle)]
    #[allow(non_snake_case)]
    pub extern "system" fn Java_io_vectorapp_gpui_CameraFeed_nativePermission<'a>(_env: JNIEnv<'a>, _class: JClass<'a>, granted: jboolean) {
        if let Some(answer) = ANSWER.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let (lock, cvar) = &*answer;
            *lock.lock().unwrap_or_else(|e| e.into_inner()) = Some(granted != 0);
            cvar.notify_all();
        }
    }
}
