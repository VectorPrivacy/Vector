//! The process bridge: a loopback SOCKS5 server every proxied connection passes through, bound
//! once and never closed, so no client ever holds a port someone else can later take.
//!
//! The main port takes RFC 1929 auth only: the password is a per-process secret, the username a
//! ticket naming the session, epoch and lane. The restricted port takes no auth and serves only
//! single-use permits, for stock clients that can carry nothing but a `SocketAddr`.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use zeroize::Zeroizing;

use super::route::{Dest, Route};
use super::{host, ConnectError, Ticket};

/// Last-resort refusal target: nothing can listen on port 0, so a connect there fails locally.
pub const NOWHERE: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));

/// Greeting through request; our own loopback clients send all of it at once.
static HANDSHAKE_DEADLINE_MS: AtomicU64 = AtomicU64::new(10_000);
const MAX_DIALS: usize = 256;
const PERMIT_TTL: Duration = Duration::from_secs(30);
/// How long a client riding a twin has to send its first byte.
const TWIN_FIRST_BYTE: Duration = Duration::from_secs(30);

const VER: u8 = 0x05;
const NO_AUTH: u8 = 0x00;
const USER_PASS: u8 = 0x02;
const NO_METHOD: u8 = 0xFF;
const CONNECT: u8 = 0x01;
const ATYP_V4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_V6: u8 = 0x04;
const REP_OK: u8 = 0x00;
const REP_NOT_ALLOWED: u8 = 0x02;
const REP_BAD_CMD: u8 = 0x07;
const REP_BAD_ATYP: u8 = 0x08;

struct Bridge {
    main: SocketAddr,
    restricted: SocketAddr,
    secret: Zeroizing<String>,
}

static BRIDGE: OnceLock<Result<Bridge, String>> = OnceLock::new();

fn bridge() -> Result<&'static Bridge, ConnectError> {
    BRIDGE.get_or_init(bind).as_ref().map_err(|e| ConnectError::Bridge(e.clone()))
}

fn random_secret() -> Zeroizing<String> {
    use rand::RngCore;
    let mut b = Zeroizing::new([0u8; 16]);
    rand::rngs::OsRng.fill_bytes(&mut *b);
    Zeroizing::new(crate::simd::hex::bytes_to_hex_string(&*b))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Port {
    Main,
    Restricted,
}

fn bind() -> Result<Bridge, String> {
    let listen = || -> Result<std::net::TcpListener, String> {
        let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("bridge bind: {e}"))?;
        l.set_nonblocking(true).map_err(|e| format!("bridge bind: {e}"))?;
        Ok(l)
    };
    let (main, restricted) = (listen()?, listen()?);
    let main_addr = main.local_addr().map_err(|e| e.to_string())?;
    let restricted_addr = restricted.local_addr().map_err(|e| e.to_string())?;
    // spawn-detached: the process bridge serves whichever session a ticket names.
    super::spawn_on(accept_loop(main, Port::Main));
    // spawn-detached: the restricted port serves permits, each naming its session.
    super::spawn_on(accept_loop(restricted, Port::Restricted));
    crate::log_info!("[Transport] bridge on {main_addr} (restricted {restricted_addr})");
    Ok(Bridge { main: main_addr, restricted: restricted_addr, secret: random_secret() })
}

/// The main port. `Bridge(e)` if it could not bind.
pub fn addr() -> Result<SocketAddr, ConnectError> {
    bridge().map(|b| b.main)
}

pub fn restricted_addr() -> Result<SocketAddr, ConnectError> {
    bridge().map(|b| b.restricted)
}

/// `(username, password)` for a ticket. Never logged, never in a view, an error or IPC.
pub fn credentials(t: &Ticket) -> (String, Zeroizing<String>) {
    let secret = bridge().map(|b| b.secret.clone()).unwrap_or_default();
    (t.encode(), secret)
}

/// `socks5h://{user}:{secret}@{addr}`. Never logged, never in a view, an error or IPC.
pub fn proxy_url(t: &Ticket) -> Result<String, ConnectError> {
    let b = bridge()?;
    Ok(format!("socks5h://{}:{}@{}", t.encode(), b.secret.as_str(), b.main))
}

/// For display only: the main port without credentials.
pub fn display_url() -> Option<String> {
    addr().ok().map(|a| format!("socks5h://{a}"))
}

// ── Permits (restricted port) ─────────────────────────────────────────────────

struct Permit {
    ticket: Ticket,
    host: String,
    port: u16,
    expires: web_time::Instant,
}

static PERMITS: Mutex<Vec<Permit>> = Mutex::new(Vec::new());

/// One connection to exactly `host:port` on the restricted port, within 30 s.
pub fn permit(t: Ticket, host: &str, port: u16) {
    let Some(dest) = Dest::parse(host) else { return };
    let mut v = PERMITS.lock().unwrap_or_else(|e| e.into_inner());
    let now = web_time::Instant::now();
    v.retain(|p| p.expires > now);
    if v.len() >= 256 {
        v.remove(0);
    }
    v.push(Permit { ticket: t, host: dest.host(), port, expires: now + PERMIT_TTL });
}

#[cfg(test)]
pub(crate) fn set_handshake_deadline_for_test(d: Duration) {
    HANDSHAKE_DEADLINE_MS.store(d.as_millis() as u64, Ordering::Relaxed);
}

#[cfg(test)]
pub(crate) fn age_permits_for_test(by: Duration) {
    for p in PERMITS.lock().unwrap_or_else(|e| e.into_inner()).iter_mut() {
        p.expires = p.expires.checked_sub(by).unwrap_or(p.expires);
    }
}

fn take_permit(dest: &Dest, port: u16) -> Option<Ticket> {
    let mut v = PERMITS.lock().unwrap_or_else(|e| e.into_inner());
    let now = web_time::Instant::now();
    v.retain(|p| p.expires > now);
    let host = dest.host();
    let i = v.iter().position(|p| p.host == host && p.port == port)?;
    Some(v.remove(i).ticket)
}

// ── Connection registry ──────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct ConnInfo {
    pub instance: u64,
    /// `None` until the handshake proved one: an epoch bump must not cut a connection that may
    /// be about to present a fresh ticket.
    pub ticket: Option<Ticket>,
    pub route: Option<Route>,
    pub outproxy: Option<String>,
    pub gen: u64,
    pub spliced: bool,
}

struct Entry {
    info: ConnInfo,
    abort: Option<tokio::task::AbortHandle>,
}

static REGISTRY: Mutex<Option<HashMap<u64, Entry>>> = Mutex::new(None);
static CONN_ID: AtomicU64 = AtomicU64::new(1);

fn with_registry<R>(f: impl FnOnce(&mut HashMap<u64, Entry>) -> R) -> R {
    let mut g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

fn update(id: u64, f: impl FnOnce(&mut ConnInfo)) {
    with_registry(|r| {
        if let Some(e) = r.get_mut(&id) {
            f(&mut e.info);
        }
    });
}

/// Abort every connection matching `pred`, dialing and spliced alike.
pub fn abort_where(pred: impl Fn(&ConnInfo) -> bool) -> usize {
    with_registry(|r| {
        let mut n = 0;
        for e in r.values().filter(|e| pred(&e.info)) {
            if let Some(h) = &e.abort {
                h.abort();
                n += 1;
            }
        }
        n
    })
}

/// Returns once the bridge holds no connection of `instance`.
pub async fn drain(instance: u64) {
    let deadline = web_time::Instant::now() + Duration::from_secs(10);
    loop {
        if !with_registry(|r| r.values().any(|e| e.info.instance == instance)) {
            return;
        }
        if web_time::Instant::now() >= deadline {
            crate::log_warn!("[Transport] bridge drain timed out for instance {instance}");
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// How many connections the registry holds (tests and diagnostics).
pub fn open_connections() -> usize {
    with_registry(|r| r.len())
}

/// How many connections match `pred`, dialing and spliced alike.
pub fn count_where(pred: impl Fn(&ConnInfo) -> bool) -> usize {
    with_registry(|r| r.values().filter(|e| pred(&e.info)).count())
}

struct Registered(u64);

impl Drop for Registered {
    fn drop(&mut self) {
        with_registry(|r| r.remove(&self.0));
    }
}

fn dials() -> &'static std::sync::Arc<tokio::sync::Semaphore> {
    static SEM: OnceLock<std::sync::Arc<tokio::sync::Semaphore>> = OnceLock::new();
    SEM.get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_DIALS)))
}

async fn accept_loop(std_listener: std::net::TcpListener, port: Port) {
    let listener = match tokio::net::TcpListener::from_std(std_listener) {
        Ok(l) => l,
        Err(e) => {
            crate::log_warn!("[Transport] bridge listener failed: {e}");
            return;
        }
    };
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let id = CONN_ID.fetch_add(1, Ordering::Relaxed);
                with_registry(|r| {
                    r.insert(id, Entry {
                        info: ConnInfo { instance: 0, ticket: None, route: None, outproxy: None, gen: 0, spliced: false },
                        abort: None,
                    })
                });
                // Built outside the task: one aborted before its first poll still leaves the registry.
                let registered = Registered(id);
                // spawn-detached: one bridged connection; its ticket names the session it serves.
                let task = tokio::spawn(async move {
                    let _registered = registered;
                    if let Err(e) = serve(id, stream, port).await {
                        crate::log_debug!("[Transport] bridge connection ended: {e}");
                    }
                });
                update_abort(id, task.abort_handle());
            }
            Err(e) => {
                crate::log_warn!("[Transport] bridge accept error: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

fn update_abort(id: u64, h: tokio::task::AbortHandle) {
    with_registry(|r| {
        if let Some(e) = r.get_mut(&id) {
            e.abort = Some(h);
        }
    });
}

async fn reply(conn: &mut TcpStream, rep: u8) {
    let _ = conn.write_all(&[VER, rep, 0x00, ATYP_V4, 0, 0, 0, 0, 0, 0]).await;
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// What the handshake established: a ticket and a destination, or a reply already sent.
enum Request {
    Ready { ticket: Ticket, dest: Dest, port: u16 },
    Done,
}

async fn handshake(conn: &mut TcpStream, port: Port) -> Result<Request, String> {
    let mut hdr = [0u8; 2];
    conn.read_exact(&mut hdr).await.map_err(|e| format!("greeting: {e}"))?;
    if hdr[0] != VER {
        return Err(format!("not SOCKS5: {}", hdr[0]));
    }
    let mut methods = vec![0u8; hdr[1] as usize];
    conn.read_exact(&mut methods).await.map_err(|e| format!("methods: {e}"))?;
    let want = if port == Port::Main { USER_PASS } else { NO_AUTH };
    if !methods.contains(&want) {
        let _ = conn.write_all(&[VER, NO_METHOD]).await;
        return Ok(Request::Done);
    }
    conn.write_all(&[VER, want]).await.map_err(|e| format!("method ack: {e}"))?;

    let mut ticket = None;
    if port == Port::Main {
        let mut v = [0u8; 2];
        conn.read_exact(&mut v).await.map_err(|e| format!("auth: {e}"))?;
        let mut user = vec![0u8; v[1] as usize];
        conn.read_exact(&mut user).await.map_err(|e| format!("auth: {e}"))?;
        let mut plen = [0u8; 1];
        conn.read_exact(&mut plen).await.map_err(|e| format!("auth: {e}"))?;
        let mut pass = Zeroizing::new(vec![0u8; plen[0] as usize]);
        conn.read_exact(&mut pass).await.map_err(|e| format!("auth: {e}"))?;
        let secret_ok = bridge().is_ok_and(|b| ct_eq(&pass, b.secret.as_bytes()));
        let parsed = std::str::from_utf8(&user).ok().and_then(Ticket::decode);
        match (v[0] == 0x01 && secret_ok, parsed) {
            (true, Some(t)) => {
                conn.write_all(&[0x01, 0x00]).await.map_err(|e| format!("auth ack: {e}"))?;
                ticket = Some(t);
            }
            _ => {
                let _ = conn.write_all(&[0x01, 0x01]).await;
                return Ok(Request::Done);
            }
        }
    }

    let mut req = [0u8; 4];
    conn.read_exact(&mut req).await.map_err(|e| format!("request: {e}"))?;
    if req[0] != VER {
        return Err("bad request version".into());
    }
    if req[1] != CONNECT {
        reply(conn, REP_BAD_CMD).await;
        return Ok(Request::Done);
    }
    let host = match req[3] {
        ATYP_V4 => {
            let mut b = [0u8; 4];
            conn.read_exact(&mut b).await.map_err(|e| format!("ipv4: {e}"))?;
            Ipv4Addr::from(b).to_string()
        }
        ATYP_DOMAIN => {
            let mut len = [0u8; 1];
            conn.read_exact(&mut len).await.map_err(|e| format!("domain: {e}"))?;
            let mut name = vec![0u8; len[0] as usize];
            conn.read_exact(&mut name).await.map_err(|e| format!("domain: {e}"))?;
            String::from_utf8(name).unwrap_or_default()
        }
        ATYP_V6 => {
            let mut b = [0u8; 16];
            conn.read_exact(&mut b).await.map_err(|e| format!("ipv6: {e}"))?;
            std::net::Ipv6Addr::from(b).to_string()
        }
        _ => {
            reply(conn, REP_BAD_ATYP).await;
            return Ok(Request::Done);
        }
    };
    let mut pb = [0u8; 2];
    conn.read_exact(&mut pb).await.map_err(|e| format!("port: {e}"))?;
    let dport = u16::from_be_bytes(pb);
    let Some(dest) = Dest::parse(&host) else {
        reply(conn, REP_NOT_ALLOWED).await;
        return Ok(Request::Done);
    };
    let ticket = match ticket {
        Some(t) => Some(t),
        None => take_permit(&dest, dport),
    };
    match ticket {
        Some(t) if t.is_live() => Ok(Request::Ready { ticket: t, dest, port: dport }),
        _ => {
            reply(conn, REP_NOT_ALLOWED).await;
            Ok(Request::Done)
        }
    }
}

async fn serve(id: u64, mut conn: TcpStream, port: Port) -> Result<(), String> {
    let deadline = Duration::from_millis(HANDSHAKE_DEADLINE_MS.load(Ordering::Relaxed));
    let (ticket, dest, dport) = match tokio::time::timeout(deadline, handshake(&mut conn, port)).await {
        Err(_) => return Err("handshake timed out".into()),
        Ok(Err(e)) => return Err(e),
        Ok(Ok(Request::Done)) => return Ok(()),
        Ok(Ok(Request::Ready { ticket, dest, port })) => (ticket, dest, port),
    };
    update(id, |i| i.ticket = Some(ticket));
    let slot = dials().clone().acquire_owned().await.map_err(|e| e.to_string())?;

    let active = match host::select(&ticket) {
        Ok(a) => a,
        Err(e) => {
            reply(&mut conn, e.socks_reply()).await;
            return Err(e.text());
        }
    };
    let gen = super::policy_gen();
    update(id, |i| {
        i.instance = active.id();
        i.gen = gen;
    });
    let host_name = dest.host();
    let route = host::route_now(&active, &dest, dport);
    if let Route::Refuse(r) = route {
        let e = ConnectError::Refused(r);
        active.record_failure(&host_name, &e);
        reply(&mut conn, e.socks_reply()).await;
        return Err(e.text());
    }
    update(id, |i| i.route = Some(route.clone()));

    let dialed = {
        let dial = host::dial_route(&active, ticket.lane, &host_name, &route);
        tokio::pin!(dial);
        let mut watching = true;
        loop {
            let mut probe = [0u8; 1];
            tokio::select! {
                r = &mut dial => break r,
                p = conn.peek(&mut probe), if watching => match p {
                    // The client hung up: nobody wants this dial any more.
                    Ok(0) | Err(_) => break Err(ConnectError::Cancelled),
                    // Pipelined bytes stay in the socket for the splice.
                    Ok(_) => watching = false,
                },
            }
        }
    };
    let (mut stream, dialed) = match dialed {
        Ok(v) => v,
        Err(e) => {
            reply(&mut conn, e.socks_reply()).await;
            return Err(e.text());
        }
    };
    drop(slot);
    update(id, |i| i.outproxy = dialed.outproxy.clone());

    // The world may have moved while the dial ran: a stale ticket, a new instance, or a route
    // the live settings no longer give all mean this stream must not be handed over.
    let still = ticket.is_live()
        && host::active().is_some_and(|a| a.id() == active.id())
        && host::route_now(&active, &dest, dport) == route
        && dialed.outproxy.as_deref().is_none_or(|o| host::outproxy_allowed(active.kind, o));
    if !still {
        drop(stream);
        reply(&mut conn, REP_NOT_ALLOWED).await;
        return Err("the route changed while dialing".into());
    }

    reply(&mut conn, REP_OK).await;
    // A twin is harmless only under TLS end to end: anything else (plain HTTP or WebSocket to
    // port 443) would hand a wrong twin's operator the request in the clear.
    if matches!(route, Route::Twin { .. }) && !opens_with_tls(&conn).await {
        drop(stream);
        return Err("only TLS may ride a twin".into());
    }
    update(id, |i| i.spliced = true);
    let _ = tokio::io::copy_bidirectional(&mut conn, &mut stream).await;
    Ok(())
}

/// Whether the client's first byte is a TLS handshake record. Nothing is consumed.
async fn opens_with_tls(conn: &TcpStream) -> bool {
    const TLS_HANDSHAKE: u8 = 0x16;
    let mut first = [0u8; 1];
    matches!(
        tokio::time::timeout(TWIN_FIRST_BYTE, conn.peek(&mut first)).await,
        Ok(Ok(1..)) if first[0] == TLS_HANDSHAKE
    )
}
