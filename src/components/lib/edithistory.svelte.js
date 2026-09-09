// The open edit-history popup: which message, its revisions, the emoji tags that resolve
// every revision's shortcodes, the bubble it anchors to, and whether it sits below it.
const s = $state({ open: false, msgId: '', entries: [], tags: null, below: false, anchor: null });
export function editHistoryState() { return s; }
export function setEditHistory(msgId, entries, tags) { s.msgId = msgId; s.entries = entries || []; s.tags = tags; s.below = false; }
export function setEditHistoryBelow(below) { s.below = !!below; }
/** Show the popup against a bubble's rect; the component measures itself and places. */
export function openEditHistory(anchor) { s.anchor = anchor; s.open = true; }
export function clearEditHistory() { s.open = false; s.anchor = null; s.msgId = ''; s.entries = []; s.tags = null; }
