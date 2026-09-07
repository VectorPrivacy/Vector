// The open channel's pinned messages as the drawer shows them. The app resolves the
// context and fetches; the drawer derives rows, notices and affordances.
const p = $state({ open: false, sealed: false, canPin: false, communityId: null, channelId: null, v: 0 });
let pins = $state.raw([]);   // verified pins, `_jumpable` resolved by the app

export function pinsState() { return p; }
export function pinsList() { return pins; }
export function setPins({ pins: list, sealed, canPin, communityId, channelId }) {
    pins = list || [];
    p.sealed = !!sealed;
    p.canPin = !!canPin;
    p.communityId = communityId || null;
    p.channelId = channelId || null;
    p.v++;
}
export function setPinsOpen(open) { p.open = !!open; }
