// The composer's state (Phase 3, COMPOSER_ISLAND_DESIGN.md): one home for what six
// modules used to poke into the chrome by hand. The editor itself (composer.js) is
// an imperative leaf and is never touched from here; this is the mode it is in, the
// draft's emptiness (mic ↔ send), the lock, and the reply/command bars' content.
const mode = $state({ kind: 'idle', id: '', name: '', snippet: null, original: '' });
const draft = $state({ empty: true, animate: false, seq: 0 });
const lock = $state({ reason: null, placeholder: '' });
const command = $state({ active: false });

export function composerMode() { return mode; }
export function composerDraft() { return draft; }
export function composerLock() { return lock; }
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

export function setCommandActive(active) {
    command.active = !!active;
}
