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
use rusqlite::params;

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
