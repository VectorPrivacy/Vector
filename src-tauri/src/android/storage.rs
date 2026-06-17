//! Android public-storage helpers.
//!
//! Bridges to the Kotlin `io.vectorapp.VectorFiles` helper so the
//! intent / FileProvider / MediaScanner work stays in type-checked code. The
//! Context is passed as the first argument to each static method.

use jni::objects::{JClass, JObject, JObjectArray, JString, JValue};
use jni::JNIEnv;

use super::utils::with_android_context;

/// Load an app class via the context's classloader (the system classloader used
/// by `find_class` on native threads can't see app classes).
///
/// `getClassLoader` is invoked on the Context *instance* (`Context.getClassLoader()`
/// always returns the app's PathClassLoader), NOT on the context's class —
/// `Class.getClassLoader()` returns the boot loader for framework context types
/// (Application / Service), which can't see app classes.
fn load_class<'a>(
    env: &mut JNIEnv<'a>,
    activity: &JObject<'a>,
    name: &str,
) -> Result<JClass<'a>, String> {
    let class_loader = env
        .call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
        .map_err(|e| format!("classloader: {:?}", e))?
        .l()
        .map_err(|e| format!("classloader obj: {:?}", e))?;
    let j_name = env
        .new_string(name.replace('/', "."))
        .map_err(|e| format!("class name str: {:?}", e))?;
    let cls = env
        .call_method(
            &class_loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&j_name)],
        )
        .map_err(|e| format!("loadClass {}: {:?}", name, e))?
        .l()
        .map_err(|e| format!("loaded class obj: {:?}", e))?;
    Ok(JClass::from(cls))
}

/// Vector's public download directory under external media storage
/// (`/Android/media/<pkg>/Vector`). `None` if external media is unavailable.
pub fn external_media_dir() -> Option<String> {
    with_android_context(|env, activity| {
        let cls = load_class(env, activity, "io/vectorapp/VectorFiles")?;
        let res = env
            .call_static_method(
                &cls,
                "externalMediaDir",
                "(Landroid/content/Context;)Ljava/lang/String;",
                &[JValue::Object(activity)],
            )
            .map_err(|e| format!("externalMediaDir: {:?}", e))?
            .l()
            .map_err(|e| format!("externalMediaDir obj: {:?}", e))?;
        if res.is_null() {
            return Err("externalMediaDir returned null".to_string());
        }
        let s: String = env
            .get_string(&JString::from(res))
            .map_err(|e| format!("externalMediaDir str: {:?}", e))?
            .into();
        Ok(s)
    })
    .ok()
}

/// Path of the `.nomedia` marker in the shared media dir. Its presence tells
/// Android's MediaScanner to skip (and evict) everything in Vector's media dir.
fn nomedia_marker() -> std::path::PathBuf {
    vector_core::db::get_download_dir().join(".nomedia")
}

/// True when the user has hidden Vector's media from the gallery. Device-wide by
/// nature: the marker governs the whole shared media dir, regardless of account.
pub fn gallery_hidden() -> bool {
    nomedia_marker().exists()
}

/// Hide or reveal all of Vector's saved media in the gallery. Writes/removes the
/// `.nomedia` marker, then forces a MediaScanner pass over existing files so they
/// are evicted (hidden) or re-indexed (visible) to match the new state.
pub fn set_gallery_hidden(hidden: bool) -> Result<(), String> {
    let dir = vector_core::db::get_download_dir();
    let marker = dir.join(".nomedia");
    if hidden {
        std::fs::write(&marker, b"").map_err(|e| format!("create .nomedia: {:?}", e))?;
    } else if let Err(e) = std::fs::remove_file(&marker) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(format!("remove .nomedia: {:?}", e));
        }
    }

    // Re-run the scanner over existing files so it re-evaluates them against the
    // marker (raw scan — bypasses the gallery_hidden gate, since when hiding we
    // explicitly want the scan to evict). Batched to bound JNI churn.
    const BATCH: usize = 128;
    let mut batch: Vec<String> = Vec::with_capacity(BATCH);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.file_name().map(|n| n == ".nomedia").unwrap_or(false) {
                continue;
            }
            batch.push(path.to_string_lossy().to_string());
            if batch.len() >= BATCH {
                scan_files_raw(&batch);
                batch.clear();
            }
        }
    }
    if !batch.is_empty() {
        scan_files_raw(&batch);
    }
    Ok(())
}

/// Best-effort: index a file into the gallery / file managers immediately.
/// No-op when the user has hidden Vector's media from the gallery.
pub fn scan_file(path: &str) {
    if gallery_hidden() {
        return;
    }
    scan_file_raw(path);
}

fn scan_file_raw(path: &str) {
    let path = path.to_string();
    let _ = with_android_context(|env, activity| {
        let cls = load_class(env, activity, "io/vectorapp/VectorFiles")?;
        let j_path = env.new_string(&path).map_err(|e| format!("path str: {:?}", e))?;
        env.call_static_method(
            &cls,
            "scanFile",
            "(Landroid/content/Context;Ljava/lang/String;)V",
            &[JValue::Object(activity), JValue::Object(&j_path)],
        )
        .map_err(|e| format!("scanFile: {:?}", e))?;
        Ok(())
    });
}

/// Batch index — one scanner request + one JNI round-trip for many files.
/// No-op when the user has hidden Vector's media from the gallery.
fn scan_files(paths: &[String]) {
    if paths.is_empty() || gallery_hidden() {
        return;
    }
    scan_files_raw(paths);
}

/// Raw batch index — ignores the gallery-hidden gate. Used by the toggle to force
/// the scanner to re-evaluate files against a just-changed `.nomedia` marker.
fn scan_files_raw(paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    let _ = with_android_context(|env, activity| {
        let cls = load_class(env, activity, "io/vectorapp/VectorFiles")?;
        let string_cls = env
            .find_class("java/lang/String")
            .map_err(|e| format!("String class: {:?}", e))?;
        let arr = env
            .new_object_array(paths.len() as i32, &string_cls, JObject::null())
            .map_err(|e| format!("new String[]: {:?}", e))?;
        for (i, p) in paths.iter().enumerate() {
            let js = env.new_string(p).map_err(|e| format!("path str: {:?}", e))?;
            env.set_object_array_element(&arr, i as i32, &js)
                .map_err(|e| format!("set arr[{}]: {:?}", i, e))?;
        }
        env.call_static_method(
            &cls,
            "scanFiles",
            "(Landroid/content/Context;[Ljava/lang/String;)V",
            &[JValue::Object(activity), JValue::Object(&arr)],
        )
        .map_err(|e| format!("scanFiles: {:?}", e))?;
        Ok(())
    });
}

/// One-time migration of pre-existing downloads into the public "Vector" dir.
///
/// Legacy downloads lived in app-private locations (`$DOWNLOAD/vector` for the
/// foreground path, `$APPDATA/vector_downloads` for bg-sync) with absolute
/// paths stored in `events.tags`. This moves any files still there into the new
/// external media dir, indexes them, and prefix-swaps this account's stored
/// paths so in-app display keeps working.
///
/// Multi-account safe: the file move is shared and naturally idempotent (the
/// first account to boot moves them), while each account rewrites its own DB
/// once — guarded by a per-account settings flag.
pub fn migrate_old_downloads() {
    use tauri::{Manager, Emitter};

    const FLAG: &str = "android_downloads_migrated_v1";
    if vector_core::db::settings::get_sql_setting(FLAG.to_string())
        .ok()
        .flatten()
        .as_deref()
        == Some("true")
    {
        return;
    }

    let new_dir = vector_core::db::get_download_dir();
    let _ = std::fs::create_dir_all(&new_dir);

    let Some(handle) = crate::TAURI_APP.get() else { return };
    let mut old_dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(d) = handle.path().download_dir() {
        old_dirs.push(d.join("vector"));
    }
    if let Ok(d) = handle.path().app_data_dir() {
        old_dirs.push(d.join("vector_downloads"));
    }

    // Phase 1: enumerate the move work up front so we can report progress.
    // (Users may have thousands of files; cross-volume copies are slow.)
    let mut moves: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
    for old_dir in &old_dirs {
        if old_dir == &new_dir || !old_dir.exists() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(old_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let Some(name) = path.file_name().map(|n| n.to_owned()) else { continue };
                let name_str = name.to_string_lossy();
                if name_str.starts_with('.') && name_str.ends_with(".tmp") {
                    continue; // in-progress download temp file
                }
                let dest = new_dir.join(&name);
                if dest.exists() {
                    continue; // already present (content-addressed name collision)
                }
                moves.push((path, dest));
            }
        }
    }

    // Phase 2: move files, preserving names. Progress is emitted at most once
    // per whole percent on the boot "progress_operation" channel (the frontend
    // already renders "{message} ({pct}%)" on the unlock screen). MediaScanner
    // requests are batched to avoid per-file JNI + connection churn.
    const SCAN_BATCH: usize = 128;
    let total = moves.len();
    if total > 0 {
        let _ = handle.emit("progress_operation", serde_json::json!({
            "type": "start", "message": "Migrating files"
        }));
        let mut scan_batch: Vec<String> = Vec::with_capacity(SCAN_BATCH);
        let mut last_pct: u32 = u32::MAX;
        for (done, (src, dest)) in moves.iter().enumerate() {
            // Rename within the same volume; fall back to copy+delete across.
            let moved = std::fs::rename(src, dest).is_ok()
                || (std::fs::copy(src, dest).is_ok() && {
                    let _ = std::fs::remove_file(src);
                    true
                });
            if moved {
                scan_batch.push(dest.to_string_lossy().to_string());
                if scan_batch.len() >= SCAN_BATCH {
                    scan_files(&scan_batch);
                    scan_batch.clear();
                }
            } else {
                // Move failed: drop any partial copy and the original. Phase 3
                // sees no file at the new path and marks the attachment
                // undownloaded, so it re-downloads cleanly post-boot.
                let _ = std::fs::remove_file(dest);
                let _ = std::fs::remove_file(src);
            }
            let done = done + 1;
            let pct = ((done as u64 * 100) / total as u64) as u32;
            if pct != last_pct {
                last_pct = pct;
                let _ = handle.emit("progress_operation", serde_json::json!({
                    "type": "progress", "current": done, "total": total, "message": "Migrating files"
                }));
            }
        }
        scan_files(&scan_batch); // flush remainder
    }

    // Phase 3: reconcile this account's stored attachment paths against disk.
    // One index-backed pass over attachment-bearing events (kind=FILE_ATTACHMENT)
    // — no full-table scan. Each attachment under an old dir is rewritten to the
    // new path if the file is actually there, else marked undownloaded (covers
    // failed/skipped moves). Paths live as JSON inside events.tags.
    const FILE_ATTACHMENT_KIND: u16 = 15; // vector_core::stored_event::event_kind::FILE_ATTACHMENT
    let old_prefixes: Vec<String> = old_dirs
        .iter()
        .filter(|d| *d != &new_dir)
        .map(|d| format!("{}/", d.to_string_lossy()))
        .collect();
    if !old_prefixes.is_empty() {
        if let Ok(conn) = crate::account_manager::get_db_connection_guard_static() {
            let rows: Vec<(i64, String)> = conn
                .prepare("SELECT id, tags FROM events WHERE kind = ?1")
                .and_then(|mut stmt| {
                    stmt.query_map(rusqlite::params![FILE_ATTACHMENT_KIND], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()
                })
                .unwrap_or_default();

            for (id, tags_json) in rows {
                let mut tags: Vec<Vec<String>> = match serde_json::from_str(&tags_json) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                let Some(idx) = tags.iter().position(|t| t.first().map(|s| s.as_str()) == Some("attachments")) else { continue };
                let mut atts: Vec<crate::Attachment> = serde_json::from_str(
                    tags[idx].get(1).map(|s| s.as_str()).unwrap_or("[]"),
                ).unwrap_or_default();

                let mut changed = false;
                for att in atts.iter_mut() {
                    if old_prefixes.iter().any(|p| att.path.starts_with(p.as_str())) {
                        if let Some(name) = std::path::Path::new(&att.path).file_name() {
                            let new_path = new_dir.join(name);
                            if new_path.exists() {
                                att.path = new_path.to_string_lossy().to_string();
                            } else {
                                att.downloaded = false;
                                att.downloading = false;
                            }
                            changed = true;
                        }
                    }
                }

                if changed {
                    if let Ok(atts_json) = serde_json::to_string(&atts) {
                        tags[idx] = vec!["attachments".to_string(), atts_json];
                        if let Ok(new_tags) = serde_json::to_string(&tags) {
                            let _ = conn.execute(
                                "UPDATE events SET tags = ?1 WHERE id = ?2",
                                rusqlite::params![new_tags, id],
                            );
                        }
                    }
                }
            }
        }
    }

    let _ = vector_core::db::settings::set_sql_setting(FLAG.to_string(), "true".to_string());
    eprintln!("[Migration] Android downloads migrated to public Vector dir ({} files)", total);
}

/// Open a file via an ACTION_VIEW chooser. Returns true if an activity launched.
pub fn open_file(path: &str) -> Result<bool, String> {
    call_file_action("openFile", path)
}

/// Share a file via Android's share sheet (ACTION_SEND). Returns true if launched.
pub fn share_file(path: &str) -> Result<bool, String> {
    call_file_action("shareFile", path)
}

/// Put files on the system clipboard as content:// URIs (FileProvider). Returns
/// true if a clip was set.
pub fn clipboard_copy_files(paths: &[String]) -> Result<bool, String> {
    if paths.is_empty() {
        return Ok(false);
    }
    with_android_context(|env, activity| {
        let cls = load_class(env, activity, "io/vectorapp/VectorFiles")?;
        let string_cls = env
            .find_class("java/lang/String")
            .map_err(|e| format!("String class: {:?}", e))?;
        let arr = env
            .new_object_array(paths.len() as i32, &string_cls, JObject::null())
            .map_err(|e| format!("new String[]: {:?}", e))?;
        for (i, p) in paths.iter().enumerate() {
            let js = env.new_string(p).map_err(|e| format!("path str: {:?}", e))?;
            env.set_object_array_element(&arr, i as i32, &js)
                .map_err(|e| format!("set arr[{}]: {:?}", i, e))?;
        }
        let res = env
            .call_static_method(
                &cls,
                "copyFilesToClipboard",
                "(Landroid/content/Context;[Ljava/lang/String;)Z",
                &[JValue::Object(activity), JValue::Object(&arr)],
            )
            .map_err(|e| format!("copyFilesToClipboard: {:?}", e))?
            .z()
            .map_err(|e| format!("copyFilesToClipboard bool: {:?}", e))?;
        Ok(res)
    })
}

/// Copy clipboard file URIs into the app cache; returns their absolute paths
/// (empty on a text-only clip or a denied read).
pub fn clipboard_read_files() -> Vec<String> {
    with_android_context(|env, activity| {
        let cls = load_class(env, activity, "io/vectorapp/VectorFiles")?;
        let res = env
            .call_static_method(
                &cls,
                "readClipboardFiles",
                "(Landroid/content/Context;)[Ljava/lang/String;",
                &[JValue::Object(activity)],
            )
            .map_err(|e| format!("readClipboardFiles: {:?}", e))?
            .l()
            .map_err(|e| format!("readClipboardFiles obj: {:?}", e))?;
        if res.is_null() {
            return Ok(Vec::new());
        }
        let arr = JObjectArray::from(res);
        let len = env
            .get_array_length(&arr)
            .map_err(|e| format!("array len: {:?}", e))?;
        let mut out = Vec::with_capacity(len as usize);
        for i in 0..len {
            let el = env
                .get_object_array_element(&arr, i)
                .map_err(|e| format!("arr[{}]: {:?}", i, e))?;
            if el.is_null() {
                continue;
            }
            let s: String = env
                .get_string(&JString::from(el))
                .map_err(|e| format!("str: {:?}", e))?
                .into();
            out.push(s);
        }
        Ok(out)
    })
    .unwrap_or_default()
}

/// Invoke a `VectorFiles` static method of shape `(Context, String) -> bool`.
fn call_file_action(method: &str, path: &str) -> Result<bool, String> {
    let path = path.to_string();
    with_android_context(|env, activity| {
        let cls = load_class(env, activity, "io/vectorapp/VectorFiles")?;
        let j_path = env.new_string(&path).map_err(|e| format!("path str: {:?}", e))?;
        let res = env
            .call_static_method(
                &cls,
                method,
                "(Landroid/content/Context;Ljava/lang/String;)Z",
                &[JValue::Object(activity), JValue::Object(&j_path)],
            )
            .map_err(|e| format!("{}: {:?}", method, e))?
            .z()
            .map_err(|e| format!("{} bool: {:?}", method, e))?;
        Ok(res)
    })
}
