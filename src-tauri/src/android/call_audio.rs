//! A call's sound as a phone call's (`CallAudioRouter.kt`): the communication mode and its
//! route, every AAudio stream opened for voice (the vendored ndk's switch), and the screen
//! dark at the ear. Holds are counted; any out loud makes it out loud.

use std::sync::Mutex;

use jni::objects::{JClass, JObject, JString, JValue};
use jni::JNIEnv;
use serde::Serialize;

static HOLDS: Mutex<Vec<bool>> = Mutex::new(Vec::new());
static APPLIED: Mutex<Option<bool>> = Mutex::new(None);
static ROUTE: Mutex<Option<Route>> = Mutex::new(None);

/// Where the call plays, and where else it could.
#[derive(Serialize, Clone)]
pub struct Route {
    pub current: String,
    pub available: Vec<String>,
}

pub struct Hold(bool);

pub fn hold(out_loud: bool) -> Hold {
    let mut holds = HOLDS.lock().unwrap_or_else(|e| e.into_inner());
    holds.push(out_loud);
    apply(&holds);
    Hold(out_loud)
}

impl Drop for Hold {
    fn drop(&mut self) {
        let mut holds = HOLDS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = holds.iter().position(|l| *l == self.0) {
            holds.swap_remove(i);
        }
        apply(&holds);
    }
}

fn apply(holds: &[bool]) {
    let wanted = (!holds.is_empty()).then(|| holds.iter().any(|l| *l));
    let mut applied = APPLIED.lock().unwrap_or_else(|e| e.into_inner());
    if *applied == wanted {
        return;
    }
    let flips = applied.is_some() != wanted.is_some();
    // The mode first, so the streams reopened next are opened inside it; handed back last.
    if wanted.is_none() {
        ndk::audio::set_voice_communication(false);
    }
    router(match wanted {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    });
    if wanted.is_some() {
        ndk::audio::set_voice_communication(true);
    }
    if flips {
        crate::audio_engine::AudioEngine::kick();
    }
    *applied = wanted;
}

fn router(mode: i32) {
    let out = super::utils::with_android_context(|env, ctx| {
        let class = class(env, ctx)?;
        env.call_static_method(&class, "apply", "(Landroid/content/Context;I)V", &[JValue::Object(ctx), JValue::Int(mode)])
            .map(|_| ())
            .map_err(|e| format!("{e:?}"))
    });
    if let Err(e) = out {
        vector_core::log_warn!("[calls] CallAudioRouter.apply: {e}");
    }
}

/// None outside a call.
pub fn route() -> Option<Route> {
    ROUTE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn set_route(route: &str) -> Result<(), String> {
    let picked = super::utils::with_android_context(|env, ctx| {
        let class = class(env, ctx)?;
        let route = env.new_string(route).map_err(|e| format!("{e:?}"))?;
        env.call_static_method(&class, "setRoute", "(Landroid/content/Context;Ljava/lang/String;)Z", &[JValue::Object(ctx), JValue::Object(&route)])
            .and_then(|v| v.z())
            .map_err(|e| format!("{e:?}"))
    })?;
    picked.then_some(()).ok_or_else(|| "That isn't available right now".to_string())
}

fn class<'a>(env: &mut JNIEnv<'a>, ctx: &JObject<'a>) -> Result<JClass<'a>, String> {
    let loader = env
        .call_method(ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
        .and_then(|v| v.l())
        .map_err(|e| format!("{e:?}"))?;
    let name = env.new_string("io.vectorapp.CallAudioRouter").map_err(|e| format!("{e:?}"))?;
    let class = env
        .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])
        .and_then(|v| v.l())
        .map_err(|e| format!("{e:?}"))?;
    Ok(JClass::from(class))
}

#[no_mangle]
pub extern "C" fn Java_io_vectorapp_CallAudioRouter_nativeRouteChanged(mut env: JNIEnv, _class: JClass, current: JString, available: JString) {
    super::utils::jni_fence("Java_io_vectorapp_CallAudioRouter_nativeRouteChanged", || (), move || {
        let current: String = env.get_string(&current).map(Into::into).unwrap_or_default();
        let available: String = env.get_string(&available).map(Into::into).unwrap_or_default();
        let route = (!current.is_empty()).then(|| Route {
            current,
            available: available.split(',').filter(|r| !r.is_empty()).map(str::to_string).collect(),
        });
        *ROUTE.lock().unwrap_or_else(|e| e.into_inner()) = route.clone();
        vector_core::emit_event("call_route", &route);
    })
}
