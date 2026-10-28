//! A page of Community history opened the way `fetch_channel_history` does: each wrap through
//! `open_wrap`, with and without the page's signatures checked together first
//! (`stream::check_signatures_together`). Pages of 200 (catch-up) and 500 (a channel opened),
//! written by 1, 8, 50 or all-different authors, new authors every round.
//!
//!   cargo run --release -p vector-core --example community_page_bench [rounds]

use nostr_sdk::prelude::*;
use std::hint::black_box;
use std::time::Instant;
use vector_core::community::v2::derive::channel_group_key;
use vector_core::community::v2::kind;
use vector_core::community::v2::stream::{self, SealForm, KIND_WRAP};
use vector_core::community::{ChannelId, Epoch};
use vector_core::crypto::batch_verify;

const SHAPES: [(usize, usize); 8] = [(200, 1), (200, 8), (200, 50), (200, 200), (500, 1), (500, 8), (500, 50), (500, 500)];

fn main() {
    let rounds: usize = std::env::args().nth(1).map_or(9, |s| s.parse().expect("rounds"));
    let chan = ChannelId([0xab; 32]);
    let group = channel_group_key(&[7u8; 32], &chan, Epoch(0));
    let page = |n: usize, authors: usize| -> Vec<Event> {
        let keys: Vec<Keys> = (0..authors).map(|_| Keys::generate()).collect();
        (0..n)
            .map(|i| {
                let tags = stream::channel_binding_tags(&chan, Epoch(0));
                let text = format!("message {i}: anyone up for a match tonight?");
                let rumor = stream::build_rumor_ms(kind::MESSAGE, keys[i % authors].public_key(), &text, tags, 1_761_600_000_000 + i as u64);
                let seal = stream::build_seal(&rumor, SealForm::Encrypted, &group, &keys[i % authors]).unwrap();
                stream::wrap_seal(&seal, &group, KIND_WRAP, Timestamp::from_secs(1_761_600_000)).unwrap().0
            })
            .collect()
    };
    let sets: Vec<Vec<Vec<Event>>> = (0..rounds).map(|_| SHAPES.iter().map(|&(n, a)| page(n, a)).collect()).collect();

    let (mut alone, mut together) = (vec![Vec::new(); SHAPES.len()], vec![Vec::new(); SHAPES.len()]);
    let mut hits = vec![(0u64, 0u64, 0u64, 0u64); SHAPES.len()];
    for (round, set) in sets.iter().enumerate() {
        for k in 0..SHAPES.len() {
            let i = (k + round) % SHAPES.len();
            let wraps = &set[i];
            let n = wraps.len() as f64;

            let t = Instant::now();
            for w in wraps {
                black_box(stream::open_wrap(w, &group).unwrap());
            }
            alone[i].push(t.elapsed().as_nanos() as f64 / n / 1000.0);

            let before = batch_verify::stats();
            let t = Instant::now();
            let to_open: Vec<_> = wraps.iter().map(|w| (w, vec![group.conv_key()], false)).collect();
            let batch = stream::check_signatures_together(&to_open);
            for w in wraps {
                black_box(stream::open_wrap(w, &group).unwrap());
            }
            drop(batch);
            together[i].push(t.elapsed().as_nanos() as f64 / n / 1000.0);
            let after = batch_verify::stats();
            let h = &mut hits[i];
            h.0 += after.key_hits - before.key_hits;
            h.1 += after.key_misses - before.key_misses;
            h.2 += after.batches_passed - before.batches_passed;
            h.3 += after.batches - before.batches;
        }
    }
    let med = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    println!("Community history page, median µs per message over {rounds} rounds");
    println!("  {:<28} {:>9} {:>9} {:>8} {:>7} {:>9}", "page", "one each", "batched", "saved", "keys", "held");
    for (i, &(n, a)) in SHAPES.iter().enumerate() {
        let (x, y) = (med(&mut alone[i]), med(&mut together[i]));
        let (kh, km, held, batches) = hits[i];
        println!(
            "  {:<28} {:>9.1} {:>9.1} {:>7.0}% {:>6.0}% {:>4}/{:<4}",
            format!("{n} messages, {a} author(s)"),
            x,
            y,
            100.0 * (x - y) / x,
            100.0 * kh as f64 / (kh + km).max(1) as f64,
            held,
            batches
        );
    }
}
