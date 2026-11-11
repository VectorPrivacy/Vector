//! Storage (`commands/system.rs` on desktop): what this account's files and the shared caches
//! weigh by type, and clearing them, all of it or one slice of the chart.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use vector_core::media::import::{self, OnDisk};

/// The caches Clear sweeps: every image cache kind and the resampled sounds.
fn cache_kinds() -> impl Iterator<Item = &'static str> {
    vector_core::vcache::KINDS.iter().copied().chain(["sounds"])
}

fn dir_size(path: &Path) -> u64 {
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_file() => m.len(),
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            _ => 0,
        })
        .sum()
}

/// The type a plain file is charted and cleared as: its name's extension, else its message's.
fn plain_ext(item: &OnDisk) -> String {
    Path::new(&item.path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| item.extension.to_lowercase())
}

/// Bytes by type: an extension, `/cache`, or `/ai_models`.
pub fn distribution() -> HashMap<String, u64> {
    let mut out: HashMap<String, u64> = HashMap::new();
    for item in import::on_disk().unwrap_or_default() {
        if vector_core::db::saved::is_saved(&item.path).unwrap_or(true) {
            continue;
        }
        if let Ok(meta) = std::fs::metadata(&item.path) {
            *out.entry(plain_ext(&item)).or_default() += meta.len();
        }
    }
    for (mime, _, bytes) in vector_core::db::media_store::totals().unwrap_or_default() {
        let ext = vector_core::crypto::extension_from_mime(&mime);
        *out.entry(if ext.is_empty() { "bin".into() } else { ext })
            .or_default() += bytes;
    }
    if let Ok(root) = vector_core::db::get_app_data_dir() {
        let cache: u64 = cache_kinds()
            .map(|k| dir_size(&root.join("cache").join(k)))
            .sum();
        if cache > 0 {
            *out.entry("/cache".into()).or_default() += cache;
        }
    }
    out
}

fn total() -> u64 {
    distribution().values().sum()
}

/// Forget the kept copies loaded messages show, of `exts` or all; their messages repaint.
async fn clear_kept(exts: Option<&HashSet<String>>) -> Result<(), String> {
    let mut changed: Vec<(String, vector_core::Message)> = Vec::new();
    {
        let mut state = vector_core::STATE.lock().await;
        for ci in 0..state.chats.len() {
            let mut ids = Vec::new();
            for message in state.chats[ci].messages.iter_mut() {
                let mut touched = false;
                for att in &mut message.attachments {
                    if !att.downloaded() || att.has_path() {
                        continue;
                    }
                    if let Some(set) = exts {
                        let ext = vector_core::crypto::extension_from_mime(
                            vector_core::crypto::mime_from_extension(att.extension()),
                        );
                        if !set.contains(&ext) {
                            continue;
                        }
                    }
                    vector_core::media::store::forget(att.id());
                    att.set_downloaded(false);
                    att.set_downloading(false);
                    att.set_path(String::new());
                    touched = true;
                }
                if touched {
                    ids.push(*message.id());
                }
            }
            let chat_id = state.chats[ci].id().to_string();
            for id in ids {
                let hex = vector_core::simd::hex::bytes_to_hex_32(&id);
                if let Some(m) = state.chats[ci].messages.find_by_hex_id(&hex) {
                    changed.push((chat_id.clone(), m.to_message(&state.interner)));
                }
            }
        }
    }
    let mut by_chat: HashMap<String, Vec<vector_core::Message>> = HashMap::new();
    for (chat, m) in &changed {
        by_chat.entry(chat.clone()).or_default().push(m.clone());
    }
    for (chat, msgs) in by_chat {
        vector_core::db::events::save_chat_messages(&chat, &msgs).await?;
    }
    for (chat, mut m) in changed {
        let old = m.id.clone();
        vector_core::traits::emit_message_update(&chat, &old, &mut m).await;
    }
    Ok(())
}

/// Let go of this account's plain files that `matches`, unless another account or a Save has them.
async fn release_downloads(matches: impl Fn(&OnDisk) -> bool) {
    let Ok((Ok(files), elsewhere)) = vector_core::db::blocking_bound(|| {
        let _ = import::reclaim_unmarked();
        (import::on_disk(), import::elsewhere())
    })
    .await
    else {
        return;
    };
    let elsewhere = std::sync::Arc::new(elsewhere);
    for item in files.into_iter().filter(|i| matches(i)) {
        let (file, others) = (item.clone(), elsewhere.clone());
        match vector_core::db::blocking_bound(move || {
            import::release(&file, others.as_ref().as_ref())
        })
        .await
        {
            Ok(Ok(events)) => {
                let updates =
                    import::reflect_released(&mut *vector_core::STATE.lock().await, &item, &events);
                for (chat, mut m) in updates {
                    let old = m.id.clone();
                    vector_core::traits::emit_message_update(&chat, &old, &mut m).await;
                }
            }
            Ok(Err(e)) => log::warn!("couldn't let a download go: {e}"),
            Err(_) => return,
        }
    }
}

fn clear_caches() {
    if let Ok(root) = vector_core::db::get_app_data_dir() {
        for kind in cache_kinds() {
            let _ = std::fs::remove_dir_all(root.join("cache").join(kind));
        }
    }
    super::sound::purge();
}

/// Delete what no account on this device shows and nobody saved. Returns the bytes freed.
async fn clear_unclaimed() -> u64 {
    let Ok(Some(shown)) = vector_core::db::blocking_bound(import::elsewhere).await else {
        return 0;
    };
    let dir = vector_core::db::get_download_dir();
    let Ok(files) = vector_core::db::blocking_bound(move || import::unclaimed(&dir, &shown)).await
    else {
        return 0;
    };
    files
        .into_iter()
        .filter(|(p, _)| std::fs::remove_file(p).is_ok())
        .map(|(_, len)| len)
        .sum()
}

/// Everything: this account's files, what no account claims, and the caches. Returns bytes freed.
pub async fn clear_all() -> Result<u64, String> {
    vector_core::db::scoped(async {
        let before = total();
        clear_kept(None).await?;
        vector_core::db::blocking_bound(|| vector_core::media::store::forget_where(|_| true))
            .await?;
        release_downloads(|_| true).await;
        let unclaimed = clear_unclaimed().await;
        clear_caches();
        Ok(before.saturating_sub(total()) + unclaimed)
    })
    .await
}

/// One slice: `cache`, or the files of `exts`. Returns bytes freed.
pub async fn clear_category(category: &str, exts: Vec<String>) -> Result<u64, String> {
    let category = category.to_string();
    vector_core::db::scoped(async move {
        let before = total();
        if category == "cache" {
            clear_caches();
        } else {
            let set: HashSet<String> = exts.iter().map(|e| e.to_lowercase()).collect();
            if set.is_empty() {
                return Err("No file types to clear".into());
            }
            clear_kept(Some(&set)).await?;
            let kept = set.clone();
            vector_core::db::blocking_bound(move || {
                vector_core::media::store::forget_where(|mime| {
                    kept.contains(&vector_core::crypto::extension_from_mime(mime))
                })
            })
            .await?;
            release_downloads(move |item| set.contains(&plain_ext(item))).await;
        }
        Ok(before.saturating_sub(total()))
    })
    .await
}

/// What turning Keep Files off would delete: (files, bytes).
pub async fn kept_summary() -> (u64, u64) {
    let (mut files, mut bytes) =
        vector_core::db::blocking_bound(vector_core::media::store::kept_files)
            .await
            .unwrap_or((0, 0));
    if let Ok(root) = vector_core::db::get_app_data_dir() {
        for kind in ["inline_images", "embed_videos"] {
            for e in std::fs::read_dir(root.join("cache").join(kind))
                .into_iter()
                .flatten()
                .flatten()
            {
                if let Some(m) = e.metadata().ok().filter(|m| m.is_file()) {
                    files += 1;
                    bytes += m.len();
                }
            }
        }
    }
    (files, bytes)
}

/// Keep Files on or off. Off deletes what it kept, so a file is kept or held, never both.
pub async fn set_keep_media(on: bool) -> Result<(), String> {
    vector_core::db::scoped(async move {
        vector_core::db::set_sql_setting("keep_media".into(), on.to_string())?;
        if on {
            return Ok(());
        }
        vector_core::db::set_sql_setting("keep_unencrypted".into(), "false".into())?;
        clear_kept(None).await?;
        vector_core::db::blocking_bound(|| vector_core::media::store::forget_where(|_| true))
            .await?;
        if let Ok(root) = vector_core::db::get_app_data_dir() {
            for kind in ["inline_images", "embed_videos"] {
                let _ = std::fs::remove_dir_all(root.join("cache").join(kind));
            }
        }
        release_downloads(|_| true).await;
        Ok(())
    })
    .await
}

/// Auto-download (enabled, limit), migrating the pre-split single limit where 0 meant off.
pub fn auto_download() -> (bool, u64) {
    const DEFAULT: u64 = 10_485_760;
    const MIN: u64 = 1_048_576;
    let get = |k: &str| {
        vector_core::db::get_sql_setting(k.to_string())
            .ok()
            .flatten()
    };
    let limit = get("max_auto_download_bytes").and_then(|v| v.parse::<u64>().ok());
    match get("auto_download_enabled").as_deref() {
        Some("true") => (true, limit.filter(|l| *l > 0).unwrap_or(DEFAULT)),
        Some("false") => (false, limit.filter(|l| *l > 0).unwrap_or(DEFAULT)),
        _ => {
            let (on, limit) = match limit {
                Some(0) => (false, MIN),
                Some(l) => (true, l),
                None => (true, DEFAULT),
            };
            let _ =
                vector_core::db::set_sql_setting("auto_download_enabled".into(), on.to_string());
            let _ = vector_core::db::set_sql_setting(
                "max_auto_download_bytes".into(),
                limit.to_string(),
            );
            (on, limit)
        }
    }
}
