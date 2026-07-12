use jni::{JavaVM, JNIEnv};
use jni::objects::JObject;
use ndk_context::android_context;

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
        return f(&mut env, ctx.as_obj());
    }

    // Fallback: Activity context (only safe when an Activity exists).
    let ctx = android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
        .map_err(|e| format!("Failed to get JavaVM: {:?}", e))?;

    let mut env = vm.attach_current_thread()
        .map_err(|e| format!("Failed to attach thread: {:?}", e))?;

    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };

    f(&mut env, &activity)
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
    let ctx = android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
        .map_err(|e| format!("Failed to get JavaVM: {:?}", e))?;

    let mut env = vm.attach_current_thread()
        .map_err(|e| format!("Failed to attach thread: {:?}", e))?;

    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };

    f(&mut env, &activity)
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