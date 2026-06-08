//! `concord` — a diagnostic CLI for the Concord protocol: parse + render a community's epoch chains,
//! rekeys, edition heads, and (next increment) every known FORK, decrypted with a real account's keys —
//! so you can SEE the precise state instead of reverse-engineering obfuscated relay blobs.
//!
//! Source toggle: `--relay` (planned) reconstructs the chain by fetching + decrypting relay events under
//! every held server root; the DEFAULT reads the account's local decrypted DB. Both render identically.
//!
//! Usage:
//!   VECTOR_NSEC=nsec1... [VECTOR_DATA_DIR=/path] [VECTOR_PASSWORD=...] concord [community_id_prefix] [--relay]
//!   VECTOR_PASSWORD=<pin> VECTOR_DATA_DIR=/copy concord --invites [--from <npub>] [--since-hours N]
//!
//! Auth: pass VECTOR_NSEC directly, OR omit it and pass VECTOR_PASSWORD — the key is then read from the
//! account DB and decrypted with the PIN (the sole `npub1…` subdir, or VECTOR_NPUB to disambiguate).
//!
//! `--invites` answers "is a direct invite actually on the network for this account?" — it fetches every
//! gift wrap addressed to me from my inbox relays (paged past the relay cap), unwraps each, and reports
//! which are COMMUNITY_INVITE_BUNDLE.
//!
//! Run against a COPY of the data dir — login writes a pkey row and purges the per-account mls store.

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
    let invites = args.iter().any(|a| a == "--invites");
    // `--from <hex|npub>` narrows the invite probe to one sender; `--since-hours N` widens the window
    // (gift wraps backdate their outer timestamp up to ~2 days, so default generously).
    let from = arg_value(&args, "--from");
    let since_arg = arg_value(&args, "--since-hours");
    let since_hours: u64 = since_arg.as_deref().and_then(|s| s.parse().ok()).unwrap_or(168);
    // Positional community-id prefix: the first bare token that isn't a flag or a flag's value.
    let consumed: Vec<&String> = [from.as_ref(), since_arg.as_ref()].into_iter().flatten().collect();
    let filter = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--") && !consumed.contains(a))
        .cloned();

    let mut nsec = std::env::var("VECTOR_NSEC").unwrap_or_default();
    let password = std::env::var("VECTOR_PASSWORD").ok();
    let data_dir = std::env::var("VECTOR_DATA_DIR").map(PathBuf::from).unwrap_or_else(|_| default_dir());

    let core = VectorCore::init(CoreConfig { data_dir: data_dir.clone(), event_emitter: None }).unwrap_or_else(|e| {
        eprintln!("init failed: {e}");
        std::process::exit(1);
    });

    // Password-from-DB login: no nsec given → read the encrypted pkey from the account DB and decrypt it
    // with the PIN. Only the PIN is needed, never the raw nsec. The data dir MUST be a COPY — login writes
    // a pkey row and purges the per-account mls store, so never point this at a live account dir.
    if nsec.is_empty() {
        let npub = resolve_account_npub(&data_dir).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        });
        vector_core::db::set_current_account(npub.clone()).and_then(|_| vector_core::db::init_database(&npub)).unwrap_or_else(|e| {
            eprintln!("open db for {npub} failed: {e}");
            std::process::exit(1);
        });
        let stored = vector_core::db::get_pkey().ok().flatten().unwrap_or_else(|| {
            eprintln!("no stored key in {npub}/vector.db");
            std::process::exit(1);
        });
        nsec = if stored.starts_with("nsec1") {
            stored
        } else {
            let pin = password.clone().unwrap_or_else(|| {
                eprintln!("account is encrypted — set VECTOR_PASSWORD=<pin>");
                std::process::exit(1);
            });
            vector_core::crypto::maybe_decrypt_inner(stored, Some(pin)).await.unwrap_or_else(|_| {
                eprintln!("incorrect PIN (could not decrypt stored key)");
                std::process::exit(1);
            })
        };
    }

    let acct = core.login(&nsec, password.as_deref()).await.unwrap_or_else(|e| {
        eprintln!("login failed: {e}");
        std::process::exit(1);
    });
    eprintln!("# account {}   source={}", acct.npub, if relay { "local+relay" } else { "local" });

    if invites {
        probe_invites(since_hours, from.as_deref()).await;
        std::process::exit(0);
    }

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

/// Value of a `--flag value` pair on the command line, if present.
fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

/// Resolve which account to open for password-from-DB login: `VECTOR_NPUB` if set, else the sole
/// `npub1…` subdir under the data dir. Errors (listing options) if it's ambiguous, so we never guess.
fn resolve_account_npub(data_dir: &std::path::Path) -> Result<String, String> {
    if let Ok(n) = std::env::var("VECTOR_NPUB") {
        if !n.is_empty() {
            return Ok(n);
        }
    }
    let npubs: Vec<String> = std::fs::read_dir(data_dir)
        .map_err(|e| format!("cannot read {}: {e}", data_dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("npub1"))
        .collect();
    match npubs.len() {
        1 => Ok(npubs.into_iter().next().unwrap()),
        0 => Err(format!("no npub1… account dir under {}", data_dir.display())),
        _ => Err(format!("multiple accounts under {} — set VECTOR_NPUB to one of:\n  {}", data_dir.display(), npubs.join("\n  "))),
    }
}

/// INVITE PROBE: is a direct community invite actually on the network for THIS account?
///
/// Fetches every kind-1059 gift wrap addressed to me from my own inbox relays (kind 10050) plus the
/// trusted relays, per-relay so a "delivered to relay A, missing on relay B" split is visible, unwraps
/// each with my key, and reports which inner rumors are COMMUNITY_INVITE_BUNDLE (3304) — with sender,
/// community, channel count and the rumor's own timestamp (the real send time; the wrap backdates).
async fn probe_invites(since_hours: u64, from: Option<&str>) {
    use nostr_sdk::{Filter, Kind, RelayUrl, Timestamp, ToBech32};
    use std::collections::{BTreeMap, HashSet};
    use std::time::Duration;
    use vector_core::stored_event::event_kind::COMMUNITY_INVITE_BUNDLE;

    let Some(me) = vector_core::state::my_public_key() else {
        println!("(no active pubkey)");
        return;
    };
    let Some(keys) = vector_core::state::MY_SECRET_KEY.to_keys() else {
        println!("(no local key; cannot unwrap)");
        return;
    };
    let Some(client) = vector_core::state::nostr_client() else {
        println!("(no nostr client)");
        return;
    };

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let since = Timestamp::from(now.saturating_sub(since_hours.saturating_mul(3600)));
    let from_pk = from.and_then(|s| {
        nostr_sdk::PublicKey::parse(s).map_err(|e| eprintln!("bad --from {s}: {e}")).ok()
    });

    // Relay set: my published inbox relays (where invites are delivered) ∪ trusted relays.
    let mut relays: Vec<RelayUrl> = vector_core::inbox_relays::trusted_relay_urls();
    {
        let f = Filter::new().author(me).kind(Kind::Custom(10050)).limit(1);
        if let Ok(evs) = client.fetch_events(f, Duration::from_secs(8)).await {
            if let Some(ev) = evs.into_iter().next() {
                for t in ev.tags.iter() {
                    let s = t.as_slice();
                    if s.len() >= 2 && s[0] == "relay" {
                        if let Ok(u) = RelayUrl::parse(&s[1]) {
                            if !relays.contains(&u) {
                                relays.push(u);
                            }
                        }
                    }
                }
            }
        }
    }
    println!(
        "\n  ── invite probe ──\n  me {}\n  window last {since_hours}h   relays {}{}",
        me.to_bech32().unwrap_or_default(),
        relays.len(),
        from_pk.map(|p| format!("   from {}", &p.to_hex()[..16])).unwrap_or_default()
    );

    // Per-relay fetch so a delivery split is visible; union the gift wraps by id for unwrapping.
    // Relays cap a single response (~500), so page backwards by `until` until the window is exhausted —
    // otherwise a truncated relay could hide the very invite we're hunting (no silent caps).
    let mut union: BTreeMap<nostr_sdk::EventId, nostr_sdk::Event> = BTreeMap::new();
    for r in &relays {
        let _ = client.add_relay(r.as_str()).await;
        let mut until: Option<Timestamp> = None;
        let mut got = 0usize;
        let mut pages = 0u32;
        loop {
            let mut f = Filter::new().kind(Kind::GiftWrap).pubkey(me).since(since).limit(500);
            if let Some(u) = until {
                f = f.until(u);
            }
            let evs = client.fetch_events_from(vec![r.clone()], f, Duration::from_secs(12)).await.unwrap_or_default();
            let n = evs.len();
            // Oldest in this page seeds the next page's `until` (one second before, to avoid re-fetching it).
            let oldest = evs.iter().map(|e| e.created_at).min();
            for ev in evs.into_iter() {
                union.insert(ev.id, ev);
            }
            got += n;
            pages += 1;
            // Stop when the relay returns a short page (no more), or we'd loop forever, or we've paged deep.
            match oldest {
                Some(o) if n >= 500 && o > since && pages < 40 => until = Some(Timestamp::from(o.as_secs().saturating_sub(1))),
                _ => break,
            }
        }
        println!("    {r}  →  {got} gift wrap(s) ({pages} page(s))");
    }
    println!("  unique gift wraps: {}", union.len());

    // Unwrap each, tally inner kinds, detail every invite bundle.
    let mut by_kind: BTreeMap<u16, usize> = BTreeMap::new();
    let mut undecryptable = 0usize;
    let mut invites_found: Vec<(nostr_sdk::PublicKey, u64, vector_core::community::invite::CommunityInvite)> = Vec::new();
    let mut seen_senders: HashSet<nostr_sdk::PublicKey> = HashSet::new();
    for ev in union.values() {
        match nostr_sdk::nips::nip59::UnwrappedGift::from_gift_wrap(&keys, ev).await {
            Ok(g) => {
                let k = g.rumor.kind.as_u16();
                *by_kind.entry(k).or_default() += 1;
                seen_senders.insert(g.sender);
                if k == COMMUNITY_INVITE_BUNDLE {
                    if let Some(inv) = vector_core::community::invite::parse_invite_rumor(g.rumor.kind, &g.rumor.content) {
                        if from_pk.map(|p| p == g.sender).unwrap_or(true) {
                            invites_found.push((g.sender, g.rumor.created_at.as_secs(), inv));
                        }
                    }
                }
            }
            Err(_) => undecryptable += 1,
        }
    }

    println!("\n  inner-kind tally (unwrapped):");
    for (k, n) in &by_kind {
        let label = match *k {
            14 => " (nip17 dm)",
            15 => " (nip17 file)",
            COMMUNITY_INVITE_BUNDLE => " (COMMUNITY INVITE)",
            _ => "",
        };
        println!("      kind {k:<5} ×{n}{label}");
    }
    if undecryptable > 0 {
        println!("      ({undecryptable} wrap(s) not addressed to / not decryptable by me)");
    }
    println!("  distinct senders seen: {}", seen_senders.len());

    println!("\n  ── community invites on network: {} ──", invites_found.len());
    if invites_found.is_empty() {
        println!("    NONE. No COMMUNITY_INVITE_BUNDLE gift wrap for this account on these relays in-window.");
        println!("    → the invite never reached these relays (sender-side / relay-mismatch), OR it is older than {since_hours}h.");
    }
    invites_found.sort_by_key(|(_, ts, _)| *ts);
    for (sender, ts, inv) in &invites_found {
        println!(
            "    • '{}'  ({}…)\n        from {}\n        sent {}  relays {}  channels {}",
            inv.name,
            &inv.community_id[..inv.community_id.len().min(16)],
            sender.to_bech32().unwrap_or_else(|_| sender.to_hex()),
            ts,
            inv.relays.len(),
            inv.channels.len(),
        );
    }
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
