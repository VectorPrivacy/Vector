//! MLS event tracking and deduplication.
//!
//! Handles:
//! - Tracking which MLS wrapper events have been processed
//! - Legacy database migration/cleanup

/// Check if an MLS event has already been processed.
/// Returns true if the event_id exists in mls_processed_events table.
pub fn is_mls_event_processed(event_id: &str) -> bool {
    let conn = match crate::db::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return false,
    };

    conn.query_row(
        "SELECT 1 FROM mls_processed_events WHERE event_id = ?1",
        rusqlite::params![event_id],
        |_| Ok(true),
    ).unwrap_or(false)
}

/// Mark an MLS event as processed.
/// Uses INSERT OR IGNORE to handle race conditions safely.
pub fn track_mls_event_processed(
    event_id: &str,
    group_id: &str,
    event_created_at: u64,
) -> Result<(), String> {
    let conn = crate::db::get_write_connection_guard_static()?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    conn.execute(
        "INSERT OR IGNORE INTO mls_processed_events (event_id, group_id, created_at, processed_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![event_id, group_id, event_created_at as i64, now as i64],
    ).map_err(|e| format!("Failed to track processed event: {}", e))?;

    Ok(())
}

/// Persist a pending (Unprocessable) MLS event for retry on subsequent syncs.
///
/// "Unprocessable" almost always means "the prerequisite commit hasn't been
/// processed yet" — the prerequisite may arrive on a later relay fetch,
/// possibly days later. We hold the full event JSON so retries don't
/// require refetching from a relay that may have GC'd it.
///
/// Idempotent: re-saving an existing event_id increments retry_count and
/// updates last_retry_at without disturbing first_seen_at.
pub fn save_pending_event(group_id: &str, event: &nostr_sdk::Event) -> Result<(), String> {
    let conn = crate::db::get_write_connection_guard_static()?;
    let event_json = serde_json::to_string(event)
        .map_err(|e| format!("Failed to serialize pending event: {}", e))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    conn.execute(
        "INSERT INTO mls_pending_events
            (event_id, group_id, event_json, first_seen_at, last_retry_at, retry_count)
            VALUES (?1, ?2, ?3, ?4, ?4, 0)
            ON CONFLICT(event_id) DO UPDATE SET
                last_retry_at = excluded.last_retry_at,
                retry_count = retry_count + 1",
        rusqlite::params![event.id.to_hex(), group_id, event_json, now],
    ).map_err(|e| format!("Failed to save pending event: {}", e))?;

    Ok(())
}

/// Load all currently-pending events for a group.
///
/// Returns events in no particular order; callers that care about ordering
/// (e.g. the sync retry loop) should sort by `created_at` themselves.
pub fn load_pending_events(group_id: &str) -> Result<Vec<nostr_sdk::Event>, String> {
    let conn = crate::db::get_db_connection_guard_static()?;
    let mut stmt = conn.prepare(
        "SELECT event_json FROM mls_pending_events WHERE group_id = ?1"
    ).map_err(|e| format!("Failed to prepare load_pending: {}", e))?;

    let rows = stmt.query_map(rusqlite::params![group_id], |row| row.get::<_, String>(0))
        .map_err(|e| format!("Failed to query mls_pending_events: {}", e))?;

    let mut events = Vec::new();
    for row in rows {
        let json = row.map_err(|e| format!("row error: {}", e))?;
        match serde_json::from_str::<nostr_sdk::Event>(&json) {
            Ok(ev) => events.push(ev),
            Err(e) => crate::log_warn!("[MLS] skipping unparseable pending event: {}", e),
        }
    }
    Ok(events)
}

/// Remove a pending event after it's been successfully processed.
pub fn remove_pending_event(event_id: &str) -> Result<(), String> {
    let conn = crate::db::get_write_connection_guard_static()?;
    conn.execute(
        "DELETE FROM mls_pending_events WHERE event_id = ?1",
        rusqlite::params![event_id],
    ).map_err(|e| format!("Failed to remove pending event: {}", e))?;
    Ok(())
}

/// Delete pending events older than `max_age_secs`. Returns count deleted.
///
/// After enough time the prerequisite is genuinely unrecoverable — the
/// originating relay has GC'd it and the event is dead weight. Callers
/// surface a "rejoin needed" UI hint when this fires non-trivially.
pub fn prune_old_pending_events(group_id: &str, max_age_secs: u64) -> Result<u32, String> {
    let conn = crate::db::get_write_connection_guard_static()?;
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(max_age_secs) as i64;

    let n = conn.execute(
        "DELETE FROM mls_pending_events WHERE group_id = ?1 AND first_seen_at < ?2",
        rusqlite::params![group_id, cutoff],
    ).map_err(|e| format!("Failed to prune pending events: {}", e))?;

    Ok(n as u32)
}

/// Wipe an old MDK database (v0.2.x) that is incompatible with the current engine.
///
/// Detection: the old dual-connection architecture created an
/// `openmls_sqlite_storage_migrations` table. The new unified MDK never creates this.
///
/// Returns `true` if a legacy database was detected and wiped.
pub fn wipe_legacy_mls_database(db_path: &std::path::Path) -> bool {
    let is_legacy = match rusqlite::Connection::open(db_path) {
        Ok(conn) => conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='openmls_sqlite_storage_migrations'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0,
        Err(_) => false,
    };

    if is_legacy {
        println!("[MLS] Detected legacy v0.2.x MLS database — wiping for clean start...");
        if let Err(e) = std::fs::remove_file(db_path) {
            eprintln!("[MLS] Failed to remove legacy database: {}", e);
        }
        // Also remove SQLite journal/WAL sidecar files
        let _ = std::fs::remove_file(db_path.with_extension("db-shm"));
        let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
        let _ = std::fs::remove_file(db_path.with_extension("db-journal"));
        println!("[MLS] Legacy database wiped. Groups will need to be re-joined.");
    }

    is_legacy
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::*;

    /// Mutex serializing tests that touch the global DB pool.
    /// Mirrors the helper in mls::service tests.
    static DB_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = DB_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        let tmp = tempfile::tempdir().unwrap();
        let n = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let account = format!("npub1pendingtest{}", n);
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard)
    }

    async fn fake_mls_event(content: &str) -> Event {
        let keys = Keys::generate();
        EventBuilder::new(Kind::MlsGroupMessage, content)
            .tag(Tag::custom(
                TagKind::SingleLetter(SingleLetterTag::lowercase(Alphabet::H)),
                vec!["c".repeat(64)],
            ))
            .build(keys.public_key())
            .sign(&keys)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn pending_event_round_trip() {
        let (_tmp, _guard) = init_test_db();
        let group_id = "c".repeat(64);
        let ev = fake_mls_event("round-trip").await;

        save_pending_event(&group_id, &ev).unwrap();
        let loaded = load_pending_events(&group_id).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, ev.id);
        assert_eq!(loaded[0].content, "round-trip");
    }

    #[tokio::test]
    async fn pending_event_idempotent_save_increments_retry_count() {
        let (_tmp, _guard) = init_test_db();
        let group_id = "c".repeat(64);
        let ev = fake_mls_event("idempotent").await;

        save_pending_event(&group_id, &ev).unwrap();
        save_pending_event(&group_id, &ev).unwrap();
        save_pending_event(&group_id, &ev).unwrap();

        let conn = crate::db::get_db_connection_guard_static().unwrap();
        let retry_count: i64 = conn.query_row(
            "SELECT retry_count FROM mls_pending_events WHERE event_id = ?1",
            rusqlite::params![ev.id.to_hex()],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(retry_count, 2);

        // first_seen_at must NOT have shifted across the re-saves.
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM mls_pending_events WHERE group_id = ?1",
            rusqlite::params![group_id],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn pending_event_remove() {
        let (_tmp, _guard) = init_test_db();
        let group_id = "c".repeat(64);
        let ev = fake_mls_event("remove").await;

        save_pending_event(&group_id, &ev).unwrap();
        remove_pending_event(&ev.id.to_hex()).unwrap();
        let loaded = load_pending_events(&group_id).unwrap();
        assert!(loaded.is_empty());
    }

    #[tokio::test]
    async fn prune_old_pending_events_respects_age() {
        let (_tmp, _guard) = init_test_db();
        let group_id = "c".repeat(64);
        let ev_old = fake_mls_event("old").await;
        let ev_new = fake_mls_event("new").await;

        save_pending_event(&group_id, &ev_old).unwrap();
        save_pending_event(&group_id, &ev_new).unwrap();

        // Backdate ev_old's first_seen_at by 100 days.
        let conn = crate::db::get_write_connection_guard_static().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let backdated = now - 100 * 24 * 60 * 60;
        conn.execute(
            "UPDATE mls_pending_events SET first_seen_at = ?1 WHERE event_id = ?2",
            rusqlite::params![backdated, ev_old.id.to_hex()],
        ).unwrap();
        drop(conn);

        // Prune 90+ days.
        let pruned = prune_old_pending_events(&group_id, 90 * 24 * 60 * 60).unwrap();
        assert_eq!(pruned, 1);

        let loaded = load_pending_events(&group_id).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, ev_new.id);
    }

    #[tokio::test]
    async fn pending_event_load_empty_group_returns_empty() {
        // Sanity: load on a never-saved group returns empty Vec, not error.
        let (_tmp, _guard) = init_test_db();
        let loaded = load_pending_events(&"d".repeat(64)).unwrap();
        assert!(loaded.is_empty());
    }

    #[tokio::test]
    async fn pending_event_remove_nonexistent_is_noop() {
        // Removing a row that was never saved must not error (DELETE is silent).
        let (_tmp, _guard) = init_test_db();
        let bogus_id = "f".repeat(64);
        let result = remove_pending_event(&bogus_id);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn pending_event_save_preserves_first_seen_at_on_conflict() {
        // BUG-CLASS: ON CONFLICT clause must NOT touch first_seen_at, otherwise
        // every save would extend the 90-day TTL and old events would never prune.
        //
        // Deterministic via SQL sentinels (no sleeps, no scheduler dependence):
        // 1. First save records first_seen_at = now (real value, varies)
        // 2. We OVERWRITE first_seen_at and last_retry_at to known sentinels (0)
        // 3. Second save (re-save) must NOT touch first_seen_at (stays 0) but
        //    MUST update last_retry_at (jumps to real `now`, > 0).
        let (_tmp, _guard) = init_test_db();
        let group_id = "c".repeat(64);
        let ev = fake_mls_event("preserve-first-seen").await;

        save_pending_event(&group_id, &ev).unwrap();

        // Pin both timestamps to a sentinel so any unintentional update
        // becomes detectable.
        let conn = crate::db::get_write_connection_guard_static().unwrap();
        conn.execute(
            "UPDATE mls_pending_events SET first_seen_at = 0, last_retry_at = 0 WHERE event_id = ?1",
            rusqlite::params![ev.id.to_hex()],
        ).unwrap();
        drop(conn);

        // Re-save (simulates retry). ON CONFLICT clause should:
        // - leave first_seen_at alone (not in the SET list)
        // - overwrite last_retry_at with `excluded.last_retry_at` (real now)
        // - bump retry_count from 0 to 1
        save_pending_event(&group_id, &ev).unwrap();

        let conn = crate::db::get_db_connection_guard_static().unwrap();
        let (final_first_seen, last_retry, retry_count): (i64, i64, i64) = conn.query_row(
            "SELECT first_seen_at, last_retry_at, retry_count FROM mls_pending_events WHERE event_id = ?1",
            rusqlite::params![ev.id.to_hex()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();

        assert_eq!(final_first_seen, 0, "first_seen_at must NOT advance on conflict (sentinel preserved)");
        assert!(last_retry > 0, "last_retry_at must advance on conflict (sentinel overwritten with now)");
        assert_eq!(retry_count, 1, "retry_count must increment on conflict");
    }

    #[tokio::test]
    async fn pending_event_corrupt_json_skipped_silently() {
        // BUG-CLASS: a single corrupt row (e.g. from a future serde-incompatible
        // nostr-sdk bump) must not poison load_pending_events. The good rows
        // should still come back.
        let (_tmp, _guard) = init_test_db();
        let group_id = "c".repeat(64);
        let good_event = fake_mls_event("good").await;

        save_pending_event(&group_id, &good_event).unwrap();

        // Inject a corrupt row directly via SQL
        let conn = crate::db::get_write_connection_guard_static().unwrap();
        conn.execute(
            "INSERT INTO mls_pending_events
                (event_id, group_id, event_json, first_seen_at, last_retry_at, retry_count)
                VALUES (?1, ?2, '{not valid json}', 0, 0, 0)",
            rusqlite::params!["dead".repeat(16), group_id],
        ).unwrap();
        drop(conn);

        let loaded = load_pending_events(&group_id).unwrap();
        assert_eq!(loaded.len(), 1, "corrupt row dropped, good row preserved");
        assert_eq!(loaded[0].id, good_event.id);
    }

    #[tokio::test]
    async fn pending_event_prune_isolated_per_group() {
        // BUG-CLASS: prune must only touch the requested group. Cross-group
        // contamination would silently destroy unrelated pending state.
        let (_tmp, _guard) = init_test_db();
        let group_a = "a".repeat(64);
        let group_b = "b".repeat(64);
        let ev_a = fake_mls_event("a").await;
        let ev_b = fake_mls_event("b").await;

        save_pending_event(&group_a, &ev_a).unwrap();
        save_pending_event(&group_b, &ev_b).unwrap();

        // Backdate group_a's row by 100 days
        let conn = crate::db::get_write_connection_guard_static().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let backdated = now - 100 * 24 * 60 * 60;
        conn.execute(
            "UPDATE mls_pending_events SET first_seen_at = ?1 WHERE event_id = ?2",
            rusqlite::params![backdated, ev_a.id.to_hex()],
        ).unwrap();
        drop(conn);

        // Prune group_a only at 90 days
        let pruned = prune_old_pending_events(&group_a, 90 * 24 * 60 * 60).unwrap();
        assert_eq!(pruned, 1);

        // group_a now empty, group_b untouched
        assert!(load_pending_events(&group_a).unwrap().is_empty());
        let loaded_b = load_pending_events(&group_b).unwrap();
        assert_eq!(loaded_b.len(), 1);
        assert_eq!(loaded_b[0].id, ev_b.id);
    }

    #[tokio::test]
    async fn pending_event_prune_zero_max_age_deletes_all_for_group() {
        // Edge: max_age_secs=0 means "anything older than now" → cutoff = now,
        // delete WHERE first_seen_at < now. To make this deterministic, we
        // backdate first_seen_at to 0 via raw SQL (no sleep needed).
        let (_tmp, _guard) = init_test_db();
        let group_id = "c".repeat(64);
        let ev = fake_mls_event("test").await;

        save_pending_event(&group_id, &ev).unwrap();
        let conn = crate::db::get_write_connection_guard_static().unwrap();
        conn.execute(
            "UPDATE mls_pending_events SET first_seen_at = 0 WHERE event_id = ?1",
            rusqlite::params![ev.id.to_hex()],
        ).unwrap();
        drop(conn);

        let pruned = prune_old_pending_events(&group_id, 0).unwrap();
        assert_eq!(pruned, 1);
        assert!(load_pending_events(&group_id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn pending_event_prune_returns_zero_when_nothing_matches() {
        // No-op prune must report 0 deletions, not error.
        let (_tmp, _guard) = init_test_db();
        let group_id = "c".repeat(64);
        let ev = fake_mls_event("fresh").await;
        save_pending_event(&group_id, &ev).unwrap();

        let pruned = prune_old_pending_events(&group_id, 999_999_999).unwrap();
        assert_eq!(pruned, 0);
        assert_eq!(load_pending_events(&group_id).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn pending_event_load_returns_only_requested_group() {
        // load_pending_events filters on group_id — verify cross-group bleed
        // can't happen.
        let (_tmp, _guard) = init_test_db();
        let group_a = "a".repeat(64);
        let group_b = "b".repeat(64);
        let ev_a = fake_mls_event("from-a").await;
        let ev_b = fake_mls_event("from-b").await;
        save_pending_event(&group_a, &ev_a).unwrap();
        save_pending_event(&group_b, &ev_b).unwrap();

        let loaded_a = load_pending_events(&group_a).unwrap();
        let loaded_b = load_pending_events(&group_b).unwrap();
        assert_eq!(loaded_a.len(), 1);
        assert_eq!(loaded_a[0].id, ev_a.id);
        assert_eq!(loaded_b.len(), 1);
        assert_eq!(loaded_b[0].id, ev_b.id);
    }

    #[test]
    fn wipe_legacy_nonexistent_path() {
        let tmp = std::env::temp_dir().join("vector_test_wipe_nonexistent.db");
        let _ = std::fs::remove_file(&tmp);
        assert!(!wipe_legacy_mls_database(&tmp));
    }

    #[test]
    fn wipe_legacy_modern_db() {
        let tmp = std::env::temp_dir().join("vector_test_wipe_modern.db");
        let _ = std::fs::remove_file(&tmp);

        // Create a modern DB (no openmls migration table)
        let conn = rusqlite::Connection::open(&tmp).unwrap();
        conn.execute("CREATE TABLE some_table (id INTEGER)", []).unwrap();
        drop(conn);

        assert!(!wipe_legacy_mls_database(&tmp));
        assert!(tmp.exists()); // Not wiped
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn wipe_legacy_old_db() {
        let tmp = std::env::temp_dir().join("vector_test_wipe_legacy.db");
        let _ = std::fs::remove_file(&tmp);

        // Create a legacy DB with the marker table
        let conn = rusqlite::Connection::open(&tmp).unwrap();
        conn.execute("CREATE TABLE openmls_sqlite_storage_migrations (id INTEGER)", []).unwrap();
        drop(conn);

        assert!(wipe_legacy_mls_database(&tmp));
        assert!(!tmp.exists()); // Wiped
    }
}
