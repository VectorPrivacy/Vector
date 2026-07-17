//! NIP-17 ephemeral wrap-key vault.
//!
//! NIP-59 gift-wraps each DM with a fresh ephemeral keypair whose secret
//! is normally discarded immediately after signing. We retain it so the
//! user can later publish an author-signed NIP-09 deletion against the
//! kind-1059 wrap event — actually removing the message from inbox
//! relays rather than relying on "throw the keys away and hope".
//!
//! Encryption-at-rest is handled by Vector's per-account database
//! envelope: ChaCha20 if the account has a password, plaintext if it
//! doesn't (passwordless accounts are unencrypted by design).

use nostr_sdk::prelude::*;
use rusqlite::{params, OptionalExtension};

/// Role of a stored wrap key. Recorded so the deletion path can label
/// audit logs and so a future feature could selectively retain/purge by
/// role (e.g. "drop self-send keys after N days").
#[repr(i64)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WrapRole {
    /// First-attempt wrap delivered to the recipient.
    Recipient = 0,
    /// Wrap delivered to our own inbox for multi-device recovery.
    SelfSend = 1,
    /// Retry wrap (used when an earlier attempt produced a different wrap
    /// event that may also be sitting on some relay).
    Retry = 2,
}

impl WrapRole {
    fn from_i64(v: i64) -> Self {
        match v {
            1 => Self::SelfSend,
            2 => Self::Retry,
            _ => Self::Recipient,
        }
    }
}

#[derive(Clone, Debug)]
pub struct StoredWrapKey {
    pub wrap_event_id: EventId,
    pub rumor_id: EventId,
    pub recipient_pubkey: PublicKey,
    pub role: WrapRole,
    pub secret: SecretKey,
    /// Relay URLs we attempted at send time. Deletion publishes the
    /// author-signed NIP-09 back to this same set.
    pub relay_urls: Vec<String>,
}

/// Persist a retained ephemeral wrap secret. Idempotent on
/// `wrap_event_id` so retries that land the same wrap won't duplicate.
pub fn store_wrap_key(
    wrap_event_id: &EventId,
    rumor_id: &EventId,
    recipient_pubkey: &PublicKey,
    role: WrapRole,
    secret: &SecretKey,
    relay_urls: &[String],
) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let relays_json = serde_json::to_string(relay_urls)
        .map_err(|e| format!("Failed to encode relay urls: {}", e))?;
    conn.execute(
        "INSERT OR REPLACE INTO nip17_wrap_keys
            (wrap_event_id, rumor_id, recipient_pubkey, role, secret, relay_urls, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            wrap_event_id.to_hex(),
            rumor_id.to_hex(),
            recipient_pubkey.to_hex(),
            role as i64,
            secret.as_secret_bytes(),
            relays_json,
            now,
        ],
    )
    .map_err(|e| format!("Failed to insert wrap key: {}", e))?;
    crate::log_info!(
        "[NIP-17 keys] stored {:?} key for rumor {} (wrap {})",
        role,
        rumor_id.to_hex(),
        wrap_event_id.to_hex()
    );
    Ok(())
}

/// Fetch every retained wrap key (recipient + self + retry) for a given
/// inner rumor id. Used at delete time to construct one NIP-09 per wrap.
pub fn get_wrap_keys_for_rumor(rumor_id: &EventId) -> Result<Vec<StoredWrapKey>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare(
            "SELECT wrap_event_id, rumor_id, recipient_pubkey, role, secret, relay_urls
             FROM nip17_wrap_keys WHERE rumor_id = ?1",
        )
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let rows = stmt
        .query_map(params![rumor_id.to_hex()], |row| {
            let wrap_hex: String = row.get(0)?;
            let rumor_hex: String = row.get(1)?;
            let recipient_hex: String = row.get(2)?;
            let role_i: i64 = row.get(3)?;
            let secret_blob: Vec<u8> = row.get(4)?;
            let relays_json: String = row.get(5)?;
            Ok((wrap_hex, rumor_hex, recipient_hex, role_i, secret_blob, relays_json))
        })
        .map_err(|e| format!("Failed to query wrap keys: {}", e))?;

    let mut out = Vec::new();
    for row_res in rows {
        let (wrap_hex, rumor_hex, recipient_hex, role_i, secret_blob, relays_json) =
            row_res.map_err(|e| format!("Row read error: {}", e))?;
        let wrap_event_id =
            EventId::from_hex(&wrap_hex).map_err(|e| format!("Bad wrap id: {}", e))?;
        let rumor_id_parsed =
            EventId::from_hex(&rumor_hex).map_err(|e| format!("Bad rumor id: {}", e))?;
        let recipient_pubkey =
            PublicKey::from_hex(&recipient_hex).map_err(|e| format!("Bad pubkey: {}", e))?;
        let secret =
            SecretKey::from_slice(&secret_blob).map_err(|e| format!("Bad secret: {}", e))?;
        let relay_urls: Vec<String> = serde_json::from_str(&relays_json)
            .map_err(|e| format!("Bad relay urls: {}", e))?;
        out.push(StoredWrapKey {
            wrap_event_id,
            rumor_id: rumor_id_parsed,
            recipient_pubkey,
            role: WrapRole::from_i64(role_i),
            secret,
            relay_urls,
        });
    }
    Ok(out)
}

/// Cheap existence check: do we hold any retained wrap key for this
/// rumor id? Used by the UI to gate the delete-message control so we
/// don't tease users with a button we can't actually fulfil.
pub fn has_wrap_keys_for_rumor(rumor_id: &EventId) -> Result<bool, String> {
    let conn = super::get_db_connection_guard_static()?;
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM nip17_wrap_keys WHERE rumor_id = ?1)",
        params![rumor_id.to_hex()],
        |row| row.get::<_, bool>(0),
    )
    .map_err(|e| format!("Failed to check wrap keys: {}", e))
}

/// Drop wrap-key rows after the corresponding NIP-09 deletions have
/// been broadcast. Caller passes the wrap event ids it actually deleted
/// so partial-success scenarios don't accidentally drop keys still
/// useful for retry.
pub fn purge_wrap_keys(wrap_event_ids: &[EventId]) -> Result<(), String> {
    if wrap_event_ids.is_empty() {
        return Ok(());
    }
    let conn = super::get_write_connection_guard_static()?;
    let placeholders = std::iter::repeat("?")
        .take(wrap_event_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "DELETE FROM nip17_wrap_keys WHERE wrap_event_id IN ({})",
        placeholders,
    );
    let hex_strings: Vec<String> = wrap_event_ids.iter().map(|id| id.to_hex()).collect();
    let params_dyn: Vec<&dyn rusqlite::ToSql> = hex_strings
        .iter()
        .map(|s| s as &dyn rusqlite::ToSql)
        .collect();
    conn.execute(&sql, params_dyn.as_slice())
        .map_err(|e| format!("Failed to purge wrap keys: {}", e))?;
    Ok(())
}

// ============================================================================
// Retained wrap body — idempotent manual retry
// ============================================================================
//
// The recipient wrap's ephemeral key is retained above for NIP-09 delete. For
// a byte-identical *resend* (so a relay no-ops the duplicate rather than
// storing a second copy) we also retain the built wrap EVENT plus its rumor,
// keyed by the local pending id so Retry can find them from the failed row.
// A gift wrap can't be rebuilt identically (random ephemeral key + NIP-44
// nonce + NIP-59 backdated created_at), so the exact bytes must be kept.
// The body is transient: nulled the instant a relay confirms delivery.

/// Everything needed to republish a failed DM's recipient wrap verbatim.
pub struct ResendPayload {
    /// The exact kind-1059 event to republish (same id → relay dedup).
    pub wrap_event: Event,
    /// The inner rumor, for finalize + the self-send copy on success.
    pub rumor: UnsignedEvent,
    /// The wrap's ephemeral secret (re-associated with the injected wrap;
    /// republishing a pre-signed event never reads it, but the send path's
    /// `BuiltGiftWrap` carries it).
    pub secret: SecretKey,
    pub recipient_pubkey: PublicKey,
    /// Relay set attempted at first send (fallback targets).
    pub relay_urls: Vec<String>,
    pub rumor_id: EventId,
}

/// Attach the republishable body to an existing recipient wrap-key row.
/// Called right after `store_wrap_key` on the first send attempt.
pub fn stash_resend_payload(
    wrap_event_id: &EventId,
    pending_id: &str,
    wrap_event: &Event,
    rumor: &UnsignedEvent,
) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE nip17_wrap_keys SET wrap_json = ?1, rumor_json = ?2, pending_id = ?3
         WHERE wrap_event_id = ?4",
        params![
            wrap_event.as_json(),
            rumor.as_json(),
            pending_id,
            wrap_event_id.to_hex(),
        ],
    )
    .map_err(|e| format!("Failed to stash resend payload: {}", e))?;
    Ok(())
}

/// Load the republishable recipient wrap for a failed message (its id is the
/// local pending id). `None` when nothing retained — the caller then falls
/// back to a fresh send. Only rows with a body (still unconfirmed) match.
pub fn get_resend_payload_by_pending(pending_id: &str) -> Result<Option<ResendPayload>, String> {
    let conn = super::get_db_connection_guard_static()?;
    // role 0 = Recipient — the only wrap a manual retry republishes.
    let row = conn
        .query_row(
            "SELECT rumor_id, recipient_pubkey, secret, relay_urls, wrap_json, rumor_json
             FROM nip17_wrap_keys
             WHERE pending_id = ?1 AND role = 0
               AND wrap_json IS NOT NULL AND rumor_json IS NOT NULL
             LIMIT 1",
            params![pending_id],
            |row| {
                let rumor_hex: String = row.get(0)?;
                let recipient_hex: String = row.get(1)?;
                let secret_blob: Vec<u8> = row.get(2)?;
                let relays_json: String = row.get(3)?;
                let wrap_json: String = row.get(4)?;
                let rumor_json: String = row.get(5)?;
                Ok((rumor_hex, recipient_hex, secret_blob, relays_json, wrap_json, rumor_json))
            },
        )
        .optional()
        .map_err(|e| format!("Failed to query resend payload: {}", e))?;

    let Some((rumor_hex, recipient_hex, secret_blob, relays_json, wrap_json, rumor_json)) = row
    else {
        return Ok(None);
    };
    let rumor_id = EventId::from_hex(&rumor_hex).map_err(|e| format!("Bad rumor id: {}", e))?;
    let recipient_pubkey =
        PublicKey::from_hex(&recipient_hex).map_err(|e| format!("Bad pubkey: {}", e))?;
    let secret = SecretKey::from_slice(&secret_blob).map_err(|e| format!("Bad secret: {}", e))?;
    let relay_urls: Vec<String> =
        serde_json::from_str(&relays_json).map_err(|e| format!("Bad relay urls: {}", e))?;
    let wrap_event = Event::from_json(&wrap_json).map_err(|e| format!("Bad wrap json: {}", e))?;
    let rumor = UnsignedEvent::from_json(&rumor_json).map_err(|e| format!("Bad rumor json: {}", e))?;
    Ok(Some(ResendPayload {
        wrap_event,
        rumor,
        secret,
        recipient_pubkey,
        relay_urls,
        rumor_id,
    }))
}

/// Drop the republishable body once delivery is confirmed (the key row stays
/// for NIP-09 delete). Keyed by rumor id so both the recipient and any retry
/// rows for the message are cleared together.
pub fn clear_resend_payload(rumor_id: &EventId) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE nip17_wrap_keys SET wrap_json = NULL, rumor_json = NULL WHERE rumor_id = ?1",
        params![rumor_id.to_hex()],
    )
    .map_err(|e| format!("Failed to clear resend payload: {}", e))?;
    Ok(())
}

/// Backstop: null retained bodies older than `max_age_secs` so a pile of
/// never-retried reds can't grow unbounded. The key row (NIP-09) survives.
/// Returns how many bodies were reaped.
pub fn prune_stale_resend_payloads(max_age_secs: i64) -> Result<usize, String> {
    let conn = super::get_write_connection_guard_static()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let cutoff = now - max_age_secs;
    let n = conn
        .execute(
            "UPDATE nip17_wrap_keys SET wrap_json = NULL, rumor_json = NULL
             WHERE wrap_json IS NOT NULL AND created_at < ?1",
            params![cutoff],
        )
        .map_err(|e| format!("Failed to prune resend payloads: {}", e))?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(81000);

    fn make_test_npub(n: u32) -> String {
        const BECH32: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let mut payload = vec![b'q'; 58];
        let mut x = n as u64;
        let mut i = 58;
        while x > 0 && i > 0 {
            i -= 1;
            payload[i] = BECH32[(x as usize) % 32];
            x /= 32;
        }
        format!("npub1{}", std::str::from_utf8(&payload).unwrap())
    }

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        let n = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let account = make_test_npub(n);
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard)
    }

    fn sample() -> (Event, UnsignedEvent, Keys, PublicKey) {
        let ephemeral = Keys::generate();
        let sender = Keys::generate();
        let recipient = Keys::generate();
        // Stand-in wrap: any valid signed event — the id is what must survive.
        let wrap = EventBuilder::text_note("wrap").sign_with_keys(&ephemeral).unwrap();
        let rumor = EventBuilder::private_msg_rumor(recipient.public_key(), "hello")
            .build(sender.public_key());
        (wrap, rumor, ephemeral, recipient.public_key())
    }

    #[test]
    fn resend_payload_round_trips_and_clears() {
        let (_tmp, _guard) = init_test_db();
        let (wrap, rumor, ephemeral, recipient) = sample();
        let rumor_id = rumor.id.unwrap();
        let relays = vec!["wss://relay.one".to_string(), "wss://relay.two".to_string()];

        store_wrap_key(&wrap.id, &rumor_id, &recipient, WrapRole::Recipient,
            ephemeral.secret_key(), &relays).unwrap();
        stash_resend_payload(&wrap.id, "pending-42", &wrap, &rumor).unwrap();

        let got = get_resend_payload_by_pending("pending-42").unwrap().expect("payload retained");
        // The whole point: the wrap republishes byte-identical (same outer id).
        assert_eq!(got.wrap_event.id, wrap.id);
        assert_eq!(got.rumor.id, Some(rumor_id));
        assert_eq!(got.rumor_id, rumor_id);
        assert_eq!(got.recipient_pubkey, recipient);
        assert_eq!(got.relay_urls, relays);

        // Confirmed delivery drops the body but keeps the key row for NIP-09.
        clear_resend_payload(&rumor_id).unwrap();
        assert!(get_resend_payload_by_pending("pending-42").unwrap().is_none());
        assert!(has_wrap_keys_for_rumor(&rumor_id).unwrap(), "key row survives NIP-09");
    }

    #[test]
    fn no_payload_for_unknown_pending() {
        let (_tmp, _guard) = init_test_db();
        assert!(get_resend_payload_by_pending("pending-none").unwrap().is_none());
    }

    #[test]
    fn prune_reaps_stale_bodies_but_keeps_keys() {
        let (_tmp, _guard) = init_test_db();
        let (wrap, rumor, ephemeral, recipient) = sample();
        let rumor_id = rumor.id.unwrap();
        let relays = vec!["wss://relay.one".to_string()];
        store_wrap_key(&wrap.id, &rumor_id, &recipient, WrapRole::Recipient,
            ephemeral.secret_key(), &relays).unwrap();
        stash_resend_payload(&wrap.id, "pending-stale", &wrap, &rumor).unwrap();
        assert!(get_resend_payload_by_pending("pending-stale").unwrap().is_some());

        // Negative TTL → cutoff in the future → the fresh row's body is reaped.
        assert_eq!(prune_stale_resend_payloads(-100).unwrap(), 1);
        assert!(get_resend_payload_by_pending("pending-stale").unwrap().is_none());
        assert!(has_wrap_keys_for_rumor(&rumor_id).unwrap(), "key row survives prune");
    }
}
