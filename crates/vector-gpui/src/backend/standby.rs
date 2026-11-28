//! The background service started with no window: at boot, or when Android restarts it.

use jni::objects::{JClass, JObject};
use jni::JNIEnv;

use crate::android::utils::jni_fence;

#[unsafe(no_mangle)]
pub extern "C" fn Java_io_vectorapp_VectorNotificationService_nativeStandby(mut env: JNIEnv, _class: JClass, context: JObject) {
    jni_fence("Java_io_vectorapp_VectorNotificationService_nativeStandby", || (), move || {
        // A window signs in for itself, whatever its login screen is showing.
        if crate::android::utils::with_android_activity(|_, _| Ok(())).is_ok() {
            return;
        }
        crate::android::background_sync::hold_context(&mut env, &context);
        crate::android::incoming_call::install();
        let Some(dir) = crate::android::data_dir() else { return };
        let rt = super::core(dir.clone());
        // spawn-detached: signs in whichever account is active; what it starts binds to that session.
        rt.spawn(async {
            if super::account::standby().await {
                log::info!("standby: listening without a window");
            }
        });
    })
}
