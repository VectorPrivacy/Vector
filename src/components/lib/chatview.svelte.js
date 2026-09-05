// The chat view's window state (Phase 2c). The engine in chat-scroll.js keeps its
// arithmetic and its scroll compensation; instead of inserting and removing rows it
// sets the window here and flushes synchronously, then measures as before.
//
// The window is anchored by message IDS, like the engine's own anchors: the array
// mutates underneath (prepends, mid-inserts), and ids survive that where indices do
// not. `rev` says "the slice's contents changed in place" (an edit, a splice).
const win = $state({ chatId: null, topId: null, bottomId: null, rev: 0 });
const divider = $state({ targetId: null, after: false });

export function windowState() {
    return win;
}
export function dividerState() {
    return divider;
}

/** Render [topId .. bottomId] of `chatId`'s array. Ids, not indices. */
export function setWindow(chatId, topId, bottomId) {
    win.chatId = chatId || null;
    win.topId = topId || null;
    win.bottomId = bottomId || null;
    win.rev++;
}
export function clearWindow() {
    win.topId = null;
    win.bottomId = null;
    win.rev++;
}
/** The array changed under the window (insert, splice, edit): re-derive. */
export function touchWindow() {
    win.rev++;
}

/** The "New" divider sits before `targetId` (or after it, when `after`). */
export function setDivider(targetId, after = false) {
    divider.targetId = targetId || null;
    divider.after = !!after;
}
export function clearDivider() {
    divider.targetId = null;
    divider.after = false;
}
