//! Release checks: the same `latest.json` manifests the Tauri updater reads, fetched over the
//! shared lane. Previews follow `0.4.2-1 < 0.4.2-2 < 0.4.2`, so a preview rides onto its release.

use std::cmp::Ordering;

const STABLE_MANIFEST: &str =
    "https://github.com/VectorPrivacy/Vector/releases/latest/download/latest.json";
const PREVIEW_MANIFEST: &str =
    "https://github.com/VectorPrivacy/Vector/releases/download/preview/latest.json";
const MAX_MANIFEST: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    pub core: [u64; 3],
    /// The preview number; `None` is a release.
    pub preview: Option<u64>,
}

impl Version {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('v');
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p.parse().ok()?)),
            None => (s, None),
        };
        let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
        let core = [parts.next()??, parts.next()??, parts.next()??];
        if parts.next().is_some() {
            return None;
        }
        Some(Self { core, preview: pre })
    }

    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Self {
            core: [0, 0, 0],
            preview: None,
        })
    }

    /// `v0.4.2` or `v0.4.2 Preview 1`: a bare `-1` reads as though it were older.
    pub fn display(&self) -> String {
        let [a, b, c] = self.core;
        match self.preview {
            Some(n) => format!("v{a}.{b}.{c} Preview {n}"),
            None => format!("v{a}.{b}.{c}"),
        }
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core
            .cmp(&other.core)
            .then_with(|| match (self.preview, other.preview) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(&b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub struct Release {
    pub version: Version,
    pub notes: String,
    /// This build's bundle, when the release ships one and this copy can replace itself.
    pub asset: Option<super::install::Asset>,
}

/// A stable follower never takes a preview, whatever the manifest says.
pub fn newer(latest: Version, current: Version, preview_channel: bool) -> bool {
    (preview_channel || latest.preview.is_none()) && latest > current
}

/// Whether this device follows previews: its own choice, else the channel of the build.
pub fn beta() -> bool {
    match super::device::get("beta_updates").as_deref() {
        Some("true") => true,
        Some("false") => false,
        _ => Version::current().preview.is_some(),
    }
}

pub fn set_beta(on: bool) {
    super::device::set("beta_updates", if on { "true" } else { "false" });
}

/// The newer release on the followed channel, if there is one.
pub async fn check(preview_channel: bool) -> Result<Option<Release>, String> {
    let url = if preview_channel {
        PREVIEW_MANIFEST
    } else {
        STABLE_MANIFEST
    };
    let client = vector_core::net::shared_http_client();
    let mut resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Update server answered {}", resp.status()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        if body.len() + chunk.len() > MAX_MANIFEST {
            return Err("Update manifest too large".into());
        }
        body.extend_from_slice(&chunk);
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("Invalid update manifest: {e}"))?;
    let Some(version) = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .and_then(Version::parse)
    else {
        return Ok(None);
    };
    if !newer(version, Version::current(), preview_channel) {
        return Ok(None);
    }
    let notes = manifest
        .get("notes")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let asset = super::install::supported().then(|| super::install::asset(&manifest)).flatten();
    Ok(Some(Release { version, notes, asset }))
}

/// Where a release is installed from when this copy can't update itself.
pub fn release_page(version: Version) -> String {
    let [a, b, c] = version.core;
    match version.preview {
        Some(n) => format!("https://github.com/VectorPrivacy/Vector/releases/tag/v{a}.{b}.{c}-{n}"),
        None => format!("https://github.com/VectorPrivacy/Vector/releases/tag/v{a}.{b}.{c}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn previews_rank_below_their_release() {
        assert!(v("0.4.1") < v("0.4.2-1"));
        assert!(v("0.4.2-1") < v("0.4.2-2"));
        assert!(v("0.4.2-2") < v("0.4.2"));
        assert!(v("0.4.10") > v("0.4.9"));
    }

    #[test]
    fn stable_never_takes_a_preview() {
        assert!(!newer(v("0.5.2-1"), v("0.5.1"), false));
        assert!(newer(v("0.5.2-1"), v("0.5.1"), true));
        assert!(newer(v("0.5.2"), v("0.5.2-3"), false));
        assert!(!newer(v("0.5.1"), v("0.5.1"), true));
    }

    #[test]
    fn malformed_versions_never_parse() {
        for s in ["", "0.5", "0.5.1.2", "0.5.x", "0.5.1-rc"] {
            assert!(Version::parse(s).is_none(), "{s}");
        }
        assert_eq!(v("v0.5.1-2").display(), "v0.5.1 Preview 2");
    }
}
