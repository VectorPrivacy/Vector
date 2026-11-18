//! A member's menu inside a community, wherever they're pressed: the member list's row or
//! their name and avatar in chat (`memberMenuItems`). Nickname, roles, then moderation,
//! offered only where the caller outranks them.

use gpui::{Context, Pixels, Point, SharedString, Window};

use super::{MenuAct, PopupAct, VectorApp, miniprofile_ops::member_role_rows};
use crate::{backend, kit, t};

const TIMEOUTS: [u64; 6] = [60, 300, 600, 3600, 86400, 604800];

/// In the largest unit that divides it, a minute aside: `60 seconds`, `5 minutes`, `1 hour`.
fn timeout_label(s: u64) -> SharedString {
    match s {
        _ if s % 604800 == 0 => t::time_weeks(s / 604800),
        _ if s % 86400 == 0 => t::time_days(s / 86400),
        _ if s % 3600 == 0 => t::time_hours(s / 3600),
        _ if s % 60 == 0 && s > 60 => t::time_minutes(s / 60),
        _ => t::time_seconds(s),
    }
}

impl VectorApp {
    fn member_rows(&self, community: &str, npub: &str) -> Vec<(kit::MenuItem, MenuAct)> {
        let (caps, view, timeouts) = if self.live {
            (
                backend::community::capabilities(community),
                backend::community::roles_view(community).ok(),
                backend::community::timeouts(community),
            )
        } else {
            let all = ["change_nickname", "manage_nicknames", "moderate_members", "kick", "ban"];
            (all.iter().map(|s| s.to_string()).collect(), self.store.borrow().fixture_roles.clone(), serde_json::Value::Null)
        };
        let store = self.store.borrow();
        let owner = store.community(community).and_then(|c| c.owner_npub.clone());
        let mine = npub == store.my_npub;
        let now = store.now_ms() / 1000;
        drop(store);
        let timed_out = timeouts[npub]["until"].as_u64().unwrap_or(0) > now;
        let above = !mine
            && view.as_ref().is_some_and(|v| {
                owner.as_deref() != Some(npub)
                    && (v["is_owner"].as_bool().unwrap_or(false)
                        || v["rank"].as_i64().is_some_and(|r| r < rank(v, npub)))
            });
        let mut rows: Vec<(kit::MenuItem, MenuAct)> = Vec::new();
        let nick = if mine { caps.contains("change_nickname") } else { caps.contains("manage_nicknames") && above };
        if nick {
            rows.push((
                kit::MenuItem::new(t::member_nickname(), "edit"),
                MenuAct::MemberNickname(community.to_string(), npub.to_string()),
            ));
        }
        if let Some(v) = view.as_ref().filter(|v| offered(v, owner.as_deref(), npub)) {
            let (items, acts) = member_role_rows(v, community, npub, self.role_busy.as_deref()).into_iter().unzip();
            rows.push((kit::MenuItem::new(t::member_roles(), "").sub(), MenuAct::Sub(t::member_roles(), items, acts)));
        }
        let mut moderation: Vec<(kit::MenuItem, MenuAct)> = Vec::new();
        let act = |seconds| MenuAct::MemberTimeout(community.to_string(), npub.to_string(), seconds);
        if above && caps.contains("moderate_members") {
            let mut items: Vec<kit::MenuItem> = TIMEOUTS.iter().map(|s| kit::MenuItem::new(timeout_label(*s), "")).collect();
            let mut acts: Vec<MenuAct> = TIMEOUTS.iter().map(|s| act(*s)).collect();
            if timed_out {
                items.push(kit::MenuItem::divider());
                acts.push(MenuAct::Nothing);
                items.push(kit::MenuItem::new(t::member_timeout_lift(), ""));
                acts.push(act(0));
            }
            moderation.push((kit::MenuItem::new(t::member_timeout(), "clock").sub(), MenuAct::Sub(t::member_timeout(), items, acts)));
        }
        if above && caps.contains("kick") {
            moderation.push((
                kit::MenuItem::new(t::member_kick(), "x").hint(Some(t::member_kick_hint().to_string())),
                MenuAct::MemberRemove(community.to_string(), npub.to_string(), false),
            ));
        }
        if above && caps.contains("ban") {
            moderation.push((
                kit::MenuItem::new(t::member_ban(), "x-user").hint(Some(t::member_ban_hint().to_string())).danger(),
                MenuAct::MemberRemove(community.to_string(), npub.to_string(), true),
            ));
        }
        if !moderation.is_empty() {
            if !rows.is_empty() {
                rows.push((kit::MenuItem::divider(), MenuAct::Nothing));
            }
            rows.extend(moderation);
        }
        if self.advanced() && !self.store.borrow().stream_hidden(npub) {
            let mut lead = vec![(kit::MenuItem::new(t::copy_id_user(), "channel-hash"), MenuAct::CopyId(npub.to_string(), t::copy_id_user_done()))];
            if !rows.is_empty() {
                lead.push((kit::MenuItem::divider(), MenuAct::Nothing));
            }
            rows.splice(0..0, lead);
        }
        rows
    }

    /// The member list's row menu.
    pub(super) fn member_menu(&mut self, community: String, npub: String, at: Point<Pixels>, cx: &mut Context<Self>) {
        let rows = self.member_rows(&community, &npub);
        if !rows.is_empty() {
            self.show_menu(at, rows, cx);
        }
    }

    /// A name or avatar right-clicked in chat: in a community, View Profile over the member's
    /// menu; elsewhere, their mini profile.
    pub(super) fn author_menu(&mut self, npub: String, key: gpui::SharedString, at: Point<Pixels>, cx: &mut Context<Self>) {
        let anchor = self.anchors.borrow().get(&key).copied();
        let community = {
            let store = self.store.borrow();
            self.open.as_ref().and_then(|id| store.community_of(id)).map(|c| c.community_id.clone())
        };
        let Some(community) = community else {
            return self.show_mini(npub, None, anchor, cx);
        };
        let items = self.member_rows(&community, &npub);
        let mut rows = vec![(kit::MenuItem::new(t::member_view_profile(), "user-circle"), MenuAct::MemberMini(npub, community, key))];
        if !items.is_empty() {
            rows.push((kit::MenuItem::divider(), MenuAct::Nothing));
            rows.extend(items);
        }
        self.show_menu(at, rows, cx);
    }

    pub(super) fn member_menu_act(&mut self, act: MenuAct, window: &mut Window, cx: &mut Context<Self>) {
        match act {
            MenuAct::MemberNickname(community, npub) => self.ask_community_nickname(community, npub, window, cx),
            MenuAct::MemberMini(npub, community, key) => {
                let at = self.anchors.borrow().get(&key).copied();
                self.show_mini(npub, Some(community), at, cx);
            }
            MenuAct::MemberTimeout(community, npub, seconds) => {
                if !self.live {
                    return;
                }
                let name = self.store.borrow().name_in(&npub, Some(&community));
                let rx = backend::run(backend::community::timeout(community, npub, seconds));
                cx.spawn(async move |this, cx| {
                    let res = rx.await;
                    this.update(cx, |this, cx| match res {
                        Ok(Ok(())) => this.show_toast(
                            &if seconds > 0 { t::member_timed_out(&name) } else { t::member_timeout_lifted(&name) },
                            cx,
                        ),
                        Ok(Err(e)) => this.show_toast(&e, cx),
                        Err(_) => {}
                    })
                    .ok();
                })
                .detach();
            }
            MenuAct::MemberRemove(community, npub, ban) => {
                let name = crate::kit::escape(&self.store.borrow().name_in(&npub, Some(&community)));
                let mut p = kit::Popup::notice(if ban { t::member_ban_title() } else { t::member_kick_title() });
                p.icon = Some("icons/vector_warning.svg");
                p.text = Some(if ban { t::member_ban_text(&name) } else { t::member_kick_text(&name) });
                p.confirm = t::dialog_confirm();
                p.cancel = Some(t::dialog_cancel());
                self.popup = Some((p, PopupAct::RemoveMember { community, npub, ban }));
            }
            _ => {}
        }
        cx.notify();
    }

    pub(super) fn remove_member(&mut self, community: String, npub: String, ban: bool, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        let rx = if ban {
            backend::run(backend::community::set_banned(community.clone(), vec![npub.clone()], true))
        } else {
            backend::run(backend::community::kick(community.clone(), npub.clone()))
        };
        cx.spawn(async move |this, cx| {
            let res = rx.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(Ok(())) => {
                        if let Some(v) = this.community.as_mut().filter(|v| v.id == community) {
                            v.members.retain(|m| *m != npub);
                        }
                        this.reload_communities(cx);
                    }
                    // A private community's ban can fail with the long bunker explanation.
                    Ok(Err(e)) if ban => {
                        let mut p = kit::Popup::notice(t::member_ban_failed());
                        p.icon = Some("icons/vector_warning.svg");
                        p.text = Some(crate::kit::escape(&e).into());
                        this.popup = Some((p, PopupAct::Notice));
                    }
                    Ok(Err(e)) => this.show_toast(&e, cx),
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// A member's standing: the highest (lowest-numbered) position among the roles they hold.
fn rank(view: &serde_json::Value, npub: &str) -> i64 {
    view["roles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["holders"].as_array().is_some_and(|h| h.iter().any(|x| x.as_str() == Some(npub))))
        .filter_map(|r| r["position"].as_i64())
        .min()
        .unwrap_or(i64::MAX)
}

/// `memberRolesOffered`: the role menu has something to offer for `npub`.
fn offered(view: &serde_json::Value, owner: Option<&str>, npub: &str) -> bool {
    crate::miniprofile::can_edit(view, owner, npub)
        && view["roles"].as_array().is_some_and(|r| r.iter().any(|r| r["manageable"].as_bool().unwrap_or(false)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn timeout_choices_read_in_their_largest_unit() {
        let got: Vec<String> = super::TIMEOUTS.iter().map(|s| super::timeout_label(*s).to_string()).collect();
        assert_eq!(got, ["60 seconds", "5 minutes", "10 minutes", "1 hour", "1 day", "1 week"]);
    }
}
