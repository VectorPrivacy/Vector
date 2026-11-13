//! The `/` command picker (`command-picker.js`, `CommandPopup.svelte`): the sectioned list
//! while the name is being typed, then the armed command's hint row.

use std::rc::Rc;

use gpui::{AnyElement, App, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px};
use crate::pic::img;

use crate::textcolor;
use crate::theme::*;

pub const SYSTEM: &str = "system";
const RECENTS_CAP: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub enum ArgType {
    String,
    Int,
    Number,
    Bool,
    User,
    Choice,
    Color,
}

#[derive(Clone, Debug)]
pub struct Arg {
    pub name: String,
    pub kind: ArgType,
    pub required: bool,
    pub description: String,
    pub choices: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Builtin {
    Effect,
    Time,
    Face(&'static str),
    Coinflip,
}

#[derive(Clone, Debug)]
pub struct Cmd {
    /// `system`, or the bot's npub.
    pub bot: String,
    pub name: String,
    pub description: String,
    pub args: Vec<Arg>,
    pub builtin: Option<Builtin>,
    pub aliases: &'static [&'static str],
}

impl Cmd {
    pub fn is_system(&self) -> bool {
        self.bot == SYSTEM
    }

    fn key(&self) -> String {
        format!("{}:{}", self.bot, self.name)
    }
}

fn arg(name: &str, kind: ArgType, required: bool, description: String) -> Arg {
    Arg { name: name.into(), kind, required, description, choices: Vec::new() }
}

/// Commands built into Vector, listed after every bot's; a bot declaring the same name wins.
pub fn system_commands(effects: bool) -> Vec<Cmd> {
    let w = textcolor::color_word();
    let sys = |name: &str, description: String, args: Vec<Arg>, builtin, aliases| Cmd {
        bot: SYSTEM.into(),
        name: name.into(),
        description,
        args,
        builtin: Some(builtin),
        aliases,
    };
    let text = || arg("text", ArgType::String, true, format!("The message to {w}"));
    let message = || arg("message", ArgType::String, false, "What to say before it".into());
    let mut out = Vec::new();
    if effects {
        out.push(sys("rainbow", format!("Send your message in rainbow {w}s"), vec![text()], Builtin::Effect, &[]));
        out.push(sys(
            "gradient",
            format!("Fade your message from one {w} to another"),
            vec![
                arg("from", ArgType::Color, true, format!("The {w} it starts in: a name or #hex")),
                arg("to", ArgType::Color, true, format!("The {w} it ends in: a name or #hex")),
                text(),
            ],
            Builtin::Effect,
            &[],
        ));
        out.push(sys(
            w,
            format!("Send your message in one {w}"),
            vec![arg(w, ArgType::Color, true, format!("Its {w}: a name or #hex")), text()],
            Builtin::Effect,
            &[],
        ));
    }
    out.push(sys(
        "time",
        "A time everyone sees in their own zone".into(),
        vec![arg("when", ArgType::String, true, "Like “friday 8pm” or “in 2 hours”".into())],
        Builtin::Time,
        &[],
    ));
    out.push(sys("shrug", "Add ¯\\_(ツ)_/¯ to your message".into(), vec![message()], Builtin::Face("¯\\_(ツ)_/¯"), &[]));
    out.push(sys(
        "flip",
        "Add (╯°□°)╯︵ ┻━┻ to your message".into(),
        vec![message()],
        Builtin::Face("(╯°□°)╯︵ ┻━┻"),
        &["tableflip"],
    ));
    out.push(sys("unflip", "Add ┬─┬ノ( º _ ºノ) to your message".into(), vec![message()], Builtin::Face("┬─┬ノ( º _ ºノ)"), &[]));
    out.push(sys("coinflip", "Flip a coin: heads or tails".into(), Vec::new(), Builtin::Coinflip, &[]));
    out
}

/// The text a builtin with a fixed output sends.
fn builtin_text(b: Builtin, rest: &str) -> Option<String> {
    match b {
        Builtin::Face(face) if rest.is_empty() => Some(face.into()),
        Builtin::Face(face) => Some(format!("{rest} {face}")),
        // Decided on the sender's device, as any dice roll in a chat is.
        Builtin::Coinflip => {
            let heads = rand::random::<bool>();
            Some(format!("🪙 Coin flip: **{}**", if heads { "Heads" } else { "Tails" }))
        }
        _ => None,
    }
}

/// A bot's command set, as a manifest declares it.
pub fn bot_commands(bot: &str, specs: &[vector_core::bot_interface::CommandSpec]) -> Vec<Cmd> {
    use vector_core::bot_interface::ArgType as T;
    specs
        .iter()
        .map(|c| Cmd {
            bot: bot.into(),
            name: c.name.clone(),
            description: c.description.clone(),
            args: c
                .args
                .iter()
                .map(|a| Arg {
                    name: a.name.clone(),
                    kind: match a.arg_type {
                        T::String => ArgType::String,
                        T::Int => ArgType::Int,
                        T::Number => ArgType::Number,
                        T::Bool => ArgType::Bool,
                        T::User => ArgType::User,
                        T::Choice => ArgType::Choice,
                    },
                    required: a.required,
                    description: a.description.clone(),
                    choices: a.choices.clone(),
                })
                .collect(),
            builtin: None,
            aliases: &[],
        })
        .collect()
}

/// One shell-style token from `s[i..]`: its value and where it ends; `Err` on an open quote.
fn next_token(s: &str, i: usize) -> Option<Result<(String, usize), ()>> {
    let rest = &s[i..];
    let start = i + (rest.len() - rest.trim_start().len());
    if start >= s.len() {
        return None;
    }
    let mut out = String::new();
    if s[start..].starts_with('"') {
        let mut it = s[start + 1..].char_indices().peekable();
        while let Some((k, c)) = it.next() {
            match c {
                '\\' if matches!(it.peek(), Some((_, '"' | '\\'))) => out.push(it.next().map_or(c, |x| x.1)),
                '"' => return Some(Ok((out, start + 1 + k + 1))),
                c => out.push(c),
            }
        }
        return Some(Err(()));
    }
    let len = s[start..].find(char::is_whitespace).unwrap_or(s.len() - start);
    Some(Ok((s[start..start + len].to_string(), start + len)))
}

/// Positional parse of what follows the name; an unquoted trailing string swallows the rest.
fn parse_args(cmd: &Cmd, rest: &str) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut cursor = 0;
    for (i, a) in cmd.args.iter().enumerate() {
        let remainder = rest[cursor..].trim_start();
        if remainder.is_empty() {
            break;
        }
        let value = if i + 1 == cmd.args.len() && a.kind == ArgType::String && !remainder.starts_with('"') {
            cursor = rest.len();
            remainder.trim_end().to_string()
        } else {
            match next_token(rest, cursor) {
                None => break,
                Some(Err(())) => return None,
                Some(Ok((v, next))) => {
                    cursor = next;
                    v
                }
            }
        };
        if value.is_empty() && !a.required {
            continue;
        }
        out.push((a.name.clone(), value));
    }
    Some(out)
}

pub fn arg_error(a: &Arg, v: &str) -> Option<String> {
    if v.len() > 1024 {
        return Some("is too long (max 1024 characters)".into());
    }
    let bad = match a.kind {
        ArgType::Int => {
            let d = v.strip_prefix(['+', '-']).unwrap_or(v);
            (d.is_empty() || !d.bytes().all(|b| b.is_ascii_digit())).then(|| "must be a whole number".into())
        }
        ArgType::Number => (v.trim().is_empty() || !v.trim().parse::<f64>().is_ok_and(f64::is_finite)).then(|| "must be a number".into()),
        ArgType::Bool => (!["true", "false", "yes", "no", "1", "0"].contains(&v.to_lowercase().as_str())).then(|| "must be true or false".into()),
        ArgType::User => (!v.starts_with("npub1") || v.len() > 70).then(|| "must be a user (npub)".into()),
        ArgType::Choice => (!a.choices.iter().any(|c| c == v)).then(|| format!("must be one of: {}", a.choices.join(", "))),
        ArgType::Color => vector_core::text_color::hex(v).is_none().then(|| format!("must be a {} name or #hex", textcolor::color_word())),
        ArgType::String => None,
    };
    bad
}

fn validate(cmd: &Cmd, parsed: &[(String, String)]) -> Option<String> {
    for a in &cmd.args {
        match parsed.iter().find(|(n, _)| *n == a.name) {
            None if a.required => return Some(format!("Missing required argument \"{}\"", a.name)),
            None => {}
            Some((_, v)) => {
                if let Some(e) = arg_error(a, v) {
                    return Some(format!("\"{}\" {e}", a.name));
                }
            }
        }
    }
    None
}

/// What a send does with its text.
#[derive(Debug, PartialEq)]
pub enum Route {
    /// Ordinary chat text, unknown `/words` included.
    Plain,
    /// A known command with bad arguments: the send is refused and the draft kept.
    Error(String),
    /// A builtin rewrote the text.
    Text(String),
    /// A bot's command, tagged to it.
    Bot(String),
}

pub struct Row {
    pub index: usize,
    pub cmd: Cmd,
    pub show_bot: bool,
}

pub struct Section {
    pub key: String,
    pub title: SharedString,
    pub avatar: Option<Face>,
    pub refreshing: bool,
    pub rows: Vec<Row>,
}

pub struct Hint {
    pub cmd: Cmd,
    pub current: usize,
    pub preview: Option<Vec<Hsla>>,
    pub choices: Vec<(String, Option<Hsla>)>,
}

pub enum View {
    Loading(String),
    List { sections: Vec<Section>, active: usize, count: usize },
    Hint(Hint),
}

/// A chat's bot commands as the backend last answered.
#[derive(Clone, Default)]
pub struct Snapshot {
    pub bots: usize,
    pub commands: Vec<Cmd>,
    pub fresh: bool,
}

#[derive(Default)]
pub struct Picker {
    pub view: Option<View>,
    pub active: usize,
    /// The explicitly picked command: (chat, bot, name).
    pub armed: Option<(String, String, String)>,
    pub suppressed: Option<String>,
    pub recents: Vec<String>,
}

pub struct Ctx<'a> {
    pub chat: &'a str,
    pub snapshot: Option<&'a Snapshot>,
    pub loading: bool,
    pub effects: bool,
    pub bot_name: &'a dyn Fn(&str) -> (SharedString, Option<std::path::PathBuf>),
}

impl Ctx<'_> {
    fn all(&self) -> Vec<Cmd> {
        let mut out: Vec<Cmd> = self.snapshot.map(|s| s.commands.clone()).unwrap_or_default();
        out.extend(system_commands(self.effects));
        out
    }
}

impl Picker {
    pub fn is_list(&self) -> bool {
        matches!(self.view, Some(View::List { .. } | View::Loading(_)))
    }

    pub fn is_hint(&self) -> bool {
        matches!(self.view, Some(View::Hint(_)))
    }

    pub fn close(&mut self) {
        self.view = None;
        self.active = 0;
    }

    fn resolve(&self, ctx: &Ctx, name: &str) -> Option<Cmd> {
        let all = ctx.all();
        let prefer = self.armed.as_ref().filter(|a| a.2 == name).map(|a| a.1.clone());
        let named: Vec<&Cmd> = all.iter().filter(|c| c.name == name).collect();
        if let Some(c) = prefer.and_then(|b| named.iter().find(|c| c.bot == b)) {
            return Some((*c).clone());
        }
        if let Some(c) = named.first() {
            return Some((*c).clone());
        }
        let word = textcolor::color_word();
        all.iter()
            .find(|c| c.is_system() && ((["color", "colour"].contains(&name) && c.name == word) || c.aliases.contains(&name)))
            .cloned()
    }

    /// Whether `/name` runs Vector's own command here.
    pub fn system_owns(&self, ctx: &Ctx, name: &str) -> bool {
        self.resolve(ctx, name).is_some_and(|c| c.is_system())
    }

    fn bump(&mut self, cmd: &Cmd) {
        let key = cmd.key();
        self.recents.retain(|k| *k != key);
        self.recents.insert(0, key);
        self.recents.truncate(RECENTS_CAP);
    }

    /// Re-read the draft: the list while the name is typed, the hint once past it.
    pub fn on_input(&mut self, ctx: &Ctx, text: &str) {
        if !text.starts_with('/') {
            self.armed = None;
            self.suppressed = None;
            return self.close();
        }
        if self.armed.as_ref().is_some_and(|a| a.0 != ctx.chat) {
            self.armed = None;
        }
        if self.suppressed.as_deref().is_some_and(|s| s != text) {
            self.suppressed = None;
        }
        let Some(end) = text.find(char::is_whitespace) else {
            self.active = 0;
            return self.render(ctx, &text[1..]);
        };
        let name = &text[1..end];
        match self.resolve(ctx, name) {
            Some(c) if c.builtin == Some(Builtin::Time) => self.close(),
            Some(c) if self.suppressed.as_deref() != Some(text) => self.hint(c, &text[end..]),
            _ => self.close(),
        }
    }

    fn render(&mut self, ctx: &Ctx, query: &str) {
        let q = query.to_lowercase();
        let mut matches: Vec<Cmd> = ctx.all().into_iter().filter(|c| c.name.contains(&q)).collect();
        matches.sort_by_key(|c| !c.name.starts_with(&q));
        if matches.is_empty() {
            let snap = ctx.snapshot;
            if snap.is_none() || (ctx.loading && snap.is_some_and(|s| s.bots != 0)) {
                let n = snap.map_or(0, |s| s.bots);
                let label = if n > 0 {
                    format!("Loading {n} bot{}…", if n == 1 { "" } else { "s" })
                } else {
                    "Looking for bots…".into()
                };
                self.view = Some(View::Loading(label));
            } else {
                self.close();
            }
            return;
        }
        let recent: Vec<Cmd> = self
            .recents
            .iter()
            .filter_map(|k| matches.iter().find(|c| c.key() == *k).cloned())
            .collect();
        let mut sections = Vec::new();
        let mut count = 0;
        let mut section = |key: String, title: SharedString, avatar, cmds: Vec<Cmd>, show_bot, refreshing| {
            let rows = cmds
                .into_iter()
                .map(|cmd| {
                    count += 1;
                    Row { index: count - 1, cmd, show_bot }
                })
                .collect();
            sections.push(Section { key, title, avatar, refreshing, rows });
        };
        if !recent.is_empty() {
            section("recent".into(), "Recently Used".into(), None, recent, true, false);
        }
        let refreshing = ctx.snapshot.is_some_and(|s| !s.fresh);
        let mut bots: Vec<String> = Vec::new();
        for c in &matches {
            if !bots.contains(&c.bot) {
                bots.push(c.bot.clone());
            }
        }
        for bot in bots {
            let cmds: Vec<Cmd> = matches.iter().filter(|c| c.bot == bot).cloned().collect();
            let (title, avatar) = if bot == SYSTEM {
                ("Vector".into(), Some(Face::Mark))
            } else {
                let (name, path) = (ctx.bot_name)(&bot);
                (name, Some(path.map_or(Face::Placeholder, Face::Path)))
            };
            let refresh = refreshing && bot != SYSTEM;
            section(bot, title, avatar, cmds, false, refresh);
        }
        if self.active >= count {
            self.active = 0;
        }
        self.view = Some(View::List { sections, active: self.active, count });
    }

    fn hint(&mut self, cmd: Cmd, rest: &str) {
        let mut filled = 0;
        let mut cursor = 0;
        let mut partial = String::new();
        while filled < cmd.args.len() {
            let Some(Ok((v, next))) = next_token(rest, cursor) else { break };
            if next >= rest.len() && !rest.ends_with(char::is_whitespace) {
                partial = v;
                break;
            }
            cursor = next;
            filled += 1;
        }
        let current = filled.min(cmd.args.len().saturating_sub(1));
        let preview = cmd.is_system().then(|| {
            let mut typed = Vec::new();
            let mut at = 0;
            while typed.len() < 2 {
                let Some(Ok((v, next))) = next_token(rest, at) else { break };
                typed.push(v);
                at = next;
            }
            textcolor::effect_preview(&cmd.name, &typed)
        });
        let choices = match cmd.args.get(current).map(|a| &a.kind) {
            Some(ArgType::Choice) => cmd.args[current].choices.iter().map(|c| (c.clone(), None)).collect(),
            Some(ArgType::Color) => textcolor::color_choices(&partial).into_iter().map(|(n, c)| (n, Some(c))).collect(),
            _ => Vec::new(),
        };
        self.view = Some(View::Hint(Hint { cmd, current, preview: preview.flatten(), choices }));
    }

    pub fn move_active(&mut self, ctx: &Ctx, text: &str, by: isize) {
        if let Some(View::List { count, .. }) = self.view
            && count > 0
        {
            self.active = (self.active as isize + by).rem_euclid(count as isize) as usize;
            self.on_input(ctx, text);
        }
    }

    pub fn row(&self, i: usize) -> Option<Cmd> {
        let Some(View::List { sections, .. }) = &self.view else { return None };
        sections.iter().flat_map(|s| &s.rows).find(|r| r.index == i).map(|r| r.cmd.clone())
    }

    /// A pick: the text the draft becomes, and whether that is the send.
    pub fn select(&mut self, chat: &str, cmd: &Cmd) -> (String, bool) {
        self.suppressed = None;
        self.close();
        self.armed = Some((chat.into(), cmd.bot.clone(), cmd.name.clone()));
        if cmd.args.is_empty() {
            return (format!("/{}", cmd.name), true);
        }
        (format!("/{} ", cmd.name), false)
    }

    /// The hint's choice in place of the word being typed.
    pub fn insert_choice(text: &str, value: &str) -> String {
        let base = if text.ends_with(char::is_whitespace) {
            text.to_string()
        } else {
            let cut = text.rfind(char::is_whitespace).map_or(0, |i| i + 1);
            text[..cut].to_string()
        };
        let base = if base.starts_with('/') { base } else if text.ends_with(' ') { text.into() } else { format!("{text} ") };
        format!("{base}{value} ")
    }

    pub fn first_choice(&self) -> Option<String> {
        match &self.view {
            Some(View::Hint(h)) => h.choices.first().map(|c| c.0.clone()),
            _ => None,
        }
    }

    pub fn suppress(&mut self, text: &str) {
        self.suppressed = Some(text.into());
        self.close();
    }

    /// Send-time routing (`routeForSend`).
    pub fn route(&mut self, ctx: &Ctx, text: &str) -> Route {
        let Some(rest) = text.strip_prefix('/') else { return Route::Plain };
        if rest.starts_with('"') {
            return Route::Plain;
        }
        let Some(Ok((name, next))) = next_token(rest, 0) else { return Route::Plain };
        if name.is_empty() {
            return Route::Plain;
        }
        let Some(cmd) = self.resolve(ctx, &name) else { return Route::Plain };
        let after = &rest[next..];
        if let Some(t) = cmd.builtin.and_then(|b| builtin_text(b, after.trim())) {
            self.bump(&cmd);
            self.armed = None;
            self.close();
            return Route::Text(t);
        }
        if cmd.builtin == Some(Builtin::Effect) {
            let picked = self.armed.as_ref().is_some_and(|a| a.0 == ctx.chat && a.1 == SYSTEM && a.2 == cmd.name);
            return match textcolor::command(text) {
                Some(c) if c.body.is_ok() => {
                    self.bump(&cmd);
                    self.armed = None;
                    self.close();
                    textcolor::command_to_markup(text).map_or(Route::Plain, Route::Text)
                }
                Some(c) if picked => Route::Error(format!("/{} {}", cmd.name, c.body.err().unwrap_or_default())),
                _ => Route::Plain,
            };
        }
        if cmd.is_system() {
            return Route::Plain;
        }
        let Some(parsed) = parse_args(&cmd, after) else {
            return Route::Error(format!("Unclosed quote in /{name}"));
        };
        if let Some(e) = validate(&cmd, &parsed) {
            return Route::Error(format!("/{name}: {e}"));
        }
        self.bump(&cmd);
        self.armed = None;
        self.close();
        Route::Bot(cmd.bot)
    }
}

const PANEL_BG: u32 = 0x0a0a0a;

fn arg_chip(label: &str, optional: bool, current: bool) -> gpui::Div {
    div()
        .flex_shrink_0()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(4.))
        .text_size(px(12.))
        .line_height(px(24.))
        .when(current, |d| d.bg(c(PRIMARY)).text_color(c(PANEL_BG)).font_weight(FontWeight::SEMIBOLD))
        .when(!current, |d| d.bg(ca(255, 255, 255, 0.08)).text_color(ca(255, 255, 255, 0.75)))
        .when(optional && !current, |d| d.opacity(0.55))
        .child(label.to_string())
}

#[derive(Clone)]
pub enum Face {
    /// Vector's own commands wear the logo mark, a tall shape rather than a face.
    Mark,
    Path(std::path::PathBuf),
    Placeholder,
}

fn avatar(face: &Face) -> AnyElement {
    let base = |i: gpui::Img| i.size(px(16.)).flex_shrink_0();
    match face {
        Face::Mark => base(img("icons/vector-mark.svg")).object_fit(gpui::ObjectFit::Contain).into_any_element(),
        Face::Path(p) => base(img(p.clone())).rounded_full().into_any_element(),
        Face::Placeholder => base(img("icons/user-placeholder.svg")).rounded_full().into_any_element(),
    }
}

fn spinner(size: f32) -> AnyElement {
    let thick = if size < 12. { 1.5 } else { 2. };
    div()
        .size(px(size))
        .flex_shrink_0()
        .child(crate::attachments::spinner(size, thick, 0.8, 0.25, ca(255, 255, 255, 0.15), c(PRIMARY)))
        .into_any_element()
}

/// `.command-selector`, anchored above the composer.
pub fn view(
    p: &Picker,
    bot_face: &dyn Fn(&str) -> (SharedString, Option<std::path::PathBuf>),
    pick: Rc<dyn Fn(usize, &mut Window, &mut App)>,
    choose: Rc<dyn Fn(String, &mut Window, &mut App)>,
    vw: f32,
) -> Option<AnyElement> {
    let narrow = vw <= 520.;
    let view = p.view.as_ref()?;
    let panel = div()
        .id("command-selector")
        .absolute()
        .bottom(px(-1.))
        // `AnchoredPanel`'s `viewportInset`: 10px clear of a phone's edges.
        .when(vw < 900., |d| d.left(px(10.)))
        .when(vw >= 900., |d| d.left_0())
        .w_full()
        .max_w(px(422f32.min(vw - 18.)))
        .max_h(px(328.))
        .overflow_y_scroll()
        .bg(c(PANEL_BG))
        .border_1()
        .border_color(c(PRIMARY))
        .rounded(px(12.))
        .shadow(vec![gpui::BoxShadow {
            color: ca(0, 0, 0, 0.5),
            offset: gpui::point(px(0.), px(-8.)),
            blur_radius: px(32.),
            spread_radius: px(0.),
            inset: false,
        }])
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation());
    let body: AnyElement = match view {
        View::Loading(label) => panel
            .min_h(px(56.))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(12.))
                    .py(px(10.))
                    .text_size(px(13.))
                    .text_color(ca(255, 255, 255, 0.65))
                    .child(spinner(14.))
                    .child(label.clone()),
            )
            .into_any_element(),
        View::List { sections, active, .. } => panel
            .pb(px(6.))
            .children(sections.iter().map(|s| {
                let header = div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(12.))
                    .pt(px(8.))
                    .pb(px(4.))
                    .bg(c(PANEL_BG))
                    .text_size(px(11.))
                    .line_height(px(24.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c(PRIMARY))
                    .children(s.avatar.as_ref().map(avatar))
                    .child(div().opacity(0.7).min_w_0().truncate().child(s.title.to_uppercase()))
                    .when(s.refreshing, |d| {
                        d.child(
                            div()
                                .ml_auto()
                                .flex()
                                .items_center()
                                .gap(px(5.))
                                .text_size(px(10.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(ca(255, 255, 255, 0.42))
                                .child(spinner(9.))
                                .child("Checking for Updates"),
                        )
                    });
                div().flex().flex_col().child(header).children(s.rows.iter().map(|r| {
                    let i = r.index;
                    let pick = pick.clone();
                    let on = i == *active;
                    let face = r.show_bot.then(|| {
                        if r.cmd.is_system() {
                            avatar(&Face::Mark)
                        } else {
                            avatar(&bot_face(&r.cmd.bot).1.map_or(Face::Placeholder, Face::Path))
                        }
                    });
                    div()
                        .id(SharedString::from(format!("cmd-{}", r.cmd.key())))
                        .relative()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_x(px(8.))
                        .gap_y(px(2.))
                        .px(px(12.))
                        .py(px(8.))
                        .min_w_0()
                        .cursor_pointer()
                        .text_size(px(14.))
                        .line_height(px(24.))
                        .text_color(ca(255, 255, 255, 0.9))
                        .when(on, |d| d.bg(ca(255, 255, 255, 0.08)))
                        .hover(|d| d.bg(ca(255, 255, 255, 0.08)))
                        .when(on, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .top(px(4.))
                                    .bottom(px(4.))
                                    .w(px(3.))
                                    .rounded_r(px(3.))
                                    .bg(c(PRIMARY)),
                            )
                        })
                        .on_mouse_down(gpui::MouseButton::Left, move |_, w, cx| {
                            cx.stop_propagation();
                            pick(i, w, cx)
                        })
                        .children(face)
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(gpui::white())
                                .child(format!("/{}", r.cmd.name)),
                        )
                        .children(r.cmd.args.iter().map(|a| arg_chip(&a.name, !a.required, false)))
                        .when(!r.cmd.description.is_empty(), |d| {
                            d.child(
                                div()
                                    .min_w_0()
                                    .map(|d| {
                                        if narrow {
                                            d.flex_basis(gpui::relative(1.)).mt(px(-1.)).line_height(px(15.)).line_clamp(2)
                                        } else {
                                            d.flex_1().truncate().text_right()
                                        }
                                    })
                                    .text_size(px(12.))
                                    .text_color(ca(255, 255, 255, 0.45))
                                    .child(r.cmd.description.clone()),
                            )
                        })
                }))
            }))
            .into_any_element(),
        View::Hint(h) => {
            let cur = h.cmd.args.get(h.current);
            panel
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(12.))
                        .pt(px(8.))
                        .pb(px(6.))
                        .text_size(px(14.))
                        .line_height(px(24.))
                        .min_w_0()
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(gpui::white())
                                .child(format!("/{}", h.cmd.name)),
                        )
                        .children(h.cmd.args.iter().enumerate().map(|(i, a)| arg_chip(&a.name, !a.required, i == h.current)))
                        .when(!h.cmd.description.is_empty(), |d| {
                            d.child(
                                div()
                                    .ml_auto()
                                    .pl(px(8.))
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(12.))
                                    .text_color(ca(255, 255, 255, 0.45))
                                    .child(h.cmd.description.clone()),
                            )
                        }),
                )
                .children(h.preview.as_ref().map(|stops| {
                    let n = stops.len().max(2) - 1;
                    div().h(px(4.)).mx(px(12.)).mb(px(6.)).rounded(px(2.)).flex().overflow_hidden().children(
                        stops.windows(2).map(|w| {
                            div().flex_1().h_full().bg(gpui::linear_gradient(
                                90.,
                                gpui::linear_color_stop(w[0], 0.),
                                gpui::linear_color_stop(w[1], 1.),
                            ))
                        }).collect::<Vec<_>>(),
                    ).when(n == 0, |d| d)
                }))
                .children(cur.filter(|a| !a.description.is_empty()).map(|a| {
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(6.))
                        .px(px(12.))
                        .pb(px(8.))
                        .text_size(px(12.))
                        .line_height(px(24.))
                        .text_color(ca(255, 255, 255, 0.6))
                        .child(div().flex_shrink_0().font_weight(FontWeight::SEMIBOLD).text_color(c(PRIMARY)).child(a.name.clone()))
                        .child(a.description.clone())
                }))
                .when(!h.choices.is_empty(), |d| {
                    d.child(div().flex().flex_wrap().gap(px(6.)).px(px(12.)).pt(px(2.)).pb(px(8.)).children(
                        h.choices.iter().map(|(v, color)| {
                            let choose = choose.clone();
                            let value = v.clone();
                            div()
                                .id(SharedString::from(format!("choice-{v}")))
                                .flex()
                                .items_center()
                                .px(px(8.))
                                .py(px(2.))
                                .rounded(px(10.))
                                .bg(ca(255, 255, 255, 0.08))
                                .border_1()
                                .border_color(ca(255, 255, 255, 0.12))
                                .text_size(px(12.))
                                .line_height(px(16.))
                                .text_color(ca(255, 255, 255, 0.85))
                                .cursor_pointer()
                                .hover(|d| d.bg(c(PRIMARY)).text_color(c(PANEL_BG)))
                                .on_mouse_down(gpui::MouseButton::Left, move |_, w, cx| {
                                    cx.stop_propagation();
                                    choose(value.clone(), w, cx)
                                })
                                .children(color.map(|c| div().size(px(8.)).mr(px(5.)).rounded_full().bg(c)))
                                .child(v.clone())
                        }),
                    ))
                })
                .into_any_element()
        }
    };
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(f: &'a dyn Fn(&str) -> (SharedString, Option<std::path::PathBuf>)) -> Ctx<'a> {
        Ctx { chat: "c", snapshot: None, loading: false, effects: true, bot_name: f }
    }

    #[test]
    fn routes_builtins() {
        let f = |_: &str| (SharedString::from(""), None);
        let ctx = ctx(&f);
        let mut p = Picker::default();
        assert_eq!(p.route(&ctx, "/shrug ok"), Route::Text("ok ¯\\_(ツ)_/¯".into()));
        assert_eq!(p.route(&ctx, "/tableflip"), Route::Text("(╯°□°)╯︵ ┻━┻".into()));
        assert_eq!(p.route(&ctx, "/rainbow hi"), Route::Text("<rainbow>hi</rainbow>".into()));
        assert_eq!(p.route(&ctx, "/color me impressed"), Route::Plain);
        assert_eq!(p.route(&ctx, "/nope x"), Route::Plain);
        assert_eq!(p.recents, vec!["system:rainbow", "system:flip", "system:shrug"]);
        p.armed = Some(("c".into(), SYSTEM.into(), "gradient".into()));
        assert!(matches!(p.route(&ctx, "/gradient red x"), Route::Error(_)));
    }

    #[test]
    fn lists_and_hints() {
        let f = |_: &str| (SharedString::from(""), None);
        let ctx = ctx(&f);
        let mut p = Picker::default();
        p.on_input(&ctx, "/fl");
        let Some(View::List { sections, .. }) = &p.view else { panic!() };
        let names: Vec<&str> = sections[0].rows.iter().map(|r| r.cmd.name.as_str()).collect();
        assert_eq!(names, ["flip", "unflip", "coinflip"]);
        p.on_input(&ctx, "/gradient re");
        let Some(View::Hint(h)) = &p.view else { panic!() };
        assert_eq!(h.current, 0);
        assert_eq!(h.choices[0].0, "red");
        p.on_input(&ctx, "/time fri");
        assert!(p.view.is_none());
        assert_eq!(Picker::insert_choice("/gradient re", "red"), "/gradient red ");
    }

    #[test]
    fn parses_quoted_args() {
        let cmd = Cmd {
            bot: "b".into(),
            name: "say".into(),
            description: String::new(),
            args: vec![arg("who", ArgType::String, true, String::new()), arg("what", ArgType::String, true, String::new())],
            builtin: None,
            aliases: &[],
        };
        let got = parse_args(&cmd, r#" "a \"b\"" rest of it "#).unwrap();
        assert_eq!(got, vec![("who".into(), "a \"b\"".into()), ("what".into(), "rest of it".into())]);
        assert!(parse_args(&cmd, " \"open").is_none());
    }
}
