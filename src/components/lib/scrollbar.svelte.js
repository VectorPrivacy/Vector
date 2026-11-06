// The chat's own scrollbar. The page holds a window of the history, so the native bar only measures
// that window; this one measures the chat. The app sets where the view sits, the bar renders it.
const bar = $state({
    shown: false,
    top: 0,          // 0..1: how far through the chat the view sits
    size: 1,         // 0..1: how much of the chat the view shows
    trackTop: 0,     // the track's box in the chat pane (px)
    trackHeight: 0,
    dragging: false,
    active: false,   // the reader scrolled a moment ago: on touch, the handle and the floating date show
    edgeShown: false, // the top of the history is in view, with its own word on what lies above
    tip: '',         // the date at the place last pointed at
    here: '',        // the date at the top of the view
    more: '',        // set while older history may still come: what the rail's top mark says
});
let handlers = $state.raw({});   // seek(p, done), point(p), wheel(e)
export function chatScrollbar() { return bar; }
export function setChatScrollbar(patch) { Object.assign(bar, patch); }
export function chatScrollbarHandlers() { return handlers; }
export function setChatScrollbarHandlers(h) { handlers = h || {}; }
