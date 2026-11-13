//! In-app updates: the release's bundle for this platform, checked against the same minisign key
//! the Tauri updater trusts, then put in place of the running app.

use std::path::{Path, PathBuf};

/// The release signing key (`plugins.updater.pubkey` in src-tauri/tauri.conf.json).
const PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDQwRUU4N0Y0RTFEN0VCRTQKUldUazY5Zmg5SWZ1UUk0bEhjRlc2cWZsRWJpb3BSNXVjaFBKYzdmZzF4VENuREkrbEV4aFoxTXgK";

#[derive(Clone, Debug)]
pub struct Asset {
    pub url: String,
    /// Base64 of the minisign `.sig` file, as the manifest carries it.
    pub signature: String,
}

/// This build's entry in a manifest's `platforms`: kept apart from the Tauri client's.
pub fn platform_key() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{os}-{}-gpui", std::env::consts::ARCH)
}

pub fn asset(manifest: &serde_json::Value) -> Option<Asset> {
    let p = manifest.get("platforms")?.get(platform_key())?;
    Some(Asset {
        url: p.get("url")?.as_str()?.to_string(),
        signature: p.get("signature")?.as_str()?.to_string(),
    })
}

/// Whether the running copy can be replaced in place.
pub fn supported() -> bool {
    target().is_some()
}

/// What the update replaces: the `.app` bundle, the AppImage, or (Windows) the install folder.
fn target() -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        return std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file());
    }
    let exe = std::env::current_exe().ok()?;
    if cfg!(target_os = "macos") {
        return exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).map(Path::to_path_buf);
    }
    exe.parent().map(Path::to_path_buf)
}

fn verify(bytes: &[u8], signature: &str) -> Result<(), String> {
    verify_with(PUBKEY, bytes, signature)
}

fn verify_with(pubkey: &str, bytes: &[u8], signature: &str) -> Result<(), String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let text = |s: &str| {
        b64.decode(s.trim()).ok().and_then(|b| String::from_utf8(b).ok()).ok_or("The update's signature is malformed")
    };
    let key = minisign_verify::PublicKey::decode(&text(pubkey)?).map_err(|_| "The release key is malformed")?;
    let sig = minisign_verify::Signature::decode(&text(signature)?).map_err(|_| "The update's signature is malformed")?;
    key.verify(bytes, &sig, false).map_err(|_| "The update's signature does not match, so it was not installed".to_string())
}

/// Download and verify the bundle, reporting `(received, total)`; the checked file's path.
pub async fn download(asset: Asset, progress: impl Fn(u64, u64)) -> Result<PathBuf, String> {
    let client = vector_core::net::build_http_client_with_options(None, Some(vector_core::net::transfer_stall()), true)?;
    let mut resp = client.get(&asset.url).send().await.map_err(|e| format!("Update download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Update download failed: HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut bytes = Vec::with_capacity(total as usize);
    let mut shown = 0u64;
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("Update download interrupted: {e}"))? {
        bytes.extend_from_slice(&chunk);
        let got = bytes.len() as u64;
        if got - shown >= 256 * 1024 || got == total {
            shown = got;
            progress(got, total);
        }
    }
    verify(&bytes, &asset.signature)?;
    let name = asset.url.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("update").to_string();
    let dir = std::env::temp_dir().join("io.vectorapp.gpui-update");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Update folder: {e}"))?;
    let path = dir.join(name);
    std::fs::write(&path, &bytes).map_err(|e| format!("Update file: {e}"))?;
    Ok(path)
}

/// Put the verified bundle in place. On Windows the installer takes over and the app must quit.
pub fn install(bundle: &Path) -> Result<(), String> {
    let target = target().ok_or("This copy of Vector can't update itself; install the new release by hand")?;
    install_at(bundle, &target)
}

#[cfg(target_os = "linux")]
fn install_at(bundle: &Path, appimage: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let staged = appimage.with_extension("update");
    std::fs::copy(bundle, &staged).map_err(|e| format!("Couldn't stage the update: {e}"))?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    std::fs::rename(&staged, appimage).map_err(|e| format!("Couldn't replace the app: {e}"))?;
    let _ = std::fs::remove_file(bundle);
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_at(bundle: &Path, app: &Path) -> Result<(), String> {
    let parent = app.parent().ok_or("The app has no folder")?;
    let staging = parent.join(".vector-update");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("Couldn't stage the update: {e}"))?;
    let file = std::fs::File::open(bundle).map_err(|e| e.to_string())?;
    tar::Archive::new(flate2::read::GzDecoder::new(file))
        .unpack(&staging)
        .map_err(|e| format!("Couldn't unpack the update: {e}"))?;
    let fresh = std::fs::read_dir(&staging)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .ok_or("The update holds no app")?;
    // Swapped by rename, so a failure part way leaves the old app where it was.
    let old = staging.join("previous.app");
    std::fs::rename(app, &old).map_err(|e| format!("Couldn't replace the app: {e}"))?;
    if let Err(e) = std::fs::rename(&fresh, app) {
        let _ = std::fs::rename(&old, app);
        return Err(format!("Couldn't replace the app: {e}"));
    }
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_file(bundle);
    Ok(())
}

#[cfg(windows)]
fn install_at(bundle: &Path, _dir: &Path) -> Result<(), String> {
    // NSIS, passive: progress only, and it starts the app again when done.
    std::process::Command::new(bundle)
        .args(["/P", "/R"])
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Couldn't start the installer: {e}"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn install_at(_: &Path, _: &Path) -> Result<(), String> {
    Err("No in-app updates on this platform".into())
}

/// Start the installed copy once this process has gone, so it isn't handed back to us as a
/// second instance. Windows' installer starts it itself.
pub fn relaunch() -> Result<(), String> {
    #[cfg(unix)]
    {
        let target = target().ok_or("Nothing to start")?;
        let start = if cfg!(target_os = "macos") { "open -n \"$1\"" } else { "exec \"$1\"" };
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("while kill -0 {} 2>/dev/null; do sleep 0.2; done; {start}", std::process::id()))
            .arg("vector-relaunch")
            .arg(target)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(unix))]
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_key_decodes() {
        use base64::Engine;
        let text = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(PUBKEY).unwrap()).unwrap();
        assert!(minisign_verify::PublicKey::decode(&text).is_ok());
    }

    const TEST_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IHRlc3QKUldRQkFnTUVCUVlIQ0FPaEI3L3p6aEMrSFhEZEdPZEx3SmxuNU5Zd202VU5YeDNjaG1RU1ZURzQK";
    const TEST_SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRQkFnTUVCUVlIQ0hxcStvenNSbDhVTG5LMGN0Q3ZJYnFPNXFrd3BMcGt6TmJTRW9CZE1YV0JDMnNYWlFIODVBbkg3RWlOUlFqbzNMMEVnNkZQaWNvMHRPeTFkOGxjcWdrPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDowCWZpbGU6YnVuZGxlCnZwSzJNcU9hQ1QrUFcvTUpvNjhIbkJXKzB4RHdaNXFCM0I3anl6aWpJUlhxSDZWTEtwY0ZpbEFBWFNKaWZ0aXZ4QkVrUFlYQ0pTM1c2V21maUVSR0NBPT0K";

    #[test]
    fn a_signed_bundle_verifies_and_a_changed_one_does_not() {
        assert!(verify_with(TEST_KEY, b"vector update bundle", TEST_SIG).is_ok());
        assert!(verify_with(TEST_KEY, b"vector update bundle!", TEST_SIG).is_err());
        // Signed by another key than the release's.
        assert!(verify(b"vector update bundle", TEST_SIG).is_err());
    }

    #[test]
    fn a_bad_signature_is_refused() {
        use base64::Engine;
        let sig = "untrusted comment: signature\nRUTk69fh9IfuQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\ntrusted comment: x\nAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n";
        let b64 = base64::engine::general_purpose::STANDARD.encode(sig);
        assert!(verify(b"bundle", &b64).is_err());
        assert!(verify(b"bundle", "not base64!").is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_appimage_is_replaced_whole_and_left_runnable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("vector-install-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (app, bundle) = (dir.join("Vector.AppImage"), dir.join("new.AppImage"));
        std::fs::write(&app, b"old").unwrap();
        std::fs::write(&bundle, b"new").unwrap();
        install_at(&bundle, &app).unwrap();
        assert_eq!(std::fs::read(&app).unwrap(), b"new");
        assert_eq!(std::fs::metadata(&app).unwrap().permissions().mode() & 0o777, 0o755);
        assert!(!bundle.exists() && !app.with_extension("update").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_platform_entry_is_this_builds_own() {
        let key = platform_key();
        assert!(key.ends_with("-gpui"));
        let m = serde_json::json!({"platforms": {key.clone(): {"url": "https://x/a.tar.gz", "signature": "c2ln"}}});
        assert_eq!(asset(&m).map(|a| a.url), Some("https://x/a.tar.gz".into()));
        assert!(asset(&serde_json::json!({"platforms": {"linux-x86_64": {"url": "u", "signature": "s"}}})).is_none());
    }
}
