//! The Nexus's side of the app (`js/marketplace.js`): opening it, its actions, and the
//! catalogue and download events.

use gpui::{AnyElement, Context, FontWeight, SharedString, Window, prelude::*, px};
use serde_json::Value;

use super::{VectorApp, backend};
use crate::composer::{InputChanged, TextInput};
use crate::nexus::{Act, Action, Kind, Nexus, View};
use crate::theme::*;

impl VectorApp {
    /// `showMiniApps` / `showMarketplacePanel`: the Library when anything is installed, else Discover.
    pub(super) fn show_nexus(&mut self, view: Option<View>, window: &mut Window, cx: &mut Context<Self>) {
        if self.nexus.is_none() {
            let search = cx.new(|cx| {
                TextInput::new("Search the App Store", cx).with_look(crate::composer::Look {
                    family: RUBIK,
                    size: px(14.),
                    line_height: px(17.),
                    weight: FontWeight::NORMAL,
                    placeholder: c(0xb2b2b2),
                    ..crate::kit::field_look()
                })
            });
            cx.subscribe_in(&search, window, |app, _, _: &InputChanged, _, cx| {
                if let Some(n) = app.nexus.as_mut() {
                    if n.details.is_some() {
                        n.details = None;
                    }
                    n.turned = std::time::Instant::now();
                    n.scroll.set_offset(gpui::Point::default());
                }
                cx.notify();
            })
            .detach();
            cx.subscribe_in(&search, window, |app, _, _: &crate::composer::CancelInput, window, cx| {
                app.on_nexus(Act::ClearSearch, window, cx);
            })
            .detach();
            let root = (!self.live).then(|| self.store.borrow().media_root.clone());
            let mut n = Nexus::new(search, root);
            if let Some(ms) = self.store.borrow().frozen_now_ms {
                n.now_ms = ms as i64;
            }
            self.nexus = Some(n);
            self.settings = None;
            self.attach = None;
            self.mini = None;
            self.nexus_load(view, cx);
        } else if let Some(v) = view {
            let n = self.nexus.as_mut().unwrap();
            n.show(v, cx);
        }
        cx.notify();
    }

    /// `initMarketplace`: the cache at once, then the network with install status.
    fn nexus_load(&mut self, view: Option<View>, cx: &mut Context<Self>) {
        let Some(n) = self.nexus.as_mut() else { return };
        if !self.live {
            let mut apps = self.store.borrow().fixture_market.clone();
            for a in &mut apps {
                a.categories = a.categories.iter().map(|c| c.trim().to_lowercase()).collect();
            }
            let now = n.now_ms;
            n.played = apps
                .iter()
                .filter(|a| a.installed)
                .enumerate()
                .map(|(i, a)| (a.id.clone(), now - 3_600_000 * (i as i64 * 30 + 2), 3))
                .collect();
            n.apps = apps;
            let installed = n.apps.iter().any(crate::nexus::is_installed);
            n.view = view.unwrap_or(if installed { View::Library } else { View::Discover });
            self.nexus_washes(cx);
            return;
        }
        n.loading = true;
        n.error.clear();
        let explicit = view.clone();
        if let Some(v) = view {
            n.view = v;
        }
        let cached = backend::run(async {
            let apps = backend::market::cached().await;
            (apps, backend::market::plays())
        });
        cx.spawn(async move |this, cx| {
            let Ok((apps, played)) = cached.await else { return };
            this.update(cx, |this, cx| {
                if let Some(n) = this.nexus.as_mut() {
                    if explicit.is_none() {
                        n.view = if apps.iter().any(crate::nexus::is_installed) { View::Library } else { View::Discover };
                    }
                    if !apps.is_empty() {
                        n.apps = apps;
                    }
                    n.played = played;
                }
                this.nexus_washes(cx);
                cx.notify();
            })
            .ok();
            let fresh = backend::run(backend::market::refresh());
            let res = fresh.await;
            this.update(cx, |this, cx| {
                let Some(n) = this.nexus.as_mut() else { return };
                n.loading = false;
                match res {
                    Ok(Ok(apps)) => n.apps = apps,
                    Ok(Err(e)) => {
                        n.error = e;
                        if !n.apps.is_empty() {
                            this.show_toast("Couldn't refresh the App Store", cx);
                        }
                    }
                    Err(_) => {}
                }
                this.nexus_washes(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Each icon's colour for the spotlight, read off the thread.
    fn nexus_washes(&mut self, cx: &mut Context<Self>) {
        let Some(n) = self.nexus.as_ref() else { return };
        let todo: Vec<(String, std::path::PathBuf)> = n
            .apps
            .iter()
            .filter(|a| !n.wash.contains_key(&a.id))
            .filter_map(|a| Some((a.id.clone(), n.icon_path(a)?)))
            .collect();
        if todo.is_empty() {
            return;
        }
        let task = cx.background_executor().spawn(async move {
            todo.into_iter().filter_map(|(id, p)| Some((id, crate::nexus::wash_of(&p)?))).collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            this.update(cx, |this, cx| {
                if let Some(n) = this.nexus.as_mut() {
                    n.wash.extend(found);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn nexus_event(&mut self, name: &str, payload: &Value, cx: &mut Context<Self>) {
        let Some(n) = self.nexus.as_mut() else { return };
        match name {
            "marketplace_install_progress" => {
                let id = payload.get("app_id").and_then(Value::as_str).unwrap_or_default();
                let pct = payload.get("progress").and_then(Value::as_f64).unwrap_or(0.) as f32;
                if let Some(a) = n.actions.get_mut(id) {
                    a.pct = pct;
                }
            }
            "marketplace_apps_updated" => {
                if let Ok(apps) = serde_json::from_value::<Vec<crate::backend::market::MarketplaceApp>>(payload.clone()) {
                    let keep: Vec<_> = n.apps.drain(..).collect();
                    n.apps = apps
                        .into_iter()
                        .map(|mut a| {
                            a.categories = a.categories.iter().map(|c| c.trim().to_lowercase()).collect();
                            if let Some(old) = keep.iter().find(|o| o.id == a.id) {
                                a.installed |= old.installed;
                                if a.local_path.is_none() {
                                    a.local_path = old.local_path.clone();
                                }
                            }
                            a
                        })
                        .collect();
                }
                self.nexus_washes(cx);
            }
            _ => return,
        }
        cx.notify();
    }

    pub(super) fn close_nexus(&mut self, cx: &mut Context<Self>) {
        self.nexus = None;
        cx.notify();
    }

    pub(super) fn on_nexus(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        let Some(n) = self.nexus.as_mut() else { return };
        match act {
            Act::Show(v) => n.show(v, cx),
            Act::Details(id) => {
                n.open_details(id.clone(), cx);
                if self.live
                    && let Some(npub) = n.app(&id).map(|a| a.publisher.clone())
                    && !self.store.borrow().profiles.contains_key(&npub)
                {
                    let _ = backend::run(async move {
                        vector_core::profile::sync::load_profile(npub, &crate::backend::sync::ProfileHandler).await;
                    });
                }
            }
            Act::CloseDetails => n.close_details(cx),
            Act::Close => return self.close_nexus(cx),
            Act::Retry => {
                if self.live && !n.loading {
                    let v = n.view.clone();
                    self.nexus_load(Some(v), cx);
                }
            }
            Act::Card(id) => self.nexus_card(id, cx),
            Act::Uninstall(id) => self.nexus_uninstall(id, cx),
            Act::UpdateAll => {
                let ids: Vec<String> = n
                    .apps
                    .iter()
                    .filter(|a| a.update_available && !n.actions.contains_key(&a.id))
                    .map(|a| a.id.clone())
                    .collect();
                self.nexus_update_all(ids, cx);
            }
            Act::FitOnly(on) => {
                n.fit_only = on;
                backend::device::set("nexus_fit_only", on.to_string());
            }
            Act::More => n.more = !n.more,
            Act::Spot(i) => {
                n.spot = i;
                n.spot_since = std::time::Instant::now();
            }
            Act::SpotHold(on) => {
                n.spot_held = on;
                n.spot_since = std::time::Instant::now();
            }
            Act::Shelf(key, dir) => {
                let h = n.shelf(key);
                let w = f32::from(h.bounds().size.width);
                let max = f32::from(h.max_offset().x);
                let at = -f32::from(h.offset().x);
                let to = (at + dir * (w * 0.8).max(160.)).clamp(0., max);
                h.set_offset(gpui::point(px(-to), px(0.)));
            }
            Act::PlayWith(_) => self.show_toast("Play with Friends isn't in this build yet", cx),
            Act::Publisher(npub) => {
                self.nexus = None;
                self.open_profile(npub, cx);
            }
            Act::OpenUrl(url) => self.open_web(&url, cx),
            Act::CopyLink(id) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(format!("https://vectorapp.io/go#nexus/{id}")));
                self.show_toast("Copied App Link", cx);
            }
            Act::ClearSearch => {
                n.search.update(cx, |s, cx| s.set_text("", cx));
                n.turned = std::time::Instant::now();
            }
            Act::Menu(id, at) => {
                self.menu = Some((at, vec![crate::kit::MenuItem::new("Copy Link", "share")], vec![super::MenuAct::NexusLink(id)]));
            }
        }
        let _ = window;
        cx.notify();
    }

    /// `runAppAction`: the button shows the action, and "Failed" for 2s if it throws.
    fn nexus_run(
        &mut self,
        id: String,
        kind: Kind,
        label: &str,
        failed: &'static str,
        fut: impl std::future::Future<Output = Result<Option<String>, String>> + Send + 'static,
        done: impl FnOnce(&mut VectorApp, Option<String>, &mut Context<VectorApp>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(n) = self.nexus.as_mut() else { return };
        n.set_action(&id, Some(Action { kind, label: SharedString::from(label.to_string()), pct: 0. }));
        cx.notify();
        if !self.live {
            return;
        }
        let rx = backend::run(fut);
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                match res {
                    Ok(out) => {
                        if let Some(n) = this.nexus.as_mut() {
                            n.set_action(&id, None);
                        }
                        done(this, out, cx);
                    }
                    Err(e) => {
                        vector_core::log_warn!("[Nexus] {id}: {e}");
                        if let Some(n) = this.nexus.as_mut() {
                            n.set_action(&id, Some(Action { kind: Kind::Failed, label: failed.into(), pct: 0. }));
                        }
                        let id = id.clone();
                        cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
                            this.update(cx, |this, cx| {
                                if let Some(n) = this.nexus.as_mut()
                                    && n.actions.get(&id).is_some_and(|a| a.kind == Kind::Failed)
                                {
                                    n.set_action(&id, None);
                                    cx.notify();
                                }
                            })
                            .ok();
                        })
                        .detach();
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `handleAppInstallOrPlay`: update if one waits, play if installed, otherwise install.
    fn nexus_card(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(app) = self.nexus.as_ref().and_then(|n| n.app(&id)).cloned() else { return };
        if app.update_available {
            self.nexus_update(id, cx);
        } else if crate::nexus::is_installed(&app) {
            let open_id = id.clone();
            self.nexus_run(
                id,
                Kind::Launching,
                crate::nexus::launching_text(&app),
                crate::nexus::action_text(&app),
                async move { backend::market::open(open_id).await.map(|_| None) },
                |this, _, _| {
                    if let Some(n) = this.nexus.as_mut() {
                        n.played = backend::market::plays();
                    }
                },
                cx,
            );
        } else {
            let version = app.version.clone();
            let install_id = id.clone();
            let patch_id = id.clone();
            self.nexus_run(
                id,
                Kind::Installing,
                "Installing",
                "Failed",
                async move { backend::market::install(install_id).await.map(Some) },
                move |this, path, _| {
                    if let Some(n) = this.nexus.as_mut() {
                        n.patch(&patch_id, |a| {
                            a.installed = true;
                            a.installed_version = Some(version);
                            a.local_path = path;
                            a.update_available = false;
                        });
                    }
                },
                cx,
            );
        }
    }

    fn nexus_update(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(app) = self.nexus.as_ref().and_then(|n| n.app(&id)).cloned() else { return };
        let (run_id, patch_id, version) = (id.clone(), id.clone(), app.version.clone());
        self.nexus_run(
            id,
            Kind::Updating,
            "Updating",
            "Failed",
            async move { backend::market::update(run_id).await.map(Some) },
            move |this, path, _| {
                if let Some(n) = this.nexus.as_mut() {
                    n.patch(&patch_id, |a| {
                        a.installed_version = Some(version);
                        a.update_available = false;
                        if path.is_some() {
                            a.local_path = path;
                        }
                    });
                }
            },
            cx,
        );
    }

    /// `updateAllApps`: one at a time, as they share the connection and the disk.
    fn nexus_update_all(&mut self, mut ids: Vec<String>, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        let first = ids.remove(0);
        self.nexus_update(first.clone(), cx);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
                let busy = this
                    .update(cx, |this, _| this.nexus.as_ref().is_some_and(|n| n.actions.get(&first).is_some_and(|a| a.kind == Kind::Updating)))
                    .unwrap_or(false);
                if !busy {
                    break;
                }
            }
            this.update(cx, |this, cx| this.nexus_update_all(ids, cx)).ok();
        })
        .detach();
    }

    fn nexus_uninstall(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(app) = self.nexus.as_ref().and_then(|n| n.app(&id)).cloned() else { return };
        let mut p = crate::kit::Popup::notice(format!("Uninstall {}?", app.name));
        p.text = Some("This will remove the app from your device. You can reinstall it anytime.".into());
        p.confirm = "Uninstall".into();
        p.cancel = Some("Cancel".into());
        self.popup = Some((p, super::PopupAct::NexusUninstall(id)));
        cx.notify();
    }

    pub(super) fn nexus_uninstall_confirmed(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(app) = self.nexus.as_ref().and_then(|n| n.app(&id)).cloned() else { return };
        let (run_id, patch_id, name) = (id.clone(), id.clone(), app.name.clone());
        self.nexus_run(
            id,
            Kind::Uninstalling,
            "Uninstalling...",
            "Failed",
            async move { backend::market::uninstall(run_id, name).await.map(|_| None) },
            move |this, _, cx| {
                if let Some(n) = this.nexus.as_mut() {
                    n.patch(&patch_id, |a| {
                        a.installed = false;
                        a.local_path = None;
                        a.installed_version = None;
                        a.update_available = false;
                    });
                    n.played = backend::market::plays();
                }
                this.show_toast("App uninstalled", cx);
            },
            cx,
        );
    }

    pub(super) fn nexus_view(&mut self, width: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        let n = self.nexus.as_mut()?;
        // The spotlight moves on by itself unless a pointer rests on it.
        let count = n.spotlight().len();
        if count > 1 && !n.spot_held && n.spot_since.elapsed().as_secs_f32() >= 7. {
            n.spot = (n.spot + 1) % count;
            n.spot_since = std::time::Instant::now();
        }
        if let Some(wait) = self.nexus_wake()
            && let Some(n) = self.nexus.as_mut()
            && n.spot_armed != Some(n.spot_since)
        {
            n.spot_armed = Some(n.spot_since);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        }
        let n = self.nexus.as_ref()?;
        let publisher = n.details.as_deref().and_then(|id| n.app(id)).and_then(|a| {
            let store = self.store.borrow();
            store.profiles.contains_key(&a.publisher).then(|| {
                (SharedString::from(store.name(&a.publisher)), store.avatar_path(&a.publisher))
            })
        });
        let app = cx.entity().downgrade();
        let on: crate::nexus::OnAct = std::rc::Rc::new(move |act, window, cx| {
            app.update(cx, |app, cx| app.on_nexus(act, window, cx)).ok();
        });
        Some(crate::nexus::view(n, publisher.as_ref(), width, &on, cx))
    }

    /// When the spotlight next turns, so the frame loop can rest until then.
    pub(super) fn nexus_wake(&self) -> Option<std::time::Duration> {
        let n = self.nexus.as_ref()?;
        if n.details.is_some() || n.spot_held || n.spotlight().len() < 2 {
            return None;
        }
        Some(std::time::Duration::from_secs(7).saturating_sub(n.spot_since.elapsed()))
    }
}
