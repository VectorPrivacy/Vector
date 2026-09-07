// Our own profile's Edit Mode: the snapshot it started from, the draft the fields
// hold, and the pictures picked but not yet uploaded. The app saves; this is the state.
const edit = $state({
    active: false,
    snapshot: { name: '', about: '', avatar: null, banner: null },
    draft: { name: '', about: '' },
    pending: { avatar: null, banner: null },     // picked file paths
    preview: { avatar: '', banner: '' },         // their on-screen previews
});

export function profileEdit() { return edit; }

export function startProfileEdit(snapshot) {
    edit.snapshot = snapshot;
    edit.draft = { name: snapshot.name || '', about: snapshot.about || '' };
    edit.pending = { avatar: null, banner: null };
    edit.preview = { avatar: '', banner: '' };
    edit.active = true;
}
export function endProfileEdit() {
    edit.active = false;
    edit.pending = { avatar: null, banner: null };
    edit.preview = { avatar: '', banner: '' };
}
export function setProfileEditPicture(kind, path, preview) {
    edit.pending[kind] = path;
    edit.preview[kind] = preview || '';
}
/** Whether anything differs from the snapshot. */
export function profileEditDirty() {
    const { draft: d, snapshot: s, pending } = edit;
    return d.name.trim() !== (s.name || '') || d.about.trim() !== (s.about || '')
        || pending.avatar !== null || pending.banner !== null;
}
