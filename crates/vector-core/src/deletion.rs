//! Message deletion — Vector's "delete from network" capability.
//!
//! NIP-17 DMs are wrapped in kind-1059 gift-wrap events signed by an
//! ephemeral key. The standard NIP-59 implementation discards that key
//! after signing, making the wrap permanently un-deletable: privacy by
//! obscurity, since the wrap continues to sit on inbox relays
//! decryptable by anyone with the recipient key.
//!
//! Vector retains the ephemeral key (see `db::nip17_keys`) so that on
//! user request we can publish an author-signed NIP-09 deletion against
//! every wrap and have relays drop it. Privacy by control.
//!
//! Scope: this module deletes the user's **own** outbound messages. It
//! does not (and cannot) delete messages sent by others — those wraps
//! were signed by ephemeral keys we never held.

use nostr_sdk::prelude::*;

use crate::inbox_relays::{get_publish_tracker, send_gift_wrap};
use crate::state::{MY_PUBLIC_KEY, MY_SECRET_KEY, NOSTR_CLIENT};

/// Cooperative-hide notice expiry: 30 days. After this window relays
/// drop the gift-wrap (NIP-40) and clients that come online later won't
/// see the deletion notice — but they also won't see the original wrap
/// (it was nuked from relays in Layer 1), so there's nothing to delete
/// on their side anyway. Recipients who already fetched and decrypted
/// the original need the notice to drop their local copy; 30 days is
/// generous coverage for "live" use.
const COOPERATIVE_HIDE_EXPIRY_SECS: u64 = 60 * 60 * 24 * 30;

/// Outcome of a delete-own-* operation.
///
/// Vector's deletion is layered and best-effort: any subset of the
/// layers may be available depending on whether we hold retained
/// ephemeral keys, whether the message has attachments, etc. The
/// outcome reports what was attempted so the caller can show an
/// honest post-action summary.
#[derive(serde::Serialize, Debug, Clone, Default)]
pub struct DeleteOutcome {
    /// Number of retained wraps for which we dispatched a NIP-09
    /// deletion task (Layer 1 — relay-level nuke). Zero when the
    /// message predates retention or was sent from a different device.
    pub wraps_dispatched: usize,
    /// Total wraps we had keys for at delete time (= wraps_dispatched
    /// for now; reserved for future "skipped due to error" reporting).
    pub wraps_total: usize,
    /// Whether we sent a cooperative-hide notice (Layer 2). Always true
    /// for own deletions in groups/DMs that succeed; tells live Vector
    /// clients to drop the row from local UI.
    pub cooperative_hide_sent: bool,
    /// Number of Blossom blobs we asked the upload server to delete.
    /// Best-effort: actual server response is logged, not surfaced.
    pub blobs_dispatched: usize,
    /// True iff at least one of (wrap nuke, cooperative hide, blob
    /// delete) was attempted. False means the only thing the operation
    /// could do is a local-state drop — caller can use this to surface
    /// "we couldn't actually remove this from the network" copy.
    pub any_network_action: bool,
}

/// Delete an outbound DM from the network by publishing NIP-09
/// deletions against every retained gift-wrap for `rumor_id`.
///
/// Per-relay event-driven dispatch: the publish/delete race is closed
/// by listening to each wrap's `WrapPublishTracker` (registered at
/// send time). NIP-09 fires at each relay only **after** that relay
/// has confirmed receiving the wrap — relays that haven't received it
/// yet wait until they do, relays that already have it get NIP-09
/// immediately, relays where the publish failed get nothing (no event
/// there to delete).
///
/// Each per-wrap deletion runs as a background tokio task so the API
/// returns immediately. Local UI removal happens synchronously; the
/// caller's UX never blocks on relay roundtrips.
///
/// Returns `Err` if no retained keys exist for the rumor (predates
/// the retention feature, sent from a different device, etc).
pub async fn delete_own_dm(rumor_id: &EventId) -> Result<DeleteOutcome, String> {
    let client = NOSTR_CLIENT.get().ok_or("Not logged in")?;
    let keys = crate::db::nip17_keys::get_wrap_keys_for_rumor(rumor_id)
        .unwrap_or_default();

    // Snapshot the message + recipient BEFORE any state/DB cleanup.
    // Attachment URLs feed Blossom DELETE; recipient pubkey feeds the
    // cooperative-hide gift wrap. We try to recover both even when no
    // retained keys exist (older messages, pre-retention sends).
    //
    // INVARIANT: `chat.id` for a DM is the counterpart's npub. The
    // `find_message` lookup here is unscoped, but `delete_own_dm` is
    // only ever called from the DM branch of the Tauri command — the
    // group branch routes to `delete_own_group_message` instead.
    // `from_bech32` silently yields `None` if the invariant ever
    // breaks; that just disables Layer 2 cooperative-hide for the
    // call. Layer 1 (retained-key relay nuke) and Layer 3 (Blossom)
    // remain functional.
    let (attachment_urls, recipient_from_state) = {
        let state = crate::state::STATE.lock().await;
        match state.find_message(&rumor_id.to_hex()) {
            Some((chat, msg)) => {
                debug_assert!(
                    matches!(chat.chat_type, crate::chat::ChatType::DirectMessage),
                    "delete_own_dm called on non-DM chat — caller bug"
                );
                let urls = msg
                    .attachments
                    .iter()
                    .map(|a| a.url.to_string())
                    .filter(|u| !u.is_empty())
                    .collect::<Vec<_>>();
                let recipient = nostr_sdk::PublicKey::from_bech32(&chat.id).ok();
                (urls, recipient)
            }
            None => (Vec::new(), None),
        }
    };

    let wraps_total = keys.len();
    let mut wraps_dispatched = 0usize;

    // Layer 1 — relay-level nuke. Only possible when we still hold
    // retained wrap keys for this rumor.
    for stored in keys.iter() {
        let client = client.clone();
        let wrap_event_id = stored.wrap_event_id;
        let secret = stored.secret.clone();
        let relay_urls = stored.relay_urls.clone();
        tokio::spawn(async move {
            delete_wrap_per_relay(&client, wrap_event_id, secret, relay_urls).await;
            if let Err(e) = crate::db::nip17_keys::purge_wrap_keys(&[wrap_event_id]) {
                crate::log_warn!("[NIP-17 delete] failed to purge wrap key: {}", e);
            }
        });
        wraps_dispatched += 1;
    }

    // Layer 2 — cooperative hide. Always send a notice if we know the
    // recipient: tells live Vector clients to drop their local copy.
    // Prefer the recipient pubkey from a retained wrap key if we have
    // one (recipient role); fall back to the chat counterpart from
    // STATE. The notice itself is signed by our main key, so this
    // works even when retained wrap keys are missing.
    let cooperative_recipient = keys
        .iter()
        .find(|k| {
            k.role == crate::db::nip17_keys::WrapRole::Recipient
                || k.role == crate::db::nip17_keys::WrapRole::Retry
        })
        .map(|k| k.recipient_pubkey)
        .or(recipient_from_state);

    let mut cooperative_hide_sent = false;
    if let Some(recipient) = cooperative_recipient {
        match publish_cooperative_hide(client, rumor_id, &recipient).await {
            Ok(()) => cooperative_hide_sent = true,
            Err(e) => crate::log_warn!("[NIP-17 delete] cooperative-hide notice failed: {}", e),
        }
    }

    // Layer 3 — Blossom blob delete. Always attempt for any
    // attachment, even when retained wrap keys are missing (Blossom
    // auth uses the user's main signing key, not the wrap key).
    let mut blobs_dispatched = 0usize;
    if !attachment_urls.is_empty() {
        if let Some(signer_keys) = MY_SECRET_KEY.to_keys() {
            blobs_dispatched = attachment_urls.len();
            crate::blossom::delete_blobs_best_effort(signer_keys, attachment_urls);
        }
    }

    let any_network_action =
        wraps_dispatched > 0 || cooperative_hide_sent || blobs_dispatched > 0;

    Ok(DeleteOutcome {
        wraps_total,
        wraps_dispatched,
        cooperative_hide_sent,
        blobs_dispatched,
        any_network_action,
    })
}

/// Per-relay deletion for a single wrap. Subscribes to the wrap's
/// publish tracker and fires NIP-09 to each relay as soon as that
/// relay confirms receiving the wrap. Relays where the publish failed
/// don't get NIP-09 (no event there to delete).
///
/// If no live tracker exists (cross-restart: the original publishes
/// completed in a previous session), falls back to a best-effort
/// broadcast against every targeted relay. Relays that don't have
/// the wrap will no-op the deletion; that's safe.
async fn delete_wrap_per_relay(
    client: &Client,
    wrap_event_id: EventId,
    secret: SecretKey,
    targeted_relays: Vec<String>,
) {
    let ephemeral_keys = Keys::new(secret);
    let deletion = match EventBuilder::new(Kind::EventDeletion, "")
        .tag(Tag::event(wrap_event_id))
        .tag(Tag::custom(TagKind::custom("k"), ["1059"]))
        .sign_with_keys(&ephemeral_keys)
    {
        Ok(ev) => ev,
        Err(e) => {
            eprintln!(
                "[NIP-17 delete] failed to sign deletion for wrap {}: {}",
                wrap_event_id.to_hex(),
                e
            );
            return;
        }
    };

    if let Some(tracker) = get_publish_tracker(&wrap_event_id) {
        // Live tracker: walk the success stream as relays settle.
        let mut delivered = 0usize;
        let mut cursor = 0usize;
        while let Some(url) = tracker.next_success(&mut cursor).await {
            if send_to_one_relay(client, &url, &deletion).await {
                delivered += 1;
            }
        }
        crate::log_info!(
            "[NIP-17 delete] wrap {} — NIP-09 delivered to {} relay(s) via tracker",
            wrap_event_id.to_hex(),
            delivered
        );
    } else {
        // No tracker (cross-restart or already-GC'd). Best-effort
        // broadcast: fire NIP-09 at every targeted relay; relays
        // that lack the wrap silently no-op.
        let urls: Vec<RelayUrl> = targeted_relays
            .iter()
            .filter_map(|s| RelayUrl::parse(s).ok())
            .collect();
        let total = urls.len();
        let mut delivered = 0usize;
        for url in urls {
            if send_to_one_relay(client, &url, &deletion).await {
                delivered += 1;
            }
        }
        crate::log_info!(
            "[NIP-17 delete] wrap {} — fallback broadcast: NIP-09 delivered to {}/{} relay(s)",
            wrap_event_id.to_hex(),
            delivered,
            total
        );
    }
}

/// Direct publish to a single relay handle. Returns true if the relay
/// acknowledged. Returns false if the relay isn't in our pool, the
/// publish hit a non-rate-limit error, or rate-limit retries were
/// exhausted.
///
/// Per-URL outcome is logged so the user can pinpoint which relay is
/// keeping a wrap alive after deletion — relays that ACK a NIP-09 but
/// don't actually drop the event are non-compliant; the
/// `verify_relay_dropped` probe scheduled below is the receipt that
/// identifies them.
///
/// Rate-limit handling: relays like damus.io will reject NIP-09s with
/// "rate-limited: you are noting too much" when the user deletes a
/// few messages in quick succession. The deletion isn't a real
/// failure, just back-pressure — so we wait and retry up to
/// `MAX_RATELIMIT_RETRIES` times (each retry sleeps 30s). The whole
/// loop runs inside the per-relay deletion task (already spawned), so
/// the user's UX is unaffected; the wrap stays on the relay only
/// until we get through.
///
/// On successful ACK, schedules a verification probe (~2s later) that
/// re-queries the relay for the original wrap event id and reports
/// whether the relay actually honored the deletion. Catches relays
/// that lie about NIP-09 compliance.
async fn send_to_one_relay(client: &Client, url: &RelayUrl, event: &Event) -> bool {
    /// Max attempts to push past a rate-limit. With 30s between
    /// attempts that's a 10-minute window — generous for any sane
    /// per-IP rate limit. If the relay is still rate-limiting us
    /// after that, something else is wrong and we give up so the
    /// task doesn't loop forever.
    const MAX_RATELIMIT_RETRIES: u32 = 20;
    /// Pause between rate-limit retries.
    const RATELIMIT_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);

    let pool = client.pool();
    let relays = pool.relays().await;
    let relay = match relays.get(url) {
        Some(r) => r.clone(),
        None => {
            crate::log_warn!("[delete] relay {} not in pool — NIP-09 not delivered", url);
            return false;
        }
    };
    drop(relays);

    let mut retries = 0u32;
    loop {
        match relay.send_event(event).await {
            Ok(_) => {
                if retries == 0 {
                    crate::log_info!("[delete] relay {} ACK'd NIP-09", url);
                } else {
                    crate::log_info!(
                        "[delete] relay {} ACK'd NIP-09 (after {} retr{})",
                        url,
                        retries,
                        if retries == 1 { "y" } else { "ies" }
                    );
                }
                if let Some(wrap_id) = extract_target_event_id(event) {
                    let url_clone = url.clone();
                    let client_clone = client.clone();
                    tokio::spawn(async move {
                        verify_relay_dropped(&client_clone, &url_clone, &wrap_id).await;
                    });
                }
                return true;
            }
            Err(e) => {
                let err_str = e.to_string();
                let lc = err_str.to_ascii_lowercase();
                let is_rate_limit = lc.contains("rate-limit")
                    || lc.contains("rate limit")
                    || lc.contains("noting too much");
                let is_transient = lc.contains("timeout")
                    || lc.contains("timed out")
                    || lc.contains("connection reset")
                    || lc.contains("connection refused")
                    || lc.contains("connection closed")
                    || lc.contains("broken pipe")
                    || lc.contains("not connected");
                let retryable = is_rate_limit || is_transient;
                if retryable && retries < MAX_RATELIMIT_RETRIES {
                    retries += 1;
                    let reason = if is_rate_limit { "rate-limited" } else { "transient error" };
                    crate::log_warn!(
                        "[delete] relay {} {} (attempt {}/{}; err: {}); waiting {}s",
                        url,
                        reason,
                        retries,
                        MAX_RATELIMIT_RETRIES,
                        err_str,
                        RATELIMIT_BACKOFF.as_secs()
                    );
                    tokio::time::sleep(RATELIMIT_BACKOFF).await;
                    continue;
                }
                if retryable {
                    crate::log_warn!(
                        "[delete] relay {} still failing after {} retries ({}); scheduling verify probe in case it eventually accepted",
                        url,
                        retries,
                        err_str
                    );
                    // Even though our publish never ACK'd, the relay may
                    // have actually received and processed the event
                    // (timeouts often mean "ACK lost on the way back").
                    // Schedule a verify probe so we still log whether the
                    // wrap is gone.
                    if let Some(wrap_id) = extract_target_event_id(event) {
                        let url_clone = url.clone();
                        let client_clone = client.clone();
                        tokio::spawn(async move {
                            verify_relay_dropped(&client_clone, &url_clone, &wrap_id).await;
                        });
                    }
                } else {
                    crate::log_warn!("[delete] relay {} rejected NIP-09: {}", url, err_str);
                }
                return false;
            }
        }
    }
}

/// Pull the target event id from a NIP-09 deletion event's first
/// `["e", ...]` tag. Used by the verification probe to know which
/// wrap to look for after asking the relay to delete it.
fn extract_target_event_id(deletion: &Event) -> Option<EventId> {
    deletion.tags.iter().find_map(|tag| {
        let s = tag.as_slice();
        if s.len() >= 2 && s[0] == "e" {
            EventId::from_hex(&s[1]).ok()
        } else {
            None
        }
    })
}

/// 2s after a relay ACKs our NIP-09, ask it whether the target wrap
/// is actually gone. Logs a clear "GONE" or "STILL PRESENT" so we can
/// identify non-compliant relays without bisecting via external tools.
async fn verify_relay_dropped(client: &Client, url: &RelayUrl, wrap_event_id: &EventId) {
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let pool = client.pool();
    let relays = pool.relays().await;
    let relay = match relays.get(url) {
        Some(r) => r.clone(),
        None => return,
    };

    let filter = Filter::new().id(*wrap_event_id);
    match relay
        .fetch_events(
            filter,
            std::time::Duration::from_secs(5),
            ReqExitPolicy::ExitOnEOSE,
        )
        .await
    {
        Ok(events) => {
            let still_present = events.into_iter().next().is_some();
            if still_present {
                crate::log_warn!(
                    "[delete-verify] relay {} STILL HAS wrap {} (non-compliant — relay ACK'd NIP-09 but did not drop the event)",
                    url,
                    wrap_event_id.to_hex()
                );
            } else {
                crate::log_info!(
                    "[delete-verify] relay {} confirmed wrap {} is GONE",
                    url,
                    wrap_event_id.to_hex()
                );
            }
        }
        Err(e) => {
            crate::log_warn!(
                "[delete-verify] relay {} probe failed for wrap {}: {}",
                url,
                wrap_event_id.to_hex(),
                e
            );
        }
    }
}

// ============================================================================
// MLS group-message deletion — sibling of delete_own_dm
// ============================================================================

/// Delete an outbound MLS group message from the network by publishing
/// NIP-09 deletions against every retained kind-445 wrapper for
/// `message_id`, plus a cooperative-hide kind-5 rumor INSIDE the MLS
/// group so live members' Vector clients drop their local copy.
///
/// Layer 1 (network deletion): NIP-09 signed by the retained ephemeral
/// key against the kind-445 wrapper. Same per-relay event-driven
/// pattern as DM deletion.
///
/// Layer 2 (cooperative hide): kind-5 rumor sent through the normal
/// MLS message path (`send_mls_message`). All cooperating Vector
/// clients in the group drop the message from local UI on receipt;
/// non-Vector clients ignore unknown rumor kinds.
///
/// Author authorization: enforced on the receiving side, not here.
/// We only let the user delete their own messages (the UI gates on
/// `mine == true`); the receiver verifies `deletion.author ==
/// original.author` before honouring (`mls/group_handler.rs`).
pub async fn delete_own_group_message(
    group_id: &str,
    message_id: &str,
) -> Result<DeleteOutcome, String> {
    let _client = NOSTR_CLIENT.get().ok_or("Not logged in")?;
    let keys = crate::db::mls_wrap_keys::get_wrap_keys_for_message(message_id)
        .unwrap_or_default();

    // Snapshot attachments BEFORE any state/DB cleanup so we can also
    // remove the encrypted ciphertext from Blossom servers.
    let attachment_urls: Vec<String> = {
        let state = crate::state::STATE.lock().await;
        state.find_message(message_id)
            .map(|(_, msg)| {
                msg.attachments
                    .iter()
                    .map(|a| a.url.to_string())
                    .filter(|u| !u.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };

    let wraps_total = keys.len();
    let mut wraps_dispatched = 0usize;

    // Layer 1 — kind-445 wrapper relay-nuke. Only possible when we
    // still hold retained wrap keys.
    for stored in keys.iter() {
        let client = NOSTR_CLIENT.get().unwrap().clone();
        let wrap_event_id = stored.wrap_event_id;
        let secret = stored.secret.clone();
        let relay_urls = stored.relay_urls.clone();
        tokio::spawn(async move {
            delete_mls_wrap_per_relay(&client, wrap_event_id, secret, relay_urls).await;
            if let Err(e) = crate::db::mls_wrap_keys::purge_wrap_keys(&[wrap_event_id]) {
                crate::log_warn!("[MLS delete] failed to purge wrap key: {}", e);
            }
        });
        wraps_dispatched += 1;
    }

    // Layer 2 — cooperative-hide kind-5 rumor inside the MLS group.
    // Always sent; works regardless of whether retained keys exist
    // (signed by the user's main key via send_mls_message).
    let target_id = EventId::from_hex(message_id)
        .map_err(|e| format!("Invalid message id: {}", e))?;
    let cooperative_hide_sent = match publish_group_cooperative_hide(group_id, &target_id).await {
        Ok(()) => true,
        Err(e) => {
            crate::log_warn!("[MLS delete] cooperative-hide notice failed: {}", e);
            false
        }
    };

    // Layer 3 — Blossom blob delete. Always attempt for any attachment.
    let mut blobs_dispatched = 0usize;
    if !attachment_urls.is_empty() {
        if let Some(signer_keys) = MY_SECRET_KEY.to_keys() {
            blobs_dispatched = attachment_urls.len();
            crate::blossom::delete_blobs_best_effort(signer_keys, attachment_urls);
        }
    }

    let any_network_action =
        wraps_dispatched > 0 || cooperative_hide_sent || blobs_dispatched > 0;

    Ok(DeleteOutcome {
        wraps_total,
        wraps_dispatched,
        cooperative_hide_sent,
        blobs_dispatched,
        any_network_action,
    })
}

/// Per-relay deletion for a single MLS wrapper. Subscribes to the
/// wrapper's publish tracker (registered at send time by
/// `inbox_relays::spawn_tracked_publish` via `send_event_first_ok`)
/// and fires NIP-09 to each relay as soon as it confirms the wrapper.
/// Falls back to a best-effort broadcast if no live tracker exists
/// (cross-restart). Mirrors `delete_wrap_per_relay` for NIP-17.
async fn delete_mls_wrap_per_relay(
    client: &Client,
    wrap_event_id: EventId,
    secret: SecretKey,
    targeted_relays: Vec<String>,
) {
    let ephemeral_keys = Keys::new(secret);
    let deletion = match EventBuilder::new(Kind::EventDeletion, "")
        .tag(Tag::event(wrap_event_id))
        .tag(Tag::custom(TagKind::custom("k"), ["445"]))
        .sign_with_keys(&ephemeral_keys)
    {
        Ok(ev) => ev,
        Err(e) => {
            eprintln!(
                "[MLS delete] failed to sign deletion for wrap {}: {}",
                wrap_event_id.to_hex(),
                e
            );
            return;
        }
    };

    if let Some(tracker) = get_publish_tracker(&wrap_event_id) {
        let mut delivered = 0usize;
        let mut cursor = 0usize;
        while let Some(url) = tracker.next_success(&mut cursor).await {
            if send_to_one_relay(client, &url, &deletion).await {
                delivered += 1;
            }
        }
        crate::log_info!(
            "[MLS delete] wrap {} — NIP-09 delivered to {} relay(s) via tracker",
            wrap_event_id.to_hex(),
            delivered
        );
    } else {
        let urls: Vec<RelayUrl> = targeted_relays
            .iter()
            .filter_map(|s| RelayUrl::parse(s).ok())
            .collect();
        let total = urls.len();
        let mut delivered = 0usize;
        for url in urls {
            if send_to_one_relay(client, &url, &deletion).await {
                delivered += 1;
            }
        }
        crate::log_info!(
            "[MLS delete] wrap {} — fallback broadcast: NIP-09 delivered to {}/{} relay(s)",
            wrap_event_id.to_hex(),
            delivered,
            total
        );
    }
}

/// Admin-moderation hide of someone else's group message.
///
/// Cooperative-only: relay-level deletion is impossible because the
/// admin doesn't hold the original sender's ephemeral wrap key (only
/// the original sender does). All this does is publish a kind-5 rumor
/// inside the MLS group, signed by the admin's main key, asking
/// cooperative Vector clients to drop the target from local storage.
///
/// Authorization is verified at the receiving side: the receiver
/// looks up the group's current admin set and accepts the deletion
/// only if the sender is in it. Non-Vector clients ignore.
///
/// Caller should already have confirmed via UI gate that the user is
/// an admin of the group before calling this.
pub async fn admin_hide_group_message(
    group_id: &str,
    message_id: &str,
) -> Result<(), String> {
    let target_id = EventId::from_hex(message_id)
        .map_err(|e| format!("Invalid message id: {}", e))?;
    publish_group_cooperative_hide(group_id, &target_id).await
}

/// Publish the cooperative-hide notice INSIDE the MLS group: a kind-5
/// rumor referencing the inner message id, signed by the user's main
/// key, delivered through `send_mls_message` (so it gets a fresh
/// kind-445 wrapper and reaches every group member). Carries a
/// NIP-40 expiration tag (30 days) so the wrapper rolls off relays
/// once the live-client window has passed.
async fn publish_group_cooperative_hide(
    group_id: &str,
    target_message_id: &EventId,
) -> Result<(), String> {
    let my_pk = *MY_PUBLIC_KEY.get().ok_or("Public key not set")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let expiration_ts = now + COOPERATIVE_HIDE_EXPIRY_SECS;

    let rumor = EventBuilder::new(Kind::EventDeletion, "")
        .tag(Tag::event(*target_message_id))
        .tag(Tag::custom(TagKind::custom("k"), ["9"]))
        .tag(Tag::expiration(Timestamp::from(expiration_ts)))
        .build(my_pk);

    crate::mls::messaging::send_mls_message(group_id, rumor, None).await
}

/// Publish the Layer-2 cooperative-hide notice — a kind-5 NIP-09 rumor
/// signed by the user's main key, gift-wrapped to the recipient and to
/// self. Carries a NIP-40 expiration tag (30 days) so relays drop the
/// wrap once the live-client window has passed.
async fn publish_cooperative_hide(
    client: &Client,
    target_rumor_id: &EventId,
    recipient: &PublicKey,
) -> Result<(), String> {
    let my_pk = *MY_PUBLIC_KEY.get().ok_or("Public key not set")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let expiration_ts = now + COOPERATIVE_HIDE_EXPIRY_SECS;

    // Build the kind-5 rumor (signed by our main key via the gift-wrap
    // path's seal step). Reference the inner rumor id with `e`, hint at
    // the original kind via `k`, expire after 30 days.
    let rumor = EventBuilder::new(Kind::EventDeletion, "")
        .tag(Tag::event(*target_rumor_id))
        .tag(Tag::custom(TagKind::custom("k"), ["14"]))
        .tag(Tag::expiration(Timestamp::from(expiration_ts)))
        .build(my_pk);

    // Wrap and send to recipient. Also wrap and send to self so other
    // devices belonging to the user drop the message from their local
    // view too. Best-effort, fire-and-forget.
    let r1 = send_gift_wrap(client, recipient, rumor.clone(), []).await;
    let r2 = send_gift_wrap(client, &my_pk, rumor, []).await;

    if r1.is_err() && r2.is_err() {
        return Err("both cooperative-hide deliveries failed".to_string());
    }
    Ok(())
}
