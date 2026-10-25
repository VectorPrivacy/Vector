//! The main window's webview keeps no HTTP cache on disk. Its only network loads are the
//! embedded players, and what they fetch (thumbnails, audio, video) is what a chat shows. Each
//! engine's cache folder is taken away before the first webview starts, and whatever an engine
//! still holds is purged once the window exists. Local storage and IndexedDB are untouched.

#[cfg(desktop)]
use std::path::PathBuf;

/// Before any webview: a file where each engine wants its cache folder, so the cache can't open
/// and the engine runs without one.
#[cfg(desktop)]
pub fn block(identifier: &str) {
    block_paths(cache_paths(identifier));
}

#[cfg(desktop)]
fn block_paths(paths: Vec<PathBuf>) {
    for path in paths {
        if path.is_file() {
            continue;
        }
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::File::create(&path);
    }
}

/// WebKit names its folder by the bundle identifier, or by the process when unbundled.
#[cfg(target_os = "macos")]
fn cache_paths(identifier: &str) -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return Vec::new() };
    let caches = home.join("Library").join("Caches");
    let process = std::env::current_exe().ok().and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()));
    [Some(identifier.to_string()), process]
        .into_iter()
        .flatten()
        .map(|name| caches.join(name).join("WebKit").join("NetworkCache"))
        .collect()
}

/// WebView2's profile lives in the app's local data folder, where Tauri points it.
#[cfg(windows)]
fn cache_paths(identifier: &str) -> Vec<PathBuf> {
    let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else { return Vec::new() };
    vec![local.join(identifier).join("EBWebView").join("Default").join("Cache")]
}

/// WebKitGTK caches under the context's data folder, the app's local data folder.
#[cfg(target_os = "linux")]
fn cache_paths(identifier: &str) -> Vec<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")));
    data.map(|d| vec![d.join(identifier).join("WebKitCache")]).unwrap_or_default()
}

/// Purge the main window's caches: anything from before `block`, or kept by an engine that
/// found a way around it. Linux also switches its cache model off.
#[cfg(desktop)]
pub fn purge(window: &tauri::WebviewWindow) {
    #[cfg(target_os = "macos")]
    let _ = window.with_webview(|wv| unsafe {
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        use objc2_foundation::{NSSet, NSString};
        // SAFETY: with_webview runs this on the main thread with wry's live WKWebView; each
        // message is a public WebKit API with the argument types it declares.
        let view = wv.inner() as *mut AnyObject;
        let config: *mut AnyObject = msg_send![view, configuration];
        let store: *mut AnyObject = msg_send![config, websiteDataStore];
        if store.is_null() {
            return;
        }
        let kinds = ["WKWebsiteDataTypeDiskCache", "WKWebsiteDataTypeFetchCache", "WKWebsiteDataTypeOfflineWebApplicationCache"]
            .map(NSString::from_str);
        let types = NSSet::from_retained_slice(&kinds);
        let since: *mut AnyObject = msg_send![class!(NSDate), distantPast];
        let done = block2::RcBlock::new(|| {});
        let _: () = msg_send![store, removeDataOfTypes: &*types, modifiedSince: since, completionHandler: &*done];
    });

    #[cfg(target_os = "linux")]
    let _ = window.with_webview(|wv| {
        use webkit2gtk::{CacheModel, WebContextExt, WebViewExt};
        if let Some(context) = wv.inner().context() {
            context.set_cache_model(CacheModel::DocumentViewer);
            context.clear_cache();
        }
    });

    #[cfg(windows)]
    let _ = window.with_webview(|wv| unsafe {
        use webview2_com::Microsoft::Web::WebView2::Win32::{
            ICoreWebView2Profile2, ICoreWebView2_13, COREWEBVIEW2_BROWSING_DATA_KINDS_CACHE_STORAGE,
            COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE,
        };
        use webview2_windows_core::Interface;
        // SAFETY: with_webview runs this on the UI thread with wry's live controller; every
        // call is a WebView2 COM method on the interface it belongs to.
        let Ok(core) = wv.controller().CoreWebView2() else { return };
        let Ok(profile) = core.cast::<ICoreWebView2_13>().and_then(|c| c.Profile()) else { return };
        let Ok(profile) = profile.cast::<ICoreWebView2Profile2>() else { return };
        let kinds = COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE | COREWEBVIEW2_BROWSING_DATA_KINDS_CACHE_STORAGE;
        let done = webview2_com::ClearBrowsingDataCompletedHandler::create(Box::new(|_| Ok(())));
        let _ = profile.ClearBrowsingData(kinds, &done);
    });
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn a_cache_folder_becomes_a_file_and_stays_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("WebKit").join("NetworkCache");
        std::fs::create_dir_all(path.join("Version 17")).unwrap();
        std::fs::write(path.join("Version 17").join("blob"), b"a cached picture").unwrap();
        block_paths(vec![path.clone()]);
        block_paths(vec![path.clone()]);
        assert!(path.is_file() && std::fs::read(&path).unwrap().is_empty());
        let fresh = dir.path().join("fresh").join("NetworkCache");
        block_paths(vec![fresh.clone()]);
        assert!(fresh.is_file());
        assert!(cache_paths("io.vectorapp").iter().all(|p| p.ends_with("WebKit/NetworkCache")));
    }
}
