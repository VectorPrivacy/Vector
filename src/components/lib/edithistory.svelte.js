// The open edit-history popup: which message, its revisions, the emoji tags that resolve
// every revision's shortcodes, the bubble it anchors to, and whether it sits below it.
const s = $state({ open: false, msgId: '', entries: [], tags: null, below: false, anchor: null });
export function editHistoryState() { return s; }
export function setEditHistory(msgId, entries, tags) { s.msgId = msgId; s.entries = entries || []; s.tags = tags; s.below = false; }
/** Show the popup against a bubble's rect; the component measures itself and places. */
export function openEditHistory(anchor) { s.anchor = anchor; s.open = true; }
/** Close it. The revisions stay until the next open, so the panel leaves with its content on it. */
export function clearEditHistory() { s.open = false; s.anchor = null; }
