//! The desktop client's Android glue: the media crate's, and the content URIs a share or a
//! picker hands over.

pub use vector_media::android::*;

#[path = "../../../src-tauri/src/android/filesystem.rs"]
#[allow(dead_code)]
pub mod filesystem;

use std::path::PathBuf;
use std::sync::OnceLock;

/// A folder `Context` names, by its getter.
fn folder(getter: &str) -> Option<PathBuf> {
    utils::with_android_context(|env, ctx| {
        let err = |e: jni::errors::Error| format!("{e:?}");
        let dir = env.call_method(ctx, getter, "()Ljava/io/File;", &[]).and_then(|v| v.l()).map_err(err)?;
        let path = env.call_method(&dir, "getAbsolutePath", "()Ljava/lang/String;", &[]).and_then(|v| v.l()).map_err(err)?;
        let path: String = env.get_string(&jni::objects::JString::from(path)).map_err(err)?.into();
        Ok(PathBuf::from(path))
    })
    .ok()
}

/// The app's data folder: where the Tauri client keeps its accounts.
pub fn data_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| folder("getDataDir")).as_ref()
}

pub fn cache_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| folder("getCacheDir")).as_ref()
}

/// The shell's own text in the app's language, kept by `io.vectorapp.Strings` for its
/// notifications, channels and prompts.
pub fn send_strings() {
    use crate::t;
    use jni::objects::{JClass, JValue};
    let table: [(&str, gpui::SharedString); 37] = [
        ("android-call-incoming", t::android_call_incoming()),
        ("android-call-incoming-video", t::android_call_incoming_video()),
        ("android-call-missed", t::android_call_missed()),
        ("android-call-decline", t::android_call_decline()),
        ("android-call-answer", t::android_call_answer()),
        ("android-call-voice", t::android_call_voice()),
        ("android-call-video", t::android_call_video()),
        ("android-call-muted", t::android_call_muted()),
        ("android-call-mute", t::android_call_mute()),
        ("android-call-unmute", t::android_call_unmute()),
        ("android-call-ongoing", t::android_call_ongoing()),
        ("android-call-return", t::android_call_return()),
        ("android-call-leave", t::android_call_leave()),
        ("android-notify-someone", t::android_notify_someone()),
        ("android-notify-me", t::android_notify_me()),
        ("android-notify-reply", t::android_notify_reply()),
        ("android-notify-mark-read", t::android_notify_mark_read()),
        ("android-service-listening", t::android_service_listening()),
        ("android-service-tagline-watch", t::android_service_tagline_watch()),
        ("android-service-tagline-miss", t::android_service_tagline_miss()),
        ("android-service-tagline-standby", t::android_service_tagline_standby()),
        ("android-channel-ring", t::android_channel_ring()),
        ("android-channel-ring-about", t::android_channel_ring_about()),
        ("android-channel-missed", t::android_channel_missed()),
        ("android-channel-missed-about", t::android_channel_missed_about()),
        ("android-channel-calls", t::android_channel_calls()),
        ("android-channel-calls-about", t::android_channel_calls_about()),
        ("android-channel-messages", t::android_channel_messages()),
        ("android-channel-messages-about", t::android_channel_messages_about()),
        ("android-channel-service", t::android_channel_service()),
        ("android-channel-service-about", t::android_channel_service_about()),
        ("android-open-with", t::android_open_with()),
        ("android-share", t::android_share()),
        ("android-biometric-use", t::android_biometric_use()),
        ("android-biometric-screen-lock", t::android_biometric_screen_lock()),
        ("android-biometric-enable", t::android_biometric_enable()),
        ("android-biometric-unlock", t::android_biometric_unlock()),
    ];
    let json = serde_json::Value::Object(table.into_iter().map(|(k, v)| (k.to_string(), serde_json::Value::from(v.as_ref()))).collect()).to_string();
    let sent = utils::with_android_context(|env, ctx| {
        let err = |e: jni::errors::Error| format!("{e:?}");
        let loader = env.call_method(ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[]).and_then(|v| v.l()).map_err(err)?;
        let name = env.new_string("io.vectorapp.Strings").map_err(err)?;
        let class = env
            .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])
            .and_then(|v| v.l())
            .map_err(err)?;
        let json = env.new_string(&json).map_err(err)?;
        let out = env.call_static_method(&JClass::from(class), "set", "(Landroid/content/Context;Ljava/lang/String;)V", &[JValue::Object(ctx), JValue::Object(&json)]);
        if out.is_err() {
            let _ = env.exception_clear();
        }
        out.map(drop).map_err(err)
    });
    if let Err(e) = sent {
        log::warn!("android strings: {e}");
    }
}

/// Calls a static `VectorBatteryHelper` method taking the context and maybe a flag; a void one
/// reads as false.
fn battery_helper(method: &str, sig: &str, flag: Option<bool>) -> bool {
    use jni::objects::{JClass, JValue};
    utils::with_android_context(|env, ctx| {
        let err = |e: jni::errors::Error| format!("{e:?}");
        let loader = env.call_method(ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[]).and_then(|v| v.l()).map_err(err)?;
        let name = env.new_string("io.vectorapp.VectorBatteryHelper").map_err(err)?;
        let class = env
            .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])
            .and_then(|v| v.l())
            .map_err(err)?;
        let mut args = vec![JValue::Object(ctx)];
        args.extend(flag.map(|f| JValue::Bool(f.into())));
        let out = env.call_static_method(&JClass::from(class), method, sig, &args);
        if out.is_err() {
            let _ = env.exception_clear();
        }
        out.map(|v| v.z().unwrap_or(false)).map_err(err)
    })
    .unwrap_or(false)
}

/// The system back gesture gives way on the screen's left edge across `span` (top, bottom),
/// else where a thumb swipes.
pub fn claim_left_edge(span: Option<(f32, f32)>) {
    use jni::objects::{JClass, JValue};
    let done = utils::with_android_context(|env, ctx| {
        let err = |e: jni::errors::Error| format!("{e:?}");
        let loader = env.call_method(ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[]).and_then(|v| v.l()).map_err(err)?;
        let name = env.new_string("io.vectorapp.gpui.Bridge").map_err(err)?;
        let class = env
            .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])
            .and_then(|v| v.l())
            .map_err(err)?;
        let class = JClass::from(class);
        let out = match span {
            Some((top, bottom)) => env.call_static_method(&class, "claimLeft", "(FF)V", &[JValue::Float(top), JValue::Float(bottom)]),
            None => env.call_static_method(&class, "releaseLeft", "()V", &[]),
        };
        if out.is_err() {
            let _ = env.exception_clear();
        }
        out.map(|_| ()).map_err(err)
    });
    if let Err(e) = done {
        log::warn!("android edge claim: {e}");
    }
}

fn battery_flag(method: &str) -> bool {
    battery_helper(method, "(Landroid/content/Context;)Z", None)
}

/// Settings' Run in Background: whether the service keeps the client connected while away.
pub fn background_enabled() -> bool {
    battery_flag("getBackgroundServiceEnabled")
}

/// Android may stop a service it is allowed to optimise for battery.
pub fn battery_exempt() -> bool {
    battery_flag("isIgnoringBatteryOptimizations")
}

/// The system's dialog asking to let Vector run unoptimised; the answer shows on return.
pub fn request_battery_exemption() {
    battery_helper("requestBatteryOptimization", "(Landroid/content/Context;)V", None);
}

/// Remembered, and the service started or stopped to match.
pub fn set_background(on: bool) {
    battery_helper("setBackgroundServiceEnabled", "(Landroid/content/Context;Z)V", Some(on));
    let method = if on { "startBackgroundService" } else { "stopBackgroundService" };
    battery_helper(method, "(Landroid/content/Context;)V", None);
}

/// Once per install, after sign-in: whether to offer Run in Background, which notifications need
/// while the app is away.
fn offer_background() -> bool {
    if battery_flag("getBackgroundServicePrompted") {
        return false;
    }
    battery_helper("setBackgroundServicePrompted", "(Landroid/content/Context;)V", None);
    true
}

pub enum Offer {
    Push,
    Background,
}

/// Once per account after sign-in: Notify When Closed, else, for a phone push can't reach, Run
/// in Background.
pub fn offer_notifications() -> Option<Offer> {
    use crate::backend::push::{self, Push};
    static ASKED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let npub = vector_core::db::get_current_account().ok()?;
    {
        let mut asked = ASKED.lock().unwrap_or_else(|e| e.into_inner());
        if asked.as_deref() == Some(npub.as_str()) {
            return None;
        }
        *asked = Some(npub);
    }
    match push::status() {
        Some(Push::Off) => {
            const PROMPTED: &str = "push_prompted";
            let prompted = vector_core::db::get_sql_setting(PROMPTED.into()).ok().flatten().is_some();
            // Running in the background already brings their notifications.
            if prompted || background_enabled() {
                return None;
            }
            let _ = vector_core::db::set_sql_setting(PROMPTED.into(), "true".into());
            Some(Offer::Push)
        }
        None => offer_background().then_some(Offer::Background),
        Some(_) => None,
    }
}

/// Test builds: `adb shell setprop debug.vector.fixtures 1` starts on the desktop's fixtures
/// (no account, no relays), as an app can't be handed `--fixtures`. Their media isn't shipped.
#[cfg(feature = "test-build")]
pub fn fixtures() -> Option<PathBuf> {
    use std::ffi::{CStr, c_char, c_int};
    unsafe extern "C" {
        fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
    }
    let mut value = [0 as c_char; 92];
    // SAFETY: PROP_VALUE_MAX (92) bytes is the most the call writes, NUL included.
    let on = unsafe {
        __system_property_get(c"debug.vector.fixtures".as_ptr(), value.as_mut_ptr());
        CStr::from_ptr(value.as_ptr())
    }
    .to_bytes()
        == b"1";
    if !on {
        return None;
    }
    let path = cache_dir()?.join("fixtures.json");
    std::fs::write(&path, include_bytes!("../reference/fixtures.json")).ok()?;
    log::info!("debug.vector.fixtures: starting on fixtures");
    Some(path)
}
