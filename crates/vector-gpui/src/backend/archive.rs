//! A folder, or a selection of files and folders, zipped to send as one attachment.

use std::collections::HashSet;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The entries a preview lists: at most this many, the rest counted.
const LISTED: usize = 200;
const MAX_DEPTH: u32 = 128;

pub struct Entry {
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
}

pub struct Zipped {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub file_count: u32,
    pub dir_count: u32,
    pub entries: Vec<Entry>,
}

struct Item {
    source: PathBuf,
    name: String,
    is_dir: bool,
    size: u64,
}

/// Where a send's plaintext working copies go: the account's own folder, not a shared temp dir.
pub fn work_dir() -> PathBuf {
    vector_core::db::current_account_dir()
        .map(|d| d.join("outgoing").join("work").join("gpui"))
        .unwrap_or_else(|_| std::env::temp_dir().join("vector-gpui-work"))
}

/// A fresh folder under `work_dir` for one send's copy.
pub fn work_slot() -> std::io::Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = work_dir().join(nanos.to_string());
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// A working copy goes once it has sent or been dismissed.
pub fn forget(path: &Path) {
    let root = work_dir();
    if let Some(dir) = path.parent().filter(|d| d.starts_with(&root) && *d != root) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn walk(
    current: &Path,
    prefix: &str,
    items: &mut Vec<Item>,
    total: &mut u64,
    depth: u32,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("Directory nesting too deep (>128 levels)".into());
    }
    let read = std::fs::read_dir(current)
        .map_err(|e| format!("Failed to read directory {}: {e}", current.display()))?;
    for entry in read {
        let entry = entry.map_err(|e| format!("Failed to read entry: {e}"))?;
        let path = entry.path();
        // Symlinks are skipped: no traversal out of the folder, no cycles.
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        if meta.is_dir() {
            items.push(Item {
                source: path.clone(),
                name: name.clone(),
                is_dir: true,
                size: 0,
            });
            walk(&path, &format!("{name}/"), items, total, depth + 1)?;
        } else if meta.is_file() {
            *total += meta.len();
            items.push(Item {
                source: path,
                name,
                is_dir: false,
                size: meta.len(),
            });
        }
    }
    Ok(())
}

/// `name`, or `name-1`, `name-2`… when the root already holds it, case-blind.
fn unique_root_name(name: &str, is_dir: bool, taken: &mut HashSet<String>) -> String {
    if taken.insert(name.to_lowercase()) {
        return name.to_string();
    }
    let path = Path::new(name);
    let (stem, ext) = match (is_dir, path.file_stem(), path.extension()) {
        (false, Some(stem), Some(ext)) => (
            stem.to_string_lossy().into_owned(),
            Some(ext.to_string_lossy().into_owned()),
        ),
        _ => (name.to_string(), None),
    };
    (1u32..)
        .map(|n| match &ext {
            Some(ext) => format!("{stem}-{n}.{ext}"),
            None => format!("{stem}-{n}"),
        })
        .find(|c| taken.insert(c.to_lowercase()))
        .expect("an unused suffix exists")
}

/// The archive's name: a lone folder's own, else "Archive".
pub fn archive_name(paths: &[PathBuf]) -> String {
    match paths {
        [only] if only.is_dir() => only
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("folder")
            .to_string(),
        _ => "Archive".into(),
    }
}

fn collect(paths: &[PathBuf]) -> Result<(String, Vec<Item>, u64), String> {
    let (mut items, mut total) = (Vec::new(), 0u64);
    if let [only] = paths
        && only.is_dir()
    {
        walk(only, "", &mut items, &mut total, 0)?;
        if items.is_empty() {
            return Err("Directory is empty".into());
        }
        return Ok((archive_name(paths), items, total));
    }
    let mut taken = HashSet::new();
    for picked in paths {
        let Some(file_name) = picked.file_name() else {
            continue;
        };
        // The user chose these roots, so an alias is followed; the walk still skips symlinks inside.
        let Ok(source) = std::fs::canonicalize(picked) else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(&source) else {
            continue;
        };
        if !meta.is_dir() && !meta.is_file() {
            continue;
        }
        let name = unique_root_name(&file_name.to_string_lossy(), meta.is_dir(), &mut taken);
        if meta.is_dir() {
            items.push(Item {
                source: source.clone(),
                name: name.clone(),
                is_dir: true,
                size: 0,
            });
            walk(&source, &format!("{name}/"), &mut items, &mut total, 1)?;
        } else {
            total += meta.len();
            items.push(Item {
                source,
                name,
                is_dir: false,
                size: meta.len(),
            });
        }
    }
    if items.is_empty() {
        return Err("Nothing to zip".into());
    }
    Ok(("Archive".into(), items, total))
}

/// Zip `paths`, reporting whole percents into `percent`; a set `cancel` stops it and removes the partial file.
pub fn zip_paths(
    paths: &[PathBuf],
    cancel: &AtomicBool,
    percent: &Arc<AtomicU64>,
) -> Result<Zipped, String> {
    use zip::CompressionMethod;
    use zip::write::SimpleFileOptions;

    let (name, items, total) = collect(paths)?;
    let file_name = format!("{name}.zip");
    let path = work_slot()
        .map_err(|e| format!("Failed to create zip file: {e}"))?
        .join(&file_name);
    let cancelled = || cancel.load(Ordering::Relaxed);

    let run = || -> Result<(u32, u32, Vec<Entry>), String> {
        let file =
            std::fs::File::create(&path).map_err(|e| format!("Failed to create zip file: {e}"))?;
        let mut zw = zip::ZipWriter::new(BufWriter::new(file));
        // Level 2: zlib-rs's level 1 is deflate_quick (up to ~20% larger); 2 still beats C zlib's 1.
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(2));
        let (mut entries, mut files, mut dirs, mut written) = (Vec::new(), 0u32, 0u32, 0u64);
        const CHUNK: usize = 512 * 1024;
        for item in &items {
            let rel = item.name.replace('\\', "/");
            if item.is_dir {
                dirs += 1;
                let dir = format!("{rel}/");
                zw.add_directory(&dir, options)
                    .map_err(|e| format!("Failed to add directory: {e}"))?;
                if entries.len() < LISTED {
                    entries.push(Entry {
                        path: dir,
                        size: 0,
                        is_dir: true,
                    });
                }
                continue;
            }
            if cancelled() {
                return Err("Cancelled".into());
            }
            // Re-checked at zip time: a file swapped for a link since the walk is skipped.
            match std::fs::symlink_metadata(&item.source) {
                Ok(m) if m.file_type().is_symlink() => continue,
                Err(_) => continue,
                _ => {}
            }
            files += 1;
            zw.start_file(&rel, options.large_file(item.size >= u32::MAX as u64))
                .map_err(|e| format!("Failed to start file in zip: {e}"))?;
            if item.size > 0 {
                let data = std::fs::read(&item.source)
                    .map_err(|e| format!("Failed to read file {}: {e}", item.source.display()))?;
                for chunk in data.chunks(CHUNK) {
                    if cancelled() {
                        return Err("Cancelled".into());
                    }
                    zw.write_all(chunk)
                        .map_err(|e| format!("Failed to write to zip: {e}"))?;
                    written += chunk.len() as u64;
                    if let Some(p) = (written * 100).checked_div(total) {
                        percent.store(p.min(100), Ordering::Relaxed);
                    }
                }
            }
            if entries.len() < LISTED {
                entries.push(Entry {
                    path: rel,
                    size: item.size,
                    is_dir: false,
                });
            }
        }
        zw.finish()
            .map_err(|e| format!("Failed to finalize zip: {e}"))?;
        Ok((files, dirs, entries))
    };
    match run() {
        Ok((file_count, dir_count, entries)) => {
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            Ok(Zipped {
                path,
                name: file_name,
                size,
                file_count,
                dir_count,
                entries,
            })
        }
        Err(e) => {
            forget(&path);
            Err(e)
        }
    }
}
