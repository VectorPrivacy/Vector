//! Slash commands for bots — the SDK face of `vector_core::bot_interface`.
//!
//! Register commands with [`VectorBot::command`]; the SDK derives the Bot
//! Interface Manifest from the registrations and publishes it at listen start
//! (login relays + discovery indexers + every held community's relays), and
//! intercepts inbound messages whose content parses as a registered command —
//! matched commands run their handler and are CONSUMED (they never reach
//! `on_message`/`on_event`, mirroring how interaction frameworks separate
//! commands from chat).
//!
//! The wire format is just message content ("/roll 20") parsed against the
//! manifest — any client, no matter how old, can invoke a command by typing.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use vector_core::bot_interface::{
    self, ArgSpec, ArgType, ArgValue, BotManifest, CommandSpec,
};

use crate::{IncomingMessage, VectorBot};

type CommandFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type CommandHandler = Arc<dyn Fn(CommandCtx) -> CommandFuture + Send + Sync>;

/// One registered command: its manifest spec + the handler.
struct Registration {
    spec: CommandSpec,
    handler: CommandHandler,
}

/// The bot's command table. Shared by every [`VectorBot`] clone. A Vec, not a
/// map: the manifest publishes commands in REGISTRATION order — the
/// arrangement is the bot author's choice and clients render it verbatim.
/// Registration order is code order, so the published event stays
/// deterministic across restarts.
#[derive(Default)]
pub(crate) struct CommandRegistry {
    commands: RwLock<Vec<Registration>>,
}

impl CommandRegistry {
    fn insert(&self, spec: CommandSpec, handler: CommandHandler) -> Result<(), String> {
        let manifest_probe = BotManifest { v: 1, commands: vec![spec.clone()] };
        manifest_probe.validate()?;
        let mut list = self.commands.write().unwrap_or_else(|e| e.into_inner());
        if list.len() >= bot_interface::MAX_COMMANDS {
            return Err(format!("too many commands (max {})", bot_interface::MAX_COMMANDS));
        }
        if list.iter().any(|r| r.spec.name == spec.name) {
            return Err("command name already registered".into());
        }
        list.push(Registration { spec, handler });
        Ok(())
    }

    /// The manifest derived from every registration, in registration order.
    pub(crate) fn manifest(&self) -> BotManifest {
        let list = self.commands.read().unwrap_or_else(|e| e.into_inner());
        BotManifest { v: 1, commands: list.iter().map(|r| r.spec.clone()).collect() }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.commands.read().unwrap_or_else(|e| e.into_inner()).is_empty()
    }
}

/// Everything a command handler needs: the triggering message, the typed
/// arguments, and the bot.
pub struct CommandCtx {
    pub bot: VectorBot,
    pub msg: IncomingMessage,
    args: HashMap<String, ArgValue>,
}

impl CommandCtx {
    /// A string-ish arg (String / User / Choice) by name, if provided.
    pub fn str(&self, name: &str) -> Option<&str> {
        self.args.get(name).map(|v| v.as_str()).filter(|s| !s.is_empty())
    }
    /// An integer arg by name, if provided.
    pub fn int(&self, name: &str) -> Option<i64> {
        self.args.get(name).and_then(|v| v.as_int())
    }
    /// A float arg by name (Int coerces), if provided.
    pub fn number(&self, name: &str) -> Option<f64> {
        self.args.get(name).and_then(|v| v.as_number())
    }
    /// A boolean arg by name, if provided.
    pub fn flag(&self, name: &str) -> Option<bool> {
        self.args.get(name).and_then(|v| v.as_bool())
    }
    /// Threaded reply to the invoking message.
    pub async fn reply(&self, text: impl AsRef<str>) -> crate::Result<String> {
        self.msg.reply(text.as_ref()).await
    }
}

/// Builder returned by [`VectorBot::command`]; finish with [`run`](Self::run).
pub struct CommandBuilder {
    bot: VectorBot,
    spec: CommandSpec,
}

impl CommandBuilder {
    pub(crate) fn new(bot: VectorBot, name: &str, description: &str) -> Self {
        Self {
            bot,
            spec: CommandSpec { name: name.to_string(), description: description.to_string(), args: vec![] },
        }
    }

    fn push(mut self, name: &str, arg_type: ArgType, description: &str, required: bool, choices: Vec<String>) -> Self {
        self.spec.args.push(ArgSpec {
            name: name.to_string(),
            arg_type,
            description: description.to_string(),
            required,
            choices,
        });
        self
    }

    /// Free-text argument. In trailing position it swallows the rest of the
    /// line ("/say hello there" → one value).
    pub fn string(self, name: &str, description: &str, required: bool) -> Self {
        self.push(name, ArgType::String, description, required, vec![])
    }
    /// Integer argument.
    pub fn int(self, name: &str, description: &str, required: bool) -> Self {
        self.push(name, ArgType::Int, description, required, vec![])
    }
    /// Float argument.
    pub fn number(self, name: &str, description: &str, required: bool) -> Self {
        self.push(name, ArgType::Number, description, required, vec![])
    }
    /// Boolean argument (true/false/yes/no/1/0).
    pub fn flag(self, name: &str, description: &str, required: bool) -> Self {
        self.push(name, ArgType::Bool, description, required, vec![])
    }
    /// An `npub1…` user argument.
    pub fn user(self, name: &str, description: &str, required: bool) -> Self {
        self.push(name, ArgType::User, description, required, vec![])
    }
    /// One-of-a-fixed-set argument (renders as a picker in clients).
    pub fn choice<S: Into<String>>(self, name: &str, description: &str, choices: impl IntoIterator<Item = S>, required: bool) -> Self {
        self.push(name, ArgType::Choice, description, required, choices.into_iter().map(Into::into).collect())
    }

    /// Register the handler. Panics on an invalid spec (bad name, duplicate,
    /// arg-order violation) — registration is developer code, fail loudly at
    /// startup rather than silently drop a command.
    pub fn run<F, Fut>(self, handler: F)
    where
        F: Fn(CommandCtx) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let handler: CommandHandler = Arc::new(move |ctx| Box::pin(handler(ctx)));
        if let Err(e) = self.bot.commands().insert(self.spec, handler) {
            panic!("invalid command registration: {e}");
        }
    }
}

impl VectorBot {
    /// Register a slash command. Chain typed args, then attach the handler:
    ///
    /// ```no_run
    /// # async fn ex(bot: vector_sdk::VectorBot) {
    /// bot.command("roll", "Roll a die")
    ///     .int("sides", "How many sides", false)
    ///     .run(|ctx| async move {
    ///         let sides = ctx.int("sides").unwrap_or(6);
    ///         let _ = ctx.reply(format!("🎲 you rolled a d{sides}")).await;
    ///     });
    /// # }
    /// ```
    ///
    /// The manifest publishes automatically at listen start; a matched inbound
    /// command runs its handler and never reaches `on_message`/`on_event`.
    pub fn command(&self, name: &str, description: &str) -> CommandBuilder {
        CommandBuilder::new(self.clone(), name, description)
    }

    /// Intercept `incoming` as a command if its content matches a registration.
    /// Returns true when consumed. A parse against the manifest that matches a
    /// NAME but fails typing/required checks replies with the error and still
    /// consumes — half-valid invocations shouldn't leak into chat handlers.
    pub(crate) fn try_command(&self, incoming: &IncomingMessage) -> bool {
        if incoming.is_mine() || self.commands().is_empty() {
            return false;
        }
        // Recipient-tagged for other bot(s) only → ordinary chat for us, even on
        // a manifest match (two bots may share a command name). Untagged =
        // broadcast — the legacy-client path — so the tag is never REQUIRED.
        let addressed = &incoming.message.addressed_bots;
        if !addressed.is_empty() && !addressed.iter().any(|n| n == self.npub()) {
            return false;
        }
        let content = incoming.text().trim();
        if !content.starts_with('/') {
            return false;
        }
        let manifest = self.commands().manifest();
        let Some(parsed) = bot_interface::parse_command_text(&manifest, content) else {
            return false; // unknown command → ordinary chat (may be for another bot)
        };
        let registry = self.commands();
        let list = registry.commands.read().unwrap_or_else(|e| e.into_inner());
        let Some(reg) = list.iter().find(|r| r.spec.name == parsed.name) else { return false };
        let spec = reg.spec.clone();
        let handler = reg.handler.clone();
        drop(list);

        let bot = self.clone();
        let incoming = incoming.clone();
        tokio::spawn(async move {
            match bot_interface::typed_args(&spec, &parsed) {
                Ok(args) => {
                    println!("[CMD] /{} {:?}", parsed.name, parsed.args);
                    let ctx = CommandCtx { bot, msg: incoming, args };
                    handler(ctx).await;
                }
                Err(e) => {
                    println!("[CMD] /{} REJECTED ({e}) raw={:?}", parsed.name, parsed.args);
                    let usage = usage_line(&spec);
                    let _ = incoming.reply(&format!("{e} — usage: {usage}")).await;
                }
            }
        });
        true
    }

    /// Build + publish the interface manifest over the widest useful reach:
    /// the connected (login) relays, the public discovery indexers, and every
    /// held community's relays — the same triple the bot-profile lesson taught
    /// (community relays are pool-isolated and Ditto drops stranger events, so
    /// the indexers are the reliable discovery path).
    pub(crate) async fn publish_interface_manifest(&self) {
        if self.commands().is_empty() {
            return;
        }
        let manifest = self.commands().manifest();
        let Some(keys) = vector_core::state::MY_SECRET_KEY.to_keys() else {
            return; // bunker identities sign remotely — manifest publish is a follow-up there
        };
        let mut relays: Vec<String> = DISCOVERY_RELAYS.iter().map(|s| s.to_string()).collect();
        if let Some(client) = vector_core::state::nostr_client() {
            relays.extend(client.relays().await.keys().map(|r| r.to_string()));
        }
        for id in vector_core::db::community::list_community_ids().unwrap_or_default() {
            if let Ok(Some(c)) = vector_core::db::community::load_community_v2(&id) {
                relays.extend(c.relays.clone());
            } else if let Ok(Some(c)) = vector_core::db::community::load_community(&id) {
                // v1 community — invocations are plain content there too, so its
                // members' pickers need the manifest on the same relays.
                relays.extend(c.relays.clone());
            }
        }
        relays.sort();
        relays.dedup();
        match bot_interface::publish_manifest(&manifest, &keys, &relays).await {
            Ok(n) => println!("[vector-sdk] interface manifest ({} command(s)) stored on {n} relay(s)", manifest.commands.len()),
            Err(e) => eprintln!("[vector-sdk] manifest publish failed: {e}"),
        }
    }
}

/// Public relays that index addressable/replaceable events network-wide — the
/// reliable discovery path for a bot's manifest (and profile). One list shared
/// with the reader side (clients query these beside a chat's own relays), so
/// publish reach and lookup reach can't drift apart.
pub use vector_core::bot_interface::DISCOVERY_RELAYS;

/// "/name <a:int> [b]" — the one-line usage hint for error replies.
fn usage_line(spec: &CommandSpec) -> String {
    let mut out = format!("/{}", spec.name);
    for a in &spec.args {
        let ty = match a.arg_type {
            ArgType::String => "text",
            ArgType::Int => "int",
            ArgType::Number => "number",
            ArgType::Bool => "true|false",
            ArgType::User => "npub",
            ArgType::Choice => "choice",
        };
        if a.required {
            out.push_str(&format!(" <{}:{}>", a.name, ty));
        } else {
            out.push_str(&format!(" [{}:{}]", a.name, ty));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_builds_the_manifest_in_registration_order() {
        let reg = CommandRegistry::default();
        let noop: CommandHandler = Arc::new(|_| Box::pin(async {}));
        reg.insert(
            CommandSpec { name: "zeta".into(), description: "z".into(), args: vec![] },
            noop.clone(),
        )
        .unwrap();
        reg.insert(
            CommandSpec {
                name: "alpha".into(),
                description: "a".into(),
                args: vec![ArgSpec {
                    name: "n".into(),
                    arg_type: ArgType::Int,
                    description: String::new(),
                    required: true,
                    choices: vec![],
                }],
            },
            noop.clone(),
        )
        .unwrap();

        // The author registered zeta FIRST — the manifest keeps that arrangement
        // (clients render it verbatim; ordering is the creator's choice).
        let m = reg.manifest();
        m.validate().unwrap();
        assert_eq!(m.commands[0].name, "zeta");
        assert_eq!(m.commands[1].name, "alpha");

        // Duplicate name refused.
        assert!(reg
            .insert(CommandSpec { name: "alpha".into(), description: String::new(), args: vec![] }, noop)
            .is_err());
    }

    #[tokio::test]
    async fn recipient_tag_routes_commands() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use vector_core::VectorCore;

        let ran = Arc::new(AtomicBool::new(false));
        let me = "npub1me000000000000000000000000000000000000000000000000000000".to_string();
        let bot = VectorBot {
            core: VectorCore,
            npub: me.clone(),
            invite_policy: Arc::new(crate::InvitePolicy::Manual),
            commands: Arc::new(Default::default()),
        };
        let flag = ran.clone();
        bot.command("ping", "test").run(move |_ctx| {
            let flag = flag.clone();
            async move {
                flag.store(true, Ordering::SeqCst);
            }
        });

        let incoming = |bots: Vec<String>| crate::IncomingMessage {
            chat_id: "chan".into(),
            is_group: true,
            is_file: false,
            message: vector_core::Message {
                content: "/ping".into(),
                addressed_bots: bots,
                ..Default::default()
            },
        };

        // Tagged for another bot: NOT consumed (ordinary chat for us).
        assert!(!bot.try_command(&incoming(vec!["npub1other".into()])));
        // Tagged for us: consumed + handler runs.
        assert!(bot.try_command(&incoming(vec![me.clone()])));
        // Untagged broadcast: consumed.
        assert!(bot.try_command(&incoming(vec![])));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(ran.load(Ordering::SeqCst));
    }

    #[test]
    fn usage_line_renders_required_and_optional() {
        let spec = CommandSpec {
            name: "roll".into(),
            description: String::new(),
            args: vec![
                ArgSpec { name: "sides".into(), arg_type: ArgType::Int, description: String::new(), required: true, choices: vec![] },
                ArgSpec { name: "label".into(), arg_type: ArgType::String, description: String::new(), required: false, choices: vec![] },
            ],
        };
        assert_eq!(usage_line(&spec), "/roll <sides:int> [label:text]");
    }
}
