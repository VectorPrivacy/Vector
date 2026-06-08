//! One-off diagnostic: fetch a Community's control plane live and print exactly what the fold sees —
//! the GroupRoot candidates (name + author + inner_id), what's gapped/quarantined, the folded grants,
//! and whether a given author resolves as MANAGE_METADATA-authorized. There are no Concord logs, so this
//! is the only window besides the raw DB.
//!
//! Usage: cargo run -p vector-core --example concord_control_dump -- <app_data_dir> <npub> <community_id_hex> [author_hex]
//! Run against a COPY of the live DB (sqlite3 live.db ".backup copy.db") so it never locks/mutates the real one.

use nostr_sdk::prelude::*;
use std::time::Duration;
use vector_core::community::{roster, CommunityId};

fn hex32(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
}
fn hx(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args: Vec<String> = std::env::args().collect();
    let app_dir = &args[1];
    let npub = &args[2];
    let cid_hex = &args[3];
    let author = args.get(4).cloned();

    vector_core::db::set_app_data_dir(std::path::PathBuf::from(app_dir));
    vector_core::db::set_current_account(npub.clone()).unwrap();
    vector_core::db::init_database(npub).unwrap();

    let cid = CommunityId(hex32(cid_hex));
    let community = vector_core::db::community::load_community(&cid).unwrap().expect("community not in DB");
    let owner_hex = community.owner_attestation.as_ref().and_then(|j| Event::from_json(j).ok()).map(|e| e.pubkey.to_hex());
    println!("community = {:?}  epoch = {}  relays = {:?}", community.name, community.server_root_epoch.0, community.relays);
    println!("proven owner = {:?}", owner_hex);

    // Fetch the control plane at the current server-root epoch pseudonym, exactly as fetch_control_folded does.
    let pseudonym = roster::control_pseudonym(&community.server_root_key, &community.id, community.server_root_epoch);
    println!("control pseudonym = {}", pseudonym);
    let client = Client::default();
    for r in &community.relays {
        let _ = client.add_relay(r).await;
    }
    client.connect().await;
    let filter = Filter::new()
        .kind(Kind::Custom(3308))
        .custom_tags(SingleLetterTag::lowercase(Alphabet::Z), [pseudonym]);
    let events = client.fetch_events_from(community.relays.clone(), filter, Duration::from_secs(20)).await.unwrap();
    println!("\nfetched {} raw control events from relays", events.len());
    let inners: Vec<Event> = events.iter().filter_map(|e| roster::open_control_edition(e, &community.server_root_key).ok()).collect();
    println!("opened {} inner editions", inners.len());

    let floors = vector_core::db::community::get_all_edition_heads(cid_hex).unwrap();
    if let Some((v, h)) = floors.get(cid_hex) {
        println!("\nGroupRoot persisted floor: v{} self_hash={}", v, hx(h));
    } else {
        println!("\nGroupRoot persisted floor: NONE (bootstrapping)");
    }

    let folded = roster::fold_roster(&inners, &community.id, &floors);

    println!("\n=== GroupRoot candidates (what the consumer scans, version desc / inner_id asc) ===");
    if folded.root_candidates.is_empty() {
        println!("  (EMPTY — GroupRoot was quarantined or no edition >= floor)");
    }
    for c in &folded.root_candidates {
        let auth = owner_hex.as_deref().map(|o| {
            roster::authorize_delegation(&folded, Some(o)).is_authorized(&c.author.to_hex(), Some(o), vector_core::community::roles::Permissions::MANAGE_METADATA)
        });
        println!(
            "  v{} name={:?} author={} inner_id={} self_hash={} AUTHORIZED={:?}",
            c.head.version, c.meta.name, c.author.to_hex(), hx(&c.head.inner_id), hx(&c.head.self_hash), auth
        );
    }

    println!("\n=== gapped / quarantined entities ===");
    for g in &folded.gapped_entities {
        let tag = if *g == community.id.0 { "  <- GroupRoot" } else { "" };
        println!("  {}{}", hx(g), tag);
    }

    println!("\n=== folded grants (authority) ===");
    for (i, g) in folded.roles.grants.iter().enumerate() {
        let who = folded.grant_authors.get(i).map(|a| a.to_hex()).unwrap_or_default();
        println!("  member={} roles={:?} (granted by {})", g.member, g.role_ids, who);
    }

    if let (Some(a), Some(o)) = (author.as_ref(), owner_hex.as_deref()) {
        let authd = roster::authorize_delegation(&folded, Some(o));
        let yes = authd.is_authorized(a, Some(o), vector_core::community::roles::Permissions::MANAGE_METADATA);
        println!("\nauthor {} MANAGE_METADATA authorized in THIS fold = {}", a, yes);
    }

    // === RE-ANCHOR COVERAGE (why privatize aborts) ===
    // The base rotation demands every version 1..=head of every tracked entity be re-fetchable. Compute
    // expected (from the persisted heads) vs what the live fetch actually returns, and print the gap.
    use std::collections::HashSet;
    let mut expected: HashSet<(String, u64)> = HashSet::new();
    for (entity, (head_v, _)) in &floors {
        for v in 1..=*head_v {
            expected.insert((entity.clone(), v));
        }
    }
    let mut fetched: HashSet<(String, u64)> = HashSet::new();
    for inner in &inners {
        if let Ok(p) = vector_core::community::edition::parse_edition_inner(inner) {
            fetched.insert((hx(&p.entity_id), p.version));
        }
    }
    let mut missing: Vec<(String, u64)> = expected.difference(&fetched).cloned().collect();
    missing.sort();
    println!("\n=== RE-ANCHOR COVERAGE ===");
    println!("expected (entity,version) pairs = {}", expected.len());
    println!("fetched distinct (entity,version) = {}", fetched.len());
    println!("MISSING from the live fetch = {} (each one aborts the base rotation):", missing.len());
    for (e, v) in &missing {
        println!("  {} v{}", e, v);
    }
}
