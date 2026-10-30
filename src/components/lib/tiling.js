// WebKit orphans a scroller's content layer, backing store and tiles, every time that layer swaps
// between one backing and tiles: its content crossing 2048px either way. Nothing frees it until
// the process ends. A layer built fresh never leaks, so at a crossing the scroller gives up its
// scrolling layers for one frame and the next frame builds new ones at the new size.

const EDGE = 2048;
const SLACK = 16;
// Cocoa WebKit only (every iOS browser, a Mac's that isn't Chromium or Gecko).
const UA = navigator.userAgent;
const LEAKS = /iPhone|iPad|iPod/.test(UA) || (/Macintosh/.test(UA) && !/Chrome|Firefox|Edg/.test(UA));

const crosses = (a, b) => a !== b && Math.min(a, b) <= EDGE + SLACK && Math.max(a, b) >= EDGE - SLACK;

/**
 * Watch `el` for that crossing: the box and its rows for size, the list for rows coming and
 * going. `changed` runs whenever the content's size moved, after layout and before paint.
 * @param {HTMLElement} el
 * @param {() => void} [changed]
 * @returns {() => void} stop watching
 */
export function guardTiling(el, changed) {
    let w = el.scrollWidth;
    let h = el.scrollHeight;
    let raf = 0;
    let held = 0;
    const check = () => {
        raf = 0;
        const nw = el.scrollWidth;
        const nh = el.scrollHeight;
        if (nw === w && nh === h) return;
        if (LEAKS && !held && (crosses(w, nw) || crosses(h, nh))) held = hold(el, () => { held = 0; });
        w = nw;
        h = nh;
        changed?.();
    };
    const sizes = new ResizeObserver(check);
    const rows = new MutationObserver((records) => {
        for (const r of records) {
            for (const n of r.addedNodes) if (n.nodeType === 1) sizes.observe(n, { box: 'border-box' });
            for (const n of r.removedNodes) if (n.nodeType === 1) sizes.unobserve(n);
        }
        // A removed row resizes nothing still watched: check on the next frame.
        if (!raf) raf = requestAnimationFrame(check);
    });
    sizes.observe(el);
    for (const n of el.children) sizes.observe(n, { box: 'border-box' });
    rows.observe(el, { childList: true });
    return () => {
        sizes.disconnect();
        rows.disconnect();
        cancelAnimationFrame(raf);
    };
}

/** Svelte action: `use:tiling` on a scroller whose content grows and shrinks. */
export function tiling(node) {
    if (LEAKS) return { destroy: guardTiling(node) };
}

// Hidden overflow has no scrolling layers. A layout scrollbar's width moves into the padding,
// so the rows keep their width and the frame paints as before.
function hold(el, done) {
    const s = el.style;
    const cs = getComputedStyle(el);
    const bar = el.offsetWidth - el.clientWidth - parseFloat(cs.borderLeftWidth) - parseFloat(cs.borderRightWidth);
    const kept = ['overflow-x', 'overflow-y', 'padding-right'].map((p) => [p, s.getPropertyValue(p), s.getPropertyPriority(p)]);
    if (bar > 0) s.setProperty('padding-right', `${parseFloat(cs.paddingRight) + bar}px`, 'important');
    s.setProperty('overflow-x', 'hidden', 'important');
    s.setProperty('overflow-y', 'hidden', 'important');
    return requestAnimationFrame(() => {
        for (const [p, v, pri] of kept) {
            if (v) s.setProperty(p, v, pri);
            else s.removeProperty(p);
        }
        done();
    });
}
