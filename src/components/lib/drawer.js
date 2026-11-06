// The slim layout's drawer: the list pane pulled over the open chat, as far out as
// `--cc-drawer-p` (0 away, 1 open), with `body.cc-drawer-on` while any of it shows. Eased
// between the two, or held to a finger. widescreen.js decides where it rests.
import { settleMs, SETTLE_EASING } from './railmotion.js';

// The channel left showing beside the open drawer (`--cc-peek`, which a chat with no
// drawer of its own sets to nothing).
const PEEK = 64;
// How far past its edge the drawer parks, so its shadow is off screen too (console.css).
const SHADOW = 32;
document.documentElement.style.setProperty('--cc-peek', PEEK + 'px');

// Set on the panes that read them, not the root: registered as not inherited
// (console.css), a frame restyles those panes alone, however much else is on the page.
// Each pane says how it reads each value: 'motion' when only its transform or opacity
// follows it, so an eased move runs on the compositor, where work on the page can't stall
// it; 'frame' when anything else does, so it's set every frame of the move.
const panes = new Map();
function put(name, key, v, how = null) {
    for (const [el, reads] of panes) {
        if (reads[key] && (!how || reads[key] === how)) el.style.setProperty(name, String(v));
    }
}

/** use:drawerPane={{ p, home }} on whatever reads `--cc-drawer-p` or `--cc-home`, each 'motion' or 'frame'. */
export function drawerPane(node, reads) {
    panes.set(node, reads);
    node.style.setProperty('--cc-drawer-p', String(pEase.get()));
    node.style.setProperty('--cc-home', String(homeEase.get()));
    return { update: (next) => panes.set(node, next), destroy: () => panes.delete(node) };
}

// The 'motion' panes' transform and opacity at `from` and at `to`, animated between; each is
// left holding `to`, so nothing jumps when its animation ends.
function glide(name, key, from, to, ms) {
    const anims = [];
    for (const [el, reads] of panes) {
        if (reads[key] !== 'motion') continue;
        const at = (v) => {
            el.style.setProperty(name, String(v));
            const cs = getComputedStyle(el);
            return { transform: cs.transform, opacity: cs.opacity };
        };
        const a = at(from), b = at(to);
        const k0 = {}, k1 = {};
        for (const prop of ['transform', 'opacity']) {
            if (a[prop] !== b[prop]) { k0[prop] = a[prop]; k1[prop] = b[prop]; }
        }
        if (Object.keys(k0).length) anims.push(el.animate([k0, k1], { duration: ms, easing: SETTLE_EASING }));
    }
    return anims;
}

/** One value eased between rests, or held to a finger; `land(v)` once it rests at `v`. Timed
 *  by its compositor animations when it has any: they start at the first frame drawn, which
 *  a busy page delays, and landing early would cut their last frames. */
function easer(name, key, land) {
    let value = 0, from = 0, to = 0, start = 0, ms = 0, frame = 0, run = 0, anims = [];
    function set(v) {
        value = v;
        put(name, key, v);
    }
    function progress() {
        const a = anims[0];
        if (a) return a.playState === 'finished' ? 1 : (a.effect.getComputedTiming().progress ?? 0);
        const t = Math.max(0, Math.min(1, (performance.now() - start) / ms));
        return 1 - (1 - t) ** 3;
    }
    function get() {
        return ms ? from + (to - from) * progress() : value;
    }
    function stop() {
        if (!ms) return;
        const v = get();
        ms = 0;
        run++;
        cancelAnimationFrame(frame);
        for (const a of anims) a.cancel();
        anims = [];
        set(v);
    }
    function go(a, b) {
        stop();
        set(a);
        ms = settleMs(a, b);
        if (!ms) {
            set(b);
            return land(b);
        }
        from = a;
        to = b;
        anims = glide(name, key, a, b, ms);
        start = performance.now();
        const mine = ++run;
        const arrive = () => {
            if (mine !== run) return;
            ms = 0;
            cancelAnimationFrame(frame);
            anims = [];
            set(b);
            land(b);
        };
        const framed = [...panes.values()].some((r) => r[key] === 'frame');
        const step = () => {
            if (!anims.length && performance.now() - start >= ms) return arrive();
            if (framed) put(name, key, get(), 'frame');
            frame = requestAnimationFrame(step);
        };
        if (anims.length) Promise.all(anims.map((x) => x.finished)).then(arrive, () => {});
        if (framed || !anims.length) frame = requestAnimationFrame(step);
    }
    return { go, stop, get, set: (v) => { stop(); set(v); } };
}

let pTo = 0;
let pDone = null;
let on = false;
const outListeners = new Set();
/** `fn()` each time the drawer starts to come out, from wherever. */
export function onDrawerOut(fn) { outListeners.add(fn); return () => outListeners.delete(fn); }
function showing(v) {
    if ((v > 0) === on) return;
    on = v > 0;
    document.body.classList.toggle('cc-drawer-on', on);
    if (on) for (const fn of outListeners) fn();
}
function landP(v) {
    document.body.classList.remove('cc-drawer-leaving');
    showing(v);
    if (v === pTo && pDone) {
        const done = pDone;
        pDone = null;
        done();
    }
}
const pEase = easer('--cc-drawer-p', 'p', landP);

/** Ease the drawer out or away, then `done`; `instant` puts it there, for a screen that opens with it. */
export function drawerTo(open, instant = false, done = null) {
    pTo = open ? 1 : 0;
    pDone = done;
    if (instant) {
        pEase.set(pTo);
        return landP(pTo);
    }
    const from = pEase.get();
    showing(Math.max(from, pTo));
    document.body.classList.toggle('cc-drawer-leaving', pTo === 0 && from > 0);
    pEase.go(from, pTo);
}

// How far the open drawer has widened into the list (`--cc-home`, 0 the drawer, 1 the list),
// with `body.cc-homing` while it moves: entering a community and leaving one from its drawer.
let homeTo = 0;
let homed = null;
function landHome(v) {
    if (v !== homeTo) return;
    document.body.classList.remove('cc-homing');
    if (homed) {
        const done = homed;
        homed = null;
        done();
    }
}
const homeEase = easer('--cc-home', 'home', landHome);

/** Ease the drawer toward the list (1) or back (0), from `from` (or wherever it is), then `done`. */
export function drawerHome(to, { from = homeEase.get(), instant = false, done = null } = {}) {
    homeTo = to;
    homed = done;
    if (instant || from === to) {
        homeEase.set(to);
        return landHome(to);
    }
    document.body.classList.add('cc-homing');
    homeEase.go(from, to);
}

/**
 * railPull options for whatever pulls the drawer. `live()`: whether there is a drawer;
 * `grabs(target)`: where a press may pull it; `end(open)`: where it was let go.
 */
export function drawerPull({ live, grabs, end }) {
    let span = 1;
    return {
        width: () => span,
        begin: () => {
            pEase.stop();
            document.body.classList.remove('cc-drawer-leaving');
            pDone = null;
            span = Math.max(1, window.innerWidth - (parseFloat(getComputedStyle(document.body).getPropertyValue('--cc-peek')) || 0) + SHADOW);
            return pEase.get();
        },
        move: (v) => {
            pEase.set(v);
            showing(v);
        },
        grabs,
        // Only a pull that can move it: out while it's away, back while it's out.
        declines: (dx) => {
            const p = pEase.get();
            return !live() || (p <= 0 && dx < 0) || (p >= 1 && dx > 0);
        },
        end,
    };
}
