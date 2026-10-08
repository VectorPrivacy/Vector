//! The meta-glue's guarantees: nothing goes direct unless the account chose Clearnet, the bridge
//! speaks only to its own tickets, and every switch closes what the old network opened.
#![cfg(test)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;

// ── Harness ──────────────────────────────────────────────────────────────────

/// Process-global state (the host slot, the epoch, the live session's preference) is shared by
/// every test in the binary, so each test here holds the database guard for its whole body.
pub(crate) fn test_lock() -> MutexGuard<'static, ()> {
    crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner())
}

/// A clean, live, Clearnet session with no instance installed; restored on drop.
struct Scene {
    _guard: MutexGuard<'static, ()>,
}

fn reset() {
    if let Some(a) = host::uninstall() {
        drop(a);
    }
    set_strict_for_test(false);
    set_preference(Some(Kind::Clearnet));
    prelogin::disarm();
    realtime::allow_always(false);
    prefs::live().set_config_value(Kind::I2p, Arc::new(i2p_config::I2pConfig::default()));
    prefs::live().set_aliases(Arc::new(aliases::AliasTable::default()));
    budget::clear_overrides_for_test();
}

fn scene() -> Scene {
    let guard = test_lock();
    reset();
    Scene { _guard: guard }
}

impl Drop for Scene {
    fn drop(&mut self) {
        reset();
    }
}

#[derive(Clone)]
enum MockDial {
    Refuse(ConnectError),
    To(SocketAddr),
    After(Duration, SocketAddr),
    Hang,
}

struct Mock {
    kind: Kind,
    ready: AtomicBool,
    dial: Mutex<MockDial>,
    dials: Mutex<Vec<(Route, Lane)>>,
    identities: AtomicUsize,
    shutdowns: AtomicUsize,
    /// Why it isn't ready, while it isn't.
    reason: Mutex<status::Reason>,
}

impl Mock {
    fn new(kind: Kind, ready: bool, dial: MockDial) -> Arc<Self> {
        Arc::new(Mock {
            kind,
            ready: AtomicBool::new(ready),
            dial: Mutex::new(dial),
            dials: Mutex::new(Vec::new()),
            identities: AtomicUsize::new(0),
            shutdowns: AtomicUsize::new(0),
            reason: Mutex::new(status::Reason::new("router_unreachable", "Can't reach your I2P router at 127.0.0.1:7656.")),
        })
    }

    fn dialed(&self) -> Vec<(Route, Lane)> {
        self.dials.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Transport for Mock {
    fn kind(&self) -> Kind {
        self.kind
    }

    fn route(&self, dest: &Dest, port: u16, ctx: &RouteCtx) -> Route {
        match self.kind {
            Kind::I2p => route::route_i2p(dest, port, ctx),
            _ => route::route_tor(dest, port, ctx),
        }
    }

    async fn dial(&self, route: &Route, lane: Lane) -> Result<(BoxedStream, Dialed), ConnectError> {
        self.dials.lock().unwrap().push((route.clone(), lane));
        let how = self.dial.lock().unwrap().clone();
        let outproxy = matches!(route, Route::Exit { .. }).then(|| "stormycloud".to_string());
        match how {
            MockDial::Refuse(e) => Err(e),
            MockDial::To(addr) => {
                let s = TcpStream::connect(addr).await.map_err(|e| ConnectError::Unreachable(e.to_string()))?;
                Ok((Box::new(s), Dialed { outproxy }))
            }
            MockDial::After(d, addr) => {
                tokio::time::sleep(d).await;
                let s = TcpStream::connect(addr).await.map_err(|e| ConnectError::Unreachable(e.to_string()))?;
                Ok((Box::new(s), Dialed { outproxy }))
            }
            MockDial::Hang => std::future::pending().await,
        }
    }

    fn ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    fn kind_status(&self) -> status::KindStatus {
        if self.ready() {
            status::KindStatus::ready(serde_json::Value::Null)
        } else {
            status::KindStatus {
                phase: status::Phase::Waiting,
                progress: None,
                reason: Some(self.reason.lock().unwrap().clone()),
                retry_in: Some(5),
                steps: Vec::new(),
                detail: serde_json::Value::Null,
            }
        }
    }

    async fn new_identity(&self) {
        self.identities.fetch_add(1, Ordering::SeqCst);
    }

    async fn shutdown(&self) {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
        self
    }
}

fn live() -> u64 {
    crate::db::live_session_id()
}

const B32: &str = "nostrajmjieip3dqgeefsgpydy3bbshe3o32z65dwkssl7qxkn5a.b32.i2p";

/// A listener that records the first bytes of every connection and keeps it open.
async fn recorder() -> (SocketAddr, Arc<Mutex<Vec<Vec<u8>>>>, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let accepted = Arc::new(AtomicUsize::new(0));
    let (s2, a2) = (seen.clone(), accepted.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut c, _)) = l.accept().await else { return };
            a2.fetch_add(1, Ordering::SeqCst);
            let s3 = s2.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let mut got = Vec::new();
                while let Ok(Ok(n)) = tokio::time::timeout(Duration::from_millis(400), c.read(&mut buf)).await {
                    if n == 0 {
                        break;
                    }
                    got.extend_from_slice(&buf[..n]);
                }
                s3.lock().unwrap().push(got);
                let _ = c.write_all(b"pong").await;
                tokio::time::sleep(Duration::from_secs(30)).await;
            });
        }
    });
    (addr, seen, accepted)
}

async fn socks_hello(port_addr: SocketAddr, t: &Ticket) -> TcpStream {
    let mut c = TcpStream::connect(port_addr).await.unwrap();
    c.write_all(&[5, 1, 2]).await.unwrap();
    let mut r = [0u8; 2];
    c.read_exact(&mut r).await.unwrap();
    assert_eq!(r, [5, 2]);
    let (user, pass) = bridge::credentials(t);
    let mut auth = vec![1, user.len() as u8];
    auth.extend_from_slice(user.as_bytes());
    auth.push(pass.len() as u8);
    auth.extend_from_slice(pass.as_bytes());
    c.write_all(&auth).await.unwrap();
    let mut a = [0u8; 2];
    c.read_exact(&mut a).await.unwrap();
    assert_eq!(a, [1, 0], "the ticket and secret authenticate");
    c
}

fn connect_domain(host: &str, port: u16) -> Vec<u8> {
    let mut req = vec![5, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    req
}

async fn reply_code(c: &mut TcpStream) -> u8 {
    let mut r = [0u8; 10];
    match tokio::time::timeout(Duration::from_secs(10), c.read_exact(&mut r)).await {
        Ok(Ok(_)) => r[1],
        _ => 0xEE,
    }
}

// ── Routing ──────────────────────────────────────────────────────────────────

#[test]
fn route_rules_table() {
    let table = aliases::AliasTable::from_entries(vec![aliases::AliasEntry {
        host: "jskitty.com".into(),
        twins: [(Kind::I2p, B32.to_string())].into_iter().collect(),
        source: aliases::AliasSource::User,
        check: aliases::AliasCheck::default(),
    }, aliases::AliasEntry {
        host: "broken.example".into(),
        twins: [(Kind::I2p, B32.to_string())].into_iter().collect(),
        source: aliases::AliasSource::User,
        check: aliases::AliasCheck { state: aliases::CheckState::Failed, at: None, text: None },
    }]);
    let allow: KindConfig = Arc::new(i2p_config::I2pConfig::default());
    let off: KindConfig = Arc::new(i2p_config::I2pConfig { exit: ExitPolicy::Off, ..Default::default() });
    fn ctx<'a>(table: &'a aliases::AliasTable, c: &'a KindConfig) -> RouteCtx<'a> {
        RouteCtx { aliases: table, config: c }
    }
    let ctx_allow = RouteCtx { aliases: &table, config: &allow };
    let d = |h: &str| Dest::parse(h).unwrap();

    // Host classification.
    assert_eq!(d("Relay.Example.COM."), Dest::Domain("relay.example.com".into()));
    assert!(matches!(d("127.0.0.1"), Dest::Ip(_)));
    assert!(matches!(d("[::1]"), Dest::Ip(_)));
    assert!(Dest::parse("").is_none());
    assert!(route::is_b32(B32));
    assert!(route::is_b32(&format!("{}.b32.i2p", "a".repeat(60))), "b33 for encrypted LeaseSets");
    assert!(!route::is_b32(&format!("{}.b32.i2p", "a".repeat(53))));
    assert!(!route::is_b32("ABCD.b32.i2p"));
    for bad in ["evil.com\r\nHost: x", "a b.com", "a:b.com", "a/b.com", "a..com", &"a".repeat(64)] {
        assert!(!route::valid_hostname(bad), "{bad:?}");
    }

    // The pre-router, every mode.
    for chosen in Kind::ALL {
        assert_eq!(route::pre_route(chosen, &d("x.com\r\n")), Some(Refusal::BadName));
        assert_eq!(route::pre_route(chosen, &d("relay.example.com")), None);
        assert_eq!(route::pre_route(chosen, &d("1.2.3.4")), None);
        let i2p = route::pre_route(chosen, &d(B32));
        let onion = route::pre_route(chosen, &d("abc.onion"));
        assert_eq!(i2p, (chosen != Kind::I2p).then_some(Refusal::WrongNetwork { needs: Kind::I2p }), "{chosen:?}");
        assert_eq!(onion, Some(Refusal::NotReachable { network: Kind::Tor }), "no build reaches an onion yet, Tor included: {chosen:?}");
    }
    assert_eq!(Refusal::NotReachable { network: Kind::Tor }.text(), "Vector can't reach .onion addresses yet.");

    // Tor: its own suffix native (once a build can), everything else (IPs and local names too) to an exit.
    assert_eq!(route::route_tor(&d("abc.onion"), 80, &ctx_allow), Route::Native { host: "abc.onion".into(), port: 80 });
    assert_eq!(route::route_tor(&d("nos.lol"), 443, &ctx_allow), Route::Exit { host: "nos.lol".into(), port: 443 });
    assert_eq!(route::route_tor(&d("1.2.3.4"), 443, &ctx_allow), Route::Exit { host: "1.2.3.4".into(), port: 443 });
    assert_eq!(route::route_tor(&d("localhost"), 80, &ctx_allow), Route::Exit { host: "localhost".into(), port: 80 });
    assert!(matches!(route::route_tor(&d("jskitty.com"), 443, &ctx_allow), Route::Exit { .. }), "only an onion twin serves Tor");

    // I2P.
    assert_eq!(route::route_i2p(&d(B32), 80, &ctx_allow), Route::Native { host: B32.into(), port: 80 });
    assert_eq!(route::route_i2p(&d("stats.i2p"), 80, &ctx_allow), Route::Native { host: "stats.i2p".into(), port: 80 });
    assert_eq!(
        route::route_i2p(&d("jskitty.com"), 443, &ctx_allow),
        Route::Twin { host: "jskitty.com".into(), via: B32.into(), port: 443 }
    );
    assert_eq!(route::route_i2p(&d("jskitty.com"), 80, &ctx_allow), Route::Exit { host: "jskitty.com".into(), port: 80 }, "a twin serves 443 only");
    assert_eq!(route::route_i2p(&d("broken.example"), 443, &ctx_allow), Route::Exit { host: "broken.example".into(), port: 443 }, "never a failed twin");
    assert_eq!(route::route_i2p(&d("1.2.3.4"), 443, &ctx_allow), Route::Refuse(Refusal::IpLiteral));
    assert_eq!(route::route_i2p(&d("printer.local"), 80, &ctx_allow), Route::Refuse(Refusal::LocalName));
    assert_eq!(route::route_i2p(&d("router"), 80, &ctx_allow), Route::Refuse(Refusal::LocalName));
    assert_eq!(route::route_i2p(&d("nos.lol"), 443, &ctx(&table, &allow)), Route::Exit { host: "nos.lol".into(), port: 443 });
    assert_eq!(route::route_i2p(&d("nos.lol"), 443, &ctx(&table, &off)), Route::Refuse(Refusal::ExitOff));
    assert_eq!(route::route_i2p(&d(B32), 80, &ctx(&table, &off)), Route::Native { host: B32.into(), port: 80 }, "I2P-Only keeps I2P");
    assert!(matches!(route::route_i2p(&d("jskitty.com"), 443, &ctx(&table, &off)), Route::Twin { .. }), "and the twins you added");
}

// ── Egress ───────────────────────────────────────────────────────────────────

#[test]
fn egress_matrix_never_direct_unless_clearnet() {
    let _s = scene();
    let hosts = ["relay.example.com", B32, "abc.onion", "1.2.3.4", "localhost"];
    #[derive(Debug, Clone, Copy)]
    enum Inst {
        None,
        TorReady,
        I2pReady,
        I2pNotReady,
        StaleOwner,
    }
    let insts = [Inst::None, Inst::TorReady, Inst::I2pReady, Inst::I2pNotReady, Inst::StaleOwner];
    let prefs: [Option<Kind>; 4] = [None, Some(Kind::Clearnet), Some(Kind::Tor), Some(Kind::I2p)];
    for pref in prefs {
        for inst in insts {
            reset();
            set_preference(pref);
            match inst {
                Inst::None => {}
                Inst::TorReady => {
                    host::activate(Mock::new(Kind::Tor, true, MockDial::Hang), live(), false).unwrap();
                }
                Inst::I2pReady => {
                    host::activate(Mock::new(Kind::I2p, true, MockDial::Hang), live(), false).unwrap();
                }
                Inst::I2pNotReady => {
                    host::activate(Mock::new(Kind::I2p, false, MockDial::Hang), live(), false).unwrap();
                }
                Inst::StaleOwner => {
                    host::activate(Mock::new(pref.unwrap_or(Kind::Tor), true, MockDial::Hang), live() + 999, false).unwrap();
                }
            }
            for owner_live in [true, false] {
                let owner = if owner_live { live() } else { live() + 12345 };
                for h in hosts {
                    let e = egress(owner, Lane::Account, h, 443);
                    let ctx = format!("pref={pref:?} inst={inst:?} live={owner_live} host={h}");
                    let foreign = (h.ends_with(".i2p") && pref != Some(Kind::I2p)) || h.ends_with(".onion");
                    let direct_ok = pref == Some(Kind::Clearnet) && owner_live && !foreign;
                    if direct_ok {
                        assert_eq!(e, Egress::Direct, "{ctx}");
                    } else {
                        assert_ne!(e, Egress::Direct, "NEVER direct: {ctx}");
                    }
                    if !owner_live {
                        assert_eq!(e, Egress::Refuse(ConnectError::Stale), "{ctx}");
                    }
                    if owner_live && foreign {
                        assert!(
                            matches!(e, Egress::Refuse(ConnectError::Refused(Refusal::WrongNetwork { .. } | Refusal::NotReachable { .. }))),
                            "{ctx}: {e:?}"
                        );
                    }
                    let ready_match = matches!((pref, inst), (Some(Kind::Tor), Inst::TorReady) | (Some(Kind::I2p), Inst::I2pReady));
                    if owner_live && !foreign && pref != Some(Kind::Clearnet) && !ready_match {
                        assert!(matches!(e, Egress::Refuse(_)), "a chosen kind not ready is refused: {ctx}: {e:?}");
                    }
                    if owner_live && ready_match && !foreign {
                        let refused_by_i2p = pref == Some(Kind::I2p) && (h == "1.2.3.4" || h == "localhost");
                        if refused_by_i2p {
                            assert!(matches!(e, Egress::Refuse(ConnectError::Refused(_))), "{ctx}");
                        } else {
                            assert!(matches!(e, Egress::Proxy(t) if t.owner == owner && t.epoch == epoch()), "{ctx}: {e:?}");
                        }
                    }
                }
            }
        }
    }
    // A kind this build lacks stays chosen and blocked with its reason.
    reset();
    set_preference(Some(Kind::I2p));
    if !Kind::I2p.compiled() {
        assert_eq!(
            egress(live(), Lane::Shared, "nos.lol", 443),
            Egress::Refuse(ConnectError::Blocked("This build doesn't include I2P.".into()))
        );
    }
    // Unknown is blocked with its own text.
    set_preference(None);
    assert_eq!(egress(live(), Lane::Shared, "nos.lol", 443), Egress::Refuse(ConnectError::NotReady(None)));
    assert_eq!(ConnectError::NotReady(None).text(), "Vector is still connecting.");
}

#[test]
fn stale_owner_is_refused() {
    let _s = scene();
    let a = live();
    // Another session installed: whatever was built under `a` is refused, whatever the network.
    crate::db::close_database();
    for k in [Some(Kind::Clearnet), Some(Kind::Tor), None] {
        set_preference(k);
        assert_eq!(egress(a, Lane::Account, "relay.example.com", 443), Egress::Refuse(ConnectError::Stale), "{k:?}");
    }
    // A replaced session never comes back: no transfer waits for it.
    assert!(!ConnectError::Stale.is_transient());
    assert!(!crate::net::HttpError::Refused(ConnectError::Stale).is_transient());
    reset();

    // Ready and Lost from an instance that is not the active one, or of another owner, are dropped.
    set_preference(Some(Kind::I2p));
    let mock = Mock::new(Kind::I2p, false, MockDial::Hang);
    let inst = host::activate(mock.clone(), live(), false).unwrap();
    let mut rx = host::subscribe();
    let before = epoch();
    host::notify(host::TransportEvent::Ready { kind: Kind::I2p, instance: inst.id() + 77, owner: live(), recovered: false });
    host::notify(host::TransportEvent::Lost { kind: Kind::I2p, instance: inst.id(), owner: live() + 5 });
    assert_eq!(epoch(), before, "foreign events never move the epoch");
    assert!(matches!(rx.try_recv(), Err(tokio::sync::broadcast::error::TryRecvError::Empty)));
    mock.ready.store(true, Ordering::SeqCst);
    host::notify(host::TransportEvent::Ready { kind: Kind::I2p, instance: inst.id(), owner: live(), recovered: false });
    assert_eq!(epoch(), before + 1, "the active instance's Ready moves it");
    assert!(matches!(rx.try_recv(), Ok(host::TransportEvent::Ready { .. })));
    assert_eq!(state(), TransportState::Active { kind: Kind::I2p });
}

// ── Budgets ──────────────────────────────────────────────────────────────────

#[test]
fn budgets_table_and_floor_only_raises() {
    let _s = scene();
    let ops = [Op::RelayConnect, Op::RelayRequest, Op::HttpConnect, Op::HttpTotal, Op::HttpRead, Op::TransferStall, Op::ReadyWait, Op::Startup, Op::BestEffort];
    let short = Duration::from_secs(5);
    let long = Duration::from_secs(3600);
    // 0: no floor, the caller's own value (Tor's mirror and check budgets were always the caller's).
    let tor = [60, 30, 45, 90, 90, 120, 30, 120, 0];
    let i2p = [90, 60, 90, 120, 120, 180, 90, 180, 45];
    for (i, op) in ops.iter().enumerate() {
        set_preference(Some(Kind::Clearnet));
        assert_eq!(budget(*op, short), short, "Clearnet passes the caller's value: {op:?}");
        set_preference(Some(Kind::Tor));
        assert_eq!(budget(*op, short), short.max(Duration::from_secs(tor[i])), "Tor keeps today's floor: {op:?}");
        assert_eq!(budget(*op, Duration::ZERO), Duration::from_secs(tor[i]));
        set_preference(Some(Kind::I2p));
        assert_eq!(budget(*op, short), Duration::from_secs(i2p[i]), "{op:?}");
        set_preference(None);
        assert_eq!(budget(*op, short), short.max(budget::highest_compiled_floor(*op)), "Unknown waits like the slowest compiled kind: {op:?}");
        for k in [Some(Kind::Clearnet), Some(Kind::Tor), Some(Kind::I2p), None] {
            set_preference(k);
            assert_eq!(budget(*op, long), long, "a floor only ever raises: {op:?} {k:?}");
        }
    }
    // The compatibility shims read the same table.
    set_preference(Some(Kind::Tor));
    assert_eq!(crate::relay_connect_timeout(short), Duration::from_secs(60));
    assert_eq!(crate::relay_request_timeout(short), Duration::from_secs(30));
    assert_eq!(crate::net::tor_http_timeout(short), Duration::from_secs(90));
    assert_eq!(crate::net::transfer_stall(), Duration::from_secs(120));
    set_preference(Some(Kind::Clearnet));
    assert_eq!(crate::relay_connect_timeout(short), short);
    assert_eq!(crate::net::transfer_stall(), crate::net::TRANSFER_STALL);
}

// ── Settings ─────────────────────────────────────────────────────────────────

fn settings_db(rows: &[(&str, &str)]) -> rusqlite::Connection {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);").unwrap();
    for (k, v) in rows {
        c.execute("INSERT INTO settings (key, value) VALUES (?1, ?2)", [k, v]).unwrap();
    }
    c
}

#[test]
fn stored_prefs_resolve_strictest() {
    use prefs::StoredKind;
    let legacy = prefs::KEY_LEGACY_TOR;
    let cases: &[(&[(&str, &str)], StoredKind)] = &[
        (&[("transport", "tor")], StoredKind::Kind(Kind::Tor)),
        (&[("transport", "i2p"), (legacy, "0")], StoredKind::Kind(Kind::I2p)),
        (&[("transport", "tor"), (legacy, "0")], StoredKind::Kind(Kind::Tor)),
        (&[("transport", "clearnet"), (legacy, "1")], StoredKind::Kind(Kind::Tor)),
        (&[("transport", "clearnet"), (legacy, "true")], StoredKind::Kind(Kind::Tor)),
        (&[("transport", "clearnet"), (legacy, "0")], StoredKind::Kind(Kind::Clearnet)),
        (&[("transport", "clearnet")], StoredKind::Kind(Kind::Clearnet)),
        (&[(legacy, "1")], StoredKind::Kind(Kind::Tor)),
        (&[(legacy, "true")], StoredKind::Kind(Kind::Tor)),
        (&[(legacy, "0")], StoredKind::Kind(Kind::Clearnet)),
        (&[(legacy, "false")], StoredKind::Kind(Kind::Clearnet)),
        (&[], StoredKind::Absent),
        (&[("transport", "Tor")], StoredKind::Unparseable("Tor".into())),
        (&[("transport", "garbage"), (legacy, "1")], StoredKind::Unparseable("garbage".into())),
    ];
    for (rows, want) in cases {
        assert_eq!(&prefs::read_stored(&settings_db(rows)).kind, want, "{rows:?}");
    }
    assert_eq!(prelogin::effective(&StoredKind::Unparseable("x".into())), None, "garbage stays Unknown");

    // persist_kind writes both rows together; the projection keeps an older build fail closed.
    for (k, legacy_want) in [(Kind::Tor, "1"), (Kind::I2p, "1"), (Kind::Clearnet, "0")] {
        let mut c = settings_db(&[]);
        prefs::persist_kind_on(&mut c, k).unwrap();
        let get = |key: &str| c.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get::<_, String>(0)).unwrap();
        assert_eq!(get("transport"), k.as_str());
        assert_eq!(get(legacy), legacy_want);
        assert_eq!(prefs::read_stored(&c).kind, StoredKind::Kind(k), "round trip");
    }

    // A read error is its own outcome, not Clearnet.
    let broken = rusqlite::Connection::open_in_memory().unwrap();
    assert!(matches!(prefs::read_stored(&broken).kind, StoredKind::ReadError(_)));

    // I2P config: defaults when absent, I2P-Only when there but unreadable, exact round trip
    // otherwise.
    let stored = prefs::read_stored(&settings_db(&[]));
    let cfg = stored.configs[&Kind::I2p].downcast_ref::<i2p_config::I2pConfig>().unwrap().clone();
    assert_eq!(cfg, i2p_config::I2pConfig::default());
    assert_eq!(cfg.sam_port, 7656);
    assert_eq!(cfg.outproxy_list(), i2p_config::defaults());
    let broken = i2p_config::I2pConfig::parse(Some("{not json"));
    assert_eq!((broken.exit, broken.outproxy_list()), (ExitPolicy::Off, Vec::new()), "a corrupt row never opens the exits");
    let custom = i2p_config::I2pConfig {
        sam_port: 7777,
        sam_user: Some("u".into()),
        sam_password: Some("p".into()),
        exit: ExitPolicy::Off,
        outproxies: Some(vec![i2p_config::Outproxy::custom("Mine", B32, 80, true)]),
    };
    assert_eq!(i2p_config::I2pConfig::parse(Some(custom.to_json().as_str())), custom);
    let stored = prefs::read_stored(&settings_db(&[("transport_cfg_i2p", custom.to_json().as_str())]));
    assert_eq!(stored.configs[&Kind::I2p].downcast_ref::<i2p_config::I2pConfig>(), Some(&custom));
    assert!(stored.multi_circuit, "multi-circuit unless the account chose one shared circuit");
    assert!(!prefs::read_stored(&settings_db(&[("tor_multi_circuit", "0")])).multi_circuit);
}

#[test]
fn transport_keys_are_protected() {
    use crate::db::settings::{PROTECTED_SETTINGS, SECRET_SETTINGS};
    for k in ["transport", prefs::KEY_LEGACY_TOR, "transport_cfg_i2p", "transport_aliases"] {
        assert!(PROTECTED_SETTINGS.contains(&k), "{k} must be out of reach of the generic setters");
    }
    assert!(SECRET_SETTINGS.contains(&"transport_cfg_i2p"), "the I2P config can hold a SAM password");
    assert_eq!(prefs::PROTECTED_KEYS.len(), 4);
}

#[test]
fn tor_enabled_literal_only_in_prefs() {
    let needle = concat!("\"tor_", "enabled\"");
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let crates = manifest.parent().unwrap();
    let roots = [crates.to_path_buf(), crates.parent().unwrap().join("src-tauri").join("src")];
    let mut hits = Vec::new();
    for root in roots {
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                if p.is_dir() {
                    if !matches!(name, "target" | "vendor" | "node_modules" | ".git") {
                        stack.push(p);
                    }
                    continue;
                }
                if p.extension().and_then(|x| x.to_str()) != Some("rs") {
                    continue;
                }
                let Ok(src) = std::fs::read_to_string(&p) else { continue };
                let lines: Vec<&str> = src.lines().collect();
                let cut = lines
                    .windows(2)
                    .position(|w| w[0].trim() == "#[cfg(test)]" && w[1].trim_start().starts_with("mod "))
                    .unwrap_or(lines.len());
                if lines[..cut].iter().any(|l| l.contains(needle)) {
                    hits.push(p.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    hits.retain(|h| !h.ends_with("vector-core/src/transport/prefs.rs"));
    assert!(hits.is_empty(), "the legacy Tor row is read and written only by transport/prefs.rs: {hits:?}");
}

// ── Sessions and pre-login ───────────────────────────────────────────────────

fn test_account() -> String {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(44_000);
    let n = N.fetch_add(1, Ordering::Relaxed);
    const B: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    let mut acct = String::from("npub1");
    let mut v = n as usize;
    for _ in 0..58 {
        acct.push(B[v % 32] as char);
        v = v / 32 + 11;
    }
    crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
    let _ = std::fs::create_dir_all(crate::db::shared_test_data_dir().join(&acct));
    crate::db::set_current_account(acct.clone()).unwrap();
    acct
}

#[test]
fn session_prefs_shared_across_rebound() {
    let _s = scene();
    crate::db::close_database();
    let before = crate::db::live_session();
    let p = prefs::of(&before);
    set_preference(Some(Kind::Tor));
    prelogin::arm(prelogin::PreloginChoice::new(Kind::Tor));
    let acct = test_account();
    crate::db::init_database(&acct).unwrap();
    let after = crate::db::live_session();
    assert_eq!(before.id(), after.id(), "binding a filling session keeps its identity");
    assert!(Arc::ptr_eq(&p, &prefs::of(&after)), "and its one SessionPrefs");
    assert_eq!(preference(), Some(Kind::Tor), "an absent stored kind is raised by the armed choice");
    prelogin::disarm();

    // Hydration writes into the session it is handed, even from a task bound to another.
    let old = after.clone();
    crate::db::close_database();
    let next = crate::db::live_session();
    let path = old.db_path().unwrap();
    {
        let mut c = crate::db::connect_at(&path).unwrap();
        prefs::persist_kind_on(&mut c, Kind::I2p).unwrap();
    }
    prefs::set_kind(&old, Some(Kind::Clearnet));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let next2 = next.clone();
    let path2 = path.clone();
    rt.block_on(crate::db::with_session(old.clone(), async move { prefs::hydrate(&next2, &path2) }));
    assert_eq!(prefs::of(&next).kind(), Some(Kind::I2p));
    assert_eq!(prefs::of(&old).kind(), Some(Kind::Clearnet), "the bound session is untouched");
}

#[test]
fn prelogin_effective_only_raises() {
    use prefs::StoredKind;
    let _s = scene();
    prelogin::disarm();
    assert_eq!(prelogin::effective(&StoredKind::Absent), Some(Kind::Clearnet));
    assert_eq!(prelogin::effective(&StoredKind::Kind(Kind::Clearnet)), Some(Kind::Clearnet));
    assert_eq!(prelogin::effective(&StoredKind::Kind(Kind::Tor)), Some(Kind::Tor));
    for armed in [Kind::Tor, Kind::I2p] {
        prelogin::arm(prelogin::PreloginChoice::new(armed));
        assert_eq!(prelogin::effective(&StoredKind::Absent), Some(armed));
        assert_eq!(prelogin::effective(&StoredKind::Kind(Kind::Clearnet)), Some(armed), "the welcome screen raises Clearnet");
        assert_eq!(prelogin::effective(&StoredKind::Kind(Kind::Tor)), Some(Kind::Tor), "an account's own choice is never changed");
        assert_eq!(prelogin::effective(&StoredKind::Kind(Kind::I2p)), Some(Kind::I2p));
        assert_eq!(prelogin::effective(&StoredKind::ReadError("x".into())), None);
    }
    // Arming Clearnet is a bug in the caller; it disarms either way.
    prelogin::arm(prelogin::PreloginChoice::new(Kind::Tor));
    let r = std::panic::catch_unwind(|| prelogin::arm(prelogin::PreloginChoice::new(Kind::Clearnet)));
    assert_eq!(r.is_err(), cfg!(debug_assertions));
    assert!(prelogin::armed().is_none());

    // Markers: legacy honoured, kept beside any non-Clearnet choice for older builds, unreadable
    // means Unknown.
    let _acct = test_account();
    let dir = crate::db::get_app_data_dir().unwrap().clone();
    prelogin::set_marker(None).unwrap();
    assert_eq!(prelogin::marker().unwrap(), None);
    std::fs::write(dir.join("tor_prelogin"), b"1").unwrap();
    assert_eq!(prelogin::marker().unwrap().map(|c| c.kind), Some(Kind::Tor), "the legacy marker still counts");
    let choice = prelogin::PreloginChoice::with_opts(Kind::I2p, serde_json::json!({ "sam_port": 7777 }));
    prelogin::set_marker(Some(&choice)).unwrap();
    assert!(dir.join("tor_prelogin").exists(), "an older build reading only the legacy marker stays off the direct path");
    assert_eq!(prelogin::marker().unwrap(), Some(choice.clone()), "the new marker wins over the legacy one");
    // Status views read the remembered kind from memory: what was last written or read.
    std::fs::remove_file(dir.join("transport_prelogin")).unwrap();
    assert_eq!(prelogin::marker_kind(), Ok(Some(Kind::I2p)), "no disk read per view");
    assert_eq!(status::view().prelogin, "i2p");
    prelogin::set_marker(Some(&choice)).unwrap();
    prelogin::set_marker(Some(&prelogin::PreloginChoice::new(Kind::Clearnet))).unwrap();
    assert!(!dir.join("tor_prelogin").exists() && prelogin::marker().unwrap().is_none(), "Clearnet removes both");
    prelogin::set_marker(Some(&choice)).unwrap();
    prelogin::apply_at_boot();
    assert_eq!(preference(), Some(Kind::I2p));
    assert_eq!(prelogin::armed().map(|c| c.kind), Some(Kind::I2p));
    std::fs::write(dir.join("transport_prelogin"), b"{garbage").unwrap();
    assert!(prelogin::marker().is_err());
    assert_eq!(prelogin::marker_kind(), Err(prelogin::UnreadableMarker), "a read refreshes what views show");
    prelogin::apply_at_boot();
    assert_eq!(preference(), None, "an unreadable marker leaves the welcome screen blocked");
    prelogin::set_marker(None).unwrap();
    assert!(!dir.join("tor_prelogin").exists());
    prelogin::apply_at_boot();
    assert_eq!(preference(), Some(Kind::Clearnet));

    // Commit: raises only an absent or Clearnet account, filling only absent config fields.
    let port = |cfg: KindConfig| cfg.downcast_ref::<i2p_config::I2pConfig>().unwrap().sam_port;
    crate::db::close_database();
    crate::db::clear_current_account_in_memory();
    prelogin::arm(prelogin::PreloginChoice::with_opts(Kind::I2p, serde_json::json!({ "sam_port": 7777 })));
    assert_eq!(port(prelogin::config_for(Kind::I2p)), 7777, "no account yet: the welcome screen's options");
    let acct = test_account();
    crate::db::init_database(&acct).unwrap();
    assert_eq!(port(prefs::config(Kind::I2p)), i2p_config::DEFAULT_SAM_PORT);
    assert_eq!(port(prelogin::config_for(Kind::I2p)), 7777, "a start before the commit already runs what it will save");
    // An unreadable preference keeps the choice armed for the next try, never disarmed unsaved.
    let rename = |from: &str, to: &str| {
        let conn = crate::db::get_write_connection_guard_static().unwrap();
        conn.execute(&format!("ALTER TABLE {from} RENAME TO {to}"), []).unwrap();
    };
    rename("settings", "settings_hidden");
    assert!(matches!(prefs::stored_kind(), StoredKind::ReadError(_)));
    prelogin::commit();
    assert_eq!(prelogin::armed().map(|c| c.kind), Some(Kind::I2p), "a read error keeps the choice armed");
    rename("settings_hidden", "settings");
    prelogin::commit();
    assert_eq!(port(prelogin::config_for(Kind::I2p)), 7777, "and after it");
    assert!(prelogin::armed().is_none());
    assert_eq!(prefs::stored_kind(), StoredKind::Kind(Kind::I2p));
    let cfg = prefs::config(Kind::I2p);
    assert_eq!(cfg.downcast_ref::<i2p_config::I2pConfig>().unwrap().sam_port, 7777);

    crate::db::close_database();
    let acct = test_account();
    crate::db::init_database(&acct).unwrap();
    prefs::persist_kind(Kind::Tor).unwrap();
    prefs::persist_config(Kind::I2p, r#"{"sam_port":7000,"sam_user":null,"sam_password":null,"exit":"allow","outproxies":null}"#).unwrap();
    prelogin::arm(prelogin::PreloginChoice::with_opts(Kind::I2p, serde_json::json!({ "sam_port": 7777 })));
    assert_eq!(port(prelogin::config_for(Kind::I2p)), port(prefs::config(Kind::I2p)), "never carried into an account that chose Tor");
    prelogin::commit();
    assert!(prelogin::armed().is_none(), "a committed account always disarms");
    assert_eq!(prefs::stored_kind(), StoredKind::Kind(Kind::Tor), "its own Tor is never replaced");
    let raw = crate::db::settings::get_sql_setting("transport_cfg_i2p".into()).unwrap().unwrap();
    assert!(raw.contains("7000"), "never over a field the account set: {raw}");
}

#[test]
fn prelogin_second_identity_rotates() {
    let _s = scene();
    set_preference(Some(Kind::Tor));
    let mock = Mock::new(Kind::Tor, true, MockDial::Hang);
    host::activate(mock.clone(), live(), true).unwrap();
    let a = nostr_sdk::prelude::Keys::generate().public_key();
    let b = nostr_sdk::prelude::Keys::generate().public_key();
    prelogin::note_identity(&a);
    prelogin::note_identity(&a);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(mock.identities.load(Ordering::SeqCst), 0, "the first identity keeps the instance");
    prelogin::note_identity(&b);
    for _ in 0..50 {
        if mock.identities.load(Ordering::SeqCst) == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(mock.identities.load(Ordering::SeqCst), 1, "a second identity starts afresh");

    // An account's own instance never rotates on identity.
    reset();
    set_preference(Some(Kind::Tor));
    let own = Mock::new(Kind::Tor, true, MockDial::Hang);
    host::activate(own.clone(), live(), false).unwrap();
    prelogin::note_identity(&a);
    prelogin::note_identity(&b);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(own.identities.load(Ordering::SeqCst), 0);
}

// ── Realtime ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn realtime_consent_per_session_and_kind() {
    let _s = scene();
    assert!(realtime::allowed(), "Clearnet needs no consent");
    set_preference(Some(Kind::Tor));
    assert_eq!(realtime::check(), Err(realtime::CONSENT_REQUIRED.to_string()));
    realtime::grant();
    assert!(realtime::allowed());
    set_preference(Some(Kind::I2p));
    assert_eq!(realtime::check(), Err(realtime::CONSENT_REQUIRED.to_string()), "a kind change drops consent");
    realtime::grant();
    assert!(realtime::allowed());
    set_preference(None);
    assert!(!realtime::allowed(), "Unknown never connects");

    set_preference(Some(Kind::I2p));
    realtime::grant();
    let old = crate::db::live_session();
    crate::db::close_database();
    set_preference(Some(Kind::I2p));
    assert_eq!(realtime::check(), Err(realtime::CONSENT_REQUIRED.to_string()), "another session holds no consent");
    let stale = crate::db::with_session(old, async { realtime::check() }).await;
    assert!(stale.is_err(), "a caller whose session is not live is denied");

    realtime::allow_always(true);
    assert!(realtime::allowed(), "the SDK's process-wide opt-in");
    realtime::allow_always(false);

    #[cfg(feature = "xdc")]
    {
        assert_eq!(crate::xdc::mesh::lease().await.err(), Some(realtime::CONSENT_REQUIRED.to_string()));
        assert_eq!(crate::xdc::mesh::mesh().await.err(), Some(realtime::CONSENT_REQUIRED.to_string()));
    }
}

// ── Guards ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn guarded_stream_closes_on_epoch_and_swap() {
    let _s = scene();
    let (addr, _seen, _) = recorder().await;
    for swap in [false, true] {
        let tcp = TcpStream::connect(addr).await.unwrap();
        let mut g = guard::Guarded::new(tcp, epoch(), live());
        g.write_all(b"hello").await.unwrap();
        let idle = tokio::spawn(async move {
            let mut buf = [0u8; 16];
            // Drain the recorder's reply, then sit idle until the guard trips.
            loop {
                match g.read(&mut buf).await {
                    Ok(0) => return None,
                    Ok(_) => continue,
                    Err(e) => return Some(e.kind()),
                }
            }
        });
        tokio::time::sleep(Duration::from_millis(600)).await;
        if swap {
            crate::db::close_database();
        } else {
            bump_epoch();
        }
        let got = tokio::time::timeout(Duration::from_secs(5), idle).await.expect("an idle socket wakes at the change").unwrap();
        assert_eq!(got, Some(std::io::ErrorKind::ConnectionAborted), "swap={swap}");
    }
}

// ── HTTP ─────────────────────────────────────────────────────────────────────

/// An HTTP server answering every request with `body`, counting connections.
async fn http_server(body: &'static [u8], slow: bool) -> (SocketAddr, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let conns = Arc::new(AtomicUsize::new(0));
    let c2 = conns.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut c, _)) = l.accept().await else { return };
            c2.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                loop {
                    let mut buf = vec![0u8; 8192];
                    let mut got = Vec::new();
                    loop {
                        let Ok(n) = c.read(&mut buf).await else { return };
                        if n == 0 {
                            return;
                        }
                        got.extend_from_slice(&buf[..n]);
                        if got.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    if slow {
                        let head = "HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n";
                        let _ = c.write_all(head.as_bytes()).await;
                        for _ in 0..1000 {
                            if c.write_all(&[b'x'; 1000]).await.is_err() {
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                        return;
                    }
                    let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                    if c.write_all(head.as_bytes()).await.is_err() || c.write_all(body).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (addr, conns)
}

#[tokio::test]
async fn http_client_resolves_per_request() {
    let _s = scene();
    let (addr, conns) = http_server(b"ok", false).await;
    let url = format!("http://{addr}/");
    let client = crate::net::build_http_client(Duration::from_secs(10)).unwrap();
    assert_eq!(client.get(&url).send().await.unwrap().text().await.unwrap(), "ok");
    assert_eq!(conns.load(Ordering::SeqCst), 1);

    // The same client, taken before the switch, never sends on the old egress.
    set_preference(Some(Kind::Tor));
    let e = client.get(&url).send().await.err().expect("Tor chosen and not ready refuses");
    if Kind::Tor.compiled() {
        assert!(matches!(e, crate::net::HttpError::Refused(ConnectError::NotReady(Some(Kind::Tor)))), "{e:?}");
        assert_eq!(e.to_string(), "Tor is still connecting.");
        assert!(e.is_transient());
    } else {
        assert_eq!(e.to_string(), "This build doesn't include Tor.", "a build without Tor blocks a Tor account");
        assert!(!e.is_transient(), "no wait brings a missing network into the build");
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(conns.load(Ordering::SeqCst), 1, "no connection reached the server");

    // A policy refusal is not transient.
    reset();
    set_preference(Some(Kind::I2p));
    prefs::live().set_config_value(Kind::I2p, Arc::new(i2p_config::I2pConfig { exit: ExitPolicy::Off, ..Default::default() }));
    host::activate(Mock::new(Kind::I2p, true, MockDial::Hang), live(), false).unwrap();
    let e = client.get("https://example.com/").send().await.err().unwrap();
    assert!(matches!(e, crate::net::HttpError::Refused(ConnectError::Refused(Refusal::ExitOff))), "{e:?}");
    assert!(!e.is_transient());
    assert_eq!(e.to_string(), "I2P-Only is on, so this server is off.");

    // Back on Clearnet, the old descriptor works again, and every bump empties the pool.
    reset();
    assert_eq!(client.get(&url).send().await.unwrap().text().await.unwrap(), "ok");
    let opened = conns.load(Ordering::SeqCst);
    assert_eq!(client.get(&url).send().await.unwrap().text().await.unwrap(), "ok");
    assert_eq!(conns.load(Ordering::SeqCst), opened, "one epoch shares one pool");
    bump_epoch();
    assert_eq!(client.get(&url).send().await.unwrap().text().await.unwrap(), "ok", "an older descriptor re-runs on the new client");
    assert_eq!(conns.load(Ordering::SeqCst), opened + 1, "the bump dropped the pooled connection");
}

#[tokio::test]
async fn http_body_aborts_on_change() {
    let _s = scene();
    let (addr, _) = http_server(b"", true).await;
    let client = crate::net::build_http_client_with_options(None, None, true).unwrap();
    let mut resp = client.get(format!("http://{addr}/")).send().await.unwrap();
    assert!(resp.chunk().await.unwrap().is_some());
    let bumper = tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        bump_epoch();
    });
    let mut err = None;
    for _ in 0..1000 {
        match resp.chunk().await {
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    bumper.await.unwrap();
    assert!(matches!(err, Some(crate::net::HttpError::NetworkChanged)), "{err:?}");

    // A whole-body read races the change too.
    let resp = client.get(format!("http://{addr}/")).send().await.unwrap();
    let bumper = tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        bump_epoch();
    });
    let r = resp.bytes().await;
    bumper.await.unwrap();
    assert!(matches!(r, Err(crate::net::HttpError::NetworkChanged)));

    // An upload body ends with an error at the change.
    use futures_util::StreamExt;
    let source = futures_util::stream::iter((0..1000).map(|_| Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"chunk"))))
        .then(|c| async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            c
        });
    let body = crate::net::guard_body(source, epoch(), live());
    futures_util::pin_mut!(body);
    let mut sent = 0;
    let mut ended = None;
    let bumper = tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        bump_epoch();
    });
    while let Some(item) = body.next().await {
        match item {
            Ok(_) => sent += 1,
            Err(e) => {
                ended = Some(e.kind());
                break;
            }
        }
    }
    bumper.await.unwrap();
    assert_eq!(ended, Some(std::io::ErrorKind::ConnectionAborted));
    assert!(sent < 1000);
}

#[tokio::test]
async fn redirect_to_foreign_suffix_refused() {
    let _s = scene();
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        let Ok((mut c, _)) = l.accept().await else { return };
        let mut buf = [0u8; 2048];
        let _ = c.read(&mut buf).await;
        let r = format!("HTTP/1.1 302 Found\r\nLocation: http://{B32}/\r\nContent-Length: 0\r\n\r\n");
        let _ = c.write_all(r.as_bytes()).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let client = crate::net::build_http_client(Duration::from_secs(10)).unwrap();
    let e = client.get(format!("http://{addr}/")).send().await.err().expect("the hop is refused");
    let text = format!("{e} {:?}", std::error::Error::source(&e));
    assert!(text.contains("only reachable over I2P"), "refused by the redirect policy before any resolver: {text}");
}

/// A file server for the transfer loops: GET serves `payload` (counted), HEAD answers 200, and
/// PUT stores nothing but answers with the descriptor of what it was sent.
async fn blob_server(payload: &'static [u8]) -> (SocketAddr, Arc<AtomicUsize>) {
    use sha2::Digest;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let gets = Arc::new(AtomicUsize::new(0));
    let g2 = gets.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut c, _)) = l.accept().await else { return };
            let gets = g2.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut got: Vec<u8> = Vec::new();
                loop {
                    let head_end = loop {
                        if let Some(p) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                            break p + 4;
                        }
                        match c.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => got.extend_from_slice(&buf[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&got[..head_end]).to_ascii_lowercase();
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
                        .unwrap_or(0);
                    while got.len() < head_end + len {
                        match c.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => got.extend_from_slice(&buf[..n]),
                        }
                    }
                    let body: Vec<u8> = got[head_end..head_end + len].to_vec();
                    got.drain(..head_end + len);
                    let reply = if head.starts_with("get ") {
                        gets.fetch_add(1, Ordering::SeqCst);
                        let mut r = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", payload.len()).into_bytes();
                        r.extend_from_slice(payload);
                        r
                    } else if head.starts_with("put ") {
                        let sha = crate::simd::hex::bytes_to_hex_string(&sha2::Sha256::digest(&body));
                        let json = format!(
                            r#"{{"url":"http://files.example.com/{sha}","sha256":"{sha}","size":{},"type":"application/octet-stream","uploaded":1}}"#,
                            body.len()
                        );
                        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{json}", json.len()).into_bytes()
                    } else {
                        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec()
                    };
                    if c.write_all(&reply).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (addr, gets)
}

/// The real transfer loops wait out a network that is coming up without spending an attempt and
/// resume once it is ready; a network that never comes up costs one wait for the whole source
/// walk; a block only the user can lift costs none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transfer_waits_for_ready_without_counting() {
    use sha2::Digest;
    let _s = scene();
    crate::db::close_database();
    let acct = test_account();
    crate::db::init_database(&acct).unwrap();
    // Files load from their source here: the Magnitude proxy is not under test.
    crate::db::settings::set_sql_setting(crate::proxy::SETTING_KEY.into(), "false".into()).unwrap();
    let payload: &'static [u8] = b"vector transfer payload";
    let hash = crate::simd::hex::bytes_to_hex_string(&sha2::Sha256::digest(payload));
    let (server, gets) = blob_server(payload).await;
    set_preference(Some(Kind::Tor));
    let install = |mock: &Arc<Mock>| {
        drop(host::uninstall());
        host::activate(mock.clone(), live(), false).unwrap().id()
    };
    let come_up = |mock: Arc<Mock>, id: u64| async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        mock.ready.store(true, Ordering::SeqCst);
        host::notify(host::TransportEvent::Ready { kind: Kind::Tor, instance: id, owner: live(), recovered: false });
    };

    // Download, one source, the network comes up mid-walk: one request reaches the server.
    let mock = Mock::new(Kind::Tor, false, MockDial::To(server));
    let id = install(&mock);
    let att = crate::Attachment { url: format!("http://files.example.com/{hash}"), original_hash: Some(hash.clone()), ..Default::default() };
    let up = tokio::spawn(come_up(mock.clone(), id));
    let got = crate::VectorCore.download_attachment_reporting(&att, None, 1 << 20, |_| true).await.expect("resumes once up");
    up.await.unwrap();
    assert_eq!(got, payload);
    assert_eq!(gets.load(Ordering::SeqCst), 1, "the wait spent no attempt and no extra request");

    // Upload with no retries at all: the wait is not an attempt, so it still lands.
    let mock = Mock::new(Kind::Tor, false, MockDial::To(server));
    let id = install(&mock);
    let up = tokio::spawn(come_up(mock.clone(), id));
    let signer = crate::signer::ActiveSigner::Keys(nostr_sdk::prelude::Keys::generate());
    let server_url = url::Url::parse("http://files.example.com/").unwrap();
    let uploaded = crate::blossom::upload_blob_with_progress(
        signer,
        &server_url,
        Arc::new(payload.to_vec()),
        Some("application/octet-stream"),
        Arc::new(|_, _| Ok(())),
        Some(0),
        None,
        None,
    )
    .await
    .expect("a network coming up spends no retry");
    up.await.unwrap();
    assert!(uploaded.ends_with(&hash), "{uploaded}");

    // Never up: the whole walk waits once, then fails with the network's own words.
    budget::override_for_test(Op::Startup, Some(Duration::from_millis(800)));
    let never = Mock::new(Kind::Tor, false, MockDial::Hang);
    install(&never);
    let two = crate::Attachment {
        url: format!("http://a.example.com/{hash}"),
        fallback_urls: vec![format!("http://b.example.com/{hash}")],
        original_hash: Some(hash.clone()),
        ..Default::default()
    };
    let t0 = web_time::Instant::now();
    let r = crate::VectorCore.download_attachment_reporting(&two, None, 1 << 20, |_| true).await;
    let took = t0.elapsed();
    assert!(matches!(r, Err(crate::DownloadError::Unreachable(ref t)) if t == "Can't reach your I2P router at 127.0.0.1:7656."), "{r:?}");
    assert!(took >= Duration::from_millis(800) && took < Duration::from_millis(1500), "one wait for both sources, took {took:?}");
    assert!(never.dialed().is_empty());

    // A block only the user can lift: no wait at all.
    *never.reason.lock().unwrap() = status::Reason::new("sam_auth_failed", "Your router turned down the SAM username or password.");
    let refused = crate::net::HttpError::Refused(blocked_error(Kind::Tor));
    assert!(!refused.is_transient(), "a turned-down login doesn't come back on its own");
    let t0 = web_time::Instant::now();
    let r = crate::VectorCore.download_attachment_reporting(&two, None, 1 << 20, |_| true).await;
    assert!(t0.elapsed() < Duration::from_millis(300), "took {:?}", t0.elapsed());
    assert!(matches!(r, Err(crate::DownloadError::Unreachable(ref t)) if t.contains("turned down")), "{r:?}");

    // A policy refusal never waits.
    set_preference(Some(Kind::Clearnet));
    let refusal = crate::net::HttpError::Refused(ConnectError::Refused(Refusal::ExitOff));
    let t0 = web_time::Instant::now();
    assert!(crate::net::wait_after(&refusal).await.is_err());
    assert!(t0.elapsed() < Duration::from_millis(100));
    crate::db::close_database();
}

/// A media server the network never let Vector reach is not marked offline, so it doesn't read
/// Offline for half an hour after switching to a network that works.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blocked_info_fetch_never_marks_a_server_offline() {
    let _s = scene();
    crate::db::close_database();
    let acct = test_account();
    crate::db::init_database(&acct).unwrap();
    let signer = crate::signer::ActiveSigner::Keys(nostr_sdk::prelude::Keys::generate());
    let server = "https://media.example.com";
    let failed = |url: &str| crate::blossom_stats::stats_for(url).and_then(|s| s.last_fail_kind);

    set_preference(Some(Kind::Tor));
    assert!(crate::blossom_info::fetch(&signer, server).await.is_err(), "Tor isn't running");
    set_preference(Some(Kind::I2p));
    prefs::live().set_config_value(Kind::I2p, Arc::new(i2p_config::I2pConfig { exit: ExitPolicy::Off, ..Default::default() }));
    host::activate(Mock::new(Kind::I2p, true, MockDial::Hang), live(), false).unwrap();
    assert!(crate::blossom_info::fetch(&signer, server).await.is_err(), "I2P-Only refuses it");
    assert_eq!(failed(server), None, "never marked offline for the network's own refusal");

    // A server that really didn't answer still is.
    drop(host::uninstall());
    set_preference(Some(Kind::Clearnet));
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let gone = format!("http://127.0.0.1:{dead}");
    assert!(crate::blossom_info::fetch(&signer, &gone).await.is_err());
    assert_eq!(failed(&gone).as_deref(), Some(crate::blossom_stats::FAIL_OFFLINE));
    crate::db::close_database();
}

// ── Relay sockets ────────────────────────────────────────────────────────────

/// The first request head a client sends to a listener.
async fn capture_upgrade(connect: impl std::future::Future<Output = ()>, l: TcpListener) -> Vec<u8> {
    let cap = tokio::spawn(async move {
        let (mut c, _) = l.accept().await.unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        while !got.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = c.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
        got
    });
    connect.await;
    cap.await.unwrap()
}

/// Everything but the random `Sec-WebSocket-Key` value.
fn without_key(head: &[u8]) -> String {
    String::from_utf8_lossy(head)
        .lines()
        .map(|l| if l.to_ascii_lowercase().starts_with("sec-websocket-key:") { "sec-websocket-key: *".to_string() } else { l.to_string() })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn ws_upgrade_request_matches_default() {
    use nostr_sdk::transport::websocket::{DefaultWebsocketTransport, WebSocketTransport};
    let _s = scene();
    let l1 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url1 = url::Url::parse(&format!("ws://{}/", l1.local_addr().unwrap())).unwrap();
    let theirs = capture_upgrade(
        async {
            let _ = tokio::time::timeout(Duration::from_millis(500), DefaultWebsocketTransport.connect(&url1, None)).await;
        },
        l1,
    )
    .await;
    let l2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port2 = l2.local_addr().unwrap().port();
    let url2 = url::Url::parse(&format!("ws://127.0.0.1:{port2}/")).unwrap();
    let ours = capture_upgrade(
        async {
            let ws = ws::VectorWs::new(live(), Lane::Account);
            let _ = tokio::time::timeout(Duration::from_millis(500), ws.connect(&url2, None)).await;
        },
        l2,
    )
    .await;
    let theirs = without_key(&theirs).replace(&url1.port().unwrap().to_string(), "PORT");
    let ours = without_key(&ours).replace(&port2.to_string(), "PORT");
    assert!(theirs.contains("user-agent: nostr-sdk/0.45.1"), "{theirs}");
    assert_eq!(ours, theirs, "the Direct path sends the default transport's upgrade, byte for byte");
}

#[tokio::test]
async fn ws_refuses_before_any_socket() {
    use nostr_sdk::transport::websocket::WebSocketTransport;
    let _s = scene();
    let ws = ws::VectorWs::new(live(), Lane::Account);
    let t0 = web_time::Instant::now();
    let url = url::Url::parse(&format!("ws://{B32}/")).unwrap();
    let e = ws.connect(&url, None).await.err().expect("refused");
    assert!(e.to_string().contains("This server is only reachable over I2P."), "{e}");
    assert!(t0.elapsed() < Duration::from_millis(200), "no connect was attempted");
    // A stale owner opens nothing either.
    let stale = ws::VectorWs::new(live() + 1, Lane::Account);
    let e = stale.connect(&url::Url::parse("wss://relay.example.com").unwrap(), None).await.err().unwrap();
    assert!(e.to_string().contains("The network changed."), "{e}");
}

// ── The bridge ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_socks_protocol() {
    let _s = scene();
    let bridge_addr = bridge::addr().unwrap();
    let (echo, seen, _) = recorder().await;
    set_preference(Some(Kind::Tor));
    let mock = Mock::new(Kind::Tor, true, MockDial::After(Duration::from_millis(300), echo));
    let inst = host::activate(mock.clone(), live(), false).unwrap();
    let t = Ticket { owner: live(), epoch: epoch(), lane: Lane::Account };

    // Only RFC 1929 is offered on the main port.
    let mut c = TcpStream::connect(bridge_addr).await.unwrap();
    c.write_all(&[5, 1, 0]).await.unwrap();
    let mut r = [0u8; 2];
    c.read_exact(&mut r).await.unwrap();
    assert_eq!(r, [5, 0xFF], "no-auth is refused");

    // A wrong secret is refused.
    let mut c = TcpStream::connect(bridge_addr).await.unwrap();
    c.write_all(&[5, 1, 2]).await.unwrap();
    c.read_exact(&mut r).await.unwrap();
    let user = t.encode();
    let mut auth = vec![1, user.len() as u8];
    auth.extend_from_slice(user.as_bytes());
    auth.extend_from_slice(&[32]);
    auth.extend_from_slice(&[b'0'; 32]);
    c.write_all(&auth).await.unwrap();
    c.read_exact(&mut r).await.unwrap();
    assert_eq!(r, [1, 1]);

    // A stale ticket is refused with 0x02 and nothing is dialed.
    let stale = Ticket { epoch: t.epoch - 1, ..t };
    let mut c = socks_hello(bridge_addr, &stale).await;
    c.write_all(&connect_domain("relay.example.com", 443)).await.unwrap();
    assert_eq!(reply_code(&mut c).await, 0x02);
    assert!(mock.dialed().is_empty());

    // CONNECT only; unknown address types refused.
    let mut c = socks_hello(bridge_addr, &t).await;
    c.write_all(&[5, 2, 0, 1, 1, 2, 3, 4, 0, 80]).await.unwrap();
    assert_eq!(reply_code(&mut c).await, 0x07);
    let mut c = socks_hello(bridge_addr, &t).await;
    c.write_all(&[5, 1, 0, 9, 0, 80]).await.unwrap();
    assert_eq!(reply_code(&mut c).await, 0x08);

    // A domain arrives exactly; success is replied only after the dial, and pipelined bytes
    // sent before the reply reach the far end untouched.
    let mut c = socks_hello(bridge_addr, &t).await;
    let t0 = web_time::Instant::now();
    c.write_all(&connect_domain("Relay.Example.com", 443)).await.unwrap();
    c.write_all(b"PIPELINED").await.unwrap();
    assert_eq!(reply_code(&mut c).await, 0x00);
    assert!(t0.elapsed() >= Duration::from_millis(280), "success only once the stream exists");
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(mock.dialed()[0].0, Route::Exit { host: "relay.example.com".into(), port: 443 });
    assert_eq!(mock.dialed()[0].1, Lane::Account);
    assert!(seen.lock().unwrap().iter().any(|b| b == b"PIPELINED"), "{:?}", seen.lock().unwrap());
    drop(c);

    // IPv4 and IPv6 parse into IP routes.
    for (req, want) in [
        (vec![5, 1, 0, 1, 1, 2, 3, 4, 1, 187], "1.2.3.4"),
        ([vec![5, 1, 0, 4], std::net::Ipv6Addr::LOCALHOST.octets().to_vec(), vec![1, 187]].concat(), "::1"),
    ] {
        let mut c = socks_hello(bridge_addr, &t).await;
        c.write_all(&req).await.unwrap();
        assert_eq!(reply_code(&mut c).await, 0x00);
        assert_eq!(mock.dialed().last().unwrap().0, Route::Exit { host: want.into(), port: 443 });
    }

    // Reply codes follow the dial's error.
    for (e, code) in [
        (ConnectError::Unreachable("arti".into()), 0x04),
        (ConnectError::RouterDown { port: 7656 }, 0x03),
        (ConnectError::Timeout, 0x06),
        (ConnectError::NoExit, 0x04),
    ] {
        *mock.dial.lock().unwrap() = MockDial::Refuse(e.clone());
        let mut c = socks_hello(bridge_addr, &t).await;
        c.write_all(&connect_domain("relay.example.com", 443)).await.unwrap();
        assert_eq!(reply_code(&mut c).await, code, "{e:?}");
    }

    // A client that hangs up mid-dial cancels it.
    *mock.dial.lock().unwrap() = MockDial::Hang;
    let to = |h: &'static str| move |i: &bridge::ConnInfo| i.route.as_ref().and_then(Route::host) == Some(h);
    let mut c = socks_hello(bridge_addr, &t).await;
    c.write_all(&connect_domain("hangup.example.com", 443)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(bridge::count_where(to("hangup.example.com")), 1);
    drop(c);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(bridge::count_where(to("hangup.example.com")), 0, "the hangup cancelled the dial");

    // abort_where reaches a dial in flight.
    let mut c = socks_hello(bridge_addr, &t).await;
    c.write_all(&connect_domain("abort.example.com", 443)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(bridge::abort_where(to("abort.example.com")), 1);
    let mut buf = [0u8; 1];
    assert!(matches!(tokio::time::timeout(Duration::from_secs(2), c.read(&mut buf)).await, Ok(Ok(0)) | Ok(Err(_))));

    // A slow greeting times out.
    bridge::set_handshake_deadline_for_test(Duration::from_millis(200));
    let mut c = TcpStream::connect(bridge_addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(matches!(tokio::time::timeout(Duration::from_secs(2), c.read(&mut buf)).await, Ok(Ok(0)) | Ok(Err(_))));
    bridge::set_handshake_deadline_for_test(Duration::from_secs(10));

    // Uninstall with join waits until the bridge holds none of the instance.
    *mock.dial.lock().unwrap() = MockDial::To(echo);
    let mut c = socks_hello(bridge_addr, &t).await;
    c.write_all(&connect_domain("spliced.example.com", 443)).await.unwrap();
    assert_eq!(reply_code(&mut c).await, 0x00);
    let id = inst.id();
    drop(inst);
    host::deactivate(true).await;
    assert_eq!(mock.shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(bridge::count_where(|i| i.instance == id), 0, "drain returned only once instance {id} was gone");
    assert!(matches!(tokio::time::timeout(Duration::from_secs(2), c.read(&mut buf)).await, Ok(Ok(0)) | Ok(Err(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_post_dial_recheck_refuses_after_policy_change() {
    let _s = scene();
    let bridge_addr = bridge::addr().unwrap();
    let (echo, _, _) = recorder().await;
    set_preference(Some(Kind::I2p));
    let mock = Mock::new(Kind::I2p, true, MockDial::After(Duration::from_millis(400), echo));
    host::activate(mock.clone(), live(), false).unwrap();
    let t = Ticket { owner: live(), epoch: epoch(), lane: Lane::Shared };
    let mut c = socks_hello(bridge_addr, &t).await;
    c.write_all(&connect_domain("nos.lol", 443)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    // I2P-Only turned on while the exit dial runs.
    prefs::set_config(Kind::I2p, Arc::new(i2p_config::I2pConfig { exit: ExitPolicy::Off, ..Default::default() }));
    assert_eq!(reply_code(&mut c).await, 0x02, "the stream is never handed over");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restricted_permits_single_use() {
    let _s = scene();
    let restricted = bridge::restricted_addr().unwrap();
    let (echo, _, _) = recorder().await;
    set_preference(Some(Kind::Tor));
    let mock = Mock::new(Kind::Tor, true, MockDial::To(echo));
    host::activate(mock.clone(), live(), false).unwrap();
    let t = Ticket { owner: live(), epoch: epoch(), lane: Lane::Account };
    let open = |host: &'static str| async move {
        let mut c = TcpStream::connect(restricted).await.unwrap();
        c.write_all(&[5, 1, 0]).await.unwrap();
        let mut r = [0u8; 2];
        c.read_exact(&mut r).await.unwrap();
        assert_eq!(r, [5, 0]);
        c.write_all(&connect_domain(host, 443)).await.unwrap();
        let code = reply_code(&mut c).await;
        (c, code)
    };
    assert_eq!(open("bunker.example.com").await.1, 0x02, "no permit, no connection");
    bridge::permit(t, "bunker.example.com", 443);
    assert_eq!(open("other.example.com").await.1, 0x02, "a permit names one host");
    let (_c, code) = open("bunker.example.com").await;
    assert_eq!(code, 0x00);
    assert_eq!(open("bunker.example.com").await.1, 0x02, "and serves one connection");
    bridge::permit(t, "bunker.example.com", 443);
    bridge::age_permits_for_test(Duration::from_secs(31));
    assert_eq!(open("bunker.example.com").await.1, 0x02, "an expired permit serves nothing");
    // The main port refuses no-auth outright.
    let mut c = TcpStream::connect(bridge::addr().unwrap()).await.unwrap();
    c.write_all(&[5, 1, 0]).await.unwrap();
    let mut r = [0u8; 2];
    c.read_exact(&mut r).await.unwrap();
    assert_eq!(r, [5, 0xFF]);
}

#[test]
fn nowhere_never_connects() {
    assert!(std::net::TcpStream::connect_timeout(&bridge::NOWHERE, Duration::from_secs(2)).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reqwest_and_nostr_send_domains_through_bridge() {
    use nostr_sdk::transport::websocket::WebSocketTransport;
    let _s = scene();
    set_preference(Some(Kind::I2p));
    let mock = Mock::new(Kind::I2p, true, MockDial::Refuse(ConnectError::Unreachable("offline".into())));
    host::activate(mock.clone(), live(), false).unwrap();

    let client = crate::net::build_http_client(Duration::from_secs(10)).unwrap();
    let e = client.get(format!("http://{B32}/")).send().await.err().unwrap();
    assert_eq!(mock.dialed()[0], (Route::Native { host: B32.into(), port: 80 }, Lane::Shared), "reqwest hands SOCKS the name");
    assert_eq!(e.to_string(), "This I2P address isn't online right now.", "the recorded reason, not a SOCKS code");

    let ws = ws::VectorWs::new(live(), Lane::Account);
    let e = ws.connect(&url::Url::parse(&format!("ws://{B32}/")).unwrap(), None).await.err().unwrap();
    assert_eq!(mock.dialed()[1], (Route::Native { host: B32.into(), port: 80 }, Lane::Account), "and so does the relay socket");
    assert!(e.to_string().contains("isn't online right now"), "{e}");

    // A refused route fails both at once, with no dial and no direct attempt.
    prefs::set_config(Kind::I2p, Arc::new(i2p_config::I2pConfig { exit: ExitPolicy::Off, ..Default::default() }));
    let t0 = web_time::Instant::now();
    assert!(client.get("https://nos.lol/").send().await.is_err());
    assert!(ws.connect(&url::Url::parse("wss://nos.lol").unwrap(), None).await.is_err());
    assert!(t0.elapsed() < Duration::from_secs(1));
    assert_eq!(mock.dialed().len(), 2);
}

/// A user's edit and a background check result never drop each other's change.
#[test]
fn concurrent_alias_writes_all_land() {
    let _s = scene();
    crate::db::close_database();
    let acct = test_account();
    crate::db::init_database(&acct).unwrap();
    let twin = |i: usize| format!("{}.b32.i2p", "a".repeat(51) + &((b'a' + (i % 26) as u8) as char).to_string());
    aliases::set("checked.example.com", Kind::I2p, Some(&twin(0)), aliases::AliasSource::User).unwrap();
    let writers: Vec<_> = (0..6)
        .map(|w| {
            std::thread::spawn(move || {
                for i in 0..20 {
                    aliases::set(&format!("h{w}-{i}.example.com"), Kind::I2p, Some(&twin(i)), aliases::AliasSource::User).unwrap();
                }
            })
        })
        .collect();
    let checker = std::thread::spawn(move || {
        for i in 0..60 {
            let state = if i % 2 == 0 { aliases::CheckState::Ok } else { aliases::CheckState::Unchecked };
            aliases::record_check("checked.example.com", aliases::AliasCheck { state, at: None, text: None }).unwrap();
        }
    });
    for w in writers {
        w.join().unwrap();
    }
    checker.join().unwrap();
    let live = aliases::table();
    let stored = prefs::read_stored(&crate::db::get_db_connection_guard_static().unwrap()).aliases;
    assert_eq!(live.entries().len(), 121, "every edit is in the live table");
    assert_eq!(stored.len(), 121, "and in the saved row");
    assert_eq!(live.get("checked.example.com").map(|e| e.check.state), Some(aliases::CheckState::Unchecked), "the last check holds");
    crate::db::close_database();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alias_validation_and_passthrough() {
    let _s = scene();
    assert_eq!(aliases::normalize_host("wss://Relay.Example.com/path").unwrap(), "relay.example.com");
    assert_eq!(aliases::normalize_host("relay.example.com").unwrap(), "relay.example.com");
    assert!(aliases::normalize_host("wss://1.2.3.4").is_err());
    assert_eq!(aliases::validate_twin(Kind::I2p, B32).unwrap(), B32);
    assert!(aliases::validate_twin(Kind::I2p, &format!("{}.b32.i2p", "b".repeat(60))).is_ok(), "b33");
    assert!(aliases::validate_twin(Kind::I2p, "relay.i2p").is_ok());
    assert_eq!(aliases::validate_twin(Kind::I2p, "relay.example.com").unwrap_err(), "Enter a .b32.i2p address.");

    let acct = test_account();
    crate::db::init_database(&acct).unwrap();
    set_preference(Some(Kind::I2p));
    let (twin_server, seen, _) = recorder().await;
    let mock = Mock::new(Kind::I2p, true, MockDial::To(twin_server));
    host::activate(mock.clone(), live(), false).unwrap();
    aliases::set("wss://relay.example.com", Kind::I2p, Some(B32), aliases::AliasSource::User).unwrap();
    assert_eq!(aliases::table().twin("relay.example.com", Kind::I2p), Some(B32));

    // A fake ClientHello rides the twin byte for byte: TLS stays end to end.
    let t = Ticket { owner: live(), epoch: epoch(), lane: Lane::Account };
    let mut c = socks_hello(bridge::addr().unwrap(), &t).await;
    c.write_all(&connect_domain("relay.example.com", 443)).await.unwrap();
    assert_eq!(reply_code(&mut c).await, 0x00);
    let hello: Vec<u8> = [&[0x16u8, 0x03, 0x01, 0x00, 0x05][..], b"hello"].concat();
    c.write_all(&hello).await.unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(mock.dialed()[0].0, Route::Twin { host: "relay.example.com".into(), via: B32.into(), port: 443 });
    assert!(seen.lock().unwrap().iter().any(|b| b == &hello));

    // Plaintext to 443 (http:// or ws:// with the port) never rides a twin: the bridge cuts it
    // before a byte reaches the twin's operator.
    let mut plain = socks_hello(bridge::addr().unwrap(), &t).await;
    plain.write_all(&connect_domain("relay.example.com", 443)).await.unwrap();
    assert_eq!(reply_code(&mut plain).await, 0x00);
    plain.write_all(b"GET /secret HTTP/1.1\r\nHost: relay.example.com:443\r\n\r\n").await.unwrap();
    let mut buf = [0u8; 16];
    assert!(matches!(tokio::time::timeout(Duration::from_secs(2), plain.read(&mut buf)).await, Ok(Ok(0)) | Ok(Err(_))));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!seen.lock().unwrap().iter().any(|b| b.windows(7).any(|w| w == b"/secret")), "no plaintext reached the twin");

    // Removing the twin aborts its connections and moves the policy generation, not the epoch.
    let (gen, ep) = (policy_gen(), epoch());
    let mut c2 = socks_hello(bridge::addr().unwrap(), &t).await;
    c2.write_all(&connect_domain("relay.example.com", 443)).await.unwrap();
    assert_eq!(reply_code(&mut c2).await, 0x00);
    aliases::set("relay.example.com", Kind::I2p, None, aliases::AliasSource::User).unwrap();
    assert!(policy_gen() > gen);
    assert_eq!(epoch(), ep);
    let mut buf = [0u8; 16];
    assert!(matches!(tokio::time::timeout(Duration::from_secs(2), c2.read(&mut buf)).await, Ok(Ok(0)) | Ok(Err(_))));
    assert_eq!(aliases::table().twin("relay.example.com", Kind::I2p), None);

    // A failed check stops routing through the twin at once.
    aliases::set("relay.example.com", Kind::I2p, Some(B32), aliases::AliasSource::User).unwrap();
    aliases::record_check("relay.example.com", aliases::AliasCheck { state: aliases::CheckState::Failed, at: None, text: None }).unwrap();
    assert_eq!(aliases::table().twin("relay.example.com", Kind::I2p), None);
    assert_eq!(egress(live(), Lane::Account, "relay.example.com", 443), Egress::Proxy(Ticket { owner: live(), epoch: epoch(), lane: Lane::Account }));
}

// ── Tor shims ────────────────────────────────────────────────────────────────

#[cfg(all(feature = "tor", not(target_arch = "wasm32")))]
#[test]
fn tor_shims_follow_the_generic_state() {
    use crate::tor::{self, TorTransportState};
    let _s = scene();
    let short = Duration::from_secs(5);
    let long = Duration::from_secs(300);

    // Tor off: direct, and every clearnet budget passes through untouched.
    tor::set_tor_enabled_pref(false);
    assert!(matches!(tor::transport_state(), TorTransportState::Disabled));
    assert_eq!(egress(live(), Lane::Account, "relay.example.com", 443), Egress::Direct);
    assert_eq!(crate::relay_connect_timeout(short), short);

    // THE leak invariant: Tor chosen but not up is never direct. Silent failure with an IP
    // disclosure as the cost, so it gets a permanent guard.
    tor::set_tor_enabled_pref(true);
    assert!(matches!(tor::transport_state(), TorTransportState::RequiredButInactive));
    assert!(
        matches!(egress(live(), Lane::Account, "relay.example.com", 443), Egress::Refuse(_)),
        "Tor enabled but inactive must never connect direct"
    );
    assert_eq!(ws::restricted_target(live(), "wss://relay.example.com"), Some(bridge::restricted_addr().unwrap()), "nor the signer's stock client");
    assert_eq!(crate::relay_connect_timeout(short), Duration::from_secs(60));
    assert_eq!(crate::relay_request_timeout(short), Duration::from_secs(30));

    // The welcome screen's choice raises an account whose own preference is off.
    tor::arm_prelogin_carry(true);
    assert!(tor::effective_tor_pref(false));
    tor::set_tor_enabled_pref(tor::effective_tor_pref(false));
    assert!(matches!(egress(live(), Lane::Account, "relay.example.com", 443), Egress::Refuse(_)));
    tor::arm_prelogin_carry(false);
    assert!(!tor::effective_tor_pref(false));
    assert!(tor::effective_tor_pref(true));

    // The floor only raises.
    for on in [true, false] {
        tor::set_tor_enabled_pref(on);
        assert_eq!(crate::relay_connect_timeout(long), long);
        assert_eq!(crate::relay_request_timeout(long), long);
    }

    // An active I2P is not Tor: fail safe for outside callers, and the Tor shims never turn it off.
    reset();
    set_preference(Some(Kind::I2p));
    host::activate(Mock::new(Kind::I2p, true, MockDial::Hang), live(), false).unwrap();
    assert!(matches!(tor::transport_state(), TorTransportState::RequiredButInactive));
    assert!(!tor::is_active());
    tor::set_tor_enabled_pref(false);
    assert_eq!(preference(), Some(Kind::I2p), "set_tor_enabled_pref(false) under I2P is a no-op");
    prelogin::arm(prelogin::PreloginChoice::new(Kind::I2p));
    tor::arm_prelogin_carry(false);
    assert_eq!(prelogin::armed().map(|c| c.kind), Some(Kind::I2p), "disarming Tor leaves an I2P carry");
    assert!(!tor::prelogin_carry_armed());
    let g = prelogin::generation();
    tor::cancel_prelogin_start();
    assert_eq!(prelogin::generation(), g + 1, "one generation counter");
    assert_eq!(tor::prelogin_generation(), prelogin::generation());

    // An active Tor reports the bridge.
    reset();
    set_preference(Some(Kind::Tor));
    host::activate(Mock::new(Kind::Tor, true, MockDial::Hang), live(), false).unwrap();
    assert!(matches!(tor::transport_state(), TorTransportState::Active(a) if a == bridge::addr().unwrap()));
}


// ── Revival, inbox refusals, the signer across a switch ──────────────────────

/// Kick revives a relay added while the network was not ready (Initialized), never one the
/// network refuses by policy, and never a tracked client that was untracked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kick_revives_deferred_relays_and_skips_refused_and_untracked() {
    use crate::ClientRelayExt;
    use nostr_sdk::prelude::RelayStatus;
    let _s = scene();
    set_preference(Some(Kind::I2p));
    let (relay_server, _, accepted) = recorder().await;
    let client = crate::nostr_client_builder().build();
    client.add_managed_relay("wss://relay.example.com").await.unwrap();
    client.add_managed_relay("wss://refused.example.com").await.unwrap();
    let relay = |url: &'static str| {
        let c = client.clone();
        async move { c.relay(url).await.unwrap().unwrap() }
    };
    assert_eq!(relay("wss://relay.example.com").await.status(), RelayStatus::Initialized, "added while I2P is not up");
    let id = cycle::track(&client);

    // I2P comes up with a twin for one host and I2P-Only on: the other is refused by policy.
    prefs::live().set_config_value(Kind::I2p, Arc::new(i2p_config::I2pConfig { exit: ExitPolicy::Off, ..Default::default() }));
    prefs::live().set_aliases(Arc::new(aliases::AliasTable::from_entries(vec![aliases::AliasEntry {
        host: "relay.example.com".into(),
        twins: [(Kind::I2p, B32.to_string())].into_iter().collect(),
        source: aliases::AliasSource::User,
        check: aliases::AliasCheck::default(),
    }])));
    let mock = Mock::new(Kind::I2p, true, MockDial::To(relay_server));
    host::activate(mock.clone(), live(), false).unwrap();
    assert!(refuses("wss://refused.example.com").is_some());
    cycle::cycle_all(cycle::CycleScope::Kick).await;
    assert!(accepted.load(Ordering::SeqCst) >= 1, "the deferred relay was connected");
    assert_eq!(mock.dialed()[0].0, Route::Twin { host: "relay.example.com".into(), via: B32.into(), port: 443 });
    assert_eq!(
        relay("wss://refused.example.com").await.status(),
        RelayStatus::Initialized,
        "a policy refusal is not retried in a loop"
    );
    assert!(!cycle::revivable(&relay("wss://refused.example.com").await));

    // Untracked: a disconnected client stays down however often Kick runs.
    cycle::untrack(id);
    let dials = mock.dialed().len();
    let r = relay("wss://relay.example.com").await;
    r.disconnect();
    tokio::time::sleep(Duration::from_millis(200)).await;
    cycle::cycle_all(cycle::CycleScope::Kick).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(mock.dialed().len(), dials, "an untracked client is never revived");
    assert_eq!(cycle::tracked_count(), 0);

    // A client of a session that is no longer live drops out on its own.
    let other = crate::nostr_client_builder().build();
    cycle::track(&other);
    assert_eq!(cycle::tracked_count(), 1);
    crate::db::close_database();
    assert_eq!(cycle::tracked_count(), 0, "tracked under a session that is gone");
    client.shutdown().await;
}

/// A recipient whose every inbox relay the network refuses is never sent to through our own
/// write relays: the send fails at once, says why, and nothing is dialed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_inbox_fails_the_send_without_a_fallback() {
    use crate::event_ext::FinalizeUnsignedWithId;
    use crate::ClientRelayExt;
    use nostr_sdk::prelude::{EventBuilder, Keys, Tag, ToBech32};
    struct Seen(Mutex<Vec<String>>);
    impl crate::sending::SendCallback for Seen {
        fn on_refused(&self, _: &str, _: &str, reason: &str) {
            self.0.lock().unwrap().push(reason.to_string());
        }
    }
    let _s = scene();
    let keys = Keys::generate();
    crate::signer::set_test_signer(Some(crate::signer::ActiveSigner::Keys(keys.clone())));
    crate::state::set_my_public_key(keys.public_key());
    let (own_relay, _, accepted) = recorder().await;
    set_preference(Some(Kind::I2p));
    prefs::live().set_config_value(Kind::I2p, Arc::new(i2p_config::I2pConfig { exit: ExitPolicy::Off, ..Default::default() }));
    let mock = Mock::new(Kind::I2p, true, MockDial::To(own_relay));
    host::activate(mock.clone(), live(), false).unwrap();
    let client = crate::nostr_client_builder().build();
    client.add_managed_relay(format!("ws://{B32}").as_str()).await.unwrap();
    crate::state::set_nostr_client(client.clone());

    let them = Keys::generate().public_key();
    crate::inbox_relays::seed_inbox_relays_for_test(them, &["wss://nos.lol", "wss://relay.damus.io"]);
    let t = crate::inbox_relays::resolve_gift_wrap_targets(&client, &them).await;
    assert_eq!(t.refused.as_deref(), Some("I2P-Only is on, so their inbox relays are off."));
    assert!(t.resolved.is_empty() && t.targeted_relays.is_empty(), "our own write relay is no fallback");

    let rumor = EventBuilder::new(nostr_sdk::prelude::Kind::PrivateDirectMessage, "hi")
        .tag(Tag::public_key(them))
        .finalize_unsigned_with_id(keys.public_key());
    let seen = Arc::new(Seen(Mutex::new(Vec::new())));
    let t0 = web_time::Instant::now();
    let r = crate::sending::send_rumor_dm(
        &them.to_bech32().unwrap(),
        "pending-refused",
        rumor,
        &crate::sending::SendConfig::gui(),
        seen.clone(),
    )
    .await;
    assert_eq!(r.err().as_deref(), Some("I2P-Only is on, so their inbox relays are off."));
    assert!(t0.elapsed() < Duration::from_secs(3), "no retry waits on a policy refusal");
    assert_eq!(seen.0.lock().unwrap().as_slice(), ["I2P-Only is on, so their inbox relays are off."]);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(mock.dialed().is_empty(), "nothing was dialed, our own relay included");
    assert_eq!(accepted.load(Ordering::SeqCst), 0);

    // A contact whose list isn't known yet, while I2P-Only keeps the relays it would be found on
    // out of reach: unknown is not absent, so the send fails rather than landing on our own relay.
    client.add_managed_relay("wss://nos.lol").await.unwrap();
    let unknown = Keys::generate().public_key();
    let t = tokio::time::timeout(Duration::from_secs(90), crate::inbox_relays::resolve_gift_wrap_targets(&client, &unknown))
        .await
        .expect("the lookup ends");
    assert_eq!(t.refused.as_deref(), Some("I2P-Only is on, so Vector can't find their inbox relays."));
    assert!(t.resolved.is_empty() && t.targeted_relays.is_empty(), "our own write relay is no fallback");
    assert!(!crate::inbox_relays::is_cached_for_test(&unknown), "a blind lookup is asked again next time");
    client.remove_relay("wss://nos.lol").force().await.unwrap();

    // Clearnet and a contact who reads only an I2P relay: the same, with that reason.
    reset();
    let i2p_only = Keys::generate().public_key();
    crate::inbox_relays::seed_inbox_relays_for_test(i2p_only, &[&format!("ws://{B32}")]);
    let t = crate::inbox_relays::resolve_gift_wrap_targets(&client, &i2p_only).await;
    assert_eq!(t.refused.as_deref(), Some("Their inbox relays are only reachable over I2P."));

    // A list with one reachable relay sends there and skips the rest: the relay is not pooled,
    // so it is connected for this send and the publish lands as it comes up.
    let (inbox_port, _) = signer_relay(Keys::generate(), keys.public_key()).await;
    let mixed = Keys::generate().public_key();
    let reachable = format!("ws://127.0.0.1:{inbox_port}");
    crate::inbox_relays::seed_inbox_relays_for_test(mixed, &[reachable.as_str(), &format!("ws://{B32}")]);
    let t = crate::inbox_relays::resolve_gift_wrap_targets(&client, &mixed).await;
    assert!(t.refused.is_none());
    assert_eq!(t.targeted_relays, vec![reachable]);
    let wrap = {
        use nostr_sdk::prelude::FinalizeEvent;
        EventBuilder::new(nostr_sdk::prelude::Kind::TextNote, "x").finalize(&Keys::generate()).unwrap()
    };
    let out = crate::inbox_relays::publish_gift_wrap_to_targets(&client, &t, &wrap).await.unwrap();
    assert_eq!(out.success.len(), 1, "a transient inbox relay still connecting takes the event: {:?}", out.failed);
    crate::inbox_relays::teardown_gift_wrap_targets(&client, &t).await;

    crate::signer::set_test_signer(None);
    crate::state::clear_my_public_key();
    if let Some(c) = crate::state::take_nostr_client() {
        c.shutdown().await;
    }
}

/// A loopback relay with a NIP-46 signer behind it: EOSE for every REQ, OK for every EVENT, and
/// an answer to `connect` and `get_public_key` sent to `signer`. Counts its open sockets.
async fn signer_relay(signer: nostr_sdk::prelude::Keys, user: nostr_sdk::prelude::PublicKey) -> (u16, Arc<AtomicUsize>) {
    use futures_util::{SinkExt, StreamExt};
    use nostr_sdk::prelude::*;
    use tokio_tungstenite::tungstenite::Message as Ws;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let open = Arc::new(AtomicUsize::new(0));
    let open2 = open.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = l.accept().await else { return };
            let (signer, open) = (signer.clone(), open2.clone());
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else { return };
                open.fetch_add(1, Ordering::SeqCst);
                let mut subs: Vec<SubscriptionId> = Vec::new();
                while let Some(Ok(frame)) = ws.next().await {
                    let text = match frame {
                        Ws::Text(t) => t.to_string(),
                        Ws::Close(_) => break,
                        _ => continue,
                    };
                    let Ok(msg) = ClientMessage::from_json(&text) else { continue };
                    let mut out: Vec<String> = Vec::new();
                    match msg {
                        ClientMessage::Req { subscription_id, .. } => {
                            subs.push(subscription_id.clone().into_owned());
                            out.push(RelayMessage::eose(subscription_id.into_owned()).as_json());
                        }
                        ClientMessage::Event(ev) => {
                            out.push(RelayMessage::ok(ev.id, true, "").as_json());
                            if ev.kind == Kind::NostrConnect {
                                let Ok(plain) = nip44::decrypt(signer.secret_key(), &ev.pubkey, &ev.content) else { continue };
                                let Ok(req) = NostrConnectMessage::from_json(&plain) else { continue };
                                let id = req.id().to_string();
                                let result = match req.to_request() {
                                    Ok(NostrConnectRequest::Connect { .. }) => ResponseResult::Ack,
                                    Ok(NostrConnectRequest::GetPublicKey) => ResponseResult::GetPublicKey(user),
                                    _ => continue,
                                };
                                let reply = NostrConnectMessage::response(id, NostrConnectResponse::with_result(result));
                                let event = NostrConnectEventBuilder::new(ev.pubkey, reply).finalize(&signer).unwrap();
                                for sub in &subs {
                                    out.push(RelayMessage::event(sub.clone(), event.clone()).as_json());
                                }
                            }
                        }
                        _ => {}
                    }
                    for o in out {
                        if ws.send(Ws::Text(o.into())).await.is_err() {
                            break;
                        }
                    }
                }
                open.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    (port, open)
}

async fn settle(what: &str, f: impl Fn() -> bool) {
    let deadline = web_time::Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(web_time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The NIP-46 client is a stock nostr-sdk client on direct sockets under Clearnet. A switch must
/// close every one: installed and bootstrapped, installed while still bootstrapping, and held by
/// a pairing outside the slot. The suspended signer comes back through the restricted port.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nip46_clients_close_on_a_switch() {
    use crate::signer::{self, BunkerConnectionState};
    use nostr_sdk::prelude::{Keys, RelayUrl};
    let _s = scene();
    drop(signer::drain_bunker_state());
    let (signer_keys, user) = (Keys::generate(), Keys::generate().public_key());
    let (port, open) = signer_relay(signer_keys.clone(), user).await;
    let relay = format!("ws://127.0.0.1:{port}");
    let bunker_url = format!("bunker://{}?relay={relay}", signer_keys.public_key().to_hex());
    let open_now = || open.load(Ordering::SeqCst);

    // Installed and bootstrapped, plus a pairing held outside the slot, both on direct sockets.
    let installed = signer::build_bunker_signer(&bunker_url, Keys::generate(), Duration::from_secs(10)).unwrap();
    assert_eq!(signer::prewarm_bunker(&installed).await.unwrap(), user);
    signer::set_bunker_signer(installed);
    let (pairing_nc, _) = signer::build_nostrconnect_session(Keys::generate(), vec![RelayUrl::parse(&relay).unwrap()], Duration::from_secs(120)).unwrap();
    let pairing = signer::track_pairing(&pairing_nc);
    assert!(signer::pairing_pending(), "a pairing outside the slot is pending too");
    let waiting = tokio::spawn(async move { pairing_nc.bunker_uri().await.is_ok() });
    settle("both clients connected", || open_now() == 2).await;

    // The switch: every socket closes, the pairing fails, the signer waits to be rebuilt.
    set_preference(Some(Kind::Tor));
    cycle::cycle_all(cycle::CycleScope::Switch).await;
    settle("every NIP-46 socket closed", || open_now() == 0).await;
    assert!(pairing.aborted());
    assert!(!waiting.await.unwrap(), "the pairing never completes on the old sockets");
    drop(pairing);
    assert!(signer::bunker_signer().is_none() && signer::bunker_suspended());
    assert_eq!(signer::bunker_state(), BunkerConnectionState::Connecting);
    assert!(!signer::pairing_pending());

    // Ready: the signer is rebuilt, bootstrapped and installed, through the restricted port.
    let mock = Mock::new(Kind::Tor, true, MockDial::To(SocketAddr::from(([127, 0, 0, 1], port))));
    host::activate(mock.clone(), live(), false).unwrap();
    cycle::cycle_all(cycle::CycleScope::Kick).await;
    settle("the signer rebuilt", || signer::bunker_signer().is_some()).await;
    assert!(!signer::bunker_suspended());
    assert_eq!(signer::bunker_state(), BunkerConnectionState::Online);
    assert!(
        mock.dialed().iter().any(|(r, lane)| *lane == Lane::Account && matches!(r, Route::Exit { host, port: p } if host == "127.0.0.1" && *p == port)),
        "the rebuilt client rides the network in use: {:?}",
        mock.dialed()
    );
    if let Some(nc) = signer::drain_bunker_state() {
        nc.shutdown().await;
    }
    reset();
    settle("closed", || open_now() == 0).await;

    // Installed while its pairing is still bootstrapping (a clone does the work).
    let (slot_nc, _) = signer::build_nostrconnect_session(Keys::generate(), vec![RelayUrl::parse(&relay).unwrap()], Duration::from_secs(120)).unwrap();
    signer::set_bunker_signer(slot_nc.clone());
    signer::set_bunker_state(BunkerConnectionState::Connecting);
    let bootstrapping = tokio::spawn(async move { slot_nc.bunker_uri().await.is_ok() });
    settle("the pairing connected", || open_now() == 1).await;
    set_preference(Some(Kind::Tor));
    cycle::cycle_all(cycle::CycleScope::Switch).await;
    settle("its socket closed", || open_now() == 0).await;
    assert!(!bootstrapping.await.unwrap());
    assert!(signer::bunker_signer().is_none(), "out of the slot");
    assert!(!signer::bunker_suspended(), "a pairing with no URI yet has nothing to rebuild");
    assert_eq!(signer::bunker_state(), BunkerConnectionState::Offline);
    drop(signer::drain_bunker_state());
}

/// A connection still in its SOCKS handshake has proved no ticket yet: an epoch bump leaves it
/// alone, so a client presenting a fresh ticket right after a bump is never reset, and the
/// registry empties once the client leaves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bump_spares_connections_still_in_their_handshake() {
    let _s = scene();
    set_preference(Some(Kind::Tor));
    let (addr, _, accepted) = recorder().await;
    host::activate(Mock::new(Kind::Tor, true, MockDial::To(addr)), live(), false).unwrap();
    let before = bridge::open_connections();
    let mut c = TcpStream::connect(bridge::addr().unwrap()).await.unwrap();
    settle("the bridge took the connection", || bridge::open_connections() == before + 1).await;
    bump_epoch();
    let t = Ticket { owner: live(), epoch: epoch(), lane: Lane::Shared };
    c.write_all(&[5, 1, 2]).await.unwrap();
    let mut r = [0u8; 2];
    c.read_exact(&mut r).await.expect("not reset by the bump");
    let (user, pass) = bridge::credentials(&t);
    let mut auth = vec![1, user.len() as u8];
    auth.extend_from_slice(user.as_bytes());
    auth.push(pass.len() as u8);
    auth.extend_from_slice(pass.as_bytes());
    c.write_all(&auth).await.unwrap();
    c.read_exact(&mut r).await.unwrap();
    assert_eq!(r, [1, 0]);
    c.write_all(&connect_domain("relay.example.com", 443)).await.unwrap();
    assert_eq!(reply_code(&mut c).await, 0x00, "the fresh ticket is served");
    settle("dialed", || accepted.load(Ordering::SeqCst) == 1).await;
    host::deactivate(true).await;
    settle("the registry let it go", || bridge::open_connections() == before).await;
    drop(c);
}

#[test]
fn a_sam_password_never_prints() {
    let cfg = i2p_config::I2pConfig { sam_user: Some("u".into()), sam_password: Some("hunter2".into()), ..Default::default() };
    assert!(!format!("{cfg:?}").contains("hunter2"));
    let json = cfg.to_json();
    assert!(json.contains("hunter2"), "saved like any other device setting");
    assert_eq!(json.capacity(), json.len(), "written once, at its exact size");
    assert_eq!(i2p_config::I2pConfig::parse(Some(json.as_str())), cfg);
    let mut carried = prelogin::PreloginChoice::with_opts(Kind::I2p, serde_json::json!({ "sam_port": 7777 }));
    carried.auth = Some(prelogin::Credentials { user: "u".into(), password: "hunter2".into() });
    assert!(!format!("{carried:?}").contains("hunter2"), "a carried login never prints");
    assert!(!serde_json::to_string(&carried).unwrap().contains("hunter2"), "nor serialises");
    let mut v = serde_json::json!({ "sam_password": "hunter2", "list": ["hunter2"] });
    prefs::scrub_json(&mut v);
    assert!(!v.to_string().contains("hunter2"));
}

/// The welcome screen's SAM login reaches the account that commits, typed, never over its own.
#[test]
fn a_carried_sam_login_fills_only_an_unset_one() {
    let _serial = test_lock();
    crate::db::close_database();
    crate::db::clear_current_account_in_memory();
    let mut choice = prelogin::PreloginChoice::with_opts(Kind::I2p, serde_json::json!({ "sam_port": 7777 }));
    choice.auth = Some(prelogin::Credentials { user: "vec".into(), password: "s3cret".into() });
    prelogin::arm(choice.clone());
    let cfg = |c: KindConfig| c.downcast_ref::<i2p_config::I2pConfig>().unwrap().clone();
    let before = cfg(prelogin::config_for(Kind::I2p));
    assert_eq!((before.sam_port, before.sam_user.as_deref(), before.sam_password.as_deref()), (7777, Some("vec"), Some("s3cret")));
    let acct = test_account();
    crate::db::init_database(&acct).unwrap();
    prelogin::commit();
    let saved = cfg(prefs::config(Kind::I2p));
    assert_eq!((saved.sam_port, saved.sam_user.as_deref(), saved.sam_password.as_deref()), (7777, Some("vec"), Some("s3cret")));

    crate::db::close_database();
    let acct = test_account();
    crate::db::init_database(&acct).unwrap();
    let own = i2p_config::I2pConfig { sam_user: Some("mine".into()), sam_password: Some("own".into()), ..Default::default() };
    prefs::persist_config(Kind::I2p, &own.to_json()).unwrap();
    prelogin::arm(choice);
    prelogin::commit();
    let stored = prefs::read_stored(&crate::db::get_db_connection_guard_static().unwrap());
    let kept = cfg(stored.configs[&Kind::I2p].clone());
    assert_eq!((kept.sam_user.as_deref(), kept.sam_password.as_deref()), (Some("mine"), Some("own")), "the account's own login stays");
    assert_eq!(kept.sam_port, i2p_config::DEFAULT_SAM_PORT, "and its own router");
    crate::db::close_database();
    prelogin::disarm();
}


/// A loopback relay that answers every REQ with EOSE (it stores nothing) and every EVENT with
/// OK, and pushes each event the test sends to every subscription it holds. `forget` drops what
/// it holds, as a relay that lost a REQ does.
struct PushRelay {
    port: u16,
    push: tokio::sync::broadcast::Sender<String>,
    forget: Arc<tokio::sync::Notify>,
    reqs: Arc<Mutex<Vec<String>>>,
    events: Arc<AtomicUsize>,
}

async fn push_relay() -> PushRelay {
    use futures_util::{SinkExt, StreamExt};
    use nostr_sdk::prelude::*;
    use tokio_tungstenite::tungstenite::Message as Ws;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let (push, _) = tokio::sync::broadcast::channel::<String>(16);
    let forget = Arc::new(tokio::sync::Notify::new());
    let reqs = Arc::new(Mutex::new(Vec::new()));
    let published = Arc::new(AtomicUsize::new(0));
    let (push2, forget2, reqs2, published2) = (push.clone(), forget.clone(), reqs.clone(), published.clone());
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = l.accept().await else { return };
            let (mut events, forget, reqs, published) = (push2.subscribe(), forget2.clone(), reqs2.clone(), published2.clone());
            tokio::spawn(async move {
                let Ok(ws) = tokio_tungstenite::accept_async(tcp).await else { return };
                let (mut sink, mut stream) = ws.split();
                let mut subs: Vec<SubscriptionId> = Vec::new();
                loop {
                    tokio::select! {
                        frame = stream.next() => {
                            let text = match frame {
                                Some(Ok(Ws::Text(t))) => t,
                                Some(Ok(Ws::Close(_))) | Some(Err(_)) | None => break,
                                _ => continue,
                            };
                            match ClientMessage::from_json(text.as_str()) {
                                Ok(ClientMessage::Req { subscription_id, .. }) => {
                                    let id = subscription_id.into_owned();
                                    reqs.lock().unwrap().push(id.to_string());
                                    if !subs.contains(&id) {
                                        subs.push(id.clone());
                                    }
                                    let _ = sink.send(Ws::Text(RelayMessage::eose(id).as_json().into())).await;
                                }
                                Ok(ClientMessage::Event(ev)) => {
                                    published.fetch_add(1, Ordering::SeqCst);
                                    let _ = sink.send(Ws::Text(RelayMessage::ok(ev.id, true, "").as_json().into())).await;
                                }
                                _ => {}
                            }
                        }
                        _ = forget.notified() => subs.clear(),
                        ev = events.recv() => {
                            let Ok(ev) = ev else { break };
                            let event = Event::from_json(&ev).unwrap();
                            for s in &subs {
                                let _ = sink.send(Ws::Text(RelayMessage::event(s.clone(), event.clone()).as_json().into())).await;
                            }
                        }
                    }
                }
            });
        }
    });
    PushRelay { port, push, forget, reqs, events: published }
}

/// Publish a fresh note through `relay` and report whether `client` hears it on a live sub.
async fn delivered(client: &nostr_sdk::prelude::Client, relay: &PushRelay) -> bool {
    use futures_util::StreamExt;
    use nostr_sdk::prelude::*;
    let mut notes = client.notifications();
    let ev = EventBuilder::new(nostr_sdk::prelude::Kind::TextNote, "live").finalize(&Keys::generate()).unwrap();
    let _ = relay.push.send(ev.as_json());
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(n) = notes.next().await {
            if matches!(&n, ClientNotification::Event { event, .. } if event.id == ev.id) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}

/// Live subscriptions made while the network was still starting reach the relay once it
/// connects, and a relay that lost them gets them back from the per-relay re-assert the shell
/// runs on every connect, under the same id and only on that relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_subs_reach_a_relay_that_connects_after_them() {
    use crate::ClientRelayExt;
    use nostr_sdk::prelude::{Filter, RelayStatus, RelayUrl};
    let _s = scene();
    set_preference(Some(Kind::Tor));
    let relay = push_relay().await;
    let url = format!("ws://127.0.0.1:{}", relay.port);
    let client = crate::nostr_client_builder().build();
    client.add_managed_relay(url.as_str()).await.unwrap();
    let r = client.relay(url.as_str()).await.unwrap().unwrap();
    let _ = r.try_connect().timeout(Duration::from_secs(2)).await;
    assert_eq!(r.status(), RelayStatus::Terminated, "refused while the network starts");

    let out = client.subscribe(Filter::new().kind(nostr_sdk::prelude::Kind::TextNote).limit(0)).await.unwrap();
    assert_eq!(out.success.len(), 1, "accepted for a relay that is not up yet");
    let id = out.value.to_string();

    let tracked = cycle::track(&client);
    host::activate(Mock::new(Kind::Tor, true, MockDial::To(SocketAddr::from(([127, 0, 0, 1], relay.port)))), live(), false).unwrap();
    cycle::cycle_all(cycle::CycleScope::Kick).await;
    cycle::untrack(tracked);
    settle("connected", || r.status() == RelayStatus::Connected).await;
    assert!(delivered(&client, &relay).await, "the sub made before the connect is live");

    relay.forget.notify_waiters();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!delivered(&client, &relay).await, "a relay that lost the REQ stays silent");
    let before = relay.reqs.lock().unwrap().len();
    crate::resubscribe_relay_after_reconnect(&client, &RelayUrl::parse(&url).unwrap()).await;
    settle("the REQ was re-sent", || relay.reqs.lock().unwrap().len() > before).await;
    assert_eq!(relay.reqs.lock().unwrap().last(), Some(&id), "the same subscription id");
    assert!(delivered(&client, &relay).await, "live again after the re-assert");
    client.shutdown().await;
}

/// A start that failed is retried only by the user (Retry or another network): every transfer
/// fails at once with its reason, never after a start budget of waiting for nothing.
#[tokio::test]
async fn a_failed_start_ends_every_wait_at_once() {
    let _s = scene();
    set_preference(Some(Kind::Tor));
    let failed = Mock::new(Kind::Tor, false, MockDial::Hang);
    *failed.reason.lock().unwrap() = status::Reason::new("tor_failed", "Tor timed out after 120s.");
    host::activate(failed.clone(), live(), false).unwrap();
    let e = crate::net::HttpError::Refused(blocked_error(Kind::Tor));
    assert!(!e.is_transient(), "nothing restarts a failed start on its own");
    let t0 = web_time::Instant::now();
    assert_eq!(crate::net::wait_after(&e).await.unwrap_err(), "Tor timed out after 120s.");
    assert_eq!(wait_ready(Duration::from_secs(120)).await.unwrap_err(), "Tor timed out after 120s.");
    assert!(t0.elapsed() < Duration::from_millis(300), "took {:?}", t0.elapsed());

    // Still starting: a wait, then the network's own words rather than a generic line.
    *failed.reason.lock().unwrap() = status::Reason::new("router_unreachable", "Can't reach your I2P router at 127.0.0.1:7656.");
    assert!(crate::net::HttpError::Refused(blocked_error(Kind::Tor)).is_transient());
    assert_eq!(wait_ready(Duration::from_millis(200)).await.unwrap_err(), "Can't reach your I2P router at 127.0.0.1:7656.");
    drop(host::uninstall());

    #[cfg(all(feature = "tor", not(target_arch = "wasm32")))]
    {
        crate::tor::set_last_bootstrap_error("Tor timed out after 120s.");
        assert_eq!(status::view().phase, status::Phase::Failed);
        assert!(!crate::net::HttpError::Refused(blocked_error(Kind::Tor)).is_transient());
        let t0 = web_time::Instant::now();
        assert_eq!(wait_ready(Duration::from_secs(120)).await.unwrap_err(), "Tor timed out after 120s.");
        assert!(t0.elapsed() < Duration::from_millis(300));
        crate::tor::clear_last_bootstrap_error();
        assert!(crate::net::HttpError::Refused(blocked_error(Kind::Tor)).is_transient(), "a new attempt clears it");
    }
}

/// A recipient's inbox lookup that couldn't reach the relays asked (the network down, or every
/// exit failing while our own relay answers) is unknown, not "no inbox relays": never cached,
/// never a fallback to our own relays, and the failed send says why.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_inbox_lookup_never_falls_back_to_our_relays() {
    use crate::ClientRelayExt;
    use nostr_sdk::prelude::{Keys, RelayStatus, ToBech32};
    struct Why(Mutex<Vec<String>>);
    impl crate::sending::SendCallback for Why {
        fn on_failed_reason(&self, _: &str, _: &str, reason: &str) {
            self.0.lock().unwrap().push(reason.to_string());
        }
    }
    let _s = scene();
    let me = Keys::generate();
    crate::signer::set_test_signer(Some(crate::signer::ActiveSigner::Keys(me.clone())));
    crate::state::set_my_public_key(me.public_key());
    let own = push_relay().await;
    let own_addr = SocketAddr::from(([127, 0, 0, 1], own.port));
    for op in [Op::RelayConnect, Op::RelayRequest, Op::BestEffort] {
        budget::override_for_test(op, Some(Duration::from_millis(1)));
    }
    budget::override_for_test(Op::ReadyWait, Some(Duration::from_millis(300)));
    set_preference(Some(Kind::I2p));
    let mock = Mock::new(Kind::I2p, false, MockDial::To(own_addr));
    let id = host::activate(mock.clone(), live(), false).unwrap().id();
    let client = crate::nostr_client_builder().build();
    client.add_managed_relay("ws://own.example.com").await.unwrap();
    crate::state::set_nostr_client(client.clone());
    let config = crate::sending::SendConfig { max_send_attempts: 2, retry_delay: Duration::from_millis(100), ..Default::default() };

    // The router is down: the send waits its ready budget, then fails with the network's words.
    let them = Keys::generate().public_key();
    let why = Arc::new(Why(Mutex::new(Vec::new())));
    let r = crate::sending::send_dm(&them.to_bech32().unwrap(), "hi", None, &config, why.clone()).await;
    assert_eq!(r.err().as_deref(), Some("Can't reach your I2P router at 127.0.0.1:7656."));
    assert_eq!(why.0.lock().unwrap().as_slice(), ["Can't reach your I2P router at 127.0.0.1:7656."], "the row says why");
    assert!(!crate::inbox_relays::is_cached_for_test(&them), "a lookup the network kept blind is not cached");
    assert_eq!(own.events.load(Ordering::SeqCst), 0);

    // I2P up, our own relay answering, the other relay asked out of reach through every exit:
    // unknown, so nothing lands on our own relay, and the next attempt looks again.
    mock.ready.store(true, Ordering::SeqCst);
    host::notify(host::TransportEvent::Ready { kind: Kind::I2p, instance: id, owner: live(), recovered: false });
    let ours = client.relay("ws://own.example.com").await.unwrap().unwrap();
    ours.try_connect().timeout(Duration::from_secs(5)).await.unwrap();
    assert_eq!(ours.status(), RelayStatus::Connected);
    *mock.dial.lock().unwrap() = MockDial::Refuse(ConnectError::NoExit);
    client.add_managed_relay("ws://far.example.com").await.unwrap();
    let far = client.relay("ws://far.example.com").await.unwrap().unwrap();
    let _ = far.try_connect().timeout(Duration::from_secs(5)).await;
    assert_ne!(far.status(), RelayStatus::Connected);
    let why = Arc::new(Why(Mutex::new(Vec::new())));
    let r = crate::sending::send_dm(&them.to_bech32().unwrap(), "hi", None, &config, why.clone()).await;
    assert!(r.is_err());
    assert_eq!(why.0.lock().unwrap().as_slice(), ["No outproxy is reachable right now."]);
    assert!(!crate::inbox_relays::is_cached_for_test(&them));
    assert_eq!(own.events.load(Ordering::SeqCst), 0, "never published to our own relay");

    // Every relay asked answered: a contact with no list is absent, and NIP-17 falls back.
    client.remove_relay("ws://far.example.com").force().await.unwrap();
    let r = crate::sending::send_dm(&them.to_bech32().unwrap(), "hi", None, &config, why.clone()).await;
    assert!(r.is_ok(), "{r:?}");
    assert!(crate::inbox_relays::is_cached_for_test(&them));
    settle("published to our relay", || own.events.load(Ordering::SeqCst) == 1).await;

    crate::signer::set_test_signer(None);
    crate::state::clear_my_public_key();
    if let Some(c) = crate::state::take_nostr_client() {
        c.shutdown().await;
    }
}

/// A settings row that is there but can't be read never turns I2P-Only off or brings back an
/// outproxy: only an explicit `"allow"` beside an outproxy list it can read opens the exits.
#[test]
fn an_unreadable_i2p_row_is_i2p_only() {
    use i2p_config::I2pConfig;
    let p = |j: &str| I2pConfig::parse(Some(j));
    assert_eq!(p(r#"{"exit":"off"}"#).exit, ExitPolicy::Off);
    let newer = p(r#"{"sam_port":7700,"exit":"strict","outproxies":null}"#);
    assert_eq!((newer.exit, newer.sam_port), (ExitPolicy::Off, 7700), "a policy a newer build wrote reads as the strictest");
    let mangled = p(r#"{"sam_port":7656,"exit":"allow","outproxies":[{"id":1}]}"#);
    assert_eq!((mangled.exit, mangled.outproxy_list()), (ExitPolicy::Off, Vec::new()), "outproxies it can't name are none");
    let login = p(r#"{"sam_port":"x","sam_user":"u","sam_password":"pw","exit":"allow","outproxies":null}"#);
    assert_eq!((login.exit, login.sam_port, login.outproxy_list()), (ExitPolicy::Allow, 7656, i2p_config::defaults()), "an explicit allow with a readable list stays");
    assert_eq!((login.sam_user.as_deref(), login.sam_password.as_deref()), (Some("u"), Some("pw")), "a readable login is kept");
    assert_eq!(p("garbage").exit, ExitPolicy::Off);
    assert_eq!(I2pConfig::unreadable().exit, ExitPolicy::Off);
    let extra = p(r#"{"sam_port":7656,"sam_user":null,"sam_password":null,"exit":"allow","outproxies":null,"later":true}"#);
    assert_eq!(extra, I2pConfig::default(), "a field this build doesn't know is ignored, not a broken row");
    let stored = prefs::read_stored(&settings_db(&[(prefs::KEY_I2P_CONFIG, r#"{"sam_port":7656,"exit":"strict"}"#)]));
    assert_eq!(stored.configs[&Kind::I2p].downcast_ref::<I2pConfig>().unwrap().exit, ExitPolicy::Off);
}

/// Loading the account the session already holds (a webview reload, Android's background sync)
/// never loosens it: a network choice or I2P-Only whose save failed stays until restart, as the
/// user was told; a stricter row still raises it.
#[test]
fn reloading_the_same_account_never_loosens() {
    use i2p_config::I2pConfig;
    let _s = scene();
    crate::db::close_database();
    let acct = test_account();
    crate::db::init_database(&acct).unwrap();
    assert_eq!(preference(), Some(Kind::Clearnet));
    let session = crate::db::live_session();

    // Applied in memory, never saved.
    set_preference(Some(Kind::Tor));
    prefs::of(&session).set_config_value(Kind::I2p, Arc::new(I2pConfig { exit: ExitPolicy::Off, ..Default::default() }));
    crate::db::init_database(&acct).unwrap();
    assert_eq!(crate::db::live_session_id(), session.id(), "the same session, rebound");
    assert_eq!(preference(), Some(Kind::Tor), "an unsaved switch survives the reload");
    assert_eq!(i2p_config::current().exit, ExitPolicy::Off, "and an unsaved I2P-Only");

    // A stricter row raises a looser session.
    set_preference(Some(Kind::Clearnet));
    prefs::of(&session).set_config_value(Kind::I2p, Arc::new(I2pConfig::default()));
    prefs::persist_kind(Kind::I2p).unwrap();
    prefs::persist_config(Kind::I2p, &I2pConfig { exit: ExitPolicy::Off, ..Default::default() }.to_json()).unwrap();
    crate::db::init_database(&acct).unwrap();
    assert_eq!(preference(), Some(Kind::I2p));
    assert_eq!(i2p_config::current().exit, ExitPolicy::Off);

    assert_eq!(prefs::stricter_kind(Some(Kind::I2p), None), None, "an unreadable row is the strictest");
    assert_eq!(prefs::stricter_kind(Some(Kind::Tor), Some(Kind::I2p)), Some(Kind::Tor), "the session's own choice between two networks");

    // Another account loads its own settings in full.
    let other = test_account();
    crate::db::init_database(&other).unwrap();
    assert_ne!(crate::db::live_session_id(), session.id());
    assert_eq!(preference(), Some(Kind::Clearnet));
    assert_eq!(i2p_config::current().exit, ExitPolicy::Allow);
    crate::db::close_database();
}

/// An instance of a network the live account didn't choose carries nothing, so installing or
/// removing it never cuts the sockets of the one in use.
#[test]
fn an_instance_nobody_rides_moves_no_epoch() {
    let _s = scene();
    let e0 = epoch();
    let other = Mock::new(Kind::Tor, true, MockDial::Hang);
    let a = host::activate(other, live(), true).unwrap();
    host::notify(host::TransportEvent::Ready { kind: Kind::Tor, instance: a.id(), owner: live(), recovered: false });
    assert_eq!(epoch(), e0, "Clearnet in use: a late Tor start lands and moves nothing");
    drop(host::uninstall());
    assert_eq!(epoch(), e0);

    set_preference(Some(Kind::Tor));
    let e1 = epoch();
    host::activate(Mock::new(Kind::Tor, true, MockDial::Hang), live(), false).unwrap();
    assert!(epoch() > e1, "the chosen network's instance does");
    let e2 = epoch();
    drop(host::uninstall());
    assert!(epoch() > e2);
}

/// The stock NIP-46 client connects within the budget of the network in use when it is built
/// (it is rebuilt on every switch), as before: Clearnet 15 s, Tor 60 s.
#[test]
fn the_signer_connects_on_the_budget_of_the_network_in_use() {
    let _s = scene();
    let timeout = || {
        let opts = format!("{:?}", ws::transport_relay_options());
        opts.split("connect_timeout: ").nth(1).and_then(|r| r.split([',', ' ']).next()).unwrap_or_default().to_string()
    };
    assert_eq!(timeout(), "15s", "Clearnet");
    set_preference(Some(Kind::Tor));
    assert_eq!(timeout(), "60s", "Tor");
    set_preference(Some(Kind::I2p));
    assert_eq!(timeout(), "90s", "I2P");
    set_preference(None);
    assert_eq!(timeout(), format!("{}s", Duration::from_secs(15).max(budget::highest_compiled_floor(Op::RelayConnect)).as_secs()), "Unknown");
}

/// A fetch that was allowed only because the account was off Clearnet (a stranger's relays for
/// an embed, a page for a preview) never goes direct, whenever the switch to Clearnet lands; an
/// account whose network isn't loaded yet dials no stranger at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fetch_allowed_only_off_clearnet_never_goes_direct() {
    use crate::ClientRelayExt;
    let _s = scene();
    let (server, hits) = http_server(b"page", false).await;
    let relay = push_relay().await;

    set_preference(None);
    assert!(!crate::nostr_embed::may_dial_strangers(), "Unknown: nothing says the address is hidden");
    set_preference(Some(Kind::Tor));
    assert!(crate::nostr_embed::may_dial_strangers());

    // Decided under Tor, sent after the switch to Clearnet.
    let http = crate::net::build_http_client(Duration::from_secs(5)).unwrap().without_direct();
    let shard = crate::transport::ws::apply_transport_without_direct(nostr_sdk::prelude::ClientBuilder::new(), Lane::Shared).build();
    set_preference(Some(Kind::Clearnet));
    assert!(!crate::nostr_embed::may_dial_strangers(), "Clearnet with the privacy proxy on");
    let r = http.get(format!("http://{server}/")).send().await;
    assert!(matches!(r, Err(crate::net::HttpError::Refused(ConnectError::Stale))), "{:?}", r.err());
    let url = format!("ws://127.0.0.1:{}", relay.port);
    shard.add_managed_relay(url.as_str()).await.unwrap();
    let r = shard.relay(url.as_str()).await.unwrap().unwrap();
    let _ = r.try_connect().timeout(Duration::from_secs(2)).await;
    assert_ne!(r.status(), nostr_sdk::prelude::RelayStatus::Connected);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 0, "no socket reached the page");
    assert!(relay.reqs.lock().unwrap().is_empty(), "nor the relay");

    // Through the network it was decided under, it goes as usual.
    set_preference(Some(Kind::Tor));
    host::activate(Mock::new(Kind::Tor, true, MockDial::To(server)), live(), false).unwrap();
    let body = http.get(format!("http://{server}/")).send().await.unwrap().bytes().await.unwrap();
    assert_eq!(&body[..], b"page");
    shard.shutdown().await;
}
