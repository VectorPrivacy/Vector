//! Pictures from disk, read the way Vector Cache keeps them: sealed under the install's key.

use std::path::Path;

use gpui::{ElementId, ImageSource, InteractiveElement, Resource, Stateful};

/// The asset prefix that routes a file through [`read`].
pub const SEALED: &str = "vcache:";

/// `gpui::img`, but a file on disk is read through Vector Cache, which GPUI's own loader can't
/// open, and with an id from its source: GPUI only steps a GIF's frames on an element with one.
pub fn img(source: impl Into<ImageSource>) -> Stateful<gpui::Img> {
    img_fit(source, BIGGEST)
}

/// [`img`] for a picture never shown wider or taller than `px` points: it decodes no larger
/// than that at 2x, in power-of-two steps so nearby sizes share one decode.
pub fn img_fit(source: impl Into<ImageSource>, px: f32) -> Stateful<gpui::Img> {
    fitted(source.into(), px, gpui::FIT)
}

/// [`img_fit`] for a face: cut to its centred square, the only part a circle or tile shows.
pub fn img_face(source: impl Into<ImageSource>, px: f32) -> Stateful<gpui::Img> {
    fitted(source.into(), px, gpui::SQUARE)
}

fn fitted(source: ImageSource, px: f32, how: &str) -> Stateful<gpui::Img> {
    let fit = ((px * 2.).ceil() as u32).clamp(64, BIGGEST as u32 * 2).next_power_of_two();
    let source = sealed(source, fit, how);
    let id = match &source {
        ImageSource::Render(image) => ElementId::NamedInteger("pic".into(), image.id.0 as u64),
        ImageSource::Resource(Resource::Embedded(name)) => ElementId::Name(format!("pic:{name}").into()),
        ImageSource::Resource(Resource::Uri(uri)) => ElementId::Name(format!("pic:{}", uri.as_ref()).into()),
        _ => ElementId::Name("pic".into()),
    };
    gpui::img(source).id(id)
}

/// The cap on any picture read from disk: past it a decode costs more than a screen can show.
const BIGGEST: f32 = 1024.;

/// `gpui::img` with no id, for a caller that names the element itself.
pub fn img_plain(source: impl Into<ImageSource>) -> gpui::Img {
    gpui::img(sealed(source.into(), BIGGEST as u32 * 2, gpui::FIT))
}

fn sealed(source: ImageSource, n: u32, fit: &str) -> ImageSource {
    match source {
        ImageSource::Resource(Resource::Path(p)) => {
            ImageSource::Resource(Resource::Embedded(format!("{SEALED}{}{fit}{n}", p.display()).into()))
        }
        // A path given as a string arrives as an asset name.
        ImageSource::Resource(Resource::Embedded(name)) if Path::new(name.as_ref()).is_absolute() => {
            ImageSource::Resource(Resource::Embedded(format!("{SEALED}{name}{fit}{n}").into()))
        }
        other => other,
    }
}

/// A file's plaintext: a sealed cache file opened, anything else as it is.
pub fn read(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
    vector_core::vcache::read(path.as_ref())
        .map(|b| b.to_vec())
        .map_err(std::io::Error::other)
}
