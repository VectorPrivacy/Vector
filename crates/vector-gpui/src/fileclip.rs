//! Files on the OS clipboard, so they paste into a file manager or another app as real files.

use std::path::PathBuf;

/// A copied file's name with its type on the end, so whatever it is pasted into knows it.
pub fn named(name: &str, id: &str, ext: &str) -> String {
    let base = if name.is_empty() { id.get(..16).unwrap_or(id) } else { name };
    let has = std::path::Path::new(base).extension().is_some_and(|e| e.eq_ignore_ascii_case(ext));
    if ext.is_empty() || has { base.to_string() } else { format!("{base}.{ext}") }
}

/// A file's bytes: from its place on disk (sealed or not), else its held copy.
pub async fn bytes_of(hash: Option<[u8; 32]>, path: String) -> Result<Vec<u8>, String> {
    match path.is_empty() {
        false => Ok(vector_core::vcache::read(std::path::Path::new(&path))?.to_vec()),
        true => crate::backend::attachments::bytes(hash, String::new())
            .await
            .ok_or_else(|| crate::t::clip_file_gone().to_string()),
    }
}

/// Private Clipboard: copied files stay in Vector's memory, and other apps see only a marker.
pub fn is_private() -> bool {
    crate::backend::prefs::sql_bools(&[("private_clipboard", true)])[0]
}

/// The privately copied file: the token the OS clipboard carries, its bytes and its name.
#[derive(Default)]
struct Private(std::sync::Mutex<Option<(String, std::sync::Arc<Vec<u8>>, String)>>);

fn private() -> std::sync::Arc<Private> {
    vector_core::db::current_session().scoped::<Private, Private>()
}

/// Copy a file inside Vector only: its bytes never leave this process.
pub fn write_private(bytes: Vec<u8>, name: String, cx: &mut gpui::App) {
    let mut id = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut id);
    let token = vector_core::simd::hex::bytes_to_hex_string(&id);
    *private().0.lock().unwrap_or_else(|e| e.into_inner()) = Some((token.clone(), std::sync::Arc::new(bytes), name));
    cx.write_to_clipboard(gpui::ClipboardItem::new_string_with_json_metadata(crate::t::clip_private_marker().to_string(), token));
}

/// The privately copied file, while the OS clipboard still names it.
pub fn read_private(cx: &mut gpui::App) -> Option<(Vec<u8>, String)> {
    let item = cx.read_from_clipboard()?;
    let token = item.entries().iter().find_map(|e| match e {
        gpui::ClipboardEntry::String(s) => s.metadata_json::<String>(),
        _ => None,
    })?;
    let slot = private();
    let held = slot.0.lock().unwrap_or_else(|e| e.into_inner());
    let (t, bytes, name) = held.as_ref()?;
    (*t == token).then(|| (bytes.to_vec(), name.clone()))
}

/// A sealed or held file written out under this launch's exports, for another app to read.
pub async fn materialize(hash: Option<[u8; 32]>, path: String, name: String) -> Result<PathBuf, String> {
    let bytes = bytes_of(hash, path).await?;
    let mut id = [0u8; 8];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut id);
    let dir = root()?.join(vector_core::simd::hex::bytes_to_hex_string(&id));
    tokio::fs::create_dir_all(&dir).await.map_err(|e| crate::t::clip_prepare_failed(e.to_string()).to_string())?;
    let name = vector_core::crypto::sanitize_filename(&name);
    let file = dir.join(if name.is_empty() { "file" } else { &name });
    tokio::fs::write(&file, bytes).await.map_err(|e| crate::t::clip_prepare_failed(e.to_string()).to_string())?;
    Ok(file)
}

fn root() -> Result<PathBuf, String> {
    Ok(dirs::cache_dir().ok_or_else(|| crate::t::clip_no_cache().to_string())?.join("io.vectorapp.gpui").join("exports"))
}

/// Clear what earlier launches handed out. Never mid-run: a paste may still be reading one.
pub fn purge() {
    if let Ok(dir) = root() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Files on the clipboard, if it holds any: an app that copies a file often puts its path as
/// text beside it, and the file is what was meant.
#[cfg(not(target_os = "macos"))]
pub fn read(cx: &mut gpui::App) -> Vec<PathBuf> {
    cx.read_from_clipboard()
        .iter()
        .flat_map(|i| i.entries())
        .find_map(|e| match e {
            gpui::ClipboardEntry::ExternalPaths(p) if !p.paths().is_empty() => Some(p.paths().to_vec()),
            _ => None,
        })
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
#[allow(unused_unsafe)]
pub fn read(_cx: &mut gpui::App) -> Vec<PathBuf> {
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::{NSString, NSURL};

    let kind = NSString::from_str("public.file-url");
    let pb = unsafe { NSPasteboard::generalPasteboard() };
    let Some(items) = (unsafe { pb.pasteboardItems() }) else { return Vec::new() };
    items
        .iter()
        .filter_map(|item| {
            let url = unsafe { item.stringForType(&kind) }?;
            let url = unsafe { NSURL::URLWithString(&url) }?;
            let path = unsafe { url.path() }?;
            Some(PathBuf::from(path.to_string()))
        })
        .filter(|p| p.exists())
        .collect()
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub fn write(paths: Vec<PathBuf>, cx: &mut gpui::App) -> Result<(), String> {
    cx.write_to_clipboard(gpui::ClipboardItem {
        entries: vec![gpui::ClipboardEntry::ExternalPaths(gpui::ExternalPaths(paths.into()))],
    });
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn write(paths: Vec<PathBuf>, _cx: &mut gpui::App) -> Result<(), String> {
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSPasteboard, NSPasteboardWriting};
    use objc2_foundation::{NSArray, NSString, NSURL};

    let urls: Vec<_> = paths
        .iter()
        .map(|p| NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy())))
        .collect();
    let writers: Vec<&ProtocolObject<dyn NSPasteboardWriting>> = urls.iter().map(|u| ProtocolObject::from_ref(&**u)).collect();
    let array = NSArray::from_slice(&writers);
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    if pb.writeObjects(&array) { Ok(()) } else { Err(crate::t::clip_rejected().into()) }
}

#[cfg(windows)]
pub fn write(paths: Vec<PathBuf>, _cx: &mut gpui::App) -> Result<(), String> {
    use clipboard_win::{Clipboard, Setter, formats::FileList};
    let paths: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    // Sleep(0) retries lose to another process holding the clipboard; back off for real.
    let mut clip = None;
    for attempt in 0..20u32 {
        match Clipboard::new_attempts(0) {
            Ok(c) => {
                clip = Some(c);
                break;
            }
            Err(e) if attempt == 19 => return Err(crate::t::clip_open_failed(e.to_string()).to_string()),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
    let _clip = clip;
    FileList.write_clipboard(&paths[..]).map_err(|e| crate::t::clip_write_failed(e.to_string()).to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "macos", windows)))]
pub fn write(_paths: Vec<PathBuf>, _cx: &mut gpui::App) -> Result<(), String> {
    Err(crate::t::clip_unsupported().into())
}

#[cfg(test)]
mod tests {
    use super::named;

    #[test]
    fn a_copied_file_always_carries_its_type() {
        assert_eq!(named("", "0123456789abcdef0123", "png"), "0123456789abcdef.png");
        assert_eq!(named("jojo", "ab", "png"), "jojo.png");
        assert_eq!(named("jojo.PNG", "ab", "png"), "jojo.PNG");
        assert_eq!(named("notes.tar", "ab", "gz"), "notes.tar.gz");
        assert_eq!(named("README", "ab", ""), "README");
    }
}
