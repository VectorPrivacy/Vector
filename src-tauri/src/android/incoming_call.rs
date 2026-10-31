//! A call ringing while Vector isn't on screen (`IncomingCall.kt`): the lock screen's ring,
//! Answer and Decline, and a missed call once it stops unanswered.

use jni::objects::{JClass, JObject, JString, JValue};
use jni::JNIEnv;

struct PhoneRinger;

impl vector_core::calls::session::Ringer for PhoneRinger {
    // Rings on screen too: the app's own ring is silent.
    fn ring(&self, call_id: &str, peer: &str, name: &str, avatar: Option<&str>, video: bool) {
        ring_now(call_id, peer, name, avatar, video);
    }

    fn over(&self, call_id: &str, missed: bool) {
        let out = super::utils::with_android_context(|env, ctx| {
            let class = class(env, ctx)?;
            let id = env.new_string(call_id).map_err(|e| format!("{e:?}"))?;
            env.call_static_method(
                &class,
                "over",
                "(Landroid/content/Context;Ljava/lang/String;Z)V",
                &[JValue::Object(ctx), JValue::Object(&id), JValue::Bool(missed as u8)],
            )
            .map(|_| ())
            .map_err(|e| format!("{e:?}"))
        });
        if let Err(e) = out {
            vector_core::log_warn!("[calls] IncomingCall.over: {e}");
        }
    }

    fn ongoing(&self, state: &vector_core::calls::session::CallState) {
        super::call_service::ongoing(state);
    }

    fn voice(&self, call: Option<&vector_core::calls::session::VoiceShown>) {
        super::call_service::voice(call);
    }
}

/// Ring now: from the call session, or from a push in a process that runs none.
pub fn ring_now(call_id: &str, peer: &str, name: &str, avatar: Option<&str>, video: bool) {
    let avatar = super::background_sync::notification_image(avatar.map(str::to_string)).unwrap_or_default();
    let out = super::utils::with_android_context(|env, ctx| {
        let class = class(env, ctx)?;
        let args = [call_id, peer, name, avatar.as_str()].map(|s| env.new_string(s));
        let [Ok(id), Ok(npub), Ok(name), Ok(avatar)] = args else { return Err("string".to_string()) };
        env.call_static_method(
            &class,
            "ring",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Z)V",
            &[JValue::Object(ctx), JValue::Object(&id), JValue::Object(&npub), JValue::Object(&name), JValue::Object(&avatar), JValue::Bool(video as u8)],
        )
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
    });
    if let Err(e) = out {
        vector_core::log_warn!("[calls] IncomingCall.ring: {e}");
    }
}

/// A call we never saw ring here was missed: its push said so.
pub fn missed(call_id: &str, peer: &str, name: &str) {
    let out = super::utils::with_android_context(|env, ctx| {
        let class = class(env, ctx)?;
        let args = [call_id, peer, name].map(|s| env.new_string(s));
        let [Ok(id), Ok(npub), Ok(name)] = args else { return Err("string".to_string()) };
        env.call_static_method(
            &class,
            "missed",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
            &[JValue::Object(ctx), JValue::Object(&id), JValue::Object(&npub), JValue::Object(&name)],
        )
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
    });
    if let Err(e) = out {
        vector_core::log_warn!("[calls] IncomingCall.missed: {e}");
    }
}

fn class<'a>(env: &mut JNIEnv<'a>, ctx: &JObject<'a>) -> Result<JClass<'a>, String> {
    let loader = env
        .call_method(ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
        .and_then(|v| v.l())
        .map_err(|e| format!("{e:?}"))?;
    let name = env.new_string("io.vectorapp.IncomingCall").map_err(|e| format!("{e:?}"))?;
    let class = env
        .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])
        .and_then(|v| v.l())
        .map_err(|e| format!("{e:?}"))?;
    Ok(JClass::from(class))
}

/// Both the app and the background service ring through this; the first to start installs it.
pub fn install() {
    vector_core::calls::session::set_ringer(Box::new(PhoneRinger));
}

/// Decline from the notification: the caller hears it, whichever process rang.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_IncomingCall_nativeDecline(mut env: JNIEnv, _class: JClass, call_id: JString, npub: JString) {
    super::utils::jni_fence("Java_io_vectorapp_IncomingCall_nativeDecline", || (), move || {
        let call_id: String = env.get_string(&call_id).map(Into::into).unwrap_or_default();
        let npub: String = env.get_string(&npub).map(Into::into).unwrap_or_default();
        if call_id.is_empty() || npub.is_empty() {
            return;
        }
        // spawn-detached: one signal to the caller; the session is whichever is live.
        tauri::async_runtime::spawn(vector_core::db::scoped(async move {
            vector_core::calls::session::decline(&call_id, &npub).await;
        }));
    })
}

/// The ring was tapped (`answer` false) or answered: the chat opens, and an answer is taken up
/// once the call rings in the app, which may first have to start and read it.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_MainActivity_nativeOnCallTap(mut env: JNIEnv, _class: JClass, call_id: JString, npub: JString, answer: jni::sys::jboolean) {
    super::utils::jni_fence("Java_io_vectorapp_MainActivity_nativeOnCallTap", || (), move || {
        let call_id: String = env.get_string(&call_id).map(Into::into).unwrap_or_default();
        let npub: String = env.get_string(&npub).map(Into::into).unwrap_or_default();
        if npub.is_empty() {
            return;
        }
        let action = crate::deep_link::DeepLinkAction {
            action_type: "chat".to_string(),
            target: npub,
            call: (answer != 0 && !call_id.is_empty()).then_some(call_id),
            ..Default::default()
        };
        crate::deep_link::set_pending_action(action.clone());
        if let Some(handle) = crate::TAURI_APP.get() {
            use tauri::Emitter;
            let _ = handle.emit("deep_link_action", &action);
        }
    })
}
