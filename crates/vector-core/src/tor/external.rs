//! A Tor already running on this device (Tor Browser, a tor daemon, Orbot) as the Tor kind: each
//! stream is a SOCKS5 CONNECT on its port, hostnames unresolved. Routing, twins and fail-closed are
//! the kind's own; only the engine differs.
//!
//! Circuits follow Vector's modes through Tor's `IsolateSOCKSAuth` (on by default): streams whose
//! SOCKS credentials differ never share a circuit, so each host, and each "New circuit", gets its
//! own username.

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::transport::host::{self, TransportEvent};
use crate::transport::status::{KindStatus, Phase, Reason};
use crate::transport::tor_config::TorConfig;
use crate::transport::{BoxedStream, ConnectError, Dest, Dialed, Kind, Lane, Route, RouteCtx, Transport};

/// Bumped by every circuit rotation: a new username is a new circuit.
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub(crate) fn rotate() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// The SOCKS username a stream to `host` carries.
pub(super) fn isolation_user(host: &str) -> String {
    let generation = GENERATION.load(Ordering::Acquire);
    if super::multi_circuit() {
        format!("vector-{generation}-{}", host.to_ascii_lowercase())
    } else {
        format!("vector-{generation}")
    }
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const WATCH_UP: Duration = Duration::from_secs(10);
const WATCH_DOWN: Duration = Duration::from_secs(3);

/// Whether a SOCKS5 server that takes username auth answers on `port`.
pub async fn probe(port: u16) -> bool {
    let check = async {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
        s.write_all(&[5, 1, 2]).await.ok()?;
        let mut reply = [0u8; 2];
        s.read_exact(&mut reply).await.ok()?;
        (reply == [5, 2]).then_some(())
    };
    matches!(tokio::time::timeout(PROBE_TIMEOUT, check).await, Ok(Some(())))
}

/// The first of `ports` with a Tor on it.
pub async fn detect(ports: &[u16]) -> Option<u16> {
    for &p in ports {
        if probe(p).await {
            return Some(p);
        }
    }
    None
}

struct Shared {
    ports: Vec<u16>,
    /// The port in use, 0 before one was found.
    port: AtomicU16,
    up: AtomicBool,
}

pub struct ExternalTor {
    shared: Arc<Shared>,
    watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ExternalTor {
    /// Looks once before returning, so a Tor that is already up is ready at install; one that
    /// isn't is watched for.
    pub async fn start(cfg: &TorConfig) -> Arc<Self> {
        let ports = cfg.ports();
        let found = detect(&ports).await;
        let shared = Arc::new(Shared {
            port: AtomicU16::new(found.unwrap_or(0)),
            up: AtomicBool::new(found.is_some()),
            ports,
        });
        match found {
            Some(p) => crate::log_info!("[Tor] using the Tor running on port {p}"),
            None => crate::log_info!("[Tor] no running Tor found yet; watching for one"),
        }
        let t = Arc::new(ExternalTor { shared: shared.clone(), watcher: Mutex::new(None) });
        // spawn-detached: watches one local port for the instance's life; aborted at shutdown.
        let h = crate::transport::spawn_on(watch(shared));
        *t.watcher.lock().unwrap_or_else(|e| e.into_inner()) = Some(h);
        t
    }

    /// Whether this instance serves `cfg`: same engine, same port choice.
    pub fn serves(&self, cfg: &TorConfig) -> bool {
        cfg.external && cfg.ports() == self.shared.ports
    }

    pub fn port(&self) -> Option<u16> {
        Some(self.shared.port.load(Ordering::Acquire)).filter(|p| *p != 0)
    }
}

/// The host's entry for this instance, when it is the installed one.
fn active_entry(shared: &Arc<Shared>) -> Option<Arc<host::ActiveTransport>> {
    let a = host::active()?;
    let t = a.transport().as_any()?.downcast_ref::<ExternalTor>()?;
    Arc::ptr_eq(&t.shared, shared).then_some(a)
}

/// Record whether Tor answers and tell the host on a flip.
fn set_up(shared: &Arc<Shared>, up: bool) {
    if shared.up.swap(up, Ordering::AcqRel) == up {
        return;
    }
    let Some(a) = active_entry(shared) else { return };
    let (instance, owner) = (a.id(), a.owner());
    if up {
        crate::log_info!("[Tor] the running Tor answers again");
        host::notify(TransportEvent::Ready { kind: Kind::Tor, instance, owner, recovered: true });
    } else {
        crate::log_warn!("[Tor] the running Tor stopped answering");
        host::notify(TransportEvent::Lost { kind: Kind::Tor, instance, owner });
    }
}

async fn watch(shared: Arc<Shared>) {
    loop {
        let up = shared.up.load(Ordering::Acquire);
        tokio::time::sleep(if up { WATCH_UP } else { WATCH_DOWN }).await;
        let current = shared.port.load(Ordering::Acquire);
        if current != 0 && probe(current).await {
            set_up(&shared, true);
            continue;
        }
        // Gone from its port: any of the chosen ports will do.
        match detect(&shared.ports).await {
            Some(p) => {
                shared.port.store(p, Ordering::Release);
                set_up(&shared, true);
            }
            None => set_up(&shared, false),
        }
    }
}

/// SOCKS5 CONNECT to `host:port` through the Tor on `socks`, isolated as `user`.
async fn socks_connect(socks: u16, host: &str, port: u16, user: &str) -> Result<TcpStream, ConnectError> {
    let mut s = TcpStream::connect(("127.0.0.1", socks)).await.map_err(|_| ConnectError::RouterDown { port: socks })?;
    let io = |_| ConnectError::Unreachable("Tor closed the connection.".into());
    s.write_all(&[5, 1, 2]).await.map_err(io)?;
    let mut reply = [0u8; 2];
    s.read_exact(&mut reply).await.map_err(io)?;
    if reply != [5, 2] {
        return Err(ConnectError::Unreachable("The program on this port isn't Tor.".into()));
    }
    let user = &user.as_bytes()[..user.len().min(255)];
    let mut auth = vec![1, user.len() as u8];
    auth.extend_from_slice(user);
    auth.extend_from_slice(&[1, b'x']);
    s.write_all(&auth).await.map_err(io)?;
    s.read_exact(&mut reply).await.map_err(io)?;
    if reply[1] != 0 {
        return Err(ConnectError::Unreachable("Tor refused Vector's connection.".into()));
    }
    let mut req = vec![5, 1, 0];
    match host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            req.push(1);
            req.extend_from_slice(&ip.octets());
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            req.push(4);
            req.extend_from_slice(&ip.octets());
        }
        Err(_) => {
            if host.is_empty() || host.len() > 255 {
                return Err(ConnectError::Refused(crate::transport::Refusal::BadName));
            }
            req.push(3);
            req.push(host.len() as u8);
            req.extend_from_slice(host.as_bytes());
        }
    }
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req).await.map_err(io)?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await.map_err(io)?;
    if head[1] != 0 {
        return Err(ConnectError::Unreachable(reply_text(head[1]).into()));
    }
    let rest = match head[3] {
        1 => 4 + 2,
        4 => 16 + 2,
        3 => {
            let mut len = [0u8; 1];
            s.read_exact(&mut len).await.map_err(io)?;
            len[0] as usize + 2
        }
        _ => return Err(ConnectError::Unreachable("Tor answered incorrectly.".into())),
    };
    let mut bound = vec![0u8; rest];
    s.read_exact(&mut bound).await.map_err(io)?;
    Ok(s)
}

/// Tor's SOCKS5 replies, its onion extensions (0xF0..) included.
fn reply_text(code: u8) -> &'static str {
    match code {
        2 => "Tor's exit policy refused this connection.",
        3 => "The Tor exit couldn't reach this server's network.",
        4 => "The Tor exit couldn't reach this server.",
        5 => "This server refused the connection from Tor.",
        6 => "The Tor exit timed out reaching this server.",
        0xF0 => "This onion service can't be found.",
        0xF1 => "This onion service's descriptor is invalid.",
        0xF2 | 0xF3 | 0xF7 => "Couldn't reach this onion service.",
        0xF4 | 0xF5 => "This onion service needs a key Vector doesn't have.",
        0xF6 => "That onion address isn't valid.",
        _ => "Tor couldn't reach this server.",
    }
}

#[async_trait::async_trait]
impl Transport for ExternalTor {
    fn kind(&self) -> Kind {
        Kind::Tor
    }

    fn route(&self, dest: &Dest, port: u16, ctx: &RouteCtx) -> Route {
        crate::transport::route::route_tor(dest, port, ctx)
    }

    async fn dial(&self, route: &Route, _lane: Lane) -> Result<(BoxedStream, Dialed), ConnectError> {
        let (host, port) = match route {
            Route::Native { host, port } | Route::Exit { host, port } => (host.as_str(), *port),
            Route::Twin { via, port, .. } => (via.as_str(), *port),
            Route::Refuse(r) => return Err(ConnectError::Refused(*r)),
        };
        let socks = self.port().ok_or(ConnectError::NotReady(Some(Kind::Tor)))?;
        match socks_connect(socks, host, port, &isolation_user(host)).await {
            Ok(s) => Ok((Box::new(s), Dialed::default())),
            Err(e) => {
                if matches!(e, ConnectError::RouterDown { .. }) {
                    set_up(&self.shared, false);
                }
                crate::log_debug!("[Tor] connect({}:{}) through port {} failed: {}", host, port, socks, e);
                Err(e)
            }
        }
    }

    fn ready(&self) -> bool {
        self.shared.up.load(Ordering::Acquire)
    }

    fn kind_status(&self) -> KindStatus {
        let port = self.port();
        let detail = serde_json::json!({
            "status": if self.ready() { "connected" } else { "waiting" },
            "bootstrap_progress": if self.ready() { 100 } else { 0 },
            "multi_circuit": super::multi_circuit(),
            "external_port": port,
        });
        if self.ready() {
            return KindStatus::ready(detail);
        }
        let text = match self.shared.ports.as_slice() {
            [p] => format!("Waiting for Tor on port {p}. Open Tor Browser or start tor."),
            _ => "Waiting for Tor. Open Tor Browser or start tor.".to_string(),
        };
        KindStatus {
            phase: Phase::Waiting,
            progress: None,
            reason: Some(Reason::new("tor_external_missing", text)),
            retry_in: None,
            steps: Vec::new(),
            detail,
        }
    }

    async fn new_identity(&self) {
        super::rotate_circuits();
    }

    async fn shutdown(&self) {
        let h = self.watcher.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = h {
            h.abort();
            let _ = h.await;
        }
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
        self
    }

    fn as_any(&self) -> Option<&(dyn std::any::Any + Send + Sync)> {
        Some(self)
    }
}

/// The installed running-Tor instance, when it belongs to the account on screen.
pub fn active() -> Option<Arc<ExternalTor>> {
    let a = host::active().filter(|a| a.kind == Kind::Tor && a.owner() == crate::db::live_session_id())?;
    a.transport().clone().into_any().downcast::<ExternalTor>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// A SOCKS5 server that wants username auth and records what it was asked for.
    async fn fake_tor(reply: u8) -> (u16, tokio::sync::mpsc::UnboundedReceiver<(String, String, u16)>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        // spawn-detached: a test's fake server, gone with the test runtime.
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else { return };
                let tx = tx.clone();
                // spawn-detached: one fake client connection.
                tokio::spawn(async move {
                    let mut hello = [0u8; 3];
                    if s.read_exact(&mut hello).await.is_err() {
                        return;
                    }
                    s.write_all(&[5, 2]).await.unwrap();
                    let mut head = [0u8; 2];
                    if s.read_exact(&mut head).await.is_err() {
                        return;
                    }
                    let mut user = vec![0u8; head[1] as usize];
                    s.read_exact(&mut user).await.unwrap();
                    let mut plen = [0u8; 1];
                    s.read_exact(&mut plen).await.unwrap();
                    let mut pass = vec![0u8; plen[0] as usize];
                    s.read_exact(&mut pass).await.unwrap();
                    s.write_all(&[1, 0]).await.unwrap();
                    let mut req = [0u8; 5];
                    s.read_exact(&mut req).await.unwrap();
                    let mut host = vec![0u8; req[4] as usize];
                    s.read_exact(&mut host).await.unwrap();
                    let mut port = [0u8; 2];
                    s.read_exact(&mut port).await.unwrap();
                    let _ = tx.send((String::from_utf8(user).unwrap(), String::from_utf8(host).unwrap(), u16::from_be_bytes(port)));
                    s.write_all(&[5, reply, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
                });
            }
        });
        (port, rx)
    }

    #[tokio::test]
    async fn a_stream_is_a_socks_connect_with_the_hostname_unresolved() {
        let (port, mut rx) = fake_tor(0).await;
        assert!(probe(port).await);
        assert_eq!(detect(&[1, port]).await, Some(port), "the first port that answers");
        socks_connect(port, "relay.example.com", 443, "vector-0-relay.example.com").await.expect("connected");
        let (user, host, p) = rx.recv().await.unwrap();
        assert_eq!((host.as_str(), p), ("relay.example.com", 443));
        assert_eq!(user, "vector-0-relay.example.com");
    }

    #[tokio::test]
    async fn tor_refusals_and_a_missing_tor_say_what_happened() {
        let (port, _rx) = fake_tor(0xF0).await;
        let e = socks_connect(port, "abc.onion", 80, "u").await.unwrap_err();
        assert_eq!(e.to_string(), "This onion service can't be found.");
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
        assert!(!probe(closed).await);
        assert!(matches!(socks_connect(closed, "a.example", 443, "u").await, Err(ConnectError::RouterDown { .. })));
    }

    /// Needs a Tor on 9150 or 9050: `cargo test -p vector-core --features tor -- --ignored running_tor`.
    #[tokio::test]
    #[ignore]
    async fn running_tor_reaches_the_tor_network() {
        let port = detect(&crate::transport::tor_config::PROBE_PORTS).await.expect("a running Tor");
        let mut s = socks_connect(port, "check.torproject.org", 80, &isolation_user("check.torproject.org")).await.unwrap();
        s.write_all(b"GET / HTTP/1.0\r\nHost: check.torproject.org\r\n\r\n").await.unwrap();
        let mut body = String::new();
        s.read_to_string(&mut body).await.unwrap();
        assert!(body.starts_with("HTTP/1."), "a clearnet server, through an exit: {}", &body[..body.len().min(80)]);
        let mut s = socks_connect(port, "mag2izmzc7nlzx3iixzi7kdppr7hm2p7kcwnqg6j7kluhp2htcrfphqd.onion", 80, "vector-live").await.unwrap();
        s.write_all(b"GET /.well-known/blossom HTTP/1.0\r\nHost: mag2izmzc7nlzx3iixzi7kdppr7hm2p7kcwnqg6j7kluhp2htcrfphqd.onion\r\n\r\n").await.unwrap();
        let mut body = String::new();
        s.read_to_string(&mut body).await.unwrap();
        assert!(body.starts_with("HTTP/1.1 200"), "an onion, natively: {}", &body[..body.len().min(80)]);
    }
}
