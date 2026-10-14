//! Dry run of the Concord v1 purge (migrations 103 + 104) against a COPY of a real database.
//!
//! `cargo run -p vector-core --example v1_purge_dry_run -- <path/to/vector.db> [scratch-dir]`
//!
//! The database and its `-wal`/`-shm` are copied into the scratch dir first; the original is
//! only ever read. The purge runs inside a transaction on the copy, prints what it changed, and
//! rolls back.

use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap_or(-1)
}

/// A community's name when it is stored in the clear (Local Encryption off).
fn name_of(conn: &Connection, id: &str) -> String {
    let name: Option<String> = conn
        .query_row("SELECT name FROM communities WHERE community_id = ?1", params![id], |r| r.get(0))
        .optional()
        .ok()
        .flatten();
    match name {
        Some(n) if !vector_core::crypto::looks_encrypted(&n) => n,
        Some(_) => "<sealed>".into(),
        None => "<no row>".into(),
    }
}

const TABLES: &[(&str, &str)] = &[
    ("communities (v1)", "SELECT COUNT(*) FROM communities WHERE protocol IS NULL OR protocol = 1"),
    ("communities (v2)", "SELECT COUNT(*) FROM communities WHERE protocol = 2"),
    ("v2 channels", "SELECT COUNT(*) FROM community_channels WHERE community_id IN (SELECT community_id FROM communities WHERE protocol = 2)"),
    ("v2-named rooms", "SELECT COUNT(*) FROM chats WHERE chat_type = 2 AND (chat_identifier IN (SELECT channel_id FROM community_channels WHERE community_id IN (SELECT community_id FROM communities WHERE protocol = 2)) OR json_extract(metadata, '$.custom_fields.community_id') IN (SELECT community_id FROM communities WHERE protocol = 2))"),
    ("events in v2-named rooms", "SELECT COUNT(*) FROM events WHERE chat_id IN (SELECT id FROM chats WHERE chat_type = 2 AND (chat_identifier IN (SELECT channel_id FROM community_channels WHERE community_id IN (SELECT community_id FROM communities WHERE protocol = 2)) OR json_extract(metadata, '$.custom_fields.community_id') IN (SELECT community_id FROM communities WHERE protocol = 2)))"),
    ("v2 epoch keys", "SELECT COUNT(*) FROM community_epoch_keys WHERE community_id IN (SELECT community_id FROM communities WHERE protocol = 2)"),
    ("v2 edition heads", "SELECT COUNT(*) FROM community_edition_heads WHERE community_id IN (SELECT community_id FROM communities WHERE protocol = 2)"),
    ("community_channels", "SELECT COUNT(*) FROM community_channels"),
    ("community rooms", "SELECT COUNT(*) FROM chats WHERE chat_type = 2"),
    ("DM chats", "SELECT COUNT(*) FROM chats WHERE chat_type = 0"),
    ("events in community rooms", "SELECT COUNT(*) FROM events WHERE chat_id IN (SELECT id FROM chats WHERE chat_type = 2)"),
    ("events in DMs", "SELECT COUNT(*) FROM events WHERE chat_id IN (SELECT id FROM chats WHERE chat_type = 0)"),
    ("attachments", "SELECT COUNT(*) FROM attachments"),
    ("community_message_keys", "SELECT COUNT(*) FROM community_message_keys"),
    ("community_epoch_keys", "SELECT COUNT(*) FROM community_epoch_keys"),
    ("community_edition_heads", "SELECT COUNT(*) FROM community_edition_heads"),
    ("pending_community_invites", "SELECT COUNT(*) FROM pending_community_invites"),
    ("notify_prefs", "SELECT COUNT(*) FROM notify_prefs"),
    ("nip17_wrap_keys", "SELECT COUNT(*) FROM nip17_wrap_keys"),
    ("deleted_messages", "SELECT COUNT(*) FROM deleted_messages"),
    ("processed_wrappers", "SELECT COUNT(*) FROM processed_wrappers"),
];

fn snapshot(conn: &Connection) -> Vec<i64> {
    TABLES.iter().map(|(_, sql)| count(conn, sql)).collect()
}

fn copy_db(src: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let name = src.file_name().expect("a database file name");
    let dst = dir.join(name);
    std::fs::copy(src, &dst)?;
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", src.display()));
        if side.exists() {
            std::fs::copy(&side, PathBuf::from(format!("{}{suffix}", dst.display())))?;
        }
    }
    Ok(dst)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(src) = args.next().map(PathBuf::from) else {
        eprintln!("usage: v1_purge_dry_run <path/to/vector.db> [scratch-dir]");
        std::process::exit(2);
    };
    let scratch = args.next().map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("v1-purge-dry-run"));
    let copy = copy_db(&src, &scratch).expect("copy the database");
    let mut conn = Connection::open(&copy).expect("open the copy");
    conn.execute_batch("PRAGMA foreign_keys = ON;").expect("foreign keys");

    let before = snapshot(&conn);
    let tx = conn.transaction().expect("transaction");
    let plan = vector_core::db::v1_purge::plan_v1_purge(&tx).expect("plan");

    println!("== plan for {}", src.display());
    println!("held flips to finish: {}", plan.flips.len());
    for (v1, v2) in &plan.flips {
        println!("  {} -> {}  ({})", short(v1), short(v2), name_of(&tx, v2));
    }
    println!("v1 communities removed: {}", plan.doomed_communities.len());
    for id in &plan.doomed_communities {
        println!("  {}  ({})", short(id), name_of(&tx, id));
    }
    println!("rooms removed: {}", plan.doomed_rooms.len());
    println!("rooms restamped to their v2 parent: {}", plan.restamps.len());
    println!("rooms with unreadable metadata (kept): {}", plan.unreadable_rooms.len());

    vector_core::db::v1_purge::apply_v1_purge(&tx, &plan).expect("apply 103");
    let again = vector_core::db::v1_purge::plan_v1_purge(&tx).expect("re-plan");
    let mutes: String = tx
        .query_row("SELECT value FROM settings WHERE key = 'purged_muted_chats'", [], |r| r.get(0))
        .unwrap_or_default();
    println!("purged rooms kept in the synced Mutes list: {mutes}");
    vector_core::db::v1_purge::drop_v1_columns(&tx).expect("apply 104");
    let after = snapshot(&tx);

    println!("\n== row counts (before -> after)");
    for (((label, _), b), a) in TABLES.iter().zip(&before).zip(&after) {
        let mark = if a != b { "  *" } else { "" };
        println!("{label:<28} {b:>8} -> {a:<8}{mark}");
    }
    println!("\nsecond run finds nothing to do: {}", again.is_empty());
    if !again.is_empty() {
        println!(
            "  second plan: {} flips, {} communities, {} rooms, {} restamps",
            again.flips.len(),
            again.doomed_communities.len(),
            again.doomed_rooms.len(),
            again.restamps.len()
        );
    }
    tx.rollback().expect("roll back");
    println!("rolled back; the copy at {} is unchanged", copy.display());
}
