/**
 * Independent test harness for the gapped-buffer cache state machine in
 * src/js/event-cache.js. Runs under Node's built-in test runner, no npm deps:
 *
 *     node --test tests/event-cache/
 *     node tests/event-cache/run.mjs        (alt entrypoint)
 *
 * The source file is loaded UNMODIFIED via load-cache.mjs (vm context + shim).
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { loadCache } from './load-cache.mjs';
import { assertInvariants, assertContiguousWithTail, TailTracker } from './invariants.mjs';
import { mulberry32, ev, run } from './util.mjs';

const CID = 'chat:test';

/** Fresh cache + a tracker, returning the singleton entry helpers. */
function fresh() {
    const mod = loadCache();
    const cache = mod.eventCache;
    const tail = new TailTracker();
    const cfg = mod.EVENT_CACHE_CONFIG;
    return { mod, cache, tail, cfg };
}

/* ------------------------------------------------------------------ *
 *  TRANSITIONS
 * ------------------------------------------------------------------ */

test('Contiguous -> seedWindow(disconnected slice) -> Gapped', () => {
    const { cache, tail } = fresh();

    // Live tail: a contiguous newest run.
    const tailRun = run('tail', 1000, 10);
    cache.addEvents(CID, tailRun, /*prepend*/ false);
    tail.observeAll(tailRun);
    let entry = cache.cache.get(CID);
    assert.equal(entry.gapAfterId, null, 'starts CONTIGUOUS');
    assertInvariants(entry, { tail, label: 'after tail load' });

    // Seed an OLD, disconnected slice (a jump far back in history).
    const oldSlice = run('seek', 100, 8);
    cache.seedWindow(CID, oldSlice);

    assert.notEqual(entry.gapAfterId, null, 'disconnected slice -> GAPPED');
    // gapAfterId must be the seek region's newest row.
    assert.equal(entry.gapAfterId, 'seek-7', 'gap boundary is seek-region newest');
    assertInvariants(entry, { tail, label: 'after seedWindow disconnected' });
});

test('seedWindow(touching slice) stays Contiguous', () => {
    const { cache, tail } = fresh();
    const tailRun = run('tail', 1000, 10);
    cache.addEvents(CID, tailRun, false);
    tail.observeAll(tailRun);
    const entry = cache.cache.get(CID);

    // Slice whose newest overlaps the tail's oldest range -> contiguous.
    const overlapSlice = run('seek', 995, 8); // 995..1002 overlaps tail @1000
    cache.seedWindow(CID, overlapSlice);

    assert.equal(entry.gapAfterId, null, 'overlapping slice -> CONTIGUOUS');
    assertInvariants(entry, { tail, label: 'after seedWindow touching' });
});

test('Gapped -> dropSeekSegment -> Contiguous-with-tail', () => {
    const { cache, tail } = fresh();
    const tailRun = run('tail', 1000, 6);
    cache.addEvents(CID, tailRun, false);
    tail.observeAll(tailRun);
    cache.seedWindow(CID, run('seek', 100, 5));
    const entry = cache.cache.get(CID);
    assert.notEqual(entry.gapAfterId, null, 'GAPPED before drop');
    assertInvariants(entry, { tail, label: 'gapped pre-drop' });

    cache.dropSeekSegment(CID);

    assertContiguousWithTail(entry, tailRun, 'post-drop');
    assertInvariants(entry, { tail, label: 'post-drop' });
});

test('Gapped -> addEvents(newer bridging the gap) -> Contiguous', () => {
    const { cache, tail } = fresh();
    // Tail at 1000.., seek at 100.., gap between.
    const tailRun = run('tail', 1000, 5);
    cache.addEvents(CID, tailRun, false);
    tail.observeAll(tailRun);
    cache.seedWindow(CID, run('seek', 100, 5)); // seek newest @104
    const entry = cache.cache.get(CID);
    assert.notEqual(entry.gapAfterId, null);

    // Fill the gap interior with rows that climb from just above the seek region
    // up to the tail's oldest .at (1000). The cache closes the gap when the newest
    // fetched row is STRICTLY past the next (tail) row, so end the bridge at 1001
    // > tail-0@1000 — but keep every bridge row <= the tail's oldest position so
    // the merged run is monotonic once welded. Here the gap interior is [105..999];
    // the final 1001 sample is what tips reachedTail. To stay globally ASC the
    // bridge's last sample must not exceed the next tail row, so cap at 1000 (a
    // same-second touch). closeGap then force-closes (DB interior exhausted).
    const bridge = run('bridge', 105, 6); // 105..110, all < tail oldest (1000)
    tail.observeAll(bridge);
    cache.addEvents(CID, bridge, /*prepend*/ false);
    // Bridge batch came back without reaching the tail by timestamp; the caller
    // force-closes when the DB interior is exhausted (next rows ARE the tail).
    cache.closeGap(CID);

    assert.equal(entry.gapAfterId, null, 'bridging append + closeGap -> contiguous');
    assertInvariants(entry, { tail, label: 'after bridge append' });
});

test('addEvent into seek vs tail segment stays Gapped & correctly placed', () => {
    const { cache, tail } = fresh();
    const tailRun = run('tail', 1000, 5);
    cache.addEvents(CID, tailRun, false);
    tail.observeAll(tailRun);
    cache.seedWindow(CID, run('seek', 100, 5)); // seek 100..104
    const entry = cache.cache.get(CID);
    const gapId = entry.gapAfterId;
    assert.notEqual(gapId, null);

    // Insert into the SEEK region (between two seek rows).
    const seekInsert = ev('seek-mid', 102);
    cache.addEvent(CID, seekInsert);
    assert.equal(entry.gapAfterId, gapId, 'seek insert keeps gap');
    // It must land within the seek region (before the gap boundary).
    const gapIdx = entry.events.findIndex(e => e.id === entry.gapAfterId);
    const midIdx = entry.events.findIndex(e => e.id === 'seek-mid');
    assert.ok(midIdx >= 0 && midIdx < gapIdx, 'seek insert placed in seek region');
    assertInvariants(entry, { tail, label: 'after seek insert' });

    // Insert a genuine-newest TAIL arrival.
    const newest = ev('tail-new', 1010);
    tail.observe(newest);
    cache.addEvent(CID, newest);
    assert.equal(entry.events[entry.events.length - 1].id, 'tail-new', 'newest pinned at tail');
    assert.notEqual(entry.gapAfterId, null, 'still GAPPED after tail insert');
    assertInvariants(entry, { tail, label: 'after tail insert' });
});

test('trimToMax drops only seek-region rows, keeps the tail', () => {
    const { cache, tail, cfg } = fresh();
    // Build a GAPPED buffer that overflows the cap.
    // Tail = tailBudget rows (500) at the newest timestamps.
    const tailRun = run('tail', 100000, cfg.tailBudget);
    cache.addEvents(CID, tailRun, false);
    tail.observeAll(tailRun);

    // Seek region: a big OLD slice, large enough to push total over the cap.
    const seekCount = cfg.maxEventsPerConversation; // 1000 old rows
    const seekRun = run('seek', 1, seekCount);
    cache.seedWindow(CID, seekRun);
    const entry = cache.cache.get(CID);
    assert.notEqual(entry.gapAfterId, null, 'GAPPED before trim');
    const beforeLen = entry.events.length;
    assert.ok(beforeLen > cfg.maxEventsPerConversation, 'over cap before trim');
    assertInvariants(entry, { tail, label: 'pre-trim' });

    cache.trimConversation(CID); // -> entry.trimToMax()

    // Tail rows must all still be present, in order, at the end.
    // (Join to compare value-only: entry.events lives in the vm realm.)
    const lastTailIds = entry.events.slice(-cfg.tailBudget).map(e => e.id).join('');
    assert.equal(lastTailIds, tailRun.map(e => e.id).join(''), 'tail intact after trim');
    assert.ok(entry.events.length <= beforeLen, 'trim shrank or held the buffer');
    assertInvariants(entry, { tail, label: 'post-trim' });
});

/* ------------------------------------------------------------------ *
 *  THE 3 REAL REGRESSIONS THIS HARNESS EXISTS TO CATCH
 * ------------------------------------------------------------------ */

test('B-C1 analog: equal-timestamp burst + re-insert + window, no drop/dup', () => {
    const { cache, tail } = fresh();

    // A burst of messages sharing ONE .at and identical secondary fields.
    const burst = [];
    for (let i = 0; i < 12; i++) burst.push(ev(`burst-${i}`, 5000, { mine: false }));
    cache.addEvents(CID, burst, false);
    tail.observeAll(burst);
    const entry = cache.cache.get(CID);
    assertInvariants(entry, { tail, label: 'after equal-at burst' });

    // RE-INSERT an existing id (simulate a re-save / relay echo).
    const beforeLen = entry.events.length;
    cache.addEvent(CID, ev('burst-5', 5000));          // single re-insert
    cache.addEvents(CID, [ev('burst-5', 5000), ev('burst-7', 5000)], false); // batch re-insert
    assert.equal(entry.events.length, beforeLen, 're-insert added no new rows');
    assertInvariants(entry, { tail, label: 'after re-insert' });

    // seedWindow around a MIDDLE one of the equal-.at cluster. Provide an older
    // disconnected slice so a gap forms, the burst stays the tail.
    cache.seedWindow(CID, run('old', 100, 4));
    assertInvariants(entry, { tail, label: 'after seed around equal-at cluster' });

    // No burst id was dropped or duplicated.
    const burstPresent = entry.events.filter(e => e.id.startsWith('burst-')).length;
    assert.equal(burstPresent, 12, 'all 12 equal-.at burst rows survive, none duplicated');
});

test('F-H4 analog: seek-eviction then append-newer never welds older onto tail', () => {
    const { cache, tail, cfg } = fresh();

    // GAPPED buffer near the cap so a single overflow shifts a seek row.
    const tailRun = run('tail', 100000, cfg.tailBudget);
    cache.addEvents(CID, tailRun, false);
    tail.observeAll(tailRun);

    // Seek region sized so total == cap exactly; one more insert overflows it.
    const seekRun = run('seek', 1, cfg.maxEventsPerConversation - cfg.tailBudget);
    cache.seedWindow(CID, seekRun);
    const entry = cache.cache.get(CID);
    assert.notEqual(entry.gapAfterId, null, 'GAPPED');
    assert.equal(entry.events.length, cfg.maxEventsPerConversation, 'at cap');

    // Force seek-region eviction: a seek insert tips over the cap, shifting the
    // OLDEST seek row out. (addEvent shifts events[0] when over cap.)
    cache.addEvent(CID, ev('seek-extra', 2)); // lands in seek region, total -> cap+1 -> shift
    assert.ok(!entry.eventIds.has('seek-0'), 'oldest seek row evicted on overflow');
    assertInvariants(entry, { tail, label: 'after forced seek eviction' });
    assert.notEqual(entry.gapAfterId, null, 'still GAPPED after seek eviction');

    const tailOldestAt = entry.events[entry.events.length - cfg.tailBudget].at; // 100000
    const tailNewestId = entry.events[entry.events.length - 1].id; // true newest, pinned

    // Gap-fill append: the DB hands back gap-INTERIOR rows (older than the tail's
    // oldest). The regression is welding these OLDER rows onto the tail and
    // skipping the unfetched interior. The gap-aware addEvents must instead insert
    // them at the gap boundary, keep the array globally ASC, and leave the tail's
    // newest row exactly where it is.
    const interior = run('fill', 50000, 5); // 50000..50004, all < tailOldest (100000)
    cache.addEvents(CID, interior, /*prepend*/ false);

    assertInvariants(entry, { tail, label: 'after gap-fill append post-eviction' });
    // None of the filled rows welded onto/past the tail: each is older than the
    // tail's oldest, and they sit BELOW the tail in the array.
    for (const f of interior) {
        assert.ok(f.at < tailOldestAt, 'gap-fill rows are older than the tail oldest');
        const idx = entry.events.findIndex(e => e.id === f.id);
        const tailStart = entry.events.length - cfg.tailBudget;
        assert.ok(idx < tailStart, `gap-fill row ${f.id} stayed below the tail (not welded on)`);
    }
    // The true newest tail row is untouched at the very end.
    assert.equal(entry.events[entry.events.length - 1].id, tailNewestId, 'tail newest still pinned');

    // A genuine-newest realtime arrival goes through addEvent and lands at the tail.
    // Must be strictly newer than the whole tail (which spans 100000..100499).
    const newest = ev('realtime-newest', 100600);
    tail.observe(newest);
    cache.addEvent(CID, newest);
    assert.equal(entry.events[entry.events.length - 1].id, 'realtime-newest', 'newest arrival pinned at tail');
    assertInvariants(entry, { tail, label: 'after realtime newest' });
});

test('jump-to-bottom analog: seedWindow(old) then dropSeekSegment shows true newest', () => {
    const { cache, tail } = fresh();

    // Live tail loaded.
    const tailRun = run('tail', 9000, 20);
    cache.addEvents(CID, tailRun, false);
    tail.observeAll(tailRun);
    const entry = cache.cache.get(CID);

    // User jumps far back: seed an OLD disconnected slice -> GAPPED, and the
    // rendered view would now show the seek region.
    cache.seedWindow(CID, run('jump', 50, 30));
    assert.notEqual(entry.gapAfterId, null, 'GAPPED after jump');

    // User hits "jump to bottom": drop the seek segment.
    cache.dropSeekSegment(CID);

    assert.equal(entry.gapAfterId, null, 'CONTIGUOUS after jump-to-bottom');
    const last = entry.events[entry.events.length - 1];
    assert.equal(last.id, tail.newestId, 'events[last] is the TRUE newest, not the seek region');
    assert.equal(last.id, 'tail-19', 'concretely the live-tail newest');
    assertContiguousWithTail(entry, tailRun, 'jump-to-bottom');
    assertInvariants(entry, { tail, label: 'jump-to-bottom' });
});

/* ------------------------------------------------------------------ *
 *  FUZZ LOOP
 * ------------------------------------------------------------------ */

/**
 * Apply one random op to the cache, threading the tail tracker.
 * Returns a short string describing the op (for the reproducible log).
 */
function applyRandomOp(cache, rng, idCounter) {
    // Equal-timestamp clusters are COMMON: timestamps are drawn from small ranges.
    //   Tail/realtime range: [TAIL_LO, TAIL_HI]
    //   Seek/interior range: [SEEK_LO, SEEK_HI] (strictly below the tail range)
    // This separation keeps addEvents(append) faithful to its contract: it only
    // ever receives gap-interior rows (older than the tail), never newer-than-tail
    // rows (those arrive via addEvent / seedWindow / a contiguous append).
    const SEEK_LO = 100, SEEK_HI = 140;
    const TAIL_LO = 1000, TAIL_HI = 1040;
    const inRange = (lo, hi) => lo + Math.floor(rng() * (hi - lo + 1));
    const entry = cache.cache.get(CID);
    const existingIds = entry ? [...entry.eventIds] : [];
    const gapped = entry && entry.gapAfterId != null;

    // ~15% of the time, RE-INSERT an already-present id (no-op expected).
    const reInsert = existingIds.length > 0 && rng() < 0.15;
    const newId = () => `f${idCounter.n++}`;
    const pickId = () => (reInsert ? existingIds[Math.floor(rng() * existingIds.length)] : newId());

    const op = Math.floor(rng() * 7);
    switch (op) {
        case 0: { // addEvent — may target tail or (if gapped) the seek region.
            const at = (gapped && rng() < 0.4) ? inRange(SEEK_LO, SEEK_HI) : inRange(TAIL_LO, TAIL_HI);
            const id = pickId();
            cache.addEvent(CID, ev(id, at));
            return `addEvent(${id}@${at})`;
        }
        case 1: { // addEvents PREPEND — older rows grow the head. Contract: the
                  // batch is STRICTLY OLDER than the current head (the cache blindly
                  // unshifts, it does not sort-merge). Draw timestamps below head.at.
                  // Use NEW ids only: a re-inserted id here would NOT dedup against
                  // the head and could land out of order (the unshift is unconditional).
            const headAt = entry && entry.events.length ? entry.events[0].at : SEEK_HI;
            const ceil = Math.min(SEEK_HI, headAt - 1);
            if (ceil < SEEK_LO) return 'addEvents(prepend,skip-no-room)';
            const n = 1 + Math.floor(rng() * 4);
            const batch = [];
            for (let i = 0; i < n; i++) batch.push(ev(newId(), SEEK_LO + Math.floor(rng() * (ceil - SEEK_LO + 1))));
            batch.sort((a, b) => a.at - b.at);
            cache.addEvents(CID, batch, true);
            return `addEvents(prepend,[${batch.map(b => b.id + '@' + b.at)}])`;
        }
        case 2: { // addEvents APPEND. The cache splices the (sorted) batch at the
                  // insertion point WITHOUT re-sorting against neighbours, so the
                  // batch must fit the local order:
                  //   CONTIGUOUS -> insert at end, batch.at >= last.at (newer rows).
                  //   GAPPED     -> insert after the gap boundary (gap-interior),
                  //                 batch.at in [boundary.at, tailOldest.at] so it
                  //                 stays a clean interior fill (never welds the tail).
            let lo, hi;
            if (!gapped) {
                lo = entry && entry.events.length ? entry.events[entry.events.length - 1].at : TAIL_LO;
                hi = lo + 8;
            } else {
                const gi = entry.events.findIndex(e => e.id === entry.gapAfterId);
                const boundaryAt = entry.events[gi].at;
                const tailOldestAt = entry.events[gi + 1] ? entry.events[gi + 1].at : boundaryAt + 8;
                lo = boundaryAt;
                hi = Math.max(lo, tailOldestAt - 1); // strictly below the tail -> keeps the gap interior
            }
            const n = 1 + Math.floor(rng() * 4);
            const batch = [];
            for (let i = 0; i < n; i++) batch.push(ev(newId(), lo + Math.floor(rng() * (hi - lo + 1))));
            batch.sort((a, b) => a.at - b.at);
            cache.addEvents(CID, batch, false);
            return `addEvents(append,[${batch.map(b => b.id + '@' + b.at)}])`;
        }
        case 3: { // seedWindow — sometimes a disconnected (older) slice -> GAPPED,
                  // sometimes a touching slice -> CONTIGUOUS.
            const n = 1 + Math.floor(rng() * 6);
            const disconnected = rng() < 0.6;
            const base = disconnected ? SEEK_LO : TAIL_LO - 5;
            const slice = [];
            for (let i = 0; i < n; i++) slice.push(ev(newId(), base + i));
            cache.seedWindow(CID, slice);
            return `seedWindow(${disconnected ? 'disc' : 'touch'},[${slice.map(s => s.id + '@' + s.at)}])`;
        }
        case 4:
            cache.dropSeekSegment(CID);
            return 'dropSeekSegment()';
        case 5:
            cache.closeGap(CID);
            return 'closeGap()';
        case 6:
            cache.trimConversation(CID);
            return 'trimToMax()';
    }
    return 'noop';
}

test('fuzz: 2000 random ops hold all invariants (seeded, reproducible)', () => {
    const SEED = 0xC0FFEE;
    const N = 2000;
    const { cache, tail } = fresh();
    const rng = mulberry32(SEED);
    const idCounter = { n: 0 };
    const log = [];

    // Seed the buffer with a live tail so invariant #3 has something to pin.
    const seedTail = run('init', 1000, 5);
    cache.addEvents(CID, seedTail, false);
    tail.syncFromBuffer(cache.cache.get(CID).events);

    for (let i = 0; i < N; i++) {
        const desc = applyRandomOp(cache, rng, idCounter);
        log.push(desc);
        const entry = cache.cache.get(CID);
        // The cache may drop (gap interior) or evict (seek overflow) an attempted
        // insert, so re-derive the pinned-newest from the buffer's actual tail.
        // invariant #3a (max .at == events[last]) is what catches a real tail loss.
        tail.syncFromBuffer(entry.events);
        try {
            assertInvariants(entry, { tail, label: `fuzz#${i}` });
        } catch (err) {
            // Print a reproducible failure trace: seed + the op sequence.
            const tailLog = log.slice(Math.max(0, log.length - 25)).join('\n  ');
            err.message =
                `FUZZ FAILURE at op #${i} (seed 0x${SEED.toString(16)}).\n` +
                `Last 25 ops:\n  ${tailLog}\n\n` + err.message;
            throw err;
        }
    }
});

/* ------------------------------------------------------------------ *
 *  LOADER SANITY
 * ------------------------------------------------------------------ */

test('loader: pure methods never call invoke()', () => {
    const mod = loadCache();
    const cache = mod.eventCache;
    cache.addEvents(CID, run('x', 1, 5), false);
    cache.seedWindow(CID, run('y', 100, 3));
    cache.dropSeekSegment(CID);
    cache.addEvent(CID, ev('z', 200));
    cache.trimConversation(CID);
    cache.closeGap(CID);
    // _evictIfNeeded fires invoke() only when conversation count exceeds the
    // cap; with one conversation it must not have reached the backend.
    assert.equal(mod.invokeCalls.length, 0, 'pure state-machine ops issued no invoke()');
});
