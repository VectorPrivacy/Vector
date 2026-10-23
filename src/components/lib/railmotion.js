// The slim rail's open amount as it moves, 0 folded away to 1 open: eased between the
// two, or held to a finger while it is pulled.

const SETTLE_MS = 280;
// A release faster than this (px/ms) goes the way it was thrown, wherever it stopped.
const FLING = 0.35;
// Movement before a press becomes a pull or a scroll.
const SLOP = 6;

/** Eases a value toward a target frame by frame; a newer `to` takes over. */
export function settler(set) {
    let frame = 0;
    return {
        to(from, to) {
            cancelAnimationFrame(frame);
            const instant = from === to || matchMedia('(prefers-reduced-motion: reduce)').matches;
            if (instant) { set(to); return; }
            // A shorter way home takes a shorter time, so a nearly-shut rail doesn't crawl.
            const ms = SETTLE_MS * Math.min(1, 0.35 + Math.abs(to - from));
            const start = performance.now();
            const step = (now) => {
                // A frame's timestamp can precede `start`; below 0 the curve overshoots.
                const t = Math.max(0, Math.min(1, (now - start) / ms));
                set(from + (to - from) * (1 - (1 - t) ** 3));
                if (t < 1) frame = requestAnimationFrame(step);
            };
            frame = requestAnimationFrame(step);
        },
        stop() { cancelAnimationFrame(frame); },
    };
}

// A trackpad's two-finger swipe arrives as wheel events with no end: this long without one
// (its momentum included) and it has let go.
const WHEEL_IDLE_MS = 140;
// One swipe at a time, whichever strip it began on: the rail slides out from under a
// resting pointer, so the rest of the swipe lands elsewhere and is followed there.
let swipe = null;
// Swipes that start just after a vertical scroll or another swipe are that one's tail.
let swipeQuietUntil = 0;

function swipeStep(e) {
    // Content moves against the fingers, so the rail follows -deltaX.
    swipe.dx -= e.deltaMode === 1 ? e.deltaX * 16 : e.deltaX;
    swipe.samples.push([e.timeStamp, swipe.dx]);
    while (swipe.samples.length > 2 && e.timeStamp - swipe.samples[0][0] > 100) swipe.samples.shift();
    swipe.last = Math.max(0, Math.min(1, swipe.p0 + swipe.dx / swipe.opts.width()));
    swipe.opts.move(swipe.last);
    clearTimeout(swipe.timer);
    swipe.timer = setTimeout(swipeEnd, WHEEL_IDLE_MS);
}

function swipeFollow(e) {
    if (!swipe) return;
    e.preventDefault();
    e.stopPropagation();
    swipeStep(e);
}

function swipeEnd() {
    const s = swipe;
    if (!s) return;
    swipe = null;
    clearTimeout(s.timer);
    window.removeEventListener('wheel', swipeFollow, { capture: true });
    swipeQuietUntil = performance.now() + 250;
    const [t0, d0] = s.samples[0];
    const [t1, d1] = s.samples[s.samples.length - 1];
    const v = (d1 - d0) / Math.max(1, t1 - t0);
    s.opts.end(v > FLING ? true : v < -FLING ? false : s.last > 0.5);
}

/**
 * Pull the rail by this element, sideways only: a press that heads up or down is left to
 * scroll. `o`: { width() px of a full pull, begin() → the amount it starts from,
 * move(amount), end(open), grabs?(target) → false where a press is never a pull }.
 * A release decides by its speed, else by how far it got. A
 * pull whose release never arrives (let go outside the window) ends at the next press,
 * a lost capture or the window losing focus, so it can't hold the rail mid-way. A
 * trackpad's sideways two-finger swipe over it pulls by the same rules.
 */
export function railPull(node, o) {
    let opts = o;
    let id = null;
    let pulling = false;
    // A press `grabs` turns down: followed only to see whether it goes sideways.
    let refused = false;
    let stray = false;
    let x0 = 0, y0 = 0, p0 = 0, last = 0;
    let axis = null;
    let samples = [];
    let swallowUntil = 0;

    const clamp = (p) => Math.max(0, Math.min(1, p));
    const amount = (x) => clamp(p0 + (x - x0) / opts.width());

    function finish(open) {
        pulling = false;
        id = null;
        // The click a pull ends on is the pull's, not a tap on what's under it.
        swallowUntil = performance.now() + 350;
        opts.end(open);
    }
    function abandon() {
        if (pulling) finish(last > 0.5);
    }

    function down(e) {
        abandon();
        if (e.pointerType === 'mouse' && e.button !== 0) return;
        refused = !!opts.grabs && !opts.grabs(e.target);
        stray = false;
        id = e.pointerId;
        x0 = e.clientX;
        y0 = e.clientY;
        axis = null;
        samples = [[e.timeStamp, e.clientX]];
    }
    function move(e) {
        if (e.pointerId !== id) return;
        if (!axis) {
            const dx = e.clientX - x0, dy = e.clientY - y0;
            if (Math.hypot(dx, dy) < SLOP) return;
            axis = Math.abs(dx) > Math.abs(dy) ? 'x' : 'y';
            if (axis === 'y') { id = null; return; }
            if (refused) { stray = true; return; }
            p0 = last = opts.begin();
            pulling = true;
            try { node.setPointerCapture(id); } catch { /* the pointer is gone: the moves stay ours */ }
        }
        if (stray) return;
        e.preventDefault();
        samples.push([e.timeStamp, e.clientX]);
        while (samples.length > 2 && e.timeStamp - samples[0][0] > 100) samples.shift();
        last = amount(e.clientX);
        opts.move(last);
    }
    function up(e) {
        if (e.pointerId !== id) return;
        // A swipe across a tile is neither a pull nor a tap on it.
        if (stray) swallowUntil = performance.now() + 350;
        if (!pulling) { id = null; return; }
        const [t0, sx] = samples[0];
        const v = (e.clientX - sx) / Math.max(1, e.timeStamp - t0);
        last = amount(e.clientX);
        finish(v > FLING ? true : v < -FLING ? false : last > 0.5);
    }
    function cancel(e) {
        if (e.pointerId !== id) return;
        if (pulling) abandon();
        else id = null;
    }
    // Once sideways, the touch stays a pull: a scroll starting mid-way would cancel it.
    function hold(e) {
        if (pulling) e.preventDefault();
    }
    function lost(e) {
        // Only the strip's own capture: a touch's implicit capture on what it landed on
        // is released, and bubbles here, the moment the pull takes the pointer.
        if (e.target === node && e.pointerId === id) abandon();
    }
    function swallow(e) {
        if (performance.now() < swallowUntil) { e.stopPropagation(); e.preventDefault(); }
    }
    // A two-finger swipe: mostly sideways, not a pinch (ctrlKey), not the tail of a scroll.
    function wheel(e) {
        if (swipe || pulling || e.ctrlKey) return;
        if (Math.abs(e.deltaX) <= Math.abs(e.deltaY)) {
            if (e.deltaY) swipeQuietUntil = performance.now() + WHEEL_IDLE_MS;
            return;
        }
        if (performance.now() < swipeQuietUntil || (opts.grabs && !opts.grabs(e.target))) return;
        e.preventDefault();
        swipe = { owner: node, opts, p0: opts.begin(), last: 0, dx: 0, samples: [], timer: 0 };
        swipe.last = swipe.p0;
        window.addEventListener('wheel', swipeFollow, { capture: true, passive: false });
        swipeStep(e);
    }

    node.addEventListener('pointerdown', down);
    node.addEventListener('pointermove', move);
    node.addEventListener('pointerup', up);
    node.addEventListener('pointercancel', cancel);
    node.addEventListener('lostpointercapture', lost);
    node.addEventListener('touchmove', hold, { passive: false });
    node.addEventListener('click', swallow, true);
    node.addEventListener('wheel', wheel, { passive: false });
    window.addEventListener('blur', abandon);
    return {
        update(next) { opts = next; },
        destroy() {
            abandon();
            node.removeEventListener('pointerdown', down);
            node.removeEventListener('pointermove', move);
            node.removeEventListener('pointerup', up);
            node.removeEventListener('pointercancel', cancel);
            node.removeEventListener('lostpointercapture', lost);
            node.removeEventListener('touchmove', hold);
            node.removeEventListener('click', swallow, true);
            node.removeEventListener('wheel', wheel);
            window.removeEventListener('blur', abandon);
            if (swipe?.owner === node) swipeEnd();
        },
    };
}
