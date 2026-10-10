// Video players that outlive the card showing them. An iframe that changes parent reloads, and
// WebKit has no moveBefore, so a player never moves in the document: it sits at the end of the
// body and is laid over whichever box shows it (a chat card, the floating player) every frame,
// cut to the part of that box on screen.
//
// Each player has one owner, the host that last created or adopted it, as in videohost.js: a
// host tearing down drops its player only while it still owns it.

const frames = new Map();   // key -> { el, owner, place, shape, orphan }
// A player handed on stays out of sight only until the next host places it. One nobody places
// is stopped rather than left playing where no one can see or close it.
const ORPHAN_MS = 1500;
let seq = 0;
let raf = 0;

// What the player's frame may do: run, keep its own storage, open YouTube in a new window, go
// fullscreen. Never navigate Vector itself. The nested player inherits it.
const SANDBOX = 'allow-scripts allow-same-origin allow-popups allow-popups-to-escape-sandbox allow-presentation';

/** A new player loading `url`, owned by `owner`, out of sight until placed. */
export function createFrame(url, title, owner) {
    const key = `f${++seq}`;
    const el = document.createElement('iframe');
    // Set before it loads: a cross-origin isolated page (Vector Web) frames only credentialless.
    if (window.crossOriginIsolated) el.setAttribute('credentialless', '');
    el.setAttribute('sandbox', SANDBOX);
    el.allow = 'autoplay; encrypted-media; picture-in-picture; fullscreen';
    el.allowFullscreen = true;
    el.referrerPolicy = 'strict-origin-when-cross-origin';
    el.title = title || 'Video';
    el.className = 'player-frame';
    el.src = url;
    document.body.append(el);
    frames.set(key, { el, owner, place: null, shape: '' });
    return key;
}

/** Take over player `key` from whoever held it; false when it is gone. */
export function adoptFrame(key, owner) {
    const f = key ? frames.get(key) : null;
    if (!f) return false;
    f.owner = owner;
    return true;
}

export function ownsFrame(key, owner) {
    return !!key && frames.get(key)?.owner === owner;
}

/**
 * Lay player `key` over `place.box`. `place.clip` is the element whose bounds it may show in,
 * `place.holes()` the elements drawn over it there, `place.radius` its corners, and
 * `place.lifted` puts it above the floating player rather than under every menu.
 */
export function placeFrame(key, place) {
    const f = frames.get(key);
    if (!f) return;
    clearTimeout(f.orphan);
    f.place = place;
    f.el.classList.toggle('lifted', !!place.lifted);
    f.el.style.borderRadius = place.radius || '';
    layout(f);
    if (!raf) raf = requestAnimationFrame(tick);
}

/** Out of sight, still playing, until a host places it again; `onOrphan` runs if none does. */
export function unplaceFrame(key, onOrphan) {
    const f = frames.get(key);
    if (!f) return;
    f.place = null;
    f.el.style.visibility = 'hidden';
    clearTimeout(f.orphan);
    f.orphan = setTimeout(() => {
        if (frames.get(key) !== f || f.place) return;
        dropFrame(key);
        onOrphan?.();
    }, ORPHAN_MS);
}

/** Stop player `key` and forget it. */
export function dropFrame(key) {
    const f = frames.get(key);
    if (!f) return;
    frames.delete(key);
    clearTimeout(f.orphan);
    f.el.remove();
}

function tick() {
    raf = 0;
    let placed = false;
    for (const f of frames.values()) {
        if (!f.place) continue;
        placed = true;
        layout(f);
    }
    if (placed) raf = requestAnimationFrame(tick);
}

const rectOf = (el) => el?.isConnected ? el.getBoundingClientRect() : null;

function layout(f) {
    const { el, place } = f;
    const r = rectOf(place.box);
    if (!r || !r.width || !r.height) { el.style.visibility = 'hidden'; return; }
    const c = rectOf(place.clip);
    // The part of the box on screen, in the box's own coordinates.
    const top = c ? Math.max(0, c.top - r.top) : 0;
    const left = c ? Math.max(0, c.left - r.left) : 0;
    const bottom = c ? Math.min(r.height, c.bottom - r.top) : r.height;
    const right = c ? Math.min(r.width, c.right - r.left) : r.width;
    if (bottom <= top || right <= left) { el.style.visibility = 'hidden'; return; }
    let path = '';
    if (top > 0 || left > 0 || bottom < r.height || right < r.width) path += box(left, top, right, bottom);
    for (const hole of place.holes?.() || []) {
        const h = rectOf(hole);
        if (!h || !h.width || !h.height) continue;
        const ht = Math.max(top, h.top - r.top), hl = Math.max(left, h.left - r.left);
        const hb = Math.min(bottom, h.bottom - r.top), hr = Math.min(right, h.right - r.left);
        if (hb <= ht || hr <= hl) continue;
        if (!path) path = box(0, 0, r.width, r.height);
        path += box(hl, ht, hr, hb);
    }
    // Written only on change: a style write per frame would restyle the frame for nothing.
    const shape = `${r.left},${r.top},${r.width},${r.height}|${path}`;
    if (shape === f.shape && el.style.visibility === 'visible') return;
    f.shape = shape;
    el.style.transform = `translate(${r.left}px, ${r.top}px)`;
    el.style.width = `${r.width}px`;
    el.style.height = `${r.height}px`;
    // Holes cut by even-odd: what lies over the player inside its box shows through.
    el.style.clipPath = path ? `path(evenodd, "${path}")` : '';
    el.style.visibility = 'visible';
}

const box = (l, t, r, b) => `M${l} ${t}H${r}V${b}H${l}Z`;
