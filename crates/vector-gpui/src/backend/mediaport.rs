//! Kept files over time: old downloads moved into Vector's store, or kept files written out
//! plain for an account that keeps them unencrypted. Paced, a file at a time, and each file is
//! whole in one place or the other, so a launch that cuts it short picks up on the next.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vector_core::STATE;
use vector_core::media::import::{self, Elsewhere, Moved, OnDisk};

static MOVING: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);

/// Source bytes a second: slow enough to go unnoticed.
const RATE: f64 = 12.0 * 1024.0 * 1024.0;
/// Originals go a batch at a time, after the rows that replace them are synced.
const BATCH: usize = 32;

fn pace(len: u64) -> Duration {
    Duration::from_secs_f64(len as f64 / RATE).max(Duration::from_millis(150))
}

fn progress(done: usize, total: usize, plain: bool) {
    vector_core::traits::emit_event(
        "media_import_progress",
        &serde_json::json!({ "done": done, "total": total, "plain": plain }),
    );
}

/// After the boot paint: the store and the rows that point into it are checked, loaded messages
/// drop files that went missing while closed, stale outgoing work goes, and the port starts.
pub async fn after_boot() {
    vector_core::media::store::check();
    settle_owed(import::settle).await;
    vector_core::sending::sweep_outgoing();
    if let Ok(Ok(missing)) = vector_core::db::blocking_bound(missing_downloads).await {
        reconcile_missing(&missing).await;
    }
    start(Duration::from_secs(60));
}

/// Rows whose file is gone, cleared in the database; returns their event ids.
fn missing_downloads() -> Result<Vec<String>, String> {
    let mut affected: Vec<String> = Vec::new();
    for (event_id, hash, path) in vector_core::db::attachments::downloaded_attachment_paths()? {
        let present = if path.is_empty() {
            vector_core::simd::hex::hex_to_bytes_32_checked(&hash)
                .is_some_and(|h| vector_core::media::store::holds(&h))
        } else {
            Path::new(&path).exists()
        };
        if !present {
            let _ = vector_core::db::attachments::clear_attachment_download(&event_id, &hash);
            if !affected.contains(&event_id) {
                affected.push(event_id);
            }
        }
    }
    Ok(affected)
}

async fn reconcile_missing(affected: &[String]) {
    if affected.is_empty() {
        return;
    }
    let affected: std::collections::HashSet<&str> = affected.iter().map(String::as_str).collect();
    let mut updates = Vec::new();
    {
        let mut state = STATE.lock().await;
        for ci in 0..state.chats.len() {
            let mut ids = Vec::new();
            for m in state.chats[ci].messages.iter_mut() {
                if m.attachments.is_empty() || !affected.contains(m.id_hex().as_str()) {
                    continue;
                }
                let mut changed = false;
                for att in m.attachments.iter_mut().filter(|a| a.downloaded()) {
                    let present = if att.has_path() {
                        Path::new(&*att.path()).exists()
                    } else {
                        vector_core::media::store::holds(att.id())
                    };
                    if !present {
                        att.set_downloaded(false);
                        att.set_downloading(false);
                        att.set_path(String::new());
                        changed = true;
                    }
                }
                if changed {
                    ids.push(m.id_hex());
                }
            }
            let chat_id = state.chats[ci].id().to_string();
            for hex in ids {
                if let Some(m) = state.chats[ci].messages.find_by_hex_id(&hex) {
                    updates.push((chat_id.clone(), m.to_message(&state.interner)));
                }
            }
        }
    }
    emit_updates(updates).await;
}

async fn emit_updates(updates: Vec<(String, vector_core::Message)>) {
    for (chat, mut m) in updates {
        let old = m.id.clone();
        vector_core::traits::emit_message_update(&chat, &old, &mut m).await;
    }
}

/// This account's old downloads (rows older versions left unmarked included), and what other
/// accounts show.
pub fn find_old_downloads() -> (Result<Vec<OnDisk>, String>, Option<Elsewhere>) {
    if let Err(e) = import::reclaim_unmarked() {
        log::warn!("couldn't check downloads left unmarked: {e}");
    }
    (import::on_disk(), import::elsewhere())
}

/// Old downloads still in the download folder that would go with Keep Files: (files, bytes).
pub async fn old_downloads() -> (u64, u64) {
    let Ok((Ok(files), Some(shown))) = vector_core::db::blocking_bound(find_old_downloads).await else {
        return (0, 0);
    };
    let goes: Vec<_> = files.iter().filter(|f| !shown.shows(f)).collect();
    let bytes = goes.iter().filter_map(|f| std::fs::metadata(&f.path).ok()).map(|m| m.len()).sum();
    (goes.len() as u64, bytes)
}

/// Delete the originals a move owes; those that can't be judged yet stay owed.
async fn settle_owed<F>(settle: F)
where
    F: FnOnce() -> Result<import::Settled, String> + Send + 'static,
{
    if let Ok(Err(e)) = vector_core::db::blocking_bound(settle).await {
        log::warn!("originals stay owed for now: {e}");
    }
}

/// Move this account's files the way its settings say, after `delay`. One run at a time.
pub fn start(delay: Duration) {
    if MOVING.swap(true, Ordering::AcqRel) {
        return;
    }
    STOP.store(false, Ordering::Release);
    vector_core::db::spawn_bound(async move {
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                MOVING.store(false, Ordering::Release);
            }
        }
        let _done = Done;
        let stopped = || STOP.load(Ordering::Acquire) || !vector_core::db::writes_to_live_account();
        let at = tokio::time::Instant::now() + delay;
        while tokio::time::Instant::now() < at && !stopped() {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        if stopped() || !vector_core::media::keep_media() {
            return;
        }
        if vector_core::media::keep_unencrypted() {
            return move_out(&stopped).await;
        }
        move_in(&stopped).await;
    });
}

/// Stop between files. False when it is still in one after a while.
pub async fn stop() -> bool {
    STOP.store(true, Ordering::Release);
    for _ in 0..150 {
        if !MOVING.load(Ordering::Acquire) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// The port picks up again after a clear, if this account keeps files.
pub fn resume() {
    if vector_core::media::keep_media() {
        start(Duration::ZERO);
    }
}

async fn move_in(stopped: &impl Fn() -> bool) {
    settle_owed(import::settle).await;
    let Ok((Ok(files), elsewhere)) = vector_core::db::blocking_bound(find_old_downloads).await else {
        return;
    };
    let elsewhere = Arc::new(elsewhere);
    let mut owed: Vec<String> = Vec::new();
    for (i, item) in files.iter().enumerate() {
        if stopped() {
            break;
        }
        let len = std::fs::metadata(&item.path).map_or(0, |m| m.len());
        let (file, others) = (item.clone(), elsewhere.clone());
        match vector_core::db::blocking_bound(move || import::move_in(&file, others.as_ref().as_ref())).await {
            Ok(Ok((outcome, events))) => {
                if outcome == Moved::Moved {
                    owed.push(item.path.clone());
                }
                let updates = import::reflect(&mut *STATE.lock().await, item, outcome, &events);
                emit_updates(updates).await;
            }
            Ok(Err(e)) => log::warn!("couldn't move a file in: {e}"),
            Err(_) => break,
        }
        if owed.len() >= BATCH {
            let (batch, others) = (std::mem::take(&mut owed), elsewhere.clone());
            settle_owed(move || import::settle_batch(&batch, others.as_ref().as_ref())).await;
        }
        progress(i + 1, files.len(), false);
        tokio::time::sleep(pace(len)).await;
    }
    if !owed.is_empty() {
        settle_owed(move || import::settle_batch(&owed, elsewhere.as_ref().as_ref())).await;
    }
    progress(files.len(), files.len(), false);
}

async fn move_out(stopped: &impl Fn() -> bool) {
    let Ok(Ok(kept)) = vector_core::db::blocking_bound(vector_core::db::attachments::kept_names).await else {
        return;
    };
    for (i, (hash, name, _)) in kept.iter().enumerate() {
        if stopped() {
            break;
        }
        let len = vector_core::media::open(hash).map_or(0, |src| src.len());
        let (hash, name) = (*hash, name.clone());
        match vector_core::db::blocking_bound(move || unseal(&hash, &name)).await {
            Ok(Ok(path)) => reflect_unsealed(&hash, &path.to_string_lossy()).await,
            Ok(Err(e)) => log::warn!("couldn't keep a file unencrypted: {e}"),
            Err(_) => break,
        }
        progress(i + 1, kept.len(), true);
        tokio::time::sleep(pace(len)).await;
    }
    progress(kept.len(), kept.len(), true);
}

/// A kept file made plain in the public folder: its rows point at that copy, then the store lets
/// go of its own.
fn unseal(hash: &[u8; 32], name: &str) -> Result<PathBuf, String> {
    let (path, wrote) = place_plain(hash, name, &vector_core::db::get_public_dir())?;
    let spelled = path.to_string_lossy().into_owned();
    if wrote {
        vector_core::db::saved::unmark(&spelled)?;
    }
    vector_core::db::attachments::out_of_store(&vector_core::simd::hex::bytes_to_hex_string(hash), &spelled)?;
    vector_core::db::sync_wal()?;
    vector_core::media::store::forget(hash);
    Ok(path)
}

/// A plain copy of a kept file in `dir`; one there already holding these bytes is used as is.
/// Returns where it is, and whether this wrote it.
fn place_plain(hash: &[u8; 32], name: &str, dir: &Path) -> Result<(PathBuf, bool), String> {
    let src = vector_core::media::open(hash).ok_or("This file isn't on this device any more")?;
    let hex = vector_core::simd::hex::bytes_to_hex_string(hash);
    let holds = |path: &Path| vector_core::crypto::stream::hash_file(path).is_ok_and(|(h, _)| h == hex);
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't save the file: {e}"))?;
    let name = file_name(name, &hex, src.mime());
    let candidate = dir.join(&name);
    if holds(&candidate) {
        return Ok((candidate, false));
    }
    let dest = vector_core::crypto::resolve_unique_filename(dir, &name);
    copy_out(&*src, &dest)?;
    // The store's keystream checks no tag: the copy proves its bytes.
    if !holds(&dest) {
        let _ = std::fs::remove_file(&dest);
        return Err("Couldn't save the file: it didn't read back as sent".into());
    }
    Ok((dest, true))
}

/// The sender's name, else the hash and type.
fn file_name(name: &str, hex: &str, mime: &str) -> String {
    let name = vector_core::crypto::sanitize_filename(name);
    if !name.is_empty() {
        return name;
    }
    let ext = vector_core::crypto::extension_from_mime(mime);
    if ext.is_empty() { hex[..12].to_string() } else { format!("{}.{ext}", &hex[..12]) }
}

/// A chunk at a time, beside `dest` first, then into place: never whole in memory.
fn copy_out(src: &dyn vector_core::media::MediaSource, dest: &Path) -> Result<(), String> {
    use std::io::Write;
    let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dest.with_file_name(format!(".{name}.{:08x}.saving", rand::random::<u32>()));
    let result = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut out = std::io::BufWriter::with_capacity(1 << 20, options.open(&tmp)?);
        let mut buf = zeroize::Zeroizing::new(vec![0u8; 1 << 20]);
        let mut at = 0u64;
        while at < src.len() {
            let n = src.read_at(at, &mut buf)?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            out.write_all(&buf[..n])?;
            at += n as u64;
        }
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        std::fs::rename(&tmp, dest)
    })();
    result.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Couldn't save the file: {e}")
    })
}

/// Loaded messages show a kept file from its plain copy.
async fn reflect_unsealed(hash: &[u8; 32], path: &str) {
    let mut updates = Vec::new();
    {
        let mut state = STATE.lock().await;
        for ci in 0..state.chats.len() {
            let mut ids = Vec::new();
            for m in state.chats[ci].messages.iter_mut() {
                let mut changed = false;
                for att in m.attachments.iter_mut().filter(|a| a.id() == hash && a.downloaded() && a.path().is_empty()) {
                    att.set_path(path.to_string());
                    changed = true;
                }
                if changed {
                    ids.push(m.id_hex());
                }
            }
            let chat_id = state.chats[ci].id().to_string();
            for hex in ids {
                if let Some(m) = state.chats[ci].messages.find_by_hex_id(&hex) {
                    updates.push((chat_id.clone(), m.to_message(&state.interner)));
                }
            }
        }
    }
    emit_updates(updates).await;
}

/// Keep Unencrypted on or off: what is kept moves, paced, out to plain files or back in.
pub async fn set_keep_unencrypted(on: bool) -> Result<(), String> {
    vector_core::db::scoped(async move {
        if on && !vector_core::media::keep_media() {
            return Err("Turn on Keep Files on This Device first.".into());
        }
        if super::attachments::any_running() || !stop().await {
            return Err("Change this once your downloads finish.".into());
        }
        vector_core::db::set_sql_setting("keep_unencrypted".into(), on.to_string())?;
        start(Duration::ZERO);
        Ok(())
    })
    .await
}

/// Where kept-unencrypted files go, as the user's file manager names it.
pub fn public_folder() -> String {
    let dir = vector_core::db::get_public_dir();
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .and_then(|home| dir.strip_prefix(home).ok().map(Path::to_path_buf))
        .unwrap_or(dir)
        .to_string_lossy()
        .into_owned()
}
