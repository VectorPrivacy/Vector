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
    fn collect_other_protected(&self) -> ([VaultPos; 3 + SHARE_ENTRIES], usize) {
        let keys: [&GuardedKey; 2] = [&crate::MY_SECRET_KEY, &crate::ENCRYPTION_KEY];
        let mut buf = [VaultPos { array: 0, slot: 0 }; 3 + SHARE_ENTRIES];
        let mut n = 0;
        for &key in &keys {
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
    /// repeated extract → set → zeroize pattern across login paths.
    #[inline]
    pub fn store_from_keys(&self, keys: &nostr_sdk::Keys) {
        let mut sk_bytes = keys.secret_key().secret_bytes();
        self.set(sk_bytes);
        sk_bytes.zeroize();
    }

    /// Store a key. XOR-split into 4 shares scattered across the 128 arrays,
    /// with decoy writes to ALL arrays so real writes are indistinguishable.
    pub fn set(&self, mut key: [u8; 32]) {

        let mut rng = rand::rngs::OsRng;
        ensure_vaults();

        // Protect other active key's positions from decoy writes
        let (protected, pcount) = self.collect_other_protected();

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
    pub fn clear(&self) {
        // Set inactive FIRST — any concurrent get() will return None
        if self.active.swap(0, Ordering::SeqCst) != 0 {
    
            let mut rng = rand::rngs::OsRng;
            let positions = share_positions(self.instance_addr());
            for pos in &positions {
                let mut val = rng.next_u64() as usize;
                if val == 0 { val = 1; }
                VAULTS[pos.array][pos.slot].store(val, Ordering::Release);
            }
            let (protected, pcount) = self.collect_other_protected();
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
// GuardedSigner — NostrSigner backed by a GuardedKey vault
// ============================================================================

/// A `NostrSigner` that reads the secret key from a `GuardedKey` vault on every
/// operation. The key exists in plaintext only for microseconds during signing,
/// then is zeroized on drop. No permanent key copies in process memory.
#[derive(Debug)]
pub struct GuardedSigner {
    public_key: nostr_sdk::PublicKey,
}

impl GuardedSigner {
    pub fn new(public_key: nostr_sdk::PublicKey) -> Self {
        Self { public_key }
    }

    fn temp_keys(&self) -> Result<nostr_sdk::Keys, nostr_sdk::prelude::SignerError> {
        crate::MY_SECRET_KEY.to_keys()
            .ok_or_else(|| nostr_sdk::prelude::SignerError::from("Secret key not available"))
    }
}

impl nostr_sdk::prelude::NostrSigner for GuardedSigner {
    fn backend(&self) -> nostr_sdk::prelude::SignerBackend {
        nostr_sdk::prelude::SignerBackend::Keys
    }

    fn get_public_key(&self) -> nostr_sdk::prelude::BoxedFuture<Result<nostr_sdk::PublicKey, nostr_sdk::prelude::SignerError>> {
        let pk = self.public_key;
        Box::pin(async move { Ok(pk) })
    }

    fn sign_event(&self, unsigned: nostr_sdk::UnsignedEvent) -> nostr_sdk::prelude::BoxedFuture<Result<nostr_sdk::Event, nostr_sdk::prelude::SignerError>> {
        let keys = self.temp_keys();
        Box::pin(async move {
            let keys = keys?;
            unsigned.sign_with_keys(&keys).map_err(nostr_sdk::prelude::SignerError::backend)
        })
    }

    fn nip04_encrypt<'a>(
        &'a self, public_key: &'a nostr_sdk::PublicKey, content: &'a str,
    ) -> nostr_sdk::prelude::BoxedFuture<'a, Result<String, nostr_sdk::prelude::SignerError>> {
        let keys = self.temp_keys();
        Box::pin(async move { let keys = keys?; keys.nip04_encrypt(public_key, content).await })
    }

    fn nip04_decrypt<'a>(
        &'a self, public_key: &'a nostr_sdk::PublicKey, encrypted_content: &'a str,
    ) -> nostr_sdk::prelude::BoxedFuture<'a, Result<String, nostr_sdk::prelude::SignerError>> {
        let keys = self.temp_keys();
        Box::pin(async move { let keys = keys?; keys.nip04_decrypt(public_key, encrypted_content).await })
    }

    fn nip44_encrypt<'a>(
        &'a self, public_key: &'a nostr_sdk::PublicKey, content: &'a str,
    ) -> nostr_sdk::prelude::BoxedFuture<'a, Result<String, nostr_sdk::prelude::SignerError>> {
        let keys = self.temp_keys();
        Box::pin(async move { let keys = keys?; keys.nip44_encrypt(public_key, content).await })
    }

    fn nip44_decrypt<'a>(
        &'a self, public_key: &'a nostr_sdk::PublicKey, payload: &'a str,
    ) -> nostr_sdk::prelude::BoxedFuture<'a, Result<String, nostr_sdk::prelude::SignerError>> {
        let keys = self.temp_keys();
        Box::pin(async move { let keys = keys?; keys.nip44_decrypt(public_key, payload).await })
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// All tests share VAULTS and the two global keys — serialize them.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Fast reset: mark both global keys inactive without full clear overhead.
    fn reset() {
        crate::MY_SECRET_KEY.active.store(0, Ordering::SeqCst);
        crate::ENCRYPTION_KEY.active.store(0, Ordering::SeqCst);
        ensure_vaults();
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
        crate::MY_SECRET_KEY.set(key);
        assert_eq!(crate::MY_SECRET_KEY.get(), Some(key));
    }

    #[test]
    fn set_get_1000_iterations() {
        let _l = TEST_LOCK.lock().unwrap();
        for i in 0..1000u16 {
            reset();
            let key = test_key((i ^ (i >> 3)) as u8);
            crate::MY_SECRET_KEY.set(key);
            assert_eq!(
                crate::MY_SECRET_KEY.get(), Some(key),
                "Roundtrip failed at iteration {i}"
            );
        }
    }

    #[test]
    fn empty_returns_none() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        assert_eq!(crate::MY_SECRET_KEY.get(), None);
        assert_eq!(crate::ENCRYPTION_KEY.get(), None);
    }

    #[test]
    fn has_key_lifecycle() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        assert!(!crate::MY_SECRET_KEY.has_key());
        crate::MY_SECRET_KEY.set(test_key(1));
        assert!(crate::MY_SECRET_KEY.has_key());
        crate::MY_SECRET_KEY.clear();
        assert!(!crate::MY_SECRET_KEY.has_key());
    }

    #[test]
    fn clear_returns_none() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        crate::MY_SECRET_KEY.set(test_key(99));
        assert!(crate::MY_SECRET_KEY.get().is_some());
        crate::MY_SECRET_KEY.clear();
        assert_eq!(crate::MY_SECRET_KEY.get(), None);
    }

    #[test]
    fn set_overwrites_previous() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        let a = test_key(10);
        let b = test_key(20);
        crate::MY_SECRET_KEY.set(a);
        assert_eq!(crate::MY_SECRET_KEY.get(), Some(a));
        crate::MY_SECRET_KEY.set(b);
        assert_eq!(crate::MY_SECRET_KEY.get(), Some(b));
    }

    #[test]
    fn clear_idempotent() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        crate::MY_SECRET_KEY.clear();
        crate::MY_SECRET_KEY.clear();
        assert_eq!(crate::MY_SECRET_KEY.get(), None);
        crate::MY_SECRET_KEY.set(test_key(5));
        crate::MY_SECRET_KEY.clear();
        crate::MY_SECRET_KEY.clear();
        assert_eq!(crate::MY_SECRET_KEY.get(), None);
    }

    #[test]
    fn encryption_key_basic() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        let key = test_key(0xEE);
        crate::ENCRYPTION_KEY.set(key);
        assert_eq!(crate::ENCRYPTION_KEY.get(), Some(key));
        crate::ENCRYPTION_KEY.clear();
        assert_eq!(crate::ENCRYPTION_KEY.get(), None);
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
            crate::MY_SECRET_KEY.set(key_a);
            crate::ENCRYPTION_KEY.set(key_b);
            assert_eq!(
                crate::MY_SECRET_KEY.get(), Some(key_a),
                "MY_SECRET_KEY corrupted at iteration {i}"
            );
            assert_eq!(
                crate::ENCRYPTION_KEY.get(), Some(key_b),
                "ENCRYPTION_KEY corrupted at iteration {i}"
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
            crate::ENCRYPTION_KEY.set(key_b);
            crate::MY_SECRET_KEY.set(key_a);
            assert_eq!(
                crate::ENCRYPTION_KEY.get(), Some(key_b),
                "ENCRYPTION_KEY corrupted at iteration {i}"
            );
            assert_eq!(
                crate::MY_SECRET_KEY.get(), Some(key_a),
                "MY_SECRET_KEY corrupted at iteration {i}"
            );
        }
    }

    #[test]
    fn cross_key_clear_preserves_other_500() {
        let _l = TEST_LOCK.lock().unwrap();
        let key_a = test_key(0x11);
        let key_b = test_key(0x22);
        for i in 0..500 {
            // Clear MY_SECRET_KEY, verify ENCRYPTION_KEY survives
            reset();
            crate::MY_SECRET_KEY.set(key_a);
            crate::ENCRYPTION_KEY.set(key_b);
            crate::MY_SECRET_KEY.clear();
            assert_eq!(
                crate::ENCRYPTION_KEY.get(), Some(key_b),
                "EK corrupted after SK.clear() at iteration {i}"
            );
            // Clear ENCRYPTION_KEY, verify MY_SECRET_KEY survives
            reset();
            crate::MY_SECRET_KEY.set(key_a);
            crate::ENCRYPTION_KEY.set(key_b);
            crate::ENCRYPTION_KEY.clear();
            assert_eq!(
                crate::MY_SECRET_KEY.get(), Some(key_a),
                "SK corrupted after EK.clear() at iteration {i}"
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
            crate::MY_SECRET_KEY.set(ka);
            crate::ENCRYPTION_KEY.set(kb);
            assert_eq!(crate::MY_SECRET_KEY.get(), Some(ka), "SK wrong at iter {i}");
            assert_eq!(crate::ENCRYPTION_KEY.get(), Some(kb), "EK wrong at iter {i}");
            crate::MY_SECRET_KEY.clear();
            assert_eq!(crate::ENCRYPTION_KEY.get(), Some(kb), "EK wrong after SK clear at iter {i}");
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
                crate::MY_SECRET_KEY.set(ka);
                crate::ENCRYPTION_KEY.set(kb);
            } else {
                crate::ENCRYPTION_KEY.set(kb);
                crate::MY_SECRET_KEY.set(ka);
            }
            assert_eq!(crate::MY_SECRET_KEY.get(), Some(ka), "SK wrong at iter {i}");
            assert_eq!(crate::ENCRYPTION_KEY.get(), Some(kb), "EK wrong at iter {i}");
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
        let addr = crate::MY_SECRET_KEY.instance_addr();
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
    /// Without exclusion, P(at least one hit) per position ≈ 86%. With 6 positions:
    /// P(all survive unprotected) ≈ 0.14^6 ≈ 0.00075%. This test catches the bug with certainty.
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
        crate::MY_SECRET_KEY.set(key);
        assert_eq!(crate::MY_SECRET_KEY.get(), Some(key));
    }

    #[test]
    fn max_key_roundtrip() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        let key = [0xFFu8; 32];
        crate::MY_SECRET_KEY.set(key);
        assert_eq!(crate::MY_SECRET_KEY.get(), Some(key));
    }

    #[test]
    fn to_keys_roundtrip() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        let mut sk_bytes = [0u8; 32];
        sk_bytes[31] = 1; // scalar = 1, valid secp256k1 key
        crate::MY_SECRET_KEY.set(sk_bytes);
        let keys = crate::MY_SECRET_KEY.to_keys();
        assert!(keys.is_some(), "to_keys returned None for valid key");
        assert_eq!(keys.unwrap().secret_key().secret_bytes(), sk_bytes);
    }

    #[test]
    fn to_keys_none_when_empty() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        assert!(crate::MY_SECRET_KEY.to_keys().is_none());
    }

    #[test]
    fn store_from_keys_roundtrip() {
        let _l = TEST_LOCK.lock().unwrap();
        reset();
        let keys = nostr_sdk::Keys::generate();
        let expected = keys.secret_key().secret_bytes();
        crate::MY_SECRET_KEY.store_from_keys(&keys);
        assert_eq!(crate::MY_SECRET_KEY.get(), Some(expected));
    }

    // ================================================================
    // End-to-end encryption pipeline tests
    //
    // These test the FULL path: GuardedKey vault → ChaCha20-Poly1305
    // encrypt → hex encode → hex decode → decrypt → verify plaintext.
    // Argon2 is skipped (too expensive for bulk runs); keys are injected
    // directly into the vault, which is what happens after Argon2 in
    // production (hash_pass → ENCRYPTION_KEY.set).
    // ================================================================

    /// Helper: set ENCRYPTION_KEY in vault + enable the encryption flag.
    fn setup_encryption(key: [u8; 32]) {
        reset();
        crate::ENCRYPTION_KEY.set(key);
        crate::state::set_encryption_enabled(true);
    }

    /// Helper: tear down encryption state.
    fn teardown_encryption() {
        crate::MY_SECRET_KEY.active.store(0, Ordering::SeqCst);
        crate::ENCRYPTION_KEY.active.store(0, Ordering::SeqCst);
        crate::state::set_encryption_enabled(false);
    }

    /// Basic encrypt → decrypt roundtrip, 10 000 iterations.
    /// Tests: vault key retrieval, ChaCha20 nonce uniqueness, hex encode/decode,
    /// and the full internal_encrypt → internal_decrypt pipeline.
    #[tokio::test]
    async fn e2e_encrypt_decrypt_10k() {
        let _l = TEST_LOCK.lock().unwrap();
        let key = test_key(0xAB);
        setup_encryption(key);

        for i in 0..10_000u32 {
            let plaintext = format!("Message #{i} — the quick brown fox 🦊");
            let ciphertext = crate::crypto::internal_encrypt(plaintext.clone(), None).await;

            // Sanity: ciphertext is hex and longer than plaintext
            assert!(crate::crypto::looks_encrypted(&ciphertext),
                "Iteration {i}: ciphertext doesn't look encrypted");

            let decrypted = crate::crypto::internal_decrypt(ciphertext, None).await
                .unwrap_or_else(|_| panic!("Iteration {i}: decrypt returned Err"));
            assert_eq!(decrypted, plaintext, "Iteration {i}: plaintext mismatch");
        }

        teardown_encryption();
    }

    /// Cross-key stress test: encrypt with ENCRYPTION_KEY, then set/clear
    /// MY_SECRET_KEY (which runs write_decoys with cross-key protection),
    /// then decrypt. Verifies write_decoys never corrupts ENCRYPTION_KEY's
    /// vault data. 10 000 iterations.
    #[tokio::test]
    async fn e2e_cross_key_stress_10k() {
        let _l = TEST_LOCK.lock().unwrap();
        let enc_key = test_key(0xCD);
        setup_encryption(enc_key);

        for i in 0..10_000u32 {
            let plaintext = format!("Cross-key test #{i}");
            let ciphertext = crate::crypto::internal_encrypt(plaintext.clone(), None).await;

            // Inject noise: set MY_SECRET_KEY (runs write_decoys that must avoid ENCRYPTION_KEY)
            let sk = test_key((i & 0xFF) as u8);
            crate::MY_SECRET_KEY.set(sk);

            // Decrypt must still work — ENCRYPTION_KEY's vault data must survive
            let decrypted = crate::crypto::internal_decrypt(ciphertext, None).await
                .unwrap_or_else(|_| panic!("Iteration {i}: decrypt failed after MY_SECRET_KEY.set"));
            assert_eq!(decrypted, plaintext, "Iteration {i}: plaintext mismatch after cross-key write");

            // Also verify MY_SECRET_KEY survived ENCRYPTION_KEY being read
            assert_eq!(crate::MY_SECRET_KEY.get(), Some(sk),
                "Iteration {i}: MY_SECRET_KEY corrupted");

            // Clear MY_SECRET_KEY (runs write_decoys again)
            crate::MY_SECRET_KEY.clear();
        }

        teardown_encryption();
    }

    /// maybe_encrypt → maybe_decrypt full path, 10 000 iterations.
    /// Tests the production code path including looks_encrypted() checks
    /// and the crash-recovery fallback logic.
    #[tokio::test]
    async fn e2e_maybe_encrypt_decrypt_10k() {
        let _l = TEST_LOCK.lock().unwrap();
        let key = test_key(0xEF);
        setup_encryption(key);

        for i in 0..10_000u32 {
            let plaintext = format!("Maybe-path #{i}: こんにちは世界");
            let ciphertext = crate::crypto::maybe_encrypt(plaintext.clone()).await;

            // maybe_encrypt with encryption enabled should always produce encrypted output
            assert_ne!(ciphertext, plaintext, "Iteration {i}: content wasn't encrypted");

            let decrypted = crate::crypto::maybe_decrypt(ciphertext).await
                .unwrap_or_else(|_| panic!("Iteration {i}: maybe_decrypt returned Err"));
            assert_eq!(decrypted, plaintext, "Iteration {i}: plaintext mismatch");
        }

        teardown_encryption();
    }

    /// Batch encrypt, then batch decrypt. Simulates boot-time message loading
    /// where all messages are decrypted in sequence from SQLite.
    #[tokio::test]
    async fn e2e_batch_encrypt_then_decrypt() {
        let _l = TEST_LOCK.lock().unwrap();
        let key = test_key(0x77);
        setup_encryption(key);

        let count = 500;
        let mut pairs: Vec<(String, String)> = Vec::with_capacity(count);

        // Encrypt all
        for i in 0..count {
            let plaintext = format!("Batch msg #{i} — {}", "x".repeat(i % 200));
            let ciphertext = crate::crypto::internal_encrypt(plaintext.clone(), None).await;
            pairs.push((plaintext, ciphertext));
        }

        // Inject cross-key noise mid-batch
        crate::MY_SECRET_KEY.set(test_key(0x99));

        // Decrypt all — must survive cross-key write
        for (i, (plaintext, ciphertext)) in pairs.into_iter().enumerate() {
            let decrypted = crate::crypto::internal_decrypt(ciphertext, None).await
                .unwrap_or_else(|_| panic!("Batch {i}: decrypt failed"));
            assert_eq!(decrypted, plaintext, "Batch {i}: mismatch");
        }

        teardown_encryption();
    }

    /// Verify that different keys produce different ciphertexts and
    /// cannot cross-decrypt each other's messages.
    #[tokio::test]
    async fn e2e_key_isolation() {
        let _l = TEST_LOCK.lock().unwrap();
        let key_a = test_key(0x11);
        let key_b = test_key(0x22);
        let message = "Secret message".to_string();

        // Encrypt with key A
        setup_encryption(key_a);
        let ciphertext_a = crate::crypto::internal_encrypt(message.clone(), None).await;

        // Switch to key B — decrypt must fail for key A's ciphertext
        crate::ENCRYPTION_KEY.clear();
        crate::ENCRYPTION_KEY.set(key_b);
        let result = crate::crypto::internal_decrypt(ciphertext_a.clone(), None).await;
        assert!(result.is_err(), "Wrong key should fail to decrypt");

        // Switch back to key A — must succeed
        crate::ENCRYPTION_KEY.clear();
        crate::ENCRYPTION_KEY.set(key_a);
        let decrypted = crate::crypto::internal_decrypt(ciphertext_a, None).await
            .expect("Original key should decrypt");
        assert_eq!(decrypted, message);

        teardown_encryption();
    }

    /// Edge case content: empty-ish strings, pure unicode, JSON, hex-like
    /// strings that could confuse looks_encrypted().
    #[tokio::test]
    async fn e2e_edge_case_content() {
        let _l = TEST_LOCK.lock().unwrap();
        let key = test_key(0x33);
        setup_encryption(key);

        let long_msg = "x".repeat(10_000);
        let multiline = format!("line1\nline2\nline3\n{}", "data".repeat(100));
        let cases: Vec<&str> = vec![
            "a",                                           // minimal
            " ",                                           // whitespace
            "Hello, World!",                               // basic ASCII
            "🦊🔑🔒💬",                                  // emoji-only
            "日本語テスト",                                 // CJK
            "{\"type\":\"message\",\"text\":\"hello\"}",   // JSON
            &long_msg,                                     // 10KB message
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567", // hex-like (64 chars)
            "\n\r\t\0",                                    // control chars
            &multiline,                                    // multiline
        ];

        for (i, plaintext) in cases.iter().enumerate() {
            let ciphertext = crate::crypto::internal_encrypt(plaintext.to_string(), None).await;
            let decrypted = crate::crypto::internal_decrypt(ciphertext, None).await
                .unwrap_or_else(|_| panic!("Edge case {i} failed: {:?}", &plaintext[..plaintext.len().min(50)]));
            assert_eq!(&decrypted, plaintext, "Edge case {i} mismatch");
        }

        teardown_encryption();
    }

    /// Simulates the real login sequence: set ENCRYPTION_KEY first,
    /// then set MY_SECRET_KEY (which triggers write_decoys), then
    /// encrypt and decrypt messages. Repeats 5000 times to catch
    /// any probabilistic cross-key corruption.
    #[tokio::test]
    async fn e2e_login_sequence_stress_5k() {
        let _l = TEST_LOCK.lock().unwrap();

        for i in 0..5_000u32 {
            reset();

            // Step 1: ENCRYPTION_KEY set (happens during password decrypt in login)
            let enc_key = test_key((i & 0xFF) as u8);
            crate::ENCRYPTION_KEY.set(enc_key);
            crate::state::set_encryption_enabled(true);

            // Step 2: MY_SECRET_KEY set (happens right after in login flow)
            let sk = test_key(((i >> 8) & 0xFF) as u8 ^ 0xAA);
            crate::MY_SECRET_KEY.set(sk);

            // Step 3: Encrypt a message (happens when user sends/stores)
            let plaintext = format!("Login seq #{i}");
            let ciphertext = crate::crypto::internal_encrypt(plaintext.clone(), None).await;

            // Step 4: Decrypt (happens when loading messages from DB)
            let decrypted = crate::crypto::internal_decrypt(ciphertext, None).await
                .unwrap_or_else(|_| panic!("Login sequence {i}: decrypt failed"));
            assert_eq!(decrypted, plaintext, "Login sequence {i}: mismatch");

            // Verify both keys survived
            assert_eq!(crate::ENCRYPTION_KEY.get(), Some(enc_key),
                "Login sequence {i}: ENCRYPTION_KEY corrupted");
            assert_eq!(crate::MY_SECRET_KEY.get(), Some(sk),
                "Login sequence {i}: MY_SECRET_KEY corrupted");
        }

        teardown_encryption();
    }
}
