//! Publish to App Store, and Edit Listing (`PublishDialog.svelte`, `showPublishAppDialog`,
//! `showEditListingDialog`).

use std::path::PathBuf;

use gpui::{AnyElement, Context, Entity, Focusable, FontWeight, SharedString, Task, Window, div, prelude::*, px, svg};

use super::{VectorApp, backend};
use crate::composer::{InputChanged, TextInput};
use crate::theme::*;

pub(super) struct PublishForm {
    /// The package a new release uploads; None edits the listing it already has.
    file: Option<String>,
    icon: Option<PathBuf>,
    id: Entity<TextInput>,
    name: Entity<TextInput>,
    description: Entity<TextInput>,
    version: Entity<TextInput>,
    categories: Entity<TextInput>,
    developer: Entity<TextInput>,
    source: Entity<TextInput>,
    changelog: Entity<TextInput>,
    game: bool,
    touch: bool,
    pad: bool,
    keys: bool,
    perms: Vec<(String, String, String, bool)>,
    hint: (SharedString, bool),
    busy: bool,
    prefill: Option<Task<()>>,
}

const ID_HINT: &str = "Unique identifier (lowercase, no spaces)";

/// `splitControlTags`: the free tags, and the controls as switches (an `-only` tag stays free).
fn split_controls(tags: &[String]) -> (Vec<String>, bool, bool, bool) {
    use crate::nexus::{KEYS, ONLY, PAD, TOUCH};
    let plain = |set: &[&str]| tags.iter().any(|t| set.contains(&t.as_str()) && !ONLY.contains(&t.as_str()));
    let rest = tags
        .iter()
        .filter(|t| {
            let t = t.as_str();
            ONLY.contains(&t) || !(TOUCH.contains(&t) || PAD.contains(&t) || KEYS.contains(&t))
        })
        .cloned()
        .collect();
    (rest, plain(&TOUCH), plain(&PAD), plain(&KEYS))
}

impl PublishForm {
    pub fn busy(&self) -> bool {
        self.busy
    }

    fn field(cx: &mut Context<VectorApp>, placeholder: &str, text: &str, multiline: bool) -> Entity<TextInput> {
        let input = cx.new(|cx| {
            let mut t = TextInput::new(placeholder.to_string(), cx).with_look(crate::composer::Look {
                family: INTER,
                size: px(15.),
                line_height: px(if multiline { 21.75 } else { 19. }),
                weight: FontWeight::NORMAL,
                color: c(0xffffff),
                placeholder: white_a(0.4),
                ..Default::default()
            });
            if multiline {
                t = t.multiline(px(240.));
            }
            t.set_text(text, cx);
            t
        });
        cx.subscribe(&input, |_, _, _: &InputChanged, cx| cx.notify()).detach();
        input
    }

    fn texts(&self, cx: &gpui::App) -> [String; 8] {
        [
            &self.id,
            &self.name,
            &self.description,
            &self.version,
            &self.categories,
            &self.developer,
            &self.source,
            &self.changelog,
        ]
        .map(|e| e.read(cx).text().trim().to_string())
    }

    /// `[game|app, ...free tags, ...controls]`, lowercased and once each.
    fn tags(&self, cx: &gpui::App) -> Vec<String> {
        let mut tags = vec![if self.game { "game" } else { "app" }.to_string()];
        let free = self.categories.read(cx).text().to_lowercase();
        let controls = [(self.touch, "mobile"), (self.pad, "controller"), (self.keys, "keyboard")];
        for t in free
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(String::from)
            .chain(controls.iter().filter(|c| c.0).map(|c| c.1.to_string()))
        {
            if !tags.contains(&t) {
                tags.push(t);
            }
        }
        tags
    }

    fn fill_from(&mut self, app: &backend::market::MarketplaceApp, cx: &mut Context<VectorApp>) {
        let set = |e: &Entity<TextInput>, t: &str, cx: &mut Context<VectorApp>| e.update(cx, |i, cx| i.set_text(t, cx));
        if !app.description.is_empty() {
            set(&self.description, &app.description, cx);
        }
        if let Some(d) = &app.developer {
            set(&self.developer, d, cx);
        }
        if let Some(s) = &app.source_url {
            set(&self.source, s, cx);
        }
        if !app.categories.is_empty() {
            let free: Vec<String> = app.categories.iter().filter(|c| *c != "game" && *c != "app").cloned().collect();
            let (rest, touch, pad, keys) = split_controls(&free);
            set(&self.categories, &rest.join(", "), cx);
            (self.touch, self.pad, self.keys) = (touch, pad, keys);
            self.game = app.categories.iter().any(|c| c == "game");
        }
        if !app.version.is_empty() {
            set(&self.version, &app.version, cx);
        }
        if let Some(log) = &app.changelog {
            set(&self.changelog, log, cx);
        }
        let asked: Vec<&str> = app.requested_permissions.split(',').map(str::trim).collect();
        for p in &mut self.perms {
            p.3 = asked.contains(&p.0.as_str());
        }
    }
}

fn all_perms() -> Vec<(String, String, String, bool)> {
    vector_core::webxdc_permissions::get_all_permission_info()
        .into_iter()
        .map(|p| (p.id, p.label, p.description, false))
        .collect()
}

impl VectorApp {
    /// The file preview's Publish, for the trusted publisher holding a Mini App.
    pub(super) fn can_publish(&self) -> bool {
        !self.live || self.store.borrow().my_npub == backend::market::trusted_publisher()
    }

    /// `showPublishAppDialog`: the package's own listing fills the form.
    pub(super) fn open_publish(&mut self, file: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let path = file.to_string_lossy().into_owned();
        let rx = backend::run(crate::miniapp::info(path.clone()));
        cx.spawn_in(window, async move |this, cx| {
            let info = rx.await.ok().and_then(Result::ok);
            this.update_in(cx, |this, window, cx| {
                let name = info.as_ref().map_or("", |i| i.name.as_str()).to_string();
                let id: String = name
                    .to_lowercase()
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("-");
                let id = if id.is_empty() { "app".to_string() } else { id };
                let desc = info.as_ref().map_or("", |i| i.description.as_str()).to_string();
                let version = info.as_ref().map(|i| i.version.clone()).filter(|v| !v.is_empty()).unwrap_or("1.0.0".into());
                let source = info.as_ref().and_then(|i| i.source.clone()).unwrap_or_default();
                let form = PublishForm {
                    file: Some(path),
                    icon: info.as_ref().and_then(|i| i.icon.clone()),
                    id: PublishForm::field(cx, "my-awesome-game", &id, false),
                    name: PublishForm::field(cx, "My Awesome Game", &name, false),
                    description: PublishForm::field(cx, "A line or two about what it is", &desc, true),
                    version: PublishForm::field(cx, "1.0.0", &version, false),
                    categories: PublishForm::field(cx, "multiplayer, arcade, 2d", "", false),
                    developer: PublishForm::field(cx, "Who made it", "", false),
                    source: PublishForm::field(cx, "https://github.com/...", &source, false),
                    changelog: PublishForm::field(cx, "What changed in this version", "", true),
                    game: true,
                    touch: false,
                    pad: false,
                    keys: false,
                    perms: all_perms(),
                    hint: (ID_HINT.into(), false),
                    busy: false,
                    prefill: None,
                };
                cx.subscribe_in(&form.id, window, |this, _, _: &InputChanged, _, cx| {
                    let Some(f) = this.publish.as_mut() else { return };
                    f.hint = (ID_HINT.into(), false);
                    f.prefill = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
                        this.update(cx, |this, cx| this.publish_prefill(cx)).ok();
                    }));
                })
                .detach();
                window.focus(&form.name.read(cx).focus_handle(cx), cx);
                this.publish = Some(form);
                this.publish_prefill(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `prefillFromExisting`: an App ID this account already published fills the rest from it.
    fn publish_prefill(&mut self, cx: &mut Context<Self>) {
        let me = self.store.borrow().my_npub.clone();
        let Some(f) = self.publish.as_ref() else { return };
        let id = f.id.read(cx).text().trim().to_lowercase();
        if id.is_empty() || f.file.is_none() {
            return;
        }
        let apps = match self.nexus.as_ref() {
            Some(n) => n.apps.clone(),
            None if self.live => {
                let rx = backend::run(backend::market::cached());
                cx.spawn(async move |this, cx| {
                    let Ok(apps) = rx.await else { return };
                    this.update(cx, |this, cx| this.publish_prefill_from(&apps, &id, &me, cx)).ok();
                })
                .detach();
                return;
            }
            None => self.store.borrow().fixture_market.clone(),
        };
        self.publish_prefill_from(&apps, &id, &me, cx);
    }

    fn publish_prefill_from(&mut self, apps: &[backend::market::MarketplaceApp], id: &str, me: &str, cx: &mut Context<Self>) {
        let Some(app) = apps.iter().find(|a| a.id == id && a.publisher == me) else { return };
        let Some(f) = self.publish.as_mut() else { return };
        f.fill_from(app, cx);
        f.hint = ("✓ Pre-filled from your existing app. Update the version and changelog!".into(), true);
        cx.notify();
    }

    /// `showEditListingDialog`: everything but the App ID and version, which name the package.
    pub(super) fn open_edit_listing(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(n) = self.nexus.as_ref() else { return };
        let Some(app) = n.app(&id).cloned() else { return };
        let icon = n.icon_path(&app);
        let mut form = PublishForm {
            file: None,
            icon,
            id: PublishForm::field(cx, "my-awesome-game", &app.id, false),
            name: PublishForm::field(cx, "My Awesome Game", &app.name, false),
            description: PublishForm::field(cx, "A line or two about what it is", "", true),
            version: PublishForm::field(cx, "1.0.0", "", false),
            categories: PublishForm::field(cx, "multiplayer, arcade, 2d", "", false),
            developer: PublishForm::field(cx, "Who made it", "", false),
            source: PublishForm::field(cx, "https://github.com/...", "", false),
            changelog: PublishForm::field(cx, "What changed in this version", "", true),
            game: crate::nexus::is_game(&app),
            touch: false,
            pad: false,
            keys: false,
            perms: all_perms(),
            hint: (SharedString::default(), false),
            busy: false,
            prefill: None,
        };
        form.fill_from(&app, cx);
        form.game = crate::nexus::is_game(&app);
        window.focus(&form.name.read(cx).focus_handle(cx), cx);
        self.publish = Some(form);
        cx.notify();
    }

    fn publish_cancel(&mut self, cx: &mut Context<Self>) {
        if self.publish.as_ref().is_some_and(|f| !f.busy) {
            self.publish = None;
            cx.notify();
        }
    }

    fn publish_submit(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.publish.as_mut() else { return };
        if f.busy {
            return;
        }
        let [id, name, description, version, _, developer, source, changelog] = f.texts(cx);
        let editing = f.file.is_none();
        if !editing && id.is_empty() {
            return self.show_toast("App ID is required", cx);
        }
        if name.is_empty() {
            return self.show_toast(if editing { "A listing needs a name" } else { "Name is required" }, cx);
        }
        let permissions: Vec<&str> = f.perms.iter().filter(|p| p.3).map(|p| p.0.as_str()).collect();
        let listing = backend::market::Listing {
            id,
            name: name.clone(),
            description,
            version,
            categories: f.tags(cx),
            changelog: Some(changelog).filter(|s| !s.is_empty()),
            developer: Some(developer).filter(|s| !s.is_empty()),
            source: Some(source).filter(|s| !s.is_empty()),
            permissions: Some(permissions.join(",")).filter(|s| !s.is_empty()),
        };
        f.busy = true;
        cx.notify();
        if !self.live {
            return;
        }
        let file = f.file.clone();
        cx.spawn(async move |this, cx| {
            let res = match file {
                Some(file) => backend::run(backend::market::publish(file, listing)).await.map(|r| r.map(|_| None)),
                None => backend::run(backend::market::edit_listing(listing)).await.map(|r| r.map(Some)),
            }
            .unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                match res {
                    Ok(apps) => {
                        this.publish = None;
                        match apps {
                            Some(apps) => {
                                if let Some(n) = this.nexus.as_mut() {
                                    n.apps = apps;
                                }
                                this.show_toast("Listing Updated", cx);
                            }
                            None => {
                                let mut p = crate::kit::Popup::notice("Published!");
                                p.icon = Some("icons/vector-check.svg");
                                p.text = Some(format!("{name} has been published to the App Store.").into());
                                this.popup = Some((p, super::PopupAct::Notice));
                            }
                        }
                    }
                    Err(e) => {
                        if let Some(f) = this.publish.as_mut() {
                            f.busy = false;
                        }
                        let msg = if editing { e } else { format!("Failed to publish: {e}") };
                        this.show_toast(&msg, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn publish_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let f = self.publish.as_ref()?;
        let editing = f.file.is_none();
        let me = cx.entity().downgrade();
        let act = |run: fn(&mut VectorApp, &mut Context<VectorApp>)| {
            let me = me.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                me.update(cx, |a, cx| run(a, cx)).ok();
            }
        };
        let flip = |pick: fn(&mut PublishForm) -> &mut bool| {
            let me = me.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                me.update(cx, |a, cx| {
                    if let Some(f) = a.publish.as_mut() {
                        let v = pick(f);
                        *v = !*v;
                    }
                    cx.notify();
                })
                .ok();
            }
        };

        let label = |text: &'static str, optional: bool| {
            div()
                .flex()
                .gap(px(4.))
                .text_size(px(11.))
                .line_height(px(24.))
                .font_weight(FontWeight::BOLD)
                .text_color(white_a(0.45))
                .child(text.to_uppercase())
                .when(optional, |d| d.child(div().font_weight(FontWeight::MEDIUM).text_color(white_a(0.35)).child("(optional)")))
        };
        let hint = |text: SharedString, accent: bool| {
            div()
                .text_size(px(12.))
                .line_height(px(16.8))
                .text_color(if accent { c(ACCENT) } else { white_a(0.45) })
                .child(text)
        };
        let boxed = |input: &Entity<TextInput>, multiline: bool| {
            let focused = input.read(cx).focus_handle(cx).is_focused(window);
            div()
                .w_full()
                .px(px(12.))
                .py(px(10.))
                .when(multiline, |d| d.min_h(px(87.25)))
                .rounded(px(12.))
                .border_1()
                .border_color(if focused { c(ACCENT) } else { c(0x2c2e2f) })
                .bg(c(0x161718))
                .child(input.clone())
        };
        let fixed = |input: &Entity<TextInput>| {
            div()
                .w_full()
                .px(px(12.))
                .py(px(10.))
                .rounded(px(12.))
                .border_1()
                .border_color(c(0x202122))
                .bg(c(0x161718))
                .text_size(px(15.))
                .line_height(px(19.))
                .text_color(white_a(0.45))
                .child(input.read(cx).text().to_string())
        };
        let field = || div().flex().flex_col().gap(px(6.)).min_w_0();

        let initial = f
            .name
            .read(cx)
            .text()
            .chars()
            .find(|c| c.is_alphanumeric())
            .map_or("?".to_string(), |c| c.to_uppercase().collect());
        let icon = div()
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .size(px(48.))
            .rounded(px(11.))
            .overflow_hidden()
            .bg(gpui::linear_gradient(
                150.,
                gpui::linear_color_stop(c(0x262626), 0.),
                gpui::linear_color_stop(c(0x121212), 1.),
            ))
            .border_1()
            .border_color(white_a(0.07))
            .child(match &f.icon {
                Some(p) => crate::pic::img(p.clone())
                    .size_full()
                    .rounded(px(11.))
                    .object_fit(gpui::ObjectFit::Cover)
                    .into_any_element(),
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
                            .truncate()
                            .text_size(px(18.))
                            .line_height(px(22.))
                            .font_weight(FontWeight::BOLD)
                            .child(if editing { "Edit Listing" } else { "Publish to App Store" }),
                    )
                    .child(div().truncate().text_size(px(13.)).line_height(px(19.5)).text_color(white_a(0.55)).child(
                        if editing {
                            "Changes go live without uploading the app again"
                        } else {
                            "Everyone on Vector can find and install it"
                        },
                    )),
            )
            .child(
                div()
                    .id("ma-publish-x")
                    .group("ma-publish-x")
                    .self_start()
                    .w(px(23.7))
                    .h(px(22.))
                    .flex()
                    .justify_center()
                    .pt(px(6.))
                    .when(!f.busy, |d| d.cursor_pointer().on_click(act(Self::publish_cancel)))
                    .child(
                        svg()
                            .path("ui/x-mark.svg")
                            .size(px(9.))
                            .text_color(white_a(0.4))
                            .group_hover("ma-publish-x", |d| d.text_color(c(0xffffff))),
                    ),
            );

        let kind_btn = |id: &'static str, text: &'static str, on: bool, set: bool| {
            let me = me.clone();
            div()
                .id(id)
                .h_full()
                .px(px(16.))
                .flex()
                .items_center()
                .rounded(px(9.))
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .when(on, |d| d.bg(c(0x2a2b2c)))
                .text_color(if on { c(0xffffff) } else { white_a(0.6) })
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    me.update(cx, |a, cx| {
                        if let Some(f) = a.publish.as_mut() {
                            f.game = set;
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .child(text)
        };
        let control = |id: &'static str, glyph: &'static str, text: &'static str, on: bool, click| {
            div()
                .id(id)
                .h(px(36.))
                .px(px(14.))
                .flex()
                .items_center()
                .gap(px(7.))
                .rounded_full()
                .border_1()
                .border_color(if on { gpui::Hsla::from(c(ACCENT)).opacity(0.45) } else { c(0x2c2e2f).into() })
                .bg(if on { gpui::Hsla::from(c(ACCENT)).opacity(0.12) } else { c(0x161718).into() })
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if on { c(ACCENT).into() } else { white_a(0.6) })
                .cursor_pointer()
                .on_click(click)
                .child(svg().path(format!("icons/{glyph}.svg")).size(px(16.)).text_color(if on { c(ACCENT).into() } else { white_a(0.6) }))
                .child(text)
        };

        let vh = f32::from(window.viewport_size().height);
        let mut perms = div()
            .id("ma-pub-perms")
            .flex()
            .flex_col()
            .max_h(px(0.46 * vh))
            .overflow_y_scroll()
            .rounded(px(14.))
            .bg(c(0x0c0c0c))
            .border_1()
            .border_color(c(0x1f1f1f));
        for (i, (id, title, sub, on)) in f.perms.iter().enumerate() {
            let me = me.clone();
            let click = move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                me.update(cx, |a, cx| {
                    if let Some(p) = a.publish.as_mut().and_then(|f| f.perms.get_mut(i)) {
                        p.3 = !p.3;
                    }
                    cx.notify();
                })
                .ok();
            };
            perms = perms.child(
                div()
                    .id(SharedString::from(format!("ma-pub-perm-{id}")))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(14.))
                    .py(px(12.))
                    .cursor_pointer()
                    .when(i > 0, |d| d.border_t_1().border_color(c(0x1f1f1f)))
                    .on_click(click.clone())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(px(15.)).line_height(px(22.5)).font_weight(FontWeight::MEDIUM).child(title.clone()))
                            .child(div().text_size(px(12.5)).line_height(px(17.5)).text_color(white_a(0.55)).child(sub.clone())),
                    )
                    .child(crate::settings::toggle(SharedString::from(format!("ma-pub-t-{id}")), *on, true, click)),
            );
        }

        let form = div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(
                field()
                    .child(label("App ID", false))
                    .child(if editing { fixed(&f.id) } else { boxed(&f.id, false) })
                    .when(!editing, |d| d.child(hint(f.hint.0.clone(), f.hint.1))),
            )
            .child(field().child(label("Name", false)).child(boxed(&f.name, false)))
            .child(field().child(label("Description", false)).child(boxed(&f.description, true)))
            .child(
                div()
                    .flex()
                    .gap(px(12.))
                    .child(
                        field()
                            .flex_1()
                            .child(label("Version", false))
                            .child(if editing { fixed(&f.version) } else { boxed(&f.version, false) }),
                    )
                    .child(
                        field().child(label("Kind", false)).child(
                            div()
                                .flex()
                                .gap(px(3.))
                                .h(px(43.))
                                .p(px(3.))
                                .rounded(px(12.))
                                .border_1()
                                .border_color(c(0x2c2e2f))
                                .bg(c(0x161718))
                                .child(kind_btn("ma-kind-game", "Game", f.game, true))
                                .child(kind_btn("ma-kind-app", "App", !f.game, false)),
                        ),
                    ),
            )
            .child(
                field()
                    .child(label("Plays With", false))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(8.))
                            .child(control("ma-c-touch", "touch", "Touch", f.touch, flip(|f| &mut f.touch)))
                            .child(control("ma-c-pad", "gamepad", "Controller", f.pad, flip(|f| &mut f.pad)))
                            .child(control("ma-c-keys", "keyboard", "Keyboard", f.keys, flip(|f| &mut f.keys))),
                    )
                    .child(hint("Devices with these get it recommended, and phones need Touch to list it.".into(), false)),
            )
            .child(
                field()
                    .child(label("Categories", false))
                    .child(boxed(&f.categories, false))
                    .child(hint("Comma-separated".into(), false)),
            )
            .child(field().child(label("Developer", true)).child(boxed(&f.developer, false)))
            .child(field().child(label("Source or Website", true)).child(boxed(&f.source, false)))
            .child(field().child(label("What's New", true)).child(boxed(&f.changelog, true)))
            .child(
                field()
                    .child(label("Permissions", true))
                    .child(hint(
                        "Each one is asked for on first launch. Leave them all off unless the app needs one.".into(),
                        false,
                    ))
                    .child(perms),
            );

        let submit = match (f.busy, editing) {
            (true, true) => "Saving…",
            (true, false) => "Publishing…",
            (false, true) => "Save Changes",
            (false, false) => "Publish",
        };
        let card = crate::security::card("ma-publish")
            .w(px(520.))
            .max_h(px(vh - 32.))
            .overflow_y_scroll()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(head)
            .child(form)
            .child(crate::security::actions().child(crate::security::button(
                "ma-publish-go",
                submit,
                false,
                !f.busy,
                act(Self::publish_submit),
            )));
        let backdrop = cx.listener(|a, _: &gpui::MouseDownEvent, _, cx| a.publish_cancel(cx));
        Some(
            gpui::deferred(
                div()
                    .id("ma-publish-overlay")
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
}
