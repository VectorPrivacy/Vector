//! Lyrics under an audio card (`Lyrics.svelte`): timed lines keep the sung one bright near
//! the top of the view and fill word by word; a click on a line plays from there. Plain
//! lyrics are text to read.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, InteractiveElement, IntoElement, ParentElement, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Window, div, point, prelude::*, px,
};
use vector_media::lyrics::Lyrics;

use crate::theme::*;

const HEIGHT: f32 = 260.;
const SLIDE: f32 = 0.4;
const GLIDE_MS: f32 = 650.;
const HOLD: Duration = Duration::from_secs(3);

pub struct Sheet {
    pub open: bool,
    since: Instant,
    pub scroll: ScrollHandle,
    glide: Option<(Instant, f32, f32)>,
    held_until: Option<Instant>,
    current: Option<isize>,
    placed: bool,
}

impl Default for Sheet {
    fn default() -> Self {
        Sheet {
            open: false,
            since: Instant::now() - Duration::from_secs(1),
            scroll: ScrollHandle::new(),
            glide: None,
            held_until: None,
            current: None,
            placed: false,
        }
    }
}

/// The sung line: the last one whose stamp has passed.
pub fn current(l: &Lyrics, pos: u64) -> isize {
    if !l.synced {
        return -1;
    }
    let mut idx = -1;
    for (i, line) in l.lines.iter().enumerate() {
        if line.at_ms.unwrap_or(0) as u64 <= pos {
            idx = i as isize;
        } else {
            break;
        }
    }
    idx
}

fn ease_out(x: f32) -> f32 {
    1. - (1. - x).powi(4)
}

fn ease_in_out(x: f32) -> f32 {
    if x < 0.5 { 4. * x * x * x } else { 1. - (-2. * x + 2.).powi(3) / 2. }
}

impl Sheet {
    pub fn toggle(&mut self) {
        self.open = !self.open;
        let t = self.reveal();
        let back = SLIDE * (1. - t);
        self.since = Instant::now() - Duration::from_secs_f32(back.max(0.));
        if self.open {
            self.placed = false;
        }
    }

    /// How far open the sheet is, 0 to 1.
    pub fn reveal(&self) -> f32 {
        let p = ease_out((self.since.elapsed().as_secs_f32() / SLIDE).min(1.));
        if self.open { p } else { 1. - p }
    }

    pub fn hold(&mut self) {
        self.held_until = Some(Instant::now() + HOLD);
        self.glide = None;
    }

    /// A clicked line: the view follows it at once, even out of a hold.
    pub fn release(&mut self) {
        self.held_until = None;
    }

    /// One frame: the slide, the glide, and following the sung line. True while moving.
    pub fn tick(&mut self, lyrics: &Lyrics, pos: u64) -> bool {
        let sliding = self.since.elapsed().as_secs_f32() < SLIDE;
        if !self.open {
            return sliding;
        }
        let cur = current(lyrics, pos);
        let held = self.held_until.is_some_and(|t| Instant::now() < t);
        if lyrics.synced && !held && (Some(cur) != self.current || !self.placed) {
            let h = f32::from(self.scroll.bounds().size.height);
            let top = if cur < 0 {
                Some(0.)
            } else {
                self.scroll.bounds_for_item(cur as usize).map(|b| {
                    f32::from(b.origin.y - self.scroll.bounds().origin.y - self.scroll.offset().y)
                        - h * 0.3
                })
            };
            if let Some(top) = top.filter(|_| h > 0.) {
                let max = f32::from(self.scroll.max_offset().y);
                let to = top.clamp(0., max.max(0.));
                if self.placed {
                    let from = -f32::from(self.scroll.offset().y);
                    self.glide = Some((Instant::now(), from, to));
                } else {
                    self.scroll.set_offset(point(px(0.), px(-to)));
                }
                self.placed = true;
                self.current = Some(cur);
            }
        }
        if let Some((t, from, to)) = self.glide {
            let x = (t.elapsed().as_secs_f32() * 1000. / GLIDE_MS).min(1.);
            let y = from + (to - from) * ease_in_out(x);
            self.scroll.set_offset(point(px(0.), px(-y)));
            if x >= 1. {
                self.glide = None;
            }
        }
        sliding || self.glide.is_some() || !self.placed
    }
}

pub type OnLine = std::rc::Rc<dyn Fn(Option<u32>, &mut Window, &mut App)>;

/// The sheet under the card; `on(Some(ms))` plays from a line, `on(None)` holds the view.
pub fn view(id: &str, l: &Lyrics, sheet: &Sheet, pos: u64, playing: bool, on: OnLine) -> Option<AnyElement> {
    let reveal = sheet.reveal();
    if reveal <= 0. {
        return None;
    }
    let cur = current(l, pos);
    let scroll = &sheet.scroll;
    let view_top = f32::from(scroll.bounds().origin.y);
    let view_h = f32::from(scroll.bounds().size.height).max(1.);
    let offset = -f32::from(scroll.offset().y);
    let max = f32::from(scroll.max_offset().y);
    let (fade_top, fade_bottom) = (
        if offset > 1. { 0.18 } else { 0. },
        if offset < max - 1. { 0.20 } else { 0. },
    );
    // The stylesheet's mask, taken per line at its middle.
    let fade = |i: usize| -> f32 {
        let Some(b) = scroll.bounds_for_item(i) else { return 1. };
        let y = (f32::from(b.origin.y + b.size.height / 2.) - view_top) / view_h;
        let mut a: f32 = 1.;
        if fade_top > 0. && y < fade_top {
            a = a.min((y / fade_top).max(0.));
        }
        if fade_bottom > 0. && y > 1. - fade_bottom {
            a = a.min(((1. - y) / fade_bottom).max(0.));
        }
        a
    };
    let lines = l.lines.iter().enumerate().map(|(i, line)| {
        let i_s = i as isize;
        if !l.synced {
            return div()
                .px(px(2.))
                .text_size(px(14.25))
                .line_height(px(22.1))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(white_a(0.82))
                .when(line.text.is_empty(), |d| d.h(px(11.4)))
                .child(line.text.clone())
                .into_any_element();
        }
        let base = if i_s == cur {
            1.
        } else if line.text.is_empty() {
            0.4
        } else if i_s < cur {
            0.3
        } else {
            0.75
        };
        let alpha = base * fade(i);
        let lit = |fill: f32| gpui::white().opacity(0.4 + 0.6 * fill.clamp(0., 1.));
        let body: AnyElement = if line.text.is_empty() {
            let pulse = if i_s == cur {
                let t = (sheet.since.elapsed().as_secs_f32() / 1.4).fract();
                Some(t)
            } else {
                None
            };
            div()
                .h(px(25.65))
                .pt(px(10.8))
                .flex()
                .items_start()
                .gap(px(7.))
                .children((0..3).map(|k| {
                    let (scale, o) = match pulse {
                        Some(t) => {
                            let p = ((t - k as f32 * 0.2 / 1.4).rem_euclid(1.) * std::f32::consts::TAU).cos();
                            let x = (1. - p) / 2.;
                            (0.75 + 0.35 * x, 0.5 + 0.5 * x)
                        }
                        None => (1., 1.),
                    };
                    div().size(px(9. * scale)).m(px(4.5 * (1. - scale))).rounded_full().bg(gpui::white().opacity(o))
                }))
                .into_any_element()
        } else if line.words.is_empty() {
            div().text_color(lit(if i_s <= cur { 1. } else { 0. })).child(line.text.clone()).into_any_element()
        } else {
            let next_line = l.lines.get(i + 1).and_then(|n| n.at_ms);
            div()
                .flex()
                .flex_wrap()
                .children(line.words.iter().enumerate().map(|(w, word)| {
                    let fill = if i_s < cur {
                        1.
                    } else if i_s > cur {
                        0.
                    } else {
                        let end = line
                            .words
                            .get(w + 1)
                            .map(|n| n.at_ms)
                            .or(next_line)
                            .unwrap_or(word.at_ms + 600);
                        let span = (end.min(word.at_ms + 1500) as f32 - word.at_ms as f32).max(1.);
                        (pos as f32 - word.at_ms as f32) / span
                    };
                    div().mr(px(5.)).text_color(lit(fill)).child(word.text.clone())
                }))
                .into_any_element()
        };
        let at = line.at_ms;
        let on = on.clone();
        div()
            .id(("lyric", i))
            .px(px(2.))
            .py(px(5.))
            .text_size(px(19.5))
            .line_height(px(25.35))
            .font_weight(gpui::FontWeight::BOLD)
            .cursor_pointer()
            .opacity(alpha)
            .hover(|s| s.opacity(1.))
            .on_click(move |_, w, cx| {
                cx.stop_propagation();
                on(at, w, cx)
            })
            .child(body)
            .into_any_element()
    });
    let hold = on.clone();
    let _ = playing;
    Some(
        div()
            .mt(px(12. * reveal))
            .h(px((HEIGHT + if l.synced { 0. } else { 8. }) * reveal))
            .overflow_hidden()
            .child(
                div()
                    .id(SharedString::from(format!("lyrics-{id}")))
                    .h(px(HEIGHT))
                    .px(px(4.))
                    .when(!l.synced, |d| d.py(px(4.)))
                    .overflow_y_scroll()
                    .track_scroll(scroll)
                    .on_scroll(move |_, w, cx| hold(None, w, cx))
                    .children(lines),
            )
            .into_any_element(),
    )
}
