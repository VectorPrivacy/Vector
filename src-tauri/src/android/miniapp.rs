//! Android-specific Mini Apps implementation using native WebView overlay.
//!
//! This module provides the Rust → Kotlin JNI bridge for opening and
//! managing Mini App overlay WebViews on Android.

use jni::objects::{JClass, JObject, JString, JValue};
use jni::JNIEnv;

use super::utils::with_android_context;

/// Load a class using the activity's classloader.
///
/// This is necessary because `env.find_class()` uses the system classloader
/// when called from a native thread, which can't access app classes.
fn load_class_from_activity<'a>(
    env: &mut JNIEnv<'a>,
    activity: &JObject<'a>,
    class_name: &str,
) -> Result<JClass<'a>, String> {
    // Get the activity's class
    let activity_class = env
        .get_object_class(activity)
        .map_err(|e| format!("Failed to get activity class: {:?}", e))?;

    // Get the classloader: activity.getClass().getClassLoader()
    let class_loader = env
        .call_method(&activity_class, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
        .map_err(|e| format!("Failed to get classloader: {:?}", e))?
        .l()
        .map_err(|e| format!("Failed to convert classloader: {:?}", e))?;

    // Create Java string for class name (use dots, not slashes)
    let class_name_java = class_name.replace('/', ".");
    let j_class_name = env
        .new_string(&class_name_java)
        .map_err(|e| format!("Failed to create class name string: {:?}", e))?;

    // Call classLoader.loadClass(className)
    let loaded_class = env
        .call_method(
            &class_loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&j_class_name)],
        )
        .map_err(|e| format!("Failed to load class {}: {:?}", class_name_java, e))?
        .l()
        .map_err(|e| format!("Failed to convert loaded class: {:?}", e))?;

    // Convert JObject to JClass
    Ok(JClass::from(loaded_class))
}

/// Open a Mini App in a full-screen overlay WebView.
///
/// This calls MiniAppManager.openMiniApp() on the Kotlin side.
pub fn open_miniapp_overlay(
    miniapp_id: &str,
    package_path: &str,
    chat_id: &str,
    message_id: &str,
    href: Option<&str>,
) -> Result<(), String> {
    log_info!(
        "Opening Mini App overlay: {} (chat: {}, message: {})",
        miniapp_id, chat_id, message_id
    );

    with_android_context(|env, activity| {
        // Load MiniAppManager class using activity's classloader
        let manager_class = load_class_from_activity(env, activity, "io/vectorapp/miniapp/MiniAppManager")
            .map_err(|e| format!("Failed to load MiniAppManager class: {:?}", e))?;

        // Create Java strings for parameters
        let j_miniapp_id = env
            .new_string(miniapp_id)
            .map_err(|e| format!("Failed to create miniapp_id string: {:?}", e))?;

        let j_package_path = env
            .new_string(package_path)
            .map_err(|e| format!("Failed to create package_path string: {:?}", e))?;

        let j_chat_id = env
            .new_string(chat_id)
            .map_err(|e| format!("Failed to create chat_id string: {:?}", e))?;

        let j_message_id = env
            .new_string(message_id)
            .map_err(|e| format!("Failed to create message_id string: {:?}", e))?;

        // href is optional - create String or null
        let j_href: JObject = if let Some(h) = href {
            env.new_string(h)
                .map_err(|e| format!("Failed to create href string: {:?}", e))?
                .into()
        } else {
            JObject::null()
        };

        // Call MiniAppManager.openMiniApp(miniappId, packagePath, chatId, messageId, href)
        // Note: We need to run on UI thread, so we call via the activity
        env.call_static_method(
            manager_class,
            "openMiniApp",
            "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
            &[
                JValue::Object(&j_miniapp_id),
                JValue::Object(&j_package_path),
                JValue::Object(&j_chat_id),
                JValue::Object(&j_message_id),
                JValue::Object(&j_href),
            ],
        )
        .map_err(|e| format!("Failed to call MiniAppManager.openMiniApp: {:?}", e))?;

        log_info!("Mini App overlay open request sent");
        Ok(())
    })
}

/// Close the currently open Mini App overlay.
pub fn close_miniapp_overlay() -> Result<(), String> {
    log_info!("Closing Mini App overlay");

    with_android_context(|env, activity| {
        let manager_class = load_class_from_activity(env, activity, "io/vectorapp/miniapp/MiniAppManager")
            .map_err(|e| format!("Failed to load MiniAppManager class: {:?}", e))?;

        env.call_static_method(manager_class, "closeMiniApp", "()V", &[])
            .map_err(|e| format!("Failed to call MiniAppManager.closeMiniApp: {:?}", e))?;

        log_info!("Mini App overlay close request sent");
        Ok(())
    })
}

/// Send an event to the currently open Mini App.
pub fn send_to_miniapp(event: &str, data: &str) -> Result<(), String> {
    log_debug!("Sending event to Mini App: {}", event);

    with_android_context(|env, activity| {
        let manager_class = load_class_from_activity(env, activity, "io/vectorapp/miniapp/MiniAppManager")
            .map_err(|e| format!("Failed to load MiniAppManager class: {:?}", e))?;

        let j_event = env
            .new_string(event)
            .map_err(|e| format!("Failed to create event string: {:?}", e))?;

        let j_data = env
            .new_string(data)
            .map_err(|e| format!("Failed to create data string: {:?}", e))?;

        env.call_static_method(
            manager_class,
            "sendToMiniApp",
            "(Ljava/lang/String;Ljava/lang/String;)V",
            &[JValue::Object(&j_event), JValue::Object(&j_data)],
        )
        .map_err(|e| format!("Failed to call MiniAppManager.sendToMiniApp: {:?}", e))?;

        Ok(())
    })
}

/// Send realtime data to the Mini App.
#[allow(dead_code)]
pub fn send_realtime_data_to_miniapp(data: &[u8]) -> Result<(), String> {
    log_debug!("Sending {} bytes realtime data to Mini App", data.len());

    with_android_context(|env, activity| {
        let manager_class = load_class_from_activity(env, activity, "io/vectorapp/miniapp/MiniAppManager")
            .map_err(|e| format!("Failed to load MiniAppManager class: {:?}", e))?;

        // Create byte array
        let j_data = env
            .byte_array_from_slice(data)
            .map_err(|e| format!("Failed to create byte array: {:?}", e))?;

        env.call_static_method(
            manager_class,
            "sendRealtimeData",
            "([B)V",
            &[JValue::Object(&j_data.into())],
        )
        .map_err(|e| format!("Failed to call MiniAppManager.sendRealtimeData: {:?}", e))?;

        Ok(())
    })
}

/// Check if a Mini App is currently open.
pub fn is_miniapp_open() -> Result<bool, String> {
    with_android_context(|env, activity| {
        let manager_class = load_class_from_activity(env, activity, "io/vectorapp/miniapp/MiniAppManager")
            .map_err(|e| format!("Failed to load MiniAppManager class: {:?}", e))?;

        let result = env
            .call_static_method(manager_class, "isOpen", "()Z", &[])
            .map_err(|e| format!("Failed to call MiniAppManager.isOpen: {:?}", e))?
            .z()
            .map_err(|e| format!("Failed to convert boolean result: {:?}", e))?;

        Ok(result)
    })
}

/// Get the current Mini App ID, if one is open.
#[allow(dead_code)]
pub fn get_current_miniapp_id() -> Result<Option<String>, String> {
    with_android_context(|env, activity| {
        let manager_class = load_class_from_activity(env, activity, "io/vectorapp/miniapp/MiniAppManager")
            .map_err(|e| format!("Failed to load MiniAppManager class: {:?}", e))?;

        let result = env
            .call_static_method(
                manager_class,
                "getCurrentMiniAppId",
                "()Ljava/lang/String;",
                &[],
            )
            .map_err(|e| format!("Failed to call MiniAppManager.getCurrentMiniAppId: {:?}", e))?
            .l()
            .map_err(|e| format!("Failed to convert result: {:?}", e))?;

        if result.is_null() {
            Ok(None)
        } else {
            let j_str = JString::from(result);
            let rust_str: String = env
                .get_string(&j_str)
                .map_err(|e| format!("Failed to convert string: {:?}", e))?
                .into();
            Ok(Some(rust_str))
        }
    })
}
