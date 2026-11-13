//! The structured command composer (`CommandComposer.svelte`): one pill per argument in
//! place of the draft, so quoting is the code's job and never the user's.

use gpui::{AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px};
use crate::pic::img;

use crate::commands::{Arg, ArgType, Cmd, arg_error};
use crate::composer::{CancelInput, Edge, InputChanged, Look, Nav, SendText, TextInput};
use crate::theme::*;

/// A member a user argument can name.
#[derive(Clone)]
pub struct Member {
    pub npub: String,
    pub name: String,
    pub avatar: Option<std::path::PathBuf>,
}

enum Field {
    Text(Entity<TextInput>),
    /// A choice or bool: a trigger that opens a drop-up.
    Pick { focus: FocusHandle, value: String },
}

struct Part {
    arg: Arg,
    field: Field,
    /// A user argument's picked member.
    npub: Option<String>,
}

struct Menu {
    part: usize,
    /// (value, label, avatar); an empty value skips an optional argument.
    options: Vec<(String, String, Option<Option<std::path::PathBuf>>)>,
    active: usize,
}

pub enum Event {
    /// The assembled `/name args` text, for the send path.
    Submit(String),
    Exit,
}

impl EventEmitter<Event> for CommandForm {}

pub struct CommandForm {
    pub cmd: Cmd,
    pub chat: String,
    parts: Vec<Part>,
    pub invalid: Option<usize>,
    pub focused: Option<usize>,
    menu: Option<Menu>,
    members: Vec<Member>,
}

fn input_look(resolved: bool) -> Look {
    Look {
        family: INTER,
        size: px(13.),
        line_height: px(16.9),
        color: if resolved { c(PRIMARY) } else { gpui::white() },
        weight: if resolved { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM },
        placeholder: white_a(0.38),
        ..Look::default()
    }
}

/// `assembleCommandText`: values with spaces or quotes quoted, so they re-parse whole.
pub fn assemble(name: &str, values: &[String]) -> String {
    let mut out = format!("/{name}");
    for v in values {
        out.push(' ');
        if v.is_empty() || v.contains(|c: char| c.is_whitespace() || c == '"') {
            out.push('"');
            out.push_str(&v.replace('\\', "\\\\").replace('"', "\\\""));
            out.push('"');
        } else {
            out.push_str(v);
        }
    }
    out
}

impl CommandForm {
    pub fn new(cmd: Cmd, chat: String, members: Vec<Member>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let n = cmd.args.len();
        let parts = cmd
            .args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let field = match a.kind {
                    ArgType::Choice | ArgType::Bool => Field::Pick { focus: cx.focus_handle(), value: String::new() },
                    _ => {
                        let placeholder = if a.kind == ArgType::User { "npub1…" } else { "" };
                        let max = if a.kind == ArgType::User { 70 } else { 1024 };
                        let grow = a.kind == ArgType::String && i + 1 == n;
                        let input = cx.new(|cx| {
                            let t = TextInput::new(placeholder, cx).edges().keep_on_submit().max_chars(max).with_look(input_look(false));
                            if grow { t.multiline(px(160.)) } else { t }
                        });
                        cx.subscribe_in(&input, window, move |f, _, _: &SendText, w, cx| f.on_enter(i, w, cx)).detach();
                        cx.subscribe_in(&input, window, |_, _, _: &CancelInput, _, cx| cx.emit(Event::Exit)).detach();
                        cx.subscribe_in(&input, window, move |f, _, e: &Edge, w, cx| f.on_edge(i, *e, w, cx)).detach();
                        cx.subscribe_in(&input, window, move |f, _, nav: &Nav, w, cx| f.on_nav(i, *nav, w, cx)).detach();
                        cx.subscribe_in(&input, window, move |f, _, _: &InputChanged, w, cx| f.on_changed(i, w, cx)).detach();
                        Field::Text(input)
                    }
                };
                Part { arg: a.clone(), field, npub: None }
            })
            .collect();
        Self { cmd, chat, parts, invalid: None, focused: None, menu: None, members }
    }

    pub fn current_hint(&self) -> String {
        self.focused.and_then(|i| self.parts.get(i)).map(|p| p.arg.description.clone()).unwrap_or_default()
    }

    fn value(&self, i: usize, cx: &App) -> String {
        let p = &self.parts[i];
        match &p.field {
            Field::Pick { value, .. } => value.clone(),
            Field::Text(t) => match &p.npub {
                Some(n) => n.clone(),
                None => t.read(cx).text().trim().to_string(),
            },
        }
    }

    pub fn focus(&mut self, i: usize, at_end: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.parts.get(i) else { return };
        self.focused = Some(i);
        match &p.field {
            Field::Text(t) => {
                window.focus(&t.focus_handle(cx), cx);
                t.update(cx, |t, cx| {
                    let pos = if at_end { t.text().len() } else { 0 };
                    t.set_caret(pos, cx);
                });
            }
            Field::Pick { focus, .. } => {
                window.focus(focus, cx);
                self.open_choices(i);
            }
        }
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(m) = self.menu.take()
            && let Some(Part { field: Field::Text(t), .. }) = self.parts.get(m.part)
        {
            t.update(cx, |t, _| t.set_nav(false));
        }
    }

    fn open_choices(&mut self, i: usize) {
        let p = &self.parts[i];
        let Field::Pick { value, .. } = &p.field else { return };
        let mut options = Vec::new();
        if !p.arg.required {
            options.push((String::new(), "(skip)".to_string(), None));
        }
        let values: Vec<String> = if p.arg.kind == ArgType::Bool { vec!["true".into(), "false".into()] } else { p.arg.choices.clone() };
        options.extend(values.into_iter().map(|v| (v.clone(), v, None)));
        if options.is_empty() {
            return;
        }
        let active = options.iter().position(|o| o.0 == *value).unwrap_or(0);
        self.menu = Some(Menu { part: i, options, active });
    }

    fn open_members(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(Part { field: Field::Text(t), .. }) = self.parts.get(i) else { return };
        let q = t.read(cx).text().to_lowercase();
        let options: Vec<_> = self
            .members
            .iter()
            .filter(|m| m.name.to_lowercase().contains(&q) || m.npub.to_lowercase().contains(&q))
            .take(6)
            .map(|m| (m.npub.clone(), m.name.clone(), Some(m.avatar.clone())))
            .collect();
        let open = !options.is_empty();
        self.menu = open.then_some(Menu { part: i, options, active: 0 });
        t.update(cx, |t, _| t.set_nav(open));
    }

    fn pick(&mut self, k: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.take() else { return };
        let Some((v, label, _)) = menu.options.get(k).cloned() else { return };
        let i = menu.part;
        match &mut self.parts[i].field {
            Field::Pick { value, .. } => *value = v,
            Field::Text(t) => {
                let t = t.clone();
                t.update(cx, |t, cx| {
                    t.set_nav(false);
                    t.set_text(&label, cx);
                    t.set_look(input_look(true), cx);
                });
                self.parts[i].npub = Some(v);
            }
        }
        self.invalid = None;
        if i + 1 < self.parts.len() {
            self.focus(i + 1, false, window, cx);
        }
        cx.notify();
    }

    fn on_enter(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i + 1 == self.parts.len() {
            self.submit(window, cx);
        } else {
            self.focus(i + 1, false, window, cx);
        }
    }

    fn on_edge(&mut self, i: usize, e: Edge, window: &mut Window, cx: &mut Context<Self>) {
        match e {
            Edge::Empty if i == 0 => cx.emit(Event::Exit),
            Edge::Empty | Edge::Start if i > 0 => self.focus(i - 1, true, window, cx),
            Edge::End if i + 1 < self.parts.len() => self.focus(i + 1, false, window, cx),
            _ => {}
        }
    }

    fn on_nav(&mut self, i: usize, nav: Nav, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.as_mut().filter(|m| m.part == i) else { return };
        let n = menu.options.len();
        match nav {
            Nav::Up => menu.active = (menu.active + n - 1) % n,
            Nav::Down => menu.active = (menu.active + 1) % n,
            Nav::Pick => {
                let k = menu.active;
                return self.pick(k, window, cx);
            }
            Nav::Close => self.close_menu(cx),
        }
        cx.notify();
    }

    fn on_changed(&mut self, i: usize, _: &mut Window, cx: &mut Context<Self>) {
        self.invalid = None;
        self.focused = Some(i);
        let p = &mut self.parts[i];
        if p.arg.kind == ArgType::User {
            if p.npub.take().is_some()
                && let Field::Text(t) = &p.field
            {
                t.update(cx, |t, cx| t.set_look(input_look(false), cx));
            }
            self.open_members(i, cx);
        } else if matches!(p.arg.kind, ArgType::Int | ArgType::Number)
            && let Field::Text(t) = &p.field
        {
            let int = p.arg.kind == ArgType::Int;
            t.update(cx, |t, cx| {
                let raw = t.text().to_string();
                let mut out = String::new();
                let mut dot = false;
                for ch in raw.chars() {
                    let keep = ch.is_ascii_digit() || (ch == '-' && out.is_empty()) || (ch == '.' && !int && !std::mem::replace(&mut dot, true));
                    if keep {
                        out.push(ch);
                    }
                }
                if out != raw {
                    t.set_text(&out, cx);
                }
            });
        }
        cx.notify();
    }

    fn on_pick_key(&mut self, i: usize, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = e.keystroke.key.as_str();
        let open = self.menu.as_ref().is_some_and(|m| m.part == i);
        cx.stop_propagation();
        match key {
            "up" | "down" if open => {
                let m = self.menu.as_mut().expect("menu");
                let n = m.options.len();
                m.active = (m.active + if key == "down" { 1 } else { n - 1 }) % n;
            }
            "enter" | "space" if open => {
                let k = self.menu.as_ref().map_or(0, |m| m.active);
                return self.pick(k, window, cx);
            }
            "escape" if open => self.menu = None,
            "up" | "down" | "space" => self.open_choices(i),
            "backspace" | "delete" => {
                let Field::Pick { value, .. } = &mut self.parts[i].field else { return };
                if !value.is_empty() {
                    value.clear();
                } else {
                    self.menu = None;
                    if i == 0 {
                        return cx.emit(Event::Exit);
                    }
                    return self.focus(i - 1, true, window, cx);
                }
            }
            "enter" if i + 1 == self.parts.len() => return self.submit(window, cx),
            "enter" | "right" if i + 1 < self.parts.len() => return self.focus(i + 1, false, window, cx),
            "left" if i > 0 => return self.focus(i - 1, true, window, cx),
            "escape" => return cx.emit(Event::Exit),
            "tab" => {
                self.menu = None;
                return cx.propagate();
            }
            _ => return cx.propagate(),
        }
        cx.notify();
    }

    /// Validates every part and sends, or marks the first bad one.
    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let values: Vec<String> = (0..self.parts.len()).map(|i| self.value(i, cx)).collect();
        for (i, v) in values.iter().enumerate() {
            let a = &self.parts[i].arg;
            if (v.is_empty() && a.required) || (!v.is_empty() && arg_error(a, v).is_some()) {
                self.invalid = Some(i);
                self.focus(i, true, window, cx);
                return;
            }
        }
        let last = values.iter().rposition(|v| !v.is_empty()).map_or(0, |i| i + 1);
        cx.emit(Event::Submit(assemble(&self.cmd.name, &values[..last])));
    }
}

impl Render for CommandForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let n = self.parts.len();
        let focused = self
            .parts
            .iter()
            .position(|p| match &p.field {
                Field::Text(t) => t.focus_handle(cx).contains_focused(window, cx),
                Field::Pick { focus, .. } => focus.contains_focused(window, cx),
            });
        if focused.is_some() && focused != self.focused {
            self.focused = focused;
        }
        let pills = self.parts.iter().enumerate().map(|(i, p)| {
            let grow = p.arg.kind == ArgType::String && i + 1 == n;
            let on = focused == Some(i);
            let name = div()
                .flex_shrink_0()
                .text_size(px(11.))
                .line_height(px(24.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if on { gpui::white() } else { white_a(0.55) })
                .child(p.arg.name.to_lowercase())
                .when(p.arg.required, |d| d.child(div().text_color(c(0xff5d5d)).child("*")))
                .flex();
            let field: AnyElement = match &p.field {
                Field::Text(t) => {
                    let chars = t.read(cx).text().chars().count().max(if p.arg.kind == ArgType::User { 6 } else { 0 });
                    let w = (chars as f32 * 7.4 + 26.).clamp(72., 34. * 7.4);
                    div()
                        .bg(ca(0, 0, 0, 0.35))
                        .rounded(px(7.))
                        .px(px(8.))
                        .py(px(2.))
                        .min_w(px(60.))
                        .when(on, |d| d.border_1().border_color(c(PRIMARY)).px(px(7.)).py(px(1.)))
                        .when(grow, |d| d.flex_1().min_w_0())
                        .when(!grow, |d| d.w(px(w)))
                        .child(t.clone())
                        .into_any_element()
                }
                Field::Pick { focus, value } => {
                    let focus = focus.clone();
                    div()
                        .id(SharedString::from(format!("cmd-pick-{i}")))
                        .track_focus(&focus)
                        .relative()
                        .flex()
                        .items_center()
                        .h(px(21.))
                        .min_w(px(72.))
                        .max_w(px(22. * 7.4))
                        .pl(px(8.))
                        .pr(px(20.))
                        .rounded(px(7.))
                        .bg(ca(0, 0, 0, 0.35))
                        .when(on, |d| d.border_1().border_color(c(PRIMARY)).pl(px(7.)).pr(px(19.)))
                        .cursor_pointer()
                        .text_size(px(13.))
                        .text_color(if value.is_empty() { white_a(0.4) } else { gpui::white() })
                        .child(div().truncate().child(if value.is_empty() { "…".to_string() } else { value.clone() }))
                        .child(
                            gpui::svg()
                                .path("icons/chevron-down.svg")
                                .absolute()
                                .right(px(6.))
                                .size(px(9.))
                                .text_color(white_a(0.55)),
                        )
                        .on_key_down(cx.listener(move |f, e: &KeyDownEvent, w, cx| f.on_pick_key(i, e, w, cx)))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |f, _, w, cx| {
                                cx.stop_propagation();
                                if f.menu.as_ref().is_some_and(|m| m.part == i) {
                                    f.menu = None;
                                    f.focused = Some(i);
                                    if let Some(Part { field: Field::Pick { focus, .. }, .. }) = f.parts.get(i) {
                                        w.focus(focus, cx);
                                    }
                                    cx.notify();
                                } else {
                                    f.focus(i, false, w, cx);
                                }
                            }),
                        )
                        .into_any_element()
                }
            };
            let menu = self.menu.as_ref().filter(|m| m.part == i).map(|m| {
                let rows = m.options.iter().enumerate().map(|(k, (v, label, face))| {
                    div()
                        .id(SharedString::from(format!("cmd-opt-{k}")))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .px(px(14.))
                        .py(px(6.))
                        .text_size(px(13.))
                        .text_color(if v.is_empty() { white_a(0.4) } else { white_a(0.9) })
                        .cursor_pointer()
                        .when(k == m.active, |d| d.bg(white_a(0.08)))
                        .hover(|d| d.bg(white_a(0.08)))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |f, _, w, cx| {
                                cx.stop_propagation();
                                f.pick(k, w, cx)
                            }),
                        )
                        .children(face.as_ref().map(|a| match a {
                            Some(p) => img(p.clone()).object_fit(gpui::ObjectFit::Cover).size(px(18.)).rounded_full().flex_shrink_0().into_any_element(),
                            None => img("icons/user-placeholder.svg").size(px(18.)).rounded_full().flex_shrink_0().into_any_element(),
                        }))
                        .child(div().truncate().max_w(px(40. * 7.)).child(label.clone()))
                });
                gpui::deferred(
                    div()
                        .absolute()
                        .bottom_full()
                        .left_0()
                        .mb(px(6.))
                        .min_w(px(112.))
                        .bg(c(0x0a0a0a))
                        .border_1()
                        .border_color(c(PRIMARY))
                        .rounded(px(10.))
                        .overflow_hidden()
                        .children(rows),
                )
                .with_priority(4)
            });
            div()
                .relative()
                .flex()
                .items_center()
                .gap(px(6.))
                .pl(px(8.))
                .pr(px(3.))
                .py(px(2.))
                .rounded(px(10.))
                .bg(white_a(0.07))
                .border_1()
                .border_color(if self.invalid == Some(i) { c(0xff5d5d) } else { white_a(0.1) })
                .max_w_full()
                .when(grow, |d| d.flex_1().min_w(px(150.)))
                .when(!grow, |d| d.flex_shrink_0())
                .child(name)
                .child(div().relative().when(grow, |d| d.flex_1().min_w_0()).child(field).children(menu))
        });
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .content_center()
            .gap(px(5.))
            .min_h(px(45.))
            .w_full()
            .min_w_0()
            .pl(px(12.))
            .pr(px(10.))
            .py(px(6.))
            .children(pills)
    }
}

/// `#chat-command-bar`: "Using /cmd with Bot", the focused argument's description, Cancel.
pub fn strip(form: &CommandForm, bot: (SharedString, Option<std::path::PathBuf>), on_cancel: impl Fn(&mut Window, &mut App) + 'static) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .flex_shrink_0()
                .whitespace_nowrap()
                .text_size(px(13.5))
                .text_color(white_a(0.85))
                .flex()
                .child("Using\u{a0}")
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(c(PRIMARY)).child(format!("/{}", form.cmd.name)))
                .child("\u{a0}with"),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(5.))
                .min_w_0()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(PRIMARY))
                .child(match bot.1 {
                    Some(p) => img(p).object_fit(gpui::ObjectFit::Cover).size(px(16.)).rounded_full().flex_shrink_0().into_any_element(),
                    None => img("icons/user-placeholder.svg").size(px(16.)).rounded_full().flex_shrink_0().into_any_element(),
                })
                .child(div().min_w_0().truncate().child(bot.0)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_right()
                .text_size(px(12.))
                .text_color(white_a(0.45))
                .child(form.current_hint()),
        )
        .child(
            div()
                .id("command-cancel")
                .absolute()
                .right(px(10.5 - 44.))
                .size(px(24.))
                .p(px(3.))
                .cursor_pointer()
                .on_click(move |_, w, cx| on_cancel(w, cx))
                .child(gpui::svg().path("icons/cancel.svg").size(px(18.)).text_color(c(crate::timeline::DANGER))),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    #[test]
    fn assembles_quoted() {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(super::assemble("say", &v(&["hi there", "a\"b", "x"])), r#"/say "hi there" "a\"b" x"#);
        assert_eq!(super::assemble("roll", &v(&["", "6"])), r#"/roll "" 6"#);
    }
}
