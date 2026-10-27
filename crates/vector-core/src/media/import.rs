//! Files downloaded before the store, moved into it: checked against their hash, sealed under
//! fresh parameters, then taken out of the download folder. Resumable by construction: a file
//! is either still a download on disk or already the store's. An original goes only once its
//! kept copy has been read back whole and the database that points at it is on disk.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::db::media_store::Origin;

/// A download still shown from its own file in the download folder.
#[derive(Debug, Clone, Default)]
pub struct OnDisk {
    pub hash: [u8; 32],
    pub path: String,
    pub extension: String,
    /// Other spellings of the same file that rows also name.
    pub aliases: Vec<String>,
}

impl OnDisk {
    fn spellings(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.path.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

/// What became of one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moved {
    /// Kept in Vector; the original is owed, and goes once nothing else shows it.
    Moved,
    /// Kept in Vector; the original stays for another account that still shows it.
    Shared,
    /// Kept in Vector already; the file holds other bytes now and is left alone.
    Changed,
    /// Not the bytes its message sent: the message stops showing it, and the file is left alone.
    Mismatch,
}

/// What another account on this device still shows: by path, and by content where its path is
/// stale (an account that hasn't logged in since its download folder moved).
#[derive(Debug, Default)]
pub struct Elsewhere {
    paths: HashSet<PathBuf>,
    hashes: HashSet<[u8; 32]>,
}

impl Elsewhere {
    pub fn shows(&self, item: &OnDisk) -> bool {
        self.hashes.contains(&item.hash)
            || item.spellings().any(|path| {
                let path = Path::new(path);
                self.paths.contains(path) || std::fs::canonicalize(path).is_ok_and(|real| self.paths.contains(&real))
            })
    }

    /// A file another account shows from `path`: that file if it is there, else any of its bytes.
    fn note(&mut self, hash: Option<[u8; 32]>, path: &str) {
        if path.is_empty() {
            return;
        }
        match std::fs::canonicalize(path) {
            Ok(real) => {
                self.paths.insert(real);
            }
            Err(_) => {
                self.paths.insert(PathBuf::from(path));
                self.hashes.extend(hash);
            }
        }
    }
}

/// Rows that name a file in one of Vector's folders without marking it downloaded, as older
/// versions often left them. Each whose file holds exactly its bytes is marked downloaded, so
/// moving in and letting go treat it as any other download. Returns how many were marked.
pub fn reclaim_unmarked() -> Result<usize, String> {
    let roots: Vec<PathBuf> = crate::db::download_roots().iter().filter_map(|r| std::fs::canonicalize(r).ok()).collect();
    reclaim_in(&roots)
}

fn reclaim_in(roots: &[PathBuf]) -> Result<usize, String> {
    let mut held: std::collections::HashMap<PathBuf, Option<String>> = std::collections::HashMap::new();
    let mut marked = 0;
    for (id, hash, path, size) in crate::db::attachments::unmarked_downloads()? {
        if crate::simd::hex::hex_to_bytes_32_checked(&hash).is_none() {
            continue;
        }
        let Ok(real) = std::fs::canonicalize(&path) else { continue };
        let Ok(meta) = std::fs::metadata(&real) else { continue };
        if !roots.iter().any(|root| real.starts_with(root)) || !meta.is_file() {
            continue;
        }
        if size != 0 && meta.len() != size && meta.len() + 16 != size {
            continue;
        }
        let bytes = held.entry(real).or_insert_with_key(|real| crate::crypto::stream::hash_file(real).ok().map(|(hex, _)| hex));
        if bytes.as_deref().is_some_and(|hex| hex.eq_ignore_ascii_case(&hash)) && crate::db::attachments::mark_downloaded(id, &path)? {
            marked += 1;
        }
    }
    Ok(marked)
}

/// This account's downloads that still sit in one of Vector's folders, one per file.
pub fn on_disk() -> Result<Vec<OnDisk>, String> {
    let roots: Vec<PathBuf> = crate::db::download_roots().iter().filter_map(|r| std::fs::canonicalize(r).ok()).collect();
    if roots.is_empty() {
        return Ok(Vec::new());
    }
    let mut files: Vec<OnDisk> = Vec::new();
    let mut at: std::collections::HashMap<PathBuf, usize> = std::collections::HashMap::new();
    for (hash, path, extension) in crate::db::attachments::downloads_on_disk()? {
        let Some(hash) = crate::simd::hex::hex_to_bytes_32_checked(&hash) else { continue };
        let Ok(real) = std::fs::canonicalize(&path) else { continue };
        if !roots.iter().any(|root| real.starts_with(root)) || !real.is_file() {
            continue;
        }
        match at.get(&real) {
            Some(&i) if files[i].spellings().any(|seen| seen == path) => {}
            Some(&i) => files[i].aliases.push(path),
            None => {
                at.insert(real, files.len());
                files.push(OnDisk { hash, path, extension, aliases: Vec::new() });
            }
        }
    }
    Ok(files)
}

/// What every other account on this device shows, and every file the user saved. None when one
/// can't be read, so nothing is deleted on a guess.
pub fn elsewhere() -> Option<Elsewhere> {
    let me = crate::db::current_account_dir().ok()?;
    let root = crate::db::get_app_data_dir().ok()?;
    let others = crate::db::get_accounts().ok()?.into_iter().map(|name| root.join(name)).filter(|dir| *dir != me);
    with_saved(shown_in(others.map(|dir| dir.join("vector.db")))?)
}

/// `shown` and every file this account's user saved.
fn with_saved(mut shown: Elsewhere) -> Option<Elsewhere> {
    match crate::db::saved::all() {
        Ok(saved) => saved.iter().for_each(|path| shown.note(None, path)),
        Err(e) => {
            crate::log_warn!("[MediaImport] couldn't read the saved files, so originals stay: {}", e);
            return None;
        }
    }
    Some(shown)
}

fn shown_in(dbs: impl IntoIterator<Item = PathBuf>) -> Option<Elsewhere> {
    let mut shown = Elsewhere::default();
    for db in dbs {
        if let Err(e) = shown_by(&db, &mut shown) {
            crate::log_warn!("[MediaImport] couldn't read another account's downloads, so originals stay: {}", e);
            return None;
        }
    }
    Some(shown)
}

/// One account's downloads. An account not opened since attachments got their own table still
/// names them in the JSON of its events or messages.
fn shown_by(db: &Path, shown: &mut Elsewhere) -> Result<(), String> {
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
    let has = |table: &str| {
        conn.query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1", [table], |_| Ok(())).is_ok()
    };
    if has("saved_files") {
        let mut stmt = conn.prepare("SELECT path FROM saved_files").map_err(|e| e.to_string())?;
        for path in stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())? {
            shown.note(None, &path.map_err(|e| e.to_string())?);
        }
    }
    if has("miniapps_history") {
        let with_ref = conn
            .query_row("SELECT 1 FROM pragma_table_info('miniapps_history') WHERE name='attachment_ref'", [], |_| Ok(()))
            .is_ok();
        let sql = if with_ref { "SELECT src_url, attachment_ref FROM miniapps_history" } else { "SELECT src_url, NULL FROM miniapps_history" };
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?))).map_err(|e| e.to_string())?;
        for row in rows {
            let (src, attachment) = row.map_err(|e| e.to_string())?;
            for path in [src, attachment].into_iter().flatten().filter(|p| !p.starts_with("vmedia:") && !p.contains("://")) {
                shown.note(None, &path);
            }
        }
    }
    if has("attachments") {
        let mut stmt = conn.prepare("SELECT DISTINCT hash, path FROM attachments WHERE path!=''").map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| e.to_string())?;
        for row in rows {
            let (hash, path) = row.map_err(|e| e.to_string())?;
            shown.note(crate::simd::hex::hex_to_bytes_32_checked(&hash), &path);
        }
        return Ok(());
    }
    let mut json = |sql: &str| -> Result<(), String> {
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        for text in stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())? {
            let text = text.map_err(|e| e.to_string())?;
            let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("unreadable attachments: {e}"))?;
            json_shown(&value, shown);
        }
        Ok(())
    };
    if has("events") {
        json("SELECT tags FROM events WHERE tags LIKE '%attachments%'")?;
    }
    if has("messages") {
        json("SELECT attachments FROM messages WHERE attachments!='[]' AND attachments!=''")?;
    }
    Ok(())
}

/// Every attachment an attachment list shows from a file, through any JSON nested inside a
/// string: its path, and its content hash (`id`).
fn json_shown(value: &serde_json::Value, shown: &mut Elsewhere) {
    match value {
        serde_json::Value::Array(items) => items.iter().for_each(|v| json_shown(v, shown)),
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(path)) = map.get("path") {
                let hash = map.get("id").and_then(|id| id.as_str()).and_then(crate::simd::hex::hex_to_bytes_32_checked);
                shown.note(hash, path);
            }
            map.values().for_each(|v| json_shown(v, shown));
        }
        serde_json::Value::String(text) if text.starts_with('[') || text.starts_with('{') => {
            if let Ok(inner) = serde_json::from_str(text) {
                json_shown(&inner, shown);
            }
        }
        _ => {}
    }
}

/// Keep one file and point its messages at the kept copy. Returns what became of it and the
/// events whose rows changed; on `Moved` the original is owed, and goes once [`settle`] has made
/// those rows durable. Nothing here deletes a file the user had.
pub fn move_in(item: &OnDisk, elsewhere: Option<&Elsewhere>) -> Result<(Moved, Vec<String>), String> {
    use super::store::Check;
    let hex = crate::simd::hex::bytes_to_hex_32(&item.hash);
    let before = stamp(Path::new(&item.path)).map_err(|e| format!("stat {}: {e}", item.path))?;
    match super::store::read_back(&item.hash) {
        // A copy that can't be judged yet stays, and so does the original.
        Check::Unknown => return Err("the kept copy couldn't be checked".into()),
        Check::Good => {
            if crate::crypto::stream::hash_file(Path::new(&item.path))?.0 != hex {
                return Ok((Moved::Changed, at_each(item, &hex, crate::db::attachments::keep_at_store)?));
            }
        }
        // No copy, or one that is provably other bytes: this file takes its place.
        Check::Bad => match seal(item, &hex) {
            Ok(()) => {}
            Err(e) if e == crate::crypto::stream::CHANGED => {
                return Ok((Moved::Mismatch, at_each(item, &hex, crate::db::attachments::forget_path)?));
            }
            Err(e) => return Err(e),
        },
    }
    let shared = elsewhere.is_some_and(|shown| shown.shows(item));
    // Held across the repoint: a settle in between would see the rows still naming the file.
    let ledger = Ledger::lock()?;
    if !shared {
        if stamp(Path::new(&item.path)).ok() != Some(before) {
            return Err("the file changed while it was being moved in".into());
        }
        ledger.append(&Owed::of(item, before))?;
    }
    let paths: Vec<&str> = item.spellings().collect();
    let events = crate::db::attachments::move_to_store(&hex, &paths)?;
    Ok((if shared { Moved::Shared } else { Moved::Moved }, events))
}

fn at_each(item: &OnDisk, hex: &str, f: fn(&str, &str) -> Result<Vec<String>, String>) -> Result<Vec<String>, String> {
    let mut events = Vec::new();
    for path in item.spellings() {
        events.extend(f(hex, path)?);
    }
    events.sort();
    events.dedup();
    Ok(events)
}

/// An original promised to go, written down before its rows let go of it, so a crash between
/// the two can't strand a copy that nothing names.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Owed {
    path: String,
    hash: String,
    #[serde(default)]
    extension: String,
    len: u64,
    modified: u64,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    tries: u32,
}

impl Owed {
    fn of(item: &OnDisk, (len, modified): (u64, u64)) -> Self {
        Owed {
            path: item.path.clone(),
            hash: crate::simd::hex::bytes_to_hex_32(&item.hash),
            extension: item.extension.clone(),
            len,
            modified,
            aliases: item.aliases.clone(),
            tries: 0,
        }
    }

    fn item(&self) -> Option<OnDisk> {
        Some(OnDisk {
            hash: crate::simd::hex::hex_to_bytes_32_checked(&self.hash)?,
            path: self.path.clone(),
            extension: self.extension.clone(),
            aliases: self.aliases.clone(),
        })
    }
}

/// What a settle did with the originals owed.
#[derive(Debug, Default)]
pub struct Settled {
    /// Deleted, or already gone.
    pub gone: Vec<String>,
    /// Still on disk: changed since, named again, shown elsewhere, or not deletable yet.
    pub stayed: usize,
}

/// The account's owed originals, one JSON line each, locked for as long as this lives. The lock
/// is the OS's, on a file beside it: every session of the account, in any process, shares it.
struct Ledger {
    path: PathBuf,
    _lock: std::fs::File,
}

impl Ledger {
    fn lock() -> Result<Self, String> {
        let dir = crate::db::current_account_dir()?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("import-pending.lock"))
            .map_err(|e| format!("open import ledger lock: {e}"))?;
        lock_file(&lock).map_err(|e| format!("lock import ledger: {e}"))?;
        Ok(Ledger { path: dir.join("import-pending.jsonl"), _lock: lock })
    }

    /// Every entry that reads, the latest for each file. A line that doesn't read (one cut short
    /// by a crash, or from another version) is passed over, never a block.
    fn read(&self) -> Result<Vec<Owed>, String> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("read import ledger: {e}")),
        };
        let mut seen = HashSet::new();
        let mut owed: Vec<Owed> = bytes
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .filter_map(|line| {
                serde_json::from_slice(line)
                    .inspect_err(|_| crate::log_warn!("[MediaImport] passed over an unreadable line of the import ledger"))
                    .ok()
            })
            .rev()
            .filter(|o: &Owed| seen.insert(o.path.clone()))
            .collect();
        owed.reverse();
        Ok(owed)
    }

    fn append(&self, owed: &Owed) -> Result<(), String> {
        use std::io::{Read, Seek, Write};
        let fresh = !self.path.exists();
        let mut line = serde_json::to_vec(owed).map_err(|e| e.to_string())?;
        line.push(b'\n');
        (|| {
            let mut file = std::fs::OpenOptions::new().create(true).read(true).append(true).open(&self.path)?;
            // A line cut short by a crash ends here: start after it, not inside it.
            if file.seek(std::io::SeekFrom::End(0))? > 0 {
                let mut last = [0u8];
                file.seek(std::io::SeekFrom::End(-1))?;
                file.read_exact(&mut last)?;
                if last[0] != b'\n' {
                    line.insert(0, b'\n');
                }
            }
            file.write_all(&line)?;
            file.sync_all()?;
            if fresh {
                sync_dir(&self.path)?;
            }
            Ok::<_, std::io::Error>(())
        })()
        .map_err(|e| format!("write import ledger: {e}"))
    }

    fn rewrite(&self, owed: &[Owed]) -> Result<(), String> {
        use std::io::Write;
        if owed.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("clear import ledger: {e}")),
                _ => Ok(()),
            };
        }
        let mut bytes = Vec::new();
        for o in owed {
            bytes.extend(serde_json::to_vec(o).map_err(|e| e.to_string())?);
            bytes.push(b'\n');
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        (|| {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&tmp, &self.path)?;
            // The rename itself, durable on every platform.
            std::fs::OpenOptions::new().write(true).open(&self.path)?.sync_all()?;
            sync_dir(&self.path)
        })()
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("write import ledger: {e}")
        })
    }
}

/// An exclusive lock on `file`, held until it closes. Android's std has none: flock is there.
fn lock_file(file: &std::fs::File) -> std::io::Result<()> {
    match file.lock() {
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => flock(file),
        locked => locked,
    }
}

#[cfg(unix)]
fn flock(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    loop {
        // SAFETY: the descriptor is open for as long as `file` is borrowed.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

#[cfg(not(unix))]
fn flock(_: &std::fs::File) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// Sync the folder holding `path`, where a platform allows opening one.
fn sync_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// A file's size and modification time, to tell it from whatever later takes its name.
fn stamp(path: &Path) -> std::io::Result<(u64, u64)> {
    let meta = std::fs::metadata(path)?;
    let modified = meta.modified()?.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    Ok((meta.len(), modified))
}

/// Delete every original owed, against what the other accounts on this device show now. At boot
/// and before a move.
pub fn settle() -> Result<Settled, String> {
    let ledger = Ledger::lock()?;
    let owed = ledger.read()?;
    if owed.is_empty() {
        return Ok(Settled::default());
    }
    let shown = elsewhere();
    settle_in(&ledger, owed, |_| true, shown.as_ref())
}

/// Delete the originals a move just owed, against what it read of the other accounts.
pub fn settle_batch(paths: &[String], shown: Option<&Elsewhere>) -> Result<Settled, String> {
    let ledger = Ledger::lock()?;
    let owed = ledger.read()?;
    let wanted: HashSet<&str> = paths.iter().map(String::as_str).collect();
    settle_in(&ledger, owed, |o| wanted.contains(o.path.as_str()), shown)
}

/// An original goes only once the rows that let go of it are on disk, while it is the file that
/// was moved, nothing here or in another account names it, and its kept copy reads back whole;
/// a copy found broken is kept anew from it first.
fn settle_in(ledger: &Ledger, owed: Vec<Owed>, pick: impl Fn(&Owed) -> bool, shown: Option<&Elsewhere>) -> Result<Settled, String> {
    if !owed.iter().any(&pick) {
        return Ok(Settled::default());
    }
    crate::db::sync_wal()?;
    let mut settled = Settled::default();
    let mut kept = Vec::new();
    for mut o in owed {
        if !pick(&o) {
            kept.push(o);
            continue;
        }
        match settle_one(&mut o, shown) {
            Ok(Fate::Gone) => settled.gone.push(o.path),
            Ok(Fate::Left) => settled.stayed += 1,
            Err(e) => {
                crate::log_warn!("[MediaImport] kept in Vector, but the original stays for now: {}", e);
                settled.stayed += 1;
                kept.push(o);
            }
        }
    }
    ledger.rewrite(&kept)?;
    Ok(settled)
}

/// What became of an owed original; an error keeps it owed for the next settle.
enum Fate {
    Gone,
    /// Left in place for good: changed, named again, or another account's now.
    Left,
}

fn settle_one(o: &mut Owed, shown: Option<&Elsewhere>) -> Result<Fate, String> {
    use super::store::Check;
    let Some(item) = o.item() else { return Ok(Fate::Left) };
    let path = Path::new(&o.path);
    match stamp(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Fate::Gone),
        Err(e) => return Err(format!("stat: {e}")),
        Ok(seen) if seen != (o.len, o.modified) => return Ok(Fate::Left),
        Ok(_) => {}
    }
    for spelling in item.spellings() {
        if crate::db::attachments::names_path(spelling)? {
            return Ok(Fate::Left);
        }
    }
    match shown {
        None => return Err("another account on this device couldn't be read".into()),
        Some(shown) if shown.shows(&item) => return Ok(Fate::Left),
        Some(_) => {}
    }
    match super::store::read_back(&item.hash) {
        Check::Good => {}
        Check::Unknown => return Err("the kept copy couldn't be checked".into()),
        Check::Bad => {
            match seal(&item, &o.hash) {
                Ok(()) => {}
                Err(e) if e == crate::crypto::stream::CHANGED => return Ok(Fate::Left),
                Err(e) => return Err(e),
            }
            // The new copy's key must be on disk before the only other copy goes.
            crate::db::sync_wal()?;
        }
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(Fate::Gone),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Fate::Gone),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && o.tries >= 2 => {
            crate::log_warn!("[MediaImport] the original can't be deleted, so it stays: {}", e);
            Ok(Fate::Left)
        }
        Err(e) => {
            o.tries += 1;
            Err(format!("delete: {e}"))
        }
    }
}

/// Let a download go when media isn't kept: the file goes, then the rows that showed it, so a
/// crash between leaves rows the boot check finds missing. A file another account shows, or
/// one whose other accounts can't be read, stays where it is. Returns the events touched.
pub fn release(item: &OnDisk, elsewhere: Option<&Elsewhere>) -> Result<Vec<String>, String> {
    if elsewhere.is_some_and(|shown| !shown.shows(item)) {
        match std::fs::remove_file(&item.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("delete: {e}")),
            _ => {}
        }
    }
    let paths: Vec<&str> = item.spellings().collect();
    crate::db::attachments::let_go(&paths)
}

/// Loaded messages stop showing a released file.
pub fn reflect_released(state: &mut crate::state::ChatState, item: &OnDisk, events: &[String]) -> Vec<(String, crate::types::Message)> {
    let mut touched = Vec::new();
    for (chat_idx, chat) in state.chats.iter_mut().enumerate() {
        for id in events {
            let Some(msg) = chat.messages.find_by_hex_id_mut(id) else { continue };
            let mut changed = false;
            for att in msg.attachments.iter_mut().filter(|a| item.spellings().any(|p| *a.path() == *p)) {
                att.set_downloaded(false);
                att.set_path(String::new());
                changed = true;
            }
            if changed {
                touched.push((chat_idx, id.clone()));
            }
        }
    }
    touched
        .into_iter()
        .filter_map(|(chat_idx, id)| {
            let chat = &state.chats[chat_idx];
            chat.messages.find_by_hex_id(&id).map(|m| (chat.id().clone(), m.to_message(&state.interner)))
        })
        .collect()
}

/// Loaded messages follow a moved file as its rows did: those of its hash show the store's copy,
/// any claiming other bytes there lose the download; for bytes that weren't the message's, only
/// rows of its hash let go. Returns each changed message as (chat id, message).
pub fn reflect(state: &mut crate::state::ChatState, item: &OnDisk, outcome: Moved, events: &[String]) -> Vec<(String, crate::types::Message)> {
    let mut touched = Vec::new();
    for (chat_idx, chat) in state.chats.iter_mut().enumerate() {
        for id in events {
            let Some(msg) = chat.messages.find_by_hex_id_mut(id) else { continue };
            let mut changed = false;
            for att in msg.attachments.iter_mut().filter(|a| item.spellings().any(|p| *a.path() == *p)) {
                match (outcome, att.id() == &item.hash) {
                    (Moved::Mismatch | Moved::Changed, false) => continue,
                    (Moved::Moved | Moved::Shared | Moved::Changed, true) => att.set_path(String::new()),
                    _ => {
                        att.set_downloaded(false);
                        att.set_path(String::new());
                    }
                }
                changed = true;
            }
            if changed {
                touched.push((chat_idx, id.clone()));
            }
        }
    }
    touched
        .into_iter()
        .filter_map(|(chat_idx, id)| {
            let chat = &state.chats[chat_idx];
            chat.messages.find_by_hex_id(&id).map(|m| (chat.id().clone(), m.to_message(&state.interner)))
        })
        .collect()
}

/// Seal the file into the store in place of any copy there, refusing bytes that aren't `hex`.
fn seal(item: &OnDisk, hex: &str) -> Result<(), String> {
    let dir = super::store::dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't keep the file: {e}"))?;
    let params = crate::crypto::generate_encryption_params();
    let tmp = dir.join(format!(".import-{}.tmp", super::store::fresh_name()));
    let (blob_sha256, blob_len) =
        crate::crypto::stream::encrypt_file_matching(Path::new(&item.path), &tmp, &params.key, &params.nonce, hex, &mut |_| true)?;
    super::store::keep_verified(&tmp, super::store::Keep {
        hash: item.hash,
        key: params.key.clone(),
        nonce: params.nonce.clone(),
        blob_sha256,
        pt_len: blob_len.saturating_sub(16),
        mime: crate::crypto::mime_from_extension(&item.extension).to_string(),
        origin: Origin::Imported,
    })
    .inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(97000);

    fn make_test_npub(n: u32) -> String {
        const BECH32: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let mut payload = vec![b'q'; 58];
        let mut x = n as u64;
        let mut i = 58;
        while x > 0 && i > 0 {
            i -= 1;
            payload[i] = BECH32[(x as usize) % 32];
            x /= 32;
        }
        format!("npub1{}", String::from_utf8(payload).unwrap())
    }

    fn init_test_db() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        let account = make_test_npub(TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        guard
    }

    /// A download of `bytes` at `path`, claimed as `hash`, on a message of its own.
    fn download(event: &str, hash: &str, path: &Path) {
        let conn = crate::db::get_write_connection_guard_static().unwrap();
        conn.execute("INSERT OR IGNORE INTO chats (chat_identifier, chat_type, participants, created_at) VALUES ('npub1chat', 0, '[]', 1)", []).unwrap();
        let chat: i64 = conn.query_row("SELECT id FROM chats WHERE chat_identifier='npub1chat'", [], |r| r.get(0)).unwrap();
        conn.execute("INSERT INTO events (id, kind, chat_id, content, created_at, received_at) VALUES (?1, 14, ?2, '', 1, 1)", rusqlite::params![event, chat]).unwrap();
        conn.execute(
            "INSERT INTO attachments (event_id, att_index, hash, extension, path, downloaded) VALUES (?1, 0, ?2, 'png', ?3, 1)",
            rusqlite::params![event, hash, path.to_string_lossy()],
        )
        .unwrap();
    }

    fn row(event: &str) -> (String, bool) {
        let conn = crate::db::get_db_connection_guard_static().unwrap();
        conn.query_row("SELECT path, downloaded FROM attachments WHERE event_id=?1", [event], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? == 1))).unwrap()
    }

    fn item(bytes: &[u8], path: &Path) -> OnDisk {
        OnDisk {
            hash: crate::simd::hex::hex_to_bytes_32(&crate::crypto::sha256_hex(bytes)),
            path: path.to_string_lossy().into_owned(),
            extension: "png".to_string(),
            aliases: Vec::new(),
        }
    }

    fn nobody_else() -> Elsewhere {
        Elsewhere::default()
    }

    fn owed() -> Vec<Owed> {
        Ledger::lock().unwrap().read().unwrap()
    }

    fn settle_all(shown: Option<&Elsewhere>) -> Settled {
        let paths: Vec<String> = owed().into_iter().map(|o| o.path).collect();
        settle_batch(&paths, shown).unwrap()
    }

    #[test]
    fn a_download_moves_in_and_its_original_goes_only_when_removed() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("photo.png");
        std::fs::write(&file, b"a photo from before the store").unwrap();
        let moving = item(b"a photo from before the store", &file);
        let hex = crate::simd::hex::bytes_to_hex_32(&moving.hash);
        download("ev-ours", &hex, &file);
        download("ev-other", &"ab".repeat(32), &file);

        let (outcome, mut events) = move_in(&moving, Some(&nobody_else())).unwrap();
        events.sort();
        assert_eq!(outcome, Moved::Moved);
        assert_eq!(events, ["ev-other", "ev-ours"]);
        assert!(file.exists(), "nothing is deleted until the rows are durable");
        assert_eq!(row("ev-ours"), (String::new(), true));
        assert_eq!(row("ev-other"), (String::new(), false));
        assert_eq!(crate::media::store::read_back(&moving.hash), crate::media::store::Check::Good);
        let src = crate::media::open(&moving.hash).unwrap();
        assert_eq!(crate::media::read_all(&*src).unwrap(), b"a photo from before the store");
        assert_eq!(src.mime(), "image/png");
        // Nothing in the store is the plaintext.
        for entry in std::fs::read_dir(crate::media::store::dir().unwrap()).unwrap().flatten() {
            assert_ne!(std::fs::read(entry.path()).unwrap(), b"a photo from before the store");
        }
        let settled = settle_all(Some(&nobody_else()));
        assert_eq!((settled.gone, settled.stayed), (vec![moving.path.clone()], 0));
        assert!(!file.exists());
        assert!(owed().is_empty());
    }

    #[test]
    fn an_original_owed_goes_only_while_it_is_the_file_that_moved() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let (kept, replaced, named) = (dir.path().join("k.png"), dir.path().join("r.png"), dir.path().join("n.png"));
        for (file, bytes) in [(&kept, &b"owed, then a crash"[..]), (&replaced, b"owed, then replaced"), (&named, b"owed, then named")] {
            std::fs::write(file, bytes).unwrap();
            download(&format!("ev-{}", file.file_stem().unwrap().to_string_lossy()), &crate::crypto::sha256_hex(bytes), file);
            assert_eq!(move_in(&item(bytes, file), Some(&nobody_else())).unwrap().0, Moved::Moved);
        }
        assert_eq!(owed().len(), 3);

        // Another file takes one name; a message names another again.
        std::fs::remove_file(&replaced).unwrap();
        std::fs::write(&replaced, b"a different file, same name").unwrap();
        download("ev-again", &"cd".repeat(32), &named);

        let settled = settle_all(Some(&nobody_else()));
        assert_eq!((settled.gone, settled.stayed), (vec![kept.to_string_lossy().into_owned()], 2));
        assert!(!kept.exists() && replaced.exists() && named.exists());
        assert!(owed().is_empty());
    }

    #[test]
    fn an_original_another_account_comes_to_show_stays() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("adopted.png");
        std::fs::write(&file, b"another account found this later").unwrap();
        let moving = item(b"another account found this later", &file);
        download("ev-adopted", &crate::simd::hex::bytes_to_hex_32(&moving.hash), &file);
        assert_eq!(move_in(&moving, Some(&nobody_else())).unwrap().0, Moved::Moved);

        // Unreadable other accounts keep it owed; one that shows it now keeps the file for good.
        assert_eq!(settle_all(None).stayed, 1);
        assert_eq!(owed().len(), 1);
        let adopted = Elsewhere { paths: HashSet::from([std::fs::canonicalize(&file).unwrap()]), ..Default::default() };
        assert_eq!(settle_all(Some(&adopted)).stayed, 1);
        assert!(file.exists());
        assert!(owed().is_empty());
    }

    #[test]
    fn a_saved_file_outlives_the_download_it_also_was() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let (saved, plain) = (dir.path().join("saved.png"), dir.path().join("plain.png"));
        std::fs::write(&saved, b"kept, then saved").unwrap();
        std::fs::write(&plain, b"kept, never saved").unwrap();
        download("ev-saved", &crate::crypto::sha256_hex(b"kept, then saved"), &saved);
        download("ev-plain", &crate::crypto::sha256_hex(b"kept, never saved"), &plain);
        crate::db::saved::mark(&saved.to_string_lossy()).unwrap();
        let shown = with_saved(nobody_else()).unwrap();

        // Let go: Vector stops showing both, and deletes only the one nobody saved.
        release(&item(b"kept, then saved", &saved), Some(&shown)).unwrap();
        release(&item(b"kept, never saved", &plain), Some(&shown)).unwrap();
        assert!(saved.exists() && !plain.exists());
        assert_eq!(row("ev-saved"), (String::new(), false));

        // Moved in: the store keeps a copy and the saved file stays where it is, owed to nobody.
        download("ev-again", &crate::crypto::sha256_hex(b"kept, then saved"), &saved);
        assert_eq!(move_in(&item(b"kept, then saved", &saved), Some(&shown)).unwrap().0, Moved::Shared);
        assert!(owed().is_empty() && saved.exists());

        // A new file at that name is not the one that was saved.
        crate::db::saved::unmark(&saved.to_string_lossy()).unwrap();
        assert!(!with_saved(nobody_else()).unwrap().shows(&item(b"kept, then saved", &saved)));
    }

    #[test]
    fn a_kept_file_moves_out_to_a_plain_copy() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("first.png");
        std::fs::write(&file, b"kept, then made plain").unwrap();
        let kept = item(b"kept, then made plain", &file);
        let hex = crate::simd::hex::bytes_to_hex_32(&kept.hash);
        download("ev-out", &hex, &file);
        move_in(&kept, Some(&nobody_else())).unwrap();
        settle_all(Some(&nobody_else()));
        assert_eq!(crate::db::attachments::kept_names().unwrap(), vec![(kept.hash, String::new(), "png".to_string())]);

        let plain = dir.path().join("plain.png");
        std::fs::write(&plain, b"kept, then made plain").unwrap();
        assert_eq!(crate::db::attachments::out_of_store(&hex, &plain.to_string_lossy()).unwrap(), ["ev-out"]);
        crate::media::store::forget(&kept.hash);
        assert_eq!(row("ev-out"), (plain.to_string_lossy().into_owned(), true), "the store letting go leaves the plain row");
        assert!(crate::db::attachments::kept_names().unwrap().is_empty());
        // Only a file in one of Vector's own folders stands in for the store.
        assert!(crate::media::open(&kept.hash).is_none());
    }

    #[test]
    fn a_new_account_starts_memory_only_and_one_from_before_keeps_media() {
        let _guard = init_test_db();
        assert!(!crate::media::keep_media());
        crate::db::set_sql_setting("keep_media".into(), "false".into()).unwrap();
        let account = crate::db::get_current_account().unwrap();
        crate::db::close_database();
        crate::db::init_database(&account).unwrap();
        assert!(!crate::media::keep_media(), "a choice already made stands");

        crate::db::get_write_connection_guard_static().unwrap().execute("DELETE FROM settings WHERE key='keep_media'", []).unwrap();
        crate::db::close_database();
        crate::db::init_database(&account).unwrap();
        assert!(crate::media::keep_media(), "an account from before the choice keeps media");
    }

    #[test]
    fn a_released_download_goes_unless_another_account_shows_it() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let (mine, shared) = (dir.path().join("mine.png"), dir.path().join("shared.png"));
        std::fs::write(&mine, b"only this account showed it").unwrap();
        std::fs::write(&shared, b"another account shows this").unwrap();
        let (a, b) = (item(b"only this account showed it", &mine), item(b"another account shows this", &shared));
        download("ev-mine", &crate::simd::hex::bytes_to_hex_32(&a.hash), &mine);
        download("ev-shared", &crate::simd::hex::bytes_to_hex_32(&b.hash), &shared);
        let shown = Elsewhere { paths: HashSet::from([std::fs::canonicalize(&shared).unwrap()]), ..Default::default() };

        assert_eq!(release(&a, Some(&shown)).unwrap(), vec!["ev-mine".to_string()]);
        assert_eq!(release(&b, Some(&shown)).unwrap(), vec!["ev-shared".to_string()]);
        assert!(!mine.exists() && shared.exists());
        assert_eq!(row("ev-mine"), (String::new(), false));
        assert_eq!(row("ev-shared"), (String::new(), false));

        // Other accounts that can't be read: nothing is deleted on a guess.
        let third = dir.path().join("third.png");
        std::fs::write(&third, b"no one can say").unwrap();
        let c = item(b"no one can say", &third);
        download("ev-third", &crate::simd::hex::bytes_to_hex_32(&c.hash), &third);
        release(&c, None).unwrap();
        assert!(third.exists());
        assert_eq!(row("ev-third"), (String::new(), false));
    }

    #[cfg(unix)]
    #[test]
    fn the_flock_fallback_excludes_another_handle() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        let open = || std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.path().join("l")).unwrap();
        let (held, other) = (open(), open());
        flock(&held).unwrap();
        // SAFETY: `other` is open for the call.
        assert_ne!(unsafe { libc::flock(other.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
        drop(held);
        flock(&other).unwrap();
    }

    #[test]
    fn a_ledger_line_that_wont_read_is_passed_over() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("after.png");
        std::fs::write(&file, b"owed after a torn line").unwrap();
        let ledger = crate::db::current_account_dir().unwrap().join("import-pending.jsonl");
        std::fs::write(&ledger, b"{\"path\":\"/torn\",\"ha").unwrap();
        let moving = item(b"owed after a torn line", &file);
        download("ev-after", &crate::simd::hex::bytes_to_hex_32(&moving.hash), &file);
        assert_eq!(move_in(&moving, Some(&nobody_else())).unwrap().0, Moved::Moved);

        let mut more = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
        std::io::Write::write_all(&mut more, b"not json at all\n").unwrap();
        let second = dir.path().join("second.png");
        std::fs::write(&second, b"owed after garbage").unwrap();
        let next = item(b"owed after garbage", &second);
        download("ev-second", &crate::simd::hex::bytes_to_hex_32(&next.hash), &second);
        assert_eq!(move_in(&next, Some(&nobody_else())).unwrap().0, Moved::Moved);
        assert_eq!(owed().iter().map(|o| o.path.clone()).collect::<Vec<_>>(), vec![moving.path.clone(), next.path.clone()]);
        assert_eq!(settle_all(Some(&nobody_else())).gone, vec![moving.path.clone(), next.path.clone()]);
        assert!(!file.exists() && !second.exists());
    }

    #[test]
    fn a_file_named_two_ways_moves_and_goes_as_one() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("twice.png");
        std::fs::write(&file, b"one file, two spellings").unwrap();
        let other = dir.path().join(".").join("twice.png");
        let hex = crate::crypto::sha256_hex(b"one file, two spellings");
        download("ev-plain", &hex, &file);
        download("ev-dotted", &hex, &other);
        let mut moving = item(b"one file, two spellings", &file);
        moving.aliases.push(other.to_string_lossy().into_owned());

        let (outcome, events) = move_in(&moving, Some(&nobody_else())).unwrap();
        assert_eq!((outcome, events), (Moved::Moved, vec!["ev-dotted".to_string(), "ev-plain".to_string()]));
        assert_eq!(row("ev-plain"), (String::new(), true));
        assert_eq!(row("ev-dotted"), (String::new(), true));
        assert_eq!(settle_all(Some(&nobody_else())).gone.len(), 1);
        assert!(!file.exists());
    }

    #[test]
    fn an_original_owed_rekeeps_a_broken_copy_before_it_goes() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("only.png");
        std::fs::write(&file, b"the only good copy").unwrap();
        let moving = item(b"the only good copy", &file);
        download("ev-only", &crate::simd::hex::bytes_to_hex_32(&moving.hash), &file);
        assert_eq!(move_in(&moving, Some(&nobody_else())).unwrap().0, Moved::Moved);
        let blob = crate::media::store::dir().unwrap().join(crate::db::media_store::get(&moving.hash).unwrap().unwrap().file);
        std::fs::remove_file(&blob).unwrap();

        assert_eq!(settle_all(Some(&nobody_else())).gone.len(), 1);
        assert!(!file.exists());
        assert_eq!(crate::media::store::read_back(&moving.hash), crate::media::store::Check::Good);
        let src = crate::media::open(&moving.hash).unwrap();
        assert_eq!(crate::media::read_all(&*src).unwrap(), b"the only good copy");
    }

    #[test]
    fn a_file_another_account_shows_stays_where_it_is() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("shared.png");
        std::fs::write(&file, b"two accounts saw this").unwrap();
        let moving = item(b"two accounts saw this", &file);
        download("ev-shared", &crate::simd::hex::bytes_to_hex_32(&moving.hash), &file);

        let by_path = Elsewhere { paths: HashSet::from([PathBuf::from(&moving.path)]), ..Default::default() };
        assert_eq!(move_in(&moving, Some(&by_path)).unwrap().0, Moved::Shared);
        assert!(crate::media::store::holds(&moving.hash));
        assert_eq!(row("ev-shared"), (String::new(), true));

        // The other account names the file under a path that has since moved: its bytes still say.
        let again = dir.path().join("moved.png");
        std::fs::write(&again, b"named under an old folder").unwrap();
        let stale = item(b"named under an old folder", &again);
        download("ev-stale", &crate::simd::hex::bytes_to_hex_32(&stale.hash), &again);
        let by_hash = Elsewhere { paths: HashSet::from([PathBuf::from("/old/folder/moved.png")]), hashes: HashSet::from([stale.hash]) };
        assert_eq!(move_in(&stale, Some(&by_hash)).unwrap().0, Moved::Shared);

        assert!(owed().is_empty(), "another account's files are its own to move");

        // Unknown elsewhere: the original is owed, and nothing is deleted on a guess.
        let unknown_file = dir.path().join("unknown.png");
        std::fs::write(&unknown_file, b"no one can say who else saw this").unwrap();
        let unknown = item(b"no one can say who else saw this", &unknown_file);
        download("ev-unknown", &crate::simd::hex::bytes_to_hex_32(&unknown.hash), &unknown_file);
        assert_eq!(move_in(&unknown, None).unwrap().0, Moved::Moved);
        assert_eq!(settle_all(None).stayed, 1);
        assert!(file.exists() && again.exists() && unknown_file.exists());
        assert_eq!(settle_all(Some(&nobody_else())).gone, vec![unknown.path.clone()]);
    }

    #[test]
    fn bytes_that_are_not_the_message_are_left_alone() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("edited.png");
        std::fs::write(&file, b"edited since it arrived").unwrap();
        let claimed = item(b"what the message sent", &file);
        download("ev-edited", &crate::simd::hex::bytes_to_hex_32(&claimed.hash), &file);

        assert_eq!(move_in(&claimed, Some(&nobody_else())).unwrap().0, Moved::Mismatch);
        assert_eq!(std::fs::read(&file).unwrap(), b"edited since it arrived");
        assert!(!crate::media::store::holds(&claimed.hash));
        assert_eq!(row("ev-edited"), (String::new(), false));
        assert_eq!(std::fs::read_dir(crate::media::store::dir().unwrap()).map(|d| d.count()).unwrap_or(0), 0);
    }

    /// Older versions often left a download's row naming its file without marking it downloaded.
    #[test]
    fn an_unmarked_row_whose_file_holds_its_bytes_moves_in_like_a_download() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file = root.join("old.png");
        std::fs::write(&file, b"downloaded long ago").unwrap();
        let hex = crate::crypto::sha256_hex(b"downloaded long ago");
        download("ev-old", &hex, &file);
        let other = root.join("other.png");
        std::fs::write(&other, b"someone else's bytes").unwrap();
        download("ev-wrong", &"cd".repeat(32), &other);
        let outside = tempfile::tempdir().unwrap();
        let away = outside.path().join("away.png");
        std::fs::write(&away, b"downloaded long ago").unwrap();
        download("ev-away", &hex, &away);
        crate::db::get_write_connection_guard_static().unwrap().execute("UPDATE attachments SET downloaded=0", []).unwrap();

        assert_eq!(reclaim_in(std::slice::from_ref(&root)).unwrap(), 1);
        assert_eq!(row("ev-old"), (file.to_string_lossy().into_owned(), true));
        assert!(!row("ev-wrong").1, "a file of other bytes stays unmarked");
        assert!(!row("ev-away").1, "only Vector's own folders count");
        assert_eq!(reclaim_in(&[root]).unwrap(), 0);

        let (outcome, _) = move_in(&item(b"downloaded long ago", &file), Some(&nobody_else())).unwrap();
        assert_eq!(outcome, Moved::Moved);
        assert_eq!(row("ev-old"), (String::new(), true));
        assert_eq!(std::fs::read(&other).unwrap(), b"someone else's bytes");
    }

    #[test]
    fn a_kept_copy_is_trusted_only_once_it_reads_back() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (dir.path().join("a.png"), dir.path().join("a-1.png"));
        std::fs::write(&first, b"downloaded twice").unwrap();
        std::fs::write(&second, b"downloaded twice, then changed").unwrap();
        let hex = crate::crypto::sha256_hex(b"downloaded twice");
        download("ev-first", &hex, &first);
        download("ev-second", &hex, &second);
        assert_eq!(move_in(&item(b"downloaded twice", &first), Some(&nobody_else())).unwrap().0, Moved::Moved);

        // The store keeps these bytes, and this file holds others: it stays, its row shows the copy.
        assert_eq!(move_in(&item(b"downloaded twice", &second), Some(&nobody_else())).unwrap().0, Moved::Changed);
        assert!(second.exists());
        assert_eq!(row("ev-second"), (String::new(), true));

        // A row whose blob vanished is no copy: the next file of those bytes is kept anew.
        let hash = crate::simd::hex::hex_to_bytes_32(&hex);
        let entry = crate::db::media_store::get(&hash).unwrap().unwrap();
        std::fs::remove_file(crate::media::store::dir().unwrap().join(&entry.file)).unwrap();
        assert_eq!(crate::media::store::read_back(&hash), crate::media::store::Check::Bad);
        let third = dir.path().join("a-2.png");
        std::fs::write(&third, b"downloaded twice").unwrap();
        download("ev-third", &hex, &third);
        assert_eq!(move_in(&item(b"downloaded twice", &third), Some(&nobody_else())).unwrap().0, Moved::Moved);
        assert_eq!(crate::media::store::read_back(&hash), crate::media::store::Check::Good);
        // The rows that showed the lost copy show its replacement, and only the new blob remains.
        assert_eq!(row("ev-first"), (String::new(), true));
        assert_eq!(row("ev-second"), (String::new(), true));
        let blobs = std::fs::read_dir(crate::media::store::dir().unwrap()).unwrap().count();
        assert_eq!(blobs, 1);
    }

    #[test]
    fn a_copy_that_cant_be_judged_is_left_with_its_original() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (dir.path().join("b.png"), dir.path().join("b-1.png"));
        std::fs::write(&first, b"kept, then unreadable").unwrap();
        std::fs::write(&second, b"kept, then unreadable").unwrap();
        let hex = crate::crypto::sha256_hex(b"kept, then unreadable");
        download("ev-kept", &hex, &first);
        download("ev-later", &hex, &second);
        assert_eq!(move_in(&item(b"kept, then unreadable", &first), Some(&nobody_else())).unwrap().0, Moved::Moved);

        // A row whose key won't open is untouched, and so is the next original.
        let hash = crate::simd::hex::hex_to_bytes_32(&hex);
        let file = crate::db::media_store::get(&hash).unwrap().unwrap().file;
        crate::db::get_write_connection_guard_static().unwrap().execute("UPDATE media_store SET key = 'not a sealed key' WHERE hash = ?1", [&hex]).unwrap();
        assert_eq!(crate::media::store::read_back(&hash), crate::media::store::Check::Unknown);
        assert!(move_in(&item(b"kept, then unreadable", &second), Some(&nobody_else())).is_err());
        assert!(second.exists());
        assert_eq!(row("ev-later"), (second.to_string_lossy().into_owned(), true));
        assert!(crate::media::store::dir().unwrap().join(&file).exists());
        assert!(crate::db::media_store::has_row(&hash).unwrap());
    }

    #[test]
    fn a_blob_of_other_bytes_is_replaced_by_the_file() {
        let _guard = init_test_db();
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (dir.path().join("c.png"), dir.path().join("c-1.png"));
        std::fs::write(&first, b"a picture worth keeping").unwrap();
        std::fs::write(&second, b"a picture worth keeping").unwrap();
        let hex = crate::crypto::sha256_hex(b"a picture worth keeping");
        download("ev-one", &hex, &first);
        download("ev-two", &hex, &second);
        assert_eq!(move_in(&item(b"a picture worth keeping", &first), Some(&nobody_else())).unwrap().0, Moved::Moved);

        // Flip a byte of the sealed copy: the tag no longer matches, so the next original replaces it.
        let hash = crate::simd::hex::hex_to_bytes_32(&hex);
        let old = crate::media::store::dir().unwrap().join(crate::db::media_store::get(&hash).unwrap().unwrap().file);
        let mut sealed = std::fs::read(&old).unwrap();
        sealed[0] ^= 1;
        std::fs::write(&old, &sealed).unwrap();
        assert_eq!(crate::media::store::read_back(&hash), crate::media::store::Check::Bad);
        assert_eq!(move_in(&item(b"a picture worth keeping", &second), Some(&nobody_else())).unwrap().0, Moved::Moved);
        assert_eq!(crate::media::store::read_back(&hash), crate::media::store::Check::Good);
        assert!(!old.exists());
        assert_eq!(row("ev-one"), (String::new(), true));
    }

    #[test]
    fn loaded_messages_follow_the_move() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.png").to_string_lossy().into_owned();
        let moving = OnDisk { hash: [0x11; 32], path: path.clone(), extension: "png".into(), ..Default::default() };
        let att = |hash: [u8; 32], path: &str| crate::types::Attachment {
            id: crate::simd::hex::bytes_to_hex_32(&hash),
            extension: "png".into(),
            path: path.to_string(),
            downloaded: true,
            ..Default::default()
        };
        let message = |seed: u8, attachments| crate::types::Message { id: format!("{seed:02x}").repeat(32), attachments, at: 1, ..Default::default() };
        let build = || {
            let mut state = crate::state::ChatState::new();
            state.create_dm_chat("npub1peer");
            state.add_message_to_chat("npub1peer", &message(1, vec![att([0x11; 32], &path)]));
            state.add_message_to_chat("npub1peer", &message(2, vec![att([0x22; 32], &path)]));
            state.add_message_to_chat("npub1peer", &message(3, vec![att([0x11; 32], "/elsewhere.png")]));
            state
        };
        let events: Vec<String> = [1u8, 2, 3].iter().map(|s| format!("{s:02x}").repeat(32)).collect();
        let shown = |out: &[(String, crate::types::Message)], seed: u8| {
            out.iter().find(|(_, m)| m.id == format!("{seed:02x}").repeat(32)).map(|(_, m)| (m.attachments[0].path.clone(), m.attachments[0].downloaded))
        };

        let mut state = build();
        let out = reflect(&mut state, &moving, Moved::Moved, &events);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|(chat, _)| chat == "npub1peer"));
        assert_eq!(shown(&out, 1), Some((String::new(), true)));
        assert_eq!(shown(&out, 2), Some((String::new(), false)));
        assert_eq!(shown(&out, 3), None);

        let mut state = build();
        let out = reflect(&mut state, &moving, Moved::Mismatch, &events);
        assert_eq!(out.len(), 1);
        assert_eq!(shown(&out, 1), Some((String::new(), false)));
    }

    #[test]
    fn another_accounts_downloads_are_read_and_an_unreadable_one_stops_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("vector.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE attachments (hash TEXT NOT NULL, path TEXT NOT NULL DEFAULT '', downloaded INTEGER NOT NULL DEFAULT 0);").unwrap();
        // A file still there is shown by its path alone; a stale path by its bytes too; a kept one not at all.
        let there = dir.path().join("there.png");
        std::fs::write(&there, b"still in its folder").unwrap();
        conn.execute(
            "INSERT INTO attachments VALUES (?1, '/d/a.png', 1), (?2, '', 1), (?3, '', 0), (?4, ?5, 1)",
            ["11".repeat(32), "22".repeat(32), "33".repeat(32), "55".repeat(32), there.to_string_lossy().into_owned()],
        )
        .unwrap();
        drop(conn);
        let shown = shown_in([db.clone()]).unwrap();
        assert_eq!(shown.paths, HashSet::from([PathBuf::from("/d/a.png"), std::fs::canonicalize(&there).unwrap()]));
        assert_eq!(shown.hashes, HashSet::from([[0x11; 32]]));
        assert!(shown.shows(&OnDisk { hash: [0x55; 32], path: there.to_string_lossy().into_owned(), extension: "png".into(), ..Default::default() }));
        assert!(!shown.shows(&OnDisk { hash: [0x55; 32], path: "/d/elsewhere.png".into(), extension: "png".into(), ..Default::default() }));

        // Older schemas name downloads in their events' tags and their messages' attachment lists.
        let old = dir.path().join("old.db");
        let tag = serde_json::json!([["attachments", serde_json::json!([{ "id": "44".repeat(32), "path": "/d/b.png" }]).to_string()]]).to_string();
        let conn = rusqlite::Connection::open(&old).unwrap();
        conn.execute_batch("CREATE TABLE events (tags TEXT); CREATE TABLE messages (attachments TEXT NOT NULL DEFAULT '[]');").unwrap();
        conn.execute("INSERT INTO events VALUES (?1)", [&tag]).unwrap();
        conn.execute("INSERT INTO messages VALUES ('[{\"path\":\"/d/c.png\"},{\"path\":\"\"}]'), ('[]')", []).unwrap();
        drop(conn);
        let both = shown_in([db.clone(), old.clone()]).unwrap();
        for path in ["/d/a.png", "/d/b.png", "/d/c.png"] {
            assert!(both.paths.contains(Path::new(path)));
        }
        assert!(both.hashes.contains(&[0x44; 32]));

        let garbled = dir.path().join("garbled.db");
        rusqlite::Connection::open(&garbled).unwrap().execute_batch("CREATE TABLE messages (attachments TEXT); INSERT INTO messages VALUES ('sealed:abc');").unwrap();
        assert!(shown_in([db, garbled]).is_none());
        assert!(shown_in([dir.path().join("missing.db")]).is_none());
    }
}
