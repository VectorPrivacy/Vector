// The login shell's state: which screen is up (start, import, invite, welcome, encrypt,
// or none), the back bar, whether the form is shown at all, and the bunker (remote
// signer) overlay with its session. One reconciler paints it; the flows only set state.
const l = $state({ screen: 'start', backBar: false, shown: true, bunker: false });
const b = $state({
    mode: 'new', url: '', qrReady: false, status: '', kind: '', copied: false, busy: false,
    // A countdown to the single-use link's expiry; `now` ticks from the session timer.
    deadline: 0, now: 0,
});

export function loginState() { return l; }
export function bunkerState() { return b; }

/** Show one screen; `backBar` unchanged when omitted. */
export function loginScreen(screen, backBar) {
    l.screen = screen;
    if (backBar !== undefined) l.backBar = !!backBar;
}
export function loginShowForm(shown) { l.shown = !!shown; }
/** The main app is up: the form and every screen go. */
export function loginHide() { l.shown = false; l.screen = 'none'; }

/** The bunker overlay replaces the screens; the back bar shows above it. */
export function loginShowBunker(mode) {
    b.mode = mode; l.screen = 'none'; l.bunker = true; l.backBar = true; l.shown = true;
    b.status = ''; b.kind = ''; b.url = ''; b.qrReady = false; b.copied = false; b.busy = false; b.deadline = 0;
}
export function loginHideBunker() {
    l.bunker = false; b.url = ''; b.qrReady = false; b.copied = false; b.busy = false; b.deadline = 0;
}

export function bunkerStatus(text, kind = '') { b.status = text || ''; b.kind = kind || ''; }
export function bunkerLink(url, qrReady) { b.url = url || ''; b.qrReady = !!qrReady; }
export function bunkerCopied(on) { b.copied = !!on; }
export function bunkerBusy(on) { b.busy = !!on; }
export function bunkerDeadline(at) { b.deadline = at || 0; b.now = Date.now(); }
export function bunkerTick(now) { b.now = now; }
