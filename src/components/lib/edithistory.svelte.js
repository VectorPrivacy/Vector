// The open edit-history popup: which message, its revisions, the emoji tags that resolve
// every revision's shortcodes, and whether the popup sits below the bubble.
const s = $state({ msgId: '', entries: [], tags: null, below: false });
export function editHistoryState() { return s; }
export function setEditHistory(msgId, entries, tags) { s.msgId = msgId; s.entries = entries || []; s.tags = tags; s.below = false; }
export function setEditHistoryBelow(below) { s.below = !!below; }
export function clearEditHistory() { s.msgId = ''; s.entries = []; s.tags = null; }
