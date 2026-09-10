// The two popups a reaction chip can raise: the hover summary and the who-reacted
// details. Each is a raw view anchored to its chip; the app opens and closes them.
let tip = $state.raw(null);       // { emoji, names: [...], anchor }
let details = $state.raw(null);   // { emoji, msgId, anchor }

export function reactionTip() { return tip; }
export function reactionDetails() { return details; }
export function openReactionTip(view) { tip = view; }
export function closeReactionTip() { if (tip) tip = null; }
export function openReactionDetails(view) { details = view; }
export function closeReactionDetails() { if (details) details = null; }

// The popups' elements while shown, for the dismiss paths' hit tests.
const els = { tip: null, details: null };
export function reactionEls() { return els; }
export function bindReactionEl(name) { return (node) => { els[name] = node; return { destroy() { els[name] = null; } }; }; }
