// The login screen's state: which screen is up (start, import, invite, welcome, encrypt,
// or none), whether it offers a way back, whether the form is shown at all, the bunker
// (remote signer) screen with its session and QR popup, the pre-login account picker, and
// the encrypt screen (title, which input is up, the biometric offers). The flows only set
// state; one component paints it.
import { flushSync } from 'svelte';

const BG_KEY = 'vector_login_hide_bg';
function readBgHidden() {
    try { return localStorage.getItem(BG_KEY) === '1'; } catch { return false; }
}

// No step until boot has read what is on disk: Start would flash up before an unlock step.
const l = $state({
    screen: 'none', backBar: false, shown: true, bunker: false,
    importKey: '', inviteCode: '',
    nip55Shown: false, nip55Busy: false,
    nip07Shown: false, nip07Busy: false,
    // The illustration behind the screens; a per-device preference, so it lives in the browser.
    bgHidden: readBgHidden(),
});
const b = $state({
    mode: 'new', url: '', qrReady: false, status: '', kind: '', copied: false, busy: false, urlInput: '',
    // A countdown to the single-use link's expiry; `now` ticks from the session timer.
    deadline: 0, now: 0,
    // The QR popup: the pairing link shows only while it is open.
    qrOpen: false,
    // Opened by the private key step's swap button, so its way back leads there.
    fromImport: false,
});
// The pill above the start / unlock screens, shown only when there is a choice to make.
const picker = $state({ shown: false, open: false, label: '', avatar: null, accounts: [], activeNpub: null });
const enc = $state({
    title: '', gradient: false, typing: false, error: false,
    // 'pin' or 'password' while the last unlock attempt was wrong: Aggroboi takes the title.
    wrong: '',
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
export function toggleLoginBg() {
    l.bgHidden = !l.bgHidden;
    try { localStorage.setItem(BG_KEY, l.bgHidden ? '1' : '0'); } catch { /* the choice lasts this session */ }
}
/** The main app is up: the form and every screen go. */
export function loginHide() { l.shown = false; l.screen = 'none'; }

/** The bunker overlay replaces the screens, with a way back. */
export function loginShowBunker(mode, fromImport = false) {
    b.mode = mode; b.fromImport = !!fromImport; l.screen = 'none'; l.bunker = true; l.backBar = true; l.shown = true;
    b.status = ''; b.kind = ''; b.url = ''; b.qrReady = false; b.copied = false; b.busy = false; b.deadline = 0; b.qrOpen = false;
}
export function loginHideBunker() {
    l.bunker = false; b.url = ''; b.qrReady = false; b.copied = false; b.busy = false; b.deadline = 0; b.urlInput = ''; b.qrOpen = false;
}
export function bunkerQrOpen(open) { b.qrOpen = !!open; }

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
