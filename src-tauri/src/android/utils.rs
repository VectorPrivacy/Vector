use jni::{JavaVM, JNIEnv};
use jni::objects::JObject;
use ndk_context::AndroidContext;

/// `ndk_context::android_context()` without the abort: the upstream getter
/// PANICS before tao registers the context, and tao 0.35 registers LATER in
/// startup than 0.34 did — so an early thread in either process mode could
/// take the whole app down with it. A probe that answers None lets every
/// caller fail soft (their `Result` surface already exists) and retry once
/// the context lands.
fn try_android_context() -> Option<AndroidContext> {
    std::panic::catch_unwind(ndk_context::android_context).ok()
}

/// Whether tao has registered the ndk context yet. Gate for code whose
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

/// Execute a function with the Android **Activity** JNI context specifically.
///
/// Unlike `with_android_context`, this never substitutes the background
/// service's Application context — it always resolves the live Activity via
/// `ndk_context`. Activity-only APIs (`requestPermissions`,
/// `startActivityForResult`, ...) throw `NoSuchMethodError` on an Application
/// context, so those callers MUST use this. Only valid while an Activity
/// exists (any foreground, user-driven action qualifies); `android_context()`
/// panics in the Activity-less service-only process, so never call this from a
/// background path.
pub fn with_android_activity<F, R>(f: F) -> Result<R, String>
where
    F: for<'a> FnOnce(&mut JNIEnv<'a>, &JObject<'a>) -> Result<R, String>,
{
    let ctx = try_android_context()
        .ok_or("No Activity context (service-only process, or before tao registers)")?;
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
        .map_err(|e| format!("Failed to get JavaVM: {:?}", e))?;

    let mut env = vm.attach_current_thread()
        .map_err(|e| format!("Failed to attach thread: {:?}", e))?;

    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };

    let out = f(&mut env, &activity);
    clear_pending_exception(&mut env, &out);
    out
}

/// Get a system service by name
pub fn get_system_service<'a>(env: &mut JNIEnv<'a>, activity: &JObject<'a>, service_name: &str) -> Result<JObject<'a>, String> {
    let service_str = env.new_string(service_name)
        .map_err(|e| format!("Failed to create service name string: {:?}", e))?;
    
    env.call_method(
        activity,
        "getSystemService",
        "(Ljava/lang/String;)Ljava/lang/Object;",
        &[(&service_str).into()],
    )
    .map_err(|e| format!("Failed to get {} service: {:?}", service_name, e))?
    .l()
    .map_err(|e| format!("Failed to convert {} service object: {:?}", service_name, e))
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