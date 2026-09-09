// The conversation pane's header: its helper bag is registered once by chat.js (the
// helpers live in scripts that load after the shell mounts), and the header reads it
// lazily on every derive.
let handlers = null;
export function chatHeaderHandlers() { return handlers; }
export function setChatHeaderHandlers(h) { handlers = h; }
