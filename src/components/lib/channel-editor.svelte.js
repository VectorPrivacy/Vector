// The channel editor: a channel's name, topic and view, drafted until Save, and on the
// new roles its permission overwrites, one target at a time.

const s = $state({
    open: false, communityId: null, channelId: null, name: '', topic: '', view: 'chat', busy: false, error: '',
    tab: 'overview',
    // perms: the channel's overwrites as read ({ activated, can_manage, roles, overwrites, grantable });
    // target: the overwrite open ('everyone', 'role:<id>', 'member:<hex>'); draft its { allow, deny } as decimal strings.
    perms: null, target: 'everyone', draft: null,
});

export function channelEditorState() { return s; }

/** Open on `channel` ({ id, name, topic, view }) of `communityId`. */
export function openChannelEditor(communityId, channel) {
    Object.assign(s, {
        open: true, communityId, channelId: channel.id, name: channel.name || '', topic: channel.topic || '',
        view: channel.view || 'chat', busy: false, error: '', tab: 'overview', perms: null, target: 'everyone', draft: null,
    });
}
export function closeChannelEditor() { s.open = false; s.busy = false; s.error = ''; }
export function setChannelEditorBusy(on, error = '') { s.busy = !!on; s.error = error; }
export function setChannelEditorTab(tab) { s.tab = tab; }

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
export function selectOverwrite(key) {
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
