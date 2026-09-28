//! Microbenchmarks for the hot crypto paths: the at-rest AEAD, attachment AES-GCM,
//! vault reads and gift-wrap unwrap.
//!
//! Usage: cargo run --release -p vector-core --example crypto_bench
//!        cargo run --release -p vector-core --example crypto_bench -- sweep
//!
//! `sweep` races every ChaCha20 backend this CPU has per message length, then pairs the
//! AEAD with Poly1305 lanes off and on, in interleaved rounds, to find crossovers; then
//! AES-GCM engines and gift-wrap unwrap paths the same way.

use nostr_sdk::prelude::*;
use std::hint::black_box;
use std::time::Instant;
use vector_core::crypto::GuardedSigner;
use vector_core::state::{ENCRYPTION_KEY, MY_SECRET_KEY};

fn bench<F: FnMut()>(name: &str, iters: u64, mut f: F) -> f64 {
    for _ in 0..(iters / 10).max(1) {
        f();
    }
    let t = Instant::now();
    for _ in 0..iters {
        f();
    }
    let ns = t.elapsed().as_nanos() as f64 / iters as f64;
    println!("{name:<52} {:>10.2} µs", ns / 1000.0);
    ns
}

/// One timed run of `iters` calls, in ns per call.
fn once<F: FnMut()>(iters: u64, f: &mut F) -> f64 {
    let t = Instant::now();
    for _ in 0..iters {
        f();
    }
    t.elapsed().as_nanos() as f64 / iters as f64
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn sweep() {
    use vector_core::crypto::chachapoly::bench;
    const ROUNDS: usize = 15;
    const PAIRS: usize = 31;
    let key = [7u8; 32];
    let nonce = [1u8; 12];
    let backends = bench::chacha_backends();
    let poly_min = bench::poly_simd_min();
    println!("arch {}; ChaCha20 backends {:?}; production Poly1305 lanes from {}",
        std::env::consts::ARCH, backends,
        if poly_min == usize::MAX { "never".to_string() } else { format!("{poly_min} B") });

    println!("\n== ChaCha20 backend per message length: median ns per seal, rounds won of {ROUNDS}");
    println!("   (blocks = keystream blocks the first pass wants: 1 for the Poly1305 key + data)");
    print!("{:>6} {:>6}", "bytes", "blocks");
    for b in &backends {
        print!("  {b:>16}");
    }
    println!();
    for len in [0usize, 16, 32, 48, 64, 96, 128, 160, 192, 224, 256, 320, 384, 448, 512, 768, 1024, 2048, 4096, 16384] {
        let ciphers: Vec<_> = backends.iter().map(|b| bench::cipher(&key, b, poly_min).unwrap()).collect();
        let mut buf = vec![0x61u8; len];
        let iters = (2_000_000 / (len as u64 + 128)).max(32);
        let mut times = vec![Vec::with_capacity(ROUNDS); backends.len()];
        let mut wins = vec![0usize; backends.len()];
        for c in &ciphers {
            once(iters / 4 + 1, &mut || { black_box(c.seal_in_place(&nonce, &[], black_box(&mut buf[..])).unwrap()); });
        }
        for round in 0..ROUNDS {
            let mut row = vec![0.0; backends.len()];
            for k in 0..backends.len() {
                let i = (k + round) % backends.len();
                let c = &ciphers[i];
                row[i] = once(iters, &mut || { black_box(c.seal_in_place(&nonce, &[], black_box(&mut buf[..])).unwrap()); });
            }
            let best = (0..row.len()).min_by(|&a, &b| row[a].partial_cmp(&row[b]).unwrap()).unwrap();
            wins[best] += 1;
            for i in 0..row.len() {
                times[i].push(row[i]);
            }
        }
        print!("{len:>6} {:>6}", 1 + len.div_ceil(64));
        for i in 0..backends.len() {
            print!("  {:>9.1} ({:>2}/{ROUNDS})", median(&mut times[i]), wins[i]);
        }
        println!();
    }

    gcm_sweep();

    if !bench::poly_simd_available() {
        println!("\n(no Poly1305 lanes on this CPU)");
        return;
    }
    println!("\n== Poly1305 lanes in the whole AEAD: median of scalar/lanes time ratio, pairs lanes won of {PAIRS}");
    let chacha = bench::default_chacha_backend();
    println!("   (ChaCha20 on the production backend, {chacha})");
    let off = bench::cipher(&key, chacha, usize::MAX).unwrap();
    let on = bench::cipher(&key, chacha, 64).unwrap();
    for len in [256usize, 384, 512, 640, 704, 768, 896, 960, 1024, 1088, 1152, 1280, 1408, 1536, 2048, 4096, 16384, 65536] {
        let mut buf = vec![0x61u8; len];
        let iters = (2_000_000 / (len as u64 + 256)).max(32);
        let seal = |c: &vector_core::crypto::chachapoly::ChaCha20Poly1305, buf: &mut Vec<u8>| {
            once(iters, &mut || { black_box(c.seal_in_place(&nonce, &[], black_box(&mut buf[..])).unwrap()); })
        };
        seal(&off, &mut buf);
        seal(&on, &mut buf);
        let mut ratios: Vec<f64> = (0..PAIRS)
            .map(|i| if i % 2 == 0 {
                let a = seal(&off, &mut buf);
                a / seal(&on, &mut buf)
            } else {
                let b = seal(&on, &mut buf);
                seal(&off, &mut buf) / b
            })
            .collect();
        let wins = ratios.iter().filter(|&&r| r > 1.0).count();
        println!("{len:>6} B   {:>6.3}x   lanes won {wins:>2}/{PAIRS}", median(&mut ratios));
    }
}

/// The attachment cipher's two engines, paired per size with a fresh key each call as
/// production does.
fn gcm_sweep() {
    use vector_core::crypto::gcm::bench;
    const PAIRS: usize = 21;
    if !bench::stitched_present() {
        println!("\n(no stitched AES-GCM on this CPU)");
        return;
    }
    println!("\n== AES-256-GCM engines: median generic/stitched time ratio, pairs stitched won of {PAIRS}");
    println!("   (production uses stitched on this CPU: {})", bench::stitched_in_production());
    let key = [7u8; 32];
    let nonce = [1u8; 16];
    for len in [0usize, 16, 64, 128, 256, 512, 1024, 4096, 65536, 1 << 20] {
        let pt = vec![0x5au8; len];
        let mut sealed = pt.clone();
        let mut s = bench::stream("generic", &key, &nonce).unwrap();
        s.encrypt(&mut sealed);
        let tag = s.finish();
        let iters = (2_000_000 / (len as u64 + 512)).max(16);
        let mut buf = pt.clone();
        let mut run = |engine: &str, open: bool| once(iters, &mut || {
            let mut s = bench::stream(engine, &key, &nonce).unwrap();
            if open {
                buf.copy_from_slice(&sealed);
                s.decrypt(&mut buf);
                assert_eq!(s.finish(), tag);
            } else {
                s.encrypt(&mut buf);
                black_box(s.finish());
            }
        });
        let mut line = format!("{len:>8} B");
        for open in [false, true] {
            run("generic", open);
            run("stitched", open);
            let mut ratios: Vec<f64> = (0..PAIRS)
                .map(|i| if i % 2 == 0 {
                    let g = run("generic", open);
                    g / run("stitched", open)
                } else {
                    let st = run("stitched", open);
                    run("generic", open) / st
                })
                .collect();
            let wins = ratios.iter().filter(|&&r| r > 1.0).count();
            line += &format!("   {} {:>6.3}x ({wins:>2}/{PAIRS})", if open { "open" } else { "seal" }, median(&mut ratios));
        }
        println!("{line}");
    }
}

/// Gift-wrap unwrap per wrap, one vault read per wrap against one per batch, with every
/// DM from a different sender or all from one. "prior" adds back the two wrap signature
/// checks (nostr-sdk's on arrival, then the unwrap's) that the ID checks replaced.
fn gift_sweep() {
    const ROUNDS: usize = 21;
    let me = Keys::generate();
    MY_SECRET_KEY.store_from_keys(&me, &[]);
    let n = vector_core::event_handler::UNWRAP_BATCH;
    let batch_of = |senders: usize| -> Vec<Event> {
        let from: Vec<Keys> = (0..senders).map(|_| Keys::generate()).collect();
        (0..n)
            .map(|i| {
                let r = EventBuilder::new(Kind::PrivateDirectMessage, "hey! are we still on for tomorrow?")
                    .tag(Tag::public_key(me.public_key()))
                    .finalize_unsigned(from[i % senders].public_key());
                GiftWrapBuilder::new(me.public_key(), r).finalize(&from[i % senders]).unwrap()
            })
            .collect()
    };
    let (many, one) = (batch_of(n), batch_of(1));
    let (many, one): (Vec<&Event>, Vec<&Event>) = (many.iter().collect(), one.iter().collect());
    type Case<'a> = (&'static str, Box<dyn Fn() + 'a>);
    let cases: Vec<Case> = vec![
        ("prior: sdk verify + verify + read per wrap", Box::new(|| for w in &many {
            black_box(w.verify().is_ok());
            black_box(w.verify().is_ok());
            black_box(GuardedSigner::unwrap_gift_wrap(w).unwrap());
        })),
        ("sdk arrival check: id only (now)", Box::new(|| for w in &many {
            black_box(w.verify_id());
        })),
        ("single: vault read per wrap", Box::new(|| for w in &many {
            black_box(GuardedSigner::unwrap_gift_wrap(w).unwrap());
        })),
        ("batch, every wrap a new sender", Box::new(|| { black_box(GuardedSigner::unwrap_batch(&many)); })),
        ("batch, one sender", Box::new(|| { black_box(GuardedSigner::unwrap_batch(&one)); })),
    ];
    let mut times: Vec<Vec<f64>> = vec![Vec::new(); cases.len()];
    for round in 0..ROUNDS {
        for k in 0..cases.len() {
            let i = (k + round) % cases.len();
            let t = Instant::now();
            (cases[i].1)();
            times[i].push(t.elapsed().as_nanos() as f64 / n as f64 / 1000.0);
        }
    }
    println!("\n== Gift-wrap unwrap: median µs per wrap over {ROUNDS} interleaved rounds of {n}");
    let base = median(&mut times[0].clone());
    for ((name, _), t) in cases.iter().zip(times.iter_mut()) {
        let m = median(t);
        println!("   {name:<40} {m:>8.1} µs   {:>5.2}x", base / m);
    }
    MY_SECRET_KEY.clear(&[]);
}

#[tokio::main]
async fn main() {
    if std::env::args().any(|a| a == "sweep") {
        sweep();
        gift_sweep();
        return;
    }
    let me = Keys::generate();
    let sender = Keys::generate();
    MY_SECRET_KEY.store_from_keys(&me, &[&ENCRYPTION_KEY]);
    ENCRYPTION_KEY.set([7u8; 32], &[&MY_SECRET_KEY]);
    vector_core::state::set_my_public_key(me.public_key());

    println!("-- at-rest AEAD: ChaCha20-Poly1305 seal, ours vs RustCrypto");
    {
        use chacha20poly1305::{AeadInPlace, KeyInit};
        let key = [7u8; 32];
        let nonce = [1u8; 12];
        let theirs = chacha20poly1305::ChaCha20Poly1305::new(&key.into());
        let ours = vector_core::crypto::chachapoly::ChaCha20Poly1305::new(&key);
        for size in [32usize, 128, 512, 4096, 65536] {
            let mut buf = vec![0x61u8; size];
            let iters = (4_000_000 / (size as u64 + 256)).max(50);
            let a = bench(&format!("  RustCrypto {size} B"), iters, || {
                black_box(theirs.encrypt_in_place_detached((&nonce).into(), &[], black_box(&mut buf[..])).unwrap());
            });
            let b = bench(&format!("  ours       {size} B"), iters, || {
                black_box(ours.seal_in_place(&nonce, &[], black_box(&mut buf[..])).unwrap());
            });
            println!("  -> {:.2}x", a / b);
        }
    }

    {
        use vector_core::crypto::chachapoly::bench::{poly1305_scalar, poly1305_simd, poly_simd_min};
        println!("-- Poly1305 alone, scalar vs lanes (`sweep` measures the AEAD switch point)");
        let key = [0x5au8; 32];
        let data = vec![0xa7u8; 4096];
        if poly1305_simd(&key, &data[..64]).is_some() {
            let _ = poly_simd_min();
            for len in [256usize, 512, 1024, 4096] {
                let d = &data[..len];
                let s = bench(&format!("  scalar {len} B"), 100_000, || { black_box(poly1305_scalar(&key, black_box(d))); });
                let a = bench(&format!("  lanes  {len} B"), 100_000, || { black_box(poly1305_simd(&key, black_box(d))); });
                println!("  -> {:.2}x", s / a);
            }
        } else {
            println!("  no lanes on this CPU");
        }
    }

    println!("-- attachments: AES-256-GCM, 16-byte nonce (hardware AES: {})", aes::hardware_accelerated());
    {
        let params = vector_core::crypto::generate_encryption_params();
        let file = vec![0u8; 4 << 20];
        let ns = bench("  encrypt_data 4 MiB", 10, || {
            black_box(vector_core::crypto::encrypt_data(black_box(&file), &params).unwrap());
        });
        println!("  -> {:.0} MB/s", 4.194304e9 / ns);
    }

    println!("-- vault");
    bench("GuardedKey::get()", 2000, || { black_box(ENCRYPTION_KEY.get()); });
    bench("GuardedKey::to_keys() (get + pubkey derive)", 2000, || { black_box(MY_SECRET_KEY.to_keys()); });
    bench("Keys::new(sk) alone", 2000, || { black_box(Keys::new(me.secret_key().clone())); });
    bench("signer::active_signer()", 2000, || { black_box(vector_core::signer::active_signer().ok()); });

    println!("-- at-rest");
    let msg = "hey! are we still on for tomorrow? I'll bring the snacks 🍿".to_string();
    let enc = vector_core::crypto::encrypt_with_key(&msg, &[7u8; 32]).unwrap();
    bench("decrypt_with_key (explicit key)", 20000, || {
        black_box(vector_core::crypto::decrypt_with_key(black_box(&enc), &[7u8; 32]).unwrap());
    });
    bench("maybe_decrypt_text (vault per call)", 2000, || {
        black_box(vector_core::crypto::maybe_decrypt_text(black_box(&enc)));
    });

    println!("-- gift wrap (NIP-59, two NIP-44 layers + seal verify)");
    let rumor = EventBuilder::new(Kind::PrivateDirectMessage, msg.clone())
        .tag(Tag::public_key(me.public_key()))
        .finalize_unsigned(sender.public_key());
    let wrap = GiftWrapBuilder::new(me.public_key(), rumor).finalize(&sender).unwrap();
    let guarded = GuardedSigner::new(me.public_key());
    let n = 300u64;
    let t = Instant::now();
    for _ in 0..n {
        black_box(UnwrappedGift::from_gift_wrap(&me, &wrap).unwrap());
    }
    let keys_ns = t.elapsed().as_nanos() as f64 / n as f64;
    println!("{:<52} {:>10.2} µs", "unwrap via raw Keys", keys_ns / 1000.0);
    let t = Instant::now();
    for _ in 0..n {
        let _s = vector_core::signer::active_signer().unwrap();
        black_box(UnwrappedGift::from_gift_wrap(&guarded, &wrap).unwrap());
    }
    let g_ns = t.elapsed().as_nanos() as f64 / n as f64;
    println!("{:<52} {:>10.2} µs", "unwrap via active_signer + GuardedSigner (old)", g_ns / 1000.0);
    let t = Instant::now();
    for _ in 0..n {
        black_box(vector_core::signer::unwrap_gift_wrap(&wrap).await.unwrap());
    }
    let p_ns = t.elapsed().as_nanos() as f64 / n as f64;
    println!("{:<52} {:>10.2} µs", "unwrap via signer::unwrap_gift_wrap (prod)", p_ns / 1000.0);

    let batch_of = |senders: usize| -> Vec<Event> {
        let from: Vec<Keys> = (0..senders).map(|_| Keys::generate()).collect();
        (0..vector_core::event_handler::UNWRAP_BATCH)
            .map(|i| {
                let r = EventBuilder::new(Kind::PrivateDirectMessage, msg.clone())
                    .tag(Tag::public_key(me.public_key()))
                    .finalize_unsigned(from[i % senders].public_key());
                GiftWrapBuilder::new(me.public_key(), r).finalize(&from[i % senders]).unwrap()
            })
            .collect()
    };
    for senders in [vector_core::event_handler::UNWRAP_BATCH, 4, 1] {
        let wraps = batch_of(senders);
        let refs: Vec<&Event> = wraps.iter().collect();
        let rounds = 40u64;
        let t = Instant::now();
        for _ in 0..rounds {
            black_box(vector_core::signer::unwrap_gift_wraps(&refs).await);
        }
        let per = t.elapsed().as_nanos() as f64 / (rounds as f64 * refs.len() as f64);
        println!("{:<52} {:>10.2} µs", format!("batch of {}, {} sender(s), per wrap", refs.len(), senders), per / 1000.0);
    }

    println!("-- unwrap breakdown (raw keys)");
    let seal = Event::from_json(nostr_sdk::prelude::nip44::decrypt(me.secret_key(), &wrap.pubkey, &wrap.content).unwrap()).unwrap();
    bench("wrap.verify() (schnorr, outer)", 2000, || { black_box(wrap.verify().is_ok()); });
    bench("nip44::decrypt (wrap layer)", 2000, || {
        black_box(nostr_sdk::prelude::nip44::decrypt(me.secret_key(), &wrap.pubkey, &wrap.content).unwrap());
    });
    bench("nip44 ConversationKey::derive (ECDH+HKDF)", 2000, || {
        black_box(nostr_sdk::prelude::nip44::v2::ConversationKey::derive(me.secret_key(), &wrap.pubkey).unwrap());
    });
    let ck = nostr_sdk::prelude::nip44::v2::ConversationKey::derive(me.secret_key(), &wrap.pubkey).unwrap();
    let payload = base64_simd::STANDARD.decode_to_vec(wrap.content.as_bytes()).unwrap();
    bench("nip44 v2 decrypt_to_bytes (given conv key)", 20000, || {
        black_box(nostr_sdk::prelude::nip44::v2::decrypt_to_bytes(&ck, &payload).unwrap());
    });
    bench("seal.verify() (schnorr)", 2000, || { black_box(seal.verify().is_ok()); });
    bench("Event::from_json(seal)", 20000, || { black_box(Event::from_json(seal.as_json()).unwrap()); });
    println!("wrap content: {} b64 chars, seal content: {} chars", wrap.content.len(), seal.content.len());
}
