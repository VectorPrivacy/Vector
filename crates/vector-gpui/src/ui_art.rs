//! Community logos and banners, picked in Community Settings or Create Community: chosen,
//! cropped, then saved.

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{Context, Window};

use super::VectorApp;
use crate::backend;
use crate::commsettings::Pick;
use crate::cropper::{Answer, Cropper};

/// The banner template, 238 x 70 under the community's header.
const BANNER_ASPECT: f32 = 238. / 70.;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Art {
    Icon,
    Banner,
    /// The icon of a Community not made yet.
    NewIcon,
}

impl VectorApp {
    /// Stage a new logo (cropped square) or banner (cropped to the template unless it fits).
    pub(super) fn pick_comm_image(
        &mut self,
        banner: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .comm_settings
            .as_ref()
            .is_none_or(|m| m.saving || !m.can("manage_metadata"))
        {
            return;
        }
        self.choose_art(if banner { Art::Banner } else { Art::Icon }, window, cx);
    }

    pub(super) fn pick_new_community_icon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.create_community.as_ref().is_some_and(|m| !m.busy) {
            self.choose_art(Art::NewIcon, window, cx);
        }
    }

    fn choose_art(&mut self, art: Art, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(if art == Art::NewIcon { "Choose Community Icon" } else { "Choose Image" }.into()),
        });
        let window = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let Some(ext) = backend::upload::picture_ext(&path) else {
                this.update(cx, |this, cx| {
                    this.show_toast("That image could not be read", cx)
                })
                .ok();
                return;
            };
            let file = path.clone();
            let decoded = cx
                .background_executor()
                .spawn(async move {
                    std::fs::read(&file)
                        .ok()
                        .and_then(|b| crate::media::decode(&b, &ext, (2048, 2048)))
                })
                .await;
            let Some((image, w, h)) = decoded else {
                this.update(cx, |this, cx| {
                    this.show_toast("That image could not be read", cx)
                })
                .ok();
                return;
            };
            let pick = Pick {
                path,
                crop: None,
                preview: Arc::new(image),
                natural: (w, h),
                cut: None,
            };
            window
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| {
                        this.crop_art(pick, art, window, cx)
                    })
                    .ok();
                })
                .ok();
        })
        .detach();
    }

    /// Fixtures: a file's icon for the Community being created, through the cropper.
    pub(super) fn fixture_new_icon(&mut self, path: std::path::PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ext) = backend::upload::picture_ext(&path) else { return };
        let Some((image, w, h)) = std::fs::read(&path)
            .ok()
            .and_then(|b| crate::media::decode(&b, &ext, (2048, 2048)))
        else {
            return;
        };
        let pick = Pick { path, crop: None, preview: Arc::new(image), natural: (w, h), cut: None };
        self.crop_art(pick, Art::NewIcon, window, cx);
    }

    fn crop_art(&mut self, pick: Pick, art: Art, window: &mut Window, cx: &mut Context<Self>) {
        let banner = art == Art::Banner;
        let (w, h) = pick.natural;
        if banner && ((w as f32 / h.max(1) as f32) - BANNER_ASPECT).abs() <= 0.01 {
            if let Some(m) = self.comm_settings.as_mut() {
                m.banner_pick = Some(pick);
                m.banner_removed = false;
            }
            return cx.notify();
        }
        let image = pick.preview.clone();
        let cropper = cx.new(|cx| {
            let c = Cropper::new(
                image,
                (w, h),
                match art {
                    Art::Banner => "Crop Banner",
                    Art::Icon => "Crop Icon",
                    Art::NewIcon => "Crop Community Icon",
                },
                "Drag to move, or drag a corner to resize.",
                false,
                window,
                cx,
            );
            if banner { c.aspect(BANNER_ASPECT) } else { c }
        });
        cx.subscribe_in(&cropper, window, |this, _, answer: &Answer, _, cx| {
            let Some((_, art, mut pick)) = this.comm_crop.take() else {
                return;
            };
            cx.notify();
            let Answer::Crop(crop) = answer else { return };
            pick.set_crop(crop.clone());
            if art == Art::NewIcon {
                if let Some(m) = this.create_community.as_mut() {
                    m.icon = Some(pick);
                }
                return;
            }
            let Some(m) = this.comm_settings.as_mut() else {
                return;
            };
            if art == Art::Banner {
                m.banner_pick = Some(pick);
                m.banner_removed = false;
            } else {
                m.icon_pick = Some(pick);
                m.icon_removed = false;
            }
        })
        .detach();
        self.comm_crop = Some((cropper, art, pick));
        cx.notify();
    }
}
