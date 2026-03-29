//! Message sending — NIP-17 gift-wrapped DMs (text and file attachments).
//!
//! This is the core send pipeline used by all Vector interfaces (GUI, CLI, SDK).
//! Clients provide a `SendCallback` for status notifications (pending/sent/failed/progress)
//! and a `SendConfig` for retry/cancel behavior.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use nostr_sdk::prelude::*;

use crate::state::{NOSTR_CLIENT, MY_SECRET_KEY, MY_PUBLIC_KEY, STATE};
use crate::types::{Message, Attachment};
use crate::crypto;

// ============================================================================
// SendCallback — Client notification trait
// ============================================================================

/// Callbacks invoked during the DM send pipeline.
///
/// Each method has a default no-op so simple callers (CLI, bots, tests)
/// implement only what they need. Methods are synchronous and non-fallible
/// by design — they should never block the send pipeline.
///
/// Exception: `on_upload_progress` returns `Result` — return `Err` to cancel.
pub trait SendCallback: Send + Sync {
    /// Message created and added to STATE as pending.
    fn on_pending(&self, _chat_id: &str, _msg: &Message) {}

    /// Attachment preview added to pending message in STATE.
    fn on_attachment_preview(&self, _chat_id: &str, _msg: &Message) {}

    /// File upload progress. Return Err("...") to cancel the upload.
    fn on_upload_progress(
        &self,
        _pending_id: &str,
        _percentage: u8,
        _bytes_sent: u64,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Upload complete, attachment URL now available.
    fn on_upload_complete(&self, _chat_id: &str, _pending_id: &str, _attachment_id: &str, _url: &str) {}

    /// Message successfully delivered to at least one relay.
    /// `old_id` is the pending ID, `msg` has the real event ID.
    fn on_sent(&self, _chat_id: &str, _old_id: &str, _msg: &Message) {}

    /// Message delivery failed after all retry attempts.
    fn on_failed(&self, _chat_id: &str, _old_id: &str, _msg: &Message) {}

    /// Persist message to database. Default is no-op.
    /// Tauri implements this to call save_message + save_slim_chat.
    fn on_persist(&self, _chat_id: &str, _msg: &Message) {}
}

/// No-op callback for headless/CLI/test use.
pub struct NoOpSendCallback;
impl SendCallback for NoOpSendCallback {}

// ============================================================================
// SendConfig — Per-call configuration
// ============================================================================

/// Configuration for a send operation.
pub struct SendConfig {
    /// Max gift-wrap send attempts (default: 1).
    pub max_send_attempts: u32,
    /// Delay between send retries (default: 5 seconds).
    pub retry_delay: std::time::Duration,
    /// Send copy to own inbox for recovery/sync (default: false).
    pub self_send: bool,
    /// Cancel token for file uploads — set to true to abort.
    pub cancel_token: Option<Arc<AtomicBool>>,
    /// Max Blossom upload retries per server (default: 3).
    pub upload_retries: u32,
    /// Delay between upload retries (default: 2 seconds).
    pub upload_retry_delay: std::time::Duration,
}

impl Default for SendConfig {
    fn default() -> Self {
        Self {
            max_send_attempts: 1,
            retry_delay: std::time::Duration::from_secs(5),
            self_send: false,
            cancel_token: None,
            upload_retries: 3,
            upload_retry_delay: std::time::Duration::from_secs(2),
        }
    }
}

impl SendConfig {
    /// Preset for GUI clients (12 retries, self-send enabled).
    pub fn gui() -> Self {
        Self {
            max_send_attempts: 12,
            self_send: true,
            ..Default::default()
        }
    }

    /// Preset for headless/background mode (3 retries, self-send enabled).
    pub fn headless() -> Self {
        Self {
            max_send_attempts: 3,
            self_send: true,
            ..Default::default()
        }
    }
}

// ============================================================================
// SendResult
// ============================================================================

/// Result of sending a message.
#[derive(serde::Serialize, Clone, Debug)]
pub struct SendResult {
    /// The pending ID used while sending
    pub pending_id: String,
    /// The real event ID after successful send (None if failed)
    pub event_id: Option<String>,
    /// The chat ID (receiver npub for DMs)
    pub chat_id: String,
}

// ============================================================================
// Internal: retry gift-wrap send
// ============================================================================

/// Shared tail of send_dm / send_file_dm / send_rumor_dm:
/// gift-wrap → retry loop → finalize/fail → self-send.
async fn retry_send_gift_wrap(
    client: &Client,
    receiver: &PublicKey,
    receiver_npub: &str,
    pending_id: &str,
    rumor: UnsignedEvent,
    event_id: &str,
    config: &SendConfig,
    callback: &dyn SendCallback,
) -> Result<SendResult, String> {
    let my_pk = *MY_PUBLIC_KEY.get().ok_or("Public key not set")?;

    for attempt in 0..config.max_send_attempts {
        match crate::inbox_relays::send_gift_wrap(client, receiver, rumor.clone(), []).await {
            Ok(output) if !output.success.is_empty() => {
                // At least one relay accepted — success
                let finalized = {
                    let mut state = STATE.lock().await;
                    state.finalize_pending_message(receiver_npub, pending_id, event_id)
                };

                if let Some((_old_id, ref finalized_msg)) = finalized {
                    callback.on_sent(receiver_npub, pending_id, finalized_msg);
                    callback.on_persist(receiver_npub, finalized_msg);
                }

                // Self-send for recovery (fire-and-forget)
                if config.self_send {
                    let client = client.clone();
                    let my_pk_clone = my_pk;
                    let rumor_clone = rumor.clone();
                    tokio::spawn(async move {
                        let _ = crate::inbox_relays::send_gift_wrap(
                            &client, &my_pk_clone, rumor_clone, [],
                        ).await;
                    });
                }

                return Ok(SendResult {
                    pending_id: pending_id.to_string(),
                    event_id: Some(event_id.to_string()),
                    chat_id: receiver_npub.to_string(),
                });
            }
            Ok(_) | Err(_) => {
                // No relay accepted or error
                if attempt + 1 >= config.max_send_attempts {
                    // All attempts exhausted — mark failed
                    let failed_msg = {
                        let mut state = STATE.lock().await;
                        state.update_message(pending_id, |msg| {
                            msg.set_failed(true);
                            msg.set_pending(false);
                        })
                    };

                    if let Some((_chat_id, ref msg)) = failed_msg {
                        callback.on_failed(receiver_npub, pending_id, msg);
                        callback.on_persist(receiver_npub, msg);
                    }

                    return Err(format!("Failed to send DM after {} attempts", config.max_send_attempts));
                }

                // Retry after delay
                tokio::time::sleep(config.retry_delay).await;
            }
        }
    }

    Err("Send loop exited unexpectedly".to_string())
}

// ============================================================================
// send_dm — Text DMs
// ============================================================================

/// Send a NIP-17 gift-wrapped text DM.
///
/// Flow: pending msg → callback.on_pending → build Kind 14 rumor →
/// gift-wrap with retry → finalize → callback.on_sent → optional self-send.
pub async fn send_dm(
    receiver_npub: &str,
    content: &str,
    reply_to: Option<&str>,
    config: &SendConfig,
    callback: &dyn SendCallback,
) -> Result<SendResult, String> {
    let client = NOSTR_CLIENT.get().ok_or("Not logged in")?;
    let my_pk = *MY_PUBLIC_KEY.get().ok_or("Public key not set")?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap();
    let pending_id = format!("pending-{}", now.as_nanos());

    let receiver = PublicKey::from_bech32(receiver_npub)
        .map_err(|e| format!("Invalid npub: {}", e))?;

    // Build pending message and add to state
    let msg = Message {
        id: pending_id.clone(),
        content: content.to_string(),
        replied_to: reply_to.unwrap_or("").to_string(),
        at: now.as_millis() as u64,
        pending: true,
        mine: true,
        npub: my_pk.to_bech32().ok(),
        ..Default::default()
    };

    {
        let mut state = STATE.lock().await;
        state.add_message_to_participant(receiver_npub, msg.clone());
    }

    callback.on_pending(receiver_npub, &msg);

    // Build the rumor
    let milliseconds = now.as_millis() % 1000;
    let mut rumor = EventBuilder::private_msg_rumor(receiver, content);

    if let Some(reply_id) = reply_to {
        if !reply_id.is_empty() {
            rumor = rumor.tag(Tag::custom(
                TagKind::e(),
                [reply_id.to_string(), String::new(), "reply".to_string()],
            ));
        }
    }

    let rumor = rumor.tag(Tag::custom(TagKind::custom("ms"), [milliseconds.to_string()]));
    let built_rumor = rumor.build(my_pk);
    let event_id = built_rumor.id.ok_or("Rumor has no id")?.to_hex();

    // Send via gift-wrap with retry
    retry_send_gift_wrap(
        client, &receiver, receiver_npub, &pending_id,
        built_rumor, &event_id, config, callback,
    ).await
}

// ============================================================================
// send_rumor_dm — Pre-built rumor (for dedup / custom events)
// ============================================================================

/// Send a pre-built rumor via NIP-17 gift-wrap.
///
/// Used when the caller has already built the rumor (e.g., after file
/// deduplication found an existing upload URL). Skips encryption/upload.
pub async fn send_rumor_dm(
    receiver_npub: &str,
    pending_id: &str,
    rumor: UnsignedEvent,
    config: &SendConfig,
    callback: &dyn SendCallback,
) -> Result<SendResult, String> {
    let client = NOSTR_CLIENT.get().ok_or("Not logged in")?;

    let receiver = PublicKey::from_bech32(receiver_npub)
        .map_err(|e| format!("Invalid npub: {}", e))?;

    let event_id = rumor.id.ok_or("Rumor has no id")?.to_hex();

    retry_send_gift_wrap(
        client, &receiver, receiver_npub, pending_id,
        rumor, &event_id, config, callback,
    ).await
}

// ============================================================================
// send_file_dm — File Attachment DMs
// ============================================================================

/// Send a NIP-17 gift-wrapped file attachment DM.
///
/// Flow: pending msg → callback.on_pending → encrypt → preview attachment →
/// callback.on_attachment_preview → upload with progress → callback.on_upload_complete →
/// build Kind 15 → gift-wrap with retry → finalize → callback.on_sent.
pub async fn send_file_dm(
    receiver_npub: &str,
    file_bytes: Arc<Vec<u8>>,
    filename: &str,
    extension: &str,
    content: Option<&str>,
    config: &SendConfig,
    callback: &dyn SendCallback,
) -> Result<SendResult, String> {
    let client = NOSTR_CLIENT.get().ok_or("Not logged in")?;
    let my_pk = *MY_PUBLIC_KEY.get().ok_or("Public key not set")?;
    let keys = MY_SECRET_KEY.to_keys().ok_or("Keys not available")?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap();
    let pending_id = format!("pending-{}", now.as_nanos());

    let receiver = PublicKey::from_bech32(receiver_npub)
        .map_err(|e| format!("Invalid npub: {}", e))?;

    // Hash + encrypt
    let file_hash = crypto::sha256_hex(&file_bytes);
    let params = crypto::generate_encryption_params();
    let encrypted = crypto::encrypt_data(&file_bytes, &params)?;
    let encrypted_size = encrypted.len() as u64;

    // Build pending message with attachment preview
    let attachment = Attachment {
        id: file_hash.clone(),
        key: params.key.clone(),
        nonce: params.nonce.clone(),
        extension: extension.to_string(),
        name: filename.to_string(),
        url: String::new(),
        path: String::new(),
        size: encrypted_size,
        img_meta: None,
        downloading: false,
        downloaded: true,
        ..Default::default()
    };

    let msg = Message {
        id: pending_id.clone(),
        content: content.unwrap_or("").to_string(),
        at: now.as_millis() as u64,
        pending: true,
        mine: true,
        npub: my_pk.to_bech32().ok(),
        attachments: vec![attachment.clone()],
        ..Default::default()
    };

    {
        let mut state = STATE.lock().await;
        state.add_message_to_participant(receiver_npub, msg.clone());
    }

    callback.on_pending(receiver_npub, &msg);

    // Upload to Blossom with progress bridged through callback
    let mime_type = crypto::mime_from_extension(extension);
    let servers = crate::state::get_blossom_servers();

    let progress_cb: crate::blossom::ProgressCallback = Arc::new(|_, _| Ok(()));

    // If caller provided a cancel token, use it; otherwise create a dummy
    let cancel_token = config.cancel_token.clone();

    let upload_url = match crate::blossom::upload_blob_with_progress_and_failover(
        keys.clone(),
        servers,
        Arc::new(encrypted),
        Some(mime_type),
        progress_cb,
        Some(config.upload_retries),
        Some(config.upload_retry_delay),
        cancel_token,
    ).await {
        Ok(url) => url,
        Err(e) => {
            // Mark as failed
            let failed_msg = {
                let mut state = STATE.lock().await;
                state.update_message(&pending_id, |msg| {
                    msg.set_failed(true);
                    msg.set_pending(false);
                })
            };
            if let Some((_chat_id, ref msg)) = failed_msg {
                callback.on_failed(receiver_npub, &pending_id, msg);
                callback.on_persist(receiver_npub, msg);
            }
            return Err(format!("Upload failed: {}", e));
        }
    };

    // Update attachment URL in state
    {
        let mut state = STATE.lock().await;
        state.update_message(&pending_id, |msg| {
            if let Some(att) = msg.attachments.last_mut() {
                att.url = upload_url.clone().into_boxed_str();
            }
        });
    }

    callback.on_upload_complete(receiver_npub, &pending_id, &file_hash, &upload_url);

    // Build Kind 15 file event
    let mut file_rumor = EventBuilder::new(Kind::from_u16(15), &upload_url)
        .tag(Tag::public_key(receiver))
        .tag(Tag::custom(TagKind::custom("file-type"), [mime_type]))
        .tag(Tag::custom(TagKind::custom("size"), [encrypted_size.to_string()]))
        .tag(Tag::custom(TagKind::custom("encryption-algorithm"), ["aes-gcm"]))
        .tag(Tag::custom(TagKind::custom("decryption-key"), [params.key.as_str()]))
        .tag(Tag::custom(TagKind::custom("decryption-nonce"), [params.nonce.as_str()]))
        .tag(Tag::custom(TagKind::custom("ox"), [file_hash.clone()]));

    if !filename.is_empty() {
        file_rumor = file_rumor.tag(Tag::custom(TagKind::custom("name"), [filename]));
    }

    let milliseconds = now.as_millis() % 1000;
    file_rumor = file_rumor.tag(Tag::custom(TagKind::custom("ms"), [milliseconds.to_string()]));

    let built_rumor = file_rumor.build(my_pk);
    let event_id = built_rumor.id.ok_or("Rumor has no id")?.to_hex();

    // Send via gift-wrap with retry
    retry_send_gift_wrap(
        client, &receiver, receiver_npub, &pending_id,
        built_rumor, &event_id, config, callback,
    ).await
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Debug, Clone, PartialEq)]
    enum CbEvent {
        Pending(String),
        AttachmentPreview(String),
        UploadProgress(String, u8, u64),
        UploadComplete(String, String),
        Sent(String, String),
        Failed(String, String),
        Persist(String),
    }

    struct MockCallback {
        events: Mutex<Vec<CbEvent>>,
        cancel_at: Option<u8>,
    }

    impl MockCallback {
        fn new() -> Self { Self { events: Mutex::new(vec![]), cancel_at: None } }
        fn with_cancel(pct: u8) -> Self { Self { events: Mutex::new(vec![]), cancel_at: Some(pct) } }
        fn events(&self) -> Vec<CbEvent> { self.events.lock().unwrap().clone() }
    }

    impl SendCallback for MockCallback {
        fn on_pending(&self, cid: &str, _: &Message) {
            self.events.lock().unwrap().push(CbEvent::Pending(cid.into()));
        }
        fn on_attachment_preview(&self, cid: &str, _: &Message) {
            self.events.lock().unwrap().push(CbEvent::AttachmentPreview(cid.into()));
        }
        fn on_upload_progress(&self, pid: &str, pct: u8, bytes: u64) -> Result<(), String> {
            self.events.lock().unwrap().push(CbEvent::UploadProgress(pid.into(), pct, bytes));
            if self.cancel_at.map_or(false, |c| pct >= c) { return Err("Cancelled".into()); }
            Ok(())
        }
        fn on_upload_complete(&self, cid: &str, _: &str, _: &str, url: &str) {
            self.events.lock().unwrap().push(CbEvent::UploadComplete(cid.into(), url.into()));
        }
        fn on_sent(&self, cid: &str, old: &str, _: &Message) {
            self.events.lock().unwrap().push(CbEvent::Sent(cid.into(), old.into()));
        }
        fn on_failed(&self, cid: &str, old: &str, _: &Message) {
            self.events.lock().unwrap().push(CbEvent::Failed(cid.into(), old.into()));
        }
        fn on_persist(&self, cid: &str, _: &Message) {
            self.events.lock().unwrap().push(CbEvent::Persist(cid.into()));
        }
    }

    #[test]
    fn config_default() {
        let c = SendConfig::default();
        assert_eq!(c.max_send_attempts, 1);
        assert!(!c.self_send);
        assert!(c.cancel_token.is_none());
        assert_eq!(c.upload_retries, 3);
    }

    #[test]
    fn config_gui() {
        let c = SendConfig::gui();
        assert_eq!(c.max_send_attempts, 12);
        assert!(c.self_send);
    }

    #[test]
    fn config_headless() {
        let c = SendConfig::headless();
        assert_eq!(c.max_send_attempts, 3);
        assert!(c.self_send);
    }

    #[test]
    fn config_custom_override() {
        let c = SendConfig { max_send_attempts: 5, ..SendConfig::gui() };
        assert_eq!(c.max_send_attempts, 5);
        assert!(c.self_send);
    }

    #[test]
    fn noop_callback_all_methods() {
        let cb = NoOpSendCallback;
        let msg = Message::default();
        cb.on_pending("c", &msg);
        cb.on_attachment_preview("c", &msg);
        assert!(cb.on_upload_progress("p", 50, 1024).is_ok());
        cb.on_upload_complete("c", "p", "a", "url");
        cb.on_sent("c", "o", &msg);
        cb.on_failed("c", "o", &msg);
        cb.on_persist("c", &msg);
    }

    #[test]
    fn text_dm_sequence() {
        let cb = MockCallback::new();
        let msg = Message::default();
        cb.on_pending("r", &msg);
        cb.on_sent("r", "p-1", &msg);
        cb.on_persist("r", &msg);
        assert_eq!(cb.events(), vec![
            CbEvent::Pending("r".into()),
            CbEvent::Sent("r".into(), "p-1".into()),
            CbEvent::Persist("r".into()),
        ]);
    }

    #[test]
    fn file_dm_sequence() {
        let cb = MockCallback::new();
        let msg = Message::default();
        cb.on_pending("r", &msg);
        cb.on_attachment_preview("r", &msg);
        cb.on_upload_progress("p", 0, 0).ok();
        cb.on_upload_progress("p", 50, 5000).ok();
        cb.on_upload_progress("p", 100, 10000).ok();
        cb.on_upload_complete("r", "p", "h", "https://blossom/h");
        cb.on_sent("r", "p", &msg);
        cb.on_persist("r", &msg);
        let e = cb.events();
        assert_eq!(e.len(), 8);
        assert!(matches!(&e[5], CbEvent::UploadComplete(_, url) if url.contains("blossom")));
    }

    #[test]
    fn failed_sequence() {
        let cb = MockCallback::new();
        let msg = Message::default();
        cb.on_pending("r", &msg);
        cb.on_failed("r", "p", &msg);
        cb.on_persist("r", &msg);
        assert_eq!(cb.events(), vec![
            CbEvent::Pending("r".into()),
            CbEvent::Failed("r".into(), "p".into()),
            CbEvent::Persist("r".into()),
        ]);
    }

    #[test]
    fn cancel_upload_at_threshold() {
        let cb = MockCallback::with_cancel(50);
        assert!(cb.on_upload_progress("p", 25, 512).is_ok());
        assert!(cb.on_upload_progress("p", 50, 1024).is_err());
        assert_eq!(cb.events().len(), 2);
    }

    #[test]
    fn cancel_triggers_failed() {
        let cb = MockCallback::with_cancel(30);
        let msg = Message::default();
        cb.on_pending("r", &msg);
        cb.on_attachment_preview("r", &msg);
        cb.on_upload_progress("p", 10, 1000).ok();
        assert!(cb.on_upload_progress("p", 30, 3000).is_err());
        cb.on_failed("r", "p", &msg);
        assert!(matches!(cb.events().last(), Some(CbEvent::Failed(..))));
    }

    #[test]
    fn send_result_serialize() {
        let r = SendResult { pending_id: "p".into(), event_id: Some("e".into()), chat_id: "c".into() };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"pending_id\":\"p\""));
    }

    #[test]
    fn send_result_none_event() {
        let r = SendResult { pending_id: "p".into(), event_id: None, chat_id: "c".into() };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("null"));
    }
}
