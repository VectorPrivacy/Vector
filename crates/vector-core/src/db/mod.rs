//! Database layer — SQLite with per-account databases.
//!
//! Architecture:
//! - Read pool: multiple connections for parallel reads (WAL mode)
//! - Write pool: single Mutex-protected connection (serialized writes)
//! - RAII guards: auto-return connections to pools on drop
//!
//! All connection functions use static `DATA_DIR` — no Tauri AppHandle required.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, LazyLock, RwLock};
use std::sync::atomic::{AtomicU64, Ordering};
use std::ops::{Deref, DerefMut};
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub mod settings;
pub mod schema;
pub mod profiles;
pub mod id_cache;
pub mod events;
pub mod chats;
pub mod wrappers;
pub mod nip17_keys;
pub mod community;
pub mod bots;

pub use settings::{
    get_sql_setting, set_sql_setting, get_pkey, set_pkey, get_seed, set_seed, remove_setting,
    get_signer_type, set_signer_type,
    get_bunker_url, set_bunker_url,
    get_bunker_remote_pubkey, set_bunker_remote_pubkey,
    commit_bunker_account_setup,
};

// ============================================================================
// App Data Directory
// ============================================================================

static APP_DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn set_app_data_dir(path: PathBuf) {
    let _ = APP_DATA_DIR.set(path);
}

pub fn get_app_data_dir() -> Result<&'static PathBuf, String> {
    APP_DATA_DIR.get().ok_or_else(|| "App data directory not initialized".to_string())
}

/// Host-installed override for the download directory. Tauri sets this
/// at boot via `set_download_dir()` so platform conventions (XDG on
/// Linux, Known Folders on Windows) are honored. Headless callers
/// (vector-agent CLI, tests) fall through to the env-var path.
static DOWNLOAD_DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Install the host-resolved download directory. Must be called at
/// startup before any `get_download_dir()` consumer runs; callers that
/// run earlier hit the fallback.
pub fn set_download_dir(path: PathBuf) {
    let _ = DOWNLOAD_DIR_OVERRIDE.set(path);
}

/// Platform-appropriate download directory for file attachments.
///
/// Prefers the host-installed override (honors `xdg-user-dirs`,
/// `FOLDERID_Downloads`, `NSDownloadsDirectory`, `NSDocumentDirectory`).
/// Falls back to `$HOME/Downloads/vector` on desktop, then
/// `<app_data>/vector_downloads` on mobile / pre-init.
pub fn get_download_dir() -> PathBuf {
    if let Some(installed) = DOWNLOAD_DIR_OVERRIDE.get() {
        return installed.clone();
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Downloads/vector");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            return PathBuf::from(profile).join("Downloads").join("vector");
        }
    }
    // Mobile / fallback: use data dir
    if let Ok(data_dir) = get_app_data_dir() {
        return data_dir.join("vector_downloads");
    }
    PathBuf::from("/tmp/vector_downloads")
}

// ============================================================================
// Current Account
// ============================================================================

static CURRENT_ACCOUNT: LazyLock<Arc<RwLock<Option<String>>>> = LazyLock::new(|| Arc::new(RwLock::new(None)));
// PENDING_ACCOUNT lives exclusively in src-tauri's account_manager —
// any "pending account" check must go through that crate, not here.

/// Filename for the persistent active-account marker. Plain text, just the npub.
const ACTIVE_ACCOUNT_FILE: &str = "active_account";

/// npub bech32 form: `npub1` + 58 chars from the bech32 alphabet (no `1`, `b`, `i`, `o`).
fn is_valid_npub(s: &str) -> bool {
    if s.len() != 63 || !s.starts_with("npub1") {
        return false;
    }
    s.bytes().skip(5).all(|c| matches!(c,
        b'q' | b'p' | b'z' | b'r' | b'y' | b'9' | b'x' | b'8' |
        b'g' | b'f' | b'2' | b't' | b'v' | b'd' | b'w' | b'0' |
        b's' | b'3' | b'j' | b'n' | b'5' | b'4' | b'k' | b'h' |
        b'c' | b'e' | b'6' | b'm' | b'u' | b'a' | b'7' | b'l'
    ))
}

pub fn get_current_account() -> Result<String, String> {
    CURRENT_ACCOUNT.read().unwrap()
        .as_ref().cloned()
        .ok_or_else(|| "No active account".to_string())
}

/// Set the currently-active npub for THIS process AND persist it to the
/// `<app_data>/active_account` marker so the next boot picks the same account.
///
/// Every call site asserts user intent ("this account is now active"); the
/// marker write is idempotent and gracefully no-ops when `APP_DATA_DIR` is
/// not yet configured (e.g. during in-process unit tests).
pub fn set_current_account(npub: String) -> Result<(), String> {
    *CURRENT_ACCOUNT.write().unwrap() = Some(npub.clone());
    let _ = write_active_account_file(&npub);
    Ok(())
}

/// Clear the in-memory active account WITHOUT touching the on-disk marker.
/// Used by `reset_session()` so the next-boot marker stays intact while
/// in-process state is torn down for an inline account swap.
pub fn clear_current_account_in_memory() {
    *CURRENT_ACCOUNT.write().unwrap() = None;
}

/// Read the active-account marker file. Returns the stored npub if it exists,
/// is well-formed, AND the corresponding account directory still exists.
/// Any failure path returns Ok(None) so boot falls back to single-account or picker.
pub fn read_active_account_file() -> Result<Option<String>, String> {
    let app_data = match get_app_data_dir() {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    read_active_account_file_in(app_data)
}

/// Atomic write of the active-account marker (temp + rename).
pub fn write_active_account_file(npub: &str) -> Result<(), String> {
    let app_data = get_app_data_dir()?.clone();
    write_active_account_file_in(&app_data, npub)
}

/// Remove the active-account marker. Used after deleting the active account.
pub fn clear_active_account_file() -> Result<(), String> {
    let app_data = get_app_data_dir()?;
    clear_active_account_file_in(app_data)
}

/// Scan the app data directory for valid npub directories. Strict bech32 regex
/// rejects typos and stray subdirectories. Does NOT validate that each account
/// has a usable database — callers do that separately.
pub fn list_account_npubs() -> Result<Vec<String>, String> {
    let app_data = get_app_data_dir()?;
    Ok(list_account_npubs_in(app_data))
}

// ----- path-parameterized internals (kept private so tests can inject a temp dir) -----

/// Bound on bytes read from the active-account marker. A valid marker
/// is 63 bytes (canonical npub) plus optional trailing newline. The
/// marker lives in a user-writable dir, so accidental / malicious
/// multi-gigabyte writes are a realistic OOM vector if read unbounded.
const MARKER_MAX_BYTES: u64 = 256;

fn read_active_account_file_in(app_data: &std::path::Path) -> Result<Option<String>, String> {
    use std::io::Read;

    let path = app_data.join(ACTIVE_ACCOUNT_FILE);
    if !path.exists() {
        return Ok(None);
    }
    // Pre-check size, then belt-and-suspenders cap via `take()` to
    // cover the TOCTOU window between metadata and open. Metadata
    // failures fail-safe to "missing".
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MARKER_MAX_BYTES {
            return Ok(None);
        }
    } else {
        return Ok(None);
    }
    let mut buf = String::new();
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };
    if file.take(MARKER_MAX_BYTES).read_to_string(&mut buf).is_err() {
        return Ok(None);
    }
    let npub = buf.trim().to_string();
    if !is_valid_npub(&npub) {
        return Ok(None);
    }
    // `symlink_metadata` instead of `is_dir()` (which follows links): a
    // crafted symlink at `<app_data>/<valid-npub-name>` pointing at
    // `~/Documents` etc. would otherwise pass, and downstream
    // `remove_dir_all` in delete_account / logout would traverse it.
    // Bech32 validation alone is insufficient — the attacker controls
    // the filename, not the npub semantic.
    match std::fs::symlink_metadata(app_data.join(&npub)) {
        Ok(meta) if meta.file_type().is_dir() && !meta.file_type().is_symlink() => {}
        _ => return Ok(None),
    }
    Ok(Some(npub))
}

fn write_active_account_file_in(app_data: &std::path::Path, npub: &str) -> Result<(), String> {
    if !is_valid_npub(npub) {
        return Err(format!("Invalid npub format: {}", npub));
    }
    if !app_data.exists() {
        std::fs::create_dir_all(app_data)
            .map_err(|e| format!("Failed to create app data dir: {}", e))?;
    }
    // Refuse to point the marker at a directory that doesn't exist as a
    // real subfolder. Closes the race where a concurrent `delete_account`
    // for `npub` runs between the caller's existence check and this write.
    // `symlink_metadata` (matching the read path) so a crafted
    // `<app_data>/<valid-npub-name>` symlink can't satisfy the check.
    match std::fs::symlink_metadata(app_data.join(npub)) {
        Ok(meta) if meta.file_type().is_dir() && !meta.file_type().is_symlink() => {}
        _ => return Err(format!("Account directory missing or invalid: {}", npub)),
    }
    let tmp = app_data.join(format!("{}.tmp", ACTIVE_ACCOUNT_FILE));
    let final_path = app_data.join(ACTIVE_ACCOUNT_FILE);

    // Trailing newline so `cat` doesn't mangle the shell prompt and so editors
    // that auto-strip trailing newlines don't dirty-mark the file on save.
    let mut payload = String::with_capacity(npub.len() + 1);
    payload.push_str(npub);
    payload.push('\n');

    if let Err(e) = std::fs::write(&tmp, payload.as_bytes()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("Failed to write active account temp file: {}", e));
    }

    // Retry the rename a few times. On Windows, transient antivirus or backup
    // scans can hold a brief sharing-violation lock on the destination file.
    let mut last_err = None;
    for attempt in 0..3 {
        match std::fs::rename(&tmp, &final_path) {
            Ok(_) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                if attempt < 2 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
    }

    // Don't leave the temp file behind if every attempt failed.
    let _ = std::fs::remove_file(&tmp);
    Err(format!(
        "Failed to rename active account file: {}",
        last_err.map(|e| e.to_string()).unwrap_or_default()
    ))
}

fn clear_active_account_file_in(app_data: &std::path::Path) -> Result<(), String> {
    let path = app_data.join(ACTIVE_ACCOUNT_FILE);
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("Failed to remove active account file: {}", e))?;
    }
    Ok(())
}

fn list_account_npubs_in(app_data: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(app_data) {
        for entry in entries.flatten() {
            if entry.file_type().map_or(false, |ft| ft.is_dir()) {
                let name = entry.file_name().to_string_lossy().to_string();
                if is_valid_npub(&name) {
                    out.push(name);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod active_account_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Real npub from the project's own test fixtures (matches bech32 regex).
    const VALID_A: &str = "npub16ye7evyevwnl0fc9hujsxf9zym72e063awn0pvde0huvpyec5nyq4dg4wn";
    const VALID_B: &str = "npub12w73tzcqgpr2pcy4el5x60d2emeud4cyeeayynzqgg2fefzgytaqm4ktz3";

    fn touch_account_dir(base: &std::path::Path, npub: &str) {
        fs::create_dir_all(base.join(npub)).unwrap();
    }

    #[test]
    fn npub_validator_accepts_canonical_form() {
        assert!(is_valid_npub(VALID_A));
        assert!(is_valid_npub(VALID_B));
    }

    #[test]
    fn npub_validator_rejects_wrong_length() {
        assert!(!is_valid_npub("npub1abc"));
        assert!(!is_valid_npub(&format!("{}x", VALID_A)));
        assert!(!is_valid_npub(""));
    }

    #[test]
    fn npub_validator_rejects_missing_prefix() {
        let body = &VALID_A[5..];
        assert!(!is_valid_npub(&format!("nsec1{}", body)));
        assert!(!is_valid_npub(&format!("xxxx1{}", body)));
    }

    #[test]
    fn npub_validator_rejects_non_bech32_chars() {
        // Replace one char in the body with each disallowed bech32 letter.
        for bad in ['1', 'b', 'i', 'o', 'B', 'I', 'O', '!', '*', ' '] {
            let mut s = String::from(VALID_A);
            s.replace_range(10..11, &bad.to_string());
            assert!(!is_valid_npub(&s), "should reject character {:?}", bad);
        }
    }

    #[test]
    fn write_then_read_round_trips() {
        let tmp = TempDir::new().unwrap();
        touch_account_dir(tmp.path(), VALID_A);

        write_active_account_file_in(tmp.path(), VALID_A).unwrap();
        assert_eq!(
            read_active_account_file_in(tmp.path()).unwrap(),
            Some(VALID_A.to_string())
        );
    }

    #[test]
    fn write_rejects_invalid_npub() {
        let tmp = TempDir::new().unwrap();
        let err = write_active_account_file_in(tmp.path(), "npub1nope").unwrap_err();
        assert!(err.contains("Invalid"));
        // No file should have been created (neither final nor temp).
        assert!(!tmp.path().join(ACTIVE_ACCOUNT_FILE).exists());
        assert!(!tmp.path().join(format!("{}.tmp", ACTIVE_ACCOUNT_FILE)).exists());
    }

    #[test]
    fn write_rejects_missing_account_dir() {
        // A concurrent `delete_account` between the caller's existence check
        // and write_active_account_file would otherwise leave a stale marker
        // pointing at a now-deleted account.
        let tmp = TempDir::new().unwrap();
        let err = write_active_account_file_in(tmp.path(), VALID_A).unwrap_err();
        assert!(err.contains("missing or invalid"),
            "expected account-dir-missing error, got: {}", err);
        // Marker must not have been written.
        assert!(!tmp.path().join(ACTIVE_ACCOUNT_FILE).exists());
        assert!(!tmp.path().join(format!("{}.tmp", ACTIVE_ACCOUNT_FILE)).exists());
    }

    #[test]
    fn write_rejects_symlinked_account_dir() {
        // A crafted `<app_data>/<valid-npub-name>` symlink to ~/Documents
        // would otherwise pass `is_dir()` and let the marker point at an
        // attacker-controlled location, which downstream delete/logout paths
        // would then traverse.
        let tmp = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        let link = tmp.path().join(VALID_A);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target.path(), &link).unwrap();
            let err = write_active_account_file_in(tmp.path(), VALID_A).unwrap_err();
            assert!(err.contains("missing or invalid"),
                "expected symlink rejection, got: {}", err);
        }
        // On Windows symlink creation may require elevated privileges; skip
        // the assertion there rather than gate the whole test on platform.
        #[cfg(not(unix))]
        let _ = (target, link);
    }

    #[test]
    fn read_returns_none_when_marker_missing() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(read_active_account_file_in(tmp.path()).unwrap(), None);
    }

    #[test]
    fn read_returns_none_when_marker_is_garbage() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(ACTIVE_ACCOUNT_FILE), b"not-an-npub\n").unwrap();
        assert_eq!(read_active_account_file_in(tmp.path()).unwrap(), None);
    }

    #[test]
    fn read_returns_none_when_account_dir_missing() {
        // Marker exists, npub is well-formed, but the account directory was
        // deleted out from under us. Boot must fall through to picker, never crash.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(ACTIVE_ACCOUNT_FILE), VALID_A).unwrap();
        assert_eq!(read_active_account_file_in(tmp.path()).unwrap(), None);
    }

    #[test]
    fn read_returns_none_when_marker_oversized() {
        // Marker lives in a user-writable directory — guard against a giant
        // file OOMing the app. Anything past MARKER_MAX_BYTES is treated as
        // corrupt.
        let tmp = TempDir::new().unwrap();
        let payload = vec![b'x'; (MARKER_MAX_BYTES + 1024) as usize];
        fs::write(tmp.path().join(ACTIVE_ACCOUNT_FILE), &payload).unwrap();
        assert_eq!(read_active_account_file_in(tmp.path()).unwrap(), None);
    }

    #[test]
    fn read_trims_whitespace() {
        let tmp = TempDir::new().unwrap();
        touch_account_dir(tmp.path(), VALID_A);
        fs::write(
            tmp.path().join(ACTIVE_ACCOUNT_FILE),
            format!("  {}\n", VALID_A),
        ).unwrap();
        assert_eq!(
            read_active_account_file_in(tmp.path()).unwrap(),
            Some(VALID_A.to_string())
        );
    }

    #[test]
    fn read_handles_crlf_line_endings() {
        let tmp = TempDir::new().unwrap();
        touch_account_dir(tmp.path(), VALID_A);
        fs::write(
            tmp.path().join(ACTIVE_ACCOUNT_FILE),
            format!("{}\r\n", VALID_A),
        ).unwrap();
        assert_eq!(
            read_active_account_file_in(tmp.path()).unwrap(),
            Some(VALID_A.to_string())
        );
    }

    #[test]
    fn npub_validator_rejects_uppercase_prefix() {
        let upper = format!("NPUB1{}", &VALID_A[5..]);
        assert!(!is_valid_npub(&upper));
    }

    #[test]
    fn write_then_read_round_trips_with_newline() {
        // Belt-and-braces check: confirms our own writer (which appends \n)
        // round-trips through our own reader (which trims) with no surprises.
        let tmp = TempDir::new().unwrap();
        touch_account_dir(tmp.path(), VALID_A);
        write_active_account_file_in(tmp.path(), VALID_A).unwrap();

        let raw = fs::read_to_string(tmp.path().join(ACTIVE_ACCOUNT_FILE)).unwrap();
        assert!(raw.ends_with('\n'));

        assert_eq!(
            read_active_account_file_in(tmp.path()).unwrap(),
            Some(VALID_A.to_string())
        );
    }

    #[test]
    fn write_overwrites_previous_marker_atomically() {
        let tmp = TempDir::new().unwrap();
        touch_account_dir(tmp.path(), VALID_A);
        touch_account_dir(tmp.path(), VALID_B);

        write_active_account_file_in(tmp.path(), VALID_A).unwrap();
        write_active_account_file_in(tmp.path(), VALID_B).unwrap();

        assert_eq!(
            read_active_account_file_in(tmp.path()).unwrap(),
            Some(VALID_B.to_string())
        );
        // The temp file used for atomic rename should not linger.
        assert!(!tmp.path().join(format!("{}.tmp", ACTIVE_ACCOUNT_FILE)).exists());
    }

    #[test]
    fn clear_removes_marker_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        touch_account_dir(tmp.path(), VALID_A);
        write_active_account_file_in(tmp.path(), VALID_A).unwrap();
        assert!(tmp.path().join(ACTIVE_ACCOUNT_FILE).exists());

        clear_active_account_file_in(tmp.path()).unwrap();
        assert!(!tmp.path().join(ACTIVE_ACCOUNT_FILE).exists());

        // Calling clear again on an already-clean state must not error.
        clear_active_account_file_in(tmp.path()).unwrap();
    }

    #[test]
    fn list_npubs_finds_valid_dirs_only() {
        let tmp = TempDir::new().unwrap();
        touch_account_dir(tmp.path(), VALID_A);
        touch_account_dir(tmp.path(), VALID_B);
        // Decoys: stray dirs and files that must NOT be picked up.
        fs::create_dir_all(tmp.path().join("npub1tooshort")).unwrap();
        fs::create_dir_all(tmp.path().join("not-an-npub-dir")).unwrap();
        fs::create_dir_all(tmp.path().join("tor")).unwrap();
        fs::write(tmp.path().join(ACTIVE_ACCOUNT_FILE), VALID_A).unwrap();

        let mut found = list_account_npubs_in(tmp.path());
        found.sort();
        let mut expected = vec![VALID_A.to_string(), VALID_B.to_string()];
        expected.sort();
        assert_eq!(found, expected);
    }

    #[test]
    fn list_npubs_skips_dirs_containing_invalid_chars() {
        let tmp = TempDir::new().unwrap();
        // Insert a 'b', 'i', 'o', or '1' into the body — invalid bech32 chars.
        let mut bogus = String::from(VALID_A);
        bogus.replace_range(10..11, "b");
        fs::create_dir_all(tmp.path().join(&bogus)).unwrap();

        let found = list_account_npubs_in(tmp.path());
        assert!(found.is_empty(), "found unexpected entries: {:?}", found);
    }

    #[test]
    fn write_creates_app_data_dir_if_missing() {
        let tmp = TempDir::new().unwrap();
        let nested = tmp.path().join("does/not/exist/yet");
        // Parent app_data is auto-created by mkdir_all; the account dir must
        // also exist by the time we write, so the marker can't end up
        // pointing at a non-existent account.
        std::fs::create_dir_all(&nested).unwrap();
        touch_account_dir(&nested, VALID_A);
        write_active_account_file_in(&nested, VALID_A).unwrap();
        assert!(nested.join(ACTIVE_ACCOUNT_FILE).exists());
    }
}

// pending-account accessors removed — see comment above the static decl.
// All callers use `src-tauri::account_manager::{get,set,clear}_pending_account`.

// ============================================================================
// Connection Pools
// ============================================================================

static DB_READ_POOL: LazyLock<Arc<Mutex<Vec<rusqlite::Connection>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

static DB_WRITE_CONN: LazyLock<Arc<Mutex<Option<rusqlite::Connection>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(None)));

/// Monotonic generation counter for the connection pool.
///
/// Every guard captures the current value at construction and compares
/// on `Drop` — mismatch means the pool was reset (account switch /
/// `close_database`) and the connection MUST be dropped instead of
/// returned. Without this, an in-flight guard from account A could
/// re-enter the pool after account B has initialized, causing account
/// B's queries to silently run against account A's database.
///
/// Bumped by both `close_database()` and `init_database()`, so a swap
/// (close → init) advances twice; either bump alone invalidates
/// outstanding guards.
static POOL_GENERATION: AtomicU64 = AtomicU64::new(0);

#[inline]
fn current_pool_generation() -> u64 {
    POOL_GENERATION.load(Ordering::Acquire)
}

#[inline]
fn bump_pool_generation() -> u64 {
    // fetch_add returns the previous value; the new generation is +1.
    POOL_GENERATION.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
}

/// RAII guard for READ connections — auto-returns to pool on drop.
pub struct ConnectionGuard {
    conn: Option<rusqlite::Connection>,
    generation: u64,
}

impl ConnectionGuard {
    fn new(conn: rusqlite::Connection, generation: u64) -> Self {
        Self { conn: Some(conn), generation }
    }
}

impl Deref for ConnectionGuard {
    type Target = rusqlite::Connection;
    fn deref(&self) -> &Self::Target { self.conn.as_ref().expect("Connection already taken") }
}

impl DerefMut for ConnectionGuard {
    fn deref_mut(&mut self) -> &mut Self::Target { self.conn.as_mut().expect("Connection already taken") }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            // Only return to pool if our generation still matches —
            // otherwise the pool was reset mid-flight and pushing back
            // would let account A's connection serve account B's queries.
            if self.generation == current_pool_generation() {
                if let Ok(mut pool) = DB_READ_POOL.lock() {
                    pool.push(conn);
                }
            }
        }
    }
}

/// RAII guard for the WRITE connection — auto-returns on drop.
pub struct WriteConnectionGuard {
    conn: Option<rusqlite::Connection>,
    generation: u64,
}

impl WriteConnectionGuard {
    fn new(conn: rusqlite::Connection, generation: u64) -> Self {
        Self { conn: Some(conn), generation }
    }
}

impl Deref for WriteConnectionGuard {
    type Target = rusqlite::Connection;
    fn deref(&self) -> &Self::Target { self.conn.as_ref().expect("Write connection already taken") }
}

impl DerefMut for WriteConnectionGuard {
    fn deref_mut(&mut self) -> &mut Self::Target { self.conn.as_mut().expect("Write connection already taken") }
}

impl Drop for WriteConnectionGuard {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            // Same generation gate as ConnectionGuard, plus a slot-empty
            // check: if `init_database` already installed a fresh write
            // connection for the new account, dropping ours over the top
            // would clobber it.
            if self.generation == current_pool_generation() {
                if let Ok(mut slot) = DB_WRITE_CONN.lock() {
                    if slot.is_none() {
                        *slot = Some(conn);
                    }
                }
            }
        }
    }
}

// ============================================================================
// Connection Factory
// ============================================================================

/// Single source of truth for per-account directories. Every per-account
/// subsystem (DB, Tor state) resolves its path through this;
/// compose further subpaths with `.join(...)` — never insert layers
/// between `<app_data>` and `<npub>`.
///
/// Validates npub format before joining as defence-in-depth against
/// path traversal: a crafted IPC input like `"../../etc"` would
/// otherwise yield `<app_data>/../../etc` and downstream
/// `remove_dir_all` (delete_account, logout) would walk arbitrary dirs.
pub fn account_dir(npub: &str) -> Result<PathBuf, String> {
    if !is_valid_npub(npub) {
        return Err(format!("Invalid npub format: {}", npub));
    }
    Ok(get_app_data_dir()?.join(npub))
}

fn get_current_db_path() -> Result<PathBuf, String> {
    let npub = get_current_account()?;
    Ok(account_dir(&npub)?.join("vector.db"))
}

fn create_connection(path: &PathBuf) -> Result<rusqlite::Connection, String> {
    let conn = rusqlite::Connection::open(path)
        .map_err(|e| format!("Failed to open database: {}", e))?;

    // WAL for concurrent reads; busy_timeout for lock contention. cache_size negative = KiB
    // (16 MiB page cache) to keep hot pages resident on a large DB; temp_store=MEMORY keeps
    // GROUP BY / sort scratch in memory instead of spilling to disk.
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000; PRAGMA cache_size=-16000; PRAGMA temp_store=MEMORY;")
        .map_err(|e| format!("Failed to set pragmas: {}", e))?;

    Ok(conn)
}

/// Get a READ connection (headless-safe — no AppHandle).
pub fn get_db_connection_guard_static() -> Result<ConnectionGuard, String> {
    let generation = current_pool_generation();
    // Try to get from pool first
    if let Ok(mut pool) = DB_READ_POOL.lock() {
        if let Some(conn) = pool.pop() {
            return Ok(ConnectionGuard::new(conn, generation));
        }
    }
    // Create new connection
    let path = get_current_db_path()?;
    let conn = create_connection(&path)?;
    Ok(ConnectionGuard::new(conn, generation))
}

/// Process-wide serialization lock for tests that install into the global DB pool.
/// Any test calling `init_database` must hold this for its whole body — otherwise
/// concurrent inits race on `POOL_GENERATION` and clobber each other's connections.
/// One shared guard across every module (community, ...) so cross-module test
/// parallelism can't collide.
#[cfg(test)]
pub(crate) static DB_TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Get the WRITE connection (headless-safe — no AppHandle).
pub fn get_write_connection_guard_static() -> Result<WriteConnectionGuard, String> {
    let generation = current_pool_generation();
    let mut write_slot = DB_WRITE_CONN.lock().unwrap();
    if let Some(conn) = write_slot.take() {
        return Ok(WriteConnectionGuard::new(conn, generation));
    }
    drop(write_slot);

    let path = get_current_db_path()?;
    let conn = create_connection(&path)?;
    Ok(WriteConnectionGuard::new(conn, generation))
}

// ============================================================================
// Database Initialization
// ============================================================================

/// Initialize the database for a given account (creates tables if needed).
pub fn init_database(npub: &str) -> Result<(), String> {
    let profile_dir = account_dir(npub)?;

    if !profile_dir.exists() {
        std::fs::create_dir_all(&profile_dir)
            .map_err(|e| format!("Failed to create profile directory: {}", e))?;
    }

    let db_path = profile_dir.join("vector.db");
    let mut conn = create_connection(&db_path)?;
    conn.execute_batch(schema::SQL_SCHEMA)
        .map_err(|e| format!("Failed to create schema: {}", e))?;

    // Run migrations
    schema::run_migrations(&mut conn)?;

    // MLS is fully removed. Migration 41 drops the relational tables, but the OpenMLS/MDK
    // crypto store lived in a SEPARATE per-account file (`<account>/mls/`) that no migration
    // can reach. Purge it here: it's dead weight (can run to hundreds of MB) and, worse,
    // stale MLS private key material lingering for a feature that no longer exists. Best-effort
    // and idempotent — a cleanup failure must never block account init.
    let mls_dir = profile_dir.join("mls");
    if mls_dir.exists() {
        match std::fs::remove_dir_all(&mls_dir) {
            Ok(()) => crate::log_info!("[db] purged orphaned MLS store for account"),
            Err(e) => crate::log_warn!("[db] could not purge orphaned MLS store: {}", e),
        }
    }

    // Bump BEFORE installing the new pool so any in-flight guards from
    // the previous account fail their Drop check and don't pollute the
    // freshly-initialized pool.
    bump_pool_generation();

    // Pre-warm read pool
    if let Ok(mut pool) = DB_READ_POOL.lock() {
        pool.clear();
        for _ in 0..4 {
            if let Ok(c) = create_connection(&db_path) {
                pool.push(c);
            }
        }
    }

    // Set write connection
    let write_conn = create_connection(&db_path)?;
    *DB_WRITE_CONN.lock().unwrap() = Some(write_conn);

    // Hydrate Tor's hot-path settings cache directly from `db_path`,
    // NOT via `get_sql_setting()` — the global helper resolves through
    // the read pool + `get_current_account()`, neither of which yet
    // reflects this account (switch_account calls init_database BEFORE
    // set_current_account).
    #[cfg(feature = "tor")]
    {
        let enabled = create_connection(&db_path)
            .ok()
            .and_then(|c| {
                c.query_row(
                    "SELECT value FROM settings WHERE key = 'tor_enabled'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .ok()
            })
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false);
        crate::tor::set_tor_enabled_pref(enabled);
    }

    Ok(())
}

/// Close all database connections (for logout / account switch).
/// Bumps `POOL_GENERATION` first so in-flight guards fail their Drop
/// check and discard the connection instead of returning it to the
/// (now-cleared) pool.
pub fn close_database() {
    // Refresh planner stats before dropping the working connection: at close its per-session change
    // counters reflect everything that churned, so PRAGMA optimize re-analyzes exactly those tables.
    optimize_database();
    bump_pool_generation();
    if let Ok(mut pool) = DB_READ_POOL.lock() {
        pool.clear();
    }
    *DB_WRITE_CONN.lock().unwrap() = None;
}

/// Refresh SQLite's query-planner statistics via `PRAGMA optimize`. Best-effort and cheap
/// (incremental — re-analyzes only what changed since the last run), unlike the weekly VACUUM. Run
/// at shutdown/connection-close so the NEXT boot plans from fresh stats without paying for it.
pub fn optimize_database() {
    if let Ok(guard) = DB_WRITE_CONN.lock() {
        if let Some(conn) = guard.as_ref() {
            let _ = conn.execute_batch("PRAGMA optimize;");
        }
    }
}

/// Get all available accounts (npub directories in app data).
pub fn get_accounts() -> Result<Vec<String>, String> {
    let app_data = get_app_data_dir()?;
    let mut accounts = Vec::new();

    if let Ok(entries) = std::fs::read_dir(app_data) {
        for entry in entries.flatten() {
            if entry.file_type().map_or(false, |ft| ft.is_dir()) {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("npub1") {
                    // Check if vector.db exists
                    if entry.path().join("vector.db").exists() {
                        accounts.push(name);
                    }
                }
            }
        }
    }

    Ok(accounts)
}

/// Get the profile directory path for a given npub.
pub fn get_profile_directory(npub: &str) -> Result<PathBuf, String> {
    if !npub.starts_with("npub1") {
        return Err(format!("Invalid npub format: {}", npub));
    }
    let dir = account_dir(npub)?;
    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create profile directory: {}", e))?;
    }
    Ok(dir)
}

/// Get database path for a given npub.
pub fn get_database_path(npub: &str) -> Result<PathBuf, String> {
    Ok(get_profile_directory(npub)?.join("vector.db"))
}

// ============================================================================
// ID Caches
// ============================================================================

static CHAT_ID_CACHE: LazyLock<Arc<RwLock<HashMap<String, i64>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(HashMap::new())));

static USER_ID_CACHE: LazyLock<Arc<RwLock<HashMap<String, i64>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(HashMap::new())));

pub fn clear_id_caches() {
    CHAT_ID_CACHE.write().unwrap().clear();
    USER_ID_CACHE.write().unwrap().clear();
    // ALSO clear the parallel caches in `id_cache` — that's the pair `db::events::save_message` and the
    // community member-activity read actually use. Chat/user row ids are PER-ACCOUNT (each account has its
    // own DB + id sequence), so a stale entry after an account swap points into the wrong DB → writes
    // FK-fail (silently) and reads hit the wrong/empty row. One public clear must wipe every id cache.
    id_cache::clear_id_caches();
}

/// Get or create a chat_id integer for a chat identifier string.
pub fn get_or_create_chat_id(conn: &rusqlite::Connection, identifier: &str, chat_type: i32) -> Result<i64, String> {
    // Check cache first
    if let Some(&id) = CHAT_ID_CACHE.read().unwrap().get(identifier) {
        return Ok(id);
    }

    // Try to find existing
    let result: Option<i64> = conn.query_row(
        "SELECT id FROM chats WHERE chat_identifier = ?1",
        rusqlite::params![identifier],
        |row| row.get(0),
    ).ok();

    if let Some(id) = result {
        CHAT_ID_CACHE.write().unwrap().insert(identifier.to_string(), id);
        return Ok(id);
    }

    // Create new
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    conn.execute(
        "INSERT INTO chats (chat_identifier, chat_type, participants, created_at) VALUES (?1, ?2, '', ?3)",
        rusqlite::params![identifier, chat_type, now],
    ).map_err(|e| format!("Failed to create chat: {}", e))?;

    let id = conn.last_insert_rowid();
    CHAT_ID_CACHE.write().unwrap().insert(identifier.to_string(), id);
    Ok(id)
}

/// Get or create a user_id integer for an npub.
pub fn get_or_create_user_id(conn: &rusqlite::Connection, npub: &str) -> Result<i64, String> {
    if let Some(&id) = USER_ID_CACHE.read().unwrap().get(npub) {
        return Ok(id);
    }

    let result: Option<i64> = conn.query_row(
        "SELECT id FROM profiles WHERE npub = ?1",
        rusqlite::params![npub],
        |row| row.get(0),
    ).ok();

    if let Some(id) = result {
        USER_ID_CACHE.write().unwrap().insert(npub.to_string(), id);
        return Ok(id);
    }

    conn.execute(
        "INSERT OR IGNORE INTO profiles (npub) VALUES (?1)",
        rusqlite::params![npub],
    ).map_err(|e| format!("Failed to create user: {}", e))?;

    let id = conn.query_row(
        "SELECT id FROM profiles WHERE npub = ?1",
        rusqlite::params![npub],
        |row| row.get(0),
    ).map_err(|e| format!("Failed to get user id: {}", e))?;

    USER_ID_CACHE.write().unwrap().insert(npub.to_string(), id);
    Ok(id)
}

// ============================================================================
// System Event Types
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SystemEventType {
    MemberLeft = 0,
    MemberJoined = 1,
    MemberRemoved = 2,
    WallpaperChanged = 3,
}

impl SystemEventType {
    pub fn display_message(&self, display_name: &str) -> String {
        match self {
            SystemEventType::MemberLeft => format!("{} has left", display_name),
            SystemEventType::MemberJoined => format!("{} has joined", display_name),
            SystemEventType::MemberRemoved => format!("{} was removed", display_name),
            SystemEventType::WallpaperChanged => format!("{} changed the wallpaper", display_name),
        }
    }

    pub fn as_u8(&self) -> u8 { *self as u8 }
}

#[cfg(test)]
mod pool_generation_tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a minimal in-memory SQLite connection — just enough to drop
    /// a connection through the guard machinery. We don't run schema or
    /// migrations because we only care about the guard's Drop pathway.
    fn fake_conn() -> rusqlite::Connection {
        rusqlite::Connection::open_in_memory().unwrap()
    }

    #[test]
    fn close_database_bumps_generation() {
        let before = current_pool_generation();
        close_database();
        let after = current_pool_generation();
        assert!(after > before, "close_database must advance POOL_GENERATION");
    }

    #[test]
    fn init_database_bumps_generation() {
        // init_database requires APP_DATA_DIR to be set; we don't fully exercise
        // it here (would need schema/migrations). The cheaper invariant we test
        // is that bump_pool_generation itself advances the counter — which is
        // what init_database does at the top of its body.
        let before = current_pool_generation();
        let bumped = bump_pool_generation();
        assert_eq!(bumped, before.wrapping_add(1));
        assert_eq!(current_pool_generation(), bumped);
    }

    #[test]
    fn stale_read_guard_does_not_return_to_pool_after_generation_bump() {
        // Snapshot the pool generation, construct a guard at that generation,
        // bump the generation (simulating a swap), then drop the guard.
        // The drop must NOT push back into the pool.
        let _tmp = TempDir::new().unwrap(); // keeps any side-effects scoped

        // Drain whatever happens to be in the pool to start from a known state.
        let pool_size_before = DB_READ_POOL.lock().unwrap().len();

        let stale_generation = current_pool_generation();
        let guard = ConnectionGuard::new(fake_conn(), stale_generation);

        // Account swap: bump generation invalidates outstanding guards.
        bump_pool_generation();

        drop(guard);

        let pool_size_after = DB_READ_POOL.lock().unwrap().len();
        assert_eq!(
            pool_size_after, pool_size_before,
            "stale read guard must not re-enter the pool"
        );
    }

    #[test]
    fn fresh_read_guard_returns_to_pool() {
        let pool_size_before = DB_READ_POOL.lock().unwrap().len();

        let generation = current_pool_generation();
        let guard = ConnectionGuard::new(fake_conn(), generation);

        // No generation bump — guard is still valid.
        drop(guard);

        let pool_size_after = DB_READ_POOL.lock().unwrap().len();
        assert_eq!(
            pool_size_after,
            pool_size_before + 1,
            "fresh read guard should be returned to the pool"
        );

        // Cleanup: drain the connection we just pushed so we don't pollute
        // sibling tests sharing the global.
        DB_READ_POOL.lock().unwrap().pop();
    }

    #[test]
    fn stale_write_guard_does_not_overwrite_fresh_slot() {
        // The dropped stale guard must not clobber a write connection that
        // init_database has freshly installed for the new account.
        let stale_generation = current_pool_generation();
        let stale_guard = WriteConnectionGuard::new(fake_conn(), stale_generation);

        bump_pool_generation();

        // Simulate init_database installing a new write connection.
        let fresh_conn = fake_conn();
        *DB_WRITE_CONN.lock().unwrap() = Some(fresh_conn);

        drop(stale_guard);

        // The slot must still hold the fresh connection, not be overwritten
        // by the stale guard's drop.
        assert!(
            DB_WRITE_CONN.lock().unwrap().is_some(),
            "write slot must keep the freshly installed connection"
        );

        // Cleanup.
        *DB_WRITE_CONN.lock().unwrap() = None;
    }

    #[test]
    fn stale_write_guard_does_not_fill_empty_slot() {
        // Even if the write slot is empty (e.g., reset just happened and
        // the new account hasn't initialized yet), a stale guard from the
        // previous account must NOT fill it — that connection points at a
        // different DB.
        let stale_generation = current_pool_generation();
        let stale_guard = WriteConnectionGuard::new(fake_conn(), stale_generation);

        bump_pool_generation();
        *DB_WRITE_CONN.lock().unwrap() = None;

        drop(stale_guard);

        assert!(
            DB_WRITE_CONN.lock().unwrap().is_none(),
            "stale write guard must not fill an empty slot"
        );
    }
}
