//! The welcome screen's network choice: remembered per install, carried into the first account
//! that commits, and only ever raising an account's own choice.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use super::i2p_config::SamSecret;
use super::prefs::StoredKind;
use super::Kind;

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PreloginChoice {
    pub kind: Kind,
    /// Connection options, per kind and opaque here (I2P: `sam_port`). Never a secret.
    #[serde(default)]
    pub opts: serde_json::Value,
    /// Router credentials (I2P's SAM login), carried in memory to the account that commits.
    #[serde(skip)]
    pub auth: Option<Credentials>,
}

/// A router login: never in the marker, never printed, the password zeroized when dropped.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub user: String,
    pub password: SamSecret,
}

impl PreloginChoice {
    pub fn new(kind: Kind) -> Self {
        Self::with_opts(kind, serde_json::json!({}))
    }

    pub fn with_opts(kind: Kind, opts: serde_json::Value) -> Self {
        PreloginChoice { kind, opts, auth: None }
    }
}

impl std::fmt::Debug for PreloginChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreloginChoice")
            .field("kind", &self.kind)
            .field("opts", &self.opts)
            .field("auth", &self.auth.as_ref().map(|_| ".."))
            .finish()
    }
}

#[cfg(not(target_arch = "wasm32"))]
const MARKER: &str = "transport_prelogin";
#[cfg(not(target_arch = "wasm32"))]
const LEGACY_MARKER: &str = "tor_prelogin";

/// The remembered choice. `Ok(None)` is Clearnet; an unreadable marker is an error, and the
/// welcome screen then shows the chooser with nothing picked.
pub fn marker() -> Result<Option<PreloginChoice>, String> {
    #[cfg(target_arch = "wasm32")]
    {
        Ok(None)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Ok(dir) = crate::db::get_app_data_dir() else { return Ok(None) };
        let read = match std::fs::read(dir.join(MARKER)) {
            Ok(bytes) => serde_json::from_slice::<PreloginChoice>(&bytes)
                .map(|choice| (choice.kind != Kind::Clearnet).then_some(choice))
                .map_err(|e| format!("unreadable network marker: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(dir.join(LEGACY_MARKER).is_file().then(|| PreloginChoice::new(Kind::Tor)))
            }
            Err(e) => Err(format!("unreadable network marker: {e}")),
        };
        remember_seen(dir, read.as_ref().map(|c| c.as_ref().map(|c| c.kind)).map_err(|_| UnreadableMarker));
        read
    }
}

/// The install's marker exists but can't be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnreadableMarker;

/// The remembered kind (`None` is Clearnet) as a view shows it.
pub type MarkerKind = Result<Option<Kind>, UnreadableMarker>;

/// The remembered kind as last read or written, per data dir: status views ask on every poll and
/// event, so they never touch the disk.
#[cfg(not(target_arch = "wasm32"))]
static SEEN: Mutex<Option<(std::path::PathBuf, MarkerKind)>> = Mutex::new(None);

#[cfg(not(target_arch = "wasm32"))]
fn remember_seen(dir: &std::path::Path, kind: MarkerKind) {
    *SEEN.lock().unwrap_or_else(|e| e.into_inner()) = Some((dir.to_path_buf(), kind));
}

/// The remembered kind, without reading the disk after the first time.
pub fn marker_kind() -> MarkerKind {
    #[cfg(target_arch = "wasm32")]
    {
        Ok(None)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Ok(dir) = crate::db::get_app_data_dir() else { return Ok(None) };
        if let Some((seen, kind)) = SEEN.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if seen.as_path() == dir.as_path() {
                return *kind;
            }
        }
        marker().map(|c| c.map(|c| c.kind)).map_err(|_| UnreadableMarker)
    }
}

/// Remember (or forget, with `None` or Clearnet) the choice. Any non-Clearnet choice also keeps
/// the legacy Tor file, so an older build of this line stays off the direct path.
pub fn set_marker(choice: Option<&PreloginChoice>) -> Result<(), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = choice;
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = crate::db::get_app_data_dir()?;
        let remove = |name: &str| match std::fs::remove_file(dir.join(name)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("remove network marker: {e}")),
            _ => Ok(()),
        };
        // Whatever lands on disk, the next status view reads it back fresh.
        *SEEN.lock().unwrap_or_else(|e| e.into_inner()) = None;
        match choice.filter(|c| c.kind != Kind::Clearnet) {
            Some(c) => {
                std::fs::create_dir_all(dir).map_err(|e| format!("create data dir: {e}"))?;
                let json = serde_json::to_vec(&remembered_part(c)).map_err(|e| e.to_string())?;
                std::fs::write(dir.join(MARKER), json).map_err(|e| format!("write network marker: {e}"))?;
                std::fs::write(dir.join(LEGACY_MARKER), b"1").map_err(|e| format!("write network marker: {e}"))?;
                remember_seen(dir, Ok(Some(c.kind)));
                Ok(())
            }
            None => {
                remove(MARKER)?;
                remove(LEGACY_MARKER)?;
                remember_seen(dir, Ok(None));
                Ok(())
            }
        }
    }
}

/// The options an install may remember: connection options only. Credentials live in memory
/// until the account that commits saves them.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn remembered_part(c: &PreloginChoice) -> PreloginChoice {
    const MARKER_KEYS: &[&str] = &["sam_port"];
    let opts = c
        .opts
        .as_object()
        .map(|o| o.iter().filter(|(k, _)| MARKER_KEYS.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    PreloginChoice::with_opts(c.kind, serde_json::Value::Object(opts))
}

static ARMED: Mutex<Option<PreloginChoice>> = Mutex::new(None);

/// Every account opened while armed runs on this kind until one commits.
pub fn arm(choice: PreloginChoice) {
    if choice.kind == Kind::Clearnet {
        disarm();
        debug_assert!(false, "Clearnet is never armed");
        return;
    }
    *ARMED.lock().unwrap_or_else(|e| e.into_inner()) = Some(choice);
}

pub fn disarm() {
    *ARMED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

pub fn armed() -> Option<PreloginChoice> {
    ARMED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The welcome screen only raises: an account that stored Tor or I2P keeps it.
pub fn effective(stored: &StoredKind) -> Option<Kind> {
    match stored {
        StoredKind::Absent | StoredKind::Kind(Kind::Clearnet) => Some(armed().map_or(Kind::Clearnet, |c| c.kind)),
        StoredKind::Kind(k) => Some(*k),
        StoredKind::Unparseable(_) | StoredKind::ReadError(_) => None,
    }
}

/// The account on screen committed: it keeps the welcome screen's choice when it had none of its
/// own. A failed read or save stays armed (the account stays raised) and is retried.
pub fn commit() {
    if !try_commit() {
        retry_commit();
    }
}

/// False when the account's stored network could not be read or the choice could not be saved.
fn try_commit() -> bool {
    let Some(choice) = armed() else { return true };
    match super::prefs::stored_kind() {
        StoredKind::Absent | StoredKind::Kind(Kind::Clearnet) => {
            if let Err(e) = super::prefs::persist_kind(choice.kind) {
                crate::log_warn!("[Transport] could not save the inherited network: {e}");
                return false;
            }
            if let Err(e) = fill_absent_config(&choice) {
                crate::log_warn!("[Transport] could not save the inherited network options: {e}");
            }
            disarm();
            true
        }
        StoredKind::ReadError(e) => {
            crate::log_warn!("[Transport] could not read the network to save the inherited one: {e}");
            false
        }
        _ => {
            disarm();
            true
        }
    }
}

static RETRYING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Clears the retry flag however the retry ends.
struct Retrying;

impl Drop for Retrying {
    fn drop(&mut self) {
        RETRYING.store(false, Ordering::Release);
    }
}

fn retry_commit() {
    if !crate::rt::can_spawn() || RETRYING.swap(true, Ordering::AcqRel) {
        return;
    }
    let guard = Retrying;
    // Bound: the retry writes this account's database, and stops once it leaves the screen.
    crate::db::spawn_bound(async move {
        let _guard = guard;
        for secs in [5u64, 15, 30, 60, 120, 300, 600] {
            crate::rt::time::sleep(std::time::Duration::from_secs(secs)).await;
            if !crate::db::session_is_live() || armed().is_none() || try_commit() {
                return;
            }
        }
    });
}

/// The account's stored config row with the choice's options, and its carried login, in every
/// field it never set. `None` when they add nothing. No account selected reads as an empty row.
fn merged_config(choice: &PreloginChoice) -> Result<Option<zeroize::Zeroizing<String>>, String> {
    let opts = choice.opts.as_object().filter(|o| !o.is_empty());
    if opts.is_none() && choice.auth.is_none() {
        return Ok(None);
    }
    let stored = match crate::db::get_current_account() {
        Ok(_) => crate::db::settings::get_sql_setting(super::prefs::config_key(choice.kind))?.map(zeroize::Zeroizing::new),
        Err(_) => None,
    };
    let mut row = stored
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let unset = |row: &serde_json::Value, k: &str| row.get(k).is_none_or(serde_json::Value::is_null);
    // A field the row leaves null (no SAM credentials) is one the account never set.
    let mut added = false;
    for (k, v) in opts.into_iter().flatten() {
        if unset(&row, k) && !v.is_null() {
            row[k] = v.clone();
            added = true;
        }
    }
    let login = choice.auth.as_ref().filter(|_| choice.kind == Kind::I2p && unset(&row, "sam_user") && unset(&row, "sam_password"));
    let json = (added || login.is_some() || stored.is_none()).then(|| super::prefs::secret_json(&row));
    super::prefs::scrub_json(&mut row);
    let (Some(json), Some(login)) = (json.clone(), login) else { return Ok(json) };
    // The login goes in typed, so the password is never a plain JSON string.
    let mut cfg = super::i2p_config::I2pConfig::parse(Some(json.as_str()));
    cfg.sam_user = Some(login.user.clone());
    cfg.sam_password = Some(login.password.clone());
    Ok(Some(cfg.to_json()))
}

/// Copy the marker's options into the kind's config, never over a field the account set.
fn fill_absent_config(choice: &PreloginChoice) -> Result<(), String> {
    let lock = super::prefs::config_write_lock();
    let _write = lock.lock().unwrap_or_else(|e| e.into_inner());
    let Some(json) = merged_config(choice)? else { return Ok(()) };
    let cfg = super::kinds::decode(choice.kind, Some(json.as_str()));
    super::prefs::persist_config(choice.kind, &json)?;
    super::prefs::set_config(choice.kind, cfg);
    Ok(())
}

/// The config `kind` starts with for the account on screen: while the welcome screen's choice
/// is carried into it, the same merge `commit` will save, so the instance started before the
/// commit is still the one the account keeps after it.
pub fn config_for(kind: Kind) -> super::KindConfig {
    let carried = armed().filter(|c| c.kind == kind).filter(|_| {
        crate::db::get_current_account().is_err()
            || matches!(super::prefs::stored_kind(), StoredKind::Absent | StoredKind::Kind(Kind::Clearnet))
    });
    match carried.map(|c| merged_config(&c)) {
        Some(Ok(Some(json))) => super::kinds::decode(kind, Some(json.as_str())),
        _ => super::prefs::config(kind),
    }
}

/// Bumped whenever an in-flight pre-login start stops being wanted, so a start that lands
/// afterwards shuts itself down.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn generation_tx() -> &'static tokio::sync::watch::Sender<u64> {
    static TX: std::sync::OnceLock<tokio::sync::watch::Sender<u64>> = std::sync::OnceLock::new();
    TX.get_or_init(|| tokio::sync::watch::channel(generation()).0)
}

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

pub fn cancel_start() {
    let next = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    generation_tx().send_replace(next);
}

/// Resolves once a pre-login start wanted at `since` no longer is, so the start can be dropped
/// mid-flight (Arti stops contacting the network with its client).
pub async fn superseded(since: u64) {
    let mut rx = generation_tx().subscribe();
    while generation() == since {
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// No account on screen: the remembered choice, armed. An unreadable marker stays Unknown.
pub fn apply_at_boot() {
    match marker() {
        Ok(Some(choice)) => {
            let kind = choice.kind;
            arm(choice);
            super::set_preference(Some(kind));
        }
        Ok(None) => {
            disarm();
            super::set_preference(Some(Kind::Clearnet));
        }
        Err(e) => {
            crate::log_warn!("[Transport] {e}");
            disarm();
            super::set_preference(None);
        }
    }
}

/// A pre-login instance that sees a second identity starts afresh, so an abandoned import and
/// the next account never share circuits or a destination.
pub fn note_identity(pk: &nostr_sdk::prelude::PublicKey) {
    let Some(active) = super::host::active() else { return };
    if !active.started_prelogin() {
        return;
    }
    if active.note_identity(*pk) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let t = active.transport().clone();
            // spawn-detached: refreshes the shared pre-login instance; reads no account state.
            super::spawn_on(async move { t.new_identity().await });
        }
    }
}
