//! Last-known bot manifests (kind 10304) — the `/` command picker's persistent
//! layer. One row per bot pubkey, replaced only by a NEWER manifest edition, so
//! the picker serves instantly from boot while a background refetch converges.
//! Manifests are PUBLIC replaceable events: rows are plaintext by design.

use rusqlite::{params, OptionalExtension};

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Store a validated manifest for `pubkey_hex`, keeping whichever edition is
/// newest (`event_created_at` = the manifest event's timestamp). An equal-time
/// re-fetch refreshes `fetched_at` only via the replace (idempotent).
pub fn upsert_bot_manifest(pubkey_hex: &str, manifest_json: &str, event_created_at: u64) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT INTO bot_manifests (pubkey, manifest, event_created_at, fetched_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(pubkey) DO UPDATE SET
             manifest = excluded.manifest,
             event_created_at = excluded.event_created_at,
             fetched_at = excluded.fetched_at
         WHERE excluded.event_created_at >= bot_manifests.event_created_at",
        params![pubkey_hex, manifest_json, event_created_at as i64, now_secs() as i64],
    )
    .map_err(|e| format!("upsert bot manifest: {e}"))?;
    Ok(())
}

/// Last-known manifest JSON for one bot, with its edition timestamp.
pub fn get_bot_manifest(pubkey_hex: &str) -> Result<Option<(String, u64)>, String> {
    let conn = super::get_db_connection_guard_static()?;
    conn.query_row(
        "SELECT manifest, event_created_at FROM bot_manifests WHERE pubkey = ?1",
        params![pubkey_hex],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)),
    )
    .optional()
    .map_err(|e| format!("get bot manifest: {e}"))
}

/// Last-known manifests for a set of bots: `(pubkey_hex, manifest_json)`.
/// Bots with no stored manifest are simply absent.
pub fn get_bot_manifests(pubkeys: &[String]) -> Result<Vec<(String, String)>, String> {
    if pubkeys.is_empty() {
        return Ok(Vec::new());
    }
    let conn = super::get_db_connection_guard_static()?;
    let placeholders = vec!["?"; pubkeys.len()].join(",");
    let sql = format!("SELECT pubkey, manifest FROM bot_manifests WHERE pubkey IN ({placeholders})");
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("get bot manifests: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(pubkeys.iter()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| format!("get bot manifests: {e}"))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        use nostr_sdk::prelude::ToBech32;
        let tmp = tempfile::tempdir().unwrap();
        let account = nostr_sdk::prelude::Keys::generate().public_key().to_bech32().unwrap();
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard)
    }

    #[test]
    fn manifest_store_keeps_the_newest_edition() {
        let (_tmp, _guard) = init_test_db();

        upsert_bot_manifest("aa", r#"{"v":1,"commands":[]}"#, 100).unwrap();
        assert_eq!(get_bot_manifest("aa").unwrap().unwrap().1, 100);

        // A newer edition replaces…
        upsert_bot_manifest("aa", r#"{"v":1,"commands":[{"name":"x","description":"d"}]}"#, 200).unwrap();
        let (json, at) = get_bot_manifest("aa").unwrap().unwrap();
        assert_eq!(at, 200);
        assert!(json.contains("\"x\""));

        // …while an OLDER one is refused (a lagging relay can't roll us back).
        upsert_bot_manifest("aa", r#"{"v":1,"commands":[]}"#, 150).unwrap();
        let (json, at) = get_bot_manifest("aa").unwrap().unwrap();
        assert_eq!(at, 200);
        assert!(json.contains("\"x\""));

        upsert_bot_manifest("bb", r#"{"v":1,"commands":[]}"#, 50).unwrap();
        let batch = get_bot_manifests(&["aa".into(), "bb".into(), "cc".into()]).unwrap();
        assert_eq!(batch.len(), 2, "absent bots are simply missing");
        assert!(get_bot_manifests(&[]).unwrap().is_empty());
    }
}
