//! App-update commands for platforms without the Tauri updater plugin.
//!
//! Desktop checks + installs through the updater plugin end-to-end. Android
//! reads the same release manifest for the version beacon, then splits on where
//! the build came from: a store-installed copy hands off to that store, which
//! owns updates and holds the matching signing key, while a SIDELOADED copy
//! downloads and installs the next release itself.
//!
//! The `fdroid` feature compiles both the check and the download out: that
//! flavour is updated by F-Droid alone.

use tauri::{AppHandle, Runtime};

/// Result of an update-manifest check.
#[derive(serde::Serialize, Clone)]
pub struct AppUpdateInfo {
    pub available: bool,
    pub current: String,
    pub latest: String,
    pub notes: String,
    /// Whether this build is on the preview channel.
    pub preview: bool,
}

/// The desktop updater manifest doubles as the Android version beacon:
/// every release tags desktop + APK builds together, so the top-level
/// `version`/`notes` apply to both.
///
/// GitHub's `/releases/latest` skips pre-releases, so a stable build never
/// sees a preview.
const UPDATE_MANIFEST_URL: &str =
    "https://github.com/VectorPrivacy/Vector/releases/latest/download/latest.json";

/// Preview channel. A fixed-tag pointer release re-pointed at the newest build
/// of *either* channel on every publish, so previews roll forward onto the
/// official release instead of stranding on the last one.
///
/// Desktop reaches this same URL through `tauri.preview.conf.json`, which the
/// release workflow merges into preview builds only.
const PREVIEW_MANIFEST_URL: &str =
    "https://github.com/VectorPrivacy/Vector/releases/download/preview/latest.json";

#[cfg(not(feature = "fdroid"))]
const MANIFEST_MAX_BYTES: usize = 1024 * 1024;

/// A semver pre-release identifier marks a preview build (`0.4.2-1`). It has to
/// be numeric: the MSI bundler rejects anything else, and it lands in the 4th
/// field of the Windows product version (`0.4.2.1`).
///
/// Windows Installer *ignores* that 4th field, so every preview of `0.4.2` and
/// `0.4.2` itself are one and the same version to it. Moving between them is a
/// same-version transition, which only `bundle > windows > allowDowngrades`
/// permits, so it must stay at its default of `true`. No numbering scheme can
/// substitute; ordering on Windows comes from the semver in the manifest, which
/// the updater compares before msiexec is ever invoked.
fn is_preview(version: &semver::Version) -> bool {
    !version.pre.is_empty()
}

/// True when `latest` is strictly newer than `current`.
///
/// Semver ordering is the whole preview scheme: it ranks
/// `0.4.1 < 0.4.2-1 < 0.4.2-2 < 0.4.2`, which is what carries a preview user
/// onto the official release. Unparseable input fails closed, so garbage never
/// announces an update.
///
/// A stable build additionally refuses previews outright, rather than trusting
/// that the release was flagged pre-release on GitHub. Mirrors the desktop
/// comparator in `lib.rs`, so both platforms fail the same way.
#[cfg_attr(feature = "fdroid", allow(dead_code))]
fn version_is_newer(latest: &str, current: &semver::Version) -> bool {
    version_is_newer_opts(latest, current, !current.pre.is_empty())
}

/// `preview_channel` names the channel the user follows, not the build they
/// run: `true` accepts anything newer (release candidates included), `false`
/// accepts newer STABLE releases only — which is what lets a preview build
/// opt back out and ride this RC until the official release lands.
fn version_is_newer_opts(latest: &str, current: &semver::Version, preview_channel: bool) -> bool {
    let Ok(latest) = semver::Version::parse(latest.trim().trim_start_matches('v')) else {
        return false;
    };
    if !latest.pre.is_empty() && !preview_channel {
        return false;
    }
    latest > *current
}

/// Fetch the release manifest and compare against the running version.
/// Uses the shared Tor-aware HTTP client, so with Tor enabled the check
/// routes through it (or fails closed while it bootstraps).
#[tauri::command]
pub async fn check_app_update<R: Runtime>(
    handle: AppHandle<R>,
    beta: Option<bool>,
) -> Result<AppUpdateInfo, String> {
    #[cfg(feature = "fdroid")]
    {
        let _ = (handle, beta);
        Err("This build is updated by F-Droid".to_string())
    }
    #[cfg(not(feature = "fdroid"))]
    {
        let current = handle.package_info().version.clone();
        let preview = is_preview(&current);
        // `beta` names the channel to FOLLOW, defaulting to the build's native
        // one. true from a stable build = offer release candidates (the pointer
        // tracks both channels, so official releases still arrive). false from a
        // preview build = the opt-out: stable manifest, stable versions only.
        let beta = beta.unwrap_or(preview);
        let client = vector_core::net::shared_http_client();
        let mut resp = client
            .get(if beta {
                PREVIEW_MANIFEST_URL
            } else {
                UPDATE_MANIFEST_URL
            })
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
            available: version_is_newer_opts(&latest, &current, beta),
            current: current.to_string(),
            latest,
            notes,
            preview,
        })
    }
}

/// Whether the account this build is about to open was written by a newer
/// Vector.
///
/// Read-only, and deliberately callable before anything touches the database:
/// the block has to replace boot, not follow a failed one. `init_database`
/// refuses regardless, so this only decides whether the user gets an
/// explanation or an error.
#[tauri::command]
pub fn check_account_downgrade() -> Option<vector_core::db::DowngradeBlock> {
    let npub = vector_core::db::read_active_account_file().ok().flatten()?;
    vector_core::db::inspect_downgrade(&npub).ok().flatten()
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

/// The release asset matching THIS build's ABI. The running binary is the
/// authority on its own architecture, so no device probing is needed — and
/// installing the wrong split would fail or ship dead native code.
#[cfg(all(target_os = "android", not(feature = "fdroid")))]
const APK_ASSET: &str = {
    #[cfg(target_arch = "aarch64")]
    {
        "Vector-arm64-v8a.apk"
    }
    #[cfg(target_arch = "arm")]
    {
        "Vector-armeabi-v7a.apk"
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "arm")))]
    {
        "Vector.apk"
    }
};

/// Download the matching release APK and hand it to the system installer.
///
/// SIDELOADS ONLY. A store-installed copy is signed by that store's key, so its
/// update has to come from the same place; this refuses rather than download
/// tens of megabytes that Android will reject. The install itself is gated on a
/// signing-certificate match, and the user still confirms in the system dialog.
///
/// Emits `update_download_progress` ({ received, total }) while streaming.
#[tauri::command]
pub async fn download_and_install_update<R: Runtime>(
    app: AppHandle<R>,
    beta: Option<bool>,
) -> Result<String, String> {
    #[cfg(all(target_os = "android", not(feature = "fdroid")))]
    {
        use tauri::Emitter;
        use futures_util::StreamExt;

        // Belt-and-braces: the UI only offers this on a sideload, but the
        // command is reachable on its own.
        if get_install_source().has_store {
            return Err("This build updates through the store that installed it".to_string());
        }

        let current = app.package_info().version.to_string();
        let preview = semver::Version::parse(&current).map(|v| is_preview(&v)).unwrap_or(false);
        let url = if beta.unwrap_or(preview) {
            format!("https://github.com/VectorPrivacy/Vector/releases/download/preview/{APK_ASSET}")
        } else {
            format!("https://github.com/VectorPrivacy/Vector/releases/latest/download/{APK_ASSET}")
        };

        let client = vector_core::net::build_http_client(std::time::Duration::from_secs(120))?;
        let resp = client.get(&url).send().await.map_err(|e| format!("update download failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("update download failed: HTTP {}", resp.status()));
        }
        let total = resp.content_length().unwrap_or(0);

        // App-private cache, not the public download dir: an update binary is
        // not the user's media, and a half-written APK must not surface in
        // their gallery or file manager.
        let dir = std::path::PathBuf::from(vector_core::db::get_download_dir()).join(".updates");
        std::fs::create_dir_all(&dir).map_err(|e| format!("update dir: {e}"))?;
        let path = dir.join(APK_ASSET);
        // Stream to a PARTIAL file, renamed only once complete, so an
        // interrupted download can never be handed to the installer.
        let part = dir.join(format!("{APK_ASSET}.part"));
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&part).map_err(|e| format!("update file: {e}"))?;
            let mut stream = resp.bytes_stream();
            let mut received: u64 = 0;
            let mut last_emit = 0u64;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| format!("update download interrupted: {e}"))?;
                file.write_all(&chunk).map_err(|e| format!("update write: {e}"))?;
                received += chunk.len() as u64;
                // Throttle: one event per ~256KiB keeps the IPC quiet on a
                // 60MB APK (Android's raw-IPC budget is not generous).
                if received - last_emit >= 256 * 1024 || received == total {
                    last_emit = received;
                    let _ = app.emit("update_download_progress", serde_json::json!({
                        "received": received,
                        "total": total,
                    }));
                }
            }
            file.flush().map_err(|e| format!("update flush: {e}"))?;
        }
        std::fs::rename(&part, &path).map_err(|e| format!("update rename: {e}"))?;

        let status = crate::android::updates::install_update(&path.to_string_lossy())?;
        match status.as_str() {
            "ok" => Ok("installing".to_string()),
            "needs-permission" => Ok("needs-permission".to_string()),
            "signature-mismatch" => {
                // Keeping it would only re-fail; the user needs their original source.
                let _ = std::fs::remove_file(&path);
                Err("This update is signed by a different key than the installed app — reinstall from where you originally got Vector".to_string())
            }
            "unverifiable" => {
                let _ = std::fs::remove_file(&path);
                Err("Could not verify the update's signature, so it was not installed".to_string())
            }
            other => Err(format!("update install failed ({other})")),
        }
    }
    #[cfg(not(all(target_os = "android", not(feature = "fdroid"))))]
    {
        let _ = (app, beta);
        Err("No in-app updater in this build".to_string())
    }
}

/// The update found by `check_channel_update`, held for the user's install
/// click. Process-global by design: an update applies to the install, not to
/// an account, so a swap must not drop it.
#[cfg(desktop)]
static PENDING_CHANNEL_UPDATE: std::sync::Mutex<Option<tauri_plugin_updater::Update>> =
    std::sync::Mutex::new(None);

/// Check a channel the baked updater config does NOT point at, by building a
/// second updater at runtime. Two callers: a stable build opted into Beta
/// Updates (`channel: "preview"` — the pointer, release candidates accepted),
/// and a preview build opted back OUT (`channel: "stable"` — the stable
/// manifest, stable versions only, so the build sits on its RC until the
/// official release lands). Same manifest format, same signature check.
///
/// `proxy` carries the Tor SOCKS address when Tor is on — the updater plugin
/// has its own HTTP client, so the frontend resolves transport exactly as it
/// does for the plugin's JS `check`.
#[tauri::command]
pub async fn check_channel_update<R: Runtime>(
    handle: AppHandle<R>,
    channel: String,
    proxy: Option<String>,
) -> Result<AppUpdateInfo, String> {
    #[cfg(desktop)]
    {
        use tauri_plugin_updater::UpdaterExt;
        let current = handle.package_info().version.clone();
        let preview = is_preview(&current);
        let (url, comparator): (_, fn(semver::Version, tauri_plugin_updater::RemoteRelease) -> bool) =
            match channel.as_str() {
                "preview" => (PREVIEW_MANIFEST_URL, |current, release| {
                    release.version > current
                }),
                "stable" => (UPDATE_MANIFEST_URL, |current, release| {
                    release.version.pre.is_empty() && release.version > current
                }),
                other => return Err(format!("Unknown update channel: {other}")),
            };
        let endpoint =
            tauri::Url::parse(url).map_err(|e| format!("Bad update endpoint: {e}"))?;
        let mut builder = handle
            .updater_builder()
            .endpoints(vec![endpoint])
            .map_err(|e| format!("Update check failed: {e}"))?
            .version_comparator(comparator);
        if let Some(p) = proxy {
            builder = builder.proxy(
                tauri::Url::parse(&p).map_err(|e| format!("Bad proxy address: {e}"))?,
            );
        }
        let updater = builder
            .build()
            .map_err(|e| format!("Update check failed: {e}"))?;
        let found = updater
            .check()
            .await
            .map_err(|e| format!("Update check failed: {e}"))?;
        let info = AppUpdateInfo {
            available: found.is_some(),
            current: current.to_string(),
            latest: found.as_ref().map(|u| u.version.clone()).unwrap_or_default(),
            notes: found
                .as_ref()
                .and_then(|u| u.body.clone())
                .unwrap_or_default(),
            preview,
        };
        *PENDING_CHANNEL_UPDATE.lock().unwrap() = found;
        Ok(info)
    }
    #[cfg(not(desktop))]
    {
        let _ = (handle, channel, proxy);
        Err("Channel checks run through the manifest on this platform".to_string())
    }
}

/// Download and install the update stashed by `check_channel_update`.
///
/// Emits `update_download_progress` ({ received, total }) like the Android
/// path; the frontend offers the restart once this returns. On Windows the
/// installer may relaunch the app itself, so the caller must tolerate never
/// seeing the result.
#[tauri::command]
pub async fn install_channel_update<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    #[cfg(desktop)]
    {
        use tauri::Emitter;
        let update = PENDING_CHANNEL_UPDATE
            .lock()
            .unwrap()
            .clone()
            .ok_or("No pending update — check for updates first")?;
        let mut received: u64 = 0;
        let mut last_emit = 0u64;
        let progress_app = app.clone();
        update
            .download_and_install(
                move |chunk, total| {
                    received += chunk as u64;
                    let total = total.unwrap_or(0);
                    // Throttle to ~256KiB steps; installers run tens of MB.
                    if received - last_emit >= 256 * 1024 || received == total {
                        last_emit = received;
                        let _ = progress_app.emit(
                            "update_download_progress",
                            serde_json::json!({ "received": received, "total": total }),
                        );
                    }
                },
                || {},
            )
            .await
            .map_err(|e| format!("Update install failed: {e}"))?;
        *PENDING_CHANNEL_UPDATE.lock().unwrap() = None;
        Ok(())
    }
    #[cfg(not(desktop))]
    {
        let _ = app;
        Err("Desktop-only".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{is_preview, version_is_newer, version_is_newer_opts};

    /// build.gradle.kts's `versionToCode`, kept in step by the test below.
    fn android_version_code(version: &str) -> Option<u32> {
        let core = version.split(['-', '+']).next()?;
        let mut parts = core.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
        let (major, minor, patch) = (parts.next()?, parts.next()?, parts.next()?);
        let slot = match version.split_once('-') {
            None => 99,
            Some((_, pre)) => match pre.split('+').next()?.parse::<u32>() {
                Ok(n) if (1..=98).contains(&n) => n,
                _ => return None,
            },
        };
        Some((major * 10000 + minor * 100 + patch) * 100 + slot)
    }

    /// F-Droid's update checker reads versionName/versionCode from a flat
    /// file, which must therefore track Cargo.toml release for release.
    #[test]
    fn android_version_properties_mirror_cargo() {
        let props = include_str!("../../gen/android/app/version.properties");
        let get = |key: &str| {
            props
                .lines()
                .find_map(|l| l.trim().strip_prefix(key)?.trim_start().strip_prefix('='))
                .map(str::trim)
        };
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(get("versionName"), Some(version), "version.properties versionName");
        assert_eq!(
            get("versionCode").and_then(|c| c.parse::<u32>().ok()),
            android_version_code(version),
            "version.properties versionCode"
        );
    }

    #[test]
    fn android_version_code_ranks_previews_below_release() {
        assert_eq!(android_version_code("0.4.2"), Some(40299));
        assert_eq!(android_version_code("0.4.2-1"), Some(40201));
        assert!(android_version_code("0.4.2-1") < android_version_code("0.4.2"));
        assert!(android_version_code("0.4.1") < android_version_code("0.4.2-1"));
        assert_eq!(android_version_code("0.4.2-rc1"), None);
    }

    fn v(s: &str) -> semver::Version {
        semver::Version::parse(s).unwrap()
    }

    #[test]
    fn version_compare_basics() {
        assert!(version_is_newer("0.4.1", &v("0.4.0")));
        assert!(version_is_newer("0.5.0", &v("0.4.9")));
        assert!(version_is_newer("1.0.0", &v("0.9.9")));
        assert!(version_is_newer("v0.4.1", &v("0.4.0")));
        assert!(version_is_newer("0.4.10", &v("0.4.9")));
        assert!(!version_is_newer("0.4.0", &v("0.4.0")));
        assert!(!version_is_newer("0.4.0", &v("0.4.1")));
    }

    #[test]
    fn malformed_manifest_never_announces() {
        assert!(!version_is_newer("garbage", &v("0.4.0")));
        assert!(!version_is_newer("", &v("0.4.0")));
        // Not semver (missing patch) — fails closed rather than guessing.
        assert!(!version_is_newer("0.5", &v("0.4.0")));
        assert!(!version_is_newer(
            "99999999999999999999999999.0.0",
            &v("0.4.0")
        ));
    }

    #[test]
    fn preview_channel_ordering() {
        // The ladder a preview user climbs: newer previews, then the release.
        assert!(version_is_newer("0.4.2-2", &v("0.4.2-1")));
        assert!(version_is_newer("0.4.2", &v("0.4.2-2")));
        // Numeric identifiers compare numerically, so 10 outranks 9.
        assert!(version_is_newer("0.4.2-10", &v("0.4.2-9")));
        // A preview of the next version still reaches a preview user.
        assert!(version_is_newer("0.4.3-1", &v("0.4.2-1")));
        // Never backwards.
        assert!(!version_is_newer("0.4.2-1", &v("0.4.2")));
        assert!(!version_is_newer("0.4.2-1", &v("0.4.2-2")));
    }

    #[test]
    fn stable_never_accepts_a_preview() {
        // Defence for a preview that reached the stable endpoint, e.g. a release
        // published without the pre-release flag. Semver alone would say yes.
        assert!(v("0.4.2-1") > v("0.4.1"));
        assert!(!version_is_newer("0.4.2-1", &v("0.4.1")));
        assert!(!version_is_newer("0.5.0-1", &v("0.4.1")));
        // Stable still takes stable.
        assert!(version_is_newer("0.4.2", &v("0.4.1")));
        // Previews still take previews, and the official release.
        assert!(version_is_newer("0.4.2-2", &v("0.4.2-1")));
        assert!(version_is_newer("0.4.2", &v("0.4.2-1")));
    }

    #[test]
    fn beta_opt_in_accepts_release_candidates() {
        // The one gate the preview channel lifts: a NEWER pre-release.
        assert!(version_is_newer_opts("0.4.4-1", &v("0.4.3"), true));
        assert!(!version_is_newer_opts("0.4.4-1", &v("0.4.3"), false));
        // Ordering is untouched: no downgrades, no same-version churn.
        assert!(!version_is_newer_opts("0.4.4-1", &v("0.4.4"), true));
        assert!(!version_is_newer_opts("0.4.3-9", &v("0.4.3"), true));
        // Stable releases still arrive with the flag on.
        assert!(version_is_newer_opts("0.4.4", &v("0.4.3"), true));
        // Garbage still fails closed.
        assert!(!version_is_newer_opts("garbage", &v("0.4.3"), true));
    }

    #[test]
    fn beta_opt_out_parks_an_rc_until_the_official_release() {
        // A preview build following the STABLE channel: no further release
        // candidates, but the official release still carries it home.
        assert!(!version_is_newer_opts("0.4.4-2", &v("0.4.4-1"), false));
        assert!(!version_is_newer_opts("0.4.5-1", &v("0.4.4-1"), false));
        assert!(version_is_newer_opts("0.4.4", &v("0.4.4-1"), false));
        // The default (no opt-out) keeps the preview ladder intact.
        assert!(version_is_newer("0.4.4-2", &v("0.4.4-1")));
    }

    #[test]
    fn preview_detection() {
        assert!(is_preview(&v("0.4.2-1")));
        assert!(!is_preview(&v("0.4.2")));
    }
}
