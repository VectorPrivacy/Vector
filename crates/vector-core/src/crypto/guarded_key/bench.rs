//! The vault's benchmarks, run by `benches/core` (`cargo bench -p vector-core --features bench`).
//! In-crate because they time the vault's private internals.

use super::fixtures::{setup, HotPaths, PARTNER, SETTER};
use super::*;
use crate::rt::time_std::Instant;

static KEY: GuardedKey = GuardedKey::empty();
static OTHER: GuardedKey = GuardedKey::empty();

fn key(seed: u8) -> [u8; 32] {
    std::array::from_fn(|i| seed.wrapping_add(i as u8).wrapping_mul(37).wrapping_add(7))
}

/// Prices each candidate vault design's read, all over the same live vault so cache
/// behaviour matches production.
pub fn designs() {
    use sha2::Digest;
    use std::hint::black_box;
    ensure_vaults();
    KEY.set(key(9), &[&OTHER]);
    let addr = KEY.instance_addr();
    let lane = KEY.lane();
    // v1's round count (4096..8191), for comparison.
    let today = {
        let base = VAULTS.as_ptr() as usize;
        let h = (addr ^ base).wrapping_mul(addr | 1);
        4096 + ((h >> 7) & 4095)
    };
    let few = mix_iterations(addr);

    let read = |iters: usize| -> [u8; 32] {
        let lay = layout_with(addr, lane, iters);
        let positions = share_positions_from(addr, lane, &lay);
        let mut key = [0u8; 32];
        for (i, pos) in positions.iter().enumerate() {
            let v = VAULTS[pos.array][pos.slot].load(Ordering::Acquire).to_ne_bytes();
            let off = (i % USIZES_PER_SHARE) * 8;
            for (k, b) in key[off..off + 8].iter_mut().zip(v) { *k ^= b; }
        }
        key
    };
    // Prekey: `kib` KiB as whole 2 KiB lane rows (256 slots) across consecutive arrays,
    // streamed through a 2 KiB stack buffer that is wiped after each read.
    fn prekey_rows(lane: usize, kib: usize, mut f: impl FnMut(&[u8])) {
        let mut buf = [0u8; LANE_WIDTH * 8];
        for row in 0..kib / 2 {
            let arr = &VAULTS[(lane * 8 + row) & (ARRAY_COUNT - 1)];
            for (j, slot) in arr[lane * LANE_WIDTH..(lane + 1) * LANE_WIDTH].iter().enumerate() {
                buf[j * 8..j * 8 + 8].copy_from_slice(&slot.load(Ordering::Relaxed).to_ne_bytes());
            }
            f(&buf);
        }
        buf.zeroize();
    }
    let shield_blake3 = |kib: usize| -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        prekey_rows(lane, kib, |b| { h.update(b); });
        *h.finalize().as_bytes()
    };
    let shield_sha256 = |kib: usize| -> [u8; 32] {
        let mut h = sha2::Sha256::new();
        prekey_rows(lane, kib, |b| h.update(b));
        h.finalize().into()
    };
    let shield_sha512 = |kib: usize| -> [u8; 32] {
        let mut h = sha2::Sha512::new();
        prekey_rows(lane, kib, |b| h.update(b));
        h.finalize()[..32].try_into().unwrap()
    };
    // Whole prekey in one buffer, so BLAKE3 can hash its 1 KiB chunks in parallel SIMD lanes.
    fn prekey_flat(lane: usize, kib: usize, f: impl FnOnce(&[u8]) -> [u8; 32]) -> [u8; 32] {
        let mut buf = [0u8; 64 * 1024];
        let rows = kib / 2;
        for row in 0..rows {
            let arr = &VAULTS[(lane * 8 + row) & (ARRAY_COUNT - 1)];
            let dst = &mut buf[row * LANE_WIDTH * 8..(row + 1) * LANE_WIDTH * 8];
            for (j, slot) in arr[lane * LANE_WIDTH..(lane + 1) * LANE_WIDTH].iter().enumerate() {
                dst[j * 8..j * 8 + 8].copy_from_slice(&slot.load(Ordering::Relaxed).to_ne_bytes());
            }
        }
        let out = f(&buf[..kib * 1024]);
        buf[..kib * 1024].zeroize();
        out
    }
    let copy_only = |kib: usize| prekey_flat(lane, kib, |b| [b[0]; 32]);
    let flat_blake3 = |kib: usize| prekey_flat(lane, kib, |b| *blake3::hash(b).as_bytes());
    // Hash the prekey where it lies: a contiguous run of one array, no stack copy to wipe.
    // Sound only while nothing writes those slots during a read (set() would own that).
    let direct_blake3 = |kib: usize| -> [u8; 32] {
        let arr = &VAULTS[(lane * 8) & (ARRAY_COUNT - 1)];
        let bytes = unsafe { std::slice::from_raw_parts(arr.as_ptr() as *const u8, kib * 1024) };
        *blake3::hash(bytes).as_bytes()
    };
    let direct_sha512 = |kib: usize| -> [u8; 32] {
        let arr = &VAULTS[(lane * 8) & (ARRAY_COUNT - 1)];
        let bytes = unsafe { std::slice::from_raw_parts(arr.as_ptr() as *const u8, kib * 1024) };
        sha2::Sha512::digest(bytes)[..32].try_into().unwrap()
    };
    let unshield = |mut k: [u8; 32], pad: [u8; 32]| { for (a, b) in k.iter_mut().zip(pad) { *a ^= b; } k };

    // Median of 15 runs of `n` calls, in µs per call.
    let time = |n: usize, f: &dyn Fn() -> [u8; 32]| -> f64 {
        let mut runs: Vec<f64> = (0..15).map(|_| {
            let t = Instant::now();
            for _ in 0..n { black_box(f()); }
            t.elapsed().as_nanos() as f64 / n as f64 / 1000.0
        }).collect();
        runs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        runs[7]
    };

    eprintln!("BENCH rounds today={today} few={few}");
    let base = time(300, &|| KEY.get().unwrap());
    eprintln!("BENCH {:<44} {:>8.2} µs", "get() today", base);
    let row = |name: &str, us: f64| eprintln!("BENCH {name:<44} {us:>8.2} µs  ({:+.1}%)", (us / base - 1.0) * 100.0);
    row("today's rounds, re-implemented", time(300, &|| read(today)));
    row("few rounds", time(20000, &|| read(few)));
    for kib in [4, 16, 64] {
        row(&format!("few rounds + BLAKE3 shield {kib} KiB"), time(5000, &|| unshield(read(few), shield_blake3(kib))));
    }
    row("few rounds + SHA-256 shield 16 KiB", time(2000, &|| unshield(read(few), shield_sha256(16))));
    row("few rounds + SHA-512 shield 16 KiB (OpenSSH)", time(2000, &|| unshield(read(few), shield_sha512(16))));
    for kib in [4, 16, 64] {
        row(&format!("  prekey copy + wipe only, {kib} KiB"), time(5000, &|| copy_only(kib)));
        row(&format!("few rounds + BLAKE3 one-shot {kib} KiB"), time(5000, &|| unshield(read(few), flat_blake3(kib))));
    }
    for kib in [4, 16, 32] {
        row(&format!("few rounds + BLAKE3 in place {kib} KiB"), time(5000, &|| unshield(read(few), direct_blake3(kib))));
    }
    row("few rounds + SHA-512 in place 16 KiB", time(2000, &|| unshield(read(few), direct_sha512(16))));
    row("today's rounds + BLAKE3 shield 16 KiB", time(300, &|| unshield(read(today), shield_blake3(16))));
    KEY.clear(&[&OTHER]);
}

/// Prices set()'s randomness sources and a vault-wide writer lock.
///
pub fn set_sources() {
    use rand::{RngCore, SeedableRng};
    use std::hint::black_box;
    ensure_vaults();
    let time = |n: usize, f: &mut dyn FnMut()| -> f64 {
        let mut runs: Vec<f64> = (0..15).map(|_| {
            let t = Instant::now();
            for _ in 0..n { f(); }
            t.elapsed().as_nanos() as f64 / n as f64 / 1000.0
        }).collect();
        runs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        runs[7]
    };
    fn decoys_with(rng: &mut impl RngCore, protected: &[VaultPos]) {
        for (array_idx, row) in VAULTS.iter().enumerate() {
            for _ in 0..SHARE_ENTRIES {
                let mut slot = (rng.next_u64() as usize) & (ARRAY_SIZE - 1);
                while protected.iter().any(|p| p.array == array_idx && p.slot == slot) {
                    slot = (rng.next_u64() as usize) & (ARRAY_SIZE - 1);
                }
                let mut val = rng.next_u64() as usize;
                if val == 0 { val = 1; }
                row[slot].store(val, Ordering::Release);
            }
        }
    }
    // All decoy randomness as one ChaCha20 keystream block, wiped after.
    fn decoys_keystream(protected: &[VaultPos]) {
        use chacha20::cipher::{KeyIvInit, StreamCipher};
        let mut seed = [0u8; 44];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        let mut cipher = chacha20::ChaCha20::new((&seed[..32]).into(), (&seed[32..]).into());
        seed.zeroize();
        let mut buf = [0u64; ARRAY_COUNT * SHARE_ENTRIES * 2];
        cipher.apply_keystream(bytemuck_u64(&mut buf));
        let mut it = buf.iter().copied();
        for (array_idx, row) in VAULTS.iter().enumerate() {
            for _ in 0..SHARE_ENTRIES {
                let mut slot = (it.next().unwrap() as usize) & (ARRAY_SIZE - 1);
                while protected.iter().any(|p| p.array == array_idx && p.slot == slot) {
                    slot = (rand::rngs::OsRng.next_u64() as usize) & (ARRAY_SIZE - 1);
                }
                let mut val = it.next().unwrap() as usize;
                if val == 0 { val = 1; }
                row[slot].store(val, Ordering::Release);
            }
        }
        buf.zeroize();
    }
    fn bytemuck_u64(b: &mut [u64]) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(b.as_mut_ptr() as *mut u8, b.len() * 8) }
    }

    let (prot, pn) = KEY.collect_other_protected(&[&OTHER]);
    let protected = &prot[..pn];
    let mut k = 0u8;
    let set_today = time(30, &mut || { k = k.wrapping_add(1); KEY.set(key(k), &[&OTHER]); });
    eprintln!("BENCH {:<46} {:>9.2} µs", "set() today", set_today);
    let os = time(30, &mut || decoys_with(&mut rand::rngs::OsRng, protected));
    eprintln!("BENCH {:<46} {:>9.2} µs  ({:.0}% of set())", "  decoy pass, OsRng per draw (today)", os, os / set_today * 100.0);
    let std_rng = time(300, &mut || {
        let mut r = rand::rngs::StdRng::from_rng(rand::rngs::OsRng).unwrap();
        decoys_with(&mut r, protected);
    });
    eprintln!("BENCH {:<46} {:>9.2} µs  ({:+.1}% vs OsRng)", "  decoy pass, StdRng seeded once", std_rng, (std_rng / os - 1.0) * 100.0);
    let ks = time(300, &mut || decoys_keystream(protected));
    eprintln!("BENCH {:<46} {:>9.2} µs  ({:+.1}% vs OsRng)", "  decoy pass, ChaCha20 keystream, wiped", ks, (ks / os - 1.0) * 100.0);
    let one_os = time(20000, &mut || { black_box(rand::rngs::OsRng.next_u64()); });
    eprintln!("BENCH {:<46} {:>9.3} µs", "  one OsRng draw (one getrandom syscall)", one_os);

    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let lock = time(200000, &mut || { drop(black_box(LOCK.lock().unwrap_or_else(|e| e.into_inner()))); });
    eprintln!("BENCH {:<46} {:>9.3} µs  ({:.4}% of set())", "uncontended writer lock + unlock", lock, lock / set_today * 100.0);
    KEY.clear(&[&OTHER]);
}

/// Prices the startup fill of the whole vault: one OS draw per entry against the keystream.
///
pub fn fill() {
    use rand::RngCore;
    let fresh = || -> Vec<[AtomicUsize; ARRAY_SIZE]> {
        (0..ARRAY_COUNT).map(|_| std::array::from_fn(|_| AtomicUsize::new(0))).collect()
    };
    let time = |f: &dyn Fn(&[[AtomicUsize; ARRAY_SIZE]])| -> f64 {
        let mut runs: Vec<f64> = (0..7).map(|_| {
            let rows = fresh();
            let t = Instant::now();
            f(&rows);
            t.elapsed().as_secs_f64() * 1000.0
        }).collect();
        runs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        runs[3]
    };
    let os = time(&|rows| {
        let mut rng = rand::rngs::OsRng;
        for row in rows {
            for slot in row.iter() {
                let mut val = rng.next_u64() as usize;
                if val == 0 { val = 1; }
                let _ = slot.compare_exchange(0, val, Ordering::SeqCst, Ordering::Relaxed);
            }
        }
    });
    let ks = time(&|rows| fill_random(rows));
    eprintln!("BENCH vault fill ({} KiB): OsRng per entry {os:.2} ms, keystream {ks:.2} ms ({:+.1}%)",
        ARRAY_COUNT * ARRAY_SIZE * 8 / 1024, (ks / os - 1.0) * 100.0);
}

/// The same hot paths timed, median of 15 batches.
pub fn hot_paths() {
    let hot = HotPaths::new();
    let mut paths = hot.paths();
    paths.insert(0, ("scrub_below() alone", &PARTNER, Box::new(scrub_below)));
    for (name, _, op) in paths {
        let mut runs: Vec<f64> = (0..15)
            .map(|_| {
                let t = Instant::now();
                for _ in 0..40 {
                    op();
                }
                t.elapsed().as_secs_f64() * 1e6 / 40.0
            })
            .collect();
        runs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("BENCH {name:<40} {:>8.2} µs", runs[7]);
    }
}

/// set() and clear() with one live partner, median of 15 batches.
pub fn set_with_a_live_partner() {
    setup([0x12; 32]);
    let mut runs: Vec<f64> = (0..15)
        .map(|i| {
            let t = Instant::now();
            for j in 0..40u8 {
                SETTER.set([i as u8 ^ j; 32], &[&PARTNER]);
            }
            t.elapsed().as_secs_f64() * 1e6 / 40.0
        })
        .collect();
    runs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("BENCH set() with a live partner: {:.1} µs", runs[7]);
    SETTER.clear(&[&PARTNER]);
    PARTNER.clear(&[]);
}

/// What the round count buys: a read's layout cost against one slide guess's, per setting.
pub fn rounds_dial() {
    setup([0x13; 32]);
    let inst = PARTNER.instance_addr();
    let lane = PARTNER.lane();
    let lay = layout(inst, lane);
    let shield_cost = {
        let t = Instant::now();
        for _ in 0..2000 { std::hint::black_box(shield(lane, &lay.prekey)); }
        t.elapsed().as_secs_f64() * 1e6 / 2000.0
    };
    for iters in [24usize, 96, 384, 1536, 6144] {
        let n = (200_000 / iters).max(50) as u32;
        let t = Instant::now();
        for k in 0..n as usize {
            let l = layout_with(inst ^ (k << 12), lane, iters);
            std::hint::black_box(share_positions_from(inst, lane, &l));
        }
        let per = t.elapsed().as_secs_f64() * 1e6 / n as f64;
        let guesses = (1u64 << 32) as f64;
        println!(
            "rounds ~{iters:>5}: read ≈ {:>7.1} µs (layout {per:.1} + shield {shield_cost:.1}); one diff guess {per:.1} µs, 2^32 guesses = {:>8.1} core-hours",
            per + shield_cost,
            guesses * per / 1e6 / 3600.0
        );
    }
    PARTNER.clear(&[]);
}
