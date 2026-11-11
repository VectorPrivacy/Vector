//! The Moderation console and its Policies tab, on `VectorApp`.

use std::collections::HashSet;

use futures::StreamExt;
use gpui::prelude::*;
use gpui::{px, Context, FontWeight, Window};
use serde_json::{json, Value};

use super::{PopupAct, VectorApp};
use crate::backend;
use crate::composer::{CancelInput, InputChanged, TextInput};
use crate::kit;
use crate::moderation::{Act, Moderation, Tab};
use crate::policies::{self, Act as PAct, Draft, Rule};
use crate::theme::*;

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

impl VectorApp {
    pub(super) fn open_moderation(&mut self, community: String, window: &mut Window, cx: &mut Context<Self>) {
        let query = cx.new(|cx| {
            TextInput::new("Search members...", cx).with_look(crate::composer::Look { size: px(13.), line_height: px(16.), weight: FontWeight::NORMAL, placeholder: white_a(0.35), ..kit::field_look() })
        });
        cx.subscribe(&query, |_, _, _: &InputChanged, cx| cx.notify()).detach();
        cx.subscribe_in(&query, window, |app, _, _: &CancelInput, window, cx| app.on_moderation(Act::Close, window, cx)).detach();
        self.moderation = Some(Moderation {
            community,
            intel: None,
            loading: true,
            error: None,
            keep: HashSet::new(),
            filter: "all",
            query,
            busy: None,
            tab: Tab::Members,
            scroll: gpui::ScrollHandle::new(),
            policies: Default::default(),
        });
        self.load_moderation(false, cx);
        cx.notify();
    }

    fn load_moderation(&mut self, refresh: bool, cx: &mut Context<Self>) {
        let Some(m) = self.moderation.as_mut() else { return };
        let intel = if self.live { backend::community::moderation_intel(&m.community) } else { self.store.borrow().fixture_moderation.clone().ok_or_else(|| "No moderation data.".to_string()) };
        match intel {
            Ok(intel) => m.set_intel(intel, refresh),
            Err(e) => {
                m.loading = false;
                m.error = Some(e);
            }
        }
        let id = m.community.clone();
        self.check_raid(id, cx);
        cx.notify();
    }

    /// The menu entry's "N flagged" reads this cache; an action invalidates it.
    pub(super) fn check_raid(&mut self, community: String, cx: &mut Context<Self>) {
        if !self.live {
            let fix = self.store.borrow().fixture_moderation.clone();
            self.raid.insert(community, fix.map(|f| (true, f["report"]["suspects"].as_u64().unwrap_or(0) as usize)).unwrap_or((false, 0)));
            return cx.notify();
        }
        if !backend::community::capabilities(&community).contains("ban") {
            return;
        }
        let id = community.clone();
        let rx = backend::run(async move { backend::community::check_raid(&id) });
        cx.spawn(async move |this, cx| {
            let Ok(raid) = rx.await else { return };
            this.update(cx, |app, cx| {
                app.raid.insert(community, raid);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn on_moderation(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.moderation.as_mut() else { return };
        match act {
            Act::Close => {
                if m.busy.is_none() {
                    self.moderation = None;
                }
            }
            Act::Tab(tab) => {
                m.tab = tab;
                if tab == Tab::Policies && !m.policies.loaded {
                    self.load_policies(true, cx);
                }
            }
            Act::Filter(f) => m.filter = f,
            Act::Keep(npub) => {
                if !m.keep.remove(&npub) {
                    m.keep.insert(npub);
                }
            }
            Act::Revoke => {
                let n = m.invites();
                let mut popup = kit::Popup::notice("Revoke every invite link");
                popup.text = Some(format!("Retire all {}? Anyone holding one can no longer join. Existing members are unaffected.", plural(n, "public invite link")).into());
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                self.popup = Some((popup, PopupAct::ModRevoke));
            }
            Act::Rotate => {
                let cut = m.cut().len();
                let live = m.invites();
                let link = if live > 0 {
                    format!("\n\n{} still live. Anyone holding one can rejoin straight after this. Revoke them first.", if live == 1 { "1 invite link is".to_string() } else { format!("{live} invite links are") })
                } else {
                    String::new()
                };
                let (title, text) = if cut > 0 {
                    (
                        "Remove members and rotate",
                        format!(
                            "Remove {} from the community, then mint a new epoch only the {} remaining can follow. They are dropped from everyone's member list and lose access, without being banned.\n\nThis publishes one removal per member, so {cut} will take a while.",
                            plural(cut, "member"),
                            m.keep.len()
                        ),
                    )
                } else {
                    ("Rotate keys", "Mint a new epoch for everyone currently in the community. Use this when an invite link leaked but the members are all real.".to_string())
                };
                let mut popup = kit::Popup::notice(title);
                popup.text = Some(format!("{text}{link}").into());
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                self.popup = Some((popup, PopupAct::ModRotate));
            }
            Act::BanRotate => {
                let cut = m.cut().len();
                if cut == 0 {
                    return;
                }
                let mut popup = kit::Popup::notice("Ban and rotate");
                popup.text = Some(format!("Ban {} and rotate the keys around them. They're added to the banlist, stripped of any role, and cannot rejoin until unbanned.", plural(cut, "member")).into());
                popup.confirm = "Confirm".into();
                popup.cancel = Some("Cancel".into());
                self.popup = Some((popup, PopupAct::ModBanRotate));
            }
            Act::Policy(act) => self.on_policy(act, window, cx),
        }
        cx.notify();
    }

    fn mod_fail(&mut self, title: &str, err: String) {
        let mut popup = kit::Popup::notice(title.to_string());
        popup.text = Some(err.into());
        self.popup = Some((popup, PopupAct::Notice));
    }

    fn mod_busy(&mut self, label: Option<String>, cx: &mut Context<Self>) {
        if let Some(m) = self.moderation.as_mut() {
            m.busy = label.map(|l| (l, " Publishing. Leave this open until it finishes.".to_string()));
        }
        cx.notify();
    }

    fn mod_done(&mut self, cx: &mut Context<Self>) {
        self.mod_busy(None, cx);
        self.load_moderation(true, cx);
    }

    pub(super) fn mod_revoke(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.moderation.as_ref().map(|m| m.community.clone()) else { return };
        if !self.live {
            return;
        }
        self.mod_busy(Some("Revoking invite links…".into()), cx);
        let rx = backend::run(backend::community::revoke_all_invites(id));
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |app, cx| {
                match res {
                    Ok((revoked, 0)) => app.show_toast(&format!("Revoked {}", plural(revoked, "invite link")), cx),
                    Ok((revoked, failed)) => app.show_toast(&format!("Revoked {revoked}, {failed} failed"), cx),
                    Err(e) => app.mod_fail("Couldn't revoke", e),
                }
                app.mod_done(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Removes everyone unticked (or no one) and rotates; with `ban`, bans them first.
    pub(super) fn mod_rotate(&mut self, ban: bool, cx: &mut Context<Self>) {
        let Some(m) = self.moderation.as_ref() else { return };
        let (id, cut) = (m.community.clone(), m.cut());
        let retain: Vec<String> = if cut.is_empty() { Vec::new() } else { m.keep.iter().cloned().collect() };
        if !self.live || (ban && cut.is_empty()) {
            return;
        }
        let n = cut.len();
        self.mod_busy(Some(if ban { format!("Banning {n}…") } else if n > 0 { format!("Removing 0/{n}") } else { "Rotating keys…".into() }), cx);
        let (tx, mut progress) = futures::channel::mpsc::unbounded::<(usize, usize)>();
        let rx = backend::run(async move {
            if ban {
                backend::community::set_banned(id.clone(), cut, true).await.map_err(|e| (true, e))?;
            }
            backend::community::refound(id, retain, move |done, total| {
                let _ = tx.unbounded_send((done, total));
            })
            .await
            .map_err(|e| (false, e))
        });
        cx.spawn(async move |this, cx| {
            while let Some((done, total)) = progress.next().await {
                let pct = if total > 0 { done * 100 / total } else { 0 };
                let ok = this
                    .update(cx, |app, cx| {
                        if let Some(m) = app.moderation.as_mut() {
                            m.busy = Some((format!("Removing {done}/{total}"), format!(" {pct}% — leave this open until it finishes.")));
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !ok {
                    break;
                }
            }
            let res = rx.await.unwrap_or_else(|_| Err((false, "cancelled".into())));
            this.update(cx, |app, cx| {
                match res {
                    Ok(summary) if ban => {
                        let _ = summary;
                        app.show_toast(&format!("Banned {n} and rotated."), cx);
                    }
                    Ok(summary) => {
                        let refused = summary["refused"].as_u64().filter(|r| *r > 0).map(|r| format!(" {r} refused.")).unwrap_or_default();
                        let kicked = summary["kicked"].as_u64().map(|k| k as usize).unwrap_or(n);
                        app.show_toast(&if n > 0 { format!("Removed {kicked} and rotated.{refused}") } else { "Keys rotated.".into() }, cx);
                    }
                    Err((true, e)) => app.mod_fail("Couldn't ban", e),
                    Err((false, e)) if ban => {
                        let held = if n == 1 { "account is" } else { "accounts are" };
                        app.mod_fail(
                            "Banned, but the keys did NOT rotate",
                            format!("The {n} {held} banned and cannot post. They still hold the current key and can READ until a rotation lands. Use Rotate keys to finish.\n\n{e}"),
                        );
                    }
                    Err((false, e)) => app.mod_fail("Couldn't complete the removal", e),
                }
                app.mod_done(cx);
            })
            .ok();
        })
        .detach();
    }

    fn load_policies(&mut self, catalogue: bool, cx: &mut Context<Self>) {
        let Some(m) = self.moderation.as_mut() else { return };
        let p = &mut m.policies;
        if catalogue && p.presets.is_empty() {
            let cat = if self.live { backend::community::policy_presets().ok() } else { self.store.borrow().fixture_presets.clone() };
            if let Some(cat) = cat {
                p.presets = cat["presets"].as_array().cloned().unwrap_or_default();
                p.kinds = cat["rule_kinds"].as_array().cloned().unwrap_or_default();
            }
        }
        let list = if self.live { backend::community::policies(&m.community).ok() } else { Some(json!({ "policies": [], "using_builtin": true })) };
        if let Some(list) = list {
            p.stored = list["policies"].as_array().cloned().unwrap_or_default();
            p.using_builtin = list["using_builtin"].as_bool().unwrap_or(true);
        }
        p.loaded = true;
        cx.notify();
    }

    fn policy_input(&mut self, placeholder: &str, text: &str, multiline: bool, window: &mut Window, cx: &mut Context<Self>) -> gpui::Entity<TextInput> {
        let input = cx.new(|cx| {
            let look = crate::composer::Look { size: px(13.), line_height: px(19.5), weight: FontWeight::NORMAL, placeholder: c(0x757575), ..kit::field_look() };
            let mut t = TextInput::new(placeholder.to_string(), cx).with_look(look).keep_on_submit();
            if multiline {
                t = t.multiline(px(220.));
            }
            t.set_text(text, cx);
            t
        });
        cx.subscribe(&input, |app, _, _: &InputChanged, cx| {
            if let Some(m) = app.moderation.as_mut() {
                m.policies.dirty();
            }
            cx.notify();
        })
        .detach();
        cx.subscribe_in(&input, window, |app, _, _: &CancelInput, window, cx| app.on_moderation(Act::Close, window, cx)).detach();
        input
    }

    fn open_policy(&mut self, preset: Value, window: &mut Window, cx: &mut Context<Self>) {
        let policy: Value = preset["bytes"].as_str().and_then(|b| serde_json::from_str(b).ok()).unwrap_or(Value::Null);
        let id = preset["id"].as_str().unwrap_or("").to_string();
        let dials = preset["dials"].as_array().cloned().unwrap_or_default();
        let rules = policy["rules"].as_array().cloned().unwrap_or_default();
        let link = rules.iter().find(|r| r["match"]["type"] == "link");
        let rate = rules.iter().find(|r| r["match"]["type"] == "rate").and_then(|r| r["match"]["per_secs"].as_u64()).unwrap_or(10);
        let mut seeded = false;
        let mut inputs = std::collections::HashMap::new();
        for d in &dials {
            let (kind, key) = (d["kind"].as_str().unwrap_or(""), d["key"].as_str().unwrap_or(""));
            let text = match kind {
                "seconds" => rate.to_string(),
                "domainlist" if id == "vector_defaults" && key == "domains" && link.is_some() => {
                    seeded = true;
                    link.and_then(|l| l["match"]["patterns"].as_array()).into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join("\n")
                }
                "wordlist" | "domainlist" | "text" => String::new(),
                _ => continue,
            };
            let placeholder = match kind {
                "wordlist" => "one word per line",
                "domainlist" => "example.com",
                "text" => "Spoiler filter",
                _ => "",
            };
            inputs.insert(key.to_string(), self.policy_input(placeholder, &text, kind != "text" && kind != "seconds", window, cx));
        }
        let Some(m) = self.moderation.as_mut() else { return };
        let p = &mut m.policies;
        p.draft = Some(Draft {
            id,
            name: preset["name"].as_str().unwrap_or("").to_string(),
            caveat: preset["caveat"].as_str().unwrap_or("").to_string(),
            dials,
            summary: preset["rules"].as_array().cloned().unwrap_or_default(),
            policy,
            strictness: "balanced",
            inputs,
            channels: HashSet::new(),
            rules: Vec::new(),
            seeded,
        });
        p.busy = false;
        p.dirty();
    }

    fn on_policy(&mut self, act: PAct, window: &mut Window, cx: &mut Context<Self>) {
        let live = self.live;
        let Some(m) = self.moderation.as_mut() else { return };
        let community = m.community.clone();
        let p = &mut m.policies;
        match act {
            PAct::Inspect => {
                if let Some(d) = p.presets.iter().find(|x| x["id"] == "vector_defaults").cloned() {
                    self.open_policy(d, window, cx);
                }
            }
            PAct::Open(id) => {
                if let Some(d) = p.presets.iter().find(|x| x["id"].as_str() == Some(id.as_str())).cloned() {
                    self.open_policy(d, window, cx);
                }
            }
            PAct::Back => p.draft = None,
            PAct::Strictness(s) => {
                if let Some(d) = p.draft.as_mut() {
                    d.strictness = s;
                }
                p.dirty();
            }
            PAct::Channel(id) => {
                if let Some(d) = p.draft.as_mut() {
                    if !d.channels.remove(&id) {
                        d.channels.insert(id);
                    }
                }
                p.dirty();
            }
            PAct::AddRule(kind) => {
                let Some(k) = p.kind(&kind).cloned() else { return };
                let text = if k["input"] == "seconds" { k["rule"]["match"]["per_secs"].as_u64().unwrap_or(10).to_string() } else { String::new() };
                let placeholder = k["input_label"].as_str().unwrap_or("").to_string();
                let input = self.policy_input(&placeholder, &text, k["input"] != "seconds", window, cx);
                let Some(m) = self.moderation.as_mut() else { return };
                if let Some(d) = m.policies.draft.as_mut() {
                    d.rules.push(Rule { kind, input });
                }
                m.policies.dirty();
            }
            PAct::RemoveRule(i) => {
                if let Some(d) = p.draft.as_mut() {
                    if i < d.rules.len() {
                        d.rules.remove(i);
                    }
                }
                p.dirty();
            }
            PAct::Preview => {
                let doc = match policies::readiness(p, cx) {
                    Ok(doc) => doc,
                    Err(e) => {
                        p.preview_error = Some(e);
                        return;
                    }
                };
                let res = if live {
                    backend::community::preview_policy(&community, &doc.to_string())
                } else {
                    Ok(json!({ "valid": true, "flagged": [], "messages_cited": 0, "corpus": 0, "shielded_matches": [], "unevaluated": [] }))
                };
                match res {
                    Ok(r) if r["valid"].as_bool() == Some(false) => {
                        p.preview_error = Some(r["error"].as_str().unwrap_or("This policy is not valid.").to_string());
                        p.previewed = false;
                    }
                    Ok(r) => {
                        p.preview = Some(r);
                        p.preview_error = None;
                        p.previewed = true;
                    }
                    Err(e) => {
                        p.preview_error = Some(e);
                        p.previewed = false;
                    }
                }
                p.busy = false;
            }
            PAct::Save => {
                if !p.previewed {
                    return;
                }
                let Some(doc) = policies::compose(p, cx) else { return };
                let Some(d) = p.draft.as_ref() else { return };
                let id = if d.id == "blank" { policies::new_id(doc["name"].as_str().unwrap_or(""), &p.stored) } else { d.id.clone() };
                if live {
                    if let Err(e) = backend::community::set_policy(&community, &id, &doc.to_string(), true) {
                        self.mod_fail("Couldn't save the policy", e);
                        return;
                    }
                }
                if let Some(m) = self.moderation.as_mut() {
                    m.policies.draft = None;
                }
                self.load_policies(false, cx);
            }
            PAct::Toggle(id) => {
                let Some(s) = p.stored.iter().find(|s| s["policy_id"].as_str() == Some(id.as_str())) else { return };
                if live {
                    let _ = backend::community::set_policy(&community, &id, s["bytes"].as_str().unwrap_or(""), !s["enabled"].as_bool().unwrap_or(false));
                }
                self.load_policies(false, cx);
            }
            PAct::Remove(id) => {
                if live {
                    let _ = backend::community::delete_policy(&community, &id);
                }
                self.load_policies(false, cx);
            }
            PAct::Explain => {
                let mut popup = kit::Popup::notice("Sensitivity");
                popup.icon = None;
                popup.text = Some(policies::STRICTNESS_EXPLAINER.into());
                self.popup = Some((popup, PopupAct::Notice));
            }
        }
        cx.notify();
    }
}
