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
use std::ops::{Deref, DerefMut};

use serde::{Deserialize, Serialize};

pub mod settings;
pub mod schema;
pub mod profiles;
pub mod id_cache;
pub mod events;
pub mod attachments;
pub mod chats;
pub mod wrappers;
pub mod nip17_keys;
pub mod community;
pub mod bots;

pub use settings::{
    get_sql_setting, set_sql_setting, advance_u64_setting, get_pkey, set_pkey, get_seed, set_seed, remove_setting,
    get_signer_type, set_signer_type,
    get_bunker_url, set_bunker_url,
    get_bunker_remote_pubkey, set_bunker_remote_pubkey,
    commit_bunker_account_setup,
    get_nip55_user_pubkey, set_nip55_user_pubkey,
    get_nip55_signer_package, set_nip55_signer_package,
    commit_nip55_account_setup,
};

// ============================================================================
// App Data Directory
// ============================================================================

static APP_DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn set_app_data_dir(path: PathBuf) {
    let _ = APP_DATA_DIR.set(path);
}

/// The data dir every test must use — ONE per process, alive for its whole life.
///
/// `APP_DATA_DIR` is set-once, so in a test binary only the first
/// `set_app_data_dir` ever takes effect: every later helper silently kept using
/// the FIRST test's `TempDir`, which was deleted the moment that test returned,
/// leaving the rest to build account dirs under a path that no longer existed.
/// Which test drew the short straw depended on thread scheduling, so the failures
/// wandered. Tests already isolate by account subdirectory, so sharing the root is
/// safe; what they cannot share is a lifetime owned by one of them.
#[cfg(test)]
pub(crate) fn shared_test_data_dir() -> &'static std::path::Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    // Never dropped: process-lifetime by construction, so no test can pull it out
    // from under another. The OS reclaims it after the run.
    DIR.get_or_init(|| tempfile::tempdir().expect("test data dir")).path()
}

pub fn get_app_data_dir() -> Result<&'static PathBuf, String> {
    APP_DATA_DIR.get().ok_or_else(|| "App data directory not initialized".to_string())
}

/// Host app version, stamped into each account on open so a later downgrade can
/// name what wrote the schema. Optional: the downgrade guard keys off the
/// migration high-water mark, never this.
static APP_VERSION: OnceLock<String> = OnceLock::new();

pub fn set_app_version(version: impl Into<String>) {
    let _ = APP_VERSION.set(version.into());
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

/// One account's database resources, held behind an `Arc`.
///
/// The connections an account uses are reachable ONLY through its own
/// `Session`, so a task that captured one keeps talking to the account it
/// started with even after a swap — it cannot be handed the next account's
/// database, because it never asks "who is current?" again.
///
/// That makes teardown structural: swapping installs a new `Session` and drops
/// the reference to the old one, whose pool closes when the last in-flight
/// guard finishes with it. Nothing to clear, nothing to remember to clear.
pub struct Session {
    /// Identity for "is this the account on screen?".
    ///
    /// Not the `Arc` address: binding a session to a database builds a new one
    /// holding the same state (see `rebound`), and a task bound to the session
    /// it grew from is still that account's. Comparing addresses would make
    /// those tasks silently stop painting after a re-initialise.
    id: u64,
    /// The database this session opens, resolved ONCE when it is built.
    /// `None` only before an account is bound, where there is nothing better
    /// than the ambient lookup to fall back to.
    db_path: Option<PathBuf>,
    read_pool: Mutex<Vec<rusqlite::Connection>>,
    write_conn: Mutex<Option<rusqlite::Connection>>,
    /// This account's chats and profiles in memory — the DB's read-through
    /// cache, and so bound to the same account for the same reason.
    chat_state: Arc<tokio::sync::Mutex<crate::state::ChatState>>,
    /// Everything else the account owns, keyed by type. See [`Session::scoped`].
    scoped: RwLock<std::collections::HashMap<std::any::TypeId, Arc<dyn std::any::Any + Send + Sync>>>,
    /// Raised when this account stops being the one on screen.
    stopped: SessionStop,
}

/// "This account is no longer current" — a flag long work polls, and a signal
/// it can await.
#[derive(Default)]
struct SessionStop {
    flag: std::sync::atomic::AtomicBool,
    wake: tokio::sync::Notify,
}

impl Session {
    fn empty() -> Arc<Self> {
        Arc::new(Session {
            id: next_session_id(),
            db_path: None,
            read_pool: Mutex::new(Vec::new()),
            write_conn: Mutex::new(None),
            chat_state: Arc::new(tokio::sync::Mutex::new(crate::state::ChatState::new())),
            scoped: RwLock::new(std::collections::HashMap::new()),
            stopped: SessionStop::default(),
        })
    }

    /// A session bound to one account's database file, with a fresh in-memory
    /// state — the caller loads it from that database.
    fn bound(db_path: PathBuf) -> Arc<Self> {
        Arc::new(Session {
            id: next_session_id(),
            db_path: Some(db_path),
            read_pool: Mutex::new(Vec::new()),
            write_conn: Mutex::new(None),
            chat_state: Arc::new(tokio::sync::Mutex::new(crate::state::ChatState::new())),
            scoped: RwLock::new(std::collections::HashMap::new()),
            stopped: SessionStop::default(),
        })
    }

    /// The same account's session, pointed at its database, keeping everything
    /// it already holds.
    ///
    /// Two callers need this. Re-initialising an account that is already open —
    /// `init_database` is documented idempotent, and the schema check and
    /// Android's background sync both re-run it — must not empty the chat list
    /// under a running app. And a login legitimately fills a session before the
    /// account's database exists: creating an account installs its keys and
    /// client first, and only reaches `init_database` once the user has chosen
    /// a PIN. Binding the session it was filling is a promotion, not a swap.
    fn rebound(&self, db_path: PathBuf) -> Arc<Self> {
        Arc::new(Session {
            id: self.id,
            db_path: Some(db_path),
            read_pool: Mutex::new(Vec::new()),
            write_conn: Mutex::new(None),
            chat_state: self.chat_state.clone(),
            scoped: RwLock::new(self.scoped.read().unwrap_or_else(|e| e.into_inner()).clone()),
            stopped: SessionStop::default(),
        })
    }

    /// This account's instance of `T`, built on first touch.
    ///
    /// The home for anything an account owns beyond its database: caches keyed
    /// by its row ids, queues holding its work, routing tables holding its
    /// keys. Previously these were process globals with a hand-written clear in
    /// two teardown paths, which is how they drifted — one path cleared fields
    /// the other did not, and a cache added without a clear leaked into the
    /// next account. Here there is nothing to clear: the account's instances go
    /// when its session does.
    ///
    /// Keyed by a marker type the owning module declares, so the session never
    /// has to know what any of this is, and two modules storing the same SHAPE
    /// (say a `Mutex<HashMap<PublicKey, _>>`) can't collide on one slot.
    ///
    /// ```ignore
    /// struct InboxRelayCache;               // the key, private to this module
    /// fn cache() -> Arc<Mutex<HashMap<PublicKey, CachedRelays>>> {
    ///     db::current_session().scoped::<InboxRelayCache, _>()
    /// }
    /// ```
    pub fn scoped<K: 'static, T: Default + Send + Sync + 'static>(self: &Arc<Self>) -> Arc<T> {
        let key = std::any::TypeId::of::<(K, T)>();
        if let Some(existing) = self.scoped.read().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return existing.clone().downcast::<T>().expect("keyed by its own TypeId");
        }
        let mut map = self.scoped.write().unwrap_or_else(|e| e.into_inner());
        // Another thread may have inserted while the read lock was released.
        map.entry(key)
            .or_insert_with(|| Arc::new(T::default()) as Arc<dyn std::any::Any + Send + Sync>)
            .clone()
            .downcast::<T>()
            .expect("keyed by its own TypeId")
    }

    /// This session's identity. Stable across a re-initialise of the same
    /// account, distinct for every other. Callers that key a cache by "which
    /// account is this" use it in place of the old generation counter.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Whether this account has been switched away from.
    ///
    /// Purely an efficiency signal. Work that continues past it is still
    /// CORRECT — it writes to this account's own storage and paints nothing —
    /// it is just no longer work anyone is waiting for. Long syncs poll this at
    /// their loop heads so a swap stops the relay traffic and the decryption
    /// rather than grinding on for a screen nobody is looking at.
    ///
    /// Never use it to decide whether a write is SAFE. That is structural now.
    pub fn stopped(&self) -> bool {
        self.stopped.flag.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Resolves once this account is switched away from — for `select!` against
    /// a fetch, so an in-flight request is dropped instead of awaited out.
    pub async fn on_stop(&self) {
        loop {
            let waiting = self.stopped.wake.notified();
            if self.stopped() {
                return;
            }
            waiting.await;
            if self.stopped() {
                return;
            }
        }
    }

    fn stop(&self) {
        self.stopped.flag.store(true, std::sync::atomic::Ordering::Release);
        self.stopped.wake.notify_waiters();
    }

    /// Whether this session is the account currently on screen.
    ///
    /// For code holding a session it captured earlier — a drop handler, a
    /// callback fired by a relay OK — where there is no await to be bound
    /// across and the question is "is what I captured still current?".
    pub fn is_live(&self) -> bool {
        self.id == CURRENT_SESSION.read().unwrap_or_else(|e| e.into_inner()).id
    }

    /// This account's in-memory chats and profiles.
    ///
    /// Reached through the session, so a task bound to account A keeps reading
    /// and writing A's chats after a swap. Its state is then an orphan nothing
    /// displays, rather than corruption in the account now on screen.
    pub fn chat_state(&self) -> Arc<tokio::sync::Mutex<crate::state::ChatState>> {
        self.chat_state.clone()
    }

    /// Where THIS session opens connections. A bound session never consults the
    /// ambient account, so a pool miss after a swap still opens the file this
    /// session belongs to rather than the incoming account's.
    fn path(&self) -> Result<PathBuf, String> {
        match &self.db_path {
            Some(p) => Ok(p.clone()),
            None => get_current_db_path(),
        }
    }

    /// Take a READ connection from this session, opening one against this
    /// session's own database if the pool is empty.
    pub fn acquire_read(self: &Arc<Self>) -> Result<ConnectionGuard, String> {
        if let Ok(mut pool) = self.read_pool.lock() {
            if let Some(conn) = pool.pop() {
                return Ok(ConnectionGuard::new(conn, self.clone()));
            }
        }
        let conn = create_connection(&self.path()?)?;
        Ok(ConnectionGuard::new(conn, self.clone()))
    }

    /// Take THE write connection from this session, opening one against this
    /// session's own database if the slot is empty.
    pub fn acquire_write(self: &Arc<Self>) -> Result<WriteConnectionGuard, String> {
        {
            let mut slot = self.write_conn.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(conn) = slot.take() {
                return Ok(WriteConnectionGuard::new(conn, self.clone()));
            }
        }
        let conn = create_connection(&self.path()?)?;
        Ok(WriteConnectionGuard::new(conn, self.clone()))
    }
}

/// The account whose resources new work binds to. Read once at the START of a
/// unit of work; hold the `Arc` for its duration.
static CURRENT_SESSION: LazyLock<RwLock<Arc<Session>>> = LazyLock::new(|| RwLock::new(Session::empty()));

tokio::task_local! {
    /// The session this task is bound to, installed by [`spawn_bound`].
    ///
    /// Readable from any code running on the task, including the synchronous
    /// `db::` helpers an async body calls — which is what lets every existing
    /// call site become account-correct without growing a parameter.
    static TASK_SESSION: Arc<Session>;
}

/// The session THIS work belongs to.
///
/// A task bound by [`spawn_bound`] gets the account it started under, for its
/// whole life, however many awaits it spans and whoever logs in meanwhile. That
/// is the property the std::sync::Arc<crate::db::Session> checks were approximating by hand.
///
/// Unbound callers (startup, the UI command that performs the swap itself) get
/// the live account, which for them is the correct and only meaningful answer.
pub fn current_session() -> Arc<Session> {
    TASK_SESSION
        .try_with(Arc::clone)
        .unwrap_or_else(|_| CURRENT_SESSION.read().unwrap_or_else(|e| e.into_inner()).clone())
}

/// Run `fut` pinned to the CURRENT account, without spawning.
///
/// The counterpart to [`spawn_bound`] for work that is NOT a task we started:
/// a Tauri command, an SDK call, a JNI entry point. Nothing installs a session
/// for those, so every `db::` call inside them re-resolves the live account and
/// a swap mid-operation silently moves the rest of their writes. Wrapping the
/// body fixes the account for its whole duration — a swap then leaves the
/// operation completing against the account that asked for it, which is both
/// the correct outcome and the one the user intended.
///
/// This is what makes the hand-written "did the account change?" checks
/// unnecessary rather than merely redundant.
/// NOT an `async fn`, and that is load-bearing. An `async fn` stores its
/// parameters in the state machine it returns, so `scoped` would be sized to
/// hold the body it binds — and nesting would multiply, exactly as passing the
/// future to `scope` unboxed does. A plain fn returning `impl Future` hands
/// back a wrapper around a `Pin<Box<_>>`: a pointer, whatever the body.
///
/// This is not hypothetical. Boot crashed with a stack overflow on BOTH Android
/// and macOS, several layers into the community sync, on stacks every test here
/// runs on happily. `binding_a_future_does_not_embed_it` is the guard.
pub fn scoped<F: std::future::Future>(fut: F) -> impl std::future::Future<Output = F::Output> {
    TASK_SESSION.scope(current_session(), Box::pin(fut))
}

/// [`scoped`], but the RESULT is refused if the account changed while it ran.
///
/// For an operation whose value goes back to the UI. The work itself completed
/// correctly against its own account; what must not happen is that value being
/// painted into, or acted on by, the account now on screen. One wrapper in
/// place of a check before every write.
pub fn scoped_result<T, E, F>(fut: F) -> impl std::future::Future<Output = Result<T, E>>
where
    F: std::future::Future<Output = Result<T, E>>,
    E: From<String>,
{
    let session = current_session();
    let id = session.id;
    // Bind first, so the async block below holds only the (pointer-sized)
    // bound future rather than the body. See `scoped`.
    let bound = TASK_SESSION.scope(session, Box::pin(fut));
    async move {
        let out = bound.await;
        if id != CURRENT_SESSION.read().unwrap_or_else(|e| e.into_inner()).id {
            return Err(E::from("account changed during the operation".to_string()));
        }
        out
    }
}

/// Run `fut` pinned to a session you already hold.
///
/// For code that captured a session earlier and wants to finish that account's
/// work through it — and for tests, which need to read an account's storage
/// after the live session has moved on.
pub fn with_session<F: std::future::Future>(
    session: Arc<Session>,
    fut: F,
) -> impl std::future::Future<Output = F::Output> {
    TASK_SESSION.scope(session, Box::pin(fut))
}

/// Spawn a task pinned to the CURRENT account.
///
/// Everything it does through `db::` resolves to that account for as long as it
/// runs, so an account switch mid-flight can no longer redirect its writes. Use
/// this for any task that touches per-account state; a bare `tokio::spawn`
/// leaves the task reading whoever is live at the moment it asks.
pub fn spawn_bound<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let session = current_session();
    // spawn-detached: this IS the binding — it installs the session it just read.
    tokio::spawn(TASK_SESSION.scope(session, fut))
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("id", &self.id).field("db", &self.db_path).finish()
    }
}

/// Has the account this work belongs to been switched away from?
///
/// Check it at the head of a long loop — a sync page, a relay in a fan-out, a
/// channel in a sweep — and stop. Continuing is safe, just wasteful.
pub fn session_stopped() -> bool {
    current_session().stopped()
}

/// The live account's session id — for caches keyed by "which account is this".
pub fn current_session_id() -> u64 {
    current_session().id
}

/// Whether the work running here belongs to the account currently on screen.
///
/// There is exactly one UI, showing one account. A task bound to a previous
/// account keeps working correctly — its database, its chats, its client — but
/// what it produces must not be painted into the account the user is now
/// looking at. Every emission asks this, which is why almost none of them has
/// to ask it by hand.
pub fn session_is_live() -> bool {
    current_session().id == CURRENT_SESSION.read().unwrap_or_else(|e| e.into_inner()).id
}

fn next_session_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Install a fresh session, dropping the reference to the previous one. Any
/// guard still outstanding against the old session returns its connection
/// there, and that pool closes with it.
fn replace_session() {
    install(Session::empty());
}

/// Install `next` and tell the outgoing account's work to stop.
fn install(next: Arc<Session>) {
    let mut current = CURRENT_SESSION.write().unwrap_or_else(|e| e.into_inner());
    if current.id != next.id {
        current.stop();
    }
    *current = next;
}

/// RAII guard for READ connections — auto-returns to its OWN session's pool.
///
/// Holding the `Arc` is what makes the return safe: the connection goes back
/// where it came from, so a guard outstanding across an account swap can never
/// hand account A's connection to account B. When that older session has no
/// references left, its pool closes with it.
pub struct ConnectionGuard {
    conn: Option<rusqlite::Connection>,
    session: Arc<Session>,
}

impl ConnectionGuard {
    fn new(conn: rusqlite::Connection, session: Arc<Session>) -> Self {
        Self { conn: Some(conn), session }
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
            if let Ok(mut pool) = self.session.read_pool.lock() {
                pool.push(conn);
            }
        }
    }
}

/// RAII guard for the WRITE connection — auto-returns on drop.
pub struct WriteConnectionGuard {
    conn: Option<rusqlite::Connection>,
    session: Arc<Session>,
}

impl WriteConnectionGuard {
    fn new(conn: rusqlite::Connection, session: Arc<Session>) -> Self {
        Self { conn: Some(conn), session }
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
            // Slot-empty check remains: a fresh write connection may already
            // have been installed for this same session.
            if let Ok(mut slot) = self.session.write_conn.lock() {
                if slot.is_none() {
                    *slot = Some(conn);
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

/// Open a connection, riding out a TRANSIENT lock on the file.
///
/// `busy_timeout` governs waits inside a connection that already exists; it
/// cannot help the open itself, and `journal_mode=WAL` needs the lock briefly.
/// A task started under the previous account can still be holding this file for
/// a moment — it resolves the CURRENT account when it takes a connection, so a
/// swap points it here — and failing outright turns that into a failed login or
/// a failed account switch. Bounded: a genuinely stuck holder still surfaces.
fn create_connection(path: &PathBuf) -> Result<rusqlite::Connection, String> {
    const OPEN_RETRIES: u32 = 4;
    let mut last_err = String::new();
    for attempt in 0..OPEN_RETRIES {
        match open_connection(path) {
            Ok(conn) => return Ok(conn),
            Err(e) if e.contains("locked") || e.contains("busy") => {
                last_err = e;
                std::thread::sleep(std::time::Duration::from_millis(50 * u64::from(attempt + 1)));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err)
}

fn open_connection(path: &PathBuf) -> Result<rusqlite::Connection, String> {
    let conn = rusqlite::Connection::open(path)
        .map_err(|e| format!("Failed to open database: {}", e))?;

    // busy_timeout FIRST, alone: `journal_mode=WAL` takes a brief exclusive lock,
    // and until the timeout is set it is still 0 — so a connection opened while
    // another is active failed outright ("database is locked") instead of waiting
    // the moment or two the other needed. Every pragma below now waits too.
    conn.execute_batch("PRAGMA busy_timeout=5000;")
        .map_err(|e| format!("Failed to set busy_timeout: {}", e))?;

    // WAL for concurrent reads. cache_size negative = KiB (16 MiB page cache) to keep hot
    // pages resident on a large DB; temp_store=MEMORY keeps GROUP BY / sort scratch in
    // memory instead of spilling to disk.
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA cache_size=-16000; PRAGMA temp_store=MEMORY;")
        .map_err(|e| format!("Failed to set pragmas: {}", e))?;

    Ok(conn)
}

/// Get a READ connection (headless-safe — no AppHandle).
pub fn get_db_connection_guard_static() -> Result<ConnectionGuard, String> {
    current_session().acquire_read()
}

/// Process-wide serialization lock for tests that install into the global DB pool.
/// Any test calling `init_database` must hold this for its whole body — otherwise
/// concurrent inits race on the shared account/data-dir state and clobber each other.
/// One shared guard across every module (community, ...) so cross-module test
/// parallelism can't collide.
#[cfg(test)]
pub(crate) static DB_TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Get the WRITE connection (headless-safe — no AppHandle).
pub fn get_write_connection_guard_static() -> Result<WriteConnectionGuard, String> {
    current_session().acquire_write()
}

// ============================================================================
// Downgrade guard
// ============================================================================

/// Where each build stamps its version after successfully opening an account.
const LAST_APP_VERSION_KEY: &str = "last_app_version";

/// A newer Vector already opened this account's database.
///
/// Vector has no downgrade path: older builds neither recognise nor preserve
/// newer schema, and the corruption that follows is silent until the user opens
/// the wrong chat.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DowngradeBlock {
    /// Migration high-water mark found in the account's DB.
    pub db_schema: u32,
    /// Highest migration this build can apply.
    pub supported_schema: u32,
    /// App version that last opened it, when one was stamped.
    pub last_app_version: Option<String>,
}

impl std::fmt::Display for DowngradeBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "This account was last opened by a newer version of Vector")?;
        if let Some(version) = &self.last_app_version {
            write!(f, " ({version})")?;
        }
        write!(
            f,
            ". Its database is at schema {} and this build only understands {}. \
             Opening it would corrupt your messages, so Vector has stopped. \
             Reinstall the newer version to continue.",
            self.db_schema, self.supported_schema
        )
    }
}

/// The check itself, against an already-open connection.
fn downgrade_block(conn: &rusqlite::Connection) -> Option<DowngradeBlock> {
    let db_schema = schema::applied_migration_high_water(conn);
    if db_schema <= schema::HIGHEST_MIGRATION_ID {
        return None;
    }
    Some(DowngradeBlock {
        db_schema,
        supported_schema: schema::HIGHEST_MIGRATION_ID,
        last_app_version: conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                rusqlite::params![LAST_APP_VERSION_KEY],
                |row| row.get::<_, String>(0),
            )
            .ok(),
    })
}

/// Whether this account's DB was written by a newer Vector.
///
/// Read-only and side-effect free, so a host can call it before
/// [`init_database`] and put a real dialog in front of the user rather than
/// surfacing a failed boot.
pub fn inspect_downgrade(npub: &str) -> Result<Option<DowngradeBlock>, String> {
    let db_path = account_dir(npub)?.join("vector.db");
    // Never create the file just to inspect it.
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = create_connection(&db_path)?;
    Ok(downgrade_block(&conn))
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

    // Before ANY write. SQL_SCHEMA is all CREATE TABLE IF NOT EXISTS, so an
    // older build would resurrect tables newer migrations dropped and then
    // start writing rows against a schema it cannot see.
    if let Some(block) = downgrade_block(&conn) {
        return Err(block.to_string());
    }

    conn.execute_batch(schema::SQL_SCHEMA)
        .map_err(|e| format!("Failed to create schema: {}", e))?;

    // Run migrations
    schema::run_migrations(&mut conn)?;

    // Stamped after migrations, so it names the build whose schema is now on
    // disk. A blocked build never reaches here, so this only ever records a
    // version that could actually read what it wrote.
    if let Some(version) = APP_VERSION.get() {
        let _ = conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![LAST_APP_VERSION_KEY, version],
        );
    }

    // SQLite's prescribed open-time step for long-lived connections: analyze every table that needs
    // it (missing stats, or grown/shrunk 10x), bounded by a temporary analysis_limit so it stays
    // fast. The 0x10000 bit forces checking all tables since a fresh connection has no query history;
    // this also satisfies the "run optimize after CREATE INDEX" guidance for the migrations above.
    let _ = conn.execute_batch("PRAGMA optimize=0x10002;");

    // Seed the in-session delete-tombstone set from this account's durable rows,
    // so ingest keeps refusing deleted messages across restarts (the swap's
    // session bump cleared the set; a fresh boot starts empty).
    {
        let mut stmt = conn.prepare("SELECT event_id FROM deleted_messages")
            .map_err(|e| format!("tombstone seed prepare: {}", e))?;
        let ids: Vec<String> = stmt.query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("tombstone seed query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);
        crate::state::seed_message_tombstones(ids);
    }

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

    // Install a NEW session before opening anything: guards still outstanding
    // against the previous one return their connections there, and that pool
    // closes with it. Nothing from the old account can reach the new pool.
    //
    // A session already pointed at this database, or not yet pointed at one at
    // all, is THIS account's — bind it and keep what it holds (see `rebound`).
    // Only a session belonging to a different account starts over.
    let session = {
        let next = {
            let current = CURRENT_SESSION.read().unwrap_or_else(|e| e.into_inner());
            match current.db_path.as_deref() {
                Some(p) if p != db_path => Session::bound(db_path.clone()),
                _ => current.rebound(db_path.clone()),
            }
        };
        install(next.clone());
        next
    };

    // Pre-warm read pool
    if let Ok(mut pool) = session.read_pool.lock() {
        for _ in 0..4 {
            if let Ok(c) = create_connection(&db_path) {
                pool.push(c);
            }
        }
    }

    // Set write connection
    let write_conn = create_connection(&db_path)?;
    *session.write_conn.lock().unwrap_or_else(|e| e.into_inner()) = Some(write_conn);

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

/// Drop this account's database resources.
///
/// Installing a fresh session IS the teardown: the previous one is released,
/// and its connections close once the last outstanding guard returns. A guard
/// still in flight keeps serving the account it began under.
pub fn close_database() {
    replace_session();
}

/// Run plain `PRAGMA optimize` on the live write connection — the periodic top-up SQLite recommends
/// for long-lived connections (the heavy lifting is the `optimize=0x10002` at connection open).
/// Best-effort and cheap: re-analyzes only tables whose stats the planner used and that changed
/// materially since the last run.
pub fn optimize_database() {
    let session = current_session();
    let guard = session.write_conn.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(conn) = guard.as_ref() {
        let _ = conn.execute_batch("PRAGMA optimize;");
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

/// Clear every id cache on account switch. Row ids are PER-ACCOUNT (each account
/// has its own DB + id sequence), so a stale entry after a swap points into the
/// wrong DB: writes FK-fail silently and reads hit the wrong row. The caches live
/// in `id_cache`; this is the public entry the swap path + callers already use.
pub fn clear_id_caches() {
    id_cache::clear_id_caches();
    community::clear_banlist_cache();
    community::clear_channel_community_cache();
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

    /// Build a minimal in-memory SQLite connection — just enough to drop
    /// a connection through the guard machinery. We don't run schema or
    /// migrations because we only care about the guard's Drop pathway.
    fn fake_conn() -> rusqlite::Connection {
        rusqlite::Connection::open_in_memory().unwrap()
    }

    #[test]
    fn close_database_installs_a_fresh_session() {
        // The guard is the POINT here, not boilerplate: `close_database`
        // replaces the process-global session, so running unguarded yanks the
        // database out from under whichever test is mid-query.
        let _guard = DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let before = current_session();
        close_database();
        let after = current_session();
        assert!(
            !Arc::ptr_eq(&before, &after),
            "close_database must install a NEW session — the old one is what in-flight guards return to"
        );
    }

    #[test]
    fn a_guard_returns_its_connection_to_its_own_session() {
        let session = Session::empty();
        drop(ConnectionGuard::new(fake_conn(), session.clone()));
        assert_eq!(session.read_pool.lock().unwrap().len(), 1, "the connection goes home");
    }

    #[test]
    fn a_guard_outstanding_across_a_swap_returns_to_the_session_it_came_from() {
        // The property that replaced the generation counter, and a stronger
        // one: the old design could only assert a stale connection did NOT
        // pollute the new pool. Holding the Arc says where it actually went,
        // so account A's connection is unreachable from account B by
        // construction rather than by a comparison someone has to remember.
        let old = Session::empty();
        let new_session = Session::empty();

        let guard = ConnectionGuard::new(fake_conn(), old.clone());
        // ...the account switches while the guard is still in flight...
        drop(guard);

        assert_eq!(old.read_pool.lock().unwrap().len(), 1, "returned to the session it was taken from");
        assert_eq!(new_session.read_pool.lock().unwrap().len(), 0, "never reachable from the new account");
    }

    #[test]
    fn a_pool_miss_after_a_swap_opens_the_sessions_own_database() {
        // The other half of the guarantee. Returning a connection was already
        // safe; ACQUIRING one was not, because a miss resolved the ambient
        // account and could open the incoming account's file into the outgoing
        // account's pool. A bound session never asks what is current.
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.db");
        let path_b = dir.path().join("b.db");

        let session_a = Session::bound(path_a.clone());
        let _unrelated = Session::bound(path_b.clone());

        // A task still holding A misses A's (empty) pool and opens a connection.
        let guard = session_a.acquire_read().expect("acquire against the held session");
        // Compare by filename: macOS reports the /private-resolved path.
        let opened = guard.path().expect("a file-backed connection").to_string();
        assert!(
            opened.ends_with("a.db") && !opened.ends_with("b.db"),
            "a miss opens the session's OWN database, never the incoming account's (opened {opened})"
        );
        assert!(!Arc::ptr_eq(&session_a, &current_session()), "the live session really did move on");
    }

    /// Run `fut` bound to `session`, exactly as `spawn_bound` would.
    ///
    /// These tests never install their throwaway sessions globally. They used
    /// to, and every other test in the binary reads chats, caches and keys
    /// through the live session — so one of these swapping it mid-run pulled
    /// them out from under whatever was executing in parallel. Binding proves
    /// the same property without a global write.
    async fn bound_to<F: std::future::Future>(session: Arc<Session>, fut: F) -> F::Output {
        TASK_SESSION.scope(session, fut).await
    }

    /// Every task that touches per-account state must be bound to an account.
    ///
    /// The worklist is empty: every spawn in this crate either binds or says on
    /// the line why it owns no account state. See [`crate::spawn_audit`].
    #[test]
    fn per_account_tasks_are_spawned_bound_to_their_account() {
        crate::spawn_audit::assert_all_spawns_bound(std::path::Path::new(env!("CARGO_MANIFEST_DIR")), &[]);
    }

    #[tokio::test]
    async fn a_bound_task_keeps_its_account_across_a_swap() {
        // What the std::sync::Arc<crate::db::Session> checks were approximating by hand: the task
        // began under account A, so its work resolves to A however many awaits
        // it spans and whoever logs in meanwhile. No check, nothing to forget.
        // The live session stands in for whoever logged in next.
        let dir = tempfile::tempdir().unwrap();
        let a = Session::bound(dir.path().join("a.db"));

        let seen = bound_to(a.clone(), async {
            tokio::task::yield_now().await;
            current_session()
        })
        .await;

        assert!(Arc::ptr_eq(&seen, &a), "the task still sees the account it started under");
        assert!(!Arc::ptr_eq(&seen, &current_session()), "the live account is not reachable from it");
    }

    #[tokio::test]
    async fn a_bound_tasks_chat_writes_cannot_reach_the_new_account() {
        // The bug this closes, in the form it actually took: a task holding a
        // chat id from account A finishes after the swap and inserts it into
        // whatever STATE it finds. The group chats that appeared in a freshly
        // created account arrived exactly this way.
        use crate::chat::{Chat, ChatType};
        let dir = tempfile::tempdir().unwrap();
        let a = Session::bound(dir.path().join("a.db"));
        let live_before = current_session().chat_state().lock().await.chats.len();

        bound_to(a.clone(), async {
            tokio::task::yield_now().await;
            crate::state::STATE.lock().await.chats.push(Chat::new("a-chat".into(), ChatType::DirectMessage, Vec::new()));
        })
        .await;

        assert_eq!(
            a.chat_state().lock().await.chats.len(),
            1,
            "it landed in the state of the account the task began under"
        );
        assert_eq!(
            current_session().chat_state().lock().await.chats.len(),
            live_before,
            "and nothing reached the account on screen"
        );
    }

    #[tokio::test]
    async fn a_bound_task_cannot_publish_through_the_new_accounts_client() {
        // The client carries the signer, so reaching the wrong one means
        // publishing account A's payload under account B's identity. A bound
        // task reaches the client it started with, which the swap has already
        // shut down — its send fails instead of succeeding as the wrong person.
        use nostr_sdk::prelude::*;
        let dir = tempfile::tempdir().unwrap();
        let a = Session::bound(dir.path().join("a.db"));
        let a_identity = Keys::generate().public_key();

        let seen = bound_to(a.clone(), async move {
            crate::state::set_my_public_key(a_identity);
            tokio::task::yield_now().await;
            crate::state::my_public_key()
        })
        .await;

        assert_eq!(seen, Some(a_identity), "the task signs as the account it began under");
        assert_ne!(crate::state::my_public_key(), Some(a_identity), "and the live account is someone else");
    }

    #[test]
    fn binding_an_unbound_session_to_a_database_keeps_what_it_holds() {
        // Creating an account installs its keys and client BEFORE its database
        // exists — `init_database` only runs once the user has chosen a PIN.
        // Replacing the session there would discard the login in progress.
        let dir = tempfile::tempdir().unwrap();
        let staging = Session::empty();
        let held = staging.scoped::<Session, Mutex<u8>>();
        *held.lock().unwrap() = 7;

        let promoted = staging.rebound(dir.path().join("a.db"));
        assert_eq!(*promoted.scoped::<Session, Mutex<u8>>().lock().unwrap(), 7, "the login survives being bound");
        assert!(promoted.db_path.is_some(), "and it now has a database");
        assert_eq!(promoted.id, staging.id, "and it is still the same account, so its tasks keep painting");
    }

    #[tokio::test]
    async fn a_bound_task_paints_nothing_into_the_account_on_screen() {
        // Binding fixes where work LANDS; it cannot fix what the user SEES,
        // because there is one UI showing one account. So emission asks once,
        // centrally, and a task belonging to a previous account goes quiet.
        let dir = tempfile::tempdir().unwrap();
        let previous = Session::bound(dir.path().join("previous.db"));
        assert!(session_is_live(), "work for the account on screen paints");

        let painted = bound_to(previous, async {
            tokio::task::yield_now().await;
            session_is_live()
        })
        .await;

        assert!(!painted, "a task bound to another account paints nothing");
        assert!(session_is_live(), "and the account now on screen still does");
    }

    #[tokio::test]
    async fn switching_accounts_tells_the_previous_one_to_stop() {
        // Purely an efficiency signal — the work would still be correct — but a
        // boot sync is minutes of relay traffic and decryption, and after a swap
        // nobody is waiting for any of it.
        let _serialized = DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let previous = current_session();
        assert!(!previous.stopped(), "running work is not told to stop");

        // `on_stop` must be pending until the switch, then resolve.
        let waiter = { let p = previous.clone(); tokio::spawn(async move { p.on_stop().await }) };
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished(), "nothing to report while the account is current");

        close_database();

        assert!(previous.stopped(), "the outgoing account is told to stop");
        assert!(!current_session().stopped(), "the incoming one is not");
        tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .expect("on_stop resolves on the switch")
            .expect("without panicking");
    }

    #[test]
    fn re_initialising_the_same_account_does_not_tell_it_to_stop() {
        // `init_database` runs more than once per account — the schema check at
        // boot, Android's background sync. Reading that as a swap would abort a
        // sync that is still perfectly wanted.
        let dir = tempfile::tempdir().unwrap();
        let staging = Session::empty();
        let promoted = staging.rebound(dir.path().join("a.db"));
        assert!(!staging.stopped(), "binding a session is not switching away from it");
        assert_eq!(promoted.id, staging.id);
    }

    /// Binding must not grow with what it binds.
    ///
    /// `scope` takes the future BY VALUE, so an unboxed `scoped` embeds the
    /// whole inner state machine in its caller's. Nesting then multiplies, and
    /// a deep chain of bound calls overflows a worker stack — which is exactly
    /// what happened: boot crashed on BOTH Android and macOS, several layers
    /// into the community sync, on stacks every one of these tests runs on
    /// happily. Sizes are what the compiler gives us; the invariant is that the
    /// wrapper adds a bounded amount regardless of the body.
    #[test]
    fn binding_a_future_does_not_embed_it() {
        // A deliberately fat body: 8KB of state the future must carry.
        let fat = async {
            let block = [0u8; 8192];
            tokio::task::yield_now().await;
            block[0]
        };
        let fat_size = std::mem::size_of_val(&fat);
        assert!(fat_size >= 8192, "the body really is large ({fat_size})");

        let bound = scoped(fat);
        let bound_size = std::mem::size_of_val(&bound);
        assert!(
            bound_size < 1024,
            "binding must cost a pointer, not a copy of the body \
             (body {fat_size} bytes, bound {bound_size})"
        );
    }

    #[test]
    fn a_dropped_session_closes_its_pool() {
        // Teardown IS the drop: no clearing step to forget.
        let session = Session::empty();
        drop(ConnectionGuard::new(fake_conn(), session.clone()));
        assert_eq!(Arc::strong_count(&session), 1, "the guard released its reference");
        drop(session); // pool + connections close here
    }

    #[test]
    fn a_stale_write_guard_cannot_clobber_the_new_accounts_connection() {
        // Under the old design the stale guard and the fresh connection shared
        // one global slot, so the guard's Drop had to be talked out of
        // overwriting it. They are now in different sessions and cannot meet.
        let old = Session::empty();
        let new_session = Session::empty();

        let stale_guard = WriteConnectionGuard::new(fake_conn(), old.clone());
        // init_database installs the new account's write connection.
        *new_session.write_conn.lock().unwrap() = Some(fake_conn());

        drop(stale_guard);

        assert!(
            new_session.write_conn.lock().unwrap().is_some(),
            "the new account's write connection is untouched"
        );
        assert!(
            old.write_conn.lock().unwrap().is_some(),
            "the stale guard returned to its own session's slot"
        );
    }

    #[test]
    fn a_new_accounts_empty_write_slot_stays_empty() {
        // The old worry: a stale guard filling the fresh account's empty slot
        // with a connection pointing at a different database. It has no way to
        // reach that slot now — it only knows its own session.
        let old = Session::empty();
        let new_session = Session::empty();

        let stale_guard = WriteConnectionGuard::new(fake_conn(), old.clone());
        drop(stale_guard);

        assert!(
            new_session.write_conn.lock().unwrap().is_none(),
            "the new account's slot is untouched by the previous account's guard"
        );
    }
}

#[cfg(test)]
mod downgrade_tests {
    use super::*;

    fn test_account() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>, String) {
        let guard = DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        close_database();
        clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        // Bases must not collide across test modules: APP_DATA_DIR is a
        // OnceLock shared by the whole binary, and this generator collapses
        // after ~4 chars, so nearby seeds yield near-identical npubs.
        // Taken: 0, 900, 5_000, 50_000, 70_000, 71_000, 81_000, 90_000.
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(61_000);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        const B: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let mut acct = String::from("npub1");
        let mut v = n as usize;
        for _ in 0..58 {
            acct.push(B[v % 32] as char);
            v = v / 32 + 7;
        }
        set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        set_current_account(acct.clone()).unwrap();
        (tmp, guard, acct)
    }

    #[tokio::test]
    async fn re_initialising_the_same_account_keeps_its_loaded_state() {
        // init_database is documented idempotent, and callers lean on that —
        // the schema check and Android's background sync both re-run it against
        // an account already loaded. Dropping its chats there would empty the
        // list under a running app.
        use crate::chat::{Chat, ChatType};
        let (_dir, _lock, acct) = test_account();
        init_database(&acct).unwrap();
        crate::state::STATE.lock().await.chats.push(Chat::new("kept".into(), ChatType::DirectMessage, Vec::new()));

        init_database(&acct).unwrap();
        assert_eq!(crate::state::STATE.lock().await.chats.len(), 1, "same account, same in-memory state");
    }

    /// A DB this build fully understands must open, or the guard is useless.
    #[test]
    fn an_equal_schema_opens_normally() {
        let (_tmp, _guard, acct) = test_account();
        init_database(&acct).unwrap();
        assert!(inspect_downgrade(&acct).unwrap().is_none());
        // Re-opening is still fine: the stamp write must not trip the guard.
        init_database(&acct).unwrap();
        assert!(inspect_downgrade(&acct).unwrap().is_none());
    }

    /// Absent DB is not a downgrade; it must not be created just to look.
    #[test]
    fn a_missing_database_is_not_a_downgrade() {
        let (_tmp, _guard, acct) = test_account();
        assert!(inspect_downgrade(&acct).unwrap().is_none());
        assert!(!account_dir(&acct).unwrap().join("vector.db").exists());
    }

    #[test]
    fn a_newer_schema_blocks_the_open_and_names_the_build() {
        let (_tmp, _guard, acct) = test_account();
        init_database(&acct).unwrap();

        // Stand in for a newer Vector having run one migration past this build.
        let db_path = account_dir(&acct).unwrap().join("vector.db");
        {
            let conn = create_connection(&db_path).unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO schema_migrations (id, applied_at) VALUES (?1, 0)",
                rusqlite::params![schema::HIGHEST_MIGRATION_ID + 1],
            )
            .unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                rusqlite::params![LAST_APP_VERSION_KEY, "9.9.9"],
            )
            .unwrap();
        }
        close_database();

        let block = inspect_downgrade(&acct)
            .unwrap()
            .expect("a higher migration id must read as a downgrade");
        assert_eq!(block.db_schema, schema::HIGHEST_MIGRATION_ID + 1);
        assert_eq!(block.supported_schema, schema::HIGHEST_MIGRATION_ID);
        assert_eq!(block.last_app_version.as_deref(), Some("9.9.9"));

        let err = init_database(&acct).unwrap_err();
        assert!(err.contains("9.9.9"), "must name the newer build: {err}");
    }

    /// The guard has to fire before SQL_SCHEMA runs: its CREATE TABLE IF NOT
    /// EXISTS statements would otherwise resurrect tables newer migrations drop.
    #[test]
    fn a_blocked_open_writes_nothing() {
        let (_tmp, _guard, acct) = test_account();
        init_database(&acct).unwrap();
        let db_path = account_dir(&acct).unwrap().join("vector.db");
        {
            let conn = create_connection(&db_path).unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO schema_migrations (id, applied_at) VALUES (?1, 0)",
                rusqlite::params![schema::HIGHEST_MIGRATION_ID + 5],
            )
            .unwrap();
            conn.execute("DROP TABLE IF EXISTS settings", []).unwrap();
        }
        close_database();

        assert!(init_database(&acct).is_err());

        // SQL_SCHEMA would have recreated `settings`; it must still be gone.
        let conn = create_connection(&db_path).unwrap();
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='settings'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        assert!(!exists, "a blocked open must not write to the database");
    }
}
