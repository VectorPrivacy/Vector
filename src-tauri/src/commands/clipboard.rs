//! Native file clipboard.
//!
//! Finder/Explorer "Copy" puts file *references* on the OS clipboard, which the
//! WebView never surfaces to JS — so reading them (to paste a file into a chat)
//! has to go through the native pasteboard. Increment 1 covers the macOS read;
//! the other desktop platforms + Android land in later increments. Everywhere
//! it isn't wired yet, the command returns an empty list and the paste handler
//! falls back to its existing image-bytes path.

/// Absolute paths of files currently on the OS clipboard, in clipboard order.
/// Empty when the clipboard holds no file references (plain text, raw image
/// bytes from a screenshot, etc.) or on a platform not yet wired.
#[tauri::command]
pub async fn read_clipboard_files() -> Result<Vec<String>, String> {
    read_clipboard_files_impl()
}

#[cfg(target_os = "macos")]
fn read_clipboard_files_impl() -> Result<Vec<String>, String> {
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::{NSArray, NSString};

    // NSPasteboard is documented thread-safe (it proxies the pasteboard server),
    // so reading the general pasteboard off the main thread is fine. We copy the
    // immutable NSString paths straight into a Rust Vec; no Obj-C state escapes.
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        // Legacy `NSFilenamesPboardType` returns an array of plain path strings —
        // simpler and more reliable across sources than reconstructing each
        // `public.file-url` item.
        let ftype = NSString::from_str("NSFilenamesPboardType");
        let Some(plist) = pb.propertyListForType(&ftype) else {
            return Ok(Vec::new());
        };
        // objc2 only erases to `NSArray<AnyObject>`; element types are checked
        // per-item (a stray non-string entry is skipped, not a hard error).
        let Ok(arr) = plist.downcast::<NSArray<AnyObject>>() else {
            return Ok(Vec::new());
        };
        let mut out = Vec::with_capacity(arr.len());
        for item in arr.iter() {
            if let Ok(s) = item.downcast::<NSString>() {
                out.push(s.to_string());
            }
        }
        Ok(out)
    }
}

#[cfg(target_os = "android")]
fn read_clipboard_files_impl() -> Result<Vec<String>, String> {
    Ok(crate::android::storage::clipboard_read_files())
}

#[cfg(not(any(target_os = "macos", target_os = "android")))]
fn read_clipboard_files_impl() -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

/// Put file references on the OS clipboard so they paste into Finder/Explorer
/// (or back into a chat) as real files. Paths must be absolute and exist on
/// disk. Increment 2 covers macOS; other platforms return an error until wired.
#[tauri::command]
pub async fn write_clipboard_files(paths: Vec<String>) -> Result<(), String> {
    if paths.is_empty() {
        return Err("No files to copy".to_string());
    }
    write_clipboard_files_impl(paths)
}

#[cfg(target_os = "macos")]
fn write_clipboard_files_impl(paths: Vec<String>) -> Result<(), String> {
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSPasteboard, NSPasteboardWriting};
    use objc2_foundation::{NSArray, NSString, NSURL};

    // SAFETY: writes immutable file-URL objects to the process-global pasteboard;
    // nothing escapes the block.
    unsafe {
        let urls: Vec<_> = paths
            .iter()
            .map(|p| NSURL::fileURLWithPath(&NSString::from_str(p)))
            .collect();
        let writers: Vec<&ProtocolObject<dyn NSPasteboardWriting>> =
            urls.iter().map(|u| ProtocolObject::from_ref(&**u)).collect();
        let array = NSArray::from_slice(&writers);

        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        if pb.writeObjects(&array) {
            Ok(())
        } else {
            Err("Pasteboard rejected the file references".to_string())
        }
    }
}

#[cfg(target_os = "android")]
fn write_clipboard_files_impl(paths: Vec<String>) -> Result<(), String> {
    match crate::android::storage::clipboard_copy_files(&paths) {
        Ok(true) => Ok(()),
        Ok(false) => Err("No files were copied to the clipboard".to_string()),
        Err(e) => Err(e),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "android")))]
fn write_clipboard_files_impl(_paths: Vec<String>) -> Result<(), String> {
    Err("Copying files to the clipboard isn't supported on this platform yet".to_string())
}
