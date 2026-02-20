use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use jni::objects::{JClass, JObject, JValue};
use jni::sys::{jboolean, jint, JNI_TRUE};
use jni::JNIEnv;
use std::sync::OnceLock;

use super::utils::with_android_context;

pub fn check_audio_permission() -> Result<bool, String> {
    with_android_context(|env, activity| {
        // Check permission
        let permission_str = env.new_string("android.permission.RECORD_AUDIO")
            .map_err(|e| format!("Failed to create permission string: {:?}", e))?;
        
        let permission_status = env.call_method(
            activity,
            "checkSelfPermission",
            "(Ljava/lang/String;)I",
            &[(&permission_str).into()]
        ).map_err(|e| format!("Failed to check permission: {:?}", e))?
            .i()
            .map_err(|e| format!("Failed to convert permission status: {:?}", e))?;
        
        // PackageManager.PERMISSION_GRANTED = 0
        Ok(permission_status == 0)
    })
}

static PERMISSION_CALLBACK: OnceLock<Arc<(Mutex<Option<bool>>, Condvar)>> = OnceLock::new();

#[cfg(target_os = "android")]
pub fn request_audio_permission_blocking() -> Result<bool, String> {
    const AUDIO_PERMISSION_REQUEST_CODE: i32 = 9876;
    
    // Initialize the callback state
    let callback_state = Arc::new((Mutex::new(None), Condvar::new()));
    PERMISSION_CALLBACK.set(callback_state.clone()).ok();
    
    // Request permission using our helper
    with_android_context(|env, activity| {
        // Create permission array
        let permission_str = env.new_string("android.permission.RECORD_AUDIO")
            .map_err(|e| format!("Failed to create permission string: {:?}", e))?;
        
        let permission_array = env.new_object_array(
            1,
            "java/lang/String",
            JObject::from(permission_str),
        ).map_err(|e| format!("Failed to create permission array: {:?}", e))?;
        
        // Request permissions
        env.call_method(
            activity,
            "requestPermissions",
            "([Ljava/lang/String;I)V",
            &[
                JValue::from(&JObject::from(permission_array)),
                JValue::from(AUDIO_PERMISSION_REQUEST_CODE),
            ],
        ).map_err(|e| format!("Failed to request permissions: {:?}", e))?;
        
        Ok(())
    })?;
    
    // Wait for the callback
    let (lock, cvar) = &*callback_state;
    
    // Wait up to 60 seconds for permission response
    let timeout = Duration::from_secs(60);
    
    let result = {
        let guard = lock.lock().unwrap();
        let wait_result = cvar.wait_timeout_while(
            guard,
            timeout,
            |result| result.is_none()
        ).unwrap();
        
        if wait_result.1.timed_out() {
            return Err("Permission request timed out".to_string());
        }
        
        // Extract the value while we still have the lock
        *wait_result.0
    };
    
    match result {
        Some(granted) => {
            // Add a small delay to ensure permission propagates
            std::thread::sleep(Duration::from_millis(200));
            Ok(granted)
        }
        None => Err("Permission result not received".to_string()),
    }
}

// This function will be called from Java when permission result is received
#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn Java_io_vectorapp_PermissionHandler_onPermissionResult(
    _env: JNIEnv,
    _class: JClass,
    request_code: jint,
    granted: jboolean,
) {
    const AUDIO_PERMISSION_REQUEST_CODE: i32 = 9876;
    
    if request_code == AUDIO_PERMISSION_REQUEST_CODE {
        if let Some(callback_state) = PERMISSION_CALLBACK.get() {
            let (lock, cvar) = &**callback_state;
            let mut result = lock.lock().unwrap();
            *result = Some(granted == JNI_TRUE);
            cvar.notify_all();
        }
    }
}
