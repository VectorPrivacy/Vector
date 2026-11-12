//! The Nexus's side of the app (`js/marketplace.js`): opening it, its actions, and the
//! catalogue and download events.

use gpui::{AnyElement, Context, Focusable, FontWeight, SharedString, Window, div, prelude::*, px};
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
            n.me = self.store.borrow().my_npub.clone();
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
            self.nexus_apps_changed(cx);
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
                this.nexus_apps_changed(cx);
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
                this.nexus_apps_changed(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The catalogue changed: an awaited link's page opens once its app arrives.
    fn nexus_apps_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(n) = self.nexus.as_mut()
            && let Some(id) = n.awaited.take_if(|id| n.apps.iter().any(|a| a.id == *id))
        {
            n.open_details(id.clone(), cx);
            self.nexus_perms(&id);
        }
        self.nexus_washes(cx);
    }

    /// `showAppDetailsById`: one app's page, by id, never launching it.
    pub(super) fn nexus_show_app(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.show_nexus(None, window, cx);
        let Some(n) = self.nexus.as_mut() else { return };
        if n.app(&id).is_some() {
            n.awaited = None;
            self.on_nexus(Act::Details(id), window, cx);
        } else {
            n.awaited = Some(id);
        }
    }

    /// `openStartKey('app/…')`: an installed app launches, or offers its invite in a chat; else its page.
    pub(super) fn open_app_pin(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let app = if self.live {
            backend::market::find(&id)
        } else {
            self.store.borrow().fixture_market.iter().find(|a| a.id == id).cloned()
        };
        let Some(app) = app.filter(crate::nexus::is_installed) else {
            return self.nexus_show_app(id, window, cx);
        };
        let can_invite = self.open.as_ref().is_some_and(|c| {
            let store = self.store.borrow();
            c.starts_with("npub1") && !store.profiles.get(c.as_ref()).is_some_and(|p| p.is_blocked)
        });
        match app.local_path.clone() {
            Some(src) if can_invite => {
                let icon = app.icon_cached.as_ref().map(std::path::PathBuf::from);
                self.launch = Some(super::miniapps_ops::Launch {
                    app: super::miniapps_ops::GridApp {
                        name: app.name.clone(),
                        src,
                        icon,
                        categories: app.categories.join(","),
                        last: 0,
                        market: Some(app.id.clone()),
                    },
                });
                cx.notify();
            }
            _ => self.perm_gate(
                backend::market::Ask::App(id.clone()),
                move |this, cx| {
                    if !this.live {
                        return;
                    }
                    let rx = backend::run(backend::market::open(id));
                    cx.spawn(async move |this, cx| {
                        if let Ok(Err(e)) = rx.await {
                            this.update(cx, |this, cx| this.show_toast(&e, cx)).ok();
                        }
                    })
                    .detach();
                },
                cx,
            ),
        }
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
                self.nexus_apps_changed(cx);
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
                self.nexus_perms(&id);
                let Some(n) = self.nexus.as_mut() else { return };
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
            Act::PlayWith(id) => self.play_with(id, window, cx),
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
            Act::EditListing(id) => return self.open_edit_listing(id, window, cx),
            Act::Perm(perm, on) => self.nexus_set_perm(perm, on, cx),
            Act::ResetPerms => {
                if let Some(app) = n.details.as_ref().and_then(|id| n.app(id)).cloned() {
                    if self.live {
                        let _ = backend::market::revoke_permissions(&app.blossom_hash);
                    } else {
                        FIXTURE_GRANTS.with(|g| g.borrow_mut().insert(app.blossom_hash.clone(), String::new()));
                    }
                    self.nexus_perms(&app.id);
                }
            }
            Act::Menu(id, at) => {
                let key = format!("app/{id}");
                let pinned = self.start_pinned(&key);
                self.menu = Some((
                    at,
                    vec![
                        crate::kit::MenuItem::new("Copy Link", "share"),
                        if pinned {
                            crate::kit::MenuItem::new("Unpin", "pin").danger()
                        } else {
                            crate::kit::MenuItem::new("Pin to Main Menu", "pin")
                        },
                    ],
                    vec![super::MenuAct::NexusLink(id), super::MenuAct::StartTogglePin(key)],
                ));
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

    /// `openMarketplaceApp`, once its permissions are settled.
    fn nexus_launch(&mut self, app: backend::market::MarketplaceApp, cx: &mut Context<Self>) {
        let open_id = app.id.clone();
        self.nexus_run(
            app.id.clone(),
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
    }

    /// `handleAppInstallOrPlay`: update if one waits, play if installed, otherwise install.
    fn nexus_card(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(app) = self.nexus.as_ref().and_then(|n| n.app(&id)).cloned() else { return };
        if app.update_available {
            self.nexus_update(id, cx);
        } else if crate::nexus::is_installed(&app) {
            self.perm_gate(backend::market::Ask::App(id.clone()), move |this, cx| this.nexus_launch(app, cx), cx);
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
                    this.nexus_perms(&patch_id);
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

    /// `openPlayWith`: install it if needed, then choose who to play with.
    fn play_with(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(app) = self.nexus.as_ref().and_then(|n| n.app(&id)).cloned() else { return };
        if !crate::nexus::is_installed(&app) {
            self.nexus_card(id.clone(), cx);
            cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
                    let state = this
                        .update(cx, |this, _| {
                            let n = this.nexus.as_ref()?;
                            Some((n.actions.contains_key(&id), n.app(&id).is_some_and(crate::nexus::is_installed)))
                        })
                        .ok()
                        .flatten();
                    match state {
                        Some((true, _)) => continue,
                        Some((false, true)) => {
                            this.update_in(cx, |this, window, cx| this.open_picker(id.clone(), window, cx)).ok();
                        }
                        _ => {}
                    }
                    return;
                }
            })
            .detach();
            return;
        }
        self.open_picker(id, window, cx);
    }

    fn open_picker(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let search = cx.new(|cx| {
            TextInput::new("Search chats", cx).with_look(crate::composer::Look {
                family: RUBIK,
                size: px(15.),
                line_height: px(18.),
                weight: FontWeight::NORMAL,
                color: c(0xffffff),
                placeholder: c(0x7a7a7a),
                ..Default::default()
            })
        });
        cx.subscribe(&search, |_, _, _: &InputChanged, cx| cx.notify()).detach();
        cx.subscribe(&search, |app, _, _: &crate::composer::CancelInput, cx| {
            if let Some(n) = app.nexus.as_mut() {
                n.picker = None;
            }
            cx.notify();
        })
        .detach();
        window.focus(&search.read(cx).focus_handle(cx), cx);
        if let Some(n) = self.nexus.as_mut() {
            n.picker = Some((id, search));
        }
        cx.notify();
    }

    /// `nexusChatChoices`: the DMs you're talking in and the channels you can post in, newest first.
    fn pick_chats(&self) -> Vec<(String, String, String, Option<std::path::PathBuf>, bool)> {
        let store = self.store.borrow();
        let mut out: Vec<(u64, (String, String, String, Option<std::path::PathBuf>, bool))> = Vec::new();
        for ch in &store.chats {
            if ch.id == store.my_npub {
                continue;
            }
            if let Some(com) = store.community_of(&ch.id) {
                let Some(channel) = com.channel(&ch.id) else { continue };
                if channel.is_voice() || store.send_lock(&ch.id).is_some() {
                    continue;
                }
                let name = if com.name.is_empty() { "Community".to_string() } else { com.name.clone() };
                let room = if channel.name.is_empty() { "general" } else { channel.name.as_str() };
                out.push((
                    crate::data::last_activity(ch),
                    (ch.id.clone(), name, format!("#{room}"), store.icon(&com.community_id), true),
                ));
            } else if ch.id.starts_with("npub1")
                && !ch.messages.is_empty()
                && !store.is_request(ch)
                && !store.profiles.get(&ch.id).is_some_and(|p| p.is_blocked)
            {
                out.push((
                    crate::data::last_activity(ch),
                    (ch.id.clone(), store.name(&ch.id), "Direct message".into(), store.avatar_path(&ch.id), false),
                ));
            }
        }
        out.sort_by(|a, b| b.0.cmp(&a.0));
        out.into_iter().map(|(_, c)| c).collect()
    }

    /// `sendAppToChat`: the invite goes out, the chat opens, and the game opens from it.
    fn send_app_to(&mut self, chat: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(n) = self.nexus.as_mut() else { return };
        let Some((id, _)) = n.picker.take() else { return };
        let Some(app) = n.app(&id).cloned() else { return };
        let icon = n.icon_path(&app);
        let Some(path) = app.local_path.clone() else {
            return self.show_toast("Install it first", cx);
        };
        self.nexus = None;
        self.open_chat(chat.clone().into(), window, cx);
        let grid = super::miniapps_ops::GridApp {
            name: app.name.clone(),
            src: path,
            icon,
            categories: app.categories.join(","),
            last: 0,
            market: Some(id),
        };
        self.gated_invite(grid, chat, cx);
    }

    pub(super) fn nexus_picker_view(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let n = self.nexus.as_ref()?;
        let (id, search) = n.picker.as_ref()?;
        let app = n.app(id)?;
        let q = search.read(cx).text().trim().to_lowercase();
        let chats: Vec<_> = self
            .pick_chats()
            .into_iter()
            .filter(|c| q.is_empty() || c.1.to_lowercase().contains(&q) || c.2.to_lowercase().contains(&q))
            .collect();
        let me = cx.entity().downgrade();
        let close = {
            let me = me.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                me.update(cx, |a, cx| {
                    if let Some(n) = a.nexus.as_mut() {
                        n.picker = None;
                    }
                    cx.notify();
                })
                .ok();
            }
        };
        let head = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(crate::nexus::app_icon(n, app, 40.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(18.))
                            .line_height(px(22.))
                            .font_weight(FontWeight::BOLD)
                            .child("Play with Friends"),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.))
                            .line_height(px(19.5))
                            .text_color(white_a(0.55))
                            .child(format!("Send {} to a chat, and it opens for you", app.name)),
                    ),
            )
            .child(
                div()
                    .id("nx-pick-x")
                    .group("nx-pick-x")
                    .self_start()
                    .w(px(23.7))
                    .h(px(22.))
                    .flex()
                    .justify_center()
                    .pt(px(6.))
                    .cursor_pointer()
                    .on_click(close)
                    .child(
                        gpui::svg()
                            .path("ui/x-mark.svg")
                            .size(px(9.))
                            .text_color(white_a(0.4))
                            .group_hover("nx-pick-x", |d| d.text_color(c(0xffffff))),
                    ),
            );
        let field = div()
            .relative()
            .flex_shrink_0()
            .h(px(40.))
            .rounded(px(10.))
            .bg(c(0x1b1b1b))
            .pl(px(38.))
            .pr(px(12.))
            .flex()
            .items_center()
            .child(div().flex_1().min_w_0().child(search.clone()))
            .child(gpui::svg().path("icons/search.svg").absolute().left(px(12.)).top(px(11.5)).size(px(17.)).text_color(c(0x8a8a8a)));
        let mut list = div().id("nx-pick-list").flex_1().min_h_0().mx(px(-8.)).overflow_y_scroll().flex().flex_col();
        if chats.is_empty() {
            list = list.child(
                div()
                    .py(px(28.))
                    .text_center()
                    .text_size(px(14.))
                    .text_color(c(0x6f6f6f))
                    .child(if q.is_empty() { "No chats yet: start one to play together" } else { "No chats match" }),
            );
        }
        for (chat, name, sub, avatar, is_group) in chats {
            let group = SharedString::from(format!("nx-pick-{chat}"));
            let me = me.clone();
            let target = chat.clone();
            list = list.child(
                div()
                    .id(group.clone())
                    .group(group.clone())
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .w_full()
                    .p(px(8.))
                    .rounded(px(12.))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(0x1a1a1a)))
                    .on_click(move |_, window, cx| {
                        me.update(cx, |a, cx| a.send_app_to(target.clone(), window, cx)).ok();
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .flex_shrink_0()
                            .size(px(40.))
                            .map(|d| if is_group { d.rounded(px(8.8)) } else { d.rounded_full() })
                            .overflow_hidden()
                            .bg(c(0x1b1b1b))
                            .child(match avatar {
                                Some(p) => gpui::img(p)
                                    .size_full()
                                    .map(|d| if is_group { d.rounded(px(8.8)) } else { d.rounded_full() })
                                    .object_fit(gpui::ObjectFit::Cover)
                                    .into_any_element(),
                                None => gpui::svg()
                                    .path(if is_group { "icons/users-multi.svg" } else { "icons/user-circle.svg" })
                                    .size(px(22.))
                                    .text_color(c(0x6f6f6f))
                                    .into_any_element(),
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().text_size(px(15.)).line_height(px(19.)).child(name))
                            .child(div().text_size(px(12.)).line_height(px(16.)).text_color(c(0x7a7a7a)).child(sub)),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c(PRIMARY))
                            .opacity(0.)
                            .group_hover(group, |s| s.opacity(1.))
                            .child("Send"),
                    ),
            );
        }
        let card = crate::security::card("nx-pick")
            .w(px(420.))
            .max_h(px(620.))
            .gap(px(12.))
            .pb(px(10.))
            .overflow_hidden()
            .font_family(RUBIK)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(head)
            .child(field)
            .child(list);
        let backdrop = cx.listener(|a, _: &gpui::MouseDownEvent, _, cx| {
            if let Some(n) = a.nexus.as_mut() {
                n.picker = None;
            }
            cx.notify();
        });
        Some(
            gpui::deferred(
                div()
                    .id("nx-pick-overlay")
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

    /// When the spotlight next turns, so the frame loop can rest until then.
    pub(super) fn nexus_wake(&self) -> Option<std::time::Duration> {
        let n = self.nexus.as_ref()?;
        if n.details.is_some() || n.spot_held || n.spotlight().len() < 2 {
            return None;
        }
        Some(std::time::Duration::from_secs(7).saturating_sub(n.spot_since.elapsed()))
    }
}

thread_local! {
    /// Fixture runs keep grants in memory: bundle hash to its granted list, present once prompted.
    static FIXTURE_GRANTS: std::cell::RefCell<std::collections::HashMap<String, String>> = Default::default();
}

/// A first launch's permission prompt and the launch it holds.
pub(super) struct PermPrompt {
    prompt: backend::market::Prompt,
    on: Vec<bool>,
    then: Box<dyn FnOnce(&mut VectorApp, &mut Context<VectorApp>)>,
}

impl VectorApp {
    /// `showPermissionPrompt` ahead of `then`, when the app asks for anything it hasn't been asked before.
    pub(super) fn perm_gate(
        &mut self,
        ask: backend::market::Ask,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        if !self.live {
            let prompt = match &ask {
                backend::market::Ask::App(id) => self.nexus.as_ref().and_then(|n| n.app(id)).and_then(|a| {
                    backend::market::prompt_of(a, |h| FIXTURE_GRANTS.with(|g| g.borrow().contains_key(h)))
                }),
                backend::market::Ask::File(_) => None,
            };
            return match prompt {
                Some(p) => self.show_perm_prompt(p, Box::new(then), cx),
                None => then(self, cx),
            };
        }
        let rx = backend::run(backend::market::prompt_for(ask));
        cx.spawn(async move |this, cx| {
            let prompt = rx.await.ok().flatten();
            this.update(cx, |this, cx| match prompt {
                Some(p) => this.show_perm_prompt(p, Box::new(then), cx),
                None => then(this, cx),
            })
            .ok();
        })
        .detach();
    }

    fn show_perm_prompt(
        &mut self,
        prompt: backend::market::Prompt,
        then: Box<dyn FnOnce(&mut VectorApp, &mut Context<VectorApp>)>,
        cx: &mut Context<Self>,
    ) {
        let on = vec![false; prompt.items.len()];
        self.perm_prompt = Some(PermPrompt { prompt, on, then });
        cx.notify();
    }

    /// Continue saves every choice, so the prompt never shows for this bundle again.
    fn perm_answer(&mut self, allow: bool, cx: &mut Context<Self>) {
        let Some(p) = self.perm_prompt.take() else { return };
        cx.notify();
        if !allow {
            return;
        }
        let perms: Vec<(String, bool)> = p.prompt.items.iter().map(|i| i.0.clone()).zip(p.on.iter().copied()).collect();
        if self.live {
            if let Err(e) = backend::market::set_permissions(&p.prompt.hash, &perms) {
                log::warn!("saving permissions: {e}");
            }
        } else {
            let granted: Vec<&str> = perms.iter().filter(|p| p.1).map(|p| p.0.as_str()).collect();
            FIXTURE_GRANTS.with(|g| g.borrow_mut().insert(p.prompt.hash.clone(), granted.join(",")));
        }
        if let Some(id) = self.nexus.as_ref().and_then(|n| n.details.clone()) {
            self.nexus_perms(&id);
        }
        (p.then)(self, cx);
    }

    /// `loadAppPermissions`: the open page's toggles, keyed by its bundle hash.
    pub(super) fn nexus_perms(&mut self, id: &str) {
        let live = self.live;
        let Some(n) = self.nexus.as_mut() else { return };
        let Some(app) = n.app(id) else { return };
        if app.requested_permissions.trim().is_empty() {
            n.perms = None;
            return;
        }
        let rows = if app.blossom_hash.is_empty() {
            Err("No permissions available".to_string())
        } else if live {
            backend::market::granted(&app.blossom_hash)
                .map(|g| backend::market::permission_rows(&app.requested_permissions, &g))
                .map_err(|_| "Failed to load permissions".to_string())
        } else {
            let g = FIXTURE_GRANTS.with(|g| g.borrow().get(&app.blossom_hash).cloned().unwrap_or_default());
            Ok(backend::market::permission_rows(&app.requested_permissions, &g))
        };
        n.perms = Some((id.to_string(), rows));
    }

    /// `setAppPermission`: flip it in place, or reload the truth when the write fails.
    fn nexus_set_perm(&mut self, perm: String, on: bool, cx: &mut Context<Self>) {
        let live = self.live;
        let Some(n) = self.nexus.as_mut() else { return };
        let Some(app) = n.details.as_ref().and_then(|id| n.app(id)).cloned() else { return };
        let saved = if live {
            backend::market::set_permission(&app.blossom_hash, &perm, on).is_ok()
        } else {
            FIXTURE_GRANTS.with(|g| {
                let mut g = g.borrow_mut();
                let list = g.entry(app.blossom_hash.clone()).or_default();
                let mut set: Vec<String> = list.split(',').filter(|s| !s.is_empty() && *s != perm).map(String::from).collect();
                if on {
                    set.push(perm.clone());
                }
                *list = set.join(",");
            });
            true
        };
        if !saved {
            return self.nexus_perms(&app.id);
        }
        if let Some((_, Ok(rows))) = n.perms.as_mut() {
            for r in rows.iter_mut().filter(|r| r.0 == perm) {
                r.3 = on;
            }
        }
        cx.notify();
    }

    pub(super) fn perm_prompt_view(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.perm_prompt.as_ref()?;
        let me = cx.entity().downgrade();
        let answer = |allow: bool| {
            let me = me.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                me.update(cx, |a, cx| a.perm_answer(allow, cx)).ok();
            }
        };
        let mut list = div()
            .id("ma-perm-list")
            .flex()
            .flex_col()
            .max_h(px(0.46 * 900.))
            .overflow_y_scroll()
            .rounded(px(14.))
            .bg(c(0x0c0c0c))
            .border_1()
            .border_color(c(0x1f1f1f));
        for (i, ((id, label, sub), on)) in p.prompt.items.iter().zip(p.on.iter().copied()).enumerate() {
            let flip = {
                let me = me.clone();
                move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                    me.update(cx, |a, cx| {
                        if let Some(v) = a.perm_prompt.as_mut().and_then(|p| p.on.get_mut(i)) {
                            *v = !*v;
                        }
                        cx.notify();
                    })
                    .ok();
                }
            };
            list = list.child(
                div()
                    .id(SharedString::from(format!("ma-perm-{id}")))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(14.))
                    .py(px(12.))
                    .cursor_pointer()
                    .when(i > 0, |d| d.border_t_1().border_color(c(0x1f1f1f)))
                    .on_click(flip.clone())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(px(15.)).line_height(px(22.5)).font_weight(FontWeight::MEDIUM).child(label.clone()))
                            .child(div().text_size(px(12.5)).line_height(px(17.5)).text_color(white_a(0.55)).child(sub.clone())),
                    )
                    .child(crate::settings::toggle(SharedString::from(format!("ma-perm-t-{id}")), on, true, flip)),
            );
        }
        let card = crate::security::card("ma-perm")
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(crate::security::head("icons/shield-filled.svg", format!("{} asks for access", p.prompt.name).into(), None))
            .child(crate::security::lead(
                "Turn on what you're happy to share. It may work with less, and you can change these later on its App Store page.",
                false,
            ))
            .child(list)
            .child(
                crate::security::actions()
                    .child(crate::security::button("ma-perm-go", "Continue", false, true, answer(true)))
                    .child(crate::security::button("ma-perm-cancel", "Cancel", true, true, answer(false))),
            );
        let backdrop = cx.listener(|a, _: &gpui::MouseDownEvent, _, cx| a.perm_answer(false, cx));
        Some(
            gpui::deferred(
                div()
                    .id("ma-perm-overlay")
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
            .with_priority(10)
            .into_any_element(),
        )
    }
}
