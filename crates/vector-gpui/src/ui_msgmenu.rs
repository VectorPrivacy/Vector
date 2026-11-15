//! `_dmsgOpenMessageMenu`: a message's right-click menu. React, Reply, Pin, Edit, its file,
//! Copy, Mark as Unread, Copy Link and ID, then Delete or a moderator's Hide.


use gpui::{Context, Pixels, Point, Window};
use crate::scan::{lit, matches, next, replace_all};

use super::{MenuAct, PopupAct, VectorApp};
use crate::{backend, kit, timeline};

#[derive(Clone)]
pub(super) enum MsgAct {
    Act(timeline::Act),
    Pin(bool),
    Copy(String),
    Unread,
    Link,
    File(String),
    CopyFile(String),
    Delete { limited: bool, attachments: bool },
    Hide,
}

impl VectorApp {
    pub(super) fn message_menu(&mut self, id: String, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(m) = self.message(&id) else { return };
        let act = |a: MsgAct| MenuAct::Msg(id.clone(), a);
        if m.mine && m.failed {
            return self.show_menu(
                at,
                vec![
                    (kit::MenuItem::new("Retry send", "refresh"), act(MsgAct::Act(timeline::Act::Retry))),
                    (kit::MenuItem::new("Delete", "trash").danger(), act(MsgAct::Act(timeline::Act::Delete))),
                ],
                cx,
            );
        }
        if m.pending {
            if !m.attachments.is_empty() {
                let cancel = MsgAct::Act(timeline::Act::CancelUpload(id.clone()));
                self.show_menu(at, vec![(kit::MenuItem::new("Cancel upload", "x").danger(), act(cancel))], cx);
            }
            return;
        }
        let Some(chat) = self.open.clone() else { return };
        let (community, dissolved) = {
            let store = self.store.borrow();
            store.community_of(&chat).map_or((None, false), |c| (Some(c.community_id.clone()), c.dissolved))
        };
        let mut rows: Vec<(kit::MenuItem, MenuAct)> = Vec::new();
        if !dissolved {
            let unique: std::collections::HashSet<&str> = m.reactions.iter().map(|r| r.emoji.as_str()).collect();
            if unique.len() < 8 && self.store.borrow().can_react(&chat) {
                rows.push((kit::MenuItem::new("React", "smile-face"), act(MsgAct::Act(timeline::Act::React))));
            }
            rows.push((kit::MenuItem::new("Reply", "reply"), act(MsgAct::Act(timeline::Act::Reply))));
            if self.pins.channel == chat.as_ref() && self.pins.can_pin && !self.pins.sealed {
                rows.push(if self.pins.pinned(&id) {
                    (kit::MenuItem::new("Unpin", "x"), act(MsgAct::Pin(false)))
                } else {
                    (kit::MenuItem::new("Pin", "pin"), act(MsgAct::Pin(true)))
                });
            }
            let commands = self.command_names();
            if m.mine && !m.content.is_empty() && m.attachments.is_empty() && crate::data::command(&m, commands.as_deref()).is_none() {
                rows.push((kit::MenuItem::new("Edit", "edit"), act(MsgAct::Act(timeline::Act::Edit))));
            }
        }
        if let Some(a) = m.attachments.iter().find(|a| a.downloaded && (a.resident || !a.path.is_empty())) {
            rows.push(if a.resident {
                (kit::MenuItem::new("Save", "download"), act(MsgAct::File(a.id.clone())))
            } else {
                (kit::MenuItem::new("Reveal in folder", "file-search"), act(MsgAct::File(a.id.clone())))
            });
            rows.push((kit::MenuItem::new("Copy", "copy"), act(MsgAct::CopyFile(a.id.clone()))));
        }
        if !m.content.is_empty() {
            let plain = strip_markdown(&m.content);
            if plain == m.content.trim() {
                rows.push((kit::MenuItem::new("Copy", "copy"), act(MsgAct::Copy(m.content.clone()))));
            } else {
                rows.push((kit::MenuItem::new("Copy", "copy"), act(MsgAct::Copy(plain))));
                rows.push((
                    kit::MenuItem::new("Copy", "file-code").hint(Some("(markdown)".into())),
                    act(MsgAct::Copy(m.content.clone())),
                ));
            }
        }
        let later_from_them = {
            let store = self.store.borrow();
            store.chat(&chat).is_some_and(|c| {
                c.messages
                    .iter()
                    .position(|x| x.id == id)
                    .is_some_and(|at| c.messages[at..].iter().any(|x| !x.mine))
            })
        };
        if later_from_them {
            rows.push((kit::MenuItem::new("Mark as Unread", "eye-off"), act(MsgAct::Unread)));
        }
        rows.push((kit::MenuItem::new("Copy Link", "share"), act(MsgAct::Link)));
        if self.advanced() && !self.store.borrow().stream_hidden(&id) {
            rows.push((kit::MenuItem::new("Copy Message ID", "channel-hash"), MenuAct::CopyId(id.clone(), "Message")));
        }
        if !self.live {
            if m.mine || community.is_none() {
                rows.push((kit::MenuItem::divider(), MenuAct::Nothing));
                rows.push((kit::MenuItem::new("Delete", "trash").danger(), act(MsgAct::Delete { limited: false, attachments: false })));
            }
            return self.show_menu(at, rows, cx);
        }
        let rx = backend::run(backend::actions::delete_options(id.clone()));
        cx.spawn(async move |this, cx| {
            let opts = rx.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                if this.open.as_ref() != Some(&chat) {
                    return;
                }
                let act = |a: MsgAct| MenuAct::Msg(id.clone(), a);
                let delete = if opts.mine {
                    let label = if opts.limited { "Delete (limited)" } else { "Delete" };
                    Some((label, act(MsgAct::Delete { limited: opts.limited, attachments: opts.attachments })))
                } else if community.is_none() {
                    Some(("Delete", act(MsgAct::Delete { limited: false, attachments: false })))
                } else if opts.hide && !dissolved {
                    Some(("Hide", act(MsgAct::Hide)))
                } else {
                    None
                };
                if let Some((label, a)) = delete {
                    rows.push((kit::MenuItem::divider(), MenuAct::Nothing));
                    rows.push((kit::MenuItem::new(label, "trash").danger(), a));
                }
                this.show_menu(at, rows, cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn message_menu_act(&mut self, id: String, act: MsgAct, window: &mut Window, cx: &mut Context<Self>) {
        match act {
            MsgAct::Act(a) => self.on_act(id, a, window, cx),
            MsgAct::Pin(on) => self.set_pinned(id, on, cx),
            MsgAct::Copy(text) => cx.write_to_clipboard(gpui::ClipboardItem::new_string(text)),
            MsgAct::Unread => self.mark_unread_from(id, cx),
            MsgAct::Link => {
                let Some(chat) = self.open.clone() else { return };
                let community = self.store.borrow().community_of(&chat).map(|c| c.community_id.clone());
                let link = match community {
                    Some(c) => format!("https://vectorapp.io/go#c/{c}/{chat}/{id}"),
                    None => format!("https://vectorapp.io/go#m/{id}"),
                };
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(link));
                self.show_toast("Copied Message Link", cx);
            }
            MsgAct::File(att) => self.reveal_attachment(id, att, cx),
            MsgAct::CopyFile(att) => self.copy_attachment(id, att, cx),
            MsgAct::Delete { limited, attachments } => {
                let mut p = kit::Popup::notice(if limited { "Limited Delete" } else { "Delete this message?" });
                p.icon = Some("icons/vector_warning.svg");
                p.text = Some(
                    match (limited, attachments) {
                        (false, _) => "Are you sure you want to delete this message?",
                        (true, true) => "This message can't be fully removed from relays because the signing key isn't on this device (sent from another device, or predates deletion support).<br><br>We can still:<br><b>Delete the attached file</b> from the storage server.<br><b>Notify other Vector users</b> to drop the message.<br><br>Continue?",
                        (true, false) => "This message can't be fully removed from relays because the signing key isn't on this device (sent from another device, or predates deletion support).<br><br>We can still <b>notify other Vector users</b> to drop the message from their copy.<br><br>Continue?",
                    }
                    .into(),
                );
                p.confirm = "Confirm".into();
                p.cancel = Some("Cancel".into());
                self.popup = Some((p, PopupAct::Delete { id, failed: false }));
            }
            MsgAct::Hide => {
                let mut p = kit::Popup::notice("Hide this message?");
                p.icon = Some("icons/vector_warning.svg");
                p.text = Some("Permanently hide this message for everyone in the community. This can't be undone.".into());
                p.confirm = "Confirm".into();
                p.cancel = Some("Cancel".into());
                self.popup = Some((p, PopupAct::Hide(id)));
            }
        }
        cx.notify();
    }

    pub(super) fn hide_message(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(chat) = self.open.clone().filter(|_| self.live) else { return };
        let rx = backend::run(backend::actions::hide(chat.to_string(), id));
        cx.spawn(async move |this, cx| {
            if let Ok(Err(e)) = rx.await {
                this.update(cx, |this, cx| {
                    let mut p = kit::Popup::notice("Hide Failed");
                    p.icon = Some("icons/vector_warning.svg");
                    p.text = Some(kit::escape(&e).into());
                    this.popup = Some((p, PopupAct::Notice));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// `markUnreadFrom`: the divider lands above this message and the chat counts it again.
    fn mark_unread_from(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(chat) = self.open.clone() else { return };
        let before = {
            let store = self.store.borrow();
            store.chat(&chat).and_then(|c| {
                let at = c.messages.iter().position(|m| m.id == id)?;
                Some(if at == 0 { String::new() } else { c.messages[at - 1].id.clone() })
            })
        };
        let Some(before) = before else { return };
        self.read_anchor = Some(crate::data::Unread::At(id.clone()));
        if let Some(c) = self.store.borrow_mut().chat_mut(&chat) {
            c.unread_from = id.clone();
            if !self.live {
                c.last_read = before;
            }
        }
        self.refresh_rows(false);
        if !self.live {
            return;
        }
        let rx = backend::run(backend::chatlist::mark_unread_from(chat.to_string(), id));
        cx.spawn(async move |this, cx| {
            let _ = rx.await;
            this.update(cx, |this, cx| this.refresh_unread(cx)).ok();
        })
        .detach();
    }

    /// A downloaded file's place on disk, or a stored one saved where the user picks.
    fn copy_attachment(&mut self, msg: String, att: String, cx: &mut Context<Self>) {
        let Some(a) = self.message(&msg).and_then(|m| m.attachments.into_iter().find(|a| a.id == att)) else { return };
        let path = if a.path.is_empty() {
            String::new()
        } else {
            let p = std::path::PathBuf::from(&a.path);
            let p = if p.is_absolute() { p } else { self.store.borrow().media_root.join(p) };
            std::path::absolute(&p).unwrap_or(p).to_string_lossy().into_owned()
        };
        if !path.is_empty() && !vector_core::vcache::is_sealed(std::path::Path::new(&path)) {
            match crate::fileclip::write(vec![path.into()], cx) {
                Ok(()) => self.show_toast("Copied file to clipboard", cx),
                Err(e) => self.show_toast(&e, cx),
            }
            return;
        }
        let hash = vector_core::simd::hex::hex_to_bytes_32_checked(&a.id);
        let rx = crate::backend::run(crate::fileclip::materialize(hash, path, a.name));
        cx.spawn(async move |this, cx| {
            let Ok(got) = rx.await else { return };
            this.update(cx, |this, cx| {
                match got.and_then(|p| crate::fileclip::write(vec![p], cx)) {
                    Ok(()) => this.show_toast("Copied file to clipboard", cx),
                    Err(e) => this.show_toast(&e, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn reveal_attachment(&mut self, msg: String, att: String, cx: &mut Context<Self>) {
        let Some(a) = self.message(&msg).and_then(|m| m.attachments.into_iter().find(|a| a.id == att)) else { return };
        if a.resident {
            let name = if a.name.is_empty() { format!("{}.{}", &a.id[..16.min(a.id.len())], a.extension) } else { a.name };
            return self.save_resident(a.id, name, cx);
        }
        let path = std::path::PathBuf::from(&a.path);
        let path = if path.is_absolute() { path } else { self.store.borrow().media_root.join(path) };
        cx.reveal_path(&path);
    }
}

type Spot = (usize, usize);

/// `` ```[^\n]*\n([\s\S]*?)\n?``` ``
fn fence_at(t: &str, p: usize) -> Option<(usize, Spot)> {
    let info = lit(t, p, "```")?;
    let s = info + memchr::memchr(b'\n', &t.as_bytes()[info..])? + 1;
    let j = s + memchr::memmem::find(&t.as_bytes()[s..], b"```")?;
    let body_end = if j > s && t.as_bytes()[j - 1] == b'\n' { j - 1 } else { j };
    Some((j + 3, (s, body_end)))
}

/// `` `([^`]+)` ``
fn code_at(t: &str, p: usize) -> Option<(usize, Spot)> {
    let s = lit(t, p, "`")?;
    let e = s + memchr::memchr(b'`', &t.as_bytes()[s..])?;
    (e > s).then_some((e + 1, (s, e)))
}

/// `\[([^\]]{min,})\]\([^)]*\)`
fn link_at(t: &str, p: usize, min: usize) -> Option<(usize, Spot)> {
    let s = lit(t, p, "[")?;
    let e = s + memchr::memchr(b']', &t.as_bytes()[s..])?;
    let open = lit(t, e + 1, "(").filter(|_| e - s >= min)?;
    let shut = open + memchr::memchr(b')', &t.as_bytes()[open..])?;
    Some((shut + 1, (s, e)))
}

/// `{d}(\S(?:[^{x}]*\S)?){d}` where `x` is the delimiter's char.
fn wrapped_at(t: &str, p: usize, d: &str) -> Option<(usize, Spot)> {
    let a = lit(t, p, d)?;
    let first = next(t, a).filter(|c| !c.is_whitespace())?;
    let a1 = a + first.len_utf8();
    let x = d.as_bytes()[0];
    let r = memchr::memchr(x, &t.as_bytes()[a1..]).map_or(t.len(), |i| a1 + i);
    let upto = next(t, r).map_or(r, |c| r + c.len_utf8());
    // The longest inner run that ends on a non-space just before the closing mark.
    let long = t[a1..upto].char_indices().rev().find_map(|(i, c)| {
        let x = a1 + i + c.len_utf8();
        (!c.is_whitespace()).then(|| lit(t, x, d)).flatten().map(|e| (e, (a, x)))
    });
    long.or_else(|| lit(t, a1, d).map(|e| (e, (a, a1))))
}

/// `(^|[^A-Za-z0-9_]){mark}(\S(?:[^_]*\S)?){mark}`: what came before, and the inner text.
fn under_at(t: &str, p: usize, mark: &str) -> Option<(usize, (Spot, Spot))> {
    let start = (p == 0).then(|| wrapped_at(t, p, mark).map(|(e, inner)| (e, ((p, p), inner)))).flatten();
    start.or_else(|| {
        let c = next(t, p).filter(|c| !(c.is_ascii_alphanumeric() || *c == '_'))?;
        let q = p + c.len_utf8();
        wrapped_at(t, q, mark).map(|(e, inner)| (e, ((p, q), inner)))
    })
}

fn line_start(t: &str, p: usize) -> bool {
    p == 0 || t.as_bytes().get(p - 1) == Some(&b'\n')
}

fn blanks(t: &str, p: usize) -> usize {
    p + t.as_bytes()[p..].iter().take_while(|&&b| b == b' ' || b == b'\t').count()
}

/// `(?m)^[ \t]*#{1,6}[ \t]+`
fn heading_at(t: &str, p: usize) -> Option<(usize, ())> {
    if !line_start(t, p) {
        return None;
    }
    let h = blanks(t, p);
    let n = t.as_bytes()[h..].iter().take_while(|&&b| b == b'#').count();
    let e = blanks(t, h + n);
    ((1..=6).contains(&n) && e > h + n).then_some((e, ()))
}

/// `(?m)^[ \t]*>[ \t]?`
fn quote_at(t: &str, p: usize) -> Option<(usize, ())> {
    if !line_start(t, p) {
        return None;
    }
    let q = lit(t, blanks(t, p), ">")?;
    Some((q + matches!(t.as_bytes().get(q), Some(b' ' | b'\t')) as usize, ()))
}

/// `(?m)^[ \t]*(?:-(?:[ \t]*-){2,}|\*(?:[ \t]*\*){2,}|_(?:[ \t]*_){2,})[ \t]*$`
fn rule_at(t: &str, p: usize) -> Option<(usize, ())> {
    if !line_start(t, p) {
        return None;
    }
    let end = memchr::memchr(b'\n', &t.as_bytes()[p..]).map_or(t.len(), |i| p + i);
    let line = &t.as_bytes()[blanks(t, p).min(end)..end];
    let &c = line.first().filter(|c| matches!(c, b'-' | b'*' | b'_'))?;
    let ok = line.iter().all(|&b| b == c || b == b' ' || b == b'\t') && line.iter().filter(|&&b| b == c).count() >= 3;
    ok.then_some((end, ()))
}

/// `stripMarkdownToPlain`: what the message reads as, its markdown dropped.
pub(super) fn strip_markdown(content: &str) -> String {
    let keep = |out: &mut String, t: &str, (a, b): Spot| *out += &t[a..b];
    let mut t = replace_all(content, fence_at, keep);
    t = replace_all(&t, code_at, keep);
    t = replace_all(&t, |t, p| lit(t, p, "!").and_then(|q| link_at(t, q, 0)), keep);
    t = replace_all(&t, |t, p| link_at(t, p, 1), keep);
    t = replace_all(&t, |t, p| wrapped_at(t, p, "**"), keep);
    t = replace_all(&t, |t, p| wrapped_at(t, p, "*"), keep);
    // Underscore emphasis is never intraword, so snake_case survives.
    for mark in ["__", "_"] {
        let mut out = String::with_capacity(t.len());
        let mut last = 0;
        for (s, e, (lead, inner)) in matches(&t, |t, p| under_at(t, p, mark)) {
            if next(&t, e).is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
                continue;
            }
            out += &t[last..s];
            out += &t[lead.0..lead.1];
            out += &t[inner.0..inner.1];
            last = e;
        }
        out += &t[last..];
        t = out;
    }
    t = replace_all(&t, |t, p| wrapped_at(t, p, "~~"), keep);
    t = replace_all(&t, |t, p| wrapped_at(t, p, "||"), keep);
    for at in [heading_at, quote_at, rule_at] {
        t = replace_all(&t, at, |_, _, ()| {});
    }
    vector_core::notify::strip_ansi(&t).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::strip_markdown;

    fn regex_strip(content: &str) -> String {
        static RULES: std::sync::LazyLock<Vec<(regex::Regex, &'static str)>> = std::sync::LazyLock::new(|| {
            [
                (r"```[^\n]*\n([\s\S]*?)\n?```", "$1"),
                (r"`([^`]+)`", "$1"),
                (r"!\[([^\]]*)\]\([^)]*\)", "$1"),
                (r"\[([^\]]+)\]\([^)]*\)", "$1"),
                (r"\*\*(\S(?:[^*]*\S)?)\*\*", "$1"),
                (r"\*(\S(?:[^*]*\S)?)\*", "$1"),
            ]
            .into_iter()
            .map(|(r, s)| (regex::Regex::new(r).unwrap(), s))
            .collect()
        });
        static UNDER: std::sync::LazyLock<[regex::Regex; 2]> = std::sync::LazyLock::new(|| {
            [
                regex::Regex::new(r"(^|[^A-Za-z0-9_])__(\S(?:[^_]*\S)?)__").unwrap(),
                regex::Regex::new(r"(^|[^A-Za-z0-9_])_(\S(?:[^_]*\S)?)_").unwrap(),
            ]
        });
        static TAIL: std::sync::LazyLock<Vec<regex::Regex>> = std::sync::LazyLock::new(|| {
            [
                r"~~(\S(?:[^~]*\S)?)~~",
                r"\|\|(\S(?:[^|]*\S)?)\|\|",
                r"(?m)^[ \t]*#{1,6}[ \t]+",
                r"(?m)^[ \t]*>[ \t]?",
                r"(?m)^[ \t]*(?:-(?:[ \t]*-){2,}|\*(?:[ \t]*\*){2,}|_(?:[ \t]*_){2,})[ \t]*$",
            ]
            .into_iter()
            .map(|r| regex::Regex::new(r).unwrap())
            .collect()
        });
        let mut t = content.to_string();
        for (re, with) in RULES.iter() {
            t = re.replace_all(&t, *with).into_owned();
        }
        // Underscore emphasis is never intraword, so snake_case survives.
        for re in UNDER.iter() {
            let mut out = String::with_capacity(t.len());
            let mut last = 0;
            for c in re.captures_iter(&t) {
                let whole = c.get(0).unwrap();
                if t[whole.end()..].chars().next().is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
                    continue;
                }
                out.push_str(&t[last..whole.start()]);
                out.push_str(&c[1]);
                out.push_str(&c[2]);
                last = whole.end();
            }
            out.push_str(&t[last..]);
            t = out;
        }
        for (i, re) in TAIL.iter().enumerate() {
            t = re.replace_all(&t, if i < 2 { "$1" } else { "" }).into_owned();
        }
        vector_core::notify::strip_ansi(&t).trim().to_string()
    }


    #[test]
    fn scanners_strip_as_the_regexes_did() {
        const PARTS: &[&str] = &[
            "```", "```rust\n", "\n", "`", "``", "*", "**", "_", "__", "~~", "||", "[", "]", "(", ")", "!", "#", "##", " ", "\t",
            ">", "-", "a", "bc", "x_y", "é", "\u{3000}", "1", ".", "\x1b[31m", "---", "* * *",
        ];
        let mut seed = 0x0dd_ba11u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..40_000 {
            let s: String = (0..1 + next() % 16).map(|_| PARTS[(next() % PARTS.len() as u64) as usize]).collect();
            assert_eq!(strip_markdown(&s), regex_strip(&s), "{s:?}");
        }
    }

    #[test]
    fn strips_like_the_web_client() {
        assert_eq!(strip_markdown("**bold** and _it_ in snake_case_name"), "bold and it in snake_case_name");
        assert_eq!(strip_markdown("# Title\n> quoted\n---\n`code`"), "Title\nquoted\n\ncode");
        assert_eq!(strip_markdown("2 * 3 * 4"), "2 * 3 * 4");
        assert_eq!(strip_markdown("[site](https://x.y) ||secret||"), "site secret");
    }
}
