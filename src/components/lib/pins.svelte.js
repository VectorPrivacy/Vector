// The open channel's pinned messages as the drawer shows them. The app resolves the
// context and fetches; the drawer derives rows, notices and affordances. `open` is the
// logical state, `shown` keeps the drawer in the layout through its slide-up.
const p = $state({ open: false, shown: false, closing: false, button: false, sealed: false, canPin: false, communityId: null, channelId: null, v: 0 });
let pins = $state.raw([]);   // verified pins, `_jumpable` resolved by the app
let handlers = null;         // the row helpers, registered once by pins.js
const els = { drawer: null, list: null, button: null };   // for hit-tests and scroll rides

export function pinsState() { return p; }
export function pinsList() { return pins; }
export function pinsHandlers() { return handlers; }
export function setPinsHandlers(h) { handlers = h; }
export function pinsEls() { return els; }
export function setPins({ pins: list, sealed, canPin, communityId, channelId }) {
    pins = list || [];
    p.sealed = !!sealed;
    p.canPin = !!canPin;
    p.communityId = communityId || null;
    p.channelId = channelId || null;
    p.v++;
}
export function setPinsButtonVisible(on) { p.button = !!on; }
export function setPinsOpen(open, instant = false) {
    p.open = !!open;
    if (open) { p.shown = true; p.closing = false; return; }
    if (instant || !p.shown) { p.shown = false; p.closing = false; return; }
    p.closing = true;
}
/** The slide-up finished: leave the layout unless a reopen cancelled the close. */
export function pinsCloseSettled() {
    if (!p.closing) return;
    p.closing = false;
    p.shown = false;
}
