//! OS notifications for what arrives while Vector isn't in front, through the desktop client's
//! notifier and its native backends. Which messages ring follows the desktop's event handlers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use serde_json::json;
use vector_core::notify::{self, MessageClass};
use vector_core::{Message, STATE, VectorCore, db};
use vector_media::services::native_notify;
use vector_media::services::{NotificationData, strip_content_for_preview};

static FOCUSED: AtomicBool = AtomicBool::new(true);

static PINNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The open chat shows its newest message: only then has an arrival been seen.
pub fn set_pinned(pinned: bool) {
    PINNED.store(pinned, Ordering::Relaxed);
}

pub fn pinned() -> bool {
    PINNED.load(Ordering::Relaxed)
}

pub fn set_focused(focused: bool) {
    FOCUSED.store(focused, Ordering::Relaxed);
}

pub fn focused() -> bool {
    FOCUSED.load(Ordering::Relaxed)
}

/// Hooks the notifier up to this client. macOS and Windows register with the OS here, on the
/// main thread, so a click that launches Vector finds it; Linux waits for its first message.
pub fn install() {
    // The Tauri client's identity: this client replaces it, so its toast clicks land here.
    // i18n-exempt: the product name
    tauri::set_config(tauri::Config { identifier: "io.vectorapp".into(), product_name: Some("Vector".into()) });
    tauri::set_window(tauri::WindowHooks {
        focused: || FOCUSED.load(Ordering::Relaxed),
        reveal: || super::emit("notify_reveal", serde_json::Value::Null),
    });
    vector_media::notify_host::set(vector_media::notify_host::Host {
        mark_read: |chat| Box::pin(mark_read(chat)),
        mute: |chat, ms| Box::pin(super::prefs::set_notify_mute(chat, ms)),
        reply: |chat, text| Box::pin(reply(chat, text)),
        open: |chat, message, account| {
            super::emit(
                "deep_link_action",
                json!({ "action_type": "chat", "target": chat, "message": message, "account": account }),
            )
        },
        sound: ring,
    });
    #[cfg(not(target_os = "linux"))]
    init();
}

fn init() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        if let Some(app) = vector_media::TAURI_APP.get() {
            native_notify::init(app);
        }
    });
}

/// The freedesktop server's signals arrive on the thread that subscribed, which must run a
/// GLib main loop.
fn ready() {
    #[cfg(target_os = "linux")]
    {
        static DONE: OnceLock<()> = OnceLock::new();
        DONE.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel();
            let posted = crate::miniapp::on_glib(move || {
                init();
                let _ = tx.send(());
            });
            if posted.is_ok() {
                let _ = rx.recv_timeout(std::time::Duration::from_secs(5));
            }
        });
    }
}

async fn mark_read(chat_id: String) -> bool {
    super::messaging::mark_as_read(chat_id, None).await
}

async fn reply(chat_id: String, text: String) -> Result<String, String> {
    if chat_id.starts_with("npub1") {
        super::messaging::send(chat_id, text, String::new(), None).await?;
    } else {
        super::community::send(chat_id.clone(), text, String::new(), None).await?;
        mark_read(chat_id).await;
    }
    Ok(String::new())
}

fn ring() {
    if super::prefs::sql("notif_global_mute").as_deref() == Some("true") {
        return;
    }
    let sound = super::prefs::sql("notif_sound").unwrap_or_default();
    super::sound::play(super::sound::Sound::parse(&sound));
}

/// Core's privacy transform, with its English placeholders said in the reader's language.
fn hide(data: &mut NotificationData, privacy: notify::ContentPrivacy) {
    use notify::ContentPrivacy as P;
    data.apply_content_privacy(privacy);
    match privacy {
        P::Full => {}
        P::HideSender if data.group_name.is_some() => {}
        P::HideSender => data.title = crate::coretext::say(&data.title).into_owned(),
        P::HideContent | P::HideAll => data.body = crate::coretext::say(&data.body).into_owned(),
    }
}

/// The desktop notifier's path, applying privacy here: it would apply it again in English.
fn show(mut data: NotificationData) {
    if FOCUSED.load(Ordering::Relaxed) {
        return;
    }
    ready();
    hide(&mut data, notify::ContentPrivacy::load());
    // The phone's message channel carries its own sound.
    #[cfg(target_os = "android")]
    crate::android::background_sync::post_notification_jni(
        &data.title,
        &data.body,
        data.avatar_path.as_deref(),
        data.chat_id.as_deref(),
        data.sender_name.as_deref(),
        data.group_name.as_deref(),
        data.group_avatar_path.as_deref(),
    );
    #[cfg(not(target_os = "android"))]
    {
        ring();
        if let Err(e) = native_notify::show(&data) {
            log::warn!("notification: {e}");
        }
    }
}

fn file_body(msg: &Message) -> String {
    let ext = msg.attachments.first().map(|a| a.extension.as_str()).unwrap_or("file");
    crate::t::chats_preview_sent(crate::data::file_type(ext).1.as_ref()).into()
}

/// A new DM or file from someone else, unless the chat is set quieter than everything.
pub async fn direct(chat_id: &str, msg: &Message) {
    let rings = STATE
        .lock()
        .await
        .get_chat(chat_id)
        .is_none_or(|c| notify::ring_for_chat(c) == notify::NotifyLevel::All);
    if !rings {
        return;
    }
    let gate = notify::StreamGate::load();
    let (name, body, avatar) = {
        let state = STATE.lock().await;
        let sender = notify::sender(&state, &gate, chat_id, &crate::t::notify_new_message());
        let body = if msg.attachments.is_empty() {
            strip_content_for_preview(&notify::resolve_mentions(&msg.content, &state, &gate))
        } else {
            file_body(msg)
        };
        (sender.name, body, sender.avatar)
    };
    show(
        NotificationData::direct_message(name, body, avatar, chat_id.to_string())
            .with_message_id(msg.id.clone())
            .with_expiry(msg.expiration),
    );
}

/// What a contact's push says, for a phone whose connection was down: their words when their
/// app sealed some in, else that they wrote.
#[cfg(target_os = "android")]
pub async fn pushed(npub: &str, text: String) {
    let (name, avatar) = known_as(npub).await;
    let body = if text.is_empty() { crate::t::core_notify_sent_you_a_message().to_string() } else { text };
    show(NotificationData::direct_message(name, body, avatar, npub.to_string()));
}

/// A push no contact here vouches for: that something arrived, and nothing it claims.
#[cfg(target_os = "android")]
pub fn pushed_unknown() {
    crate::android::background_sync::post_notification_jni(
        &crate::t::notify_new_message(),
        &crate::t::notify_you_have_a_new_message(),
        None,
        None,
        None,
        None,
        None,
    );
}

/// The name and picture `npub` goes by here; a stranger by the start of their npub.
#[cfg(target_os = "android")]
pub async fn known_as(npub: &str) -> (String, Option<String>) {
    let gate = notify::StreamGate::load();
    let fallback = format!("{}…", &npub[..npub.len().min(14)]);
    let state = STATE.lock().await;
    let sender = notify::sender(&state, &gate, npub, &fallback);
    (sender.name, sender.avatar)
}

/// A Community message rings by its channel's level: everything, or only a ping (an
/// @mention, a reply to us, a standing Role mention, or an allowed @everyone). A blocked or
/// DM-muted sender, or a post its readers collapse, never rings.
pub async fn community(chat_id: &str, msg: &Message) {
    if msg.mine {
        return;
    }
    let sender_npub = msg.npub.as_deref().unwrap_or_default();
    if sender_npub.is_empty() {
        return;
    }
    let Some(community_id) = db::community::community_id_for_channel(chat_id).ok().flatten() else {
        return;
    };
    if VectorCore.post_silenced(&community_id, chat_id, sender_npub) {
        return;
    }
    let everyone = msg.mentions_everyone()
        && notify::everyone_allowed(Some(&community_id))
        && VectorCore
            .member_has_permission(&community_id, sender_npub, vector_core::community::roles::Permissions::MENTION_EVERYONE)
            .unwrap_or(false);
    let role = VectorCore.role_mentions_ping_me(&community_id, sender_npub, &msg.content, &msg.role_mentions);
    let reply = !msg.replied_to.is_empty() && db::events::is_own_event(&msg.replied_to);
    let class = if everyone {
        MessageClass::Everyone
    } else if msg.mentions_me() || reply || role {
        MessageClass::Mention
    } else {
        MessageClass::Normal
    };
    let rings = {
        let state = STATE.lock().await;
        let Some(chat) = state.get_chat(chat_id) else { return };
        let quiet = state.get_profile(sender_npub).is_some_and(|p| p.flags.is_blocked())
            || state.get_chat(sender_npub).is_some_and(notify::muted_for_chat);
        !quiet && notify::passes(notify::ring_for_chat(chat), class)
    };
    if !rings {
        return;
    }
    let gate = notify::StreamGate::load();
    let nickname = VectorCore.member_nickname(&community_id, sender_npub);
    let (sender, community_name, avatar, content) = {
        let state = STATE.lock().await;
        let notify::Sender { name, avatar } = notify::sender_in(&state, &gate, sender_npub, &crate::t::notify_someone(), nickname);
        let community_name = state
            .get_chat(chat_id)
            .and_then(|c| c.metadata.get_name().map(str::to_string))
            .unwrap_or_else(|| crate::t::community_unnamed().into());
        let content = if msg.attachments.is_empty() {
            strip_content_for_preview(&notify::resolve_mentions(&msg.content, &state, &gate))
        } else {
            file_body(msg)
        };
        (name, community_name, avatar, content)
    };
    let icon = db::community::community_image(&community_id, false)
        .ok()
        .flatten()
        .and_then(|icon| super::images::cached_avatar(&icon.url));
    show(
        NotificationData::community_message(sender, community_name, content, avatar, icon, chat_id.to_string())
            .with_message_id(msg.id.clone())
            .with_sender(sender_npub.to_string())
            .with_expiry(msg.expiration),
    );
}

/// What the dock or taskbar badge shows: chats that ring for everything, as on desktop.
pub fn recount() {
    let Some(rt) = super::RT.get() else { return };
    let _runtime = rt.enter();
    db::spawn_bound(async {
        if !STATE.lock().await.unread_seeded {
            let counts = db::events::unread_counts().await.unwrap_or_default();
            STATE.lock().await.unread_seed(counts);
        }
        let total = STATE.lock().await.sum_unread();
        vector_core::emit_event("unread_total", &json!(total));
    });
}

/// The chat was read: its notifications go.
pub fn read(chat_id: &str) {
    #[cfg(target_os = "android")]
    crate::android::background_sync::cancel_notification_jni(chat_id);
    native_notify::remove_chat(chat_id);
}

pub fn deleted(message_id: &str) {
    native_notify::remove_message(message_id);
}

pub fn blocked(npub: &str) {
    // The phone's shade keeps one notification per chat, and a DM's chat is its sender.
    #[cfg(target_os = "android")]
    crate::android::background_sync::cancel_notification_jni(npub);
    native_notify::remove_sender(npub);
}

/// The account is leaving this machine's screen: its notifications leave with it.
pub fn account_gone(npub: &str) {
    #[cfg(target_os = "android")]
    crate::android::background_sync::cancel_all_message_notifications_jni();
    native_notify::remove_account(npub);
}

/// A reply typed into a notification before its account was logged in.
pub fn logged_in() {
    native_notify::flush_pending_reply();
}

#[cfg(test)]
mod privacy_tests {
    use super::*;
    use vector_core::notify::ContentPrivacy as P;

    fn dm() -> NotificationData {
        // i18n-exempt: test fixture
        NotificationData::direct_message("Alice".into(), "see you at 8".into(), None, "npub1x".into())
    }

    fn group() -> NotificationData {
        // i18n-exempt: test fixture
        NotificationData::community_message("Alice".into(), "Book Club".into(), "see you at 8".into(), None, None, "chan".into())
    }

    /// In English it says exactly what core's own transform says, and every placeholder it
    /// puts on screen has a translation.
    #[test]
    fn hidden_previews_match_core_and_are_translated() {
        for privacy in [P::Full, P::HideContent, P::HideSender, P::HideAll] {
            for make in [dm, group] {
                let (mut ours, mut core) = (make(), make());
                hide(&mut ours, privacy);
                core.apply_content_privacy(privacy);
                assert_eq!((&ours.title, &ours.body, &ours.sender_name), (&core.title, &core.body, &core.sender_name));
                let placeholder = match privacy {
                    P::Full => None,
                    P::HideSender if core.group_name.is_some() => None,
                    P::HideSender => Some(core.title.clone()),
                    P::HideContent | P::HideAll => Some(core.body.clone()),
                };
                if let Some(p) = placeholder {
                    assert!(matches!(crate::coretext::say(&p), std::borrow::Cow::Owned(_)), "no translation for {p:?}");
                }
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// Needs a session bus with a notification server; run under `dbus-launch` beside one.
    #[test]
    #[ignore]
    fn shows_and_retracts_through_the_session_bus() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        tauri::async_runtime::set(rt.handle().clone());
        tauri::set_data_dir(std::env::temp_dir().join(format!("vector-notify-{}", std::process::id())));
        let _ = vector_media::TAURI_APP.set(tauri::AppHandle::new(|_, _| {}));
        install();
        set_focused(false);
        let chat = "npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq".to_string();
        // i18n-exempt: test fixture
        show(NotificationData::direct_message("Alice".into(), "hi **there**".into(), None, chat.clone()).with_message_id("ab".into()));
        std::thread::sleep(std::time::Duration::from_secs(1));
        read(&chat);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
