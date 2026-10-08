//! I2P through the transport meta-glue, against a client-only router the operator already runs
//! with SAM on 127.0.0.1:7656.
//!
//! Ignored, and skipped unless `VECTOR_I2P_LIVE=1`:
//! `VECTOR_I2P_LIVE=1 cargo test -p vector-core --features i2p --test i2p_live -- --ignored --nocapture --test-threads=1`
//!
//! Client sessions only, LeaseSets unpublished. Throwaway keys, REQs only. Prints timings, never
//! an address.

#![cfg(all(feature = "i2p", not(target_arch = "wasm32")))]

use std::time::{Duration, Instant};

use nostr_sdk::prelude::*;
use vector_core::i2p::sam::Liveness;
use vector_core::i2p::I2pTransport;
use vector_core::ClientRelayExt;
use vector_core::transport::i2p_config::{self, OutproxyInput};
use vector_core::transport::{self, host, ExitPolicy, Kind, Lane, StartCtx, Transport, TransportState};

/// The .i2p Nostr relay that answered in the research probes (port 80).
const I2P_RELAY: &str = "ws://nostrajmjieip3dqgeefsgpydy3bbshe3o32z65dwkssl7qxkn5a.b32.i2p";

fn live() -> bool {
    std::env::var("VECTOR_I2P_LIVE").is_ok_and(|v| v == "1")
}

fn step(label: &str, since: Instant) {
    println!("[i2p-live] {label}: {:.2}s", since.elapsed().as_secs_f64());
}

/// Connect, then a REQ for one note answered by EOSE.
async fn req_to_eose(client: &Client, relay: &str, connect: Duration) -> Result<(usize, Duration, Duration), String> {
    client.add_managed_relay(relay).await.map_err(|e| e.to_string())?;
    let url = RelayUrl::parse(relay).map_err(|e| e.to_string())?;
    let r = client.relay(&url).await.map_err(|e| e.to_string())?.ok_or("relay missing")?;
    let t0 = Instant::now();
    r.try_connect().timeout(connect).await.map_err(|e| format!("connect: {e}"))?;
    let opened = t0.elapsed();
    if r.status() != RelayStatus::Connected {
        return Err(format!("not connected: {:?}", r.status()));
    }
    let t1 = Instant::now();
    let events = client
        .fetch_events(ReqTarget::single(url, [Filter::new().kind(nostr_sdk::prelude::Kind::TextNote).limit(1)]))
        .timeout(Duration::from_secs(120))
        .await
        .map_err(|e| format!("fetch: {e}"))?;
    Ok((events.len(), opened, t1.elapsed()))
}

async fn eose_with_tries(client: &Client, relay: &str, tries: usize) -> (usize, Duration, Duration) {
    let mut last = String::new();
    for attempt in 1..=tries {
        match req_to_eose(client, relay, Duration::from_secs(120)).await {
            Ok(v) => return v,
            Err(e) => {
                println!("[i2p-live]   {relay} attempt {attempt} failed: {e}");
                last = e;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
    panic!("{relay}: {last}");
}

fn i2p() -> std::sync::Arc<I2pTransport> {
    vector_core::i2p::active().expect("the I2P instance is installed")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn i2p_through_the_meta_glue() {
    if !live() {
        eprintln!("[i2p-live] skipped: set VECTOR_I2P_LIVE=1");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    vector_core::db::set_app_data_dir(root.path().to_path_buf());
    let npub = Keys::generate().public_key().to_bech32().unwrap();
    std::fs::create_dir_all(root.path().join(&npub)).unwrap();
    vector_core::db::set_current_account(npub.clone()).unwrap();
    vector_core::db::init_database(&npub).unwrap();

    // I2P chosen and not started: blocked in process.
    transport::prefs::persist_kind(Kind::I2p).unwrap();
    transport::set_preference(Some(Kind::I2p));
    assert!(matches!(transport::state(), TransportState::RequiredButInactive { kind: Kind::I2p }));
    let t0 = Instant::now();
    let refused = vector_core::net::build_http_client(Duration::from_secs(10)).unwrap().get("https://example.com/").send().await;
    assert!(refused.is_err() && t0.elapsed() < Duration::from_millis(500), "refused at once while I2P starts");
    println!("[i2p-live] before start: {}", refused.err().unwrap());

    // (a) Sessions: both lanes, created in parallel.
    let factory = transport::kinds::factory(Kind::I2p).expect("compiled in");
    let owner = vector_core::db::live_session_id();
    let t0 = Instant::now();
    let inst = factory
        .start(StartCtx { owner, started_prelogin: false, dirs: None, config: transport::prefs::config(Kind::I2p) })
        .await
        .unwrap();
    host::activate(inst, owner, false).unwrap();
    transport::wait_ready(Duration::from_secs(180)).await.expect("both sessions within 180 s");
    step("(a) sessions ready (both lanes)", t0);
    assert_eq!(transport::state(), TransportState::Active { kind: Kind::I2p });
    let probe = i2p().session_probe().unwrap();
    let t0 = Instant::now();
    assert_eq!(probe.check().await, [Liveness::Alive, Liveness::Alive]);
    step("liveness probe of both sessions", t0);
    let view = transport::status::view();
    println!("[i2p-live] steps: {:?}", view.steps.iter().map(|s| format!("{}={} ({})", s.id, s.state, s.text)).collect::<Vec<_>>());

    // (b) A .i2p relay, inside I2P, on each lane.
    let acct = vector_core::nostr_client_builder().build();
    let (n, opened, eose) = eose_with_tries(&acct, I2P_RELAY, 3).await;
    println!("[i2p-live] (b) .i2p relay, Account lane: open {:.2}s, REQ->EOSE {:.2}s ({n} event)", opened.as_secs_f64(), eose.as_secs_f64());
    assert_eq!(transport::route_view(I2P_RELAY).class, "native");
    let shared = vector_core::apply_transport(ClientBuilder::new(), Lane::Shared).build();
    let (n, opened, eose) = eose_with_tries(&shared, I2P_RELAY, 3).await;
    println!("[i2p-live] (b) .i2p relay, Shared lane: open {:.2}s, REQ->EOSE {:.2}s ({n} event)", opened.as_secs_f64(), eose.as_secs_f64());
    let t0 = Instant::now();
    let again = acct.fetch_events(ReqTarget::single(RelayUrl::parse(I2P_RELAY).unwrap(), [Filter::new().kind(nostr_sdk::prelude::Kind::Metadata).limit(1)])).timeout(Duration::from_secs(60)).await;
    println!("[i2p-live] (b) .i2p relay, warm REQ->EOSE {:.2}s ({:?} event)", t0.elapsed().as_secs_f64(), again.map(|e| e.len()).ok());

    // (c) Clearnet relays through the outproxy.
    let (n, opened, eose) = eose_with_tries(&acct, "wss://nos.lol", 3).await;
    println!("[i2p-live] (c) wss://nos.lol via outproxy: open {:.2}s, REQ->EOSE {:.2}s ({n} event)", opened.as_secs_f64(), eose.as_secs_f64());
    let rv = transport::route_view("wss://nos.lol");
    println!("[i2p-live] (c) route: {} via {:?}: {}", rv.class, rv.via, rv.text);
    assert_eq!(rv.class, "exit");
    assert!(rv.via.is_some());
    match req_to_eose(&acct, "wss://relay.damus.io", Duration::from_secs(120)).await {
        Ok((n, o, e)) => println!("[i2p-live] (c) wss://relay.damus.io via outproxy: open {:.2}s, REQ->EOSE {:.2}s ({n} event)", o.as_secs_f64(), e.as_secs_f64()),
        Err(e) => println!("[i2p-live] (c) wss://relay.damus.io via outproxy failed (relay side, shared exit IP): {e}"),
    }

    // (d) HTTPS through the shared client.
    let http = vector_core::net::shared_http_client();
    for url in ["https://example.com/", "https://nos.lol/"] {
        let t0 = Instant::now();
        let mut done = None;
        for attempt in 1..=3 {
            match http.get(url).header("Accept", "application/nostr+json").send().await {
                Ok(r) => {
                    let status = r.status();
                    let body = r.bytes().await.map(|b| b.len()).unwrap_or(0);
                    done = Some((status, body));
                    break;
                }
                Err(e) => println!("[i2p-live]   {url} attempt {attempt}: {e}"),
            }
        }
        let (status, body) = done.expect("HTTPS through an outproxy");
        println!("[i2p-live] (d) GET {url}: {status}, {body} bytes in {:.2}s", t0.elapsed().as_secs_f64());
        assert!(status.is_success());
    }
    println!("[i2p-live] outproxies: {}", transport::status::view().detail["outproxies"]);

    // Port memory: StormyCloud allows 443 only; plain HTTP fails over to Acetone.
    let t0 = Instant::now();
    match http.get("http://example.com/").send().await {
        Ok(r) => println!("[i2p-live] port 80 via outproxy: {} in {:.2}s", r.status(), t0.elapsed().as_secs_f64()),
        Err(e) => println!("[i2p-live] port 80 via outproxy failed: {e} ({:.2}s)", t0.elapsed().as_secs_f64()),
    }
    let detail = transport::status::view().detail;
    println!("[i2p-live] after port 80: stormycloud refused {} / acetone {}", detail["outproxies"]["stormycloud"]["refused_ports"], detail["outproxies"]["acetone"]["state"]);

    // A dead outproxy first: skipped after one try, the next one carries the request.
    let bogus = format!("{}.b32.i2p", "a".repeat(52));
    let stormy = i2p_config::defaults()[0].clone();
    i2p_config::set_outproxies(Some(vec![
        OutproxyInput { name: "Bogus".into(), address: bogus.clone(), port: 80, enabled: true },
        OutproxyInput { name: stormy.name.clone(), address: stormy.address.clone(), port: stormy.port as i64, enabled: true },
    ]))
    .unwrap();
    // A host with no pooled connection, so this request dials.
    let t0 = Instant::now();
    let r = http.get("https://example.org/").send().await;
    println!("[i2p-live] bogus first, then StormyCloud: {:?} in {:.2}s", r.as_ref().map(|r| r.status()).map_err(|e| e.to_string()), t0.elapsed().as_secs_f64());
    let detail = transport::status::view().detail;
    let bogus_id = i2p_config::current().outproxy_list()[0].id.clone();
    println!("[i2p-live] bogus outproxy health: {}", detail["outproxies"][&bogus_id]);
    assert!(r.is_ok());
    // Within a minute of the sessions an unknown LeaseSet is the router's cold lookup: shown, not held.
    let young = detail["session_age"].as_u64().is_some_and(|a| a < 61 + t0.elapsed().as_secs());
    let state = &detail["outproxies"][&bogus_id]["state"];
    if young {
        assert!(state == "cooling" || (state == "unknown" && detail["outproxies"][&bogus_id]["last_error"].is_string()), "{state}");
    } else {
        assert_eq!(state, "cooling");
    }
    i2p_config::set_outproxies(None).unwrap();

    // I2P-Only: clearnet refused before any byte leaves; I2P itself stays.
    i2p_config::set_exit(ExitPolicy::Off).unwrap();
    let t0 = Instant::now();
    let fresh = vector_core::nostr_client_builder().build();
    let e = req_to_eose(&fresh, "wss://nos.lol", Duration::from_secs(10)).await.err().unwrap();
    assert!(t0.elapsed() < Duration::from_secs(1), "refused at once");
    println!("[i2p-live] I2P-Only, wss://nos.lol: {e} ({:.3}s)", t0.elapsed().as_secs_f64());
    assert!(e.contains("I2P-Only is on"), "{e}");
    let (n, o, eose) = eose_with_tries(&fresh, I2P_RELAY, 3).await;
    println!("[i2p-live] I2P-Only, .i2p relay: open {:.2}s, REQ->EOSE {:.2}s ({n} event)", o.as_secs_f64(), eose.as_secs_f64());
    i2p_config::set_exit(ExitPolicy::Allow).unwrap();

    // A name the address book doesn't hold.
    let t0 = Instant::now();
    let e = http.get("http://vector-no-such-name-4f7a.i2p/").send().await.err().unwrap();
    println!("[i2p-live] unknown .i2p name: {e} ({:.2}s)", t0.elapsed().as_secs_f64());
    assert_eq!(e.to_string(), "This I2P name isn't in your router's address book. Use its .b32.i2p address.");

    // New addresses: both sessions replaced, and both lanes dial again.
    let old = i2p().session_probe().unwrap();
    let t0 = Instant::now();
    i2p().new_identity().await;
    transport::wait_ready(Duration::from_secs(180)).await.unwrap();
    step("new I2P address (both sessions replaced)", t0);
    assert_eq!(old.check().await, [Liveness::Gone, Liveness::Gone]);
    let relay = acct.relay(I2P_RELAY).await.unwrap().unwrap();
    println!("[i2p-live] relay after renewal: {:?}", relay.status());
    transport::cycle::track(&acct);
    let t0 = Instant::now();
    transport::cycle::cycle_all(transport::cycle::CycleScope::Kick).await;
    println!(
        "[i2p-live] relay after kick: {:?} ({:.2}s), last error: {:?}",
        relay.status(),
        t0.elapsed().as_secs_f64(),
        transport::route_view(I2P_RELAY).last_error
    );
    let (n, o, eose) = eose_with_tries(&acct, I2P_RELAY, 3).await;
    println!("[i2p-live] after renewal, .i2p relay: open {:.2}s, REQ->EOSE {:.2}s ({n} event)", o.as_secs_f64(), eose.as_secs_f64());

    // Shutdown ends both sessions.
    let probe = i2p().session_probe().unwrap();
    let t0 = Instant::now();
    host::deactivate(true).await;
    loop {
        if probe.check().await == [Liveness::Gone, Liveness::Gone] {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(2), "both sessions gone within 2 s");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    step("shutdown to both sessions gone", t0);
    acct.shutdown().await;
    shared.shutdown().await;
    fresh.shutdown().await;
}
