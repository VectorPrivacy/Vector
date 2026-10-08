//! Tor through the transport meta-glue, against the live Tor network.
//!
//! Ignored, and skipped unless `VECTOR_TOR_LIVE=1`:
//! `VECTOR_TOR_LIVE=1 cargo test -p vector-core --features tor --test tor_live -- --ignored --nocapture --test-threads=1`
//!
//! Throwaway keys, temporary directories, REQs only. Prints timings, never an address.

#![cfg(all(feature = "tor", not(target_arch = "wasm32")))]

use std::time::{Duration, Instant};

use nostr_sdk::prelude::*;
use vector_core::transport::{self, Kind, TransportState};

fn live() -> bool {
    std::env::var("VECTOR_TOR_LIVE").is_ok_and(|v| v == "1")
}

fn step(label: &str, since: Instant) {
    println!("[tor-live] {label}: {:.2}s", since.elapsed().as_secs_f64());
}

/// A REQ for one note, answered by EOSE from `relay`. A host stays pinned to its circuit, and an
/// exit can keep failing it (a DNS lookup at the exit, a dead circuit), so a failed attempt takes
/// a new circuit before the next, as a user's New circuit would.
async fn req_to_eose_tries(client: &Client, relay: &str, tries: usize) -> Result<(usize, Duration), String> {
    client.add_relay(relay).await.map_err(|e| e.to_string())?;
    let t0 = Instant::now();
    let relay_url = RelayUrl::parse(relay).map_err(|e| e.to_string())?;
    let r = client.relay(&relay_url).await.map_err(|e| e.to_string())?.ok_or("relay missing")?;
    let mut last = String::new();
    for attempt in 1..=tries {
        match r.try_connect().timeout(Duration::from_secs(90)).await {
            Ok(()) => {
                let events = client
                    .fetch_events(ReqTarget::single(relay_url.clone(), [Filter::new().kind(nostr_sdk::prelude::Kind::TextNote).limit(1)]))
                    .timeout(Duration::from_secs(60))
                    .await
                    .map_err(|e| format!("fetch: {e}"))?;
                if attempt > 1 {
                    println!("[tor-live]   {relay} connected on attempt {attempt}");
                }
                return Ok((events.len(), t0.elapsed()));
            }
            Err(e) => {
                last = format!("connect: {e}");
                println!("[tor-live]   {relay} attempt {attempt} failed ({last}); new circuit");
                vector_core::tor::rotate_circuits();
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
    Err(last)
}

/// Exits fail single streams often enough (measured: about half the streams to nos.lol, bridge
/// and arti alike, in `tor_bridge_matches_direct_arti`) that each step gets five fresh circuits.
async fn req_to_eose(client: &Client, relay: &str) -> Result<(usize, Duration), String> {
    req_to_eose_tries(client, relay, 5).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn tor_through_the_meta_glue() {
    if !live() {
        eprintln!("[tor-live] skipped: set VECTOR_TOR_LIVE=1");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    vector_core::db::set_app_data_dir(root.path().to_path_buf());
    let npub = Keys::generate().public_key().to_bech32().unwrap();
    std::fs::create_dir_all(root.path().join(&npub)).unwrap();
    vector_core::db::set_current_account(npub.clone()).unwrap();
    vector_core::db::init_database(&npub).unwrap();
    assert_eq!(transport::state(), TransportState::Clearnet, "a fresh account starts on Clearnet");

    // Clearnet baseline: the Direct path, byte for byte the default transport's.
    let direct = vector_core::nostr_client_builder().build();
    let t0 = Instant::now();
    match req_to_eose(&direct, "wss://nos.lol").await {
        Ok((n, took)) => println!("[tor-live] clearnet REQ->EOSE nos.lol: {:.2}s ({n} event)", took.as_secs_f64()),
        Err(e) => println!("[tor-live] clearnet REQ failed (not a Tor regression): {e}"),
    }
    step("clearnet total", t0);

    // Tor chosen and not up: blocked in process, the direct socket closed by the switch.
    transport::prefs::persist_kind(Kind::Tor).unwrap();
    transport::set_preference(Some(Kind::Tor));
    assert!(matches!(transport::state(), TransportState::RequiredButInactive { kind: Kind::Tor }));
    let t0 = Instant::now();
    let refused = vector_core::net::build_http_client(Duration::from_secs(10)).unwrap().get("https://nos.lol/").send().await;
    assert!(refused.is_err() && t0.elapsed() < Duration::from_millis(500), "refused at once while Tor starts");
    let blocked = vector_core::nostr_client_builder().build();
    assert!(req_to_eose_tries(&blocked, "wss://relay.damus.io", 1).await.is_err(), "no relay connect before Tor is up");
    blocked.shutdown().await;

    // Bootstrap through the new path (bootstrap, then install in the host).
    let tor_dir = root.path().join(&npub).join("tor");
    let (state_dir, cache_dir) = (tor_dir.join("state"), tor_dir.join("cache"));
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::create_dir_all(&cache_dir).unwrap();
    let t0 = Instant::now();
    let svc = vector_core::tor::TorService::start(state_dir.clone(), cache_dir.clone(), &[]).await.expect("bootstrap");
    step("tor bootstrap (cold cache)", t0);
    assert_eq!(transport::state(), TransportState::Active { kind: Kind::Tor });
    assert!(vector_core::tor::is_active());
    let bridge = transport::bridge::addr().unwrap();
    assert!(matches!(vector_core::tor::transport_state(), vector_core::tor::TorTransportState::Active(a) if a == bridge));

    // A relay through the bridge, isolated per host.
    vector_core::tor::set_multi_circuit(true);
    let client = vector_core::nostr_client_builder().build();
    let (n, took) = req_to_eose(&client, "wss://nos.lol").await.expect("REQ->EOSE over Tor");
    println!("[tor-live] tor REQ->EOSE nos.lol: {:.2}s ({n} event)", took.as_secs_f64());

    // HTTPS through the shared client: the exit says it is Tor.
    let t0 = Instant::now();
    let http = vector_core::net::shared_http_client();
    let mut resp = None;
    for attempt in 1..=5 {
        match http.get("https://check.torproject.org/api/ip").send().await {
            Ok(r) => {
                resp = Some(r);
                break;
            }
            Err(e) => println!("[tor-live]   HTTPS attempt {attempt} failed: {e}"),
        }
    }
    let resp = resp.expect("HTTPS over Tor");
    let body: serde_json::Value = resp.json().await.expect("json");
    step("tor HTTPS GET check.torproject.org", t0);
    assert_eq!(body.get("IsTor").and_then(|v| v.as_bool()), Some(true), "the request left through a Tor exit");
    println!("[tor-live] exit confirms Tor: {}", body["IsTor"]);

    // Each host rides its own circuit.
    let (circuits, hosts) = vector_core::tor::active_circuits();
    println!("[tor-live] multi-circuit: {circuits} circuit(s) for {hosts} host(s)");
    assert!(hosts >= 2 && circuits >= 2, "two hosts, two circuits");

    // New circuit: fresh tokens, the relay reconnects onto a new circuit.
    let ep = transport::epoch();
    vector_core::tor::rotate_circuits();
    let t0 = Instant::now();
    let (n, _) = req_to_eose(&client, "wss://relay.primal.net").await.expect("REQ after a new circuit");
    step(&format!("tor REQ->EOSE relay.primal.net after rotate ({n} event)"), t0);
    assert_eq!(transport::epoch(), ep, "a new circuit never moves the epoch");

    // Another network's name is refused before any Tor connect.
    let t0 = Instant::now();
    let b32 = "nostrajmjieip3dqgeefsgpydy3bbshe3o32z65dwkssl7qxkn5a.b32.i2p";
    let e = http.get(format!("http://{b32}/")).send().await.err().expect("refused");
    assert!(e.to_string().contains("only reachable over I2P"), "{e}");
    assert!(t0.elapsed() < Duration::from_millis(200));

    // Stop with join releases the state lock: the same dirs start again.
    client.shutdown().await;
    direct.shutdown().await;
    let t0 = Instant::now();
    svc.stop_and_join().await;
    drop(svc);
    assert!(!vector_core::tor::is_active());
    assert!(matches!(transport::state(), TransportState::RequiredButInactive { kind: Kind::Tor }), "stopped Tor stays blocked");
    step("stop_and_join", t0);
    let t0 = Instant::now();
    let svc = vector_core::tor::TorService::start(state_dir, cache_dir, &[]).await.expect("restart on the same dirs");
    step("tor restart (warm cache)", t0);
    let again = vector_core::nostr_client_builder().build();
    let (n, took) = req_to_eose(&again, "wss://nos.lol").await.expect("REQ after restart");
    println!("[tor-live] tor REQ->EOSE nos.lol after restart: {:.2}s ({n} event)", took.as_secs_f64());
    again.shutdown().await;
    svc.stop_and_join().await;
}

/// The bridge path against arti called directly, on ONE arti client (the installed service's),
/// a fresh circuit for every attempt and the arm order alternating each round: the same success
/// rate means a failing host is the network's doing, not the bridge's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn tor_bridge_matches_direct_arti() {
    if !live() {
        return;
    }
    use arti_client::StreamPrefs;
    let root = tempfile::tempdir().unwrap();
    vector_core::db::set_app_data_dir(root.path().to_path_buf());
    let npub = Keys::generate().public_key().to_bech32().unwrap();
    std::fs::create_dir_all(root.path().join(&npub)).unwrap();
    vector_core::db::set_current_account(npub.clone()).unwrap();
    vector_core::db::init_database(&npub).unwrap();
    transport::set_preference(Some(Kind::Tor));
    let dir = root.path().join("bridge-tor");
    let svc = vector_core::tor::TorService::start(dir.join("state"), dir.join("cache"), &[]).await.unwrap();
    let arti = svc.arti();

    let hosts: Vec<String> = std::env::var("VECTOR_TOR_HOSTS")
        .unwrap_or_else(|_| "nos.lol,relay.damus.io,relay.primal.net".into())
        .split(',')
        .map(str::to_string)
        .collect();
    let rounds: usize = std::env::var("VECTOR_TOR_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(6);
    let mut totals: Vec<(usize, usize)> = vec![(0, 0); hosts.len()];
    for i in 0..rounds {
        for (h, host) in hosts.iter().enumerate() {
            let bridge = async {
                vector_core::tor::rotate_circuits();
                let t = transport::Ticket { owner: vector_core::db::live_session_id(), epoch: transport::epoch(), lane: transport::Lane::Account };
                let (user, pass) = transport::bridge::credentials(&t);
                let t0 = Instant::now();
                let r = tokio::time::timeout(
                    Duration::from_secs(60),
                    tokio_socks::tcp::Socks5Stream::connect_with_password(transport::bridge::addr().unwrap(), (host.as_str(), 443), &user, &pass),
                )
                .await;
                (matches!(r, Ok(Ok(_))), t0.elapsed())
            };
            let direct = async {
                let mut prefs = StreamPrefs::new();
                prefs.set_isolation(tor_circmgr::isolation::IsolationToken::new());
                let t0 = Instant::now();
                let r = tokio::time::timeout(Duration::from_secs(60), arti.connect_with_prefs((host.as_str(), 443), &prefs)).await;
                (matches!(r, Ok(Ok(_))), t0.elapsed())
            };
            // Neither arm always goes first: the first warms what the second rides.
            let ((b_ok, b_took), (d_ok, d_took)) = if i % 2 == 0 {
                let b = bridge.await;
                (b, direct.await)
            } else {
                let d = direct.await;
                (bridge.await, d)
            };
            totals[h].0 += b_ok as usize;
            totals[h].1 += d_ok as usize;
            println!(
                "[tor-ab] round {i} {host}: bridge {} in {:.2}s, arti {} in {:.2}s",
                if b_ok { "ok" } else { "fail" },
                b_took.as_secs_f64(),
                if d_ok { "ok" } else { "fail" },
                d_took.as_secs_f64()
            );
        }
    }
    let (mut b_all, mut d_all) = (0, 0);
    for (host, (b, d)) in hosts.iter().zip(&totals) {
        println!("[tor-ab] {host}: bridge {b}/{rounds}, arti {d}/{rounds}");
        b_all += b;
        d_all += d;
    }
    println!("[tor-ab] all hosts: bridge {b_all}/{}, arti {d_all}/{}", rounds * hosts.len(), rounds * hosts.len());
    svc.stop_and_join().await;
}
