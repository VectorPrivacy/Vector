// The hover toolbar over a message row: which actions apply to the row under the
// cursor, and the delete button's flavour. The app decides; the buttons derive.
let tb = $state.raw({
    show: {},            // { react, reply, edit, reveal, copy, retry, cancel, delete }: true = offered
    path: null,          // the downloaded attachment behind reveal / copy
    del: null,           // { mode: 'delete' | 'hide' | 'failed', label, partial, hasAttachments }
});
export function messageToolbar() { return tb; }
export function setMessageToolbar(view) { tb = view; }

// The host the buttons sit in: shown over one row at a content-space position the
// app computes, and the swipe-to-reply chip beside a row mid-gesture. Both are
// children of the message list so they ride the scroll for free.
const host = $state({ open: false, target: '', top: '', left: '' });
const swipe = $state({ visible: false, past: false, top: '', left: '', opacity: '0', transform: 'scale(0.4)', transition: '' });
const els = { host: null };
let handlers = $state.raw({});   // hoverIn, hoverOut, click
export function toolbarHost() { return host; }
export function toolbarSwipe() { return swipe; }
export function toolbarEls() { return els; }
export function toolbarHandlers() { return handlers; }
export function setToolbarHandlers(h) { handlers = h || {}; }
export function setToolbarHost(patch) { Object.assign(host, patch); }
export function setToolbarSwipe(patch) { Object.assign(swipe, patch); }
