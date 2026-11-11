//! The emoji picker (`PickerPanel.svelte`): search, the mode toggle, the section rail, and the
//! sections (search results, recently used, every emoji) as one virtual list, so the ~1,900
//! cells cost only the rows on screen. Kaomoji mode is `KaomojiGrid.svelte`; GIF mode is
//! `GifGrid.svelte` over `gifs.js`.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, ListAlignment, ListOffset,
    ListState, ParentElement, Render, ScrollHandle, SharedString, Styled, Window, div, img, list, prelude::*, px, svg,
};

use crate::composer::{CancelInput, InputChanged, Look, SendText, TextInput};
use gpui::AnimationExt;

use crate::backend::gifs::Gif;
use crate::emoji::{self, Emoji, Kaomoji};
use crate::theme::*;

const COLS: usize = 6;
const CELL: f32 = 42.;
const GAP: f32 = 4.;
const KAOMOJI_RECENTS: usize = 18;
const GIF_PAGE: usize = 12;
const GIF_GAP: f32 = 6.;
const GIF_TRENDING_TTL: std::time::Duration = std::time::Duration::from_secs(5 * 60);
const GIF_SEARCHES: usize = 10;

#[derive(Clone, PartialEq)]
enum GifPhase {
    Idle,
    Loading,
    Empty(String),
    Ready,
}

enum Preview {
    Loading,
    Ready(Arc<gpui::RenderImage>),
    Failed,
}

/// The GIF grid: the listing shown, its paging, and what has been fetched for it.
struct Gifs {
    phase: GifPhase,
    items: Vec<Gif>,
    /// The search shown; empty for trending.
    query: String,
    offset: usize,
    more: bool,
    loading_more: bool,
    /// Bumped by each new listing, so a slower earlier answer lands nowhere.
    seq: u64,
    trending: Option<(std::time::Instant, Vec<Gif>)>,
    searches: std::collections::VecDeque<(String, Vec<Gif>)>,
    previews: HashMap<String, Preview>,
    thumbs: HashMap<String, Option<Arc<gpui::RenderImage>>>,
    scroll: ScrollHandle,
    debounce: Option<gpui::Task<()>>,
}

impl Default for Gifs {
    fn default() -> Self {
        Self {
            phase: GifPhase::Idle,
            items: Vec::new(),
            query: String::new(),
            offset: 0,
            more: false,
            loading_more: false,
            seq: 0,
            trending: None,
            searches: Default::default(),
            previews: HashMap::new(),
            thumbs: HashMap::new(),
            scroll: ScrollHandle::new(),
            debounce: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Section {
    Results,
    Recents,
    All,
}

impl Section {
    fn title(self) -> &'static str {
        match self {
            Section::Results => "Search Results",
            Section::Recents => "Recently Used",
            Section::All => "Emojis",
        }
    }

    fn icon(self) -> Option<&'static str> {
        match self {
            Section::Results => None,
            Section::Recents => Some("icons/clock.svg"),
            Section::All => Some("icons/smile-face.svg"),
        }
    }
}

enum Item {
    Header(Section),
    Row { cells: Vec<&'static Emoji>, last: bool },
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Emoji,
    Gif,
    Kaomoji,
}

/// A pick: the emoji, and whether Shift kept the panel open.
pub struct Picked(pub String, pub bool);
/// A kaomoji, typed as text; Shift keeps the panel open. Carries the new recents to keep.
pub struct PickedText(pub String, pub bool, pub Vec<String>);
/// A GIF, as the URL its message carries.
pub struct PickedGif(pub String);
pub struct Dismissed;

pub struct EmojiPicker {
    search: Entity<TextInput>,
    query: String,
    used: Rc<HashMap<String, f64>>,
    items: Rc<Vec<Item>>,
    starts: Vec<(Section, usize)>,
    list: ListState,
    collapsed: HashSet<Section>,
    pub mode: Mode,
    /// Reactions take one emoji: no kaomoji.
    pub reaction: bool,
    kaomoji_list: ScrollHandle,
    kaomoji_strip: ScrollHandle,
    /// The category the kaomoji list is scrolled to.
    kaomoji_at: usize,
    kaomoji_recents: Vec<String>,
    gifs: Gifs,
}

impl EventEmitter<Picked> for EmojiPicker {}
impl EventEmitter<PickedGif> for EmojiPicker {}
impl EventEmitter<PickedText> for EmojiPicker {}
impl EventEmitter<Dismissed> for EmojiPicker {}

impl Focusable for EmojiPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.read(cx).focus_handle(cx)
    }
}

impl EmojiPicker {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let look = Look {
            family: INTER,
            weight: FontWeight::MEDIUM,
            size: px(16.),
            line_height: px(19.),
            color: c(0xffffff),
            placeholder: c(0x757575),
            ..Default::default()
        };
        let search = cx.new(|cx| TextInput::new("Search Emojis...", cx).with_look(look).keep_on_submit().submit_empty());
        cx.subscribe(&search, |this, input, _: &InputChanged, cx| {
            let text = input.read(cx).text().to_string();
            if this.mode == Mode::Gif {
                this.gif_search_later(text, cx);
                return;
            }
            this.query = text.trim().to_string();
            this.rebuild();
            cx.notify();
        })
        .detach();
        cx.subscribe(&search, |_, _, _: &CancelInput, cx| cx.emit(Dismissed)).detach();
        cx.subscribe(&search, |this, _, _: &SendText, cx| this.pick_first(cx)).detach();
        let mut this = Self {
            search,
            query: String::new(),
            used: Rc::default(),
            items: Rc::default(),
            starts: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            collapsed: HashSet::new(),
            mode: Mode::Emoji,
            reaction: false,
            kaomoji_list: ScrollHandle::new(),
            kaomoji_strip: ScrollHandle::new(),
            kaomoji_at: 0,
            kaomoji_recents: Vec::new(),
            gifs: Gifs::default(),
        };
        this.rebuild();
        this
    }

    /// Opened afresh: an empty search, the top of the list, and the account's usage.
    pub fn reset(&mut self, reaction: bool, used: HashMap<String, f64>, window: &mut Window, cx: &mut Context<Self>) {
        self.reaction = reaction;
        self.set_mode(Mode::Emoji, cx);
        self.used = Rc::new(used);
        self.search.update(cx, |s, cx| s.clear(window, cx));
        self.query.clear();
        self.rebuild();
        window.focus(&self.search.read(cx).focus_handle(cx), cx);
        cx.notify();
    }

    pub fn set_usage(&mut self, used: HashMap<String, f64>, cx: &mut Context<Self>) {
        self.used = Rc::new(used);
        self.rebuild();
        cx.notify();
    }

    pub fn set_kaomoji_recents(&mut self, recents: Vec<String>, cx: &mut Context<Self>) {
        self.kaomoji_recents = recents.into_iter().filter(|f| emoji::kaomoji(f).is_some()).take(KAOMOJI_RECENTS).collect();
        cx.notify();
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.mode = mode;
        let placeholder = match mode {
            Mode::Emoji => "Search Emojis...",
            Mode::Gif => "Search GIFs...",
            Mode::Kaomoji => "Search Kaomoji...",
        };
        self.search.update(cx, |s, cx| s.set_placeholder(placeholder, cx));
        if mode == Mode::Gif {
            let text = self.search.read(cx).text().trim().to_string();
            if self.gifs.phase == GifPhase::Idle || text != self.gifs.query {
                self.gif_list(text, cx);
            }
        } else {
            // Emoji and kaomoji share one query: what's typed carries across.
            let text = self.search.read(cx).text().trim().to_string();
            if text != self.query {
                self.query = text;
                self.rebuild();
            }
        }
        cx.notify();
    }

    /// The open picker warms the trending page, so GIF mode shows it at once.
    pub fn prefetch_gifs(&mut self, cx: &mut Context<Self>) {
        if self.gifs.trending.as_ref().is_some_and(|(at, _)| at.elapsed() < GIF_TRENDING_TTL) {
            return;
        }
        let rx = crate::backend::run(crate::backend::gifs::listing(crate::backend::gifs::trending(0, GIF_PAGE)));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(gifs)) = rx.await else { return };
            if gifs.is_empty() {
                return;
            }
            this.update(cx, |this, _| this.gifs.trending = Some((std::time::Instant::now(), gifs))).ok();
        })
        .detach();
    }

    fn gif_search_later(&mut self, text: String, cx: &mut Context<Self>) {
        self.gifs.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
            this.update(cx, |this, cx| this.gif_list(text.trim().to_string(), cx)).ok();
        }));
    }

    /// The first page of trending (an empty query) or of a search, from cache when it has one.
    fn gif_list(&mut self, query: String, cx: &mut Context<Self>) {
        let g = &mut self.gifs;
        g.seq += 1;
        g.query = query.clone();
        g.offset = 0;
        g.more = true;
        g.loading_more = false;
        g.scroll.set_offset(gpui::point(px(0.), px(0.)));
        let key = query.to_lowercase();
        let cached = if query.is_empty() {
            g.trending.as_ref().filter(|(at, _)| at.elapsed() < GIF_TRENDING_TTL).map(|(_, v)| v.clone())
        } else {
            g.searches.iter().position(|(k, _)| *k == key).and_then(|i| g.searches.remove(i)).map(|(k, v)| {
                g.searches.push_back((k, v.clone()));
                v
            })
        };
        if let Some(items) = cached {
            g.more = items.len() >= GIF_PAGE;
            g.offset = items.len();
            g.items = items;
            g.phase = GifPhase::Ready;
            cx.notify();
            return;
        }
        g.phase = GifPhase::Loading;
        g.items.clear();
        let seq = g.seq;
        let q = if query.is_empty() { crate::backend::gifs::trending(0, GIF_PAGE) } else { crate::backend::gifs::search(&query, 0, GIF_PAGE) };
        let rx = crate::backend::run(crate::backend::gifs::listing(q));
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(String::new()));
            this.update(cx, |this, cx| {
                let g = &mut this.gifs;
                if g.seq != seq {
                    return;
                }
                match result {
                    Ok(items) if !items.is_empty() => {
                        if query.is_empty() {
                            g.trending = Some((std::time::Instant::now(), items.clone()));
                        } else {
                            if g.searches.len() >= GIF_SEARCHES {
                                g.searches.pop_front();
                            }
                            g.searches.push_back((key, items.clone()));
                        }
                        g.more = items.len() >= GIF_PAGE;
                        g.offset = items.len();
                        g.items = items;
                        g.phase = GifPhase::Ready;
                    }
                    Ok(_) => {
                        g.more = false;
                        g.phase = GifPhase::Empty(if query.is_empty() {
                            "No trending GIFs found".into()
                        } else {
                            let shown: String = query.chars().take(32).collect();
                            let ellipsis = if query.chars().count() > 32 { "..." } else { "" };
                            format!("No GIFs found for \"{shown}{ellipsis}\"")
                        });
                    }
                    Err(_) => {
                        g.more = false;
                        g.phase = GifPhase::Empty(if query.is_empty() { "Failed to load GIFs".into() } else { "Search failed".into() });
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn gif_more(&mut self, cx: &mut Context<Self>) {
        let g = &mut self.gifs;
        if g.loading_more || !g.more || g.phase != GifPhase::Ready {
            return;
        }
        g.loading_more = true;
        let (seq, offset) = (g.seq, g.offset);
        let q = if g.query.is_empty() { crate::backend::gifs::trending(offset, GIF_PAGE) } else { crate::backend::gifs::search(&g.query, offset, GIF_PAGE) };
        let rx = crate::backend::run(crate::backend::gifs::listing(q));
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(String::new()));
            this.update(cx, |this, cx| {
                let g = &mut this.gifs;
                if g.seq != seq {
                    return;
                }
                g.loading_more = false;
                match result {
                    Ok(items) if !items.is_empty() => {
                        g.more = items.len() >= GIF_PAGE;
                        g.offset += items.len();
                        g.items.extend(items);
                    }
                    _ => g.more = false,
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn gif_preview(&mut self, id: &str, side: u32, cx: &mut Context<Self>) {
        if self.gifs.previews.contains_key(id) {
            return;
        }
        self.gifs.previews.insert(id.to_string(), Preview::Loading);
        let rx = crate::backend::run(crate::backend::gifs::preview(id.to_string()));
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let bytes = rx.await.ok().flatten();
            let image = match bytes {
                Some(b) => cx.background_executor().spawn(async move { crate::media::decode_gif_cover(&b, side) }).await,
                None => None,
            };
            this.update(cx, |this, cx| {
                this.gifs.previews.insert(id, image.map_or(Preview::Failed, |i| Preview::Ready(Arc::new(i))));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn gif_body(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        // Two columns across the panel's 400px, less its 8px sides, 4px scrollbar room and the gap.
        let side = (400. - 16. - 4. - GIF_GAP) / 2.;
        let device = (side * window.scale_factor()).round() as u32;
        let pitch = side + GIF_GAP;
        let skeleton = |i: usize| {
            div().id(SharedString::from(format!("gif-skel-{i}"))).h(px(side)).rounded(px(8.)).bg(c(0x1a1a1a)).with_animation(
                SharedString::from(format!("gif-skel-a-{i}")),
                gpui::Animation::new(std::time::Duration::from_millis(1500)).repeat(),
                |d, t| {
                    let wave = (1. - (t * 2. - 1.0f32).abs()).max(0.);
                    let v = 0x1a as f32 + (0x25 - 0x1a) as f32 * wave;
                    d.bg(gpui::rgb(((v as u32) << 16) | ((v as u32) << 8) | v as u32))
                },
            )
        };
        let grid = |cells: Vec<AnyElement>| {
            let mut rows = div().flex().flex_col().gap(px(GIF_GAP));
            let mut it = cells.into_iter().peekable();
            while it.peek().is_some() {
                let a = it.next().unwrap();
                let b = it.next().unwrap_or_else(|| div().into_any_element());
                rows = rows.child(div().flex().gap(px(GIF_GAP)).child(div().flex_1().min_w_0().child(a)).child(div().flex_1().min_w_0().child(b)));
            }
            rows
        };
        let content: AnyElement = match self.gifs.phase.clone() {
            GifPhase::Idle | GifPhase::Loading => grid((0..GIF_PAGE).map(|i| skeleton(i).into_any_element()).collect()).into_any_element(),
            GifPhase::Empty(message) => div()
                .min_h(px(150.))
                .p(px(20.))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .text_size(px(13.))
                .text_color(white_a(0.4))
                .child(svg().path("icons/image.svg").size(px(32.)).text_color(white_a(0.2)))
                .child(message)
                .into_any_element(),
            GifPhase::Ready => {
                let view = self.gifs.scroll.bounds().size.height.max(px(280.));
                let top = -self.gifs.scroll.offset().y;
                let reach = ((f32::from(top + view) + 200.) / pitch).ceil() as usize * 2;
                let wanted: Vec<String> = self.gifs.items.iter().take(reach).map(|g| g.id.clone()).collect();
                for id in wanted {
                    self.gif_preview(&id, device, cx);
                }
                let mut cells = Vec::with_capacity(self.gifs.items.len() + GIF_PAGE);
                for gif in self.gifs.items.clone() {
                    let thumb = match &gif.thumbhash {
                        Some(h) => self.gifs.thumbs.entry(gif.id.clone()).or_insert_with(|| crate::media::decode_thumbhash(h, (device, device)).map(Arc::new)).clone(),
                        None => None,
                    };
                    let inner: AnyElement = match self.gifs.previews.get(&gif.id) {
                        Some(Preview::Ready(image)) => img(image.clone()).absolute().inset_0().size_full().object_fit(gpui::ObjectFit::Cover).into_any_element(),
                        Some(Preview::Failed) => div().size_full().flex().items_center().justify_center().child(svg().path("icons/image.svg").size(px(24.)).text_color(white_a(0.3))).into_any_element(),
                        _ => div()
                            .size_full()
                            .relative()
                            .flex()
                            .items_center()
                            .justify_center()
                            .children(thumb.map(|t| img(t).absolute().inset_0().size_full().object_fit(gpui::ObjectFit::Cover)))
                            .child(crate::attachments::spinner(24., 2., 0.8, 0.25, white_a(0.1), c(PRIMARY)))
                            .into_any_element(),
                    };
                    let id = gif.id.clone();
                    cells.push(
                        div()
                            .id(SharedString::from(format!("gif-{}", gif.id)))
                            .relative()
                            .w_full()
                            .h(px(side))
                            .rounded(px(8.))
                            .overflow_hidden()
                            .bg(c(0x1a1a1a))
                            .border_2()
                            .border_color(gpui::transparent_black())
                            .hover(|s| s.border_color(c(PRIMARY)))
                            .active(|s| s.border_color(c(PRIMARY)).opacity(0.8))
                            .cursor_pointer()
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(PickedGif(crate::backend::gifs::original_url(&id)))))
                            .child(inner)
                            .into_any_element(),
                    );
                }
                if self.gifs.loading_more {
                    cells.extend((0..GIF_PAGE).map(|i| skeleton(100 + i).into_any_element()));
                }
                // Within 100px of the end fetches the next page.
                let max = self.gifs.scroll.max_offset().y;
                if max > px(0.) && max - top < px(100.) {
                    let me = cx.entity().downgrade();
                    cx.defer(move |cx| {
                        me.update(cx, |this, cx| this.gif_more(cx)).ok();
                    });
                }
                grid(cells).into_any_element()
            }
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .px(px(8.))
            .pb(px(8.))
            .bg(c(0x0a0a0a))
            .child(div().id("gif-grid").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.gifs.scroll).pr(px(4.)).child(content))
            .into_any_element()
    }

    fn pick_kaomoji(&mut self, face: &'static str, shift: bool, cx: &mut Context<Self>) {
        self.kaomoji_recents.retain(|f| f != face);
        self.kaomoji_recents.insert(0, face.to_string());
        self.kaomoji_recents.truncate(KAOMOJI_RECENTS);
        cx.emit(PickedText(face.to_string(), shift, self.kaomoji_recents.clone()));
        cx.notify();
    }

    /// Enter in the search: the first result, or the first recent.
    fn pick_first(&mut self, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Emoji => {
                let first = self.items.iter().find_map(|i| match i {
                    Item::Row { cells, .. } => cells.first().map(|e| e.emoji),
                    Item::Header(_) => None,
                });
                if let Some(e) = first {
                    cx.emit(Picked(e.to_string(), false));
                }
            }
            Mode::Kaomoji => {
                let first = if self.query.is_empty() {
                    self.kaomoji_recents.first().and_then(|f| emoji::kaomoji(f)).or_else(|| emoji::KAOMOJI.first().and_then(|c| c.items.first()))
                } else {
                    emoji::search_kaomoji(&self.query).into_iter().next()
                };
                if let Some(k) = first {
                    self.pick_kaomoji(k.face, false, cx);
                }
            }
            Mode::Gif => {
                if let Some(g) = self.gifs.items.first().filter(|_| self.gifs.phase == GifPhase::Ready) {
                    cx.emit(PickedGif(crate::backend::gifs::original_url(&g.id)));
                }
            }
        }
    }

    fn kaomoji_item(&self, k: &'static Kaomoji, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(SharedString::from(format!("kao-{}", k.face)))
            .px(px(10.))
            .py(px(6.))
            .rounded(px(8.))
            .max_w_full()
            .overflow_hidden()
            .bg(white_a(0.05))
            .hover(|s| s.bg(white_a(0.12)))
            .active(|s| s.bg(white_a(0.2)))
            .cursor_pointer()
            .text_color(white_a(0.9))
            .whitespace_nowrap()
            .text_ellipsis()
            .tooltip(crate::kit::tooltip(k.name))
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| this.pick_kaomoji(k.face, ev.modifiers().shift, cx)))
            .child(k.face)
            .into_any_element()
    }

    fn kaomoji_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let items = |this: &Self, faces: Vec<&'static Kaomoji>, cx: &mut Context<Self>| {
            div().flex().flex_wrap().gap(px(6.)).children(faces.into_iter().map(|k| this.kaomoji_item(k, cx)))
        };
        let header = |icon: Option<&'static str>, title: &'static str| {
            div()
                .pt(px(8.))
                .pb(px(4.))
                .flex()
                .items_center()
                .font_family(INTER)
                .text_size(px(14.))
                .line_height(px(24.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(c(0xffffff))
                .children(icon.map(|i| svg().path(i).size(px(18.)).mr(px(8.)).text_color(c(0xffffff))))
                .child(title)
        };
        let mut list = div()
            .id("kaomoji-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.kaomoji_list)
            .px(px(8.))
            .pb(px(8.))
            .font_family(".SystemUIFont")
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(14.))
            .line_height(px(14. * 1.3));
        let recents: Vec<_> = self.kaomoji_recents.iter().filter_map(|f| emoji::kaomoji(f)).collect();
        let mut strip = None;
        if !self.query.is_empty() {
            let hits = emoji::search_kaomoji(&self.query);
            list = list.child(if hits.is_empty() {
                div()
                    .py(px(24.))
                    .px(px(8.))
                    .flex()
                    .justify_center()
                    .font_family(INTER)
                    .text_size(px(13.))
                    .text_color(white_a(0.5))
                    .child(format!("No kaomoji for “{}”", self.query))
            } else {
                items(self, hits, cx)
            });
        } else {
            let shift = usize::from(!recents.is_empty());
            if !recents.is_empty() {
                list = list.child(div().child(header(Some("icons/clock.svg"), "Recently Used")).child(items(self, recents, cx)));
            }
            for cat in emoji::KAOMOJI {
                list = list.child(div().child(header(None, cat.name)).child(items(self, cat.items.iter().collect(), cx)));
            }
            // The strip follows the list: the category under the top edge lights up.
            let at = self.kaomoji_list.top_item().saturating_sub(shift);
            if at != self.kaomoji_at {
                self.kaomoji_at = at;
                self.kaomoji_strip.scroll_to_item(at);
            }
            let pills = emoji::KAOMOJI.iter().enumerate().map(|(i, cat)| {
                let on = i == self.kaomoji_at;
                div()
                    .id(SharedString::from(format!("kao-cat-{i}")))
                    .flex_shrink_0()
                    .px(px(9.))
                    .py(px(3.))
                    .rounded_full()
                    .cursor_pointer()
                    .font_family(INTER)
                    .text_size(px(11.))
                    .line_height(px(14.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .bg(white_a(if on { 0.16 } else { 0.06 }))
                    .text_color(if on { c(0xffffff) } else { white_a(0.65) })
                    .hover(|s| s.bg(white_a(0.12)).text_color(c(0xffffff)))
                    .active(|s| s.bg(white_a(0.2)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.kaomoji_list.scroll_to_top_of_item(i + shift);
                        cx.notify();
                    }))
                    .child(cat.name)
            });
            let max = self.kaomoji_strip.max_offset().x;
            let x = -self.kaomoji_strip.offset().x;
            let fade = |left: bool| {
                let (from, to) = if left { (1., 0.) } else { (0., 1.) };
                div().absolute().top_0().bottom(px(6.)).w(px(48.)).when(left, |d| d.left_0()).when(!left, |d| d.right_0()).bg(gpui::linear_gradient(
                    90.,
                    gpui::linear_color_stop(ca(10, 10, 10, from), 0.),
                    gpui::linear_color_stop(ca(10, 10, 10, to), 1.),
                ))
            };
            strip = Some(
                div()
                    .relative()
                    .flex_shrink_0()
                    .child(
                        div()
                            .id("kaomoji-cats")
                            .flex()
                            .gap(px(4.))
                            .px(px(8.))
                            .pb(px(6.))
                            .overflow_x_scroll()
                            .track_scroll(&self.kaomoji_strip)
                            .children(pills),
                    )
                    .when(x > px(1.), |d| d.child(fade(true)))
                    .when(x < max - px(1.), |d| d.child(fade(false))),
            );
        }
        div().flex_1().min_h_0().flex().flex_col().bg(c(0x0a0a0a)).children(strip).child(list).into_any_element()
    }

    /// A pick counts toward the recents before the next open reloads them.
    pub fn bump(&mut self, emoji: &str) {
        let used = Rc::make_mut(&mut self.used);
        let v = used.entry(emoji.to_string()).or_insert(0.);
        *v = (*v + 1.).min(25.);
    }

    fn rebuild(&mut self) {
        let used = self.used.clone();
        let score = move |e: &str| used.get(e).copied().unwrap_or(0.);
        let mut items = Vec::new();
        let mut starts = Vec::new();
        let mut section = |items: &mut Vec<Item>, s: Section, cells: Vec<&'static Emoji>, collapsed: bool| {
            starts.push((s, items.len()));
            items.push(Item::Header(s));
            if collapsed {
                return;
            }
            let rows: Vec<_> = cells.chunks(COLS).map(|c| c.to_vec()).collect();
            let n = rows.len();
            for (i, cells) in rows.into_iter().enumerate() {
                items.push(Item::Row { cells, last: i + 1 == n });
            }
        };
        if !self.query.is_empty() {
            section(&mut items, Section::Results, emoji::search(&self.query, &score), false);
        } else {
            section(&mut items, Section::Recents, emoji::most_used(&score), self.collapsed.contains(&Section::Recents));
            section(&mut items, Section::All, emoji::EMOJIS.iter().collect(), self.collapsed.contains(&Section::All));
        }
        self.starts = starts;
        self.list = ListState::new(items.len(), ListAlignment::Top, px(400.));
        self.items = Rc::new(items);
    }

    fn active(&self) -> Option<Section> {
        let top = self.list.logical_scroll_top().item_ix;
        self.starts.iter().rev().find(|(_, start)| *start <= top).map(|(s, _)| *s)
    }

    fn jump(&mut self, s: Section, cx: &mut Context<Self>) {
        if let Some((_, ix)) = self.starts.iter().find(|(x, _)| *x == s) {
            self.list.scroll_to(ListOffset { item_ix: *ix, offset_in_item: px(0.) });
            cx.notify();
        }
    }

    fn toggle(&mut self, s: Section, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&s) {
            self.collapsed.insert(s);
        }
        let top = self.list.logical_scroll_top();
        self.rebuild();
        self.list.scroll_to(top);
        cx.notify();
    }

    fn header(&self, s: Section, cx: &mut Context<Self>) -> AnyElement {
        let collapsed = self.collapsed.contains(&s);
        div()
            .id(SharedString::from(format!("picker-h-{}", s.title())))
            .h(px(32. + if collapsed { 3. } else { 6. }))
            .pb(px(if collapsed { 3. } else { 6. }))
            .flex()
            .items_center()
            .cursor_pointer()
            .font_family(INTER)
            .text_size(px(14.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(c(0xffffff))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(s, cx)))
            .children(s.icon().map(|i| svg().path(i).size(px(18.)).mr(px(8.)).text_color(c(0xffffff))))
            .child(s.title())
            .child(
                svg()
                    .path("icons/chevron-down.svg")
                    .size(px(18.))
                    .ml(px(2.))
                    .text_color(white_a(0.5))
                    .when(collapsed, |i| i.with_transformation(gpui::Transformation::rotate(gpui::radians(-std::f32::consts::FRAC_PI_2)))),
            )
            .into_any_element()
    }

    fn row(cells: &[&'static Emoji], last: bool, picker: gpui::WeakEntity<Self>) -> AnyElement {
        let mut row = div().w_full().flex().gap(px(GAP)).pb(px(if last { 3. } else { GAP }));
        for e in cells {
            let group: SharedString = format!("pick-{}", e.emoji).into();
            let glyph: AnyElement = match emoji::twemoji(e.emoji) {
                Some(src) => img(src).size(px(30.)).group_hover(group.clone(), |s| s.size(px(33.75))).into_any_element(),
                None => div().text_size(px(24.)).child(e.emoji).into_any_element(),
            };
            let picker = picker.clone();
            let emoji = e.emoji;
            row = row.child(
                div().flex_1().min_w_0().h(px(CELL)).child(
                    div()
                        .id(group.clone())
                        .group(group)
                        .size(px(CELL))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|s| s.bg(white_a(0.1)))
                        .on_click(move |ev, _, cx| {
                            let shift = ev.modifiers().shift;
                            picker.update(cx, |_, cx| cx.emit(Picked(emoji.to_string(), shift))).ok();
                        })
                        .child(glyph),
                ),
            );
        }
        for _ in cells.len()..COLS {
            row = row.child(div().flex_1());
        }
        row.into_any_element()
    }
}

impl Render for EmojiPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode_btn = |label: &'static str, mode: Mode, cx: &mut Context<Self>| {
            let active = self.mode == mode;
            div()
                .id(label)
                .h(px(27.))
                .px(px(12.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .cursor_pointer()
                .font_family(INTER)
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .when(active, |d| d.bg(c(PRIMARY)).text_color(c(0x000000)))
                .when(!active, |d| d.text_color(white_a(0.5)).hover(|s| s.text_color(white_a(0.8))))
                .on_click(cx.listener(move |this, _, _, cx| this.set_mode(mode, cx)))
                .child(label)
        };
        let search = div()
            .p(px(10.))
            .bg(c(0x0a0a0a))
            .rounded_tr(px(12.))
            .flex()
            .items_center()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .h(px(40.))
                    .rounded(px(8.))
                    .bg(c(0x202020))
                    .pl(px(44.))
                    .pr(px(12.))
                    .flex()
                    .items_center()
                    .child(self.search.clone())
                    .child(svg().path("icons/search.svg").absolute().left(px(12.)).top(px(8.)).size(px(24.)).text_color(white_a(0.4))),
            )
            .child(
                div()
                    .ml(px(16.))
                    .flex()
                    .gap(px(4.))
                    .child(mode_btn("Emoji", Mode::Emoji, cx))
                    .child(mode_btn("GIF", Mode::Gif, cx))
                    .when(!self.reaction, |d| d.child(mode_btn("(◕‿◕)", Mode::Kaomoji, cx))),
            );

        let active = if self.query.is_empty() { self.active() } else { None };
        let rail_btn = |icon: &'static str, s: Option<Section>, cx: &mut Context<Self>| {
            let on = s.is_some() && active == s;
            div()
                .id(icon)
                .relative()
                .size(px(30.))
                .my(px(3.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|d| d.opacity(0.7))
                .when(s.is_none(), |d| d.border_1().border_dashed().border_color(white_a(0.18)))
                .when(on, |d| d.child(div().absolute().left(px(-12.)).top(px(1.)).w(px(6.)).h(px(28.)).rounded(px(2.)).bg(c(PRIMARY))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(s) = s {
                        this.jump(s, cx);
                    }
                }))
                .child(svg().path(icon).size(px(26.)).text_color(if s.is_some() { c(0xffffff) } else { white_a(0.75) }))
        };
        let rail = div()
            .w(px(50.))
            .h_full()
            .flex_shrink_0()
            .pt(px(8.))
            .flex()
            .flex_col()
            .items_center()
            .bg(c(0x202020))
            .border_t(px(4.))
            .border_color(c(PRIMARY))
            .child(rail_btn("icons/clock.svg", Some(Section::Recents), cx))
            .child(rail_btn("icons/smile-face.svg", Some(Section::All), cx))
            .child(rail_btn("icons/plus-circle.svg", None, cx));

        let items = self.items.clone();
        let me = cx.entity().downgrade();
        let header_of = cx.entity().downgrade();
        let grid = list(self.list.clone(), move |i, _, cx| match &items[i] {
            Item::Header(s) => {
                let s = *s;
                header_of.update(cx, |this, cx| this.header(s, cx)).unwrap_or_else(|_| div().into_any_element())
            }
            Item::Row { cells, last } => Self::row(cells, *last, me.clone()),
        })
        .size_full();

        let body = match self.mode {
            Mode::Emoji => div()
                .flex_1()
                .min_h_0()
                .flex()
                .bg(c(0x0a0a0a))
                .child(rail)
                .child(div().flex_1().h(px(280.)).pt(px(8.)).pl(px(8.)).pr(px(5.)).child(grid))
                .into_any_element(),
            Mode::Kaomoji => self.kaomoji_body(cx),
            Mode::Gif => self.gif_body(window, cx),
        };

        div()
            .id("emoji-picker")
            .occlude()
            .w(px(402.))
            .h(px(352.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(gpui::rgb(0x0e0e0e))
            .border_1()
            .border_color(c(0x494949))
            .rounded(px(12.))
            .shadow(vec![gpui::BoxShadow {
                color: ca(0, 0, 0, 0.5),
                offset: gpui::point(px(0.), px(-8.)),
                blur_radius: px(32.),
                spread_radius: px(0.),
                inset: false,
            }])
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(Dismissed)))
            .child(search)
            .child(body)
    }
}
