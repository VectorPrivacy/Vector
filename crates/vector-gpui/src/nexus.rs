//! The Nexus: Vector's Mini App store (`components/marketplace/*`, `lib/nexus.js`). Beside the
//! rail its own column holds the search, pages and categories; the page beside it is a view,
//! a search, or one app in full.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AnimationExt, AnyElement, App, Entity, FontWeight, Hsla, ObjectFit, ScrollHandle, SharedString, Window, div, img,
    prelude::*, px, svg,
};

use crate::backend::market::MarketplaceApp;
use crate::composer::TextInput;
use crate::theme::*;

const BG: u32 = 0x030303;
const RAISE: u32 = 0x0c0c0c;
const RAISE_2: u32 = 0x131313;
const FILL: u32 = 0x1b1b1b;
const FILL_2: u32 = 0x252525;
const LINE: u32 = 0x191919;
const LINE_2: u32 = 0x262626;
const DIM: u32 = 0xb2b2b2;
const MUTE: u32 = 0x7a7a7a;
const CTA: u32 = ACCENT;
const FAIR: u32 = 0xfca559;
const DANGER: u32 = 0xff2ea9;
const FEW: usize = 8;
const PAGE_MAX: f32 = 1040.;

#[derive(Clone, PartialEq, Debug)]
pub enum View {
    Discover,
    Library,
    Updates,
    All,
    Category(String),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Installing,
    Updating,
    Launching,
    Uninstalling,
    Failed,
}

#[derive(Clone)]
pub struct Action {
    pub kind: Kind,
    pub label: SharedString,
    pub pct: f32,
}

/// What this device plays with.
#[derive(Clone, Copy)]
pub struct Device {
    pub touch: bool,
    pub keys: bool,
    pub pad: bool,
}

pub struct Nexus {
    pub apps: Vec<MarketplaceApp>,
    pub view: View,
    pub details: Option<String>,
    pub search: Entity<TextInput>,
    pub loading: bool,
    pub error: String,
    pub fit_only: bool,
    pub actions: HashMap<String, Action>,
    /// `(id, last played ms, opens)`, newest first.
    pub played: Vec<(String, i64, i64)>,
    pub more: bool,
    pub device: Device,
    pub spot: usize,
    pub spot_since: Instant,
    pub spot_held: bool,
    /// Each icon's colour, for the spotlight's wash.
    pub wash: HashMap<String, Hsla>,
    pub scroll: ScrollHandle,
    pub shelves: HashMap<&'static str, ScrollHandle>,
    /// When the page last changed: it rises in.
    pub turned: Instant,
    pub opened: Instant,
    /// Where the list was when an app opened over it, so Back lands there.
    pub list_top: Option<(String, gpui::Point<gpui::Pixels>)>,
    /// Relative icon paths resolve here (fixtures).
    pub root: Option<PathBuf>,
    pub now_ms: i64,
    /// The spotlight turn a wake is already set for.
    pub spot_armed: Option<Instant>,
    /// Play with Friends: the app going out, and the chat search.
    pub picker: Option<(String, Entity<TextInput>)>,
    /// The open page's permission toggles: its app id, then `(id, label, description, granted)` rows.
    pub perms: Option<(String, Result<Vec<(String, String, String, bool)>, String>)>,
    /// This account's npub: its own listings can be edited.
    pub me: String,
    /// A link's app the catalogue doesn't hold yet: its page opens when it arrives.
    pub awaited: Option<String>,
    /// A phone: no nav column, a back header over a search and a strip of chips.
    pub slim: bool,
    /// The page is 600px or less (`@container nx`): one column, smaller cards.
    pub narrow: bool,
    pub chips: ScrollHandle,
}

pub enum Act {
    Show(View),
    Details(String),
    CloseDetails,
    Close,
    Retry,
    Card(String),
    Uninstall(String),
    UpdateAll,
    FitOnly(bool),
    More,
    Spot(usize),
    SpotHold(bool),
    Shelf(&'static str, f32),
    PlayWith(String),
    Publisher(String),
    OpenUrl(String),
    CopyLink(String),
    ClearSearch,
    Menu(String, gpui::Point<gpui::Pixels>),
    Perm(String, bool),
    ResetPerms,
    EditListing(String),
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

fn click(on: &OnAct, act: impl Fn() -> Act + 'static) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| on(act(), w, cx)
}

// ---- lib/nexus.js ----

pub const TOUCH: [&str; 6] = ["mobile", "touch", "touchscreen", "tap", "mobile-only", "touch-only"];
pub const PAD: [&str; 4] = ["controller", "gamepad", "controller-only", "gamepad-only"];
pub const KEYS: [&str; 3] = ["keyboard", "mouse", "kbm"];
pub const ONLY: [&str; 4] = ["mobile-only", "touch-only", "controller-only", "gamepad-only"];

fn canonical(t: &str) -> &str {
    if ONLY.contains(&t) {
        t
    } else if TOUCH.contains(&t) {
        "mobile"
    } else if PAD.contains(&t) {
        "controller"
    } else if KEYS.contains(&t) {
        "keyboard"
    } else {
        t
    }
}

pub struct Controls {
    pub touch: bool,
    pub pad: bool,
    pub keys: bool,
    pub only: bool,
}

pub fn controls_of(app: &MarketplaceApp) -> Controls {
    let has = |set: &[&str]| app.categories.iter().any(|t| set.contains(&t.as_str()));
    Controls { touch: has(&TOUCH), pad: has(&PAD), keys: has(&KEYS), only: has(&ONLY) }
}

pub fn fits(app: &MarketplaceApp, d: Device) -> bool {
    let c = controls_of(app);
    (d.keys && !c.only) || (d.touch && c.touch) || (d.pad && c.pad)
}

pub fn needs_label(app: &MarketplaceApp) -> String {
    let c = controls_of(app);
    let parts: Vec<&str> = [(c.keys, "a keyboard"), (c.pad, "a controller"), (c.touch, "a touchscreen")]
        .into_iter()
        .filter_map(|(on, s)| on.then_some(s))
        .collect();
    if parts.is_empty() { "a keyboard and mouse".into() } else { parts.join(" or ") }
}

pub fn is_game(app: &MarketplaceApp) -> bool {
    app.categories.iter().any(|c| c == "game") || !app.categories.iter().any(|c| c == "app")
}

pub fn is_installed(app: &MarketplaceApp) -> bool {
    app.installed || app.local_path.is_some()
}

pub fn is_multiplayer(app: &MarketplaceApp) -> bool {
    app.categories.iter().any(|c| c == "multiplayer")
}

fn in_category(app: &MarketplaceApp, cat: &str) -> bool {
    match cat {
        "app" => !is_game(app),
        "mobile" => controls_of(app).touch,
        "controller" => controls_of(app).pad,
        "keyboard" => controls_of(app).keys,
        _ => app.categories.iter().any(|c| c == cat),
    }
}

pub fn tags_of(app: &MarketplaceApp) -> impl Iterator<Item = &String> {
    app.categories.iter().filter(|c| *c != "game" && *c != "app")
}

pub fn tag_label(tag: &str) -> String {
    let said = match tag {
        "2d" => "2D",
        "3d" => "3D",
        "fps" => "FPS",
        "rpg" => "RPG",
        "mmo" => "MMO",
        "pvp" => "PvP",
        "dnd" => "D&D",
        "asmr" => "ASMR",
        "io" => "IO",
        "kbm" => "Keyboard & Mouse",
        _ => "",
    };
    if !said.is_empty() {
        return said.into();
    }
    let mut out = String::with_capacity(tag.len());
    let mut up = true;
    for ch in tag.chars() {
        if up {
            out.extend(ch.to_uppercase());
        } else {
            out.push(ch);
        }
        up = ch == ' ' || ch == '-';
    }
    out
}

fn lead_tag(app: &MarketplaceApp) -> String {
    let tags: Vec<&String> = tags_of(app).collect();
    tags.iter()
        .find(|t| t.as_str() != "multiplayer")
        .or(tags.first())
        .map(|t| t.to_string())
        .unwrap_or_else(|| if is_game(app) { "game".into() } else { "app".into() })
}

fn newest_first<'a>(list: impl Iterator<Item = &'a MarketplaceApp>) -> Vec<&'a MarketplaceApp> {
    let mut v: Vec<_> = list.collect();
    v.sort_by(|a, b| b.published_at.cmp(&a.published_at));
    v
}

fn tag_counts<'a>(apps: impl Iterator<Item = &'a MarketplaceApp>) -> Vec<(String, usize)> {
    let mut count: Vec<(String, usize)> = Vec::new();
    for app in apps {
        let mut seen: Vec<&str> = Vec::new();
        for t in tags_of(app) {
            let t = canonical(t);
            if seen.contains(&t) {
                continue;
            }
            seen.push(t);
            match count.iter_mut().find(|c| c.0 == t) {
                Some(c) => c.1 += 1,
                None => count.push((t.to_string(), 1)),
            }
        }
    }
    count.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    count
}

fn matches(app: &MarketplaceApp, q: &str) -> bool {
    q.is_empty()
        || app.name.to_lowercase().contains(q)
        || app.categories.iter().any(|c| c.contains(q) || tag_label(c).to_lowercase().contains(q))
        || app.developer.as_deref().unwrap_or("").to_lowercase().contains(q)
        || app.description.to_lowercase().contains(q)
}

fn search_rank(app: &MarketplaceApp, q: &str) -> u8 {
    let name = app.name.to_lowercase();
    if name == q {
        0
    } else if name.starts_with(q) {
        1
    } else if name.contains(q) {
        2
    } else if app.categories.iter().any(|c| c.contains(q)) {
        3
    } else {
        4
    }
}

pub fn action_text(app: &MarketplaceApp) -> &'static str {
    if is_game(app) { "Play" } else { "Open" }
}

pub fn launching_text(app: &MarketplaceApp) -> &'static str {
    if is_game(app) { "Launching..." } else { "Opening..." }
}

/// "just now", "5m ago", "2d ago", "3 Oct".
pub fn played_ago(ms: i64, now: i64) -> String {
    let s = ((now - ms).max(0) / 1000) as i64;
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", s / 60),
        3600..86400 => format!("{}h ago", s / 3600),
        86400..604800 => format!("{}d ago", s / 86400),
        _ => chrono::DateTime::from_timestamp_millis(ms)
            .map(|d| d.with_timezone(&chrono::Local).format("%-d %b").to_string())
            .unwrap_or_default(),
    }
}

fn publish_date(secs: u64) -> String {
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%b %-d, %Y").to_string())
        .unwrap_or_default()
}

fn source_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) => {
            let mut s = u.host_str().unwrap_or("").to_string();
            let path = u.path();
            if !path.is_empty() && path != "/" {
                if path.chars().count() > 20 {
                    s.extend(path.chars().take(20));
                    s.push_str("...");
                } else {
                    s.push_str(path);
                }
            }
            s
        }
        Err(_) if url.chars().count() > 30 => format!("{}...", url.chars().take(30).collect::<String>()),
        Err(_) => url.to_string(),
    }
}

impl Nexus {
    pub fn new(search: Entity<TextInput>, root: Option<PathBuf>) -> Self {
        let fit_only = crate::backend::device::get("nexus_fit_only").as_deref() != Some("false");
        Self {
            apps: Vec::new(),
            view: View::Discover,
            details: None,
            search,
            loading: false,
            error: String::new(),
            fit_only,
            actions: HashMap::new(),
            played: Vec::new(),
            more: false,
            device: Device { touch: false, keys: true, pad: false },
            spot: 0,
            spot_since: Instant::now(),
            spot_held: false,
            wash: HashMap::new(),
            scroll: ScrollHandle::new(),
            shelves: HashMap::new(),
            turned: Instant::now(),
            opened: Instant::now(),
            list_top: None,
            root,
            now_ms: chrono::Utc::now().timestamp_millis(),
            spot_armed: None,
            picker: None,
            perms: None,
            me: String::new(),
            awaited: None,
            slim: false,
            narrow: false,
            chips: ScrollHandle::new(),
        }
    }

    pub fn shows(&self, app: &MarketplaceApp) -> bool {
        !self.fit_only || fits(app, self.device)
    }

    pub fn app(&self, id: &str) -> Option<&MarketplaceApp> {
        self.apps.iter().find(|a| a.id == id)
    }

    /// The local file for an app's icon, if it has one yet.
    pub fn icon_path(&self, app: &MarketplaceApp) -> Option<PathBuf> {
        let p = PathBuf::from(app.icon_cached.as_ref()?);
        Some(match (&self.root, p.is_relative()) {
            (Some(root), true) => root.join(p),
            _ => p,
        })
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).text().trim().to_lowercase()
    }

    fn page_key(&self, cx: &App) -> String {
        format!("{:?}|{}", self.view, self.query(cx))
    }

    /// A page: leaves any open app and the search.
    pub fn show(&mut self, view: View, cx: &mut App) {
        self.view = view;
        self.details = None;
        self.list_top = None;
        self.search.update(cx, |s, cx| s.set_text("", cx));
        self.turn();
    }

    pub fn open_details(&mut self, id: String, cx: &App) {
        if self.details.is_none() {
            self.list_top = Some((self.page_key(cx), self.scroll.offset()));
        }
        self.details = Some(id);
        self.turn();
    }

    pub fn close_details(&mut self, cx: &App) {
        self.details = None;
        let back = self.list_top.take().filter(|(k, _)| *k == self.page_key(cx));
        self.turned = Instant::now();
        self.scroll.set_offset(back.map_or(gpui::Point::default(), |(_, at)| at));
    }

    fn turn(&mut self) {
        self.turned = Instant::now();
        self.scroll.set_offset(gpui::Point::default());
    }

    pub fn set_action(&mut self, id: &str, action: Option<Action>) {
        match action {
            Some(a) => self.actions.insert(id.to_string(), a),
            None => self.actions.remove(id),
        };
    }

    pub fn patch(&mut self, id: &str, f: impl FnOnce(&mut MarketplaceApp)) {
        if let Some(a) = self.apps.iter_mut().find(|a| a.id == id) {
            f(a);
        }
    }

    /// The front page's lead: the newest to play together, topped up with the newest games.
    pub fn spotlight(&self) -> Vec<&MarketplaceApp> {
        let newest = newest_first(self.apps.iter().filter(|a| self.shows(a)));
        let games: Vec<_> = newest.iter().copied().filter(|a| is_game(a)).collect();
        let mut pick: Vec<&MarketplaceApp> = games.iter().copied().filter(|a| is_multiplayer(a)).take(5).collect();
        for g in &games {
            if pick.len() < 5 && !pick.iter().any(|p| p.id == g.id) {
                pick.push(g);
            }
        }
        let d = self.device;
        let best = |a: &MarketplaceApp| (d.pad && controls_of(a).pad) || (d.touch && !d.keys && controls_of(a).touch);
        let (mut a, b): (Vec<_>, Vec<_>) = pick.into_iter().partition(|x| best(x));
        a.extend(b);
        a
    }

    /// Whether anything here moves on its own: a ring, a rising page, the spotlight's turn.
    pub fn animating(&self) -> bool {
        self.turned.elapsed().as_secs_f32() < 0.5
            || self.opened.elapsed().as_secs_f32() < 0.3
            || self.loading
            || self.actions.values().any(|a| matches!(a.kind, Kind::Installing | Kind::Updating))
    }

    pub fn shelf(&mut self, key: &'static str) -> ScrollHandle {
        self.shelves.entry(key).or_default().clone()
    }
}

/// The spotlight's colour for an icon: the mean of its more saturated pixels.
pub fn wash_of(path: &std::path::Path) -> Option<Hsla> {
    let img = image::open(path).ok()?.thumbnail(24, 24).into_rgb8();
    let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
    for p in img.pixels() {
        let [pr, pg, pb] = p.0;
        let max = pr.max(pg).max(pb);
        let min = pr.min(pg).min(pb);
        if max > 40 && max - min > 30 {
            r += pr as u32;
            g += pg as u32;
            b += pb as u32;
            n += 1;
        }
    }
    (n > 0).then(|| ca((r / n) as u8, (g / n) as u8, (b / n) as u8, 1.))
}

// ---- pieces ----

fn glyph(name: &str, size: f32, color: Hsla) -> gpui::Svg {
    svg().path(SharedString::from(format!("icons/{name}.svg"))).size(px(size)).flex_shrink_0().text_color(color)
}

fn rise(id: impl Into<SharedString>, el: impl IntoElement, delay: f32, dur: f32, dy: f32) -> AnyElement {
    let total = delay + dur;
    div()
        .child(el)
        .with_animation(
            id.into(),
            gpui::Animation::new(std::time::Duration::from_secs_f32(total)),
            move |d, t| {
                let k = ((t * total - delay) / dur).clamp(0., 1.);
                let e = crate::kit::bezier(0.22, 1., 0.36, 1.)(k);
                d.opacity(e).relative().top(px(dy * (1. - e)))
            },
        )
        .into_any_element()
}

/// `AppIcon`: the icon, or its initial on a grey tile.
pub fn app_icon(n: &Nexus, app: &MarketplaceApp, size: f32) -> gpui::Div {
    icon(n, app, size)
}

fn icon(n: &Nexus, app: &MarketplaceApp, size: f32) -> gpui::Div {
    let radius = size * 0.225;
    let base = div()
        .relative()
        .size(px(size))
        .flex_shrink_0()
        .rounded(px(radius))
        .overflow_hidden()
        .bg(c(0x161616));
    let face = match n.icon_path(app) {
        Some(p) => img(p).size_full().rounded(px(radius)).object_fit(ObjectFit::Cover).into_any_element(),
        None => {
            let initial = app
                .name
                .chars()
                .find(|c| c.is_alphanumeric())
                .map(|c| c.to_uppercase().collect::<String>())
                .unwrap_or_else(|| "?".into());
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::linear_gradient(
                    150.,
                    gpui::linear_color_stop(c(0x262626), 0.),
                    gpui::linear_color_stop(c(0x121212), 1.),
                ))
                .text_size(px(size * 0.42))
                .line_height(px(size * 0.42))
                .font_weight(FontWeight::BOLD)
                .text_color(c(0x707070))
                .child(initial)
                .into_any_element()
        }
    };
    base.child(face).child(
        div().absolute().inset_0().rounded(px(radius)).border_1().border_color(white_a(0.07)),
    )
}

/// `AppAction`: Install, Update, or Play / Open once it's here; the download's ring meanwhile.
fn app_action(n: &Nexus, app: &MarketplaceApp, lg: bool, on: &OnAct, key: &str) -> AnyElement {
    action_button(n, app, lg, on, key).into_any_element()
}

fn action_button(n: &Nexus, app: &MarketplaceApp, lg: bool, on: &OnAct, key: &str) -> gpui::Stateful<gpui::Div> {
    let action = n.actions.get(&app.id);
    let downloading = action.is_some_and(|a| matches!(a.kind, Kind::Installing | Kind::Updating));
    let busy = action.is_some_and(|a| a.kind != Kind::Failed);
    let failed = action.is_some_and(|a| a.kind == Kind::Failed);
    let (kind_bg, kind_fg) = if failed {
        (ca(255, 46, 169, 0.15), c(DANGER))
    } else if busy {
        (c(FILL), c(DIM))
    } else if app.update_available || !is_installed(app) {
        (c(CTA), c(0x000000))
    } else {
        (c(FILL), c(PRIMARY))
    };
    let label: SharedString = match action {
        Some(a) if !downloading => a.label.clone(),
        _ if app.update_available => "Update".into(),
        _ if is_installed(app) => action_text(app).into(),
        _ => "Install".into(),
    };
    let id = app.id.clone();
    let play = !busy && !failed && is_installed(app) && !app.update_available;
    div()
        .id(SharedString::from(format!("nx-act-{key}-{}", app.id)))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .min_w(px(if lg { 108. } else { 78. }))
        .h(px(if lg { 38. } else { 30. }))
        .px(px(if lg { 22. } else { 16. }))
        .rounded_full()
        .bg(kind_bg)
        .text_color(kind_fg)
        .text_size(px(if lg { 15. } else { 13. }))
        .font_weight(FontWeight::SEMIBOLD)
        .when(!busy, |d| {
            d.cursor_pointer().hover(move |s| if play { s.bg(c(FILL_2)) } else { s.opacity(0.94) })
        })
        .on_click({
            let on = on.clone();
            move |_, w, cx| {
                cx.stop_propagation();
                if !busy {
                    on(Act::Card(id.clone()), w, cx);
                }
            }
        })
        .map(|d| {
            if downloading {
                let pct = action.map_or(0., |a| a.pct).max(4.) / 100.;
                d.child(crate::attachments::spinner(
                    if lg { 22. } else { 18. },
                    3.,
                    f32::INFINITY,
                    pct,
                    white_a(0.14),
                    c(PRIMARY),
                ))
            } else {
                d.child(label)
            }
        })
}

fn btn(id: impl Into<SharedString>, label: impl Into<SharedString>, accent: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id.into())
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .h(px(38.))
        .px(px(18.))
        .rounded_full()
        .text_size(px(14.))
        .font_weight(FontWeight::SEMIBOLD)
        .whitespace_nowrap()
        .cursor_pointer()
        .map(|d| {
            if accent {
                d.bg(c(CTA)).text_color(c(0x000000)).hover(|s| s.opacity(0.94))
            } else {
                d.bg(c(FILL)).text_color(c(0xffffff)).hover(|s| s.bg(c(FILL_2)))
            }
        })
        .child(label.into())
}

fn soft(id: impl Into<SharedString>, glyph_name: Option<&str>, label: &'static str, disabled: bool) -> gpui::Stateful<gpui::Div> {
    let mut b = div()
        .id(id.into())
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .h(px(38.))
        .px(px(18.))
        .rounded_full()
        .bg(c(FILL))
        .text_color(c(0xffffff))
        .text_size(px(14.))
        .font_weight(FontWeight::SEMIBOLD)
        .whitespace_nowrap();
    if disabled {
        b = b.opacity(0.5);
    } else {
        b = b.cursor_pointer().hover(|s| s.bg(c(FILL_2)));
    }
    if let Some(g) = glyph_name {
        b = b.child(glyph(g, 16., c(PRIMARY)));
    }
    b.child(label)
}

fn link(id: impl Into<SharedString>, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id.into())
        .flex_shrink_0()
        .text_size(px(14.))
        .line_height(px(17.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(c(PRIMARY))
        .cursor_pointer()
        .hover(|s| s.opacity(0.75))
        .child(label)
}

fn chip(id: impl Into<SharedString>, label: String) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id.into())
        .flex()
        .flex_shrink_0()
        .items_center()
        .h(px(32.))
        .px(px(14.))
        .rounded_full()
        .bg(c(FILL))
        .text_color(c(DIM))
        .text_size(px(14.))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(|s| s.bg(c(FILL_2)).text_color(c(0xffffff)))
        .child(label)
}

fn h3(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .text_size(px(18.))
        .line_height(px(22.))
        .font_weight(FontWeight::BOLD)
        .text_color(c(0xffffff))
        .child(text.into())
}

// ---- the nav column ----

fn nav_row(
    id: String,
    glyph_name: Option<&'static str>,
    label: String,
    n: usize,
    badge: bool,
    active: bool,
    act: impl Fn() -> Act + 'static,
    on: &OnAct,
) -> AnyElement {
    let group = SharedString::from(format!("nx-nav-{id}"));
    let mut row = div()
        .id(SharedString::from(format!("nx-nav-{id}")))
        .group(group.clone())
        .h(px(38.))
        .mb(px(1.))
        .pl(px(if glyph_name.is_some() { 11. } else { 34. }))
        .pr(px(11.))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(px(8.))
        .text_size(px(15.))
        .line_height(px(18.))
        .text_color(if active { c(0xffffff) } else { c(0xb2b2b2) })
        .when(active, |d| d.bg(c(0x1b1b1b)))
        .when(!active, |d| d.hover(|d| d.bg(c(0x151515)).text_color(c(0xffffff))))
        .cursor_pointer()
        .on_click(click(on, act));
    if let Some(g) = glyph_name {
        row = row.child(
            glyph(g, 16., if active { c(0xffffff) } else { c(0x5b6060) })
                .group_hover(group.clone(), |s| s.text_color(c(0xffffff))),
        );
    }
    row = row.child(div().flex_1().min_w_0().truncate().child(label));
    if badge {
        row = row.child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .min_w(px(20.))
                .h(px(20.))
                .px(px(6.))
                .rounded(px(10.))
                .bg(c(PRIMARY))
                .text_color(c(0x000000))
                .text_size(px(11.))
                .font_weight(FontWeight::BOLD)
                .child(n.to_string()),
        );
    } else if n > 0 {
        row = row.child(
            div()
                .text_size(px(12.))
                .text_color(if active { c(0x9a9a9a) } else { c(0x5b6060) })
                .group_hover(group, |s| s.text_color(c(0x9a9a9a)))
                .child(n.to_string()),
        );
    }
    row.into_any_element()
}

fn group_label(text: &'static str) -> gpui::Div {
    div()
        .mt(px(22.))
        .mx(px(8.))
        .mb(px(8.))
        .h(px(14.))
        .flex()
        .items_center()
        .text_size(px(16.))
        .line_height(px(14.))
        .text_color(c(0xffffff))
        .child(text)
}

fn nav(n: &Nexus, on: &OnAct, cx: &App) -> AnyElement {
    let q = n.query(cx);
    let here = |v: &View| q.is_empty() && n.details.is_none() && n.view == *v;
    let fit: Vec<&MarketplaceApp> = n.apps.iter().filter(|a| n.shows(a)).collect();
    let installed = n.apps.iter().filter(|a| is_installed(a)).count();
    let updates = n.apps.iter().filter(|a| a.update_available).count();
    let tools = fit.iter().filter(|a| !is_game(a)).count();
    let cats = tag_counts(fit.iter().copied());
    let mut list = div().flex().flex_col();
    list = list.child(nav_row("discover".into(), Some("globe"), "Discover".into(), 0, false, here(&View::Discover), || Act::Show(View::Discover), on));
    list = list.child(nav_row("library".into(), Some("grid"), "Library".into(), installed, false, here(&View::Library), || Act::Show(View::Library), on));
    if updates > 0 {
        list = list.child(nav_row("updates".into(), Some("arrow-up"), "Updates".into(), updates, true, here(&View::Updates), || Act::Show(View::Updates), on));
    }
    list = list.child(nav_row("all".into(), Some("align-left"), "All Apps".into(), fit.len(), false, here(&View::All), || Act::Show(View::All), on));
    if !cats.is_empty() {
        list = list.child(group_label("Categories"));
        if tools > 0 {
            let v = View::Category("app".into());
            list = list.child(nav_row("cat-app".into(), None, "Apps & Tools".into(), tools, false, here(&v), move || Act::Show(v.clone()), on));
        }
        let shown = if n.more { cats.len() } else { cats.len().min(FEW) };
        for (tag, count) in &cats[..shown] {
            let v = View::Category(tag.clone());
            list = list.child(nav_row(format!("cat-{tag}"), None, tag_label(tag), *count, false, here(&v), move || Act::Show(v.clone()), on));
        }
        if cats.len() > FEW {
            list = list.child(
                div()
                    .id("nx-nav-more")
                    .group("nx-nav-more")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(30.))
                    .pl(px(34.))
                    .pr(px(8.))
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(c(0x6f6f6f))
                    .hover(|s| s.text_color(c(0xffffff)))
                    .cursor_pointer()
                    .on_click(click(on, || Act::More))
                    .child(if n.more { "Fewer".to_string() } else { format!("{} More", cats.len() - FEW) })
                    .child(
                        glyph(if n.more { "chevron-up" } else { "chevron-down" }, 12., c(0x6f6f6f))
                            .group_hover("nx-nav-more", |s| s.text_color(c(0xffffff))),
                    ),
            );
        }
    }
    let d = n.device;
    let controls: Vec<&str> = [(d.touch, "Touch"), (d.keys, "Keyboard & Mouse"), (d.pad, "Controller")]
        .into_iter()
        .filter_map(|(on, s)| on.then_some(s))
        .collect();
    let fit_only = n.fit_only;
    list = list.child(group_label("This Device")).child(
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(8.))
            .child(
                div()
                    .flex()
                    .gap(px(10.))
                    .child(glyph("touch", 18., if d.touch { c(PRIMARY) } else { c(0x3d3d3d) }))
                    .child(glyph("keyboard", 18., if d.keys { c(PRIMARY) } else { c(0x3d3d3d) }))
                    .child(glyph("gamepad", 18., if d.pad { c(PRIMARY) } else { c(0x3d3d3d) })),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .line_height(px(24.))
                    .text_color(c(0xb2b2b2))
                    .child(if controls.is_empty() { "No controls found".to_string() } else { controls.join(" · ") }),
            )
            .when(!d.pad, |e| {
                e.child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(16.8))
                        .text_color(c(0x5b6060))
                        .child("Press a button on a controller to add it"),
                )
            })
            .child(
                div()
                    .mt(px(8.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(14.))
                    .line_height(px(24.))
                    .text_color(c(0xb2b2b2))
                    .child("Only What Fits")
                    .child(crate::settings::toggle("nx-fit", fit_only, true, click(on, move || Act::FitOnly(!fit_only)))),
            ),
    );
    let has_query = !q.is_empty();
    div()
        .w(px(crate::ui::LIST_W))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .bg(c(LIST_BG))
        .border_r_1()
        .border_color(c(DIVIDER))
        .child(
            div()
                .h(px(50.))
                .flex_shrink_0()
                .px(px(10.))
                .relative()
                .flex()
                .items_center()
                .border_b_1()
                .border_color(c(0x171717))
                .child(
                    div()
                        .w_full()
                        .h(px(34.))
                        .rounded(px(8.))
                        .bg(c(0x1f1f1f))
                        .pl(px(34.))
                        .pr(px(if has_query { 38. } else { 12. }))
                        .flex()
                        .items_center()
                        .child(div().flex_1().min_w_0().child(n.search.clone())),
                )
                .child(glyph("search", 17., c(0xb2b2b2)).absolute().left(px(20.5)).top(px(16.5)))
                .when(has_query, |d| {
                    d.child(
                        div()
                            .id("nx-search-clear")
                            .absolute()
                            .right(px(21.))
                            .top(px(16.))
                            .size(px(18.))
                            .cursor_pointer()
                            .group("nx-clear")
                            .on_click(click(on, || Act::ClearSearch))
                            .child(
                                svg()
                                    .path("ui/search-clear.svg")
                                    .size_full()
                                    .text_color(c(0xb2b2b2))
                                    .group_hover("nx-clear", |d| d.text_color(c(0xffffff))),
                            ),
                    )
                }),
        )
        .child(
            div()
                .id("nx-nav-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .pt(px(18.))
                .px(px(10.))
                .pb(px(24.))
                .child(
                    div()
                        .mx(px(10.))
                        .mb(px(17.))
                        .pb(px(16.))
                        .border_b_1()
                        .border_color(c(0x262626))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(glyph("nexus", 22., c(0xffffff)).ml(px(-2.)))
                        .child(
                            div()
                                .text_size(px(14.))
                                .line_height(px(17.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(c(0xffffff))
                                .child("App Store"),
                        ),
                )
                .child(list),
        )
        .into_any_element()
}

// ---- pages ----

/// `AppRow`: icon, name, what it is, a line about it, and its action.
fn row(n: &Nexus, app: &MarketplaceApp, i: usize, note: Option<String>, on: &OnAct, key: &str) -> AnyElement {
    let id = app.id.clone();
    let menu_id = app.id.clone();
    let mut sub = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .mt(px(1.))
        .text_size(px(12.))
        .line_height(px(16.))
        .text_color(c(MUTE));
    if is_multiplayer(app) {
        sub = sub.child(glyph("users-multi", 13., c(PRIMARY)));
    }
    sub = sub.child(tag_label(&lead_tag(app)));
    let dot = || div().size(px(3.)).rounded_full().bg(c(MUTE)).opacity(0.6);
    if app.update_available {
        sub = sub.child(dot()).child(div().text_color(c(PRIMARY)).child("Update ready"));
    } else if !fits(app, n.device) {
        sub = sub.child(dot()).child(div().text_color(c(FAIR)).child(format!("For {}", needs_label(app))));
    }
    let on2 = on.clone();
    let el = div()
        .id(SharedString::from(format!("nx-row-{key}-{}", app.id)))
        .flex()
        .items_center()
        .gap(px(14.))
        .min_w_0()
        .mx(px(-10.))
        .p(px(10.))
        .rounded(px(14.))
        .cursor_pointer()
        .hover(|s| s.bg(c(RAISE_2)))
        .on_click(click(on, move || Act::Details(id.clone())))
        .on_mouse_down(gpui::MouseButton::Right, move |e, w, cx| on2(Act::Menu(menu_id.clone(), e.position), w, cx))
        .child(icon(n, app, 56.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .truncate()
                        .text_size(px(15.))
                        .line_height(px(19.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(c(0xffffff))
                        .child(app.name.clone()),
                )
                .child(sub)
                .child(
                    div()
                        .mt(px(3.))
                        .truncate()
                        .text_size(px(13.))
                        .line_height(px(17.))
                        .text_color(c(DIM))
                        .child(note.unwrap_or_else(|| app.description.clone())),
                ),
        )
        .child(app_action(n, app, false, on, key));
    let delay = i.min(14) as f32 * 0.018;
    rise(SharedString::from(format!("nx-rise-{key}-{}-{:?}", app.id, n.turned)), el, delay, 0.4, 8.)
}

/// `.nx-rows`: as many 320px-or-wider columns as fit, 18px apart.
fn rows(items: Vec<AnyElement>, width: f32) -> AnyElement {
    let cols = (((width + 18.) / (320. + 18.)).floor() as usize).max(1);
    let mut grid = div().flex().flex_col().gap(px(2.));
    let mut it = items.into_iter().peekable();
    while it.peek().is_some() {
        let mut line = div().flex().gap(px(18.));
        for _ in 0..cols {
            line = line.child(div().flex_1().min_w_0().children(it.next()));
        }
        grid = grid.child(line);
    }
    grid.into_any_element()
}

fn tile(n: &Nexus, app: &MarketplaceApp, sub: Option<String>, on: &OnAct, key: &str) -> AnyElement {
    let id = app.id.clone();
    let menu_id = app.id.clone();
    let group = SharedString::from(format!("nx-tile-{key}-{}", app.id));
    let on2 = on.clone();
    div()
        .id(group.clone())
        .group(group.clone())
        .flex()
        .flex_col()
        .items_start()
        .flex_shrink_0()
        .w(px(if n.narrow { 84. } else { 100. }))
        .cursor_pointer()
        .on_click(click(on, move || Act::Details(id.clone())))
        .on_mouse_down(gpui::MouseButton::Right, move |e, w, cx| on2(Act::Menu(menu_id.clone(), e.position), w, cx))
        .child(icon(n, app, if n.narrow { 72. } else { 84. }).group_hover(group, |s| {
            s.top(px(-3.)).shadow(vec![gpui::BoxShadow {
                color: ca(0, 0, 0, 0.55),
                offset: gpui::point(px(0.), px(10.)),
                blur_radius: px(22.),
                spread_radius: px(0.),
                inset: false,
            }])
        }))
        .child(
            div()
                .w_full()
                .mt(px(9.))
                .truncate()
                .text_size(px(13.))
                .line_height(px(16.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(c(0xffffff))
                .child(app.name.clone()),
        )
        .child(
            div()
                .w_full()
                .truncate()
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(c(MUTE))
                .child(sub.unwrap_or_else(|| tag_label(&lead_tag(app)))),
        )
        .into_any_element()
}

/// `Shelf`: a titled row that scrolls sideways, with See All and arrows.
fn shelf(
    narrow: bool,
    key: &'static str,
    title: &str,
    glyph_name: Option<&str>,
    more: Option<Act>,
    tiles: Vec<AnyElement>,
    handle: &ScrollHandle,
    on: &OnAct,
) -> AnyElement {
    let at = -f32::from(handle.offset().x);
    let max = f32::from(handle.max_offset().x);
    let at_start = at <= 2.;
    let at_end = at >= max - 2.;
    let arrow = |dir: f32, disabled: bool| {
        div()
            .id(SharedString::from(format!("nx-arrow-{key}-{dir}")))
            .flex()
            .items_center()
            .justify_center()
            .size(px(28.))
            .rounded_full()
            .bg(c(FILL))
            .when(disabled, |d| d.opacity(0.3))
            .when(!disabled, |d| {
                d.cursor_pointer()
                    .hover(|s| s.bg(c(FILL_2)))
                    .on_click(click(on, move || Act::Shelf(key, dir)))
            })
            .child(glyph(if dir < 0. { "chevron-left" } else { "chevron-right" }, 14., c(0xffffff)))
    };
    let mut head = div().flex().items_center().gap(px(14.)).mb(px(12.)).h(px(28.));
    let mut title_el = h3(title.to_string());
    if let Some(g) = glyph_name {
        title_el = div().flex().items_center().child(glyph(g, 18., c(PRIMARY)).mr(px(8.))).child(title_el);
    }
    head = head.child(title_el).child(div().flex_1());
    if let Some(more) = more {
        let more = Rc::new(std::cell::RefCell::new(Some(more)));
        let on = on.clone();
        head = head.child(link(SharedString::from(format!("nx-more-{key}")), "See All").on_click(move |_, w, cx| {
            if let Some(a) = more.borrow().as_ref().map(clone_act) {
                on(a, w, cx);
            }
        }));
    }
    head = head.child(div().flex().gap(px(6.)).child(arrow(-1., at_start)).child(arrow(1., at_end)));
    let fade = |left: bool, on: bool| {
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(32.))
            .when(left, |d| d.left_0())
            .when(!left, |d| d.right_0())
            .when(on, |d| {
                d.bg(gpui::linear_gradient(
                    if left { 90. } else { 270. },
                    gpui::linear_color_stop(c(BG), 0.),
                    gpui::linear_color_stop(ca(3, 3, 3, 0.), 1.),
                ))
            })
    };
    div()
        .mt(px(if narrow { 30. } else { 36. }))
        .child(head)
        .child(
            div()
                .relative()
                .mt(px(-6.))
                .mb(px(-10.))
                .child(
                    div()
                        .id(SharedString::from(format!("nx-track-{key}")))
                        .flex()
                        .gap(px(18.))
                        .pt(px(6.))
                        .pb(px(10.))
                        .overflow_x_scroll()
                        .track_scroll(handle)
                        .children(tiles),
                )
                .child(fade(true, !at_start))
                .child(fade(false, !at_end && max > 0.)),
        )
        .into_any_element()
}

fn clone_act(a: &Act) -> Act {
    match a {
        Act::Show(v) => Act::Show(v.clone()),
        _ => Act::Show(View::All),
    }
}

fn spotlight(n: &Nexus, apps: &[&MarketplaceApp], on: &OnAct) -> Option<AnyElement> {
    let at = n.spot.min(apps.len().checked_sub(1)?);
    let shown = apps[at];
    let wash = n.wash.get(&shown.id).copied().unwrap_or(c(PRIMARY));
    let mut tags = div().flex().flex_wrap().gap(px(6.)).mt(px(14.));
    for t in tags_of(shown).take(3) {
        tags = tags.child(
            div()
                .flex()
                .items_center()
                .h(px(24.))
                .px(px(10.))
                .rounded_full()
                .bg(white_a(0.09))
                .text_color(white_a(0.82))
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .child(tag_label(t)),
        );
    }
    let id = shown.id.clone();
    let second = if is_multiplayer(shown) && is_installed(shown) {
        let id = id.clone();
        soft("nx-spot-friends", Some("users-multi"), "Play with Friends", false)
            .on_click(click(on, move || Act::PlayWith(id.clone())))
    } else {
        let id = id.clone();
        soft("nx-spot-details", None, "Details", false).on_click(click(on, move || Act::Details(id.clone())))
    };
    let narrow = n.narrow;
    let art_size = if narrow { 76. } else { 132. };
    let art = || {
        div()
        .flex_shrink_0()
        .shadow(vec![gpui::BoxShadow {
            color: ca(0, 0, 0, 0.6),
            offset: gpui::point(px(0.), px(20.)),
            blur_radius: px(40.),
            spread_radius: px(0.),
            inset: false,
        }])
        .rounded(px(art_size * 0.225))
        .child(icon(n, shown, art_size))
    };
    let body = div()
        .flex()
        .when(narrow, |d| d.flex_col().items_start().gap(px(14.)).pt(px(20.)).px(px(18.)).pb(px(40.)).child(art()))
        .when(!narrow, |d| d.items_center().gap(px(28.)).pt(px(32.)).px(px(36.)).pb(px(44.)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .h(px(13.))
                        .mt(px(if narrow { 14. } else { 7. }))
                        .flex()
                        .text_size(px(11.))
                        .line_height(px(13.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(c(PRIMARY))
                        .child(crate::kit::spaced(
                            if is_multiplayer(shown) { "PLAY TOGETHER" } else { "NEW IN THE APP STORE" },
                            11. * 0.14,
                        )),
                )
                .child(
                    div()
                        .mt(px(if narrow { 12. } else { 14. }))
                        .mb(px(6.))
                        .text_size(px(if narrow { 25. } else { 34. }))
                        .line_height(px(if narrow { 29. } else { 38. }))
                        .font_weight(FontWeight::BOLD)
                        .text_color(c(0xffffff))
                        .child(shown.name.clone()),
                )
                .child(
                    div()
                        .max_w(px(470.))
                        .text_size(px(14.))
                        .line_height(px(21.))
                        .text_color(white_a(0.74))
                        .line_clamp(2)
                        .child(shown.description.clone()),
                )
                .child(tags)
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(10.))
                        .mt(px(20.))
                        .child(app_action(n, shown, true, on, "spot"))
                        .child(second),
                ),
        )
        .when(!narrow, |d| d.child(art()));
    let body = rise(SharedString::from(format!("nx-spot-{}", shown.id)), body, 0., 0.5, 8.);
    let mut dots = div()
        .absolute()
        .left(px(if narrow { 18. } else { 36. }))
        .bottom(px(if narrow { 16. } else { 18. }))
        .flex()
        .gap(px(6.));
    if apps.len() > 1 {
        for i in 0..apps.len() {
            dots = dots.child(
                div()
                    .id(SharedString::from(format!("nx-dot-{i}")))
                    .h(px(6.))
                    .w(px(if i == at { 20. } else { 6. }))
                    .rounded(px(3.))
                    .bg(if i == at { c(0xffffff) } else { white_a(0.28) })
                    .cursor_pointer()
                    .on_click({
                        let on = on.clone();
                        move |_, w, cx| {
                            cx.stop_propagation();
                            on(Act::Spot(i), w, cx)
                        }
                    }),
            );
        }
    }
    let radius = if narrow { 18. } else { 20. };
    let (on_in, on_out) = (on.clone(), on.clone());
    let menu_id = shown.id.clone();
    let on_menu = on.clone();
    Some(
        div()
            .id("nx-spot")
            .relative()
            .overflow_hidden()
            .when(!narrow, |d| d.min_h(px(250.)))
            .rounded(px(radius))
            .bg(c(0x0b0b0b))
            .cursor_pointer()
            .on_hover(move |hovered, w, cx| {
                if *hovered { on_in(Act::SpotHold(true), w, cx) } else { on_out(Act::SpotHold(false), w, cx) }
            })
            .on_click(click(on, move || Act::Details(id.clone())))
            .on_mouse_down(gpui::MouseButton::Right, move |e, w, cx| on_menu(Act::Menu(menu_id.clone(), e.position), w, cx))
            .child(div().absolute().inset_0().rounded(px(radius)).bg(if narrow {
                gpui::linear_gradient(
                    180.,
                    gpui::linear_color_stop(wash.opacity(0.6), 0.),
                    gpui::linear_color_stop(ca(3, 3, 3, 0.), 0.7),
                )
            } else {
                gpui::linear_gradient(
                    90.,
                    gpui::linear_color_stop(ca(3, 3, 3, 0.), 0.3),
                    gpui::linear_color_stop(wash.opacity(0.6), 0.85),
                )
            }))
            .child(div().absolute().inset_0().rounded(px(radius)).bg(if narrow {
                gpui::linear_gradient(
                    180.,
                    gpui::linear_color_stop(ca(3, 3, 3, 0.3), 0.),
                    gpui::linear_color_stop(ca(3, 3, 3, 0.9), 0.65),
                )
            } else {
                gpui::linear_gradient(
                    90.,
                    gpui::linear_color_stop(ca(3, 3, 3, 0.94), 0.),
                    gpui::linear_color_stop(ca(3, 3, 3, 0.2), 1.),
                )
            }))
            .child(body)
            .child(dots)
            .child(div().absolute().inset_0().rounded(px(radius)).border_1().border_color(c(LINE_2)))
            .into_any_element(),
    )
}

fn discover(n: &Nexus, width: f32, on: &OnAct) -> Vec<AnyElement> {
    let fit: Vec<&MarketplaceApp> = n.apps.iter().filter(|a| n.shows(a)).collect();
    let newest = newest_first(fit.iter().copied());
    let games: Vec<&MarketplaceApp> = newest.iter().copied().filter(|a| is_game(a)).collect();
    let together: Vec<&MarketplaceApp> = games.iter().copied().filter(|a| is_multiplayer(a)).collect();
    let tools: Vec<&MarketplaceApp> = newest.iter().copied().filter(|a| !is_game(a)).collect();
    let d = n.device;
    let mut out = Vec::new();
    out.extend(spotlight(n, &n.spotlight(), on));
    let mut picks: Vec<(&'static str, &str, &str, &str, Vec<&MarketplaceApp>)> = Vec::new();
    if d.pad {
        picks.push(("pad", "Grab Your Controller", "gamepad", "controller", games.iter().copied().filter(|a| controls_of(a).pad).collect()));
    }
    if d.touch && !d.keys {
        picks.push(("touch", "Play on the Go", "touch", "mobile", newest.iter().copied().filter(|a| controls_of(a).touch).collect()));
    } else if d.keys {
        picks.push(("keys", "Made for Keyboard & Mouse", "keyboard", "keyboard", newest.iter().copied().filter(|a| controls_of(a).keys).collect()));
    }
    for (key, title, g, tag, apps) in picks.into_iter().filter(|p| !p.4.is_empty()) {
        let tiles = apps.iter().map(|a| tile(n, a, None, on, key)).collect();
        let h = n.shelves.get(key).cloned().unwrap_or_default();
        out.push(shelf(n.narrow, key, title, Some(g), Some(Act::Show(View::Category(tag.into()))), tiles, &h, on));
    }
    let recent: Vec<(&MarketplaceApp, i64)> = n
        .played
        .iter()
        .filter_map(|(id, at, _)| n.app(id).filter(|a| is_installed(a)).map(|a| (a, *at)))
        .take(12)
        .collect();
    if !recent.is_empty() {
        let tiles = recent.iter().map(|(a, at)| tile(n, a, Some(played_ago(*at, n.now_ms)), on, "recent")).collect();
        let h = n.shelves.get("recent").cloned().unwrap_or_default();
        out.push(shelf(n.narrow, "recent", "Jump Back In", None, Some(Act::Show(View::Library)), tiles, &h, on));
    }
    if !together.is_empty() {
        let tiles = together.iter().map(|a| tile(n, a, None, on, "together")).collect();
        let h = n.shelves.get("together").cloned().unwrap_or_default();
        out.push(shelf(n.narrow, "together", "Play Together", None, Some(Act::Show(View::Category("multiplayer".into()))), tiles, &h, on));
    }
    let arrivals: Vec<AnyElement> = newest.iter().take(6).enumerate().map(|(i, a)| row(n, a, i, None, on, "new")).collect();
    let gap = if n.narrow { 30. } else { 36. };
    out.push(
        div()
            .mt(px(gap))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .mb(px(12.))
                    .child(h3("New Arrivals"))
                    .child(div().flex_1())
                    .child(link("nx-new-all", "See All").on_click(click(on, || Act::Show(View::All)))),
            )
            .child(rows(arrivals, width))
            .into_any_element(),
    );
    if !tools.is_empty() {
        let tiles = tools.iter().map(|a| tile(n, a, None, on, "tools")).collect();
        let h = n.shelves.get("tools").cloned().unwrap_or_default();
        let more = (tools.len() > 6).then(|| Act::Show(View::Category("app".into())));
        out.push(shelf(n.narrow, "tools", "Apps & Tools", None, more, tiles, &h, on));
    }
    let cats: Vec<(String, usize)> = tag_counts(fit.iter().copied())
        .into_iter()
        .filter(|(t, c)| t != "multiplayer" && *c > 1)
        .take(8)
        .collect();
    if !cats.is_empty() {
        let narrow = n.narrow;
        let (cols, cat_gap, face) = if narrow { (2, 8., 28.) } else { ((((width + 10.) / (210. + 10.)).floor() as usize).max(1), 10., 34.) };
        let mut grid = div().flex().flex_col().gap(px(cat_gap));
        let mut it = cats.iter().peekable();
        while it.peek().is_some() {
            let mut line = div().flex().gap(px(cat_gap));
            for _ in 0..cols {
                let cell = it.next().map(|(tag, count)| {
                    let group = SharedString::from(format!("nx-cat-{tag}"));
                    let v = View::Category(tag.clone());
                    let mut faces = div().flex().flex_shrink_0();
                    for (k, a) in newest.iter().filter(|a| in_category(a, tag)).take(if narrow { 2 } else { 3 }).enumerate() {
                        let (dx, dy) = [(-4., 0.), (0., -3.), (4., 0.)][k];
                        faces = faces.child(
                            div()
                                .relative()
                                .when(k > 0, |d| d.ml(px(-10.)))
                                .rounded(px(face * 0.225 + 2.))
                                .border_2()
                                .border_color(c(RAISE))
                                .group_hover(group.clone(), move |s| s.left(px(dx)).top(px(dy)).border_color(c(RAISE_2)))
                                .child(icon(n, a, face)),
                        );
                    }
                    div()
                        .id(group.clone())
                        .group(group.clone())
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(12.))
                        .h(px(if narrow { 62. } else { 68. }))
                        .pl(px(if narrow { 12. } else { 16. }))
                        .pr(px(if narrow { 10. } else { 14. }))
                        .rounded(px(14.))
                        .bg(c(RAISE))
                        .border_1()
                        .border_color(c(LINE))
                        .cursor_pointer()
                        .hover(|s| s.bg(c(RAISE_2)))
                        .on_click(click(on, move || Act::Show(v.clone())))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(15.))
                                        .line_height(px(19.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(c(0xffffff))
                                        .child(tag_label(tag)),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .line_height(px(16.))
                                        .text_color(c(MUTE))
                                        .child(format!("{count} {}", if *count == 1 { "app" } else { "apps" })),
                                ),
                        )
                        .child(faces)
                });
                line = line.child(div().flex_1().min_w_0().children(cell));
            }
            grid = grid.child(line);
        }
        out.push(
            div()
                .mt(px(gap))
                .child(div().flex().items_center().mb(px(12.)).child(h3("Browse by Category")))
                .child(grid)
                .into_any_element(),
        );
    }
    out
}

fn empty(g: &str, title: String, hint: &str, button: Option<(&'static str, Act)>, on: &OnAct, pulse: bool) -> AnyElement {
    let mut col = div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .py(px(72.))
        .px(px(20.))
        .child({
            let g = glyph(g, 40., if pulse { c(PRIMARY) } else { c(0x3a3a3a) }).mb(px(10.));
            if pulse {
                g.with_animation(
                    "nx-pulse",
                    gpui::Animation::new(std::time::Duration::from_millis(1400)).repeat(),
                    |s, t| s.opacity(1. - 0.65 * (t * std::f32::consts::PI).sin()),
                )
                .into_any_element()
            } else {
                g.into_any_element()
            }
        })
        .child(
            div()
                .text_size(px(17.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(0xffffff))
                .child(title),
        );
    if !hint.is_empty() {
        col = col.child(
            div()
                .max_w(px(340.))
                .mb(px(12.))
                .text_center()
                .text_size(px(14.))
                .text_color(c(MUTE))
                .child(hint.to_string()),
        );
    }
    if let Some((label, act)) = button {
        let act = Rc::new(std::cell::RefCell::new(Some(act)));
        let on = on.clone();
        col = col.child(btn("nx-empty-btn", label, true).on_click(move |_, w, cx| {
            let a = act.borrow_mut().take();
            if let Some(a) = a {
                on(a, w, cx);
            }
        }));
    }
    col.into_any_element()
}

fn browse(n: &Nexus, q: &str, width: f32, on: &OnAct) -> Vec<AnyElement> {
    let yours = q.is_empty() && matches!(n.view, View::Library | View::Updates);
    let played: HashMap<&str, i64> = n.played.iter().map(|(id, at, _)| (id.as_str(), *at)).collect();
    let mut all: Vec<&MarketplaceApp> = if !q.is_empty() {
        let mut v: Vec<_> = n.apps.iter().filter(|a| matches(a, q)).collect();
        v.sort_by(|a, b| search_rank(a, q).cmp(&search_rank(b, q)).then(b.published_at.cmp(&a.published_at)));
        v
    } else {
        match &n.view {
            View::Library => {
                let mut v: Vec<_> = n.apps.iter().filter(|a| is_installed(a)).collect();
                v.sort_by(|a, b| {
                    played.get(b.id.as_str()).unwrap_or(&0).cmp(played.get(a.id.as_str()).unwrap_or(&0)).then(a.name.cmp(&b.name))
                });
                v
            }
            View::Updates => n.apps.iter().filter(|a| a.update_available).collect(),
            View::Category(cat) => newest_first(n.apps.iter().filter(|a| in_category(a, cat))),
            _ => newest_first(n.apps.iter()),
        }
    };
    let unfit = if yours { 0 } else { all.iter().filter(|a| !fits(a, n.device)).count() };
    if !yours {
        all.retain(|a| n.shows(a));
    }
    let updates: Vec<&MarketplaceApp> = n.apps.iter().filter(|a| a.update_available).collect();
    let updating = updates.iter().any(|a| n.actions.contains_key(&a.id));
    let mut out = Vec::new();
    if q.is_empty() && matches!(n.view, View::Library | View::Updates) && !updates.is_empty() {
        out.push(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .mb(px(14.))
                .pl(px(16.))
                .pr(px(10.))
                .py(px(10.))
                .rounded(px(14.))
                .bg(ca(51, 219, 152, 0.09))
                .border_1()
                .border_color(ca(51, 219, 152, 0.22))
                .child(glyph("arrow-up", 18., c(PRIMARY)))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(14.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(c(0xffffff))
                        .child(if updates.len() == 1 { "1 update ready".to_string() } else { format!("{} updates ready", updates.len()) }),
                )
                .child(
                    btn("nx-update-all", if updating { "Updating" } else { "Update All" }, true)
                        .h(px(30.))
                        .px(px(14.))
                        .text_size(px(13.))
                        .when(updating, |d| d.opacity(0.5))
                        .when(!updating, |d| d.on_click(click(on, || Act::UpdateAll))),
                )
                .into_any_element(),
        );
    }
    let g = if n.device.touch && !n.device.keys { "touch" } else { "keyboard" };
    if !all.is_empty() {
        let items = all
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let note = (matches!(n.view, View::Library) && q.is_empty()).then(|| match played.get(a.id.as_str()) {
                    Some(at) => format!("Played {}", played_ago(*at, n.now_ms)),
                    None => "Not played yet".into(),
                });
                row(n, a, i, note, on, "list")
            })
            .collect();
        out.push(rows(items, width));
    } else if !q.is_empty() && unfit == 0 {
        out.push(empty("search", format!("No results for \u{201c}{q}\u{201d}"), "Try a name, a maker, or a kind of game", None, on, false));
    } else if matches!(n.view, View::Library) && q.is_empty() {
        out.push(empty("grid", "Nothing installed yet".into(), "Apps you install from the App Store live here", Some(("Discover Apps", Act::Show(View::Discover))), on, false));
    } else if matches!(n.view, View::Updates) && q.is_empty() {
        out.push(empty("check-circle", "You're up to date".into(), "Every app you have is on its newest version", None, on, false));
    } else {
        out.push(empty(g, "Nothing here fits this device".into(), "These are made for other controls", None, on, false));
    }
    if unfit > 0 {
        let fit_only = n.fit_only;
        out.push(
            div()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .mt(px(22.))
                .text_size(px(13.))
                .text_color(c(MUTE))
                .child(glyph(g, 15., c(MUTE)))
                .child(if fit_only {
                    if unfit == 1 { "1 more is made for other controls".to_string() } else { format!("{unfit} more are made for other controls") }
                } else {
                    "Showing what may not fit this device".to_string()
                })
                .child(
                    link("nx-fit-link", if fit_only { "Show" } else { "Hide" })
                        .text_size(px(13.))
                        .on_click(click(on, move || Act::FitOnly(!fit_only))),
                )
                .into_any_element(),
        );
    }
    out
}

fn details(n: &Nexus, app: &MarketplaceApp, publisher: Option<&(SharedString, Option<PathBuf>)>, on: &OnAct) -> AnyElement {
    let action = n.actions.get(&app.id);
    let busy = action.is_some_and(|a| a.kind != Kind::Failed);
    let installed = is_installed(app);
    let npub_short = if app.publisher.len() > 20 {
        format!("{}…{}", &app.publisher[..12], &app.publisher[app.publisher.len() - 8..])
    } else {
        app.publisher.clone()
    };
    let maker = app
        .developer
        .clone()
        .or_else(|| publisher.map(|p| p.0.to_string()))
        .unwrap_or_else(|| "Unknown maker".into());
    let narrow = n.narrow;
    let fill = |d: gpui::Stateful<gpui::Div>| d.when(narrow, |d| d.flex_1().min_w_0());
    let mut actions = div()
        .flex()
        .flex_wrap()
        .gap(px(10.))
        .mt(px(if narrow { 18. } else { 16. }))
        .child(fill(action_button(n, app, true, on, "detail")));
    if is_multiplayer(app) {
        let id = app.id.clone();
        let b = fill(soft("nx-d-friends", Some("users-multi"), "Play with Friends", busy));
        actions = actions.child(if busy { b } else { b.on_click(click(on, move || Act::PlayWith(id.clone()))) });
    }
    if !n.me.is_empty() && app.publisher == n.me {
        let id = app.id.clone();
        actions = actions.child(
            fill(soft("nx-d-edit", Some("edit"), "Edit Listing", false))
                .when(narrow, |d| d.flex_basis(gpui::relative(1.)))
                .on_click(click(on, move || Act::EditListing(id.clone()))),
        );
    }
    let copy_id = app.id.clone();
    let id_block = div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(if narrow { 23. } else { 30. }))
                        .line_height(px(if narrow { 28. } else { 34. }))
                        .font_weight(FontWeight::BOLD)
                        .text_color(c(0xffffff))
                        .child(app.name.clone())
                        .child(
                            div()
                                .id("nx-copy-link")
                                .tooltip(crate::kit::tooltip("Copy Link"))
                                .cursor_pointer()
                                .opacity(0.)
                                .hover(|s| s.opacity(1.))
                                .on_click(click(on, move || Act::CopyLink(copy_id.clone())))
                                .child(glyph("share", 16., c(DIM))),
                        ),
                )
                .child(div().mt(px(4.)).text_size(px(14.)).text_color(c(PRIMARY)).child(maker));
    let head = if narrow {
        div()
            .pt(px(6.))
            .pb(px(20.))
            .child(div().flex().items_center().gap(px(16.)).child(icon(n, app, 88.)).child(id_block))
            .child(actions)
    } else {
        div()
            .flex()
            .items_center()
            .gap(px(24.))
            .pt(px(6.))
            .pb(px(24.))
            .child(icon(n, app, 112.))
            .child(id_block.child(actions))
    };
    let fact = |k: &str, v: String, s: Option<String>, first: bool| {
        div()
            .when(narrow, |d| d.w_1_2())
            .when(!narrow, |d| d.flex_1())
            .min_w_0()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(3.))
            .px(px(8.))
            .when(!first, |d| d.border_l_1().border_color(c(LINE_2)))
            .child(
                div()
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .text_size(px(11.))
                    .line_height(px(24.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(c(MUTE))
                    .child(crate::kit::spaced(k.to_uppercase(), 11. * 0.12)),
            )
            .child(
                div()
                    .max_w_full()
                    .truncate()
                    .text_size(px(16.))
                    .line_height(px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c(0xffffff))
                    .child(v),
            )
            .children(s.map(|s| div().text_size(px(11.)).text_color(c(PRIMARY)).child(s)))
    };
    let game = is_game(app);
    let facts = div()
        .flex()
        .when(narrow, |d| d.flex_wrap().gap_y(px(14.)))
        .py(px(14.))
        .border_t_1()
        .border_b_1()
        .border_color(c(LINE_2))
        .child(fact(
            "Version",
            app.version.clone(),
            app.update_available.then(|| app.installed_version.clone()).flatten().map(|v| format!("You have {v}")),
            true,
        ))
        .child(fact("Size", crate::media::format_bytes_with(app.size as f64, 1, false), None, false))
        .child(fact("Released", publish_date(app.published_at), None, narrow))
        .child(fact(
            if game { "Players" } else { "Kind" },
            if game { if is_multiplayer(app) { "Multiplayer" } else { "Solo" } } else { "App" }.into(),
            None,
            false,
        ));
    let ctl = controls_of(app);
    let d = n.device;
    let mut page = div().max_w(px(760.)).w_full().mx_auto().child(head).child(facts);
    if ctl.touch || ctl.pad || ctl.keys {
        let play = |g: &str, label: &'static str, have: bool| {
            div()
                .flex()
                .items_center()
                .gap(px(7.))
                .text_size(px(14.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if have { c(PRIMARY) } else { c(MUTE) })
                .child(glyph(g, 17., if have { c(PRIMARY) } else { c(MUTE) }))
                .child(label)
        };
        let mut plays = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(px(14.))
            .gap_y(px(8.))
            .py(px(14.))
            .border_b_1()
            .border_color(c(LINE_2))
            .child(
                div()
                    .mr(px(4.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .text_size(px(11.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(c(MUTE))
                    .child(crate::kit::spaced("PLAYS WITH", 11. * 0.12)),
            );
        if ctl.touch {
            plays = plays.child(play("touch", "Touch", d.touch));
        }
        if ctl.pad {
            plays = plays.child(play("gamepad", "Controller", d.pad));
        }
        if ctl.keys {
            plays = plays.child(play("keyboard", "Keyboard & Mouse", d.keys));
        }
        page = page.child(plays);
    }
    if !fits(app, d) {
        page = page.child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .mt(px(14.))
                .px(px(14.))
                .py(px(10.))
                .rounded(px(12.))
                .bg(ca(252, 165, 89, 0.1))
                .text_color(c(FAIR))
                .text_size(px(13.))
                .line_height(px(18.2))
                .child(glyph("info", 16., c(FAIR)))
                .child(format!("Made for {}, which this device doesn't seem to have", needs_label(app))),
        );
    }
    let tags: Vec<&String> = tags_of(app).collect();
    if !tags.is_empty() {
        let mut chips = div().flex().flex_wrap().gap(px(8.)).mt(px(20.));
        for t in tags {
            let v = View::Category(t.clone());
            chips = chips.child(chip(SharedString::from(format!("nx-tag-{t}")), tag_label(t)).on_click(click(on, move || Act::Show(v.clone()))));
        }
        page = page.child(chips);
    }
    let prose = |text: String| {
        div()
            .text_size(px(15.))
            .line_height(px(24.))
            .text_color(white_a(0.78))
            .whitespace_normal()
            .child(text)
    };
    if !app.description.is_empty() {
        page = page.child(div().mt(px(32.)).child(h3("About").mb(px(10.))).child(prose(app.description.clone())));
    }
    if let Some(log) = app.changelog.as_ref().filter(|l| !l.is_empty()) {
        page = page.child(
            div()
                .mt(px(32.))
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .justify_between()
                        .gap(px(12.))
                        .mb(px(10.))
                        .child(h3("What's New"))
                        .child(div().text_size(px(13.)).text_color(c(MUTE)).child(format!("Version {}", app.version))),
                )
                .child(prose(log.clone())),
        );
    }
    if installed && !app.requested_permissions.trim().is_empty() {
        page = page.child(permissions(n, app, on));
    }
    let pub_npub = app.publisher.clone();
    let avatar = match publisher.and_then(|p| p.1.clone()) {
        Some(p) => img(p).size_full().object_fit(ObjectFit::Cover).into_any_element(),
        None => glyph("user-circle", 22., c(0x6f6f6f)).into_any_element(),
    };
    page = page.child(
        div()
            .mt(px(32.))
            .child(h3("Publisher").mb(px(10.)))
            .child(
                div()
                    .id("nx-pub")
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .w_full()
                    .px(px(16.))
                    .py(px(12.))
                    .rounded(px(14.))
                    .bg(c(RAISE))
                    .border_1()
                    .border_color(c(LINE))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(RAISE_2)))
                    .on_click(click(on, move || Act::Publisher(pub_npub.clone())))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .flex_shrink_0()
                            .size(px(40.))
                            .rounded_full()
                            .overflow_hidden()
                            .bg(c(FILL))
                            .child(avatar),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(15.))
                                    .line_height(px(19.))
                                    .text_color(c(0xffffff))
                                    .child(publisher.map(|p| p.0.to_string()).unwrap_or_else(|| npub_short.clone())),
                            )
                            .when(publisher.is_some(), |d| {
                                d.child(div().text_size(px(12.)).line_height(px(16.)).text_color(c(MUTE)).child(npub_short.clone()))
                            }),
                    )
                    .child(glyph("chevron-right", 16., c(MUTE))),
            ),
    );
    let info_row = |k: &'static str, v: AnyElement, first: bool| {
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .min_h(px(52.))
            .px(px(16.))
            .py(px(10.))
            .when(!first, |d| d.border_t_1().border_color(c(LINE_2)))
            .child(div().flex_shrink_0().w(px(110.)).text_size(px(14.)).text_color(c(MUTE)).child(k))
            .child(v)
    };
    let value = |s: String| {
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .justify_end()
            .child(div().truncate().text_size(px(14.)).text_color(c(0xffffff)).child(s))
            .into_any_element()
    };
    let mut info = div()
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(14.))
        .bg(c(RAISE))
        .border_1()
        .border_color(c(LINE));
    let mut first = true;
    if let Some(dev) = &app.developer {
        info = info.child(info_row("Developer", value(dev.clone()), true));
        first = false;
    }
    info = info.child(info_row("Category", value(if game { "Game" } else { "App" }.into()), first));
    if installed && let Some(v) = &app.installed_version {
        info = info.child(info_row("Installed", value(format!("Version {v}")), false));
    }
    if let Some(url) = &app.source_url {
        let u = url.clone();
        info = info.child(info_row(
            "Source",
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .justify_end()
                .child(
                    div()
                        .id("nx-source")
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .min_w_0()
                        .text_size(px(14.))
                        .text_color(c(PRIMARY))
                        .cursor_pointer()
                        .on_click(click(on, move || Act::OpenUrl(u.clone())))
                        .child(div().truncate().child(source_url(url)))
                        .child(glyph("external", 12., c(PRIMARY))),
                )
                .into_any_element(),
            false,
        ));
    }
    info = info.child(info_row(
        "App ID",
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .justify_end()
            .child(div().truncate().font_family(MONO).text_size(px(12.)).text_color(c(0xffffff)).child(app.id.clone()))
            .into_any_element(),
        false,
    ));
    let mut sec = div().mt(px(32.)).child(h3("Information").mb(px(10.))).child(info);
    if installed {
        let (id, label) = (
            app.id.clone(),
            match action {
                Some(a) if a.kind == Kind::Uninstalling => a.label.to_string(),
                _ => format!("Uninstall {}", app.name),
            },
        );
        sec = sec.child(
            div()
                .id("nx-uninstall")
                .flex()
                .items_center()
                .justify_center()
                .w_full()
                .h(px(44.))
                .mt(px(14.))
                .rounded(px(12.))
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(DANGER))
                .when(busy, |d| d.opacity(0.5))
                .when(!busy, |d| {
                    d.cursor_pointer()
                        .hover(|s| s.bg(ca(255, 46, 169, 0.12)))
                        .on_click(click(on, move || Act::Uninstall(id.clone())))
                })
                .child(label),
        );
    }
    page.child(sec).into_any_element()
}

/// The whole Nexus, `width` wide beside the rail.
pub fn view(
    n: &Nexus,
    publisher: Option<&(SharedString, Option<PathBuf>)>,
    width: f32,
    on: &OnAct,
    cx: &App,
) -> AnyElement {
    let q = n.query(cx);
    let searching = !q.is_empty();
    let app = n.details.as_deref().and_then(|id| n.app(id));
    let title: String = match (app, searching, &n.view) {
        (Some(a), _, _) => a.name.clone(),
        (_, true, _) => "Search".into(),
        (_, _, View::Library) => "Library".into(),
        (_, _, View::Updates) => "Updates".into(),
        (_, _, View::All) => "All Apps".into(),
        (_, _, View::Category(c)) if c == "app" => "Apps & Tools".into(),
        (_, _, View::Category(c)) => tag_label(c),
        _ => "Discover".into(),
    };
    let count = |n: usize| format!("{n} {}", if n == 1 { "app" } else { "apps" });
    let fit: Vec<&MarketplaceApp> = n.apps.iter().filter(|a| n.shows(a)).collect();
    let heading: Option<(String, String)> = if searching {
        Some(("Results".into(), count(fit.iter().filter(|a| matches(a, &q)).count())))
    } else {
        match &n.view {
            View::Library => Some(("Library".into(), count(n.apps.iter().filter(|a| is_installed(a)).count()))),
            View::Updates => Some(("Updates".into(), String::new())),
            View::All => Some(("All Apps".into(), count(fit.len()))),
            View::Category(c) => Some((title.clone(), count(fit.iter().filter(|a| in_category(a, c)).count()))),
            View::Discover => None,
        }
    };
    let main_w = if n.slim { width } else { width - crate::ui::LIST_W };
    let pad = if n.narrow { 16. } else { 32. };
    let inner = main_w.min(PAGE_MAX) - pad * 2.;
    let mut page = div()
        .w_full()
        .max_w(px(PAGE_MAX))
        .mx_auto()
        .pt(px(if n.narrow { 16. } else { 24. }))
        .px(px(pad))
        .pb(px(if n.narrow { 56. } else { 72. }));
    if n.loading && n.apps.is_empty() {
        page = page.child(empty("nexus", "Opening the App Store".into(), "", None, on, true));
    } else if !n.error.is_empty() && n.apps.is_empty() {
        page = page.child(empty("info", "The App Store didn't load".into(), &n.error, Some(("Try Again", Act::Retry)), on, false));
    } else if let Some(app) = app {
        page = page.child(details(n, app, publisher, on));
    } else {
        if let Some((t, sub)) = heading {
            page = page.child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(12.))
                    .mt(px(2.))
                    .mb(px(18.))
                    .child(
                        div()
                            .text_size(px(if n.narrow { 24. } else { 28. }))
                            .line_height(px(if n.narrow { 28. } else { 32. }))
                            .font_weight(FontWeight::BOLD)
                            .text_color(c(0xffffff))
                            .child(t),
                    )
                    .when(!sub.is_empty(), |d| d.child(div().text_size(px(14.)).text_color(c(MUTE)).child(sub))),
            );
        }
        page = page.children(if matches!(n.view, View::Discover) && !searching {
            discover(n, inner, on)
        } else {
            browse(n, &q, inner, on)
        });
    }
    let page = page.with_animation(
        SharedString::from(format!("nx-page-{:?}", n.turned)),
        gpui::Animation::new(std::time::Duration::from_millis(if app.is_some() { 340 } else { 320 })),
        {
            let push = n.slim && app.is_some();
            move |d, t| {
                let e = crate::kit::bezier(0.22, 1., 0.36, 1.)(t);
                if push { d.opacity(e).ml(px(32. * (1. - e))) } else { d.opacity(e).mt(px(8. * (1. - e))) }
            }
        },
    );
    let back = app.is_some() || n.slim;
    let title = if n.slim { app.map(|a| a.name.clone()).unwrap_or_else(|| "App Store".into()) } else { title };
    let closes = app.is_none();
    let top = div()
        .h(px(if n.slim { 56. } else { 50. }))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(6.))
        .pl(px(if n.slim { 10. } else if back { 12. } else { 28. }))
        .pr(px(if n.slim { 10. } else { 14. }))
        .border_b_1()
        .border_color(c(if n.slim { 0x191919 } else { 0x171717 }))
        .when(back, |d| {
            d.child(
                div()
                    .id("nx-top-back")
                    .group("nx-top-back")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(36.))
                    .rounded_full()
                    .cursor_pointer()
                    .hover(|s| s.bg(c(0x151515)))
                    .on_click(click(on, move || if closes { Act::Close } else { Act::CloseDetails }))
                    .child(glyph("chevron-left", 20., c(0xb2b2b2)).group_hover("nx-top-back", |s| s.text_color(c(0xffffff)))),
            )
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(if n.slim { 17. } else { 16. }))
                .line_height(px(19.))
                .when(n.slim, |d| d.text_center().font_weight(FontWeight::BOLD))
                .text_color(c(0xffffff))
                .child(title),
        )
        .child(
            div()
                .id("nx-refresh")
                .group("nx-refresh")
                .flex()
                .items_center()
                .justify_center()
                .size(px(36.))
                .rounded_full()
                .when(!n.loading, |d| {
                    d.cursor_pointer().hover(|s| s.bg(c(0x151515))).on_click(click(on, || Act::Retry))
                })
                .child({
                    let g = glyph("refresh", 17., c(0xb2b2b2)).group_hover("nx-refresh", |s| s.text_color(c(0xffffff)));
                    if n.loading {
                        g.with_animation(
                            "nx-spin",
                            gpui::Animation::new(std::time::Duration::from_millis(900)).repeat(),
                            |s, t| s.with_transformation(gpui::Transformation::rotate(gpui::radians(t * std::f32::consts::TAU))),
                        )
                        .into_any_element()
                    } else {
                        g.into_any_element()
                    }
                }),
        );
    let main = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(top)
        .children((n.slim && app.is_none()).then(|| slim_head(n, &q, on)))
        .child(
            div()
                .id("nx-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&n.scroll)
                .child(page),
        );
    div()
        .size_full()
        .flex()
        .bg(c(BG))
        .font_family(RUBIK)
        .when(!n.slim, |d| d.child(nav(n, on, cx)))
        .child(main)
        .with_animation(
            SharedString::from(format!("nx-open-{:?}", n.opened)),
            gpui::Animation::new(std::time::Duration::from_millis(240)),
            |d, t| {
                let e = crate::kit::bezier(0.22, 1., 0.36, 1.)(t);
                d.opacity(e)
            },
        )
        .into_any_element()
}

/// `.nx-mhead`: on a phone, the search, then the pages and busiest categories as one strip.
fn slim_head(n: &Nexus, q: &str, on: &OnAct) -> AnyElement {
    let searching = !q.is_empty();
    let fit: Vec<&MarketplaceApp> = n.apps.iter().filter(|a| n.shows(a)).collect();
    let updates = n.apps.iter().filter(|a| a.update_available).count();
    let mut chips: Vec<(View, String, usize)> = vec![(View::Discover, "Discover".into(), 0), (View::Library, "Library".into(), 0)];
    if updates > 0 {
        chips.push((View::Updates, "Updates".into(), updates));
    }
    chips.push((View::All, "All".into(), 0));
    if n.apps.iter().any(|a| !is_game(a)) {
        chips.push((View::Category("app".into()), "Apps & Tools".into(), 0));
    }
    for (tag, _) in tag_counts(fit.iter().copied()).into_iter().filter(|(_, c)| *c > 1).take(10) {
        let label = tag_label(&tag);
        chips.push((View::Category(tag), label, 0));
    }
    let lit = |d: gpui::Stateful<gpui::Div>, on: bool| {
        if on { d.bg(c(PRIMARY).opacity(0.16)).text_color(c(PRIMARY)) } else { d }
    };
    let fit_only = n.fit_only;
    let fit_glyph = if n.device.touch && !n.device.keys { "touch" } else { "keyboard" };
    let mut strip = div()
        .id("nx-mchips")
        .flex()
        .gap(px(8.))
        .py(px(12.))
        .px(px(16.))
        .overflow_x_scroll()
        .track_scroll(&n.chips)
        .border_b_1()
        .border_color(c(0x191919))
        .child(
            lit(chip("nx-chip-fit", "Fits This Device".into()), fit_only)
                .gap(px(6.))
                .child(glyph(fit_glyph, 15., if fit_only { c(PRIMARY) } else { c(DIM) }))
                .on_click(click(on, move || Act::FitOnly(!fit_only))),
        )
        .child(div().flex_shrink_0().w(px(1.)).my(px(6.)).mx(px(2.)).bg(c(LINE_2)));
    for (i, (v, label, count)) in chips.into_iter().enumerate() {
        let here = !searching && n.view == v;
        let mut el = lit(chip(SharedString::from(format!("nx-mchip-{i}")), label), here).gap(px(6.));
        if count > 0 {
            el = el.child(
                div()
                    .h(px(18.))
                    .min_w(px(18.))
                    .px(px(5.))
                    .rounded(px(10.))
                    .bg(c(PRIMARY))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(11.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(c(0x000000))
                    .child(count.to_string()),
            );
        }
        strip = strip.child(el.on_click(click(on, move || Act::Show(v.clone()))));
    }
    let clear = on.clone();
    let search = div()
        .relative()
        .mt(px(12.))
        .mx(px(16.))
        .h(px(44.))
        .rounded(px(10.))
        .border_1()
        .border_color(c(0x2c2c2c))
        .bg(c(0x0e0e0e))
        .pl(px(41.))
        .pr(px(if searching { 43. } else { 13. }))
        .flex()
        .items_center()
        .child(div().flex_1().min_w_0().child(n.search.clone()))
        .child(glyph("search", 19., c(0x8a8a8a)).absolute().left(px(12.)).top(px(11.5)))
        .when(searching, |d| {
            d.child(
                div()
                    .id("nx-msearch-clear")
                    .absolute()
                    .right(px(9.))
                    .top(px(9.))
                    .size(px(24.))
                    .cursor_pointer()
                    .on_click(move |_, w, cx| clear(Act::ClearSearch, w, cx))
                    .child(svg().path("ui/search-clear.svg").size_full().text_color(c(0x8a8a8a))),
            )
        });
    div().flex_shrink_0().flex().flex_col().child(search).child(strip).into_any_element()
}

/// The details page's Permissions section: what the app may use, switchable any time.
fn permissions(n: &Nexus, app: &MarketplaceApp, on: &OnAct) -> gpui::Div {
    let hint = |t: String| div().text_size(px(13.)).line_height(px(18.85)).text_color(c(MUTE)).child(t);
    let row = || div().flex().items_center().gap(px(12.)).min_h(px(52.)).px(px(16.)).py(px(10.));
    let mut list = div()
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(14.))
        .bg(c(RAISE))
        .border_1()
        .border_color(c(LINE));
    let mut any = false;
    match n.perms.as_ref().filter(|p| p.0 == app.id).map(|p| &p.1) {
        None => list = list.child(row().child(hint("Loading permissions…".into()))),
        Some(Err(e)) => list = list.child(row().child(hint(e.clone()))),
        Some(Ok(items)) => {
            any = !items.is_empty();
            for (i, (id, label, sub, granted)) in items.iter().enumerate() {
                let (pid, on_now) = (id.clone(), *granted);
                let on = on.clone();
                list = list.child(
                    row()
                        .when(i > 0, |d| d.border_t_1().border_color(c(LINE_2)))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(div().truncate().text_size(px(15.)).line_height(px(19.)).text_color(c(0xffffff)).child(label.clone()))
                                .child(div().text_size(px(12.)).line_height(px(16.)).text_color(c(MUTE)).child(sub.clone())),
                        )
                        .child(crate::settings::toggle(
                            SharedString::from(format!("nx-perm-{id}")),
                            *granted,
                            false,
                            move |_, w, cx| on(Act::Perm(pid.clone(), !on_now), w, cx),
                        )),
                );
            }
        }
    }
    div()
        .mt(px(32.))
        .flex()
        .flex_col()
        .child(h3("Permissions").mb(px(10.)))
        .child(hint(format!("What {} may use. Turn any off at any time.", app.name)).mt(px(-4.)).mb(px(12.)))
        .child(list)
        .when(any, |d| {
            d.child(
                div()
                    .id("nx-perm-reset")
                    .mt(px(12.))
                    .self_start()
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(c(DANGER))
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.75))
                    .on_click(click(on, || Act::ResetPerms))
                    .child("Reset Permissions"),
            )
        })
}
