//! Reading a `.xdc` package's `manifest.toml`, and nothing else from it.

use std::io::{Cursor, Read};

use serde::Deserialize;

/// More than any real manifest; bounds what one read inflates.
const MANIFEST_CAP: u64 = 64 * 1024;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Manifest {
    /// Empty when the package names none; the file name stands in (see [`Manifest::or_named`]).
    #[serde(default)]
    pub name: String,
    /// A stable app id, when the author set one (`id = "3d-tic-tac-toe"`).
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub source_code_url: Option<String>,
}

impl Manifest {
    /// The manifest with `file_name` standing in for a name the package doesn't
    /// set, as the WebXDC spec has it. Kept apart from reading, since the same
    /// bytes can be shared under any file name.
    pub fn or_named(mut self, file_name: &str) -> Self {
        if self.name.trim().is_empty() {
            self.name = file_name.trim_end_matches(".xdc").to_string();
        }
        self
    }
}

/// What the package's own bytes say: its `manifest.toml`, or an empty one when
/// it has none. Reads one capped entry; the rest of the archive is never
/// inflated. CPU-bound: call from a blocking thread.
pub fn read_manifest(bytes: &[u8]) -> Result<Manifest, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("Invalid mini app: {e}"))?;
    let stored = archive
        .file_names()
        .find(|n| n.eq_ignore_ascii_case("manifest.toml"))
        .map(str::to_string);
    let Some(stored) = stored else {
        return Ok(Manifest::default());
    };
    let mut entry = archive.by_name(&stored).map_err(|e| format!("Invalid mini app: {e}"))?;
    // Entry headers can't be trusted; bound the bytes actually inflated.
    let mut raw = Vec::new();
    (&mut entry).take(MANIFEST_CAP + 1).read_to_end(&mut raw).map_err(|e| e.to_string())?;
    if raw.len() as u64 > MANIFEST_CAP {
        return Err("Invalid manifest: too large".into());
    }
    let text = String::from_utf8(raw).map_err(|e| format!("Invalid manifest: {e}"))?;
    toml::from_str(&text).map_err(|e| format!("Invalid manifest: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut out);
            let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            for (name, body) in files {
                z.start_file(*name, opts).unwrap();
                z.write_all(body).unwrap();
            }
            z.finish().unwrap();
        }
        out.into_inner()
    }

    #[test]
    fn it_reads_the_manifest_id_and_name() {
        let pkg = zip_of(&[("index.html", b"<html>"), ("manifest.toml", b"name = \"Chess\"\nid = \"chess\"\n")]);
        let m = read_manifest(&pkg).unwrap().or_named("fallback.xdc");
        assert_eq!((m.name.as_str(), m.id.as_deref()), ("Chess", Some("chess")));
    }

    #[test]
    fn a_package_without_a_name_takes_the_file_name() {
        let bare = read_manifest(&zip_of(&[("index.html", b"<html>")])).unwrap();
        assert_eq!((bare.name.as_str(), bare.id.as_deref()), ("", None), "the bytes alone name nothing");
        assert_eq!(bare.clone().or_named("chess.xdc").name, "chess");
        assert_eq!(bare.or_named("notchess.xdc").name, "notchess", "one package, two names");
        let unnamed = read_manifest(&zip_of(&[("manifest.toml", b"id = \"chess\"\n")])).unwrap().or_named("c.xdc");
        assert_eq!((unnamed.name.as_str(), unnamed.id.as_deref()), ("c", Some("chess")));
    }

    #[test]
    fn an_inflating_manifest_is_refused_without_reading_it_all() {
        let bomb = vec![b'#'; 8 * 1024 * 1024];
        let pkg = zip_of(&[("manifest.toml", &bomb)]);
        assert!(pkg.len() < 64 * 1024, "the bomb is small on the wire");
        assert!(read_manifest(&pkg).is_err());
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(read_manifest(b"not a zip").is_err());
    }
}
