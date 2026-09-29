//! Reading a `.xdc` package (a zip): manifest, icon, entry point, realtime use.

use std::io::{Cursor, Read};

use serde::Deserialize;

/// Nothing legitimate is this large; it bounds what one open reads into memory.
pub const MAX_PACKAGE_BYTES: usize = 500 * 1024 * 1024;
const ICON_CAP: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Manifest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub source_code_url: Option<String>,
}

pub struct Package {
    pub manifest: Manifest,
    pub icon: Option<(Vec<u8>, &'static str)>,
    pub file_hash: String,
    pub uses_realtime: bool,
}

type Archive<'a> = zip::ZipArchive<Cursor<&'a [u8]>>;

/// An entry's stored name, matched case-insensitively when there's no exact one.
fn resolve<'a>(archive: &'a Archive<'_>, target: &str) -> Option<String> {
    if archive.file_names().any(|n| n == target) {
        return Some(target.to_string());
    }
    archive.file_names().find(|n| n.eq_ignore_ascii_case(target)).map(str::to_string)
}

/// Entry headers can't be trusted; bound the bytes actually inflated.
fn read_capped(entry: &mut impl Read, cap: u64) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    entry.take(cap + 1).read_to_end(&mut buf).map_err(|e| e.to_string())?;
    if buf.len() as u64 > cap {
        return Err("Zip entry exceeds decompressed size limit".into());
    }
    Ok(buf)
}

fn read_entry(archive: &mut Archive<'_>, name: &str, cap: u64) -> Option<Vec<u8>> {
    let stored = resolve(archive, name)?;
    let mut entry = archive.by_name(&stored).ok()?;
    read_capped(&mut entry, cap).ok()
}

fn icon_mime(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".svg") {
        "image/svg+xml"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else {
        "image/png"
    }
}

pub fn parse(bytes: &[u8], fallback_name: &str) -> Result<Package, String> {
    if bytes.is_empty() {
        return Err("File is empty".into());
    }
    if bytes.len() > MAX_PACKAGE_BYTES {
        return Err(format!("File too large ({} MB)", bytes.len() / (1024 * 1024)));
    }
    let file_hash = vector_core::simd::hex::bytes_to_hex_string(&vector_core::crypto::sha256::digest(bytes));
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("Invalid mini app: {e}"))?;

    let manifest = match read_entry(&mut archive, "manifest.toml", 1024 * 1024) {
        Some(raw) => {
            let text = String::from_utf8(raw).map_err(|e| format!("Invalid manifest: {e}"))?;
            toml::from_str(&text).map_err(|e| format!("Invalid manifest: {e}"))?
        }
        None => Manifest { name: fallback_name.to_string(), ..Default::default() },
    };
    if resolve(&archive, "index.html").is_none() {
        return Err("Invalid mini app: missing index.html".into());
    }

    let icon = if manifest.icon.is_empty() {
        ["icon.png", "icon.jpg", "icon.svg"]
            .iter()
            .find_map(|n| read_entry(&mut archive, n, ICON_CAP).map(|b| (b, icon_mime(n))))
    } else {
        read_entry(&mut archive, &manifest.icon, ICON_CAP).map(|b| (b, icon_mime(&manifest.icon)))
    };

    Ok(Package { uses_realtime: uses_realtime(&mut archive), manifest, icon, file_hash })
}

/// Whether any script or page calls `joinRealtimeChannel`, so a channel can be
/// warmed before the app asks for it.
fn uses_realtime(archive: &mut Archive<'_>) -> bool {
    const NEEDLE: &[u8] = b"joinRealtimeChannel";
    for i in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(i) else { continue };
        let name = entry.name().to_ascii_lowercase();
        if !(name.ends_with(".html") || name.ends_with(".htm") || name.ends_with(".js") || name.ends_with(".mjs")) {
            continue;
        }
        let Ok(buf) = read_capped(&mut entry, 128 * 1024 * 1024) else { continue };
        if buf.windows(NEEDLE.len()).any(|w| w == NEEDLE) {
            return true;
        }
    }
    false
}

/// A package's storage origin label, as desktop derives it: a marketplace app
/// keeps one sandbox across versions, anything else gets one per file.
pub fn partition(file_hash: &str, marketplace_id: Option<&str>) -> String {
    match marketplace_id.filter(|id| vector_core::webxdc::is_safe_app_id(id)) {
        Some(id) => {
            let mut slug: String = id
                .to_ascii_lowercase()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            slug.truncate(24);
            let slug = slug.trim_matches('-');
            let tag = vector_core::simd::hex::bytes_to_hex_string(&vector_core::crypto::sha256::digest(id.as_bytes()));
            if slug.is_empty() { format!("app-{}", &tag[..8]) } else { format!("{}-{}", slug, &tag[..8]) }
        }
        None => file_hash[..32.min(file_hash.len())].to_ascii_lowercase(),
    }
}
