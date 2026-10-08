//! The transport meta-glue: one egress decision for every TCP connection Vector opens, and one
//! process bridge that any anonymity network plugs into.
//!
//! Clearnet is `Egress::Direct`. Every other kind (Tor, I2P, …) is a [`Transport`] installed in
//! the [`host`] slot, reached through the authenticated SOCKS5 [`bridge`]. A chosen kind that is
//! not ready refuses in process: nothing ever falls back to a direct socket.
//!
//! Inside `community/` refer to this module as `crate::transport::…`: the name collides with
//! `community::transport`.

pub mod aliases;
pub mod budget;
pub mod cycle;
pub mod guard;
pub mod host;
pub mod i2p_config;
pub mod kinds;
pub mod prefs;
pub mod prelogin;
pub mod realtime;
pub mod route;
pub mod status;
pub mod ws;
#[cfg(not(target_arch = "wasm32"))]
pub mod bridge;
#[cfg(not(target_arch = "wasm32"))]
pub mod runtime;
#[cfg(all(feature = "twin-check", not(target_arch = "wasm32")))]
pub mod twins;
#[cfg(test)]
mod tests;

#[cfg(not(target_arch = "wasm32"))]
pub use runtime::{runtime, spawn_on};

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;

pub use budget::{budget, Budgets, Op};
pub use route::{validate_relay_url, Dest, Refusal, Route};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Clearnet,
    Tor,
    I2p,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Clearnet, Kind::Tor, Kind::I2p];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Clearnet => "clearnet",
            Kind::Tor => "tor",
            Kind::I2p => "i2p",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Clearnet => "Clearnet",
            Kind::Tor => "Tor",
            Kind::I2p => "I2P",
        }
    }

    /// Whether this build can run the kind. Never true for an anonymity kind on wasm32.
    pub fn compiled(self) -> bool {
        match self {
            Kind::Clearnet => true,
            Kind::Tor => cfg!(all(feature = "tor", not(target_arch = "wasm32"))),
            Kind::I2p => cfg!(all(feature = "i2p", not(target_arch = "wasm32"))),
        }
    }

    /// Names only this kind can reach. Known in every build, so a build without the kind still
    /// refuses them before any DNS.
    pub fn native_suffixes(self) -> &'static [&'static str] {
        match self {
            Kind::Clearnet => &[],
            Kind::Tor => &[".onion"],
            Kind::I2p => &[".i2p"],
        }
    }

    /// Whether this build reaches the kind's own addresses: a `.onion` needs the embedded Tor.
    pub fn reaches_native(self) -> bool {
        match self {
            Kind::I2p => true,
            Kind::Tor => cfg!(all(feature = "tor", not(target_arch = "wasm32"))),
            Kind::Clearnet => false,
        }
    }

    /// What a server's address inside this network is called, for a twin.
    pub fn address_noun(self) -> &'static str {
        match self {
            Kind::Tor => "onion address",
            Kind::I2p => "I2P address",
            Kind::Clearnet => "address",
        }
    }

    pub fn budgets(self) -> &'static Budgets {
        budget::table(self)
    }

    fn code(self) -> u8 {
        match self {
            Kind::Clearnet => 1,
            Kind::Tor => 2,
            Kind::I2p => 3,
        }
    }

    fn from_code(c: u8) -> Option<Kind> {
        match c {
            1 => Some(Kind::Clearnet),
            2 => Some(Kind::Tor),
            3 => Some(Kind::I2p),
            _ => None,
        }
    }
}

/// Compiled kinds, in `Kind` order.
pub fn supported() -> Vec<Kind> {
    Kind::ALL.into_iter().filter(|k| k.compiled()).collect()
}

/// Which identity a connection speaks for. I2P gives each lane its own destination, so a host a
/// stranger names only ever learns the Shared one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lane {
    Account,
    Shared,
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
impl Lane {
    fn tag(self) -> char {
        match self {
            Lane::Account => 'a',
            Lane::Shared => 's',
        }
    }

    fn from_tag(c: &str) -> Option<Lane> {
        match c {
            "a" => Some(Lane::Account),
            "s" => Some(Lane::Shared),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExitPolicy {
    #[default]
    Allow,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportState {
    /// Preference not loaded or unparseable: blocked.
    Unknown,
    Clearnet,
    /// The chosen kind's instance is installed, owned by the live session and ready.
    Active { kind: Kind },
    /// Kind chosen but not ready (starting, retrying, failed, not in this build): blocked.
    RequiredButInactive { kind: Kind },
}

/// What a proxied connection proves to the bridge: the session that built the client, the epoch
/// it was decided in, and the lane it speaks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ticket {
    pub owner: u64,
    pub epoch: u64,
    pub lane: Lane,
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
impl Ticket {
    pub(crate) fn encode(&self) -> String {
        format!("t1.{:x}.{:x}.{}", self.owner, self.epoch, self.lane.tag())
    }

    pub(crate) fn decode(s: &str) -> Option<Ticket> {
        let mut parts = s.split('.');
        if parts.next()? != "t1" {
            return None;
        }
        let owner = u64::from_str_radix(parts.next()?, 16).ok()?;
        let epoch = u64::from_str_radix(parts.next()?, 16).ok()?;
        let lane = Lane::from_tag(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        Some(Ticket { owner, epoch, lane })
    }

    /// Owned by the live session and decided in the current epoch.
    pub fn is_live(&self) -> bool {
        self.owner == crate::db::live_session_id() && self.epoch == epoch()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Egress {
    Direct,
    Proxy(Ticket),
    Refuse(ConnectError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectError {
    Refused(Refusal),
    /// `None` while the preference is Unknown.
    NotReady(Option<Kind>),
    /// The chosen kind is blocked with a status reason: that reason's text.
    Blocked(String),
    /// A ticket from an older epoch, or a session that is no longer live.
    Stale,
    RouterDown { port: u16 },
    UnknownName,
    /// The destination or exit is unreachable; the detail is for logs (Tor: arti's text).
    Unreachable(String),
    /// The exit was reached and couldn't reach the server itself; the exit's name.
    ExitUnreachable(String),
    NoExit,
    ExitRefusedPort(u16),
    Timeout,
    /// The SOCKS client hung up mid-dial.
    Cancelled,
    /// The bridge could not bind; the detail is for logs.
    Bridge(String),
}

impl ConnectError {
    pub fn socks_reply(&self) -> u8 {
        match self {
            ConnectError::Refused(_) | ConnectError::Stale => 0x02,
            ConnectError::NotReady(_) | ConnectError::Blocked(_) | ConnectError::Bridge(_) | ConnectError::Cancelled => 0x01,
            ConnectError::RouterDown { .. } => 0x03,
            ConnectError::UnknownName
            | ConnectError::Unreachable(_)
            | ConnectError::ExitUnreachable(_)
            | ConnectError::NoExit
            | ConnectError::ExitRefusedPort(_) => 0x04,
            ConnectError::Timeout => 0x06,
        }
    }

    /// The line the UI shows. `Unreachable` keeps arti's text under Tor.
    pub fn text(&self) -> String {
        match self {
            ConnectError::Refused(r) => r.text(),
            ConnectError::NotReady(None) => "Vector is still connecting.".into(),
            ConnectError::NotReady(Some(k)) => format!("{} is still connecting.", k.label()),
            ConnectError::Blocked(t) => t.clone(),
            ConnectError::Stale => "The network changed. Try again.".into(),
            ConnectError::RouterDown { port } => format!("Can't reach your I2P router at 127.0.0.1:{port}."),
            ConnectError::UnknownName => "This I2P name isn't in your router's address book. Use its .b32.i2p address.".into(),
            ConnectError::Unreachable(detail) => match preference() {
                Some(Kind::I2p) => "This I2P address isn't online right now.".into(),
                _ => detail.clone(),
            },
            ConnectError::ExitUnreachable(name) => format!("The {name} outproxy couldn't reach this server."),
            ConnectError::NoExit => "No outproxy is reachable right now.".into(),
            ConnectError::ExitRefusedPort(p) => format!("No outproxy here allows port {p}."),
            ConnectError::Timeout => "Timed out.".into(),
            ConnectError::Cancelled => "Cancelled.".into(),
            ConnectError::Bridge(_) => "Vector couldn't open its local proxy.".into(),
        }
    }

    /// The network will come back on its own: a policy refusal won't, nor will a stale owner (a
    /// session never becomes live again once replaced), nor a block only the user can lift.
    pub fn is_transient(&self) -> bool {
        match self {
            ConnectError::NotReady(_) => true,
            ConnectError::Blocked(_) => !status::waits_for_user(),
            _ => false,
        }
    }

    /// Our side of the connection failed (the network, its router, its exits), so the failure
    /// says nothing about the server.
    pub fn is_network(&self) -> bool {
        matches!(
            self,
            ConnectError::NotReady(_)
                | ConnectError::Blocked(_)
                | ConnectError::Stale
                | ConnectError::RouterDown { .. }
                | ConnectError::NoExit
                | ConnectError::ExitRefusedPort(_)
                | ConnectError::Bridge(_)
        )
    }
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text())
    }
}

impl std::error::Error for ConnectError {}

pub trait AsyncStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin> AsyncStream for T {}
pub type BoxedStream = Box<dyn AsyncStream>;

/// A kind's parsed config, opaque to shared code; each kind downcasts its own type.
pub type KindConfig = std::sync::Arc<dyn std::any::Any + Send + Sync>;

/// The live session's routing inputs, read per dial so an instance never holds a stale copy.
pub struct RouteCtx<'a> {
    pub aliases: &'a aliases::AliasTable,
    pub config: &'a KindConfig,
}

#[derive(Clone, Debug, Default)]
pub struct Dialed {
    /// The outproxy id an `Exit` used.
    pub outproxy: Option<String>,
}

#[async_trait::async_trait]
pub trait Transport: Send + Sync + 'static {
    fn kind(&self) -> Kind;
    /// This kind's rules, after the shared pre-router. Pure, no I/O.
    fn route(&self, dest: &Dest, port: u16, ctx: &RouteCtx) -> Route;
    /// Open the stream `route` names for `lane`. Ok only once the far end exists.
    async fn dial(&self, route: &Route, lane: Lane) -> Result<(BoxedStream, Dialed), ConnectError>;
    fn ready(&self) -> bool;
    fn kind_status(&self) -> status::KindStatus;
    /// Unlinkable from here on.
    async fn new_identity(&self);
    async fn shutdown(&self);
    fn into_any(self: std::sync::Arc<Self>) -> std::sync::Arc<dyn std::any::Any + Send + Sync>;
    /// The instance itself, for a factory's `compatible` (which holds no `Arc`).
    fn as_any(&self) -> Option<&(dyn std::any::Any + Send + Sync)> {
        None
    }
}

pub struct StartCtx {
    pub owner: u64,
    pub started_prelogin: bool,
    /// Tor: state and cache dirs.
    pub dirs: Option<(std::path::PathBuf, std::path::PathBuf)>,
    pub config: KindConfig,
}

#[async_trait::async_trait]
pub trait TransportFactory: Send + Sync + 'static {
    fn kind(&self) -> Kind;
    /// Tor bootstraps within `budget(Startup)` and returns a ready instance; others may return at
    /// once and report Ready through `host::notify`.
    async fn start(&self, ctx: StartCtx) -> Result<std::sync::Arc<dyn Transport>, String>;
    /// Keep a welcome-screen instance when an existing account is unlocked.
    fn adopt_on_unlock(&self) -> bool;
    /// Whether `inst` can serve an account whose config is `cfg`.
    fn compatible(&self, inst: &dyn Transport, cfg: &KindConfig) -> bool;
}

// ── Epoch and policy generation ──────────────────────────────────────────────

static EPOCH: AtomicU64 = AtomicU64::new(1);
static POLICY_GEN: AtomicU64 = AtomicU64::new(1);

fn epoch_tx() -> &'static tokio::sync::watch::Sender<u64> {
    static TX: OnceLock<tokio::sync::watch::Sender<u64>> = OnceLock::new();
    TX.get_or_init(|| tokio::sync::watch::channel(EPOCH.load(Ordering::Acquire)).0)
}

/// Bumped only when the egress decision can change: preference, live session, an instance
/// installed or removed, or its readiness.
pub fn epoch() -> u64 {
    EPOCH.load(Ordering::Acquire)
}

/// Bumped when a routing input of the live session changes: exit policy, outproxies, aliases.
pub fn policy_gen() -> u64 {
    POLICY_GEN.load(Ordering::Acquire)
}

pub(crate) fn bump_policy_gen() -> u64 {
    POLICY_GEN.fetch_add(1, Ordering::AcqRel) + 1
}

/// Advance the epoch: drop every HTTP client, abort every bridge connection of an older epoch,
/// then wake guarded streams and body reads.
pub(crate) fn bump_epoch() -> u64 {
    let next = EPOCH.fetch_add(1, Ordering::AcqRel) + 1;
    crate::net::forget_clients();
    #[cfg(not(target_arch = "wasm32"))]
    bridge::abort_where(|c| c.ticket.is_some_and(|t| t.epoch < next));
    epoch_tx().send_replace(next);
    next
}

/// Resolves once `epoch() != since`.
pub async fn changed(since: u64) {
    let mut rx = epoch_tx().subscribe();
    loop {
        if *rx.borrow_and_update() != since || epoch() != since {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// The installed session changed: guarded sockets of the outgoing account close.
pub(crate) fn on_session_installed(prev: u64, next: u64) {
    if prev != next {
        bump_epoch();
    }
}

// ── Preference and state ─────────────────────────────────────────────────────

static STRICT: AtomicBool = AtomicBool::new(false);

/// First-party binaries that hold accounts call this once at startup: every session from here on
/// starts Unknown (blocked) until its stored network is loaded.
pub fn strict_boot() {
    STRICT.store(true, Ordering::Release);
}

pub(crate) fn is_strict() -> bool {
    STRICT.load(Ordering::Acquire)
}

#[cfg(test)]
pub(crate) fn set_strict_for_test(on: bool) {
    STRICT.store(on, Ordering::Release);
}

/// The live session's chosen kind. `None` = Unknown.
pub fn preference() -> Option<Kind> {
    prefs::live().kind()
}

/// Set the live session's kind. Bumps the epoch when it changes; a kind change also drops that
/// session's realtime consent.
pub fn set_preference(kind: Option<Kind>) {
    prefs::set_kind(&crate::db::live_session(), kind);
}

pub fn state() -> TransportState {
    state_for(preference())
}

fn state_for(pref: Option<Kind>) -> TransportState {
    match pref {
        None => TransportState::Unknown,
        Some(Kind::Clearnet) => TransportState::Clearnet,
        Some(k) => match host::active() {
            Some(a) if a.kind == k && a.owner() == crate::db::live_session_id() && a.transport().ready() => {
                TransportState::Active { kind: k }
            }
            _ => TransportState::RequiredButInactive { kind: k },
        },
    }
}

/// Why nothing can leave right now, while the chosen network isn't usable.
pub fn blocked_reason() -> Option<String> {
    match state() {
        TransportState::Clearnet | TransportState::Active { .. } => None,
        TransportState::Unknown => Some(ConnectError::NotReady(None).text()),
        TransportState::RequiredButInactive { kind } => Some(blocked_error(kind).text()),
    }
}

/// Why the chosen kind refuses right now, for `RequiredButInactive`.
fn blocked_error(k: Kind) -> ConnectError {
    match status::reason_for(k) {
        Some(r) => ConnectError::Blocked(r.text),
        None => ConnectError::NotReady(Some(k)),
    }
}

/// The one decision every TCP egress makes. `owner` is the session the client was built under.
pub fn egress(owner: u64, lane: Lane, host: &str, port: u16) -> Egress {
    if owner != crate::db::live_session_id() {
        return Egress::Refuse(ConnectError::Stale);
    }
    let dest = match Dest::parse(host) {
        Some(d) => d,
        None => return Egress::Refuse(ConnectError::Refused(Refusal::BadName)),
    };
    let pref = preference();
    if let Some(r) = route::pre_route(pref.unwrap_or(Kind::Clearnet), &dest) {
        return Egress::Refuse(ConnectError::Refused(r));
    }
    match state_for(pref) {
        TransportState::Clearnet => Egress::Direct,
        TransportState::Unknown => Egress::Refuse(ConnectError::NotReady(None)),
        TransportState::RequiredButInactive { kind } => Egress::Refuse(blocked_error(kind)),
        TransportState::Active { kind } => {
            let Some(active) = host::active().filter(|a| a.kind == kind) else {
                return Egress::Refuse(ConnectError::NotReady(Some(kind)));
            };
            let p = prefs::live();
            let (aliases, config) = (p.aliases(), p.config(kind));
            let ctx = RouteCtx { aliases: &aliases, config: &config };
            match active.transport().route(&dest, port, &ctx) {
                Route::Refuse(r) => Egress::Refuse(ConnectError::Refused(r)),
                _ => Egress::Proxy(Ticket { owner, epoch: epoch(), lane }),
            }
        }
    }
}

/// Host and port of a URL, with the scheme's default port.
pub(crate) fn host_port(url_or_host: &str) -> Option<(String, u16)> {
    if let Ok(u) = url::Url::parse(url_or_host) {
        if let (Some(h), Some(p)) = (u.host_str(), u.port_or_known_default()) {
            return Some((h.trim_start_matches('[').trim_end_matches(']').to_string(), p));
        }
    }
    if url_or_host.contains("://") || url_or_host.is_empty() {
        return None;
    }
    Some((url_or_host.to_string(), 443))
}

/// The refusal text when the live account's route can never reach this URL or host (relay
/// lists, community targets). Transient blocks are not refusals.
pub fn refuses(url_or_host: &str) -> Option<String> {
    refusal(url_or_host).map(|r| r.text())
}

/// The rule behind [`refuses`].
pub fn refusal(url_or_host: &str) -> Option<Refusal> {
    let (host, port) = host_port(url_or_host)?;
    let dest = Dest::parse(&host)?;
    let pref = preference()?;
    if let Some(r) = route::pre_route(pref, &dest) {
        return Some(r);
    }
    if let TransportState::Active { kind } = state_for(Some(pref)) {
        let active = host::active().filter(|a| a.kind == kind)?;
        let p = prefs::live();
        let (aliases, config) = (p.aliases(), p.config(kind));
        if let Route::Refuse(r) = active.transport().route(&dest, port, &RouteCtx { aliases: &aliases, config: &config }) {
            return Some(r);
        }
    }
    None
}

pub fn route_view(url: &str) -> status::RouteView {
    status::route_view(url)
}

/// Hold until the chosen kind is ready or `max` passes. A block only the user can lift (a
/// failed start, a router that turned Vector down) ends the wait at once with its reason.
pub async fn wait_ready(max: std::time::Duration) -> Result<(), String> {
    let deadline = web_time::Instant::now() + max;
    loop {
        let since = epoch();
        match state() {
            TransportState::Clearnet | TransportState::Active { .. } => return Ok(()),
            TransportState::Unknown => {
                if web_time::Instant::now() >= deadline {
                    return Err(ConnectError::NotReady(None).text());
                }
            }
            TransportState::RequiredButInactive { kind } => {
                if status::waits_for_user() || web_time::Instant::now() >= deadline {
                    return Err(blocked_error(kind).text());
                }
            }
        }
        let left = deadline.saturating_duration_since(web_time::Instant::now());
        // A ready flip always bumps the epoch; the short tick covers a state read between bumps.
        let _ = crate::rt::time::timeout(left.min(std::time::Duration::from_secs(1)), changed(since)).await;
    }
}
