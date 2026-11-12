//! `QrScanner.svelte` + `scan.js`: the camera full screen behind a viewfinder, and the first
//! Vector QR it reads routed where it leads.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::prelude::*;
use gpui::{AnyElement, Context, RenderImage, Task, Window, div, img, px};

use super::VectorApp;
use crate::callvideo::{self, In, VideoKind};
use crate::theme::*;

/// Decoding is the costly half: a few reads a second find a code as fast as a hand holds still.
const DECODE_EVERY: Duration = Duration::from_millis(100);
/// The square handed to the decoder, at most this many pixels a side.
const DECODE_SIDE: usize = 640;
/// The preview's width: enough for a phone-sized window without painting a full frame.
const PREVIEW_W: usize = 960;
const NUDGE_GAP: Duration = Duration::from_secs(3);

pub(super) struct Scanner {
    capture: Option<callvideo::Capture>,
    pub frame: Option<Arc<RenderImage>>,
    pub retired: Vec<Arc<RenderImage>>,
    nudged: Option<Instant>,
    _task: Task<()>,
}

enum Out {
    Frame(Arc<RenderImage>),
    Read(String),
    Lost,
}

/// The centre square of the luma plane, at most `DECODE_SIDE` a side: what the viewfinder frames.
fn read_qr(pic: &callvideo::yuv::I420) -> Option<String> {
    let side = pic.w.min(pic.h);
    if side == 0 {
        return None;
    }
    let (x0, y0) = ((pic.w - side) / 2, (pic.h - side) / 2);
    let n = side.min(DECODE_SIDE);
    let y = &pic.data[..pic.w * pic.h];
    let mut grey = rqrr::PreparedImage::prepare_from_greyscale(n, n, |x, yy| {
        let sx = x0 + x * side / n;
        let sy = y0 + yy * side / n;
        y[sy * pic.w + sx]
    });
    grey.detect_grids().into_iter().find_map(|g| g.decode().ok().map(|(_, text)| text))
}

fn preview(pic: &callvideo::yuv::I420) -> Option<Arc<RenderImage>> {
    let pic = if pic.w > PREVIEW_W { pic.scale(PREVIEW_W, pic.h * PREVIEW_W / pic.w) } else { pic.clone() };
    let (yp, u, v) = pic.planes();
    let bgra = callvideo::yuv::to_bgra(yp, u, v, (pic.w, pic.w / 2), pic.w, pic.h);
    let buf = image::RgbaImage::from_raw(pic.w as u32, pic.h as u32, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
}

/// Newest frame only: a slow decode drops what queued behind it rather than falling behind.
fn pump(rx: Receiver<In>, out: UnboundedSender<Out>) {
    let mut last = Instant::now() - DECODE_EVERY;
    loop {
        let mut newest = match rx.recv() {
            Ok(m) => m,
            Err(_) => return,
        };
        loop {
            match rx.try_recv() {
                Ok(m) => newest = m,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        match newest {
            In::Frame { pic, .. } => {
                if let Some(image) = preview(&pic)
                    && out.unbounded_send(Out::Frame(image)).is_err()
                {
                    return;
                }
                if last.elapsed() >= DECODE_EVERY {
                    last = Instant::now();
                    if let Some(text) = read_qr(&pic) {
                        let _ = out.unbounded_send(Out::Read(text.trim().to_string()));
                    }
                }
            }
            In::SourceLost(_) => {
                let _ = out.unbounded_send(Out::Lost);
                return;
            }
            _ => {}
        }
    }
}

impl VectorApp {
    pub(super) fn open_qr_scanner(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.scanner.is_some() {
            return;
        }
        if !self.live {
            self.scanner = Some(Scanner { capture: None, frame: None, retired: Vec::new(), nudged: None, _task: Task::ready(()) });
            cx.notify();
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let (out_tx, mut out_rx) = unbounded();
        let opening = cx.background_spawn(async move { callvideo::open_camera(VideoKind::Camera, tx) });
        std::thread::Builder::new()
            .name("qr-scan".into())
            .spawn(move || pump(rx, out_tx))
            .ok();
        let task = cx.spawn(async move |this, cx| {
            let capture = opening.await;
            let ok = this
                .update(cx, |app, cx| match (capture, app.scanner.as_mut()) {
                    (Ok(c), Some(s)) => {
                        s.capture = Some(c);
                        true
                    }
                    (Ok(_), None) => false,
                    (Err(e), _) => {
                        app.scanner = None;
                        log::warn!("qr scanner: {e}");
                        app.show_toast("Camera unavailable", cx);
                        false
                    }
                })
                .unwrap_or(false);
            if !ok {
                return;
            }
            while let Some(out) = out_rx.next().await {
                let alive = this
                    .update(cx, |app, cx| {
                        let Some(s) = app.scanner.as_mut() else { return false };
                        match out {
                            Out::Frame(image) => {
                                if let Some(old) = s.frame.replace(image) {
                                    s.retired.push(old);
                                }
                                cx.notify();
                            }
                            Out::Read(text) => app.route_scanned(text, cx),
                            Out::Lost => {
                                app.close_qr_scanner(cx);
                                app.show_toast("Camera unavailable", cx);
                                return false;
                            }
                        }
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    return;
                }
            }
        });
        self.scanner = Some(Scanner { capture: None, frame: None, retired: Vec::new(), nudged: None, _task: task });
        cx.notify();
    }

    pub(super) fn close_qr_scanner(&mut self, cx: &mut Context<Self>) {
        if let Some(mut s) = self.scanner.take() {
            s.capture = None;
            self.retired_frames.extend(s.frame.take());
            self.retired_frames.append(&mut s.retired);
        }
        cx.notify();
    }

    /// `routeScannedQr`: a sign-in code sends this account to that device; a link opens its
    /// place; an npub or a wrapped invite opens as typed into New Message would.
    fn route_scanned(&mut self, text: String, cx: &mut Context<Self>) {
        let transfer = text.trim_start().to_lowercase().starts_with("vector transfer code:");
        let link = (!transfer).then(|| vector_core::deep_link::parse_deep_link(text.trim())).flatten();
        let invite = crate::newchat::invite_key(&text);
        let go = crate::newchat::go_link(&text);
        let npub = text
            .split(|c: char| !c.is_ascii_alphanumeric())
            .find(|w| w.len() == 63 && w.starts_with("npub1"))
            .map(str::to_string);
        if !transfer && link.is_none() && invite.is_none() && go.is_none() && npub.is_none() {
            let now = Instant::now();
            if let Some(s) = self.scanner.as_mut()
                && s.nudged.is_none_or(|t| now - t >= NUDGE_GAP)
            {
                s.nudged = Some(now);
                self.show_toast("Not a Vector QR code", cx);
            }
            return;
        }
        self.close_qr_scanner(cx);
        self.new_chat = None;
        let view = cx.entity().downgrade();
        cx.defer(move |cx| {
            let Some(app) = view.upgrade() else { return };
            let Some(window) = cx.active_window() else { return };
            let _ = window.update(cx, |_, window, cx| {
                app.update(cx, |app, cx| {
                    if transfer {
                        app.security.update(cx, |f, cx| f.join_scanned(true, &text, cx));
                    } else if let Some(a) = link {
                        app.open_deep_link(a, window, cx);
                    } else if let Some(key) = invite {
                        app.preview_invite_link(key, window, cx);
                    } else if let Some(g) = go {
                        app.open_go_link(g, window, cx);
                    } else if let Some(n) = npub {
                        app.open_profile(n, cx);
                    }
                    cx.notify();
                })
            });
        });
    }

    /// `.qr-scanner`: the camera behind a dimmed viewfinder, a hint, and Cancel.
    pub(super) fn qr_scanner_view(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.scanner.as_ref()?;
        let vp = window.viewport_size();
        let (w, h) = (f32::from(vp.width), f32::from(vp.height));
        let side = (w * 0.7).min(280.);
        let (left, top) = ((w - side) / 2., (h - side) / 2. - 20.);
        let veil = gpui::black().opacity(0.55);
        Some(
            div()
                .id("qr-scanner")
                .occlude()
                .absolute()
                .inset_0()
                .bg(gpui::black())
                .children(s.frame.clone().map(|f| img(f).absolute().inset_0().size_full().object_fit(gpui::ObjectFit::Cover)))
                .child(div().absolute().left_0().right_0().top_0().h(px(top)).bg(veil))
                .child(div().absolute().left_0().right_0().top(px(top + side)).bottom_0().bg(veil))
                .child(div().absolute().left_0().w(px(left)).top(px(top)).h(px(side)).bg(veil))
                .child(div().absolute().left(px(left + side)).right_0().top(px(top)).h(px(side)).bg(veil))
                .child(
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(top))
                        .size(px(side))
                        .rounded(px(18.))
                        .border_2()
                        .border_color(c(ACCENT)),
                )
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px(top + side + 18.))
                        .flex()
                        .justify_center()
                        .text_size(px(14.))
                        .text_color(white_a(0.85))
                        .child("Point at a Vector QR code"),
                )
                .child(
                    div()
                        .id("qr-scanner-cancel")
                        .absolute()
                        .bottom(px(28.))
                        .left(px((w - (w * 0.8).min(360.)) / 2.))
                        .w(px((w * 0.8).min(360.)))
                        .py(px(12.))
                        .rounded(px(12.))
                        .bg(c(ACCENT))
                        .flex()
                        .justify_center()
                        .text_size(px(16.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(c(0x000000))
                        .cursor_pointer()
                        .on_click(cx.listener(|app, _, _, cx| app.close_qr_scanner(cx)))
                        .child("Cancel"),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_code_off_a_frame() {
        let text = "vector transfer code: 7-orbit-lemon-stage";
        let qr = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium).unwrap();
        let (w, h, cell) = (1280usize, 720usize, 12usize);
        let mut pic = callvideo::yuv::I420::new(w, h);
        pic.data[..w * h].fill(235);
        let n = qr.size() as usize;
        let (x0, y0) = ((w - n * cell) / 2, (h - n * cell) / 2);
        for my in 0..n {
            for mx in 0..n {
                if qr.get_module(mx as i32, my as i32) {
                    for dy in 0..cell {
                        let row = (y0 + my * cell + dy) * w + x0 + mx * cell;
                        pic.data[row..row + cell].fill(16);
                    }
                }
            }
        }
        assert_eq!(read_qr(&pic).as_deref(), Some(text));
    }
}
