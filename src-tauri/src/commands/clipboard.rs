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
    // Clipboard managers (macOS pasteboard history, Windows history tools)
    // re-serve a copied file as a `file://` URI STRING where the native formats
    // normally carry plain paths. Every consumer downstream expects a
    // filesystem path, so normalize at this one shared boundary — an
    // unparseable URI passes through untouched rather than vanishing.
    Ok(read_clipboard_files_impl()?
        .into_iter()
        .map(|s| {
            if s.starts_with("file://") {
                tauri::Url::parse(&s)
                    .ok()
                    .and_then(|u| u.to_file_path().ok())
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or(s)
            } else {
                s
            }
        })
        .collect())
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

#[cfg(target_os = "windows")]
fn read_clipboard_files_impl() -> Result<Vec<String>, String> {
    // Probe CF_HDROP WITHOUT opening the clipboard (IsClipboardFormatAvailable):
    // this runs on every paste, and a text/image clipboard must not contend for
    // the open at all. Absent format = no file paste, not an error.
    if !clipboard_win::is_format_avail(clipboard_win::formats::CF_HDROP) {
        return Ok(Vec::new());
    }
    // The open races the WebView2 PROCESS, which snapshots the clipboard around
    // the paste event — and clipboard-win's built-in retries are Sleep(0)
    // scheduler yields that burn out inside that window. Retry with a real
    // backoff, and surface the final failure instead of swallowing it into an
    // empty list: the frontend catches and falls back to the in-band file blob.
    let mut last_err = String::new();
    for attempt in 0..20u32 {
        match clipboard_win::get_clipboard::<Vec<String>, _>(clipboard_win::formats::FileList) {
            Ok(paths) => return Ok(paths),
            Err(e) => {
                last_err = e.to_string();
                if attempt < 19 {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    }
    Err(format!("clipboard file read failed: {last_err}"))
}

#[cfg(target_os = "linux")]
fn read_clipboard_files_impl() -> Result<Vec<String>, String> {
    // GTK clipboard ops must run on the GTK main thread; hop there and ferry the
    // result back. wait_for_uris runs a nested main loop, which is safe on-thread.
    let app = crate::TAURI_APP.get().ok_or("App handle unavailable")?.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let paths = (|| -> Vec<String> {
            let Some(display) = gdk::Display::default() else { return Vec::new(); };
            // `default` is the CLIPBOARD selection (not PRIMARY); returns Option.
            let Some(clipboard) = gtk::Clipboard::default(&display) else { return Vec::new(); };
            clipboard
                .wait_for_uris()
                .iter()
                // file:// URIs → local paths (glib handles percent-decoding + host).
                .filter_map(|uri| glib::filename_from_uri(uri.as_str()).ok())
                .map(|(path, _host)| path.to_string_lossy().into_owned())
                .collect()
        })();
        let _ = tx.send(paths);
    })
    .map_err(|e| e.to_string())?;
    // Bounded so an app teardown mid-read can't hang the worker; a timeout just
    // means "no files" and paste falls back.
    Ok(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap_or_default())
}

#[cfg(not(any(target_os = "macos", target_os = "android", target_os = "windows", target_os = "linux")))]
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

#[cfg(target_os = "windows")]
fn write_clipboard_files_impl(paths: Vec<String>) -> Result<(), String> {
    // CF_HDROP: clipboard-win builds the DROPFILES structure from the path list.
    // `FileList`'s Setter is impl'd over the unsized `[T]`, so the by-value
    // `set_clipboard` helper can't reach it — call the trait method on a slice
    // under our own clipboard guard instead.
    use clipboard_win::{formats::FileList, Clipboard, Setter};
    // new_attempts retries are Sleep(0) yields — worthless against another
    // process holding the clipboard (WebView2, clipboard-history tools). Real
    // backoff: ~200ms worst case before giving up.
    let mut clip = None;
    for attempt in 0..20u32 {
        match Clipboard::new_attempts(0) {
            Ok(c) => {
                clip = Some(c);
                break;
            }
            Err(e) if attempt == 19 => return Err(format!("Failed to open clipboard: {}", e)),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
    let _clip = clip;
    FileList
        .write_clipboard(&paths[..])
        .map_err(|e| format!("Clipboard write failed: {}", e))
}

#[cfg(target_os = "linux")]
fn write_clipboard_files_impl(paths: Vec<String>) -> Result<(), String> {
    // Owner-served selection: `set_with_data`'s closure IS the persistent owner,
    // held by GTK until another app claims the clipboard. Serve three formats —
    // text/uri-list (portable paste target), x-special/gnome-copied-files (what
    // GTK file managers read: verb + uri lines, so paste-in-Files copies), and
    // plain text (terminals/editors get the paths). GDK backs this on both X11
    // and Wayland.
    let app = crate::TAURI_APP.get().ok_or("App handle unavailable")?.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let result = (|| -> Result<(), String> {
            let display = gdk::Display::default().ok_or("No display")?;
            let clipboard = gtk::Clipboard::default(&display).ok_or("No clipboard")?;
            let uris = paths
                .iter()
                .map(|p| {
                    glib::filename_to_uri(p, None)
                        .map(|u| u.to_string())
                        .map_err(|e| format!("Not a valid file path ({p}): {e}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let gnome_payload = format!("copy\n{}", uris.join("\n"));
            let text_payload = paths.join("\n");
            let targets = [
                gtk::TargetEntry::new("text/uri-list", gtk::TargetFlags::empty(), 0),
                gtk::TargetEntry::new("x-special/gnome-copied-files", gtk::TargetFlags::empty(), 1),
                gtk::TargetEntry::new("UTF8_STRING", gtk::TargetFlags::empty(), 2),
                gtk::TargetEntry::new("text/plain;charset=utf-8", gtk::TargetFlags::empty(), 2),
            ];
            let ok = clipboard.set_with_data(&targets, move |_clip, sel, info| {
                match info {
                    0 => {
                        let refs: Vec<&str> = uris.iter().map(|s| s.as_str()).collect();
                        sel.set_uris(&refs);
                    }
                    1 => sel.set(&gdk::Atom::intern("x-special/gnome-copied-files"), 8, gnome_payload.as_bytes()),
                    _ => {
                        sel.set_text(&text_payload);
                    }
                }
            });
            if !ok {
                return Err("Could not claim the clipboard".to_string());
            }
            Ok(())
        })();
        let _ = tx.send(result);
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| "Clipboard write timed out".to_string())?
}

#[cfg(not(any(target_os = "macos", target_os = "android", target_os = "windows", target_os = "linux")))]
fn write_clipboard_files_impl(_paths: Vec<String>) -> Result<(), String> {
    Err("Copying files to the clipboard isn't supported on this platform yet".to_string())
}
