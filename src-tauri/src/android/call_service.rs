//! The call's foreground service (`CallService.kt`): it keeps a backgrounded call's
//! microphone and playback live, and puts Leave in the notification shade.

use std::sync::Mutex;

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

/// The 1:1 call for the notification: shown once it is under way, cleared as it ends. Posted
/// only when what it shows changes.
pub fn ongoing(state: &vector_core::calls::session::CallState) {
    use vector_core::calls::session::Phase;
    static LAST: Mutex<Option<(String, bool, bool, String)>> = Mutex::new(None);
    let video = state.video_offered || state.video_mine.any() || state.video_peer.any();
    let shown = (state.phase == Phase::Active).then(|| (state.id.clone(), state.muted, video, state.name.clone()));
    {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if *last == shown {
            return;
        }
        *last = shown.clone();
    }
    let avatar = super::background_sync::notification_image(state.avatar.clone()).unwrap_or_default();
    let out = super::utils::with_android_context(|env, ctx| {
        let class = class(env, ctx)?;
        match &shown {
            Some((_, muted, video, name)) => {
                let name = env.new_string(name).map_err(|e| format!("{e:?}"))?;
                let avatar = env.new_string(&avatar).map_err(|e| format!("{e:?}"))?;
                env.call_static_method(
                    &class,
                    "show",
                    "(Ljava/lang/String;Ljava/lang/String;ZZJ)V",
                    &[JValue::Object(&name), JValue::Object(&avatar), JValue::Bool(*muted as u8), JValue::Bool(*video as u8), JValue::Long(state.active_ms as i64)],
                )
            }
            None => env.call_static_method(&class, "clear", "()V", &[]),
        }
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
    });
    if let Err(e) = out {
        vector_core::log_warn!("[calls] CallService.show: {e}");
    }
}

/// The voice Channel's call for the notification, posted only when what it shows changes.
pub fn voice(call: Option<&vector_core::calls::session::VoiceShown>) {
    static LAST: Mutex<Option<(String, String, String, bool)>> = Mutex::new(None);
    let shown = call.map(|c| (c.channel_id.to_string(), c.channel.to_string(), c.community.to_string(), c.muted));
    {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if *last == shown {
            return;
        }
        *last = shown.clone();
    }
    let since = call.map_or(0, |c| c.since_ms as i64);
    let out = super::utils::with_android_context(|env, ctx| {
        let class = class(env, ctx)?;
        match &shown {
            Some((_, channel, community, muted)) => {
                let channel = env.new_string(channel).map_err(|e| format!("{e:?}"))?;
                let community = env.new_string(community).map_err(|e| format!("{e:?}"))?;
                env.call_static_method(
                    &class,
                    "showVoice",
                    "(Ljava/lang/String;Ljava/lang/String;ZJ)V",
                    &[JValue::Object(&channel), JValue::Object(&community), JValue::Bool(*muted as u8), JValue::Long(since)],
                )
            }
            None => env.call_static_method(&class, "clear", "()V", &[]),
        }
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
    });
    if let Err(e) = out {
        vector_core::log_warn!("[calls] CallService.showVoice: {e}");
    }
}

/// Mute from the notification, in whichever call runs.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_CallService_nativeSetMuted(_env: JNIEnv, _class: JClass, on: jni::sys::jboolean) {
    super::utils::jni_fence("Java_io_vectorapp_CallService_nativeSetMuted", || (), move || {
        // spawn-detached: one toggle on whichever call runs; it resolves its own session.
        tauri::async_runtime::spawn(vector_core::db::scoped(async move {
            if vector_core::calls::voice::snapshot().is_some() {
                let _ = vector_core::calls::voice::set_muted(on != 0);
            } else {
                let _ = vector_core::calls::session::set_muted(on != 0).await;
            }
        }));
    })
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
