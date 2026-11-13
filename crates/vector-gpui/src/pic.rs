//! Pictures from disk, read the way Vector Cache keeps them: sealed under the install's key.

use std::path::Path;

use gpui::{ImageSource, Resource};

/// The asset prefix that routes a file through [`read`].
pub const SEALED: &str = "vcache:";

/// `gpui::img`, but a file on disk is read through Vector Cache, which GPUI's own loader can't open.
pub fn img(source: impl Into<ImageSource>) -> gpui::Img {
    gpui::img(match source.into() {
        ImageSource::Resource(Resource::Path(p)) => {
            ImageSource::Resource(Resource::Embedded(format!("{SEALED}{}", p.display()).into()))
        }
        // A path given as a string arrives as an asset name.
        ImageSource::Resource(Resource::Embedded(name)) if Path::new(name.as_ref()).is_absolute() => {
            ImageSource::Resource(Resource::Embedded(format!("{SEALED}{name}").into()))
        }
        other => other,
    })
}

/// A file's plaintext: a sealed cache file opened, anything else as it is.
pub fn read(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
    vector_core::vcache::read(path.as_ref())
        .map(|b| b.to_vec())
        .map_err(std::io::Error::other)
}
