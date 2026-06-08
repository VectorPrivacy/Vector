//! `concord` — a diagnostic CLI for the Concord protocol: parse + render a community's epoch chains,
//! rekeys, edition heads, and (next increment) every known FORK, decrypted with a real account's keys —
//! so you can SEE the precise state instead of reverse-engineering obfuscated relay blobs.
//!
//! Source toggle: `--relay` (planned) reconstructs the chain by fetching + decrypting relay events under
//! every held server root; the DEFAULT reads the account's local decrypted DB. Both render identically.
//!
//! Usage:
//!   VECTOR_NSEC=nsec1... [VECTOR_DATA_DIR=/path] [VECTOR_PASSWORD=...] concord [community_id_prefix] [--relay]
//!
//! Run it against the SAME account whose DB you want to inspect (e.g. the agent: point VECTOR_DATA_DIR at
//! `…/io.vectorapp/agent` and VECTOR_NSEC at that account). Read-only.

use std::path::PathBuf;
use vector_core::community::SERVER_ROOT_SCOPE_HEX;
use vector_core::db::community as cdb;
use vector_core::{CoreConfig, VectorCore};

/// First 6 bytes of a 32-byte key as hex + ellipsis — enough to eyeball equality/divergence across accounts.
fn prefix(b: &[u8]) -> String {
    let h: String = b.iter().take(6).map(|x| format!("{x:02x}")).collect();
    format!("{h}…")
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let relay = args.iter().any(|a| a == "--relay");
    let filter = args.iter().skip(1).find(|a| !a.starts_with("--")).cloned();

    let nsec = std::env::var("VECTOR_NSEC").unwrap_or_default();
    if nsec.is_empty() {
        eprintln!("Usage: VECTOR_NSEC=nsec1... [VECTOR_DATA_DIR=...] concord [community_id_prefix] [--relay]");
        std::process::exit(1);
    }
    let data_dir = std::env::var("VECTOR_DATA_DIR").map(PathBuf::from).unwrap_or_else(|_| default_dir());

    let core = VectorCore::init(CoreConfig { data_dir, event_emitter: None }).unwrap_or_else(|e| {
        eprintln!("init failed: {e}");
        std::process::exit(1);
    });
    let password = std::env::var("VECTOR_PASSWORD").ok();
    let acct = core.login(&nsec, password.as_deref()).await.unwrap_or_else(|e| {
        eprintln!("login failed: {e}");
        std::process::exit(1);
    });
    eprintln!("# account {}   source={}", acct.npub, if relay { "local+relay" } else { "local" });

    let ids = cdb::list_community_ids().unwrap_or_default();
    let mut shown = 0usize;
    for id in ids {
        let hex = id.to_hex();
        if let Some(f) = &filter {
            if !hex.starts_with(f.as_str()) {
                continue;
            }
        }
        if let Ok(Some(c)) = cdb::load_community(&id) {
            print_local(&c);
            if relay {
                print_relay(&c).await;
            }
            shown += 1;
        }
    }
    if shown == 0 {
        println!("(no matching communities)");
    }
    // Exit hard: login may have spawned background relay tasks we don't need for a one-shot dump.
    std::process::exit(0);
}

fn print_local(c: &vector_core::community::Community) {
    let hex = c.id.to_hex();
    let mode = match cdb::get_community_invite_registry(&hex) {
        Ok(r) if !r.is_empty() => "PUBLIC",
        _ => "private",
    };
    println!("\n━━━━━━ {}  ({}…)  [{}]", c.name, &hex[..16], mode);
    println!("  server-root: epoch {}  key {}", c.server_root_epoch.0, prefix(c.server_root_key.as_bytes()));

    if let Ok(pending) = cdb::get_read_cut_pending(&hex) {
        let target = cdb::get_read_cut_target_epoch(&hex).unwrap_or(0);
        if pending || target != c.server_root_epoch.0 {
            println!("  read-cut:    pending={pending}  target_epoch={target}");
        }
    }
    if let Ok(bl) = cdb::get_community_banlist(&hex) {
        if !bl.is_empty() {
            let who: Vec<String> = bl.iter().map(|h| format!("{}…", &h[..h.len().min(10)])).collect();
            println!("  banlist:     {}", who.join(", "));
        }
    }

    // Base (server-root) epoch chain — one root per epoch; a future fork view shows siblings here.
    if let Ok(mut roots) = cdb::held_epoch_keys(&hex, SERVER_ROOT_SCOPE_HEX) {
        roots.sort_by_key(|(e, _)| e.0);
        let chain: Vec<String> = roots.iter().map(|(e, k)| format!("{}:{}", e.0, prefix(k))).collect();
        println!("  base chain:  {}", chain.join("  →  "));
    }

    for ch in &c.channels {
        let chex = ch.id.to_hex();
        println!(
            "  ┌─ #{}  ({}…)  head epoch {}  key {}",
            ch.name,
            &chex[..12],
            ch.epoch.0,
            prefix(ch.key.as_bytes())
        );
        if let Ok(mut ks) = cdb::held_epoch_keys(&hex, &chex) {
            ks.sort_by_key(|(e, _)| e.0);
            let chain: Vec<String> = ks.iter().map(|(e, k)| format!("{}:{}", e.0, prefix(k))).collect();
            println!("  └─ epochs:   {}", chain.join("  →  "));
        }
    }

    // Control-plane edition heads (refuse-downgrade floors): entity → epoch.version.
    if let Ok(heads) = cdb::get_all_edition_heads_epoched(&hex) {
        if !heads.is_empty() {
            let mut hv: Vec<(&String, &(u64, u64, [u8; 32]))> = heads.iter().collect();
            hv.sort_by(|a, b| a.0.cmp(b.0));
            println!("  edition heads (entity → epoch.version):");
            for (entity, (epoch, version, _)) in hv {
                let label = if entity == &hex {
                    "GroupRoot".to_string()
                } else {
                    format!("{}…", &entity[..entity.len().min(12)])
                };
                println!("      {label:<16} {epoch}.{version}");
            }
        }
    }

    // Per-creator public invite links — the basis of the computed Public/Private mode.
    if let Ok(sets) = cdb::get_invite_link_sets(&hex) {
        for s in sets.iter().filter(|s| !s.locators.is_empty()) {
            println!("  invite-links: {}… holds {}", &s.creator_hex[..s.creator_hex.len().min(10)], s.locators.len());
        }
    }
}

/// RELAY reconstruction: fetch the rekey events from the community's relays and decrypt each under EVERY
/// held server root, so we SEE every fork sibling at each epoch + whether THIS account is a recipient of
/// each (the exact inputs the convergence heal works from). Uses local keys to decrypt; events from relays.
async fn print_relay(c: &vector_core::community::Community) {
    use vector_core::community::derive::{self, RekeyScope};
    use vector_core::community::rekey;
    use vector_core::community::transport::{LiveTransport, Query, Transport};
    use vector_core::community::{Epoch, ServerRootKey};
    use vector_core::stored_event::event_kind::COMMUNITY_REKEY;
    use std::collections::{BTreeMap, HashSet};

    let hex = c.id.to_hex();
    println!("\n  ── RELAY reconstruction ──");
    let Some(keys) = vector_core::state::MY_SECRET_KEY.to_keys() else {
        println!("    (no local key; cannot decrypt)");
        return;
    };
    let sk = keys.secret_key();
    let tx = LiveTransport::with_timeout(std::time::Duration::from_secs(15));

    // Per-relay latency, split into the two costs that matter on a high-latency link, measured with a FRESH
    // throwaway client per relay so the handshake is real (not hidden behind an already-open pool socket):
    //   • socket-connect = TCP + TLS + WebSocket upgrade (the cost paid on every cold start / reconnect)
    //   • in-tunnel fetch = REQ → response once the socket is up (what the warmed pool would show)
    // A re-founding's coverage gate fetches over the shared pool, so a relay that's cheap in-tunnel but
    // expensive to (re)connect can still blow the budget after a reconnect — hence reporting both.
    {
        use nostr_sdk::{Client, Filter, Kind};
        use std::time::{Duration, Instant};
        use vector_core::stored_event::event_kind::COMMUNITY_CONTROL;
        println!("    ── per-relay latency (fresh connection) ──");
        for r in &c.relays {
            let client = Client::default();
            let _ = client.add_relay(r.as_str()).await;
            let t0 = Instant::now();
            client.try_connect(Duration::from_secs(15)).await;
            let connect_ms = t0.elapsed().as_millis();
            let filter = Filter::new().kind(Kind::Custom(COMMUNITY_CONTROL)).limit(1);
            let t1 = Instant::now();
            let res = client.fetch_events_from(vec![r.clone()], filter, Duration::from_secs(15)).await;
            let fetch_ms = t1.elapsed().as_millis();
            let n = res.as_ref().map(|e| e.len()).unwrap_or(0);
            println!("      {r}\n        socket-connect {connect_ms:>6} ms   in-tunnel fetch {fetch_ms:>6} ms   ({n} ev)");
            let _ = client.shutdown().await;
        }
    }

    let roots = vector_core::db::community::held_epoch_keys(&hex, vector_core::community::SERVER_ROOT_SCOPE_HEX).unwrap_or_default();
    if roots.is_empty() {
        println!("    (no held server roots)");
        return;
    }
    let cur_base = c.server_root_epoch.0;

    // BASE re-foundings: a base rekey to epoch e is addressed under the PRIOR root (e-1), so try each held
    // root as a prior. ≥2 distinct delivered roots at one epoch = a re-founding fork (B2).
    println!("    BASE (fork = ≥2 roots at one epoch):");
    let mut base: BTreeMap<u64, Vec<([u8; 32], Option<[u8; 32]>)>> = BTreeMap::new();
    for (pe, prior) in &roots {
        let target = pe.0 + 1;
        let z = derive::base_rekey_pseudonym(&ServerRootKey(*prior), &c.id, Epoch(target)).to_hex();
        let q = Query { kinds: vec![COMMUNITY_REKEY], z_tags: vec![z], since: None, ..Default::default() };
        for ev in tx.fetch(&q, &c.relays).await.unwrap_or_default() {
            if let Ok(p) = rekey::open_rekey_event(&ev, prior) {
                if matches!(p.scope, RekeyScope::ServerRoot) && p.new_epoch.0 == target {
                    base.entry(target).or_default().push((p.rotator.to_bytes(), peek_key(sk, &p)));
                }
            }
        }
    }
    if base.is_empty() {
        println!("      (none found on relays)");
    }
    for (epoch, sibs) in &base {
        let distinct: HashSet<_> = sibs.iter().filter_map(|(_, m)| *m).collect();
        let tag = if distinct.len() >= 2 { "   <<< FORK" } else { "" };
        let star = if *epoch == cur_base { " (current)" } else { "" };
        println!("      epoch {epoch}{star}: {} candidate(s), {} distinct root(s){tag}", sibs.len(), distinct.len());
        for (rot, mine) in sibs {
            let m = mine.map(|k| prefix(&k)).unwrap_or_else(|| "NOT A RECIPIENT".into());
            println!("         rotator {}  root→me {m}", hexpref(rot));
        }
        // VERDICT: a correct client adopts the LOWEST root (deterministic B2 tiebreak). If my head root
        // disagrees with this, that's the bug, in one glance.
        if distinct.len() >= 2 {
            if let Some(win) = distinct.iter().min() {
                println!("         → verdict: converge to LOWEST root {} (B2 tiebreak)", prefix(win));
            }
        }
    }

    // CHANNEL rekeys: a re-founding rekeys the channel under the (shared) root current at publish; search
    // EVERY held root. ≥2 distinct delivered keys at one epoch = the A-B2 channel fork.
    for ch in &c.channels {
        let head = ch.epoch.0;
        println!("    #{} (fork = ≥2 keys at one epoch):", ch.name);
        let mut by_epoch: BTreeMap<u64, Vec<(u64, [u8; 32], Option<[u8; 32]>)>> = BTreeMap::new();
        for (re, root) in &roots {
            let z_tags: Vec<String> = (1..=head + 1)
                .map(|e| derive::rekey_pseudonym(&ServerRootKey(*root), &ch.id, Epoch(e)).to_hex())
                .collect();
            let q = Query { kinds: vec![COMMUNITY_REKEY], z_tags, since: None, ..Default::default() };
            for ev in tx.fetch(&q, &c.relays).await.unwrap_or_default() {
                if let Ok(p) = rekey::open_rekey_event(&ev, root) {
                    if matches!(p.scope, RekeyScope::Channel(id) if id == ch.id) {
                        by_epoch.entry(p.new_epoch.0).or_default().push((re.0, p.rotator.to_bytes(), peek_key(sk, &p)));
                    }
                }
            }
        }
        if by_epoch.is_empty() {
            println!("      (no channel rekeys found on relays)");
        }
        for (epoch, sibs) in &by_epoch {
            let distinct: HashSet<_> = sibs.iter().filter_map(|(_, _, m)| *m).collect();
            let tag = if distinct.len() >= 2 { "   <<< FORK (channel diverged)" } else { "" };
            let star = if *epoch == head { " (current head)" } else { "" };
            println!("      epoch {epoch}{star}: {} candidate(s), {} distinct key(s){tag}", sibs.len(), distinct.len());
            for (root_ep, rot, mine) in sibs {
                let m = mine.map(|k| prefix(&k)).unwrap_or_else(|| "NOT A RECIPIENT".into());
                println!("         under root@{root_ep}  rotator {}  key→me {m}", hexpref(rot));
            }
            // VERDICT: a correct client adopts the LOWEST key (A-B2 tiebreak). If my channel head disagrees,
            // that's the bug. NOT A RECIPIENT on the winning sibling = I can't converge (a retain-set gap).
            if distinct.len() >= 2 {
                if let Some(win) = distinct.iter().min() {
                    println!("         → verdict: converge to LOWEST key {} (A-B2 tiebreak)", prefix(win));
                }
            }
        }
    }

    // BY-AUTHOR census: a BROAD fetch of every rekey on the relays (no #z filter), grouped by who rotated.
    // Each is tried under every held root; what opens shows that author's published rekeys, what DOESN'T is
    // counted as OPAQUE (under a root I don't hold) — so "the winner never published a channel rekey" vs
    // "published one under a root I dropped" is answerable in one glance.
    {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let q = Query { kinds: vec![COMMUNITY_REKEY], z_tags: vec![], since: Some(now.saturating_sub(48 * 3600)), ..Default::default() };
        let events = tx.fetch(&q, &c.relays).await.unwrap_or_default();
        println!("\n    ── by-author census ({} rekey events on relays, last 48h) ──", events.len());
        let mut by_author: BTreeMap<String, Vec<(String, u64, u64, Option<[u8; 32]>)>> = BTreeMap::new();
        let mut opaque: BTreeMap<String, usize> = BTreeMap::new();
        for ev in &events {
            let mut opened = false;
            for (re, root) in &roots {
                if let Ok(p) = rekey::open_rekey_event(ev, root) {
                    let scope = match p.scope {
                        RekeyScope::ServerRoot => "base".to_string(),
                        RekeyScope::Channel(id) => format!("chan:{}", &id.to_hex()[..6]),
                    };
                    let key: String = p.rotator.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
                    by_author.entry(key).or_default().push((scope, p.new_epoch.0, re.0, peek_key(sk, &p)));
                    opened = true;
                    break;
                }
            }
            if !opened {
                let z = ev.tags.iter().find_map(|t| {
                    let s = t.as_slice();
                    (s.len() >= 2 && s[0] == "z").then(|| s[1].clone())
                }).unwrap_or_default();
                *opaque.entry(z.chars().take(12).collect()).or_default() += 1;
            }
        }
        for (rot, mut items) in by_author {
            items.sort_by_key(|(_, e, _, _)| *e);
            println!("      rotator {}…", &rot[..rot.len().min(10)]);
            for (scope, epoch, root_ep, mine) in items {
                let m = mine.map(|k| prefix(&k)).unwrap_or_else(|| "not-to-me".into());
                println!("        {scope} epoch {epoch}  under root@{root_ep}  key→me {m}");
            }
        }
        if !opaque.is_empty() {
            let total: usize = opaque.values().sum();
            println!("      {total} OPAQUE event(s) under roots I don't hold (publisher's root unknown to me):");
            for (z, n) in opaque {
                println!("        #z {z}…  ×{n}");
            }
        }
    }
}

/// Open MY blob in a rekey (the key it delivers to this account), or None if I'm not a recipient.
fn peek_key(sk: &nostr_sdk::SecretKey, p: &vector_core::community::rekey::ParsedRekey) -> Option<[u8; 32]> {
    use vector_core::community::{derive, rekey};
    let secret = rekey::rekey_pairwise_secret(sk, &p.rotator).ok()?;
    let loc = derive::recipient_pseudonym(&secret, p.scope, p.new_epoch).to_hex();
    let blob = p.blobs.iter().find(|b| b.locator == loc)?;
    rekey::open_rekey_blob(sk, &p.rotator, p.scope, p.new_epoch, blob).ok()
}

/// Short hex of a pubkey/id (first 5 bytes).
fn hexpref(b: &[u8]) -> String {
    let h: String = b.iter().take(5).map(|x| format!("{x:02x}")).collect();
    format!("{h}…")
}

fn default_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join("Library/Application Support/io.vectorapp/agent");
    }
    #[cfg(target_os = "linux")]
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".local/share/io.vectorapp/agent");
    }
    PathBuf::from("/tmp/vector-data")
}
