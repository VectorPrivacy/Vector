//! # Vector SDK
//!
//! Build a private-messaging bot in about a dozen lines.
//!
//! [Vector](https://vectorapp.io) is a private, encrypted messenger. This SDK lets your
//! bot send and receive messages, files, and reactions, join communities, and ride out
//! network drops — without ever touching the protocol or encryption underneath.
//!
//! ```no_run
//! use vector_sdk::VectorBot;
//!
//! #[tokio::main]
//! async fn main() -> vector_sdk::Result<()> {
//!     let bot = VectorBot::builder()
//!         .nsec("nsec1...")          // or .mnemonic("twelve words ...")
//!         .build()
//!         .await?;
//!
//!     println!("Online as {}", bot.npub());
//!
//!     // Reply to everything. The SAME handler serves DMs *and* Community channels.
//!     bot.on_message(|_bot, msg| async move {
//!         if msg.is_mine() { return; }              // ignore our own messages
//!         let _ = msg.reply(&format!("You said: {}", msg.text())).await;
//!     }).await?;
//!
//!     Ok(())
//! }
//! ```
//!
//! That bot already handles direct messages *and* communities, reconnects after a
//! network drop, and catches up on what it missed.
//!
//! ## One API, everywhere
//!
//! Your bot sends and receives through a **`Channel`** — a direct-message chat or a
//! community channel, handled **identically**. You never branch on which it is:
//! [`bot.channel(id)`](VectorBot::channel) opens either by id, and
//! [`msg.reply(...)`](IncomingMessage::reply) answers wherever the message came from.
//!
//! ```no_run
//! # use vector_sdk::VectorBot;
//! # async fn run(bot: VectorBot, id: &str, msg_id: &str) -> vector_sdk::Result<()> {
//! let chat = bot.channel(id);            // DM or Community channel — auto-detected
//! chat.send("hi").await?;                //
//! chat.react(msg_id, "👍").await?;       // identical surface either way
//! chat.send_file("./photo.png").await?;  //
//! chat.typing().await?;                  // "typing…" indicator
//! # Ok(()) }
//! ```
//!
//! ## What a bot can do
//!
//! | You want to… | …you call |
//! | --- | --- |
//! | Send / reply / edit / delete | [`Channel::send`] · [`reply`](Channel::reply) · [`edit`](Channel::edit) · [`delete`](Channel::delete) |
//! | React (emoji or custom image) | [`Channel::react`] · [`react_custom`](Channel::react_custom) |
//! | Send & receive files | [`Channel::send_file`] · [`VectorBot::download_attachment`] / [`save_attachment`](VectorBot::save_attachment) |
//! | Receive messages | [`VectorBot::on_message`] |
//! | Answer typed slash commands (with a `/` picker) | [`VectorBot::command`] → [`CommandBuilder`] |
//! | Receive *everything* (joins, reactions, invites…) | [`VectorBot::on_event`] → match on [`BotEvent`] |
//! | Moderate a community | [`IncomingMessage::member`] → [`Member::kick`] · [`ban`](Member::ban) · [`grant_admin`](Member::grant_admin) |
//! | Manage a community | [`IncomingMessage::community`] / [`VectorBot::community`] → [`Community`] |
//! | Be invitable safely | [`builder().public()`](VectorBotBuilder::public) / [`whitelist(..)`](VectorBotBuilder::whitelist) |
//! | Manage profiles | [`fetch_profile`](VectorBot::fetch_profile) · [`update_profile`](VectorBot::update_profile) · [`block`](VectorBot::block) … |
//! | Anything else | [`bot.core()`](VectorBot::core) → the full [`VectorCore`] facade |
//!
//! ## Receiving: `on_message` vs `on_event`
//!
//! [`on_message`](VectorBot::on_message) is the fast path — one async handler per
//! inbound message, DMs and Community channels alike; a slow handler won't hold up the others.
//! (For slash commands, reach for [`command`](VectorBot::command) rather than matching on
//! `msg.text()` here — see [Commands](#commands) below.)
//!
//! For everything beyond messages, [`on_event`](VectorBot::on_event) delivers the
//! full stream as a [`BotEvent`] you `match` on — `Message`, `MessageUpdate` (a
//! reaction/edit landed), `Delete`, `MemberJoin`, `MemberLeave`, `Typing`,
//! `Invite`, and `Removed` (the bot was kicked/banned):
//!
//! ```no_run
//! # use vector_sdk::{VectorBot, BotEvent};
//! # async fn run(bot: VectorBot) -> vector_sdk::Result<()> {
//! bot.on_event(|bot, event| async move {
//!     match event {
//!         BotEvent::Message(msg) if !msg.is_mine() => { let _ = msg.reply("hi").await; }
//!         BotEvent::MemberJoin { channel_id, npub } => {
//!             let _ = bot.channel(channel_id).send(&format!("welcome {}!", &npub[..12])).await;
//!         }
//!         _ => {}
//!     }
//! }).await?;
//! # Ok(()) }
//! ```
//!
//! ## Commands
//!
//! Don't parse `msg.text()` by hand. Declare a **command** instead: give it a name, a
//! description, and typed arguments, and the SDK publishes a machine-readable manifest for
//! it. Every Vector client then renders a `/` picker listing your command, and offers a
//! typed field per argument — a dropdown for a [`choice`](CommandBuilder::choice), a member
//! picker for a [`user`](CommandBuilder::user), a number field for an [`int`](CommandBuilder::int)
//! — and validates the input *before* the invocation is ever sent. Your handler receives the
//! arguments already parsed and type-checked.
//!
//! ```no_run
//! # use vector_sdk::VectorBot;
//! # async fn run(bot: VectorBot) {
//! bot.command("weather", "Current conditions for a city")
//!     .string("city", "Which city", true)             // required free text
//!     .choice("units", "Temperature units", ["c", "f"], false) // optional dropdown
//!     .run(|ctx| async move {
//!         let city = ctx.str("city").unwrap_or_default();
//!         let units = ctx.str("units").unwrap_or("c");
//!         let _ = ctx.reply(format!("Weather in {city} in °{}…", units.to_uppercase())).await;
//!     });
//! # }
//! ```
//!
//! Argument types: [`string`](CommandBuilder::string), [`int`](CommandBuilder::int),
//! [`number`](CommandBuilder::number), [`flag`](CommandBuilder::flag) (bool),
//! [`user`](CommandBuilder::user) (an npub), and [`choice`](CommandBuilder::choice). Read them
//! back off the [`CommandCtx`] with `ctx.str/int/number/flag(name)`. A matched command runs its
//! handler and never reaches [`on_message`](VectorBot::on_message), so commands and free-form
//! chat coexist; the manifest publishes automatically once the bot starts listening. See the
//! [`slash_command_bot`](https://github.com/VectorPrivacy/Vector/blob/master/crates/vector-sdk/examples/slash_command_bot.rs)
//! example for a full bot.
//!
//! ## Communities
//!
//! When a message comes from a community, you get the sender as a member you can act on
//! directly:
//!
//! ```no_run
//! # use vector_sdk::IncomingMessage;
//! # async fn run(msg: IncomingMessage) -> vector_sdk::Result<()> {
//! if let Some(member) = msg.member() {     // the sender, as a Member of this community
//!     if !member.is_admin() {
//!         member.ban().await?;             // or .kick() / .unban() / .grant_admin()
//!     }
//! }
//! # Ok(()) }
//! ```
//!
//! ## Public vs private bots
//!
//! A bot must accept invites to be useful in communities, but a *private* bot
//! mustn't be spammable into random ones. Set the policy on the builder:
//!
//! ```no_run
//! # use vector_sdk::VectorBot;
//! # async fn run() -> vector_sdk::Result<()> {
//! VectorBot::builder().nsec("nsec1...").public().build().await?;                 // accept from anyone
//! VectorBot::builder().nsec("nsec1...").whitelist(["npub1owner…"]).build().await?; // only these accounts
//! # Ok(()) }
//! ```
//!
//! Auto-accept fires for live invites *and* for ones received while the bot was
//! offline (swept on the next connect), so a restarted bot still joins what it
//! was invited to. The default is [`InvitePolicy::Manual`] — see
//! [`pending_invites`](VectorBot::pending_invites) / [`accept_invite`](VectorBot::accept_invite).
//!
//! ## Staying connected
//!
//! If the bot loses its connection, [`on_message`](VectorBot::on_message) /
//! [`on_event`](VectorBot::on_event) reconnect on their own and catch up on what was
//! missed. Your handler fires for messages that arrive while the
//! bot is running; to read older history, use
//! [`bot.core().get_messages(...)`](VectorCore).
//!
//! ## Identity: bring your own, or let the bot make one
//!
//! Supply a key with [`nsec`](VectorBotBuilder::nsec) / [`mnemonic`](VectorBotBuilder::mnemonic) —
//! or supply nothing, and [`build`](VectorBotBuilder::build) **creates an identity on first run and
//! persists it** (`identity.nsec`) in the bot's data directory, reusing the same one every run after.
//! So a first bot needs zero setup:
//!
//! ```no_run
//! # use vector_sdk::VectorBot;
//! # async fn run() -> vector_sdk::Result<()> {
//! let bot = VectorBot::builder().build().await?; // first run mints + stores an nsec; reused after
//! println!("online as {}", bot.npub());
//! # Ok(()) }
//! ```
//!
//! It never mints a *fresh* key per run — the identity is stable, so the bot keeps its DMs and
//! community memberships across restarts. Running several keyless bots? Give each its own
//! [`data_dir`](VectorBotBuilder::data_dir) so they get distinct identities.
//!
//! ## Single identity per process
//!
//! `vector_core` is built on process-global state, so **one [`VectorBot`] owns
//! the process's identity at a time**. Build one bot per process. (Multiple
//! identities means multiple processes — or [`VectorCore::swap_session`] to
//! switch the active account in place.)
//!
//! ## Reaching deeper
//!
//! Everything not surfaced here — creating communities, reading history, and
//! lower-level controls — is one hop away via [`VectorBot::core`], which hands you
//! the full [`VectorCore`] facade.
//!
//! ## Examples
//!
//! Runnable bots live in [`examples/`](https://github.com/VectorPrivacy/Vector/tree/master/crates/vector-sdk/examples):
//!
//! - **`echo_bot`** — the minimal hello-world; replies to every message.
//! - **`slash_command_bot`** — a `/command` router (`/ping`, `/roll`, `/help`…).
//! - **`ai_bot`** — an LLM chatbot with a typing indicator and threaded replies.
//! - **`moderation_bot`** — welcomes joiners and auto-bans on a word filter.
//! - **`whitelist_bot`** — a private bot that only joins communities it trusts.
//! - **`file_bot`** / **`save_files_bot`** — send a file / receive and decrypt one.
//!
//! ```sh
//! VECTOR_NSEC=nsec1... cargo run -p vector-sdk --example echo_bot
//! ```

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

// Curated re-exports so downstream crates can depend only on `vector_sdk`.
pub use vector_core::{
    self, Attachment, AttachmentFile, CoreConfig, DeleteOutcome, EditEntry, EventEmitter,
    ImageMetadata, InboundEventHandler, LoginResult, Message, NoOpEventHandler, Reaction, Result,
    SendResult, SerializableChat, SiteMetadata, SlimProfile, Status, SyncPriority, VectorCore,
    VectorError,
};

/// What a [`Community::contain_raid`] call actually accomplished.
pub use vector_core::community::v2::service::ContainmentReport;

/// Alias for the SDK's error type.
pub use vector_core::VectorError as Error;

/// Re-exported Nostr primitives, so downstreams can depend only on `vector_sdk`.
pub mod nostr {
    pub use nostr_sdk::prelude::{FromBech32, Keys, PublicKey, SecretKey, ToBech32};
}

// Brings `PublicKey::from_bech32` / `.to_bech32()` into scope for id auto-detection + whitelist
// normalization.
use nostr_sdk::prelude::{FromBech32 as _, ToBech32 as _};

mod commands;
pub use commands::{CommandBuilder, CommandCtx, DISCOVERY_RELAYS};

// ============================================================================
// VectorBot
// ============================================================================

/// How a bot handles inbound Community invites (gift-wrapped invite bundles). Set on the builder
/// with [`public`](VectorBotBuilder::public) / [`whitelist`](VectorBotBuilder::whitelist) /
/// [`invite_policy`](VectorBotBuilder::invite_policy).
#[derive(Clone, Debug)]
pub enum InvitePolicy {
    /// Don't auto-accept — invites are parked for manual handling via
    /// [`VectorBot::pending_invites`] / [`VectorBot::accept_invite`]. (Default.)
    Manual,
    /// A **public** bot: auto-accept Community invites from anyone.
    Public,
    /// A **private** bot: auto-accept invites *only* when the inviter's npub is in this whitelist;
    /// ignore all others. This is what keeps a bot from being spammed into random communities.
    /// Entries must be bech32 `npub1…` (the form inviters are compared as). Prefer the
    /// [`whitelist`](VectorBotBuilder::whitelist) builder, which normalizes hex → bech32 for you.
    Whitelist(Vec<String>),
}

impl InvitePolicy {
    /// Whether an invite from `inviter_npub` should be auto-accepted under this policy.
    fn accepts(&self, inviter_npub: Option<&str>) -> bool {
        match self {
            InvitePolicy::Manual => false,
            InvitePolicy::Public => true,
            InvitePolicy::Whitelist(list) => {
                inviter_npub.is_some_and(|npub| list.iter().any(|w| w == npub))
            }
        }
    }
}

/// A logged-in Vector bot: an identity connected to relays, ready to send and
/// receive. Cheap to [`Clone`] — clones share the same underlying session.
#[derive(Clone)]
pub struct VectorBot {
    core: VectorCore,
    npub: String,
    invite_policy: Arc<InvitePolicy>,
    commands: Arc<commands::CommandRegistry>,
}

impl VectorBot {
    /// Start building a bot. Provide a key with [`VectorBotBuilder::nsec`] (or
    /// [`mnemonic`](VectorBotBuilder::mnemonic)), then call
    /// [`build`](VectorBotBuilder::build).
    pub fn builder() -> VectorBotBuilder {
        VectorBotBuilder::default()
    }

    /// Generate a fresh random account secret key (bech32 `nsec`). Handy for
    /// spinning up a brand-new bot identity.
    pub fn generate_nsec() -> Result<String> {
        VectorCore.generate_nsec()
    }

    /// This bot's own npub (bech32).
    pub fn npub(&self) -> &str {
        &self.npub
    }

    /// The underlying [`VectorCore`] facade, for operations not surfaced
    /// ergonomically here (communities, `sync_dms`, custom rumors, etc.).
    pub fn core(&self) -> VectorCore {
        self.core
    }

    pub(crate) fn commands(&self) -> &commands::CommandRegistry {
        &self.commands
    }

    /// Whether the realtime Community subscription has REGISTERED for this
    /// session — the pollable twin of [`BotEvent::Ready`], for health checks
    /// from outside the event handler. `false` only during the brief cold-start
    /// registration (a bounded relay-connect wait), before which the bot is deaf
    /// to live community traffic.
    pub fn subscription_ready(&self) -> bool {
        vector_core::community::v2::realtime::subscription_ready()
    }

    /// This bot's invite policy (see [`InvitePolicy`]).
    pub fn invite_policy(&self) -> &InvitePolicy {
        &self.invite_policy
    }

    /// Parked Community invites awaiting a decision — each `{ community_id, name, inviter_npub }`.
    /// (Auto-accepted invites are already gone; these are the ones held under
    /// [`InvitePolicy::Manual`] or rejected by a whitelist.)
    pub fn pending_invites(&self) -> Result<Vec<serde_json::Value>> {
        self.core.list_pending_invites()
    }

    /// Accept a parked Community invite by id, then start receiving its channels.
    pub async fn accept_invite(&self, community_id: &str) -> Result<serde_json::Value> {
        let res = self.core.accept_pending_invite(community_id).await?;
        // The realtime sub was built without this community; refresh so its channels flow in.
        if let Some(client) = vector_core::state::nostr_client() {
            vector_core::community::realtime::refresh_subscription(&client).await;
        }
        Ok(res)
    }

    /// Apply the invite policy to every currently-parked invite — auto-joining the ones it allows.
    /// Called at `on_message` startup (so a restarted bot picks up invites received while it was
    /// down); also safe to call manually. No-op under [`InvitePolicy::Manual`].
    pub async fn process_pending_invites(&self) {
        if matches!(*self.invite_policy, InvitePolicy::Manual) {
            return;
        }
        let Ok(invites) = self.core.list_pending_invites() else { return };
        for inv in invites {
            let Some(cid) = inv.get("community_id").and_then(|c| c.as_str()) else { continue };
            let inviter = inv.get("inviter_npub").and_then(|n| n.as_str());
            if self.invite_policy.accepts(inviter) {
                let _ = self.accept_invite(cid).await;
            }
        }
    }

    /// Apply [`invite_policy`](Self::invite_policy) to a just-arrived invite: auto-accept it when the
    /// policy allows (and the inviter passes a whitelist), otherwise leave it parked. Invoked
    /// automatically by the `on_message` listen loop; no-op under [`InvitePolicy::Manual`]. Exposed
    /// so a custom [`listen_with`](Self::listen_with) handler can opt into the same policy.
    pub async fn apply_invite_policy(&self, community_id: &str) {
        if matches!(*self.invite_policy, InvitePolicy::Manual) {
            return;
        }
        // Resolve the inviter from the parked record (needed for the whitelist check).
        let inviter = self
            .core
            .list_pending_invites()
            .ok()
            .and_then(|invites| {
                invites.into_iter().find_map(|i| {
                    (i.get("community_id").and_then(|c| c.as_str()) == Some(community_id))
                        .then(|| i.get("inviter_npub").and_then(|n| n.as_str()).map(String::from))
                        .flatten()
                })
            });
        if self.invite_policy.accepts(inviter.as_deref()) {
            let _ = self.accept_invite(community_id).await;
        }
    }

    /// A unified messaging handle for a chat or channel, **auto-detecting** whether `id` is a DM
    /// (an `npub`) or a Community channel (a 64-char hex channel id). Send and receive work the
    /// same way regardless — you never branch on the transport. Infallible; an invalid id surfaces
    /// as an error when you actually send.
    pub fn channel(&self, id: impl Into<String>) -> Channel {
        let id = id.into();
        let kind = channel_kind_for(&id);
        Channel { core: self.core, id, kind }
    }

    /// An explicit DM handle for an `npub` (skips auto-detection).
    pub fn dm(&self, npub: impl Into<String>) -> Channel {
        Channel { core: self.core, id: npub.into(), kind: ChannelKind::Dm }
    }

    /// A [`Community`] handle by its community id, for management (members, invites, roles,
    /// metadata). To *message* a community channel, use [`channel`](Self::channel) with the
    /// channel id instead.
    pub fn community(&self, community_id: impl Into<String>) -> Community {
        Community { core: self.core, id: community_id.into() }
    }

    /// Every Community this bot is a member of.
    pub async fn communities(&self) -> Vec<Community> {
        self.core
            .list_communities()
            .await
            .into_iter()
            .filter_map(|v| {
                v.get("community_id")
                    .or_else(|| v.get("id"))
                    .and_then(|i| i.as_str())
                    .map(|id| self.community(id.to_string()))
            })
            .collect()
    }

    // ---- receiving ----

    /// Register an async message handler and block, processing inbound DMs and
    /// file attachments until the client disconnects. The handler is invoked
    /// once per message with a clone of the bot (so it can reply) and an
    /// [`IncomingMessage`]. A slow handler won't hold up other messages.
    ///
    /// ```no_run
    /// # use vector_sdk::VectorBot;
    /// # async fn run(bot: VectorBot) -> vector_sdk::Result<()> {
    /// bot.on_message(|_bot, msg| async move {
    ///     if msg.is_mine() { return; } // ignore our own echoes
    ///     // `reply` works the same for DMs and Community channels.
    ///     let _ = msg.reply(&format!("You said: {}", msg.text())).await;
    /// }).await?;
    /// # Ok(()) }
    /// ```
    pub async fn on_message<F, Fut>(&self, handler: F) -> Result<()>
    where
        F: Fn(VectorBot, IncomingMessage) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.prepare_listen().await;
        let adapter = ClosureHandler {
            bot: self.clone(),
            handler: Arc::new(handler),
        };
        self.core.listen(Arc::new(adapter)).await
    }

    /// Register an async handler for **every** kind of inbound event — messages, reactions/edits,
    /// deletes, member join/leave, typing, invites, and being removed — and block until disconnect.
    /// Match on [`BotEvent`]; ignore the variants you don't care about. A superset of
    /// [`on_message`](Self::on_message) (use that if you only want messages).
    ///
    /// ```no_run
    /// # use vector_sdk::{VectorBot, BotEvent};
    /// # async fn run(bot: VectorBot) -> vector_sdk::Result<()> {
    /// bot.on_event(|bot, event| async move {
    ///     match event {
    ///         BotEvent::Message(msg) if !msg.is_mine() => { let _ = msg.reply("hi").await; }
    ///         BotEvent::MemberJoin { channel_id, npub } => {
    ///             let _ = bot.channel(channel_id).send(&format!("welcome {}!", &npub[..12])).await;
    ///         }
    ///         _ => {}
    ///     }
    /// }).await?;
    /// # Ok(()) }
    /// ```
    pub async fn on_event<F, Fut>(&self, handler: F) -> Result<()>
    where
        F: Fn(VectorBot, BotEvent) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.prepare_listen().await;
        let adapter = EventClosureHandler {
            bot: self.clone(),
            handler: Arc::new(handler),
        };
        self.core.listen(Arc::new(adapter)).await
    }

    /// Shared listen startup: catch up DMs FIRST so any invite delivered while offline is parked,
    /// THEN apply the invite policy to everything parked (so a restarted private bot still auto-joins
    /// communities it was invited to). Live invites are handled by the event adapters. Registered
    /// slash commands publish their interface manifest here so pickers can discover them.
    async fn prepare_listen(&self) {
        let _ = self.core.sync_dms(None, &NoOpEventHandler).await;
        self.process_pending_invites().await;
        self.publish_interface_manifest().await;
    }

    /// Escape hatch: drive the receive loop with a custom
    /// [`InboundEventHandler`] for full control over every event kind.
    pub async fn listen_with(&self, handler: Arc<dyn InboundEventHandler>) -> Result<()> {
        self.core.listen(handler).await
    }

    /// Backfill historical DMs via NIP-77 negentropy set reconciliation.
    /// Returns `(events_processed, new_messages)`. Pass `Some(days)` to limit
    /// the window, or `None` for a full sync.
    pub async fn sync_dms(&self, since_days: Option<u64>) -> Result<(u32, u32)> {
        self.core.sync_dms(since_days, &NoOpEventHandler).await
    }

    /// Catch up every Community this bot is in — refold consensus (re-foundings / rekeys / banlist /
    /// metadata) and fetch recent messages into local state. Runs automatically inside
    /// [`on_message`](Self::on_message)/`listen` on connect and periodically for outage resilience;
    /// exposed for manual use (e.g. right after a known reconnect).
    pub async fn sync_communities(&self) -> Result<()> {
        self.core.sync_communities().await
    }

    // ---- profiles ----

    /// Fetch a profile from relays and return the merged result. Returns `None`
    /// if nothing could be resolved.
    pub async fn fetch_profile(&self, npub: &str) -> Option<SlimProfile> {
        self.core.load_profile(npub).await;
        self.core.get_profile(npub).await
    }

    /// Read a profile already in local state without hitting the network.
    pub async fn cached_profile(&self, npub: &str) -> Option<SlimProfile> {
        self.core.get_profile(npub).await
    }

    /// Update this bot's own profile metadata (broadcasts a kind-0 event). The profile is always
    /// tagged `bot: true` so clients can badge it as a bot — that's the whole point of the SDK. If
    /// you're building a human client, use [`vector_core`]'s `update_profile` directly instead.
    pub async fn update_profile(&self, name: &str, avatar: &str, banner: &str, about: &str) -> bool {
        self.core.update_bot_profile(name, avatar, banner, about).await
    }

    /// Set this bot's status (kind-30315).
    pub async fn set_status(&self, status: &str) -> bool {
        self.core.update_status(status).await
    }

    /// Block a user (adds them to the mute list).
    pub async fn block(&self, npub: &str) -> bool {
        self.core.block_user(npub).await
    }

    /// Unblock a previously blocked user.
    pub async fn unblock(&self, npub: &str) -> bool {
        self.core.unblock_user(npub).await
    }

    /// Set a local-only nickname for a user (never broadcast).
    pub async fn set_nickname(&self, npub: &str, nickname: &str) -> bool {
        self.core.set_nickname(npub, nickname).await
    }

    /// List all blocked users.
    pub async fn blocked_users(&self) -> Vec<SlimProfile> {
        self.core.get_blocked_users().await
    }

    // ---- attachments ----

    /// Download a received attachment and decrypt it to plaintext bytes (fetches the encrypted blob
    /// from its Blossom URL, then AES-decrypts with the attachment's embedded key + nonce). Find
    /// attachments on `msg.message.attachments`. Prefer
    /// [`download_attachment_from`](Self::download_attachment_from) when you have the message —
    /// knowing the author unlocks an extra recovery path for dead links.
    pub async fn download_attachment(&self, attachment: &Attachment) -> Result<Vec<u8>> {
        self.core.download_attachment(attachment).await
    }

    /// [`download_attachment`](Self::download_attachment) with the full source walk: the primary
    /// URL, then any mirrors the sender embedded, then the same content-address on each server the
    /// author advertises (BUD-03) — pass `msg.message.npub` as `author_npub`.
    pub async fn download_attachment_from(
        &self,
        attachment: &Attachment,
        author_npub: Option<&str>,
    ) -> Result<Vec<u8>> {
        self.core.download_attachment_from(attachment, author_npub).await
    }

    /// Upload a local image (avatar, banner, …) to Blossom and return its public URL. Unlike
    /// [`send_file`](Channel::send_file)'s encrypted attachments, this is uploaded in the clear so
    /// other clients can fetch it directly — pass the URL to [`update_profile`](Self::update_profile).
    pub async fn upload_image(&self, path: impl AsRef<std::path::Path>) -> Result<String> {
        let path = path.as_ref().to_string_lossy().into_owned();
        self.core.upload_public_image(&path).await
    }

    /// Download a received attachment and write the decrypted bytes to `path`. Returns the path.
    pub async fn save_attachment(&self, attachment: &Attachment, path: impl Into<PathBuf>) -> Result<PathBuf> {
        let path = path.into();
        let bytes = self.core.download_attachment(attachment).await?;
        std::fs::write(&path, bytes).map_err(VectorError::Io)?;
        Ok(path)
    }

    // ---- lifecycle ----

    /// Disconnect from relays and close the local database.
    pub async fn logout(&self) {
        self.core.logout().await
    }
}

// ============================================================================
// Builder
// ============================================================================

/// Builder for a [`VectorBot`]. Created via [`VectorBot::builder`].
#[derive(Default)]
pub struct VectorBotBuilder {
    key: Option<String>,
    password: Option<String>,
    data_dir: Option<PathBuf>,
    event_emitter: Option<Box<dyn EventEmitter>>,
    invite_policy: Option<InvitePolicy>,
    #[cfg(feature = "tor")]
    tor: bool,
    #[cfg(feature = "tor")]
    tor_bridges: Vec<String>,
}

impl VectorBotBuilder {
    /// Set the account key: an `nsec1…` secret key **or** a BIP-39 mnemonic
    /// phrase. Equivalent to [`nsec`](Self::nsec) / [`mnemonic`](Self::mnemonic).
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Set the account's `nsec1…` secret key.
    pub fn nsec(self, nsec: impl Into<String>) -> Self {
        self.key(nsec)
    }

    /// Set the account from a BIP-39 mnemonic seed phrase (NIP-06).
    pub fn mnemonic(self, phrase: impl Into<String>) -> Self {
        self.key(phrase)
    }

    /// Provide the password/PIN for an encrypted-at-rest account.
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// Set the Community invite policy explicitly (see [`InvitePolicy`]). Defaults to
    /// [`InvitePolicy::Manual`].
    pub fn invite_policy(mut self, policy: InvitePolicy) -> Self {
        self.invite_policy = Some(policy);
        self
    }

    /// Make this a **public** bot — auto-accept Community invites from anyone.
    /// Shorthand for [`invite_policy(InvitePolicy::Public)`](Self::invite_policy).
    pub fn public(self) -> Self {
        self.invite_policy(InvitePolicy::Public)
    }

    /// Make this a **private** bot — auto-accept invites *only* from these pubkeys, ignoring all
    /// others. Accepts `npub1…` or hex; each is normalized to bech32 (un-parseable entries are
    /// dropped) so the whitelist always matches the inviter form the SDK compares against.
    /// Shorthand for [`invite_policy(InvitePolicy::Whitelist(..))`](Self::invite_policy).
    pub fn whitelist(self, npubs: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let normalized = npubs
            .into_iter()
            .filter_map(|n| {
                let s = n.into();
                nostr_sdk::prelude::PublicKey::parse(&s).ok().and_then(|pk| pk.to_bech32().ok())
            })
            .collect();
        self.invite_policy(InvitePolicy::Whitelist(normalized))
    }

    /// Override the data directory (SQLite DB + per-account storage). Defaults
    /// to a per-OS application directory.
    pub fn data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.data_dir = Some(dir.into());
        self
    }

    /// Plug in a custom [`EventEmitter`] to bridge core events into your app or
    /// logs. Optional — defaults to a no-op.
    pub fn event_emitter(mut self, emitter: Box<dyn EventEmitter>) -> Self {
        self.event_emitter = Some(emitter);
        self
    }

    /// Route **all** of this bot's traffic through embedded Tor. Requires the `tor` feature.
    ///
    /// Tor is started and bootstrapped during [`build`](Self::build) *before* the bot connects,
    /// so the bot never touches the network in the clear. Bootstrapping can take several seconds.
    #[cfg(feature = "tor")]
    pub fn tor(mut self) -> Self {
        self.tor = true;
        self
    }

    /// Like [`tor`](Self::tor), but route through the given Tor **bridges** instead of public
    /// entry relays — for networks where Tor itself is blocked. Each entry is a bridge line
    /// (e.g. `"1.2.3.4:443 <fingerprint>"`). Implies [`tor`](Self::tor).
    #[cfg(feature = "tor")]
    pub fn tor_bridges(mut self, bridges: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tor = true;
        self.tor_bridges = bridges.into_iter().map(Into::into).collect();
        self
    }

    /// Initialize core, resolve the identity, log in, and connect to relays.
    ///
    /// If no key was supplied via [`nsec`](Self::nsec) / [`mnemonic`](Self::mnemonic), the bot loads
    /// — or, on first run, **creates and persists** — an identity (`identity.nsec`) in its data
    /// directory, so it keeps the same npub across restarts. An explicit key always takes precedence.
    pub async fn build(self) -> Result<VectorBot> {
        let data_dir = self.data_dir.unwrap_or_else(default_data_dir);
        std::fs::create_dir_all(&data_dir).ok();

        let core = VectorCore::init(CoreConfig {
            data_dir: data_dir.clone(),
            event_emitter: self.event_emitter,
        })?;

        // An explicit key wins; otherwise load — or, on first run, create — a persistent identity.
        let (key, fresh_identity) = match self.key {
            Some(key) => (key, None),
            None => {
                let (nsec, path, created) = load_or_create_identity(core, &data_dir)?;
                (nsec, created.then_some(path))
            }
        };

        // Bring Tor up BEFORE login connects: prime this account's DB with the Tor preference and
        // start the service, so login's own relay connect already routes through Tor (no clear-net
        // handshake). login re-reads the same DB setting, so the preference sticks.
        #[cfg(feature = "tor")]
        if self.tor {
            use nostr_sdk::prelude::*;
            let keys = if key.starts_with("nsec1") {
                Keys::new(
                    SecretKey::from_bech32(&key)
                        .map_err(|e| VectorError::Other(format!("invalid nsec: {e}")))?,
                )
            } else {
                Keys::from_mnemonic(&key, None)
                    .map_err(|e| VectorError::Other(format!("invalid mnemonic: {e}")))?
            };
            let npub = keys.public_key().to_bech32().map_err(|e| VectorError::Other(e.to_string()))?;

            vector_core::db::set_current_account(npub.clone()).map_err(VectorError::Other)?;
            vector_core::db::init_database(&npub).map_err(VectorError::Other)?;
            vector_core::db::settings::set_sql_setting("tor_enabled".to_string(), "1".to_string())
                .map_err(VectorError::Other)?;
            vector_core::tor::set_tor_enabled_pref(true);

            let tor_dir = vector_core::db::account_dir(&npub).map_err(VectorError::Other)?.join("tor");
            let (state_dir, cache_dir) = (tor_dir.join("state"), tor_dir.join("cache"));
            std::fs::create_dir_all(&state_dir).ok();
            std::fs::create_dir_all(&cache_dir).ok();

            // Fail closed: blackhole the shared client until bootstrap finishes, then start + rebuild.
            vector_core::net::rebuild_shared_http_client().map_err(VectorError::Other)?;
            vector_core::tor::TorService::start(state_dir, cache_dir, &self.tor_bridges)
                .await
                .map_err(VectorError::Other)?;
            vector_core::net::rebuild_shared_http_client().map_err(VectorError::Other)?;
        }

        let result = core.login(&key, self.password.as_deref()).await?;

        // One-time provisioning notice — the only thing the SDK writes to stderr.
        if let Some(path) = fresh_identity {
            eprintln!(
                "[vector-sdk] Created a new bot identity {} (stored at {}). \
                 Back it up — that file is the bot.",
                result.npub,
                path.display()
            );
        }

        Ok(VectorBot {
            core,
            npub: result.npub,
            invite_policy: Arc::new(self.invite_policy.unwrap_or(InvitePolicy::Manual)),
            commands: Arc::new(Default::default()),
        })
    }
}

// ============================================================================
// Channel — unified DM + Community messaging handle
// ============================================================================

/// Whether a [`Channel`] targets a direct message or a Community channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelKind {
    /// A 1:1 direct message, addressed by the recipient's `npub`.
    Dm,
    /// A Community channel, addressed by its channel id.
    Community,
}

/// A unified handle for a chat or channel — **a DM and a Community channel behave the same**. Every
/// method routes to the right transport under the hood, so a bot author never branches on DM-vs-
/// channel. Obtained from [`VectorBot::channel`] / [`dm`](VectorBot::dm) /
/// [`community`](VectorBot::community), or [`IncomingMessage::channel`].
#[derive(Clone)]
pub struct Channel {
    core: VectorCore,
    id: String,
    kind: ChannelKind,
}

/// A durable position in a channel's history, for paging and for building your
/// own sync mechanism on top of [`Channel::history_before`] / [`Channel::sync_before`].
///
/// Ordered by `(at_ms, id)` and compared BY VALUE, so it keeps working after its
/// message is gone (deletes, moderation hides, self-destruct timers) and stays
/// unambiguous inside a same-millisecond burst.
///
/// Persist it however you like: it is serde-serializable, and its string form is
/// **wire-stable** — `"<at_ms>:<message id hex>"` — parse it back with
/// [`FromStr`](std::str::FromStr). A cursor is per-channel; store it keyed by the
/// channel id.
///
/// ```no_run
/// # use vector_sdk::Cursor;
/// # fn f(msgs: &[vector_sdk::Message]) -> Result<(), Box<dyn std::error::Error>> {
/// let cursor = Cursor::of(&msgs[0]);
/// let saved = cursor.to_string();          // "1785979414499:0bcd2059e0b8..."
/// let restored: Cursor = saved.parse()?;   // ...after a restart
/// # Ok(()) }
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct Cursor {
    /// Message timestamp, unix **milliseconds**.
    pub at_ms: u64,
    /// Message id (64-char hex). Tiebreak within a millisecond.
    pub id: String,
}

impl Cursor {
    /// The cursor at `message`'s position.
    pub fn of(message: &Message) -> Self {
        Self { at_ms: message.at, id: message.id.clone() }
    }
}

impl From<&Message> for Cursor {
    fn from(m: &Message) -> Self {
        Self::of(m)
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.at_ms, self.id)
    }
}

impl std::str::FromStr for Cursor {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        let (at, id) = s.split_once(':').ok_or("cursor is '<at_ms>:<message id hex>'")?;
        let at_ms: u64 = at.parse().map_err(|_| "cursor at_ms is not a number".to_string())?;
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("cursor id is not 64-char hex".to_string());
        }
        Ok(Self { at_ms, id: id.to_ascii_lowercase() })
    }
}

impl Channel {
    /// The id of this chat or channel — an `npub` for a DM, a channel id for a Community channel.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether this is a DM or a Community channel.
    pub fn kind(&self) -> ChannelKind {
        self.kind
    }

    /// `true` for a direct message.
    pub fn is_dm(&self) -> bool {
        matches!(self.kind, ChannelKind::Dm)
    }

    /// `true` for a Community channel.
    pub fn is_community(&self) -> bool {
        matches!(self.kind, ChannelKind::Community)
    }

    /// The newest `limit` messages this bot holds LOCALLY, chronological.
    ///
    /// Read-only and never touches the network: it answers from the bot's own
    /// store, which live delivery, back-fills and [`Channel::sync`] keep fed.
    /// This is how a bot reads what a `ChannelKeyed` back-fill fetched — those
    /// messages are history, not delivery, and never arrive as live events.
    /// Unit is MESSAGES ([`Channel::sync`]'s unit is events).
    pub async fn history(&self, limit: usize) -> Vec<Message> {
        self.core.get_messages_before(&self.id, None, limit).await
    }

    /// Up to `limit` messages strictly before `cursor`, chronological — the
    /// paging form of [`Channel::history`]. The cursor is compared by value, so
    /// it still works after its message was deleted. Page backwards by looping:
    /// next cursor = [`Cursor::of`] the first message of the page you just got.
    pub async fn history_before(&self, cursor: &Cursor, limit: usize) -> Vec<Message> {
        self.core.get_messages_before(&self.id, Some((cursor.at_ms, &cursor.id)), limit).await
    }

    /// Fetch ONE page of up to `max_events` events (per relay) from this
    /// Community channel's relays, ingest it locally, and return how many
    /// MESSAGES were new. `0` means the page held nothing new — the walk's
    /// natural stop — not that the channel is empty.
    ///
    /// The unit is EVENTS: reactions, edits, deletes and presence share the
    /// plane and spend the budget, so a page rarely yields `max_events` new
    /// messages. Clamped to 500; depth comes from looping with
    /// [`Channel::sync_before`], not from a bigger page. Read what landed with
    /// [`Channel::history`]. Community channels only — DM recovery is
    /// [`VectorBot::sync_dms`].
    pub async fn sync(&self, max_events: usize) -> Result<usize> {
        self.sync_page(max_events, None, None).await
    }

    /// [`Channel::sync`] for the page of events at and before `cursor` — the
    /// relay-side half of a walk-until-dry loop:
    ///
    /// ```no_run
    /// # async fn f(channel: &vector_sdk::Channel) -> Result<(), Box<dyn std::error::Error>> {
    /// let mut cursor: Option<vector_sdk::Cursor> = None;
    /// loop {
    ///     let page = match &cursor {
    ///         Some(c) => channel.history_before(c, 100).await,
    ///         None => channel.history(100).await,
    ///     };
    ///     if page.is_empty() {
    ///         let fetched = match &cursor {
    ///             Some(c) => channel.sync_before(c, 200).await?,
    ///             None => channel.sync(200).await?,
    ///         };
    ///         if fetched == 0 { break; } // relays dry too: true start of history
    ///         continue;
    ///     }
    ///     // ...process(&page)...
    ///     cursor = Some(vector_sdk::Cursor::of(&page[0]));
    /// }
    /// # Ok(()) }
    /// ```
    pub async fn sync_before(&self, cursor: &Cursor, max_events: usize) -> Result<usize> {
        // Relay filters are seconds-granular; +1 keeps the cursor's own second
        // covered (ingest dedups the overlap — duplicates are safe, skips are not).
        self.sync_page(max_events, Some(cursor.at_ms / 1000 + 1), None).await
    }

    /// [`Channel::sync`] bounded to events at or after `since_ms` (unix ms) —
    /// "pull the last N days" for time-based processors.
    pub async fn sync_since(&self, since_ms: u64, max_events: usize) -> Result<usize> {
        self.sync_page(max_events, None, Some(since_ms / 1000)).await
    }

    async fn sync_page(&self, max_events: usize, until_s: Option<u64>, since_s: Option<u64>) -> Result<usize> {
        if self.is_dm() {
            return Err(Error::Other(
                "DM history rides the DM reconcile — use bot.sync_dms(), then Channel::history reads it".into(),
            ));
        }
        self.core.sync_channel_events(&self.id, max_events, until_s, since_s).await
    }

    /// Send a text message. Returns the new message's event id.
    pub async fn send(&self, text: &str) -> Result<String> {
        match self.kind {
            ChannelKind::Dm => self
                .core
                .send_dm(&self.id, text)
                .await
                .map(|r| r.event_id.unwrap_or(r.pending_id)),
            ChannelKind::Community => self.core.send_community_message(&self.id, text, None).await,
        }
    }

    /// Send a text message as a **threaded reply** to `replied_to` (an existing message's id).
    /// Works for DMs and Community channels. Returns the new message's event id.
    pub async fn reply(&self, replied_to: &str, text: &str) -> Result<String> {
        match self.kind {
            ChannelKind::Dm => self
                .core
                .send_dm_reply(&self.id, replied_to, text)
                .await
                .map(|r| r.event_id.unwrap_or(r.pending_id)),
            ChannelKind::Community => {
                self.core.send_community_message(&self.id, text, Some(replied_to)).await
            }
        }
    }

    /// React to a message with a unicode emoji (e.g. `"👍"`).
    pub async fn react(&self, message_id: &str, emoji: &str) -> Result<()> {
        match self.kind {
            ChannelKind::Dm => self.core.send_reaction(&self.id, message_id, emoji, None).await.map(|_| ()),
            ChannelKind::Community => self.core.send_community_reaction(&self.id, message_id, emoji, None).await,
        }
    }

    /// React with a custom NIP-30 pack emoji: a `:shortcode:` plus its image URL.
    pub async fn react_custom(&self, message_id: &str, shortcode_emoji: &str, image_url: &str) -> Result<()> {
        match self.kind {
            ChannelKind::Dm => self.core.send_reaction(&self.id, message_id, shortcode_emoji, Some(image_url)).await.map(|_| ()),
            ChannelKind::Community => self.core.send_community_reaction(&self.id, message_id, shortcode_emoji, Some(image_url)).await,
        }
    }

    /// Send an ephemeral typing indicator. Useful while the bot is "thinking".
    pub async fn typing(&self) -> Result<()> {
        match self.kind {
            ChannelKind::Dm => self.core.send_typing(&self.id).await,
            ChannelKind::Community => self.core.send_community_typing(&self.id).await,
        }
    }

    /// Edit a message you previously sent.
    pub async fn edit(&self, message_id: &str, new_content: &str) -> Result<()> {
        match self.kind {
            ChannelKind::Dm => self.core.edit_dm(&self.id, message_id, new_content).await.map(|_| ()),
            ChannelKind::Community => self.core.edit_community_message(&self.id, message_id, new_content).await,
        }
    }

    /// Delete a message you sent.
    pub async fn delete(&self, message_id: &str) -> Result<()> {
        match self.kind {
            ChannelKind::Dm => self.core.delete_dm(message_id).await.map(|_| ()),
            ChannelKind::Community => self.core.delete_community_message(message_id).await,
        }
    }

    /// Send a file from disk as an encrypted attachment — works for DMs and Community channels.
    pub async fn send_file(&self, path: impl AsRef<std::path::Path>) -> Result<String> {
        let path = path.as_ref().to_string_lossy().into_owned();
        match self.kind {
            ChannelKind::Dm => self
                .core
                .send_file(&self.id, &path)
                .await
                .map(|r| r.event_id.unwrap_or(r.pending_id)),
            ChannelKind::Community => self.core.send_community_file(&self.id, &path).await,
        }
    }
}

// ============================================================================
// Inbound message handling
// ============================================================================

/// An inbound message delivered to an [`VectorBot::on_message`] handler. The same handler receives
/// both DMs and Community channel messages — use [`reply`](Self::reply) / [`channel`](Self::channel)
/// to respond uniformly without caring which it is.
#[derive(Clone, Debug)]
pub struct IncomingMessage {
    /// The chat or channel id. For a DM this is the sender's npub; for a Community message it's the
    /// channel id. Prefer [`reply`](Self::reply) / [`channel`](Self::channel) over using it directly.
    pub chat_id: String,
    /// `true` when this message arrived in a Community channel rather than a DM.
    pub is_group: bool,
    /// `true` when this message carries a file attachment.
    pub is_file: bool,
    /// The full message: content, attachments, reactions, timestamps, and the
    /// `mine` flag (true for the bot's own messages).
    pub message: Message,
}

impl IncomingMessage {
    /// The [`Channel`] this message arrived on — reply, react, or type into it uniformly,
    /// regardless of whether it's a DM or a Community channel.
    pub fn channel(&self) -> Channel {
        Channel {
            core: VectorCore,
            id: self.chat_id.clone(),
            kind: if self.is_group { ChannelKind::Community } else { ChannelKind::Dm },
        }
    }

    /// Respond as a **threaded reply** to this message — the response references it, so clients
    /// render it as a reply. Works identically for DMs and Community channels. (For a plain,
    /// non-threaded response in the same chat or channel, use `msg.channel().send(...)`.)
    pub async fn reply(&self, text: &str) -> Result<String> {
        self.channel().reply(&self.message.id, text).await
    }

    /// React to *this* message with an emoji.
    pub async fn react(&self, emoji: &str) -> Result<()> {
        self.channel().react(&self.message.id, emoji).await
    }

    /// The [`Community`] this message belongs to — `None` for DMs. Use it for community-level
    /// management (invites, roles, metadata).
    pub fn community(&self) -> Option<Community> {
        if !self.is_group {
            return None;
        }
        let community_id = vector_core::db::community::community_id_for_channel(&self.chat_id)
            .ok()
            .flatten()?;
        Some(Community { core: VectorCore, id: community_id })
    }

    /// The sender as a [`Member`] of this community — `None` for DMs or if the sender is unknown.
    /// Act on them directly: `msg.member()?.kick().await`, `.ban()`, `.grant_admin()`, etc.
    pub fn member(&self) -> Option<Member> {
        let community = self.community()?;
        let npub = self.message.npub.clone()?;
        Some(Member { core: VectorCore, community_id: community.id, npub })
    }

    /// The message text.
    pub fn text(&self) -> &str {
        &self.message.content
    }

    /// `true` if this is the bot's own message (e.g. its own echo).
    pub fn is_mine(&self) -> bool {
        self.message.mine
    }
}

// ============================================================================
// Community + Member — object model for community management
// ============================================================================

/// A handle to a Community for management — members, invites, roles, metadata. Obtained from
/// [`VectorBot::community`], [`VectorBot::communities`], or [`IncomingMessage::community`]. To
/// *message* a channel within it, use a [`Channel`] (`bot.channel(channel_id)`).
#[derive(Clone)]
pub struct Community {
    core: VectorCore,
    id: String,
}

impl Community {
    /// The community id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// A handle to a member of this community by npub — act on them directly.
    pub fn member(&self, npub: impl Into<String>) -> Member {
        Member { core: self.core, community_id: self.id.clone(), npub: npub.into() }
    }

    /// Ban several members as ONE moderation unit: one banlist edition, one
    /// grant-strip pass, and (in a private community) ONE key rotation — not one
    /// per target. Prefer this over looping [`Member::ban`] when moderating in
    /// bulk; N single bans cost N rotations and churn every reader N times.
    ///
    /// Additive: targets are appended to the existing folded banlist. The wire
    /// caps a banlist at 500 entries, so a batch that would exceed it fails
    /// before anything publishes.
    ///
    /// ```no_run
    /// # async fn f(community: vector_sdk::Community) -> vector_sdk::Result<()> {
    /// community.ban_many(&[
    ///     "npub1spammer…",
    ///     "npub1alsospam…",
    /// ]).await?;
    /// # Ok(()) }
    /// ```
    pub async fn ban_many(&self, npubs: &[&str]) -> Result<()> {
        self.core.set_members_banned(&self.id, npubs, true).await
    }

    /// Contain a raid: ban the raiders, cut everyone who walked in through the
    /// invite link during the raid, and roll the community root with a **severing
    /// refounding** — the rotation that revokes the public invite links instead of
    /// carrying them into the new epoch.
    ///
    /// This is the difference between a ban and a containment. [`ban_many`] in a
    /// public community silences the accounts you name and leaves the door open,
    /// which is correct for spammers and useless against a raid: the next wave
    /// walks in through the same link with fresh keys. Here the door closes with
    /// the rotation, and only a fresh link deliberately minted by a creator
    /// re-opens it.
    ///
    /// `window_start_secs` is when the raid began (unix seconds). Members who
    /// joined at-or-after it are cut from the new epoch even if they never posted
    /// — a raid arrives *through* the door, and a lurker who slipped in at exactly
    /// the wrong moment can rejoin later. They are cut, not banned. Pass `0` to cut
    /// only the accounts you name.
    ///
    /// Requires a Concord v2 community and the `BAN` permission. The returned
    /// report is honest about partial failure: check
    /// [`refound_ok`](ContainmentReport::refound_ok)
    /// — when it is false the raiders are silenced but the door may still be open,
    /// which is a human's problem to hear about.
    ///
    /// ```no_run
    /// # async fn f(community: vector_sdk::Community, raid_began: u64) -> vector_sdk::Result<()> {
    /// let report = community.contain_raid(&["npub1raider…", "npub1raider2…"], raid_began).await?;
    /// if !report.refound_ok {
    ///     // Tell someone: the community is still reachable through the old link.
    /// }
    /// # Ok(()) }
    /// ```
    pub async fn contain_raid(
        &self,
        npubs: &[&str],
        window_start_secs: u64,
    ) -> Result<ContainmentReport> {
        self.core.contain_raid(&self.id, npubs, window_start_secs, &[]).await
    }

    /// [`contain_raid`](Self::contain_raid), plus an explicit extra cut list.
    ///
    /// The window cut works from Guestbook Join timestamps, which are claimed by
    /// their AUTHORS — a quiet raider who backdated their Join reads as
    /// established and slips through. A caller that watched the accounts arrive
    /// on its own clock names them in `cut_also`: they are cut from the new
    /// epoch (not banned) no matter what their Join claims.
    pub async fn contain_raid_with(
        &self,
        npubs: &[&str],
        window_start_secs: u64,
        cut_also: &[&str],
    ) -> Result<ContainmentReport> {
        self.core.contain_raid(&self.id, npubs, window_start_secs, cut_also).await
    }

    /// Lift several bans as one unit — the reductive mirror of
    /// [`ban_many`](Self::ban_many): one banlist edition removing every target,
    /// no rotation.
    pub async fn unban_many(&self, npubs: &[&str]) -> Result<()> {
        self.core.set_members_banned(&self.id, npubs, false).await
    }

    /// Every channel in this community, **including private ones this bot cannot
    /// read yet**.
    ///
    /// A private channel you have been told about but not yet handed the key for
    /// is listed with [`is_readable`](CommunityChannel::is_readable) false. That
    /// is the difference between "not a member" and "a member still waiting on
    /// the key", which is otherwise indistinguishable from silence.
    ///
    /// ```no_run
    /// # use vector_sdk::VectorBot;
    /// # async fn run(bot: VectorBot) -> vector_sdk::Result<()> {
    /// for community in bot.communities().await {
    ///     for ch in community.channels().await {
    ///         if ch.is_private() && !ch.is_readable() {
    ///             eprintln!("waiting on a key for #{}", ch.name());
    ///         }
    ///     }
    /// }
    /// # Ok(()) }
    /// ```
    pub async fn channels(&self) -> Vec<CommunityChannel> {
        self.core
            .list_communities()
            .await
            .into_iter()
            .find(|v| {
                v.get("community_id").or_else(|| v.get("id")).and_then(|i| i.as_str()) == Some(self.id.as_str())
            })
            .and_then(|v| v.get("channels").cloned())
            .and_then(|c| serde_json::from_value::<Vec<CommunityChannel>>(c).ok())
            .unwrap_or_default()
    }

    /// Whether this community has been DISSOLVED (permanently sealed by its owner).
    ///
    /// The local history survives — it is never auto-deleted — but the tombstone means
    /// no relay will ever accept another message or control edit, by anyone. A bot that
    /// does not check this retries sends into a sealed room forever, so gate any
    /// unattended posting loop on it.
    ///
    /// ```no_run
    /// # async fn f(bot: &vector_sdk::VectorBot) -> Result<(), Box<dyn std::error::Error>> {
    /// for community in bot.communities().await {
    ///     if community.is_dissolved().await { continue; }
    ///     // ...safe to post
    /// }
    /// # Ok(()) }
    /// ```
    pub async fn is_dissolved(&self) -> bool {
        self.core
            .list_communities()
            .await
            .into_iter()
            .find(|v| {
                v.get("community_id").or_else(|| v.get("id")).and_then(|i| i.as_str()) == Some(self.id.as_str())
            })
            .and_then(|v| v.get("dissolved").and_then(|d| d.as_bool()))
            .unwrap_or(false)
    }

    /// A handle to one channel of this community by id.
    pub fn channel(&self, channel_id: impl Into<String>) -> Channel {
        Channel { core: self.core, id: channel_id.into(), kind: ChannelKind::Community }
    }

    /// Create a public channel — readable by every member, no key distribution.
    /// Returns its id.
    pub async fn create_channel(&self, name: &str) -> Result<String> {
        self.core.create_channel(&self.id, name, false).await
    }

    /// Create a private channel: its own key, plus the channel-scoped role that
    /// is its access list. Only you can read it until you
    /// [`grant_access`](Self::grant_access) to someone. Returns its id.
    pub async fn create_private_channel(&self, name: &str) -> Result<String> {
        self.core.create_channel(&self.id, name, true).await
    }

    /// Rename a channel. Its id and history survive.
    pub async fn rename_channel(&self, channel_id: &str, name: &str) -> Result<()> {
        self.core.rename_channel(&self.id, channel_id, name).await
    }

    /// Delete a channel. Terminal — the id is never reused.
    pub async fn delete_channel(&self, channel_id: &str) -> Result<()> {
        self.core.delete_channel(&self.id, channel_id).await
    }

    /// Let `npub` read a private channel: grants its access role and sends them
    /// the key. They pick it up on their next sync.
    pub async fn grant_access(&self, channel_id: &str, npub: &str) -> Result<()> {
        self.core.grant_channel_access(&self.id, channel_id, npub).await
    }

    /// Stop `npub` reading a private channel: drops the access role and rotates
    /// the key so the removal actually takes effect. Messages they already read
    /// stay read — rekeying protects the future, not the past.
    pub async fn revoke_access(&self, channel_id: &str, npub: &str) -> Result<()> {
        self.core.revoke_channel_access(&self.id, channel_id, npub).await
    }

    /// Who may read a private channel, as [`Member`] handles — the holders of its
    /// access role. The owner is entitled whether or not they hold one, so they
    /// appear here only if granted (a channel's creator is, so an owner-created
    /// channel does list them).
    pub fn channel_members(&self, channel_id: &str) -> Vec<Member> {
        self.core
            .channel_access(&self.id, channel_id)
            .ok()
            .and_then(|v| v.get("members").and_then(|m| m.as_array()).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|n| n.as_str().map(|n| self.member(n.to_string())))
            .collect()
    }

    /// The raw access summary for a channel: its scoped roles, the members
    /// holding them, the owner, and whether we can read it.
    pub fn channel_access(&self, channel_id: &str) -> Result<serde_json::Value> {
        self.core.channel_access(&self.id, channel_id)
    }

    /// Every member of this community — the Complete Memberlist.
    ///
    /// Unified from four sources, not just one: the Guestbook (joins/leaves/kicks),
    /// anyone **observed publishing** on a channel, everyone the roster grants a
    /// role to, and the proven owner. Publishing is cryptographic proof of key
    /// possession, so someone whose Join was lost — or who never published one —
    /// still counts. Banned npubs are subtracted.
    ///
    /// Ordering is respected rather than naively unioned: a member who left and
    /// went quiet drops out, while one who posted *after* leaving counts again,
    /// since the later evidence wins. The same rule stops an un-ban resurrecting
    /// a phantom from pre-ban activity.
    pub async fn members(&self) -> Vec<Member> {
        self.core
            .get_community_members(&self.id)
            .await
            .into_iter()
            .filter_map(|v| v.get("npub").and_then(|n| n.as_str()).map(|n| self.member(n.to_string())))
            .collect()
    }

    /// Invite an npub via a gift-wrapped private invite (requires the create-invite permission).
    pub async fn invite(&self, npub: &str) -> Result<()> {
        self.core.invite_to_community(&self.id, npub).await.map(|_| ())
    }

    /// Mint a public invite link for this community (never expires, no label —
    /// see `create_invite_with` to set either).
    pub async fn create_invite(&self) -> Result<String> {
        self.core.create_public_invite(&self.id, None, None).await
    }

    /// Mint a public invite link with an optional absolute expiry (unix ms) and an
    /// attribution label, surfaced as "joined via <label>".
    pub async fn create_invite_with(&self, expires_at_ms: Option<u64>, label: Option<String>) -> Result<String> {
        self.core.create_public_invite(&self.id, expires_at_ms, label).await
    }

    /// Update the community's name and/or description.
    pub async fn edit(&self, name: Option<&str>, description: Option<&str>) -> Result<()> {
        self.core.edit_community_metadata(&self.id, name, description).await
    }

    /// Leave this community.
    pub async fn leave(&self) -> Result<()> {
        self.core.leave_community(&self.id).await
    }

    /// Dissolve this community (owner only, irreversible).
    pub async fn dissolve(&self) -> Result<()> {
        self.core.dissolve_community(&self.id).await
    }

    /// Your own role-based capabilities here (JSON flags: manage_*, create_invite, kick, ban, …).
    pub fn capabilities(&self) -> Result<serde_json::Value> {
        self.core.community_capabilities(&self.id)
    }

    /// The owner + admin npubs (`{ owner, admins: [...] }`).
    pub fn roles(&self) -> Result<serde_json::Value> {
        self.core.community_roles(&self.id)
    }
}

/// One channel of a Community, as [`Community::channels`] reports it.
///
/// Deliberately includes channels this bot cannot read: a private channel whose
/// key hasn't arrived is enumerable but unreadable, and a bot that can't see the
/// difference has no way to tell "nobody has talked to me" from "I was granted
/// access and never got the key".
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CommunityChannel {
    channel_id: String,
    name: String,
    #[serde(default)]
    private: bool,
    /// Absent on legacy (v1) communities, which have no private channels.
    #[serde(default = "default_true")]
    readable: bool,
    #[serde(default)]
    epoch: u64,
}

fn default_true() -> bool {
    true
}

impl CommunityChannel {
    /// The channel id (32-byte hex) — what [`Community::channel`] takes.
    pub fn id(&self) -> &str {
        &self.channel_id
    }

    /// The channel's display name. Empty for a private channel recorded before
    /// its metadata folded.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether this channel is private (independently keyed, readable only by
    /// granted role-holders).
    pub fn is_private(&self) -> bool {
        self.private
    }

    /// Whether this bot can actually read it. False only for a private channel
    /// whose key hasn't been delivered yet — see [`Community::channels`].
    pub fn is_readable(&self) -> bool {
        self.readable
    }

    /// The channel's current key generation. Climbs on every rekey; `0` means a
    /// private channel we hold no key for.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

/// A handle to a member of a community — act on them directly. Obtained from
/// [`Community::member`] or [`IncomingMessage::member`].
#[derive(Clone)]
pub struct Member {
    core: VectorCore,
    community_id: String,
    npub: String,
}

impl Member {
    /// This member's npub.
    pub fn npub(&self) -> &str {
        &self.npub
    }

    /// The id of the community this handle is scoped to.
    pub fn community_id(&self) -> &str {
        &self.community_id
    }

    /// Cooperatively kick them (they can rejoin). Requires KICK + outranking them.
    pub async fn kick(&self) -> Result<()> {
        self.core.kick_member(&self.community_id, &self.npub).await
    }

    /// Ban them (terminal; in a private community this triggers a read-cut rekey). Requires BAN.
    ///
    /// Bans serialize per community, so concurrent handlers calling this compose
    /// correctly — but each single ban is its own rotation. Banning a wave? Use
    /// [`Community::ban_many`]: one rotation for the whole batch.
    pub async fn ban(&self) -> Result<()> {
        self.core.set_member_banned(&self.community_id, &self.npub, true).await
    }

    /// Lift a ban.
    pub async fn unban(&self) -> Result<()> {
        self.core.set_member_banned(&self.community_id, &self.npub, false).await
    }

    /// Grant them the @admin role (requires MANAGE_ROLES).
    pub async fn grant_admin(&self) -> Result<()> {
        self.core.grant_admin(&self.community_id, &self.npub).await
    }

    /// Revoke their @admin role.
    pub async fn revoke_admin(&self) -> Result<()> {
        self.core.revoke_admin(&self.community_id, &self.npub).await
    }

    /// Fetch this member's profile.
    pub async fn profile(&self) -> Option<SlimProfile> {
        self.core.load_profile(&self.npub).await;
        self.core.get_profile(&self.npub).await
    }

    /// Whether this member is the community owner.
    pub fn is_owner(&self) -> bool {
        self.core
            .community_roles(&self.community_id)
            .ok()
            .and_then(|r| r.get("owner").and_then(|o| o.as_str()).map(|o| o == self.npub))
            .unwrap_or(false)
    }

    /// Whether this member is an admin (the owner counts as admin).
    pub fn is_admin(&self) -> bool {
        let Ok(roles) = self.core.community_roles(&self.community_id) else { return false };
        let owner = roles.get("owner").and_then(|o| o.as_str()) == Some(self.npub.as_str());
        let admin = roles
            .get("admins")
            .and_then(|a| a.as_array())
            .map(|arr| arr.iter().any(|n| n.as_str() == Some(self.npub.as_str())))
            .unwrap_or(false);
        owner || admin
    }
}

/// Adapts a user closure into an [`InboundEventHandler`].
struct ClosureHandler<F> {
    bot: VectorBot,
    handler: Arc<F>,
}

impl<F, Fut> ClosureHandler<F>
where
    F: Fn(VectorBot, IncomingMessage) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    fn dispatch(&self, chat_id: &str, msg: &Message, is_file: bool, is_group: bool) {
        let handler = self.handler.clone();
        let bot = self.bot.clone();
        let incoming = IncomingMessage {
            chat_id: chat_id.to_string(),
            is_group,
            is_file,
            message: msg.clone(),
        };
        // A registered slash command consumes the message (its handler runs
        // instead of the chat handler — commands aren't chat).
        if bot.try_command(&incoming) {
            return;
        }
        tokio::spawn(async move {
            handler(bot, incoming).await;
        });
    }
}

impl<F, Fut> InboundEventHandler for ClosureHandler<F>
where
    F: Fn(VectorBot, IncomingMessage) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    fn on_dm_received(&self, chat_id: &str, msg: &Message, _is_new: bool) {
        self.dispatch(chat_id, msg, false, false);
    }

    fn on_file_received(&self, chat_id: &str, msg: &Message, _is_new: bool) {
        self.dispatch(chat_id, msg, true, false);
    }

    fn on_community_message(&self, chat_id: &str, msg: &Message, is_new: bool) {
        // The SDK's contract is realtime-only delivery (history flows through
        // `Channel::history` + cursors) — a relay backfilling from a peer pushes
        // stale events over the live sub, and those arrive with is_new=false.
        if !is_new {
            return;
        }
        // Community has a single message hook, so derive is_file from the payload (DMs split it
        // across on_dm_received / on_file_received instead).
        self.dispatch(chat_id, msg, !msg.attachments.is_empty(), true);
    }

    fn on_community_invite(&self, community_id: &str) {
        // Apply the bot's InvitePolicy — auto-accept (public / whitelisted inviter) or leave parked.
        let bot = self.bot.clone();
        let community_id = community_id.to_string();
        tokio::spawn(async move {
            bot.apply_invite_policy(&community_id).await;
        });
    }
}

// ============================================================================
// BotEvent — the full inbound-event stream for `on_event`
// ============================================================================

/// Every kind of inbound event a bot can observe. Delivered to [`VectorBot::on_event`]. DMs and
/// Community channels are unified: `chat_id` is the sender's npub for a DM, the channel id for a
/// Community message.
#[derive(Clone, Debug)]
#[non_exhaustive] // event kinds grow; match with a `_` arm so additions stop being breaking
pub enum BotEvent {
    /// The realtime subscriptions REGISTERED: healthy relays deliver live events
    /// from this moment. Fires once per listen, right after the startup catch-up
    /// sync — stream auth continues in the background, and each AUTH-gating relay
    /// joins the stream the moment its own auth completes, gating nobody else.
    /// No longer gated on the slowest relay in the set. Fires even
    /// with `communities: 0`, so "subscribed to nothing" and "still connecting"
    /// are different observable states. Messages sent BEFORE this are not
    /// delivered live; recover them with a sync + [`Channel::history`].
    Ready { communities: usize },
    /// A new message (DM or Community channel).
    Message(IncomingMessage),
    /// A reaction or edit landed on an existing message; `message` is the updated view (inspect
    /// `.reactions` / `.content`, keyed by `message.id`).
    MessageUpdate { chat_id: String, message: Message },
    /// A message was deleted (cooperative delete / moderation tombstone).
    Delete { chat_id: String, message_id: String },
    /// A member joined a Community channel.
    MemberJoin { channel_id: String, npub: String },
    /// A member left (or was kicked from) a Community channel.
    MemberLeave { channel_id: String, npub: String },
    /// A member is typing in a Community channel; `until` is the unix-secs the indicator expires.
    Typing { chat_id: String, npub: String, until: u64 },
    /// A Community invite arrived. Already auto-handled per [`InvitePolicy`]; surfaced for visibility
    /// (and for `Manual` policy, so you can decide via [`VectorBot::accept_invite`]).
    Invite { community_id: String },
    /// This bot was removed from a Community (kicked / banned / a leave authored on another device).
    Removed { community_id: String },
    /// A private channel became readable — its key arrived after an admin granted
    /// access. Until this fires, the channel is listed by
    /// [`Community::channels`] with `is_readable()` false.
    ///
    /// `backfilled` counts the messages that PREDATE the grant and were pulled
    /// into local state before this event. They are history, not delivery — none
    /// of them arrive as [`BotEvent::Message`], ever. Read them explicitly and
    /// decide what deserves acting on:
    ///
    /// ```no_run
    /// # async fn f(bot: &vector_sdk::VectorBot, community_id: &str, channel_id: &str, backfilled: usize) {
    /// if backfilled > 0 {
    ///     let missed = bot.community(community_id).channel(channel_id).history(backfilled).await;
    ///     // e.g. answer the question that was asked before the grant
    /// }
    /// # }
    /// ```
    ChannelKeyed { community_id: String, channel_id: String, backfilled: usize },
}

/// Adapts a user `on_event` closure into an [`InboundEventHandler`], mapping every hook to a [`BotEvent`].
struct EventClosureHandler<F> {
    bot: VectorBot,
    handler: Arc<F>,
}

impl<F, Fut> EventClosureHandler<F>
where
    F: Fn(VectorBot, BotEvent) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    fn emit(&self, event: BotEvent) {
        let handler = self.handler.clone();
        let bot = self.bot.clone();
        tokio::spawn(async move {
            handler(bot, event).await;
        });
    }

    fn message(&self, chat_id: &str, msg: &Message, is_group: bool, is_file: bool) {
        let incoming = IncomingMessage {
            chat_id: chat_id.to_string(),
            is_group,
            is_file,
            message: msg.clone(),
        };
        // A registered slash command consumes the message (its handler runs
        // instead of the event stream — commands aren't chat).
        if self.bot.try_command(&incoming) {
            return;
        }
        self.emit(BotEvent::Message(incoming));
    }
}

impl<F, Fut> InboundEventHandler for EventClosureHandler<F>
where
    F: Fn(VectorBot, BotEvent) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    fn on_dm_received(&self, chat_id: &str, msg: &Message, _is_new: bool) {
        self.message(chat_id, msg, false, false);
    }
    fn on_file_received(&self, chat_id: &str, msg: &Message, _is_new: bool) {
        self.message(chat_id, msg, false, true);
    }
    fn on_community_message(&self, chat_id: &str, msg: &Message, is_new: bool) {
        // Realtime-only by contract (see `BotEvent::Ready`): a relay backfill
        // replay arrives with is_new=false and never becomes a BotEvent.
        if !is_new {
            return;
        }
        self.message(chat_id, msg, true, !msg.attachments.is_empty());
    }
    fn on_reaction_received(&self, chat_id: &str, msg: &Message) {
        self.emit(BotEvent::MessageUpdate { chat_id: chat_id.to_string(), message: msg.clone() });
    }
    fn on_community_update(&self, chat_id: &str, _target_id: &str, msg: &Message) {
        self.emit(BotEvent::MessageUpdate { chat_id: chat_id.to_string(), message: msg.clone() });
    }
    fn on_message_deleted(&self, chat_id: &str, message_id: &str) {
        self.emit(BotEvent::Delete { chat_id: chat_id.to_string(), message_id: message_id.to_string() });
    }
    fn on_community_removed(&self, chat_id: &str, target_id: &str) {
        self.emit(BotEvent::Delete { chat_id: chat_id.to_string(), message_id: target_id.to_string() });
    }
    fn on_community_presence(
        &self,
        chat_id: &str,
        npub: &str,
        joined: bool,
        _event_id: &str,
        created_at: u64,
        _invited_by: Option<&str>,
        _invited_label: Option<&str>,
    ) {
        // Presence flows from live arrivals AND from guestbook/history folds — a
        // sync or relay backfill replays joins that happened weeks ago. Member
        // state still updates (core owns that); only FRESH transitions become
        // BotEvents, or a welcome bot greets everyone who ever joined.
        if !vector_core::community::is_realtime_fresh(created_at.saturating_mul(1000)) {
            return;
        }
        let (channel_id, npub) = (chat_id.to_string(), npub.to_string());
        self.emit(if joined {
            BotEvent::MemberJoin { channel_id, npub }
        } else {
            BotEvent::MemberLeave { channel_id, npub }
        });
    }
    fn on_community_typing(&self, chat_id: &str, npub: &str, until: u64) {
        self.emit(BotEvent::Typing { chat_id: chat_id.to_string(), npub: npub.to_string(), until });
    }
    fn on_community_self_removed(&self, community_id: &str) {
        self.emit(BotEvent::Removed { community_id: community_id.to_string() });
    }
    fn on_community_invite(&self, community_id: &str) {
        // Auto-handle per policy (same as on_message), AND surface the event for visibility.
        let bot = self.bot.clone();
        let cid = community_id.to_string();
        tokio::spawn(async move {
            bot.apply_invite_policy(&cid).await;
        });
        self.emit(BotEvent::Invite { community_id: community_id.to_string() });
    }
    fn on_channel_keyed(&self, community_id: &str, channel_id: &str, backfilled: usize) {
        self.emit(BotEvent::ChannelKeyed {
            community_id: community_id.to_string(),
            channel_id: channel_id.to_string(),
            backfilled,
        });
    }
    fn on_subscription_ready(&self, communities: usize) {
        self.emit(BotEvent::Ready { communities });
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Classify an id: a valid bech32 `npub` is a DM, anything else (a 64-char hex channel
/// id) is a Community channel.
fn channel_kind_for(id: &str) -> ChannelKind {
    if nostr_sdk::prelude::PublicKey::from_bech32(id).is_ok() {
        ChannelKind::Dm
    } else {
        ChannelKind::Community
    }
}

/// Load the bot's persistent identity from `<data_dir>/identity.nsec`, creating and storing a fresh
/// one on first run. Returns `(nsec, path, created)` where `created` is true only on first run.
fn load_or_create_identity(core: VectorCore, data_dir: &std::path::Path) -> Result<(String, PathBuf, bool)> {
    let path = data_dir.join("identity.nsec");
    if let Ok(contents) = std::fs::read_to_string(&path) {
        let nsec = contents.trim();
        if !nsec.is_empty() {
            return Ok((nsec.to_string(), path, false));
        }
    }
    let nsec = core.generate_nsec()?;
    std::fs::write(&path, &nsec).map_err(VectorError::Io)?;
    restrict_to_owner(&path);
    Ok((nsec, path, true))
}

/// Best-effort tighten of the identity file to owner-only read/write (no-op off unix).
#[cfg(unix)]
fn restrict_to_owner(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn restrict_to_owner(_path: &std::path::Path) {}

/// A per-OS default data directory for a bot's storage.
fn default_data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Library/Application Support/io.vectorapp/sdk");
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(data) = std::env::var("XDG_DATA_HOME") {
            return PathBuf::from(data).join("io.vectorapp/sdk");
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(".local/share/io.vectorapp/sdk");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("io.vectorapp/sdk");
        }
    }
    PathBuf::from("vector-sdk-data")
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::Keys;

    #[test]
    fn cursor_string_form_is_wire_stable_and_roundtrips() {
        // GOLDEN: persisted cursors live in users' own databases — a format
        // change corrupts every one of them, so the exact bytes are pinned.
        let c = Cursor { at_ms: 1785979414499, id: "0b".repeat(32) };
        let s = c.to_string();
        assert_eq!(s, format!("1785979414499:{}", "0b".repeat(32)));
        assert_eq!(s.parse::<Cursor>().unwrap(), c);
        // serde too, for the JSON-storage crowd.
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Cursor>(&json).unwrap(), c);
    }

    #[test]
    fn cursor_rejects_malformed_input_instead_of_guessing() {
        for bad in ["", "123", "abc:def", "12:notahexid", &format!("9:{}", "z".repeat(64)), &format!("x:{}", "0b".repeat(32))] {
            assert!(bad.parse::<Cursor>().is_err(), "{bad:?} must not parse");
        }
        // Uppercase hex normalizes: message ids compare as lowercase.
        let c: Cursor = format!("5:{}", "0B".repeat(32)).parse().unwrap();
        assert_eq!(c.id, "0b".repeat(32));
    }

    #[test]
    fn cursor_orders_by_time_then_id_inside_a_same_ms_wall() {
        let a = Cursor { at_ms: 900, id: "aa".repeat(32) };
        let b = Cursor { at_ms: 900, id: "bb".repeat(32) };
        let older = Cursor { at_ms: 500, id: "ff".repeat(32) };
        assert!(older < a && a < b, "time first, id tiebreak inside the wall");
    }

    #[test]
    fn invite_policy_matrix() {
        let a = Keys::generate().public_key().to_bech32().unwrap();
        let b = Keys::generate().public_key().to_bech32().unwrap();

        // Manual never auto-accepts.
        assert!(!InvitePolicy::Manual.accepts(Some(&a)));
        assert!(!InvitePolicy::Manual.accepts(None));

        // Public accepts anyone (even an unknown/absent inviter).
        assert!(InvitePolicy::Public.accepts(Some(&a)));
        assert!(InvitePolicy::Public.accepts(None));

        // Whitelist accepts ONLY listed inviters, and never a missing one.
        let wl = InvitePolicy::Whitelist(vec![a.clone()]);
        assert!(wl.accepts(Some(&a)), "whitelisted inviter must be accepted");
        assert!(!wl.accepts(Some(&b)), "non-whitelisted inviter must be rejected");
        assert!(!wl.accepts(None), "missing inviter must be rejected under whitelist");
    }

    #[test]
    fn channel_kind_auto_detection() {
        // A valid bech32 npub → DM.
        let npub = Keys::generate().public_key().to_bech32().unwrap();
        assert_eq!(channel_kind_for(&npub), ChannelKind::Dm);
        // A 64-char hex channel id (and a raw-hex pubkey) → Community (not bech32).
        assert_eq!(channel_kind_for(&"a".repeat(64)), ChannelKind::Community);
        assert_eq!(channel_kind_for(&Keys::generate().public_key().to_hex()), ChannelKind::Community);
    }

    #[test]
    fn a_channel_view_distinguishes_locked_from_readable() {
        // The whole point of issue #82: a bot must be able to tell "granted but
        // awaiting the key" from "not a member" — otherwise a mute bot is
        // indistinguishable from a quiet room.
        let json = serde_json::json!([
            { "channel_id": "aa".repeat(32), "name": "general", "private": false, "readable": true, "epoch": 0 },
            { "channel_id": "bb".repeat(32), "name": "mods", "private": true, "readable": true, "epoch": 3 },
            { "channel_id": "cc".repeat(32), "name": "vault", "private": true, "readable": false, "epoch": 0 },
        ]);
        let chans: Vec<CommunityChannel> = serde_json::from_value(json).unwrap();

        assert!(!chans[0].is_private() && chans[0].is_readable(), "a public channel reads");
        assert!(chans[1].is_private() && chans[1].is_readable(), "a keyed private channel reads");
        assert_eq!(chans[1].epoch(), 3, "its key generation is visible");

        let locked = &chans[2];
        assert!(locked.is_private() && !locked.is_readable(), "a keyless private channel is enumerable but locked");
        assert_eq!(locked.name(), "vault", "and still nameable, so it can be reported");
        assert_eq!(locked.id(), "cc".repeat(32), "and addressable, so access can be requested for it");
    }

    #[test]
    fn a_legacy_channel_without_the_readable_field_is_assumed_readable() {
        // v1 communities have no private channels and emit no `readable` — the
        // default must not render every legacy channel as locked.
        let json = serde_json::json!([{ "channel_id": "aa".repeat(32), "name": "general" }]);
        let chans: Vec<CommunityChannel> = serde_json::from_value(json).unwrap();
        assert!(chans[0].is_readable() && !chans[0].is_private());
    }
}
