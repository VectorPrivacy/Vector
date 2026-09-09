// The shared context menu: what it lists and where it sits. context-menu.js opens it,
// clamps it to the viewport after measuring the rendered element, and dismisses it.
const m = $state({ open: false, x: 0, y: 0, items: [] });
const els = $state.raw({ root: null });
let handlers = {};   // activate(item)
export function contextMenuState() { return m; }
export function contextMenuEls() { return els; }
export function contextMenuHandlers() { return handlers; }
export function setContextMenuHandlers(h) { handlers = h || {}; }
export function setContextMenu(patch) { Object.assign(m, patch); }
