// The composer's state: one home for what six
// modules used to poke into the chrome by hand. The editor itself (composer.js) is
// an imperative leaf and is never touched from here; this is the mode it is in, the
// draft's emptiness (mic ↔ send), the lock, and the reply/command bars' content.
const mode = $state({ kind: 'idle', id: '', name: '', snippet: null, original: '' });
const draft = $state({ empty: true, animate: false, seq: 0 });
const lock = $state({ reason: null, placeholder: '' });
// A transient placeholder while a send or edit is in flight ('' = none). Not a lock:
// the editor stays usable, the text just says what is happening.
const status = $state({ text: '' });
// The structured command composer: the picked command's argument pills replace
// the editor while they are filled. `values` holds the picker-typed args (choice,
// bool); free-text fields own their text in the DOM. `attach` wires a mounted
// field to the controller that walks focus between parts.
const command = $state({
    active: false, seq: 0, name: '', bot: null, hint: '', args: [], values: [], invalid: -1, attach: null,
});

export function composerMode() { return mode; }
export function composerDraft() { return draft; }
export function composerLock() { return lock; }
export function composerStatus() { return status; }
export function composerCommand() { return command; }

/** Replying to `id`. `snippet` is { html, emojiTags } or { text } or null. */
export function startReply(id, name, snippet) {
    mode.kind = 'reply';
    mode.id = id;
    mode.name = name || '';
    mode.snippet = snippet || null;
    mode.original = '';
}
export function cancelReply() {
    if (mode.kind !== 'reply') return;
    mode.kind = 'idle';
    mode.id = '';
}
/** Editing `id`; `original` is the content the edit started from. */
export function startEdit(id, original) {
    mode.kind = 'edit';
    mode.id = id;
    mode.original = original || '';
    mode.name = '';
    mode.snippet = null;
}
export function cancelEdit() {
    if (mode.kind !== 'edit') return;
    mode.kind = 'idle';
    mode.id = '';
    mode.original = '';
}

/** The draft's emptiness changed. `animate` = the user typed it (swap animates). */
export function setDraftEmpty(empty, animate = false) {
    draft.empty = !!empty;
    draft.animate = !!animate;
    draft.seq++;
}

/** Lock the composer (a dissolved community, a blocked contact) or release it (null). */
export function setLock(reason, placeholder = '') {
    lock.reason = reason || null;
    lock.placeholder = placeholder || '';
}

/** Show a transient placeholder ('Sending...') or clear it (''). */
export function setComposerStatus(text) { status.text = text || ''; }

/** Enter the command composer for `name` with `args` [{ name, type, required, description, grow }]. */
export function setCommand({ name, bot, args, attach }) {
    command.active = true;
    command.seq++;
    command.name = name;
    command.bot = bot || null;
    command.hint = '';
    command.args = args;
    command.values = args.map(() => '');
    command.invalid = -1;
    command.attach = attach;
}
/** Leave the composer. Name and bot stay for the strip's collapse animation. */
export function clearCommand() {
    command.active = false;
    command.args = [];
    command.values = [];
    command.invalid = -1;
    command.hint = '';
    command.attach = null;
}
export function setCommandHint(text) {
    command.hint = text || '';
}
export function setCommandInvalid(idx) {
    command.invalid = idx;
}
export function setCommandValue(idx, value) {
    if (idx >= 0 && idx < command.values.length) command.values[idx] = value || '';
}

// The choice drop-up for a picker part: options anchored to their trigger.
let choice = $state.raw({ open: false });

export function choiceMenu() { return choice; }
export function openChoiceMenu(view) {
    choice = { open: true, ...view };
}
export function closeChoiceMenu() {
    if (choice.open) choice = { open: false };
}

// The autocomplete popup: at most one open at a time. The view is RAW (a fresh
// snapshot per open, so the panel re-derives from the object's identity) and keeps
// its handler: the controller that parsed the trigger is the one that inserts.
let popup = $state.raw({ kind: null });

export function composerPopup() { return popup; }

/** Open (or re-render) the `kind` popup with `view`; the previous kind closes. */
export function openPopup(kind, view) {
    popup = { kind, ...view };
}
/** Close the popup if it is `kind` (a stale close from another controller is a no-op). */
export function closePopup(kind) {
    if (popup.kind === kind) popup = { kind: null };
}

// The box's chrome flags: the add-file button's open state (mirrors the attachment
// panel), the emoji button's face (a wink while the picker is up), and the
// scroll-return badge.
const chrome = $state({ attachmentOpen: false, emojiIcon: 'smile', scrollBadge: '', selfDestructSecs: 0 });
export function composerChrome() { return chrome; }
export function setAttachmentOpen(on) { chrome.attachmentOpen = !!on; }
export function setEmojiIcon(face) { chrome.emojiIcon = face === 'wink' ? 'wink' : 'smile'; }
export function setScrollBadge(text) { chrome.scrollBadge = text || ''; }
/** The open chat's Self-Destruct Timer, shown as the clock badge on Send. */
export function setSelfDestructSecs(secs) { chrome.selfDestructSecs = secs || 0; }

// The box's handlers, registered by chat.js once its scripts have loaded. Raw state so
// the box's editor effects re-run once the editor is reachable.
let handlers = $state.raw(null);
export function composerHandlers() { return handlers; }
export function setComposerHandlers(h) { handlers = h; }

// Element refs for the code that has no reactive equivalent: the voice recorder's hold
// gesture, the picker's hit-tests and the scroll handler. Bound by the box.
const els = {};
export function composerEls() { return els; }
