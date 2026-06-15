//! # Vector SDK
//!
//! An ergonomic Rust SDK for building [Vector](https://vectorapp.io) bots and
//! clients. Vector is a private messenger on the Nostr protocol; this SDK is a
//! thin, friendly skin over [`vector_core`] — the headless library that holds
//! **all** of Vector's protocol logic. You get NIP-17 gift-wrapped DMs, file
//! attachments, reactions, typing indicators, edits, deletes, and profiles
//! without ever touching a relay, a gift-wrap, or an encryption key directly.
//!
//! ```no_run
//! use vector_sdk::VectorBot;
//!
//! #[tokio::main]
//! async fn main() -> vector_sdk::Result<()> {
//!     let bot = VectorBot::builder()
//!         .nsec("nsec1...")
//!         .build()
//!         .await?;
//!
//!     // Send a message — `channel` auto-detects DM (npub) vs Community channel (hex id).
//!     bot.channel("npub1...").send("Hello from a bot!").await?;
//!
//!     // Echo every inbound message back — same handler for DMs AND Community channels.
//!     bot.on_message(|_bot, msg| async move {
//!         if msg.is_mine() { return; }
//!         let _ = msg.reply(&format!("Echo: {}", msg.text())).await;
//!     }).await?;
//!
//!     Ok(())
//! }
//! ```
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
//! Everything not surfaced ergonomically here — communities, history sync,
//! custom rumors — is one hop away via [`VectorBot::core`], which hands you the
//! full [`VectorCore`] facade.

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

// Brings `PublicKey::from_bech32` into scope for DM-vs-Community id auto-detection.
use nostr_sdk::prelude::FromBech32 as _;

// ============================================================================
// VectorBot
// ============================================================================

/// A logged-in Vector bot: an identity connected to relays, ready to send and
/// receive. Cheap to [`Clone`] — clones share the same underlying session.
#[derive(Clone)]
pub struct VectorBot {
    core: VectorCore,
    npub: String,
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

    /// A unified messaging handle for a conversation, **auto-detecting** whether `id` is a DM
    /// (an `npub`) or a Community channel (a 64-char hex channel id). Send and receive work the
    /// same way regardless — you never branch on the transport. Infallible; an invalid id surfaces
    /// as an error when you actually send.
    pub fn channel(&self, id: impl Into<String>) -> Channel {
        let id = id.into();
        let kind = if nostr_sdk::PublicKey::from_bech32(&id).is_ok() {
            ChannelKind::Dm
        } else {
            ChannelKind::Community
        };
        Channel { core: self.core, id, kind }
    }

    /// An explicit DM handle for an `npub` (skips auto-detection).
    pub fn dm(&self, npub: impl Into<String>) -> Channel {
        Channel { core: self.core, id: npub.into(), kind: ChannelKind::Dm }
    }

    /// An explicit Community-channel handle for a channel id (skips auto-detection).
    pub fn community(&self, channel_id: impl Into<String>) -> Channel {
        Channel { core: self.core, id: channel_id.into(), kind: ChannelKind::Community }
    }

    // ---- receiving ----

    /// Register an async message handler and block, processing inbound DMs and
    /// file attachments until the client disconnects. The handler is invoked
    /// once per message with a clone of the bot (so it can reply) and an
    /// [`IncomingMessage`]. Each invocation runs on its own task, so a slow
    /// handler won't stall the receive loop.
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
        let adapter = ClosureHandler {
            bot: self.clone(),
            handler: Arc::new(handler),
        };
        self.core.listen(Arc::new(adapter)).await
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

    /// Update this bot's own profile metadata (broadcasts a kind-0 event).
    pub async fn update_profile(&self, name: &str, avatar: &str, banner: &str, about: &str) -> bool {
        self.core.update_profile(name, avatar, banner, about).await
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

    /// Initialize core, log in, and connect to relays.
    pub async fn build(self) -> Result<VectorBot> {
        let key = self.key.ok_or_else(|| {
            VectorError::Other("VectorBot requires a key — call .nsec(...) or .mnemonic(...)".into())
        })?;
        let data_dir = self.data_dir.unwrap_or_else(default_data_dir);
        std::fs::create_dir_all(&data_dir).ok();

        let core = VectorCore::init(CoreConfig {
            data_dir,
            event_emitter: self.event_emitter,
        })?;
        let result = core.login(&key, self.password.as_deref()).await?;
        Ok(VectorBot {
            core,
            npub: result.npub,
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

/// A unified handle for a conversation — **a DM and a Community channel behave the same**. Every
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
    /// The conversation id — an `npub` for a DM, a channel id for a Community channel.
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

    /// Send a file from disk as an encrypted attachment.
    ///
    /// Currently supported for DMs only; Community file attachments are not yet exposed through the
    /// SDK and return an error.
    pub async fn send_file(&self, path: impl AsRef<std::path::Path>) -> Result<String> {
        match self.kind {
            ChannelKind::Dm => {
                let path = path.as_ref().to_string_lossy().into_owned();
                self.core
                    .send_file(&self.id, &path)
                    .await
                    .map(|r| r.event_id.unwrap_or(r.pending_id))
            }
            ChannelKind::Community => Err(VectorError::Other(
                "file send to Community channels is not yet supported via the SDK".into(),
            )),
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
    /// The conversation id. For a DM this is the sender's npub; for a Community message it's the
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

    /// Respond in the same conversation. Works identically for DMs and Community channels.
    pub async fn reply(&self, text: &str) -> Result<String> {
        self.channel().send(text).await
    }

    /// React to *this* message with an emoji, in its own conversation.
    pub async fn react(&self, emoji: &str) -> Result<()> {
        self.channel().react(&self.message.id, emoji).await
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
        self.dispatch(chat_id, msg, false, true);
    }
}

// ============================================================================
// Helpers
// ============================================================================

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
