//! Per-server latency, speed and reachability, and the one-word status the
//! list shows.
//!
//! One row per server, no history: latency and speed are exponentially
//! weighted averages, the last few upload speeds ride along as a short array
//! for a sparkline. The footprint does not grow with use.
//!
//! Latency is the BUD-06 preflight round-trip — a small request, so it is
//! dominated by distance and server load and comparable between servers.
//! Speed is this device's throughput to the server, which the uplink caps:
//! shown as a number, never graded.

use serde::Serialize;

/// Weight of a new sample against the running average.
const ALPHA: f64 = 0.3;
/// Below this an upload is all round-trip and says nothing about throughput.
pub const MIN_SPEED_SAMPLE_BYTES: u64 = 256 * 1024;
/// Sparkline length.
const RECENT_MAX: usize = 12;
/// A failure this old with nothing since no longer means the server is down.
const OFFLINE_WINDOW_SECS: i64 = 30 * 60;
/// A quota refusal is good for the rest of the day at most.
const MAXED_WINDOW_SECS: i64 = 24 * 3600;

pub const FAIL_OFFLINE: &str = "offline";
pub const FAIL_MAXED: &str = "maxed";

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ServerStats {
    pub latency_ms: Option<f64>,
    pub mbps: Option<f64>,
    pub best_mbps: Option<f64>,
    pub uploads: u64,
    pub bytes_total: u64,
    pub last_ok_at: Option<i64>,
    pub last_fail_at: Option<i64>,
    pub last_fail_kind: Option<String>,
    /// Recent upload speeds in Mbit/s, oldest first.
    pub recent: Vec<f32>,
}

/// The list's one-word answer to "will my next upload here work?".
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ServerStatus {
    /// `online` · `offline` · `maxed` · `no_access` · `disabled` · `untested`
    pub state: &'static str,
    pub label: &'static str,
    /// A `.relay-status` modifier: `connected` · `connecting` · `disconnected` · `disabled`
    pub tone: &'static str,
    pub latency_ms: Option<u32>,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn norm_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_lowercase()
}

fn ewma(prev: Option<f64>, sample: f64) -> f64 {
    match prev {
        Some(p) => p + ALPHA * (sample - p),
        None => sample,
    }
}

// ============================================================================
// Storage
// ============================================================================

pub fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<(), String> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS blossom_server_stats (
            server_url     TEXT    PRIMARY KEY,
            latency_ms     REAL,
            mbps           REAL,
            best_mbps      REAL,
            uploads        INTEGER NOT NULL DEFAULT 0,
            bytes_total    INTEGER NOT NULL DEFAULT 0,
            last_ok_at     INTEGER,
            last_fail_at   INTEGER,
            last_fail_kind TEXT,
            recent         TEXT    NOT NULL DEFAULT '[]',
            updated_at     INTEGER NOT NULL
        );",
    )
    .map_err(|e| format!("Failed to create blossom_server_stats table: {}", e))
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ServerStats> {
    let recent_json: String = row.get(8)?;
    Ok(ServerStats {
        latency_ms: row.get(0)?,
        mbps: row.get(1)?,
        best_mbps: row.get(2)?,
        uploads: row.get::<_, i64>(3)? as u64,
        bytes_total: row.get::<_, i64>(4)? as u64,
        last_ok_at: row.get(5)?,
        last_fail_at: row.get(6)?,
        last_fail_kind: row.get(7)?,
        recent: serde_json::from_str(&recent_json).unwrap_or_default(),
    })
}

const SELECT: &str = "SELECT latency_ms, mbps, best_mbps, uploads, bytes_total, last_ok_at, last_fail_at, last_fail_kind, recent
                        FROM blossom_server_stats WHERE server_url = ?1";

pub fn stats_for(server_url: &str) -> Option<ServerStats> {
    let conn = crate::db::get_db_connection_guard_static().ok()?;
    conn.query_row(SELECT, rusqlite::params![norm_url(server_url)], read_row).ok()
}

fn load_or_default(conn: &rusqlite::Connection, server: &str) -> ServerStats {
    conn.query_row(SELECT, rusqlite::params![server], read_row)
        .unwrap_or(ServerStats {
            latency_ms: None,
            mbps: None,
            best_mbps: None,
            uploads: 0,
            bytes_total: 0,
            last_ok_at: None,
            last_fail_at: None,
            last_fail_kind: None,
            recent: Vec::new(),
        })
}

fn save(conn: &rusqlite::Connection, server: &str, s: &ServerStats) -> Result<(), String> {
    conn.execute(
        "INSERT INTO blossom_server_stats
            (server_url, latency_ms, mbps, best_mbps, uploads, bytes_total, last_ok_at, last_fail_at, last_fail_kind, recent, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(server_url) DO UPDATE SET
            latency_ms = ?2, mbps = ?3, best_mbps = ?4, uploads = ?5, bytes_total = ?6,
            last_ok_at = ?7, last_fail_at = ?8, last_fail_kind = ?9, recent = ?10, updated_at = ?11",
        rusqlite::params![
            server, s.latency_ms, s.mbps, s.best_mbps, s.uploads as i64, s.bytes_total as i64,
            s.last_ok_at, s.last_fail_at, s.last_fail_kind,
            serde_json::to_string(&s.recent).unwrap_or_else(|_| "[]".to_string()),
            now_secs(),
        ],
    )
    .map_err(|e| format!("Failed to save server stats: {}", e))?;
    Ok(())
}

fn update(server_url: &str, f: impl FnOnce(&mut ServerStats)) {
    let Ok(conn) = crate::db::get_write_connection_guard_static() else { return };
    let server = norm_url(server_url);
    let mut s = load_or_default(&conn, &server);
    f(&mut s);
    if let Err(e) = save(&conn, &server, &s) {
        crate::log_warn!("[Blossom Stats] {}: {}", server, e);
    }
}

/// A small request answered in `ms`.
pub fn record_latency(server_url: &str, ms: f64) {
    if ms <= 0.0 { return; }
    update(server_url, |s| {
        s.latency_ms = Some(ewma(s.latency_ms, ms));
        s.last_ok_at = Some(now_secs());
    });
}

/// `bytes` accepted in `ms`. Small uploads are ignored: they measure the
/// round-trip, not the pipe.
pub fn record_upload(server_url: &str, bytes: u64, ms: f64) {
    if bytes < MIN_SPEED_SAMPLE_BYTES || ms <= 0.0 { return; }
    let mbps = (bytes as f64 * 8.0) / (ms / 1000.0) / 1_000_000.0;
    update(server_url, |s| {
        s.mbps = Some(ewma(s.mbps, mbps));
        s.best_mbps = Some(s.best_mbps.map_or(mbps, |b| b.max(mbps)));
        s.uploads += 1;
        s.bytes_total += bytes;
        s.last_ok_at = Some(now_secs());
        s.recent.push(mbps as f32);
        if s.recent.len() > RECENT_MAX {
            let drop = s.recent.len() - RECENT_MAX;
            s.recent.drain(..drop);
        }
    });
}

/// The server answered, whatever it said: it is reachable.
pub fn record_ok(server_url: &str) {
    update(server_url, |s| s.last_ok_at = Some(now_secs()));
}

/// `kind` is [`FAIL_OFFLINE`] or [`FAIL_MAXED`].
pub fn record_failure(server_url: &str, kind: &str) {
    update(server_url, |s| {
        s.last_fail_at = Some(now_secs());
        s.last_fail_kind = Some(kind.to_string());
    });
}

// ============================================================================
// Status
// ============================================================================

/// Pure: what the list should say, from what is known. `info` is the
/// server's document when it has one; `stats` this device's history.
pub fn derive_status(
    enabled: bool,
    info: Option<&crate::blossom_info::ServerInfo>,
    stats: Option<&ServerStats>,
    now: i64,
) -> ServerStatus {
    let latency_ms = stats.and_then(|s| s.latency_ms).map(|l| l.round() as u32);
    let status = |state: &'static str, label: &'static str, tone: &'static str| ServerStatus {
        state, label, tone, latency_ms,
    };
    if !enabled {
        return status("disabled", "Disabled", "disabled");
    }
    if let Some(c) = info.and_then(|i| i.caller.as_ref()) {
        if !c.allowed {
            return status("no_access", "No access", "disconnected");
        }
        let full = |used: u64, limit: u64| limit > 0 && used >= limit;
        if full(c.storage_used, c.storage_limit) || full(c.daily_used, c.daily_limit) {
            return status("maxed", "Maxed", "connecting");
        }
    }
    if let Some(s) = stats {
        let failed_since_ok = match (s.last_fail_at, s.last_ok_at) {
            (Some(f), Some(o)) => f > o,
            (Some(_), None) => true,
            _ => false,
        };
        if failed_since_ok {
            let age = now - s.last_fail_at.unwrap_or(now);
            match s.last_fail_kind.as_deref() {
                Some(FAIL_MAXED) if age <= MAXED_WINDOW_SECS => {
                    return status("maxed", "Maxed", "connecting");
                }
                Some(FAIL_OFFLINE) if age <= OFFLINE_WINDOW_SECS => {
                    return status("offline", "Offline", "disconnected");
                }
                _ => {}
            }
        }
        if s.last_ok_at.is_some() {
            return status("online", "Online", "connected");
        }
    }
    if info.is_some() {
        return status("online", "Online", "connected");
    }
    status("untested", "Untested", "disabled")
}

pub fn status_for(server_url: &str, enabled: bool) -> ServerStatus {
    let info = crate::blossom_info::cached(server_url);
    let stats = stats_for(server_url);
    derive_status(enabled, info.as_ref(), stats.as_ref(), now_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blossom_info::{CallerInfo, ServerInfo};

    fn stats() -> ServerStats {
        ServerStats {
            latency_ms: Some(48.4), mbps: Some(12.0), best_mbps: Some(20.0), uploads: 3,
            bytes_total: 3_000_000, last_ok_at: Some(1000), last_fail_at: None,
            last_fail_kind: None, recent: vec![10.0, 12.0],
        }
    }

    fn doc(caller: Option<CallerInfo>) -> ServerInfo {
        ServerInfo {
            name: None, software: None, version: None, description: None, buds: vec![],
            extensions: vec![], max_blob: None, mime: vec![], capacity_used: None,
            capacity_limit: None, caller, fetched_at: 0,
        }
    }

    fn caller(allowed: bool, storage: (u64, u64), daily: (u64, u64)) -> CallerInfo {
        CallerInfo {
            tier: "recognised".into(), max_blob: 1, storage_used: storage.0, storage_limit: storage.1,
            blobs: 0, daily_used: daily.0, daily_limit: daily.1, allowed, reasons: vec![],
        }
    }

    #[test]
    fn disabled_wins_over_everything() {
        let s = derive_status(false, Some(&doc(Some(caller(false, (0, 1), (0, 1))))), Some(&stats()), 2000);
        assert_eq!(s.state, "disabled");
        assert_eq!(s.latency_ms, Some(48), "the number still rides along");
    }

    #[test]
    fn the_document_decides_access_and_quota() {
        assert_eq!(derive_status(true, Some(&doc(Some(caller(false, (0, 1), (0, 1))))), None, 0).state, "no_access");
        assert_eq!(derive_status(true, Some(&doc(Some(caller(true, (5, 5), (0, 1))))), None, 0).state, "maxed");
        assert_eq!(derive_status(true, Some(&doc(Some(caller(true, (0, 5), (1, 1))))), None, 0).state, "maxed");
        assert_eq!(derive_status(true, Some(&doc(Some(caller(true, (0, 5), (0, 1))))), None, 0).state, "online");
        // A zero limit is "no limit", not "full".
        assert_eq!(derive_status(true, Some(&doc(Some(caller(true, (5, 0), (0, 0))))), None, 0).state, "online");
    }

    #[test]
    fn a_recent_failure_with_nothing_since_is_offline_then_forgotten() {
        let mut s = stats();
        s.last_fail_at = Some(1500);
        s.last_fail_kind = Some(FAIL_OFFLINE.into());
        assert_eq!(derive_status(true, None, Some(&s), 1600).state, "offline");
        assert_eq!(derive_status(true, None, Some(&s), 1500 + OFFLINE_WINDOW_SECS + 1).state, "online");
        // A success after the failure clears it at once.
        s.last_ok_at = Some(1501);
        assert_eq!(derive_status(true, None, Some(&s), 1600).state, "online");
    }

    #[test]
    fn a_quota_refusal_reads_maxed_for_the_day() {
        let mut s = stats();
        s.last_fail_at = Some(1500);
        s.last_fail_kind = Some(FAIL_MAXED.into());
        assert_eq!(derive_status(true, None, Some(&s), 1600).state, "maxed");
        assert_eq!(derive_status(true, None, Some(&s), 1500 + MAXED_WINDOW_SECS + 1).state, "online");
    }

    #[test]
    fn nothing_known_is_untested_and_a_document_alone_is_online() {
        assert_eq!(derive_status(true, None, None, 0).state, "untested");
        assert_eq!(derive_status(true, Some(&doc(None)), None, 0).state, "online");
    }

    #[test]
    fn ewma_starts_at_the_first_sample_and_then_leans_on_history() {
        assert_eq!(ewma(None, 50.0), 50.0);
        assert_eq!(ewma(Some(50.0), 100.0), 65.0);
    }
}
