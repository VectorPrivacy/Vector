//! The Audit Log in words (`lib/audit-text.js`): each entry as a sentence of plain and bold
//! parts, the details it expands to, and the family the filter groups it under.

use serde_json::Value;

use crate::kit::Choice;

pub const FAMILIES: &[Choice] = &[
    Choice {
        value: "all",
        label: "All Actions",
    },
    Choice {
        value: "community",
        label: "Community",
    },
    Choice {
        value: "roles",
        label: "Roles",
    },
    Choice {
        value: "members",
        label: "Members",
    },
    Choice {
        value: "channels",
        label: "Channels",
    },
    Choice {
        value: "messages",
        label: "Messages",
    },
    Choice {
        value: "voice",
        label: "Voice",
    },
];

pub fn family_of(action: &str) -> &'static str {
    if action.starts_with("voice_") {
        "voice"
    } else if action.starts_with("message_") || action == "pins_update" {
        "messages"
    } else if action.starts_with("member_") || action == "banlist_update" {
        "members"
    } else if action.starts_with("role") || action == "everyone_update" {
        "roles"
    } else if action.starts_with("channel_")
        || action.starts_with("category_")
        || action.starts_with("overwrite_")
    {
        "channels"
    } else {
        "community"
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tone {
    Add,
    Remove,
    Change,
}

pub fn tone_of(action: &str) -> Tone {
    let ends = |s: &str| action.ends_with(s);
    if ends("_create")
        || ends("_pin")
        || matches!(action, "invite_create" | "member_unban" | "roles_activate")
    {
        Tone::Add
    } else if ends("_delete")
        || ends("_unpin")
        || matches!(
            action,
            "member_ban"
                | "member_kick"
                | "member_timeout"
                | "voice_disconnect"
                | "voice_mute"
                | "voice_deafen"
        )
    {
        Tone::Remove
    } else {
        Tone::Change
    }
}

const PERMS: &[(u32, &str)] = &[
    (0, "Manage Roles"),
    (1, "Manage Channels"),
    (2, "Manage Community"),
    (3, "Kick Members"),
    (4, "Ban Members"),
    (5, "Manage Messages"),
    (6, "Create Invites"),
    (8, "View Audit Log"),
    (9, "Mention @everyone"),
    (11, "Pin Messages"),
    (13, "View Channels"),
    (14, "Send Messages"),
    (15, "Send in Threads"),
    (16, "Attach Files"),
    (17, "Add Reactions"),
    (18, "Read Message History"),
    (19, "Connect"),
    (20, "Speak"),
    (21, "Video"),
    (22, "Priority Speaker"),
    (23, "Mute Members"),
    (24, "Deafen Members"),
    (25, "Move Members"),
    (26, "Timeout Members"),
    (28, "Manage Nicknames"),
    (29, "Change Nickname"),
];

pub fn permission_name(bit: u64) -> String {
    PERMS
        .iter()
        .find(|(b, _)| u64::from(*b) == bit)
        .map(|(_, n)| n.to_string())
        .unwrap_or_else(|| format!("Permission {bit}"))
}

fn perms(list: &Value) -> String {
    list.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_u64)
                .map(permission_name)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn field<'a>(entry: &'a Value, name: &str) -> Option<&'a Value> {
    arr(entry, "changes")
        .iter()
        .find(|c| s(c, "field") == Some(name))
}

fn target_name(t: Option<&Value>, member: &dyn Fn(Option<&str>) -> String) -> String {
    let Some(t) = t.filter(|t| !t.is_null()) else {
        return String::new();
    };
    let name = s(t, "name").filter(|n| !n.is_empty());
    match s(t, "kind").unwrap_or("") {
        "everyone" => "@everyone".into(),
        "role" => name
            .map(|n| format!("@{n}"))
            .unwrap_or_else(|| "a deleted role".into()),
        "member" => member(s(t, "npub")),
        "channel" => match name {
            Some(n) if s(t, "view") == Some("voice") => n.into(),
            Some(n) => format!("#{n}"),
            None => "a private channel".into(),
        },
        "category" => name.unwrap_or("a category").into(),
        _ => String::new(),
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        one.into()
    } else {
        format!("{n} {many}")
    }
}

/// `entry` as `(text, bold)` parts; `member` names an npub (or none) as the log shows them.
pub fn sentence(entry: &Value, member: &dyn Fn(Option<&str>) -> String) -> Vec<(String, bool)> {
    let actor = match s(entry, "actor") {
        Some(a) => member(Some(a)),
        None => "Someone".into(),
    };
    let t = entry.get("target");
    let name = |x: Option<&Value>| (target_name(x, member), true);
    let plain = |x: &str| (x.to_string(), false);
    let new_of = |f: &str| field(entry, f).and_then(|c| c.get("new"));
    let sub = |key: &str| t.and_then(|t| t.get(key));
    let author = || (member(sub("author").and_then(Value::as_str)), true);
    let count = || {
        new_of("count")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0)
            .unwrap_or(1)
    };
    let action = s(entry, "action").unwrap_or("");
    let rest: Vec<(String, bool)> = match action {
        "community_create" => vec![plain(" created the community")],
        "community_update" => vec![plain(" updated the community")],
        "roles_activate" => vec![plain(" switched the community to its new roles")],
        "everyone_update" => vec![plain(" changed what "), name(t), plain(" can do")],
        "role_create" => vec![plain(" created the role "), name(t)],
        "role_update" => vec![plain(" updated the role "), name(t)],
        "role_delete" => vec![plain(" deleted the role "), name(t)],
        "member_roles_update" => {
            let c = field(entry, "roles");
            let added = c.map(|c| arr(c, "added")).unwrap_or(&[]);
            let removed = c.map(|c| arr(c, "removed")).unwrap_or(&[]);
            if added.len() == 1 && removed.is_empty() {
                vec![
                    plain(" gave "),
                    name(t),
                    plain(" the role "),
                    name(added.first()),
                ]
            } else if removed.len() == 1 && added.is_empty() {
                vec![
                    plain(" took "),
                    name(removed.first()),
                    plain(" from "),
                    name(t),
                ]
            } else {
                vec![plain(" changed the roles of "), name(t)]
            }
        }
        "member_ban" => vec![plain(" banned "), name(t)],
        "member_unban" => vec![plain(" unbanned "), name(t)],
        "banlist_update" => vec![plain(" updated the ban list")],
        "member_kick" => vec![plain(" kicked "), name(t)],
        "member_timeout" => {
            let until = new_of("until")
                .and_then(Value::as_u64)
                .map(|ms| format!(" until {}", when_full(ms)))
                .unwrap_or_default();
            vec![plain(" timed out "), name(t), plain(&until)]
        }
        "member_timeout_remove" => vec![plain(" lifted the timeout on "), name(t)],
        "member_nick_update" => {
            let nick = new_of("nick")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty());
            let own = t
                .and_then(|t| s(t, "npub"))
                .is_some_and(|n| Some(n) == s(entry, "actor"));
            let mut whose = if own {
                vec![plain(" their nickname")]
            } else {
                vec![plain(" the nickname of "), name(t)]
            };
            match nick {
                Some(n) => {
                    let mut v = vec![plain(" set")];
                    v.append(&mut whose);
                    v.extend([plain(" to "), (n.to_string(), true)]);
                    v
                }
                None => {
                    let mut v = vec![plain(" removed")];
                    v.append(&mut whose);
                    v
                }
            }
        }
        "channel_create" => {
            let voice = new_of("view").and_then(Value::as_str) == Some("voice");
            let private = new_of("private").and_then(Value::as_bool).unwrap_or(false);
            vec![
                plain(&format!(
                    " created the {}{}channel ",
                    if private { "private " } else { "" },
                    if voice { "voice " } else { "" }
                )),
                name(t),
            ]
        }
        "channel_update" => vec![plain(" updated "), name(t)],
        "channel_delete" => vec![plain(" deleted the channel "), name(t)],
        "category_create" => vec![plain(" created the category "), name(t)],
        "category_update" => vec![plain(" updated the category "), name(t)],
        "category_delete" => vec![plain(" deleted the category "), name(t)],
        "overwrite_create" => vec![
            plain(" set permissions for "),
            name(sub("for")),
            plain(" in "),
            name(sub("in")),
        ],
        "overwrite_update" => vec![
            plain(" changed permissions for "),
            name(sub("for")),
            plain(" in "),
            name(sub("in")),
        ],
        "overwrite_delete" => vec![
            plain(" cleared the permissions for "),
            name(sub("for")),
            plain(" in "),
            name(sub("in")),
        ],
        "channel_sync" => vec![
            plain(" synced "),
            name(t),
            plain(" to "),
            name(new_of("category")),
        ],
        "channel_unsync" => vec![plain(" unsynced "), name(t), plain(" from its category")],
        "message_pin" => vec![
            plain(" pinned a message by "),
            author(),
            plain(" in "),
            name(sub("channel")),
        ],
        "message_unpin" => vec![
            plain(" unpinned a message by "),
            author(),
            plain(" in "),
            name(sub("channel")),
        ],
        "pins_update" => vec![plain(" changed the pins in "), name(t)],
        "message_delete" => vec![
            plain(" deleted a message by "),
            author(),
            plain(" in "),
            name(sub("channel")),
        ],
        "invite_create" => vec![plain(&format!(
            " created {}",
            plural(count(), "an invite link", "invite links")
        ))],
        "invite_delete" => vec![plain(&format!(
            " revoked {}",
            plural(count(), "an invite link", "invite links")
        ))],
        "keys_rotate" if field(entry, "severed").is_some() => {
            vec![plain(" locked the community down and rotated its keys")]
        }
        "keys_rotate" => vec![plain(" rotated the community's keys")],
        "voice_mute" => vec![
            plain(" muted "),
            name(t),
            plain(" in "),
            name(new_of("channel")),
        ],
        "voice_unmute" => vec![
            plain(" unmuted "),
            name(t),
            plain(" in "),
            name(new_of("channel")),
        ],
        "voice_deafen" => vec![
            plain(" deafened "),
            name(t),
            plain(" in "),
            name(new_of("channel")),
        ],
        "voice_undeafen" => vec![
            plain(" undeafened "),
            name(t),
            plain(" in "),
            name(new_of("channel")),
        ],
        "voice_move" => vec![plain(" moved "), name(t), plain(" to "), name(new_of("to"))],
        "voice_disconnect" => vec![
            plain(" disconnected "),
            name(t),
            plain(" from "),
            name(new_of("channel")),
        ],
        _ => vec![plain(" made a change")],
    };
    let mut out = vec![(actor, true)];
    out.extend(rest);
    out
}

fn view_name(v: &Value) -> String {
    match v.as_str() {
        Some("chat") => "Text".into(),
        Some("voice") => "Voice".into(),
        Some("forum") => "Forum".into(),
        Some(o) => o.into(),
        None => String::new(),
    }
}

fn hex_color(v: &Value) -> String {
    match v.as_u64().filter(|c| *c != 0) {
        Some(c) => format!("#{c:06x}"),
        None => "none".into(),
    }
}

fn shown(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        o => o.to_string(),
    }
}

/// What `entry` changed, one line each; empty when the sentence says it all.
pub fn details(entry: &Value, member: &dyn Fn(Option<&str>) -> String) -> Vec<String> {
    let mut out = Vec::new();
    let action = s(entry, "action").unwrap_or("");
    let creating = action.ends_with("_create");
    let truthy = |v: Option<&Value>| match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.),
        Some(_) => true,
    };
    for c in arr(entry, "changes") {
        let old = c.get("old").filter(|v| !v.is_null());
        let new = c.get("new");
        let added = arr(c, "added");
        let removed = arr(c, "removed");
        match s(c, "field").unwrap_or("") {
            "name" => {
                if !creating || action == "community_create" {
                    let new = new.map(shown).unwrap_or_default();
                    out.push(match old {
                        Some(o) => format!("Name: {} → {new}", shown(o)),
                        None => format!("Name: {new}"),
                    });
                }
            }
            "description" => out.push(if truthy(new) {
                "Changed the description".into()
            } else {
                "Removed the description".into()
            }),
            "topic" => out.push(if truthy(new) {
                format!("Topic: {}", new.map(shown).unwrap_or_default())
            } else {
                "Removed the topic".into()
            }),
            f @ ("icon" | "banner") => {
                let verb = match new.and_then(Value::as_str) {
                    Some("removed") => "Removed",
                    Some("added") => "Added",
                    _ => "Changed",
                };
                out.push(format!("{verb} the {f}"));
            }
            f @ ("relays" | "voice_relays") => {
                let kind = if f == "relays" {
                    "relay"
                } else {
                    "voice relay"
                };
                out.extend(
                    added
                        .iter()
                        .map(|r| format!("Added the {kind} {}", shown(r))),
                );
                out.extend(
                    removed
                        .iter()
                        .map(|r| format!("Removed the {kind} {}", shown(r))),
                );
            }
            "color" => out.push(match old {
                Some(o) => format!(
                    "Colour: {} → {}",
                    hex_color(o),
                    hex_color(new.unwrap_or(&Value::Null))
                ),
                None => format!("Colour: {}", hex_color(new.unwrap_or(&Value::Null))),
            }),
            "permissions" => {
                if !added.is_empty() {
                    out.push(format!(
                        "{}: {}",
                        if creating { "Permissions" } else { "Granted" },
                        perms(&c["added"])
                    ));
                }
                if !removed.is_empty() {
                    out.push(format!("Revoked: {}", perms(&c["removed"])));
                }
            }
            "allow" => {
                if !added.is_empty() {
                    out.push(format!("Allowed: {}", perms(&c["added"])));
                }
                if !removed.is_empty() {
                    out.push(format!("No longer allowed: {}", perms(&c["removed"])));
                }
            }
            "deny" => {
                if !added.is_empty() {
                    out.push(format!("Denied: {}", perms(&c["added"])));
                }
                if !removed.is_empty() {
                    out.push(format!("No longer denied: {}", perms(&c["removed"])));
                }
            }
            "position" => out.push("Moved it in the list".into()),
            "display" => out.push(if truthy(new) {
                "Shown on its own in the member list".into()
            } else {
                "No longer shown on its own in the member list".into()
            }),
            "mentionable" => out.push(if truthy(new) {
                "Anyone can mention it".into()
            } else {
                "No longer mentionable by everyone".into()
            }),
            "view" if !creating => out.push(format!(
                "Type: {} → {}",
                old.map(view_name).unwrap_or_default(),
                new.map(view_name).unwrap_or_default()
            )),
            "private" if !creating => out.push("Made it private".into()),
            "category" if action == "channel_update" => out.push(if truthy(new) {
                format!("Moved to {}", target_name(new, member))
            } else {
                "Moved out of its category".into()
            }),
            "roles" => {
                out.extend(
                    added
                        .iter()
                        .map(|r| format!("Added {}", target_name(Some(r), member))),
                );
                out.extend(
                    removed
                        .iter()
                        .map(|r| format!("Removed {}", target_name(Some(r), member))),
                );
                if let Some(held) = c.get("held").and_then(Value::as_array) {
                    out.push(if held.is_empty() {
                        "Holds no roles".into()
                    } else {
                        format!(
                            "Holds {}",
                            held.iter()
                                .map(|r| target_name(Some(r), member))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    });
                }
            }
            "nick" => match (
                old.and_then(Value::as_str).filter(|s| !s.is_empty()),
                new.and_then(Value::as_str).filter(|s| !s.is_empty()),
            ) {
                (Some(o), Some(n)) => out.push(format!("Nickname: {o} → {n}")),
                (Some(o), None) => out.push(format!("Was {o}")),
                _ => {}
            },
            "banned" => {
                if let Some(held) = c.get("held").and_then(Value::as_array) {
                    out.push(if held.is_empty() {
                        "Nobody is banned".into()
                    } else {
                        format!(
                            "Banned: {}",
                            held.iter()
                                .map(|m| target_name(Some(m), member))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    });
                }
            }
            _ => {}
        }
    }
    if entry
        .get("partial")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        out.push("What came before this was compacted when the community's keys were rotated, so what it changed isn't known.".into());
    }
    out
}

pub fn plain_text(entry: &Value, member: &dyn Fn(Option<&str>) -> String) -> String {
    sentence(entry, member)
        .into_iter()
        .map(|(t, _)| t)
        .collect()
}

/// When an entry happened, as a person says it.
pub fn when(ms: u64, now: u64) -> String {
    let diff = now.saturating_sub(ms);
    if diff < 60_000 {
        return "Just now".into();
    }
    if diff < 3_600_000 {
        return plural(diff / 60_000, "A minute ago", "minutes ago");
    }
    let d = crate::data::local(ms);
    let today = crate::data::local(now);
    let days = (today.date_naive() - d.date_naive()).num_days();
    match days {
        0 => format!("Today at {}", crate::data::clock(ms)),
        1 => format!("Yesterday at {}", crate::data::clock(ms)),
        _ if chrono::Datelike::year(&d) == chrono::Datelike::year(&today) => {
            d.format("%b %-d").to_string()
        }
        _ => d.format("%b %-d, %Y").to_string(),
    }
}

pub fn when_full(ms: u64) -> String {
    crate::data::local(ms)
        .format("%b %-d, %Y, %-I:%M %p")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sentences_and_details() {
        let member = |n: Option<&str>| {
            n.map(|n| n.chars().take(5).collect())
                .unwrap_or_else(|| "someone".into())
        };
        let e = json!({"actor": "alice", "action": "member_roles_update", "target": {"kind": "member", "npub": "bobby"}, "changes": [{"field": "roles", "added": [{"kind": "role", "name": "Mods"}], "removed": []}]});
        assert_eq!(plain_text(&e, &member), "alice gave bobby the role @Mods");
        assert_eq!(details(&e, &member), vec!["Added @Mods"]);
        let e = json!({"action": "role_create", "target": {"kind": "role", "name": "Mods"}, "changes": [{"field": "name", "new": "Mods"}, {"field": "permissions", "added": [3, 4], "removed": []}]});
        assert_eq!(plain_text(&e, &member), "Someone created the role @Mods");
        assert_eq!(
            details(&e, &member),
            vec!["Permissions: Kick Members, Ban Members"]
        );
        assert_eq!(tone_of("member_ban"), Tone::Remove);
        assert_eq!(family_of("overwrite_update"), "channels");
        assert_eq!(when(1_000_000, 1_000_000 + 120_000), "2 minutes ago");
    }
}
