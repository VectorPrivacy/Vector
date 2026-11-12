//! Voice transcription on Whisper (`Transcription.svelte`, `cTranscriber`): one transcript per
//! attachment, the model downloaded on first use with its progress in the card, and the
//! sections lit as the audio plays.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, HighlightStyle, InteractiveElement, InteractiveText, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, StyledText, Window, div,
    prelude::*, px, svg,
};

use crate::theme::*;

/// What a transcription produced; mirrors `whisper::TranscriptionResult`.
#[derive(Clone)]
pub struct Section {
    pub text: String,
    pub at: u64,
}

pub enum Phase {
    Loading,
    Ready { sections: Vec<Section>, lang: String, language: String },
    Error(String),
}

pub struct Transcript {
    pub phase: Phase,
    pub open: bool,
    /// When `open` last changed, for the slide.
    since: Instant,
}

impl Transcript {
    /// 0 shut to 1 open: 0.35s in, 0.25s out, eased.
    pub fn reveal(&self) -> f32 {
        let t = self.since.elapsed().as_secs_f32();
        let ease = |x: f32| 1. - (1. - x).powi(3);
        if self.open { ease((t / 0.35).min(1.)) } else { 1. - ease((t / 0.25).min(1.)) }
    }

    pub fn moving(&self) -> bool {
        self.since.elapsed() < Duration::from_millis(400) || matches!(self.phase, Phase::Loading)
    }
}

/// The model download, shown in whichever card asked for it.
pub struct Download {
    pub pct: u32,
    pub text: String,
    pub failed: bool,
    /// When it should leave the card (after "Ready!" or a failure).
    pub until: Option<Instant>,
}

#[derive(Default)]
pub struct Transcripts {
    by_att: HashMap<String, Transcript>,
    pub download: Option<Download>,
}

/// What the app should start: a transcription of `att`.
pub struct Job {
    pub att: String,
}

impl Transcripts {
    pub fn get(&self, att: &str) -> Option<&Transcript> {
        self.by_att.get(att)
    }

    /// `toggleTranscript`: open or shut one that's ready, or make it.
    pub fn toggle(&mut self, att: &str) -> Option<Job> {
        match self.by_att.get_mut(att) {
            Some(t) if matches!(t.phase, Phase::Loading) => None,
            Some(t) if matches!(t.phase, Phase::Ready { .. }) => {
                t.open = !t.open;
                t.since = Instant::now();
                None
            }
            _ => {
                self.by_att.insert(
                    att.to_string(),
                    Transcript { phase: Phase::Loading, open: false, since: Instant::now() },
                );
                Some(Job { att: att.to_string() })
            }
        }
    }

    pub fn landed(&mut self, att: &str, phase: Phase) {
        self.by_att.insert(att.to_string(), Transcript { phase, open: true, since: Instant::now() });
    }

    pub fn progress(&mut self, pct: u32) {
        self.download = Some(Download { pct, text: format!("Downloading... {pct}%"), failed: false, until: None });
    }

    pub fn download_done(&mut self, ok: bool) {
        self.download = Some(if ok {
            Download { pct: 100, text: "Ready!".into(), failed: false, until: Some(Instant::now() + Duration::from_secs(1)) }
        } else {
            Download {
                pct: self.download.as_ref().map_or(0, |d| d.pct),
                text: "Download Failed".into(),
                failed: true,
                until: Some(Instant::now() + Duration::from_secs(3)),
            }
        });
    }

    /// One frame: true while anything still moves.
    pub fn tick(&mut self) -> bool {
        if self.download.as_ref().is_some_and(|d| d.until.is_some_and(|u| Instant::now() >= u)) {
            self.download = None;
        }
        self.download.is_some() || self.by_att.values().any(|t| t.moving())
    }
}

/// The index of the section under the playhead, while playing.
fn current(sections: &[Section], pos: u64, playing: bool) -> Option<usize> {
    if !playing {
        return None;
    }
    sections.iter().enumerate().rev().find(|(_, s)| pos >= s.at).map(|(i, _)| i)
}

/// `.audio-transcribe-btn` in its transcript role.
pub fn button(id: &str, t: Option<&Transcript>, accent: gpui::Hsla, on: Rc<dyn Fn(&mut Window, &mut App)>) -> AnyElement {
    let (icon, lit, spin) = match t.map(|t| (&t.phase, t.open)) {
        Some((Phase::Loading, _)) => ("icons/loading.svg", false, true),
        Some((Phase::Ready { .. }, true)) => ("icons/file-minus.svg", true, false),
        _ => ("icons/file-plus.svg", false, false),
    };
    let glyph = svg().path(icon).size(px(17.)).text_color(if lit { accent } else { white_a(0.7) });
    div()
        .id(SharedString::from(format!("audio-transcribe-{id}")))
        .size(px(34.))
        .ml(px(10.))
        .flex_shrink_0()
        .rounded(px(8.))
        .border_1()
        .border_color(c(0x363636))
        .flex()
        .items_center()
        .justify_center()
        .when(!spin, |d| d.cursor_pointer().hover(|s| s.bg(white_a(0.07))))
        .on_click(move |_, w, cx| {
            cx.stop_propagation();
            on(w, cx)
        })
        .child(if spin {
            use gpui::AnimationExt as _;
            glyph
                .with_animation(
                    "transcribe-spin",
                    gpui::Animation::new(Duration::from_secs(1)).repeat(),
                    |s, t| s.with_transformation(gpui::Transformation::rotate(gpui::percentage(t))),
                )
                .into_any_element()
        } else {
            glyph.into_any_element()
        })
        .into_any_element()
}

/// `.transcribe-download`: the model's progress where the bars were, with its Cancel.
pub fn download_view(d: &Download, on_cancel: Rc<dyn Fn(&mut Window, &mut App)>) -> AnyElement {
    div()
        .flex_1()
        .min_w_0()
        .ml(px(10.))
        .px(px(10.))
        .py(px(6.))
        .rounded(px(8.))
        .bg(ca(0, 0, 0, 0.25))
        .child(
            div()
                .w_full()
                .min_w(px(120.))
                .child(
                    div()
                        .flex()
                        .justify_center()
                        .text_size(px(12.))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_color(c(PRIMARY))
                        .child(d.text.clone()),
                )
                .child(
                    div().mt(px(4.)).h(px(4.)).w_full().rounded(px(2.)).bg(ca(60, 60, 60, 0.8)).child(
                        div()
                            .h_full()
                            .w(gpui::relative(d.pct.min(100) as f32 / 100.))
                            .rounded(px(2.))
                            .bg(if d.failed { c(0xff5e5e) } else { c(PRIMARY) }),
                    ),
                )
                .child(
                    div().flex().justify_center().child(
                        div()
                            .id("cancel-download-inline")
                            .mt(px(6.))
                            .px(px(12.))
                            .py(px(3.))
                            .text_size(px(11.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .rounded(px(4.))
                            .border_1()
                            .border_color(c(crate::timeline::DANGER))
                            .text_color(c(crate::timeline::DANGER))
                            .cursor_pointer()
                            .hover(|s| s.bg(ca(255, 46, 169, 0.15)))
                            .on_click(move |_, w, cx| {
                                cx.stop_propagation();
                                on_cancel(w, cx)
                            })
                            .child("Cancel"),
                    ),
                ),
        )
        .into_any_element()
}

/// `.transcription-result`: the language, then the sections; a click on one seeks there.
pub fn view(
    id: &str,
    t: &Transcript,
    pos: u64,
    playing: bool,
    accent: gpui::Hsla,
    on_seek: Rc<dyn Fn(u64, &mut Window, &mut App)>,
) -> Option<AnyElement> {
    let reveal = t.reveal();
    if reveal <= 0. || matches!(t.phase, Phase::Loading) {
        return None;
    }
    let body: AnyElement = match &t.phase {
        Phase::Error(e) => div().text_color(c(0xff5e5e)).child(format!("Error: {e}")).into_any_element(),
        Phase::Ready { sections, lang, language } => {
            let named = (!language.is_empty() && language != "en" && !lang.is_empty() && lang != "auto")
                .then(|| crate::langnames::name(language))
                .flatten();
            let lang_row = named.map(|name| {
                let flag = crate::langnames::flag(lang).unwrap_or_else(|| "\u{1F30D}".into());
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(10.))
                    .mb(px(6.))
                    .h(px(16.))
                    .text_size(px(10.8))
                    .line_height(px(15.12))
                    .text_color(white_a(0.34))
                    .child("Language Detected:")
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .min_w_0()
                            .text_color(white_a(0.55))
                            .children(crate::emoji::twemoji(&flag).map(|p| gpui::img(p).size(px(14.))))
                            .child(name),
                    )
            });
            let text: AnyElement = if sections.is_empty() {
                div().child("No transcription available").into_any_element()
            } else {
                let mut joined = String::new();
                let mut ranges = Vec::with_capacity(sections.len());
                for (i, s) in sections.iter().enumerate() {
                    let start = joined.len();
                    joined.push_str(&s.text);
                    ranges.push(start..joined.len());
                    if i + 1 < sections.len() {
                        // The sections' 1px side margins, as hair spaces.
                        joined.push_str("\u{200A} \u{200A}");
                    }
                }
                let lit = current(sections, pos, playing);
                let highlights = lit
                    .map(|i| {
                        vec![(
                            ranges[i].clone(),
                            HighlightStyle {
                                color: Some(accent),
                                background_color: Some(accent.opacity(0.16)),
                                ..Default::default()
                            },
                        )]
                    })
                    .unwrap_or_default();
                let ats: Vec<u64> = sections.iter().map(|s| s.at).collect();
                InteractiveText::new(
                    SharedString::from(format!("transcript-{id}")),
                    StyledText::new(joined).with_highlights(highlights),
                )
                .on_click(ranges, move |i, w, cx| on_seek(ats[i], w, cx))
                .into_any_element()
            };
            div()
                .children(lang_row)
                .child(div().text_size(px(15.)).line_height(px(22.5)).text_color(white_a(0.92)).child(text))
                .into_any_element()
        }
        Phase::Loading => return None,
    };
    Some(
        div()
            .mt(px(10. * reveal))
            .rounded(px(10.))
            .bg(ca(0, 0, 0, 0.3))
            .overflow_hidden()
            .opacity(reveal.max(0.2))
            .when(reveal < 1., |d| d.max_h(px(400. * reveal)))
            .child(div().px(px(14.)).py(px(12. * reveal)).child(body))
            .into_any_element(),
    )
}

/// Speech in a format Whisper reads: a voice message, or a named file whose tags (once read)
/// name no track. Music shows its own lyrics or nothing.
pub fn supported(att: &vector_core::Attachment, tags_read: bool, music: bool) -> bool {
    ["wav", "mp3", "flac", "ogg", "opus", "oga", "m4a", "aac"].contains(&att.extension.as_str())
        && (att.name.is_empty() || (tags_read && !music))
}
