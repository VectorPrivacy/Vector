use jni::{JavaVM, JNIEnv};
use jni::objects::JObject;
use ndk_context::android_context;

/// Standard buffer size for reading streams
pub const STREAM_BUFFER_SIZE: i32 = 8192;

/// Execute a function with an Android JNI context
pub fn with_android_context<F, R>(f: F) -> Result<R, String>
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