//! Keeps the I2P transport up against the router on 127.0.0.1:7656 with one `.i2p` relay and one
//! outproxy relay in use, printing every phase change, every EOSE and every refusal, so an
//! operator can stop and restart the router and watch it recover on its own.
//!
//! `cargo run -p vector-core --features i2p --example i2p_soak -- [minutes]`
//!
//! Client sessions only. A throwaway account, REQs only. Prints no address.

#[cfg(all(feature = "i2p", not(target_arch = "wasm32")))]
#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    soak::run().await;
}

#[cfg(not(all(feature = "i2p", not(target_arch = "wasm32"))))]
fn main() {
    eprintln!("i2p_soak needs --features i2p");
}

#[cfg(all(feature = "i2p", not(target_arch = "wasm32")))]
mod soak {
    use std::time::{Duration, Instant};

    use nostr_sdk::prelude::*;
    use vector_core::transport::{self, host, Egress, Kind, Lane, StartCtx};
    use vector_core::ClientRelayExt;

    const I2P_RELAY: &str = "ws://nostrajmjieip3dqgeefsgpydy3bbshe3o32z65dwkssl7qxkn5a.b32.i2p";
    const CLEARNET_RELAY: &str = "wss://nos.lol";

    fn t(start: Instant) -> String {
        format!("t={:7.1}s", start.elapsed().as_secs_f64())
    }

    fn phase_line() -> String {
        let v = transport::status::view();
        let phase = vector_core::i2p::active().map(|t| t.phase_name()).unwrap_or("none");
        match v.reason {
            Some(r) => format!("{phase} [{}] {} (retry in {:?}s)", r.code, r.text, v.retry_in),
            None => phase.to_string(),
        }
    }

    async fn eose(client: &Client, relay: &str) -> Result<(usize, f64), String> {
        let url = RelayUrl::parse(relay).map_err(|e| e.to_string())?;
        let r = client.relay(&url).await.map_err(|e| e.to_string())?.ok_or("relay missing")?;
        if r.status() != RelayStatus::Connected {
            let _ = r.try_connect().timeout(Duration::from_secs(90)).await;
        }
        if r.status() != RelayStatus::Connected {
            return Err(format!("not connected ({:?})", r.status()));
        }
        let t0 = Instant::now();
        let events = client
            .fetch_events(ReqTarget::single(url, [Filter::new().kind(nostr_sdk::prelude::Kind::TextNote).limit(1)]))
            .timeout(Duration::from_secs(60))
            .await
            .map_err(|e| e.to_string())?;
        Ok((events.len(), t0.elapsed().as_secs_f64()))
    }

    pub async fn run() {
        let minutes: u64 = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(10);
        let start = Instant::now();
        let root = tempfile::tempdir().unwrap();
        vector_core::db::set_app_data_dir(root.path().to_path_buf());
        let npub = Keys::generate().public_key().to_bech32().unwrap();
        std::fs::create_dir_all(root.path().join(&npub)).unwrap();
        vector_core::db::set_current_account(npub.clone()).unwrap();
        vector_core::db::init_database(&npub).unwrap();
        transport::prefs::persist_kind(Kind::I2p).unwrap();
        transport::set_preference(Some(Kind::I2p));

        let owner = vector_core::db::live_session_id();
        let factory = transport::kinds::factory(Kind::I2p).expect("compiled in");
        let inst = factory
            .start(StartCtx { owner, started_prelogin: false, dirs: None, config: transport::prefs::config(Kind::I2p) })
            .await
            .unwrap();
        host::activate(inst, owner, false).unwrap();
        println!("[soak] {} pid {} started, running {minutes} min", t(start), std::process::id());

        let client = vector_core::nostr_client_builder().build();
        for r in [I2P_RELAY, CLEARNET_RELAY] {
            client.add_managed_relay(r).await.unwrap();
        }
        transport::cycle::track(&client);

        // Every phase change, as it happens.
        tokio::spawn(async move {
            let mut last = String::new();
            loop {
                let now = phase_line();
                if now != last {
                    println!("[soak] {} phase {now}", t(start));
                    last = now;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });

        // Ready brings the relays back, as the app's listener does.
        let mut rx = host::subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(host::TransportEvent::Ready { recovered, .. }) => {
                        println!("[soak] {} READY (recovered: {recovered}); reconnecting relays", t(start));
                        transport::cycle::cycle_all(transport::cycle::CycleScope::Kick).await;
                    }
                    Ok(host::TransportEvent::Lost { .. }) => println!("[soak] {} LOST", t(start)),
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => return,
                }
            }
        });

        let end = start + Duration::from_secs(minutes * 60);
        while Instant::now() < end {
            let live = vector_core::db::live_session_id();
            for host in ["nos.lol", "nostrajmjieip3dqgeefsgpydy3bbshe3o32z65dwkssl7qxkn5a.b32.i2p"] {
                match transport::egress(live, Lane::Account, host, 443) {
                    Egress::Direct => println!("[soak] {} EGRESS {host}: DIRECT (must never happen)", t(start)),
                    Egress::Proxy(_) => {}
                    Egress::Refuse(e) => println!("[soak] {} egress {host}: refused: {}", t(start), e.text()),
                }
            }
            for relay in [I2P_RELAY, CLEARNET_RELAY] {
                match eose(&client, relay).await {
                    Ok((n, s)) => println!("[soak] {} EOSE {relay}: {n} event in {s:.2}s", t(start)),
                    Err(e) => println!("[soak] {} REQ {relay} failed: {e}", t(start)),
                }
            }
            let t0 = Instant::now();
            match vector_core::net::build_http_client(Duration::from_secs(60)).unwrap().get("https://nos.lol/").header("Accept", "application/nostr+json").send().await {
                Ok(r) => println!("[soak] {} HTTPS nos.lol: {} in {:.2}s", t(start), r.status(), t0.elapsed().as_secs_f64()),
                Err(e) => println!("[soak] {} HTTPS nos.lol refused: {e} ({:.3}s)", t(start), t0.elapsed().as_secs_f64()),
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        host::deactivate(true).await;
        client.shutdown().await;
        println!("[soak] {} done", t(start));
    }
}
