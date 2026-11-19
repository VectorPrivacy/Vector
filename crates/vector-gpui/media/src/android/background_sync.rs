//! What the shared JNI modules take from the desktop client's background service: the process's
//! VM and application context, whether the activity is on screen, and the message notifications
//! `VectorNotificationService` posts. The client itself stays connected while the service keeps
//! the process alive, so there is no second, standalone client here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::sys::jboolean;
use jni::{JNIEnv, JavaVM};
use nostr_sdk::prelude::ToBech32;

pub static BG_JAVA_VM: OnceLock<JavaVM> = OnceLock::new();
pub static BG_APP_CONTEXT: OnceLock<GlobalRef> = OnceLock::new();
static FOREGROUND: AtomicBool = AtomicBool::new(false);

pub fn is_activity_in_foreground() -> bool {
    FOREGROUND.load(Ordering::Acquire)
}

/// Runs `f` with `VectorNotificationService`'s class and the application context, on any thread.
fn with_service<R>(
    what: &str,
    f: impl FnOnce(&mut JNIEnv, &JClass, &JObject) -> jni::errors::Result<R>,
) -> Option<R> {
    let (vm, context) = (BG_JAVA_VM.get()?, BG_APP_CONTEXT.get()?);
    let mut env = vm.attach_current_thread().ok()?;
    let context = context.as_obj();
    let out = (|| {
        // The context's loader: a framework class's own is the boot loader, which has none of ours.
        let loader = env.call_method(context, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?.l()?;
        let name = env.new_string("io.vectorapp.VectorNotificationService")?;
        let class = env
            .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])?
            .l()?;
        f(&mut env, &JClass::from(class), context)
    })();
    match out {
        Ok(r) => Some(r),
        Err(e) => {
            let _ = env.exception_clear();
            log::warn!("notification {what}: {e}");
            None
        }
    }
}

/// One message in its chat's notification. The caller has already applied the reader's privacy
/// choice, in their language.
pub fn post_notification_jni(
    title: &str,
    body: &str,
    avatar_path: Option<&str>,
    chat_id: Option<&str>,
    sender_name: Option<&str>,
    group_name: Option<&str>,
    group_avatar_path: Option<&str>,
) {
    if is_activity_in_foreground() {
        return;
    }
    let avatar = notification_image(avatar_path.map(str::to_string)).unwrap_or_default();
    let group_avatar = notification_image(group_avatar_path.map(str::to_string)).unwrap_or_default();
    let texts: [&str; 7] = [
        title,
        body,
        &avatar,
        chat_id.unwrap_or_default(),
        sender_name.unwrap_or_default(),
        group_name.unwrap_or_default(),
        &group_avatar,
    ];
    with_service("post", |env, class, context| {
        let mut args = vec![JValue::Object(context)];
        let strings = texts.iter().map(|s| env.new_string(s)).collect::<Result<Vec<_>, _>>()?;
        args.extend(strings.iter().map(JValue::from));
        env.call_static_method(
            class,
            "showMessageNotification",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
            &args,
        )
        .map(drop)
    });
}

/// The chat was read, here or elsewhere: its notification goes.
pub fn cancel_notification_jni(chat_id: &str) {
    if chat_id.is_empty() {
        return;
    }
    with_service("cancel", |env, class, context| {
        let chat = env.new_string(chat_id)?;
        env.call_static_method(
            class,
            "cancelNotification",
            "(Landroid/content/Context;Ljava/lang/String;)V",
            &[JValue::Object(context), JValue::Object(&chat)],
        )
        .map(drop)
    });
}

/// Every message notification, for an account leaving the phone's screen.
pub fn cancel_all_message_notifications_jni() {
    with_service("cancel all", |env, class, context| {
        env.call_static_method(class, "cancelAllMessageNotifications", "(Landroid/content/Context;)V", &[
            JValue::Object(context),
        ])
        .map(drop)
    });
}

/// A sealed cache file opened into a plain one the shade can read, kept until the source changes.
pub(crate) fn notification_image(path: Option<String>) -> Option<String> {
    let path = path.filter(|p| !p.is_empty())?;
    let source = std::path::Path::new(&path);
    if !vector_core::vcache::is_sealed(source) {
        return Some(path);
    }
    let dir = vector_core::db::get_app_data_dir().ok()?.join("notifications").join("avatars");
    std::fs::create_dir_all(&dir).ok()?;
    let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("img");
    let modified = std::fs::metadata(source).and_then(|m| m.modified()).ok()?;
    let stamp = modified.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let key = vector_core::crypto::sha256_hex(format!("{path}\n{stamp}").as_bytes());
    let out = dir.join(format!("{}.{ext}", &key[..32]));
    if !out.is_file() {
        let bytes = vector_core::vcache::read(source).ok()?;
        let tmp = dir.join(format!(".{}.{:08x}.tmp", &key[..32], rand::random::<u32>()));
        if std::fs::write(&tmp, &*bytes).and_then(|_| std::fs::rename(&tmp, &out)).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return None;
        }
    }
    Some(out.to_string_lossy().into_owned())
}

fn string(env: &mut JNIEnv, s: &JString) -> String {
    env.get_string(s).map(Into::into).unwrap_or_default()
}

/// `MainActivity.onCreate`: the activity for calls that need one, and the process's VM and
/// application context for the rest.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_MainActivity_nativeCreated(mut env: JNIEnv, activity: JObject) {
    super::utils::jni_fence("Java_io_vectorapp_MainActivity_nativeCreated", || (), move || {
        super::utils::set_activity(&mut env, &activity);
        if BG_JAVA_VM.get().is_none() {
            if let Ok(vm) = env.get_java_vm() {
                let _ = BG_JAVA_VM.set(vm);
            }
        }
        if BG_APP_CONTEXT.get().is_none() {
            let app = env.call_method(&activity, "getApplicationContext", "()Landroid/content/Context;", &[]).and_then(|v| v.l());
            match app.and_then(|app| env.new_global_ref(app)) {
                Ok(global) => {
                    let _ = BG_APP_CONTEXT.set(global);
                }
                Err(_) => {
                    let _ = env.exception_clear();
                }
            }
        }
    })
}

#[no_mangle]
pub extern "C" fn Java_io_vectorapp_MainActivity_nativeDestroyed(mut env: JNIEnv, activity: JObject) {
    super::utils::jni_fence("Java_io_vectorapp_MainActivity_nativeDestroyed", || (), move || {
        super::utils::clear_activity(&mut env, &activity);
    })
}

/// Resumed or paused: nothing is posted while the app is on screen.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_MainActivity_nativeForeground(_env: JNIEnv, _activity: JObject, on: jboolean) {
    FOREGROUND.store(on != 0, Ordering::Release);
}

/// A tap that launched the app before the client could take it.
static PENDING_LINK: Mutex<Option<vector_core::deep_link::DeepLinkAction>> = Mutex::new(None);

/// Keeps `action` for the client when it isn't up yet; false when it can take it now.
pub(crate) fn hold_link(action: &vector_core::deep_link::DeepLinkAction) -> bool {
    let mut pending = PENDING_LINK.lock().unwrap_or_else(|e| e.into_inner());
    if crate::notify_host::get().is_some() {
        return false;
    }
    *pending = Some(action.clone());
    true
}

/// The client has installed its hooks: a tap that came first is taken now.
pub(crate) fn deliver_pending_link() {
    let action = PENDING_LINK.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let (Some(action), Some(app)) = (action, crate::TAURI_APP.get()) {
        use tauri::Emitter;
        let _ = app.emit("deep_link_action", &action);
    }
}

/// A message notification was tapped: its chat opens, once the client is up to open it.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_MainActivity_nativeOnNotificationTap(mut env: JNIEnv, _class: JClass, chat_id: JString) {
    super::utils::jni_fence("Java_io_vectorapp_MainActivity_nativeOnNotificationTap", || (), move || {
        let chat_id = string(&mut env, &chat_id);
        if chat_id.is_empty() {
            return;
        }
        let action = vector_core::deep_link::DeepLinkAction {
            action_type: "chat".into(),
            target: chat_id.clone(),
            ..Default::default()
        };
        if let (false, Some(host)) = (hold_link(&action), crate::notify_host::get()) {
            (host.open)(chat_id, None, None);
        }
    })
}

#[no_mangle]
pub extern "C" fn Java_io_vectorapp_NotificationActionReceiver_nativeMarkAsRead(mut env: JNIEnv, _class: JClass, chat_id: JString) {
    super::utils::jni_fence("Java_io_vectorapp_NotificationActionReceiver_nativeMarkAsRead", || (), move || {
        let chat_id = string(&mut env, &chat_id);
        let Some(host) = crate::notify_host::get().filter(|_| !chat_id.is_empty()) else { return };
        // spawn-detached: a read for whichever account is live, as the notification was.
        tauri::async_runtime::spawn(vector_core::db::scoped((host.mark_read)(chat_id)));
    })
}

/// An inline reply: sent, then shown under the conversation so the shade's spinner clears.
#[no_mangle]
pub extern "C" fn Java_io_vectorapp_NotificationActionReceiver_nativeSendReply(
    mut env: JNIEnv,
    _class: JClass,
    chat_id: JString,
    content: JString,
) {
    super::utils::jni_fence("Java_io_vectorapp_NotificationActionReceiver_nativeSendReply", || (), move || {
        let (chat_id, content) = (string(&mut env, &chat_id), string(&mut env, &content));
        let Some(host) = crate::notify_host::get().filter(|_| !chat_id.is_empty() && !content.is_empty()) else {
            return;
        };
        // spawn-detached: a reply for whichever account is live, as the notification was.
        tauri::async_runtime::spawn(vector_core::db::scoped(async move {
            if let Err(e) = (host.reply)(chat_id.clone(), content.clone()).await {
                log::warn!("notification reply: {e}");
                return;
            }
            let (me, avatar) = own_label().await;
            post_notification_jni(&me, &content, avatar.as_deref(), Some(&chat_id), Some(&me), None, None);
        }));
    })
}

/// How the shade names our own replies: "Me", with the profile's name when it has one.
async fn own_label() -> (String, Option<String>) {
    let Some(npub) = vector_core::my_public_key().and_then(|pk| pk.to_bech32().ok()) else {
        return ("Me".into(), None);
    };
    let state = vector_core::STATE.lock().await;
    let Some(profile) = state.get_profile(&npub) else { return ("Me".into(), None) };
    let name = [profile.display_name(), profile.name()].into_iter().find(|n| !n.is_empty());
    let label = name.map_or_else(|| "Me".to_string(), |n| format!("Me ({n})"));
    (label, profile.has_avatar_cached().then(|| profile.avatar_cached().into_owned()))
}
