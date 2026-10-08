#![cfg(test)]
//! The I2P kind against a scripted SAM bridge: only client-side SAM ever goes out, every router
//! answer maps to the right failure, outproxies fail over in order, and nothing goes direct when
//! the router is down.

use std::sync::{Arc, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::mock_sam::{assert_client_only, HelloMode, MockSam, Peer, Script};
use super::sam::{self, HelloError, Liveness, StreamFail};
use super::*;
use crate::transport::aliases::{self, AliasSource, CheckState};
use crate::transport::i2p_config::{self, Outproxy, OutproxyInput};
use crate::transport::{self, bridge, prefs, Egress, Ticket};

// ── Harness ──────────────────────────────────────────────────────────────────

struct Scene {
    _guard: MutexGuard<'static, ()>,
}

fn reset() {
    drop(host::uninstall());
    transport::set_strict_for_test(false);
    transport::set_preference(Some(Kind::Clearnet));
    transport::prelogin::disarm();
    prefs::live().set_config_value(Kind::I2p, Arc::new(I2pConfig::default()));
    prefs::live().set_aliases(Arc::new(aliases::AliasTable::default()));
}

fn scene() -> Scene {
    let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    reset();
    Scene { _guard: guard }
}

impl Drop for Scene {
    fn drop(&mut self) {
        reset();
    }
}

fn fast() -> Timing {
    let ms = Duration::from_millis;
    Timing {
        backoff: vec![ms(50), ms(100), ms(200), ms(400)],
        watch: ms(150),
        not_sam: ms(300),
        rejected: ms(300),
        session_cap: Duration::from_secs(5),
        head_deadline: ms(500),
        port_watch: ms(20),
        fresh_session: Duration::ZERO,
    }
}

fn live() -> u64 {
    crate::db::live_session_id()
}

fn b32(c: char) -> String {
    format!("{}.b32.i2p", c.to_string().repeat(52))
}

fn b64(tag: &str) -> String {
    format!("{}{tag}", "Q".repeat(400))
}

fn config_for(m: &MockSam) -> I2pConfig {
    I2pConfig { sam_port: m.port, ..Default::default() }
}

fn start(m: &MockSam, auth: Option<(&str, &str)>, timing: Timing) -> Arc<I2pTransport> {
    let mut cfg = config_for(m);
    if let Some((u, p)) = auth {
        cfg.sam_user = Some(u.into());
        cfg.sam_password = Some(p.into());
    }
    let t = Arc::new(I2pTransport::new(&cfg, live(), timing));
    t.spawn_keeper();
    t
}

async fn until(what: &str, max: Duration, f: impl Fn() -> bool) {
    let deadline = web_time::Instant::now() + max;
    while !f() {
        assert!(web_time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn ready(t: &I2pTransport) {
    let deadline = web_time::Instant::now() + Duration::from_secs(15);
    while !t.ready() {
        assert!(
            web_time::Instant::now() < deadline,
            "no sessions: phase {}, owner live {}",
            t.phase_name(),
            t.inner.owner_live()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Use `cfg` as the live account's I2P settings.
fn use_config(cfg: I2pConfig) {
    prefs::live().set_config_value(Kind::I2p, Arc::new(cfg));
}

fn list_of(entries: &[(&str, &str, u16)]) -> Vec<Outproxy> {
    entries.iter().map(|(name, addr, port)| Outproxy::custom(name, addr, *port, true)).collect()
}

fn nick_of(t: &I2pTransport, lane: Lane) -> String {
    t.inner.nick(lane).expect("sessions are up")
}

async fn socks_connect(t: &Ticket, host: &str, port: u16) -> (TcpStream, u8) {
    let mut c = TcpStream::connect(bridge::addr().unwrap()).await.unwrap();
    c.write_all(&[5, 1, 2]).await.unwrap();
    let mut r = [0u8; 2];
    c.read_exact(&mut r).await.unwrap();
    let (user, pass) = bridge::credentials(t);
    let mut auth = vec![1, user.len() as u8];
    auth.extend_from_slice(user.as_bytes());
    auth.push(pass.len() as u8);
    auth.extend_from_slice(pass.as_bytes());
    c.write_all(&auth).await.unwrap();
    c.read_exact(&mut r).await.unwrap();
    assert_eq!(r, [1, 0]);
    let mut req = vec![5, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    c.write_all(&req).await.unwrap();
    let mut rep = [0u8; 10];
    let code = match tokio::time::timeout(Duration::from_secs(10), c.read_exact(&mut rep)).await {
        Ok(Ok(_)) => rep[1],
        _ => 0xEE,
    };
    (c, code)
}

async fn closed(c: &mut TcpStream) -> bool {
    let mut buf = [0u8; 16];
    matches!(tokio::time::timeout(Duration::from_secs(3), c.read(&mut buf)).await, Ok(Ok(0)) | Ok(Err(_)))
}

fn test_account() -> String {
    use nostr_sdk::prelude::ToBech32;
    let acct = nostr_sdk::prelude::Keys::generate().public_key().to_bech32().unwrap();
    crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
    let _ = std::fs::create_dir_all(crate::db::shared_test_data_dir().join(&acct));
    crate::db::set_current_account(acct.clone()).unwrap();
    crate::db::init_database(&acct).unwrap();
    acct
}

// ── SAM lines ────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sam_lines_are_client_only() {
    let _s = scene();
    let (relay, named) = (b32('n'), b64("R"));
    let mut script = Script::default();
    script.peers.insert(relay.clone(), Peer::Echo);
    script.names.insert("relay.i2p".into(), named.clone());
    script.peers.insert(named.clone(), Peer::Echo);
    let m = MockSam::start(script).await;
    let t = start(&m, None, fast());
    ready(&t).await;

    let sessions = m.sessions();
    assert_eq!(sessions.len(), 2, "one session per lane");
    let (acct, shared) = (nick_of(&t, Lane::Account), nick_of(&t, Lane::Shared));
    assert_ne!(acct, shared);
    for (lane, port) in [(Lane::Account, 80), (Lane::Shared, 6878)] {
        t.dial(&Route::Native { host: relay.clone(), port }, lane).await.unwrap();
    }
    t.dial(&Route::Native { host: "relay.i2p".into(), port: 80 }, Lane::Shared).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    let m2 = MockSam::start(Script { require_auth: Some(("vec-user".into(), "p4ss!word".into())), ..Default::default() }).await;
    let t2 = start(&m2, Some(("vec-user", "p4ss!word")), fast());
    ready(&t2).await;
    t.shutdown().await;
    t2.shutdown().await;

    let lines = m.lines();
    assert_client_only(&lines);
    assert_client_only(&m2.lines());
    let creates: Vec<&String> = lines.iter().filter(|l| l.starts_with("SESSION CREATE")).collect();
    assert_eq!(creates.len(), 2);
    for c in creates {
        let nick = c.split(' ').find_map(|t| t.strip_prefix("ID=")).unwrap();
        assert_eq!(nick.len(), 32);
        assert!(nick.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()), "{nick}");
        assert!(!c.to_ascii_lowercase().contains("vector"), "no app marker: {c}");
        assert_eq!(
            c,
            &format!(
                "SESSION CREATE STYLE=STREAM ID={nick} DESTINATION=TRANSIENT SIGNATURE_TYPE=7 i2cp.leaseSetEncType=6,4 \
i2cp.dontPublishLeaseSet=true inbound.length=3 inbound.quantity=3 outbound.length=3 outbound.quantity=3 \
inbound.allowZeroHop=false outbound.allowZeroHop=false"
            )
        );
    }
    assert!(lines.iter().filter(|l| l.starts_with("HELLO")).all(|l| l == "HELLO VERSION MIN=3.2 MAX=3.3"));
    assert!(m2.lines().iter().filter(|l| l.starts_with("HELLO")).all(|l| l == "HELLO VERSION MIN=3.2 MAX=3.3 USER=vec-user PASSWORD=p4ss!word"));

    // Each lane dials from its own session; every stream names its port.
    let connects = m.connects();
    assert!(connects.contains(&(acct.clone(), relay.clone(), Some(80))));
    assert!(connects.contains(&(shared.clone(), relay.clone(), Some(6878))));
    assert!(connects.contains(&(shared.clone(), named.clone(), Some(80))), "a name rides its looked-up destination");
    assert_eq!(lines.iter().filter(|l| *l == "NAMING LOOKUP NAME=relay.i2p").count(), 1);

    // The liveness probe: a random 16-hex destination and no port.
    let probes: Vec<_> = connects.iter().filter(|(_, _, p)| p.is_none()).collect();
    assert!(probes.len() >= 2, "both sessions were probed");
    for (_, d, _) in &probes {
        assert_eq!(d.len(), 16);
        assert!(d.bytes().all(|b| b.is_ascii_hexdigit()));
    }
    assert!(probes.windows(2).any(|w| w[0].1 != w[1].1), "a fresh destination per probe");
    for l in lines.iter().filter(|l| l.starts_with("STREAM CONNECT")) {
        let ok = l.ends_with(" SILENT=false") || l.contains(" SILENT=false TO_PORT=");
        assert!(ok, "{l}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn router_pings_are_answered_never_sent() {
    let _s = scene();
    let m = MockSam::start(Script { ping_on_control: true, ..Default::default() }).await;
    let t = start(&m, None, fast());
    ready(&t).await;
    until("pongs", Duration::from_secs(3), || m.st.pongs.lock().unwrap().len() == 2).await;
    assert!(m.st.pongs.lock().unwrap().iter().all(|p| p == "PONG mock-1"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(t.ready(), "a ping does not cost the session");
    assert_eq!(m.sessions().len(), 2);
    assert_client_only(&m.lines());
    t.shutdown().await;
}

// ── HELLO and the keeper's reasons ───────────────────────────────────────────

async fn dead_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

/// A port where something listens but never speaks SAM (a web console, an HTTP proxy) is named as
/// such in the router test, not as a router that isn't running.
#[tokio::test]
async fn router_test_tells_a_silent_service_from_no_router() {
    // Connections land in the backlog and are never answered.
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = silent.local_addr().unwrap().port();
    let (hello, test) = tokio::join!(sam::hello(port, None), sam::test_router(port, None));
    assert_eq!(hello.err(), Some(HelloError::Silent));
    assert_eq!((test.ok, test.text.as_str()), (false, "This port isn't a SAM bridge."));
    drop(silent);
    let r = sam::test_router(dead_port().await, None).await;
    assert_eq!(r.text, "No router answered on this port.");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hello_version_and_auth_gate() {
    let _s = scene();
    let auth = sam::SamAuth::new("u", "p");
    type Case = (HelloMode, Option<(String, String)>, bool, Result<&'static str, HelloError>);
    let cases: Vec<Case> = vec![
        (HelloMode::Ok("3.3"), None, false, Ok("3.3")),
        (HelloMode::Ok("3.2"), None, false, Ok("3.2")),
        (HelloMode::Ok("3.1"), None, false, Err(HelloError::NoVersion("3.1".into()))),
        (HelloMode::NoVersion, None, false, Err(HelloError::NoVersion(String::new()))),
        (HelloMode::Garbage, None, false, Err(HelloError::NotSam)),
        (HelloMode::Close, None, false, Err(HelloError::Refused)),
        (HelloMode::I2pError, None, false, Err(HelloError::AuthRequired)),
        (HelloMode::I2pError, None, true, Err(HelloError::AuthFailed)),
        (HelloMode::Ok("3.3"), Some(("u".into(), "other".into())), true, Err(HelloError::AuthFailed)),
        (HelloMode::Ok("3.3"), Some(("u".into(), "p".into())), true, Ok("3.3")),
    ];
    for (mode, require, send_auth, want) in cases {
        let m = MockSam::start(Script { hello: mode.clone(), require_auth: require, ..Default::default() }).await;
        let got = sam::hello(m.port, send_auth.then_some(&auth)).await.map(|(_, v)| v.to_string());
        assert_eq!(got.as_deref().map_err(Clone::clone), want, "{mode:?}");
    }
    assert_eq!(sam::hello(dead_port().await, None).await.err(), Some(HelloError::Refused));

    // The keeper turns each into its reason code and text.
    let port = dead_port().await;
    let cases: Vec<(Option<HelloMode>, bool, &str, String)> = vec![
        (None, false, "router_unreachable", format!("Can't reach your I2P router at 127.0.0.1:{port}.")),
        (Some(HelloMode::Garbage), false, "sam_refused", String::new()),
        (Some(HelloMode::NoVersion), false, "sam_too_old", "Your router's SAM is too old. Vector needs 3.2 or newer.".into()),
        (Some(HelloMode::Ok("3.1")), false, "sam_too_old", "Your router speaks SAM 3.1. Vector needs 3.2 or newer.".into()),
        (Some(HelloMode::I2pError), false, "sam_auth_required", "Your router asks for a SAM username and password.".into()),
        (Some(HelloMode::I2pError), true, "sam_auth_failed", "Your router turned down the SAM username or password.".into()),
    ];
    for (mode, with_auth, code, text) in cases {
        let (m, p) = match mode {
            Some(mode) => {
                let m = MockSam::start(Script { hello: mode, ..Default::default() }).await;
                let p = m.port;
                (Some(m), p)
            }
            None => (None, port),
        };
        let cfg = I2pConfig {
            sam_port: p,
            sam_user: with_auth.then(|| "u".into()),
            sam_password: with_auth.then(|| "p".into()),
            ..Default::default()
        };
        let t = Arc::new(I2pTransport::new(&cfg, live(), fast()));
        t.spawn_keeper();
        until(code, Duration::from_secs(3), || t.kind_status().reason.is_some_and(|r| r.code == code)).await;
        let st = t.kind_status();
        let want = if code == "sam_refused" { format!("The service on port {p} isn't an I2P SAM bridge.") } else { text };
        assert_eq!(st.reason.unwrap().text, want);
        assert_eq!(st.phase, crate::transport::status::Phase::Waiting);
        assert!(st.retry_in.is_some());
        assert_eq!(st.steps[0].state, "fail");
        assert!(!t.ready());
        t.shutdown().await;
        drop(m);
    }

    // The router test in Settings: a HELLO, no session.
    let m = MockSam::start(Script::default()).await;
    let r = sam::test_router(m.port, None).await;
    assert_eq!((r.ok, r.version.as_deref(), r.text.as_str()), (true, Some("3.3"), "Found a router. SAM 3.3."));
    assert!(m.sessions().is_empty() && !m.lines().iter().any(|l| l.starts_with("SESSION")));
    assert_eq!(sam::test_router(dead_port().await, None).await.text, "No router answered on this port.");
    let m = MockSam::start(Script { hello: HelloMode::Garbage, ..Default::default() }).await;
    assert_eq!(sam::test_router(m.port, None).await.text, "This port isn't a SAM bridge.");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn liveness_probe_classifies() {
    let _s = scene();
    let m = MockSam::start(Script::default()).await;
    let nick = sam::new_nick();
    let ctl = sam::create_session(m.port, None, &nick).await.unwrap();
    assert_eq!(sam::liveness(m.port, None, &nick).await, Liveness::Alive);
    assert_eq!(sam::liveness(m.port, None, &sam::new_nick()).await, Liveness::Gone);
    m.script().bad_dest_reply = "I2P_ERROR";
    assert!(matches!(sam::liveness(m.port, None, &nick).await, Liveness::Inconclusive(_)));
    m.script().bad_dest_reply = "INVALID_KEY";
    drop(ctl);
    until("session gone", Duration::from_secs(2), || m.sessions().is_empty()).await;
    assert_eq!(sam::liveness(m.port, None, &nick).await, Liveness::Gone, "closing the control socket ends the session");
    m.down();
    assert_eq!(sam::liveness(m.port, None, &nick).await, Liveness::RouterDown);
    assert_client_only(&m.lines());
}

// ── Keeper ───────────────────────────────────────────────────────────────────

fn phases_recorder(t: Arc<I2pTransport>) -> (Arc<std::sync::Mutex<Vec<&'static str>>>, tokio::task::JoinHandle<()>) {
    let seen = Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));
    let s2 = seen.clone();
    let h = tokio::spawn(async move {
        loop {
            let p = t.phase_name();
            {
                let mut v = s2.lock().unwrap();
                if v.last() != Some(&p) {
                    v.push(p);
                }
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });
    (seen, h)
}

fn in_order(seen: &[&str], want: &[&str]) -> bool {
    let mut it = seen.iter();
    want.iter().all(|w| it.any(|s| s == w))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeper_two_lanes_recover_after_router_restart() {
    let _s = scene();
    let m = MockSam::start(Script { session_delay: Duration::from_millis(50), ..Default::default() }).await;
    use_config(config_for(&m));
    transport::set_preference(Some(Kind::I2p));
    let mut timing = fast();
    timing.backoff = vec![Duration::from_millis(50), Duration::from_millis(4000), Duration::from_millis(6000)];
    let t = start(&m, None, timing);
    let mut rx = host::subscribe();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let ev2 = events.clone();
    let collector = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => ev2.lock().unwrap().push(ev),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            }
        }
    });
    let inst = host::activate(t.clone(), live(), false).unwrap();
    let (seen, rec) = phases_recorder(t.clone());
    ready(&t).await;
    assert_eq!(transport::state(), transport::TransportState::Active { kind: Kind::I2p });
    let first = m.sessions();
    assert_eq!(first.len(), 2);
    let t0 = web_time::Instant::now();
    assert!(m.lines().iter().filter(|l| l.starts_with("SESSION CREATE")).count() == 2, "created in parallel, once each");

    // The router goes away: the control sockets close and the keeper notices at once.
    until("the recorder sees ready", Duration::from_secs(1), || seen.lock().unwrap().last() == Some(&"ready")).await;
    let mark = seen.lock().unwrap().len();
    m.down();
    until("lost", Duration::from_secs(2), || !t.ready()).await;
    until("router unreachable", Duration::from_secs(2), || t.phase_name() == "router_unreachable").await;
    let reason = format!("Can't reach your I2P router at 127.0.0.1:{}.", m.port);
    assert_eq!(transport::egress(live(), Lane::Account, "nos.lol", 443), Egress::Refuse(ConnectError::Blocked(reason.clone())));
    assert_eq!(transport::egress(live(), Lane::Shared, &b32('z'), 80), Egress::Refuse(ConnectError::Blocked(reason.clone())));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let t_up = web_time::Instant::now();
    m.up().await;
    ready(&t).await;
    assert!(t_up.elapsed() < Duration::from_millis(2000), "a returning router is noticed before the backoff ends");
    let second = m.sessions();
    assert_eq!(second.len(), 2);
    assert!(second.iter().all(|n| !first.contains(n)), "fresh nicknames for both lanes");
    until("the recorder sees ready", Duration::from_secs(1), || seen.lock().unwrap().last() == Some(&"ready")).await;
    assert!(in_order(&seen.lock().unwrap(), &["ready", "lost", "router_unreachable", "creating_sessions", "ready"]), "{:?}", seen.lock().unwrap());
    assert!(!seen.lock().unwrap()[mark..].contains(&"probing"), "a retry keeps the waiting phase until the router answers: {:?}", seen.lock().unwrap());

    // Backoff reset on Ready: the next outage retries after the first step again.
    let since = seen.lock().unwrap().len();
    m.down();
    let t1 = web_time::Instant::now();
    until("probing again", Duration::from_secs(2), || seen.lock().unwrap()[since..].contains(&"router_unreachable")).await;
    assert!(t1.elapsed() < Duration::from_millis(1000), "the backoff restarted at its first step");
    m.up().await;
    ready(&t).await;

    // New addresses: both sessions replaced, the old ones closed.
    let before = m.sessions();
    t.new_identity().await;
    ready(&t).await;
    let after = m.sessions();
    assert_eq!(after.len(), 2);
    assert!(after.iter().all(|n| !before.contains(n)), "{before:?} -> {after:?}");

    tokio::time::sleep(Duration::from_millis(50)).await;
    collector.abort();
    let events = events.lock().unwrap().clone();
    let readies: Vec<bool> = events
        .iter()
        .filter_map(|e| match e {
            host::TransportEvent::Ready { instance, recovered, .. } if *instance == inst.id() => Some(*recovered),
            _ => None,
        })
        .collect();
    assert_eq!(readies.first(), Some(&false), "the first Ready is not a recovery");
    assert_eq!(readies.iter().filter(|r| **r).count(), 3, "two router restarts and one renewal recovered: {readies:?}");
    let losts = events.iter().filter(|e| matches!(e, host::TransportEvent::Lost { instance, .. } if *instance == inst.id())).count();
    assert_eq!(losts, 3);
    println!("[i2p-test] keeper cycle {:.2}s", t0.elapsed().as_secs_f64());
    rec.abort();
    drop(inst);
    host::deactivate(true).await;
    until("sessions closed", Duration::from_secs(2), || m.sessions().is_empty()).await;
    assert_client_only(&m.lines());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_swapped_out_account_drops_its_sessions() {
    let _s = scene();
    let m = MockSam::start(Script::default()).await;
    use_config(config_for(&m));
    transport::set_preference(Some(Kind::I2p));
    let t = start(&m, None, fast());
    let owner = live();
    host::activate(t.clone(), owner, false).unwrap();
    ready(&t).await;
    assert_eq!(m.sessions().len(), 2);

    crate::db::close_database();
    assert_ne!(live(), owner);
    until("sessions dropped", Duration::from_secs(2), || m.sessions().is_empty()).await;
    assert_eq!(t.phase_name(), "parked");
    assert_eq!(transport::egress(owner, Lane::Account, "nos.lol", 443), Egress::Refuse(ConnectError::Stale));
    let stale = Ticket { owner, epoch: transport::epoch(), lane: Lane::Account };
    assert_eq!(host::dial(stale, Dest::parse(&b32('a')).unwrap(), 80).await.err(), Some(ConnectError::Stale));

    // The next account chose I2P too: the parked instance serves nothing for it.
    transport::set_preference(Some(Kind::I2p));
    assert_eq!(transport::state(), transport::TransportState::RequiredButInactive { kind: Kind::I2p });
    assert!(matches!(transport::egress(live(), Lane::Account, "nos.lol", 443), Egress::Refuse(ConnectError::NotReady(Some(Kind::I2p)))));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(m.sessions().is_empty(), "a parked instance opens nothing");
    assert_client_only(&m.lines());
    host::deactivate(true).await;
}

// ── Streams ──────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_error_classification() {
    let _s = scene();
    transport::set_preference(Some(Kind::I2p));
    let java = b32('j');
    let java_b64 = b64("J");
    let mut script = Script::default();
    for (c, status) in [('c', "CANT_REACH_PEER"), ('p', "PEER_NOT_FOUND"), ('t', "TIMEOUT"), ('e', "I2P_ERROR"), ('k', "INVALID_KEY")] {
        script.peers.insert(b32(c), Peer::Status(status));
    }
    script.peers.insert(b32('g'), Peer::Garbled);
    script.peers.insert(b32('x'), Peer::Close);
    script.peers.insert(java.clone(), Peer::Status("INVALID_KEY"));
    script.names.insert(java.clone(), java_b64.clone());
    script.peers.insert(java_b64.clone(), Peer::Echo);
    let m = MockSam::start(script).await;
    let t = start(&m, None, fast());
    ready(&t).await;
    let nick = nick_of(&t, Lane::Account);

    let raw = |d: String| {
        let nick = nick.clone();
        let port = m.port;
        async move { sam::connect(port, None, &nick, &d, 80).await.err() }
    };
    assert_eq!(raw(b32('c')).await, Some(StreamFail::Unreachable("LeaseSet not found".into())));
    assert!(matches!(raw(b32('p')).await, Some(StreamFail::Unreachable(_))));
    assert_eq!(raw(b32('t')).await, Some(StreamFail::Timeout));
    assert!(matches!(raw(b32('e')).await, Some(StreamFail::SessionSuspect(_))));
    assert_eq!(raw(b32('k')).await, Some(StreamFail::BadKey));
    assert!(matches!(raw(b32('g')).await, Some(StreamFail::SessionSuspect(_))), "an over-long reply");
    assert!(matches!(raw(b32('x')).await, Some(StreamFail::SessionSuspect(_))), "EOF");
    assert_eq!(sam::connect(m.port, None, &sam::new_nick(), &b32('c'), 80).await.err(), Some(StreamFail::SessionGone));

    let dial = |c: String| {
        let t = t.clone();
        async move { t.dial(&Route::Native { host: c, port: 80 }, Lane::Account).await.err() }
    };
    assert_eq!(dial(b32('c')).await, Some(ConnectError::Unreachable("LeaseSet not found".into())));
    assert_eq!(dial(b32('t')).await, Some(ConnectError::Timeout));
    assert_eq!(dial(b32('k')).await, Some(ConnectError::Refused(Refusal::BadName)), "a b32 the router won't take, after the lookup retry");
    assert!(dial(java.clone()).await.is_none(), "a router that wants base64 gets it on the retry");
    assert!(m.lines().contains(&format!("NAMING LOOKUP NAME={java}")));
    assert!(m.connects().iter().any(|(_, d, p)| d == &java_b64 && *p == Some(80)));
    assert_eq!(ConnectError::Unreachable("LeaseSet not found".into()).text(), "This I2P address isn't online right now.");

    // The router dies under a dial in progress.
    m.down();
    assert_eq!(t.inner.open(&nick, &b32('c'), 80).await.err(), Some(ConnectError::RouterDown { port: m.port }));
    assert_eq!(ConnectError::RouterDown { port: 7656 }.text(), "Can't reach your I2P router at 127.0.0.1:7656.");
    until("not ready", Duration::from_secs(2), || !t.ready()).await;
    assert_eq!(dial(b32('c')).await, Some(ConnectError::NotReady(Some(Kind::I2p))), "no sessions, no dial");
    assert_client_only(&m.lines());
    t.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reply_read_consumes_no_stream_byte() {
    let _s = scene();
    let relay = b32('r');
    let mut script = Script::default();
    script.peers.insert(relay.clone(), Peer::Greet(b"FIRST-BYTES".to_vec()));
    let m = MockSam::start(script).await;
    let nick = sam::new_nick();
    let _ctl = sam::create_session(m.port, None, &nick).await.unwrap();
    let mut s = sam::connect(m.port, None, &nick, &relay, 80).await.unwrap();
    let mut got = [0u8; 11];
    tokio::time::timeout(Duration::from_secs(2), s.read_exact(&mut got)).await.unwrap().unwrap();
    assert_eq!(&got, b"FIRST-BYTES", "sent in the same write as the status line, every byte reaches the stream");
    s.write_all(b"ping").await.unwrap();
    let mut back = [0u8; 4];
    s.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"ping");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn naming_lookup_cache_and_errors() {
    let _s = scene();
    transport::set_preference(Some(Kind::I2p));
    let dest = b64("N");
    let mut script = Script::default();
    script.names.insert("relay.i2p".into(), dest.clone());
    script.peers.insert(dest.clone(), Peer::Echo);
    let m = MockSam::start(script).await;
    let t = start(&m, None, fast());
    ready(&t).await;
    let lookups = || m.lines().iter().filter(|l| *l == "NAMING LOOKUP NAME=relay.i2p").count();
    let route = |h: &str| Route::Native { host: h.into(), port: 80 };

    t.dial(&route("relay.i2p"), Lane::Shared).await.unwrap();
    t.dial(&route("relay.i2p"), Lane::Shared).await.unwrap();
    assert_eq!(lookups(), 1, "a hit within the hour asks nothing");
    t.inner.names.age_for_test(Duration::from_secs(3601));
    t.dial(&route("relay.i2p"), Lane::Shared).await.unwrap();
    assert_eq!(lookups(), 2, "an expired entry is looked up again");

    let miss = t.dial(&route("nobody.i2p"), Lane::Shared).await.err().unwrap();
    assert_eq!(miss, ConnectError::UnknownName, "i2pd's INVALID_KEY");
    m.script().name_miss = "KEY_NOT_FOUND";
    assert_eq!(t.dial(&route("other.i2p"), Lane::Shared).await.err(), Some(ConnectError::UnknownName), "Java's KEY_NOT_FOUND");
    assert_eq!(miss.text(), "This I2P name isn't in your router's address book. Use its .b32.i2p address.");
    assert_client_only(&m.lines());
    t.shutdown().await;
}

// ── Outproxies ───────────────────────────────────────────────────────────────

const CONNECT_HEAD: &str = "CONNECT nos.lol:443 HTTP/1.1\r\nHost: nos.lol:443\r\nUser-Agent: MYOB/6.66 (AN/ON)\r\n\r\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outproxy_connect_handshake() {
    let _s = scene();
    transport::set_preference(Some(Kind::I2p));
    let mut script = Script::default();
    let ok = b32('o');
    script.peers.insert(ok.clone(), Peer::Outproxy(b"HTTP/1.1 200 Connection established\r\n\r\nTUNNEL".to_vec()));
    let cases: Vec<(char, Peer)> = vec![
        ('f', Peer::Outproxy(b"HTTP/1.1 403 Forbidden\r\n\r\n".to_vec())),
        ('m', Peer::Outproxy(b"HTTP/1.0 429 Too Many Requests\r\n\r\n".to_vec())),
        ('u', Peer::Outproxy(b"HTTP/1.1 503 Service Unavailable\r\n\r\n".to_vec())),
        ('b', Peer::Outproxy(b"HTTP/1.1 502 Bad Gateway\r\n\r\n".to_vec())),
        ('l', Peer::Outproxy([&b"HTTP/1.1 200 OK\r\nX-Pad: "[..], &vec![b'a'; 9000], b"\r\n\r\n"].concat())),
        ('s', Peer::OutproxyMute),
        ('h', Peer::Outproxy(b"SSH-2.0-OpenSSH\r\n\r\n".to_vec())),
    ];
    for (c, p) in &cases {
        script.peers.insert(b32(*c), p.clone());
    }
    let m = MockSam::start(script).await;
    let t = start(&m, None, fast());
    ready(&t).await;
    let nick = nick_of(&t, Lane::Shared);
    let one = |c: char| list_of(&[("Test", &b32(c), 80)]);

    // 200: the tunnel opens and its first byte is still in the socket.
    let list = list_of(&[("Ok", &ok, 80)]);
    let (mut s, id) = outproxy::connect(&t.inner, &nick, &list, "nos.lol", 443, &|_| true).await.unwrap();
    assert_eq!(id, list[0].id);
    let mut first = [0u8; 6];
    s.read_exact(&mut first).await.unwrap();
    assert_eq!(&first, b"TUNNEL");
    assert_eq!(m.st.heads.lock().unwrap()[0], CONNECT_HEAD);
    assert_eq!(t.inner.outproxies.state_of(&list[0].id), "ok");

    // 403: this port is remembered as refused, so it is not asked again.
    let l = one('f');
    assert_eq!(outproxy::connect(&t.inner, &nick, &l, "nos.lol", 443, &|_| true).await.err(), Some(ConnectError::ExitRefusedPort(443)));
    let dials = m.connects().len();
    assert_eq!(outproxy::connect(&t.inner, &nick, &l, "nos.lol", 443, &|_| true).await.err(), Some(ConnectError::ExitRefusedPort(443)));
    assert_eq!(m.connects().len(), dials, "a refused port is not tried again");
    assert_eq!(t.inner.outproxies.view(&l)[&l[0].id]["refused_ports"], serde_json::json!([443]));
    assert_eq!(t.inner.outproxies.state_of(&l[0].id), "unknown", "a port refusal is not a fault");
    assert_eq!(ConnectError::ExitRefusedPort(443).text(), "No outproxy here allows port 443.");

    // 429 and 503 cool the outproxy down.
    for c in ['m', 'u'] {
        let l = one(c);
        assert_eq!(outproxy::connect(&t.inner, &nick, &l, "nos.lol", 443, &|_| true).await.err(), Some(ConnectError::NoExit));
        assert_eq!(t.inner.outproxies.state_of(&l[0].id), "cooling");
        assert!(t.inner.outproxies.view(&l)[&l[0].id]["cooling_for"].as_u64().unwrap() <= 15);
    }

    // 502: the exit can't reach the target. Not the outproxy's fault; nothing else is tried.
    let l = list_of(&[("Bad gateway", &b32('b'), 80), ("Ok", &ok, 80)]);
    let e = outproxy::connect(&t.inner, &nick, &l, "nos.lol", 443, &|_| true).await.err().unwrap();
    assert_eq!(e, ConnectError::ExitUnreachable("Bad gateway".into()));
    assert_eq!(e.text(), "The Bad gateway outproxy couldn't reach this server.", "names the exit, not an I2P address");
    assert_eq!(e.socks_reply(), 0x04);
    assert_eq!(t.inner.outproxies.state_of(&l[0].id), "unknown");
    assert_eq!(m.st.heads.lock().unwrap().len(), 5, "the second outproxy was never asked");

    // An oversized head, a silent outproxy and a non-HTTP answer are faults.
    for c in ['l', 's', 'h'] {
        let l = one(c);
        let t0 = web_time::Instant::now();
        assert_eq!(outproxy::connect(&t.inner, &nick, &l, "nos.lol", 443, &|_| true).await.err(), Some(ConnectError::NoExit), "{c}");
        assert_eq!(t.inner.outproxies.state_of(&l[0].id), "cooling", "{c}");
        assert!(t0.elapsed() < Duration::from_secs(3));
    }
    assert_eq!(ConnectError::NoExit.text(), "No outproxy is reachable right now.");
    assert_client_only(&m.lines());
    t.shutdown().await;
}

/// Our own router going away mid-CONNECT is not the outproxy's fault: its health stays as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn router_loss_mid_connect_spares_the_outproxy() {
    let _s = scene();
    transport::set_preference(Some(Kind::I2p));
    let mute = b32('s');
    let mut script = Script::default();
    script.peers.insert(mute.clone(), Peer::OutproxyMute);
    let m = MockSam::start(script).await;
    let mut timing = fast();
    timing.head_deadline = Duration::from_secs(10);
    let t = start(&m, None, timing);
    ready(&t).await;
    let nick = nick_of(&t, Lane::Shared);
    let list = list_of(&[("Mute", &mute, 80)]);
    let (t2, l2) = (t.clone(), list.clone());
    let dial = tokio::spawn(async move { outproxy::connect(&t2.inner, &nick, &l2, "nos.lol", 443, &|_| true).await.err() });
    until("the CONNECT head reached the outproxy", Duration::from_secs(5), || !m.st.heads.lock().unwrap().is_empty()).await;
    m.down();
    let e = tokio::time::timeout(Duration::from_secs(5), dial).await.expect("the dial ends with the router").unwrap();
    assert!(
        matches!(e, Some(ConnectError::RouterDown { .. }) | Some(ConnectError::NotReady(Some(Kind::I2p)))),
        "the router's own failure, not the outproxy's: {e:?}"
    );
    assert_eq!(t.inner.outproxies.state_of(&list[0].id), "unknown", "the outproxy keeps its health");
    assert!(t.inner.outproxies.view(&list)[&list[0].id]["last_error"].is_null());
    assert!(!t.inner.outproxies.all_failing(&list));
    t.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outproxy_failover_and_health() {
    let _s = scene();
    let _acct = test_account();
    transport::set_preference(Some(Kind::I2p));
    let (good, slow) = (b32('g'), b32('s'));
    let mut script = Script::default();
    script.peers.insert(good.clone(), Peer::Outproxy(b"HTTP/1.1 200 Connection established\r\n\r\n".to_vec()));
    script.peers.insert(
        slow.clone(),
        Peer::Slow(Duration::from_millis(600), Box::new(Peer::Outproxy(b"HTTP/1.1 200 Connection established\r\n\r\n".to_vec()))),
    );
    for c in ['f', 'h'] {
        script.peers.insert(b32(c), Peer::Outproxy(b"HTTP/1.1 403 Forbidden\r\n\r\n".to_vec()));
    }
    let m = MockSam::start(script).await;
    let t = start(&m, None, fast());
    ready(&t).await;
    let nick = nick_of(&t, Lane::Shared);
    let dials_to = |d: &str| m.connects().iter().filter(|(_, x, _)| x == d).count();

    // In order; a dead first one is skipped and cools down.
    let dead = b32('d');
    let l = list_of(&[("Dead", &dead, 80), ("Good", &good, 80)]);
    let (_s, id) = outproxy::connect(&t.inner, &nick, &l, "nos.lol", 443, &|_| true).await.unwrap();
    assert_eq!(id, l[1].id);
    assert_eq!(t.inner.outproxies.state_of(&l[0].id), "cooling");
    let (_s, _) = outproxy::connect(&t.inner, &nick, &l, "nos.lol", 443, &|_| true).await.unwrap();
    assert_eq!(dials_to(&dead), 1, "a cooling outproxy is skipped");
    assert_eq!(dials_to(&good), 2);

    // At most two per connect; all failing is NoExit.
    let three = list_of(&[("A", &b32('a'), 80), ("B", &b32('b'), 80), ("C", &b32('c'), 80)]);
    assert_eq!(outproxy::connect(&t.inner, &nick, &three, "nos.lol", 443, &|_| true).await.err(), Some(ConnectError::NoExit));
    assert_eq!((dials_to(&b32('a')), dials_to(&b32('b')), dials_to(&b32('c'))), (1, 1, 0));
    // Every one cooling: the one whose rest ends first.
    let cooled = list_of(&[("B", &b32('b'), 80), ("A", &b32('a'), 80)]);
    let plan = t.inner.outproxies.plan(&cooled, 443).unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].address, b32('a'), "A failed first, so it cools first");
    assert!(t.inner.outproxies.all_failing(&cooled));

    // Every one refusing the port.
    let refusing = list_of(&[("F", &b32('f'), 80), ("H", &b32('h'), 80)]);
    assert_eq!(outproxy::connect(&t.inner, &nick, &refusing, "example.com", 80, &|_| true).await.err(), Some(ConnectError::ExitRefusedPort(80)));
    assert!(t.inner.outproxies.plan(&refusing, 443).is_ok(), "only the refused port is remembered");

    // Through the bridge: I2P-Only cuts exits being dialed and exits already spliced, and a
    // save that fails keeps it on.
    let slow_list = list_of(&[("Slow", &slow, 80)]);
    use_config(I2pConfig { sam_port: m.port, outproxies: Some(slow_list.clone()), ..Default::default() });
    host::activate(t.clone(), live(), false).unwrap();
    let ticket = Ticket { owner: live(), epoch: transport::epoch(), lane: Lane::Shared };
    let (mut spliced, code) = socks_connect(&ticket, "nos.lol", 443).await;
    assert_eq!(code, 0x00);
    let dialing = tokio::spawn(async move { socks_connect(&ticket, "relay.example.com", 443).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let gen = transport::policy_gen();
    {
        let conn = crate::db::get_write_connection_guard_static().unwrap();
        conn.execute("DROP TABLE settings", []).unwrap();
    }
    assert_eq!(i2p_config::set_exit(ExitPolicy::Off).err().as_deref(), Some(i2p_config::UNSAVED));
    assert!(transport::policy_gen() > gen);
    assert!(closed(&mut spliced).await, "the spliced exit was cut");
    let (_, code) = dialing.await.unwrap();
    assert_ne!(code, 0x00, "the exit being dialed is never handed over");
    assert_eq!(i2p_config::current().exit, ExitPolicy::Off, "the tighter policy stays in force");
    assert_eq!(
        transport::egress(live(), Lane::Shared, "nos.lol", 443),
        Egress::Refuse(ConnectError::Refused(Refusal::ExitOff))
    );
    assert_eq!(ConnectError::Refused(Refusal::ExitOff).text(), "I2P-Only is on, so this server is off.");
    assert!(matches!(transport::egress(live(), Lane::Shared, &good, 80), Egress::Proxy(_)), "I2P itself stays");
    assert!(i2p_config::set_exit(ExitPolicy::Allow).is_err(), "loosening saves first, and the save fails");
    assert_eq!(i2p_config::current().exit, ExitPolicy::Off, "so nothing loosened");
    host::deactivate(true).await;
    assert_client_only(&m.lines());
}

/// Settings learns of every health change it shows, with no poll: a new state, or another
/// outproxy carrying the tunnels.
#[test]
fn outproxy_health_changes_reach_the_ui() {
    let _s = scene();
    let h = outproxy::Health::default();
    let mut rx = host::subscribe();
    let mut changed = || {
        let mut n = 0;
        while let Ok(ev) = rx.try_recv() {
            n += usize::from(matches!(ev, host::TransportEvent::Changed));
        }
        n
    };
    h.fault("a", "no answer", web_time::Instant::now());
    assert_eq!(changed(), 1, "not tried → resting");
    h.fault("a", "no answer", web_time::Instant::now());
    assert_eq!(changed(), 0, "still resting: nothing new to show");
    h.ok("a");
    assert_eq!(changed(), 1);
    h.ok("a");
    assert_eq!(changed(), 0, "another tunnel through the same one shows nothing new");
    h.ok("b");
    assert_eq!(changed(), 1, "the Clearnet step names another outproxy");
    h.refused_port("b", 80, "refused port 80 (403)");
    assert_eq!(changed(), 0, "a refused port keeps it OK for every other port");
}

/// A burst of dials that fail together (a router restart, a cold netDb) is one failure, not one
/// per dial; a failure a later success overtook says nothing; a LeaseSet still unknown to a
/// fresh session is the router's, never the outproxy's.
#[test]
fn outproxy_failures_count_once_per_episode() {
    let _s = scene();
    let h = outproxy::Health::default();
    let cooling = |h: &outproxy::Health| h.view(&[Outproxy::custom("A", &b32('a'), 80, true)]).as_object().unwrap().values().next().unwrap()["cooling_for"].as_u64();
    let a = Outproxy::custom("A", &b32('a'), 80, true).id;
    let burst = web_time::Instant::now();
    for _ in 0..8 {
        h.fault(&a, "unreachable: LeaseSet not found", burst);
    }
    assert!(cooling(&h).is_some_and(|s| s <= 15), "eight concurrent failures rest it once: {:?}", cooling(&h));
    std::thread::sleep(std::time::Duration::from_millis(5));
    h.fault(&a, "no answer", web_time::Instant::now());
    assert!(cooling(&h).is_some_and(|s| s > 15 && s <= 30), "a failure after it is the next step: {:?}", cooling(&h));

    let before_ok = web_time::Instant::now();
    std::thread::sleep(std::time::Duration::from_millis(5));
    h.ok(&a);
    h.fault(&a, "no answer", before_ok);
    assert_eq!(h.state_of(&a), "ok", "a dial a success overtook leaves it OK");
    h.note(&a, "unreachable: LeaseSet not found");
    assert_eq!(h.state_of(&a), "ok", "a note never rests it");
    assert!(!h.all_failing(&[Outproxy::custom("A", &b32('a'), 80, true)]));
}

/// Right after the sessions are made the router may not know an outproxy's LeaseSet yet: shown,
/// never held against it. An outproxy removed while a connect runs is never dialed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_session_or_a_removal_costs_no_outproxy() {
    let _s = scene();
    transport::set_preference(Some(Kind::I2p));
    let good = b32('g');
    let mut script = Script::default();
    script.peers.insert(good.clone(), Peer::Outproxy(b"HTTP/1.1 200 Connection established\r\n\r\n".to_vec()));
    let m = MockSam::start(script).await;
    let mut timing = fast();
    timing.fresh_session = Duration::from_secs(60);
    let t = start(&m, None, timing);
    ready(&t).await;
    let nick = nick_of(&t, Lane::Shared);
    let dials_to = |d: &str| m.connects().iter().filter(|(_, x, _)| x == d).count();

    let dead = b32('d');
    let l = list_of(&[("Dead", &dead, 80), ("Good", &good, 80)]);
    let (_s, id) = outproxy::connect(&t.inner, &nick, &l, "nos.lol", 443, &|_| true).await.unwrap();
    assert_eq!(id, l[1].id);
    assert_eq!(t.inner.outproxies.state_of(&l[0].id), "unknown", "not rested for the router's cold lookup");
    assert!(t.inner.outproxies.view(&l)[&l[0].id]["last_error"].as_str().is_some_and(|e| e.contains("LeaseSet")), "but shown");

    let gone = l[1].id.clone();
    let heads = m.st.heads.lock().unwrap().len();
    let r = outproxy::connect(&t.inner, &nick, &l, "nos.lol", 443, &|id| id != gone).await;
    assert_eq!(r.err(), Some(ConnectError::NoExit));
    assert_eq!(dials_to(&good), 1, "the removed outproxy is never dialed");
    assert_eq!(m.st.heads.lock().unwrap().len(), heads, "and never sees a CONNECT");
    assert_client_only(&m.lines());
    t.shutdown().await;
}

/// Removing an outproxy cuts the exits still dialing too: they haven't said which outproxy they
/// are trying, and it may be the one just removed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_an_outproxy_cuts_exits_still_dialing() {
    let _s = scene();
    let _acct = test_account();
    transport::set_preference(Some(Kind::I2p));
    let (slow, other) = (b32('s'), b32('o'));
    let mut script = Script::default();
    script.peers.insert(
        slow.clone(),
        Peer::Slow(Duration::from_millis(800), Box::new(Peer::Outproxy(b"HTTP/1.1 200 Connection established\r\n\r\n".to_vec()))),
    );
    script.peers.insert(other.clone(), Peer::Outproxy(b"HTTP/1.1 200 Connection established\r\n\r\n".to_vec()));
    let m = MockSam::start(script).await;
    let t = start(&m, None, fast());
    ready(&t).await;
    let row = |name: &str, address: &str| OutproxyInput { name: name.into(), address: address.into(), port: 80, enabled: true };
    use_config(I2pConfig { sam_port: m.port, ..Default::default() });
    i2p_config::set_outproxies(Some(vec![row("Slow", &slow), row("Other", &other)])).unwrap();
    host::activate(t.clone(), live(), false).unwrap();
    let ticket = Ticket { owner: live(), epoch: transport::epoch(), lane: Lane::Shared };
    let dialing = tokio::spawn(async move { socks_connect(&ticket, "relay.example.com", 443).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    i2p_config::set_outproxies(Some(vec![row("Other", &other)])).unwrap();
    let (_, code) = dialing.await.unwrap();
    assert_ne!(code, 0x00, "an exit dialing when its outproxy went is never handed over");
    assert!(m.st.heads.lock().unwrap().iter().all(|h| !h.contains("relay.example.com")), "and the removed outproxy got no CONNECT");
    host::deactivate(true).await;
    assert_client_only(&m.lines());
}

#[test]
fn outproxy_list_validation() {
    let _s = scene();
    let row = |name: &str, address: &str, port: i64, enabled: bool| OutproxyInput { name: name.into(), address: address.into(), port, enabled };
    let stormy = i2p_config::defaults()[0].clone();
    let custom = b32('c');

    assert_eq!(i2p_config::outproxies_from(&[row("X", &custom, 0, true)]).err().unwrap(), "Enter a port from 1 to 65535.");
    assert_eq!(i2p_config::outproxies_from(&[row("X", &custom, 65536, true)]).err().unwrap(), "Enter a port from 1 to 65535.");
    assert_eq!(i2p_config::outproxies_from(&[row("X", "example.com", 80, true)]).err().unwrap(), "Enter a .b32.i2p or .i2p address.");
    assert!(i2p_config::outproxies_from(&[row("X", "exit.stormycloud.i2p", 80, true)]).is_ok(), "an address-book name");

    let rows = i2p_config::outproxies_from(&[row("Renamed", &stormy.address, 80, false), row(" Mine ", &custom.to_uppercase(), 4444, true)]).unwrap();
    assert_eq!((rows[0].id.as_str(), rows[0].name.as_str(), rows[0].builtin, rows[0].enabled), ("stormycloud", "StormyCloud", true, false));
    assert!(rows[1].id.starts_with("custom-") && rows[1].id.len() == "custom-".len() + 8);
    assert_eq!((rows[1].name.as_str(), rows[1].address.as_str(), rows[1].builtin), ("Mine", custom.as_str(), false));

    let check = |list: Vec<Outproxy>, exit: ExitPolicy| i2p_config::validate_outproxy_list(&list, exit).err();
    let named = |n: &str| Outproxy::custom(n, &custom, 80, true);
    assert_eq!(check(vec![named("  ")], ExitPolicy::Allow).as_deref(), Some("Enter a name up to 32 characters."));
    assert_eq!(check(vec![named(&"n".repeat(33))], ExitPolicy::Allow).as_deref(), Some("Enter a name up to 32 characters."));
    assert!(check(vec![named(&"n".repeat(32))], ExitPolicy::Allow).is_none());
    let nine: Vec<Outproxy> = (0..9).map(|i| Outproxy::custom("N", &custom, 1000 + i, true)).collect();
    assert_eq!(check(nine, ExitPolicy::Allow).as_deref(), Some("You can add up to 8 outproxies."));
    assert_eq!(check(vec![named("A"), named("B")], ExitPolicy::Allow).as_deref(), Some("This outproxy is already in the list."));
    let off = Outproxy { enabled: false, ..named("A") };
    assert_eq!(check(vec![off.clone()], ExitPolicy::Allow).as_deref(), Some("Keep one outproxy on, or turn on I2P-Only."));
    assert!(check(vec![off], ExitPolicy::Off).is_none(), "I2P-Only needs none");
    assert!(check(Vec::new(), ExitPolicy::Off).is_none());

    assert_eq!(i2p_config::validate_sam_auth(Some("u"), None).err().as_deref(), Some("Enter both a username and a password."));
    for bad in ["has space", "quo\"te", "eq=ual", &"x".repeat(65)] {
        assert_eq!(
            i2p_config::validate_sam_auth(Some(bad), Some("p")).err().as_deref(),
            Some("Use letters, numbers and symbols, without spaces or quotes."),
            "{bad}"
        );
    }
    assert_eq!(aliases::validate_twin(Kind::I2p, "relay.example.com").err().as_deref(), Some("Enter a .b32.i2p address."));
    assert_eq!(transport::validate_relay_url("ws://relay.example.com").err().as_deref(), Some("Use wss://, or ws:// for an .i2p or .onion address."));
    assert_eq!(transport::validate_relay_url("https://relay.example.com").err().as_deref(), Some("Use wss://, or ws:// for an .i2p or .onion address."));
    assert_eq!(transport::validate_relay_url(&format!(" ws://{}/ ", b32('r'))).unwrap(), format!("ws://{}", b32('r')));
    assert_eq!(transport::validate_relay_url(&format!("ws://{}:6878", b32('r'))).unwrap(), format!("ws://{}:6878", b32('r')));
    assert!(transport::validate_relay_url("ws://relay.i2p").is_ok());
    assert!(transport::validate_relay_url("ws://i2p").is_err());
    assert!(transport::validate_relay_url("ws://evil.i2p.example.com").is_err(), "the host must end in .i2p");
    assert_eq!(transport::validate_relay_url("wss://nos.lol/").unwrap(), "wss://nos.lol");
    assert_eq!(transport::validate_relay_url("wss://").err().as_deref(), Some("Relay URL must include a host"));

    // Saving: a custom list, then null back to the built-ins.
    let _acct = test_account();
    let v = i2p_config::set_outproxies(Some(vec![row("Mine", &custom, 4444, true)])).unwrap();
    assert!(v.customized);
    assert_eq!(v.outproxies.len(), 1);
    assert_eq!(prefs::read_stored(&crate::db::get_db_connection_guard_static().unwrap()).configs[&Kind::I2p]
        .downcast_ref::<I2pConfig>()
        .unwrap()
        .outproxies
        .as_ref()
        .map(Vec::len), Some(1), "saved");
    let v = i2p_config::set_outproxies(None).unwrap();
    assert!(!v.customized);
    assert_eq!(v.outproxies, i2p_config::defaults());
    assert_eq!(v.defaults, i2p_config::defaults());
    // Adding one and removing it again saves the built-in list as itself, not a frozen copy.
    let builtin_rows: Vec<OutproxyInput> = i2p_config::defaults().iter().map(|o| row(&o.name, &o.address, o.port.into(), true)).collect();
    let v = i2p_config::set_outproxies(Some(builtin_rows.clone())).unwrap();
    assert!(!v.customized);
    assert_eq!(i2p_config::current().outproxies, None, "stored as the defaults");
    let mut reordered = builtin_rows.clone();
    reordered.reverse();
    assert!(i2p_config::set_outproxies(Some(reordered)).unwrap().customized, "a new order is the user's");
    assert!(i2p_config::current().outproxies.is_some());
    let mut one_off = builtin_rows;
    one_off[1].enabled = false;
    assert!(i2p_config::set_outproxies(Some(one_off)).unwrap().customized, "so is one turned off");
    i2p_config::set_outproxies(None).unwrap();

    // The router: credentials kept, cleared, replaced; the password never comes back.
    let v = i2p_config::set_router(7657, Some(Some(("me".into(), "secret".into())))).unwrap();
    assert_eq!((v.sam_port, v.sam_user.as_deref(), v.sam_auth), (7657, Some("me"), true));
    assert!(!serde_json::to_string(&v).unwrap().contains("secret"));
    let v = i2p_config::set_router(7656, None).unwrap();
    assert_eq!((v.sam_port, v.sam_auth), (7656, true), "kept");
    let v = i2p_config::set_router(7656, Some(None)).unwrap();
    assert!(!v.sam_auth && v.sam_user.is_none());
    assert_eq!(i2p_config::set_router(0, None).err().as_deref(), Some("Enter a port from 1 to 65535."));
}

// ── Twins ────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn twin_rides_tls_untouched_and_checks_by_certificate() {
    let _s = scene();
    let _acct = test_account();
    transport::set_preference(Some(Kind::I2p));
    let twin = b32('w');
    let mut script = Script::default();
    script.peers.insert(twin.clone(), Peer::Echo);
    let m = MockSam::start(script).await;
    use_config(config_for(&m));
    let t = start(&m, None, fast());
    host::activate(t.clone(), live(), false).unwrap();
    ready(&t).await;
    let (acct_nick, shared_nick) = (nick_of(&t, Lane::Account), nick_of(&t, Lane::Shared));

    aliases::set("wss://relay.example.com", Kind::I2p, Some(&twin), AliasSource::User).unwrap();
    assert_eq!(aliases::table().get("relay.example.com").unwrap().check.state, CheckState::Unchecked);
    assert_eq!(transport::route_view("wss://relay.example.com").class, "twin", "an unchecked twin is used on 443");
    assert_eq!(transport::route_view("ws://relay.example.com").class, "exit", "a twin serves 443 only");

    // A ClientHello rides the twin byte for byte, to port 443 of the twin.
    let ticket = Ticket { owner: live(), epoch: transport::epoch(), lane: Lane::Account };
    let (mut c, code) = socks_connect(&ticket, "relay.example.com", 443).await;
    assert_eq!(code, 0x00);
    let hello: Vec<u8> = [&[0x16u8, 0x03, 0x01, 0x00, 0x05][..], b"hello"].concat();
    c.write_all(&hello).await.unwrap();
    let mut back = vec![0u8; hello.len()];
    c.read_exact(&mut back).await.unwrap();
    assert_eq!(back, hello);
    assert!(m.connects().contains(&(acct_nick, twin.clone(), Some(443))));

    // The check: an echo is not relay.example.com's TLS, so the twin is never used again and
    // what rode it is cut.
    let e = probe::check_alias("relay.example.com").await.unwrap();
    assert_eq!(e.check.state, CheckState::Failed);
    assert_eq!(e.check.text.as_deref(), Some("This address doesn't serve relay.example.com. Vector won't use it."));
    assert!(e.check.at.is_some());
    assert!(closed(&mut c).await, "a twin that failed its check is cut");
    assert_eq!(transport::route_view("wss://relay.example.com").class, "exit");
    assert!(m.connects().contains(&(shared_nick, twin.clone(), Some(443))), "checked on the Shared lane");

    // An address entered while it can't be checked is checked when I2P next comes up.
    aliases::set("relay2.example.com", Kind::I2p, Some(&twin), AliasSource::User).unwrap();
    t.new_identity().await;
    ready(&t).await;
    until("check on Ready", Duration::from_secs(5), || {
        aliases::table().get("relay2.example.com").is_some_and(|e| e.check.state == CheckState::Failed)
    })
    .await;

    assert_eq!(
        probe::probe_twin(&t, "relay.example.com", &twin).await.err().as_deref(),
        Some("This address doesn't serve relay.example.com. Vector won't use it.")
    );
    let unreachable = probe::probe_twin(&t, "relay.example.com", &b32('q')).await.err().unwrap();
    assert_eq!(unreachable, "This I2P address isn't online right now.", "a twin that can't be reached is not judged");
    host::deactivate(true).await;
    assert_eq!(probe::check_alias("relay.example.com").await.err().as_deref(), Some("Connect to I2P to check this address."));
    assert_client_only(&m.lines());
}

#[test]
fn privacy_addresses_parse() {
    let doc = |v: serde_json::Value| serde_json::json!({ "name": "r", "privacy_addresses": v });
    let a = b32('a');
    assert_eq!(probe::first_b32(&doc(serde_json::json!(["abc.onion", format!("ws://{a}/"), "x"]))), Some(a.clone()));
    assert_eq!(probe::first_b32(&doc(serde_json::json!([format!("{a}:443")]))), Some(a.clone()));
    assert_eq!(probe::first_b32(&doc(serde_json::json!([a.to_uppercase()]))), Some(a.clone()));
    assert_eq!(probe::first_b32(&doc(serde_json::json!(["relay.i2p", "abc.onion"]))), None, "names need the router's address book");
    assert_eq!(probe::first_b32(&serde_json::json!({ "name": "r" })), None);
    assert_eq!(probe::first_b32(&doc(serde_json::json!("not a list"))), None);
}

// ── Fail closed ──────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn router_down_fails_closed_with_its_reason() {
    use nostr_sdk::transport::websocket::WebSocketTransport;
    let _s = scene();
    let port = dead_port().await;
    use_config(I2pConfig { sam_port: port, ..Default::default() });
    transport::set_preference(Some(Kind::I2p));
    let cfg = I2pConfig { sam_port: port, ..Default::default() };
    let t = Arc::new(I2pTransport::new(&cfg, live(), fast()));
    t.spawn_keeper();
    host::activate(t.clone(), live(), false).unwrap();
    until("router unreachable", Duration::from_secs(3), || t.phase_name() == "router_unreachable").await;
    let reason = format!("Can't reach your I2P router at 127.0.0.1:{port}.");

    for host in ["nos.lol", &b32('a'), "1.2.3.4", "localhost"] {
        assert_eq!(transport::egress(live(), Lane::Account, host, 443), Egress::Refuse(ConnectError::Blocked(reason.clone())), "{host}");
    }
    assert!(matches!(transport::egress(live(), Lane::Account, "abc.onion", 443), Egress::Refuse(ConnectError::Refused(_))));

    let t0 = web_time::Instant::now();
    let e = crate::net::build_http_client(Duration::from_secs(10)).unwrap().get("https://nos.lol/").send().await.err().unwrap();
    assert_eq!(e.to_string(), reason);
    assert!(e.is_transient(), "the router coming back is worth waiting for");
    let ws = crate::transport::ws::VectorWs::new(live(), Lane::Account);
    let e = ws.connect(&url::Url::parse("wss://relay.damus.io").unwrap(), None).await.err().unwrap();
    assert!(e.to_string().contains(&reason), "{e}");
    assert!(t0.elapsed() < Duration::from_secs(1), "refused in process, nothing dialed");
    let ticket = Ticket { owner: live(), epoch: transport::epoch(), lane: Lane::Account };
    let (_c, code) = socks_connect(&ticket, "nos.lol", 443).await;
    assert_eq!(code, 0x01, "the bridge refuses too");

    let view = transport::status::view();
    assert_eq!(view.kind, "i2p");
    assert!(view.blocked && !view.ready);
    assert_eq!(view.reason.as_ref().map(|r| r.code.as_str()), Some("router_unreachable"));
    assert_eq!(view.reason.as_ref().map(|r| r.text.as_str()), Some(reason.as_str()));
    assert!(view.retry_in.is_some());
    assert_eq!(view.steps.iter().map(|s| (s.id.as_str(), s.state.as_str())).collect::<Vec<_>>(), [("router", "fail"), ("tunnels", "pending"), ("exit", "pending")]);
    // An outproxy that worked before the router went says nothing about now.
    t.inner.outproxies.ok("stormycloud");
    let exit = transport::status::view().steps[2].clone();
    assert_eq!((exit.state.as_str(), exit.text.as_str()), ("pending", "Waiting…"));
    let rv = transport::route_view("wss://nos.lol");
    assert_eq!((rv.class.as_str(), rv.text.clone()), ("blocked", format!("Blocked: {reason}")));
    host::deactivate(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_view_and_factory() {
    let _s = scene();
    let m = MockSam::start(Script::default()).await;
    use_config(I2pConfig { sam_port: m.port, ..Default::default() });
    transport::set_preference(Some(Kind::I2p));
    let factory = crate::transport::kinds::factory(Kind::I2p).expect("compiled in");
    assert!(factory.adopt_on_unlock());
    let ctx = StartCtx { owner: live(), started_prelogin: false, dirs: None, config: prefs::config(Kind::I2p) };
    let inst = factory.start(ctx).await.unwrap();
    assert!(!inst.ready(), "returns at once; Ready comes from the keeper");
    let a = host::activate(inst.clone(), live(), false).unwrap();
    until("ready", Duration::from_secs(20), || inst.ready()).await;
    assert_eq!(transport::state(), transport::TransportState::Active { kind: Kind::I2p });

    let same: KindConfig = Arc::new(I2pConfig { sam_port: m.port, ..Default::default() });
    let other_port: KindConfig = Arc::new(I2pConfig { sam_port: m.port.wrapping_add(1), ..Default::default() });
    let other_auth: KindConfig = Arc::new(I2pConfig { sam_port: m.port, sam_user: Some("u".into()), sam_password: Some("p".into()), ..Default::default() });
    assert!(factory.compatible(inst.as_ref(), &same));
    assert!(!factory.compatible(inst.as_ref(), &other_port), "another router means a restart");
    assert!(!factory.compatible(inst.as_ref(), &other_auth));

    let view = transport::status::view();
    assert_eq!((view.kind.as_str(), view.label, view.phase), ("i2p", Some("I2P"), crate::transport::status::Phase::Ready));
    assert!(view.supported.contains(&Kind::I2p));
    assert_eq!(view.steps[0].text, format!("SAM 3.3 on port {}", m.port));
    assert_eq!((view.steps[1].state.as_str(), view.steps[1].text.as_str()), ("ok", "Ready"));
    assert_eq!((view.steps[2].label.as_str(), view.steps[2].text.as_str()), ("Clearnet", "Not used yet"));
    assert_eq!(view.detail["sam_version"], "3.3");
    assert_eq!(view.detail["sam_auth"], false);
    assert_eq!(view.detail["exit"], "allow");
    assert_eq!(view.detail["outproxies"]["stormycloud"]["state"], "unknown");
    use_config(I2pConfig { sam_port: m.port, exit: ExitPolicy::Off, ..Default::default() });
    assert_eq!(transport::status::view().steps[2].text, "Off (I2P-Only)");
    assert_eq!(super::active().map(|t| t.sam_port()), Some(m.port));
    drop(a);
    host::deactivate(true).await;
    until("sessions closed", Duration::from_secs(2), || m.sessions().is_empty()).await;
}

#[test]
fn a_destination_maps_to_its_b32() {
    // Computed independently: base32(sha256(raw destination)), lowercase, unpadded.
    let dest = super::mock_sam::own_destination("vector");
    assert_eq!(sam::b32_of(&dest).as_deref(), Some("oqhjwam3hwgvqza5jwua2gty3jgzamdtjjefrwospmgvgkvtdvcq.b32.i2p"));
    let both = super::mock_sam::own_destination("lane0");
    assert!(both.contains('-') && both.contains('~'), "covers both of I2P's base64 symbols");
    assert_eq!(sam::b32_of(&both).as_deref(), Some("nkvnwksiwdbl2teco2z6hyzjjtikg67qc7l7xa5iq3i2hmdk4gdq.b32.i2p"));
    assert!(crate::transport::route::is_b32(&sam::b32_of(&dest).unwrap()));
    assert_eq!(sam::b32_of("AAAA"), None, "too short for a destination");
    assert_eq!(sam::b32_of("not base64!"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_lane_reports_the_address_it_is_seen_as() {
    let _s = scene();
    let m = MockSam::start(Script::default()).await;
    let t = start(&m, None, fast());
    ready(&t).await;
    let addresses = t.kind_status().detail["addresses"].clone();
    let seen = |lane| sam::b32_of(&super::mock_sam::own_destination(&nick_of(&t, lane)));
    assert_eq!(addresses["account"].as_str(), seen(Lane::Account).as_deref());
    assert_eq!(addresses["shared"].as_str(), seen(Lane::Shared).as_deref());
    assert_ne!(addresses["account"], addresses["shared"], "the lanes are separate identities");
    assert_client_only(&m.lines());
    t.shutdown().await;
    assert!(t.kind_status().detail["addresses"].is_null(), "no address once the sessions are gone");
}
