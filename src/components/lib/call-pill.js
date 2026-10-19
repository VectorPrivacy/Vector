// Where a call's pill sits: dragged anywhere and remembered per device. A 1:1 call and a voice
// Channel's call never run together, so both pills share the one place.
const POS_KEY = 'call_pill_pos';

export function loadPillPos() {
    try {
        const p = JSON.parse(localStorage.getItem(POS_KEY));
        if (p && Number.isFinite(p.x) && Number.isFinite(p.y)) return p;
    } catch (_) {}
    return null;
}

export function savePillPos(pos) {
    try { localStorage.setItem(POS_KEY, JSON.stringify(pos)); } catch (_) {}
}

// The window chrome's strip is off limits: the pill must never cover its buttons.
function chromeHeight() {
    const v = parseFloat(getComputedStyle(document.body).getPropertyValue('--chrome-h'));
    return Number.isFinite(v) ? v : 0;
}

/** `p` kept on screen for a pill `w` by `h`; the same object when nothing moved, so a re-clamp
 *  never counts as a change. */
export function clampPill(p, w, h) {
    const top = chromeHeight() + 8;
    const x = Math.min(Math.max(8, p.x), Math.max(8, window.innerWidth - w - 8));
    const y = Math.min(Math.max(top, p.y), Math.max(top, window.innerHeight - h - 8));
    return x === p.x && y === p.y ? p : { x, y };
}

/** The header row's pointer handlers: past a few pixels of slop it drags the pill, short of
 *  them it's a tap. `pill` gives the element; `move` gets each wanted top-left, `drop` the end of
 *  a drag, `tap` a tap. */
export function pillDrag(pill, move, drop, tap) {
    let drag = null;
    return {
        down(e) {
            if (e.button !== 0 || e.target.closest('button, input, a')) return;
            const r = pill().getBoundingClientRect();
            drag = { sx: e.clientX, sy: e.clientY, ox: r.left, oy: r.top, moved: false };
            // Capture on the row that listens, so its own pointerup ends the drag.
            e.currentTarget.setPointerCapture(e.pointerId);
        },
        move(e) {
            if (!drag) return;
            const dx = e.clientX - drag.sx;
            const dy = e.clientY - drag.sy;
            if (!drag.moved && Math.hypot(dx, dy) < 4) return;
            drag.moved = true;
            move({ x: drag.ox + dx, y: drag.oy + dy });
        },
        up() {
            if (!drag) return;
            const moved = drag.moved;
            drag = null;
            if (moved) drop();
            else tap();
        },
    };
}
