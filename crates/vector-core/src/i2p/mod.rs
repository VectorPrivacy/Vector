//! I2P as a transport kind, over a router the user already runs (i2pd, Java I2P,
//! i2pd-android) through its SAMv3 bridge.
//!
//! An instance keeps two transient STREAM sessions, one per lane, so a host a stranger names
//! only ever learns the Shared destination. `.i2p` hosts are dialed inside I2P, clearnet hosts
//! through the user's outproxy list, and an alias twin carries TLS on 443 untouched. Nothing
//! here ever accepts a connection: sessions are client-only and their LeaseSets unpublished.

pub mod keeper;
pub mod names;
pub mod outproxy;
pub mod probe;
pub mod sam;
#[cfg(test)]
mod mock_sam;
#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tokio::net::TcpStream;
use web_time::Instant;

use crate::transport::host::{self, ActiveTransport, TransportEvent};
use crate::transport::i2p_config::I2pConfig;
use crate::transport::status::{KindStatus, Phase, Reason, Step};
use crate::transport::{
    BoxedStream, ConnectError, Dest, Dialed, ExitPolicy, Kind, KindConfig, Lane, Refusal, Route, RouteCtx, StartCtx, Transport,
    TransportFactory,
};

use sam::{SamAuth, SamVersion, StreamFail};

/// The keeper's clock. Tests run it in milliseconds.
#[derive(Clone, Debug)]
pub(crate) struct Timing {
    pub backoff: Vec<Duration>,
    pub watch: Duration,
    pub not_sam: Duration,
    pub rejected: Duration,
    pub session_cap: Duration,
    pub head_deadline: Duration,
    /// How often an unreachable router's port is checked for its return.
    pub port_watch: Duration,
    /// How long after its sessions are made the router may not know an outproxy's LeaseSet yet.
    pub fresh_session: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        let s = Duration::from_secs;
        Timing {
            backoff: vec![s(2), s(5), s(10), s(20), s(40), s(60)],
            watch: s(15),
            not_sam: s(60),
            rejected: s(120),
            session_cap: sam::SESSION_CAP,
            head_deadline: outproxy::HEAD_DEADLINE,
            port_watch: s(2),
            fresh_session: outproxy::FRESH_SESSION,
        }
    }
}

/// Where the keeper is. Only the keeper task changes it.
#[derive(Clone, Debug)]
pub(crate) enum I2pPhase {
    Probing,
    RouterUnreachable { next: Instant },
    SamRejected { next: Instant, code: &'static str, text: String },
    CreatingSessions { since: Instant },
    Ready { since: Instant },
    SessionFailed { next: Instant, why: String },
    Lost { next: Instant },
    /// The session that owns this instance is no longer on screen: no router sessions held.
    Parked,
}

impl I2pPhase {
    fn name(&self) -> &'static str {
        match self {
            I2pPhase::Probing => "probing",
            I2pPhase::RouterUnreachable { .. } => "router_unreachable",
            I2pPhase::SamRejected { .. } => "sam_rejected",
            I2pPhase::CreatingSessions { .. } => "creating_sessions",
            I2pPhase::Ready { .. } => "ready",
            I2pPhase::SessionFailed { .. } => "session_failed",
            I2pPhase::Lost { .. } => "lost",
            I2pPhase::Parked => "parked",
        }
    }
}

struct PhaseState {
    phase: I2pPhase,
    version: Option<SamVersion>,
    /// Whether the router answered the last time anyone asked.
    router_up: Option<bool>,
}

/// The destinations an instance dials from: one transient session per lane.
#[derive(Clone, Debug)]
pub(crate) struct Sessions {
    pub account: String,
    pub shared: String,
    /// The `.b32.i2p` each lane is seen as, when the router said.
    pub account_address: Option<String>,
    pub shared_address: Option<String>,
}

pub(crate) struct Inner {
    pub(crate) port: u16,
    pub(crate) auth: Option<SamAuth>,
    pub(crate) owner: u64,
    pub(crate) timing: Timing,
    sessions: RwLock<Option<Arc<Sessions>>>,
    state: Mutex<PhaseState>,
    pub(crate) outproxies: outproxy::Health,
    pub(crate) names: names::NameCache,
    /// Wakes the keeper: retry now, or check the sessions now.
    pub(crate) kick: tokio::sync::Notify,
    renew: AtomicBool,
    renewed: tokio::sync::Notify,
    keeper: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

pub struct I2pTransport {
    pub(crate) inner: Arc<Inner>,
}

fn auth_of(cfg: &I2pConfig) -> Option<SamAuth> {
    match (cfg.sam_user.as_deref(), cfg.sam_password.as_deref()) {
        (Some(u), Some(p)) if !u.is_empty() && !p.is_empty() => Some(SamAuth::new(u, p)),
        _ => None,
    }
}

fn secs_until(t: Instant) -> u64 {
    let now = Instant::now();
    if t <= now {
        0
    } else {
        t.duration_since(now).as_secs_f64().ceil() as u64
    }
}

/// Read the live account's I2P settings in place: a copy would leave the SAM password behind in
/// freed memory on every status view and every exit dial.
fn with_live_config<R>(f: impl FnOnce(&I2pConfig) -> R) -> R {
    let cfg = crate::transport::prefs::live().config(Kind::I2p);
    match cfg.downcast_ref::<I2pConfig>() {
        Some(c) => f(c),
        None => f(&I2pConfig::default()),
    }
}

impl Inner {
    fn new(cfg: &I2pConfig, owner: u64, timing: Timing) -> Self {
        Inner {
            port: cfg.sam_port,
            auth: auth_of(cfg),
            owner,
            timing,
            sessions: RwLock::new(None),
            state: Mutex::new(PhaseState { phase: I2pPhase::Probing, version: None, router_up: None }),
            outproxies: outproxy::Health::default(),
            names: names::NameCache::default(),
            kick: tokio::sync::Notify::new(),
            renew: AtomicBool::new(false),
            renewed: tokio::sync::Notify::new(),
            keeper: Mutex::new(None),
        }
    }

    pub(crate) fn owner_live(&self) -> bool {
        crate::db::live_session_id() == self.owner
    }

    pub(crate) fn sessions(&self) -> Option<Arc<Sessions>> {
        self.sessions.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn set_sessions(&self, s: Option<Sessions>) {
        *self.sessions.write().unwrap_or_else(|e| e.into_inner()) = s.map(Arc::new);
    }

    pub(crate) fn nick(&self, lane: Lane) -> Option<String> {
        self.sessions().map(|s| match lane {
            Lane::Account => s.account.clone(),
            Lane::Shared => s.shared.clone(),
        })
    }

    pub(crate) fn phase(&self) -> I2pPhase {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).phase.clone()
    }

    pub(crate) fn is_ready(&self) -> bool {
        matches!(self.phase(), I2pPhase::Ready { .. })
    }

    /// How long the current sessions have been up.
    pub(crate) fn session_age(&self) -> Option<std::time::Duration> {
        match self.phase() {
            I2pPhase::Ready { since } => Some(since.elapsed()),
            _ => None,
        }
    }

    pub(crate) fn set_router(&self, up: bool, version: Option<SamVersion>) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.router_up = Some(up);
        if version.is_some() {
            st.version = version;
        }
    }

    /// Move to `next` and tell the host. A readiness flip is Ready or Lost (they move the epoch);
    /// anything else only refreshes the view.
    pub(crate) fn set_phase(self: &Arc<Self>, next: I2pPhase, recovered: bool) {
        let (was, now) = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let was = matches!(st.phase, I2pPhase::Ready { .. });
            st.phase = next;
            (was, matches!(st.phase, I2pPhase::Ready { .. }))
        };
        crate::log_debug!("[I2P] {}", self.phase().name());
        let Some(a) = self.active_entry() else { return };
        let (instance, owner) = (a.id(), a.owner());
        match (was, now) {
            (false, true) => host::notify(TransportEvent::Ready { kind: Kind::I2p, instance, owner, recovered }),
            (true, false) => host::notify(TransportEvent::Lost { kind: Kind::I2p, instance, owner }),
            _ if owner == crate::db::live_session_id() => host::notify(TransportEvent::Changed),
            _ => {}
        }
    }

    /// The host's entry for this instance, when it is the installed one.
    pub(crate) fn active_entry(self: &Arc<Self>) -> Option<Arc<ActiveTransport>> {
        let a = host::active()?;
        let t = a.transport().as_any()?.downcast_ref::<I2pTransport>()?;
        Arc::ptr_eq(&t.inner, self).then_some(a)
    }

    /// A router-side failure as the caller sees it. Anything that hints the session or router
    /// went away wakes the keeper to check now.
    pub(crate) fn fail(&self, e: StreamFail) -> ConnectError {
        match e {
            StreamFail::RouterDown => {
                self.kick.notify_one();
                ConnectError::RouterDown { port: self.port }
            }
            StreamFail::SessionGone => {
                self.kick.notify_one();
                ConnectError::NotReady(Some(Kind::I2p))
            }
            StreamFail::SessionSuspect(m) => {
                self.kick.notify_one();
                ConnectError::Unreachable(m)
            }
            StreamFail::Unreachable(m) => ConnectError::Unreachable(m),
            StreamFail::Timeout => ConnectError::Timeout,
            StreamFail::BadKey => ConnectError::Refused(Refusal::BadName),
        }
    }

    /// A stream to `dest` (a `.b32.i2p` address or an address-book name) on `port`. A router
    /// that only takes base64 destinations gets a lookup and one retry.
    pub(crate) async fn open_raw(&self, nick: &str, dest: &str, port: u16) -> Result<TcpStream, StreamFail> {
        let auth = self.auth.as_ref();
        if crate::transport::route::is_b32(dest) {
            return match sam::connect(self.port, auth, nick, dest, port).await {
                Err(StreamFail::BadKey) => {
                    let b64 = sam::lookup(self.port, auth, dest).await?;
                    sam::connect(self.port, auth, nick, &b64, port).await
                }
                r => r,
            };
        }
        let b64 = match self.names.get(dest) {
            Some(v) => v,
            None => {
                let v = sam::lookup(self.port, auth, dest).await?;
                self.names.put(dest, v.clone());
                v
            }
        };
        match sam::connect(self.port, auth, nick, &b64, port).await {
            Err(StreamFail::BadKey) => {
                self.names.forget(dest);
                Err(StreamFail::BadKey)
            }
            r => r,
        }
    }

    pub(crate) async fn open(&self, nick: &str, dest: &str, port: u16) -> Result<TcpStream, ConnectError> {
        match self.open_raw(nick, dest, port).await {
            Ok(s) => Ok(s),
            Err(StreamFail::BadKey) if !crate::transport::route::is_b32(dest) => Err(ConnectError::UnknownName),
            Err(e) => Err(self.fail(e)),
        }
    }

    fn router_step(&self, st: &PhaseState) -> Step {
        let ok = || Step::new("router", "Router", "ok", format!("SAM {} on port {}", st.version.map(|v| v.to_string()).unwrap_or_else(|| "3".into()), self.port));
        let fail = || Step::new("router", "Router", "fail", "Not answering");
        let looking = || Step::new("router", "Router", "pending", "Looking…");
        match &st.phase {
            I2pPhase::Probing | I2pPhase::Parked => looking(),
            I2pPhase::RouterUnreachable { .. } | I2pPhase::SamRejected { .. } => fail(),
            I2pPhase::CreatingSessions { .. } | I2pPhase::Ready { .. } | I2pPhase::SessionFailed { .. } => ok(),
            I2pPhase::Lost { .. } => match st.router_up {
                Some(true) => ok(),
                Some(false) => fail(),
                None => looking(),
            },
        }
    }

    fn tunnels_step(st: &PhaseState) -> Step {
        match &st.phase {
            I2pPhase::CreatingSessions { since } => {
                Step::new("tunnels", "Tunnels", "pending", format!("Building… {}s", since.elapsed().as_secs()))
            }
            I2pPhase::Ready { .. } => Step::new("tunnels", "Tunnels", "ok", "Ready"),
            I2pPhase::SessionFailed { .. } => Step::new("tunnels", "Tunnels", "fail", "Failed"),
            _ => Step::new("tunnels", "Tunnels", "pending", "Waiting…"),
        }
    }

    fn exit_step(&self, st: &PhaseState, cfg: &I2pConfig) -> Step {
        let list = cfg.outproxy_list();
        if cfg.exit == ExitPolicy::Off {
            return Step::new("exit", "Clearnet", "off", "Off (I2P-Only)");
        }
        // Nothing reaches an outproxy without sessions: the last one that worked says nothing now.
        if !matches!(st.phase, I2pPhase::Ready { .. }) {
            return Step::new("exit", "Clearnet", "pending", "Waiting…");
        }
        // Health counts failures; a tunnel still carrying traffic means something gets through.
        let carrying = || {
            crate::transport::bridge::count_where(|c| {
                c.spliced && c.ticket.is_some_and(|t| t.owner == self.owner) && c.outproxy.as_ref().is_some_and(|id| list.iter().any(|o| &o.id == id))
            }) > 0
        };
        if self.outproxies.all_failing(&list) && !carrying() {
            return Step::new("exit", "Clearnet", "fail", "No outproxy answering");
        }
        let worked = self.outproxies.last_ok().and_then(|id| list.iter().find(|o| o.id == id).map(|o| o.name.clone()));
        match worked {
            Some(name) => Step::new("exit", "Clearnet", "ok", format!("Through {name}")),
            None => Step::new("exit", "Clearnet", "unknown", "Not used yet"),
        }
    }

    fn status(&self) -> KindStatus {
        with_live_config(|cfg| self.status_with(cfg))
    }

    fn status_with(&self, cfg: &I2pConfig) -> KindStatus {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let port = self.port;
        let (phase, reason, retry_in) = match &st.phase {
            I2pPhase::Probing | I2pPhase::CreatingSessions { .. } => (Phase::Starting, None, None),
            I2pPhase::Ready { .. } => (Phase::Ready, None, None),
            I2pPhase::RouterUnreachable { next } => (
                Phase::Waiting,
                Some(Reason::new("router_unreachable", format!("Can't reach your I2P router at 127.0.0.1:{port}."))),
                Some(secs_until(*next)),
            ),
            I2pPhase::SamRejected { next, code, text } => (Phase::Waiting, Some(Reason::new(code, text.clone())), Some(secs_until(*next))),
            I2pPhase::SessionFailed { next, why } => (
                Phase::Waiting,
                Some(Reason::new("session_failed", format!("Your router couldn't open a session: {why}"))),
                Some(secs_until(*next)),
            ),
            I2pPhase::Lost { next } => {
                (Phase::Waiting, Some(Reason::new("session_lost", "Lost the I2P session. Reconnecting.")), Some(secs_until(*next)))
            }
            I2pPhase::Parked => (Phase::Waiting, None, None),
        };
        let session_age = match &st.phase {
            I2pPhase::Ready { since } => Some(since.elapsed().as_secs()),
            _ => None,
        };
        let addresses = self.sessions().filter(|_| session_age.is_some()).map(|s| {
            serde_json::json!({ "account": s.account_address, "shared": s.shared_address })
        });
        let steps = vec![self.router_step(&st), Self::tunnels_step(&st), self.exit_step(&st, cfg)];
        let detail = serde_json::json!({
            "sam_port": port,
            "sam_version": st.version.map(|v| v.to_string()),
            "sam_auth": self.auth.is_some(),
            "session_age": session_age,
            "addresses": addresses,
            "exit": cfg.exit,
            "outproxies": self.outproxies.view(&cfg.outproxy_list()),
        });
        KindStatus { phase, progress: None, reason, retry_in, steps, detail }
    }
}

impl I2pTransport {
    pub(crate) fn new(cfg: &I2pConfig, owner: u64, timing: Timing) -> Self {
        I2pTransport { inner: Arc::new(Inner::new(cfg, owner, timing)) }
    }

    pub(crate) fn spawn_keeper(&self) {
        let inner = self.inner.clone();
        // spawn-detached: the keeper owns only this instance's router sessions.
        let h = crate::transport::spawn_on(keeper::run(inner));
        *self.inner.keeper.lock().unwrap_or_else(|e| e.into_inner()) = Some(h);
    }

    pub fn sam_port(&self) -> u16 {
        self.inner.port
    }

    /// Whether this instance talks to the router `cfg` names (same port and credentials).
    pub fn serves(&self, cfg: &I2pConfig) -> bool {
        self.inner.port == cfg.sam_port && self.inner.auth == auth_of(cfg)
    }

    /// Skip the keeper's wait and try the router now.
    pub fn retry_now(&self) {
        self.inner.kick.notify_one();
    }

    /// The keeper's phase, for logs and the soak tool.
    pub fn phase_name(&self) -> &'static str {
        self.inner.phase().name()
    }
}

/// Checks an instance's two sessions with the router without handing out their names.
pub struct SessionProbe {
    port: u16,
    auth: Option<SamAuth>,
    nicks: [String; 2],
}

impl SessionProbe {
    /// Account lane first, then Shared.
    pub async fn check(&self) -> [sam::Liveness; 2] {
        let (a, s) = tokio::join!(
            sam::liveness(self.port, self.auth.as_ref(), &self.nicks[0]),
            sam::liveness(self.port, self.auth.as_ref(), &self.nicks[1]),
        );
        [a, s]
    }
}

impl I2pTransport {
    /// A probe of the sessions up right now, if any.
    pub fn session_probe(&self) -> Option<SessionProbe> {
        let s = self.inner.sessions()?;
        Some(SessionProbe { port: self.inner.port, auth: self.inner.auth.clone(), nicks: [s.account.clone(), s.shared.clone()] })
    }
}

impl Drop for I2pTransport {
    fn drop(&mut self) {
        if let Some(h) = self.inner.keeper.lock().unwrap_or_else(|e| e.into_inner()).take() {
            h.abort();
        }
    }
}

#[async_trait::async_trait]
impl Transport for I2pTransport {
    fn kind(&self) -> Kind {
        Kind::I2p
    }

    fn route(&self, dest: &Dest, port: u16, ctx: &RouteCtx) -> Route {
        crate::transport::route::route_i2p(dest, port, ctx)
    }

    async fn dial(&self, route: &Route, lane: Lane) -> Result<(BoxedStream, Dialed), ConnectError> {
        let inner = &self.inner;
        let nick = inner.nick(lane).ok_or(ConnectError::NotReady(Some(Kind::I2p)))?;
        match route {
            Route::Native { host, port } => {
                let s = inner.open(&nick, host, *port).await?;
                Ok((Box::new(s), Dialed::default()))
            }
            Route::Twin { via, port, .. } => {
                let s = inner.open(&nick, via, *port).await?;
                Ok((Box::new(s), Dialed::default()))
            }
            Route::Exit { host, port } => {
                let list = with_live_config(I2pConfig::outproxy_list);
                let allowed = |id: &str| host::outproxy_allowed(Kind::I2p, id);
                let (s, id) = outproxy::connect(inner, &nick, &list, host, *port, &allowed).await?;
                Ok((Box::new(s), Dialed { outproxy: Some(id) }))
            }
            Route::Refuse(r) => Err(ConnectError::Refused(*r)),
        }
    }

    fn ready(&self) -> bool {
        self.inner.is_ready()
    }

    fn kind_status(&self) -> KindStatus {
        self.inner.status()
    }

    /// Both destinations replaced; returns once the old ones are gone (or the keeper is busy
    /// building, when the next sessions are fresh anyway).
    async fn new_identity(&self) {
        if !self.inner.is_ready() {
            return;
        }
        let renewed = self.inner.renewed.notified();
        tokio::pin!(renewed);
        renewed.as_mut().enable();
        self.inner.renew.store(true, Ordering::Release);
        self.inner.kick.notify_one();
        let _ = tokio::time::timeout(Duration::from_secs(5), renewed).await;
    }

    /// Ends the keeper, which closes both control sockets: the router drops the sessions.
    async fn shutdown(&self) {
        let h = self.inner.keeper.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = h {
            h.abort();
            let _ = h.await;
        }
        self.inner.set_sessions(None);
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
        self
    }

    fn as_any(&self) -> Option<&(dyn std::any::Any + Send + Sync)> {
        Some(self)
    }
}

pub struct I2pFactory;

#[async_trait::async_trait]
impl TransportFactory for I2pFactory {
    fn kind(&self) -> Kind {
        Kind::I2p
    }

    /// Returns at once; the keeper reports Ready once both sessions exist.
    async fn start(&self, ctx: StartCtx) -> Result<Arc<dyn Transport>, String> {
        let cfg = ctx.config.downcast_ref::<I2pConfig>().cloned().unwrap_or_default();
        let t = Arc::new(I2pTransport::new(&cfg, ctx.owner, Timing::default()));
        t.spawn_keeper();
        Ok(t)
    }

    fn adopt_on_unlock(&self) -> bool {
        true
    }

    fn compatible(&self, inst: &dyn Transport, cfg: &KindConfig) -> bool {
        let Some(t) = inst.as_any().and_then(|a| a.downcast_ref::<I2pTransport>()) else { return false };
        t.serves(&cfg.downcast_ref::<I2pConfig>().cloned().unwrap_or_default())
    }
}

/// The installed I2P instance, when it belongs to the account on screen.
pub fn active() -> Option<Arc<I2pTransport>> {
    let a = host::active().filter(|a| a.kind == Kind::I2p && a.owner() == crate::db::live_session_id())?;
    a.transport().clone().into_any().downcast::<I2pTransport>().ok()
}
