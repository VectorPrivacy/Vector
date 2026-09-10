// The two dialog lifecycles, as factories over a `$state` view.
//
// `fadeDialog`: `open` mounts the element, `active` drives the fade; `close` drops `active`
// and unmounts after the fade. Handlers are fixed at mount.
export function fadeDialog(fields) {
    const s = $state({ open: false, active: false, ...fields });
    return {
        state: () => s,
        /** Show with these fields; re-opening while open just repaints. */
        open(view) {
            Object.assign(s, view);
            if (s.open) { s.active = true; return; }
            s.open = true;
            s.active = false;
            setTimeout(() => { if (s.open) s.active = true; }, 10);
        },
        patch(view) { Object.assign(s, view); },
        close() {
            s.active = false;
            setTimeout(() => { if (!s.active) s.open = false; }, 300);
        },
    };
}


// `popOverlay`: display flips with `active`, the card replays its pop animation when `tick`
// moves (the component adds the class after the overlay has rendered, because WebKit never
// starts an animation declared on a subtree emerging from display:none), and `closing` runs
// the mirrored pop-out before the overlay actually hides.
export function popOverlay(fields) {
    const s = $state({ active: false, closing: false, tick: 0, ...fields });
    let timer = null;
    return {
        state: () => s,
        open(view) {
            clearTimeout(timer);
            Object.assign(s, view);
            s.closing = false;
            s.active = true;
            s.tick++;
        },
        patch(view) { Object.assign(s, view); },
        /** True when a close is already running; the caller then does nothing. */
        closing: () => s.closing,
        close() {
            if (s.closing || !s.active) return;
            s.closing = true;
            timer = setTimeout(() => { s.active = false; s.closing = false; }, 160);
        },
    };
}

