//! Compiled-in assets. `icons/` is the Tauri client's own icon folder, so an icon it adds or
//! redraws reaches this client on the next build; `ui/` holds what the Svelte markup inlines.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../src/icons"]
struct SharedIcons;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/assets/icons"]
struct UiIcons;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../src/twemoji/svg"]
struct Twemoji;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/assets/fonts"]
struct Fonts;

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let file = if let Some(p) = path.strip_prefix("icons/") {
            SharedIcons::get(p)
        } else if let Some(p) = path.strip_prefix("ui/") {
            UiIcons::get(p)
        } else if let Some(p) = path.strip_prefix("twemoji/") {
            Twemoji::get(p)
        } else {
            None
        };
        Ok(file.map(|f| f.data))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let names: Vec<SharedString> = if path.starts_with("icons") {
            SharedIcons::iter()
                .map(|p| format!("icons/{p}").into())
                .collect()
        } else {
            UiIcons::iter().map(|p| format!("ui/{p}").into()).collect()
        };
        Ok(names)
    }
}

pub fn fonts() -> Vec<Cow<'static, [u8]>> {
    // CoreText refuses anything that isn't a font, so the folder's README must not reach it.
    Fonts::iter()
        .filter(|name| name.ends_with(".ttf") || name.ends_with(".otf"))
        .filter_map(|name| Fonts::get(&name).map(|f| f.data))
        .collect()
}

pub fn twemoji_names() -> impl Iterator<Item = Cow<'static, str>> {
    Twemoji::iter()
}
