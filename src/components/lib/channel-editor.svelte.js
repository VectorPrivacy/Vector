// Channel Settings: a channel's name, topic and view, and on the new roles its permission
// overwrites one target at a time, drafted against what it holds (`saved`) until Save, in the
// Community Settings layout and lifecycle.
import { popOverlay } from './dialog-lifecycle.svelte.js';

export const ceOverlay = popOverlay({});

const s = $state({
    communityId: null, channelId: null, name: '', topic: '', view: 'chat', busy: false, error: '',
    saved: { name: '', topic: '', view: 'chat' },
    section: 'overview',
    query: '',
    // Bumped when a close or a switch of target is refused over unsaved changes, and which.
    nudge: 0,
    nudged: 'close',
    // A Category opens on its permissions alone: it has no topic or view.
    isCategory: false,
    private: false,
    // perms: the channel's overwrites as read ({ activated, can_manage, roles, overwrites, grantable });
    // target: the overwrite open ('everyone', 'role:<id>', 'member:<hex>'); draft its { allow, deny } as decimal strings.
    perms: null, target: 'everyone', draft: null,
});

export function channelEditorState() { return s; }

/** Open on `channel` ({ id, name, topic, view, private }) of `communityId`, or on a Category's permissions. */
export function openChannelEditor(communityId, channel, isCategory = false) {
    const saved = { name: channel.name || '', topic: channel.topic || '', view: channel.view || 'chat' };
    Object.assign(s, {
        communityId, channelId: channel.id, ...saved, saved, busy: false, error: '',
        section: isCategory ? 'permissions' : 'overview', query: '',
        perms: null, target: 'everyone', draft: null, isCategory, private: !!channel.private,
    });
    ceOverlay.open({});
}
export function closeChannelEditor() { ceOverlay.close(); s.busy = false; s.error = ''; }
export function setChannelEditorBusy(on, error = '') { s.busy = !!on; s.error = error; }
export function setChannelEditorSection(id) { s.section = id; }
export function setChannelEditorQuery(q) { s.query = q; }
export function channelEditorNudge() { s.nudged = 'close'; s.nudge++; }

/** What Save made the channel's name, topic and view. */
export function channelEditorSaved(saved) { s.saved = { ...saved }; }

/** Back to what the channel holds, for the open target too. */
export function resetChannelEditor() {
    Object.assign(s, { name: s.saved.name, topic: s.saved.topic, view: s.saved.view, error: '' });
    s.draft = storedOverwrite(s.target);
}

/** Whether the name, topic or view differ from what the channel holds. */
export function channelOverviewDirty() {
    return !s.isCategory && (s.name !== s.saved.name || s.topic !== s.saved.topic || s.view !== s.saved.view);
}
export function channelEditorDirty() { return channelOverviewDirty() || overwriteDirty(); }

/** The overwrite stored for `key`, as { allow, deny } decimal strings. */
export function storedOverwrite(key) {
    const o = s.perms?.overwrites.find((x) => overwriteKey(x) === key);
    return { allow: o?.allow || '0', deny: o?.deny || '0' };
}
export function overwriteKey(o) { return o.kind === 'everyone' ? 'everyone' : `${o.kind}:${o.id}`; }

export function setChannelPerms(perms) {
    s.perms = perms;
    s.draft = storedOverwrite(s.target);
}
/** Open another target's overwrite; one with unsaved bits stays open, and the bar says why. */
export function selectOverwrite(key) {
    if (key !== s.target && overwriteDirty()) { s.nudged = 'switch'; s.nudge++; return; }
    s.target = key;
    s.draft = storedOverwrite(key);
}
/** Set one bit to 'allow', 'deny' or 'inherit' in the open draft. */
export function setOverwriteBit(bit, state) {
    if (!s.draft) return;
    let allow = BigInt(s.draft.allow), deny = BigInt(s.draft.deny);
    allow &= ~bit; deny &= ~bit;
    if (state === 'allow') allow |= bit;
    if (state === 'deny') deny |= bit;
    s.draft = { allow: String(allow), deny: String(deny) };
}
export function overwriteDirty() {
    if (!s.draft) return false;
    const stored = storedOverwrite(s.target);
    return stored.allow !== s.draft.allow || stored.deny !== s.draft.deny;
}
