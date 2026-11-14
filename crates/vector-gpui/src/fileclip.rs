//! Files on the OS clipboard, so they paste into a file manager or another app as real files.

use std::path::PathBuf;

/// A sealed or held file written out under this launch's exports, for another app to read.
pub async fn materialize(hash: Option<[u8; 32]>, path: String, name: String) -> Result<PathBuf, String> {
    let bytes = match path.is_empty() {
        false => vector_core::vcache::read(std::path::Path::new(&path))?.to_vec(),
        true => crate::backend::attachments::bytes(hash, String::new())
            .await
            .ok_or("This file isn't on this device any more")?,
    };
    let mut id = [0u8; 8];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut id);
    let dir = root()?.join(vector_core::simd::hex::bytes_to_hex_string(&id));
    tokio::fs::create_dir_all(&dir).await.map_err(|e| format!("Couldn't prepare the file: {e}"))?;
    let name = vector_core::crypto::sanitize_filename(&name);
    let file = dir.join(if name.is_empty() { "file" } else { &name });
    tokio::fs::write(&file, bytes).await.map_err(|e| format!("Couldn't prepare the file: {e}"))?;
    Ok(file)
}

fn root() -> Result<PathBuf, String> {
    Ok(dirs::cache_dir().ok_or("No cache folder")?.join("io.vectorapp.gpui").join("exports"))
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
    if pb.writeObjects(&array) { Ok(()) } else { Err("Pasteboard rejected the file references".into()) }
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
            Err(e) if attempt == 19 => return Err(format!("Failed to open clipboard: {e}")),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
    let _clip = clip;
    FileList.write_clipboard(&paths[..]).map_err(|e| format!("Clipboard write failed: {e}"))
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "macos", windows)))]
pub fn write(_paths: Vec<PathBuf>, _cx: &mut gpui::App) -> Result<(), String> {
    Err("Copying files to the clipboard isn't supported on this platform yet".into())
}
