//! Clearnet hosts over I2P: an I2P stream to an outproxy, then an HTTP CONNECT through it. The
//! list is the user's, tried in order; health, cooling and refused ports are remembered per
//! instance only.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use web_time::Instant;

use crate::transport::i2p_config::Outproxy;
use crate::transport::ConnectError;

use super::sam::StreamFail;
use super::Inner;

const HEAD_CAP: usize = 8 * 1024;
pub(crate) const HEAD_DEADLINE: Duration = Duration::from_secs(30);
/// An outproxy that refused a port is not asked for it again for this long.
const PORT_MEMORY: Duration = Duration::from_secs(6 * 3600);
const COOL_BASE: Duration = Duration::from_secs(15);
const COOL_CAP: Duration = Duration::from_secs(600);
/// A router that just made its sessions hasn't looked the outproxies up yet: an unknown LeaseSet
/// then is the router's, not the outproxy's.
pub(crate) const FRESH_SESSION: Duration = Duration::from_secs(60);
/// Outproxies tried for one connect.
pub const MAX_ATTEMPTS: usize = 2;

#[derive(Clone, Debug, Default)]
struct Entry {
    failures: u32,
    cooling_until: Option<Instant>,
    last_ok: Option<u64>,
    ok_at: Option<Instant>,
    fault_at: Option<Instant>,
    last_error: Option<String>,
    refused_ports: HashMap<u16, Instant>,
}

impl Entry {
    fn cooling(&self, now: Instant) -> bool {
        self.cooling_until.is_some_and(|t| t > now)
    }

    fn refuses(&self, port: u16) -> bool {
        self.refused_ports.get(&port).is_some_and(|at| at.elapsed() < PORT_MEMORY)
    }
}

#[derive(Default)]
pub struct Health {
    map: Mutex<HashMap<String, Entry>>,
    last_ok: Mutex<Option<String>>,
}

fn now_secs() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn cool_for(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(16);
    COOL_BASE.saturating_mul(1u32 << shift).min(COOL_CAP)
}

impl Health {
    fn with<R>(&self, id: &str, f: impl FnOnce(&mut Entry) -> R) -> R {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        f(map.entry(id.to_string()).or_default())
    }

    fn entry(&self, id: &str) -> Entry {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned().unwrap_or_default()
    }

    /// Apply `f`; when what Settings shows for `id` (its state, or which outproxy carried the
    /// last tunnel) moved, say so: nothing else would refresh it once I2P is up.
    fn update(&self, id: &str, f: impl FnOnce(&mut Entry)) {
        let before = (self.state_of(id), self.last_ok());
        self.with(id, f);
        if (self.state_of(id), self.last_ok()) != before {
            crate::transport::host::notify(crate::transport::host::TransportEvent::Changed);
        }
    }

    pub fn ok(&self, id: &str) {
        self.update(id, |e| {
            e.failures = 0;
            e.cooling_until = None;
            e.last_ok = Some(now_secs());
            e.ok_at = Some(Instant::now());
            e.last_error = None;
            *self.last_ok.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.to_string());
        });
    }

    /// A dial that began at `started` failed through `id`. Dials that began together and failed
    /// together are one failure, and one a later success overtook is none: neither says more.
    pub fn fault(&self, id: &str, why: &str, started: Instant) {
        self.update(id, |e| {
            if e.ok_at.is_some_and(|t| t >= started) {
                return;
            }
            e.last_error = Some(why.to_string());
            if e.fault_at.is_some_and(|t| t >= started) {
                return;
            }
            let now = Instant::now();
            e.failures = e.failures.saturating_add(1);
            e.cooling_until = Some(now + cool_for(e.failures));
            e.fault_at = Some(now);
        });
    }

    /// A failure that isn't the outproxy's to answer for: shown, never held against it.
    pub fn note(&self, id: &str, why: &str) {
        self.update(id, |e| e.last_error = Some(why.to_string()));
    }

    pub fn refused_port(&self, id: &str, port: u16, why: &str) {
        self.update(id, |e| {
            e.refused_ports.insert(port, Instant::now());
            e.last_error = Some(why.to_string());
        });
    }

    /// The outproxy id that last opened a tunnel on this instance.
    pub fn last_ok(&self) -> Option<String> {
        self.last_ok.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Which outproxies to try for `port`, in order: enabled ones that haven't refused the
    /// port, skipping those cooling down; if all are cooling, the one that cools first.
    pub fn plan(&self, list: &[Outproxy], port: u16) -> Result<Vec<Outproxy>, ConnectError> {
        let enabled: Vec<&Outproxy> = list.iter().filter(|o| o.enabled).collect();
        if enabled.is_empty() {
            return Err(ConnectError::NoExit);
        }
        let now = Instant::now();
        let open: Vec<(&Outproxy, Entry)> =
            enabled.into_iter().map(|o| (o, self.entry(&o.id))).filter(|(_, e)| !e.refuses(port)).collect();
        if open.is_empty() {
            return Err(ConnectError::ExitRefusedPort(port));
        }
        let warm: Vec<Outproxy> = open.iter().filter(|(_, e)| !e.cooling(now)).map(|(o, _)| (*o).clone()).collect();
        if !warm.is_empty() {
            return Ok(warm);
        }
        let soonest = open.iter().min_by_key(|(_, e)| e.cooling_until).map(|(o, _)| (*o).clone());
        Ok(soonest.into_iter().collect())
    }

    /// `ok` (worked last), `unknown` (not tried), `cooling`, or `failing`.
    pub fn state_of(&self, id: &str) -> &'static str {
        let e = self.entry(id);
        if e.cooling(Instant::now()) {
            "cooling"
        } else if e.failures > 0 {
            "failing"
        } else if e.last_ok.is_some() {
            "ok"
        } else {
            "unknown"
        }
    }

    /// `detail.outproxies` of the status view.
    pub fn view(&self, list: &[Outproxy]) -> serde_json::Value {
        let now = Instant::now();
        let mut out = serde_json::Map::new();
        for o in list {
            let e = self.entry(&o.id);
            let mut ports: Vec<u16> = e.refused_ports.iter().filter(|(_, at)| at.elapsed() < PORT_MEMORY).map(|(p, _)| *p).collect();
            ports.sort_unstable();
            let cooling_for = e.cooling_until.filter(|t| *t > now).map(|t| t.duration_since(now).as_secs().max(1));
            out.insert(
                o.id.clone(),
                serde_json::json!({
                    "state": self.state_of(&o.id),
                    "last_ok": e.last_ok,
                    "last_error": e.last_error,
                    "cooling_for": cooling_for,
                    "refused_ports": ports,
                }),
            );
        }
        serde_json::Value::Object(out)
    }

    /// Whether every enabled outproxy is cooling or failing right now.
    pub fn all_failing(&self, list: &[Outproxy]) -> bool {
        let enabled: Vec<&Outproxy> = list.iter().filter(|o| o.enabled).collect();
        !enabled.is_empty() && enabled.iter().all(|o| matches!(self.state_of(&o.id), "cooling" | "failing"))
    }
}

/// How an outproxy answered a CONNECT.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Head {
    Open,
    /// 400, 403, 405: it won't carry this port or request.
    RefusedPort(u16),
    /// 429, 503: overloaded or rate limited.
    Overloaded(u16),
    /// 502, 504: the target is unreachable from the exit.
    TargetUnreachable(u16),
    /// Not HTTP, too long, too slow, or a status that makes no sense here.
    Fault(String),
}

/// Send the CONNECT and read the response head a byte at a time, so the first tunnel byte stays
/// in the socket.
pub async fn handshake(s: &mut TcpStream, host: &str, port: u16, deadline: Duration) -> Head {
    let req = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\nUser-Agent: MYOB/6.66 (AN/ON)\r\n\r\n");
    if let Err(e) = s.write_all(req.as_bytes()).await {
        return Head::Fault(format!("write: {e}"));
    }
    match tokio::time::timeout(deadline, read_head(s)).await {
        Err(_) => Head::Fault("no answer".into()),
        Ok(Err(e)) => Head::Fault(e),
        Ok(Ok(head)) => classify(&head),
    }
}

async fn read_head(s: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut head = Vec::with_capacity(128);
    let mut b = [0u8; 1];
    loop {
        match s.read(&mut b).await {
            Ok(0) => return Err("closed before answering".into()),
            Ok(_) => {
                head.push(b[0]);
                if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
                    return Ok(head);
                }
                if head.len() >= HEAD_CAP {
                    return Err("an oversized answer".into());
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn classify(head: &[u8]) -> Head {
    let text = String::from_utf8_lossy(head);
    let status = text.lines().next().unwrap_or("");
    let mut parts = status.split_whitespace();
    let (Some(proto), Some(code)) = (parts.next(), parts.next()) else {
        return Head::Fault("not HTTP".into());
    };
    if !proto.starts_with("HTTP/1.") {
        return Head::Fault("not HTTP".into());
    }
    let Ok(code) = code.parse::<u16>() else {
        return Head::Fault("not HTTP".into());
    };
    match code {
        200 => Head::Open,
        400 | 403 | 405 => Head::RefusedPort(code),
        429 | 503 => Head::Overloaded(code),
        502 | 504 => Head::TargetUnreachable(code),
        other => Head::Fault(format!("answered {other}")),
    }
}

/// Reach `host:port` through the list, in order, at most [`MAX_ATTEMPTS`] outproxies. Returns
/// the open tunnel and the id of the outproxy that carried it. `allowed` is asked again right
/// before each outproxy is dialed and again before it gets the CONNECT.
pub(crate) async fn connect(
    inner: &Inner,
    nick: &str,
    list: &[Outproxy],
    host: &str,
    port: u16,
    allowed: &(dyn Fn(&str) -> bool + Send + Sync),
) -> Result<(TcpStream, String), ConnectError> {
    let started = Instant::now();
    let plan = inner.outproxies.plan(list, port)?;
    let mut tried = 0;
    let mut all_refused = true;
    for o in plan.iter().take(MAX_ATTEMPTS) {
        // Removed or turned off since the plan was made: its operator never sees the CONNECT.
        if !allowed(&o.id) {
            continue;
        }
        tried += 1;
        let mut stream = match inner.open_raw(nick, &o.address, o.port).await {
            Ok(s) => s,
            // Our own router or session failed: not the outproxy's fault, and no other one
            // would fare better.
            Err(StreamFail::RouterDown) => return Err(inner.fail(StreamFail::RouterDown)),
            Err(StreamFail::SessionGone) => return Err(inner.fail(StreamFail::SessionGone)),
            Err(e) => {
                if matches!(e, StreamFail::SessionSuspect(_)) {
                    if let Some(ours) = ours_failed(inner, nick).await {
                        return Err(inner.fail(ours));
                    }
                    inner.kick.notify_one();
                }
                if lookup_pending(inner, &e) {
                    inner.outproxies.note(&o.id, &stream_text(&e));
                } else {
                    inner.outproxies.fault(&o.id, &stream_text(&e), started);
                }
                all_refused = false;
                continue;
            }
        };
        if !allowed(&o.id) {
            continue;
        }
        match handshake(&mut stream, host, port, inner.timing.head_deadline).await {
            Head::Open => {
                inner.outproxies.ok(&o.id);
                return Ok((stream, o.id.clone()));
            }
            Head::RefusedPort(code) => inner.outproxies.refused_port(&o.id, port, &format!("refused port {port} ({code})")),
            Head::Overloaded(code) => {
                inner.outproxies.fault(&o.id, &format!("busy ({code})"), started);
                all_refused = false;
            }
            Head::TargetUnreachable(code) => {
                crate::log_debug!("[I2P] {} could not reach {host} ({code})", o.name);
                return Err(ConnectError::ExitUnreachable(o.name.clone()));
            }
            Head::Fault(why) => {
                if let Some(ours) = ours_failed(inner, nick).await {
                    return Err(inner.fail(ours));
                }
                inner.outproxies.fault(&o.id, &why, started);
                all_refused = false;
            }
        }
    }
    if tried > 0 && all_refused {
        Err(ConnectError::ExitRefusedPort(port))
    } else {
        Err(ConnectError::NoExit)
    }
}

/// The router hasn't found the outproxy's LeaseSet yet, on a session it only just made.
fn lookup_pending(inner: &Inner, e: &StreamFail) -> bool {
    matches!(e, StreamFail::Unreachable(m) if m.to_ascii_lowercase().contains("leaseset"))
        && inner.session_age().is_some_and(|age| age < inner.timing.fresh_session)
}

/// A stream that dies without a word may have died with our own router or session: then no
/// outproxy is to blame, and no other one would fare better.
async fn ours_failed(inner: &Inner, nick: &str) -> Option<StreamFail> {
    if !inner.is_ready() {
        return Some(StreamFail::SessionGone);
    }
    match super::sam::liveness(inner.port, inner.auth.as_ref(), nick).await {
        super::sam::Liveness::RouterDown => Some(StreamFail::RouterDown),
        super::sam::Liveness::Gone => Some(StreamFail::SessionGone),
        super::sam::Liveness::Alive | super::sam::Liveness::Inconclusive(_) => None,
    }
}

fn stream_text(e: &StreamFail) -> String {
    match e {
        StreamFail::Unreachable(m) => format!("unreachable: {m}"),
        StreamFail::Timeout => "timed out".into(),
        StreamFail::BadKey => "bad address".into(),
        StreamFail::SessionSuspect(m) => format!("router: {m}"),
        StreamFail::RouterDown => "router down".into(),
        StreamFail::SessionGone => "session gone".into(),
    }
}
