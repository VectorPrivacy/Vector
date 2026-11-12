//! `.downgrade-block`: a full takeover for a build older than the account's database. There is
//! no way past it: continuing would write an older schema over a newer one.

use gpui::{AnyElement, FontWeight, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div, img, prelude::FluentBuilder, px};

use crate::theme::{INTER, TEXT, c, ca, white_a};
use crate::timeline::DANGER;

/// `v1.2.3`, or `v1.2.3 Preview N` for a `-N` build, as the updater names versions.
pub fn display(raw: &str) -> String {
    let (core, pre) = raw.split_once('-').map_or((raw, None), |(c, p)| (c, Some(p)));
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        return raw.to_string();
    }
    let pre: Option<u32> = pre.and_then(|p| p.bytes().take_while(u8::is_ascii_digit).map(char::from).collect::<String>().parse().ok());
    match pre {
        Some(n) => format!("v{core} Preview {n}"),
        None => format!("v{core}"),
    }
}

/// This build's version and the one the account needs, already displayed.
pub fn versions(block: &vector_core::db::DowngradeBlock) -> (String, String) {
    let required = block.last_app_version.as_deref().map_or_else(|| "A newer version".to_string(), display);
    (display(env!("CARGO_PKG_VERSION")), required)
}

fn version(label: &'static str, value: &str, accent: bool) -> impl IntoElement {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(4.))
        .px(px(8.))
        .py(px(12.))
        .child(div().text_size(px(11.)).line_height(px(24.)).text_color(white_a(0.45 * 0.965)).child(label.to_uppercase()))
        .child(
            div()
                .text_size(px(14.))
                .line_height(px(24.))
                .font_weight(FontWeight::BOLD)
                .text_color(if accent { c(DANGER) } else { c(TEXT) })
                .child(value.to_string()),
        )
}

fn button(id: &'static str, label: &'static str, accept: bool, on: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static) -> impl IntoElement {
    div()
        .id(id)
        .w_full()
        .h(px(41.2))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .border_1()
        .map(|d| if accept { d.border_color(ca(90, 252, 180, 0.65)).bg(ca(89, 252, 179, 0.1)) } else { d.border_color(white_a(0.2)) })
        .text_color(c(0xffffff))
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|d| d.opacity(0.75))
        .on_click(move |_, w, cx| on(w, cx))
        .child(label)
}

pub fn render(current: &str, required: &str) -> AnyElement {
    let dim = |o: f32, size: f32, lh: f32, text: &'static str| {
        div().text_size(px(size)).line_height(px(lh)).text_center().text_color(ca(246, 246, 246, o)).child(text)
    };
    let card = div()
        .w(px(410.))
        .flex()
        .flex_col()
        .items_center()
        .px(px(24.))
        .py(px(28.))
        .rounded(px(12.))
        .bg(c(0x0d0d0d))
        .border_1()
        .border_color(c(DANGER).opacity(0.45))
        .font_family(INTER)
        .child(img("icons/vector_warning.svg").size(px(84.)).mb(px(12.)))
        .child(
            // The inline icon's line box and the heading's own top margin, as the stylesheet lays them.
            div()
                .mt(px(22.6))
                .mb(px(14.))
                .text_size(px(20.))
                .line_height(px(24.))
                .font_weight(FontWeight::BOLD)
                .text_color(c(DANGER))
                .child("Vector can't open this account"),
        )
        .child(dim(0.8, 14., 21., "Your data was last used with a newer version of Vector. Opening it with this older build would damage your message history.").mb(px(20.)))
        .child(
            div()
                .w_full()
                .mb(px(20.))
                .flex()
                .rounded(px(8.))
                .bg(white_a(0.04))
                .child(version("This build", current, false))
                .child(div().w(px(1.)).my(px(10.)).bg(white_a(0.1)))
                .child(version("Your data needs", required, true)),
        )
        .child(dim(0.55, 13., 19.5, "Nothing has been lost. Your messages are intact and will open again on the newer version.").mb(px(20.)))
        .child(button("downgrade-latest", "Get the latest version", true, |_, cx| cx.open_url("https://vectorapp.io")).into_any_element())
        .child(div().h(px(10.)))
        .child(button("downgrade-quit", "Quit Vector", false, |_, cx| cx.quit()));
    div()
        .id("downgrade-block")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .p(px(20.))
        .bg(ca(0, 0, 0, 0.92))
        .occlude()
        .child(card)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions_read_as_the_updater_names_them() {
        assert_eq!(super::display("0.6.0"), "v0.6.0");
        assert_eq!(super::display("0.6.0-2"), "v0.6.0 Preview 2");
        assert_eq!(super::display("nightly"), "nightly");
    }
}
