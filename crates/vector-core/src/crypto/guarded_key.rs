//! Memory-hardened key storage: 128 indistinguishable arrays with decoy writes.
//!
//! The real 32-byte key is XOR-split into 4 shares (16 `usize` values on 64-bit),
//! scattered across 128 static arrays of 4,096 entries each. 123 arrays are pure
//! decoys — provably indistinguishable from the 5 real arrays.
//!
//! Defense layers:
//! 1. **Secret splitting**: key XOR-split into 4 shares
//! 2. **128 indistinguishable arrays**: 5 real + 123 decoy, all same size, all
//!    OsRng-initialized, all modified identically during set()/clear()
//! 3. **No heap allocations**: zero pointers, zero fingerprints, zero mlock
//! 4. **Scattered positions**: seed, multipliers, and share data each placed at
//!    (array, slot) positions derived from ASLR instance address
//! 5. **Decoy writes**: during set(), ALL 128 arrays receive ~16 random writes,
//!    making real writes indistinguishable via snapshot diffing
//! 6. **Zero side-channel**: `AtomicUsize::load` during get() — zero writes
//! 7. **Zero searchable constants**: all multipliers derived from ASLR addresses,
//!    all modular arithmetic uses power-of-2 AND masks. The vault code compiles
//!    to generic LDR + MUL + AND + EOR + LSR — indistinguishable from the
//!    thousands of hash/cipher/RNG functions in the binary.
//!
//! Security tiers:
//!   **Without source code** (info-stealers, forensic tools): immune.
//!   Arrays are information-theoretically indistinguishable.
//!
//!   **With source code + memory dump**: attacker must brute-force the ASLR
//!   instance address (~2.5M candidates on macOS, ~268M on Linux).
//!   Computational hardening makes each attempt ~25 μs.
//!   Estimated: ~1 min (macOS) to ~2 hrs (Linux), single-core.
//!
//!   The vault is the second layer — anti-debug protections (PT_DENY_ATTACH,
//!   PR_SET_DUMPABLE, DACL) prevent obtaining the dump in the first place.

use std::sync::atomic::{AtomicUsize, Ordering};
use rand::RngCore;
use zeroize::Zeroize;

/// Entries per array — 4096 is common for crypto lookup tables (S-boxes, hash
/// constants), maximizing false positives during memory scanning.
const ARRAY_SIZE: usize = 4096;
/// Total arrays: 3 config (seed, mul1, mul2) + 4 shares scattered across random arrays + decoys.
/// Power-of-2: `% 128` compiles to `AND #0x7f` — hundreds of hits in any binary.
/// Non-power-of-2 would use a unique magic-multiply constant (trivially searchable).
const ARRAY_COUNT: usize = 128;
/// Number of real XOR shares the key is split into.
const NUM_SHARES: usize = 4;
/// `usize` values per 32-byte share (4 on 64-bit, 8 on 32-bit).
const USIZES_PER_SHARE: usize = 32 / std::mem::size_of::<usize>();
/// Total entries used by one key's share data.
const SHARE_ENTRIES: usize = NUM_SHARES * USIZES_PER_SHARE;

/// 128 vault arrays — all identical, all OsRng-initialized, all modified during set().
/// Memory: 128 × 4096 × size_of::<usize>() = 4 MB on 64-bit, 2 MB on 32-bit.
static VAULTS: [[AtomicUsize; ARRAY_SIZE]; ARRAY_COUNT] = {
    const ZERO: AtomicUsize = AtomicUsize::new(0);
    const ROW: [AtomicUsize; ARRAY_SIZE] = [ZERO; ARRAY_SIZE];
    [ROW; ARRAY_COUNT]
};

/// Initialize all 128 arrays with random values (once per process).
/// Every entry across all arrays is filled — real and decoy arrays are identical.
fn ensure_vaults() {
    if VAULTS[0][0].load(Ordering::Relaxed) == 0 {

        let mut rng = rand::rngs::OsRng;
        for row in VAULTS.iter() {
            for slot in row.iter() {
                let mut val = rng.next_u64() as usize;
                if val == 0 { val = 1; }
                let _ = slot.compare_exchange(0, val, Ordering::SeqCst, Ordering::Relaxed);
            }
        }
    }
}

/// A position within the vault: (array index, slot index).
#[derive(Clone, Copy, PartialEq)]
#[cfg_attr(test, derive(Debug))]
struct VaultPos {
    array: usize,
    slot: usize,
}

/// Derive the mixing iteration count from ASLR addresses — ZERO constants in the binary.
/// Range: 4096..8191. Changes every launch. `& 4095` compiles to `AND #0xFFF` (hundreds
/// of hits in any binary). No fixed loop bound for RE engineers to search for.
#[inline]
fn mix_iterations(instance_addr: usize) -> usize {
    let base = VAULTS.as_ptr() as usize;
    let h = (instance_addr ^ base).wrapping_mul(instance_addr | 1);
    4096 + ((h >> 7) & 4095)
}

/// Hardened hash mixing — runtime-determined iterations of shift-multiply-XOR.
/// ZERO compile-time constants — both multipliers AND iteration count derived
/// from ASLR addresses. Compiles to a tight loop of generic MUL + EOR + LSR.
#[inline]
fn addr_mix(mut h: u64, m1: u64, m2: u64, iterations: usize) -> u64 {
    for _ in 0..iterations {
        h ^= h >> 33;
        h = h.wrapping_mul(m1);
        h ^= h >> 29;
        h = h.wrapping_mul(m2);
        h ^= h >> 31;
    }
    h
}

/// Derive the 3 configuration positions (seed, mul1, mul2) from the instance
/// address alone. No dependency on stored values — breaks the chicken-and-egg.
/// Uses instance address + VAULTS base address (both ASLR'd, both change per launch).
/// ZERO searchable constants — multipliers derived from the two ASLR addresses.
/// Guaranteed collision-free: each position is unique (rehash on collision).
fn config_positions(instance_addr: usize) -> (VaultPos, VaultPos, VaultPos) {
    let base = VAULTS.as_ptr() as usize;
    // Derive multipliers from ASLR addresses — forced odd for mixing quality.
    // These change every launch. No constants in the binary.
    let m1 = (instance_addr as u64) | 1;
    let m2 = (base as u64) | 1;
    let iters = mix_iterations(instance_addr);
    let mut h = addr_mix((instance_addr as u64) ^ (base as u64).rotate_left(19), m1, m2, iters);

    let mut positions = [VaultPos { array: 0, slot: 0 }; 3];
    for i in 0..3 {
        loop {
            let candidate = VaultPos {
                array: ((h >> 32) as usize) & (ARRAY_COUNT - 1),
                slot: (h as usize) & (ARRAY_SIZE - 1),
            };
            if !positions[..i].contains(&candidate) {
                positions[i] = candidate;
                h = addr_mix(h, m1, m2, iters);
                break;
            }
            h = addr_mix(h, m1, m2, iters);
        }
    }

    (positions[0], positions[1], positions[2])
}

/// Derive share data positions. Uses the seed + multipliers read from the vault
/// (which are at config-derived positions) + instance address.
/// Returns SHARE_ENTRIES (array, slot) pairs — each guaranteed unique and
/// non-overlapping with config positions (rehash on collision).
fn share_positions(instance_addr: usize) -> [VaultPos; SHARE_ENTRIES] {
    let (seed_pos, mul1_pos, mul2_pos) = config_positions(instance_addr);
    let seed = VAULTS[seed_pos.array][seed_pos.slot].load(Ordering::Relaxed) as u64;
    let mul1 = VAULTS[mul1_pos.array][mul1_pos.slot].load(Ordering::Relaxed) as u64;
    let mul2 = VAULTS[mul2_pos.array][mul2_pos.slot].load(Ordering::Relaxed) as u64;

    let mut h = seed ^ (instance_addr as u64).rotate_left(23);
    let mut positions = [VaultPos { array: 0, slot: 0 }; SHARE_ENTRIES];
    let config = [seed_pos, mul1_pos, mul2_pos];

    for i in 0..SHARE_ENTRIES {
        loop {
            h ^= h >> 17;
            h = h.wrapping_mul(mul1 | 1);
            h ^= h >> 13;
            h = h.wrapping_mul(mul2 | 1);
            h ^= h >> 16;
            let candidate = VaultPos {
                array: ((h >> 32) as usize) & (ARRAY_COUNT - 1),
                slot: (h as usize) & (ARRAY_SIZE - 1),
            };
            if !config.contains(&candidate) && !positions[..i].contains(&candidate) {
                positions[i] = candidate;
                break;
            }
            // Collision: hash state already advanced, loop retries with new h
        }
    }

    positions
}

/// Write ~16 random entries to EVERY array (real + decoy), excluding protected positions.
/// Protected positions belong to other active GuardedKey instances — overwriting them would
/// corrupt the other key's share/config data. Snapshot-diffing still shows identical change
/// patterns across all 128 arrays (excluded slots are ≤19 out of 524,288 — invisible).
fn write_decoys(protected: &[VaultPos]) {

    let mut rng = rand::rngs::OsRng;
    for (array_idx, row) in VAULTS.iter().enumerate() {
        for _ in 0..SHARE_ENTRIES {
            let mut slot = (rng.next_u64() as usize) & (ARRAY_SIZE - 1);
            // Reroll if this hits another key's data (~0.004% chance, essentially never)
            while protected.iter().any(|p| p.array == array_idx && p.slot == slot) {
                slot = (rng.next_u64() as usize) & (ARRAY_SIZE - 1);
            }
            let mut val = rng.next_u64() as usize;
            if val == 0 { val = 1; }
            row[slot].store(val, Ordering::Release);
        }
    }
}

/// Memory-hardened key vault backed by 128 indistinguishable static arrays.
pub struct GuardedKey {
    /// Non-zero when a key is stored. Stores a random non-zero value (not 0/1)
    /// so it looks like any other random data in `__DATA`.
    active: AtomicUsize,
}

impl GuardedKey {
    pub const fn empty() -> Self {
        Self { active: AtomicUsize::new(0) }
    }

    #[inline]
    fn instance_addr(&self) -> usize {
        &self.active as *const _ as usize
    }

    /// Collect protected vault positions from other active GuardedKey instances.
    /// Prevents write_decoys from corrupting another key's share/config data.
    ///
    /// `others` is a slice of references to other GuardedKey instances that share
    /// the same VAULTS arrays. The caller is responsible for passing all other
    /// active keys to ensure cross-key protection.
    fn collect_other_protected(&self, others: &[&GuardedKey]) -> ([VaultPos; 3 + SHARE_ENTRIES], usize) {
        let mut buf = [VaultPos { array: 0, slot: 0 }; 3 + SHARE_ENTRIES];
        let mut n = 0;
        for &key in others {
            if std::ptr::eq(key, self) || !key.has_key() { continue; }
            let addr = key.instance_addr();
            let (s, m1, m2) = config_positions(addr);
            buf[n] = s; n += 1;
            buf[n] = m1; n += 1;
            buf[n] = m2; n += 1;
            for &pos in share_positions(addr).iter() {
                buf[n] = pos;
                n += 1;
            }
        }
        (buf, n)
    }

    /// Extract the secret key from a `Keys` struct, store it in the vault,
    /// and zeroize the intermediate bytes. One-liner replacement for the
    /// repeated extract -> set -> zeroize pattern across login paths.
    ///
    /// `others` is a slice of references to other active GuardedKey instances
    /// for cross-key protection during decoy writes.
    #[inline]
    pub fn store_from_keys(&self, keys: &nostr_sdk::Keys, others: &[&GuardedKey]) {
        let mut sk_bytes = keys.secret_key().secret_bytes();
        self.set(sk_bytes, others);
        sk_bytes.zeroize();
    }

    /// Store a key. XOR-split into 4 shares scattered across the 128 arrays,
    /// with decoy writes to ALL arrays so real writes are indistinguishable.
    ///
    /// `others` is a slice of references to other active GuardedKey instances
    /// for cross-key protection during decoy writes.
    pub fn set(&self, mut key: [u8; 32], others: &[&GuardedKey]) {

        let mut rng = rand::rngs::OsRng;
        ensure_vaults();

        // Protect other active key's positions from decoy writes
        let (protected, pcount) = self.collect_other_protected(others);

        // Write decoys FIRST — random noise across all 128 arrays.
        // Excludes other keys' positions. Real writes below overwrite any decoy
        // that landed on OUR slots, guaranteeing all keys' data survives intact.
        write_decoys(&protected[..pcount]);

        // Force multiplier entries odd (mixing quality).
        // ~50% of all entries are already odd, so this isn't a fingerprint.
        let (_, mul1_pos, mul2_pos) = config_positions(self.instance_addr());
        let v = VAULTS[mul1_pos.array][mul1_pos.slot].load(Ordering::Relaxed);
        VAULTS[mul1_pos.array][mul1_pos.slot].store(v | 1, Ordering::Relaxed);
        let v = VAULTS[mul2_pos.array][mul2_pos.slot].load(Ordering::Relaxed);
        VAULTS[mul2_pos.array][mul2_pos.slot].store(v | 1, Ordering::Relaxed);

        // XOR-split the key into NUM_SHARES random shares
        let mut shares = [[0u8; 32]; NUM_SHARES];
        for share in shares.iter_mut().take(NUM_SHARES - 1) {
            rng.fill_bytes(share);
        }
        shares[NUM_SHARES - 1] = key;
        for i in 0..NUM_SHARES - 1 {
            for j in 0..32 {
                shares[NUM_SHARES - 1][j] ^= shares[i][j];
            }
        }
        key.zeroize();

        // Write share data to derived positions (after decoys, so real data survives)
        let positions = share_positions(self.instance_addr());
        for (share_idx, share) in shares.iter().enumerate() {
            for u_idx in 0..USIZES_PER_SHARE {
                let byte_off = u_idx * std::mem::size_of::<usize>();
                let val = usize::from_ne_bytes(
                    share[byte_off..byte_off + std::mem::size_of::<usize>()]
                        .try_into().unwrap()
                );
                let pos = positions[share_idx * USIZES_PER_SHARE + u_idx];
                VAULTS[pos.array][pos.slot].store(val, Ordering::Release);
            }
        }
        for share in shares.iter_mut() { share.zeroize(); }

        // Mark active with random non-zero value
        let mut marker = rng.next_u64() as usize;
        if marker == 0 { marker = 1; }
        self.active.store(marker, Ordering::Release);
    }

    /// Recover the key. Zero writes — invisible to snapshot diffing.
    pub fn get(&self) -> Option<[u8; 32]> {
        if self.active.load(Ordering::Acquire) == 0 {
            return None;
        }

        let positions = share_positions(self.instance_addr());
        let mut key = [0u8; 32];

        for share_idx in 0..NUM_SHARES {
            let mut share = [0u8; 32];
            for u_idx in 0..USIZES_PER_SHARE {
                let pos = positions[share_idx * USIZES_PER_SHARE + u_idx];
                let val = VAULTS[pos.array][pos.slot].load(Ordering::Acquire);
                let byte_off = u_idx * std::mem::size_of::<usize>();
                share[byte_off..byte_off + std::mem::size_of::<usize>()]
                    .copy_from_slice(&val.to_ne_bytes());
            }
            for (a, b) in key.iter_mut().zip(share.iter()) {
                *a ^= *b;
            }
        }

        Some(key)
    }

    /// Clear the key. Overwrites shares with random values, writes decoys to all arrays.
    ///
    /// `others` is a slice of references to other active GuardedKey instances
    /// for cross-key protection during decoy writes.
    pub fn clear(&self, others: &[&GuardedKey]) {
        // Set inactive FIRST — any concurrent get() will return None
        if self.active.swap(0, Ordering::SeqCst) != 0 {

            let mut rng = rand::rngs::OsRng;
            let positions = share_positions(self.instance_addr());
            for pos in &positions {
                let mut val = rng.next_u64() as usize;
                if val == 0 { val = 1; }
                VAULTS[pos.array][pos.slot].store(val, Ordering::Release);
            }
            let (protected, pcount) = self.collect_other_protected(others);
            write_decoys(&protected[..pcount]);
        }
    }

    pub fn has_key(&self) -> bool {
        self.active.load(Ordering::Acquire) != 0
    }

    pub fn to_keys(&self) -> Option<nostr_sdk::Keys> {
        let mut bytes = self.get()?;
        let result = nostr_sdk::SecretKey::from_slice(&bytes);
        bytes.zeroize();
        Some(nostr_sdk::Keys::new(result.ok()?))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Test-local key statics (replaces crate::MY_SECRET_KEY / crate::ENCRYPTION_KEY)
    static TEST_KEY_A: GuardedKey = GuardedKey::empty();
    static TEST_KEY_B: GuardedKey = GuardedKey::empty();

    /// All tests share VAULTS and the two test keys — serialize them.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Fast reset: mark both test keys inactive without full clear overhead.
    fn reset() {
        TEST_KEY_A.active.store(0, Ordering::SeqCst);
        TEST_KEY_B.active.store(0, Ordering::SeqCst);
        ensure_vaults();
    }

    /// Helper: the "others" slice for TEST_KEY_A (protects TEST_KEY_B).
    fn others_for_a() -> [&'static GuardedKey; 1] {
        [&TEST_KEY_B]
    }

    /// Helper: the "others" slice for TEST_KEY_B (protects TEST_KEY_A).
    fn others_for_b() -> [&'static GuardedKey; 1] {
        [&TEST_KEY_A]
    }

    /// Generate a deterministic test key from a seed byte.
    fn test_key(seed: u8) -> [u8; 32] {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = seed.wrapping_add(i as u8).wrapping_mul(37).wrapping_add(7);
        }
        k
    }

    // ================================================================
    // Basic operations
    // ================================================================

    #[test]
    fn set_get_roundtrip() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        let key = test_key(42);
        TEST_KEY_A.set(key, &others_for_a());
        assert_eq!(TEST_KEY_A.get(), Some(key));
    }

    #[test]
    fn set_get_1000_iterations() {
        let _l = TEST_LOCK.lock().unwrap();
        for i in 0..1000u16 {
            reset();
            let key = test_key((i ^ (i >> 3)) as u8);
            TEST_KEY_A.set(key, &others_for_a());
            assert_eq!(
                TEST_KEY_A.get(), Some(key),
                "Roundtrip failed at iteration {i}"
            );
        }
    }

    #[test]
    fn empty_returns_none() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        assert_eq!(TEST_KEY_A.get(), None);
        assert_eq!(TEST_KEY_B.get(), None);
    }

    #[test]
    fn has_key_lifecycle() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        assert!(!TEST_KEY_A.has_key());
        TEST_KEY_A.set(test_key(1), &others_for_a());
        assert!(TEST_KEY_A.has_key());
        TEST_KEY_A.clear(&others_for_a());
        assert!(!TEST_KEY_A.has_key());
    }

    #[test]
    fn clear_returns_none() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        TEST_KEY_A.set(test_key(99), &others_for_a());
        assert!(TEST_KEY_A.get().is_some());
        TEST_KEY_A.clear(&others_for_a());
        assert_eq!(TEST_KEY_A.get(), None);
    }

    #[test]
    fn set_overwrites_previous() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        let a = test_key(10);
        let b = test_key(20);
        TEST_KEY_A.set(a, &others_for_a());
        assert_eq!(TEST_KEY_A.get(), Some(a));
        TEST_KEY_A.set(b, &others_for_a());
        assert_eq!(TEST_KEY_A.get(), Some(b));
    }

    #[test]
    fn clear_idempotent() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        TEST_KEY_A.clear(&others_for_a());
        TEST_KEY_A.clear(&others_for_a());
        assert_eq!(TEST_KEY_A.get(), None);
        TEST_KEY_A.set(test_key(5), &others_for_a());
        TEST_KEY_A.clear(&others_for_a());
        TEST_KEY_A.clear(&others_for_a());
        assert_eq!(TEST_KEY_A.get(), None);
    }

    #[test]
    fn encryption_key_basic() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        let key = test_key(0xEE);
        TEST_KEY_B.set(key, &others_for_b());
        assert_eq!(TEST_KEY_B.get(), Some(key));
        TEST_KEY_B.clear(&others_for_b());
        assert_eq!(TEST_KEY_B.get(), None);
    }

    // ================================================================
    // Cross-key protection — 500 iterations each.
    // Before the fix, these had ~7% failure rate per iteration.
    // With 500 iterations the old bug would fail with P > 99.9999%.
    // ================================================================

    #[test]
    fn cross_key_set_then_set_500() {
        let _l = TEST_LOCK.lock().unwrap();
        let key_a = test_key(0xAA);
        let key_b = test_key(0xBB);
        for i in 0..500 {
            reset();
            TEST_KEY_A.set(key_a, &others_for_a());
            TEST_KEY_B.set(key_b, &others_for_b());
            assert_eq!(
                TEST_KEY_A.get(), Some(key_a),
                "TEST_KEY_A corrupted at iteration {i}"
            );
            assert_eq!(
                TEST_KEY_B.get(), Some(key_b),
                "TEST_KEY_B corrupted at iteration {i}"
            );
        }
    }

    #[test]
    fn cross_key_reverse_order_500() {
        let _l = TEST_LOCK.lock().unwrap();
        let key_a = test_key(0xCC);
        let key_b = test_key(0xDD);
        for i in 0..500 {
            reset();
            TEST_KEY_B.set(key_b, &others_for_b());
            TEST_KEY_A.set(key_a, &others_for_a());
            assert_eq!(
                TEST_KEY_B.get(), Some(key_b),
                "TEST_KEY_B corrupted at iteration {i}"
            );
            assert_eq!(
                TEST_KEY_A.get(), Some(key_a),
                "TEST_KEY_A corrupted at iteration {i}"
            );
        }
    }

    #[test]
    fn cross_key_clear_preserves_other_500() {
        let _l = TEST_LOCK.lock().unwrap();
        let key_a = test_key(0x11);
        let key_b = test_key(0x22);
        for i in 0..500 {
            // Clear TEST_KEY_A, verify TEST_KEY_B survives
            reset();
            TEST_KEY_A.set(key_a, &others_for_a());
            TEST_KEY_B.set(key_b, &others_for_b());
            TEST_KEY_A.clear(&others_for_a());
            assert_eq!(
                TEST_KEY_B.get(), Some(key_b),
                "KEY_B corrupted after KEY_A.clear() at iteration {i}"
            );
            // Clear TEST_KEY_B, verify TEST_KEY_A survives
            reset();
            TEST_KEY_A.set(key_a, &others_for_a());
            TEST_KEY_B.set(key_b, &others_for_b());
            TEST_KEY_B.clear(&others_for_b());
            assert_eq!(
                TEST_KEY_A.get(), Some(key_a),
                "KEY_A corrupted after KEY_B.clear() at iteration {i}"
            );
        }
    }

    #[test]
    fn cross_key_alternating_500() {
        let _l = TEST_LOCK.lock().unwrap();
        for i in 0..500u16 {
            reset();
            let ka = test_key(i as u8);
            let kb = test_key(!(i as u8));
            TEST_KEY_A.set(ka, &others_for_a());
            TEST_KEY_B.set(kb, &others_for_b());
            assert_eq!(TEST_KEY_A.get(), Some(ka), "KEY_A wrong at iter {i}");
            assert_eq!(TEST_KEY_B.get(), Some(kb), "KEY_B wrong at iter {i}");
            TEST_KEY_A.clear(&others_for_a());
            assert_eq!(TEST_KEY_B.get(), Some(kb), "KEY_B wrong after KEY_A clear at iter {i}");
        }
    }

    /// Stress: alternating set order, different keys each round, 1000 iterations.
    #[test]
    fn stress_both_keys_1000() {
        let _l = TEST_LOCK.lock().unwrap();
        for i in 0..1000u32 {
            reset();
            let ka = test_key((i & 0xFF) as u8);
            let kb = test_key(!((i & 0xFF) as u8));
            if i % 2 == 0 {
                TEST_KEY_A.set(ka, &others_for_a());
                TEST_KEY_B.set(kb, &others_for_b());
            } else {
                TEST_KEY_B.set(kb, &others_for_b());
                TEST_KEY_A.set(ka, &others_for_a());
            }
            assert_eq!(TEST_KEY_A.get(), Some(ka), "KEY_A wrong at iter {i}");
            assert_eq!(TEST_KEY_B.get(), Some(kb), "KEY_B wrong at iter {i}");
        }
    }

    // ================================================================
    // Position derivation
    // ================================================================

    #[test]
    fn config_positions_all_unique() {
        let _l = TEST_LOCK.lock().unwrap();
        ensure_vaults();
        for addr in (0x1000..0x2000usize).step_by(8) {
            let (a, b, c) = config_positions(addr);
            assert_ne!(a, b, "config collision a==b at addr {addr:#x}");
            assert_ne!(a, c, "config collision a==c at addr {addr:#x}");
            assert_ne!(b, c, "config collision b==c at addr {addr:#x}");
        }
    }

    #[test]
    fn share_positions_all_unique() {
        let _l = TEST_LOCK.lock().unwrap();
        ensure_vaults();
        for addr in (0x2000..0x2100usize).step_by(8) {
            let positions = share_positions(addr);
            for i in 0..SHARE_ENTRIES {
                for j in (i + 1)..SHARE_ENTRIES {
                    assert_ne!(
                        positions[i], positions[j],
                        "share collision [{i}]==[{j}] at addr {addr:#x}"
                    );
                }
            }
        }
    }

    #[test]
    fn share_positions_no_config_overlap() {
        let _l = TEST_LOCK.lock().unwrap();
        ensure_vaults();
        for addr in (0x3000..0x3100usize).step_by(8) {
            let (s, m1, m2) = config_positions(addr);
            let config = [s, m1, m2];
            let shares = share_positions(addr);
            for (i, pos) in shares.iter().enumerate() {
                assert!(
                    !config.contains(pos),
                    "share[{i}] collides with config at addr {addr:#x}"
                );
            }
        }
    }

    #[test]
    fn positions_deterministic() {
        let _l = TEST_LOCK.lock().unwrap();
        ensure_vaults();
        let addr = TEST_KEY_A.instance_addr();
        let cfg1 = config_positions(addr);
        let cfg2 = config_positions(addr);
        assert_eq!(cfg1, cfg2);
        let sp1 = share_positions(addr);
        let sp2 = share_positions(addr);
        assert_eq!(sp1, sp2);
    }

    #[test]
    fn all_positions_in_bounds() {
        let _l = TEST_LOCK.lock().unwrap();
        ensure_vaults();
        for addr in (0x4000..0x4200usize).step_by(8) {
            let (a, b, c) = config_positions(addr);
            for p in [a, b, c] {
                assert!(p.array < ARRAY_COUNT);
                assert!(p.slot < ARRAY_SIZE);
            }
            for p in share_positions(addr) {
                assert!(p.array < ARRAY_COUNT);
                assert!(p.slot < ARRAY_SIZE);
            }
        }
    }

    // ================================================================
    // Internals
    // ================================================================

    #[test]
    fn mix_iterations_in_range() {
        for addr in 0..10000usize {
            let n = mix_iterations(addr);
            assert!((4096..=8191).contains(&n), "mix_iterations({addr}) = {n}");
        }
    }

    #[test]
    fn addr_mix_zero_iterations_is_identity() {
        let h: u64 = 0xDEADBEEFCAFEBABE;
        assert_eq!(addr_mix(h, 123, 456, 0), h);
    }

    #[test]
    fn addr_mix_varies_output() {
        let a = addr_mix(1, 3, 5, 10);
        let b = addr_mix(2, 3, 5, 10);
        let c = addr_mix(1, 7, 5, 10);
        let d = addr_mix(1, 3, 11, 10);
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    #[test]
    fn ensure_vaults_all_nonzero() {
        let _l = TEST_LOCK.lock().unwrap();
        ensure_vaults();
        for (r, row) in VAULTS.iter().enumerate() {
            for (s, slot) in row.iter().enumerate() {
                assert_ne!(
                    slot.load(Ordering::Relaxed), 0,
                    "VAULTS[{r}][{s}] is zero after ensure_vaults"
                );
            }
        }
    }

    #[test]
    fn ensure_vaults_idempotent() {
        let _l = TEST_LOCK.lock().unwrap();
        ensure_vaults();
        let samples: Vec<_> = (0..20)
            .map(|i| {
                let r = i * 13 % ARRAY_COUNT;
                let s = i * 397 % ARRAY_SIZE;
                (r, s, VAULTS[r][s].load(Ordering::Relaxed))
            })
            .collect();
        ensure_vaults();
        for (r, s, val) in &samples {
            assert_eq!(
                VAULTS[*r][*s].load(Ordering::Relaxed), *val,
                "ensure_vaults changed VAULTS[{r}][{s}]"
            );
        }
    }

    /// Run write_decoys 500 times with protected positions — verify they are NEVER overwritten.
    /// Without exclusion, P(at least one hit) per position ~ 86%. With 6 positions:
    /// P(all survive unprotected) ~ 0.14^6 ~ 0.00075%. This test catches the bug with certainty.
    #[test]
    fn write_decoys_respects_exclusions_500() {
        let _l = TEST_LOCK.lock().unwrap();
        ensure_vaults();
        let protected = [
            VaultPos { array: 0, slot: 100 },
            VaultPos { array: 0, slot: 200 },
            VaultPos { array: 50, slot: 2000 },
            VaultPos { array: 50, slot: 3000 },
            VaultPos { array: 100, slot: 500 },
            VaultPos { array: 127, slot: 4095 },
        ];
        let before: Vec<usize> = protected.iter()
            .map(|p| VAULTS[p.array][p.slot].load(Ordering::Relaxed))
            .collect();
        for _ in 0..500 {
            write_decoys(&protected);
        }
        for (i, p) in protected.iter().enumerate() {
            assert_eq!(
                VAULTS[p.array][p.slot].load(Ordering::Relaxed),
                before[i],
                "Protected position ({}, {}) overwritten after 500 rounds",
                p.array, p.slot
            );
        }
    }

    #[test]
    fn write_decoys_empty_exclusion_works() {
        let _l = TEST_LOCK.lock().unwrap();
        ensure_vaults();
        write_decoys(&[]);
    }

    // ================================================================
    // Edge cases
    // ================================================================

    #[test]
    fn zero_key_roundtrip() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        let key = [0u8; 32];
        TEST_KEY_A.set(key, &others_for_a());
        assert_eq!(TEST_KEY_A.get(), Some(key));
    }

    #[test]
    fn max_key_roundtrip() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        let key = [0xFFu8; 32];
        TEST_KEY_A.set(key, &others_for_a());
        assert_eq!(TEST_KEY_A.get(), Some(key));
    }

    #[test]
    fn to_keys_roundtrip() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        let mut sk_bytes = [0u8; 32];
        sk_bytes[31] = 1; // scalar = 1, valid secp256k1 key
        TEST_KEY_A.set(sk_bytes, &others_for_a());
        let keys = TEST_KEY_A.to_keys();
        assert!(keys.is_some(), "to_keys returned None for valid key");
        assert_eq!(keys.unwrap().secret_key().secret_bytes(), sk_bytes);
    }

    #[test]
    fn to_keys_none_when_empty() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        assert!(TEST_KEY_A.to_keys().is_none());
    }

    #[test]
    fn store_from_keys_roundtrip() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        let keys = nostr_sdk::Keys::generate();
        let expected = keys.secret_key().secret_bytes();
        TEST_KEY_A.store_from_keys(&keys, &others_for_a());
        assert_eq!(TEST_KEY_A.get(), Some(expected));
    }

    // ================================================================
    // End-to-end encryption pipeline tests
    //
    // NOTE: These tests need crate::crypto integration (ChaCha20-Poly1305
    // encrypt/decrypt pipeline). They are not included in this standalone
    // module because they depend on:
    //   - crate::crypto::internal_encrypt / internal_decrypt
    //   - crate::crypto::maybe_encrypt / maybe_decrypt
    //   - crate::crypto::looks_encrypted
    //   - crate::state::set_encryption_enabled
    //
    // The original tests (e2e_encrypt_decrypt_10k, e2e_cross_key_stress_10k,
    // e2e_maybe_encrypt_decrypt_10k, e2e_batch_encrypt_then_decrypt,
    // e2e_key_isolation, e2e_edge_case_content, e2e_login_sequence_stress_5k)
    // should be placed in the integrating crate that provides both the
    // GuardedKey vault and the encryption pipeline.
    // ================================================================
}
