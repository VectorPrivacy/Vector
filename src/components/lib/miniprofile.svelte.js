// The mini profile popup: who it shows, where it sits, and whether a fetch for an
// identity relays have nothing for has been given up on (then it reads "Anon").
let mini = $state.raw({ npub: null, anchor: null, reuse: null, settled: false });

export function miniProfile() { return mini; }
/** Open for `npub`. `anchor` positions it (null = centred); `reuse` = { left, top } | { centered } keeps a replaced popup's spot. */
export function openMiniProfile(npub, anchor, reuse) {
    mini = { npub, anchor: anchor || null, reuse: reuse || null, settled: false };
}
export function settleMiniProfile(npub) {
    if (mini.npub === npub) mini = { ...mini, settled: true };
}
export function closeMiniProfile() {
    if (mini.npub) mini = { npub: null, anchor: null, reuse: null, settled: false };
}

// The popup element while open, for the dismiss paths' hit tests.
const els = $state.raw({ popup: null });
export function miniProfileEls() { return els; }
export function bindMiniProfileEl(node) { els.popup = node; return { destroy() { els.popup = null; } }; }
