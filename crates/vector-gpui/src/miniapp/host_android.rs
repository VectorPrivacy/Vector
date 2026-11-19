//! Mini Apps on Android open nowhere yet: every open answers so, and the rest has no window to
//! reach.

use gpui::App;

use super::{Event, PlayerSpec, Spec};

pub fn start(_events: impl Fn(Event) + Send + Sync + 'static) {}

pub fn attach(_cx: &mut App) {}

pub fn player(_spec: PlayerSpec) -> Result<(), String> {
    Err(crate::t::miniapp_unsupported().to_string())
}

pub fn open(_spec: Spec) -> Result<(), String> {
    Err(crate::t::miniapp_unsupported().to_string())
}

pub fn focus(_label: &str, _navigate: Option<String>) {}

pub fn eval(_label: &str, _js: String) {}

pub fn destroy(_label: &str) {}

pub fn close(_label: &str) {}
