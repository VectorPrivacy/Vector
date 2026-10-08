#![cfg(test)]
//! A scriptable SAM bridge that records every line a client sends it, so tests can assert the
//! client only ever speaks the client-side subset of SAM.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Debug)]
pub(crate) enum HelloMode {
    Ok(&'static str),
    NoVersion,
    I2pError,
    Garbage,
    Close,
}

#[derive(Clone, Debug)]
pub(crate) enum Peer {
    /// `RESULT=OK`, then echo.
    Echo,
    /// `RESULT=OK` and these bytes in the same write, then echo.
    Greet(Vec<u8>),
    /// `RESULT=OK`; read an HTTP CONNECT head, answer with these bytes, then echo.
    Outproxy(Vec<u8>),
    /// `RESULT=OK`; read the head and never answer.
    OutproxyMute,
    /// `STREAM STATUS RESULT=<this>`.
    Status(&'static str),
    /// An over-long reply line.
    Garbled,
    /// Close without a reply.
    Close,
    Slow(Duration, Box<Peer>),
}

pub(crate) struct Script {
    pub hello: HelloMode,
    pub require_auth: Option<(String, String)>,
    pub session_delay: Duration,
    pub session_error: Option<String>,
    pub peers: HashMap<String, Peer>,
    pub names: HashMap<String, String>,
    pub name_miss: &'static str,
    /// The answer to a connect toward a destination that can't exist (the liveness probe).
    pub bad_dest_reply: &'static str,
    pub ping_on_control: bool,
}

impl Default for Script {
    fn default() -> Self {
        Script {
            hello: HelloMode::Ok("3.3"),
            require_auth: None,
            session_delay: Duration::ZERO,
            session_error: None,
            peers: HashMap::new(),
            names: HashMap::new(),
            name_miss: "INVALID_KEY",
            bad_dest_reply: "INVALID_KEY",
            ping_on_control: false,
        }
    }
}

#[derive(Default)]
pub(crate) struct St {
    pub lines: Mutex<Vec<String>>,
    pub script: Mutex<Script>,
    sessions: Mutex<HashMap<String, Arc<tokio::sync::Notify>>>,
    /// `(nick, destination, TO_PORT)` of every STREAM CONNECT.
    pub connects: Mutex<Vec<(String, String, Option<u16>)>>,
    pub heads: Mutex<Vec<String>>,
    pub pongs: Mutex<Vec<String>>,
    conns: Mutex<Vec<tokio::task::AbortHandle>>,
}

pub(crate) struct MockSam {
    pub port: u16,
    pub st: Arc<St>,
    accept: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl MockSam {
    pub async fn start(script: Script) -> Arc<MockSam> {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let st = Arc::new(St { script: Mutex::new(script), ..Default::default() });
        let m = Arc::new(MockSam { port, st, accept: Mutex::new(None) });
        m.serve(l);
        m
    }

    fn serve(&self, l: TcpListener) {
        let st = self.st.clone();
        let h = tokio::spawn(async move {
            loop {
                let Ok((s, _)) = l.accept().await else { return };
                let st2 = st.clone();
                let t = tokio::spawn(conn(st2, s));
                st.conns.lock().unwrap().push(t.abort_handle());
            }
        });
        *self.accept.lock().unwrap() = Some(h);
    }

    /// The router stops: the port refuses, every socket closes, every session is gone.
    pub fn down(&self) {
        if let Some(h) = self.accept.lock().unwrap().take() {
            h.abort();
        }
        self.drop_sessions();
        for c in self.st.conns.lock().unwrap().drain(..) {
            c.abort();
        }
    }

    /// The router comes back on the same port.
    pub async fn up(&self) {
        let mut tries = 0;
        let l = loop {
            match TcpListener::bind(("127.0.0.1", self.port)).await {
                Ok(l) => break l,
                Err(e) if tries < 50 => {
                    tries += 1;
                    let _ = e;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("rebind mock SAM: {e}"),
            }
        };
        self.serve(l);
    }

    /// The router forgets every session but stays up.
    pub fn drop_sessions(&self) {
        for (_, kill) in self.st.sessions.lock().unwrap().drain() {
            kill.notify_one();
        }
    }

    pub fn sessions(&self) -> Vec<String> {
        let mut v: Vec<String> = self.st.sessions.lock().unwrap().keys().cloned().collect();
        v.sort();
        v
    }

    pub fn lines(&self) -> Vec<String> {
        self.st.lines.lock().unwrap().clone()
    }

    pub fn connects(&self) -> Vec<(String, String, Option<u16>)> {
        self.st.connects.lock().unwrap().clone()
    }

    pub fn script(&self) -> MutexGuard<'_, Script> {
        self.st.script.lock().unwrap()
    }
}

impl Drop for MockSam {
    fn drop(&mut self) {
        self.down();
    }
}

async fn read_line(s: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut b = [0u8; 1];
    loop {
        match s.read(&mut b).await {
            Ok(0) | Err(_) => return None,
            Ok(_) if b[0] == b'\n' => return Some(String::from_utf8_lossy(&buf).into_owned()),
            Ok(_) => buf.push(b[0]),
        }
    }
}

fn arg<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split(' ').find_map(|t| t.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

async fn conn(st: Arc<St>, mut s: TcpStream) {
    let Some(hello) = read_line(&mut s).await else { return };
    st.lines.lock().unwrap().push(hello.clone());
    let (mode, auth) = {
        let sc = st.script.lock().unwrap();
        (sc.hello.clone(), sc.require_auth.clone())
    };
    let reply = match mode {
        HelloMode::Close => return,
        HelloMode::Garbage => "SSH-2.0-OpenSSH_9.6\n".to_string(),
        HelloMode::NoVersion => "HELLO REPLY RESULT=NOVERSION\n".to_string(),
        HelloMode::I2pError => "HELLO REPLY RESULT=I2P_ERROR MESSAGE=\"USER and PASSWORD required\"\n".to_string(),
        HelloMode::Ok(v) => {
            let given = (arg(&hello, "USER").map(str::to_string), arg(&hello, "PASSWORD").map(str::to_string));
            match auth {
                Some((u, p)) if given != (Some(u.clone()), Some(p.clone())) => {
                    "HELLO REPLY RESULT=I2P_ERROR MESSAGE=\"Authorization failed\"\n".to_string()
                }
                _ => format!("HELLO REPLY RESULT=OK VERSION={v}\n"),
            }
        }
    };
    if s.write_all(reply.as_bytes()).await.is_err() || !reply.contains("RESULT=OK") {
        return;
    }
    let Some(line) = read_line(&mut s).await else { return };
    st.lines.lock().unwrap().push(line.clone());
    if line.starts_with("SESSION CREATE ") {
        session(st, s, &line).await;
    } else if line.starts_with("STREAM CONNECT ") {
        stream(st, s, &line).await;
    } else if line.starts_with("NAMING LOOKUP ") {
        let name = arg(&line, "NAME").unwrap_or("").to_string();
        let (hit, miss) = {
            let sc = st.script.lock().unwrap();
            (sc.names.get(&name).cloned(), sc.name_miss)
        };
        let reply = match hit {
            Some(v) => format!("NAMING REPLY RESULT=OK NAME={name} VALUE={v}\n"),
            None => format!("NAMING REPLY RESULT={miss} NAME={name}\n"),
        };
        let _ = s.write_all(reply.as_bytes()).await;
    }
}

async fn session(st: Arc<St>, mut s: TcpStream, line: &str) {
    let nick = arg(line, "ID").unwrap_or("").to_string();
    let (delay, err, ping) = {
        let sc = st.script.lock().unwrap();
        (sc.session_delay, sc.session_error.clone(), sc.ping_on_control)
    };
    tokio::time::sleep(delay).await;
    if let Some(e) = err {
        let _ = s.write_all(format!("SESSION STATUS RESULT=I2P_ERROR MESSAGE=\"{e}\"\n").as_bytes()).await;
        return;
    }
    let kill = Arc::new(tokio::sync::Notify::new());
    let duplicate = {
        let mut sessions = st.sessions.lock().unwrap();
        let dup = sessions.contains_key(&nick);
        if !dup {
            sessions.insert(nick.clone(), kill.clone());
        }
        dup
    };
    if duplicate {
        let _ = s.write_all(b"SESSION STATUS RESULT=DUPLICATED_ID\n").await;
        return;
    }
    let key = "A".repeat(884);
    if s.write_all(format!("SESSION STATUS RESULT=OK DESTINATION={key}\n").as_bytes()).await.is_err() {
        st.sessions.lock().unwrap().remove(&nick);
        return;
    }
    if ping {
        let _ = s.write_all(b"PING mock-1\n").await;
    }
    loop {
        tokio::select! {
            _ = kill.notified() => break,
            l = read_line(&mut s) => match l {
                Some(l) if l.starts_with("PONG") => st.pongs.lock().unwrap().push(l),
                Some(l) if l == "NAMING LOOKUP NAME=ME" => {
                    st.lines.lock().unwrap().push(l);
                    let reply = format!("NAMING REPLY RESULT=OK NAME=ME VALUE={}\n", own_destination(&nick));
                    if s.write_all(reply.as_bytes()).await.is_err() {
                        break;
                    }
                }
                Some(l) => st.lines.lock().unwrap().push(l),
                None => break,
            },
        }
    }
    st.sessions.lock().unwrap().remove(&nick);
}

fn looks_like_destination(d: &str) -> bool {
    crate::transport::route::is_b32(d) || (d.len() >= 300 && d.bytes().all(|b| b.is_ascii_alphanumeric() || b"-~=".contains(&b)))
}

async fn stream(st: Arc<St>, mut s: TcpStream, line: &str) {
    let nick = arg(line, "ID").unwrap_or("").to_string();
    let dest = arg(line, "DESTINATION").unwrap_or("").to_string();
    let to_port = arg(line, "TO_PORT").and_then(|p| p.parse().ok());
    st.connects.lock().unwrap().push((nick.clone(), dest.clone(), to_port));
    if !st.sessions.lock().unwrap().contains_key(&nick) {
        let _ = s.write_all(b"STREAM STATUS RESULT=INVALID_ID MESSAGE=\"no such session\"\n").await;
        return;
    }
    let peer = {
        let sc = st.script.lock().unwrap();
        match sc.peers.get(&dest) {
            Some(p) => p.clone(),
            None if looks_like_destination(&dest) => Peer::Status("CANT_REACH_PEER"),
            None => Peer::Status(sc.bad_dest_reply),
        }
    };
    serve_peer(st, s, peer).await;
}

async fn serve_peer(st: Arc<St>, mut s: TcpStream, mut peer: Peer) {
    while let Peer::Slow(d, inner) = peer {
        tokio::time::sleep(d).await;
        peer = *inner;
    }
    match peer {
        Peer::Status(r) => {
            let msg = if r == "CANT_REACH_PEER" { " MESSAGE=\"LeaseSet not found\"" } else { "" };
            let _ = s.write_all(format!("STREAM STATUS RESULT={r}{msg}\n").as_bytes()).await;
        }
        Peer::Garbled => {
            let _ = s.write_all(format!("STREAM STATUS RESULT={}\n", "X".repeat(5000)).as_bytes()).await;
        }
        Peer::Close => {}
        Peer::Echo => {
            if s.write_all(b"STREAM STATUS RESULT=OK\n").await.is_ok() {
                echo(s).await;
            }
        }
        Peer::Greet(bytes) => {
            let mut out = b"STREAM STATUS RESULT=OK\n".to_vec();
            out.extend_from_slice(&bytes);
            if s.write_all(&out).await.is_ok() {
                echo(s).await;
            }
        }
        Peer::Outproxy(answer) => {
            if s.write_all(b"STREAM STATUS RESULT=OK\n").await.is_err() {
                return;
            }
            let Some(head) = read_head(&mut s).await else { return };
            st.heads.lock().unwrap().push(head);
            if s.write_all(&answer).await.is_ok() {
                echo(s).await;
            }
        }
        Peer::OutproxyMute => {
            if s.write_all(b"STREAM STATUS RESULT=OK\n").await.is_err() {
                return;
            }
            if let Some(head) = read_head(&mut s).await {
                st.heads.lock().unwrap().push(head);
            }
            tokio::time::sleep(Duration::from_secs(120)).await;
        }
        Peer::Slow(..) => unreachable!(),
    }
}

async fn read_head(s: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut b = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        match s.read(&mut b).await {
            Ok(0) | Err(_) => return None,
            Ok(_) => buf.push(b[0]),
        }
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

async fn echo(mut s: TcpStream) {
    let (mut r, mut w) = s.split();
    let _ = tokio::io::copy(&mut r, &mut w).await;
}

/// The client-side subset of SAM, and nothing else: no PING, no inbound, no other styles, and
/// every session created unpublished with zero-hop tunnels off.
pub(crate) fn assert_client_only(lines: &[String]) {
    for l in lines {
        for banned in ["PING", "STREAM ACCEPT", "STREAM FORWARD", "DATAGRAM", "RAW", "PRIMARY", "SESSION ADD"] {
            assert!(!l.starts_with(banned), "the client sent {banned}: {l}");
        }
        if l.starts_with("SESSION CREATE") {
            for must in [
                "STYLE=STREAM",
                "DESTINATION=TRANSIENT",
                "i2cp.dontPublishLeaseSet=true",
                "inbound.allowZeroHop=false",
                "outbound.allowZeroHop=false",
            ] {
                assert!(l.split(' ').any(|t| t == must), "{must} missing: {l}");
            }
        }
        let first = l.split(' ').next().unwrap_or("");
        assert!(["HELLO", "SESSION", "STREAM", "NAMING", "PONG"].contains(&first), "unexpected line: {l}");
    }
}

/// A session's public destination in I2P base64, distinct per nickname.
pub fn own_destination(nick: &str) -> String {
    use sha2::Digest;
    let seed = sha2::Sha256::digest(nick.as_bytes());
    let bytes: Vec<u8> = seed.iter().copied().cycle().take(391).collect();
    base64_simd::STANDARD.encode_to_string(&bytes).replace('+', "-").replace('/', "~")
}
