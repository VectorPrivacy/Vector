// The slim layout's drawer: the list pane pulled over the open chat, as far out as
// `--cc-drawer-p` on the root (0 away, 1 open), with `body.cc-drawer-on` while any of it
// shows. Eased between the two, or held to a finger. widescreen.js decides where it rests.
import { settler } from './railmotion.js';

// The channel left showing beside the open drawer (`--cc-peek`, which a chat with no
// drawer of its own sets to nothing).
const PEEK = 64;
document.documentElement.style.setProperty('--cc-peek', PEEK + 'px');

let p = 0;
let pTo = 0;
let pDone = null;
function paint(v) {
    p = v;
    document.documentElement.style.setProperty('--cc-drawer-p', String(v));
    document.body.classList.toggle('cc-drawer-on', v > 0);
    if (v === pTo && pDone) {
        const done = pDone;
        pDone = null;
        done();
    }
}
const settle = settler(paint);

/** Ease the drawer out or away, then `done`; `instant` puts it there, for a screen that opens with it. */
export function drawerTo(open, instant = false, done = null) {
    pTo = open ? 1 : 0;
    pDone = done;
    if (instant) {
        settle.stop();
        paint(pTo);
    } else {
        settle.to(p, pTo);
    }
}

// How far the open drawer has widened into the list (`--cc-home`, 0 the drawer, 1 the list),
// with `body.cc-homing` while it moves: entering a community and leaving one from its drawer.
let home = 0;
let homeTo = 0;
let homed = null;
function paintHome(v) {
    home = v;
    document.documentElement.style.setProperty('--cc-home', String(v));
    if (v !== homeTo) return;
    document.body.classList.remove('cc-homing');
    if (homed) {
        const done = homed;
        homed = null;
        done();
    }
}
const homeSettle = settler(paintHome);

/** Ease the drawer toward the list (1) or back (0), from `from` (or wherever it is), then `done`. */
export function drawerHome(to, { from = home, instant = false, done = null } = {}) {
    homeTo = to;
    homed = done;
    homeSettle.stop();
    if (instant || from === to) return paintHome(to);
    document.body.classList.add('cc-homing');
    paintHome(from);
    homeSettle.to(from, to);
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
            settle.stop();
            pDone = null;
            span = Math.max(1, window.innerWidth - (parseFloat(getComputedStyle(document.body).getPropertyValue('--cc-peek')) || 0));
            return p;
        },
        move: paint,
        grabs,
        // Only a pull that can move it: out while it's away, back while it's out.
        declines: (dx) => !live() || (p <= 0 && dx < 0) || (p >= 1 && dx > 0),
        end,
    };
}
