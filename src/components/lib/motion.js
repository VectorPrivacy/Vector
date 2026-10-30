// Animated pictures move only where they are seen: the window active and the picture on screen.
// A face or community icon also waits for its row to be hovered (`data-motion-host`), and so does
// everything in a row marked `data-motion="hover"` (people and chat lists: names, statuses,
// previews). `"touch"` is a hover row that moves outright where nothing can hover; `"live"` is an
// open view, where everything moves. Any other moment shows a still: a face's thumb, or a first
// frame from `stillOf`, and a CSS loop holds its frame. An <img> is painted over with
// `content: url()`, so its own src, load events and layout stay the app's.

let stillOf = null;
let active = true;
let hovered = null;
let sheet = null;
let io = null;
const stills = new Map();
const frames = new Map();
const recs = new WeakMap();
const tracked = new Set();

/**
 * @typedef {object} Frames
 * @property {string} still   what shows while the picture may not move
 * @property {string} moving  what shows while it may
 * @property {boolean} face   an avatar or community icon
 */

/**
 * Watch every <img> in the document from now on. `stillOf` answers null for a picture that
 * doesn't animate.
 * @param {{ stillOf: (src: string) => Promise<Frames|null> }} opts
 */
export function startMotion(opts) {
    stillOf = opts.stillOf;
    sheet = document.head.appendChild(document.createElement('style')).sheet;
    io = new IntersectionObserver(onSeen, { threshold: [0, 0.0001] });
    new MutationObserver(onMutations).observe(document.documentElement, { subtree: true, childList: true, attributes: true, attributeFilter: ['src'] });
    document.querySelectorAll('img').forEach(track);
    document.addEventListener('pointerover', onPointer, { passive: true });
    document.documentElement.addEventListener('pointerleave', () => hover(null), { passive: true });
}

/** The window is active (focused and visible): only then does anything move. */
export function setMotionActive(on) {
    if (active === !!on) return;
    active = !!on;
    if (!active) hovered = null;
    for (const el of tracked) apply(el);
    loops(active);
}

const held = new WeakSet();

// checkVisibility arrived in Safari 17.4; an older WebKit only knows whether it's laid out.
const shown = (el) => (el.checkVisibility ? el.checkVisibility({ opacityProperty: true, visibilityProperty: true }) : el.offsetParent !== null);

/** CSS loops (pulses, spinners) and muted video loops (GIFs) hold their frame while the window is inactive. */
function loops(on) {
    for (const a of document.getAnimations()) {
        if (on && held.has(a)) {
            held.delete(a);
            if (a.playState === 'paused') a.play();
        } else if (!on && a.playState === 'running' && a.effect?.getTiming().iterations === Infinity) {
            held.add(a);
            a.pause();
        }
    }
    for (const v of document.querySelectorAll('video[loop]')) {
        if (on && held.has(v)) {
            held.delete(v);
            if (v.paused && shown(v)) v.play().catch(() => {});
        } else if (!on && v.muted && !v.paused) {
            held.add(v);
            v.pause();
        }
    }
}

/**
 * For an element that shows `src` some other way than an <img>, a background: `paint` gets the
 * src to show now. A face (an avatar or community icon) moves only while live or hovered.
 * @param {HTMLElement} el
 * @param {{ src: string|null, paint: (src: string|null) => void }} params
 */
export function moving(el, params) {
    const r = { paint: params.paint };
    recs.set(el, r);
    watch(el, r, params.src);
    return {
        update(next) { r.paint = next.paint; if (next.src !== r.base) watch(el, r, next.src); },
        destroy() { untrack(el); },
    };
}

function track(img) {
    const src = img.getAttribute('src') ? img.src : '';
    let r = recs.get(img);
    if (r?.base === src) return;
    if (!r) {
        r = {};
        recs.set(img, r);
    }
    watch(img, r, src);
}

function watch(el, r, src) {
    io?.unobserve(el);
    tracked.delete(el);
    r.seen = false;
    r.base = src;
    r.still = r.moving = null;
    r.face = false;
    paint(el, r, null);
    if (!src || src.startsWith('data:')) return;
    lookup(src).then((frames) => {
        if (recs.get(el) !== r || r.base !== src || !frames) return;
        Object.assign(r, frames);
        tracked.add(el);
        io.observe(el);
        apply(el);
    });
}

function untrack(el) {
    io?.unobserve(el);
    tracked.delete(el);
    recs.delete(el);
}

function lookup(src) {
    let found = stills.get(src);
    if (!found) {
        const ours = !isAsset(src) && stillOf ? stillOf(src) : null;
        found = Promise.resolve(ours ?? assetStill(src)).catch(() => {
            stills.delete(src);
            return null;
        });
        stills.set(src, found);
    }
    return found;
}

function isAsset(src) {
    return src.startsWith(location.href.replace(/[?#].*$/, '').replace(/[^/]*$/, '')) && !/\/(vfs|cache)\//.test(src);
}

/** The app's own GIFs: their first frame, drawn once. */
async function assetStill(src) {
    if (!isAsset(src) || !/\.gif([?#]|$)/i.test(src)) return null;
    const img = new Image();
    img.src = src;
    await img.decode();
    const canvas = document.createElement('canvas');
    canvas.width = img.naturalWidth;
    canvas.height = img.naturalHeight;
    canvas.getContext('2d').drawImage(img, 0, 0);
    const blob = await new Promise((done) => canvas.toBlob(done, 'image/png'));
    return blob && { still: URL.createObjectURL(blob), moving: src, face: false };
}

function apply(el) {
    const r = recs.get(el);
    if (!r?.still) return;
    const move = active && r.seen && free(el, r);
    const show = move ? r.moving : r.still;
    paint(el, r, show === r.base ? null : show);
}

const NO_HOVER = matchMedia('(hover: none)');
const HOST = '[data-motion-host], [data-motion="hover"], [data-motion="touch"]';

function free(el, r) {
    const mark = el.closest('[data-motion]')?.dataset.motion;
    if (mark === 'live' || (mark === 'touch' && NO_HOVER.matches)) return true;
    if (!r.face && mark !== 'hover' && mark !== 'touch') return true;
    return !!hovered?.contains(el);
}

/** Paint `src` over `el`'s own, or (null) let its own show. */
function paint(el, r, src) {
    if (r.paint) {
        if (r.shown !== (src ?? r.base)) r.paint(r.shown = src ?? r.base);
        return;
    }
    if (!src) {
        if (el.dataset.frame) delete el.dataset.frame;
        return;
    }
    let id = frames.get(src);
    if (id == null) {
        id = String(frames.size);
        frames.set(src, id);
        sheet.insertRule(`img[data-frame="${id}"]{content:url("${src.replace(/["\\]/g, '\\$&')}")!important}`, sheet.cssRules.length);
    }
    if (el.dataset.frame !== id) el.dataset.frame = id;
}

function onSeen(entries) {
    for (const e of entries) {
        const r = recs.get(e.target);
        if (!r) continue;
        r.seen = e.isIntersecting && e.intersectionRect.width > 0 && e.intersectionRect.height > 0;
        apply(e.target);
    }
}

function onMutations(list) {
    for (const m of list) {
        if (m.type === 'attributes') {
            if (m.target.tagName === 'IMG' && m.target.isConnected) track(m.target);
            continue;
        }
        for (const n of m.removedNodes) if (n.nodeType === 1) each(n, untrackImg);
        for (const n of m.addedNodes) if (n.nodeType === 1 && n.isConnected) each(n, track);
    }
}

function untrackImg(img) {
    if (!img.isConnected) untrack(img);
}

function each(node, fn) {
    if (node.tagName === 'IMG') fn(node);
    else if (node.firstElementChild) node.querySelectorAll('img').forEach(fn);
}

function onPointer(e) {
    if (e.pointerType === 'touch') return;
    const t = e.target;
    hover(t.closest?.(HOST) ?? (t.tagName === 'IMG' ? t : null));
}

function hover(host) {
    if (host === hovered) return;
    const was = hovered;
    hovered = active ? host : null;
    if (was) within(was);
    if (hovered) within(hovered);
}

function within(host) {
    if (recs.has(host)) apply(host);
    for (const img of host.querySelectorAll('img')) if (recs.has(img)) apply(img);
}
