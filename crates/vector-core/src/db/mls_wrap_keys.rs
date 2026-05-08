//! MLS group-message ephemeral wrap-key vault.
//!
//! Sibling of `db::nip17_keys`. MDK signs each kind-445 wrapper with a
//! freshly-generated ephemeral keypair and discards the secret after
//! signing — Vector patches MDK to expose `create_message_retained`
//! which returns that key, and we persist it here so the original
//! sender can later publish a NIP-09 deletion against the wrapper
//! (the only key that satisfies `event.pubkey == deletion.pubkey`
//! per NIP-09 for that specific wrapper event id).
//!
//! Encryption-at-rest is handled by Vector's per-account database
//! envelope: ChaCha20 if the account has a password, plaintext if it
//! doesn't (passwordless accounts are unencrypted by design).

use nostr_sdk::prelude::*;
use rusqlite::params;

#[derive(Clone, Debug)]
pub struct StoredMlsWrapKey {
    pub wrap_event_id: EventId,
    /// Inner rumor id (what the UI references when the user clicks delete).
    pub message_id: String,
    /// MLS group id (hex). Used for per-group operations and audit.
    pub group_id: String,
    pub secret: SecretKey,
    /// Relay URLs the wrapper was published to. Deletion publishes the
    /// author-signed NIP-09 back to this same set.
    pub relay_urls: Vec<String>,
}

/// Persist a retained ephemeral wrap secret. Idempotent on
/// `wrap_event_id`.
pub fn store_wrap_key(
    wrap_event_id: &EventId,
    message_id: &str,
    group_id: &str,
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
        "INSERT OR REPLACE INTO mls_wrap_keys
            (wrap_event_id, message_id, group_id, secret, relay_urls, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            wrap_event_id.to_hex(),
            message_id,
            group_id,
            secret.as_secret_bytes(),
            relays_json,
            now,
        ],
    )
    .map_err(|e| format!("Failed to insert MLS wrap key: {}", e))?;
    Ok(())
}

/// Fetch every retained wrap key for a given inner message id. Used at
/// delete time to construct one NIP-09 per wrapper. There may be
/// multiple wrappers per message id if retries spawned new wraps.
pub fn get_wrap_keys_for_message(message_id: &str) -> Result<Vec<StoredMlsWrapKey>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare(
            "SELECT wrap_event_id, message_id, group_id, secret, relay_urls
             FROM mls_wrap_keys WHERE message_id = ?1",
        )
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let rows = stmt
        .query_map(params![message_id], |row| {
            let wrap_hex: String = row.get(0)?;
            let msg_id: String = row.get(1)?;
            let group_id: String = row.get(2)?;
            let secret_blob: Vec<u8> = row.get(3)?;
            let relays_json: String = row.get(4)?;
            Ok((wrap_hex, msg_id, group_id, secret_blob, relays_json))
        })
        .map_err(|e| format!("Failed to query MLS wrap keys: {}", e))?;

    let mut out = Vec::new();
    for row_res in rows {
        let (wrap_hex, msg_id, group_id, secret_blob, relays_json) =
            row_res.map_err(|e| format!("Row read error: {}", e))?;
        let wrap_event_id =
            EventId::from_hex(&wrap_hex).map_err(|e| format!("Bad wrap id: {}", e))?;
        let secret =
            SecretKey::from_slice(&secret_blob).map_err(|e| format!("Bad secret: {}", e))?;
        let relay_urls: Vec<String> = serde_json::from_str(&relays_json)
            .map_err(|e| format!("Bad relay urls: {}", e))?;
        out.push(StoredMlsWrapKey {
            wrap_event_id,
            message_id: msg_id,
            group_id,
            secret,
            relay_urls,
        });
    }
    Ok(out)
}

/// Cheap existence check: do we hold any retained wrap key for this
/// message id? UI uses this to gate the delete-message control.
pub fn has_wrap_keys_for_message(message_id: &str) -> Result<bool, String> {
    let conn = super::get_db_connection_guard_static()?;
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM mls_wrap_keys WHERE message_id = ?1)",
        params![message_id],
        |row| row.get::<_, bool>(0),
    )
    .map_err(|e| format!("Failed to check MLS wrap keys: {}", e))
}

/// Drop wrap-key rows after the corresponding NIP-09 deletions have
/// been broadcast.
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
        "DELETE FROM mls_wrap_keys WHERE wrap_event_id IN ({})",
        placeholders,
    );
    let hex_strings: Vec<String> = wrap_event_ids.iter().map(|id| id.to_hex()).collect();
    let params_dyn: Vec<&dyn rusqlite::ToSql> = hex_strings
        .iter()
        .map(|s| s as &dyn rusqlite::ToSql)
        .collect();
    conn.execute(&sql, params_dyn.as_slice())
        .map_err(|e| format!("Failed to purge MLS wrap keys: {}", e))?;
    Ok(())
}
