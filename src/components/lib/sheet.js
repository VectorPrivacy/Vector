// The slim layout's details sheet: a community channel's details slid over the channel, which
// stays open beneath, as far away as `--cc-sheet-away` (0 covering it, 1 slid off to the right),
// set on the sheet itself (not inherited, as drawer.js's). Eased between the two, or held to a
// finger. widescreen.js decides where it rests.
import { settler } from './railmotion.js';

let away = 1;
let settled = null;
let sheet = null;

/** use:sheetPane on the sheet. */
export function sheetPane(node) {
    sheet = node;
    node.style.setProperty('--cc-sheet-away', String(away));
    return { destroy: () => { if (sheet === node) sheet = null; } };
}

function paint(v) {
    away = v;
    sheet?.style.setProperty('--cc-sheet-away', String(v));
    if (v === 1 && settled) {
        const done = settled;
        settled = null;
        done();
    }
}
const settle = settler(paint);

/** Slide the sheet in, or away and then `done`; `instant` puts it there at once. */
export function sheetTo(gone, { instant = false, done = null } = {}) {
    settled = gone ? done : null;
    if (instant) {
        settle.stop();
        paint(gone ? 1 : 0);
    } else {
        settle.to(away, gone ? 1 : 0);
    }
}

/** railPull options for the sheet: a pull to the right takes it away. `end(gone)`: where it was let go. */
export function sheetPull({ live, end }) {
    return {
        width: () => Math.max(1, window.innerWidth),
        begin: () => { settle.stop(); return away; },
        move: paint,
        declines: (dx) => !live() || (away <= 0 && dx < 0),
        end,
    };
}
