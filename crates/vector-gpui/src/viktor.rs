//! Viktor, the empty list's mascot (`bindViktor`): idle on the smile's first frame, the
//! smile looping while hovered, one exclamation on a click and once to greet.

use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{Context, SharedString, Task, Window, div, img, px};

const SMILE: &str = "icons/viktor-smile.gif";
const EXCLAIM: &str = "icons/viktor-exclaim.gif";
const SMILE_MS: u64 = 1500;
const EXCLAIM_MS: u64 = 1600;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Idle,
    Smile,
    Exclaim,
}

pub struct Viktor {
    mode: Mode,
    /// Each play is a fresh element id, so its clip starts at frame one.
    play: u32,
    hovering: bool,
    smile_start: Instant,
    timer: Option<Task<()>>,
    size: f32,
}

impl Viktor {
    /// `greet`: one exclamation shortly after the first paint.
    pub fn new(size: f32, greet: bool, cx: &mut Context<Self>) -> Self {
        let mut v = Self { mode: Mode::Idle, play: 0, hovering: false, smile_start: Instant::now(), timer: None, size };
        if greet {
            v.after(400, cx, |v, cx| v.exclaim(cx));
        }
        v
    }

    fn after(&mut self, ms: u64, cx: &mut Context<Self>, then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static) {
        self.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(ms)).await;
            this.update(cx, |v, cx| then(v, cx)).ok();
        }));
    }

    fn smile(&mut self, cx: &mut Context<Self>) {
        self.timer = None;
        self.mode = Mode::Smile;
        self.play += 1;
        self.smile_start = Instant::now();
        cx.notify();
    }

    fn idle(&mut self, cx: &mut Context<Self>) {
        self.mode = Mode::Idle;
        cx.notify();
    }

    fn exclaim(&mut self, cx: &mut Context<Self>) {
        self.mode = Mode::Exclaim;
        self.play += 1;
        cx.notify();
        self.after(EXCLAIM_MS, cx, |v, cx| if v.hovering { v.smile(cx) } else { v.idle(cx) });
    }

    fn hover(&mut self, on: bool, cx: &mut Context<Self>) {
        self.hovering = on;
        match (on, self.mode) {
            (true, Mode::Idle) => self.smile(cx),
            (true, Mode::Smile) => self.timer = None,
            // The loop in progress runs to its end before settling.
            (false, Mode::Smile) => {
                let into = self.smile_start.elapsed().as_millis() as u64 % SMILE_MS;
                self.after(SMILE_MS - into, cx, |v, cx| {
                    if v.mode == Mode::Smile && !v.hovering {
                        v.idle(cx);
                    }
                });
            }
            _ => {}
        }
    }
}

impl Render for Viktor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = px(self.size);
        // Without an id an image holds no frame state, so the clip shows its first frame: the idle pose.
        let pose = match self.mode {
            Mode::Idle => img(SMILE).size(size).into_any_element(),
            Mode::Smile => img(SMILE).id(SharedString::from(format!("viktor-{}", self.play))).size(size).into_any_element(),
            Mode::Exclaim => img(EXCLAIM).id(SharedString::from(format!("viktor-{}", self.play))).size(size).into_any_element(),
        };
        div()
            .id("viktor")
            .size(size)
            .flex_shrink_0()
            .cursor_pointer()
            .on_hover(cx.listener(|v, on: &bool, _, cx| v.hover(*on, cx)))
            .on_click(cx.listener(|v, _, _, cx| v.exclaim(cx)))
            .child(pose)
    }
}
