//! `RouteButton`: where a phone plays a call (the ear, the speaker, a headset); each tap moves
//! it to the next. Shown only while the phone offers more than one.

use gpui::{App, IntoElement, ParentElement, Styled, Window, prelude::*, px, svg};

use crate::call::call_btn;
use crate::kit::Tipped as _;
use crate::t;
use crate::theme::{ACCENT, c, white_a};

#[derive(Clone, PartialEq, Debug)]
pub struct Route {
    pub current: String,
    pub available: Vec<String>,
}

#[cfg(target_os = "android")]
pub fn current() -> Option<Route> {
    crate::android::call_audio::route().map(|r| Route { current: r.current, available: r.available })
}

#[cfg(not(target_os = "android"))]
pub fn current() -> Option<Route> {
    None
}

/// Plays the call through `route` from now on.
#[cfg(target_os = "android")]
pub fn set(route: &str) -> Result<(), String> {
    crate::android::call_audio::set_route(route).map_err(|_| t::call_route_unavailable().to_string())
}

#[cfg(not(target_os = "android"))]
pub fn set(_route: &str) -> Result<(), String> {
    Err(t::call_route_unavailable().to_string())
}

impl Route {
    /// The one after the current, round the list.
    pub fn next(&self) -> Option<&str> {
        let at = self.available.iter().position(|r| *r == self.current);
        let n = self.available.len();
        (n > 0).then(|| self.available[at.map_or(0, |i| (i + 1) % n)].as_str())
    }
}

pub fn button(
    id: &'static str,
    on: impl Fn(String, &mut Window, &mut App) + 'static,
) -> Option<impl IntoElement> {
    let r = current().filter(|r| r.available.len() > 1)?;
    let next = r.next()?.to_string();
    let speaker = r.current == "speaker";
    let (path, tip) = match r.current.as_str() {
        "speaker" => ("icons/volume-max.svg", t::call_sound_speaker()),
        "bluetooth" => ("ui/call-bluetooth.svg", t::call_sound_bluetooth()),
        "headset" => ("ui/call-headset.svg", t::call_sound_headset()),
        _ => ("ui/call.svg", t::call_sound_earpiece()),
    };
    Some(
        call_btn(id, speaker)
            .tip(tip)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, w, cx| {
                cx.stop_propagation();
                on(next.clone(), w, cx)
            })
            .child(svg().path(path).size(px(18.)).text_color(if speaker { c(ACCENT) } else { white_a(0.88) })),
    )
}

#[cfg(test)]
mod tests {
    use super::Route;

    fn r(current: &str, available: &[&str]) -> Route {
        Route { current: current.into(), available: available.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn next_goes_round_the_list() {
        assert_eq!(r("earpiece", &["earpiece", "speaker", "bluetooth"]).next(), Some("speaker"));
        assert_eq!(r("bluetooth", &["earpiece", "speaker", "bluetooth"]).next(), Some("earpiece"));
        assert_eq!(r("gone", &["earpiece", "speaker"]).next(), Some("earpiece"));
        assert_eq!(r("earpiece", &[]).next(), None);
    }
}
