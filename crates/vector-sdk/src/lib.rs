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
//! ## Communities
//!
//! The SDK speaks the current community protocol only: communities you create or
//! join through it are current-protocol, and legacy memberships an account may
//! hold are ignored rather than surfaced (raw ids handed to [`VectorBot::channel`]
//! bypass that filter).
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

/// Alias for the SDK's error type.
pub use vector_core::VectorError as Error;

/// Re-exported Nostr primitives, so downstreams can depend only on `vector_sdk`.
pub mod nostr {
    pub use nostr_sdk::prelude::{FromBech32, Keys, PublicKey, SecretKey, ToBech32};
}

// Brings `PublicKey::from_bech32` / `.to_bech32()` into scope for id auto-detection + whitelist
// normalization.
use nostr_sdk::prelude::{FromBech32 as _, ToBech32 as _};

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

    /// This bot's invite policy (see [`InvitePolicy`]).
    pub fn invite_policy(&self) -> &InvitePolicy {
        &self.invite_policy
    }

    /// Parked Community invites awaiting a decision — each `{ community_id, name, inviter_npub }`.
    /// (Auto-accepted invites are already gone; these are the ones held under
    /// [`InvitePolicy::Manual`] or rejected by a whitelist.) The SDK is
    /// current-protocol only: an invite to a legacy community never surfaces here.
    pub fn pending_invites(&self) -> Result<Vec<serde_json::Value>> {
        Ok(self
            .core
            .list_pending_invites()?
            .into_iter()
            .filter(|i| i.get("version").and_then(|v| v.as_u64()) == Some(2))
            .collect())
    }

    /// Accept a parked Community invite by id, then start receiving its channels
    /// (the core refreshes the v2 realtime subscription itself).
    pub async fn accept_invite(&self, community_id: &str) -> Result<serde_json::Value> {
        self.core.accept_pending_invite(community_id).await
    }

    /// Apply the invite policy to every currently-parked invite — auto-joining the ones it allows.
    /// Called at `on_message` startup (so a restarted bot picks up invites received while it was
    /// down); also safe to call manually. No-op under [`InvitePolicy::Manual`].
    pub async fn process_pending_invites(&self) {
        if matches!(*self.invite_policy, InvitePolicy::Manual) {
            return;
        }
        let Ok(invites) = self.pending_invites() else { return };
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
        // Resolve the parked record (protocol-filtered) — the whitelist needs the
        // inviter, and a legacy invite must never be auto-joined by a v2-only bot.
        let Some(record) = self.pending_invites().ok().and_then(|invites| {
            invites
                .into_iter()
                .find(|i| i.get("community_id").and_then(|c| c.as_str()) == Some(community_id))
        }) else {
            return;
        };
        let inviter = record.get("inviter_npub").and_then(|n| n.as_str()).map(String::from);
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

    /// Create a new Community owned by this bot and start receiving its channels.
    /// Returns a handle to it (its `#general` channel is ready to message, and
    /// `create_invite` / `invite` mint shareable links or Direct Invites). New
    /// Communities are created on the modern protocol.
    pub async fn create_community(&self, name: impl Into<String>) -> Result<Community> {
        let summary = self.core.create_community_v2(&name.into()).await?;
        let id = summary
            .get("community_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Other("community creation returned no id".into()))?
            .to_string();
        Ok(Community { core: self.core, id })
    }

    /// Every Community this bot is a member of. The SDK is current-protocol only:
    /// a legacy (v1) membership held by this account is ignored, not surfaced.
    pub async fn communities(&self) -> Vec<Community> {
        self.core
            .list_communities()
            .await
            .into_iter()
            .filter(|v| v.get("version").and_then(|x| x.as_u64()) == Some(2))
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
    /// **Ordering:** each message is handled on its own task, so delivery order is
    /// NOT guaranteed — even within one chat, two messages (or an edit and its
    /// delete) can be handled out of order. A handler that mutates shared state per
    /// chat must tolerate reordering (e.g. key by message id, not arrival order).
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
    /// communities it was invited to). Live invites are handled by the event adapters.
    async fn prepare_listen(&self) {
        let _ = self.core.sync_dms(None, &NoOpEventHandler).await;
        self.process_pending_invites().await;
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

    /// Catch up every Community this bot is in — rediscover memberships across
    /// devices, then refold consensus (re-foundings / rekeys / banlist / metadata).
    /// Runs automatically inside [`on_message`](Self::on_message)/`listen` on connect
    /// and periodically for outage resilience; exposed for manual use (e.g. right
    /// after a known reconnect). Inside a running listen loop the refold is queued
    /// to its follow worker; headless, it runs inline before returning.
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
    /// attachments on `msg.message.attachments`.
    pub async fn download_attachment(&self, attachment: &Attachment) -> Result<Vec<u8>> {
        self.core.download_attachment(attachment).await
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
                nostr_sdk::PublicKey::parse(&s).ok().and_then(|pk| pk.to_bech32().ok())
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
            // The channel is this handle — never resolved from local history (a
            // headless bot holds none).
            ChannelKind::Community => self.core.delete_community_message_in(&self.id, message_id).await,
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

    /// Observed members (best-effort, from recent activity).
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

    /// Mint a public invite link for this community.
    pub async fn create_invite(&self) -> Result<String> {
        self.core.create_public_invite(&self.id).await
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

    fn on_community_message(&self, chat_id: &str, msg: &Message, _is_new: bool) {
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
pub enum BotEvent {
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
        self.emit(BotEvent::Message(IncomingMessage {
            chat_id: chat_id.to_string(),
            is_group,
            is_file,
            message: msg.clone(),
        }));
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
    fn on_community_message(&self, chat_id: &str, msg: &Message, _is_new: bool) {
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
        _created_at: u64,
        _invited_by: Option<&str>,
        _invited_label: Option<&str>,
    ) {
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
}

// ============================================================================
// Helpers
// ============================================================================

/// Classify an id: a valid bech32 `npub` is a DM, anything else (a 64-char hex channel
/// id) is a Community channel.
fn channel_kind_for(id: &str) -> ChannelKind {
    if nostr_sdk::PublicKey::from_bech32(id).is_ok() {
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
    use nostr_sdk::Keys;

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
}
