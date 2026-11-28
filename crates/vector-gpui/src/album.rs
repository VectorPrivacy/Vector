//! An album in one file (`AudioSession`'s tracks): songs are places in the file, so moving
//! on inside it is gapless; the listener's way through is in order or shuffled, with repeat.

use rand::seq::SliceRandom;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Repeat {
    Off,
    All,
    One,
}

pub struct Track {
    pub start: u64,
    /// None for the last: it runs to the file's end.
    pub end: Option<u64>,
    pub title: String,
}

pub struct Album {
    pub tracks: Vec<Track>,
    /// The song under the playhead; -1 before the first.
    pub track: isize,
    pub shuffle: bool,
    pub repeat: Repeat,
    order: Vec<usize>,
    order_at: isize,
}

/// What the playhead should do after a step.
#[derive(Debug, PartialEq)]
pub enum Step {
    Stay,
    Seek(u64),
    /// Nothing follows: stop here.
    Stop(u64),
}

impl Album {
    pub fn new(chapters: &[vector_media::chapters::Chapter], pos: u64) -> Self {
        let mut a = Album {
            tracks: chapters
                .iter()
                .map(|c| Track { start: c.start_ms as u64, end: c.end_ms.map(u64::from), title: c.title.clone() })
                .collect(),
            track: -1,
            shuffle: false,
            repeat: Repeat::Off,
            order: Vec::new(),
            order_at: 0,
        };
        a.track = a.track_at(pos);
        a.reorder();
        a
    }

    pub fn track_at(&self, ms: u64) -> isize {
        let mut at = -1;
        for (i, t) in self.tracks.iter().enumerate() {
            if t.start <= ms + 1 {
                at = i as isize;
            } else {
                break;
            }
        }
        at
    }

    /// Where song `i` runs in a file `duration` long.
    pub fn span(&self, i: isize, duration: u64) -> (u64, u64) {
        match usize::try_from(i).ok().and_then(|i| self.tracks.get(i)) {
            Some(t) => (t.start, t.end.unwrap_or(duration)),
            None => (0, duration),
        }
    }

    /// The span the controls show: the current song, or the first before any.
    pub fn now(&self, duration: u64) -> (u64, u64) {
        self.span(self.track.max(0), duration)
    }

    pub fn set_shuffle(&mut self, on: bool) {
        self.shuffle = on;
        self.reorder();
    }

    pub fn cycle_repeat(&mut self) {
        self.repeat = match self.repeat {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        };
    }

    /// The tracks in order, or shuffled with the current first.
    fn reorder(&mut self) {
        let n = self.tracks.len();
        let cur = self.track.max(0) as usize;
        let mut rest: Vec<usize> = (0..n).filter(|&i| i != cur).collect();
        if self.shuffle {
            rest.shuffle(&mut rand::thread_rng());
        }
        self.order = if n > 0 { std::iter::once(cur).chain(rest).collect() } else { Vec::new() };
        self.order_at = 0;
    }

    fn next_index(&mut self) -> Option<usize> {
        if self.tracks.is_empty() {
            return None;
        }
        if self.shuffle {
            if ((self.order_at + 1) as usize) < self.order.len() {
                return Some(self.order[(self.order_at + 1) as usize]);
            }
            if self.repeat != Repeat::All {
                return None;
            }
            let last = self.order.get(self.order_at.max(0) as usize).copied();
            self.reorder();
            // A fresh round never starts with the song that just ended.
            if self.order.len() > 1 && self.order.first().copied() == last {
                self.order.rotate_left(1);
            }
            self.order_at = -1;
            return self.order.first().copied();
        }
        let i = (self.track + 1) as usize;
        if i < self.tracks.len() {
            Some(i)
        } else if self.repeat == Repeat::All {
            Some(0)
        } else {
            None
        }
    }

    fn goto(&mut self, i: usize, keep_order: bool) -> u64 {
        if self.shuffle && !keep_order {
            if let Some(at) = self.order.iter().position(|&o| o == i) {
                self.order_at = at as isize;
            }
        }
        self.track = i as isize;
        self.tracks[i].start
    }

    /// Song `i` from its start, whatever the order was.
    pub fn play_track(&mut self, i: usize) -> Option<u64> {
        if i >= self.tracks.len() {
            return None;
        }
        let ms = self.goto(i, false);
        if self.shuffle {
            self.reorder();
        }
        Some(ms)
    }

    pub fn next(&mut self) -> Option<u64> {
        let n = self.next_index()?;
        Some(self.goto(n, false))
    }

    /// Back to the start of the song, or near its start already, to the song before.
    pub fn prev(&mut self, pos: u64, duration: u64) -> u64 {
        let i = self.track.max(0) as usize;
        if pos.saturating_sub(self.span(i as isize, duration).0) > 3000 {
            return self.goto(i, false);
        }
        if self.shuffle && self.order_at > 0 {
            self.order_at -= 1;
            return self.goto(self.order[self.order_at as usize], true);
        }
        if !self.shuffle && i > 0 {
            return self.goto(i - 1, false);
        }
        if !self.shuffle && self.repeat == Repeat::All {
            return self.goto(self.tracks.len() - 1, false);
        }
        self.goto(i, false)
    }

    /// A seek is a choice, not the song moving on: the track follows it quietly.
    pub fn seeked(&mut self, ms: u64) {
        self.track = self.track_at(ms);
    }

    /// While playing: note the song moving on, and step in when the listener's way is not
    /// simply the next song in the file.
    pub fn tick(&mut self, pos: u64) -> Step {
        let i = self.track_at(pos);
        if i == self.track {
            return Step::Stay;
        }
        let from = self.track;
        self.track = i;
        if i != from + 1 || from < 0 {
            return Step::Stay;
        }
        if self.repeat == Repeat::One {
            return Step::Seek(self.goto(from as usize, false));
        }
        if self.shuffle {
            match self.next_index() {
                None => {
                    let first = self.order.first().copied().unwrap_or(0);
                    return Step::Stop(self.goto(first, false));
                }
                Some(n) => {
                    self.order_at += 1;
                    if n as isize != i {
                        return Step::Seek(self.goto(n, true));
                    }
                }
            }
        }
        Step::Stay
    }

    /// The file ended: where to carry on from, if anywhere.
    pub fn ended(&mut self) -> Option<u64> {
        let last = self.track;
        let n = if self.repeat == Repeat::One { usize::try_from(last).ok() } else { self.next_index() };
        match n {
            Some(n) => {
                if self.shuffle && self.repeat != Repeat::One {
                    self.order_at += 1;
                }
                Some(self.goto(n, true))
            }
            None => {
                self.track = self.track_at(0);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vector_media::chapters::Chapter;

    fn album() -> Album {
        let ch = |s: u32, e: Option<u32>, t: &str| Chapter { start_ms: s, end_ms: e, title: t.into() };
        Album::new(&[ch(0, Some(1000), "a"), ch(1000, Some(2000), "b"), ch(2000, None, "c")], 0)
    }

    #[test]
    fn songs_follow_in_order_and_stop_at_the_end() {
        let mut a = album();
        assert_eq!(a.tick(1200), Step::Stay);
        assert_eq!(a.track, 1);
        assert_eq!(a.next(), Some(2000));
        assert_eq!(a.ended(), None);
        assert_eq!(a.track, 0);
    }

    #[test]
    fn repeat_one_holds_the_song() {
        let mut a = album();
        a.cycle_repeat();
        a.cycle_repeat();
        assert_eq!(a.tick(1100), Step::Seek(0));
        assert_eq!(a.track, 0);
    }

    #[test]
    fn prev_restarts_a_song_well_under_way() {
        let mut a = album();
        a.play_track(2);
        assert_eq!(a.prev(5500, 9000), 2000);
        assert_eq!(a.prev(2100, 9000), 1000);
    }
}
