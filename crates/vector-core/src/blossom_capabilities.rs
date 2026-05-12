//! Per-(server, mime, encrypted) Blossom capability cache and routing.
//!
//! Uploads report outcomes here; `rank_servers()` reorders the enabled
//! list so known-good servers are tried first.

use std::collections::HashMap;
use serde::Serialize;

pub const OUTCOME_ACCEPTED: u8 = 1;
pub const OUTCOME_REJECTED_MIME: u8 = 2;
/// Seeded by a 413-only rejection (no successful upload yet). Kept
/// distinct from ACCEPTED so the UI doesn't claim an empty accepted state.
pub const OUTCOME_SIZE_ONLY: u8 = 3;

/// Rows older than this are routed as "unknown" and re-probed. Server
/// policies drift (limit bumps, MIME allow-list edits) so we refresh.
pub const STALE_AFTER_SECS: i64 = 4 * 24 * 3600;

#[derive(Clone, Debug, Serialize)]
pub struct CapabilityEntry {
    pub mime_type: String,
    /// Encrypted ciphertext rarely passes a server's content-sniff even
    /// when the declared MIME is allowed. Rows split on this flag so the
    /// two contexts learn independently.
    pub is_encrypted: bool,
    pub outcome: u8,            // 1=accepted, 2=rejected_mime, 3=size_only
    pub max_accepted_size: u64,
    /// Smallest size we've seen this server reject with HTTP 413, if any.
    pub min_rejected_size: Option<u64>,
    pub updated_at: i64,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn norm_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_lowercase()
}

// ============================================================================
// Storage
// ============================================================================

/// Record a successful upload. Bumps `max_accepted_size`; clears
/// `min_rejected_size` if reality contradicts it (the old rejection
/// was a flake, not a policy). `session` pins the write to the
/// account that started the upload so a mid-flight swap can't bleed.
pub fn record_accepted(
    server_url: &str,
    mime_type: &str,
    is_encrypted: bool,
    size_bytes: u64,
    session: crate::state::SessionGuard,
) -> Result<(), String> {
    if !session.is_valid() { return Ok(()); }
    let conn = crate::db::get_write_connection_guard_static()?;
    if !session.is_valid() { return Ok(()); }
    let server = norm_url(server_url);
    let mime = mime_type.to_lowercase();
    let enc = if is_encrypted { 1i64 } else { 0i64 };
    let now = now_secs();
    conn.execute(
        "INSERT INTO blossom_server_capabilities
            (server_url, mime_type, is_encrypted, outcome, max_accepted_size, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(server_url, mime_type, is_encrypted) DO UPDATE SET
            outcome = ?4,
            max_accepted_size = MAX(max_accepted_size, ?5),
            min_rejected_size = CASE
                WHEN min_rejected_size IS NOT NULL AND min_rejected_size <= ?5
                THEN NULL
                ELSE min_rejected_size
            END,
            updated_at = ?6",
        rusqlite::params![server, mime, enc, OUTCOME_ACCEPTED as i64, size_bytes as i64, now],
    ).map_err(|e| format!("Failed to record accepted capability: {}", e))?;
    Ok(())
}

/// Mark this `(server, mime, encrypted)` triple as MIME-rejected.
/// Future uploads route around the server.
pub fn record_rejected_mime(
    server_url: &str,
    mime_type: &str,
    is_encrypted: bool,
    session: crate::state::SessionGuard,
) -> Result<(), String> {
    if !session.is_valid() { return Ok(()); }
    let conn = crate::db::get_write_connection_guard_static()?;
    if !session.is_valid() { return Ok(()); }
    let server = norm_url(server_url);
    let mime = mime_type.to_lowercase();
    let enc = if is_encrypted { 1i64 } else { 0i64 };
    let now = now_secs();
    conn.execute(
        "INSERT INTO blossom_server_capabilities
            (server_url, mime_type, is_encrypted, outcome, max_accepted_size, updated_at)
         VALUES (?1, ?2, ?3, ?4, 0, ?5)
         ON CONFLICT(server_url, mime_type, is_encrypted) DO UPDATE SET
            outcome = ?4,
            updated_at = ?5",
        rusqlite::params![server, mime, enc, OUTCOME_REJECTED_MIME as i64, now],
    ).map_err(|e| format!("Failed to record rejected capability: {}", e))?;
    Ok(())
}

/// Record an HTTP 413. Tracks the smallest rejected size; combined with
/// `max_accepted_size` this gives pre-flight "too large" feedback.
/// Outcome is `SIZE_ONLY` (not `REJECTED_MIME`) — smaller blobs of the
/// same MIME may still succeed.
pub fn record_rejected_size(
    server_url: &str,
    mime_type: &str,
    is_encrypted: bool,
    size_bytes: u64,
    session: crate::state::SessionGuard,
) -> Result<(), String> {
    if !session.is_valid() { return Ok(()); }
    let conn = crate::db::get_write_connection_guard_static()?;
    if !session.is_valid() { return Ok(()); }
    let server = norm_url(server_url);
    let mime = mime_type.to_lowercase();
    let enc = if is_encrypted { 1i64 } else { 0i64 };
    let now = now_secs();
    // ON CONFLICT keeps any existing `outcome` (especially ACCEPTED) — a
    // 413 above the known accepted size still leaves smaller blobs viable.
    conn.execute(
        "INSERT INTO blossom_server_capabilities
            (server_url, mime_type, is_encrypted, outcome, max_accepted_size, min_rejected_size, updated_at)
         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6)
         ON CONFLICT(server_url, mime_type, is_encrypted) DO UPDATE SET
            min_rejected_size = MIN(COALESCE(min_rejected_size, ?5), ?5),
            updated_at = ?6",
        rusqlite::params![server, mime, enc, OUTCOME_SIZE_ONLY as i64, size_bytes as i64, now],
    ).map_err(|e| format!("Failed to record size rejection: {}", e))?;
    Ok(())
}

/// True iff a row exists for `(server, mime, encrypted)` and is younger
/// than `STALE_AFTER_SECS`. Used by the probe scheduler to skip rows
/// we already have current data for.
pub fn has_fresh_capability_for(server_url: &str, mime_type: &str, is_encrypted: bool) -> bool {
    let conn = match crate::db::get_db_connection_guard_static() {
        Ok(c) => c,
        Err(_) => return false,
    };
    let server = norm_url(server_url);
    let mime = mime_type.to_lowercase();
    let enc = if is_encrypted { 1i64 } else { 0i64 };
    let cutoff = now_secs().saturating_sub(STALE_AFTER_SECS);
    conn.query_row(
        "SELECT 1 FROM blossom_server_capabilities
         WHERE server_url = ?1 AND mime_type = ?2 AND is_encrypted = ?3 AND updated_at >= ?4",
        rusqlite::params![server, mime, enc, cutoff],
        |_| Ok(()),
    ).is_ok()
}

/// Drop every cached row for `server_url`. Called on hard-remove so a
/// later re-add starts with a clean slate.
pub fn purge_server(server_url: &str) -> Result<usize, String> {
    let conn = crate::db::get_write_connection_guard_static()?;
    let server = norm_url(server_url);
    let n = conn.execute(
        "DELETE FROM blossom_server_capabilities WHERE server_url = ?1",
        rusqlite::params![server],
    ).map_err(|e| format!("Failed to purge capabilities for {}: {}", server, e))?;
    Ok(n)
}

/// All rows for `server_url`, most-recent first. Renders the info dialog.
pub fn list_for_server(server_url: &str) -> Result<Vec<CapabilityEntry>, String> {
    let conn = crate::db::get_db_connection_guard_static()?;
    let server = norm_url(server_url);
    let mut stmt = conn.prepare(
        "SELECT mime_type, is_encrypted, outcome, max_accepted_size, min_rejected_size, updated_at
           FROM blossom_server_capabilities
          WHERE server_url = ?1
       ORDER BY updated_at DESC"
    ).map_err(|e| format!("Prepare failed: {}", e))?;
    let rows = stmt.query_map(rusqlite::params![server], |row| {
        let enc: i64 = row.get(1)?;
        let min_rej: Option<i64> = row.get(4)?;
        Ok(CapabilityEntry {
            mime_type: row.get(0)?,
            is_encrypted: enc != 0,
            outcome: row.get::<_, i64>(2)? as u8,
            max_accepted_size: row.get::<_, i64>(3)? as u64,
            min_rejected_size: min_rej.map(|v| v as u64),
            updated_at: row.get(5)?,
        })
    }).map_err(|e| format!("Query failed: {}", e))?;
    let mut out = Vec::new();
    for r in rows {
        if let Ok(entry) = r { out.push(entry); }
    }
    Ok(out)
}

// ============================================================================
// Routing
// ============================================================================

/// Per-server snapshot used for tier classification.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CapabilityState {
    pub outcome: u8,
    pub max_accepted_size: u64,
    pub min_rejected_size: Option<u64>,
}

/// Pure tier-classification (DB-free, unit-testable). Reorders into
/// known-good → unknown → too-large → MIME-rejected, stable within tier.
pub fn classify(
    cache: &HashMap<String, CapabilityState>,
    servers: Vec<String>,
    size_bytes: u64,
) -> Vec<String> {
    let mut known_good = Vec::new();
    let mut unknown = Vec::new();
    let mut too_large = Vec::new();
    let mut mime_rejected = Vec::new();
    for s in servers {
        let key = norm_url(&s);
        match cache.get(&key) {
            Some(st) if st.outcome == OUTCOME_REJECTED_MIME => mime_rejected.push(s),
            Some(st) => {
                // At or above the known 413 ceiling, demote behind unknown.
                if let Some(min_rej) = st.min_rejected_size {
                    if size_bytes >= min_rej {
                        too_large.push(s);
                        continue;
                    }
                }
                if size_bytes <= st.max_accepted_size {
                    known_good.push(s);
                } else {
                    unknown.push(s);
                }
            }
            None => unknown.push(s),
        }
    }
    known_good.extend(unknown);
    known_good.extend(too_large);
    known_good.extend(mime_rejected);
    known_good
}

/// Reorder `servers` for an upload of `(mime, encrypted, size_bytes)`.
pub fn rank_servers(servers: Vec<String>, mime: &str, is_encrypted: bool, size_bytes: u64) -> Vec<String> {
    if servers.is_empty() { return servers; }
    let cache = load_cache_for(&servers, mime, is_encrypted).unwrap_or_default();
    classify(&cache, servers, size_bytes)
}

/// Pre-flight check: is there any enabled server we haven't already
/// learned will reject this size/MIME/context? Unknown servers count
/// as "likely accepts" so we stay optimistic.
pub fn any_server_likely_accepts(servers: &[String], mime: &str, is_encrypted: bool, size_bytes: u64) -> bool {
    if servers.is_empty() { return false; }
    let cache = match load_cache_for(servers, mime, is_encrypted) { Ok(c) => c, Err(_) => return true };
    for s in servers {
        let key = norm_url(s);
        match cache.get(&key) {
            Some(st) if st.outcome == OUTCOME_REJECTED_MIME => continue,
            Some(st) => {
                if let Some(min_rej) = st.min_rejected_size {
                    if size_bytes >= min_rej { continue; }
                }
                return true;
            }
            None => return true,
        }
    }
    false
}

fn load_cache_for(servers: &[String], mime: &str, is_encrypted: bool) -> Result<HashMap<String, CapabilityState>, String> {
    if servers.is_empty() { return Ok(HashMap::new()); }
    let conn = crate::db::get_db_connection_guard_static()?;
    let mime_lower = mime.to_lowercase();
    let enc: i64 = if is_encrypted { 1 } else { 0 };
    // Stale rows route as unknown.
    let cutoff = now_secs().saturating_sub(STALE_AFTER_SECS);
    // rusqlite doesn't accept slices in IN — build the clause manually.
    let placeholders = servers.iter().enumerate()
        .map(|(i, _)| format!("?{}", i + 4)).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT server_url, outcome, max_accepted_size, min_rejected_size
           FROM blossom_server_capabilities
          WHERE mime_type = ?1 AND is_encrypted = ?2 AND updated_at >= ?3 AND server_url IN ({})",
        placeholders,
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("Prepare failed: {}", e))?;
    let cutoff_param: i64 = cutoff;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&mime_lower, &enc, &cutoff_param];
    let normalized: Vec<String> = servers.iter().map(|s| norm_url(s)).collect();
    for n in normalized.iter() { params.push(n); }
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
        let url: String = row.get(0)?;
        let outcome: i64 = row.get(1)?;
        let max: i64 = row.get(2)?;
        let min_rej: Option<i64> = row.get(3)?;
        Ok((url, CapabilityState {
            outcome: outcome as u8,
            max_accepted_size: max as u64,
            min_rejected_size: min_rej.map(|v| v as u64),
        }))
    }).map_err(|e| format!("Query failed: {}", e))?;
    let mut out = HashMap::new();
    for r in rows {
        if let Ok((url, state)) = r {
            out.insert(url, state);
        }
    }
    Ok(out)
}

// ============================================================================
// Outcome classification
// ============================================================================

/// HTTP 413 = "blob exceeds size cap". Drives `min_rejected_size`.
pub fn is_size_rejection(http_status: Option<u16>) -> bool {
    matches!(http_status, Some(413))
}

/// Classify an upload error as a permanent MIME rejection.
///
/// 415 is BUD-02's canonical signal. 401/402 are also treated permanent
/// (Vector's auth is fixed-shape, so 401 won't change; 402 means paid
/// server, we don't pay). 408/429/413/409 are explicitly NOT permanent.
/// For 5xx and other 4xx with no clear status signal we fall back to a
/// narrow body-keyword check — non-compliant servers (e.g. nostrcheck
/// returning `500 "could not be processed"`, blossom.band's 400 sniff
/// mismatch) would otherwise force a re-discover on every upload.
pub fn is_mime_rejection(http_status: Option<u16>, error_msg: &str) -> bool {
    if matches!(http_status, Some(415)) { return true; }
    if matches!(http_status, Some(401)) { return true; }
    if matches!(http_status, Some(402)) { return true; }
    if matches!(http_status, Some(413) | Some(409)) { return false; }
    if matches!(http_status, Some(408) | Some(429)) { return false; }
    if let Some(s) = http_status {
        if (500..=504).contains(&s) {
            // Only the "could not process" idiom is permanent on 5xx —
            // everything else stays transient (could be a temporary outage).
            let lower = error_msg.to_ascii_lowercase();
            return lower.contains("could not be processed")
                || lower.contains("cannot be processed");
        }
    }
    // Other 4xx or unknown status: substring-match body hints.
    let lower = error_msg.to_ascii_lowercase();
    let hints = [
        "could not be processed",
        "cannot be processed",
        "unsupported",
        "file type",
        "mime",
        "invalid file",
        "not allowed",
        "does not match",
        "doesn't match",
        "content-type",
    ];
    hints.iter().any(|h| lower.contains(h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_of(entries: &[(&str, u8, u64)]) -> HashMap<String, CapabilityState> {
        entries.iter()
            .map(|(url, o, max)| (norm_url(url), CapabilityState {
                outcome: *o,
                max_accepted_size: *max,
                min_rejected_size: None,
            }))
            .collect()
    }

    fn cache_with_size_cap(entries: &[(&str, u8, u64, u64)]) -> HashMap<String, CapabilityState> {
        entries.iter()
            .map(|(url, o, max, min_rej)| (norm_url(url), CapabilityState {
                outcome: *o,
                max_accepted_size: *max,
                min_rejected_size: Some(*min_rej),
            }))
            .collect()
    }

    #[test]
    fn norm_url_handles_case_and_trailing_slash() {
        assert_eq!(norm_url("HTTPS://Example.COM/"), "https://example.com");
        assert_eq!(norm_url("  https://example.com  "), "https://example.com");
        assert_eq!(norm_url("https://example.com"), "https://example.com");
    }

    #[test]
    fn classify_empty_input_returns_empty() {
        let cache = HashMap::new();
        assert!(classify(&cache, vec![], 1024).is_empty());
    }

    #[test]
    fn classify_all_unknown_preserves_order() {
        let cache = HashMap::new();
        let servers = vec!["https://a".to_string(), "https://b".to_string(), "https://c".to_string()];
        assert_eq!(classify(&cache, servers.clone(), 1024), servers);
    }

    #[test]
    fn classify_known_good_floats_to_top() {
        let cache = cache_of(&[("https://b", OUTCOME_ACCEPTED, 10_000)]);
        let servers = vec!["https://a".to_string(), "https://b".to_string(), "https://c".to_string()];
        assert_eq!(classify(&cache, servers, 5_000), vec!["https://b", "https://a", "https://c"]);
    }

    #[test]
    fn classify_known_good_falls_to_unknown_when_over_size_ceiling() {
        let cache = cache_of(&[("https://b", OUTCOME_ACCEPTED, 10_000)]);
        let servers = vec!["https://a".to_string(), "https://b".to_string(), "https://c".to_string()];
        let out = classify(&cache, servers, 20_000);
        assert_eq!(out, vec!["https://a", "https://b", "https://c"]);
    }

    #[test]
    fn classify_mime_rejected_sinks_to_bottom() {
        let cache = cache_of(&[("https://b", OUTCOME_REJECTED_MIME, 0)]);
        let servers = vec!["https://a".to_string(), "https://b".to_string(), "https://c".to_string()];
        let out = classify(&cache, servers, 1024);
        assert_eq!(out, vec!["https://a", "https://c", "https://b"]);
    }

    #[test]
    fn classify_mixed_tiers_full_ordering() {
        let cache = cache_of(&[
            ("https://b", OUTCOME_ACCEPTED, 10_000),
            ("https://c", OUTCOME_REJECTED_MIME, 0),
            ("https://d", OUTCOME_ACCEPTED, 1_000),
        ]);
        let servers = vec![
            "https://a".to_string(), "https://b".to_string(),
            "https://c".to_string(), "https://d".to_string(),
        ];
        assert_eq!(
            classify(&cache, servers, 5_000),
            vec!["https://b", "https://a", "https://d", "https://c"],
        );
    }

    #[test]
    fn classify_demotes_servers_above_known_size_ceiling() {
        let cache = cache_with_size_cap(&[("https://b", OUTCOME_ACCEPTED, 10_000, 50_000)]);
        let servers = vec!["https://a".to_string(), "https://b".to_string(), "https://c".to_string()];
        let out = classify(&cache, servers, 60_000);
        assert_eq!(out, vec!["https://a", "https://c", "https://b"]);
    }

    #[test]
    fn any_server_likely_accepts_smoke_test() {
        // Full fn needs DB access; this just sanity-checks the data shape.
        let cache = cache_with_size_cap(&[
            ("https://a", OUTCOME_ACCEPTED, 1_000, 10_000),
            ("https://b", OUTCOME_ACCEPTED, 1_000, 10_000),
        ]);
        for (_, st) in &cache {
            assert!(st.min_rejected_size.unwrap() <= 50_000);
        }
    }

    #[test]
    fn classify_keys_are_normalized_against_cache() {
        let cache = cache_of(&[("https://b.example.com", OUTCOME_ACCEPTED, 10_000)]);
        let servers = vec!["https://a".to_string(), "https://B.Example.com/".to_string()];
        let out = classify(&cache, servers, 5_000);
        assert_eq!(out, vec!["https://B.Example.com/", "https://a"]);
    }

    #[test]
    fn mime_rejection_415_is_always_a_reject() {
        assert!(is_mime_rejection(Some(415), ""));
    }

    #[test]
    fn mime_rejection_413_and_409_never_mime_regardless_of_body() {
        assert!(!is_mime_rejection(Some(413), "Payload Too Large mime"));
        assert!(!is_mime_rejection(Some(409), "Conflict file type"));
    }

    #[test]
    fn mime_rejection_401_treated_as_permanent() {
        assert!(is_mime_rejection(Some(401), "Unauthorized"));
        assert!(is_mime_rejection(Some(401), ""));
    }

    #[test]
    fn mime_rejection_5xx_with_body_hint_recorded() {
        // nostrcheck-shape: 500 + "could not be processed".
        assert!(is_mime_rejection(
            Some(500),
            r#"Upload failed with status 500 Internal Server Error: {"status":"error","message":"File could not be processed"}"#,
        ));
    }

    #[test]
    fn mime_rejection_content_type_sniff_mismatch_recorded() {
        // blossom.band-shape: 400 + body says the bytes didn't match the declared MIME.
        assert!(is_mime_rejection(
            Some(400),
            "Upload failed with status 400 Bad Request: Content-Type header does not match the file content, expected application/json",
        ));
    }

    #[test]
    fn mime_rejection_transient_5xx_without_hint_not_recorded() {
        assert!(!is_mime_rejection(Some(500), "Internal Server Error"));
        assert!(!is_mime_rejection(Some(503), "service unavailable"));
    }

    #[test]
    fn mime_rejection_transient_5xx_with_unrelated_body_keywords_not_recorded() {
        // "file type" / "content-type" in a transient body must not demote permanently.
        assert!(!is_mime_rejection(Some(503), "file type detection service down"));
        assert!(!is_mime_rejection(Some(429), "rate limit reached for content-type uploads"));
    }

    #[test]
    fn mime_rejection_payment_required_treated_as_permanent() {
        assert!(is_mime_rejection(Some(402), ""));
        assert!(is_mime_rejection(Some(402), "Payment Required"));
    }

    #[test]
    fn mime_rejection_no_status_with_body_hint_recorded() {
        assert!(is_mime_rejection(None, "Unsupported file type"));
    }

    #[test]
    fn mime_rejection_no_status_no_body_hint_not_recorded() {
        assert!(!is_mime_rejection(None, "network error"));
        assert!(!is_mime_rejection(None, "timeout"));
    }
}
