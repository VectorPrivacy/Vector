//! Concord v1's removal from a database: finish every held flip, then drop each v1
//! community, its rooms and its side rows, keeping v2 whole.
//!
//! Key-free: it reads only plaintext columns (ids, protocol, chat metadata), so it runs
//! before any vault opens. A room whose metadata won't parse is kept and logged, never fatal.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Connection, OptionalExtension};

/// Tables keyed by `community_id` whose rows go with a doomed community.
const PER_COMMUNITY_TABLES: &[&str] = &[
    "community_channels",
    "community_epoch_keys",
    "community_edition_heads",
    "community_public_invites",
    "community_invite_link_sets",
    "community_bans",
    "community_policies",
    "community_guestbook",
    "community_pins",
    "pending_channel_keys",
    "pending_community_invites",
    "community_migrations",
];

/// Settings only v1 reads. `community_list_json` goes last, after its tombstones are mirrored.
const V1_SETTINGS: &[&str] = &[
    "community_list_published_at",
    "community_list_compacted",
    "invite_list_json",
    "invite_list_published_at",
    "concord_control_probe_cursor",
    "community_list_json",
];

const TOMBSTONE_MIRROR_KEY: &str = "community_list_tombstones";

/// What the purge would do, computed without writing to the main database.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PurgePlan {
    /// Held twins to finish: (v1 id, v2 id). Every channel the v1 row owns moves to the twin.
    pub flips: Vec<(String, String)>,
    /// Every v1 community, flipped or not.
    pub doomed_communities: Vec<String>,
    /// `chats.id` row ids of the rooms that go.
    pub doomed_rooms: Vec<i64>,
    /// Rooms of v2 channels whose metadata still names v1: (chat_identifier, v2 id).
    pub restamps: Vec<(String, String)>,
    /// Rooms whose metadata would not parse, kept untouched.
    pub unreadable_rooms: Vec<String>,
}

impl PurgePlan {
    pub fn is_empty(&self) -> bool {
        self.flips.is_empty() && self.doomed_communities.is_empty() && self.doomed_rooms.is_empty() && self.restamps.is_empty()
    }
}

fn table_exists(conn: &Connection, table: &str) -> bool {
    conn.query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1", params![table], |_| Ok(()))
        .optional()
        .ok()
        .flatten()
        .is_some()
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2",
        params![table, column],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

fn strings(conn: &Connection, sql: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

fn pairs(conn: &Connection, sql: &str) -> Result<Vec<(String, String)>, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

/// The room's community id and protocol stamp, or `Err` when its metadata won't parse.
fn room_stamp(metadata: &str) -> Result<(Option<String>, Option<String>), ()> {
    let v: serde_json::Value = serde_json::from_str(metadata).map_err(|_| ())?;
    let obj = v.as_object().ok_or(())?;
    let field = |k: &str| obj.get("custom_fields").and_then(|cf| cf.get(k)).and_then(|s| s.as_str()).map(str::to_string);
    Ok((field("community_id"), field("proto_version")))
}

/// Compute the purge. Reads only; nothing in the main database changes.
pub fn plan_v1_purge(conn: &Connection) -> Result<PurgePlan, String> {
    let mut plan = PurgePlan::default();
    // Without a protocol column nothing can be told apart, so nothing is touched.
    if !table_exists(conn, "communities") || !column_exists(conn, "communities", "protocol") {
        crate::log_warn!("[v1-purge] no protocol column: skipped");
        return Ok(plan);
    }
    let v1_ids: BTreeSet<String> =
        strings(conn, "SELECT community_id FROM communities WHERE protocol IS NULL OR protocol = 1")?.into_iter().collect();
    let v2_ids: BTreeSet<String> =
        strings(conn, "SELECT community_id FROM communities WHERE protocol = 2")?.into_iter().collect();
    if v1_ids.is_empty() {
        return Ok(plan);
    }

    // A twin's join seeds a vsk-2 head (entity = channel id) for every channel before the
    // v2 save skips the v1-owned row, so the heads pair a channel with its twin.
    let mut votes: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    if table_exists(conn, "community_edition_heads") {
        let mut stmt = conn
            .prepare(
                "SELECT ch.community_id, h.community_id, COUNT(DISTINCT ch.channel_id)
                   FROM community_channels ch
                   JOIN community_edition_heads h ON h.entity_id = ch.channel_id
                  GROUP BY ch.community_id, h.community_id",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        for (v1, v2, n) in rows {
            if v1_ids.contains(&v1) && v2_ids.contains(&v2) {
                votes.entry(v1).or_default().insert(v2, n);
            }
        }
    }
    // The ledger is this device's own record of the pairing, so it outranks the heads.
    let mut ledger: BTreeMap<String, String> = BTreeMap::new();
    if table_exists(conn, "community_migrations") {
        for (v1, v2) in pairs(conn, "SELECT community_id, v2_community_id FROM community_migrations")? {
            if v1_ids.contains(&v1) && v2_ids.contains(&v2) {
                ledger.insert(v1, v2);
            }
        }
    }
    let twin_of = |v1: &str| -> Option<String> {
        if let Some(v2) = ledger.get(v1) {
            return Some(v2.clone());
        }
        // Heads in two twins: the one naming the most channels, then the lowest id.
        let mut best: Option<(&String, i64)> = None;
        for (v2, n) in votes.get(v1)? {
            if best.is_none_or(|(_, top)| *n > top) {
                best = Some((v2, *n));
            }
        }
        best.map(|(v2, _)| v2.clone())
    };
    let channels_of = |id: &str| -> Result<Vec<String>, String> {
        let mut stmt = conn
            .prepare("SELECT channel_id FROM community_channels WHERE community_id = ?1")
            .map_err(|e| e.to_string())?;
        let rows = stmt.query_map(params![id], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    };

    // Channel → the v2 community it belongs to once the flips land.
    let mut v2_parent: BTreeMap<String, String> = BTreeMap::new();
    for v2 in &v2_ids {
        for ch in channels_of(v2)? {
            v2_parent.insert(ch, v2.clone());
        }
    }
    let mut doomed_channels: BTreeSet<String> = BTreeSet::new();
    for v1 in &v1_ids {
        plan.doomed_communities.push(v1.clone());
        let owned = channels_of(v1)?;
        match twin_of(v1) {
            Some(v2) if !owned.is_empty() => {
                for ch in owned {
                    v2_parent.insert(ch, v2.clone());
                }
                plan.flips.push((v1.clone(), v2));
            }
            _ => doomed_channels.extend(owned),
        }
    }
    let doomed: BTreeSet<&str> = plan.doomed_communities.iter().map(String::as_str).collect();

    let mut stmt = conn
        .prepare("SELECT id, chat_identifier, chat_type, metadata FROM chats WHERE chat_type = 2")
        .map_err(|e| e.to_string())?;
    let rooms = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(3)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for (row, ident, metadata) in rooms {
        if let Some(parent) = v2_parent.get(&ident) {
            if let Ok((cid, proto)) = room_stamp(&metadata) {
                let names_v1 = cid.is_some_and(|c| c != *parent) || proto.is_some_and(|p| p != "2");
                if names_v1 {
                    plan.restamps.push((ident, parent.clone()));
                }
            }
            continue;
        }
        if doomed_channels.contains(&ident) {
            plan.doomed_rooms.push(row);
            continue;
        }
        match room_stamp(&metadata) {
            Ok((Some(cid), _)) if doomed.contains(cid.as_str()) => plan.doomed_rooms.push(row),
            // Stamped v1 by a community no longer held: nothing can serve it.
            Ok((Some(cid), Some(proto))) if proto == "1" && !v2_ids.contains(&cid) => plan.doomed_rooms.push(row),
            Ok((Some(_), _)) => {}
            Ok((None, _)) => plan.doomed_rooms.push(row),
            Err(()) => {
                crate::log_warn!("[v1-purge] unreadable room metadata, kept: {}", short(&ident));
                plan.unreadable_rooms.push(ident);
            }
        }
    }
    Ok(plan)
}

/// Re-stamp a room's metadata to its v2 parent, keeping every other field.
fn restamped(metadata: &str, v2: &str) -> Option<String> {
    let mut v: serde_json::Value = serde_json::from_str(metadata).ok()?;
    let cf = v
        .as_object_mut()?
        .entry("custom_fields")
        .or_insert_with(|| serde_json::Value::Object(Default::default()))
        .as_object_mut()?;
    cf.insert("community_id".into(), serde_json::Value::String(v2.to_string()));
    cf.insert("proto_version".into(), serde_json::Value::String("2".into()));
    serde_json::to_string(&v).ok()
}

/// Merge `community_list_json`'s tombstones into the v2 mirror, keeping the newest per id.
fn seed_tombstone_mirror(conn: &Connection) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    struct Tombstone {
        community_id: String,
        removed_at: u64,
    }
    let get = |key: &str| -> Result<Option<String>, String> {
        conn.query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())
    };
    let Some(raw) = get("community_list_json")? else { return Ok(()) };
    let Ok(list) = serde_json::from_str::<serde_json::Value>(&raw) else {
        // Set aside, not lost with the key it lived under.
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('community_list_json_unread', ?1)",
            params![raw],
        )
        .map_err(|e| e.to_string())?;
        return Ok(());
    };
    // Entry by entry: one malformed tombstone must not cost the rest.
    let tombstones: Vec<Tombstone> = list
        .get("tombstones")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|t| serde_json::from_value(t).ok())
        .collect();
    let mut mirror: BTreeMap<String, u64> = match get(TOMBSTONE_MIRROR_KEY)? {
        Some(held) => match serde_json::from_str(&held) {
            Ok(m) => m,
            Err(_) => {
                conn.execute(
                    "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                    params![format!("{TOMBSTONE_MIRROR_KEY}_unread"), held],
                )
                .map_err(|e| e.to_string())?;
                BTreeMap::new()
            }
        },
        None => BTreeMap::new(),
    };
    for t in tombstones {
        let at = mirror.entry(t.community_id.to_ascii_lowercase()).or_insert(0);
        *at = (*at).max(t.removed_at);
    }
    let json = serde_json::to_string(&mirror).map_err(|e| e.to_string())?;
    conn.execute("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)", params![TOMBSTONE_MIRROR_KEY, json])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// A room changing parent keeps what it inherited: the old parent's level and mute land on the
/// room itself where it has none of its own. Never on the new parent, whose other rooms it would
/// silence.
fn carry_parent_prefs(conn: &Connection, room: &str, old_parent: &str) -> Result<(), String> {
    if !table_exists(conn, "notify_prefs") {
        return Ok(());
    }
    let read = |scope: &str| -> Result<Option<(Option<i64>, i64, Option<i64>)>, String> {
        conn.query_row(
            "SELECT level, mute_until, suppress_everyone FROM notify_prefs WHERE scope_id = ?1",
            params![scope],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| e.to_string())
    };
    let Some((parent_level, parent_mute, _)) = read(old_parent)? else { return Ok(()) };
    let (own_level, own_mute, own_suppress) = read(room)?.unwrap_or((None, crate::notify::MUTE_OFF, None));
    let level = own_level.or(parent_level);
    let mute = if crate::notify::is_muted_at(own_mute, crate::notify::now_ms()) { own_mute } else { parent_mute };
    if level == own_level && mute == own_mute {
        return Ok(());
    }
    conn.execute(
        "INSERT OR REPLACE INTO notify_prefs (scope_id, level, mute_until, suppress_everyone) VALUES (?1, ?2, ?3, ?4)",
        params![room, level, mute, own_suppress],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

const PURGED_MUTES_KEY: &str = "purged_muted_chats";

/// The removed rooms the synced Mutes list carried, so this device keeps publishing them for a
/// sibling that still holds the rooms. Mirrors `notify::in_legacy_mute_list` on stored rows.
fn record_purged_mutes(conn: &Connection) -> Result<(), String> {
    let mut stmt = conn
        .prepare(
            "SELECT c.chat_identifier, c.muted, c.metadata,
                    (SELECT mute_until FROM notify_prefs WHERE scope_id = c.chat_identifier)
               FROM chats c WHERE c.id IN (SELECT id FROM temp.purge_rooms)",
        )
        .map_err(|e| e.to_string())?;
    let rooms = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0, r.get::<_, String>(2)?, r.get::<_, Option<i64>>(3)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);
    let parent_mute = |cid: &str| -> Option<i64> {
        conn.query_row("SELECT mute_until FROM notify_prefs WHERE scope_id = ?1", params![cid], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    };
    let now = crate::notify::now_ms();
    let mut muted = load_purged_mutes(conn);
    for (ident, legacy_flag, metadata, own) in rooms {
        let own = own.unwrap_or(crate::notify::MUTE_OFF);
        let parent = room_stamp(&metadata).ok().and_then(|(cid, _)| cid).and_then(|c| parent_mute(&c)).unwrap_or(crate::notify::MUTE_OFF);
        let any_muted = crate::notify::is_muted_at(own, now) || crate::notify::is_muted_at(parent, now);
        let forever = own == crate::notify::MUTE_FOREVER || parent == crate::notify::MUTE_FOREVER;
        if (any_muted && forever) || (legacy_flag && !any_muted) {
            muted.insert(ident);
        }
    }
    let json = serde_json::to_string(&muted).map_err(|e| e.to_string())?;
    conn.execute("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)", params![PURGED_MUTES_KEY, json])
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn load_purged_mutes(conn: &Connection) -> BTreeSet<String> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", params![PURGED_MUTES_KEY], |r| r.get::<_, String>(0))
        .optional()
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Rooms the purge removed that the synced Mutes list still carries.
pub fn purged_muted_chats() -> Vec<String> {
    crate::db::get_db_connection_guard_static().map(|c| load_purged_mutes(&c).into_iter().collect()).unwrap_or_default()
}

/// Forget the purged rooms a received Mutes list no longer names: a sibling unmuted them.
pub fn retain_purged_mutes(keep: impl Fn(&str) -> bool) {
    let Ok(conn) = crate::db::get_write_connection_guard_static() else { return };
    let mut muted = load_purged_mutes(&conn);
    let before = muted.len();
    muted.retain(|id| keep(id));
    if muted.len() != before {
        if let Ok(json) = serde_json::to_string(&muted) {
            let _ = conn.execute("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)", params![PURGED_MUTES_KEY, json]);
        }
    }
}

/// Carry out `plan` on `conn`, which the caller holds inside one transaction.
pub fn apply_v1_purge(conn: &Connection, plan: &PurgePlan) -> Result<(), String> {
    // Old keys must not linger in free pages; the connection's own setting comes back after.
    let secure: i64 = conn.query_row("PRAGMA secure_delete", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA secure_delete = ON").map_err(|e| e.to_string())?;
    let result = apply(conn, plan);
    let restore = match secure {
        0 => "OFF",
        2 => "FAST",
        _ => "ON",
    };
    let _ = conn.execute_batch(&format!("PRAGMA secure_delete = {restore}"));
    result
}

fn apply(conn: &Connection, plan: &PurgePlan) -> Result<(), String> {
    let exec = |sql: &str| conn.execute_batch(sql).map_err(|e| format!("{e}: {sql}"));

    for (v1, v2) in &plan.flips {
        conn.execute("UPDATE community_channels SET community_id = ?2 WHERE community_id = ?1", params![v1, v2])
            .map_err(|e| e.to_string())?;
    }
    for (ident, v2) in &plan.restamps {
        let metadata: Option<String> = conn
            .query_row("SELECT metadata FROM chats WHERE chat_identifier = ?1", params![ident], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(next) = metadata.as_deref().and_then(|m| restamped(m, v2)) {
            if let Some(old) = metadata.as_deref().and_then(|m| room_stamp(m).ok()).and_then(|(cid, _)| cid) {
                carry_parent_prefs(conn, ident, &old)?;
            }
            conn.execute("UPDATE chats SET metadata = ?1 WHERE chat_identifier = ?2", params![next, ident])
                .map_err(|e| e.to_string())?;
        }
    }

    exec(
        "CREATE TEMP TABLE IF NOT EXISTS purge_communities (id TEXT PRIMARY KEY);
         CREATE TEMP TABLE IF NOT EXISTS purge_rooms (id INTEGER PRIMARY KEY);
         DELETE FROM purge_communities;
         DELETE FROM purge_rooms;",
    )?;
    for id in &plan.doomed_communities {
        conn.execute("INSERT OR IGNORE INTO temp.purge_communities (id) VALUES (?1)", params![id]).map_err(|e| e.to_string())?;
    }
    for row in &plan.doomed_rooms {
        conn.execute("INSERT OR IGNORE INTO temp.purge_rooms (id) VALUES (?1)", params![row]).map_err(|e| e.to_string())?;
    }

    // Through the events, before they go: afterwards these keys are unattributable.
    if table_exists(conn, "community_message_keys") {
        exec(
            "DELETE FROM community_message_keys WHERE message_id IN
               (SELECT id FROM events WHERE chat_id IN (SELECT id FROM temp.purge_rooms))",
        )?;
    }
    if column_exists(conn, "nip17_wrap_keys", "pending_id") {
        exec("DELETE FROM nip17_wrap_keys WHERE pending_id LIKE 'community-invite-%'")?;
    }
    // Notify prefs stay: the synced Notify and Mutes lists are projections of local state, and a
    // sibling still holding these rooms must not hear them unmute.
    if table_exists(conn, "notify_prefs") {
        for (v1, v2) in &plan.flips {
            conn.execute(
                "INSERT OR IGNORE INTO notify_prefs (scope_id, level, mute_until, suppress_everyone)
                 SELECT ?2, level, mute_until, suppress_everyone FROM notify_prefs WHERE scope_id = ?1",
                params![v1, v2],
            )
            .map_err(|e| e.to_string())?;
        }
        record_purged_mutes(conn)?;
    }
    if table_exists(conn, "attachments") {
        exec(
            "DELETE FROM attachments WHERE event_id IN
               (SELECT id FROM events WHERE chat_id IN (SELECT id FROM temp.purge_rooms))",
        )?;
    }
    exec("DELETE FROM events WHERE chat_id IN (SELECT id FROM temp.purge_rooms)")?;
    exec("DELETE FROM chats WHERE id IN (SELECT id FROM temp.purge_rooms)")?;
    for table in PER_COMMUNITY_TABLES {
        if table_exists(conn, table) {
            exec(&format!("DELETE FROM {table} WHERE community_id IN (SELECT id FROM temp.purge_communities)"))?;
        }
    }
    exec("DELETE FROM communities WHERE community_id IN (SELECT id FROM temp.purge_communities)")?;

    seed_tombstone_mirror(conn)?;
    for key in V1_SETTINGS {
        conn.execute("DELETE FROM settings WHERE key = ?1", params![key]).map_err(|e| e.to_string())?;
    }
    exec("DROP TABLE IF EXISTS temp.purge_communities; DROP TABLE IF EXISTS temp.purge_rooms;")?;
    crate::log_info!(
        "[v1-purge] {} flips finished, {} communities and {} rooms removed, {} restamped",
        plan.flips.len(),
        plan.doomed_communities.len(),
        plan.doomed_rooms.len(),
        plan.restamps.len()
    );
    Ok(())
}

/// The tables and columns only v1 read. Runs after the purge, never before it: the purge
/// pairs twins through `community_migrations`.
const V1_COLUMNS: &[(&str, &str)] = &[
    ("communities", "owner_attestation"),
    ("communities", "read_cut_pending"),
    ("communities", "read_cut_target_epoch"),
    ("communities", "migrated_to"),
    ("communities", "migration_pointer"),
    ("communities", "migration_checked"),
    ("community_channels", "rekeyed_at_server_epoch"),
];

/// Drop what [`V1_COLUMNS`] names, and the migration ledger table. Presence-checked, so a
/// database that never gained one of them passes over it.
pub fn drop_v1_columns(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("DROP TABLE IF EXISTS community_migrations").map_err(|e| e.to_string())?;
    for (table, column) in V1_COLUMNS {
        if column_exists(conn, table, column) {
            conn.execute_batch(&format!("ALTER TABLE {table} DROP COLUMN {column}"))
                .map_err(|e| format!("drop {table}.{column}: {e}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        const B: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as usize;
        let mut account = String::from("npub1");
        for i in 0..58 {
            account.push(B[(i * 13 + 9 + n) % 32] as char);
        }
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        restore_v1_shape(&crate::db::get_write_connection_guard_static().unwrap());
        (tmp, guard)
    }

    /// The v1 columns and ledger as a database before 103 holds them; a test database opens past 104.
    fn restore_v1_shape(conn: &Connection) {
        conn.execute_batch(
            "ALTER TABLE communities ADD COLUMN owner_attestation TEXT;
             ALTER TABLE communities ADD COLUMN read_cut_pending INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE communities ADD COLUMN read_cut_target_epoch INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE communities ADD COLUMN migrated_to TEXT;
             ALTER TABLE communities ADD COLUMN migration_pointer TEXT;
             ALTER TABLE communities ADD COLUMN migration_checked INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE community_channels ADD COLUMN rekeyed_at_server_epoch INTEGER NOT NULL DEFAULT 0;
             CREATE TABLE community_migrations (
                 community_id    TEXT PRIMARY KEY,
                 v2_community_id TEXT NOT NULL,
                 phase           INTEGER NOT NULL DEFAULT 0,
                 twin            TEXT NOT NULL DEFAULT '',
                 updated_at      INTEGER NOT NULL DEFAULT 0
             );",
        )
        .unwrap();
    }

    fn id(b: u8) -> String {
        format!("{b:02x}").repeat(32)
    }

    struct Fixture<'a>(&'a Connection);

    impl Fixture<'_> {
        fn community(&self, cid: &str, protocol: i64) -> &Self {
            self.0
                .execute(
                    "INSERT INTO communities (community_id, server_root_key, name, relays, created_at, protocol)
                     VALUES (?1, x'00', 'n', '[]', 1, ?2)",
                    params![cid, protocol],
                )
                .unwrap();
            self
        }
        fn set(&self, cid: &str, column: &str, value: &str) -> &Self {
            self.0
                .execute(&format!("UPDATE communities SET {column} = ?2 WHERE community_id = ?1"), params![cid, value])
                .unwrap();
            self
        }
        fn channel(&self, ch: &str, cid: &str) -> &Self {
            self.0
                .execute(
                    "INSERT INTO community_channels (channel_id, community_id, channel_key, epoch, name, created_at)
                     VALUES (?1, ?2, x'00', 0, 'c', 1)",
                    params![ch, cid],
                )
                .unwrap();
            self
        }
        fn head(&self, cid: &str, entity: &str) -> &Self {
            self.0
                .execute(
                    "INSERT INTO community_edition_heads (community_id, entity_id, version, self_hash) VALUES (?1, ?2, 1, x'00')",
                    params![cid, entity],
                )
                .unwrap();
            self
        }
        fn ledger(&self, v1: &str, v2: &str) -> &Self {
            self.0
                .execute("INSERT INTO community_migrations (community_id, v2_community_id) VALUES (?1, ?2)", params![v1, v2])
                .unwrap();
            self
        }
        /// A room with one message, an attachment on it and its retained delete key.
        fn room(&self, ident: &str, chat_type: i64, metadata: &str) -> i64 {
            self.0
                .execute(
                    "INSERT INTO chats (chat_identifier, chat_type, participants, created_at, metadata) VALUES (?1, ?2, '[]', 1, ?3)",
                    params![ident, chat_type, metadata],
                )
                .unwrap();
            let row = self.0.last_insert_rowid();
            let ev = format!("ev-{ident}");
            self.0
                .execute(
                    "INSERT INTO events (id, kind, chat_id, content, created_at, received_at) VALUES (?1, 14, ?2, 'hi', 1, 1)",
                    params![ev, row],
                )
                .unwrap();
            self.0
                .execute("INSERT INTO attachments (event_id, att_index, hash) VALUES (?1, 0, 'h')", params![ev])
                .unwrap();
            self.0
                .execute(
                    "INSERT INTO community_message_keys (outer_event_id, ephemeral_secret, relays, created_at, message_id)
                     VALUES (?1, x'00', '[]', 1, ?2)",
                    params![format!("outer-{ident}"), ev],
                )
                .unwrap();
            self.0
                .execute("INSERT INTO notify_prefs (scope_id, mute_until) VALUES (?1, -1)", params![ident])
                .unwrap();
            row
        }
        fn side_rows(&self, cid: &str) -> &Self {
            self.0
                .execute(
                    "INSERT INTO community_epoch_keys (community_id, scope_id, epoch, key, created_at) VALUES (?1, 's', 0, x'00', 1)",
                    params![cid],
                )
                .unwrap();
            self.0.execute("INSERT INTO community_bans (community_id, npub) VALUES (?1, 'x')", params![cid]).unwrap();
            self.0.execute("INSERT INTO notify_prefs (scope_id, mute_until) VALUES (?1, -1)", params![cid]).unwrap();
            self
        }
    }

    fn meta(cid: Option<&str>, proto: &str) -> String {
        match cid {
            Some(c) => format!(r#"{{"custom_fields":{{"community_id":"{c}","proto_version":"{proto}","name":"keep me"}}}}"#),
            None => r#"{"custom_fields":{"name":"bare"}}"#.to_string(),
        }
    }

    fn count(conn: &Connection, sql: &str, p: &str) -> i64 {
        conn.query_row(sql, params![p], |r| r.get(0)).unwrap()
    }

    fn room_exists(conn: &Connection, ident: &str) -> bool {
        count(conn, "SELECT COUNT(*) FROM chats WHERE chat_identifier = ?1", ident) == 1
            && count(conn, "SELECT COUNT(*) FROM events WHERE id = 'ev-' || ?1", ident) == 1
            && count(conn, "SELECT COUNT(*) FROM attachments WHERE event_id = 'ev-' || ?1", ident) == 1
    }

    fn room_gone(conn: &Connection, ident: &str) -> bool {
        count(conn, "SELECT COUNT(*) FROM chats WHERE chat_identifier = ?1", ident) == 0
            && count(conn, "SELECT COUNT(*) FROM events WHERE id = 'ev-' || ?1", ident) == 0
            && count(conn, "SELECT COUNT(*) FROM attachments WHERE event_id = 'ev-' || ?1", ident) == 0
            && count(conn, "SELECT COUNT(*) FROM community_message_keys WHERE message_id = 'ev-' || ?1", ident) == 0
    }

    fn parent(conn: &Connection, ch: &str) -> Option<String> {
        conn.query_row("SELECT community_id FROM community_channels WHERE channel_id = ?1", params![ch], |r| r.get(0))
            .optional()
            .unwrap()
    }

    fn stamp(conn: &Connection, ident: &str) -> serde_json::Value {
        let m: String = conn.query_row("SELECT metadata FROM chats WHERE chat_identifier = ?1", params![ident], |r| r.get(0)).unwrap();
        serde_json::from_str(&m).unwrap()
    }

    fn purge(conn: &Connection) -> PurgePlan {
        let tx = conn.unchecked_transaction().unwrap();
        let plan = plan_v1_purge(&tx).unwrap();
        apply_v1_purge(&tx, &plan).unwrap();
        tx.commit().unwrap();
        plan
    }

    #[test]
    fn the_purge_keeps_v2_whole_and_takes_only_v1() {
        let (_tmp, _guard) = init_test_db();
        let conn = crate::db::get_write_connection_guard_static().unwrap();
        let f = Fixture(&conn);

        // Migrated and finalized: rooms already on the twin; the v1 husk goes, pointer or not.
        let (fin_v1, fin_v2, fin_ch) = (id(0x10), id(0x11), id(0x12));
        f.community(&fin_v1, 1).set(&fin_v1, "migrated_to", &fin_v2).set(&fin_v1, "migration_pointer", "sealed").side_rows(&fin_v1);
        f.ledger(&fin_v1, &fin_v2);
        f.community(&fin_v2, 2).channel(&fin_ch, &fin_v2).head(&fin_v2, &fin_ch).side_rows(&fin_v2);
        f.room(&fin_ch, 2, &meta(Some(&fin_v2), "2"));

        // A member's husk: flipped, pointer still set, no ledger.
        let husk_m = id(0x13);
        f.community(&husk_m, 1).set(&husk_m, "migrated_to", &fin_v2).set(&husk_m, "migration_pointer", "sealed").side_rows(&husk_m);

        // Migrated, but the rooms' stamps still name v1.
        let (stale_v1, stale_v2, stale_ch) = (id(0x20), id(0x21), id(0x22));
        f.community(&stale_v1, 1).set(&stale_v1, "migrated_to", &stale_v2);
        f.community(&stale_v2, 2).channel(&stale_ch, &stale_v2);
        f.room(&stale_ch, 2, &meta(Some(&stale_v1), "1"));

        // Held twin: twin row and heads, but the channel still sits on the v1 row.
        let (held_v1, held_v2, held_ch) = (id(0x30), id(0x31), id(0x32));
        f.community(&held_v1, 1).channel(&held_ch, &held_v1).set(&held_v1, "migration_pointer", "sealed");
        conn.execute("INSERT INTO notify_prefs (scope_id, level, mute_until) VALUES (?1, 2, -1)", params![held_v1]).unwrap();
        f.community(&held_v2, 2).head(&held_v2, &held_ch);
        f.room(&held_ch, 2, &meta(Some(&held_v1), "1"));

        // Owner's wizard: a ledger row and a twin, channel still on v1.
        let (own_v1, own_v2, own_ch) = (id(0x40), id(0x41), id(0x42));
        f.community(&own_v1, 1).channel(&own_ch, &own_v1).ledger(&own_v1, &own_v2);
        f.community(&own_v2, 2);
        f.room(&own_ch, 2, &meta(Some(&own_v1), "1"));

        // Pointed at a twin we never got: gone like any unmigrated community.
        let (lone_v1, lone_ch) = (id(0x50), id(0x52));
        f.community(&lone_v1, 1).channel(&lone_ch, &lone_v1).set(&lone_v1, "migration_pointer", "sealed");
        f.room(&lone_ch, 2, &meta(Some(&lone_v1), "1"));
        // A ledger row naming a twin that isn't held: gone too.
        let (wiz_v1, wiz_ch) = (id(0x58), id(0x59));
        f.community(&wiz_v1, 1).channel(&wiz_ch, &wiz_v1).ledger(&wiz_v1, &id(0x5A));
        f.room(&wiz_ch, 2, &meta(None, ""));

        // Two twins claim it: the one naming more of its channels takes them all.
        let (amb_v1, amb_a, amb_b, amb_ch, amb_ch2) = (id(0x5B), id(0x5C), id(0x5D), id(0x5E), id(0x5F));
        f.community(&amb_v1, 1).channel(&amb_ch, &amb_v1).channel(&amb_ch2, &amb_v1);
        f.community(&amb_a, 2).head(&amb_a, &amb_ch);
        f.community(&amb_b, 2).head(&amb_b, &amb_ch).head(&amb_b, &amb_ch2);
        f.room(&amb_ch, 2, &meta(Some(&amb_v1), "1"));
        f.room(&amb_ch2, 2, &meta(Some(&amb_v1), "1"));
        // An even split goes to the lowest id.
        let (tie_v1, tie_a, tie_b, tie_ch, tie_ch2) = (id(0x57), id(0x86), id(0x87), id(0x88), id(0x89));
        f.community(&tie_v1, 1).channel(&tie_ch, &tie_v1).channel(&tie_ch2, &tie_v1);
        f.community(&tie_b, 2).head(&tie_b, &tie_ch);
        f.community(&tie_a, 2).head(&tie_a, &tie_ch2);
        f.room(&tie_ch, 2, &meta(Some(&tie_v1), "1"));
        // The ledger outranks heads naming another twin.
        let (led_v1, led_x, led_y, led_ch) = (id(0x53), id(0x54), id(0x55), id(0x56));
        f.community(&led_v1, 1).channel(&led_ch, &led_v1).ledger(&led_v1, &led_x);
        f.community(&led_x, 2);
        f.community(&led_y, 2).head(&led_y, &led_ch);
        f.room(&led_ch, 2, &meta(Some(&led_v1), "1"));

        // Never migrated, still chatting: gone, with everything it owns.
        let (old_v1, old_ch, old_ch2) = (id(0x60), id(0x61), id(0x62));
        f.community(&old_v1, 1).channel(&old_ch, &old_v1).channel(&old_ch2, &old_v1).side_rows(&old_v1);
        f.room(&old_ch, 2, &meta(Some(&old_v1), "1"));
        f.room(&old_ch2, 2, &meta(None, ""));
        // A room whose channel row is gone but whose stamp names the doomed community.
        let stray = id(0x63);
        f.room(&stray, 2, &meta(Some(&old_v1), "1"));

        // Sealed by its tombstone, never migrated.
        let (husk_v1, husk_ch) = (id(0x64), id(0x65));
        f.community(&husk_v1, 1).set(&husk_v1, "dissolved", "1").channel(&husk_ch, &husk_v1);
        f.room(&husk_ch, 2, &meta(Some(&husk_v1), "1"));
        // Another v1 row's head never pairs: only a protocol-2 twin can claim a channel.
        let (cross_v1, cross_other, cross_ch) = (id(0x66), id(0x67), id(0x68));
        f.community(&cross_v1, 1).channel(&cross_ch, &cross_v1);
        f.community(&cross_other, 1).head(&cross_other, &cross_ch);
        f.room(&cross_ch, 2, &meta(Some(&cross_v1), "1"));
        for (cid, row) in [(&old_v1, 1), (&id(0x69), 2), (&id(0x6A), 3)] {
            conn.execute(
                "INSERT INTO pending_community_invites (community_id, bundle_json, inviter_npub, received_at) VALUES (?1, '{}', 'i', ?2)",
                params![cid, row],
            )
            .unwrap();
        }

        // v2-native, untouched.
        let (nat_v2, nat_ch) = (id(0x70), id(0x71));
        f.community(&nat_v2, 2).channel(&nat_ch, &nat_v2).side_rows(&nat_v2);
        f.room(&nat_ch, 2, &meta(Some(&nat_v2), "2"));
        // A v2 room with no stamp: its channel row is what counts.
        let nat_bare = id(0x72);
        f.channel(&nat_bare, &nat_v2);
        f.room(&nat_bare, 2, r#"{}"#);

        // A bare community room nothing owns.
        let bare = id(0x80);
        f.room(&bare, 2, &meta(None, ""));
        // A v1 room naming a community we don't hold (left long ago).
        let orphan = id(0x81);
        f.room(&orphan, 2, &meta(Some(&id(0x82)), "1"));
        // A v2 room naming one we don't hold: v2's business, not the purge's.
        let orphan_v2 = id(0x8A);
        f.room(&orphan_v2, 2, &meta(Some(&id(0x8B)), "2"));
        // A held v2 community's room with no channel row and a stale v1 stamp.
        let unrowed = id(0x8C);
        f.room(&unrowed, 2, &meta(Some(&nat_v2), "1"));
        // Unreadable metadata, unowned.
        let garbled = id(0x83);
        f.room(&garbled, 2, "{not json");
        // A DM.
        let dm = "npub1dm".to_string();
        f.room(&dm, 0, r#"{"custom_fields":{}}"#);

        conn.execute("INSERT INTO deleted_messages (event_id, deleted_at) VALUES ('gone', 1)", []).unwrap();
        conn.execute(
            "INSERT INTO nip17_wrap_keys (wrap_event_id, rumor_id, recipient_pubkey, role, secret, relay_urls, created_at, pending_id)
             VALUES ('w1', 'r', 'p', 0, x'00', '[]', 1, 'community-invite-abc'), ('w2', 'r', 'p', 0, x'00', '[]', 1, 'dm-xyz')",
            [],
        )
        .unwrap();
        crate::db::settings::set_sql_setting(
            "community_list_json".into(),
            format!(r#"{{"entries":[],"tombstones":[{{"community_id":"{}","removed_at":9000}}]}}"#, id(0x90)),
        )
        .unwrap();
        crate::db::settings::set_sql_setting("invite_list_json".into(), "{}".into()).unwrap();

        let snapshot = |conn: &Connection| -> Vec<String> {
            let mut out = Vec::new();
            for sql in [
                "SELECT id || kind || chat_id || content || tags || created_at || received_at FROM events ORDER BY id",
                "SELECT channel_id || community_id || hex(channel_key) || epoch || name FROM community_channels ORDER BY channel_id",
                "SELECT event_id || att_index || hash FROM attachments ORDER BY event_id",
            ] {
                let mut stmt = conn.prepare(sql).unwrap();
                out.extend(stmt.query_map([], |r| r.get::<_, String>(0)).unwrap().map(Result::unwrap));
            }
            out
        };
        let v2_rooms = [&fin_ch, &nat_ch, &nat_bare];
        let before: Vec<String> =
            snapshot(&conn).into_iter().filter(|r| v2_rooms.iter().any(|ch| r.contains(ch.as_str()))).collect();

        // Rooms whose own mute is gone: one inherits its community's, one has none to inherit.
        conn.execute("DELETE FROM notify_prefs WHERE scope_id IN (?1, ?2)", params![old_ch2, stray]).unwrap();
        // A restamped room that only inherited its v1 community's mute.
        conn.execute("DELETE FROM notify_prefs WHERE scope_id = ?1", params![stale_ch]).unwrap();
        conn.execute("INSERT INTO notify_prefs (scope_id, level, mute_until) VALUES (?1, 1, -1)", params![stale_v1]).unwrap();
        // The legacy flag alone, and a mute that will run out.
        let (flagged, timed) = (id(0x84), id(0x85));
        f.channel(&flagged, &old_v1);
        f.room(&flagged, 2, &meta(None, ""));
        f.channel(&timed, &old_v1);
        f.room(&timed, 2, &meta(None, ""));
        conn.execute("DELETE FROM notify_prefs WHERE scope_id IN (?1, ?2)", params![flagged, timed]).unwrap();
        conn.execute("UPDATE chats SET muted = 1 WHERE chat_identifier = ?1", params![flagged]).unwrap();
        conn.execute("INSERT INTO notify_prefs (scope_id, mute_until) VALUES (?1, ?2)", params![timed, crate::notify::now_ms() + 3_600_000]).unwrap();

        let plan = purge(&conn);

        let after: Vec<String> =
            snapshot(&conn).into_iter().filter(|r| v2_rooms.iter().any(|ch| r.contains(ch.as_str()))).collect();
        assert_eq!(before, after, "v2 rooms and their channels are byte-identical");

        assert_eq!(
            plan.flips,
            vec![
                (held_v1.clone(), held_v2.clone()),
                (own_v1.clone(), own_v2.clone()),
                (led_v1.clone(), led_x.clone()),
                (tie_v1.clone(), tie_a.clone()),
                (amb_v1.clone(), amb_b.clone()),
            ]
        );

        for c in [
            &fin_v1, &husk_m, &stale_v1, &held_v1, &own_v1, &old_v1, &husk_v1, &cross_v1, &cross_other, &lone_v1, &wiz_v1,
            &amb_v1, &tie_v1, &led_v1,
        ] {
            assert_eq!(count(&conn, "SELECT COUNT(*) FROM communities WHERE community_id = ?1", c), 0, "{}", short(c));
            for table in PER_COMMUNITY_TABLES {
                let sql = format!("SELECT COUNT(*) FROM {table} WHERE community_id = ?1");
                assert_eq!(count(&conn, &sql, c), 0, "{table} keeps a row for {}", short(c));
            }
        }
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM notify_prefs WHERE scope_id = ?1", &old_v1), 1, "notify prefs stay");
        assert_eq!(
            conn.query_row("SELECT mute_until FROM notify_prefs WHERE scope_id = ?1", params![held_v2], |r| r.get::<_, i64>(0)).unwrap(),
            -1,
            "a flipped community's mute moves to its twin"
        );
        let mutes: BTreeSet<String> = load_purged_mutes(&conn);
        for r in [&old_ch, &stray, &bare, &husk_ch, &cross_ch] {
            assert!(mutes.contains(r.as_str()), "{} stays in the synced Mutes list", short(r));
        }
        assert!(!mutes.contains(old_ch2.as_str()), "a room nothing muted is not added");
        assert!(mutes.contains(flagged.as_str()), "the legacy muted flag alone counts");
        assert!(!mutes.contains(timed.as_str()), "a mute that runs out was never in the Mutes list");
        let carried: (Option<i64>, i64) = conn
            .query_row("SELECT level, mute_until FROM notify_prefs WHERE scope_id = ?1", params![stale_ch], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(carried, (Some(1), -1), "a restamped room keeps the mute it inherited");
        assert!(!mutes.contains(nat_ch.as_str()), "a kept room is not the purge's to record");
        let v1_left: i64 = conn.query_row("SELECT COUNT(*) FROM communities WHERE protocol IS NULL OR protocol = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(v1_left, 0, "no v1 row survives");
        for c in [&fin_v2, &stale_v2, &held_v2, &own_v2, &nat_v2, &amb_a, &amb_b, &tie_a, &tie_b, &led_x, &led_y] {
            assert_eq!(count(&conn, "SELECT COUNT(*) FROM communities WHERE community_id = ?1", c), 1, "{}", short(c));
        }
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM community_epoch_keys WHERE community_id = ?1", &nat_v2), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM community_bans WHERE community_id = ?1", &fin_v2), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM notify_prefs WHERE scope_id = ?1", &nat_v2), 1);

        for r in [&fin_ch, &stale_ch, &held_ch, &own_ch, &nat_ch, &nat_bare, &amb_ch, &amb_ch2, &tie_ch, &led_ch, &orphan_v2, &unrowed, &garbled, &dm] {
            assert!(room_exists(&conn, r), "{} must survive", short(r));
        }
        for r in [&old_ch, &old_ch2, &stray, &bare, &husk_ch, &cross_ch, &flagged, &timed, &lone_ch, &wiz_ch, &orphan] {
            assert!(room_gone(&conn, r), "{} must go", short(r));
        }
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM community_message_keys WHERE message_id = 'ev-' || ?1", &nat_ch), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM notify_prefs WHERE scope_id = ?1", &nat_ch), 1);

        assert_eq!(parent(&conn, &held_ch).as_deref(), Some(held_v2.as_str()), "a held twin's room moves to it");
        assert_eq!(parent(&conn, &own_ch).as_deref(), Some(own_v2.as_str()));
        for (ch, v2) in [(&amb_ch, &amb_b), (&amb_ch2, &amb_b), (&tie_ch, &tie_a), (&tie_ch2, &tie_a), (&led_ch, &led_x)] {
            assert_eq!(parent(&conn, ch).as_deref(), Some(v2.as_str()), "{} moves to {}", short(ch), short(v2));
        }
        for ch in [&lone_ch, &wiz_ch, &old_ch] {
            assert_eq!(parent(&conn, ch), None);
        }
        for (room, v2) in [(&stale_ch, &stale_v2), (&held_ch, &held_v2), (&own_ch, &own_v2), (&amb_ch, &amb_b), (&tie_ch, &tie_a), (&led_ch, &led_x)] {
            let s = stamp(&conn, room);
            assert_eq!(s["custom_fields"]["community_id"], v2.as_str());
            assert_eq!(s["custom_fields"]["proto_version"], "2");
            assert_eq!(s["custom_fields"]["name"], "keep me", "a restamp keeps every other field");
        }
        assert_eq!(stamp(&conn, &nat_bare), serde_json::json!({}), "a v2 room is restamped only when it names v1");

        let invites = |c: &str| count(&conn, "SELECT COUNT(*) FROM pending_community_invites WHERE community_id = ?1", c);
        assert_eq!(invites(&old_v1), 0, "an invite to a removed community goes with it");
        assert_eq!((invites(&id(0x69)), invites(&id(0x6A))), (1, 1), "an invite to a community we don't hold stays");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM deleted_messages WHERE event_id = ?1", "gone"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM nip17_wrap_keys WHERE wrap_event_id = ?1", "w1"), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM nip17_wrap_keys WHERE wrap_event_id = ?1", "w2"), 1);
        let get = |k: &str| crate::db::settings::get_sql_setting(k.to_string()).unwrap();
        assert_eq!(get("community_list_json"), None);
        assert_eq!(get("invite_list_json"), None);
        let mirror: BTreeMap<String, u64> = serde_json::from_str(&get(TOMBSTONE_MIRROR_KEY).unwrap()).unwrap();
        assert_eq!(mirror.get(&id(0x90)), Some(&9000), "the v1 list's removals carry into the v2 mirror");

        let again = {
            let tx = conn.unchecked_transaction().unwrap();
            let p = plan_v1_purge(&tx).unwrap();
            apply_v1_purge(&tx, &p).unwrap();
            tx.commit().unwrap();
            p
        };
        assert!(again.is_empty(), "a second run finds nothing: {again:?}");
        for r in [&fin_ch, &held_ch, &nat_ch, &amb_ch, &orphan_v2, &garbled, &dm] {
            assert!(room_exists(&conn, r));
        }
    }

    #[test]
    fn the_mirror_seed_merges_and_never_lowers() {
        let (_tmp, _guard) = init_test_db();
        let conn = crate::db::get_write_connection_guard_static().unwrap();
        let mirror = format!(r#"{{"{}":5000,"{}":100}}"#, id(0x01), id(0x02));
        crate::db::settings::set_sql_setting(TOMBSTONE_MIRROR_KEY.into(), mirror).unwrap();
        crate::db::settings::set_sql_setting(
            "community_list_json".into(),
            format!(
                r#"{{"tombstones":[{{"community_id":"{}","removed_at":10}},{{"community_id":"{}","removed_at":700}}]}}"#,
                id(0x01),
                id(0x02).to_ascii_uppercase()
            ),
        )
        .unwrap();
        purge(&conn);
        let held: BTreeMap<String, u64> =
            serde_json::from_str(&crate::db::settings::get_sql_setting(TOMBSTONE_MIRROR_KEY.into()).unwrap().unwrap()).unwrap();
        assert_eq!(held.get(&id(0x01)), Some(&5000));
        assert_eq!(held.get(&id(0x02)), Some(&700));
        assert_eq!(held.len(), 2);
    }

    #[test]
    fn an_unreadable_list_or_mirror_loses_nothing() {
        let (_tmp, _guard) = init_test_db();
        let conn = crate::db::get_write_connection_guard_static().unwrap();
        crate::db::settings::set_sql_setting(TOMBSTONE_MIRROR_KEY.into(), "{oops".into()).unwrap();
        crate::db::settings::set_sql_setting(
            "community_list_json".into(),
            format!(r#"{{"tombstones":[{{"community_id":"{}","removed_at":10}}]}}"#, id(0x03)),
        )
        .unwrap();
        purge(&conn);
        let get = |k: &str| crate::db::settings::get_sql_setting(k.to_string()).unwrap();
        assert_eq!(get(&format!("{TOMBSTONE_MIRROR_KEY}_unread")).as_deref(), Some("{oops"));
        assert!(get(TOMBSTONE_MIRROR_KEY).unwrap().contains(&id(0x03)));

        crate::db::settings::set_sql_setting(
            "community_list_json".into(),
            format!(r#"{{"tombstones":[{{"community_id":"{}","removed_at":"late"}},{{"community_id":"{}","removed_at":20}}]}}"#, id(0x04), id(0x05)),
        )
        .unwrap();
        purge(&conn);
        let mirror = get(TOMBSTONE_MIRROR_KEY).unwrap();
        assert!(!mirror.contains(&id(0x04)) && mirror.contains(&id(0x05)), "one bad entry costs only itself");

        crate::db::settings::set_sql_setting("community_list_json".into(), "{broken".into()).unwrap();
        purge(&conn);
        assert_eq!(get("community_list_json_unread").as_deref(), Some("{broken"));
    }

    #[test]
    fn a_database_missing_v1_columns_still_purges() {
        let (_tmp, _guard) = init_test_db();
        let conn = crate::db::get_write_connection_guard_static().unwrap();
        conn.execute_batch(
            "ALTER TABLE communities DROP COLUMN migration_pointer;
             DROP INDEX IF EXISTS idx_nip17_wrap_keys_pending;
             ALTER TABLE nip17_wrap_keys DROP COLUMN pending_id;
             DROP TABLE community_migrations;",
        )
        .unwrap();
        let f = Fixture(&conn);
        let (old_v1, old_ch, nat_v2, nat_ch) = (id(0x60), id(0x61), id(0x70), id(0x71));
        f.community(&old_v1, 1).channel(&old_ch, &old_v1);
        f.room(&old_ch, 2, &meta(Some(&old_v1), "1"));
        f.community(&nat_v2, 2).channel(&nat_ch, &nat_v2);
        f.room(&nat_ch, 2, &meta(Some(&nat_v2), "2"));
        purge(&conn);
        assert!(room_gone(&conn, &old_ch));
        assert!(room_exists(&conn, &nat_ch));
    }

    #[test]
    fn v2_storage_works_once_the_v1_columns_are_gone() {
        let (_tmp, _guard) = init_test_db();
        let owner = nostr_sdk::prelude::Keys::generate();
        let g = crate::community::v2::control::genesis(
            &owner,
            crate::community::v2::control::CommunityMetadata { name: "After".into(), ..Default::default() },
            1_000,
        )
        .unwrap();
        let c = crate::community::v2::community::CommunityV2::from_genesis(&g, "After", None, vec!["wss://r".into()], 1_000);
        crate::db::community::save_community_v2(&c).unwrap();
        {
            let conn = crate::db::get_write_connection_guard_static().unwrap();
            drop_v1_columns(&conn).unwrap();
            drop_v1_columns(&conn).unwrap();
            for (table, column) in V1_COLUMNS {
                assert!(!column_exists(&conn, table, column), "{table}.{column}");
            }
            assert!(!table_exists(&conn, "community_migrations"));
        }
        let loaded = crate::db::community::load_community_v2(c.id()).unwrap().expect("still loads");
        assert_eq!(loaded.name, "After");
        crate::db::community::save_community_v2(&loaded).unwrap();
        let cid = crate::simd::hex::bytes_to_hex_32(&c.id().0);
        crate::db::community::delete_community_retain_keys(&cid).unwrap();
        assert!(crate::db::community::load_community_v2(c.id()).unwrap().is_none());
    }

    #[test]
    fn a_sibling_unmuting_a_purged_room_lets_it_go() {
        let (_tmp, _guard) = init_test_db();
        crate::db::settings::set_sql_setting(PURGED_MUTES_KEY.into(), format!(r#"["{}","{}"]"#, id(0x01), id(0x02))).unwrap();
        assert_eq!(purged_muted_chats(), vec![id(0x01), id(0x02)]);
        retain_purged_mutes(|room| room == id(0x02));
        assert_eq!(purged_muted_chats(), vec![id(0x02)]);
    }

    #[test]
    fn without_a_protocol_column_nothing_is_touched() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE communities (community_id TEXT PRIMARY KEY);
             INSERT INTO communities VALUES ('a');",
        )
        .unwrap();
        assert_eq!(plan_v1_purge(&conn).unwrap(), PurgePlan::default());
    }
}
