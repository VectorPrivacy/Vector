//! Self-Destruct Timer — per-chat NIP-40 message expiry ("disappearing
//! messages"). The per-chat lifespan is a DURATION stored in the account
//! settings KV; each outgoing DM in that chat is stamped with an absolute
//! NIP-40 expiry so relays drop the gift-wrap and every compliant client
//! purges its local copy on schedule. Purge is local-only per client — the
//! expiry tag travels with the message, so no delete broadcast is needed.

use std::sync::atomic::{AtomicBool, Ordering};

const KEY_PREFIX: &str = "self_destruct:";

/// Longest the sweeper sleeps when nothing is near expiry — bounds how quickly
/// a newly-arrived self-destruct message is first noticed (it's then scheduled
/// precisely). Kept below the shortest offered timer (10s) so even a fresh
/// short-fused message is always seen before it expires. Near an expiry the
/// loop sleeps exactly until it, down to a 1s floor.
const SWEEP_MAX_SECS: u64 = 8;

/// Configured self-destruct DURATION in seconds for a chat, or `None` when the
/// chat keeps messages permanently. Stored per-account, so it naturally follows
/// the active account's DB pool.
pub fn chat_duration_secs(chat_id: &str) -> Option<u64> {
    crate::db::settings::get_sql_setting(format!("{KEY_PREFIX}{chat_id}"))
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|d| *d > 0)
}

/// Set the self-destruct duration for a chat. `None` (or 0) clears it back to
/// permanent by removing the key.
pub fn set_chat_duration_secs(chat_id: &str, secs: Option<u64>) -> Result<(), String> {
    let key = format!("{KEY_PREFIX}{chat_id}");
    match secs {
        Some(d) if d > 0 => crate::db::settings::set_sql_setting(key, d.to_string()),
        _ => crate::db::settings::remove_setting(&key),
    }
}

/// Resolve the absolute NIP-40 expiry (unix seconds) to stamp on a NEW message
/// sent to `chat_id`, honoring the chat's configured lifespan. `None` when the
/// chat is permanent.
pub fn resolve_send_expiry(chat_id: &str) -> Option<u64> {
    let duration = chat_duration_secs(chat_id)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(now + duration)
}

/// Reset the in-flight flag whatever exit `sweep_expired` takes.
struct SweepGuard;
impl Drop for SweepGuard {
    fn drop(&mut self) {
        SWEEP_RUNNING.store(false, Ordering::Release);
    }
}
static SWEEP_RUNNING: AtomicBool = AtomicBool::new(false);

/// Purge every message whose NIP-40 expiry has passed: drop it from STATE and
/// the DB, remove cached attachment files no sibling still needs, and — for OUR
/// OWN file messages — issue a Blossom blob delete (blobs carry no self-expiry).
/// Emits `message_removed` with reason "self-destruct" per purged row so the UI
/// can derez it.
///
/// Local-only: every client honors the same NIP-40 tag independently, so no
/// delete is broadcast. Safe to call repeatedly (a ticker + a boot catch-up).
pub async fn sweep_expired() -> Option<u64> {
    if SWEEP_RUNNING.swap(true, Ordering::AcqRel) {
        return None; // a sweep is already in flight
    }
    let _guard = SweepGuard;

    // Snapshot the session so a mid-sweep account swap can't purge account A's
    // rows against account B's DB (see SessionGuard contract).
    let session = crate::state::SessionGuard::capture();

    let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(_) => return None,
    };

    // Pass 1 — scan under the lock: collect the expired, and track the soonest
    // still-pending expiry so the loop can time the next sweep to the second.
    let mut soonest: Option<u64> = None;
    let expired_ids: Vec<String> = {
        let state = crate::state::STATE.lock().await;
        let mut ids = Vec::new();
        for chat in &state.chats {
            for msg in chat.messages.iter() {
                let exp = msg.expiration_secs;
                if exp == 0 {
                    continue;
                }
                let exp = exp as u64;
                if exp <= now {
                    ids.push(msg.id_hex());
                } else {
                    soonest = Some(soonest.map_or(exp, |s| s.min(exp)));
                }
            }
        }
        ids
    };
    if !session.is_valid() {
        return None;
    }
    if expired_ids.is_empty() {
        return soonest;
    }

    // Pass 2 — purge each. Re-lock per id so the sweep never holds STATE across
    // an await (DB delete, blob delete).
    for id in expired_ids {
        purge_one(&id, None).await;
    }

    soonest
}

/// Purge one expired message everywhere it lives: STATE (emitting the derez
/// event when it was resident), cached attachment files, our own Blossom
/// blobs, and the DB row. `db_fallback` supplies (attachments, mine) for rows
/// that never hydrated into STATE (a DB load caught them first); `None` keeps
/// the sweep's contract of skipping ids something else already removed.
pub(crate) async fn purge_one(id: &str, db_fallback: Option<(Vec<crate::types::Attachment>, bool)>) {
    let removed = {
        let mut state = crate::state::STATE.lock().await;
        state.remove_message(id)
    };
    let (attachments, mine, resident_chat) = match removed {
        Some((chat_id, msg)) => (msg.attachments, msg.mine, Some(chat_id)),
        None => match db_fallback {
            Some((attachments, mine)) => (attachments, mine, None),
            None => return, // already gone (client-side derez or a prior sweep)
        },
    };

    if !attachments.is_empty() {
        // Refcount filter: keep files/blobs a sibling message still points at.
        let unique = crate::deletion::filter_unreferenced_attachments(id, attachments).await;
        crate::deletion::delete_cached_attachment_files_pub(&unique);

        // Our own, now-unreferenced blob → wipe it network-side too.
        if mine {
            let urls: Vec<String> = unique
                .iter()
                .flat_map(|a| a.all_urls().map(str::to_string))
                .collect();
            if !urls.is_empty() {
                if crate::state::nostr_client().is_some() {
                    if let Ok(signer) = crate::signer::active_signer() {
                        crate::blossom::delete_blobs_best_effort(signer, urls);
                    }
                }
            }
        }
    }

    let _ = crate::db::events::delete_event(id).await;

    // Only a STATE-resident message can be on screen — DB-only rows were
    // caught before rendering and vanish silently.
    if let Some(chat_id) = resident_chat {
        crate::traits::emit_event(
            "message_removed",
            &serde_json::json!({ "id": id, "chat_id": chat_id, "reason": "self-destruct" }),
        );
    }
}

/// Drop already-expired messages from a freshly-loaded DB window, purging
/// their remnants in the background. Rows that expired while the app was
/// closed (or while their chat was out of STATE) must NEVER render — not
/// even for the frame between hydration and the next sweep tick.
pub fn strip_expired(messages: &mut Vec<crate::types::Message>) {
    let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(_) => return,
    };
    let mut expired: Vec<(String, Vec<crate::types::Attachment>, bool)> = Vec::new();
    messages.retain(|m| match m.expiration {
        Some(exp) if exp <= now => {
            expired.push((m.id.clone(), m.attachments.clone(), m.mine));
            false
        }
        _ => true,
    });
    if expired.is_empty() {
        return;
    }
    let session = crate::state::SessionGuard::capture();
    tokio::spawn(async move {
        for (id, attachments, mine) in expired {
            if !session.is_valid() {
                return;
            }
            purge_one(&id, Some((attachments, mine))).await;
        }
    });
}

/// Time until the next sweep: sleep exactly until the soonest pending expiry
/// (down to a 1s floor) so the final stretch purges in real time, but never
/// longer than SWEEP_MAX_SECS so a newly-arrived message is noticed promptly.
fn next_sweep_delay(soonest: Option<u64>) -> std::time::Duration {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let secs = match soonest {
        Some(exp) => exp.saturating_sub(now).clamp(1, SWEEP_MAX_SECS),
        None => SWEEP_MAX_SECS,
    };
    std::time::Duration::from_secs(secs)
}

/// The self-destruct sweep loop: purge, then sleep until the next expiry is due
/// (adaptive — 1s-tight near a deadline, up to SWEEP_MAX_SECS when idle). The
/// immediate first pass catches anything that expired while offline. Hosts with
/// their own async runtime (e.g. Tauri) should spawn this directly.
pub async fn run_sweeper_loop() {
    loop {
        let soonest = sweep_expired().await;
        tokio::time::sleep(next_sweep_delay(soonest)).await;
    }
}

/// Convenience for tokio-native hosts (CLI/SDK): spawn `run_sweeper_loop` once.
/// Idempotent — a second call is a no-op.
pub fn start_sweeper() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    tokio::spawn(run_sweeper_loop());
}
