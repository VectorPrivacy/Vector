//! Notify When Closed on the phone (`VectorPushService.java`, `VectorPush.java`). UnifiedPush
//! hands over an endpoint, through a distributor app or the embedded FCM one, and each message
//! decrypted. The endpoint becomes this account's sealed-push device, the same as a browser's
//! subscription, so every Vector contact's app can wake this phone.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub enum Push {
    Off,
    /// Asked for; the endpoint is on its way.
    Busy,
    On,
}

/// This phone's push, or None when nothing on it can deliver one.
pub fn status() -> Option<Push> {
    #[cfg(target_os = "android")]
    {
        let registered = vector_core::push::device().is_some();
        let out = phone::status(registered);
        match out.as_deref() {
            Some("on") => Some(Push::On),
            Some("busy") => Some(Push::Busy),
            Some("off") => Some(Push::Off),
            None if registered => Some(Push::On),
            _ => None,
        }
    }
    #[cfg(not(target_os = "android"))]
    None
}

/// Ask for an endpoint: a distributor app the user has, else FCM's own through Play Services.
#[cfg(target_os = "android")]
pub fn enable() -> Result<(), String> {
    let vapid = phone::vapid_public()?;
    if phone::enable(&vapid)? {
        Ok(())
    } else {
        Err(crate::t::settings_push_unavailable().into())
    }
}

/// Every contact holding this phone's ticket is told to forget it.
#[cfg(target_os = "android")]
pub async fn disable() -> Result<(), String> {
    vector_core::push::disable().await?;
    let _ = vector_core::db::remove_setting(phone::VAPID_KEY);
    phone::disable();
    Ok(())
}

#[cfg(target_os = "android")]
mod phone {
    use jni::objects::{JByteArray, JClass, JObject, JString, JValue};
    use jni::JNIEnv;
    use vector_core::db;

    use crate::android::utils::{jni_fence, with_android_activity, with_android_context};

    /// The VAPID key a registration was asked for with, kept until the endpoint arrives.
    pub(super) const VAPID_KEY: &str = "push_native_vapid";
    const ORIGIN: &str = "https://vectorapp.io";

    /// This account's VAPID public key, made on first use.
    pub(super) fn vapid_public() -> Result<String, String> {
        let secret = match db::get_sql_setting(VAPID_KEY.into())? {
            Some(s) => s,
            None => {
                let (secret, _) = vector_push::vapid::generate();
                db::set_sql_setting(VAPID_KEY.into(), secret.clone())?;
                secret
            }
        };
        vector_push::vapid::public_key(&secret).map_err(|e| e.to_string())
    }

    fn helper<'a>(env: &mut JNIEnv<'a>, ctx: &JObject<'a>) -> Result<JClass<'a>, String> {
        let err = |e: jni::errors::Error| format!("{e:?}");
        let loader = env.call_method(ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[]).and_then(|v| v.l()).map_err(err)?;
        let name = env.new_string("io.vectorapp.VectorPush").map_err(err)?;
        let class = env
            .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])
            .and_then(|v| v.l())
            .map_err(err)?;
        Ok(JClass::from(class))
    }

    pub(super) fn status(registered: bool) -> Option<String> {
        with_android_context(|env, ctx| {
            let class = helper(env, ctx)?;
            let out = env
                .call_static_method(
                    &class,
                    "status",
                    "(Landroid/content/Context;Z)Ljava/lang/String;",
                    &[JValue::Object(ctx), JValue::Bool(registered.into())],
                )
                .and_then(|v| v.l())
                .map_err(|e| format!("{e:?}"))?;
            env.get_string(&JString::from(out)).map(Into::into).map_err(|e| format!("{e:?}"))
        })
        .ok()
    }

    pub(super) fn enable(vapid: &str) -> Result<bool, String> {
        with_android_activity(|env, activity| {
            let class = helper(env, activity)?;
            let key = env.new_string(vapid).map_err(|e| format!("{e:?}"))?;
            env.call_static_method(
                &class,
                "enable",
                "(Landroid/app/Activity;Ljava/lang/String;)Z",
                &[JValue::Object(activity), JValue::Object(&key)],
            )
            .and_then(|v| v.z())
            .map_err(|e| format!("{e:?}"))
        })
    }

    pub(super) fn disable() {
        let _ = with_android_context(|env, ctx| {
            let class = helper(env, ctx)?;
            env.call_static_method(&class, "disable", "(Landroid/content/Context;)V", &[JValue::Object(ctx)])
                .map(drop)
                .map_err(|e| format!("{e:?}"))
        });
    }

    /// A process the push started cold has no window, no context kept and no account open: the
    /// context is kept, and the account the app last used opened.
    fn ready(env: &mut JNIEnv, context: &JObject) -> Option<&'static tokio::runtime::Runtime> {
        crate::android::background_sync::hold_context(env, context);
        crate::android::incoming_call::install();
        let rt = crate::backend::core(crate::android::data_dir()?.clone());
        rt.block_on(crate::backend::account::open_headless()).then_some(rt)
    }

    fn string(env: &mut JNIEnv, s: &JString) -> String {
        env.get_string(s).map(String::from).unwrap_or_default()
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn Java_io_vectorapp_VectorPushService_nativeNewEndpoint(
        mut env: JNIEnv,
        _class: JClass,
        context: JObject,
        _data_dir: JString,
        url: JString,
        p256dh: JString,
        auth: JString,
    ) {
        jni_fence("Java_io_vectorapp_VectorPushService_nativeNewEndpoint", || (), move || {
            let Some(rt) = ready(&mut env, &context) else { return };
            let (endpoint, p256dh, auth) = (string(&mut env, &url), string(&mut env, &p256dh), string(&mut env, &auth));
            let Ok(Some(vapid)) = db::get_sql_setting(VAPID_KEY.into()) else {
                log::warn!("push: an endpoint arrived with no VAPID key asked for");
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
            let _rt = rt.enter();
            db::spawn_bound(async move {
                match vector_core::push::adopt(device).await {
                    Ok(()) => crate::backend::emit("push_state", serde_json::json!({ "push": "on" })),
                    Err(e) => log::warn!("push: endpoint not taken: {e}"),
                }
            });
        })
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn Java_io_vectorapp_VectorPushService_nativeMessage(
        mut env: JNIEnv,
        _class: JClass,
        context: JObject,
        _data_dir: JString,
        content: JByteArray,
    ) {
        jni_fence("Java_io_vectorapp_VectorPushService_nativeMessage", || (), move || {
            let Ok(bytes) = env.convert_byte_array(&content) else { return };
            let Some(rt) = ready(&mut env, &context) else { return };
            rt.block_on(db::scoped(deliver(bytes)));
        })
    }

    /// What a push says, in the words this account has for its sender. One no contact here
    /// vouches for (another account's, or forged) shows nothing it claims.
    async fn deliver(bytes: Vec<u8>) {
        use crate::android::incoming_call;
        use crate::backend::notify;
        let Some((npub, notice)) = vector_core::push::receive(&bytes) else {
            notify::pushed_unknown();
            return;
        };
        match notice.kind.as_str() {
            "call" | "video_call" => {
                let (name, avatar) = notify::known_as(&npub).await;
                incoming_call::ring_now(&notice.msg, &npub, &name, avatar.as_deref(), notice.kind == "video_call");
            }
            "missed_call" => {
                let (name, _) = notify::known_as(&npub).await;
                incoming_call::missed(&notice.msg, &npub, &name);
            }
            _ => notify::pushed(&npub, notice.text).await,
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn Java_io_vectorapp_VectorPushService_nativeUnregistered(
        mut env: JNIEnv,
        _class: JClass,
        context: JObject,
        _data_dir: JString,
    ) {
        jni_fence("Java_io_vectorapp_VectorPushService_nativeUnregistered", || (), move || {
            let Some(rt) = ready(&mut env, &context) else { return };
            rt.block_on(db::scoped(async {
                if let Err(e) = vector_core::push::disable().await {
                    log::warn!("push: unregistered, but tickets not withdrawn: {e}");
                }
                crate::backend::emit("push_state", serde_json::json!({ "push": "off" }));
            }));
        })
    }
}
