// The chat view's window state (Phase 2c). The engine in chat-scroll.js keeps its
// arithmetic and its scroll compensation; instead of inserting and removing rows it
// sets the window here and flushes synchronously, then measures as before.
//
// The window is anchored by message IDS, like the engine's own anchors: the array
// mutates underneath (prepends, mid-inserts), and ids survive that where indices do
// not. `rev` says "the slice's contents changed in place" (an edit, a splice).
import { SvelteMap } from 'svelte/reactivity';

const win = $state({ chatId: null, topId: null, bottomId: null, rev: 0 });
const divider = $state({ targetId: null, after: false });
// Per-message versions: messages are mutated in place (an edit, a failed flag), so
// the row cannot see the change through its prop; this is what refills it.
const messages = new SvelteMap();

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

/** Read: a message's version (its row refills when it changes). */
export function messageVersion(id) {
    return messages.get(id) ?? 0;
}
/** Write: this message object changed in place. */
export function touchMessage(id) {
    messages.set(id, (messages.get(id) ?? 0) + 1);
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

// What the list shows beside the rows: the "start of channel" marker of an empty
// community and the end-of-timeline notices (blocked peer, dissolved community, v1→v2
// upgrade). The openers write these; the list renders them after the rows.
const notices = $state({ empty: '', blocked: false, dissolved: '', migrated: false });
export function noticeState() { return notices; }
export function setNotice(key, value) { notices[key] = value; }
export function clearNotices() { notices.empty = ''; notices.blocked = false; notices.dissolved = ''; notices.migrated = false; }

// The live arrival that plays the slide-in: one id, cleared when its animation ends.
const arrival = $state({ id: null });
export function arrivalState() { return arrival; }
export function setArrival(id) { arrival.id = id || null; }

// A once-a-second clock for the countdowns: a row reading it re-derives on the tick.
const clock = $state({ sec: Math.floor(Date.now() / 1000) });
export function clockSec() { return clock.sec; }
setInterval(() => { clock.sec = Math.floor(Date.now() / 1000); }, 1000);
