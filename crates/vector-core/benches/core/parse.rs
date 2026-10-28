//! Reading the NIP-59 layers from JSON: `vector_core::event_json` against rust-nostr's readers.

use std::hint::black_box;
use std::time::Instant;

use nostr_sdk::prelude::*;
use vector_core::event_json;

/// Seals and rumors of 2,000 DMs, as a recipient decrypts them.
pub fn gift_wrap_layers() {
    const N: usize = 2_000;
    const ROUNDS: usize = 31;

    let me = Keys::generate();
    let peers: Vec<Keys> = (0..8).map(|_| Keys::generate()).collect();
    let line = "Did you see the \"new\" build? It's fast 🚀 https://vectorapp.io/?a=1&b=2\n";
    let (mut seals, mut rumors) = (Vec::with_capacity(N), Vec::with_capacity(N));
    for i in 0..N {
        let from = &peers[i % peers.len()];
        // Mostly short messages, some paragraphs, the odd long one.
        let text = match i % 10 {
            0..=5 => line[..30].to_string(),
            6..=8 => line.repeat(2),
            _ => line.repeat(16),
        };
        let mut tags = vec![Tag::parse(["p", &me.public_key().to_hex()]).unwrap()];
        if i % 4 == 0 {
            tags.push(Tag::parse(["e", &EventId::from_byte_array([7; 32]).to_hex(), "", "reply"]).unwrap());
        }
        let rumor = EventBuilder::new(Kind::PrivateDirectMessage, text).tags(tags).finalize_unsigned(from.public_key());
        let wrap = GiftWrapBuilder::new(me.public_key(), rumor).finalize(from).unwrap();
        let seal = nip44::decrypt(me.secret_key(), &wrap.pubkey, &wrap.content).unwrap();
        let parsed = Event::from_json(&seal).unwrap();
        rumors.push(nip44::decrypt(me.secret_key(), &parsed.pubkey, &parsed.content).unwrap());
        seals.push(seal);
    }
    let avg = |v: &[String]| v.iter().map(String::len).sum::<usize>() / v.len();
    println!("{N} DMs: seals avg {} B, rumors avg {} B; ns per event, median of {ROUNDS} rounds\n", avg(&seals), avg(&rumors));

    let time = |f: &dyn Fn()| {
        let mut ns: Vec<f64> = (0..ROUNDS)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed().as_nanos() as f64 / N as f64
            })
            .collect();
        ns.sort_by(f64::total_cmp);
        ns[ROUNDS / 2]
    };
    let seal_nostr = time(&|| seals.iter().for_each(|s| drop(black_box(Event::from_json(black_box(s)).unwrap()))));
    let seal_ours = time(&|| seals.iter().for_each(|s| drop(black_box(event_json::event(black_box(s)).unwrap()))));
    let rumor_nostr = time(&|| rumors.iter().for_each(|s| drop(black_box(UnsignedEvent::from_json(black_box(s)).unwrap()))));
    let rumor_ours = time(&|| rumors.iter().for_each(|s| drop(black_box(event_json::unsigned(black_box(s)).unwrap()))));

    println!("{:<8}{:>14}{:>14}{:>9}", "layer", "rust-nostr", "event_json", "speedup");
    println!("{:<8}{seal_nostr:>14.0}{seal_ours:>14.0}{:>8.1}x", "seal", seal_nostr / seal_ours);
    println!("{:<8}{rumor_nostr:>14.0}{rumor_ours:>14.0}{:>8.1}x", "rumor", rumor_nostr / rumor_ours);
}
