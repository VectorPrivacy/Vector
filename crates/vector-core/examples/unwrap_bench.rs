//! What a DM sync pays per gift wrap in `GuardedSigner::unwrap_batch`, by batch size and by
//! how many senders a batch holds. Senders are new every round, so nothing carries over
//! from one round to the next. Public API only: the same file builds against an older
//! revision for an A/B.
//!
//!   cargo run --release -p vector-core --example unwrap_bench [rounds] [wraps senders]
//!
//! With `wraps senders`, only that shape runs, so no other shape's work shares its caches.

use nostr_sdk::prelude::*;
use std::hint::black_box;
use std::time::Instant;
use vector_core::crypto::GuardedSigner;
use vector_core::state::MY_SECRET_KEY;

/// (wraps in the batch, distinct senders among them)
const SHAPES: [(usize, usize); 7] = [(1, 1), (16, 16), (16, 4), (16, 1), (64, 64), (64, 8), (64, 1)];

fn wraps(me: &Keys, n: usize, senders: usize) -> Vec<Event> {
    let from: Vec<Keys> = (0..senders).map(|_| Keys::generate()).collect();
    (0..n)
        .map(|i| {
            let r = EventBuilder::new(Kind::PrivateDirectMessage, "hey! are we still on for tomorrow?")
                .tag(Tag::public_key(me.public_key()))
                .finalize_unsigned(from[i % senders].public_key());
            GiftWrapBuilder::new(me.public_key(), r).finalize(&from[i % senders]).unwrap()
        })
        .collect()
}

fn main() {
    let rounds: usize = std::env::args().nth(1).map_or(15, |s| s.parse().expect("rounds"));
    let only: Option<(usize, usize)> = match (std::env::args().nth(2), std::env::args().nth(3)) {
        (Some(n), Some(s)) => Some((n.parse().expect("wraps"), s.parse().expect("senders"))),
        _ => None,
    };
    let shapes: Vec<(usize, usize)> = only.map_or_else(|| SHAPES.to_vec(), |shape| vec![shape]);
    let me = Keys::generate();
    MY_SECRET_KEY.store_from_keys(&me, &[]);
    // Every round gets its own wraps from its own senders.
    let sets: Vec<Vec<Vec<Event>>> = (0..rounds).map(|_| shapes.iter().map(|&(n, s)| wraps(&me, n, s)).collect()).collect();
    let mut times: Vec<Vec<f64>> = vec![Vec::new(); shapes.len()];
    for (round, set) in sets.iter().enumerate() {
        // A different shape first each round, so none always runs warm or cold.
        for k in 0..shapes.len() {
            let i = (k + round) % shapes.len();
            let refs: Vec<&Event> = set[i].iter().collect();
            let t = Instant::now();
            let out = GuardedSigner::unwrap_batch(&refs);
            let ns = t.elapsed().as_nanos() as f64;
            assert!(out.iter().all(Result::is_ok));
            black_box(out);
            times[i].push(ns / refs.len() as f64 / 1000.0);
        }
    }
    println!("unwrap_batch, median µs per wrap over {rounds} rounds (new senders every round)");
    for ((n, s), t) in shapes.iter().zip(times.iter_mut()) {
        t.sort_by(f64::total_cmp);
        println!("  {:<34} {:>7.1}", format!("{n} wraps, {s} sender(s)"), t[t.len() / 2]);
    }
    MY_SECRET_KEY.clear(&[]);
}
