//! `RailShortcuts`' Communities: the synced arrangement (order and folders) painted over
//! the communities this device holds, dragged to rearrange.

use std::collections::{HashMap, HashSet};

use gpui::prelude::*;
use gpui::{AnyElement, Hsla, SharedString, Window, div, px, svg};
use vector_core::rail_layout::{RailAnchor, RailDrag, RailDrop, RailLayout, RailNode};

use crate::community::{Act, OnAct, logo};
use crate::data::{Community, Store};
use crate::theme::*;

pub enum Row<'a> {
    Item(&'a Community),
    Folder {
        id: String,
        name: String,
        hue: Option<u16>,
        members: Vec<&'a Community>,
    },
}

/// The layout over the live communities. A key it can't resolve is skipped, never dropped;
/// one it never placed goes on top, newest joined first.
pub fn rows<'a>(store: &'a Store, layout: &RailLayout) -> Vec<Row<'a>> {
    let live: Vec<&Community> = store.communities.iter().filter(|c| !c.dissolved).collect();
    let by_key: HashMap<&str, &Community> = live.iter().map(|c| (c.community_id.as_str(), *c)).collect();
    let mut seen = HashSet::new();
    let mut placed = Vec::new();
    for node in &layout.nodes {
        match node {
            RailNode::Item { key } => {
                if let Some(c) = by_key.get(key.as_str())
                    && seen.insert(key.as_str())
                {
                    placed.push(Row::Item(c));
                }
            }
            RailNode::Folder { id, name, hue, keys } => {
                let members: Vec<&Community> = keys
                    .iter()
                    .filter_map(|k| by_key.get(k.as_str()).filter(|_| seen.insert(k.as_str())).copied())
                    .collect();
                if !members.is_empty() {
                    placed.push(Row::Folder {
                        id: id.clone(),
                        name: name.clone(),
                        hue: *hue,
                        members,
                    });
                }
            }
        }
    }
    // Held in join order, oldest first.
    let mut out: Vec<Row> = live
        .iter()
        .rev()
        .filter(|c| !seen.contains(c.community_id.as_str()))
        .map(|c| Row::Item(c))
        .collect();
    out.extend(placed);
    out
}

/// The communities as painted: what a first drag freezes into the layout.
pub fn painted(store: &Store, layout: &RailLayout) -> Vec<String> {
    rows(store, layout)
        .iter()
        .flat_map(|r| match r {
            Row::Item(c) => vec![c.community_id.clone()],
            Row::Folder { members, .. } => members.iter().map(|c| c.community_id.clone()).collect(),
        })
        .collect()
}

/// Folder colours: neutral grey by default, then a spread around the wheel.
pub const HUES: [(&str, u16); 8] = [
    ("Red", 0),
    ("Orange", 28),
    ("Yellow", 50),
    ("Green", 140),
    ("Teal", 175),
    ("Blue", 212),
    ("Purple", 265),
    ("Pink", 320),
];

pub fn tint(hue: Option<u16>) -> Hsla {
    match hue {
        Some(h) => gpui::hsla(h as f32 / 360., 0.65, 0.62, 1.),
        None => c(0x9a9a9a),
    }
}

#[derive(Clone)]
pub struct Drag {
    pub source: RailDrag,
    name: SharedString,
}

struct Ghost(SharedString);

impl Render for Ghost {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .w(px(crate::ui::RAIL_W - 16.))
            .h(px(31.))
            .px(px(8.))
            .flex()
            .items_center()
            .rounded(px(10.))
            .bg(c(0x161616))
            .opacity(0.92)
            .shadow_lg()
            .font_family(RUBIK)
            .text_size(px(14.))
            .text_color(c(0xffffff))
            .child(div().truncate().child(self.0.clone()))
    }
}

/// Where a drop lands in each band of a row: above, onto, below.
struct Zones {
    before: RailDrop,
    into: Option<RailDrop>,
    after: RailDrop,
    /// A row inside a folder: a folder never lands here.
    child: bool,
}

fn anchor(row: &Row) -> RailAnchor {
    match row {
        Row::Item(c) => RailAnchor::Item {
            key: c.community_id.clone(),
        },
        Row::Folder { id, .. } => RailAnchor::Folder { id: id.clone() },
    }
}

fn before(row: Option<&Row>) -> RailDrop {
    match row {
        Some(r) => RailDrop::Before { anchor: anchor(r) },
        None => RailDrop::End,
    }
}

/// The strip's rows. `active` is the open Community; `open` the folders this device holds open.
pub fn render(
    store: &Store,
    layout: &RailLayout,
    open: &HashSet<String>,
    active: Option<&str>,
    on: &OnAct,
) -> AnyElement {
    let rows = rows(store, layout);
    let unread = |c: &Community| {
        c.channels
            .iter()
            .any(|ch| store.chat(&ch.channel_id).is_some_and(|chat| store.unread_count(chat) > 0))
    };
    let mut list = div();
    for (ix, row) in rows.iter().enumerate() {
        let next = before(rows.get(ix + 1));
        match row {
            Row::Item(com) => {
                let zones = Zones {
                    before: before(Some(row)),
                    into: Some(RailDrop::Combine {
                        with_key: com.community_id.clone(),
                    }),
                    after: next,
                    child: false,
                };
                let open_now = active == Some(com.community_id.as_str());
                list = list.child(item(store, com, open_now, !open_now && unread(com), 8., zones, on));
            }
            Row::Folder { id, name, hue, members } => {
                let is_open = open.contains(id);
                let label = if name.is_empty() {
                    members.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")
                } else {
                    name.clone()
                };
                let holds_active = members.iter().any(|c| active == Some(c.community_id.as_str()));
                let any_unread = members.iter().any(|c| active != Some(c.community_id.as_str()) && unread(c));
                let tint = tint(*hue);
                let head_zones = Zones {
                    before: before(Some(row)),
                    into: Some(RailDrop::IntoFolder {
                        folder_id: id.clone(),
                        before_key: is_open.then(|| members[0].community_id.clone()),
                    }),
                    after: if is_open {
                        RailDrop::IntoFolder {
                            folder_id: id.clone(),
                            before_key: Some(members[0].community_id.clone()),
                        }
                    } else {
                        next.clone()
                    },
                    child: false,
                };
                let head = folder_head(
                    store,
                    id,
                    &label,
                    members,
                    tint,
                    is_open,
                    !is_open && holds_active,
                    !is_open && any_unread,
                    head_zones,
                    on,
                );
                let mut band = div()
                    .mx(px(4.))
                    .rounded(px(12.))
                    .when(is_open, |d| d.bg(tint.opacity(0.09)).my(px(4.)).pt(px(4.)).pb(px(4.)))
                    .child(head);
                if is_open {
                    for (i, com) in members.iter().enumerate() {
                        let zones = Zones {
                            before: RailDrop::IntoFolder {
                                folder_id: id.clone(),
                                before_key: Some(com.community_id.clone()),
                            },
                            into: None,
                            after: match members.get(i + 1) {
                                Some(n) => RailDrop::IntoFolder {
                                    folder_id: id.clone(),
                                    before_key: Some(n.community_id.clone()),
                                },
                                // The bottom of the last row is the way back out.
                                None => next.clone(),
                            },
                            child: true,
                        };
                        let open_now = active == Some(com.community_id.as_str());
                        band = band.child(item(store, com, open_now, !open_now && unread(com), 4., zones, on));
                    }
                }
                list = list.child(band);
            }
        }
    }
    list.into_any_element()
}

/// The open bar, or the grey unread one, at the rail's edge.
fn bars(d: gpui::Stateful<gpui::Div>, left: f32, inset: f32, open: bool, unread: bool) -> gpui::Stateful<gpui::Div> {
    d.when(open, |d| {
        d.child(
            div()
                .absolute()
                .left(px(-left))
                .top(px(inset))
                .bottom(px(inset))
                .w(px(4.))
                .rounded_r(px(4.))
                .bg(c(PRIMARY)),
        )
    })
    .when(unread, |d| {
        d.child(
            div()
                .absolute()
                .left(px(-left))
                .top(gpui::relative(0.5))
                .mt(px(-5.))
                .w(px(4.))
                .h(px(10.))
                .rounded_r(px(4.))
                .bg(c(ICON_MUTED)),
        )
    })
}

fn name(text: SharedString) -> impl IntoElement {
    div()
        .flex_1()
        .min_w_0()
        .truncate()
        .font_family(RUBIK)
        .text_size(px(14.))
        .text_color(c(0xffffff))
        .child(text)
}

fn row_shell(id: SharedString, inset: f32) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .relative()
        .mx(px(inset))
        .h(px(31.))
        .rounded(px(10.))
        .flex()
        .items_center()
        .px(px(8.))
        .gap(px(10.))
        .cursor_pointer()
        .hover(|d| d.bg(white_a(0.045)))
}

fn item(store: &Store, com: &Community, open: bool, unread: bool, inset: f32, zones: Zones, on: &OnAct) -> AnyElement {
    let id = com.community_id.clone();
    let source = RailDrag::Item { key: id.clone() };
    let el = bars(
        row_shell(SharedString::from(format!("rail-c-{id}")), inset),
        inset,
        3.,
        open,
        unread,
    )
    .on_click({
        let (on, id) = (on.clone(), id.clone());
        move |_, w, cx| on(Act::Open(id.clone()), w, cx)
    })
    .on_mouse_down(gpui::MouseButton::Right, {
        let (on, id) = (on.clone(), id.clone());
        move |e, w, cx| {
            cx.stop_propagation();
            on(Act::RailMenu(Some(id.clone()), None, e.position), w, cx)
        }
    })
    .child(logo(store, &com.community_id, 26., 4.9))
    .child(name(com.name.clone().into()));
    draggable(el, source, com.name.clone().into(), zones, on)
}

#[allow(clippy::too_many_arguments)]
fn folder_head(
    store: &Store,
    id: &str,
    label: &str,
    members: &[&Community],
    tint: Hsla,
    open: bool,
    active: bool,
    unread: bool,
    zones: Zones,
    on: &OnAct,
) -> AnyElement {
    let tile = div()
        .relative()
        .size(px(26.))
        .flex_shrink_0()
        .rounded(px(4.9))
        .when(!open, |d| {
            d.bg(tint.opacity(0.26)).child(
                div().absolute().inset(px(3.)).flex().flex_wrap().gap(px(2.)).children(
                    members
                        .iter()
                        .take(4)
                        .map(|c| logo(store, &c.community_id, 9., 2.7)),
                ),
            )
        })
        .when(open, |d| {
            d.flex().items_center().justify_center().child(
                svg()
                    .path("icons/folder.svg")
                    .size(px(15.6))
                    .mt(px(-1.))
                    .text_color(tint),
            )
        });
    let fid = id.to_string();
    let el = bars(
        row_shell(SharedString::from(format!("rail-f-{id}")), 4.),
        8.,
        3.,
        active,
        unread,
    )
    .on_click({
        let (on, fid) = (on.clone(), fid.clone());
        move |_, w, cx| on(Act::RailFolder(fid.clone()), w, cx)
    })
    .on_mouse_down(gpui::MouseButton::Right, {
        let (on, fid) = (on.clone(), fid.clone());
        move |e, w, cx| {
            cx.stop_propagation();
            on(Act::RailMenu(None, Some(fid.clone()), e.position), w, cx)
        }
    })
    .child(tile)
    .child(name(label.to_string().into()));
    draggable(el, RailDrag::Folder { id: fid }, label.to_string().into(), zones, on)
}

/// A drag lifts the row; its top and bottom bands drop beside it, its middle drops onto it.
fn draggable(
    el: gpui::Stateful<gpui::Div>,
    source: RailDrag,
    label: SharedString,
    zones: Zones,
    on: &OnAct,
) -> AnyElement {
    let mut el = el.on_drag(
        Drag {
            source: source.clone(),
            name: label,
        },
        |d: &Drag, _, _, cx| cx.new(|_| Ghost(d.name.clone())),
    );
    let child = zones.child;
    for (band, drop) in [("into", zones.into), ("before", Some(zones.before)), ("after", Some(zones.after))] {
        let Some(drop) = drop else { continue };
        let (on, src) = (on.clone(), source.clone());
        let z = div().absolute().left_0().right_0();
        let z = match band {
            "before" => z.top_0().h(gpui::relative(0.3)),
            "after" => z.bottom_0().h(gpui::relative(0.3)),
            _ => z.top_0().bottom_0(),
        };
        let into = band == "into";
        let top = band == "before";
        el = el.child(
            z.drag_over::<Drag>(move |d, _, _, _| {
                if into {
                    d.rounded(px(10.)).bg(c(PRIMARY).opacity(0.16)).border_2().border_color(c(PRIMARY))
                } else if top {
                    d.border_t_2().border_color(c(PRIMARY))
                } else {
                    d.border_b_2().border_color(c(PRIMARY))
                }
            })
            .on_drop(move |d: &Drag, w, cx| {
                let folder = matches!(d.source, RailDrag::Folder { .. });
                // A row let go on itself stays put, and a folder never nests.
                if d.source != src && !(folder && (into || child)) {
                    on(Act::RailDrop(d.source.clone(), drop.clone()), w, cx);
                }
            }),
        );
    }
    el.into_any_element()
}

/// The collapsed rail's Communities: 45px tiles, folders as tiles of their members.
pub fn render_slim(
    store: &Store,
    layout: &RailLayout,
    open: &HashSet<String>,
    active: Option<&str>,
    rail_w: f32,
    on: &OnAct,
) -> AnyElement {
    let rows = rows(store, layout);
    let unread = |c: &Community| {
        c.channels
            .iter()
            .any(|ch| store.chat(&ch.channel_id).is_some_and(|chat| store.unread_count(chat) > 0))
    };
    // The open bar reaches from the rail's edge.
    let edge = (45. - rail_w) / 2.;
    let mut list = div().flex().flex_col().gap(px(GAP)).py(px(6.));
    for (ix, row) in rows.iter().enumerate() {
        let next = before(rows.get(ix + 1));
        match row {
            Row::Item(com) => {
                let zones = Zones {
                    before: before(Some(row)),
                    into: Some(RailDrop::Combine {
                        with_key: com.community_id.clone(),
                    }),
                    after: next,
                    child: false,
                };
                let open_now = active == Some(com.community_id.as_str());
                list = list.child(tile(store, com, open_now, !open_now && unread(com), edge, zones, on));
            }
            Row::Folder { id, name, hue, members } => {
                let is_open = open.contains(id);
                let label = if name.is_empty() {
                    members.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")
                } else {
                    name.clone()
                };
                let holds_active = members.iter().any(|c| active == Some(c.community_id.as_str()));
                let any_unread = members.iter().any(|c| active != Some(c.community_id.as_str()) && unread(c));
                let tint = tint(*hue);
                let zones = Zones {
                    before: before(Some(row)),
                    into: Some(RailDrop::IntoFolder {
                        folder_id: id.clone(),
                        before_key: is_open.then(|| members[0].community_id.clone()),
                    }),
                    after: if is_open {
                        RailDrop::IntoFolder {
                            folder_id: id.clone(),
                            before_key: Some(members[0].community_id.clone()),
                        }
                    } else {
                        next.clone()
                    },
                    child: false,
                };
                let fid = id.clone();
                let tile_el = div()
                    .relative()
                    .size(px(45.))
                    .rounded(px(8.5))
                    .when(!is_open, |d| {
                        d.bg(tint.opacity(0.26)).child(
                            div().absolute().inset(px(6.)).flex().flex_wrap().gap(px(3.)).children(
                                members.iter().take(4).map(|c| logo(store, &c.community_id, 15., 4.5)),
                            ),
                        )
                    })
                    .when(is_open, |d| {
                        d.flex().items_center().justify_center().child(
                            svg().path("icons/folder.svg").size(px(27.)).mt(px(-1.6)).text_color(tint),
                        )
                    });
                let head = bars(
                    ring(SharedString::from(format!("rail-f-{id}"))),
                    -edge - 3.,
                    6.,
                    !is_open && holds_active,
                    !is_open && any_unread,
                )
                .on_click({
                    let (on, fid) = (on.clone(), fid.clone());
                    move |_, w, cx| on(Act::RailFolder(fid.clone()), w, cx)
                })
                .on_mouse_down(gpui::MouseButton::Right, {
                    let (on, fid) = (on.clone(), fid.clone());
                    move |e, w, cx| {
                        cx.stop_propagation();
                        on(Act::RailMenu(None, Some(fid.clone()), e.position), w, cx)
                    }
                })
                .child(tile_el);
                let head = draggable(head, RailDrag::Folder { id: fid }, label.into(), zones, on);
                let mut band = div()
                    .w(px(53.))
                    .mx_auto()
                    .flex_shrink_0()
                    .rounded(px(12.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .when(is_open, |d| d.bg(tint.opacity(0.09)).mt(px(-4.)).pt(px(4.)).pb(px(4.)))
                    .child(head);
                if is_open {
                    let mut inner = div().pt(px(GAP)).flex().flex_col().gap(px(GAP));
                    for (i, com) in members.iter().enumerate() {
                        let zones = Zones {
                            before: RailDrop::IntoFolder {
                                folder_id: id.clone(),
                                before_key: Some(com.community_id.clone()),
                            },
                            into: None,
                            after: match members.get(i + 1) {
                                Some(n) => RailDrop::IntoFolder {
                                    folder_id: id.clone(),
                                    before_key: Some(n.community_id.clone()),
                                },
                                None => next.clone(),
                            },
                            child: true,
                        };
                        let open_now = active == Some(com.community_id.as_str());
                        inner = inner.child(tile(store, com, open_now, !open_now && unread(com), edge, zones, on));
                    }
                    band = band.child(inner);
                }
                list = list.child(band);
            }
        }
    }
    list.into_any_element()
}

/// Rings stand 3px out from their tile, with 2px between neighbours.
const GAP: f32 = 8.;

/// A tile's hover ring, drawn as a plate 3px out from the tile it holds.
fn ring(id: SharedString) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .relative()
        .m(px(-3.))
        .p(px(3.))
        .rounded(px(11.5))
        .mx_auto()
        .flex_shrink_0()
        .cursor_pointer()
        .hover(|d| d.bg(white_a(0.045)))
}

fn tile(store: &Store, com: &Community, open: bool, unread: bool, edge: f32, zones: Zones, on: &OnAct) -> AnyElement {
    let id = com.community_id.clone();
    let el = bars(ring(SharedString::from(format!("rail-c-{id}"))), -edge - 3., 6., open, unread)
        .on_click({
            let (on, id) = (on.clone(), id.clone());
            move |_, w, cx| on(Act::Open(id.clone()), w, cx)
        })
        .on_mouse_down(gpui::MouseButton::Right, {
            let (on, id) = (on.clone(), id.clone());
            move |e, w, cx| {
                cx.stop_propagation();
                on(Act::RailMenu(Some(id.clone()), None, e.position), w, cx)
            }
        })
        .child(logo(store, &com.community_id, 45., 8.5).bg(gpui::rgba(0x090909e6)));
    draggable(el, RailDrag::Item { key: id }, com.name.clone().into(), zones, on)
}
