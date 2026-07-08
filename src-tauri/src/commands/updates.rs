//! App-update commands for platforms without the Tauri updater plugin.
//!
//! Desktop checks + installs through the updater plugin end-to-end. Android
//! can't self-update, so it reads the same release manifest for the version
//! beacon and hands off to wherever the APK came from (store or website).

use tauri::{AppHandle, Runtime};

/// Result of an update-manifest check.
#[derive(serde::Serialize, Clone)]
pub struct AppUpdateInfo {
    pub available: bool,
    pub current: String,
    pub latest: String,
    pub notes: String,
}

/// The desktop updater manifest doubles as the Android version beacon:
/// every release tags desktop + APK builds together, so the top-level
/// `version`/`notes` apply to both.
const UPDATE_MANIFEST_URL: &str =
    "https://github.com/VectorPrivacy/Vector/releases/latest/download/latest.json";
const MANIFEST_MAX_BYTES: usize = 1024 * 1024;

/// True when `latest` is strictly newer than `current` (dotted numeric
/// compare over the first three segments). Non-numeric or overflowing
/// segments read as 0, so garbage never announces an update; a legitimately
/// larger version does. Pre-release/build suffixes (after `-`/`+`) are
/// ignored, comparing on the numeric core.
fn version_is_newer(latest: &str, current: &str) -> bool {
    fn parts(v: &str) -> [u64; 3] {
        let mut out = [0u64; 3];
        for (i, seg) in v
            .trim()
            .trim_start_matches('v')
            .split(['.', '-', '+'])
            .take(3)
            .enumerate()
        {
            out[i] = seg.parse().unwrap_or(0);
        }
        out
    }
    parts(latest) > parts(current)
}

/// Fetch the release manifest and compare against the running version.
/// Uses the shared Tor-aware HTTP client, so with Tor enabled the check
/// routes through it (or fails closed while it bootstraps).
#[tauri::command]
pub async fn check_app_update<R: Runtime>(handle: AppHandle<R>) -> Result<AppUpdateInfo, String> {
    let current = handle.package_info().version.to_string();
    let client = vector_core::net::shared_http_client();
    let mut resp = client
        .get(UPDATE_MANIFEST_URL)
        .send()
        .await
        .map_err(|e| format!("Update check failed: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("Update check failed: HTTP {}", resp.status()));
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("Update check failed: {}", e))?
    {
        body.extend_from_slice(&chunk);
        if body.len() > MANIFEST_MAX_BYTES {
            return Err("Update manifest too large".to_string());
        }
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("Update manifest parse failed: {}", e))?;
    let latest = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().trim_start_matches('v').to_string())
        .unwrap_or_default();
    if latest.is_empty() {
        return Err("Update manifest missing version".to_string());
    }
    let notes = manifest
        .get("notes")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(AppUpdateInfo {
        available: version_is_newer(&latest, &current),
        current,
        latest,
        notes,
    })
}

/// Where this build can be updated from.
#[derive(serde::Serialize, Clone)]
pub struct InstallSource {
    /// A store installed us and we can hand off to it. `false` = sideload
    /// (browser/adb/file manager) or the installer's app is gone.
    pub has_store: bool,
    /// Store name for the button ("Zapstore", "F-Droid", ...). Empty when
    /// `has_store` is false.
    pub label: String,
}

/// Human-readable name for the store that installed this build. Any store
/// is supported, not a fixed list: the label comes from the installer's own
/// app name, so F-Droid, UP Store, Aurora, etc. all read correctly. A couple
/// of overrides just tidy verbose official names.
#[tauri::command]
pub fn get_install_source() -> InstallSource {
    #[cfg(target_os = "android")]
    {
        if let Ok(Some(pkg)) = crate::android::updates::get_installer_package() {
            // A "store" is an installer that can actually open our store page.
            // Browsers, file managers, and the system installer UI get recorded
            // as the installer for a sideload but resolve no market:// handler,
            // so they correctly fall through to the website.
            if crate::android::updates::resolves_market_link(&pkg).unwrap_or(false) {
                let label = match pkg.as_str() {
                    "com.android.vending" => "Google Play".to_string(),
                    _ => crate::android::updates::get_app_label(&pkg)
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "your app store".to_string()),
                };
                return InstallSource { has_store: true, label };
            }
        }
        InstallSource { has_store: false, label: String::new() }
    }
    #[cfg(not(target_os = "android"))]
    {
        InstallSource { has_store: false, label: String::new() }
    }
}

/// Open Vector's page in whatever store installed this build, landing the
/// user on that store's own Update control. The `market://` app-details
/// scheme is registered by every mainstream Android store (Play, Zapstore,
/// F-Droid, Aurora, ...), so this is store-agnostic by design. Returns
/// `false` when it can't hand off (only reachable if the store vanished
/// since `get_install_source` confirmed it), so the caller opens the website.
#[tauri::command]
pub fn open_update_source() -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        let installer = match crate::android::updates::get_installer_package() {
            Ok(Some(pkg)) => pkg,
            _ => return Ok(false),
        };
        // Pin the store scheme to the installer so Android routes it straight
        // back to the store that shipped us, not a chooser.
        crate::android::updates::open_url_in_app(&installer, "market://details?id=io.vectorapp")
    }
    #[cfg(not(target_os = "android"))]
    {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::version_is_newer;

    #[test]
    fn version_compare_basics() {
        assert!(version_is_newer("0.4.1", "0.4.0"));
        assert!(version_is_newer("0.5.0", "0.4.9"));
        assert!(version_is_newer("1.0.0", "0.9.9"));
        assert!(version_is_newer("v0.4.1", "0.4.0"));
        assert!(!version_is_newer("0.4.0", "0.4.0"));
        assert!(!version_is_newer("0.4.0", "0.4.1"));
        assert!(!version_is_newer("garbage", "0.4.0"));
        assert!(!version_is_newer("", "0.4.0"));
        assert!(version_is_newer("0.4.10", "0.4.9"));
        // Overflowing segment fails safe to 0 (never announces).
        assert!(!version_is_newer("99999999999999999999999999.0.0", "0.4.0"));
        // Pre-release/build suffixes ignored: compares on the numeric core.
        assert!(!version_is_newer("0.5.0-beta", "0.5.0"));
        assert!(!version_is_newer("0.5.0", "0.5.0-beta"));
        assert!(version_is_newer("0.5.1-beta", "0.5.0"));
        // Missing trailing segments read as 0.
        assert!(!version_is_newer("0.4", "0.4.0"));
        assert!(!version_is_newer("0.4.0", "0.4"));
    }
}
