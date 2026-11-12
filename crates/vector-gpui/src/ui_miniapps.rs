//! The composer's Mini Apps view (`AttachmentPanel` + `MiniAppsGrid`) and the launch dialog.

use std::path::PathBuf;

use gpui::{
    Focusable,
    AnyElement, Context, Entity, FontWeight, ObjectFit, SharedString, Window, div, img, prelude::*, px,
    svg,
};

use super::{VectorApp, backend};
use crate::composer::{CancelInput, InputChanged, TextInput};
use crate::theme::*;

/// An app in the grid, as history keeps it.
#[derive(Clone)]
pub(super) struct GridApp {
    pub name: String,
    pub src: String,
    pub icon: Option<PathBuf>,
    pub categories: String,
    pub last: i64,
}

impl GridApp {
    /// `isMiniAppGame`: untagged counts as a game.
    fn game(&self) -> bool {
        let cats: Vec<String> = self
            .categories
            .split(',')
            .map(|c| c.trim().to_lowercase())
            .filter(|c| !c.is_empty())
            .collect();
        cats.is_empty() || cats.iter().any(|c| c == "game") || !cats.iter().any(|c| c == "app")
    }
}

pub(super) struct MiniPanel {
    search: Entity<TextInput>,
    apps: Vec<GridApp>,
    loaded: bool,
    opened: std::time::Instant,
}

pub(super) struct Launch {
    app: GridApp,
}

const GAP: f32 = 12.;

impl VectorApp {
    /// `showAttachmentPanelMiniApps`.
    pub(super) fn show_miniapps(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let search = cx.new(|cx| {
            TextInput::new("Search", cx).with_look(crate::composer::Look {
                family: INTER,
                size: px(13.),
                line_height: px(16.),
                weight: FontWeight::NORMAL,
                color: white_a(0.9),
                placeholder: white_a(0.5),
                ..Default::default()
            })
        });
        cx.subscribe(&search, |_, _, _: &InputChanged, cx| cx.notify()).detach();
        cx.subscribe(&search, |app, _, _: &CancelInput, cx| {
            app.mini = None;
            cx.notify();
        })
        .detach();
        window.focus(&search.read(cx).focus_handle(cx), cx);
        self.mini = Some(MiniPanel {
            search,
            apps: Vec::new(),
            loaded: !self.live,
            opened: std::time::Instant::now(),
        });
        self.load_mini_history(cx);
        cx.notify();
    }

    fn load_mini_history(&mut self, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        let rx = backend::run(async {
            let history = vector_core::db::miniapps::get_miniapps_history(None).unwrap_or_default();
            let mut apps = Vec::with_capacity(history.len());
            for h in history {
                let icon = crate::miniapp::info(h.src_url.clone()).await.ok().and_then(|i| i.icon);
                apps.push(GridApp {
                    name: h.name,
                    src: h.src_url,
                    icon,
                    categories: h.categories,
                    last: h.last_opened_at,
                });
            }
            apps.sort_by(|a, b| b.last.cmp(&a.last));
            apps
        });
        cx.spawn(async move |this, cx| {
            let Ok(apps) = rx.await else { return };
            this.update(cx, |this, cx| {
                if let Some(m) = this.mini.as_mut() {
                    m.apps = apps;
                    m.loaded = true;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// `#attachment-panel-miniapps-view`: Back and search over the grid.
    pub(super) fn miniapps_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.mini.as_ref()?;
        let query = m.search.read(cx).text().trim().to_lowercase();
        let searching = !query.is_empty();
        let t = |i: usize| ((m.opened.elapsed().as_secs_f32() - i as f32 * 0.08) / 0.3).clamp(0., 1.);
        let tile = |i: usize, id: SharedString, face: AnyElement, label: SharedString, store: bool| {
            let it = t(i);
            let pivx = id.as_ref() == "mini-pivx";
            div()
                .id(id.clone())
                .relative()
                .left(px(-10. * (1. - it)))
                .opacity(it)
                .w(px(64.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.))
                .cursor_pointer()
                .group(id.clone())
                .child(
                    div()
                        .size(px(58.))
                        .rounded(px(12.))
                        .overflow_hidden()
                        .map(|d| match (store, pivx) {
                            (true, _) => d.bg(c(0x131313)),
                            (_, true) => d.bg(gpui::linear_gradient(
                                180.,
                                gpui::linear_color_stop(c(0x452568), 0.),
                                gpui::linear_color_stop(c(0x110b1b), 1.),
                            )),
                            _ => d.bg(white_a(0.1)),
                        })
                        .border_1()
                        .border_color(if store { c(0x59fcb3) } else { gpui::transparent_black() })
                        .group_hover(id, |s| s.bg(white_a(0.18)).border_color(c(PRIMARY)))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(face),
                )
                .child(
                    div()
                        .max_w(px(64.))
                        .text_size(px(11.))
                        .line_height(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(white_a(0.7))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(label),
                )
        };
        let glyph = |path: &'static str, size: f32| svg().path(path).size(px(size)).text_color(white_a(0.7)).into_any_element();
        let mut tiles: Vec<AnyElement> = Vec::new();
        if !searching {
            tiles.push(
                tile(0, "mini-nexus".into(), svg().path("icons/nexus.svg").size(px(26.)).text_color(c(PRIMARY)).into_any_element(), "App Store".into(), true)
                    .on_click(cx.listener(|app, _, _, cx| app.show_toast("The App Store isn't in this build yet", cx)))
                    .into_any_element(),
            );
        }
        let shown: Vec<GridApp> = m
            .apps
            .iter()
            .filter(|a| !searching || a.name.to_lowercase().contains(&query))
            .cloned()
            .collect();
        for (k, a) in shown.iter().enumerate() {
            let face = match &a.icon {
                Some(p) => img(p.clone()).size_full().rounded(px(12.)).object_fit(ObjectFit::Cover).into_any_element(),
                None => glyph("icons/play.svg", 22.),
            };
            let app = a.clone();
            tiles.push(
                tile(k + 1, SharedString::from(format!("mini-app-{k}")), face, a.name.clone().into(), false)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.launch = Some(Launch { app: app.clone() });
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        if !searching || "pivx".contains(&query) || "pivx wallet".contains(&query) {
            let i = tiles.len();
            tiles.push(
                tile(i, "mini-pivx".into(), img("icons/pivx.svg").size(px(38.)).into_any_element(), "PIVX".into(), false)
                    .on_click(cx.listener(|app, _, _, cx| app.show_toast("The PIVX Wallet isn't in this build yet", cx)))
                    .into_any_element(),
            );
        }
        if m.opened.elapsed().as_secs_f32() < 0.3 + tiles.len() as f32 * 0.08 {
            let me = cx.entity().downgrade();
            cx.defer(move |cx| {
                me.update(cx, |_, cx| cx.notify()).ok();
            });
        }
        let empty = m.loaded && m.apps.is_empty() && !searching;
        let none_found = searching && shown.is_empty();
        let topbar = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .w_full()
            .child(
                div()
                    .id("mini-back")
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(4.))
                    .pl(px(10.))
                    .pr(px(14.))
                    .py(px(6.))
                    .rounded(px(20.))
                    .bg(c(PRIMARY))
                    .cursor_pointer()
                    .on_click(cx.listener(|app, _, _, cx| {
                        app.mini = None;
                        cx.notify();
                    }))
                    .child(svg().path("icons/chevron-double-left.svg").size(px(20.)).text_color(c(0x000000)))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c(0x000000))
                            .child("Back"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(6.))
                    .rounded(px(20.))
                    .bg(white_a(0.08))
                    .border_1()
                    .border_color(white_a(0.1))
                    .child(svg().path("icons/search.svg").size(px(16.)).text_color(c(PRIMARY)))
                    .child(div().flex_1().child(m.search.clone())),
            );
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .w_full()
                .child(topbar)
                .child(
                    div()
                        .id("miniapps-grid")
                        .max_h(px(195.))
                        .overflow_y_scroll()
                        .pt(px(6.))
                        .pr(px(6.))
                        .pl(px(1.))
                        .pb(px(1.))
                        .flex()
                        .flex_wrap()
                        .gap(px(GAP))
                        .children(tiles)
                        .when(empty, |d| {
                            d.child(
                                div()
                                    .w_full()
                                    .p(px(16.))
                                    .flex()
                                    .justify_center()
                                    .text_size(px(12.))
                                    .text_color(white_a(0.5))
                                    .child("No recent Mini Apps"),
                            )
                        })
                        .when(none_found, |d| {
                            d.child(
                                div()
                                    .w_full()
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .gap(px(4.))
                                    .py(px(12.))
                                    .text_size(px(13.))
                                    .text_color(white_a(0.6))
                                    .child("No Mini Apps found")
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(white_a(0.4))
                                            .child("Try a different search, or check out the App Store!"),
                                    ),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    /// `LaunchDialog`: play and invite the open chat, or play alone.
    pub(super) fn launch_view(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let l = self.launch.as_ref()?;
        let verb = if l.app.game() { "Play" } else { "Open" };
        let initial = l
            .app
            .name
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".into());
        let me = cx.entity().downgrade();
        let close = {
            let me = me.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                me.update(cx, |a, cx| {
                    a.launch = None;
                    cx.notify();
                })
                .ok();
            }
        };
        let solo = {
            let me = me.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                me.update(cx, |a, cx| a.launch_choice(false, cx)).ok();
            }
        };
        let invite = move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
            me.update(cx, |a, cx| a.launch_choice(true, cx)).ok();
        };
        let icon = div()
            .size(px(48.))
            .flex_shrink_0()
            .rounded(px(11.))
            .overflow_hidden()
            .bg(gpui::linear_gradient(
                150.,
                gpui::linear_color_stop(c(0x262626), 0.),
                gpui::linear_color_stop(c(0x121212), 1.),
            ))
            .border_1()
            .border_color(white_a(0.07))
            .flex()
            .items_center()
            .justify_center()
            .child(match &l.app.icon {
                Some(p) => img(p.clone()).size_full().object_fit(ObjectFit::Cover).into_any_element(),
                None => div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(c(0x707070))
                    .child(initial)
                    .into_any_element(),
            });
        let head = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(icon)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(18.))
                            .line_height(px(22.))
                            .font_weight(FontWeight::BOLD)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(l.app.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .line_height(px(19.5))
                            .text_color(white_a(0.55))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child("Play here, or bring this chat along"),
                    ),
            )
            .child(
                div()
                    .id("ma-launch-x")
                    .group("ma-launch-x")
                    .self_start()
                    .w(px(23.7))
                    .h(px(22.))
                    .flex()
                    .justify_center()
                    .pt(px(6.))
                    .cursor_pointer()
                    .on_click(close)
                    .child(
                        svg()
                            .path("icons/x-mark.svg")
                            .size(px(9.))
                            .text_color(white_a(0.4))
                            .group_hover("ma-launch-x", |d| d.text_color(c(0xffffff))),
                    ),
            );
        let card = crate::security::card("ma-launch")
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(head)
            .child(
                crate::security::actions()
                    .child(crate::security::button("ma-invite", format!("{verb} & Invite"), false, true, invite))
                    .child(crate::security::button("ma-solo", format!("{verb} Alone"), true, true, solo)),
            );
        let backdrop = cx.listener(|a, _: &gpui::MouseDownEvent, _, cx| {
            a.launch = None;
            cx.notify();
        });
        Some(
            gpui::deferred(
                div()
                    .id("miniapp-launch-overlay")
                    .absolute()
                    .inset_0()
                    .p(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(ca(0, 0, 0, 0.88))
                    .occlude()
                    .on_mouse_down(gpui::MouseButton::Left, backdrop)
                    .child(card),
            )
            .with_priority(9)
            .into_any_element(),
        )
    }

    /// `playMiniAppSolo` / `playMiniAppAndInvite`.
    fn launch_choice(&mut self, invite: bool, cx: &mut Context<Self>) {
        let Some(Launch { app }) = self.launch.take() else {
            return;
        };
        self.mini = None;
        self.attach = None;
        cx.notify();
        let chat = self.open.as_ref().map(|s| s.to_string());
        match chat.filter(|_| invite) {
            Some(chat) => self.invite_miniapp(app, chat, cx),
            None => self.play_solo(app.src, cx),
        }
    }

    fn play_solo(&mut self, src: String, cx: &mut Context<Self>) {
        let id = format!("solo_{}", now_ms());
        let rx = backend::run(crate::miniapp::open(src, "solo".into(), id, None, None));
        cx.spawn(async move |this, cx| {
            if let Ok(Err(e)) = rx.await {
                this.update(cx, |this, cx| this.show_toast(&e, cx)).ok();
            }
        })
        .detach();
    }

    /// `inviteMiniAppToChat`: send the package, then open it from the message the send surfaces.
    fn invite_miniapp(&mut self, app: GridApp, chat: String, cx: &mut Context<Self>) {
        let Some(vector_core::media::MediaRef::File(path)) = vector_core::media::MediaRef::parse(&app.src)
        else {
            self.play_solo(app.src, cx);
            return;
        };
        let prior = self.newest_own_xdc(&chat).map(|(id, _, _)| id);
        let receiver = chat.clone();
        let send = backend::run(backend::messaging::send_file(
            receiver,
            String::new(),
            PathBuf::from(path),
            String::new(),
            false,
            false,
            None,
        ));
        let chat2 = chat.clone();
        cx.spawn(async move |this, cx| {
            // The pending bubble lands well before the upload ends; play from it once it does.
            for _ in 0..50 {
                cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                let found = this
                    .update(cx, |this, _| this.newest_own_xdc(&chat2).filter(|(id, _, _)| Some(id) != prior.as_ref()))
                    .ok()
                    .flatten();
                if let Some((msg, file, topic)) = found {
                    let rx = backend::run(crate::miniapp::open(file, chat2.clone(), msg, None, topic));
                    if let Ok(Err(e)) = rx.await {
                        this.update(cx, |this, cx| this.show_toast(&e, cx)).ok();
                    }
                    return;
                }
            }
            this.update(cx, |this, cx| this.play_solo(app.src.clone(), cx)).ok();
        })
        .detach();
        cx.spawn(async move |this, cx| {
            if let Ok(Err(e)) = send.await {
                this.update(cx, |this, cx| this.notice("Upload Failed", e, cx)).ok();
            }
        })
        .detach();
    }

    /// The newest message of ours in `chat` carrying a Mini App: (id, package, topic).
    fn newest_own_xdc(&self, chat: &str) -> Option<(String, String, Option<String>)> {
        let store = self.store.borrow();
        let c = store.chat(chat)?;
        c.messages.iter().rev().filter(|m| m.mine).find_map(|m| {
            let att = m.attachments.iter().find(|a| a.extension.eq_ignore_ascii_case("xdc"))?;
            let file = if !att.path.is_empty() {
                let p = PathBuf::from(&att.path);
                if p.is_absolute() { p } else { store.media_root.join(p) }
                    .to_string_lossy()
                    .into_owned()
            } else {
                vector_core::media::MediaRef::held(&vector_core::simd::hex::hex_to_bytes_32_checked(&att.id)?)
            };
            Some((m.id.clone(), file, att.webxdc_topic.clone()))
        })
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}
