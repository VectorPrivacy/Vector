//! Memory-hardened key storage: 32 indistinguishable arrays with decoy writes, and a
//! shielded key.
//!
//! The real 32-byte key is never stored. What is stored is the key XORed with
//! BLAKE3(prekey), XOR-split into 4 shares (16 `usize` values on 64-bit) and scattered
//! across 32 static arrays of 4,096 entries each. The prekey is 16 KiB of the arrays'
//! own random fill: the key's lane row in 8 arrays, which `set()` never writes.
//!
//! Defense layers:
//! 1. **Shielding**: recovering the key needs every one of the prekey's 131,072 bits
//!    exactly. Noisy or partial extraction (cold boot, Rowhammer/RAMBleed, speculative
//!    leaks, torn dumps) yields nothing; one wrong bit gives an unrelated key.
//! 2. **Secret splitting**: the shielded key is XOR-split into 4 shares
//! 3. **32 indistinguishable arrays**: all same size, all randomly initialized, all
//!    modified identically during set()/clear()
//! 4. **No heap allocations**: zero pointers, zero fingerprints, zero mlock
//! 5. **Scattered positions**: prekey rows, seed, multipliers and share data each sit at
//!    (array, slot) positions derived from the ASLR instance address. The slot
//!    dimension is partitioned into LANE_COUNT disjoint lanes; each live key owns one
//!    lane (chosen at set() to differ from every live key's, its own included), so two
//!    keys can never share a slot.
//! 6. **Decoy writes**: during set()/clear(), ALL arrays receive random writes, prekey
//!    rows included, so real writes are indistinguishable via snapshot diffing. Every
//!    live key is re-shielded under its reshuffled prekey in the same write.
//! 7. **Zero writes on read**: get() only loads.
//! 8. **Zero searchable constants**: all multipliers and round counts derive from ASLR
//!    addresses, all modular arithmetic uses power-of-2 AND masks.
//!
//! Security tiers:
//!   **Without source code** (info-stealers, forensic tools): immune.
//!   Arrays are information-theoretically indistinguishable.
//!
//!   **Noisy or partial memory extraction**: needs the full 16 KiB prekey bit-exact.
//!
//!   **Source code + a clean, complete dump**: recoverable, as any in-process secret is.
//!   A dump carries the image base, so the layout is one derivation away. The defence
//!   here is the anti-debug layer (PT_DENY_ATTACH, PR_SET_DUMPABLE, DACL), which stops
//!   the dump being taken.

use std::sync::atomic::{fence, AtomicUsize, Ordering};
use rand::RngCore;
use zeroize::Zeroize;

/// Entries per array — 4096 is common for crypto lookup tables (S-boxes, hash
/// constants), maximizing false positives during memory scanning.
const ARRAY_SIZE: usize = 4096;
/// Total arrays: 3 config (seed, mul1, mul2) + 4 shares scattered across random arrays + decoys.
/// Power-of-2: `% 32` compiles to `AND #0x1f` — hundreds of hits in any binary.
/// Non-power-of-2 would use a unique magic-multiply constant (trivially searchable).
const ARRAY_COUNT: usize = 32;
/// Decoy writes per array per set()/clear(). With 32 arrays this keeps the 2,048 changed
/// slots a snapshot diff must search the same as 128 arrays × 16 gave.
const DECOYS_PER_ARRAY: usize = 64;
/// Number of real XOR shares the key is split into.
const NUM_SHARES: usize = 4;
/// `usize` values per 32-byte share (4 on 64-bit, 8 on 32-bit).
const USIZES_PER_SHARE: usize = 32 / std::mem::size_of::<usize>();
/// Total entries used by one key's share data.
const SHARE_ENTRIES: usize = NUM_SHARES * USIZES_PER_SHARE;
/// Lane rows that make up one key's prekey: 16 KiB on 64-bit and on 32-bit alike.
const PREKEY_ROWS: usize = 16 * 1024 / (LANE_WIDTH * std::mem::size_of::<usize>());

/// Number of disjoint slot-lanes the slot dimension is partitioned into.
/// Each live key occupies exactly one lane, so two keys in distinct lanes can
/// never share an (array, slot) — eliminating cross-key write collisions without
/// any coordination at read time. Power-of-2: `& (LANE_COUNT-1)` compiles to a
/// plain AND mask (ubiquitous in any binary). 16 lanes ⇒ up to 16 concurrent keys
/// (production uses 2). `LANE_WIDTH` = 256 slots ≫ the 19 positions a key needs.
const LANE_COUNT: usize = 16;
/// Slots per lane. `ARRAY_SIZE / LANE_COUNT`, power-of-2.
const LANE_WIDTH: usize = ARRAY_SIZE / LANE_COUNT;

/// 32 vault arrays — all identical, all randomly initialized, all modified during set().
/// Memory: 32 × 4096 × size_of::<usize>() = 1 MiB on 64-bit, 512 KiB on 32-bit.
static VAULTS: [[AtomicUsize; ARRAY_SIZE]; ARRAY_COUNT] = {
    // Array-repeat seeds for this static only; each use is a fresh copy.
    #[allow(clippy::declare_interior_mutable_const)]
    const ZERO: AtomicUsize = AtomicUsize::new(0);
    #[allow(clippy::declare_interior_mutable_const, clippy::large_const_arrays)]
    const ROW: [AtomicUsize; ARRAY_SIZE] = [ZERO; ARRAY_SIZE];
    [ROW; ARRAY_COUNT]
};

/// Initialize all arrays with random values (once per process).
/// Every entry across all arrays is filled — real and decoy arrays are identical.
fn ensure_vaults() {
    if VAULTS[0][0].load(Ordering::Relaxed) == 0 {
        fill_random(&VAULTS);
    }
}

/// Fill every still-empty entry. Racing fillers keep whichever value lands first.
fn fill_random(rows: &[[AtomicUsize; ARRAY_SIZE]]) {
    let mut draws = Draws::new();
    for row in rows {
        for slot in row.iter() {
            let _ = slot.compare_exchange(0, draws.next(), Ordering::SeqCst, Ordering::Relaxed);
        }
    }
}

/// Non-zero random words from a ChaCha20 keystream seeded once from the OS. Decoys and fills
/// take thousands of words, and a syscall each dominated `set()`. Wiped on drop: whoever
/// recovers this state could tell decoy writes from real ones.
struct Draws {
    cipher: chacha20::ChaCha20,
    buf: [u64; 64],
    at: usize,
}

impl Draws {
    fn new() -> Self {
        use chacha20::cipher::KeyIvInit;
        let mut seed = [0u8; 44];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        let cipher = chacha20::ChaCha20::new((&seed[..32]).into(), (&seed[32..]).into());
        seed.zeroize();
        Self { cipher, buf: [0; 64], at: 64 }
    }

    #[inline]
    fn next(&mut self) -> usize {
        use chacha20::cipher::StreamCipher;
        if self.at == self.buf.len() {
            self.buf = [0; 64];
            // SAFETY: a [u64; 64] is 512 initialised bytes, viewed as bytes for the keystream.
            let bytes = unsafe { std::slice::from_raw_parts_mut(self.buf.as_mut_ptr() as *mut u8, 512) };
            self.cipher.apply_keystream(bytes);
            self.at = 0;
        }
        let v = self.buf[self.at] as usize;
        self.at += 1;
        if v == 0 { 1 } else { v }
    }
}

impl Drop for Draws {
    fn drop(&mut self) {
        self.buf.zeroize();
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
/// Range: 16..31, changing every launch, so there is no fixed loop bound to search for. Not a
/// work factor: a dump carries the image base, so an attacker runs this derivation once.
#[inline]
fn mix_iterations(instance_addr: usize) -> usize {
    let base = VAULTS.as_ptr() as usize;
    let h = (instance_addr ^ base).wrapping_mul(instance_addr | 1);
    16 + ((h >> 7) & 15)
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

/// Map a raw hash to a slot inside `lane`'s disjoint slot-range.
/// `slot = lane*LANE_WIDTH + (raw % LANE_WIDTH)` — distinct lanes never overlap.
/// Both ops are AND masks (power-of-2), so this stays a generic AND/OR/SHIFT
/// sequence with no searchable constants.
#[inline]
fn lane_slot(raw: usize, lane: usize) -> usize {
    (lane << LANE_WIDTH.trailing_zeros()) | (raw & (LANE_WIDTH - 1))
}

/// Where one key lives in one lane: the arrays whose lane row is its prekey, and its 3
/// configuration positions (seed, mul1, mul2), which never sit in a prekey array.
struct Layout {
    prekey: [usize; PREKEY_ROWS],
    config: (VaultPos, VaultPos, VaultPos),
}

/// Derive a key's layout from the instance address. No dependency on stored values, which
/// breaks the chicken-and-egg. Uses instance address + VAULTS base address (both ASLR'd,
/// both change per launch); multipliers derive from the two, so no searchable constants.
/// `lane` confines every slot to one of LANE_COUNT disjoint ranges.
fn layout(instance_addr: usize, lane: usize) -> Layout {
    layout_with(instance_addr, lane, mix_iterations(instance_addr))
}

fn layout_with(instance_addr: usize, lane: usize, iters: usize) -> Layout {
    let base = VAULTS.as_ptr() as usize;
    // Derive multipliers from ASLR addresses — forced odd for mixing quality.
    let m1 = (instance_addr as u64) | 1;
    let m2 = (base as u64) | 1;
    let mut h = addr_mix((instance_addr as u64) ^ (base as u64).rotate_left(19), m1, m2, iters);

    let mut prekey = [0usize; PREKEY_ROWS];
    for i in 0..PREKEY_ROWS {
        loop {
            let candidate = ((h >> 32) as usize) & (ARRAY_COUNT - 1);
            h = addr_mix(h, m1, m2, iters);
            if !prekey[..i].contains(&candidate) {
                prekey[i] = candidate;
                break;
            }
        }
    }

    let mut positions = [VaultPos { array: 0, slot: 0 }; 3];
    for i in 0..3 {
        loop {
            let candidate = VaultPos {
                array: ((h >> 32) as usize) & (ARRAY_COUNT - 1),
                slot: lane_slot(h as usize, lane),
            };
            h = addr_mix(h, m1, m2, iters);
            if !positions[..i].contains(&candidate) && !prekey.contains(&candidate.array) {
                positions[i] = candidate;
                break;
            }
        }
    }

    Layout { prekey, config: (positions[0], positions[1], positions[2]) }
}

fn config_positions(instance_addr: usize, lane: usize) -> (VaultPos, VaultPos, VaultPos) {
    layout(instance_addr, lane).config
}

/// Derive share data positions. Uses the seed + multipliers read from the vault
/// (which are at config-derived positions) + instance address.
/// `lane` confines the slot dimension to the key's disjoint lane (same lane as its
/// config positions). Returns SHARE_ENTRIES (array, slot) pairs — each guaranteed
/// unique and non-overlapping with config positions (rehash on collision).
fn share_positions(instance_addr: usize, lane: usize) -> [VaultPos; SHARE_ENTRIES] {
    share_positions_from(instance_addr, lane, &layout(instance_addr, lane))
}

fn share_positions_from(instance_addr: usize, lane: usize, layout: &Layout) -> [VaultPos; SHARE_ENTRIES] {
    let (seed_pos, mul1_pos, mul2_pos) = layout.config;
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
                slot: lane_slot(h as usize, lane),
            };
            if !config.contains(&candidate)
                && !positions[..i].contains(&candidate)
                && !layout.prekey.contains(&candidate.array)
            {
                positions[i] = candidate;
                break;
            }
            // Collision: hash state already advanced, loop retries with new h
        }
    }

    positions
}

/// BLAKE3 of a key's prekey: its lane row in each of its prekey arrays, 16 KiB in all.
/// Copied out with atomic loads (a writer may be mid-decoy; the epoch check discards that
/// read) and wiped after.
fn shield(lane: usize, prekey: &[usize; PREKEY_ROWS]) -> [u8; 32] {
    let mut buf = [0usize; PREKEY_ROWS * LANE_WIDTH];
    for (row, &array) in buf.chunks_exact_mut(LANE_WIDTH).zip(prekey) {
        let slots = &VAULTS[array][lane * LANE_WIDTH..(lane + 1) * LANE_WIDTH];
        for (dst, slot) in row.iter_mut().zip(slots) {
            *dst = slot.load(Ordering::Relaxed);
        }
    }
    // SAFETY: an initialised local [usize] viewed as its bytes, for hashing only.
    let bytes = unsafe {
        std::slice::from_raw_parts(buf.as_ptr() as *const u8, std::mem::size_of_val(&buf))
    };
    let pad = *blake3::hash(bytes).as_bytes();
    buf.zeroize();
    pad
}

/// Vault-wide seqlock: odd while a set() or clear() is rewriting, bumped past it after. A
/// read that overlaps one starts again, so it never mixes old and new shares or prekeys.
static EPOCH: AtomicUsize = AtomicUsize::new(0);

/// Holds the epoch odd for one writer; even again on drop, panics included.
struct Rewriting(usize);

impl Rewriting {
    fn begin() -> Self {
        let epoch = EPOCH.load(Ordering::Relaxed);
        EPOCH.store(epoch.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        Self(epoch)
    }
}

impl Drop for Rewriting {
    fn drop(&mut self) {
        EPOCH.store(self.0.wrapping_add(2), Ordering::Release);
    }
}

/// The plaintext of every live partner, read before a rewrite and re-shielded after it.
struct Held<'a> {
    keys: [(Option<&'a GuardedKey>, [u8; 32]); LANE_COUNT],
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        for (_, key) in self.keys.iter_mut() {
            key.zeroize();
        }
    }
}

/// Write DECOYS_PER_ARRAY random entries to EVERY array (real + decoy), excluding protected positions.
/// Protected positions belong to other active GuardedKey instances — overwriting them would
/// corrupt the other key's share/config data. Snapshot-diffing still shows identical change
/// patterns across all arrays (excluded slots are a few dozen out of 131,072 — invisible).
fn write_decoys(protected: &[VaultPos], draws: &mut Draws) {
    for (array_idx, row) in VAULTS.iter().enumerate() {
        for _ in 0..DECOYS_PER_ARRAY {
            let mut slot = draws.next() & (ARRAY_SIZE - 1);
            // Reroll if this hits another key's data (~0.004% chance, essentially never)
            while protected.iter().any(|p| p.array == array_idx && p.slot == slot) {
                slot = draws.next() & (ARRAY_SIZE - 1);
            }
            row[slot].store(draws.next(), Ordering::Release);
        }
    }
}

/// Serialises every set() and clear(). Each one reads its partners' lanes once, so two running
/// together could decoy over each other's fresh slots. Reads never take it.
static WRITER: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn writer() -> std::sync::MutexGuard<'static, ()> {
    WRITER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Memory-hardened key vault backed by 32 indistinguishable static arrays.
pub struct GuardedKey {
    /// Non-zero when a key is stored. Stores a random non-zero value (not 0/1)
    /// so it looks like any other random data in `__DATA`.
    active: AtomicUsize,
}

/// The process's standing vaults, always kept clear of whatever a caller passes, then the
/// caller's own, each once. A standing vault a call site forgets can't then share its lane.
fn partners<'a>(others: &'a [&'a GuardedKey]) -> impl Iterator<Item = &'a GuardedKey> {
    let standing: [&'static GuardedKey; 3] = [
        &crate::state::ENCRYPTION_KEY,
        &crate::state::MY_SECRET_KEY,
        &crate::state::PREVIOUS_AT_REST_KEY,
    ];
    let mut seen: Vec<usize> = Vec::with_capacity(standing.len() + others.len());
    standing.into_iter().chain(others.iter().copied()).filter(move |k| {
        let addr = *k as *const GuardedKey as usize;
        if seen.contains(&addr) {
            false
        } else {
            seen.push(addr);
            true
        }
    })
}

impl GuardedKey {
    pub const fn empty() -> Self {
        Self { active: AtomicUsize::new(0) }
    }

    #[inline]
    fn instance_addr(&self) -> usize {
        &self.active as *const _ as usize
    }

    /// The lane a marker maps to. Lanes partition the slot space; a key lives
    /// entirely within one lane so it can never share a slot with a key in a
    /// different lane. `& (LANE_COUNT-1)` is a plain AND mask.
    #[inline]
    fn lane_of(marker: usize) -> usize {
        marker & (LANE_COUNT - 1)
    }

    /// This instance's current lane, read from its active marker (0 if empty).
    #[inline]
    fn lane(&self) -> usize {
        Self::lane_of(self.active.load(Ordering::Acquire))
    }

    /// Pick a marker whose lane differs from every currently-live key's lane, this one's
    /// included, so this key's positions stay disjoint from theirs and from its own old ones. `rng` supplies the random
    /// marker; we reroll until its lane is free. With LANE_COUNT lanes and far fewer
    /// concurrent keys, this terminates in ~1 try. The marker itself remains a
    /// full-range random non-zero usize — the lane is an emergent low-bit property,
    /// not separately stored, so it adds no searchable fingerprint.
    fn pick_marker(&self, others: &[&GuardedKey], rng: &mut rand::rngs::OsRng) -> usize {
        let mut taken = [false; LANE_COUNT];
        if self.has_key() {
            taken[self.lane()] = true;
        }
        for key in partners(others) {
            if std::ptr::eq(key, self) || !key.has_key() { continue; }
            taken[key.lane()] = true;
        }
        loop {
            let mut marker = rng.next_u64() as usize;
            if marker == 0 { marker = 1; }
            if !taken[Self::lane_of(marker)] {
                return marker;
            }
        }
    }

    /// Collect protected vault positions from other active GuardedKey instances.
    /// Prevents write_decoys from corrupting another key's share/config data.
    ///
    /// `others` is a slice of references to other GuardedKey instances that share
    /// the same VAULTS arrays. The caller is responsible for passing all other
    /// active keys to ensure cross-key protection.
    ///
    /// Sized for up to LANE_COUNT live keys (the max that can coexist in distinct
    /// lanes). Extra entries are simply not collected — lanes already guarantee
    /// disjointness, so the decoy exclusion is a redundant safety net.
    fn collect_other_protected(&self, others: &[&GuardedKey]) -> ([VaultPos; (3 + SHARE_ENTRIES) * LANE_COUNT], usize) {
        let mut buf = [VaultPos { array: 0, slot: 0 }; (3 + SHARE_ENTRIES) * LANE_COUNT];
        let mut n = 0;
        for key in partners(others) {
            if std::ptr::eq(key, self) || !key.has_key() { continue; }
            if n + 3 + SHARE_ENTRIES > buf.len() { break; }
            let addr = key.instance_addr();
            let lane = key.lane();
            let (s, m1, m2) = config_positions(addr, lane);
            buf[n] = s; n += 1;
            buf[n] = m1; n += 1;
            buf[n] = m2; n += 1;
            for &pos in share_positions(addr, lane).iter() {
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
    pub fn store_from_keys(&self, keys: &nostr_sdk::prelude::Keys, others: &[&GuardedKey]) {
        let mut sk_bytes = keys.secret_key().secret_bytes();
        self.set(sk_bytes, others);
        sk_bytes.zeroize();
    }

    /// Store a key: shielded under its new lane's prekey, XOR-split into 4 shares
    /// scattered across the arrays, with decoy writes to ALL arrays so real writes are
    /// indistinguishable.
    ///
    /// INVARIANT: pass EVERY other live key (the standing vaults are always included). Decoys
    /// reshuffle every prekey, and only the keys named here are re-shielded under theirs.
    pub fn set(&self, mut key: [u8; 32], others: &[&GuardedKey]) {
        let _writer = writer();
        let mut rng = rand::rngs::OsRng;
        let mut draws = Draws::new();
        ensure_vaults();

        // A fresh lane, free of every live key's including our own: new positions and a new
        // prekey, while readers of the old key keep a consistent copy until the epoch moves.
        let marker = self.pick_marker(others, &mut rng);
        let lane = Self::lane_of(marker);

        let old_marker = self.active.load(Ordering::Acquire);
        let old_positions = (old_marker != 0)
            .then(|| share_positions(self.instance_addr(), Self::lane_of(old_marker)));

        let (mut protected, mut pcount) = self.collect_other_protected(others);
        if old_marker != 0 {
            let (s, m1, m2) = config_positions(self.instance_addr(), Self::lane_of(old_marker));
            for pos in [s, m1, m2].into_iter().chain(old_positions.into_iter().flatten()) {
                if pcount < protected.len() {
                    protected[pcount] = pos;
                    pcount += 1;
                }
            }
        }

        let held = self.hold_partners(others);
        let rewriting = Rewriting::begin();

        // Decoys first, prekey rows included. Real writes below overwrite any decoy that
        // landed on OUR slots; protected slots keep every other key's positions intact.
        write_decoys(&protected[..pcount], &mut draws);

        // Force multiplier entries odd (mixing quality).
        // ~50% of all entries are already odd, so this isn't a fingerprint.
        let (_, mul1_pos, mul2_pos) = config_positions(self.instance_addr(), lane);
        let v = VAULTS[mul1_pos.array][mul1_pos.slot].load(Ordering::Relaxed);
        VAULTS[mul1_pos.array][mul1_pos.slot].store(v | 1, Ordering::Relaxed);
        let v = VAULTS[mul2_pos.array][mul2_pos.slot].load(Ordering::Relaxed);
        VAULTS[mul2_pos.array][mul2_pos.slot].store(v | 1, Ordering::Relaxed);

        Self::write_shielded(self.instance_addr(), lane, &mut key, &mut rng);

        // Commit the lane-bearing marker. get() re-derives the same lane from it.
        self.active.store(marker, Ordering::Release);

        // Scrub the previous key's shares so it never lingers (matches clear()'s scrub).
        for pos in old_positions.iter().flatten() {
            VAULTS[pos.array][pos.slot].store(draws.next(), Ordering::Release);
        }

        Self::reshield(held, &mut rng);
        drop(rewriting);
    }

    /// Shield `key` under `lane`'s prekey and write it as 4 shares. Wipes `key`.
    fn write_shielded(instance_addr: usize, lane: usize, key: &mut [u8; 32], rng: &mut rand::rngs::OsRng) {
        let layout = layout(instance_addr, lane);
        let mut pad = shield(lane, &layout.prekey);
        let mut shares = [[0u8; 32]; NUM_SHARES];
        for share in shares.iter_mut().take(NUM_SHARES - 1) {
            rng.fill_bytes(share);
        }
        for j in 0..32 {
            let mut last = key[j] ^ pad[j];
            for share in shares.iter().take(NUM_SHARES - 1) {
                last ^= share[j];
            }
            shares[NUM_SHARES - 1][j] = last;
        }
        key.zeroize();
        pad.zeroize();

        let positions = share_positions_from(instance_addr, lane, &layout);
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
    }

    /// Read every other live partner's key before a rewrite reshuffles its prekey.
    fn hold_partners<'a>(&self, others: &'a [&'a GuardedKey]) -> Held<'a> {
        let mut held = Held { keys: [(None, [0u8; 32]); LANE_COUNT] };
        let mut n = 0;
        for key in partners(others) {
            if std::ptr::eq(key, self) || n == LANE_COUNT { continue; }
            if let Some(plain) = key.get() {
                held.keys[n] = (Some(key), plain);
                n += 1;
            }
        }
        held
    }

    /// Re-shield every held partner under its prekey as the decoys left it.
    fn reshield(mut held: Held<'_>, rng: &mut rand::rngs::OsRng) {
        for (key, plain) in held.keys.iter_mut() {
            if let Some(key) = key {
                Self::write_shielded(key.instance_addr(), key.lane(), plain, rng);
            }
        }
    }

    /// Recover the key. Zero writes — invisible to snapshot diffing.
    ///
    /// A read that a set() or clear() overlapped sees the epoch (or this key's marker) move
    /// and starts again, so it never returns a mix of old and new shares or prekeys.
    pub fn get(&self) -> Option<[u8; 32]> {
        loop {
            let epoch = EPOCH.load(Ordering::Acquire);
            if epoch & 1 == 1 {
                std::hint::spin_loop();
                std::thread::yield_now();
                continue;
            }
            let marker = self.active.load(Ordering::Acquire);
            if marker == 0 {
                return None;
            }
            let mut key = self.read_shielded(marker);
            fence(Ordering::Acquire);
            if EPOCH.load(Ordering::Relaxed) == epoch && self.active.load(Ordering::Relaxed) == marker {
                return Some(key);
            }
            key.zeroize();
        }
    }

    fn read_shielded(&self, marker: usize) -> [u8; 32] {
        // Lane comes from the marker, so reads land on exactly the slots set() wrote.
        let lane = Self::lane_of(marker);
        let layout = layout(self.instance_addr(), lane);
        let positions = share_positions_from(self.instance_addr(), lane, &layout);
        let mut key = shield(lane, &layout.prekey);

        for (i, pos) in positions.iter().enumerate() {
            let val = VAULTS[pos.array][pos.slot].load(Ordering::Acquire).to_ne_bytes();
            let byte_off = (i % USIZES_PER_SHARE) * std::mem::size_of::<usize>();
            for (a, b) in key[byte_off..byte_off + val.len()].iter_mut().zip(val) {
                *a ^= b;
            }
        }

        key
    }

    /// Clear the key. Overwrites shares with random values, writes decoys to all arrays,
    /// and re-shields every other live key named in `others`.
    pub fn clear(&self, others: &[&GuardedKey]) {
        let _writer = writer();
        if self.active.load(Ordering::Acquire) == 0 {
            return;
        }
        let mut rng = rand::rngs::OsRng;
        let mut draws = Draws::new();
        let held = self.hold_partners(others);
        let rewriting = Rewriting::begin();

        let old_marker = self.active.swap(0, Ordering::SeqCst);
        // Scrub the slots we actually wrote: lane from the now-cleared marker.
        let positions = share_positions(self.instance_addr(), Self::lane_of(old_marker));
        for pos in &positions {
            VAULTS[pos.array][pos.slot].store(draws.next(), Ordering::Release);
        }
        let (protected, pcount) = self.collect_other_protected(others);
        write_decoys(&protected[..pcount], &mut draws);

        Self::reshield(held, &mut rng);
        drop(rewriting);
    }

    pub fn has_key(&self) -> bool {
        self.active.load(Ordering::Acquire) != 0
    }

    pub fn to_keys(&self) -> Option<nostr_sdk::prelude::Keys> {
        let mut bytes = self.get()?;
        let result = nostr_sdk::prelude::SecretKey::from_slice(&bytes);
        bytes.zeroize();
        Some(nostr_sdk::prelude::Keys::new(result.ok()?))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Test-local key statics (replaces crate::MY_SECRET_KEY / crate::ENCRYPTION_KEY)
    static TEST_KEY_A: GuardedKey = GuardedKey::empty();
    static TEST_KEY_B: GuardedKey = GuardedKey::empty();

    // SERIALIZE on the crate-wide `DB_TEST_GUARD` — NOT a vault-local lock. The `VAULTS` arrays are
    // ONE process-global shared by EVERY GuardedKey, including the production `MY_SECRET_KEY` /
    // `ENCRYPTION_KEY` that other tests write via `init_test_db` (which holds `DB_TEST_GUARD`). Those
    // writers pass only their partner in `others`, so their decoys/lane don't know about TEST_KEY_A/B and
    // would clobber them if run concurrently — the cause of the cross-key flake. Sharing the same guard
    // makes all vault-touching vector-core tests mutually exclusive. (Production is unaffected: its two
    // keys are always mutually `others`-aware, verified across every login/clear path. Poison-tolerant
    // via `into_inner` so one test panic doesn't cascade poison across the suite.)

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
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let key = test_key(42);
        TEST_KEY_A.set(key, &others_for_a());
        assert_eq!(TEST_KEY_A.get(), Some(key));
    }

    #[test]
    #[ignore = "vault stress: hundreds of guarded set/get rounds, serialized on the vault statics (~41s of a 52s suite). Run in CI by NAME (`-- --ignored guarded_key`), never --include-ignored: that also sweeps in the live-relay tests."]
    fn set_get_1000_iterations() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
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
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert_eq!(TEST_KEY_A.get(), None);
        assert_eq!(TEST_KEY_B.get(), None);
    }

    #[test]
    fn has_key_lifecycle() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert!(!TEST_KEY_A.has_key());
        TEST_KEY_A.set(test_key(1), &others_for_a());
        assert!(TEST_KEY_A.has_key());
        TEST_KEY_A.clear(&others_for_a());
        assert!(!TEST_KEY_A.has_key());
    }

    #[test]
    fn clear_returns_none() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        TEST_KEY_A.set(test_key(99), &others_for_a());
        assert!(TEST_KEY_A.get().is_some());
        TEST_KEY_A.clear(&others_for_a());
        assert_eq!(TEST_KEY_A.get(), None);
    }

    #[test]
    fn set_overwrites_previous() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
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
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
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
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let key = test_key(0xEE);
        TEST_KEY_B.set(key, &others_for_b());
        assert_eq!(TEST_KEY_B.get(), Some(key));
        TEST_KEY_B.clear(&others_for_b());
        assert_eq!(TEST_KEY_B.get(), None);
    }

    // ================================================================
    // Cross-key protection — 500 iterations each.
    // Two keys live in disjoint lanes, so neither key's writes (decoys, shares,
    // or multiplier-odd-forcing) can ever land on the other's slots.
    // ================================================================

    #[test]
    #[ignore = "vault stress: hundreds of guarded set/get rounds, serialized on the vault statics (~41s of a 52s suite). Run in CI by NAME (`-- --ignored guarded_key`), never --include-ignored: that also sweeps in the live-relay tests."]
    fn cross_key_set_then_set_500() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
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
    #[ignore = "vault stress: hundreds of guarded set/get rounds, serialized on the vault statics (~41s of a 52s suite). Run in CI by NAME (`-- --ignored guarded_key`), never --include-ignored: that also sweeps in the live-relay tests."]
    fn cross_key_reverse_order_500() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
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
    #[ignore = "vault stress: hundreds of guarded set/get rounds, serialized on the vault statics (~41s of a 52s suite). Run in CI by NAME (`-- --ignored guarded_key`), never --include-ignored: that also sweeps in the live-relay tests."]
    fn cross_key_clear_preserves_other_500() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
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
    #[ignore = "vault stress: hundreds of guarded set/get rounds, serialized on the vault statics (~41s of a 52s suite). Run in CI by NAME (`-- --ignored guarded_key`), never --include-ignored: that also sweeps in the live-relay tests."]
    fn cross_key_alternating_500() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
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
    /// Two keys occupy disjoint lanes, so a real write from one can never clobber the
    /// other's share slots regardless of decoy layout.
    #[test]
    #[ignore = "vault stress: hundreds of guarded set/get rounds, serialized on the vault statics (~41s of a 52s suite). Run in CI by NAME (`-- --ignored guarded_key`), never --include-ignored: that also sweeps in the live-relay tests."]
    fn stress_both_keys_1000() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
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
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        ensure_vaults();
        for lane in 0..LANE_COUNT {
            for addr in (0x1000..0x2000usize).step_by(8) {
                let (a, b, c) = config_positions(addr, lane);
                assert_ne!(a, b, "config collision a==b at addr {addr:#x} lane {lane}");
                assert_ne!(a, c, "config collision a==c at addr {addr:#x} lane {lane}");
                assert_ne!(b, c, "config collision b==c at addr {addr:#x} lane {lane}");
            }
        }
    }

    #[test]
    fn share_positions_all_unique() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        ensure_vaults();
        for lane in 0..LANE_COUNT {
            for addr in (0x2000..0x2100usize).step_by(8) {
                let positions = share_positions(addr, lane);
                for i in 0..SHARE_ENTRIES {
                    for j in (i + 1)..SHARE_ENTRIES {
                        assert_ne!(
                            positions[i], positions[j],
                            "share collision [{i}]==[{j}] at addr {addr:#x} lane {lane}"
                        );
                    }
                }
            }
        }
    }

    /// A standing vault keeps its lane even when a set or clear elsewhere doesn't name it.
    #[test]
    fn a_standing_vault_survives_sets_that_forget_it() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let standing = &crate::state::PREVIOUS_AT_REST_KEY;
        standing.set([0xAB; 32], &[]);
        for i in 0..64u8 {
            let other = GuardedKey::empty();
            other.set([i; 32], &[]);
            assert_ne!(other.lane(), standing.lane());
            other.clear(&[]);
        }
        assert_eq!(standing.get(), Some([0xAB; 32]));
        standing.clear(&[]);
    }

    #[test]
    fn share_positions_no_config_overlap() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        ensure_vaults();
        for lane in 0..LANE_COUNT {
            for addr in (0x3000..0x3100usize).step_by(8) {
                let (s, m1, m2) = config_positions(addr, lane);
                let config = [s, m1, m2];
                let shares = share_positions(addr, lane);
                for (i, pos) in shares.iter().enumerate() {
                    assert!(
                        !config.contains(pos),
                        "share[{i}] collides with config at addr {addr:#x} lane {lane}"
                    );
                }
            }
        }
    }

    /// Distinct lanes are physically disjoint: no (array, slot) is shared between
    /// two different lanes for the same address. This is the property that makes
    /// cross-key writes collision-safe.
    #[test]
    fn distinct_lanes_are_disjoint() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        ensure_vaults();
        for addr in (0x5000..0x5100usize).step_by(8) {
            for la in 0..LANE_COUNT {
                for lb in 0..LANE_COUNT {
                    if la == lb { continue; }
                    let (sa, m1a, m2a) = config_positions(addr, la);
                    let a_all: Vec<VaultPos> = [sa, m1a, m2a]
                        .into_iter()
                        .chain(share_positions(addr, la))
                        .collect();
                    let (sb, m1b, m2b) = config_positions(addr, lb);
                    let b_all: Vec<VaultPos> = [sb, m1b, m2b]
                        .into_iter()
                        .chain(share_positions(addr, lb))
                        .collect();
                    for p in &a_all {
                        assert!(
                            !b_all.contains(p),
                            "lane {la} and lane {lb} share {p:?} at addr {addr:#x}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn positions_deterministic() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        ensure_vaults();
        let addr = TEST_KEY_A.instance_addr();
        let cfg1 = config_positions(addr, 3);
        let cfg2 = config_positions(addr, 3);
        assert_eq!(cfg1, cfg2);
        let sp1 = share_positions(addr, 3);
        let sp2 = share_positions(addr, 3);
        assert_eq!(sp1, sp2);
    }

    #[test]
    fn all_positions_in_bounds() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        ensure_vaults();
        for lane in 0..LANE_COUNT {
            for addr in (0x4000..0x4200usize).step_by(8) {
                let (a, b, c) = config_positions(addr, lane);
                for p in [a, b, c] {
                    assert!(p.array < ARRAY_COUNT);
                    assert!(p.slot < ARRAY_SIZE);
                }
                for p in share_positions(addr, lane) {
                    assert!(p.array < ARRAY_COUNT);
                    assert!(p.slot < ARRAY_SIZE);
                }
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
            assert!((16..=31).contains(&n), "mix_iterations({addr}) = {n}");
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
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
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
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
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
    /// Without exclusion, P(at least one hit) per position ~ 99.96%, so a broken exclusion is
    /// caught with near-certainty.
    #[test]
    #[ignore = "vault stress: hundreds of guarded set/get rounds, serialized on the vault statics (~41s of a 52s suite). Run in CI by NAME (`-- --ignored guarded_key`), never --include-ignored: that also sweeps in the live-relay tests."]
    fn write_decoys_respects_exclusions_500() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        ensure_vaults();
        let protected = [
            VaultPos { array: 0, slot: 100 },
            VaultPos { array: 0, slot: 200 },
            VaultPos { array: 13, slot: 2000 },
            VaultPos { array: 13, slot: 3000 },
            VaultPos { array: 25, slot: 500 },
            VaultPos { array: 31, slot: 4095 },
        ];
        let before: Vec<usize> = protected.iter()
            .map(|p| VAULTS[p.array][p.slot].load(Ordering::Relaxed))
            .collect();
        for _ in 0..500 {
            write_decoys(&protected, &mut Draws::new());
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
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        ensure_vaults();
        write_decoys(&[], &mut Draws::new());
    }

    // ================================================================
    // Edge cases
    // ================================================================

    #[test]
    fn zero_key_roundtrip() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let key = [0u8; 32];
        TEST_KEY_A.set(key, &others_for_a());
        assert_eq!(TEST_KEY_A.get(), Some(key));
    }

    #[test]
    fn max_key_roundtrip() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let key = [0xFFu8; 32];
        TEST_KEY_A.set(key, &others_for_a());
        assert_eq!(TEST_KEY_A.get(), Some(key));
    }

    #[test]
    fn to_keys_roundtrip() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
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
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert!(TEST_KEY_A.to_keys().is_none());
    }

    #[test]
    fn store_from_keys_roundtrip() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let keys = nostr_sdk::prelude::Keys::generate();
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

    /// A read racing a stream of replacements only ever sees one of the keys, never a blend.
    #[test]
    fn reads_racing_a_replacement_see_a_whole_key() {
        use std::sync::atomic::{AtomicBool, AtomicU64};
        use std::sync::Arc;
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let (k1, k2) = (test_key(1), test_key(2));
        TEST_KEY_A.set(k1, &others_for_a());
        let stop = Arc::new(AtomicBool::new(false));
        let (reads, torn) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let reader = {
            let (stop, reads, torn) = (stop.clone(), reads.clone(), torn.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let got = TEST_KEY_A.get();
                    reads.fetch_add(1, Ordering::SeqCst);
                    if got != Some(k1) && got != Some(k2) {
                        torn.fetch_add(1, Ordering::SeqCst);
                    }
                }
            })
        };
        for i in 0..200 {
            TEST_KEY_A.set(if i % 2 == 0 { k2 } else { k1 }, &others_for_a());
        }
        stop.store(true, Ordering::SeqCst);
        reader.join().unwrap();
        assert!(reads.load(Ordering::SeqCst) > 0);
        assert_eq!(torn.load(Ordering::SeqCst), 0);
        TEST_KEY_A.clear(&others_for_a());
    }

    /// Replacing a key always moves it to a fresh lane, so the old shares stay intact for readers.
    #[test]
    fn a_replacement_never_reuses_the_live_lane() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        TEST_KEY_A.set(test_key(7), &others_for_a());
        for i in 0..64u8 {
            let before = TEST_KEY_A.lane();
            TEST_KEY_A.set(test_key(i), &others_for_a());
            assert_ne!(TEST_KEY_A.lane(), before);
            assert_eq!(TEST_KEY_A.get(), Some(test_key(i)));
        }
        TEST_KEY_A.clear(&others_for_a());
    }

    /// Prices each candidate vault design's read, all over the same live vault so cache
    /// behaviour matches production. Run with `--release -- --ignored bench_vault_designs --nocapture`.
    #[test]
    #[ignore = "benchmark"]
    fn bench_vault_designs() {
        use sha2::Digest;
        use std::hint::black_box;
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        TEST_KEY_A.set(test_key(9), &others_for_a());
        let addr = TEST_KEY_A.instance_addr();
        let lane = TEST_KEY_A.lane();
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
                let t = std::time::Instant::now();
                for _ in 0..n { black_box(f()); }
                t.elapsed().as_nanos() as f64 / n as f64 / 1000.0
            }).collect();
            runs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            runs[7]
        };

        eprintln!("BENCH rounds today={today} few={few}");
        let base = time(300, &|| TEST_KEY_A.get().unwrap());
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
        TEST_KEY_A.clear(&others_for_a());
    }

    /// Prices set()'s randomness sources and a vault-wide writer lock.
    /// Run with `--release -- --ignored bench_vault_set --nocapture`.
    #[test]
    #[ignore = "benchmark"]
    fn bench_vault_set() {
        use rand::{RngCore, SeedableRng};
        use std::hint::black_box;
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let time = |n: usize, f: &mut dyn FnMut()| -> f64 {
            let mut runs: Vec<f64> = (0..15).map(|_| {
                let t = std::time::Instant::now();
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

        let (prot, pn) = TEST_KEY_A.collect_other_protected(&others_for_a());
        let protected = &prot[..pn];
        let mut k = 0u8;
        let set_today = time(30, &mut || { k = k.wrapping_add(1); TEST_KEY_A.set(test_key(k), &others_for_a()); });
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
        TEST_KEY_A.clear(&others_for_a());
    }

    /// Draws never yields zero (an empty marker) and doesn't repeat across its buffer refills.
    #[test]
    fn draws_are_nonzero_and_distinct() {
        let mut d = Draws::new();
        let words: Vec<usize> = (0..4096).map(|_| d.next()).collect();
        assert!(words.iter().all(|&w| w != 0));
        let mut sorted = words.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), words.len());
        let mut other = Draws::new();
        assert_ne!(other.next(), words[0], "each Draws is seeded afresh");
    }

    /// Prices the startup fill of the whole vault: one OS draw per entry against the keystream.
    /// Run with `--release -- --ignored bench_vault_fill --nocapture`.
    #[test]
    #[ignore = "benchmark"]
    fn bench_vault_fill() {
        use rand::RngCore;
        let fresh = || -> Vec<[AtomicUsize; ARRAY_SIZE]> {
            (0..ARRAY_COUNT).map(|_| std::array::from_fn(|_| AtomicUsize::new(0))).collect()
        };
        let time = |f: &dyn Fn(&[[AtomicUsize; ARRAY_SIZE]])| -> f64 {
            let mut runs: Vec<f64> = (0..7).map(|_| {
                let rows = fresh();
                let t = std::time::Instant::now();
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

    /// Two keys changed at the same time from two threads both survive intact.
    #[test]
    fn concurrent_sets_of_two_keys_both_survive() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let mut corrupted = 0;
        for round in 0..40u8 {
            let a = std::thread::spawn(move || {
                for i in 0..8u8 { TEST_KEY_A.set(test_key(round ^ i), &others_for_a()); }
            });
            let b = std::thread::spawn(move || {
                for i in 0..8u8 { TEST_KEY_B.set(test_key(!(round ^ i)), &others_for_b()); }
            });
            a.join().unwrap();
            b.join().unwrap();
            corrupted += usize::from(TEST_KEY_A.get() != Some(test_key(round ^ 7)));
            corrupted += usize::from(TEST_KEY_B.get() != Some(test_key(!(round ^ 7))));
        }
        assert_eq!(corrupted, 0);
        TEST_KEY_A.clear(&others_for_a());
        TEST_KEY_B.clear(&others_for_b());
    }

    /// Prekey arrays are distinct, and neither config nor share data ever sits in one.
    #[test]
    fn prekey_rows_hold_nothing_else() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        ensure_vaults();
        for lane in 0..LANE_COUNT {
            for addr in (0x6000..0x6100usize).step_by(8) {
                let lay = layout(addr, lane);
                let mut arrays = lay.prekey.to_vec();
                arrays.sort_unstable();
                arrays.dedup();
                assert_eq!(arrays.len(), PREKEY_ROWS);
                let (s, m1, m2) = lay.config;
                for pos in [s, m1, m2].into_iter().chain(share_positions_from(addr, lane, &lay)) {
                    assert!(!lay.prekey.contains(&pos.array), "{pos:?} sits in a prekey array");
                }
            }
        }
    }

    /// The shares hold the shielded key, never the key, and one flipped prekey bit garbles it.
    #[test]
    fn the_key_is_stored_shielded_and_needs_every_prekey_bit() {
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let key = test_key(0x5c);
        TEST_KEY_A.set(key, &others_for_a());
        let (addr, lane) = (TEST_KEY_A.instance_addr(), TEST_KEY_A.lane());
        let lay = layout(addr, lane);
        let mut unshielded = [0u8; 32];
        for (i, pos) in share_positions_from(addr, lane, &lay).iter().enumerate() {
            let val = VAULTS[pos.array][pos.slot].load(Ordering::Relaxed).to_ne_bytes();
            let off = (i % USIZES_PER_SHARE) * std::mem::size_of::<usize>();
            for (a, b) in unshielded[off..off + val.len()].iter_mut().zip(val) { *a ^= b; }
        }
        assert_ne!(unshielded, key);

        let slot = &VAULTS[lay.prekey[3]][lane * LANE_WIDTH + 77];
        let original = slot.load(Ordering::Relaxed);
        slot.store(original ^ (1 << 13), Ordering::Relaxed);
        let garbled = TEST_KEY_A.get().unwrap();
        slot.store(original, Ordering::Relaxed);
        assert_ne!(garbled, key);
        assert_eq!(TEST_KEY_A.get(), Some(key));
        TEST_KEY_A.clear(&others_for_a());
    }

    /// Reads of one key stay whole while another key's set() reshuffles its prekey.
    #[test]
    fn reads_of_a_partner_survive_its_reshielding() {
        use std::sync::atomic::{AtomicBool, AtomicU64};
        use std::sync::Arc;
        let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let kb = test_key(0xB0);
        TEST_KEY_B.set(kb, &others_for_b());
        let stop = Arc::new(AtomicBool::new(false));
        let (reads, wrong) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let reader = {
            let (stop, reads, wrong) = (stop.clone(), reads.clone(), wrong.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    if TEST_KEY_B.get() != Some(kb) {
                        wrong.fetch_add(1, Ordering::SeqCst);
                    }
                    reads.fetch_add(1, Ordering::SeqCst);
                }
            })
        };
        for i in 0..300u16 {
            TEST_KEY_A.set(test_key(i as u8), &others_for_a());
            if i % 7 == 0 { TEST_KEY_A.clear(&others_for_a()); }
        }
        stop.store(true, Ordering::SeqCst);
        reader.join().unwrap();
        assert!(reads.load(Ordering::SeqCst) > 0);
        assert_eq!(wrong.load(Ordering::SeqCst), 0);
        TEST_KEY_A.clear(&others_for_a());
        TEST_KEY_B.clear(&others_for_b());
    }
}
