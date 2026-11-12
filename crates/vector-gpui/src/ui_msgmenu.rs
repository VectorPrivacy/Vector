//! `_dmsgOpenMessageMenu`: a message's right-click menu. React, Reply, Pin, Edit, its file,
//! Copy, Mark as Unread, Copy Link and ID, then Delete or a moderator's Hide.

use std::sync::LazyLock;

use gpui::{Context, Pixels, Point, Window};
use regex::Regex;

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
            if unique.len() < 8 {
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
        self.read_anchor = Some(before.clone());
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

/// `stripMarkdownToPlain`: what the message reads as, its markdown dropped.
pub(super) fn strip_markdown(content: &str) -> String {
    static RULES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
        [
            (r"```[^\n]*\n([\s\S]*?)\n?```", "$1"),
            (r"`([^`]+)`", "$1"),
            (r"!\[([^\]]*)\]\([^)]*\)", "$1"),
            (r"\[([^\]]+)\]\([^)]*\)", "$1"),
            (r"\*\*(\S(?:[^*]*\S)?)\*\*", "$1"),
            (r"\*(\S(?:[^*]*\S)?)\*", "$1"),
        ]
        .into_iter()
        .map(|(r, s)| (Regex::new(r).unwrap(), s))
        .collect()
    });
    static UNDER: LazyLock<[Regex; 2]> = LazyLock::new(|| {
        [
            Regex::new(r"(^|[^A-Za-z0-9_])__(\S(?:[^_]*\S)?)__").unwrap(),
            Regex::new(r"(^|[^A-Za-z0-9_])_(\S(?:[^_]*\S)?)_").unwrap(),
        ]
    });
    static TAIL: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        [
            r"~~(\S(?:[^~]*\S)?)~~",
            r"\|\|(\S(?:[^|]*\S)?)\|\|",
            r"(?m)^[ \t]*#{1,6}[ \t]+",
            r"(?m)^[ \t]*>[ \t]?",
            r"(?m)^[ \t]*(?:-(?:[ \t]*-){2,}|\*(?:[ \t]*\*){2,}|_(?:[ \t]*_){2,})[ \t]*$",
        ]
        .into_iter()
        .map(|r| Regex::new(r).unwrap())
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

#[cfg(test)]
mod tests {
    use super::strip_markdown;

    #[test]
    fn strips_like_the_web_client() {
        assert_eq!(strip_markdown("**bold** and _it_ in snake_case_name"), "bold and it in snake_case_name");
        assert_eq!(strip_markdown("# Title\n> quoted\n---\n`code`"), "Title\nquoted\n\ncode");
        assert_eq!(strip_markdown("2 * 3 * 4"), "2 * 3 * 4");
        assert_eq!(strip_markdown("[site](https://x.y) ||secret||"), "site secret");
    }
}
