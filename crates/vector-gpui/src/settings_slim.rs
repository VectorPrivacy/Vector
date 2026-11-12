//! Settings on a phone (`.sm`): the categories as one card of rows, each folding its blocks
//! open beneath it. A search opens every category it matches.

use super::*;

const CARD: u32 = 0x0e0e0e;
const ROW_EDGE: u32 = 0x222222;
const ROW_INK: u32 = 0xcfcfcf;

fn query(s: &Settings, cx: &App) -> String {
    s.search.read(cx).text().trim().to_lowercase()
}

fn hits(cat: &Category, query: &str) -> Vec<bool> {
    cat.anchors
        .iter()
        .map(|a| query.is_empty() || format!("{} {} {}", cat.label, a.label, a.keys).to_lowercase().contains(query))
        .collect()
}

fn heading(cat: &'static str, a: &Anchor, on: &OnAct) -> AnyElement {
    let group = SharedString::from(format!("sm-h-{}", a.id));
    let anchor = a.id;
    div()
        .group(group.clone())
        .mb(px(12.))
        .flex()
        .items_center()
        .text_size(px(18.))
        .line_height(px(22.))
        .font_weight(FontWeight::BOLD)
        .text_color(c(0xffffff))
        .child(a.label)
        .child(
            div()
                .id(SharedString::from(format!("sm-link-{}", a.id)))
                .ml(px(6.))
                .p(px(5.))
                .rounded(px(6.))
                .cursor_pointer()
                .opacity(0.)
                .group_hover(group, |d| d.opacity(0.55))
                .hover(|d| d.opacity(1.))
                .on_click(click(on, move || Act::CopyPlace(cat, anchor)))
                .child(svg().path("icons/share.svg").size(px(14.)).text_color(c(0xffffff))),
        )
        .into_any_element()
}

fn danger_head(s: &Settings) -> AnyElement {
    let gif = if matches!(s.theme.as_str(), "vector" | "cyberpunk") {
        "icons/aggro-glitch.gif"
    } else {
        "icons/aggro-glitch-red.gif"
    };
    div()
        .pt(px(10.))
        .pb(px(2.))
        .flex()
        .flex_col()
        .items_center()
        .child(img(gif).size(px(64.)))
        .child(
            div()
                .mt(px(4.))
                .text_size(px(12.))
                .line_height(px(15.))
                .text_color(c(crate::timeline::DANGER))
                .child("Irreversible Actions"),
        )
        .into_any_element()
}

fn body(cat: &Category, shown: &[bool], s: &Settings, on: &OnAct) -> AnyElement {
    let geo = s.slim_geo.clone();
    let scroll = s.slim_scroll.clone();
    let id = cat.id;
    let probe = canvas(
        move |b, window, _| {
            let mut g = geo.borrow_mut();
            g.bodies.insert(id, f32::from(b.size.height));
            if g.follow != Some(id) {
                return;
            }
            g.follow = None;
            let (Some(row), view) = (g.rows.get(id).copied(), scroll.bounds()) else { return };
            let top = f32::from(row.top());
            let below = f32::from(view.bottom()) - top - f32::from(row.size.height);
            let want = f32::from(b.size.height).min(f32::from(view.size.height) / 2.);
            let by = (want - below).min(top - f32::from(view.top()) - 10.);
            if by > 0. {
                g.glide = Some((std::time::Instant::now(), f32::from(scroll.offset().y), by));
                window.request_animation_frame();
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0();
    let many = cat.anchors.len() > 1;
    let mut page = div().relative().pt(px(2.)).px(px(16.)).pb(px(12.)).flex().flex_col().child(probe);
    if cat.danger {
        page = page.child(danger_head(s));
    }
    let mut first = true;
    for (a, &on_) in cat.anchors.iter().zip(shown) {
        if !on_ {
            continue;
        }
        page = page.child(
            div()
                .pt(px(12.))
                .pb(px(4.))
                .when(!first, |d| d.border_t_1().border_color(c(ROW_EDGE)))
                .when(many, |d| d.child(heading(cat.id, a, on)))
                .children(block(a.id, s, on)),
        );
        first = false;
    }
    page.with_animation(
        SharedString::from(format!("sm-in-{}", cat.id)),
        gpui::Animation::new(std::time::Duration::from_millis(260)).with_easing(crate::kit::bezier(0.22, 1., 0.36, 1.)),
        |d, t| d.opacity(t).mt(px(-4. * (1. - t))),
    )
    .into_any_element()
}

fn row_probe(id: &'static str, s: &Settings) -> impl IntoElement {
    let geo = s.slim_geo.clone();
    canvas(move |b, _, _| { geo.borrow_mut().rows.insert(id, b); }, |_, _, _, _| {}).absolute().inset_0()
}

fn cat_row(cat: &Category, first: bool, open: bool, s: &Settings, on: &OnAct) -> AnyElement {
    let ink = if cat.danger { c(crate::timeline::DANGER) } else { c(ROW_INK) };
    let label = if cat.danger { c(crate::timeline::DANGER) } else { c(0xffffff) };
    let id = cat.id;
    div()
        .id(SharedString::from(format!("sm-row-{id}")))
        .relative()
        .child(row_probe(id, s))
        .min_h(px(54.))
        .px(px(16.))
        .flex()
        .items_center()
        .gap(px(14.))
        .cursor_pointer()
        .when(!first, |d| d.border_t_1().border_color(c(ROW_EDGE)))
        .on_click(click(on, move || Act::Fold(id)))
        .child(svg().path(SharedString::from(format!("icons/{}.svg", if cat.danger { "vector_warning" } else { cat.icon }))).size(px(24.)).flex_shrink_0().text_color(ink))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(17.))
                .line_height(px(22.))
                .text_color(label)
                .child(cat.label),
        )
        .child(
            svg()
                .path(if open { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" })
                .size(px(18.))
                .flex_shrink_0()
                .text_color(c(ROW_INK)),
        )
        .into_any_element()
}

/// `.sm-update`: a waiting update, lifted out of the list into a card of its own.
fn update_row(version: Option<String>, open: bool, s: &Settings, on: &OnAct) -> AnyElement {
    div()
        .id("sm-row-updates")
        .relative()
        .child(row_probe("updates", s))
        .min_h(px(54.))
        .px(px(16.))
        .flex()
        .items_center()
        .gap(px(14.))
        .cursor_pointer()
        .on_click(click(on, || Act::Fold("updates")))
        .child(svg().path("icons/refresh.svg").size(px(24.)).flex_shrink_0().text_color(c(ACCENT)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .text_size(px(17.))
                .line_height(px(22.))
                .text_color(c(ACCENT))
                .child(div().flex_shrink_0().child("Update Pending"))
                .children(version.map(|v| {
                    div().ml(px(8.)).min_w_0().truncate().text_size(px(13.)).text_color(c(0x8a8a8a)).child(format!("– {v}"))
                })),
        )
        .child(
            div()
                .flex_shrink_0()
                .min_w(px(24.))
                .h(px(24.))
                .rounded(px(12.))
                .bg(c(ACCENT))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(0x000000))
                .child("1"),
        )
        .child(
            svg()
                .path(if open { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" })
                .size(px(18.))
                .flex_shrink_0()
                .text_color(c(ROW_INK)),
        )
        .into_any_element()
}

pub fn slim_view(s: &Settings, on: &OnAct, cx: &App) -> AnyElement {
    let cats = categories(&s.offers);
    let q = query(s, cx);
    let mut card = div().rounded(px(10.)).bg(c(CARD)).flex().flex_col();
    let mut first = true;
    let waiting = s.updates.phase == UpdPhase::Available;
    let mut pending = None;
    for cat in &cats {
        let shown = hits(cat, &q);
        if waiting && cat.id == "updates" {
            let open = if q.is_empty() { s.slim_open == Some(cat.id) } else { !s.slim_shut.contains(&cat.id) };
            pending = shown.contains(&true).then(|| {
                let version = s.updates.found.as_ref().map(|f| f.0.clone());
                div()
                    .mb(px(14.))
                    .rounded(px(10.))
                    .bg(c(CARD))
                    .border(px(1.5))
                    .border_color(c(ACCENT))
                    .flex()
                    .flex_col()
                    .child(update_row(version, open, s, on))
                    .when(open, |d| d.child(body(cat, &shown, s, on)))
            });
            continue;
        }
        if !shown.contains(&true) {
            continue;
        }
        let open = if q.is_empty() { s.slim_open == Some(cat.id) } else { !s.slim_shut.contains(&cat.id) };
        card = card.child(cat_row(cat, first, open, s, on)).when(open, |d| d.child(body(cat, &shown, s, on)));
        first = false;
    }
    let empty = first;
    let link = |label: &'static str, key: &'static str| {
        div()
            .id(SharedString::from(format!("sm-foot-{key}")))
            .text_size(px(15.))
            .line_height(px(24.))
            .text_color(c(0x5b6060))
            .hover(|d| d.text_color(c(0xffffff)))
            .cursor_pointer()
            .on_click(click(on, move || Act::Link(key)))
            .child(label)
    };
    let has_query = !q.is_empty();
    let clear = on.clone();
    let search = div()
        .relative()
        .mb(px(14.))
        .h(px(46.))
        .rounded(px(8.))
        .border_1()
        .border_color(c(0x333333))
        .bg(c(CARD))
        .pl(px(45.))
        .pr(px(if has_query { 46. } else { 14. }))
        .flex()
        .items_center()
        .text_size(px(17.))
        .child(div().flex_1().min_w_0().child(s.search.clone()))
        .child(
            svg()
                .path("icons/search.svg")
                .absolute()
                .left(px(13.))
                .top(px(11.))
                .size(px(22.))
                .text_color(c(0x8a8a8a)),
        )
        .when(has_query, |d| {
            d.child(
                div()
                    .id("sm-search-clear")
                    .absolute()
                    .right(px(13.))
                    .top(px(11.))
                    .size(px(22.))
                    .cursor_pointer()
                    .on_click(move |_, w, cx| clear(Act::ClearSearch, w, cx))
                    .child(svg().path("ui/search-clear.svg").size_full().text_color(c(0x8a8a8a))),
            )
        });
    div()
        .id("sm-scroll")
        .size_full()
        .overflow_y_scroll()
        .track_scroll(&s.slim_scroll)
        .on_scroll_wheel({
            let geo = s.slim_geo.clone();
            move |_, _, _| geo.borrow_mut().glide = None
        })
        .bg(c(APP_BG))
        .child(
            div()
                .pt(px(16.))
                .px(px(16.))
                .pb(px(28.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .mt(px(14.))
                        .mb(px(20.))
                        .text_center()
                        .text_size(px(30.))
                        .line_height(px(36.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(c(0xffffff))
                        .child("Settings"),
                )
                .child(search)
                .children(pending)
                .child(card)
                .when(empty, |d| {
                    d.child(
                        div()
                            .mt(px(18.))
                            .text_center()
                            .text_size(px(15.))
                            .text_color(c(0x6f6f6f))
                            .child("No matches"),
                    )
                })
                .child(
                    div()
                        .mt(px(22.))
                        .flex()
                        .flex_wrap()
                        .justify_center()
                        .gap(px(24.))
                        .child(link("Donate", "donate"))
                        .child(link("Docs", "gitbook"))
                        .child(link("Privacy Policy", "privacy")),
                ),
        )
        .into_any_element()
}
