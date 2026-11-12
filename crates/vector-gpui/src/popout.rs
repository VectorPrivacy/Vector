//! The floating player (`MediaPopout.svelte`, `lib/popout.svelte.js`): media that left its chat
//! still playing, in a box that moves by dragging and resizes from any corner.

use gpui::{Point, Pixels};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Audio,
    Video,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub kind: Kind,
    pub att: String,
    pub msg: String,
    pub chat: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Corner {
    Tl,
    Tr,
    Bl,
    Br,
}

impl Corner {
    pub const ALL: [Corner; 4] = [Corner::Tl, Corner::Tr, Corner::Bl, Corner::Br];

    pub fn top(self) -> bool {
        matches!(self, Corner::Tl | Corner::Tr)
    }

    pub fn left(self) -> bool {
        matches!(self, Corner::Tl | Corner::Bl)
    }
}

#[derive(Clone, Debug)]
pub enum Act {
    Press(Point<Pixels>, Option<Corner>),
    Drag(Point<Pixels>),
    Release,
    Near(Option<Corner>),
    Toggle,
    Seek(f32, bool),
    Prev,
    Next,
    Songs,
    Lyrics,
    Transcribe,
    Line(Option<u32>),
    SeekTo(u64),
    Track(usize),
    Art,
    Close,
    ShowInChat,
    SoundOnly,
    Video(crate::video::Act),
}

#[derive(Clone, Copy)]
enum Gesture {
    /// A press on the box: it moves once the pointer travels.
    Move { sx: f32, sy: f32, right: f32, bottom: f32, pending: bool },
    /// The corner opposite the grabbed one stays at (ax, ay).
    Resize { corner: Corner, ax: f32, ay: f32 },
}

pub const AUDIO_LIMITS: (f32, f32) = (260., 480.);
pub const VIDEO_LIMITS: (f32, f32) = (220., 720.);
/// Padding, the two rows and the gap between them, and the border.
pub const SOUND_H: f32 = 116.;
/// Under a video: the gap, the words row and the padding below, and the border.
pub const VIDEO_CHROME: f32 = 8. + 36. + 10. + 2.;
const KEY: &str = "media_popout";

#[derive(Serialize, Deserialize)]
struct Saved {
    right: f32,
    bottom: f32,
    #[serde(default)]
    widths: Widths,
    #[serde(default, rename = "soundOnly")]
    sound_only: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
struct Widths {
    audio: f32,
    video: f32,
}

impl Default for Widths {
    fn default() -> Self {
        Widths { audio: 320., video: 360. }
    }
}

pub struct Popout {
    pub item: Option<Item>,
    pub right: f32,
    pub bottom: f32,
    widths: Widths,
    sound_pref: bool,
    /// A tight window opens each video as sound; showing its picture is for that video only.
    open_sound: Option<bool>,
    gesture: Option<Gesture>,
    pub near: Option<Corner>,
    /// Display → Floating Player.
    pub enabled: bool,
    pub scrub: Option<f32>,
    /// When a drag of the box last ended: the release is not a click on what lies under it.
    moved: Option<std::time::Instant>,
    pub tracks_open: bool,
    /// The box's height as last laid out: panels opening grow it.
    pub measured: std::rc::Rc<std::cell::Cell<f32>>,
    last_h: f32,
}

impl Default for Popout {
    fn default() -> Self {
        let saved = crate::backend::device::get(KEY).and_then(|s| serde_json::from_str::<Saved>(&s).ok());
        Popout {
            item: None,
            right: saved.as_ref().map_or(16., |s| s.right),
            bottom: saved.as_ref().map_or(96., |s| s.bottom),
            widths: saved.as_ref().map_or_else(Widths::default, |s| s.widths),
            sound_pref: saved.as_ref().is_some_and(|s| s.sound_only),
            open_sound: None,
            gesture: None,
            near: None,
            enabled: true,
            scrub: None,
            moved: None,
            tracks_open: false,
            measured: Default::default(),
            last_h: 0.,
        }
    }
}

impl Popout {
    /// A press that ended a drag, not a tap.
    pub fn swallow(&self) -> bool {
        matches!(self.gesture, Some(Gesture::Move { pending: false, .. }))
            || self.moved.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(150))
    }

    fn save(&self) {
        let s = Saved { right: self.right, bottom: self.bottom, widths: self.widths, sound_only: self.sound_pref };
        if let Ok(j) = serde_json::to_string(&s) {
            crate::backend::device::set(KEY, j);
        }
    }

    pub fn open(&mut self, item: Item, win: (f32, f32)) {
        self.open_sound = (item.kind == Kind::Video && (win.0 < 640. || win.1 < 560.)).then_some(true);
        self.tracks_open = false;
        self.measured.set(0.);
        self.last_h = 0.;
        self.item = Some(item);
        self.scrub = None;
    }

    pub fn close(&mut self) -> Option<Item> {
        self.gesture = None;
        self.scrub = None;
        self.item.take()
    }

    pub fn sound_only(&self) -> bool {
        self.item.as_ref().is_some_and(|i| i.kind == Kind::Video) && self.open_sound.unwrap_or(self.sound_pref)
    }

    pub fn toggle_sound(&mut self) {
        match &mut self.open_sound {
            Some(o) => *o = !*o,
            None => {
                self.sound_pref = !self.sound_pref;
                self.save();
            }
        }
    }

    fn picture(&self) -> bool {
        self.item.as_ref().is_some_and(|i| i.kind == Kind::Video) && !self.sound_only()
    }

    /// The box's width in a window this size, for a video of this aspect.
    pub fn width(&self, win: (f32, f32), aspect: f32) -> f32 {
        if self.picture() {
            let cap = video_cap(win, aspect);
            self.widths.video.min(win.0 - 24.).min(cap).max(VIDEO_LIMITS.0.min(win.0 - 24.))
        } else {
            self.widths.audio.min(win.0 - 24.)
        }
    }

    pub fn height(&self, width: f32, aspect: f32) -> f32 {
        if self.picture() { (width - 2.) / aspect + VIDEO_CHROME } else { self.measured.get().max(SOUND_H) }
    }

    /// Kept inside the window.
    pub fn clamp(&mut self, win: (f32, f32), size: (f32, f32)) {
        self.right = self.right.min(win.0 - size.0 - 8.).max(8.);
        self.bottom = self.bottom.min(win.1 - size.1 - 8.).max(8.);
    }

    /// A height change grows the box toward the open space: in the window's upper half it
    /// keeps its top edge, in the lower its bottom one.
    pub fn grew(&mut self, win: (f32, f32)) {
        let h = self.measured.get();
        if h <= 0. {
            return;
        }
        if self.last_h > 0. && h != self.last_h && self.gesture.is_none() {
            let top = win.1 - self.bottom - self.last_h;
            if top + self.last_h / 2. < win.1 / 2. {
                self.bottom = (win.1 - top - h).max(8.);
            }
        }
        self.last_h = h;
    }

    pub fn dragging(&self) -> bool {
        self.gesture.is_some()
    }

    /// A gesture step; true when the box changed.
    pub fn gesture(&mut self, act: &Act, win: (f32, f32), aspect: f32) -> bool {
        let w = self.width(win, aspect);
        let h = self.height(w, aspect);
        match *act {
            Act::Press(p, corner) => {
                let (x, y) = (f32::from(p.x), f32::from(p.y));
                let (left, top) = (win.0 - self.right - w, win.1 - self.bottom - h);
                self.gesture = Some(match corner {
                    Some(c) => Gesture::Resize {
                        corner: c,
                        ax: if c.left() { left + w } else { left },
                        ay: if c.top() { top + h } else { top },
                    },
                    None => Gesture::Move { sx: x, sy: y, right: self.right, bottom: self.bottom, pending: true },
                });
                false
            }
            Act::Drag(p) => {
                let (x, y) = (f32::from(p.x), f32::from(p.y));
                match &mut self.gesture {
                    Some(Gesture::Move { sx, sy, right, bottom, pending }) => {
                        if *pending {
                            if ((x - *sx).powi(2) + (y - *sy).powi(2)).sqrt() < 4. {
                                return false;
                            }
                            *pending = false;
                        }
                        self.right = *right - (x - *sx);
                        self.bottom = *bottom - (y - *sy);
                        self.clamp(win, (w, h));
                        true
                    }
                    Some(Gesture::Resize { corner, ax, ay }) => {
                        let (corner, ax, ay) = (*corner, *ax, *ay);
                        let pin_right = corner.left();
                        let pin_bottom = corner.top();
                        let dx = if pin_right { ax - x } else { x - ax };
                        let dy = if pin_bottom { ay - y } else { y - ay };
                        let picture = self.picture();
                        // A video's height follows its width: its corner moves along one line,
                        // to the point on it nearest the pointer.
                        let want = if picture {
                            let chrome = VIDEO_CHROME;
                            (dx + (dy - chrome) / aspect) / (1. + 1. / (aspect * aspect))
                        } else {
                            dx
                        };
                        let room = (if pin_right { ax } else { win.0 - ax }) - 8.;
                        let (min, max) = if picture { VIDEO_LIMITS } else { AUDIO_LIMITS };
                        let cap = if picture { min.max(video_cap(win, aspect)) } else { max };
                        let nw = want.min(max).min(cap).min(room).max(min).round();
                        if picture {
                            self.widths.video = nw;
                        } else {
                            self.widths.audio = nw;
                        }
                        let bw = self.width(win, aspect);
                        let bh = self.height(bw, aspect);
                        self.right = if pin_right { win.0 - ax } else { win.0 - ax - bw };
                        self.bottom = if pin_bottom { win.1 - ay } else { win.1 - ay - bh };
                        true
                    }
                    None => false,
                }
            }
            Act::Release => {
                let Some(g) = self.gesture.take() else { return false };
                match g {
                    Gesture::Move { pending: true, .. } => false,
                    Gesture::Resize { .. } => {
                        self.clamp(win, (w, h));
                        self.save();
                        true
                    }
                    Gesture::Move { .. } => {
                        self.moved = Some(std::time::Instant::now());
                        self.save();
                        true
                    }
                }
            }
            Act::Near(c) => {
                let changed = self.near != c;
                self.near = c;
                changed
            }
            _ => false,
        }
    }
}

/// The picture keeps to about half the window's height, the box to 60% of its width.
fn video_cap(win: (f32, f32), aspect: f32) -> f32 {
    (win.0 * 0.6).min((win.1 * 0.55).min(win.1 - 160.) * aspect).max(160.)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pop() -> Popout {
        Popout {
            item: Some(Item { kind: Kind::Audio, att: "a".into(), msg: "m".into(), chat: "c".into() }),
            right: 16.,
            bottom: 96.,
            widths: Widths::default(),
            sound_pref: false,
            open_sound: None,
            gesture: None,
            near: None,
            enabled: true,
            scrub: None,
            moved: None,
            tracks_open: false,
            measured: Default::default(),
            last_h: 0.,
        }
    }

    #[test]
    fn a_tap_does_not_move_the_box() {
        let mut p = pop();
        let win = (1280., 800.);
        p.gesture(&Act::Press(gpui::point(px(1000.), px(620.)), None), win, 1.);
        assert!(!p.gesture(&Act::Drag(gpui::point(px(1002.), px(621.))), win, 1.));
        p.gesture(&Act::Release, win, 1.);
        assert_eq!((p.right, p.bottom), (16., 96.));
    }

    #[test]
    fn a_drag_moves_it_and_the_window_holds_it() {
        let mut p = pop();
        let win = (1280., 800.);
        p.gesture(&Act::Press(gpui::point(px(1000.), px(620.)), None), win, 1.);
        p.gesture(&Act::Drag(gpui::point(px(900.), px(600.))), win, 1.);
        assert_eq!((p.right, p.bottom), (116., 116.));
        p.gesture(&Act::Drag(gpui::point(px(-500.), px(-500.))), win, 1.);
        assert_eq!(p.right, 1280. - 320. - 8.);
    }

    #[test]
    fn a_corner_resize_pins_the_opposite_one() {
        let mut p = pop();
        let win = (1280., 800.);
        // The top-left grip: the bottom-right corner (right 16) stays.
        p.gesture(&Act::Press(gpui::point(px(944.), px(588.)), Some(Corner::Tl)), win, 1.);
        p.gesture(&Act::Drag(gpui::point(px(904.), px(588.))), win, 1.);
        assert_eq!((p.width(win, 1.), p.right), (360., 16.));
        p.gesture(&Act::Drag(gpui::point(px(0.), px(588.))), win, 1.);
        assert_eq!(p.width(win, 1.), AUDIO_LIMITS.1);
    }

    use gpui::px;
}
