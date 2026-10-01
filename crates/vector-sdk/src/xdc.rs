//! Mini Apps (WebXDC): let a bot join the games and tools people share.
//!
//! A Mini App is an `.xdc` file sent into a chat. Everyone who has it open
//! joins its **realtime channel**: whatever one of them sends, the others
//! receive. A bot joins that channel too and speaks the app's own messages, as
//! an opponent, a referee, or the server behind an app that needs more than a
//! phone (an LLM, a database, a game's master copy).
//!
//! ```no_run
//! # use vector_sdk::VectorBot;
//! # async fn run(bot: VectorBot) -> vector_sdk::Result<()> {
//! // Runs whenever someone opens an app whose manifest says
//! // `id = "vector-counter"`, for as long as they have it open.
//! bot.xdc("vector-counter").run(|_bot, mut session| async move {
//!     while let Some(event) = session.next().await {
//!         if let vector_sdk::XdcEvent::Data(frame) = event {
//!             let msg: serde_json::Value = frame.json().unwrap_or_default();
//!             if msg["t"] == "hello" {
//!                 let _ = session.send_json(&serde_json::json!({ "t": "count", "value": 0 })).await;
//!             }
//!         }
//!     }
//! });
//! bot.on_message(|_, _| async {}).await?;
//! # Ok(()) }
//! ```
//!
//! Two guides walk through it: [Add a bot to your Mini App] and
//! [Write a bot for any Mini App]. The `xdc_*` examples are complete bots.
//!
//! [Add a bot to your Mini App]: https://github.com/VectorPrivacy/Vector/blob/master/crates/vector-sdk/guides/bot-for-your-mini-app.md
//! [Write a bot for any Mini App]: https://github.com/VectorPrivacy/Vector/blob/master/crates/vector-sdk/guides/bot-for-any-mini-app.md
//!
//! ## The channel
//!
//! - Messages reach whoever has the app open right now, and nothing is
//!   stored. Apps greet newcomers with their state, and a bot should too.
//! - A message can get lost, so apps send their whole state after a change.
//! - The bytes are the app's own format, nearly always JSON. Up to 128,000
//!   bytes per message.
//! - [`XdcFrame::from`] says who sent a message. [`XdcFrame::verified_sender`]
//!   is the one to act on when it matters (a move, a vote).
//! - Players in a session can see each other's IP address, and the bot's. A
//!   bot with Tor on stays out of sessions unless [`allow_outside_tor`] says
//!   otherwise.
//! - Matching by app id reads the app's manifest, downloading the app once,
//!   up to [`download_limit`] (32 MiB unless you [set it](set_download_limit)).
//!   `"*"` matches every app without downloading any.
//! - A player who opened an app while the bot was offline is joined when the
//!   bot comes back, if they still have it open: in DMs always, in Communities
//!   when the bot sees them open it live.
//!
//! It works the same in DMs and in Communities. The bot's
//! [`selfAddr`](XdcSession::self_addr) is its npub, as the app would report it
//! for a person.
//!
//! ## How long a session lasts
//!
//! | Started by | Ends when |
//! | --- | --- |
//! | [`bot.xdc(..).run(..)`](XdcAppBuilder::run) | the handler returns, or nobody has been connected for 3 minutes ([`idle_timeout`](XdcAppBuilder::idle_timeout)), or no message has arrived for 15 minutes ([`quiet_timeout`](XdcAppBuilder::quiet_timeout)) |
//! | [`Xdc::join`] (by hand) | the [`XdcSession`] is dropped or [`left`](XdcSession::leave); no timeouts |
//!
//! The defaults tidy up after players who wander off, so a bot in many chats
//! holds no connections nobody uses. A session joined by hand can opt into
//! the same with [`set_idle_timeout`](XdcSession::set_idle_timeout) and
//! [`set_quiet_timeout`](XdcSession::set_quiet_timeout). When a session ends,
//! [`next`](XdcSession::next) returns `None`. Every session also ends when
//! the bot's account is swapped out.


use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vector_core::types::Attachment;
pub use vector_core::xdc::package::Manifest;
pub use vector_core::xdc::{allow_outside_tor, XdcEvent, XdcFrame, XdcPeer};

use crate::{Channel, Error, Result, VectorBot, VectorCore};

/// A Mini App shared in a chat: the message that carries it, and the realtime
/// session it opens.
#[derive(Clone)]
pub struct Xdc {
    chat_id: String,
    message_id: String,
    attachment: Attachment,
}

impl std::fmt::Debug for Xdc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Xdc")
            .field("chat_id", &self.chat_id)
            .field("message_id", &self.message_id)
            .field("name", &self.attachment.name)
            .field("topic", &self.attachment.webxdc_topic)
            .finish()
    }
}

impl Xdc {
    pub(crate) fn new(chat_id: String, message_id: String, attachment: Attachment) -> Self {
        Self { chat_id, message_id, attachment }
    }

    /// The DM npub or Community channel id it was shared in.
    pub fn chat_id(&self) -> &str {
        &self.chat_id
    }

    /// The message that carries it.
    pub fn message_id(&self) -> &str {
        &self.message_id
    }

    /// Where to reply about it: the chat it was shared in.
    pub fn channel(&self) -> Channel {
        Channel { core: VectorCore, id: self.chat_id.clone(), kind: crate::channel_kind_for(&self.chat_id) }
    }

    /// The file name it was sent as (`3d-tic-tac-toe.xdc`).
    pub fn file_name(&self) -> &str {
        &self.attachment.name
    }

    /// The file's SHA-256 as the sender states it. A claim until the file is
    /// downloaded: [`manifest`](Self::manifest) checks it, and
    /// [`XdcMatch::Hash`] only matches what the download proves.
    pub fn file_hash(&self) -> &str {
        &self.attachment.id
    }

    /// The realtime topic this copy's players share. `None` for an app sent
    /// by a client that predates per-message topics.
    pub fn topic(&self) -> Option<&str> {
        self.attachment.webxdc_topic.as_deref()
    }

    /// The attachment, for [`VectorBot::download_attachment`].
    pub fn attachment(&self) -> &Attachment {
        &self.attachment
    }

    /// The app's `manifest.toml` (name, id, version…), named after the file when
    /// it sets no name. Downloads the file the first time, up to
    /// [`download_limit`] (32 MiB unless set); cached by everything that decides
    /// the download after.
    pub async fn manifest(&self) -> Result<Manifest> {
        inspect(&self.attachment).await.map(|(m, _)| m)
    }

    /// Join the app's realtime session now: the bot connects to everyone
    /// already in it, and everyone who opens it later connects to the bot.
    ///
    /// The session lasts until it is dropped or [`left`](XdcSession::leave): no
    /// idle or quiet timeout applies, unlike a session a
    /// [`run`](XdcAppBuilder::run) handler gets. Opt in with
    /// [`set_idle_timeout`](XdcSession::set_idle_timeout) /
    /// [`set_quiet_timeout`](XdcSession::set_quiet_timeout).
    pub async fn join(&self) -> Result<XdcSession> {
        let topic = self.topic().ok_or_else(|| Error::Other("this app carries no realtime topic".into()))?;
        let inner = vector_core::xdc::join(&self.chat_id, topic).await.map_err(Error::Other)?;
        Ok(XdcSession::new(self.clone(), inner, Timeouts::default()))
    }
}

#[derive(Clone, Copy, Default)]
struct Timeouts {
    /// End after this long with nobody connected.
    alone: Option<Duration>,
    /// End after this long with no frames from anyone.
    quiet: Option<Duration>,
}

/// A joined realtime session. Dropping it leaves.
pub struct XdcSession {
    app: Xdc,
    self_addr: String,
    inner: vector_core::xdc::XdcSession,
    peers: HashMap<[u8; 32], XdcPeer>,
    timeouts: Timeouts,
    /// Stored instants, not timers inside `next`, so a `select!` loop that drops
    /// `next` keeps its deadlines.
    alone_since: Option<tokio::time::Instant>,
    last_frame: tokio::time::Instant,
}

impl XdcSession {
    fn new(app: Xdc, inner: vector_core::xdc::XdcSession, timeouts: Timeouts) -> Self {
        let self_addr = vector_core::my_public_key()
            .and_then(|pk| nostr_sdk::prelude::ToBech32::to_bech32(&pk).ok())
            .unwrap_or_default();
        let now = tokio::time::Instant::now();
        Self { app, self_addr, inner, peers: HashMap::new(), timeouts, alone_since: Some(now), last_frame: now }
    }

    /// The app this session belongs to.
    pub fn app(&self) -> &Xdc {
        &self.app
    }

    /// The bot's npub: what the app calls `webxdc.selfAddr`.
    pub fn self_addr(&self) -> &str {
        &self.self_addr
    }

    /// Broadcast raw bytes to everyone on the channel (at most 128 000).
    pub async fn send(&self, bytes: impl Into<Vec<u8>>) -> Result<()> {
        self.inner.send(bytes).await.map_err(Error::Other)
    }

    /// Broadcast text, as an app's `TextEncoder` would encode it.
    pub async fn send_text(&self, text: &str) -> Result<()> {
        self.send(text.as_bytes().to_vec()).await
    }

    /// Broadcast a value as JSON: the encoding nearly every app uses.
    pub async fn send_json<T: serde::Serialize + ?Sized>(&self, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec(value).map_err(|e| Error::Other(e.to_string()))?;
        self.send(bytes).await
    }

    fn deadline(&self) -> Option<tokio::time::Instant> {
        let alone = self.timeouts.alone.zip(self.alone_since).and_then(|(d, since)| since.checked_add(d));
        let quiet = self.timeouts.quiet.and_then(|d| self.last_frame.checked_add(d));
        match (alone, quiet) {
            (Some(a), Some(q)) => Some(a.min(q)),
            (a, q) => a.or(q),
        }
    }

    /// The next event. `None` once the session ends: it was left or torn down,
    /// or a timeout ran out (nobody connected, or nothing said; see
    /// [`set_idle_timeout`](Self::set_idle_timeout)), in which case the session
    /// leaves as it returns.
    pub async fn next(&mut self) -> Option<XdcEvent> {
        let event = match self.deadline() {
            Some(at) => match tokio::time::timeout_at(at, self.inner.recv()).await {
                Ok(event) => event,
                Err(_) => {
                    self.inner.end();
                    return None;
                }
            },
            None => self.inner.recv().await,
        }?;
        match &event {
            XdcEvent::PeerJoined(p) => {
                self.peers.insert(p.node, p.clone());
                self.alone_since = None;
            }
            XdcEvent::PeerLeft(p) => {
                self.peers.remove(&p.node);
                if self.peers.is_empty() {
                    self.alone_since = Some(tokio::time::Instant::now());
                }
            }
            XdcEvent::Data(f) => {
                self.last_frame = tokio::time::Instant::now();
                // A node can be named after it arrived (its wait ran out first);
                // keep the freshest attribution on hand for `peers()`.
                if let Some(p) = self.peers.get_mut(&f.from.node) {
                    if p.npub.is_none() {
                        p.npub = f.from.npub.clone();
                    }
                }
            }
            _ => {}
        }
        Some(event)
    }

    /// A handle that sends on this session from another task (a timer, a
    /// stream), while this one keeps reading. It stops working once the
    /// session is left.
    pub fn sender(&self) -> XdcSender {
        XdcSender(self.inner.sender())
    }

    /// Peers directly connected right now, as of the last [`next`](Self::next).
    pub fn peers(&self) -> Vec<XdcPeer> {
        self.peers
            .values()
            .map(|p| XdcPeer { node: p.node, npub: p.npub.clone().or_else(|| self.inner.npub_of(&p.node)) })
            .collect()
    }

    /// The npub of the player behind a device (`node`), if known.
    pub fn npub_of(&self, node: &[u8; 32]) -> Option<String> {
        self.inner.npub_of(node)
    }

    /// End after this long with nobody connected (`None`: never). A session from
    /// a [`run`](XdcAppBuilder::run) handler starts at 3 minutes; one joined by
    /// hand at `None`.
    pub fn set_idle_timeout(&mut self, idle: Option<Duration>) {
        self.timeouts.alone = idle;
    }

    /// End after this long with no frames from anyone (`None`: never). A session
    /// from a [`run`](XdcAppBuilder::run) handler starts at 15 minutes; one
    /// joined by hand at `None`.
    pub fn set_quiet_timeout(&mut self, quiet: Option<Duration>) {
        self.timeouts.quiet = quiet;
    }

    /// Leave now and tell the chat.
    pub async fn leave(self) {
        self.inner.leave().await
    }
}

/// Sends on a session from any task; from [`XdcSession::sender`].
#[derive(Clone)]
pub struct XdcSender(vector_core::xdc::XdcSender);

impl XdcSender {
    pub async fn send(&self, bytes: impl Into<Vec<u8>>) -> Result<()> {
        self.0.send(bytes).await.map_err(Error::Other)
    }

    pub async fn send_text(&self, text: &str) -> Result<()> {
        self.send(text.as_bytes().to_vec()).await
    }

    pub async fn send_json<T: serde::Serialize + ?Sized>(&self, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec(value).map_err(|e| Error::Other(e.to_string()))?;
        self.send(bytes).await
    }
}

/// Which apps an [`XdcAppBuilder`] handles.
#[derive(Clone, Debug)]
pub enum XdcMatch {
    /// Every Mini App.
    Any,
    /// Apps whose manifest `id` (or, lacking one, `name`) is this.
    Id(String),
    /// One exact build: the SHA-256 of the file, as downloaded.
    Hash(String),
}

impl From<&str> for XdcMatch {
    fn from(s: &str) -> Self {
        if s == "*" { XdcMatch::Any } else { XdcMatch::Id(s.to_string()) }
    }
}

/// When a [`run`](XdcAppBuilder::run) handler joins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinWhen {
    /// When someone opens the app. Apps nobody is playing cost the bot nothing.
    Opened,
    /// As soon as the app is shared, before anyone opens it, and again
    /// whenever someone opens it after the bot's session ended: for a bot that
    /// hosts, greets, or must be there first.
    Shared,
}

type SessionFn = dyn Fn(VectorBot, XdcSession) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync;
struct AppHandler {
    matcher: XdcMatch,
    when: JoinWhen,
    timeouts: Timeouts,
    max_sessions: usize,
    max_per_chat: usize,
    live: Mutex<usize>,
    per_chat: Mutex<HashMap<String, usize>>,
    run: Arc<SessionFn>,
}

/// Declares a Mini App handler: which apps, when to join, and what to do
/// with each session. From [`VectorBot::xdc`]; does nothing until [`run`](Self::run).
#[must_use = "a handler is registered by .run(...)"]
pub struct XdcAppBuilder {
    bot: VectorBot,
    matcher: XdcMatch,
    when: JoinWhen,
    timeouts: Timeouts,
    max_sessions: usize,
    max_per_chat: usize,
}

impl XdcAppBuilder {
    /// Join when the app is shared rather than only when it is opened.
    pub fn join_when(mut self, when: JoinWhen) -> Self {
        self.when = when;
        self
    }

    /// End a session after this long with nobody connected; the handler's
    /// [`next`](XdcSession::next) returns `None`. Default 3 minutes; `None` keeps
    /// sessions until the handler returns.
    pub fn idle_timeout(mut self, idle: Option<Duration>) -> Self {
        self.timeouts.alone = idle;
        self
    }

    /// End a session after this long with no frames from anyone, even with
    /// peers connected. Default 15 minutes; `None` disables it.
    pub fn quiet_timeout(mut self, quiet: Option<Duration>) -> Self {
        self.timeouts.quiet = quiet;
        self
    }

    /// At most this many sessions of this handler at once (default 32).
    pub fn max_sessions(mut self, n: usize) -> Self {
        self.max_sessions = n;
        self
    }

    /// At most this many sessions of this handler in any one chat (default 4),
    /// so one chat can't take every slot.
    pub fn max_per_chat(mut self, n: usize) -> Self {
        self.max_per_chat = n;
        self
    }

    /// Register the handler. It runs once per session, in its own task. The
    /// session ends when the handler returns, or earlier when its
    /// [`idle_timeout`](Self::idle_timeout) (3 min alone) or
    /// [`quiet_timeout`](Self::quiet_timeout) (15 min without frames) runs out:
    /// [`next`](XdcSession::next) then returns `None`. Register before the bot
    /// starts listening.
    pub fn run<F, Fut>(self, handler: F)
    where
        F: Fn(VectorBot, XdcSession) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let run: Arc<SessionFn> = Arc::new(move |bot, session| Box::pin(handler(bot, session)));
        self.bot.xdc_router().handlers.lock().unwrap().push(Arc::new(AppHandler {
            matcher: self.matcher,
            when: self.when,
            timeouts: self.timeouts,
            max_sessions: self.max_sessions,
            max_per_chat: self.max_per_chat,
            live: Mutex::new(0),
            per_chat: Mutex::new(HashMap::new()),
            run,
        }));
    }
}

/// How old an advertisement may be and still mean "someone is playing now".
/// Catch-up sync replays old ones; a crashed client never sends its departure.
const OPENED_FRESHNESS_SECS: u64 = 15 * 60;
/// Topics remembered as opened before their app's message arrived.
const EARLY_OPENS_CAP: usize = 256;
/// How long an open waits for its app's message to be stored, checking each second.
const EARLY_OPEN_LOOKUPS: u32 = 20;

/// A peer signal as a hook hands it over.
struct SignalIn {
    chat_id: String,
    npub: String,
    topic: String,
    node_addr: Option<String>,
    event_id: String,
    created_at: u64,
}

/// A topic a handler session holds. `rejoin`: someone opened the app while
/// the session was ending; start again once it has.
#[derive(Default)]
struct Active {
    rejoin: Option<Opener>,
}

/// The advertisement that said someone opened an app: `topic` as it sent it.
/// Checked again right before joining, since a catch-up replays signals out of
/// order and the same player's departure may already be stored by then.
#[derive(Clone, Debug)]
struct Opener {
    topic: String,
    npub: String,
    at: u64,
}

impl Opener {
    fn still_open(&self) -> bool {
        vector_core::db::miniapps::peer_signal_is_current(&self.topic, &self.npub, self.at, true).unwrap_or(false)
    }
}

/// Peer signals processed at once: a first full sync replays thousands.
static SIGNALS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

/// Routes Mini App signals and shares to registered handlers.
#[derive(Default)]
pub(crate) struct XdcRouter {
    handlers: Mutex<Vec<Arc<AppHandler>>>,
    active: Mutex<HashMap<String, Active>>,
    /// Apps someone opened before their message reached us, by (chat, topic):
    /// the sender's client can advertise while its upload is still in flight.
    early_opens: Mutex<HashMap<(String, String), (Opener, std::time::Instant)>>,
}

/// What each package's bytes say (its manifest, unnamed when it sets no name),
/// by the file's real hash, and which hash each download source produced.
/// Content-addressed, so shared by every account.
#[derive(Default)]
struct ManifestCache {
    by_hash: HashMap<String, Manifest>,
    /// By [`source_key`]: everything that decides the bytes a download yields.
    sources: HashMap<String, String>,
}

static MANIFESTS: tokio::sync::Mutex<Option<ManifestCache>> = tokio::sync::Mutex::const_new(None);
/// Downloads for manifests at once: each holds up to [`download_limit`] in memory.
static INSPECTING: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
const MANIFEST_CACHE_CAP: usize = 256;

/// The default [`download_limit`]: 32 MiB, about four times the largest app in
/// the public WebXDC store.
pub const DEFAULT_DOWNLOAD_LIMIT: usize = 32 * 1024 * 1024;
static DOWNLOAD_LIMIT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(DEFAULT_DOWNLOAD_LIMIT);

/// Set the largest `.xdc` (its size, in bytes) the bot downloads to
/// read an app's manifest: to match a handler registered by id or hash, and for
/// [`Xdc::manifest`]. Anyone in a chat with the bot can post a file, so this
/// bounds what a stranger can make it download; up to two such downloads run at
/// once. Raise it for big apps, lower it for a tighter bound; `usize::MAX`
/// removes it. Apps larger than the limit simply don't match. Handlers for
/// `"*"` never download. Process-wide, like [`allow_outside_tor`]; also
/// settable with [`VectorBotBuilder::xdc_download_limit`](crate::VectorBotBuilder::xdc_download_limit).
pub fn set_download_limit(bytes: usize) {
    DOWNLOAD_LIMIT.store(bytes, std::sync::atomic::Ordering::Relaxed);
}

/// The current limit (see [`set_download_limit`]); [`DEFAULT_DOWNLOAD_LIMIT`] unless changed.
pub fn download_limit() -> usize {
    DOWNLOAD_LIMIT.load(std::sync::atomic::Ordering::Relaxed)
}
/// One inspection, every source included: a server trickling bytes must not
/// hold a download slot for good.
const INSPECT_DEADLINE: Duration = Duration::from_secs(90);

/// Sources that recently yielded no app, and until when they are refused, so
/// events about a bad share can't each cost a download. A file that isn't a
/// readable app is refused for long; one that couldn't be reached, briefly.
static FAILED: Mutex<Option<HashMap<String, std::time::Instant>>> = Mutex::new(None);
const UNREADABLE_FOR: Duration = Duration::from_secs(600);
const UNREACHABLE_FOR: Duration = Duration::from_secs(60);

/// One key per distinct download: the claimed id, every URL, and the key and
/// nonce that decrypt it. Any member can copy an id or a URL; a different key or
/// mirror is a different download, and never borrows another's result.
fn source_key(att: &Attachment) -> String {
    let mut parts = vec![att.id.as_str(), att.url.as_str(), att.key.as_str(), att.nonce.as_str()];
    parts.push(att.original_hash.as_deref().unwrap_or(""));
    parts.extend(att.fallback_urls.iter().map(String::as_str));
    // Length-prefixed, so no two different sources spell the same key.
    let joined: String = parts.iter().map(|p| format!("{}:{p}", p.len())).collect();
    vector_core::simd::hex::bytes_to_hex_string(&vector_core::crypto::sha256::digest(joined.as_bytes()))
}

/// The app's manifest and the file's real SHA-256. The name falls back to this
/// attachment's file name, never another share's of the same bytes.
async fn inspect(att: &Attachment) -> Result<(Manifest, String)> {
    let limit = download_limit();
    // The claimed size costs nothing to check, so it is never remembered. AES-GCM adds 16 bytes.
    if usize::try_from(att.size).unwrap_or(usize::MAX) > limit.saturating_add(16) {
        return Err(Error::Other(format!(
            "{} is larger than the {} download limit",
            att.name,
            vector_core::crypto::format_bytes(limit as u64)
        )));
    }
    let source = source_key(att);
    // A failure under one limit says nothing about another.
    let failure = format!("{source}@{limit}");
    if let Some(known) = known(&source, &failure, att).await {
        return known;
    }
    let _permit = INSPECTING.acquire().await.map_err(|e| Error::Other(e.to_string()))?;
    // Another event about this source may have read it while this one queued.
    if let Some(known) = known(&source, &failure, att).await {
        return known;
    }
    let outcome = match tokio::time::timeout(INSPECT_DEADLINE, inspect_download(att, limit)).await {
        Ok(outcome) => outcome,
        Err(_) => Err(Inspect::Unreadable(Error::Other(format!("{} took too long to download", att.name)))),
    };
    match outcome {
        Ok((manifest, hash)) => {
            let mut cache = MANIFESTS.lock().await;
            let cache = cache.get_or_insert_with(ManifestCache::default);
            if cache.by_hash.len() >= MANIFEST_CACHE_CAP || cache.sources.len() >= MANIFEST_CACHE_CAP * 4 {
                *cache = ManifestCache::default();
            }
            cache.by_hash.insert(hash.clone(), manifest.clone());
            cache.sources.insert(source, hash.clone());
            Ok((manifest.or_named(&att.name), hash))
        }
        Err(Inspect::Unreadable(e)) => {
            remember_failure(failure, UNREADABLE_FOR);
            Err(e)
        }
        Err(Inspect::Unreachable(e)) => {
            remember_failure(failure, UNREACHABLE_FOR);
            Err(e)
        }
    }
}

/// What is already known about `source`: its manifest, or a recent failure.
async fn known(source: &str, failure: &str, att: &Attachment) -> Option<Result<(Manifest, String)>> {
    if let Some(hit) = MANIFESTS.lock().await.as_ref().and_then(|c| {
        let hash = c.sources.get(source)?;
        Some((c.by_hash.get(hash)?.clone().or_named(&att.name), hash.clone()))
    }) {
        return Some(Ok(hit));
    }
    let now = std::time::Instant::now();
    if FAILED.lock().unwrap().as_ref().and_then(|m| m.get(failure)).is_some_and(|until| *until > now) {
        return Some(Err(Error::Other("this app could not be read recently".into())));
    }
    None
}

fn remember_failure(source: String, for_: Duration) {
    let now = std::time::Instant::now();
    let mut failed = FAILED.lock().unwrap();
    let failed = failed.get_or_insert_with(HashMap::new);
    failed.retain(|_, until| *until > now);
    if failed.len() < 1024 {
        failed.insert(source, now + for_);
    }
}

/// Why an inspection failed: the file itself, or getting it.
enum Inspect {
    Unreadable(Error),
    Unreachable(Error),
}

async fn inspect_download(att: &Attachment, limit: usize) -> std::result::Result<(Manifest, String), Inspect> {
    let bytes = VectorCore.download_attachment_within(att, None, limit.saturating_add(16)).await.map_err(|e| match e {
        vector_core::DownloadError::Unreachable(_) => Inspect::Unreachable(Error::Other(e.to_string())),
        _ => Inspect::Unreadable(Error::Other(e.to_string())),
    })?;
    tokio::task::spawn_blocking(move || {
        let hash = vector_core::simd::hex::bytes_to_hex_string(&vector_core::crypto::sha256::digest(&bytes));
        vector_core::xdc::package::read_manifest(&bytes).map(|m| (m, hash))
    })
    .await
    .map_err(|e| Inspect::Unreachable(Error::Other(e.to_string())))?
    .map_err(|e| Inspect::Unreadable(Error::Other(e)))
}

/// Gives a handler's slot back however its task ends, panic included.
struct Slot {
    router: Arc<XdcRouter>,
    handler: Arc<AppHandler>,
    topic: String,
    chat_id: String,
    bot: VectorBot,
    app: Xdc,
    finished: Option<std::pin::Pin<Box<dyn Future<Output = ()> + Send>>>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        release(&self.handler, &self.chat_id);
        // Release the topic only once the old session has fully left: a rejoin
        // before its departure signal goes out would be erased by it.
        let (router, topic, bot, app) = (self.router.clone(), self.topic.clone(), self.bot.clone(), self.app.clone());
        let finished = self.finished.take();
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        vector_core::db::spawn_bound(async move {
            if let Some(f) = finished {
                let _ = tokio::time::timeout(Duration::from_secs(20), f).await;
            }
            let rejoin = router.active.lock().unwrap_or_else(|e| e.into_inner()).remove(&topic).and_then(|a| a.rejoin);
            if let Some(opener) = rejoin {
                router.start(bot, app, JoinWhen::Opened, Some(opener)).await;
            }
        });
    }
}

impl XdcRouter {
    pub(crate) fn has_handlers(&self) -> bool {
        !self.handlers.lock().unwrap().is_empty()
    }

    async fn matches(&self, m: &XdcMatch, app: &Xdc) -> bool {
        let inspected = match m {
            XdcMatch::Any => return true,
            _ => inspect(&app.attachment).await,
        };
        match (m, inspected) {
            (XdcMatch::Hash(h), Ok((_, hash))) => h.eq_ignore_ascii_case(&hash),
            (XdcMatch::Id(id), Ok((man, _))) => man.id.as_deref() == Some(id.as_str()) || (man.id.is_none() && man.name == *id),
            (_, Err(e)) => {
                vector_core::log_warn!("[xdc] could not read {}: {e}", app.file_name());
                false
            }
            (XdcMatch::Any, _) => true,
        }
    }

    /// A peer signal arrived (DM or Community). Persist it, feed it to a session
    /// we're in, else start a handler if someone just opened a matching app.
    async fn on_signal(self: Arc<Self>, bot: VectorBot, s: SignalIn) {
        let sig = {
            let _permit = SIGNALS.acquire().await;
            vector_core::xdc::on_signal(&s.chat_id, &s.npub, &s.topic, s.node_addr.as_deref(), &s.event_id, s.created_at).await
        };
        let Some(sig) = sig else { return };
        if !self.has_handlers() || vector_core::xdc::is_joined(&sig.topic) {
            return;
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        if !sig.current || sig.node_addr.is_none() || sig.npub == bot.npub() || now.saturating_sub(sig.created_at) > OPENED_FRESHNESS_SECS {
            return;
        }
        let Ok(topic) = vector_core::xdc::wire::decode_topic(&sig.topic).map(|t| vector_core::xdc::wire::encode_topic(&t)) else { return };
        let opener = Opener { topic: sig.topic.clone(), npub: sig.npub.clone(), at: sig.created_at };
        if let Some(a) = self.active.lock().unwrap().get_mut(&topic) {
            a.rejoin = Some(opener);
            return;
        }
        // Only the chat the signal came through: a topic is that chat's session.
        let Some(found) = vector_core::xdc::find_by_topic_in(&sig.chat_id, &topic).await else {
            let key = (sig.chat_id.clone(), topic);
            let already_polling = {
                let mut early = self.early_opens.lock().unwrap();
                early.retain(|_, (_, at)| at.elapsed().as_secs() < OPENED_FRESHNESS_SECS);
                let polling = early.get(&key).is_some_and(|(_, at)| at.elapsed().as_secs() < u64::from(EARLY_OPEN_LOOKUPS));
                if !early.contains_key(&key) && early.len() >= EARLY_OPENS_CAP {
                    return;
                }
                early.insert(key.clone(), (opener, std::time::Instant::now()));
                polling
            };
            // The message may arrive live (then `on_shared` takes the open), or be stored by a
            // catch-up sync that announces nothing: look for it a while, once per app.
            if !already_polling {
                self.await_message(bot, key).await;
            }
            return;
        };
        let app = Xdc::new(found.chat_id, found.message_id, found.attachment);
        self.start(bot, app, JoinWhen::Opened, Some(opener)).await;
    }

    /// Start a parked open once its app's message is stored, unless `on_shared` takes it first.
    async fn await_message(self: Arc<Self>, bot: VectorBot, key: (String, String)) {
        for _ in 0..EARLY_OPEN_LOOKUPS {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if !self.early_opens.lock().unwrap().contains_key(&key) {
                return;
            }
            if self.clone().take_parked(&bot, &key).await {
                return;
            }
        }
    }

    /// If the message for a parked open is stored now, start it; true once the open is gone.
    async fn take_parked(self: Arc<Self>, bot: &VectorBot, key: &(String, String)) -> bool {
        let Some(found) = vector_core::xdc::find_by_topic_in(&key.0, &key.1).await else { return false };
        let parked = self.early_opens.lock().unwrap().remove(key);
        if let Some((opener, _)) = parked {
            let app = Xdc::new(found.chat_id, found.message_id, found.attachment);
            self.start(bot.clone(), app, JoinWhen::Opened, Some(opener)).await;
        }
        true
    }

    /// Look again for every parked open's message: a catch-up sync stores
    /// messages without announcing them, and may outlast the per-open polling.
    pub(crate) async fn rescan_parked(self: Arc<Self>, bot: VectorBot) {
        let keys: Vec<_> = self.early_opens.lock().unwrap().keys().cloned().collect();
        for key in keys {
            self.clone().take_parked(&bot, &key).await;
        }
    }

    /// A message arrived carrying a Mini App.
    async fn on_shared(self: Arc<Self>, bot: VectorBot, app: Xdc) {
        let opener = app.topic().and_then(|t| vector_core::xdc::wire::decode_topic(t).ok()).and_then(|t| {
            let key = (app.chat_id.clone(), vector_core::xdc::wire::encode_topic(&t));
            let mut early = self.early_opens.lock().unwrap();
            early.remove(&key).filter(|(_, at)| at.elapsed().as_secs() < OPENED_FRESHNESS_SECS).map(|(o, _)| o)
        });
        let when = if opener.is_some() { JoinWhen::Opened } else { JoinWhen::Shared };
        self.start(bot, app, when, opener).await;
    }

    /// Start the first handler that takes `app`. An open wakes every handler
    /// (a Shared one whose session ended comes back for it); a share wakes
    /// only Shared ones.
    async fn start(self: Arc<Self>, bot: VectorBot, app: Xdc, why: JoinWhen, opener: Option<Opener>) {
        let Some(topic) = app.topic().and_then(|t| vector_core::xdc::wire::decode_topic(t).ok()).map(|t| vector_core::xdc::wire::encode_topic(&t)) else {
            return;
        };
        if vector_core::xdc::is_joined(&topic) {
            return;
        }
        {
            let mut active = self.active.lock().unwrap();
            if active.contains_key(&topic) {
                return;
            }
            active.insert(topic.clone(), Active::default());
        }
        // Gives the topic back however this ends, a panicking matcher included,
        // unless a session took it.
        let mut claim = TopicClaim { router: self.clone(), topic: topic.clone(), held: true };
        let handlers: Vec<_> = self
            .handlers
            .lock()
            .unwrap()
            .iter()
            .filter(|h| why == JoinWhen::Opened || h.when == JoinWhen::Shared)
            .cloned()
            .collect();
        for h in handlers {
            if !self.matches(&h.matcher, &app).await {
                continue;
            }
            if !reserve(&h, &app.chat_id) {
                vector_core::log_warn!("[xdc] {} not joined: handler at its session limit", app.file_name());
                break;
            }
            if opener.as_ref().is_some_and(|o| !o.still_open()) {
                release(&h, &app.chat_id);
                break;
            }
            match vector_core::xdc::join(&app.chat_id, &topic).await {
                Ok(inner) => {
                    let slot = Slot {
                        router: self.clone(),
                        handler: h.clone(),
                        topic: topic.clone(),
                        chat_id: app.chat_id.clone(),
                        bot: bot.clone(),
                        app: app.clone(),
                        finished: Some(Box::pin(inner.finished())),
                    };
                    let session = XdcSession::new(app.clone(), inner, h.timeouts);
                    let run = h.run.clone();
                    claim.held = false;
                    vector_core::db::spawn_bound(async move {
                        let _slot = slot;
                        run(bot, session).await;
                    });
                    return;
                }
                Err(e) => {
                    release(&h, &app.chat_id);
                    vector_core::log_warn!("[xdc] could not join {}: {e}", app.file_name());
                    break;
                }
            }
        }
    }
}

struct TopicClaim {
    router: Arc<XdcRouter>,
    topic: String,
    held: bool,
}

impl Drop for TopicClaim {
    fn drop(&mut self) {
        if self.held {
            self.router.active.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.topic);
        }
    }
}

fn reserve(h: &AppHandler, chat_id: &str) -> bool {
    let mut live = h.live.lock().unwrap_or_else(|e| e.into_inner());
    let mut per_chat = h.per_chat.lock().unwrap_or_else(|e| e.into_inner());
    let in_chat = per_chat.get(chat_id).copied().unwrap_or(0);
    if *live >= h.max_sessions || in_chat >= h.max_per_chat {
        return false;
    }
    *live += 1;
    per_chat.insert(chat_id.to_string(), in_chat + 1);
    true
}

fn release(h: &AppHandler, chat_id: &str) {
    let mut live = h.live.lock().unwrap_or_else(|e| e.into_inner());
    *live = live.saturating_sub(1);
    let mut per_chat = h.per_chat.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(n) = per_chat.get_mut(chat_id) {
        *n = n.saturating_sub(1);
        if *n == 0 {
            per_chat.remove(chat_id);
        }
    }
}

impl VectorBot {
    /// Handle a Mini App: `"*"` for every app, or the app's manifest `id`.
    /// Then [`run`](XdcAppBuilder::run) a handler per session. Handlers ride
    /// whichever receive loop the bot runs ([`on_message`](Self::on_message) or
    /// [`on_event`](Self::on_event)).
    pub fn xdc(&self, app: impl Into<XdcMatch>) -> XdcAppBuilder {
        XdcAppBuilder {
            bot: self.clone(),
            matcher: app.into(),
            when: JoinWhen::Opened,
            timeouts: Timeouts { alone: Some(Duration::from_secs(180)), quiet: Some(Duration::from_secs(900)) },
            max_sessions: 32,
            max_per_chat: 4,
        }
    }

    pub(crate) fn xdc_router(&self) -> Arc<XdcRouter> {
        self.xdc.clone()
    }

    /// Route a peer signal through the Mini App layer (spawned: it persists).
    pub(crate) fn xdc_signal(&self, chat_id: &str, npub: &str, topic: &str, node_addr: Option<&str>, event_id: &str, created_at: u64) {
        let signal = SignalIn {
            chat_id: chat_id.to_string(),
            npub: npub.to_string(),
            topic: topic.to_string(),
            node_addr: node_addr.map(str::to_string),
            event_id: event_id.to_string(),
            created_at,
        };
        vector_core::db::spawn_bound(self.xdc_router().on_signal(self.clone(), signal));
    }

    /// A message arrived; start `JoinWhen::Shared` handlers for any Mini App on it,
    /// and `Opened` ones if someone opened it before it got here.
    pub(crate) fn xdc_shared(&self, chat_id: &str, msg: &vector_core::Message) {
        if msg.mine || !self.xdc.has_handlers() {
            return;
        }
        for att in msg.attachments.iter().filter(|a| vector_core::xdc::is_app(a)) {
            let app = Xdc::new(chat_id.to_string(), msg.id.clone(), att.clone());
            vector_core::db::spawn_bound(self.xdc_router().on_shared(self.clone(), app));
        }
    }
}

impl crate::IncomingMessage {
    /// The Mini App this message carries, if it is one.
    pub fn xdc(&self) -> Option<Xdc> {
        let att = self.message.attachments.iter().find(|a| vector_core::xdc::is_app(a))?;
        Some(Xdc::new(self.chat_id.clone(), self.message.id.clone(), att.clone()))
    }
}

impl Channel {
    /// Send a Mini App (`.xdc` file) and get it back as an [`Xdc`] to join.
    pub async fn send_xdc(&self, path: impl AsRef<std::path::Path>) -> Result<Xdc> {
        let id = self.send_file(path).await?;
        let (chat_id, message) = self
            .core
            .get_message(&id)
            .await
            .ok_or_else(|| Error::Other("sent, but the message is not in local state".into()))?;
        let att = message
            .attachments
            .iter()
            .find(|a| vector_core::xdc::is_app(a))
            .ok_or_else(|| Error::Other("the file sent is not a Mini App".into()))?;
        Ok(Xdc::new(chat_id, message.id.clone(), att.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, url: &str, key: &str) -> Attachment {
        Attachment { id: id.into(), url: url.into(), key: key.into(), nonce: "n".into(), ..Default::default() }
    }

    #[tokio::test]
    async fn the_download_limit_is_the_bot_devs_to_set() {
        set_download_limit(1_000);
        let mut big = app("big", "", "k");
        big.size = 5_000;
        let err = inspect(&big).await.unwrap_err().to_string();
        assert!(err.contains("download limit"), "{err}");

        // Under the limit it is fetched (and fails here: no URL), and that failure is remembered…
        let mut small = app("small", "", "k");
        small.size = 900;
        assert!(inspect(&small).await.unwrap_err().to_string().contains("no URL"));
        assert!(inspect(&small).await.unwrap_err().to_string().contains("recently"));
        // …only under that limit: a new limit tries again.
        set_download_limit(2_000);
        assert!(inspect(&small).await.unwrap_err().to_string().contains("no URL"));
        big.size = 1_900;
        assert!(!inspect(&big).await.unwrap_err().to_string().contains("download limit"), "a raised limit admits it");

        set_download_limit(DEFAULT_DOWNLOAD_LIMIT);
        assert_eq!(download_limit(), 32 * 1024 * 1024);
    }

    #[test]
    fn a_copied_id_and_url_with_another_key_or_mirror_is_another_source() {
        let alice = app("h", "https://blossom.example/h", "k1");
        let base = source_key(&alice);
        assert_eq!(base, source_key(&alice.clone()));
        assert_ne!(base, source_key(&app("h", "https://blossom.example/h", "k2")), "a different key");
        let mut mirrored = alice.clone();
        mirrored.fallback_urls.push("https://evil.example/h".into());
        assert_ne!(base, source_key(&mirrored), "an added mirror");
        let mut renonced = alice.clone();
        renonced.nonce = "m".into();
        assert_ne!(base, source_key(&renonced), "a different nonce");
        assert_ne!(source_key(&app("ab", "c", "k")), source_key(&app("a", "bc", "k")), "fields can't run together");
    }

    fn handler(max: usize, per_chat: usize) -> AppHandler {
        AppHandler {
            matcher: XdcMatch::Any,
            when: JoinWhen::Opened,
            timeouts: Timeouts::default(),
            max_sessions: max,
            max_per_chat: per_chat,
            live: Mutex::new(0),
            per_chat: Mutex::new(HashMap::new()),
            run: Arc::new(|_, _| Box::pin(async {})),
        }
    }

    #[test]
    fn slots_are_bounded_per_handler_and_per_chat() {
        let h = handler(3, 2);
        assert!(reserve(&h, "a") && reserve(&h, "a"));
        assert!(!reserve(&h, "a"), "a chat can't take more than its share");
        assert!(reserve(&h, "b"));
        assert!(!reserve(&h, "c"), "the handler is full");
        assert!(!h.per_chat.lock().unwrap().contains_key("c"), "a refusal leaves no entry");
        release(&h, "a");
        assert!(reserve(&h, "c"));
        release(&h, "b");
        assert!(!h.per_chat.lock().unwrap().contains_key("b"), "an emptied chat leaves no entry");
    }

    #[test]
    fn a_panicking_handler_still_gives_its_slot_back() {
        let h = Arc::new(handler(1, 1));
        assert!(reserve(&h, "a"));
        let router = Arc::new(XdcRouter::default());
        router.active.lock().unwrap().insert("T".into(), Active::default());
        let slot = Slot {
            router: router.clone(),
            handler: h.clone(),
            topic: "T".into(),
            chat_id: "a".into(),
            bot: unsafe_bot(),
            app: Xdc::new("a".into(), "m".into(), Attachment::default()),
            finished: None,
        };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _slot = slot;
            panic!("handler bug");
        }));
        assert!(r.is_err());
        assert_eq!(*h.live.lock().unwrap(), 0);
        assert!(h.per_chat.lock().unwrap().is_empty());
        assert!(reserve(&h, "a"), "the slot is usable again");
    }

    /// A bot value for bookkeeping tests; never logged in, never used to send.
    fn unsafe_bot() -> VectorBot {
        VectorBot::test_stub()
    }
}
