//! OS notifications for what arrives while Vector isn't in front, through the desktop client's
//! notifier and its native backends. Which messages ring follows the desktop's event handlers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use serde_json::json;
use vector_core::notify::{self, MessageClass};
use vector_core::{Message, STATE, VectorCore, db};
use vector_media::services::native_notify;
use vector_media::services::{NotificationData, show_notification_generic, strip_content_for_preview};

static FOCUSED: AtomicBool = AtomicBool::new(true);

pub fn set_focused(focused: bool) {
    FOCUSED.store(focused, Ordering::Relaxed);
}

/// Hooks the notifier up to this client. macOS and Windows register with the OS here, on the
/// main thread, so a click that launches Vector finds it; Linux waits for its first message.
pub fn install() {
    // Its own identity: the Windows activator registered under the desktop client's would
    // send that client's toast clicks here.
    tauri::set_config(tauri::Config { identifier: "io.vectorapp.gpui".into(), product_name: Some("Vector".into()) });
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
        sound: || {
            if super::prefs::sql("notif_global_mute").as_deref() == Some("true") {
                return;
            }
            let sound = super::prefs::sql("notif_sound").unwrap_or_default();
            super::sound::play(super::sound::Sound::parse(&sound));
        },
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
/// GLib main loop: the GTK thread the Mini App windows use.
fn ready() {
    #[cfg(target_os = "linux")]
    {
        static DONE: OnceLock<()> = OnceLock::new();
        DONE.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel();
            let posted = crate::miniapp::on_gtk(move || {
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
    if !super::messaging::mark_as_read(chat_id.clone(), None).await {
        return false;
    }
    let last_read = STATE
        .lock()
        .await
        .get_chat(&chat_id)
        .map(|c| vector_core::compact::decode_message_id(&c.last_read));
    if let Some(last_read) = last_read {
        vector_core::emit_event("chat_mark_read", &json!({ "chat_id": chat_id, "last_read": last_read }));
    }
    true
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

fn show(data: NotificationData) {
    if FOCUSED.load(Ordering::Relaxed) {
        return;
    }
    ready();
    show_notification_generic(data);
}

fn file_body(msg: &Message) -> String {
    let ext = msg.attachments.first().map(|a| a.extension.as_str()).unwrap_or("file");
    format!("Sent a {}", crate::data::file_type(ext).1)
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
        let sender = notify::sender(&state, &gate, chat_id, "New Message");
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
        let notify::Sender { name, avatar } = notify::sender_in(&state, &gate, sender_npub, "Someone", nickname);
        let community_name = state
            .get_chat(chat_id)
            .and_then(|c| c.metadata.get_name().map(str::to_string))
            .unwrap_or_else(|| "Community".to_string());
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

/// The chat was read: its notifications go.
pub fn read(chat_id: &str) {
    native_notify::remove_chat(chat_id);
}

pub fn deleted(message_id: &str) {
    native_notify::remove_message(message_id);
}

pub fn blocked(npub: &str) {
    native_notify::remove_sender(npub);
}

/// The account is leaving this machine's screen: its notifications leave with it.
pub fn account_gone(npub: &str) {
    native_notify::remove_account(npub);
}

/// A reply typed into a notification before its account was logged in.
pub fn logged_in() {
    native_notify::flush_pending_reply();
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
        show(NotificationData::direct_message("Alice".into(), "hi **there**".into(), None, chat.clone()).with_message_id("ab".into()));
        std::thread::sleep(std::time::Duration::from_secs(1));
        read(&chat);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
