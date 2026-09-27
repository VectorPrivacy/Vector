//! Microbenchmarks for the hot crypto paths: the at-rest AEAD, attachment AES-GCM,
//! vault reads and gift-wrap unwrap.
//!
//! Usage: cargo run --release -p vector-core --example crypto_bench

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

#[tokio::main]
async fn main() {
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

    #[cfg(target_arch = "x86_64")]
    {
        use vector_core::crypto::chachapoly::bench::{poly1305_avx2, poly1305_scalar, AVX2_MIN};
        println!("-- Poly1305 alone, scalar vs AVX2 lanes (the AEAD switches at {AVX2_MIN} B, set from the full AEAD)");
        let key = [0x5au8; 32];
        let data = vec![0xa7u8; 4096];
        if poly1305_avx2(&key, &data[..64]).is_some() {
            for len in (256..=768).step_by(64).chain([1024, 4096]) {
                let d = &data[..len];
                let s = bench(&format!("  scalar {len} B"), 100_000, || { black_box(poly1305_scalar(&key, black_box(d))); });
                let a = bench(&format!("  avx2   {len} B"), 100_000, || { black_box(poly1305_avx2(&key, black_box(d))); });
                println!("  -> {:.2}x{}", s / a, if a < s { "  (avx2 wins)" } else { "" });
            }
        } else {
            println!("  no AVX2 on this CPU");
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
    println!("{:<52} {:>10.2} µs", "unwrap via active_signer + GuardedSigner (prod)", g_ns / 1000.0);

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
