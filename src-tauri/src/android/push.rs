//! Push for the phone (`VectorPushService.java`, `VectorPush.java`). UnifiedPush hands over an
//! endpoint, through a distributor app or the embedded FCM one, and each message already
//! decrypted. The endpoint becomes this account's sealed-push device, the same as a browser's
//! subscription, so every Vector contact's app can wake this phone.

use jni::objects::{JByteArray, JClass, JObject, JString, JValue};
use jni::JNIEnv;

/// The VAPID key a registration was asked for with, kept until the endpoint arrives.
const VAPID_KEY: &str = "push_native_vapid";
const ORIGIN: &str = "https://vectorapp.io";

/// This account's VAPID public key, made on first use.
fn vapid_public() -> Result<String, String> {
    let secret = match vector_core::db::settings::get_sql_setting(VAPID_KEY.into())? {
        Some(s) => s,
        None => {
            let (secret, _) = vector_push::vapid::generate();
            vector_core::db::settings::set_sql_setting(VAPID_KEY.into(), secret.clone())?;
            secret
        }
    };
    vector_push::vapid::public_key(&secret).map_err(|e| e.to_string())
}

fn helper<'a>(env: &mut JNIEnv<'a>, ctx: &JObject<'a>) -> Result<JClass<'a>, String> {
    let loader = env
        .call_method(ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
        .and_then(|v| v.l())
        .map_err(|e| format!("{e:?}"))?;
    let name = env.new_string("io.vectorapp.VectorPush").map_err(|e| format!("{e:?}"))?;
    let class = env
        .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])
        .and_then(|v| v.l())
        .map_err(|e| format!("{e:?}"))?;
    Ok(JClass::from(class))
}

/// `on`, `off`, `busy` (asked, no endpoint yet) or `unsupported` (no distributor, no Play Services).
pub fn status() -> String {
    let registered = vector_core::push::device().is_some();
    super::utils::with_android_context(|env, ctx| {
        let class = helper(env, ctx)?;
        let out = env
            .call_static_method(
                &class,
                "status",
                "(Landroid/content/Context;Z)Ljava/lang/String;",
                &[JValue::Object(ctx), JValue::Bool(registered as u8)],
            )
            .and_then(|v| v.l())
            .map_err(|e| format!("{e:?}"))?;
        env.get_string(&JString::from(out)).map(Into::into).map_err(|e| format!("{e:?}"))
    })
    .unwrap_or_else(|_| if registered { "on" } else { "unsupported" }.into())
}

/// Ask for an endpoint: a distributor the user has, else FCM's own through Play Services.
pub fn enable() -> Result<(), String> {
    let vapid = vapid_public()?;
    super::utils::with_android_activity(|env, activity| {
        let class = helper(env, activity)?;
        let key = env.new_string(&vapid).map_err(|e| format!("{e:?}"))?;
        let started = env
            .call_static_method(&class, "enable", "(Landroid/app/Activity;Ljava/lang/String;)Z", &[JValue::Object(activity), JValue::Object(&key)])
            .and_then(|v| v.z())
            .map_err(|e| format!("{e:?}"))?;
        started
            .then_some(())
            .ok_or_else(|| "This phone has no way to receive push: install a UnifiedPush app such as ntfy".to_string())
    })
}

pub async fn disable() -> Result<(), String> {
    vector_core::push::disable().await?;
    let _ = vector_core::db::settings::remove_setting(VAPID_KEY);
    super::utils::with_android_context(|env, ctx| {
        let class = helper(env, ctx)?;
        env.call_static_method(&class, "disable", "(Landroid/content/Context;)V", &[JValue::Object(ctx)])
            .map(|_| ())
            .map_err(|e| format!("{e:?}"))
    })
}

/// A process the push started cold has no context kept and no account open: keep the service's
/// context and open the account the app last used.
fn ready(env: &mut JNIEnv, context: &JObject, data_dir: &JString) -> bool {
    super::background_sync::hold_context(env, context);
    super::incoming_call::install();
    if vector_core::db::get_current_account().is_ok() {
        return true;
    }
    let Ok(dir) = env.get_string(data_dir).map(String::from) else { return false };
    match super::background_sync::bootstrap_pipeline(&dir) {
        Ok(_) => {
            tauri::async_runtime::block_on(super::background_sync::preload_profiles_into_state());
            true
        }
        Err(e) => {
            vector_core::log_warn!("[Push] no account to deliver to: {e}");
            false
        }
    }
}

#[no_mangle]
pub extern "C" fn Java_io_vectorapp_VectorPushService_nativeNewEndpoint(
    mut env: JNIEnv,
    _class: JClass,
    context: JObject,
    data_dir: JString,
    url: JString,
    p256dh: JString,
    auth: JString,
) {
    super::utils::jni_fence("Java_io_vectorapp_VectorPushService_nativeNewEndpoint", || (), move || {
        if !ready(&mut env, &context, &data_dir) {
            return;
        }
        let read = |env: &mut JNIEnv, s: &JString| env.get_string(s).map(String::from).unwrap_or_default();
        let (endpoint, p256dh, auth) = (read(&mut env, &url), read(&mut env, &p256dh), read(&mut env, &auth));
        let Ok(Some(vapid)) = vector_core::db::settings::get_sql_setting(VAPID_KEY.into()) else {
            vector_core::log_warn!("[Push] an endpoint arrived with no VAPID key asked for");
            return;
        };
        if endpoint.is_empty() || p256dh.is_empty() || auth.is_empty() {
            return;
        }
        // Same endpoint and keys: the ticket every contact holds still names this device.
        let id = match vector_core::push::device() {
            Some(d) if d.endpoint == endpoint && d.p256dh == p256dh && d.auth == auth => d.id,
            _ => format!("{:016x}", rand::random::<u64>()),
        };
        let device = vector_core::push::Device { id, endpoint, p256dh, auth, vapid, origin: ORIGIN.into() };
        // spawn-detached: the device belongs to the account open now; adopt binds itself.
        tauri::async_runtime::spawn(vector_core::db::scoped(async move {
            match vector_core::push::adopt(device).await {
                Ok(()) => vector_core::emit_event("push_state", &serde_json::json!({ "push": "on" })),
                Err(e) => vector_core::log_warn!("[Push] endpoint not taken: {e}"),
            }
        }));
    })
}

#[no_mangle]
pub extern "C" fn Java_io_vectorapp_VectorPushService_nativeMessage(
    mut env: JNIEnv,
    _class: JClass,
    context: JObject,
    data_dir: JString,
    content: JByteArray,
) {
    super::utils::jni_fence("Java_io_vectorapp_VectorPushService_nativeMessage", || (), move || {
        let Ok(bytes) = env.convert_byte_array(&content) else { return };
        if !ready(&mut env, &context, &data_dir) {
            return;
        }
        tauri::async_runtime::block_on(vector_core::db::scoped(deliver(bytes)));
    })
}

/// Show what a push says, in the words this account has for its sender. One no contact here
/// vouches for (another account's, or forged) shows nothing it claims.
async fn deliver(bytes: Vec<u8>) {
    let Some((npub, notice)) = vector_core::push::receive(&bytes) else {
        super::background_sync::post_notification_jni("New message", "You have a new message", None, None, None, None, None);
        return;
    };
    let (name, avatar) = {
        let state = vector_core::STATE.lock().await;
        match state.get_profile(&npub) {
            Some(p) => {
                let name = [p.nickname(), p.display_name(), p.name()].into_iter().find(|n| !n.is_empty()).map(str::to_string);
                (name, p.has_avatar_cached().then(|| p.avatar_cached().into_owned()))
            }
            None => (None, None),
        }
    };
    let name = name.unwrap_or_else(|| format!("{}…", &npub[..npub.len().min(14)]));
    match notice.kind.as_str() {
        "call" | "video_call" => {
            super::incoming_call::ring_now(&notice.msg, &npub, &name, avatar.as_deref(), notice.kind == "video_call");
        }
        "missed_call" => super::incoming_call::missed(&notice.msg, &npub, &name),
        _ => {
            let body = if notice.text.is_empty() { "Sent you a message".to_string() } else { notice.text };
            super::background_sync::post_notification_jni(&name, &body, avatar.as_deref(), Some(&npub), Some(&name), None, None);
        }
    }
}

#[no_mangle]
pub extern "C" fn Java_io_vectorapp_VectorPushService_nativeUnregistered(mut env: JNIEnv, _class: JClass, context: JObject, data_dir: JString) {
    super::utils::jni_fence("Java_io_vectorapp_VectorPushService_nativeUnregistered", || (), move || {
        if !ready(&mut env, &context, &data_dir) {
            return;
        }
        tauri::async_runtime::block_on(vector_core::db::scoped(async {
            if let Err(e) = vector_core::push::disable().await {
                vector_core::log_warn!("[Push] unregistered, but tickets not withdrawn: {e}");
            }
            vector_core::emit_event("push_state", &serde_json::json!({ "push": "off" }));
        }));
    })
}
