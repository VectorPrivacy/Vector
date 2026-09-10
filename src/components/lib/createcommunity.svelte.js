// The Create Community panel: name, avatar, picked invitees, and the create action's
// progress. `seq` bumps per open so the panel remounts its picker fresh.
const s = $state({ seq: 0, name: '', filter: '', avatarPath: null, avatarPreview: '', selected: 0, busy: false, status: '', error: false, profilesSeq: 0 });

export function ccState() { return s; }
export function ccOpen() {
    s.seq++; s.name = ''; s.filter = ''; s.avatarPath = null; s.avatarPreview = '';
    s.selected = 0; s.busy = false; s.status = ''; s.error = false;
}
export function ccSetAvatar(path, preview) { s.avatarPath = path; s.avatarPreview = preview || ''; }
export function ccSetSelected(n) { s.selected = n; }
export function ccSetBusy(busy, status) { s.busy = !!busy; s.status = status || ''; s.error = false; }
export function ccSetError(msg) { s.busy = false; s.status = msg || ''; s.error = !!msg; }
/** A profile load landed while the panel is open: the picker swaps its snapshot. */
export function ccProfilesChanged() { s.profilesSeq++; }
