//! Sealed push on the web: the page makes the browser's push subscription and hands it
//! here; tickets go out to contacts, and the page's service worker gets the contact list
//! it names and verifies senders with. See `vector_core::push`.

use std::future::Future;
use std::pin::Pin;

use serde_json::{json, Value};
use vector_core::push::{self, Device};
use vector_core::{db, ChatType, STATE};

use crate::commands::Args;

/// Contacts handed a ticket the moment notifications are turned on. Everyone else gets one
/// with our next message to them.
const RECENT_CONTACTS: usize = 30;

pub fn dispatch<'a>(
    cmd: &'a str,
    a: &'a Args,
) -> Pin<Box<dyn Future<Output = Option<Result<Value, String>>> + 'a>> {
    Box::pin(async move {
        let result = match cmd {
            "push_set_device" => set_device(a).await,
            "push_disable" => push::disable().await.map(|_| Value::Null),
            "push_worker_state" => Ok(worker_state().await),
            _ => return None,
        };
        Some(result)
    })
}

async fn set_device(a: &Args) -> Result<Value, String> {
    let device: Device = a.de("device")?;
    let fresh = push::device().as_ref() != Some(&device);
    push::set_device(device).await?;
    if fresh {
        let recent = recent_contacts().await;
        push::prepare(&recent).await?;
        db::spawn_bound(async move {
            // Spaced out: a burst of wraps at once ties every recipient to the sender.
            for npub in recent {
                match push::share_with(&npub).await {
                    Ok(true) => vector_core::rt::time::sleep(std::time::Duration::from_millis(1500)).await,
                    Ok(false) => {}
                    Err(e) => vector_core::log_warn!("[Push] ticket to {}: {}", npub, e),
                }
            }
        });
    }
    Ok(worker_state().await)
}

async fn recent_contacts() -> Vec<String> {
    let state = STATE.lock().await;
    let mut chats: Vec<(u64, String)> = state
        .chats
        .iter()
        .filter(|c| matches!(c.chat_type(), ChatType::DirectMessage))
        .filter(|c| !state.get_profile(c.id()).is_some_and(|p| p.flags.is_blocked()))
        .map(|c| (c.last_message_time().unwrap_or(0), c.id().clone()))
        .collect();
    chats.sort_by(|a, b| b.0.cmp(&a.0));
    chats.into_iter().take(RECENT_CONTACTS).map(|(_, id)| id).collect()
}

/// What the service worker reads from Cache Storage: who each handle is, and how much of a
/// notification the user lets show.
async fn worker_state() -> Value {
    let privacy = db::settings::get_sql_setting("notif_content_privacy".into()).ok().flatten().unwrap_or_else(|| "full".into());
    json!({
        "v": 1,
        "enabled": push::device().is_some(),
        "privacy": privacy,
        "contacts": push::worker_contacts().await,
    })
}
