// The login screen's state: which screen is up (start, import, invite, welcome, encrypt,
// or none), the back bar, whether the form is shown at all, the bunker (remote signer)
// overlay with its session, the pre-login account picker, and the encrypt screen (title,
// which input is up, the biometric offers). The flows only set state; one component paints it.
import { flushSync } from 'svelte';

const l = $state({
    screen: 'start', backBar: false, shown: true, bunker: false,
    importKey: '', inviteCode: '',
    nip55Shown: false, nip55Busy: false,
});
const b = $state({
    mode: 'new', url: '', qrReady: false, status: '', kind: '', copied: false, busy: false, urlInput: '',
    // A countdown to the single-use link's expiry; `now` ticks from the session timer.
    deadline: 0, now: 0,
});
// The pill above the start / unlock screens, shown only when there is a choice to make.
const picker = $state({ shown: false, open: false, label: '', avatar: null, accounts: [], activeNpub: null });
const enc = $state({
    title: '', gradient: false, typing: false, error: false,
    headerShown: true, lockShown: true,
    typeSelectShown: false, pinShown: false, passwordShown: false, password: '',
    bioOptionShown: false, bioOptionLabel: 'Use Biometrics', recommended: '*Recommended Option',
    bioBtnShown: false, bioBtnLabel: 'Unlock with Biometrics',
    // Bumped to clear the PIN boxes (`pinFocus` says whether the first one takes focus)
    // and to focus whichever input is up.
    pinTick: 0, pinFocus: true, focusTick: 0,
});

export function loginState() { return l; }
export function bunkerState() { return b; }
export function pickerState() { return picker; }
export function encryptState() { return enc; }

export function patchLogin(patch) { Object.assign(l, patch); }
export function patchBunker(patch) { Object.assign(b, patch); }
export function patchPicker(patch) { Object.assign(picker, patch); }
export function patchEncrypt(patch) { Object.assign(enc, patch); }

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
    l.bunker = false; b.url = ''; b.qrReady = false; b.copied = false; b.busy = false; b.deadline = 0; b.urlInput = '';
}

export function bunkerStatus(text, kind = '') { b.status = text || ''; b.kind = kind || ''; }
export function bunkerLink(url) { b.url = url || ''; if (!url) b.qrReady = false; }
export function bunkerCopied(on) { b.copied = !!on; }
export function bunkerBusy(on) { b.busy = !!on; }
export function bunkerDeadline(at) { b.deadline = at || 0; b.now = Date.now(); }
export function bunkerTick(now) { b.now = now; }

/** Empty the PIN boxes; the first takes focus when asked. */
export function resetLoginPin(focusFirst = true) { enc.pinFocus = focusFirst; enc.pinTick++; flushSync(); }
/** Focus whichever encrypt input is up. Synchronous, so a flow can call it and move on. */
export function focusLoginInput() { enc.focusTick++; flushSync(); }
