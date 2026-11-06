// The chat's own scrollbar. The page holds a window of the history, so the native bar only measures
// that window; this one measures the chat. The app sets where the view sits, the bar renders it.
const bar = $state({
    shown: false,
    top: 0,          // 0..1: how far through the chat the view sits
    size: 1,         // 0..1: how much of the chat the view shows
    trackTop: 0,     // the track's box in the chat pane (px)
    trackHeight: 0,
    dragging: false,
    label: '',       // the date under the view, while dragging
});
let handlers = $state.raw({});   // seek(p, done), wheel(dy)
export function chatScrollbar() { return bar; }
export function setChatScrollbar(patch) { Object.assign(bar, patch); }
export function chatScrollbarHandlers() { return handlers; }
export function setChatScrollbarHandlers(h) { handlers = h || {}; }
