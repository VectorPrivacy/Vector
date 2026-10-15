//! The `init_finished` payload with every field left out that matches its template.
//!
//! Most of a boot's profiles are bare npubs, so their fields are nearly all empty, and every
//! cached image path repeats the cache directory. The page rebuilds each object from its
//! template in one place (`expandBootPayload` in `js/auth.js`) before anything reads it.
//!
//! A template holds only keys every item carries, so a key an item omits stays omitted, and
//! restoring a template value gives back exactly the value that was left out.

use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

use crate::chat::SerializableChat;
use crate::profile::SlimProfile;

/// Profile paths sent relative to the cache root, under the key with this suffix.
const RELATIVE: char = '~';

#[derive(Serialize)]
pub struct BootPayload<'a> {
    profiles: CompactProfiles<'a>,
    chats: Vec<Value>,
    templates: Templates,
    cache_root: &'a str,
}

#[derive(Serialize)]
struct Templates {
    profile: Value,
    chat: Map<String, Value>,
    message: Map<String, Value>,
}

pub fn boot_payload<'a>(profiles: &'a [SlimProfile], chats: &[SerializableChat], cache_root: Option<&'a str>) -> BootPayload<'a> {
    let root = cache_root.filter(|r| !r.is_empty());

    let mut chats = to_items(chats);
    let chat = template(&chats, &["messages"]);
    prune(&mut chats, &chat);
    let mut messages: Vec<&mut Value> = chats
        .iter_mut()
        .filter_map(|c| c.get_mut("messages").and_then(Value::as_array_mut))
        .flat_map(|m| m.iter_mut())
        .collect();
    let message = template_refs(&messages, &[]);
    for m in messages.iter_mut() {
        prune_one(m, &message);
    }

    BootPayload {
        profiles: CompactProfiles { items: profiles, root },
        chats,
        templates: Templates {
            profile: serde_json::to_value(SlimProfile::default()).unwrap_or_default(),
            chat,
            message,
        },
        cache_root: root.unwrap_or(""),
    }
}

/// Profiles stream out typed: thousands of them, so no `Value` tree. The template is the
/// default profile.
struct CompactProfiles<'a> {
    items: &'a [SlimProfile],
    root: Option<&'a str>,
}

impl Serialize for CompactProfiles<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let empty = SlimProfile::default();
        let mut seq = s.serialize_seq(Some(self.items.len()))?;
        for p in self.items {
            seq.serialize_element(&CompactProfile { p, empty: &empty, root: self.root })?;
        }
        seq.end()
    }
}

struct CompactProfile<'a> {
    p: &'a SlimProfile,
    empty: &'a SlimProfile,
    root: Option<&'a str>,
}

impl Serialize for CompactProfile<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let (p, d) = (self.p, self.empty);
        let mut m = s.serialize_map(None)?;
        macro_rules! changed {
            ($($f:ident),*) => {$(
                if p.$f != d.$f {
                    m.serialize_entry(stringify!($f), &p.$f)?;
                }
            )*};
        }
        changed!(id, name, display_name, nickname, lud06, lud16, banner, avatar, about, website, nip05, status, last_updated, mine, bot, is_blocked);
        for (key, path) in [("avatar_cached", &p.avatar_cached), ("banner_cached", &p.banner_cached)] {
            match self.root.and_then(|r| path.strip_prefix(r)) {
                Some(rel) => m.serialize_entry(&format_args!("{key}{RELATIVE}"), rel)?,
                None if !path.is_empty() => m.serialize_entry(key, path)?,
                None => {}
            }
        }
        changed!(stream_consent);
        m.end()
    }
}

fn to_items<T: serde::Serialize>(items: &[T]) -> Vec<Value> {
    items.iter().filter_map(|i| serde_json::to_value(i).ok()).collect()
}

fn template(items: &[Value], skip: &[&str]) -> Map<String, Value> {
    template_refs(&items.iter().collect::<Vec<_>>(), skip)
}

/// Per key every item carries, its most common default-like or short value, when it repeats.
fn template_refs<V: std::ops::Deref<Target = Value>>(items: &[V], skip: &[&str]) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(first) = items.first().and_then(|v| v.as_object()) else { return out };
    'keys: for key in first.keys() {
        if skip.contains(&key.as_str()) {
            continue;
        }
        let mut seen: Vec<(&Value, usize)> = Vec::new();
        for item in items {
            let Some(v) = item.get(key) else { continue 'keys };
            if let Some(entry) = seen.iter_mut().find(|(c, _)| *c == v) {
                entry.1 += 1;
            } else if candidate(v) {
                seen.push((v, 1));
            }
        }
        if let Some((v, n)) = seen.into_iter().max_by_key(|(_, n)| *n) {
            if n > 1 {
                out.insert(key.clone(), v.clone());
            }
        }
    }
    out
}

/// Worth templating: a value that plausibly repeats across many items.
fn candidate(v: &Value) -> bool {
    match v {
        Value::String(s) => s.len() <= 16,
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.values().all(default_like),
        _ => true,
    }
}

fn default_like(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.values().all(default_like),
    }
}

fn prune(items: &mut [Value], tpl: &Map<String, Value>) {
    for item in items {
        prune_one(item, tpl);
    }
}

fn prune_one(item: &mut Value, tpl: &Map<String, Value>) {
    if let Some(obj) = item.as_object_mut() {
        obj.retain(|k, v| tpl.get(k) != Some(v));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `expandBootPayload`, in Rust: the template under each item, relative paths re-rooted.
    fn expand(payload: &Value) -> (Vec<Value>, Vec<Value>) {
        let root = payload["cache_root"].as_str().unwrap();
        let fill = |items: &Value, tpl: &Value| -> Vec<Value> {
            items
                .as_array()
                .unwrap()
                .iter()
                .map(|p| {
                    let mut o = tpl.as_object().unwrap().clone();
                    for (k, v) in p.as_object().unwrap() {
                        match k.strip_suffix(RELATIVE) {
                            Some(k) => o.insert(k.to_string(), Value::String(format!("{root}{}", v.as_str().unwrap()))),
                            None => o.insert(k.clone(), v.clone()),
                        };
                    }
                    Value::Object(o)
                })
                .collect()
        };
        let mut chats = fill(&payload["chats"], &payload["templates"]["chat"]);
        for c in &mut chats {
            let m = fill(&c["messages"], &payload["templates"]["message"]);
            c["messages"] = Value::Array(m);
        }
        (fill(&payload["profiles"], &payload["templates"]["profile"]), chats)
    }

    fn profile(id: &str, name: &str, avatar_cached: &str) -> SlimProfile {
        SlimProfile { id: id.into(), name: name.into(), avatar_cached: avatar_cached.into(), ..Default::default() }
    }

    fn chat(id: &str, messages: Vec<crate::types::Message>) -> SerializableChat {
        let mut interner = crate::compact::NpubInterner::new();
        let mut c = crate::chat::Chat::new_dm(id.to_string(), &mut interner);
        for m in messages {
            c.internal_add_message(m, &mut interner);
        }
        c.to_serializable(&interner)
    }

    #[test]
    fn expanding_the_payload_gives_back_exactly_what_was_compacted() {
        let root = "/data/io.vectorapp/cache/";
        let profiles = vec![
            profile("npub1a", "", ""),
            profile("npub1b", "", ""),
            profile("npub1c", "Cat", "/data/io.vectorapp/cache/avatars/c.png"),
            profile("npub1d", "Dog", "/elsewhere/d.png"),
            SlimProfile { id: "npub1e".into(), mine: true, last_updated: 7, ..Default::default() },
        ];
        let msg = |id: &str, content: &str, npub: Option<&str>| crate::types::Message {
            id: id.repeat(64 / id.len()),
            content: content.into(),
            at: 1_700_000_000_000,
            npub: npub.map(str::to_string),
            ..Default::default()
        };
        let chats = vec![
            chat("npub1a", vec![msg("a1", "hi", Some("npub1a")), msg("a2", "", None)]),
            chat("npub1b", vec![]),
            chat("npub1c", vec![msg("c1", "yo", Some("npub1c"))]),
        ];

        let payload = serde_json::to_value(boot_payload(&profiles, &chats, Some(root))).unwrap();
        let (p, c) = expand(&payload);
        assert_eq!(p, to_items(&profiles), "profiles round-trip");
        assert_eq!(c, to_items(&chats), "chats and their messages round-trip");

        let sent = &payload["profiles"];
        assert_eq!(sent[0].as_object().unwrap().len(), 1, "a bare profile is just its id: {}", sent[0]);
        assert_eq!(sent[2]["avatar_cached~"], "avatars/c.png", "a cached path rides relative to the root");
        assert_eq!(sent[3]["avatar_cached"], "/elsewhere/d.png", "a path outside the root is sent whole");
        let missing_npub = payload["chats"][0]["messages"][1].as_object().unwrap();
        assert!(!missing_npub.contains_key("npub"), "an omitted optional key stays omitted");
    }

    #[test]
    fn expanding_without_a_root_or_items_is_a_no_op() {
        let payload = serde_json::to_value(boot_payload(&[], &[], None)).unwrap();
        let (p, c) = expand(&payload);
        assert!(p.is_empty() && c.is_empty());
        let profiles = vec![profile("npub1a", "", "/x/a.png"), profile("npub1b", "", "/x/a.png")];
        let payload = serde_json::to_value(boot_payload(&profiles, &[], None)).unwrap();
        assert_eq!(expand(&payload).0, to_items(&profiles));
    }

    #[test]
    fn every_profile_field_survives_the_compact_form() {
        // Each field set to a value unlike its default, so a field the serializer skips shows.
        let mut full = serde_json::to_value(SlimProfile::default()).unwrap();
        for (_, v) in full.as_object_mut().unwrap() {
            *v = match &*v {
                Value::String(_) => Value::String("x".into()),
                Value::Bool(_) => Value::Bool(true),
                Value::Number(_) => Value::from(7),
                Value::Object(o) => {
                    let mut o = o.clone();
                    for (_, f) in o.iter_mut() {
                        if f.is_string() {
                            *f = Value::String("y".into());
                        }
                    }
                    Value::Object(o)
                }
                other => other.clone(),
            };
        }
        let profile: SlimProfile = serde_json::from_value(full).unwrap();
        let profiles = [profile];
        let payload = serde_json::to_value(boot_payload(&profiles, &[], Some("/root/"))).unwrap();
        assert_eq!(expand(&payload).0, to_items(&profiles));
    }
}
