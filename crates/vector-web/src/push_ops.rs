//! Sealed push on the web: the page makes the browser's push subscription and hands it
//! here; tickets go out to contacts, and the page's service worker gets the contact list
//! it names and verifies senders with. See `vector_core::push`.

use std::future::Future;
use std::pin::Pin;

use serde_json::{json, Value};
use vector_core::push::{self, Device};


use crate::commands::Args;

pub fn dispatch<'a>(
    cmd: &'a str,
    a: &'a Args,
) -> Pin<Box<dyn Future<Output = Option<Result<Value, String>>> + 'a>> {
    Box::pin(async move {
        let result = match cmd {
            "push_set_device" => set_device(a).await,
            "push_disable" => push::disable().await.map(|_| Value::Null),
            "push_worker_state" => Ok(worker_state().await),
            "push_viewing" => push::viewing(a.opt_str("npub").filter(|n| n.starts_with("npub1"))).await.map(|_| Value::Null),
            _ => return None,
        };
        Some(result)
    })
}

async fn set_device(a: &Args) -> Result<Value, String> {
    push::adopt(a.de::<Device>("device")?).await?;
    Ok(worker_state().await)
}

/// What the service worker reads from Cache Storage: who each handle is, and how much of a
/// notification the user lets show, Streamer Mode included.
async fn worker_state() -> Value {
    json!({
        "v": 1,
        "enabled": push::device().is_some(),
        "privacy": vector_core::notify::ContentPrivacy::load().as_str(),
        "streamer": vector_core::synced_prefs::load_settings().streamer.on,
        "contacts": push::worker_contacts().await,
    })
}
