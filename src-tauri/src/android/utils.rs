use jni::{JavaVM, JNIEnv};
use jni::objects::{GlobalRef, JObject};
use ndk_context::AndroidContext;
use std::sync::{Mutex, OnceLock};

/// The live Activity, for calls an Application context rejects: the ndk context holds the Application.
static ACTIVITY: Mutex<Option<GlobalRef>> = Mutex::new(None);
static ACTIVITY_VM: OnceLock<JavaVM> = OnceLock::new();

/// Called from MainActivity.onCreate; a recreated Activity replaces the old one.
pub fn set_activity(env: &mut JNIEnv<'_>, activity: &JObject<'_>) {
    if ACTIVITY_VM.get().is_none() {
        match env.get_java_vm() {
            Ok(vm) => { let _ = ACTIVITY_VM.set(vm); }
            Err(e) => { vector_core::log_warn!("[JNI] set_activity: no JavaVM: {:?}", e); return; }
        }
    }
    match env.new_global_ref(activity) {
        Ok(global) => { if let Ok(mut slot) = ACTIVITY.lock() { *slot = Some(global); } }
        Err(e) => vector_core::log_warn!("[JNI] set_activity: no global ref: {:?}", e),
    }
}

/// Called from MainActivity.onDestroy. Only drops the Activity it was given: a recreated one may
/// already have taken the slot.
pub fn clear_activity(env: &mut JNIEnv<'_>, activity: &JObject<'_>) {
    if let Ok(mut slot) = ACTIVITY.lock() {
        if slot.as_ref().is_some_and(|held| env.is_same_object(held.as_obj(), activity).unwrap_or(false)) {
            *slot = None;
        }
    }
}

/// `ndk_context::android_context()` without the abort: the upstream getter
/// PANICS before tao registers the context, and tao 0.35 registers LATER in
/// startup than 0.34 did — so an early thread in either process mode could
/// take the whole app down with it. A probe that answers None lets every
/// caller fail soft (their `Result` surface already exists) and retry once
/// the context lands.
fn try_android_context() -> Option<AndroidContext> {
    std::panic::catch_unwind(ndk_context::android_context).ok()
}

/// Whether the ndk context has been registered yet. Gate for code whose
/// DEPENDENCIES call `ndk_context::android_context()` unguarded (iroh's
/// hickory-resolver reads system DNS with it and the panic aborts the
/// process): they must not run before registration, and never in the
/// Activity-less service-only process, where it never comes.
pub fn context_registered() -> bool {
    try_android_context().is_some()
}

/// Unwind fence for JNI entry points. A Rust panic crossing an `extern "C"`
/// boundary is a process abort; inside the fence it becomes a logged default
/// instead. The default must be safe for the Java caller (unit, or a null
/// object the Kotlin side treats as failure).
pub fn jni_fence<T>(name: &str, default: impl FnOnce() -> T, body: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(v) => v,
        Err(_) => {
            vector_core::log_warn!("[JNI] {} panicked — returned default instead of aborting", name);
            default()
        }
    }
}

/// Standard buffer size for reading streams
pub const STREAM_BUFFER_SIZE: i32 = 8192;

/// Execute a function with an Android JNI context.
///
/// Prefers the background service's stored VM + application context, which is
/// registered whenever the foreground service starts (full-app AND service-only
/// modes) and stays valid in the swiped-off, Activity-less process. `ndk_context`
/// is only a fallback for the brief early-startup window before the service
/// registers its context: `ndk_context::android_context()` PANICS when there is
/// no Activity, so reaching it in service-only mode aborts the calling thread.
///
/// Every caller here is context-agnostic (MediaScanner, ContentResolver, system
/// services, clipboard) or launches with FLAG_ACTIVITY_NEW_TASK, so the
/// application context behaves identically to the Activity context.
pub fn with_android_context<F, R>(f: F) -> Result<R, String>
where
    F: for<'a> FnOnce(&mut JNIEnv<'a>, &JObject<'a>) -> Result<R, String>,
{
    if let (Some(vm), Some(ctx)) = (
        crate::android::background_sync::BG_JAVA_VM.get(),
        crate::android::background_sync::BG_APP_CONTEXT.get(),
    ) {
        let mut env = vm
            .attach_current_thread()
            .map_err(|e| format!("Failed to attach thread (bg context): {:?}", e))?;
        let out = f(&mut env, ctx.as_obj());
        clear_pending_exception(&mut env, &out);
        return out;
    }

    // Fallback: Activity context (only present once tao has registered it).
    let ctx = try_android_context()
        .ok_or("Android context not registered yet (early startup or service-only mode)")?;
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
        .map_err(|e| format!("Failed to get JavaVM: {:?}", e))?;

    let mut env = vm.attach_current_thread()
        .map_err(|e| format!("Failed to attach thread: {:?}", e))?;

    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };

    let out = f(&mut env, &activity);
    clear_pending_exception(&mut env, &out);
    out
}

/// A failed JNI call leaves its Java exception PENDING on the thread; left
/// there, the VM aborts the whole process at detach instead of letting the
/// Err surface (a stripped method becomes a crash rather than a message).
fn clear_pending_exception<R>(env: &mut JNIEnv<'_>, out: &Result<R, String>) {
    if out.is_err() && env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
}

/// Execute a function with the live Activity.
///
/// Unlike `with_android_context`, this never substitutes an Application context: Activity-only
/// APIs (`requestPermissions`, `startActivityForResult`, window flags) throw on one, so those
/// callers MUST use this. Fails soft when no Activity exists (the service-only process, or after
/// the Activity is destroyed).
pub fn with_android_activity<F, R>(f: F) -> Result<R, String>
where
    F: for<'a> FnOnce(&mut JNIEnv<'a>, &JObject<'a>) -> Result<R, String>,
{
    let activity = ACTIVITY
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .ok_or("No Activity (service-only process, or not created yet)")?;
    let vm = ACTIVITY_VM.get().ok_or("No JavaVM for the Activity")?;
    let mut env = vm.attach_current_thread()
        .map_err(|e| format!("Failed to attach thread: {:?}", e))?;

    let out = f(&mut env, activity.as_obj());
    clear_pending_exception(&mut env, &out);
    out
}

/// Get the ContentResolver
pub fn get_content_resolver<'a>(env: &mut JNIEnv<'a>, activity: &JObject<'a>) -> Result<JObject<'a>, String> {
    env.call_method(
        activity,
        "getContentResolver",
        "()Landroid/content/ContentResolver;",
        &[],
    )
    .map_err(|e| format!("Failed to get ContentResolver: {:?}", e))?
    .l()
    .map_err(|e| format!("Failed to convert ContentResolver object: {:?}", e))
}