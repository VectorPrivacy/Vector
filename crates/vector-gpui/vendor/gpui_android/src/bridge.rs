//! The JNI edge. The Kotlin bridge calls in from the UI thread; each call becomes an [`Event`]
//! queued for GPUI's thread, which an eventfd wakes. GPUI calls out through the bridge's
//! static methods, from its own thread, attached to the VM for good.

use jni::objects::{GlobalRef, JClass, JObject, JString};
use jni::sys::{jboolean, jfloat, jint, jlong};
use jni::{JNIEnv, JavaVM};
use ndk::native_window::NativeWindow;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

pub(crate) enum Event {
    SurfaceCreated(NativeWindow),
    SurfaceChanged { width: i32, height: i32 },
    /// Answered once nothing draws into the surface any more.
    SurfaceDestroyed(mpsc::SyncSender<()>),
    /// Device pixels: top, right, bottom, left, then the keyboard's height.
    Insets { safe: [f32; 4], ime: f32 },
    Configuration { density: f32, dark: bool },
    Touch { id: i32, phase: i32, x: f32, y: f32, pressure: f32, time: Instant },
    Key { down: bool, key_code: i32, unicode: u32, base: u32, meta: i32, repeat: i32 },
    Lifecycle(i32),
    Focus(bool),
    Back,
    TrimMemory(i32),
    OpenUrl(String),
    /// The keyboard's view of the focused field after an edit, in UTF-16 offsets; a
    /// composing region of -1 is none. `serial` counts the keyboard's edits.
    ImeState { text: String, selection: (i32, i32), composing: (i32, i32), serial: i32 },
    /// The keyboard's action key (send, done, go…): GPUI's fields all take it as Enter.
    ImeAction,
}

/// Activity lifecycle as the bridge numbers it.
pub(crate) mod lifecycle {
    pub const STARTED: i32 = 1;
    pub const RESUMED: i32 = 2;
    pub const PAUSED: i32 = 3;
    pub const STOPPED: i32 = 4;
    pub const DESTROYED: i32 = 5;
}

struct Bridge {
    vm: JavaVM,
    class: GlobalRef,
    /// Also what `ndk_context` hands out, so it must never be released.
    context: GlobalRef,
}

static BRIDGE: OnceLock<Bridge> = OnceLock::new();
static WAKE: OnceLock<OwnedFd> = OnceLock::new();
static QUEUE: Mutex<Vec<Event>> = Mutex::new(Vec::new());

unsafe extern "Rust" {
    /// The app's entry point, defined by the app with `#[unsafe(no_mangle)]`.
    fn gpui_android_main();
}

pub fn java_vm() -> Option<&'static JavaVM> {
    BRIDGE.get().map(|b| &b.vm)
}

/// The application `Context`, alive for the whole process.
pub fn app_context() -> Option<&'static GlobalRef> {
    BRIDGE.get().map(|b| &b.context)
}

/// Runs `f` with this thread's JNI environment and the bridge class. A Java exception is
/// logged and cleared, and the call answers `None`.
pub fn with_env<R>(
    f: impl for<'a> FnOnce(&mut JNIEnv<'a>, &JClass<'a>) -> jni::errors::Result<R>,
) -> Option<R> {
    let bridge = BRIDGE.get()?;
    let mut env = bridge.vm.attach_current_thread_permanently().ok()?;
    let class = <&JClass>::from(bridge.class.as_obj());
    match f(&mut env, class) {
        Ok(value) => Some(value),
        Err(error) => {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_describe();
                let _ = env.exception_clear();
            }
            log::error!("gpui_android: bridge call failed: {error}");
            None
        }
    }
}

pub(crate) fn wake_fd() -> RawFd {
    WAKE.get().expect("the bridge starts before GPUI runs").as_raw_fd()
}

pub(crate) fn take_events() -> Vec<Event> {
    std::mem::take(&mut *QUEUE.lock().unwrap_or_else(|e| e.into_inner()))
}

pub(crate) fn clear_wake(fd: RawFd) {
    let mut value = 0u64;
    // SAFETY: an eventfd read takes exactly eight bytes into a valid buffer.
    unsafe { libc::read(fd, (&raw mut value).cast(), 8) };
}

pub(crate) fn signal(fd: RawFd) {
    let value = 1u64;
    // SAFETY: an eventfd write takes exactly eight bytes from a valid buffer.
    unsafe { libc::write(fd, (&raw const value).cast(), 8) };
}

pub(crate) fn new_eventfd() -> OwnedFd {
    // SAFETY: eventfd returns a new descriptor this process owns, or -1.
    let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
    assert!(fd >= 0, "eventfd: {}", std::io::Error::last_os_error());
    // SAFETY: fd was just created and is owned by nothing else.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

fn push(event: Event) {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner()).push(event);
    if let Some(fd) = WAKE.get() {
        signal(fd.as_raw_fd());
    }
}

fn string(env: &mut JNIEnv, value: &JString) -> String {
    env.get_string(value).map(Into::into).unwrap_or_default()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeStart<'l>(
    env: JNIEnv<'l>,
    class: JClass<'l>,
    context: JObject<'l>,
) {
    if BRIDGE.get().is_some() {
        return;
    }
    let (Ok(vm), Ok(class), Ok(context)) = (
        env.get_java_vm(),
        env.new_global_ref(&class),
        env.new_global_ref(&context),
    ) else {
        log::error!("gpui_android: the bridge could not start");
        return;
    };
    // SAFETY: both pointers stay valid for the process: the VM, and a global reference
    // to the application context the bridge never releases.
    unsafe {
        ndk_context::initialize_android_context(
            vm.get_java_vm_pointer().cast(),
            context.as_obj().as_raw().cast(),
        )
    };
    let _ = WAKE.set(new_eventfd());
    if BRIDGE.set(Bridge { vm, class, context }).is_err() {
        return;
    }
    std::thread::Builder::new()
        .name("gpui-main".into())
        .spawn(|| {
            if let Some(vm) = java_vm() {
                let _ = vm.attach_current_thread_permanently();
            }
            // SAFETY: the app defines this entry point; it takes nothing and returns nothing.
            unsafe { gpui_android_main() };
            // GPUI starts once per process, so a relaunch after quitting needs a fresh one.
            std::process::exit(0);
        })
        .expect("spawn the GPUI thread");
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeSurfaceCreated<'l>(
    env: JNIEnv<'l>,
    _class: JClass<'l>,
    surface: JObject<'l>,
) {
    // SAFETY: `surface` is a live android.view.Surface for the length of this call; the
    // returned window holds its own reference.
    if let Some(window) = unsafe { NativeWindow::from_surface(env.get_raw().cast(), surface.as_raw().cast()) } {
        push(Event::SurfaceCreated(window));
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeSurfaceChanged<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
    width: jint,
    height: jint,
) {
    push(Event::SurfaceChanged { width, height });
}

/// Blocks the UI thread until GPUI has let go of the surface, as `surfaceDestroyed` requires.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeSurfaceDestroyed<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
) {
    let (done, wait) = mpsc::sync_channel(1);
    push(Event::SurfaceDestroyed(done));
    if wait.recv_timeout(Duration::from_secs(2)).is_err() {
        log::warn!("gpui_android: the surface went before GPUI released it");
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeTouch<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
    id: jint,
    phase: jint,
    x: jfloat,
    y: jfloat,
    pressure: jfloat,
    uptime_nanos: jlong,
) {
    let time = Instant::now().checked_sub(since_uptime(uptime_nanos)).unwrap_or_else(Instant::now);
    push(Event::Touch { id, phase, x, y, pressure, time });
}

/// How long ago a `SystemClock.uptime` moment was: CLOCK_MONOTONIC, the clock `Instant` reads.
fn since_uptime(nanos: jlong) -> Duration {
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime writes one timespec into a valid buffer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    let now = now.tv_sec as i64 * 1_000_000_000 + now.tv_nsec as i64;
    Duration::from_nanos(now.saturating_sub(nanos).max(0) as u64)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeKey<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
    down: jboolean,
    key_code: jint,
    unicode: jint,
    base: jint,
    meta: jint,
    repeat: jint,
) {
    push(Event::Key {
        down: down != 0,
        key_code,
        unicode: unicode as u32,
        base: base as u32,
        meta,
        repeat,
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeInsets<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
    top: jfloat,
    right: jfloat,
    bottom: jfloat,
    left: jfloat,
    ime: jfloat,
) {
    push(Event::Insets { safe: [top, right, bottom, left], ime });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeConfiguration<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
    density: jfloat,
    dark: jboolean,
) {
    push(Event::Configuration { density, dark: dark != 0 });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeLifecycle<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
    phase: jint,
) {
    push(Event::Lifecycle(phase));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeFocus<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
    focused: jboolean,
) {
    push(Event::Focus(focused != 0));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeBack<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
) {
    push(Event::Back);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeTrimMemory<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
    level: jint,
) {
    push(Event::TrimMemory(level));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeOpenUrl<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    url: JString<'l>,
) {
    let url = string(&mut env, &url);
    if !url.is_empty() {
        push(Event::OpenUrl(url));
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeImeState<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    text: JString<'l>,
    selection_start: jint,
    selection_end: jint,
    composing_start: jint,
    composing_end: jint,
    serial: jint,
) {
    let text = string(&mut env, &text);
    push(Event::ImeState {
        text,
        selection: (selection_start, selection_end),
        composing: (composing_start, composing_end),
        serial,
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativeImeAction<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
    _action: jint,
) {
    push(Event::ImeAction);
}

/// Calls a static `void` method of the bridge.
pub(crate) fn call_void(name: &str, sig: &str, args: &[jni::objects::JValue]) {
    with_env(|env, class| env.call_static_method(class, name, sig, args).map(drop));
}

pub(crate) fn show_keyboard(show: bool) {
    call_void(if show { "showKeyboard" } else { "hideKeyboard" }, "()V", &[]);
}

pub(crate) fn set_back_enabled(enabled: bool) {
    call_void("setBackEnabled", "(Z)V", &[jni::objects::JValue::Bool(enabled.into())]);
}

pub(crate) fn finish() {
    call_void("finish", "()V", &[]);
}

pub(crate) fn open_url(url: &str) {
    with_env(|env, class| {
        let url = env.new_string(url)?;
        env.call_static_method(class, "openUrl", "(Ljava/lang/String;)V", &[(&url).into()])
            .map(drop)
    });
}

pub(crate) fn open_file(path: &std::path::Path) {
    with_env(|env, class| {
        let path = env.new_string(path.to_string_lossy())?;
        env.call_static_method(class, "openFile", "(Ljava/lang/String;)V", &[(&path).into()])
            .map(drop)
    });
}

pub(crate) fn set_clipboard(text: &str) {
    with_env(|env, class| {
        let text = env.new_string(text)?;
        env.call_static_method(class, "setClipboard", "(Ljava/lang/String;)V", &[(&text).into()])
            .map(drop)
    });
}

pub(crate) fn clipboard() -> Option<String> {
    with_env(|env, class| {
        let value = env
            .call_static_method(class, "getClipboard", "()Ljava/lang/String;", &[])?
            .l()?;
        if value.is_null() {
            return Ok(None);
        }
        let value = JString::from(value);
        Ok(Some(env.get_string(&value)?.into()))
    })
    .flatten()
}

/// The field the keyboard edits, as GPUI holds it: UTF-16 offsets, -1 for no composing region.
/// `serial` is the last keyboard edit GPUI has applied, so the keyboard can tell this state
/// from one that predates its newer edits.
pub(crate) fn set_text_state(text: &str, selection: (i32, i32), composing: (i32, i32), serial: i32) {
    with_env(|env, class| {
        let text = env.new_string(text)?;
        env.call_static_method(
            class,
            "setTextState",
            "(Ljava/lang/String;IIIII)V",
            &[
                (&text).into(),
                selection.0.into(),
                selection.1.into(),
                composing.0.into(),
                composing.1.into(),
                serial.into(),
            ],
        )
        .map(drop)
    });
}

/// The keyboard's options for the focused field, packed as `text_input` encodes them.
pub(crate) fn set_input_options(options: jint) {
    call_void("setInputOptions", "(I)V", &[options.into()]);
}

type Picked = futures::channel::oneshot::Sender<anyhow::Result<Option<Vec<std::path::PathBuf>>>>;

/// Pickers still open, by the id the bridge was handed.
static PICKS: Mutex<Vec<(jint, Picked)>> = Mutex::new(Vec::new());

/// The system's document picker. The chosen files arrive as copies in the app's cache, since
/// GPUI's callers read paths; `extensions` empty takes any file.
pub(crate) fn pick_files(multiple: bool, extensions: &[String], answer: Picked) {
    static NEXT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    PICKS.lock().unwrap_or_else(|e| e.into_inner()).push((id, answer));
    let asked = with_env(|env, class| {
        let list = env.new_object_array(extensions.len() as jint, "java/lang/String", JObject::null())?;
        for (i, ext) in extensions.iter().enumerate() {
            let ext = env.new_string(ext)?;
            env.set_object_array_element(&list, i as jint, ext)?;
        }
        env.call_static_method(
            class,
            "pickFiles",
            "(IZ[Ljava/lang/String;)V",
            &[id.into(), jni::objects::JValue::Bool(multiple.into()), (&list).into()],
        )
        .map(drop)
    });
    if asked.is_none() {
        answer_pick(id, Err(anyhow::anyhow!("The file picker could not open")));
    }
}

fn answer_pick(id: jint, result: anyhow::Result<Option<Vec<std::path::PathBuf>>>) {
    let mut picks = PICKS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(at) = picks.iter().position(|(i, _)| *i == id) {
        let _ = picks.swap_remove(at).1.send(result);
    }
}

/// The picker closed: `paths` null when cancelled.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_vectorapp_gpui_Bridge_nativePicked<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    id: jint,
    paths: jni::objects::JObjectArray<'l>,
) {
    if paths.is_null() {
        answer_pick(id, Ok(None));
        return;
    }
    let count = env.get_array_length(&paths).unwrap_or(0);
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let Ok(path) = env.get_object_array_element(&paths, i) else { continue };
        if let Ok(path) = env.get_string(&JString::from(path)) {
            out.push(std::path::PathBuf::from(String::from(path)));
        }
    }
    answer_pick(id, Ok(Some(out)));
}
