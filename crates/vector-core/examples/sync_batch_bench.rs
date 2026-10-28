//! A sync chunk of 64 gift wraps, unwrapped as four `unwrap_batch` calls of 16 (each one vault
//! read and one 16-signature batch) against one call of 64 (still four vault reads of 16, one
//! 64-signature batch). New senders every round.
//!
//!   cargo run --release -p vector-core --example sync_batch_bench [rounds]

use nostr_sdk::prelude::*;
use std::time::Instant;
use vector_core::crypto::GuardedSigner;
use vector_core::state::MY_SECRET_KEY;

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
    let rounds: usize = std::env::args().nth(1).map_or(21, |s| s.parse().expect("rounds"));
    let me = Keys::generate();
    MY_SECRET_KEY.store_from_keys(&me, &[]);
    let senders = [1usize, 4, 8, 16, 64];
    let sets: Vec<Vec<Vec<Event>>> = (0..rounds).map(|_| senders.iter().map(|&s| wraps(&me, 64, s)).collect()).collect();
    let (mut sixteens, mut one) = (vec![Vec::new(); senders.len()], vec![Vec::new(); senders.len()]);
    for (round, set) in sets.iter().enumerate() {
        for k in 0..senders.len() {
            let i = (k + round) % senders.len();
            let refs: Vec<&Event> = set[i].iter().collect();
            // Alternate which goes first, so neither always runs on a warm key cache.
            for first in [round % 2 == 0, round % 2 == 1] {
                let t = Instant::now();
                if first {
                    for part in refs.chunks(16) {
                        assert!(GuardedSigner::unwrap_batch(part).iter().all(Result::is_ok));
                    }
                    sixteens[i].push(t.elapsed().as_nanos() as f64 / 64.0 / 1000.0);
                } else {
                    assert!(GuardedSigner::unwrap_batch(&refs).iter().all(Result::is_ok));
                    one[i].push(t.elapsed().as_nanos() as f64 / 64.0 / 1000.0);
                }
            }
        }
    }
    let med = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    println!("64 wraps, median µs per wrap over {rounds} rounds");
    println!("  {:<22} {:>10} {:>10} {:>7}", "senders", "4 x 16", "1 x 64", "saved");
    for (i, s) in senders.iter().enumerate() {
        let (a, b) = (med(&mut sixteens[i]), med(&mut one[i]));
        println!("  {:<22} {:>10.1} {:>10.1} {:>6.1}%", s, a, b, 100.0 * (a - b) / a);
    }
    MY_SECRET_KEY.clear(&[]);
}
