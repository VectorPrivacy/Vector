//! Inbound gift-wrap unwrap cost: every received DM pays it, and a sync pays it
//! once per backlogged wrap.
//!
//! Each `GuardedKey` read is deliberately expensive (randomised 4096..8191 mix
//! rounds per launch), so absolute numbers swing between runs; compare rows from
//! the same run.
//!
//! `#[ignore]`d: a measurement, not an assertion. Run deliberately:
//!   cargo test --release -p vector-core --test bench_unwrap -- --ignored --nocapture

use std::hint::black_box;
use std::time::Instant;

use nostr_sdk::prelude::*;
use vector_core::state::{ENCRYPTION_KEY, MY_SECRET_KEY};
use vector_core::GuardedSigner;

const ITERS: u32 = 2_000;

fn bench(name: &str, mut f: impl FnMut()) {
    for _ in 0..ITERS / 10 {
        f();
    }
    let mut best = f64::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        for _ in 0..ITERS {
            f();
        }
        best = best.min(t.elapsed().as_nanos() as f64 / ITERS as f64);
    }
    println!("{name:<48} {:>8.1} µs", best / 1000.0);
}

#[test]
#[ignore = "benchmark, not an assertion"]
fn bench_gift_wrap_unwrap() {
    let me = Keys::generate();
    let sender = Keys::generate();
    MY_SECRET_KEY.store_from_keys(&me, &[&ENCRYPTION_KEY]);
    let signer = GuardedSigner::new(me.public_key());

    let rumor = EventBuilder::new(Kind::PrivateDirectMessage, "a typical short DM of a few dozen bytes")
        .tag(Tag::public_key(me.public_key()))
        .finalize_unsigned(sender.public_key());
    let wraps: Vec<Event> = (0..64)
        .map(|_| nip59::GiftWrapBuilder::new(me.public_key(), rumor.clone()).finalize(&sender).unwrap())
        .collect();
    let w = &wraps[0];

    bench("vault get()", || { black_box(MY_SECRET_KEY.get()); });
    bench("vault to_keys()", || { black_box(MY_SECRET_KEY.to_keys()); });
    bench("event.verify()", || { w.verify().unwrap(); });
    bench("to_keys() + nip44 decrypt (signer path before)", || {
        let keys = MY_SECRET_KEY.to_keys().unwrap();
        black_box(Nip44::nip44_decrypt(&keys, &w.pubkey, &w.content).unwrap());
    });
    bench("GuardedSigner nip44_decrypt (one layer)", || {
        black_box(Nip44::nip44_decrypt(&signer, &w.pubkey, &w.content).unwrap());
    });

    let mut i = 0;
    bench("nip59 UnwrappedGift::from_gift_wrap(GuardedSigner)", || {
        i += 1;
        black_box(nip59::UnwrappedGift::from_gift_wrap(&signer, &wraps[i % wraps.len()]).unwrap());
    });
    let mut i = 0;
    bench("GuardedSigner::unwrap_gift_wrap", || {
        i += 1;
        black_box(signer.unwrap_gift_wrap(&wraps[i % wraps.len()]).unwrap());
    });

    MY_SECRET_KEY.clear(&[&ENCRYPTION_KEY]);
}
