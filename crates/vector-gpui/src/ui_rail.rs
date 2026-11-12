//! The community rail's arrangement: folders opened per device, edits synced per account.

use gpui::{AppContext, Context, Focusable, Pixels, Point, SharedString, Window};
use vector_core::rail_layout::{RailDrag, RailDrop, RailLayout, RailNode};

use super::{MenuAct, PopupAct, VectorApp};
use crate::{backend, kit};

const OPEN_KEY: &str = "rail_open_folders";

impl VectorApp {
    pub(super) fn load_rail(&mut self) {
        self.rail_open = backend::device::get(OPEN_KEY)
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .unwrap_or_default()
            .into_iter()
            .collect();
        let layout = if self.live {
            backend::rail::load()
        } else {
            // Fixtures: VECTOR_RAIL_LAYOUT holds the synced document.
            std::env::var("VECTOR_RAIL_LAYOUT")
                .map(|s| RailLayout::from_json(&s))
                .unwrap_or_default()
        };
        self.store.borrow_mut().rail = layout;
    }

    pub(super) fn rail_event(&mut self, payload: &serde_json::Value, cx: &mut Context<Self>) {
        self.store.borrow_mut().rail = RailLayout::from_json(&payload.to_string());
        cx.notify();
    }

    /// Which folders are open is the screen's business, not the account's.
    pub(super) fn rail_toggle(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.rail_open.remove(&id) {
            self.rail_open.insert(id);
        }
        let ids: Vec<&String> = self.rail_open.iter().collect();
        backend::device::set(OPEN_KEY, serde_json::to_string(&ids).unwrap_or_default());
        cx.notify();
    }

    pub(super) fn rail_drop(&mut self, source: RailDrag, target: RailDrop, cx: &mut Context<Self>) {
        let live = {
            let store = self.store.borrow();
            crate::rail::painted(&store, &store.rail)
        };
        if !self.live {
            let mut store = self.store.borrow_mut();
            let mut next = store.rail.merged_with(&live);
            match next.apply_drop(&source, &target, &vector_core::rail_layout::new_folder_id()) {
                Ok(()) => store.rail = next,
                Err(e) => {
                    drop(store);
                    self.show_toast(&e, cx);
                }
            }
            cx.notify();
            return;
        }
        self.rail_commit(backend::run(backend::rail::drop(source, target, live)), cx);
    }

    fn rail_edit(&mut self, f: impl FnOnce(&mut RailLayout) -> Result<(), String> + Send + 'static, cx: &mut Context<Self>) {
        if !self.live {
            let result = f(&mut self.store.borrow_mut().rail);
            if let Err(e) = result {
                self.show_toast(&e, cx);
            }
            cx.notify();
            return;
        }
        self.rail_commit(backend::run(backend::rail::edit(f)), cx);
    }

    fn rail_commit(&mut self, rx: futures::channel::oneshot::Receiver<Result<RailLayout, String>>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("Backend stopped".into()));
            this.update(cx, |this, cx| {
                match result {
                    Ok(layout) => this.store.borrow_mut().rail = layout,
                    Err(e) => this.show_toast(&e, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn rail_folder_menu(&mut self, folder: String, at: Point<Pixels>, cx: &mut Context<Self>) {
        let (hue, members) = {
            let store = self.store.borrow();
            let Some((hue, keys)) = store.rail.nodes.iter().find_map(|n| match n {
                RailNode::Folder { id, hue, keys, .. } if *id == folder => Some((*hue, keys.clone())),
                _ => None,
            }) else {
                return;
            };
            let unread: Vec<String> = keys
                .into_iter()
                .filter(|k| {
                    store.community(k).is_some_and(|c| {
                        c.channels
                            .iter()
                            .any(|ch| store.chat(&ch.channel_id).is_some_and(|chat| store.unread_count(chat) > 0))
                    })
                })
                .collect();
            (hue, unread)
        };
        let mut rows: Vec<(kit::MenuItem, MenuAct)> = Vec::new();
        if !members.is_empty() {
            rows.push((kit::MenuItem::new("Mark as Read", "check"), MenuAct::RailRead(members)));
            rows.push((kit::MenuItem::divider(), MenuAct::Nothing));
        }
        rows.push((kit::MenuItem::new("Rename Folder", "edit"), MenuAct::RailRename(folder.clone())));
        let mut items = vec![kit::MenuItem::new("Grey", "").swatch(crate::rail::tint(None)).checked(hue.is_none())];
        let mut acts = vec![MenuAct::RailHue(folder.clone(), None)];
        for (label, h) in crate::rail::HUES {
            items.push(kit::MenuItem::new(label, "").swatch(crate::rail::tint(Some(h))).checked(hue == Some(h)));
            acts.push(MenuAct::RailHue(folder.clone(), Some(h)));
        }
        rows.push((
            kit::MenuItem::new("Colour", "palette").sub(),
            MenuAct::Sub(SharedString::from("Colour"), items, acts),
        ));
        rows.push((kit::MenuItem::divider(), MenuAct::Nothing));
        rows.push((kit::MenuItem::new("Ungroup", "folder"), MenuAct::RailUngroup(folder)));
        self.show_menu(at, rows, cx);
    }

    pub(super) fn rail_menu_act(&mut self, act: MenuAct, window: &mut Window, cx: &mut Context<Self>) {
        match act {
            MenuAct::RailRead(ids) => {
                if self.live {
                    let rx = backend::run(async move {
                        for id in ids {
                            backend::community::mark_read(id).await;
                        }
                    });
                    cx.spawn(async move |this, cx| {
                        let _ = rx.await;
                        this.update(cx, |this, cx| this.refresh_unread(cx)).ok();
                    })
                    .detach();
                }
            }
            MenuAct::RailRename(folder) => self.ask_rail_rename(folder, window, cx),
            MenuAct::RailHue(folder, hue) => self.rail_edit(move |l| l.set_folder_hue(&folder, hue), cx),
            MenuAct::RailUngroup(folder) => self.rail_edit(move |l| l.dissolve_folder(&folder), cx),
            _ => {}
        }
    }

    fn ask_rail_rename(&mut self, folder: String, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.store.borrow().rail.nodes.iter().find_map(|n| match n {
            RailNode::Folder { id, name, .. } if *id == folder => Some(name.clone()),
            _ => None,
        });
        let Some(current) = current else { return };
        let placeholder = if current.is_empty() { "Folder Name".to_string() } else { current };
        let input = cx.new(|cx| {
            crate::composer::TextInput::new(placeholder, cx)
                .with_look(kit::field_look())
                .keep_on_submit()
                .submit_empty()
        });
        cx.subscribe_in(&input, window, |app, _, _: &crate::composer::SendText, window, cx| {
            app.answer_popup(true, window, cx)
        })
        .detach();
        cx.subscribe_in(&input, window, |app, _, _: &crate::composer::CancelInput, window, cx| {
            app.answer_popup(false, window, cx)
        })
        .detach();
        window.focus(&input.focus_handle(cx), cx);
        let mut popup = kit::Popup::notice("Rename Folder");
        popup.icon = None;
        popup.confirm = "Confirm".into();
        popup.cancel = Some("Cancel".into());
        popup.input = Some(input);
        self.popup = Some((popup, PopupAct::RailRename(folder)));
        cx.notify();
    }

    pub(super) fn rail_rename(&mut self, folder: String, name: String, cx: &mut Context<Self>) {
        self.rail_edit(move |l| l.rename_folder(&folder, &name), cx);
    }
}

