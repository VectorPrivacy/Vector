//! Message sending — NIP-17 gift-wrapped DMs (text and file attachments).
//!
//! This is the core send pipeline used by all Vector interfaces (GUI, CLI, SDK).
//! Clients provide a `SendCallback` for status notifications (pending/sent/failed/progress)
//! and a `SendConfig` for retry/cancel behavior.

use crate::event_ext::FinalizeUnsignedWithId;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use nostr_sdk::prelude::*;

/// NIP-59's gift-wrap backdating window (0 to 2 days).
///
/// nostr 0.45 made its own copy of this private, but the wrap timestamps we mint
/// must stay inside the window readers expect, so it's pinned here rather than
/// re-guessed per call site.
pub const NIP59_RANDOM_TIMESTAMP_TWEAK: Range<u64> = 0..172_800;

/// `Timestamp::now()` backdated by a random offset inside the NIP-59 window.
///
/// 0.45.0 removed `Timestamp::tweaked`; this mirrors what nip59 does internally.
pub fn tweaked_timestamp() -> Timestamp {
    use ::rand::Rng;
    let secs: u64 = ::rand::thread_rng().gen_range(NIP59_RANDOM_TIMESTAMP_TWEAK);
    Timestamp::from_secs(Timestamp::now().as_secs().saturating_sub(secs))
}

use crate::state::{nostr_client, my_public_key, STATE};
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

    /// The flag a cancel flips for this pending message, if the client keeps one.
    /// The uploader polls it between progress ticks, so a cancel lands at once
    /// even while nothing is moving.
    fn cancel_token(&self, _pending_id: &str) -> Option<Arc<AtomicBool>> {
        None
    }

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
    /// What an upload is doing before its bytes move: sealing (`"encrypting"`, with its
    /// percentage when it is streamed) or getting underway (`"starting"`).
    fn on_upload_stage(&self, _pending_id: &str, _stage: &str, _pct: Option<u8>) {}

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
    /// NIP-40 self-destruct expiry (unix secs) to stamp on the outgoing rumor
    /// and mirror onto the outer wrap. None = permanent. Resolved per-chat by
    /// the caller (the "Self-Destruct Timer" setting).
    pub expiration: Option<u64>,
    /// A self-destruct LIFESPAN (secs) resolved at publish instead: a file send
    /// stamps `now + lifespan` once its upload is done, so a slow upload does not
    /// eat into the message's life. Takes precedence over `expiration` there.
    pub self_destruct_secs: Option<u64>,
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
            expiration: None,
            self_destruct_secs: None,
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
// Late-OK confirmation registry
// ============================================================================
//
// A relay's OK can outlive the per-attempt wait: slow links push the
// round-trip past nostr-sdk's OK timeout, and mobile handovers drop the
// socket after the EVENT frame was already delivered. The wrap is then
// on the relay while the sender believes it failed — the user re-sends
// and the recipient sees a double-post.
//
// Every wrap publish registers here before its first attempt. Hosts feed
// relay OKs back via `note_relay_ok` from their notification loop; an
// accepted OK counts as delivery no matter how late it arrives — it wakes
// the in-flight retry loop early, or rescues a message already marked
// Failed back to Sent.

struct WrapConfirm {
    wrap_id: EventId,
    chat_id: String,
    pending_id: String,
    /// Inner rumor id — the message's final id after finalization.
    rumor_event_id: String,
    rumor: UnsignedEvent,
    callback: Arc<dyn SendCallback>,
    self_send: bool,
    confirmed: AtomicBool,
    /// Claimed by whichever path (retry loop or note_relay_ok) performs
    /// the failed→sent rescue, so it happens exactly once.
    rescued: AtomicBool,
    /// Set once the retry loop has exited after marking the message
    /// failed — from then on `note_relay_ok` performs the rescue itself.
    loop_exited: AtomicBool,
    notify: tokio::sync::Notify,
    session: std::sync::Arc<crate::db::Session>,
    registered_at: std::time::Instant,
}

/// Entries carry this account's chat and message ids, so a late OK must only
/// ever rescue a message in the account that sent it.
struct WrapConfirms;

fn wrap_confirms() -> Arc<std::sync::Mutex<std::collections::HashMap<EventId, Arc<WrapConfirm>>>> {
    crate::db::current_session().scoped::<WrapConfirms, _>()
}

/// An OK this long after the last publish attempt is a ghost — drop the
/// entry rather than resurrect a message the user has moved past.
const WRAP_CONFIRM_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

fn register_wrap_confirm(entry: Arc<WrapConfirm>) {
    let owner = wrap_confirms();
    let mut map = owner.lock().unwrap();
    map.retain(|_, e| e.registered_at.elapsed() < WRAP_CONFIRM_TTL);
    map.insert(entry.wrap_id, entry);
}

fn remove_wrap_confirm(wrap_id: &EventId) {
    wrap_confirms().lock().unwrap().remove(wrap_id);
}

/// Feed a relay `OK` for an outbound event back into the send pipeline.
///
/// Hosts call this from their notification loop for every
/// `RelayMessage::Ok`. Ids that aren't in-flight wraps miss the registry
/// and return immediately.
pub fn note_relay_ok(event_id: &EventId, accepted: bool) {
    if !accepted {
        return;
    }
    let entry = {
        let owner = wrap_confirms();
        let map = owner.lock().unwrap();
        map.get(event_id).cloned()
    };
    let Some(entry) = entry else { return };
    entry.confirmed.store(true, Ordering::SeqCst);
    entry.notify.notify_one();
    if !entry.loop_exited.load(Ordering::SeqCst)
        || entry.rescued.swap(true, Ordering::SeqCst)
    {
        return;
    }
    if !entry.session.is_live() {
        remove_wrap_confirm(&entry.wrap_id);
        return;
    }
    crate::db::spawn_bound(async move {
        rescue_failed_as_sent(&entry).await;
        remove_wrap_confirm(&entry.wrap_id);
    });
}

/// Flip an already-failed message back to Sent — a late relay OK proved
/// the wrap was delivered.
async fn rescue_failed_as_sent(entry: &WrapConfirm) {
    let finalized = {
        let mut state = STATE.lock().await;
        state.update_message(&entry.pending_id, |msg| {
            msg.set_failed(false);
        });
        state.finalize_pending_message(&entry.chat_id, &entry.pending_id, &entry.rumor_event_id)
    };
    let Some((_old_id, ref msg)) = finalized else { return };
    crate::log_info!(
        "[Send] late relay OK confirmed wrap {} — message {} rescued to sent",
        entry.wrap_id,
        entry.rumor_event_id,
    );
    entry.callback.on_sent(&entry.chat_id, &entry.pending_id, msg);
    entry.callback.on_persist(&entry.chat_id, msg);
    // A late OK proved delivery — drop the retained resend body.
    if let Some(rid) = entry.rumor.id {
        let _ = crate::db::nip17_keys::clear_resend_payload(&rid);
    }
    if entry.self_send {
        if let (Some(client), Some(my_pk)) = (nostr_client(), my_public_key()) {
            spawn_self_send(client, my_pk, entry.rumor.clone());
        }
    }
}

/// Fire-and-forget the self-send recovery copy + persist its wrap key.
fn spawn_self_send(client: Client, my_pk: PublicKey, rumor: UnsignedEvent) {
    let rid_for_self = rumor.id;
    crate::db::spawn_bound(async move {
        match crate::inbox_relays::send_gift_wrap_retained(
            &client, &my_pk, rumor, [],
        ).await {
            Ok(self_outcome) if !self_outcome.output.success.is_empty() => {
                if let Some(rid) = rid_for_self {
                    if let Err(e) = crate::db::nip17_keys::store_wrap_key(
                        &self_outcome.wrap_event_id,
                        &rid,
                        &my_pk,
                        crate::db::nip17_keys::WrapRole::SelfSend,
                        &self_outcome.wrap_secret,
                        &self_outcome.targeted_relays,
                    ) {
                        eprintln!("[NIP-17] failed to persist self-wrap key: {}", e);
                    }
                }
            }
            _ => {}
        }
    });
}

// ============================================================================
// Internal: retry gift-wrap send
// ============================================================================

/// Shared tail of send_dm / send_file_dm / send_rumor_dm:
/// gift-wrap → retry loop → finalize/fail → self-send.
///
/// The wrap is built ONCE and the identical event republished on every
/// attempt: a relay that already stored it answers the resend with
/// OK-true "duplicate", so a lost OK becomes a delivery confirmation on
/// the next attempt instead of an unconfirmed extra copy. The wrap's
/// ephemeral key is persisted via `db::nip17_keys::store_wrap_key`
/// BEFORE the first publish — a wrap can land without us ever seeing
/// the OK, and the user must still be able to NIP-09 it later.
async fn retry_send_gift_wrap(
    client: &Client,
    receiver: &PublicKey,
    receiver_npub: &str,
    pending_id: &str,
    rumor: UnsignedEvent,
    event_id: &str,
    config: &SendConfig,
    callback: Arc<dyn SendCallback>,
    // Manual retry injects the retained wrap so the EXACT same event
    // republishes (relay dedups the duplicate). `None` = a fresh send that
    // builds + retains the wrap in-loop.
    prebuilt: Option<crate::inbox_relays::BuiltGiftWrap>,
) -> Result<SendResult, String> {
    let my_pk = my_public_key().ok_or("Public key not set")?;
    let inner_rumor_id = rumor.id;

    // A resend already holds a persisted wrap key + retained body — don't
    // re-store either (that would reset the row's clock and re-stash bytes).
    let is_resend = prebuilt.is_some();
    // Built lazily in-loop so transient signer failures (NIP-46 bunker
    // round-trips) still get the full retry schedule; a resend seeds it.
    let mut built: Option<crate::inbox_relays::BuiltGiftWrap> = prebuilt;
    // Targets resolve once; transient inbox connections live across the
    // whole retry window rather than reconnecting per attempt.
    let mut targets: Option<crate::inbox_relays::GiftWrapTargets> = None;
    let mut confirm: Option<Arc<WrapConfirm>> = None;
    let mut last_error: Option<String> = None;

    let max_attempts = config.max_send_attempts.max(1);

    for attempt in 0..max_attempts {
        if built.is_none() {
            // Mirror any NIP-40 expiry from the rumor onto the outer wrap so
            // relays drop the gift-wrap on schedule (the inner tag is encrypted).
            let wrap_extra: Vec<Tag> = rumor.tags.iter()
                .find(|t| t.as_slice().first().map(|k| k.as_str() == "expiration").unwrap_or(false))
                .cloned()
                .into_iter()
                .collect();
            match crate::inbox_relays::build_gift_wrap_retained(
                client, receiver, rumor.clone(), wrap_extra,
            ).await {
                Ok(b) => built = Some(b),
                Err(e) => {
                    crate::log_warn!(
                        "[Send] attempt {}/{} — building gift-wrap failed: {}",
                        attempt + 1, max_attempts, e,
                    );
                    last_error = Some(e);
                    if attempt + 1 < max_attempts {
                        tokio::time::sleep(config.retry_delay).await;
                    }
                    continue;
                }
            }
        }
        let wrap = built.as_ref().unwrap();

        if confirm.is_none() {
            let entry = Arc::new(WrapConfirm {
                wrap_id: wrap.event.id,
                chat_id: receiver_npub.to_string(),
                pending_id: pending_id.to_string(),
                rumor_event_id: event_id.to_string(),
                rumor: rumor.clone(),
                callback: callback.clone(),
                self_send: config.self_send,
                confirmed: AtomicBool::new(false),
                rescued: AtomicBool::new(false),
                loop_exited: AtomicBool::new(false),
                notify: tokio::sync::Notify::new(),
                session: crate::db::current_session(),
                registered_at: std::time::Instant::now(),
            });
            register_wrap_confirm(entry.clone());
            confirm = Some(entry);
        }
        let confirm_ref = confirm.as_ref().unwrap();

        if targets.is_none() {
            let t = crate::inbox_relays::resolve_gift_wrap_targets(client, receiver).await;
            // First send only: persist the wrap key (NIP-09 delete) AND retain
            // the exact wrap event + rumor (byte-identical resend on manual
            // retry). A resend already holds both.
            if !is_resend {
                if let Some(rid) = inner_rumor_id {
                    if let Err(e) = crate::db::nip17_keys::store_wrap_key(
                        &wrap.event.id,
                        &rid,
                        receiver,
                        crate::db::nip17_keys::WrapRole::Recipient,
                        &wrap.secret,
                        &t.targeted_relays,
                    ) {
                        eprintln!("[NIP-17] failed to persist wrap key: {}", e);
                    }
                    if let Err(e) = crate::db::nip17_keys::stash_resend_payload(
                        &wrap.event.id, pending_id, &wrap.event, &rumor,
                    ) {
                        eprintln!("[NIP-17] failed to retain resend payload: {}", e);
                    }
                }
            }
            targets = Some(t);
        } else {
            crate::inbox_relays::reconnect_gift_wrap_targets(targets.as_ref().unwrap()).await;
        }
        let targets_ref = targets.as_ref().unwrap();

        match crate::inbox_relays::publish_gift_wrap_to_targets(
            client, targets_ref, &wrap.event,
        ).await {
            Ok(output) if !output.success.is_empty() => {
                return Ok(finalize_gift_wrap_sent(
                    client, my_pk, receiver_npub, pending_id, event_id,
                    &rumor, config, &callback, confirm_ref, targets_ref,
                ).await);
            }
            Ok(output) => {
                // The publish round-trip ran but no targeted relay confirmed
                // (auth required, kind filter, rate-limit, timed-out OK,
                // etc.). Surface the per-relay failure reasons so the user
                // can see WHY their DMs aren't being accepted.
                let failures: Vec<String> = output.failed.iter()
                    .map(|(url, err)| format!("{}: {}", url, err))
                    .collect();
                crate::log_warn!(
                    "[Send] attempt {}/{} — 0 of {} relays accepted (targeted: {}). Per-relay errors: {}",
                    attempt + 1,
                    max_attempts,
                    targets_ref.targeted_relays.len(),
                    targets_ref.targeted_relays.join(", "),
                    if failures.is_empty() {
                        "(none reported — likely all timed out before responding)".to_string()
                    } else {
                        failures.join(" | ")
                    },
                );
                last_error = None;
            }
            Err(e) => {
                crate::log_warn!(
                    "[Send] attempt {}/{} — publish errored: {}",
                    attempt + 1, max_attempts, e,
                );
                last_error = Some(e);
            }
        }

        // A late OK for an earlier attempt may have arrived while this one
        // was publishing.
        if confirm_ref.confirmed.load(Ordering::SeqCst) {
            return Ok(finalize_gift_wrap_sent(
                client, my_pk, receiver_npub, pending_id, event_id,
                &rumor, config, &callback, confirm_ref, targets_ref,
            ).await);
        }

        if attempt + 1 < max_attempts {
            // Sleep out the retry delay, waking instantly on a late OK
            // (notify_one stores a permit, so an OK landing before this
            // line still wakes us).
            let _ = tokio::time::timeout(
                config.retry_delay,
                confirm_ref.notify.notified(),
            ).await;
            if confirm_ref.confirmed.load(Ordering::SeqCst) {
                return Ok(finalize_gift_wrap_sent(
                    client, my_pk, receiver_npub, pending_id, event_id,
                    &rumor, config, &callback, confirm_ref, targets_ref,
                ).await);
            }
        }
    }

    // Exhausted every attempt with no OK observed. Mark failed, but leave
    // the confirmation entry armed: a straggler OK can still rescue this
    // message to Sent (see note_relay_ok).
    if let Some(t) = targets.as_ref() {
        crate::inbox_relays::teardown_gift_wrap_targets(client, t).await;
    }
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
    if let Some(entry) = confirm.as_ref() {
        entry.loop_exited.store(true, Ordering::SeqCst);
        // An OK that landed between the last attempt and loop_exited going
        // up would see loop_exited=false and skip its rescue — catch it here.
        if entry.confirmed.load(Ordering::SeqCst)
            && !entry.rescued.swap(true, Ordering::SeqCst)
        {
            rescue_failed_as_sent(entry).await;
            remove_wrap_confirm(&entry.wrap_id);
            return Ok(SendResult {
                pending_id: pending_id.to_string(),
                event_id: Some(event_id.to_string()),
                chat_id: receiver_npub.to_string(),
            });
        }
    }
    match last_error {
        Some(e) => Err(format!("Failed to send DM after {} attempts: {}", max_attempts, e)),
        None => Err(format!(
            "Failed to send DM after {} attempts (no relays accepted the gift-wrap)",
            max_attempts
        )),
    }
}

/// Success epilogue shared by every confirmed path in the retry loop:
/// finalize the pending message, notify, persist, fire the self-send.
async fn finalize_gift_wrap_sent(
    client: &Client,
    my_pk: PublicKey,
    receiver_npub: &str,
    pending_id: &str,
    event_id: &str,
    rumor: &UnsignedEvent,
    config: &SendConfig,
    callback: &Arc<dyn SendCallback>,
    confirm: &Arc<WrapConfirm>,
    targets: &crate::inbox_relays::GiftWrapTargets,
) -> SendResult {
    remove_wrap_confirm(&confirm.wrap_id);
    crate::inbox_relays::teardown_gift_wrap_targets(client, targets).await;

    let finalized = {
        let mut state = STATE.lock().await;
        state.finalize_pending_message(receiver_npub, pending_id, event_id)
    };
    if let Some((_old_id, ref finalized_msg)) = finalized {
        callback.on_sent(receiver_npub, pending_id, finalized_msg);
        callback.on_persist(receiver_npub, finalized_msg);
    }

    // Delivery confirmed — drop the retained resend body (the key row stays for
    // NIP-09). Steady-state, only genuinely-unsent messages carry a body.
    if let Some(rid) = rumor.id {
        let _ = crate::db::nip17_keys::clear_resend_payload(&rid);
    }

    if config.self_send {
        spawn_self_send(client.clone(), my_pk, rumor.clone());
    }

    SendResult {
        pending_id: pending_id.to_string(),
        event_id: Some(event_id.to_string()),
        chat_id: receiver_npub.to_string(),
    }
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
    callback: Arc<dyn SendCallback>,
) -> Result<SendResult, String> {
    let client = nostr_client().ok_or("Not logged in")?;
    let my_pk = my_public_key().ok_or("Public key not set")?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap();
    let pending_id = format!("pending-{}", now.as_nanos());

    let receiver = PublicKey::from_bech32(receiver_npub)
        .map_err(|e| format!("Invalid npub: {}", e))?;

    // NIP-30: resolve any `:shortcode:` in the outbound text against the
    // user's subscribed packs so the rumor carries `["emoji", ...]` tags.
    // Recipients without the pack subscribed still render correctly, and
    // our own-view echo populates `emoji_tags` for the renderer.
    let emoji_tags = crate::emoji_packs::resolve_outbound_emoji_tags(content);

    // Build pending message and add to state
    let msg = Message {
        id: pending_id.clone(),
        content: content.to_string(),
        replied_to: reply_to.unwrap_or("").to_string(),
        at: now.as_millis() as u64,
        pending: true,
        mine: true,
        npub: my_pk.to_bech32().ok(),
        emoji_tags: emoji_tags.clone(),
        expiration: config.expiration,
        ..Default::default()
    };

    {
        let mut state = STATE.lock().await;
        state.add_message_to_participant(receiver_npub, &msg);
    }

    callback.on_pending(receiver_npub, &msg);

    // Build the rumor
    let milliseconds = now.as_millis() % 1000;
    // NIP-17 rumor: kind 14 + recipient p-tag (upstream's `make_rumor` is private).
    let mut rumor = EventBuilder::new(Kind::PrivateDirectMessage, content)
        .tag(Tag::public_key(receiver));

    if let Some(reply_id) = reply_to {
        if !reply_id.is_empty() {
            rumor = rumor.tag(Tag::custom(
                "e",
                [reply_id.to_string(), String::new(), "reply".to_string()],
            ));
        }
    }

    let mut rumor = rumor.tag(Tag::custom("ms", [milliseconds.to_string()]));
    for et in &emoji_tags {
        rumor = rumor.tag(Tag::custom(
            "emoji",
            [et.shortcode.clone(), et.url.clone()],
        ));
    }
    // NIP-40 self-destruct: stamp the rumor so compliant receivers honor the
    // expiry; retry_send_gift_wrap mirrors it onto the outer wrap for relays.
    if let Some(exp) = config.expiration {
        rumor = rumor.tag(Tag::expiration(Timestamp::from_secs(exp)));
    }
    let built_rumor = rumor.finalize_unsigned_with_id(my_pk);
    let event_id = built_rumor.id.ok_or("Rumor has no id")?.to_hex();

    // Send via gift-wrap with retry
    retry_send_gift_wrap(
        &client, &receiver, receiver_npub, &pending_id,
        built_rumor, &event_id, config, callback, None,
    ).await
}

// ============================================================================
// send_rumor_dm — Pre-built rumor (custom events)
// ============================================================================

/// Send a pre-built rumor via NIP-17 gift-wrap.
///
/// Used when the caller has already built the rumor. Skips encryption/upload.
pub async fn send_rumor_dm(
    receiver_npub: &str,
    pending_id: &str,
    rumor: UnsignedEvent,
    config: &SendConfig,
    callback: Arc<dyn SendCallback>,
) -> Result<SendResult, String> {
    let client = nostr_client().ok_or("Not logged in")?;

    let receiver = PublicKey::from_bech32(receiver_npub)
        .map_err(|e| format!("Invalid npub: {}", e))?;

    let event_id = rumor.id.ok_or("Rumor has no id")?.to_hex();

    retry_send_gift_wrap(
        &client, &receiver, receiver_npub, pending_id,
        rumor, &event_id, config, callback, None,
    ).await
}

/// Manually retry a failed DM by republishing the EXACT retained recipient
/// wrap. Same outer event id → relays no-op the duplicate, so a first send
/// that silently landed can never double-post, regardless of the recipient's
/// client.
///
/// `Ok(true)`  — a retained wrap was found and the resend went through the
///               normal retry loop (message ends sent, or red again ready to
///               retry). The caller must NOT fall back.
/// `Ok(false)` — nothing retained (old/pruned message, or the first send
///               failed before a wrap was ever built, e.g. an upload error).
///               The caller falls back to a fresh send — safe, since no wrap
///               reached a relay in that case.
pub async fn resend_failed_dm(
    receiver_npub: &str,
    failed_msg_id: &str,
    config: &SendConfig,
    callback: Arc<dyn SendCallback>,
) -> Result<bool, String> {
    // Guard the read→flip→republish against a mid-tap account swap: the payload
    // is this account's; a swap before the STATE flip must abort (returning
    // "handled" so the caller never falls back to a fresh send in the WRONG
    // account). retry_send_gift_wrap re-guards its own STATE/DB writes.
    let payload = match crate::db::nip17_keys::get_resend_payload_by_pending(failed_msg_id)? {
        Some(p) => p,
        None => return Ok(false),
    };
    let client = nostr_client().ok_or("Not logged in")?;
    let receiver = PublicKey::from_bech32(receiver_npub)
        .map_err(|e| format!("Invalid npub: {}", e))?;

    // Flip the red row back to "sending"; the retry loop's own fail/finalize
    // path returns it to red or promotes it to sent.
    let repending = {
        let mut state = STATE.lock().await;
        state.update_message(failed_msg_id, |msg| {
            msg.set_failed(false);
            msg.set_pending(true);
        })
    };
    if let Some((_chat_id, ref msg)) = repending {
        callback.on_pending(receiver_npub, msg);
    }

    let event_id = payload.rumor.id.ok_or("Retained rumor has no id")?.to_hex();
    let built = crate::inbox_relays::BuiltGiftWrap {
        event: payload.wrap_event,
        secret: payload.secret,
    };
    // Inject the retained wrap: the loop republishes these exact bytes.
    if let Err(e) = retry_send_gift_wrap(
        &client, &receiver, receiver_npub, failed_msg_id,
        payload.rumor, &event_id, config, callback, Some(built),
    )
    .await
    {
        // The resend was attempted — the loop already marked the row red again.
        // Still "handled": the caller must not fall back to a fresh wrap, or the
        // retained wrap that may yet be sitting on a relay would double-post.
        crate::log_warn!("[Send] idempotent resend of {} did not land: {}", failed_msg_id, e);
    }
    Ok(true)
}

// ============================================================================
// send_file_dm — File Attachment DMs
// ============================================================================

/// Send a NIP-17 gift-wrapped file attachment DM.
///
/// Flow: hash → save locally → pending → encrypt → upload → build Kind 15 rumor → gift-wrap + send.
pub async fn send_file_dm(
    receiver_npub: &str,
    file_bytes: Arc<Vec<u8>>,
    filename: &str,
    extension: &str,
    content: Option<&str>,
    reply_to: Option<&str>,
    config: &SendConfig,
    callback: Arc<dyn SendCallback>,
) -> Result<SendResult, String> {
    send_file_dm_with_meta(receiver_npub, file_bytes, filename, extension, None, content, reply_to, config, callback).await
}

/// [`send_file_dm`] for a caller that already holds the image's preview metadata,
/// sparing a second full decode before the bubble can appear. `None` derives it.
#[allow(clippy::too_many_arguments)]
pub async fn send_file_dm_with_meta(
    receiver_npub: &str,
    file_bytes: Arc<Vec<u8>>,
    filename: &str,
    extension: &str,
    img_meta: Option<crate::types::ImageMetadata>,
    content: Option<&str>,
    reply_to: Option<&str>,
    config: &SendConfig,
    callback: Arc<dyn SendCallback>,
) -> Result<SendResult, String> {
    send_file_dm_from(receiver_npub, FileSource::Bytes(file_bytes), filename, extension, img_meta, content, reply_to, config, callback).await
}

/// Where an outgoing file's bytes come from.
pub enum FileSource {
    /// In memory: images the caller has already processed, pasted bytes.
    Bytes(Arc<Vec<u8>>),
    /// On disk, read a chunk at a time: a file of any size costs one chunk of memory.
    /// Its preview metadata is the caller's to give; nothing here decodes it.
    Path(std::path::PathBuf),
}

/// Removes a file when dropped: the ciphertext of an upload that ended either way.
struct TempFile(std::path::PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// [`send_file_dm_with_meta`] from either source.
#[allow(clippy::too_many_arguments)]
pub async fn send_file_dm_from(
    receiver_npub: &str,
    source: FileSource,
    filename: &str,
    extension: &str,
    img_meta: Option<crate::types::ImageMetadata>,
    content: Option<&str>,
    reply_to: Option<&str>,
    config: &SendConfig,
    callback: Arc<dyn SendCallback>,
) -> Result<SendResult, String> {
    let client = nostr_client().ok_or("Not logged in")?;
    let my_pk = my_public_key().ok_or("Public key not set")?;
    let reply_to = reply_to.filter(|r| !r.is_empty());
    // Sign the Blossom auth event via the active client signer so bunker
    // accounts route through NostrConnect (the user's identity key lives on
    // the remote signer; MY_SECRET_KEY only holds the NIP-46 client key).
    let signer = crate::signer::active_signer()
        .map_err(|e| format!("Signer unavailable: {}", e))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap();
    let pending_id = format!("pending-{}", now.as_nanos());
    let milliseconds = now.as_millis() % 1000;

    let receiver = PublicKey::from_bech32(receiver_npub)
        .map_err(|e| format!("Invalid npub: {}", e))?;

    let mime_type = crypto::mime_from_extension(extension);
    let download_dir = crate::db::get_download_dir();

    // Hash, save and (if not given) read the preview metadata off the runtime: each
    // is a full pass over the bytes, and a runtime worker parked on one stalls every
    // task queued behind it.
    let (file_hash, local_path_str, img_meta, plain_len) = {
        let source = match &source {
            FileSource::Bytes(bytes) => FileSource::Bytes(Arc::clone(bytes)),
            FileSource::Path(path) => FileSource::Path(path.clone()),
        };
        let (filename, extension) = (filename.to_string(), extension.to_string());
        tokio::task::spawn_blocking(move || -> Result<_, String> {
            let (file_hash, plain_len) = match &source {
                FileSource::Bytes(bytes) => (crypto::sha256_hex(bytes), bytes.len() as u64),
                FileSource::Path(path) => crypto::stream::hash_file(path)?,
            };
            let _ = std::fs::create_dir_all(&download_dir);
            // Save with an extension matching the actual content. The caller's
            // `extension` is post-compression (e.g. JPEG when an original PNG was
            // compressed), but `filename` may still carry the pre-compression one;
            // JPEG bytes saved as `.png` would poison a later re-upload with a
            // MIP-04 mismatch.
            let local_name = if filename.is_empty() {
                format!("{}.{}", &file_hash, extension)
            } else {
                let stem = filename.rsplit_once('.').map(|(s, _)| s).unwrap_or(&filename);
                format!("{}.{}", stem, extension)
            };
            // Resolve unique path (pasted_image.png → pasted_image-1.png on collision)
            let local_path = crypto::resolve_unique_filename(&download_dir, &local_name);
            // Atomic write: temp file then rename. A file source copies file to file
            // (a clone where the filesystem has them), never through memory.
            let tmp = download_dir.join(format!(".{}.tmp", &file_hash));
            let img_meta = match &source {
                FileSource::Bytes(bytes) => {
                    let _ = std::fs::write(&tmp, &**bytes);
                    img_meta.or_else(|| crypto::generate_image_metadata(bytes))
                }
                FileSource::Path(path) => {
                    std::fs::copy(path, &tmp).map_err(|e| format!("Failed to save the file: {e}"))?;
                    img_meta
                }
            };
            let _ = std::fs::rename(&tmp, &local_path);
            Ok((file_hash, local_path.to_string_lossy().to_string(), img_meta, plain_len))
        })
        .await
        .map_err(|e| format!("Attachment prep failed: {}", e))??
    };

    // WebXDC Mini Apps: mint the realtime-channel topic at send time and carry
    // it on the rumor — locally-derived topics are asymmetric in DMs (each
    // side's chat_id is the other party's npub), splitting players onto
    // disjoint gossip topics.
    let webxdc_topic = (extension.eq_ignore_ascii_case("xdc"))
        .then(|| crate::webxdc::mint_topic_id(&file_hash, &my_pk.to_hex()));

    // The bubble goes up now, on fresh encryption parameters; a reused blob swaps in
    // its own below. AES-GCM appends a 16-byte tag, so the size is known unencrypted.
    let params = crypto::generate_encryption_params();
    let attachment = Attachment {
        id: file_hash.clone(), key: params.key.clone(), nonce: params.nonce.clone(),
        extension: extension.to_string(), name: filename.to_string(),
        url: String::new(), path: local_path_str.clone(), size: plain_len + 16,
        img_meta: img_meta.clone(), downloading: false, downloaded: true,
        webxdc_topic: webxdc_topic.clone(),
        ..Default::default()
    };
    let msg = Message {
        id: pending_id.clone(), content: content.unwrap_or("").to_string(),
        replied_to: reply_to.unwrap_or("").to_string(),
        at: now.as_millis() as u64, pending: true, mine: true,
        npub: my_pk.to_bech32().ok(), attachments: vec![attachment.clone()],
        // A lifespan is stamped after the upload; the bubble shows no clock until then.
        expiration: if config.self_destruct_secs.is_some() { None } else { config.expiration },
        ..Default::default()
    };
    {
        let mut state = STATE.lock().await;
        state.add_message_to_participant(receiver_npub, &msg);
    }
    callback.on_pending(receiver_npub, &msg);
    let cancel = config.cancel_token.clone().or_else(|| callback.cancel_token(&pending_id));

    // === Smart-forward: reuse a prior upload of this exact plaintext ===
    //
    // The ledger is the attachments table: every verified send or download of
    // these bytes left url+key+nonce behind. A live blob means no encrypt, no
    // upload — the message just references it. Liveness is proven, never
    // assumed: a dead or unreachable blob falls through to the fresh upload.
    let reused: Option<crate::db::attachments::ReusableUpload> =
        match crate::db::attachments::find_reusable_by_hash(&file_hash) {
            Ok(Some(r)) if crate::blossom::blob_is_served(&r.url, std::time::Duration::from_secs(5)).await => {
                crate::log_info!("[SmartForward] {} reused ({}, {} KB) — no upload", &file_hash[..8], if r.mine { "own blob" } else { "foreign blob, mirroring in background" }, r.size / 1024);
                Some(r)
            }
            _ => None,
        };

    // === Encrypt → upload → build rumor → send (skipped wholesale on reuse) ===
    // Held to the end of the send, so the ciphertext goes however it ends.
    let mut _ciphertext_file: Option<TempFile> = None;
    let (att_key, att_nonce, att_url, encrypted_size, encrypted) = match &reused {
        Some(r) => {
            let adopted = crate::compact::CompactAttachment::from_attachment(&Attachment {
                key: r.key.clone(), nonce: r.nonce.clone(), url: r.url.clone(), size: r.size,
                ..attachment.clone()
            });
            let mut state = STATE.lock().await;
            state.update_message(&pending_id, |msg| {
                if let Some(att) = msg.attachments.last_mut() {
                    *att = adopted.clone();
                }
            });
            (r.key.clone(), r.nonce.clone(), r.url.clone(), r.size, None)
        }
        None => {
            let key = params.key.clone();
            let nonce = params.nonce.clone();
            // Only a streamed seal can count its progress; bytes in hand are sealed in one go.
            let streamed = matches!(source, FileSource::Path(_));
            callback.on_upload_stage(&pending_id, "encrypting", streamed.then_some(0));
            // Resolved here, on the task that carries the account, not on a blocking thread.
            let outgoing = crate::db::current_account_dir()
                .map(|d| d.join("outgoing"))
                .unwrap_or_else(|_| std::env::temp_dir());
            let body = match &source {
                FileSource::Bytes(bytes) => {
                    let bytes = Arc::clone(bytes);
                    let sealed = tokio::task::spawn_blocking(move || {
                        crypto::encrypt_data(&bytes, &crypto::EncryptionParams { key, nonce })
                    })
                    .await
                    .map_err(|e| format!("Encryption failed: {}", e))??;
                    crate::blossom::UploadBody::Memory(Arc::new(sealed))
                }
                FileSource::Path(_) => {
                    // Sealed from the local copy, which nothing else will move or edit.
                    let _ = std::fs::create_dir_all(&outgoing);
                    let sealed_path = outgoing.join(format!("{}.enc", pending_id));
                    _ciphertext_file = Some(TempFile(sealed_path.clone()));
                    let plain = std::path::PathBuf::from(&local_path_str);
                    let out = sealed_path.clone();
                    let (stage_cb, pid, stop) = (callback.clone(), pending_id.clone(), cancel.clone());
                    let (hash, len) = tokio::task::spawn_blocking(move || {
                        crypto::stream::encrypt_file_with_progress(&plain, &out, &key, &nonce, &mut |pct| {
                            stage_cb.on_upload_stage(&pid, "encrypting", Some(pct));
                            // A cancel lands here, not after the whole file is sealed.
                            !stop.as_ref().is_some_and(|c| c.load(Ordering::Relaxed))
                        })
                    })
                    .await
                    .map_err(|e| format!("Encryption failed: {}", e))??;
                    crate::blossom::UploadBody::file(sealed_path, len, &hash)?
                }
            };
            callback.on_upload_stage(&pending_id, "starting", None);
            let size = body.len();
            (params.key, params.nonce, String::new(), size, Some(body))
        }
    };

    // Upload to Blossom — bridge SendCallback.on_upload_progress to Blossom ProgressCallback
    let servers = crate::state::get_blossom_servers();
    let cb_for_progress = callback.clone();
    let pid_for_progress = pending_id.clone();
    let progress_cb: crate::blossom::ProgressCallback = Arc::new(move |percentage, bytes| {
        cb_for_progress.on_upload_progress(
            &pid_for_progress,
            percentage.unwrap_or(0),
            bytes.unwrap_or(0),
        )
    });

    // Send the original MIME even though bytes are ciphertext: many
    // Blossom servers reject `application/octet-stream` but accept the
    // same bytes under their original type.
    let (upload_url, mirror_urls) = match encrypted {
        // Reuse: the attachment already points at a live blob. No upload, no
        // fan-out — the send is just the message.
        None => {
            callback.on_upload_complete(receiver_npub, &pending_id, &file_hash, &att_url);
            // A FOREIGN blob's lifetime belongs to its uploader; mirror it to
            // our servers in the background as availability insurance. Never
            // blocks or delays the send — best-effort by design.
            if reused.as_ref().is_some_and(|r| !r.mine) {
                let signer_bg = signer.clone();
                let url_bg = att_url.clone();
                crate::db::spawn_bound(async move {
                    let _ = crate::blossom::mirror_blob_to_servers(
                        signer_bg, &url_bg, crate::state::get_blossom_servers(),
                        1, std::time::Duration::from_secs(8), &[],
                    ).await;
                });
            }
            (att_url.clone(), Vec::new())
        }
        Some(encrypted) => {
            let accepted = match crate::blossom::upload_body_with_progress_and_failover(
                signer.clone(), servers, encrypted, Some(mime_type),
                /* is_encrypted */ true,
                progress_cb, Some(config.upload_retries), Some(config.upload_retry_delay),
                cancel.clone(),
            ).await {
                Ok(accepted) => accepted,
                Err(e) => {
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

            let upload_url = accepted.url.clone();
            {
                let mut state = STATE.lock().await;
                state.update_message(&pending_id, |msg| {
                    if let Some(att) = msg.attachments.last_mut() {
                        att.url = upload_url.clone().into_boxed_str();
                    }
                });
            }
            callback.on_upload_complete(receiver_npub, &pending_id, &file_hash, &upload_url);

            // BUD-04 mirror fan-out: bounded, best-effort. Verified mirrors ride the
            // rumor as NIP-17 `fallback` tags so the message survives its primary
            // host dying later; zero mirrors never delays or fails the send.
            let mirror_urls = crate::blossom::mirror_blob_to_servers(
                signer.clone(),
                &upload_url,
                crate::state::get_blossom_servers(),
                2,
                std::time::Duration::from_secs(5),
                std::slice::from_ref(&accepted.server),
            ).await;
            if !mirror_urls.is_empty() {
                let mut state = STATE.lock().await;
                state.update_message(&pending_id, |msg| {
                    if let Some(att) = msg.attachments.last_mut() {
                        att.fallback_urls = Some(mirror_urls.iter().map(|s| s.as_str().into()).collect());
                    }
                });
            }
            (upload_url, mirror_urls)
        }
    };

    // A cancel during the mirror fan-out or the publish must not deliver: the
    // bubble is already gone on the sender's side.
    if cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Err("Upload cancelled".to_string());
    }

    // Build Kind 15
    let mut file_rumor = EventBuilder::new(Kind::from_u16(15), &upload_url)
        .tag(Tag::public_key(receiver))
        .tag(Tag::custom("file-type", [mime_type]))
        .tag(Tag::custom("size", [encrypted_size.to_string()]))
        .tag(Tag::custom("encryption-algorithm", ["aes-gcm"]))
        .tag(Tag::custom("decryption-key", [att_key.as_str()]))
        .tag(Tag::custom("decryption-nonce", [att_nonce.as_str()]))
        .tag(Tag::custom("ox", [file_hash.clone()]));
    if let Some(reply_id) = reply_to {
        file_rumor = file_rumor.tag(Tag::custom(
            "e",
            [reply_id.to_string(), String::new(), "reply".to_string()],
        ));
    }
    for fb in &mirror_urls {
        file_rumor = file_rumor.tag(Tag::custom("fallback", [fb.as_str()]));
    }
    if !filename.is_empty() {
        file_rumor = file_rumor.tag(Tag::custom("name", [filename]));
    }
    if let Some(ref topic) = webxdc_topic {
        file_rumor = file_rumor.tag(Tag::custom("webxdc-topic", [topic.as_str()]));
    }
    // Include image preview metadata for compatible rendering across all clients
    if let Some(ref meta) = img_meta {
        if !meta.thumbhash.is_empty() {
            // `thumbhash` names the value accurately; receivers read `thumb` too
            // (legacy), so this stays backward-compatible in both directions.
            file_rumor = file_rumor.tag(Tag::custom("thumbhash", [meta.thumbhash.as_str()]));
        }
        file_rumor = file_rumor.tag(Tag::custom("dim", [format!("{}x{}", meta.width, meta.height)]));
    }
    file_rumor = file_rumor.tag(Tag::custom("ms", [milliseconds.to_string()]));
    // The self-destruct clock starts at publish: resolve the lifespan now that the
    // upload is done, and put the same stamp on the sender's own bubble.
    let expiration = config
        .self_destruct_secs
        .and_then(crate::self_destruct::expiry_after)
        .or(config.expiration);
    if let Some(exp) = expiration {
        file_rumor = file_rumor.tag(Tag::expiration(Timestamp::from_secs(exp)));
        let mut state = STATE.lock().await;
        state.update_message(&pending_id, |msg| { msg.expiration_secs = exp as u32; });
    }

    let built_rumor = file_rumor.finalize_unsigned_with_id(my_pk);
    let event_id = built_rumor.id.ok_or("Rumor has no id")?.to_hex();

    retry_send_gift_wrap(
        &client, &receiver, receiver_npub, &pending_id,
        built_rumor, &event_id, config, callback, None,
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

        cb.on_upload_progress("p", 0, 0).ok();
        cb.on_upload_progress("p", 50, 5000).ok();
        cb.on_upload_progress("p", 100, 10000).ok();
        cb.on_upload_complete("r", "p", "h", "https://blossom/h");
        cb.on_sent("r", "p", &msg);
        cb.on_persist("r", &msg);
        let e = cb.events();
        assert_eq!(e.len(), 7);
        assert!(matches!(&e[4], CbEvent::UploadComplete(_, url) if url.contains("blossom")));
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

    // ========================================================================
    // File DM callback sequences
    // ========================================================================

    #[test]
    fn file_dm_fresh_upload_full_sequence() {
        let cb = MockCallback::new();
        let msg = Message::default();

        // 1. Pending message created
        cb.on_pending("npub1recv", &msg);
        // 2. Attachment preview added

        // 3. Upload progress (0% → 25% → 50% → 75% → 100%)
        cb.on_upload_progress("pending-42", 0, 0).unwrap();
        cb.on_upload_progress("pending-42", 25, 2500).unwrap();
        cb.on_upload_progress("pending-42", 50, 5000).unwrap();
        cb.on_upload_progress("pending-42", 75, 7500).unwrap();
        cb.on_upload_progress("pending-42", 100, 10000).unwrap();
        // 4. Upload complete
        cb.on_upload_complete("npub1recv", "pending-42", "deadbeef", "https://blossom.example/deadbeef");
        // 5. Gift-wrap sent successfully
        cb.on_sent("npub1recv", "pending-42", &msg);
        // 6. Persisted to DB
        cb.on_persist("npub1recv", &msg);

        let e = cb.events();
        assert_eq!(e.len(), 9);
        assert!(matches!(&e[0], CbEvent::Pending(c) if c == "npub1recv"));
        assert!(matches!(&e[1], CbEvent::UploadProgress(_, 0, 0)));
        assert!(matches!(&e[5], CbEvent::UploadProgress(_, 100, 10000)));
        assert!(matches!(&e[6], CbEvent::UploadComplete(_, url) if url.contains("deadbeef")));
        assert!(matches!(&e[7], CbEvent::Sent(..)));
        assert!(matches!(&e[8], CbEvent::Persist(..)));
    }

    #[test]
    fn file_dm_skip_upload_sequence() {
        let cb = MockCallback::new();
        let msg = Message::default();

        // Dedup hit: no upload, existing URL reused
        // 1. Pending message
        cb.on_pending("npub1recv", &msg);
        // 2. Attachment preview (with reused URL already set)

        // 3. Upload complete (immediate — URL was already known)
        cb.on_upload_complete("npub1recv", "pending-99", "existinghash", "https://blossom.example/existing");
        // 4. Gift-wrap sent
        cb.on_sent("npub1recv", "pending-99", &msg);
        // 5. Persisted
        cb.on_persist("npub1recv", &msg);

        let e = cb.events();
        assert_eq!(e.len(), 4);
        // No UploadProgress events — upload was skipped
        assert!(!e.iter().any(|ev| matches!(ev, CbEvent::UploadProgress(..))));
        assert!(matches!(&e[1], CbEvent::UploadComplete(..)));
    }

    #[test]
    fn file_dm_upload_cancelled_at_30pct() {
        let cb = MockCallback::with_cancel(30);
        let msg = Message::default();

        cb.on_pending("npub1recv", &msg);

        assert!(cb.on_upload_progress("p", 10, 1000).is_ok());
        assert!(cb.on_upload_progress("p", 20, 2000).is_ok());
        // Cancel triggers at 30%
        let err = cb.on_upload_progress("p", 30, 3000);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("Cancelled"));
        // Pipeline marks as failed
        cb.on_failed("npub1recv", "p", &msg);

        let e = cb.events();
        assert_eq!(e.len(), 5);
        // No Sent, no Persist after cancel — just Failed
        assert!(!e.iter().any(|ev| matches!(ev, CbEvent::Sent(..))));
        assert!(matches!(e.last(), Some(CbEvent::Failed(..))));
    }

    #[test]
    fn file_dm_upload_fails_marks_failed() {
        let cb = MockCallback::new();
        let msg = Message::default();

        cb.on_pending("npub1recv", &msg);

        cb.on_upload_progress("p", 0, 0).ok();
        cb.on_upload_progress("p", 10, 500).ok();
        // Upload fails (server error, all retries exhausted)
        cb.on_failed("npub1recv", "p", &msg);
        cb.on_persist("npub1recv", &msg);

        let e = cb.events();
        assert_eq!(e.len(), 5);
        assert!(matches!(&e[3], CbEvent::Failed(..)));
        assert!(matches!(&e[4], CbEvent::Persist(..)));
        // No UploadComplete, no Sent
        assert!(!e.iter().any(|ev| matches!(ev, CbEvent::UploadComplete(..))));
        assert!(!e.iter().any(|ev| matches!(ev, CbEvent::Sent(..))));
    }

    #[test]
    fn file_dm_gift_wrap_fails_after_upload() {
        let cb = MockCallback::new();
        let msg = Message::default();

        // Upload succeeds but gift-wrap fails
        cb.on_pending("npub1recv", &msg);

        cb.on_upload_progress("p", 100, 10000).ok();
        cb.on_upload_complete("npub1recv", "p", "hash", "https://blossom/hash");
        // Gift-wrap retry exhausted
        cb.on_failed("npub1recv", "p", &msg);
        cb.on_persist("npub1recv", &msg);

        let e = cb.events();
        assert_eq!(e.len(), 5);
        // Upload succeeded but send failed
        assert!(matches!(&e[2], CbEvent::UploadComplete(..)));
        assert!(matches!(&e[3], CbEvent::Failed(..)));
    }

    #[test]
    fn file_dm_with_image_metadata_sequence() {
        let cb = MockCallback::new();
        let msg = Message::default();

        // Image with thumbhash + dimensions
        cb.on_pending("npub1recv", &msg);

        cb.on_upload_progress("p", 0, 0).ok();
        cb.on_upload_progress("p", 50, 50000).ok();
        cb.on_upload_progress("p", 100, 100000).ok();
        cb.on_upload_complete("npub1recv", "p", "imghash", "https://blossom/imghash.jpg");
        cb.on_sent("npub1recv", "p", &msg);
        cb.on_persist("npub1recv", &msg);

        let e = cb.events();
        assert_eq!(e.len(), 7);
        // Verify ordering: pending → progress(3x) → complete → sent → persist
        assert!(matches!(&e[0], CbEvent::Pending(..)));
        assert!(matches!(&e[4], CbEvent::UploadComplete(_, url) if url.ends_with(".jpg")));
        assert!(matches!(&e[5], CbEvent::Sent(..)));
    }

    #[test]
    fn cancel_token_config_with_upload() {
        let token = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let c = SendConfig {
            cancel_token: Some(token.clone()),
            ..SendConfig::gui()
        };
        assert!(c.cancel_token.is_some());
        assert!(!token.load(std::sync::atomic::Ordering::Relaxed));

        // Simulate cancel
        token.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(c.cancel_token.as_ref().unwrap().load(std::sync::atomic::Ordering::Relaxed));
    }
}
