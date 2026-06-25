/**
 * Teeth check: prove the invariant harness actually catches a real violation.
 *
 * We monkey-patch the cache's methods IN THE TEST (never the source) to
 * reintroduce the exact regressions the invariants exist to guard, then assert
 * that assertInvariants / assertContiguousWithTail go RED. If a break did NOT
 * trip an assertion, the harness would be toothless — so each case fails the
 * test when the violation slips through.
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { loadCache } from './load-cache.mjs';
import { assertInvariants, assertContiguousWithTail, TailTracker } from './invariants.mjs';
import { run, ev } from './util.mjs';

const CID = 'chat:teeth';

/** Run `fn`; return true iff it threw an AssertionError (the harness bit). */
function caught(fn) {
    try { fn(); return false; }
    catch (e) { return e instanceof assert.AssertionError; }
}

test('teeth: dropSeekSegment that fails to clear gapAfterId is caught', () => {
    const { eventCache: cache } = loadCache();
    const tail = new TailTracker();

    const tailRun = run('tail', 1000, 6);
    cache.addEvents(CID, tailRun, false);
    tail.observeAll(tailRun);
    cache.seedWindow(CID, run('seek', 100, 5));
    const entry = cache.cache.get(CID);

    // MONKEY-PATCH: drop the seek rows but (buggily) leave gapAfterId dangling,
    // pointing at a now-removed row. This is the jump-to-bottom regression.
    const realDrop = cache.dropSeekSegment.bind(cache);
    cache.dropSeekSegment = (id) => {
        const e = cache.cache.get(id);
        const savedGap = e.gapAfterId;
        realDrop(id);
        e.gapAfterId = savedGap; // BUG: don't clear it
    };

    cache.dropSeekSegment(CID);

    // The dangling gap (#4) AND the not-contiguous shape must both be caught.
    assert.ok(
        caught(() => assertInvariants(entry, { tail })),
        'assertInvariants must RED on a dangling gapAfterId'
    );
    assert.ok(
        caught(() => assertContiguousWithTail(entry, tailRun)),
        'assertContiguousWithTail must RED when state is not contiguous'
    );

    // Restore + confirm the harness goes GREEN once the bug is removed.
    cache.dropSeekSegment = realDrop;
    entry.gapAfterId = null; // what the correct drop would have done
    assert.ok(!caught(() => assertInvariants(entry, { tail })),
        'harness is GREEN after the monkey-patch is removed');
    assertContiguousWithTail(entry, tailRun, 'restored');
});

test('teeth: an insert that skips the sort (out-of-order) is caught', () => {
    const { eventCache: cache } = loadCache();
    const tail = new TailTracker();

    const tailRun = run('tail', 1000, 6);
    cache.addEvents(CID, tailRun, false);
    tail.observeAll(tailRun);
    const entry = cache.cache.get(CID);
    assert.ok(!caught(() => assertInvariants(entry, { tail })), 'clean before break');

    // MONKEY-PATCH a bad insert: shove an OLD row onto the end without sorting,
    // breaking ASC order (invariant #1) and unpinning the tail (invariant #3a).
    entry.events.push(ev('rogue-old', 1)); // @1, far older than the tail
    entry.eventIds.add('rogue-old');

    assert.ok(
        caught(() => assertInvariants(entry, { tail })),
        'assertInvariants must RED on an out-of-order / unpinned-tail insert'
    );
});

test('teeth: a dropped tail row (eviction into the tail) is caught', () => {
    const { eventCache: cache } = loadCache();
    const tail = new TailTracker();

    const tailRun = run('tail', 1000, 6);
    cache.addEvents(CID, tailRun, false);
    tail.observeAll(tailRun); // newest = tail-5
    const entry = cache.cache.get(CID);

    // MONKEY-PATCH: evict the newest (tail) row — the thing trimToMax must NEVER do.
    entry.events.pop();                 // removes tail-5
    entry.eventIds.delete('tail-5');

    assert.ok(
        caught(() => assertInvariants(entry, { tail })),
        'assertInvariants must RED when the pinned newest row is dropped'
    );
});
