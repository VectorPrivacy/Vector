//! Adversarial checks on the vault's dump resistance: what an attacker who knows the scheme
//! but not the ASLR slide can recover from snapshots of the VAULTS block alone.
//!
//! Run: `CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true cargo test --release -p vector-core --lib
//! guarded_key::attack -- --ignored --nocapture --test-threads=1`

use super::*;
use std::collections::HashMap;
use crate::rt::time_std::Instant;

static PARTNER: GuardedKey = GuardedKey::empty();
static SETTER: GuardedKey = GuardedKey::empty();

type Dump = Vec<[usize; ARRAY_SIZE]>;

fn dump() -> Dump {
    VAULTS
        .iter()
        .map(|row| {
            let mut out = [0usize; ARRAY_SIZE];
            for (d, s) in out.iter_mut().zip(row) {
                *d = s.load(Ordering::Relaxed);
            }
            out
        })
        .collect()
}

fn changed(a: &Dump, b: &Dump) -> Vec<VaultPos> {
    let mut out = Vec::new();
    for array in 0..ARRAY_COUNT {
        for slot in 0..ARRAY_SIZE {
            if a[array][slot] != b[array][slot] {
                out.push(VaultPos { array, slot });
            }
        }
    }
    out
}

fn slot_lane(slot: usize) -> usize {
    slot / LANE_WIDTH
}

/// `shield()` computed from a dump instead of the live vault.
fn pad_from(d: &Dump, lane: usize, prekey: &[usize]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for &array in prekey {
        let row = &d[array][lane * LANE_WIDTH..(lane + 1) * LANE_WIDTH];
        // SAFETY: an initialised [usize] viewed as its bytes, for hashing only.
        let bytes = unsafe { std::slice::from_raw_parts(row.as_ptr() as *const u8, std::mem::size_of_val(row)) };
        hasher.update(bytes);
    }
    *hasher.finalize().as_bytes()
}

fn word(b: &[u8; 32], u: usize) -> usize {
    usize::from_ne_bytes(b[u * 8..u * 8 + 8].try_into().unwrap())
}

/// Every 4-subset of `cands` whose (D1 ^ D2) XOR equals `target`, by meet-in-the-middle on pairs.
struct PairIndex {
    deltas: Vec<usize>,
    pairs: HashMap<usize, Vec<(u16, u16)>>,
}

impl PairIndex {
    fn new(cands: &[VaultPos], d1: &Dump, d2: &Dump) -> Self {
        let deltas: Vec<usize> = cands.iter().map(|p| d1[p.array][p.slot] ^ d2[p.array][p.slot]).collect();
        let mut pairs: HashMap<usize, Vec<(u16, u16)>> = HashMap::new();
        for i in 0..deltas.len() {
            for j in i + 1..deltas.len() {
                pairs.entry(deltas[i] ^ deltas[j]).or_default().push((i as u16, j as u16));
            }
        }
        Self { deltas, pairs }
    }

    fn find4(&self, target: usize) -> Vec<[usize; 4]> {
        let mut out = Vec::new();
        let n = self.deltas.len();
        for i in 0..n {
            for j in i + 1..n {
                let want = target ^ self.deltas[i] ^ self.deltas[j];
                if let Some(list) = self.pairs.get(&want) {
                    for &(k, l) in list {
                        let (k, l) = (k as usize, l as usize);
                        if j < k {
                            out.push([i, j, k, l]);
                        }
                    }
                }
            }
        }
        out
    }
}

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn ordering(&mut self) -> [usize; PREKEY_ROWS] {
        let mut out = [0usize; PREKEY_ROWS];
        let mut i = 0;
        while i < PREKEY_ROWS {
            let c = (self.next() as usize) & (ARRAY_COUNT - 1);
            if !out[..i].contains(&c) {
                out[i] = c;
                i += 1;
            }
        }
        out
    }
}

fn setup(partner_key: [u8; 32]) {
    ensure_vaults();
    PARTNER.clear(&[&SETTER]);
    SETTER.clear(&[&PARTNER]);
    PARTNER.set(partner_key, &[&SETTER]);
}

/// A partner rewritten in place by someone else's set() would sit in one diff under two pads.
/// For the right prekey ordering, pad1 ^ pad2 picks out its 4 slots per word among the lane's
/// changed slots, and for a wrong one nothing matches: lanes × ordered prekeys, a 64-bit filter
/// and no curve operation per candidate. The partner must stay out of the diff entirely.
#[test]
#[ignore = "attack demo: run by name with --nocapture"]
fn partner_reshield_two_dump_attack() {
    let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let partner_key = [0x5Au8; 32];
    setup(partner_key);
    SETTER.set([0x11; 32], &[&PARTNER]);

    let d1 = dump();
    SETTER.set([0x22; 32], &[&PARTNER]);
    let d2 = dump();

    let diff = changed(&d1, &d2);
    let mut per_lane = [0usize; LANE_COUNT];
    for p in &diff {
        per_lane[slot_lane(p.slot)] += 1;
    }
    let lane = PARTNER.lane();
    let truth = share_positions(PARTNER.instance_addr(), lane);
    let in_diff = truth.iter().filter(|p| diff.contains(p)).count();
    println!("changed slots: {}  per lane: {:?}", diff.len(), per_lane);
    println!("partner lane {lane}: {in_diff}/{} of its share slots are in the diff", truth.len());
    if in_diff == SHARE_ENTRIES {
        recover_partner(&diff, &d1, &d2, lane, partner_key);
    }
    assert_eq!(in_diff, 0, "a partner never appears in another key's diff");


    // The key just set has no old copy in D1, so the same filter finds nothing for it.
    let sl = SETTER.lane();
    let scands: Vec<VaultPos> = diff.iter().copied().filter(|p| slot_lane(p.slot) == sl).collect();
    let sindex = PairIndex::new(&scands, &d1, &d2);
    let sprekey = layout(SETTER.instance_addr(), sl).prekey;
    let (s1, s2) = (pad_from(&d1, sl, &sprekey), pad_from(&d2, sl, &sprekey));
    let shits = sindex.find4(word(&s1, 0) ^ word(&s2, 0)).len();
    println!("same filter on the freshly set key: {shits} hits");

    PARTNER.clear(&[&SETTER]);
    SETTER.clear(&[&PARTNER]);
}

/// Over two diffs, decoys rarely land on the same slot twice, so anything rewritten in both
/// stands out. A partner's slots must not be among them.
#[test]
#[ignore = "attack demo: run by name with --nocapture"]
fn partner_slots_fall_out_of_two_diffs() {
    let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    setup([0x77; 32]);
    let d0 = dump();
    SETTER.set([0x01; 32], &[&PARTNER]);
    let d1 = dump();
    SETTER.set([0x02; 32], &[&PARTNER]);
    let d2 = dump();
    SETTER.clear(&[&PARTNER]);
    let d3 = dump();

    let a = changed(&d0, &d1);
    let b = changed(&d1, &d2);
    let c = changed(&d2, &d3);
    let ab: Vec<VaultPos> = a.iter().copied().filter(|p| b.contains(p)).collect();
    let abc: Vec<VaultPos> = ab.iter().copied().filter(|p| c.contains(p)).collect();
    let lane = PARTNER.lane();
    let truth = share_positions(PARTNER.instance_addr(), lane);
    for (name, set) in [("2 diffs", &ab), ("3 diffs", &abc)] {
        let mut per_lane = [0usize; LANE_COUNT];
        for p in set.iter() {
            per_lane[slot_lane(p.slot)] += 1;
        }
        let hit = truth.iter().filter(|p| set.contains(p)).count();
        println!(
            "{name}: {} slots in the intersection, {hit}/{} are partner shares; per lane {:?} (partner lane {lane})",
            set.len(),
            truth.len(),
            per_lane
        );
        assert_eq!(hit, 0, "a partner's slots never change under another key's writes");
    }
    PARTNER.clear(&[]);
}


/// The one-diff attack on a partner rewritten in place.
fn recover_partner(diff: &[VaultPos], d1: &Dump, d2: &Dump, lane: usize, partner_key: [u8; 32]) {
    let cands: Vec<VaultPos> = diff.iter().copied().filter(|p| slot_lane(p.slot) == lane).collect();
    let t = Instant::now();
    let index = PairIndex::new(&cands, d1, d2);
    println!("candidates in lane: {}  pair index built in {:?}", cands.len(), t.elapsed());

    let prekey = layout(PARTNER.instance_addr(), lane).prekey;
    let (p1, p2) = (pad_from(d1, lane, &prekey), pad_from(d2, lane, &prekey));
    let mut recovered = [0u8; 32];
    for u in 0..USIZES_PER_SHARE {
        let sols = index.find4(word(&p1, u) ^ word(&p2, u));
        assert_eq!(sols.len(), 1, "word {u}: the true prekey ordering yields exactly one 4-subset");
        let mut w = word(&p2, u);
        for &i in &sols[0] {
            w ^= d2[cands[i].array][cands[i].slot];
        }
        recovered[u * 8..u * 8 + 8].copy_from_slice(&w.to_ne_bytes());
    }
    assert_eq!(recovered, partner_key, "partner key recovered from two VAULTS snapshots");
    println!("recovered the partner key from the diff with the true ordering");

    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    let trials = 20_000;
    let mut false_hits = 0;
    let t = Instant::now();
    for _ in 0..trials {
        let guess = rng.ordering();
        if guess == prekey {
            continue;
        }
        let (g1, g2) = (pad_from(d1, lane, &guess), pad_from(d2, lane, &guess));
        if !index.find4(word(&g1, 0) ^ word(&g2, 0)).is_empty() {
            false_hits += 1;
        }
    }
    let per = t.elapsed() / trials;
    let t = Instant::now();
    for _ in 0..trials {
        let guess = rng.ordering();
        std::hint::black_box((pad_from(d1, lane, &guess), pad_from(d2, lane, &guess)));
    }
    let pads = t.elapsed() / trials;
    println!("wrong orderings: {false_hits}/{trials} false hits; {per:?} per guess with pair MITM, {pads:?} for the two pads alone");
    assert_eq!(false_hits, 0);

    let orderings: f64 = (0..PREKEY_ROWS).map(|i| (ARRAY_COUNT - i) as f64).product();
    let space = orderings * LANE_COUNT as f64;
    println!(
        "search space: {LANE_COUNT} lanes × {orderings:.3e} ordered prekeys = 2^{:.1}; \
         at the two-pad cost that is {:.1} core-years (a 4-subset table per lane makes the lookup O(1))",
        space.log2(),
        space * pads.as_secs_f64() / 3.156e7
    );

}

/// `layout_with` for a hypothetical VAULTS base, as an attacker brute-forcing the slide runs it.
fn layout_at(instance: usize, base: usize, lane: usize) -> Layout {
    let h0 = (instance ^ base).wrapping_mul(instance | 1);
    let iters = 16 + ((h0 >> 7) & 15);
    let m1 = (instance as u64) | 1;
    let m2 = (base as u64) | 1;
    let mut h = addr_mix((instance as u64) ^ (base as u64).rotate_left(19), m1, m2, iters);
    let mut prekey = [0usize; PREKEY_ROWS];
    for i in 0..PREKEY_ROWS {
        loop {
            let c = ((h >> 32) as usize) & (ARRAY_COUNT - 1);
            h = addr_mix(h, m1, m2, iters);
            if !prekey[..i].contains(&c) {
                prekey[i] = c;
                break;
            }
        }
    }
    let mut positions = [VaultPos { array: 0, slot: 0 }; 3];
    for i in 0..3 {
        loop {
            let c = VaultPos { array: ((h >> 32) as usize) & (ARRAY_COUNT - 1), slot: lane_slot(h as usize, lane) };
            h = addr_mix(h, m1, m2, iters);
            if !positions[..i].contains(&c) && !prekey.contains(&c.array) {
                positions[i] = c;
                break;
            }
        }
    }
    Layout { prekey, config: (positions[0], positions[1], positions[2]) }
}

/// `share_positions_from` reading a dump.
fn shares_at(d: &Dump, instance: usize, lane: usize, layout: &Layout) -> [VaultPos; SHARE_ENTRIES] {
    let (s, a, b) = layout.config;
    let seed = d[s.array][s.slot] as u64;
    let mul1 = d[a.array][a.slot] as u64;
    let mul2 = d[b.array][b.slot] as u64;
    let mut h = seed ^ (instance as u64).rotate_left(23);
    let mut out = [VaultPos { array: 0, slot: 0 }; SHARE_ENTRIES];
    let config = [s, a, b];
    for i in 0..SHARE_ENTRIES {
        loop {
            h ^= h >> 17;
            h = h.wrapping_mul(mul1 | 1);
            h ^= h >> 13;
            h = h.wrapping_mul(mul2 | 1);
            h ^= h >> 16;
            let c = VaultPos { array: ((h >> 32) as usize) & (ARRAY_COUNT - 1), slot: lane_slot(h as usize, lane) };
            if !config.contains(&c) && !out[..i].contains(&c) && !layout.prekey.contains(&c.array) {
                out[i] = c;
                break;
            }
        }
    }
    out
}

/// Brute-forcing the ASLR slide against two VAULTS snapshots, with the build's static offsets
/// known: a candidate (slide, lane) for the key just set survives only if all its share slots
/// changed. Times a
/// candidate under v2's 16–31 rounds and under v1's 4096–8191.
#[test]
#[ignore = "attack demo: run by name with --nocapture"]
fn aslr_slide_brute_with_a_diff_filter() {
    let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    setup([0x33; 32]);
    let d1 = dump();
    SETTER.set([0x44; 32], &[&PARTNER]);
    let d2 = dump();
    let mut is_changed = vec![false; ARRAY_COUNT * ARRAY_SIZE];
    for p in changed(&d1, &d2) {
        is_changed[p.array * ARRAY_SIZE + p.slot] = true;
    }

    let base = VAULTS.as_ptr() as usize;
    let offset = SETTER.instance_addr().wrapping_sub(base);
    let real = layout(SETTER.instance_addr(), SETTER.lane());
    let mine = layout_at(SETTER.instance_addr(), base, SETTER.lane());
    assert!(real.prekey == mine.prekey && real.config == mine.config, "replica matches the vault");

    let span = 1usize << 16;
    let t = Instant::now();
    let mut survivors = Vec::new();
    for k in 0..span {
        let cand_base = base.wrapping_add(k.wrapping_sub(span / 2) << 12);
        let inst = cand_base.wrapping_add(offset);
        for lane in 0..LANE_COUNT {
            let lay = layout_at(inst, cand_base, lane);
            let pos = shares_at(&d2, inst, lane, &lay);
            if pos.iter().all(|p| is_changed[p.array * ARRAY_SIZE + p.slot]) {
                survivors.push((k, lane));
            }
        }
    }
    let per = t.elapsed() / (span * LANE_COUNT) as u32;
    println!("{} candidates, survivors {:?} (true: k={}, lane {}), {per:?} each", span * LANE_COUNT, survivors, span / 2, SETTER.lane());
    assert_eq!(survivors, vec![(span / 2, SETTER.lane())]);

    // v1 cost per candidate: config positions alone took 3+ addr_mix calls at 4096–8191 rounds.
    let n = 2_000u32;
    let t = Instant::now();
    for k in 0..n as u64 {
        let mut h = k;
        for _ in 0..3 {
            h = addr_mix(h, 0x9E37_79B9_7F4A_7C15 ^ k | 1, 0xC2B2_AE3D_27D4_EB4F | 1, 6144);
        }
        std::hint::black_box(h);
    }
    let v1 = t.elapsed() / n;
    let cands = (1u64 << 28) as f64 * LANE_COUNT as f64;
    println!(
        "2^28 slides × 16 lanes: v2 {:.0} core-seconds, v1 {:.0} core-seconds",
        cands * per.as_secs_f64(),
        cands * v1.as_secs_f64()
    );
    PARTNER.clear(&[&SETTER]);
    SETTER.clear(&[&PARTNER]);
}

#[inline(never)]
fn scrub_stack() {
    let mut buf = [0u8; 80 * 1024];
    buf.zeroize();
    std::hint::black_box(&buf);
}

#[inline(never)]
fn read_and_wipe() {
    let mut k = PARTNER.get().unwrap();
    std::hint::black_box(&k);
    k.zeroize();
}

/// After a get() whose caller wipes its copy, does the dead stack still hold the key or the pad?
/// The pad is key-equivalent: pad XOR the in-vault shares is the key, no prekey needed.
#[test]
#[ignore = "attack demo: run by name with --nocapture"]
fn stack_residue_after_get() {
    let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let key = Box::new([0xC3u8; 32]);
    setup(*key);
    let mut snap = vec![0u8; 64 * 1024];
    let marker = 0u8;
    let top = std::hint::black_box(&marker) as *const u8 as usize;
    scrub_stack();
    read_and_wipe();
    // SAFETY: reads this thread's own stack below the live frame; the region is mapped.
    unsafe {
        for (i, b) in snap.iter_mut().enumerate() {
            *b = std::ptr::read_volatile((top - 64 * 1024 + i) as *const u8);
        }
    }
    let lane = PARTNER.lane();
    let pad = shield(lane, &layout(PARTNER.instance_addr(), lane).prekey);
    let find = |needle: &[u8]| snap.windows(needle.len()).filter(|w| *w == needle).count();
    println!("dead stack, 64 KiB below the caller: key copies {}, pad copies {}", find(&key[..]), find(&pad));
    PARTNER.clear(&[]);
}

/// The cost of keeping decoys off live prekey rows: those rows go quiet. Counts the 256-slot
/// rows that no decoy touched across N diffs, and how many of them are the partner's prekey.
#[test]
#[ignore = "attack demo: run by name with --nocapture"]
fn quiet_prekey_rows_by_diff_count() {
    let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    setup([0x66; 32]);
    let lane = PARTNER.lane();
    let prekey = layout(PARTNER.instance_addr(), lane).prekey;
    let mut touched = vec![false; ARRAY_COUNT * LANE_COUNT];
    let mut prev = dump();
    for n in 1..=4u8 {
        if n % 2 == 1 { SETTER.set([n; 32], &[&PARTNER]) } else { SETTER.clear(&[&PARTNER]) }
        let next = dump();
        for p in changed(&prev, &next) {
            touched[p.array * LANE_COUNT + slot_lane(p.slot)] = true;
        }
        let quiet: Vec<(usize, usize)> = (0..ARRAY_COUNT * LANE_COUNT)
            .filter(|&i| !touched[i])
            .map(|i| (i / LANE_COUNT, i % LANE_COUNT))
            .collect();
        let real = quiet.iter().filter(|(a, l)| *l == lane && prekey.contains(a)).count();
        println!("{n} diff(s): {} quiet rows, {real}/{PREKEY_ROWS} of them the partner's prekey", quiet.len());
        prev = next;
    }
    SETTER.clear(&[&PARTNER]);
    PARTNER.clear(&[]);
}

/// set() and clear() with one live partner, median of 15 batches.
#[test]
#[ignore = "benchmark"]
fn bench_set_with_a_live_partner() {
    let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
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

/// A key set and later replaced or cleared would, if its shares were scrubbed, show up in both
/// diffs: written in the first, rewritten in the second. The dump between holds it live.
#[test]
#[ignore = "attack demo: run by name with --nocapture"]
fn retired_slots_stay_out_of_two_diffs() {
    let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    setup([0x55; 32]);
    for retire_by_clear in [false, true] {
        let d0 = dump();
        let first = [0x99u8; 32];
        SETTER.set(first, &[&PARTNER]);
        let lane = SETTER.lane();
        let live = share_positions(SETTER.instance_addr(), lane);
        let prekey = layout(SETTER.instance_addr(), lane).prekey;
        let d1 = dump();
        if retire_by_clear { SETTER.clear(&[&PARTNER]) } else { SETTER.set([0x98; 32], &[&PARTNER]) }
        let d2 = dump();

        let b = changed(&d1, &d2);
        let both: Vec<VaultPos> = changed(&d0, &d1).into_iter().filter(|p| b.contains(p)).collect();
        let hit = live.iter().filter(|p| both.contains(p)).count();
        let mut stale = pad_from(&d2, lane, &prekey);
        for (i, p) in live.iter().enumerate() {
            let off = (i % USIZES_PER_SHARE) * 8;
            for (a, b) in stale[off..off + 8].iter_mut().zip(d2[p.array][p.slot].to_ne_bytes()) { *a ^= b; }
        }
        println!(
            "retired by {}: {} slots in both diffs, {hit}/{} of them the key's live slots; old key readable after: {}",
            if retire_by_clear { "clear" } else { "set" },
            both.len(),
            live.len(),
            stale == first
        );
        // Decoys hit any slot with p = 1/64 per pass, so stray hits are ~Binomial(16, 1/64);
        // a scrubbed retirement shows all 16.
        assert!(hit <= 8, "a retired key's slots are not rewritten: {hit}/16");
        assert_ne!(stale, first, "a retired key cannot be read back from what it left behind");
        if !retire_by_clear { SETTER.clear(&[&PARTNER]); }
    }
    PARTNER.clear(&[]);
}

/// A slot whose value changed by its lowest bit alone was forced odd, not decoyed: random
/// decoys almost never do that, so it names a key's multiplier slots.
#[test]
fn low_bit_only_changes_across_sets() {
    let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    setup([0x21; 32]);
    let mut flagged = 0;
    let sets = 40u8;
    for n in 0..sets {
        let d1 = dump();
        SETTER.set([n; 32], &[&PARTNER]);
        let d2 = dump();
        flagged += changed(&d1, &d2).iter().filter(|p| d1[p.array][p.slot] ^ d2[p.array][p.slot] == 1).count();
    }
    println!("{sets} sets: {flagged} slots changed by their low bit alone");
    assert_eq!(flagged, 0, "every write is a whole random word");
    SETTER.clear(&[&PARTNER]);
    PARTNER.clear(&[]);
}

/// A key set again into a lane it held before re-derives that lane's positions. Unless they
/// move, its new shares land on its old slots, which then change in two diffs.
#[test]
fn a_lane_held_again_gets_fresh_slots() {
    let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    setup([0x31; 32]);
    let mut seen: HashMap<usize, [VaultPos; SHARE_ENTRIES]> = HashMap::new();
    let (mut reuses, mut same) = (0, 0);
    for n in 0..200u8 {
        SETTER.set([n; 32], &[&PARTNER]);
        let lane = SETTER.lane();
        let now = share_positions(SETTER.instance_addr(), lane);
        if let Some(before) = seen.get(&lane) {
            reuses += 1;
            let overlap = now.iter().filter(|p| before.contains(p)).count();
            if overlap == SHARE_ENTRIES {
                same += 1;
            }
        }
        seen.insert(lane, now);
    }
    println!("{reuses} sets into a lane held before: {same} landed on exactly the same 16 slots");
    assert!(reuses > 0, "the run reused a lane");
    assert_eq!(same, 0, "a fresh seed moves the shares");
    SETTER.clear(&[&PARTNER]);
    PARTNER.clear(&[]);
}

/// What the round count buys: a read's layout cost against one slide guess's, per setting.
#[test]
#[ignore = "benchmark"]
fn bench_rounds_dial() {
    let _l = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
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
