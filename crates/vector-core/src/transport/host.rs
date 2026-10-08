//! The process-wide slot for the one active transport instance, its events, and the dial the
//! bridge runs for every proxied connection.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use super::route::{Dest, Route};
use super::{BoxedStream, ConnectError, Dialed, Kind, RouteCtx, Ticket, Transport};

const FAILURE_CAP: usize = 128;

struct Failure {
    at: web_time::Instant,
    error: ConnectError,
}

pub struct ActiveTransport {
    pub kind: Kind,
    id: u64,
    /// Fixed for the instance's life: another owner means a new instance.
    owner: u64,
    started_prelogin: bool,
    transport: Arc<dyn Transport>,
    failures: Mutex<HashMap<String, Failure>>,
    exits: Mutex<HashMap<String, String>>,
    identity: Mutex<Option<nostr_sdk::prelude::PublicKey>>,
}

impl ActiveTransport {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn owner(&self) -> u64 {
        self.owner
    }

    pub fn started_prelogin(&self) -> bool {
        self.started_prelogin
    }

    pub fn transport(&self) -> &Arc<dyn Transport> {
        &self.transport
    }

    pub fn last_failure(&self, host: &str) -> Option<(web_time::Instant, ConnectError)> {
        self.failures.lock().unwrap_or_else(|e| e.into_inner()).get(host).map(|f| (f.at, f.error.clone()))
    }

    /// The outproxy id the last exit to `host` used.
    pub fn last_exit(&self, host: &str) -> Option<String> {
        self.exits.lock().unwrap_or_else(|e| e.into_inner()).get(host).cloned()
    }

    pub(crate) fn record_failure(&self, host: &str, e: &ConnectError) {
        self.record(host, Err(e));
    }

    fn record(&self, host: &str, outcome: Result<&Dialed, &ConnectError>) {
        let mut failures = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        match outcome {
            Ok(dialed) => {
                failures.remove(host);
                if let Some(id) = &dialed.outproxy {
                    let mut exits = self.exits.lock().unwrap_or_else(|e| e.into_inner());
                    if exits.len() >= 256 && !exits.contains_key(host) {
                        exits.clear();
                    }
                    exits.insert(host.to_string(), id.clone());
                }
            }
            Err(ConnectError::Stale | ConnectError::Cancelled) => {}
            Err(e) => {
                if failures.len() >= FAILURE_CAP && !failures.contains_key(host) {
                    if let Some(oldest) = failures.iter().min_by_key(|(_, f)| f.at).map(|(h, _)| h.clone()) {
                        failures.remove(&oldest);
                    }
                }
                failures.insert(host.to_string(), Failure { at: web_time::Instant::now(), error: e.clone() });
            }
        }
    }

    /// Whether the live session's egress can run through this instance. One that can't (another
    /// network chosen, another account's) moves no epoch coming or going: no socket rode it.
    pub(crate) fn serves_live(&self) -> bool {
        super::preference() == Some(self.kind) && self.owner == crate::db::live_session_id()
    }

    /// Record the identity using a pre-login instance. True when a different one used it before.
    pub(crate) fn note_identity(&self, pk: nostr_sdk::prelude::PublicKey) -> bool {
        let mut slot = self.identity.lock().unwrap_or_else(|e| e.into_inner());
        let rotate = matches!(*slot, Some(prev) if prev != pk);
        *slot = Some(pk);
        rotate
    }
}

static SLOT: RwLock<Option<Arc<ActiveTransport>>> = RwLock::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn active() -> Option<Arc<ActiveTransport>> {
    SLOT.read().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn active_as<T: Send + Sync + 'static>() -> Option<Arc<T>> {
    active()?.transport().clone().into_any().downcast::<T>().ok()
}

/// Install `t`. Err if a slot is occupied: callers uninstall first.
pub fn activate(t: Arc<dyn Transport>, owner: u64, started_prelogin: bool) -> Result<Arc<ActiveTransport>, String> {
    let a = Arc::new(ActiveTransport {
        kind: t.kind(),
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        owner,
        started_prelogin,
        transport: t,
        failures: Mutex::new(HashMap::new()),
        exits: Mutex::new(HashMap::new()),
        identity: Mutex::new(None),
    });
    {
        let mut slot = SLOT.write().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return Err("A network is already running.".into());
        }
        *slot = Some(a.clone());
    }
    if !a.serves_live() {
        notify(TransportEvent::Changed);
    } else if a.transport().ready() {
        notify(TransportEvent::Ready { kind: a.kind, instance: a.id, owner, recovered: false });
        #[cfg(all(feature = "twin-check", not(target_arch = "wasm32")))]
        super::twins::check_pending();
    } else {
        super::bump_epoch();
        notify(TransportEvent::Changed);
    }
    Ok(a)
}

/// Empty the slot and cut every connection the instance carried. Returns it for shutdown.
pub fn uninstall() -> Option<Arc<ActiveTransport>> {
    let a = SLOT.write().unwrap_or_else(|e| e.into_inner()).take()?;
    if a.serves_live() {
        super::bump_epoch();
    }
    #[cfg(not(target_arch = "wasm32"))]
    super::bridge::abort_where(|c| c.instance == a.id);
    notify(TransportEvent::Changed);
    Some(a)
}

/// `uninstall`, then shut the instance down; with `join`, wait until the bridge holds none of it.
pub async fn deactivate(join: bool) {
    let Some(a) = uninstall() else { return };
    a.transport().shutdown().await;
    #[cfg(not(target_arch = "wasm32"))]
    if join {
        super::bridge::drain(a.id).await;
    }
    #[cfg(target_arch = "wasm32")]
    let _ = join;
}

/// The dial behind every proxied connection: a live ticket, the active instance its owner runs,
/// the shared pre-router, the kind's rules on the live settings, then the kind's own dial.
pub async fn dial(t: Ticket, dest: Dest, port: u16) -> Result<(BoxedStream, Route, Dialed), ConnectError> {
    let a = select(&t)?;
    let host = dest.host();
    let route = route_now(&a, &dest, port);
    if let Route::Refuse(r) = route {
        let e = ConnectError::Refused(r);
        a.record_failure(&host, &e);
        return Err(e);
    }
    let (stream, dialed) = dial_route(&a, t.lane, &host, &route).await?;
    Ok((stream, route, dialed))
}

/// The instance that may serve `t`: live, owned by the ticket's session, of the chosen kind and
/// ready.
pub(crate) fn select(t: &Ticket) -> Result<Arc<ActiveTransport>, ConnectError> {
    if !t.is_live() {
        return Err(ConnectError::Stale);
    }
    let pref = super::preference();
    let Some(a) = active().filter(|a| a.owner == t.owner && Some(a.kind) == pref) else {
        return Err(match pref {
            Some(k) if k != Kind::Clearnet => ConnectError::NotReady(Some(k)),
            _ => ConnectError::Stale,
        });
    };
    if !a.transport().ready() {
        return Err(ConnectError::NotReady(Some(a.kind)));
    }
    Ok(a)
}

/// Run the kind's dial for an already-routed connection and record the outcome for `host`.
pub(crate) async fn dial_route(a: &ActiveTransport, lane: super::Lane, host: &str, route: &Route) -> Result<(BoxedStream, Dialed), ConnectError> {
    match a.transport().dial(route, lane).await {
        Ok((stream, dialed)) => {
            a.record(host, Ok(&dialed));
            Ok((stream, dialed))
        }
        Err(e) => {
            a.record(host, Err(&e));
            Err(e)
        }
    }
}

/// Whether the live settings still allow the exit an I2P connection used.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn outproxy_allowed(kind: Kind, id: &str) -> bool {
    let cfg = super::prefs::live().config(kind);
    match cfg.downcast_ref::<super::i2p_config::I2pConfig>() {
        Some(c) => c.exit == super::ExitPolicy::Allow && c.outproxy_list().iter().any(|o| o.id == id && o.enabled),
        None => true,
    }
}

/// The route the live settings give `dest` on this instance right now.
pub(crate) fn route_now(a: &ActiveTransport, dest: &Dest, port: u16) -> Route {
    if let Some(r) = super::route::pre_route(a.kind, dest) {
        return Route::Refuse(r);
    }
    let p = super::prefs::live();
    let (aliases, config) = (p.aliases(), p.config(a.kind));
    a.transport().route(dest, port, &RouteCtx { aliases: &aliases, config: &config })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportEvent {
    Changed,
    Ready { kind: Kind, instance: u64, owner: u64, recovered: bool },
    Lost { kind: Kind, instance: u64, owner: u64 },
}

fn events() -> &'static tokio::sync::broadcast::Sender<TransportEvent> {
    static TX: OnceLock<tokio::sync::broadcast::Sender<TransportEvent>> = OnceLock::new();
    TX.get_or_init(|| tokio::sync::broadcast::channel(64).0)
}

pub fn subscribe() -> tokio::sync::broadcast::Receiver<TransportEvent> {
    events().subscribe()
}

/// Every kind calls this on each `ready()` flip. Ready and Lost move the epoch while the live
/// egress can run through the instance; an event from an instance that is not the active one, or
/// whose owner is not live, is dropped.
pub fn notify(ev: TransportEvent) {
    match &ev {
        TransportEvent::Ready { instance, owner, .. } | TransportEvent::Lost { instance, owner, .. } => {
            let Some(a) = active().filter(|a| a.id == *instance) else { return };
            if *owner != a.owner || *owner != crate::db::live_session_id() {
                return;
            }
            debug_assert_eq!(
                a.transport().ready(),
                matches!(ev, TransportEvent::Ready { .. }),
                "a kind must report the readiness its event names"
            );
            if a.serves_live() {
                super::bump_epoch();
            }
        }
        TransportEvent::Changed => {}
    }
    let _ = events().send(ev);
    emit_state();
}

const EMIT_GAP: std::time::Duration = std::time::Duration::from_millis(250);

static EMIT: Mutex<(Option<web_time::Instant>, bool)> = Mutex::new((None, false));

/// At most four `transport_state` events a second; the trailing one always lands, carrying the
/// state at the moment it is sent.
fn emit_state() {
    let wait = {
        let mut g = EMIT.lock().unwrap_or_else(|e| e.into_inner());
        let now = web_time::Instant::now();
        match g.0 {
            Some(last) if now.duration_since(last) < EMIT_GAP => {
                if g.1 || !can_trail() {
                    return;
                }
                g.1 = true;
                Some(EMIT_GAP - now.duration_since(last))
            }
            _ => {
                g.0 = Some(now);
                None
            }
        }
    };
    match wait {
        None => send_view(),
        Some(delay) => {
            // Cancelled with a short-lived caller runtime, even before its first poll: the next
            // event must still emit.
            let pending = TrailingPending;
            let trailing = async move {
                let pending = pending;
                crate::rt::time::sleep(delay).await;
                EMIT.lock().unwrap_or_else(|e| e.into_inner()).0 = Some(web_time::Instant::now());
                drop(pending);
                send_view();
            };
            // The process-lifetime runtime when one exists, so the trailing event outlives the
            // caller's (Android background sync); a Clearnet-only process never builds it for this.
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(rt) = super::runtime::built() {
                // spawn-detached: emits the live view once the gap passes; reads no account state.
                rt.spawn(trailing);
                return;
            }
            // spawn-detached: emits the live view once the gap passes; reads no account state.
            crate::rt::spawn(trailing);
        }
    }
}

fn can_trail() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    if super::runtime::built().is_some() {
        return true;
    }
    crate::rt::can_spawn()
}

/// The trailing emit is armed while this lives.
struct TrailingPending;

impl Drop for TrailingPending {
    fn drop(&mut self) {
        EMIT.lock().unwrap_or_else(|e| e.into_inner()).1 = false;
    }
}

fn send_view() {
    crate::traits::emit_event("transport_state", &super::status::view());
}
