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

/// Raw ChaCha20 as NIP-44 runs it: the `chacha20` crate against Vector's own keystream.
pub fn chacha20_raw() {
    use chacha20::cipher::{KeyIvInit, StreamCipher};
    let (key, nonce) = ([7u8; 32], [9u8; 12]);
    for size in [256usize, 512, 1024, 2048, 4096, 16384, 65536] {
        let mut buf = vec![0u8; size];
        let iters = (32 * 1024 * 1024 / size).max(1000);
        let (mut theirs, mut ours) = (f64::MAX, f64::MAX);
        for _ in 0..5 {
            let t = std::time::Instant::now();
            for _ in 0..iters {
                let mut c = chacha20::ChaCha20::new(&key.into(), &nonce.into());
                c.apply_keystream(std::hint::black_box(&mut buf));
            }
            theirs = theirs.min(t.elapsed().as_nanos() as f64 / iters as f64);
            let t = std::time::Instant::now();
            for _ in 0..iters {
                vector_core::crypto::chachapoly::chacha20_xor(&key, &nonce, 0, std::hint::black_box(&mut buf));
            }
            ours = ours.min(t.elapsed().as_nanos() as f64 / iters as f64);
        }
        println!("{size:>6} B   chacha20 0.9 {theirs:>8.0} ns ({:.2} GB/s)   ours {ours:>8.0} ns ({:.2} GB/s)   {:.2}x", size as f64 / theirs, size as f64 / ours, theirs / ours);
    }
}

/// One NIP-44 v2 decrypt: rust-nostr's against Vector's, at a rumor's, a seal's and a page's size.
pub fn nip44_decrypt() {
    use nostr_sdk::prelude::nip44::v2::{decrypt_to_bytes, encrypt_to_bytes_with_nonce, ConversationKey};
    let ck = ConversationKey::new([5u8; 32]);
    for size in [487usize, 1124, 4096, 32_768] {
        let payload = encrypt_to_bytes_with_nonce(&ck, &vec![0x61u8; size], [3u8; 32]).unwrap();
        let iters = (16 * 1024 * 1024 / size).max(2000);
        let (mut theirs, mut ours) = (f64::MAX, f64::MAX);
        for _ in 0..5 {
            let t = std::time::Instant::now();
            for _ in 0..iters {
                std::hint::black_box(decrypt_to_bytes(&ck, std::hint::black_box(&payload)).unwrap());
            }
            theirs = theirs.min(t.elapsed().as_nanos() as f64 / iters as f64);
            let t = std::time::Instant::now();
            for _ in 0..iters {
                std::hint::black_box(vector_core::crypto::nip44::decrypt(&ck, std::hint::black_box(payload.clone())).unwrap());
            }
            ours = ours.min(t.elapsed().as_nanos() as f64 / iters as f64);
        }
        println!("{size:>6} B plaintext   rust-nostr {theirs:>7.0} ns   ours (incl. the payload copy) {ours:>7.0} ns   {:.2}x", theirs / ours);
    }
}

/// Each ChaCha20 backend forced, by message size: where the crossovers sit on this CPU.
pub fn chacha20_backends() {
    use vector_core::crypto::chachapoly::bench as cb;
    let (key, nonce) = ([7u8; 32], [9u8; 12]);
    let names = cb::chacha_backends();
    print!("{:>7}", "bytes");
    for n in &names {
        print!("{n:>10}");
    }
    println!("   (GB/s)");
    for size in [64usize, 128, 256, 384, 512, 640, 1024, 2048, 4096, 16384, 65536] {
        let mut buf = vec![0u8; size];
        let iters = (32 * 1024 * 1024 / size).max(2000);
        print!("{size:>7}");
        for n in &names {
            let mut best = f64::MAX;
            for _ in 0..5 {
                let t = std::time::Instant::now();
                for _ in 0..iters {
                    cb::chacha20_xor_on(n, &key, &nonce, std::hint::black_box(&mut buf));
                }
                best = best.min(t.elapsed().as_nanos() as f64 / iters as f64);
            }
            print!("{:>10.2}", size as f64 / best);
        }
        println!();
    }
}
