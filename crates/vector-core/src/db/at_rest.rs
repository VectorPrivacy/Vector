//! Local Encryption at-rest sweep: enable, disable and rekey every encrypted
//! column inside one transaction, so a crash leaves the store on one key.
//!
//! Adding an encrypted column is a two-place change: name it in
//! `SWEEP` (enable finds plaintext by that list alone) and add its writer
//! to `every_encrypting_writer_is_covered_by_the_at_rest_sweep`. Rekey and disable
//! also run `sweep_residue_in_tx`, so a missed column still moves with the key.

use zeroize::Zeroize;
use crate::state::set_encryption_enabled;
use crate::stored_event::event_kind;

/// Known plaintext for the NIP-55 PIN verification canary. Encrypted under the
/// derived key at setup (and when Local Encryption is toggled on) and
/// re-decrypted at boot to reject a wrong PIN (a keyless account has no pkey
/// whose failed decrypt would otherwise do this).
pub const NIP55_PIN_CANARY: &str = "vector-nip55-pin-ok";

fn encrypt_with_key(input: &str, key: &[u8; 32]) -> String {
    crate::crypto::encrypt_with_key(input, key).expect("Encryption should not fail")
}

fn decrypt_with_key(ciphertext: &str, key: &[u8; 32]) -> Result<String, ()> {
    crate::crypto::decrypt_with_key(ciphertext, key).map_err(|_| ())
}

/// Payload of the `encryption_migration_progress` event.
#[derive(serde::Serialize, Clone)]
pub struct MigrationProgress {
    pub total: usize,
    pub completed: usize,
    pub phase: String,
}

// ============================================================================
// Transactional Migrations (Crash-Safe)
// ============================================================================

/// Disable encryption inside a single SQLite transaction.
///
/// All mutations (event decryption, seed/pkey/PIVX decryption, flag updates)
/// are wrapped in one transaction on the write connection. If anything fails
/// or the app crashes before COMMIT, the database stays fully encrypted.
/// This prevents the mixed plaintext/ciphertext state (audit C3/C4).
///
/// Memory-efficient: collects only event IDs upfront, processes one at a time.
pub fn disable(
    key: &[u8; 32],
    progress: &dyn Fn(MigrationProgress),
) -> Result<(), String> {
    // Held from before the transaction until the vault and the flag show the result, so no
    // write sealed under the outgoing key can land after it.
    let _change = crate::crypto::gate::key_change()?;
    if !crate::db::writes_to_live_account() {
        return Err("The account changed before encryption could be turned off".to_string());
    }
    let mut conn = crate::db::get_write_connection_guard_static()?;
    let tx = conn.transaction()
        .map_err(|e| format!("Failed to begin transaction: {}", e))?;

    // Set migration state inside the transaction — rolls back with everything else on crash
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('migration_state', 'decrypting')",
        [],
    ).map_err(|e| format!("Failed to set migration_state: {}", e))?;

    prove_key_in_tx(&tx, key, false)?;

    // 1. Collect encrypted event IDs (memory-efficient: just ID strings)
    let all_ids: Vec<String> = {
        let mut stmt = tx.prepare(
            r#"SELECT id FROM events
               WHERE kind IN (?1, ?2, ?3, ?4)
               AND length(content) >= 56
               AND content NOT GLOB '*[^0-9a-f]*'"#,
        ).map_err(|e| format!("Failed to prepare ID query: {}", e))?;

        let rows = stmt.query_map(
            rusqlite::params![
                event_kind::CHAT_MESSAGE as i32,
                event_kind::PRIVATE_DIRECT_MESSAGE as i32,
                event_kind::MESSAGE_EDIT as i32,
                event_kind::FILE_ATTACHMENT as i32,
            ],
            |row| row.get::<_, String>(0),
        ).map_err(|e| format!("Failed to query IDs: {}", e))?;

        rows.filter_map(|r| r.ok()).collect()
    };

    let total = all_ids.len();
    let mut completed = 0;
    let mut last_emitted_percent: i32 = -1;
    let mut skipped = 0;

    // 2. Decrypt each event within the transaction
    for id in &all_ids {
        let content: Option<String> = tx.query_row(
            "SELECT content FROM events WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        ).ok();

        if let Some(content) = content {
            match decrypt_with_key(&content, key) {
                Ok(plaintext) => {
                    tx.execute(
                        "UPDATE events SET content = ?1 WHERE id = ?2",
                        rusqlite::params![plaintext, id],
                    ).map_err(|e| format!("Failed to update event: {}", e))?;
                }
                Err(_) => {
                    // Content looks encrypted (hex) but isn't — skip it
                    crate::log_info!("[Encryption] Skipping event {} - looks encrypted but isn't", id);
                    skipped += 1;
                }
            }
        }

        completed += 1;
        let current_percent = if total > 0 {
            ((completed as f64 / total as f64) * 100.0) as i32
        } else {
            100
        };

        if current_percent >= last_emitted_percent + 5 {
            last_emitted_percent = current_percent;
            progress(MigrationProgress {
                total,
                completed,
                phase: "decrypting".to_string(),
            });
        }
    }

    // 3. Decrypt settings and PIVX keys within the same transaction
    progress(MigrationProgress {
        total,
        completed,
        phase: "finalizing".to_string(),
    });

    decrypt_setting_in_tx(&tx, "seed", key, |v| v.contains(' '))?;
    decrypt_setting_in_tx(&tx, "pkey", key, |v| v.starts_with("nsec"))?;
    decrypt_setting_in_tx(&tx, "bunker_url", key, |v| v.starts_with("bunker://"))?;
    decrypt_pivx_in_tx(&tx, key)?;
    decrypt_columns_in_tx(&tx, key)?;
    sweep_residue_in_tx(&tx, key, None)?;

    // 4. Verify plaintext state within the transaction (before committing)
    verify_plaintext_state_in_tx(&tx)?;

    // 5. Update flags within the transaction
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('encryption_enabled', 'false')",
        [],
    ).map_err(|e| format!("Failed to update encryption_enabled: {}", e))?;
    super::settings::write_kdf_in_tx(&tx, None)?;

    // Plaintext account needs no wrong-PIN detector and no unlock method —
    // drop both in-tx so no reader can infer a mode that no longer exists.
    tx.execute(
        "DELETE FROM settings WHERE key = 'nip55_pin_check'",
        [],
    ).map_err(|e| format!("Failed to clear pin canary: {}", e))?;
    tx.execute(
        "DELETE FROM settings WHERE key = 'security_type'",
        [],
    ).map_err(|e| format!("Failed to clear security_type: {}", e))?;

    // The biometric wrap holds the PRE-migration derived key — drop it in the
    // SAME transaction so no crash window can leave a stale wrap behind.
    tx.execute(
        "DELETE FROM settings WHERE key = 'biometric_wrapped_key'",
        [],
    ).map_err(|e| format!("Failed to clear biometric wrap: {}", e))?;

    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('migration_state', '')",
        [],
    ).map_err(|e| format!("Failed to clear migration_state: {}", e))?;

    // 6. COMMIT — the atomic point. Everything succeeds or nothing does.
    tx.commit().map_err(|e| format!("Failed to commit disable transaction: {}", e))?;

    set_encryption_enabled(false);
    crate::state::ENCRYPTION_KEY.clear(&[&crate::state::MY_SECRET_KEY]);
    crate::crypto::forget_previous_key();

    if skipped > 0 {
        crate::log_info!("[Encryption] Skipped {} false-positive hex events", skipped);
    }
    crate::log_info!("[Encryption] Disable complete: decrypted {} events", total - skipped);

    Ok(())
}

/// Enable encryption inside a single SQLite transaction.
///
/// All mutations (event encryption, seed/pkey/PIVX encryption, flag updates)
/// are wrapped in one transaction on the write connection. If anything fails
/// or the app crashes before COMMIT, the database stays fully plaintext.
/// This prevents the mixed plaintext/ciphertext state (audit C3/C4).
///
/// Memory-efficient: collects only event IDs upfront, processes one at a time.
pub fn enable(
    key: &[u8; 32],
    kdf: &crate::crypto::Kdf,
    security_type: &str,
    biometric_wrap: Option<&str>,
    nip55_canary: Option<&str>,
    progress: &dyn Fn(MigrationProgress),
) -> Result<(), String> {
    if kdf.is_legacy() {
        return Err("A new key needs its own salt".to_string());
    }
    // Held from before the transaction until the vault and the flag show the result, so no
    // write sealed under the outgoing key can land after it.
    let _change = crate::crypto::gate::key_change()?;
    if !crate::db::writes_to_live_account() {
        return Err("The account changed before encryption could be turned on".to_string());
    }
    let mut conn = crate::db::get_write_connection_guard_static()?;
    let tx = conn.transaction()
        .map_err(|e| format!("Failed to begin transaction: {}", e))?;

    // Set migration state inside the transaction — rolls back with everything else on crash
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('migration_state', 'encrypting')",
        [],
    ).map_err(|e| format!("Failed to set migration_state: {}", e))?;

    // 1. Collect plaintext event IDs (memory-efficient: just ID strings)
    let all_ids: Vec<String> = {
        let mut stmt = tx.prepare(
            r#"SELECT id FROM events
               WHERE kind IN (?1, ?2, ?3, ?4) AND content <> ''
               AND (length(content) < 56 OR content GLOB '*[^0-9a-f]*')"#,
        ).map_err(|e| format!("Failed to prepare ID query: {}", e))?;

        let rows = stmt.query_map(
            rusqlite::params![
                event_kind::CHAT_MESSAGE as i32,
                event_kind::PRIVATE_DIRECT_MESSAGE as i32,
                event_kind::MESSAGE_EDIT as i32,
                event_kind::FILE_ATTACHMENT as i32,
            ],
            |row| row.get::<_, String>(0),
        ).map_err(|e| format!("Failed to query IDs: {}", e))?;

        rows.filter_map(|r| r.ok()).collect()
    };

    let total = all_ids.len();
    let mut completed = 0;
    let mut last_emitted_percent: i32 = -1;

    // 2. Encrypt each event within the transaction
    for id in &all_ids {
        let content: Option<String> = tx.query_row(
            "SELECT content FROM events WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        ).ok();

        if let Some(content) = content {
            let encrypted = encrypt_with_key(&content, key);
            tx.execute(
                "UPDATE events SET content = ?1 WHERE id = ?2",
                rusqlite::params![encrypted, id],
            ).map_err(|e| format!("Failed to update event: {}", e))?;
        }

        completed += 1;
        let current_percent = if total > 0 {
            ((completed as f64 / total as f64) * 100.0) as i32
        } else {
            100
        };

        if current_percent >= last_emitted_percent + 5 {
            last_emitted_percent = current_percent;
            progress(MigrationProgress {
                total,
                completed,
                phase: "encrypting".to_string(),
            });
        }
    }

    // 3. Encrypt settings and PIVX keys within the same transaction
    progress(MigrationProgress {
        total,
        completed,
        phase: "finalizing".to_string(),
    });

    // The loop above picks plaintext by its look; this catches text that only looks sealed.
    seal_plain_messages_in_tx(&tx, key)?;
    encrypt_setting_in_tx(&tx, "seed", key, |v| v.contains(' '))?;
    encrypt_setting_in_tx(&tx, "pkey", key, |v| v.starts_with("nsec"))?;
    encrypt_setting_in_tx(&tx, "bunker_url", key, |v| v.starts_with("bunker://"))?;
    encrypt_pivx_in_tx(&tx, key)?;
    encrypt_columns_in_tx(&tx, key)?;

    // 4. Verify encrypted state within the transaction (before committing)
    verify_encrypted_state_in_tx(&tx, key)?;

    // 5. Update flags within the transaction
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('encryption_enabled', 'true')",
        [],
    ).map_err(|e| format!("Failed to update encryption_enabled: {}", e))?;
    super::settings::write_kdf_in_tx(&tx, kdf.descriptor().as_deref())?;

    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('security_type', ?1)",
        rusqlite::params![security_type],
    ).map_err(|e| format!("Failed to update security_type: {}", e))?;

    // Keyless-account canary: same atomicity rule as the wrap — the only
    // wrong-PIN detector must land with the flags that make it load-bearing.
    if let Some(c) = nip55_canary {
        tx.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('nip55_pin_check', ?1)",
            rusqlite::params![c],
        ).map_err(|e| format!("Failed to set pin canary: {}", e))?;
    }

    // Biometric-only enable carries its wrap INTO this transaction — the row
    // is the account's sole credential, so it must land atomically with the
    // flags that lock the store to it. Otherwise drop any stale wrap (it
    // holds a pre-migration key) in the same transaction.
    match biometric_wrap {
        Some(w) => {
            tx.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES ('biometric_wrapped_key', ?1)",
                rusqlite::params![w],
            ).map_err(|e| format!("Failed to set biometric wrap: {}", e))?;
        }
        None => {
            tx.execute(
                "DELETE FROM settings WHERE key = 'biometric_wrapped_key'",
                [],
            ).map_err(|e| format!("Failed to clear biometric wrap: {}", e))?;
        }
    }

    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('migration_state', '')",
        [],
    ).map_err(|e| format!("Failed to clear migration_state: {}", e))?;

    // 6. COMMIT — the atomic point. Everything succeeds or nothing does.
    tx.commit().map_err(|e| format!("Failed to commit enable transaction: {}", e))?;

    crate::state::ENCRYPTION_KEY.set(*key, &[&crate::state::MY_SECRET_KEY]);
    set_encryption_enabled(true);

    crate::log_info!("[Encryption] Enable complete: encrypted {} events", total);

    Ok(())
}

// ============================================================================
// In-Transaction Helpers
// ============================================================================

/// Decrypt a setting value within a transaction.
fn decrypt_setting_in_tx(
    tx: &rusqlite::Transaction,
    key_name: &str,
    key: &[u8; 32],
    is_plaintext: fn(&str) -> bool,
) -> Result<(), String> {
    let val: Option<String> = tx.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params![key_name],
        |row| row.get(0),
    ).ok();

    if let Some(current) = val {
        if is_plaintext(&current) {
            return Ok(()); // Already plaintext, nothing to decrypt
        }

        let decrypted = decrypt_with_key(&current, key)
            .map_err(|_| format!("Failed to decrypt setting '{}'", key_name))?;

        tx.execute(
            "UPDATE settings SET value = ?1 WHERE key = ?2",
            rusqlite::params![decrypted, key_name],
        ).map_err(|e| format!("Failed to update setting '{}': {}", key_name, e))?;
    }

    Ok(())
}

/// Encrypt a setting value within a transaction.
/// Only encrypts if the value is currently plaintext.
fn encrypt_setting_in_tx(
    tx: &rusqlite::Transaction,
    key_name: &str,
    key: &[u8; 32],
    is_plaintext: fn(&str) -> bool,
) -> Result<(), String> {
    let val: Option<String> = tx.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params![key_name],
        |row| row.get(0),
    ).ok();

    if let Some(current) = val {
        if is_plaintext(&current) {
            let encrypted = encrypt_with_key(&current, key);
            tx.execute(
                "UPDATE settings SET value = ?1 WHERE key = ?2",
                rusqlite::params![encrypted, key_name],
            ).map_err(|e| format!("Failed to update setting '{}': {}", key_name, e))?;
        }
    }

    Ok(())
}

/// Decrypt all PIVX promo private keys within a transaction.
fn decrypt_pivx_in_tx(
    tx: &rusqlite::Transaction,
    key: &[u8; 32],
) -> Result<(), String> {
    let keys: Vec<(i64, String)> = {
        let mut stmt = tx.prepare("SELECT id, privkey_encrypted FROM pivx_promos")
            .map_err(|e| format!("Failed to prepare PIVX query: {}", e))?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
            .map_err(|e| format!("Failed to query PIVX promos: {}", e))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    for (id, key_val) in keys {
        // Only decrypt if longer than a raw key (64 chars) — meaning it's encrypted
        if key_val.len() > 64 {
            let decrypted = decrypt_with_key(&key_val, key)
                .map_err(|_| format!("Failed to decrypt PIVX key {}", id))?;
            tx.execute(
                "UPDATE pivx_promos SET privkey_encrypted = ?1 WHERE id = ?2",
                rusqlite::params![decrypted, id],
            ).map_err(|e| format!("Failed to update PIVX key: {}", e))?;
        }
    }

    Ok(())
}

/// Encrypt all PIVX promo private keys within a transaction.
fn encrypt_pivx_in_tx(
    tx: &rusqlite::Transaction,
    key: &[u8; 32],
) -> Result<(), String> {
    let keys: Vec<(i64, String)> = {
        let mut stmt = tx.prepare("SELECT id, privkey_encrypted FROM pivx_promos")
            .map_err(|e| format!("Failed to prepare PIVX query: {}", e))?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
            .map_err(|e| format!("Failed to query PIVX promos: {}", e))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    for (id, key_val) in keys {
        // Raw PIVX keys are exactly 64 hex chars; encrypted output is always longer
        if key_val.len() <= 64 {
            let encrypted = encrypt_with_key(&key_val, key);
            tx.execute(
                "UPDATE pivx_promos SET privkey_encrypted = ?1 WHERE id = ?2",
                rusqlite::params![encrypted, id],
            ).map_err(|e| format!("Failed to update PIVX key: {}", e))?;
        }
    }

    Ok(())
}

// ============================================================================
// Community (Concord) at-rest migration
// ============================================================================
//
// The community tables hold their own secrets + identifying metadata, wrapped the same way as the
// pkey/event-content/PIVX fields. One direction-agnostic engine: `dec` decrypts with the given key
// first, then `enc` encrypts with the given key, so enable = enc only, disable = dec only, rekey =
// dec(old) then enc(new).
//
// The discriminator in both directions is the AEAD tag: "already encrypted under `k`" iff the value
// opens under `k`. A hex/length heuristic would mistake bare-hex plaintext (invite tokens, creator
// pubkeys) for ciphertext; the tag can't be fooled, and a re-run never double-wraps.

fn xform_text(v: &str, enc: Option<&[u8; 32]>, dec: Option<&[u8; 32]>) -> Result<String, String> {
    // Empty means unset in every sealed column, and queries test it with `<> ''`.
    if v.is_empty() {
        return Ok(String::new());
    }
    let mut s = zeroize::Zeroizing::new(v.to_string());
    if let Some(k) = dec {
        if let Ok(p) = decrypt_with_key(&s, k) {
            s = zeroize::Zeroizing::new(p);
        }
    }
    if let Some(k) = enc {
        if decrypt_with_key(&s, k).map(zeroize::Zeroizing::new).is_err() {
            return Ok(encrypt_with_key(&s, k));
        }
    }
    Ok(s.to_string())
}

fn xform_blob(v: &[u8], enc: Option<&[u8; 32]>, dec: Option<&[u8; 32]>) -> Result<Vec<u8>, String> {
    let mut b = zeroize::Zeroizing::new(v.to_vec());
    if let Some(k) = dec {
        if let Ok(p) = crate::crypto::decrypt_blob_with_key(&b, k) {
            b = zeroize::Zeroizing::new(p);
        }
    }
    if let Some(k) = enc {
        if crate::crypto::decrypt_blob_with_key(&b, k).map(zeroize::Zeroizing::new).is_err() {
            return crate::crypto::encrypt_blob_with_key(&b, k);
        }
    }
    Ok(b.to_vec())
}

#[cfg(test)]
mod xform_tests {
    use super::*;
    const K: [u8; 32] = [7u8; 32];
    const K2: [u8; 32] = [9u8; 32];

    #[test]
    fn bare_hex_plaintext_is_encrypted_not_skipped() {
        // C1 regression: a 64-char-hex plaintext (invite token / creator pubkey) must be wrapped,
        // never mistaken for ciphertext.
        let token = "de".repeat(32); // 64 lowercase hex chars
        let wrapped = xform_text(&token, Some(&K), None).unwrap();
        assert_ne!(wrapped, token, "bare-hex plaintext must be encrypted");
        assert_eq!(decrypt_with_key(&wrapped, &K).unwrap(), token);
    }

    #[test]
    fn encrypt_is_idempotent() {
        let token = "de".repeat(32);
        let once = xform_text(&token, Some(&K), None).unwrap();
        let twice = xform_text(&once, Some(&K), None).unwrap();
        assert_eq!(once, twice, "re-running encrypt must not double-wrap");
        assert_eq!(decrypt_with_key(&twice, &K).unwrap(), token);
    }

    #[test]
    fn disable_then_reenable_roundtrips_bare_hex() {
        let token = "ab".repeat(32);
        let enc = xform_text(&token, Some(&K), None).unwrap();
        let dec = xform_text(&enc, None, Some(&K)).unwrap();
        assert_eq!(dec, token, "disable decrypts back to plaintext");
        let re = xform_text(&dec, Some(&K), None).unwrap();
        assert_eq!(decrypt_with_key(&re, &K).unwrap(), token, "re-enable wraps the bare-hex again");
    }

    #[test]
    fn rekey_rewraps_bare_hex_under_new_key() {
        let token = "cd".repeat(32);
        let old = xform_text(&token, Some(&K), None).unwrap();
        let new = xform_text(&old, Some(&K2), Some(&K)).unwrap(); // dec old, enc new
        assert!(decrypt_with_key(&new, &K).is_err(), "no longer under old key");
        assert_eq!(decrypt_with_key(&new, &K2).unwrap(), token, "now under new key");
    }

    #[test]
    fn nonhex_plaintext_roundtrips() {
        assert_eq!(xform_text("", Some(&K), None).unwrap(), "", "empty stays empty");
        for v in ["general", "{\"roles\":[]}", "wss://relay.example"] {
            let enc = xform_text(v, Some(&K), None).unwrap();
            assert_eq!(decrypt_with_key(&enc, &K).unwrap(), v);
            assert_eq!(xform_text(&enc, None, Some(&K)).unwrap(), v);
        }
    }

    /// The Concord tables exactly as the sweep addresses them (columns it does
    /// not touch omitted).
    fn migrate_test_schema(conn: &rusqlite::Connection) {
        conn.execute_batch(
            "CREATE TABLE communities (community_id TEXT, server_root_key BLOB, name TEXT, relays TEXT, description TEXT, icon TEXT, banner TEXT, banlist TEXT, banlist_marks TEXT, owner_attestation TEXT, roles TEXT, invite_registry TEXT, owner_pubkey TEXT, owner_salt TEXT, meta_extra TEXT, control_pk TEXT, control_root BLOB, migration_pointer TEXT);
             CREATE TABLE community_channels (channel_id TEXT, channel_key BLOB, name TEXT, meta_extra TEXT);
             CREATE TABLE community_epoch_keys (key BLOB);
             CREATE TABLE community_message_keys (outer_event_id TEXT, ephemeral_secret BLOB, relays TEXT);
             CREATE TABLE pending_channel_keys (id INTEGER PRIMARY KEY AUTOINCREMENT, channel_key BLOB, sender TEXT);
             CREATE TABLE pending_community_invites (community_id TEXT, bundle_json TEXT, inviter_npub TEXT);
             CREATE TABLE community_public_invites (token TEXT, url TEXT, label TEXT);
             CREATE TABLE community_invite_link_sets (creator TEXT, locators TEXT);
             CREATE TABLE community_guestbook (community_id TEXT, events TEXT, cursor_secs INTEGER);
             CREATE TABLE community_policies (community_id TEXT, policy_id TEXT, bytes TEXT, hash TEXT, enabled INTEGER, updated_at INTEGER);
             CREATE TABLE community_migrations (community_id TEXT PRIMARY KEY, twin TEXT NOT NULL DEFAULT '');
             CREATE TABLE community_pins (community_id TEXT, channel_id TEXT, content TEXT, PRIMARY KEY (community_id, channel_id));
             CREATE TABLE attachments (key TEXT, nonce TEXT, name TEXT, url TEXT, fallback_urls TEXT, img_meta TEXT);
             CREATE TABLE events (preview_metadata TEXT);
             CREATE TABLE nip17_wrap_keys (rumor_json TEXT);
             CREATE TABLE community_bans (community_id TEXT, npub TEXT);",
        ).unwrap();
    }

    /// Every encrypted store must survive a rekey. The guestbook was missed
    /// when it landed: a PIN change left it under the old key, the read
    /// swallowed the failure as an empty list, and every member count collapsed
    /// until a full plane walk rebuilt it.
    #[test]
    fn guestbook_and_policies_survive_enable_rekey_disable() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        migrate_test_schema(&conn);
        conn.execute(
            "INSERT INTO community_guestbook (community_id, events, cursor_secs) VALUES (?1, ?2, 7)",
            rusqlite::params!["aa".repeat(32), r#"[{"rumor_id":"x"}]"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO community_policies (community_id, policy_id, bytes, hash, enabled, updated_at)
             VALUES (?1, 'p1', ?2, 'hh', 1, 0)",
            rusqlite::params!["aa".repeat(32), r#"{"format":1,"name":"x"}"#],
        )
        .unwrap();
        let read = |c: &rusqlite::Connection| -> (String, String) {
            (
                c.query_row("SELECT events FROM community_guestbook", [], |r| r.get(0)).unwrap(),
                c.query_row("SELECT bytes FROM community_policies", [], |r| r.get(0)).unwrap(),
            )
        };
        let before = read(&conn);
        let k1 = [0x21u8; 32];
        let k2 = [0x22u8; 32];
        let step = |c: &mut rusqlite::Connection, enc: Option<&[u8; 32]>, dec: Option<&[u8; 32]>| {
            let tx = c.transaction().unwrap();
            sweep_columns_in_tx(&tx, enc, dec).unwrap();
            tx.commit().unwrap();
        };
        step(&mut conn, Some(&k1), None);
        // The teeth: a table the sweep never touches stays PLAINTEXT, and a
        // round-trip over it would pass trivially. Assert the enable actually
        // wrapped both stores before checking they come back.
        let enabled = read(&conn);
        assert_ne!(enabled.0, before.0, "the guestbook must actually be encrypted by the sweep");
        assert_ne!(enabled.1, before.1, "the policy bytes must actually be encrypted by the sweep");

        step(&mut conn, Some(&k2), Some(&k1));
        step(&mut conn, None, Some(&k2));
        assert_eq!(read(&conn), before, "a rekey round-trip must return the plaintext it started with");
    }

    /// Sweep parity for the v2 columns: the owner commitment and the CORD-02 §2
    /// control pair must survive enable → rekey → disable. A missed column stays
    /// under the old key — the pair silently drops to read-only on load, and a
    /// garbled owner commitment fails the whole v2 load.
    #[test]
    fn v2_control_pair_survives_enable_rekey_disable() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        migrate_test_schema(&conn);
        let control_root: Vec<u8> = vec![0x5Au8; 32];
        conn.execute(
            "INSERT INTO communities (community_id, server_root_key, name, relays, banlist, roles, invite_registry, owner_pubkey, owner_salt, meta_extra, control_pk, control_root)
             VALUES (?1, ?2, 'n', '[]', '[]', '{}', '[]', ?3, ?4, '{\"custom\":null,\"extra\":{}}', ?5, ?6)",
            rusqlite::params!["cc".repeat(32), vec![0x11u8; 32], "ab".repeat(32), "cd".repeat(32), "ef".repeat(32), control_root],
        ).unwrap();
        let read = |c: &rusqlite::Connection| -> (String, String, Vec<u8>) {
            c.query_row("SELECT owner_pubkey, control_pk, control_root FROM communities", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            }).unwrap()
        };

        let tx = conn.transaction().unwrap();
        encrypt_columns_in_tx(&tx, &K).unwrap();
        tx.commit().unwrap();
        let (owner_pk, control_pk, root) = read(&conn);
        assert_ne!(owner_pk, "ab".repeat(32), "owner commitment must be wrapped after enable");
        assert_ne!(control_pk, "ef".repeat(32), "control_pk must be wrapped after enable");
        assert_ne!(root, vec![0x5Au8; 32], "control_root must be wrapped after enable");

        let tx = conn.transaction().unwrap();
        rekey_columns_in_tx(&tx, &K, &K2).unwrap();
        tx.commit().unwrap();

        let tx = conn.transaction().unwrap();
        decrypt_columns_in_tx(&tx, &K2).unwrap();
        tx.commit().unwrap();
        let (owner_pk, control_pk, root) = read(&conn);
        assert_eq!(owner_pk, "ab".repeat(32), "owner commitment survives enable → rekey → disable");
        assert_eq!(control_pk, "ef".repeat(32), "control_pk survives enable → rekey → disable");
        assert_eq!(root, vec![0x5Au8; 32], "control_root survives enable → rekey → disable");
    }

    /// Sweep parity: every encrypted community_public_invites column must survive
    /// enable → rekey → disable (a column missed by the sweep garbles on the first rekey).
    #[test]
    fn public_invite_label_survives_enable_rekey_disable() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        migrate_test_schema(&conn);
        conn.execute(
            "INSERT INTO community_public_invites (token, url, label) VALUES (?1, ?2, ?3)",
            rusqlite::params!["ab".repeat(32), "https://vectorapp.io/invite#x", "Reddit"],
        ).unwrap();
        let read_label = |c: &rusqlite::Connection| -> String {
            c.query_row("SELECT label FROM community_public_invites", [], |r| r.get(0)).unwrap()
        };

        let tx = conn.transaction().unwrap();
        encrypt_columns_in_tx(&tx, &K).unwrap();
        tx.commit().unwrap();
        assert_ne!(read_label(&conn), "Reddit", "label must be wrapped after enable");

        let tx = conn.transaction().unwrap();
        rekey_columns_in_tx(&tx, &K, &K2).unwrap();
        tx.commit().unwrap();

        let tx = conn.transaction().unwrap();
        decrypt_columns_in_tx(&tx, &K2).unwrap();
        tx.commit().unwrap();
        assert_eq!(read_label(&conn), "Reddit", "label must survive enable → rekey → disable");
    }

    /// The sweep list must name real columns: a typo would fail every PIN change with
    /// "no such column" instead of quietly sweeping nothing.
    #[test]
    fn sweep_list_matches_the_real_schema() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::db::schema::SQL_SCHEMA).unwrap();
        crate::db::schema::run_migrations(&mut conn).unwrap();
        for (table, cols) in SWEEP {
            conn.prepare(&format!("SELECT {} FROM {table}", cols.join(", ")))
                .unwrap_or_else(|e| panic!("{table}: {e}"));
        }
    }

    /// Every listed column, with NULLs where a column allows them, round-trips through
    /// enable, rekey and disable, and a NULL never costs its row the sweep.
    #[test]
    fn every_swept_column_survives_enable_rekey_disable() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        migrate_test_schema(&conn);
        let mut n = 0u8;
        for (table, cols) in SWEEP {
            for with_nulls in [false, true] {
                let vals: Vec<rusqlite::types::Value> = cols.iter().map(|c| {
                    n += 1;
                    let blob = conn
                        .query_row("SELECT type FROM pragma_table_info(?1) WHERE name = ?2", [table, c], |r| r.get::<_, String>(0))
                        .unwrap() == "BLOB";
                    if with_nulls && *c != cols[0] {
                        rusqlite::types::Value::Null
                    } else if blob {
                        rusqlite::types::Value::Blob(vec![n; 32])
                    } else {
                        rusqlite::types::Value::Text(format!("{c}-{n}"))
                    }
                }).collect();
                let marks = vec!["?"; cols.len()].join(", ");
                let sql = format!("INSERT OR REPLACE INTO {table} ({}) VALUES ({marks})", cols.join(", "));
                conn.execute(&sql, rusqlite::params_from_iter(vals.iter())).unwrap();
            }
        }
        let snapshot = |c: &rusqlite::Connection| -> Vec<Vec<rusqlite::types::Value>> {
            SWEEP.iter().flat_map(|(table, cols)| {
                let mut stmt = c.prepare(&format!("SELECT {} FROM {table} ORDER BY rowid", cols.join(", "))).unwrap();
                let rows = stmt
                    .query_map([], |r| (0..cols.len()).map(|i| r.get::<_, rusqlite::types::Value>(i)).collect())
                    .unwrap()
                    .map(|r| r.unwrap())
                    .collect::<Vec<Vec<rusqlite::types::Value>>>();
                rows
            }).collect()
        };
        let before = snapshot(&conn);
        let step = |c: &mut rusqlite::Connection, enc: Option<&[u8; 32]>, dec: Option<&[u8; 32]>| {
            let tx = c.transaction().unwrap();
            sweep_columns_in_tx(&tx, enc, dec).unwrap();
            tx.commit().unwrap();
        };

        step(&mut conn, Some(&K), None);
        let enabled = snapshot(&conn);
        for (row_before, row_enabled) in before.iter().zip(&enabled) {
            for (b, e) in row_before.iter().zip(row_enabled) {
                match b {
                    rusqlite::types::Value::Null => assert_eq!(e, b, "NULL stays NULL"),
                    _ => assert_ne!(e, b, "every listed value must actually be wrapped by enable"),
                }
            }
        }
        step(&mut conn, Some(&K2), Some(&K));
        {
            let tx = conn.transaction().unwrap();
            assert_eq!(sweep_residue_in_tx(&tx, &K, Some(&K2)).unwrap(), 0, "the list alone leaves nothing on the old key");
        }
        step(&mut conn, None, Some(&K2));
        assert_eq!(snapshot(&conn), before, "enable, rekey and disable return every value it started with");
    }

    /// The net under the lists: a column nobody listed still moves with the key, in its own
    /// storage class, and only what opens under the old key is touched.
    #[test]
    fn residue_sweep_moves_unlisted_ciphertext_and_nothing_else() {
        const K3: [u8; 32] = [11u8; 32];
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE future (t TEXT, b BLOB, n INTEGER);").unwrap();
        let text_ct = encrypt_with_key("hello", &K);
        let blob_ct = crate::crypto::encrypt_blob_with_key(&[0x42u8; 32], &K).unwrap();
        let foreign = encrypt_with_key("someone else's", &K3);
        let bare_hex = "ab".repeat(32);
        conn.execute("INSERT INTO future VALUES (?1, ?2, 1)", rusqlite::params![text_ct, blob_ct]).unwrap();
        conn.execute("INSERT INTO future VALUES (?1, NULL, 2)", rusqlite::params![foreign]).unwrap();
        conn.execute("INSERT INTO future VALUES (?1, x'00ff', 3)", rusqlite::params![bare_hex]).unwrap();

        let tx = conn.transaction().unwrap();
        assert_eq!(sweep_residue_in_tx(&tx, &K, Some(&K2)).unwrap(), 2);
        tx.commit().unwrap();
        let (t, b): (String, Vec<u8>) = conn.query_row("SELECT t, b FROM future WHERE n = 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(decrypt_with_key(&t, &K2).unwrap(), "hello");
        assert_eq!(crate::crypto::decrypt_blob_with_key(&b, &K2).unwrap(), vec![0x42u8; 32]);
        let untouched: (String, String) = conn.query_row(
            "SELECT (SELECT t FROM future WHERE n = 2), (SELECT t FROM future WHERE n = 3)", [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(untouched, (foreign, bare_hex), "values not under the old key are left alone");

        let tx = conn.transaction().unwrap();
        assert_eq!(sweep_residue_in_tx(&tx, &K2, None).unwrap(), 2);
        tx.commit().unwrap();
        let (t, b): (String, Vec<u8>) = conn.query_row("SELECT t, b FROM future WHERE n = 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((t.as_str(), b), ("hello", vec![0x42u8; 32]), "disable opens text to text and blob to blob");
    }

    /// Ciphertext the sweep cannot address or cannot open to text aborts the transaction
    /// rather than being left on a key the store stops using.
    #[test]
    fn residue_sweep_refuses_what_it_cannot_move() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE keyed (k TEXT PRIMARY KEY, v TEXT) WITHOUT ROWID;").unwrap();
        conn.execute("INSERT INTO keyed VALUES ('a', ?1)", [encrypt_with_key("x", &K)]).unwrap();
        let tx = conn.transaction().unwrap();
        assert!(sweep_residue_in_tx(&tx, &K, Some(&K2)).is_err());
        drop(tx);

        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE odd (v TEXT);").unwrap();
        let sealed = crate::crypto::encrypt_blob_with_key(&[0xff, 0xfe, 0x00], &K).unwrap();
        conn.execute("INSERT INTO odd VALUES (?1)", [crate::simd::hex::bytes_to_hex_string(&sealed)]).unwrap();
        let tx = conn.transaction().unwrap();
        assert!(sweep_residue_in_tx(&tx, &K, None).is_err(), "bytes that are not text never land in a text column");
    }

    /// The key proof on the real schema: whichever anchor the store has decides, and a store
    /// with none passes only when the caller does not need one.
    #[test]
    fn a_key_is_proven_by_whatever_the_store_holds() {
        let fresh = || {
            let mut c = rusqlite::Connection::open_in_memory().unwrap();
            c.execute_batch(crate::db::schema::SQL_SCHEMA).unwrap();
            crate::db::schema::run_migrations(&mut c).unwrap();
            c
        };
        let proves = |c: &mut rusqlite::Connection, k: &[u8; 32], require: bool| {
            let tx = c.transaction().unwrap();
            prove_key_in_tx(&tx, k, require).is_ok()
        };

        let mut empty = fresh();
        assert!(proves(&mut empty, &K, false), "nothing encrypted, nothing to lose");
        assert!(!proves(&mut empty, &K, true), "an enable-direction pass needs an anchor");

        let mut local = fresh();
        local.execute("INSERT INTO settings (key, value) VALUES ('pkey', ?1)", [encrypt_with_key("nsec1abc", &K)]).unwrap();
        assert!(proves(&mut local, &K, true));
        assert!(!proves(&mut local, &K2, false));

        let mut keyless = fresh();
        keyless.execute("INSERT INTO settings (key, value) VALUES ('nip55_pin_check', ?1)", [encrypt_with_key(NIP55_PIN_CANARY, &K)]).unwrap();
        assert!(proves(&mut keyless, &K, true));
        assert!(!proves(&mut keyless, &K2, false));

        let mut no_canary = fresh();
        let blob = crate::crypto::encrypt_blob_with_key(&[7u8; 32], &K).unwrap();
        no_canary.execute(
            "INSERT INTO community_epoch_keys (community_id, scope_id, epoch, key, created_at) VALUES ('c', 's', 0, ?1, 0)",
            [blob],
        ).unwrap();
        assert!(proves(&mut no_canary, &K, true), "a sealed community key is proof enough");
        assert!(!proves(&mut no_canary, &K2, false), "a key that opens none of the samples is refused");
    }

    #[test]
    fn blob_roundtrip_and_idempotent() {
        let raw = [0x42u8; 32];
        let enc = xform_blob(&raw, Some(&K), None).unwrap();
        assert_eq!(enc.len(), 60);
        assert_eq!(xform_blob(&enc, Some(&K), None).unwrap(), enc, "blob encrypt idempotent");
        assert_eq!(xform_blob(&enc, None, Some(&K)).unwrap(), raw.to_vec());
        // rekey path for blobs
        let re = xform_blob(&enc, Some(&K2), Some(&K)).unwrap();
        assert_eq!(crate::crypto::decrypt_blob_with_key(&re, &K2).unwrap(), raw.to_vec());
    }
}

/// Every encrypted column outside the hand-swept message content, settings and PIVX rows, by
/// table. Enable depends on this list alone (plaintext carries nothing to find it by); rekey and
/// disable are also backed by `sweep_residue_in_tx`, which catches a column missing here by its tag.
const SWEEP: &[(&str, &[&str])] = &[
    ("attachments", &["key", "nonce", "name", "url", "fallback_urls", "img_meta"]),
    ("events", &["preview_metadata"]),
    ("nip17_wrap_keys", &["rumor_json"]),
    ("communities", &[
        "server_root_key", "name", "relays", "description", "icon", "banner", "banlist",
        "banlist_marks", "owner_attestation", "roles", "invite_registry", "owner_pubkey",
        "owner_salt", "meta_extra", "control_pk", "control_root", "migration_pointer",
    ]),
    ("community_channels", &["channel_key", "name", "meta_extra"]),
    ("community_epoch_keys", &["key"]),
    ("community_message_keys", &["ephemeral_secret", "relays"]),
    ("pending_channel_keys", &["channel_key", "sender"]),
    ("pending_community_invites", &["bundle_json", "inviter_npub"]),
    ("community_guestbook", &["events"]),
    ("community_policies", &["bytes"]),
    ("community_public_invites", &["token", "url", "label"]),
    ("community_invite_link_sets", &["creator", "locators"]),
    ("community_migrations", &["twin"]),
    ("community_pins", &["content"]),
    ("community_bans", &["npub"]),
];

fn sweep_columns_in_tx(
    tx: &rusqlite::Transaction,
    enc: Option<&[u8; 32]>,
    dec: Option<&[u8; 32]>,
) -> Result<(), String> {
    for (table, cols) in SWEEP {
        xform_columns_in_tx(tx, table, cols, enc, dec)?;
    }
    Ok(())
}

/// Transform the named columns of every row by storage class (text, blob; NULL and numbers kept).
/// A row that fails to read aborts the transaction: skipping it would leave it on the old key.
/// A column this database never gained holds nothing to move, so it is skipped, not fatal.
fn xform_columns_in_tx(
    tx: &rusqlite::Transaction,
    table: &str,
    cols: &[&str],
    enc: Option<&[u8; 32]>,
    dec: Option<&[u8; 32]>,
) -> Result<(), String> {
    use rusqlite::types::Value;
    let present: Vec<String> = {
        let mut stmt = tx
            .prepare("SELECT name FROM pragma_table_info(?1)")
            .map_err(|e| format!("columns of {table}: {e}"))?;
        let mapped = stmt.query_map([table], |r| r.get::<_, String>(0)).map_err(|e| format!("columns of {table}: {e}"))?;
        mapped.collect::<Result<_, _>>().map_err(|e| format!("columns of {table}: {e}"))?
    };
    let cols: Vec<&str> = cols.iter().copied().filter(|c| present.iter().any(|p| p == c)).collect();
    if cols.is_empty() {
        crate::log_warn!("[Encryption] {table} is missing from this database; nothing to sweep there");
        return Ok(());
    }
    let rows: Vec<(i64, Vec<Value>)> = {
        let mut stmt = tx
            .prepare(&format!("SELECT rowid, {} FROM {table}", cols.join(", ")))
            .map_err(|e| format!("prepare {table}: {e}"))?;
        let mapped = stmt
            .query_map([], |r| {
                let vals = (1..=cols.len()).map(|i| r.get::<_, Value>(i)).collect::<Result<Vec<_>, _>>()?;
                Ok((r.get::<_, i64>(0)?, vals))
            })
            .map_err(|e| format!("query {table}: {e}"))?;
        mapped.collect::<Result<_, _>>().map_err(|e| format!("read {table}: {e}"))?
    };
    let set = cols.iter().enumerate().map(|(i, c)| format!("{c} = ?{}", i + 1)).collect::<Vec<_>>().join(", ");
    let mut update = tx
        .prepare(&format!("UPDATE {table} SET {set} WHERE rowid = ?{}", cols.len() + 1))
        .map_err(|e| format!("prepare update {table}: {e}"))?;
    for (rowid, vals) in rows {
        let mut out = Vec::with_capacity(vals.len() + 1);
        for v in &vals {
            out.push(match v {
                Value::Text(s) => Value::Text(xform_text(s, enc, dec)?),
                Value::Blob(b) => Value::Blob(xform_blob(b, enc, dec)?),
                other => other.clone(),
            });
        }
        if out == vals {
            continue;
        }
        out.push(Value::Integer(rowid));
        update
            .execute(rusqlite::params_from_iter(out.iter()))
            .map_err(|e| format!("update {table}: {e}"))?;
    }
    Ok(())
}

/// The tag-based net under the sweep lists: any text or blob cell, in any table, that opens under
/// `from` moves to `to` (None = plaintext). Nothing is left behind on a key the store stops using,
/// even in a column no list names yet. Returns how many cells the lists had missed.
pub(crate) fn sweep_residue_in_tx(
    tx: &rusqlite::Transaction,
    from: &[u8; 32],
    to: Option<&[u8; 32]>,
) -> Result<usize, String> {
    let tables: Vec<(String, bool)> = {
        let mut stmt = tx
            .prepare("SELECT name, wr FROM pragma_table_list WHERE schema = 'main' AND type = 'table' AND name NOT LIKE 'sqlite_%'")
            .map_err(|e| format!("residue: list tables: {e}"))?;
        let mapped = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))
            .map_err(|e| format!("residue: list tables: {e}"))?;
        mapped.collect::<Result<_, _>>().map_err(|e| format!("residue: list tables: {e}"))?
    };

    let quote = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
    let cipher = crate::crypto::chachapoly::ChaCha20Poly1305::new(from);
    let mut scratch = Vec::new();
    let mut total = 0;
    for (table, without_rowid) in tables {
        let cols: Vec<String> = {
            let mut stmt = tx
                .prepare("SELECT name FROM pragma_table_info(?1)")
                .map_err(|e| format!("residue: columns of {table}: {e}"))?;
            let mapped = stmt
                .query_map([&table], |r| r.get::<_, String>(0))
                .map_err(|e| format!("residue: columns of {table}: {e}"))?;
            mapped.collect::<Result<_, _>>().map_err(|e| format!("residue: columns of {table}: {e}"))?
        };
        if cols.is_empty() {
            continue;
        }
        let quoted: Vec<String> = cols.iter().map(|c| quote(c)).collect();
        let rowid = if without_rowid { "NULL" } else { "rowid" };
        // Positions only: holding the moved values for a whole table would put every
        // ciphertext it holds in memory at once.
        let mut found: Vec<(i64, usize)> = Vec::new();
        {
            let mut stmt = tx
                .prepare(&format!("SELECT {rowid}, {} FROM {}", quoted.join(", "), quote(&table)))
                .map_err(|e| format!("residue: scan {table}: {e}"))?;
            let mut rows = stmt.query([]).map_err(|e| format!("residue: scan {table}: {e}"))?;
            while let Some(row) = rows.next().map_err(|e| format!("residue: scan {table}: {e}"))? {
                for (i, col) in cols.iter().enumerate() {
                    let cell = row.get_ref(i + 1).map_err(|e| format!("residue: read {table}.{col}: {e}"))?;
                    if !opens_under(cell, &cipher, &mut scratch) {
                        continue;
                    }
                    if without_rowid {
                        return Err(format!("residue: {table}.{col} holds ciphertext but the table has no rowid to address it"));
                    }
                    found.push((row.get(0).map_err(|e| format!("residue: rowid of {table}: {e}"))?, i));
                }
            }
        }
        for &(id, i) in &found {
            let col = &quoted[i];
            let moved = tx
                .query_row(&format!("SELECT {col} FROM {} WHERE rowid = ?1", quote(&table)), [id], |r| {
                    Ok(move_cell(r.get_ref(0)?, from, to))
                })
                .map_err(|e| format!("residue: reread {table}.{}: {e}", cols[i]))?
                .map_err(|e| format!("residue: {table}.{}: {e}", cols[i]))?;
            let Some(moved) = moved else { continue };
            tx.execute(&format!("UPDATE {} SET {col} = ?1 WHERE rowid = ?2", quote(&table)), rusqlite::params![moved, id])
                .map_err(|e| format!("residue: update {table}.{}: {e}", cols[i]))?;
        }
        let mut by_col = std::collections::BTreeMap::<&str, usize>::new();
        for &(_, i) in &found {
            *by_col.entry(cols[i].as_str()).or_default() += 1;
        }
        for (col, n) in by_col {
            crate::log_warn!("[Encryption] {n} value(s) in {table}.{col} were outside the sweep lists; moved with the key");
        }
        total += found.len();
    }
    Ok(total)
}

/// Seal every message, file caption and edit whose text is still plaintext under `key`: rows that
/// landed before encryption counted as on, and text a hex-looking heuristic once skipped. Sealed
/// rows open under `key` and are left as they are. Returns how many were sealed.
fn seal_plain_messages_in_tx(tx: &rusqlite::Transaction, key: &[u8; 32]) -> Result<usize, String> {
    let cipher = crate::crypto::chachapoly::ChaCha20Poly1305::new(key);
    let mut scratch = Vec::new();
    let sealed: Vec<(i64, String)> = {
        let mut stmt = tx
            .prepare("SELECT rowid, content FROM events WHERE kind IN (?1, ?2, ?3, ?4) AND content <> ''")
            .map_err(|e| format!("plain messages: {e}"))?;
        let mut rows = stmt
            .query(rusqlite::params![
                event_kind::CHAT_MESSAGE as i32,
                event_kind::PRIVATE_DIRECT_MESSAGE as i32,
                event_kind::MESSAGE_EDIT as i32,
                event_kind::FILE_ATTACHMENT as i32,
            ])
            .map_err(|e| format!("plain messages: {e}"))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| format!("plain messages: {e}"))? {
            let cell = row.get_ref(1).map_err(|e| format!("plain messages: {e}"))?;
            if opens_under(cell, &cipher, &mut scratch) {
                continue;
            }
            let rusqlite::types::ValueRef::Text(text) = cell else { continue };
            let Ok(text) = std::str::from_utf8(text) else { continue };
            out.push((row.get(0).map_err(|e| format!("plain messages: {e}"))?, encrypt_with_key(text, key)));
        }
        out
    };
    for (rowid, content) in &sealed {
        tx.execute("UPDATE events SET content = ?1 WHERE rowid = ?2", rusqlite::params![content, rowid])
            .map_err(|e| format!("seal message: {e}"))?;
    }
    Ok(sealed.len())
}

/// Whether a cell is ciphertext under the cipher's key, in either at-rest encoding. `scratch`
/// holds each candidate's bytes and is wiped after every check.
fn opens_under(
    cell: rusqlite::types::ValueRef<'_>,
    cipher: &crate::crypto::chachapoly::ChaCha20Poly1305,
    scratch: &mut Vec<u8>,
) -> bool {
    use rusqlite::types::ValueRef;
    scratch.clear();
    match cell {
        ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
            Ok(s) if crate::crypto::looks_encrypted(s) => match crate::simd::hex::hex_string_to_bytes_checked(s) {
                Some(framed) => *scratch = framed,
                None => return false,
            },
            _ => return false,
        },
        ValueRef::Blob(bytes) => scratch.extend_from_slice(bytes),
        _ => return false,
    }
    let opened = cipher.open_framed_in_place(scratch).is_ok();
    crate::crypto::wipe(scratch);
    opened
}

/// A cell under `from`, re-sealed under `to` or opened to plaintext in its own storage class.
/// None when the cell is not ciphertext under `from`.
fn move_cell(
    cell: rusqlite::types::ValueRef<'_>,
    from: &[u8; 32],
    to: Option<&[u8; 32]>,
) -> Result<Option<rusqlite::types::Value>, String> {
    use rusqlite::types::{Value, ValueRef};
    match cell {
        ValueRef::Text(bytes) => {
            let Ok(s) = std::str::from_utf8(bytes) else { return Ok(None) };
            if !crate::crypto::looks_encrypted(s) {
                return Ok(None);
            }
            let Some(framed) = crate::simd::hex::hex_string_to_bytes_checked(s) else { return Ok(None) };
            let Ok(mut plain) = crate::crypto::decrypt_blob_with_key(&framed, from) else { return Ok(None) };
            let moved = match to {
                Some(k) => crate::crypto::encrypt_blob_with_key(&plain, k)
                    .map(|sealed| Value::Text(crate::simd::hex::bytes_to_hex_string(&sealed))),
                None => String::from_utf8(plain.clone())
                    .map(Value::Text)
                    .map_err(|_| "opens to bytes that are not text".to_string()),
            };
            plain.zeroize();
            moved.map(Some)
        }
        ValueRef::Blob(bytes) => {
            let Ok(mut plain) = crate::crypto::decrypt_blob_with_key(bytes, from) else { return Ok(None) };
            let moved = match to {
                Some(k) => crate::crypto::encrypt_blob_with_key(&plain, k).map(Value::Blob),
                None => Ok(Value::Blob(plain.clone())),
            };
            plain.zeroize();
            moved.map(Some)
        }
        _ => Ok(None),
    }
}

/// Encrypt all Concord secrets + metadata (enable flow).
fn encrypt_columns_in_tx(tx: &rusqlite::Transaction, key: &[u8; 32]) -> Result<(), String> {
    sweep_columns_in_tx(tx, Some(key), None)
}
/// Decrypt all Concord secrets + metadata (disable flow).
fn decrypt_columns_in_tx(tx: &rusqlite::Transaction, key: &[u8; 32]) -> Result<(), String> {
    sweep_columns_in_tx(tx, None, Some(key))
}
/// Re-wrap all Concord secrets + metadata old key → new key (PIN-rekey flow).
pub(crate) fn rekey_columns_in_tx(tx: &rusqlite::Transaction, old_key: &[u8; 32], new_key: &[u8; 32]) -> Result<(), String> {
    sweep_columns_in_tx(tx, Some(new_key), Some(old_key))
}

/// Bumped whenever `SWEEP` gains a column or the backfill gains a step: an account that turned
/// encryption on before then may still hold that data in plaintext, and the backfill runs once
/// more to wrap it.
/// The flag row keeps its first name, `community_at_rest_encrypted`.
const BACKFILL_VERSION: &str = "5";

/// One-time backfill for an account whose Local Encryption predates some of what it now covers:
/// any swept column, message text, seed or bunker URL still in plaintext is sealed once, gated by a
/// per-account settings flag. Idempotent (the field discriminators skip already-wrapped rows), so a crash mid-pass
/// re-runs next login. No-op when encryption is off, the key vault is empty, or the flag is current.
/// Best-effort at the call site — a failure leaves the flag unset and retries, never blocks login.
pub fn backfill_at_rest() -> Result<(), String> {
    if !crate::state::is_encryption_enabled_fast() {
        return Ok(());
    }
    if crate::db::get_sql_setting("community_at_rest_encrypted".to_string())
        .ok()
        .flatten()
        .as_deref()
        == Some(BACKFILL_VERSION)
    {
        return Ok(());
    }
    let _change = crate::crypto::gate::key_change()?;
    let mut key = match crate::state::ENCRYPTION_KEY.get() {
        Some(k) => k,
        None => return Ok(()), // locked / not yet derived — retry on a later login
    };
    let conn = crate::db::get_write_connection_guard_static()?;
    let tx = conn.unchecked_transaction().map_err(|e| format!("backfill tx: {e}"))?;
    // Setup used to seal the seed through the vault before encryption counted as on, which
    // stored it in plaintext; any plaintext seed or bunker URL is wrapped here too.
    let res = prove_key_in_tx(&tx, &key, true)
        .and_then(|_| encrypt_setting_in_tx(&tx, "seed", &key, |v| v.contains(' ')))
        .and_then(|_| encrypt_setting_in_tx(&tx, "bunker_url", &key, |v| v.starts_with("bunker://")))
        .and_then(|_| encrypt_columns_in_tx(&tx, &key))
        .and_then(|_| seal_plain_messages_in_tx(&tx, &key).map(|_| ()));
    key.zeroize();
    res?;
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('community_at_rest_encrypted', ?1)",
        [BACKFILL_VERSION],
    )
    .map_err(|e| format!("backfill flag: {e}"))?;
    tx.commit().map_err(|e| format!("backfill commit: {e}"))?;
    crate::log_info!("[Encryption] Community at-rest backfill complete");
    Ok(())
}

// ============================================================================
// Post-Migration Verification (In-Transaction)
// ============================================================================

/// Verify all critical data is plaintext within a transaction.
/// Catches any bugs (double-encryption, missed fields) BEFORE committing.
fn verify_plaintext_state_in_tx(tx: &rusqlite::Transaction) -> Result<(), String> {
    // Verify pkey is plaintext (starts with "nsec")
    if let Ok(pkey) = tx.query_row::<String, _, _>(
        "SELECT value FROM settings WHERE key = 'pkey'", [], |row| row.get(0),
    ) {
        if !pkey.starts_with("nsec") {
            return Err(format!(
                "VERIFICATION FAILED: pkey is not plaintext after decryption (len={}, prefix={}). \
                 Aborting to protect your data — encryption status was NOT changed.",
                pkey.len(), &pkey[..pkey.len().min(8)]
            ));
        }
    }

    // Verify seed is plaintext (contains spaces = BIP39 mnemonic)
    if let Ok(seed) = tx.query_row::<String, _, _>(
        "SELECT value FROM settings WHERE key = 'seed'", [], |row| row.get(0),
    ) {
        if !seed.contains(' ') {
            return Err(
                "VERIFICATION FAILED: seed is not plaintext after decryption. \
                 Aborting to protect your data — encryption status was NOT changed."
                    .to_string(),
            );
        }
    }

    // Verify PIVX keys are raw (exactly 64 hex chars)
    let mut stmt = tx.prepare("SELECT id, length(privkey_encrypted) FROM pivx_promos")
        .map_err(|e| format!("Verification query failed: {}", e))?;
    let bad_keys: Vec<i64> = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let len: i64 = row.get(1)?;
            Ok((id, len))
        })
        .map_err(|e| format!("Verification query failed: {}", e))?
        .filter_map(|r| r.ok())
        .filter(|(_, len)| *len > 64)
        .map(|(id, _)| id)
        .collect();

    if !bad_keys.is_empty() {
        return Err(format!(
            "VERIFICATION FAILED: {} PIVX key(s) still encrypted (IDs: {:?}). \
             Aborting to protect your data — encryption status was NOT changed.",
            bad_keys.len(), bad_keys
        ));
    }

    crate::log_info!("[Encryption] Verification passed: all data confirmed plaintext");
    Ok(())
}

/// Verify all critical data is encrypted within a transaction.
/// Decrypts each value to confirm it round-trips back to valid plaintext.
fn verify_encrypted_state_in_tx(
    tx: &rusqlite::Transaction,
    key: &[u8; 32],
) -> Result<(), String> {
    // Verify pkey is encrypted and round-trips to nsec
    if let Ok(pkey) = tx.query_row::<String, _, _>(
        "SELECT value FROM settings WHERE key = 'pkey'", [], |row| row.get(0),
    ) {
        if pkey.starts_with("nsec") {
            return Err(
                "VERIFICATION FAILED: pkey is still plaintext after encryption. \
                 Aborting to protect your data — encryption status was NOT changed."
                    .to_string(),
            );
        }
        match decrypt_with_key(&pkey, key) {
            Ok(decrypted) if decrypted.starts_with("nsec") => {}
            Ok(decrypted) => {
                return Err(format!(
                    "VERIFICATION FAILED: pkey decrypts but not to nsec (len={}, prefix={}). \
                     Possible double-encryption. Aborting.",
                    decrypted.len(), &decrypted[..decrypted.len().min(6)]
                ));
            }
            Err(_) => {
                return Err(
                    "VERIFICATION FAILED: pkey cannot be decrypted with current key. Aborting."
                        .to_string(),
                );
            }
        }
    }

    // Verify seed is encrypted and round-trips to BIP39
    if let Ok(seed) = tx.query_row::<String, _, _>(
        "SELECT value FROM settings WHERE key = 'seed'", [], |row| row.get(0),
    ) {
        if seed.contains(' ') {
            return Err(
                "VERIFICATION FAILED: seed is still plaintext after encryption. Aborting."
                    .to_string(),
            );
        }
        match decrypt_with_key(&seed, key) {
            Ok(decrypted) if decrypted.contains(' ') => {}
            Ok(_) => {
                return Err(
                    "VERIFICATION FAILED: seed decrypts but not to BIP39 mnemonic. \
                     Possible double-encryption. Aborting."
                        .to_string(),
                );
            }
            Err(_) => {
                return Err(
                    "VERIFICATION FAILED: seed cannot be decrypted with current key. Aborting."
                        .to_string(),
                );
            }
        }
    }

    crate::log_info!("[Encryption] Verification passed: all data confirmed encrypted and round-trips correctly");
    Ok(())
}

/// Does `key` provably decrypt this account's material? Checks whichever
/// anchor the account shape has: the pkey ciphertext (local/bunker) or the
/// canary (keyless NIP-55). No anchor (plaintext pkey, no canary) = nothing to
/// contradict, so accept — matching every other credential check in the app.
pub fn key_matches_account(key: &[u8; 32]) -> bool {
    let Ok(conn) = crate::db::get_db_connection_guard_static() else {
        return false;
    };
    let pkey: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = 'pkey'", [], |r| r.get(0))
        .ok();
    if let Some(p) = pkey {
        if !p.starts_with("nsec") {
            return matches!(decrypt_with_key(&p, key), Ok(d) if d.starts_with("nsec"));
        }
    }
    let canary: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = 'nip55_pin_check'", [], |r| r.get(0))
        .ok();
    if let Some(c) = canary {
        return matches!(
            decrypt_with_key(&c, key),
            Ok(d) if d == NIP55_PIN_CANARY
        );
    }
    true
}

/// Perform the entire re-key operation inside a single SQLite transaction.
///
/// If ANY step fails (or the app crashes), the transaction auto-rolls back
/// and the database remains entirely on the old key. This prevents the
/// catastrophic "split-key" state where some data uses the old key and
/// some uses the new key (audit C1).
///
/// Memory-efficient: collects only event IDs upfront (small strings),
/// then processes one event at a time within the transaction.
pub fn rekey(
    old_key: &[u8; 32],
    new_key: &[u8; 32],
    new_kdf: &crate::crypto::Kdf,
    security_type: &str,
    biometric_wrap: Option<&str>,
    progress: &dyn Fn(MigrationProgress),
) -> Result<(), String> {
    let wrap = match biometric_wrap {
        Some(w) => Wrap::Set(w),
        None => Wrap::Drop,
    };
    rekey_with(old_key, new_key, new_kdf, Some(security_type), wrap, Upgrade::No, progress).map(|_| ())
}

/// Move a legacy-salted account onto its own salt: the same transaction as [`rekey`], from the
/// key the unlock just proved, keeping the account's mode. The key must prove against something
/// the account holds, since a wrong one would be locked in under the new salt. The space the old
/// ciphertext freed is zeroed as it goes, the WAL is emptied after, and the next maintenance pass
/// vacuums the rest. `Ok(false)` when another unlock got there first.
pub fn upgrade_legacy_derivation(
    legacy_key: &[u8; 32],
    new_key: &[u8; 32],
    new_kdf: &crate::crypto::Kdf,
    progress: &dyn Fn(MigrationProgress),
) -> Result<bool, String> {
    if new_kdf.is_legacy() {
        return Err("an upgrade needs a salted derivation".to_string());
    }
    // Upgrades run only for typed credentials, so any wrap left here holds a key from an older
    // layout and goes with the legacy key.
    rekey_with(legacy_key, new_key, new_kdf, None, Wrap::Drop, Upgrade::Legacy, progress)
}

/// What a re-key does with the biometric wrap: it always moves with the key it protects.
enum Wrap<'a> {
    /// The new key's wrap (switching TO biometric).
    Set(&'a str),
    /// No wrap (a credential change, switching AWAY from biometric, or an upgrade).
    Drop,
}

/// Whether a re-key is the one-time move off the legacy salt, which proves its key positively,
/// re-checks under the key change that nothing moved the account first, and wipes as it goes.
#[derive(Clone, Copy, PartialEq)]
enum Upgrade {
    No,
    Legacy,
}

fn rekey_with(
    old_key: &[u8; 32],
    new_key: &[u8; 32],
    new_kdf: &crate::crypto::Kdf,
    security_type: Option<&str>,
    wrap: Wrap<'_>,
    upgrade: Upgrade,
    progress: &dyn Fn(MigrationProgress),
) -> Result<bool, String> {
    if !same_key(old_key, new_key) && new_kdf.is_legacy() {
        return Err("A new key needs its own salt".to_string());
    }
    // Held from before the transaction until the vault and the flag show the result, so no
    // write sealed under the outgoing key can land after it.
    let _change = crate::crypto::gate::key_change()?;
    if !crate::db::writes_to_live_account() {
        return Err("The account changed before its key could".to_string());
    }
    if upgrade == Upgrade::Legacy {
        // A second unlock may have moved the account while this one derived.
        let vault_holds_legacy = crate::state::ENCRYPTION_KEY.get().map(zeroize::Zeroizing::new).is_some_and(|k| same_key(&k, old_key));
        if !crate::crypto::Kdf::of_account()?.is_legacy() || !vault_holds_legacy {
            return Ok(false);
        }
    }
    let mut conn = crate::db::get_write_connection_guard_static()?;
    let wipe = upgrade == Upgrade::Legacy;
    let secure_delete_before: i64 = if wipe {
        let before = conn.query_row("PRAGMA secure_delete", [], |r| r.get(0)).map_err(|e| format!("secure_delete: {e}"))?;
        conn.execute_batch("PRAGMA secure_delete = ON;").map_err(|e| format!("secure_delete: {e}"))?;
        before
    } else {
        0
    };
    let result = rekey_tx(&mut conn, old_key, new_key, new_kdf, security_type, wrap, upgrade, progress);
    if wipe {
        // Best-effort: the commit is what matters, and the next VACUUM catches what this misses.
        let _ = conn.execute_batch(&format!("PRAGMA secure_delete = {secure_delete_before};"));
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
    result.map(|_| true)
}

#[allow(clippy::too_many_arguments)]
fn rekey_tx(
    conn: &mut rusqlite::Connection,
    old_key: &[u8; 32],
    new_key: &[u8; 32],
    new_kdf: &crate::crypto::Kdf,
    security_type: Option<&str>,
    wrap: Wrap<'_>,
    upgrade: Upgrade,
    progress: &dyn Fn(MigrationProgress),
) -> Result<(), String> {
    let (require_anchor, wipe) = (upgrade == Upgrade::Legacy, upgrade == Upgrade::Legacy);
    // Begin transaction — auto-rolls back on drop if not committed
    let tx = conn.transaction()
        .map_err(|e| format!("Failed to begin transaction: {}", e))?;

    // Set migration state inside the transaction — rolls back with everything else on crash
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('migration_state', 'rekeying')",
        [],
    ).map_err(|e| format!("Failed to set migration_state: {}", e))?;

    prove_key_in_tx(&tx, old_key, require_anchor)?;
    // The same key on both sides changes only the mode rows: re-sealing a store to itself
    // gains nothing and would hold the transaction for a full sweep.
    if !same_key(old_key, new_key) {
        rekey_listed_in_tx(&tx, old_key, new_key, progress)?;
        sweep_residue_in_tx(&tx, old_key, Some(new_key))?;
        verify_encrypted_state_in_tx(&tx, new_key)?;
    }

    // 5. Update metadata within the same transaction: the derivation lands with the data
    // sealed under it, so a crash can never leave one without the other.
    super::settings::write_kdf_in_tx(&tx, new_kdf.descriptor().as_deref())?;
    if let Some(security_type) = security_type {
        tx.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('security_type', ?1)",
            rusqlite::params![security_type],
        ).map_err(|e| format!("Failed to update security_type: {}", e))?;
    }

    // The wrap always moves with the key it protects, inside this transaction, so a crash can
    // never leave a wrap that holds a key the store no longer uses.
    match wrap {
        Wrap::Set(w) => {
            tx.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES ('biometric_wrapped_key', ?1)",
                rusqlite::params![w],
            ).map_err(|e| format!("Failed to set biometric wrap: {}", e))?;
        }
        Wrap::Drop => {
            tx.execute(
                "DELETE FROM settings WHERE key = 'biometric_wrapped_key'",
                [],
            ).map_err(|e| format!("Failed to clear biometric wrap: {}", e))?;
        }
    }
    if wipe {
        // The free pages still hold the old ciphertext; the post-sync maintenance pass
        // vacuums once this mark is gone.
        tx.execute("DELETE FROM settings WHERE key = 'last_vacuum'", [])
            .map_err(|e| format!("Failed to schedule vacuum: {}", e))?;
    }

    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('migration_state', '')",
        [],
    ).map_err(|e| format!("Failed to clear migration_state: {}", e))?;

    // Other connections see the old rows until COMMIT and the new ones after, and a read can
    // straddle it. The vault takes the new key first and the old one stays openable a moment,
    // so every read opens whichever it got.
    let moving = !same_key(old_key, new_key);
    if moving {
        crate::crypto::hold_previous_key(old_key);
        crate::state::ENCRYPTION_KEY.set(*new_key, &[]);
    }

    // 6. COMMIT — the atomic point. Everything succeeds or nothing does.
    if let Err(e) = tx.commit() {
        // A commit that reports failure may still have reached disk; the recorded derivation
        // says which key the store is under.
        let landed = crate::crypto::Kdf::of_account().is_ok_and(|k| k == *new_kdf) && !new_kdf.is_legacy();
        if moving && !landed {
            crate::state::ENCRYPTION_KEY.set(*old_key, &[]);
            crate::crypto::forget_previous_key();
        } else if moving {
            crate::crypto::forget_previous_key_later();
        }
        return Err(format!("Failed to commit re-key transaction: {}", e));
    }
    if moving {
        crate::crypto::forget_previous_key_later();
    }
    Ok(())
}

/// After a verified unlock with `password`, whose legacy-salt key `legacy_key` just proved
/// itself, move the account onto its own salt. `Ok(false)` when there is nothing to move: an
/// account already salted, a plaintext store, a biometric one (its key never came from a typed
/// credential), one with nothing to prove the key against, or one whose last attempt failed less
/// than a day ago. Any failure leaves the account exactly as it was.
pub async fn upgrade_after_unlock(password: &str, legacy_key: &[u8; 32]) -> Result<bool, String> {
    const RETRY_AFTER_SECS: u64 = 24 * 60 * 60;
    if !crate::crypto::Kdf::of_account()?.is_legacy() || !crate::state::is_encryption_enabled_fast() {
        return Ok(false);
    }
    if crate::db::get_sql_setting("security_type".to_string())?.as_deref() == Some("biometric") {
        return Ok(false);
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let last_try: u64 = crate::db::get_sql_setting("kdf_upgrade_attempt".to_string())?.and_then(|v| v.parse().ok()).unwrap_or(0);
    if now.saturating_sub(last_try) < RETRY_AFTER_SECS {
        return Ok(false);
    }
    // Checked before the slow derivation: a store with nothing to prove against never upgrades.
    if !key_proves_positively(legacy_key)? {
        return Ok(false);
    }

    // The unlock screen says why this first unlock takes longer.
    crate::emit_event("encryption_upgrading", &MigrationProgress { total: 0, completed: 0, phase: "deriving".to_string() });
    let new_kdf = crate::crypto::Kdf::fresh();
    let new_key = zeroize::Zeroizing::new(crate::crypto::derive_key(password, &new_kdf).await);
    let progress = |p: MigrationProgress| crate::emit_event("encryption_upgrading", &p);
    match upgrade_legacy_derivation(legacy_key, &new_key, &new_kdf, &progress) {
        Ok(moved) => {
            if moved {
                crate::log_info!("[Encryption] Moved this account onto its own key derivation salt");
            }
            Ok(moved)
        }
        Err(e) => {
            let _ = crate::db::set_sql_setting("kdf_upgrade_attempt".to_string(), now.to_string());
            Err(e)
        }
    }
}

/// Whether `key` opens a positive anchor in this store (the pkey, the canary, or sampled
/// ciphertext), read-only.
fn key_proves_positively(key: &[u8; 32]) -> Result<bool, String> {
    let mut conn = crate::db::get_db_connection_guard_static()?;
    let tx = conn.transaction().map_err(|e| format!("key proof: {e}"))?;
    Ok(prove_key_in_tx(&tx, key, true).is_ok())
}

/// Rebuild the database file so nothing an upgrade replaced survives in its free pages. For hosts
/// with no maintenance pass of their own; slow on a large store.
pub fn vacuum_now() -> Result<(), String> {
    let conn = crate::db::get_write_connection_guard_static()?;
    conn.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);").map_err(|e| format!("vacuum: {e}"))
}

/// Everything [`rekey`] moves by list: message content, the sealed settings, PIVX keys and
/// every `SWEEP` column, from `old_key` to `new_key` inside the caller's transaction.
pub(crate) fn rekey_listed_in_tx(
    tx: &rusqlite::Transaction,
    old_key: &[u8; 32],
    new_key: &[u8; 32],
    progress: &dyn Fn(MigrationProgress),
) -> Result<(), String> {
    // 1. Collect all encrypted event IDs (memory-efficient: just ID strings)
    let all_ids: Vec<String> = {
        let mut stmt = tx.prepare(
            r#"SELECT id FROM events
               WHERE kind IN (?1, ?2, ?3, ?4)
               AND length(content) >= 56
               AND content NOT GLOB '*[^0-9a-f]*'"#,
        ).map_err(|e| format!("Failed to prepare ID query: {}", e))?;

        let rows = stmt.query_map(
            rusqlite::params![
                event_kind::CHAT_MESSAGE as i32,
                event_kind::PRIVATE_DIRECT_MESSAGE as i32,
                event_kind::MESSAGE_EDIT as i32,
                event_kind::FILE_ATTACHMENT as i32,
            ],
            |row| row.get::<_, String>(0),
        ).map_err(|e| format!("Failed to query IDs: {}", e))?;

        rows.filter_map(|r| r.ok()).collect()
    };

    let total = all_ids.len();
    let mut completed = 0;
    let mut last_emitted_percent: i32 = -1;

    // 2. Re-key each event (one at a time, memory-efficient)
    for id in &all_ids {
        let content: Option<String> = tx.query_row(
            "SELECT content FROM events WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        ).ok();

        if let Some(content) = content {
            match decrypt_with_key(&content, old_key) {
                Ok(plaintext) => {
                    let re_encrypted = encrypt_with_key(&plaintext, new_key);
                    tx.execute(
                        "UPDATE events SET content = ?1 WHERE id = ?2",
                        rusqlite::params![re_encrypted, id],
                    ).map_err(|e| format!("Failed to update event: {}", e))?;
                }
                Err(_) => {
                    // Content looks encrypted but can't be decrypted — skip
                    crate::log_warn!("[Rekey] Skipping event {} - decrypt failed", id);
                }
            }
        }

        completed += 1;
        let current_percent = if total > 0 {
            ((completed as f64 / total as f64) * 100.0) as i32
        } else {
            100
        };

        if current_percent >= last_emitted_percent + 5 {
            last_emitted_percent = current_percent;
            progress(MigrationProgress {
                total,
                completed,
                phase: "rekeying".to_string(),
            });
        }
    }

    // 3. Re-key settings within the same transaction
    progress(MigrationProgress {
        total,
        completed,
        phase: "finalizing".to_string(),
    });

    rekey_setting_in_tx(tx, "pkey", old_key, new_key)?;
    rekey_setting_in_tx(tx, "seed", old_key, new_key)?;
    // A keyless (NIP-55) account has no pkey, so the canary is its ONLY
    // wrong-credential detector; a bunker account cannot log in without its
    // url. Both are key-derived, so both move with the key or the account is
    // locked out at the NEXT boot rather than at the failing operation.
    rekey_setting_in_tx(tx, "nip55_pin_check", old_key, new_key)?;
    rekey_setting_in_tx(tx, "bunker_url", old_key, new_key)?;
    rekey_pivx_in_tx(tx, old_key, new_key)?;
    rekey_columns_in_tx(tx, old_key, new_key)?;

    Ok(())
}

fn same_key(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Prove `key` opens this store before a sweep writes under it. The pkey or the canary decides
/// when present; otherwise the newest message ciphertexts and community key blobs must include
/// one that opens. `require_anchor` also refuses a store with nothing to prove against: an
/// enable-direction pass wraps plaintext under whatever key it is handed.
fn prove_key_in_tx(tx: &rusqlite::Transaction, key: &[u8; 32], require_anchor: bool) -> Result<(), String> {
    use rusqlite::OptionalExtension;
    const WRONG: &str = "This key does not open the account's encrypted data; nothing was changed";
    let setting = |name: &str| -> Result<Option<String>, String> {
        tx.query_row("SELECT value FROM settings WHERE key = ?1", [name], |r| r.get::<_, String>(0))
            .optional()
            .map_err(|e| format!("read {name}: {e}"))
    };
    if let Some(pkey) = setting("pkey")? {
        if !pkey.starts_with("nsec") {
            let opened = decrypt_with_key(&pkey, key).map(zeroize::Zeroizing::new);
            return match opened {
                Ok(nsec) if nsec.starts_with("nsec") => Ok(()),
                _ => Err(WRONG.to_string()),
            };
        }
    }
    if let Some(canary) = setting("nip55_pin_check")? {
        return match decrypt_with_key(&canary, key) {
            Ok(plain) if plain == NIP55_PIN_CANARY => Ok(()),
            _ => Err(WRONG.to_string()),
        };
    }
    let samples = [
        "SELECT content FROM events WHERE kind IN (?1, ?2, ?3, ?4) AND length(content) >= 56
           AND content NOT GLOB '*[^0-9a-f]*' ORDER BY created_at DESC LIMIT 256",
        "SELECT server_root_key FROM communities WHERE length(server_root_key) > 32 LIMIT 256",
        "SELECT channel_key FROM community_channels WHERE length(channel_key) > 32 LIMIT 256",
        "SELECT key FROM community_epoch_keys WHERE length(key) > 32 LIMIT 256",
    ];
    let kinds = [
        event_kind::CHAT_MESSAGE as i64,
        event_kind::PRIVATE_DIRECT_MESSAGE as i64,
        event_kind::MESSAGE_EDIT as i64,
        event_kind::FILE_ATTACHMENT as i64,
    ];
    let cipher = crate::crypto::chachapoly::ChaCha20Poly1305::new(key);
    let mut scratch = Vec::new();
    let mut seen = false;
    for sql in samples {
        let mut stmt = tx.prepare(sql).map_err(|e| format!("key proof: {e}"))?;
        let params: &[i64] = if sql.contains("?1") { &kinds } else { &[] };
        let mut rows = stmt.query(rusqlite::params_from_iter(params)).map_err(|e| format!("key proof: {e}"))?;
        while let Some(row) = rows.next().map_err(|e| format!("key proof: {e}"))? {
            seen = true;
            if opens_under(row.get_ref(0).map_err(|e| format!("key proof: {e}"))?, &cipher, &mut scratch) {
                return Ok(());
            }
        }
    }
    match (seen, require_anchor) {
        (true, _) => Err(WRONG.to_string()),
        (false, true) => Err("Nothing here can confirm the key, so the store was left as it is".to_string()),
        (false, false) => Ok(()),
    }
}

/// Re-key a single settings value within a transaction.
fn rekey_setting_in_tx(
    tx: &rusqlite::Transaction,
    key: &str,
    old_key: &[u8; 32],
    new_key: &[u8; 32],
) -> Result<(), String> {

    let val: Option<String> = tx.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params![key],
        |row| row.get(0),
    ).ok();

    if let Some(encrypted_val) = val {
        // A row that isn't ciphertext under `old_key` and isn't hex at all was
        // written plaintext (e.g. bunker_url stored before encryption was
        // enabled) — wrap it under the new key rather than failing the rekey.
        let looks_encrypted = encrypted_val.len() >= 56
            && encrypted_val.bytes().all(|b| b.is_ascii_hexdigit());
        let plaintext = match decrypt_with_key(&encrypted_val, old_key) {
            Ok(p) => p,
            Err(_) if !looks_encrypted => encrypted_val.clone(),
            Err(_) => return Err(format!("Failed to decrypt setting '{}'", key)),
        };

        let re_encrypted = encrypt_with_key(&plaintext, new_key);

        tx.execute(
            "UPDATE settings SET value = ?1 WHERE key = ?2",
            rusqlite::params![re_encrypted, key],
        ).map_err(|e| format!("Failed to update setting '{}': {}", key, e))?;
    }

    Ok(())
}

/// Re-key all PIVX promo private keys within a transaction.
fn rekey_pivx_in_tx(
    tx: &rusqlite::Transaction,
    old_key: &[u8; 32],
    new_key: &[u8; 32],
) -> Result<(), String> {

    let keys: Vec<(i64, String)> = {
        let mut stmt = tx.prepare("SELECT id, privkey_encrypted FROM pivx_promos")
            .map_err(|e| format!("Failed to prepare PIVX query: {}", e))?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
            .map_err(|e| format!("Failed to query PIVX promos: {}", e))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    for (id, key_val) in keys {
        // Only re-key if longer than a raw key (64 chars) — meaning it's encrypted
        if key_val.len() > 64 {
            let decrypted = decrypt_with_key(&key_val, old_key)
                .map_err(|_| format!("Failed to decrypt PIVX key {}", id))?;
            let re_encrypted = encrypt_with_key(&decrypted, new_key);

            tx.execute(
                "UPDATE pivx_promos SET privkey_encrypted = ?1 WHERE id = ?2",
                rusqlite::params![re_encrypted, id],
            ).map_err(|e| format!("Failed to update PIVX key: {}", e))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod rekey_setting_tests {
    use super::*;
    const OLD: [u8; 32] = [3u8; 32];
    const NEW: [u8; 32] = [5u8; 32];

    fn conn() -> rusqlite::Connection {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);").unwrap();
        c
    }
    fn put(c: &rusqlite::Connection, k: &str, v: &str) {
        c.execute("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![k, v]).unwrap();
    }
    fn get(c: &rusqlite::Connection, k: &str) -> String {
        c.query_row("SELECT value FROM settings WHERE key = ?1", rusqlite::params![k], |r| r.get(0)).unwrap()
    }

    /// The bug that shipped: a keyless account's canary stayed under the dead
    /// key, so the NEXT cold boot rejected the correct credential.
    #[test]
    fn canary_survives_rekey() {
        let mut c = conn();
        put(&c, "nip55_pin_check", &encrypt_with_key(NIP55_PIN_CANARY, &OLD));
        let tx = c.transaction().unwrap();
        rekey_setting_in_tx(&tx, "nip55_pin_check", &OLD, &NEW).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            decrypt_with_key(&get(&c, "nip55_pin_check"), &NEW).unwrap(),
            NIP55_PIN_CANARY
        );
    }

    /// Same shape for bunker accounts: an un-rekeyed url = permanent login failure.
    #[test]
    fn bunker_url_survives_rekey() {
        let mut c = conn();
        let url = "bunker://abc?relay=wss://relay.example";
        put(&c, "bunker_url", &encrypt_with_key(url, &OLD));
        let tx = c.transaction().unwrap();
        rekey_setting_in_tx(&tx, "bunker_url", &OLD, &NEW).unwrap();
        tx.commit().unwrap();
        assert_eq!(decrypt_with_key(&get(&c, "bunker_url"), &NEW).unwrap(), url);
    }

    /// A row written before encryption was enabled is plaintext: wrap it under
    /// the new key rather than failing the whole migration.
    #[test]
    fn plaintext_row_is_adopted_not_fatal() {
        let mut c = conn();
        let url = "bunker://plain?relay=wss://r.example";
        put(&c, "bunker_url", url);
        let tx = c.transaction().unwrap();
        rekey_setting_in_tx(&tx, "bunker_url", &OLD, &NEW).unwrap();
        tx.commit().unwrap();
        assert_eq!(decrypt_with_key(&get(&c, "bunker_url"), &NEW).unwrap(), url);
    }

    /// Ciphertext that does NOT belong to `old_key` must abort the rekey — the
    /// alternative is committing a value nobody can ever decrypt.
    #[test]
    fn wrong_old_key_aborts() {
        let mut c = conn();
        put(&c, "pkey", &encrypt_with_key("nsec1example", &[9u8; 32]));
        let tx = c.transaction().unwrap();
        assert!(rekey_setting_in_tx(&tx, "pkey", &OLD, &NEW).is_err());
    }

    /// Absent rows are a no-op (accounts legitimately lack pkey, seed, canary).
    #[test]
    fn missing_row_is_noop() {
        let mut c = conn();
        let tx = c.transaction().unwrap();
        rekey_setting_in_tx(&tx, "nip55_pin_check", &OLD, &NEW).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            c.query_row("SELECT count(*) FROM settings", [], |r| r.get::<_, i64>(0)).unwrap(), 0
        );
    }
}
