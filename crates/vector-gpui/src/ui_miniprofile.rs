//! The mini profile's lifecycle: opening it beside a tap, its roles, and taking or giving one.

use gpui::{Bounds, Context, Pixels, Window};

use super::{MenuAct, VectorApp};
use crate::miniprofile::{Act, Mini};
use crate::{backend, kit};

impl VectorApp {
    /// `showMiniProfile`: within a community (`community`, else the open channel's) it lists
    /// their roles there.
    pub(super) fn show_mini(&mut self, npub: String, community: Option<String>, at: Option<Bounds<Pixels>>, cx: &mut Context<Self>) {
        let community = community.or_else(|| {
            let store = self.store.borrow();
            self.open.as_ref().and_then(|id| store.community_of(id)).map(|c| c.community_id.clone())
        });
        self.menu = None;
        self.mini_profile = Some(Mini {
            npub: npub.clone(),
            community,
            at,
            opened: std::time::Instant::now(),
            roles: None,
            busy: None,
        });
        self.mini_roles();
        if self.live {
            backend::prefs::refresh(npub);
        }
        // The Anon fallback lands without an event to repaint it.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(6)).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
        cx.notify();
    }

    fn roles_view(&self, community: &str) -> Option<serde_json::Value> {
        if self.live {
            backend::community::roles_view(community).ok()
        } else {
            self.store.borrow().fixture_roles.clone()
        }
    }

    /// `loadMiniProfileRoles`.
    fn mini_roles(&mut self) {
        let Some((npub, community)) = self.mini_profile.as_ref().and_then(|m| Some((m.npub.clone(), m.community.clone()?))) else {
            return;
        };
        let view = self.roles_view(&community);
        let owner = self.store.borrow().community(&community).and_then(|c| c.owner_npub.clone());
        if let Some(m) = self.mini_profile.as_mut() {
            m.roles = view.map(|v| crate::miniprofile::roles(&v, owner.as_deref(), &npub));
        }
    }

    pub(super) fn on_mini(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        match act {
            Act::Close => self.mini_profile = None,
            Act::Message(npub) => {
                self.mini_profile = None;
                self.open_chat(npub.into(), window, cx);
            }
            Act::View(npub) => {
                self.mini_profile = None;
                self.open_profile(npub, cx);
            }
            Act::RemoveRole(role) => self.mini_set_role(role, false, cx),
            Act::AddRole(at) => self.mini_role_menu(at, cx),
        }
        cx.notify();
    }

    /// `openMemberRoleMenu`: every role the caller may hand out, ticked where held.
    fn mini_role_menu(&mut self, at: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(m) = self.mini_profile.as_ref() else { return };
        let Some(community) = m.community.clone() else { return };
        let npub = m.npub.clone();
        let Some(view) = self.roles_view(&community) else { return };
        let mut rows = vec![(kit::MenuItem::header("Roles"), MenuAct::Nothing)];
        rows.extend(member_role_rows(&view, &community, &npub, self.role_busy.as_deref()));
        self.show_menu(at, rows, cx);
    }

    fn mini_set_role(&mut self, role: String, on: bool, cx: &mut Context<Self>) {
        let Some((community, npub)) = self.mini_profile.as_ref().and_then(|m| Some((m.community.clone()?, m.npub.clone()))) else {
            return;
        };
        self.member_role(community, npub, role, on, cx);
    }

    /// `setMemberRole`: one change at a time; the open menu and the mini profile repaint.
    pub(super) fn member_role(&mut self, community: String, npub: String, role: String, on: bool, cx: &mut Context<Self>) {
        if !self.live || self.role_busy.is_some() {
            return;
        }
        self.role_busy = Some(role.clone());
        self.member_role_repaint(&community, &npub);
        let rx = backend::run(backend::community::set_member_role(community.clone(), npub.clone(), role, on));
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                this.role_busy = None;
                if let Ok(Err(e)) = res {
                    this.show_toast(&e, cx);
                }
                this.member_role_repaint(&community, &npub);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn member_role_repaint(&mut self, community: &str, npub: &str) {
        if self.mini_profile.as_ref().is_some_and(|m| m.npub == npub) {
            if let Some(m) = self.mini_profile.as_mut() {
                m.busy = self.role_busy.clone();
            }
            self.mini_roles();
        }
        let Some(view) = self.roles_view(community) else { return };
        let busy = self.role_busy.clone();
        let Some((_, items, acts)) = self.menu.as_mut() else { return };
        let ours = acts.iter().any(|a| matches!(a, MenuAct::MemberRole { npub: n, .. } if n == npub));
        if !ours {
            return;
        }
        // The row back out of a submenu, or the Roles header, stays on top.
        let lead = items
            .first()
            .is_some_and(|i| i.kind == kit::MenuKind::Back || (i.kind == kit::MenuKind::Header && i.label.as_ref() == "Roles"));
        let rows = member_role_rows(&view, community, npub, busy.as_deref());
        items.truncate(lead as usize);
        acts.truncate(lead as usize);
        for (i, a) in rows {
            items.push(i);
            acts.push(a);
        }
    }
}

/// `memberRoleItems`: what the caller may hand out, the held ones above them shown fixed;
/// channel roles under their own header, since holding one is what opens a private channel.
pub(super) fn member_role_rows(view: &serde_json::Value, community: &str, npub: &str, busy: Option<&str>) -> Vec<(kit::MenuItem, MenuAct)> {
    let channels = view["channels"].as_array().cloned().unwrap_or_default();
    let channel = |id: &str| {
        channels
            .iter()
            .find(|c| c["channel_id"].as_str() == Some(id))
            .and_then(|c| c["name"].as_str())
            .unwrap_or_default()
            .to_string()
    };
    let item = |r: &serde_json::Value| {
        let id = r["role_id"].as_str().unwrap_or_default().to_string();
        let name = r["name"].as_str().unwrap_or_default().to_string();
        let held = r["holders"].as_array().is_some_and(|h| h.iter().any(|x| x.as_str() == Some(npub)));
        let ch = r["channel_id"].as_str().map(channel).unwrap_or_default();
        let hint = (!ch.is_empty() && ch != name).then(|| format!("#{ch}"));
        let manageable = r["manageable"].as_bool().unwrap_or(false);
        (
            kit::MenuItem::new(name, "")
                .hint(hint)
                .swatch(crate::miniprofile::tint(r["color"].as_u64()))
                .checked(held)
                .disabled(!manageable || busy.is_some()),
            MenuAct::MemberRole {
                community: community.to_string(),
                npub: npub.to_string(),
                role: id,
                on: !held,
            },
        )
    };
    let shown: Vec<&serde_json::Value> = view["roles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| {
            r["manageable"].as_bool().unwrap_or(false)
                || r["holders"].as_array().is_some_and(|h| h.iter().any(|x| x.as_str() == Some(npub)))
        })
        .collect();
    let mut rows: Vec<_> = shown.iter().filter(|r| r["channel_id"].is_null()).map(|r| item(r)).collect();
    let channel_rows: Vec<_> = shown.iter().filter(|r| !r["channel_id"].is_null()).map(|r| item(r)).collect();
    if !channel_rows.is_empty() {
        if !rows.is_empty() {
            rows.push((kit::MenuItem::divider(), MenuAct::Nothing));
        }
        rows.push((kit::MenuItem::header("Private channels"), MenuAct::Nothing));
        rows.extend(channel_rows);
    }
    rows
}
