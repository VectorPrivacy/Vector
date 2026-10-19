//! The call's foreground service (`CallService.kt`): it keeps a backgrounded call's
//! microphone and playback live, and puts Leave in the notification shade.

use jni::objects::{JClass, JObject, JValue};
use jni::JNIEnv;

fn class<'a>(env: &mut JNIEnv<'a>, ctx: &JObject<'a>) -> Result<JClass<'a>, String> {
    let loader = env
        .call_method(ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
        .and_then(|v| v.l())
        .map_err(|e| format!("{e:?}"))?;
    let name = env.new_string("io.vectorapp.CallService").map_err(|e| format!("{e:?}"))?;
    let class = env
        .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])
        .and_then(|v| v.l())
        .map_err(|e| format!("{e:?}"))?;
    Ok(JClass::from(class))
}

fn call(method: &str) {
    let out = super::utils::with_android_context(|env, ctx| {
        let class = class(env, ctx)?;
        env.call_static_method(&class, method, "(Landroid/content/Context;)V", &[JValue::Object(ctx)])
            .map(|_| ())
            .map_err(|e| format!("{e:?}"))
    });
    if let Err(e) = out {
        vector_core::log_warn!("[calls] CallService.{method}: {e}");
    }
}

pub fn start() {
    call("start");
}

pub fn stop() {
    call("stop");
}

pub fn keep_screen_on(on: bool) {
    let _ = super::utils::with_android_activity(|env, activity| {
        let class = class(env, activity)?;
        env.call_static_method(&class, "keepScreenOn", "(Landroid/app/Activity;Z)V", &[JValue::Object(activity), JValue::Bool(on as u8)])
            .map(|_| ())
            .map_err(|e| format!("{e:?}"))
    });
}

/// Leave from the notification, or with the app swiped away: whichever call runs ends.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_CallService_nativeLeaveCall(_env: JNIEnv, _class: JClass) {
    super::utils::jni_fence("Java_io_vectorapp_CallService_nativeLeaveCall", || (), || {
        tauri::async_runtime::spawn(vector_core::db::scoped(async {
            vector_core::calls::voice::leave().await;
            if vector_core::calls::session::snapshot().is_some() {
                let _ = vector_core::calls::session::hangup().await;
            }
        }));
    })
}
