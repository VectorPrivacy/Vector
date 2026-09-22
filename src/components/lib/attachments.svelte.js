// Transfers, one record per id: uploads by the pending message's id, downloads by the
// attachment's id. The app's event listeners are the only writers; the attachment
// components read. Speed is smoothed here (an adaptive lerp paced by rAF) so a chunky
// backend rate reads as a steady number, and nothing else has to own a timer.
import { SvelteMap } from 'svelte/reactivity';

const map = new SvelteMap();   // id → { kind: 'upload' | 'download', phase: 'active' | 'failed', pct, bps, error }
const lerps = new Map();       // id → { display, target, factor, lastBytes, lastTime, raf }
const slowTimers = new Map();  // id → timeout, one per publish that outstays its welcome

export function transfer(id) { return map.get(id) ?? null; }

function patch(id, fields) {
    map.set(id, { kind: 'download', phase: 'active', pct: 0, bps: 0, error: '', ...(map.get(id) || {}), ...fields });
}

/** An upload chunk landed: `bytesSent` is cumulative, so the rate is its delta over time. */
export function uploadProgressed(pendingId, pct, bytesSent) {
    // 100% is the last byte leaving, not the send finishing: the server still has to
    // answer and the blob still has to be verified. It is already unstoppable by then,
    // so the phase flips here rather than waiting for the completion notice.
    if (pct >= 100) { markPublishing(pendingId); return; }
    patch(pendingId, { kind: 'upload', phase: 'active', pct });
    if (bytesSent == null) return;
    const now = performance.now();
    let st = lerps.get(pendingId);
    if (!st) {
        lerps.set(pendingId, { display: 0, target: 0, factor: 0.05, lastBytes: bytesSent, lastTime: now, raf: null });
        return;
    }
    const dtSec = (now - st.lastTime) / 1000;
    if (dtSec > 0.05) {
        const bps = (bytesSent - st.lastBytes) / dtSec;
        if (bps >= 0) st.target = bps;
        st.factor = Math.min(0.15, Math.max(0.008, 3.0 / (dtSec * 60)));
        st.lastBytes = bytesSent;
        st.lastTime = now;
    }
    if (!st.raf) st.raf = requestAnimationFrame(() => step(pendingId));
}

/** A download chunk landed with the backend's own rate. */
export function downloadProgressed(attachmentId, pct, bytesPerSec) {
    patch(attachmentId, { kind: 'download', phase: 'active', pct });
    if (pct >= 100) { stop(attachmentId); return; }
    if (!(bytesPerSec > 0)) return;
    const now = performance.now();
    let st = lerps.get(attachmentId);
    if (!st) {
        st = { display: bytesPerSec, target: bytesPerSec, factor: 0.05, lastBytes: 0, lastTime: now, raf: null };
        lerps.set(attachmentId, st);
    } else {
        // The factor is tuned so the animation spans about one chunk interval.
        const dtSec = (now - st.lastTime) / 1000;
        if (dtSec > 0.05) st.factor = Math.min(0.15, Math.max(0.008, 3.0 / (dtSec * 60)));
        st.target = bytesPerSec;
        st.lastTime = now;
    }
    if (!st.raf) st.raf = requestAnimationFrame(() => step(attachmentId));
}

function step(id) {
    const st = lerps.get(id);
    if (!st) return;
    st.display += (st.target - st.display) * st.factor;
    if (Math.abs(st.target - st.display) < 500) st.display = st.target;
    if (map.has(id)) patch(id, { bps: st.display });
    st.raf = st.display !== st.target ? requestAnimationFrame(() => step(id)) : null;
}
function stop(id) {
    const st = lerps.get(id);
    if (st?.raf) cancelAnimationFrame(st.raf);
    lerps.delete(id);
}

/** The bytes are on the server; only the Nostr publish is left. There is nothing to
 *  cancel from here, so every surface drops its button rather than offering one that
 *  would abandon the blob. */
function markPublishing(id) {
    const already = map.get(id)?.phase === 'publishing';
    stop(id);
    patch(id, { kind: 'upload', phase: 'publishing', pct: 100, bps: 0 });
    // A publish that lands promptly says nothing; only one that leaves a full ring on
    // screen earns a label, so the common case stays quiet.
    if (already || slowTimers.has(id)) return;
    slowTimers.set(id, setTimeout(() => {
        slowTimers.delete(id);
        if (map.get(id)?.phase === 'publishing') patch(id, { slow: true });
    }, 1000));
}

/** No guard on an existing record: an upload quick enough to finish without a progress
 *  frame has none, and that is exactly when the button must still go. */
export function uploadPublishing(pendingId) { markPublishing(pendingId); }

/** Whether a transfer is past the point of cancelling. */
export function transferPublishing(id) { return map.get(id)?.phase === 'publishing'; }

/** Whether it has sat in the publish long enough to be worth saying so. */
export function transferSlow(id) { return map.get(id)?.slow === true; }

/** The transfer ended well (or was cancelled): nothing is left to show. */
export function transferDone(id) {
    stop(id);
    clearTimeout(slowTimers.get(id));
    slowTimers.delete(id);
    map.delete(id);
}

export function uploadProgress(pendingId) { return map.get(pendingId)?.pct ?? null; }
export function downloadProgress(attachmentId) { return map.get(attachmentId)?.pct ?? null; }
