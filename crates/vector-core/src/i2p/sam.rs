//! A minimal SAMv3 client for a router the user already runs. It speaks five commands and
//! nothing else: transient STREAM sessions that never publish a LeaseSet or build a zero-hop
//! tunnel, outbound stream connects, name lookups and a liveness probe. No line it sends can
//! make the router accept a connection or carry someone else's traffic.

use std::net::Ipv4Addr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use zeroize::Zeroizing;

/// SAM is cleartext, so it never leaves the loopback interface.
pub const SAM_HOST: Ipv4Addr = Ipv4Addr::LOCALHOST;

const TCP_CONNECT: Duration = Duration::from_secs(5);
const HELLO_REPLY: Duration = Duration::from_secs(5);
pub(crate) const SESSION_CAP: Duration = Duration::from_secs(120);
pub(crate) const STREAM_CAP: Duration = Duration::from_secs(90);
const LOOKUP_CAP: Duration = Duration::from_secs(30);
const OWN_ADDRESS_CAP: Duration = Duration::from_secs(10);
const PROBE_CAP: Duration = Duration::from_secs(10);
const MAX_LINE: usize = 4096;
/// The SESSION STATUS reply carries the transient private key.
const MAX_SESSION_LINE: usize = 16 * 1024;

/// Options every session is created with. Zero-hop tunnels off in both directions; the LeaseSet
/// is never published, so nothing can reach this destination unasked.
pub(crate) const SESSION_OPTIONS: &str = "SIGNATURE_TYPE=7 i2cp.leaseSetEncType=6,4 i2cp.dontPublishLeaseSet=true \
inbound.length=3 inbound.quantity=3 outbound.length=3 outbound.quantity=3 inbound.allowZeroHop=false outbound.allowZeroHop=false";

#[derive(Clone)]
pub struct SamAuth {
    pub user: String,
    pub password: Zeroizing<String>,
}

impl SamAuth {
    pub fn new(user: &str, password: &str) -> Self {
        SamAuth { user: user.to_string(), password: Zeroizing::new(password.to_string()) }
    }
}

impl PartialEq for SamAuth {
    fn eq(&self, other: &Self) -> bool {
        self.user == other.user && self.password.as_str() == other.password.as_str()
    }
}

impl std::fmt::Debug for SamAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SamAuth").field("user", &self.user).finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SamVersion {
    pub major: u8,
    pub minor: u8,
}

impl SamVersion {
    pub const MIN: SamVersion = SamVersion { major: 3, minor: 2 };

    fn parse(v: &str) -> Option<SamVersion> {
        let (a, b) = v.split_once('.')?;
        Some(SamVersion { major: a.parse().ok()?, minor: b.split('.').next()?.parse().ok()? })
    }
}

impl std::fmt::Display for SamVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelloError {
    /// Nothing answered on the port (refused, reset, or closed before a reply).
    Refused,
    Timeout,
    /// Something took the connection and never replied: a web console or proxy, most often.
    Silent,
    /// Something answered, but not a SAM bridge.
    NotSam,
    /// The router can't speak 3.2 or newer; the version it named, when it named one.
    NoVersion(String),
    AuthRequired,
    AuthFailed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamFail {
    RouterDown,
    SessionGone,
    /// The router answered oddly (`I2P_ERROR`, a garbled line, EOF): the session may be gone.
    SessionSuspect(String),
    Unreachable(String),
    BadKey,
    Timeout,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Liveness {
    Alive,
    Gone,
    RouterDown,
    Inconclusive(String),
}

// ── Lines ────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) enum LineError {
    Eof,
    TooLong,
    Io(std::io::Error),
}

/// One reply line, read a byte at a time so nothing after the `\n` is consumed: on a stream
/// socket every later byte belongs to the stream.
pub(crate) async fn read_line(s: &mut TcpStream, max: usize) -> Result<Zeroizing<String>, LineError> {
    // Sized for the longest line up front: a growing buffer would leave copies of a session's
    // private key behind in freed memory, outside the zeroized one.
    let mut buf = Zeroizing::new(Vec::with_capacity(max));
    let mut b = [0u8; 1];
    loop {
        match s.read(&mut b).await {
            Ok(0) => return Err(LineError::Eof),
            Ok(_) => {
                if b[0] == b'\n' {
                    break;
                }
                if buf.len() >= max {
                    return Err(LineError::TooLong);
                }
                buf.push(b[0]);
            }
            Err(e) => return Err(LineError::Io(e)),
        }
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(Zeroizing::new(String::from_utf8_lossy(&buf).into_owned()))
}

/// A reply split into its two command words and its `KEY=VALUE` pairs. Values may be quoted.
#[derive(Debug, Default)]
pub(crate) struct Reply {
    pub topic: String,
    pub kind: String,
    pub args: Vec<(String, String)>,
}

impl Reply {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.args.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn result(&self) -> &str {
        self.get("RESULT").unwrap_or("")
    }

    /// The router's own words for a failure, short and printable.
    pub fn detail(&self) -> String {
        let raw = self.get("MESSAGE").filter(|m| !m.is_empty()).unwrap_or_else(|| self.result());
        let clean: String = raw.chars().filter(|c| !c.is_control()).take(120).collect();
        if clean.is_empty() {
            "no reason given".into()
        } else {
            clean
        }
    }
}

pub(crate) fn parse(line: &str) -> Reply {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    let mut it = tokens.into_iter();
    let topic = it.next().unwrap_or_default();
    let kind = it.next().unwrap_or_default();
    let args = it.filter_map(|t| t.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))).collect();
    Reply { topic, kind, args }
}

/// A SESSION STATUS reply, cut before its `DESTINATION` (the private key) so the key is never
/// copied out of the zeroized line.
fn parse_session_reply(line: &str) -> Reply {
    match line.find(" DESTINATION=") {
        Some(i) => parse(&line[..i]),
        None => parse(line),
    }
}

async fn send_line(s: &mut TcpStream, line: &str) -> std::io::Result<()> {
    let mut out = Zeroizing::new(String::with_capacity(line.len() + 1));
    out.push_str(line);
    out.push('\n');
    s.write_all(out.as_bytes()).await
}

fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut b = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut b);
    crate::simd::hex::bytes_to_hex_string(&b)
}

/// A fresh session nickname: 128 random bits, nothing that names the app or the account.
pub fn new_nick() -> String {
    random_hex(16)
}

// ── HELLO ────────────────────────────────────────────────────────────────────

/// Built in one allocation of its exact size, so the password is never copied into a buffer that
/// is freed without being zeroized.
fn hello_line(auth: Option<&SamAuth>) -> Zeroizing<String> {
    const BASE: &str = "HELLO VERSION MIN=3.2 MAX=3.3";
    let Some(a) = auth else { return Zeroizing::new(BASE.to_string()) };
    let parts = [BASE, " USER=", a.user.as_str(), " PASSWORD=", a.password.as_str()];
    let mut line = Zeroizing::new(String::with_capacity(parts.iter().map(|p| p.len()).sum()));
    for p in parts {
        line.push_str(p);
    }
    line
}

/// Open a SAM socket and agree on 3.2 or newer.
pub async fn hello(port: u16, auth: Option<&SamAuth>) -> Result<(TcpStream, SamVersion), HelloError> {
    let mut s = match tokio::time::timeout(TCP_CONNECT, TcpStream::connect((SAM_HOST, port))).await {
        Err(_) => return Err(HelloError::Timeout),
        Ok(Err(_)) => return Err(HelloError::Refused),
        Ok(Ok(s)) => s,
    };
    let _ = s.set_nodelay(true);
    let line = hello_line(auth);
    if send_line(&mut s, &line).await.is_err() {
        return Err(HelloError::Refused);
    }
    let reply = match tokio::time::timeout(HELLO_REPLY, read_line(&mut s, MAX_LINE)).await {
        Err(_) => return Err(HelloError::Silent),
        Ok(Err(LineError::TooLong)) => return Err(HelloError::NotSam),
        Ok(Err(_)) => return Err(HelloError::Refused),
        Ok(Ok(l)) => parse(&l),
    };
    if reply.topic != "HELLO" || reply.kind != "REPLY" {
        return Err(HelloError::NotSam);
    }
    match reply.result() {
        "OK" => {
            let named = reply.get("VERSION").unwrap_or("").to_string();
            match SamVersion::parse(&named) {
                Some(v) if v >= SamVersion::MIN => Ok((s, v)),
                _ => Err(HelloError::NoVersion(named)),
            }
        }
        "NOVERSION" => Err(HelloError::NoVersion(String::new())),
        "I2P_ERROR" if auth.is_some() => Err(HelloError::AuthFailed),
        "I2P_ERROR" => Err(HelloError::AuthRequired),
        _ => Err(HelloError::NotSam),
    }
}

// ── Sessions ─────────────────────────────────────────────────────────────────

/// A session's control socket. Dropping it ends the session: the router tears a session down
/// within half a second of its control socket closing.
pub struct ControlSocket {
    stream: TcpStream,
    buf: Vec<u8>,
    address: Option<String>,
}

/// What the router said on a session's control socket.
#[derive(Debug, PartialEq, Eq)]
pub enum ControlEvent {
    /// The socket closed: the session is gone.
    Closed(String),
    /// The router pinged; answer with [`ControlSocket::pong`].
    Ping(String),
    Other,
}

impl ControlSocket {
    /// The session's own `.b32.i2p` address, when the router told us.
    pub fn address(&self) -> Option<&str> {
        self.address.as_deref()
    }

    /// Wait for the router to say something. Cancel-safe: a partial line stays buffered.
    pub async fn next_event(&mut self) -> ControlEvent {
        loop {
            if let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=i).collect();
                let text = String::from_utf8_lossy(&line).trim().to_string();
                return match text.strip_prefix("PING") {
                    Some(rest) if rest.is_empty() || rest.starts_with(' ') => ControlEvent::Ping(rest.trim().to_string()),
                    _ => ControlEvent::Other,
                };
            }
            if self.buf.len() > MAX_LINE {
                self.buf.clear();
            }
            let mut chunk = [0u8; 256];
            match self.stream.read(&mut chunk).await {
                Ok(0) => return ControlEvent::Closed("the router closed the session".into()),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) => return ControlEvent::Closed(e.to_string()),
            }
        }
    }

    /// Answer a router's PING. Vector never sends one of its own.
    pub async fn pong(&mut self, text: &str) -> std::io::Result<()> {
        let line = if text.is_empty() { "PONG".to_string() } else { format!("PONG {text}") };
        send_line(&mut self.stream, &line).await
    }
}

/// Create a transient STREAM session named `nick` on its own control socket.
pub async fn create_session(port: u16, auth: Option<&SamAuth>, nick: &str) -> Result<ControlSocket, String> {
    let (mut s, _) = hello(port, auth).await.map_err(|e| hello_text(&e, port))?;
    let line = format!("SESSION CREATE STYLE=STREAM ID={nick} DESTINATION=TRANSIENT {SESSION_OPTIONS}");
    send_line(&mut s, &line).await.map_err(|e| e.to_string())?;
    let reply = match tokio::time::timeout(SESSION_CAP, read_line(&mut s, MAX_SESSION_LINE)).await {
        Err(_) => return Err("timed out".into()),
        Ok(Err(LineError::Eof)) => return Err("the router closed the connection".into()),
        Ok(Err(LineError::TooLong)) => return Err("an unreadable reply".into()),
        Ok(Err(LineError::Io(e))) => return Err(e.to_string()),
        Ok(Ok(l)) => parse_session_reply(&l),
    };
    if reply.topic != "SESSION" || reply.kind != "STATUS" {
        return Err("an unreadable reply".into());
    }
    match reply.result() {
        "OK" => {
            let address = own_address(&mut s).await;
            Ok(ControlSocket { stream: s, buf: Vec::new(), address })
        }
        _ => Err(reply.detail()),
    }
}

/// The session's public address, asked of the router rather than derived from the private key
/// its SESSION STATUS carried. Best effort: a router that won't say leaves the session usable.
async fn own_address(s: &mut TcpStream) -> Option<String> {
    send_line(s, "NAMING LOOKUP NAME=ME").await.ok()?;
    let read = async {
        loop {
            let line = read_line(s, MAX_SESSION_LINE).await.ok()?;
            if let Some(rest) = line.strip_prefix("PING") {
                send_line(s, &format!("PONG{rest}")).await.ok()?;
                continue;
            }
            let reply = parse(&line);
            if reply.topic == "NAMING" && reply.kind == "REPLY" {
                return match reply.result() {
                    "OK" => reply.get("VALUE").and_then(b32_of),
                    _ => None,
                };
            }
        }
    };
    tokio::time::timeout(OWN_ADDRESS_CAP, read).await.ok().flatten()
}

/// `xxxx.b32.i2p` for a destination in I2P's base64 (`-` and `~` stand for `+` and `/`).
pub fn b32_of(destination: &str) -> Option<String> {
    use sha2::Digest;
    let standard: String = destination.chars().map(|c| match c { '-' => '+', '~' => '/', c => c }).collect();
    let bytes = base64_simd::STANDARD.decode_to_vec(standard.as_bytes()).ok()?;
    // 256-byte encryption key, 128-byte signing key, 3-byte certificate header.
    if bytes.len() < 387 {
        return None;
    }
    Some(format!("{}.b32.i2p", base32_lower(&sha2::Sha256::digest(&bytes))))
}

fn base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &b in bytes {
        acc = (acc << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// A short reason for a failed HELLO, for logs and session errors.
pub fn hello_text(e: &HelloError, port: u16) -> String {
    match e {
        HelloError::Refused | HelloError::Timeout => format!("no router answered on port {port}"),
        HelloError::Silent => format!("no SAM reply on port {port}"),
        HelloError::NotSam => "not a SAM bridge".into(),
        HelloError::NoVersion(_) => "SAM too old".into(),
        HelloError::AuthRequired => "SAM credentials required".into(),
        HelloError::AuthFailed => "SAM credentials refused".into(),
    }
}

fn stream_fail(reply: &Reply) -> StreamFail {
    match reply.result() {
        "INVALID_ID" => StreamFail::SessionGone,
        "CANT_REACH_PEER" | "PEER_NOT_FOUND" => StreamFail::Unreachable(reply.detail()),
        "TIMEOUT" => StreamFail::Timeout,
        "INVALID_KEY" => StreamFail::BadKey,
        _ => StreamFail::SessionSuspect(reply.detail()),
    }
}

async fn hello_for_stream(port: u16, auth: Option<&SamAuth>) -> Result<TcpStream, StreamFail> {
    match hello(port, auth).await {
        Ok((s, _)) => Ok(s),
        Err(HelloError::Refused) => Err(StreamFail::RouterDown),
        Err(e) => Err(StreamFail::SessionSuspect(hello_text(&e, port))),
    }
}

async fn read_reply(s: &mut TcpStream, cap: Duration) -> Result<Reply, StreamFail> {
    match tokio::time::timeout(cap, read_line(s, MAX_LINE)).await {
        Err(_) => Err(StreamFail::Timeout),
        Ok(Err(LineError::Eof)) => Err(StreamFail::SessionSuspect("the router closed the connection".into())),
        Ok(Err(LineError::TooLong)) => Err(StreamFail::SessionSuspect("an unreadable reply".into())),
        Ok(Err(LineError::Io(e))) => Err(StreamFail::SessionSuspect(e.to_string())),
        Ok(Ok(l)) => Ok(parse(&l)),
    }
}

/// Open a stream from session `nick` to `dest` (a `.b32.i2p` address or a base64 destination)
/// on `to_port`. The returned socket IS the stream.
pub async fn connect(port: u16, auth: Option<&SamAuth>, nick: &str, dest: &str, to_port: u16) -> Result<TcpStream, StreamFail> {
    let mut s = hello_for_stream(port, auth).await?;
    let line = format!("STREAM CONNECT ID={nick} DESTINATION={dest} SILENT=false TO_PORT={to_port}");
    send_line(&mut s, &line).await.map_err(|e| StreamFail::SessionSuspect(e.to_string()))?;
    let reply = read_reply(&mut s, STREAM_CAP).await?;
    if reply.topic != "STREAM" || reply.kind != "STATUS" {
        return Err(StreamFail::SessionSuspect("an unreadable reply".into()));
    }
    match reply.result() {
        "OK" => Ok(s),
        _ => Err(stream_fail(&reply)),
    }
}

/// Resolve a name (or a b32 a router wants as base64) through the router's address book.
pub async fn lookup(port: u16, auth: Option<&SamAuth>, name: &str) -> Result<String, StreamFail> {
    let mut s = hello_for_stream(port, auth).await?;
    send_line(&mut s, &format!("NAMING LOOKUP NAME={name}")).await.map_err(|e| StreamFail::SessionSuspect(e.to_string()))?;
    let reply = read_reply(&mut s, LOOKUP_CAP).await?;
    if reply.topic != "NAMING" || reply.kind != "REPLY" {
        return Err(StreamFail::SessionSuspect("an unreadable reply".into()));
    }
    match reply.result() {
        "OK" => match reply.get("VALUE") {
            Some(v) if !v.is_empty() && v.bytes().all(|b| b.is_ascii_alphanumeric() || b"-~=".contains(&b)) => Ok(v.to_string()),
            _ => Err(StreamFail::SessionSuspect("an unreadable reply".into())),
        },
        "KEY_NOT_FOUND" | "INVALID_KEY" => Err(StreamFail::BadKey),
        _ => Err(StreamFail::SessionSuspect(reply.detail())),
    }
}

/// Is session `nick` alive? A connect to a destination that can't exist answers at once,
/// without any network traffic: `INVALID_KEY` for a live session, `INVALID_ID` for a gone one.
pub async fn liveness(port: u16, auth: Option<&SamAuth>, nick: &str) -> Liveness {
    let mut s = match hello(port, auth).await {
        Ok((s, _)) => s,
        Err(HelloError::Refused) => return Liveness::RouterDown,
        Err(e) => return Liveness::Inconclusive(hello_text(&e, port)),
    };
    let line = format!("STREAM CONNECT ID={nick} DESTINATION={} SILENT=false", random_hex(8));
    if let Err(e) = send_line(&mut s, &line).await {
        return Liveness::Inconclusive(e.to_string());
    }
    match read_reply(&mut s, PROBE_CAP).await {
        Ok(r) if r.topic == "STREAM" && r.kind == "STATUS" => match r.result() {
            "INVALID_KEY" => Liveness::Alive,
            "INVALID_ID" => Liveness::Gone,
            other => Liveness::Inconclusive(format!("liveness answered {other}")),
        },
        Ok(_) => Liveness::Inconclusive("an unreadable reply".into()),
        Err(e) => Liveness::Inconclusive(format!("{e:?}")),
    }
}

/// What the router test in Settings reports: a HELLO, no session.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RouterTest {
    pub ok: bool,
    pub version: Option<String>,
    pub text: String,
}

pub async fn test_router(port: u16, auth: Option<&SamAuth>) -> RouterTest {
    match hello(port, auth).await {
        Ok((_, v)) => RouterTest { ok: true, version: Some(v.to_string()), text: format!("Found a router. SAM {v}.") },
        Err(e) => {
            let version = match &e {
                HelloError::NoVersion(v) if !v.is_empty() => Some(v.clone()),
                _ => None,
            };
            RouterTest { ok: false, version, text: test_text(&e) }
        }
    }
}

fn test_text(e: &HelloError) -> String {
    match e {
        HelloError::Refused | HelloError::Timeout => "No router answered on this port.".into(),
        // A SAM bridge answers HELLO at once; silence is another service on the wrong port.
        HelloError::Silent | HelloError::NotSam => "This port isn't a SAM bridge.".into(),
        HelloError::NoVersion(v) => too_old_text(v),
        HelloError::AuthRequired => "Your router asks for a SAM username and password.".into(),
        HelloError::AuthFailed => "Your router turned down the SAM username or password.".into(),
    }
}

/// A router that named its version says which; one that only answered NOVERSION can't.
pub(crate) fn too_old_text(v: &str) -> String {
    if v.is_empty() {
        "Your router's SAM is too old. Vector needs 3.2 or newer.".into()
    } else {
        format!("Your router speaks SAM {v}. Vector needs 3.2 or newer.")
    }
}
