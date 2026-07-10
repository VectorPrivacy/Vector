//! Bot Interface — manifests + slash commands (Phase 1 of the bot-UI layer).
//!
//! Transport-agnostic by design: everything here is CONTENT-level (structured
//! tags on ordinary chat rumors, plus one plain addressable discovery event),
//! so the same commands work in NIP-17 DMs and Concord v1/v2 channels — the
//! envelope is whatever the conversation already uses.
//!
//! Two pieces:
//!
//! 1. **Manifest** ([`BotManifest`], kind [`KIND_BOT_MANIFEST`]): an addressable
//!    event signed by the bot's key describing every command with typed args.
//!    Clients fetch it by pubkey to render a `/` picker with argument hints and
//!    validate input before anything hits the wire.
//! 2. **Invocation** ([`parse_command_text`]): a command is a NORMAL chat
//!    message whose `content` IS the invocation ("/price btc") — no extra
//!    structure on the wire. The manifest's ordering rules (required args
//!    first, a greedy String only in trailing position) make the text
//!    deterministically parseable, so the bot recovers exact typed arguments
//!    from content alone, and EVERY existing client is already a fully
//!    capable command sender.
//!
//! The kind number is provisional pending upstream registry coordination.

use std::collections::HashMap;

use nostr_sdk::prelude::{Event, EventBuilder, Keys, Kind, Tag};
use serde::{Deserialize, Serialize};

/// Addressable bot-interface manifest (outside any wrap; sibling of the public
/// invite bundle in the registry). One per bot pubkey at an empty `d`.
pub const KIND_BOT_MANIFEST: u16 = 33304;

/// Bounds (validated on BOTH build and parse — a foreign manifest is untrusted
/// input and must never cost unbounded memory or render work).
pub const MAX_COMMANDS: usize = 64;
pub const MAX_ARGS: usize = 8;
pub const MAX_CHOICES: usize = 32;
pub const MAX_NAME_LEN: usize = 32;
pub const MAX_DESCRIPTION_LEN: usize = 200;
pub const MAX_MANIFEST_BYTES: usize = 32_768;
/// A single argument value on the wire (tag or text) is clamped before typing.
pub const MAX_ARG_VALUE_LEN: usize = 1_024;

// ── Manifest ─────────────────────────────────────────────────────────────────

/// The typed shape of one command argument.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ArgType {
    /// Free text. As the LAST argument it greedily swallows the remainder of a
    /// plain-text invocation ("/say hello there" → one value "hello there").
    String,
    /// Signed integer (i64).
    Int,
    /// Float (f64).
    Number,
    /// "true"/"false" (also accepts "yes"/"no"/"1"/"0" from text).
    Bool,
    /// A user reference — an `npub1…` string on the wire.
    User,
    /// One of a fixed set of strings (renders as a picker).
    Choice,
}

/// One declared argument of a command.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ArgSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub arg_type: ArgType,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    /// Populated only for [`ArgType::Choice`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
}

/// One command a bot answers.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CommandSpec {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<ArgSpec>,
}

/// The bot's full published interface. Unknown fields are ignored on read
/// (forward compatibility); `v` gates breaking schema changes.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BotManifest {
    pub v: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<CommandSpec>,
}

/// Command/arg names are lowercase slugs so every client renders and matches
/// them identically (the cross-client contract).
fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_NAME_LEN
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

impl BotManifest {
    /// Fail-closed structural validation, applied to our own manifests before
    /// publish AND to fetched foreign ones before use.
    pub fn validate(&self) -> Result<(), String> {
        if self.v != 1 {
            return Err(format!("unsupported manifest version {}", self.v));
        }
        if self.commands.len() > MAX_COMMANDS {
            return Err(format!("too many commands ({} > {MAX_COMMANDS})", self.commands.len()));
        }
        let mut seen = std::collections::HashSet::new();
        for c in &self.commands {
            if !valid_name(&c.name) {
                return Err(format!("bad command name {:?}", c.name));
            }
            if !seen.insert(c.name.as_str()) {
                return Err(format!("duplicate command {:?}", c.name));
            }
            if c.description.len() > MAX_DESCRIPTION_LEN {
                return Err(format!("description too long on /{}", c.name));
            }
            if c.args.len() > MAX_ARGS {
                return Err(format!("too many args on /{}", c.name));
            }
            let mut arg_seen = std::collections::HashSet::new();
            let mut optional_seen = false;
            for a in &c.args {
                if !valid_name(&a.name) {
                    return Err(format!("bad arg name {:?} on /{}", a.name, c.name));
                }
                if !arg_seen.insert(a.name.as_str()) {
                    return Err(format!("duplicate arg {:?} on /{}", a.name, c.name));
                }
                if a.description.len() > MAX_DESCRIPTION_LEN {
                    return Err(format!("arg description too long on /{}", c.name));
                }
                // Positional text fallback needs required args to come first —
                // an optional hole would make "/cmd a b" ambiguous.
                if a.required && optional_seen {
                    return Err(format!("required arg {:?} after an optional one on /{}", a.name, c.name));
                }
                optional_seen |= !a.required;
                match a.arg_type {
                    ArgType::Choice => {
                        if a.choices.is_empty() || a.choices.len() > MAX_CHOICES {
                            return Err(format!("choice arg {:?} needs 1..={MAX_CHOICES} choices", a.name));
                        }
                        if a.choices.iter().any(|ch| ch.is_empty() || ch.len() > MAX_NAME_LEN) {
                            return Err(format!("bad choice value on {:?}", a.name));
                        }
                    }
                    _ if !a.choices.is_empty() => {
                        return Err(format!("choices on non-choice arg {:?}", a.name));
                    }
                    _ => {}
                }
            }
        }
        let bytes = serde_json::to_string(self).map_err(|e| e.to_string())?.len();
        if bytes > MAX_MANIFEST_BYTES {
            return Err(format!("manifest too large ({bytes} > {MAX_MANIFEST_BYTES} bytes)"));
        }
        Ok(())
    }

    /// Look up a command by name.
    pub fn command(&self, name: &str) -> Option<&CommandSpec> {
        self.commands.iter().find(|c| c.name == name)
    }

    /// Parse + validate a manifest from an event's content. The event must be
    /// the manifest kind and is otherwise treated as untrusted input.
    pub fn from_event(event: &Event) -> Result<Self, String> {
        if event.kind != Kind::Custom(KIND_BOT_MANIFEST) {
            return Err(format!("not a bot manifest (kind {})", event.kind));
        }
        if event.content.len() > MAX_MANIFEST_BYTES {
            return Err("manifest content over the size cap".into());
        }
        let m: BotManifest = serde_json::from_str(&event.content).map_err(|e| format!("manifest parse: {e}"))?;
        m.validate()?;
        Ok(m)
    }

    /// Build the signed addressable manifest event (empty `d`: one manifest per
    /// bot identity).
    pub fn to_event(&self, keys: &Keys) -> Result<Event, String> {
        self.validate()?;
        let content = serde_json::to_string(self).map_err(|e| e.to_string())?;
        EventBuilder::new(Kind::Custom(KIND_BOT_MANIFEST), content)
            .tags([Tag::identifier("")])
            .sign_with_keys(keys)
            .map_err(|e| e.to_string())
    }
}

// ── Invocation ───────────────────────────────────────────────────────────────

/// A command invocation recovered from a message's content. Values are raw
/// strings in manifest order; type them via [`typed_args`].
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedCommand {
    pub name: String,
    /// Named argument values in manifest order.
    pub args: Vec<(String, String)>,
}

/// The canonical content for an invocation a picker client builds — exactly
/// what a human would have typed ("/name value…"). Values containing spaces
/// or quotes are quoted with `\"` escapes so the text re-parses to the same
/// arguments.
pub fn command_text(name: &str, args: &[(String, String)]) -> String {
    let mut out = format!("/{name}");
    for (_, v) in args {
        out.push(' ');
        if v.is_empty() || v.contains(char::is_whitespace) || v.contains('"') {
            out.push('"');
            out.push_str(&v.replace('\\', "\\\\").replace('"', "\\\""));
            out.push('"');
        } else {
            out.push_str(v);
        }
    }
    out
}

/// One shell-style token: either a bare word or a `"quoted span"` (which may
/// contain spaces; `\"` is a literal quote, `\\` a literal backslash). Returns
/// (value, byte offset just past the token).
fn next_token(s: &str, mut i: usize) -> Option<(String, usize)> {
    let b = s.as_bytes();
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= b.len() {
        return None;
    }
    let mut out = String::new();
    if b[i] == b'"' {
        i += 1;
        while i < b.len() {
            match b[i] {
                b'\\' if i + 1 < b.len() && (b[i + 1] == b'"' || b[i + 1] == b'\\') => {
                    out.push(b[i + 1] as char);
                    i += 2;
                }
                b'"' => return Some((out, i + 1)),
                _ => {
                    // Multi-byte chars pass through verbatim.
                    let ch = s[i..].chars().next()?;
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
        }
        None // unterminated quote — malformed, not a command
    } else {
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() {
            i += 1;
        }
        out.push_str(&s[start..i]);
        Some((out, i))
    }
}

/// Parse "/name arg arg…" content against the manifest's arg order — THE
/// invocation parse. Returns `None` when the text isn't a known command (it is
/// then ordinary chat).
///
/// Multi-word values: a `"quoted value"` groups words in ANY position (`\"`
/// escapes a literal quote), and an UNQUOTED trailing String arg swallows the
/// raw remainder of the line — so the common forms ("/price btc",
/// "/say hello there") need no syntax at all, and two multi-word strings are
/// still expressible: `/announce "Big news" "Meeting at 5pm"`.
pub fn parse_command_text(manifest: &BotManifest, content: &str) -> Option<ParsedCommand> {
    let content = content.trim();
    let rest = content.strip_prefix('/')?;
    let (name, mut cursor) = next_token(rest, 0)?;
    if name.starts_with('"') || name.is_empty() {
        return None;
    }
    let spec = manifest.command(&name)?;
    let mut args: Vec<(String, String)> = Vec::new();
    for (i, a) in spec.args.iter().enumerate() {
        let remainder = rest.get(cursor..).unwrap_or("").trim_start();
        if remainder.is_empty() {
            break;
        }
        let is_last_declared = i + 1 == spec.args.len();
        let value = if is_last_declared && matches!(a.arg_type, ArgType::String) && !remainder.starts_with('"') {
            // Greedy tail: take the raw remainder verbatim (spacing preserved).
            cursor = rest.len();
            remainder.trim_end().to_string()
        } else {
            let (tok, next) = next_token(rest, cursor)?;
            cursor = next;
            tok
        };
        if value.len() > MAX_ARG_VALUE_LEN {
            return None;
        }
        args.push((a.name.clone(), value));
    }
    Some(ParsedCommand { name, args })
}

// ── Typing + validation against the manifest ─────────────────────────────────

/// One argument value, typed per its spec.
#[derive(Clone, Debug, PartialEq)]
pub enum ArgValue {
    String(String),
    Int(i64),
    Number(f64),
    Bool(bool),
    /// An `npub1…` user reference (kept as the bech32 string).
    User(String),
    Choice(String),
}

impl ArgValue {
    pub fn as_str(&self) -> &str {
        match self {
            ArgValue::String(s) | ArgValue::User(s) | ArgValue::Choice(s) => s,
            _ => "",
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            ArgValue::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn as_number(&self) -> Option<f64> {
        match self {
            ArgValue::Number(n) => Some(*n),
            ArgValue::Int(i) => Some(*i as f64),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ArgValue::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// Type-check a parsed invocation against the manifest: every provided value
/// parses as its declared type, choices are members, required args are present.
/// Unknown arg names are DROPPED (a newer client may know newer args; the bot's
/// manifest is authoritative for what it consumes).
pub fn typed_args(spec: &CommandSpec, parsed: &ParsedCommand) -> Result<HashMap<String, ArgValue>, String> {
    let mut out = HashMap::new();
    for (k, v) in &parsed.args {
        let Some(a) = spec.args.iter().find(|a| &a.name == k) else {
            continue;
        };
        let typed = match a.arg_type {
            ArgType::String => ArgValue::String(v.clone()),
            ArgType::Int => ArgValue::Int(v.parse::<i64>().map_err(|_| format!("{k}: not an integer"))?),
            ArgType::Number => ArgValue::Number(v.parse::<f64>().map_err(|_| format!("{k}: not a number"))?),
            ArgType::Bool => match v.to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" => ArgValue::Bool(true),
                "false" | "no" | "0" => ArgValue::Bool(false),
                _ => return Err(format!("{k}: not a boolean")),
            },
            ArgType::User => {
                if !v.starts_with("npub1") || v.len() > 70 {
                    return Err(format!("{k}: not an npub"));
                }
                ArgValue::User(v.clone())
            }
            ArgType::Choice => {
                if !a.choices.iter().any(|c| c == v) {
                    return Err(format!("{k}: not one of {:?}", a.choices));
                }
                ArgValue::Choice(v.clone())
            }
        };
        out.insert(k.clone(), typed);
    }
    for a in &spec.args {
        if a.required && !out.contains_key(&a.name) {
            return Err(format!("missing required arg {:?}", a.name));
        }
    }
    Ok(out)
}

// ── Network: publish + fetch ─────────────────────────────────────────────────

/// Publish `manifest` as the signed addressable event over the given relays
/// (targeted send — the caller decides the reach: login relays, communities,
/// indexers). Returns how many relays accepted it.
pub async fn publish_manifest(manifest: &BotManifest, keys: &Keys, relays: &[String]) -> Result<usize, String> {
    let event = manifest.to_event(keys)?;
    let client = crate::state::nostr_client().ok_or("no client connected")?;
    for r in relays {
        let _ = client.add_relay(r.as_str()).await;
    }
    client.connect().await;
    let out = client
        .send_event_to(relays.to_vec(), &event)
        .await
        .map_err(|e| e.to_string())?;
    Ok(out.success.len())
}

/// Fetch + validate a bot's manifest by pubkey from the given relays. Returns
/// the newest valid one, or `None` when the bot has published no interface.
pub async fn fetch_manifest(bot: &nostr_sdk::prelude::PublicKey, relays: &[String]) -> Option<BotManifest> {
    let client = crate::state::nostr_client()?;
    let filter = nostr_sdk::prelude::Filter::new()
        .kind(Kind::Custom(KIND_BOT_MANIFEST))
        .author(*bot)
        .limit(1);
    let events = if relays.is_empty() {
        client.fetch_events(filter, std::time::Duration::from_secs(8)).await.ok()?
    } else {
        client
            .fetch_events_from(relays.to_vec(), filter, std::time::Duration::from_secs(8))
            .await
            .ok()?
    };
    events
        .into_iter()
        .max_by_key(|e| e.created_at)
        .and_then(|e| BotManifest::from_event(&e).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::Keys;

    fn price_manifest() -> BotManifest {
        BotManifest {
            v: 1,
            commands: vec![
                CommandSpec {
                    name: "price".into(),
                    description: "Get a coin price".into(),
                    args: vec![ArgSpec {
                        name: "asset".into(),
                        arg_type: ArgType::Choice,
                        description: "Which coin".into(),
                        required: true,
                        choices: vec!["btc".into(), "xmr".into(), "pivx".into()],
                    }],
                },
                CommandSpec {
                    name: "say".into(),
                    description: "Echo".into(),
                    args: vec![
                        ArgSpec {
                            name: "count".into(),
                            arg_type: ArgType::Int,
                            description: String::new(),
                            required: true,
                            choices: vec![],
                        },
                        ArgSpec {
                            name: "text".into(),
                            arg_type: ArgType::String,
                            description: String::new(),
                            required: false,
                            choices: vec![],
                        },
                    ],
                },
            ],
        }
    }

    #[test]
    fn manifest_round_trips_through_its_event() {
        let keys = Keys::generate();
        let m = price_manifest();
        let ev = m.to_event(&keys).unwrap();
        assert_eq!(ev.kind, Kind::Custom(KIND_BOT_MANIFEST));
        let back = BotManifest::from_event(&ev).unwrap();
        assert_eq!(back.commands.len(), 2);
        assert_eq!(back.command("price").unwrap().args[0].choices.len(), 3);
    }

    #[test]
    fn validation_rejects_the_sharp_edges() {
        let mut m = price_manifest();
        m.commands[0].name = "Bad Name".into();
        assert!(m.validate().is_err());

        let mut m = price_manifest();
        m.commands.push(m.commands[0].clone());
        assert!(m.validate().is_err(), "duplicate command name");

        let mut m = price_manifest();
        m.commands[0].args[0].choices.clear();
        assert!(m.validate().is_err(), "choice without choices");

        // Required after optional breaks positional text parsing.
        let mut m = price_manifest();
        m.commands[1].args[0].required = false;
        m.commands[1].args[1].required = true;
        assert!(m.validate().is_err());
    }

    /// A command with TWO multi-word strings — expressible only via quoting.
    fn announce_manifest() -> BotManifest {
        BotManifest {
            v: 1,
            commands: vec![CommandSpec {
                name: "announce".into(),
                description: "Post an announcement".into(),
                args: vec![
                    ArgSpec { name: "title".into(), arg_type: ArgType::String, description: String::new(), required: true, choices: vec![] },
                    ArgSpec { name: "body".into(), arg_type: ArgType::String, description: String::new(), required: true, choices: vec![] },
                ],
            }],
        }
    }

    #[test]
    fn command_text_and_parse_are_inverses() {
        let m = price_manifest();
        let args = vec![("asset".to_string(), "btc".to_string())];
        let content = command_text("price", &args);
        assert_eq!(content, "/price btc");
        let p = parse_command_text(&m, &content).unwrap();
        assert_eq!(p.name, "price");
        assert_eq!(p.args, args);

        // Multi-word + embedded-quote values round-trip via quoting.
        let m = announce_manifest();
        let args = vec![
            ("title".to_string(), "Big \"news\" day".to_string()),
            ("body".to_string(), "Meeting at 5pm".to_string()),
        ];
        let content = command_text("announce", &args);
        let p = parse_command_text(&m, &content).unwrap();
        assert_eq!(p.args, args);
    }

    #[test]
    fn text_parses_positionally_and_greedily() {
        let m = price_manifest();
        let p = parse_command_text(&m, "/price btc").unwrap();
        assert_eq!(p.args, vec![("asset".to_string(), "btc".to_string())]);

        // Unquoted trailing String arg swallows the RAW remainder (spacing kept).
        let p = parse_command_text(&m, "/say 3 hello  there world").unwrap();
        assert_eq!(p.args[0], ("count".to_string(), "3".to_string()));
        assert_eq!(p.args[1], ("text".to_string(), "hello  there world".to_string()));

        assert!(parse_command_text(&m, "/unknown x").is_none());
        assert!(parse_command_text(&m, "not a command").is_none());
        assert!(parse_command_text(&m, "/").is_none());
    }

    #[test]
    fn quoting_terminates_multi_word_values() {
        let m = announce_manifest();
        // Both strings quoted — unambiguous.
        let p = parse_command_text(&m, r#"/announce "Hello everyone" "Meeting at 5pm""#).unwrap();
        assert_eq!(p.args[0].1, "Hello everyone");
        assert_eq!(p.args[1].1, "Meeting at 5pm");

        // First quoted, trailing unquoted → greedy tail.
        let p = parse_command_text(&m, r#"/announce "Hello everyone" Meeting at 5pm"#).unwrap();
        assert_eq!(p.args[0].1, "Hello everyone");
        assert_eq!(p.args[1].1, "Meeting at 5pm");

        // Unquoted first string takes ONE word (position rules unchanged).
        let p = parse_command_text(&m, "/announce Hello Meeting at 5pm").unwrap();
        assert_eq!(p.args[0].1, "Hello");
        assert_eq!(p.args[1].1, "Meeting at 5pm");

        // Escapes inside quotes.
        let p = parse_command_text(&m, r#"/announce "say \"hi\" \\ ok" done"#).unwrap();
        assert_eq!(p.args[0].1, r#"say "hi" \ ok"#);

        // Unterminated quote → not a command (ordinary chat).
        assert!(parse_command_text(&m, r#"/announce "dangling"#).is_none());
    }

    #[test]
    fn typing_enforces_the_manifest() {
        let m = price_manifest();
        let spec = m.command("price").unwrap();

        let ok = ParsedCommand {
            name: "price".into(),
            args: vec![("asset".into(), "btc".into())],
        };
        let t = typed_args(spec, &ok).unwrap();
        assert_eq!(t["asset"], ArgValue::Choice("btc".into()));

        let bad_choice = ParsedCommand {
            name: "price".into(),
            args: vec![("asset".into(), "doge".into())],
        };
        assert!(typed_args(spec, &bad_choice).is_err());

        let missing = ParsedCommand { name: "price".into(), args: vec![] };
        assert!(typed_args(spec, &missing).is_err());

        // Unknown arg names are dropped, not fatal (newer-manifest tolerance).
        let extra = ParsedCommand {
            name: "price".into(),
            args: vec![("asset".into(), "btc".into()), ("future".into(), "1".into())],
        };
        let t = typed_args(spec, &extra).unwrap();
        assert!(!t.contains_key("future"));

        let spec = m.command("say").unwrap();
        let typed = typed_args(
            spec,
            &ParsedCommand {
                name: "say".into(),
                args: vec![("count".into(), "5".into()), ("text".into(), "hi".into())],
            },
        )
        .unwrap();
        assert_eq!(typed["count"].as_int(), Some(5));
        assert_eq!(typed["text"].as_str(), "hi");

        let not_int = ParsedCommand {
            name: "say".into(),
            args: vec![("count".into(), "many".into())],
        };
        assert!(typed_args(spec, &not_int).is_err());
    }
}
