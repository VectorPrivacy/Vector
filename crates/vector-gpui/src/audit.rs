//! The Audit Log in words (`lib/audit-text.js`): each entry as a sentence of plain and bold
//! parts, the details it expands to, and the family the filter groups it under.

use gpui::SharedString;
use serde_json::Value;

use crate::kit::{self, Choice};
use crate::t;

pub fn families() -> &'static [Choice] {
    let c = |value: &'static str, label: SharedString| Choice { value, label: kit::interned(&label) };
    kit::choices("audit-families", || {
        vec![
            c("all", t::audit_family_all()),
            c("community", t::audit_family_community()),
            c("roles", t::audit_family_roles()),
            c("members", t::audit_family_members()),
            c("channels", t::audit_family_channels()),
            c("messages", t::audit_family_messages()),
            c("voice", t::audit_family_voice()),
        ]
    })
}

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

pub fn permission_name(bit: u64) -> String {
    crate::roles::groups(true)
        .into_iter()
        .flat_map(|(_, list)| list.iter())
        .find(|p| u64::from(p.bit) == bit)
        .map_or_else(|| t::audit_permission_unknown(bit).to_string(), |p| (p.name)().to_string())
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
            .unwrap_or_else(|| t::audit_deleted_role().into()),
        "member" => member(s(t, "npub")),
        "channel" => match name {
            Some(n) if s(t, "view") == Some("voice") => n.into(),
            Some(n) => format!("#{n}"),
            None => t::audit_private_channel().into(),
        },
        "category" => name.map_or_else(|| t::audit_unknown_category().into(), str::to_string),
        _ => String::new(),
    }
}

/// `entry` as `(text, bold)` parts; `member` names an npub (or none) as the log shows them.
pub fn sentence(entry: &Value, member: &dyn Fn(Option<&str>) -> String) -> Vec<(String, bool)> {
    let bold = |text: String| format!("<b>{}</b>", kit::escape(&text));
    let actor = bold(match s(entry, "actor") {
        Some(a) => member(Some(a)),
        None => t::audit_someone().into(),
    });
    let actor = actor.as_str();
    let t_ = entry.get("target");
    let name = |x: Option<&Value>| bold(target_name(x, member));
    let new_of = |f: &str| field(entry, f).and_then(|c| c.get("new"));
    let sub = |key: &str| t_.and_then(|t| t.get(key));
    let author = || bold(member(sub("author").and_then(Value::as_str)));
    let count = || new_of("count").and_then(Value::as_u64).filter(|n| *n > 0).unwrap_or(1);
    let target = || name(t_);
    let html = match s(entry, "action").unwrap_or("") {
        "community_create" => t::audit_community_create(actor),
        "community_update" => t::audit_community_update(actor),
        "roles_activate" => t::audit_roles_activate(actor),
        "everyone_update" => t::audit_everyone_update(actor, target()),
        "role_create" => t::audit_role_create(actor, target()),
        "role_update" => t::audit_role_update(actor, target()),
        "role_delete" => t::audit_role_delete(actor, target()),
        "member_roles_update" => {
            let c = field(entry, "roles");
            let added = c.map(|c| arr(c, "added")).unwrap_or(&[]);
            let removed = c.map(|c| arr(c, "removed")).unwrap_or(&[]);
            if added.len() == 1 && removed.is_empty() {
                t::audit_member_role_give(actor, target(), name(added.first()))
            } else if removed.len() == 1 && added.is_empty() {
                t::audit_member_role_take(actor, target(), name(removed.first()))
            } else {
                t::audit_member_roles_update(actor, target())
            }
        }
        "member_ban" => t::audit_member_ban(actor, target()),
        "member_unban" => t::audit_member_unban(actor, target()),
        "banlist_update" => t::audit_banlist_update(actor),
        "member_kick" => t::audit_member_kick(actor, target()),
        "member_timeout" => match new_of("until").and_then(Value::as_u64) {
            Some(ms) => t::audit_member_timeout_until(actor, target(), kit::escape(&when_full(ms))),
            None => t::audit_member_timeout(actor, target()),
        },
        "member_timeout_remove" => t::audit_member_timeout_remove(actor, target()),
        "member_nick_update" => {
            let nick = new_of("nick").and_then(Value::as_str).filter(|n| !n.is_empty());
            let own = t_.and_then(|t| s(t, "npub")).is_some_and(|n| Some(n) == s(entry, "actor"));
            match (nick, own) {
                (Some(n), true) => t::audit_nick_set_own(actor, bold(n.to_string())),
                (Some(n), false) => t::audit_nick_set(actor, target(), bold(n.to_string())),
                (None, true) => t::audit_nick_remove_own(actor),
                (None, false) => t::audit_nick_remove(actor, target()),
            }
        }
        "channel_create" => {
            let voice = new_of("view").and_then(Value::as_str) == Some("voice");
            let private = new_of("private").and_then(Value::as_bool).unwrap_or(false);
            match (private, voice) {
                (false, false) => t::audit_channel_create(actor, target()),
                (true, false) => t::audit_channel_create_private(actor, target()),
                (false, true) => t::audit_channel_create_voice(actor, target()),
                (true, true) => t::audit_channel_create_private_voice(actor, target()),
            }
        }
        "channel_update" => t::audit_channel_update(actor, target()),
        "channel_delete" => t::audit_channel_delete(actor, target()),
        "category_create" => t::audit_category_create(actor, target()),
        "category_update" => t::audit_category_update(actor, target()),
        "category_delete" => t::audit_category_delete(actor, target()),
        "overwrite_create" => t::audit_overwrite_create(actor, name(sub("in")), name(sub("for"))),
        "overwrite_update" => t::audit_overwrite_update(actor, name(sub("in")), name(sub("for"))),
        "overwrite_delete" => t::audit_overwrite_delete(actor, name(sub("in")), name(sub("for"))),
        "channel_sync" => t::audit_channel_sync(actor, name(new_of("category")), target()),
        "channel_unsync" => t::audit_channel_unsync(actor, target()),
        "message_pin" => t::audit_message_pin(actor, author(), name(sub("channel"))),
        "message_unpin" => t::audit_message_unpin(actor, author(), name(sub("channel"))),
        "pins_update" => t::audit_pins_update(actor, target()),
        "message_delete" => t::audit_message_delete(actor, author(), name(sub("channel"))),
        "invite_create" => t::audit_invite_create(actor, count()),
        "invite_delete" => t::audit_invite_delete(actor, count()),
        "keys_rotate" if field(entry, "severed").is_some() => t::audit_keys_lockdown(actor),
        "keys_rotate" => t::audit_keys_rotate(actor),
        "voice_mute" => t::audit_voice_mute(actor, name(new_of("channel")), target()),
        "voice_unmute" => t::audit_voice_unmute(actor, name(new_of("channel")), target()),
        "voice_deafen" => t::audit_voice_deafen(actor, name(new_of("channel")), target()),
        "voice_undeafen" => t::audit_voice_undeafen(actor, name(new_of("channel")), target()),
        "voice_move" => t::audit_voice_move(actor, name(new_of("to")), target()),
        "voice_disconnect" => t::audit_voice_disconnect(actor, name(new_of("channel")), target()),
        _ => t::audit_unknown(actor),
    };
    kit::runs(&html)
}

fn view_name(v: &Value) -> String {
    match v.as_str() {
        Some("chat") => t::audit_view_chat().into(),
        Some("voice") => t::audit_view_voice().into(),
        Some("forum") => t::audit_view_forum().into(),
        Some(o) => o.into(),
        None => String::new(),
    }
}

fn hex_color(v: &Value) -> String {
    match v.as_u64().filter(|c| *c != 0) {
        Some(c) => format!("#{c:06x}"),
        None => t::audit_colour_none().into(),
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
                        Some(o) => t::audit_detail_rename(new, shown(o)),
                        None => t::audit_detail_name(new),
                    });
                }
            }
            "description" => out.push(if truthy(new) { t::audit_detail_description() } else { t::audit_detail_description_removed() }),
            "topic" => out.push(if truthy(new) {
                t::audit_detail_topic(new.map(shown).unwrap_or_default())
            } else {
                t::audit_detail_topic_removed()
            }),
            f @ ("icon" | "banner") => out.push(match (f, new.and_then(Value::as_str)) {
                ("icon", Some("removed")) => t::audit_detail_icon_removed(),
                ("icon", Some("added")) => t::audit_detail_icon_added(),
                ("icon", _) => t::audit_detail_icon_changed(),
                (_, Some("removed")) => t::audit_detail_banner_removed(),
                (_, Some("added")) => t::audit_detail_banner_added(),
                _ => t::audit_detail_banner_changed(),
            }),
            "landing" => out.push(if new.and_then(|n| s(n, "kind")) == Some("channel") {
                t::audit_detail_landing(target_name(new, member))
            } else {
                t::audit_detail_landing_default()
            }),
            "system_messages" => out.push(match new {
                Some(Value::String(o)) if o == "off" => t::audit_detail_system_off(),
                Some(n) if s(n, "kind") == Some("channel") => t::audit_detail_system(target_name(new, member)),
                _ => t::audit_detail_system_default(),
            }),
            f @ ("relays" | "voice_relays") => {
                let voice = f == "voice_relays";
                out.extend(added.iter().map(|r| {
                    if voice { t::audit_detail_voice_relay_added(shown(r)) } else { t::audit_detail_relay_added(shown(r)) }
                }));
                out.extend(removed.iter().map(|r| {
                    if voice { t::audit_detail_voice_relay_removed(shown(r)) } else { t::audit_detail_relay_removed(shown(r)) }
                }));
            }
            "color" => {
                let now = hex_color(new.unwrap_or(&Value::Null));
                out.push(match old {
                    Some(o) => t::audit_detail_recolour(now, hex_color(o)),
                    None => t::audit_detail_colour(now),
                })
            }
            "permissions" => {
                if !added.is_empty() {
                    let list = perms(&c["added"]);
                    out.push(if creating { t::audit_detail_permissions(list) } else { t::audit_detail_granted(list) });
                }
                if !removed.is_empty() {
                    out.push(t::audit_detail_revoked(perms(&c["removed"])));
                }
            }
            "allow" => {
                if !added.is_empty() {
                    out.push(t::audit_detail_allowed(perms(&c["added"])));
                }
                if !removed.is_empty() {
                    out.push(t::audit_detail_unallowed(perms(&c["removed"])));
                }
            }
            "deny" => {
                if !added.is_empty() {
                    out.push(t::audit_detail_denied(perms(&c["added"])));
                }
                if !removed.is_empty() {
                    out.push(t::audit_detail_undenied(perms(&c["removed"])));
                }
            }
            "position" => out.push(t::audit_detail_position()),
            "display" => out.push(if truthy(new) { t::audit_detail_display() } else { t::audit_detail_display_off() }),
            "mentionable" => out.push(if truthy(new) { t::audit_detail_mentionable() } else { t::audit_detail_mentionable_off() }),
            "view" if !creating => out.push(t::audit_detail_view(
                new.map(view_name).unwrap_or_default(),
                old.map(view_name).unwrap_or_default(),
            )),
            "private" if !creating => out.push(t::audit_detail_private()),
            "category" if action == "channel_update" => out.push(if truthy(new) {
                t::audit_detail_moved(target_name(new, member))
            } else {
                t::audit_detail_uncategorised()
            }),
            "roles" => {
                out.extend(added.iter().map(|r| t::audit_detail_role_added(target_name(Some(r), member))));
                out.extend(removed.iter().map(|r| t::audit_detail_role_removed(target_name(Some(r), member))));
                if let Some(held) = c.get("held").and_then(Value::as_array) {
                    out.push(if held.is_empty() {
                        t::audit_detail_no_roles()
                    } else {
                        t::audit_detail_roles(held.iter().map(|r| target_name(Some(r), member)).collect::<Vec<_>>().join(", "))
                    });
                }
            }
            "nick" => match (
                old.and_then(Value::as_str).filter(|s| !s.is_empty()),
                new.and_then(Value::as_str).filter(|s| !s.is_empty()),
            ) {
                (Some(o), Some(n)) => out.push(t::audit_detail_renick(n, o)),
                (Some(o), None) => out.push(t::audit_detail_nick_removed(o)),
                _ => {}
            },
            "banned" => {
                if let Some(held) = c.get("held").and_then(Value::as_array) {
                    out.push(if held.is_empty() {
                        t::audit_detail_no_bans()
                    } else {
                        t::audit_detail_bans(held.iter().map(|m| target_name(Some(m), member)).collect::<Vec<_>>().join(", "))
                    });
                }
            }
            _ => {}
        }
    }
    if entry.get("partial").and_then(Value::as_bool).unwrap_or(false) {
        out.push(t::audit_detail_partial());
    }
    out.into_iter().map(String::from).collect()
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
        return t::audit_just_now().into();
    }
    if diff < 3_600_000 {
        return t::audit_minutes_ago(diff / 60_000).into();
    }
    let d = crate::data::local(ms);
    let today = crate::data::local(now);
    let days = (today.date_naive() - d.date_naive()).num_days();
    match days {
        0 => t::audit_today_at(crate::data::clock(ms)).into(),
        1 => t::audit_yesterday_at(crate::data::clock(ms)).into(),
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
        let e = json!({"actor": "alice", "action": "voice_mute", "target": {"kind": "member", "npub": "bobby"}, "changes": [{"field": "channel", "new": {"kind": "channel", "name": "Lounge", "view": "voice"}}]});
        assert_eq!(plain_text(&e, &member), "alice muted bobby in Lounge");
        let e = json!({"actor": "alice", "action": "overwrite_update", "target": {"for": {"kind": "role", "name": "Mods"}, "in": {"kind": "channel", "name": "dev"}}});
        assert_eq!(plain_text(&e, &member), "alice changed permissions for @Mods in #dev");
        let e = json!({"actor": "alice", "action": "channel_sync", "target": {"kind": "channel", "name": "dev"}, "changes": [{"field": "category", "new": {"kind": "category", "name": "Work"}}]});
        assert_eq!(plain_text(&e, &member), "alice synced #dev to Work");
        let e = json!({"actor": "alice", "action": "channel_update", "target": {"kind": "channel", "name": "devs"}, "changes": [{"field": "name", "old": "dev", "new": "devs"}]});
        assert_eq!(details(&e, &member), vec!["Name: dev → devs"]);
        assert_eq!(tone_of("member_ban"), Tone::Remove);
        assert_eq!(family_of("overwrite_update"), "channels");
        assert_eq!(when(1_000_000, 1_000_000 + 120_000), "2 minutes ago");
    }
}
