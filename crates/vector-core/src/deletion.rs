//! Message deletion: Vector's "delete from network" for DMs.
//!
//! A NIP-17 DM travels as kind-1059 gift wraps, each signed by a single-use key and `p`-tagged
//! to the one person who can open it. A relay honours a deletion of a wrap only from that
//! `p`-tagged owner (NIP-59; CORD-01 §Deletions). A Concord plane shares one wrap key among all
//! its members, so a deletion signed by a wrap's author would let any of them wipe the plane:
//! Vector's relay refuses those, as every relay serving Concord should. A relay also refuses a
//! vanish request (NIP-62) from a key that has written wraps, for the same reason.
//!
//! So each side deletes the copies addressed to itself:
//! - The sender deletes its own self-copies with its own key, at once.
//! - Either side deletes a message it received the same way: its copy by its own key, and a
//!   hide request to itself for its other devices.
//! - A hide request (a kind-5 rumor gift-wrapped to the recipient and to self, its wrap
//!   expiring after 30 days) asks every other device to drop the message and delete the copy
//!   it was sent the same way. A client that ignores it keeps the message.
//! - A device that later meets any other wrap of a deleted message deletes that one too, and
//!   one whose hide request came before the message holds it until the message arrives.
//!
//! Every wrap whose single-use key we kept (`db::nip17_keys`) also gets an author-signed
//! deletion, best-effort: a relay still honouring those drops the recipient's copy even when
//! their client never will, and our own on a relay that ignores the owner. On a relay that
//! follows the spec it changes nothing.

use crate::event_ext::FinalizeUnsignedWithId;
use nostr_sdk::prelude::*;

use crate::inbox_relays::{get_publish_tracker, resolve_gift_wrap_targets, send_gift_wrap, teardown_gift_wrap_targets};
use crate::state::{my_public_key, nostr_client};

/// Hide-request expiry: 30 days, on the rumor and on its wrap, so relays drop it. A device
/// first online after that no longer holds the message: every wrap it could fetch is gone.
pub(crate) const COOPERATIVE_HIDE_EXPIRY_SECS: u64 = 60 * 60 * 24 * 30;

/// Wrap-deletion expiry: the deletion outlives any honest re-broadcast of what it deletes,
/// then leaves rather than sit on relays forever.
const WRAP_DELETION_EXPIRY_SECS: u64 = 60 * 60 * 24 * 90;

/// The most wraps one deletion names.
pub(crate) const WRAPS_PER_DELETION: usize = 100;

/// Outcome of a delete-own-* operation.
///
/// Vector's deletion is layered and best-effort: any subset of the
/// layers may be available depending on whether we hold retained
/// ephemeral keys, whether the message has attachments, etc. The
/// outcome reports what was attempted so the caller can show an
/// honest post-action summary.
#[derive(serde::Serialize, Debug, Clone, Default)]
pub struct DeleteOutcome {
    /// Wrap deletions dispatched: our own copies signed by our key, and best-effort ones
    /// signed by each retained single-use key.
    pub wraps_dispatched: usize,
    /// Wraps of the message this device knows of.
    pub wraps_total: usize,
    /// Whether the hide request went out: live Vector clients drop the message and delete
    /// their own copies of it.
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

/// Delete an outbound DM from the network: our own copies of it with our key, the hide
/// request for the recipient's, its attachments from Blossom. Every relay round trip runs
/// in the background; the call returns once the local state is gone.
pub async fn delete_own_dm(rumor_id: &EventId) -> Result<DeleteOutcome, String> {
    let client = nostr_client().ok_or("Not logged in")?;
    // Durable tombstone FIRST: a wrap some relay keeps re-serves after a restart, and without
    // it the message would re-ingest and resurrect.
    let rumor_hex = rumor_id.to_hex();
    if let Err(e) = crate::db::events::add_message_tombstone(&rumor_hex) {
        crate::log_warn!("[NIP-17 delete] tombstone write failed: {}", e);
    }
    crate::state::note_message_deleted(&rumor_hex);
    let keys = crate::db::nip17_keys::get_wrap_keys_for_rumor(rumor_id)
        .unwrap_or_default();
    let received = crate::db::events::wrapper_event_id_of(&rumor_hex);

    // Snapshot the message + recipient BEFORE any state/DB cleanup.
    // Attachment URLs feed Blossom DELETE; the attachments themselves
    // feed local-cache file removal; recipient pubkey feeds the
    // hide request. We try to recover all three even when no retained
    // keys exist (older messages, pre-retention sends).
    //
    // INVARIANT: `chat.id` for a DM is the counterpart's npub. The
    // `find_message` lookup here is unscoped, but `delete_own_dm` is
    // only ever called from the DM branch of the Tauri command.
    // `from_bech32` silently yields `None` if the invariant ever
    // breaks; that just disables the hide request for the call.
    let (all_attachments, recipient_from_state) = {
        let state = crate::state::STATE.lock().await;
        match state.find_message(&rumor_id.to_hex()) {
            Some((chat, msg)) => {
                debug_assert!(
                    matches!(chat.chat_type, crate::chat::ChatType::DirectMessage),
                    "delete_own_dm called on non-DM chat — caller bug"
                );
                let recipient = nostr_sdk::prelude::PublicKey::from_bech32(&chat.id).ok();
                (msg.attachments.clone(), recipient)
            }
            None => (Vec::new(), None),
        }
    };

    // Refcount filter: drop attachments still referenced by sibling
    // messages so we don't yank cached files / Blossom blobs from
    // messages that still need them. Vector dedupes uploads by SHA-256:
    // re-sending the same file produces multiple messages pointing at
    // the same cached path AND the same Blossom blob. Deleting one
    // shouldn't delete the underlying resources.
    let unique_attachments = filter_unreferenced_attachments(
        &rumor_id.to_hex(),
        all_attachments,
    ).await;

    // Local cache nuke (canonicalize + managed-dir-only inside helper).
    delete_cached_attachment_files(&unique_attachments);

    // Blossom URLs derived from the filtered (refcount-aware) set — the
    // primary plus every mirror (same hash, other origins) — then gated
    // against the DB ledger: a smart-forwarded blob backs OTHER messages too.
    let attachment_urls: Vec<String> = scrub_safe_urls(&unique_attachments, &rumor_id.to_hex());

    let (wraps_dispatched, wraps_total) = dispatch_wrap_deletions(&client, &keys, received).await;

    // The hide request, when we know the recipient: their live clients drop the message and
    // delete their own copies. Signed by our main key, so it needs no retained wrap key.
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
        match publish_cooperative_hide(&client, rumor_id, Some(&recipient), 14).await {
            Ok(()) => cooperative_hide_sent = true,
            Err(e) => crate::log_warn!("[NIP-17 delete] cooperative-hide notice failed: {}", e),
        }
    }

    // Layer 3 — Blossom blob delete. Route through the active client
    // signer so bunker accounts sign DELETE auth under the user's
    // identity (Blossom enforces "uploader == authorized signer";
    // signing with the NIP-46 client keypair returns 401).
    let mut blobs_dispatched = 0usize;
    if !attachment_urls.is_empty() {
        if let Ok(signer) = crate::signer::active_signer() {
            blobs_dispatched = attachment_urls.len();
            crate::blossom::delete_blobs_best_effort(signer, attachment_urls);
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

/// Revoke one of OUR OWN DM reactions, as [`delete_own_dm`] deletes a message: our copies
/// with our key, a hide request (k=7) so the counterpart's clients drop the chip and delete
/// theirs. `recipient` is the DM counterpart, for when no recipient-role wrap key is retained.
pub async fn delete_own_reaction(
    reaction_id: &EventId,
    recipient: PublicKey,
) -> Result<DeleteOutcome, String> {
    let client = nostr_client().ok_or("Not logged in")?;
    // Durable tombstone first, as for messages: a surviving self-wrap re-served later
    // would otherwise bring the reaction back.
    let reaction_hex = reaction_id.to_hex();
    if let Err(e) = crate::db::events::add_message_tombstone(&reaction_hex) {
        crate::log_warn!("[reaction delete] tombstone write failed: {}", e);
    }
    crate::state::note_message_deleted(&reaction_hex);
    let keys = crate::db::nip17_keys::get_wrap_keys_for_rumor(reaction_id)
        .unwrap_or_default();
    let received = crate::db::events::wrapper_event_id_of(&reaction_hex);
    let (wraps_dispatched, wraps_total) = dispatch_wrap_deletions(&client, &keys, received).await;

    // The hide request (k=7), to a recipient from a retained recipient/retry wrap key or the
    // passed-in counterpart. Signed by our main key, so it needs no retained wrap key.
    let cooperative_recipient = keys
        .iter()
        .find(|k| {
            k.role == crate::db::nip17_keys::WrapRole::Recipient
                || k.role == crate::db::nip17_keys::WrapRole::Retry
        })
        .map(|k| k.recipient_pubkey)
        .unwrap_or(recipient);

    let mut cooperative_hide_sent = false;
    match publish_cooperative_hide(&client, reaction_id, Some(&cooperative_recipient), 7).await {
        Ok(()) => cooperative_hide_sent = true,
        Err(e) => crate::log_warn!("[reaction delete] cooperative-hide failed: {}", e),
    }

    let any_network_action = wraps_dispatched > 0 || cooperative_hide_sent;
    Ok(DeleteOutcome {
        wraps_total,
        wraps_dispatched,
        cooperative_hide_sent,
        blobs_dispatched: 0,
        any_network_action,
    })
}

/// Delete a DM someone sent us. Its gift wrap is addressed to us, so our key deletes our copy
/// from relays (NIP-59), and a hide request to ourselves takes it off our other devices. The
/// sender's copies are theirs to delete.
pub async fn delete_received_dm(rumor_id: &EventId) -> Result<DeleteOutcome, String> {
    let client = nostr_client().ok_or("Not logged in")?;
    let rumor_hex = rumor_id.to_hex();
    // Tombstone first, as for our own: another wrap of it re-served later must not bring it back.
    if let Err(e) = crate::db::events::add_message_tombstone(&rumor_hex) {
        crate::log_warn!("[NIP-17 delete] tombstone write failed: {}", e);
    }
    crate::state::note_message_deleted(&rumor_hex);
    let received = crate::db::events::wrapper_event_id_of(&rumor_hex);

    // A downloaded file goes with it; the sender's upload is theirs, so Blossom is left alone.
    let attachments = {
        let state = crate::state::STATE.lock().await;
        state.find_message(&rumor_hex).map(|(_, msg)| msg.attachments.clone()).unwrap_or_default()
    };
    delete_cached_attachment_files(&filter_unreferenced_attachments(&rumor_hex, attachments).await);

    let (wraps_dispatched, wraps_total) = dispatch_wrap_deletions(&client, &[], received).await;
    let cooperative_hide_sent = match publish_cooperative_hide(&client, rumor_id, None, 14).await {
        Ok(()) => true,
        Err(e) => {
            crate::log_warn!("[NIP-17 delete] hide request to our devices failed: {}", e);
            false
        }
    };
    Ok(DeleteOutcome {
        wraps_total,
        wraps_dispatched,
        cooperative_hide_sent,
        blobs_dispatched: 0,
        any_network_action: wraps_dispatched > 0 || cooperative_hide_sent,
    })
}

/// A deletion of gift wraps: `k` names their kind, and it expires once nothing could
/// still re-broadcast them.
pub(crate) fn wrap_deletion(wrap_ids: &[EventId]) -> EventBuilder {
    let expires = Timestamp::now().as_secs().saturating_add(WRAP_DELETION_EXPIRY_SECS);
    wrap_ids
        .iter()
        .fold(EventBuilder::new(Kind::EventDeletion, ""), |b, id| b.tag(Tag::event(*id)))
        .tag(Tag::custom("k", ["1059"]))
        .tag(Tag::expiration(Timestamp::from(expires)))
}

/// [`wrap_deletion`] signed by our own key, the `p`-tagged owner of the wraps it names: the
/// one signer every relay honours. Refused when the session's identity moved meanwhile.
async fn owner_deletion(wrap_ids: &[EventId], owner: &PublicKey) -> Result<Event, String> {
    let deletion = crate::sign_builder(wrap_deletion(wrap_ids)).await?;
    if deletion.pubkey != *owner {
        return Err("the account changed before the deletion was signed".to_string());
    }
    Ok(deletion)
}

/// Take a message's wraps off the relays: our own copies with our own key, and every wrap whose
/// single-use key we kept with that key too, best-effort, for relays still honouring an author's
/// deletion. Returns (dispatched, known).
async fn dispatch_wrap_deletions(
    client: &Client,
    keys: &[crate::db::nip17_keys::StoredWrapKey],
    received: Option<EventId>,
) -> (usize, usize) {
    let Some(me) = my_public_key() else { return (0, keys.len()) };
    let own = own_wraps(keys, received);
    let known = own.len() + keys.iter().filter(|k| !own.contains(&k.wrap_event_id)).count();
    let mut dispatched = 0usize;
    for wrap_id in own {
        // Signed now, while the account it belongs to is the one in use.
        match owner_deletion(&[wrap_id], &me).await {
            Ok(deletion) => {
                let client = client.clone();
                crate::db::spawn_bound(async move {
                    delete_wrap_per_relay(&client, wrap_id, &deletion, &me, true).await;
                });
                dispatched += 1;
            }
            Err(e) => crate::log_warn!("[NIP-17 delete] our own copy {} stays: {}", wrap_id.to_hex(), e),
        }
    }
    for stored in keys {
        let deletion = match wrap_deletion(&[stored.wrap_event_id]).finalize(&Keys::new(stored.secret.clone())) {
            Ok(ev) => ev,
            Err(e) => {
                crate::log_warn!("[NIP-17 delete] failed to sign deletion for wrap {}: {}", stored.wrap_event_id.to_hex(), e);
                continue;
            }
        };
        let (client, wrap_id, owner) = (client.clone(), stored.wrap_event_id, stored.recipient_pubkey);
        crate::db::spawn_bound(async move {
            delete_wrap_per_relay(&client, wrap_id, &deletion, &owner, false).await;
        });
        dispatched += 1;
    }
    let wraps: Vec<EventId> = keys.iter().map(|k| k.wrap_event_id).collect();
    if let Err(e) = crate::db::nip17_keys::purge_wrap_keys(&wraps) {
        crate::log_warn!("[NIP-17 delete] failed to purge wrap keys: {}", e);
    }
    (dispatched, known)
}

/// A message's wraps addressed to us, which our own key deletes: our self-copies, and the one
/// that brought it to this device, when one did.
fn own_wraps(keys: &[crate::db::nip17_keys::StoredWrapKey], received: Option<EventId>) -> Vec<EventId> {
    use crate::db::nip17_keys::WrapRole;
    let mut own: Vec<EventId> = keys.iter().filter(|k| k.role == WrapRole::SelfSend).map(|k| k.wrap_event_id).collect();
    if let Some(w) = received.filter(|w| !own.contains(w)) {
        own.push(w);
    }
    own
}

/// Pending deletions of wraps addressed to us, gathered so a sync that meets many deleted
/// messages signs one deletion, not one per wrap (a remote signer asks for each).
struct ReceivedWrapDeletions;

fn received_wrap_deletions() -> std::sync::Arc<std::sync::Mutex<Vec<EventId>>> {
    crate::db::current_session().scoped::<ReceivedWrapDeletions, _>()
}

#[cfg(test)]
pub(crate) fn queued_received_wrap_deletions() -> Vec<EventId> {
    received_wrap_deletions().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Delete a wrap addressed to us that carried a message since deleted, with our own key: the
/// copy a hide request asks us to take down, or one more copy of a message already gone.
pub(crate) fn delete_received_wrap(wrap_id: EventId) {
    let pending = received_wrap_deletions();
    let first = {
        let mut q = pending.lock().unwrap_or_else(|e| e.into_inner());
        if q.contains(&wrap_id) {
            return;
        }
        q.push(wrap_id);
        q.len() == 1
    };
    if !first {
        return;
    }
    crate::db::spawn_bound(async move {
        crate::rt::time::sleep(std::time::Duration::from_secs(2)).await;
        let wraps = std::mem::take(&mut *pending.lock().unwrap_or_else(|e| e.into_inner()));
        let (Some(client), Some(me)) = (nostr_client(), my_public_key()) else { return };
        for batch in wraps.chunks(WRAPS_PER_DELETION) {
            match owner_deletion(batch, &me).await {
                Ok(deletion) => publish_to_inbox(&client, &me, &deletion, true).await,
                Err(e) => crate::log_warn!("[NIP-17 delete] {} received wrap(s) stay: {}", batch.len(), e),
            }
        }
    });
}

/// Send `deletion` for `wrap_event_id` to each relay that holds it. A wrap published this
/// session gets it from each relay as that relay confirms the wrap, so it never outruns the
/// wrap; any other goes to `owner`'s inbox relays, where wraps addressed to them sit.
/// `verify` asks each relay that acknowledges whether the wrap is really gone.
async fn delete_wrap_per_relay(client: &Client, wrap_event_id: EventId, deletion: &Event, owner: &PublicKey, verify: bool) {
    let Some(tracker) = get_publish_tracker(&wrap_event_id) else {
        publish_to_inbox(client, owner, deletion, verify).await;
        return;
    };
    let mut delivered = 0usize;
    let mut cursor = 0usize;
    while let Some(url) = tracker.next_success(&mut cursor).await {
        let relay = client.relays().await.get(&url).cloned();
        match relay {
            Some(relay) if send_to_one_relay(client, &url, &relay, deletion, verify).await => delivered += 1,
            Some(_) => {}
            None => crate::log_warn!("[delete] relay {} not in pool — deletion not delivered", url),
        }
    }
    crate::log_info!("[NIP-17 delete] wrap {} — deletion delivered to {} relay(s) via tracker", wrap_event_id.to_hex(), delivered);
}

/// Send `deletion` to every inbox relay of `owner`, connecting any not pooled for the call.
async fn publish_to_inbox(client: &Client, owner: &PublicKey, deletion: &Event, verify: bool) {
    let targets = resolve_gift_wrap_targets(client, owner).await;
    let total = targets.resolved.len();
    let mut delivered = 0usize;
    for (url, relay) in &targets.resolved {
        if send_to_one_relay(client, url, relay, deletion, verify).await {
            delivered += 1;
        }
    }
    teardown_gift_wrap_targets(client, &targets).await;
    crate::log_info!("[NIP-17 delete] deletion {} delivered to {}/{} inbox relay(s)", deletion.id.to_hex(), delivered, total);
}

/// Best-effort delete of every cached attachment file for a message.
/// Only unlinks files that canonicalize to a path under Vector's
/// managed download directory — files the user has moved or copied
/// elsewhere are never touched. Symlink and `..` escape attempts are
/// rejected by the canonicalize check.
///
/// Used by both sender-initiated deletes (so the user's own cached
/// copy disappears alongside the network deletion) and cooperative-
/// hide receivers (so the recipient's downloaded copy goes when the
/// sender asks for the message to disappear). Mirrors the
/// "delete-for-everyone" semantics of iMessage/Signal at the file
/// layer, scoped to Vector's own cache.
pub fn delete_cached_attachment_files_pub(attachments: &[crate::types::Attachment]) {
    delete_cached_attachment_files(attachments);
}

/// Filter `attachments` down to those NOT referenced by any OTHER
/// undeleted message in STATE.
///
/// Vector dedupes uploads by SHA-256 hash: re-sending the same file
/// reuses the on-disk cache + the same Blossom URL across multiple
/// messages. Without this filter, deleting one of those messages
/// would unlink the cached file (or DELETE the Blossom blob) even
/// though sibling messages still reference it — the user's other
/// copies would 404 and lose their local preview.
///
/// `excluding_message_id` is the id of the message we're deleting,
/// so it doesn't count as a "reference" to itself when determining
/// whether the attachment is shared.
pub async fn filter_unreferenced_attachments(
    excluding_message_id: &str,
    attachments: Vec<crate::types::Attachment>,
) -> Vec<crate::types::Attachment> {
    if attachments.is_empty() {
        return attachments;
    }
    // STATE pass first, lock released before any DB read (lock-order hygiene).
    let state_referenced: Vec<bool> = {
        let state = crate::state::STATE.lock().await;
        attachments
            .iter()
            .map(|att| {
                // Dedup key: the attachment's SHA-256 (Attachment.id).
                // Empty id can't be matched, so treat as unique.
                let hash = &*att.id;
                if hash.is_empty() {
                    return false;
                }
                state.chats.iter().any(|chat| {
                    chat.iter_compact().any(|m| {
                        let msg_id_hex = m.id_hex();
                        msg_id_hex != excluding_message_id
                            && m.attachments.iter().any(|a| a.id_eq(hash))
                    })
                })
            })
            .collect()
    };
    // DB pass second: STATE windows to recent messages, so a reference in an
    // older message is invisible there — the ledger is the authority. A read
    // failure counts as referenced: deleting less is recoverable, deleting a
    // still-shared file is not.
    attachments
        .into_iter()
        .zip(state_referenced)
        .filter(|(att, in_state)| {
            if *in_state {
                return false;
            }
            let hash = &*att.id;
            if hash.is_empty() {
                return true;
            }
            !crate::db::attachments::hash_referenced_elsewhere(hash, excluding_message_id).unwrap_or(true)
        })
        .map(|(att, _)| att)
        .collect()
}

/// Of these attachments' remote urls, the ones no OTHER event still references
/// — the only ones a delete may scrub from Blossom. One blob backs many
/// messages under smart-forward, and the shared copy must outlive every send
/// but the last. A ledger read failure scrubs nothing, same reasoning as above.
pub fn scrub_safe_urls(attachments: &[crate::types::Attachment], excluding_event_id: &str) -> Vec<String> {
    let urls: Vec<String> = attachments.iter().flat_map(|a| a.all_urls().map(str::to_string)).collect();
    if urls.is_empty() {
        return urls;
    }
    crate::db::attachments::urls_unreferenced_elsewhere(&urls, excluding_event_id).unwrap_or_default()
}

fn delete_cached_attachment_files(attachments: &[crate::types::Attachment]) {
    if attachments.is_empty() {
        return;
    }
    let ram = crate::media::ram::cache();
    for hash in attachments.iter().filter_map(|a| crate::simd::hex::hex_to_bytes_32_checked(&a.id)) {
        crate::media::pending::forget(&hash);
        #[cfg(not(target_arch = "wasm32"))]
        crate::media::store::forget(&hash);
        #[cfg(target_arch = "wasm32")]
        crate::db::spawn_bound(async move { crate::media::store::forget(&hash).await });
        // Last, so a copy finishing meanwhile finds its picture gone and goes too (natively: on
        // wasm the store's forget runs later, and nothing there makes copies).
        ram.remove(&hash);
    }
    #[cfg(target_arch = "wasm32")]
    {
        let dir = crate::db::get_download_dir();
        let paths: Vec<std::path::PathBuf> = attachments
            .iter()
            .map(|a| std::path::PathBuf::from(&*a.path))
            .filter(|p| p.starts_with(&dir) && !p.components().any(|c| c == std::path::Component::ParentDir))
            .collect();
        crate::db::spawn_bound(async move {
            for p in paths {
                crate::webfiles::remove(&p).await;
            }
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    delete_cached_attachment_files_native(attachments);
}

#[cfg(not(target_arch = "wasm32"))]
fn delete_cached_attachment_files_native(attachments: &[crate::types::Attachment]) {
    let roots: Vec<std::path::PathBuf> = crate::db::download_roots().iter().filter_map(|r| r.canonicalize().ok()).collect();
    if roots.is_empty() {
        return;
    }
    for att in attachments {
        if att.path.is_empty() {
            continue;
        }
        let candidate = match std::path::PathBuf::from(&*att.path).canonicalize() {
            Ok(p) => p,
            // File already gone, or path unresolvable — nothing to do.
            Err(_) => continue,
        };
        if !roots.iter().any(|root| candidate.starts_with(root)) {
            // User-managed path (moved/copied out of Vector's cache);
            // never touch.
            continue;
        }
        if crate::db::saved::is_saved(&candidate.to_string_lossy()).unwrap_or(true) {
            continue;
        }
        if let Err(e) = std::fs::remove_file(&candidate) {
            crate::log_warn!(
                "[delete] failed to remove cached attachment {}: {}",
                candidate.display(),
                e
            );
        }
    }
}

/// Direct publish to a single relay handle. Returns true if the relay
/// acknowledged. Returns false if the publish hit a non-rate-limit
/// error, or rate-limit retries were exhausted.
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
/// With `verify`, an ACK schedules a probe (~2s later) asking the relay whether the wraps
/// are really gone. Off for a best-effort author deletion, which a relay that follows the
/// spec acknowledges and ignores.
async fn send_to_one_relay(client: &Client, url: &RelayUrl, relay: &Relay, event: &Event, verify: bool) -> bool {
    /// Max attempts to push past a rate-limit. With 30s between
    /// attempts that's a 10-minute window — generous for any sane
    /// per-IP rate limit. If the relay is still rate-limiting us
    /// after that, something else is wrong and we give up so the
    /// task doesn't loop forever.
    const MAX_RATELIMIT_RETRIES: u32 = 20;
    /// Pause between rate-limit retries.
    const RATELIMIT_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);

    let mut retries = 0u32;
    let wait = crate::relay_request_timeout(std::time::Duration::from_secs(10));
    loop {
        match relay.send_event(event).ok_timeout(wait).authentication_timeout(wait).await {
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
                schedule_verify(client, url, event, verify);
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
                    crate::rt::time::sleep(RATELIMIT_BACKOFF).await;
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
                    schedule_verify(client, url, event, verify);
                } else {
                    crate::log_warn!("[delete] relay {} rejected NIP-09: {}", url, err_str);
                }
                return false;
            }
        }
    }
}

/// The wraps a deletion names, from its `e` tags.
fn deletion_targets(deletion: &Event) -> Vec<EventId> {
    deletion
        .tags
        .iter()
        .filter_map(|tag| {
            let s = tag.as_slice();
            (s.len() >= 2 && s[0] == "e").then(|| EventId::from_hex(&s[1]).ok()).flatten()
        })
        .collect()
}

fn schedule_verify(client: &Client, url: &RelayUrl, deletion: &Event, verify: bool) {
    let wraps = deletion_targets(deletion);
    if !verify || wraps.is_empty() {
        return;
    }
    let (client, url) = (client.clone(), url.clone());
    crate::db::spawn_bound(async move {
        verify_relay_dropped(&client, &url, &wraps).await;
    });
}

/// 2s after a relay ACKs a deletion, ask it whether the wraps are actually gone. Logs a clear
/// "GONE" or "STILL PRESENT" so a relay that acknowledges and keeps them is easy to name.
async fn verify_relay_dropped(client: &Client, url: &RelayUrl, wraps: &[EventId]) {
    crate::rt::time::sleep(std::time::Duration::from_secs(2)).await;
    let Some(relay) = client.relays().await.get(url).cloned() else { return };
    let filter = Filter::new().ids(wraps.iter().copied());
    match relay
        .fetch_events(filter)
        .timeout(std::time::Duration::from_secs(5))
        .policy(ReqExitPolicy::ExitOnEOSE)
        .await
    {
        Ok(events) => {
            let kept = events.len();
            if kept > 0 {
                crate::log_warn!("[delete-verify] relay {} STILL HAS {}/{} wrap(s) after acknowledging their deletion", url, kept, wraps.len());
            } else {
                crate::log_info!("[delete-verify] relay {} confirmed {} wrap(s) GONE", url, wraps.len());
            }
        }
        Err(e) => crate::log_warn!("[delete-verify] relay {} probe failed: {}", url, e),
    }
}

/// Publish the hide request: a kind-5 rumor signed by the user's main key, gift-wrapped to
/// the recipient and to self. Rumor and wrap both expire after 30 days, so relays drop it.
async fn publish_cooperative_hide(
    client: &Client,
    target_rumor_id: &EventId,
    recipient: Option<&PublicKey>,
    original_kind: u16,
) -> Result<(), String> {
    let my_pk = my_public_key().ok_or("Public key not set")?;
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let expiration_ts = now + COOPERATIVE_HIDE_EXPIRY_SECS;

    // Build the kind-5 rumor (signed by our main key via the gift-wrap
    // path's seal step). Reference the inner rumor id with `e`, hint at
    // the original kind via `k` (14 = DM message, 7 = reaction), expire
    // after 30 days. The `k` lets the receiver remove the right thing.
    let rumor = EventBuilder::new(Kind::EventDeletion, "")
        .tag(Tag::event(*target_rumor_id))
        .tag(Tag::custom("k", [original_kind.to_string()]))
        .tag(Tag::expiration(Timestamp::from(expiration_ts)))
        .finalize_unsigned_with_id(my_pk);

    // Wrap and send to the recipient, when there is one, and to self so the user's other
    // devices drop it too. Best-effort.
    let expires = || [Tag::expiration(Timestamp::from(expiration_ts))];
    let to_them = match recipient {
        Some(recipient) => send_gift_wrap(client, recipient, rumor.clone(), expires()).await.is_ok(),
        None => false,
    };
    let to_me = send_gift_wrap(client, &my_pk, rumor, expires()).await.is_ok();
    if !to_them && !to_me {
        return Err("no cooperative-hide delivery went out".to_string());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn pending_received_wrap_deletions() -> Vec<EventId> {
    received_wrap_deletions().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::nip17_keys::{StoredWrapKey, WrapRole};

    fn stored(role: WrapRole) -> StoredWrapKey {
        let k = Keys::generate();
        StoredWrapKey {
            wrap_event_id: EventBuilder::new(Kind::GiftWrap, "").finalize(&k).unwrap().id,
            rumor_id: EventId::from_byte_array([0; 32]),
            recipient_pubkey: Keys::generate().public_key(),
            role,
            secret: k.secret_key().clone(),
            relay_urls: Vec::new(),
        }
    }

    #[test]
    fn a_wrap_deletion_names_its_wraps_and_expires() {
        let wraps = [EventId::from_byte_array([0; 32]), EventId::from_byte_array([7; 32])];
        let ev = wrap_deletion(&wraps).finalize(&Keys::generate()).unwrap();
        assert_eq!(ev.kind, Kind::EventDeletion);
        let tag = |name: &str| ev.tags.iter().filter(|t| t.as_slice()[0] == name).map(|t| t.as_slice()[1].clone()).collect::<Vec<_>>();
        assert_eq!(tag("e"), wraps.iter().map(|w| w.to_hex()).collect::<Vec<_>>());
        assert_eq!(tag("k"), vec!["1059".to_string()]);
        let expires: u64 = tag("expiration")[0].parse().unwrap();
        let now = Timestamp::now().as_secs();
        assert!(expires > now + WRAP_DELETION_EXPIRY_SECS - 60 && expires <= now + WRAP_DELETION_EXPIRY_SECS, "it leaves the relays in time");
    }

    #[test]
    fn our_own_key_deletes_the_copies_addressed_to_us() {
        let (mine, theirs, retry) = (stored(WrapRole::SelfSend), stored(WrapRole::Recipient), stored(WrapRole::Retry));
        let keys = [mine.clone(), theirs, retry];
        let received = EventId::from_byte_array([9; 32]);
        assert_eq!(own_wraps(&keys, Some(received)), vec![mine.wrap_event_id, received], "our self-copy and the wrap that reached us");
        assert_eq!(own_wraps(&keys, Some(mine.wrap_event_id)), vec![mine.wrap_event_id], "named once");
        assert_eq!(own_wraps(&keys, None), vec![mine.wrap_event_id]);
    }

    #[tokio::test]
    async fn an_owner_deletion_is_signed_by_us_and_never_for_someone_else() {
        let _guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let me = Keys::generate();
        crate::state::MY_SECRET_KEY.store_from_keys(&me, &[]);
        crate::state::set_my_public_key(me.public_key());
        let wraps = [EventId::from_byte_array([3; 32])];
        let deletion = owner_deletion(&wraps, &me.public_key()).await.unwrap();
        assert_eq!(deletion.pubkey, me.public_key());
        assert!(deletion.verify().is_ok());
        assert!(owner_deletion(&wraps, &Keys::generate().public_key()).await.is_err(), "a swapped account signs nothing for the last one");
    }
}
