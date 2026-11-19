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

/// How this copy was installed, as the Tauri updater names it in a manifest key: an update
/// arrives as the same kind of bundle, so each install upgrades in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    App,
    AppImage,
    Deb,
    Rpm,
    Nsis,
    Msi,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::App => "app",
            Kind::AppImage => "appimage",
            Kind::Deb => "deb",
            Kind::Rpm => "rpm",
            Kind::Nsis => "nsis",
            Kind::Msi => "msi",
        }
    }
}

/// The manifest entries for `kind`, best first: its own, then the one the 0.5 test builds read
/// for the three kinds they shipped as.
fn keys_for(kind: Kind) -> Vec<String> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let base = format!("{os}-{}", std::env::consts::ARCH);
    let mut keys = vec![format!("{base}-{}", kind.name())];
    if matches!(kind, Kind::App | Kind::AppImage | Kind::Nsis) {
        keys.push(format!("{base}-gpui"));
    }
    keys
}

pub fn asset(manifest: &serde_json::Value) -> Option<Asset> {
    asset_for(manifest, kind()?)
}

fn asset_for(manifest: &serde_json::Value, kind: Kind) -> Option<Asset> {
    let platforms = manifest.get("platforms")?;
    keys_for(kind).iter().find_map(|key| {
        let p = platforms.get(key)?;
        Some(Asset {
            url: p.get("url")?.as_str()?.to_string(),
            signature: p.get("signature")?.as_str()?.to_string(),
        })
    })
}

/// Whether the running copy can be replaced in place.
pub fn supported() -> bool {
    kind().is_some()
}

fn kind() -> Option<Kind> {
    target().map(|(kind, _)| kind)
}

/// What the update replaces: the `.app` bundle, the AppImage, or the install folder.
fn target() -> Option<(Kind, PathBuf)> {
    let exe = std::env::current_exe().ok()?;
    if cfg!(target_os = "macos") {
        return exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).map(|p| (Kind::App, p.to_path_buf()));
    }
    if cfg!(target_os = "linux") {
        if let Some(image) = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file()) {
            return Some((Kind::AppImage, image));
        }
        return linux_package(&exe).map(|kind| (kind, exe));
    }
    let dir = exe.parent()?.to_path_buf();
    // The NSIS installer leaves its uninstaller beside the exe; the MSI installs per machine.
    if dir.join("uninstall.exe").is_file() {
        return Some((Kind::Nsis, dir));
    }
    let under = ["ProgramW6432", "ProgramFiles"].iter().filter_map(std::env::var_os).any(|p| dir.starts_with(p));
    under.then_some((Kind::Msi, dir))
}

/// Which package manager owns `exe`, when one of the `vector` packages put it there.
fn linux_package(exe: &Path) -> Option<Kind> {
    if !exe.starts_with("/usr") {
        return None;
    }
    let listed = std::fs::read_to_string("/var/lib/dpkg/info/vector.list")
        .is_ok_and(|list| list.lines().any(|line| Path::new(line) == exe));
    if listed {
        return Some(Kind::Deb);
    }
    let owned = std::process::Command::new("rpm")
        .args(["-qf", "--quiet"])
        .arg(exe)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    owned.then_some(Kind::Rpm)
}

fn verify(bytes: &[u8], signature: &str) -> Result<(), String> {
    verify_with(PUBKEY, bytes, signature)
}

fn verify_with(pubkey: &str, bytes: &[u8], signature: &str) -> Result<(), String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let text = |s: &str| {
        b64.decode(s.trim()).ok().and_then(|b| String::from_utf8(b).ok()).ok_or_else(|| crate::t::settings_update_bad_signature().to_string())
    };
    let key = minisign_verify::PublicKey::decode(&text(pubkey)?).map_err(|_| crate::t::settings_update_bad_key().to_string())?;
    let sig = minisign_verify::Signature::decode(&text(signature)?).map_err(|_| crate::t::settings_update_bad_signature().to_string())?;
    key.verify(bytes, &sig, false).map_err(|_| crate::t::settings_update_signature_mismatch().to_string())
}

/// Download and verify the bundle, reporting `(received, total)`; the checked file's path.
pub async fn download(asset: Asset, progress: impl Fn(u64, u64)) -> Result<PathBuf, String> {
    let client = vector_core::net::build_http_client_with_options(None, Some(vector_core::net::transfer_stall()), true)?;
    let mut resp = client.get(&asset.url).send().await.map_err(|e| crate::t::settings_update_download_failed(e.to_string()).to_string())?;
    if !resp.status().is_success() {
        return Err(crate::t::settings_update_download_http(resp.status().to_string()).to_string());
    }
    let total = resp.content_length().unwrap_or(0);
    let mut bytes = Vec::with_capacity(total as usize);
    let mut shown = 0u64;
    while let Some(chunk) = resp.chunk().await.map_err(|e| crate::t::settings_update_download_interrupted(e.to_string()).to_string())? {
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
    std::fs::create_dir_all(&dir).map_err(|e| crate::t::settings_update_folder_failed(e.to_string()).to_string())?;
    let path = dir.join(name);
    std::fs::write(&path, &bytes).map_err(|e| crate::t::settings_update_file_failed(e.to_string()).to_string())?;
    Ok(path)
}

/// Put the verified bundle in place. On Windows the installer takes over and the app must quit.
pub fn install(bundle: &Path) -> Result<(), String> {
    let (kind, target) = target().ok_or_else(|| crate::t::settings_update_manual().to_string())?;
    match kind {
        Kind::Deb => install_package(bundle, &target, "dpkg", "-i"),
        Kind::Rpm => install_package(bundle, &target, "rpm", "-U"),
        Kind::Msi => install_msi(bundle),
        Kind::App | Kind::AppImage | Kind::Nsis => install_at(bundle, &target),
    }
}

/// The package manager runs as root once this process has gone, through polkit's prompt, then
/// starts the app again whether or not it took the update.
fn install_package(bundle: &Path, exe: &Path, tool: &str, arg: &str) -> Result<(), String> {
    std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("while kill -0 {} 2>/dev/null; do sleep 0.2; done; pkexec {tool} {arg} \"$1\"; rm -f \"$1\"; exec \"$2\"", std::process::id())) // i18n-exempt: shell
        .arg("vector-update")
        .arg(bundle)
        .arg(exe)
        .spawn()
        .map(|_| ())
        .map_err(|e| crate::t::settings_update_installer_failed(e.to_string()).to_string())
}

/// msiexec waits for this process to let go of its files; AUTOLAUNCHAPP starts the app after.
fn install_msi(bundle: &Path) -> Result<(), String> {
    std::process::Command::new("msiexec.exe")
        .arg("/i")
        .arg(bundle)
        .args(["/passive", "AUTOLAUNCHAPP=True"]) // i18n-exempt: msiexec property
        .spawn()
        .map(|_| ())
        .map_err(|e| crate::t::settings_update_installer_failed(e.to_string()).to_string())
}

#[cfg(target_os = "linux")]
fn install_at(bundle: &Path, appimage: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let staged = appimage.with_extension("update");
    std::fs::copy(bundle, &staged).map_err(|e| crate::t::settings_update_stage_failed(e.to_string()).to_string())?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    std::fs::rename(&staged, appimage).map_err(|e| crate::t::settings_update_replace_failed(e.to_string()).to_string())?;
    let _ = std::fs::remove_file(bundle);
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_at(bundle: &Path, app: &Path) -> Result<(), String> {
    let parent = app.parent().ok_or_else(|| crate::t::settings_update_no_folder().to_string())?;
    let staging = parent.join(".vector-update");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| crate::t::settings_update_stage_failed(e.to_string()).to_string())?;
    let file = std::fs::File::open(bundle).map_err(|e| e.to_string())?;
    tar::Archive::new(flate2::read::GzDecoder::new(file))
        .unpack(&staging)
        .map_err(|e| crate::t::settings_update_unpack_failed(e.to_string()).to_string())?;
    let fresh = std::fs::read_dir(&staging)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .ok_or_else(|| crate::t::settings_update_empty().to_string())?;
    // Swapped by rename, so a failure part way leaves the old app where it was.
    let old = staging.join("previous.app");
    std::fs::rename(app, &old).map_err(|e| crate::t::settings_update_replace_failed(e.to_string()).to_string())?;
    if let Err(e) = std::fs::rename(&fresh, app) {
        let _ = std::fs::rename(&old, app);
        return Err(crate::t::settings_update_replace_failed(e.to_string()).to_string());
    }
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_file(bundle);
    Ok(())
}

#[cfg(windows)]
fn install_at(bundle: &Path, dir: &Path) -> Result<(), String> {
    // NSIS (scripts/installer.nsi): no pages, start the app again when done, into this copy's
    // folder. `/D=` must come last and unquoted.
    std::process::Command::new(bundle)
        .args(["/P".into(), "/R".into(), format!("/D={}", dir.display())])
        .spawn()
        .map(|_| ())
        .map_err(|e| crate::t::settings_update_installer_failed(e.to_string()).to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn install_at(_: &Path, _: &Path) -> Result<(), String> {
    Err(crate::t::settings_update_unsupported().into())
}

/// Start the installed copy once this process has gone, so it isn't handed back to us as a
/// second instance. Installers and package managers start it themselves.
pub fn relaunch() -> Result<(), String> {
    #[cfg(unix)]
    {
        let (kind, target) = target().ok_or_else(|| crate::t::settings_update_nothing_to_start().to_string())?;
        if !matches!(kind, Kind::App | Kind::AppImage) {
            return Ok(());
        }
        // i18n-exempt: shell
        let start = if cfg!(target_os = "macos") { "open -n \"$1\"" } else { "exec \"$1\"" };
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("while kill -0 {} 2>/dev/null; do sleep 0.2; done; {start}", std::process::id())) // i18n-exempt: shell
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
    fn each_install_takes_the_bundle_of_its_own_kind() {
        let base = format!("{}-{}", if cfg!(target_os = "macos") { "darwin" } else { std::env::consts::OS }, std::env::consts::ARCH);
        let entry = |name: &str| serde_json::json!({"url": format!("https://x/{name}"), "signature": "c2ln"});
        let m = serde_json::json!({"platforms": {
            base.clone(): entry("bare"),
            format!("{base}-appimage"): entry("appimage"),
            format!("{base}-deb"): entry("deb"),
            format!("{base}-msi"): entry("msi"),
            format!("{base}-gpui"): entry("gpui"),
        }});
        let url = |kind| asset_for(&m, kind).map(|a| a.url);
        assert_eq!(url(Kind::AppImage).as_deref(), Some("https://x/appimage"));
        assert_eq!(url(Kind::Deb).as_deref(), Some("https://x/deb"));
        assert_eq!(url(Kind::Msi).as_deref(), Some("https://x/msi"));
        // A 0.5 manifest has only the test builds' entry.
        assert_eq!(url(Kind::Nsis).as_deref(), Some("https://x/gpui"));
        // Never another kind's bundle, nor the bare entry older clients read.
        assert_eq!(url(Kind::Rpm), None);
    }
}
