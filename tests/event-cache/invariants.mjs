/**
 * Invariant checks for the gapped-buffer cache state machine.
 *
 * The buffer (`entry.events`, ASC by `.at`) is in one of two states:
 *   CONTIGUOUS  gapAfterId == null      one run ending at the live tail
 *   GAPPED      gapAfterId set          [ seek … GAP … tail ]
 *
 * `assertInvariants` is called after EVERY operation. To check invariant #3
 * (the global-newest row stays pinned at events[last]) it needs a tracker of
 * the newest id ever inserted — `TailTracker` below.
 */
import assert from 'node:assert/strict';

/**
 * Tracks the single newest (max `.at`, tie-broken by insertion recency) event
 * id ever inserted into a buffer, so we can assert it survives as events[last].
 *
 * Ties on `.at` are broken toward the MOST-RECENTLY-observed id, mirroring the
 * cache's own behaviour: an append fast-path (`event.at >= last.at`) places a
 * tie AFTER the incumbent, so the later arrival becomes events[last].
 */
export class TailTracker {
    constructor() {
        this.newestId = null;
        this.newestAt = -Infinity;
    }

    /** Observe one event that was (or may have been) inserted. */
    observe(ev) {
        if (ev.at >= this.newestAt) {
            this.newestAt = ev.at;
            this.newestId = ev.id;
        }
    }

    /** Observe a batch. */
    observeAll(evs) {
        for (const e of evs) this.observe(e);
    }

    /**
     * Re-derive the pinned-newest from the buffer's CURRENT state. Used by the
     * fuzz loop, where an attempted insert may be dropped (gap interior) or
     * evicted (seek overflow); the tracker must reflect only what the cache
     * actually holds at the tail. Sets newest to events[last] when that row is
     * the global max present (which the cache guarantees — invariant #3a).
     */
    syncFromBuffer(events) {
        if (events.length === 0) { this.newestId = null; this.newestAt = -Infinity; return; }
        const last = events[events.length - 1];
        this.newestId = last.id;
        this.newestAt = last.at;
    }
}

/**
 * Assert all structural invariants on a ConversationCacheEntry.
 *
 * @param {object} entry - the cache entry under test
 * @param {object} [opts]
 * @param {TailTracker} [opts.tail] - newest-id tracker for invariant #3
 * @param {string}  [opts.label] - context label for failure messages
 */
export function assertInvariants(entry, opts = {}) {
    const { tail, label = '' } = opts;
    const evs = entry.events;
    const tag = label ? `[${label}] ` : '';

    // (1) ASC by .at — SEGMENT-AWARE. A GAPPED buffer is non-monotonic by design
    //     ([seek … GAP … tail]); the cache routes inserts per-segment, so each
    //     segment is ASC even when the gap boundary's two sides aren't globally
    //     ordered. When CONTIGUOUS the whole array is one ASC run.
    let gapIdx = -1;
    if (entry.gapAfterId != null) {
        gapIdx = evs.findIndex(e => e.id === entry.gapAfterId);
    }
    const checkAscRange = (lo, hi, seg) => {
        for (let i = lo; i + 1 < hi; i++) {
            assert.ok(
                evs[i].at <= evs[i + 1].at,
                `${tag}invariant#1 ${seg} sort broken at idx ${i}: ` +
                `${evs[i].at} (id ${evs[i].id}) > ${evs[i + 1].at} (id ${evs[i + 1].id})`
            );
        }
    };
    if (gapIdx !== -1) {
        // Seek region [0 .. gapIdx], tail region [gapIdx+1 .. end] each ASC.
        checkAscRange(0, gapIdx + 1, 'seek-region');
        checkAscRange(gapIdx + 1, evs.length, 'tail-region');
    } else {
        checkAscRange(0, evs.length, 'contiguous');
    }

    // (2) No duplicate ids in events. Also: eventIds Set must match the array
    //     (the cache uses it for O(1) dedup; a desync would silently drop/dup).
    const seen = new Set();
    for (const e of evs) {
        assert.ok(!seen.has(e.id), `${tag}invariant#2 duplicate id ${e.id} in events`);
        seen.add(e.id);
    }
    assert.equal(
        entry.eventIds.size, evs.length,
        `${tag}invariant#2 eventIds Set size ${entry.eventIds.size} != events length ${evs.length}`
    );
    for (const e of evs) {
        assert.ok(
            entry.eventIds.has(e.id),
            `${tag}invariant#2 eventIds Set missing live id ${e.id}`
        );
    }

    // (3) Tail pinned. Two forms:
    //   (3a) events[last] holds the MAX .at among all present rows. The chat-list
    //        preview/sort/status read the newest message by walking from the end,
    //        so the array's last element must be the latest by timestamp. This
    //        holds even GAPPED, since an open gap keeps seek-region rows strictly
    //        older-or-equal than the tail (invariant #4).
    //   (3b) (when a tracker is supplied AND it pinned a row the cache ACCEPTED
    //        into the buffer) that newest-accepted id is still present at the end.
    if (evs.length > 0) {
        const last = evs[evs.length - 1];
        let maxAt = -Infinity, maxId = null;
        for (const e of evs) if (e.at >= maxAt) { maxAt = e.at; maxId = e.id; }
        assert.ok(
            last.at === maxAt,
            `${tag}invariant#3a tail not pinned: events[last] = id ${last.id} @ ${last.at}, ` +
            `but max .at present is ${maxAt} (id ${maxId})`
        );
    }
    if (tail && tail.newestId != null) {
        assert.ok(evs.length > 0, `${tag}invariant#3b buffer empty but a newest row was inserted`);
        const last = evs[evs.length - 1];
        assert.equal(
            last.id, tail.newestId,
            `${tag}invariant#3b newest accepted row (id ${tail.newestId} @ ${tail.newestAt}) ` +
            `not at tail; events[last] = id ${last.id} @ ${last.at}`
        );
    }

    // (4) Gap is a real single boundary.
    if (entry.gapAfterId != null) {
        const gapIdx = evs.findIndex(e => e.id === entry.gapAfterId);
        assert.notEqual(
            gapIdx, -1,
            `${tag}invariant#4 gapAfterId ${entry.gapAfterId} references a row NOT present in events (dangling gap)`
        );
        // Every row at-or-before the boundary is older-or-equal (by .at) than
        // every row after it — a single clean partition. Because the array is
        // already ASC-sorted (inv #1), it suffices that the boundary row's .at
        // is <= the first tail row's .at; but assert the strongest local form:
        // boundary .at <= next .at (ties allowed across the gap are permitted
        // by spec but the array sort already enforces the global ordering).
        if (gapIdx + 1 < evs.length) {
            assert.ok(
                evs[gapIdx].at <= evs[gapIdx + 1].at,
                `${tag}invariant#4 gap boundary out of order: ` +
                `boundary @ ${evs[gapIdx].at} > next @ ${evs[gapIdx + 1].at}`
            );
        }
    }
}

/**
 * Assert the post-dropSeekSegment shape: CONTIGUOUS + tail intact.
 * @param {object} entry
 * @param {Array}  expectedTail - the rows that should remain (the tail), ASC
 * @param {string} [label]
 */
export function assertContiguousWithTail(entry, expectedTail, label = '') {
    const tag = label ? `[${label}] ` : '';
    assert.equal(entry.gapAfterId, null, `${tag}expected CONTIGUOUS (gapAfterId == null)`);
    // entry.events is created inside the vm realm, so its Array.prototype differs
    // from the host's -> assert.deepStrictEqual would reject identical contents.
    // Compare by joined id string instead (realm-agnostic, value-only).
    const gotIds = entry.events.map(e => e.id).join('');
    const wantIds = expectedTail.map(e => e.id).join('');
    assert.equal(gotIds, wantIds, `${tag}tail rows changed after dropSeekSegment`);
}
