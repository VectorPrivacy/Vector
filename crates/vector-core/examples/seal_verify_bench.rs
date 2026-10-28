//! Seal signatures alone: one at a time as `Event::verify_signature` checks them, against
//! `crypto::batch_verify`, by batch size and senders per batch, with what the verifier
//! counted: key-cache hits, batches that held. "cold": new senders every round, so only
//! repeats inside a batch hit the cache. "warm": the same senders every round, as contacts
//! recur from one sync batch to the next on the same thread.
//!
//!   cargo run --release -p vector-core --example seal_verify_bench [rounds]

use nostr_sdk::prelude::*;
use std::hint::black_box;
use std::time::Instant;
use vector_core::crypto::batch_verify;

const SHAPES: [(usize, usize, bool); 9] = [
    (16, 16, false),
    (16, 4, false),
    (16, 1, false),
    (16, 4, true),
    (64, 64, false),
    (64, 8, false),
    (64, 8, true),
    (256, 256, false),
    (256, 32, true),
];

fn seals(from: &[Keys], n: usize) -> Vec<Event> {
    (0..n).map(|i| EventBuilder::new(Kind::Seal, format!("sealed {i}")).finalize(&from[i % from.len()]).unwrap()).collect()
}

fn main() {
    let rounds: usize = std::env::args().nth(1).map_or(15, |s| s.parse().expect("rounds"));
    let warm_keys: Vec<Vec<Keys>> = SHAPES.iter().map(|&(_, s, _)| (0..s).map(|_| Keys::generate()).collect()).collect();
    let sets: Vec<Vec<Vec<Event>>> = (0..rounds)
        .map(|_| {
            SHAPES
                .iter()
                .zip(&warm_keys)
                .map(|(&(n, s, warm), keys)| {
                    if warm {
                        seals(keys, n)
                    } else {
                        seals(&(0..s).map(|_| Keys::generate()).collect::<Vec<_>>(), n)
                    }
                })
                .collect()
        })
        .collect();

    let mut one: Vec<Vec<f64>> = vec![Vec::new(); SHAPES.len()];
    let mut batch: Vec<Vec<f64>> = vec![Vec::new(); SHAPES.len()];
    let mut counted = vec![batch_verify::stats(); SHAPES.len()];
    for v in counted.iter_mut() {
        *v = Default::default();
    }
    for (round, set) in sets.iter().enumerate() {
        for k in 0..SHAPES.len() {
            let i = (k + round) % SHAPES.len();
            let refs: Vec<&Event> = set[i].iter().collect();
            let n = refs.len() as f64;

            let t = Instant::now();
            for e in &refs {
                black_box(e.verify_signature());
            }
            one[i].push(t.elapsed().as_nanos() as f64 / n / 1000.0);

            let before = batch_verify::stats();
            let t = Instant::now();
            let ok = batch_verify::signatures(&refs);
            batch[i].push(t.elapsed().as_nanos() as f64 / n / 1000.0);
            assert!(ok.iter().all(|v| *v));
            let after = batch_verify::stats();
            let c = &mut counted[i];
            c.batches += after.batches - before.batches;
            c.batches_passed += after.batches_passed - before.batches_passed;
            c.batched += after.batched - before.batched;
            c.one_at_a_time += after.one_at_a_time - before.one_at_a_time;
            c.key_hits += after.key_hits - before.key_hits;
            c.key_misses += after.key_misses - before.key_misses;
        }
    }
    let med = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    println!("seal signatures, median µs per signature over {rounds} rounds");
    println!("  {:<30} {:>9} {:>9} {:>8} {:>10} {:>9}", "batch", "one each", "batched", "speedup", "key hits", "held");
    for (i, &(n, s, warm)) in SHAPES.iter().enumerate() {
        let (a, b) = (med(&mut one[i]), med(&mut batch[i]));
        let c = counted[i];
        let lookups = (c.key_hits + c.key_misses).max(1);
        println!(
            "  {:<30} {:>9.1} {:>9.1} {:>7.2}x {:>9.0}% {:>4}/{:<4}",
            format!("{n} seals, {s} sender(s){}", if warm { ", warm" } else { "" }),
            a,
            b,
            a / b,
            100.0 * c.key_hits as f64 / lookups as f64,
            c.batches_passed,
            c.batches
        );
    }
}
