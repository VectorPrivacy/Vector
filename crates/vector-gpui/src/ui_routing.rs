//! `transport.js` and Tor's half of `settings.js`: the network view the Routing card, the notice
//! and the welcome screen read, and every action Routing takes on it.

use std::time::{Duration, Instant};

use gpui::{AppContext, Context, Focusable, Window};
use serde_json::Value;

use super::{PopupAct, VectorApp};
use crate::backend::{self, transport as net};
use crate::composer::{CancelInput, SendText, TextInput};
use crate::kit;
use crate::routing::{Load, NetAct, Routing};

/// While a switch runs or a network starts, the view is re-read this often.
const FAST: Duration = Duration::from_millis(1500);
/// While I2P's panel is on screen: outproxy health and the Clearnet step move with each connection.
const WATCH: Duration = Duration::from_secs(5);

const LINKS: &[(&str, &str)] = &[
    ("torAttribution", "https://torproject.org"),
    ("i2p", "https://i2p.net"),
    ("bridges", "https://bridges.torproject.org/bridges/en?transport=obfs4"),
];

fn json<T: serde::Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn port_look() -> crate::composer::Look {
    crate::composer::Look {
        family: crate::markdown::MONO,
        weight: gpui::FontWeight::NORMAL,
        size: gpui::px(13.),
        line_height: gpui::px(18.),
        color: crate::theme::c(0xffffff),
        placeholder: crate::theme::white_a(0.25),
        ..Default::default()
    }
}

fn field_look(mono: bool, masked: bool) -> crate::composer::Look {
    crate::composer::Look {
        family: if mono { crate::markdown::MONO } else { crate::theme::RUBIK },
        weight: gpui::FontWeight::NORMAL,
        size: gpui::px(if mono { 12. } else { 13. }),
        line_height: gpui::px(18.),
        color: crate::theme::c(0xffffff),
        placeholder: crate::theme::white_a(0.25),
        masked,
        ..Default::default()
    }
}

impl VectorApp {
    fn routing(&mut self) -> Option<&mut Routing> {
        self.settings.as_mut().map(|s| &mut s.net)
    }

    /// The live view, or the fixtures' demo one.
    pub(super) fn net_initial(&self) -> Option<Value> {
        if self.live {
            Some(json(net::transport_get_state()))
        } else {
            Some(self.store.borrow().fixture_net.as_ref().and_then(|n| n.get("view").cloned()).unwrap_or_else(|| {
                serde_json::json!({ "kind": "clearnet", "supported": ["clearnet"], "phase": "direct", "ready": true, "steps": [] })
            }))
        }
    }

    /// `applyTransportView`: take a view the backend sent or answered with.
    pub(super) fn on_transport_view(&mut self, view: Value, cx: &mut Context<Self>) {
        if view.get("kind").and_then(Value::as_str).is_none_or(str::is_empty) {
            return;
        }
        let seq = view.get("config_seq").and_then(Value::as_u64);
        let config_moved = seq != self.net_seq;
        let flipped = self.net_view.as_ref().is_none_or(|p| p["kind"] != view["kind"] || p["ready"] != view["ready"]);
        self.net_seq = seq;
        self.net_view = Some(view.clone());
        let tor_up = view.get("kind").and_then(Value::as_str) == Some("tor") && view.get("ready").and_then(Value::as_bool) == Some(true);
        if let Some(r) = self.routing() {
            r.view = Some(view.clone());
            r.at = Instant::now();
            if !tor_up {
                r.circuits = Default::default();
            }
        }
        if config_moved {
            self.load_net_config(cx);
        } else if flipped {
            self.check_stranded(cx);
        }
        if let Some(at) = self.net_renewed {
            let in_use = view.get("kind").and_then(Value::as_str) == Some("i2p") && view.get("ready").and_then(Value::as_bool) == Some(true);
            if in_use || at.elapsed() > Duration::from_secs(180) {
                self.net_renewed = None;
                if in_use {
                    self.show_toast("New I2P address in use.", cx);
                }
            }
        }
        if tor_up && self.routing().is_some_and(|r| r.circuits.phase == Load::Idle && !r.engine.external && r.viewed() == "tor") {
            self.load_circuits(false, false, cx);
        }
        if view.get("phase").and_then(Value::as_str) == Some("starting") {
            self.net_poll(cx);
        }
        cx.notify();
    }

    fn fetch_net_view(&mut self, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        let view = json(net::transport_get_state());
        self.on_transport_view(view, cx);
    }

    /// Re-read the view while a switch runs, a network starts, or I2P's panel shows.
    pub(super) fn net_poll(&mut self, cx: &mut Context<Self>) {
        if !self.live || self.net_polling {
            return;
        }
        self.net_polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                let wait = this
                    .update(cx, |app, _| {
                        let starting = app.net_view.as_ref().and_then(|v| v.get("phase")).and_then(Value::as_str) == Some("starting");
                        let r = app.settings.as_ref().map(|s| &s.net);
                        let locked = r.is_some_and(|r| r.locked);
                        let watching = r.is_some_and(|r| r.viewed() == "i2p" && r.in_use() == "i2p");
                        let counting = r.is_some_and(|r| r.viewed() == "tor" && r.multi && r.circuits.phase == Load::Ok);
                        if locked || starting {
                            Some(FAST)
                        } else if watching || counting {
                            Some(WATCH)
                        } else {
                            app.net_polling = false;
                            None
                        }
                    })
                    .ok()
                    .flatten();
                let Some(wait) = wait else { return };
                cx.background_executor().timer(wait).await;
                let alive = this
                    .update(cx, |app, cx| {
                        app.fetch_net_view(cx);
                        if app.routing().is_some_and(|r| r.viewed() == "tor" && r.multi && r.circuits.phase == Load::Ok) {
                            app.load_circuits(false, true, cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    return;
                }
            }
        })
        .detach();
    }

    /// Settings opened: the view, I2P's settings and Tor's, and the inputs Routing types into.
    pub(super) fn load_routing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.net_view.clone().or_else(|| self.net_initial());
        let Some(r) = self.routing() else { return };
        r.view = view;
        r.at = Instant::now();
        let port = cx.new(|cx| TextInput::new("", cx).with_look(port_look()).centered().max_chars(5).keep_on_submit());
        cx.subscribe_in(&port, window, |app, _, _: &SendText, window, cx| app.net_act(NetAct::SaveRouter, window, cx)).detach();
        cx.subscribe_in(&port, window, |app, input, _: &CancelInput, _, cx| {
            let saved = app.routing().and_then(|r| r.config.as_ref()).and_then(|c| c.get("sam_port")).and_then(Value::as_u64);
            if let Some(p) = saved {
                input.update(cx, |i, cx| i.set_text(&p.to_string(), cx));
            }
        })
        .detach();
        cx.observe(&port, |app, input, cx| {
            let text = input.read(cx).text().to_string();
            let digits: String = text.chars().filter(char::is_ascii_digit).collect();
            if digits != text {
                input.update(cx, |i, cx| i.set_text(&digits, cx));
                return;
            }
            let Some(r) = app.routing() else { return };
            let saved = r.config.as_ref().and_then(|c| c.get("sam_port")).and_then(Value::as_u64).map(|p| p.to_string());
            let dirty = saved.is_some_and(|s| s != digits);
            if dirty != r.port_dirty {
                r.port_dirty = dirty;
                r.note = None;
            }
            cx.notify();
        })
        .detach();
        r.port = Some(port);
        self.load_net_config(cx);
        self.load_tor_settings(window, cx);
        self.net_poll(cx);
    }

    /// I2P's settings in effect.
    fn load_net_config(&mut self, cx: &mut Context<Self>) {
        let config = if self.live { Some(json(net::i2p_get_config())) } else { self.store.borrow().fixture_i2p.clone() };
        self.net_config = config.clone();
        self.check_stranded(cx);
        let Some(r) = self.routing() else { return };
        r.config = config.clone();
        let port = config.as_ref().and_then(|c| c.get("sam_port")).and_then(Value::as_u64);
        if let (Some(input), Some(p), false) = (r.port.clone(), port, r.port_dirty) {
            input.update(cx, |i, cx| i.set_text(&p.to_string(), cx));
        }
        if let Some(r) = self.routing() {
            r.port_dirty = false;
        }
        cx.notify();
    }

    fn load_tor_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (state, bridges, obfs4) = if self.live {
            (json(net::tor_get_state()), json(net::tor_get_bridges()), json(net::tor_check_obfs4_proxy()))
        } else {
            let fx = self.store.borrow().fixture_net.clone().unwrap_or(Value::Null);
            (fx.get("tor").cloned().unwrap_or(Value::Null), serde_json::json!({ "enabled": false, "lines": "" }), serde_json::json!({ "installed": false }))
        };
        let lines = bridges.get("lines").and_then(Value::as_str).unwrap_or("").to_string();
        let input = cx.new(|cx| {
            let mut i = TextInput::new("obfs4 1.2.3.4:443 ABCD... cert=... iat-mode=0", cx).with_look(field_look(true, false)).multiline(gpui::px(160.));
            i.set_text(&lines, cx);
            i
        });
        cx.observe(&input, |app, input, cx| {
            let text = input.read(cx).text().to_string();
            if let Some(r) = app.routing() {
                r.bridges_dirty = text != r.bridges.saved;
                r.bridges.obfs4_missing = r.bridges.no_obfs4 && text.lines().any(|l| l.trim().to_ascii_lowercase().starts_with("obfs4 "));
                r.bridges.status = None;
            }
            cx.notify();
        })
        .detach();
        let Some(r) = self.routing() else { return };
        r.multi = state.get("multi_circuit").and_then(Value::as_bool).unwrap_or(true);
        r.bridges.enabled = bridges.get("enabled").and_then(Value::as_bool).unwrap_or(false);
        r.bridges.saved = lines;
        r.bridges.no_obfs4 = !obfs4.get("installed").and_then(Value::as_bool).unwrap_or(false);
        r.bridge_lines = Some(input);
        self.load_tor_engine(window, cx);
        if self.routing().is_some_and(|r| r.in_use() == "tor" && r.view.as_ref().and_then(|v| v.get("ready")).and_then(Value::as_bool) == Some(true)) {
            self.load_circuits(false, false, cx);
        }
    }

    fn load_tor_engine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.routing().is_some_and(|r| r.tor_port.is_none()) {
            let input = cx.new(|cx| TextInput::new("Auto", cx).with_look(port_look()).centered().max_chars(5).keep_on_submit());
            cx.subscribe_in(&input, window, |app, _, _: &SendText, window, cx| app.net_act(NetAct::SaveTorPort, window, cx)).detach();
            cx.observe(&input, |app, input, cx| {
                let text = input.read(cx).text().to_string();
                let digits: String = text.chars().filter(char::is_ascii_digit).collect();
                if digits != text {
                    input.update(cx, |i, cx| i.set_text(&digits, cx));
                    return;
                }
                if let Some(r) = app.routing() {
                    let saved = r.engine.port.map(|p| p.to_string()).unwrap_or_default();
                    r.tor_port_dirty = digits != saved;
                }
                cx.notify();
            })
            .detach();
            if let Some(r) = self.routing() {
                r.tor_port = Some(input);
            }
        }
        if !self.live {
            return;
        }
        let rx = backend::run(net::tor_get_engine());
        cx.spawn(async move |this, cx| {
            let Ok(e) = rx.await else { return };
            this.update(cx, |app, cx| app.set_engine(e.external, e.port, e.found, cx)).ok();
        })
        .detach();
    }

    fn set_engine(&mut self, external: bool, port: Option<u16>, found: Option<u16>, cx: &mut Context<Self>) {
        let Some(r) = self.routing() else { return };
        r.engine.external = external;
        r.engine.port = port;
        r.engine.found = found;
        r.tor_port_dirty = false;
        if let Some(input) = r.tor_port.clone() {
            input.update(cx, |i, cx| i.set_text(&port.map(|p| p.to_string()).unwrap_or_default(), cx));
        }
        if let Some(r) = self.routing() {
            r.tor_port_dirty = false;
        }
        cx.notify();
    }

    /// `loadTorCircuits`; `quiet` re-reads the count without a loading state.
    fn load_circuits(&mut self, force_new: bool, quiet: bool, cx: &mut Context<Self>) {
        if !self.live {
            let c = self.store.borrow().fixture_net.as_ref().and_then(|n| n.get("circuits").cloned()).unwrap_or(Value::Null);
            if let Some(r) = self.routing() {
                apply_circuits(r, &c);
            }
            return;
        }
        let Some(r) = self.routing() else { return };
        if r.circuits.phase == Load::Loading {
            return;
        }
        if !quiet {
            r.circuits.phase = Load::Loading;
        }
        let rx = backend::run(net::tor_get_circuits(Some(force_new)));
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |app, cx| {
                let Some(r) = app.routing() else { return };
                match res {
                    Ok(c) => apply_circuits(r, &json(c)),
                    Err(e) if !quiet => r.circuits.phase = Load::Error(e),
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn lock_net(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(r) = self.routing() {
            r.locked = on;
        }
        if on {
            self.net_poll(cx);
        }
        cx.notify();
    }

    /// Run a backend step that answers with a new view, under the lock; `then` follows on the UI.
    fn net_call<F, T>(&mut self, fut: F, cx: &mut Context<Self>, then: impl FnOnce(&mut VectorApp, Result<T, String>, &mut Context<VectorApp>) + 'static)
    where
        F: std::future::Future<Output = Result<T, String>> + Send + 'static,
        T: Send + 'static,
    {
        self.lock_net(true, cx);
        let rx = backend::run(fut);
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |app, cx| {
                app.lock_net(false, cx);
                then(app, res, cx);
                app.fetch_net_view(cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn net_act(&mut self, act: NetAct, window: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
        match act {
            NetAct::View(kind) => {
                let Some(r) = self.routing() else { return };
                let kinds = r.kinds();
                r.thumb_from = kinds.iter().position(|k| *k == r.viewed()).unwrap_or(0);
                r.viewed = kind;
                if self.routing().is_some_and(|r| r.viewed() == "tor") {
                    let tor_up = self.net_view.as_ref().is_some_and(|v| v["kind"] == "tor" && v["ready"] == true);
                    if tor_up {
                        self.load_circuits(false, false, cx);
                    }
                }
                self.net_poll(cx);
            }
            NetAct::Help(key) => {
                if key == "tor" || key == "i2p" {
                    self.open_net_info(key, cx);
                } else {
                    self.settings_act(crate::settings::Act::Help(key), window, cx);
                }
            }
            NetAct::Link(key) => {
                if let Some((_, url)) = LINKS.iter().find(|(k, _)| *k == key) {
                    cx.open_url(url);
                }
            }
            NetAct::Use(kind) => self.use_network(kind, None, false, cx),
            NetAct::Retry => {
                if !self.live || self.routing().is_some_and(|r| r.locked) {
                    return;
                }
                self.net_call(net::transport_retry(None), cx, |app, res, cx| match res {
                    Ok(v) => app.on_transport_view(json(v), cx),
                    Err(e) => app.show_toast(&e, cx),
                });
            }
            NetAct::Renew => {
                if !self.live {
                    return;
                }
                if let Some(r) = self.routing() {
                    r.renewing = true;
                }
                let rx = backend::run(net::transport_retry(Some(true)));
                cx.spawn(async move |this, cx| {
                    let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
                    this.update(cx, |app, cx| {
                        if let Some(r) = app.routing() {
                            r.renewing = false;
                        }
                        match res {
                            Ok(v) => {
                                app.on_transport_view(json(v), cx);
                                app.net_renewed = Some(Instant::now());
                            }
                            Err(e) => app.show_toast(&e, cx),
                        }
                    })
                    .ok();
                })
                .detach();
            }
            NetAct::TorEngine(external) => {
                if !self.live {
                    return;
                }
                let port = self.routing().and_then(|r| r.engine.port).map(i64::from);
                self.choose_engine(external, port, cx);
            }
            NetAct::SaveTorPort => {
                let Some(input) = self.routing().and_then(|r| r.tor_port.clone()) else { return };
                let text = input.read(cx).text().to_string();
                let p: i64 = text.parse().unwrap_or(0);
                if p > 65535 || !self.live {
                    return;
                }
                self.choose_engine(true, (p > 0).then_some(p), cx);
            }
            NetAct::FindTor => self.load_tor_engine(window, cx),
            NetAct::TorBridges(enabled) => self.set_bridges(Some(enabled), cx),
            NetAct::ApplyBridges => self.set_bridges(None, cx),
            NetAct::TorMulti(on) => {
                if let Some(r) = self.routing() {
                    r.multi = on;
                }
                if !self.live {
                    return;
                }
                self.net_call(net::tor_set_multi_circuit(on), cx, |app, res, cx| {
                    match res {
                        Ok(st) => {
                            if let Some(r) = app.routing() {
                                r.multi = st.multi_circuit;
                            }
                        }
                        Err(_) => {
                            let st = net::tor_get_state();
                            if let Some(r) = app.routing() {
                                r.multi = st.multi_circuit;
                            }
                        }
                    }
                    if app.net_view.as_ref().is_some_and(|v| v["kind"] == "tor" && v["ready"] == true) {
                        app.load_circuits(false, false, cx);
                    }
                });
            }
            NetAct::NewCircuit => self.load_circuits(true, false, cx),
            NetAct::TestRouter => self.test_router(cx),
            NetAct::SaveRouter => self.save_router(cx),
            NetAct::AuthOpen => {
                let user = cx.new(|cx| TextInput::new("", cx).with_look(field_look(false, false)).keep_on_submit());
                let pass = cx.new(|cx| TextInput::new("", cx).with_look(field_look(false, true)).keep_on_submit());
                for i in [&user, &pass] {
                    cx.subscribe_in(i, window, |app, _, _: &SendText, window, cx| app.net_act(NetAct::AuthSave, window, cx)).detach();
                    cx.observe(i, |_, _, cx| cx.notify()).detach();
                }
                window.focus(&user.focus_handle(cx), cx);
                if let Some(r) = self.routing() {
                    r.auth = Some((user, pass));
                    r.auth_error.clear();
                }
            }
            NetAct::AuthCancel => {
                if let Some(r) = self.routing() {
                    r.auth = None;
                    r.auth_error.clear();
                }
            }
            NetAct::AuthSave => {
                let Some((user, pass)) = self.routing().and_then(|r| r.auth.clone()) else { return };
                let (u, p) = (user.read(cx).text().to_string(), pass.read(cx).text().to_string());
                if u.is_empty() && p.is_empty() {
                    return;
                }
                self.set_router_auth(Some((u, p)), cx);
            }
            NetAct::AuthRemove => self.set_router_auth(None, cx),
            NetAct::I2pOnly(on) => {
                if !on || !self.live {
                    self.set_i2p_only(on, cx);
                    return;
                }
                let rx = crate::backend::run(crate::backend::network::relay_rows());
                cx.spawn(async move |this, cx| {
                    let Ok(rows) = rx.await else { return };
                    this.update(cx, |app, cx| {
                        if Self::relays_on_i2p(&rows) {
                            return app.set_i2p_only(true, cx);
                        }
                        let mut popup = kit::Popup::notice("Turn on I2P-Only?");
                        popup.icon = None;
                        popup.text = Some("None of your relays are on I2P. Turn on I2P-Only anyway?".into());
                        popup.confirm = "Turn On".into();
                        popup.cancel = Some("Cancel".into());
                        app.popup = Some((popup, PopupAct::NetI2pOnly));
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            NetAct::OpMove(i, by) => {
                let Some(mut list) = self.outproxy_inputs() else { return };
                let j = i as isize + by;
                if j < 0 || j as usize >= list.len() {
                    return;
                }
                let o = list.remove(i);
                list.insert(j as usize, o);
                self.save_outproxies(Some(list), false, cx);
            }
            NetAct::OpToggle(i, on) => {
                let Some(mut list) = self.outproxy_inputs() else { return };
                if let Some(o) = list.get_mut(i) {
                    o.enabled = on;
                }
                self.save_outproxies(Some(list), false, cx);
            }
            NetAct::OpRemove(i) => {
                let Some(name) = self.outproxy_inputs().and_then(|l| l.get(i).map(|o| o.name.clone())) else { return };
                let mut popup = kit::Popup::notice(format!("Remove {name}?"));
                popup.icon = None;
                popup.text = Some("Vector stops using this outproxy.".into());
                popup.confirm = "Remove".into();
                popup.cancel = Some("Cancel".into());
                self.popup = Some((popup, PopupAct::NetOpRemove(i)));
            }
            NetAct::OpReset => {
                let own = self
                    .routing()
                    .and_then(|r| r.config.as_ref())
                    .and_then(|c| c.get("outproxies"))
                    .and_then(Value::as_array)
                    .is_some_and(|l| l.iter().any(|o| o.get("builtin").and_then(Value::as_bool) == Some(false)));
                let mut popup = kit::Popup::notice("Use the built-in outproxies?");
                popup.icon = None;
                popup.text = Some(if own { "Your own outproxies are removed." } else { "The built-in order comes back." }.into());
                popup.confirm = "Reset".into();
                popup.cancel = Some("Cancel".into());
                self.popup = Some((popup, PopupAct::NetOpReset));
            }
            NetAct::OpAdd => {
                let mk = |ph: &str, mono: bool, cx: &mut Context<Self>| cx.new(|cx| TextInput::new(ph.to_string(), cx).with_look(field_look(mono, false)).keep_on_submit());
                let name = mk("", false, cx);
                let addr = mk("xxxx.b32.i2p", true, cx);
                let port = mk("80", true, cx);
                for i in [&name, &addr, &port] {
                    cx.subscribe_in(i, window, |app, _, _: &SendText, window, cx| app.net_act(NetAct::OpSave, window, cx)).detach();
                    cx.observe(i, |_, _, cx| cx.notify()).detach();
                }
                window.focus(&name.focus_handle(cx), cx);
                if let Some(r) = self.routing() {
                    r.adding = Some([name, addr, port]);
                    r.ops_error.clear();
                }
            }
            NetAct::OpCancel => {
                if let Some(r) = self.routing() {
                    r.adding = None;
                    r.ops_error.clear();
                }
            }
            NetAct::OpSave => {
                let Some([name, addr, port]) = self.routing().and_then(|r| r.adding.clone()) else { return };
                let (n, a, p) = (name.read(cx).text().trim().to_string(), addr.read(cx).text().trim().to_lowercase(), port.read(cx).text().trim().to_string());
                if n.is_empty() || a.is_empty() {
                    return;
                }
                let Some(mut list) = self.outproxy_inputs() else { return };
                list.push(vector_core::transport::i2p_config::OutproxyInput { name: n, address: a, port: p.parse().unwrap_or(80), enabled: true });
                self.save_outproxies(Some(list), true, cx);
            }
            NetAct::CopyAddress(b32) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(b32));
                self.show_toast("Address copied", cx);
            }
            NetAct::OpenNetwork => self.settings_act(crate::settings::Act::Anchor("network", "relays"), window, cx),
        }
    }

    /// `useTransportKind`: I2P checks its router first; a live Mini App session asks to stay.
    pub(super) fn use_network(&mut self, kind: String, keep_realtime: Option<bool>, checked: bool, cx: &mut Context<Self>) {
        if !self.live || self.routing().is_some_and(|r| r.locked) {
            return;
        }
        let port = self.routing().and_then(|r| r.config.as_ref()).and_then(|c| c.get("sam_port")).and_then(Value::as_i64);
        if kind == "i2p" && !checked && let Some(port) = port {
            self.lock_net(true, cx);
            let rx = backend::run(net::i2p_test_router(port, None, None));
            cx.spawn(async move |this, cx| {
                let res = rx.await.ok().and_then(Result::ok);
                this.update(cx, |app, cx| {
                    app.lock_net(false, cx);
                    match res {
                        Some(r) if r.ok => app.use_network(kind, keep_realtime, true, cx),
                        r => {
                            let why = match r.map(|r| r.text) {
                                Some(t) if !t.is_empty() && !t.starts_with("No router answered") => t,
                                _ => format!("No I2P router answered on port {port}."),
                            };
                            let mut popup = kit::Popup::notice("Switch to I2P?");
                            popup.icon = None;
                            popup.text = Some(format!("{} Switch anyway?", kit::escape(&why)).into());
                            popup.confirm = "Switch".into();
                            popup.cancel = Some("Cancel".into());
                            app.popup = Some((popup, PopupAct::NetUse(kind)));
                        }
                    }
                })
                .ok();
            })
            .detach();
            return;
        }
        let realtime = self.net_view.as_ref().and_then(|v| v.get("realtime_active")).and_then(Value::as_bool) == Some(true);
        if realtime && kind != "clearnet" && keep_realtime.is_none() {
            let label = crate::routing::label(&kind).to_string();
            let mut popup = kit::Popup::notice("Keep your Mini App session?");
            popup.icon = None;
            popup.text = Some(format!("It connects directly, outside {label}.<br>End it to keep everything on {label}.").into());
            popup.confirm = "Keep".into();
            popup.cancel = Some("End".into());
            self.popup = Some((popup, PopupAct::NetKeepRealtime(kind)));
            return;
        }
        let k = kind.clone();
        self.net_call(net::transport_set(kind, keep_realtime), cx, move |app, res, cx| match res {
            Ok(v) => app.on_transport_view(json(v), cx),
            Err(e) => {
                let now = json(net::transport_get_state());
                // A network that failed to start says so on its own card.
                let says = now.get("kind").and_then(Value::as_str) == Some(k.as_str()) && !now["reason"].is_null();
                if !says {
                    app.show_toast(&e, cx);
                }
            }
        });
    }

    fn choose_engine(&mut self, external: bool, port: Option<i64>, cx: &mut Context<Self>) {
        if let Some(r) = self.routing() {
            r.engine.busy = true;
        }
        self.net_call(net::tor_set_engine(external, port), cx, |app, res, cx| {
            if let Some(r) = app.routing() {
                r.engine.busy = false;
            }
            match res {
                Ok(e) => {
                    app.set_engine(e.external, e.port, e.found, cx);
                    if let Some(r) = app.routing() {
                        r.circuits = Default::default();
                    }
                }
                Err(e) => app.show_toast(&e, cx),
            }
        });
    }

    /// The toggle (`Some`) saves the on/off state; Apply (`None`) the lines.
    fn set_bridges(&mut self, toggled: Option<bool>, cx: &mut Context<Self>) {
        let Some(lines) = self.routing().and_then(|r| r.bridge_lines.clone()).map(|i| i.read(cx).text().to_string()) else { return };
        let running = self.net_view.as_ref().is_some_and(|v| v["kind"] == "tor" && v["ready"] == true);
        let live = self.live;
        let Some(r) = self.routing() else { return };
        let enabled = toggled.unwrap_or(r.bridges.enabled);
        r.bridges.enabled = enabled;
        if toggled == Some(true) && lines.trim().is_empty() {
            r.bridges.status = Some((if running { "Add bridge lines, then Apply." } else { "Add bridge lines, then Save." }.into(), false));
            return;
        }
        if !live {
            return;
        }
        r.bridges.busy = true;
        r.bridges.status = Some((
            match (toggled, running) {
                (Some(true), true) => "Enabling bridges, reconnecting…",
                (Some(false), true) => "Disabling bridges, reconnecting…",
                (None, true) => "Applying & reconnecting…",
                (None, false) => "Saving…",
                _ => "",
            }
            .into(),
            false,
        ));
        let saved = lines.clone();
        self.net_call(net::tor_set_bridges(enabled, lines), cx, move |app, res, cx| {
            let Some(r) = app.routing() else { return };
            r.bridges.busy = false;
            match res {
                Ok(b) => {
                    r.bridges.saved = saved;
                    r.bridges_dirty = false;
                    let text = match (toggled, running) {
                        (Some(true), true) => "Bridges enabled. Tor reconnected.",
                        (Some(false), true) => "Bridges disabled. Tor reconnected directly.",
                        (Some(true), false) => "Bridges on. Tor will connect through them.",
                        (Some(false), false) => "Bridges off. Tor will connect directly.",
                        (None, false) => "Bridges saved. Tor will use them.",
                        (None, true) if b.enabled => "Bridges applied. Tor reconnected.",
                        (None, true) => "Bridges saved. Tor will use them.",
                    };
                    r.bridges.status = Some((text.into(), false));
                    if running {
                        app.load_circuits(false, false, cx);
                    }
                }
                Err(e) => {
                    if toggled.is_some() {
                        r.bridges.enabled = !enabled;
                    }
                    r.bridges.status = Some((format!("Failed: {e}"), true));
                }
            }
        });
    }

    fn router_port(&self, cx: &gpui::App) -> Option<i64> {
        let input = self.settings.as_ref()?.net.port.clone()?;
        input.read(cx).text().parse().ok()
    }

    fn test_router(&mut self, cx: &mut Context<Self>) {
        let Some(port) = self.router_port(cx).filter(|p| (1..=65535).contains(p)) else {
            if let Some(r) = self.routing() {
                r.note = Some(("Enter a port from 1 to 65535.".into(), false));
            }
            return;
        };
        if !self.live {
            return;
        }
        if let Some(r) = self.routing() {
            r.testing = true;
            r.note = None;
        }
        let rx = backend::run(net::i2p_test_router(port, None, None));
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |app, cx| {
                let Some(r) = app.routing() else { return };
                r.testing = false;
                r.note = Some(match res {
                    Ok(t) => (t.text, t.ok),
                    Err(e) => (e, false),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn save_router(&mut self, cx: &mut Context<Self>) {
        let saved = self.routing().and_then(|r| r.config.as_ref()).and_then(|c| c.get("sam_port")).and_then(Value::as_i64);
        let Some(port) = self.router_port(cx) else { return };
        if Some(port) == saved {
            if let Some(r) = self.routing() {
                r.port_dirty = false;
            }
            return;
        }
        if !(1..=65535).contains(&port) {
            if let Some(r) = self.routing() {
                r.note = Some(("Enter a port from 1 to 65535.".into(), false));
            }
            return;
        }
        if !self.live {
            return;
        }
        if let Some(r) = self.routing() {
            r.saving = true;
        }
        self.net_call(net::i2p_set_router(port, None), cx, |app, res, cx| {
            if let Some(r) = app.routing() {
                r.saving = false;
                match &res {
                    Ok(_) => r.note = None,
                    Err(e) => r.note = Some((e.clone(), false)),
                }
            }
            if let Ok(v) = res {
                app.on_transport_view(json(v), cx);
            }
            app.load_net_config(cx);
        });
    }

    fn set_router_auth(&mut self, auth: Option<(String, String)>, cx: &mut Context<Self>) {
        let Some(port) = self.routing().and_then(|r| r.config.as_ref()).and_then(|c| c.get("sam_port")).and_then(Value::as_i64) else { return };
        if !self.live {
            return;
        }
        if let Some(r) = self.routing() {
            r.auth_busy = true;
            r.auth_error.clear();
        }
        let saving = auth.is_some();
        self.net_call(net::i2p_set_router(port, Some(auth)), cx, move |app, res, cx| {
            if let Some(r) = app.routing() {
                r.auth_busy = false;
                match &res {
                    Ok(_) if saving => r.auth = None,
                    Ok(_) => {}
                    Err(e) => r.auth_error = e.clone(),
                }
            }
            if let Ok(v) = res {
                app.on_transport_view(json(v), cx);
            }
            app.load_net_config(cx);
        });
    }

    /// Whether any enabled relay is reachable inside I2P: an .i2p host, or one with a working
    /// I2P address.
    fn relays_on_i2p(relays: &[crate::backend::network::RelayRow]) -> bool {
        let aliases: Vec<Value> = net::transport_get_aliases().iter().map(json).collect();
        relays.iter().filter(|r| r.enabled).any(|r| {
            let host = url::Url::parse(&r.url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default();
            host.ends_with(".i2p") || aliases.iter().any(|v| v["host"] == host.as_str() && !v["twins"]["i2p"].is_null() && v["check"]["state"] != "failed")
        })
    }

    /// `checkI2pStranded`: I2P-Only in use with none of the relays inside I2P.
    pub(super) fn check_stranded(&mut self, cx: &mut Context<Self>) {
        let only = self.net_config.as_ref().is_some_and(|c| c["exit"] == "off");
        let in_i2p = self.net_view.as_ref().is_some_and(|v| v["kind"] == "i2p");
        if !(only && in_i2p) || !self.live {
            self.set_stranded(false, cx);
            return;
        }
        let rx = crate::backend::run(crate::backend::network::relay_rows());
        cx.spawn(async move |this, cx| {
            let Ok(rows) = rx.await else { return };
            let stranded = !Self::relays_on_i2p(&rows);
            this.update(cx, |app, cx| app.set_stranded(stranded, cx)).ok();
        })
        .detach();
    }

    fn set_stranded(&mut self, on: bool, cx: &mut Context<Self>) {
        self.net_stranded = on;
        if let Some(r) = self.routing() {
            r.stranded = on;
        }
        cx.notify();
    }

    pub(super) fn set_i2p_only(&mut self, on: bool, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        if let Some(r) = self.routing() {
            r.exit_error.clear();
        }
        let mode = if on { "off" } else { "allow" }.to_string();
        self.net_call(net::i2p_set_exit(mode), cx, |app, res, cx| {
            match res {
                Ok(v) => app.on_transport_view(json(v), cx),
                Err(e) => {
                    if let Some(r) = app.routing() {
                        r.exit_error = e;
                    }
                }
            }
            app.load_net_config(cx);
        });
    }

    fn outproxy_inputs(&mut self) -> Option<Vec<vector_core::transport::i2p_config::OutproxyInput>> {
        let list = self.routing()?.config.as_ref()?.get("outproxies")?.as_array()?.clone();
        Some(
            list.iter()
                .map(|o| vector_core::transport::i2p_config::OutproxyInput {
                    name: o["name"].as_str().unwrap_or("").into(),
                    address: o["address"].as_str().unwrap_or("").into(),
                    port: o["port"].as_i64().unwrap_or(0),
                    enabled: o["enabled"].as_bool().unwrap_or(true),
                })
                .collect(),
        )
    }

    pub(super) fn save_outproxies(&mut self, list: Option<Vec<vector_core::transport::i2p_config::OutproxyInput>>, closing: bool, cx: &mut Context<Self>) {
        if !self.live {
            return;
        }
        if let Some(r) = self.routing() {
            r.ops_busy = true;
            r.ops_error.clear();
        }
        let rx = backend::run(net::i2p_set_outproxies(list));
        cx.spawn(async move |this, cx| {
            let res = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |app, cx| {
                let Some(r) = app.routing() else { return };
                r.ops_busy = false;
                match res {
                    Ok(cfg) => {
                        r.config = Some(json(cfg));
                        if closing {
                            r.adding = None;
                        }
                    }
                    Err(e) => r.ops_error = e,
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn remove_outproxy(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(mut list) = self.outproxy_inputs() else { return };
        if i < list.len() {
            list.remove(i);
            self.save_outproxies(Some(list), false, cx);
        }
    }

    fn open_net_info(&mut self, key: &'static str, cx: &mut Context<Self>) {
        self.net_info = Some(key);
        cx.notify();
    }
}

fn apply_circuits(r: &mut Routing, c: &Value) {
    r.circuits.phase = Load::Ok;
    r.circuits.count = c.get("circuits").and_then(Value::as_u64).unwrap_or(0) as usize;
    r.circuits.hosts = c.get("hosts").and_then(Value::as_u64).unwrap_or(0) as usize;
    r.circuits.hops = c.get("hops").and_then(Value::as_array).cloned().unwrap_or_default();
}

