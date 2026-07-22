//! Probe a public-invite token's bundle on the relays: is the bundle still present, and is there a
//! NIP-09 deletion referencing its coordinate? Used to verify that revoking a public invite actually
//! removes it from relays.
//!
//! Usage: cargo run -p vector-core --example invite_probe -- <token_hex> <relay1,relay2,...>

use nostr_sdk::prelude::*;
use std::time::Duration;
use vector_core::community::public_invite;

fn hex32(s: &str) -> [u8; 32] {
    let mut o = [0u8; 32];
    for i in 0..32 {
        o[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
    }
    o
}

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args: Vec<String> = std::env::args().collect();
    let token = hex32(&args[1]);
    let relays: Vec<String> = args[2].split(',').map(|s| s.to_string()).collect();

    let signer_pk = public_invite::signer_pubkey(&token);
    let locator = public_invite::locator_hex(&token);
    let coord_a = format!("30078:{}:{}", signer_pk.to_hex(), locator);
    println!("token   = {}", args[1]);
    println!("signer  = {}", signer_pk.to_hex());
    println!("locator = {}", locator);
    println!("coord   = {}", coord_a);

    let client = Client::default();
    for r in &relays {
        let _ = client.add_relay(r).await;
    }
    client.connect().await;

    // 1) The bundle itself, at (kind 30078, author=signer, #d=locator).
    let bundle_filter = Filter::new().kind(Kind::Custom(30078)).author(signer_pk).identifier(locator.clone());
    let bundles = client.fetch_events_from(relays.clone(), bundle_filter, Duration::from_secs(15)).await.unwrap();
    println!("\nEvents at the coordinate: {}", bundles.len());
    for e in bundles.iter() {
        let vsk = e.tags.iter().find_map(|t| { let s = t.as_slice(); (s.len() >= 2 && s[0] == "vsk").then(|| s[1].clone()) }).unwrap_or_default();
        let kind = if e.content.is_empty() && vsk == "9" { "TOMBSTONE (revoked)" } else if vsk == "6" { "LIVE BUNDLE" } else { "?" };
        println!("  vsk={} content_len={} -> {}  (id={})", vsk, e.content.len(), kind, e.id);
    }

    // 2) Any NIP-09 deletion (kind 5) by the token-signer referencing this coordinate (`a` tag).
    let del_filter = Filter::new().kind(Kind::Custom(5)).author(signer_pk);
    let dels = client.fetch_events_from(relays.clone(), del_filter, Duration::from_secs(15)).await.unwrap();
    let matching: Vec<_> = dels
        .iter()
        .filter(|e| e.tags.iter().any(|t| { let s = t.as_slice(); s.len() >= 2 && s[0] == "a" && s[1] == coord_a }))
        .collect();
    println!("\nDELETION (kind 5) events by the signer referencing this coord: {}", matching.len());
    for e in &matching {
        println!("  id={} created_at={}", e.id, e.created_at.as_secs());
    }

    println!(
        "\n=> bundle is {} on relays; matching deletion is {} present",
        if bundles.is_empty() { "GONE" } else { "STILL PRESENT" },
        if matching.is_empty() { "NOT" } else { "" }
    );
}
