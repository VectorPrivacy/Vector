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

/// Optional recipient tag a picker client attaches to an invocation:
/// `["bot", <bot pubkey hex>]`. Addressing is the ONE piece of a command not
/// derivable from content — two bots can share a command name — so it rides a
/// tag while the invocation stays plain text. Semantics: tagged → only the
/// named bot(s) execute (others skip even on a manifest match); untagged →
/// broadcast, any matching bot may answer (the legacy-client path — bots never
/// REQUIRE the tag). Deliberately NOT `p`: chat rumors already carry `p` for
/// DM recipients and reply parents, and a skip-unless-me rule keyed on `p`
/// would silently swallow a command sent as a reply to a human. The tag is
/// routing, not authority — bots authorize by SENDER, never by tag.
pub const TAG_BOT: &str = "bot";

/// Recipient tags honored per message (routing metadata must stay cheap).
pub const MAX_BOT_TAGS: usize = 8;

/// Build the recipient tag a picker attaches: `["bot", <hex>]`.
pub fn bot_tag(bot: &nostr_sdk::prelude::PublicKey) -> Tag {
    Tag::custom(nostr_sdk::prelude::TagKind::Custom(TAG_BOT.into()), [bot.to_hex()])
}

/// Extract the addressed bots from a rumor's tags as npubs (deduped, capped,
/// invalid values skipped). Empty = untagged/broadcast.
pub fn addressed_bots<'a, I: IntoIterator<Item = &'a Tag>>(tags: I) -> Vec<String> {
    use nostr_sdk::prelude::ToBech32;
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let s = t.as_slice();
        if s.first().map(|k| k.as_str()) != Some(TAG_BOT) {
            continue;
        }
        let Some(v) = s.get(1) else { continue };
        let Ok(pk) = nostr_sdk::prelude::PublicKey::from_hex(v) else { continue };
        let Ok(npub) = pk.to_bech32() else { continue };
        if !out.contains(&npub) {
            out.push(npub);
        }
        if out.len() >= MAX_BOT_TAGS {
            break;
        }
    }
    out
}

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

// ── Picker surface: batch discovery + per-chat cache ─────────────────────────

/// One bot's published commands, shaped for a client's `/` picker. The client
/// resolves the bot's display name/avatar from its own profile cache.
#[derive(Serialize, Clone, Debug)]
pub struct ChatBotCommands {
    /// The bot's npub.
    pub bot: String,
    pub commands: Vec<CommandSpec>,
}

/// What the composer's `/` picker renders, instantly answerable from local
/// state. `fresh: false` means a background refetch was spawned — a
/// `chat_commands_updated` event follows with the converged list.
#[derive(Serialize, Clone, Debug)]
pub struct ChatCommandsSnapshot {
    /// How many bot-flagged members this chat has (spinner copy: "Loading N bots").
    pub bots: usize,
    /// Last-known command sets, one entry per bot with a stored manifest,
    /// commands in MANIFEST order (bots arrange their own list).
    pub commands: Vec<ChatBotCommands>,
    /// `true` = served from the fresh-TTL cache; nothing further will arrive.
    pub fresh: bool,
}

/// Batch-fetch validated manifests for a set of authors over specific relays —
/// the picker's ONE REQ (all bots of a room in a single query). Transport-
/// generic so community relays are queried directly and tests run offline.
/// Per author: the NEWEST event wins, then must validate (a bot that breaks
/// its own manifest has no usable interface — parity with [`fetch_manifest`]);
/// authors the relay volunteers beyond the asked set are dropped, as is
/// anything failing signature verification. Returns each manifest with its
/// event timestamp (the store's newest-wins key).
pub async fn fetch_manifests<T: crate::community::transport::Transport + ?Sized>(
    transport: &T,
    authors: &[nostr_sdk::prelude::PublicKey],
    relays: &[String],
) -> Result<Vec<(nostr_sdk::prelude::PublicKey, BotManifest, u64)>, String> {
    use crate::community::transport::Query;
    if authors.is_empty() {
        return Ok(Vec::new());
    }
    let query = Query {
        kinds: vec![KIND_BOT_MANIFEST],
        authors: authors.iter().map(|p| p.to_hex()).collect(),
        ..Default::default()
    };
    let events = transport.fetch(&query, relays).await?;
    let mut best: HashMap<nostr_sdk::prelude::PublicKey, &Event> = HashMap::new();
    for ev in &events {
        if !authors.contains(&ev.pubkey) || ev.verify().is_err() {
            continue;
        }
        match best.get(&ev.pubkey) {
            Some(b) if b.created_at >= ev.created_at => {}
            _ => {
                best.insert(ev.pubkey, ev);
            }
        }
    }
    let mut out: Vec<(nostr_sdk::prelude::PublicKey, BotManifest, u64)> = best
        .into_iter()
        .filter_map(|(pk, ev)| BotManifest::from_event(ev).ok().map(|m| (pk, m, ev.created_at.as_secs())))
        .collect();
    out.sort_by_key(|(pk, _, _)| pk.to_hex());
    Ok(out)
}

/// Assemble picker entries from the persistent manifest store for a set of bot
/// pubkeys (hex, pre-sorted order preserved). Bots with no stored manifest are
/// absent; a stored row that no longer parses/validates is skipped.
pub fn assemble_from_store(bot_hexes: &[String]) -> Vec<ChatBotCommands> {
    use nostr_sdk::prelude::ToBech32;
    let rows = crate::db::bots::get_bot_manifests(bot_hexes).unwrap_or_default();
    let by_pk: HashMap<&str, &str> = rows.iter().map(|(pk, m)| (pk.as_str(), m.as_str())).collect();
    bot_hexes
        .iter()
        .filter_map(|hex| {
            let json = by_pk.get(hex.as_str())?;
            let manifest: BotManifest = serde_json::from_str(json).ok()?;
            manifest.validate().ok()?;
            let npub = nostr_sdk::prelude::PublicKey::from_hex(hex).ok()?.to_bech32().ok()?;
            Some(ChatBotCommands { bot: npub, commands: manifest.commands })
        })
        .collect()
}

/// Freshness memory per chat: (session generation, refreshed-at, the bot set
/// the refresh covered). One REQ per chat per minute; a CHANGED bot set (a bot
/// joined/left) counts as stale immediately. The generation tag makes an
/// account swap a natural invalidation.
const COMMANDS_TTL: std::time::Duration = std::time::Duration::from_secs(60);
static COMMANDS_FRESH: std::sync::LazyLock<
    std::sync::Mutex<HashMap<String, (u64, std::time::Instant, Vec<String>)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));
/// Chats with a refresh REQ in flight (stampede guard for `/` keystrokes).
static REFRESH_INFLIGHT: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

/// `true` while the last completed refresh for this chat is within TTL AND
/// covered exactly `bot_hexes`.
pub fn commands_fresh(chat_id: &str, bot_hexes: &[String]) -> bool {
    let generation = crate::state::SessionGuard::capture().generation();
    let map = match COMMANDS_FRESH.lock() {
        Ok(m) => m,
        Err(_) => return false,
    };
    match map.get(chat_id) {
        Some((g, at, bots)) => *g == generation && at.elapsed() < COMMANDS_TTL && bots == bot_hexes,
        None => false,
    }
}

fn mark_commands_fresh(chat_id: &str, generation: u64, bot_hexes: &[String]) {
    if let Ok(mut map) = COMMANDS_FRESH.lock() {
        if map.len() > 256 {
            map.clear();
        }
        map.insert(chat_id.to_string(), (generation, std::time::Instant::now(), bot_hexes.to_vec()));
    }
}

/// Background half of the picker flow: ONE REQ for every bot's manifest (5s
/// unification window), persist newer editions, mark the chat fresh, and tell
/// the UI to swap in the converged list. Deduped per chat; session-guarded
/// before every write.
pub fn spawn_commands_refresh(chat_id: String, bots: Vec<nostr_sdk::prelude::PublicKey>, relays: Vec<String>) {
    {
        let Ok(mut inflight) = REFRESH_INFLIGHT.lock() else { return };
        if !inflight.insert(chat_id.clone()) {
            return; // already fetching for this chat
        }
    }
    let session = crate::state::SessionGuard::capture();
    tokio::spawn(async move {
        let transport = crate::community::transport::LiveTransport::with_timeout(std::time::Duration::from_secs(5));
        let fetched = fetch_manifests(&transport, &bots, &relays).await;
        if let Ok(mut inflight) = REFRESH_INFLIGHT.lock() {
            inflight.remove(&chat_id);
        }
        let Ok(found) = fetched else { return }; // transient failure: stay stale, next `/` retries
        if !session.is_valid() {
            return;
        }
        for (pk, manifest, created_at) in &found {
            if let Ok(json) = serde_json::to_string(manifest) {
                let _ = crate::db::bots::upsert_bot_manifest(&pk.to_hex(), &json, *created_at);
            }
        }
        let bot_hexes: Vec<String> = bots.iter().map(|p| p.to_hex()).collect();
        let commands = assemble_from_store(&bot_hexes);
        if !session.is_valid() {
            return;
        }
        mark_commands_fresh(&chat_id, session.generation(), &bot_hexes);
        crate::traits::emit_event(
            "chat_commands_updated",
            &serde_json::json!({ "chat_id": chat_id, "bots": bots.len(), "commands": commands }),
        );
    });
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
    fn bot_recipient_tags_extract_dedup_and_cap() {
        use nostr_sdk::prelude::ToBech32;
        let a = Keys::generate().public_key();
        let b = Keys::generate().public_key();
        let tags: Vec<Tag> = vec![
            bot_tag(&a),
            bot_tag(&a), // dup
            bot_tag(&b),
            Tag::custom(nostr_sdk::prelude::TagKind::Custom(TAG_BOT.into()), ["nothex"]),
            Tag::custom(nostr_sdk::prelude::TagKind::Custom("p".into()), [a.to_hex()]), // not ours
        ];
        let out = addressed_bots(tags.iter());
        assert_eq!(out, vec![a.to_bech32().unwrap(), b.to_bech32().unwrap()]);

        // Cap: 20 distinct tags → MAX_BOT_TAGS honored.
        let many: Vec<Tag> = (0..20).map(|_| bot_tag(&Keys::generate().public_key())).collect();
        assert_eq!(addressed_bots(many.iter()).len(), MAX_BOT_TAGS);

        // Untagged → empty (broadcast).
        assert!(addressed_bots([].iter()).is_empty());
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

    #[tokio::test]
    async fn batch_fetch_returns_newest_valid_per_author_and_ignores_strangers() {
        use crate::community::transport::{memory::MemoryRelay, Transport};
        use nostr_sdk::prelude::Timestamp;
        let relay = MemoryRelay::new();
        let relays = vec!["r1".to_string()];
        let bot_a = Keys::generate();
        let bot_b = Keys::generate();
        let stranger = Keys::generate();

        let manifest_event = |m: &BotManifest, keys: &Keys, at: u64| {
            EventBuilder::new(Kind::Custom(KIND_BOT_MANIFEST), serde_json::to_string(m).unwrap())
                .tags([Tag::identifier("")])
                .custom_created_at(Timestamp::from_secs(at))
                .sign_with_keys(keys)
                .unwrap()
        };
        // A: an old manifest, then a newer edition with a different command set.
        let old_ev = manifest_event(&price_manifest(), &bot_a, 100);
        let newer = BotManifest {
            v: 1,
            commands: vec![CommandSpec { name: "newer".into(), description: "n".into(), args: vec![] }],
        };
        let new_ev = manifest_event(&newer, &bot_a, 200);
        // B: newest is garbage — B has no usable interface (no fallback to older).
        let b_garbage = EventBuilder::new(Kind::Custom(KIND_BOT_MANIFEST), "not json")
            .tags([Tag::identifier("")])
            .custom_created_at(Timestamp::from_secs(300))
            .sign_with_keys(&bot_b)
            .unwrap();
        // Stranger: a VALID manifest outside the asked author set.
        let s_ev = price_manifest().to_event(&stranger).unwrap();
        for ev in [&old_ev, &new_ev, &b_garbage, &s_ev] {
            relay.publish(ev, &relays).await.unwrap();
        }

        let found = fetch_manifests(&relay, &[bot_a.public_key(), bot_b.public_key()], &relays)
            .await
            .unwrap();
        assert_eq!(found.len(), 1, "only A has a usable newest manifest: {found:?}");
        assert_eq!(found[0].0, bot_a.public_key());
        assert!(found[0].1.command("newer").is_some(), "the newest edition won");
        assert!(found[0].1.command("price").is_none(), "the older edition lost");
        assert_eq!(found[0].2, 200, "the winning edition's timestamp rides along");

        let none = fetch_manifests(&relay, &[], &relays).await.unwrap();
        assert!(none.is_empty(), "empty author set short-circuits");
    }

    #[test]
    fn command_freshness_is_generation_ttl_and_botset_scoped() {
        // Serialize with bed tests — they bump the session generation mid-test.
        let _guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let generation = crate::state::SessionGuard::capture().generation();
        let bots = vec!["aa".to_string(), "bb".to_string()];

        assert!(!commands_fresh("cmd-fresh-a", &bots), "unseen chat is stale");
        mark_commands_fresh("cmd-fresh-a", generation, &bots);
        assert!(commands_fresh("cmd-fresh-a", &bots));

        // A CHANGED bot set is immediately stale (a bot joined/left the room).
        let grown = vec!["aa".to_string(), "bb".to_string(), "cc".to_string()];
        assert!(!commands_fresh("cmd-fresh-a", &grown));

        // Another generation's entry is invisible — the account-swap invalidation.
        mark_commands_fresh("cmd-fresh-b", generation.wrapping_add(1), &bots);
        assert!(!commands_fresh("cmd-fresh-b", &bots));
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
